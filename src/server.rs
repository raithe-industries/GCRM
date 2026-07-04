// ------------------------------------------------------------
// RAiTHE INDUSTRIES INCORPORATED
// Copyright (c) 2026 All Rights Reserved.
//
// This file is part of a proprietary system. Unauthorized use,
// reproduction, or distribution is strictly prohibited.
// ------------------------------------------------------------

// src/server.rs — Axum HTTP server + WebSocket broadcast
//
// Routes:
//   GET  /               → dashboard HTML
//   GET  /ws             → WebSocket (snapshot + timeline push on connect, then live)
//   GET  /api/latest     → latest snapshot JSON
//   GET  /api/timeline   → historical timeline (EpochStore, newest-first)
//   GET  /api/epoch      → full 4-year P(WWIII) history (?limit=N, ?since=<rfc3339>)
//   GET  /api/articles   → article store (newest-first, filterable)
//   GET  /api/sources    → feed registry + active source counts
//   GET  /api/nuclear    → nuclear alert status
//   GET  /api/health     → process health check

use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    response::{Html, IntoResponse, Json, Redirect},
    routing::get,
    Router,
};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::{broadcast, Mutex};
use tracing::{info, warn};

use crate::aggregator::{snapshot_to_json, SharedState};
use crate::ingestor::RSS_FEEDS;
use crate::models::RiskSnapshot;

// ── Broadcast channel capacity ────────────────────────────────────────────────
// 64 slots — enough for burst without unbounded growth.
const BROADCAST_CAP: usize = 200;

// ── Server state ──────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct ServerState {
    pub app_state:        SharedState,
    pub broadcast_tx:     broadcast::Sender<Arc<String>>,
    pub client_count:     Arc<Mutex<usize>>,
    pub dashboard_html:   Arc<String>,
    pub methodology_html: Arc<String>,
}

impl ServerState {
    pub fn new(app_state: SharedState, base_path: &str) -> (Self, broadcast::Sender<Arc<String>>) {
        let (tx, _) = broadcast::channel(BROADCAST_CAP);
        let html = Arc::new(generate_dashboard_html(base_path));
        // The alert-band prose is rendered from the engine's AlertSettings (the same
        // source the dashboard hero/timeline read live) so the methodology can never
        // disagree with the running classification — same anti-drift pattern as the
        // forecast ceiling above.
        let alerts = crate::models::AlertSettings::default();
        let methodology = Arc::new(
            render_base_path(METHODOLOGY_HTML, base_path)
                .replace("{{CALIBRATION_EVIDENCE}}", &crate::backtest::calibration_evidence_html())
                .replace("{{FORECAST_PROB_CEILING}}",
                         &format!("{:.2}", crate::models::FORECAST_PROB_CEILING))
                // P₀: the flat logistic baseline prior, rendered from the engine's own
                // BASELINE_ANNUAL so the whitepaper's quiet-year number can never drift
                // from the running prior (same anti-drift pattern as the forecast ceiling).
                .replace("{{BASELINE_ANNUAL_PCT}}",
                         &format!("{:.1}", crate::models::BASELINE_ANNUAL * 100.0))
                .replace("{{ALERT_ELEVATED}}", &format!("{:.1}%", alerts.elevated * 100.0))
                .replace("{{ALERT_CRITICAL}}", &format!("{:.1}%", alerts.critical * 100.0))
                .replace("{{ALERT_30D}}", &format!("{:.1}%", alerts.thirty_day_warn * 100.0))
                // Guardrail-collapse internals: how the operator-tunable regime factors
                // enter the model. Rendered from the engine's own coupler constants
                // (single source of truth) so the whitepaper's quantified mechanism can
                // never drift from bayesian::guardrail_from_regime — same anti-drift
                // pattern as the alert bands / forecast ceiling above.
                .replace("{{GUARDRAIL_AMPLIFIER_PCT}}",
                         &format!("{:.0}%", crate::bayesian::GUARDRAIL_AMPLIFIER * 100.0))
                .replace("{{GUARDRAIL_SATURATION_X}}",
                         &format!("{:.1}×", 1.0 + crate::bayesian::GUARDRAIL_REGIME_SPAN))
                // Systemic coupler magnitudes: the maximum lift each channel adds to
                // L_sys, rendered from the engine's own theater.rs constants (single
                // source of truth) so the whitepaper's quantified couplers — and the
                // design ordering brink > breadth (breadth never swamps the brink,
                // locked by breadth_never_swamps_the_nuclear_brink) — can never drift
                // from the running model. Same anti-drift pattern as the guardrail
                // figures above.
                .replace("{{COUPLING_GP_PCT}}",
                         &format!("{:.0}%", crate::theater::COUPLING_GP_WEIGHT * 100.0))
                .replace("{{COUPLING_ALLIANCE_PCT}}",
                         &format!("{:.0}%", crate::theater::COUPLING_ALLIANCE_WEIGHT * 100.0))
                .replace("{{GP_ENTANGLEMENT_SAT}}",
                         &format!("{:.0}", crate::theater::GP_ENTANGLEMENT_SATURATION))
                .replace("{{BREADTH_ASYMPTOTE_PCT}}",
                         &format!("{:.0}%", crate::theater::BREADTH_ASYMPTOTE * 100.0))
                .replace("{{BRINK_AMPLIFIER_PCT}}",
                         &format!("{:.0}%", crate::theater::BRINK_AMPLIFIER * 100.0))
                // Persistence floor (#persistence): the fraction of slow war-state heat the
                // floor holds, and the half-life stretch that turns the kinetic decay into a
                // multi-week one — rendered from theater.rs so the prose can't drift.
                .replace("{{FLOOR_FRACTION_PCT}}",
                         &format!("{:.0}%", crate::theater::FLOOR_FRACTION * 100.0))
                .replace("{{WAR_STATE_HALF_LIFE_SCALE}}",
                         &format!("{:.0}×", crate::theater::WAR_STATE_HALF_LIFE_SCALE)),
        );
        let state = Self {
            app_state,
            broadcast_tx:     tx.clone(),
            client_count:     Arc::new(Mutex::new(0)),
            dashboard_html:   html,
            methodology_html: methodology,
        };
        (state, tx)
    }
}

fn render_base_path(template: &str, base_path: &str) -> String {
    let bp = if base_path == "/" { "" } else { base_path };
    template.replace("{{BASE_PATH}}", bp)
            .replace("{{LOGO_VER}}", &logo_version())
}

fn generate_dashboard_html(base_path: &str) -> String {
    render_base_path(DASHBOARD_HTML, base_path)
        // The domain chart's "elevated" reference line reads its cutoff from the
        // model constant, so it can never drift from the engine's real threshold.
        .replace("{{ELEVATION_THRESHOLD}}",
                 &format!("{}", crate::models::ELEVATION_THRESHOLD))
        // The model-state footer's Bayesian chain and the "what this means" calibration
        // line both quote the flat quiet-year baseline prior. They render from
        // BASELINE_ANNUAL (single source of truth) so the operator-facing dashboard can
        // never quote a stale prior after a recalibration — same anti-drift guarantee the
        // methodology page already carries for P₀.
        .replace("{{BASELINE_ANNUAL_PCT}}",
                 &format!("{:.1}", crate::models::BASELINE_ANNUAL * 100.0))
        // The Confidence info-modal explains how the operator's data-quality score is
        // built. Its blend weights and saturation points render from the engine's own
        // CONF_W_*/CONFIDENCE_*_SATURATION constants (the same ones estimate_confidence
        // blends, single source of truth) so the explanation can never drift from the
        // running formula after a re-weighting — same anti-drift guarantee P₀ and the
        // elevation line already carry on this surface.
        .replace("{{CONF_W_DOMAIN}}", &format!("{:.1}", crate::bayesian::CONF_W_DOMAIN))
        .replace("{{CONF_W_EVENTS}}", &format!("{:.1}", crate::bayesian::CONF_W_EVENTS))
        .replace("{{CONF_W_SOURCES}}", &format!("{:.1}", crate::bayesian::CONF_W_SOURCES))
        .replace("{{CONFIDENCE_EVENT_SAT}}",
                 &format!("{:.0}", crate::bayesian::CONFIDENCE_EVENT_SATURATION))
        .replace("{{CONFIDENCE_SOURCE_SAT}}",
                 &format!("{:.0}", crate::bayesian::CONFIDENCE_SOURCE_SATURATION))
        // The "For scale" info line and the hero's vs-history positioning anchor the bare
        // P(WWIII)% to two crises an operator knows (Ukraine 2022, Cuba 1962). The two
        // poles render from the model's OWN output for those analogs (backtest::
        // analog_model_pct, the live engine via calibration_anchors), so the reference can
        // never drift from what the model actually produces — and the hero positions the
        // live read against them on the same scale. Same anti-drift pattern as P₀ above.
        .replace("{{ANALOG_UKRAINE_PCT}}",
                 &format!("{:.0}", crate::backtest::analog_model_pct("ukraine_2022").unwrap_or(39.0)))
        .replace("{{ANALOG_CUBA_PCT}}",
                 &format!("{:.0}", crate::backtest::analog_model_pct("cuba_1962").unwrap_or(80.0)))
}

// ── Snapshot broadcaster ──────────────────────────────────────────────────────
// Receives RiskSnapshot from aggregator, serialises, pushes to all WS clients.
// Also pushes article updates every 3 snapshots (~9s) — matches Python behaviour.
// Timeline history is maintained by EpochStore in AppState (aggregator.rs).

