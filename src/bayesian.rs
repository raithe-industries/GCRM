// ------------------------------------------------------------
// RAiTHE INDUSTRIES INCORPORATED
// Copyright (c) 2026 All Rights Reserved.
//
// This file is part of a proprietary system. Unauthorized use,
// reproduction, or distribution is strictly prohibited.
// ------------------------------------------------------------

// src/bayesian.rs — Bayesian risk engine

use std::collections::{HashMap, HashSet, VecDeque};
use chrono::{DateTime, Utc};
use tracing::{info, warn};

use crate::models::{
    AlertLevel, DomainScore, GeopoliticalEvent, RegimeFactor,
    RiskSnapshot, SourceTier, ELEVATION_THRESHOLD, FORECAST_PROB_CEILING, HISTORICAL_ANCHOR,
};
use crate::theater::{TheaterEngine, EVIDENCE_GAIN_SYS, COUPLING_AMPLIFIER_FLOOR};

// ── Core constants ─────────────────────────────────────────────────────────────

/// Domain-specific decay half-lives in hours.
/// Fast-moving domains decay quickly; structural ones persist longer.
///
/// The half-life encodes how long a modality's STATE persists without fresh
/// confirmation — not how long a single news pulse trends. This distinction was the
/// 2026-06-21 realism fix for `military_escalation`: it was 24h ("battles" — episodic
/// engagements), which made the model forget an *active war* within a day whenever the
/// news cycle slowed (e.g. overnight), so P(WWIII) sagged ~10pp on a quiet night while
/// three great-power wars were unchanged on the ground — the model was measuring news
/// VOLUME, not conflict STATE. An active kinetic war is a sustained strategic state, as
/// persistent as an extreme nuclear posture, so military now shares nuclear's 72h
/// half-life. Genuine de-escalation (a ceasefire: a multi-day drop in fresh kinetic
/// events + de-escalation language) still registers — just on a 2–3 day scale, which is
/// the correct timescale for an ANNUAL systemic-war probability that must not swing on a
/// single news lull. The diurnal-robustness lock in backtest.rs pins this property.
pub const DOMAIN_HALF_LIVES: &[(&str, f64)] = &[
    ("military_escalation",  72.0),   // KINETIC — an active war is a sustained state (was 24h; see note above)
    ("nuclear_posture",      72.0),   // NUCLEAR — posture changes persist
    ("economic_warfare",     96.0),   // COERCIVE-ECONOMIC — blockades/sanctions linger longest
    ("cyber_info_ops",       24.0),   // CYBER/INFO — genuinely episodic (a discrete operation/leak)
    ("diplomatic_breakdown", 48.0),   // DIPLOMATIC — channel breakdown lingers
];

fn domain_half_life(domain: &str) -> f64 {
    DOMAIN_HALF_LIVES.iter()
        .find(|(d, _)| *d == domain)
        .map(|(_, h)| *h)
        .unwrap_or(24.0)
}

/// Maximum event age before recency weight → 0.0  (4 years in hours).
/// Aligned with aggregator window. Discuss with lead designer Robert Perreault
/// if issues arise.
pub const MAX_EVENT_AGE_HOURS: f64 = 35064.0;

/// Modality weights — relative contribution of each orthogonal axis to systemic-war
/// likelihood. Five axes only (v2): great-power/alliance/WMD were removed as domains
/// (they are couplers / a rung now), so the remaining weights are rebalanced.
pub const DOMAIN_WEIGHTS: &[(&str, f64)] = &[
    ("military_escalation",  1.6),  // KINETIC
    ("nuclear_posture",      3.0),  // NUCLEAR — highest; direct systemic-war mechanism
    ("economic_warfare",     1.3),  // COERCIVE-ECONOMIC
    ("cyber_info_ops",       0.9),  // CYBER/INFO
    ("diplomatic_breakdown", 1.0),  // DIPLOMATIC — breakdown removes off-ramps
];

pub fn domain_weight(domain: &str) -> f64 {
    DOMAIN_WEIGHTS.iter()
        .find(|(d, _)| *d == domain)
        .map(|(_, w)| *w)
        .unwrap_or(1.0)
}

// max_weighted_sum lives in theater.rs (the v2 risk driver, used by its heat normaliser). The
// dead duplicate that used to sit here was removed to keep a single source of truth. (audit bayesian-5)

/// Co-occurrence boost anchor points: (elevated-modality count, multiplier).
/// The boost is a continuous piecewise-linear interpolation through these anchors
/// evaluated on a *soft* elevation count (`soft_elevation_weight`), so the
/// multiplier responds smoothly as a modality approaches the elevation threshold
/// instead of stepping when it crosses.
///
/// v2 retune: there are now FIVE orthogonal modalities, not eight overlapping
/// domains. The old curve (4→3.5, 5→5.0, …→7.0 over eight domains) was calibrated
/// for a world where four lit buckets was rare and partly collinear; with five
/// orthogonal axes, four elevated is a normal acute-crisis signature, so the curve
/// is compressed to top out near 2.6× at all five. NOTE: provisional — the final
/// curve is fitted against historical analogs in the Phase-3 backtest harness.
const CO_OCCURRENCE_ANCHORS: &[(f64, f64)] = &[
    (0.0, 1.00),
    (1.0, 1.00),
    (2.0, 1.25),
    (3.0, 1.60),
    (4.0, 2.10),
    (5.0, 2.60),
];

/// Peak-aware aggregation width (v2). The old scorer used the MEAN signal over
/// every event tagged to a domain, so a handful of severe, fresh, corroborated
/// signals were averaged against hundreds of ambient mentions — more chatter
/// pushed risk DOWN (a signal-inversion bug). v2 scores a domain from the strength
/// of its TOP-K strongest contributions instead, so a real crisis is not diluted
/// by background noise. Volume still adds a small breadth bonus but can never
/// dilute the peak.
const PEAK_K: usize = 5;

/// Half-width of the smooth ramp centred on ELEVATION_THRESHOLD used to compute
/// a domain's partial elevation weight. A domain scoring ≥ threshold+ramp counts
/// as fully elevated (1.0); ≤ threshold−ramp counts as 0.0; in between it ramps
/// smoothly (smoothstep), so a domain hovering at the boundary no longer flips
/// the co-occurrence boost discontinuously.
const ELEVATION_RAMP: f64 = 0.08;

/// Smooth 0..1 elevation weight for a single domain score. This is the SINGLE
/// source of truth for "how elevated, smoothly, is one modality" — used both by
/// the systemic co-occurrence (Step 5 here) and the intra-theater co-occurrence
/// (`theater::score_theater`), so "elevated" means exactly the same thing, on the
/// same ramp, everywhere. A score below `ELEVATION_THRESHOLD − ELEVATION_RAMP`
/// contributes EXACTLY 0; a faint sub-threshold modality can never inflate either
/// co-occurrence boost.
pub fn soft_elevation_weight(score: f64) -> f64 {
    let lo = ELEVATION_THRESHOLD - ELEVATION_RAMP;
    let hi = ELEVATION_THRESHOLD + ELEVATION_RAMP;
    if score <= lo { return 0.0; }
    if score >= hi { return 1.0; }
    let t = (score - lo) / (hi - lo); // 0..1 across the ramp
    t * t * (3.0 - 2.0 * t)           // smoothstep
}

/// Continuous co-occurrence boost: piecewise-linear interpolation of
/// CO_OCCURRENCE_ANCHORS at the (possibly fractional) soft elevation count.
pub fn co_occurrence_boost(soft_elevated: f64) -> f64 {
    let x = soft_elevated.max(0.0);
    let anchors = CO_OCCURRENCE_ANCHORS;
    let last = anchors[anchors.len() - 1];
    if x >= last.0 { return last.1; }
    for w in anchors.windows(2) {
        let (x0, y0) = w[0];
        let (x1, y1) = w[1];
        if x >= x0 && x <= x1 {
            let t = (x - x0) / (x1 - x0);
            return y0 + t * (y1 - y0);
        }
    }
    1.0
}

// Evidence gain (β) for the log-odds risk model is theater::EVIDENCE_GAIN_SYS — the v2 single
// source of truth, applied as `P = sigmoid(logit(prior) + β·l_sys)` in compute(). The dead v1
// EVIDENCE_GAIN const here (with a doc describing the superseded regime-in-prior behaviour) was
// removed to prevent the documentation drifting from the live constant. (audit bayesian-4)

// ── Guardrail-collapse coupler (Step 6b) ────────────────────────────────────────
//
// The v2 soft amplifier for structural guardrail erosion — arms-control death,
// deterrence decay, doctrine shifts toward compellence. Until those migrate to
// explicit couplers they are carried by the operator-tunable regime multiplier, so
// the guardrail term is DERIVED from it. Two honesty properties shape it:
//   • It enters the SYSTEMIC LIKELIHOOD, never the prior (Step 7 keeps the prior the
//     flat quiet-year baseline). A structurally degraded but quiet world must not
//     silently inflate the floor — it raises the stakes of acute signal, nothing more.
//   • It is deliberately SOFT and SUBORDINATE: background guardrail erosion must never
//     out-amplify the hottest theater + great-power/concurrency couplers that signal an
//     actual regional war going systemic (those live in theater.rs).

/// Regime-multiplier EXCESS above the neutral 1.0 at which guardrail collapse is
/// treated as COMPLETE: `guardrail = clamp((regime_multiplier − 1) / SPAN, 0, 1)`, so a
/// regime product of `1 + SPAN = 5.0×` saturates the coupler at 1.0 and larger products
/// add nothing further; a risk-REDUCING regime (< 1.0) floors at 0, never negative.
/// 4.0 places saturation at the high end of the plausible regime range — but note the
/// seeded acute factor set already compounds to ~5.46×, so the LIVE coupler currently
/// sits at full collapse. That is a deliberate design point of the current factor set,
/// not a knob to chase by blind-tweaking (see improvement-log 2026-06-10).
pub const GUARDRAIL_REGIME_SPAN: f64 = 4.0;

/// MAXIMUM fractional boost full guardrail collapse adds to the systemic likelihood:
/// `l_sys_amplified = l_sys × (1 + GUARDRAIL_AMPLIFIER × guardrail)`, so `guardrail = 1.0`
/// lifts l_sys by at most +12% and `guardrail = 0` leaves it untouched. Small by design —
/// a background multiplier kept well below the acute theater couplers (the single-theater
/// brink amplifier is +70%, breadth +26%, both in theater.rs), so structural decay can
/// never swamp an actual flashpoint.
pub const GUARDRAIL_AMPLIFIER: f64 = 0.12;

/// Clamped 0..1 guardrail-collapse coupler derived from the regime multiplier.
/// Monotone non-decreasing in `regime_multiplier`; 0 at/below neutral (1.0), 1.0 at/above
/// `1 + GUARDRAIL_REGIME_SPAN`. Pure function so the mapping is locked by test.
/// Public so the operator regime inspector (api.rs) reports exactly the coupler the
/// engine computes — the regime product drives guardrail collapse, NOT the prior.
pub fn guardrail_from_regime(regime_multiplier: f64) -> f64 {
    ((regime_multiplier - 1.0) / GUARDRAIL_REGIME_SPAN).clamp(0.0, 1.0)
}

// ── Operator-facing "data quality" confidence (Step 9) ──────────────────────────
//
// The snapshot-level `estimate_confidence` is the number rendered as the dashboard
// "Confidence — data quality" cell. It is DISPLAY-ONLY: it does not enter the
// P(WWIII) forecast (Step 7 is already complete by the time it is computed), so it
// carries no calibration weight and is safe to refactor as long as the value is
// preserved. But the operator reads it as "how much evidence does this number rest
// on", so per pillar-1 it must mean what it says — hence these named constants +
// the pure `estimate_confidence` below, locked by test, rather than the bare
// literals that were buried inline before.
//
// It blends three observable evidence signals, each saturating so a flood of
// low-grade events can never masquerade as certainty:
//   • the average per-domain source-tier confidence (the dominant term),
//   • event VOLUME (log-saturating — diminishing returns past a healthy feed), and
//   • the BREADTH of live sources (a single chatty source ≠ corroboration).

/// Confidence floor when the window holds ZERO events — the model is running on the
/// regime prior alone (feed outage / cold start), so it is near-blind. Not 0: the
/// structural prior still carries a little information.
pub const CONFIDENCE_OFFLINE_FLOOR: f64 = 0.05;

