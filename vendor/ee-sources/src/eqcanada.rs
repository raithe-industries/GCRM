//! Earthquakes Canada (Natural Resources Canada) — the national seismograph network.
//! Free, no API key, via the standard FDSN event web service.
//!
//! USGS's global feed only surfaces the largest Canadian quakes; NRCan's catalogue
//! adds the dense small-magnitude Canadian seismicity (and felt mining events) that
//! USGS drops. The FDSN service does not offer GeoJSON (it 422s), so we read the
//! pipe-delimited `format=text` table and normalize it to [`EventKind::Earthquake`].

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use std::time::Duration;

/// Earthquakes Canada source. `days` bounds how far back the catalogue is pulled.
pub struct EqCanada {
    pub days: u32,
}

impl Default for EqCanada {
    fn default() -> Self {
        Self { days: 7 }
    }
}

impl EqCanada {
    pub fn url(&self) -> String {
        // `www` host directly — the bare host 301-redirects. `format=text` is the
        // FDSN tabular form; `format=geojson` is rejected by this service.
        let start = (Utc::now() - chrono::Duration::days(self.days as i64))
            .format("%Y-%m-%d");
        format!(
            "https://www.earthquakescanada.nrcan.gc.ca/fdsnws/event/1/query?starttime={start}&format=text"
        )
    }
}

#[async_trait]
impl Source for EqCanada {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "eqcanada",
            name: "Earthquakes Canada (NRCan)",
            domain: EventKind::Earthquake,
            cadence: Duration::from_secs(300),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        let client = reqwest::Client::builder()
            .user_agent("engineering-effects/0.1 (+https://raithe.ca)")
            .build()?;
        let body = client.get(self.url()).send().await?.text().await?;
        parse_eqcanada(&body)
    }
}

/// Pure parser: FDSN `format=text` table -> events. Unit-tested offline.
///
/// Columns: `EventID|Time|Latitude|Longitude|Depth/km|MagType|Magnitude|EventLocationName`.
/// Comment/header lines (leading `#`) and short/garbled rows are skipped; an empty
/// body (FDSN's no-data response) yields no events rather than an error.
pub fn parse_eqcanada(text: &str) -> anyhow::Result<Vec<Event>> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.split('|').collect();
        if cols.len() < 8 {
            continue;
        }
        let event_id = cols[0].trim();
        let (Some(lat), Some(lon)) = (cols[2].trim().parse::<f64>().ok(), cols[3].trim().parse::<f64>().ok())
        else {
            continue;
        };
        let Some(geo) = Geo::new(lat, lon) else { continue };

        let time = DateTime::parse_from_rfc3339(cols[1].trim())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());
        let mag = cols[6].trim().parse::<f64>().unwrap_or(0.0);

        // Location names are bilingual ("English/Français"); keep the English half.
        let place = cols[7].split('/').next().unwrap_or(cols[7]).trim();
        let title = if place.is_empty() {
            "Earthquake (Canada)".to_string()
        } else {
            place.to_string()
        };

        out.push(Event {
            id: format!("eqcanada-{event_id}"),
            source_id: "eqcanada".to_string(),
            kind: EventKind::Earthquake,
            title,
            time,
            geo: Some(geo),
            // Same normalization as USGS: magnitude ~0..9 -> [0, 1].
            severity: Severity::new(mag / 9.0),
            url: Some("https://earthquakescanada.nrcan.gc.ca".to_string()),
            raw: serde_json::json!({
                "event_id": event_id, "mag": mag, "mag_type": cols[5].trim(),
                "depth_km": cols[4].trim().parse::<f64>().ok(), "place": place,
            }),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = "#EventID|Time|Latitude|Longitude|Depth/km|MagType|Magnitude|EventLocationName\n\
20260613.0638001|2026-06-13T06:38:16.000Z|48.5321|-71.157|0.49|MwN|2.72|Mining event, Niobec Mine, QC, felt/Événement minier, Mine Niobec, QC, ressenti\n\
20260612.1228001|2026-06-12T12:28:25.000Z|47.6181|-127.8314|10|Mw'|2.85|287 km SW of Port Alberni, BC/287 km SO de Port Alberni, BC\n\
garbage-row-without-enough-fields\n";

    #[test]
    fn parses_fixture() {
        let ev = parse_eqcanada(FIXTURE).unwrap();
        // Header + garbage row dropped; two real quakes remain.
        assert_eq!(ev.len(), 2);

        assert_eq!(ev[0].id, "eqcanada-20260613.0638001");
        assert_eq!(ev[0].kind, EventKind::Earthquake);
        // English half of the bilingual location only.
        assert_eq!(ev[0].title, "Mining event, Niobec Mine, QC, felt");
        let g = ev[0].geo.unwrap();
        assert!((g.lat - 48.5321).abs() < 1e-6 && (g.lon + 71.157).abs() < 1e-6);
        assert!((ev[0].severity.value() - 2.72 / 9.0).abs() < 1e-9);

        assert_eq!(ev[1].title, "287 km SW of Port Alberni, BC");
    }

    #[test]
    fn empty_body_yields_no_events() {
        assert!(parse_eqcanada("").unwrap().is_empty());
        // Header-only (FDSN no-data) is also clean.
        assert!(parse_eqcanada("#EventID|Time|Latitude|Longitude|Depth/km|MagType|Magnitude|Name").unwrap().is_empty());
    }

    #[test]
    fn url_uses_www_host_and_text_format() {
        let u = EqCanada::default().url();
        assert!(u.starts_with("https://www.earthquakescanada.nrcan.gc.ca/fdsnws/event/1/query"));
        assert!(u.contains("format=text"));
        assert!(u.contains("starttime="));
    }
}
