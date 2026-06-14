//! Fintraffic Digitraffic — live AIS vessel positions in the Baltic / Finnish waters,
//! from Fintraffic (the Finnish state transport operator). Free, no API key (a polite
//! `Digitraffic-User` header; responses are gzip — the workspace reqwest has gzip on).
//!
//! Fills the previously-empty [`EventKind::Vessel`] layer with real AIS. The Baltic is a
//! strategically active NATO/Russia maritime theatre (incl. shadow-fleet tankers), so this
//! is on-mission situational awareness, not generic traffic. Joins the `locations` feed
//! (position + navigational status + speed) with the `vessels` metadata feed (name + ship
//! type) by MMSI, plots vessels in an ABNORMAL navigational state (not-under-command /
//! restricted-manoeuvrability / aground) loudly and moving commercial traffic faintly, and
//! drops routine moored/anchored vessels as noise.

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use std::collections::HashMap;
use std::time::Duration;

/// Fintraffic Digitraffic AIS source.
#[derive(Default)]
pub struct DigitrafficAis;

impl DigitrafficAis {
    pub fn locations_url(&self) -> &'static str {
        "https://meri.digitraffic.fi/api/ais/v1/locations"
    }
    pub fn vessels_url(&self) -> &'static str {
        "https://meri.digitraffic.fi/api/ais/v1/vessels"
    }
}

#[async_trait]
impl Source for DigitrafficAis {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "digitraffic_ais",
            name: "Fintraffic AIS (Baltic)",
            domain: EventKind::Vessel,
            cadence: Duration::from_secs(300),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        let client = reqwest::Client::builder()
            .user_agent("engineering-effects/0.1 (+https://raithe.ca)")
            .build()?;
        let req = |u: &str| {
            client
                .get(u)
                .header("Digitraffic-User", "raithe/gcrm")
                .send()
        };
        let locations = req(self.locations_url()).await?.text().await?;
        let vessels = req(self.vessels_url()).await?.text().await?;
        parse_digitraffic_ais(&locations, &vessels)
    }
}

/// AIS ship-type code → short label (bucketed by tens per the AIS spec).
fn ship_type_label(t: i64) -> &'static str {
    match t / 10 {
        3 => "Fishing/Tug",
        4 => "High-speed craft",
        5 => "Special craft",
        6 => "Passenger",
        7 => "Cargo",
        8 => "Tanker",
        _ => "Vessel",
    }
}

/// AIS navigational-status code → label. `None` for codes we don't surface.
fn nav_status_label(s: i64) -> &'static str {
    match s {
        0 => "Under way",
        1 => "At anchor",
        2 => "Not under command",
        3 => "Restricted manoeuvr.",
        4 => "Constrained by draught",
        5 => "Moored",
        6 => "Aground",
        7 => "Fishing",
        8 => "Under sail",
        _ => "Under way",
    }
}

/// Whether a navigational status is operationally ABNORMAL (a real maritime signal).
fn is_anomalous(nav: i64) -> bool {
    matches!(nav, 2 | 3 | 6) // not-under-command / restricted-manoeuvr. / aground
}

