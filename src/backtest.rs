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
// are fitted here, not guessed. Most of it runs under `cargo test`; `calibration_evidence_html()`
// is ALSO called at runtime to render the live calibration readout on the methodology page.
//
// Target bands (Robert, 2026-05-31; current-full re-anchored 2026-06-09 — see below). The
// idealised full-intensity analogs are scored against these expert-set CENTRES by the
// calibration evidence harness at the foot of this file (Brier / cross-entropy):
//   quiet world          ~  2%
//   Ukraine Feb 2022     ~ 39%   (one theater at full war)
//   current-2026 (full)  ~ 60%   (idealised 3-theater, no direct brink; re-anchored from
//                                  ~65% to reflect the 2026-06-03 brink>breadth fix, which
//                                  deliberately lowered broad, no-brink scenarios)
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

#[cfg(test)] // watch-only analog (no hard band); used only by the calibration_readout test
fn live_hot_2026() -> (f64, Vec<GeopoliticalEvent>) {
    // The live 2026-06 corpus: FOUR concurrent hot theaters with one (US–China/Taiwan)
    // escalated to the Great-Power-War rung. Breadth PLUS an active great-power war —
    // but still no single direct US–Russia nuclear-brink ultimatum à la Cuba. Robert's
    // target: ~82% (near-apex, off the 0.90 ceiling, with resolution — not a flat peg).
    // NB: "great_power_conflict" is NOT a v2 scored modality (it was folded into the
    // gp coupler), so listing it added ZERO heat — the great-power rung is conveyed by
    // gp=true + actor_ids below, which feed the coupler. Use only real scored modalities
    // here so the analog's modeled heat matches its stated near-apex intent.
    let mut ev = evset("us_china_taiwan",
        &[("military_escalation", 0.95, 0.92), ("economic_warfare", 0.88, 0.80),
          ("nuclear_posture", 0.62, 0.65),     ("diplomatic_breakdown", 0.85, 0.75)],
        &["china", "united_states", "taiwan"], true, 9);
    ev.extend(evset("us_iran",
        &[("military_escalation", 0.92, 0.90), ("economic_warfare", 0.85, 0.70),
          ("diplomatic_breakdown", 0.85, 0.70), ("nuclear_posture", 0.55, 0.60)],
        &["united_states", "iran", "israel"], true, 8));
    ev.extend(evset("nato_russia",
        &[("military_escalation", 0.85, 0.85), ("nuclear_posture", 0.70, 0.70),
          ("diplomatic_breakdown", 0.80, 0.70)],
        &["russia", "ukraine", "nato"], true, 8));
    ev.extend(evset("india_pakistan",
        &[("military_escalation", 0.62, 0.60), ("diplomatic_breakdown", 0.55, 0.50)],
        &["india", "pakistan"], false, 5));
    (5.46, ev)
}

#[cfg(test)] // watch/measurement analog (no hard band) — reproduces the LIVE railed peg
fn live_pegged_2026() -> (f64, Vec<GeopoliticalEvent>) {
    // Reproduces the LIVE 2026-06 RAILED operating point that the four band analogs miss:
    // FIVE concurrent hot theaters, great powers entangled across ≥3, a live Article-5 /
    // alliance invocation, and the seeded acute regime (≈5.46×) — so EVERY l_sys input sits
    // at its rail (top-theater heat clamped at 1.0, concurrency≈5, gp_entanglement=1,
    // alliance_activation=1, guardrail_collapse=1) and P pegs ~83% with ~zero local slope.
    // Still NO single direct US–Russia nuclear brink (that apex stays Cuba's). DECISION
    // 2026-06-25 (Robert): this railed read is an ACCEPTED structural maximum, NOT a defect to
    // de-saturate — empirically it still responds to decay (−20pp by 72h) and to a brink emerging
    // (→ the 0.90 apex); it is only flat to incremental escalation while already maxed-without-a-
    // brink, which is honest. The treatment is the `couplers.breadth_saturated` disclosure, not a
    // recalibration. This analog DOCUMENTS that operating point; bands_* never exercise it
    // (resolved region, L_sys ≤ 2.32). Measurement only.
    let mut ev = evset("us_china_taiwan",
        &[("military_escalation", 0.95, 0.92), ("economic_warfare", 0.88, 0.80),
          ("nuclear_posture", 0.62, 0.65),     ("diplomatic_breakdown", 0.88, 0.78)],
        &["china", "united_states", "taiwan"], true, 10);
    // NATO–Russia carries a live alliance (Article-5) signal → alliance_activation = 1.0.
    let mut nr = evset("nato_russia",
        &[("military_escalation", 0.90, 0.88), ("nuclear_posture", 0.72, 0.72),
          ("diplomatic_breakdown", 0.85, 0.75), ("economic_warfare", 0.78, 0.66)],
        &["russia", "ukraine", "nato", "united_states"], true, 9);
    for e in &mut nr { e.alliance_indicator = true; }
    ev.extend(nr);
    ev.extend(evset("us_iran",
        &[("military_escalation", 0.92, 0.90), ("economic_warfare", 0.85, 0.72),
          ("diplomatic_breakdown", 0.85, 0.72), ("nuclear_posture", 0.55, 0.60)],
        &["united_states", "iran", "israel"], true, 8));
    ev.extend(evset("india_pakistan",
        &[("military_escalation", 0.80, 0.76), ("diplomatic_breakdown", 0.66, 0.60)],
        &["india", "pakistan"], false, 6));
    ev.extend(evset("korea",
        &[("military_escalation", 0.70, 0.66), ("diplomatic_breakdown", 0.58, 0.52)],
        &["north_korea", "united_states"], true, 5));
    (5.46, ev)
}

