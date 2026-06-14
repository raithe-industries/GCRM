//! NAV CANADA NOTAMs — live airspace/aerodrome hazards (runway & taxiway closures,
//! runway-surface conditions, navaid outages, airspace restrictions) at major Canadian
//! airports, from NAV CANADA, the country's civil air-navigation service provider.
//! Free, no API key.
//!
//! Reads the NAV CANADA `weather/api/alpha` NOTAM endpoint (queried across a curated
//! set of major Canadian aerodromes in one call) into normalized [`EventKind::Aircraft`]
//! [`Event`]s. This is the *hazard* complement to [`crate::opensky`]'s live aircraft
//! POSITIONS — no other feed carries NOTAMs. Records are geocoded from the ICAO Q-line
//! coordinate embedded in the NOTAM text, falling back to a fixed aerodrome lookup.

use async_trait::async_trait;
use chrono::{NaiveDateTime, TimeZone, Utc};
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use std::collections::HashSet;
use std::time::Duration;

/// Curated major Canadian aerodromes (every province + territory). The API aggregates
/// repeated `site=` params (and surfaces nearby aerodromes too — all geocodable).
const SITES: [&str; 16] = [
    "CYYZ", "CYVR", "CYUL", "CYYC", "CYEG", "CYWG", "CYOW", "CYHZ", "CYQB", "CYYJ",
    "CYXE", "CYQR", "CYYT", "CYZF", "CYXY", "CYFB",
];

/// NAV CANADA NOTAM source.
#[derive(Default)]
pub struct NavCanada;

impl NavCanada {
    pub fn url(&self) -> String {
        let sites: String = SITES.iter().map(|s| format!("site={s}&")).collect();
        format!("https://plan.navcanada.ca/weather/api/alpha/?{sites}alpha=notam")
    }
}

#[async_trait]
impl Source for NavCanada {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "navcanada",
            name: "NAV CANADA NOTAMs",
            domain: EventKind::Aircraft,
            cadence: Duration::from_secs(600),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        let client = reqwest::Client::builder()
            .user_agent("engineering-effects/0.1 (+https://raithe.ca)")
            .build()?;
        let body = client.get(self.url()).send().await?.text().await?;
        parse_navcanada(&body)
    }
}

/// Fixed coordinate for a queried ICAO code — the fallback when a NOTAM carries no
/// Q-line coordinate.
fn icao_coord(icao: &str) -> Option<(f64, f64)> {
    let c = match icao {
        "CYYZ" => (43.6772, -79.6306),
        "CYVR" => (49.1939, -123.1844),
        "CYUL" => (45.4706, -73.7408),
        "CYYC" => (51.1139, -114.0203),
        "CYEG" => (53.3097, -113.5797),
        "CYWG" => (49.9100, -97.2399),
        "CYOW" => (45.3225, -75.6692),
        "CYHZ" => (44.8808, -63.5086),
        "CYQB" => (46.7911, -71.3933),
        "CYYJ" => (48.6469, -123.4258),
        "CYXE" => (52.1708, -106.6997),
        "CYQR" => (50.4319, -104.6658),
        "CYYT" => (47.6186, -52.7519),
        "CYZF" => (62.4628, -114.4403),
        "CYXY" => (60.7096, -135.0676),
        "CYFB" => (63.7564, -68.5558),
        _ => return None,
    };
    Some(c)
}