pub async fn broadcast_snapshots(
    mut snap_rx:    tokio::sync::mpsc::Receiver<RiskSnapshot>,
    server_state:   ServerState,
) {
    let mut article_push_counter = 0u32;

    while let Some(snap) = snap_rx.recv().await {
        let mut data = snapshot_to_json(&snap);

        // Merge model calibration timestamp so dashboard can show honest "MODEL UPDATED" indicator
        {
            let cal = server_state.app_state.last_calibrated_at.lock().await;
            data["model_calibrated_at"] = match *cal {
                Some(ref ts) => serde_json::Value::String(ts.to_rfc3339()),
                None         => serde_json::Value::Null,
            };
        }

        // Durable, server-computed trailing-6h trend (EpochStore::trend_6h). It
        // lives in the payload so the dashboard never reconstructs it from a
        // fragile per-tab session buffer — a UI refactor can no longer silently
        // reset the "6h Trend" readout to "—". The browser only renders it.
        {
            let es = server_state.app_state.epoch_store.lock().await;
            // Awareness layer: alongside the trend MAGNITUDE, report whether the locus of
            // risk relocated over the window. `lead_then` (the hottest theater 6h ago) comes
            // from the durable ring; `lead` (now) is read from the live snapshot via the same
            // `lead_theater` single source of truth, so a shift is judged consistently. The
            // browser renders `lead→X (was Y)` only when `lead_shifted`, so a stable leader
            // adds no clutter and the bare delta is never overstated as attribution.
            let mut t6 = es.trend_6h(snap.p_wwiii_annual);
            // Honesty layer: publish the headline as an INTERVAL, not a bare point, plus the
            // plain-language reference-class limit and error posture. The interval is computed
            // server-side from the durable ring (same discipline as trend_6h); the epistemic
            // text is the SINGLE source of truth in models.rs, so page prose can't drift from it.
            let uncertainty = es.uncertainty_6h(snap.p_wwiii_annual, snap.estimate_confidence);
            let lead_now = crate::models::lead_theater(&snap.theaters);
            // Honesty layer: a frozen "+0.000%" 6h trend is the model PEGGED at the top of its
            // range — the headline P clamped at the forecast ceiling AND zero empirical movement
            // across the window — not a calm flat line or a freeze/bug. Judged server-side from
            // the same durable ring (`empirical_hw_pct`) plus the headline P, so the browser only
            // renders the flag. The number itself is unchanged; this just names WHY it cannot
            // move. (Keys on the headline ceiling, not a per-theater heat clamp: post-de-saturation
            // heat asymptotes below 1.0, so the old heat-railed gate was permanently dead.) See
            // models::systemic_pegged.
            let empirical_hw_pct = uncertainty.get("empirical_hw_pct").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let samples = t6.get("samples").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let pegged = crate::models::systemic_pegged(snap.p_wwiii_annual, empirical_hw_pct, samples);
            if let Some(obj) = t6.as_object_mut() {
                let then = obj.get("lead_then").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let shifted = !lead_now.is_empty() && !then.is_empty() && lead_now != then;
                obj.insert("lead".into(), serde_json::Value::String(lead_now));
                obj.insert("lead_shifted".into(), serde_json::Value::Bool(shifted));
                obj.insert("pegged".into(), serde_json::Value::Bool(pegged));
            }
            data["trend_6h"] = t6;
            data["uncertainty"] = uncertainty;
        }
        data["epistemic"] = serde_json::json!({
            "reference_class": crate::models::EPISTEMIC_REFERENCE_CLASS,
            "error_posture":   crate::models::ERROR_POSTURE_NOTE,
        });

        // Update latest in shared state
        {
            let mut latest = server_state.app_state.latest_snapshot.lock().await;
            *latest = Some(data.clone());
        }

        // Broadcast snapshot to all connected WebSocket clients
        let payload = json!({"type": "snapshot", "data": data}).to_string();
        let _ = server_state.broadcast_tx.send(Arc::new(payload));

        // Article push every 3 snapshots
        article_push_counter += 1;
        if article_push_counter >= 3 {
            article_push_counter = 0;
            let store = server_state.app_state.article_store.lock().await;
            let total = store.len();
            let recent: Vec<_> = store.query(200, None, None)
                .into_iter()
                .cloned()
                .collect();
            drop(store);
            let art_payload = json!({
                "type":  "articles",
                "data":  recent,
                "total": total,
            }).to_string();
            let _ = server_state.broadcast_tx.send(Arc::new(art_payload));
        }
    }

    warn!("broadcast_snapshots: snapshot channel closed — server shutting down");
}

// ── WebSocket handler ─────────────────────────────────────────────────────────

async fn ws_handler(
    ws:    WebSocketUpgrade,
    State(state): State<ServerState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: ServerState) {
    // Increment client counter
    *state.client_count.lock().await += 1;

    // On connect: send latest snapshot immediately if available
    let latest = state.app_state.latest_snapshot.lock().await.clone();
    if let Some(snap) = latest {
        let msg = json!({"type": "snapshot", "data": snap}).to_string();
        if socket.send(Message::Text(msg)).await.is_err() {
            *state.client_count.lock().await -= 1;
            return;
        }
    }

    // Send a bounded slice of recent timeline history from EpochStore (in-memory, no disk
    // read). Capped per connect so a 4-day ring isn't cloned+serialized in full to every
    // client; the chart fills forward via live pushes, and /api/epoch?limit=N serves deeper
    // durable history on demand. (audit aggregator-1)
    let timeline: Vec<serde_json::Value> = {
        let es = state.app_state.epoch_store.lock().await;
        es.query(crate::aggregator::WS_TIMELINE_BOOTSTRAP).into_iter().cloned().collect()
    };
    let tl_msg = json!({"type": "timeline", "data": timeline}).to_string();
    if socket.send(Message::Text(tl_msg)).await.is_err() {
        *state.client_count.lock().await -= 1;
        return;
    }

    // Subscribe to live broadcast
    let mut rx = state.broadcast_tx.subscribe();

    loop {
        tokio::select! {
            // Forward broadcast messages to this client
            Ok(msg) = rx.recv() => {
                if socket.send(Message::Text((*msg).clone())).await.is_err() {
                    break;
                }
            }
            // Keep-alive: drain any incoming client messages (browser pings/close)
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(_))                       => break,
                    _                                  => {} // ignore pings/text
                }
            }
        }
    }

    *state.client_count.lock().await -= 1;
}

// ── REST handlers ─────────────────────────────────────────────────────────────

async fn get_latest(State(state): State<ServerState>) -> impl IntoResponse {
    let latest = state.app_state.latest_snapshot.lock().await.clone();
    Json(latest.unwrap_or_else(|| json!({})))
}

// /api/map — OSINT world-map payload: live ee-sources feeds (GeoJSON) + GCRM theater
// flashpoints + the ee-view layer registry & base-map catalogue. Feeds are fetched
// live and best-effort (per-source timeout), so a slow provider never blanks the map.
async fn get_map(State(state): State<ServerState>) -> impl IntoResponse {
    let snapshot = state.app_state.latest_snapshot.lock().await.clone();
    Json(crate::osint::map_payload(snapshot).await)
}

// /api/finance — ee-correlate Finance Radar over the live Yahoo market stream.
async fn get_finance() -> impl IntoResponse {
    Json(crate::osint::finance_payload().await)
}

#[derive(Deserialize)]
struct TimelineParams {
    limit: Option<usize>,
}

async fn get_timeline(
    State(state): State<ServerState>,
    Query(params): Query<TimelineParams>,
) -> impl IntoResponse {
    // Sane default cap when no explicit ?limit is given (an uncapped default cloned the whole
    // ring per request); callers wanting deeper history pass ?limit=N. (audit aggregator-1)
    let limit = params.limit.unwrap_or(crate::aggregator::WS_TIMELINE_BOOTSTRAP);
    let es = state.app_state.epoch_store.lock().await;
    let entries: Vec<serde_json::Value> = es.query(limit).into_iter().cloned().collect();
    let total = es.len();
    drop(es);
    Json(json!({"entries": entries, "total": total}))
}

// /api/epoch — full 4-year P(WWIII) history from the in-memory EpochStore ring.
// Supports optional ?limit=N and ?since=<rfc3339> for dashboard chart queries.
#[derive(Deserialize)]
struct EpochParams {
    limit: Option<usize>,
    since: Option<String>,
}

async fn get_epoch(
    State(state): State<ServerState>,
    Query(params): Query<EpochParams>,
) -> impl IntoResponse {
    let limit = params.limit.unwrap_or(usize::MAX);
    let es = state.app_state.epoch_store.lock().await;
    let entries: Vec<serde_json::Value> = es.query(limit)
        .into_iter()
        .filter(|e| {
            if let Some(ref since) = params.since {
                // TimelineEntry serializes its timestamp as "t" (models.rs), not "ts" —
                // the old "ts" lookup was always None, so `since` silently never filtered.
                e.get("t")
                    .and_then(|t| t.as_str())
                    .is_none_or(|ts| ts >= since.as_str())
            } else {
                true
            }
        })
        .cloned()
        .collect();
    let total = es.len();
    drop(es);
    let returned = entries.len();
    Json(json!({
        "entries": entries,
        "returned": returned,
        "total_in_store": total,
        "note": "Full 4-year P(WWIII) timeline. Newest-first. Use ?limit=N or ?since=<rfc3339> to filter.",
    }))
}

#[derive(Deserialize)]
struct ArticleParams {
    limit:  Option<usize>,
    source: Option<String>,
    domain: Option<String>,
}

async fn get_articles(
    State(state): State<ServerState>,
    Query(params): Query<ArticleParams>,
) -> impl IntoResponse {
    let limit  = params.limit.unwrap_or(2000);
    let store  = state.app_state.article_store.lock().await;
    let total  = store.len();
    let arts: Vec<_> = store.query(
        limit,
        params.source.as_deref(),
        params.domain.as_deref(),
    )
    .into_iter()
    .cloned()
    .collect();
    let shown = arts.len();
    Json(json!({"articles": arts, "total": total, "shown": shown}))
}

async fn get_sources(State(state): State<ServerState>) -> impl IntoResponse {
    // Honesty: count per-source from the ACTUAL article store (what is currently in
    // the feed), not the cumulative-since-boot registry. So "live/silent" reflects
    // which feeds are presently producing, and counts stay correct as old articles
    // rotate out of the window. A feed that died days ago no longer reads as "live".
    let counts: std::collections::HashMap<String, usize> = {
        let store = state.app_state.article_store.lock().await;
        let mut m = std::collections::HashMap::new();
        for a in store.articles.iter() {
            *m.entry(a.source.clone()).or_insert(0) += 1;
        }
        m
    };
    let configured: Vec<_> = RSS_FEEDS.iter().map(|f| json!({
        "url":    f.url,
        "source": f.source,
        "tier":   f.tier as u8,
    })).collect();
    // GNews/GDELT are search APIs, not RSS_FEEDS entries — without this block a
    // dark loop (e.g. GDELT 429-throttled for days) was invisible here, only
    // inferable from a silently empty store. (audit-news c)
    let search_apis = state.app_state.search_api_health.lock().await.clone();
    Json(json!({
        "active_sources":     counts,
        "configured_sources": configured,
        "total_configured":   RSS_FEEDS.len(),
        "search_apis":        search_apis,
        "search_apis_note":   "GNews/GDELT poll-loop health. Timestamps are RFC3339 UTC; \
                               consecutive_failures counts attempts since the last successful \
                               fetch+parse (0 = healthy).",
    }))
}

// Nuclear detector returns real seismic alerts from the detector module.
// Alerts are labelled honestly as seismic anomalies, not nuclear confirmations.
async fn get_nuclear(State(state): State<ServerState>) -> impl IntoResponse {
    let alerts = state.app_state.nuclear_alerts.lock().await;
    // "alert" only for corroborated/escalated or high-confidence events; lone
    // unverified anomalies keep the pill in "monitoring" instead of alarming.
    let significant = alerts.iter().any(|a| {
        a.level != crate::detector::SeismicAlertLevel::Anomaly || a.confidence >= 0.5
    });
    let status = if significant { "alert" } else { "monitoring" };
    Json(json!({
        "alerts":  *alerts,
        "count":   alerts.len(),
        "status":  status,
        "note":    "Seismic anomaly detection only. Does not confirm nuclear detonations.",
        "latency": "FDSN network data latency: ~3-8 minutes after event occurrence.",
    }))
}

async fn get_brief(State(state): State<ServerState>) -> impl IntoResponse {
    // v2 Phase 4: the cached AI analyst brief. Returns a pending placeholder until
    // the first generation (the task warms up a few seconds after boot).
    match state.app_state.analyst_brief.lock().await.clone() {
        Some(b) => Json(b),
        None => Json(json!({
            "text":   "Analyst brief is generating — check back shortly.",
            "source": "pending",
            "generated_at": serde_json::Value::Null,
        })),
    }
}

async fn get_health(State(state): State<ServerState>) -> impl IntoResponse {
    let clients       = *state.client_count.lock().await;
    let epoch_entries = state.app_state.epoch_store.lock().await.len();
    let latest        = state.app_state.latest_snapshot.lock().await.clone();
    let latest_at     = latest.and_then(|s| s["computed_at"].as_str().map(|t| t.to_string()));
    Json(json!({
        "status":        "ok",
        "clients":       clients,
        "epoch_entries": epoch_entries,
        "latest_at":     latest_at,
    }))
}

async fn get_dashboard(State(state): State<ServerState>) -> impl IntoResponse {
    Html((*state.dashboard_html).clone())
}

async fn get_methodology(State(state): State<ServerState>) -> impl IntoResponse {
    Html((*state.methodology_html).clone())
}

