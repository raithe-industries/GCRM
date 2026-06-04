// ------------------------------------------------------------
// RAiTHE INDUSTRIES INCORPORATED
// Copyright (c) 2026 All Rights Reserved.
//
// This file is part of a proprietary system. Unauthorized use,
// reproduction, or distribution is strictly prohibited.
// ------------------------------------------------------------

// src/theater.rs — Theater decomposition + systemic index (GCRM v2, Phase 2)
//
// A systemic (world) war is not "many global domains light up." It is a regional
// war in a theater that COUPLES to great powers, while OTHER theaters are also hot
// and the GUARDRAILS are gone. This module scores each theater independently from
// the events assigned to it (peak-aware, reusing the modality scorer), places each
// on a discrete escalation rung, and combines them with the systemic couplers into:
//
//   systemic_index (0..100, escalation-ladder-aligned)   — the public headline
//   L_sys          (systemic likelihood)                 — drives the secondary P
//
// The structural / guardrail component is still carried by the operator-tunable
// regime multiplier (passed in) until settings migrate to explicit couplers.

use std::collections::HashMap;

use crate::bayesian::{co_occurrence_boost, domain_weight, DomainScorer, DOMAIN_WEIGHTS};
use crate::models::{
    EscalationRung, GeopoliticalEvent, SystemicCouplers, Theater, TheaterState,
    ELEVATION_THRESHOLD,
};

// ── Tuning knobs (provisional — fitted in the Phase-3 backtest harness) ──────────

/// Master sensitivity of the systemic likelihood on the log-odds scale.
/// Chosen so the current acute-crisis corpus (live US/Israel–Iran war + closed
/// Hormuz + Ukraine yr5 + dead arms control) reads in the >25% band Robert set as
/// the target, while a quiet world stays near the baseline.
pub const EVIDENCE_GAIN_SYS: f64 = 2.4;

/// Heat at/above which a theater counts as "hot" (≥ Crisis) for concurrency.
const HOT_HEAT: f64 = 0.18;

/// Half-width of the smooth ramp around HOT_HEAT for fractional concurrency.
const HOT_RAMP: f64 = 0.06;

/// Half-width of the smooth ramp around ELEVATION_THRESHOLD for intra-theater
/// modality co-occurrence (mirrors bayesian::ELEVATION_RAMP).
const ELEV_RAMP: f64 = 0.08;

/// Nuclear-posture modality score at/above which a theater that also entangles
/// ≥ `BRINK_MIN_GREAT_POWERS` distinct great powers counts as a direct nuclear-brink
/// (apex) configuration — a Cuba-1962 head-to-head. This is the SINGLE source of
/// truth for what the apex state IS: it is shared by the systemic amplifier below
/// (`brink_mult`) AND the I&W "nuclear-brink (apex)" indicator (indicators.rs), so
/// the headline number and the board that explains it can never disagree about
/// whether the apex configuration is live. Fitted against the Cuba band in
/// backtest.rs — do not blind-tweak.
pub const BRINK_NUCLEAR_THRESHOLD: f64 = 0.78;

/// Distinct great powers that must be directly entangled in ONE theater for a brink.
pub const BRINK_MIN_GREAT_POWERS: usize = 2;

/// Public-facing ceiling for the systemic index (a *forecast*). The 0–100 scale keeps
/// 100 as its visible terminal point — but 100 means "confirmed by record" (a verified
/// detonation / mass-casualty great-power war), NOT "the model is certain". Nothing in
/// GCRM can record-verify that: the top "Systemic" rung is reached via NEWS-keyword
/// inference (`nuclear_use_in`), and the seismic detector deliberately reports anomalies,
/// never nuclear confirmations (server.rs). So a model-inferred reading must never print
/// 100 ("certainty about the future is unwise") — it saturates at 95 ("very high, not
/// certain"). 100 is reserved for an out-of-band, record-verified assertion, never inference.
pub const FORECAST_INDEX_CEILING: f64 = 95.0;

/// Canonical great-power actor ids → a coarse great-power label, for counting how
/// many DISTINCT great powers are entangled across hot theaters.
fn great_power_label(actor_id: &str) -> Option<&'static str> {
    match actor_id {
        "united_states" | "united_states_military" => Some("us"),
        "russia" | "russia_military"               => Some("russia"),
        "china"  | "china_military"                => Some("china"),
        "nato"                                      => Some("nato"),
        _ => None,
    }
}

