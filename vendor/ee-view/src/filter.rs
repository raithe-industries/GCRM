//! Event filtering — time-window, bounding-box, and kind predicates.
//!
//! Dashboards put three controls in front of every layer and widget: a *time
//! slider* (show only what happened in this window), a *map viewport* (show only
//! what falls inside this box), and *layer toggles* (show only these kinds). This
//! module is the frontend-agnostic core of those controls: a composable
//! [`EventFilter`] that any view — a map layer, a ticker, a timeline, Cinema
//! Mode's ≤1 h temporal window — can build once and apply to an event slice.
//!
//! Everything is pure: a filter is a value, and applying it neither mutates the
//! events nor touches I/O, so it is fully unit-testable offline.

use chrono::{DateTime, Duration, Utc};
use ee_core::{BBox, Event, EventKind};

/// A composable predicate over [`Event`]s combining three independent
/// constraints. An unset constraint is a no-op, so a default [`EventFilter`]
/// matches everything; constraints are ANDed together.
///
/// ```
/// use ee_view::filter::EventFilter;
/// use ee_core::EventKind;
///
/// let f = EventFilter::new()
///     .kind(EventKind::Earthquake)
///     .kind(EventKind::Wildfire);
/// // `f.matches(&event)` is now true only for quakes or wildfires.
/// ```
#[derive(Debug, Clone, Default)]
pub struct EventFilter {
    /// Inclusive lower time bound.
    since: Option<DateTime<Utc>>,
    /// Inclusive upper time bound.
    until: Option<DateTime<Utc>>,
    /// Spatial viewport; events outside it (or with no location) are excluded.
    bbox: Option<BBox>,
    /// Allowed kinds. `None` (or empty) means "any kind".
    kinds: Option<Vec<EventKind>>,
}

impl EventFilter {
    /// A filter with no constraints — matches every event.
    pub fn new() -> Self {
        Self::default()
    }

    /// Keep events at or after `t` (inclusive).
    pub fn since(mut self, t: DateTime<Utc>) -> Self {
        self.since = Some(t);
        self
    }

    /// Keep events at or before `t` (inclusive).
    pub fn until(mut self, t: DateTime<Utc>) -> Self {
        self.until = Some(t);
        self
    }

    /// Keep events whose time falls in the inclusive `[start, end]` window.
    /// Bounds are normalized so passing them out of order still works.
    pub fn window(mut self, start: DateTime<Utc>, end: DateTime<Utc>) -> Self {
        let (lo, hi) = if start <= end { (start, end) } else { (end, start) };
        self.since = Some(lo);
        self.until = Some(hi);
        self
    }

    /// Keep events from the last `span` up to `now` (e.g. Cinema Mode's ≤1 h
    /// window: `EventFilter::last(Duration::hours(1), Utc::now())`).
    pub fn last(self, span: Duration, now: DateTime<Utc>) -> Self {
        self.window(now - span, now)
    }

    /// Restrict to events located inside `bbox`. Geo-less events are excluded.
    pub fn bbox(mut self, bbox: BBox) -> Self {
        self.bbox = Some(bbox);
        self
    }

    /// Allow one more [`EventKind`] (call repeatedly to allow several).
    pub fn kind(mut self, kind: EventKind) -> Self {
        let kinds = self.kinds.get_or_insert_with(Vec::new);
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
        self
    }

    /// Allow exactly the given set of kinds (replaces any previously set).
    pub fn kinds<I: IntoIterator<Item = EventKind>>(mut self, kinds: I) -> Self {
        let mut v: Vec<EventKind> = Vec::new();
        for k in kinds {
            if !v.contains(&k) {
                v.push(k);
            }
        }
        self.kinds = Some(v);
        self
    }

    /// Does this event satisfy every active constraint?
    pub fn matches(&self, e: &Event) -> bool {
        if let Some(since) = self.since {
            if e.time < since {
                return false;
            }
        }
        if let Some(until) = self.until {
            if e.time > until {
                return false;
            }
        }
        if let Some(bbox) = self.bbox {
            // A spatial filter inherently drops locationless events.
            match e.geo {
                Some(g) if bbox.contains(&g) => {}
                _ => return false,
            }
        }
        if let Some(kinds) = &self.kinds {
            // An empty allow-list means "no kind restriction".
            if !kinds.is_empty() && !kinds.contains(&e.kind) {
                return false;
            }
        }
        true
    }