/// The read is **blind**: zero live events in the window, so the headline number is
/// the BASELINE PRIOR, not a measurement of the live world. This is the exact
/// condition under which `estimate_confidence` returns the offline floor — kept as a
/// named predicate (single source of truth, locked by
/// `is_data_blind_agrees_with_the_offline_confidence_floor`) so the operator-facing
/// "no live signal" warning can never drift from the model's own offline state.
/// Honesty: a baseline read during a total ingestion outage must NOT masquerade as a
/// calm, measured quiet world — the two are indistinguishable by the number alone.
pub fn is_data_blind(events: usize) -> bool {
    events == 0
}

/// Independent live feeds required before a read counts as broadly corroborated. Below
/// this, the headline — though a real measurement, not the blind baseline — rests on
/// only one or two reporting outlets (a feed-fleet partial outage where most sources are
/// dark), so it leans on a narrow base that one editorial line or a single feed bug could
/// skew. Three is the classic corroboration floor (two independent confirmations plus the
/// originator). Well below `CONFIDENCE_SOURCE_SATURATION` (20), so the breadth term of
/// confidence is far from saturated whenever this trips.
pub const MIN_CORROBORATING_SOURCES: usize = 3;

/// The read is **thinly sourced**: it has live events (so it is NOT blind) but fewer than
/// `MIN_CORROBORATING_SOURCES` distinct active feeds behind it. A weaker honesty state
/// than blindness — the number means something, but it is thinly corroborated — and
/// mutually exclusive with `is_data_blind` by construction (blind requires zero events).
/// Surfaced as a header caveat so a partial outage doesn't masquerade as a full-coverage
/// "Live" read. DISPLAY-only; never feeds the forecast. Locked by
/// `is_thinly_sourced_is_a_narrow_base_distinct_from_blindness`.
pub fn is_thinly_sourced(events: usize, sources: usize) -> bool {
    events > 0 && sources < MIN_CORROBORATING_SOURCES
}

/// The read is **at the forecast ceiling**: annual P(WWIII) has been hard-clamped to
/// `FORECAST_PROB_CEILING` (0.90) in `compute`, so the displayed number is a FLOOR, not a
/// point estimate — the unclamped systemic signal sits at or above it. Honesty: a clamped
/// 90% must NOT masquerade as a measured 90%; the operator needs to know the true read
/// could be higher but is capped for epistemic humility (the model has no ground truth).
/// The same class of "the number doesn't mean what it appears to" as `is_data_blind`. The
/// single source of truth for the operator-facing "capped" caveat, locked by
/// `is_at_forecast_ceiling_agrees_with_the_clamp`. DISPLAY-only — the clamp itself lives in
/// `compute`, so the forecast is already capped before this is read.
pub fn is_at_forecast_ceiling(p_annual: f64) -> bool {
    p_annual >= FORECAST_PROB_CEILING - 1e-9
}

/// Confidence when events exist but none carry a usable per-domain confidence
/// (degenerate edge — keeps the blend from reading the domain term as 0).
const CONFIDENCE_NO_DOMAIN_CONF: f64 = 0.1;

/// Event count at which the volume term saturates (log scale): more than ~200
/// events in the window adds essentially no further confidence — past this a
/// healthy feed is already well-corroborated and extra volume is noise, not signal.
pub const CONFIDENCE_EVENT_SATURATION: f64 = 200.0;

/// Active-source count at which the breadth term saturates: ~20 independent live
/// sources is treated as full corroboration breadth; fewer caps the term linearly.
pub const CONFIDENCE_SOURCE_SATURATION: f64 = 20.0;

/// Blend weights for the three confidence signals. They sum to 1.0 so the result
/// is a true weighted mean in [0,1] — the domain source-tier quality dominates,
/// volume next, breadth last.
pub const CONF_W_DOMAIN: f64 = 0.5;
pub const CONF_W_EVENTS: f64 = 0.3;
pub const CONF_W_SOURCES: f64 = 0.2;
// The blend is only a bounded weighted mean if the weights sum to 1 — enforce it
// at compile time so a future re-weighting can't silently push confidence > 1.
const _: () = assert!(CONF_W_DOMAIN + CONF_W_EVENTS + CONF_W_SOURCES == 1.0);

/// Operator-facing data-quality confidence in [0,1]. Pure (so it is locked by
/// `estimate_confidence_*` tests) and DISPLAY-ONLY — never feeds the forecast.
/// Monotone non-decreasing in both `events` and `sources`; returns the offline
/// floor when the window is empty. `avg_domain_conf` is the mean per-domain
/// source-tier confidence over domains that actually saw events.
pub fn estimate_confidence(avg_domain_conf: f64, events: usize, sources: usize) -> f64 {
    if events == 0 {
        return CONFIDENCE_OFFLINE_FLOOR;
    }
    let event_factor =
        ((1.0 + events as f64).ln() / (1.0 + CONFIDENCE_EVENT_SATURATION).ln()).min(1.0);
    let source_factor = (sources as f64 / CONFIDENCE_SOURCE_SATURATION).min(1.0);
    let blended = avg_domain_conf * CONF_W_DOMAIN
        + event_factor * CONF_W_EVENTS
        + source_factor * CONF_W_SOURCES;
    (blended.clamp(0.0, 1.0) * 1e3).round() / 1e3
}

/// Logistic function. Maps log-odds → probability in (0, 1).
fn sigmoid(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

const TRACKED_ACTORS: &[&str] = &[
    "united_states", "united_states_military",
    "russia", "russia_military",
    "china", "china_military",
    "north_korea", "iran", "iran_military",
    "israel", "israel_military",
    "nato", "ukraine", "ukraine_military",
    "pakistan", "india",
];

fn is_tracked_actor(actor: &str) -> bool {
    TRACKED_ACTORS.contains(&actor)
}

// ── Recency decay ──────────────────────────────────────────────────────────────

/// Domain-specific exponential decay. Nuclear/diplomatic decay slower.
/// Returns 0.0 for events older than MAX_EVENT_AGE_HOURS, and never exceeds 1.0.
///
/// Future-dated events (feed clock skew / timezone bugs — common enough that the
/// dashboard flags them) have a negative age, which would make the exponent
/// positive and return a weight > 1.0, *amplifying* the event's signal in
/// score_all (where this is a multiplier). Clamp age to 0 so a future-dated
/// event is treated as brand-new (weight 1.0) rather than super-weighted.
pub fn recency_weight(published_at: &DateTime<Utc>, domain: &str) -> f64 {
    recency_weight_scaled(published_at, domain, 1.0)
}

/// Recency weight with the domain half-life multiplied by `half_life_scale`. `scale = 1.0`
/// reproduces `recency_weight` exactly. The persistence-floor prototype (theater.rs) uses a
/// large scale (the "war-state" half-life) to compute a slowly-decaying floor under a hot
/// theater, so an active war does not collapse during a multi-day news lull while still
/// fading if it goes truly silent. Scale only affects AGED events: at age 0 the weight is
/// 1.0 for any scale, so any floor built on this is identical to the fast read at peak
/// freshness — the calibration bands (all scored at Utc::now) are provably unchanged.
pub fn recency_weight_scaled(published_at: &DateTime<Utc>, domain: &str, half_life_scale: f64) -> f64 {
    let age_hours = (Utc::now() - *published_at).num_seconds() as f64 / 3600.0;
    if age_hours > MAX_EVENT_AGE_HOURS {
        return 0.0;
    }
    let age_hours = age_hours.max(0.0); // future-dated → treat as "just now", cap weight at 1.0
    let half_life = domain_half_life(domain) * half_life_scale.max(1e-9);
    (-std::f64::consts::LN_2 * age_hours / half_life).exp()
}

/// Returns the longest half-life among the domains present in an event's signals.
fn event_max_half_life(event: &GeopoliticalEvent) -> f64 {
    // Prefer domain_signals keys (weighted NLP output); fall back to domain_tags
    let domains: Vec<&str> = if !event.domain_signals.is_empty() {
        event.domain_signals.keys().map(|s| s.as_str()).collect()
    } else {
        event.domain_tags.iter().map(|s| s.as_str()).collect()
    };

    domains.iter()
        .map(|d| domain_half_life(d))
        .fold(0.0_f64, f64::max)
        // Fall back to military_escalation half-life if no domains are tagged
        .max(domain_half_life("military_escalation"))
}

// ── Anomaly detector ──────────────────────────────────────────────────────────

/// Detects sudden spikes in domain activity vs rolling baseline.
#[derive(Debug, Default)]
pub struct AnomalyDetector {
    history: HashMap<String, VecDeque<usize>>,
    window:  usize,
}

impl AnomalyDetector {
    pub fn new(window: usize) -> Self {
        Self { history: HashMap::new(), window }
    }

    /// Returns map of domain → is_anomaly.
    pub fn update(&mut self, domain_event_counts: &HashMap<String, usize>) -> HashMap<String, bool> {
        let mut anomalies = HashMap::new();
        for (domain, &count) in domain_event_counts {
            let hist = self.history.entry(domain.clone()).or_default();
            hist.push_back(count);
            if hist.len() > self.window {
                hist.pop_front();
            }
            let is_anomaly = if hist.len() >= 3 {
                let prior: Vec<usize> = hist.iter().copied().collect();
                let n = prior.len() - 1;
                let avg = prior[..n].iter().sum::<usize>() as f64 / n as f64;
                count as f64 > (3.0 * avg).max(3.0)
            } else {
                false
            };
            anomalies.insert(domain.clone(), is_anomaly);
        }
        anomalies
    }
}

// ── Domain scorer ──────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct DomainScorer {
    anomaly_detector: AnomalyDetector,
}

impl DomainScorer {
    pub fn new() -> Self {
        Self { anomaly_detector: AnomalyDetector::new(60) }
    }

    pub fn score_all(&mut self, events: &[GeopoliticalEvent]) -> HashMap<String, DomainScore> {
        self.score_all_scaled(events, 1.0)
    }

