//! USGS Volcano Hazards Program — HANS public API. U.S. volcano operational
//! alert state (Volcano Alert Level + Aviation Color Code). Free, no API key,
//! U.S. Government public domain.
//!
//! The HANS `getElevatedVolcanoes` product returns the volcanoes currently
//! **above background** — alert level ADVISORY/WATCH/WARNING and/or aviation
//! colour YELLOW/ORANGE/RED — each as `{ vnum, volcano_name, alert_level,
//! color_code, obs_abbr, sent_utc, notice_url }`. That notice record carries
//! the operational status but **no coordinates**, so this connector joins it by
//! `vnum` against the `getUSVolcanoes` catalogue (`{ vnum, volcano_name,
//! latitude, longitude }`) to place each elevated volcano. Emits one normalized
//! [`EventKind::Volcano`] [`Event`] per elevated U.S. volcano; volcanoes at the
//! all-clear state (NORMAL / GREEN / UNASSIGNED) are dropped, so an all-quiet
//! network yields zero events, not an error.
//!
//! USGS is the authoritative monitor for U.S. volcanoes — the Alaska Volcano
//! Observatory (most of the country's active volcanoes, e.g. Great Sitkin), the
//! Hawaiian Volcano Observatory (Kīlauea, Mauna Loa), the Cascades and
//! Yellowstone observatories. That standardized U.S. alert level + aviation
//! colour is the **operational** read GVP's weekly eruption catalogue and NASA
//! EONET (event-based) don't carry, over **U.S. / Alaska** geography that GeoNet
//! (NZ) doesn't cover.

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

/// USGS HANS volcano-alert source.
#[derive(Default)]
pub struct UsgsVolcano;

impl UsgsVolcano {
    pub fn elevated_url(&self) -> &'static str {
        "https://volcanoes.usgs.gov/hans-public/api/volcano/getElevatedVolcanoes"
    }
    pub fn catalog_url(&self) -> &'static str {
        "https://volcanoes.usgs.gov/hans-public/api/volcano/getUSVolcanoes"
    }
}

#[async_trait]
impl Source for UsgsVolcano {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "usgs_volcano",
            name: "USGS Volcano Alert Levels (HANS)",
            domain: EventKind::Volcano,
            cadence: Duration::from_secs(1800),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        // Two fetches: the elevated notices (status, no coords) + the US volcano
        // catalogue (coords by vnum). The catalogue is large but cacheable upstream.
        let elevated = crate::http::fetch_text(self.elevated_url()).await?;
        let catalog = crate::http::fetch_text(self.catalog_url()).await?;
        parse_usgs_volcano(&elevated, &catalog)
    }
}

/// Ground-hazard alert-level rank (0 = all-clear/unassigned). WARNING is the
/// hazardous-eruption state; ADVISORY is above background.
fn alert_rank(s: &str) -> f64 {
    match s.trim().to_ascii_uppercase().as_str() {
        "WARNING" => 1.0,
        "WATCH" => 0.8,
        "ADVISORY" => 0.55,
        _ => 0.0,
    }
}

/// Aviation colour-code rank (0 = green/unassigned). RED = significant ash
/// imminent/underway; ORANGE = eruption likely/minor ash; YELLOW = elevated unrest.
fn color_rank(s: &str) -> f64 {
    match s.trim().to_ascii_uppercase().as_str() {
        "RED" => 1.0,
        "ORANGE" => 0.8,
        "YELLOW" => 0.55,
        _ => 0.0,
    }
}

/// Title-case a HANS code ("WATCH" -> "Watch", "ORANGE" -> "Orange").
fn titlecase(s: &str) -> String {
    let s = s.trim();
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase(),
        None => String::new(),
    }
}

/// Operator chip: the ground alert level plus the aviation colour code, e.g.
/// "Alert Watch · Aviation Orange". `raw` is the elevated notice object. At least
/// one of the two carries signal (else the volcano was dropped as all-clear).
pub fn alert_chip(raw: &Value) -> Option<String> {
    let lvl = raw.get("alert_level").and_then(Value::as_str).unwrap_or("").trim();
    let col = raw.get("color_code").and_then(Value::as_str).unwrap_or("").trim();
    let meaningful = |s: &str, clear: &str| {
        !s.is_empty() && !s.eq_ignore_ascii_case(clear) && !s.eq_ignore_ascii_case("unassigned")
    };
    let head = meaningful(lvl, "normal").then(|| format!("Alert {}", titlecase(lvl)));
    let tail = meaningful(col, "green").then(|| format!("Aviation {}", titlecase(col)));
    match (head, tail) {
        (Some(h), Some(t)) => Some(format!("{h} · {t}")),
        (Some(h), None) => Some(h),
        (None, Some(t)) => Some(t),
        (None, None) => None,
    }
}

