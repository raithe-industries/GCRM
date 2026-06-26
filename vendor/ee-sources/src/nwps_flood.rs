//! NOAA / NWS National Water Prediction Service (NWPS) — observed river flooding.
//! Free, no API key. U.S. Government public domain (credit "NOAA / NWS NWPS").
//!
//! Reads the NWS `water/riv_gauges` "Observed River Stages" map layer as GeoJSON —
//! a `FeatureCollection`, one `Point` feature per AHPS river gauge, carrying the
//! gauge's current **observed flood category** in `status`
//! (`action` / `minor` / `moderate` / `major`, plus the all-clear `no_flooding`
//! and undefined states). Emits one normalized [`EventKind::Weather`] [`Event`]
//! per gauge that is **at or above action (near-flood) stage** — every all-clear /
//! undefined gauge is dropped, so the layer carries only rivers actually flooding.
//! A network with no rivers in flood therefore yields zero events, not an error.
//!
//! Why this clears the signal-meaningfulness bar where a raw gauge level can't: the
//! `status` field is the **baseline-relative flood category** — NWS has already
//! compared the live stage against that gauge's own action/minor/moderate/major
//! flood thresholds, so the plotted value carries real operator meaning ("major
//! flooding") rather than an absolute "2.79 m" that's incomparable across rivers.
//! NOAA's Office of Water Prediction is the authoritative U.S. flood-forecast body,
//! and river flooding is a hazard no current GCRM feed carries.

use async_trait::async_trait;
use chrono::Utc;
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use serde_json::Value;
use std::time::Duration;

/// NOAA NWPS observed-river-flooding source.
#[derive(Default)]
pub struct NwpsFlood;

impl NwpsFlood {
    /// The "Observed River Stages" layer (id 0) of the NWS `water/riv_gauges`
    /// map service, queried as GeoJSON. This service replaced the retired
    /// `idpgis.ncep.noaa.gov` AHPS layer.
    pub fn url(&self) -> &'static str {
        "https://mapservices.weather.noaa.gov/eventdriven/rest/services/water/riv_gauges/MapServer/0/query"
    }
}

#[async_trait]
impl Source for NwpsFlood {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "nwps_flood",
            name: "NOAA NWPS river flooding",
            domain: EventKind::Weather,
            cadence: Duration::from_secs(1800),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        let client = reqwest::Client::builder()
            .user_agent("engineering-effects/0.1 (+https://raithe.ca)")
            .build()?;
        // Ask the service for only the gauges already at/above action stage; the
        // parser re-filters defensively in case the where-clause is ignored.
        let body = client
            .get(self.url())
            .query(&[
                ("where", "status IN ('major','moderate','minor','action')"),
                ("outFields", "*"),
                ("returnGeometry", "true"),
                ("f", "geojson"),
            ])
            .send()
            .await?
            .text()
            .await?;
        parse_nwps_flood(&body)
    }
}

/// The four flood categories we plot, most-severe first. Anything else (the
/// all-clear `no_flooding`, `not_defined`, `obs_not_current`, `low_threshold`,
/// `out_of_service`, …) carries no flood signal and is dropped.
fn severity_for_status(status: &str) -> Option<f64> {
    match status {
        "major" => Some(1.0),     // major flooding
        "moderate" => Some(0.8),  // moderate flooding
        "minor" => Some(0.55),    // minor flooding
        "action" => Some(0.35),   // near-flood / action stage
        _ => None,
    }
}

/// Human label for a flood category — the operator read behind the dot.
fn status_label(status: &str) -> &'static str {
    match status {
        "major" => "Major flooding",
        "moderate" => "Moderate flooding",
        "minor" => "Minor flooding",
        "action" => "Near flood stage",
        _ => "Flooding",
    }
}

