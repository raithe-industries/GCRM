//! NASA FIRMS — global satellite active-fire detections. Requires a free MAP_KEY
//! (`FIRMS_MAP_KEY` env). The single highest-volume global fire source; densifies the
//! Wildfire layer worldwide (CWFIS only covers Canada/N-America).
//!
//! Reads the FIRMS area CSV API (MODIS by default — ~10× fewer points than VIIRS, so
//! one global pull stays light) into normalized [`EventKind::Wildfire`] [`Event`]s.

use async_trait::async_trait;
use chrono::{NaiveDate, TimeZone, Utc};
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use std::time::Duration;

/// NASA FIRMS active-fire source. `source` = a FIRMS product (e.g. `MODIS_NRT`,
/// `VIIRS_SNPP_NRT`); `area` = a bbox or `world`; `days` = look-back (1–10).
pub struct Firms {
    pub source: String,
    pub area: String,
    pub days: u32,
}

impl Default for Firms {
    fn default() -> Self {
        Self { source: "MODIS_NRT".into(), area: "world".into(), days: 1 }
    }
}

#[async_trait]
impl Source for Firms {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "firms",
            name: "NASA FIRMS Wildfires (global)",
            domain: EventKind::Wildfire,
            cadence: Duration::from_secs(900),
            needs_key: true,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        // No key configured → stay dormant (no error spam), like the OpenSky auth path.
        let Ok(key) = std::env::var("FIRMS_MAP_KEY") else {
            return Ok(Vec::new());
        };
        let url = format!(
            "https://firms.modaps.eosdis.nasa.gov/api/area/csv/{key}/{}/{}/{}",
            self.source, self.area, self.days
        );
        let client = reqwest::Client::builder()
            .user_agent("engineering-effects/0.1 (+https://raithe.ca)")
            .build()?;
        let body = client.get(url).send().await?.text().await?;
        parse_firms(&body)
    }
}

/// Pure parser: FIRMS area CSV -> events, busiest (highest FRP) first so a downstream
/// cap keeps the most significant fires. Low-confidence detections are dropped. The
/// MAP_KEY never appears here (it's only in the request URL). Unit-tested offline.
pub fn parse_firms(csv: &str) -> anyhow::Result<Vec<Event>> {
    let mut lines = csv.lines();
    // Header maps column names to indices (FIRMS column order varies by product).
    let Some(header) = lines.next() else { return Ok(Vec::new()) };
    if !header.starts_with("latitude") {
        // An error/HTML body (bad key, throttle) — treat as "no fires", not a hard error.
        return Ok(Vec::new());
    }
    let idx = |name: &str| header.split(',').position(|h| h == name);
    let (ci_lat, ci_lon) = (idx("latitude"), idx("longitude"));
    let (Some(ci_lat), Some(ci_lon)) = (ci_lat, ci_lon) else { return Ok(Vec::new()) };
    let ci_conf = idx("confidence");
    let ci_frp = idx("frp");
    let ci_date = idx("acq_date");
    let ci_time = idx("acq_time");
    let ci_sat = idx("satellite");

    let mut out = Vec::new();
    for line in lines {
        let c: Vec<&str> = line.split(',').collect();
        let get = |i: Option<usize>| i.and_then(|i| c.get(i)).map(|s| s.trim());
        let (Some(lat), Some(lon)) = (
            get(Some(ci_lat)).and_then(|s| s.parse::<f64>().ok()),
            get(Some(ci_lon)).and_then(|s| s.parse::<f64>().ok()),
        ) else {
            continue;
        };
        let Some(geo) = Geo::new(lat, lon) else { continue };

        // MODIS confidence is 0–100; VIIRS is l/n/h. Drop the lowest tier either way.
        let conf_raw = get(ci_conf).unwrap_or("");
        let low_conf = conf_raw.parse::<f64>().map(|n| n < 50.0).unwrap_or(false) || conf_raw == "l";
        if low_conf {
            continue;
        }

        let frp = get(ci_frp).and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
        let sat = get(ci_sat).unwrap_or("");
        let date = get(ci_date).unwrap_or("");
        let hhmm = get(ci_time).unwrap_or("");
        let time = parse_firms_time(date, hhmm);

        out.push(Event {
            id: format!("firms-{lat:.4}-{lon:.4}-{date}-{hhmm}"),
            source_id: "firms".to_string(),
            kind: EventKind::Wildfire,
            title: if sat.is_empty() {
                "Active fire".to_string()
            } else {
                format!("Active fire ({sat})")
            },
            time,
            geo: Some(geo),
            severity: Severity::new((frp / 100.0).clamp(0.3, 1.0)),
            url: Some("https://firms.modaps.eosdis.nasa.gov/map/".to_string()),
            raw: serde_json::json!({ "frp": frp, "confidence": conf_raw, "satellite": sat }),
        });
    }
    // Biggest fires first → a per-feed cap keeps the most significant globally.
    out.sort_by(|a, b| {
        let fa = a.raw.get("frp").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
        let fb = b.raw.get("frp").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
        fb.partial_cmp(&fa).unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(out)
}

/// FIRMS `acq_date` (YYYY-MM-DD) + `acq_time` (UTC HHMM, e.g. "3" = 00:03) -> UTC.
fn parse_firms_time(date: &str, hhmm: &str) -> chrono::DateTime<Utc> {
    if let Ok(d) = NaiveDate::parse_from_str(date, "%Y-%m-%d") {
        let padded = format!("{hhmm:0>4}");
        let h: u32 = padded.get(0..2).and_then(|x| x.parse().ok()).unwrap_or(0);
        let m: u32 = padded.get(2..4).and_then(|x| x.parse().ok()).unwrap_or(0);
        if let Some(dt) = d.and_hms_opt(h.min(23), m.min(59), 0) {
            return Utc.from_utc_datetime(&dt);
        }
    }
    Utc::now()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = "latitude,longitude,brightness,scan,track,acq_date,acq_time,satellite,instrument,confidence,version,bright_t31,frp,daynight\n\
-14.01663,133.34868,306.17,1.4,1.17,2026-06-13,3,Terra,MODIS,45,6.1NRT,296.16,6.22,D\n\
38.5,-120.4,330.0,1.0,1.0,2026-06-13,1430,Aqua,MODIS,80,6.1NRT,300.0,150.0,D\n\
51.2,10.1,320.0,1.0,1.0,2026-06-13,1200,Terra,MODIS,65,6.1NRT,298.0,40.0,D\n";

    #[test]
    fn parses_filters_and_sorts() {
        let ev = parse_firms(FIXTURE).unwrap();
        // Row 1 (confidence 45 < 50) is dropped; rows 2 & 3 kept, sorted by FRP desc.
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0].kind, EventKind::Wildfire);
        // Highest FRP (150) first.
        assert!((ev[0].geo.unwrap().lat - 38.5).abs() < 1e-9);
        assert_eq!(ev[0].title, "Active fire (Aqua)");
        // frp 150 -> severity clamps to 1.0; frp 40 -> 0.4.
        assert!((ev[0].severity.value() - 1.0).abs() < 1e-9);
        assert!((ev[1].severity.value() - 0.4).abs() < 1e-9);
        assert_eq!(ev[0].time.format("%Y-%m-%d %H:%M").to_string(), "2026-06-13 14:30");
    }

    #[test]
    fn tolerates_error_body() {
        // A non-CSV body (bad key / throttle) yields no fires, not an error.
        assert_eq!(parse_firms("Invalid MAP_KEY").unwrap().len(), 0);
        assert_eq!(parse_firms("").unwrap().len(), 0);
    }
}
