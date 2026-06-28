//! ACLED Aggregated conflict data — weekly event & fatality counts by Admin 1.
//!
//! ACLED (Armed Conflict Location & Event Data) is the standard project for
//! near-real-time political-violence and demonstration data. ACLED's full
//! event-level API is licensed (the live `acled` connector ships dormant), but
//! ACLED publishes a genuinely **free, no-key Aggregated Data product**: weekly
//! counts of events, fatalities and population exposure rolled up to the first
//! sub-national administrative unit (Admin 1), each row already carrying the
//! Admin 1 **centroid latitude/longitude**. That makes it directly mappable
//! without any external centroid table.
//!
//! This is a distinct modality from [`super::ucdp_ged`] (which plots *discrete*
//! georeferenced events): here each dot is one Admin 1 region coloured by its
//! **recent conflict intensity** — the trailing-window sum of events and
//! fatalities — a regional heat read across ACLED's broad taxonomy (political
//! violence, explosions/remote violence, demonstrations, strategic developments).
//! Fills [`EventKind::Conflict`] with weekly cadence and ACLED breadth UCDP's
//! monthly candidate-GED doesn't carry.
//!
//! ## Ingestion — Path B (committed snapshot)
//! ACLED's aggregated files are downloaded from `acleddata.com` (also mirrored on
//! HDX); the host is unreachable from the cloud build sandbox (403) and the
//! download is a manual/registered step, so the data ships as a **committed
//! snapshot** ([`SNAPSHOT`], `include_str!`-embedded) refreshed by a local job
//! that re-downloads the official aggregated regional file(s) and re-commits the
//! CSV. The wire schema is real — the canonical 13-column aggregated layout
//! (`WEEK,REGION,COUNTRY,ADMIN1,EVENT_TYPE,SUB_EVENT_TYPE,EVENTS,FATALITIES,`
//! `POPULATION_EXPOSURE,DISORDER_TYPE,ID,CENTROID_LATITUDE,CENTROID_LONGITUDE`),
//! confirmed against many independent public copies — and the shipped seed is
//! built from real ACLED Middle-East weekly aggregate values (Jan–Mar 2026), so
//! the parser is exercised against genuine bytes, not documentation guesswork.

use async_trait::async_trait;
use chrono::{Duration as ChronoDuration, NaiveDate, TimeZone, Utc};
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use std::collections::BTreeMap;
use std::time::Duration;

/// Real committed ACLED aggregated-weekly snapshot (see module docs for refresh).
pub const SNAPSHOT: &str = include_str!("acled_aggregated_snapshot.csv");

/// Trailing window (days) ending at the file's most recent `WEEK`. Only rows
/// inside it are summed, so a multi-year aggregated file plots *current* regional
/// intensity rather than years of stacked history. ~4 inclusive weeks.
const WINDOW_DAYS: i64 = 27;

/// ACLED Aggregated weekly conflict source (Path-B committed snapshot).
#[derive(Default)]
pub struct AcledAggregated;

#[async_trait]
impl Source for AcledAggregated {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "acled_aggregated",
            name: "ACLED Aggregated Conflict (weekly, Admin 1)",
            domain: EventKind::Conflict,
            // Refreshed weekly upstream; the snapshot is re-committed by a local job.
            cadence: Duration::from_secs(21_600),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        // Path B: the snapshot is committed with the connector; the origin host is
        // not reachable for an automated live pull. A local job refreshes the file.
        parse_acled_aggregated(SNAPSHOT)
    }
}

/// RFC4180-style CSV reader: quote-aware (Admin 1 / actor names can carry commas,
/// embedded `""`, and newlines inside quoted fields).
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

/// Parse `WEEK` (the aggregated file uses an ISO `YYYY-MM-DD` week-start date).
fn parse_week(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok()
}

/// Slug for a stable feature id (lowercase alphanumerics, single dashes).
fn slug(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.extend(c.to_lowercase());
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    out.trim_end_matches('-').to_string()
}

/// Per-Admin 1 accumulator over the trailing window.
#[derive(Default)]
struct Agg {
    events: f64,
    fatalities: f64,
    lat: f64,
    lon: f64,
    latest: Option<NaiveDate>,
    /// sub-event-type / disorder label → events seen, to pick the dominant label.
    labels: BTreeMap<String, f64>,
}

