//! Widget data shapes — the frontend-agnostic payloads behind dashboard widgets.
//!
//! Every situational-awareness dashboard renders the same event stream through a
//! handful of recurring *widget archetypes*. SitDeck ships "**55 drag-and-drop
//! widgets** (18 categories)" and World Monitor "55 widgets" (`sitdeck-features.md`
//! *Widgets (55, by category)*; `worldmonitor-features.md`), but underneath the 55
//! there are only a few data shapes. This module owns four of them — the ones the
//! capability map calls out: a **ticker**, a **table**, a **timeline**, and a
//! **gauge** (capability-map: *Presentation primitives → Widget data shapes (ticker,
//! table, timeline, gauge)*).
//!
//! Each builder takes a slice of normalized [`Event`]s and returns a pure,
//! `serde`-serializable shape a frontend can render without knowing anything about
//! provider formats — no I/O, no hidden clock (recency-sensitive builders take an
//! explicit `now`). A shared [`SeverityBand`] gives every widget the same colour
//! coding, so a chip in the ticker matches the needle in the gauge.

use chrono::{DateTime, Duration, Utc};
use ee_core::{Event, EventKind, Severity};
use serde::Serialize;

/// Five colour bands over the normalized `[0,1]` severity scale. Shared by the
/// ticker chips and the gauge needle so the whole dashboard speaks one colour code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SeverityBand {
    Low,
    Moderate,
    Elevated,
    High,
    Critical,
}

impl SeverityBand {
    /// Band for a normalized severity. Thresholds: <0.2 Low, <0.4 Moderate,
    /// <0.6 Elevated, <0.8 High, ≥0.8 Critical.
    pub fn of(severity: f64) -> Self {
        match severity {
            s if s < 0.2 => SeverityBand::Low,
            s if s < 0.4 => SeverityBand::Moderate,
            s if s < 0.6 => SeverityBand::Elevated,
            s if s < 0.8 => SeverityBand::High,
            _ => SeverityBand::Critical,
        }
    }

    /// Human-readable label for a chip / legend.
    pub fn label(self) -> &'static str {
        match self {
            SeverityBand::Low => "Low",
            SeverityBand::Moderate => "Moderate",
            SeverityBand::Elevated => "Elevated",
            SeverityBand::High => "High",
            SeverityBand::Critical => "Critical",
        }
    }

    /// Suggested `#rrggbb` colour (a style hint, not a mandate).
    pub fn color(self) -> &'static str {
        match self {
            SeverityBand::Low => "#2a9d8f",
            SeverityBand::Moderate => "#8ab17d",
            SeverityBand::Elevated => "#e9c46a",
            SeverityBand::High => "#f4a261",
            SeverityBand::Critical => "#e63946",
        }
    }
}

// --- Ticker -----------------------------------------------------------------

/// One item in a scrolling ticker / marquee.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TickerItem {
    pub id: String,
    pub kind: EventKind,
    /// Short headline (the event title).
    pub label: String,
    /// Normalized severity in `[0,1]`.
    pub severity: f64,
    pub band: SeverityBand,
    /// `#rrggbb` chip colour, from the band.
    pub color: &'static str,
    pub time: DateTime<Utc>,
    pub url: Option<String>,
}

/// Build a ticker: the `max` most urgent events, severity-first then newest-first.
///
/// A ticker is a glanceable "what's hot right now" strip, so it ranks by severity
/// (ties broken by recency) and truncates to `max` items. Pure and deterministic.
pub fn ticker(events: &[Event], max: usize) -> Vec<TickerItem> {
    let mut items: Vec<TickerItem> = events
        .iter()
        .map(|e| {
            let sev = e.severity.value();
            let band = SeverityBand::of(sev);
            TickerItem {
                id: e.id.clone(),
                kind: e.kind,
                label: e.title.clone(),
                severity: sev,
                band,
                color: band.color(),
                time: e.time,
                url: e.url.clone(),
            }
        })
        .collect();
    items.sort_by(|a, b| {
        b.severity
            .partial_cmp(&a.severity)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.time.cmp(&a.time))
            .then(a.id.cmp(&b.id))
    });
    items.truncate(max);
    items
}

// --- Table ------------------------------------------------------------------

/// How a [`table`] is ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableSort {
    /// Newest first.
    TimeDesc,
    /// Most severe first.
    SeverityDesc,
    /// Grouped by kind (then newest within a kind).
    Kind,
}

