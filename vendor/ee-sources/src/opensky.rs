//! OpenSky Network — live civil/military aircraft positions. Free, no API key
//! (anonymous access is rate-limited; an optional bounding box keeps responses small).
//!
//! Parses the OpenSky `/states/all` JSON
//! (<https://opensky-network.org/api/states/all>) into normalized
//! [`EventKind::Aircraft`] [`Event`]s, one per tracked aircraft with a known position.

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use std::time::Duration;

/// OpenSky aircraft-state source. With no `bbox`, fetches the (large) global set;
/// set a bounding box `(min_lat, min_lon, max_lat, max_lon)` to scope it.
#[derive(Default)]
pub struct OpenSky {
    pub bbox: Option<(f64, f64, f64, f64)>,
}

impl OpenSky {
    pub fn url(&self) -> String {
        let base = "https://opensky-network.org/api/states/all".to_string();
        match self.bbox {
            Some((la1, lo1, la2, lo2)) => {
                format!("{base}?lamin={la1}&lomin={lo1}&lamax={la2}&lomax={lo2}")
            }
            None => base,
        }
    }
}

#[async_trait]
impl Source for OpenSky {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "opensky",
            name: "OpenSky Aircraft",
            domain: EventKind::Aircraft,
            cadence: Duration::from_secs(60),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        let client = reqwest::Client::builder()
            .user_agent("engineering-effects/0.1 (+https://raithe.ca)")
            .build()?;
        let body = client.get(self.url()).send().await?.text().await?;
        parse_opensky(&body)
    }
}

/// Pure parser: OpenSky `/states/all` JSON -> events. Unit-tested offline.
///
/// Each state is a 17-element array; the indices we use:
/// `0` icao24, `1` callsign, `2` origin_country, `4` last_contact (unix s),
/// `5` longitude, `6` latitude, `14` squawk. Aircraft with no position fix are
/// skipped (a positionless aircraft can't be placed on a map).
pub fn parse_opensky(json: &str) -> anyhow::Result<Vec<Event>> {
    let root: serde_json::Value = serde_json::from_str(json)?;
    let states = root
        .get("states")
        .and_then(|s| s.as_array())
        .ok_or_else(|| anyhow::anyhow!("opensky: missing 'states' array"))?;

    let mut out = Vec::with_capacity(states.len());
    for s in states {
        let a = match s.as_array() {
            Some(a) if a.len() >= 15 => a,
            _ => continue,
        };

        let icao = a[0].as_str().unwrap_or("").trim().to_string();
        if icao.is_empty() {
            continue;
        }

        // Position fix is mandatory for a map point.
        let geo = match (a[6].as_f64(), a[5].as_f64()) {
            (Some(lat), Some(lon)) => match Geo::new(lat, lon) {
                Some(g) => g,
                None => continue,
            },
            _ => continue,
        };

        let callsign = a[1].as_str().unwrap_or("").trim();
        let country = a[2].as_str().unwrap_or("");
        let ident = if callsign.is_empty() { icao.as_str() } else { callsign };
        let title = if country.is_empty() {
            ident.to_string()
        } else {
            format!("{ident} · {country}")
        };

        let time = a[4]
            .as_i64()
            .and_then(|s| Utc.timestamp_opt(s, 0).single())
            .unwrap_or_else(Utc::now);

        // Emergency squawk codes are the only intrinsic severity signal here:
        // 7500 hijack, 7600 radio failure, 7700 general emergency.
        let squawk = a[14].as_str().unwrap_or("");
        let severity = match squawk {
            "7500" | "7600" | "7700" => 0.9,
            _ => 0.1,
        };

        out.push(Event {
            id: icao,
            source_id: "opensky".to_string(),
            kind: EventKind::Aircraft,
            title,
            time,
            geo: Some(geo),
            severity: Severity::new(severity),
            url: None,
            raw: s.clone(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Two valid states (one with an emergency squawk) and one with no position.
    const FIXTURE: &str = r#"{
      "time": 1780922631,
      "states": [
        ["abc123","DLH456  ","Germany",1780922600,1780922631,8.5,50.1,11000,false,230.0,180.0,0,null,11200,"1000",false,0],
        ["def456","N99EM   ","United States",1780922600,1780922631,-95.4,29.8,9000,false,200.0,90.0,0,null,9100,"7700",false,0],
        ["ghost1","BLANK   ","Nowhere",1780922600,1780922631,null,null,null,true,0,0,0,null,null,"2000",false,0]
      ]
    }"#;

    #[test]
    fn parses_fixture() {
        let ev = parse_opensky(FIXTURE).unwrap();
        // The positionless third state is skipped.
        assert_eq!(ev.len(), 2);

        assert_eq!(ev[0].id, "abc123");
        assert_eq!(ev[0].kind, EventKind::Aircraft);
        assert_eq!(ev[0].title, "DLH456 · Germany");
        let g = ev[0].geo.unwrap();
        assert!((g.lat - 50.1).abs() < 1e-9 && (g.lon - 8.5).abs() < 1e-9);
        assert!((ev[0].severity.value() - 0.1).abs() < 1e-9);

        // Emergency squawk 7700 -> elevated severity.
        assert_eq!(ev[1].id, "def456");
        assert!((ev[1].severity.value() - 0.9).abs() < 1e-9);
    }

    #[test]
    fn errors_on_missing_array() {
        assert!(parse_opensky(r#"{"time":1}"#).is_err());
    }

    #[test]
    fn url_includes_bbox() {
        let s = OpenSky { bbox: Some((45.0, 5.0, 47.0, 10.0)) };
        assert!(s.url().contains("lamin=45"));
        assert!(OpenSky::default().url().ends_with("/states/all"));
    }
}
