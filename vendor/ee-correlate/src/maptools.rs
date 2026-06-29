//! Map tools — the interactive query operations a situational-awareness map offers
//! once events are on it. Three pure primitives, mirroring the SitDeck map toolset
//! (capability-map: *Special modes → Map tools data: Locate / Track / Area
//! Intelligence*):
//!
//! - [`locate`] — **Locate**: nearest located events to a point, with great-circle
//!   distance and bearing, optionally capped by radius and count. The "what's around
//!   here?" click-on-the-map query.
//! - [`track`] / [`tracks`] — **Track (movement over time)**: reconstruct the ordered
//!   path of a moving entity from its successive position events, with per-leg
//!   distance / bearing / speed and a path bounding box. The aircraft- or vessel-trail
//!   the map draws when you follow one contact.
//! - [`area_intel`] — **Area Intelligence (bbox rollup)**: a focused summary of one
//!   viewport box — count, kind breakdown, severity, time span, centroid. Distinct
//!   from [`crate::rollup`], which ranks *many* named regions by composite score; this
//!   answers "what is in *this* box right now?".
//!
//! Everything here is pure: it takes a slice of events and returns derived structures,
//! with no I/O, so it is fully unit-testable offline.

use chrono::{DateTime, Utc};
use ee_core::{BBox, Event, EventKind, Geo};
use serde::Serialize;

/// Initial great-circle bearing from `from` to `to`, in degrees clockwise from north
/// (`[0, 360)`). Undefined when the points coincide; returns `0.0` there.
fn bearing_deg(from: &Geo, to: &Geo) -> f64 {
    let phi1 = from.lat.to_radians();
    let phi2 = to.lat.to_radians();
    let dlon = (to.lon - from.lon).to_radians();
    let y = dlon.sin() * phi2.cos();
    let x = phi1.cos() * phi2.sin() - phi1.sin() * phi2.cos() * dlon.cos();
    let deg = y.atan2(x).to_degrees();
    (deg + 360.0) % 360.0
}

/// Smallest [`BBox`] covering every point. `points` must be non-empty.
fn bounding_box(points: &[Geo]) -> BBox {
    let min_lat = points.iter().map(|g| g.lat).fold(f64::INFINITY, f64::min);
    let max_lat = points.iter().map(|g| g.lat).fold(f64::NEG_INFINITY, f64::max);
    let min_lon = points.iter().map(|g| g.lon).fold(f64::INFINITY, f64::min);
    let max_lon = points.iter().map(|g| g.lon).fold(f64::NEG_INFINITY, f64::max);
    // If the naive longitude span exceeds 180°, the set is better described as straddling the
    // antimeridian: recompute on the unwrapped [0,360) axis and convert back, yielding a wrap
    // box (min_lon > max_lon, understood by BBox::contains) that covers the SHORT arc instead
    // of a planet-spanning box that would falsely contain everything. Only adopt it when it is
    // actually tighter — a genuinely globe-spanning set can't be improved. (audit ee_correlate-3)
    if max_lon - min_lon > 180.0 {
        let unwrap = |lon: f64| if lon < 0.0 { lon + 360.0 } else { lon };
        let umin = points.iter().map(|g| unwrap(g.lon)).fold(f64::INFINITY, f64::min);
        let umax = points.iter().map(|g| unwrap(g.lon)).fold(f64::NEG_INFINITY, f64::max);
        if umax - umin < max_lon - min_lon {
            let rewrap = |lon: f64| if lon > 180.0 { lon - 360.0 } else { lon };
            return BBox { min_lat, min_lon: rewrap(umin), max_lat, max_lon: rewrap(umax) };
        }
    }
    BBox { min_lat, min_lon, max_lat, max_lon }
}

// --------------------------------------------------------------------------- Locate

/// Tunables for [`locate`].
#[derive(Debug, Clone, Copy)]
pub struct LocateParams {
    /// Optional maximum great-circle distance (km); `None` imposes no radius limit.
    pub radius_km: Option<f64>,
    /// Maximum number of results; `0` means unlimited.
    pub limit: usize,
}

impl Default for LocateParams {
    /// A general-purpose default: no radius cap, nearest 10.
    fn default() -> Self {
        Self { radius_km: None, limit: 10 }
    }
}