    /// As `score_all`, but the recency decay uses each domain's half-life × `half_life_scale`.
    /// `scale = 1.0` is the normal (fast) read. The persistence-floor prototype calls this with
    /// a large scale to obtain a slowly-decaying "war-state" heat for the floor (see theater.rs).
    pub fn score_all_scaled(&mut self, events: &[GeopoliticalEvent], half_life_scale: f64) -> HashMap<String, DomainScore> {
        // Local accumulators — named scored_* to avoid shadowing event.domain_signals field.
        let mut scored_signals:    HashMap<String, Vec<f64>>        = HashMap::new();
        let mut domain_event_ids:  HashMap<String, Vec<String>>     = HashMap::new();
        let mut domain_tiers:      HashMap<String, Vec<SourceTier>> = HashMap::new();
        let mut domain_gp_count:   HashMap<String, usize>           = HashMap::new();
        let mut domain_actors:     HashMap<String, HashSet<String>> = HashMap::new();
        let mut domain_event_count: HashMap<String, usize>          = HashMap::new();

        // Seed every tracked domain with a zero count so quiet batches are recorded
        // in the anomaly baseline. Without this, a normally-quiet domain only ever
        // records its active batches, inflating its rolling average and masking the
        // very spikes the anomaly detector exists to catch.
        for &(domain, _) in DOMAIN_WEIGHTS {
            domain_event_count.insert(domain.to_string(), 0);
        }

        for event in events {
            // Iterate domain_signals (weighted NLP quality per domain).
            // Falls back to domain_tags at signal=1.0 for any event that
            // predates this change (corroboration_count: backward compat).
            let signals_iter: Vec<(String, f64)> = if !event.domain_signals.is_empty() {
                event.domain_signals.iter()
                    .map(|(d, &s)| (d.clone(), s))
                    .collect()
            } else {
                // Legacy path: events with no domain_signals (e.g. from tests
                // or older serialised data) treat all tags as full signal.
                event.domain_tags.iter()
                    .map(|d| (d.clone(), 1.0_f64))
                    .collect()
            };

            for (domain, nlp_signal) in signals_iter {
                if !DOMAIN_WEIGHTS.iter().any(|(d, _)| *d == domain) {
                    continue;
                }
                let rw = recency_weight_scaled(&event.published_at, &domain, half_life_scale);
                if rw < 0.01 { continue; }

                // Corroboration factor: each additional confirmed source adds
                // credibility beyond the base tier weight. Capped at 1.0.
                let corroboration_factor = (event.corroboration_count as f64 * 0.05)
                    .min(0.25); // max +0.25 from corroboration (5+ sources)
                let effective_credibility =
                    (event.credibility_weight + corroboration_factor).min(1.0);
                let effective_weight = rw * effective_credibility;

                // NOTE (v2): great-power involvement is NOT a per-domain scoring
                // bonus. v1 added a +0.12 lift to a `great_power_conflict` domain, but
                // v2 removed that domain entirely (the five DOMAIN_WEIGHTS modalities
                // measure the KIND of force, never WHO) and folded great-power coupling
                // into the systemic `gp_entanglement` coupler in theater.rs — exactly to
                // kill the v1 collinearity where one great-power strike lit four buckets
                // and was counted ~4×. So great_power_involved must change the systemic
                // likelihood (via the coupler), never an individual modality's score; it
                // only increments the display-only great_power_event_count below. Locked
                // by `great_power_involvement_does_not_add_a_per_domain_score_bonus`.

                // Domain-specific evidence (nlp_signal) is the SPINE. severity and
                // escalation are event-level — identical for every domain tagged on
                // the same story — so as an additive pedestal they compressed
                // co-tagged domains (nuclear/diplomatic/economic/great-power) to
                // near-identical scores. Here they MULTIPLY the domain's own evidence
                // instead: story intensity scales each domain by how strong THAT
                // domain's keyword/LLM evidence is, so two domains on the same severe
                // story diverge in proportion to their own signal rather than sharing
                // a common floor. A 0.55 floor keeps a strong-keyword/low-intensity
                // domain from collapsing; the 0.45 swing lets intensity matter. Final
                // clamp (below) bounds the result to [0,1].
                let intensity = 0.5 * event.severity
                              + 0.5 * event.escalation_language_score; // [0,1] shared story intensity
                let base_signal = nlp_signal * (0.55 + 0.45 * intensity);

                // Sentiment modulator: sentiment_score ∈ [-1, 1] where positive is
                // conciliatory (de-escalatory) and negative is hostile. Hostile
                // coverage amplifies the signal up to +15%, conciliatory coverage
                // damps it up to -15%. Bounded so tone refines but never dominates
                // the hard escalation evidence above.
                let sentiment_mod = 1.0 - 0.15 * event.sentiment_score;
                let signal = (base_signal * sentiment_mod).clamp(0.0, 1.0);

                scored_signals.entry(domain.clone()).or_default()
                    .push(signal * effective_weight);
                domain_event_ids.entry(domain.clone()).or_default()
                    .push(event.id.clone());
                domain_tiers.entry(domain.clone()).or_default()
                    .push(event.source_tier);
                *domain_event_count.entry(domain.clone()).or_insert(0) += 1;
                if event.great_power_involved {
                    *domain_gp_count.entry(domain.clone()).or_insert(0) += 1;
                }
                for aid in &event.actor_ids {
                    if is_tracked_actor(aid) {
                        domain_actors.entry(domain.clone()).or_default()
                            .insert(aid.clone());
                    }
                }
            }
        }

        let anomalies = self.anomaly_detector.update(&domain_event_count);

        let mut scores = HashMap::new();
        for &(domain, _) in DOMAIN_WEIGHTS {
            let signals     = scored_signals.get(domain).cloned().unwrap_or_default();
            let event_count = signals.len();
            let anomaly     = *anomalies.get(domain).unwrap_or(&false);

            let raw_score = if !signals.is_empty() {
                // Peak-aware core: blend the single strongest contribution with the
                // mean of the top-K. Severe corroborated signals dominate; ambient
                // chatter no longer dilutes (v2 fix for the mean-inversion bug).
                let mut sorted = signals.clone();
                sorted.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
                let k         = sorted.len().min(PEAK_K);
                let peak      = sorted[0];
                let topk_mean = sorted[..k].iter().sum::<f64>() / k as f64;
                let core      = 0.60 * peak + 0.40 * topk_mean;

                // Volume = breadth bonus only (corroborating coverage), never dilution.
                let volume_factor = ((1.0 + event_count as f64).ln()
                    / (1.0 + 20.0_f64).ln()).min(1.0);
                let actor_set  = domain_actors.get(domain)
                    .map(|s| s.len())
                    .unwrap_or(0);
                let actor_diversity = (actor_set as f64 / 4.0).min(1.0);
                let mut s = core * (0.85 + 0.10 * volume_factor + 0.05 * actor_diversity);
                if anomaly {
                    s = (s * 1.3).min(1.0);
                    info!("ANOMALY detected in domain: {domain} (event spike)");
                }
                s.min(1.0)
            } else {
                0.0
            };

            let confidence = if event_count == 0 {
                0.05
            } else {
                let tiers = domain_tiers.get(domain).cloned().unwrap_or_default();
                let tier_quality = tiers.iter().map(|t| match t {
                    SourceTier::Tier1 => 1.00,
                    SourceTier::Tier2 => 0.65,
                    SourceTier::Tier3 => 0.20,
                }).sum::<f64>() / event_count as f64;
                let count_factor = ((1.0 + event_count as f64).ln()
                    / (1.0 + 15.0_f64).ln()).min(1.0);
                let actor_conf = (domain_actors.get(domain)
                    .map(|s| s.len()).unwrap_or(0) as f64 / 3.0).min(1.0);
                (tier_quality * 0.5 + count_factor * 0.35 + actor_conf * 0.15)
                    .clamp(0.0, 1.0)
            };

            scores.insert(domain.to_string(), DomainScore {
                domain_id: domain.to_string(),
                score:     (raw_score * 1e4).round() / 1e4,
                confidence: (confidence * 1e3).round() / 1e3,
                event_count,
                great_power_event_count: *domain_gp_count.get(domain).unwrap_or(&0),
                contributing_events: domain_event_ids.get(domain).cloned().unwrap_or_default(),
                computed_at: Utc::now(),
            });
        }

        // Ensure all five modalities are always present (zeroed if no events).
        for &(domain, _) in DOMAIN_WEIGHTS {
            scores.entry(domain.to_string()).or_insert_with(|| DomainScore::zero(domain));
        }

        scores
    }
}

// ── Regime multiplier ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct RegimeMultiplier {
    factors: HashMap<String, RegimeFactor>,
}

impl RegimeMultiplier {
    pub fn new(factors: Vec<RegimeFactor>) -> Self {
        Self {
            factors: factors.into_iter().map(|f| (f.id.clone(), f)).collect(),
        }
    }

    pub fn compute(&self) -> f64 {
        let product: f64 = self.factors.values()
            .filter(|f| f.active)
            .map(|f| f.multiplier)
            .product();
        (product * 1e4).round() / 1e4
    }

    /// Toggle a regime factor — called by /api/regime/:id/toggle
    #[allow(dead_code)]
    pub fn set_factor(&mut self, id: &str, active: bool) {
        if let Some(f) = self.factors.get_mut(id) {
            f.active = active;
        }
    }

    /// Returns all factors as a vec — used by /api/regime for JSON serialisation
    #[allow(dead_code)]
    pub fn as_vec(&self) -> Vec<RegimeFactor> {
        let mut v: Vec<_> = self.factors.values().cloned().collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }
}

// ── Actor tracker ──────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct ActorTracker {
    counts:  HashMap<String, usize>,
    domains: HashMap<String, HashSet<String>>,
}

impl ActorTracker {
    /// Update actor counts and domain associations from the current event window.
    pub fn update(&mut self, events: &[GeopoliticalEvent]) {
        self.counts.clear();
        self.domains.clear();
        for event in events {
            let half_life = event_max_half_life(event);
            let age_hours = (Utc::now() - event.published_at).num_seconds() as f64 / 3600.0;
            // Compute recency using the event's own longest half-life
            let rw = if age_hours > MAX_EVENT_AGE_HOURS {
                0.0
            } else {
                (-std::f64::consts::LN_2 * age_hours / half_life).exp()
            };
            if rw < 0.1 { continue; }

            for aid in &event.actor_ids {
                if is_tracked_actor(aid) {
                    *self.counts.entry(aid.clone()).or_insert(0) += 1;
                    // Use domain_signals keys if available, fall back to domain_tags
                    let domains: Vec<&str> = if !event.domain_signals.is_empty() {
                        event.domain_signals.keys().map(|s| s.as_str()).collect()
                    } else {
                        event.domain_tags.iter().map(|s| s.as_str()).collect()
                    };
                    for d in domains {
                        self.domains.entry(aid.clone()).or_default()
                            .insert(d.to_string());
                    }
                }
            }
        }
    }

    pub fn top_actors(&self, n: usize) -> Vec<String> {
        let mut pairs: Vec<_> = self.counts.iter().collect();
        pairs.sort_by(|a, b| b.1.cmp(a.1));
        pairs.into_iter().take(n).map(|(k, _)| k.clone()).collect()
    }
}

// ── Bayesian engine ────────────────────────────────────────────────────────────

/// Calibrated risk index formula (log-odds / logistic form):
///
///   P_risk = sigmoid( logit(P₀) + β × L )   clamped to [0, FORECAST_PROB_CEILING]
///
/// where:
///   P₀     = BASELINE_ANNUAL — the FLAT modern quiet-year baseline. v2 does NOT
///            multiply the regime into the prior (that was the superseded v1 form);
///            the regime enters L as a guardrail-collapse amplifier (Step 6), so a
///            calm world sits at P₀ and the systemic likelihood does all the lifting.
///   L      = systemic likelihood l_sys × (1 + GUARDRAIL_AMPLIFIER × guardrail_collapse)
///   β      = EVIDENCE_GAIN_SYS
///
/// NOTE — Mathematical character of this formula:
///   This is NOT a formal Bayesian update P(H|E) = P(E|H)P(H)/P(E). It is a
///   calibrated risk index that combines the flat baseline prior with the
///   likelihood evidence additively on the log-odds scale — the standard way to
///   fold evidence into a prior probability. Properties this buys over the older
///   `P₀_adj × (1 + L·k)` form: the output is always a valid probability in
///   (0, ceiling], it is monotonic and smooth in L, it returns the prior exactly
///   when L = 0, and it saturates gracefully so strong multi-domain signals are
///   expressive rather than capped near 15%. It still does not derive from a
///   generative model; "posterior" here means the output probability, not a
///   formal posterior distribution.
///
/// NOTE — The forecast ceiling:
///   The `.min(FORECAST_PROB_CEILING)` clamp (0.90, defined in models.rs) is an
///   engineering ceiling, not a probabilistic prior. Its purpose is to prevent the
///   model from emitting values near certainty, which would be epistemically
///   unjustifiable regardless of observed signals (the model has no access to
///   ground truth). The appropriate ceiling for extreme scenarios — e.g. confirmed
///   nuclear detonation — is a design decision belonging to Robert Perreault and is
///   not derived from the model itself. See models::FORECAST_PROB_CEILING for the
///   single source of truth (the value lives there, not as a bare literal here).
///
/// Calibration targets:
///   Cuba 1962 equivalent (6 domains, max signals)       → ~30-40% annual
///   Ukraine 2022 equivalent (5 domains, high signals)   → ~8-12% annual
///   Current world 2026 (4-5 domains, moderate)          → ~4-8% annual
///   Quiet period (1-2 domains, low signals)             → ~0.5-1.5% annual
pub struct BayesianRiskEngine {
    regime:         RegimeMultiplier,
    domain_scorer:  DomainScorer,
    actor_tracker:  ActorTracker,
    alert_elevated: f64,
    alert_critical: f64,
    prev_annual:    f64,
    prev_30day:     f64,
    /// False until the first snapshot is computed. The very first tick after a
    /// (re)start has NO genuine previous snapshot to diff against, so its delta is
    /// reported as 0 rather than a cold-start artifact (differencing the seed
    /// `prev_annual = HISTORICAL_ANCHOR` / `prev_30day = 0.0` would render a
    /// fabricated "▲ +N% last snap" jump on the dashboard that never happened).
    has_prev_snapshot: bool,
    theater_engine: TheaterEngine,
}

