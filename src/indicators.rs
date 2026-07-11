// ------------------------------------------------------------
// RAiTHE INDUSTRIES INCORPORATED
// Copyright (c) 2026 All Rights Reserved.
//
// This file is part of a proprietary system. Unauthorized use,
// reproduction, or distribution is strictly prohibited.
// ------------------------------------------------------------

// src/indicators.rs — Indications & Warning (I&W) checklist  [GCRM v2, Phase 4]
//
// The intelligence-community I&W method: define specific OBSERVABLE warning
// conditions and track which have "tripped". Far more defensible and legible than a
// single opaque number. These are evaluated deterministically from the current
// systemic snapshot (theaters + couplers), so the board never depends on the LLM.

use serde::ser::{Serialize, SerializeStruct, Serializer};

use crate::models::{EscalationRung, RiskSnapshot};

#[derive(Debug, Clone)]
pub struct Indicator {
    pub id:      &'static str,
    pub label:   &'static str,
    pub tripped: bool,
    pub theater: Option<String>, // which theater tripped it, if specific
    pub detail:  String,
}

impl Indicator {
    /// Whether this is an apex (highest-stakes, red-lit) condition — derived from
    /// the id against `APEX_INDICATORS`, the single source of truth, so there is no
    /// stored flag that can drift.
    pub fn is_apex(&self) -> bool { APEX_INDICATORS.contains(&self.id) }
}

// Serialize with a derived `apex` field so the dashboard reads which lights are red
// off the data (`i.apex`) instead of re-hardcoding the apex set client-side. The
// engine (`APEX_INDICATORS`) stays the one place that decides which conditions are
// apex; add one here and its light goes red with no parallel frontend edit.
impl Serialize for Indicator {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut st = s.serialize_struct("Indicator", 6)?;
        st.serialize_field("id", self.id)?;
        st.serialize_field("label", self.label)?;
        st.serialize_field("tripped", &self.tripped)?;
        st.serialize_field("theater", &self.theater)?;
        st.serialize_field("detail", &self.detail)?;
        st.serialize_field("apex", &self.is_apex())?;
        st.end()
    }
}

/// The apex warning conditions: an active great-power kinetic war and a direct
/// ≥2-great-power nuclear-brink standoff — the two great-power-WAR states that light
/// red on the board. Single source of truth, exposed per-indicator via the derived
/// `apex` field (which the dashboard renders), so a future apex condition added here
/// lights red automatically without a parallel edit to the frontend.
pub const APEX_INDICATORS: &[&str] = &["gp_kinetic", "nuclear_brink"];

fn modality(snap_theater: &crate::models::TheaterState, m: &str) -> f64 {
    snap_theater.modality_scores.get(m).copied().unwrap_or(0.0)
}

