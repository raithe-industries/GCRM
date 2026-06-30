//! JMA (Japan Meteorological Agency) — Japan's national meteorological/seismological
//! agency and the WMO-designated authority for the region. Free, no API key.
//! Attribution: "気象庁 / Japan Meteorological Agency".
//!
//! Reads the open `bosai/quake/data/list.json` product — the rolling list of recent
//! earthquake bulletins. Each record carries an inline `cod` (an ISO-6709 coordinate
//! string, e.g. `+37.7+141.7-50000/` = lat +37.7, lon +141.7, depth 50 km), the
//! magnitude `mag`, the epicentre area name (`anm` / `en_anm`), the bulletin type
//! (`ttl` / `en_ttl`), the event id `eid`, and — the signal no raw quake catalogue
//! carries — `maxi`, the **maximum observed JMA seismic intensity (Shindo)** on Japan's
//! national 0–7 scale (`1,2,3,4,5-,5+,6-,6+,7`). Emits one normalized
//! [`EventKind::Earthquake`] event per quake at its epicentre.
//!
//! Why this isn't another USGS/EMSC quake feed: those are raw *detection* catalogues
//! (every instrument-detected event, magnitude only). JMA `list.json` filtered to events
//! with an observed Shindo is a **human-impact** product — only quakes that produced
//! measurable shaking on the ground, graded by the baseline-relative Shindo intensity
//! they were felt at, over Japan / the NW-Pacific (a key non-North-America theatre).
//! Shindo is a defined ground-shaking scale (each level a named effect), so a
//! "Shindo 5+" dot is real, unit-bearing signal, not a raw number. JMA's Shindo is a
//! distinct national scale from Indonesia's MMI (`bmkg_quake`) and complements it.
//!
//! JMA issues several bulletin types for the *same* quake (intensity bulletin →
//! hypocentre+intensity → updates), so records are **deduplicated by `eid`**, keeping
//! the record with the highest reported Shindo. Bulletins with no hypocentre (the
//! `震度速報` intensity flash has no `cod`) or no observed Shindo (a hypocentre-only
//! notice for a quake nobody felt) are dropped — those are exactly what USGS/EMSC
//! already cover.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

/// JMA recent-earthquake source (`bosai` open data).
#[derive(Default)]
pub struct JmaQuake;

impl JmaQuake {
    pub fn url(&self) -> &'static str {
        "https://www.jma.go.jp/bosai/quake/data/list.json"
    }
}

#[async_trait]
impl Source for JmaQuake {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "jma_quake",
            name: "JMA Seismic Intensity (Japan)",
            domain: EventKind::Earthquake,
            cadence: Duration::from_secs(600),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        let body = crate::http::fetch_text(self.url()).await?;
        parse_jma(&body)
    }
}

/// A numeric rank for a JMA Shindo token, used both to order severity and to pick the
/// loudest bulletin when several share an `eid`. The JMA scale is 0–7 with a lower/upper
/// split at 5 and 6: `5-` (5 lower) < `5+` (5 upper) < `6-` < `6+` < `7`. `None` for an
/// absent/unknown intensity (e.g. an empty string or `不明`) — such records are dropped.
/// Tolerates the ASCII hyphen, the Unicode minus, and the Japanese 弱/強 (weak/strong) forms.
pub fn shindo_rank(maxi: &str) -> Option<f64> {
    Some(match maxi.trim() {
        "1" => 1.0,
        "2" => 2.0,
        "3" => 3.0,
        "4" => 4.0,
        "5-" | "5−" | "5弱" => 5.0,
        "5+" | "5強" => 5.5,
        "6-" | "6−" | "6弱" => 6.0,
        "6+" | "6強" => 6.5,
        "7" => 7.0,
        _ => return None,
    })
}

