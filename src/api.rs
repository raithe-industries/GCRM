// ------------------------------------------------------------
// RAiTHE INDUSTRIES INCORPORATED
// Copyright (c) 2026 All Rights Reserved.
//
// This file is part of a proprietary system. Unauthorized use,
// reproduction, or distribution is strictly prohibited.
// ------------------------------------------------------------

// src/api.rs — Operator API
//
// All routes under /api/regime/* and /api/operator/* require the
// X-GCRM-Key header to match settings.yml dashboard.operator_key.
// Public routes (/api/latest, /api/sources, etc.) are unaffected.
//
// Routes:
//   GET  /api/regime                  → list all regime factors + current product
//   POST /api/regime/:id/toggle       → activate or deactivate a factor (key required)
//   POST /api/regime/:id/set          → set multiplier value (key required)
//   GET  /api/domains                 → domain weights and half-lives
//   POST /api/operator/assert         → manually assert a geopolitical event (key required)
//   GET  /api/operator/log            → operator event audit log (key required)
//   GET  /api/operator/seismic        → seismic alert detail (key required)
//   POST /api/operator/seismic/:id/dismiss → dismiss a seismic alert (key required)

use std::sync::Arc;
use std::time::Instant;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
};
use chrono::Utc;
use dashmap::DashMap;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tracing::{info, warn};

use crate::aggregator::AppState;
use crate::bayesian::{
    guardrail_from_regime, DOMAIN_HALF_LIVES, DOMAIN_WEIGHTS, GUARDRAIL_AMPLIFIER,
    GUARDRAIL_REGIME_SPAN,
};
use crate::models::{DOMAIN_IDS, RegimeFactor};

/// Maximum operator API requests per key per minute.
const RATE_LIMIT_CAPACITY: u32 = 60;

/// A regime product this high is far past the guardrail-collapse saturation point
/// (`1 + GUARDRAIL_REGIME_SPAN` = 5×, above which the coupler clamps at 1.0 and adds
/// nothing further), so it signals that an operator has stacked implausibly many
/// structural multipliers — not that the prior has moved (in v2 the prior is FLAT;
/// the regime product enters only as the bounded guardrail-collapse amplifier on the
/// systemic likelihood, never the prior).
const REGIME_PRODUCT_WARN_THRESHOLD: f64 = 20.0;

// Per-key token bucket. Each key starts with RATE_LIMIT_CAPACITY tokens.
// One token is consumed per request. Tokens refill at 1/second (continuous
// leaky-bucket approximation). A key that hasn't been used for 60+ seconds
// returns to full capacity on next use.
//
// The bucket is stored in a DashMap keyed on the literal key string so that
// different operator keys have independent rate budgets. In a single-key
// deployment this is equivalent to a global rate limiter.

#[derive(Debug)]
struct RateBucket {
    /// Tokens remaining in the bucket.
    tokens:     f64,
    /// Wall clock time of the last refill computation.
    last_refill: Instant,
}

impl RateBucket {
    fn new() -> Self {
        Self {
            tokens:      RATE_LIMIT_CAPACITY as f64,
            last_refill: Instant::now(),
        }
    }

    /// Attempt to consume one token. Returns true if successful (request allowed),
    /// false if the bucket is empty (request should be rejected with 429).
    /// Also returns the number of seconds until the next token is available.
    fn try_consume(&mut self) -> (bool, u64) {
        let now     = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        // Refill: 1 token per second, capped at RATE_LIMIT_CAPACITY
        self.tokens = (self.tokens + elapsed).min(RATE_LIMIT_CAPACITY as f64);
        self.last_refill = now;

        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            (true, 0)
        } else {
            // Time until the next full token: (1.0 - tokens) seconds
            let retry_after = ((1.0 - self.tokens).ceil() as u64).max(1);
            (false, retry_after)
        }
    }
}

// ── Operator state — shared between api.rs handlers and main.rs ───────────────

#[derive(Clone)]
pub struct OperatorState {
    pub app_state:    Arc<AppState>,
    pub operator_key: String,
    /// Live-editable copy of regime factors.
    /// Initialised from settings.yml at startup, modified via API.
    pub regime:       Arc<tokio::sync::Mutex<Vec<RegimeFactor>>>,
    rate_buckets:     Arc<DashMap<String, RateBucket>>,
}

