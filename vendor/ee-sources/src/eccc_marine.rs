//! Environment Canada marine weather — active warnings for Canadian marine zones,
//! including the Great Lakes (Lake Ontario/Erie/Huron/Superior) that ring Ontario.
//! Free, no API key.
//!
//! Reads the MSC GeoMet `marineweather-realtime` collection and emits a normalized
//! [`EventKind::Weather`] [`Event`] ONLY for zones with an IN-EFFECT warning, so the
//! layer carries real marine hazards rather than every calm forecast zone.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use std::time::Duration;

/// ECCC marine-warning source (Canada-wide; Great Lakes included).
#[derive(Default)]
pub struct EcccMarine;

impl EcccMarine {
    pub fn url(&self) -> &'static str {
        "https://api.weather.gc.ca/collections/marineweather-realtime/items?f=json&limit=400"
    }
}

#[async_trait]
impl Source for EcccMarine {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "eccc_marine",
            name: "ECCC Marine Warnings (Canada)",
            domain: EventKind::Weather,
            cadence: Duration::from_secs(600),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        let client = reqwest::Client::builder()
            .user_agent("engineering-effects/0.1 (+https://raithe.ca)")
            .build()?;
        let body = client.get(self.url()).send().await?.text().await?;
        parse_eccc_marine(&body)
    }
}

/// Centroid (mean vertex) of a Polygon/MultiPolygon coordinate tree.
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
    let mut pts = Vec::new();
    collect(geometry.get("coordinates")?, &mut pts);
    if pts.is_empty() {
        return None;
    }
    let (slon, slat) = pts.iter().fold((0.0, 0.0), |(a, b), (lo, la)| (a + lo, b + la));
    let n = pts.len() as f64;
    Geo::new(slat / n, slon / n)
}

/// First IN-EFFECT warning in a feature's `warnings.locations[].events[]`, as
/// `(event_name_en, location_name_en, type_en)`.
fn active_warning(props: &serde_json::Value) -> Option<(String, String, String)> {
    let locations = props.get("warnings")?.get("locations")?.as_array()?;
    for loc in locations {
        let loc_name = loc.get("name").and_then(|n| n.get("en")).and_then(|v| v.as_str()).unwrap_or("");
        if let Some(events) = loc.get("events").and_then(|e| e.as_array()) {
            for ev in events {
                let status = ev.get("status").and_then(|s| s.get("en")).and_then(|v| v.as_str()).unwrap_or("");
                if status.eq_ignore_ascii_case("IN EFFECT") {
                    let name = ev.get("name").and_then(|n| n.get("en")).and_then(|v| v.as_str()).unwrap_or("Marine warning");
                    let ty = ev.get("type").and_then(|t| t.get("en")).and_then(|v| v.as_str()).unwrap_or("warning");
                    return Some((name.to_string(), loc_name.to_string(), ty.to_string()));
                }
            }
        }
    }
    None
}

/// Operator chip for an active marine warning: the standardized mean-wind band the
/// named warning denotes, with units. ECCC marine wind-warning names map to fixed
/// thresholds a watch-floor operator won't carry by heart — a "Gale warning" is
/// 34–47 kn, a "Storm warning" 48–63 kn — so the chip turns the title's hazard word
/// into the actual wind speed it implies. Non-wind hazards (e.g. freezing spray) carry
/// no band, so they degrade to the alert tier (Warning/Watch) and still read.
pub fn warning_chip(raw: &serde_json::Value) -> Option<String> {
    let props = raw.get("properties")?;
    let (name, _zone, ty) = active_warning(props)?;
    let lname = name.to_ascii_lowercase();
    // ECCC mean-wind warning bands (excluding gusts), most severe first. "storm surge"
    // is a water-level hazard, not a wind band, so it's excluded from the "storm" match.
    let band = if lname.contains("hurricane force") {
        Some("≥64 kn winds")
    } else if lname.contains("storm") && !lname.contains("surge") {
        Some("48–63 kn winds")
    } else if lname.contains("gale") {
        Some("34–47 kn winds")
    } else if lname.contains("strong wind") {
        Some("20–33 kn winds")
    } else {
        None
    };
    Some(match band {
        Some(b) => b.to_string(),
        // Non-wind hazard: title-case the alert tier so the chip still says something.
        None => {
            let mut c = ty.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase(),
                None => return None,
            }
        }
    })
}