impl BayesianRiskEngine {
    pub fn new(
        regime_factors: Vec<RegimeFactor>,
        alert_elevated: f64,
        alert_critical: f64,
    ) -> Self {
        Self {
            regime:         RegimeMultiplier::new(regime_factors),
            domain_scorer:  DomainScorer::new(),
            actor_tracker:  ActorTracker::default(),
            alert_elevated,
            alert_critical,
            prev_annual:    HISTORICAL_ANCHOR,
            prev_30day:     0.0,
            has_prev_snapshot: false,
            theater_engine: TheaterEngine::new(),
        }
    }

    pub fn compute(&mut self, events: &[GeopoliticalEvent]) -> RiskSnapshot {
        // ── Step 1: Structural regime multiplier ──
        // v2: this does NOT adjust the prior (the superseded v1 form). It drives
        // guardrail collapse (Step 6), which softly amplifies the systemic likelihood
        // l_sys — never the flat baseline prior. See guardrail_from_regime.
        // (historical_anchor defaults to the flat HISTORICAL_ANCHOR baseline.)
        let mut snap = RiskSnapshot {
            regime_multiplier: self.regime.compute(),
            ..RiskSnapshot::default()
        };

        // ── Step 2: Actor tracking ──
        self.actor_tracker.update(events);

        // ── Step 3: Domain scores ──
        snap.domain_scores    = self.domain_scorer.score_all(events);
        // events_in_window counts LIVE events only — those whose recency weight is still
        // meaningful (~10-day cutoff at the 72h military half-life), the SAME gate Step 4
        // applies to sources/regions/great-power events. A warm multi-year backlog must NOT
        // keep is_data_blind() false during a live ingestion outage: this field drives the
        // honesty caveats (data_blind, thinly_sourced), the offline warn, and the confidence
        // volume term, and a baseline read during a total outage must not masquerade as a
        // calm, measured quiet world (see is_data_blind / CONFIDENCE_OFFLINE_FLOOR). The raw
        // stored-window size is not a "live signal" count and is deliberately not reported.
        snap.events_in_window = events.iter()
            .filter(|e| recency_weight(&e.published_at, "military_escalation") > 0.1)
            .count();

        // ── Step 4: Metadata ──
        snap.great_power_events = events.iter()
            .filter(|e| e.great_power_involved
                && recency_weight(&e.published_at, "military_escalation") > 0.1)
            .count();

        let mut regions: Vec<String> = events.iter()
            .filter_map(|e| {
                if recency_weight(&e.published_at, "military_escalation") > 0.1 {
                    e.region.clone()
                } else {
                    None
                }
            })
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        regions.sort();
        snap.regions_active = regions;
        snap.top_actors     = self.actor_tracker.top_actors(6);

        let mut sources: HashSet<&str> = HashSet::new();
        for e in events {
            if recency_weight(&e.published_at, "military_escalation") > 0.1 {
                sources.insert(&e.source);
            }
        }
        snap.sources_active = sources.len();

        // ── Step 5: Co-occurrence ──
        // Hard count is reported for human display; the boost is driven by the
        // soft (fractional) count so it varies continuously near the threshold.
        let elevated = snap.domain_scores.values()
            .filter(|ds| ds.score >= ELEVATION_THRESHOLD)
            .count();
        let soft_elevated: f64 = snap.domain_scores.values()
            .map(|ds| soft_elevation_weight(ds.score))
            .sum();
        snap.elevated_domains    = elevated;
        snap.co_occurrence_boost = (co_occurrence_boost(soft_elevated) * 1e4).round() / 1e4;

        // ── Step 6: Global modality weighted sum (compat display only) ──
        // The v2 risk driver is the per-theater systemic likelihood (Step 6b). This
        // global weighted sum is retained only for the legacy domain-grid display.
        let weighted_sum: f64 = DOMAIN_WEIGHTS.iter()
            .map(|(d, w)| {
                snap.domain_scores.get(*d)
                    .map(|ds| ds.score * w)
                    .unwrap_or(0.0)
            })
            .sum();
        snap.weighted_domain_sum = (weighted_sum * 1e6).round() / 1e6;

        // ── Step 6b: Theater decomposition + systemic likelihood (v2) ──
        // Risk is no longer a global average. It is the hottest theater amplified by
        // great-power coupling, multi-theater concurrency, and guardrail collapse —
        // the actual signature of a regional war going systemic.
        let mut tout = self.theater_engine.compute(events);
        // Guardrail collapse is carried by the operator-tunable regime multiplier
        // (arms-control death, deterrence erosion, …) until settings migrate to
        // explicit couplers. Normalise it to 0..1 for display and as a soft amplifier
        // of the systemic likelihood (never the prior). See GUARDRAIL_* above.
        let guardrail = guardrail_from_regime(snap.regime_multiplier);
        tout.couplers.guardrail_collapse = (guardrail * 1e3).round() / 1e3;
        let l_sys = tout.l_sys * (1.0 + GUARDRAIL_AMPLIFIER * guardrail);
        // The "dominant coupling channel" read-out (couplers.coupling_driver) is named in
        // the theater engine from only the four ACUTE couplers — it cannot see this fifth,
        // structural one, because guardrail collapse is derived here from the regime
        // multiplier. Its lift on l_sys is GUARDRAIL_AMPLIFIER × guardrail, directly
        // comparable to the acute lifts. If structural collapse is the largest single
        // amplifier of a LIVE crisis, name it — otherwise the operator would be told
        // "regional, not yet systemically coupled" while eroded arms-control / deterrence is
        // the only thing lifting the systemic likelihood. Gated on tout.l_sys > floor (a real
        // hot theater exists): per the engine's honesty invariant, guardrails amplify a live
        // crisis but never manufacture risk from calm, so a quiet world is never "led by"
        // them. Strict `>` keeps the apex-severity tie-break — an acute coupler of equal lift
        // still wins (guardrail is the soft, subordinate background channel).
        let guardrail_lift = GUARDRAIL_AMPLIFIER * guardrail;
        if tout.l_sys > COUPLING_AMPLIFIER_FLOOR
            && guardrail_lift > COUPLING_AMPLIFIER_FLOOR
            && guardrail_lift > tout.coupling_driver_lift
        {
            tout.couplers.coupling_driver = "structural guardrail collapse".to_string();
        }
        snap.theaters         = tout.theaters;
        snap.couplers         = tout.couplers;
        // Authoritative public headline index: a continuous rendering of the SAME final,
        // guardrail-amplified `l_sys` that produces P(WWIII) below — so the 0..95 index and the
        // headline probability are one number on two scales and can never disagree. (The theater
        // engine's own `tout.systemic_index` is the pre-guardrail view used by its unit tests;
        // the public number applies the guardrail here.) Replaces the retired rung staircase that
        // read 83.3 for Ukraine-2022, the present world, the live peg and Cuba alike.
        snap.systemic_index   = (crate::theater::index_from_l_sys(l_sys) * 1e2).round() / 1e2;
        snap.driver           = tout.driver;
        snap.likelihood_ratio = (l_sys * 1e6).round() / 1e6;

        // ── Step 7: Risk index computation (log-odds / logistic) ──
        // v2: the logistic prior is the FLAT modern baseline (quiet-year floor). The
        // structural regime no longer inflates the prior — it enters l_sys above as a
        // guardrail amplifier — so a calm world sits at the baseline and the SYSTEMIC
        // likelihood does all the lifting. L = 0 reproduces the baseline; large L
        // saturates toward the 0.90 ceiling along an S-curve.
        let prior         = HISTORICAL_ANCHOR.clamp(1e-9, 0.5); // BASELINE_ANNUAL (flat)
        let prior_logodds = (prior / (1.0 - prior)).ln();
        let raw = sigmoid(prior_logodds + EVIDENCE_GAIN_SYS * l_sys)
            .min(FORECAST_PROB_CEILING); // Engineering ceiling — epistemic humility, not a prior (see models::FORECAST_PROB_CEILING)
        snap.p_wwiii_annual = (raw * 1e8).round() / 1e8;

        // Re-express the annual read over the nearer horizons under a constant-hazard
        // assumption: P(window) = 1 − (1 − P_annual)^(window_days/365). The fields are
        // named — and the dashboard labels them — "30-day"/"90-day", so the exponent must
        // be the day fraction of the SAME 365-day year the annual figure uses. The old
        // 1/12 and 3/12 silently switched the year to 12 equal months (30.4 / 91.25 days),
        // so the served number meant a slightly different horizon than its label claimed.
        const DAYS_PER_YEAR: f64 = 365.0;
        snap.p_wwiii_30day  = ((1.0 - (1.0 - raw).powf(30.0 / DAYS_PER_YEAR)) * 1e8).round() / 1e8;
        snap.p_wwiii_90day  = ((1.0 - (1.0 - raw).powf(90.0 / DAYS_PER_YEAR)) * 1e8).round() / 1e8;

        // ── Step 8: Delta (change since the PREVIOUS snapshot) ──
        // The first tick after a (re)start has no real previous snapshot, so its delta is
        // 0 (a stable "─"), not the cold-start seed differenced into a phantom jump. From
        // the second tick on, the delta is a true inter-snapshot move.
        if self.has_prev_snapshot {
            snap.delta_annual = ((snap.p_wwiii_annual - self.prev_annual) * 1e8).round() / 1e8;
            snap.delta_30day  = ((snap.p_wwiii_30day  - self.prev_30day)  * 1e8).round() / 1e8;
        } else {
            snap.delta_annual = 0.0;
            snap.delta_30day  = 0.0;
            self.has_prev_snapshot = true;
        }
        self.prev_annual  = snap.p_wwiii_annual;
        self.prev_30day   = snap.p_wwiii_30day;

        // ── Step 9: Confidence (operator-facing "data quality"; display-only) ──
        if snap.events_in_window == 0 {
            warn!("No events in window — model running on regime prior only (offline?)");
        }
        let domain_confs: Vec<f64> = snap.domain_scores.values()
            .filter(|ds| ds.event_count > 0)
            .map(|ds| ds.confidence)
            .collect();
        let avg_conf = if domain_confs.is_empty() {
            CONFIDENCE_NO_DOMAIN_CONF
        } else {
            domain_confs.iter().sum::<f64>() / domain_confs.len() as f64
        };
        snap.estimate_confidence =
            estimate_confidence(avg_conf, snap.events_in_window, snap.sources_active);

        // ── Step 10: Alert ──
        // Record the configured thresholds onto the snapshot so the JSON is
        // self-describing — the dashboard draws the critical band + colours risk
        // from these live values, never a hardcoded literal that could drift.
        snap.alert_elevated_threshold = self.alert_elevated;
        snap.alert_critical_threshold = self.alert_critical;
        if raw >= self.alert_critical {
            snap.alert_level   = AlertLevel::Critical;
            snap.alert_message = format!(
                "CRITICAL — P(WWIII) {:.3}% exceeds {:.1}% threshold. \
                 {} domains elevated. Co-occurrence ×{}. Confidence: {:.0}%.",
                raw * 100.0,
                self.alert_critical * 100.0,
                elevated,
                snap.co_occurrence_boost,
                snap.estimate_confidence * 100.0,
            );
        } else if raw >= self.alert_elevated {
            snap.alert_level   = AlertLevel::Elevated;
            snap.alert_message = format!(
                "ELEVATED — P(WWIII) {:.3}%. {} domain(s) above {:.0}% threshold. \
                 Δ {:+.4}%/snapshot.",
                raw * 100.0,
                elevated,
                ELEVATION_THRESHOLD * 100.0,
                snap.delta_annual * 100.0,
            );
        } else {
            snap.alert_level   = AlertLevel::Normal;
            snap.alert_message = String::new();
        }

        info!(
            "P(WWIII)={:.4}% | idx={:.0} | {} | Δ{:+.4}% | regime×{} | elevated={}/{} | \
             L_sys={:.4} | events={} | confidence={:.0}%",
            raw * 100.0,
            snap.systemic_index,
            snap.driver,
            snap.delta_annual * 100.0,
            snap.regime_multiplier,
            elevated,
            DOMAIN_WEIGHTS.len(),
            l_sys,
            snap.events_in_window,
            snap.estimate_confidence * 100.0,
        );

        snap
    }

