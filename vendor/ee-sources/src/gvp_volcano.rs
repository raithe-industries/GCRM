//! Smithsonian Global Volcanism Program (GVP) — recent/ongoing volcanic eruptions
//! via the GVP GeoServer WFS. Free, no API key.
//!
//! Pulls the most recently-started eruptions from the `E3WebApp_Eruptions1960` layer
//! as GeoJSON and normalizes them to [`EventKind::Volcano`] [`Event`]s — a global
//! hazard layer concentrated on the Pacific Ring of Fire.

use async_trait::async_trait;
use chrono::{NaiveDate, TimeZone, Utc};
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use std::time::Duration;

/// GVP eruption source. `count` bounds how many of the most-recent eruptions to pull.
pub struct GvpVolcano {
    pub count: u32,
}

impl Default for GvpVolcano {
    fn default() -> Self {
        Self { count: 150 }
    }
}

impl GvpVolcano {
    pub fn url(&self) -> String {
        format!(
            "https://webservices.volcano.si.edu/geoserver/GVP-VOTW/ows?service=WFS&version=2.0.0\
             &request=GetFeature&typeNames=GVP-VOTW:E3WebApp_Eruptions1960\
             &outputFormat=application/json&sortBy=StartDate+D&count={}",
            self.count
        )
    }
}

#[async_trait]
impl Source for GvpVolcano {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "gvp_volcano",
            name: "Smithsonian Volcanoes (global)",
            domain: EventKind::Volcano,
            cadence: Duration::from_secs(3600),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        let client = reqwest::Client::builder()
            .user_agent("engineering-effects/0.1 (+https://raithe.ca)")
            .build()?;
        let body = client.get(self.url()).send().await?.text().await?;
        parse_gvp(&body)
    }
}

/// Parse a GVP `YYYYMMDD` fixed-width date string into a UTC datetime (defaults a
/// missing/zero month or day to 01; falls back to `now` on garbage).
fn parse_gvp_date(s: &str) -> chrono::DateTime<Utc> {
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() >= 4 {
        let y: i32 = digits[0..4].parse().unwrap_or(0);
        let m: u32 = digits.get(4..6).and_then(|x| x.parse().ok()).filter(|&m| (1..=12).contains(&m)).unwrap_or(1);
        let d: u32 = digits.get(6..8).and_then(|x| x.parse().ok()).filter(|&d| (1..=31).contains(&d)).unwrap_or(1);
        if let Some(date) = NaiveDate::from_ymd_opt(y, m, d) {
            if let Some(dt) = date.and_hms_opt(0, 0, 0) {
                return Utc.from_utc_datetime(&dt);
            }
        }
    }
    Utc::now()
}

/// Pure parser: GVP WFS GeoJSON -> events. Unit-tested offline.
pub fn parse_gvp(json: &str) -> anyhow::Result<Vec<Event>> {
    let root: serde_json::Value = serde_json::from_str(json)?;
    // WFS without `outputFormat=application/json` returns XML; tolerate a non-JSON or
    // featureless response as "no eruptions" rather than a hard error.
    let Some(features) = root.get("features").and_then(|f| f.as_array()) else {
        return Ok(Vec::new());
    };

    let mut out = Vec::with_capacity(features.len());
    for f in features {
        let props = f.get("properties").cloned().unwrap_or(serde_json::Value::Null);

        let geo = f
            .get("geometry")
            .filter(|g| g.get("type").and_then(|t| t.as_str()) == Some("Point"))
            .and_then(|g| g.get("coordinates"))
            .and_then(|c| c.as_array())
            .filter(|c| c.len() >= 2)
            .and_then(|c| match (c[0].as_f64(), c[1].as_f64()) {
                (Some(lon), Some(lat)) => Geo::new(lat, lon),
                _ => None,
            });
        let Some(geo) = geo else { continue };

        let name = props.get("VolcanoName").and_then(|v| v.as_str()).unwrap_or("Volcano");
        let start = props.get("StartDate").and_then(|v| v.as_str()).unwrap_or("");
        let time = if start.is_empty() { Utc::now() } else { parse_gvp_date(start) };

        let id = f
            .get("id")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| format!("{name}-{start}"));

        // VEI (0–8) sets severity when present; eruptions are inherently significant.
        let vei = props.get("ExplosivityIndexMax").and_then(serde_json::Value::as_f64);
        let severity = vei.map(|v| (v / 8.0).clamp(0.3, 1.0)).unwrap_or(0.6);

        out.push(Event {
            id: format!("gvp-{id}"),
            source_id: "gvp_volcano".to_string(),
            kind: EventKind::Volcano,
            title: name.to_string(),
            time,
            geo: Some(geo),
            severity: Severity::new(severity),
            url: Some("https://volcano.si.edu/gvp_currenteruptions.cfm".to_string()),
            raw: f.clone(),
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
        {"type":"Feature","id":"E3WebApp_Eruptions1960.1",
         "geometry":{"type":"Point","coordinates":[130.657,31.585]},
         "properties":{"VolcanoName":"Kikai","StartDate":"20251229","ContinuingEruption":"Yes","ExplosivityIndexMax":2}},
        {"type":"Feature","id":"E3WebApp_Eruptions1960.2",
         "geometry":{"type":"Point","coordinates":[-155.287,19.421]},
         "properties":{"VolcanoName":"Kilauea","StartDate":"20251115","ContinuingEruption":""}}
      ]
    }"#;

    #[test]
    fn parses_fixture() {
        let ev = parse_gvp(FIXTURE).unwrap();
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0].kind, EventKind::Volcano);
        assert_eq!(ev[0].title, "Kikai");
        let g = ev[0].geo.unwrap();
        assert!((g.lat - 31.585).abs() < 1e-9 && (g.lon - 130.657).abs() < 1e-9);
        // VEI 2 -> 2/8 = 0.25, clamped up to the 0.3 floor.
        assert!((ev[0].severity.value() - 0.3).abs() < 1e-9);
        // No VEI -> baseline 0.6.
        assert!((ev[1].severity.value() - 0.6).abs() < 1e-9);
        // StartDate parsed to the right year.
        assert_eq!(ev[0].time.format("%Y-%m-%d").to_string(), "2025-12-29");
    }

    #[test]
    fn tolerates_non_json() {
        assert_eq!(parse_gvp(r#"{"type":"x"}"#).unwrap().len(), 0);
    }

    #[test]
    fn date_parser_handles_zero_day() {
        // Zero month/day -> defaults to 01.
        assert_eq!(parse_gvp_date("20260000").format("%Y-%m-%d").to_string(), "2026-01-01");
    }
}
