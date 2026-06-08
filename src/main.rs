// ------------------------------------------------------------
// RAiTHE INDUSTRIES INCORPORATED
// Copyright (c) 2026 All Rights Reserved.
//
// This file is part of a proprietary system. Unauthorized use,
// reproduction, or distribution is strictly prohibited.
// ------------------------------------------------------------

// src/main.rs — GCRM entry point (Rust)
//
// Pipeline:
//   Ingestor (RSS/GNews/GDELT)
//     → mpsc::channel<RawArticle>         (raw_tx / raw_rx)
//     → NlpSidecar (pure Rust NlpProcessor — no external dependencies)
//     → mpsc::channel<GeopoliticalEvent>  (event_tx / event_rx)
//     → Aggregator (Bayesian engine)
//     → mpsc::channel<RiskSnapshot>       (snap_tx / snap_rx)
//     → Server broadcast loop → WebSocket clients
//
// H-01: Startup rejects known-insecure operator keys. If the configured key
//        matches any value in INSECURE_KEYS, operator endpoints are disabled
//        (key replaced with empty string — api.rs returns 403 on all operator
//        routes) and an ERROR-level banner is logged. The system still starts
//        for read-only monitoring; the operator API is locked out until a
//        real key is configured in settings.yml.

mod aggregator;
mod api;
#[cfg(test)]
mod backtest;
mod backfill;
mod bayesian;
mod brief;
mod detector;
mod indicators;
mod ingestor;
mod llm_enricher;
mod models;
mod nlp_sidecar;
mod processor;
mod server;
mod theater;

use std::path::Path;
use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use aggregator::{Aggregator, AppState, load_epoch, load_articles, load_events};
use ingestor::Ingestor;
use models::{
    AlertSettings, DashboardSettings, IngestionSettings, LlmSettings,
    RawArticle, RegimeFactor, Settings,
};
use api::OperatorState;
use detector::{SeismicMonitor, CtbtoMonitor, NuclearNewsMonitor};
use nlp_sidecar::NlpSidecar;
use server::{broadcast_snapshots, serve, ServerState};

// ── Channel capacities ────────────────────────────────────────────────────────

const RAW_CAP:   usize = 5000;
const EVENT_CAP: usize = 5000;
const SNAP_CAP:  usize = 100;

// ── Insecure operator key blocklist (H-01) ──────────────────────────────────
//
// Any operator_key matching one of these values (case-insensitive) will be
// rejected at startup. The operator API will be disabled (key set to empty
// string, which api.rs already handles as 403 Forbidden on all operator
// routes). The system continues running for read-only monitoring.
//
// This prevents deployment with placeholder keys that could be guessed by
// anyone who has read the source code or default config.

const INSECURE_KEYS: &[&str] = &[
    "change_me_before_deploy",
    "changeme",
    "change_me",
    "password",
    "admin",
    "operator",
    "secret",
    "test",
    "testingmeasure1",
    "default",
    "example",
    "placeholder",
];

/// Check whether the operator key is known-insecure.
/// Returns true if the key is empty or matches any entry in INSECURE_KEYS
/// (case-insensitive comparison).
fn is_insecure_key(key: &str) -> bool {
    if key.is_empty() {
        return true;
    }
    let lower = key.to_lowercase();
    INSECURE_KEYS.iter().any(|&blocked| lower == blocked)
}

// ── Settings loader ───────────────────────────────────────────────────────────

fn load_settings() -> Settings {
    let candidates = ["settings.yml", "config/settings.yml", "config/settings.example.yml"];
    for path in &candidates {
        if Path::new(path).exists() {
            if path.contains("example") {
                warn!("Using example config — no live API keys.");
            }
            match std::fs::read_to_string(path) {
                Ok(text) => match serde_yaml::from_str::<Settings>(&text) {
                    Ok(s)  => { info!("Config loaded from {path}"); return s; }
                    Err(e) => { warn!("Config parse error in {path}: {e} — using defaults"); }
                },
                Err(e) => warn!("Could not read {path}: {e}"),
            }
        }
    }
    warn!("No config file found — using built-in defaults. Copy settings.yml into the working directory.");
    default_settings()
}

