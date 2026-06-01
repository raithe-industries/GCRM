// ------------------------------------------------------------
// RAiTHE INDUSTRIES INCORPORATED
// Copyright (c) 2026 All Rights Reserved.
//
// This file is part of a proprietary system. Unauthorized use,
// reproduction, or distribution is strictly prohibited.
// ------------------------------------------------------------

// src/brief.rs — AI analyst brief generator  [GCRM v2, Phase 4]
//
// A periodic task that turns the current systemic snapshot into a short, measured
// prose brief — the "why the number is where it is" insight layer. Runs out of band
// (never blocks the pipeline), caches its output in AppState, and is served at
// GET {base}/api/brief. Falls back to a deterministic templated brief when the LLM
// is disabled or unreachable, so the endpoint always returns something useful.

use std::time::Duration;

use chrono::Utc;
use serde_json::{json, Value};
use tracing::{info, warn};

use crate::aggregator::SharedState;
use crate::llm_enricher::LlmEnricher;
use crate::models::LlmSettings;

/// How often the brief is regenerated.
const BRIEF_INTERVAL_SECS: u64 = 300; // 5 minutes
/// Initial delay so the first real snapshot exists before the first brief.
const BRIEF_WARMUP_SECS: u64 = 12;

pub async fn run_brief_loop(state: SharedState, settings: LlmSettings) {
    let enricher = LlmEnricher::new(settings.clone());
    info!("Analyst brief: task online (interval {}s, source={})",
          BRIEF_INTERVAL_SECS, if settings.enabled { "llm+template" } else { "template" });

    tokio::time::sleep(Duration::from_secs(BRIEF_WARMUP_SECS)).await;
    let mut tick = tokio::time::interval(Duration::from_secs(BRIEF_INTERVAL_SECS));

    loop {
        tick.tick().await;

        let snap = { state.latest_snapshot.lock().await.clone() };
        let Some(snap) = snap else { continue };

        let context = build_context(&snap);
        let (text, source) = match enricher.brief(&context).await {
            Some(t) => (t, "llm"),
            None    => (templated_brief(&snap), "template"),
        };

        let brief = json!({
            "generated_at": Utc::now().to_rfc3339(),
            "text":         text,
            "source":       source,
            "model":        settings.model,
        });
        *state.analyst_brief.lock().await = Some(brief);
        if source == "llm" {
            info!("Analyst brief updated (LLM, {} chars)", text.len());
        } else {
            warn!("Analyst brief updated (templated fallback — LLM unavailable)");
        }
    }
}

fn f(snap: &Value, ptr: &str) -> f64 { snap.pointer(ptr).and_then(|v| v.as_f64()).unwrap_or(0.0) }
fn s<'a>(snap: &'a Value, ptr: &str) -> &'a str { snap.pointer(ptr).and_then(|v| v.as_str()).unwrap_or("") }

/// Theaters sorted hottest-first.
fn theaters_sorted(snap: &Value) -> Vec<&Value> {
    let mut ts: Vec<&Value> = snap.get("theaters").and_then(|v| v.as_array()).map(|a| a.iter().collect()).unwrap_or_default();
    ts.sort_by(|a, b| f(b, "/heat").partial_cmp(&f(a, "/heat")).unwrap_or(std::cmp::Ordering::Equal));
    ts
}

/// Compact factual context handed to the LLM.
fn build_context(snap: &Value) -> String {
    let mut out = format!(
        "Systemic risk index: {:.0}/100. P(systemic war, annualized): {:.1}%. Alert: {}. Driver: {}.\n\
         Couplers: great-power entanglement {:.2}, theater concurrency {:.1}, alliance {:.2}, guardrail collapse {:.2}.\n\
         Theaters (hottest first):\n",
        f(snap, "/systemic/index"),
        f(snap, "/probabilities/annual_pct"),
        s(snap, "/alert/level"),
        s(snap, "/systemic/driver"),
        f(snap, "/couplers/gp_entanglement"),
        f(snap, "/couplers/concurrency"),
        f(snap, "/couplers/alliance_activation"),
        f(snap, "/couplers/guardrail_collapse"),
    );
    for t in theaters_sorted(snap) {
        let actors = t.get("top_actors").and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", "))
            .unwrap_or_default();
        out.push_str(&format!(
            "- {}: {} (heat {:.2}, {}). Key actors: {}.\n",
            s(t, "/label"), s(t, "/rung_label"), f(t, "/heat"), s(t, "/trend"),
            if actors.is_empty() { "n/a".into() } else { actors },
        ));
    }
    out.push_str("\nWrite the analyst brief now: explain the overall reading and the top 2-3 theater drivers.");
    out
}

/// Deterministic prose fallback built directly from the snapshot.
fn templated_brief(snap: &Value) -> String {
    let idx = f(snap, "/systemic/index");
    let pct = f(snap, "/probabilities/annual_pct");
    let alert = s(snap, "/alert/level");
    let driver = s(snap, "/systemic/driver");

    let hot: Vec<String> = theaters_sorted(snap).into_iter()
        .filter(|t| f(t, "/heat") >= 0.18)
        .map(|t| format!("{} ({})", s(t, "/label"), s(t, "/rung_label")))
        .collect();

    let mut p = format!(
        "Systemic risk index stands at {:.0}/100 ({} alert), an annualized systemic-war estimate of {:.1}%. \
         The dominant driver is {}.",
        idx, alert, pct, if driver.is_empty() { "no single theater above baseline" } else { driver },
    );
    if !hot.is_empty() {
        p.push_str(&format!(
            " Theaters currently elevated: {}. Multiple concurrently-hot theaters coupled to nuclear-armed \
             great powers are what drive the systemic reading rather than any single regional war.",
            hot.join("; "),
        ));
    } else {
        p.push_str(" No theater is currently above the crisis threshold; the reading sits near the structural baseline.");
    }
    p.push_str(" (Automated summary — the narrative model is offline; figures are model-generated and not a forecast of certainty.)");
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Value {
        json!({
            "systemic": { "index": 76.0, "driver": "US/Israel–Iran at Great-Power War; 3 theaters hot" },
            "probabilities": { "annual_pct": 45.0 },
            "alert": { "level": "critical" },
            "couplers": { "gp_entanglement": 1.0, "concurrency": 3.0, "alliance_activation": 0.0, "guardrail_collapse": 1.0 },
            "theaters": [
                { "label": "US/Israel–Iran", "rung_label": "Great-Power War", "heat": 0.83, "trend": "rising", "top_actors": ["united_states","iran"] },
                { "label": "NATO–Russia",    "rung_label": "Great-Power War", "heat": 0.77, "trend": "stable", "top_actors": ["russia","ukraine"] },
                { "label": "Korean Peninsula","rung_label": "Stable",          "heat": 0.01, "trend": "stable", "top_actors": [] }
            ]
        })
    }

    #[test]
    fn context_includes_index_and_theaters() {
        let c = build_context(&sample());
        assert!(c.contains("76/100"));
        assert!(c.contains("US/Israel–Iran"));
        assert!(c.contains("NATO–Russia"));
        // hottest first
        assert!(c.find("US/Israel–Iran").unwrap() < c.find("NATO–Russia").unwrap());
    }

    #[test]
    fn templated_brief_mentions_hot_theaters_only() {
        let b = templated_brief(&sample());
        assert!(b.contains("76/100"));
        assert!(b.contains("US/Israel–Iran"));
        assert!(!b.contains("Korean Peninsula"), "stable theater should not be listed as elevated");
    }
}
