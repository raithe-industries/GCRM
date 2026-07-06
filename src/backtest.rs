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
/// At the railed operating point a ±CONVENTIONAL-intensity nudge still moves P by ~nothing (Δ≈0pp)
/// because a maxed conventional war is honestly maxed — that flatness is retained and disclosed via
/// `couplers.breadth_saturated`. What CHANGED on 2026-06-28 (the de-saturation): the public INDEX
/// is no longer pegged with it — it is now a continuous rendering of l_sys (`index_from_l_sys`) that
/// discriminates every state — and rising NUCLEAR posture now lifts the read continuously (the brink
/// amplifier ramps, no longer a 0.78 cliff). Pure measurement; no assertion.
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
    // Bound the headline move, but NOT to the old near-frozen ≤3pp: the heat de-saturation
    // (realism #3) deliberately restored top-end resolution, so the read now responds to news
    // flow instead of railing flat — a 12h lull eases the read within its band. The real "silence
    // ≠ peace" guarantees are assertion (1) (l_sys retains ≥93%, the strengthened FLOOR_FRACTION
    // holds the war-state) and (2) (stays inside the band). The P-level move is larger than the
    // l_sys move only because the logistic slope is steepest near this operating point. ≤5pp.
    assert!(lull.p_wwiii_annual > fresh.p_wwiii_annual - 0.05,
        "a 12h lull should hold the war IN BAND but may ease within it (resolution restored); \
         fresh={:.3} lull={:.3}", fresh.p_wwiii_annual, lull.p_wwiii_annual);

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
fn snapshot_attributes_the_cuba_headline_to_nuclear_posture() {
    // End-to-end lock for the modality-sensitivity read through `compute`: the Cuba nuclear-brink
    // analog's headline must attribute to `nuclear_posture` — removing that modality collapses the
    // +70% brink amplifier, the single largest l_sys term. Proves the leave-one-out is wired from
    // the scored board through P and reaches the snapshot. Diagnostic only: the headline P itself is
    // unchanged by this read (it is computed before Step 7b and pinned by the `bands_*` tests).
    let s = run(cuba_1962().0, &cuba_1962().1);
    let lb = &s.load_bearing_modality;
    assert!(lb.available, "the Cuba brink headline must have an attributable load-bearing modality");
    assert_eq!(lb.modality, "nuclear_posture",
        "Cuba's headline is held up by nuclear posture, got {}", lb.modality);
    assert!(lb.p_drop_pp > 0.0, "a load-bearing modality must carry a positive headline P drop");
    assert_eq!(lb.profile.len(), 5, "the attribution profile must cover all five modalities");
    // The profile is sorted largest-first and its top entry is the named modality.
    assert_eq!(lb.profile[0].0, lb.modality, "profile must be sorted with the load-bearing modality first");

    // An EMPTY board attributes nothing — with no events the headline sits at the flat baseline
    // and no modality carries it, so naming one would overclaim (available=false).
    let empty_snap = run(1.0, &[]);
    assert!(!empty_snap.load_bearing_modality.available,
        "an empty board must not name a load-bearing modality");
    assert!(empty_snap.load_bearing_modality.modality.is_empty(),
        "an empty board's load-bearing modality id must be blank");
}