impl OperatorState {
    pub fn new(
        app_state:      Arc<AppState>,
        operator_key:   String,
        regime_factors: Vec<RegimeFactor>,
    ) -> Self {
        Self {
            app_state,
            operator_key,
            regime:       Arc::new(tokio::sync::Mutex::new(regime_factors)),
            rate_buckets: Arc::new(DashMap::new()),
        }
    }
}

/// Constant-time string equality. The operator key gates controls that change
/// the published risk number (regime toggles), so key verification must not leak
/// the secret through response-timing differences. Length is allowed to differ
/// up front; the byte comparison itself never short-circuits on the first
/// mismatch.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn check_key(headers: &HeaderMap, expected: &str) -> Result<String, (StatusCode, Json<Value>)> {
    if expected.is_empty() {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": "Operator API is disabled — set dashboard.operator_key in settings.yml"
            })),
        ));
    }
    let provided = headers
        .get("X-GCRM-Key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !constant_time_eq(provided, expected) {
        warn!("Operator API: rejected request with invalid key");
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Invalid or missing X-GCRM-Key header"})),
        ));
    }
    Ok(provided.to_string())
}

fn check_rate_limit(
    key:     &str,
    buckets: &DashMap<String, RateBucket>,
) -> Result<(), (StatusCode, Json<Value>)> {
    let mut bucket = buckets.entry(key.to_string()).or_insert_with(RateBucket::new);
    let (allowed, retry_after) = bucket.try_consume();
    if !allowed {
        warn!("Operator API: rate limit exceeded for key (retry_after={retry_after}s)");
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({
                "error":       "Rate limit exceeded — maximum 60 operator requests per minute",
                "retry_after": retry_after,
            })),
        ));
    }
    Ok(())
}

/// Authenticate and apply rate limit. Call at the top of every keyed handler.
fn check_key_and_rate(
    headers: &HeaderMap,
    state:   &OperatorState,
) -> Result<(), (StatusCode, Json<Value>)> {
    let key = check_key(headers, &state.operator_key)?;
    check_rate_limit(&key, &state.rate_buckets)?;
    Ok(())
}

// ── Regime factor helpers ─────────────────────────────────────────────────────

fn regime_product(factors: &[RegimeFactor]) -> f64 {
    let p: f64 = factors.iter()
        .filter(|f| f.active)
        .map(|f| f.multiplier)
        .product();
    (p * 1e4).round() / 1e4
}

fn regime_summary(factors: &[RegimeFactor]) -> Value {
    let product      = regime_product(factors);
    let active_count = factors.iter().filter(|f| f.active).count();
    // v2 honesty: the regime product is STRUCTURAL PRESSURE, not an "adjusted prior".
    // The v2 engine holds the prior FLAT (BASELINE_ANNUAL) and feeds the regime product
    // through the SAME `guardrail_from_regime` coupler it uses internally — so this panel
    // reports exactly what toggling a factor does to the forecast: it moves guardrail
    // collapse, a bounded amplifier on the systemic likelihood, never the prior.
    let guardrail = guardrail_from_regime(product);
    let likelihood_amplifier_pct = 100.0 * GUARDRAIL_AMPLIFIER * guardrail;
    json!({
        "factors":            factors,
        "active_count":       active_count,
        "product":            product,
        "guardrail_collapse":       (guardrail * 1e6).round() / 1e6,
        "likelihood_amplifier_pct": (likelihood_amplifier_pct * 1e6).round() / 1e6,
    })
}

/// Returns an array of warning strings. Empty if all thresholds satisfied.
fn regime_warnings(factors: &[RegimeFactor]) -> Vec<String> {
    let product = regime_product(factors);
    let mut warnings = Vec::new();
    if product > REGIME_PRODUCT_WARN_THRESHOLD {
        let saturation = 1.0 + GUARDRAIL_REGIME_SPAN; // product at which guardrail collapse clamps at 1.0
        warnings.push(format!(
            "Regime product {:.2}× exceeds the warning threshold ({:.0}×) — far past the \
             guardrail-collapse saturation point ({:.0}×). Structural guardrails are already \
             modeled as fully collapsed (guardrail = 1.00, the maximum +{:.0}% lift on the \
             systemic likelihood); stacking more multipliers does nothing further. Re-check \
             whether this many factors are warranted. The prior is unaffected (v2 prior is flat).",
            product,
            REGIME_PRODUCT_WARN_THRESHOLD,
            saturation,
            GUARDRAIL_AMPLIFIER * 100.0,
        ));
        warn!(
            "Regime product {:.4}× exceeds {:.0}× warning threshold — guardrail collapse \
             saturated (max +{:.0}% on systemic likelihood); prior unaffected",
            product,
            REGIME_PRODUCT_WARN_THRESHOLD,
            GUARDRAIL_AMPLIFIER * 100.0,
        );
    }
    warnings
}