/// Hard-coded defaults matching settings.yml v6 exactly.
/// Allows the binary to run without any config file present.
fn default_settings() -> Settings {
    Settings {
        regime_factors: vec![
            RegimeFactor { id: "active_superpower_war".into(),            label: "Active US kinetic war (Operation Epic Fury — US/Israel vs Iran)".into(),  multiplier: 1.4,  active: true  },
            RegimeFactor { id: "arms_control_dead".into(),                label: "Global arms control / nonproliferation framework collapsed".into(),        multiplier: 1.4,  active: true  },
            RegimeFactor { id: "dprk_nuclear_irreversible".into(),        label: "DPRK nuclear status constitutionally irreversible".into(),                 multiplier: 1.2,  active: true  },
            RegimeFactor { id: "deterrence_structural_intact".into(),     label: "Nuclear deterrence (MAD) structurally intact — risk reducer".into(),       multiplier: 0.7,  active: true  },
            RegimeFactor { id: "war_in_europe_year5".into(),              label: "Active conventional war in Europe — Ukraine, year 5".into(),               multiplier: 1.4,  active: true  },
            RegimeFactor { id: "russia_hybrid_nato".into(),               label: "Systematic Russian hybrid warfare campaign against NATO states".into(),     multiplier: 1.2,  active: true  },
            RegimeFactor { id: "russia_nuclear_doctrine_compellence".into(), label: "Russia shifting nuclear doctrine toward compellence".into(),             multiplier: 1.15, active: true  },
            RegimeFactor { id: "taiwan_south_china_sea_rising".into(),    label: "Taiwan Strait / South China Sea strategic competition intensifying".into(), multiplier: 1.3,  active: true  },
            RegimeFactor { id: "us_institutional_norm_erosion".into(),    label: "US institutional norm erosion — unpredictable superpower behavior".into(),  multiplier: 1.2,  active: true  },
            RegimeFactor { id: "cyber_info_warfare".into(),               label: "State-sponsored cyber and information warfare normalized".into(),           multiplier: 1.1,  active: true  },
            // ── Inactive standby factors ──
            RegimeFactor { id: "dprk_7th_nuclear_test".into(),            label: "DPRK conducts 7th nuclear test".into(),                                    multiplier: 1.35, active: false },
            RegimeFactor { id: "russia_nato_kinetic".into(),              label: "Russia conducts kinetic attack on NATO member state territory".into(),      multiplier: 1.6,  active: false },
            RegimeFactor { id: "nuclear_weapon_detonated".into(),         label: "Nuclear weapon detonated (any state, any yield)".into(),                   multiplier: 2.5,  active: false },
            RegimeFactor { id: "ceasefire_iran_durable".into(),           label: "Durable ceasefire achieved in Iran war (30+ days holding)".into(),          multiplier: 0.7,  active: false },
        ],
        alerts: AlertSettings {
            elevated:        0.025,  // 2.5% annual
            critical:        0.08,   // 8.0% annual
            thirty_day_warn: 0.01,
        },
        ingestion: IngestionSettings {
            poll_interval_seconds: 1,
            max_events_per_batch:  500,
        },
        dashboard: DashboardSettings {
            host:         "0.0.0.0".into(),
            port:         8000,
            operator_key: "CHANGE_ME_BEFORE_DEPLOY".into(),
            base_path:    String::new(),
        },
        llm: LlmSettings::default(),
    }
}

// ── Signal handler ────────────────────────────────────────────────────────────

async fn wait_for_shutdown() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut sigint  = signal(SignalKind::interrupt()).expect("SIGINT handler");
    let mut sigterm = signal(SignalKind::terminate()).expect("SIGTERM handler");
    tokio::select! {
        _ = sigint.recv()  => info!("SIGINT received"),
        _ = sigterm.recv() => info!("SIGTERM received"),
    }
    info!("Shutdown signal received — stopping GCRM...");
}

