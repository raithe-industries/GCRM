//! PVMBG / MAGMA Indonesia — Indonesian volcano operational alert levels.
//!
//! Indonesia has more active volcanoes than any country on Earth (~127 monitored),
//! and PVMBG (Pusat Vulkanologi dan Mitigasi Bencana Geologi — the Geological
//! Agency of the Ministry of Energy and Mineral Resources) is the authoritative
//! national monitor. Each monitored volcano carries a **ground alert level**
//! `ga_status` on PVMBG's four-step scale — **1 Normal, 2 Waspada (Advisory),
//! 3 Siaga (Watch), 4 Awas (Warning)** — and, when ash is a hazard to aviation,
//! a **VONA** (Volcano Observatory Notice for Aviation) with the ICAO aviation
//! colour code (GREEN/YELLOW/ORANGE/RED). This connector emits one normalized
//! [`EventKind::Volcano`] [`Event`] per volcano **above background** (status ≥ 2);
//! volcanoes at Normal (status 1) are dropped, so an all-quiet network yields zero
//! events, not an error — exactly as `usgs_volcano` (US/Alaska) and
//! `geonet_volcano` (NZ) do for their regions.
//!
//! ## Ingestion — Path B (committed snapshot)
//! MAGMA's home-map volcano list (current alert level per volcano + latest VONA
//! colour, the `ga_*`/`vona[]` shape) is embedded server-side in the
//! `magma.esdm.go.id` page rather than exposed as a clean public full-list JSON
//! endpoint, and the host is unreachable from the cloud build sandbox (403). So
//! this ships as a **Path-B snapshot**: a real captured PVMBG payload committed
//! alongside the connector ([`SNAPSHOT`], `include_str!`-embedded), refreshed by a
//! local/manual re-capture job that re-commits the array. The wire schema is real
//! (confirmed against PVMBG's own `magma-indonesia/magma-indonesia` source — the
//! `HomeController::gunungApi()` select and `VonaApiService` mappings — and a
//! captured copy in `mandalateknologi/demo-peta`), so the parser is built against
//! genuine bytes, not documentation guesswork.

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use serde_json::Value;
use std::time::Duration;

/// Real captured PVMBG/MAGMA volcano-alert snapshot (see module docs for refresh).
pub const SNAPSHOT: &str = include_str!("magma_volcano_snapshot.json");

/// PVMBG / MAGMA Indonesia volcano-alert source (Path-B committed snapshot).
#[derive(Default)]
pub struct MagmaVolcano;

#[async_trait]
impl Source for MagmaVolcano {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "magma_volcano",
            name: "MAGMA Indonesia Volcano Alert Levels (PVMBG)",
            domain: EventKind::Volcano,
            cadence: Duration::from_secs(3600),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        // Path B: the snapshot is committed with the connector; no live host is
        // reachable for the full-list feed. A local job refreshes the file.
        parse_magma_volcano(SNAPSHOT)
    }
}

/// PVMBG ground alert-level rank (0 = Normal/all-clear). Awas is the danger state;
/// Waspada is the first step above background.
fn status_rank(level: i64) -> f64 {
    match level {
        4 => 1.0,  // Awas (Warning)
        3 => 0.8,  // Siaga (Watch)
        2 => 0.55, // Waspada (Advisory)
        _ => 0.0,  // Normal / unknown
    }
}

/// PVMBG alert-level name (None for Normal/unknown — those don't plot).
fn level_name(level: i64) -> Option<&'static str> {
    match level {
        4 => Some("Awas (Warning)"),
        3 => Some("Siaga (Watch)"),
        2 => Some("Waspada (Advisory)"),
        _ => None,
    }
}

/// ICAO aviation colour-code rank (0 = green/unassigned). RED = significant ash
/// imminent/underway; ORANGE = eruption likely/minor ash; YELLOW = elevated unrest.
fn color_rank(s: &str) -> f64 {
    match s.trim().to_ascii_uppercase().as_str() {
        "RED" => 1.0,
        "ORANGE" => 0.8,
        "YELLOW" => 0.55,
        _ => 0.0,
    }
}

/// Title-case an aviation code ("ORANGE" -> "Orange").
fn titlecase(s: &str) -> String {
    let s = s.trim();
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase(),
        None => String::new(),
    }
}

/// A JSON value that may be a number or a numeric string.
fn as_f64_loose(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
}

/// `ga_status` may arrive as a JSON number or a numeric string; normalize to int.
fn status_int(v: &Value) -> i64 {
    match v.get("ga_status") {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)).unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse::<i64>().unwrap_or(0),
        _ => 0,
    }
}

/// The latest VONA notice for a volcano (the `vona` entry with the largest `no`),
/// or the first entry if `no` is absent. None when there are no VONA notices.
fn latest_vona(v: &Value) -> Option<&Value> {
    let arr = v.get("vona").and_then(Value::as_array)?;
    arr.iter()
        .filter(|e| e.is_object())
        .max_by_key(|e| e.get("no").and_then(Value::as_i64).unwrap_or(i64::MIN))
        .or_else(|| arr.first())
}