/// A copy of [`live_pegged_2026`] with ONE theater's conventional intensity nudged by `d`
/// (severity + signal), staying in the no-brink breadth regime. DOCUMENTS the accepted
/// structural-max behaviour: P does not move between `nudged(+)` and `nudged(−)` because the
/// world is already maxed-without-a-brink (per the 2026-06-25 decision that flatness is honest,
/// surfaced via `breadth_saturated`, not a defect). Never adds a nuclear brink (a separate apex
/// lever), so it isolates incremental-escalation response, not the brink jump.
#[cfg(test)]
fn live_pegged_nudged(d: f64) -> (f64, Vec<GeopoliticalEvent>) {
    let (rm, mut ev) = live_pegged_2026();
    for e in &mut ev {
        if e.theater.as_deref() == Some("us_iran") && e.domain_tags.iter().any(|t| t == "military_escalation") {
            e.severity = (e.severity + d).clamp(0.0, 1.0);
            if let Some(s) = e.domain_signals.get_mut("military_escalation") {
                *s = (*s + d).clamp(0.0, 1.0);
            }
        }
    }
    (rm, ev)
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

/// Re-date every event in a scenario to `hours` ago. The four `bands_*` analogs all score
/// at peak freshness (Utc::now()), so the decay half-lives were never exercised by the
/// harness — yet the live model's read moves continuously as events age between news bursts.
/// This lets the diurnal-robustness lock score the SAME conflict corpus after a news lull
/// (an overnight gap with no fresh events) to verify the systemic read does not collapse on
/// a quiet cycle while the underlying wars are unchanged.
#[cfg(test)]
fn aged(scn: (f64, Vec<GeopoliticalEvent>), hours: i64) -> (f64, Vec<GeopoliticalEvent>) {
    let (rm, mut ev) = scn;
    let t = Utc::now() - chrono::Duration::hours(hours);
    for e in &mut ev { e.published_at = t; }
    (rm, ev)
}

/// Stamp every event with a strongly de-escalatory escalation_step (a ceasefire / peace deal,
/// −1 … +1 scale). Used to verify the persistence floor RELEASES on genuine de-escalation
/// evidence rather than being propped up by mere silence (silence ≠ peace).
#[cfg(test)]
fn deescalated(scn: (f64, Vec<GeopoliticalEvent>)) -> (f64, Vec<GeopoliticalEvent>) {
    let (rm, mut ev) = scn;
    for e in &mut ev { e.escalation_step = -0.7; }
    (rm, ev)
}

// ── Tests ────────────────────────────────────────────────────────────────────────

/// Readout of the calibrated bands. Run with `cargo test calibration_readout -- --nocapture`.
#[test]
fn calibration_readout() {
    let scenarios: [(&str, (f64, Vec<GeopoliticalEvent>)); 6] = [
        ("quiet",          quiet()),
        ("ukraine_2022",   ukraine_2022()),
        ("current_2026",   current_2026()),
        ("live_hot_2026",  live_hot_2026()),
        ("live_pegged_26", live_pegged_2026()),
        ("cuba_1962",      cuba_1962()),
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

/// Measured readout of the LIVE railed peg and its local slope. Run:
///   cargo test pegged_resolution_readout -- --nocapture
/// Documents the accepted structural max: at the railed operating point a ±intensity nudge moves
/// P by ~nothing (Δ≈0pp) because the world is already maxed-without-a-brink. Per the 2026-06-25
/// decision this is honest (surfaced via `couplers.breadth_saturated`), not a defect to fix.
/// Pure measurement; no assertion.
#[test]
fn pegged_resolution_readout() {
    let base = run(live_pegged_2026().0, &live_pegged_2026().1);
    let c = &base.couplers;
    eprintln!("\n── GCRM live railed-peg resolution probe ──");
    eprintln!("pegged       P={:6.2}%  idx={:5.1}  L_sys={:.3}  | coupling×{:.2} conc={:.2} gp={:.2} alliance={:.2} guard={:.2}  topHeat={:.4}  [{}]",
        base.p_wwiii_annual * 100.0, base.systemic_index, base.likelihood_ratio,
        c.coupling_multiplier, c.concurrency, c.gp_entanglement, c.alliance_activation,
        c.guardrail_collapse,
        base.theaters.iter().map(|t| t.heat).fold(0.0_f64, f64::max),
        base.driver);
    for d in [-0.30_f64, -0.15, 0.0, 0.15, 0.30] {
        let s = live_pegged_nudged(d);
        let snap = run(s.0, &s.1);
        eprintln!("nudge {:+.2}   P={:6.2}%  L_sys={:.3}   ΔP_vs_base={:+.3}pp",
            d, snap.p_wwiii_annual * 100.0, snap.likelihood_ratio,
            (snap.p_wwiii_annual - base.p_wwiii_annual) * 100.0);
    }
    let hot  = { let s = live_pegged_nudged(0.30);  run(s.0, &s.1).p_wwiii_annual };
    let cool = { let s = live_pegged_nudged(-0.30); run(s.0, &s.1).p_wwiii_annual };
    eprintln!("→ incremental-escalation response (P[+0.30] − P[−0.30]) = {:+.3}pp  (≈0 = accepted structural max)\n",
        (hot - cool) * 100.0);
}

/// Measured readout of the news-lull behavior. Run:
///   cargo test diurnal_readout -- --nocapture
#[test]
fn diurnal_readout() {
    eprintln!("\n── GCRM diurnal robustness (current_2026 aged across a news lull) ──");
    for h in [0i64, 6, 12, 18, 24, 48, 72, 168] {
        let s = aged(current_2026(), h);
        let snap = run(s.0, &s.1);
        eprintln!("age {:>4}h  P={:6.2}%  L_sys={:.3}  idx={:5.1}",
            h, snap.p_wwiii_annual * 100.0, snap.likelihood_ratio, snap.systemic_index);
    }
    eprintln!();
}

/// Measured readout of the persistence floor: a silent war (floor HOLDS) vs the same corpus
/// carrying de-escalation evidence (floor RELEASED), across ages. Run:
///   cargo test persistence_floor_readout -- --nocapture
#[test]
fn persistence_floor_readout() {
    eprintln!("\n── GCRM persistence floor (current_2026): silent war (floor holds) vs de-escalation (released) ──");
    for h in [0i64, 12, 24, 48, 72, 96, 168, 336] {
        let silent = { let s = aged(current_2026(), h); run(s.0, &s.1) };
        let deesc  = { let s = aged(deescalated(current_2026()), h); run(s.0, &s.1) };
        eprintln!("age {:>4}h   silent P={:6.2}% (L={:.3})    de-escalating P={:6.2}% (L={:.3})",
            h, silent.p_wwiii_annual * 100.0, silent.likelihood_ratio,
            deesc.p_wwiii_annual * 100.0, deesc.likelihood_ratio);
    }
    eprintln!();
}

#[test]
fn diurnal_robustness_active_war_survives_a_news_lull() {
    // REALISM LOCK (2026-06-21). A systemic-war ANNUAL probability must not swing on a single
    // quiet news cycle. Before the fix, military escalation decayed with a 24h half-life, so a
    // half-day with no fresh kinetic events halved the dominant signal and the live read sagged
    // ~10pp overnight even though three great-power wars were unchanged on the ground — the model
    // was tracking news VOLUME, not conflict STATE. With the corrected 72h sustained-state
    // half-life the systemic likelihood is essentially flat across an overnight lull, and only
    // genuine multi-day silence (a real de-escalation) cools it. This is the discipline the four
    // `bands_*` tests lacked — they all score at peak freshness, never exercising the decay.
    // Scope: the LULL window (≤24h). The multi-day tail (hold vs de-escalation release) is the
    // persistence floor's job and is locked by the `persistence_floor_*` tests below.
    let fresh = run(current_2026().0, &current_2026().1);
    let lull  = { let s = aged(current_2026(), 12);  run(s.0, &s.1) };  // overnight
    let day   = { let s = aged(current_2026(), 24);  run(s.0, &s.1) };  // a full quiet day

    // (1) An overnight lull keeps the systemic likelihood ≥93% intact and the headline P inside
    //     its calibration band — the model no longer confuses a quiet night with peace. Under the
    //     old 24h half-life a 12h lull dropped l_sys far more, so this bound genuinely pins the fix.
    assert!(lull.likelihood_ratio >= 0.93 * fresh.likelihood_ratio,
        "12h lull must retain ≥93% of l_sys; fresh={:.3} lull={:.3} ({:.0}%)",
        fresh.likelihood_ratio, lull.likelihood_ratio,
        100.0 * lull.likelihood_ratio / fresh.likelihood_ratio);
    assert!(lull.p_wwiii_annual > 0.55 && lull.p_wwiii_annual < 0.75,
        "12h lull must stay in the current-world band (0.55–0.75), got {:.2}%", lull.p_wwiii_annual * 100.0);
    assert!(lull.p_wwiii_annual > fresh.p_wwiii_annual - 0.03,
        "a 12h news lull must not drop P by >3pp; fresh={:.3} lull={:.3}",
        fresh.p_wwiii_annual, lull.p_wwiii_annual);

    // (2) Decay is still ALIVE in the lull window (not a frozen floor): a longer lull never reads
    //     hotter than a shorter one.
    assert!(day.p_wwiii_annual <= lull.p_wwiii_annual + 1e-9,
        "a full-day lull must not read hotter than an overnight one");
}

// ── Persistence floor (PROTOTYPE) ──────────────────────────────────────────────────
// The asymmetric, evidence-gated floor: a war HOLDS through a multi-day news gap (silence ≠
// peace) but a war carrying genuine de-escalation evidence (negative escalation_step) is RELEASED
// and cools to the pure-decay baseline. A quiet world never manufactures a phantom floor.

#[test]
fn persistence_floor_holds_a_silent_war_through_a_multiday_gap() {
    // 96h (4 days) with NO fresh events and NO de-escalation evidence: the floor holds the read
    // well above baseline — far above where the same corpus lands once de-escalation evidence
    // releases the floor (which is the pure-72h-decay read). This is the core asymmetry.
    let silent = { let s = aged(current_2026(), 96); run(s.0, &s.1) };
    let released = { let s = aged(deescalated(current_2026()), 96); run(s.0, &s.1) };
    assert!(silent.p_wwiii_annual > 0.18,
        "a 4-day-silent active war should be HELD elevated by the floor, got {:.2}%", silent.p_wwiii_annual * 100.0);
    assert!(silent.p_wwiii_annual > released.p_wwiii_annual + 0.10,
        "the floor must hold a silent war ≥10pp above the de-escalation-released read; silent={:.2}% released={:.2}%",
        silent.p_wwiii_annual * 100.0, released.p_wwiii_annual * 100.0);
}

#[test]
fn persistence_floor_releases_on_deescalation_evidence() {
    // Same 4-day-old corpus, but carrying ceasefire/peace evidence (escalation_step −0.7): the
    // floor RELEASES and the read cools toward baseline — well below the silent (held) case and
    // below the elevated band. Cooling is EARNED by evidence, not granted by mere silence.
    let silent   = { let s = aged(current_2026(), 96); run(s.0, &s.1) };
    let released = { let s = aged(deescalated(current_2026()), 96); run(s.0, &s.1) };
    assert!(released.p_wwiii_annual < 0.10,
        "a de-escalating war should cool toward baseline, got {:.2}%", released.p_wwiii_annual * 100.0);
    assert!(released.p_wwiii_annual < 0.5 * silent.p_wwiii_annual,
        "de-escalation must cool the read to under half the silent-hold value; released={:.2}% silent={:.2}%",
        released.p_wwiii_annual * 100.0, silent.p_wwiii_annual * 100.0);
}

#[test]
fn persistence_floor_never_engages_in_a_quiet_world() {
    // Honesty invariant: a calm world (no theater at sustained war) must NEVER get a phantom floor.
    // Aged a full week, quiet stays at baseline — the floor's war-rung gate keeps it at exactly zero.
    let quiet_week = { let s = aged(quiet(), 168); run(s.0, &s.1) };
    assert!(quiet_week.p_wwiii_annual < 0.05,
        "a quiet world must not manufacture a persistence floor, got {:.2}%", quiet_week.p_wwiii_annual * 100.0);
}

#[test]
fn persistence_floor_is_band_neutral_at_peak_freshness() {
    // At age 0, slow_heat == fast_heat so floor = FLOOR_FRACTION × fast_heat < fast_heat → the
    // floor cannot raise the read. All four bands scoring at Utc::now are therefore unchanged;
    // this pins the no-regression property directly on the current-world analog.
    let fresh = p_of(current_2026());
    assert!(fresh > 0.55 && fresh < 0.75,
        "the floor must not move the fresh current-world band, got {:.2}%", fresh * 100.0);
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
    // Idealised full-intensity current world: 3 hot theaters, great powers fighting in
    // SEPARATE theaters, no single direct nuclear brink. Re-anchored ~65% → ~60% on
    // 2026-06-09: the 2026-06-03 saturating-breadth fix deliberately lowered broad,
    // no-brink scenarios so the nuclear brink (Cuba) dominates — 60% is that design
    // intent, not a regression. The acceptance band is left as Robert set it (0.55–0.75).
    let c = p_of(current_2026());
    assert!(c > 0.55 && c < 0.75, "current-full should be ~60%, got {:.3}%", c * 100.0);
}

// NOTE: live_hot_2026 stays in `calibration_readout` as a watch scenario but has no hard
// band assertion — the synthetic analog under-represents the live corpus's maxed
// alliance/heat (it reads well below the real live l_sys), so the real ~82% target is
// verified against the live instance, not pinned to this proxy. The four bands below +
// the saturating-breadth change are what lock the recalibration.

#[test]
fn bands_cuba() {
    let k = p_of(cuba_1962());
    assert!(k > 0.70 && k < 0.90, "cuba (nuclear-brink apex) should be ~80%, got {:.3}%", k * 100.0);
}

#[test]
fn live_pegged_analog_reaches_the_railed_operating_point() {
    // Guards that the harness keeps EXERCISING the live railed state the four band analogs miss
    // (they top out at L_sys≈2.32, in the resolved region), so the `breadth_saturated` structural-
    // max disclosure is always tested at the real operating point. Locks that live_pegged_2026
    // reproduces it: five concurrent hot theaters with great powers entangled and an alliance
    // engaged, near-apex P off the ceiling — and crucially a BREADTH peg, NOT the single-theater
    // nuclear brink (that apex is Cuba's). Intent-level bounds only.
    let snap = run(live_pegged_2026().0, &live_pegged_2026().1);
    let c = &snap.couplers;
    assert!(snap.p_wwiii_annual > 0.78 && snap.p_wwiii_annual < crate::models::FORECAST_PROB_CEILING,
        "pegged analog must sit near-apex but off the {:.0}% ceiling, got {:.2}%",
        crate::models::FORECAST_PROB_CEILING * 100.0, snap.p_wwiii_annual * 100.0);
    assert!(c.concurrency >= 4.5, "breadth must be near-maximal (~5 hot theaters), got conc={:.2}", c.concurrency);
    assert!(c.gp_entanglement > 0.5 && c.alliance_activation > 0.0,
        "great-power entanglement + alliance must be live (gp={:.2} alliance={:.2})",
        c.gp_entanglement, c.alliance_activation);
    assert!(!snap.driver.to_lowercase().contains("brink"),
        "the live peg is a BREADTH state, not the nuclear-brink apex (that is Cuba's): {}", snap.driver);
}

#[test]
fn breadth_saturation_is_flagged_at_the_railed_peg_and_nowhere_in_the_resolved_bands() {
    // HONESTY lock for the `couplers.breadth_saturated` disclosure. The railed live peg
    // (where the de-saturation thread measured ~0.0pp resolution) must SET the flag — every
    // breadth amplifier railed, no brink — so an operator reading the ~83% breadth peg (which
    // sits below the 0.90 forecast ceiling, so `at_ceiling` stays false) is told the number is
    // a structural maximum, not a still-climbing point estimate.
    let pegged = run(live_pegged_2026().0, &live_pegged_2026().1);
    assert!(pegged.couplers.breadth_saturated,
        "the live railed peg rails every breadth amplifier with no brink → breadth_saturated");
    assert!(pegged.p_wwiii_annual < crate::models::FORECAST_PROB_CEILING,
        "precondition: the saturated peg is BELOW the forecast ceiling (so at_ceiling does not \
         fire and this flag is the only honest signal), got {:.2}%", pegged.p_wwiii_annual * 100.0);

    // Every analog with genuine top-end resolution must NOT flag saturation: quiet/ukraine/
    // current are not railed (heat or alliance below the rail), and Cuba is a SINGLE-theater
    // brink apex (hot_count 1 + brink live), not a breadth peg. A false positive here would
    // slap a "structural maximum" caveat on a read that can still climb — a worse lie than none.
    for (name, scn) in [("quiet", quiet()), ("ukraine_2022", ukraine_2022()),
                        ("current_2026", current_2026()), ("cuba_1962", cuba_1962())] {
        let s = run(scn.0, &scn.1);
        assert!(!s.couplers.breadth_saturated,
            "{name} retains resolution (not all breadth amplifiers railed, or it is the \
             single-theater brink apex) and must NOT read breadth_saturated");
    }
}

#[test]
fn railed_peg_is_an_accepted_flat_structural_max() {
    // DECIDED 2026-06-25 (Robert): the railed live peg is an ACCEPTED structural maximum, NOT a
    // defect to de-saturate. Empirically it still responds to what matters — decay (−20pp by 72h)
    // and a nuclear brink emerging (→ the 0.90 apex) — and is flat ONLY to incremental escalation
    // while already maxed-without-a-brink. The honest treatment is the `couplers.breadth_saturated`
    // disclosure (locked by `breadth_saturation_is_flagged_*`), NOT a recalibration. This test locks
    // the flatness as DELIBERATE: a ±0.30 no-brink intensity swing moves the headline <0.5pp. If it
    // ever fails (the peg starts moving), the model was de-saturated — revisit the 2026-06-25
    // decision. A self-improve pass must NOT change FITTED calibration constants to alter this
    // without Robert's sign-off (value-laden, his call per the honest-forecasting principle).
    let hot  = { let s = live_pegged_nudged(0.30);  run(s.0, &s.1).p_wwiii_annual };
    let cool = { let s = live_pegged_nudged(-0.30); run(s.0, &s.1).p_wwiii_annual };
    assert!((hot - cool).abs() * 100.0 < 0.5,
        "the railed peg is a DELIBERATE flat structural max (got {:+.3}pp); if it moved, the model \
         de-saturated — revisit the 2026-06-25 decision, do not silently re-fit", (hot - cool) * 100.0);
}

// ── Calibration evidence harness (roadmap 1.1) ─────────────────────────────────────
//
// The `bands_*` tests assert each analog lands INSIDE Robert's target band, but the bands
// are wide (Cuba 70–90%), so a constant can drift halfway to a band edge with every test
// still green and no number to show for it. This harness turns the expert-anchored band
// CENTRES (quiet ~2%, Ukraine ~39%, current-full ~60%, Cuba ~80%) into proper scoring
// numbers — Brier and cross-entropy — so a calibration change is EARNED BY A NUMBER, not
// just "still in band". It is EVIDENCE, not a trip-wire: the locking test pins only the
// scoring MATH and the robust in-band invariant, and the Brier/RMSE baseline is recorded in
// docs/scorecard.md as the figure future calibration work should lower (or justify moving).
// Adding a tighter-than-band gate here would fight legitimate live-targeted recalibration
// (which can move a synthetic analog within its band while improving the live read), so we
// deliberately do not.

/// One expert-anchored calibration point: the analog's name, the centre probability Robert
/// set, the [lo, hi] acceptance band (kept in sync with the `bands_*` tests), and the live
/// model's output for that analog.
struct Anchor { name: &'static str, centre: f64, lo: f64, hi: f64, p: f64 }

/// The four hard-band analogs scored against the live model. `live_hot_2026` is excluded —
/// it is a watch scenario with no hard band (see the note above `bands_cuba`).
fn calibration_anchors() -> Vec<Anchor> {
    vec![
        Anchor { name: "quiet",        centre: 0.02, lo: 0.005, hi: 0.05, p: p_of(quiet()) },
        Anchor { name: "ukraine_2022", centre: 0.39, lo: 0.30,  hi: 0.50, p: p_of(ukraine_2022()) },
        Anchor { name: "current_2026", centre: 0.60, lo: 0.55,  hi: 0.75, p: p_of(current_2026()) },
        Anchor { name: "cuba_1962",    centre: 0.80, lo: 0.70,  hi: 0.90, p: p_of(cuba_1962()) },
    ]
}

/// Brier score (mean squared error) of (prediction, target) pairs. 0.0 = every prediction
/// exactly on its target; higher = worse. A strictly proper scoring rule.
fn brier_score(pairs: &[(f64, f64)]) -> f64 {
    if pairs.is_empty() { return 0.0; }
    pairs.iter().map(|(p, t)| (p - t) * (p - t)).sum::<f64>() / pairs.len() as f64
}

/// Mean binary cross-entropy of (prediction, target) pairs, treating each expert centre as a
/// soft label. Predictions are clamped to [EPS, 1-EPS] so a 0/1 prediction cannot blow up to
/// infinity. Lower = better; note the floor is the targets' own entropy, not zero.
#[cfg(test)] // auxiliary proper-scoring lens for the test readout; runtime evidence uses Brier/RMSE
fn cross_entropy(pairs: &[(f64, f64)]) -> f64 {
    const EPS: f64 = 1e-9;
    if pairs.is_empty() { return 0.0; }
    pairs.iter().map(|(p, t)| {
        let p = p.clamp(EPS, 1.0 - EPS);
        -(t * p.ln() + (1.0 - t) * (1.0 - p).ln())
    }).sum::<f64>() / pairs.len() as f64
}

/// Aggregate calibration evidence for the live model against the anchored analogs.
struct CalibrationEvidence { brier: f64, rmse: f64, in_band: usize, n: usize }

fn calibration_evidence() -> (CalibrationEvidence, Vec<Anchor>) {
    let anchors = calibration_anchors();
    let pairs: Vec<(f64, f64)> = anchors.iter().map(|a| (a.p, a.centre)).collect();
    let brier = brier_score(&pairs);
    let ev = CalibrationEvidence {
        brier,
        rmse: brier.sqrt(),
        in_band: anchors.iter().filter(|a| a.p >= a.lo && a.p <= a.hi).count(),
        n: anchors.len(),
    };
    (ev, anchors)
}

/// Render the live calibration evidence as an HTML fragment for the methodology page
/// (substituted into the `{{CALIBRATION_EVIDENCE}}` placeholder at startup). Computed from
/// the RUNNING model, so the page shows the calibration's real fidelity instead of a
/// hand-written table that silently goes stale (as the old "~65%" row did). No user input —
/// every value is a formatted number, so the fragment is injection-safe.
pub fn calibration_evidence_html() -> String {
    let (ev, anchors) = calibration_evidence();
    let label = |n: &str| -> &'static str { match n {
        "quiet"        => "Quiet modern year",
        "ukraine_2022" => "Ukraine, Feb 2022 (one full-war theater)",
        "current_2026" => "Present world (idealized: 3 theaters, no direct brink)",
        "cuba_1962"    => "Cuba, Oct 1962 (direct nuclear brink)",
        _              => "Analog",
    }};
    let mut rows = String::new();
    for a in &anchors {
        rows.push_str(&format!(
            "<tr><td>{}</td><td>{:.2}%</td><td>~{:.0}%</td><td>{:+.2}pp</td></tr>",
            label(a.name), a.p * 100.0, a.centre * 100.0, (a.p - a.centre) * 100.0));
    }
    format!(
        "<table><tr><th>Analog</th><th>Model P (annualized)</th><th>Anchor</th><th>&Delta;</th></tr>{rows}</table>\n\
         <p>Aggregate fidelity vs the anchored centres: <b>Brier {:.6}</b> &middot; <b>RMSE {:.2}pp</b> &middot; \
         <b>{}/{} within band</b> &mdash; computed live from the running model at startup with proper scoring \
         rules (0 is a perfect match to the anchors; lower is better).</p>",
        ev.brier, ev.rmse * 100.0, ev.in_band, ev.n)
}

/// The live model's annualized P (as a percentage) for a named calibration analog —
/// the single source of truth for the operator-facing "historical reference" scale on
/// the dashboard (the For-scale info line + the hero's vs-history positioning). Driven
/// by `calibration_anchors` (the running engine), so the reference can never drift from
/// what the model ACTUALLY produces for that analog — and, crucially, the hero compares
/// the LIVE read against these poles on the very same model scale. Returns `None` for an
/// unknown analog. Startup-only (template substitution); no hot path.
pub fn analog_model_pct(name: &str) -> Option<f64> {
    calibration_anchors().into_iter().find(|a| a.name == name).map(|a| a.p * 100.0)
}

#[test]
fn analog_model_pct_reports_the_live_model_output_for_named_analogs() {
    // The dashboard's historical-reference poles must be the model's OWN output for each
    // analog (so the live hero positions on one consistent scale), not Robert's expert
    // centres — and they must keep the calibrated ordering. Unknown analogs → None.
    let ukr = analog_model_pct("ukraine_2022").expect("ukraine analog");
    let cuba = analog_model_pct("cuba_1962").expect("cuba analog");
    // Same value calibration_anchors carries (the live engine), not a hand-typed constant.
    let anchors = calibration_anchors();
    let a_ukr = anchors.iter().find(|a| a.name == "ukraine_2022").unwrap().p * 100.0;
    assert!((ukr - a_ukr).abs() < 1e-9, "must equal the live anchor's model output");
    assert!(ukr < cuba, "Ukraine analog must score below the Cuba nuclear-brink analog");
    assert!(ukr > 30.0 && ukr < 50.0, "ukraine analog ~39%, got {ukr:.2}%");
    assert!(cuba > 70.0 && cuba < 90.0, "cuba analog ~80%, got {cuba:.2}%");
    assert!(analog_model_pct("nope").is_none(), "unknown analog must be None");
}

#[test]
fn brier_score_is_correct() {
    assert_eq!(brier_score(&[]), 0.0);
    assert!(brier_score(&[(0.0, 0.0), (1.0, 1.0)]).abs() < 1e-12);          // perfect
    assert!((brier_score(&[(0.5, 0.0)]) - 0.25).abs() < 1e-12);
    assert!((brier_score(&[(1.0, 0.0)]) - 1.0).abs() < 1e-12);             // worst case
    assert!((brier_score(&[(0.5, 0.0), (1.0, 0.0)]) - 0.625).abs() < 1e-12); // mean(0.25, 1.0)
}

#[test]
fn cross_entropy_is_correct_and_bounded() {
    assert!(cross_entropy(&[(1.0, 1.0)]) < 1e-6);                 // perfect confident hit → 0
    assert!(cross_entropy(&[(0.0, 0.0)]) < 1e-6);
    assert!((cross_entropy(&[(0.5, 0.5)]) - std::f64::consts::LN_2).abs() < 1e-9); // -ln(0.5)
    assert!(cross_entropy(&[(0.0, 1.0)]).is_finite());           // extreme miss clamped, not inf
}

/// Reproducible calibration evidence. Run:
///   cargo test calibration_evidence_report -- --nocapture
/// Prints the per-analog table + aggregate Brier / RMSE / cross-entropy, and pins the robust
/// invariant (all four anchors in band). The Brier/RMSE baseline is recorded in
/// docs/scorecard.md as the number calibration work should lower.
#[test]
fn calibration_evidence_report() {
    let (ev, anchors) = calibration_evidence();
    eprintln!("\n── GCRM calibration evidence (model vs expert-anchored centres) ──");
    for a in &anchors {
        let inb = if a.p >= a.lo && a.p <= a.hi { "in " } else { "OUT" };
        eprintln!("{:14} P={:6.2}%  centre={:5.1}%  err={:+5.2}pp  band[{:4.1},{:4.1}]%  {}",
            a.name, a.p * 100.0, a.centre * 100.0, (a.p - a.centre) * 100.0,
            a.lo * 100.0, a.hi * 100.0, inb);
    }
    let xe = cross_entropy(&anchors.iter().map(|a| (a.p, a.centre)).collect::<Vec<_>>());
    eprintln!("aggregate: Brier={:.5}  RMSE={:.2}pp  cross-entropy={:.4}  in-band={}/{}\n",
        ev.brier, ev.rmse * 100.0, xe, ev.in_band, ev.n);

    // Robust invariants only — evidence, not a brittle trip-wire.
    assert_eq!(ev.in_band, ev.n, "all anchored analogs must land in their target band");
    // Loose sanity ceiling: cannot fail while in-band, documents the metric's scale.
    assert!(ev.brier < 0.03, "calibration Brier unexpectedly high: {:.5}", ev.brier);
}
