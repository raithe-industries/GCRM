//! Signal-convergence detection — when *distinct* signal streams align in one place
//! and time.
//!
//! World Monitor's "cross-stream correlation" flags the moments that matter most: not
//! a single feed spiking, but **several different domains lighting up together** —
//! military + economic + disaster + escalation aligning over the same ground in the
//! same window. A lone aftershock swarm is loud but one-dimensional; a swarm that
//! coincides with conflict events, military overflights and a cyber intrusion in the
//! same theatre is a developing crisis. This module detects the latter.
//!
//! ## Relationship to the other primitives
//! - [`crate::cluster`] groups located events that are near in space and time, but is
//!   **kind-blind**: a fifty-event aftershock sequence and a fifty-event multi-domain
//!   crisis both read as "one big cluster".
//! - [`crate::cii`] ranks *fixed regions* by a cross-category composite, but it is not
//!   space-time-local — it cannot tell you *that these particular events, here, now*
//!   are converging.
//!
//! Convergence sits between them: it reuses the spatial-temporal cluster as the unit of
//! "co-located in space and time", then keeps only the clusters that span **≥
//! `min_domains` distinct [`SignalDomain`]s**, and scores each by **breadth** (how many
//! distinct streams) blended with **intensity** (how hot each stream is). The result is
//! the short list of "multiple streams are converging *here*" alerts a watch floor wants.
//!
//! ## Caveat (honest about the data)
//! Convergence is spatial, so it inherits [`cluster`]'s contract: geo-less events are
//! excluded (they cannot be co-located). In practice this catches military / disaster /
//! movement convergence well; purely non-geographic streams (many market or headline
//! feeds) are better related by [`crate::cii`]. Geo-less events are tallied separately so
//! the caller can see what was set aside.
//!
//! Everything is pure: a slice of events in, derived structs out, no I/O.

use crate::cluster::{cluster, ClusterParams};
use chrono::{DateTime, Duration, Utc};
use ee_core::{Event, EventKind, Geo};
use serde::Serialize;

/// A coarse "stream" grouping over [`EventKind`] — the cross-domain axis convergence is
/// measured on. Several event kinds collapse into one analytic stream (e.g. quakes,
/// wildfires and severe weather are all `Disaster`), because the signal of interest is
/// *different kinds of trouble aligning*, not two flavours of the same trouble.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalDomain {
    /// Armed conflict / kinetic activity.
    Military,
    /// Natural hazards: seismic, wildfire, severe weather.
    Disaster,
    /// Cyber intrusions / exploited vulnerabilities.
    Cyber,
    /// Markets / macro-financial moves.
    Economic,
    /// Asset movement: aircraft and vessels.
    Movement,
    /// Narrative / geopolitical escalation signals (headlines).
    Escalation,
    /// Anything not yet mapped to a first-class stream.
    Other,
}

impl SignalDomain {
    /// The full set of streams, in canonical order. Convergence breadth is measured
    /// against this taxonomy.
    pub const ALL: [SignalDomain; 7] = [
        SignalDomain::Military,
        SignalDomain::Disaster,
        SignalDomain::Cyber,
        SignalDomain::Economic,
        SignalDomain::Movement,
        SignalDomain::Escalation,
        SignalDomain::Other,
    ];

    /// Map a normalized [`EventKind`] to its analytic stream (total function).
    pub fn of(kind: EventKind) -> Self {
        match kind {
            EventKind::Conflict => SignalDomain::Military,
            EventKind::Earthquake
            | EventKind::Wildfire
            | EventKind::Volcano
            | EventKind::Weather
            | EventKind::AirQuality
            | EventKind::Health => SignalDomain::Disaster,
            EventKind::Cyber => SignalDomain::Cyber,
            EventKind::Market => SignalDomain::Economic,
            EventKind::Aircraft | EventKind::Vessel => SignalDomain::Movement,
            EventKind::News => SignalDomain::Escalation,
            EventKind::Transport | EventKind::Other => SignalDomain::Other,
        }
    }

