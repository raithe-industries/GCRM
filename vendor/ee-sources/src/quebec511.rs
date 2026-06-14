//! Québec 511 — live road events (closures, collisions, road/structure damage, work)
//! on Québec's provincial network, from the Ministère des Transports et de la Mobilité
//! durable du Québec (MTMD). Free, no API key.
//!
//! Reads the MTMD MapServer WFS `ms:evenements` collection as GeoJSON into normalized
//! [`EventKind::Transport`] [`Event`]s — Québec road coverage the national feeds and
//! the Ontario/BC/Alberta 511 feeds don't carry. Fields are French (`cause`, `entrave`,
//! `localisation`, …); geometry is `LineString`, so the first vertex is the map dot.
//! Source: Transports Québec / Données Québec (CC-BY 4.0).

use async_trait::async_trait;
use chrono::{NaiveDateTime, TimeZone, Utc};
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use std::time::Duration;

/// Québec 511 (MTMD WFS) road-event source.
#[derive(Default)]
pub struct Quebec511;

impl Quebec511 {
    pub fn url(&self) -> &'static str {
        "https://ws.mapserver.transports.gouv.qc.ca/swtq?service=wfs&version=2.0.0\
         &request=getfeature&typename=ms:evenements&srsname=EPSG:4326&outputformat=geojson"
    }
}

#[async_trait]
impl Source for Quebec511 {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "quebec511",
            name: "Québec 511 Road Events",
            domain: EventKind::Transport,
            cadence: Duration::from_secs(300),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        let client = reqwest::Client::builder()
            .user_agent("engineering-effects/0.1 (+https://raithe.ca)")
            .build()?;
        let body = client.get(self.url()).send().await?.text().await?;
        parse_quebec511(&body)
    }
}

/// English label for an MTMD `cause` (French source field), for UI parity with the
/// other 511 feeds. Empty/unknown causes fall back to a generic "Road event".
fn cause_label(cause: &str) -> &str {
    match cause {
        "Accident" => "Accident",
        "Bris de la route" => "Road damage",
        "Bris de la structure" => "Structure damage",
        "Érosion" => "Erosion",
        "Travaux" => "Roadwork",
        "Mesures préventives" => "Preventive measures",
        "Inspection" => "Inspection",
        "" => "Road event",
        other => other,
    }
}

/// Severity from `cause` (what happened) lifted by `entrave` (how much road is taken).
/// Collisions and structural failures are loud; routine work and inspections quiet; a
/// full closure (`Fermeture` with no lane qualifier) is loudest.
fn severity_for(cause: &str, entrave: &str) -> f64 {
    let mut s: f64 = match cause {
        "Accident" | "Bris de la route" | "Bris de la structure" | "Érosion" => 0.8,
        "Travaux" => 0.4,
        "Mesures préventives" | "Inspection" => 0.45,
        _ => 0.5,
    };
    if entrave.contains("Fermeture") {
        // "Fermeture de N voie(s)" = partial; bare "Fermeture" = full closure.
        s = s.max(if entrave.contains("voie") { 0.7 } else { 0.9 });
    }
    s
}

