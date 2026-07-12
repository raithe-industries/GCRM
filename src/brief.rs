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
use crate::models::{EscalationRung, LlmSettings};

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

/// The authoritative escalation-rung level (0..5) of a serialized theater, read from the
/// engine's own `/rung` field — the SAME rung the board renders — so the brief can never
/// disagree with the board about how elevated a theater is. Crucially this respects the
/// rung OVERRIDES (`rung_for`: a chemical/bio attack floors a theater at Limited War, a
/// confirmed nuclear detonation forces Systemic) which raise the rung independently of
/// `heat`: a nuclear-use theater can sit at heat well below the Crisis heat boundary yet be
/// the apex front, so filtering on raw heat would silently drop it. Missing/unknown rung →
/// level 0 (Stable), the fail-safe that leaves such a theater out of the elevated list.
fn theater_rung_level(t: &Value) -> u8 {
    t.get("rung").cloned()
        .and_then(|v| serde_json::from_value::<EscalationRung>(v).ok())
        .map(|r| r.level())
        .unwrap_or(0)
}

/// The model's own one-line account of WHICH coupling channel is turning a regional
/// crisis into a *world*-war risk, from the live `coupling_driver` read-out. `None`
/// when no channel lifts above the floor (the elevation is regionally contained).
/// Honest by construction — a restatement of the engine's dominant amplifier, never a
/// canned mechanism claim that could contradict the actual systemic state.
fn coupling_sentence(coupling_driver: &str) -> Option<&'static str> {
    match coupling_driver {
        "single-theater nuclear brink" => Some(
            "The dominant systemic amplifier is a direct nuclear-armed great-power confrontation \
             concentrated in a single theater — the apex configuration, not breadth across regions."),
        "great-power entanglement" => Some(
            "The dominant systemic amplifier is great powers entangled across multiple theaters, \
             which is what turns regional crises into a world-war risk rather than any single war."),
        "multi-theater concurrency" => Some(
            "The dominant systemic amplifier is multiple theaters hot at once, raising the odds that \
             one regional war pulls the others in."),
        "alliance activation" => Some(
            "The dominant systemic amplifier is a mutual-defense alliance invocation, which can convert \
             a bilateral clash into a bloc-wide war."),
        "structural guardrail collapse" => Some(
            "The dominant systemic amplifier is structural guardrail collapse — eroded arms-control and \
             deterrence frameworks — which raises the danger of any live crisis rather than acute coupling \
             across theaters."),
        _ => None,
    }
}

