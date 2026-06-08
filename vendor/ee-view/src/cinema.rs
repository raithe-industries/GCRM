//! Cinema Mode data engine — a playback-ready reel of geo-located events.
//!
//! SitDeck's *Cinema Mode* is a live spinning 3D globe that streams events pinned
//! to their coordinates, with audio alerts and a strict ≤1 h temporal window
//! ("TV-ready"; see `docs/sitdeck-features.md` → *Special modes → Cinema Mode*).
//! This module is the frontend-agnostic **data** side of that feature: given a
//! batch of [`Event`]s and an explicit `now`, it produces a [`CinemaReel`] — a
//! time-ordered list of [`CinemaFrame`]s, each carrying everything a spinning-globe
//! renderer needs (a camera target, a normalized timeline position, a visual
//! intensity, and an audio-alert flag).
//!
//! It deliberately does only the *data shaping* a globe frontend can't infer on
//! its own:
//! - keeps **only located events** (a frame must pin to coordinates), and
//! - keeps **only events inside the window** `[now - window, now]`, with the
//!   window clamped to Cinema Mode's ≤1 h ceiling, and
//! - emits them **oldest-first** (playback order) with a normalized `progress`
//!   and a globe **camera target**, plus optional time-compressed playback cues.
//!
//! Everything is pure and deterministic: `now` is supplied by the caller, so no
//! hidden clock is read and the reel is fully unit-testable offline.

use chrono::{DateTime, Duration, Utc};
use ee_core::Event;

/// Cinema Mode's hard temporal ceiling: the window never exceeds one hour.
pub const MAX_WINDOW: Duration = Duration::hours(1);

/// How to build a [`CinemaReel`]: the temporal window and the audio-alert
/// threshold. Built fluently; defaults to the full 1 h window and a 0.6 alert
/// threshold.
#[derive(Debug, Clone, Copy)]
pub struct CinemaConfig {
    window: Duration,
    alert_threshold: f64,
}

impl Default for CinemaConfig {
    fn default() -> Self {
        Self { window: MAX_WINDOW, alert_threshold: 0.6 }
    }
}

impl CinemaConfig {
    /// A default config: full 1 h window, audio alerts at severity ≥ 0.6.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the temporal window. Clamped to `[1s, MAX_WINDOW]` — Cinema Mode never
    /// shows more than the last hour, and a zero/negative window is meaningless.
    pub fn window(mut self, window: Duration) -> Self {
        self.window = window.clamp(Duration::seconds(1), MAX_WINDOW);
        self
    }

    /// Set the severity at or above which a frame raises an audio alert.
    /// Clamped to `[0.0, 1.0]` to match the normalized severity scale.
    pub fn alert_threshold(mut self, t: f64) -> Self {
        self.alert_threshold = t.clamp(0.0, 1.0);
        self
    }

    /// Build the reel: the playback-ready, time-ordered frames for `now`.
    pub fn reel(&self, events: &[Event], now: DateTime<Utc>) -> CinemaReel {
        let start = now - self.window;
        let window_ms = self.window.num_milliseconds().max(1) as f64;

        let mut frames: Vec<CinemaFrame> = events
            .iter()
            .filter_map(|e| {
                // A globe frame must pin to coordinates.
                let geo = e.geo?;
                // Inside the window `[start, now]` (inclusive); drop stale & future.
                if e.time < start || e.time > now {
                    return None;
                }
                let offset = e.time - start;
                let progress = (offset.num_milliseconds() as f64 / window_ms).clamp(0.0, 1.0);
                let intensity = e.severity.value();
                Some(CinemaFrame {
                    event: e.clone(),
                    offset,
                    progress,
                    lat: geo.lat,
                    lon: geo.lon,
                    intensity,
                    alert: intensity >= self.alert_threshold,
                })
            })
            .collect();

        // Playback order: oldest first; stable tie-break on id for determinism.
        frames.sort_by(|a, b| {
            a.event.time.cmp(&b.event.time).then_with(|| a.event.id.cmp(&b.event.id))
        });

        CinemaReel { start, end: now, window: self.window, frames }
    }
}