/// Pure parser: MTMD `ms:evenements` GeoJSON -> events. Unit-tested offline.
pub fn parse_quebec511(json: &str) -> anyhow::Result<Vec<Event>> {
    let root: serde_json::Value = serde_json::from_str(json)?;
    let features = root
        .get("features")
        .and_then(|f| f.as_array())
        .ok_or_else(|| anyhow::anyhow!("quebec511: missing 'features' array"))?;

    let mut out = Vec::with_capacity(features.len());
    for f in features {
        // Geometry is LineString; plot the first vertex. GeoJSON order is [lon, lat].
        let first = f
            .get("geometry")
            .filter(|g| !g.is_null())
            .and_then(|g| g.get("coordinates"))
            .and_then(|c| c.as_array())
            .and_then(|c| c.first());
        let (Some(lon), Some(lat)) = (
            first.and_then(|p| p.get(0)).and_then(serde_json::Value::as_f64),
            first.and_then(|p| p.get(1)).and_then(serde_json::Value::as_f64),
        ) else {
            continue;
        };
        let Some(geo) = Geo::new(lat, lon) else { continue };

        let props = f.get("properties").cloned().unwrap_or(serde_json::Value::Null);
        // `identifiant` is a JSON number; require it for a stable id.
        let Some(ident) = props.get("identifiant").and_then(|v| v.as_i64()) else { continue };

        let cause = props.get("cause").and_then(|v| v.as_str()).unwrap_or("");
        let entrave = props.get("entrave").and_then(|v| v.as_str()).unwrap_or("");
        let route = props.get("numeroRoute").and_then(|v| v.as_str()).unwrap_or("").trim();
        let dir = props.get("direction").and_then(|v| v.as_str()).unwrap_or("").trim();

        let mut where_ = if !route.is_empty() {
            route.to_string()
        } else {
            props.get("localisation").and_then(|v| v.as_str()).unwrap_or("Québec").chars().take(60).collect()
        };
        if !dir.is_empty() {
            where_.push_str(&format!(" {dir}"));
        }
        let title = format!("{} — {where_}", cause_label(cause));

        // `enVigueurDepuis` is ISO-8601 with no timezone; read it as UTC.
        let time = props
            .get("enVigueurDepuis")
            .and_then(|v| v.as_str())
            .and_then(|s| NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").ok())
            .map(|ndt| Utc.from_utc_datetime(&ndt))
            .unwrap_or_else(Utc::now);

        out.push(Event {
            id: format!("qc511-{ident}"),
            source_id: "quebec511".to_string(),
            kind: EventKind::Transport,
            title,
            time,
            geo: Some(geo),
            severity: Severity::new(severity_for(cause, entrave)),
            url: Some("https://www.quebec511.info/".to_string()),
            raw: serde_json::json!({
                "cause": cause, "entrave": entrave,
                "localisation": props.get("localisation").cloned().unwrap_or(serde_json::Value::Null),
                "municipalite": props.get("municipalite").cloned().unwrap_or(serde_json::Value::Null),
            }),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
      "type":"FeatureCollection",
      "features":[
        {"type":"Feature","geometry":{"type":"LineString","coordinates":[[-70.881684,48.336338],[-70.88,48.34]]},
         "properties":{"identifiant":123676,"cause":"Travaux","entrave":"Fermeture de 1 voie sur 2",
           "localisation":"R-170 à la hauteur rivière à Mars","numeroRoute":"170","direction":"OUEST",
           "municipalite":"Saguenay","enVigueurDepuis":"2026-05-22T18:50:00"}},
        {"type":"Feature","geometry":{"type":"LineString","coordinates":[[-73.5,45.5],[-73.6,45.6]]},
         "properties":{"identifiant":81387,"cause":"Accident","entrave":"Fermeture",
           "localisation":"chemin du Barrage","numeroRoute":"","direction":"","municipalite":"Val-des-Monts",
           "enVigueurDepuis":"2026-06-14T15:04:00"}},
        {"type":"Feature","geometry":null,"properties":{"identifiant":9,"cause":"Inspection"}}
      ]
    }"#;

    #[test]
    fn parses_fixture() {
        let ev = parse_quebec511(FIXTURE).unwrap();
        // The null-geometry third feature is dropped.
        assert_eq!(ev.len(), 2);

        assert_eq!(ev[0].id, "qc511-123676");
        assert_eq!(ev[0].kind, EventKind::Transport);
        assert_eq!(ev[0].title, "Roadwork — 170 OUEST");
        // Travaux base 0.4 lifted to 0.7 by a partial "Fermeture de 1 voie".
        assert!((ev[0].severity.value() - 0.7).abs() < 1e-9);
        let g = ev[0].geo.unwrap();
        assert!((g.lat - 48.336338).abs() < 1e-6 && (g.lon + 70.881684).abs() < 1e-6);

        // No route -> falls back to localisation; bare "Fermeture" = full closure (0.9).
        assert_eq!(ev[1].title, "Accident — chemin du Barrage");
        assert!((ev[1].severity.value() - 0.9).abs() < 1e-9);
    }

    #[test]
    fn errors_on_missing_array() {
        assert!(parse_quebec511(r#"{"x":1}"#).is_err());
    }
}