/// One row in a tabular widget.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TableRow {
    pub id: String,
    pub time: DateTime<Utc>,
    pub kind: EventKind,
    pub title: String,
    pub severity: f64,
    pub band: SeverityBand,
    /// `[lat, lon]` when located, else `None`.
    pub location: Option<[f64; 2]>,
    pub url: Option<String>,
}

/// Build a sortable table over the events. Pure and deterministic (ties always
/// break on `id`, so the order is stable across refreshes).
pub fn table(events: &[Event], sort: TableSort) -> Vec<TableRow> {
    let mut rows: Vec<TableRow> = events
        .iter()
        .map(|e| TableRow {
            id: e.id.clone(),
            time: e.time,
            kind: e.kind,
            title: e.title.clone(),
            severity: e.severity.value(),
            band: SeverityBand::of(e.severity.value()),
            location: e.geo.map(|g| [g.lat, g.lon]),
            url: e.url.clone(),
        })
        .collect();
    rows.sort_by(|a, b| {
        let primary = match sort {
            TableSort::TimeDesc => b.time.cmp(&a.time),
            TableSort::SeverityDesc => b
                .severity
                .partial_cmp(&a.severity)
                .unwrap_or(std::cmp::Ordering::Equal),
            TableSort::Kind => (a.kind as u8)
                .cmp(&(b.kind as u8))
                .then(b.time.cmp(&a.time)),
        };
        primary.then(a.id.cmp(&b.id))
    });
    rows
}

// --- Timeline ---------------------------------------------------------------

/// One time bin in a [`Timeline`] histogram.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TimelineBucket {
    /// Inclusive start of the bin (UTC).
    pub start: DateTime<Utc>,
    /// Exclusive end of the bin (UTC); the final bin includes `now`.
    pub end: DateTime<Utc>,
    /// Events falling in this bin.
    pub count: usize,
    /// Peak normalized severity among them (0.0 when empty).
    pub peak: f64,
}

/// A fixed-width histogram of events over a trailing time window — the data behind
/// a timeline / sparkline widget.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Timeline {
    /// Bins oldest-first, contiguous and equal-width.
    pub buckets: Vec<TimelineBucket>,
    /// Bin width, in seconds.
    pub bucket_secs: i64,
    /// Window start (oldest bin start) and end (`now`).
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    /// Events that fell inside the window (sum of bin counts).
    pub counted: usize,
    /// Busiest single bin's count (handy for scaling a y-axis).
    pub peak_count: usize,
}

/// Bucket events into a trailing-window histogram ending at `now`.
///
/// Produces `ceil(span / bucket)` contiguous equal-width bins covering
/// `[now - n*bucket, now]`, oldest-first; events outside the window are ignored.
/// Pure and deterministic (`now` is supplied, never read from the clock). A
/// non-positive `bucket` or `span` yields an empty timeline.
pub fn timeline(events: &[Event], bucket: Duration, span: Duration, now: DateTime<Utc>) -> Timeline {
    let bucket_secs = bucket.num_seconds();
    let span_secs = span.num_seconds();
    if bucket_secs <= 0 || span_secs <= 0 {
        return Timeline {
            buckets: Vec::new(),
            bucket_secs: bucket_secs.max(0),
            start: now,
            end: now,
            counted: 0,
            peak_count: 0,
        };
    }

    // Number of bins to cover the span (round up so the whole window is represented).
    let n = ((span_secs + bucket_secs - 1) / bucket_secs).max(1) as usize;
    let start = now - Duration::seconds(bucket_secs * n as i64);

    let mut buckets: Vec<TimelineBucket> = (0..n)
        .map(|i| {
            let b_start = start + Duration::seconds(bucket_secs * i as i64);
            let b_end = b_start + Duration::seconds(bucket_secs);
            TimelineBucket { start: b_start, end: b_end, count: 0, peak: 0.0 }
        })
        .collect();

    let mut counted = 0usize;
    for e in events {
        // Window is (start, now] — drop anything older than the window or in the future.
        if e.time <= start || e.time > now {
            continue;
        }
        let offset = (e.time - start).num_seconds();
        let idx = ((offset / bucket_secs) as usize).min(n - 1);
        let b = &mut buckets[idx];
        b.count += 1;
        b.peak = b.peak.max(e.severity.value());
        counted += 1;
    }

    let peak_count = buckets.iter().map(|b| b.count).max().unwrap_or(0);
    Timeline { buckets, bucket_secs, start, end: now, counted, peak_count }
}

// --- Gauge ------------------------------------------------------------------

