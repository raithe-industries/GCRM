//! NOAA Aviation Weather Center (AWC) — international SIGMETs. Free, no API key.
//! U.S. Government public domain (credit "NOAA / NWS Aviation Weather Center").
//!
//! Reads the AWC `api/data/isigmet?format=geojson` product — a GeoJSON
//! `FeatureCollection`, one `Polygon` feature per active international SIGMET
//! (SIGnificant METeorological information), the en-route aviation hazard warning
//! issued by each Meteorological Watch Office for its Flight Information Region.
//! Each feature carries the hazard type (`hazard`: TS / TC / VA / TURB / ICE / DS /
//! SS / MTW / GR / IFR …), an intensity/coverage qualifier (`qualifier`: SEV / EMBD
//! / ISOL / OCNL / FRQ …), the affected flight-level band (`base` / `top`, feet
//! MSL), the issuing FIR (`firName`), and the raw SIGMET text. Emits one normalized
//! [`EventKind::Weather`] [`Event`] per SIGMET, plotted at the centroid of its
//! hazard polygon. An empty `FeatureCollection` (no active intl SIGMETs in scope)
//! therefore yields zero events, not an error.
//!
//! Why this clears the bar: AWC is the authoritative U.S. aviation-weather body and
//! the aggregator of international SIGMETs across the world's FIRs, so this opens an
//! **en-route aviation-hazard modality** no current feed carries (the ground-weather
//! warnings of NWS/ECCC, the cyclone *tracks* of NHC/JMA, and the river flooding of
//! NWPS are all distinct), with **global** geography. Every value is signal-meaningful
//! and unit-bearing: the hazard is a named WMO aviation phenomenon, the qualifier its
//! standardized intensity/coverage, and the band its flight levels — no raw scalar.
//!
//! **Path B (mirrored snapshot for verification):** the live host 403s in the
//! GitHub-only Signal-Hunter sandbox, so the parser + fixture are anchored to a real
//! captured AWC international-SIGMET payload committed on GitHub
//! (`thomasdubdub/sigmet-sectors/20200316.json`: real SBRE RECIFE / LFRR BREST
//! SIGMETs). Prod (full network) fetches the live `format=geojson` endpoint directly.

use async_trait::async_trait;
use chrono::Utc;
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use serde_json::Value;
use std::collections::HashSet;
use std::time::Duration;

/// NOAA AWC international-SIGMET source.
#[derive(Default)]
pub struct AwcSigmet;

impl AwcSigmet {
    /// The current AWC Data API international-SIGMET product, as GeoJSON.
    pub fn url(&self) -> &'static str {
        "https://aviationweather.gov/api/data/isigmet?format=geojson"
    }
}

#[async_trait]
impl Source for AwcSigmet {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "awc_sigmet",
            name: "NOAA AWC International SIGMETs",
            domain: EventKind::Weather,
            cadence: Duration::from_secs(1800),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        let body = crate::http::fetch_text(self.url()).await?;
        parse_awc_sigmet(&body)
    }
}

/// Base severity for the SIGMET hazard type. Volcanic ash and tropical cyclones are
/// the most dangerous to aviation and tend to be widespread; convection (TS) next;
/// turbulence/icing/mountain-wave/dust mid; IFR (ceiling/visibility) lowest.
fn hazard_severity(hazard: &str) -> f64 {
    match hazard {
        "VA" => 0.9,           // volcanic ash
        "TC" => 0.9,           // tropical cyclone
        "TSGR" => 0.75,        // thunderstorms with hail
        "TS" => 0.7,           // thunderstorms / convection
        "GR" => 0.65,          // hail
        "TURB" => 0.55,        // turbulence
        "ICE" => 0.55,         // icing
        "MTW" => 0.5,          // mountain wave
        "DS" | "SS" => 0.5,    // dust / sand storm
        "IFR" => 0.4,          // IFR conditions (ceiling/visibility)
        _ => 0.5,
    }
}

/// Severity bump from the qualifier — a SEV/HVY SIGMET is graver than its hazard
/// base alone implies. Returns `None` for coverage-only qualifiers (EMBD/ISOL/…).
fn qualifier_severity(qualifier: &str) -> Option<f64> {
    match qualifier {
        "SEV" => Some(0.85), // severe
        "HVY" => Some(0.7),  // heavy
        _ => None,
    }
}

