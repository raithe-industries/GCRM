//! UCDP Georeferenced Event Dataset (Candidate) — recent georeferenced organized-violence
//! events worldwide, from the Uppsala Conflict Data Program (Uppsala University), the
//! standard academic conflict-event source. Free, no API key (the live API is now
//! token-gated, but the candidate-GED CSV is a public direct download).
//!
//! Fills the [`EventKind::Conflict`] layer (the credentialed `acled` feed is dormant —
//! ACLED Open access has no API). Each event carries lat/lon, a best fatality estimate,
//! and a violence type; severity is log-scaled by fatalities. Monthly-updated.
//! Source: UCDP, Department of Peace and Conflict Research, Uppsala University.

use async_trait::async_trait;
use chrono::{NaiveDate, TimeZone, Utc};
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use std::time::Duration;

/// Fallback if the downloads page can't be scraped for the current version.
const DEFAULT_URL: &str = "https://ucdp.uu.se/downloads/candidateged/GEDEvent_v26_0_4.csv";

/// UCDP candidate-GED source.
#[derive(Default)]
pub struct UcdpGed;

#[async_trait]
impl Source for UcdpGed {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "ucdp_ged",
            name: "UCDP Conflict Events (Uppsala)",
            domain: EventKind::Conflict,
            // Candidate GED is republished monthly; no need to poll hard.
            cadence: Duration::from_secs(21_600),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        let client = reqwest::Client::builder()
            .user_agent("engineering-effects/0.1 (+https://raithe.ca)")
            .build()?;
        // Discover the current candidate-GED CSV (the filename carries a monthly version),
        // falling back to a known-good URL if the listing can't be read.
        let url = match client.get("https://ucdp.uu.se/downloads/").send().await {
            Ok(r) => r.text().await.ok().and_then(|p| pick_candidate_url(&p)).unwrap_or_else(|| DEFAULT_URL.to_string()),
            Err(_) => DEFAULT_URL.to_string(),
        };
        let body = client.get(&url).send().await?.text().await?;
        parse_ucdp_ged(&body)
    }
}

/// Newest `candidateged/GEDEvent_v*.csv` path referenced on the downloads page (lexically
/// greatest = latest version), as an absolute URL.
fn pick_candidate_url(page: &str) -> Option<String> {
    let needle = "candidateged/GEDEvent_v";
    let mut best: Option<&str> = None;
    let mut start = 0;
    while let Some(i) = page[start..].find(needle) {
        let abs = start + i;
        match page[abs..].find(".csv") {
            Some(end) => {
                let frag = &page[abs..abs + end + 4];
                if best.map_or(true, |b| frag > b) {
                    best = Some(frag);
                }
                start = abs + end + 4;
            }
            None => break,
        }
    }
    best.map(|f| format!("https://ucdp.uu.se/downloads/{f}"))
}

/// RFC4180-style CSV reader: quote-aware, handling embedded commas, escaped `""`, and
/// newlines inside quoted fields (UCDP source-text columns contain all three).
fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let mut records = Vec::new();
    let mut record: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
        } else {
            match c {
                '"' => in_quotes = true,
                ',' => record.push(std::mem::take(&mut field)),
                '\r' => {}
                '\n' => {
                    record.push(std::mem::take(&mut field));
                    records.push(std::mem::take(&mut record));
                }
                _ => field.push(c),
            }
        }
    }
    if !field.is_empty() || !record.is_empty() {
        record.push(field);
        records.push(record);
    }
    records
}

/// State-based / non-state / one-sided violence label.
fn violence_label(tov: &str) -> &'static str {
    match tov {
        "1" => "State-based",
        "2" => "Non-state",
        "3" => "One-sided",
        _ => "Conflict",
    }
}

