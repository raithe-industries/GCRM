//! USGS earthquakes — free, no API key required.
//!
//! Parses the USGS GeoJSON summary feed
//! (<https://earthquake.usgs.gov/earthquakes/feed/v1.0/>) into normalized [`Event`]s.

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use std::time::Duration;

/// USGS earthquake source. `feed` is a summary feed name, e.g. `all_hour`, `all_day`.
pub struct Usgs {
    pub feed: String,
}

impl Default for Usgs {
    fn default() -> Self {
        Self { feed: "all_hour".to_string() }
    }
}

impl Usgs {
    pub fn url(&self) -> String {
        format!(
            "https://earthquake.usgs.gov/earthquakes/feed/v1.0/summary/{}.geojson",
            self.feed
        )
    }
}

#[async_trait]
impl Source for Usgs {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "usgs",
            name: "USGS Earthquakes",
            domain: EventKind::Earthquake,
            cadence: Duration::from_secs(300),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        let body = crate::http::fetch_text(&self.url()).await?;
        parse_usgs(&body)
    }
}

/// Pure parser: USGS GeoJSON `FeatureCollection` -> events. Unit-tested offline.
pub fn parse_usgs(json: &str) -> anyhow::Result<Vec<Event>> {
    let root: serde_json::Value = serde_json::from_str(json)?;
    let features = root
        .get("features")
        .and_then(|f| f.as_array())
        .ok_or_else(|| anyhow::anyhow!("usgs: missing 'features' array"))?;

    let mut out = Vec::with_capacity(features.len());
    for f in features {
        let props = f.get("properties").cloned().unwrap_or(serde_json::Value::Null);

        // geometry.coordinates = [lon, lat, depth]
        let geo = f
            .get("geometry")
            .and_then(|g| g.get("coordinates"))
            .and_then(|c| c.as_array())
            .filter(|c| c.len() >= 2)
            .and_then(|c| match (c[0].as_f64(), c[1].as_f64()) {
                (Some(lon), Some(lat)) => Geo::new(lat, lon),
                _ => None,
            });

        let mag = props.get("mag").and_then(|m| m.as_f64()).unwrap_or(0.0);
        let time_ms = props.get("time").and_then(|t| t.as_i64()).unwrap_or(0);
        let time = Utc.timestamp_millis_opt(time_ms).single().unwrap_or_else(Utc::now);

        let id = f
            .get("id")
            .and_then(|i| i.as_str())
            .map(String::from)
            .unwrap_or_else(|| format!("usgs-{time_ms}"));
        let title = props
            .get("place")
            .and_then(|p| p.as_str())
            .unwrap_or("Unknown location")
            .to_string();
        let url = props.get("url").and_then(|u| u.as_str()).map(String::from);

        out.push(Event {
            id,
            source_id: "usgs".to_string(),
            kind: EventKind::Earthquake,
            title,
            time,
            geo,
            // Normalize magnitude (~0..9) into [0, 1].
            severity: Severity::new(mag / 9.0),
            url,
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
        {"type":"Feature","id":"nc1",
         "properties":{"mag":4.5,"place":"10km N of Testville","time":1700000000000,"url":"https://example.com/nc1"},
         "geometry":{"type":"Point","coordinates":[-122.5,38.1,5.0]}},
        {"type":"Feature","id":"nc2",
         "properties":{"mag":1.2,"place":"Nowhere","time":1700000100000,"url":null},
         "geometry":{"type":"Point","coordinates":[-200.0,38.1,5.0]}}
      ]
    }"#;

    #[test]
    fn parses_fixture() {
        let ev = parse_usgs(FIXTURE).unwrap();
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0].id, "nc1");
        assert_eq!(ev[0].kind, EventKind::Earthquake);
        assert_eq!(ev[0].title, "10km N of Testville");
        let g = ev[0].geo.unwrap();
        assert!((g.lat - 38.1).abs() < 1e-9 && (g.lon + 122.5).abs() < 1e-9);
        assert!((ev[0].severity.value() - 0.5).abs() < 0.01);
        // Second feature's longitude is out of range -> geo dropped, event still kept.
        assert!(ev[1].geo.is_none());
    }

    #[test]
    fn url_uses_feed() {
        assert!(Usgs::default().url().ends_with("all_hour.geojson"));
    }
}