// RAiTHE "A" mark + full favicon set — vendored from assets/ and compiled into
// the binary so the dashboard stays a single self-contained artifact. NOTE:
// include_bytes! freezes these at BUILD time, so swapping a file on disk has no
// effect until `cargo build` reruns — a service restart alone keeps the old art.
static LOGO_A:      &[u8] = include_bytes!("../assets/logo-a.png");
static FAVICON_ICO: &[u8] = include_bytes!("../assets/favicon.ico");
static FAVICON_16:  &[u8] = include_bytes!("../assets/favicon-16x16.png");
static FAVICON_32:  &[u8] = include_bytes!("../assets/favicon-32x32.png");
static APPLE_TOUCH: &[u8] = include_bytes!("../assets/apple-touch-icon.png");
static ICON_192:    &[u8] = include_bytes!("../assets/icon-192.png");
static ICON_512:    &[u8] = include_bytes!("../assets/icon-512.png");

// Content-derived cache-buster. Every icon URL carries ?v=<logo_version()>, so a
// rebuilt binary with changed art yields a NEW url → the browser and the
// Cloudflare edge miss their day-long cached copy and refetch immediately. This
// is what makes "I swapped the file" actually show up without a manual purge.
fn logo_version() -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for a in [LOGO_A, FAVICON_ICO, ICON_192, ICON_512] { a.hash(&mut h); }
    format!("{:x}", h.finish())
}