/// Case-insensitive string lookup over a GeoJSON `properties` object (ArcGIS
/// field casing can vary between mirrors of the same layer).
fn prop_str<'a>(props: &'a Value, key: &str) -> Option<&'a str> {
    let obj = props.as_object()?;
    obj.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .and_then(|(_, v)| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Operator chip for a gauge: the flood category, e.g. "Major flooding".
/// `raw` is the feature's `properties`.
pub fn flood_chip(raw: &Value) -> Option<String> {
    let status = prop_str(raw, "status")?.to_ascii_lowercase();
    severity_for_status(&status).map(|_| status_label(&status).to_string())
}

/// Pure parser: NWPS `riv_gauges` observed-stage GeoJSON -> events. Unit-tested
/// offline. A missing `features` array is malformed (error); gauges not at/above
/// action stage are filtered out, so a no-flooding network is Ok/empty.
pub fn parse_nwps_flood(json: &str) -> anyhow::Result<Vec<Event>> {
    let root: Value = serde_json::from_str(json)?;
    let features = root
        .get("features")
        .and_then(|f| f.as_array())
        .ok_or_else(|| anyhow::anyhow!("nwps_flood: missing 'features' array"))?;

    let mut out = Vec::with_capacity(features.len());
    for f in features {
        let props = f.get("properties").cloned().unwrap_or(Value::Null);

        // Keep only gauges in an actual flood category (drops the all-clear and
        // undefined states), and ladder severity off that baseline-relative read.
        let Some(status) = prop_str(&props, "status").map(|s| s.to_ascii_lowercase()) else {
            continue;
        };
        let Some(sev) = severity_for_status(&status) else { continue };

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

        let lid = prop_str(&props, "gaugelid").unwrap_or("");
        if lid.is_empty() {
            continue;
        }
        // Prefer the human gauge location ("Red River at Fargo"); fall back to the
        // waterbody name, then the bare gauge id.
        let title = prop_str(&props, "location")
            .or_else(|| prop_str(&props, "waterbody"))
            .unwrap_or(lid)
            .to_string();

        let url = prop_str(&props, "url")
            .map(str::to_string)
            .unwrap_or_else(|| format!("https://water.noaa.gov/gauges/{lid}"));

        out.push(Event {
            id: format!("nwps-flood-{lid}"),
            source_id: "nwps_flood".to_string(),
            kind: EventKind::Weather,
            title,
            time: Utc::now(),
            geo: Some(geo),
            severity: Severity::new(sev),
            url: Some(url),
            raw: props,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Built to the confirmed `water/riv_gauges/MapServer/0` GeoJSON shape: a
    // FeatureCollection of Point gauges whose `properties.status` is the observed
    // AHPS flood category. One major, one moderate, one action (all kept), and one
    // `no_flooding` all-clear gauge (must be dropped).
    const FIXTURE: &str = r#"{
      "type": "FeatureCollection",
      "features": [
        {"type":"Feature","geometry":{"type":"Point","coordinates":[-96.7898,46.8772]},
         "properties":{"gaugelid":"FGON8","status":"major","location":"Red River at Fargo",
           "waterbody":"Red River of the North","state":"ND","units":"ft",
           "obstime":"2026-06-26T12:00:00Z","wfo":"FGF",
           "url":"https://water.noaa.gov/gauges/FGON8"}},
        {"type":"Feature","geometry":{"type":"Point","coordinates":[-90.1799,38.6286]},
         "properties":{"gaugelid":"EADM7","status":"moderate","location":"Mississippi River at St. Louis",
           "waterbody":"Mississippi River","state":"MO","units":"ft",
           "obstime":"2026-06-26T12:00:00Z","wfo":"LSX"}},
        {"type":"Feature","geometry":{"type":"Point","coordinates":[-95.9286,36.1539]},
         "properties":{"gaugelid":"TULO2","status":"action","location":"Arkansas River at Tulsa",
           "waterbody":"Arkansas River","state":"OK","units":"ft",
           "obstime":"2026-06-26T12:00:00Z","wfo":"TSA"}},
        {"type":"Feature","geometry":{"type":"Point","coordinates":[-77.0319,38.8951]},
         "properties":{"gaugelid":"WASD2","status":"no_flooding","location":"Potomac River at Washington",
           "waterbody":"Potomac River","state":"DC","units":"ft"}}
      ]
    }"#;

    #[test]
    fn parses_fixture_dropping_all_clear() {
        let ev = parse_nwps_flood(FIXTURE).unwrap();
        // The no_flooding gauge is filtered out; only the three in flood remain.
        assert_eq!(ev.len(), 3);

        assert_eq!(ev[0].id, "nwps-flood-FGON8");
        assert_eq!(ev[0].kind, EventKind::Weather);
        assert_eq!(ev[0].title, "Red River at Fargo");
        // major -> 1.0.
        assert!((ev[0].severity.value() - 1.0).abs() < 1e-9);
        let g = ev[0].geo.unwrap();
        assert!((g.lat - 46.8772).abs() < 1e-6 && (g.lon + 96.7898).abs() < 1e-6);
        assert_eq!(flood_chip(&ev[0].raw).as_deref(), Some("Major flooding"));
        assert_eq!(ev[0].url.as_deref(), Some("https://water.noaa.gov/gauges/FGON8"));

        // moderate -> 0.8; no url field -> synthesized from the gauge id.
        assert!((ev[1].severity.value() - 0.8).abs() < 1e-9);
        assert_eq!(ev[1].url.as_deref(), Some("https://water.noaa.gov/gauges/EADM7"));
        assert_eq!(flood_chip(&ev[1].raw).as_deref(), Some("Moderate flooding"));

        // action -> 0.35, labelled as near-flood stage.
        assert!((ev[2].severity.value() - 0.35).abs() < 1e-9);
        assert_eq!(flood_chip(&ev[2].raw).as_deref(), Some("Near flood stage"));
    }

    #[test]
    fn no_flood_network_is_ok_not_error() {
        // Every gauge below action stage -> zero plotted events, not a failure.
        let json = r#"{"type":"FeatureCollection","features":[
          {"type":"Feature","geometry":{"type":"Point","coordinates":[-77.03,38.89]},
           "properties":{"gaugelid":"WASD2","status":"no_flooding","location":"Potomac River"}},
          {"type":"Feature","geometry":{"type":"Point","coordinates":[-80.0,40.0]},
           "properties":{"gaugelid":"XYZA1","status":"not_defined","location":"Somewhere"}}
        ]}"#;
        assert!(parse_nwps_flood(json).unwrap().is_empty());
    }

    #[test]
    fn errors_on_bad_input() {
        // Missing the features array is malformed.
        assert!(parse_nwps_flood(r#"{"type":"FeatureCollection"}"#).is_err());
        // Not JSON at all (e.g. an HTML error page).
        assert!(parse_nwps_flood("<html>403 Forbidden</html>").is_err());
    }

    #[test]
    fn status_casing_is_tolerated() {
        // ArcGIS mirrors can return upper/mixed-case field names or values.
        let json = r#"{"type":"FeatureCollection","features":[
          {"type":"Feature","geometry":{"type":"Point","coordinates":[-96.79,46.88]},
           "properties":{"GAUGELID":"FGON8","STATUS":"Major","LOCATION":"Red River at Fargo"}}
        ]}"#;
        let ev = parse_nwps_flood(json).unwrap();
        assert_eq!(ev.len(), 1);
        assert!((ev[0].severity.value() - 1.0).abs() < 1e-9);
        assert_eq!(flood_chip(&ev[0].raw).as_deref(), Some("Major flooding"));
    }

    #[test]
    fn severity_ladders_with_category() {
        assert_eq!(severity_for_status("action"), Some(0.35));
        assert_eq!(severity_for_status("minor"), Some(0.55));
        assert_eq!(severity_for_status("moderate"), Some(0.8));
        assert_eq!(severity_for_status("major"), Some(1.0));
        assert_eq!(severity_for_status("no_flooding"), None);
    }
}