    /// Borrow the matching events from a slice, preserving input order.
    pub fn apply<'a>(&self, events: &'a [Event]) -> Vec<&'a Event> {
        events.iter().filter(|e| self.matches(e)).collect()
    }

    /// Drop in-place every event that does not match.
    pub fn retain(&self, events: &mut Vec<Event>) {
        events.retain(|e| self.matches(e));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use ee_core::{Geo, Severity};

    fn ev(id: &str, kind: EventKind, lat: f64, lon: f64, secs: i64) -> Event {
        Event {
            id: id.into(),
            source_id: "test".into(),
            kind,
            title: id.into(),
            time: Utc.timestamp_opt(1_700_000_000 + secs, 0).single().unwrap(),
            geo: Geo::new(lat, lon),
            severity: Severity::new(0.5),
            url: None,
            raw: serde_json::Value::Null,
        }
    }

    fn at(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000 + secs, 0).single().unwrap()
    }

    #[test]
    fn empty_filter_matches_everything() {
        let f = EventFilter::new();
        let events = vec![
            ev("a", EventKind::Earthquake, 10.0, 10.0, 0),
            ev("b", EventKind::Cyber, 0.0, 0.0, 100),
        ];
        assert_eq!(f.apply(&events).len(), 2);
    }

    #[test]
    fn time_window_is_inclusive() {
        let f = EventFilter::new().window(at(100), at(300));
        let events = vec![
            ev("before", EventKind::News, 0.0, 0.0, 50),
            ev("lo_edge", EventKind::News, 0.0, 0.0, 100),
            ev("inside", EventKind::News, 0.0, 0.0, 200),
            ev("hi_edge", EventKind::News, 0.0, 0.0, 300),
            ev("after", EventKind::News, 0.0, 0.0, 350),
        ];
        let got: Vec<&str> = f.apply(&events).iter().map(|e| e.id.as_str()).collect();
        assert_eq!(got, vec!["lo_edge", "inside", "hi_edge"]);
    }

    #[test]
    fn window_normalizes_reversed_bounds() {
        let f = EventFilter::new().window(at(300), at(100));
        assert!(f.matches(&ev("x", EventKind::News, 0.0, 0.0, 200)));
    }

    #[test]
    fn last_keeps_recent_relative_to_now() {
        let now = at(10_000);
        let f = EventFilter::new().last(Duration::seconds(60), now);
        assert!(f.matches(&ev("recent", EventKind::News, 0.0, 0.0, 9_950)));
        assert!(!f.matches(&ev("old", EventKind::News, 0.0, 0.0, 9_000)));
        // Future events (after `now`) are excluded too.
        assert!(!f.matches(&ev("future", EventKind::News, 0.0, 0.0, 10_500)));
    }

    #[test]
    fn bbox_keeps_inside_and_drops_geoless() {
        let bbox = BBox { min_lat: 0.0, min_lon: 0.0, max_lat: 10.0, max_lon: 10.0 };
        let f = EventFilter::new().bbox(bbox);
        assert!(f.matches(&ev("inside", EventKind::Earthquake, 5.0, 5.0, 0)));
        assert!(!f.matches(&ev("outside", EventKind::Earthquake, 50.0, 50.0, 0)));
        let mut geoless = ev("geoless", EventKind::Cyber, 5.0, 5.0, 0);
        geoless.geo = None;
        assert!(!f.matches(&geoless));
    }

    #[test]
    fn kinds_restrict_to_allowed_set() {
        let f = EventFilter::new().kind(EventKind::Earthquake).kind(EventKind::Wildfire);
        assert!(f.matches(&ev("q", EventKind::Earthquake, 0.0, 0.0, 0)));
        assert!(f.matches(&ev("fire", EventKind::Wildfire, 0.0, 0.0, 0)));
        assert!(!f.matches(&ev("cve", EventKind::Cyber, 0.0, 0.0, 0)));
    }

    #[test]
    fn kind_is_deduplicated_and_kinds_replaces() {
        // Repeated `.kind` does not double-count; `.kinds` replaces the set.
        let f = EventFilter::new()
            .kind(EventKind::Earthquake)
            .kind(EventKind::Earthquake)
            .kinds([EventKind::Vessel, EventKind::Vessel, EventKind::Aircraft]);
        assert!(!f.matches(&ev("q", EventKind::Earthquake, 0.0, 0.0, 0)));
        assert!(f.matches(&ev("ship", EventKind::Vessel, 0.0, 0.0, 0)));
        assert!(f.matches(&ev("plane", EventKind::Aircraft, 0.0, 0.0, 0)));
    }

    #[test]
    fn constraints_compose_with_and() {
        // Time AND bbox AND kind must all hold.
        let f = EventFilter::new()
            .window(at(0), at(1000))
            .bbox(BBox { min_lat: 0.0, min_lon: 0.0, max_lat: 10.0, max_lon: 10.0 })
            .kind(EventKind::Earthquake);
        // Right kind, right place, right time.
        assert!(f.matches(&ev("hit", EventKind::Earthquake, 5.0, 5.0, 500)));
        // Right kind & place, but out of the time window.
        assert!(!f.matches(&ev("late", EventKind::Earthquake, 5.0, 5.0, 5000)));
        // Right kind & time, but outside the box.
        assert!(!f.matches(&ev("away", EventKind::Earthquake, 80.0, 80.0, 500)));
        // Right place & time, but wrong kind.
        assert!(!f.matches(&ev("wrong", EventKind::Wildfire, 5.0, 5.0, 500)));
    }

    #[test]
    fn retain_mutates_in_place() {
        let mut events = vec![
            ev("keep", EventKind::Earthquake, 0.0, 0.0, 0),
            ev("drop", EventKind::Cyber, 0.0, 0.0, 0),
        ];
        EventFilter::new().kind(EventKind::Earthquake).retain(&mut events);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, "keep");
    }
}