// ── GET /api/regime ───────────────────────────────────────────────────────────
// Public read — shows all factors, current product (structural pressure), and the
// guardrail-collapse coupler it drives. No key required: this is display information only.

pub async fn get_regime(State(state): State<OperatorState>) -> impl IntoResponse {
    let factors  = state.regime.lock().await;
    let summary  = regime_summary(&factors);
    let warnings = regime_warnings(&factors);
    // Include warnings in the public read so the dashboard can surface them
    Json(json!({
        "factors":      summary["factors"],
        "active_count": summary["active_count"],
        "product":      summary["product"],
        "guardrail_collapse":       summary["guardrail_collapse"],
        "likelihood_amplifier_pct": summary["likelihood_amplifier_pct"],
        "warnings":     warnings,
    }))
}

// ── POST /api/regime/:id/toggle ───────────────────────────────────────────────
// Toggle a regime factor active/inactive. Key + rate limit required.

pub async fn toggle_regime(
    State(state): State<OperatorState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(e) = check_key_and_rate(&headers, &state) { return e.into_response(); }

    let mut factors = state.regime.lock().await;
    match factors.iter_mut().find(|f| f.id == id) {
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("Regime factor '{id}' not found")})),
        ).into_response(),
        Some(factor) => {
            factor.active = !factor.active;
            let new_state = factor.active;
            let label     = factor.label.clone();
            let product   = regime_product(&factors);
            let warnings  = regime_warnings(&factors);
            info!(
                "Operator: regime factor '{}' → {} (product now {:.4}×)",
                id, if new_state { "ACTIVE" } else { "INACTIVE" }, product
            );
            // Sync to shared_regime so Aggregator picks it up immediately
            *state.app_state.shared_regime.lock().await = factors.clone();
            *state.app_state.last_calibrated_at.lock().await = Some(Utc::now());

            let entry = json!({
                "ts":      Utc::now().to_rfc3339(),
                "action":  "regime_toggle",
                "id":      id,
                "label":   label,
                "active":  new_state,
                "product": product,
            });
            let _ = write_operator_log(&state, entry.clone()).await;
            Json(json!({
                "id":       id,
                "active":   new_state,
                "product":  product,
                "regime":   regime_summary(&factors),
                "warnings": warnings,
            })).into_response()
        }
    }
}

// ── POST /api/regime/:id/set ──────────────────────────────────────────────────
// Set multiplier value for a factor. Key + rate limit required.

#[derive(Deserialize)]
pub struct SetMultiplierBody {
    multiplier: f64,
}

pub async fn set_regime_multiplier(
    State(state): State<OperatorState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<SetMultiplierBody>,
) -> impl IntoResponse {
    if let Err(e) = check_key_and_rate(&headers, &state) { return e.into_response(); }

    // Multiplier sanity bounds — prevent accidental model destruction
    if body.multiplier <= 0.0 || body.multiplier > 10.0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "Multiplier must be in range (0.0, 10.0]"})),
        ).into_response();
    }

    let mut factors = state.regime.lock().await;
    match factors.iter_mut().find(|f| f.id == id) {
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("Regime factor '{id}' not found")})),
        ).into_response(),
        Some(factor) => {
            let old    = factor.multiplier;
            factor.multiplier = (body.multiplier * 1e4).round() / 1e4;
            let new_val = factor.multiplier;
            drop(factors);
            // Sync to shared_regime so Aggregator picks up the new multiplier
            *state.app_state.shared_regime.lock().await = state.regime.lock().await.clone();
            let factors  = state.regime.lock().await;
            let product  = regime_product(&factors);
            let warnings = regime_warnings(&factors);
            info!(
                "Operator: regime factor '{}' multiplier {:.4} → {:.4} (product {:.4}×)",
                id, old, new_val, product
            );
            let entry = json!({
                "ts":      Utc::now().to_rfc3339(),
                "action":  "regime_set_multiplier",
                "id":      id,
                "old":     old,
                "new":     new_val,
                "product": product,
            });
            let summary = regime_summary(&factors);
            drop(factors);
            let _ = write_operator_log(&state, entry).await;
            Json(json!({
                "id":         id,
                "multiplier": new_val,
                "product":    product,
                "regime":     summary,
                "warnings":   warnings,
            })).into_response()
        }
    }
}