    /// Short UI label.
    pub fn label(&self) -> &'static str {
        match self {
            SignalDomain::Military => "Military",
            SignalDomain::Disaster => "Disaster",
            SignalDomain::Cyber => "Cyber",
            SignalDomain::Economic => "Economic",
            SignalDomain::Movement => "Movement",
            SignalDomain::Escalation => "Escalation",
            SignalDomain::Other => "Other",
        }
    }

    /// Canonical order index, for deterministic tie-breaking.
    fn rank(&self) -> u8 {
        match self {
            SignalDomain::Military => 0,
            SignalDomain::Disaster => 1,
            SignalDomain::Cyber => 2,
            SignalDomain::Economic => 3,
            SignalDomain::Movement => 4,
            SignalDomain::Escalation => 5,
            SignalDomain::Other => 6,
        }
    }
}

/// Tunables for [`convergence`].
#[derive(Debug, Clone, Copy)]
pub struct ConvergenceParams {
    /// Maximum great-circle distance (km) for two events to be co-located.
    pub radius_km: f64,
    /// Maximum time gap for two events to be co-incident.
    pub window: Duration,
    /// Minimum number of **distinct** [`SignalDomain`]s a cluster must span to count as
    /// a convergence. The definitional floor is 2 (values below are clamped to 2); raise
    /// it to demand broader alignment (e.g. 3 = "at least three streams").
    pub min_domains: usize,
    /// Weight on the breadth term (distinct domains / taxonomy size). With
    /// `intensity_weight` this should sum to 1 so the score stays in `[0, 1]`.
    pub breadth_weight: f64,
    /// Weight on the intensity term (mean per-domain peak severity).
    pub intensity_weight: f64,
}

impl Default for ConvergenceParams {
    /// Theatre-scale defaults: 200 km / 12 h gathers the distinct streams of one
    /// developing situation, `min_domains = 2` is the convergence floor, and breadth and
    /// intensity weigh equally.
    fn default() -> Self {
        Self {
            radius_km: 200.0,
            window: Duration::hours(12),
            min_domains: 2,
            breadth_weight: 0.5,
            intensity_weight: 0.5,
        }
    }
}

/// One stream's contribution to a convergence.
#[derive(Debug, Clone, Serialize)]
pub struct DomainSignal {
    pub domain: SignalDomain,
    /// Number of member events in this stream.
    pub count: usize,
    /// Peak member severity in `[0, 1]` for this stream.
    pub peak: f64,
}

/// A detected convergence: a spatial-temporal cluster spanning multiple signal streams.
#[derive(Debug, Clone, Serialize)]
pub struct Convergence {
    /// Member events, earliest-first (inherited from the underlying cluster).
    pub events: Vec<Event>,
    /// Representative location (cluster centroid).
    pub centroid: Geo,
    /// Earliest member time.
    pub start: DateTime<Utc>,
    /// Latest member time.
    pub end: DateTime<Utc>,
    /// The distinct streams present, strongest-first (peak severity, then volume).
    pub domains: Vec<DomainSignal>,
    /// Composite convergence score in `[0, 1]` = breadth ⊕ intensity.
    pub score: f64,
}

impl Convergence {
    /// Number of distinct signal streams that converged.
    pub fn domain_count(&self) -> usize {
        self.domains.len()
    }

    /// Total member events across all streams.
    pub fn size(&self) -> usize {
        self.events.len()
    }

    /// Time spanned by the convergence.
    pub fn span(&self) -> Duration {
        self.end - self.start
    }

    /// The strongest contributing stream.
    pub fn dominant(&self) -> SignalDomain {
        self.domains.first().map(|d| d.domain).unwrap_or(SignalDomain::Other)
    }
}

