//! NOAA National Hurricane Center — active tropical cyclones (Atlantic, Eastern &
//! Central Pacific). Free, no API key. U.S. Government work / public domain.
//!
//! Reads the NHC `CurrentStorms.json` product — a top-level object with an
//! `activeStorms` array, one entry per live system, carrying the current position
//! (`latitudeNumeric` / `longitudeNumeric`), `classification` (HU/TS/TD/…), max
//! sustained wind `intensity` (kt), `pressure` (mb), and `movementDir`/`movementSpeed`.
//! Emits one normalized [`EventKind::Weather`] [`Event`] per active storm. An empty
//! `activeStorms` list — the normal state outside an active basin — yields zero events,
//! not an error, so the layer simply lights up when storms form.
//!
//! NHC is the authoritative primary source for Atlantic/E-Pacific tropical cyclones,
//! with 6-hourly advisory cadence and live intensity/category — coverage the EONET
//! severe-storm catalogue (lagging, less operational) and GDACS alert levels don't carry.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use serde_json::Value;
use std::time::Duration;

/// NOAA NHC active-tropical-cyclone source.
#[derive(Default)]
pub struct Nhc;

impl Nhc {
    pub fn url(&self) -> &'static str {
        "https://www.nhc.noaa.gov/CurrentStorms.json"
    }
}

#[async_trait]
impl Source for Nhc {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "nhc",
            name: "NHC Tropical Cyclones",
            domain: EventKind::Weather,
            cadence: Duration::from_secs(1800),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        let body = crate::http::fetch_text(self.url()).await?;
        parse_nhc(&body)
    }
}

/// Human label for an NHC `classification` code (the chip/title shouldn't show a raw
/// two-letter code). Unknown codes pass through so a new code still reads.
pub fn classification_label(code: &str) -> &str {
    match code {
        "HU" => "Hurricane",
        "MH" => "Major Hurricane",
        "TS" => "Tropical Storm",
        "TD" => "Tropical Depression",
        "STS" => "Subtropical Storm",
        "SD" | "STD" => "Subtropical Depression",
        "SS" => "Subtropical Storm",
        "PTC" => "Potential Tropical Cyclone",
        "PC" | "EX" => "Post-Tropical Cyclone",
        "DB" | "LO" | "WV" => "Tropical Disturbance",
        other => other,
    }
}

/// Saffir–Simpson category (1–5) for a max-sustained-wind value in knots, or `None`
/// below hurricane strength (<64 kt).
pub fn saffir_category(kt: f64) -> Option<u8> {
    Some(match kt {
        k if k >= 137.0 => 5,
        k if k >= 113.0 => 4,
        k if k >= 96.0 => 3,
        k if k >= 83.0 => 2,
        k if k >= 64.0 => 1,
        _ => return None,
    })
}

/// Normalized 0–1 severity from max-sustained wind (kt), falling back to the
/// classification when intensity is absent.
fn severity_for(class: &str, kt: Option<f64>) -> f64 {
    if let Some(kt) = kt {
        return match kt {
            k if k >= 137.0 => 1.0,
            k if k >= 113.0 => 0.9,
            k if k >= 96.0 => 0.8,
            k if k >= 83.0 => 0.7,
            k if k >= 64.0 => 0.6,
            k if k >= 34.0 => 0.45,
            _ => 0.3,
        };
    }
    match class {
        "HU" | "MH" => 0.6,
        "TS" | "STS" | "SS" => 0.45,
        "TD" | "SD" | "STD" => 0.3,
        _ => 0.4,
    }
}

/// Operator chip for an active cyclone: the classification (with Saffir–Simpson
/// category for hurricane-strength systems) plus max sustained wind in knots —
/// e.g. "Hurricane Cat 1 · 75 kt", "Tropical Storm · 45 kt".
pub fn storm_chip(raw: &Value) -> Option<String> {
    let class = raw.get("classification").and_then(Value::as_str).unwrap_or("");
    let kt = raw.get("intensity").and_then(Value::as_f64);
    let mut label = classification_label(class).to_string();
    if class == "HU" || class == "MH" {
        if let Some(cat) = kt.and_then(saffir_category) {
            label = format!("{label} Cat {cat}");
        }
    }
    match kt {
        Some(kt) if kt > 0.0 => Some(format!("{label} · {kt:.0} kt")),
        _ if !label.is_empty() => Some(label),
        _ => None,
    }
}

/// Read a JSON value as f64 whether it's a number or a numeric string ("75").
fn num(v: Option<&Value>) -> Option<f64> {
    v.and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())))
}

/// Coordinate from the numeric field, falling back to the signed-text form
/// ("20.3N" / "148.8W").
fn parse_coord(numeric: Option<&Value>, text: Option<&Value>) -> Option<f64> {
    if let Some(n) = numeric.and_then(Value::as_f64) {
        return Some(n);
    }
    let s = text.and_then(Value::as_str)?.trim();
    let (num, dir) = s.split_at(s.len().checked_sub(1)?);
    let v: f64 = num.trim().parse().ok()?;
    Some(match dir {
        "S" | "W" | "s" | "w" => -v,
        _ => v,
    })
}

