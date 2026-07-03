//! BMKG (Badan Meteorologi, Klimatologi, dan Geofisika) — Indonesia's national
//! agency and operator of InaTEWS, the Indonesian Tsunami Early Warning System.
//! Free, no API key. Attribution required: "BMKG".
//!
//! Reads the open `gempadirasakan.json` product — the **felt** earthquakes (the ~15
//! most recent quakes that were actually reported felt by people), a list under
//! `Infogempa.gempa`. Each record carries an inline `Coordinates` ("lat,lon"), the
//! `Magnitude`, `Kedalaman` (depth), `Wilayah` (region), the official tsunami
//! assessment `Potensi`, and — the signal no raw quake catalogue carries — `Dirasakan`,
//! the **felt intensity on the Modified-Mercalli (MMI) scale** per affected place
//! (e.g. "III Denpasar, II Mataram"). Emits one normalized [`EventKind::Earthquake`]
//! event per felt quake at its own lat/lon.
//!
//! Why this isn't another USGS/EMSC quake feed: those are raw *detection* catalogues
//! (every instrument-detected event, magnitude only). BMKG `gempadirasakan` is a
//! **human-impact** product — only quakes felt by people, graded by the baseline-relative
//! MMI intensity they were felt at, plus Indonesia's national tsunami-potential flag —
//! over the world's most seismically/tsunami-exposed region. MMI is a defined
//! ground-shaking scale (each level a named effect), so a "felt MMI V" dot is real,
//! unit-bearing signal, not a raw number.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use serde_json::Value;
use std::time::Duration;

/// BMKG felt-earthquake source (InaTEWS open data).
#[derive(Default)]
pub struct BmkgQuake;

impl BmkgQuake {
    pub fn url(&self) -> &'static str {
        "https://data.bmkg.go.id/DataMKG/TEWS/gempadirasakan.json"
    }
}

#[async_trait]
impl Source for BmkgQuake {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "bmkg_quake",
            name: "BMKG Felt Earthquakes",
            domain: EventKind::Earthquake,
            cadence: Duration::from_secs(600),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        let body = crate::http::fetch_text(self.url()).await?;
        parse_bmkg(&body)
    }
}

/// Roman numeral (I..XII, the MMI range) → integer. `None` for anything outside the scale.
fn roman_to_int(tok: &str) -> Option<u8> {
    let mut total: i32 = 0;
    let mut prev = 0i32;
    for c in tok.chars().rev() {
        let v = match c {
            'I' => 1,
            'V' => 5,
            'X' => 10,
            _ => return None,
        };
        if v < prev {
            total -= v;
        } else {
            total += v;
            prev = v;
        }
    }
    if (1..=12).contains(&total) {
        Some(total as u8)
    } else {
        None
    }
}

/// Integer MMI → Roman numeral for display (1..=12; clamps above XII).
fn int_to_roman(n: u8) -> &'static str {
    match n {
        1 => "I",
        2 => "II",
        3 => "III",
        4 => "IV",
        5 => "V",
        6 => "VI",
        7 => "VII",
        8 => "VIII",
        9 => "IX",
        10 => "X",
        11 => "XI",
        _ => "XII",
    }
}

/// Peak felt MMI in a BMKG `Dirasakan` string ("IV-V Pacitan, III Trenggalek" → 5).
/// Scans for maximal runs of the Roman letters I/V/X (region names are mixed-case, so
/// they never form an all-uppercase IVX run) and returns the largest valid MMI.
pub fn max_mmi(dirasakan: &str) -> Option<u8> {
    let mut best: Option<u8> = None;
    let mut run = String::new();
    let flush = |run: &mut String, best: &mut Option<u8>| {
        if !run.is_empty() {
            if let Some(v) = roman_to_int(run) {
                if best.is_none_or(|b| v > b) {
                    *best = Some(v);
                }
            }
            run.clear();
        }
    };
    for c in dirasakan.chars() {
        if matches!(c, 'I' | 'V' | 'X') {
            run.push(c);
        } else {
            flush(&mut run, &mut best);
        }
    }
    flush(&mut run, &mut best);
    best
}

