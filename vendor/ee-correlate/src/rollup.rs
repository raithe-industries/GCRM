//! Per-region severity rollup.
//!
//! Clustering finds incidents *wherever* they happen; a rollup answers the other
//! dashboard question — "how hot is each place I care about, right now?". Given a set
//! of caller-defined [`Region`]s (labelled bounding boxes) and a stream of events, it
//! buckets each located event into every region that contains it and reduces each
//! bucket to a single comparable line: event count, a per-kind breakdown, peak and
//! mean severity, and a composite **score** in `[0, 1]`.
//!
//! The score blends three normalized terms with caller-tunable weights:
//! - **peak severity** — the single worst event in the region (acute intensity),
//! - **mean severity** — the sustained level across the region's events,
//! - **volume** — a saturating function of event count, `1 - exp(-count/scale)`, so a
//!   region with many events outranks one with a single equal-severity event without
//!   letting raw count dominate.
//!
//! With the default weights (`0.5 / 0.2 / 0.3`, summing to 1) the score stays in
//! `[0, 1]`, directly comparable to the project's `[0, 1]` [`ee_core::Severity`]
//! convention. Regions are reported **worst-first**. Every configured region appears
//! in the report — even quiet ones (score 0) — so a region panel stays stable across
//! refreshes (the same philosophy as the freshness monitor: reported == configured).
//!
//! Regions may overlap (e.g. "Europe" and "Eastern Europe"); an event in the overlap
//! counts toward both. Geo-less events cannot be placed and are tallied separately, as
//! are located events that fall in no configured region. Everything here is pure: it
//! takes slices and returns derived structures, with no I/O.

use ee_core::{Event, EventKind, Region};
use serde::Serialize;

/// Tunables for [`rollup`]. The three weights need not sum to 1, but the default set
/// does, which keeps [`RegionRollup::score`] within `[0, 1]`.
#[derive(Debug, Clone, Copy)]
pub struct RollupParams {
    /// Weight on the region's peak (max) severity.
    pub peak_weight: f64,
    /// Weight on the region's mean severity.
    pub mean_weight: f64,
    /// Weight on the (saturating) event-volume term.
    pub volume_weight: f64,
    /// Event count at which the volume term reaches ~63% of its maximum. Larger values
    /// make volume matter more gradually.
    pub volume_scale: f64,
}

impl Default for RollupParams {
    /// Peak-led, with a meaningful sustained-level and volume contribution; volume
    /// saturates around ~10 events. Weights sum to 1, so the score lands in `[0, 1]`.
    fn default() -> Self {
        Self { peak_weight: 0.5, mean_weight: 0.2, volume_weight: 0.3, volume_scale: 10.0 }
    }
}

/// The reduced state of one region.
#[derive(Debug, Clone, Serialize)]
pub struct RegionRollup {
    /// The region's name (from [`Region::name`]).
    pub region: String,
    /// Number of located events that fell inside the region.
    pub count: usize,
    /// Highest member severity in `[0, 1]` (0 when the region is empty).
    pub peak_severity: f64,
    /// Mean member severity in `[0, 1]` (0 when the region is empty).
    pub mean_severity: f64,
    /// Most common [`EventKind`] in the region (ties broken by first seen); `None`
    /// when the region is empty.
    pub dominant_kind: Option<EventKind>,
    /// Per-kind counts, ordered most-frequent-first (ties by first seen).
    pub kind_counts: Vec<(EventKind, usize)>,
    /// Composite risk score in `[0, 1]` under the active [`RollupParams`].
    pub score: f64,
}

/// The full rollup over a set of regions, plus the events that could not be placed.
#[derive(Debug, Clone, Serialize)]
pub struct RollupReport {
    /// One entry per configured region, ordered worst-first (descending score; ties
    /// broken by descending count, then region name for determinism).
    pub regions: Vec<RegionRollup>,
    /// Located events that fell inside no configured region.
    pub unassigned: usize,
    /// Events with no location (cannot be placed in any region).
    pub geoless: usize,
}

impl RollupReport {
    /// The highest-scoring region, if any region is configured.
    pub fn worst(&self) -> Option<&RegionRollup> {
        self.regions.first()
    }

    /// Total located events placed into at least one region. Note: an event in an
    /// overlap of K regions is counted K times across [`RollupReport::regions`], so
    /// this is *not* simply the sum of per-region counts.
    pub fn placed(&self) -> usize {
        self.regions.iter().map(|r| r.count).sum::<usize>()
    }
}