/// Latest VONA aviation colour code (uppercased), if any.
fn latest_avcode(v: &Value) -> Option<String> {
    latest_vona(v)
        .and_then(|e| e.get("cu_avcode"))
        .and_then(Value::as_str)
        .map(|s| s.trim().to_ascii_uppercase())
        .filter(|s| !s.is_empty())
}

/// MAGMA VONA timestamps look like `"2026-04-06 04:00:00"` (UTC).
fn parse_magma_time(s: &str) -> Option<DateTime<Utc>> {
    chrono::NaiveDateTime::parse_from_str(s.trim(), "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|n| Utc.from_utc_datetime(&n))
}

/// Tolerate a bare JSON array or a `{volcanoes:[…]}` / `{data:[…]}` wrapper.
fn rows(root: &Value) -> Option<&Vec<Value>> {
    root.as_array()
        .or_else(|| root.get("volcanoes").and_then(Value::as_array))
        .or_else(|| root.get("data").and_then(Value::as_array))
}

/// Operator chip: PVMBG alert level plus the latest VONA aviation colour, e.g.
/// "Alert Siaga (Watch) · Aviation Yellow". `raw` is the volcano record. At least
/// one side carries signal (else the volcano was dropped as Normal/all-clear).
pub fn alert_chip(raw: &Value) -> Option<String> {
    let head = level_name(status_int(raw)).map(|n| format!("Alert {n}"));
    let tail = latest_avcode(raw)
        .filter(|c| color_rank(c) > 0.0)
        .map(|c| format!("Aviation {}", titlecase(&c)));
    match (head, tail) {
        (Some(h), Some(t)) => Some(format!("{h} · {t}")),
        (Some(h), None) => Some(h),
        (None, Some(t)) => Some(t),
        (None, None) => None,
    }
}

/// Pure parser: MAGMA volcano-alert JSON -> events. Unit-tested offline. A
/// malformed payload is an error; volcanoes at Normal (status 1) or with no usable
/// coordinate are filtered out, so an all-quiet network is Ok/empty.
pub fn parse_magma_volcano(json: &str) -> anyhow::Result<Vec<Event>> {
    let root: Value = serde_json::from_str(json)?;
    let arr = rows(&root)
        .ok_or_else(|| anyhow::anyhow!("magma_volcano: payload is not a volcano array"))?;

    let mut out = Vec::with_capacity(arr.len());
    for v in arr {
        let Some(code) = v
            .get("ga_code")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };

        // Drop Normal (status 1) / unknown: a non-elevated dot carries no risk
        // signal. The aviation colour can raise severity above the ground level
        // (e.g. an ash-erupting Waspada volcano flagged ORANGE).
        let severity = status_rank(status_int(v)).max(color_rank(latest_avcode(v).as_deref().unwrap_or("")));
        if severity <= 0.0 {
            continue;
        }

        let lat = v.get("ga_lat_gapi").and_then(as_f64_loose);
        let lon = v.get("ga_lon_gapi").and_then(as_f64_loose);
        let (Some(lat), Some(lon)) = (lat, lon) else { continue };
        let Some(geo) = Geo::new(lat, lon) else { continue };

        let name = v
            .get("ga_nama_gapi")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("Volcano");
        let time = latest_vona(v)
            .and_then(|e| e.get("issued_time"))
            .and_then(Value::as_str)
            .and_then(parse_magma_time)
            .unwrap_or_else(Utc::now);

        out.push(Event {
            id: format!("magma-volcano-{code}"),
            source_id: "magma_volcano".to_string(),
            kind: EventKind::Volcano,
            title: name.to_string(),
            time,
            geo: Some(geo),
            severity: Severity::new(severity),
            url: Some(format!("https://magma.esdm.go.id/v1/vona?code={code}")),
            raw: v.clone(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Built from the real PVMBG `ga_*`/`vona[]` shape: Merapi (Siaga/3, VONA
    // YELLOW), Dukono (Waspada/2 but VONA ORANGE — colour raises severity above the
    // ground level), Awu (Waspada/2, no VONA), a Normal entry (Agung, status 1 +
    // GREEN) that must be dropped as all-clear, and an elevated entry with no
    // coordinate (must be dropped — can't be placed).
    const FIXTURE: &str = r#"{"volcanoes":[
      {"ga_code":"MER","ga_nama_gapi":"Merapi","ga_status":3,"ga_lon_gapi":110.442,"ga_lat_gapi":-7.542,
       "vona":[{"no":20700,"issued_time":"2026-04-01 00:00:00","cu_avcode":"GREEN"},
               {"no":20714,"issued_time":"2026-04-02 00:00:00","cu_avcode":"YELLOW"}]},
      {"ga_code":"DUK","ga_nama_gapi":"Dukono","ga_status":"2","ga_lon_gapi":127.894,"ga_lat_gapi":1.693,
       "vona":[{"no":20766,"issued_time":"2026-04-06 04:00:00","cu_avcode":"ORANGE"}]},
      {"ga_code":"AWU","ga_nama_gapi":"Awu","ga_status":2,"ga_lon_gapi":125.45598,"ga_lat_gapi":3.682846,"vona":[]},
      {"ga_code":"AGU","ga_nama_gapi":"Agung","ga_status":1,"ga_lon_gapi":115.508,"ga_lat_gapi":-8.342,
       "vona":[{"no":20676,"issued_time":"2026-04-01 00:11:00","cu_avcode":"GREEN"}]},
      {"ga_code":"NOC","ga_nama_gapi":"NoCoords","ga_status":4,"vona":[]}
    ]}"#;

    #[test]
    fn parses_fixture_dropping_normal_and_no_coords() {
        let ev = parse_magma_volcano(FIXTURE).unwrap();
        // Agung (Normal/1) dropped as all-clear; NoCoords (status 4, no lat/lon) dropped.
        assert_eq!(ev.len(), 3);

        // Merapi: Siaga (0.8) vs latest VONA YELLOW (0.55) -> 0.8; latest VONA is the
        // max-`no` entry (YELLOW), not the older GREEN.
        let mer = ev.iter().find(|e| e.id == "magma-volcano-MER").unwrap();
        assert_eq!(mer.kind, EventKind::Volcano);
        assert_eq!(mer.title, "Merapi");
        assert!((mer.severity.value() - 0.8).abs() < 1e-9);
        let g = mer.geo.unwrap();
        assert!((g.lat + 7.542).abs() < 1e-6 && (g.lon - 110.442).abs() < 1e-6);
        assert_eq!(alert_chip(&mer.raw).as_deref(), Some("Alert Siaga (Watch) · Aviation Yellow"));
        assert_eq!(mer.time.format("%Y-%m-%d %H:%M").to_string(), "2026-04-02 00:00");

        // Dukono: ground level Waspada (0.55) but the ORANGE VONA raises severity to 0.8.
        let duk = ev.iter().find(|e| e.id == "magma-volcano-DUK").unwrap();
        assert!((duk.severity.value() - 0.8).abs() < 1e-9);
        assert_eq!(alert_chip(&duk.raw).as_deref(), Some("Alert Waspada (Advisory) · Aviation Orange"));

        // Awu: Waspada with no VONA -> level only, severity 0.55, chip has no aviation part.
        let awu = ev.iter().find(|e| e.id == "magma-volcano-AWU").unwrap();
        assert!((awu.severity.value() - 0.55).abs() < 1e-9);
        assert_eq!(alert_chip(&awu.raw).as_deref(), Some("Alert Waspada (Advisory)"));
    }

    #[test]
    fn all_normal_network_is_ok_not_error() {
        // Every monitored volcano at Normal -> zero plotted events, not a failure.
        let normal = r#"[{"ga_code":"AGU","ga_nama_gapi":"Agung","ga_status":1,
          "ga_lon_gapi":115.508,"ga_lat_gapi":-8.342,"vona":[{"no":1,"cu_avcode":"GREEN"}]}]"#;
        assert!(parse_magma_volcano(normal).unwrap().is_empty());
        // An empty array (bare or wrapped) is fine too.
        assert!(parse_magma_volcano("[]").unwrap().is_empty());
        assert!(parse_magma_volcano(r#"{"volcanoes":[]}"#).unwrap().is_empty());
    }

    #[test]
    fn errors_on_bad_input() {
        // Payload not JSON (e.g. an HTML 403 page).
        assert!(parse_magma_volcano("<html>403 Forbidden</html>").is_err());
        // JSON but not a volcano array.
        assert!(parse_magma_volcano(r#"{"oops":true}"#).is_err());
    }

    #[test]
    fn severity_ladders_and_chip_colour_only() {
        // Awas/RED is the apex; Waspada the floor of the plotted range.
        assert!((status_rank(4).max(color_rank("RED")) - 1.0).abs() < 1e-9);
        assert!((status_rank(2).max(color_rank("GREEN")) - 0.55).abs() < 1e-9);
        // Normal never plots, even with a stray colour rank of 0.
        assert!(status_rank(1).max(color_rank("GREEN")) <= 0.0);
        // A record at Normal but with an ORANGE aviation colour still surfaces a
        // colour-only chip (the chip describes whatever signal is present).
        let raw = serde_json::json!({"ga_status":1,"vona":[{"no":9,"cu_avcode":"ORANGE"}]});
        assert_eq!(alert_chip(&raw).as_deref(), Some("Aviation Orange"));
    }

    #[test]
    fn committed_snapshot_parses_to_volcano_events() {
        // The real committed PVMBG snapshot must parse and yield only elevated
        // Volcano events with valid geo (proves the shipped data file is well-formed).
        let ev = parse_magma_volcano(SNAPSHOT).unwrap();
        assert!(ev.len() >= 10, "snapshot should carry the elevated volcanoes, got {}", ev.len());
        assert!(ev.iter().all(|e| e.kind == EventKind::Volcano && e.geo.is_some()));
        assert!(ev.iter().all(|e| e.severity.value() >= 0.55));
        // Agung (Normal/1) is in the snapshot but must not plot.
        assert!(!ev.iter().any(|e| e.id == "magma-volcano-AGU"));
        // Merapi is at Siaga in the snapshot.
        assert!(ev.iter().any(|e| e.id == "magma-volcano-MER"));
    }
}
