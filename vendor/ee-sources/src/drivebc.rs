//! DriveBC — live road events (closures, collisions, construction, weather) on British
//! Columbia's provincial highway network, from the BC Ministry of Transportation and
//! Infrastructure. Free, no API key (a polite `User-Agent` is enough).
//!
//! Reads the DriveBC Open511 `events` API into normalized [`EventKind::Transport`]
//! [`Event`]s — a province-specific BC signal the national feeds don't carry, and the
//! western complement to the Ontario-only [`crate::ontario511`] feed (zero geographic
//! overlap, distinct jurisdiction, the real Open511 standard vs Ontario's 511on.ca v2).

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use std::time::Duration;

/// DriveBC Open511 road-event source.
#[derive(Default)]
pub struct DriveBc;

impl DriveBc {
    pub fn url(&self) -> &'static str {
        // `limit` is generous: the active set is low-hundreds and Open511 paginates at
        // 50/page by default, so an explicit cap pulls every active BC event in one shot.
        "https://api.open511.gov.bc.ca/events?format=json&limit=500"
    }
}

#[async_trait]
impl Source for DriveBc {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "drivebc",
            name: "DriveBC Road Events",
            domain: EventKind::Transport,
            cadence: Duration::from_secs(300),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        let body = crate::http::fetch_text(self.url()).await?;
        parse_drivebc(&body)
    }
}

/// Friendly label for an Open511 `event_type`.
fn type_label(t: &str) -> &str {
    match t {
        "INCIDENT" => "Incident",
        "CONSTRUCTION" => "Construction",
        "SPECIAL_EVENT" => "Event",
        "WEATHER_CONDITION" => "Weather",
        "ROAD_CONDITION" => "Road condition",
        _ => "Road event",
    }
}

/// First plottable coordinate for an Open511 geometry: a `Point`'s own coordinate, or
/// the first vertex of a `LineString`. GeoJSON order is `[lon, lat]`.
fn first_coord(geometry: &serde_json::Value) -> Option<(f64, f64)> {
    let coords = geometry.get("coordinates")?;
    match geometry.get("type").and_then(|t| t.as_str()) {
        Some("Point") => {
            let lon = coords.get(0)?.as_f64()?;
            let lat = coords.get(1)?.as_f64()?;
            Some((lon, lat))
        }
        Some("LineString") => {
            let first = coords.get(0)?;
            let lon = first.get(0)?.as_f64()?;
            let lat = first.get(1)?.as_f64()?;
            Some((lon, lat))
        }
        _ => None,
    }
}

