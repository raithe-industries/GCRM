//! ACLED — Armed Conflict Location & Event Data. The gold standard for global,
//! point-geocoded conflict events. Requires a free myACLED account (OAuth); reads
//! `ACLED_USERNAME` / `ACLED_PASSWORD` (+ optional `ACLED_CLIENT_ID`, default "acled").
//!
//! Auth flow (current acleddata.com API): POST credentials to `/oauth/token`
//! (grant_type=password) for a bearer token, then GET `/api/acled/read`. Normalizes
//! recent events to [`EventKind::Conflict`] [`Event`]s — fills the long-dormant
//! Armed-Conflict layer.

use async_trait::async_trait;
use chrono::{NaiveDate, TimeZone, Utc};
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use std::time::Duration;

/// ACLED conflict source. `days` bounds the look-back window.
pub struct Acled {
    pub days: i64,
}

impl Default for Acled {
    fn default() -> Self {
        Self { days: 30 }
    }
}

#[async_trait]
impl Source for Acled {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "acled",
            name: "ACLED Armed Conflict (global)",
            domain: EventKind::Conflict,
            cadence: Duration::from_secs(3600),
            needs_key: true,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        // No credentials → stay dormant (no error), like the OpenSky/FIRMS auth paths.
        let (Ok(user), Ok(pass)) = (std::env::var("ACLED_USERNAME"), std::env::var("ACLED_PASSWORD"))
        else {
            return Ok(Vec::new());
        };
        let client_id = std::env::var("ACLED_CLIENT_ID").unwrap_or_else(|_| "acled".to_string());
        let client = reqwest::Client::builder()
            .user_agent("engineering-effects/0.1 (+https://raithe.ca)")
            .build()?;

        // 1) OAuth password grant -> bearer token.
        let tok: serde_json::Value = client
            .post("https://acleddata.com/oauth/token")
            .form(&[
                ("grant_type", "password"),
                ("username", user.as_str()),
                ("password", pass.as_str()),
                ("client_id", client_id.as_str()),
            ])
            .send()
            .await?
            .json()
            .await?;
        let Some(token) = tok.get("access_token").and_then(|t| t.as_str()) else {
            anyhow::bail!("acled: no access_token (check ACLED_USERNAME/PASSWORD)");
        };

        // 2) Recent events, point-geocoded.
        let since = (Utc::now() - chrono::Duration::days(self.days)).format("%Y-%m-%d");
        let body = client
            .get("https://acleddata.com/api/acled/read")
            .query(&[
                ("_format", "json"),
                ("limit", "1000"),
                ("event_date", &since.to_string()),
                ("event_date_where", ">="),
            ])
            .bearer_auth(token)
            .send()
            .await?
            .text()
            .await?;
        parse_acled(&body)
    }
}

/// Map ACLED fatalities -> normalized severity (a deadly event reads louder).
fn severity_for(fatalities: f64) -> f64 {
    if fatalities <= 0.0 {
        0.35
    } else {
        (0.45 + fatalities / 25.0).clamp(0.45, 1.0)
    }
}

/// Pure parser: ACLED `read` JSON -> events. Unit-tested offline.
pub fn parse_acled(json: &str) -> anyhow::Result<Vec<Event>> {
    let root: serde_json::Value = serde_json::from_str(json)?;
    let Some(data) = root.get("data").and_then(|d| d.as_array()) else {
        return Ok(Vec::new());
    };

    let mut out = Vec::with_capacity(data.len());
    for e in data {
        // ACLED lat/lon arrive as strings.
        let num = |k: &str| {
            e.get(k).and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
        };
        let (Some(lat), Some(lon)) = (num("latitude"), num("longitude")) else { continue };
        let Some(geo) = Geo::new(lat, lon) else { continue };

        let s = |k: &str| e.get(k).and_then(|v| v.as_str()).unwrap_or("");
        let id = s("event_id_cnty");
        if id.is_empty() {
            continue;
        }
        let etype = s("event_type");
        let location = s("location");
        let country = s("country");
        let title = match (location.is_empty(), country.is_empty()) {
            (false, false) => format!("{etype} — {location}, {country}"),
            (true, false) => format!("{etype} — {country}"),
            _ => etype.to_string(),
        };
        let fatalities = e
            .get("fatalities")
            .and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|x| x.parse().ok())))
            .unwrap_or(0.0);
        let time = NaiveDate::parse_from_str(s("event_date"), "%Y-%m-%d")
            .ok()
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .map(|dt| Utc.from_utc_datetime(&dt))
            .unwrap_or_else(Utc::now);

        out.push(Event {
            id: format!("acled-{id}"),
            source_id: "acled".to_string(),
            kind: EventKind::Conflict,
            title,
            time,
            geo: Some(geo),
            severity: Severity::new(severity_for(fatalities)),
            url: Some("https://acleddata.com/".to_string()),
            raw: e.clone(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
      "status":200,"success":true,"count":2,
      "data":[
        {"event_id_cnty":"SYR1","event_date":"2026-06-12","event_type":"Battles",
         "sub_event_type":"Armed clash","country":"Syria","location":"Aleppo",
         "latitude":"36.2021","longitude":"37.1343","fatalities":"7"},
        {"event_id_cnty":"SDN9","event_date":"2026-06-11","event_type":"Protests",
         "country":"Sudan","location":"Khartoum","latitude":"15.5007","longitude":"32.5599","fatalities":"0"}
      ]
    }"#;

    #[test]
    fn parses_fixture() {
        let ev = parse_acled(FIXTURE).unwrap();
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0].kind, EventKind::Conflict);
        assert_eq!(ev[0].id, "acled-SYR1");
        assert_eq!(ev[0].title, "Battles — Aleppo, Syria");
        let g = ev[0].geo.unwrap();
        assert!((g.lat - 36.2021).abs() < 1e-6 && (g.lon - 37.1343).abs() < 1e-6);
        // 7 fatalities -> 0.45 + 7/25 = 0.73.
        assert!((ev[0].severity.value() - 0.73).abs() < 1e-9);
        // 0 fatalities -> 0.35 floor.
        assert!((ev[1].severity.value() - 0.35).abs() < 1e-9);
    }

    #[test]
    fn tolerates_no_data() {
        assert_eq!(parse_acled(r#"{"status":401,"message":"denied"}"#).unwrap().len(), 0);
    }
}
