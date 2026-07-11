//! Vigicrues (France) — the national flood-vigilance service's current section levels.
//! Free, no API key. Licence Ouverte / Etalab (credit "Vigicrues").
//!
//! Reads `services/1/InfoVigiCru.geojson` — one GeoJSON feature per monitored river
//! *tronçon* (reach), each carrying its current **flood-vigilance level** `NivInfViCr`
//! on France's national 1–4 colour scale:
//!   1 Vert   (no particular vigilance) ·
//!   2 Jaune  (risk of a rise / minor flooding, no major damage) ·
//!   3 Orange (risk of significant flooding) ·
//!   4 Rouge  (risk of major flooding — direct threat to life/property).
//! The connector plots only reaches **above the all-clear** (level ≥ 2); level-1 (green)
//! sections drop, so a calm France — the healthy peacetime state — yields zero events,
//! not an error, and the layer lights up precisely when a river goes on vigilance.
//!
//! Why this clears the signal-meaningfulness bar where a raw river level can't (the
//! reason ECCC hydrometric is rejected): `NivInfViCr` is a **baseline-relative
//! public-action category** — Vigicrues (the state flood-forecasting service, SCHAPI)
//! has already compared conditions against each reach's own thresholds — so the plotted
//! value carries real operator meaning ("Vigilance Rouge"), not an incomparable absolute
//! gauge reading in metres. It extends the baseline-relative flood modality (US
//! `nwps_flood`, England `ea_flood`) to **France / continental Europe** — new geography,
//! no overlap.
//!
//! Each feature carries an inline `LineString`/`MultiLineString` geometry (the reach
//! itself); the connector plots the dot at the reach's mean vertex (centroid), and
//! emits one normalized [`EventKind::Weather`] [`Event`] per on-vigilance reach.

use async_trait::async_trait;
use chrono::Utc;
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use serde_json::Value;
use std::time::Duration;

/// Vigicrues national flood-vigilance source (metropolitan France).
#[derive(Default)]
pub struct Vigicrues;

impl Vigicrues {
    /// The vigilance GeoJSON: every monitored reach + its current `NivInfViCr` level,
    /// with inline reach geometry. Auth-free.
    pub fn url(&self) -> &'static str {
        "https://www.vigicrues.gouv.fr/services/1/InfoVigiCru.geojson"
    }
}

#[async_trait]
impl Source for Vigicrues {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "vigicrues",
            name: "Vigicrues (France)",
            domain: EventKind::Weather,
            cadence: Duration::from_secs(900),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        let body = crate::http::fetch_text(self.url()).await?;
        parse_vigicrues(&body)
    }
}

/// Severity from the national vigilance level (4 = most severe). Levels 2–4 are the
/// on-vigilance tiers; level 1 (green, all-clear) and anything out of range drop.
fn severity_for_level(level: i64) -> Option<f64> {
    match level {
        4 => Some(1.0),  // Rouge  — risk of major flooding, threat to life
        3 => Some(0.7),  // Orange — risk of significant flooding
        2 => Some(0.4),  // Jaune  — risk of a rise / minor flooding
        _ => None,       // 1 Vert (all-clear) or out of range
    }
}

/// Canonical human label for a vigilance level — the operator read behind the dot.
/// Deterministic (not dependent on any upstream free-text colour string).
fn level_label(level: i64) -> &'static str {
    match level {
        4 => "Vigilance Rouge",
        3 => "Vigilance Orange",
        2 => "Vigilance Jaune",
        _ => "Vigilance",
    }
}

/// A JSON value that may be a number or a numeric string -> i64.
fn as_i64_loose(v: &Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_f64().map(|f| f as i64))
        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<i64>().ok()))
}

/// Read a property under any of the given keys (case-variant tolerant across the
/// `services/1` short schema and the documented v1.1 long schema).
fn prop<'a>(props: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|k| props.get(k))
}