/// Pure parser: NHC `CurrentStorms.json` -> events. Unit-tested offline. An absent
/// `activeStorms` array is malformed (error); an empty one is the normal off-season
/// case (Ok, zero events).
pub fn parse_nhc(json: &str) -> anyhow::Result<Vec<Event>> {
    let root: Value = serde_json::from_str(json)?;
    let storms = root
        .get("activeStorms")
        .and_then(|s| s.as_array())
        .ok_or_else(|| anyhow::anyhow!("nhc: missing 'activeStorms' array"))?;

    let mut out = Vec::with_capacity(storms.len());
    for s in storms {
        let lat = parse_coord(s.get("latitudeNumeric"), s.get("latitude"));
        let lon = parse_coord(s.get("longitudeNumeric"), s.get("longitude"));
        let (Some(lat), Some(lon)) = (lat, lon) else { continue };
        let Some(geo) = Geo::new(lat, lon) else { continue };

        let id = s
            .get("id")
            .and_then(Value::as_str)
            .or_else(|| s.get("binNumber").and_then(Value::as_str))
            .unwrap_or("");
        if id.is_empty() {
            continue;
        }

        let class = s.get("classification").and_then(Value::as_str).unwrap_or("");
        let name = s.get("name").and_then(Value::as_str).unwrap_or("").trim();
        let kt = num(s.get("intensity"));
        let label = classification_label(class);
        let title = if name.is_empty() {
            label.to_string()
        } else {
            format!("{label} {name}")
        };

        let time = s
            .get("lastUpdate")
            .and_then(Value::as_str)
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        out.push(Event {
            id: format!("nhc-{id}"),
            source_id: "nhc".to_string(),
            kind: EventKind::Weather,
            title,
            time,
            geo: Some(geo),
            severity: Severity::new(severity_for(class, kt)),
            url: Some("https://www.nhc.noaa.gov/".to_string()),
            raw: serde_json::json!({
                "classification": class,
                "name": name,
                "intensity": kt,
                "pressure": num(s.get("pressure")),
                "movementDir": s.get("movementDir").cloned().unwrap_or(Value::Null),
                "movementSpeed": s.get("movementSpeed").cloned().unwrap_or(Value::Null),
            }),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Built from real NHC CurrentStorms.json output (storms Kiko/Dexter/Ivo). The
    // third entry (Ivo) omits the numeric coordinate fields to exercise the
    // signed-text ("15.9N"/"103.0W") coordinate fallback.
    const FIXTURE: &str = r#"{
      "activeStorms": [
        {"id":"ep112025","binNumber":"CP4","name":"Kiko","classification":"HU","intensity":"75",
         "pressure":"984","latitude":"20.3N","longitude":"148.8W","latitudeNumeric":20.3,
         "longitudeNumeric":-148.8,"movementDir":300,"movementSpeed":15,"lastUpdate":"2025-09-08T15:00:00.000Z"},
        {"id":"al042025","binNumber":"AT4","name":"Dexter","classification":"TS","intensity":"45",
         "pressure":"998","latitude":"40.6N","longitude":"52.1W","latitudeNumeric":40.6,
         "longitudeNumeric":-52.1,"movementDir":70,"movementSpeed":18,"lastUpdate":"2025-08-07T09:00:00.000Z"},
        {"id":"ep092025","name":"Ivo","classification":"TD","intensity":"30",
         "latitude":"15.9N","longitude":"103.0W","movementDir":295,"movementSpeed":23,
         "lastUpdate":"2025-08-07T09:00:00.000Z"}
      ]
    }"#;

    #[test]
    fn parses_fixture() {
        let ev = parse_nhc(FIXTURE).unwrap();
        assert_eq!(ev.len(), 3);

        assert_eq!(ev[0].id, "nhc-ep112025");
        assert_eq!(ev[0].kind, EventKind::Weather);
        assert_eq!(ev[0].title, "Hurricane Kiko");
        // 75 kt = Cat 1 -> 0.6.
        assert!((ev[0].severity.value() - 0.6).abs() < 1e-9);
        let g = ev[0].geo.unwrap();
        assert!((g.lat - 20.3).abs() < 1e-6 && (g.lon + 148.8).abs() < 1e-6);
        assert_eq!(storm_chip(&ev[0].raw).as_deref(), Some("Hurricane Cat 1 · 75 kt"));

        assert_eq!(ev[1].title, "Tropical Storm Dexter");
        assert!((ev[1].severity.value() - 0.45).abs() < 1e-9);
        assert_eq!(storm_chip(&ev[1].raw).as_deref(), Some("Tropical Storm · 45 kt"));

        // Ivo: no numeric coords -> parsed from "15.9N"/"103.0W".
        let g2 = ev[2].geo.unwrap();
        assert!((g2.lat - 15.9).abs() < 1e-6 && (g2.lon + 103.0).abs() < 1e-6);
        assert!((ev[2].severity.value() - 0.3).abs() < 1e-9);
        assert_eq!(storm_chip(&ev[2].raw).as_deref(), Some("Tropical Depression · 30 kt"));
    }

    #[test]
    fn empty_active_storms_is_ok_not_error() {
        // Off-season / quiet-basin: an empty list is the normal state, not a failure.
        let ev = parse_nhc(r#"{"activeStorms":[]}"#).unwrap();
        assert!(ev.is_empty());
    }

    #[test]
    fn errors_on_bad_input() {
        // Missing the activeStorms array is malformed.
        assert!(parse_nhc(r#"{"foo":1}"#).is_err());
        // Not JSON at all.
        assert!(parse_nhc("<html>403</html>").is_err());
    }

    #[test]
    fn saffir_category_bands() {
        assert_eq!(saffir_category(50.0), None); // below hurricane strength
        assert_eq!(saffir_category(64.0), Some(1));
        assert_eq!(saffir_category(96.0), Some(3));
        assert_eq!(saffir_category(140.0), Some(5));
    }

    #[test]
    fn major_hurricane_chip_carries_category() {
        let raw = serde_json::json!({"classification":"HU","intensity":130.0});
        // 130 kt = Cat 4.
        assert_eq!(storm_chip(&raw).as_deref(), Some("Hurricane Cat 4 · 130 kt"));
    }
}
