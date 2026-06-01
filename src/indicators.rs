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

fn great_power_count(actor_ids: &[String]) -> usize {
    let mut set = std::collections::HashSet::new();
    for a in actor_ids {
        let lbl = match a.as_str() {
            "united_states" | "united_states_military" => Some("us"),
            "russia" | "russia_military"               => Some("russia"),
            "china"  | "china_military"                => Some("china"),
            "nato"                                      => Some("nato"),
            _ => None,
        };
        if let Some(l) = lbl { set.insert(l); }
    }
    set.len()
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

    // 2. Nuclear signaling elevated.
    let nuc = theaters.iter().map(|t| (t, modality(t, "nuclear_posture")))
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    let ind_nuclear = match nuc {
        Some((t, v)) if v >= 0.45 => Indicator {
            id: "nuclear_signaling", label: "Nuclear signaling elevated", tripped: true,
            theater: Some(t.label.clone()), detail: format!("{} nuclear posture {:.2}", t.label, v) },
        _ => Indicator { id: "nuclear_signaling", label: "Nuclear signaling elevated",
            tripped: false, theater: None, detail: "Below threshold".into() },
    };

    // 3. Energy / chokepoint weaponized (Gulf economic modality — Hormuz).
    let gulf_eco = theaters.iter().find(|t| t.theater_id == "us_iran").map(|t| modality(t, "economic_warfare")).unwrap_or(0.0);
    let ind_energy = Indicator {
        id: "energy_chokepoint", label: "Energy / chokepoint weaponized",
        tripped: gulf_eco >= 0.45, theater: Some("US/Israel–Iran".into()),
        detail: format!("Gulf coercive-economic {:.2}", gulf_eco),
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

    // 6. Mutual-defense alliance invoked.
    let ind_alliance = Indicator {
        id: "alliance_invoked", label: "Mutual-defense alliance invoked",
        tripped: c.alliance_activation > 0.0, theater: None,
        detail: if c.alliance_activation > 0.0 { "Article 5 / collective-defense signal".into() }
                else { "None".into() },
    };

    // 7. Arms-control guardrails collapsed.
    let ind_guardrails = Indicator {
        id: "guardrails", label: "Arms-control guardrails collapsed",
        tripped: c.guardrail_collapse >= 0.70, theater: None,
        detail: format!("guardrail collapse {:.2}", c.guardrail_collapse),
    };

    // 8. Cross-domain escalation within a single theater (≥3 modalities elevated).
    let cross = theaters.iter().map(|t| {
        let n = ["military_escalation","nuclear_posture","economic_warfare","cyber_info_ops","diplomatic_breakdown"]
            .iter().filter(|m| modality(t, m) >= 0.32).count();
        (t, n)
    }).max_by_key(|(_, n)| *n);
    let ind_cross = match cross {
        Some((t, n)) if n >= 3 => Indicator {
            id: "cross_domain", label: "Cross-domain escalation in one theater", tripped: true,
            theater: Some(t.label.clone()), detail: format!("{} modalities elevated in {}", n, t.label) },
        _ => Indicator { id: "cross_domain", label: "Cross-domain escalation in one theater",
            tripped: false, theater: None, detail: "No theater with 3+ elevated modalities".into() },
    };

    // 9. Nuclear-brink configuration (direct ≥2-great-power nuclear confrontation).
    let brink = theaters.iter().find(|t|
        modality(t, "nuclear_posture") >= 0.70 && great_power_count(&t.top_actors) >= 2);
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
        let mut snap = RiskSnapshot::default();
        snap.theaters = vec![
            theater("us_iran", EscalationRung::GreatPowerWar, true,
                &[("military_escalation",0.7),("economic_warfare",0.6),("diplomatic_breakdown",0.5)],
                &["united_states","iran"]),
            theater("nato_russia", EscalationRung::GreatPowerWar, true,
                &[("military_escalation",0.6),("nuclear_posture",0.75),("diplomatic_breakdown",0.5)],
                &["united_states","russia"]),
        ];
        snap.couplers = SystemicCouplers {
            gp_entanglement: 1.0, alliance_activation: 0.0, concurrency: 2.5,
            guardrail_collapse: 1.0, coupling_multiplier: 2.0,
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
        assert!(trip("nuclear_brink"), "nato_russia has nuclear 0.75 + US & Russia → brink");
    }
}
