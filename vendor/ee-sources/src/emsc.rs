//! EMSC (European-Mediterranean Seismological Centre) — global earthquakes via the
//! seismicportal.eu FDSN event service. Free, no API key.
//!
//! Complements [`crate::usgs`]: EMSC aggregates regional networks (BMKG, AFAD, CSN,
//! INGV …) and is markedly denser than USGS outside the Americas, so it fills the
//! Asia/Europe/Pacific seismicity the US feed under-reports. Returns normalized
//! [`EventKind::Earthquake`] [`Event`]s.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use std::time::Duration;

/// EMSC earthquake source. `days` bounds the window; `min_mag` filters out micro-noise.
pub struct Emsc {
    pub days: u32,
    pub min_mag: f64,
}

impl Default for Emsc {
    fn default() -> Self {
        Self { days: 1, min_mag: 2.0 }
    }
}

impl Emsc {
    pub fn url(&self) -> String {
        let start = (Utc::now() - chrono::Duration::days(self.days as i64)).format("%Y-%m-%d");
        format!(
            "https://www.seismicportal.eu/fdsnws/event/1/query?format=json&limit=800&start={start}&minmag={}",
            self.min_mag
        )
    }
}

#[async_trait]
impl Source for Emsc {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "emsc",
            name: "EMSC Earthquakes (global)",
            domain: EventKind::Earthquake,
            cadence: Duration::from_secs(300),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        let client = reqwest::Client::builder()
            .user_agent("engineering-effects/0.1 (+https://raithe.ca)")
            .build()?;
        // EMSC returns Content-Type text/plain even for JSON — parse the body directly.
        let body = client.get(self.url()).send().await?.text().await?;
        parse_emsc(&body)
    }
}

/// Title-case an EMSC region label (they arrive ALL-CAPS, e.g. "MOLUCCA SEA").
fn title_case(s: &str) -> String {
    s.split_whitespace()
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Pure parser: EMSC FDSN GeoJSON -> events. Unit-tested offline.
///
/// Uses the scalar `properties.lat`/`lon`/`mag`/`depth` (cleaner than the
/// `[lon, lat, depth]` geometry tuple, whose depth is sign-flipped).
pub fn parse_emsc(json: &str) -> anyhow::Result<Vec<Event>> {
    let root: serde_json::Value = serde_json::from_str(json)?;
    let features = root
        .get("features")
        .and_then(|f| f.as_array())
        .ok_or_else(|| anyhow::anyhow!("emsc: missing 'features' array"))?;

    let mut out = Vec::with_capacity(features.len());
    for f in features {
        let props = f.get("properties").cloned().unwrap_or(serde_json::Value::Null);

        let (Some(lat), Some(lon)) = (
            props.get("lat").and_then(serde_json::Value::as_f64),
            props.get("lon").and_then(serde_json::Value::as_f64),
        ) else {
            continue;
        };
        let Some(geo) = Geo::new(lat, lon) else { continue };

        let id = f
            .get("id")
            .and_then(|v| v.as_str())
            .or_else(|| props.get("unid").and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string();
        if id.is_empty() {
            continue;
        }

        let mag = props.get("mag").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
        let region = props.get("flynn_region").and_then(|v| v.as_str()).unwrap_or("");
        let title = if region.is_empty() {
            "Earthquake".to_string()
        } else {
            title_case(region)
        };
        let time = props
            .get("time")
            .and_then(|t| t.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        out.push(Event {
            id: format!("emsc-{id}"),
            source_id: "emsc".to_string(),
            kind: EventKind::Earthquake,
            title,
            time,
            geo: Some(geo),
            severity: Severity::new(mag / 9.0),
            url: Some(format!("https://www.seismicportal.eu/eventdetails.html?unid={id}")),
            raw: f.clone(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
      "type":"FeatureCollection",
      "features":[
        {"type":"Feature","id":"20260613_0000123",
         "geometry":{"type":"Point","coordinates":[126.36,2.38,-30.0]},
         "properties":{"lat":2.38,"lon":126.36,"depth":30.0,"mag":3.0,"magtype":"mb",
           "time":"2026-06-13T21:32:00.0Z","flynn_region":"MOLUCCA SEA, INDONESIA","unid":"20260613_0000123"}},
        {"type":"Feature","id":"x2",
         "geometry":{"type":"Point","coordinates":[13.2,42.6,-10.0]},
         "properties":{"lat":42.6,"lon":13.2,"mag":4.5,"time":"2026-06-13T20:00:00.0Z","flynn_region":"CENTRAL ITALY"}}
      ]
    }"#;

    #[test]
    fn parses_fixture() {
        let ev = parse_emsc(FIXTURE).unwrap();
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0].id, "emsc-20260613_0000123");
        assert_eq!(ev[0].kind, EventKind::Earthquake);
        assert_eq!(ev[0].title, "Molucca Sea, Indonesia");
        let g = ev[0].geo.unwrap();
        assert!((g.lat - 2.38).abs() < 1e-9 && (g.lon - 126.36).abs() < 1e-9);
        assert!((ev[0].severity.value() - 3.0 / 9.0).abs() < 1e-9);
        assert_eq!(ev[1].title, "Central Italy");
    }

    #[test]
    fn errors_on_missing_array() {
        assert!(parse_emsc(r#"{"type":"x"}"#).is_err());
    }
}