// ── GET /api/domains ──────────────────────────────────────────────────────────
// Returns domain weights and half-lives. Public read.

pub async fn get_domains(_: State<OperatorState>) -> impl IntoResponse {
    use crate::bayesian::domain_weight;

    let domains: Vec<Value> = DOMAIN_IDS.iter().map(|&id| {
        let half_life = DOMAIN_HALF_LIVES.iter()
            .find(|(d, _)| *d == id)
            .map(|(_, h)| *h)
            .unwrap_or(24.0);
        json!({
            "id":              id,
            "weight":          domain_weight(id),
            "half_life_hours": half_life,
            "description":     domain_description(id),
        })
    }).collect();

    let total_weight: f64 = DOMAIN_WEIGHTS.iter().map(|(_, w)| w).sum();
    Json(json!({
        "domains":        domains,
        "total_weight":   total_weight,
        "scaling_factor": 20.0,
        "note": "Weights encode relative contribution to P(WWIII). Nuclear posture and military escalation have the highest weights.",
    }))
}

fn domain_description(id: &str) -> &'static str {
    match id {
        "military_escalation"  => "Active conventional military conflict, strikes, deployments",
        "nuclear_posture"      => "Nuclear weapons posture, testing, doctrine changes",
        "diplomatic_breakdown" => "Diplomatic crises, expulsions, treaty failures",
        "economic_warfare"     => "Sanctions, embargoes, financial weapons, resource denial",
        "cyber_info_ops"       => "State-sponsored cyber attacks, information warfare",
        // v2 removed great-power / alliance / WMD as domains (they are couplers and a
        // rung override now), so the only ids reaching this fn are the five in DOMAIN_IDS.
        _                      => "Unknown domain",
    }
}

// ── POST /api/operator/assert ─────────────────────────────────────────────────
// Manually assert a ground-truth event into the system. Key + rate limit required.

#[derive(Deserialize)]
pub struct AssertEventBody {
    /// Human-readable description of the confirmed event
    description: String,
    /// Regime factor IDs to activate
    activate:    Option<Vec<String>>,
    /// Regime factor IDs to deactivate
    deactivate:  Option<Vec<String>>,
    /// Optional note for the audit log
    note:        Option<String>,
    /// Severity 0-1 for the event record
    severity:    Option<f64>,
}

pub async fn assert_event(
    State(state): State<OperatorState>,
    headers: HeaderMap,
    Json(body): Json<AssertEventBody>,
) -> impl IntoResponse {
    if let Err(e) = check_key_and_rate(&headers, &state) { return e.into_response(); }

    if body.description.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "description is required"})),
        ).into_response();
    }

    let mut changes: Vec<Value> = Vec::new();
    let mut factors = state.regime.lock().await;

    for id in body.activate.as_deref().unwrap_or(&[]) {
        if let Some(f) = factors.iter_mut().find(|f| &f.id == id) {
            if !f.active {
                f.active = true;
                changes.push(json!({"id": id, "action": "activated"}));
                info!("Operator assert: activated regime factor '{id}'");
            }
        } else {
            warn!("Operator assert: regime factor '{id}' not found");
        }
    }

    for id in body.deactivate.as_deref().unwrap_or(&[]) {
        if let Some(f) = factors.iter_mut().find(|f| &f.id == id) {
            if f.active {
                f.active = false;
                changes.push(json!({"id": id, "action": "deactivated"}));
                info!("Operator assert: deactivated regime factor '{id}'");
            }
        }
    }

    let product  = regime_product(&factors);
    let warnings = regime_warnings(&factors);
    *state.app_state.shared_regime.lock().await = factors.clone();
    *state.app_state.last_calibrated_at.lock().await = Some(Utc::now());
    drop(factors);

    let event_id = uuid::Uuid::new_v4().to_string();
    let entry = json!({
        "id":            event_id,
        "ts":            Utc::now().to_rfc3339(),
        "action":        "operator_assert",
        "description":   body.description,
        "severity":      body.severity.unwrap_or(0.5),
        "changes":       changes,
        "product_after": product,
        "note":          body.note,
    });

    state.app_state.operator_events.lock().await.push(entry.clone());
    let _ = write_operator_log(&state, entry.clone()).await;

    info!(
        "Operator assert: '{}' | regime product now {:.4}×",
        entry["description"], product
    );

    Json(json!({
        "ok":       true,
        "id":       event_id,
        "product":  product,
        "entry":    entry,
        "warnings": warnings,
    })).into_response()
}

