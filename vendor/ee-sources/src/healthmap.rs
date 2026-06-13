//! HealthMap — global disease-outbreak surveillance. Free, no API key.
//!
//! Reads the public `getAlerts.php` marker feed (the same one the HealthMap web UI
//! calls) and normalizes each geocoded outbreak cluster to an [`EventKind::Health`]
//! [`Event`]. Fills the Africa/Asia/South-America signal gap the other feeds miss.
//!
//! NOTE: this is an undocumented public endpoint (no published SLA); it is paired with
//! the map's per-feed last-good resilience so a hiccup degrades gracefully.

use async_trait::async_trait;
use chrono::Utc;
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use std::time::Duration;

/// HealthMap outbreak source. `days` bounds the look-back window (kept tight — the
/// full-window response is several MB).
pub struct HealthMap {
    pub days: i64,
}

impl Default for HealthMap {
    fn default() -> Self {
        Self { days: 3 }
    }
}

impl HealthMap {
    pub fn url(&self) -> String {
        let edate = Utc::now().format("%Y-%m-%d");
        let sdate = (Utc::now() - chrono::Duration::days(self.days)).format("%Y-%m-%d");
        format!("https://www.healthmap.org/getAlerts.php?diseases=&sdate={sdate}&edate={edate}")
    }
}

#[async_trait]
impl Source for HealthMap {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "healthmap",
            name: "HealthMap Outbreaks (global)",
            domain: EventKind::Health,
            cadence: Duration::from_secs(1800),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        let client = reqwest::Client::builder()
            .user_agent("engineering-effects/0.1 (+https://raithe.ca)")
            .build()?;
        let body = client.get(self.url()).send().await?.text().await?;
        parse_healthmap(&body)
    }
}

/// Pure parser: HealthMap `getAlerts.php` JSON -> events. Unit-tested offline.
///
/// Each `.markers[]` entry is one geocoded cluster (`lat`/`lon`, `label` = disease
/// list, `alertids` = constituent alerts). Severity scales with the alert count so a
/// busy cluster reads louder. A non-`markers` body is treated as "no outbreaks".
pub fn parse_healthmap(json: &str) -> anyhow::Result<Vec<Event>> {
    let root: serde_json::Value = serde_json::from_str(json)?;
    let Some(markers) = root.get("markers").and_then(|m| m.as_array()) else {
        return Ok(Vec::new());
    };

    let mut out = Vec::with_capacity(markers.len());
    for m in markers {
        // lat/lon arrive as numbers or numeric strings depending on the record.
        let num = |k: &str| {
            m.get(k).and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
        };
        let (Some(lat), Some(lon)) = (num("lat"), num("lon")) else { continue };
        let Some(geo) = Geo::new(lat, lon) else { continue };

        let place = m.get("place_name").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
        let disease = m.get("label").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
        let title = place.or(disease).unwrap_or("Disease outbreak").to_string();

        let n_alerts = m.get("alertids").and_then(|a| a.as_array()).map(|a| a.len()).unwrap_or(1);
        let place_id = m
            .get("place_id")
            .and_then(|v| v.as_str().map(String::from).or_else(|| v.as_i64().map(|i| i.to_string())))
            .unwrap_or_else(|| format!("{lat:.3},{lon:.3}"));

        out.push(Event {
            id: format!("healthmap-{place_id}"),
            source_id: "healthmap".to_string(),
            kind: EventKind::Health,
            title,
            // No reliable per-marker timestamp; the query window bounds it to "recent".
            time: Utc::now(),
            geo: Some(geo),
            severity: Severity::new((n_alerts as f64 / 8.0).clamp(0.3, 0.95)),
            url: Some("https://www.healthmap.org/".to_string()),
            raw: m.clone(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
      "markers":[
        {"lat":12.8333,"lon":108.1667,"place_name":"Dak Lak Province, Vietnam",
         "label":"Dengue","alertids":["1","2","3"],"place_id":"7788"},
        {"lat":"-12.2953","lon":"17.5447","place_name":"Angola","label":"Cholera","alertids":["9"],"place_id":4421},
        {"lat":999,"lon":0,"place_name":"bad","label":"x","place_id":"z"}
      ]
    }"#;

    #[test]
    fn parses_fixture() {
        let ev = parse_healthmap(FIXTURE).unwrap();
        // The out-of-range third marker is dropped.
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0].kind, EventKind::Health);
        assert_eq!(ev[0].id, "healthmap-7788");
        assert_eq!(ev[0].title, "Dak Lak Province, Vietnam");
        let g = ev[0].geo.unwrap();
        assert!((g.lat - 12.8333).abs() < 1e-9 && (g.lon - 108.1667).abs() < 1e-9);
        // 3 alerts -> 3/8 = 0.375.
        assert!((ev[0].severity.value() - 0.375).abs() < 1e-9);
        // String lat/lon + numeric place_id handled.
        assert_eq!(ev[1].id, "healthmap-4421");
        assert!((ev[1].geo.unwrap().lat + 12.2953).abs() < 1e-9);
    }

    #[test]
    fn tolerates_no_markers() {
        assert_eq!(parse_healthmap(r#"{"foo":1}"#).unwrap().len(), 0);
    }
}