/// Indonesia's tsunami assessment from the `Potensi` text. `None` when there is no
/// potential ("Tidak berpotensi tsunami", the normal case); otherwise the alert word.
pub fn tsunami_level(potensi: &str) -> Option<&'static str> {
    let p = potensi.to_lowercase();
    if p.contains("tidak") {
        return None; // "Tidak berpotensi tsunami"
    }
    if p.contains("awas") {
        Some("Awas")
    } else if p.contains("siaga") {
        Some("Siaga")
    } else if p.contains("waspada") {
        Some("Waspada")
    } else if p.contains("berpotensi") {
        Some("Potensi")
    } else {
        None
    }
}

/// Normalized 0–1 severity from felt MMI intensity (the human-impact scale), with a
/// raw-magnitude fallback when no MMI was parsed, then floored by any tsunami potential.
fn severity_for(mmi: Option<u8>, mag: Option<f64>, tsunami: Option<&str>) -> f64 {
    let base: f64 = if let Some(m) = mmi {
        match m {
            0..=1 => 0.2,
            2 => 0.25,
            3 => 0.35,
            4 => 0.45,
            5 => 0.55,
            6 => 0.7,
            7 => 0.82,
            8 => 0.9,
            _ => 1.0, // IX+
        }
    } else {
        match mag.unwrap_or(0.0) {
            m if m >= 7.0 => 0.85,
            m if m >= 6.0 => 0.7,
            m if m >= 5.0 => 0.55,
            m if m >= 4.0 => 0.4,
            _ => 0.3,
        }
    };
    let bump = match tsunami {
        Some("Awas") => 1.0,
        Some("Siaga") => 0.95,
        Some(_) => 0.9, // Waspada / generic potential
        None => 0.0,
    };
    base.max(bump)
}

/// Operator chip for a felt quake: the peak MMI intensity + magnitude, plus any tsunami
/// potential — e.g. "Felt MMI IV · M4.8" / "Felt MMI VI · M6.2 · Tsunami Siaga".
pub fn felt_chip(raw: &Value) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(m) = raw.get("mmi").and_then(Value::as_u64) {
        parts.push(format!("Felt MMI {}", int_to_roman(m as u8)));
    }
    if let Some(mag) = raw.get("magnitude").and_then(Value::as_f64) {
        parts.push(format!("M{mag:.1}"));
    }
    if let Some(t) = raw.get("tsunami").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        parts.push(format!("Tsunami {t}"));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" · "))
    }
}

/// Read a JSON value as f64 whether it's a number or a numeric string ("4.8").
fn num(v: Option<&Value>) -> Option<f64> {
    v.and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())))
}

/// Parse the BMKG `Coordinates` string "lat,lon" into a validated [`Geo`].
fn parse_coords(s: &str) -> Option<Geo> {
    let (lat, lon) = s.split_once(',')?;
    let lat: f64 = lat.trim().parse().ok()?;
    let lon: f64 = lon.trim().parse().ok()?;
    Geo::new(lat, lon)
}