/// Pure parser: DriveBC Open511 `events` JSON -> events. Unit-tested offline.
///
/// Open511 `severity` is a usable MINOR/MAJOR enum (no Ontario-style "Unknown" noise):
/// MAJOR is loud; otherwise severity is derived from `event_type` (incidents/weather
/// above routine construction), mirroring [`crate::ontario511`]'s loud/quiet logic.
pub fn parse_drivebc(json: &str) -> anyhow::Result<Vec<Event>> {
    let root: serde_json::Value = serde_json::from_str(json)?;
    let events = root
        .get("events")
        .and_then(|e| e.as_array())
        .ok_or_else(|| anyhow::anyhow!("drivebc: missing 'events' array"))?;

    let mut out = Vec::with_capacity(events.len());
    for e in events {
        let Some(geom) = e.get("geography") else { continue };
        let Some((lon, lat)) = first_coord(geom) else { continue };
        let Some(geo) = Geo::new(lat, lon) else { continue };

        let Some(id) = e.get("id").and_then(|v| v.as_str()) else { continue };
        // id looks like "drivebc.ca/DBC-90617"; keep the stable DBC suffix.
        let short = id.rsplit('/').next().unwrap_or(id);

        let etype = e.get("event_type").and_then(|v| v.as_str()).unwrap_or("");
        let severity_enum = e.get("severity").and_then(|v| v.as_str()).unwrap_or("");

        // Road name + direction from the first road; fall back to a clipped description.
        let road = e
            .get("roads")
            .and_then(|r| r.as_array())
            .and_then(|r| r.first());
        let road_name = road.and_then(|r| r.get("name")).and_then(|v| v.as_str()).unwrap_or("").trim();
        let dir = road
            .and_then(|r| r.get("direction"))
            .and_then(|v| v.as_str())
            .filter(|d| !d.is_empty() && *d != "NONE" && *d != "BOTH")
            .unwrap_or("");
        let where_ = if !road_name.is_empty() {
            if dir.is_empty() { road_name.to_string() } else { format!("{road_name} {dir}") }
        } else {
            e.get("description").and_then(|v| v.as_str()).unwrap_or("BC").chars().take(60).collect()
        };
        let title = format!("{} — {where_}", type_label(etype));

        let severity = if severity_enum == "MAJOR" {
            0.85
        } else {
            match etype {
                "INCIDENT" => 0.6,
                "WEATHER_CONDITION" => 0.55,
                "ROAD_CONDITION" => 0.5,
                "SPECIAL_EVENT" => 0.4,
                "CONSTRUCTION" => 0.35,
                _ => 0.4,
            }
        };

        let time = e
            .get("updated")
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        out.push(Event {
            id: format!("drivebc-{short}"),
            source_id: "drivebc".to_string(),
            kind: EventKind::Transport,
            title,
            time,
            geo: Some(geo),
            severity: Severity::new(severity),
            url: e.get("url").and_then(|v| v.as_str()).map(String::from),
            raw: serde_json::json!({
                "event_type": etype, "severity": severity_enum,
                "description": e.get("description").cloned().unwrap_or(serde_json::Value::Null),
            }),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
      "events": [
        {"id":"drivebc.ca/DBC-1","url":"https://api.open511.gov.bc.ca/events/drivebc.ca/DBC-1",
         "event_type":"CONSTRUCTION","severity":"MINOR","updated":"2026-06-14T11:06:01-07:00",
         "description":"Utility work.","roads":[{"name":"Highway 99","direction":"SOUTHBOUND"}],
         "geography":{"type":"Point","coordinates":[-120.779441,49.664635]}},
        {"id":"drivebc.ca/DBC-2","url":"https://api.open511.gov.bc.ca/events/drivebc.ca/DBC-2",
         "event_type":"INCIDENT","severity":"MAJOR","updated":"2026-06-14T08:00:00-07:00",
         "description":"Collision.","roads":[{"name":"Blackburn Road","direction":"NONE"}],
         "geography":{"type":"LineString","coordinates":[[-123.1,49.2],[-123.2,49.3]]}},
        {"id":"drivebc.ca/DBC-3","event_type":"CONSTRUCTION","severity":"MINOR",
         "geography":{"type":"Point","coordinates":[999,0]}}
      ]
    }"#;

    #[test]
    fn parses_fixture() {
        let ev = parse_drivebc(FIXTURE).unwrap();
        // Third event has out-of-range lon -> dropped.
        assert_eq!(ev.len(), 2);

        assert_eq!(ev[0].id, "drivebc-DBC-1");
        assert_eq!(ev[0].kind, EventKind::Transport);
        assert_eq!(ev[0].title, "Construction — Highway 99 SOUTHBOUND");
        assert!((ev[0].severity.value() - 0.35).abs() < 1e-9);
        let g = ev[0].geo.unwrap();
        assert!((g.lat - 49.664635).abs() < 1e-6 && (g.lon + 120.779441).abs() < 1e-6);

        // LineString -> first vertex; MAJOR severity is loud; direction NONE is dropped.
        assert_eq!(ev[1].title, "Incident — Blackburn Road");
        assert!((ev[1].severity.value() - 0.85).abs() < 1e-9);
        let g2 = ev[1].geo.unwrap();
        assert!((g2.lat - 49.2).abs() < 1e-6 && (g2.lon + 123.1).abs() < 1e-6);
    }

    #[test]
    fn errors_on_missing_array() {
        assert!(parse_drivebc(r#"{"x":1}"#).is_err());
    }
}