    /// Toggle regime factor — called by operator API toggle handler
    #[allow(dead_code)]
    pub fn set_regime_factor(&mut self, id: &str, active: bool) {
        self.regime.set_factor(id, active);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn minimal_engine() -> BayesianRiskEngine {
        BayesianRiskEngine::new(
            vec![
                RegimeFactor { id: "proxy_wars".into(),        label: "Active proxy wars".into(),  multiplier: 1.4, active: true  },
                RegimeFactor { id: "war_in_europe".into(),     label: "War in Europe".into(),       multiplier: 1.6, active: true  },
                RegimeFactor { id: "deterrence_intact".into(), label: "Deterrence intact".into(),   multiplier: 0.7, active: true  },
            ],
            0.025,  // elevated threshold
            0.08,   // critical threshold
        )
    }

    fn make_event(domain: &str, severity: f64, hours_ago: f64, tier: SourceTier) -> GeopoliticalEvent {
        use crate::models::EventType;
        let mut e = GeopoliticalEvent::new(
            "Test event".into(),
            "testsource".into(),
            tier,
            Utc::now() - Duration::seconds((hours_ago * 3600.0) as i64),
        );
        e.domain_tags             = vec![domain.to_string()];
        e.severity                = severity;
        e.escalation_language_score = 0.3;
        e.event_type              = EventType::MilitaryStrike;
        e.theater                 = Some("us_iran".to_string()); // v2: drive a real theater
        e
    }

    fn make_event_with_signals(domain: &str, severity: f64, hours_ago: f64, tier: SourceTier) -> GeopoliticalEvent {
        use crate::models::EventType;
        let mut e = GeopoliticalEvent::new(
            "Test event".into(),
            "testsource".into(),
            tier,
            Utc::now() - Duration::seconds((hours_ago * 3600.0) as i64),
        );
        e.domain_tags             = vec![domain.to_string()];
        e.domain_signals          = [(domain.to_string(), 0.8)].into_iter().collect();
        e.severity                = severity;
        e.escalation_language_score = 0.3;
        e.event_type              = EventType::MilitaryStrike;
        e.theater                 = Some("us_iran".to_string()); // v2: drive a real theater
        e
    }

    // ── Historical prior ──────────────────────────────────────────────────────

    #[test]
    fn anchor_is_modern_baseline() {
        // v2: anchor is the modern quiet-year baseline, not the 2/2026 frequency.
        assert!((HISTORICAL_ANCHOR - 0.015).abs() < 1e-9);
        assert!((HISTORICAL_ANCHOR - 2.0 / 2026.0).abs() > 1e-4);
    }

    #[test]
    fn anchor_is_modest_quiet_year_floor() {
        // v2: the modern quiet-year baseline (~1.5%) sits above the old sub-1% floor
        // but well below the elevated alert threshold (2.5%).
        const { assert!(HISTORICAL_ANCHOR > 0.005 && HISTORICAL_ANCHOR < 0.025) };
    }

    // ── Recency decay ─────────────────────────────────────────────────────────

    #[test]
    fn fresh_event_weight_near_one() {
        let pub_at = Utc::now() - Duration::seconds(300); // 5 min ago
        assert!(recency_weight(&pub_at, "military_escalation") > 0.99);
    }

    #[test]
    fn military_half_life_is_72h() {
        // Military escalation now persists like nuclear posture (72h): an active war is a
        // sustained state, not an episodic news pulse (2026-06-21 realism fix). At one
        // half-life (72h) the weight is ~0.5; at the OLD half-life (24h) it is still ~0.79,
        // not halved — the property that keeps the systemic read stable across a news lull.
        let w_72 = recency_weight(&(Utc::now() - Duration::hours(72)), "military_escalation");
        assert!(w_72 > 0.45 && w_72 < 0.55, "72h military weight should be ~0.5, got {w_72}");
        let w_24 = recency_weight(&(Utc::now() - Duration::hours(24)), "military_escalation");
        assert!(w_24 > 0.75 && w_24 < 0.82, "24h military weight should now be ~0.79, got {w_24}");
    }

    #[test]
    fn beyond_max_age_weight_is_zero() {
        let pub_at = Utc::now() - Duration::seconds(((MAX_EVENT_AGE_HOURS + 1.0) * 3600.0) as i64);
        assert_eq!(recency_weight(&pub_at, "military_escalation"), 0.0);
    }

    #[test]
    fn future_dated_event_weight_capped_at_one() {
        // Feed clock skew / tz bugs can date an item ahead of now. Such an event
        // must never be super-weighted (> 1.0) — that would amplify its signal in
        // score_all. It is treated as brand-new instead (weight ~1.0).
        let week_ahead = Utc::now() + Duration::hours(168);
        let w = recency_weight(&week_ahead, "cyber_info_ops"); // 24h half-life domain
        assert!(w <= 1.0, "future-dated weight must not exceed 1.0, got {w}");
        assert!(w > 0.99, "future-dated event should read as fresh, got {w}");
    }

    #[test]
    fn h72_event_still_has_weight() {
        // At ~one half-life (73h) a military event retains ~half its weight (sustained-state
        // 72h half-life): a multi-day-old war signal still counts, but is clearly decayed.
        let pub_at = Utc::now() - Duration::hours(73);
        let w = recency_weight(&pub_at, "military_escalation");
        assert!(w > 0.40 && w < 0.55, "73h military weight should be ~half, got {w}");
    }

    #[test]
    fn sustained_state_domains_decay_slowest_cyber_fastest() {
        // Corrected persistence ordering (2026-06-21): military escalation is a sustained
        // strategic state and now shares nuclear posture's 72h half-life — they are PEERS,
        // both slower than diplomatic (48h) and far slower than the genuinely episodic cyber
        // (24h); economic warfare (96h) lingers longest of all. This locks the realism intent
        // that an active war must not decay faster than a nuclear posture shift.
        let pub_at = Utc::now() - Duration::hours(48);
        let w_cyber = recency_weight(&pub_at, "cyber_info_ops");
        let w_diplo = recency_weight(&pub_at, "diplomatic_breakdown");
        let w_mil   = recency_weight(&pub_at, "military_escalation");
        let w_nuc   = recency_weight(&pub_at, "nuclear_posture");
        let w_eco   = recency_weight(&pub_at, "economic_warfare");
        assert!(w_cyber < w_diplo, "cyber must decay fastest");
        assert!(w_diplo < w_mil,   "military must persist longer than diplomatic");
        assert!((w_mil - w_nuc).abs() < 1e-9, "military and nuclear are sustained-state peers (both 72h)");
        assert!(w_eco > w_nuc,     "economic warfare must linger longest");
    }

    #[test]
    fn economic_domain_decays_slower_than_military() {
        let pub_at = Utc::now() - Duration::hours(48);
        let w_mil = recency_weight(&pub_at, "military_escalation");
        let w_eco = recency_weight(&pub_at, "economic_warfare");
        assert!(w_eco > w_mil);
    }

    // ── AnomalyDetector window (I-10) ─────────────────────────────────────────

    #[test]
    fn anomaly_detector_window_is_60() {
        let scorer = DomainScorer::new();
        assert_eq!(scorer.anomaly_detector.window, 60,
            "AnomalyDetector window must be 60 batches (I-10 fix)");
    }

    #[test]
    fn anomaly_detector_no_false_positive_on_small_burst() {
        let mut det = AnomalyDetector::new(60);
        let mut counts = HashMap::new();
        counts.insert("military_escalation".into(), 50usize); // spike
        let anomalies = det.update(&counts);
        assert!(!anomalies["military_escalation"],
            "Single spike with insufficient baseline history should not trigger anomaly");
    }

    // ── ActorTracker half-life fix (I-11) ─────────────────────────────────────

    #[test]
    fn actor_tracker_retains_nuclear_actors_at_78h() {
        let mut tracker = ActorTracker::default();
        let mut event = make_event_with_signals("nuclear_posture", 0.9, 78.0, SourceTier::Tier1);
        event.actor_ids = vec!["north_korea".into()];
        tracker.update(&[event]);
        assert!(tracker.counts.contains_key("north_korea"),
            "Nuclear actor at 78h should be retained — recency_weight(nuclear, 78h) ≈ 0.48 > 0.1");
    }

    #[test]
    fn actor_tracker_retains_military_actors_at_82h_drops_past_10_days() {
        // Corrected for the 72h sustained-state military half-life (2026-06-21). The 0.1
        // retention threshold is now crossed at:
        //   h = 72 × ln(10) / ln(2) ≈ 239.2h (~10 days)
        // so a military actor at 82h is RETAINED (an active war's combatant stays tracked,
        // not forgotten overnight), and is only dropped once the kinetic signal has been
        // silent for well over a week.
        // At 82h:  recency_weight("military_escalation", 82h)  = exp(-ln2 × 82/72)  ≈ 0.454 > 0.1 (retained).
        // At 260h: recency_weight("military_escalation", 260h) = exp(-ln2 × 260/72) ≈ 0.082 < 0.1 (dropped).
        let mut retained = ActorTracker::default();
        let mut e82 = make_event_with_signals("military_escalation", 0.9, 82.0, SourceTier::Tier1);
        e82.actor_ids = vec!["russia_military".into()];
        retained.update(&[e82]);
        assert!(retained.counts.contains_key("russia_military"),
            "Military actor at 82h should now be retained — recency_weight(military, 82h) ≈ 0.454 > 0.1");

        let mut dropped = ActorTracker::default();
        let mut e260 = make_event_with_signals("military_escalation", 0.9, 260.0, SourceTier::Tier1);
        e260.actor_ids = vec!["russia_military".into()];
        dropped.update(&[e260]);
        assert!(!dropped.counts.contains_key("russia_military"),
            "Military actor at 260h (~11 days quiet) should be dropped — recency_weight ≈ 0.082 < 0.1");
    }

    #[test]
    fn actor_tracker_retains_economic_actors_at_78h() {
        // Economic event: half-life 96h. recency_weight("economic_warfare", 78h) ≈ 0.57.
        let mut tracker = ActorTracker::default();
        let mut event = make_event_with_signals("economic_warfare", 0.7, 78.0, SourceTier::Tier1);
        event.actor_ids = vec!["china".into()];
        tracker.update(&[event]);
        assert!(tracker.counts.contains_key("china"),
            "Economic actor at 78h should be retained — recency_weight(economic, 78h) ≈ 0.57 > 0.1");
    }

    #[test]
    fn every_domain_id_has_explicit_half_life_and_weight() {
        // The /api/domains endpoint and the decay/scoring paths look up each domain's
        // half-life and weight by id, falling back to a generic default (24.0h / 1.0)
        // when an id is missing. That fallback would silently mis-serve a domain added
        // to DOMAIN_IDS without a matching table entry. Lock the invariant: every
        // canonical domain must appear explicitly in BOTH tables, and neither table may
        // carry a stray id that isn't a canonical domain.
        for &id in crate::models::DOMAIN_IDS {
            assert!(DOMAIN_HALF_LIVES.iter().any(|(d, _)| *d == id),
                "DOMAIN_HALF_LIVES is missing an explicit entry for domain {id}");
            assert!(DOMAIN_WEIGHTS.iter().any(|(d, _)| *d == id),
                "DOMAIN_WEIGHTS is missing an explicit entry for domain {id}");
        }
        for (d, _) in DOMAIN_HALF_LIVES {
            assert!(crate::models::DOMAIN_IDS.contains(d),
                "DOMAIN_HALF_LIVES carries a stray id {d} not in DOMAIN_IDS");
        }
        for (d, _) in DOMAIN_WEIGHTS {
            assert!(crate::models::DOMAIN_IDS.contains(d),
                "DOMAIN_WEIGHTS carries a stray id {d} not in DOMAIN_IDS");
        }
    }

    #[test]
    fn event_max_half_life_returns_longest() {
        use crate::models::EventType;
        let mut event = GeopoliticalEvent::new(
            "Test".into(), "src".into(), SourceTier::Tier1, Utc::now()
        );
        event.domain_signals = [
            ("cyber_info_ops".into(),      0.5),  // 24h
            ("nuclear_posture".into(),     0.8),  // 72h — longest of this pair
        ].into_iter().collect();
        event.event_type = EventType::NuclearTest;
        let hl = event_max_half_life(&event);
        assert!((hl - 72.0).abs() < 1e-9,
            "Max half-life for a cyber+nuclear event should be 72h (nuclear_posture), got {hl}");
    }

    #[test]
    fn event_max_half_life_fallback_military_when_no_domains() {
        use crate::models::EventType;
        let mut event = GeopoliticalEvent::new(
            "Test".into(), "src".into(), SourceTier::Tier1, Utc::now()
        );
        event.event_type = EventType::MilitaryStrike;
        // No domain_signals, no domain_tags
        let hl = event_max_half_life(&event);
        assert!((hl - 72.0).abs() < 1e-9,
            "Max half-life with no domains should fall back to military_escalation 72h, got {hl}");
    }

    // ── Domain scorer ─────────────────────────────────────────────────────────

    #[test]
    fn empty_events_gives_zero_scores() {
        let mut scorer = DomainScorer::new();
        let scores = scorer.score_all(&[]);
        for ds in scores.values() {
            assert_eq!(ds.score, 0.0, "domain {} should be zero", ds.domain_id);
        }
    }

    #[test]
    fn nuclear_event_elevates_nuclear_domain() {
        let mut scorer = DomainScorer::new();
        let event = make_event("nuclear_posture", 0.9, 1.0, SourceTier::Tier1);
        let scores = scorer.score_all(&[event]);
        assert!(scores["nuclear_posture"].score > 0.3);
    }

    #[test]
    fn stale_events_dont_score() {
        let mut scorer = DomainScorer::new();
        let event = make_event("military_escalation", 0.7, MAX_EVENT_AGE_HOURS + 1.0, SourceTier::Tier1);
        let scores = scorer.score_all(&[event]);
        assert_eq!(scores["military_escalation"].score, 0.0);
    }

    #[test]
    fn h80_event_still_scores_but_is_decayed() {
        // Military escalation is a sustained-state 72h domain: at 80h (just past one half-life)
        // weight = exp(-ln2 * 80/72) ≈ 0.46, so an old war signal still scores meaningfully but
        // is clearly decayed relative to a fresh one. Compared against a fresh equivalent so the
        // invariant is "decayed-but-present", not a magic magnitude tied to a specific half-life.
        let fresh = DomainScorer::new()
            .score_all(&[make_event("military_escalation", 0.7, 0.5, SourceTier::Tier1)])
            ["military_escalation"].score;
        let aged = DomainScorer::new()
            .score_all(&[make_event("military_escalation", 0.7, 80.0, SourceTier::Tier1)])
            ["military_escalation"].score;
        assert!(aged > 0.0, "an 80h military event should still score, got {aged}");
        assert!(aged < fresh, "an 80h event must be decayed below a fresh one (fresh={fresh}, aged={aged})");
        assert!(aged > 0.3 * fresh,
            "at ~one half-life the 80h score should retain a meaningful fraction, got {aged} vs fresh {fresh}");
    }

    #[test]
    fn tier3_source_scores_lower() {
        let mut scorer = DomainScorer::new();
        let e1 = make_event("military_escalation", 0.7, 1.0, SourceTier::Tier1);
        let e3 = make_event("military_escalation", 0.7, 1.0, SourceTier::Tier3);
        let s1 = scorer.score_all(&[e1])["military_escalation"].score;
        let mut scorer2 = DomainScorer::new();
        let s3 = scorer2.score_all(&[e3])["military_escalation"].score;
        assert!(s1 > s3, "tier1 score {s1} should exceed tier3 score {s3}");
    }

    #[test]
    fn contributing_events_populated() {
        let mut scorer = DomainScorer::new();
        let event = make_event("military_escalation", 0.8, 1.0, SourceTier::Tier1);
        let id = event.id.clone();
        let scores = scorer.score_all(&[event]);
        assert!(scores["military_escalation"].contributing_events.contains(&id));
    }

    #[test]
    fn great_power_event_count_populated() {
        let mut scorer = DomainScorer::new();
        let mut event = make_event("military_escalation", 0.8, 1.0, SourceTier::Tier1);
        event.great_power_involved = true;
        let scores = scorer.score_all(&[event]);
        assert_eq!(scores["military_escalation"].great_power_event_count, 1);
    }

    #[test]
    fn great_power_involvement_does_not_add_a_per_domain_score_bonus() {
        // Honesty / v2 design intent (roadmap 1.2): great-power involvement is a
        // SYSTEMIC COUPLER (gp_entanglement in theater.rs), never a per-domain scoring
        // bonus. v1 carried a `great_power_conflict` domain with a +0.12 lift; v2
        // dropped that domain (the five DOMAIN_WEIGHTS modalities measure the KIND of
        // force, not WHO) so the old `gp_bonus` branch was dead code — it keyed on a
        // domain that score_all can never iterate. Removing it must change NOTHING, and
        // a future run must not "re-add" a per-domain GP bonus believing GP is unscored.
        //
        // Lock: the same events scored with great_power_involved true vs false produce
        // byte-identical modality SCORES (GP enters only via the coupler), while the
        // display-only great_power_event_count still reflects the flag.
        let mut e_plain = make_event_with_signals("military_escalation", 0.9, 1.0, SourceTier::Tier1);
        e_plain.domain_signals.insert("nuclear_posture".into(), 0.7);
        e_plain.domain_tags.push("nuclear_posture".into());
        e_plain.great_power_involved = false;

        let mut e_gp = e_plain.clone();
        e_gp.great_power_involved = true;

        let plain = DomainScorer::new().score_all(&[e_plain]);
        let gp    = DomainScorer::new().score_all(&[e_gp]);

        for &(domain, _) in DOMAIN_WEIGHTS {
            assert_eq!(
                plain[domain].score, gp[domain].score,
                "great_power_involved must not change the {domain} score — \
                 GP is a coupler, not a per-domain bonus"
            );
        }
        // The flag still flows to the display-only count (so awareness isn't lost).
        assert_eq!(plain["military_escalation"].great_power_event_count, 0);
        assert_eq!(gp["military_escalation"].great_power_event_count, 1);
    }

    // ── Bayesian engine ───────────────────────────────────────────────────────

    #[test]
    fn compute_records_the_configured_alert_thresholds_on_the_snapshot() {
        // The snapshot must carry the SAME alert-band thresholds the engine used to
        // classify it, so the dashboard's critical reference line + risk colours
        // track the live AlertSettings instead of a hardcoded literal. Construct an
        // engine with non-default thresholds to prove it's the engine's configured
        // values that flow through, not a constant.
        let mut engine = BayesianRiskEngine::new(vec![], 0.03, 0.11);
        let snap = engine.compute(&[]);
        assert_eq!(snap.alert_elevated_threshold, 0.03);
        assert_eq!(snap.alert_critical_threshold, 0.11);
        // And they must agree with the band that actually classified the snapshot.
        assert!(snap.p_wwiii_annual < snap.alert_elevated_threshold);
        assert_eq!(snap.alert_level, AlertLevel::Normal);
    }

    #[test]
    fn baseline_probability_below_alert() {
        let mut engine = minimal_engine();
        let snap = engine.compute(&[]);
        // v2 flat prior: an empty/quiet world sits at the baseline (~1.5%), below the
        // 2.5% elevated threshold.
        assert!(snap.p_wwiii_annual <= 0.016, "empty world should sit at the flat baseline");
        assert_eq!(snap.alert_level, AlertLevel::Normal);
    }

    #[test]
    fn regime_multiplier_product() {
        // proxy_wars(1.4) × war_in_europe(1.6) × deterrence_intact(0.7) = 1.568
        let mut engine = minimal_engine();
        let snap = engine.compute(&[]);
        assert!((snap.regime_multiplier - 1.568).abs() < 0.01);
    }

    #[test]
    fn guardrail_coupler_is_a_bounded_soft_subordinate_amplifier() {
        // Honesty (roadmap 1.2): the guardrail-collapse coupler is now named, bounded,
        // and SOFT. Lock the RELATIONSHIPS, not the fitted magnitudes.

        // (a) magnitude sanity — a soft background term, not an acute driver.
        const { assert!(GUARDRAIL_AMPLIFIER > 0.0 && GUARDRAIL_AMPLIFIER < 0.20,
            "guardrail amplifier must be a small soft background multiplier") };
        const { assert!(GUARDRAIL_REGIME_SPAN > 0.0) };

        // (b) the regime→guardrail map: 0 at/below neutral, linear, saturating at 1.0.
        assert_eq!(guardrail_from_regime(1.0), 0.0, "neutral regime leaks nothing");
        assert_eq!(guardrail_from_regime(0.7), 0.0, "a risk-reducing regime cannot go negative");
        assert_eq!(guardrail_from_regime(1.0 + GUARDRAIL_REGIME_SPAN), 1.0, "saturates at full collapse");
        assert_eq!(guardrail_from_regime(100.0), 1.0, "clamped at 1.0 above saturation");
        let mid = guardrail_from_regime(1.0 + GUARDRAIL_REGIME_SPAN / 2.0);
        assert!((mid - 0.5).abs() < 1e-12, "linear between neutral and saturation");
        assert!(guardrail_from_regime(2.5) > guardrail_from_regime(1.5), "monotone increasing");

        // (c) the amplifier is bounded: l_sys × (1 + AMP·guardrail) ∈ [l_sys, l_sys·(1+AMP)].
        let l = 10.0_f64;
        let quiet = l * (1.0 + GUARDRAIL_AMPLIFIER * guardrail_from_regime(1.0));
        assert_eq!(quiet, l, "no guardrail collapse → l_sys untouched (never inflates the floor)");
        let full = l * (1.0 + GUARDRAIL_AMPLIFIER * guardrail_from_regime(1.0 + GUARDRAIL_REGIME_SPAN));
        assert!((full - l * (1.0 + GUARDRAIL_AMPLIFIER)).abs() < 1e-12, "full collapse caps at +AMP");
        assert!(full < l * 1.20, "soft: even full guardrail collapse lifts l_sys by < 20%");
    }

    #[test]
    fn guardrail_collapse_is_live_in_compute_and_only_amplifies_the_likelihood() {
        // The coupler must be LIVE (not vestigial) and must enter ONLY the systemic
        // likelihood, never the flat prior. So with events held fixed, the sole effect
        // of a more structurally-degraded regime is to scale l_sys by exactly
        // (1 + GUARDRAIL_AMPLIFIER · guardrail) and nudge p_wwiii up with it.
        let events = vec![
            make_event_with_signals("military_escalation", 0.9, 1.0, SourceTier::Tier1),
            make_event_with_signals("nuclear_posture",     0.9, 2.0, SourceTier::Tier1),
        ];

        // neutral regime (product 1.0) → guardrail 0
        let mut neutral = BayesianRiskEngine::new(
            vec![RegimeFactor { id: "n".into(), label: "neutral".into(), multiplier: 1.0, active: true }],
            0.025, 0.08,
        );
        let s_neutral = neutral.compute(&events);

        // degraded regime (product 3.0) → guardrail 0.5 (squarely in the responsive band)
        let mut degraded = BayesianRiskEngine::new(
            vec![RegimeFactor { id: "d".into(), label: "degraded".into(), multiplier: 3.0, active: true }],
            0.025, 0.08,
        );
        let s_degraded = degraded.compute(&events);

        // both must see a real (non-zero) systemic likelihood for the ratio to mean anything
        assert!(s_neutral.likelihood_ratio > 0.0, "events must produce l_sys > 0");
        assert_eq!(s_neutral.couplers.guardrail_collapse, 0.0, "neutral regime → no collapse");

        let g = guardrail_from_regime(3.0);
        assert!((g - 0.5).abs() < 1e-12);
        assert!((s_degraded.couplers.guardrail_collapse - (g * 1e3).round() / 1e3).abs() < 1e-9);

        // the SAME theater likelihood is scaled by exactly (1 + AMP·guardrail)
        assert!(s_degraded.likelihood_ratio > s_neutral.likelihood_ratio,
            "guardrail collapse must raise l_sys — the coupler is live");
        let ratio = s_degraded.likelihood_ratio / s_neutral.likelihood_ratio;
        assert!((ratio - (1.0 + GUARDRAIL_AMPLIFIER * g)).abs() < 5e-3,
            "l_sys must scale by 1 + AMP·guardrail, got ratio = {ratio}");

        // and that lifts the headline, monotone (never lowers it)
        assert!(s_degraded.p_wwiii_annual >= s_neutral.p_wwiii_annual);
    }

    #[test]
    fn guardrail_collapse_is_named_dominant_coupler_only_when_it_outlifts_the_acute_ones() {
        // Awareness/honesty: the "dominant coupling channel" read-out is named in the theater
        // engine from only the four ACUTE couplers; the fifth, structural one (guardrail
        // collapse) is derived here from the regime multiplier and was therefore UNNAMEABLE —
        // even when it is the single largest amplifier of a live crisis. This locks the
        // Bayesian-engine overlay that fixes that, and its honesty guard rails.
        use crate::models::EventType;
        let ev = |theater: &str, domain: &str, sev: f64, actors: &[&str], gp: bool| {
            let mut e = GeopoliticalEvent::new("t".into(), "src".into(), SourceTier::Tier1, Utc::now());
            e.theater                   = Some(theater.to_string());
            e.domain_signals            = [(domain.to_string(), 0.9)].into_iter().collect();
            e.domain_tags               = vec![domain.to_string()];
            e.severity                  = sev;
            e.escalation_language_score = 0.4;
            e.actor_ids                 = actors.iter().map(|s| s.to_string()).collect();
            e.great_power_involved      = gp;
            e.event_type                = EventType::MilitaryStrike;
            e
        };
        // Maximally degraded regime → guardrail pinned at its +GUARDRAIL_AMPLIFIER ceiling,
        // the strongest the structural coupler can ever be.
        let degraded = || vec![RegimeFactor { id: "d".into(), label: "d".into(), multiplier: 10.0, active: true }];
        // A single hot theater, several modalities, NON-great-power actors (clears HOT_HEAT).
        let single = |theater: &'static str, actors: &'static [&'static str], gp: bool| {
            let mut v = Vec::new();
            for _ in 0..6 {
                v.push(ev(theater, "military_escalation",  0.9,  actors, gp));
                v.push(ev(theater, "economic_warfare",     0.9,  actors, gp));
                v.push(ev(theater, "cyber_info_ops",       0.85, actors, gp));
                v.push(ev(theater, "diplomatic_breakdown", 0.85, actors, gp));
            }
            v
        };

        // (A) Single non-GP hot theater: brink/breadth/GP/alliance all zero, so structural
        //     guardrail collapse is the ONLY thing amplifying the live l_sys → it is named.
        let mut eng = BayesianRiskEngine::new(degraded(), 0.025, 0.08);
        let sa = eng.compute(&single("us_iran", &["iran"], false));
        assert!(sa.likelihood_ratio > 0.0, "precondition: a live crisis exists");
        assert!(sa.couplers.guardrail_collapse > 0.0, "precondition: guardrails collapsed");
        assert_eq!(sa.couplers.coupling_driver, "structural guardrail collapse",
            "all acute couplers zero → structural collapse leads, got {:?}", sa.couplers.coupling_driver);

        // (B) Add great-power entanglement (US+Russia, conventional): the acute gp lift (~0.30)
        //     outranks even a maxed guardrail lift (≤ +GUARDRAIL_AMPLIFIER ≈ 0.12), so the
        //     acute coupler keeps the name — guardrail is soft/subordinate, never overrides.
        let mut eng = BayesianRiskEngine::new(degraded(), 0.025, 0.08);
        let sg = eng.compute(&single("nato_russia", &["united_states", "russia"], true));
        assert!(sg.couplers.gp_entanglement > 0.0, "precondition: great powers entangled");
        assert_eq!(sg.couplers.coupling_driver, "great-power entanglement",
            "an acute coupler outlifting guardrail must keep the name, got {:?}", sg.couplers.coupling_driver);

        // (C) Calm world (no events) + collapsed guardrails: guardrails amplify a live crisis
        //     but never manufacture risk from calm — so NOTHING is named, never guardrail.
        let mut eng = BayesianRiskEngine::new(degraded(), 0.025, 0.08);
        let sc = eng.compute(&[]);
        assert!(sc.couplers.guardrail_collapse > 0.0, "precondition: guardrails still collapsed");
        assert_eq!(sc.couplers.coupling_driver, "",
            "a calm world names no dominant coupler even with collapsed guardrails, got {:?}",
            sc.couplers.coupling_driver);
    }

    #[test]
    fn nuclear_events_elevate_probability() {
        let mut engine = minimal_engine();
        let events: Vec<_> = (0..5)
            .map(|_| make_event("nuclear_posture", 0.85, 1.0, SourceTier::Tier1))
            .collect();
        let snap = engine.compute(&events);
        assert!(snap.p_wwiii_annual > HISTORICAL_ANCHOR);
    }

    #[test]
    fn multi_domain_co_occurrence_boosts_probability() {
        let domains = ["nuclear_posture", "military_escalation", "great_power_conflict",
                       "alliance_activation", "wmd_mass_casualty"];
        let mut engine_single = minimal_engine();
        let mut engine_multi  = minimal_engine();
        let single_event = vec![make_event("nuclear_posture", 0.8, 1.0, SourceTier::Tier1)];
        let multi_events: Vec<_> = domains.iter()
            .map(|d| make_event(d, 0.8, 1.0, SourceTier::Tier1))
            .collect();
        let snap_single = engine_single.compute(&single_event);
        let snap_multi  = engine_multi.compute(&multi_events);
        assert!(snap_multi.p_wwiii_annual > snap_single.p_wwiii_annual);
    }

    #[test]
    fn probability_never_exceeds_one() {
        let all_domains = ["nuclear_posture", "military_escalation", "great_power_conflict",
                           "alliance_activation", "wmd_mass_casualty",
                           "diplomatic_breakdown", "economic_warfare", "cyber_info_ops"];
        let events: Vec<_> = all_domains.iter()
            .flat_map(|d| (0..20).map(|_| make_event(d, 1.0, 1.0, SourceTier::Tier1)))
            .collect();
        let mut engine = minimal_engine();
        let snap = engine.compute(&events);
        assert!(snap.p_wwiii_annual <= 1.0);
        assert!(snap.p_wwiii_30day  <= 1.0);
    }

    #[test]
    fn forecast_prob_ceiling_is_the_named_honesty_clamp() {
        // Honesty invariant: annual P(WWIII) must never reach near-certainty. It is
        // hard-clamped to the NAMED constant FORECAST_PROB_CEILING (epistemic
        // humility — the model has no ground truth). This locks three things that
        // were previously only a bare `.min(0.90)` literal next to stale 0.85
        // comments: (a) the constant's value + meaning, (b) that the clamp is LIVE
        // (an apex world actually reaches it, so it isn't vestigial), and (c) that
        // no world can exceed it.
        assert!((FORECAST_PROB_CEILING - 0.90).abs() < 1e-12,
            "FORECAST_PROB_CEILING is the documented 0.90 epistemic ceiling");

        // (b) The clamp is LIVE, not vestigial: drive the exact formula compute()
        // uses (sigmoid of the same log-odds prior + a saturating systemic
        // likelihood). Unclamped it would exceed the ceiling; the clamp pulls it
        // down to EXACTLY FORECAST_PROB_CEILING.
        let prior         = HISTORICAL_ANCHOR.clamp(1e-9, 0.5);
        let prior_logodds = (prior / (1.0 - prior)).ln();
        let unclamped     = sigmoid(prior_logodds + EVIDENCE_GAIN_SYS * 100.0);
        assert!(unclamped > FORECAST_PROB_CEILING,
            "a saturating likelihood must push the raw probability above the ceiling \
             (else the clamp is dead code), got {unclamped}");
        assert!((unclamped.min(FORECAST_PROB_CEILING) - FORECAST_PROB_CEILING).abs() < 1e-12,
            "the clamp must pull a saturating probability to exactly the ceiling");

        // (c) No real-engine world can exceed the ceiling: an apex world (every
        // domain saturated with max-strength Tier-1 signal) stays at or below it.
        let all_domains = ["nuclear_posture", "military_escalation", "great_power_conflict",
                           "alliance_activation", "wmd_mass_casualty",
                           "diplomatic_breakdown", "economic_warfare", "cyber_info_ops"];
        let events: Vec<_> = all_domains.iter()
            .flat_map(|d| (0..20).map(|_| make_event(d, 1.0, 1.0, SourceTier::Tier1)))
            .collect();
        let mut engine = minimal_engine();
        let snap = engine.compute(&events);
        assert!(snap.p_wwiii_annual <= FORECAST_PROB_CEILING,
            "p_wwiii_annual {} must never exceed the ceiling {}",
            snap.p_wwiii_annual, FORECAST_PROB_CEILING);
    }

    #[test]
    fn is_at_forecast_ceiling_agrees_with_the_clamp() {
        // The operator-facing "capped" caveat must trip on EXACTLY the clamp condition
        // (single source of truth), never on a sub-ceiling measured read — so a clamped
        // 90% can't masquerade as a measured 90% on the dashboard.
        assert!(is_at_forecast_ceiling(FORECAST_PROB_CEILING));
        assert!(is_at_forecast_ceiling(1.0));
        assert!(!is_at_forecast_ceiling(FORECAST_PROB_CEILING - 0.01));
        assert!(!is_at_forecast_ceiling(0.0));

        // It agrees with the actual clamp: drive the exact formula compute() uses with a
        // saturating likelihood (the same construction as
        // forecast_prob_ceiling_is_the_named_honesty_clamp). The unclamped value exceeds
        // the ceiling; the clamped value reads as capped — so the caveat keys off the real
        // clamp, not a vestige.
        let prior         = HISTORICAL_ANCHOR.clamp(1e-9, 0.5);
        let prior_logodds = (prior / (1.0 - prior)).ln();
        let unclamped     = sigmoid(prior_logodds + EVIDENCE_GAIN_SYS * 100.0);
        assert!(unclamped > FORECAST_PROB_CEILING, "precondition: saturating raw exceeds the ceiling");
        assert!(is_at_forecast_ceiling(unclamped.min(FORECAST_PROB_CEILING)),
            "a clamped saturating read must register as capped");

        // A real calm computed read does NOT read as capped.
        let mut engine = minimal_engine();
        let calm = engine.compute(&[make_event("diplomatic_breakdown", 0.5, 0.3, SourceTier::Tier1)]);
        assert!(!is_at_forecast_ceiling(calm.p_wwiii_annual),
            "a calm sub-ceiling read must not read as capped, got {}", calm.p_wwiii_annual);
    }

    #[test]
    fn thirty_day_less_than_annual() {
        let mut engine = minimal_engine();
        let events = vec![make_event("nuclear_posture", 0.7, 1.0, SourceTier::Tier1)];
        let snap = engine.compute(&events);
        assert!(snap.p_wwiii_30day < snap.p_wwiii_annual);
    }

    #[test]
    fn horizon_windows_use_exact_day_fraction_of_the_year() {
        // The 30-/90-day fields must mean exactly what their labels say: the day fraction
        // of the SAME 365-day year the annual read uses, under constant hazard
        // P(window) = 1 − (1 − P_annual)^(days/365). Locks against the old 1/12 & 3/12
        // (= 30.4 / 91.25-day) convention, which mislabeled the served horizon.
        let mut engine = minimal_engine();
        let events = vec![make_event("nuclear_posture", 0.7, 1.0, SourceTier::Tier1)];
        let snap = engine.compute(&events);
        // Compare against the annual read with a tolerance well above the 1e-8 rounding of
        // p_annual (the engine converts from the unrounded raw) yet far below the ~3e-5
        // gap to the old month convention.
        const EPS: f64 = 1e-6;
        let p = snap.p_wwiii_annual;
        let day_30 = 1.0 - (1.0 - p).powf(30.0 / 365.0);
        let day_90 = 1.0 - (1.0 - p).powf(90.0 / 365.0);
        let mon_30 = 1.0 - (1.0 - p).powf(1.0 / 12.0);
        assert!((snap.p_wwiii_30day - day_30).abs() < EPS,
            "30-day must be the 30/365 horizon of the annual read");
        assert!((snap.p_wwiii_90day - day_90).abs() < EPS,
            "90-day must be the 90/365 horizon of the annual read");
        // Regression guard: it must NOT match the old 1/12-year (30.4-day) convention.
        assert!((snap.p_wwiii_30day - mon_30).abs() > EPS,
            "30-day must not revert to the 1/12-year (30.4-day) convention");
    }

    #[test]
    fn first_snapshot_after_restart_reports_zero_delta_not_a_cold_start_jump() {
        // The very first compute() after a (re)start has no genuine previous snapshot.
        // Differencing the cold-start seed (prev_annual = HISTORICAL_ANCHOR, prev_30day =
        // 0.0) would render a fabricated "▲ +N% last snap" jump on the dashboard. Feed a
        // HOT window so p_annual lands far above the anchor: the honest first delta is 0.
        let mut engine = minimal_engine();
        let events: Vec<_> = (0..10)
            .map(|_| make_event("nuclear_posture", 0.9, 1.0, SourceTier::Tier1))
            .collect();
        let first = engine.compute(&events);
        assert!(first.p_wwiii_annual > HISTORICAL_ANCHOR + 0.01,
            "test premise: the hot window must push p_annual well above the seed anchor \
             (got {})", first.p_wwiii_annual);
        assert_eq!(first.delta_annual, 0.0,
            "first snapshot must not difference the cold-start anchor seed");
        assert_eq!(first.delta_30day, 0.0,
            "first snapshot must not difference the cold-start 30-day seed");
        // Second tick (a quiet window after the hot one) IS a real inter-snapshot move.
        let second = engine.compute(&[]);
        assert_ne!(second.delta_annual, 0.0,
            "the second snapshot reports a true delta against the first");
    }

    #[test]
    fn delta_annual_computed() {
        let mut engine = minimal_engine();
        let _snap1 = engine.compute(&[]);
        let events: Vec<_> = (0..10)
            .map(|_| make_event("nuclear_posture", 0.9, 1.0, SourceTier::Tier1))
            .collect();
        let snap2 = engine.compute(&events);
        assert_ne!(snap2.delta_annual, 0.0);
    }

    #[test]
    fn snapshot_fields_fully_populated() {
        let mut engine = minimal_engine();
        let events = vec![make_event("military_escalation", 0.7, 1.0, SourceTier::Tier1)];
        let snap = engine.compute(&events);
        assert!(snap.historical_anchor > 0.0);
        assert!(snap.regime_multiplier > 0.0);
        assert_eq!(snap.events_in_window, 1);
        assert!(snap.weighted_domain_sum >= 0.0);
        assert!(snap.likelihood_ratio    >= 0.0);
        assert!(snap.elevated_domains    <= DOMAIN_WEIGHTS.len());
        assert!(snap.co_occurrence_boost >= 1.0);
        assert!(snap.estimate_confidence >= 0.0);
        assert!(snap.estimate_confidence <= 1.0);
    }

    #[test]
    fn estimate_confidence_is_a_bounded_monotone_blend_with_an_offline_floor() {
        // Weights are a partition of unity, so the blend is a true weighted mean in
        // [0,1] — the compile-time assert backs this; restate it here as a guard.
        assert!((CONF_W_DOMAIN + CONF_W_EVENTS + CONF_W_SOURCES - 1.0).abs() < 1e-12);

        // Zero events → exactly the offline floor, regardless of the (stale) domain conf.
        assert_eq!(estimate_confidence(1.0, 0, 50), CONFIDENCE_OFFLINE_FLOOR);
        assert_eq!(estimate_confidence(0.0, 0, 0), CONFIDENCE_OFFLINE_FLOOR);

        // Bounded in [0,1] across a wide grid, AND a fully-corroborated world (perfect
        // domain conf, saturating volume + breadth) reads exactly 1.0 — the weighted
        // mean of three saturated unit terms.
        for &ev in &[1usize, 5, 50, 200, 2000] {
            for &src in &[0usize, 5, 20, 100] {
                for &ac in &[0.0, 0.4, 1.0] {
                    let c = estimate_confidence(ac, ev, src);
                    assert!((0.0..=1.0).contains(&c), "conf {c} out of range");
                }
            }
        }
        assert!((estimate_confidence(1.0, 2000, 100) - 1.0).abs() < 1e-9,
            "saturated evidence should read full confidence");

        // Monotone NON-DECREASING in event volume (more corroborating events never
        // lowers data-quality), holding sources/domain-conf fixed.
        let mut prev = estimate_confidence(0.5, 1, 10);
        for &ev in &[2usize, 10, 50, 200, 1000] {
            let c = estimate_confidence(0.5, ev, 10);
            assert!(c + 1e-12 >= prev, "confidence must not fall as events rise ({ev})");
            prev = c;
        }
        // Monotone NON-DECREASING in source breadth — a wider source base never lowers it.
        let mut prevs = estimate_confidence(0.5, 50, 0);
        for &src in &[1usize, 5, 20, 100] {
            let c = estimate_confidence(0.5, 50, src);
            assert!(c + 1e-12 >= prevs, "confidence must not fall as sources rise ({src})");
            prevs = c;
        }

        // Volume term log-saturates: counts beyond CONFIDENCE_EVENT_SATURATION add
        // essentially nothing (a flood of low-grade events can't fake certainty).
        let at_sat = estimate_confidence(0.0, CONFIDENCE_EVENT_SATURATION as usize, 0);
        let way_over = estimate_confidence(0.0, CONFIDENCE_EVENT_SATURATION as usize * 50, 0);
        assert!((at_sat - CONF_W_EVENTS).abs() < 2e-3, "volume term saturates at its weight");
        assert!(way_over <= CONF_W_EVENTS + 1e-9 && way_over >= at_sat,
            "beyond saturation the volume term is capped at its weight");
    }

    #[test]
    fn is_data_blind_agrees_with_the_offline_confidence_floor() {
        // The "blind" predicate (drives the dashboard's NO-LIVE-SIGNAL warning) must be
        // EXACTLY the condition under which confidence collapses to the offline floor —
        // i.e. the read is the baseline prior, not a measurement. Locking the two
        // together stops the operator-facing warning from drifting off the model state.
        for &ev in &[0usize, 1, 5, 50, 200, 5000] {
            let blind = is_data_blind(ev);
            // Sweep the other inputs: blindness depends ONLY on event volume, and it
            // holds iff confidence is pinned at the offline floor.
            for &ac in &[0.0, 0.5, 1.0] {
                for &src in &[0usize, 3, 20, 100] {
                    let at_floor = (estimate_confidence(ac, ev, src) - CONFIDENCE_OFFLINE_FLOOR).abs() < 1e-12;
                    assert_eq!(blind, ev == 0, "blindness is the zero-event state ({ev})");
                    if blind {
                        assert!(at_floor, "a blind read must sit at the offline floor (ev={ev})");
                    }
                }
            }
        }
        // A non-blind read with one corroborating event already lifts above the floor.
        assert!(estimate_confidence(0.5, 1, 1) > CONFIDENCE_OFFLINE_FLOOR);
    }

    #[test]
    fn is_thinly_sourced_is_a_narrow_base_distinct_from_blindness() {
        // The thin-coverage state is the partial-outage sibling of blindness: live events
        // exist (so the read is a measurement, not the baseline) but they come from fewer
        // than MIN_CORROBORATING_SOURCES distinct feeds. It must (1) require events,
        // (2) trip iff sources are below the floor, (3) be mutually exclusive with a blind
        // read, and (4) only ever fire while the confidence breadth term is well short of
        // saturation (a thin base can't read as fully corroborated).
        assert!(MIN_CORROBORATING_SOURCES < CONFIDENCE_SOURCE_SATURATION as usize,
            "the corroboration floor must sit below breadth saturation");
        for &ev in &[0usize, 1, 5, 50, 200] {
            for &src in &[0usize, 1, 2, 3, 5, 20, 100] {
                let thin = is_thinly_sourced(ev, src);
                // (1)+(2): exactly events>0 AND a below-floor source base.
                assert_eq!(thin, ev > 0 && src < MIN_CORROBORATING_SOURCES,
                    "thin iff a live read on a below-floor source base (ev={ev}, src={src})");
                // (3): a blind read is never also "thin" (and vice versa).
                assert!(!(thin && is_data_blind(ev)),
                    "thin and blind are mutually exclusive (ev={ev}, src={src})");
                // (4): whenever thin, the breadth term of confidence is below half its weight.
                if thin {
                    let breadth = (src as f64 / CONFIDENCE_SOURCE_SATURATION).min(1.0);
                    assert!(breadth < 0.5,
                        "a thinly-sourced read must read as poorly corroborated on breadth (src={src})");
                }
            }
        }
        // At the floor (3 sources) a live read is NO LONGER thin — broadly corroborated.
        assert!(!is_thinly_sourced(50, MIN_CORROBORATING_SOURCES));
        assert!(is_thinly_sourced(50, MIN_CORROBORATING_SOURCES - 1));
    }

    #[test]
    fn hostile_sentiment_scores_higher_than_conciliatory() {
        // Identical events except sentiment tone; hostile must out-score conciliatory.
        let mut hostile_ev = make_event("military_escalation", 0.7, 1.0, SourceTier::Tier1);
        hostile_ev.sentiment_score = -1.0; // fully hostile
        let mut concil_ev = make_event("military_escalation", 0.7, 1.0, SourceTier::Tier1);
        concil_ev.sentiment_score = 1.0;   // fully conciliatory

        let mut s_hostile = DomainScorer::new();
        let mut s_concil  = DomainScorer::new();
        let hostile = s_hostile.score_all(&[hostile_ev])["military_escalation"].score;
        let concil  = s_concil.score_all(&[concil_ev])["military_escalation"].score;
        assert!(hostile > concil,
            "hostile sentiment ({hostile:.4}) should exceed conciliatory ({concil:.4})");
    }

    // ── Co-occurrence boosts ──────────────────────────────────────────────────

    #[test]
    fn co_occurrence_boost_values() {
        // v2 five-modality curve.
        assert!((co_occurrence_boost(0.0) - 1.00).abs() < 1e-9);
        assert!((co_occurrence_boost(1.0) - 1.00).abs() < 1e-9);
        assert!((co_occurrence_boost(2.0) - 1.25).abs() < 1e-9);
        assert!((co_occurrence_boost(3.0) - 1.60).abs() < 1e-9);
        assert!((co_occurrence_boost(4.0) - 2.10).abs() < 1e-9);
        assert!((co_occurrence_boost(5.0) - 2.60).abs() < 1e-9);
        // Saturates at five orthogonal modalities — no eighth domain to climb to.
        assert!((co_occurrence_boost(8.0) - 2.60).abs() < 1e-9);
        assert!((co_occurrence_boost(12.0) - 2.60).abs() < 1e-9);
    }

    #[test]
    fn co_occurrence_boost_is_continuous_and_monotonic() {
        // Fractional input interpolates linearly between anchors (2→1.25, 3→1.60).
        let mid = co_occurrence_boost(2.5);
        assert!((mid - 1.425).abs() < 1e-9, "expected 1.425 midpoint, got {mid}");
        // Monotonic non-decreasing across the whole range — no step discontinuity.
        let mut prev = co_occurrence_boost(0.0);
        let mut x = 0.0;
        while x <= 8.0 {
            let b = co_occurrence_boost(x);
            assert!(b + 1e-9 >= prev, "boost must be monotonic at x={x}");
            prev = b;
            x += 0.05;
        }
    }

    #[test]
    fn soft_elevation_weight_ramps_smoothly() {
        assert_eq!(soft_elevation_weight(0.0), 0.0);
        assert_eq!(soft_elevation_weight(ELEVATION_THRESHOLD - ELEVATION_RAMP), 0.0);
        assert_eq!(soft_elevation_weight(ELEVATION_THRESHOLD + ELEVATION_RAMP), 1.0);
        let mid = soft_elevation_weight(ELEVATION_THRESHOLD);
        assert!((mid - 0.5).abs() < 1e-9, "midpoint of ramp should be 0.5, got {mid}");
    }

    // ── Regime multiplier ─────────────────────────────────────────────────────

    #[test]
    fn regime_multiplier_inactive_factor_excluded() {
        let mut rm = RegimeMultiplier::new(vec![
            RegimeFactor { id: "a".into(), label: "A".into(), multiplier: 2.0, active: true  },
            RegimeFactor { id: "b".into(), label: "B".into(), multiplier: 3.0, active: false },
        ]);
        assert_eq!(rm.compute(), 2.0);
        rm.set_factor("b", true);
        assert_eq!(rm.compute(), 6.0);
    }
}