/// Count of DISTINCT great powers among a theater's dominant actors.
pub fn distinct_great_powers(top_actors: &[String]) -> usize {
    top_actors.iter()
        .filter_map(|a| great_power_label(a))
        .collect::<std::collections::HashSet<_>>()
        .len()
}

/// Whether a theater is a direct nuclear-brink (apex) configuration: extreme nuclear
/// posture AND ≥ `BRINK_MIN_GREAT_POWERS` distinct great powers entangled in the SAME
/// theater. This single predicate is used by BOTH the systemic amplifier (`brink_mult`,
/// theater.rs) and the I&W "nuclear-brink (apex)" indicator (indicators.rs), so the
/// headline number and the board light trip on exactly the same condition.
pub fn theater_is_nuclear_brink(t: &TheaterState) -> bool {
    t.modality_scores.get("nuclear_posture").copied().unwrap_or(0.0) >= BRINK_NUCLEAR_THRESHOLD
        && distinct_great_powers(&t.top_actors) >= BRINK_MIN_GREAT_POWERS
}

fn smoothstep(x: f64, lo: f64, hi: f64) -> f64 {
    if x <= lo { return 0.0; }
    if x >= hi { return 1.0; }
    let t = (x - lo) / (hi - lo);
    t * t * (3.0 - 2.0 * t)
}

fn max_weighted_sum() -> f64 {
    DOMAIN_WEIGHTS.iter().map(|(_, w)| w).sum()
}

/// Map a theater's heat (+ overrides) to a discrete escalation rung.
fn rung_for(heat: f64, gp_involved: bool, wmd_used: bool, nuclear_used: bool) -> EscalationRung {
    let mut r = if heat < 0.06 {
        EscalationRung::Stable
    } else if heat < 0.18 {
        EscalationRung::Tension
    } else if heat < 0.38 {
        EscalationRung::Crisis
    } else if heat < 0.62 {
        EscalationRung::LimitedWar
    } else {
        EscalationRung::GreatPowerWar
    };
    // A chemical/bio attack floors the theater at Limited War.
    if wmd_used && r.level() < EscalationRung::LimitedWar.level() {
        r = EscalationRung::LimitedWar;
    }
    // A great power directly in a war makes it Great-Power War.
    if gp_involved && r.level() >= EscalationRung::LimitedWar.level() {
        r = EscalationRung::GreatPowerWar;
    }
    // Confirmed nuclear use is the systemic rung (kept strict so it ~never fires
    // on conventional crises — no weapon has been used).
    if nuclear_used {
        r = EscalationRung::Systemic;
    }
    r
}

/// Strict detection of actual nuclear *use* — a real detonation in anger, NOT
/// posture, threats, capability talk, drills, or tests.
///
/// This is the single trigger for the apex Systemic rung, which pegs the headline at
/// the 95 forecast ceiling and floods P(WWIII). So it must be unforgiving: "nuclear
/// strike" is the dominant phrasing of *threats* ("Russia threatens nuclear strike
/// if NATO intervenes") and "nuclear detonation" routinely describes *tests* ("North
/// Korea nuclear detonation in latest weapons test") — neither is use-in-war, yet the
/// old plain substring match forced the catastrophe rung on both. A confirmed strike
/// is reported in the indicative ("nuclear detonation confirmed", "a nuclear weapon
/// was used"), never in the conditional/subjunctive or alongside drill/test framing.
///
/// We therefore require a use-phrase AND the absence of any whole-word non-use framing
/// token. Whole-word matching (split on non-alphanumerics) avoids substring traps such
/// as "latest"→"test". The `any()` over the whole window keeps recall high for a real
/// detonation (which spawns many headlines): a single clean confirmation still trips
/// the rung even if other headlines carry threat/test framing.
fn nuclear_use_in(tev: &[GeopoliticalEvent]) -> bool {
    const USE_PHRASES: &[&str] = &[
        "nuclear detonation", "nuclear weapon used", "nuclear weapon was used",
        "nuclear strike", "atomic bombing", "warhead detonated", "nuclear bomb detonated",
    ];
    // Whole-word tokens that reframe a use-phrase as a NON-use: threats, warnings,
    // hypotheticals, capability/posture statements, drills/tests, and averted/denied
    // events. ("may" is deliberately omitted — it collides with the month.)
    const NON_USE_TOKENS: &[&str] = &[
        "threat", "threats", "threaten", "threatens", "threatened", "threatening",
        "warn", "warns", "warning", "warned", "vow", "vows", "vowed",
        "could", "would", "might", "if", "risk", "risks", "fear", "fears", "feared",
        "ready", "prepared", "plan", "plans", "planning", "option", "options",
        "consider", "considers", "considering", "drill", "drills", "exercise",
        "exercises", "test", "tests", "testing", "capability", "capabilities",
        "doctrine", "posture", "simulate", "simulated", "simulation", "scenario",
        "scenarios", "deter", "deterrence", "deterrent", "preempt", "preemptive",
        "hypothetical", "fictional", "brink", "avert", "averted", "prevent",
        "prevented", "deny", "denies", "denied",
    ];
    tev.iter().any(|e| {
        if !e.nuclear_indicator { return false; }
        let t = e.title.to_lowercase();
        if !USE_PHRASES.iter().any(|p| t.contains(p)) { return false; }
        let non_use_framing = t
            .split(|c: char| !c.is_alphanumeric())
            .any(|w| NON_USE_TOKENS.contains(&w));
        !non_use_framing
    })
}

