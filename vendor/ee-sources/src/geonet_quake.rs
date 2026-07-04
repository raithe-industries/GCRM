//! GeoNet (GNS Science / Toka Tū Ake) — New Zealand **felt earthquakes**, graded by
//! computed **Modified Mercalli Intensity (MMI)**. Free, no API key. CC BY 3.0 NZ
//! (credit "GeoNet / GNS Science").
//!
//! Reads the GeoNet `quake` product filtered to `?MMI=3` — a GeoJSON
//! `FeatureCollection`, one `Point` feature per recent NZ quake whose **computed MMI
//! shaking at the closest locality** reaches the felt threshold. Each feature carries
//! `publicID`, `time`, `depth`, `magnitude`, `mmi` (the calculated intensity, an
//! integer on the MMI scale), `locality` (distance/direction to the nearest place),
//! and `quality` (best / preliminary / automatic / deleted).
//!
//! This is deliberately NOT another raw detection catalogue like USGS/EMSC/eqcanada
//! (every instrument-detected event, magnitude only). Filtered to a computed felt MMI,
//! it is a **human-impact** product — only quakes that actually shook people — over
//! **New Zealand / the SW Pacific**, a seismically very active plate boundary the
//! global catalogues carry only sparsely at small magnitudes. It's the seismic sibling
//! of `bmkg_quake` (Indonesia MMI) and `jma_quake` (Japan Shindo): same felt-intensity
//! modality, different authoritative national body and geography.
//!
//! One normalized [`EventKind::Earthquake`] [`Event`] per felt quake at its own lat/lon
//! (inline Point geometry — no external join). Retracted quakes (`quality == "deleted"`)
//! and any feature below the felt MMI floor / without geometry are dropped, so a quiet
//! window (empty `features`) is Ok/empty, not an error.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use serde_json::Value;
use std::time::Duration;

/// Felt-intensity floor: GeoNet computes MMI at the closest locality; MMI 3 ("weak",
/// felt indoors) is the lowest rung people report feeling. Querying `?MMI=3` filters
/// server-side; the parser re-applies the floor so the semantics hold regardless.
const FELT_MMI_FLOOR: i64 = 3;

/// GeoNet NZ felt-earthquake source.
#[derive(Default)]
pub struct GeonetQuake;

impl GeonetQuake {
    pub fn url(&self) -> &'static str {
        // `MMI` is a required query parameter on this endpoint; 3 = the felt threshold.
        "https://api.geonet.org.nz/quake?MMI=3"
    }
}

#[async_trait]
impl Source for GeonetQuake {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "geonet_quake",
            name: "GeoNet NZ Felt Earthquakes (MMI)",
            domain: EventKind::Earthquake,
            cadence: Duration::from_secs(600),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        // GeoNet content-negotiates by Accept header; v2 is the current GeoJSON form.
        let body = crate::http::checked(
            crate::http::client()
                .get(self.url())
                .header("Accept", "application/vnd.geo+json;version=2")
                .send()
                .await?,
        )?
        .text()
        .await?;
        parse_geonet_quake(&body)
    }
}

/// Normalized 0–1 severity from the computed MMI. Aligned with the `bmkg_quake` MMI
/// ladder (VI → 0.7, IX+ → 1.0) so the same felt intensity reads the same across the
/// seismic feeds. Sub-felt intensities are dropped upstream and never reach here.
fn severity_for_mmi(mmi: i64) -> f64 {
    match mmi {
        m if m >= 9 => 1.0,  // violent / extreme
        8 => 0.95,           // severe
        7 => 0.85,           // very strong (damage begins)
        6 => 0.7,            // strong (felt by all)
        5 => 0.55,           // moderate
        4 => 0.4,            // light
        _ => 0.3,            // 3: weak (felt threshold)
    }
}

/// Operator chip for a felt quake: the intensity + magnitude, e.g. "Felt MMI 5 · M5.9".
/// MMI is a defined ground-shaking scale (each level a named human effect), so the value
/// is baseline-relative and unit-bearing — not a raw number. `raw` is the feature's
/// `properties`.
pub fn quake_chip(raw: &Value) -> Option<String> {
    let mmi = raw.get("mmi").and_then(Value::as_f64)?.round() as i64;
    let head = format!("Felt MMI {mmi}");
    match raw.get("magnitude").and_then(Value::as_f64) {
        Some(mag) => Some(format!("{head} · M{mag:.1}")),
        None => Some(head),
    }
}

