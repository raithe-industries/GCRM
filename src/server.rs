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

// RAiTHE "A" mark — shared brand favicon + the rotating branding glyph. Compiled
// into the binary so the dashboard stays a single self-contained artifact.
static LOGO_A: &[u8] = include_bytes!("../assets/logo-a.png");

async fn get_logo() -> impl IntoResponse {
    (
        [
            (axum::http::header::CONTENT_TYPE,  "image/png"),
            (axum::http::header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        LOGO_A,
    )
}

// ── Router ────────────────────────────────────────────────────────────────────

pub fn build_router(state: ServerState, operator_state: crate::api::OperatorState, base_path: &str) -> Router {
    let inner = Router::new()
        .route("/",              get(get_dashboard))
        .route("/methodology",   get(get_methodology))
        .route("/logo-a.png",    get(get_logo))
        .route("/favicon.ico",   get(get_logo))
        .route("/ws",            get(ws_handler))
        .route("/api/latest",    get(get_latest))
        .route("/api/timeline",  get(get_timeline))
        .route("/api/epoch",     get(get_epoch))
        .route("/api/articles",  get(get_articles))
        .route("/api/sources",   get(get_sources))
        .route("/api/nuclear",   get(get_nuclear))
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

const DASHBOARD_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width,initial-scale=1.0">
<title>Global Conflict Risk Monitor</title>
<link rel="icon" type="image/png" href="{{BASE_PATH}}/logo-a.png">
<link rel="apple-touch-icon" href="{{BASE_PATH}}/logo-a.png">
<script src="https://cdn.jsdelivr.net/npm/chart.js@4.4.1/dist/chart.umd.min.js"></script>
<script src="https://cdn.jsdelivr.net/npm/chartjs-plugin-annotation@3.0.1/dist/chartjs-plugin-annotation.min.js"></script>
<style>
*{box-sizing:border-box;margin:0;padding:0}
:root{
  --bg:#000000;--bg2:#080c10;--bg3:#0d1117;--border:#1a2030;
  --t1:#ffffff;--t2:#b8c8d8;--t3:#7090a8;--t4:#405060;
  --accent:#1a6b8a;--accent2:#1e8aaa;--accent-glow:#1e8aaa33;
  --green:#1D9E75;--amber:#d4962a;--red:#c0392b;--purple:#7F77DD;
  --mil:#c0392b;--nuc:#d4662a;--dip:#5a7abf;--eco:#1D9E75;
  --cyb:#1a9e9e;--ali:#d4962a;--gp:#9b6fbf;--wmd:#c0392b;
}
body{font-family:system-ui,sans-serif;background:var(--bg);color:var(--t2);height:100vh;display:flex;flex-direction:column;overflow:hidden}
.cmd-strip{display:grid;grid-template-columns:repeat(5,1fr);background:var(--bg2);border-bottom:0.5px solid var(--border);flex-shrink:0}
.cmd-cell{padding:7px 14px;border-right:0.5px solid var(--border)}
.cmd-cell:last-child{border-right:none}
.cmd-label{font-size:8px;color:var(--t4);letter-spacing:.06em;text-transform:uppercase;margin-bottom:2px}
.cmd-val{font-size:13px;font-weight:600;font-family:monospace;color:var(--t1);transition:color .4s}
.cmd-sub{font-size:9px;color:var(--t3);margin-top:1px}
.nuke-overlay{display:none;position:fixed;top:0;left:0;right:0;bottom:0;z-index:1000;background:rgba(200,0,0,0.15);pointer-events:none;animation:nuke-pulse 1s ease-in-out infinite}
.nuke-banner{display:none;position:fixed;top:0;left:0;right:0;z-index:1001;background:#600000;border-bottom:3px solid #ff0000;padding:12px 20px;font-size:14px;font-weight:700;color:#ff4444;letter-spacing:.05em;pointer-events:all;cursor:pointer;animation:nuke-flash 0.5s ease-in-out infinite alternate}
@keyframes nuke-pulse{0%,100%{opacity:0.3}50%{opacity:0.8}}
@keyframes nuke-flash{0%{background:#600000}100%{background:#900000}}
.alert-bar{padding:6px 16px;font-size:11px;color:var(--t1);display:none;flex-shrink:0;font-weight:500}
.alert-bar.elevated{display:block;background:#1a1400;border-bottom:0.5px solid #5a4000}
.alert-bar.critical{display:block;background:#1a0000;border-bottom:0.5px solid #700;animation:pulse-bar 2s infinite}
@keyframes pulse-bar{0%,100%{opacity:1}50%{opacity:.75}}
.ticker{height:24px;background:var(--bg2);border-bottom:0.5px solid var(--border);overflow:hidden;display:flex;align-items:center;flex-shrink:0}
.ticker-inner{display:flex;gap:40px;animation:scroll 80s linear infinite;white-space:nowrap;padding:0 20px}
.ticker-inner:hover{animation-play-state:paused}
@keyframes scroll{0%{transform:translateX(0)}100%{transform:translateX(-50%)}}
.tick-item{font-size:10px;color:var(--t3);display:flex;align-items:center;gap:6px;cursor:pointer}
.tick-dot{width:5px;height:5px;border-radius:50%;flex-shrink:0}
.topbar{display:flex;align-items:center;justify-content:space-between;padding:6px 16px;border-bottom:0.5px solid var(--border);flex-shrink:0;background:var(--bg2)}
.topbar-left{display:flex;align-items:center;gap:16px}
.logo{font-size:15px;font-weight:700;color:var(--t1);letter-spacing:.12em;text-transform:uppercase}
.topbar-live{display:flex;align-items:center;gap:6px;margin-left:14px;margin-top:1px}
.topbar-live-dot{width:6px;height:6px;border-radius:50%;background:#404040;flex-shrink:0;transition:background .4s}
.topbar-live-dot.connected{background:#c0392b;animation:pulse-dot 2s ease-in-out infinite}
@keyframes pulse-dot{0%,100%{opacity:1;transform:scale(1)}50%{opacity:.6;transform:scale(.75)}}
.sub{font-size:10px;color:var(--t4);font-family:monospace}
.topbar-right{display:flex;align-items:center;gap:10px;font-size:10px;color:var(--t4);font-family:monospace}
.main{display:grid;grid-template-columns:176px 1fr 310px;flex:1;overflow:hidden}
.left-panel{border-right:0.5px solid var(--border);display:flex;flex-direction:column;overflow:hidden;background:var(--bg2)}
.panel-title{font-size:9px;font-weight:600;color:var(--t1);padding:7px 12px;border-bottom:0.5px solid var(--border);letter-spacing:.06em;flex-shrink:0}
.gauge-wrap{padding:10px;display:flex;flex-direction:column;align-items:center;border-bottom:0.5px solid var(--border);flex-shrink:0}
#gauge-canvas{width:148px;height:82px}
.gauge-val{font-size:22px;font-weight:700;font-family:monospace;color:var(--t1);margin-top:2px;text-align:center;transition:color .4s}
.gauge-pct-label{font-size:9px;color:var(--t3);text-align:center;margin-top:1px}
.gauge-context{margin-top:5px;width:100%;font-size:8px;color:var(--t4);line-height:1.8}
.gauge-context-row{display:flex;justify-content:space-between}
.conf-row{display:flex;align-items:center;gap:5px;margin-top:6px;font-size:9px;color:var(--t2);width:100%}
.conf-bar{flex:1;height:3px;background:var(--border);border-radius:2px;overflow:hidden}
.conf-fill{height:100%;border-radius:2px;background:var(--purple);transition:width .6s}
.left-metrics{display:flex;flex-direction:column;flex-shrink:0}
.lm{padding:7px 12px;border-bottom:0.5px solid var(--border)}
.lm-label{font-size:8px;color:var(--t4);margin-bottom:2px;letter-spacing:.04em}
.lm-val{font-size:13px;font-weight:500;font-family:monospace;color:var(--t1);transition:color .4s}
.lm-sub{font-size:9px;color:var(--t3);margin-top:1px}
.center-panel{display:flex;flex-direction:column;overflow:hidden}
.domains{display:grid;grid-template-columns:repeat(4,1fr);gap:1px;background:var(--border);flex-shrink:0;border-bottom:0.5px solid var(--border)}
.domain{background:var(--bg);padding:7px 9px;cursor:pointer;transition:background .2s}
.domain:hover{background:var(--bg3)}
.domain.active{background:#0d0d20;box-shadow:inset 0 0 0 1px var(--purple)}
.dn{font-size:8px;color:var(--t3);margin-bottom:3px;letter-spacing:.04em}
.dbar{height:2px;background:var(--bg3);border-radius:1px;overflow:hidden;margin-bottom:3px}
.dfill{height:100%;border-radius:1px;transition:width .5s ease}
.drow{display:flex;justify-content:space-between;align-items:center}
.dscore{font-size:13px;font-weight:500;font-family:monospace;color:var(--t1);transition:color .4s}
.dlabel{font-size:7px;padding:1px 3px;border-radius:2px}
.dlabel.critical{background:#2a0000;color:#ff7070}
.dlabel.elevated{background:#1f1000;color:#ffaa40}
.dlabel.moderate{background:#0f0f25;color:#9090ff}
.dlabel.low{background:#0a1a10;color:#40c070}
.ddelta{font-size:9px;margin-left:4px}
.dconf{font-size:7px;color:var(--t4);margin-top:2px}
.scen-row{display:flex;align-items:center;gap:5px;padding:5px 10px;border-bottom:0.5px solid var(--border);flex-shrink:0;flex-wrap:wrap}
.scen-label{font-size:8px;color:var(--t4)}
.sbtn{font-size:9px;padding:2px 7px;border-radius:3px;background:transparent;border:0.5px solid var(--border);color:var(--t3);cursor:pointer}
.sbtn:hover{border-color:var(--t4);color:var(--t1)}
.sbtn.active{border-color:var(--purple);color:#c0bcff;background:#0d0d20}
.charts-area{flex:1;display:grid;grid-template-rows:3fr 2fr;overflow:hidden}
.chart-card{padding:8px 10px;border-bottom:0.5px solid var(--border);display:flex;flex-direction:column;overflow:hidden;min-height:0}
.ct{font-size:9px;color:var(--t1);margin-bottom:4px;flex-shrink:0;font-weight:600;letter-spacing:.04em;display:flex;justify-content:space-between;align-items:center}
.chart-inner{flex:1;position:relative;min-height:0}
.context-strip{padding:4px 10px;border-bottom:0.5px solid var(--border);font-size:9px;flex-shrink:0;display:flex;gap:16px;align-items:center;background:var(--bg2)}
.ca{color:var(--t4)}
.ca span{color:var(--t2);font-family:monospace}
.formula{padding:6px 10px;font-family:monospace;font-size:9px;color:var(--t4);line-height:1.7;flex-shrink:0;background:var(--bg)}
.formula span{color:var(--t2)}
.meta-pills{display:none}
.mpill{font-size:8px;padding:1px 5px;border-radius:8px;border:0.5px solid var(--border);color:var(--t4)}
.mpill.hi{border-color:var(--purple);color:#a0a0ff}
.right-panel{border-left:0.5px solid var(--border);display:flex;flex-direction:column;overflow:hidden;background:var(--bg2)}
.tabs{display:flex;border-bottom:0.5px solid var(--border);flex-shrink:0}
.tab{flex:1;padding:6px;font-size:9px;color:var(--t4);cursor:pointer;text-align:center;border-bottom:2px solid transparent;transition:color .2s}
.tab:hover{color:var(--t2)}
.tab.active{color:var(--t1);border-bottom-color:var(--purple)}
.panel-body{flex:1;overflow-y:auto;font-size:11px}
.art-item{padding:7px 10px;border-bottom:0.5px solid var(--border);cursor:pointer;transition:background .15s}
.art-item:hover{background:var(--bg3)}
.art-title{font-size:10px;color:var(--t1);line-height:1.35;margin-bottom:2px}
.art-meta{font-size:8px;color:var(--t4);display:flex;gap:6px;flex-wrap:wrap;align-items:center}
.art-tags{display:flex;gap:3px;flex-wrap:wrap;margin-top:2px}
.art-tag{font-size:7px;padding:1px 4px;border-radius:2px;font-weight:600;letter-spacing:.04em}
.art-tier1{border-left:2px solid var(--green)}
.art-tier2{border-left:2px solid var(--t4)}
.art-tier3{border-left:2px solid var(--amber)}
.art-mover{border-left:2px solid var(--red) !important;background:rgba(226,75,74,0.04)}
.tf-btn{font-size:8px;padding:1px 5px;border-radius:3px;background:transparent;border:0.5px solid var(--border);color:var(--t4);cursor:pointer;white-space:nowrap}
.tf-btn:hover{border-color:var(--t4);color:var(--t2)}
.tf-btn.active{border-color:var(--purple);color:#c0bcff;background:#0d0d20}
.src-name{font-size:10px;color:var(--t1)}
.src-count{font-size:9px;color:var(--t4);font-family:monospace}
.src-tier{font-size:7px;padding:1px 4px;border-radius:2px}
.src-tier.t1{background:#0a1a0a;color:var(--green)}
.src-tier.t2{background:#101018;color:var(--t4)}
#log-body{font-family:monospace;font-size:9px;color:var(--t4);padding:6px 10px;line-height:1.6}
.op-toggle-btn{background:none;border:1px solid var(--border);color:var(--t3);padding:3px 8px;border-radius:3px;cursor:pointer;font-size:13px;margin-left:12px;transition:all .2s}.op-toggle-btn:hover{border-color:var(--t2);color:var(--t1)}
.op-drawer{position:fixed;top:0;right:-420px;width:420px;height:100vh;background:var(--bg2);border-left:1px solid var(--border);z-index:2000;display:flex;flex-direction:column;transition:right .25s cubic-bezier(.4,0,.2,1);overflow:hidden}
.op-drawer.open{right:0}
.op-drawer-header{display:flex;align-items:center;justify-content:space-between;padding:14px 16px;border-bottom:1px solid var(--border);flex-shrink:0}
.op-drawer-title{font-size:11px;font-weight:700;letter-spacing:.1em;color:var(--t1);text-transform:uppercase}
.op-close{background:none;border:none;color:var(--t3);font-size:18px;cursor:pointer;padding:0 4px;line-height:1}.op-close:hover{color:var(--t1)}
.op-section{padding:12px 16px;border-bottom:1px solid var(--border)}
.op-section-title{font-size:9px;font-weight:700;letter-spacing:.1em;color:var(--t4);text-transform:uppercase;margin-bottom:10px}
.op-body{overflow-y:auto;flex:1;padding-bottom:16px}
.regime-factor{display:flex;align-items:center;justify-content:space-between;padding:5px 0;border-bottom:1px solid #1a1a30}
.regime-factor:last-child{border-bottom:none}
.rf-label{font-size:10px;color:var(--t2);flex:1;padding-right:8px;line-height:1.3}
.rf-mult{font-size:9px;color:var(--t4);font-family:monospace;width:36px;text-align:right;flex-shrink:0}
.rf-toggle{width:34px;height:18px;border-radius:9px;border:none;cursor:pointer;font-size:8px;font-weight:700;flex-shrink:0;margin-left:8px;transition:all .2s}
.rf-toggle.on{background:#1D9E75;color:#fff}.rf-toggle.off{background:#2a2a40;color:var(--t4)}
.op-product{font-size:11px;font-family:monospace;color:var(--amber);padding:8px 0 2px}
.assert-form{display:flex;flex-direction:column;gap:8px}
.assert-input{background:var(--bg3);border:1px solid var(--border);color:var(--t1);padding:7px 10px;border-radius:3px;font-size:11px;width:100%;font-family:system-ui}
.assert-input:focus{outline:none;border-color:var(--t3)}
.assert-btn{background:#1e1e38;border:1px solid var(--border);color:var(--t2);padding:7px 12px;border-radius:3px;cursor:pointer;font-size:10px;font-weight:600;letter-spacing:.05em;transition:all .2s}
.assert-btn:hover{background:#2e2e58;border-color:var(--t2);color:var(--t1)}
.assert-btn.primary{background:#1a2a1a;border-color:#1D9E75;color:#1D9E75}.assert-btn.primary:hover{background:#1D9E75;color:#fff}
.op-log-entry{font-size:9px;font-family:monospace;color:var(--t4);padding:3px 0;border-bottom:1px solid #111120;line-height:1.4}
.op-log-entry:last-child{border-bottom:none}
.op-key-input{background:var(--bg3);border:1px solid var(--border);color:var(--t1);padding:5px 8px;border-radius:3px;font-size:10px;font-family:monospace;width:100%}
.seismic-alert{background:#1a0808;border:1px solid #600;border-radius:3px;padding:8px 10px;margin:4px 0;font-size:10px}
.seismic-alert .sa-level{color:#E24B4A;font-weight:700;font-size:9px;letter-spacing:.05em}
.seismic-alert .sa-desc{color:var(--t2);margin-top:3px;line-height:1.4}
.seismic-alert .sa-conf{color:var(--t4);font-size:9px;margin-top:3px;font-family:monospace}
.op-overlay{display:none;position:fixed;inset:0;background:rgba(0,0,0,.4);z-index:1999}.op-overlay.open{display:block}
.raithe-footer{background:#000;border-top:1px solid var(--border);padding:0 20px;display:grid;grid-template-columns:1fr auto 1fr;align-items:center;flex-shrink:0;height:30px}
.raithe-footer-left{display:flex;align-items:center;gap:8px}
.raithe-footer-name{font-size:9px;font-weight:700;letter-spacing:.2em;color:var(--t3);text-transform:uppercase}
.raithe-footer-copy{font-size:9px;color:var(--t4);opacity:.6}
.raithe-footer-center{font-size:9px;color:var(--t4);opacity:.5;letter-spacing:.06em;font-family:monospace;text-align:center}
.raithe-footer-right{display:flex;align-items:center;justify-content:flex-end;gap:16px}
::-webkit-scrollbar{width:3px;height:3px}
::-webkit-scrollbar-track{background:transparent}
::-webkit-scrollbar-thumb{background:var(--border);border-radius:2px}
@keyframes cal-pulse{0%,100%{opacity:1}50%{opacity:.45}}
.cal-fresh{animation:cal-pulse 2s ease-in-out infinite}
.plain-eng{border-top:1px solid var(--border);padding:10px 12px;background:var(--bg);flex:1;min-height:0;overflow-y:auto}
/* RAiTHE "A" brand mark — its own pinned strip at the foot of the left rail
   (flex-shrink:0, never scrolled away), slowly rotating on its vertical axis
   in 3D so it reads as a turning badge rather than a flat spin. */
.left-foot{flex-shrink:0;background:var(--bg);padding:6px 12px 8px}
.brand-a{display:flex;justify-content:center;align-items:center;padding:12px 0 2px;perspective:360px;line-height:0;cursor:pointer}
/* Two-faced "coin": front + a back face pre-rotated 180°, both backface-hidden,
   so whichever side faces the viewer always shows the A upright — the spin reads
   as a turning solid badge, never a mirrored or blank flip. */
.brand-a-coin{position:relative;width:40px;height:40px;transform-style:preserve-3d;animation:brand-a-spin 7s linear infinite}
.brand-a-face{position:absolute;top:0;left:0;width:40px;height:40px;opacity:.45;backface-visibility:hidden;-webkit-backface-visibility:hidden;transition:opacity .3s}
.brand-a-back{transform:rotateY(180deg)}
.brand-a:hover .brand-a-coin{animation-play-state:paused}
.brand-a:hover .brand-a-face{opacity:.85}
@keyframes brand-a-spin{from{transform:rotateY(0deg)}to{transform:rotateY(360deg)}}
@media(prefers-reduced-motion:reduce){.brand-a-coin{animation:none}}
.pe-title{font-size:8px;font-weight:700;letter-spacing:.1em;color:#fff;text-align:center;text-transform:uppercase;margin-bottom:8px}
.pe-item{margin-bottom:9px}
.pe-label{font-size:8px;font-weight:600;color:var(--t3);letter-spacing:.03em;margin-bottom:2px;text-align:center}
.pe-text{font-size:9px;color:var(--t4);line-height:1.55}
.pe-scale{display:flex;flex-direction:column;gap:2px;margin-top:4px}
.pe-scale-row{display:flex;align-items:baseline;gap:5px}
.pe-val{font-size:9px;font-family:monospace;color:var(--t2);min-width:38px;flex-shrink:0}
.pe-desc{font-size:8px;color:var(--t4);line-height:1.4}
.pe-btn{display:block;text-align:center;font-size:9px;font-weight:600;letter-spacing:.04em;color:#c0bcff;background:#0d0d20;border:0.5px solid var(--purple);border-radius:3px;padding:6px;text-decoration:none;transition:all .2s}
.pe-btn:hover{background:var(--purple);color:#fff}
/* ── Default desktop scale-up (≈ the old "133% zoom" look) ──────────────────
   The base sizes above are intentionally tiny for laptops/embeds. On any real
   desktop monitor (≥1100 CSS px) everything steps up ~30% so the cockpit is
   legible at native 100% browser zoom. Scrollable panels absorb the extra
   height; the centre charts flex to fit. */
@media(min-width:1100px){
  .main{grid-template-columns:210px 1fr 350px}
  .logo{font-size:18px}
  .sub,.topbar-right,.tick-item{font-size:12px}
  .cmd-label,.lm-label,.panel-title,.dn,.scen-label,.pe-title,.pe-label{font-size:10px}
  .cmd-val,.lm-val,.dscore{font-size:16px}
  .cmd-sub,.lm-sub,.gauge-pct-label,.conf-row,.ddelta{font-size:11px}
  .gauge-val{font-size:27px}
  #gauge-canvas{width:188px;height:104px}
  .gauge-context{font-size:10px}
  .dlabel,.dconf,.art-tag,.src-tier,.art-meta{font-size:9px}
  .sbtn,.tf-btn,.ct,.tab,.context-strip,.formula,.ca,.pe-text,.pe-val,.pe-desc,.pe-btn,.src-count,
  #log-body{font-size:11px}
  .alert-bar,.panel-body,.art-title,.src-name{font-size:13px}
}
@media(min-width:1700px){
  .main{grid-template-columns:230px 1fr 380px}
  .logo{font-size:20px}
  .cmd-val,.lm-val,.dscore{font-size:18px}
  .gauge-val{font-size:31px}
  #gauge-canvas{width:204px;height:113px}
  .art-title,.panel-body,.src-name{font-size:14px}
  .cmd-label,.lm-label,.panel-title,.dn,.pe-title,.pe-label{font-size:11px}
}
</style>
</head>
<body>
<div class="nuke-overlay" id="nuke-overlay"></div>
<div class="nuke-banner" id="nuke-banner" onclick="document.getElementById('nuke-banner').style.display='none'">
  ⚠ SEISMIC NUCLEAR ALERT — <span id="nuke-banner-text"></span> — Click to dismiss
</div>
<div class="topbar">
  <div class="topbar-left">
    <div class="logo">GLOBAL CONFLICT RISK MONITOR</div>
    <div class="topbar-live">
      <div class="topbar-live-dot" id="live-dot"></div>
      <span class="sub" id="ts">Connecting...</span>
    </div>
  </div>
  <div class="topbar-right">
    <span id="ev-count">—</span><span>|</span>
    <span id="src-count">—</span><span>|</span>
    <span id="nuc-status" style="color:#404040">● USGS</span><span>|</span>
    <span id="snap-id">—</span>
    <span id="model-cal-pill" style="display:none;font-size:10px;color:var(--amber);font-family:monospace;font-weight:600;letter-spacing:.05em"></span>
    <button class="op-toggle-btn" onclick="toggleOperatorPanel()" title="Operator Panel">⚙</button>
  </div>
</div>
<div class="cmd-strip">
  <div class="cmd-cell"><div class="cmd-label">Threat Level</div><div class="cmd-val" id="cmd-threat">—</div><div class="cmd-sub" id="cmd-threat-sub">awaiting data</div></div>
  <div class="cmd-cell"><div class="cmd-label">WWIII Risk (Annual)</div><div class="cmd-val" id="cmd-risk">—</div><div class="cmd-sub" id="cmd-risk-delta">—</div></div>
  <div class="cmd-cell"><div class="cmd-label">Primary Driver</div><div class="cmd-val" id="cmd-driver" style="font-size:11px">—</div><div class="cmd-sub" id="cmd-driver-sub">highest domain</div></div>
  <div class="cmd-cell"><div class="cmd-label">Confidence</div><div class="cmd-val" id="cmd-conf">—</div><div class="cmd-sub" id="cmd-conf-sub">data quality</div></div>
  <div class="cmd-cell"><div class="cmd-label">6h Trend</div><div class="cmd-val" id="cmd-trend">—</div><div class="cmd-sub" id="cmd-trend-sub">vs 6 hrs ago</div></div>
</div>
<div id="alert-bar" class="alert-bar"></div>
<div class="ticker"><div class="ticker-inner" id="ticker-inner"><span class="tick-item"><span class="tick-dot" style="background:#6060a0"></span>Awaiting live feed...</span></div></div>
<div class="main">
  <div class="left-panel">
    <div class="panel-title">THREAT LEVEL</div>
    <div class="gauge-wrap">
      <canvas id="gauge-canvas" width="148" height="82"></canvas>
      <div class="gauge-val" id="gauge-val">—</div>
      <div class="gauge-pct-label" id="gauge-ratio">— × above baseline</div>
      <div style="font-size:8px;color:var(--t4);text-align:center;margin-top:1px">annual P(WWIII) — log scale 0.1%→50% · <a href="{{BASE_PATH}}/methodology" style="color:var(--purple);text-decoration:none">methodology ↗</a></div>
      <div class="gauge-context">
        <div class="gauge-context-row"><span>Baseline:</span><span style="color:var(--t2)">0.10% / year</span></div>
        <div class="gauge-context-row"><span>This reading:</span><span id="gauge-ratio-ctx" style="color:var(--amber)">—</span></div>
        <div class="gauge-context-row"><span>Normal:</span><span style="color:var(--t2)">0.5–2.0%</span></div>
        <div class="gauge-context-row"><span>Elevated:</span><span style="color:var(--amber)">≥1.5%</span></div>
        <div class="gauge-context-row"><span>Critical:</span><span style="color:var(--red)">≥5.0%</span></div>
      </div>
      <div class="conf-row">
        <span style="color:var(--t4);font-size:8px">CONF</span>
        <div class="conf-bar"><div class="conf-fill" id="conf-fill" style="width:0%"></div></div>
        <span id="conf-pct" style="font-size:9px;color:var(--t2)">—</span>
      </div>
    </div>
    <!-- At-a-glance rail: the two derived windows + freshness. The annual reading is
         the hero gauge above; regime ×, P₀, GP-event count and elevated-domain tally
         are model internals that live in the methodology and model-state footer. -->
    <div class="left-metrics">
      <div class="lm"><div class="lm-label">30-DAY WINDOW</div><div class="lm-val" id="m-30d">—</div><div class="lm-sub" style="color:var(--t4)">rolling estimate</div></div>
      <div class="lm"><div class="lm-label">90-DAY WINDOW</div><div class="lm-val" id="m-90d">—</div><div class="lm-sub" style="color:var(--t4)">rolling estimate</div></div>
      <div class="lm"><div class="lm-label">LAST COMPUTED</div><div class="lm-val" id="m-computed" style="font-size:11px">—</div><div class="lm-sub" style="color:var(--t4)">model update time</div></div>
    </div>
    <div class="plain-eng">
      <div class="pe-title">WHAT THIS MEANS</div>
      <div class="pe-item">
        <div class="pe-label">THE NUMBER</div>
        <div class="pe-text">Estimated chance a world-war-scale conflict starts within 12 months, read from live news. For scale: ~0.1% baseline · ≥8% acute crisis · ~40% = Cuba 1962.</div>
      </div>
      <div class="pe-item">
        <div class="pe-label">THE 8 BARS</div>
        <div class="pe-text">Risk domains (military, nuclear, diplomatic…). Several spiking at once compounds danger. Click a bar to see the articles behind it.</div>
      </div>
      <div class="pe-item">
        <div class="pe-label">REGIME ×</div>
        <div class="pe-text">A multiplier for slow-moving conditions (wars, treaties, nuclear posture) that sets the floor before any news is read.</div>
      </div>
    </div>
    <!-- Pinned foot: methodology link + rotating RAiTHE mark. Lives outside the
         scrollable explainer so it (and the metrics above) stay visible while only
         the descriptive prose yields when the viewport is short. -->
    <div class="left-foot">
      <a href="{{BASE_PATH}}/methodology" class="pe-btn">Full methodology &amp; math ↗</a>
      <a href="https://raithe.ca" target="_blank" rel="noopener" class="brand-a" title="RAiTHE Industries"><span class="brand-a-coin"><img class="brand-a-face" src="{{BASE_PATH}}/logo-a.png" alt="RAiTHE Industries" width="40" height="40"><img class="brand-a-face brand-a-back" src="{{BASE_PATH}}/logo-a.png" alt="" aria-hidden="true" width="40" height="40"></span></a>
    </div>
  </div>
  <div class="center-panel">
    <div class="domains" id="domain-grid"></div>
    <div class="scen-row">
      <span class="scen-label">SCENARIO:</span>
      <button class="sbtn active" id="scen-live" onclick="setScen('live')">Live data</button>
      <button class="sbtn" id="scen-hot" onclick="setScen('hot')">Hot war</button>
      <button class="sbtn" id="scen-cold" onclick="setScen('cold')">Cold war</button>
      <button class="sbtn" id="scen-nuke" onclick="setScen('nuke')">Nuclear alert</button>
      <button class="sbtn" id="scen-epstein" onclick="setScen('epstein')">Epstein fallout</button>
      <button class="sbtn" id="scen-religious" onclick="setScen('religious')">Religious extremism</button>
    </div>
    <div class="charts-area">
      <div class="chart-card">
        <div class="ct"><span>P(WWIII) — full historical timeline (persists across sessions)</span><span style="font-size:8px;color:var(--t4);font-weight:400" id="spike-label"></span></div>
        <div class="chart-inner"><canvas id="timeline-chart"></canvas></div>
      </div>
      <div class="chart-card">
        <div class="ct">Domain scores — current snapshot (with Δ trend)</div>
        <div class="chart-inner"><canvas id="domain-chart"></canvas></div>
      </div>
    </div>
    <div class="context-strip">
      <div class="ca">Baseline: <span>0.10%</span></div>
      <div class="ca">Hist avg (2026): <span>~1.7%</span></div>
      <div class="ca">Session peak: <span id="ca-peak">—</span></div>
      <div class="ca">6h change: <span id="ca-6h">—</span></div>
      <div class="ca">Session low: <span id="ca-low">—</span></div>
    </div>
    <div class="formula">
      P₀ = <span>2/2026 = 0.000987/yr</span> · adjusted = <span id="f-adj">—</span> · likelihood = <span id="f-lik">—</span><br>
      P(WWIII|E) = <span id="f-post">—</span> · 30d: <span id="f-30d">—</span> · 90d: <span id="f-90d">—</span>
    </div>
    <div class="meta-pills" id="meta-row"></div>
  </div>
  <div class="right-panel">
    <div class="tabs">
      <div class="tab active" id="tab-articles" onclick="switchTab('articles')">Articles</div>
      <div class="tab" id="tab-sources" onclick="switchTab('sources')">Sources</div>
      <div class="tab" id="tab-log" onclick="switchTab('log')">Live log</div>
    </div>
    <div style="padding:3px 8px;font-size:8px;color:var(--t4);border-bottom:0.5px solid var(--border);display:flex;justify-content:space-between;align-items:center;flex-shrink:0">
      <span id="art-count">loading...</span>
      <span><span id="art-filter-label" style="color:var(--purple)"></span><span onclick="clearFilters()" style="cursor:pointer;color:var(--t4);margin-left:6px">✕ clear</span></span>
    </div>
    <div style="padding:3px 8px;display:flex;gap:3px;border-bottom:0.5px solid var(--border);flex-shrink:0;flex-wrap:wrap" title="Filter by article age — each band is a distinct era of pulled news, not a cumulative window">
      <button class="tf-btn active" data-min="0"    data-max="0"    onclick="setAgeFilter(0,0,this)">All</button>
      <button class="tf-btn"        data-min="0"    data-max="24"   onclick="setAgeFilter(0,24,this)">&lt;24h</button>
      <button class="tf-btn"        data-min="24"   data-max="72"   onclick="setAgeFilter(24,72,this)">1–3d</button>
      <button class="tf-btn"        data-min="72"   data-max="672"  onclick="setAgeFilter(72,672,this)">3d–4w</button>
      <button class="tf-btn"        data-min="672"  data-max="4368" onclick="setAgeFilter(672,4368,this)">4w–6m</button>
      <button class="tf-btn"        data-min="4368" data-max="8760" onclick="setAgeFilter(4368,8760,this)">6–12m</button>
    </div>
    <div style="padding:2px 8px;font-size:7px;color:var(--t4);border-bottom:0.5px solid var(--border);flex-shrink:0;letter-spacing:.02em">▣ Tip: click any domain card to audit the articles feeding its score</div>
    <div class="panel-body" id="panel-articles"></div>
    <div class="panel-body" id="panel-sources" style="display:none"></div>
    <div class="panel-body" id="panel-log" style="display:none"><div id="log-body"></div></div>
  </div>
</div>
<script>
const BASE_PATH='{{BASE_PATH}}';
function toET(date){return date.toLocaleTimeString('en-US',{timeZone:'America/Toronto',hour:'numeric',minute:'2-digit',second:'2-digit',hour12:true})+' ET';}
function toETShort(date){return date.toLocaleTimeString('en-US',{timeZone:'America/Toronto',hour:'numeric',minute:'2-digit',hour12:true})+' ET';}
let _lastCalTs=null,_firstSnapReceived=false;
// Show warmup state immediately on page load
(function(){const pill=document.getElementById('model-cal-pill');if(!pill)return;pill.style.display='inline-block';pill.textContent='⏳ COLLECTING DATA...';pill.classList.add('cal-fresh');})();
function updateModelCalIndicator(calAt,conf,evCount){
  const pill=document.getElementById('model-cal-pill');if(!pill)return;
  if(calAt){
    const d=new Date(calAt);if(isNaN(d.getTime()))return;
    const ageMs=Date.now()-d.getTime();const isFresh=ageMs<10*60*1000;
    const tsStr=toETShort(d);
    pill.style.display='inline-block';pill.classList.add('cal-fresh');
    pill.textContent=isFresh?'⬆ UPDATED '+tsStr:'● CALIBRATED '+tsStr;
    if(_lastCalTs!==calAt)_lastCalTs=calAt;
    return;
  }
  if(!_firstSnapReceived||evCount===0){
    pill.style.display='inline-block';pill.classList.add('cal-fresh');
    pill.textContent='⏳ COLLECTING';
  }else if(evCount<15){
    pill.style.display='inline-block';pill.classList.add('cal-fresh');
    pill.textContent='◐ POPULATING';
  }else if(conf==null||conf<0.90){
    pill.style.display='inline-block';pill.classList.add('cal-fresh');
    pill.textContent='◐ CALIBRATING';
  }else{
    pill.style.display='none';pill.classList.remove('cal-fresh');
  }
}
async function refreshSrcCount(){
  // Sources are a statement, not a metric: we draw on N curated feeds, full stop.
  // A live "X/N delivering" ratio is misleading — RSS feeds publish in bursts, so
  // a perfectly healthy feed with nothing new this poll reads as "down". We state
  // the breadth of the source base instead.
  try{const r=await fetch(BASE_PATH+'/api/sources');const d=await r.json();
  const cfg=(d.configured_sources||[]).length;
  document.getElementById('src-count').textContent=cfg+' sources';
  }catch(e){}
}
refreshSrcCount();
const DID=['military_escalation','nuclear_posture','diplomatic_breakdown','economic_warfare','cyber_info_ops','alliance_activation','great_power_conflict','wmd_mass_casualty'];
// Live timeline chart capacity. Mirrors the server's epoch ring (MAX_EPOCH_ENTRIES
// = 350,640 ≈ 4 days at the 1s tick), and is applied identically on initial load
// (applyTimeline) and on live append — so the ceiling is one stated, auditable
// value rather than two that disagree. GCRM's signal lives in the MOVEMENT over
// time, so we keep the full durable window instead of truncating recent history.
const MAX_TL_POINTS=350640;
const DSHORT=['Military','Nuclear','Diplomatic','Economic','Cyber','Alliance','Gr.Power','WMD'];
const DCOLORS=['--mil','--nuc','--dip','--eco','--cyb','--ali','--gp','--wmd'];
const DTAGS={military_escalation:'MIL',nuclear_posture:'NUC',diplomatic_breakdown:'DIP',economic_warfare:'ECO',cyber_info_ops:'CYB',alliance_activation:'ALI',great_power_conflict:'GP',wmd_mass_casualty:'WMD'};
const TAG_COLORS={MIL:'#E24B4A',NUC:'#FF6B35',DIP:'#7F77DD',ECO:'#1D9E75',CYB:'#00CED1',ALI:'#EF9F27',GP:'#FF69B4',WMD:'#FF0000'};
const GC=document.getElementById('gauge-canvas').getContext('2d');
let lastGaugePct=0;
// ── Logarithmic risk→gauge mapping ──────────────────────────────────────────
// Linear 0–5% hid reality: anything above 5% (Ukraine ~10%, Cuba ~40%) pinned
// at full. A log scale from 0.1% baseline to 50% ceiling spreads the whole
// historically-meaningful range across the dial so every regime is legible.
const G_MIN=0.001,G_MAX=0.50,G_DEN=Math.log10(G_MAX/G_MIN); // den = log10(500)
function riskToFrac(p){
  if(p<=G_MIN)return 0;
  return Math.min(1,Math.log10(p/G_MIN)/G_DEN);
}
// Zone ends as gauge fractions of the alert thresholds (elevated 1.5%, critical 5%).
const GZ_ELEV=riskToFrac(0.015),GZ_CRIT=riskToFrac(0.05);
// Tick reference points (risk → short label).
const G_TICKS=[{p:0.001,l:'.1'},{p:0.01,l:'1'},{p:0.05,l:'5'},{p:0.50,l:'50'}];
function drawGauge(pct){
  pct=Math.min(1,pct);const W=148,H=82,cx=W/2,cy=H-10,r=58;
  GC.clearRect(0,0,W,H);
  GC.beginPath();GC.arc(cx,cy,r,Math.PI,0);GC.strokeStyle='#1e1e38';GC.lineWidth=9;GC.stroke();
  const zones=[{e:GZ_ELEV,c:'#1D9E75'},{e:GZ_CRIT,c:'#EF9F27'},{e:1,c:'#E24B4A'}];let s=Math.PI;
  for(const z of zones){const e=Math.PI+z.e*Math.PI;GC.beginPath();GC.arc(cx,cy,r,s,Math.min(e,Math.PI+pct*Math.PI));GC.strokeStyle=z.c;GC.lineWidth=9;GC.stroke();if(pct<=z.e)break;s=e;}
  // Tick marks + labels at log reference points
  GC.font='6px monospace';GC.fillStyle='#6878a0';GC.textAlign='center';
  for(const t of G_TICKS){const f=riskToFrac(t.p),a=Math.PI+f*Math.PI;
    const x1=cx+(r-5)*Math.cos(a),y1=cy+(r-5)*Math.sin(a),x2=cx+(r+5)*Math.cos(a),y2=cy+(r+5)*Math.sin(a);
    GC.beginPath();GC.moveTo(x1,y1);GC.lineTo(x2,y2);GC.strokeStyle='#3a4560';GC.lineWidth=1;GC.stroke();
    const lx=cx+(r+10)*Math.cos(a),ly=cy+(r+10)*Math.sin(a)+2;GC.fillText(t.l,lx,ly);}
  const a=Math.PI+pct*Math.PI;GC.beginPath();GC.moveTo(cx,cy);GC.lineTo(cx+(r-13)*Math.cos(a),cy+(r-13)*Math.sin(a));GC.strokeStyle='#ffffff';GC.lineWidth=2;GC.stroke();
  GC.beginPath();GC.arc(cx,cy,4,0,2*Math.PI);GC.fillStyle='#ffffff';GC.fill();
}
drawGauge(0);
function animateGauge(target){const diff=target-lastGaugePct;const step=diff*0.12;lastGaugePct+=step;drawGauge(lastGaugePct);if(Math.abs(diff)>0.0005)requestAnimationFrame(()=>animateGauge(target));}
const GRID='rgba(255,255,255,0.04)',TICK='#9090c0';
let spikeAnnotations={};
const tlChart=new Chart(document.getElementById('timeline-chart'),{type:'line',data:{labels:[],datasets:[{label:'Annual',data:[],borderColor:'#E24B4A',backgroundColor:'rgba(226,75,74,0.08)',fill:true,tension:.3,pointRadius:0,borderWidth:1.5},{label:'30-day',data:[],borderColor:'#EF9F27',fill:false,tension:.3,pointRadius:0,borderWidth:1,borderDash:[3,3]},{label:'Baseline',data:[],borderColor:'rgba(96,96,160,0.4)',fill:false,tension:0,pointRadius:0,borderWidth:0.5,borderDash:[6,4]},{label:'HistAvg',data:[],borderColor:'rgba(127,119,221,0.3)',fill:false,tension:0,pointRadius:0,borderWidth:0.5,borderDash:[2,6]}]},options:{responsive:true,maintainAspectRatio:false,plugins:{legend:{display:false},annotation:{annotations:spikeAnnotations}},scales:{x:{display:false},y:{min:0,grid:{color:GRID},ticks:{color:TICK,font:{size:9},callback:v=>(v*100).toFixed(2)+'%'}}},animation:{duration:200,easing:'easeOutQuart'}}});
const dmChart=new Chart(document.getElementById('domain-chart'),{type:'bar',data:{labels:DSHORT,datasets:[{label:'Current',data:new Array(8).fill(0),backgroundColor:new Array(8).fill('#1D9E75'),borderRadius:2,borderWidth:0}]},options:{responsive:true,maintainAspectRatio:false,plugins:{legend:{display:false}},scales:{x:{grid:{display:false},ticks:{color:TICK,font:{size:9}}},y:{min:0,max:1,grid:{color:GRID},ticks:{color:TICK,font:{size:9},callback:v=>Math.round(v*100)+'%'}}},animation:{duration:300,easing:'easeOutQuart'}}});
let curScen='live',liveData=null,prevDomainScores={},sessionPeak=0,sessionLow=Infinity,history6h=[],spikeIdx=0;
function dc(s){return s>=.7?'#E24B4A':s>=.35?'#EF9F27':s>=.2?'#7F77DD':'#1D9E75'}
function pc(p){return p>=.05?'var(--red)':p>=.015?'var(--amber)':'var(--green)'}
function confLabel(c){return c>=.8?'High':c>=.5?'Medium':'Low'}
function threatLabel(p){return p>=.05?'CRITICAL':p>=.015?'ELEVATED':p>=.005?'MODERATE':'NORMAL'}
function domainLabel(id){return id.replace(/_/g,' ').replace(/\b./g,c=>c.toUpperCase())}
const SCEN={live:null,hot:{military_escalation:.82,diplomatic_breakdown:.75,alliance_activation:.68,great_power_conflict:.80,economic_warfare:.60},cold:{diplomatic_breakdown:.65,economic_warfare:.70,cyber_info_ops:.60,great_power_conflict:.55,military_escalation:.40},nuke:{nuclear_posture:.78,wmd_mass_casualty:.55,military_escalation:.70,alliance_activation:.60,great_power_conflict:.75},epstein:{diplomatic_breakdown:.45,cyber_info_ops:.55,great_power_conflict:.42,economic_warfare:.38,military_escalation:.25},religious:{military_escalation:.65,diplomatic_breakdown:.60,wmd_mass_casualty:.42,alliance_activation:.45,great_power_conflict:.50,cyber_info_ops:.35}};
function setScen(id){curScen=id;document.querySelectorAll('.sbtn').forEach(b=>b.classList.remove('active'));document.getElementById('scen-'+id).classList.add('active');if(liveData)applyData(liveData);}
let tickerItems=[];
function updateTicker(articles){if(!articles||!articles.length)return;tickerItems=articles.slice(0,40);const inner=document.getElementById('ticker-inner');const html=tickerItems.map(a=>{const dot=a.tier===1?'#1D9E75':a.tier===2?'#6060a0':'#EF9F27';return`<span class="tick-item" onclick="openArticle('${a.url}')"><span class="tick-dot" style="background:${dot}"></span>${a.title}</span>`;}).join('');inner.innerHTML=html+html;}
function openArticle(url){window.open(url,'_blank')}
let currentTab='articles';
function switchTab(tab){currentTab=tab;['articles','sources','log'].forEach(t=>{document.getElementById('tab-'+t).classList.toggle('active',t===tab);document.getElementById('panel-'+t).style.display=t===tab?'block':'none';});if(tab==='articles')fetchArticles();if(tab==='sources'){document.getElementById('panel-sources').innerHTML='<div style="padding:8px 10px;font-size:9px;color:var(--t4)">Loading sources...</div>';fetchSources();}if(tab==='log'){const el=document.getElementById('log-body');el.innerHTML=logLines.slice(0,200).join('<br>');}}
let lastMovers=new Set(),_artDomainFilter='',_artSrcFilter='',_artAgeMin=0,_artAgeMax=0;
// Age-bracket filter: each band shows a distinct slice of pulled news by age,
// not a cumulative "last X hours" window. min/max are in hours; (0,0) = All.
// e.g. (672,4368) = items aged 4 weeks to 6 months — "what GCRM pulled ~a month ago".
function setAgeFilter(min,max,btn){_artAgeMin=min;_artAgeMax=max;document.querySelectorAll('.tf-btn').forEach(b=>b.classList.remove('active'));if(btn)btn.classList.add('active');renderArticles(_artCache,_artTotal);}
// Article timestamps are shown in Toronto time (ET) as the primary clock, with
// the absolute UTC instant as a secondary reference and the time GCRM actually
// pulled the article on a second line — so it reads clearly as "got X at Y ET
// (Z UTC), pulled at W ET". Source feeds deliver UTC-normalised timestamps
// (feed-rs converts on parse), so UTC is the honest universal reference.
function fmtArticleDate(isoStr,ingestedIso){
  try{if(!isoStr)return'<span style="color:#404060">— no date —</span>';const pub=new Date(isoStr);if(isNaN(pub.getTime()))return`<span style="color:#404060">${isoStr}</span>`;
  const now=Date.now();const ageMs=now-pub.getTime();const ageH=ageMs/3600000;
  const isFuture=ageMs<-120000;
  const torontoTime=pub.toLocaleTimeString('en-US',{timeZone:'America/Toronto',hour:'numeric',minute:'2-digit',hour12:true});
  const torontoDate=pub.toLocaleDateString('en-US',{timeZone:'America/Toronto',month:'short',day:'numeric'});
  const todayDate=new Date().toLocaleDateString('en-US',{timeZone:'America/Toronto',month:'short',day:'numeric'});
  const torontoFull=(torontoDate!==todayDate)?torontoDate+' '+torontoTime+' ET':torontoTime+' ET';
  const utcTime=pub.toLocaleTimeString('en-GB',{timeZone:'UTC',hour:'2-digit',minute:'2-digit',hour12:false});
  // Secondary line: when GCRM pulled it (ET) + the absolute UTC reference.
  let pulledPart='';if(ingestedIso){const ing=new Date(ingestedIso);if(!isNaN(ing.getTime())){const ingT=ing.toLocaleTimeString('en-US',{timeZone:'America/Toronto',hour:'numeric',minute:'2-digit',hour12:true});pulledPart='pulled '+ingT+' ET · ';}}
  const subLine='<span style="display:block;color:#404058;font-size:7px;margin-top:1px">'+pulledPart+utcTime+' UTC</span>';
  if(isFuture){return'<span style="display:block;font-family:monospace;color:#606080">'+torontoFull+' <span style="color:#404058;font-size:7px">(future-dated by source)</span></span>'+subLine;}
  let relAge;if(ageH<1){const m=Math.floor(ageMs/60000);relAge=m<=1?'just now':m+'m ago';}else if(ageH<24){const h=Math.floor(ageH),m=Math.floor((ageH-h)*60);relAge=m>0?h+'h '+m+'m ago':h+'h ago';}else if(ageH<168){relAge=Math.floor(ageH/24)+'d '+Math.floor(ageH%24)+'h ago';}else{relAge=Math.floor(ageH/24)+'d ago';}
  let badge='';if(ageH>168)badge='<span style="font-size:7px;padding:1px 4px;background:#200800;color:#c05818;border-radius:2px;margin-left:4px">'+Math.floor(ageH/24)+'d OLD</span>';else if(ageH>24)badge='<span style="font-size:7px;padding:1px 4px;background:#141400;color:#7a7a20;border-radius:2px;margin-left:4px">'+Math.floor(ageH)+'h OLD</span>';
  return'<span style="display:block;font-family:monospace;color:#9090c0">'+torontoFull+'<span style="color:#505070;font-family:inherit"> · '+relAge+'</span>'+badge+'</span>'+subLine;
  }catch(err){return'<span style="color:#404060">'+(isoStr||'unknown date')+'</span>';}}
function renderArticles(arts,total){const el=document.getElementById('panel-articles');if(!el)return;const now=Date.now();let filtered=arts;if(_artAgeMin>0||_artAgeMax>0)filtered=filtered.filter(a=>{try{const age=now-new Date(a.published_at).getTime();return age>=_artAgeMin*3600000&&(_artAgeMax===0||age<_artAgeMax*3600000);}catch{return false;}});if(_artDomainFilter)filtered=filtered.filter(a=>(a.domain_tags||[]).includes(_artDomainFilter));if(_artSrcFilter)filtered=filtered.filter(a=>a.source===_artSrcFilter);const RENDER_CAP=400;const countEl=document.getElementById('art-count');if(countEl)countEl.textContent=(filtered.length>RENDER_CAP?(RENDER_CAP+' of '+filtered.length):filtered.length)+' shown / '+total+' total';const scrollTop=el.scrollTop;el.innerHTML=filtered.slice(0,RENDER_CAP).map(a=>{const isMover=lastMovers.has(a.id)||lastMovers.has(a.url);const tierCls=isMover?'art-mover':'art-tier'+a.tier;const tags=(a.domain_tags||[]).map(dt=>{const tag=DTAGS[dt]||dt.slice(0,3).toUpperCase();const col=TAG_COLORS[tag]||'#6060a0';return'<span class="art-tag" data-dt="'+dt+'" style="background:'+col+'22;color:'+col+';cursor:pointer" onclick="filterByDomain(this.dataset.dt)">'+tag+'</span>';}).join('');const moverBadge=isMover?'<span style="font-size:7px;padding:1px 4px;background:#2a0000;color:#ff6060;border-radius:2px;margin-left:4px">↑MODEL</span>':'';const title=a.title.replace(/</g,'&lt;').replace(/>/g,'&gt;');const srcColor=a.tier===1?'#1D9E75':a.tier===2?'#7070a0':'#EF9F27';return'<div class="art-item '+tierCls+'" data-url="'+encodeURIComponent(a.url)+'" onclick="window.open(decodeURIComponent(this.dataset.url),\'_blank\')"><div class="art-title">'+title+moverBadge+'</div><div class="art-meta" style="flex-direction:column;align-items:flex-start;gap:2px"><span style="color:'+srcColor+'">'+a.source+'</span><span>'+fmtArticleDate(a.published_at,a.fetched_at||a.ingested_at)+'</span></div>'+(tags?'<div class="art-tags">'+tags+'</div>':'')+' </div>';}).join('');if(scrollTop>0)el.scrollTop=scrollTop;updateTicker(filtered.slice(0,40));}
// Keep the visible filter label + domain-card highlight in sync with the active
// domain filter so the audit path is discoverable (a domain card click filters
// the feed to the articles feeding that score).
function syncDomainFilterUI(){const lbl=document.getElementById('art-filter-label');if(lbl)lbl.textContent=_artDomainFilter?('▣ '+domainLabel(_artDomainFilter)+' signals'):'';document.querySelectorAll('#domain-grid .domain').forEach((d,i)=>{d.classList.toggle('active',DID[i]===_artDomainFilter);});}
function filterByDomain(dt){_artDomainFilter=(_artDomainFilter===dt)?'':dt;syncDomainFilterUI();fetchArticles();}
function clearFilters(){_artDomainFilter='';_artSrcFilter='';_artAgeMin=0;_artAgeMax=0;document.querySelectorAll('.tf-btn').forEach(b=>b.classList.toggle('active',b.dataset.min==='0'&&b.dataset.max==='0'));syncDomainFilterUI();fetchArticles();}
let _artCache=[],_artTotal=0;
async function fetchArticles(){try{let url=BASE_PATH+'/api/articles?limit=6000'+(_artDomainFilter?('&domain='+encodeURIComponent(_artDomainFilter)):'');const r=await fetch(url);const d=await r.json();
  // Merge rather than replace, so a manual refresh never discards deeper history
  // already accumulated via the WS push merge.
  const have=new Set(_artCache.map(a=>a.id||a.url));
  const add=(d.articles||[]).filter(a=>!have.has(a.id||a.url));
  _artCache=_artCache.length?_artCache.concat(add):(d.articles||[]);
  _artCache.sort((x,y)=>new Date(y.published_at)-new Date(x.published_at));
  if(_artCache.length>8000)_artCache.length=8000;
  _artTotal=d.total;renderArticles(_artCache,_artTotal);}catch(e){console.warn('fetchArticles error',e);}}
async function fetchSources(){try{const r=await fetch(BASE_PATH+'/api/sources');if(!r.ok)throw new Error('HTTP '+r.status);const d=await r.json();const el=document.getElementById('panel-sources');if(!el)return;const active=d.active_sources||{};const configured=d.configured_sources||[];el.innerHTML='<div style="padding:5px 10px;font-size:9px;color:var(--t3);border-bottom:0.5px solid var(--border);text-align:center;letter-spacing:.04em">'+configured.length+' curated intelligence sources</div>'+configured.map(s=>{const cnt=active[s.source]||0;const barW=Math.min(100,cnt/10*100);const barCol=cnt>100?'var(--green)':cnt>0?'var(--amber)':'#2a2a3a';const tierCol=s.tier===1?'var(--green)':'var(--t4)';return'<div class="src-item" style="'+(cnt===0?'opacity:0.45':'')+'"><div style="flex:1;min-width:0"><div class="src-name" style="white-space:nowrap;overflow:hidden;text-overflow:ellipsis">'+s.source+'</div><div style="font-size:7px;color:var(--t4);white-space:nowrap;overflow:hidden;text-overflow:ellipsis">'+s.url.replace('https://','').slice(0,42)+'</div><div style="margin-top:3px;height:2px;background:var(--border);border-radius:1px;width:80px;overflow:hidden"><div style="height:100%;width:'+barW+'%;background:'+barCol+';border-radius:1px;transition:width .4s"></div></div></div><div style="text-align:right;flex-shrink:0;margin-left:6px"><span class="src-tier" style="font-size:7px;padding:1px 4px;border-radius:2px;background:'+(s.tier===1?'#0a1a0a':'#101018')+';color:'+tierCol+'">'+(s.tier===1?'T1':'T2')+'</span><div class="src-count" style="margin-top:2px">'+cnt+' art</div></div></div>'}).join('');}catch(e){const el=document.getElementById('panel-sources');if(el)el.innerHTML='<div style="padding:8px 10px;font-size:9px;color:var(--red)">Sources fetch failed: '+e.message+'</div>';}}
const logLines=[];
function addLog(msg,color='var(--t4)'){logLines.unshift('<span style="color:'+color+'">'+toETShort(new Date())+' '+msg+'</span>');if(logLines.length>500)logLines.pop();if(currentTab==='log'){const el=document.getElementById('log-body');el.innerHTML=logLines.slice(0,200).join('<br>');}}
let prevAnnual=null,spikeAnnual=null;
function checkSpike(pA){if(prevAnnual===null){prevAnnual=pA;return;}const delta=pA-prevAnnual;if(Math.abs(delta)>0.0002){const id='spike_'+spikeIdx++;const color=delta>0?'#E24B4A88':'#1D9E7588';spikeAnnotations[id]={type:'line',xMin:tlChart.data.labels.length-1,xMax:tlChart.data.labels.length-1,borderColor:color,borderWidth:1,label:{display:true,content:(delta>0?'▲':'▼')+' '+(Math.abs(delta)*100).toFixed(3)+'%',color:'#c0c0e0',font:{size:7},position:'start',backgroundColor:'rgba(7,7,15,0.8)',padding:2}};const keys=Object.keys(spikeAnnotations);if(keys.length>20)delete spikeAnnotations[keys[0]];tlChart.options.plugins.annotation.annotations=spikeAnnotations;document.getElementById('spike-label').textContent='Last spike: '+(delta>0?'+':'')+(delta*100).toFixed(3)+'% at '+toETShort(new Date());}prevAnnual=pA;}
function update6hBuffer(pA){const now=Date.now();history6h.push({t:now,p:pA});history6h=history6h.filter(e=>now-e.t<6*3600*1000);}
function get6hDelta(pA){if(history6h.length<2)return null;return pA-history6h[0].p;}
async function pollNuclear(){try{const r=await fetch(BASE_PATH+'/api/nuclear');const d=await r.json();const status=document.getElementById('nuc-status');if(d.status==='alert'){status.style.color='#c0392b';status.textContent='● USGS ALERT';}else if(d.status==='monitoring'){status.style.color='#1D9E75';status.textContent='● USGS ✓';}else{status.style.color='#404040';status.textContent='● USGS off';}const sig=(d.alerts||[]).filter(a=>a.level!=='anomaly'||a.confidence>=0.5);if(sig.length>0){const top=sig.reduce((m,a)=>a.confidence>m.confidence?a:m,sig[0]);document.getElementById('nuke-banner-text').textContent=top.level+' | M'+top.magnitude+' depth='+top.depth_km+'km near '+top.nearest_site_name+' ('+Math.round(top.distance_km)+'km) | score='+Math.round(top.confidence*100)+'%';document.getElementById('nuke-banner').style.display='block';document.getElementById('nuke-overlay').style.display='block';addLog('⚠ SEISMIC ANOMALY: '+top.description,'#c0392b');}else{document.getElementById('nuke-banner').style.display='none';document.getElementById('nuke-overlay').style.display='none';}}catch(e){document.getElementById('nuc-status').style.color='#404040';document.getElementById('nuc-status').textContent='● USGS off';}}
function applyData(d){
  _firstSnapReceived=true;
  const _evCount=(d.meta?.events_in_window||0);
  const conf=d.confidence??0.5;
  updateModelCalIndicator(d.model_calibrated_at||null,conf,_evCount);
  liveData=d;const ov=SCEN[curScen];const doms={};
  for(const[k,v]of Object.entries(d.domains||{}))doms[k]={...v,score:ov?(ov[k]??v.score*.25):v.score};
  const pA=d.probabilities.annual,p30=d.probabilities.thirty_day,p90=d.probabilities.ninety_day,dA=d.delta?.annual??0;
  if(pA>sessionPeak)sessionPeak=pA;if(pA<sessionLow)sessionLow=pA;update6hBuffer(pA);
  document.getElementById('ca-peak').textContent=(sessionPeak*100).toFixed(3)+'%';
  document.getElementById('ca-low').textContent=(sessionLow*100).toFixed(3)+'%';
  const delta6h=get6hDelta(pA);if(delta6h!==null){const d6el=document.getElementById('ca-6h');d6el.textContent=(delta6h>=0?'+':'')+(delta6h*100).toFixed(3)+'%';d6el.style.color=delta6h>0.0005?'#E24B4A':delta6h<-0.0005?'#1D9E75':'var(--t2)';}
  checkSpike(pA);animateGauge(riskToFrac(pA));
  const gv=document.getElementById('gauge-val');gv.textContent=(pA*100).toFixed(2)+'%';gv.style.color=pA>=.05?'#E24B4A':pA>=.015?'#EF9F27':'#1D9E75';
  const riskRatio=Math.round(pA/0.001);document.getElementById('gauge-ratio').textContent=riskRatio+'× above baseline (0.1%)';
  const ctxEl=document.getElementById('gauge-ratio-ctx');if(ctxEl){ctxEl.textContent=riskRatio+'× baseline';ctxEl.style.color=pA>=.05?'#E24B4A':pA>=.015?'#EF9F27':'#1D9E75';}
  document.getElementById('conf-fill').style.width=(conf*100).toFixed(0)+'%';document.getElementById('conf-pct').textContent=(conf*100).toFixed(0)+'%';
  const _cd=new Date(d.computed_at);const _et=toET(_cd);
  document.getElementById('ts').textContent='Live · '+_et+(curScen!=='live'?' · SCENARIO: '+curScen.toUpperCase():'')+' P₀=0.000987/yr';
  const threat=threatLabel(pA);const cmdThreat=document.getElementById('cmd-threat');cmdThreat.textContent=threat;cmdThreat.style.color=pA>=.05?'#E24B4A':pA>=.015?'#EF9F27':pA>=.005?'#7F77DD':'#1D9E75';
  document.getElementById('cmd-threat-sub').textContent=pA>=.05?'Immediate escalation risk':pA>=.015?'Above normal — monitor':pA>=.005?'Moderate background':'Baseline conditions';
  document.getElementById('cmd-risk').textContent=(pA*100).toFixed(2)+'%';document.getElementById('cmd-risk').style.color=pA>=.05?'#E24B4A':pA>=.015?'#EF9F27':'var(--t1)';
  document.getElementById('cmd-risk-delta').textContent=dA>0?'▲ +'+(dA*100).toFixed(4)+'% last snap':dA<0?'▼ '+(dA*100).toFixed(4)+'% last snap':'─ stable';
  document.getElementById('cmd-risk-delta').style.color=dA>0?'#E24B4A':dA<0?'#1D9E75':'var(--t4)';
  let topDomain='—',topScore=0;for(const[k,v]of Object.entries(doms)){if((v.score||0)>topScore){topScore=v.score;topDomain=k;}}
  document.getElementById('cmd-driver').textContent=domainLabel(topDomain);document.getElementById('cmd-driver').style.color=dc(topScore);
  document.getElementById('cmd-driver-sub').textContent=topScore?(Math.round(topScore*100)+'% — highest domain'):'no elevation';
  const confL=confLabel(conf);document.getElementById('cmd-conf').textContent=confL;document.getElementById('cmd-conf').style.color=conf>=.8?'#1D9E75':conf>=.5?'#EF9F27':'#7070a0';
  document.getElementById('cmd-conf-sub').textContent=(conf*100).toFixed(0)+'% data quality';
  const d6=get6hDelta(pA);const trendEl=document.getElementById('cmd-trend');
  if(d6===null){trendEl.textContent='—';}else{trendEl.textContent=(d6>=0?'+':'')+(d6*100).toFixed(3)+'%';trendEl.style.color=d6>0.0005?'#E24B4A':d6<-0.0005?'#1D9E75':'var(--t2)';document.getElementById('cmd-trend-sub').textContent='vs 6 hrs ago ('+history6h.length+' samples)';}
  const setMv=(id,val,color)=>{const el=document.getElementById(id);if(el){el.textContent=val;el.style.color=color;}};
  // Rail shows only the 30/90-day windows + last-computed (see HTML note above).
  // The annual reading is the hero gauge; regime ×, P₀, GP count and the elevated
  // tally are surfaced in the methodology and the model-state footer, not the rail.
  setMv('m-30d',(p30*100).toFixed(2)+'%',pc(p30*4));setMv('m-90d',(p90*100).toFixed(2)+'%',pc(p90*2));
  setMv('m-computed',toETShort(new Date(d.computed_at)),'var(--t2)');
  const elev=d.co_occurrence.elevated_count; // still feeds the model-state co-occurrence line below
  const ab=document.getElementById('alert-bar');const al=d.alert.level;
  ab.className='alert-bar'+(al==='critical'?' critical':al==='elevated'?' elevated':'');ab.textContent=d.alert.message||'';
  const grid=document.getElementById('domain-grid');grid.innerHTML='';
  DID.forEach(id=>{const ds=doms[id]||{score:0,label:'low',confidence:0,event_count:0,great_power_events:0};const prev=prevDomainScores[id]||0;const delta=ds.score-prev;const pct=Math.round(ds.score*100);const col=dc(ds.score);let arrow='';if(Math.abs(delta)>0.01){arrow=delta>0?'<span class="ddelta" style="color:#E24B4A">▲</span>':'<span class="ddelta" style="color:#1D9E75">▼</span>';}const tag=DTAGS[id]||'?';const tagCol=TAG_COLORS[tag]||'#6060a0';const div=document.createElement('div');div.className=(_artDomainFilter===id)?'domain active':'domain';div.title='Click to audit '+id.replace(/_/g,' ')+': filters the article feed to the signals feeding this score. "signals" = articles tagged to this domain; "gt-power" = those involving the US, Russia, China, or NATO.';div.onclick=()=>filterByDomain(id);div.innerHTML='<div class="dn" style="display:flex;justify-content:space-between;align-items:center"><span>'+id.replace(/_/g,' ').toUpperCase()+'</span><span class="art-tag" style="background:'+tagCol+'22;color:'+tagCol+'">'+tag+'</span></div><div class="dbar"><div class="dfill" style="width:'+pct+'%;background:'+col+'"></div></div><div class="drow"><span class="dscore" style="color:'+col+'">'+pct+'%'+arrow+'</span><span class="dlabel '+(ds.label||'low')+'">'+(ds.label||'low')+'</span></div><div class="dconf">'+(ds.event_count||0)+' signals · '+Math.round((ds.confidence||0)*100)+'% conf'+(ds.great_power_events>0?' · '+ds.great_power_events+' gt-power':'')+'</div>';grid.appendChild(div);});
  prevDomainScores={};DID.forEach(id=>prevDomainScores[id]=doms[id]?.score||0);
  dmChart.data.datasets[0].data=DID.map(id=>doms[id]?.score||0);dmChart.data.datasets[0].backgroundColor=DID.map(id=>dc(doms[id]?.score||0));dmChart.update();
  document.getElementById('f-adj').textContent=d.prior.adjusted_prior.toFixed(6);
  document.getElementById('f-lik').textContent='×'+d.co_occurrence.boost.toFixed(1)+' boost, '+elev+' elevated';
  document.getElementById('f-post').textContent=(pA*100).toFixed(6)+'%';document.getElementById('f-30d').textContent=(p30*100).toFixed(6)+'%';document.getElementById('f-90d').textContent=(p90*100).toFixed(6)+'%';
  const meta=d.meta||{};const _ev=meta.events_in_window||0;document.getElementById('ev-count').textContent=_ev+(_ev===1?' event':' events');document.getElementById('snap-id').textContent=d.snapshot_id.slice(0,8);
  const pills=[...(meta.regions_active||[]).map(r=>r.replace(/_/g,' ')).slice(0,4),...(meta.top_actors||[]).slice(0,4).map(a=>a.replace(/_/g,' '))];
  document.getElementById('meta-row').innerHTML=pills.map((p,i)=>'<span class="mpill'+(i>3?' hi':'')+'">'+p+'</span>').join('');
  addLog('P(WWIII)='+(pA*100).toFixed(2)+'% Δ'+(dA>=0?'+':'')+(dA*100).toFixed(4)+'% · '+elev+' elevated · '+(meta.events_in_window||0)+' events',pA>=.05?'#E24B4A':pA>=.015?'#EF9F27':'#6060a0');
}
function applyTimeline(entries){
  if(entries.length>MAX_TL_POINTS)entries=entries.slice(-MAX_TL_POINTS);
  tlChart.data.labels=entries.map(e=>e.t);tlChart.data.datasets[0].data=entries.map(e=>e.p_annual);tlChart.data.datasets[1].data=entries.map(e=>e.p_30day);
  const n=entries.length;tlChart.data.datasets[2].data=new Array(n).fill(0.001);tlChart.data.datasets[3].data=new Array(n).fill(0.017);tlChart.update('none');
  if(entries.length>0){const peaks=entries.map(e=>e.p_annual);sessionPeak=Math.max(...peaks);sessionLow=Math.min(...peaks);}
}
function setLive(on){const dot=document.getElementById('live-dot');if(dot){dot.classList.toggle('connected',on);}}
function connect(){
  const wsProto=location.protocol==='https:'?'wss:':'ws:';const ws=new WebSocket(wsProto+'//'+location.host+BASE_PATH+'/ws');
  ws.onopen=()=>setLive(true);
  ws.onmessage=e=>{const msg=JSON.parse(e.data);if(msg.type==='snapshot'){applyData(msg.data);tlChart.data.labels.push(msg.data.computed_at);tlChart.data.datasets[0].data.push(msg.data.probabilities.annual);tlChart.data.datasets[1].data.push(msg.data.probabilities.thirty_day);tlChart.data.datasets[2].data.push(0.001);tlChart.data.datasets[3].data.push(0.017);if(tlChart.data.labels.length>MAX_TL_POINTS){tlChart.data.labels.shift();tlChart.data.datasets.forEach(ds=>ds.data.shift());}tlChart.update('none');}else if(msg.type==='timeline'){applyTimeline(msg.data);if(msg.data.length===0)fetchEpoch();}else if(msg.type==='articles'){
    // MERGE the periodic push (newest ~200) into the deep cache instead of
    // replacing it — otherwise the WS push would clobber the large initial
    // fetch every ~9s and the age brackets would lose all their history.
    const have=new Set(_artCache.map(a=>a.id||a.url));
    const fresh=(msg.data||[]).filter(a=>!have.has(a.id||a.url));
    if(fresh.length)_artCache=fresh.concat(_artCache);
    if(_artCache.length>8000)_artCache.length=8000;
    _artTotal=msg.total;
    if(currentTab==='articles')renderArticles(_artCache,_artTotal);
    updateTicker(_artCache.slice(0,40));
  }};
  ws.onclose=()=>{setLive(false);addLog('WebSocket disconnected — reconnecting...','#c0392b');setTimeout(connect,4000)};
  ws.onerror=()=>ws.close();
}
async function fetchEpoch(){
  try{
    const r=await fetch(BASE_PATH+'/api/epoch');
    const d=await r.json();
    if(d.entries&&d.entries.length>0)applyTimeline(d.entries);
  }catch(e){addLog('Epoch fetch failed: '+e.message,'#c0392b');}
}
connect();fetchArticles();
setInterval(()=>{if(currentTab==='sources')fetchSources();},10000);
setInterval(()=>{if(currentTab==='articles'&&_artCache.length===0)fetchArticles();},2000);
pollNuclear();setInterval(pollNuclear,15000);
setInterval(refreshSrcCount,30000);
</script>
<!-- ── Operator panel overlay ──────────────────────────────────────────── -->
<div class="op-overlay" id="op-overlay" onclick="toggleOperatorPanel()"></div>
<div class="op-drawer" id="op-drawer">
  <div class="op-drawer-header">
    <span class="op-drawer-title">Operator Panel</span>
    <button class="op-close" onclick="toggleOperatorPanel()">✕</button>
  </div>
  <div class="op-body">
    <!-- API Key -->
    <div class="op-section">
      <div class="op-section-title">API Key</div>
      <input class="op-key-input" id="op-key" type="password" placeholder="X-GCRM-Key" autocomplete="off"/>
      <div style="font-size:9px;color:var(--t4);margin-top:5px">Set in settings.yml → dashboard.operator_key</div>
    </div>
    <!-- Regime factors -->
    <div class="op-section">
      <div class="op-section-title">Regime Factors</div>
      <div class="op-product" id="op-product">Loading...</div>
      <div id="op-regime-list" style="margin-top:8px"></div>
      <button class="assert-btn" style="margin-top:10px;width:100%" onclick="fetchRegime()">↺ Refresh</button>
    </div>
    <!-- Manual event assertion -->
    <div class="op-section">
      <div class="op-section-title">Assert Event</div>
      <div class="assert-form">
        <input class="assert-input" id="op-assert-desc" placeholder="Event description (required)" type="text"/>
        <input class="assert-input" id="op-assert-activate" placeholder="Activate factor IDs (comma-separated)" type="text"/>
        <input class="assert-input" id="op-assert-deactivate" placeholder="Deactivate factor IDs (comma-separated)" type="text"/>
        <input class="assert-input" id="op-assert-severity" placeholder="Severity 0.0–1.0 (optional)" type="number" min="0" max="1" step="0.01"/>
        <button class="assert-btn primary" onclick="assertEvent()">▶ Assert Confirmed Event</button>
        <div id="op-assert-result" style="font-size:9px;color:var(--t4);font-family:monospace;min-height:16px"></div>
      </div>
    </div>
    <!-- Seismic alerts -->
    <div class="op-section">
      <div class="op-section-title">Seismic Alerts <button class="assert-btn" style="float:right;padding:2px 8px" onclick="fetchSeismic()">↺</button></div>
      <div id="op-seismic-list" style="margin-top:8px;clear:both"><span style="font-size:9px;color:var(--t4)">No alerts — monitoring</span></div>
    </div>
    <!-- Operator log -->
    <div class="op-section">
      <div class="op-section-title">Operator Log <button class="assert-btn" style="float:right;padding:2px 8px" onclick="fetchOpLog()">↺</button></div>
      <div id="op-log-list" style="margin-top:8px;clear:both;max-height:200px;overflow-y:auto"><span style="font-size:9px;color:var(--t4)">No entries</span></div>
    </div>
  </div>
</div>
<!-- ── Footer ──────────────────────────────────────────────────────────── -->
<div class="raithe-footer">
  <div class="raithe-footer-left">
    <span class="raithe-footer-name">RAITHE INDUSTRIES INC.</span>
    <span class="raithe-footer-copy">· © 2026</span>
  </div>
  <div class="raithe-footer-center">GCRM v1</div>
  <div class="raithe-footer-right"><a href="https://github.com/raithe-industries/GCRM/blob/main/docs/README.md" target="_blank" rel="noopener" style="color:var(--accent2);font-size:9px;font-family:monospace;text-decoration:none;letter-spacing:.1em;opacity:.7">DOCS</a></div>
</div>
<script>
// ── Operator panel ──────────────────────────────────────────────────────────
function toggleOperatorPanel(){
  const d=document.getElementById('op-drawer');
  const o=document.getElementById('op-overlay');
  const open=d.classList.toggle('open');
  o.classList.toggle('open',open);
  if(open){fetchRegime();fetchSeismic();}
}
function opKey(){return document.getElementById('op-key').value.trim();}
function opResult(msg,col){const el=document.getElementById('op-assert-result');el.textContent=msg;el.style.color=col||'var(--t4)';}

async function fetchRegime(){
  const key=opKey();if(!key){document.getElementById('op-product').textContent='Enter API key above';return;}
  try{
    const r=await fetch(BASE_PATH+'/api/regime',{headers:{'X-GCRM-Key':key}});
    const d=await r.json();
    if(d.error){document.getElementById('op-product').textContent='⚠ '+d.error;return;}
    document.getElementById('op-product').textContent=
      'Product: '+d.product.toFixed(4)+'× · Adjusted P₀: '+(d.adjusted_prior_pct||0).toFixed(4)+'%/yr · '+d.active_count+' active';
    const list=document.getElementById('op-regime-list');list.innerHTML='';
    (d.factors||[]).forEach(f=>{
      const row=document.createElement('div');row.className='regime-factor';
      row.innerHTML='<div class="rf-label">'+f.label+'</div>'
        +'<div class="rf-mult">×'+f.multiplier.toFixed(2)+'</div>'
        +'<button class="rf-toggle '+(f.active?'on':'off')+'" onclick="toggleFactor(\''+f.id+'\')">'
        +(f.active?'ON':'OFF')+'</button>';
      list.appendChild(row);
    });
  }catch(e){document.getElementById('op-product').textContent='Error: '+e.message;}
}

async function toggleFactor(id){
  const key=opKey();if(!key)return;
  try{
    const r=await fetch(BASE_PATH+'/api/regime/'+id+'/toggle',{method:'POST',headers:{'X-GCRM-Key':key}});
    const d=await r.json();
    if(d.error){alert(d.error);return;}
    fetchRegime();
  }catch(e){alert('Error: '+e.message);}
}

async function assertEvent(){
  const key=opKey();if(!key){opResult('API key required','#E24B4A');return;}
  const desc=document.getElementById('op-assert-desc').value.trim();
  if(!desc){opResult('Description required','#E24B4A');return;}
  const activateRaw=document.getElementById('op-assert-activate').value.trim();
  const deactivateRaw=document.getElementById('op-assert-deactivate').value.trim();
  const severityRaw=document.getElementById('op-assert-severity').value.trim();
  const body={description:desc};
  if(activateRaw)body.activate=activateRaw.split(',').map(s=>s.trim()).filter(Boolean);
  if(deactivateRaw)body.deactivate=deactivateRaw.split(',').map(s=>s.trim()).filter(Boolean);
  if(severityRaw)body.severity=parseFloat(severityRaw);
  try{
    const r=await fetch(BASE_PATH+'/api/operator/assert',{method:'POST',
      headers:{'X-GCRM-Key':key,'Content-Type':'application/json'},
      body:JSON.stringify(body)});
    const d=await r.json();
    if(d.error){opResult('⚠ '+d.error,'#E24B4A');return;}
    opResult('✓ Asserted: '+d.id.slice(0,8)+' · product now '+d.product.toFixed(4)+'×','#1D9E75');
    document.getElementById('op-assert-desc').value='';
    fetchRegime();fetchOpLog();
  }catch(e){opResult('Error: '+e.message,'#E24B4A');}
}

async function fetchSeismic(){
  const key=opKey();
  const url=key?BASE_PATH+'/api/operator/seismic':BASE_PATH+'/api/nuclear';
  const hdrs=key?{'X-GCRM-Key':key}:{};
  try{
    const r=await fetch(url,{headers:hdrs});
    const d=await r.json();
    const list=document.getElementById('op-seismic-list');
    const alerts=d.alerts||[];
    if(alerts.length===0){list.innerHTML='<span style="font-size:9px;color:var(--t4)">No alerts — all networks nominal</span>';return;}
    list.innerHTML=alerts.map(a=>`
      <div class="seismic-alert">
        <div class="sa-level">${a.level}</div>
        <div class="sa-desc">M${a.magnitude?.toFixed(1)} · ${a.depth_km?.toFixed(1)}km depth · ${a.place||a.nearest_site_name||'unknown'}</div>
        <div class="sa-conf">Confidence: ${Math.round((a.confidence||0)*100)}% · ${a.networks?.length||0} network(s) · ${a.actor?.replace(/_/g,' ')||''}</div>
        ${key?`<button class="assert-btn" style="margin-top:5px;font-size:8px" onclick="dismissSeismic('${a.id}')">Dismiss</button>`:''}
      </div>`).join('');
  }catch(e){document.getElementById('op-seismic-list').innerHTML='<span style="font-size:9px;color:#E24B4A">'+e.message+'</span>';}
}

async function dismissSeismic(id){
  const key=opKey();if(!key)return;
  await fetch(BASE_PATH+'/api/operator/seismic/'+encodeURIComponent(id)+'/dismiss',{method:'POST',headers:{'X-GCRM-Key':key}});
  fetchSeismic();
}

async function fetchOpLog(){
  const key=opKey();if(!key)return;
  try{
    const r=await fetch(BASE_PATH+'/api/operator/log',{headers:{'X-GCRM-Key':key}});
    const d=await r.json();
    const list=document.getElementById('op-log-list');
    const entries=d.entries||[];
    if(entries.length===0){list.innerHTML='<span style="font-size:9px;color:var(--t4)">No log entries</span>';return;}
    list.innerHTML=entries.slice(0,50).map(e=>{
      const ts=toETShort(new Date(e.ts));
      const action=e.action||'';
      const desc=e.description||e.id||'';
      return `<div class="op-log-entry">${ts} · ${action} · ${desc}</div>`;
    }).join('');
  }catch(e){}
}
</script>
</body>
</html>"#;

// ── Methodology / whitepaper page ─────────────────────────────────────────────
// Deep, accurate explanation of the entire risk model. Self-contained, dark
// theme matching the dashboard. Every constant and formula below mirrors the
// live engine in bayesian.rs — if the engine changes, update this page.
// `r##"..."##` delimiter so in-page `href="#anchor"` links are safe.

const METHODOLOGY_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width,initial-scale=1.0">
<title>GCRM — Methodology &amp; Mathematics</title>
<link rel="icon" type="image/png" href="{{BASE_PATH}}/logo-a.png">
<style>
*{box-sizing:border-box;margin:0;padding:0}
:root{--bg:#05080c;--bg2:#0a0f16;--bg3:#0e141e;--border:#1a2333;--t1:#fff;--t2:#c2d2e2;--t3:#7c98b4;--t4:#4a5c70;--purple:#8e86e8;--green:#1D9E75;--amber:#EF9F27;--red:#E24B4A;--code:#0d1320}
html{scroll-behavior:smooth}
body{font-family:system-ui,-apple-system,sans-serif;background:var(--bg);color:var(--t2);line-height:1.65;font-size:15px}
.wrap{max-width:1180px;margin:0 auto;display:grid;grid-template-columns:240px 1fr;gap:0}
.toc{position:sticky;top:0;align-self:start;height:100vh;overflow-y:auto;padding:28px 20px;border-right:1px solid var(--border);background:var(--bg2)}
.toc-brand{font-size:13px;font-weight:800;letter-spacing:.12em;color:var(--t1);text-transform:uppercase;margin-bottom:4px}
.toc-sub{font-size:10px;color:var(--t4);letter-spacing:.08em;margin-bottom:20px}
.toc a{display:block;font-size:12.5px;color:var(--t3);text-decoration:none;padding:4px 0;border-left:2px solid transparent;padding-left:10px;transition:all .15s}
.toc a:hover{color:var(--t1);border-left-color:var(--purple)}
.toc-back{display:inline-block;margin-top:22px;font-size:11px;color:var(--purple);text-decoration:none;font-weight:600}
.toc-back:hover{text-decoration:underline}
main{padding:48px 56px;min-width:0}
h1{font-size:30px;color:var(--t1);font-weight:800;letter-spacing:-.01em;margin-bottom:6px}
.lede{font-size:16px;color:var(--t3);margin-bottom:8px;max-width:70ch}
.stamp{font-size:11px;color:var(--t4);font-family:monospace;margin-bottom:40px}
section{margin-bottom:46px;scroll-margin-top:24px}
h2{font-size:21px;color:var(--t1);font-weight:700;margin-bottom:14px;padding-bottom:8px;border-bottom:1px solid var(--border)}
h2 .num{color:var(--purple);font-family:monospace;font-size:15px;margin-right:10px}
h3{font-size:15px;color:var(--t1);font-weight:600;margin:22px 0 8px}
p{margin-bottom:13px;max-width:74ch}
ul,ol{margin:0 0 13px 22px;max-width:72ch}
li{margin-bottom:6px}
strong{color:var(--t1);font-weight:600}
code{font-family:'SF Mono',Menlo,monospace;font-size:13px;background:var(--code);color:#b9c4ff;padding:1px 6px;border-radius:3px}
.eq{background:var(--code);border:1px solid var(--border);border-left:3px solid var(--purple);border-radius:5px;padding:16px 20px;margin:16px 0;font-family:'SF Mono',Menlo,monospace;font-size:14px;color:var(--t1);overflow-x:auto;line-height:1.9}
.eq .c{color:var(--t4)}
.eq .v{color:#7fd1b9}
.note{background:rgba(142,134,232,.06);border:1px solid rgba(142,134,232,.25);border-radius:5px;padding:13px 18px;margin:16px 0;font-size:14px;color:var(--t2)}
.note b{color:var(--purple)}
.warn{background:rgba(226,75,74,.06);border:1px solid rgba(226,75,74,.28);border-radius:5px;padding:13px 18px;margin:16px 0;font-size:14px}
.warn b{color:var(--red)}
table{width:100%;border-collapse:collapse;margin:16px 0;font-size:13.5px}
th,td{text-align:left;padding:8px 12px;border-bottom:1px solid var(--border)}
th{color:var(--t3);font-weight:600;font-size:11px;letter-spacing:.06em;text-transform:uppercase;background:var(--bg2)}
td{color:var(--t2)}
td.mono,th.mono{font-family:monospace}
td.r,th.r{text-align:right}
.pill{display:inline-block;font-size:11px;font-family:monospace;padding:1px 7px;border-radius:10px;background:var(--bg3);border:1px solid var(--border)}
.g{color:var(--green)}.a{color:var(--amber)}.r2{color:var(--red)}.p{color:var(--purple)}
footer{border-top:1px solid var(--border);margin-top:50px;padding-top:20px;font-size:11px;color:var(--t4);font-family:monospace}
@media(max-width:880px){.wrap{grid-template-columns:1fr}.toc{position:static;height:auto;border-right:none;border-bottom:1px solid var(--border)}main{padding:32px 22px}}
</style>
</head>
<body>
<div class="wrap">
<nav class="toc">
  <div class="toc-brand">GCRM</div>
  <div class="toc-sub">METHODOLOGY &amp; MATHEMATICS</div>
  <a href="#what">1 · What the number is</a>
  <a href="#pipeline">2 · System pipeline</a>
  <a href="#prior">3 · The historical prior</a>
  <a href="#regime">4 · Regime multiplier</a>
  <a href="#nlp">5 · Domain scoring</a>
  <a href="#decay">6 · Recency decay</a>
  <a href="#cooc">7 · Co-occurrence boost</a>
  <a href="#likelihood">8 · The likelihood L</a>
  <a href="#engine">9 · The risk model</a>
  <a href="#gauge">10 · Reading the gauge</a>
  <a href="#confidence">11 · Confidence</a>
  <a href="#alerts">12 · Alerts &amp; calibration</a>
  <a href="#nuclear">13 · Nuclear detector</a>
  <a href="#limits">14 · What it is NOT</a>
  <a href="{{BASE_PATH}}/" class="toc-back">← Back to live dashboard</a>
</nav>
<main>
<h1>How GCRM Computes Risk</h1>
<p class="lede">A complete, honest walk-through of the mathematics behind the annual P(WWIII) figure — from a single news headline to the number on the dial. No black boxes.</p>
<div class="stamp">RAiTHE INDUSTRIES INCORPORATED · Global Conflict Risk Monitor · live engine reference</div>

<section id="what">
<h2><span class="num">01</span>What the number actually is</h2>
<p>The headline figure is a <strong>calibrated annual probability estimate</strong>: given everything in the live news signal right now, roughly how likely is it that a world-war-scale conflict begins within the next 12 months.</p>
<p>It is expressed as a percentage per year. The historical base rate — two world wars across roughly 2,000 years of recorded great-power history — sits near <strong>0.1% per year</strong>. Everything GCRM does is reason, transparently, about how far above or below that base rate today's conditions push us.</p>
<div class="note"><b>Read it as:</b> not a prediction that war <em>will</em> happen, but a continuously-updated thermometer. 0.5% is a quiet world. 5% is an acute crisis. 40% is the Cuban Missile Crisis. The point is the <em>movement</em> and the <em>structure</em> behind it.</div>
</section>

<section id="pipeline">
<h2><span class="num">02</span>The system pipeline</h2>
<p>Every figure is the end of a five-stage concurrent pipeline. Each stage is an independent task; data flows one way through bounded channels.</p>
<div class="eq">
Ingestor <span class="c">(42 RSS feeds + GNews + GDELT)</span><br>
&nbsp;&nbsp;→ NLP processor <span class="c">(pure-Rust keyword scoring)</span> + LLM enricher <span class="c">(local Ollama)</span><br>
&nbsp;&nbsp;→ Aggregator <span class="c">(time-windowed event buffer, corroboration)</span><br>
&nbsp;&nbsp;→ Bayesian risk engine <span class="c">(this document)</span><br>
&nbsp;&nbsp;→ WebSocket broadcast → your screen
</div>
<p>Stages 1–3 decide <em>what is happening in the world</em> and how much to trust each signal. Stage 4 — the risk engine — turns that into a probability. This page is mostly about stage 4, with enough of stages 1–3 to make the inputs legible.</p>
</section>

<section id="prior">
<h2><span class="num">03</span>The historical prior <span class="pill p">P₀</span></h2>
<p>We anchor to history before reading a single headline. Two world wars in the modern era, over the span of recorded great-power conflict, give a naive annual base rate:</p>
<div class="eq">P₀ &nbsp;=&nbsp; 2 / 2026 &nbsp;=&nbsp; <span class="v">0.000987</span> &nbsp;<span class="c">≈ 0.0987% per year</span></div>
<p>This is deliberately crude and deliberately <em>low</em>. It is the floor the evidence has to argue its way up from. Using the current year as the denominator is a transparent convention, not a claim of precision — its job is to put the prior in the right order of magnitude.</p>
</section>

<section id="regime">
<h2><span class="num">04</span>The regime multiplier <span class="pill p">×</span></h2>
<p>The naive prior assumes an <em>average</em> century. Today is not average. The <strong>regime multiplier</strong> answers one precise question: <em>how dangerous is the era itself, before any of today's headlines are read?</em> It scales the historical prior P₀ up — or down — for slow-moving <strong>structural</strong> conditions: the geopolitical backdrop that holds steady over months to years, not the daily news cycle.</p>
<p>Each structural condition is a single operator-maintained factor carrying its own multiplier. The active factors are <strong>multiplied together, not added</strong>, because each is treated as a conditionally-independent amplifier of the same underlying danger; a factor below 1.0 is a genuine <em>reducer</em>. Their product multiplies P₀ to give the <strong>adjusted prior</strong> P₀,adj — the floor that live evidence then has to argue its way up from.</p>
<table>
<tr><th>Active structural factor</th><th class="r mono">×</th></tr>
<tr><td>Active US kinetic war (Iran theatre)</td><td class="r mono">1.40</td></tr>
<tr><td>Global arms-control framework collapsed</td><td class="r mono">1.40</td></tr>
<tr><td>Conventional war in Europe (Ukraine, yr 5)</td><td class="r mono">1.40</td></tr>
<tr><td>Taiwan / South China Sea competition</td><td class="r mono">1.30</td></tr>
<tr><td>DPRK nuclear status irreversible</td><td class="r mono">1.20</td></tr>
<tr><td>Russian hybrid warfare vs NATO</td><td class="r mono">1.20</td></tr>
<tr><td>US institutional norm erosion</td><td class="r mono">1.20</td></tr>
<tr><td>Russia nuclear doctrine → compellence</td><td class="r mono">1.15</td></tr>
<tr><td>Cyber / information warfare normalized</td><td class="r mono">1.10</td></tr>
<tr><td>Nuclear deterrence (MAD) intact <span class="g">— reducer</span></td><td class="r mono g">0.70</td></tr>
</table>
<div class="eq">regime &nbsp;=&nbsp; 1.4·1.4·1.4·1.3·1.2·1.2·1.2·1.15·1.1·0.7 &nbsp;≈&nbsp; <span class="v">5.46×</span><br>
P₀<span class="c">,adj</span> &nbsp;=&nbsp; P₀ × regime &nbsp;=&nbsp; 0.000987 × 5.46 &nbsp;≈&nbsp; <span class="v">0.539% / yr</span></div>
<p><strong>What the multiplier is — and is not.</strong> It sets the <em>starting point</em>, not the answer. At 5.46× the era alone lifts baseline annual risk to ≈0.54%/yr — about 5.5× the long-run historical average — but it is <em>never</em> multiplied into the final probability. Live event evidence is folded in separately, on the log-odds scale (see <a href="#engine">§09, the risk model</a>), so the regime only moves where the number <em>starts</em>, not how far the day's news pushes it. Note the <span class="g">0.70 deterrence reducer</span>: mutually-assured destruction genuinely <em>lowers</em> the probability of all-out war, and the model honours that. Factors are toggled only when a structural condition durably changes — a held ceasefire, a ratified treaty, a confirmed nuclear test. They never move on a single news cycle.</p>
<div class="note"><b>Standby factors</b> sit dormant at the operator console (e.g. <code>nuclear_weapon_detonated ×2.5</code>, <code>russia_nato_kinetic ×1.6</code>) and switch on only if that threshold event is confirmed.</div>
</section>

<section id="nlp">
<h2><span class="num">05</span>From headline to domain score</h2>
<p>Each article is scored against <strong>eight risk domains</strong>. A single event contributes a per-domain signal in <code>[0,1]</code> built from an explicit, bounded budget. The <em>domain-specific</em> keyword evidence is the primary driver, so each domain reflects what its own slice of the coverage actually says rather than the event's raw severity:</p>
<div class="eq">intensity &nbsp;=&nbsp; <span class="v">0.5</span>·severity &nbsp;+&nbsp; <span class="v">0.5</span>·escalation<br>
base &nbsp;=&nbsp; nlp_keyword · ( <span class="v">0.55</span> + <span class="v">0.45</span>·intensity ) &nbsp;+&nbsp; gp_bonus<span class="c">(≤0.12, great-power domain only)</span><br>
signal &nbsp;=&nbsp; clamp( base × (1 − 0.15·sentiment) , 0, 1 )</div>
<ul>
<li><strong>nlp_keyword</strong> — noisy-OR keyword/LLM evidence strength for <em>this</em> domain (definitive terms beat ambient ones). It is the <em>spine</em>: the whole signal is proportional to it, so two domains tagged on the same story diverge by their own evidence instead of collapsing together.</li>
<li><strong>intensity</strong> — a shared story-level factor (severity + escalation) that <em>multiplies</em> a domain's own evidence rather than adding to it. A severe story amplifies every domain it touches, but only in proportion to each domain's keyword strength — it can't, by itself, lift a weakly-evidenced domain to match a strongly-evidenced one. <strong>severity</strong> = event-type seriousness, casualties, nuclear/WMD indicators; <strong>escalation</strong> = density of escalatory vs conciliatory language.</li>
<li><strong>gp_bonus</strong> — +0.12 added only to the <strong>great-power conflict</strong> domain when a great power (US, Russia, China, NATO) is directly involved. It no longer leaks into every domain, so e.g. diplomatic breakdown and great-power conflict are now independent readings.</li>
<li><strong>sentiment</strong> — hostile tone amplifies up to +15%, conciliatory tone damps up to −15%. Tone refines, never dominates.</li>
</ul>
<p>Each signal is then weighted by <strong>credibility</strong> (source tier + corroboration from independent outlets, capped) and <strong>recency</strong> (next section) before contributing to its domain. The optional local LLM runs a second pass per article and is merged in at the max of the two scores, discounted 10% so a definitive keyword hit always outranks an LLM estimate.</p>
<h3>Domain strategic weights</h3>
<p>Not every domain pushes toward world war equally. The engine assigns each a relative weight:</p>
<table>
<tr><th>Domain</th><th class="r mono">weight</th><th class="r mono">half-life</th></tr>
<tr><td>Nuclear posture</td><td class="r mono r2">3.0</td><td class="r mono">72 h</td></tr>
<tr><td>WMD / mass casualty</td><td class="r mono r2">2.8</td><td class="r mono">~4 yr</td></tr>
<tr><td>Great-power conflict</td><td class="r mono">2.0</td><td class="r mono">48 h</td></tr>
<tr><td>Alliance activation</td><td class="r mono">1.6</td><td class="r mono">72 h</td></tr>
<tr><td>Military escalation</td><td class="r mono">1.5</td><td class="r mono">24 h</td></tr>
<tr><td>Economic warfare</td><td class="r mono">1.4</td><td class="r mono">96 h</td></tr>
<tr><td>Diplomatic breakdown</td><td class="r mono">1.1</td><td class="r mono">48 h</td></tr>
<tr><td>Cyber / info ops</td><td class="r mono">0.9</td><td class="r mono">24 h</td></tr>
</table>
<p>Nuclear posture carries the highest weight because it is the most direct mechanism by which a regional crisis becomes a global one.</p>
</section>

<section id="decay">
<h2><span class="num">06</span>Recency decay</h2>
<p>News is perishable, but not all news perishes at the same rate. Each domain has an exponential <strong>half-life</strong>: the time after which an event's weight halves.</p>
<div class="eq">recency(age) &nbsp;=&nbsp; exp( −ln2 · age / half_life )</div>
<p>A street battle (military, 24 h) is stale within days. A sanctions regime (economic, 96 h) lingers. A nuclear posture shift (72 h) persists for most of a week. WMD use is given a ~4-year half-life on purpose: it redefines the strategic situation for a whole presidential term, so it stays on the books across the entire analysis window. Events past the 4-year horizon drop to zero weight entirely.</p>
</section>

<section id="cooc">
<h2><span class="num">07</span>Co-occurrence — why simultaneity matters</h2>
<p>Three crises at once is far more dangerous than three crises spread across a decade. Simultaneity is how regional wars become systemic. GCRM models this with a <strong>co-occurrence boost</strong> driven by how many domains are elevated at the same time:</p>
<table>
<tr><th class="r mono">domains elevated</th><th class="r mono">boost ×</th></tr>
<tr><td class="r mono">1</td><td class="r mono">1.0</td></tr>
<tr><td class="r mono">2</td><td class="r mono">1.3</td></tr>
<tr><td class="r mono">3</td><td class="r mono">2.0</td></tr>
<tr><td class="r mono">4</td><td class="r mono">3.5</td></tr>
<tr><td class="r mono">5</td><td class="r mono r2">5.0</td></tr>
<tr><td class="r mono">6 → 8</td><td class="r mono r2">5.7 → 7.0</td></tr>
</table>
<p>Between these anchor points the boost interpolates linearly, and the elevation count itself is <em>soft</em> — a domain near the threshold contributes a fractional amount via a smoothstep ramp, so the boost moves continuously instead of jumping when a score crosses the line. Eight domains lit at once is an unprecedented, near-certain systemic-war signature, and the curve reflects that.</p>
</section>

<section id="likelihood">
<h2><span class="num">08</span>Assembling the likelihood <span class="pill p">L</span></h2>
<p>The eight weighted domain scores collapse into one evidence term. We take the weighted sum, normalise by the maximum possible, and apply the co-occurrence boost:</p>
<div class="eq">L &nbsp;=&nbsp; ( Σ domain_score·weight / Σ weight ) &nbsp;×&nbsp; co_occurrence_boost</div>
<p>L is roughly <code>0</code> in a quiet world and can reach <code>~5+</code> when every domain is lit and corroborated simultaneously. It is the single number that says <em>"here is how much today's news argues for danger."</em></p>
</section>

<section id="engine">
<h2><span class="num">09</span>The risk model <span class="pill p">log-odds</span></h2>
<p>Now we fold the evidence into the prior. GCRM does this on the <strong>log-odds (logit) scale</strong> — the standard, well-behaved way to combine a prior probability with new evidence:</p>
<div class="eq">P &nbsp;=&nbsp; sigmoid( &nbsp;logit(P₀<span class="c">,adj</span>) &nbsp;+&nbsp; β · L&nbsp; ) &nbsp;&nbsp;<span class="c">clamped to [0, 0.85]</span><br><br>
<span class="c">where</span> &nbsp;logit(p) = ln( p / (1−p) ) &nbsp;&nbsp;and&nbsp;&nbsp; sigmoid(x) = 1 / (1 + e<span class="c">⁻ˣ</span>) &nbsp;&nbsp;and&nbsp;&nbsp; β = <span class="v">2.0</span></div>
<p>Three properties make this the right shape:</p>
<ul>
<li><strong>L = 0 returns the prior exactly.</strong> A quiet world reads precisely <code>0.539%</code> — no evidence, no inflation.</li>
<li><strong>It is smooth and monotonic.</strong> More evidence always means more risk, with no discontinuities.</li>
<li><strong>It saturates gracefully.</strong> Strong multi-domain signals climb an S-curve toward the ceiling instead of being capped near 15% — so a real crisis can express genuinely high risk.</li>
</ul>
<h3>Worked example — at today's regime (P₀,adj ≈ 0.539%)</h3>
<table>
<tr><th class="mono">L</th><th class="r mono">→ P(WWIII)/yr</th><th>interpretation</th></tr>
<tr><td class="mono">0.0</td><td class="r mono g">0.54%</td><td>quiet — prior only</td></tr>
<tr><td class="mono">0.5</td><td class="r mono g">1.5%</td><td>low-level activity</td></tr>
<tr><td class="mono">1.0</td><td class="r mono a">3.9%</td><td>several domains warm</td></tr>
<tr><td class="mono">1.5</td><td class="r mono a">9.8%</td><td>acute crisis</td></tr>
<tr><td class="mono">2.0</td><td class="r mono r2">22.8%</td><td>Ukraine-2022 territory</td></tr>
<tr><td class="mono">2.6</td><td class="r mono r2">49.6%</td><td>Cuba-1962 territory</td></tr>
<tr><td class="mono">≥4</td><td class="r mono r2">→ 85%</td><td>engineering ceiling</td></tr>
</table>
<div class="warn"><b>The 85% ceiling is not a probability claim.</b> It is a deliberate cap of epistemic humility. The model has no access to ground truth and must never emit near-certainty. Where the ceiling should sit for an extreme confirmed event is a human design decision, not something the model derives on its own.</div>
<div class="note"><b>Honest naming:</b> this is a <em>calibrated risk index</em>, not a formal Bayesian posterior from a generative model. "P" here is the output probability — combining a prior and evidence additively in log-odds — not a posterior distribution. The system is built to inform, not to overclaim.</div>
</section>

<section id="gauge">
<h2><span class="num">10</span>Reading the gauge — why it is logarithmic</h2>
<p>Real risk spans more than two orders of magnitude: <code>0.1%</code> baseline to <code>~50%</code> at the Cuban Missile Crisis. A linear dial maxing at 5% would pin <em>everything</em> serious — Ukraine, Cuba, a nuclear alert — at the same full-scale reading, erasing the distinctions that matter most.</p>
<p>So the dial is <strong>logarithmic</strong>, mapping <code>0.1% → 50%</code> across the arc:</p>
<div class="eq">fraction &nbsp;=&nbsp; log₁₀(P / 0.1%) / log₁₀(50% / 0.1%)</div>
<p>The tick marks (<span class="mono">.1 · 1 · 5 · 50</span>) are spaced by this log law, so each step is a <em>tenfold-ish</em> jump in danger. The coloured bands: <span class="g">green below 1.5%</span>, <span class="a">amber 1.5–5%</span>, <span class="r2">red above 5%</span>. This way a move from 4% to 9% is visible and dramatic, exactly as it should be — instead of both sitting jammed against the same red stop.</p>
</section>

<section id="confidence">
<h2><span class="num">11</span>Confidence — how much to trust the reading</h2>
<p>The probability is only as good as the evidence underneath it. A separate <strong>confidence</strong> score blends three independent signals:</p>
<div class="eq">confidence &nbsp;=&nbsp; tier_quality·<span class="v">0.5</span> &nbsp;+&nbsp; event_volume·<span class="v">0.3</span> &nbsp;+&nbsp; source_diversity·<span class="v">0.2</span></div>
<ul>
<li><strong>tier_quality</strong> — are signals coming from Tier-1 wires or unverified aggregators?</li>
<li><strong>event_volume</strong> — a handful of articles or hundreds? (log-scaled)</li>
<li><strong>source_diversity</strong> — how many independent outlets corroborate?</li>
</ul>
<p>Low confidence with high risk means "something may be happening but the picture is thin" — a fundamentally different state from a well-corroborated high reading, and the dashboard shows both.</p>
</section>

<section id="alerts">
<h2><span class="num">12</span>Alert thresholds &amp; calibration targets</h2>
<p>Banners fire at operator-configured thresholds in <code>settings.yml</code>. The model is calibrated so that historically-recognisable situations land in sensible bands:</p>
<table>
<tr><th>Situation</th><th class="r">annual P(WWIII)</th></tr>
<tr><td>Quiet period (1–2 domains, low signal)</td><td class="r mono g">0.5 – 1.5%</td></tr>
<tr><td>Current world, 2026 (4–5 domains, moderate)</td><td class="r mono a">4 – 8%</td></tr>
<tr><td>Acute crisis (5–6 domains, high signal)</td><td class="r mono r2">8 – 15%</td></tr>
<tr><td>Ukraine, Feb 2022 equivalent</td><td class="r mono r2">~8 – 12%</td></tr>
<tr><td>Cuban Missile Crisis, Oct 1962 equivalent</td><td class="r mono r2">~30 – 50%</td></tr>
</table>
<p>These targets are the yardstick the parameters (β, the boost anchors, the domain weights) are tuned against. Precise crisis calibration is an ongoing process of back-testing against historical event replays.</p>
</section>

<section id="nuclear">
<h2><span class="num">13</span>The nuclear detection subsystem</h2>
<p>Running alongside the risk engine is a dedicated detector for the one event that would override everything else: an underground nuclear test.</p>
<ul>
<li><strong>Seismic monitor</strong> — polls FDSN-standard seismological APIs for anomalies near the 10 known test sites (Punggye-ri, Novaya Zemlya, Lop Nur, Nevada, and others).</li>
<li><strong>CTBTO monitor</strong> — watches official public statements.</li>
<li><strong>Nuclear news monitor</strong> — flags headline spikes around nuclear keywords.</li>
<li><strong>Alert fusion</strong> — combines the three into one confidence-weighted signal.</li>
</ul>
<div class="warn"><b>Every alert is labelled "SEISMIC ANOMALY," never "nuclear test,"</b> until officially confirmed. The system reports what it can measure, not what it cannot.</div>
</section>

<section id="limits">
<h2><span class="num">14</span>What GCRM is — and is not</h2>
<p><strong>It is</strong> a transparent, continuously-updated risk index that turns the firehose of global news into one defensible, quantified figure with every step of its reasoning open to inspection — this page being the proof.</p>
<p><strong>It is not</strong> a crystal ball. It does not forecast specific events, does not claim certainty, and its core computation uses no generative AI for the probability itself. It reads only openly-available sources. The number is a thermometer for structural and acute conditions — most valuable in its <em>movement</em> and the <em>structure</em> it exposes, not as a literal oracle.</p>
<div class="note"><b>Design philosophy:</b> every ceiling, threshold, and weight is a stated, auditable human decision. The model is built to be honest about the limits of what it can know — and to make those limits visible rather than hide them.</div>
</section>

<footer>
© 2026 RAiTHE INDUSTRIES INCORPORATED · Global Conflict Risk Monitor · This page mirrors the live engine. Constants shown match the production build.<br>
<a href="{{BASE_PATH}}/" style="color:var(--purple);text-decoration:none">← Return to live dashboard</a>
</footer>
</main>
</div>
</body>
</html>"##;

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
    fn dashboard_html_has_all_api_routes() {
        assert!(DASHBOARD_HTML.contains("/api/articles"));
        assert!(DASHBOARD_HTML.contains("/api/sources"));
        assert!(DASHBOARD_HTML.contains("/api/nuclear"));
        assert!(DASHBOARD_HTML.contains("/api/operator/assert"));
        assert!(DASHBOARD_HTML.contains("/api/operator/log"));
        assert!(DASHBOARD_HTML.contains("/api/operator/seismic"));
        assert!(DASHBOARD_HTML.contains("/api/regime"));
        assert!(DASHBOARD_HTML.contains("/api/epoch"));
    }

    #[test]
    fn dashboard_html_has_raithe_branding() {
        assert!(DASHBOARD_HTML.contains("RAITHE INDUSTRIES INC."));
        assert!(DASHBOARD_HTML.contains("raithe-footer"));
    }

    #[test]
    fn dashboard_html_has_operator_panel() {
        assert!(DASHBOARD_HTML.contains("op-drawer"));
        assert!(DASHBOARD_HTML.contains("toggleOperatorPanel"));
        assert!(DASHBOARD_HTML.contains("assertEvent"));
        assert!(DASHBOARD_HTML.contains("toggleFactor"));
    }

    #[test]
    fn methodology_html_is_substantial_and_complete() {
        // Page must exist and cover every section the engine actually implements.
        assert!(METHODOLOGY_HTML.len() > 8000, "methodology page should be a real whitepaper");
        for anchor in ["#prior", "#regime", "#nlp", "#decay", "#cooc",
                       "#likelihood", "#engine", "#gauge", "#confidence", "#nuclear"] {
            assert!(METHODOLOGY_HTML.contains(anchor), "methodology missing section {anchor}");
        }
        // Key constants from bayesian.rs must be documented accurately.
        assert!(METHODOLOGY_HTML.contains("2 / 2026"), "missing historical anchor");
        assert!(METHODOLOGY_HTML.contains("0.85"),     "missing engineering ceiling");
        assert!(METHODOLOGY_HTML.contains("sigmoid"),  "missing logistic model");
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
    fn dashboard_html_has_chart_js() {
        assert!(DASHBOARD_HTML.contains("chart.js"));
    }

    #[test]
    fn dashboard_html_has_scenario_buttons() {
        for scen in &["live", "hot", "cold", "nuke", "epstein", "religious"] {
            assert!(
                DASHBOARD_HTML.contains(scen),
                "Dashboard HTML missing scenario: {scen}"
            );
        }
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
