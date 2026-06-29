//! The normalized event model. Every [`crate::Source`] produces these, regardless
//! of provider, so the rest of the system never deals with raw provider formats.

use crate::geo::Geo;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The domain a signal belongs to. `Other` is the catch-all for a new provider
/// before it earns a first-class variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Earthquake,
    Wildfire,
    Volcano,
    Aircraft,
    Vessel,
    Conflict,
    Cyber,
    Market,
    Weather,
    AirQuality,
    Health,
    Transport,
    News,
    Other,
}

/// Normalized severity in `[0.0, 1.0]`. Construct via [`Severity::new`] (clamps).
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct Severity(f64);

impl Severity {
    pub fn new(v: f64) -> Self {
        // Guard NaN/±Inf BEFORE clamping: f64::clamp PASSES NaN THROUGH, so a severity
        // computed from a ratio with a zero denominator (common in feed parsers) would
        // otherwise construct an out-of-range Severity and NaN-poison every downstream
        // sum/mean/composite up to the systemic index. A non-finite severity is treated
        // as the lowest, making the `[0.0, 1.0]` invariant actually total. (audit ee_core_cargo-1)
        let v = if v.is_finite() { v } else { 0.0 };
        Self(v.clamp(0.0, 1.0))
    }
    pub fn value(&self) -> f64 {
        self.0
    }
}

/// A single normalized situational-awareness event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// Stable identifier (provider id where available; otherwise a content hash).
    pub id: String,
    /// The [`crate::SourceMeta::id`] of the producer.
    pub source_id: String,
    pub kind: EventKind,
    pub title: String,
    pub time: DateTime<Utc>,
    /// Some events (e.g. headlines) have no location.
    pub geo: Option<Geo>,
    pub severity: Severity,
    pub url: Option<String>,
    /// Original provider payload, retained for traceability/debugging.
    pub raw: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_clamps() {
        assert_eq!(Severity::new(2.0).value(), 1.0);
        assert_eq!(Severity::new(-1.0).value(), 0.0);
        assert_eq!(Severity::new(0.5).value(), 0.5);
    }

    #[test]
    fn severity_rejects_non_finite() {
        // A NaN/Inf severity (e.g. an x/0 ratio in a feed parser) must not leak through and
        // poison downstream sums — the [0,1] invariant has to be total. Any non-finite is
        // treated as the lowest (a non-finite is a computation error, not real max severity).
        // (audit ee_core_cargo-1)
        assert_eq!(Severity::new(f64::NAN).value(), 0.0);
        assert_eq!(Severity::new(f64::INFINITY).value(), 0.0);
        assert_eq!(Severity::new(f64::NEG_INFINITY).value(), 0.0);
        assert!(Severity::new(f64::NAN).value().is_finite());
    }

    #[test]
    fn event_roundtrips_json() {
        let e = Event {
            id: "x".into(),
            source_id: "usgs".into(),
            kind: EventKind::Earthquake,
            title: "test".into(),
            time: Utc::now(),
            geo: Geo::new(0.0, 0.0),
            severity: Severity::new(0.4),
            url: None,
            raw: serde_json::json!({ "a": 1 }),
        };
        let s = serde_json::to_string(&e).unwrap();
        let back: Event = serde_json::from_str(&s).unwrap();
        assert_eq!(e, back);
    }
}