// ── GET /api/operator/log ─────────────────────────────────────────────────────
// Returns the in-memory operator event log (last 500 entries). Key + rate limit required.

pub async fn get_operator_log(
    State(state): State<OperatorState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = check_key_and_rate(&headers, &state) { return e.into_response(); }
    let log = state.app_state.operator_events.lock().await;
    let entries: Vec<_> = log.iter().rev().take(500).cloned().collect();
    Json(json!({"entries": entries, "total": log.len()})).into_response()
}

// ── GET /api/operator/seismic ─────────────────────────────────────────────────
// Returns detailed seismic alert list. Key + rate limit required.

pub async fn get_seismic_detail(
    State(state): State<OperatorState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = check_key_and_rate(&headers, &state) { return e.into_response(); }
    let alerts = state.app_state.nuclear_alerts.lock().await;
    Json(json!({
        "alerts": *alerts,
        "count":  alerts.len(),
    })).into_response()
}

// ── POST /api/operator/seismic/:id/dismiss ────────────────────────────────────
// Dismiss a seismic alert. Key + rate limit required.

pub async fn dismiss_seismic(
    State(state): State<OperatorState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(e) = check_key_and_rate(&headers, &state) { return e.into_response(); }

    let mut alerts = state.app_state.nuclear_alerts.lock().await;
    let before  = alerts.len();
    alerts.retain(|a| a.id != id);
    let removed = before - alerts.len();

    if removed == 0 {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("Seismic alert '{id}' not found")})),
        ).into_response();
    }

    info!("Operator: dismissed seismic alert '{id}'");
    let entry = json!({
        "ts":     Utc::now().to_rfc3339(),
        "action": "seismic_dismiss",
        "id":     id,
    });
    let _ = write_operator_log(&state, entry).await;
    Json(json!({"ok": true, "dismissed": id})).into_response()
}

// ── Audit log writer ──────────────────────────────────────────────────────────

async fn write_operator_log(state: &OperatorState, entry: Value) -> std::io::Result<()> {
    let line = serde_json::to_string(&entry).unwrap_or_default() + "\n";
    let _ = tokio::fs::create_dir_all("logs").await;
    let mut f = tokio::fs::OpenOptions::new()
        .create(true).append(true)
        .open("logs/operator_events.jsonl")
        .await?;
    f.write_all(line.as_bytes()).await?;

    // Keep in-memory log capped at 500 entries
    let mut log = state.app_state.operator_events.lock().await;
    if log.len() > 500 { log.drain(0..100); }
    Ok(())
}

// ── Router builder — called from server.rs ────────────────────────────────────