/// A single-needle gauge summarizing a stream into one `[0,1]` reading + band.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Gauge {
    /// Caption (e.g. `"Threat Level"`).
    pub label: String,
    /// Composite reading in `[0,1]`.
    pub value: f64,
    pub band: SeverityBand,
    pub color: &'static str,
    /// Events the reading was computed over.
    pub count: usize,
    /// Peak severity among them (the loudest single signal).
    pub peak: f64,
}

/// Build a gauge: a composite "how hot is this stream" needle.
///
/// `value = 0.7·peak + 0.3·(1 − e^(−count/scale))` with `scale = 10` — a loud single
/// signal dominates, while sheer volume nudges the needle up and saturates. Matches
/// the project's `[0,1]` severity convention and the `ee-correlate` rollup weighting,
/// so the gauge agrees with the regional panels. Empty input reads 0.0 (Low).
pub fn gauge(label: impl Into<String>, events: &[Event]) -> Gauge {
    const VOLUME_SCALE: f64 = 10.0;
    let peak = events
        .iter()
        .map(|e| e.severity.value())
        .fold(0.0_f64, f64::max);
    let volume = 1.0 - (-(events.len() as f64) / VOLUME_SCALE).exp();
    let value = Severity::new(0.7 * peak + 0.3 * volume).value();
    let band = SeverityBand::of(value);
    Gauge {
        label: label.into(),
        value,
        band,
        color: band.color(),
        count: events.len(),
        peak,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use ee_core::Geo;

    fn ev(id: &str, kind: EventKind, sev: f64, secs_ago: i64, now: DateTime<Utc>) -> Event {
        Event {
            id: id.into(),
            source_id: "test".into(),
            kind,
            title: format!("{id} {kind:?}"),
            time: now - Duration::seconds(secs_ago),
            geo: Geo::new(10.0, 20.0),
            severity: Severity::new(sev),
            url: None,
            raw: serde_json::Value::Null,
        }
    }

    fn now() -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000, 0).single().unwrap()
    }

    #[test]
    fn severity_band_thresholds() {
        assert_eq!(SeverityBand::of(0.0), SeverityBand::Low);
        assert_eq!(SeverityBand::of(0.19), SeverityBand::Low);
        assert_eq!(SeverityBand::of(0.2), SeverityBand::Moderate);
        assert_eq!(SeverityBand::of(0.5), SeverityBand::Elevated);
        assert_eq!(SeverityBand::of(0.75), SeverityBand::High);
        assert_eq!(SeverityBand::of(0.8), SeverityBand::Critical);
        assert_eq!(SeverityBand::of(1.0), SeverityBand::Critical);
        // Every band has a non-empty label + a #rrggbb colour.
        for b in [
            SeverityBand::Low,
            SeverityBand::Moderate,
            SeverityBand::Elevated,
            SeverityBand::High,
            SeverityBand::Critical,
        ] {
            assert!(!b.label().is_empty());
            assert!(b.color().starts_with('#') && b.color().len() == 7);
        }
    }

    #[test]
    fn ticker_ranks_severity_then_recency_and_truncates() {
        let n = now();
        let events = vec![
            ev("a", EventKind::Earthquake, 0.3, 10, n),
            ev("b", EventKind::Cyber, 0.9, 100, n),  // most severe
            ev("c", EventKind::Wildfire, 0.5, 5, n), // newer than d, same sev
            ev("d", EventKind::Weather, 0.5, 50, n),
        ];
        let t = ticker(&events, 3);
        assert_eq!(t.len(), 3); // truncated from 4
        assert_eq!(t[0].id, "b"); // highest severity
        assert_eq!(t[0].band, SeverityBand::Critical);
        // Equal severity (c,d) -> newer first.
        assert_eq!(t[1].id, "c");
        assert_eq!(t[2].id, "d");
    }

    #[test]
    fn table_sorts_three_ways() {
        let n = now();
        let events = vec![
            ev("a", EventKind::Wildfire, 0.4, 10, n),
            ev("b", EventKind::Earthquake, 0.9, 100, n),
            ev("c", EventKind::Earthquake, 0.2, 5, n),
        ];

        let by_time = table(&events, TableSort::TimeDesc);
        assert_eq!(by_time[0].id, "c"); // newest (5s ago)

        let by_sev = table(&events, TableSort::SeverityDesc);
        assert_eq!(by_sev[0].id, "b"); // 0.9

        let by_kind = table(&events, TableSort::Kind);
        // Earthquake (variant 0) sorts before Wildfire (variant 1); within Earthquake,
        // newest first -> c (5s) before b (100s).
        assert_eq!(by_kind[0].id, "c");
        assert_eq!(by_kind[1].id, "b");
        assert_eq!(by_kind[2].id, "a");
        // Located rows carry [lat, lon].
        assert_eq!(by_kind[0].location, Some([10.0, 20.0]));
    }

    #[test]
    fn timeline_buckets_window_and_excludes_outside() {
        let n = now();
        // 1h window in 15-min bins -> 4 bins.
        let events = vec![
            ev("recent", EventKind::Earthquake, 0.8, 60, n), // bin 3 (last 15m)
            ev("mid", EventKind::Wildfire, 0.5, 1900, n),    // ~31m ago -> bin 1
            ev("mid2", EventKind::Wildfire, 0.6, 2000, n),   // ~33m ago -> bin 1
            ev("stale", EventKind::Cyber, 0.9, 4000, n),     // >1h ago -> excluded
            ev("future", EventKind::News, 0.9, -10, n),      // future -> excluded
        ];
        let tl = timeline(&events, Duration::minutes(15), Duration::hours(1), n);
        assert_eq!(tl.buckets.len(), 4);
        assert_eq!(tl.bucket_secs, 900);
        assert_eq!(tl.end, n);
        assert_eq!(tl.counted, 3); // stale + future dropped
        // Bin 1 has the two mid events, peak 0.6.
        assert_eq!(tl.buckets[1].count, 2);
        assert!((tl.buckets[1].peak - 0.6).abs() < 1e-9);
        // Bin 3 (most recent) has the one recent event.
        assert_eq!(tl.buckets[3].count, 1);
        assert!((tl.buckets[3].peak - 0.8).abs() < 1e-9);
        assert_eq!(tl.peak_count, 2);
        // Bins are contiguous and equal-width.
        for w in tl.buckets.windows(2) {
            assert_eq!(w[0].end, w[1].start);
        }
    }

    #[test]
    fn timeline_rejects_nonpositive_params() {
        let n = now();
        let tl = timeline(&[], Duration::zero(), Duration::hours(1), n);
        assert!(tl.buckets.is_empty());
        assert_eq!(tl.counted, 0);
    }

    #[test]
    fn gauge_peak_dominates_and_volume_lifts() {
        let n = now();
        // Single loud signal.
        let loud = vec![ev("x", EventKind::Earthquake, 0.9, 1, n)];
        let g_loud = gauge("Threat Level", &loud);
        assert_eq!(g_loud.count, 1);
        assert!((g_loud.peak - 0.9).abs() < 1e-9);
        // value = 0.7*0.9 + 0.3*(1-e^-0.1) ≈ 0.63 + 0.0286 ≈ 0.659 -> High.
        assert!(g_loud.value > 0.63 && g_loud.value < 0.7);
        assert_eq!(g_loud.band, SeverityBand::High);

        // Many quiet signals lift the needle via volume but stay below the loud one.
        let many: Vec<Event> = (0..30)
            .map(|i| ev(&format!("q{i}"), EventKind::News, 0.2, i, n))
            .collect();
        let g_many = gauge("Threat Level", &many);
        assert_eq!(g_many.count, 30);
        // peak 0.2 -> 0.7*0.2=0.14, volume ~1 -> +0.3 => ~0.44, below the loud gauge.
        assert!(g_many.value < g_loud.value);
        assert!(g_many.value > 0.4);

        // Empty -> 0.0, Low.
        let g_empty = gauge("Threat Level", &[]);
        assert_eq!(g_empty.value, 0.0);
        assert_eq!(g_empty.band, SeverityBand::Low);
    }

    #[test]
    fn shapes_serialize_to_json() {
        let n = now();
        let events = vec![ev("a", EventKind::Earthquake, 0.85, 10, n)];
        let tj = serde_json::to_string(&ticker(&events, 5)).unwrap();
        assert!(tj.contains("\"band\":\"critical\""));
        let rj = serde_json::to_string(&table(&events, TableSort::TimeDesc)).unwrap();
        assert!(rj.contains("\"location\":[10.0,20.0]"));
        let lj = serde_json::to_string(&timeline(&events, Duration::minutes(30), Duration::hours(1), n))
            .unwrap();
        assert!(lj.contains("\"bucket_secs\":1800"));
        let gj = serde_json::to_string(&gauge("Threat Level", &events)).unwrap();
        assert!(gj.contains("\"label\":\"Threat Level\""));
    }
}