/// One event positioned for spinning-globe playback.
#[derive(Debug, Clone, PartialEq)]
pub struct CinemaFrame {
    /// The underlying normalized event.
    pub event: Event,
    /// Time since the window start (`reel.start`) — the frame's playback position.
    pub offset: Duration,
    /// Normalized position across the window, `0.0` (window start) ..= `1.0` (now).
    pub progress: f64,
    /// Globe camera target latitude (copied from `event.geo` for convenience).
    pub lat: f64,
    /// Globe camera target longitude.
    pub lon: f64,
    /// Visual weight, the event's normalized severity in `[0, 1]`.
    pub intensity: f64,
    /// Whether this frame trips the audio-alert threshold.
    pub alert: bool,
}

/// A built, playback-ready reel: the window bounds plus its ordered frames.
#[derive(Debug, Clone)]
pub struct CinemaReel {
    /// Window start (`end - window`).
    pub start: DateTime<Utc>,
    /// Window end — the `now` the reel was built for.
    pub end: DateTime<Utc>,
    /// The effective (clamped) window length.
    pub window: Duration,
    /// Frames in playback order (oldest first).
    pub frames: Vec<CinemaFrame>,
}

impl CinemaReel {
    /// Number of frames in the reel.
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// Whether the reel has no frames.
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// The frames that trip the audio-alert threshold, in playback order.
    pub fn alerts(&self) -> Vec<&CinemaFrame> {
        self.frames.iter().filter(|f| f.alert).collect()
    }

    /// The most intense frame (ties broken toward the later event), if any.
    pub fn peak(&self) -> Option<&CinemaFrame> {
        self.frames.iter().reduce(|best, f| {
            if f.intensity > best.intensity
                || (f.intensity == best.intensity && f.event.time >= best.event.time)
            {
                f
            } else {
                best
            }
        })
    }

    /// The span actually covered by the frames (last − first event time); zero
    /// for an empty or single-frame reel.
    pub fn span(&self) -> Duration {
        match (self.frames.first(), self.frames.last()) {
            (Some(a), Some(b)) => b.event.time - a.event.time,
            _ => Duration::zero(),
        }
    }

    /// Map each frame onto a compressed `reel_duration` playback timeline,
    /// preserving relative timing within the window. Pairs each frame with the
    /// instant it should fire (`progress × reel_duration`), oldest first — so a
    /// full hour can be replayed as, say, a 60 s loop without losing pacing.
    pub fn playback_cues(&self, reel_duration: Duration) -> Vec<(Duration, &CinemaFrame)> {
        let span_ms = reel_duration.num_milliseconds().max(0) as f64;
        self.frames
            .iter()
            .map(|f| (Duration::milliseconds((f.progress * span_ms) as i64), f))
            .collect()
    }
}

