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

use crate::bayesian::{co_occurrence_boost, domain_weight, recency_weight, soft_elevation_weight, DomainScorer, DOMAIN_WEIGHTS};
use crate::models::{
    DomainScore, EscalationRung, GeopoliticalEvent, SystemicCouplers, Theater, TheaterState,
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

/// Heat below which a theater sits on the **Stable** rung — i.e. nothing is
/// happening there worth amplifying. This is the honesty floor the systemic
/// couplers must respect: a Stable theater must contribute EXACTLY ZERO to the
/// concurrency / great-power-entanglement / alliance amplifiers, or a quiet world
/// would silently inflate the headline. That holds today because the concurrency
/// ramp's lower edge (HOT_HEAT − HOT_RAMP = 0.12) and the entanglement/alliance
/// gate (heat ≥ HOT_HEAT = 0.18) both sit strictly above this ceiling — a
/// relationship LOCKED by `quiet_theater_never_leaks_into_couplers` so a future
/// recalibration of the ramp can't dishonestly let stable theaters leak.
const STABLE_HEAT_CEILING: f64 = 0.06;

// ── Escalation-rung heat boundaries ──────────────────────────────────────────
// The heat→rung band is partitioned by FOUR boundaries, shared as the single
// source of truth by `rung_for` (which rung a heat lands in) and `within_band`
// (where inside that rung's band it sits). Both functions MUST read the same
// boundaries: the systemic index is `(rung.level() + within_band)/6`, so if the
// two drifted, a heat just inside a rung could report a within-band fraction that
// no longer matches its band — the index would jump discontinuously at the seam
// (a heat one ulp either side of a boundary would read wildly different) and
// silently lie about how far up the rung a theater is. The lower two boundaries
// already carry semantic names (`STABLE_HEAT_CEILING` = Stable→Tension,
// `HOT_HEAT` = Tension→Crisis); the upper two are named here so all four live in
// exactly one place. Locked by `rung_for_and_within_band_share_one_contiguous_partition`.
/// Crisis → Limited-War heat boundary (sustained kinetic conflict).
const LIMITED_WAR_HEAT: f64 = 0.38;
/// Limited-War → Great-Power-War heat boundary (great-power forces directly engaged).
const GREAT_POWER_WAR_HEAT: f64 = 0.62;

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

// ── Systemic amplifier weights (the couplers) ────────────────────────────────────
// HOW MUCH each systemic coupler may amplify the hottest theater's intensity into the
// headline likelihood. These are the model's most calibration-critical free parameters,
// so per roadmap 1.2 each is NAMED (not a bare literal) with its rationale and pinned by
// a test, and the design-intent RELATIONSHIPS between them are locked. Fitted against the
// backtest bands (quiet/Ukraine/current/Cuba) — do NOT blind-tweak; move them only with
// evidence + a test, and keep the relationships below intact.

/// Great-power entanglement weight in the coupling multiplier: a world where
/// `GP_ENTANGLEMENT_SATURATION` distinct great powers are directly entangled across hot
/// theaters is amplified by up to +45%. The largest coupler weight, because direct
/// great-power entanglement is the strongest single escalator from a regional war to a
/// systemic one.
pub const COUPLING_GP_WEIGHT: f64 = 0.45;

/// Alliance-activation weight in the coupling multiplier: a mutual-defense invocation in a
/// hot theater adds up to +30% (half that for an invocation in a non-hot theater). Below
/// the GP weight — an alliance call is a strong escalator but a step short of great powers
/// already directly entangled.
pub const COUPLING_ALLIANCE_WEIGHT: f64 = 0.30;

/// Distinct great powers that must be directly entangled across hot theaters to SATURATE
/// great-power entanglement at 1.0. Three (e.g. US + Russia + China all in it) is the
/// practical ceiling for a systemic configuration.
pub const GP_ENTANGLEMENT_SATURATION: f64 = 3.0;

/// Maximum additional amplification from multi-theater CONCURRENCY (breadth) as the number
/// of simultaneously-hot theaters grows without bound. Saturating (not linear) by
/// deliberate design (recalibrated 2026-06-03): each extra hot theater adds less. Kept
/// strictly BELOW `BRINK_AMPLIFIER` so breadth can never swamp the single-theater
/// nuclear-brink apex — the "breadth-swamps-brink" regression a previous linear
/// +0.12/theater term produced (a no-brink four-theater world pegged flat at the 0.90
/// ceiling). LOCKED by `breadth_never_swamps_the_nuclear_brink`.
pub const BREADTH_ASYMPTOTE: f64 = 0.26;

/// e-fold scale of the breadth saturation: at `breadth = BREADTH_EFOLD` extra hot theaters
/// the concurrency bonus has reached (1 − 1/e) ≈ 63% of its asymptote. Larger = slower
/// saturation. ~1.7 lands the live four-theater world at ~82% WITH resolution (headroom
/// below the ceiling) rather than pegged.
const BREADTH_EFOLD: f64 = 1.7;

/// Single-theater nuclear-brink (apex) amplifier: a direct ≥`BRINK_MIN_GREAT_POWERS`
/// great-power nuclear standoff within ONE theater (Cuba 1962) multiplies the systemic
/// likelihood by 1 + 0.70. Strictly greater than `BREADTH_ASYMPTOTE` by design so the apex
/// head-to-head always outweighs mere breadth at equal intensity.
pub const BRINK_AMPLIFIER: f64 = 0.70;

// ── Persistence floor (PROTOTYPE, 2026-06-21) ─────────────────────────────────────
// An active war is a sustained STATE, not a news pulse. The 72h kinetic half-life already
// keeps a theater stable across an overnight lull; this floor handles the multi-DAY tail
// ASYMMETRICALLY — fast rise, slow *earned* fall — so a war does not collapse during a
// several-day news gap, yet still fades if it goes truly silent or shows de-escalation.
//   heat = max(fast_heat, floor),  floor = FLOOR_FRACTION × slow_war_state_heat
// where slow_war_state_heat is the SAME intra-theater heat recomputed with a long half-life.
// Gated to theaters that reached sustained war and RELEASED on de-escalation evidence, so a
// quiet world never manufactures phantom heat. At peak freshness fast_heat == slow_heat, so
// the floor sits below fast_heat and the calibration bands (all scored at age 0) cannot move.
// Provisional — fitted against the persistence/diurnal backtest scenarios.

/// Multiplier on every domain half-life for the slow war-state floor. 5.0 turns the 72h kinetic
/// half-life into a ~15-day floor decay, so a silent war fades over weeks rather than days.
pub const WAR_STATE_HALF_LIFE_SCALE: f64 = 5.0;

/// The floor holds at this fraction of the slow war-state heat. Strictly < 1.0 so that at peak
/// freshness (fast_heat == slow_heat) floor < fast_heat → the live read is unchanged at age 0.
/// 0.85 (Robert, 2026-06-21): engages by ~24h and catches the 1–2 day "data-gap cliff" (a 3-war
/// world holds ~38% across a 2-day gap rather than decaying to ~31%), reflecting the chosen error
/// posture — err toward holding (false alarm) over premature stand-down (false calm). The
/// de-escalation gate still releases the floor regardless of this value.
pub const FLOOR_FRACTION: f64 = 0.85;

/// The floor engages only for a theater whose slow war-state heat reached at least sustained war
/// (Limited-War rung). A crisis/tension spike never earns a multi-week floor, and a quiet world
/// (slow heat ≈ 0) never gets phantom heat — the honesty invariant.
pub const WAR_STATE_FLOOR_GATE: f64 = LIMITED_WAR_HEAT;

/// Recency-weighted mean escalation_step below which a theater counts as genuinely DE-ESCALATING
/// (escalation_step is −1 ceasefire/deal … +1 escalatory). When de-escalation evidence dominates,
/// the floor is RELEASED so a real peace process cools the read quickly instead of being propped up.
pub const DEESCALATION_STEP_THRESHOLD: f64 = -0.30;

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

/// Whether the SYSTEMIC read's leading driver is a remembered war-state rather than fresh
/// fighting. The systemic index is monotone in theater heat, so the highest-heat theater is its
/// dominant contributor; this returns true when that lead theater's `heat` is `held_by_floor` —
/// i.e. the persistence floor is propping the headline up through a multi-day news gap (silence ≠
/// peace) with no fresh escalation driving the lead. False for a quiet world (no theater is
/// floor-held) and the moment de-escalation evidence releases the floor. The headline analog of
/// the per-theater `⏸ held` chip: a headline that rests on memory must say so (pillar-1). Single
/// source of truth for `meta.read_held_by_floor`, so the dashboard caveat can't drift from the model.
pub fn systemic_read_is_floor_held(theaters: &[TheaterState]) -> bool {
    theaters
        .iter()
        // heat is clamped finite in [0,1]; unwrap_or(Equal) keeps a stray NaN from panicking.
        .max_by(|a, b| a.heat.partial_cmp(&b.heat).unwrap_or(std::cmp::Ordering::Equal))
        .is_some_and(|lead| lead.held_by_floor)
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

/// Intra-theater heat from a set of modality scores: the weighted modality sum normalised by the
/// maximum possible, amplified by the shared intra-theater co-occurrence boost, capped at 1.0.
/// Factored out so the fast read and the slow war-state floor use the IDENTICAL formula.
fn heat_from_scores(scores: &HashMap<String, DomainScore>) -> f64 {
    let weighted: f64 = DOMAIN_WEIGHTS.iter()
        .map(|(m, _)| scores.get(*m).map(|d| d.score * domain_weight(m)).unwrap_or(0.0))
        .sum();
    let soft_elev: f64 = scores.values().map(|d| soft_elevation_weight(d.score)).sum();
    let cooc = co_occurrence_boost(soft_elev);
    ((weighted / max_weighted_sum()) * cooc).min(1.0)
}

/// Escalation momentum: the recency-weighted mean signed `escalation_step` of a theater's
/// events, in [−1, +1] (−1 ceasefire/deal … +1 escalatory). Recency-weighted on the military
/// half-life so a fresh ceasefire outweighs stale war chatter. Returns `None` when no event
/// carries non-negligible recency weight — a theater with no qualifying coverage has no news-flow
/// direction (silence is neither escalation nor de-escalation). This is the single computation
/// behind BOTH the de-escalation floor gate (`theater_is_deescalating`, a threshold on it) and the
/// operator-facing `escalation_momentum` gauge (the magnitude itself), so the gate and the readout
/// can never disagree about which way a theater's coverage is trending.
fn escalation_momentum(tev: &[GeopoliticalEvent]) -> Option<f64> {
    let (mut wsum, mut w) = (0.0_f64, 0.0_f64);
    for e in tev {
        let rw = recency_weight(&e.published_at, "military_escalation");
        if rw < 0.01 { continue; }
        wsum += rw * e.escalation_step;
        w += rw;
    }
    if w <= 0.0 { None } else { Some(wsum / w) }
}

/// Whether a theater's recent coverage shows genuine de-escalation — its `escalation_momentum`
/// is below `DEESCALATION_STEP_THRESHOLD`. A theater with no qualifying events is NOT
/// de-escalating (silence ≠ peace, so `None` → false), which is what makes the floor hold through
/// a lull but release on real conciliation.
fn theater_is_deescalating(tev: &[GeopoliticalEvent]) -> bool {
    escalation_momentum(tev).is_some_and(|m| m < DEESCALATION_STEP_THRESHOLD)
}

/// Map a theater's heat (+ overrides) to a discrete escalation rung.
fn rung_for(heat: f64, gp_involved: bool, wmd_used: bool, nuclear_used: bool) -> EscalationRung {
    let mut r = if heat < STABLE_HEAT_CEILING {
        EscalationRung::Stable
    } else if heat < HOT_HEAT {
        EscalationRung::Tension
    } else if heat < LIMITED_WAR_HEAT {
        EscalationRung::Crisis
    } else if heat < GREAT_POWER_WAR_HEAT {
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
    // hypotheticals, capability/posture statements, drills/tests, averted/denied
    // events, and UNVERIFIED allegations/rumours. ("may" is deliberately omitted — it
    // collides with the month; "claim*" is omitted — it collides with the casualty
    // idiom "the strike claimed N lives", which describes a real use.)
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
        // Unverified-allegation / uncertainty framing — a clean confirmation reads in
        // the definitive ("confirmed", "was used"), never hedged as merely alleged or
        // rumoured. (Recall is preserved: `any()` over the window still trips on a
        // single definitive headline even when other coverage is hedged.)
        "alleged", "allegedly", "allege", "alleges", "allegation", "allegations",
        "reportedly", "unconfirmed", "unverified", "purported", "purportedly",
        "rumored", "rumoured", "rumor", "rumour", "rumors", "rumours",
        // Drill / training / wargame / simulation framing — siblings of the
        // "drill"/"exercise"/"test" tokens above. A firehose surfaces these constantly
        // (e.g. "North Korea trumpets TRAINING for nuclear strikes", "…SIMULATES …Nuclear
        // Strikes"): a rehearsed/simulated strike is not a strike. ("simulate"/"simulated"/
        // "simulation" were present but the third-person "simulates" was not — whole-word
        // matching needs every inflection.)
        "train", "trains", "training", "trained", "drilled",
        "rehearse", "rehearses", "rehearsal", "rehearsals", "rehearsed",
        "wargame", "wargames", "maneuver", "maneuvers", "manoeuvre", "manoeuvres",
        "simulates",
        // Advocacy / call-for framing — someone URGING or PETITIONING for a strike is
        // describing a demand, not a detonation ("…urges legislature to petition … for
        // nuclear strike on Ukraine").
        "urge", "urges", "urged", "urging",
        "petition", "petitions", "petitioned",
        "advocate", "advocates", "advocated",
        // Explainer / authorization-process framing — "the PROCESS the US uses to
        // AUTHORIZE a nuclear strike" explains the mechanism, it does not report a use.
        "authorize", "authorizes", "authorized", "authorise", "authorises", "authorised",
        "authorization", "authorisation", "process",
        // Prospective two-way framing — a "nuclear exchange" / "strikes' exchange" is the
        // standard term for a HYPOTHETICAL mutual strike ("…confrontation may escalate into
        // nuclear strikes' exchange"), a sibling of the "scenario"/"hypothetical"/"brink"
        // tokens above. (Recall tradeoff accepted: a real exchange spawns cleaner confirming
        // headlines that `any()` still trips on.)
        "exchange", "exchanges",
    ];

    // Whole-word question words that, when they LEAD a headline, mark it as an
    // explainer/hypothetical rather than a confirmed-use report ("Can the president
    // launch a nuclear strike on his own?", "How would a nuclear strike unfold?"). A
    // real detonation is reported in the declarative, never led by an interrogative —
    // so a leading question word is treated as non-use framing.
    const INTERROGATIVE_LEAD: &[&str] = &[
        "can", "could", "should", "would", "will", "might",
        "is", "are", "do", "does", "did",
        "how", "what", "why", "who", "when", "where", "which",
    ];
    tev.iter().any(|e| {
        if !e.nuclear_indicator { return false; }
        let t = e.title.to_lowercase();
        if !USE_PHRASES.iter().any(|p| t.contains(p)) { return false; }
        // A headline that OPENS with a question word is an explainer/hypothetical, not a
        // confirmed-detonation report — decline before the token scan.
        if let Some(first) = t
            .split(|c: char| !c.is_alphanumeric())
            .find(|w| !w.is_empty())
        {
            if INTERROGATIVE_LEAD.contains(&first) { return false; }
        }
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
    /// Previous-tick raw modality scores per theater id (modality_id → 0..1), for the
    /// per-theater "what is rising" delta-driver. Kept separate from `prev_heat` because
    /// the rising driver is about *which modality moved*, not the aggregate heat.
    prev_scores: HashMap<String, HashMap<String, f64>>,
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
    /// Lift magnitude of the dominant *acute* coupler named in `couplers.coupling_driver`
    /// (0.0 when none fired). Internal — not serialized. The Bayesian engine compares the
    /// regime-derived guardrail-collapse lift against this to decide whether the structural
    /// coupler is the true dominant amplifier, then overwrites `coupling_driver` if so.
    pub coupling_driver_lift: f64,
}

impl TheaterEngine {
    pub fn new() -> Self {
        Self { prev_heat: HashMap::new(), prev_scores: HashMap::new() }
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
        let gp_entanglement = (gp_set.len() as f64 / GP_ENTANGLEMENT_SATURATION).min(1.0);

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
        //
        // Identify WHICH theater carries the brink (the most acute by nuclear posture),
        // not just whether one exists: the apex lever (BRINK_AMPLIFIER, +70%, the single
        // largest term in `l_sys`) lives in THAT theater, which — per the note above —
        // need NOT be the hottest by raw heat. The systemic "where" must name the brink
        // theater, not a louder conventional one. `any(theater_is_nuclear_brink)` is
        // exactly `brink_theater.is_some()`, so the amplifier is unchanged.
        let brink_theater = states.iter()
            .filter(|s| theater_is_nuclear_brink(s))
            .max_by(|a, b| {
                let na = a.modality_scores.get("nuclear_posture").copied().unwrap_or(0.0);
                let nb = b.modality_scores.get("nuclear_posture").copied().unwrap_or(0.0);
                na.partial_cmp(&nb).unwrap_or(std::cmp::Ordering::Equal)
            });
        let brink = if brink_theater.is_some() { 1.0 } else { 0.0 };

        // Multipliers. Coupling rewards great-power entanglement; concurrency rewards
        // multiple simultaneously-hot theaters with DIMINISHING returns; brink is the
        // single-theater apex amplifier.
        let coupling_multiplier =
            1.0 + COUPLING_GP_WEIGHT * gp_entanglement + COUPLING_ALLIANCE_WEIGHT * alliance_activation;
        // Saturating breadth (recalibrated 2026-06-03): each extra hot theater adds less,
        // asymptoting at +`BREADTH_ASYMPTOTE`. Previously linear (+0.12 per theater), which let
        // a no-brink FOUR-theater world (live 2026) drive l_sys ABOVE the Cuba nuclear-brink
        // apex and peg P(WWIII) flat at the 0.90 ceiling — breadth swamping the brink, the
        // opposite of the design intent. Saturating it lands that state at ~82% WITH
        // resolution, while quiet/ukraine/cuba (concurrency ≤ 1) are mathematically unchanged.
        // See the constants above for rationale; the breadth-vs-brink relationship is locked
        // by `breadth_never_swamps_the_nuclear_brink`.
        let breadth          = (concurrency - 1.0).max(0.0);
        let concurrency_mult = 1.0 + BREADTH_ASYMPTOTE * (1.0 - (-breadth / BREADTH_EFOLD).exp());
        let brink_mult       = 1.0 + BRINK_AMPLIFIER * brink;                // single-theater apex

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
        let driver = if top_heat < STABLE_HEAT_CEILING {
            "No theater above baseline".to_string()
        } else if let Some(bt) = brink_theater {
            // Apex configuration: lead the "where" with the nuclear-brink theater (the
            // +70% apex lever), even when a louder conventional theater is hotter by raw
            // heat. The hottest theater stays visible in the dashboard sub-line ("hottest:
            // …") and the ladder strip, so the operator gets BOTH apex and hottest.
            format!("{} at nuclear brink; {} theater{} hot",
                bt.label, hot_count, if hot_count == 1 { "" } else { "s" })
        } else {
            format!("{} at {}; {} theater{} hot",
                top_label, max_rung.label(), hot_count, if hot_count == 1 { "" } else { "s" })
        };

        // Which coupling channel is lifting the systemic likelihood most — read directly
        // off the multiplicative excesses that build l_sys (never a new lever). Answers
        // the systemic "why": is this close to a world war because of a single-theater
        // nuclear brink, great powers entangled across theaters, many theaters hot at
        // once, or an alliance invocation?
        let (coupling_driver, coupling_driver_lift) = dominant_coupling_amplifier(
            brink_mult - 1.0,
            COUPLING_GP_WEIGHT * gp_entanglement,
            concurrency_mult - 1.0,
            COUPLING_ALLIANCE_WEIGHT * alliance_activation,
        );
        let coupling_driver = coupling_driver.to_string();

        // Breadth saturation (HONESTY): every BREADTH/coupling amplifier of `l_sys` is at its
        // structural rail and no single-theater nuclear brink is live — so the read has run out
        // of resolution to further escalation of the crises ALREADY on the board. The hottest
        // theater's heat is clamped at the model maximum (`max_heat == 1.0`, so intensifying it
        // does nothing), great-power entanglement and alliance activation are both maxed, and
        // ≥2 theaters are hot (a genuine breadth peg, not a single maxed crisis). The only lever
        // left that can raise the read is a direct nuclear brink (`brink_mult`). This is the
        // saturated operating point the de-saturation backtest thread measured — a ~83% breadth
        // peg that sits BELOW the 0.90 forecast ceiling, so the existing `at_ceiling` caveat does
        // NOT fire and a bare number would read as a still-climbing point estimate. Surfaced so it
        // can't. Computed purely from the rails (`RAIL_EPS` absorbs float noise) — no fitted
        // constant touched, P unchanged.
        const RAIL_EPS: f64 = 1e-3;
        let breadth_saturated =
            hot_count >= 2
            && brink == 0.0
            && max_heat >= 1.0 - RAIL_EPS
            && gp_entanglement >= 1.0 - RAIL_EPS
            && alliance_activation >= 1.0 - RAIL_EPS;

        let couplers = SystemicCouplers {
            gp_entanglement,
            alliance_activation,
            concurrency: (concurrency * 1e3).round() / 1e3,
            guardrail_collapse: 0.0, // set by the caller from the regime multiplier
            coupling_multiplier: (coupling_multiplier * concurrency_mult * brink_mult * 1e4).round() / 1e4,
            coupling_driver,
            breadth_saturated,
        };

        TheaterOutput {
            theaters: states,
            couplers,
            l_sys: (l_sys * 1e6).round() / 1e6,
            systemic_index: (systemic_index * 1e2).round() / 1e2,
            driver,
            coupling_driver_lift,
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
            // No qualifying events → every modality is at zero. Nothing can be rising, so
            // there is no rising-driver to name; reset the per-modality history to zero so
            // a later re-escalation is measured from a clean baseline.
            self.prev_scores.insert(id.clone(), HashMap::new());
            return TheaterState {
                theater_id: id, label: theater.label().to_string(),
                rung: EscalationRung::Stable, rung_label: EscalationRung::Stable.label().to_string(),
                heat: 0.0, modality_scores: HashMap::new(),
                trend: trend.into(), delta: (delta * 1e4).round() / 1e4, event_count: 0,
                gp_involved: false, alliance_invoked: false, top_actors: vec![],
                top_driver: String::new(), rising_driver: String::new(),
                secondary_driver: String::new(), held_by_floor: false,
                fresh_rung_label: EscalationRung::Stable.label().to_string(),
                // No qualifying coverage → no news-flow direction (neutral, not "de-escalating").
                escalation_momentum: 0.0,
            };
        }

        // Peak-aware modality scoring on this theater's events (fresh scorer; the
        // anomaly detector simply never fires with no cross-tick history). The displayed
        // modality_scores below stay the FAST read — current evidence, not the floor.
        // Intra-theater co-occurrence (inside heat_from_scores): simultaneous modalities within
        // ONE theater are far more dangerous than the same breadth spread across the globe, and it
        // reuses the shared `soft_elevation_weight` ramp so "elevated" means one thing model-wide.
        let mut scorer = DomainScorer::new();
        let scores = scorer.score_all(tev);
        let fast_heat = heat_from_scores(&scores);

        // ── Persistence floor (PROTOTYPE) ──
        // A slowly-decaying war-state heat (same formula, long half-life) holds a hot theater up
        // through a multi-day lull. Gated to theaters that reached sustained war and released on
        // de-escalation evidence, so a quiet world never gets a phantom floor and a real peace
        // process cools fast. At age 0 slow_heat == fast_heat, so floor < fast_heat → no change.
        let mut slow_scorer = DomainScorer::new();
        let slow_scores = slow_scorer.score_all_scaled(tev, WAR_STATE_HALF_LIFE_SCALE);
        let slow_heat = heat_from_scores(&slow_scores);
        let floor = if slow_heat >= WAR_STATE_FLOOR_GATE && !theater_is_deescalating(tev) {
            FLOOR_FRACTION * slow_heat
        } else {
            0.0
        };
        let heat = fast_heat.max(floor).min(1.0);
        // The read is HELD when the floor strictly outweighs the fresh evidence: the
        // displayed heat is a remembered war-state carried through a news gap, not a live
        // measurement. Honest by construction; surfaced so the operator can tell a
        // live-hot theater from one the model is holding quiet (silence ≠ peace).
        let held_by_floor = floor > fast_heat;

        let gp_involved      = tev.iter().any(|e| e.great_power_involved);
        let alliance_invoked = tev.iter().any(|e| e.alliance_indicator);
        let wmd_used         = tev.iter().any(|e| e.wmd_indicator && e.severity > 0.6);
        let nuclear_used     = nuclear_use_in(tev);
        let rung = rung_for(heat, gp_involved, wmd_used, nuclear_used);
        // The rung the FRESH evidence alone supports (fast_heat, not the held floor). When the
        // floor is holding the read up (`held_by_floor`), this is ≤ the displayed rung and shows
        // the operator how far the live read has decayed below the remembered war-state. Equal to
        // `rung` whenever the floor is not lifting the displayed heat. Honest by construction.
        let fresh_rung = rung_for(fast_heat, gp_involved, wmd_used, nuclear_used);

        let delta = heat - prev;
        let trend = if delta > 0.005 { "rising" } else if delta < -0.005 { "falling" } else { "stable" };
        self.prev_heat.insert(id.clone(), heat);

        // Dominant tracked actors in this theater (by mention count).
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for e in tev {
            for a in &e.actor_ids { *counts.entry(a.as_str()).or_insert(0) += 1; }
        }
        let mut pairs: Vec<(&str, usize)> = counts.into_iter().collect();
        pairs.sort_by_key(|b| std::cmp::Reverse(b.1));
        let top_actors: Vec<String> = pairs.into_iter().take(4).map(|(a, _)| a.to_string()).collect();

        let modality_scores: HashMap<String, f64> = DOMAIN_WEIGHTS.iter()
            .map(|(m, _)| (m.to_string(), scores.get(*m).map(|d| d.score).unwrap_or(0.0)))
            .collect();

        // Per-theater "why": the modality contributing the most WEIGHTED heat — the
        // single largest `score × domain_weight` term in the sum that builds `heat`
        // above. This surfaces what *kind* of force is driving each flashpoint, not
        // just how hot it is (Awareness). Honest by construction: it names the model's
        // own dominant term, never a fitted/derived value. Empty for a Stable theater,
        // where there is no signal worth naming.
        let top_driver = if rung == EscalationRung::Stable {
            String::new()
        } else {
            DOMAIN_WEIGHTS.iter()
                .map(|(m, _)| (*m, scores.get(*m).map(|d| d.score).unwrap_or(0.0) * domain_weight(m)))
                .filter(|(_, contrib)| *contrib > 0.0)
                .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(m, _)| m.to_string())
                .unwrap_or_default()
        };

        // Per-theater "what is rising": the modality whose WEIGHTED score climbed the most
        // since the previous tick — the model's own answer to *why this flashpoint is heating
        // up*, which `top_driver` (the dominant LEVEL) cannot give: a theater can be hottest on
        // nuclear posture yet be rising because military escalation just jumped. Honest by
        // construction (the largest positive `Δscore × domain_weight` term), and only surfaced
        // when the theater is actually rising — a flat/cooling theater names nothing.
        let prev_scores = self.prev_scores.get(&id);
        let rising_driver = if trend == "rising" {
            DOMAIN_WEIGHTS.iter()
                .map(|(m, _)| {
                    let now  = scores.get(*m).map(|d| d.score).unwrap_or(0.0);
                    let was  = prev_scores.and_then(|p| p.get(*m)).copied().unwrap_or(0.0);
                    (*m, (now - was) * domain_weight(m))
                })
                .filter(|(_, gain)| *gain > 0.0)
                .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(m, _)| m.to_string())
                .unwrap_or_default()
        } else {
            String::new()
        };
        // Per-theater "second dimension": the second-largest WEIGHTED contributor among
        // the modalities the model considers *elevated* (raw score ≥ ELEVATION_THRESHOLD —
        // the same cutoff that feeds the intra-theater co-occurrence amplifier above). This
        // names the second active KIND of force, the co-occurrence story `top_driver` (one
        // dominant level) cannot tell: a theater hottest on nuclear posture that ALSO has
        // elevated military escalation is a two-dimensional crisis, not a one-dimensional
        // posture story — exactly what the co-occurrence boost responds to. Honest by
        // construction (the model's own second-largest elevated weighted term); empty unless
        // at least two modalities are elevated, so a single-dimension flashpoint names nothing.
        let secondary_driver = if rung == EscalationRung::Stable {
            String::new()
        } else {
            let mut elevated: Vec<(&str, f64)> = DOMAIN_WEIGHTS.iter()
                .map(|(m, _)| (*m, scores.get(*m).map(|d| d.score).unwrap_or(0.0)))
                .filter(|(_, s)| *s >= ELEVATION_THRESHOLD)
                .map(|(m, s)| (m, s * domain_weight(m)))
                .collect();
            elevated.sort_by(|(_, a), (_, b)| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
            elevated.get(1).map(|(m, _)| m.to_string()).unwrap_or_default()
        };

        // Escalation momentum: the recency-weighted DIRECTION of this theater's news flow
        // (the same mean the de-escalation floor gate thresholds), surfaced as a magnitude. A
        // leading signal distinct from `delta`/`trend` (which measure the heat SCORE's change):
        // talks can dominate the coverage (momentum < 0) while heat is still flat or held high.
        let momentum = (escalation_momentum(tev).unwrap_or(0.0) * 1e3).round() / 1e3;

        // Record this tick's raw modality scores for the next tick's delta-driver.
        let now_scores: HashMap<String, f64> = DOMAIN_WEIGHTS.iter()
            .map(|(m, _)| (m.to_string(), scores.get(*m).map(|d| d.score).unwrap_or(0.0)))
            .collect();
        self.prev_scores.insert(id.clone(), now_scores);

        TheaterState {
            theater_id: id, label: theater.label().to_string(),
            rung, rung_label: rung.label().to_string(),
            heat: (heat * 1e4).round() / 1e4,
            modality_scores,
            trend: trend.to_string(), delta: (delta * 1e4).round() / 1e4,
            event_count: tev.len(),
            gp_involved, alliance_invoked, top_actors,
            top_driver, rising_driver, secondary_driver,
            held_by_floor,
            fresh_rung_label: fresh_rung.label().to_string(),
            escalation_momentum: momentum,
        }
    }
}

/// Fractional position of `heat` within its rung's heat band → [0,1].
fn within_band(heat: f64, rung: EscalationRung) -> f64 {
    let (lo, hi) = match rung {
        EscalationRung::Stable        => (0.0, STABLE_HEAT_CEILING),
        EscalationRung::Tension       => (STABLE_HEAT_CEILING, HOT_HEAT),
        EscalationRung::Crisis        => (HOT_HEAT, LIMITED_WAR_HEAT),
        EscalationRung::LimitedWar    => (LIMITED_WAR_HEAT, GREAT_POWER_WAR_HEAT),
        EscalationRung::GreatPowerWar => (GREAT_POWER_WAR_HEAT, 1.0),
        EscalationRung::Systemic      => (1.0, 1.0),
    };
    if hi <= lo { return 1.0; }
    ((heat - lo) / (hi - lo)).clamp(0.0, 1.0)
}

/// Tiny floor so float dust on an otherwise-uncoupled world doesn't name a phantom
/// channel; well below the smallest real lift any coupler can produce when engaged.
/// Shared with the Bayesian engine so the structural guardrail coupler is held to the
/// same threshold as the four acute ones.
pub const COUPLING_AMPLIFIER_FLOOR: f64 = 1e-6;

/// Names the systemic *coupling* amplifier contributing the largest multiplicative lift
/// to the systemic likelihood — the model's own answer to "what is turning this regional
/// crisis into a *world*-war risk right now". The candidate channels are exactly the
/// multiplicative excesses that build `l_sys` (`brink_mult`/`coupling_multiplier`/
/// `concurrency_mult`): the single-theater nuclear brink, great-power entanglement,
/// multi-theater concurrency, and alliance activation. Each `*_lift` is that channel's
/// `(multiplier − 1)` contribution, so this is a pure read-out of the engine's own terms —
/// it can never disagree with the math and introduces no new lever.
///
/// Returns `("", 0.0)` when no channel lifts above `AMPLIFIER_FLOOR` (the risk is purely
/// single-theater heat — an honest "regional, not yet systemically coupled" read).
/// Ties resolve in apex-severity order (brink ≻ great-power entanglement ≻ concurrency ≻
/// alliance): the nuclear brink is the most dangerous configuration, so it wins any tie.
///
/// Returns the winning channel's label AND its lift magnitude. The magnitude lets a later
/// stage (the Bayesian engine, which alone knows the regime-derived guardrail-collapse
/// lift) compare the fifth, structural coupler against these four acute ones — see
/// `COUPLING_AMPLIFIER_FLOOR` and the guardrail overlay in `BayesianEngine::compute`.
pub fn dominant_coupling_amplifier(brink_lift: f64, gp_lift: f64, breadth_lift: f64, alliance_lift: f64) -> (&'static str, f64) {
    // Ordered by apex severity; the first strict-max wins, so ties favour the earlier
    // (more dangerous) channel.
    let channels = [
        ("single-theater nuclear brink", brink_lift),
        ("great-power entanglement",     gp_lift),
        ("multi-theater concurrency",    breadth_lift),
        ("alliance activation",          alliance_lift),
    ];
    let (label, lift) = channels.iter().fold(("", 0.0_f64), |best, &(l, v)| {
        if v > best.1 { (l, v) } else { best }
    });
    if lift > COUPLING_AMPLIFIER_FLOOR { (label, lift) } else { ("", 0.0) }
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
    fn escalation_momentum_surfaces_the_signed_news_flow_direction() {
        // The recency-weighted mean signed `escalation_step`, surfaced as a per-theater gauge in
        // [−1,+1] — the same quantity the de-escalation floor gate thresholds, exposed as a
        // magnitude rather than collapsed to a boolean, and a LEADING signal distinct from
        // heat/delta. Lock:
        //   (1)+(2) the SIGN tracks the dominant direction of the recent coverage;
        //   (3) equal-recency events average to their mean step (the model's own weighted mean);
        //   (4) a quiet/uncovered theater reports exactly 0.0 (silence has no direction);
        //   (5) the de-escalation floor gate still equals `momentum < threshold` — proving the
        //       extraction of `escalation_momentum` left `theater_is_deescalating` identical.
        let gulf = |out: &TheaterOutput| out.theaters.iter().find(|s| s.theater_id == "us_iran").unwrap().clone();
        let make = |step: f64| -> Vec<GeopoliticalEvent> {
            // All events fresh (published_at = now) → equal recency weight, so the weighted mean
            // is exactly `step` regardless of the (near-identical) weights.
            (0..6).map(|_| {
                let mut a = ev("us_iran", "military_escalation", 0.8, 0.7, &["united_states", "iran"], true);
                a.escalation_step = step;
                a
            }).collect()
        };

        // (1)+(2) sign tracks the direction of the news flow.
        let esc = gulf(&TheaterEngine::new().compute(&make(0.6)));
        assert!(esc.escalation_momentum > 0.0,
            "escalatory coverage → positive momentum, got {}", esc.escalation_momentum);
        let deesc = gulf(&TheaterEngine::new().compute(&make(-0.6)));
        assert!(deesc.escalation_momentum < 0.0,
            "de-escalatory coverage → negative momentum, got {}", deesc.escalation_momentum);

        // (3) equal-recency events average to their mean step (within 1e-3 display rounding).
        assert!((esc.escalation_momentum - 0.6).abs() < 2e-3,
            "equal-weight events average to the mean step 0.6, got {}", esc.escalation_momentum);
        assert!((deesc.escalation_momentum + 0.6).abs() < 2e-3,
            "got {}", deesc.escalation_momentum);

        // (4) a quiet world has no news-flow direction.
        let quiet = TheaterEngine::new().compute(&[]);
        assert!(quiet.theaters.iter().all(|s| s.escalation_momentum == 0.0),
            "a quiet world must report exactly 0.0 momentum (silence has no direction)");

        // The gauge reaches the served snapshot JSON (the operator/federation contract — an
        // additive, compatible field), as a number carrying the computed value.
        let j = serde_json::to_value(&esc).unwrap();
        assert!((j["escalation_momentum"].as_f64().unwrap() - esc.escalation_momentum).abs() < 1e-12,
            "escalation_momentum must serialize as a number into the snapshot");

        // (5) the de-escalation floor gate is exactly `momentum < DEESCALATION_STEP_THRESHOLD`,
        //     so the refactor that extracted the gauge kept the gate behaviour-identical.
        for step in [-0.9, -0.31, -0.30, -0.05, 0.0, 0.3, 0.9] {
            let evs = make(step);
            let m = escalation_momentum(&evs).unwrap();
            assert_eq!(theater_is_deescalating(&evs), m < DEESCALATION_STEP_THRESHOLD,
                "gate must equal momentum < threshold at step {step} (m={m})");
        }
    }

    #[test]
    fn held_by_floor_flags_a_war_carried_through_a_news_gap_not_a_fresh_read() {
        use chrono::Duration;
        // A sustained great-power war in one theater (strong kinetic + nuclear signal), aged to a
        // chosen number of hours and carrying a chosen signed escalation_step. The persistence
        // floor exists to hold exactly this read through a multi-day news gap; `held_by_floor`
        // must mark when the displayed heat is that remembered war-state, not the fresh evidence.
        let make = |age_h: i64, step: f64| -> Vec<GeopoliticalEvent> {
            let mut v = Vec::new();
            for _ in 0..8 {
                let mut a = ev("us_iran", "military_escalation", 0.95, 0.9, &["united_states", "iran"], true);
                let mut b = ev("us_iran", "nuclear_posture", 0.9, 0.9, &["iran"], false);
                a.published_at = Utc::now() - Duration::hours(age_h);
                b.published_at = Utc::now() - Duration::hours(age_h);
                a.escalation_step = step; b.escalation_step = step;
                v.push(a); v.push(b);
            }
            v
        };
        let gulf = |out: &TheaterOutput| out.theaters.iter().find(|s| s.theater_id == "us_iran").unwrap().clone();

        // (1) FRESH (age 0): slow_heat == fast_heat, so floor = FLOOR_FRACTION × slow_heat <
        //     fast_heat → NOT held. A live war reads as a live measurement.
        let fresh = gulf(&TheaterEngine::new().compute(&make(0, 0.2)));
        assert!(!fresh.held_by_floor,
            "a fresh active war is a live read, not floor-held; heat={}", fresh.heat);

        // (2) AGED 96h, no de-escalation evidence: the fast read has decayed below the slowly
        //     decaying war-state floor, so the displayed heat is HELD by memory — flagged.
        let aged = gulf(&TheaterEngine::new().compute(&make(96, 0.2)));
        assert!(aged.held_by_floor,
            "a 4-day-silent active war should be HELD by the floor and flagged; heat={}", aged.heat);

        // (3) AGED 96h WITH genuine de-escalation evidence (strongly negative step): the floor
        //     RELEASES, so nothing is held — the read cools honestly to the pure decay.
        let deesc = gulf(&TheaterEngine::new().compute(&make(96, -0.7)));
        assert!(!deesc.held_by_floor,
            "a de-escalating war releases the floor — not held; heat={}", deesc.heat);

        // (4) A quiet world never manufactures a held flag.
        let quiet = TheaterEngine::new().compute(&[]);
        assert!(quiet.theaters.iter().all(|s| !s.held_by_floor),
            "a quiet world must never flag a held read");
    }

    #[test]
    fn fresh_rung_label_shows_how_far_a_held_read_decayed_below_the_floor() {
        use chrono::Duration;
        // The persistence floor can hold a theater's displayed rung ABOVE what the fresh evidence
        // alone supports. `fresh_rung_label` names the live-evidence rung so an operator sees how
        // far the held read has decayed — in the same rung vocabulary. Honest by construction:
        //   * the fresh rung can NEVER read higher than the displayed rung (heat >= fast_heat), and
        //   * a live (not-held) read shows them equal, while a multi-day-silent war shows the floor
        //     strictly holding the rung above the fresh read at some age.
        let make = |age_h: i64| -> Vec<GeopoliticalEvent> {
            let mut v = Vec::new();
            for _ in 0..8 {
                let mut a = ev("us_iran", "military_escalation", 0.95, 0.9, &["united_states", "iran"], true);
                let mut b = ev("us_iran", "nuclear_posture", 0.9, 0.9, &["iran"], false);
                a.published_at = Utc::now() - Duration::hours(age_h);
                b.published_at = Utc::now() - Duration::hours(age_h);
                a.escalation_step = 0.2; b.escalation_step = 0.2;
                v.push(a); v.push(b);
            }
            v
        };
        let gulf = |out: &TheaterOutput| out.theaters.iter().find(|s| s.theater_id == "us_iran").unwrap().clone();
        // Map a displayed rung label back to its level for the never-higher invariant.
        let lvl = |label: &str| -> u8 {
            [EscalationRung::Stable, EscalationRung::Tension, EscalationRung::Crisis,
             EscalationRung::LimitedWar, EscalationRung::GreatPowerWar, EscalationRung::Systemic]
                .iter().find(|r| r.label() == label).map(|r| r.level())
                .unwrap_or_else(|| panic!("unknown rung label {label:?}"))
        };

        // (1) Fresh (age 0): not held, fresh rung == displayed rung (no floor lift).
        let fresh = gulf(&TheaterEngine::new().compute(&make(0)));
        assert!(!fresh.held_by_floor);
        assert_eq!(fresh.fresh_rung_label, fresh.rung_label,
            "a live read's fresh rung must equal its displayed rung");

        // (2) Across a multi-day silence the fresh rung never exceeds the displayed rung, and at
        //     some age the floor strictly holds the rung above the fresh read (a real demotion).
        let mut saw_strict_demotion = false;
        for age in [24, 48, 72, 96, 120, 168, 240] {
            let s = gulf(&TheaterEngine::new().compute(&make(age)));
            assert!(lvl(&s.fresh_rung_label) <= lvl(&s.rung_label),
                "fresh rung must never read higher than the displayed rung (age {age}h): fresh={} disp={}",
                s.fresh_rung_label, s.rung_label);
            if s.held_by_floor && lvl(&s.fresh_rung_label) < lvl(&s.rung_label) { saw_strict_demotion = true; }
        }
        assert!(saw_strict_demotion,
            "a multi-day-silent war must, at some age, show the floor holding the rung above the fresh read");
    }

    #[test]
    fn systemic_read_is_floor_held_when_the_lead_theater_is_held() {
        use chrono::Duration;
        // The headline analog of the per-theater held flag: the systemic index is monotone in
        // theater heat, so its lead (highest-heat) theater is its dominant driver. The aggregate
        // flag must trip exactly when that lead is being HELD by the persistence floor — the
        // headline rests on a remembered war-state, not fresh fighting.
        let make = |age_h: i64, step: f64| -> Vec<GeopoliticalEvent> {
            let mut v = Vec::new();
            for _ in 0..8 {
                let mut a = ev("us_iran", "military_escalation", 0.95, 0.9, &["united_states", "iran"], true);
                let mut b = ev("us_iran", "nuclear_posture", 0.9, 0.9, &["iran"], false);
                a.published_at = Utc::now() - Duration::hours(age_h);
                b.published_at = Utc::now() - Duration::hours(age_h);
                a.escalation_step = step; b.escalation_step = step;
                v.push(a); v.push(b);
            }
            v
        };

        // (1) Fresh war → the lead reads live, headline not held.
        let fresh = TheaterEngine::new().compute(&make(0, 0.2));
        assert!(!systemic_read_is_floor_held(&fresh.theaters),
            "a fresh-hot lead theater is a live headline, not floor-held");

        // (2) 4-day-silent war, no de-escalation → the lead is held by the floor, so is the headline.
        let aged = TheaterEngine::new().compute(&make(96, 0.2));
        assert!(systemic_read_is_floor_held(&aged.theaters),
            "a headline led by a floor-held war must flag as held");

        // (3) Same gap WITH de-escalation evidence → the floor releases, headline not held.
        let deesc = TheaterEngine::new().compute(&make(96, -0.7));
        assert!(!systemic_read_is_floor_held(&deesc.theaters),
            "a de-escalating lead releases the floor — headline not held");

        // (4) A quiet world never manufactures a held headline.
        let quiet = TheaterEngine::new().compute(&[]);
        assert!(!systemic_read_is_floor_held(&quiet.theaters),
            "a quiet world must never flag a held headline");
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
    fn top_driver_names_the_dominant_weighted_modality() {
        // Awareness "why" per theater: top_driver must name the modality with the
        // largest weighted heat contribution (score × domain_weight), and be empty for
        // a Stable theater. These lock the relationship, not a magnitude.

        // (a) A theater fed ONLY kinetic signals names that modality (others score 0).
        let mut te = TheaterEngine::new();
        let mut kin = Vec::new();
        for _ in 0..8 {
            kin.push(ev("us_iran", "military_escalation", 0.95, 0.9, &["united_states", "iran"], true));
        }
        let out = te.compute(&kin);
        let gulf = out.theaters.iter().find(|s| s.theater_id == "us_iran").unwrap();
        assert_ne!(gulf.rung, EscalationRung::Stable, "8 strong kinetic events should clear Stable");
        assert_eq!(gulf.top_driver, "military_escalation",
            "only-kinetic theater should be driven by military_escalation, got {:?}", gulf.top_driver);

        // (b) Equal-strength kinetic AND nuclear: nuclear's higher weight (3.0 vs 1.6)
        // makes it the dominant contributor even at equal score — locks that the driver
        // is the WEIGHTED term, not the raw score.
        let mut te2 = TheaterEngine::new();
        let mut mixed = Vec::new();
        for _ in 0..6 {
            mixed.push(ev("us_iran", "military_escalation", 0.9, 0.9, &["united_states", "iran"], true));
            mixed.push(ev("us_iran", "nuclear_posture",     0.9, 0.9, &["iran"], false));
        }
        let out2 = te2.compute(&mixed);
        let gulf2 = out2.theaters.iter().find(|s| s.theater_id == "us_iran").unwrap();
        assert_eq!(gulf2.top_driver, "nuclear_posture",
            "equal-score kinetic+nuclear should be driven by the heavier-weighted nuclear_posture, got {:?}",
            gulf2.top_driver);

        // (c) A quiet world names no driver.
        let mut te3 = TheaterEngine::new();
        let quiet = te3.compute(&[]);
        assert!(quiet.theaters.iter().all(|s| s.top_driver.is_empty()),
            "Stable theaters must not name a driver");
    }

    #[test]
    fn rising_driver_names_the_modality_that_moved_not_the_dominant_level() {
        // Awareness "what's heating up" per theater: rising_driver must name the modality
        // with the largest POSITIVE weighted change since the previous tick — which can
        // differ from top_driver (the dominant LEVEL). Lock the relationship, not magnitudes,
        // and drive two ticks on ONE engine so the cross-tick delta is exercised.
        let nuclear = |n: usize| -> Vec<GeopoliticalEvent> {
            let mut v = Vec::new();
            for _ in 0..n { v.push(ev("us_iran", "nuclear_posture", 0.9, 0.9, &["iran"], false)); }
            v
        };
        let military = |n: usize| -> Vec<GeopoliticalEvent> {
            let mut v = Vec::new();
            for _ in 0..n { v.push(ev("us_iran", "military_escalation", 0.95, 0.9, &["united_states","iran"], true)); }
            v
        };

        let mut te = TheaterEngine::new();

        // Tick 1: a theater hot ONLY on nuclear posture. It is rising from zero, so the
        // single positive mover is nuclear — rising_driver and top_driver agree here.
        let t1 = te.compute(&nuclear(8));
        let g1 = t1.theaters.iter().find(|s| s.theater_id == "us_iran").unwrap();
        assert_eq!(g1.trend, "rising", "a theater hot from zero must read rising");
        assert_eq!(g1.top_driver, "nuclear_posture");
        assert_eq!(g1.rising_driver, "nuclear_posture",
            "rising-from-zero on nuclear should name nuclear, got {:?}", g1.rising_driver);

        // Tick 2: hold nuclear identical, SPIKE military escalation. Heat rises, so the
        // delta-driver is military_escalation — even though nuclear (weight 3.0) is still
        // the dominant LEVEL (top_driver). This is the honesty point top_driver can't make.
        let mut t2ev = nuclear(8);
        t2ev.extend(military(8));
        let t2 = te.compute(&t2ev);
        let g2 = t2.theaters.iter().find(|s| s.theater_id == "us_iran").unwrap();
        assert_eq!(g2.trend, "rising", "adding a hot modality must raise heat → rising");
        assert_eq!(g2.top_driver, "nuclear_posture",
            "nuclear still dominates the LEVEL (weight 3.0), got {:?}", g2.top_driver);
        assert_eq!(g2.rising_driver, "military_escalation",
            "the modality that CLIMBED is military_escalation, got {:?}", g2.rising_driver);

        // Tick 3: identical to tick 2 → heat flat → not rising → no rising_driver named.
        let mut t3ev = nuclear(8);
        t3ev.extend(military(8));
        let t3 = te.compute(&t3ev);
        let g3 = t3.theaters.iter().find(|s| s.theater_id == "us_iran").unwrap();
        assert_ne!(g3.trend, "rising", "an unchanged theater must not read rising");
        assert!(g3.rising_driver.is_empty(),
            "a non-rising theater must name no rising-driver, got {:?}", g3.rising_driver);

        // A quiet world names nothing rising.
        let mut teq = TheaterEngine::new();
        let q = teq.compute(&[]);
        assert!(q.theaters.iter().all(|s| s.rising_driver.is_empty()),
            "Stable theaters must not name a rising-driver");
    }

    #[test]
    fn secondary_driver_names_the_second_elevated_force_dimension() {
        // Awareness "second dimension" per theater: secondary_driver must name the
        // SECOND-largest WEIGHTED contributor AMONG the modalities the model considers
        // elevated (score >= ELEVATION_THRESHOLD). It is the co-occurrence story
        // top_driver (a single dominant level) cannot tell, and it must stay empty when a
        // flashpoint has only one elevated dimension. Locks the relationship + the gate.

        // (a) Two strongly-elevated modalities: nuclear (weight 3.0) is the dominant
        // driver, military_escalation is the SECOND elevated dimension.
        let mut te = TheaterEngine::new();
        let mut mixed = Vec::new();
        for _ in 0..6 {
            mixed.push(ev("us_iran", "military_escalation", 0.9, 0.9, &["united_states", "iran"], true));
            mixed.push(ev("us_iran", "nuclear_posture",     0.9, 0.9, &["iran"], false));
        }
        let out = te.compute(&mixed);
        let gulf = out.theaters.iter().find(|s| s.theater_id == "us_iran").unwrap();
        assert_eq!(gulf.top_driver, "nuclear_posture",
            "dominant level is nuclear, got {:?}", gulf.top_driver);
        assert_eq!(gulf.secondary_driver, "military_escalation",
            "second elevated dimension should be military_escalation, got {:?}", gulf.secondary_driver);
        // The named secondary must itself be elevated (sanity on the fixture + gate).
        assert!(gulf.modality_scores["nuclear_posture"] >= ELEVATION_THRESHOLD);
        assert!(gulf.modality_scores["military_escalation"] >= ELEVATION_THRESHOLD);

        // (b) A single elevated dimension names NO secondary — even though the theater is
        // clearly hot (only-kinetic). This is the distinction from top_driver, which always
        // names the dominant term.
        let mut te2 = TheaterEngine::new();
        let mut kin = Vec::new();
        for _ in 0..8 {
            kin.push(ev("us_iran", "military_escalation", 0.95, 0.9, &["united_states", "iran"], true));
        }
        let out2 = te2.compute(&kin);
        let gulf2 = out2.theaters.iter().find(|s| s.theater_id == "us_iran").unwrap();
        assert_ne!(gulf2.rung, EscalationRung::Stable, "8 strong kinetic events should clear Stable");
        assert_eq!(gulf2.top_driver, "military_escalation");
        assert!(gulf2.secondary_driver.is_empty(),
            "a single-dimension flashpoint must name no secondary driver, got {:?}", gulf2.secondary_driver);

        // (c) The elevation GATE: a faint second modality whose score stays BELOW
        // ELEVATION_THRESHOLD is NOT named — even though it is the second-largest weighted
        // term overall. This is the honesty distinction from "2nd largest weighted period".
        let mut te3 = TheaterEngine::new();
        let mut faint = Vec::new();
        for _ in 0..8 { faint.push(ev("us_iran", "nuclear_posture", 0.9, 0.9, &["iran"], false)); }
        faint.push(ev("us_iran", "military_escalation", 0.12, 0.1, &["iran"], false));
        let out3 = te3.compute(&faint);
        let gulf3 = out3.theaters.iter().find(|s| s.theater_id == "us_iran").unwrap();
        let mil = gulf3.modality_scores.get("military_escalation").copied().unwrap_or(0.0);
        assert!(mil < ELEVATION_THRESHOLD,
            "fixture sanity: the faint kinetic blip should stay sub-elevated, got {:.3}", mil);
        assert!(gulf3.secondary_driver.is_empty(),
            "a sub-elevated second modality (score {:.3} < {}) must not be a secondary driver, got {:?}",
            mil, ELEVATION_THRESHOLD, gulf3.secondary_driver);

        // (d) A quiet world names no secondary driver.
        let mut te4 = TheaterEngine::new();
        let quiet = te4.compute(&[]);
        assert!(quiet.theaters.iter().all(|s| s.secondary_driver.is_empty()),
            "Stable theaters must not name a secondary driver");
    }

    #[test]
    fn intra_theater_co_occurrence_uses_the_shared_ramp_and_ignores_sub_threshold_modalities() {
        // HONESTY INVARIANT (flagged open since 2026-06-09): the intra-theater
        // co-occurrence boost is driven by the SAME `soft_elevation_weight` ramp the
        // systemic co-occurrence uses — so "elevated" means one thing model-wide — and a
        // modality scoring below the elevation ramp contributes EXACTLY ZERO co-occurrence
        // amplification. Before this lock the theater path duplicated the ramp with its own
        // ELEV_RAMP constant + inline smoothstep, free to silently drift from the systemic one.

        // The ramp's zero band: a clearly sub-threshold score adds 0 elevation weight.
        for s in [0.0, 0.10, 0.20] {
            assert_eq!(soft_elevation_weight(s), 0.0,
                "a sub-threshold score {s:.2} must add 0 elevation weight");
        }

        // Reconstruct the co-occurrence multiplier the engine actually applied:
        // heat = (weighted_sum / max_weighted_sum) * cooc  (uncapped) ⇒ cooc = heat·max/weighted.
        let cooc_of = |g: &TheaterState| -> f64 {
            let weighted: f64 = DOMAIN_WEIGHTS.iter()
                .map(|(m, _)| g.modality_scores.get(*m).copied().unwrap_or(0.0) * domain_weight(m))
                .sum();
            g.heat * max_weighted_sum() / weighted
        };
        // (1) One elevated modality (nuclear) + a FAINT sub-threshold second modality.
        // The faint blip sits in the ramp's zero band, so it must add NO co-occurrence:
        // only nuclear is elevated, so the boost stays ~neutral (co_occurrence_boost(1.0)=1.0).
        let mut te = TheaterEngine::new();
        let mut faint = Vec::new();
        for _ in 0..8 { faint.push(ev("us_iran", "nuclear_posture", 0.9, 0.9, &["iran"], false)); }
        faint.push(ev("us_iran", "military_escalation", 0.10, 0.1, &["iran"], false));
        let of = te.compute(&faint);
        let gf = of.theaters.iter().find(|s| s.theater_id == "us_iran").unwrap();
        let mil = gf.modality_scores["military_escalation"];
        // CRISP invariant on the shared ramp: the faint blip contributes EXACTLY 0 weight.
        assert_eq!(soft_elevation_weight(mil), 0.0,
            "the faint kinetic blip (score {mil:.3}) must sit in the ramp's zero band → 0 weight");
        assert!(gf.heat < 1.0, "fixture: heat must be uncapped to read cooc, got {}", gf.heat);
        let cooc_faint = cooc_of(gf);
        assert!(cooc_faint < 1.01,
            "a sub-threshold modality must leave the co-occurrence boost essentially neutral, got {cooc_faint}");

        // (2) Promote that second modality ABOVE the ramp → it is now elevated and the
        // co-occurrence boost jumps to the shared two-elevated anchor. The ONLY change from
        // (1) is the second modality crossing the shared elevation ramp — proving that ramp
        // is exactly the boundary and the engine reads the shared `co_occurrence_boost` table.
        let mut te2 = TheaterEngine::new();
        let mut both = Vec::new();
        for _ in 0..8 {
            both.push(ev("us_iran", "nuclear_posture",     0.9, 0.9, &["iran"], false));
            both.push(ev("us_iran", "military_escalation", 0.9, 0.9, &["united_states", "iran"], true));
        }
        let ob = te2.compute(&both);
        let gb = ob.theaters.iter().find(|s| s.theater_id == "us_iran").unwrap();
        assert!(gb.modality_scores["military_escalation"] >= ELEVATION_THRESHOLD,
            "fixture: the promoted modality must clear elevation, got {:.3}",
            gb.modality_scores["military_escalation"]);
        assert!(gb.heat < 1.0, "fixture: heat must be uncapped, got {}", gb.heat);
        let cooc_both = cooc_of(gb);
        // Crossing the ramp DOES amplify — far above the neutral faint case…
        assert!(cooc_both > cooc_faint + 0.1,
            "two elevated modalities must amplify co-occurrence (got {cooc_both}) well above the \
             sub-threshold case ({cooc_faint})");
        // …and the engine's boost matches the SHARED co_occurrence_boost two-elevated anchor (1.25),
        // i.e. the intra-theater path reads the same elevation/boost machinery as the systemic path.
        assert!((cooc_both - co_occurrence_boost(2.0)).abs() < 1e-2,
            "intra-theater cooc with two fully-elevated modalities must match the shared boost \
             anchor co_occurrence_boost(2.0)={}, got {cooc_both}", co_occurrence_boost(2.0));
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

        // HONESTY: only ONE theater is hot in the `single` world; the other four are
        // eventless → Stable → they must leak exactly 0 concurrency. A fully-hot
        // theater saturates its smoothstep at 1.0, so total concurrency is exactly 1.0.
        // If a quiet theater leaked, this would read > 1.0.
        assert!((o1.couplers.concurrency - 1.0).abs() < 1e-3,
            "one hot theater (+ four Stable) must yield concurrency 1.0, got {}",
            o1.couplers.concurrency);
    }

    #[test]
    // The constant assertions are the point: they pin relationships between tuning
    // constants and fail with a readable message at test time (a `const` block would
    // turn them into bare compile errors and drop the explanations).
    #[allow(clippy::assertions_on_constants)]
    fn quiet_theater_never_leaks_into_couplers() {
        // HONESTY INVARIANT: a Stable theater (heat at/below STABLE_HEAT_CEILING) must
        // contribute EXACTLY ZERO to every systemic amplifier — concurrency,
        // great-power entanglement and alliance activation. This pins the RELATIONSHIP
        // between the coupler gates and the rung structure, not any fitted magnitude, so
        // it survives legitimate recalibration but trips the moment a ramp/threshold tweak
        // would let a quiet world silently inflate the headline.

        // (1) The concurrency ramp must not have begun by the Stable ceiling: its lower
        //     edge (HOT_HEAT − HOT_RAMP) sits strictly above STABLE_HEAT_CEILING.
        assert!(HOT_HEAT - HOT_RAMP > STABLE_HEAT_CEILING,
            "concurrency ramp lower edge {} must stay above the Stable ceiling {} so a \
             stable theater contributes 0 concurrency",
            HOT_HEAT - HOT_RAMP, STABLE_HEAT_CEILING);

        // (2) …and the smoothstep actually returns 0 across the ENTIRE Stable band,
        //     up to and including the ceiling itself.
        for i in 0..=60 {
            let h = i as f64 / 1000.0; // 0.000 .. 0.060 (the Stable band)
            assert!(h <= STABLE_HEAT_CEILING);
            let c = smoothstep(h, HOT_HEAT - HOT_RAMP, HOT_HEAT + HOT_RAMP);
            assert_eq!(c, 0.0, "stable heat {h} leaked {c} concurrency into the amplifier");
        }

        // (3) Great-power entanglement and alliance activation both gate on
        //     `heat >= HOT_HEAT`, which is strictly above the Stable ceiling — so a
        //     stable theater can never enter either set.
        assert!(HOT_HEAT > STABLE_HEAT_CEILING,
            "entanglement/alliance gate {} must stay above the Stable ceiling {}",
            HOT_HEAT, STABLE_HEAT_CEILING);
    }

    #[test]
    fn rung_for_and_within_band_share_one_contiguous_partition() {
        // PROVENANCE / HONESTY INVARIANT: `rung_for` (which rung a heat lands in) and
        // `within_band` (its fractional position inside that rung) MUST read the same
        // four heat boundaries. The systemic index is `(rung.level() + within)/6`, so the
        // two functions agreeing is what keeps the index continuous across a rung seam —
        // if they drifted (the bug this run closed: four bare-literal copies of the
        // boundaries), `within_band` would compute the fraction against a band that no
        // longer contains the heat, clamp it to 0/1, and the index would jump.

        // Combined position the index uses, with no escalation overrides so rung_for is
        // driven purely by heat.
        let pos = |h: f64| -> f64 {
            let r = rung_for(h, false, false, false);
            r.level() as f64 + within_band(h, r)
        };

        // (1) Continuity + monotonicity in heat across every conventional rung seam.
        // Contiguous bands make `pos` continuous: just below a boundary the lower rung is
        // at the TOP of its band (within→1), AT the boundary the next rung starts at the
        // BOTTOM (within=0), so level jumps +1 while within drops ~1 — net continuous.
        // A drift between the two functions would make `pos` jump or run backwards here.
        let mut prev = pos(0.0);
        let mut h = 0.0;
        while h <= GREAT_POWER_WAR_HEAT {
            let cur = pos(h);
            assert!(cur >= prev - 1e-9, "index position ran backwards at heat {h}: {prev} -> {cur}");
            assert!((cur - prev).abs() < 0.05,
                "index position jumped at heat {h}: {prev} -> {cur} — rung_for/within_band boundaries drifted");
            prev = cur;
            h += 0.0005;
        }

        // (2) Each boundary is exactly a shared constant separating adjacent rungs: AT the
        // boundary the heat sits at the bottom of the upper band (within == 0), and one ulp
        // below it sits near the top of the lower band (within ≈ 1).
        for b in [STABLE_HEAT_CEILING, HOT_HEAT, LIMITED_WAR_HEAT, GREAT_POWER_WAR_HEAT] {
            let r_at = rung_for(b, false, false, false);
            let r_below = rung_for(b - 1e-6, false, false, false);
            assert_eq!(r_at.level(), r_below.level() + 1,
                "boundary {b} must separate two adjacent rungs");
            assert_eq!(within_band(b, r_at), 0.0,
                "heat exactly at boundary {b} must sit at the BOTTOM of its band");
            assert!(within_band(b - 1e-6, r_below) > 0.99,
                "heat just below boundary {b} must sit near the TOP of the lower band");
        }
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
    fn driver_names_the_brink_theater_not_the_hottest_one() {
        // Awareness "WHERE" for the apex case: the systemic `driver` string is the
        // operator's headline "where". The brink theater carries the single largest
        // lever on l_sys (BRINK_AMPLIFIER, +70%) yet — per the l_sys test above — need
        // NOT be the hottest by raw heat. So the "where" must name the BRINK theater,
        // not a louder conventional one that wins the raw-heat sort. The hottest stays
        // visible in the dashboard sub-line + ladder strip.

        // Hottest-by-heat, conventional, no nuclear → never a brink itself.
        let conventional_hottest = || {
            let mut v = Vec::new();
            for _ in 0..6 {
                v.push(ev("us_iran", "military_escalation", 1.0, 0.9, &["united_states", "iran"], true));
                v.push(ev("us_iran", "economic_warfare",    0.9, 0.9, &["united_states", "iran"], true));
                v.push(ev("us_iran", "cyber_info_ops",      0.85, 0.9, &["united_states", "iran"], true));
                v.push(ev("us_iran", "diplomatic_breakdown",0.85, 0.9, &["united_states", "iran"], true));
            }
            v
        };
        // Cooler theater whose heat is purely extreme nuclear posture → a 2-GP brink.
        let mut world = conventional_hottest();
        for _ in 0..6 {
            let mut e = ev("nato_russia", "nuclear_posture", 1.0, 1.0, &["united_states", "russia"], true);
            e.escalation_language_score = 0.8; // clear the 0.78 brink threshold
            world.push(e);
        }

        let mut te = TheaterEngine::new();
        let out = te.compute(&world);

        // Precondition: the conventional theater is hottest; the brink sits in the cooler one.
        let hottest = out.theaters.iter()
            .max_by(|a, b| a.heat.partial_cmp(&b.heat).unwrap()).unwrap();
        assert_eq!(hottest.theater_id, "us_iran",
            "precondition: conventional theater must be hottest, got {}", hottest.theater_id);
        let brink_t = out.theaters.iter().find(|t| theater_is_nuclear_brink(t)).unwrap();
        assert_eq!(brink_t.theater_id, "nato_russia", "precondition: brink is the cooler theater");

        // The driver names the BRINK theater + the apex configuration, NOT the hottest.
        assert!(out.driver.contains("NATO–Russia"),
            "driver must name the brink theater, got {:?}", out.driver);
        assert!(out.driver.contains("nuclear brink"),
            "driver must name the apex configuration, got {:?}", out.driver);
        assert!(!out.driver.contains("US/Israel–Iran"),
            "the hottest theater must NOT be the headline 'where' when a brink leads, got {:?}", out.driver);

        // Contrast: with the brink theater downgraded to ONE great power (not a brink),
        // the driver falls back to naming the hottest theater with its rung label.
        let mut world2 = conventional_hottest();
        for _ in 0..6 {
            let mut e = ev("nato_russia", "nuclear_posture", 1.0, 1.0, &["russia"], true);
            e.escalation_language_score = 0.8;
            world2.push(e);
        }
        let mut te2 = TheaterEngine::new();
        let out2 = te2.compute(&world2);
        assert!(!out2.theaters.iter().any(theater_is_nuclear_brink),
            "precondition: no brink when only one great power is present");
        assert!(out2.driver.contains("US/Israel–Iran") && !out2.driver.contains("nuclear brink"),
            "with no brink, the driver names the hottest theater, got {:?}", out2.driver);
    }

    #[test]
    fn coupling_driver_names_the_dominant_systemic_amplifier() {
        // Awareness "why" at the SYSTEMIC level: coupling_driver names the coupling
        // channel turning a regional crisis into a world-war risk — read off the SAME
        // lifts that build l_sys, never a new lever. Unit checks pin the decomposition +
        // the tie/floor rules; four live worlds isolate each channel, and one proves the
        // honest "regional, not yet systemically coupled" empty read.

        // (a) Pure decomposition + tie/floor. The function returns (label, lift); the lift
        //     magnitude lets the Bayesian engine compare the structural guardrail coupler.
        assert_eq!(dominant_coupling_amplifier(0.0, 0.0, 0.0, 0.0), ("", 0.0),
            "no lift anywhere → no systemic coupling named");
        assert_eq!(dominant_coupling_amplifier(0.70, 0.30, 0.18, 0.0).0, "single-theater nuclear brink");
        assert_eq!(dominant_coupling_amplifier(0.70, 0.30, 0.18, 0.0).1, 0.70,
            "the winning channel's lift magnitude is returned for the guardrail comparison");
        assert_eq!(dominant_coupling_amplifier(0.0, 0.30, 0.18, 0.0).0, "great-power entanglement");
        assert_eq!(dominant_coupling_amplifier(0.0, 0.0, 0.18, 0.0).0, "multi-theater concurrency");
        assert_eq!(dominant_coupling_amplifier(0.0, 0.0, 0.0, 0.30).0, "alliance activation");
        assert_eq!(dominant_coupling_amplifier(0.3, 0.3, 0.3, 0.3).0, "single-theater nuclear brink",
            "a tie must resolve to the most dangerous channel (apex order)");

        // (b) Brink world: a US–Russia nuclear standoff → brink lift 0.70 outranks the
        //     0.30 great-power-entanglement lift it also carries.
        let mut te = TheaterEngine::new();
        let mut brink = Vec::new();
        for _ in 0..6 {
            let mut e = ev("nato_russia", "nuclear_posture", 1.0, 1.0, &["united_states", "russia"], true);
            e.escalation_language_score = 0.8; // push nuclear posture past the brink threshold
            brink.push(e);
        }
        let ob = te.compute(&brink);
        assert!(ob.theaters.iter().any(theater_is_nuclear_brink), "precondition: a brink theater exists");
        assert!(ob.couplers.gp_entanglement > 0.0, "precondition: great powers also entangled");
        assert_eq!(ob.couplers.coupling_driver, "single-theater nuclear brink",
            "a nuclear brink must be the named dominant amplifier, got {:?}", ob.couplers.coupling_driver);

        // A multi-modality CONVENTIONAL hot theater (no nuclear → never a brink itself).
        // Several modalities are needed to clear HOT_HEAT (heat blends across all five).
        let conventional = |theater: &'static str, actors: &'static [&'static str]| {
            let mut v = Vec::new();
            for _ in 0..6 {
                v.push(ev(theater, "military_escalation",  1.0,  0.9, actors, false));
                v.push(ev(theater, "economic_warfare",     0.9,  0.9, actors, false));
                v.push(ev(theater, "cyber_info_ops",       0.85, 0.9, actors, false));
                v.push(ev(theater, "diplomatic_breakdown", 0.85, 0.9, actors, false));
            }
            v
        };

        // (c) Great-power-entanglement world: US+Russia hot CONVENTIONALLY in one theater
        //     (no nuclear → not a brink; one hot theater → no breadth) → gp lift 0.30 leads.
        let mut te = TheaterEngine::new();
        let og = te.compute(&conventional("nato_russia", &["united_states", "russia"]));
        let nr = og.theaters.iter().find(|s| s.theater_id == "nato_russia").unwrap();
        assert!(!theater_is_nuclear_brink(nr), "precondition: conventional, NOT a brink");
        assert!(og.couplers.gp_entanglement > 0.0, "precondition: great powers entangled");
        assert!(og.couplers.concurrency < 1.5, "precondition: one hot theater → no breadth, got {}", og.couplers.concurrency);
        assert_eq!(og.couplers.coupling_driver, "great-power entanglement",
            "got {:?}", og.couplers.coupling_driver);

        // (d) Breadth world: three theaters hot with NON-great-power actors, no nuclear →
        //     brink/gp/alliance all 0, only concurrency lifts.
        let mut te = TheaterEngine::new();
        let mut br = conventional("us_iran", &["iran"]);
        br.extend(conventional("india_pakistan", &["india", "pakistan"]));
        br.extend(conventional("korea", &["north_korea", "south_korea"]));
        let obr = te.compute(&br);
        assert_eq!(obr.couplers.gp_entanglement, 0.0, "precondition: no great powers entangled");
        assert!(obr.couplers.concurrency > 2.0, "precondition: ≥3 theaters hot, got {}", obr.couplers.concurrency);
        assert_eq!(obr.couplers.coupling_driver, "multi-theater concurrency",
            "got {:?}", obr.couplers.coupling_driver);

        // (e) Regional, not yet systemic: a SINGLE non-GP theater hot → no coupling channel
        //     lifts (one hot theater = no breadth, no GP, no brink). Honest empty read.
        let mut te = TheaterEngine::new();
        let oreg = te.compute(&conventional("us_iran", &["iran"]));
        assert!(oreg.systemic_index > 0.0, "precondition: the theater is genuinely hot");
        assert_eq!(oreg.couplers.coupling_driver, "",
            "a single uncoupled regional crisis must name no systemic amplifier, got {:?}",
            oreg.couplers.coupling_driver);

        // (f) Quiet world → nothing.
        let mut te = TheaterEngine::new();
        assert_eq!(te.compute(&[]).couplers.coupling_driver, "");
    }

    #[test]
    fn breadth_never_swamps_the_nuclear_brink() {
        // HONESTY INVARIANT (the design intent of the 2026-06-03 saturating-breadth fix,
        // previously only asserted in prose): a no-brink multi-theater world must NEVER
        // out-amplify the single-theater nuclear-brink apex at equal intensity. A previous
        // LINEAR breadth term (+0.12 per hot theater) let a four-theater no-brink world
        // (live 2026) drive l_sys ABOVE the Cuba head-to-head and peg P(WWIII) flat at the
        // 0.90 ceiling — breadth swamping the brink, the opposite of intent. The fix is
        // structural; it is locked here two complementary ways.

        // (1) Structural guarantee that survives any future recalibration: the single-theater
        // apex amplifier strictly exceeds the MOST breadth can ever add. With equal max_heat
        // and equal coupling this means the brink head-to-head always wins on amplification.
        #[allow(clippy::assertions_on_constants)]
        {
            assert!(BRINK_AMPLIFIER > BREADTH_ASYMPTOTE,
                "the single-theater nuclear-brink amplifier (1+{BRINK_AMPLIFIER}) must strictly \
                 exceed the maximum breadth amplification (1+{BREADTH_ASYMPTOTE}), or breadth \
                 could swamp the brink");
        }

        // (2) Behavioural bound through the live engine: drive it with 1..=5 IDENTICAL hot
        // theaters that are conventional (no great powers → coupling=1, no nuclear → brink=0),
        // so per-theater max_heat is held constant and the l_sys ratio vs the single-theater
        // world IS the breadth amplifier. It must (a) be 1.0 at one theater (no breadth bonus),
        // (b) rise monotonically, and (c) stay strictly below 1+BREADTH_ASYMPTOTE no matter how
        // many theaters are hot — hence strictly below the 1+BRINK_AMPLIFIER apex.
        let conventional = |theater: &str| {
            let mut v = Vec::new();
            for _ in 0..6 {
                // Non-great-power actors → no entanglement; no nuclear modality → no brink.
                v.push(ev(theater, "military_escalation", 0.95, 0.9, &["iran", "israel"], false));
                v.push(ev(theater, "economic_warfare",    0.85, 0.7, &["iran", "israel"], false));
            }
            v
        };
        let theaters = ["us_iran", "nato_russia", "us_china_taiwan", "india_pakistan", "korea"];

        let mut te_base = TheaterEngine::new();
        let base = te_base.compute(&conventional(theaters[0]));
        assert_eq!(base.couplers.gp_entanglement, 0.0, "control must have no entanglement");
        assert!(base.l_sys > 0.0, "control world must be hot, got l_sys={}", base.l_sys);

        let mut prev_ratio = 1.0;
        for n in 1..=theaters.len() {
            let mut world = Vec::new();
            for t in &theaters[..n] {
                world.extend(conventional(t));
            }
            let mut te = TheaterEngine::new();
            let out = te.compute(&world);
            assert_eq!(out.couplers.gp_entanglement, 0.0,
                "{n}-theater world must stay free of great-power entanglement");
            let ratio = out.l_sys / base.l_sys;
            // (c) bounded strictly below the asymptote, always.
            assert!(ratio < 1.0 + BREADTH_ASYMPTOTE,
                "breadth amplification at {n} theaters ({ratio}) must stay below \
                 1+BREADTH_ASYMPTOTE ({})", 1.0 + BREADTH_ASYMPTOTE);
            if n == 1 {
                // (a) a single hot theater earns no breadth bonus at all.
                assert!((ratio - 1.0).abs() < 1e-6,
                    "one hot theater must have no breadth bonus, got ratio {ratio}");
            } else {
                // (b) adding a hot theater never lowers the breadth amplifier.
                assert!(ratio > prev_ratio - 1e-9,
                    "adding a hot theater must not lower breadth amplification: {prev_ratio} -> {ratio}");
            }
            prev_ratio = ratio;
        }
        // Even all five hot theaters amplify by strictly less than the single-theater brink.
        assert!(prev_ratio < 1.0 + BRINK_AMPLIFIER,
            "five hot theaters ({prev_ratio}) must amplify less than the brink (1+{BRINK_AMPLIFIER})");
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
    fn unverified_nuclear_claim_does_not_force_systemic_rung() {
        // An UNVERIFIED allegation of a nuclear strike is not a confirmed use. The apex
        // Systemic rung (which pegs the headline at 95 and floods P(WWIII)) must trip
        // only on a clean confirmation, never on hedged "allegedly / reportedly /
        // unconfirmed" framing — exactly the genre that surrounds an unfolding claim
        // before verification. The title carries the "nuclear strike" use-phrase AND
        // nuclear_indicator is set, so the plain match would fire; the uncertainty
        // tokens must catch the allegation framing and decline.
        let mut te = TheaterEngine::new();
        let mut e = ev("nato_russia", "nuclear_posture", 1.0, 1.0, &["russia", "united_states"], true);
        e.title = "Russia allegedly carried out nuclear strike, reports unconfirmed".into();
        e.nuclear_indicator = true;
        let out = te.compute(&[e]);
        let t = out.theaters.iter().find(|s| s.theater_id == "nato_russia").unwrap();
        assert_ne!(t.rung, EscalationRung::Systemic,
            "an unverified nuclear claim must not force the systemic (nuclear-use) rung");
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

    #[test]
    fn real_world_non_use_headlines_do_not_force_systemic_rung() {
        // Regression: these are ACTUAL production headlines (logs/events_*.jsonl) that
        // carried the "nuclear strike" use-phrase, were tagged nuclear_indicator, and
        // FALSELY tripped the apex Systemic ("nuclear war occurred") rung — pegging the
        // systemic index at 95 — because the non-use framing slipped through the token
        // guard (drill/sim inflections, advocacy, explainer/authorization, interrogative
        // lead). None reports a detonation; each must decline the systemic rung.
        let cases = [
            "North Korea trumpets training for ‘tactical’ nuclear strikes",
            "North Korea Launches 2 Ballistic Missile, Simulates ‘Scorched Earth’ Nuclear Strikes",
            "This is the process the US uses to authorize a nuclear strike",
            "Head of dissolved party urges legislature to petition Putin for nuclear strike on Ukraine",
            "Can the president launch a nuclear strike on his own?",
            "NATO, Russia direct confrontation may escalate into nuclear strikes’ exchange — Lavrov",
        ];
        for title in cases {
            let mut te = TheaterEngine::new();
            let mut e = ev("nato_russia", "nuclear_posture", 1.0, 1.0, &["russia", "united_states"], true);
            e.title = title.to_string();
            e.nuclear_indicator = true;
            let out = te.compute(&[e]);
            let t = out.theaters.iter().find(|s| s.theater_id == "nato_russia").unwrap();
            assert_ne!(t.rung, EscalationRung::Systemic,
                "non-use headline must not force the systemic rung: {title:?}");
        }
    }

    // ── Systemic cross-check invariants (roadmap 1.3) ──────────────────────────
    // These lock the model's core honesty properties — bounded outputs, escalation
    // monotonicity ("more escalation never lowers the index"), de-escalation actually
    // de-escalating, and the apex pegging at the forecast ceiling rather than 100.
    // None of these were guarded before: a future calibration tweak could silently
    // break monotonicity or let the headline exceed 95, producing a dishonest number,
    // with nothing to catch it. They assert relationships the model must always satisfy,
    // not fitted magnitudes, so they pin behaviour without freezing the calibration.

    /// Multi-modality, clearly-hot theater used as a building block for the invariants.
    fn strong_theater(theater: &str, actors: &[&str], gp: bool) -> Vec<GeopoliticalEvent> {
        let mut v = Vec::new();
        for _ in 0..6 {
            v.push(ev(theater, "military_escalation", 0.95, 0.9, actors, gp));
            v.push(ev(theater, "nuclear_posture",     0.90, 0.9, actors, gp));
            v.push(ev(theater, "economic_warfare",    0.85, 0.7, actors, gp));
        }
        v
    }

    #[test]
    fn systemic_outputs_stay_bounded_over_many_worlds() {
        // Honesty invariant: whatever the window looks like, the public index stays in
        // [0, FORECAST_INDEX_CEILING], l_sys is non-negative, and every per-theater /
        // coupler field stays in its declared range. A change that lets the headline
        // exceed the 95 forecast ceiling — or go negative — is a dishonest number; this
        // fuzz catches it across 400 deterministically-generated worlds.
        let theaters = ["nato_russia", "us_iran", "us_china_taiwan", "india_pakistan", "korea"];
        let domains  = ["military_escalation", "nuclear_posture", "economic_warfare",
                        "cyber_info_ops", "diplomatic_breakdown", "great_power_conflict"];
        let actors   = ["united_states", "russia", "china", "nato", "iran", "india", "pakistan"];
        // Deterministic LCG so the fuzz is reproducible (no external rng dependency).
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = |m: u64| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (state >> 33) % m
        };
        for _ in 0..400 {
            let mut events = Vec::new();
            let n = next(40) as usize;
            for _ in 0..n {
                let t   = theaters[next(theaters.len() as u64) as usize];
                let d   = domains[next(domains.len() as u64) as usize];
                let sig = next(101) as f64 / 100.0;
                let sev = next(101) as f64 / 100.0;
                let a1  = actors[next(actors.len() as u64) as usize];
                let a2  = actors[next(actors.len() as u64) as usize];
                let gp  = next(2) == 1;
                let mut e = ev(t, d, sig, sev, &[a1, a2], gp);
                e.escalation_language_score = next(101) as f64 / 100.0;
                events.push(e);
            }
            let mut te = TheaterEngine::new();
            let out = te.compute(&events);
            assert!((0.0..=FORECAST_INDEX_CEILING).contains(&out.systemic_index),
                "systemic_index out of bounds: {}", out.systemic_index);
            assert!(out.l_sys >= 0.0, "l_sys negative: {}", out.l_sys);
            assert!((0.0..=1.0).contains(&out.couplers.gp_entanglement));
            assert!((0.0..=1.0).contains(&out.couplers.alliance_activation));
            assert!((0.0..=5.0).contains(&out.couplers.concurrency));
            assert!(out.couplers.coupling_multiplier >= 1.0);
            for st in &out.theaters {
                assert!((0.0..=1.0).contains(&st.heat), "heat out of range: {}", st.heat);
                assert!((-1.0..=1.0).contains(&st.delta), "delta out of range: {}", st.delta);
                assert!(st.rung.level() <= EscalationRung::Systemic.level());
            }
        }
    }

    #[test]
    fn adding_a_hot_theater_never_lowers_systemic_outputs() {
        // Monotonicity at the systemic level: adding a second hot theater (all else
        // equal) must never LOWER the headline index, and must RAISE the systemic
        // likelihood (more concurrent fronts = more systemic danger). This is the
        // "more escalation never lowers the index" invariant the wide bands could hide.
        let one = strong_theater("us_iran", &["united_states", "iran"], true);
        let mut two = one.clone();
        two.extend(strong_theater("nato_russia", &["russia", "nato"], true));
        let o1 = TheaterEngine::new().compute(&one);
        let o2 = TheaterEngine::new().compute(&two);
        assert!(o2.systemic_index >= o1.systemic_index,
            "a second hot theater must not lower the index: {} -> {}",
            o1.systemic_index, o2.systemic_index);
        assert!(o2.l_sys > o1.l_sys,
            "a second hot theater must raise systemic likelihood: {} -> {}", o1.l_sys, o2.l_sys);
        assert!(o2.couplers.concurrency > o1.couplers.concurrency);
    }

    #[test]
    fn adding_a_modality_never_cools_a_theater_or_the_index() {
        // Intra-theater monotonicity: a strict SUPERSET of escalation modalities in one
        // theater must be at least as hot (here, strictly hotter) and never lower the
        // index. Scoring is per-domain (bayesian::score_all) with a fresh per-call
        // scorer, so adding distinct hot modalities only adds positive weighted terms
        // and raises co-occurrence — it can never cool an existing modality.
        let mut lo = Vec::new();
        for _ in 0..6 {
            lo.push(ev("us_iran", "military_escalation", 0.9, 0.8, &["united_states", "iran"], true));
        }
        let mut hi = lo.clone();
        for _ in 0..6 {
            hi.push(ev("us_iran", "nuclear_posture",  0.9, 0.8, &["united_states", "iran"], true));
            hi.push(ev("us_iran", "economic_warfare", 0.85, 0.7, &["united_states", "iran"], true));
        }
        let o1 = TheaterEngine::new().compute(&lo);
        let o2 = TheaterEngine::new().compute(&hi);
        let g1 = o1.theaters.iter().find(|s| s.theater_id == "us_iran").unwrap();
        let g2 = o2.theaters.iter().find(|s| s.theater_id == "us_iran").unwrap();
        assert!(g2.heat > g1.heat,
            "adding hot modalities must not cool a theater: {} -> {}", g1.heat, g2.heat);
        assert!(o2.systemic_index >= o1.systemic_index,
            "adding hot modalities must not lower the index: {} -> {}",
            o1.systemic_index, o2.systemic_index);
    }

    #[test]
    fn de_escalation_lowers_the_systemic_index() {
        // De-escalation must actually de-escalate: a theater that goes from clearly hot
        // to a quiet window must drop the headline and the systemic likelihood, not stay
        // pinned. (Per-tick state lives in the same engine, so this also exercises the
        // cool-off path.)
        let mut te = TheaterEngine::new();
        let o_hot = te.compute(&strong_theater("us_iran", &["united_states", "iran"], true));
        assert!(o_hot.systemic_index > 50.0,
            "precondition: hot world index should be high, got {}", o_hot.systemic_index);
        let o_calm = te.compute(&[]);
        assert!(o_calm.systemic_index < o_hot.systemic_index,
            "de-escalation must lower the index: {} -> {}", o_hot.systemic_index, o_calm.systemic_index);
        assert!(o_calm.systemic_index < 1.0, "a quiet world must read near zero, got {}", o_calm.systemic_index);
        assert!(o_calm.l_sys < o_hot.l_sys, "de-escalation must lower l_sys");
    }

    #[test]
    fn systemic_rung_pegs_index_at_forecast_ceiling_not_100() {
        // The apex (nuclear-use) Systemic rung sits at the top of the 0..100 ladder
        // (level 5, full within-band → raw 100), but a model-INFERRED forecast must read
        // "very high" (95), never "certain" (100). This locks the FORECAST_INDEX_CEILING
        // clamp to the actual apex output, so a future change to the index formula can't
        // silently let the headline print 100 on a news-inferred detonation.
        let mut te = TheaterEngine::new();
        let mut e = ev("us_iran", "nuclear_posture", 1.0, 1.0, &["united_states", "russia"], true);
        e.title = "Nuclear detonation confirmed over military target".into();
        e.nuclear_indicator = true;
        let out = te.compute(&[e]);
        let g = out.theaters.iter().find(|s| s.theater_id == "us_iran").unwrap();
        assert_eq!(g.rung, EscalationRung::Systemic);
        assert_eq!(out.systemic_index, FORECAST_INDEX_CEILING,
            "apex Systemic rung must peg the index at the forecast ceiling (95), got {}",
            out.systemic_index);
        assert!(out.systemic_index < 100.0, "a forecast must never print certainty (100)");
    }
}
