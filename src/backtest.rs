// ------------------------------------------------------------
// RAiTHE INDUSTRIES INCORPORATED
// Copyright (c) 2026 All Rights Reserved.
//
// This file is part of a proprietary system. Unauthorized use,
// reproduction, or distribution is strictly prohibited.
// ------------------------------------------------------------

// src/backtest.rs — Calibration backtest harness (GCRM v2, Phase 3)
//
// Replays synthetic historical analogs through the full Bayesian/theater engine and
// checks that P(WWIII) lands in the target bands Robert set, AND that the ordering
// quiet < Ukraine-2022 < current-2026 < Cuba-1962 holds. This is the discipline that
// makes the headline defensible: the constants in theater.rs / bayesian.rs / models.rs
// are fitted here, not guessed. The whole module is test-only.
//
// Target bands (Robert, 2026-05-31). The curve is fitted so the LIVE corpus (a
// fragile-ceasefire 3-theater world) reads ~45%; the idealised full-intensity analogs
// therefore ride higher than the live number:
//   quiet world          ~  2%
//   Ukraine Feb 2022     ~ 39%   (one theater at full war)
//   current-2026 (full)  ~ 65%   (idealised — live corpus reads ~45%, see scratch verify)
//   Cuba Oct 1962        ~ 80%   (direct US–USSR nuclear brink — the apex, near ceiling)

use chrono::Utc;

use crate::bayesian::BayesianRiskEngine;
use crate::models::{GeopoliticalEvent, RegimeFactor, RiskSnapshot, SourceTier};

