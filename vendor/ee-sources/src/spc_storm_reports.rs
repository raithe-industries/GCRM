//! NOAA / NWS Storm Prediction Center (SPC) — confirmed severe-storm reports.
//! Free, no API key. U.S. Government public domain (credit "NOAA / NWS SPC").
//!
//! Reads SPC's daily "today" Local Storm Report files — three plain CSVs, one per
//! hazard type:
//!   * `today_torn.csv` — confirmed **tornado** touchdowns
//!   * `today_hail.csv` — large-**hail** reports (size in inches)
//!   * `today_wind.csv` — damaging-**wind** reports (gust in mph)
//!
//! Each file is the same 8-column layout — `Time, <magnitude>, Location, County,
//! State, Lat, Lon, Comments` — where the 2nd column is `F_Scale` (tornado), `Size`
//! (hail) or `Speed` (wind). The first line is a header. The free-text `Comments`
//! field can itself contain commas, so each data row is split into the first seven
//! fields plus an everything-else comment (`splitn(8, ',')`).
//!
//! This connector emits one normalized [`EventKind::Weather`] [`Event`] per report,
//! plotted at the report's own lat/lon. These are **confirmed ground-truth severe-
//! weather occurrences** — the touchdown/impact, not a forecast — a modality no
//! current GCRM feed carries: NWS/ECCC ship *warnings* (what may happen), NHC/JMA
//! ship cyclone *tracks*, NWPS ships river flooding, AWC ships en-route aviation
//! hazards. SPC is the authoritative U.S. severe-convective body.
//!
//! Signal-meaningfulness: every plotted value carries real-world meaning + units —
//! a confirmed tornado (with EF rating when assessed), hail diameter in inches
//! (severe ≥ 1.0", significant ≥ 2.0"), or a wind gust in mph (severe ≥ 58 mph,
//! significant ≥ 75 mph). No raw baseline-free scalar. An empty report day (just the
//! CSV header, no rows — common early in the UTC day or in quiet weather) yields
//! zero events, not an error.

use async_trait::async_trait;
use chrono::Utc;
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use serde_json::{json, Value};
use std::time::Duration;

/// NOAA SPC daily severe-storm reports source.
#[derive(Default)]
pub struct SpcStormReports;

impl SpcStormReports {
    /// The three "today" per-hazard report CSVs (tornado / hail / wind). SPC keeps a
    /// stable, no-date `today_*.csv` alias for each, refreshed through the UTC day.
    pub fn torn_url(&self) -> &'static str {
        "https://www.spc.noaa.gov/climo/reports/today_torn.csv"
    }
    pub fn hail_url(&self) -> &'static str {
        "https://www.spc.noaa.gov/climo/reports/today_hail.csv"
    }
    pub fn wind_url(&self) -> &'static str {
        "https://www.spc.noaa.gov/climo/reports/today_wind.csv"
    }
}

#[async_trait]
impl Source for SpcStormReports {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "spc_storm_reports",
            name: "NOAA SPC storm reports",
            domain: EventKind::Weather,
            cadence: Duration::from_secs(1800),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        // Three small CSVs, one per hazard type. Each names its own report kind, so
        // the type is known from which file a row came (the files share a layout).
        let torn = crate::http::fetch_text(self.torn_url()).await?;
        let hail = crate::http::fetch_text(self.hail_url()).await?;
        let wind = crate::http::fetch_text(self.wind_url()).await?;
        parse_spc_reports(&torn, &hail, &wind)
    }
}

/// The three hazard kinds, one per source CSV.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReportKind {
    Tornado,
    Hail,
    Wind,
}

impl ReportKind {
    fn tag(self) -> &'static str {
        match self {
            ReportKind::Tornado => "torn",
            ReportKind::Hail => "hail",
            ReportKind::Wind => "wind",
        }
    }
    fn raw_type(self) -> &'static str {
        match self {
            ReportKind::Tornado => "tornado",
            ReportKind::Hail => "hail",
            ReportKind::Wind => "wind",
        }
    }
    fn label(self) -> &'static str {
        match self {
            ReportKind::Tornado => "Tornado",
            ReportKind::Hail => "Hail",
            ReportKind::Wind => "Wind",
        }
    }
}