pub fn operator_routes() -> axum::Router<OperatorState> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/api/regime",              get(get_regime))
        .route("/api/regime/:id/toggle",   post(toggle_regime))
        .route("/api/regime/:id/set",      post(set_regime_multiplier))
        .route("/api/domains",             get(get_domains))
        .route("/api/operator/assert",     post(assert_event))
        .route("/api/operator/log",        get(get_operator_log))
        .route("/api/operator/seismic",    get(get_seismic_detail))
        .route("/api/operator/seismic/:id/dismiss", post(dismiss_seismic))
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_factors() -> Vec<RegimeFactor> {
        vec![
            RegimeFactor { id: "active_war".into(),   label: "Active war".into(),       multiplier: 1.4, active: true  },
            RegimeFactor { id: "arms_control".into(),  label: "Arms control dead".into(), multiplier: 1.4, active: true  },
            RegimeFactor { id: "deterrence".into(),    label: "Deterrence intact".into(),  multiplier: 0.7, active: true  },
            RegimeFactor { id: "standby_a".into(),     label: "Standby A".into(),          multiplier: 2.0, active: false },
        ]
    }

    // ── regime_product ────────────────────────────────────────────────────────

    #[test]
    fn regime_product_active_only() {
        let factors = test_factors();
        // 1.4 × 1.4 × 0.7 = 1.372
        let p = regime_product(&factors);
        assert!((p - 1.372).abs() < 0.001, "Expected ~1.372, got {p}");
    }

    #[test]
    fn regime_product_inactive_excluded() {
        let mut factors = test_factors();
        factors[2].active = false;
        factors[3].active = true;
        // 1.4 × 1.4 × 2.0 = 3.92
        let p = regime_product(&factors);
        assert!((p - 3.92).abs() < 0.001, "Expected ~3.92, got {p}");
    }

    #[test]
    fn regime_product_all_inactive_is_one() {
        let mut factors = test_factors();
        factors.iter_mut().for_each(|f| f.active = false);
        assert_eq!(regime_product(&factors), 1.0);
    }

    #[test]
    fn regime_product_single_active() {
        let mut factors = test_factors();
        factors.iter_mut().for_each(|f| f.active = false);
        factors[0].active = true;
        assert!((regime_product(&factors) - 1.4).abs() < 0.001);
    }

    // ── regime_summary ────────────────────────────────────────────────────────

    #[test]
    fn regime_summary_fields_present() {
        let factors = test_factors();
        let v = regime_summary(&factors);
        assert!(v["factors"].is_array());
        assert!(v["active_count"].is_number());
        assert!(v["product"].is_number());
        assert!(v["guardrail_collapse"].is_number());
        assert!(v["likelihood_amplifier_pct"].is_number());
    }

    #[test]
    fn regime_summary_active_count_correct() {
        let factors = test_factors();
        let v = regime_summary(&factors);
        assert_eq!(v["active_count"].as_u64().unwrap(), 3);
    }

    /// HONESTY (roadmap 2.3): the regime inspector reports STRUCTURAL PRESSURE driving the
    /// v2 guardrail-collapse coupler — NOT a v1 "adjusted prior" (HISTORICAL_ANCHOR × product).
    /// The v2 engine holds the prior FLAT and the regime product enters only through
    /// `guardrail_from_regime` as a bounded amplifier on the systemic likelihood. This locks
    /// that the panel sources that exact engine coupler and that the misleading v1 field is GONE.
    #[test]
    fn regime_summary_reports_guardrail_collapse_not_an_adjusted_prior() {
        // The discredited v1 fields must not be served anymore.
        let neutral = {
            let mut f = test_factors();
            f.iter_mut().for_each(|x| x.active = false);
            f
        };
        let v0 = regime_summary(&neutral);
        assert!(v0["adjusted_prior"].is_null(),
            "v1 'adjusted_prior' must no longer be served (it implied the regime moves the prior)");
        assert!(v0["adjusted_prior_pct"].is_null(),
            "v1 'adjusted_prior_pct' must no longer be served");
        // Neutral regime (product 1.0) → zero collapse, zero likelihood lift.
        assert_eq!(v0["product"].as_f64().unwrap(), 1.0);
        assert_eq!(v0["guardrail_collapse"].as_f64().unwrap(), 0.0);
        assert_eq!(v0["likelihood_amplifier_pct"].as_f64().unwrap(), 0.0);

        // An elevated regime reports EXACTLY the engine's own coupler + its bounded lift.
        let factors = vec![
            RegimeFactor { id: "a".into(), label: "A".into(), multiplier: 2.0, active: true },
            RegimeFactor { id: "b".into(), label: "B".into(), multiplier: 1.5, active: true },
        ];
        let product = regime_product(&factors); // 3.0
        let v = regime_summary(&factors);
        let g = guardrail_from_regime(product);
        assert!(g > 0.0 && g < 1.0, "product 3.0× sits mid-ramp, not saturated");
        assert!((v["guardrail_collapse"].as_f64().unwrap() - g).abs() < 1e-6,
            "panel must report the engine's own guardrail_from_regime(product)");
        assert!((v["likelihood_amplifier_pct"].as_f64().unwrap()
                 - 100.0 * GUARDRAIL_AMPLIFIER * g).abs() < 1e-6,
            "the lift is exactly the bounded guardrail amplifier on the systemic likelihood");

        // Past the saturation point the coupler clamps at full collapse (+max lift), never higher.
        let extreme = vec![
            RegimeFactor { id: "x".into(), label: "X".into(), multiplier: 10.0, active: true },
        ];
        let ve = regime_summary(&extreme);
        assert_eq!(ve["guardrail_collapse"].as_f64().unwrap(), 1.0);
        assert!((ve["likelihood_amplifier_pct"].as_f64().unwrap()
                 - 100.0 * GUARDRAIL_AMPLIFIER).abs() < 1e-6,
            "saturated collapse is capped at the max +{}% lift", GUARDRAIL_AMPLIFIER * 100.0);
    }

    // ── regime_warnings ────────────────────────────────────────────────────────

    #[test]
    fn regime_warnings_empty_below_threshold() {
        // Product = 1.372 — well below 20×
        let factors = test_factors();
        assert!(regime_warnings(&factors).is_empty(),
            "Product {:.3}× should produce no warnings", regime_product(&factors));
    }

    #[test]
    fn regime_warnings_triggered_above_threshold() {
        // Construct factors with product > 20×
        let factors = vec![
            RegimeFactor { id: "a".into(), label: "A".into(), multiplier: 5.0, active: true },
            RegimeFactor { id: "b".into(), label: "B".into(), multiplier: 5.0, active: true },
        ];
        // Product = 25.0 > 20.0
        let p = regime_product(&factors);
        assert!(p > REGIME_PRODUCT_WARN_THRESHOLD,
            "Test setup: product {p:.1} should exceed {REGIME_PRODUCT_WARN_THRESHOLD}");
        let warnings = regime_warnings(&factors);
        assert!(!warnings.is_empty(), "Product > 20× should generate a warning");
        assert!(warnings[0].contains("20"),
            "Warning should mention the threshold: {}", warnings[0]);
    }

    #[test]
    fn regime_warnings_at_exactly_threshold_not_triggered() {
        // Product = exactly 20.0 — threshold is strictly greater-than
        let factors = vec![
            RegimeFactor { id: "a".into(), label: "A".into(), multiplier: 4.0, active: true },
            RegimeFactor { id: "b".into(), label: "B".into(), multiplier: 5.0, active: true },
        ];
        // Product = 20.0 exactly — should NOT trigger (threshold is > not >=)
        let p = regime_product(&factors);
        assert!((p - 20.0).abs() < 0.001);
        assert!(regime_warnings(&factors).is_empty(),
            "Product exactly at threshold should not trigger warning");
    }

    #[test]
    fn regime_product_warn_threshold_is_20() {
        assert!((REGIME_PRODUCT_WARN_THRESHOLD - 20.0).abs() < 1e-9,
            "Warning threshold must be 20× (I-20 fix)");
    }

    // ── domain_description ────────────────────────────────────────────────────

    #[test]
    fn all_domain_ids_have_descriptions() {
        for &id in DOMAIN_IDS {
            let desc = domain_description(id);
            assert!(!desc.is_empty(), "No description for domain: {id}");
            assert_ne!(desc, "Unknown domain", "Generic description for domain: {id}");
        }
    }

    #[test]
    fn domain_descriptions_mention_domain() {
        assert!(domain_description("nuclear_posture").to_lowercase().contains("nuclear"));
        assert!(domain_description("cyber_info_ops").to_lowercase().contains("cyber"));
    }

    // ── check_key ─────────────────────────────────────────────────────────────

    #[test]
    fn constant_time_eq_matches_only_on_full_equality() {
        assert!(constant_time_eq("secret123", "secret123"));
        assert!(!constant_time_eq("secret123", "secret124")); // same length, last byte differs
        assert!(!constant_time_eq("secret", "secret123"));     // length mismatch
        assert!(!constant_time_eq("secret123", "secret"));     // length mismatch (other order)
        assert!(constant_time_eq("", ""));                      // both empty
        assert!(!constant_time_eq("x", ""));
    }

    #[test]
    fn check_key_valid() {
        let mut headers = HeaderMap::new();
        headers.insert("X-GCRM-Key", "secret123".parse().unwrap());
        assert!(check_key(&headers, "secret123").is_ok());
    }

    #[test]
    fn check_key_wrong_value() {
        let mut headers = HeaderMap::new();
        headers.insert("X-GCRM-Key", "wrong".parse().unwrap());
        assert!(check_key(&headers, "secret123").is_err());
    }

    #[test]
    fn check_key_missing_header() {
        let headers = HeaderMap::new();
        assert!(check_key(&headers, "secret123").is_err());
    }

    #[test]
    fn check_key_empty_key_always_rejects() {
        let mut headers = HeaderMap::new();
        headers.insert("X-GCRM-Key", "anything".parse().unwrap());
        let result = check_key(&headers, "");
        assert!(result.is_err());
        let (status, _) = result.unwrap_err();
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[test]
    fn check_key_empty_attempts_against_empty_key_still_rejected() {
        let mut headers = HeaderMap::new();
        headers.insert("X-GCRM-Key", "".parse().unwrap());
        assert!(check_key(&headers, "").is_err());
    }

    // ── Rate limiter ────────────────────────────────────────────────────────

    #[test]
    fn rate_bucket_starts_full() {
        let mut bucket = RateBucket::new();
        let (allowed, _) = bucket.try_consume();
        assert!(allowed, "First request on a fresh bucket should be allowed");
    }

    #[test]
    fn rate_bucket_allows_up_to_capacity() {
        let mut bucket = RateBucket::new();
        for i in 0..RATE_LIMIT_CAPACITY {
            let (allowed, _) = bucket.try_consume();
            assert!(allowed, "Request {i} of {RATE_LIMIT_CAPACITY} should be allowed");
        }
    }

    #[test]
    fn rate_bucket_rejects_when_empty() {
        let mut bucket = RateBucket::new();
        // Drain all tokens
        for _ in 0..RATE_LIMIT_CAPACITY {
            bucket.try_consume();
        }
        let (allowed, retry_after) = bucket.try_consume();
        assert!(!allowed, "Request after bucket exhaustion should be rejected");
        assert!(retry_after >= 1, "Retry-After should be at least 1 second, got {retry_after}");
    }

    #[test]
    fn rate_bucket_returns_correct_retry_after() {
        let mut bucket = RateBucket::new();
        for _ in 0..RATE_LIMIT_CAPACITY {
            bucket.try_consume();
        }
        let (_, retry_after) = bucket.try_consume();
        // Should be 1s since the bucket was just drained
        assert_eq!(retry_after, 1, "Retry-After should be 1s immediately after drain");
    }

    #[test]
    fn rate_limit_capacity_is_60() {
        assert_eq!(RATE_LIMIT_CAPACITY, 60,
            "Rate limit capacity must be 60 req/min (I-19 fix)");
    }

    #[test]
    fn check_rate_limit_allows_valid_key() {
        let buckets = DashMap::new();
        assert!(check_rate_limit("testkey", &buckets).is_ok());
    }

    #[test]
    fn check_rate_limit_rejects_after_exhaustion() {
        let buckets = DashMap::new();
        for _ in 0..RATE_LIMIT_CAPACITY {
            check_rate_limit("testkey", &buckets).unwrap();
        }
        let result = check_rate_limit("testkey", &buckets);
        assert!(result.is_err());
        let (status, _) = result.unwrap_err();
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    }

    #[test]
    fn rate_limit_independent_per_key() {
        let buckets = DashMap::new();
        // Drain key_a
        for _ in 0..RATE_LIMIT_CAPACITY {
            check_rate_limit("key_a", &buckets).unwrap();
        }
        // key_b should still have its full budget
        assert!(check_rate_limit("key_b", &buckets).is_ok(),
            "Different keys have independent rate buckets");
    }

    // ── multiplier bounds ─────────────────────────────────────────────────────

    #[test]
    fn multiplier_zero_is_invalid() {
        let m = 0.0f64;
        assert!(m <= 0.0);
    }

    #[test]
    fn multiplier_above_ten_is_invalid() {
        let m = 10.1f64;
        assert!(m > 10.0);
    }

    #[test]
    fn multiplier_one_is_valid() {
        let m = 1.0f64;
        assert!(m > 0.0 && m <= 10.0);
    }

    // ── operator_routes builds ────────────────────────────────────────────────

    #[test]
    fn operator_routes_have_all_expected_paths() {
        let _router = operator_routes();
    }
}