fn icon(content_type: &'static str, bytes: &'static [u8]) -> impl IntoResponse {
    (
        [
            (axum::http::header::CONTENT_TYPE,  content_type),
            (axum::http::header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        bytes,
    )
}

async fn get_logo()        -> impl IntoResponse { icon("image/png",     LOGO_A) }
async fn get_favicon_ico() -> impl IntoResponse { icon("image/x-icon",  FAVICON_ICO) }
async fn get_favicon_16()  -> impl IntoResponse { icon("image/png",     FAVICON_16) }
async fn get_favicon_32()  -> impl IntoResponse { icon("image/png",     FAVICON_32) }
async fn get_apple_touch() -> impl IntoResponse { icon("image/png",     APPLE_TOUCH) }
async fn get_icon_192()    -> impl IntoResponse { icon("image/png",     ICON_192) }
async fn get_icon_512()    -> impl IntoResponse { icon("image/png",     ICON_512) }

// ── Router ────────────────────────────────────────────────────────────────────

pub fn build_router(state: ServerState, operator_state: crate::api::OperatorState, base_path: &str) -> Router {
    let inner = Router::new()
        .route("/",              get(get_dashboard))
        .route("/methodology",   get(get_methodology))
        .route("/logo-a.png",           get(get_logo))
        .route("/favicon.ico",          get(get_favicon_ico))
        .route("/favicon-16x16.png",    get(get_favicon_16))
        .route("/favicon-32x32.png",    get(get_favicon_32))
        .route("/apple-touch-icon.png", get(get_apple_touch))
        .route("/icon-192.png",         get(get_icon_192))
        .route("/icon-512.png",         get(get_icon_512))
        .route("/ws",            get(ws_handler))
        .route("/api/latest",    get(get_latest))
        .route("/api/timeline",  get(get_timeline))
        .route("/api/epoch",     get(get_epoch))
        .route("/api/articles",  get(get_articles))
        .route("/api/sources",   get(get_sources))
        .route("/api/nuclear",   get(get_nuclear))
        .route("/api/brief",     get(get_brief))
        .route("/api/health",    get(get_health))
        .route("/api/map",       get(get_map))
        .route("/api/finance",   get(get_finance))
        .with_state(state)
        .merge(crate::api::operator_routes().with_state(operator_state));

    let bp = if base_path == "/" { "" } else { base_path };
    if bp.is_empty() {
        inner
    } else {
        // axum's nest() serves the nested root at "/risk" but NOT "/risk/", so a
        // trailing-slash link (the methodology "back to dashboard" link uses
        // {{BASE_PATH}}/) 404s. Redirect "/risk/" → "/risk" to fix it.
        let target = bp.to_string();
        Router::new()
            .nest(bp, inner)
            .route(&format!("{bp}/"), get(move || {
                let t = target.clone();
                async move { Redirect::temporary(&t) }
            }))
    }
}

// ── Server entry point ────────────────────────────────────────────────────────

pub async fn serve(
    host:           String,
    port:           u16,
    state:          ServerState,
    operator_state: crate::api::OperatorState,
    base_path:      String,
) -> anyhow::Result<()> {
    let addr: SocketAddr = format!("{host}:{port}").parse()?;
    let bp = if base_path == "/" { String::new() } else { base_path.clone() };
    let router = build_router(state, operator_state, &bp);

    info!("Dashboard → http://localhost:{port}{bp}/");
    info!("WebSocket → ws://localhost:{port}{bp}/ws");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router).await?;
    Ok(())
}

// ── Dashboard HTML ────────────────────────────────────────────────────────────
// Identical to server.py DASHBOARD_HTML — no changes to the frontend.

const DASHBOARD_HTML: &str = include_str!("dashboard.html");

// ── Methodology / whitepaper page ─────────────────────────────────────────────
// Deep, accurate explanation of the entire risk model. Self-contained, dark
// theme matching the dashboard. Every constant and formula below mirrors the
// live engine in bayesian.rs — if the engine changes, update this page.
// `r##"..."##` delimiter so in-page `href="#anchor"` links are safe.

const METHODOLOGY_HTML: &str = include_str!("methodology.html");

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashboard_html_is_nonempty() {
        assert!(!DASHBOARD_HTML.is_empty());
    }

    #[test]
    fn dashboard_html_has_doctype() {
        assert!(DASHBOARD_HTML.starts_with("<!DOCTYPE html>"));
    }

    #[test]
    fn dashboard_html_has_websocket_connect() {
        assert!(DASHBOARD_HTML.contains("new WebSocket"));
    }

    #[test]
    fn dashboard_renders_the_honesty_interval_and_error_posture() {
        // Honesty layer: the hero must publish an interval (not just a point) and the error
        // posture, both server-driven. Lock the render hooks so a UI refactor can't silently
        // drop them and revert the headline to a bare false-precise number.
        assert!(DASHBOARD_HTML.contains("gauge-interval"), "headline must render the uncertainty interval");
        assert!(DASHBOARD_HTML.contains("gauge-posture"),  "headline must render the error posture");
        assert!(DASHBOARD_HTML.contains("d.uncertainty"),  "interval must read the server-computed uncertainty block");
        assert!(DASHBOARD_HTML.contains("d.epistemic"),    "posture must read the server-provided epistemic block");
    }

    #[test]
    fn dashboard_flags_a_floor_held_theater_instead_of_a_live_read() {
        // Honesty/awareness: when a theater's heat is HELD by the persistence floor (a remembered
        // war-state carried through a news gap) rather than fresh evidence, the ladder chip must
        // say so — the number is a memory, not a live read. Lock the render hooks so a UI refactor
        // can't silently drop the caveat and present a held read as live fighting.
        assert!(DASHBOARD_HTML.contains("held_by_floor"),
            "ladder chip must read the server-provided held_by_floor flag");
        assert!(DASHBOARD_HTML.contains("tl-held"),
            "ladder chip must render the held tag");
        // And when the floor holds the rung above what fresh evidence supports, the chip must name
        // the fresh-evidence rung so the operator sees how far the live read has decayed.
        assert!(DASHBOARD_HTML.contains("fresh_rung_label"),
            "ladder chip must read the server-provided fresh_rung_label");
        assert!(DASHBOARD_HTML.contains("fresh: "),
            "held chip must surface the fresh-evidence rung when the floor lifts it");
    }

    #[test]
    fn dashboard_renders_per_theater_escalation_momentum() {
        // Awareness: the ladder chip must surface each theater's escalation MOMENTUM (the
        // recency-weighted DIRECTION of its news flow, server field `escalation_momentum`) — a
        // leading signal distinct from the heat trend. Lock the render hooks so a UI refactor
        // can't silently drop the gauge. Shown only when the flow is decisively one-sided, with
        // de-escalation (talks) and escalation framed distinctly.
        assert!(DASHBOARD_HTML.contains("escalation_momentum"),
            "ladder chip must read the server-provided escalation_momentum gauge");
        assert!(DASHBOARD_HTML.contains("tl-mom"),
            "ladder chip must render the momentum tag");
        assert!(DASHBOARD_HTML.contains("⇩ talks") && DASHBOARD_HTML.contains("⇧ escalatory"),
            "the momentum tag must name both the de-escalation (talks) and escalation directions");
    }

    #[test]
    fn dashboard_renders_systemic_news_flow_direction() {
        // Awareness (LEADING): the hero must surface the SYSTEMIC news-flow direction — the
        // heat-weighted aggregate of the per-theater momentum (server field
        // couplers.systemic_momentum) — so an operator sees which way the WHOLE board is tilting
        // before the lagging headline delta moves. Lock the render hooks so a UI refactor can't
        // silently drop the gauge, and so it keeps reading the SERVER field (not a client recompute).
        assert!(DASHBOARD_HTML.contains("gauge-momentum"),
            "hero must carry the systemic news-flow direction element");
        assert!(DASHBOARD_HTML.contains("couplers.systemic_momentum"),
            "the readout must read the server-provided couplers.systemic_momentum aggregate");
        assert!(DASHBOARD_HTML.contains("⇩ news flow de-escalating")
            && DASHBOARD_HTML.contains("⇧ news flow escalating"),
            "the readout must name both the de-escalation and escalation systemic directions");
    }

    #[test]
    fn dashboard_map_popup_flags_a_floor_held_theater_not_a_live_read() {
        // Honesty/awareness on the MAP surface: the world-map flashpoint popup is the only
        // operator surface that must not paint a floor-held theater (a remembered war-state
        // carried through a news gap) identical to a live-hot one — the same persistence-floor
        // contract the ladder chip (above) and hero already enforce. Lock the popup render hooks
        // so a UI refactor can't silently drop the caveat. The flags reach the popup via the
        // theater GeoJSON feature (osint::build_theater_features), locked separately there.
        assert!(DASHBOARD_HTML.contains("heldLine"),
            "map popup must build a held caveat line for a floor-held theater");
        assert!(DASHBOARD_HTML.contains("p.held_by_floor"),
            "map popup must read the feature's held_by_floor flag");
        assert!(DASHBOARD_HTML.contains("p.fresh_rung_label"),
            "map popup must read the feature's fresh_rung_label to show how far the read decayed");
    }

    #[test]
    fn dashboard_flags_a_floor_held_headline_not_a_live_read() {
        // Honesty/awareness at the HEADLINE (the at-a-glance read): when the lead theater driving
        // the systemic index is HELD by the persistence floor, the hero must carry a caveat so a
        // memory-held P(WWIII) can't masquerade as fresh fighting. Lock the render hooks so a UI
        // refactor can't silently drop it. Driven by meta.read_held_by_floor.
        assert!(DASHBOARD_HTML.contains("gauge-held"),
            "hero must carry the floor-held caveat element");
        assert!(DASHBOARD_HTML.contains("read_held_by_floor"),
            "hero caveat must read the server-provided read_held_by_floor flag");
    }

    #[test]
    fn dashboard_html_uses_live_endpoints() {
        // The cockpit is WebSocket-driven with the live article feed + sources + epoch.
        assert!(DASHBOARD_HTML.contains("/ws"));
        assert!(DASHBOARD_HTML.contains("/api/articles"));
        assert!(DASHBOARD_HTML.contains("/api/sources"));
        assert!(DASHBOARD_HTML.contains("/api/epoch"));
    }

    #[test]
    fn dashboard_html_renders_6h_trend_from_server_field() {
        // Guards the recurring "6h Trend = —" regression: the readout element must
        // exist AND the page must consume the durable server-computed `trend_6h`
        // field (not only the fragile client-side session buffer). If a dashboard
        // refactor drops either, this fails `cargo test` and the self-improve
        // routine can't ship it. Pairs with EpochStore::trend_6h + its unit tests.
        assert!(
            DASHBOARD_HTML.contains("cmd-trend"),
            "dashboard dropped the #cmd-trend (6h Trend) readout"
        );
        assert!(
            DASHBOARD_HTML.contains("trend_6h"),
            "dashboard no longer reads the server-computed trend_6h field — \
             the 6h Trend would silently revert to the broken client-buffer path"
        );
    }

    #[test]
    fn dashboard_renders_6h_trend_lead_shift() {
        // Awareness (pillar 3 — show WHERE): the 6h-trend sub-line must surface a relocation
        // of the hottest theater (`lead→X (was Y)`) when the server flags `lead_shifted`. The
        // magnitude alone can't show a net-flat headline hiding one theater cooling as another
        // heats. If a refactor drops the client read of the server flag, this fails the suite.
        assert!(
            DASHBOARD_HTML.contains("lead_shifted"),
            "dashboard no longer consumes the server-computed trend lead_shifted flag — \
             the locus-of-risk relocation would silently stop showing"
        );
        assert!(
            DASHBOARD_HTML.contains("lead→"),
            "dashboard dropped the lead-shift readout text"
        );
    }

    #[test]
    fn dashboard_renders_pegged_at_ceiling_honesty_flag() {
        // Honesty (pillar 1 — never let a frozen number read as calm): the 6h-trend cell must
        // consume the server-computed `pegged` flag (models::systemic_pegged) so a +0.000%
        // produced by the model being railed at its dynamic-range ceiling is surfaced as
        // "pegged at model ceiling", not a reassuring flat line. If a refactor drops the read,
        // this fails `cargo test`. Pairs with models::systemic_pegged + its unit test.
        assert!(
            DASHBOARD_HTML.contains("pegged"),
            "dashboard no longer consumes the server `pegged` flag — a ceiling-pegged read \
             would silently revert to looking like a calm flat trend"
        );
        assert!(
            DASHBOARD_HTML.contains("resolution exhausted"),
            "dashboard dropped the pegged-at-ceiling explanation text"
        );
    }

    #[test]
    fn dashboard_warns_when_the_live_read_goes_stale() {
        // Pillar-1 honesty: the header status must not keep asserting "Live" with a
        // frozen timestamp when snapshots stop arriving (stalled worker, or a WS that
        // hangs with TCP still open and no onclose). A real-time read that silently
        // freezes is a lie. The dashboard must gate the "Live" label on the actual
        // data age via a freshness watchdog, and surface a STALE warning otherwise.
        // The watchdog must run on a timer (so it fires WITHOUT a new snapshot — the
        // exact stall case) and the "Live" text must come from renderFreshness, not an
        // unconditional set on snapshot receipt.
        assert!(
            DASHBOARD_HTML.contains("renderFreshness"),
            "dashboard dropped the freshness watchdog — a stalled read could keep claiming Live"
        );
        assert!(
            DASHBOARD_HTML.contains("STALE"),
            "dashboard no longer surfaces a STALE state — a frozen read would masquerade as Live"
        );
        assert!(
            DASHBOARD_HTML.contains("_lastSnapMs"),
            "dashboard no longer tracks snapshot receipt time — staleness can't be measured"
        );
        assert!(
            DASHBOARD_HTML.contains("setInterval(renderFreshness"),
            "freshness watchdog is not on a timer — it would never fire during an actual stall \
             (no new snapshot to trigger it)"
        );
        // The only place the header status is set to "Live" must be renderFreshness,
        // gated on age — never an unconditional set in the snapshot handler. A revert
        // to a bare `ts.textContent='Live · '...` outside the watchdog resurrects the lie.
        assert_eq!(
            DASHBOARD_HTML.matches("'Live · '").count(),
            1,
            "the \"Live\" header label must be produced only by the age-gated renderFreshness \
             watchdog (a second, ungated set would let a stale read claim Live)"
        );
    }

    #[test]
    fn dashboard_flags_a_capped_read_instead_of_a_measured_ceiling() {
        // Pillar-1 honesty: when the forecast is hard-clamped to FORECAST_PROB_CEILING,
        // the displayed number is a FLOOR, not a point estimate. A bare "90.0%" would
        // masquerade as a measured 90%; the hero must read "≥" + carry a "capped" caveat,
        // both gated on the server-computed `at_ceiling` flag (single source of truth:
        // bayesian::is_at_forecast_ceiling).
        assert!(
            DASHBOARD_HTML.contains("at_ceiling"),
            "dashboard no longer reads the server at_ceiling flag — a clamped read would show a bare measured 90%"
        );
        assert!(
            DASHBOARD_HTML.contains("_atCeiling"),
            "dashboard no longer tracks the capped state"
        );
        assert!(
            DASHBOARD_HTML.contains("gauge-cap") && DASHBOARD_HTML.contains("capped at ceiling"),
            "dashboard dropped the capped caveat — a clamped read would masquerade as a measured ceiling"
        );
        // The hero value must prefix "≥" when capped (a floor, not a point estimate).
        assert!(
            DASHBOARD_HTML.contains("(_atCeiling?'≥':'')+(pA*100).toFixed(1)"),
            "hero P(WWIII) no longer marks a capped read with ≥"
        );
    }

    #[test]
    fn dashboard_flags_a_breadth_saturated_read_as_a_structural_max() {
        // Pillar-1 honesty + awareness: when every systemic breadth amplifier is railed (top
        // heat at the model max, gp-entanglement + alliance both maxed, no live nuclear brink),
        // the read is a STRUCTURAL MAXIMUM that the current crises intensifying can no longer
        // move. That peg sits BELOW FORECAST_PROB_CEILING, so the `at_ceiling`/`gauge-cap`
        // caveat never fires — a bare % would masquerade as a still-climbing point estimate.
        // The hero must carry a caveat gated on the server-computed `breadth_saturated` flag
        // (single source of truth: theater::compute → couplers.breadth_saturated, mirrored to
        // meta.breadth_saturated in aggregator.rs and disclosed in the analyst brief).
        assert!(
            DASHBOARD_HTML.contains("breadth_saturated"),
            "dashboard no longer reads the server breadth_saturated flag — a railed structural-max read would masquerade as a still-climbing number"
        );
        assert!(
            DASHBOARD_HTML.contains("gauge-saturated") && DASHBOARD_HTML.contains("structural max"),
            "dashboard dropped the breadth-saturated caveat — a railed peg would read as a precise point estimate that could still rise"
        );
    }

    #[test]
    fn dashboard_flags_a_blind_read_instead_of_claiming_live() {
        // Pillar-1 honesty: snapshots can keep arriving (connection Live, watchdog quiet)
        // while the window holds ZERO live events — a feed outage / cold start. Then the
        // headline is the BASELINE PRIOR, not a measurement, and a calm green ~1.5% read
        // is indistinguishable from a genuinely quiet world. The header must NOT say
        // "Live" in that state; it must surface a no-live-signal warning gated on the
        // server-computed `data_blind` flag (single source of truth: bayesian::is_data_blind).
        assert!(
            DASHBOARD_HTML.contains("data_blind"),
            "dashboard no longer reads the server data_blind flag — a blind read would claim Live"
        );
        assert!(
            DASHBOARD_HTML.contains("NO LIVE SIGNAL"),
            "dashboard dropped the no-live-signal warning — a baseline-only read would masquerade as a measured calm world"
        );
        assert!(
            DASHBOARD_HTML.contains("_dataBlind"),
            "dashboard no longer tracks the blind state for the freshness watchdog"
        );
        // The blind warning must live INSIDE renderFreshness (the age-gated header
        // watchdog), so STALE still takes precedence and the warning re-renders on the
        // timer even without a new snapshot.
        let fresh = &DASHBOARD_HTML[DASHBOARD_HTML.find("function renderFreshness").expect("renderFreshness present")..];
        let fresh = &fresh[..fresh.find("setInterval(renderFreshness").unwrap_or(fresh.len())];
        assert!(
            fresh.contains("NO LIVE SIGNAL") && fresh.contains("_dataBlind"),
            "the blind warning must be produced by the age-gated renderFreshness watchdog, after the STALE check"
        );
    }

    #[test]
    fn dashboard_flags_a_thinly_sourced_read_instead_of_full_coverage_live() {
        // Pillar-1 honesty, partial-outage sibling of the blind-read warning: a window
        // can hold live events (so it is NOT blind, watchdog quiet) yet draw them from
        // only one or two feeds — a feed-fleet partial outage. The headline is then a
        // real measurement but rests on a narrow base, so a flat "Live" overstates how
        // broadly corroborated it is. The header must caveat it, gated on the
        // server-computed `thinly_sourced` flag (single source of truth:
        // bayesian::is_thinly_sourced), inside the age-gated watchdog AFTER the blind check.
        assert!(
            DASHBOARD_HTML.contains("thinly_sourced"),
            "dashboard no longer reads the server thinly_sourced flag — a one-feed read would claim full-coverage Live"
        );
        assert!(
            DASHBOARD_HTML.contains("THIN COVERAGE"),
            "dashboard dropped the thin-coverage warning — a narrowly-sourced read would masquerade as fully corroborated"
        );
        let fresh = &DASHBOARD_HTML[DASHBOARD_HTML.find("function renderFreshness").expect("renderFreshness present")..];
        let fresh = &fresh[..fresh.find("setInterval(renderFreshness").unwrap_or(fresh.len())];
        assert!(
            fresh.contains("THIN COVERAGE") && fresh.contains("_thinSourced"),
            "the thin-coverage warning must be produced by the age-gated renderFreshness watchdog"
        );
        // Ordering: the stronger blind state must be checked before thin coverage, so a
        // zero-event read reads as NO LIVE SIGNAL, not the weaker THIN COVERAGE.
        assert!(
            fresh.find("NO LIVE SIGNAL").unwrap() < fresh.find("THIN COVERAGE").unwrap(),
            "the blind check must precede the thin-coverage check (blind is the stronger state)"
        );
    }

    #[test]
    fn dashboard_iw_board_flags_a_blind_read_instead_of_a_calm_all_clear() {
        // Pillar-1 honesty, board surface: during a blind read (zero live events) every
        // theater/coupler I&W light derives from NO signal, so the lights all read "clear"
        // and the board summary would say a reassuring grey "0 / 12 tripped" — a calm
        // all-clear indistinguishable from a genuinely quiet world. The board is its own
        // operator surface (its own summary line), so it must carry the same blind-read
        // honesty the header watchdog (2.6) added, gated on the SAME `_dataBlind` flag.
        let render = &DASHBOARD_HTML[DASHBOARD_HTML.find("function renderIndicators").expect("renderIndicators present")..];
        let render = &render[..render.find("function applyData").unwrap_or(render.len())];
        assert!(
            render.contains("_dataBlind"),
            "the I&W board summary no longer consults the blind-read flag — a blackout reads as a calm all-clear"
        );
        assert!(
            render.contains("all-clear unconfirmed"),
            "the I&W board dropped the blind-read qualifier — a 0/N all-clear would masquerade as a measured quiet board"
        );
        // A light that IS tripped during a blind read (e.g. the independent seismic
        // monitor) must still be surfaced, not buried under the no-signal note.
        assert!(
            render.contains("no live event signal"),
            "a real trip during a blind read must still show its count, tagged as running on no live event signal"
        );
    }

    #[test]
    fn dashboard_iw_board_flags_a_thinly_sourced_read_instead_of_a_full_coverage_all_clear() {
        // Pillar-1 honesty, board surface: the same partial-outage hole the header's
        // thin-coverage warning closed (thinly_sourced) also exists on the I&W board. A
        // thin read (live events from fewer than the corroboration floor of feeds) leaves
        // every light derived from a narrow base, so a grey "0 / N tripped" all-clear
        // overstates how broadly the quiet is corroborated — the board analog of a flat
        // "Live". The board summary must caveat it, gated on the SAME server-computed
        // `_thinSourced` flag the header reads, and AFTER the stronger blind branch.
        let render = &DASHBOARD_HTML[DASHBOARD_HTML.find("function renderIndicators").expect("renderIndicators present")..];
        let render = &render[..render.find("function applyData").unwrap_or(render.len())];
        assert!(
            render.contains("_thinSourced"),
            "the I&W board summary no longer consults the thin-coverage flag — a one-feed read reads as a full-coverage all-clear"
        );
        assert!(
            render.contains("thin coverage"),
            "the I&W board dropped the thin-coverage qualifier — a narrowly-sourced all-clear would masquerade as broadly corroborated"
        );
        // Ordering: blind (zero events) is the stronger state and must be checked before
        // the thin branch, so a blackout reads "no live signal", not the weaker "thin".
        assert!(
            render.find("_dataBlind").unwrap() < render.find("_thinSourced").unwrap(),
            "the blind check must precede the thin-coverage check on the board (blind is the stronger state)"
        );
    }

    #[test]
    fn dashboard_iw_board_flags_a_stale_read_instead_of_a_frozen_all_clear() {
        // Pillar-1 honesty, board surface: the header freshness watchdog flips to STALE
        // when snapshots stop arriving, but renderIndicators (which writes the I&W board
        // summary) runs ONLY on snapshot arrival — by definition never during a stall.
        // So a stalled read leaves the board summary frozen on its last trip count,
        // presenting a stale "0 / N tripped" all-clear as a current read: the board
        // analog of the header lie the watchdog (2.5) catches. The watchdog must re-flag
        // the board summary as STALE on the timer, from cached counts, so the board can't
        // go quiet while the world goes unwatched.
        let fresh = &DASHBOARD_HTML[DASHBOARD_HTML.find("function renderFreshness").expect("renderFreshness present")..];
        let fresh = &fresh[..fresh.find("setInterval(renderFreshness").unwrap_or(fresh.len())];
        // The board re-flag must live in the age-gated STALE branch (so it fires on the
        // timer without a new snapshot — the exact stall case), and address the board's
        // own summary element.
        let stale_branch = &fresh[fresh.find("age>STALE_AFTER_MS").expect("stale branch present")..];
        let stale_branch = &stale_branch[..stale_branch.find("_dataBlind").unwrap_or(stale_branch.len())];
        assert!(
            stale_branch.contains("iw-summary"),
            "the STALE branch must re-flag the I&W board summary — otherwise a stalled board keeps a frozen all-clear"
        );
        assert!(
            stale_branch.contains("STALE"),
            "the board re-flag must carry the STALE qualifier so the frozen count reads as not-current"
        );
        // The watchdog reconstructs the summary from counts cached by renderIndicators
        // (it has no fresh `inds` during a stall) — guard both the cache write and read.
        assert!(
            stale_branch.contains("_lastTripped") && stale_branch.contains("_lastIndsLen"),
            "the STALE board re-flag must use the cached trip/total counts (no live inds during a stall)"
        );
        let render = &DASHBOARD_HTML[DASHBOARD_HTML.find("function renderIndicators").expect("renderIndicators present")..];
        let render = &render[..render.find("function applyData").unwrap_or(render.len())];
        assert!(
            render.contains("_lastTripped=tripped") && render.contains("_lastApexTrip=apexTrip"),
            "renderIndicators must cache the board counts so the freshness watchdog can re-flag them as STALE"
        );
    }

    #[test]
    fn dashboard_left_rail_scrolls_instead_of_clipping_on_short_viewports() {
        // Pillar-2 legibility: the cockpit is a fixed-height (100vh, body overflow
        // hidden) 3-column grid. The left rail (gauge → windows → "what this means" →
        // Full-methodology button → brand foot) is taller than a short laptop/landscape
        // viewport, so it MUST scroll within its track. As a CSS-grid item it has the
        // default `min-height:auto`, which lets the item grow past its row track to fit
        // its content — so its own `overflow-y:auto` sees no overflow, never shows a
        // scrollbar, and the methodology button + brand foot get clipped below the fold
        // with no way to reach them (the exact 2.1 symptom). `min-height:0` makes the
        // item respect the track height so the scrollbar engages. Guard both halves of
        // the contract so a refactor can't drop either and silently re-clip the rail.
        let rule = DASHBOARD_HTML
            .split(".left-panel{")
            .nth(1)
            .and_then(|s| s.split('}').next())
            .expect("dashboard lost the .left-panel rule");
        assert!(
            rule.contains("overflow-y:auto"),
            "left rail must scroll its overflow (overflow-y:auto) so a short viewport can reach \
             the methodology button"
        );
        assert!(
            rule.contains("min-height:0"),
            "left rail needs min-height:0 — without it the grid item grows past its track and \
             overflow-y:auto never engages, clipping the methodology button off-screen"
        );
    }

    #[test]
    fn dashboard_center_column_scrolls_instead_of_clipping_on_short_viewports() {
        // Pillar-2 legibility (2.1), the vertical twin of the left-rail fix above. The
        // ≤680px rule handles NARROW viewports; nothing handled SHORT ones. On a short,
        // wide viewport (landscape phone, split-screen, a 480p projector) the center
        // column — domains → theater ladder → I&W board (all flex-shrink:0) → charts
        // (flex:1) — is overflow:hidden with no scroll, so the fixed strips crush the
        // charts toward zero height and the bottom card clips below the fold with no way
        // to reach it. The fix is a `max-height` media query that lets the PAGE scroll and
        // pins the charts to explicit heights (the latter also kills the Chart.js
        // no-bounded-height resize loop). Guard the contract so a refactor can't silently
        // drop the short-viewport safety and re-clip the center column.
        let rule = DASHBOARD_HTML
            .split("@media(max-height:640px){")
            .nth(1)
            .and_then(|s| s.split('}').next())
            .expect("dashboard lost the short-viewport (@media max-height) rule");
        assert!(
            rule.contains("overflow-y:auto"),
            "short-viewport rule must let the page scroll (body overflow-y:auto) so a clipped \
             center card can be reached"
        );
        // The charts need an explicit height in this mode — otherwise the flex:1 charts
        // still collapse AND Chart.js's responsive canvas loops on an unbounded parent.
        let block = DASHBOARD_HTML
            .split("@media(max-height:640px){")
            .nth(1)
            .and_then(|s| s.split("</style>").next())
            .expect("short-viewport rule must live inside the <style> block");
        assert!(
            block.contains(".chart-inner{height:") && block.contains("flex:none"),
            "short-viewport rule must pin the charts to an explicit height (flex:none) — without \
             it the charts squish to zero and Chart.js hits the resize→render loop"
        );
    }

    #[test]
    fn nuke_banner_formats_magnitude_and_depth_to_one_decimal() {
        // Pillar-2 legibility: the red seismic-anomaly banner is the most prominent,
        // highest-stakes element on the dashboard. It built its text from the raw
        // JSON `magnitude`/`depth_km` floats, so a depth of 0.7331km or a magnitude
        // of 5.2999999 rendered with full float noise — while the operator-panel
        // seismic list right beside it already formatted both to one decimal
        // (`a.magnitude?.toFixed(1)`). The banner must match: a number rendered with
        // garbled precision on the apex alert reads as broken, which per the mission
        // is a FAILED render even when the value is correct. Guard the formatted form
        // and forbid a revert to the raw concatenation.
        //
        // The fields are now finiteness-gated (audit dashboard-10) before .toFixed(1), so a
        // partial alert payload renders 'M?' / omits the field instead of 'Mundefined'/'NaNkm'
        // — the optional-chaining `?.` form was itself the bug (undefined?.toFixed → undefined
        // → "Mundefined"). The one-decimal contract still holds.
        assert!(
            DASHBOARD_HTML.contains("top.magnitude.toFixed(1)"),
            "nuke banner must format magnitude to one decimal (it was rendering raw float noise)"
        );
        assert!(
            DASHBOARD_HTML.contains("top.depth_km.toFixed(1)"),
            "nuke banner must format depth to one decimal (it was rendering raw float noise)"
        );
        assert!(
            DASHBOARD_HTML.contains("Number.isFinite(top.magnitude)"),
            "nuke banner must finiteness-guard magnitude so a partial payload can't render 'Mundefined'"
        );
        assert!(
            !DASHBOARD_HTML.contains("'M'+top.magnitude+' depth='"),
            "nuke banner reverted to the raw unformatted magnitude/depth concatenation"
        );
    }

    #[test]
    fn dashboard_renders_historical_analogs_from_the_model() {
        // The hero's vs-history positioning and the "For scale" info line anchor the bare
        // P(WWIII)% to two crises an operator knows (Ukraine 2022, Cuba 1962). Both poles
        // MUST render from the model's own analog output (templated), never a hand-typed
        // literal that could silently drift after a recalibration — and the hero compares
        // the live read against them on the same scale, so a stale pole would mis-position
        // the read. The template carries the placeholders; the render substitutes them with
        // backtest::analog_model_pct so the reference can never lie about the model's own scale.
        assert!(DASHBOARD_HTML.contains("{{ANALOG_UKRAINE_PCT}}"),
            "dashboard template lost the Ukraine-analog placeholder");
        assert!(DASHBOARD_HTML.contains("{{ANALOG_CUBA_PCT}}"),
            "dashboard template lost the Cuba-analog placeholder");
        assert!(DASHBOARD_HTML.contains("function renderHistContext"),
            "dashboard dropped the historical-positioning readout");
        assert!(DASHBOARD_HTML.contains("id=\"gauge-hist\""),
            "dashboard dropped the hero vs-history element");
        let rendered = generate_dashboard_html("/risk");
        assert!(!rendered.contains("{{ANALOG_UKRAINE_PCT}}") && !rendered.contains("{{ANALOG_CUBA_PCT}}"),
            "analog placeholders were not substituted at render time");
        let ukr = format!("{:.0}", crate::backtest::analog_model_pct("ukraine_2022").unwrap());
        let cuba = format!("{:.0}", crate::backtest::analog_model_pct("cuba_1962").unwrap());
        // The rendered poles must equal the live model's own analog output (one consistent scale).
        assert!(rendered.contains(&format!("const HIST_UKR={}/100,HIST_CUBA={}/100", ukr, cuba)),
            "rendered hero poles must embed the model's Ukraine ({ukr}) / Cuba ({cuba}) analog scores");
    }

    #[test]
    fn dashboard_html_renders_elevation_threshold_from_model() {
        // The domain bar chart draws a dashed "elevated" reference line so an
        // operator can see at a glance which force domains have crossed the cutoff
        // that feeds the co-occurrence amplifier. The line's value MUST come from
        // the model (templated), not a hand-typed JS literal that could silently
        // drift from the engine — that would be a dishonest render.
        assert!(
            DASHBOARD_HTML.contains("{{ELEVATION_THRESHOLD}}"),
            "dashboard template lost the elevation-threshold placeholder"
        );
        assert!(
            DASHBOARD_HTML.contains("elevLine"),
            "dashboard dropped the elevation reference-line canvas plugin"
        );
        let rendered = generate_dashboard_html("/risk");
        assert!(
            !rendered.contains("{{ELEVATION_THRESHOLD}}"),
            "elevation-threshold placeholder was not substituted at render time"
        );
        // The rendered JS constant must equal the live model threshold, so the line
        // can never lie about where "elevated" begins.
        assert!(
            rendered.contains(&format!(
                "const ELEV_THRESH={}",
                crate::models::ELEVATION_THRESHOLD
            )),
            "rendered dashboard must embed models::ELEVATION_THRESHOLD ({}) as ELEV_THRESH",
            crate::models::ELEVATION_THRESHOLD
        );
    }

    #[test]
    fn dashboard_html_draws_alert_bands_from_live_thresholds() {
        // The timeline draws dashed "elevated"/"critical" reference lines so an
        // operator sees how close the live read is to the alert bands. Their values
        // MUST come from each snapshot's live thresholds (d.alert.*_threshold),
        // never a hardcoded JS literal that could silently drift from AlertSettings.
        assert!(
            DASHBOARD_HTML.contains("alertBands"),
            "dashboard dropped the timeline alert-band canvas plugin"
        );
        assert!(
            DASHBOARD_HTML.contains("d.alert.critical_threshold")
                && DASHBOARD_HTML.contains("d.alert.elevated_threshold"),
            "dashboard must adopt the alert-band thresholds from the live snapshot"
        );
        // Risk colours must read the live vars, not the old hardcoded thresholds.
        assert!(
            DASHBOARD_HTML.contains("p>=ALERT_CRIT?'var(--red)':p>=ALERT_ELEV"),
            "pc() must colour risk from the live ALERT_CRIT/ALERT_ELEV thresholds"
        );
        assert!(
            !DASHBOARD_HTML.contains("p>=.08?'var(--red)':p>=.025"),
            "the drift-prone hardcoded P(WWIII) alert-band literals must be gone"
        );
    }

    #[test]
    fn dashboard_surfaces_the_systemic_coupling_driver() {
        // Awareness "why" at the systemic level: the model-state footer must name the
        // dominant coupling amplifier (what is turning a regional crisis into a world-war
        // risk) from the LIVE engine coupler — never a hand-typed label.
        assert!(
            DASHBOARD_HTML.contains("d.couplers.coupling_driver"),
            "footer must read the live coupling_driver coupler"
        );
        assert!(
            DASHBOARD_HTML.contains("led by "),
            "footer must label the dominant systemic amplifier when one is present"
        );
    }

    #[test]
    fn dashboard_primary_driver_subline_names_the_coupling_mechanism() {
        // The Primary Driver cell's help text promises it names "the dominant force-domain
        // or coupling pushing the risk right now", but the cell historically showed only the
        // geography (lead theater + rung + count) — the WHY mechanism lived only in the
        // model-state footer. The sub-line must read the LIVE engine coupler so the headline
        // command strip actually delivers the systemic "why" it documents (pillar 3 — show
        // WHERE & WHY). If a refactor drops this, the cell silently breaks its own promise.
        // Sources the sub-line value from the LIVE engine coupler (never a hand-typed label).
        assert!(
            DASHBOARD_HTML.contains("_cdrv=d.couplers&&d.couplers.coupling_driver"),
            "Primary Driver sub-line no longer reads the live coupling_driver mechanism"
        );
        // Assigns that mechanism to the #cmd-driver-sub readout with the 'via <channel>' prefix.
        assert!(
            DASHBOARD_HTML.contains("cmd-driver-sub').textContent=_cdrv?('via '+_cdrv)"),
            "Primary Driver sub-line must render the coupling mechanism as 'via <channel>'"
        );
    }

    #[test]
    fn dashboard_html_has_raithe_branding() {
        assert!(DASHBOARD_HTML.contains("RAITHE INDUSTRIES INC."));
        assert!(DASHBOARD_HTML.contains("raithe-footer"));
    }

    #[test]
    fn dashboard_html_has_v2_sections() {
        // Restored cockpit, evolved to the v2 theater model: systemic index in the
        // command bar, the live theater-ladder strip, and the real timeline chart.
        assert!(DASHBOARD_HTML.contains("GLOBAL CONFLICT RISK MONITOR"));
        assert!(DASHBOARD_HTML.contains("theater-ladder"));
        assert!(DASHBOARD_HTML.contains("systemic index"));
        assert!(DASHBOARD_HTML.contains("timeline-chart"));
    }

    #[test]
    fn methodology_html_is_substantial_and_complete() {
        // Page must exist and cover every section the v2 engine implements.
        assert!(METHODOLOGY_HTML.len() > 8000, "methodology page should be a real whitepaper");
        for anchor in ["#baseline", "#modalities", "#theaters", "#persistence", "#couplers",
                       "#likelihood", "#index", "#alerts", "#calibration", "#ai",
                       "#confidence", "#nuclear"] {
            assert!(METHODOLOGY_HTML.contains(anchor), "methodology missing section {anchor}");
        }
        // The v2 model must be documented accurately — and must NOT describe the
        // removed v1 mechanics.
        assert!(METHODOLOGY_HTML.contains("systemic index"),    "missing systemic index");
        assert!(METHODOLOGY_HTML.contains("escalation ladder"), "missing escalation ladder");
        // The engineering ceiling is templated from the model's FORECAST_PROB_CEILING
        // constant (single source of truth, substituted at startup) — so the raw
        // template carries the placeholder, and the rendered value is checked by
        // methodology_renders_forecast_ceiling_from_the_model_constant.
        assert!(METHODOLOGY_HTML.contains("{{FORECAST_PROB_CEILING}}"),
            "engineering ceiling must be templated from the model constant");
        assert!(METHODOLOGY_HTML.contains("sigmoid"),  "missing logistic model");
        assert!(METHODOLOGY_HTML.contains("backtest"), "missing calibration backtest");
        assert!(!METHODOLOGY_HTML.contains("2 / 2026"), "must not describe the removed 2/2026 anchor");
    }

    #[test]
    fn methodology_advertises_the_live_iw_board_count() {
        // The methodology page states how many warning conditions the I&W board tracks
        // ("tracks <N> deterministic observable warning conditions"). That word was found
        // STALE once already ("ten" while the board had eleven), and again when a twelfth
        // light was added — a quiet legibility drift between the engine and what the
        // operator is told. Tie the advertised count to the live board length so the two
        // can never silently disagree again.
        let n = crate::indicators::evaluate(&crate::models::RiskSnapshot::default()).len();
        let word = match n {
            9 => "nine", 10 => "ten", 11 => "eleven", 12 => "twelve",
            13 => "thirteen", 14 => "fourteen", 15 => "fifteen",
            _ => panic!("add the number-word for an I&W board of {n} lights"),
        };
        assert!(
            METHODOLOGY_HTML.contains(&format!("{word} deterministic observable warning conditions")),
            "methodology must advertise the live board count ({n} → \"{word}\")"
        );
    }

    #[test]
    fn methodology_base_path_substituted() {
        let (state, _) = ServerState::new(crate::aggregator::AppState::new(), "/risk");
        assert!(state.methodology_html.contains("/risk/"),
            "base path must be substituted into methodology links");
        assert!(!state.methodology_html.contains("{{BASE_PATH}}"),
            "no unrendered template tokens may remain");
    }

    #[test]
    fn methodology_renders_live_calibration_evidence() {
        // 1.1b: the methodology page must surface the model's live calibration fidelity,
        // computed at startup — not a hand-written table that goes stale. Guards both that
        // the placeholder is substituted and that the readout (Brier + in-band) is present.
        let (state, _) = ServerState::new(crate::aggregator::AppState::new(), "/risk");
        assert!(!state.methodology_html.contains("{{CALIBRATION_EVIDENCE}}"),
            "the calibration-evidence placeholder must be substituted at startup");
        assert!(state.methodology_html.contains("Brier"),
            "methodology must show the live calibration fidelity (Brier/RMSE)");
        assert!(state.methodology_html.contains("within band"),
            "methodology must show the in-band count");
    }

    #[test]
    fn methodology_renders_alert_bands_from_alert_settings() {
        // 2.3: the alert-band prose is rendered from the engine's AlertSettings —
        // the same source the dashboard hero/timeline read live — so the methodology
        // can never disagree with the running classification (anti-drift). Guards that
        // every alert placeholder is substituted and that the rendered values match the
        // settings the engine actually uses.
        let (state, _) = ServerState::new(crate::aggregator::AppState::new(), "/risk");
        let m = &*state.methodology_html;
        for tok in ["{{ALERT_ELEVATED}}", "{{ALERT_CRITICAL}}", "{{ALERT_30D}}"] {
            assert!(!m.contains(tok), "alert placeholder {tok} must be substituted at startup");
        }
        let a = crate::models::AlertSettings::default();
        assert!(m.contains(&format!("{:.1}%", a.elevated * 100.0)),
            "methodology must render the elevated band from AlertSettings");
        assert!(m.contains(&format!("{:.1}%", a.critical * 100.0)),
            "methodology must render the critical band from AlertSettings");
        assert!(m.contains(&format!("{:.1}%", a.thirty_day_warn * 100.0)),
            "methodology must render the 30-day warning band from AlertSettings");
        // The raw template must carry placeholders, not hand-typed numbers, so the
        // page cannot drift from the engine.
        assert!(METHODOLOGY_HTML.contains("{{ALERT_CRITICAL}}"),
            "alert bands must be templated, not hardcoded");
    }

    #[test]
    fn methodology_renders_guardrail_collapse_from_the_model_constants() {
        // 2.3 (regime internals): the methodology now quantifies HOW the operator-tunable
        // regime factors enter the model — the guardrail-collapse mechanism. Its two
        // figures (the +12% max likelihood lift and the 5.0× regime-product saturation
        // point) are rendered from the engine's own GUARDRAIL_AMPLIFIER / GUARDRAIL_REGIME_SPAN
        // (single source of truth), so the whitepaper can never disagree with
        // bayesian::guardrail_from_regime. Anti-drift, same pattern as the alert bands.
        let (state, _) = ServerState::new(crate::aggregator::AppState::new(), "/risk");
        let m = &*state.methodology_html;
        for tok in ["{{GUARDRAIL_AMPLIFIER_PCT}}", "{{GUARDRAIL_SATURATION_X}}"] {
            assert!(!m.contains(tok), "guardrail placeholder {tok} must be substituted at startup");
        }
        let amp = format!("+{:.0}%", crate::bayesian::GUARDRAIL_AMPLIFIER * 100.0);
        assert!(m.contains(&amp),
            "methodology must render the guardrail amplifier ({amp}) from GUARDRAIL_AMPLIFIER");
        let sat = format!("{:.1}×", 1.0 + crate::bayesian::GUARDRAIL_REGIME_SPAN);
        assert!(m.contains(&sat),
            "methodology must render the regime-product saturation point ({sat}) from GUARDRAIL_REGIME_SPAN");
        // The mechanism's honesty point must be stated: it touches only the likelihood,
        // never the flat prior (a regime can't manufacture risk from a quiet world).
        assert!(m.contains("baseline prior"),
            "methodology must state guardrail collapse leaves a quiet world at the baseline prior");
        // Raw template carries placeholders, not hand-typed numbers — a revert to a
        // hardcoded value fails this.
        assert!(METHODOLOGY_HTML.contains("{{GUARDRAIL_AMPLIFIER_PCT}}"),
            "guardrail internals must be templated, not hardcoded");
    }

    #[test]
    fn methodology_renders_the_persistence_floor_from_the_model_constants() {
        // 2.3 (model evolution): the persistence floor (theater.rs, 2026-06-21) holds an
        // active war's heat through a multi-day news gap and is surfaced to the operator as
        // the "⏸ held by persistence" caveat — but the whitepaper never documented it, so an
        // operator had nowhere to learn what a held read means. The new #persistence section
        // explains it, and its two figures (the hold fraction and the half-life stretch) are
        // rendered from theater.rs's own FLOOR_FRACTION / WAR_STATE_HALF_LIFE_SCALE (single
        // source of truth), so the prose can never disagree with the running model.
        let (state, _) = ServerState::new(crate::aggregator::AppState::new(), "/risk");
        let m = &*state.methodology_html;
        for tok in ["{{FLOOR_FRACTION_PCT}}", "{{WAR_STATE_HALF_LIFE_SCALE}}"] {
            assert!(!m.contains(tok), "persistence placeholder {tok} must be substituted at startup");
        }
        let frac = format!("{:.0}%", crate::theater::FLOOR_FRACTION * 100.0);
        assert!(m.contains(&frac),
            "methodology must render the floor hold fraction ({frac}) from FLOOR_FRACTION");
        let scale = format!("{:.0}×", crate::theater::WAR_STATE_HALF_LIFE_SCALE);
        assert!(m.contains(&scale),
            "methodology must render the half-life stretch ({scale}) from WAR_STATE_HALF_LIFE_SCALE");
        // The honesty point must be stated: the floor never moves a live (peak-freshness)
        // reading, and it surfaces the held caveat the dashboard shows.
        assert!(m.contains("held by persistence"),
            "methodology must name the held-by-persistence caveat the operator sees");
        assert!(m.contains("calibration bands"),
            "methodology must state the floor leaves the (full-freshness) calibration bands untouched");
        // Raw template carries placeholders, not hand-typed numbers — a revert to a
        // hardcoded value fails this.
        assert!(METHODOLOGY_HTML.contains("{{FLOOR_FRACTION_PCT}}"),
            "the persistence-floor figures must be templated, not hardcoded");
    }

    #[test]
    fn methodology_renders_coupler_magnitudes_from_the_model_constants() {
        // 2.3 (systemic couplers): the #couplers section now quantifies the maximum lift
        // each coupler adds to L_sys — rendered from theater.rs's own constants (single
        // source of truth), so the whitepaper can never disagree with the running model.
        // Anti-drift, same pattern as the guardrail figures.
        let (state, _) = ServerState::new(crate::aggregator::AppState::new(), "/risk");
        let m = &*state.methodology_html;
        for tok in ["{{COUPLING_GP_PCT}}", "{{COUPLING_ALLIANCE_PCT}}", "{{GP_ENTANGLEMENT_SAT}}",
                    "{{BREADTH_ASYMPTOTE_PCT}}", "{{BRINK_AMPLIFIER_PCT}}"] {
            assert!(!m.contains(tok), "coupler placeholder {tok} must be substituted at startup");
        }
        let gp = format!("+{:.0}%", crate::theater::COUPLING_GP_WEIGHT * 100.0);
        assert!(m.contains(&gp),
            "methodology must render the great-power coupler lift ({gp}) from COUPLING_GP_WEIGHT");
        let alliance = format!("+{:.0}%", crate::theater::COUPLING_ALLIANCE_WEIGHT * 100.0);
        assert!(m.contains(&alliance),
            "methodology must render the alliance coupler lift ({alliance}) from COUPLING_ALLIANCE_WEIGHT");
        let breadth = format!("+{:.0}%", crate::theater::BREADTH_ASYMPTOTE * 100.0);
        assert!(m.contains(&breadth),
            "methodology must render the concurrency ceiling ({breadth}) from BREADTH_ASYMPTOTE");
        let brink = format!("+{:.0}%", crate::theater::BRINK_AMPLIFIER * 100.0);
        assert!(m.contains(&brink),
            "methodology must render the nuclear-brink lift ({brink}) from BRINK_AMPLIFIER");
        assert!(m.contains(&format!("{:.0}", crate::theater::GP_ENTANGLEMENT_SATURATION)),
            "methodology must render the great-power entanglement saturation count");
        // The honesty relationship the model locks (breadth_never_swamps_the_nuclear_brink)
        // must be visible on the operator-facing page: the rendered brink lift is strictly
        // greater than the rendered concurrency ceiling.
        const { assert!(crate::theater::BRINK_AMPLIFIER > crate::theater::BREADTH_ASYMPTOTE,
            "design invariant: the brink amplifier must exceed the concurrency asymptote") };
        // Pillar-1 honesty: that invariant is MULTIPLIER-level (brink amplifier > breadth
        // ceiling), NOT an absolute headline guarantee. The model's own live behaviour
        // contradicts the absolute reading — a no-brink multi-theater peg with great powers
        // entangled, alliances invoked and guardrails collapsed reads ABOVE the single-theater
        // Cuba brink apex, because the systemic couplers compound multiplicatively (see
        // backtest::pegged_resolution_readout ≈83.6% vs cuba_1962 ≈79.8%). So the page must
        // state the QUALIFIED relationship (equal coupling) and must NOT make the over-broad
        // "breadth can never swamp a brink" claim that would falsely reassure on a flat read.
        assert!(!m.contains("never swamp"),
            "methodology must not make the false absolute 'breadth can never swamp a brink' claim");
        assert!(m.contains("at equal great-power"),
            "the brink-vs-breadth claim must be qualified to equal great-power coupling (multiplier-level)");
        assert!(m.contains("compound multiplicatively"),
            "the page must disclose that the systemic couplers compound — a broad, interlocked world can exceed an isolated brink");
        // Raw template carries placeholders, not hand-typed numbers — a revert to a
        // hardcoded magnitude fails this.
        assert!(METHODOLOGY_HTML.contains("{{BRINK_AMPLIFIER_PCT}}"),
            "coupler magnitudes must be templated, not hardcoded");
    }

    #[test]
    fn methodology_renders_forecast_ceiling_from_the_model_constant() {
        // The operator-facing 0.90 ceiling prose is rendered from the model's own
        // FORECAST_PROB_CEILING constant (single source of truth), so it can never
        // silently drift from the running model the way the old hand-written 0.85
        // doc comments did. Guards that the placeholder is substituted and that the
        // rendered value matches the constant.
        let (state, _) = ServerState::new(crate::aggregator::AppState::new(), "/risk");
        assert!(!state.methodology_html.contains("{{FORECAST_PROB_CEILING}}"),
            "the forecast-ceiling placeholder must be substituted at startup");
        let rendered = format!("{:.2}", crate::models::FORECAST_PROB_CEILING);
        assert!(state.methodology_html.contains(&format!("{rendered} ceiling")),
            "methodology must render the ceiling value ({rendered}) from the model constant");
    }

    #[test]
    fn methodology_renders_baseline_prior_from_the_model_constant() {
        // 2.3 (P₀): the baseline-prior section quotes the model's flat quiet-year prior.
        // It is rendered from BASELINE_ANNUAL (single source of truth), so a recalibration
        // of the prior can never leave the whitepaper quoting a stale percentage — the same
        // anti-drift guarantee as the forecast ceiling and alert bands.
        let (state, _) = ServerState::new(crate::aggregator::AppState::new(), "/risk");
        let m = &*state.methodology_html;
        assert!(!m.contains("{{BASELINE_ANNUAL_PCT}}"),
            "the baseline-prior placeholder must be substituted at startup");
        let rendered = format!("{:.1}", crate::models::BASELINE_ANNUAL * 100.0);
        assert!(m.contains(&format!("{rendered}%/yr")),
            "methodology must render the baseline ({rendered}%/yr) from BASELINE_ANNUAL");
        // The raw template must carry the placeholder, not a hand-typed number, so the
        // prose cannot drift from the running prior.
        assert!(METHODOLOGY_HTML.contains("{{BASELINE_ANNUAL_PCT}}"),
            "the baseline prior must be templated, not hardcoded");
    }

    #[test]
    fn dashboard_renders_baseline_prior_from_the_model_constant() {
        // The dashboard's model-state footer (the live Bayesian chain) and its
        // "what this means" calibration line both quote the flat quiet-year prior P₀.
        // Those numbers MUST render from BASELINE_ANNUAL, not hand-typed literals that
        // could silently drift from the engine after a recalibration — the same
        // anti-drift guarantee the methodology page carries (and the primary operator
        // surface had been missed). A revert to a hardcoded "1.5%/yr" fails this.
        assert!(
            DASHBOARD_HTML.contains("{{BASELINE_ANNUAL_PCT}}"),
            "dashboard baseline prior must be templated, not hardcoded"
        );
        // Both references must be templated (footer chain + info-modal line), so neither
        // can drift independently.
        assert_eq!(
            DASHBOARD_HTML.matches("{{BASELINE_ANNUAL_PCT}}").count(),
            2,
            "both dashboard baseline-prior references must carry the placeholder"
        );
        let rendered = generate_dashboard_html("/risk");
        assert!(
            !rendered.contains("{{BASELINE_ANNUAL_PCT}}"),
            "baseline-prior placeholder was not substituted at render time"
        );
        let pct = format!("{:.1}", crate::models::BASELINE_ANNUAL * 100.0);
        assert!(
            rendered.contains(&format!("{pct}%/yr")),
            "rendered dashboard must embed the baseline ({pct}%/yr) from BASELINE_ANNUAL"
        );
        assert!(
            rendered.contains(&format!("~{pct}%</code> modern quiet-year baseline")),
            "rendered dashboard calibration line must embed the baseline from BASELINE_ANNUAL"
        );
    }

    #[test]
    fn dashboard_renders_confidence_formula_from_the_model_constants() {
        // HONESTY (pillar 1): the Confidence info-modal explains how the operator's
        // data-quality score is built. Its blend weights and saturation points MUST
        // render from the engine's own CONF_W_*/CONFIDENCE_*_SATURATION constants — the
        // same ones estimate_confidence blends — not hand-typed numbers that could
        // silently drift from the running formula after a re-weighting. A revert to a
        // hardcoded "×0.5 … 200 events … 20 feeds" fails this.
        const PHS: [&str; 5] = ["{{CONF_W_DOMAIN}}", "{{CONF_W_EVENTS}}",
            "{{CONF_W_SOURCES}}", "{{CONFIDENCE_EVENT_SAT}}", "{{CONFIDENCE_SOURCE_SAT}}"];
        for ph in PHS {
            assert!(DASHBOARD_HTML.contains(ph),
                "confidence formula must be templated, not hardcoded ({ph})");
        }
        let rendered = generate_dashboard_html("/risk");
        for ph in PHS {
            assert!(!rendered.contains(ph),
                "confidence placeholder was not substituted at render time ({ph})");
        }
        // The rendered prose must quote the live constants, so the operator-facing
        // explanation can never disagree with what estimate_confidence actually computes.
        let blend = format!(
            "avg domain confidence ×{:.1} + event volume ×{:.1} + active sources ×{:.1}",
            crate::bayesian::CONF_W_DOMAIN, crate::bayesian::CONF_W_EVENTS,
            crate::bayesian::CONF_W_SOURCES);
        assert!(rendered.contains(&blend),
            "rendered confidence blend must embed CONF_W_* from the model");
        assert!(rendered.contains(&format!(
            "saturates near {:.0} events", crate::bayesian::CONFIDENCE_EVENT_SATURATION)),
            "rendered prose must embed CONFIDENCE_EVENT_SATURATION");
        assert!(rendered.contains(&format!(
            "near {:.0} feeds", crate::bayesian::CONFIDENCE_SOURCE_SATURATION)),
            "rendered prose must embed CONFIDENCE_SOURCE_SATURATION");
    }

    #[test]
    fn dashboard_explains_the_v2_flat_prior_not_the_v1_adjusted_prior() {
        // HONESTY (pillar 1): the operator-facing explanation of the headline must
        // describe the v2 computation — a FLAT modern baseline prior with the regime
        // multiplier entering ONLY as a bounded guardrail-collapse amplifier of the
        // systemic likelihood, combined on the log-odds scale (bayesian.rs Step 7,
        // locked by guardrail_collapse_is_live_in_compute_and_only_amplifies_the_likelihood).
        // It previously told the operator the number was "a regime-adjusted prior
        // multiplied by a coupling likelihood" — the SUPERSEDED v1 multiplicative form —
        // and drew `structural-adjusted = baseline × regime` (≈8%) as a chain step toward
        // the posterior, which the v2 engine never uses. The number must mean what the
        // dashboard says it means.
        //
        // The v1 story must be gone from BOTH the footer chain and the "how it's built" modal:
        assert!(!DASHBOARD_HTML.contains("structural-adjusted"),
            "footer must not draw the unused structural-adjusted prior as a chain step");
        assert!(!DASHBOARD_HTML.contains("regime-adjusted prior"),
            "modal must not describe the headline as a regime-adjusted prior (v1 form)");
        assert!(!DASHBOARD_HTML.contains("f-adj"),
            "the adjusted-prior readout (id=f-adj) must be removed");
        assert!(!DASHBOARD_HTML.contains("d.prior"),
            "the footer JS must no longer read the unused snapshot prior (d.prior.adjusted_prior)");
        // The v2 story must be present and honest: a flat prior, the guardrail-collapse
        // channel (the regime's only path into the forecast), and the log-odds fold.
        assert!(DASHBOARD_HTML.contains("(modern, flat)"),
            "footer must state the prior is flat (not regime-adjusted)");
        assert!(DASHBOARD_HTML.contains("log-odds"),
            "modal must describe the log-odds fold (v2 logistic form)");
        assert!(DASHBOARD_HTML.contains("guardrail collapse"),
            "the regime's only forecast channel — guardrail collapse — must be shown");
        // The footer's new guardrail readout must source the live snapshot coupler, not a
        // hardcoded number — the same anti-drift discipline as the alert bands.
        assert!(DASHBOARD_HTML.contains("f-guard")
            && DASHBOARD_HTML.contains("couplers.guardrail_collapse"),
            "the guardrail readout must read d.couplers.guardrail_collapse live");
    }

    #[test]
    fn dashboard_regime_inspector_shows_structural_pressure_not_adjusted_prior() {
        // HONESTY (roadmap 2.3): the operator regime inspector previously labeled
        // `HISTORICAL_ANCHOR × regime_product` as "Adjusted P₀ … %/yr" — the SUPERSEDED v1
        // form that implies toggling a regime factor moves the forecast PRIOR. In v2 the prior
        // is FLAT; the regime product enters only as the bounded guardrail-collapse amplifier on
        // the systemic likelihood. The panel must say what toggling a factor actually does.
        assert!(!DASHBOARD_HTML.contains("Adjusted P₀"),
            "regime inspector must not call the regime product an 'Adjusted P₀' (v1 form)");
        assert!(!DASHBOARD_HTML.contains("adjusted_prior_pct"),
            "regime inspector JS must no longer read the discredited adjusted_prior_pct field");
        // The honest v2 readout: structural pressure → guardrail collapse → bounded lift on L.
        assert!(DASHBOARD_HTML.contains("Structural pressure"),
            "regime inspector must label the regime product as structural pressure");
        assert!(DASHBOARD_HTML.contains("guardrail_collapse")
            && DASHBOARD_HTML.contains("likelihood_amplifier_pct"),
            "regime inspector must read the live guardrail-collapse coupler and its likelihood lift");
        assert!(DASHBOARD_HTML.contains("prior unaffected"),
            "the inspector must state plainly that the regime does not move the prior in v2");
    }

    #[test]
    fn dashboard_links_to_methodology() {
        assert!(DASHBOARD_HTML.contains("{{BASE_PATH}}/methodology"),
            "dashboard must link to the methodology page");
    }

    #[test]
    fn dashboard_html_has_all_domain_ids() {
        for domain in crate::models::DOMAIN_IDS {
            assert!(
                DASHBOARD_HTML.contains(domain),
                "Dashboard HTML missing domain: {domain}"
            );
        }
    }

    #[test]
    fn dashboard_html_has_live_chart() {
        assert!(DASHBOARD_HTML.contains("timeline-chart"));
        assert!(DASHBOARD_HTML.contains("new Chart"));
        assert!(DASHBOARD_HTML.contains("applyData"));
    }

    #[test]
    fn dashboard_html_renders_theaters() {
        assert!(DASHBOARD_HTML.contains("d.theaters"));
        assert!(DASHBOARD_HTML.contains("rung_label"));
        assert!(DASHBOARD_HTML.contains("couplers"));
    }

    #[test]
    fn dashboard_theater_ladder_is_wired() {
        // The v2 ladder strip must be a real, populated container — not just the
        // literal asserted by dashboard_html_has_v2_sections. It reads from the live
        // theaters array and renders one chip per flashpoint with its escalation rung.
        assert!(DASHBOARD_HTML.contains("id=\"theater-ladder\""), "ladder container missing");
        assert!(DASHBOARD_HTML.contains("tl-chip"),  "ladder chips not rendered");
        assert!(DASHBOARD_HTML.contains("RUNG_LVL"),  "rung-level map missing");
        assert!(DASHBOARD_HTML.contains("rungColor"), "rung colour helper missing");
    }

    #[test]
    fn dashboard_rung_levels_match_engine() {
        // The client RUNG_LVL map drives the ladder's per-theater colour. It MUST
        // mirror EscalationRung::level() exactly — otherwise the strip would mis-rank
        // severity. Lock every rung's snake_case id → numeric level so the two sides
        // can never silently drift.
        use crate::models::EscalationRung::*;
        for (rung, snake) in [
            (Stable, "stable"), (Tension, "tension"), (Crisis, "crisis"),
            (LimitedWar, "limited_war"), (GreatPowerWar, "great_power_war"),
            (Systemic, "systemic"),
        ] {
            let pair = format!("{}:{}", snake, rung.level());
            assert!(DASHBOARD_HTML.contains(&pair),
                "dashboard RUNG_LVL missing `{pair}` — client/engine rung levels drifted");
        }
    }

    #[test]
    fn dashboard_headline_colour_follows_the_rung_not_a_rounded_index() {
        // HONESTY/LEGIBILITY: the headline rung WORD (`cmd-threat`, `cc-rung`) must be
        // coloured by the hottest theater's actual rung — the same rungColor()/RUNG_LVL
        // the theater chips use — so the word and its colour can never contradict each
        // other. The old `idxCol` re-derived the band from the ROUNDED systemic index
        // with integer cuts (`sysIdx>=34`/`sysIdx>=67`); because the Crisis-rung floor is
        // index 33.33 (rounds to 33, below the >=34 amber cut), a real Crisis read showed
        // the moderate-indigo colour while the word said "Crisis" and its chip showed
        // amber. Lock the rung-derived derivation and forbid a regression to the
        // rounded-index integer thresholds.
        assert!(DASHBOARD_HTML.contains("const idxCol=rungColor(RUNG_LVL[_top.rung]"),
            "headline colour must be derived from the hottest theater's rung via rungColor()/RUNG_LVL");
        assert!(!DASHBOARD_HTML.contains("sysIdx>=67"),
            "headline colour must not re-derive the band from the rounded systemic index (>=67 cut)");
        assert!(!DASHBOARD_HTML.contains("sysIdx>=34"),
            "headline colour must not re-derive the band from the rounded systemic index (>=34 cut)");
    }

    #[test]
    fn dashboard_renders_iw_board() {
        // The I&W board (indicators::evaluate) is computed and served at
        // data.indicators, and the methodology page advertises it ("an I&W board
        // tracks twelve deterministic observable warning conditions"). It must
        // actually be rendered, so the operator can see WHICH danger conditions
        // have tripped — the "why" behind the headline, not just how high.
        assert!(DASHBOARD_HTML.contains("id=\"iw-board\""), "I&W board container missing");
        assert!(DASHBOARD_HTML.contains("renderIndicators"), "I&W render fn missing");
        assert!(DASHBOARD_HTML.contains("d.indicators"), "dashboard must read the live indicators array");
        assert!(DASHBOARD_HTML.contains("Indications &amp; Warning"), "I&W board title missing");
        // The red (apex) lights must be driven by the engine's per-indicator `apex`
        // flag carried in the data — NOT a hard-coded client-side set that can silently
        // drift when indicators.rs adds/renames an apex condition. Lock that the
        // dashboard reads `i.apex` and no longer hard-codes the apex ids.
        assert!(DASHBOARD_HTML.contains("i.tripped&&i.apex"),
            "the apex (red) light must be driven by the engine's `i.apex` flag");
        assert!(!DASHBOARD_HTML.contains("IW_APEX"),
            "the client-side hard-coded apex set must be gone — apex is engine-driven now");
        // The at-a-glance board summary must surface an apex trip (so an operator sees a
        // great-power-war condition is live without scanning every dot).
        assert!(DASHBOARD_HTML.contains("APEX") && DASHBOARD_HTML.contains("apexTrip"),
            "the I&W summary must flag apex trips distinctly");
        // Cross-check the engine actually emits a serialized `apex` field, so the
        // dashboard's `i.apex` read is backed by real data (engine = single source).
        let inds = crate::indicators::evaluate(&crate::models::RiskSnapshot::default());
        let v = serde_json::to_value(&inds).unwrap();
        assert!(v.as_array().map(|a| a.iter().all(|x| x.get("apex").is_some())).unwrap_or(false),
            "every serialized indicator must carry an `apex` field for the dashboard to read");
    }

    #[test]
    fn dashboard_html_has_operator_panel() {
        assert!(DASHBOARD_HTML.contains("Operator Panel"));
        assert!(DASHBOARD_HTML.contains("op-drawer"));
        assert!(DASHBOARD_HTML.contains("/api/regime/"));
        assert!(DASHBOARD_HTML.contains("/api/operator/assert"));
        assert!(DASHBOARD_HTML.contains("X-GCRM-Key"));
    }

    #[test]
    fn broadcast_cap_is_reasonable() {
        const { assert!(BROADCAST_CAP >= 16 && BROADCAST_CAP <= 256) };
    }

    #[test]
    fn server_state_creates_broadcast_channel() {
        // Verify ServerState::new returns a functional broadcast sender
        let app_state = crate::aggregator::AppState::new();
        let (state, tx) = ServerState::new(app_state, "");
        // Subscribe and verify we can send/receive
        let mut rx = state.broadcast_tx.subscribe();
        let msg = Arc::new("test".to_string());
        tx.send(msg.clone()).unwrap();
        // The receiver should have the message buffered
        assert_eq!(*rx.try_recv().unwrap(), "test");
    }

    #[test]
    fn route_count() {
        let app_state      = crate::aggregator::AppState::new();
        let (state, _)     = ServerState::new(Arc::clone(&app_state), "");
        let op_state       = crate::api::OperatorState::new(
            app_state,
            "test_key".into(),
            vec![],
        );
        let _router = build_router(state, op_state, "");
    }

    // Regression: build_router with a non-empty base path nests the inner router
    // AND registers a trailing-slash redirect ("/risk/" → "/risk"). axum builds
    // its route table eagerly, so a route conflict between the nest and the
    // redirect would panic here at construction — the prod path uses a base path.
    #[test]
    fn route_build_with_base_path_does_not_panic() {
        let app_state  = crate::aggregator::AppState::new();
        let (state, _) = ServerState::new(Arc::clone(&app_state), "/risk");
        let op_state   = crate::api::OperatorState::new(app_state, "test_key".into(), vec![]);
        let _router = build_router(state, op_state, "/risk");
    }
}
