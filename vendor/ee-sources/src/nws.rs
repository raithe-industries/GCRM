//! NOAA / NWS active weather alerts — free, no API key (a `User-Agent` is required).
//!
//! Parses the National Weather Service active-alerts GeoJSON
//! (<https://api.weather.gov/alerts/active>) into normalized [`EventKind::Weather`]
//! [`Event`]s: warnings, watches, and advisories across the United States.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use std::time::Duration;

/// NWS active weather-alert source.
#[derive(Default)]
pub struct Nws;

impl Nws {
    pub fn url(&self) -> &'static str {
        "https://api.weather.gov/alerts/active"
    }
}

#[async_trait]
impl Source for Nws {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "nws",
            name: "NWS Weather Alerts",
            domain: EventKind::Weather,
            cadence: Duration::from_secs(300),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        // NWS rejects requests without a descriptive User-Agent (HTTP 403); the shared
        // client sets one.
        let body = crate::http::fetch_text(self.url()).await?;
        parse_nws(&body)
    }
}

/// Map an NWS `severity` label to normalized severity.
fn severity_for(s: &str) -> f64 {
    match s {
        "Extreme" => 0.95,
        "Severe" => 0.7,
        "Moderate" => 0.45,
        "Minor" => 0.2,
        _ => 0.3, // Unknown
    }
}

/// Centroid (mean vertex) of a GeoJSON Polygon/MultiPolygon coordinate tree.
/// NWS alert geometry is often null (zone-only); callers treat that as no `geo`.
fn centroid(geometry: &serde_json::Value) -> Option<Geo> {
    // Recursively collect every [lon, lat] leaf pair.
    fn collect(v: &serde_json::Value, acc: &mut Vec<(f64, f64)>) {
        if let Some(arr) = v.as_array() {
            if arr.len() == 2 && arr[0].is_number() && arr[1].is_number() {
                if let (Some(lon), Some(lat)) = (arr[0].as_f64(), arr[1].as_f64()) {
                    acc.push((lon, lat));
                }
            } else {
                for x in arr {
                    collect(x, &mut *acc);
                }
            }
        }
    }
    let coords = geometry.get("coordinates")?;
    let mut pts = Vec::new();
    collect(coords, &mut pts);
    if pts.is_empty() {
        return None;
    }
    let (slon, slat) = pts.iter().fold((0.0, 0.0), |(a, b), (lon, lat)| (a + lon, b + lat));
    let n = pts.len() as f64;
    Geo::new(slat / n, slon / n)
}

/// Pure parser: NWS alerts GeoJSON -> events. Unit-tested offline.
pub fn parse_nws(json: &str) -> anyhow::Result<Vec<Event>> {
    let root: serde_json::Value = serde_json::from_str(json)?;
    let features = root
        .get("features")
        .and_then(|f| f.as_array())
        .ok_or_else(|| anyhow::anyhow!("nws: missing 'features' array"))?;

    let mut out = Vec::with_capacity(features.len());
    for f in features {
        let props = f.get("properties").cloned().unwrap_or(serde_json::Value::Null);

        let id = f
            .get("id")
            .and_then(|i| i.as_str())
            .map(String::from)
            .unwrap_or_default();
        if id.is_empty() {
            continue;
        }

        let geo = f.get("geometry").filter(|g| !g.is_null()).and_then(centroid);

        let time = props
            .get("sent")
            .or_else(|| props.get("effective"))
            .or_else(|| props.get("onset"))
            .and_then(|t| t.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        let event = props.get("event").and_then(|e| e.as_str()).unwrap_or("Weather Alert");
        let area = props.get("areaDesc").and_then(|a| a.as_str()).unwrap_or("");
        let title = if area.is_empty() {
            event.to_string()
        } else {
            format!("{event} — {area}")
        };

        let severity = props.get("severity").and_then(|s| s.as_str()).unwrap_or("Unknown");

        out.push(Event {
            id,
            source_id: "nws".to_string(),
            kind: EventKind::Weather,
            title,
            time,
            geo,
            severity: Severity::new(severity_for(severity)),
            url: props.get("@id").and_then(|u| u.as_str()).map(String::from),
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
        {"id":"urn:oid:alert.1",
         "geometry":{"type":"Polygon","coordinates":[[[-100.0,40.0],[-100.0,42.0],[-98.0,42.0],[-98.0,40.0],[-100.0,40.0]]]},
         "properties":{"event":"Severe Thunderstorm Warning","areaDesc":"Lincoln, NE",
           "severity":"Severe","sent":"2026-06-08T07:40:00-05:00","@id":"https://api.weather.gov/alerts/1"}},
        {"id":"urn:oid:alert.2","geometry":null,
         "properties":{"event":"Heat Advisory","areaDesc":"Phoenix","severity":"Minor","sent":"2026-06-08T12:00:00+00:00"}},
        {"id":"","properties":{"event":"x"}}
      ]
    }"#;

    #[test]
    fn parses_fixture() {
        let ev = parse_nws(FIXTURE).unwrap();
        // The id-less third entry is dropped.
        assert_eq!(ev.len(), 2);

        assert_eq!(ev[0].id, "urn:oid:alert.1");
        assert_eq!(ev[0].kind, EventKind::Weather);
        assert_eq!(ev[0].title, "Severe Thunderstorm Warning — Lincoln, NE");
        assert!((ev[0].severity.value() - 0.7).abs() < 1e-9); // Severe
        // Polygon centroid sits inside the box.
        let g = ev[0].geo.unwrap();
        assert!((g.lat - 41.2).abs() < 0.5 && (g.lon + 99.2).abs() < 0.5);

        // Null geometry -> no geo; Minor -> low severity.
        assert!(ev[1].geo.is_none());
        assert!((ev[1].severity.value() - 0.2).abs() < 1e-9);
    }

    #[test]
    fn errors_on_missing_array() {
        assert!(parse_nws(r#"{"type":"x"}"#).is_err());
    }
}