/// Compact factual context handed to the LLM.
fn build_context(snap: &Value) -> String {
    let coupling_driver = s(snap, "/couplers/coupling_driver");
    let mut out = format!(
        "Systemic risk index: {:.0}/100. P(systemic war, annualized): {:.1}%. Alert: {}. Driver: {}.\n\
         Couplers: great-power entanglement {:.2}, theater concurrency {:.1}, alliance {:.2}, guardrail collapse {:.2}.\n\
         Dominant coupling channel: {}.\n\
         Theaters (hottest first):\n",
        f(snap, "/systemic/index"),
        f(snap, "/probabilities/annual_pct"),
        s(snap, "/alert/level"),
        s(snap, "/systemic/driver"),
        f(snap, "/couplers/gp_entanglement"),
        f(snap, "/couplers/concurrency"),
        f(snap, "/couplers/alliance_activation"),
        f(snap, "/couplers/guardrail_collapse"),
        if coupling_driver.is_empty() { "none (regional, not yet systemically coupled)" } else { coupling_driver },
    );
    if snap.pointer("/couplers/breadth_saturated").and_then(Value::as_bool).unwrap_or(false) {
        out.push_str(
            "NOTE: the read is BREADTH-SATURATED — every systemic breadth amplifier is railed \
             (top-theater heat clamped at the model maximum, great-power entanglement and \
             alliance activation both maxed) and no single-theater nuclear brink is live. The \
             estimate has no headroom left from intensification of the current crises; the only \
             lever that would raise it is a direct nuclear brink.\n");
    }
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
    let coupling_driver = s(snap, "/couplers/coupling_driver");

    // "Elevated" is judged by the AUTHORITATIVE rung (Crisis+), not a raw-heat proxy. The old
    // `heat >= 0.18` cut was an un-named duplicate of the engine's Tension→Crisis heat boundary
    // (HOT_HEAT), and — being on raw heat — it silently dropped the two rung OVERRIDES: a
    // chemical/bio attack (floored at Limited War) and a confirmed nuclear detonation (forced to
    // Systemic) can sit below that heat while being the most dangerous front on the board. Keying
    // off `rung` includes exactly the same heat-driven theaters (rung ≥ Crisis ⇔ heat ≥ HOT_HEAT)
    // and additionally the override-elevated ones, so the fallback brief can no longer omit a
    // nuclear-war theater the board is flagging. Ordered rung-first (then heat) so the apex front
    // leads even when its heat is low — a Systemic theater must never be buried below a Crisis one.
    let mut hot_ts: Vec<&Value> = theaters_sorted(snap).into_iter()
        .filter(|t| theater_rung_level(t) >= EscalationRung::Crisis.level())
        .collect();
    hot_ts.sort_by(|a, b| theater_rung_level(b).cmp(&theater_rung_level(a))
        .then(f(b, "/heat").partial_cmp(&f(a, "/heat")).unwrap_or(std::cmp::Ordering::Equal)));
    let hot: Vec<String> = hot_ts.into_iter()
        .map(|t| format!("{} ({})", s(t, "/label"), s(t, "/rung_label")))
        .collect();

    let mut p = format!(
        "Systemic risk index stands at {:.0}/100 ({} alert), an annualized systemic-war estimate of {:.1}%. \
         The dominant driver is {}.",
        idx, alert, pct, if driver.is_empty() { "no single theater above baseline" } else { driver },
    );
    if !hot.is_empty() {
        p.push_str(&format!(" Theaters currently elevated: {}.", hot.join("; ")));
        // Account for the systemic reading from the model's OWN dominant coupling channel,
        // never a canned mechanism claim. A single-theater nuclear brink, for instance,
        // must NOT print "multiple concurrently-hot theaters … rather than any single war".
        match coupling_sentence(coupling_driver) {
            Some(sentence) => { p.push(' '); p.push_str(sentence); }
            None => p.push_str(
                " No coupling channel yet links them into a systemic risk; the elevation \
                 remains regionally contained."),
        }
        // Honesty: when every breadth amplifier is railed, the estimate is a structural MAX,
        // not a precise point estimate that will keep climbing as the current crises worsen.
        // Say so plainly so the operator does not read a saturated breadth peg (which sits
        // below the forecast ceiling, so the "capped" caveat never fires) as still-rising.
        if snap.pointer("/couplers/breadth_saturated").and_then(Value::as_bool).unwrap_or(false) {
            p.push_str(
                " The breadth amplifiers are railed: this is a structural-maximum read with no \
                 headroom left from the current crises intensifying — only a direct nuclear brink \
                 would raise it further.");
        }
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
            "couplers": { "gp_entanglement": 1.0, "concurrency": 3.0, "alliance_activation": 0.0, "guardrail_collapse": 1.0, "coupling_driver": "great-power entanglement" },
            "theaters": [
                { "label": "US/Israel–Iran", "rung": "great_power_war", "rung_label": "Great-Power War", "heat": 0.83, "trend": "rising", "top_actors": ["united_states","iran"] },
                { "label": "NATO–Russia",    "rung": "great_power_war", "rung_label": "Great-Power War", "heat": 0.77, "trend": "stable", "top_actors": ["russia","ukraine"] },
                { "label": "Korean Peninsula","rung": "stable",          "rung_label": "Stable",          "heat": 0.01, "trend": "stable", "top_actors": [] }
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

    #[test]
    fn templated_brief_lists_an_override_elevated_theater_below_the_heat_boundary() {
        // HONESTY/AWARENESS lock: the "elevated theaters" filter must key off the AUTHORITATIVE
        // rung, not raw heat. `rung_for` floors a chemical/bio attack at Limited War and forces a
        // confirmed nuclear detonation to Systemic REGARDLESS of heat — so such a theater can sit
        // below the Tension→Crisis heat boundary (0.18) yet be the apex front. The pre-fix filter
        // `heat >= 0.18` silently DROPPED it, so the LLM-offline fallback brief could omit a
        // nuclear-war theater the board was flagging. Construct exactly that: a nuclear-use theater
        // at heat 0.10 (Systemic rung) plus a conventional Crisis theater and a Stable one.
        let mut snap = sample();
        snap["theaters"] = json!([
            { "label": "US/Israel–Iran", "rung": "crisis",  "rung_label": "Crisis",      "heat": 0.40, "trend": "rising", "top_actors": ["united_states","iran"] },
            { "label": "Kashmir LoC",    "rung": "systemic", "rung_label": "Systemic War","heat": 0.10, "trend": "rising", "top_actors": ["india","pakistan"] },
            { "label": "Korean Peninsula","rung": "stable",  "rung_label": "Stable",      "heat": 0.03, "trend": "stable", "top_actors": [] }
        ]);
        let b = templated_brief(&snap);
        // (1) The nuclear-use Systemic theater is LISTED even though heat 0.10 < 0.18 — the defect.
        assert!(b.contains("Kashmir LoC (Systemic War)"),
            "an override-elevated (nuclear-use → Systemic) theater below the heat boundary must \
             still be named as elevated, got:\n{b}");
        // (2) It LEADS the elevated list (rung-first ordering): the apex front must not be buried
        //     below a merely-hotter conventional Crisis.
        assert!(b.contains("Kashmir LoC (Systemic War); US/Israel–Iran (Crisis)"),
            "the most severe rung must lead the elevated list regardless of heat, got:\n{b}");
        // (3) The Stable theater is still excluded — the fix widens by RUNG, not by dropping the
        //     hot-only discipline.
        assert!(!b.contains("Korean Peninsula"),
            "a Stable theater must remain excluded from the elevated list, got:\n{b}");
    }

    #[test]
    fn context_includes_the_dominant_coupling_channel() {
        let c = build_context(&sample());
        assert!(c.contains("Dominant coupling channel: great-power entanglement"),
            "the LLM context must surface the model's own dominant coupling channel, got:\n{c}");
    }

    #[test]
    fn templated_brief_accounts_for_systemic_reading_from_the_live_coupling_driver() {
        // Honesty lock: the systemic-mechanism sentence must come from the model's own
        // `coupling_driver`, never a canned claim. The pre-fix code printed "Multiple
        // concurrently-hot theaters … rather than any single regional war" for EVERY hot
        // world — flatly wrong when the dominant amplifier is a single-theater nuclear brink.
        let mut snap = sample();
        snap["couplers"]["coupling_driver"] = json!("single-theater nuclear brink");
        let b = templated_brief(&snap);
        assert!(b.contains("single theater"),
            "a nuclear-brink world must name the single-theater apex, got:\n{b}");
        assert!(!b.contains("rather than any single regional war"),
            "the canned 'rather than any single regional war' claim must be GONE — it \
             contradicts a single-theater brink, got:\n{b}");

        // The great-power-entanglement default fixture names that channel.
        let b2 = templated_brief(&sample());
        assert!(b2.contains("great powers entangled across multiple theaters"),
            "the gp-entanglement world must name that channel, got:\n{b2}");

        // Hot theaters but NO dominant coupling channel → honest "regionally contained",
        // not a fabricated coupling story.
        let mut snap3 = sample();
        snap3["couplers"]["coupling_driver"] = json!("");
        let b3 = templated_brief(&snap3);
        assert!(b3.contains("regionally contained"),
            "an uncoupled-but-hot world must read as regionally contained, got:\n{b3}");

        // Structural guardrail collapse leading a live crisis → the brief names the structural
        // channel (eroded arms-control/deterrence), not a fabricated acute-coupling story.
        let mut snap4 = sample();
        snap4["couplers"]["coupling_driver"] = json!("structural guardrail collapse");
        let b4 = templated_brief(&snap4);
        assert!(b4.contains("structural guardrail collapse"),
            "a guardrail-led world must name the structural channel, got:\n{b4}");
    }

    #[test]
    fn templated_brief_discloses_a_breadth_saturated_read_as_a_structural_maximum() {
        // Honesty lock: a railed breadth peg (couplers.breadth_saturated) sits BELOW the
        // forecast ceiling, so the "capped" caveat never fires. The brief must still warn the
        // operator that the number is a structural maximum with no headroom from the current
        // crises worsening — otherwise a saturated 83% reads as a still-climbing point estimate.
        let mut snap = sample();
        assert!(!templated_brief(&snap).contains("structural-maximum read"),
            "a resolved read must NOT carry the saturation caveat");
        snap["couplers"]["breadth_saturated"] = json!(true);
        let b = templated_brief(&snap);
        assert!(b.contains("structural-maximum read") && b.contains("nuclear brink"),
            "a breadth-saturated read must disclose the structural maximum + the only remaining \
             lever (a nuclear brink), got:\n{b}");
        // The LLM context must carry the same fact so the generated brief can't omit it.
        snap["couplers"]["breadth_saturated"] = json!(true);
        assert!(build_context(&snap).contains("BREADTH-SATURATED"),
            "the LLM context must surface the breadth-saturation state");
    }
}
