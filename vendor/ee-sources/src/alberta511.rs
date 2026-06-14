//! Alberta 511 — live road events (closures, collisions, construction, restrictions)
//! on Alberta's provincial highway network, from Alberta Transportation and Economic
//! Corridors. Free, no API key.
//!
//! 511 Alberta runs on the same Castle Rock / OneNetwork platform as Ontario's
//! [`crate::ontario511`], so the wire format is byte-identical — this source reuses
//! that pure parser ([`crate::ontario511::parse_511`]) with an Alberta `source_id`
//! and id prefix. It adds Alberta highway coverage (a distinct jurisdiction, zero
//! overlap with Ontario) under the same [`EventKind::Transport`] layer.

use async_trait::async_trait;
use ee_core::{Event, EventKind, Source, SourceMeta};
use std::time::Duration;

/// Alberta 511 road-event source.
#[derive(Default)]
pub struct Alberta511;

impl Alberta511 {
    pub fn url(&self) -> &'static str {
        // NOTE: singular `/event` (the plural `/events` 404s) and GET-only (HEAD 405s) —
        // same quirks as the Ontario twin; don't HEAD-probe it in a health check.
        "https://511.alberta.ca/api/v2/get/event?format=json&lang=en"
    }
}

#[async_trait]
impl Source for Alberta511 {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "alberta511",
            name: "Alberta 511 Road Events",
            domain: EventKind::Transport,
            cadence: Duration::from_secs(300),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        let client = reqwest::Client::builder()
            .user_agent("engineering-effects/0.1 (+https://raithe.ca)")
            .build()?;
        let body = client.get(self.url()).send().await?.text().await?;
        parse_alberta511(&body)
    }
}

/// Pure parser: Alberta 511 events JSON array -> events. Delegates to the shared 511
/// parser (the field schema matches Ontario's exactly); `ab511-` ids keep Alberta
/// events distinct from Ontario's `on511-` ids under the shared Transport layer.
pub fn parse_alberta511(json: &str) -> anyhow::Result<Vec<Event>> {
    crate::ontario511::parse_511(json, "alberta511", "ab511", "https://511.alberta.ca/")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Byte-identical schema to Ontario 511 (verified live: same ID/EventType/
    // IsFullClosure/RoadwayName/DirectionOfTravel/LastUpdated/Latitude/Longitude fields).
    const FIXTURE: &str = r#"[
      {"ID":7,"EventType":"closures","RoadwayName":"Moraine Lake Rd","DirectionOfTravel":"All",
       "Description":"All lanes closed.","IsFullClosure":true,"Severity":"None","LastUpdated":1781353801,
       "Latitude":51.41239,"Longitude":-116.19046},
      {"ID":13068,"EventType":"accidentsAndIncidents","RoadwayName":"HWY-93A","DirectionOfTravel":"Both",
       "Description":"Washout on HWY-93A.","IsFullClosure":false,"Severity":"Major","LastUpdated":1781415588,
       "Latitude":52.80252,"Longitude":-118.04611}
    ]"#;

    #[test]
    fn parses_fixture() {
        let ev = parse_alberta511(FIXTURE).unwrap();
        assert_eq!(ev.len(), 2);
        // Alberta ids carry the ab511- prefix (never collide with Ontario's on511-).
        assert_eq!(ev[0].id, "ab511-7");
        assert_eq!(ev[0].source_id, "alberta511");
        assert_eq!(ev[0].kind, EventKind::Transport);
        assert_eq!(ev[0].title, "Closure — Moraine Lake Rd All");
        // Full closure is loud regardless of the sparse Severity string.
        assert!((ev[0].severity.value() - 0.85).abs() < 1e-9);
        let g = ev[0].geo.unwrap();
        assert!((g.lat - 51.41239).abs() < 1e-5 && (g.lon + 116.19046).abs() < 1e-5);
        assert_eq!(ev[1].id, "ab511-13068");
        assert_eq!(ev[1].title, "Incident — HWY-93A Both");
    }

    #[test]
    fn errors_on_non_array() {
        assert!(parse_alberta511(r#"{"x":1}"#).is_err());
    }
}