/// One event found near a query point.
#[derive(Debug, Clone, Serialize)]
pub struct Located {
    pub event: Event,
    /// Great-circle distance from the query point, km.
    pub distance_km: f64,
    /// Initial bearing from the query point to the event, degrees clockwise from north.
    pub bearing_deg: f64,
}

/// **Locate:** the located events nearest a `target`, closest-first.
///
/// Geo-less events cannot be placed and are skipped. Results within `radius_km` (when
/// set) are sorted by ascending distance — ties broken by event time, then id, for
/// determinism — and truncated to `limit`.
pub fn locate(target: Geo, events: &[Event], params: &LocateParams) -> Vec<Located> {
    let mut out: Vec<Located> = events
        .iter()
        .filter_map(|e| e.geo.map(|g| (e, g)))
        .filter_map(|(e, g)| {
            let distance_km = target.haversine_km(&g);
            if let Some(r) = params.radius_km {
                if distance_km > r {
                    return None;
                }
            }
            Some(Located {
                event: e.clone(),
                distance_km,
                bearing_deg: bearing_deg(&target, &g),
            })
        })
        .collect();

    out.sort_by(|a, b| {
        a.distance_km
            .partial_cmp(&b.distance_km)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.event.time.cmp(&b.event.time))
            .then_with(|| a.event.id.cmp(&b.event.id))
    });
    if params.limit > 0 && out.len() > params.limit {
        out.truncate(params.limit);
    }
    out
}

// ---------------------------------------------------------------------------- Track

/// One position along a reconstructed [`TrackPath`], in chronological order.
#[derive(Debug, Clone, Serialize)]
pub struct TrackPoint {
    pub event: Event,
    /// Distance from the previous point, km (`0.0` for the first point).
    pub leg_km: f64,
    /// Bearing from the previous point, degrees clockwise from north (`0.0` for the first).
    pub bearing_deg: f64,
    /// Speed over the leg, km/h (`0.0` for the first point or a zero time gap).
    pub speed_kmh: f64,
    /// Distance travelled from the start up to and including this point, km.
    pub cumulative_km: f64,
}

/// A moving entity's reconstructed path. Always has at least one point.
#[derive(Debug, Clone, Serialize)]
pub struct TrackPath {
    /// Points in chronological order.
    pub points: Vec<TrackPoint>,
    /// Sum of all leg distances, km.
    pub total_distance_km: f64,
    /// First point's time.
    pub start: DateTime<Utc>,
    /// Last point's time.
    pub end: DateTime<Utc>,
    /// Elapsed time from first to last point, seconds.
    pub duration_secs: i64,
    /// Total distance over total time, km/h (`0.0` for a zero-duration track).
    pub mean_speed_kmh: f64,
    /// Bounding box covering the path.
    pub bbox: BBox,
}

impl TrackPath {
    /// Number of points on the path.
    pub fn len(&self) -> usize {
        self.points.len()
    }
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }
}

/// **Track:** reconstruct one entity's path from its position events.
///
/// `points` are the successive sightings of a *single* entity (the caller groups them;
/// see [`tracks`] for grouping a mixed stream). Geo-less events are skipped; the rest
/// are ordered by time and reduced to per-leg distance / bearing / speed plus a path
/// summary. Returns `None` if no point is located.
pub fn track(points: &[Event]) -> Option<TrackPath> {
    let mut located: Vec<(&Event, Geo)> =
        points.iter().filter_map(|e| e.geo.map(|g| (e, g))).collect();
    if located.is_empty() {
        return None;
    }
    located.sort_by(|a, b| a.0.time.cmp(&b.0.time).then_with(|| a.0.id.cmp(&b.0.id)));

    let bbox = bounding_box(&located.iter().map(|(_, g)| *g).collect::<Vec<_>>());

    let mut track_points = Vec::with_capacity(located.len());
    let mut cumulative = 0.0;
    for (i, (e, g)) in located.iter().enumerate() {
        let (leg_km, bearing, speed_kmh) = if i == 0 {
            (0.0, 0.0, 0.0)
        } else {
            let (pe, pg) = located[i - 1];
            let leg = pg.haversine_km(g);
            let dt = (e.time - pe.time).num_seconds();
            let speed = if dt > 0 { leg / (dt as f64 / 3600.0) } else { 0.0 };
            (leg, bearing_deg(&pg, g), speed)
        };
        cumulative += leg_km;
        track_points.push(TrackPoint {
            event: (*e).clone(),
            leg_km,
            bearing_deg: bearing,
            speed_kmh,
            cumulative_km: cumulative,
        });
    }

    let start = located.first().unwrap().0.time;
    let end = located.last().unwrap().0.time;
    let duration_secs = (end - start).num_seconds();
    let mean_speed_kmh =
        if duration_secs > 0 { cumulative / (duration_secs as f64 / 3600.0) } else { 0.0 };

    Some(TrackPath {
        points: track_points,
        total_distance_km: cumulative,
        start,
        end,
        duration_secs,
        mean_speed_kmh,
        bbox,
    })
}

