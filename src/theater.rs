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

/// Strict detection of actual nuclear *use* (not posture/threats/talks).
fn nuclear_use_in(tev: &[GeopoliticalEvent]) -> bool {
    const USE_PHRASES: &[&str] = &[
        "nuclear detonation", "nuclear weapon used", "nuclear strike",
        "atomic bombing", "warhead detonated",
    ];
    tev.iter().any(|e| {
        e.nuclear_indicator && {
            let t = e.title.to_lowercase();
            USE_PHRASES.iter().any(|p| t.contains(p))
        }
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

        // The hottest theater drives both the headline index and the systemic
        // likelihood. Pull its nuclear signal and great-power count for the brink test.
        let top = states.iter().max_by(|a, b| a.heat.partial_cmp(&b.heat).unwrap_or(std::cmp::Ordering::Equal));
        let (max_rung, top_label, top_heat, top_nuclear, top_gp) = match top {
            Some(s) => {
                let gp = s.top_actors.iter()
                    .filter_map(|a| great_power_label(a))
                    .collect::<std::collections::HashSet<_>>().len();
                (s.rung, s.label.clone(), s.heat,
                 s.modality_scores.get("nuclear_posture").copied().unwrap_or(0.0), gp)
            }
            None => (EscalationRung::Stable, String::new(), 0.0, 0.0, 0),
        };

        // Nuclear brink: a DIRECT nuclear-armed superpower confrontation in the
        // hottest theater (≥2 great powers + extreme nuclear signaling) is the apex
        // systemic configuration — Cuba 1962 head-to-head, not three separate
        // regional wars. This is what lets single-theater intensity outweigh breadth.
        let brink = if top_nuclear >= 0.78 && top_gp >= 2 { 1.0 } else { 0.0 };

        // Multipliers. Coupling rewards great-power entanglement; concurrency rewards
        // multiple simultaneously-hot theaters (modestly, so breadth does not swamp a
        // single nuclear brink); brink is the apex amplifier.
        let coupling_multiplier = 1.0 + 0.45 * gp_entanglement + 0.30 * alliance_activation;
        let concurrency_mult    = 1.0 + 0.12 * (concurrency - 1.0).max(0.0); // breadth: modest
        let brink_mult          = 1.0 + 0.70 * brink;                        // single-theater apex

        let max_heat = top_heat;
        let l_sys = max_heat * brink_mult * coupling_multiplier * concurrency_mult;
        let within = within_band(top_heat, max_rung);
        let systemic_index = (100.0 * (max_rung.level() as f64 + within) / 6.0).clamp(0.0, 100.0);

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
            self.prev_heat.insert(id.clone(), 0.0);
            return TheaterState {
                theater_id: id, label: theater.label().to_string(),
                rung: EscalationRung::Stable, rung_label: EscalationRung::Stable.label().to_string(),
                heat: 0.0, modality_scores: HashMap::new(),
                trend: "stable".into(), delta: 0.0, event_count: 0,
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
    fn nuclear_use_forces_systemic_rung() {
        let mut te = TheaterEngine::new();
        let mut e = ev("us_iran", "nuclear_posture", 1.0, 1.0, &["iran"], false);
        e.title = "Nuclear detonation confirmed over military target".into();
        e.nuclear_indicator = true;
        let out = te.compute(&[e]);
        let gulf = out.theaters.iter().find(|s| s.theater_id == "us_iran").unwrap();
        assert_eq!(gulf.rung, EscalationRung::Systemic);
    }
}
