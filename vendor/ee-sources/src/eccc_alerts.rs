//! Environment and Climate Change Canada (ECCC) weather alerts — the Canadian
//! counterpart to NWS. Free, no API key (a `User-Agent` is polite, not required).
//!
//! Parses the MSC GeoMet OGC-API `weather-alerts` collection
//! (<https://api.weather.gc.ca/collections/weather-alerts/items>) into normalized
//! [`EventKind::Weather`] [`Event`]s: warnings, watches, and statements across all of
//! Canada. The U.S.-only [`crate::nws`] feed leaves Canada blank; this fills it.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use std::time::Duration;

/// ECCC / MSC GeoMet weather-alert source (Canada-wide).
#[derive(Default)]
pub struct EcccAlerts;

impl EcccAlerts {
    pub fn url(&self) -> &'static str {
        // pygeoapi (OGC API - Features). `limit` is generous — there are rarely more
        // than a few hundred active alerts nationwide, and the map caps per-feed anyway.
        "https://api.weather.gc.ca/collections/weather-alerts/items?f=json&limit=600"
    }
}

#[async_trait]
impl Source for EcccAlerts {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "eccc_alerts",
            name: "ECCC Weather Alerts (Canada)",
            domain: EventKind::Weather,
            cadence: Duration::from_secs(300),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        let body = crate::http::fetch_text(self.url()).await?;
        parse_eccc_alerts(&body)
    }
}

/// Alert severity from ECCC's type + risk-colour fields. `warning` outranks `watch`
/// outranks `statement`; a red/orange risk colour lifts the floor regardless of type.
fn severity_for(alert_type: &str, risk_colour: &str) -> f64 {
    let mut s: f64 = match alert_type {
        "warning" => 0.7,
        "watch" => 0.5,
        _ => 0.35, // statement / advisory / unknown
    };
    match risk_colour.to_ascii_lowercase().as_str() {
        "red" => s = s.max(0.9),
        "orange" => s = s.max(0.72),
        _ => {}
    }
    s
}

/// Capitalize the first character (ECCC alert names arrive all-lowercase).
fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(f) => f.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Centroid (mean vertex) of a GeoJSON Polygon/MultiPolygon coordinate tree. ECCC
/// alert geometry is polygonal; we plot a single representative point per alert.
fn centroid(geometry: &serde_json::Value) -> Option<Geo> {
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

/// Pure parser: ECCC weather-alerts GeoJSON -> events. Unit-tested offline.
pub fn parse_eccc_alerts(json: &str) -> anyhow::Result<Vec<Event>> {
    let root: serde_json::Value = serde_json::from_str(json)?;
    let features = root
        .get("features")
        .and_then(|f| f.as_array())
        .ok_or_else(|| anyhow::anyhow!("eccc_alerts: missing 'features' array"))?;

    let mut out = Vec::with_capacity(features.len());
    for f in features {
        let props = f.get("properties").cloned().unwrap_or(serde_json::Value::Null);

        // Prefer the property id (stable alert id); fall back to the feature id.
        let id = props
            .get("id")
            .and_then(|v| v.as_str())
            .or_else(|| f.get("id").and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string();
        if id.is_empty() {
            continue;
        }

        let geo = f.get("geometry").filter(|g| !g.is_null()).and_then(centroid);

        let time = props
            .get("publication_datetime")
            .or_else(|| props.get("validity_datetime"))
            .or_else(|| props.get("event_end_datetime"))
            .and_then(|t| t.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        // ECCC names arrive lowercase ("rainfall warning"); capitalize so Canadian
        // alerts read consistently with the (already-capitalized) NWS alerts on the map.
        let name = capitalize_first(
            props.get("alert_name_en").and_then(|v| v.as_str()).unwrap_or("Weather Alert"),
        );
        let region = props
            .get("feature_name_en")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .or_else(|| props.get("province").and_then(|v| v.as_str()).filter(|s| !s.is_empty()));
        let title = match region {
            Some(r) => format!("{name} — {r}"),
            None => name.to_string(),
        };

        let alert_type = props.get("alert_type").and_then(|v| v.as_str()).unwrap_or("");
        let risk_colour = props.get("risk_colour_en").and_then(|v| v.as_str()).unwrap_or("");

        out.push(Event {
            id: format!("eccc-{id}"),
            source_id: "eccc_alerts".to_string(),
            kind: EventKind::Weather,
            title,
            time,
            geo,
            severity: Severity::new(severity_for(alert_type, risk_colour)),
            url: None,
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
        {"type":"Feature","id":"feat-1",
         "geometry":{"type":"Polygon","coordinates":[[[-114.0,51.0],[-114.0,52.0],[-113.0,52.0],[-113.0,51.0],[-114.0,51.0]]]},
         "properties":{"id":"alert.1","alert_name_en":"severe thunderstorm warning","alert_type":"warning",
           "risk_colour_en":"red","feature_name_en":"Calgary","province":"AB",
           "publication_datetime":"2026-06-13T18:00:00Z"}},
        {"type":"Feature","id":"feat-2","geometry":null,
         "properties":{"id":"alert.2","alert_name_en":"rainfall warning","alert_type":"watch",
           "risk_colour_en":"yellow","province":"BC","publication_datetime":"2026-06-13T12:00:00Z"}},
        {"type":"Feature","geometry":null,"properties":{"alert_name_en":"x"}}
      ]
    }"#;

    #[test]
    fn parses_fixture() {
        let ev = parse_eccc_alerts(FIXTURE).unwrap();
        // The id-less third feature is dropped.
        assert_eq!(ev.len(), 2);

        assert_eq!(ev[0].id, "eccc-alert.1");
        assert_eq!(ev[0].kind, EventKind::Weather);
        assert_eq!(ev[0].title, "Severe thunderstorm warning — Calgary");
        // warning + red risk colour -> top of the band.
        assert!((ev[0].severity.value() - 0.9).abs() < 1e-9);
        // Polygon centroid sits inside the box.
        let g = ev[0].geo.unwrap();
        assert!((g.lat - 51.5).abs() < 0.5 && (g.lon + 113.5).abs() < 0.5);

        // Null geometry -> no geo; falls back to province for the region label.
        assert!(ev[1].geo.is_none());
        assert_eq!(ev[1].title, "Rainfall warning — BC");
        assert!((ev[1].severity.value() - 0.5).abs() < 1e-9); // watch + yellow
    }

    #[test]
    fn errors_on_missing_array() {
        assert!(parse_eccc_alerts(r#"{"type":"x"}"#).is_err());
    }
}
