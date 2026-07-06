//! STUK (Säteilyturvakeskus — Finnish Radiation and Nuclear Safety Authority) —
//! **external radiation dose rate** over Finland. Free, no key, open data served
//! through the Finnish Meteorological Institute (FMI / Ilmatieteenlaitos) open-data
//! WFS (`opendata.fmi.fi`, producer STUK; credit "STUK / Ilmatieteenlaitos").
//!
//! Reads the FMI WFS stored query
//! `stuk::observations::external-radiation::multipointcoverage` — a WFS 2.0 GML
//! *multipoint coverage*: the latest external dose-rate reading (µSv/h) for each of
//! Finland's ~255 automatic monitoring stations, which report at 10-minute intervals.
//!
//! ## Why it's on the map (and not duplicative)
//! This extends the **radiation / nuclear-monitoring modality** (opened by
//! [`super::odlinfo`], Germany) to **Finland — a NATO frontline state with the EU's
//! longest border with Russia and two operating nuclear power plants (Loviisa,
//! Olkiluoto)**. A dose-rate network is a first-order WWIII-risk observable: a reactor
//! release, a detonation, or a dispersal event lights it up before almost anything
//! else. Distinct authority (STUK), distinct geography (the whole Finland/Russia
//! frontier), no overlap with the German network.
//!
//! **Signal-meaningfulness (same universal-baseline argument as `odlinfo`):** an
//! ambient dose rate in µSv/h has a *universal* natural-background baseline — in
//! Finland 0.05–0.30 µSv/h (STUK), essentially everywhere on Earth 0.05–0.20 — so a
//! reading above it is interpretable anywhere without a per-station table (unlike a
//! river gauge). The connector plots **only stations elevated above natural
//! background** (`value` ≥ [`ELEVATED_FLOOR`]); all background stations drop, so an
//! all-normal network — the healthy peacetime state — is Ok/empty (0 events, not an
//! error), and the layer lights up precisely when radiation rises. STUK's own
//! automatic-network alarm level is **0.4 µSv/h**; the 0.3 floor here sits at the top
//! of Finnish natural background, one notch below that alarm, and shares the exact
//! severity ladder with `odlinfo` so both radiation feeds calibrate identically.
//!
//! One [`EventKind::Other`] [`Event`] (the catch-all for a new modality before it
//! earns a first-class variant) per elevated station at its own lat/lon.
//!
//! ## Path A (prod fetches live) — GitHub-anchored schema
//! The live host 403s web fetch in-sandbox (as every gov host does), so endpoint,
//! stored-query id, wire schema and **auth model** are anchored to committed bytes:
//! STUK's own official open-data client `StukFi/opendata` (`wfs_scripts/fmi_utils.py` +
//! `process_data.py`, fetched off `raw.githubusercontent.com`). It requests
//! `https://opendata.fmi.fi/wfs/eng?request=GetFeature&storedquery_id=stuk::observations::external-radiation::multipointcoverage&starttime=…&endtime=…`
//! with a **plain keyless `urlopen`** (no key, no header → auth-free), and parses the
//! GML exactly as here: `gml:Point` members give each station's `gml:name` +
//! `gml:pos` ("lat lon"); `gmlcov:positions` lists "lat lon epoch" per measurement;
//! `gml:doubleOrNilReasonTupleList` lists the dose-rate value per measurement
//! (NaN-aware), index-aligned with the positions. Unit (µSv/h), background and the
//! 0.4 alarm level are confirmed from STUK's public "Radiation today" documentation.

use async_trait::async_trait;
use chrono::{Duration as ChronoDuration, TimeZone, Utc};
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use std::collections::HashMap;
use std::time::Duration;

/// Elevated floor in µSv/h. Finnish natural background is 0.05–0.30 µSv/h (STUK); 0.3
/// sits at the top of that band, so only genuinely elevated readings plot. STUK's own
/// automatic-network alarm level is 0.4 µSv/h; a real radiological event runs far
/// higher (≥1, often ≫10) and saturates the ladder. Shared with `odlinfo`.
const ELEVATED_FLOOR: f64 = 0.3;

/// STUK external-radiation dose-rate source (FMI open-data WFS multipoint coverage).
#[derive(Default)]
pub struct StukRadiation;