// ── Theater engine ───────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct TheaterEngine {
    /// Previous-tick heat per theater id, for trend/delta.
    prev_heat: HashMap<String, f64>,
}

/// Output bundle returned to the Bayesian engine each tick.
pub struct TheaterOutput {
    pub theaters:      Vec<TheaterState>,
    pub couplers:      SystemicCouplers,
    /// Systemic likelihood fed into the log-odds risk computation.
    pub l_sys:         f64,
    /// 0..100 escalation-ladder-aligned headline index.
    pub systemic_index: f64,
    pub driver:        String,
}

impl TheaterEngine {
    pub fn new() -> Self {
        Self { prev_heat: HashMap::new() }
    }

    pub fn compute(&mut self, events: &[GeopoliticalEvent]) -> TheaterOutput {
        // Partition the window into per-theater event sets (one clone per event).
        let mut by_theater: HashMap<&str, Vec<GeopoliticalEvent>> = HashMap::new();
        for e in events {
            if let Some(t) = &e.theater {
                by_theater.entry(t.as_str()).or_default().push(e.clone());
            }
        }

        let mut states: Vec<TheaterState> = Vec::new();
        for theater in Theater::primary() {
            let id  = theater.id();
            let tev = by_theater.get(id).map(|v| v.as_slice()).unwrap_or(&[]);
            states.push(self.score_theater(theater, tev));
        }

        // ── Couplers ──
        // Concurrency: fractional count of simultaneously-hot theaters.
        let concurrency: f64 = states.iter()
            .map(|s| smoothstep(s.heat, HOT_HEAT - HOT_RAMP, HOT_HEAT + HOT_RAMP))
            .sum();

        // Great-power entanglement: distinct great powers active across HOT theaters.
        let mut gp_set: std::collections::HashSet<&'static str> = std::collections::HashSet::new();
        for s in &states {
            if s.heat >= HOT_HEAT {
                for a in &s.top_actors {
                    if let Some(lbl) = great_power_label(a) { gp_set.insert(lbl); }
                }
            }
        }
        let gp_entanglement = (gp_set.len() as f64 / 3.0).min(1.0);

        // Alliance activation: any mutual-defense invocation in a hot theater.
        let alliance_activation = if states.iter().any(|s| s.alliance_invoked && s.heat >= HOT_HEAT) {
            1.0
        } else if states.iter().any(|s| s.alliance_invoked) {
            0.5
        } else {
            0.0
        };

        // The hottest theater drives the headline index and the systemic-likelihood
        // base intensity.
        let top = states.iter().max_by(|a, b| a.heat.partial_cmp(&b.heat).unwrap_or(std::cmp::Ordering::Equal));
        let (max_rung, top_label, top_heat) = match top {
            Some(s) => (s.rung, s.label.clone(), s.heat),
            None => (EscalationRung::Stable, String::new(), 0.0),
        };

        // Nuclear brink: a DIRECT nuclear-armed superpower confrontation WITHIN a
        // single theater (≥2 distinct great powers + extreme nuclear signaling) is the
        // apex systemic configuration — Cuba 1962 head-to-head, not three separate
        // regional wars. This is what lets single-theater intensity outweigh breadth.
        //
        // It is detected across ALL theaters, not just the hottest by raw heat. A
        // superpower nuclear standoff is the apex risk even when a concurrent
        // conventional war elsewhere carries more kinetic volume and would otherwise
        // win the "hottest" slot — a textbook Cuba-style brink has little kinetic
        // activity yet maximal nuclear danger, so pinning the test to the hottest
        // theater silently dropped the amplifier in exactly that configuration.
        //
        // The condition is the shared `theater_is_nuclear_brink` predicate, which the
        // I&W "nuclear-brink (apex)" indicator ALSO uses (indicators.rs). They share a
        // single threshold/great-power definition, so the headline amplifier and the
        // board light trip on exactly the same state and can never disagree about
        // whether the apex configuration is live.
        let brink = if states.iter().any(theater_is_nuclear_brink) { 1.0 } else { 0.0 };

        // Multipliers. Coupling rewards great-power entanglement; concurrency rewards
        // multiple simultaneously-hot theaters with DIMINISHING returns; brink is the
        // single-theater apex amplifier.
        let coupling_multiplier = 1.0 + 0.45 * gp_entanglement + 0.30 * alliance_activation;
        // Saturating breadth (recalibrated 2026-06-03): each extra hot theater adds less,
        // asymptoting at +26%. Previously linear (+0.12 per theater), which let a no-brink
        // FOUR-theater world (live 2026) drive l_sys ABOVE the Cuba nuclear-brink apex and
        // peg P(WWIII) flat at the 0.90 ceiling — breadth swamping the brink, the opposite
        // of the design intent. Saturating it lands that state at ~82% WITH resolution,
        // while quiet/ukraine/cuba (concurrency ≤ 1) are mathematically unchanged.
        let breadth          = (concurrency - 1.0).max(0.0);
        let concurrency_mult = 1.0 + 0.26 * (1.0 - (-breadth / 1.7).exp());
        let brink_mult       = 1.0 + 0.70 * brink;                           // single-theater apex

        let max_heat = top_heat;
        let l_sys = max_heat * brink_mult * coupling_multiplier * concurrency_mult;
        let within = within_band(top_heat, max_rung);
        // Forecast headline: saturate at 95, never 100 (see FORECAST_INDEX_CEILING). The
        // raw escalation-ladder position can hit 100 at the Systemic rung, but that rung is
        // news-inferred, not record-verified — so a forecast may read "very high" (95), never
        // "certain" (100). The scale itself stays 0–100 so 100 remains the visible terminal state.
        let systemic_index =
            (100.0 * (max_rung.level() as f64 + within) / 6.0).clamp(0.0, FORECAST_INDEX_CEILING);

        let hot_count = states.iter().filter(|s| s.heat >= HOT_HEAT).count();
        let driver = if top_heat < 0.06 {
            "No theater above baseline".to_string()
        } else {
            format!("{} at {}; {} theater{} hot",
                top_label, max_rung.label(), hot_count, if hot_count == 1 { "" } else { "s" })
        };

        let couplers = SystemicCouplers {
            gp_entanglement,
            alliance_activation,
            concurrency: (concurrency * 1e3).round() / 1e3,
            guardrail_collapse: 0.0, // set by the caller from the regime multiplier
            coupling_multiplier: (coupling_multiplier * concurrency_mult * brink_mult * 1e4).round() / 1e4,
        };

        TheaterOutput {
            theaters: states,
            couplers,
            l_sys: (l_sys * 1e6).round() / 1e6,
            systemic_index: (systemic_index * 1e2).round() / 1e2,
            driver,
        }
    }