// ── Main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    // ── One-shot migration subcommand ──────────────────────────────────────────
    // `gcrm backfill` tags archived events with their theater (so the systemic layer
    // lights up immediately on restart) and exits without starting the server.
    if std::env::args().nth(1).as_deref() == Some("backfill") {
        backfill::run();
        return;
    }

    // ── Logging ───────────────────────────────────────────────────────────────
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("gcrm=info,warn"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_thread_ids(false)
        .compact()
        .init();

    std::fs::create_dir_all("logs").ok();

    // ── Settings ──────────────────────────────────────────────────────────────
    let mut settings = load_settings();

    // ── H-01: Reject insecure operator keys ──────────────────────────────────
    //
    // If the configured key is empty or matches a known-insecure value,
    // disable operator endpoints by clearing the key. api.rs check_key()
    // already returns 403 Forbidden when the expected key is empty — this
    // leverages that existing behavior.
    //
    // The system continues to start: the dashboard, WebSocket, and all
    // read-only /api/* routes remain functional. Only the operator API
    // (/api/regime/*, /api/operator/*) is locked out.
    if is_insecure_key(&settings.dashboard.operator_key) {
        error!("{}", "!".repeat(60));
        error!("  OPERATOR KEY REJECTED — INSECURE VALUE DETECTED");
        error!("");
        error!("  The configured operator_key is a known-insecure default.");
        error!("  All operator API endpoints are DISABLED.");
        error!("  Regime factors cannot be toggled via the API.");
        error!("  Manual events cannot be asserted via the API.");
        error!("");
        error!("  To enable the operator API, set a strong random key:");
        error!("    openssl rand -hex 32");
        error!("  Then update dashboard.operator_key in settings.yml");
        error!("{}", "!".repeat(60));
        settings.dashboard.operator_key = String::new();
    }

    // ── Compute and log regime product for verification ───────────────────────
    let regime_product: f64 = settings.regime_factors.iter()
        .filter(|f| f.active)
        .map(|f| f.multiplier)
        .product();
    let active_count   = settings.regime_factors.iter().filter(|f| f.active).count();
    let adjusted_prior = models::HISTORICAL_ANCHOR * regime_product;

    info!("{}", "=".repeat(60));
    info!("  Global Conflict Risk Monitor (Rust)");
    info!("  P₀ = BASELINE_ANNUAL (modern quiet-year baseline) = {:.6} / yr", models::BASELINE_ANNUAL);
    info!("  Regime: {active_count} active factors × {regime_product:.4} = adjusted prior {:.4}%/yr",
          adjusted_prior * 100.0);
    info!("  Nuclear detector: ENABLED — {} FDSN sources, {} test sites",
          detector::FDSN_SOURCES.len(), detector::KNOWN_TEST_SITES.len());
    if settings.dashboard.operator_key.is_empty() {
        info!("  Operator API: DISABLED (insecure key rejected)");
    } else {
        info!("  Operator API: ENABLED");
    }
    info!("{}", "=".repeat(60));

    // ── Channels ──────────────────────────────────────────────────────────────
    let (raw_tx,   raw_rx)   = mpsc::channel::<RawArticle>(RAW_CAP);
    let (event_tx, event_rx) = mpsc::channel::<models::GeopoliticalEvent>(EVENT_CAP);
    let (snap_tx,  snap_rx)  = mpsc::channel::<models::RiskSnapshot>(SNAP_CAP);

    // ── Shared state ──────────────────────────────────────────────────────────
    let app_state = AppState::new();

    // Ensure the logs dir exists once at startup; the timeline/article/event
    // append paths assume it exists (no per-write create_dir_all → fewer syscalls).
    if let Err(e) = std::fs::create_dir_all("logs") {
        warn!("Could not create logs dir: {e}");
    }

    // ── Boot EpochStore — load full timeline history from disk before serving ─
    {
        let epoch = load_epoch().await;
        *app_state.epoch_store.lock().await = epoch;
    }

    // ── Boot ArticleStore — restore the article feed from disk before serving ─
    {
        let max_size = app_state.article_store.lock().await.max_size;
        let articles = load_articles(max_size).await;
        *app_state.article_store.lock().await = articles;
    }

    // ── Server state ──────────────────────────────────────────────────────────
    let (server_state, _broadcast_tx) = ServerState::new(Arc::clone(&app_state), &settings.dashboard.base_path);

    // ── Ingestor ──────────────────────────────────────────────────────────────
    let ingestor = Ingestor::new(
        raw_tx,
        Arc::clone(&app_state),
        settings.ingestion.poll_interval_seconds,
    );

    // ── Aggregator ────────────────────────────────────────────────────────────
    let mut aggregator = Aggregator::new(
        settings.regime_factors.clone(),
        settings.alerts.clone(),
        event_rx,
        snap_tx,
        Arc::clone(&app_state),
        settings.ingestion.poll_interval_seconds,
    );
    // Restore the Bayesian event window from disk so domain scores + P(WWIII)
    // survive restarts instead of resetting to baseline.
    aggregator.preload_events(load_events().await);

    // ── Operator API state ────────────────────────────────────────────────────
    *app_state.shared_regime.lock().await = settings.regime_factors.clone();

    let operator_state = OperatorState::new(
        Arc::clone(&app_state),
        settings.dashboard.operator_key.clone(),
        settings.regime_factors.clone(),
    );
    let host      = settings.dashboard.host.clone();
    let port      = settings.dashboard.port;
    let base_path = settings.dashboard.base_path.clone();

    info!("Dashboard → http://localhost:{port}");
    info!("Pipeline: Ingestor → NLP processor (pure Rust) → Aggregator → WebSocket → Dashboard");
    info!("Press Ctrl+C to stop");

    let (nlp_sidecar, nlp_handle) = NlpSidecar::with_shutdown(
        raw_rx,
        event_tx,
        Arc::clone(&app_state),
        settings.llm.clone(),
    );

    let server_state_bc      = server_state.clone();
    let seismic_monitor      = SeismicMonitor::new(Arc::clone(&app_state));
    let ctbto_monitor        = CtbtoMonitor::new(Arc::clone(&app_state));
    let nuclear_news_monitor = NuclearNewsMonitor::new(Arc::clone(&app_state));

    // Named handle so shutdown can AWAIT the NLP task's dedup-cache save before the
    // process exits — a bare select arm dropped it mid-save, so the cache never wrote.
    let mut nlp_task = tokio::spawn(nlp_sidecar.run());

    tokio::select! {
        _ = tokio::spawn(ingestor.run()) => {
            error!("Ingestor task exited unexpectedly");
        }
        _ = &mut nlp_task => {
            error!("NLP sidecar task exited unexpectedly");
        }
        _ = tokio::spawn(aggregator.run()) => {
            error!("Aggregator task exited unexpectedly");
        }
        _ = tokio::spawn(broadcast_snapshots(snap_rx, server_state_bc)) => {
            error!("Broadcast task exited unexpectedly");
        }
        _ = tokio::spawn(seismic_monitor.run()) => {
            error!("Seismic monitor task exited unexpectedly");
        }
        _ = tokio::spawn(ctbto_monitor.run()) => {
            error!("CTBTO monitor task exited unexpectedly");
        }
        _ = tokio::spawn(nuclear_news_monitor.run()) => {
            error!("Nuclear news monitor task exited unexpectedly");
        }
        _ = tokio::spawn(brief::run_brief_loop(Arc::clone(&app_state), settings.llm.clone())) => {
            error!("Analyst brief task exited unexpectedly");
        }
        result = tokio::spawn(serve(host, port, server_state, operator_state, base_path)) => {
            match result {
                Ok(Ok(())) => info!("Server stopped cleanly"),
                Ok(Err(e)) => error!("Server error: {e}"),
                Err(e)     => error!("Server panic: {e}"),
            }
        }
        _ = wait_for_shutdown() => {
            // Signal the NLP task and AWAIT it (bounded) so its dedup-cache save
            // actually completes before exit — previously main returned immediately
            // and the runtime killed the task mid-save, so the cache never persisted.
            nlp_handle.shutdown();
            match tokio::time::timeout(std::time::Duration::from_secs(10), &mut nlp_task).await {
                Ok(_)  => info!("NLP task stopped (dedup cache saved). Exiting."),
                Err(_) => tracing::warn!("NLP task did not stop within 10s — exiting anyway."),
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insecure_key_rejects_default_placeholder() {
        assert!(is_insecure_key("CHANGE_ME_BEFORE_DEPLOY"));
    }

    #[test]
    fn insecure_key_rejects_case_insensitive() {
        assert!(is_insecure_key("change_me_before_deploy"));
        assert!(is_insecure_key("Change_Me_Before_Deploy"));
        assert!(is_insecure_key("CHANGEME"));
        assert!(is_insecure_key("changeme"));
    }

    #[test]
    fn insecure_key_rejects_empty() {
        assert!(is_insecure_key(""));
    }

    #[test]
    fn insecure_key_rejects_common_placeholders() {
        assert!(is_insecure_key("password"));
        assert!(is_insecure_key("admin"));
        assert!(is_insecure_key("secret"));
        assert!(is_insecure_key("test"));
        assert!(is_insecure_key("default"));
        assert!(is_insecure_key("placeholder"));
        assert!(is_insecure_key("testingmeasure1"));
    }

    #[test]
    fn insecure_key_accepts_strong_random_key() {
        assert!(!is_insecure_key("a3f8b2c1d4e5f6789012345678abcdef0123456789abcdef0123456789abcdef"));
    }

    #[test]
    fn insecure_key_accepts_reasonable_custom_key() {
        assert!(!is_insecure_key("my-production-operator-key-2026"));
    }

    #[test]
    fn insecure_key_does_not_substring_match() {
        // "testingmeasure1" is blocked, but extensions of it are not —
        // we do exact match (case-insensitive), not substring.
        assert!(!is_insecure_key("testingmeasure1_extended"));
        assert!(!is_insecure_key("my_password_is_strong"));
    }

    #[test]
    fn default_settings_key_is_insecure() {
        let settings = default_settings();
        assert!(is_insecure_key(&settings.dashboard.operator_key),
            "The hard-coded default key must be caught by the insecure key check");
    }

    #[test]
    fn insecure_keys_list_is_nonempty() {
        assert!(!INSECURE_KEYS.is_empty());
    }

    #[test]
    fn insecure_keys_list_entries_are_lowercase() {
        for key in INSECURE_KEYS {
            assert_eq!(*key, key.to_lowercase(),
                "INSECURE_KEYS entries must be lowercase for case-insensitive comparison: {key}");
        }
    }
}
