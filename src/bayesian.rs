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
use crate::theater::{TheaterEngine, EVIDENCE_GAIN_SYS};

// ── Core constants ─────────────────────────────────────────────────────────────

/// Domain-specific decay half-lives in hours.
/// Fast-moving domains decay quickly; structural ones persist longer.
pub const DOMAIN_HALF_LIVES: &[(&str, f64)] = &[
    ("military_escalation",  24.0),   // KINETIC — battles; active wars are persistent
    ("nuclear_posture",      72.0),   // NUCLEAR — posture changes persist
    ("economic_warfare",     96.0),   // COERCIVE-ECONOMIC — blockades/sanctions linger
    ("cyber_info_ops",       24.0),   // CYBER/INFO — episodic
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

#[allow(dead_code)] // v2 risk uses theater heat (theater.rs has its own); kept for reference
fn max_weighted_sum() -> f64 {
    DOMAIN_WEIGHTS.iter().map(|(_, w)| w).sum()
}

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

/// Smooth 0..1 elevation weight for a single domain score.
fn soft_elevation_weight(score: f64) -> f64 {
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

/// Evidence gain (β) for the log-odds risk model. Controls how strongly the
/// likelihood term `L` (weighted domain sum × co-occurrence boost, range ≈0–7)
/// moves the probability above the regime-adjusted prior.
///
///   P = sigmoid( logit(prior) + β·L )
///
/// At L = 0 the output equals the prior exactly (calibrated quiet baseline). As
/// L grows the output rises along a logistic S-curve and saturates toward the
/// engineering ceiling — so, unlike the old `prior × (1 + L·k)` form, strong
/// multi-domain signals can express genuinely high risk instead of being capped
/// near 15%. β is the master sensitivity knob; the alert thresholds in
/// settings.yml are tuned jointly with it. Indicative behaviour at β = 2.0
/// (regime ≈ 1.5): L≈1.1 → ~1.5% (elevated), L≈1.8 → ~5% (critical),
/// L≈2.6 → ~22%, L≈5 → ceiling. Precise crisis calibration (Cuba/Ukraine)
/// requires backtesting against historical event replays.
#[allow(dead_code)] // superseded by theater::EVIDENCE_GAIN_SYS in v2 (Phase 2); kept for reference
const EVIDENCE_GAIN: f64 = 2.0;

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
    let age_hours = (Utc::now() - *published_at).num_seconds() as f64 / 3600.0;
    if age_hours > MAX_EVENT_AGE_HOURS {
        return 0.0;
    }
    let age_hours = age_hours.max(0.0); // future-dated → treat as "just now", cap weight at 1.0
    let half_life = domain_half_life(domain);
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
                let rw = recency_weight(&event.published_at, &domain);
                if rw < 0.01 { continue; }

                // Corroboration factor: each additional confirmed source adds
                // credibility beyond the base tier weight. Capped at 1.0.
                let corroboration_factor = (event.corroboration_count as f64 * 0.05)
                    .min(0.25); // max +0.25 from corroboration (5+ sources)
                let effective_credibility =
                    (event.credibility_weight + corroboration_factor).min(1.0);
                let effective_weight = rw * effective_credibility;

                // Great-power involvement is the great_power_conflict domain's OWN
                // signal — applied only here, not to every domain a GP event touches.
                // Previously the bonus leaked into all tagged domains, making e.g.
                // diplomatic_breakdown track great_power_conflict. Cross-domain GP
                // amplification is already handled by the regime multiplier and the
                // co-occurrence boost.
                let gp_bonus = if domain == "great_power_conflict" && event.great_power_involved {
                    0.12
                } else {
                    0.0
                };

                // Domain-specific evidence (nlp_signal) is the SPINE. severity and
                // escalation are event-level — identical for every domain tagged on
                // the same story — so as an additive pedestal they compressed
                // co-tagged domains (nuclear/diplomatic/economic/great-power) to
                // near-identical scores. Here they MULTIPLY the domain's own evidence
                // instead: story intensity scales each domain by how strong THAT
                // domain's keyword/LLM evidence is, so two domains on the same severe
                // story diverge in proportion to their own signal rather than sharing
                // a common floor. A 0.55 floor keeps a strong-keyword/low-intensity
                // domain from collapsing; the 0.45 swing lets intensity matter.
                // gp_bonus stays additive and great-power-domain only. Final clamp
                // bounds the result to [0,1].
                let intensity = 0.5 * event.severity
                              + 0.5 * event.escalation_language_score; // [0,1] shared story intensity
                let base_signal = nlp_signal * (0.55 + 0.45 * intensity) + gp_bonus;

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

        // Ensure all 8 domains are always present (zeroed if no events)
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
///   P_risk = sigmoid( logit(P₀_adj) + β × L )   clamped to [0, FORECAST_PROB_CEILING]
///
/// where:
///   P₀_adj = BASELINE_ANNUAL × regime_multiplier
///           = (modern quiet-year baseline) × product(active_regime_factors)
///   L      = weighted_domain_sum / max_weighted_sum × co_occurrence_boost
///   β      = EVIDENCE_GAIN
///
/// NOTE — Mathematical character of this formula:
///   This is NOT a formal Bayesian update P(H|E) = P(E|H)P(H)/P(E). It is a
///   calibrated risk index that combines a regime-adjusted prior with the
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
            theater_engine: TheaterEngine::new(),
        }
    }

    pub fn compute(&mut self, events: &[GeopoliticalEvent]) -> RiskSnapshot {
        let mut snap = RiskSnapshot::default();
        snap.historical_anchor = HISTORICAL_ANCHOR;

        // ── Step 1: Regime-adjusted prior ──
        snap.regime_multiplier = self.regime.compute();
        snap.adjusted_prior    = HISTORICAL_ANCHOR * snap.regime_multiplier;

        // ── Step 2: Actor tracking ──
        self.actor_tracker.update(events);

        // ── Step 3: Domain scores ──
        snap.domain_scores    = self.domain_scorer.score_all(events);
        snap.events_in_window = events.len();

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
        // explicit couplers. Normalise it to 0..1 for display and as a soft amplifier.
        let guardrail = ((snap.regime_multiplier - 1.0) / 4.0).clamp(0.0, 1.0);
        tout.couplers.guardrail_collapse = (guardrail * 1e3).round() / 1e3;
        let l_sys = tout.l_sys * (1.0 + 0.12 * guardrail);
        snap.theaters         = tout.theaters;
        snap.couplers         = tout.couplers;
        snap.systemic_index   = tout.systemic_index;
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

        snap.p_wwiii_30day  = ((1.0 - (1.0 - raw).powf(1.0 / 12.0)) * 1e8).round() / 1e8;
        snap.p_wwiii_90day  = ((1.0 - (1.0 - raw).powf(3.0 / 12.0)) * 1e8).round() / 1e8;

        // ── Step 8: Delta ──
        snap.delta_annual = ((snap.p_wwiii_annual - self.prev_annual) * 1e8).round() / 1e8;
        snap.delta_30day  = ((snap.p_wwiii_30day  - self.prev_30day)  * 1e8).round() / 1e8;
        self.prev_annual  = snap.p_wwiii_annual;
        self.prev_30day   = snap.p_wwiii_30day;

        // ── Step 9: Confidence ──
        if snap.events_in_window == 0 {
            snap.estimate_confidence = 0.05;
            warn!("No events in window — model running on regime prior only (offline?)");
        } else {
            let domain_confs: Vec<f64> = snap.domain_scores.values()
                .filter(|ds| ds.event_count > 0)
                .map(|ds| ds.confidence)
                .collect();
            let avg_conf = if domain_confs.is_empty() {
                0.1
            } else {
                domain_confs.iter().sum::<f64>() / domain_confs.len() as f64
            };
            let event_factor = ((1.0 + snap.events_in_window as f64).ln()
                / (1.0 + 200.0_f64).ln()).min(1.0);
            let source_factor = (snap.sources_active as f64 / 20.0).min(1.0);
            snap.estimate_confidence =
                ((avg_conf * 0.5 + event_factor * 0.3 + source_factor * 0.2) * 1e3).round() / 1e3;
        }

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
    fn h24_event_weight_near_half() {
        let pub_at = Utc::now() - Duration::hours(24);
        let w = recency_weight(&pub_at, "military_escalation");
        assert!(w > 0.45 && w < 0.55);
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
        let pub_at = Utc::now() - Duration::hours(73);
        let w = recency_weight(&pub_at, "military_escalation");
        assert!(w > 0.0 && w < 0.5);
    }

    #[test]
    fn nuclear_domain_decays_slower_than_military() {
        let pub_at = Utc::now() - Duration::hours(48);
        let w_mil = recency_weight(&pub_at, "military_escalation");
        let w_nuc = recency_weight(&pub_at, "nuclear_posture");
        assert!(w_nuc > w_mil);
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
    fn actor_tracker_drops_military_actors_at_82h() {
        // Military event at 82h. The 0.1 threshold is crossed at:
        //   h = 24 × ln(10) / ln(2) ≈ 79.73h
        // At 82h: recency_weight("military_escalation", 82h) = exp(-ln2 × 82/24) ≈ 0.0975 < 0.1.
        // At 78h: recency_weight = exp(-ln2 × 78/24) ≈ 0.1047 > 0.1 (not yet dropped).
        // 82h is safely past the threshold in both directions.
        let mut tracker = ActorTracker::default();
        let mut event = make_event_with_signals("military_escalation", 0.9, 82.0, SourceTier::Tier1);
        event.actor_ids = vec!["russia_military".into()];
        tracker.update(&[event]);
        assert!(!tracker.counts.contains_key("russia_military"),
            "Military actor at 82h should be dropped — recency_weight(military, 82h) ≈ 0.0975 < 0.1");
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
            ("military_escalation".into(), 0.5),  // 24h
            ("nuclear_posture".into(),     0.8),  // 72h — longest
        ].into_iter().collect();
        event.event_type = EventType::NuclearTest;
        let hl = event_max_half_life(&event);
        assert!((hl - 72.0).abs() < 1e-9,
            "Max half-life for military+nuclear event should be 72h (nuclear_posture), got {hl}");
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
        assert!((hl - 24.0).abs() < 1e-9,
            "Max half-life with no domains should fall back to military_escalation 24h, got {hl}");
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
    fn h80_event_still_scores() {
        // weight = exp(-ln2 * 80/24) ≈ 0.099 — small but non-zero
        let mut scorer = DomainScorer::new();
        let event = make_event("military_escalation", 0.7, 80.0, SourceTier::Tier1);
        let scores = scorer.score_all(&[event]);
        assert!(scores["military_escalation"].score > 0.0);
        assert!(scores["military_escalation"].score < 0.15);
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
    fn thirty_day_less_than_annual() {
        let mut engine = minimal_engine();
        let events = vec![make_event("nuclear_posture", 0.7, 1.0, SourceTier::Tier1)];
        let snap = engine.compute(&events);
        assert!(snap.p_wwiii_30day < snap.p_wwiii_annual);
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
        assert!(snap.adjusted_prior    > 0.0);
        assert_eq!(snap.events_in_window, 1);
        assert!(snap.weighted_domain_sum >= 0.0);
        assert!(snap.likelihood_ratio    >= 0.0);
        assert!(snap.elevated_domains    <= DOMAIN_WEIGHTS.len());
        assert!(snap.co_occurrence_boost >= 1.0);
        assert!(snap.estimate_confidence >= 0.0);
        assert!(snap.estimate_confidence <= 1.0);
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
