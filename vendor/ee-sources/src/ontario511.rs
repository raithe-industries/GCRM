//! Ontario 511 — live road events (closures, collisions, construction) on Ontario's
//! provincial highway network, from the Ministry of Transportation. Free, no API key.
//!
//! Reads the 511on.ca events API into normalized [`EventKind::Transport`] [`Event`]s —
//! a province-specific Ontario signal the national feeds don't carry.

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use std::time::Duration;

/// Ontario 511 road-event source.
#[derive(Default)]
pub struct Ontario511;

impl Ontario511 {
    pub fn url(&self) -> &'static str {
        // NOTE: singular `/event` — the plural `/events` 404s.
        "https://511on.ca/api/v2/get/event?format=json&lang=en"
    }
}

#[async_trait]
impl Source for Ontario511 {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "ontario511",
            name: "Ontario 511 Road Events",
            domain: EventKind::Transport,
            cadence: Duration::from_secs(300),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        let body = crate::http::fetch_text(self.url()).await?;
        parse_ontario511(&body)
    }
}

/// Friendly label for a 511 `EventType`.
fn type_label(t: &str) -> &str {
    match t {
        "accidentsAndIncidents" => "Incident",
        "roadwork" => "Roadwork",
        "closures" => "Closure",
        "specialEvents" => "Event",
        "weatherConditions" => "Weather",
        _ => "Road event",
    }
}

/// Pure parser: 511 events JSON array -> events. Unit-tested offline.
///
/// 511's `Severity` is uniformly "Unknown" (and only sparsely populated on the Alberta
/// twin), so severity is derived from the full-closure flag + event type instead
/// (closures/collisions loud, routine roadwork quiet).
pub fn parse_ontario511(json: &str) -> anyhow::Result<Vec<Event>> {
    parse_511(json, "ontario511", "on511", "https://511on.ca/")
}

/// Shared parser for the Castle Rock / OneNetwork "511" `get/event` JSON array, whose
/// field schema (`ID`/`EventType`/`IsFullClosure`/`RoadwayName`/`DirectionOfTravel`/
/// `LastUpdated`/`Latitude`/`Longitude`) is byte-identical across the provincial
/// services that run on it (Ontario 511, Alberta 511). `source_id`/`id_prefix`/`url`
/// keep each province's events separable on the map.
pub fn parse_511(
    json: &str,
    source_id: &str,
    id_prefix: &str,
    url: &str,
) -> anyhow::Result<Vec<Event>> {
    let root: serde_json::Value = serde_json::from_str(json)?;
    let arr = root
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("{source_id}: expected a top-level JSON array"))?;

    let mut out = Vec::with_capacity(arr.len());
    for e in arr {
        let (Some(lat), Some(lon)) = (
            e.get("Latitude").and_then(serde_json::Value::as_f64),
            e.get("Longitude").and_then(serde_json::Value::as_f64),
        ) else {
            continue;
        };
        let Some(geo) = Geo::new(lat, lon) else { continue };

        let id = e.get("ID").and_then(|v| v.as_i64().map(|i| i.to_string()).or_else(|| v.as_str().map(String::from)));
        let Some(id) = id else { continue };

        let etype = e.get("EventType").and_then(|v| v.as_str()).unwrap_or("");
        let full_closure = e.get("IsFullClosure").and_then(|v| v.as_bool()).unwrap_or(false);
        let road = e.get("RoadwayName").and_then(|v| v.as_str()).unwrap_or("").trim();
        let dir = e.get("DirectionOfTravel").and_then(|v| v.as_str()).unwrap_or("").trim();

        let where_ = match (road.is_empty(), dir.is_empty()) {
            (false, false) => format!("{road} {dir}"),
            (false, true) => road.to_string(),
            _ => e.get("Description").and_then(|v| v.as_str()).unwrap_or("Ontario").chars().take(60).collect(),
        };
        let title = format!("{} — {where_}", type_label(etype));

        let severity = if full_closure {
            0.85
        } else {
            match etype {
                "accidentsAndIncidents" => 0.6,
                "closures" => 0.7,
                "roadwork" => 0.35,
                _ => 0.4,
            }
        };

        let time = e
            .get("LastUpdated")
            .and_then(serde_json::Value::as_i64)
            .and_then(|s| Utc.timestamp_opt(s, 0).single())
            .unwrap_or_else(Utc::now);

        out.push(Event {
            id: format!("{id_prefix}-{id}"),
            source_id: source_id.to_string(),
            kind: EventKind::Transport,
            title,
            time,
            geo: Some(geo),
            severity: Severity::new(severity),
            url: Some(url.to_string()),
            raw: serde_json::json!({
                "EventType": etype, "IsFullClosure": full_closure,
                "Description": e.get("Description").cloned().unwrap_or(serde_json::Value::Null),
                "LanesAffected": e.get("LanesAffected").cloned().unwrap_or(serde_json::Value::Null),
            }),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"[
      {"ID":86216,"EventType":"roadwork","RoadwayName":"HWY 17","DirectionOfTravel":"Eastbound",
       "Description":"Daily construction.","IsFullClosure":false,"Severity":"Unknown","LastUpdated":1757532060,
       "Latitude":48.79229,"Longitude":-87.20525},
      {"ID":86220,"EventType":"accidentsAndIncidents","RoadwayName":"HWY 401","DirectionOfTravel":"Eastbound",
       "Description":"Collision, all lanes closed.","IsFullClosure":true,"LastUpdated":1757532000,
       "Latitude":42.85355,"Longitude":-81.27517},
      {"ID":1,"EventType":"roadwork","Latitude":999,"Longitude":0}
    ]"#;

    #[test]
    fn parses_fixture() {
        let ev = parse_ontario511(FIXTURE).unwrap();
        // Third record has out-of-range lat -> dropped.
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0].kind, EventKind::Transport);
        assert_eq!(ev[0].id, "on511-86216");
        assert_eq!(ev[0].title, "Roadwork — HWY 17 Eastbound");
        assert!((ev[0].severity.value() - 0.35).abs() < 1e-9);
        // Full-closure collision -> loud + clear title.
        assert_eq!(ev[1].title, "Incident — HWY 401 Eastbound");
        assert!((ev[1].severity.value() - 0.85).abs() < 1e-9);
        assert!(ev[1].raw.get("IsFullClosure").unwrap().as_bool().unwrap());
    }

    #[test]
    fn errors_on_non_array() {
        assert!(parse_ontario511(r#"{"x":1}"#).is_err());
    }
}