impl StukRadiation {
    /// Live WFS URL for a `[start, end]` window. Auth-free (STUK's own client uses a
    /// keyless request). English service endpoint (`/wfs/eng`).
    fn url(start: &str, end: &str) -> String {
        format!(
            "https://opendata.fmi.fi/wfs/eng?request=GetFeature&storedquery_id=\
stuk::observations::external-radiation::multipointcoverage&starttime={start}&endtime={end}"
        )
    }
}

#[async_trait]
impl Source for StukRadiation {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "stuk_radiation",
            name: "STUK External Radiation Dose Rate (Finland)",
            domain: EventKind::Other,
            cadence: Duration::from_secs(600), // stations report every 10 minutes
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        // A 1-hour window ending now: the 10-minute rounds inside it are settled and
        // per-station dedup below keeps each station's newest reading. The window is
        // built at fetch time so the pure parser stays offline-testable.
        let end = Utc::now();
        let start = end - ChronoDuration::minutes(60);
        let fmt = "%Y-%m-%dT%H:%M:00Z";
        let url = Self::url(&start.format(fmt).to_string(), &end.format(fmt).to_string());
        let body = crate::http::fetch_text(&url).await?;
        parse_stuk_radiation(&body)
    }
}

/// Normalized 0–1 severity from the dose rate (µSv/h). Identical ladder to `odlinfo`:
/// below [`ELEVATED_FLOOR`] is dropped upstream, so the lowest rung is "above normal".
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
/// universal natural background, so the value is meaningful — not a raw scalar. `raw`
/// is the flat properties object this connector stores.
pub fn dose_chip(raw: &serde_json::Value) -> Option<String> {
    let v = raw.get("value").and_then(serde_json::Value::as_f64)?;
    let unit = raw
        .get("unit")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("µSv/h");
    Some(format!("{v:.2} {unit} · {}", dose_band(v)))
}

/// Full `<tag …> … </tag>` element substrings (tag given with its namespace prefix).
fn elements<'a>(xml: &'a str, tag: &str) -> Vec<&'a str> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(rel) = xml[i..].find(&open) {
        let s = i + rel;
        let Some(erel) = xml[s..].find(&close) else { break };
        let e = s + erel + close.len();
        out.push(&xml[s..e]);
        i = e;
    }
    out
}

/// Inner text of the first `<tag …> … </tag>` within `el`.
fn inner<'a>(el: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}");
    let s = el.find(&open)?;
    let gt = el[s..].find('>')? + s + 1;
    let close = format!("</{tag}>");
    let e = el[gt..].find(&close)? + gt;
    Some(&el[gt..e])
}

/// Value of the `name="…"` attribute in an element's open tag.
fn attr<'a>(el: &'a str, name: &str) -> Option<&'a str> {
    let key = format!("{name}=\"");
    let s = el.find(&key)? + key.len();
    let e = el[s..].find('"')? + s;
    Some(&el[s..e])
}

/// Normalize a "lat lon …" string to its first two whitespace tokens joined by one
/// space — the join key between a station's `gml:pos` and a `gmlcov:positions` row.
fn latlon_key(s: &str) -> Option<String> {
    let mut it = s.split_whitespace();
    let a = it.next()?;
    let b = it.next()?;
    Some(format!("{a} {b}"))
}

struct Station {
    site: String,
    id: String,
}

/// One measurement's newest state for a station.
struct Latest {
    epoch: i64,
    lat: f64,
    lon: f64,
    value: f64,
}