/// (modality_id, nlp_signal, severity)
type Spec = (&'static str, f64, f64);

/// Build `per` events for each (modality, signal, severity) spec in one theater.
fn evset(theater: &str, specs: &[Spec], actors: &[&str], gp: bool, per: usize) -> Vec<GeopoliticalEvent> {
    let mut v = Vec::new();
    for (m, sig, sev) in specs {
        for _ in 0..per {
            let mut e = GeopoliticalEvent::new(
                "Analog backtest event headline text".into(),
                "wire".into(), SourceTier::Tier1, Utc::now(),
            );
            e.theater                   = Some(theater.to_string());
            e.domain_signals            = [(m.to_string(), *sig)].into_iter().collect();
            e.domain_tags               = vec![m.to_string()];
            e.severity                  = *sev;
            e.escalation_language_score = 0.7;
            e.sentiment_score           = -0.7; // hostile
            e.actor_ids                 = actors.iter().map(|s| s.to_string()).collect();
            e.great_power_involved      = gp;
            if *m == "nuclear_posture" { e.nuclear_indicator = true; }
            v.push(e);
        }
    }
    v
}

fn regime(mult: f64) -> Vec<RegimeFactor> {
    vec![RegimeFactor { id: "era".into(), label: "era structural".into(), multiplier: mult, active: true }]
}

fn run(regime_mult: f64, events: &[GeopoliticalEvent]) -> RiskSnapshot {
    let mut eng = BayesianRiskEngine::new(regime(regime_mult), 0.025, 0.08);
    eng.compute(events)
}

// ── Analog scenarios ─────────────────────────────────────────────────────────────

fn quiet() -> (f64, Vec<GeopoliticalEvent>) {
    // Calm modern year: minor diplomatic/cyber friction, no great-power war.
    let ev = evset("us_china_taiwan",
        &[("diplomatic_breakdown", 0.55, 0.35), ("cyber_info_ops", 0.50, 0.30)],
        &["china", "taiwan"], false, 4);
    (1.3, ev)
}

fn ukraine_2022() -> (f64, Vec<GeopoliticalEvent>) {
    // One theater very hot: invasion + sanctions + nuclear sabre-rattling. NATO backs
    // Ukraine but is not a direct combatant; no direct superpower nuclear brink.
    let ev = evset("nato_russia",
        &[("military_escalation", 0.95, 0.92), ("nuclear_posture", 0.72, 0.80),
          ("economic_warfare", 0.90, 0.70),   ("diplomatic_breakdown", 0.85, 0.70)],
        &["russia", "ukraine", "nato", "united_states"], true, 8);
    (3.4, ev)
}

fn current_2026() -> (f64, Vec<GeopoliticalEvent>) {
    // Three concurrent hot theaters; great powers fighting in SEPARATE theaters
    // (US in the Gulf, Russia in Ukraine, China posture over Taiwan) — breadth, not a
    // single direct superpower nuclear brink.
    let mut ev = evset("us_iran",
        &[("military_escalation", 0.92, 0.90), ("diplomatic_breakdown", 0.85, 0.70),
          ("economic_warfare", 0.85, 0.70),    ("nuclear_posture", 0.55, 0.60)],
        &["united_states", "iran", "israel"], true, 8);
    ev.extend(evset("nato_russia",
        &[("military_escalation", 0.85, 0.85), ("nuclear_posture", 0.70, 0.70),
          ("diplomatic_breakdown", 0.80, 0.70), ("economic_warfare", 0.70, 0.60)],
        &["russia", "ukraine", "nato"], true, 8));
    ev.extend(evset("us_china_taiwan",
        &[("military_escalation", 0.70, 0.70), ("diplomatic_breakdown", 0.50, 0.50)],
        &["china", "taiwan", "united_states"], true, 6));
    (5.46, ev)
}

fn cuba_1962() -> (f64, Vec<GeopoliticalEvent>) {
    // Direct US–USSR nuclear brink in ONE theater: extreme nuclear signaling + naval
    // blockade + ultimatum, both superpowers head-to-head → nuclear-brink apex.
    let ev = evset("nato_russia",
        &[("nuclear_posture", 0.97, 1.00), ("military_escalation", 0.90, 0.90),
          ("diplomatic_breakdown", 0.92, 0.85)],
        &["united_states", "russia"], true, 10);
    (2.6, ev)
}

fn p_of(scn: (f64, Vec<GeopoliticalEvent>)) -> f64 {
    run(scn.0, &scn.1).p_wwiii_annual
}

// ── Tests ────────────────────────────────────────────────────────────────────────

/// Readout of the calibrated bands. Run with `cargo test calibration_readout -- --nocapture`.
#[test]
fn calibration_readout() {
    let scenarios: [(&str, (f64, Vec<GeopoliticalEvent>)); 4] = [
        ("quiet",        quiet()),
        ("ukraine_2022", ukraine_2022()),
        ("current_2026", current_2026()),
        ("cuba_1962",    cuba_1962()),
    ];
    eprintln!("\n── GCRM v2 calibration backtest ──");
    for (name, scn) in scenarios {
        let rm = scn.0;
        let snap = run(rm, &scn.1);
        let c = &snap.couplers;
        eprintln!("{:14} P={:6.2}%  idx={:5.1}  L_sys={:.3}  | coupling×{:.2} conc={:.2} gp={:.2} guard={:.2}  [{}]",
            name, snap.p_wwiii_annual * 100.0, snap.systemic_index,
            snap.likelihood_ratio, c.coupling_multiplier, c.concurrency,
            c.gp_entanglement, c.guardrail_collapse, snap.driver);
    }
    eprintln!();
}

#[test]
fn ordering_holds() {
    let q = p_of(quiet());
    let u = p_of(ukraine_2022());
    let c = p_of(current_2026());
    let k = p_of(cuba_1962());
    assert!(q < u, "quiet {q:.4} must be < ukraine {u:.4}");
    assert!(u < c, "ukraine {u:.4} must be < current {c:.4}");
    assert!(c < k, "current {c:.4} must be < cuba {k:.4}");
}

#[test]
fn bands_quiet() {
    let q = p_of(quiet());
    assert!(q > 0.005 && q < 0.05, "quiet should be ~2% (0.5–5%), got {:.3}%", q * 100.0);
}

#[test]
fn bands_ukraine() {
    let u = p_of(ukraine_2022());
    assert!(u > 0.30 && u < 0.50, "ukraine (one full-war theater) should be ~39%, got {:.3}%", u * 100.0);
}

#[test]
fn bands_current_full() {
    // Idealised full-intensity current world. The LIVE corpus (fragile ceasefire)
    // reads ~45% — verified on the scratch instance, not in this unit test.
    let c = p_of(current_2026());
    assert!(c > 0.55 && c < 0.75, "current-full should be ~65%, got {:.3}%", c * 100.0);
}

#[test]
fn bands_cuba() {
    let k = p_of(cuba_1962());
    assert!(k > 0.70 && k < 0.90, "cuba (nuclear-brink apex) should be ~80%, got {:.3}%", k * 100.0);
}
