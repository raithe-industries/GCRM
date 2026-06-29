//! NASA EONET — the Earth Observatory Natural Event Tracker. Free, no API key.
//!
//! Parses EONET's GeoJSON event feed
//! (<https://eonet.gsfc.nasa.gov/api/v3/events/geojson>) into normalized [`Event`]s.
//! EONET aggregates several natural-event categories (wildfires, severe storms,
//! volcanoes, sea & lake ice), so one connector enriches multiple map layers.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use std::time::Duration;

/// NASA EONET natural-event source. `days` bounds how far back open events are pulled.
pub struct Eonet {
    pub days: u32,
}

impl Default for Eonet {
    fn default() -> Self {
        Self { days: 30 }
    }
}

impl Eonet {
    pub fn url(&self) -> String {
        format!(
            "https://eonet.gsfc.nasa.gov/api/v3/events/geojson?days={}",
            self.days
        )
    }
}

#[async_trait]
impl Source for Eonet {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "eonet",
            name: "NASA EONET Natural Events",
            // Spans several categories; per-event kind is set precisely below.
            domain: EventKind::Wildfire,
            cadence: Duration::from_secs(1800),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        let body = crate::http::fetch_text(&self.url()).await?;
        parse_eonet(&body)
    }
}

/// Map an EONET category id to our [`EventKind`] taxonomy + a baseline severity.
fn kind_and_severity(category: &str) -> (EventKind, f64) {
    match category {
        "wildfires" => (EventKind::Wildfire, 0.55),
        "severeStorms" => (EventKind::Weather, 0.55),
        "volcanoes" => (EventKind::Other, 0.7),
        _ => (EventKind::Other, 0.4), // seaLakeIce, dustHaze, earthquakes (dup), etc.
    }
}

/// Last `[lon, lat]` pair from a GeoJSON Point or LineString geometry (a storm's
/// track is a LineString; its last vertex is its most recent position).
fn last_point(geometry: &serde_json::Value) -> Option<(f64, f64)> {
    let coords = geometry.get("coordinates")?;
    match geometry.get("type").and_then(|t| t.as_str()) {
        Some("Point") => {
            let c = coords.as_array()?;
            Some((c.first()?.as_f64()?, c.get(1)?.as_f64()?))
        }
        Some("LineString") => {
            let c = coords.as_array()?.last()?.as_array()?;
            Some((c.first()?.as_f64()?, c.get(1)?.as_f64()?))
        }
        _ => None,
    }
}

/// Pure parser: EONET GeoJSON `FeatureCollection` -> events. Unit-tested offline.
pub fn parse_eonet(json: &str) -> anyhow::Result<Vec<Event>> {
    let root: serde_json::Value = serde_json::from_str(json)?;
    // EONET occasionally returns a non-FeatureCollection (a rate-limit / hiccup
    // response with no `features`). Treat that as "no events" rather than an error,
    // so a transient blip never surfaces as a feed error on the map.
    let Some(features) = root.get("features").and_then(|f| f.as_array()) else {
        return Ok(Vec::new());
    };

    let mut out = Vec::with_capacity(features.len());
    for f in features {
        let props = f.get("properties").cloned().unwrap_or(serde_json::Value::Null);

        let id = props.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if id.is_empty() {
            continue;
        }

        let category = props
            .get("categories")
            .and_then(|c| c.as_array())
            .and_then(|c| c.first())
            .and_then(|c| c.get("id"))
            .and_then(|c| c.as_str())
            .unwrap_or("");
        let (kind, severity) = kind_and_severity(category);

        let geo = f
            .get("geometry")
            .and_then(last_point)
            .and_then(|(lon, lat)| Geo::new(lat, lon));

        let time = props
            .get("date")
            .and_then(|d| d.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        let title = props
            .get("title")
            .and_then(|t| t.as_str())
            .unwrap_or("Natural event")
            .to_string();

        out.push(Event {
            id: format!("eonet-{id}"),
            source_id: "eonet".to_string(),
            kind,
            title,
            time,
            geo,
            severity: Severity::new(severity),
            url: props.get("link").and_then(|u| u.as_str()).map(String::from),
            raw: f.clone(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
      "type": "FeatureCollection",
      "features": [
        {"type":"Feature",
         "geometry":{"type":"Point","coordinates":[-120.5,38.2]},
         "properties":{"id":"EONET_1","title":"Test Wildfire","date":"2026-06-01T12:00:00Z",
           "link":"https://eonet.x/1","categories":[{"id":"wildfires","title":"Wildfires"}]}},
        {"type":"Feature",
         "geometry":{"type":"LineString","coordinates":[[100.0,15.0],[102.0,16.0],[104.5,17.5]]},
         "properties":{"id":"EONET_2","title":"Tropical Storm Test","date":"2026-06-02T00:00:00Z",
           "categories":[{"id":"severeStorms","title":"Severe Storms"}]}},
        {"type":"Feature","geometry":{"type":"Point","coordinates":[0,0]},
         "properties":{"title":"no id"}}
      ]
    }"#;

    #[test]
    fn parses_fixture() {
        let ev = parse_eonet(FIXTURE).unwrap();
        // The id-less third feature is dropped.
        assert_eq!(ev.len(), 2);

        assert_eq!(ev[0].id, "eonet-EONET_1");
        assert_eq!(ev[0].kind, EventKind::Wildfire);
        let g = ev[0].geo.unwrap();
        assert!((g.lat - 38.2).abs() < 1e-9 && (g.lon + 120.5).abs() < 1e-9);
        assert!((ev[0].severity.value() - 0.55).abs() < 1e-9);

        // LineString storm: position is the LAST vertex; kind = Weather.
        assert_eq!(ev[1].kind, EventKind::Weather);
        let g2 = ev[1].geo.unwrap();
        assert!((g2.lat - 17.5).abs() < 1e-9 && (g2.lon - 104.5).abs() < 1e-9);
    }

    #[test]
    fn tolerates_missing_array() {
        // A non-FeatureCollection (EONET hiccup) yields no events, not an error.
        assert_eq!(parse_eonet(r#"{"type":"x"}"#).unwrap().len(), 0);
        // Valid but empty collection is also fine.
        assert_eq!(parse_eonet(r#"{"type":"FeatureCollection","features":[]}"#).unwrap().len(), 0);
    }
}
