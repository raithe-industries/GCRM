//! GeoNet (GNS Science / Toka Tū Ake) — New Zealand Volcanic Alert Levels. Free,
//! no API key. CC BY 3.0 NZ (credit "GeoNet / GNS Science").
//!
//! Reads the GeoNet `volcano/val` product — a GeoJSON `FeatureCollection`, one
//! `Point` feature per monitored NZ volcano, carrying the current official
//! **Volcanic Alert Level** (`level`, 0–5), the ICAO **Aviation Colour Code**
//! (`acc`: Green/Yellow/Orange/Red), and plain-language `activity` / `hazards`.
//! Emits one normalized [`EventKind::Volcano`] [`Event`] per volcano **at level ≥ 1**
//! — the all-clear state (VAL 0, "no volcanic unrest") is dropped so the layer
//! carries only volcanoes in actual unrest/eruption. An all-quiet network (every
//! volcano at 0) therefore yields zero events, not an error.
//!
//! GeoNet is New Zealand's official geological-hazard monitor; its VAL is the
//! authoritative operational alert state for NZ volcanoes (Ruapehu, Whakaari/White
//! Island, Tongariro, …). That standardized alert level + aviation colour code is
//! coverage the Smithsonian GVP eruption catalogue and NASA EONET (event-based,
//! global) don't carry, and NZ / the SW Pacific is otherwise sparse on the map.

use async_trait::async_trait;
use chrono::Utc;
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use serde_json::Value;
use std::time::Duration;

/// GeoNet NZ Volcanic-Alert-Level source.
#[derive(Default)]
pub struct GeonetVolcano;

impl GeonetVolcano {
    pub fn url(&self) -> &'static str {
        "https://api.geonet.org.nz/volcano/val"
    }
}

#[async_trait]
impl Source for GeonetVolcano {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "geonet_volcano",
            name: "GeoNet NZ Volcanic Alert Levels",
            domain: EventKind::Volcano,
            cadence: Duration::from_secs(1800),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        let client = reqwest::Client::builder()
            .user_agent("engineering-effects/0.1 (+https://raithe.ca)")
            .build()?;
        // GeoNet content-negotiates by Accept header; v2 is the current GeoJSON form.
        let body = client
            .get(self.url())
            .header("Accept", "application/vnd.geo+json;version=2")
            .send()
            .await?
            .text()
            .await?;
        parse_geonet_val(&body)
    }
}

/// Normalized 0–1 severity from the NZ Volcanic Alert Level (1–5). Level 0 (no
/// unrest) is dropped upstream and never reaches here.
fn severity_for_level(level: i64) -> f64 {
    match level {
        l if l >= 5 => 1.0, // major eruption
        4 => 0.9,           // moderate eruption
        3 => 0.75,          // minor eruption
        2 => 0.55,          // moderate to heightened unrest
        _ => 0.35,          // 1: minor unrest
    }
}

/// Operator chip for a volcano: the alert level plus the aviation colour code,
/// e.g. "Alert Level 2 · Aviation Orange". `raw` is the feature's `properties`.
pub fn val_chip(raw: &Value) -> Option<String> {
    let level = raw.get("level").and_then(Value::as_f64)?;
    let head = format!("Alert Level {}", level as i64);
    let acc = raw
        .get("acc")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("none"));
    Some(match acc {
        Some(c) => format!("{head} · Aviation {c}"),
        None => head,
    })
}