/// Pure parser: GeoNet `quake` GeoJSON -> events. Unit-tested offline. A missing
/// `features` array is malformed (error). Quakes below the felt MMI floor, retracted
/// (`quality == "deleted"`), or lacking Point geometry are dropped, so a quiet window
/// (empty features) parses to Ok/empty rather than erroring.
pub fn parse_geonet_quake(json: &str) -> anyhow::Result<Vec<Event>> {
    let root: Value = serde_json::from_str(json)?;
    let features = root
        .get("features")
        .and_then(|f| f.as_array())
        .ok_or_else(|| anyhow::anyhow!("geonet_quake: missing 'features' array"))?;

    let mut out = Vec::with_capacity(features.len());
    for f in features {
        let props = f.get("properties").cloned().unwrap_or(Value::Null);

        // A retracted quake carries no risk signal — drop it.
        let quality = props.get("quality").and_then(Value::as_str).unwrap_or("");
        if quality.eq_ignore_ascii_case("deleted") {
            continue;
        }

        // Keep only quakes that actually shook people (felt MMI floor).
        let Some(mmi) = props.get("mmi").and_then(Value::as_f64) else { continue };
        let mmi = mmi.round() as i64;
        if mmi < FELT_MMI_FLOOR {
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

        let pid = props.get("publicID").and_then(Value::as_str).unwrap_or("");
        if pid.is_empty() {
            continue;
        }

        // GeoNet's `time` is RFC3339 (e.g. "2019-07-24T18:00:00.000Z"); some records
        // omit it — fall back to "now" so a live-but-timeless quake still plots.
        let time = props
            .get("time")
            .and_then(Value::as_str)
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        let title = props
            .get("locality")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(capitalize_first)
            .unwrap_or_else(|| "New Zealand earthquake".to_string());

        out.push(Event {
            id: format!("geonet-quake-{pid}"),
            source_id: "geonet_quake".to_string(),
            kind: EventKind::Earthquake,
            title,
            time,
            geo: Some(geo),
            severity: Severity::new(severity_for_mmi(mmi)),
            url: Some(format!("https://www.geonet.org.nz/earthquake/{pid}")),
            raw: props,
        });
    }
    Ok(out)
}

/// Uppercase the first character (GeoNet localities read "20 km north-east of …").
fn capitalize_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(first) => first.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Built from the REAL GeoNet quake GeoJSON shape (anchored to the committed bytes in
    // exxamalte/python-aio-geojson-geonetnz-quakes tests/fixtures/quakes-1.json:
    // FeatureCollection of Points with props publicID/time/depth/magnitude/mmi/locality/
    // quality, and a record that omits `time`). Extended to exercise the MMI ladder and
    // the drop rules: a strong felt quake, a moderate one with no `time`, a sub-felt
    // quake (MMI 2 — dropped), and a retracted one (quality "deleted" — dropped).
    const FIXTURE: &str = r#"{
      "type": "FeatureCollection",
      "features": [
        {"type":"Feature","geometry":{"type":"Point","coordinates":[178.2567291,-38.07928467]},
         "properties":{"publicID":"2019p111111","time":"2019-07-24T18:00:00.000Z",
           "depth":5.92,"magnitude":5.94,"mmi":7,"locality":"25 km north-east of Gisborne","quality":"best"}},
        {"type":"Feature","geometry":{"type":"Point","coordinates":[178.2912567,-38.46707928]},
         "properties":{"publicID":"2019p222222",
           "depth":0,"magnitude":5.02,"mmi":5,"locality":"within 5 km of Te Araroa","quality":"best"}},
        {"type":"Feature","geometry":{"type":"Point","coordinates":[172.5,-43.5]},
         "properties":{"publicID":"2019p333333","time":"2019-07-24T19:00:00.000Z",
           "depth":12.0,"magnitude":3.1,"mmi":2,"locality":"10 km west of Darfield","quality":"best"}},
        {"type":"Feature","geometry":{"type":"Point","coordinates":[174.0,-41.0]},
         "properties":{"publicID":"2019p444444","time":"2019-07-24T20:00:00.000Z",
           "depth":8.0,"magnitude":4.0,"mmi":6,"locality":"20 km south of Wellington","quality":"deleted"}}
      ]
    }"#;

    #[test]
    fn parses_fixture_keeping_felt_and_dropping_subfelt_and_deleted() {
        let ev = parse_geonet_quake(FIXTURE).unwrap();
        // MMI-2 (sub-felt) and the "deleted" quake are dropped; two felt quakes remain.
        assert_eq!(ev.len(), 2);

        assert_eq!(ev[0].id, "geonet-quake-2019p111111");
        assert_eq!(ev[0].kind, EventKind::Earthquake);
        assert_eq!(ev[0].title, "25 km north-east of Gisborne");
        // MMI 7 -> 0.85.
        assert!((ev[0].severity.value() - 0.85).abs() < 1e-9);
        let g = ev[0].geo.unwrap();
        assert!((g.lat + 38.07928467).abs() < 1e-6 && (g.lon - 178.2567291).abs() < 1e-6);
        assert_eq!(quake_chip(&ev[0].raw).as_deref(), Some("Felt MMI 7 · M5.9"));
        assert_eq!(ev[0].time.to_rfc3339(), "2019-07-24T18:00:00+00:00");
        assert_eq!(
            ev[0].url.as_deref(),
            Some("https://www.geonet.org.nz/earthquake/2019p111111")
        );

        // Second record has no `time` -> "now" fallback, still emitted.
        assert_eq!(ev[1].id, "geonet-quake-2019p222222");
        assert!((ev[1].severity.value() - 0.55).abs() < 1e-9); // MMI 5
        assert_eq!(quake_chip(&ev[1].raw).as_deref(), Some("Felt MMI 5 · M5.0"));
    }

    #[test]
    fn quiet_window_is_ok_not_error() {
        // No felt quakes in the window -> zero events, not a failure.
        let json = r#"{"type":"FeatureCollection","features":[]}"#;
        assert!(parse_geonet_quake(json).unwrap().is_empty());
        // A window that only holds a sub-felt quake also yields nothing.
        let sub = r#"{"type":"FeatureCollection","features":[
          {"type":"Feature","geometry":{"type":"Point","coordinates":[174.0,-41.0]},
           "properties":{"publicID":"x","magnitude":2.5,"mmi":1,"quality":"automatic"}}
        ]}"#;
        assert!(parse_geonet_quake(sub).unwrap().is_empty());
    }

    #[test]
    fn errors_on_bad_input() {
        // Missing the features array is malformed.
        assert!(parse_geonet_quake(r#"{"type":"FeatureCollection"}"#).is_err());
        // Not JSON at all (e.g. a 403 HTML body).
        assert!(parse_geonet_quake("<html>403 Forbidden</html>").is_err());
    }

    #[test]
    fn drops_records_without_geometry_or_id() {
        // A felt quake but with no geometry -> dropped (can't plot a dot).
        let no_geom = r#"{"type":"FeatureCollection","features":[
          {"type":"Feature","geometry":null,
           "properties":{"publicID":"g","magnitude":5.0,"mmi":5,"quality":"best"}}
        ]}"#;
        assert!(parse_geonet_quake(no_geom).unwrap().is_empty());
        // Felt + geometry but no publicID -> dropped (no stable id).
        let no_id = r#"{"type":"FeatureCollection","features":[
          {"type":"Feature","geometry":{"type":"Point","coordinates":[174.0,-41.0]},
           "properties":{"magnitude":5.0,"mmi":5,"quality":"best"}}
        ]}"#;
        assert!(parse_geonet_quake(no_id).unwrap().is_empty());
    }

    #[test]
    fn severity_ladders_with_mmi() {
        assert!((severity_for_mmi(3) - 0.3).abs() < 1e-9);
        assert!((severity_for_mmi(6) - 0.7).abs() < 1e-9);
        assert!((severity_for_mmi(8) - 0.95).abs() < 1e-9);
        assert!((severity_for_mmi(9) - 1.0).abs() < 1e-9);
        // Above the top of the scale still saturates at 1.0.
        assert!((severity_for_mmi(12) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn chip_handles_missing_magnitude() {
        // MMI present, magnitude absent -> chip carries the intensity alone.
        let raw = json!({"mmi": 6});
        assert_eq!(quake_chip(&raw).as_deref(), Some("Felt MMI 6"));
        // No MMI at all -> no chip.
        assert_eq!(quake_chip(&json!({"magnitude": 5.0})), None);
    }
}