#[test]
fn snapshot_attributes_the_headline_to_a_load_bearing_theater() {
    // End-to-end lock for the theater-sensitivity read through `compute`: the multi-theater
    // current-2026 analog's headline must attribute to a load-bearing THEATER — the flashpoint
    // whose absence from the board drops P the most. Proves the leave-one-out over theaters is
    // wired from the scored board through P and reaches the snapshot. Diagnostic only: the headline
    // P itself is unchanged by this read (computed after the P math, pinned by the `bands_*` tests).
    let s = run(current_2026().0, &current_2026().1);
    let lt = &s.load_bearing_theater;
    assert!(lt.available, "the 3-theater current-world headline must have a load-bearing theater");
    assert!(!lt.theater.is_empty() && !lt.theater_id.is_empty(),
        "a named load-bearing theater must carry both a label and an id");
    assert!(lt.p_drop_pp > 0.0, "a load-bearing theater must carry a positive headline P drop");
    assert_eq!(lt.profile.len(), s.theaters.len(),
        "the attribution profile must cover every theater on the board");
    // The profile is sorted largest-first and its top entry is the named theater's label.
    assert_eq!(lt.profile[0].0, lt.theater,
        "profile must be sorted with the load-bearing theater first");
    for w in lt.profile.windows(2) {
        assert!(w[0].1 >= w[1].1, "the theater attribution profile must be sorted largest-first");
    }

    // An EMPTY board attributes nothing — with no events the headline sits at the flat baseline
    // and no theater carries it, so naming one would overclaim (available=false).
    let empty_snap = run(1.0, &[]);
    assert!(!empty_snap.load_bearing_theater.available,
        "an empty board must not name a load-bearing theater");
    assert!(empty_snap.load_bearing_theater.theater.is_empty(),
        "an empty board's load-bearing theater must be blank");
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
fn live_peg_resolves_after_desaturation_and_no_band_is_breadth_saturated() {
    // The heat de-saturation (realism #3) ENGINEERED AWAY the railed peg: heat now approaches 1.0
    // softly (max ≈ 0.98, never the ≥0.999 rail), so the live 5-theater operating point retains
    // top-end resolution — it can move with the news and climb toward the apex — instead of pegging
    // flat. So `breadth_saturated` (the old "structural maximum, can't climb" disclosure) correctly
    // NO LONGER fires here: the read is resolved, not railed. The flag remains a latent guard for a
    // hypothetical future world that still rails every amplifier despite the soft curve. The headline
    // honesty is now carried by the read's own movement, not a frozen-state caveat.
    let pegged = run(live_pegged_2026().0, &live_pegged_2026().1);
    assert!(!pegged.couplers.breadth_saturated,
        "the live peg now RESOLVES (heat un-railed by the de-saturation) → not breadth_saturated");
    assert!(pegged.p_wwiii_annual < crate::models::FORECAST_PROB_CEILING,
        "the live peg sits below the forecast ceiling with headroom to climb, got {:.2}%",
        pegged.p_wwiii_annual * 100.0);

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
fn railed_peg_index_now_discriminates_states() {
    // REVERSES the 2026-06-25 deferral. DECIDED 2026-06-28 (Robert, in person — the frozen headline
    // "seems stale and stupid… fix it"): the railed live peg WAS a defect, and it is de-saturated.
    //
    // The defect was never that P is flat to incremental CONVENTIONAL escalation while already
    // maxed-without-a-brink — a maxed conventional war IS maxed, which stays honest (and is still
    // disclosed via `couplers.breadth_saturated`). The defect was the PUBLIC INDEX: the retired
    // `(rung.level + within_band)/6` staircase read an identical 83.3 for Ukraine-2022, the present
    // world, the live peg AND Cuba-1962 — collapsing a 45-point spread in the underlying forecast to
    // one frozen number that could not tell a one-theater war from the Cuban Missile Crisis. The
    // index is now the forecast on a 0..95 scale (`index_from_l_sys` = P / PROB_CEILING ×
    // INDEX_CEILING), so it DISCRIMINATES every world-state and moves with the read.
    let idx = |scn: (f64, Vec<GeopoliticalEvent>)| run(scn.0, &scn.1).systemic_index;
    let (q, u, c, peg, cuba) =
        (idx(quiet()), idx(ukraine_2022()), idx(current_2026()), idx(live_pegged_2026()), idx(cuba_1962()));
    // Strict separation where the old staircase pegged everything at 83.3.
    assert!(q < u && u < c && c < peg,
        "index must separate quiet<ukraine<current<peg, got {q:.1} {u:.1} {c:.1} {peg:.1}");
    assert!((cuba - peg).abs() > 1.0,
        "the nuclear-brink apex and the breadth peg must not read identically (cuba={cuba:.1} peg={peg:.1})");
    // Lock that the old 83.3 plateau is gone: ukraine and the present world no longer pile onto it.
    assert!(u < 60.0 && (80.0..95.0).contains(&peg),
        "ukraine must drop off the old 83.3 plateau and the peg must read near-apex (got u={u:.1} peg={peg:.1})");
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
fn nuclear_brink_amplifier_ramps_smoothly_not_a_cliff() {
    // The brink amplifier used to be BINARY: 0 below 0.78 scored nuclear posture, a full +70%
    // jump at/above it. That cliff made rising nuclear danger — the most decision-relevant
    // escalation — invisible until it snapped. It now ramps smoothstep(0.78 → 0.95), so a
    // direct ≥2-great-power nuclear standoff registers continuously as posture climbs. Bands are
    // preserved: every calibration analog's brink-eligible theater scores ≤ 0.69 (< threshold →
    // no brink) and Cuba scores ~0.953 (≥ 0.95 → full apex), so nothing in quiet/ukraine/current/
    // cuba sits in the (0.78, 0.95) ramp zone and the four `bands_*` are mathematically unchanged.
    let brink_world = |nuc_signal: f64| {
        // ONE theater, two great powers head-to-head (US–Russia) → brink-eligible; vary nuclear.
        let ev = evset("nato_russia",
            &[("nuclear_posture", nuc_signal, nuc_signal), ("military_escalation", 0.90, 0.90),
              ("diplomatic_breakdown", 0.90, 0.85)],
            &["united_states", "russia"], true, 10);
        run(2.6, &ev).p_wwiii_annual
    };
    // Signals chosen so scored posture spans below-threshold → mid-ramp → full apex.
    let below = brink_world(0.74);   // scores below 0.78 → no brink
    let mid   = brink_world(0.90);   // scores inside the (0.78, 0.95) ramp → partial brink
    let full  = brink_world(0.98);   // scores ≥ 0.95 → full apex
    assert!(mid > below + 0.005,
        "mid-ramp must exceed the below-threshold read — the cliff is gone: {below:.3} -> {mid:.3}");
    assert!(full > mid + 0.005,
        "full apex must exceed mid-ramp — brink scales with posture: {mid:.3} -> {full:.3}");
    // Cuba (the apex band) still reaches FULL brink, so its calibration is untouched.
    let cuba = run(cuba_1962().0, &cuba_1962().1);
    assert!(cuba.driver.to_lowercase().contains("brink"),
        "Cuba must still read the full nuclear-brink apex: {}", cuba.driver);
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