/// Pure parser: GeoNet `volcano/val` GeoJSON -> events. Unit-tested offline. A
/// missing `features` array is malformed (error); volcanoes at level 0 (the
/// normal all-clear state) are filtered out, so an all-quiet network is Ok/empty.
pub fn parse_geonet_val(json: &str) -> anyhow::Result<Vec<Event>> {
    let root: Value = serde_json::from_str(json)?;
    let features = root
        .get("features")
        .and_then(|f| f.as_array())
        .ok_or_else(|| anyhow::anyhow!("geonet_volcano: missing 'features' array"))?;

    let mut out = Vec::with_capacity(features.len());
    for f in features {
        let props = f.get("properties").cloned().unwrap_or(Value::Null);

        // Drop the all-clear state (VAL 0 = no volcanic unrest): a level-0 dot
        // carries no risk signal, so the layer shows only volcanoes in unrest.
        let Some(level) = props.get("level").and_then(Value::as_f64) else { continue };
        let level = level.round() as i64;
        if level < 1 {
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

        let vid = props.get("volcanoID").and_then(Value::as_str).unwrap_or("");
        if vid.is_empty() {
            continue;
        }
        let name = props
            .get("volcanoTitle")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(vid);

        out.push(Event {
            id: format!("geonet-val-{vid}"),
            source_id: "geonet_volcano".to_string(),
            kind: EventKind::Volcano,
            title: name.to_string(),
            time: Utc::now(),
            geo: Some(geo),
            severity: Severity::new(severity_for_level(level)),
            url: Some("https://www.geonet.org.nz/volcano".to_string()),
            raw: props,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Built from the real GeoNet volcano/val GeoJSON shape: White Island (heightened
    // unrest, level 2 / Orange), Ruapehu (minor unrest, level 1, no aviation colour),
    // and Taupo at the all-clear level 0 (must be dropped).
    const FIXTURE: &str = r#"{
      "type": "FeatureCollection",
      "features": [
        {"type":"Feature","geometry":{"type":"Point","coordinates":[177.183,-37.521]},
         "properties":{"volcanoID":"whiteisland","volcanoTitle":"White Island","level":2,
           "acc":"Orange","activity":"Moderate to heightened volcanic unrest.",
           "hazards":"Volcanic unrest hazards, potential for eruption hazards."}},
        {"type":"Feature","geometry":{"type":"Point","coordinates":[175.563,-39.281]},
         "properties":{"volcanoID":"ruapehu","volcanoTitle":"Ruapehu","level":1,
           "acc":"","activity":"Minor volcanic unrest.","hazards":"Volcanic unrest hazards."}},
        {"type":"Feature","geometry":{"type":"Point","coordinates":[176.0,-38.82]},
         "properties":{"volcanoID":"taupo","volcanoTitle":"Taupo","level":0,
           "acc":"Green","activity":"No volcanic unrest.","hazards":"Volcanic environment hazards."}}
      ]
    }"#;

    #[test]
    fn parses_fixture_dropping_all_clear() {
        let ev = parse_geonet_val(FIXTURE).unwrap();
        // Taupo (level 0) is filtered out; only the two volcanoes in unrest remain.
        assert_eq!(ev.len(), 2);

        assert_eq!(ev[0].id, "geonet-val-whiteisland");
        assert_eq!(ev[0].kind, EventKind::Volcano);
        assert_eq!(ev[0].title, "White Island");
        // Level 2 -> 0.55.
        assert!((ev[0].severity.value() - 0.55).abs() < 1e-9);
        let g = ev[0].geo.unwrap();
        assert!((g.lat + 37.521).abs() < 1e-6 && (g.lon - 177.183).abs() < 1e-6);
        assert_eq!(val_chip(&ev[0].raw).as_deref(), Some("Alert Level 2 · Aviation Orange"));

        assert_eq!(ev[1].title, "Ruapehu");
        // Level 1 -> 0.35; empty acc -> chip carries the level alone.
        assert!((ev[1].severity.value() - 0.35).abs() < 1e-9);
        assert_eq!(val_chip(&ev[1].raw).as_deref(), Some("Alert Level 1"));
    }

    #[test]
    fn all_quiet_network_is_ok_not_error() {
        // Every monitored volcano at level 0 -> zero plotted events, not a failure.
        let json = r#"{"type":"FeatureCollection","features":[
          {"type":"Feature","geometry":{"type":"Point","coordinates":[176.0,-38.82]},
           "properties":{"volcanoID":"taupo","volcanoTitle":"Taupo","level":0,"acc":"Green"}}
        ]}"#;
        assert!(parse_geonet_val(json).unwrap().is_empty());
    }

    #[test]
    fn errors_on_bad_input() {
        // Missing the features array is malformed.
        assert!(parse_geonet_val(r#"{"type":"FeatureCollection"}"#).is_err());
        // Not JSON at all.
        assert!(parse_geonet_val("<html>403 Forbidden</html>").is_err());
    }

    #[test]
    fn severity_ladders_with_level() {
        assert!((severity_for_level(1) - 0.35).abs() < 1e-9);
        assert!((severity_for_level(3) - 0.75).abs() < 1e-9);
        assert!((severity_for_level(5) - 1.0).abs() < 1e-9);
    }
}
