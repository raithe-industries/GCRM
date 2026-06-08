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

use serde::Serialize;

use crate::models::{EscalationRung, RiskSnapshot};

#[derive(Debug, Clone, Serialize)]
pub struct Indicator {
    pub id:      &'static str,
    pub label:   &'static str,
    pub tripped: bool,
    pub theater: Option<String>, // which theater tripped it, if specific
    pub detail:  String,
}

fn modality(snap_theater: &crate::models::TheaterState, m: &str) -> f64 {
    snap_theater.modality_scores.get(m).copied().unwrap_or(0.0)
}

/// Evaluate the full I&W checklist against the current snapshot. Returns every
/// indicator (tripped or not) so the dashboard can render the whole board.
pub fn evaluate(snap: &RiskSnapshot) -> Vec<Indicator> {
    let theaters = &snap.theaters;
    let c = &snap.couplers;

    // 1. Great-power kinetic conflict active.
    let gp_kinetic: Vec<&crate::models::TheaterState> = theaters.iter()
        .filter(|t| t.gp_involved && t.rung.level() >= EscalationRung::LimitedWar.level())
        .collect();
    let ind_gp_kinetic = Indicator {
        id: "gp_kinetic", label: "Great-power kinetic conflict active",
        tripped: !gp_kinetic.is_empty(),
        theater: gp_kinetic.first().map(|t| t.label.clone()),
        detail: if gp_kinetic.is_empty() { "No great power in active war".into() }
                else { format!("{} theater(s): {}", gp_kinetic.len(),
                    gp_kinetic.iter().map(|t| t.label.as_str()).collect::<Vec<_>>().join(", ")) },
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

    // 4. Multiple theaters concurrently hot.
    let ind_concurrency = Indicator {
        id: "multi_theater", label: "Multiple theaters concurrently hot",
        tripped: c.concurrency >= 1.8, theater: None,
        detail: format!("{:.1} theaters hot", c.concurrency),
    };

    // 5. Multiple great powers entangled.
    let ind_gp_entangle = Indicator {
        id: "gp_entanglement", label: "Multiple great powers entangled",
        tripped: c.gp_entanglement >= 0.60, theater: None,
        detail: format!("entanglement {:.2}", c.gp_entanglement),
    };

    // 6. Mutual-defense alliance invoked. Name the theater carrying the
    //    collective-defense signal (same theater-attribution idiom as the kinetic /
    //    nuclear / chokepoint / cross-domain lights), so the operator can see WHERE
    //    Article 5 tripped rather than a bare global "Article 5 / collective-defense
    //    signal". The coupler `alliance_activation` is derived from these theaters'
    //    `alliance_invoked` flags, so it is > 0.0 exactly when some theater is found.
    //    Pick the HOTTEST alliance-invoked theater (not merely the first in list
    //    order): a HOT invocation is what drives `alliance_activation` to its 1.0
    //    apex, so naming the hottest keeps the label pointed at the theater actually
    //    carrying the signal rather than a cold invocation that happens to sort first.
    let alliance_theater = theaters.iter()
        .filter(|t| t.alliance_invoked)
        .max_by(|a, b| a.heat.partial_cmp(&b.heat).unwrap_or(std::cmp::Ordering::Equal));
    let ind_alliance = Indicator {
        id: "alliance_invoked", label: "Mutual-defense alliance invoked",
        tripped: c.alliance_activation > 0.0,
        theater: alliance_theater.map(|t| t.label.clone()),
        detail: match alliance_theater {
            Some(t) => format!("Article 5 / collective-defense signal: {}", t.label),
            None => "None".into(),
        },
    };

    // 7. Arms-control guardrails collapsed.
    let ind_guardrails = Indicator {
        id: "guardrails", label: "Arms-control guardrails collapsed",
        tripped: c.guardrail_collapse >= 0.70, theater: None,
        detail: format!("guardrail collapse {:.2}", c.guardrail_collapse),
    };

    // 8. Cross-domain escalation within a single theater (≥3 modalities elevated).
    //    On a clear reading, surface the hottest near-miss (how many modalities the
    //    leading theater has elevated, against the 3 needed) — same legibility idiom as
    //    the nuclear/energy lights — so a theater sitting at 2/3 (one axis from tripping)
    //    is distinguishable from a quiet board, rather than a bare "No theater with 3+".
    let cross = theaters.iter().map(|t| {
        let n = ["military_escalation","nuclear_posture","economic_warfare","cyber_info_ops","diplomatic_breakdown"]
            .iter().filter(|m| modality(t, m) >= 0.32).count();
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

    // 9. Nuclear-brink configuration (direct ≥2-great-power nuclear confrontation).
    // Uses the SAME `theater_is_nuclear_brink` predicate as the systemic amplifier
    // (theater.rs), so this board light trips on exactly the state where the headline's
    // 1.70× apex amplifier engages — the number and the board can never disagree about
    // whether the apex configuration is live. (Previously this tripped at nuclear ≥0.70
    // while the amplifier required ≥0.78, so the board over-claimed the apex in the
    // 0.70–0.78 band.)
    let brink = theaters.iter().find(|t| crate::theater::theater_is_nuclear_brink(t));
    let ind_brink = Indicator {
        id: "nuclear_brink", label: "Nuclear-brink configuration (apex)",
        tripped: brink.is_some(), theater: brink.map(|t| t.label.clone()),
        detail: match brink { Some(t) => format!("Direct nuclear-superpower confrontation: {}", t.label),
                              None => "No direct nuclear-superpower brink".into() },
    };

    vec![ind_gp_kinetic, ind_nuclear, ind_energy, ind_concurrency, ind_gp_entangle,
         ind_alliance, ind_guardrails, ind_cross, ind_brink]
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
        }
    }

    #[test]
    fn empty_snapshot_trips_nothing() {
        let snap = RiskSnapshot::default();
        let inds = evaluate(&snap);
        assert_eq!(inds.len(), 9);
        assert!(inds.iter().all(|i| !i.tripped));
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
        assert!(trip("guardrails"));
        assert!(trip("cross_domain"));
        assert!(trip("nuclear_brink"), "nato_russia has nuclear 0.80 + US & Russia → brink");
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
}