    fn score_theater(&mut self, theater: Theater, tev: &[GeopoliticalEvent]) -> TheaterState {
        let id = theater.id().to_string();
        let prev = self.prev_heat.get(&id).copied().unwrap_or(0.0);

        if tev.is_empty() {
            // A theater that was hot last tick and now has zero qualifying events has
            // genuinely de-escalated — report that "falling" honestly rather than a flat
            // "stable", so a cooling flashpoint shows a ▼ on the ladder strip. (A fresh or
            // never-hot theater keeps prev≈0 → delta≈0 → "stable", so this only changes
            // the cool-off transition, never a quiet world. delta can only be ≤0 here.)
            let delta = 0.0 - prev;
            let trend = if delta < -0.005 { "falling" } else { "stable" };
            self.prev_heat.insert(id.clone(), 0.0);
            return TheaterState {
                theater_id: id, label: theater.label().to_string(),
                rung: EscalationRung::Stable, rung_label: EscalationRung::Stable.label().to_string(),
                heat: 0.0, modality_scores: HashMap::new(),
                trend: trend.into(), delta: (delta * 1e4).round() / 1e4, event_count: 0,
                gp_involved: false, alliance_invoked: false, top_actors: vec![],
            };
        }

        // Peak-aware modality scoring on this theater's events (fresh scorer; the
        // anomaly detector simply never fires with no cross-tick history).
        let mut scorer = DomainScorer::new();
        let scores = scorer.score_all(tev);

        let weighted: f64 = DOMAIN_WEIGHTS.iter()
            .map(|(m, _)| scores.get(*m).map(|d| d.score * domain_weight(m)).unwrap_or(0.0))
            .sum();
        // Intra-theater co-occurrence: simultaneous modalities within ONE theater
        // are far more dangerous than the same breadth spread across the globe.
        let soft_elev: f64 = scores.values()
            .map(|d| smoothstep(d.score, ELEVATION_THRESHOLD - ELEV_RAMP, ELEVATION_THRESHOLD + ELEV_RAMP))
            .sum();
        let cooc = co_occurrence_boost(soft_elev);
        let heat = ((weighted / max_weighted_sum()) * cooc).min(1.0);

        let gp_involved      = tev.iter().any(|e| e.great_power_involved);
        let alliance_invoked = tev.iter().any(|e| e.alliance_indicator);
        let wmd_used         = tev.iter().any(|e| e.wmd_indicator && e.severity > 0.6);
        let nuclear_used     = nuclear_use_in(tev);
        let rung = rung_for(heat, gp_involved, wmd_used, nuclear_used);

        let delta = heat - prev;
        let trend = if delta > 0.005 { "rising" } else if delta < -0.005 { "falling" } else { "stable" };
        self.prev_heat.insert(id.clone(), heat);

        // Dominant tracked actors in this theater (by mention count).
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for e in tev {
            for a in &e.actor_ids { *counts.entry(a.as_str()).or_insert(0) += 1; }
        }
        let mut pairs: Vec<(&str, usize)> = counts.into_iter().collect();
        pairs.sort_by(|a, b| b.1.cmp(&a.1));
        let top_actors: Vec<String> = pairs.into_iter().take(4).map(|(a, _)| a.to_string()).collect();

        let modality_scores: HashMap<String, f64> = DOMAIN_WEIGHTS.iter()
            .map(|(m, _)| (m.to_string(), scores.get(*m).map(|d| d.score).unwrap_or(0.0)))
            .collect();

        TheaterState {
            theater_id: id, label: theater.label().to_string(),
            rung, rung_label: rung.label().to_string(),
            heat: (heat * 1e4).round() / 1e4,
            modality_scores,
            trend: trend.to_string(), delta: (delta * 1e4).round() / 1e4,
            event_count: tev.len(),
            gp_involved, alliance_invoked, top_actors,
        }
    }
}