/// Decimal (lat, lon) from the ICAO Q-line coordinate token `ddmm[NS]dddmm[EW]`
/// (e.g. `4341N07938W` = 43°41′N 079°38′W) embedded in a NOTAM's raw text.
fn qline_coord(raw: &str) -> Option<(f64, f64)> {
    let b = raw.as_bytes();
    if b.len() < 11 {
        return None;
    }
    for i in 0..=b.len() - 11 {
        let w = &b[i..i + 11];
        let digits = |s: &[u8]| s.iter().all(|c| c.is_ascii_digit());
        if digits(&w[0..4])
            && (w[4] == b'N' || w[4] == b'S')
            && digits(&w[5..10])
            && (w[10] == b'E' || w[10] == b'W')
        {
            let n = |s: &[u8]| std::str::from_utf8(s).ok().and_then(|x| x.parse::<f64>().ok());
            let lat = n(&w[0..2])? + n(&w[2..4])? / 60.0;
            let lon = n(&w[5..8])? + n(&w[8..10])? / 60.0;
            let lat = if w[4] == b'S' { -lat } else { lat };
            let lon = if w[10] == b'W' { -lon } else { lon };
            return Some((lat, lon));
        }
    }
    None
}

/// Severity from the hazard keywords in a NOTAM's (uppercased) raw text. Active-runway
/// closures are loudest; routine surface conditions and informational items quiet.
fn severity_for(raw_upper: &str) -> f64 {
    if raw_upper.contains("RWY") && raw_upper.contains("CLSD") {
        0.8
    } else if raw_upper.contains("SIGMET") || raw_upper.contains("SEV TURB") {
        0.7
    } else if raw_upper.contains("TWY") && raw_upper.contains("CLSD") {
        0.4
    } else if raw_upper.contains("RSC") {
        0.4
    } else if raw_upper.contains("CLSD") || raw_upper.contains("CLOSED") {
        0.5
    } else {
        0.3
    }
}

/// Short hazard tag for the map popup chip.
pub fn hazard_tag(raw_upper: &str) -> &'static str {
    if raw_upper.contains("RWY") && raw_upper.contains("CLSD") {
        "Runway closed"
    } else if raw_upper.contains("TWY") && raw_upper.contains("CLSD") {
        "Taxiway closed"
    } else if raw_upper.contains("RSC") {
        "Runway surface"
    } else if raw_upper.contains("SIGMET") || raw_upper.contains("SEV TURB") {
        "SIGMET"
    } else if raw_upper.contains("CLSD") || raw_upper.contains("CLOSED") {
        "Closure"
    } else {
        "NOTAM"
    }
}

/// One-line plain-language summary: the NOTAM's `E)` field collapsed to a single line.
fn summary(raw: &str) -> String {
    let body = raw.find("E)").map(|i| &raw[i + 2..]).unwrap_or(raw);
    let oneline = body.split_whitespace().collect::<Vec<_>>().join(" ");
    oneline.chars().take(90).collect()
}

/// Parse a NAV CANADA naive timestamp (`2026-06-14T16:57:00`, no zone) as UTC.
fn parse_time(s: &str) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").ok()
}