/// Human label for a hazard code — the operator read behind the dot.
fn hazard_label(hazard: &str) -> String {
    match hazard {
        "TS" => "Thunderstorms".into(),
        "TSGR" => "Thunderstorms with hail".into(),
        "TC" => "Tropical cyclone".into(),
        "VA" => "Volcanic ash".into(),
        "TURB" => "Turbulence".into(),
        "ICE" => "Icing".into(),
        "MTW" => "Mountain wave".into(),
        "DS" => "Duststorm".into(),
        "SS" => "Sandstorm".into(),
        "GR" => "Hail".into(),
        "IFR" => "IFR conditions".into(),
        other => other.to_string(), // surface the raw WMO code rather than hide it
    }
}

/// Human adjective for an intensity/coverage qualifier, or `None` to omit unknowns.
fn qualifier_label(qualifier: &str) -> Option<&'static str> {
    Some(match qualifier {
        "SEV" => "Severe",
        "HVY" => "Heavy",
        "MOD" => "Moderate",
        "EMBD" => "Embedded",
        "ISOL" => "Isolated",
        "OCNL" => "Occasional",
        "FRQ" => "Frequent",
        "OBSC" => "Obscured",
        "SQL" => "Squall-line",
        "WDSPR" => "Widespread",
        _ => return None,
    })
}

/// Case-insensitive string lookup over a JSON object.
fn prop_str<'a>(props: &'a Value, key: &str) -> Option<&'a str> {
    props
        .as_object()?
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .and_then(|(_, v)| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Case-insensitive numeric lookup tolerating number-or-string encodings; rejects
/// the non-finite sentinels (`NaN`) that some AWC mirrors emit for an absent level.
fn prop_f64(props: &Value, key: &str) -> Option<f64> {
    let v = props
        .as_object()?
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v)?;
    let n = v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))?;
    n.is_finite().then_some(n)
}

/// A flight-level band string from base/top feet MSL, e.g. "FL170–330", "to FL430",
/// "above FL170". Returns `None` when neither bound is present.
fn level_band(base: Option<f64>, top: Option<f64>) -> Option<String> {
    let fl = |ft: f64| (ft / 100.0).round() as i64;
    match (base, top) {
        (Some(b), Some(t)) => Some(format!("FL{:03}\u{2013}{:03}", fl(b), fl(t))),
        (None, Some(t)) => Some(format!("to FL{:03}", fl(t))),
        (Some(b), None) => Some(format!("above FL{:03}", fl(b))),
        (None, None) => None,
    }
}

/// Centroid (mean vertex) of a GeoJSON Polygon's exterior ring, dropping the closing
/// vertex when it repeats the first. `coords` is `geometry.coordinates`.
fn polygon_centroid(coords: &Value) -> Option<Geo> {
    // Polygon: [ ring0, ... ]; ring0: [ [lon,lat], ... ].
    let ring = coords.as_array()?.first()?.as_array()?;
    let mut pts: Vec<(f64, f64)> = ring
        .iter()
        .filter_map(|p| {
            let a = p.as_array()?;
            Some((a.first()?.as_f64()?, a.get(1)?.as_f64()?)) // (lon, lat)
        })
        .collect();
    if pts.len() >= 2 && pts.first() == pts.last() {
        pts.pop(); // drop the duplicated closing vertex so it isn't double-weighted
    }
    if pts.is_empty() {
        return None;
    }
    let (sx, sy) = pts.iter().fold((0.0, 0.0), |(x, y), (lon, lat)| (x + lon, y + lat));
    let n = pts.len() as f64;
    Geo::new(sy / n, sx / n)
}

/// Fallback centroid from the `coords` property string ("lat,lon,lat,lon,…"), used
/// only if the feature lacks a usable GeoJSON geometry.
fn coords_centroid(s: &str) -> Option<Geo> {
    let nums: Vec<f64> = s.split(',').filter_map(|t| t.trim().parse().ok()).collect();
    if nums.len() < 2 {
        return None;
    }
    // Pairs are lat,lon; closing pair may repeat the first — averaging is robust to it.
    let pairs: Vec<(f64, f64)> = nums.chunks_exact(2).map(|c| (c[0], c[1])).collect();
    let (sla, slo) = pairs.iter().fold((0.0, 0.0), |(a, b), (la, lo)| (a + la, b + lo));
    let n = pairs.len() as f64;
    Geo::new(sla / n, slo / n)
}

