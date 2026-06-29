//! CBSA Border Wait Times — live wait estimates at Canada's land border crossings with
//! the United States, from the Canada Border Services Agency. Free, no API key
//! (Open Government Licence – Canada).
//!
//! Reads the published `bwt-eng.csv` into normalized [`EventKind::Transport`] [`Event`]s
//! — the 29 federal land crossings nationwide (NB→BC), a national border-flow signal no
//! other feed carries. The CSV is NOT geocoded (it names crossings, not coordinates), so
//! each crossing is placed from a fixed lookup of its well-known location ([`crossing_coord`]).

use async_trait::async_trait;
use chrono::Utc;
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use std::time::Duration;

/// CBSA border-wait-times source.
#[derive(Default)]
pub struct CbsaBwt;

impl CbsaBwt {
    pub fn url(&self) -> &'static str {
        "https://www.cbsa-asfc.gc.ca/bwt-taf/bwt-eng.csv"
    }
}

#[async_trait]
impl Source for CbsaBwt {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "cbsa_bwt",
            name: "CBSA Border Wait Times",
            domain: EventKind::Transport,
            cadence: Duration::from_secs(300),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        let body = crate::http::fetch_text(self.url()).await?;
        parse_cbsa_bwt(&body)
    }
}

/// Fixed coordinate for each CBSA "Customs Office" name (the Canadian side of the
/// crossing). The set is small and stable; an unknown name is skipped (logged by the
/// caller as an empty result for that row) rather than mis-plotted.
fn crossing_coord(name: &str) -> Option<(f64, f64)> {
    let c = match name {
        "St. Stephen" => (45.1801, -67.2766),
        "St. Stephen 3rd Bridge" => (45.1636, -67.2849),
        "Edmundston" => (47.3590, -68.3253),
        "Woodstock Road" => (46.1330, -67.7930),
        "Stanstead" => (45.0053, -72.0970),
        "St-Armand/Philipsburg" => (45.0250, -73.0790),
        "Lacolle: Route 221" => (45.0060, -73.3690),
        "Lacolle: Route 223" => (45.0090, -73.3440),
        "St-Bernard-de-Lacolle" => (45.0050, -73.3720),
        "Hemmingford" => (45.0230, -73.5870),
        "Cornwall Traffic Office" => (45.0020, -74.7320),
        "Prescott" => (44.7050, -75.5160),
        "Thousand Islands Bridge" => (44.3540, -75.9930),
        "Sault Ste. Marie" => (46.5040, -84.3490),
        "Fort Frances Bridge" => (48.6020, -93.4030),
        "Queenston-Lewiston Bridge" => (43.1620, -79.0490),
        "Rainbow Bridge" => (43.0900, -79.0680),
        "Peace Bridge" => (42.9090, -78.9060),
        "Blue Water Bridge" => (43.0010, -82.4170),
        "Windsor and Detroit Tunnel" => (42.3170, -83.0390),
        "Ambassador Bridge" => (42.3120, -83.0730),
        "Emerson" => (49.0040, -97.2100),
        "North Portal" => (49.0010, -102.5560),
        "Coutts" => (49.0000, -111.9580),
        "Abbotsford-Huntingdon" => (49.0030, -122.2650),
        "Aldergrove" => (49.0030, -122.4810),
        "Pacific Highway" => (49.0030, -122.7370),
        "Douglas" => (49.0030, -122.7570),
        "Boundary Bay" => (49.0000, -123.0670),
        _ => return None,
    };
    Some(c)
}

/// Wait in minutes from a CBSA flow cell. `No Delay` -> 0; `N minute(s)` -> N;
/// `Not Applicable` / `--` / empty -> `None` (no data, not zero).
fn wait_minutes(cell: &str) -> Option<u32> {
    let c = cell.trim();
    if c.eq_ignore_ascii_case("No Delay") {
        return Some(0);
    }
    if c.to_ascii_lowercase().contains("minute") {
        let num: String = c.chars().take_while(|ch| ch.is_ascii_digit()).collect();
        return num.parse().ok();
    }
    None
}

