//! Environment and Climate Change Canada (ECCC) Air Quality Health Index (AQHI) —
//! live station observations. Free, no API key. A distinctly Canadian signal and a
//! direct wildfire-smoke proxy: when fires burn, AQHI spikes across whole regions.
//!
//! Reads the MSC GeoMet OGC-API `aqhi-observations-realtime` collection, latest
//! reading per station (<https://api.weather.gc.ca/collections/aqhi-observations-realtime>),
//! into normalized [`EventKind::AirQuality`] [`Event`]s.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use std::time::Duration;

/// ECCC AQHI observation source (latest reading per Canadian station).
#[derive(Default)]
pub struct EcccAqhi;

impl EcccAqhi {
    pub fn url(&self) -> &'static str {
        // `latest=true` collapses the history to one current reading per station
        // (~120 nationwide) instead of the full ~8k-row observation backlog.
        "https://api.weather.gc.ca/collections/aqhi-observations-realtime/items?f=json&latest=true&limit=500"
    }
}

#[async_trait]
impl Source for EcccAqhi {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "eccc_aqhi",
            name: "ECCC Air Quality (Canada)",
            domain: EventKind::AirQuality,
            cadence: Duration::from_secs(600),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        let client = reqwest::Client::builder()
            .user_agent("engineering-effects/0.1 (+https://raithe.ca)")
            .build()?;
        let body = client.get(self.url()).send().await?.text().await?;
        parse_eccc_aqhi(&body)
    }
}

/// AQHI risk band label for a reading (Canada's official 1–10+ scale).
pub fn aqhi_risk(aqhi: f64) -> &'static str {
    match aqhi {
        a if a <= 3.0 => "Low risk",
        a if a <= 6.0 => "Moderate risk",
        a if a <= 10.0 => "High risk",
        _ => "Very high risk",
    }
}

/// Pure parser: AQHI observations GeoJSON -> events. Unit-tested offline.
///
/// Severity scales so clean air is a faint dot and smoke spikes are loud:
/// `(aqhi - 1) / 9`, clamped — AQHI 1 ≈ 0, AQHI 10+ ≈ 1.
pub fn parse_eccc_aqhi(json: &str) -> anyhow::Result<Vec<Event>> {
    let root: serde_json::Value = serde_json::from_str(json)?;
    let features = root
        .get("features")
        .and_then(|f| f.as_array())
        .ok_or_else(|| anyhow::anyhow!("eccc_aqhi: missing 'features' array"))?;

    let mut out = Vec::with_capacity(features.len());
    for f in features {
        let props = f.get("properties").cloned().unwrap_or(serde_json::Value::Null);

        let id = props
            .get("id")
            .and_then(|v| v.as_str())
            .or_else(|| f.get("id").and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string();
        if id.is_empty() {
            continue;
        }

        let geo = f
            .get("geometry")
            .filter(|g| g.get("type").and_then(|t| t.as_str()) == Some("Point"))
            .and_then(|g| g.get("coordinates"))
            .and_then(|c| c.as_array())
            .filter(|c| c.len() >= 2)
            .and_then(|c| match (c[0].as_f64(), c[1].as_f64()) {
                (Some(lon), Some(lat)) => Geo::new(lat, lon),
                _ => None,
            });
        let Some(geo) = geo else { continue };

        let aqhi = props.get("aqhi").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let place = props
            .get("location_name_en")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("Air quality station");
        // The AQHI value + risk band ride in the map popup's value chip
        // (osint::feed_detail); the title carries just the station/place.
        let title = place.to_string();

        let time = props
            .get("observation_datetime")
            .and_then(|d| d.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        out.push(Event {
            id: format!("aqhi-{id}"),
            source_id: "eccc_aqhi".to_string(),
            kind: EventKind::AirQuality,
            title,
            time,
            geo: Some(geo),
            severity: Severity::new(((aqhi - 1.0) / 9.0).clamp(0.0, 1.0)),
            url: Some("https://weather.gc.ca/airquality/pages/index_e.html".to_string()),
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
        {"type":"Feature","id":"f1",
         "geometry":{"type":"Point","coordinates":[-123.37,48.43]},
         "properties":{"id":"AQ_OBS-JBOBQ-1","aqhi":2.07,"location_name_en":"Victoria / Saanich",
           "observation_datetime":"2026-06-13T23:00:00Z","latest":true}},
        {"type":"Feature","id":"f2",
         "geometry":{"type":"Point","coordinates":[-114.07,51.05]},
         "properties":{"id":"AQ_OBS-CALG-1","aqhi":8.4,"location_name_en":"Calgary",
           "observation_datetime":"2026-06-13T23:00:00Z","latest":true}},
        {"type":"Feature","geometry":{"type":"Point","coordinates":[0,0]},"properties":{"aqhi":1}}
      ]
    }"#;

    #[test]
    fn parses_fixture() {
        let ev = parse_eccc_aqhi(FIXTURE).unwrap();
        // The id-less third feature is dropped.
        assert_eq!(ev.len(), 2);

        assert_eq!(ev[0].id, "aqhi-AQ_OBS-JBOBQ-1");
        assert_eq!(ev[0].kind, EventKind::AirQuality);
        assert_eq!(ev[0].title, "Victoria / Saanich");
        // AQHI 2.07 -> low severity; AQHI 8.4 -> high.
        assert!(ev[0].severity.value() < 0.15);
        assert_eq!(ev[1].title, "Calgary");
        assert!((ev[1].severity.value() - (8.4 - 1.0) / 9.0).abs() < 1e-9);
    }

    #[test]
    fn risk_bands() {
        assert_eq!(aqhi_risk(2.0), "Low risk");
        assert_eq!(aqhi_risk(5.0), "Moderate risk");
        assert_eq!(aqhi_risk(9.0), "High risk");
        assert_eq!(aqhi_risk(11.0), "Very high risk");
    }
}
