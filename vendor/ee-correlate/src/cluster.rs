//! Spatial-temporal clustering of events.
//!
//! Two located events are *linked* when they are within `radius_km` of each other
//! (great-circle distance) **and** within `window` of each other in time. Clusters
//! are the connected components of that link graph (single-linkage): if A links B
//! and B links C, then A, B and C share a cluster even if A and C alone would not.
//!
//! This is the classic shape of an "incident": an earthquake mainshock plus its
//! aftershocks, a wildfire complex's many thermal detections, or a flurry of
//! conflict events in one town over a few hours. Events with no location cannot be
//! placed in space and are excluded from clustering (see [`cluster`]).

use chrono::{DateTime, Duration, Utc};
use ee_core::{Event, EventKind, Geo};
use serde::Serialize;

/// Tunables for [`cluster`].
#[derive(Debug, Clone, Copy)]
pub struct ClusterParams {
    /// Maximum great-circle distance (km) for two events to be linked.
    pub radius_km: f64,
    /// Maximum time gap for two events to be linked.
    pub window: Duration,
}

impl Default for ClusterParams {
    /// A general-purpose default: 100 km / 24 h — wide enough to gather an
    /// aftershock sequence or a regional event burst into one incident.
    fn default() -> Self {
        Self { radius_km: 100.0, window: Duration::hours(24) }
    }
}

/// A group of events that are close in space and time. Always non-empty.
#[derive(Debug, Clone, Serialize)]
pub struct Cluster {
    /// Member events, ordered earliest-first.
    pub events: Vec<Event>,
    /// Unweighted mean of member coordinates (a representative location).
    pub centroid: Geo,
    /// Earliest member time.
    pub start: DateTime<Utc>,
    /// Latest member time.
    pub end: DateTime<Utc>,
    /// Highest member severity in `[0, 1]` — the cluster's headline intensity.
    pub peak_severity: f64,
    /// The most common [`EventKind`] among members (ties broken by first seen).
    pub dominant_kind: EventKind,
}

impl Cluster {
    /// Number of member events.
    pub fn size(&self) -> usize {
        self.events.len()
    }
}

/// Group located events into spatial-temporal clusters (single-linkage).
///
/// Only events with a `geo` are clustered; geo-less events are silently skipped, so
/// callers can pass a mixed stream. Every located event lands in exactly one cluster
/// (an isolated event becomes a cluster of size 1). The returned clusters are sorted
/// largest-first, then by descending peak severity, for stable, useful output.
pub fn cluster(events: &[Event], params: &ClusterParams) -> Vec<Cluster> {
    // Keep only located events, remembering their coordinates alongside.
    let located: Vec<(&Event, Geo)> =
        events.iter().filter_map(|e| e.geo.map(|g| (e, g))).collect();
    let n = located.len();
    if n == 0 {
        return Vec::new();
    }

    // Union-find over the located events.
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]]; // path halving
            x = parent[x];
        }
        x
    }
    let window_secs = params.window.num_seconds().abs();
    for i in 0..n {
        for j in (i + 1)..n {
            let (ei, gi) = located[i];
            let (ej, gj) = located[j];
            let dt = (ei.time - ej.time).num_seconds().abs();
            if dt <= window_secs && gi.haversine_km(&gj) <= params.radius_km {
                let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
                if ri != rj {
                    parent[ri] = rj;
                }
            }
        }
    }

    // Gather members by component root.
    let mut groups: std::collections::HashMap<usize, Vec<usize>> = std::collections::HashMap::new();
    for i in 0..n {
        let r = find(&mut parent, i);
        groups.entry(r).or_default().push(i);
    }

    let mut clusters: Vec<Cluster> =
        groups.into_values().map(|idx| build_cluster(&located, &idx)).collect();

    // Largest first; break ties by peak severity, then earliest start, for determinism.
    clusters.sort_by(|a, b| {
        b.size()
            .cmp(&a.size())
            .then(b.peak_severity.partial_cmp(&a.peak_severity).unwrap_or(std::cmp::Ordering::Equal))
            .then(a.start.cmp(&b.start))
    });
    clusters
}

/// Assemble a [`Cluster`] from a component's member indices.
fn build_cluster(located: &[(&Event, Geo)], idx: &[usize]) -> Cluster {
    let mut members: Vec<Event> = idx.iter().map(|&i| located[i].0.clone()).collect();
    members.sort_by_key(|e| e.time);

    // Circular-mean centroid (antimeridian-safe — a cluster straddling 180° centres near the
    // dateline, not on the wrong hemisphere). Falls back to the first member for the degenerate
    // antipodally-balanced case; idx is non-empty here. (audit ee_correlate-3)
    let geos: Vec<Geo> = idx.iter().map(|&i| located[i].1).collect();
    let centroid = ee_core::geo::centroid(&geos).unwrap_or(located[idx[0]].1);

    let start = members.first().map(|e| e.time).unwrap();
    let end = members.last().map(|e| e.time).unwrap();
    let peak_severity = members
        .iter()
        .map(|e| e.severity.value())
        .fold(0.0_f64, f64::max);
    let dominant_kind = dominant_kind(&members);

    Cluster { events: members, centroid, start, end, peak_severity, dominant_kind }
}