/// Tornado severity ladders off the (E)F rating when one has been assessed
/// (EF0 → 0.6, EF5 → 1.0); a confirmed but unrated/preliminary tornado ("UNK")
/// still plots high (0.85) — a touchdown is a touchdown.
fn tornado_severity(fscale: Option<i64>) -> f64 {
    match fscale {
        Some(n) => (0.6 + 0.08 * (n.clamp(0, 5) as f64)).clamp(0.6, 1.0),
        None => 0.85,
    }
}

/// Hail severity by diameter (inches): severe at 1.0", significant at 2.0",
/// destructive at 3"+.
fn hail_severity(inches: f64) -> f64 {
    if inches >= 3.0 {
        0.95
    } else if inches >= 2.5 {
        0.85
    } else if inches >= 1.75 {
        0.7
    } else if inches >= 1.0 {
        0.45
    } else {
        0.3
    }
}

/// Wind severity by gust (mph): severe at 58 mph, significant at 75 mph, extreme
/// at 90+. An estimated/unknown-speed damaging-wind report still plots (0.5).
fn wind_severity(mph: Option<f64>) -> f64 {
    match mph {
        Some(s) if s >= 90.0 => 0.95,
        Some(s) if s >= 75.0 => 0.75,
        Some(s) if s >= 65.0 => 0.55,
        Some(s) if s >= 58.0 => 0.4,
        Some(_) => 0.3,
        None => 0.5,
    }
}

/// SPC daily hail `Size` is conventionally given in **hundredths of an inch** as an
/// integer (e.g. `100` = 1.00", `275` = 2.75"). Some mirrors instead carry a decimal
/// inch value (e.g. `1.75`). Disambiguate by magnitude: no real hail approaches 8",
/// so any value above 8 is hundredths and is scaled down; anything ≤ 8 is already
/// inches. Total over either encoding.
fn hail_inches(raw: f64) -> f64 {
    if raw > 8.0 {
        raw / 100.0
    } else {
        raw
    }
}

/// Operator chip for a report, derived from the flat `raw` payload this connector
/// stores: e.g. "EF2 Tornado", "Tornado", "2.75 in hail", "70 mph wind",
/// "Damaging wind".
pub fn report_chip(raw: &Value) -> Option<String> {
    match raw.get("type").and_then(Value::as_str)? {
        "tornado" => Some(match raw.get("fscale").and_then(Value::as_i64) {
            Some(n) => format!("EF{n} Tornado"),
            None => "Tornado".to_string(),
        }),
        "hail" => raw
            .get("size_in")
            .and_then(Value::as_f64)
            .map(|d| format!("{d:.2} in hail")),
        "wind" => Some(match raw.get("speed_mph").and_then(Value::as_f64) {
            Some(s) => format!("{s:.0} mph wind"),
            None => "Damaging wind".to_string(),
        }),
        _ => None,
    }
}

/// Trim a CSV field and return `None` for an empty one.
fn field(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}