/// HANS numbers/strings: a JSON value that may be a number or a numeric string.
fn as_f64_loose(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
}

/// `vnum` may arrive as a JSON string or number; normalize to the string key.
fn vnum_of(v: &Value) -> String {
    match v.get("vnum") {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// HANS timestamps look like `"2026-04-29 19:44:16"` (UTC).
fn parse_hans_time(s: &str) -> Option<DateTime<Utc>> {
    chrono::NaiveDateTime::parse_from_str(s.trim(), "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|n| Utc.from_utc_datetime(&n))
}

/// Tolerate either a bare JSON array or `{items:[…]}` / `{data:[…]}` wrappers
/// (the HANS API shape has varied across versions).
fn rows(root: &Value) -> Option<&Vec<Value>> {
    root.as_array()
        .or_else(|| root.get("items").and_then(Value::as_array))
        .or_else(|| root.get("data").and_then(Value::as_array))
}

/// Build the `vnum -> (lat, lon)` lookup from the `getUSVolcanoes` catalogue.
fn parse_catalog(json: &str) -> anyhow::Result<HashMap<String, (f64, f64)>> {
    let root: Value = serde_json::from_str(json)?;
    let arr = rows(&root).ok_or_else(|| anyhow::anyhow!("usgs_volcano: catalogue is not an array"))?;
    let mut map = HashMap::with_capacity(arr.len());
    for v in arr {
        let vnum = vnum_of(v);
        if vnum.is_empty() {
            continue;
        }
        let lat = v.get("latitude").and_then(as_f64_loose);
        let lon = v.get("longitude").and_then(as_f64_loose);
        if let (Some(lat), Some(lon)) = (lat, lon) {
            map.insert(vnum, (lat, lon));
        }
    }
    Ok(map)
}

/// Pure parser: join the elevated-notices JSON to the catalogue JSON -> events.
/// Unit-tested offline. A malformed elevated/catalogue payload is an error;
/// volcanoes at the all-clear state (NORMAL/GREEN/UNASSIGNED) or with no
/// catalogue coordinates are filtered out, so an all-quiet network is Ok/empty.
pub fn parse_usgs_volcano(elevated_json: &str, catalog_json: &str) -> anyhow::Result<Vec<Event>> {
    let coords = parse_catalog(catalog_json)?;
    let root: Value = serde_json::from_str(elevated_json)?;
    let arr = rows(&root).ok_or_else(|| anyhow::anyhow!("usgs_volcano: elevated list is not an array"))?;

    let mut out = Vec::with_capacity(arr.len());
    for v in arr {
        let vnum = vnum_of(v);
        if vnum.is_empty() {
            continue;
        }
        let alert = v.get("alert_level").and_then(Value::as_str).unwrap_or("");
        let color = v.get("color_code").and_then(Value::as_str).unwrap_or("");

        // Drop the all-clear / unassigned state: a non-elevated dot carries no
        // risk signal, so the layer shows only volcanoes above background.
        let severity = alert_rank(alert).max(color_rank(color));
        if severity <= 0.0 {
            continue;
        }

        // Without a catalogue coordinate the notice can't be placed on the map.
        let Some(&(lat, lon)) = coords.get(&vnum) else { continue };
        let Some(geo) = Geo::new(lat, lon) else { continue };

        let name = v
            .get("volcano_name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("Volcano");
        let time = v
            .get("sent_utc")
            .and_then(Value::as_str)
            .and_then(parse_hans_time)
            .unwrap_or_else(Utc::now);
        let url = v
            .get("notice_url")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| s.starts_with("http"))
            .map(str::to_string)
            .unwrap_or_else(|| "https://volcanoes.usgs.gov/hans-public/".to_string());

        out.push(Event {
            id: format!("usgs-volcano-{vnum}"),
            source_id: "usgs_volcano".to_string(),
            kind: EventKind::Volcano,
            title: name.to_string(),
            time,
            geo: Some(geo),
            severity: Severity::new(severity),
            url: Some(url),
            raw: v.clone(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Built from the real HANS shape: getElevatedVolcanoes returns Great Sitkin
    // (Alaska, WATCH/ORANGE) and Kīlauea (Hawaii, ADVISORY/YELLOW); a NORMAL/GREEN
    // entry (Mount Hood) that must be dropped as all-clear; and an elevated entry
    // with no catalogue coordinate (must be dropped — can't be placed).
    const ELEVATED: &str = r#"[
      {"vnum":"311120","volcano_name":"Great Sitkin","obs_abbr":"avo",
       "alert_level":"WATCH","color_code":"ORANGE","sent_utc":"2026-04-29 19:44:16",
       "notice_url":"https://volcanoes.usgs.gov/vsc/notice/avo/311120"},
      {"vnum":"332010","volcano_name":"Kilauea","obs_abbr":"hvo",
       "alert_level":"ADVISORY","color_code":"YELLOW","sent_utc":"2026-04-26 18:47:22"},
      {"vnum":"322010","volcano_name":"Mount Hood","obs_abbr":"cvo",
       "alert_level":"NORMAL","color_code":"GREEN","sent_utc":"2026-04-20 00:00:00"},
      {"vnum":"999999","volcano_name":"Phantom","obs_abbr":"xxx",
       "alert_level":"WARNING","color_code":"RED","sent_utc":"2026-04-29 00:00:00"}
    ]"#;

    // getUSVolcanoes catalogue: real coordinates, keyed by vnum. Note Phantom
    // (999999) is absent, so the elevated Phantom entry can't be placed.
    const CATALOG: &str = r#"[
      {"vnum":"311120","volcano_name":"Great Sitkin","latitude":52.0764,"longitude":-176.1317},
      {"vnum":"332010","volcano_name":"Kilauea","latitude":"19.421","longitude":"-155.287"},
      {"vnum":"322010","volcano_name":"Mount Hood","latitude":45.374,"longitude":-121.695}
    ]"#;

    #[test]
    fn parses_fixture_joining_coords_and_dropping_all_clear() {
        let ev = parse_usgs_volcano(ELEVATED, CATALOG).unwrap();
        // Mount Hood (NORMAL/GREEN) dropped as all-clear; Phantom (no coords) dropped.
        assert_eq!(ev.len(), 2);

        assert_eq!(ev[0].id, "usgs-volcano-311120");
        assert_eq!(ev[0].kind, EventKind::Volcano);
        assert_eq!(ev[0].title, "Great Sitkin");
        // WATCH (0.8) vs ORANGE (0.8) -> 0.8.
        assert!((ev[0].severity.value() - 0.8).abs() < 1e-9);
        let g = ev[0].geo.unwrap();
        assert!((g.lat - 52.0764).abs() < 1e-6 && (g.lon + 176.1317).abs() < 1e-6);
        assert_eq!(alert_chip(&ev[0].raw).as_deref(), Some("Alert Watch · Aviation Orange"));
        // Timestamp parsed from sent_utc.
        assert_eq!(ev[0].time.format("%Y-%m-%d %H:%M").to_string(), "2026-04-29 19:44");

        // Kīlauea: coords arrived as numeric strings in the catalogue -> still placed.
        assert_eq!(ev[1].title, "Kilauea");
        let g = ev[1].geo.unwrap();
        assert!((g.lat - 19.421).abs() < 1e-6 && (g.lon + 155.287).abs() < 1e-6);
        // ADVISORY (0.55) vs YELLOW (0.55) -> 0.55.
        assert!((ev[1].severity.value() - 0.55).abs() < 1e-9);
        assert_eq!(alert_chip(&ev[1].raw).as_deref(), Some("Alert Advisory · Aviation Yellow"));
    }

    #[test]
    fn all_clear_network_is_ok_not_error() {
        // Every monitored volcano at NORMAL/GREEN -> zero plotted events, not a failure.
        let elevated = r#"[{"vnum":"322010","volcano_name":"Mount Hood",
          "alert_level":"NORMAL","color_code":"GREEN"}]"#;
        assert!(parse_usgs_volcano(elevated, CATALOG).unwrap().is_empty());
        // An empty elevated list is fine too.
        assert!(parse_usgs_volcano("[]", CATALOG).unwrap().is_empty());
    }

    #[test]
    fn errors_on_bad_input() {
        // Elevated payload not JSON (e.g. an HTML error page).
        assert!(parse_usgs_volcano("<html>403 Forbidden</html>", CATALOG).is_err());
        // Catalogue not an array.
        assert!(parse_usgs_volcano("[]", r#"{"oops":true}"#).is_err());
    }

    #[test]
    fn severity_ladders_and_chip_handles_colour_only() {
        // WARNING/RED is the apex; YELLOW alone (unassigned ground level) still plots.
        assert!((alert_rank("WARNING").max(color_rank("RED")) - 1.0).abs() < 1e-9);
        assert!((alert_rank("UNASSIGNED").max(color_rank("YELLOW")) - 0.55).abs() < 1e-9);
        // Colour-only chip when the ground alert level is unassigned.
        let raw = serde_json::json!({"alert_level":"UNASSIGNED","color_code":"ORANGE"});
        assert_eq!(alert_chip(&raw).as_deref(), Some("Aviation Orange"));
    }
}