/// Pure parser: FMI/STUK external-radiation multipoint-coverage GML -> events.
/// Offline-tested. An `ows:ExceptionReport` or a coverage missing its
/// value/position blocks is an error (routes to feed-health/last-good). Per station
/// the *newest* reading in the window wins; stations at/below [`ELEVATED_FLOOR`], with
/// a non-finite value, or without a usable coordinate/timestamp drop — so an
/// all-normal network (the healthy peacetime state) parses to Ok/empty.
pub fn parse_stuk_radiation(gml: &str) -> anyhow::Result<Vec<Event>> {
    // Station name/id map, keyed by the "lat lon" of each gml:Point.
    let mut names: HashMap<String, Station> = HashMap::new();
    for pt in elements(gml, "gml:Point") {
        let Some(pos) = inner(pt, "gml:pos").and_then(latlon_key) else { continue };
        let site = inner(pt, "gml:name").map(|s| s.trim().to_string()).unwrap_or_default();
        // gml:id like "point-1-1-100968": the station id is the trailing segment.
        let id = attr(pt, "gml:id")
            .and_then(|g| g.rsplit('-').next())
            .unwrap_or("")
            .to_string();
        names.entry(pos).or_insert(Station { site, id });
    }

    // The measurement grid: positions ("lat lon epoch" per row) aligned index-for-index
    // with the value tuples. Absent blocks => an exception / malformed response.
    let (Some(pos_block), Some(val_block)) = (
        inner(gml, "gmlcov:positions"),
        inner(gml, "gml:doubleOrNilReasonTupleList"),
    ) else {
        if let Some(exc) = inner(gml, "ows:ExceptionText").or_else(|| inner(gml, "ExceptionText")) {
            anyhow::bail!("stuk_radiation: upstream exception: {}", exc.trim());
        }
        anyhow::bail!("stuk_radiation: no coverage data in response");
    };

    let pos_rows: Vec<&str> = pos_block.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let val_rows: Vec<&str> = val_block.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if pos_rows.len() != val_rows.len() {
        // Misaligned arrays would silently mislabel readings — treat as format drift.
        anyhow::bail!(
            "stuk_radiation: {} positions vs {} values — coverage misaligned",
            pos_rows.len(),
            val_rows.len()
        );
    }

    let mut latest: HashMap<String, Latest> = HashMap::new();
    let mut paired = 0usize; // rows with a usable coordinate + value
    let mut timed = 0usize; // rows whose epoch parsed
    for (p, v) in pos_rows.iter().zip(val_rows.iter()) {
        let mut pt = p.split_whitespace();
        let (Some(lat), Some(lon), Some(epoch)) = (pt.next(), pt.next(), pt.next()) else {
            continue;
        };
        let (Some(lat), Some(lon)) = (lat.parse::<f64>().ok(), lon.parse::<f64>().ok()) else {
            continue;
        };
        // First token of the value tuple; "NaN" (missing reading) parses to NaN.
        let value = v.split_whitespace().next().and_then(|t| t.parse::<f64>().ok());
        let Some(value) = value else { continue };
        paired += 1;
        let Ok(epoch) = epoch.parse::<i64>() else { continue };
        timed += 1;

        let key = format!("{lat} {lon}");
        let slot = latest.entry(key).or_insert(Latest { epoch, lat, lon, value });
        if epoch >= slot.epoch {
            *slot = Latest { epoch, lat, lon, value };
        }
    }

    // Format-drift tripwire: readings existed but none carried a parseable epoch — the
    // positions/time encoding has drifted. Erroring routes it to feed-health rather
    // than silently blanking the (normally-empty) radiation layer.
    if paired > 0 && timed == 0 {
        anyhow::bail!("stuk_radiation: {paired} reading(s) but no parseable timestamp — coverage format drift?");
    }

    let mut out: Vec<Event> = Vec::new();
    for (key, l) in latest {
        if !l.value.is_finite() || l.value < ELEVATED_FLOOR {
            continue; // background / missing — the all-clear state
        }
        let Some(geo) = Geo::new(l.lat, l.lon) else { continue };
        let Some(time) = Utc.timestamp_opt(l.epoch, 0).single() else { continue };

        let station = names.get(&key);
        let site = station
            .map(|s| s.site.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_default();
        let sid = station
            .map(|s| s.id.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| key.replace(' ', "_"));

        let title = if site.is_empty() {
            "Elevated radiation dose rate (Finland)".to_string()
        } else {
            format!("Elevated radiation dose rate · {site}")
        };

        out.push(Event {
            id: format!("stuk_radiation-{sid}"),
            source_id: "stuk_radiation".to_string(),
            kind: EventKind::Other,
            title,
            time,
            geo: Some(geo),
            severity: Severity::new(severity_for_dose(l.value)),
            url: Some("https://stuk.fi/en/radiation-today".to_string()),
            raw: serde_json::json!({
                "value": l.value,
                "unit": "µSv/h",
                "site": site,
                "id": sid,
                "timestamp": time.to_rfc3339(),
            }),
        });
    }

    // Hottest first, so a downstream cap keeps the most elevated stations.
    out.sort_by(|a, b| {
        b.severity
            .value()
            .partial_cmp(&a.severity.value())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.time.cmp(&a.time))
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real FMI/STUK multipoint-coverage shape (anchored to StukFi/opendata's own
    // parser: gml:Point members with gml:name + gml:pos; gmlcov:positions "lat lon
    // epoch"; gml:doubleOrNilReasonTupleList values in µSv/h, NaN-aware, index-aligned).
    // Rows exercise: a normal-background station (dropped), an above-normal one, a
    // station read twice where the NEWER reading is normal (dedup → dropped), a station
    // read twice where the newer reading is elevated (dedup → kept), and a NaN (dropped).
    const FIXTURE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<wfs:FeatureCollection xmlns:wfs="http://www.opengis.net/wfs/2.0"
    xmlns:gml="http://www.opengis.net/gml/3.2"
    xmlns:gmlcov="http://www.opengis.net/gmlcov/1.0"
    xmlns:swe="http://www.opengis.net/swe/2.0"
    xmlns:om="http://www.opengis.net/om/2.0">
  <wfs:member>
    <om:featureOfInterest>
      <gml:MultiPoint gml:id="mp-1">
        <gml:pointMember>
          <gml:Point gml:id="point-1-1-100968" srsName="EPSG:4326"><gml:name>Helsinki Kaisaniemi</gml:name><gml:pos>60.17523 24.94459 </gml:pos></gml:Point>
        </gml:pointMember>
        <gml:pointMember>
          <gml:Point gml:id="point-1-2-101932" srsName="EPSG:4326"><gml:name>Sodankyla Tahtela</gml:name><gml:pos>67.36697 26.62882 </gml:pos></gml:Point>
        </gml:pointMember>
        <gml:pointMember>
          <gml:Point gml:id="point-1-3-101267" srsName="EPSG:4326"><gml:name>Olkiluoto</gml:name><gml:pos>61.24 21.44 </gml:pos></gml:Point>
        </gml:pointMember>
        <gml:pointMember>
          <gml:Point gml:id="point-1-4-101661" srsName="EPSG:4326"><gml:name>Loviisa</gml:name><gml:pos>60.40 26.34 </gml:pos></gml:Point>
        </gml:pointMember>
        <gml:pointMember>
          <gml:Point gml:id="point-1-5-101570" srsName="EPSG:4326"><gml:name>Kuopio</gml:name><gml:pos>62.89 27.68 </gml:pos></gml:Point>
        </gml:pointMember>
      </gml:MultiPoint>
    </om:featureOfInterest>
    <om:result>
      <gmlcov:MultiPointCoverage gml:id="mpcv-1">
        <gml:domainSet>
          <gmlcov:SimpleMultiPoint gml:id="smp-1" srsDimension="3">
            <gmlcov:positions>
              60.17523 24.94459 1751716800
              67.36697 26.62882 1751716800
              61.24 21.44 1751716200
              61.24 21.44 1751716800
              60.40 26.34 1751716200
              60.40 26.34 1751716800
              62.89 27.68 1751716800
            </gmlcov:positions>
          </gmlcov:SimpleMultiPoint>
        </gml:domainSet>
        <gml:rangeSet>
          <gml:DataBlock>
            <gml:rangeParameters/>
            <gml:doubleOrNilReasonTupleList>
              0.09
              0.45
              0.10
              0.62
              0.35
              0.12
              NaN
            </gml:doubleOrNilReasonTupleList>
          </gml:DataBlock>
        </gml:rangeSet>
      </gmlcov:MultiPointCoverage>
    </om:result>
  </wfs:member>
</wfs:FeatureCollection>"#;

    #[test]
    fn keeps_elevated_dedups_to_newest_and_drops_normal_and_nan() {
        let ev = parse_stuk_radiation(FIXTURE).unwrap();
        // Kept: Sodankyla (0.45, above normal) and Olkiluoto (newest 0.62, elevated).
        // Dropped: Helsinki 0.09 (normal), Loviisa (newest 0.12 normal), Kuopio (NaN).
        assert_eq!(ev.len(), 2, "two elevated stations after dedup");

        // Hottest first: Olkiluoto (0.5) before Sodankyla (0.4).
        assert_eq!(ev[0].id, "stuk_radiation-101267");
        assert_eq!(ev[0].kind, EventKind::Other);
        assert_eq!(ev[0].source_id, "stuk_radiation");
        assert_eq!(ev[0].title, "Elevated radiation dose rate · Olkiluoto");
        assert!((ev[0].severity.value() - 0.5).abs() < 1e-9);
        assert_eq!(dose_chip(&ev[0].raw).as_deref(), Some("0.62 µSv/h · Elevated"));
        // Olkiluoto's NEWER epoch (…6800) won the dedup, not the older 0.10 reading.
        assert_eq!(ev[0].time, Utc.timestamp_opt(1751716800, 0).single().unwrap());
        let g = ev[0].geo.unwrap();
        assert!((g.lat - 61.24).abs() < 1e-6 && (g.lon - 21.44).abs() < 1e-6);

        assert_eq!(ev[1].id, "stuk_radiation-101932");
        assert_eq!(ev[1].title, "Elevated radiation dose rate · Sodankyla Tahtela");
        assert!((ev[1].severity.value() - 0.4).abs() < 1e-9);
        assert_eq!(dose_chip(&ev[1].raw).as_deref(), Some("0.45 µSv/h · Above normal"));

        // No dropped station leaked through.
        assert!(ev.iter().all(|e| e.id != "stuk_radiation-100968")); // Helsinki normal
        assert!(ev.iter().all(|e| e.id != "stuk_radiation-101661")); // Loviisa now normal
        assert!(ev.iter().all(|e| e.id != "stuk_radiation-101570")); // Kuopio NaN
    }

    #[test]
    fn all_normal_network_is_ok_not_error() {
        // A coverage where every station reads natural background -> nothing plots.
        let normal = r#"<wfs:FeatureCollection xmlns:gml="x" xmlns:gmlcov="y">
          <gml:Point gml:id="point-1-1-1"><gml:name>Quiet</gml:name><gml:pos>60.0 25.0</gml:pos></gml:Point>
          <gmlcov:positions>
            60.0 25.0 1751716800
            61.0 26.0 1751716800
          </gmlcov:positions>
          <gml:doubleOrNilReasonTupleList>
            0.11
            0.20
          </gml:doubleOrNilReasonTupleList>
        </wfs:FeatureCollection>"#;
        assert!(parse_stuk_radiation(normal).unwrap().is_empty());
    }

    #[test]
    fn errors_on_exception_and_malformed() {
        // FMI ows:ExceptionReport (e.g. no data / bad request) must surface as an error.
        let exc = r#"<ows:ExceptionReport xmlns:ows="http://www.opengis.net/ows/1.1">
          <ows:Exception exceptionCode="OperationParsingFailed">
            <ows:ExceptionText>No data available.</ows:ExceptionText>
          </ows:Exception>
        </ows:ExceptionReport>"#;
        assert!(parse_stuk_radiation(exc).is_err());
        // A 403 HTML body is not a coverage -> error.
        assert!(parse_stuk_radiation("<html>403 Forbidden</html>").is_err());
    }

    #[test]
    fn misaligned_positions_and_values_error() {
        // Two positions but one value -> silent mislabelling risk -> error.
        let bad = r#"<x xmlns:gmlcov="y" xmlns:gml="z">
          <gmlcov:positions>
            60.0 25.0 1751716800
            61.0 26.0 1751716800
          </gmlcov:positions>
          <gml:doubleOrNilReasonTupleList>
            0.62
          </gml:doubleOrNilReasonTupleList>
        </x>"#;
        assert!(parse_stuk_radiation(bad).is_err());
    }

    #[test]
    fn severity_and_band_ladder() {
        assert!((severity_for_dose(0.3) - 0.4).abs() < 1e-9);
        assert!((severity_for_dose(0.5) - 0.5).abs() < 1e-9);
        assert!((severity_for_dose(1.0) - 0.7).abs() < 1e-9);
        assert!((severity_for_dose(10.0) - 0.9).abs() < 1e-9);
        assert!((severity_for_dose(100.0) - 1.0).abs() < 1e-9);
        assert_eq!(dose_band(0.35), "Above normal");
        assert_eq!(dose_band(0.6), "Elevated");
        assert_eq!(dose_band(2.0), "High");
        assert_eq!(dose_band(50.0), "Very high");
        assert_eq!(dose_band(200.0), "Extreme");
    }

    #[test]
    fn chip_handles_missing_value_and_unit() {
        assert_eq!(dose_chip(&serde_json::json!({"unit":"µSv/h"})), None);
        assert_eq!(
            dose_chip(&serde_json::json!({"value": 0.42})).as_deref(),
            Some("0.42 µSv/h · Above normal")
        );
    }
}