/// Roll up located events into the given regions and rank them worst-first.
///
/// Each located event is added to **every** region whose bounding box contains it, so
/// overlapping regions are handled correctly. Geo-less events and located events
/// outside all regions are tallied in [`RollupReport::geoless`] /
/// [`RollupReport::unassigned`] respectively.
pub fn rollup(regions: &[Region], events: &[Event], params: &RollupParams) -> RollupReport {
    // Collect the member events for each region (by index, parallel to `regions`).
    let mut buckets: Vec<Vec<&Event>> = vec![Vec::new(); regions.len()];
    let mut unassigned = 0usize;
    let mut geoless = 0usize;

    for e in events {
        let Some(g) = e.geo else {
            geoless += 1;
            continue;
        };
        let mut matched = false;
        for (i, region) in regions.iter().enumerate() {
            if region.contains(&g) {
                buckets[i].push(e);
                matched = true;
            }
        }
        if !matched {
            unassigned += 1;
        }
    }

    let mut rolled: Vec<RegionRollup> = regions
        .iter()
        .zip(buckets)
        .map(|(region, members)| build_rollup(region.name.clone(), &members, params))
        .collect();

    // Worst-first; break ties by count then name so output is fully deterministic.
    rolled.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.count.cmp(&a.count))
            .then(a.region.cmp(&b.region))
    });

    RollupReport { regions: rolled, unassigned, geoless }
}

/// Reduce one region's member events to a [`RegionRollup`].
fn build_rollup(region: String, members: &[&Event], params: &RollupParams) -> RegionRollup {
    let count = members.len();
    if count == 0 {
        return RegionRollup {
            region,
            count: 0,
            peak_severity: 0.0,
            mean_severity: 0.0,
            dominant_kind: None,
            kind_counts: Vec::new(),
            score: 0.0,
        };
    }

    let peak_severity = members.iter().map(|e| e.severity.value()).fold(0.0_f64, f64::max);
    let mean_severity =
        members.iter().map(|e| e.severity.value()).sum::<f64>() / count as f64;
    let kind_counts = kind_counts(members);
    let dominant_kind = kind_counts.first().map(|(k, _)| *k);

    // Saturating volume term in [0, 1): one event ~0, many events -> 1.
    let volume = if params.volume_scale > 0.0 {
        1.0 - (-(count as f64) / params.volume_scale).exp()
    } else {
        1.0
    };
    let score = params.peak_weight * peak_severity
        + params.mean_weight * mean_severity
        + params.volume_weight * volume;

    RegionRollup {
        region,
        count,
        peak_severity,
        mean_severity,
        dominant_kind,
        kind_counts,
        score,
    }
}