/// Parse one report CSV section into events, appending to `out`. Sets `any_header`
/// when the section carried a recognizable header line (so a header-only file is a
/// valid "no reports" section, while a non-CSV body — e.g. an HTML error page — is
/// not and leaves `any_header` untouched).
fn parse_section(csv: &str, kind: ReportKind, out: &mut Vec<Event>, any_header: &mut bool) {
    let trimmed = csv.trim();
    if trimmed.is_empty() {
        return; // section absent this fetch — neither data nor malformed
    }
    let mut lines = trimmed.lines().filter(|l| !l.trim().is_empty());
    // First non-empty line must be the column header ("Time,...").
    let Some(header) = lines.next() else { return };
    if !header.trim_start().to_ascii_lowercase().starts_with("time") {
        return; // not a report CSV (didn't get the feed) — don't treat rows as data
    }
    *any_header = true;

    for line in lines {
        // Comments can contain commas, so keep the first seven fields and let the
        // eighth absorb the remainder.
        let parts: Vec<&str> = line.splitn(8, ',').collect();
        if parts.len() < 7 {
            continue; // too few columns to carry a geocoded report
        }
        let time = field(parts.first().copied()).unwrap_or("");
        let mag = field(parts.get(1).copied());
        let location = field(parts.get(2).copied()).unwrap_or("");
        let county = field(parts.get(3).copied()).unwrap_or("");
        let state = field(parts.get(4).copied()).unwrap_or("");
        let comments = field(parts.get(7).copied()).unwrap_or("");

        let (Some(lat), Some(lon)) = (
            parts.get(5).and_then(|s| s.trim().parse::<f64>().ok()),
            parts.get(6).and_then(|s| s.trim().parse::<f64>().ok()),
        ) else {
            continue;
        };
        let Some(geo) = Geo::new(lat, lon) else { continue };

        // Per-type magnitude + severity + the values surfaced on the chip.
        let mut raw = serde_json::Map::new();
        raw.insert("type".into(), json!(kind.raw_type()));
        let severity = match kind {
            ReportKind::Tornado => {
                let fscale = mag.and_then(|m| m.parse::<i64>().ok());
                if let Some(n) = fscale {
                    raw.insert("fscale".into(), json!(n));
                }
                tornado_severity(fscale)
            }
            ReportKind::Hail => {
                let Some(inches) = mag.and_then(|m| m.parse::<f64>().ok()).map(hail_inches) else {
                    continue; // a hail report with no parseable size carries no signal
                };
                raw.insert("size_in".into(), json!(inches));
                hail_severity(inches)
            }
            ReportKind::Wind => {
                let mph = mag.and_then(|m| m.parse::<f64>().ok());
                if let Some(s) = mph {
                    raw.insert("speed_mph".into(), json!(s));
                }
                wind_severity(mph)
            }
        };

        raw.insert("location".into(), json!(location));
        raw.insert("county".into(), json!(county));
        raw.insert("state".into(), json!(state));
        raw.insert("time".into(), json!(time));
        raw.insert("comments".into(), json!(comments));

        let title = if !location.is_empty() && !state.is_empty() {
            format!("{location}, {state}")
        } else if !location.is_empty() {
            location.to_string()
        } else {
            format!("{} report", kind.label())
        };

        out.push(Event {
            id: format!("spc-{}-{}-{:.3}-{:.3}", kind.tag(), time, lat, lon),
            source_id: "spc_storm_reports".to_string(),
            kind: EventKind::Weather,
            title,
            time: Utc::now(),
            geo: Some(geo),
            severity: Severity::new(severity),
            url: Some("https://www.spc.noaa.gov/climo/reports/today.html".to_string()),
            raw: Value::Object(raw),
        });
    }
}