/// Normalized 0–1 severity from the JMA Shindo rank. Shindo 1 is barely perceptible;
/// 5+ causes furniture to topple; 7 is the maximum (heavy structural damage).
fn severity_for(rank: f64) -> f64 {
    match rank {
        r if r >= 7.0 => 1.0,
        r if r >= 6.5 => 0.93, // 6+
        r if r >= 6.0 => 0.85, // 6-
        r if r >= 5.5 => 0.75, // 5+
        r if r >= 5.0 => 0.65, // 5-
        r if r >= 4.0 => 0.52, // 4
        r if r >= 3.0 => 0.38, // 3
        r if r >= 2.0 => 0.25, // 2
        _ => 0.15,             // 1
    }
}

/// Operator chip for a JMA quake: the peak Shindo intensity + magnitude, e.g.
/// "Shindo 5+ · M6.1" / "Shindo 3 · M4.2".
pub fn quake_chip(raw: &Value) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(s) = raw.get("shindo").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        parts.push(format!("Shindo {s}"));
    }
    if let Some(mag) = raw.get("magnitude").and_then(Value::as_f64) {
        parts.push(format!("M{mag:.1}"));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" · "))
    }
}

/// Read JSON as f64 whether it's a number or a numeric string ("5.3"); `None` when absent
/// or non-numeric ("" / "M不明").
fn num(v: Option<&Value>) -> Option<f64> {
    v.and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())))
}