/// The full convergence report over an event stream.
#[derive(Debug, Clone, Serialize)]
pub struct ConvergenceReport {
    /// Detected convergences, strongest-first (descending score; ties by descending
    /// domain count, then earliest start for determinism).
    pub convergences: Vec<Convergence>,
    /// Total spatial-temporal clusters examined (the population convergences are drawn
    /// from).
    pub clusters_examined: usize,
    /// Clusters that did **not** meet `min_domains` (single-stream incidents).
    pub non_convergent: usize,
    /// Events with no location (excluded from spatial clustering).
    pub geoless: usize,
}

impl ConvergenceReport {
    /// The highest-scoring convergence, if any.
    pub fn top(&self) -> Option<&Convergence> {
        self.convergences.first()
    }

    /// Number of detected convergences.
    pub fn count(&self) -> usize {
        self.convergences.len()
    }
}

/// Detect signal convergences in an event stream.
///
/// Located events are grouped into spatial-temporal clusters (single-linkage, via
/// [`crate::cluster`]); each cluster that spans at least `min_domains` distinct
/// [`SignalDomain`]s becomes a [`Convergence`], scored by breadth and intensity and
/// ranked strongest-first. Geo-less events are excluded and tallied separately.
pub fn convergence(events: &[Event], params: &ConvergenceParams) -> ConvergenceReport {
    let geoless = events.iter().filter(|e| e.geo.is_none()).count();
    let min_domains = params.min_domains.max(2);

    let clusters = cluster(
        events,
        &ClusterParams { radius_km: params.radius_km, window: params.window },
    );
    let clusters_examined = clusters.len();

    let mut non_convergent = 0usize;
    let mut convergences: Vec<Convergence> = Vec::new();
    for c in clusters {
        let domains = domain_breakdown(&c.events);
        if domains.len() < min_domains {
            non_convergent += 1;
            continue;
        }
        let score = score(&domains, params);
        convergences.push(Convergence {
            events: c.events,
            centroid: c.centroid,
            start: c.start,
            end: c.end,
            domains,
            score,
        });
    }

    convergences.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.domain_count().cmp(&a.domain_count()))
            .then(a.start.cmp(&b.start))
    });

    ConvergenceReport { convergences, clusters_examined, non_convergent, geoless }
}

/// Reduce a cluster's members to its per-stream breakdown, strongest-first.
fn domain_breakdown(events: &[Event]) -> Vec<DomainSignal> {
    let mut out: Vec<DomainSignal> = Vec::new();
    for e in events {
        let domain = SignalDomain::of(e.kind);
        let sev = e.severity.value();
        if let Some(slot) = out.iter_mut().find(|s| s.domain == domain) {
            slot.count += 1;
            slot.peak = slot.peak.max(sev);
        } else {
            out.push(DomainSignal { domain, count: 1, peak: sev });
        }
    }
    // Strongest-first: peak severity, then volume, then canonical order for determinism.
    out.sort_by(|a, b| {
        b.peak
            .partial_cmp(&a.peak)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.count.cmp(&a.count))
            .then(a.domain.rank().cmp(&b.domain.rank()))
    });
    out
}