/// Pure parser: the three SPC report CSVs (tornado / hail / wind) -> events.
/// Unit-tested offline. A header-only file is a valid empty section (no reports),
/// so a quiet day is Ok/empty; if **none** of the three inputs is a recognizable
/// report CSV (e.g. all three are HTML error pages), that's malformed -> Err.
pub fn parse_spc_reports(torn: &str, hail: &str, wind: &str) -> anyhow::Result<Vec<Event>> {
    let mut out = Vec::new();
    let mut any_header = false;
    parse_section(torn, ReportKind::Tornado, &mut out, &mut any_header);
    parse_section(hail, ReportKind::Hail, &mut out, &mut any_header);
    parse_section(wind, ReportKind::Wind, &mut out, &mut any_header);
    if !any_header {
        anyhow::bail!("spc_storm_reports: no recognizable report CSV header in any section");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Built to the confirmed SPC daily report layout: a header line then
    // `Time,<mag>,Location,County,State,Lat,Lon,Comments` rows. The 2nd tornado row
    // has commas inside its Comments to exercise the splitn(8) handling; the hail
    // section mixes the hundredths-inch encoding (275 = 2.75") with a quarter (1.00");
    // the wind section has a measured gust and an unknown-speed report.
    const TORN: &str = "Time,F_Scale,Location,County,State,Lat,Lon,Comments\n\
        1853,UNK,3 SSW MEDFORD,GRANT,OK,36.76,-97.75,BRIEF TORNADO REPORTED BY CHASER. (OUN)\n\
        2014,2,5 ENE HENNESSEY,KINGFISHER,OK,36.13,-97.80,ROOF DAMAGE, OUTBUILDINGS, AND TREES. (OUN)\n";
    const HAIL: &str = "Time,Size,Location,County,State,Lat,Lon,Comments\n\
        1920,100,2 W ENID,GARFIELD,OK,36.40,-97.92,QUARTER SIZE HAIL. (OUN)\n\
        2002,275,4 N KINGFISHER,KINGFISHER,OK,35.92,-97.93,BASEBALL SIZE HAIL. (OUN)\n";
    const WIND: &str = "Time,Speed,Location,County,State,Lat,Lon,Comments\n\
        1944,70,1 S OKARCHE,CANADIAN,OK,35.71,-97.97,MEASURED 70 MPH WIND GUST. (OUN)\n\
        2100,UNK,3 E YUKON,CANADIAN,OK,35.51,-97.70,NUMEROUS TREES DOWN. (OUN)\n";

    #[test]
    fn parses_all_three_sections() {
        let ev = parse_spc_reports(TORN, HAIL, WIND).unwrap();
        // 2 tornado + 2 hail + 2 wind.
        assert_eq!(ev.len(), 6);

        // --- Tornado: unrated touchdown then EF2 ---
        let t0 = &ev[0];
        assert_eq!(t0.source_id, "spc_storm_reports");
        assert_eq!(t0.kind, EventKind::Weather);
        assert_eq!(t0.title, "3 SSW MEDFORD, OK");
        assert!((t0.severity.value() - 0.85).abs() < 1e-9); // UNK -> confirmed-but-unrated
        assert_eq!(report_chip(&t0.raw).as_deref(), Some("Tornado"));
        let g = t0.geo.unwrap();
        assert!((g.lat - 36.76).abs() < 1e-6 && (g.lon + 97.75).abs() < 1e-6);
        assert_eq!(t0.id, "spc-torn-1853-36.760--97.750");

        let t1 = &ev[1];
        // EF2 -> 0.6 + 0.16 = 0.76; comma-laden comment didn't break the columns.
        assert!((t1.severity.value() - 0.76).abs() < 1e-9);
        assert_eq!(report_chip(&t1.raw).as_deref(), Some("EF2 Tornado"));
        assert_eq!(
            t1.raw.get("comments").and_then(Value::as_str),
            Some("ROOF DAMAGE, OUTBUILDINGS, AND TREES. (OUN)")
        );

        // --- Hail: 1.00" quarter then 2.75" baseball (hundredths encoding) ---
        let h0 = &ev[2];
        assert_eq!(report_chip(&h0.raw).as_deref(), Some("1.00 in hail"));
        assert!((h0.severity.value() - 0.45).abs() < 1e-9); // 1.0" severe
        let h1 = &ev[3];
        assert_eq!(report_chip(&h1.raw).as_deref(), Some("2.75 in hail"));
        assert!((h1.severity.value() - 0.85).abs() < 1e-9); // 2.75" -> significant tier

        // --- Wind: measured 70 mph then unknown-speed ---
        let w0 = &ev[4];
        assert_eq!(report_chip(&w0.raw).as_deref(), Some("70 mph wind"));
        assert!((w0.severity.value() - 0.55).abs() < 1e-9); // 70 mph
        let w1 = &ev[5];
        assert_eq!(report_chip(&w1.raw).as_deref(), Some("Damaging wind"));
        assert!((w1.severity.value() - 0.5).abs() < 1e-9); // unknown speed
    }

    #[test]
    fn empty_report_day_is_ok_not_error() {
        // Header-only files (a quiet day / early in the UTC day) -> zero events, Ok.
        let h = "Time,F_Scale,Location,County,State,Lat,Lon,Comments\n";
        assert!(parse_spc_reports(h, h, h).unwrap().is_empty());
    }

    #[test]
    fn errors_when_no_section_is_a_report_csv() {
        // All three came back as something other than a report CSV (e.g. an HTML 403
        // page) -> malformed, surfaced as an error so last-good can take over.
        let html = "<html><body>403 Forbidden</body></html>";
        assert!(parse_spc_reports(html, html, html).is_err());
        // But one good section among bad ones still parses (no spurious error).
        let ev = parse_spc_reports(html, HAIL, html).unwrap();
        assert_eq!(ev.len(), 2);
    }

    #[test]
    fn hail_size_handles_both_encodings_and_drops_unsized() {
        // Hundredths (175 = 1.75") and decimal inches (1.75) yield the same reading.
        let hundredths = "Time,Size,Location,County,State,Lat,Lon,Comments\n\
            1900,175,A,B,KS,38.0,-97.0,c\n";
        let decimal = "Time,Size,Location,County,State,Lat,Lon,Comments\n\
            1900,1.75,A,B,KS,38.0,-97.0,c\n";
        let empty = "Time,Size,Location,County,State,Lat,Lon,Comments\n";
        let a = parse_spc_reports(empty, hundredths, empty).unwrap();
        let b = parse_spc_reports(empty, decimal, empty).unwrap();
        assert_eq!(report_chip(&a[0].raw).as_deref(), Some("1.75 in hail"));
        assert_eq!(report_chip(&b[0].raw).as_deref(), Some("1.75 in hail"));

        // A hail row with no parseable size carries no signal -> dropped.
        let no_size = "Time,Size,Location,County,State,Lat,Lon,Comments\n\
            1900,,A,B,KS,38.0,-97.0,c\n";
        assert!(parse_spc_reports(empty, no_size, empty).unwrap().is_empty());
    }

    #[test]
    fn drops_rows_with_bad_coordinates() {
        // Unparseable / out-of-range coords -> the row is dropped, not plotted at 0,0.
        let bad = "Time,Speed,Location,County,State,Lat,Lon,Comments\n\
            1900,60,A,B,KS,NA,-97.0,c\n\
            1901,60,A,B,KS,99.9,-97.0,c\n\
            1902,60,A,B,KS,38.0,-97.0,c\n";
        let empty = "Time,F_Scale,Location,County,State,Lat,Lon,Comments\n";
        let ev = parse_spc_reports(empty, empty, bad).unwrap();
        assert_eq!(ev.len(), 1); // only the well-formed third row survives
        assert!((ev[0].geo.unwrap().lat - 38.0).abs() < 1e-9);
    }

    #[test]
    fn severity_ladders_by_type_and_magnitude() {
        assert!((tornado_severity(None) - 0.85).abs() < 1e-9);
        assert!((tornado_severity(Some(0)) - 0.6).abs() < 1e-9);
        assert!((tornado_severity(Some(5)) - 1.0).abs() < 1e-9);
        assert!((hail_severity(0.75) - 0.3).abs() < 1e-9);
        assert!((hail_severity(1.0) - 0.45).abs() < 1e-9);
        assert!((hail_severity(3.0) - 0.95).abs() < 1e-9);
        assert!((wind_severity(Some(50.0)) - 0.3).abs() < 1e-9);
        assert!((wind_severity(Some(58.0)) - 0.4).abs() < 1e-9);
        assert!((wind_severity(Some(95.0)) - 0.95).abs() < 1e-9);
        assert!((wind_severity(None) - 0.5).abs() < 1e-9);
        // hundredths-inch normalization
        assert!((hail_inches(275.0) - 2.75).abs() < 1e-9);
        assert!((hail_inches(2.75) - 2.75).abs() < 1e-9);
    }
}