/// Read the `maxi` Shindo value as a string whether the wire encodes it as a string
/// ("5+") or a bare number (`4`).
fn read_maxi(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// Parse a JMA ISO-6709 coordinate string ("+37.7+141.7-50000/") into
/// (lat°, lon°, depth_m). Splits into signed decimal tokens; the first two are lat/lon,
/// the optional third is depth in metres. `None` if fewer than two parseable tokens.
fn parse_iso6709(s: &str) -> Option<(f64, f64, Option<f64>)> {
    let s = s.trim().trim_end_matches('/');
    if s.is_empty() {
        return None;
    }
    let mut tokens: Vec<String> = Vec::new();
    let mut cur = String::new();
    for c in s.chars() {
        if (c == '+' || c == '-') && !cur.is_empty() {
            tokens.push(std::mem::take(&mut cur));
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    let lat: f64 = tokens.first()?.parse().ok()?;
    let lon: f64 = tokens.get(1)?.parse().ok()?;
    let depth_m = tokens.get(2).and_then(|t| t.parse::<f64>().ok());
    Some((lat, lon, depth_m))
}

/// Pure parser: JMA `quake/data/list.json` → events. The product is a top-level array of
/// bulletins; an empty array is the normal quiet-window case (Ok, zero events), anything
/// that isn't a JSON array is malformed (error). Records with no inline `cod` hypocentre
/// or no observed Shindo are dropped; the remaining ones are deduplicated by `eid`,
/// keeping the bulletin with the highest reported intensity.
pub fn parse_jma(json: &str) -> anyhow::Result<Vec<Event>> {
    let root: Value = serde_json::from_str(json)?;
    let arr = root
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("jma_quake: expected a top-level JSON array"))?;

    // First-seen order + best (highest-Shindo) bulletin per event id.
    let mut order: Vec<String> = Vec::new();
    let mut best: HashMap<String, (f64, Event)> = HashMap::new();

    for r in arr {
        // Require an inline epicentre (the intensity-flash bulletin has none → dropped:
        // USGS/EMSC carry pure-detection events).
        let Some((lat, lon, depth_m)) = r.get("cod").and_then(Value::as_str).and_then(parse_iso6709)
        else {
            continue;
        };
        let Some(geo) = Geo::new(lat, lon) else {
            continue;
        };

        // Require an observed Shindo — this is the human-impact filter that makes the feed
        // non-duplicative of the raw detection catalogues.
        let maxi = read_maxi(r.get("maxi"));
        let Some(rank) = shindo_rank(&maxi) else {
            continue;
        };

        let mag = num(r.get("mag"));
        let area = r
            .get("en_anm")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or_else(|| r.get("anm").and_then(Value::as_str))
            .unwrap_or("")
            .trim();
        let depth_km = depth_m.map(|d| (d.abs() / 1000.0).round());

        let time = r
            .get("at")
            .and_then(Value::as_str)
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        let eid = r
            .get("eid")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("{lat:.3},{lon:.3}"));

        let title = match (mag, area.is_empty()) {
            (Some(m), false) => format!("M{m:.1} earthquake — {area}"),
            (Some(m), true) => format!("M{m:.1} earthquake"),
            (None, false) => format!("Earthquake — {area}"),
            (None, true) => "Earthquake".to_string(),
        };

        let ev = Event {
            id: format!("jma-{eid}"),
            source_id: "jma_quake".to_string(),
            kind: EventKind::Earthquake,
            title,
            time,
            geo: Some(geo),
            severity: Severity::new(severity_for(rank)),
            url: Some("https://www.jma.go.jp/bosai/map.html#contents=earthquake_map".to_string()),
            raw: serde_json::json!({
                "magnitude": mag,
                "shindo": maxi,
                "area": area,
                "depth_km": depth_km,
            }),
        };

        // Keep the loudest bulletin for each event (a later update may raise the intensity).
        match best.get(&eid) {
            Some((prev, _)) if *prev >= rank => {}
            _ => {
                if !best.contains_key(&eid) {
                    order.push(eid.clone());
                }
                best.insert(eid, (rank, ev));
            }
        }
    }

    Ok(order
        .into_iter()
        .filter_map(|e| best.remove(&e).map(|(_, ev)| ev))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Built from the real JMA `quake/data/list.json` shape (a top-level array of bulletins;
    // ISO-6709 `cod`; `maxi` Shindo token; `eid` shared across a quake's bulletins).
    // Records 1+2: the SAME event (eid E1) — a hypocentre+intensity bulletin (Shindo 5-)
    // then an update raising it to 5+; dedup keeps the louder 5+. Record 3: a separate
    // smaller quake (Shindo 3). Record 4: a hypocentre-only notice for an UNFELT quake
    // (no `maxi`) → dropped (USGS/EMSC carry it). Record 5: an intensity flash with no
    // hypocentre (`cod` empty) → dropped.
    const FIXTURE: &str = r#"[
      {"ttl":"震源・震度に関する情報","en_ttl":"Earthquake Information","eid":"E1",
       "at":"2026-06-21T09:16:30+09:00","cod":"+38.0+142.0-30000/","mag":"6.1","maxi":"5-",
       "anm":"福島県沖","en_anm":"Off Fukushima Prefecture"},
      {"ttl":"震源・震度に関する情報","en_ttl":"Earthquake Information","eid":"E1",
       "at":"2026-06-21T09:20:00+09:00","cod":"+38.0+142.0-30000/","mag":"6.1","maxi":"5+",
       "anm":"福島県沖","en_anm":"Off Fukushima Prefecture"},
      {"ttl":"震源・震度に関する情報","en_ttl":"Earthquake Information","eid":"E2",
       "at":"2026-06-20T22:05:00+09:00","cod":"+34.5+135.5-10000/","mag":"4.2","maxi":"3",
       "anm":"大阪府北部","en_anm":"Northern Osaka Prefecture"},
      {"ttl":"震源に関する情報","en_ttl":"Hypocenter Information","eid":"E3",
       "at":"2026-06-20T03:00:00+09:00","cod":"+30.0+140.0-400000/","mag":"5.0","maxi":"",
       "anm":"父島近海","en_anm":"Near Chichijima Island"},
      {"ttl":"震度速報","en_ttl":"Seismic Intensity Information","eid":"E4",
       "at":"2026-06-19T11:00:00+09:00","cod":"","mag":"","maxi":"4",
       "anm":"","en_anm":""}
    ]"#;

    #[test]
    fn parses_fixture() {
        let ev = parse_jma(FIXTURE).unwrap();
        // E3 (no Shindo) and E4 (no hypocentre) dropped; E1's two bulletins dedup → 2 events.
        assert_eq!(ev.len(), 2);

        // E1: deduped to the louder 5+ bulletin.
        assert_eq!(ev[0].id, "jma-E1");
        assert_eq!(ev[0].kind, EventKind::Earthquake);
        assert_eq!(ev[0].title, "M6.1 earthquake — Off Fukushima Prefecture");
        let g = ev[0].geo.unwrap();
        assert!((g.lat - 38.0).abs() < 1e-6 && (g.lon - 142.0).abs() < 1e-6);
        // Shindo 5+ → 0.75.
        assert!((ev[0].severity.value() - 0.75).abs() < 1e-9);
        assert_eq!(ev[0].raw.get("shindo").unwrap(), "5+");
        assert_eq!(ev[0].raw.get("depth_km").and_then(Value::as_f64), Some(30.0));
        assert_eq!(quake_chip(&ev[0].raw).as_deref(), Some("Shindo 5+ · M6.1"));

        // E2: a smaller quake, Shindo 3 → 0.38.
        assert_eq!(ev[1].id, "jma-E2");
        assert!((ev[1].severity.value() - 0.38).abs() < 1e-9);
        assert_eq!(quake_chip(&ev[1].raw).as_deref(), Some("Shindo 3 · M4.2"));
    }

    #[test]
    fn empty_array_is_ok_not_error() {
        // A quiet window (no recent bulletins) is the normal state, not a failure.
        let ev = parse_jma("[]").unwrap();
        assert!(ev.is_empty());
    }

    #[test]
    fn errors_on_bad_input() {
        // Not an array (the product is a top-level array).
        assert!(parse_jma(r#"{"foo":1}"#).is_err());
        // Not JSON at all (e.g. an HTML 403 page).
        assert!(parse_jma("<html>403</html>").is_err());
    }

    #[test]
    fn dedup_keeps_highest_shindo_regardless_of_order() {
        // Even if the louder bulletin arrives first, the quake plots once at its peak.
        let json = r#"[
          {"eid":"X","at":"2026-06-21T09:20:00+09:00","cod":"+38.0+142.0-30000/","mag":"6.1","maxi":"6-"},
          {"eid":"X","at":"2026-06-21T09:16:30+09:00","cod":"+38.0+142.0-30000/","mag":"6.1","maxi":"4"}
        ]"#;
        let ev = parse_jma(json).unwrap();
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].raw.get("shindo").unwrap(), "6-");
        assert!((ev[0].severity.value() - 0.85).abs() < 1e-9);
    }

    #[test]
    fn shindo_rank_and_severity_ladder() {
        // The lower/upper split orders correctly and unknown intensities are rejected.
        assert!(shindo_rank("5-").unwrap() < shindo_rank("5+").unwrap());
        assert!(shindo_rank("5+").unwrap() < shindo_rank("6-").unwrap());
        assert_eq!(shindo_rank("7"), Some(7.0));
        assert_eq!(shindo_rank("5弱"), Some(5.0)); // Japanese weak/strong forms tolerated
        assert_eq!(shindo_rank("5強"), Some(5.5));
        assert_eq!(shindo_rank(""), None);
        assert_eq!(shindo_rank("不明"), None);
        // Severity ladder anchors.
        assert!((severity_for(7.0) - 1.0).abs() < 1e-9);
        assert!((severity_for(5.5) - 0.75).abs() < 1e-9);
        assert!((severity_for(1.0) - 0.15).abs() < 1e-9);
    }

    #[test]
    fn iso6709_parsing() {
        // Standard lat/lon/depth, depth returned in metres.
        let (lat, lon, d) = parse_iso6709("+37.7+141.7-50000/").unwrap();
        assert!((lat - 37.7).abs() < 1e-9 && (lon - 141.7).abs() < 1e-9);
        assert!((d.unwrap() + 50000.0).abs() < 1e-9);
        // Southern/western signs.
        let (lat, lon, _) = parse_iso6709("-8.5+115.2-10000/").unwrap();
        assert!((lat + 8.5).abs() < 1e-9 && (lon - 115.2).abs() < 1e-9);
        // No depth component.
        let (_, _, d) = parse_iso6709("+34+135/").unwrap();
        assert!(d.is_none());
        // Empty / malformed → None.
        assert!(parse_iso6709("").is_none());
        assert!(parse_iso6709("+37.7/").is_none());
    }
}