/// Pure parser: BMKG `gempadirasakan.json` → events. Unit-tested offline. A missing
/// `Infogempa.gempa` node is malformed (error); an empty list is the normal quiet-window
/// case (Ok, zero events). `gempa` may be an array (the felt list) or a single object.
pub fn parse_bmkg(json: &str) -> anyhow::Result<Vec<Event>> {
    let root: Value = serde_json::from_str(json)?;
    let gempa = root
        .get("Infogempa")
        .and_then(|i| i.get("gempa"))
        .ok_or_else(|| anyhow::anyhow!("bmkg_quake: missing 'Infogempa.gempa'"))?;

    // Tolerate both the felt-list (array) and the single-latest (object) shapes.
    let records: Vec<&Value> = match gempa {
        Value::Array(a) => a.iter().collect(),
        Value::Object(_) => vec![gempa],
        _ => anyhow::bail!("bmkg_quake: 'gempa' is neither array nor object"),
    };

    let mut out = Vec::with_capacity(records.len());
    for r in records {
        let Some(geo) = r
            .get("Coordinates")
            .and_then(Value::as_str)
            .and_then(parse_coords)
        else {
            continue; // no usable location → not a map dot
        };

        let mag = num(r.get("Magnitude"));
        let wilayah = r.get("Wilayah").and_then(Value::as_str).unwrap_or("").trim();
        let kedalaman = r.get("Kedalaman").and_then(Value::as_str).unwrap_or("").trim();
        let dirasakan = r.get("Dirasakan").and_then(Value::as_str).unwrap_or("").trim();
        let potensi = r.get("Potensi").and_then(Value::as_str).unwrap_or("").trim();

        let mmi = max_mmi(dirasakan);
        let tsunami = tsunami_level(potensi);

        let time = r
            .get("DateTime")
            .and_then(Value::as_str)
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        // Stable id from the event timestamp (unique per BMKG event); fall back to the
        // coordinate string so two records can't collide if a DateTime is ever absent.
        let id = r
            .get("DateTime")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("{:.4},{:.4}", geo.lat, geo.lon));

        let title = match (mag, wilayah.is_empty()) {
            (Some(m), false) => format!("M{m:.1} felt earthquake — {wilayah}"),
            (Some(m), true) => format!("M{m:.1} felt earthquake"),
            (None, false) => format!("Felt earthquake — {wilayah}"),
            (None, true) => "Felt earthquake".to_string(),
        };

        out.push(Event {
            id: format!("bmkg-{id}"),
            source_id: "bmkg_quake".to_string(),
            kind: EventKind::Earthquake,
            title,
            time,
            geo: Some(geo),
            severity: Severity::new(severity_for(mmi, mag, tsunami)),
            url: Some("https://www.bmkg.go.id/gempabumi/".to_string()),
            raw: serde_json::json!({
                "magnitude": mag,
                "depth": kedalaman,
                "region": wilayah,
                "felt": dirasakan,
                "mmi": mmi,
                "potensi": potensi,
                "tsunami": tsunami,
            }),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Built from the real BMKG `gempadirasakan.json` shape (Infogempa.gempa array;
    // Coordinates "lat,lon"; Dirasakan MMI list; Potensi assessment). Record 1: a routine
    // felt quake near Bali (MMI III–IV, no tsunami). Record 2: a strong quake with a real
    // tsunami "Siaga" potential. Record 3: omits Dirasakan to exercise the magnitude
    // fallback. Record 4: a blank Coordinates string → dropped (no usable location).
    const FIXTURE: &str = r#"{
      "Infogempa": {
        "gempa": [
          {"Tanggal":"21 Jun 2026","Jam":"16:16:30 WIB","DateTime":"2026-06-21T09:16:30+00:00",
           "Coordinates":"-8.65,115.21","Lintang":"8.65 LS","Bujur":"115.21 BT","Magnitude":"4.8",
           "Kedalaman":"10 km","Wilayah":"Bali","Potensi":"Tidak berpotensi tsunami",
           "Dirasakan":"IV Denpasar, III Mataram, II Kuta"},
          {"Tanggal":"20 Jun 2026","Jam":"03:02:11 WIB","DateTime":"2026-06-20T20:02:11+00:00",
           "Coordinates":"-3.10,128.40","Lintang":"3.10 LS","Bujur":"128.40 BT","Magnitude":"6.2",
           "Kedalaman":"15 km","Wilayah":"Laut Banda","Potensi":"Siaga","Dirasakan":"VI Ambon, V Masohi"},
          {"Tanggal":"19 Jun 2026","Jam":"22:40:00 WIB","DateTime":"2026-06-19T15:40:00+00:00",
           "Coordinates":"1.20,126.50","Lintang":"1.20 LU","Bujur":"126.50 BT","Magnitude":"5.1",
           "Kedalaman":"40 km","Wilayah":"Maluku Utara","Potensi":"Tidak berpotensi tsunami","Dirasakan":""},
          {"Tanggal":"18 Jun 2026","Jam":"10:00:00 WIB","DateTime":"2026-06-18T03:00:00+00:00",
           "Coordinates":"","Magnitude":"3.9","Kedalaman":"5 km","Wilayah":"Nowhere",
           "Potensi":"Tidak berpotensi tsunami","Dirasakan":"II Nowhere"}
        ]
      }
    }"#;

    #[test]
    fn parses_fixture() {
        let ev = parse_bmkg(FIXTURE).unwrap();
        // The blank-Coordinates record is dropped → 3 plotted.
        assert_eq!(ev.len(), 3);

        // Record 1: Bali felt quake, peak MMI IV (Denpasar), no tsunami.
        assert_eq!(ev[0].id, "bmkg-2026-06-21T09:16:30+00:00");
        assert_eq!(ev[0].kind, EventKind::Earthquake);
        assert_eq!(ev[0].title, "M4.8 felt earthquake — Bali");
        let g = ev[0].geo.unwrap();
        assert!((g.lat + 8.65).abs() < 1e-6 && (g.lon - 115.21).abs() < 1e-6);
        // MMI IV → 0.45.
        assert!((ev[0].severity.value() - 0.45).abs() < 1e-9);
        assert_eq!(felt_chip(&ev[0].raw).as_deref(), Some("Felt MMI IV · M4.8"));

        // Record 2: MMI VI base 0.7, but a "Siaga" tsunami potential floors it at 0.95.
        assert!((ev[1].severity.value() - 0.95).abs() < 1e-9);
        assert_eq!(
            felt_chip(&ev[1].raw).as_deref(),
            Some("Felt MMI VI · M6.2 · Tsunami Siaga")
        );

        // Record 3: no Dirasakan → magnitude fallback (M5.1 → 0.55), chip omits MMI.
        assert_eq!(ev[2].raw.get("mmi").unwrap(), &Value::Null);
        assert!((ev[2].severity.value() - 0.55).abs() < 1e-9);
        assert_eq!(felt_chip(&ev[2].raw).as_deref(), Some("M5.1"));
    }

    #[test]
    fn empty_list_is_ok_not_error() {
        // A quiet window (no recent felt quakes) is the normal state, not a failure.
        let ev = parse_bmkg(r#"{"Infogempa":{"gempa":[]}}"#).unwrap();
        assert!(ev.is_empty());
    }

    #[test]
    fn single_object_gempa_is_tolerated() {
        // The latest-quake product ships `gempa` as a single object, not an array.
        let json = r#"{"Infogempa":{"gempa":{"DateTime":"2026-06-21T09:16:30+00:00",
          "Coordinates":"-8.65,115.21","Magnitude":"4.8","Wilayah":"Bali",
          "Potensi":"Tidak berpotensi tsunami","Dirasakan":"IV Denpasar"}}}"#;
        let ev = parse_bmkg(json).unwrap();
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].raw.get("mmi").and_then(Value::as_u64), Some(4));
    }

    #[test]
    fn errors_on_bad_input() {
        // Missing the Infogempa.gempa node is malformed.
        assert!(parse_bmkg(r#"{"foo":1}"#).is_err());
        // Not JSON at all (e.g. an HTML 403 page).
        assert!(parse_bmkg("<html>403</html>").is_err());
    }

    #[test]
    fn mmi_and_tsunami_parsing() {
        // Peak MMI across a multi-place Dirasakan list, including a hyphenated range.
        assert_eq!(max_mmi("IV-V Pacitan, III Trenggalek, II Malang"), Some(5));
        assert_eq!(max_mmi("II Denpasar"), Some(2));
        assert_eq!(max_mmi(""), None); // no MMI present
        // Region names are mixed-case, so they never produce a spurious IVX run.
        assert_eq!(max_mmi("III Mataram"), Some(3));

        assert_eq!(tsunami_level("Tidak berpotensi tsunami"), None);
        assert_eq!(tsunami_level("Siaga"), Some("Siaga"));
        assert_eq!(tsunami_level("Waspada"), Some("Waspada"));
        assert_eq!(tsunami_level("Awas"), Some("Awas"));
    }

    #[test]
    fn severity_ladder() {
        // MMI ladder.
        assert!((severity_for(Some(2), Some(3.0), None) - 0.25).abs() < 1e-9);
        assert!((severity_for(Some(6), Some(6.0), None) - 0.7).abs() < 1e-9);
        assert!((severity_for(Some(9), Some(7.5), None) - 1.0).abs() < 1e-9);
        // Tsunami floor lifts a modest-MMI quake.
        assert!((severity_for(Some(4), Some(6.5), Some("Awas")) - 1.0).abs() < 1e-9);
        // Magnitude fallback when MMI is absent.
        assert!((severity_for(None, Some(6.0), None) - 0.7).abs() < 1e-9);
    }
}