/// Composite convergence score in `[0, 1]`: breadth (distinct streams / taxonomy size)
/// blended with intensity (mean per-stream peak severity).
fn score(domains: &[DomainSignal], params: &ConvergenceParams) -> f64 {
    if domains.is_empty() {
        return 0.0;
    }
    let distinct = domains.len() as f64;
    let breadth = (distinct / SignalDomain::ALL.len() as f64).clamp(0.0, 1.0);
    let intensity = domains.iter().map(|d| d.peak).sum::<f64>() / distinct;
    (params.breadth_weight * breadth + params.intensity_weight * intensity).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use ee_core::Severity;

    fn ev(kind: EventKind, lat: f64, lon: f64, secs: i64, sev: f64) -> Event {
        Event {
            id: format!("{kind:?}-{secs}"),
            source_id: "test".into(),
            kind,
            title: format!("{kind:?}"),
            time: Utc.timestamp_opt(1_700_000_000 + secs, 0).single().unwrap(),
            geo: Geo::new(lat, lon),
            severity: Severity::new(sev),
            url: None,
            raw: serde_json::Value::Null,
        }
    }

    #[test]
    fn domain_mapping_collapses_kinds() {
        assert_eq!(SignalDomain::of(EventKind::Conflict), SignalDomain::Military);
        assert_eq!(SignalDomain::of(EventKind::Earthquake), SignalDomain::Disaster);
        assert_eq!(SignalDomain::of(EventKind::Wildfire), SignalDomain::Disaster);
        assert_eq!(SignalDomain::of(EventKind::Weather), SignalDomain::Disaster);
        assert_eq!(SignalDomain::of(EventKind::Cyber), SignalDomain::Cyber);
        assert_eq!(SignalDomain::of(EventKind::Market), SignalDomain::Economic);
        assert_eq!(SignalDomain::of(EventKind::Aircraft), SignalDomain::Movement);
        assert_eq!(SignalDomain::of(EventKind::Vessel), SignalDomain::Movement);
        assert_eq!(SignalDomain::of(EventKind::News), SignalDomain::Escalation);
        assert_eq!(SignalDomain::of(EventKind::Other), SignalDomain::Other);
        assert_eq!(SignalDomain::Military.label(), "Military");
    }

    #[test]
    fn multi_domain_cluster_converges_single_domain_swarm_does_not() {
        // One theatre: conflict + a quake + a military overflight (3 streams).
        // Elsewhere: a pure aftershock swarm (loud but one stream).
        let events = vec![
            ev(EventKind::Conflict, 50.0, 30.0, 0, 0.7),
            ev(EventKind::Earthquake, 50.1, 30.1, 600, 0.6),
            ev(EventKind::Aircraft, 49.9, 29.9, 1200, 0.5),
            // Aftershock swarm far away (Japan), all Disaster.
            ev(EventKind::Earthquake, 35.0, 139.0, 0, 0.9),
            ev(EventKind::Earthquake, 35.05, 139.05, 300, 0.8),
            ev(EventKind::Earthquake, 34.95, 138.95, 600, 0.7),
        ];
        let report = convergence(&events, &ConvergenceParams::default());
        // Two clusters examined; only the multi-domain one converges.
        assert_eq!(report.clusters_examined, 2);
        assert_eq!(report.count(), 1);
        assert_eq!(report.non_convergent, 1);
        let c = report.top().unwrap();
        assert_eq!(c.domain_count(), 3);
        assert_eq!(c.size(), 3);
        // Strongest stream first: Military (0.7) leads.
        assert_eq!(c.dominant(), SignalDomain::Military);
    }

    #[test]
    fn breadth_outranks_at_equal_intensity() {
        // Cluster A: 4 distinct streams, all peak 0.6.
        // Cluster B (far away): 2 distinct streams, all peak 0.6.
        let events = vec![
            ev(EventKind::Conflict, 0.0, 0.0, 0, 0.6),
            ev(EventKind::Earthquake, 0.1, 0.1, 60, 0.6),
            ev(EventKind::Cyber, 0.05, 0.05, 120, 0.6),
            ev(EventKind::Aircraft, 0.0, 0.1, 180, 0.6),
            // Far cluster, two streams.
            ev(EventKind::Conflict, 40.0, 40.0, 0, 0.6),
            ev(EventKind::Market, 40.1, 40.1, 60, 0.6),
        ];
        let report = convergence(&events, &ConvergenceParams::default());
        assert_eq!(report.count(), 2);
        let top = report.top().unwrap();
        assert_eq!(top.domain_count(), 4);
        // Same intensity, more breadth -> higher score.
        assert!(top.score > report.convergences[1].score);
    }

    #[test]
    fn intensity_breaks_ties_at_equal_breadth() {
        // Two 2-stream clusters; one is hotter.
        let events = vec![
            ev(EventKind::Conflict, 0.0, 0.0, 0, 0.9),
            ev(EventKind::Cyber, 0.1, 0.1, 60, 0.9),
            ev(EventKind::Conflict, 40.0, 40.0, 0, 0.4),
            ev(EventKind::Cyber, 40.1, 40.1, 60, 0.4),
        ];
        let report = convergence(&events, &ConvergenceParams::default());
        assert_eq!(report.count(), 2);
        let hot = report.top().unwrap();
        assert!((hot.domains[0].peak - 0.9).abs() < 1e-9);
        assert!(hot.score > report.convergences[1].score);
    }

    #[test]
    fn spatial_separation_prevents_convergence() {
        // Conflict and a quake of different streams but far apart -> two single-domain
        // clusters, no convergence.
        let events = vec![
            ev(EventKind::Conflict, 0.0, 0.0, 0, 0.7),
            ev(EventKind::Earthquake, 50.0, 50.0, 600, 0.7),
        ];
        let report = convergence(&events, &ConvergenceParams::default());
        assert_eq!(report.clusters_examined, 2);
        assert_eq!(report.count(), 0);
        assert_eq!(report.non_convergent, 2);
    }

    #[test]
    fn temporal_separation_prevents_convergence() {
        // Same place, two different streams, but a week apart -> not co-incident.
        let events = vec![
            ev(EventKind::Conflict, 10.0, 10.0, 0, 0.7),
            ev(EventKind::Cyber, 10.0, 10.0, 7 * 24 * 3600, 0.7),
        ];
        let report = convergence(&events, &ConvergenceParams::default());
        assert_eq!(report.count(), 0);
    }

    #[test]
    fn min_domains_threshold_gates() {
        // A 2-stream cluster: detected at the default floor, gated when demanding 3.
        let events = vec![
            ev(EventKind::Conflict, 0.0, 0.0, 0, 0.7),
            ev(EventKind::Cyber, 0.1, 0.1, 60, 0.7),
        ];
        let lax = convergence(&events, &ConvergenceParams::default());
        assert_eq!(lax.count(), 1);

        let strict = convergence(
            &events,
            &ConvergenceParams { min_domains: 3, ..ConvergenceParams::default() },
        );
        assert_eq!(strict.count(), 0);
        assert_eq!(strict.non_convergent, 1);
    }

    #[test]
    fn geoless_events_excluded_and_tallied() {
        let mut headline = ev(EventKind::News, 0.0, 0.0, 0, 0.5);
        headline.geo = None;
        let mut market = ev(EventKind::Market, 0.0, 0.0, 60, 0.5);
        market.geo = None;
        let events = vec![
            headline,
            market,
            ev(EventKind::Conflict, 0.0, 0.0, 0, 0.7),
            ev(EventKind::Earthquake, 0.05, 0.05, 60, 0.7),
        ];
        let report = convergence(&events, &ConvergenceParams::default());
        assert_eq!(report.geoless, 2);
        // Only the two located events form a (2-stream) convergence.
        assert_eq!(report.count(), 1);
        assert_eq!(report.top().unwrap().size(), 2);
    }

    #[test]
    fn score_stays_in_unit_range() {
        // All seven streams, all saturated, co-located -> score <= 1.
        let mut events = Vec::new();
        for (i, k) in [
            EventKind::Conflict,
            EventKind::Earthquake,
            EventKind::Cyber,
            EventKind::Market,
            EventKind::Aircraft,
            EventKind::News,
            EventKind::Other,
        ]
        .into_iter()
        .enumerate()
        {
            events.push(ev(k, 0.0, 0.0, i as i64 * 60, 1.0));
        }
        let report = convergence(&events, &ConvergenceParams::default());
        let c = report.top().unwrap();
        assert_eq!(c.domain_count(), 7);
        assert!(c.score <= 1.0 + 1e-9);
        assert!(c.score > 0.9, "full-spectrum saturation should be near 1, got {}", c.score);
    }

    #[test]
    fn empty_input_yields_empty_report() {
        let report = convergence(&[], &ConvergenceParams::default());
        assert_eq!(report.count(), 0);
        assert_eq!(report.clusters_examined, 0);
        assert_eq!(report.geoless, 0);
    }
}
