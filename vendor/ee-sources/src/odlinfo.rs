//! Bundesamt für Strahlenschutz (BfS) — Germany's Federal Office for Radiation
//! Protection — **ambient gamma dose rate (ODL — Ortsdosisleistung)**. Free, no key,
//! open data (Datenlizenz Deutschland – Namensnennung 2.0; credit "© Bundesamt für
//! Strahlenschutz (BfS)").
//!
//! Reads the BfS ODL-Info **OGC WFS opendata** service, layer `odlinfo_odl_1h_latest`
//! (`imis.bfs.de/ogc/opendata/ows`, `outputFormat=application/json`) — a GeoJSON
//! `FeatureCollection`, one `Point` feature per one of the ~1,700 fixed monitoring
//! stations across Germany carrying its latest 1-hour mean **gamma dose rate** in
//! `value` (µSv/h, `Gamma-ODL-Brutto` = cosmic + terrestrial), an ISO `end_measure`
//! timestamp, the station `id`/`kenn`/`name`/`plz`, and a `site_status` (1 = in
//! operation, 2 = defective, 3 = test).
//!
//! This opens a **radiation / nuclear-monitoring modality no other feed carries** — a
//! first-order WWIII-risk observable (a reactor release, a detonation, or a dispersal
//! event lights up the dose-rate network before almost anything else) over a NATO
//! frontline state. It is NOT another natural-hazard layer.
//!
//! **Signal-meaningfulness (the reason this is not an ECCC-hydrometric "nonsense
//! number"):** unlike a river gauge — whose absolute level means nothing without a
//! per-station flood baseline — an ambient gamma dose rate in µSv/h has a *universal*
//! natural-background baseline (~0.05–0.20 µSv/h essentially everywhere on Earth). A
//! reading above that is interpretable anywhere without a per-station table. So the
//! connector plots **only stations elevated above natural background** (`value` ≥
//! [`ELEVATED_FLOOR`]); all the background stations are dropped, so an all-normal
//! network — the healthy, expected peacetime state — is Ok/empty (0 events, not an
//! error), and the layer lights up precisely when radiation actually rises. Same
//! drop-the-all-clear pattern as `usgs_volcano` / `nwps_flood`. Non-operational
//! stations (defective / test) are dropped so a stuck or garbage reading can't raise a
//! false alarm.
//!
//! One normalized [`EventKind::Other`] [`Event`] (the catch-all for a new modality
//! before it earns a first-class variant) per elevated station at its own lat/lon
//! (inline Point geometry — no external join).

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use serde_json::Value;
use std::time::Duration;

/// Elevated floor in µSv/h. Natural background gamma dose rate is ~0.05–0.20 µSv/h in
/// Germany (a few geology-high sites reach ~0.25). 0.3 clears normal background and
/// local geology, so only genuinely elevated readings — 2–4× typical and up — plot; a
/// real radiological event runs far higher (≥1, often ≫10) and saturates the ladder.
const ELEVATED_FLOOR: f64 = 0.3;

/// `site_status` value for an operational station (1 = in Betrieb). Defective (2) and
/// test-mode (3) stations are dropped: their readings can't be trusted for an alarm.
const STATUS_OPERATIONAL: i64 = 1;

/// BfS ODL-Info gamma-dose-rate source.
#[derive(Default)]
pub struct Odlinfo;

impl Odlinfo {
    pub fn url(&self) -> &'static str {
        // WFS opendata: the latest 1-hour mean per station, as GeoJSON. Auth-free
        // (the ODL-Info OpenAPI spec declares no security scheme for this service).
        "https://www.imis.bfs.de/ogc/opendata/ows?service=WFS&version=1.1.0\
&request=GetFeature&typeName=opendata:odlinfo_odl_1h_latest&outputFormat=application/json"
    }
}

#[async_trait]
impl Source for Odlinfo {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "odlinfo",
            name: "BfS ODL Gamma Dose Rate (Germany)",
            domain: EventKind::Other,
            cadence: Duration::from_secs(3600), // 1-hour means, refreshed hourly
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        let body = crate::http::fetch_text(self.url()).await?;
        parse_odlinfo(&body)
    }
}

/// Normalized 0–1 severity from the dose rate (µSv/h). Graded against the universal
/// natural-background baseline: below [`ELEVATED_FLOOR`] is dropped upstream and never
/// reaches here, so the lowest rung is the "above normal" band.
fn severity_for_dose(v: f64) -> f64 {
    if v >= 100.0 {
        1.0 // extreme — severe radiological emergency
    } else if v >= 10.0 {
        0.9 // very high
    } else if v >= 1.0 {
        0.7 // high — well beyond any natural level
    } else if v >= 0.5 {
        0.5 // elevated
    } else {
        0.4 // above normal (≥ 0.3)
    }
}