/// Operator chip behind an Admin 1 conflict dot: the trailing-window intensity,
/// e.g. "41 events · 66 fatalities · Air/drone strike". Counts and fatalities are
/// inherently unit-bearing conflict measures; the dominant ACLED label names what
/// drove them. `None` only if the record somehow carries no events.
pub fn intensity_chip(raw: &serde_json::Value) -> Option<String> {
    let events = raw.get("events").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
    if events <= 0.0 {
        return None;
    }
    let fatalities = raw.get("fatalities").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
    let label = raw.get("label").and_then(serde_json::Value::as_str).filter(|s| !s.is_empty());
    let mut chip = format!("{events:.0} events");
    if fatalities > 0.0 {
        chip.push_str(&format!(" · {fatalities:.0} fatalities"));
    }
    if let Some(l) = label {
        chip.push_str(&format!(" · {l}"));
    }
    Some(chip)
}

/// Pure parser: ACLED aggregated-weekly CSV -> one [`EventKind::Conflict`] event
/// per Admin 1, summed over the trailing [`WINDOW_DAYS`] window ending at the
/// file's most recent `WEEK`. Offline-tested. A header missing the required
/// columns is an error; rows outside the window or without a usable centroid are
/// dropped, so an aggregated file whose latest weeks are all-quiet is Ok/empty.
pub fn parse_acled_aggregated(csv: &str) -> anyhow::Result<Vec<Event>> {
    let rows = parse_csv(csv);
    let header = rows.first().ok_or_else(|| anyhow::anyhow!("acled_aggregated: empty CSV"))?;
    // Case-insensitive header lookup (ACLED ships ALL-CAPS column names).
    let col = |name: &str| {
        header
            .iter()
            .position(|h| h.trim().eq_ignore_ascii_case(name))
    };
    let (Some(i_week), Some(i_admin1), Some(i_events), Some(i_fat)) =
        (col("WEEK"), col("ADMIN1"), col("EVENTS"), col("FATALITIES"))
    else {
        return Err(anyhow::anyhow!("acled_aggregated: missing expected columns"));
    };
    let (Some(i_lat), Some(i_lon)) = (
        col("CENTROID_LATITUDE").or_else(|| col("LATITUDE")),
        col("CENTROID_LONGITUDE").or_else(|| col("LONGITUDE")),
    ) else {
        return Err(anyhow::anyhow!("acled_aggregated: missing centroid coordinates"));
    };
    let (i_country, i_sub, i_disorder) = (col("COUNTRY"), col("SUB_EVENT_TYPE"), col("DISORDER_TYPE"));

    let get = |r: &[String], idx: Option<usize>| -> String {
        idx.and_then(|i| r.get(i)).cloned().unwrap_or_default()
    };
    let num = |s: &str| s.trim().parse::<f64>().ok();

    // First pass: parse valid rows and find the most recent week.
    struct Row {
        week: NaiveDate,
        country: String,
        admin1: String,
        events: f64,
        fatalities: f64,
        lat: f64,
        lon: f64,
        label: String,
    }
    let mut parsed: Vec<Row> = Vec::new();
    let mut max_week: Option<NaiveDate> = None;
    for r in rows.iter().skip(1) {
        let Some(week) = r.get(i_week).and_then(|s| parse_week(s)) else { continue };
        let admin1 = r.get(i_admin1).cloned().unwrap_or_default().trim().to_string();
        if admin1.is_empty() {
            continue;
        }
        let (Some(lat), Some(lon)) = (
            r.get(i_lat).and_then(|s| num(s)),
            r.get(i_lon).and_then(|s| num(s)),
        ) else {
            continue;
        };
        if Geo::new(lat, lon).is_none() {
            continue;
        }
        let events = r.get(i_events).and_then(|s| num(s)).unwrap_or(0.0);
        let fatalities = r.get(i_fat).and_then(|s| num(s)).unwrap_or(0.0);
        // Prefer the specific sub-event type; fall back to the disorder category.
        let label = {
            let sub = get(r, i_sub);
            if sub.trim().is_empty() { get(r, i_disorder) } else { sub }
        }
        .trim()
        .to_string();
        max_week = Some(max_week.map_or(week, |m| m.max(week)));
        parsed.push(Row {
            week,
            country: get(r, i_country).trim().to_string(),
            admin1,
            events,
            fatalities,
            lat,
            lon,
            label,
        });
    }

    let Some(max_week) = max_week else { return Ok(Vec::new()) };
    let window_start = max_week - ChronoDuration::days(WINDOW_DAYS);

    // Second pass: aggregate the in-window rows per (country, Admin 1).
    let mut groups: BTreeMap<(String, String), Agg> = BTreeMap::new();
    for row in parsed.into_iter().filter(|r| r.week >= window_start) {
        let g = groups.entry((row.country.clone(), row.admin1.clone())).or_default();
        g.events += row.events;
        g.fatalities += row.fatalities;
        g.lat = row.lat;
        g.lon = row.lon;
        g.latest = Some(g.latest.map_or(row.week, |w| w.max(row.week)));
        if !row.label.is_empty() {
            *g.labels.entry(row.label).or_default() += row.events.max(1.0);
        }
    }

    let mut out = Vec::with_capacity(groups.len());
    for ((country, admin1), g) in groups {
        if g.events <= 0.0 && g.fatalities <= 0.0 {
            continue;
        }
        let Some(geo) = Geo::new(g.lat, g.lon) else { continue };
        let week = g.latest.unwrap_or(max_week);
        let time = week
            .and_hms_opt(0, 0, 0)
            .map(|dt| Utc.from_utc_datetime(&dt))
            .unwrap_or_else(Utc::now);

        // Dominant ACLED label (the one with the most events) names the driver.
        let label = g
            .labels
            .iter()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(k, _)| k.clone())
            .unwrap_or_default();

        let place = if country.is_empty() {
            admin1.clone()
        } else {
            format!("{admin1}, {country}")
        };
        let title = if g.fatalities >= 1.0 {
            format!("{place} — {:.0} killed ({:.0} events)", g.fatalities, g.events)
        } else {
            format!("{place} — {:.0} conflict events", g.events)
        };

        // Log-scaled by fatalities (same ladder as UCDP): ~1 death faint, ~1000+
        // saturates; an active-but-no-deaths region still gets a small floor.
        let severity = ((1.0 + g.fatalities).ln() / 7.0).clamp(0.12, 1.0);

        out.push(Event {
            id: format!("acled-agg-{}", slug(&place)),
            source_id: "acled_aggregated".to_string(),
            kind: EventKind::Conflict,
            title,
            time,
            geo: Some(geo),
            severity: Severity::new(severity),
            url: Some(
                "https://acleddata.com/conflict-data/download-data-files/aggregated-data".to_string(),
            ),
            raw: serde_json::json!({
                "country": country,
                "admin1": admin1,
                "events": g.events,
                "fatalities": g.fatalities,
                "label": label,
                "week": week.format("%Y-%m-%d").to_string(),
            }),
        });
    }

    // Deadliest first, so a downstream cap keeps the hottest regions.
    out.sort_by(|a, b| {
        b.severity
            .value()
            .partial_cmp(&a.severity.value())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.time.cmp(&a.time))
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Canonical 13-column aggregated layout. Region "Hot" has three weeks — two
    // inside the 4-week window ending at the file's max week (2026-03-07) and one
    // old (2026-01-03) that must be excluded. Region "Quiet" has events but zero
    // fatalities (severity floor). An out-of-range-centroid row must be dropped.
    const FIXTURE: &str = "WEEK,REGION,COUNTRY,ADMIN1,EVENT_TYPE,SUB_EVENT_TYPE,EVENTS,FATALITIES,POPULATION_EXPOSURE,DISORDER_TYPE,ID,CENTROID_LATITUDE,CENTROID_LONGITUDE\n\
2026-01-03,Middle East,Iran,Hot,Explosions/Remote violence,Air/drone strike,99,500,,Political violence,1,30.0,52.0\n\
2026-02-28,Middle East,Iran,Hot,Explosions/Remote violence,Air/drone strike,16,42,,Political violence,1,30.0,52.0\n\
2026-03-07,Middle East,Iran,Hot,Battles,Armed clash,25,24,,Political violence,1,30.0,52.0\n\
2026-03-07,Middle East,Iran,Quiet,Protests,Peaceful protest,7,0,,Demonstrations,2,35.0,51.0\n\
2026-03-07,Middle East,Nowhere,Bad,Battles,Armed clash,3,9,,Political violence,3,999.0,0.0\n";

    #[test]
    fn windows_and_aggregates() {
        let ev = parse_acled_aggregated(FIXTURE).unwrap();
        // Two plottable regions (bad-centroid dropped); deadliest (Hot) first.
        assert_eq!(ev.len(), 2);

        let hot = ev.iter().find(|e| e.id == "acled-agg-hot-iran").unwrap();
        assert_eq!(hot.kind, EventKind::Conflict);
        // Old 2026-01-03 week excluded: events 16+25=41, fatalities 42+24=66
        // (NOT 99/500 from the out-of-window January row). Dominant label is the
        // one with the most events across the window: Armed clash (25) > Air/drone
        // strike (16).
        assert_eq!(
            intensity_chip(&hot.raw).as_deref(),
            Some("41 events · 66 fatalities · Armed clash")
        );
        assert_eq!(hot.title, "Hot, Iran — 66 killed (41 events)");
        // Time is the latest in-window week.
        assert_eq!(hot.time.format("%Y-%m-%d").to_string(), "2026-03-07");
        let g = hot.geo.unwrap();
        assert!((g.lat - 30.0).abs() < 1e-9 && (g.lon - 52.0).abs() < 1e-9);
        // Severity from 66 fatalities, well above the floor and below saturation.
        assert!(hot.severity.value() > 0.12 && hot.severity.value() < 1.0);
        assert!(hot.severity.value() > 0.55); // ln(67)/7 ≈ 0.60
    }

    #[test]
    fn zero_fatality_region_floors_and_omits_fatalities() {
        let ev = parse_acled_aggregated(FIXTURE).unwrap();
        let quiet = ev.iter().find(|e| e.id == "acled-agg-quiet-iran").unwrap();
        // 7 events, 0 fatalities -> severity floor, no "killed" in the title/chip.
        assert!((quiet.severity.value() - 0.12).abs() < 1e-9);
        assert_eq!(quiet.title, "Quiet, Iran — 7 conflict events");
        assert_eq!(
            intensity_chip(&quiet.raw).as_deref(),
            Some("7 events · Peaceful protest")
        );
    }

    #[test]
    fn errors_on_bad_input() {
        // Not CSV with the required columns.
        assert!(parse_acled_aggregated("foo,bar\n1,2\n").is_err());
        // Header present but no data rows -> empty, not an error.
        let header = "WEEK,COUNTRY,ADMIN1,EVENTS,FATALITIES,CENTROID_LATITUDE,CENTROID_LONGITUDE\n";
        assert!(parse_acled_aggregated(header).unwrap().is_empty());
    }

    #[test]
    fn committed_snapshot_parses_to_conflict_events() {
        // The shipped real ACLED Middle-East aggregate must parse into Conflict
        // dots, all geocoded, all within the trailing window of the latest week.
        let ev = parse_acled_aggregated(SNAPSHOT).unwrap();
        assert!(ev.len() >= 30, "snapshot should carry many Admin 1 regions, got {}", ev.len());
        assert!(ev.iter().all(|e| e.kind == EventKind::Conflict && e.geo.is_some()));
        assert!(ev.iter().all(|e| e.severity.value() >= 0.12));
        // Windowing held: nothing older than ~4 weeks before the latest week.
        let latest = ev.iter().map(|e| e.time).max().unwrap();
        let earliest = ev.iter().map(|e| e.time).min().unwrap();
        assert!((latest - earliest) <= ChronoDuration::days(WINDOW_DAYS));
        // Every dot carries a meaningful intensity chip.
        assert!(ev.iter().all(|e| intensity_chip(&e.raw).is_some()));
    }
}