/// Fractional position of `heat` within its rung's heat band → [0,1].
fn within_band(heat: f64, rung: EscalationRung) -> f64 {
    let (lo, hi) = match rung {
        EscalationRung::Stable        => (0.0, 0.06),
        EscalationRung::Tension       => (0.06, 0.18),
        EscalationRung::Crisis        => (0.18, 0.38),
        EscalationRung::LimitedWar    => (0.38, 0.62),
        EscalationRung::GreatPowerWar => (0.62, 1.0),
        EscalationRung::Systemic      => (1.0, 1.0),
    };
    if hi <= lo { return 1.0; }
    ((heat - lo) / (hi - lo)).clamp(0.0, 1.0)
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::SourceTier;
    use chrono::Utc;

    fn ev(theater: &str, domain: &str, signal: f64, severity: f64, actors: &[&str], gp: bool) -> GeopoliticalEvent {
        let mut e = GeopoliticalEvent::new("Test headline event".into(), "src".into(), SourceTier::Tier1, Utc::now());
        e.theater = Some(theater.to_string());
        e.domain_signals = [(domain.to_string(), signal)].into_iter().collect();
        e.domain_tags = vec![domain.to_string()];
        e.severity = severity;
        e.escalation_language_score = 0.4;
        e.actor_ids = actors.iter().map(|s| s.to_string()).collect();
        e.great_power_involved = gp;
        e
    }

    #[test]
    fn empty_window_is_all_stable() {
        let mut te = TheaterEngine::new();
        let out = te.compute(&[]);
        assert_eq!(out.theaters.len(), 5);
        assert!(out.theaters.iter().all(|s| s.rung == EscalationRung::Stable));
        assert!(out.systemic_index < 1.0);
        assert!(out.l_sys.abs() < 1e-9);
    }

    #[test]
    fn hot_gulf_theater_drives_index_and_rung() {
        let mut te = TheaterEngine::new();
        let mut events = Vec::new();
        for _ in 0..6 {
            events.push(ev("us_iran", "military_escalation", 0.95, 0.9, &["united_states", "iran"], true));
            events.push(ev("us_iran", "nuclear_posture", 0.9, 0.9, &["iran"], false));
            events.push(ev("us_iran", "economic_warfare", 0.85, 0.7, &["iran"], false));
        }
        let out = te.compute(&events);
        let gulf = out.theaters.iter().find(|s| s.theater_id == "us_iran").unwrap();
        assert!(gulf.heat > 0.4, "gulf heat should be high, got {}", gulf.heat);
        assert!(gulf.rung.level() >= EscalationRung::LimitedWar.level(),
            "gulf should be at least Limited War, got {:?}", gulf.rung);
        assert!(out.systemic_index > 50.0, "index should be high, got {}", out.systemic_index);
        assert!(out.driver.contains("Iran"));
    }

    #[test]
    fn concurrency_raises_likelihood() {
        // Two hot theaters should produce more systemic likelihood than one.
        // Each theater needs enough multi-modality heat to clear the "hot" threshold.
        let strong = |theater: &'static str, a: &'static [&'static str]| -> Vec<GeopoliticalEvent> {
            let mut v = Vec::new();
            for _ in 0..6 {
                v.push(ev(theater, "military_escalation", 0.95, 0.9, a, true));
                v.push(ev(theater, "nuclear_posture",     0.90, 0.9, a, false));
                v.push(ev(theater, "economic_warfare",    0.85, 0.7, a, false));
            }
            v
        };
        let single = strong("us_iran", &["united_states", "iran"]);
        let mut both = single.clone();
        both.extend(strong("nato_russia", &["russia", "nato"]));
        let mut te1 = TheaterEngine::new();
        let mut te2 = TheaterEngine::new();
        let o1 = te1.compute(&single);
        let o2 = te2.compute(&both);
        assert!(o2.l_sys > o1.l_sys, "two hot theaters {} should exceed one {}", o2.l_sys, o1.l_sys);
        assert!(o2.couplers.concurrency > o1.couplers.concurrency);
    }

    #[test]
    fn cooling_theater_reports_falling_not_stable() {
        // A theater that is hot on one tick and has no qualifying events the next has
        // de-escalated; the trend must read "falling" (▼), not a misleading "stable".
        let mut te = TheaterEngine::new();
        let mut hot = Vec::new();
        for _ in 0..6 {
            hot.push(ev("us_iran", "military_escalation", 0.95, 0.9, &["united_states", "iran"], true));
            hot.push(ev("us_iran", "nuclear_posture",     0.90, 0.9, &["iran"], false));
        }
        let o1 = te.compute(&hot);
        let g1 = o1.theaters.iter().find(|s| s.theater_id == "us_iran").unwrap();
        assert!(g1.heat > 0.1, "precondition: theater should be hot, got {}", g1.heat);

        // Next tick: the window holds no events for this theater — it has cooled off.
        let o2 = te.compute(&[]);
        let g2 = o2.theaters.iter().find(|s| s.theater_id == "us_iran").unwrap();
        assert_eq!(g2.heat, 0.0, "cooled theater heat should be 0");
        assert_eq!(g2.trend, "falling",
            "a theater that cooled from hot to zero must read falling, not stable");
        assert!(g2.delta < 0.0, "delta should be negative on cool-off, got {}", g2.delta);
    }

    #[test]
    fn quiet_world_theaters_stay_stable() {
        // Symmetric guard: a fresh/never-hot theater must NOT spuriously read "falling".
        let mut te = TheaterEngine::new();
        let out = te.compute(&[]);
        assert!(out.theaters.iter().all(|s| s.trend == "stable"),
            "a never-hot world must read stable, not falling");
        assert!(out.theaters.iter().all(|s| s.delta == 0.0));
    }

    #[test]
    fn brink_fires_in_a_non_hottest_theater() {
        // The nuclear-brink amplifier must engage when ANY single theater is a
        // superpower nuclear standoff (≥2 great powers + nuclear posture ≥0.78) —
        // even if a different, purely-conventional theater has more raw heat and is
        // the "hottest". (Cuba 1962 had near-zero kinetic activity yet maximal
        // nuclear danger.) The old code only inspected the hottest theater, so this
        // configuration silently lost the ~1.70× brink multiplier.
        //
        // Two worlds, identical except for whether the cooler theater is a 2-power
        // brink, are compared so concurrency / coupling / heat are held constant and
        // the l_sys ratio isolates brink_mult alone.
        let conventional_hottest = || {
            // Multi-modality conventional war, one great power (US; Iran is not a GP
            // label) and NO nuclear → never a brink itself, but the hottest by heat.
            let mut v = Vec::new();
            for _ in 0..6 {
                v.push(ev("us_iran", "military_escalation", 1.0, 0.9, &["united_states", "iran"], true));
                v.push(ev("us_iran", "economic_warfare",    0.9, 0.9, &["united_states", "iran"], true));
                v.push(ev("us_iran", "cyber_info_ops",      0.85, 0.9, &["united_states", "iran"], true));
                v.push(ev("us_iran", "diplomatic_breakdown",0.85, 0.9, &["united_states", "iran"], true));
            }
            v
        };
        // Cooler theater whose heat comes only from extreme nuclear posture.
        let nuclear_theater = |actors: &[&str]| {
            let mut v = Vec::new();
            for _ in 0..6 {
                let mut e = ev("nato_russia", "nuclear_posture", 1.0, 1.0, actors, true);
                e.escalation_language_score = 0.8; // push the nuclear modality score past 0.78
                v.push(e);
            }
            v
        };

        // Brink world: the cooler theater is a US–Russia nuclear standoff (2 GP).
        let mut brink_world = conventional_hottest();
        brink_world.extend(nuclear_theater(&["united_states", "russia"]));
        // Control world: identical, but the cooler theater has only ONE great power
        // (Russia), so it is NOT a brink anywhere. gp_entanglement is unchanged
        // because the conventional theater already contributes "us".
        let mut control_world = conventional_hottest();
        control_world.extend(nuclear_theater(&["russia"]));

        let mut te_b = TheaterEngine::new();
        let mut te_c = TheaterEngine::new();
        let o_brink   = te_b.compute(&brink_world);
        let o_control = te_c.compute(&control_world);

        // Precondition: the brink theater must NOT be the hottest — otherwise the old
        // hottest-only logic would have caught it and this test wouldn't lock the fix.
        let hottest = o_brink.theaters.iter()
            .max_by(|a, b| a.heat.partial_cmp(&b.heat).unwrap()).unwrap();
        assert_eq!(hottest.theater_id, "us_iran",
            "precondition: the conventional theater must be hottest, got {} ({})",
            hottest.theater_id, hottest.heat);
        let nuc = o_brink.theaters.iter().find(|t| t.theater_id == "nato_russia").unwrap();
        assert!(nuc.heat < hottest.heat, "precondition: brink theater must be cooler");
        assert!(nuc.modality_scores.get("nuclear_posture").copied().unwrap_or(0.0) >= 0.78,
            "precondition: nuclear posture must clear the 0.78 brink threshold, got {:?}",
            nuc.modality_scores.get("nuclear_posture"));

        // The two worlds differ ONLY by the brink, so the l_sys ratio is brink_mult
        // (1 + 0.70 = 1.70). Under the old hottest-only logic both would read brink=0
        // and the ratio would be 1.0 — so this assertion fails on the bug and passes
        // on the fix.
        assert!(o_control.l_sys > 0.0 && o_brink.l_sys > 0.0);
        let ratio = o_brink.l_sys / o_control.l_sys;
        assert!((1.6..=1.8).contains(&ratio),
            "brink in a non-hottest theater should raise l_sys by ~1.70×, got ratio {ratio} \
             (brink l_sys={}, control l_sys={})", o_brink.l_sys, o_control.l_sys);
    }

    #[test]
    fn nuclear_use_forces_systemic_rung() {
        let mut te = TheaterEngine::new();
        let mut e = ev("us_iran", "nuclear_posture", 1.0, 1.0, &["iran"], false);
        e.title = "Nuclear detonation confirmed over military target".into();
        e.nuclear_indicator = true;
        let out = te.compute(&[e]);
        let gulf = out.theaters.iter().find(|s| s.theater_id == "us_iran").unwrap();
        assert_eq!(gulf.rung, EscalationRung::Systemic);
    }

    #[test]
    fn nuclear_threat_does_not_force_systemic_rung() {
        // A THREAT to use nuclear weapons is not nuclear USE. The apex Systemic rung
        // (which pegs the headline at 95 and floods P(WWIII)) must trip only on a real
        // detonation, never on sabre-rattling — otherwise the single most common
        // nuclear-headline genre would over-claim catastrophe. The title contains the
        // "nuclear strike" use-phrase AND nuclear_indicator is set, so the OLD plain
        // substring match would have forced Systemic here; the strict-use guard must
        // catch the threat framing ("threatens", "if") and decline.
        let mut te = TheaterEngine::new();
        let mut e = ev("nato_russia", "nuclear_posture", 1.0, 1.0, &["russia", "united_states"], true);
        e.title = "Russia threatens nuclear strike if NATO intervenes".into();
        e.nuclear_indicator = true;
        let out = te.compute(&[e]);
        let t = out.theaters.iter().find(|s| s.theater_id == "nato_russia").unwrap();
        assert_ne!(t.rung, EscalationRung::Systemic,
            "a nuclear THREAT must not force the systemic (nuclear-use) rung");
    }

    #[test]
    fn nuclear_test_does_not_force_systemic_rung() {
        // An underground nuclear TEST detonates a device but is not use-in-war. The
        // title carries the "nuclear detonation" use-phrase (so the old match would
        // fire) but is reframed by the whole-word "test" token — and crucially the
        // "latest" substring must NOT be mistaken for "test".
        let mut te = TheaterEngine::new();
        let mut e = ev("us_china_taiwan", "nuclear_posture", 1.0, 1.0, &["china"], false);
        e.title = "North Korea nuclear detonation in latest weapons test".into();
        e.nuclear_indicator = true;
        let out = te.compute(&[e]);
        let t = out.theaters.iter().find(|s| s.theater_id == "us_china_taiwan").unwrap();
        assert_ne!(t.rung, EscalationRung::Systemic,
            "a nuclear TEST detonation must not force the systemic (nuclear-use) rung");
    }

    #[test]
    fn real_nuclear_use_still_fires_despite_noisy_window() {
        // Recall guard: a real detonation spawns many headlines, some of which carry
        // threat/test framing. `any()` over the window must still trip Systemic as long
        // as one clean confirmation is present, so the strict guard cannot mute a true
        // event just because neighbouring coverage is hedged.
        let mut te = TheaterEngine::new();
        let mut threat = ev("nato_russia", "nuclear_posture", 1.0, 1.0, &["russia", "united_states"], true);
        threat.title = "Analysts warn a nuclear strike could follow".into();
        threat.nuclear_indicator = true;
        let mut confirmed = ev("nato_russia", "nuclear_posture", 1.0, 1.0, &["russia", "united_states"], true);
        confirmed.title = "A nuclear weapon was used; nuclear detonation confirmed".into();
        confirmed.nuclear_indicator = true;
        let out = te.compute(&[threat, confirmed]);
        let t = out.theaters.iter().find(|s| s.theater_id == "nato_russia").unwrap();
        assert_eq!(t.rung, EscalationRung::Systemic,
            "one clean confirmation in the window must still force the systemic rung");
    }
}