/// Pure parser: UCDP candidate-GED CSV -> events, newest first. Offline-tested.
pub fn parse_ucdp_ged(csv: &str) -> anyhow::Result<Vec<Event>> {
    let rows = parse_csv(csv);
    let header = rows.first().ok_or_else(|| anyhow::anyhow!("ucdp_ged: empty CSV"))?;
    let col = |name: &str| header.iter().position(|h| h == name);
    let (Some(i_id), Some(i_lat), Some(i_lon), Some(i_best)) =
        (col("id"), col("latitude"), col("longitude"), col("best"))
    else {
        return Err(anyhow::anyhow!("ucdp_ged: missing expected columns"));
    };
    let (i_tov, i_country, i_end) = (col("type_of_violence"), col("country"), col("date_end"));

    let get = |r: &[String], idx: Option<usize>| -> String {
        idx.and_then(|i| r.get(i)).cloned().unwrap_or_default()
    };

    let mut out = Vec::new();
    for r in rows.iter().skip(1) {
        let id = r.get(i_id).cloned().unwrap_or_default();
        let (Some(lat), Some(lon)) = (
            r.get(i_lat).and_then(|s| s.trim().parse::<f64>().ok()),
            r.get(i_lon).and_then(|s| s.trim().parse::<f64>().ok()),
        ) else {
            continue;
        };
        let Some(geo) = Geo::new(lat, lon) else { continue };
        if id.is_empty() {
            continue;
        }

        let best: f64 = r.get(i_best).and_then(|s| s.trim().parse().ok()).unwrap_or(0.0);
        let tov = violence_label(&get(r, i_tov));
        let country = get(r, i_country);
        let country = if country.is_empty() { "Unknown".to_string() } else { country };

        let title = if best >= 1.0 {
            format!("{country} — {best:.0} killed ({tov})")
        } else {
            format!("{country} — conflict event ({tov})")
        };

        // date_end is "YYYY-MM-DD HH:MM:SS.sss"; anchor at UTC midnight of the date.
        let time = get(r, i_end)
            .get(0..10)
            .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .map(|dt| Utc.from_utc_datetime(&dt))
            .unwrap_or_else(Utc::now);

        // Log-scaled by fatalities: ~1 death faint, ~1000+ saturates; a located event
        // with 0 recorded deaths still gets a small floor (it is a real clash).
        let severity = ((1.0 + best).ln() / 7.0).clamp(0.12, 1.0);

        out.push(Event {
            id: format!("ucdp-{id}"),
            source_id: "ucdp_ged".to_string(),
            kind: EventKind::Conflict,
            title,
            time,
            geo: Some(geo),
            severity: Severity::new(severity),
            url: Some("https://ucdp.uu.se/".to_string()),
            raw: serde_json::json!({ "best": best, "type": tov, "country": country }),
        });
    }

    // Most recent first, so a downstream cap keeps current events.
    out.sort_by(|a, b| b.time.cmp(&a.time));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A quoted source-headline field carries a comma + escaped quote to exercise the parser.
    const FIXTURE: &str = "id,type_of_violence,source_headline,latitude,longitude,best,country,date_end\n\
625373,1,\"Strikes hit Tehran, \"\"heavy\"\" toll\",35.835556,51.010278,13,Iran,2026-04-02 00:00:00.000\n\
700000,2,\"Clashes, no deaths\",9.05,7.49,0,Nigeria,2026-05-13 00:00:00.000\n\
1,1,bad-coords,999,0,5,Nowhere,2026-01-01 00:00:00.000\n";

    #[test]
    fn parses_fixture() {
        let ev = parse_ucdp_ged(FIXTURE).unwrap();
        // Out-of-range-coord row dropped; two valid events, newest (Nigeria 2026-05-13) first.
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0].id, "ucdp-700000");
        assert_eq!(ev[0].kind, EventKind::Conflict);
        assert_eq!(ev[0].title, "Nigeria — conflict event (Non-state)");
        // 0 deaths -> severity floor.
        assert!((ev[0].severity.value() - 0.12).abs() < 1e-9);

        assert_eq!(ev[1].id, "ucdp-625373");
        assert_eq!(ev[1].title, "Iran — 13 killed (State-based)");
        let g = ev[1].geo.unwrap();
        assert!((g.lat - 35.835556).abs() < 1e-6 && (g.lon - 51.010278).abs() < 1e-6);
        assert!(ev[1].severity.value() > 0.12); // 13 deaths > floor
    }

    #[test]
    fn csv_handles_quoted_commas_and_escapes() {
        let rows = parse_csv("a,b,c\n1,\"x,y\",\"he said \"\"hi\"\"\"\n");
        assert_eq!(rows[1], vec!["1", "x,y", "he said \"hi\""]);
    }

    #[test]
    fn picks_latest_candidate_url() {
        let page = r#"<a href="downloads/candidateged/GEDEvent_v26_0_3.csv">x</a>
                      <a href="downloads/candidateged/GEDEvent_v26_0_4.csv">y</a>"#;
        assert_eq!(
            pick_candidate_url(page).as_deref(),
            Some("https://ucdp.uu.se/downloads/candidateged/GEDEvent_v26_0_4.csv")
        );
        assert!(pick_candidate_url("nothing here").is_none());
    }

    #[test]
    fn errors_on_missing_columns() {
        assert!(parse_ucdp_ged("foo,bar\n1,2\n").is_err());
    }
}