/// Convenience: build a reel with the default config for `now`.
pub fn reel(events: &[Event], now: DateTime<Utc>) -> CinemaReel {
    CinemaConfig::default().reel(events, now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use ee_core::{EventKind, Geo, Severity};

    const T0: i64 = 1_700_000_000;

    fn at(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(T0 + secs, 0).single().unwrap()
    }

    fn ev(id: &str, lat: f64, lon: f64, sev: f64, secs: i64) -> Event {
        Event {
            id: id.into(),
            source_id: "test".into(),
            kind: EventKind::Earthquake,
            title: id.into(),
            time: at(secs),
            geo: Geo::new(lat, lon),
            severity: Severity::new(sev),
            url: None,
            raw: serde_json::Value::Null,
        }
    }

    #[test]
    fn keeps_only_located_events_in_window() {
        let now = at(3600);
        // In window (located), in window (geo-less -> dropped),
        // stale (before window), future (after now).
        let mut geoless = ev("geoless", 0.0, 0.0, 0.5, 1800);
        geoless.geo = None;
        let events = vec![
            ev("inside", 10.0, 20.0, 0.5, 1800),
            geoless,
            ev("stale", 10.0, 20.0, 0.5, -10),
            ev("future", 10.0, 20.0, 0.5, 4000),
        ];
        let r = reel(&events, now);
        let ids: Vec<&str> = r.frames.iter().map(|f| f.event.id.as_str()).collect();
        assert_eq!(ids, vec!["inside"]);
    }

    #[test]
    fn window_bounds_are_inclusive() {
        let now = at(3600);
        let events = vec![
            ev("lo_edge", 1.0, 1.0, 0.5, 0), // exactly now - 1h
            ev("hi_edge", 2.0, 2.0, 0.5, 3600), // exactly now
        ];
        let r = reel(&events, now);
        assert_eq!(r.len(), 2);
        assert!((r.frames[0].progress - 0.0).abs() < 1e-9);
        assert!((r.frames[1].progress - 1.0).abs() < 1e-9);
    }

    #[test]
    fn frames_are_oldest_first_with_progress() {
        let now = at(3600);
        let events = vec![
            ev("late", 0.0, 0.0, 0.5, 2700), // 3/4 through the hour
            ev("early", 0.0, 0.0, 0.5, 900),  // 1/4 through the hour
        ];
        let r = reel(&events, now);
        assert_eq!(r.frames[0].event.id, "early");
        assert_eq!(r.frames[1].event.id, "late");
        assert!((r.frames[0].progress - 0.25).abs() < 1e-6);
        assert!((r.frames[1].progress - 0.75).abs() < 1e-6);
    }

    #[test]
    fn frame_carries_globe_camera_target() {
        let now = at(3600);
        let r = reel(&[ev("e", 35.68, 139.69, 0.4, 1800)], now);
        let f = &r.frames[0];
        assert!((f.lat - 35.68).abs() < 1e-9 && (f.lon - 139.69).abs() < 1e-9);
    }

    #[test]
    fn window_is_clamped_to_one_hour() {
        let now = at(100_000);
        // Ask for a 3 h window; it must collapse to 1 h, excluding the 2-h-old event.
        let cfg = CinemaConfig::new().window(Duration::hours(3));
        let events = vec![
            ev("recent", 0.0, 0.0, 0.5, 100_000 - 1800), // 30 min ago -> kept
            ev("old", 0.0, 0.0, 0.5, 100_000 - 7200),    // 2 h ago -> excluded
        ];
        let r = cfg.reel(&events, now);
        assert_eq!(r.window, MAX_WINDOW);
        let ids: Vec<&str> = r.frames.iter().map(|f| f.event.id.as_str()).collect();
        assert_eq!(ids, vec!["recent"]);
    }

    #[test]
    fn alert_threshold_flags_high_severity() {
        let now = at(3600);
        let cfg = CinemaConfig::new().alert_threshold(0.7);
        let events = vec![
            ev("loud", 0.0, 0.0, 0.8, 1000),
            ev("edge", 0.0, 0.0, 0.7, 1500), // exactly at threshold -> alert
            ev("quiet", 0.0, 0.0, 0.6, 2000),
        ];
        let r = cfg.reel(&events, now);
        let alerting: Vec<&str> = r.alerts().iter().map(|f| f.event.id.as_str()).collect();
        assert_eq!(alerting, vec!["loud", "edge"]);
    }

    #[test]
    fn peak_and_span_are_correct() {
        let now = at(3600);
        let events = vec![
            ev("a", 0.0, 0.0, 0.3, 600),
            ev("b", 0.0, 0.0, 0.9, 1200),
            ev("c", 0.0, 0.0, 0.5, 3000),
        ];
        let r = reel(&events, now);
        assert_eq!(r.peak().unwrap().event.id, "b");
        // span = last - first = 3000 - 600 = 2400 s.
        assert_eq!(r.span(), Duration::seconds(2400));
    }

    #[test]
    fn playback_cues_compress_into_target_duration() {
        let now = at(3600);
        let events = vec![
            ev("q", 0.0, 0.0, 0.5, 900),  // progress 0.25
            ev("h", 0.0, 0.0, 0.5, 1800), // progress 0.50
        ];
        let r = reel(&events, now);
        let cues = r.playback_cues(Duration::seconds(60));
        // 0.25 * 60s = 15s, 0.50 * 60s = 30s.
        assert_eq!(cues[0].0, Duration::seconds(15));
        assert_eq!(cues[1].0, Duration::seconds(30));
        assert_eq!(cues[0].1.event.id, "q");
    }

    #[test]
    fn empty_when_nothing_in_window() {
        let now = at(3600);
        let r = reel(&[ev("old", 0.0, 0.0, 0.5, -100)], now);
        assert!(r.is_empty());
        assert!(r.peak().is_none());
        assert_eq!(r.span(), Duration::zero());
    }
}