/// Per-kind counts ordered most-frequent-first; ties broken by first appearance.
fn kind_counts(members: &[&Event]) -> Vec<(EventKind, usize)> {
    let mut counts: Vec<(EventKind, usize)> = Vec::new();
    for e in members {
        if let Some(slot) = counts.iter_mut().find(|(k, _)| *k == e.kind) {
            slot.1 += 1;
        } else {
            counts.push((e.kind, 1));
        }
    }
    // Stable sort on descending count preserves first-seen order within ties.
    counts.sort_by(|a, b| b.1.cmp(&a.1));
    counts
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use ee_core::{BBox, Geo, Severity};

    fn ev(id: &str, kind: EventKind, lat: f64, lon: f64, sev: f64) -> Event {
        Event {
            id: id.into(),
            source_id: "test".into(),
            kind,
            title: id.into(),
            time: Utc.timestamp_opt(1_700_000_000, 0).single().unwrap(),
            geo: Geo::new(lat, lon),
            severity: Severity::new(sev),
            url: None,
            raw: serde_json::Value::Null,
        }
    }

    fn region(name: &str, min_lat: f64, min_lon: f64, max_lat: f64, max_lon: f64) -> Region {
        Region { name: name.into(), bbox: BBox { min_lat, min_lon, max_lat, max_lon } }
    }

    #[test]
    fn buckets_events_into_regions_and_ranks_worst_first() {
        let regions = vec![
            region("Japan", 30.0, 129.0, 46.0, 146.0),
            region("California", 32.0, -125.0, 42.0, -114.0),
        ];
        let events = vec![
            // California: two quakes, peak 0.6.
            ev("c1", EventKind::Earthquake, 38.0, -122.0, 0.3),
            ev("c2", EventKind::Earthquake, 36.0, -120.0, 0.6),
            // Japan: one strong quake, peak 0.9.
            ev("j1", EventKind::Earthquake, 35.6, 139.7, 0.9),
        ];
        let report = rollup(&regions, &events, &RollupParams::default());
        assert_eq!(report.regions.len(), 2);
        assert_eq!(report.unassigned, 0);
        assert_eq!(report.geoless, 0);
        assert_eq!(report.placed(), 3);

        // Japan's single 0.9 outscores California's two events (peak 0.6) -> worst.
        let worst = report.worst().unwrap();
        assert_eq!(worst.region, "Japan");
        assert!((worst.peak_severity - 0.9).abs() < 1e-9);
        assert_eq!(worst.count, 1);

        let ca = &report.regions[1];
        assert_eq!(ca.region, "California");
        assert_eq!(ca.count, 2);
        assert!((ca.peak_severity - 0.6).abs() < 1e-9);
        assert!((ca.mean_severity - 0.45).abs() < 1e-9);
    }

    #[test]
    fn counts_geoless_and_unassigned_separately() {
        let regions = vec![region("Box", 0.0, 0.0, 10.0, 10.0)];
        let mut headline = ev("n", EventKind::News, 5.0, 5.0, 0.2);
        headline.geo = None;
        let events = vec![
            ev("inside", EventKind::Wildfire, 5.0, 5.0, 0.4),
            ev("outside", EventKind::Wildfire, 50.0, 50.0, 0.4),
            headline,
        ];
        let report = rollup(&regions, &events, &RollupParams::default());
        assert_eq!(report.regions[0].count, 1);
        assert_eq!(report.unassigned, 1);
        assert_eq!(report.geoless, 1);
    }

    #[test]
    fn overlapping_regions_both_count_the_event() {
        let regions = vec![
            region("Wide", 0.0, 0.0, 20.0, 20.0),
            region("Narrow", 4.0, 4.0, 6.0, 6.0),
        ];
        let events = vec![ev("e", EventKind::Conflict, 5.0, 5.0, 0.5)];
        let report = rollup(&regions, &events, &RollupParams::default());
        // Event lands in both regions; total placed double-counts it.
        assert_eq!(report.placed(), 2);
        assert_eq!(report.unassigned, 0);
        for r in &report.regions {
            assert_eq!(r.count, 1);
        }
    }

    #[test]
    fn empty_region_scores_zero_and_sinks_to_bottom() {
        let regions = vec![
            region("Quiet", -80.0, -179.0, -70.0, -170.0),
            region("Busy", 0.0, 0.0, 10.0, 10.0),
        ];
        let events = vec![ev("e", EventKind::Earthquake, 5.0, 5.0, 0.5)];
        let report = rollup(&regions, &events, &RollupParams::default());
        // Busy ranks first; the quiet region is still reported, with a zero score.
        assert_eq!(report.regions[0].region, "Busy");
        let quiet = &report.regions[1];
        assert_eq!(quiet.region, "Quiet");
        assert_eq!(quiet.count, 0);
        assert_eq!(quiet.score, 0.0);
        assert!(quiet.dominant_kind.is_none());
        assert!(quiet.kind_counts.is_empty());
    }

    #[test]
    fn dominant_kind_and_breakdown() {
        let regions = vec![region("Box", 0.0, 0.0, 10.0, 10.0)];
        let events = vec![
            ev("a", EventKind::Wildfire, 1.0, 1.0, 0.3),
            ev("b", EventKind::Earthquake, 2.0, 2.0, 0.3),
            ev("c", EventKind::Wildfire, 3.0, 3.0, 0.3),
        ];
        let report = rollup(&regions, &events, &RollupParams::default());
        let r = &report.regions[0];
        assert_eq!(r.dominant_kind, Some(EventKind::Wildfire));
        // Breakdown most-frequent-first: wildfire(2) before earthquake(1).
        assert_eq!(r.kind_counts[0], (EventKind::Wildfire, 2));
        assert_eq!(r.kind_counts[1], (EventKind::Earthquake, 1));
    }

    #[test]
    fn volume_breaks_a_peak_tie() {
        // Two regions with the same peak & mean severity; the busier one scores higher.
        let regions = vec![
            region("Sparse", 0.0, 0.0, 10.0, 10.0),
            region("Dense", 20.0, 20.0, 30.0, 30.0),
        ];
        let mut events = vec![ev("s", EventKind::Earthquake, 5.0, 5.0, 0.5)];
        for i in 0..8 {
            events.push(ev(
                &format!("d{i}"),
                EventKind::Earthquake,
                25.0,
                25.0,
                0.5,
            ));
        }
        let report = rollup(&regions, &events, &RollupParams::default());
        assert_eq!(report.regions[0].region, "Dense");
        assert!(report.regions[0].score > report.regions[1].score);
    }

    #[test]
    fn no_regions_yields_empty_report_but_tallies_inputs() {
        let events = vec![ev("e", EventKind::Earthquake, 5.0, 5.0, 0.5)];
        let report = rollup(&[], &events, &RollupParams::default());
        assert!(report.regions.is_empty());
        assert!(report.worst().is_none());
        assert_eq!(report.unassigned, 1);
    }
}