fn str_prop(props: &Value, keys: &[&str]) -> Option<String> {
    prop(props, keys)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The reach's current vigilance level: `NivInfViCr` on the `services/1` product, with
/// fallbacks to the v1.1 `NivSituVigiCruEnt` and lowercase variants.
fn reach_level(props: &Value) -> Option<i64> {
    prop(props, &["NivInfViCr", "nivinfvicr", "NivSituVigiCruEnt", "nivsituvigicruent"])
        .and_then(as_i64_loose)
}

/// Centroid (mean vertex) of a LineString/MultiLineString coordinate tree — the same
/// recursive leaf-collector `eccc_marine` uses for polygons: it walks to every `[lon,
/// lat]` pair regardless of nesting depth, so both geometry types are handled.
fn centroid(geometry: &Value) -> Option<Geo> {
    fn collect(v: &Value, acc: &mut Vec<(f64, f64)>) {
        if let Some(arr) = v.as_array() {
            if arr.len() == 2 && arr[0].is_number() && arr[1].is_number() {
                if let (Some(lon), Some(lat)) = (arr[0].as_f64(), arr[1].as_f64()) {
                    acc.push((lon, lat));
                }
            } else {
                for x in arr {
                    collect(x, acc);
                }
            }
        }
    }
    let mut pts = Vec::new();
    collect(geometry.get("coordinates")?, &mut pts);
    if pts.is_empty() {
        return None;
    }
    let n = pts.len() as f64;
    let lon = pts.iter().map(|(x, _)| x).sum::<f64>() / n;
    let lat = pts.iter().map(|(_, y)| y).sum::<f64>() / n;
    Geo::new(lat, lon)
}

/// Operator chip: the vigilance tier plus the reach name, e.g.
/// "Vigilance Rouge · Rhône aval" / "Vigilance Jaune · Loire moyenne".
/// `raw` is the feature's `properties` object.
pub fn vigilance_chip(raw: &Value) -> Option<String> {
    let level = reach_level(raw)?;
    severity_for_level(level)?;
    let head = level_label(level);
    match str_prop(raw, &["LbEntCru", "lbentcru", "NomEntVigiCru", "nomentvigicruent"]) {
        Some(name) => Some(format!("{head} · {name}")),
        None => Some(head.to_string()),
    }
}

/// Pure parser: the InfoVigiCru GeoJSON -> events. Unit-tested offline. A payload
/// missing its `features` array is malformed (error); reaches at level 1 (green) or
/// without a usable geometry are filtered out, so an all-calm France is Ok/empty.
pub fn parse_vigicrues(json: &str) -> anyhow::Result<Vec<Event>> {
    let root: Value = serde_json::from_str(json)?;
    let features = root
        .get("features")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("vigicrues: payload missing 'features' array"))?;

    let mut out = Vec::with_capacity(features.len());
    for f in features {
        let props = f.get("properties").unwrap_or(&Value::Null);

        let Some(level) = reach_level(props) else { continue };
        let Some(sev) = severity_for_level(level) else { continue };

        let Some(geo) =
            f.get("geometry").filter(|g| !g.is_null()).and_then(centroid)
        else {
            continue;
        };

        let code = str_prop(props, &["CdEntCru", "cdentcru", "CdEntVigiCru", "gid", "id"])
            .or_else(|| f.get("id").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_else(|| format!("{level}-{:.4}-{:.4}", geo.lat, geo.lon));

        let name = str_prop(props, &["LbEntCru", "lbentcru", "NomEntVigiCru", "nomentvigicruent"])
            .unwrap_or_else(|| code.clone());

        out.push(Event {
            id: format!("vigicrues-{code}"),
            source_id: "vigicrues".to_string(),
            kind: EventKind::Weather,
            title: name,
            time: Utc::now(),
            geo: Some(geo),
            severity: Severity::new(sev),
            url: Some("https://www.vigicrues.gouv.fr/".to_string()),
            // Retain the feature properties so the chip can surface the tier + name.
            raw: props.clone(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Built to the confirmed InfoVigiCru.geojson shape (anchored to the committed
    // `kalisio/k-vigicrues` client: it fetches
    // `https://www.vigicrues.gouv.fr/services/1/InfoVigiCru.geojson`, reads
    // `properties.NivInfViCr` clamped to 1–4 as the level and `properties.LbEntCru`
    // as the reach name, and validates LineString/MultiLineString geometries).
    // One Rouge (4), one Orange (3, MultiLineString), one Jaune (2, arriving as the
    // string "2") — all kept — plus one Vert (1, dropped as all-clear) and one
    // Orange reach with no geometry (can't be placed, dropped). Coordinates are real
    // metropolitan-France reaches.
    const FC: &str = r#"{
      "type":"FeatureCollection",
      "features":[
        {"type":"Feature","properties":{"CdEntCru":"AG3","LbEntCru":"Rhône aval","NivInfViCr":4},
         "geometry":{"type":"LineString","coordinates":[[4.62,44.10],[4.65,43.95],[4.68,43.80]]}},
        {"type":"Feature","properties":{"CdEntCru":"LO7","LbEntCru":"Loire moyenne","NivInfViCr":3},
         "geometry":{"type":"MultiLineString","coordinates":[[[2.10,47.20],[2.30,47.30]],[[2.40,47.35],[2.55,47.40]]]}},
        {"type":"Feature","properties":{"CdEntCru":"GA5","LbEntCru":"Garonne agenaise","NivInfViCr":"2"},
         "geometry":{"type":"LineString","coordinates":[[0.60,44.18],[0.75,44.22]]}},
        {"type":"Feature","properties":{"CdEntCru":"SE1","LbEntCru":"Seine amont","NivInfViCr":1},
         "geometry":{"type":"LineString","coordinates":[[3.10,48.20],[3.30,48.30]]}},
        {"type":"Feature","properties":{"CdEntCru":"DO2","LbEntCru":"Doubs","NivInfViCr":3},
         "geometry":null}
      ]
    }"#;

    #[test]
    fn parses_fixture_dropping_green_and_geometryless() {
        let ev = parse_vigicrues(FC).unwrap();
        // Vert (level 1) dropped; the geometryless Orange reach dropped.
        assert_eq!(ev.len(), 3);

        assert_eq!(ev[0].id, "vigicrues-AG3");
        assert_eq!(ev[0].source_id, "vigicrues");
        assert_eq!(ev[0].kind, EventKind::Weather);
        assert_eq!(ev[0].title, "Rhône aval");
        assert!((ev[0].severity.value() - 1.0).abs() < 1e-9); // Rouge -> 1.0
        let g = ev[0].geo.unwrap();
        // Mean vertex of the three-point LineString.
        assert!((g.lat - 43.95).abs() < 1e-6 && (g.lon - 4.65).abs() < 1e-6, "got {:?}", (g.lat, g.lon));
        assert_eq!(vigilance_chip(&ev[0].raw).as_deref(), Some("Vigilance Rouge · Rhône aval"));

        // Orange MultiLineString -> 0.7, centroid over all four vertices.
        assert_eq!(ev[1].title, "Loire moyenne");
        assert!((ev[1].severity.value() - 0.7).abs() < 1e-9);
        let g = ev[1].geo.unwrap();
        assert!((g.lon - 2.3375).abs() < 1e-6 && (g.lat - 47.3125).abs() < 1e-6, "got {:?}", (g.lat, g.lon));
        assert_eq!(vigilance_chip(&ev[1].raw).as_deref(), Some("Vigilance Orange · Loire moyenne"));

        // NivInfViCr arrived as the string "2" -> still parsed; Jaune -> 0.4.
        assert!((ev[2].severity.value() - 0.4).abs() < 1e-9);
        assert_eq!(vigilance_chip(&ev[2].raw).as_deref(), Some("Vigilance Jaune · Garonne agenaise"));
    }

    #[test]
    fn all_calm_is_ok_not_error() {
        // Empty features (the common quiet state) -> zero plotted events, not a failure.
        assert!(parse_vigicrues(r#"{"type":"FeatureCollection","features":[]}"#).unwrap().is_empty());
        // Only a Vert (level 1) reach -> also empty (all-clear dropped).
        let fc = r#"{"type":"FeatureCollection","features":[
          {"type":"Feature","properties":{"CdEntCru":"X","LbEntCru":"y","NivInfViCr":1},
           "geometry":{"type":"LineString","coordinates":[[2.0,48.0],[2.1,48.1]]}}]}"#;
        assert!(parse_vigicrues(fc).unwrap().is_empty());
    }

    #[test]
    fn errors_on_bad_input() {
        // Payload missing the 'features' array is malformed.
        assert!(parse_vigicrues(r#"{"type":"FeatureCollection"}"#).is_err());
        // Not JSON at all (e.g. an HTML 403 page).
        assert!(parse_vigicrues("<html>403 Forbidden</html>").is_err());
    }

    #[test]
    fn severity_ladders_with_level() {
        assert_eq!(severity_for_level(4), Some(1.0));
        assert_eq!(severity_for_level(3), Some(0.7));
        assert_eq!(severity_for_level(2), Some(0.4));
        assert_eq!(severity_for_level(1), None); // Vert (all-clear) drops
        assert_eq!(severity_for_level(5), None);
    }

    #[test]
    fn reads_v11_long_schema_field_names() {
        // The documented v1.1 product names the level `NivSituVigiCruEnt` and the reach
        // `NomEntVigiCru`; the connector reads those as fallbacks too.
        let fc = r#"{"type":"FeatureCollection","features":[
          {"type":"Feature","properties":{"CdEntVigiCru":"RH","NomEntVigiCru":"Rhin","NivSituVigiCruEnt":4},
           "geometry":{"type":"LineString","coordinates":[[7.6,48.5],[7.7,48.6]]}}]}"#;
        let ev = parse_vigicrues(fc).unwrap();
        assert_eq!(ev.len(), 1);
        assert!((ev[0].severity.value() - 1.0).abs() < 1e-9);
        assert_eq!(vigilance_chip(&ev[0].raw).as_deref(), Some("Vigilance Rouge · Rhin"));
    }
}
