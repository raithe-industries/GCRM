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
        let methodology = Arc::new(render_base_path(METHODOLOGY_HTML, base_path));
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

    // Send full timeline history from EpochStore (in-memory, no disk read)
    let timeline: Vec<serde_json::Value> = {
        let es = state.app_state.epoch_store.lock().await;
        es.query(usize::MAX).into_iter().cloned().collect()
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

#[derive(Deserialize)]
struct TimelineParams {
    limit: Option<usize>,
}

async fn get_timeline(
    State(state): State<ServerState>,
    Query(params): Query<TimelineParams>,
) -> impl IntoResponse {
    let limit = params.limit.unwrap_or(usize::MAX);
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
                e.get("ts")
                    .and_then(|t| t.as_str())
                    .map_or(true, |ts| ts >= since.as_str())
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
    Json(json!({
        "active_sources":     counts,
        "configured_sources": configured,
        "total_configured":   RSS_FEEDS.len(),
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
    fn dashboard_html_uses_live_endpoints() {
        // The cockpit is WebSocket-driven with the live article feed + sources + epoch.
        assert!(DASHBOARD_HTML.contains("/ws"));
        assert!(DASHBOARD_HTML.contains("/api/articles"));
        assert!(DASHBOARD_HTML.contains("/api/sources"));
        assert!(DASHBOARD_HTML.contains("/api/epoch"));
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
        for anchor in ["#baseline", "#modalities", "#theaters", "#couplers",
                       "#likelihood", "#index", "#calibration", "#ai", "#confidence", "#nuclear"] {
            assert!(METHODOLOGY_HTML.contains(anchor), "methodology missing section {anchor}");
        }
        // The v2 model must be documented accurately — and must NOT describe the
        // removed v1 mechanics.
        assert!(METHODOLOGY_HTML.contains("systemic index"),    "missing systemic index");
        assert!(METHODOLOGY_HTML.contains("escalation ladder"), "missing escalation ladder");
        assert!(METHODOLOGY_HTML.contains("0.90"),  "missing v2 engineering ceiling");
        assert!(METHODOLOGY_HTML.contains("sigmoid"),  "missing logistic model");
        assert!(METHODOLOGY_HTML.contains("backtest"), "missing calibration backtest");
        assert!(!METHODOLOGY_HTML.contains("2 / 2026"), "must not describe the removed 2/2026 anchor");
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
    fn dashboard_html_has_operator_panel() {
        assert!(DASHBOARD_HTML.contains("Operator Panel"));
        assert!(DASHBOARD_HTML.contains("op-drawer"));
        assert!(DASHBOARD_HTML.contains("/api/regime/"));
        assert!(DASHBOARD_HTML.contains("/api/operator/assert"));
        assert!(DASHBOARD_HTML.contains("X-GCRM-Key"));
    }

    #[test]
    fn broadcast_cap_is_reasonable() {
        assert!(BROADCAST_CAP >= 16);
        assert!(BROADCAST_CAP <= 256);
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