/// Pure parser: marine GeoJSON -> events for zones with an active warning. Offline-tested.
pub fn parse_eccc_marine(json: &str) -> anyhow::Result<Vec<Event>> {
    let root: serde_json::Value = serde_json::from_str(json)?;
    let features = root
        .get("features")
        .and_then(|f| f.as_array())
        .ok_or_else(|| anyhow::anyhow!("eccc_marine: missing 'features' array"))?;

    let mut out = Vec::new();
    for f in features {
        let props = f.get("properties").cloned().unwrap_or(serde_json::Value::Null);
        let Some((event_name, zone, ty)) = active_warning(&props) else { continue };
        let Some(geo) = f.get("geometry").filter(|g| !g.is_null()).and_then(centroid) else { continue };

        let title = if zone.is_empty() {
            event_name.clone()
        } else {
            format!("{event_name} — {zone}")
        };
        let severity = match ty.to_ascii_lowercase().as_str() {
            "warning" => 0.7,
            "watch" => 0.5,
            _ => 0.4,
        };
        let time = props
            .get("lastUpdated")
            .and_then(|d| d.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);
        let id = f
            .get("id")
            .and_then(|v| v.as_str().map(String::from).or_else(|| v.as_i64().map(|i| i.to_string())))
            .unwrap_or_else(|| format!("{zone}-{event_name}"));

        out.push(Event {
            id: format!("marine-{id}"),
            source_id: "eccc_marine".to_string(),
            kind: EventKind::Weather,
            title,
            time,
            geo: Some(geo),
            severity: Severity::new(severity),
            url: Some("https://weather.gc.ca/marine/index_e.html".to_string()),
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
        {"type":"Feature","id":"z1",
         "geometry":{"type":"Polygon","coordinates":[[[-79.5,43.3],[-79.5,43.9],[-76.5,43.9],[-76.5,43.3],[-79.5,43.3]]]},
         "properties":{"lastUpdated":"2026-06-14T04:00:00Z","warnings":{"locations":[
           {"name":{"en":"Lake Ontario"},"events":[
             {"name":{"en":"Strong wind warning"},"type":{"en":"warning"},"status":{"en":"IN EFFECT"}}]}]}}},
        {"type":"Feature","id":"z2",
         "geometry":{"type":"Polygon","coordinates":[[[-83,41],[-83,42],[-82,42],[-82,41],[-83,41]]]},
         "properties":{"warnings":{"locations":[]}}}
      ]
    }"#;

    #[test]
    fn emits_only_active_warnings() {
        let ev = parse_eccc_marine(FIXTURE).unwrap();
        // Only the zone with an IN EFFECT warning is emitted.
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].kind, EventKind::Weather);
        assert_eq!(ev[0].title, "Strong wind warning — Lake Ontario");
        assert!((ev[0].severity.value() - 0.7).abs() < 1e-9);
        // Centroid sits inside the Lake Ontario box.
        let g = ev[0].geo.unwrap();
        assert!((g.lat - 43.6).abs() < 0.3 && (g.lon + 78.0).abs() < 0.6);
    }

    #[test]
    fn errors_on_missing_array() {
        assert!(parse_eccc_marine(r#"{"x":1}"#).is_err());
    }

    #[test]
    fn wind_chip_maps_named_warning_to_its_band() {
        // The fixture's active warning is a "Strong wind warning" → its ECCC band.
        let ev = parse_eccc_marine(FIXTURE).unwrap();
        assert_eq!(warning_chip(&ev[0].raw).as_deref(), Some("20–33 kn winds"));

        // Gale / Storm / Hurricane-force escalate; "storm surge" is NOT a wind band.
        let band = |name: &str| {
            let raw = serde_json::json!({"properties":{"warnings":{"locations":[
                {"name":{"en":"Zone"},"events":[
                    {"name":{"en":name},"type":{"en":"warning"},"status":{"en":"IN EFFECT"}}]}]}}});
            warning_chip(&raw)
        };
        assert_eq!(band("Gale warning").as_deref(), Some("34–47 kn winds"));
        assert_eq!(band("Storm warning").as_deref(), Some("48–63 kn winds"));
        assert_eq!(band("Hurricane force wind warning").as_deref(), Some("≥64 kn winds"));
        // Non-wind hazard → alert tier, not a fabricated wind band.
        assert_eq!(band("Freezing spray warning").as_deref(), Some("Warning"));
        assert_eq!(band("Storm surge warning").as_deref(), Some("Warning"));
    }
}