/// **Track (grouped):** split a mixed event stream into one [`TrackPath`] per entity.
///
/// `key` maps each event to its entity identity (e.g. an aircraft's ICAO24 or a
/// vessel's MMSI); events for which it returns `None` are ignored. Tracks with no
/// located points are dropped. Results are sorted by total distance travelled
/// (longest first), ties broken by key, for stable output.
pub fn tracks<F>(events: &[Event], key: F) -> Vec<(String, TrackPath)>
where
    F: Fn(&Event) -> Option<String>,
{
    let mut groups: std::collections::HashMap<String, Vec<Event>> = std::collections::HashMap::new();
    for e in events {
        if let Some(k) = key(e) {
            groups.entry(k).or_default().push(e.clone());
        }
    }
    let mut out: Vec<(String, TrackPath)> =
        groups.into_iter().filter_map(|(k, evs)| track(&evs).map(|t| (k, t))).collect();
    out.sort_by(|a, b| {
        b.1.total_distance_km
            .partial_cmp(&a.1.total_distance_km)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    out
}

// -------------------------------------------------------------------- Area Intelligence

/// Summary of the events inside one viewport box (output of [`area_intel`]).
#[derive(Debug, Clone, Serialize)]
pub struct AreaReport {
    /// The queried box.
    pub bbox: BBox,
    /// Number of located events inside the box.
    pub total: usize,
    /// Per-kind counts inside the box, most frequent first (ties: first-seen order).
    pub by_kind: Vec<(EventKind, usize)>,
    /// Highest severity inside the box (`0.0` when empty).
    pub peak_severity: f64,
    /// Mean severity inside the box (`0.0` when empty).
    pub mean_severity: f64,
    /// Earliest event time inside the box.
    pub earliest: Option<DateTime<Utc>>,
    /// Latest event time inside the box.
    pub latest: Option<DateTime<Utc>>,
    /// Mean coordinate of the events inside the box.
    pub centroid: Option<Geo>,
    /// Events not counted (geo-less, or located outside the box).
    pub excluded: usize,
}

/// **Area Intelligence:** summarize everything inside a bounding box.
///
/// Located events inside `bbox` are tallied; geo-less events and those outside the box
/// are counted in `excluded`. Bounds are inclusive (see [`BBox::contains`]).
pub fn area_intel(bbox: BBox, events: &[Event]) -> AreaReport {
    let mut by_kind: Vec<(EventKind, usize)> = Vec::new();
    let mut total = 0usize;
    let mut excluded = 0usize;
    let mut peak = 0.0_f64;
    let mut sev_sum = 0.0;
    let mut geos: Vec<Geo> = Vec::new();
    let mut earliest: Option<DateTime<Utc>> = None;
    let mut latest: Option<DateTime<Utc>> = None;

    for e in events {
        let inside = e.geo.map(|g| bbox.contains(&g)).unwrap_or(false);
        if !inside {
            excluded += 1;
            continue;
        }
        let g = e.geo.unwrap();
        total += 1;
        sev_sum += e.severity.value();
        peak = peak.max(e.severity.value());
        geos.push(g);
        earliest = Some(earliest.map_or(e.time, |t| t.min(e.time)));
        latest = Some(latest.map_or(e.time, |t| t.max(e.time)));
        if let Some(slot) = by_kind.iter_mut().find(|(k, _)| *k == e.kind) {
            slot.1 += 1;
        } else {
            by_kind.push((e.kind, 1));
        }
    }

    // Stable: count desc, preserving first-seen order on ties.
    by_kind.sort_by_key(|k| std::cmp::Reverse(k.1));

    let (mean_severity, centroid) = if total > 0 {
        // Circular-mean centroid — antimeridian-safe (audit ee_correlate-3).
        (sev_sum / total as f64, ee_core::geo::centroid(&geos))
    } else {
        (0.0, None)
    };

    AreaReport {
        bbox,
        total,
        by_kind,
        peak_severity: peak,
        mean_severity,
        earliest,
        latest,
        centroid,
        excluded,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use ee_core::Severity;

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
    fn bearing_cardinal_directions() {
        let o = Geo::new(0.0, 0.0).unwrap();
        assert!((bearing_deg(&o, &Geo::new(1.0, 0.0).unwrap()) - 0.0).abs() < 1e-6); // north
        assert!((bearing_deg(&o, &Geo::new(0.0, 1.0).unwrap()) - 90.0).abs() < 1e-6); // east
        assert!((bearing_deg(&o, &Geo::new(-1.0, 0.0).unwrap()) - 180.0).abs() < 1e-6); // south
        assert!((bearing_deg(&o, &Geo::new(0.0, -1.0).unwrap()) - 270.0).abs() < 1e-6); // west
    }

    #[test]
    fn locate_orders_by_distance_and_caps() {
        let here = Geo::new(40.0, -120.0).unwrap();
        let events = vec![
            ev("far", EventKind::Earthquake, 41.0, -120.0, 0, 0.5), // ~111 km north
            ev("near", EventKind::Earthquake, 40.1, -120.0, 0, 0.5), // ~11 km north
            ev("mid", EventKind::Earthquake, 40.5, -120.0, 0, 0.5), // ~55 km north
        ];
        let r = locate(here, &events, &LocateParams::default());
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].event.id, "near");
        assert_eq!(r[1].event.id, "mid");
        assert_eq!(r[2].event.id, "far");
        // All are due north of the query point.
        assert!((r[0].bearing_deg - 0.0).abs() < 1e-6);
        assert!(r[0].distance_km < r[1].distance_km);

        // Radius cap drops the far one; limit caps the count.
        let capped = locate(here, &events, &LocateParams { radius_km: Some(60.0), limit: 1 });
        assert_eq!(capped.len(), 1);
        assert_eq!(capped[0].event.id, "near");
    }

    #[test]
    fn locate_skips_geoless() {
        let mut headline = ev("news", EventKind::News, 0.0, 0.0, 0, 0.2);
        headline.geo = None;
        let located = ev("q", EventKind::Earthquake, 0.1, 0.0, 0, 0.5);
        let r = locate(Geo::new(0.0, 0.0).unwrap(), &[headline, located], &LocateParams::default());
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].event.id, "q");
    }

    #[test]
    fn track_builds_ordered_path_with_speed() {
        // An aircraft flying due east along the equator, 1° (~111.3 km) every 30 min.
        // Fed out of order to prove chronological sorting.
        let pts = vec![
            ev("p1", EventKind::Aircraft, 0.0, 1.0, 1800, 0.1),
            ev("p0", EventKind::Aircraft, 0.0, 0.0, 0, 0.1),
            ev("p2", EventKind::Aircraft, 0.0, 2.0, 3600, 0.1),
        ];
        let t = track(&pts).unwrap();
        assert_eq!(t.len(), 3);
        assert_eq!(t.points[0].event.id, "p0");
        assert_eq!(t.points[2].event.id, "p2");

        // First point has no leg.
        assert_eq!(t.points[0].leg_km, 0.0);
        // Each leg ~111.3 km, heading east (~90°).
        assert!((t.points[1].leg_km - 111.3).abs() < 1.0);
        assert!((t.points[1].bearing_deg - 90.0).abs() < 1e-6);
        // 111.3 km in 30 min -> ~222 km/h.
        assert!((t.points[1].speed_kmh - 222.6).abs() < 2.0);
        // Cumulative ~222.6 km over the whole hour.
        assert!((t.total_distance_km - 222.6).abs() < 2.0);
        assert_eq!(t.duration_secs, 3600);
        assert!((t.mean_speed_kmh - 222.6).abs() < 2.0);
        // BBox spans 0..2° longitude on the equator.
        assert!((t.bbox.min_lon - 0.0).abs() < 1e-9 && (t.bbox.max_lon - 2.0).abs() < 1e-9);
    }

    #[test]
    fn track_returns_none_without_location() {
        let mut a = ev("a", EventKind::Aircraft, 0.0, 0.0, 0, 0.1);
        let mut b = ev("b", EventKind::Aircraft, 0.0, 0.0, 60, 0.1);
        a.geo = None;
        b.geo = None;
        assert!(track(&[a, b]).is_none());
    }

    #[test]
    fn tracks_group_by_entity_and_rank() {
        // Two contacts interleaved; key on source-provided callsign in the title.
        let events = vec![
            ev("AAL1@t0", EventKind::Aircraft, 0.0, 0.0, 0, 0.1),
            ev("UAL2@t0", EventKind::Aircraft, 10.0, 10.0, 0, 0.1),
            ev("AAL1@t1", EventKind::Aircraft, 0.0, 3.0, 3600, 0.1), // moves ~333 km
            ev("UAL2@t1", EventKind::Aircraft, 10.0, 10.5, 3600, 0.1), // moves ~55 km
        ];
        let key = |e: &Event| e.id.split('@').next().map(String::from);
        let result = tracks(&events, key);
        assert_eq!(result.len(), 2);
        // Longest path first.
        assert_eq!(result[0].0, "AAL1");
        assert_eq!(result[1].0, "UAL2");
        assert!(result[0].1.total_distance_km > result[1].1.total_distance_km);
    }

    #[test]
    fn area_intel_summarizes_box() {
        let bbox = BBox { min_lat: 0.0, min_lon: 0.0, max_lat: 10.0, max_lon: 10.0 };
        let events = vec![
            ev("in1", EventKind::Wildfire, 1.0, 1.0, 0, 0.4),
            ev("in2", EventKind::Wildfire, 2.0, 2.0, 100, 0.8),
            ev("in3", EventKind::Earthquake, 3.0, 3.0, 50, 0.2),
            ev("out", EventKind::Wildfire, 50.0, 50.0, 0, 0.9), // outside
        ];
        let mut geoless = ev("g", EventKind::News, 0.0, 0.0, 0, 0.5);
        geoless.geo = None;
        let mut all = events;
        all.push(geoless);

        let r = area_intel(bbox, &all);
        assert_eq!(r.total, 3);
        assert_eq!(r.excluded, 2); // one outside + one geo-less
        // Wildfire (2) ahead of Earthquake (1).
        assert_eq!(r.by_kind[0], (EventKind::Wildfire, 2));
        assert_eq!(r.by_kind[1], (EventKind::Earthquake, 1));
        assert!((r.peak_severity - 0.8).abs() < 1e-9);
        assert!((r.mean_severity - (0.4 + 0.8 + 0.2) / 3.0).abs() < 1e-9);
        assert_eq!(r.earliest.unwrap().timestamp(), 1_700_000_000);
        assert_eq!(r.latest.unwrap().timestamp(), 1_700_000_100);
        let c = r.centroid.unwrap();
        assert!((c.lat - 2.0).abs() < 1e-9 && (c.lon - 2.0).abs() < 1e-9);
    }

    #[test]
    fn area_intel_empty_box_is_zeroed() {
        let bbox = BBox { min_lat: 80.0, min_lon: 80.0, max_lat: 81.0, max_lon: 81.0 };
        let r = area_intel(bbox, &[ev("a", EventKind::Earthquake, 0.0, 0.0, 0, 0.5)]);
        assert_eq!(r.total, 0);
        assert_eq!(r.excluded, 1);
        assert_eq!(r.peak_severity, 0.0);
        assert_eq!(r.mean_severity, 0.0);
        assert!(r.centroid.is_none());
        assert!(r.earliest.is_none());
    }
}