/// Plain-language band for a dose rate (µSv/h), for the operator chip.
fn dose_band(v: f64) -> &'static str {
    if v >= 100.0 {
        "Extreme"
    } else if v >= 10.0 {
        "Very high"
    } else if v >= 1.0 {
        "High"
    } else if v >= 0.5 {
        "Elevated"
    } else {
        "Above normal"
    }
}

/// Operator chip for an elevated station: the dose rate with units + the band, e.g.
/// "0.45 µSv/h · Above normal" / "3.10 µSv/h · High". µSv/h is a defined unit against a
/// universal natural background, so the value is meaningful — not a raw scalar. `raw` is
/// the feature's `properties`.
pub fn dose_chip(raw: &Value) -> Option<String> {
    let v = raw.get("value").and_then(Value::as_f64)?;
    let unit = raw
        .get("unit")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("µSv/h");
    Some(format!("{v:.2} {unit} · {}", dose_band(v)))
}

/// Pure parser: BfS ODL-Info WFS GeoJSON -> events. Unit-tested offline. A missing
/// `features` array is malformed (error). Stations at/below the elevated floor,
/// non-operational (defective/test), or lacking geometry / a finite value are dropped,
/// so an all-normal network (the healthy peacetime state) parses to Ok/empty.
pub fn parse_odlinfo(json: &str) -> anyhow::Result<Vec<Event>> {
    let root: Value = serde_json::from_str(json)?;
    let features = root
        .get("features")
        .and_then(|f| f.as_array())
        .ok_or_else(|| anyhow::anyhow!("odlinfo: missing 'features' array"))?;

    let mut out = Vec::with_capacity(features.len());
    for f in features {
        let props = f.get("properties").cloned().unwrap_or(Value::Null);

        // Drop non-operational stations: a defective (2) or test-mode (3) station can
        // report a stale/garbage value that would raise a false radiation alarm.
        // Absent status (defensive) is treated as operational.
        let status = props
            .get("site_status")
            .and_then(|s| s.as_f64())
            .map(|s| s.round() as i64);
        if matches!(status, Some(s) if s != STATUS_OPERATIONAL) {
            continue;
        }

        // Keep only stations elevated above natural background.
        let Some(value) = props.get("value").and_then(Value::as_f64) else { continue };
        if !value.is_finite() || value < ELEVATED_FLOOR {
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

        // Stable id: the international station id, else the internal Kennung.
        let sid = props
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or_else(|| props.get("kenn").and_then(Value::as_str))
            .filter(|s| !s.is_empty());
        let Some(sid) = sid else { continue };

        // `end_measure` is RFC3339 (e.g. "2021-11-30T21:00:00Z"); fall back to "now"
        // so a live reading with a missing/odd timestamp still plots.
        let time = props
            .get("end_measure")
            .and_then(Value::as_str)
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        let place = props
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let title = match place {
            Some(p) => format!("Elevated gamma dose rate · {p}"),
            None => "Elevated gamma dose rate (Germany)".to_string(),
        };

        // Link to the BfS station detail page (keyed by the internal Kennung).
        let url = props
            .get("kenn")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(|kenn| {
                format!(
                    "https://odlinfo.bfs.de/ODL/EN/topics/location-of-measuring-stations/map/_documents/Messstelle.html?id={kenn}"
                )
            });

        out.push(Event {
            id: format!("odlinfo-{sid}"),
            source_id: "odlinfo".to_string(),
            kind: EventKind::Other,
            title,
            time,
            geo: Some(geo),
            severity: Severity::new(severity_for_dose(value)),
            url,
            raw: props,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Built from the REAL BfS ODL-Info WFS GeoJSON shape, anchored to the committed
    // bundesAPI OpenAPI spec (bundesAPI/strahlenschutz-api openapi.yaml: server
    // imis.bfs.de/ogc/opendata/ows; FeatureCollection with totalFeatures + features of
    // ExtendedFeature — Point geometry [lon,lat], geometry_name "geom", properties
    // id/kenn/plz/name/start_measure/end_measure/value/unit "µSv/h"/validated/nuclide
    // "Gamma-ODL-Brutto"/duration "1h"/site_status 1|2|3/site_status_text/kid/
    // height_above_sea/value_cosmic/value_terrestrial; example value 0.124 unit "µSv/h").
    // Exercises the elevated ladder and the drop rules: a normal-background station
    // (dropped), an above-normal one, a high one, a very-high one, a defective station
    // with a garbage-high value (dropped: not operational), and a no-geometry record.
    const FIXTURE: &str = r#"{
      "type": "FeatureCollection",
      "totalFeatures": 6,
      "numberReturned": 6,
      "timeStamp": "2026-07-04T12:00:00.000Z",
      "features": [
        {"type":"Feature","id":"odlinfo_odl_1h_latest.fid-1","geometry_name":"geom",
         "geometry":{"type":"Point","coordinates":[9.44,50.85]},
         "properties":{"id":"DEZ0001","kenn":"066340191","plz":"36280","name":"Oberaula",
           "end_measure":"2026-07-04T11:00:00Z","value":0.089,"unit":"µSv/h","validated":2,
           "nuclide":"Gamma-ODL-Brutto","duration":"1h","site_status":1,"site_status_text":"in Betrieb"}},
        {"type":"Feature","id":"odlinfo_odl_1h_latest.fid-2","geometry_name":"geom",
         "geometry":{"type":"Point","coordinates":[13.40,52.52]},
         "properties":{"id":"DEZ0002","kenn":"110000002","plz":"10117","name":"Berlin-Mitte",
           "end_measure":"2026-07-04T11:00:00Z","value":0.45,"unit":"µSv/h","validated":2,
           "nuclide":"Gamma-ODL-Brutto","duration":"1h","site_status":1,"site_status_text":"in Betrieb"}},
        {"type":"Feature","id":"odlinfo_odl_1h_latest.fid-3","geometry_name":"geom",
         "geometry":{"type":"Point","coordinates":[11.58,48.14]},
         "properties":{"id":"DEZ0003","kenn":"091620003","plz":"80331","name":"München",
           "end_measure":"2026-07-04T11:00:00Z","value":3.1,"unit":"µSv/h","validated":2,
           "nuclide":"Gamma-ODL-Brutto","duration":"1h","site_status":1,"site_status_text":"in Betrieb"}},
        {"type":"Feature","id":"odlinfo_odl_1h_latest.fid-4","geometry_name":"geom",
         "geometry":{"type":"Point","coordinates":[6.96,50.94]},
         "properties":{"id":"DEZ0004","kenn":"053150004","plz":"50667","name":"Köln",
           "end_measure":"2026-07-04T11:00:00Z","value":25.0,"unit":"µSv/h","validated":2,
           "nuclide":"Gamma-ODL-Brutto","duration":"1h","site_status":1,"site_status_text":"in Betrieb"}},
        {"type":"Feature","id":"odlinfo_odl_1h_latest.fid-5","geometry_name":"geom",
         "geometry":{"type":"Point","coordinates":[8.68,50.11]},
         "properties":{"id":"DEZ0005","kenn":"064120005","plz":"60311","name":"Frankfurt (defekt)",
           "end_measure":"2026-07-04T11:00:00Z","value":5.0,"unit":"µSv/h","validated":2,
           "nuclide":"Gamma-ODL-Brutto","duration":"1h","site_status":2,"site_status_text":"defekt"}},
        {"type":"Feature","id":"odlinfo_odl_1h_latest.fid-6","geometry_name":"geom",
         "geometry":null,
         "properties":{"id":"DEZ0006","kenn":"000000006","plz":"","name":"No geometry",
           "end_measure":"2026-07-04T11:00:00Z","value":9.9,"unit":"µSv/h","site_status":1}}
      ]
    }"#;

    #[test]
    fn parses_fixture_keeping_elevated_and_dropping_normal_defective_and_no_geometry() {
        let ev = parse_odlinfo(FIXTURE).unwrap();
        // Normal background (0.089), the defective station, and the no-geometry record
        // are all dropped; three elevated operational stations remain.
        assert_eq!(ev.len(), 3);

        // Berlin: above normal (0.45).
        assert_eq!(ev[0].id, "odlinfo-DEZ0002");
        assert_eq!(ev[0].kind, EventKind::Other);
        assert_eq!(ev[0].source_id, "odlinfo");
        assert_eq!(ev[0].title, "Elevated gamma dose rate · Berlin-Mitte");
        assert!((ev[0].severity.value() - 0.4).abs() < 1e-9);
        assert_eq!(dose_chip(&ev[0].raw).as_deref(), Some("0.45 µSv/h · Above normal"));
        let g = ev[0].geo.unwrap();
        assert!((g.lat - 52.52).abs() < 1e-6 && (g.lon - 13.40).abs() < 1e-6);
        assert_eq!(ev[0].time.to_rfc3339(), "2026-07-04T11:00:00+00:00");
        assert_eq!(
            ev[0].url.as_deref(),
            Some("https://odlinfo.bfs.de/ODL/EN/topics/location-of-measuring-stations/map/_documents/Messstelle.html?id=110000002")
        );

        // München: high (3.1).
        assert_eq!(ev[1].id, "odlinfo-DEZ0003");
        assert!((ev[1].severity.value() - 0.7).abs() < 1e-9);
        assert_eq!(dose_chip(&ev[1].raw).as_deref(), Some("3.10 µSv/h · High"));

        // Köln: very high (25.0).
        assert_eq!(ev[2].id, "odlinfo-DEZ0004");
        assert!((ev[2].severity.value() - 0.9).abs() < 1e-9);
        assert_eq!(dose_chip(&ev[2].raw).as_deref(), Some("25.00 µSv/h · Very high"));
    }

    #[test]
    fn all_normal_network_is_ok_not_error() {
        // Empty network -> zero events, not a failure.
        let empty = r#"{"type":"FeatureCollection","totalFeatures":0,"features":[]}"#;
        assert!(parse_odlinfo(empty).unwrap().is_empty());
        // A network where every station reads natural background -> nothing plots (the
        // healthy, expected peacetime state).
        let normal = r#"{"type":"FeatureCollection","features":[
          {"type":"Feature","geometry":{"type":"Point","coordinates":[9.0,50.0]},
           "properties":{"id":"DEZ0009","kenn":"9","name":"Quiet","value":0.11,"unit":"µSv/h","site_status":1}},
          {"type":"Feature","geometry":{"type":"Point","coordinates":[10.0,51.0]},
           "properties":{"id":"DEZ0010","kenn":"10","name":"Also quiet","value":0.20,"unit":"µSv/h","site_status":1}}
        ]}"#;
        assert!(parse_odlinfo(normal).unwrap().is_empty());
    }

    #[test]
    fn errors_on_bad_input() {
        // Missing the features array is malformed.
        assert!(parse_odlinfo(r#"{"type":"FeatureCollection"}"#).is_err());
        // Not JSON at all (e.g. a 403 HTML body).
        assert!(parse_odlinfo("<html>403 Forbidden</html>").is_err());
    }

    #[test]
    fn drops_records_without_geometry_or_value() {
        // Elevated but no geometry -> dropped (can't plot a dot).
        let no_geom = r#"{"type":"FeatureCollection","features":[
          {"type":"Feature","geometry":null,
           "properties":{"id":"a","kenn":"a","value":2.0,"unit":"µSv/h","site_status":1}}
        ]}"#;
        assert!(parse_odlinfo(no_geom).unwrap().is_empty());
        // Geometry but no value -> dropped (nothing to grade).
        let no_val = r#"{"type":"FeatureCollection","features":[
          {"type":"Feature","geometry":{"type":"Point","coordinates":[9.0,50.0]},
           "properties":{"id":"b","kenn":"b","unit":"µSv/h","site_status":1}}
        ]}"#;
        assert!(parse_odlinfo(no_val).unwrap().is_empty());
        // Elevated + geometry but no id/kenn -> dropped (no stable id).
        let no_id = r#"{"type":"FeatureCollection","features":[
          {"type":"Feature","geometry":{"type":"Point","coordinates":[9.0,50.0]},
           "properties":{"value":2.0,"unit":"µSv/h","site_status":1}}
        ]}"#;
        assert!(parse_odlinfo(no_id).unwrap().is_empty());
    }

    #[test]
    fn severity_and_band_ladder_with_dose() {
        assert!((severity_for_dose(0.3) - 0.4).abs() < 1e-9);
        assert!((severity_for_dose(0.5) - 0.5).abs() < 1e-9);
        assert!((severity_for_dose(1.0) - 0.7).abs() < 1e-9);
        assert!((severity_for_dose(10.0) - 0.9).abs() < 1e-9);
        assert!((severity_for_dose(100.0) - 1.0).abs() < 1e-9);
        assert!((severity_for_dose(500.0) - 1.0).abs() < 1e-9);
        assert_eq!(dose_band(0.35), "Above normal");
        assert_eq!(dose_band(0.6), "Elevated");
        assert_eq!(dose_band(2.0), "High");
        assert_eq!(dose_band(50.0), "Very high");
        assert_eq!(dose_band(200.0), "Extreme");
    }

    #[test]
    fn chip_handles_missing_value_and_unit() {
        // No value -> no chip.
        assert_eq!(dose_chip(&serde_json::json!({"unit":"µSv/h"})), None);
        // Value present, unit absent -> default unit.
        assert_eq!(
            dose_chip(&serde_json::json!({"value": 0.42})).as_deref(),
            Some("0.42 µSv/h · Above normal")
        );
    }
}