/// Pure parser: AIS `locations` GeoJSON + `vessels` metadata JSON -> events. Offline-tested.
///
/// Emits vessels that are EITHER in an abnormal nav state (loud) OR moving commercial
/// traffic (cargo/tanker/passenger, faint); routine anchored/moored craft are dropped.
/// Results are sorted loudest-first so a downstream cap keeps the meaningful ones.
pub fn parse_digitraffic_ais(locations: &str, vessels: &str) -> anyhow::Result<Vec<Event>> {
    let loc_root: serde_json::Value = serde_json::from_str(locations)?;
    let features = loc_root
        .get("features")
        .and_then(|f| f.as_array())
        .ok_or_else(|| anyhow::anyhow!("digitraffic_ais: missing 'features' array"))?;

    // MMSI -> (name, ship_type) from the metadata feed (best-effort; absent is fine).
    let meta: HashMap<i64, (String, i64)> = serde_json::from_str::<serde_json::Value>(vessels)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|m| {
            let mmsi = m.get("mmsi").and_then(serde_json::Value::as_i64)?;
            let name = m.get("name").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
            let st = m.get("shipType").and_then(serde_json::Value::as_i64).unwrap_or(0);
            Some((mmsi, (name, st)))
        })
        .collect();

    let mut out = Vec::new();
    for f in features {
        let props = f.get("properties").cloned().unwrap_or(serde_json::Value::Null);
        let Some(mmsi) = props.get("mmsi").and_then(serde_json::Value::as_i64) else { continue };

        let coords = f.get("geometry").and_then(|g| g.get("coordinates")).and_then(|c| c.as_array());
        let (Some(lon), Some(lat)) = (
            coords.and_then(|c| c.first()).and_then(serde_json::Value::as_f64),
            coords.and_then(|c| c.get(1)).and_then(serde_json::Value::as_f64),
        ) else {
            continue;
        };
        let Some(geo) = Geo::new(lat, lon) else { continue };

        let nav = props.get("navStat").and_then(serde_json::Value::as_i64).unwrap_or(0);
        let sog = props.get("sog").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
        let (name, ship_type) = meta.get(&mmsi).cloned().unwrap_or_default();
        let st_bucket = ship_type / 10;
        let commercial = matches!(st_bucket, 6 | 7 | 8); // passenger / cargo / tanker

        // Keep only meaningful vessels: abnormal status, or moving commercial traffic.
        let anomalous = is_anomalous(nav);
        if !anomalous && !(commercial && sog >= 0.5) {
            continue;
        }

        let severity = match nav {
            6 => 0.85, // aground
            2 => 0.80, // not under command
            3 => 0.60, // restricted manoeuvrability
            _ => 0.25, // routine moving commercial vessel
        };

        let label = ship_type_label(ship_type);
        let title = if name.is_empty() {
            format!("MMSI {mmsi} ({label})")
        } else {
            format!("{name} ({label})")
        };

        // timestampExternal is epoch milliseconds.
        let time = props
            .get("timestampExternal")
            .and_then(serde_json::Value::as_i64)
            .and_then(|ms| Utc.timestamp_millis_opt(ms).single())
            .unwrap_or_else(Utc::now);

        out.push(Event {
            id: format!("ais-{mmsi}"),
            source_id: "digitraffic_ais".to_string(),
            kind: EventKind::Vessel,
            title,
            time,
            geo: Some(geo),
            severity: Severity::new(severity),
            url: Some("https://www.digitraffic.fi/en/marine-traffic/".to_string()),
            raw: serde_json::json!({ "sog": sog, "status": nav_status_label(nav), "ship_type": label }),
        });
    }

    // Loudest first, so a per-feed cap keeps abnormal vessels over routine traffic.
    out.sort_by(|a, b| b.severity.value().partial_cmp(&a.severity.value()).unwrap_or(std::cmp::Ordering::Equal));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCATIONS: &str = r#"{
      "type":"FeatureCollection",
      "features":[
        {"type":"Feature","geometry":{"type":"Point","coordinates":[24.9,59.4]},
         "properties":{"mmsi":1,"sog":0.0,"navStat":6,"heading":90,"timestampExternal":1759212938646}},
        {"type":"Feature","geometry":{"type":"Point","coordinates":[20.8,55.7]},
         "properties":{"mmsi":2,"sog":12.4,"navStat":0,"heading":300,"timestampExternal":1759212938646}},
        {"type":"Feature","geometry":{"type":"Point","coordinates":[22.0,60.0]},
         "properties":{"mmsi":3,"sog":0.0,"navStat":5,"heading":0,"timestampExternal":1759212938646}},
        {"type":"Feature","geometry":{"type":"Point","coordinates":[999,0]},
         "properties":{"mmsi":4,"sog":3.0,"navStat":0}}
      ]
    }"#;
    const VESSELS: &str = r#"[
      {"mmsi":1,"name":"AGROUND STAR","shipType":80},
      {"mmsi":2,"name":"NORD SUPERIOR","shipType":70},
      {"mmsi":3,"name":"MOORED ONE","shipType":80}
    ]"#;

    #[test]
    fn parses_and_filters() {
        let ev = parse_digitraffic_ais(LOCATIONS, VESSELS).unwrap();
        // mmsi 3 (moored, not anomalous) dropped; mmsi 4 (bad coords) dropped.
        // mmsi 1 (aground) + mmsi 2 (moving cargo) kept.
        assert_eq!(ev.len(), 2);
        // Loudest first -> aground tanker leads.
        assert_eq!(ev[0].id, "ais-1");
        assert_eq!(ev[0].kind, EventKind::Vessel);
        assert_eq!(ev[0].title, "AGROUND STAR (Tanker)");
        assert!((ev[0].severity.value() - 0.85).abs() < 1e-9);
        assert_eq!(ev[0].raw.get("status").unwrap().as_str().unwrap(), "Aground");
        // Moving cargo vessel kept, faint.
        assert_eq!(ev[1].title, "NORD SUPERIOR (Cargo)");
        assert!((ev[1].severity.value() - 0.25).abs() < 1e-9);
        let g = ev[0].geo.unwrap();
        assert!((g.lat - 59.4).abs() < 1e-6 && (g.lon - 24.9).abs() < 1e-6);
    }

    #[test]
    fn errors_on_missing_features() {
        assert!(parse_digitraffic_ais(r#"{"x":1}"#, "[]").is_err());
        // A broken vessels-metadata body is tolerated: anomalous vessels still plot (no
        // ship type needed); moving vessels of now-unknown type are dropped.
        let ev = parse_digitraffic_ais(LOCATIONS, "not json").unwrap();
        assert_eq!(ev.iter().find(|e| e.id == "ais-1").unwrap().title, "MMSI 1 (Vessel)");
        assert!(!ev.iter().any(|e| e.id == "ais-2"));
    }
}
