//! GDACS — the Global Disaster Alert and Coordination System. Free, no API key.
//!
//! Parses GDACS's public event-list GeoJSON
//! (<https://www.gdacs.org/gdacsapi/api/events/geteventlist/MAP>) into normalized
//! [`Event`]s. GDACS aggregates multiple hazard types (earthquakes, tropical
//! cyclones, floods, droughts, volcanoes, wildfires), so one connector populates
//! several map layers at once.

use async_trait::async_trait;
use chrono::{NaiveDateTime, TimeZone, Utc};
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use std::time::Duration;

/// GDACS multi-hazard disaster source.
#[derive(Default)]
pub struct Gdacs;

impl Gdacs {
    pub fn url(&self) -> &'static str {
        "https://www.gdacs.org/gdacsapi/api/events/geteventlist/MAP"
    }
}

#[async_trait]
impl Source for Gdacs {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "gdacs",
            name: "GDACS Global Disaster Alerts",
            // GDACS spans several hazard kinds; `Weather` is the closest single
            // domain for the disaster mix (per-event kind is set precisely below).
            domain: EventKind::Weather,
            cadence: Duration::from_secs(900),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        let client = reqwest::Client::builder()
            .user_agent("engineering-effects/0.1 (+https://raithe.ca)")
            .build()?;
        let body = client.get(self.url()).send().await?.text().await?;
        parse_gdacs(&body)
    }
}

/// Map a GDACS `eventtype` code to our [`EventKind`] taxonomy.
fn kind_for(eventtype: &str) -> EventKind {
    match eventtype {
        "EQ" => EventKind::Earthquake,             // earthquake
        "WF" => EventKind::Wildfire,               // wildfire
        "TC" | "FL" | "DR" => EventKind::Weather,  // cyclone / flood / drought
        _ => EventKind::Other,                     // VO (volcano), etc.
    }
}

/// Map a GDACS alert level to normalized severity.
fn severity_for(alertlevel: &str) -> f64 {
    match alertlevel {
        "Red" => 0.9,
        "Orange" => 0.6,
        _ => 0.3, // Green / unknown
    }
}

/// Pure parser: GDACS event-list GeoJSON -> events. Unit-tested offline.
pub fn parse_gdacs(json: &str) -> anyhow::Result<Vec<Event>> {
    let root: serde_json::Value = serde_json::from_str(json)?;
    let features = root
        .get("features")
        .and_then(|f| f.as_array())
        .ok_or_else(|| anyhow::anyhow!("gdacs: missing 'features' array"))?;

    let mut out = Vec::with_capacity(features.len());
    for f in features {
        let props = f.get("properties").cloned().unwrap_or(serde_json::Value::Null);

        let eventtype = props.get("eventtype").and_then(|v| v.as_str()).unwrap_or("");
        let eventid = props
            .get("eventid")
            .map(|v| v.to_string().trim_matches('"').to_string())
            .unwrap_or_default();
        if eventtype.is_empty() || eventid.is_empty() {
            continue; // no stable identity
        }

        // geometry.coordinates = [lon, lat]
        let geo = f
            .get("geometry")
            .and_then(|g| g.get("coordinates"))
            .and_then(|c| c.as_array())
            .filter(|c| c.len() >= 2)
            .and_then(|c| match (c[0].as_f64(), c[1].as_f64()) {
                (Some(lon), Some(lat)) => Geo::new(lat, lon),
                _ => None,
            });

        // fromdate is a naive UTC timestamp ("YYYY-MM-DDTHH:MM:SS").
        let time = props
            .get("fromdate")
            .and_then(|d| d.as_str())
            .and_then(|s| NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").ok())
            .map(|dt| Utc.from_utc_datetime(&dt))
            .unwrap_or_else(Utc::now);

        let alertlevel = props.get("alertlevel").and_then(|v| v.as_str()).unwrap_or("Green");

        // Prefer a human name; fall back to the description, then a synthesized label.
        let title = props
            .get("eventname")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from)
            .or_else(|| {
                props
                    .get("htmldescription")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.trim().to_string())
            })
            .unwrap_or_else(|| {
                let country = props.get("country").and_then(|v| v.as_str()).unwrap_or("");
                format!("{eventtype} event {country}").trim().to_string()
            });

        let url = props.get("url").and_then(|u| u.as_str()).map(String::from);

        out.push(Event {
            id: format!("gdacs-{eventtype}-{eventid}"),
            source_id: "gdacs".to_string(),
            kind: kind_for(eventtype),
            title,
            time,
            geo,
            severity: Severity::new(severity_for(alertlevel)),
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
        {"type":"Feature",
         "geometry":{"type":"Point","coordinates":[120.5,23.1]},
         "properties":{"eventtype":"TC","eventid":1001,"eventname":"Typhoon Test",
           "htmldescription":"Orange cyclone","alertlevel":"Orange",
           "fromdate":"2026-06-01T00:00:00","country":"Taiwan",
           "url":"https://gdacs.org/x"}},
        {"type":"Feature",
         "geometry":{"type":"Point","coordinates":[-95.7,39.4]},
         "properties":{"eventtype":"FL","eventid":1002,"eventname":"",
           "htmldescription":"Green Flood in United States","alertlevel":"Green",
           "fromdate":"2026-05-19T01:00:00","country":"United States"}},
        {"type":"Feature",
         "geometry":{"type":"Point","coordinates":[0,0]},
         "properties":{"eventtype":"","eventid":0}}
      ]
    }"#;

    #[test]
    fn parses_fixture() {
        let ev = parse_gdacs(FIXTURE).unwrap();
        // The third (typeless) entry is dropped.
        assert_eq!(ev.len(), 2);

        assert_eq!(ev[0].id, "gdacs-TC-1001");
        assert_eq!(ev[0].kind, EventKind::Weather);
        assert_eq!(ev[0].title, "Typhoon Test");
        assert!((ev[0].severity.value() - 0.6).abs() < 1e-9); // Orange
        let g = ev[0].geo.unwrap();
        assert!((g.lat - 23.1).abs() < 1e-9 && (g.lon - 120.5).abs() < 1e-9);
        assert_eq!(ev[0].time.format("%Y-%m-%d").to_string(), "2026-06-01");

        // Empty name falls back to the html description; Green -> baseline severity.
        assert_eq!(ev[1].title, "Green Flood in United States");
        assert!((ev[1].severity.value() - 0.3).abs() < 1e-9);
    }

    #[test]
    fn errors_on_missing_array() {
        assert!(parse_gdacs(r#"{"type":"x"}"#).is_err());
    }
}