/// Operator chip for a SIGMET: qualified hazard plus its flight-level band, e.g.
/// "Severe Turbulence · FL170–330" or "Embedded Thunderstorms · to FL430".
/// `raw` is the feature's `properties`.
pub fn sigmet_chip(raw: &Value) -> Option<String> {
    let hazard = prop_str(raw, "hazard")?.to_ascii_uppercase();
    let hz = hazard_label(&hazard);
    let head = match prop_str(raw, "qualifier").and_then(|q| qualifier_label(&q.to_ascii_uppercase())) {
        Some(q) => format!("{q} {hz}"),
        None => hz,
    };
    match level_band(prop_f64(raw, "base"), prop_f64(raw, "top")) {
        Some(band) => Some(format!("{head} \u{00b7} {band}")),
        None => Some(head),
    }
}

/// Pure parser: AWC international-SIGMET GeoJSON -> events. Unit-tested offline. A
/// missing `features` array is malformed (error); a feature without a hazard or a
/// placeable geometry is skipped, so an empty/quiet network is Ok/empty.
pub fn parse_awc_sigmet(json: &str) -> anyhow::Result<Vec<Event>> {
    let root: Value = serde_json::from_str(json)?;
    let features = root
        .get("features")
        .and_then(|f| f.as_array())
        .ok_or_else(|| anyhow::anyhow!("awc_sigmet: missing 'features' array"))?;

    let mut out = Vec::with_capacity(features.len());
    // The AWC aggregate sometimes carries the SAME issuance twice as byte-identical
    // features (observed live 2026-07-04: WIII/FAOR/SAME series each ×2), so a map
    // rebuild plotted stacked twin dots with colliding ids. Collapse on the synthetic
    // identity key (fir+series+hazard+from) — first record wins.
    let mut seen: HashSet<String> = HashSet::new();
    for f in features {
        let props = f.get("properties").cloned().unwrap_or(Value::Null);

        let Some(hazard) = prop_str(&props, "hazard").map(|h| h.to_ascii_uppercase()) else {
            continue; // no hazard -> no aviation signal
        };

        // Prefer the GeoJSON polygon centroid; fall back to the coords string.
        let geo = f
            .get("geometry")
            .filter(|g| g.get("type").and_then(|t| t.as_str()) == Some("Polygon"))
            .and_then(|g| g.get("coordinates"))
            .and_then(polygon_centroid)
            .or_else(|| prop_str(&props, "coords").and_then(coords_centroid));
        let Some(geo) = geo else { continue };

        let qualifier = prop_str(&props, "qualifier").map(|q| q.to_ascii_uppercase());
        let sev = qualifier
            .as_deref()
            .and_then(qualifier_severity)
            .map_or(hazard_severity(&hazard), |q| q.max(hazard_severity(&hazard)));

        let fir = prop_str(&props, "firName")
            .or_else(|| prop_str(&props, "firId"))
            .unwrap_or("SIGMET")
            .to_string();
        // FIR names carry no hazard; pair them so the title alone reads ("LFRR BREST
        // — Turbulence"), with the chip adding qualifier + flight levels.
        let title = format!("{fir} \u{2014} {}", hazard_label(&hazard));

        // The current GeoJSON output carries no stable feature id, so synthesize a
        // deterministic one from the issuing FIR + series + hazard + valid-from time.
        let series = prop_str(&props, "seriesId").unwrap_or("");
        let from = prop_str(&props, "validTimeFrom").unwrap_or("");
        let fir_id = prop_str(&props, "firId").unwrap_or("fir");
        let id = format!("awc-sigmet-{fir_id}-{series}-{hazard}-{from}");
        if !seen.insert(id.clone()) {
            continue; // upstream duplicate of an already-emitted issuance
        }

        out.push(Event {
            id,
            source_id: "awc_sigmet".to_string(),
            kind: EventKind::Weather,
            title,
            time: Utc::now(),
            geo: Some(geo),
            severity: Severity::new(sev),
            url: Some("https://aviationweather.gov/sigmet/".to_string()),
            raw: props,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Anchored to the real captured AWC international-SIGMET payload committed at
    // thomasdubdub/sigmet-sectors/20200316.json: SBRE RECIFE (EMBD TS, top FL430,
    // no base) and LFRR BREST (SEV TURB, FL170–330) are genuine records; a Tokyo VA
    // (volcanic ash) record exercises the most-severe hazard, and the trailing two
    // exercise the drop paths (no hazard, no geometry/coords).
    const FIXTURE: &str = r#"{
      "type": "FeatureCollection",
      "features": [
        {"type":"Feature",
         "properties":{"icaoId":"SBBR","firId":"SBRE","firName":"SBRE RECIFE","hazard":"TS",
           "validTimeFrom":"2020-03-16T08:00:00Z","validTimeTo":"2020-03-16T12:00:00Z",
           "qualifier":"EMBD","geom":"AREA",
           "coords":"-6.533,-44.917,-11.000,-35.367,-8.417,-33.817,-6.533,-44.917",
           "top":43000,"rawSigmet":"SBRE SIGMET 3 ... EMBD TS FCST ... TOP FL430="},
         "geometry":{"type":"Polygon","coordinates":[[[-44.92,-6.53],[-35.37,-11.00],[-33.82,-8.42],[-44.92,-6.53]]]}},
        {"type":"Feature",
         "properties":{"icaoId":"LFPW","firId":"LFRR","firName":"LFRR BREST","hazard":"TURB",
           "validTimeFrom":"2020-03-16T08:00:00Z","validTimeTo":"2020-03-16T12:00:00Z",
           "qualifier":"SEV","geom":"AREA","base":17000,"top":33000,
           "rawSigmet":"LFRR SIGMET 1 ... SEV TURB FL170/330="},
         "geometry":{"type":"Polygon","coordinates":[[[-5.0,48.0],[-1.0,48.0],[-1.0,46.0],[-5.0,46.0],[-5.0,48.0]]]}},
        {"type":"Feature",
         "properties":{"icaoId":"RJTD","firId":"RJJJ","firName":"RJJJ FUKUOKA","hazard":"VA",
           "validTimeFrom":"2020-03-16T08:00:00Z","validTimeTo":"2020-03-16T14:00:00Z",
           "qualifier":"","base":0,"top":15000,"rawSigmet":"RJJJ SIGMET ... VA ERUPTION ..."},
         "geometry":{"type":"Polygon","coordinates":[[[140.0,32.0],[142.0,32.0],[142.0,30.0],[140.0,30.0],[140.0,32.0]]]}},
        {"type":"Feature",
         "properties":{"icaoId":"XXXX","firId":"XXXX","firName":"XXXX NOWHERE","validTimeFrom":"2020-03-16T08:00:00Z"},
         "geometry":{"type":"Polygon","coordinates":[[[0.0,0.0],[1.0,0.0],[1.0,1.0],[0.0,0.0]]]}},
        {"type":"Feature",
         "properties":{"icaoId":"YYYY","firId":"YYYY","firName":"YYYY VOID","hazard":"ICE"},
         "geometry":null}
      ]
    }"#;

    #[test]
    fn parses_fixture_dropping_unplaceable_and_hazardless() {
        let ev = parse_awc_sigmet(FIXTURE).unwrap();
        // The no-hazard feature and the no-geometry/no-coords feature are dropped.
        assert_eq!(ev.len(), 3);

        // Recife: embedded thunderstorms, top only -> 0.7 severity, centroid in Brazil.
        assert_eq!(ev[0].kind, EventKind::Weather);
        assert_eq!(ev[0].title, "SBRE RECIFE \u{2014} Thunderstorms");
        assert!((ev[0].severity.value() - 0.7).abs() < 1e-9);
        let g = ev[0].geo.unwrap();
        assert!(g.lat < 0.0 && (-45.0..-33.0).contains(&g.lon));
        assert_eq!(sigmet_chip(&ev[0].raw).as_deref(), Some("Embedded Thunderstorms \u{00b7} to FL430"));
        assert_eq!(ev[0].id, "awc-sigmet-SBRE--TS-2020-03-16T08:00:00Z");

        // Brest: SEV qualifier lifts turbulence (0.55) to 0.85; band FL170–330.
        assert_eq!(ev[1].title, "LFRR BREST \u{2014} Turbulence");
        assert!((ev[1].severity.value() - 0.85).abs() < 1e-9);
        assert_eq!(sigmet_chip(&ev[1].raw).as_deref(), Some("Severe Turbulence \u{00b7} FL170\u{2013}330"));
        let g = ev[1].geo.unwrap();
        assert!((45.0..49.0).contains(&g.lat) && (-6.0..0.0).contains(&g.lon));

        // Fukuoka: volcanic ash -> 0.9; empty qualifier omitted from the chip.
        assert_eq!(ev[2].title, "RJJJ FUKUOKA \u{2014} Volcanic ash");
        assert!((ev[2].severity.value() - 0.9).abs() < 1e-9);
        assert_eq!(sigmet_chip(&ev[2].raw).as_deref(), Some("Volcanic ash \u{00b7} FL000\u{2013}150"));
    }

    #[test]
    fn upstream_duplicate_issuance_collapses_to_one_event() {
        // The live AWC aggregate has been observed carrying the same issuance twice as
        // byte-identical features (2026-07-04: WIII/FAOR/SAME series each ×2), which
        // plotted stacked twin dots with colliding ids. Duplicate the first fixture
        // feature and assert the parser still emits exactly the fixture's 3 events.
        let mut root: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        let feats = root.get_mut("features").unwrap().as_array_mut().unwrap();
        let twin = feats[0].clone();
        feats.insert(1, twin);
        let ev = parse_awc_sigmet(&root.to_string()).unwrap();
        assert_eq!(ev.len(), 3, "duplicate upstream issuance must collapse, not double-plot");
        assert_eq!(ev[0].id, "awc-sigmet-SBRE--TS-2020-03-16T08:00:00Z");
        assert_ne!(ev[1].id, ev[0].id, "distinct issuances keep distinct ids");
    }

    #[test]
    fn empty_collection_is_ok_not_error() {
        // No active international SIGMETs in scope -> zero events, not a failure.
        let json = r#"{"type":"FeatureCollection","features":[]}"#;
        assert!(parse_awc_sigmet(json).unwrap().is_empty());
    }

    #[test]
    fn coords_string_fallback_when_geometry_absent() {
        // A feature with no GeoJSON geometry but a coords string still places.
        let json = r#"{"type":"FeatureCollection","features":[
          {"type":"Feature",
           "properties":{"firId":"KZWY","firName":"KZWY NEW YORK OCEANIC","hazard":"TC",
             "qualifier":"","coords":"30.0,-60.0,32.0,-60.0,32.0,-58.0,30.0,-58.0","top":50000}}
        ]}"#;
        let ev = parse_awc_sigmet(json).unwrap();
        assert_eq!(ev.len(), 1);
        let g = ev[0].geo.unwrap();
        assert!((30.0..33.0).contains(&g.lat) && (-61.0..-57.0).contains(&g.lon));
        // Tropical cyclone -> 0.9.
        assert!((ev[0].severity.value() - 0.9).abs() < 1e-9);
        assert_eq!(sigmet_chip(&ev[0].raw).as_deref(), Some("Tropical cyclone \u{00b7} to FL500"));
    }

    #[test]
    fn errors_on_bad_input() {
        // Missing the features array is malformed.
        assert!(parse_awc_sigmet(r#"{"type":"FeatureCollection"}"#).is_err());
        // Not JSON at all (e.g. an HTML 403 page).
        assert!(parse_awc_sigmet("<html>403 Forbidden</html>").is_err());
    }

    #[test]
    fn severity_ladders_by_hazard_and_qualifier() {
        assert!((hazard_severity("VA") - 0.9).abs() < 1e-9);
        assert!((hazard_severity("TS") - 0.7).abs() < 1e-9);
        assert!((hazard_severity("IFR") - 0.4).abs() < 1e-9);
        assert_eq!(qualifier_severity("SEV"), Some(0.85));
        assert_eq!(qualifier_severity("EMBD"), None);
        // NaN top must not produce a band (some mirrors emit it for an absent level).
        let raw = serde_json::json!({"hazard":"ICE","qualifier":"MOD","top":"NaN"});
        assert_eq!(sigmet_chip(&raw).as_deref(), Some("Moderate Icing"));
    }
}