/// Evaluate the full I&W checklist against the current snapshot. Returns every
/// indicator (tripped or not) so the dashboard can render the whole board.
pub fn evaluate(snap: &RiskSnapshot) -> Vec<Indicator> {
    let theaters = &snap.theaters;
    let c = &snap.couplers;

    // 1. Great-power kinetic conflict active. On a clear reading, surface the hottest
    //    great-power theater's rung as a near-miss (same legibility idiom as the
    //    nuclear/energy/cross-domain lights) so the operator can tell whether a great
    //    power sits one rung from active war (e.g. at Crisis) or the board is genuinely
    //    quiet, rather than a bare "No great power in active war".
    let mut gp_kinetic: Vec<&crate::models::TheaterState> = theaters.iter()
        .filter(|t| t.gp_involved && t.rung.level() >= EscalationRung::LimitedWar.level())
        .collect();
    // Most-escalated first (highest rung, then hottest), so both the WHERE attribution
    // (`theater`) and the detail list LEAD with the theater an operator should look at
    // first — not whichever happened to sort first in `theaters`. Mirrors the alliance
    // light (hottest invoker) and the systemic driver; without it, a GreatPowerWar theater
    // listed after a LimitedWar one would hand the apex attribution to the lesser war.
    gp_kinetic.sort_by(|a, b| b.rung.level().cmp(&a.rung.level())
        .then(b.heat.partial_cmp(&a.heat).unwrap_or(std::cmp::Ordering::Equal)));
    let gp_nearest = theaters.iter()
        .filter(|t| t.gp_involved)
        .max_by(|a, b| a.rung.level().cmp(&b.rung.level())
            .then(a.heat.partial_cmp(&b.heat).unwrap_or(std::cmp::Ordering::Equal)));
    let ind_gp_kinetic = Indicator {
        id: "gp_kinetic", label: "Great-power kinetic conflict active",
        tripped: !gp_kinetic.is_empty(),
        theater: gp_kinetic.first().map(|t| t.label.clone()),
        detail: if !gp_kinetic.is_empty() {
            format!("{} theater(s): {}", gp_kinetic.len(),
                gp_kinetic.iter().map(|t| t.label.as_str()).collect::<Vec<_>>().join(", "))
        } else {
            match gp_nearest {
                Some(t) => format!("No great power in active war (closest {} at {})",
                    t.label, t.rung_label),
                None => "No great power in active war".into(),
            }
        },
    };

    // 2. Nuclear signaling elevated. On a clear reading, surface the hottest near-miss
    //    value (same idiom as the energy/chokepoint light) so the operator can see how
    //    close the nuclear axis is to tripping rather than just a bare "Below threshold".
    let nuc = theaters.iter().map(|t| (t, modality(t, "nuclear_posture")))
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    let ind_nuclear = match nuc {
        Some((t, v)) if v >= 0.45 => Indicator {
            id: "nuclear_signaling", label: "Nuclear signaling elevated", tripped: true,
            theater: Some(t.label.clone()), detail: format!("{} nuclear posture {:.2}", t.label, v) },
        Some((t, v)) => Indicator {
            id: "nuclear_signaling", label: "Nuclear signaling elevated", tripped: false,
            theater: None, detail: format!("Below threshold (max {} {:.2})", t.label, v) },
        None => Indicator { id: "nuclear_signaling", label: "Nuclear signaling elevated",
            tripped: false, theater: None, detail: "No theater data".into() },
    };

    // 3. Energy / chokepoint weaponized — coercive-economic escalation (blockade,
    //    energy/grain/chip weaponization) in ANY theater, not just the Gulf. A
    //    Taiwan-Strait quarantine or a Black-Sea grain blockade is as much a
    //    weaponized chokepoint as Hormuz, and both are top great-power-war triggers,
    //    so this scans every theater (the same global-max idiom as the nuclear
    //    signaling and cross-domain lights) and names the hottest — rather than going
    //    dark on a non-Gulf chokepoint, the one blind spot this board used to have.
    let chokepoint = theaters.iter().map(|t| (t, modality(t, "economic_warfare")))
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    let ind_energy = match chokepoint {
        Some((t, v)) if v >= 0.45 => Indicator {
            id: "energy_chokepoint", label: "Energy / chokepoint weaponized", tripped: true,
            theater: Some(t.label.clone()),
            detail: format!("{} coercive-economic {:.2}", t.label, v) },
        Some((t, v)) => Indicator {
            id: "energy_chokepoint", label: "Energy / chokepoint weaponized", tripped: false,
            theater: None,
            detail: format!("Below threshold (max {} {:.2})", t.label, v) },
        None => Indicator {
            id: "energy_chokepoint", label: "Energy / chokepoint weaponized", tripped: false,
            theater: None, detail: "No theater data".into() },
    };

    // 4. Crisis diplomacy breaking down — the `diplomatic_breakdown` modality elevated in
    //    ANY theater (recalled ambassadors, walked-out talks, severed crisis-communication
    //    channels). This is the classic 1914 leading warning: when the off-ramps close, a
    //    crisis loses its brakes. The model already scores it (a weight-1.0 modality in
    //    DOMAIN_WEIGHTS that feeds the headline) and the cross-domain light COUNTS it, but
    //    no board light NAMED it — so a diplomatic collapse short of a 3-modality
    //    cross-domain trip went dark on the operator's at-a-glance board. Same global-max
    //    idiom and 0.45 "meaningfully elevated" bar as the nuclear/energy lights (the
    //    per-modality signaling tier, above the model's faint 0.32 elevation line), naming
    //    the hottest theater and a near-miss on a clear read.
    let diplo = theaters.iter().map(|t| (t, modality(t, "diplomatic_breakdown")))
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    let ind_diplomatic = match diplo {
        Some((t, v)) if v >= 0.45 => Indicator {
            id: "diplomatic_breakdown", label: "Crisis diplomacy breaking down", tripped: true,
            theater: Some(t.label.clone()),
            detail: format!("{} diplomatic breakdown {:.2}", t.label, v) },
        Some((t, v)) => Indicator {
            id: "diplomatic_breakdown", label: "Crisis diplomacy breaking down", tripped: false,
            theater: None,
            detail: format!("Below threshold (max {} {:.2})", t.label, v) },
        None => Indicator {
            id: "diplomatic_breakdown", label: "Crisis diplomacy breaking down", tripped: false,
            theater: None, detail: "No theater data".into() },
    };

    // 5. Cyber / critical-infrastructure attack — the `cyber_info_ops` modality elevated
    //    in ANY theater (attacks on power grids, financial systems, military C2, undersea
    //    cables, or a coordinated info-ops campaign). This is the modern leading edge of
    //    great-power conflict: cyber strikes on critical infrastructure routinely PRECEDE
    //    kinetic action (degrade the adversary's command-and-control before the first shot).
    //    `cyber_info_ops` is a tracked modality (weight 0.9 in DOMAIN_WEIGHTS that feeds the
    //    headline) and the cross-domain light COUNTS it, but — unlike the other four
    //    modalities (military→gp_kinetic, nuclear→nuclear_signaling, economic→energy_chokepoint,
    //    diplomatic→diplomatic_breakdown) — no board light NAMED it, so a cyber/infrastructure
    //    escalation short of a 3-modality cross-domain trip went dark on the operator's
    //    at-a-glance board. Same global-max idiom and 0.45 "meaningfully elevated" bar as the
    //    nuclear/energy/diplomatic lights, naming the hottest theater and a near-miss on a clear
    //    read. Not apex (the apex set is reserved for great-power-WAR configurations).
    let cyber = theaters.iter().map(|t| (t, modality(t, "cyber_info_ops")))
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    let ind_cyber = match cyber {
        Some((t, v)) if v >= 0.45 => Indicator {
            id: "cyber_infrastructure", label: "Cyber / critical-infrastructure attack", tripped: true,
            theater: Some(t.label.clone()),
            detail: format!("{} cyber / info-ops {:.2}", t.label, v) },
        Some((t, v)) => Indicator {
            id: "cyber_infrastructure", label: "Cyber / critical-infrastructure attack", tripped: false,
            theater: None,
            detail: format!("Below threshold (max {} {:.2})", t.label, v) },
        None => Indicator {
            id: "cyber_infrastructure", label: "Cyber / critical-infrastructure attack", tripped: false,
            theater: None, detail: "No theater data".into() },
    };

    // 6. Multiple theaters concurrently hot.
    let ind_concurrency = Indicator {
        id: "multi_theater", label: "Multiple theaters concurrently hot",
        tripped: c.concurrency >= 1.8, theater: None,
        detail: format!("{:.1} theaters hot", c.concurrency),
    };

    // 7. Active escalation at a flashpoint — a theater already at Crisis or above that
    //    is ALSO rising this tick. The other eleven lights are all LEVEL reads; none flags
    //    VELOCITY-at-altitude — a hot flashpoint getting *worse* — which is the classic
    //    I&W leading indicator (the I&W method is fundamentally about detecting CHANGE,
    //    not just standing level). It reuses the MODEL's own classification — the rung
    //    (Crisis = heat ≥ HOT_HEAT, the same "hot" boundary the concurrency coupler uses)
    //    and `trend == "rising"` — so it introduces NO new calibrated threshold and can
    //    never disagree with the ladder strip about which theaters are hot/rising. Names
    //    the HOTTEST qualifying theater (same hottest-qualifying rule as the apex lights)
    //    and surfaces the rising driver, so the operator sees both WHERE risk is
    //    accelerating and WHY. On a clear reading it names the hottest theater rising at
    //    all (even below Crisis), so a sub-Crisis flashpoint heating up is visible rather
    //    than hidden behind a bare "nothing escalating".
    let hottest_escalating = theaters.iter()
        .filter(|t| t.rung.level() >= EscalationRung::Crisis.level() && t.trend == "rising")
        .max_by(|a, b| a.heat.partial_cmp(&b.heat).unwrap_or(std::cmp::Ordering::Equal));
    let nearest_rising = theaters.iter()
        .filter(|t| t.trend == "rising")
        .max_by(|a, b| a.heat.partial_cmp(&b.heat).unwrap_or(std::cmp::Ordering::Equal));
    let ind_escalating = match hottest_escalating {
        Some(t) => Indicator {
            id: "active_escalation", label: "Active escalation at a flashpoint",
            tripped: true, theater: Some(t.label.clone()),
            detail: {
                let why = if !t.rising_driver.is_empty() {
                    format!(", rising on {}", t.rising_driver)
                } else { String::new() };
                format!("{} at {} and rising ({:+.3}{})", t.label, t.rung_label, t.delta, why)
            },
        },
        None => Indicator {
            id: "active_escalation", label: "Active escalation at a flashpoint",
            tripped: false, theater: None,
            detail: match nearest_rising {
                Some(t) => format!("No hot theater rising (closest {} at {}, {:+.3})",
                    t.label, t.rung_label, t.delta),
                None => "No theater rising".into(),
            },
        },
    };

    // 8. Multiple great powers entangled.
    let ind_gp_entangle = Indicator {
        id: "gp_entanglement", label: "Multiple great powers entangled",
        tripped: c.gp_entanglement >= 0.60, theater: None,
        detail: format!("entanglement {:.2}", c.gp_entanglement),
    };

    // 9. Mutual-defense alliance invoked. This light is a strict read of the alliance
    //    COUPLER (`alliance_activation`) — the quantity that actually feeds the headline
    //    P — so the light, the theater it names, and the number can never disagree (the
    //    same discipline as the nuclear-brink light sharing `theater_is_nuclear_brink`
    //    with `brink_mult`). The coupler activates only for an invocation in a theater at
    //    or above Tension (`heat ≥ STABLE_HEAT_CEILING`; a Stable theater contributes
    //    ZERO — the honesty floor de-leaked 2026-07-11, commit 0741264). So the theater
    //    and detail are attached EXACTLY when the coupler is live, and the HOTTEST
    //    alliance-invoked theater — the one whose (hot) invocation drives
    //    `alliance_activation` toward its 1.0 apex — is then guaranteed active, never a
    //    cold stray. A lone treaty-consultation headline in an otherwise-quiet theater
    //    leaves the coupler at 0.0, so the light reads clear AND names no theater, rather
    //    than a not-tripped light that still asserts an "Article 5 / collective-defense
    //    signal" in a theater that contributes nothing to P (the pre-fix contradiction:
    //    `tripped` keyed on the heat-gated coupler while theater/detail keyed on the bare
    //    `alliance_invoked` flag, so the two diverged for a Stable-only invocation).
    let alliance_theater = if c.alliance_activation > 0.0 {
        theaters.iter()
            .filter(|t| t.alliance_invoked)
            .max_by(|a, b| a.heat.partial_cmp(&b.heat).unwrap_or(std::cmp::Ordering::Equal))
    } else {
        None
    };
    let ind_alliance = Indicator {
        id: "alliance_invoked", label: "Mutual-defense alliance invoked",
        tripped: c.alliance_activation > 0.0,
        theater: alliance_theater.map(|t| t.label.clone()),
        detail: match alliance_theater {
            Some(t) => format!("Article 5 / collective-defense signal: {}", t.label),
            None => "None".into(),
        },
    };

    // (Retired 2026-07-03: the "Arms-control guardrails collapsed" light. It was the one
    // light on the board that observed NOTHING — `couplers.guardrail_collapse` is a pure
    // function of the operator-typed regime config (bayesian::guardrail_from_regime over
    // the regime multiplier; theater.rs never computes it), so the light only ever echoed
    // settings.yml back at the operator. Its 0.70 trip needs a regime product ≥ 3.8, which
    // the 2026-06-30 regime de-double-count made unreachable (active structural product
    // 2.90 → collapse frozen at 0.475). The VALUE keeps its full engine role — the l_sys
    // guardrail amplifier, dominant-coupler naming, and the regime inspector panel — only
    // the dead board light went. Do not re-add it: this board is for OBSERVABLE warning
    // conditions, and a config echo can never be one.)

    // 10. Cross-domain escalation within a single theater (≥3 modalities elevated).
    //    On a clear reading, surface the hottest near-miss (how many modalities the
    //    leading theater has elevated, against the 3 needed) — same legibility idiom as
    //    the nuclear/energy lights — so a theater sitting at 2/3 (one axis from tripping)
    //    is distinguishable from a quiet board, rather than a bare "No theater with 3+".
    //    "Elevated" here is the MODEL's `ELEVATION_THRESHOLD` (the same cutoff that feeds
    //    the intra-theater co-occurrence amplifier and that the dashboard draws as the
    //    "elevated" line), counted over the model's own modality set `DOMAIN_WEIGHTS` —
    //    not a hardcoded value/list. So the board's "cross-domain" reading can never drift
    //    from what the headline number calls elevated, even if either is recalibrated.
    let cross = theaters.iter().map(|t| {
        let n = crate::bayesian::DOMAIN_WEIGHTS.iter()
            .filter(|(m, _)| modality(t, m) >= crate::models::ELEVATION_THRESHOLD)
            .count();
        (t, n)
    }).max_by_key(|(_, n)| *n);
    let ind_cross = match cross {
        Some((t, n)) if n >= 3 => Indicator {
            id: "cross_domain", label: "Cross-domain escalation in one theater", tripped: true,
            theater: Some(t.label.clone()), detail: format!("{} modalities elevated in {}", n, t.label) },
        Some((t, n)) => Indicator {
            id: "cross_domain", label: "Cross-domain escalation in one theater", tripped: false,
            theater: None, detail: format!("Below threshold (max {} {}/3 modalities)", t.label, n) },
        None => Indicator { id: "cross_domain", label: "Cross-domain escalation in one theater",
            tripped: false, theater: None, detail: "No theater data".into() },
    };

    // 11. Nuclear-brink configuration (direct ≥2-great-power nuclear confrontation).
    // Uses the SAME `theater_is_nuclear_brink` predicate as the systemic amplifier
    // (theater.rs), so this board light trips on exactly the state where the headline's
    // 1.70× apex amplifier engages — the number and the board can never disagree about
    // whether the apex configuration is live. (Previously this tripped at nuclear ≥0.70
    // while the amplifier required ≥0.78, so the board over-claimed the apex in the
    // 0.70–0.78 band.)
    // Among ANY theaters in the apex nuclear-brink configuration, name the HOTTEST —
    // same hottest-qualifying-theater rule as the gp-kinetic and alliance lights — so the
    // apex WHERE pointer can't land on a cooler brink that merely sorts first in `theaters`.
    let brink = theaters.iter().filter(|t| crate::theater::theater_is_nuclear_brink(t))
        .max_by(|a, b| a.heat.partial_cmp(&b.heat).unwrap_or(std::cmp::Ordering::Equal));
    let ind_brink = Indicator {
        id: "nuclear_brink", label: "Nuclear-brink configuration (apex)",
        tripped: brink.is_some(), theater: brink.map(|t| t.label.clone()),
        detail: match brink { Some(t) => format!("Direct nuclear-superpower confrontation: {}", t.label),
                              None => "No direct nuclear-superpower brink".into() },
    };

    // 12. Seismic event consistent with a nuclear test. The strongest PHYSICAL nuclear
    // indicator — a shallow event at a known test site that has cleared the natural-
    // earthquake discriminator (no aftershock sequence, or a CTBTO statement). Sourced
    // from the seismic monitor's own `SeismicAlert::is_test_consistent` determination
    // (carried on the snapshot by the aggregator), so it is still deterministic and
    // LLM-independent — but it adds the one warning the theater/coupler engine cannot
    // see (a possible detonation), which previously lived only on the standalone banner.
    // Not apex: the apex set is reserved for great-power-WAR configurations, and this is
    // an explicitly "consistent with" heuristic, so it lights amber, named to its site.
    let ind_seismic = if snap.seismic_test_consistent {
        Indicator {
            id: "seismic_test", label: "Seismic event consistent with nuclear test",
            tripped: true, theater: Some(snap.seismic_site.clone()),
            detail: format!(
                "Shallow seismic event at {} cleared the aftershock / CTBTO discriminator",
                if snap.seismic_site.is_empty() { "a known test site" } else { &snap.seismic_site }
            ),
        }
    } else {
        Indicator {
            id: "seismic_test", label: "Seismic event consistent with nuclear test",
            tripped: false, theater: None,
            detail: "No test-consistent seismic anomaly at a known test site".into(),
        }
    };

    vec![ind_gp_kinetic, ind_nuclear, ind_energy, ind_diplomatic, ind_cyber, ind_concurrency,
         ind_escalating, ind_gp_entangle, ind_alliance, ind_cross, ind_brink, ind_seismic]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{RiskSnapshot, SystemicCouplers, TheaterState, EscalationRung};

    fn theater(id: &str, rung: EscalationRung, gp: bool, scores: &[(&str, f64)], actors: &[&str]) -> TheaterState {
        TheaterState {
            theater_id: id.into(), label: id.into(), rung, rung_label: rung.label().into(),
            heat: 0.5, modality_scores: scores.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
            trend: "stable".into(), delta: 0.0, event_count: 5, gp_involved: gp,
            alliance_invoked: false, top_actors: actors.iter().map(|s| s.to_string()).collect(),
            top_driver: String::new(), rising_driver: String::new(),
            secondary_driver: String::new(), held_by_floor: false,
            fresh_rung_label: rung.label().into(),
            escalation_momentum: 0.0,
        }
    }

    #[test]
    fn apex_flag_marks_exactly_the_two_apex_conditions_and_serializes() {
        // The two great-power-WAR conditions (an active great-power kinetic war and a
        // direct nuclear-brink standoff) are apex; every other light is not. The flag is
        // DERIVED from the id against APEX_INDICATORS — the single source of truth the
        // dashboard now reads (`i.apex`) instead of re-hardcoding the apex set — so the
        // red lights can never drift from the engine. Lock both the predicate and that it
        // reaches the serialized snapshot the dashboard consumes.
        let inds = evaluate(&RiskSnapshot::default());
        for i in &inds {
            let want = i.id == "gp_kinetic" || i.id == "nuclear_brink";
            assert_eq!(i.is_apex(), want, "apex flag wrong for `{}`", i.id);
            assert_eq!(APEX_INDICATORS.contains(&i.id), want,
                "APEX_INDICATORS membership disagrees with the apex set for `{}`", i.id);
        }
        // Exactly two apex conditions exist, and every APEX_INDICATORS id is a real light.
        assert_eq!(inds.iter().filter(|i| i.is_apex()).count(), APEX_INDICATORS.len());
        let ids: Vec<&str> = inds.iter().map(|i| i.id).collect();
        for a in APEX_INDICATORS {
            assert!(ids.contains(a), "APEX_INDICATORS id `{a}` is not produced by evaluate()");
        }
        // The derived `apex` field must appear in the serialized JSON (what the dashboard reads).
        let v = serde_json::to_value(&inds).unwrap();
        let gp = v.as_array().unwrap().iter().find(|x| x["id"] == "gp_kinetic").unwrap();
        assert_eq!(gp["apex"], serde_json::json!(true), "gp_kinetic must serialize apex=true");
        let conc = v.as_array().unwrap().iter().find(|x| x["id"] == "multi_theater").unwrap();
        assert_eq!(conc["apex"], serde_json::json!(false), "a non-apex light must serialize apex=false");
    }

    #[test]
    fn empty_snapshot_trips_nothing() {
        let snap = RiskSnapshot::default();
        let inds = evaluate(&snap);
        assert_eq!(inds.len(), 12);
        assert!(inds.iter().all(|i| !i.tripped));
    }

    #[test]
    fn guardrails_board_light_stays_retired() {
        // Retired 2026-07-03: the "Arms-control guardrails collapsed" light observed only
        // operator config — `couplers.guardrail_collapse` is a pure function of the regime
        // multiplier (bayesian::guardrail_from_regime), and the 2026-06-30 regime
        // de-double-count froze it at 0.475, far below its 0.70 trip. The value keeps its
        // engine role (l_sys amplifier, dominant-coupler naming, regime panel); the BOARD
        // is for observable warning conditions only. Lock the light out so a future
        // improve run doesn't re-add a config echo — even with the coupler railed at 1.0,
        // no board light may read it.
        let snap = RiskSnapshot {
            couplers: SystemicCouplers { guardrail_collapse: 1.0, ..Default::default() },
            ..Default::default()
        };
        let inds = evaluate(&snap);
        assert!(inds.iter().all(|i| i.id != "guardrails"),
            "the retired guardrails board light must not come back (it observes only settings.yml)");
        assert!(inds.iter().all(|i| !i.tripped),
            "a railed guardrail_collapse coupler alone must trip nothing on the board");
    }

    #[test]
    fn hot_world_trips_key_indicators() {
        let snap = RiskSnapshot {
            theaters: vec![
                theater("us_iran", EscalationRung::GreatPowerWar, true,
                    &[("military_escalation",0.7),("economic_warfare",0.6),("diplomatic_breakdown",0.5)],
                    &["united_states","iran"]),
                theater("nato_russia", EscalationRung::GreatPowerWar, true,
                    &[("military_escalation",0.6),("nuclear_posture",0.80),("diplomatic_breakdown",0.5)],
                    &["united_states","russia"]),
            ],
            couplers: SystemicCouplers {
                gp_entanglement: 1.0, alliance_activation: 0.0, concurrency: 2.5,
                guardrail_collapse: 1.0, coupling_multiplier: 2.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let inds = evaluate(&snap);
        let trip = |id: &str| inds.iter().find(|i| i.id == id).unwrap().tripped;
        assert!(trip("gp_kinetic"));
        assert!(trip("nuclear_signaling"));
        assert!(trip("energy_chokepoint"));
        assert!(trip("multi_theater"));
        assert!(trip("gp_entanglement"));
        assert!(trip("cross_domain"));
        assert!(trip("nuclear_brink"), "nato_russia has nuclear 0.80 + US & Russia → brink");
    }

    #[test]
    fn every_indicator_carries_a_legible_nonempty_label_and_unique_id() {
        // The I&W board renders one cell per indicator showing its `label`, and the
        // deploy-time eyes gate now asserts each rendered cell is present and its label is
        // legible (non-empty). Lock the SERVER side of that legibility contract so a future
        // light with a blank label (an unreadable dot on the board) or a duplicated id
        // (which would collide in the apex/`i.apex` lookup and mis-key the cell) can never
        // ship. The `.iw-label` cell is 8px and ellipsis-clipped, so also cap the length so
        // a pathologically long label can't blow past the cell. Checked on BOTH a quiet and
        // a hot snapshot so the tripped/clear label branches are both exercised.
        let hot = RiskSnapshot {
            theaters: vec![
                theater("us_iran", EscalationRung::GreatPowerWar, true,
                    &[("military_escalation",0.7),("economic_warfare",0.6),("diplomatic_breakdown",0.5)],
                    &["united_states","iran"]),
                theater("nato_russia", EscalationRung::GreatPowerWar, true,
                    &[("military_escalation",0.6),("nuclear_posture",0.80),("diplomatic_breakdown",0.5)],
                    &["united_states","russia"]),
            ],
            couplers: SystemicCouplers {
                gp_entanglement: 1.0, alliance_activation: 1.0, concurrency: 2.5,
                guardrail_collapse: 1.0, coupling_multiplier: 2.0,
                ..Default::default()
            },
            ..Default::default()
        };
        for snap in [RiskSnapshot::default(), hot] {
            let inds = evaluate(&snap);
            assert_eq!(inds.len(), 12, "the board is a fixed 12 warning conditions");
            let mut ids = std::collections::HashSet::new();
            for i in &inds {
                assert!(!i.label.trim().is_empty(),
                    "indicator `{}` has a blank label — an unreadable board cell", i.id);
                assert!(i.label.chars().count() <= 48,
                    "indicator `{}` label is {} chars — too long for the 8px board cell",
                    i.id, i.label.chars().count());
                assert!(!i.id.trim().is_empty(), "an indicator has a blank id");
                assert!(ids.insert(i.id),
                    "duplicate indicator id `{}` — two board cells would collide", i.id);
            }
        }
    }

    #[test]
    fn gp_kinetic_clear_surfaces_hottest_near_miss() {
        // No great power at Limited War or above → clear, but the detail must name the
        // hottest great-power theater's rung (same legibility contract as the
        // nuclear/energy/cross-domain lights), so a great power one rung from active war
        // is visible rather than hidden behind a bare "No great power in active war".
        let snap = RiskSnapshot {
            theaters: vec![
                // Non-great-power theater, hotter rung — must NOT be picked as the near-miss.
                theater("regional", EscalationRung::LimitedWar, false,
                    &[("military_escalation", 0.50)], &["someone"]),
                // Great-power theater at Crisis — one rung short of tripping the light.
                theater("nato_russia", EscalationRung::Crisis, true,
                    &[("military_escalation", 0.30)], &["russia", "united_states"]),
            ],
            ..Default::default()
        };
        let inds = evaluate(&snap);
        let gp = inds.iter().find(|i| i.id == "gp_kinetic").unwrap();
        assert!(!gp.tripped, "no great power at Limited War+ must read clear");
        assert!(gp.detail.contains("nato_russia"),
            "clear detail should name the hottest great-power theater, got {:?}", gp.detail);
        assert!(gp.detail.contains(EscalationRung::Crisis.label()),
            "clear detail should report the near-miss rung, got {:?}", gp.detail);
    }

    #[test]
    fn gp_kinetic_clear_with_no_great_power_theater_is_bare() {
        // No great-power-involved theater at all → the bare clear message, no near-miss.
        let snap = RiskSnapshot {
            theaters: vec![
                theater("regional", EscalationRung::Crisis, false,
                    &[("military_escalation", 0.30)], &["someone"]),
            ],
            ..Default::default()
        };
        let inds = evaluate(&snap);
        let gp = inds.iter().find(|i| i.id == "gp_kinetic").unwrap();
        assert!(!gp.tripped);
        assert_eq!(gp.detail, "No great power in active war");
    }

    #[test]
    fn chokepoint_light_trips_outside_the_gulf() {
        // Regression guard: the "Energy / chokepoint weaponized" light must scan ALL
        // theaters, not just the Gulf. A Taiwan-Strait quarantine (coercive-economic
        // escalation in us_china_taiwan) with a cold Gulf is a real weaponized
        // chokepoint — the old Gulf-only code went dark on exactly this, leaving the
        // board misleadingly "clear". It must now trip AND name the responsible theater.
        let snap = RiskSnapshot {
            theaters: vec![
                // Gulf cold on the coercive-economic axis (would not trip under old code).
                theater("us_iran", EscalationRung::Tension, false,
                    &[("economic_warfare", 0.05)], &["iran"]),
                // Taiwan Strait blockaded: coercive-economic well above the 0.45 threshold.
                theater("us_china_taiwan", EscalationRung::Crisis, true,
                    &[("economic_warfare", 0.71)], &["china", "united_states", "taiwan"]),
            ],
            ..Default::default()
        };
        let inds = evaluate(&snap);
        let energy = inds.iter().find(|i| i.id == "energy_chokepoint").unwrap();
        assert!(energy.tripped,
            "a non-Gulf chokepoint (Taiwan Strait) must trip the energy/chokepoint light");
        assert_eq!(energy.theater.as_deref(), Some("us_china_taiwan"),
            "the tripped light must name the theater that actually weaponized the chokepoint");
    }

    #[test]
    fn chokepoint_light_clear_when_no_theater_weaponizes() {
        // Below-threshold coercive-economic everywhere → clear, and the detail reports
        // the hottest near-miss so the operator can see how close it is to tripping.
        let snap = RiskSnapshot {
            theaters: vec![
                theater("us_iran", EscalationRung::Tension, false,
                    &[("economic_warfare", 0.20)], &["iran"]),
                theater("nato_russia", EscalationRung::Tension, false,
                    &[("economic_warfare", 0.30)], &["russia"]),
            ],
            ..Default::default()
        };
        let inds = evaluate(&snap);
        let energy = inds.iter().find(|i| i.id == "energy_chokepoint").unwrap();
        assert!(!energy.tripped, "no theater above 0.45 must read clear");
        assert!(energy.detail.contains("0.30"),
            "clear detail should surface the hottest near-miss value, got {:?}", energy.detail);
    }

    #[test]
    fn diplomatic_breakdown_light_trips_and_names_the_hottest_theater() {
        // The board scores 5 modalities but only NAMED 3 (military via gp_kinetic, nuclear,
        // economic/chokepoint). `diplomatic_breakdown` — the 1914 "off-ramps closing"
        // warning — had no dedicated light, so a diplomatic collapse short of a 3-modality
        // cross-domain trip went dark. This light closes that gap: it scans ALL theaters
        // (global-max idiom) and names the hottest above the 0.45 signaling bar.
        let snap = RiskSnapshot {
            theaters: vec![
                // Talks intact in one theater.
                theater("us_iran", EscalationRung::Tension, false,
                    &[("diplomatic_breakdown", 0.20)], &["iran"]),
                // Crisis communication severed in another, above the 0.45 bar.
                theater("nato_russia", EscalationRung::Crisis, true,
                    &[("diplomatic_breakdown", 0.66)], &["russia", "united_states"]),
            ],
            ..Default::default()
        };
        let inds = evaluate(&snap);
        let diplo = inds.iter().find(|i| i.id == "diplomatic_breakdown").unwrap();
        assert!(diplo.tripped, "diplomatic breakdown at 0.66 must trip the light");
        assert_eq!(diplo.theater.as_deref(), Some("nato_russia"),
            "the tripped light must name the theater whose off-ramps actually closed");
        // It is NOT apex (the apex set is reserved for great-power-WAR configurations).
        assert!(!diplo.is_apex(), "a diplomatic-breakdown light must not light red (apex)");
    }

    #[test]
    fn diplomatic_breakdown_clear_surfaces_hottest_near_miss() {
        // Below-threshold diplomatic breakdown everywhere → clear, and the detail reports
        // the hottest near-miss so the operator sees how close the off-ramps are to closing
        // (same legibility contract as the energy/nuclear lights).
        let snap = RiskSnapshot {
            theaters: vec![
                theater("us_iran", EscalationRung::Tension, false,
                    &[("diplomatic_breakdown", 0.15)], &["iran"]),
                theater("nato_russia", EscalationRung::Tension, false,
                    &[("diplomatic_breakdown", 0.41)], &["russia"]),
            ],
            ..Default::default()
        };
        let inds = evaluate(&snap);
        let diplo = inds.iter().find(|i| i.id == "diplomatic_breakdown").unwrap();
        assert!(!diplo.tripped, "no theater at/above 0.45 must read clear");
        assert!(diplo.detail.contains("0.41"),
            "clear detail should surface the hottest near-miss value, got {:?}", diplo.detail);
    }

    #[test]
    fn cyber_infrastructure_light_trips_and_names_the_hottest_theater() {
        // `cyber_info_ops` is the 5th tracked modality but was the only one with no named
        // board light (military→gp_kinetic, nuclear→nuclear_signaling, economic→energy_chokepoint,
        // diplomatic→diplomatic_breakdown all had one). A cyber/critical-infrastructure attack —
        // the modern opening move of great-power conflict — short of a 3-modality cross-domain
        // trip went dark. This light closes that gap: it scans ALL theaters (global-max idiom)
        // and names the hottest above the 0.45 signaling bar.
        let snap = RiskSnapshot {
            theaters: vec![
                // Low-grade probing in one theater.
                theater("us_iran", EscalationRung::Tension, false,
                    &[("cyber_info_ops", 0.20)], &["iran"]),
                // Grid / C2 attack in another, above the 0.45 bar.
                theater("nato_russia", EscalationRung::Crisis, true,
                    &[("cyber_info_ops", 0.71)], &["russia", "united_states"]),
            ],
            ..Default::default()
        };
        let inds = evaluate(&snap);
        let cyber = inds.iter().find(|i| i.id == "cyber_infrastructure").unwrap();
        assert!(cyber.tripped, "cyber/info-ops at 0.71 must trip the light");
        assert_eq!(cyber.theater.as_deref(), Some("nato_russia"),
            "the tripped light must name the theater carrying the infrastructure attack");
        // It is NOT apex (the apex set is reserved for great-power-WAR configurations).
        assert!(!cyber.is_apex(), "a cyber-infrastructure light must not light red (apex)");
    }

    #[test]
    fn cyber_infrastructure_clear_surfaces_hottest_near_miss() {
        // Below-threshold cyber/info-ops everywhere → clear, and the detail reports the hottest
        // near-miss so the operator sees how close the cyber axis is to tripping (same
        // legibility contract as the energy/nuclear/diplomatic lights).
        let snap = RiskSnapshot {
            theaters: vec![
                theater("us_iran", EscalationRung::Tension, false,
                    &[("cyber_info_ops", 0.15)], &["iran"]),
                theater("nato_russia", EscalationRung::Tension, false,
                    &[("cyber_info_ops", 0.42)], &["russia"]),
            ],
            ..Default::default()
        };
        let inds = evaluate(&snap);
        let cyber = inds.iter().find(|i| i.id == "cyber_infrastructure").unwrap();
        assert!(!cyber.tripped, "no theater at/above 0.45 must read clear");
        assert!(cyber.detail.contains("0.42"),
            "clear detail should surface the hottest near-miss value, got {:?}", cyber.detail);
    }

    #[test]
    fn nuclear_signaling_clear_surfaces_hottest_near_miss() {
        // Below-threshold nuclear posture everywhere → clear, but the detail must report
        // the hottest near-miss value so the operator can see how close the nuclear axis
        // is to tripping (same legibility contract as the energy/chokepoint light), rather
        // than a bare "Below threshold" that hides whether posture sits at 0.10 or 0.44.
        let snap = RiskSnapshot {
            theaters: vec![
                theater("us_iran", EscalationRung::Tension, false,
                    &[("nuclear_posture", 0.20)], &["iran"]),
                theater("nato_russia", EscalationRung::Crisis, true,
                    &[("nuclear_posture", 0.44)], &["russia", "united_states"]),
            ],
            ..Default::default()
        };
        let inds = evaluate(&snap);
        let nuc = inds.iter().find(|i| i.id == "nuclear_signaling").unwrap();
        assert!(!nuc.tripped, "no theater at/above 0.45 must read clear");
        assert!(nuc.detail.contains("0.44"),
            "clear detail should surface the hottest near-miss value, got {:?}", nuc.detail);
    }

    #[test]
    fn cross_domain_clear_surfaces_hottest_near_miss() {
        // Fewer than 3 elevated modalities anywhere → clear, but the detail must report
        // the hottest theater's count against the 3 needed (same legibility contract as
        // the nuclear/energy lights), so a theater one axis from tripping is visible
        // rather than hidden behind a bare "No theater with 3+ elevated modalities".
        let snap = RiskSnapshot {
            theaters: vec![
                theater("us_iran", EscalationRung::Tension, false,
                    &[("military_escalation", 0.40)], &["iran"]),
                // Two modalities elevated — one axis short of tripping the cross-domain light.
                theater("nato_russia", EscalationRung::Crisis, true,
                    &[("military_escalation", 0.50), ("diplomatic_breakdown", 0.40)],
                    &["russia", "united_states"]),
            ],
            ..Default::default()
        };
        let inds = evaluate(&snap);
        let cross = inds.iter().find(|i| i.id == "cross_domain").unwrap();
        assert!(!cross.tripped, "no theater with 3+ elevated modalities must read clear");
        assert!(cross.detail.contains("nato_russia") && cross.detail.contains("2/3"),
            "clear detail should surface the hottest near-miss count, got {:?}", cross.detail);
    }

    #[test]
    fn alliance_light_names_the_invoking_theater() {
        // When a mutual-defense alliance is invoked, the light must name the theater
        // carrying the collective-defense signal (same theater-attribution idiom as the
        // kinetic / nuclear / chokepoint lights), not just report a bare global signal.
        let mut snap = RiskSnapshot::default();
        let mut t = theater("nato_russia", EscalationRung::LimitedWar, true,
            &[("military_escalation", 0.60)], &["russia", "nato", "united_states"]);
        t.alliance_invoked = true;
        snap.theaters = vec![
            theater("us_iran", EscalationRung::Tension, false, &[], &["iran"]),
            t,
        ];
        // Coupler derived as theater.rs would: an alliance invoked in a hot theater.
        snap.couplers.alliance_activation = 1.0;
        let inds = evaluate(&snap);
        let alliance = inds.iter().find(|i| i.id == "alliance_invoked").unwrap();
        assert!(alliance.tripped, "an invoked alliance must trip the light");
        assert_eq!(alliance.theater.as_deref(), Some("nato_russia"),
            "the tripped light must name the theater that invoked collective defense");
        assert!(alliance.detail.contains("nato_russia"),
            "detail should name the invoking theater, got {:?}", alliance.detail);
    }

    #[test]
    fn alliance_light_names_the_hottest_invoking_theater() {
        // Regression guard: when more than one theater has invoked collective defense,
        // the light must name the HOTTEST one — the theater whose hot invocation drives
        // `alliance_activation` to its 1.0 apex — not merely the first in list order. A
        // cold invocation that happens to sort first must not steal the attribution from
        // the hot theater actually carrying the signal.
        let mut snap = RiskSnapshot::default();
        // Cold alliance invocation, listed FIRST (would be picked by the old `find`).
        let mut cold = theater("us_iran", EscalationRung::Tension, true,
            &[("military_escalation", 0.20)], &["united_states", "iran"]);
        cold.alliance_invoked = true;
        cold.heat = 0.15;
        // Hot alliance invocation, listed SECOND — this is the signal-carrying theater.
        let mut hot = theater("nato_russia", EscalationRung::LimitedWar, true,
            &[("military_escalation", 0.70)], &["russia", "nato", "united_states"]);
        hot.alliance_invoked = true;
        hot.heat = 0.85;
        snap.theaters = vec![cold, hot];
        snap.couplers.alliance_activation = 1.0;
        let inds = evaluate(&snap);
        let alliance = inds.iter().find(|i| i.id == "alliance_invoked").unwrap();
        assert!(alliance.tripped, "an invoked alliance must trip the light");
        assert_eq!(alliance.theater.as_deref(), Some("nato_russia"),
            "the light must name the hottest alliance-invoked theater, not the first listed");
        assert!(alliance.detail.contains("nato_russia"),
            "detail should name the hottest invoking theater, got {:?}", alliance.detail);
    }

    #[test]
    fn alliance_light_clear_when_none_invoked() {
        // No theater with an invoked alliance → clear, unnamed, "None".
        let snap = RiskSnapshot {
            theaters: vec![
                theater("us_iran", EscalationRung::Tension, false, &[], &["iran"]),
            ],
            ..Default::default()
        };
        let inds = evaluate(&snap);
        let alliance = inds.iter().find(|i| i.id == "alliance_invoked").unwrap();
        assert!(!alliance.tripped, "no invoked alliance must read clear");
        assert!(alliance.theater.is_none(), "a clear alliance light must name no theater");
        assert_eq!(alliance.detail, "None");
    }

    #[test]
    fn alliance_light_stable_only_invocation_reads_clear_and_unnamed() {
        // Honesty lock (post 2026-07-11 coupler de-leak): a mutual-defense invocation in a
        // STABLE theater (heat < STABLE_HEAT_CEILING) no longer activates the coupler, so
        // `alliance_activation == 0.0`. The light's THREE fields must agree — a not-tripped
        // light must not simultaneously name a theater and assert an Article 5 signal. This
        // is the exact "stray treaty-consultation headline in a quiet theater" case the
        // coupler's honesty floor describes as reachable.
        let mut snap = RiskSnapshot::default();
        let mut stable = theater("us_iran", EscalationRung::Stable, false,
            &[("military_escalation", 0.02)], &["iran", "united_states"]);
        stable.alliance_invoked = true;
        stable.heat = 0.03; // Stable — strictly below STABLE_HEAT_CEILING (0.06)
        snap.theaters = vec![stable];
        // Coupler as theater.rs computes it for a Stable-only invocation: ZERO.
        snap.couplers.alliance_activation = 0.0;
        let inds = evaluate(&snap);
        let alliance = inds.iter().find(|i| i.id == "alliance_invoked").unwrap();
        assert!(!alliance.tripped,
            "a Stable-only invocation leaves the coupler at 0.0 → the light reads clear");
        assert!(alliance.theater.is_none(),
            "a not-tripped alliance light must name NO theater (it did before the fix)");
        assert_eq!(alliance.detail, "None",
            "a not-tripped alliance light must not assert an Article 5 signal, got {:?}",
            alliance.detail);
    }

    #[test]
    fn cross_domain_light_tracks_the_model_elevation_threshold_and_modality_set() {
        use crate::bayesian::DOMAIN_WEIGHTS;
        use crate::models::ELEVATION_THRESHOLD;
        // The cross-domain light's notion of an "elevated modality" must be the MODEL's —
        // `ELEVATION_THRESHOLD` over the model's own `DOMAIN_WEIGHTS` set — not a hardcoded
        // value/list that can silently drift from the headline. Lock both halves:

        // (a) Threshold boundary tracks the constant: every model modality set to EXACTLY
        //     ELEVATION_THRESHOLD counts as elevated → n == DOMAIN_WEIGHTS.len() (≥3 → trips).
        //     If the code kept a stale hardcoded threshold while the constant changed, a
        //     modality placed at the new threshold would fall on the wrong side and break this.
        let at: Vec<(&str, f64)> =
            DOMAIN_WEIGHTS.iter().map(|(m, _)| (*m, ELEVATION_THRESHOLD)).collect();
        let snap = RiskSnapshot {
            theaters: vec![theater("nato_russia", EscalationRung::Crisis, true, &at,
                &["russia", "united_states"])],
            ..Default::default()
        };
        let cross = evaluate(&snap).into_iter().find(|i| i.id == "cross_domain").unwrap();
        assert!(cross.tripped, "all model modalities at the elevation threshold must trip");
        assert!(cross.detail.contains(&format!("{} modalities", DOMAIN_WEIGHTS.len())),
            "the count must span the model's whole modality set, got {:?}", cross.detail);

        // (b) Just BELOW the constant is not elevated: every modality at ELEVATION_THRESHOLD
        //     − 0.01 counts zero → clear. A stale hardcoded threshold below the (changed)
        //     constant would still count these and wrongly trip.
        let below: Vec<(&str, f64)> =
            DOMAIN_WEIGHTS.iter().map(|(m, _)| (*m, ELEVATION_THRESHOLD - 0.01)).collect();
        let snap = RiskSnapshot {
            theaters: vec![theater("nato_russia", EscalationRung::Crisis, true, &below,
                &["russia", "united_states"])],
            ..Default::default()
        };
        let cross = evaluate(&snap).into_iter().find(|i| i.id == "cross_domain").unwrap();
        assert!(!cross.tripped, "modalities just below the elevation threshold must read clear");
        assert!(cross.detail.contains("0/3"),
            "near-miss detail should report 0/3 elevated, got {:?}", cross.detail);
    }

    #[test]
    fn apex_lights_name_the_hottest_qualifying_theater() {
        use crate::theater::BRINK_NUCLEAR_THRESHOLD;
        // The two APEX lights (gp_kinetic, nuclear_brink — the board's red, highest-stakes
        // great-power-war conditions) must point their WHERE attribution at the HOTTEST
        // qualifying theater, not whichever sorts first in `theaters` — the same rule the
        // alliance light already enforces. Regression guard: a cooler/lesser qualifier
        // listed FIRST must not steal the apex attribution from the hotter one listed second.

        // ── gp_kinetic: a LimitedWar GP theater listed first, a GreatPowerWar one second.
        let mut lesser = theater("us_iran", EscalationRung::LimitedWar, true,
            &[("military_escalation", 0.60)], &["united_states", "iran"]);
        lesser.heat = 0.55;
        let mut greater = theater("nato_russia", EscalationRung::GreatPowerWar, true,
            &[("military_escalation", 0.90)], &["russia", "nato", "united_states"]);
        greater.heat = 0.90;
        let snap = RiskSnapshot {
            theaters: vec![lesser, greater],
            ..Default::default()
        };
        let inds = evaluate(&snap);
        let gp = inds.iter().find(|i| i.id == "gp_kinetic").unwrap();
        assert!(gp.tripped, "two great powers at war must trip the kinetic light");
        assert_eq!(gp.theater.as_deref(), Some("nato_russia"),
            "the apex attribution must name the most-escalated theater, not the first listed");
        // Detail must LEAD with the most-escalated theater (hottest-first ordering).
        let di_greater = gp.detail.find("nato_russia").unwrap();
        let di_lesser = gp.detail.find("us_iran").unwrap();
        assert!(di_greater < di_lesser,
            "detail should list the most-escalated theater first, got {:?}", gp.detail);

        // ── nuclear_brink: two brink theaters, the hotter listed SECOND.
        let two_gp = ["united_states", "russia"];
        let mut cool_brink = theater("indo_pacific", EscalationRung::GreatPowerWar, true,
            &[("nuclear_posture", BRINK_NUCLEAR_THRESHOLD + 0.02)], &two_gp);
        cool_brink.heat = 0.60;
        let mut hot_brink = theater("nato_russia", EscalationRung::GreatPowerWar, true,
            &[("nuclear_posture", BRINK_NUCLEAR_THRESHOLD + 0.10)], &two_gp);
        hot_brink.heat = 0.95;
        let snap = RiskSnapshot {
            theaters: vec![cool_brink, hot_brink],
            ..Default::default()
        };
        let inds = evaluate(&snap);
        let brink = inds.iter().find(|i| i.id == "nuclear_brink").unwrap();
        assert!(brink.tripped, "a direct nuclear-superpower confrontation must trip the apex");
        assert_eq!(brink.theater.as_deref(), Some("nato_russia"),
            "the apex brink must name the hottest brink theater, not the first listed");
    }

    #[test]
    fn nuclear_brink_indicator_matches_systemic_amplifier() {
        use crate::theater::{theater_is_nuclear_brink, BRINK_NUCLEAR_THRESHOLD};
        // The board's "nuclear-brink (apex)" light must trip on EXACTLY the condition
        // the systemic amplifier (theater.rs) uses, so the headline number and the
        // board that explains it can never disagree about whether the apex is live.
        // (Regression guard: the board once tripped at nuclear ≥0.70 while the
        // amplifier required ≥0.78, over-claiming the apex in the 0.70–0.78 band.)
        let two_gp = ["united_states", "russia"];

        // Just below the unified threshold with 2 great powers → NOT a brink.
        let under = theater("nato_russia", EscalationRung::GreatPowerWar, true,
            &[("nuclear_posture", BRINK_NUCLEAR_THRESHOLD - 0.02)], &two_gp);
        // Just above → a brink.
        let over = theater("nato_russia", EscalationRung::GreatPowerWar, true,
            &[("nuclear_posture", BRINK_NUCLEAR_THRESHOLD + 0.02)], &two_gp);
        // Above the nuclear threshold but only ONE great power → NOT a brink.
        let one_gp = theater("nato_russia", EscalationRung::GreatPowerWar, true,
            &[("nuclear_posture", BRINK_NUCLEAR_THRESHOLD + 0.02)], &["russia"]);

        // Model predicate.
        assert!(!theater_is_nuclear_brink(&under), "below threshold is not a brink");
        assert!(theater_is_nuclear_brink(&over), "above threshold + 2 GP is a brink");
        assert!(!theater_is_nuclear_brink(&one_gp), "one great power is not a brink");

        // The board must agree with the predicate in every case.
        let board_trips = |t: &TheaterState| {
            let snap = RiskSnapshot {
                theaters: vec![t.clone()],
                ..Default::default()
            };
            evaluate(&snap).iter().find(|i| i.id == "nuclear_brink").unwrap().tripped
        };
        assert!(!board_trips(&under),
            "board must NOT show apex brink below the amplifier's threshold");
        assert!(board_trips(&over),
            "board must show apex brink exactly when the amplifier engages");
        assert!(!board_trips(&one_gp),
            "a single great power is not a brink, on the board or in the model");
    }

    #[test]
    fn active_escalation_trips_on_a_hot_rising_theater_and_names_the_hottest() {
        // The board's only VELOCITY light: a theater at Crisis+ that is ALSO rising must
        // trip it, naming the HOTTEST such theater (not the first listed) and surfacing
        // the rising driver as the WHY. Regression guard: a cooler rising flashpoint
        // listed FIRST must not steal the attribution from a hotter one listed second.
        let mut cool = theater("us_iran", EscalationRung::Crisis, true,
            &[("military_escalation", 0.40)], &["united_states", "iran"]);
        cool.trend = "rising".into(); cool.heat = 0.30; cool.delta = 0.05;
        let mut hot = theater("nato_russia", EscalationRung::LimitedWar, true,
            &[("military_escalation", 0.70)], &["russia", "united_states"]);
        hot.trend = "rising".into(); hot.heat = 0.55; hot.delta = 0.12;
        hot.rising_driver = "military_escalation".into();
        let snap = RiskSnapshot { theaters: vec![cool, hot], ..Default::default() };

        let inds = evaluate(&snap);
        let esc = inds.iter().find(|i| i.id == "active_escalation").unwrap();
        assert!(esc.tripped, "a Crisis+ theater that is rising must trip the velocity light");
        assert_eq!(esc.theater.as_deref(), Some("nato_russia"),
            "must name the HOTTEST escalating theater, not the first listed");
        assert!(esc.detail.contains("military_escalation"),
            "detail should surface the rising driver (the WHY), got {:?}", esc.detail);
    }

    #[test]
    fn active_escalation_requires_velocity_not_just_level() {
        // A hot but NON-rising theater must read CLEAR — standing level alone is not
        // escalation (the other eleven lights already cover level). The clear detail must
        // surface the hottest theater that IS rising at all, even one below Crisis, so a
        // sub-Crisis flashpoint heating up stays visible rather than hidden.
        let mut hot_stable = theater("nato_russia", EscalationRung::LimitedWar, true,
            &[("military_escalation", 0.70)], &["russia", "united_states"]);
        hot_stable.trend = "stable".into(); hot_stable.heat = 0.55;
        let mut warming = theater("us_china_taiwan", EscalationRung::Tension, true,
            &[("military_escalation", 0.20)], &["china", "united_states"]);
        warming.trend = "rising".into(); warming.heat = 0.15; warming.delta = 0.03;
        let snap = RiskSnapshot { theaters: vec![hot_stable, warming], ..Default::default() };

        let inds = evaluate(&snap);
        let esc = inds.iter().find(|i| i.id == "active_escalation").unwrap();
        assert!(!esc.tripped, "a hot but STABLE theater must not trip the velocity light");
        assert!(esc.detail.contains("us_china_taiwan"),
            "clear detail should name the hottest theater rising at all, got {:?}", esc.detail);
    }

    #[test]
    fn seismic_test_light_trips_off_the_snapshot_flag_and_names_the_site() {
        // The 11th light surfaces the seismic monitor's test-consistent determination onto
        // the board. It must trip purely off the snapshot's `seismic_test_consistent` flag
        // (set by the aggregator from `SeismicAlert::is_test_consistent`), name the site as
        // the WHERE, and — being a physical-indicator heuristic, not a great-power-WAR state
        // — must NOT be apex (it stays amber so it can't paint itself the same red as a
        // confirmed great-power war).
        let snap = RiskSnapshot {
            seismic_test_consistent: true,
            seismic_site: "Punggye-ri".into(),
            ..Default::default()
        };
        let inds = evaluate(&snap);
        let s = inds.iter().find(|i| i.id == "seismic_test").unwrap();
        assert!(s.tripped, "the seismic light must trip when the snapshot flag is set");
        assert_eq!(s.theater.as_deref(), Some("Punggye-ri"), "must name the test site as the WHERE");
        assert!(s.detail.contains("Punggye-ri"), "detail should name the site, got {:?}", s.detail);
        assert!(!s.is_apex(), "the seismic light is amber, not apex");

        // Default (no live anomaly) reads CLEAR, with no site attribution.
        let clear = evaluate(&RiskSnapshot::default());
        let sc = clear.iter().find(|i| i.id == "seismic_test").unwrap();
        assert!(!sc.tripped && sc.theater.is_none(),
            "no test-consistent anomaly must read clear with no site");
    }
}