/// Pure parser: CBSA `bwt-eng.csv` -> events. Unit-tested offline.
///
/// The CSV is `;;`-delimited, leads with a UTF-8 BOM, and carries four flow columns
/// (Commercial/Travellers × Canada-/U.S.-bound). The plotted wait is the worst across
/// all flows; rows with no numeric/known wait at all are skipped. Time is the fetch
/// time (the feed is a live snapshot; its `Last updated` cell is a zone-abbrev string
/// with no numeric offset, so we don't try to parse it into a precise UTC instant).
pub fn parse_cbsa_bwt(csv: &str) -> anyhow::Result<Vec<Event>> {
    let mut lines = csv.trim_start_matches('\u{feff}').lines();
    let header = lines.next().ok_or_else(|| anyhow::anyhow!("cbsa_bwt: empty body"))?;
    if !header.contains("Customs Office") {
        return Err(anyhow::anyhow!("cbsa_bwt: unexpected header: {header}"));
    }

    let now = Utc::now();
    let mut out = Vec::new();
    for line in lines {
        let cols: Vec<&str> = line.split(";;").map(|c| c.trim()).collect();
        if cols.len() < 7 || cols[0].is_empty() {
            continue;
        }
        let name = cols[0];
        let Some((lat, lon)) = crossing_coord(name) else { continue };
        let Some(geo) = Geo::new(lat, lon) else { continue };

        // Worst wait across the four flow columns (cols 3..=6). None if no flow reports.
        let max_wait = cols[3..7].iter().filter_map(|c| wait_minutes(c)).max();
        let Some(max_wait) = max_wait else { continue };

        let title = if max_wait == 0 {
            format!("{name} — no delay")
        } else {
            format!("{name} — {max_wait} min wait")
        };

        out.push(Event {
            id: format!("cbsa-{name}"),
            source_id: "cbsa_bwt".to_string(),
            kind: EventKind::Transport,
            title,
            time: now,
            geo: Some(geo),
            // >=60 min saturates; a clean crossing is a faint dot (cf. eccc_aqhi).
            severity: Severity::new(max_wait as f64 / 60.0),
            url: Some("https://www.cbsa-asfc.gc.ca/bwt-taf/menu-eng.html".to_string()),
            raw: serde_json::json!({
                "location": cols.get(1).copied().unwrap_or(""),
                "last_updated": cols.get(2).copied().unwrap_or(""),
                "max_wait_min": max_wait,
            }),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = "\u{feff}Customs Office;; Location;; Last updated;; Commercial Flow - Canada bound;; Commercial Flow - U.S. bound;; Travellers Flow - Canada bound;; Travellers Flow - U.S. bound;;\n\
St-Bernard-de-Lacolle;; Saint-Bernard-de-Lacolle, QC/Champlain, NY;; 2026-06-14 15:13 EDT;; No Delay;; --;; No Delay;; 30 minutes;;\n\
Peace Bridge;; Fort Erie, ON/Buffalo, NY;; 2026-06-14 15:20 EDT;; 7 minutes;; --;; 7 minutes;; 4 minutes;;\n\
Edmundston;; Edmundston, NB/Madawaska, ME;; 2026-06-14 07:58 ADT;; Not Applicable;; --;; --;; --;;\n\
Unknown Crossing;; Nowhere;; x;; No Delay;; --;; No Delay;; --;;\n";

    #[test]
    fn parses_fixture() {
        let ev = parse_cbsa_bwt(FIXTURE).unwrap();
        // Edmundston has no numeric/known wait (all --/N.A.) -> skipped; Unknown Crossing
        // has no coordinate -> skipped. Two plotted.
        assert_eq!(ev.len(), 2);

        assert_eq!(ev[0].id, "cbsa-St-Bernard-de-Lacolle");
        assert_eq!(ev[0].kind, EventKind::Transport);
        // Worst flow is 30 min (US-bound travellers).
        assert_eq!(ev[0].title, "St-Bernard-de-Lacolle — 30 min wait");
        assert!((ev[0].severity.value() - 0.5).abs() < 1e-9);
        let g = ev[0].geo.unwrap();
        assert!((g.lat - 45.005).abs() < 0.01 && (g.lon + 73.372).abs() < 0.01);

        // Peace Bridge worst flow is 7 min.
        assert_eq!(ev[1].title, "Peace Bridge — 7 min wait");
    }

    #[test]
    fn errors_on_bad_header() {
        assert!(parse_cbsa_bwt("not;;a;;cbsa;;file\n").is_err());
    }

    #[test]
    fn wait_minutes_vocab() {
        assert_eq!(wait_minutes("No Delay"), Some(0));
        assert_eq!(wait_minutes("30 minutes"), Some(30));
        assert_eq!(wait_minutes("1 minute"), Some(1));
        assert_eq!(wait_minutes("--"), None);
        assert_eq!(wait_minutes("Not Applicable"), None);
        assert_eq!(wait_minutes(""), None);
    }
}