/// Pure parser: NAV CANADA alpha-NOTAM JSON -> events. Unit-tested offline.
///
/// Keeps only currently-active NOTAMs (now within `startValidity`..`endValidity`),
/// dedupes on `pk` (the multi-site query overlaps nearby aerodromes), double-decodes the
/// `text` field (a JSON string wrapping `{raw,english,french}`), and geocodes from the
/// Q-line coordinate, falling back to the aerodrome `location`.
pub fn parse_navcanada(json: &str) -> anyhow::Result<Vec<Event>> {
    let root: serde_json::Value = serde_json::from_str(json)?;
    let data = root
        .get("data")
        .and_then(|d| d.as_array())
        .ok_or_else(|| anyhow::anyhow!("navcanada: missing 'data' array"))?;

    let now = Utc::now().naive_utc();
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for rec in data {
        if rec.get("type").and_then(|t| t.as_str()) != Some("notam") {
            continue;
        }
        let Some(pk) = rec.get("pk").and_then(|v| v.as_str().map(String::from).or_else(|| v.as_i64().map(|i| i.to_string()))) else {
            continue;
        };
        if !seen.insert(pk.clone()) {
            continue;
        }

        // Active window: drop NOTAMs not yet started or already expired.
        if let Some(start) = rec.get("startValidity").and_then(|v| v.as_str()).and_then(parse_time) {
            if start > now {
                continue;
            }
        }
        if let Some(end) = rec.get("endValidity").and_then(|v| v.as_str()).and_then(parse_time) {
            if end < now {
                continue;
            }
        }

        // `text` is a JSON string wrapping {raw, english, french}; decode it again.
        let raw_text = rec
            .get("text")
            .and_then(|v| v.as_str())
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
            .and_then(|inner| inner.get("raw").and_then(|r| r.as_str()).map(String::from))
            .unwrap_or_default();
        if raw_text.is_empty() {
            continue;
        }

        let location = rec.get("location").and_then(|v| v.as_str()).unwrap_or("");
        let Some((lat, lon)) = qline_coord(&raw_text).or_else(|| icao_coord(location)) else {
            continue;
        };
        let Some(geo) = Geo::new(lat, lon) else { continue };

        let raw_upper = raw_text.to_ascii_uppercase();
        let title = format!("NOTAM {location}: {}", summary(&raw_text));
        let time = rec
            .get("startValidity")
            .and_then(|v| v.as_str())
            .and_then(parse_time)
            .map(|ndt| Utc.from_utc_datetime(&ndt))
            .unwrap_or_else(Utc::now);

        out.push(Event {
            id: format!("notam-{pk}"),
            source_id: "navcanada".to_string(),
            kind: EventKind::Aircraft,
            title,
            time,
            geo: Some(geo),
            severity: Severity::new(severity_for(&raw_upper)),
            url: Some("https://plan.navcanada.ca/".to_string()),
            raw: serde_json::json!({ "location": location, "tag": hazard_tag(&raw_upper) }),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qline_parses_dms() {
        // 43°41′N 079°38′W -> ~43.683, -79.633.
        let (lat, lon) = qline_coord("Q) CZYZ/QMRLC/IV/NBO/A/000/999/4341N07938W005").unwrap();
        assert!((lat - 43.6833).abs() < 1e-3 && (lon + 79.6333).abs() < 1e-3);
        assert!(qline_coord("no coordinate here").is_none());
    }

    fn rec(pk: &str, loc: &str, start: &str, end: &str, e_text: &str) -> String {
        // `text` must itself be a JSON string (double-encoded), like the live API.
        let inner = serde_json::json!({ "raw": format!("Q) CZYZ/QMRLC/IV/NBO/A/000/999/4341N07938W005\nA) {loc} B) x C) y\nE) {e_text}") }).to_string();
        serde_json::json!({
            "type": "notam", "pk": pk, "location": loc,
            "startValidity": start, "endValidity": end, "text": inner
        })
        .to_string()
    }

    #[test]
    fn parses_active_dedupes_and_filters() {
        // Active runway closure (far-future end), an expired one, and a duplicate pk.
        let body = format!(
            r#"{{"data":[{a},{b},{c}]}}"#,
            a = rec("1", "CYYZ", "2026-01-01T00:00:00", "2099-01-01T00:00:00", "RWY 06L/24R CLSD"),
            b = rec("2", "CYVR", "2000-01-01T00:00:00", "2000-01-02T00:00:00", "TWY A CLSD"),
            c = rec("1", "CYYZ", "2026-01-01T00:00:00", "2099-01-01T00:00:00", "RWY 06L/24R CLSD"),
        );
        let ev = parse_navcanada(&body).unwrap();
        // Expired NOTAM dropped; duplicate pk dropped -> one event.
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].id, "notam-1");
        assert_eq!(ev[0].kind, EventKind::Aircraft);
        assert!(ev[0].title.starts_with("NOTAM CYYZ: RWY 06L/24R CLSD"));
        // Runway closure is loud.
        assert!((ev[0].severity.value() - 0.8).abs() < 1e-9);
        assert_eq!(ev[0].raw.get("tag").unwrap().as_str().unwrap(), "Runway closed");
    }

    #[test]
    fn errors_on_missing_data() {
        assert!(parse_navcanada(r#"{"meta":{}}"#).is_err());
    }
}