/// Most frequent kind; ties broken by earliest appearance in `members`.
fn dominant_kind(members: &[Event]) -> EventKind {
    let mut counts: Vec<(EventKind, usize)> = Vec::new();
    for e in members {
        if let Some(slot) = counts.iter_mut().find(|(k, _)| *k == e.kind) {
            slot.1 += 1;
        } else {
            counts.push((e.kind, 1));
        }
    }
    // `counts` preserves first-seen order, so `max_by_key` on count yields the
    // earliest-appearing kind on a tie.
    counts.iter().max_by_key(|(_, c)| *c).map(|(k, _)| *k).unwrap_or(EventKind::Other)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use ee_core::{EventKind, Severity};

    fn ev(id: &str, kind: EventKind, lat: f64, lon: f64, secs: i64, sev: f64) -> Event {
        Event {
            id: id.into(),
            source_id: "test".into(),
            kind,
            title: id.into(),
            time: Utc.timestamp_opt(1_700_000_000 + secs, 0).single().unwrap(),
            geo: Geo::new(lat, lon),
            severity: Severity::new(sev),
            url: None,
            raw: serde_json::Value::Null,
        }
    }

    #[test]
    fn groups_near_in_space_and_time() {
        // Three quakes clustered around Petrolia within minutes (one incident),
        // plus a far-away unrelated event.
        let events = vec![
            ev("a", EventKind::Earthquake, 40.30, -124.40, 0, 0.5),
            ev("b", EventKind::Earthquake, 40.33, -124.42, 120, 0.7),
            ev("c", EventKind::Earthquake, 40.31, -124.39, 300, 0.4),
            ev("z", EventKind::Earthquake, 35.68, 139.69, 60, 0.9), // Tokyo
        ];
        let clusters = cluster(&events, &ClusterParams::default());
        assert_eq!(clusters.len(), 2);

        // Largest first: the 3-event Petrolia incident.
        let big = &clusters[0];
        assert_eq!(big.size(), 3);
        assert_eq!(big.dominant_kind, EventKind::Earthquake);
        assert!((big.peak_severity - 0.7).abs() < 1e-9);
        // Members ordered earliest-first.
        assert_eq!(big.events[0].id, "a");
        assert_eq!(big.events[2].id, "c");
        // Time span = 0..300s.
        assert_eq!((big.end - big.start).num_seconds(), 300);
        // Centroid lands near Petrolia.
        assert!((big.centroid.lat - 40.31).abs() < 0.1);

        let lone = &clusters[1];
        assert_eq!(lone.size(), 1);
        assert_eq!(lone.events[0].id, "z");
    }

    #[test]
    fn single_linkage_chains_through_intermediates() {
        // a-b within radius, b-c within radius, but a-c apart: single linkage joins all.
        let p = ClusterParams { radius_km: 60.0, window: Duration::hours(1) };
        let events = vec![
            ev("a", EventKind::Wildfire, 0.0, 0.0, 0, 0.3),
            ev("b", EventKind::Wildfire, 0.0, 0.45, 60, 0.3), // ~50 km east of a
            ev("c", EventKind::Wildfire, 0.0, 0.90, 120, 0.3), // ~50 km east of b, ~100 from a
        ];
        let clusters = cluster(&events, &p);
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].size(), 3);
    }

    #[test]
    fn time_gap_splits_clusters() {
        // Same place, but the second event is outside the time window -> two clusters.
        let p = ClusterParams { radius_km: 100.0, window: Duration::minutes(10) };
        let events = vec![
            ev("a", EventKind::Conflict, 50.0, 30.0, 0, 0.6),
            ev("b", EventKind::Conflict, 50.0, 30.0, 3600, 0.6), // +1 h
        ];
        let clusters = cluster(&events, &p);
        assert_eq!(clusters.len(), 2);
    }

    #[test]
    fn skips_geoless_events() {
        let mut headline = ev("news", EventKind::News, 0.0, 0.0, 0, 0.2);
        headline.geo = None;
        let located = ev("q", EventKind::Earthquake, 10.0, 10.0, 0, 0.5);
        let clusters = cluster(&[headline, located], &ClusterParams::default());
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].events[0].id, "q");
    }

    #[test]
    fn dominant_kind_is_the_majority() {
        let events = vec![
            ev("a", EventKind::Wildfire, 0.0, 0.0, 0, 0.3),
            ev("b", EventKind::Earthquake, 0.0, 0.05, 10, 0.3),
            ev("c", EventKind::Wildfire, 0.0, 0.02, 20, 0.3),
        ];
        let clusters = cluster(&events, &ClusterParams::default());
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].dominant_kind, EventKind::Wildfire);
    }

    #[test]
    fn empty_input_yields_no_clusters() {
        assert!(cluster(&[], &ClusterParams::default()).is_empty());
    }
}
