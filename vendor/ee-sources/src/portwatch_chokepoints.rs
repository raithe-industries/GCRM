//! IMF PortWatch — daily maritime **chokepoint transit disruption**.
//!
//! PortWatch (IMF, in partnership with the University of Oxford) estimates daily
//! ship transits through the world's 28 strategic maritime chokepoints — the
//! Strait of Hormuz, the Taiwan Strait, Bab-el-Mandeb, the Malacca Strait, the
//! Suez and Panama Canals, and so on — from satellite AIS on ~90,000 vessels.
//! A sustained collapse in transits through Hormuz or the Taiwan Strait is a
//! first-order WWIII-risk observable (blockade, mining, closure, war disruption),
//! and a surge at an alternate (e.g. the Cape of Good Hope) corroborates a
//! disruption elsewhere. This is the maritime **chokepoint** modality no feed
//! carried, and it lands in the [`EventKind::Vessel`] layer that was otherwise
//! Baltic-only (`digitraffic_ais`), extending it to the Asian/Middle-East theaters.
//!
//! ## Signal-meaningfulness — a raw transit count becomes a baseline-relative anomaly
//! A bare "42 ships crossed Hormuz today" is a nonsense number (the ECCC-hydrometric
//! trap). PortWatch ships only raw daily counts, so this connector computes the
//! meaning itself, the endorsed level→anomaly route: for each chokepoint it takes
//! the mean of the most recent [`RECENT_DAYS`] as the **current** rate and the
//! **median** of the older days in the window as that chokepoint's own **transit
//! norm**, then plots the deviation. Only chokepoints whose transit is
//! **abnormally low** (a closure/blockade signal) or abnormally high (rerouting
//! surge) plot; a chokepoint flowing normally drops, so an all-normal world is 0
//! events, not an error (the `nwps_flood`/`odlinfo` drop-the-all-clear pattern).
//! Every plotted value carries direction + magnitude + the baseline + raw units
//! (transits/day): "Transit down 62% vs norm (15 vs 40/day)".
//!
//! ## Ingestion — Path A (live ArcGIS FeatureServer)
//! Prod fetches the public IMF PortWatch hosted feature service
//! (`services9.arcgis.com/.../Daily_Chokepoints_Data/FeatureServer/0/query`),
//! auth-free — the 2000 most-recent daily rows (`orderByFields=date DESC`), enough
//! to establish each chokepoint's ~4–8-week norm in a single fetch. The host 403s
//! web fetch in-sandbox (the standing egress wall), so the endpoint + field schema
//! are anchored to committed GitHub bytes: the World Bank `alternative-data-for-
//! crisis` chokepoints-monitor notebook (the exact FeatureServer URL, and the
//! attribute fields `date`/`portid`/`portname`/`n_total` + Point geometry) and the
//! `amanid/imf-portwatch-analytics` client (the same auth-free ArcGIS access,
//! 7-day-moving-average disruption method vs. the historical trend). The
//! "% of the 1-year average" closure threshold matches the community
//! `montanaflynn/ishormuzopenyet` monitor. Attribution: "IMF PortWatch".

use async_trait::async_trait;
use chrono::{DateTime, Duration as ChronoDuration, TimeZone, Utc};
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Duration;

/// Most-recent days treated as the "current" transit rate (a 7-day moving average,
/// matching PortWatch's own smoothing + the Polymarket/ishormuzopenyet convention).
const RECENT_DAYS: i64 = 7;
/// Minimum older-than-recent days required to compute a stable norm; a chokepoint
/// with less history in the window is skipped (can't be judged, so not plotted).
const MIN_BASELINE_DAYS: usize = 14;
/// Chokepoints whose norm is below this many transits/day are too low-volume for a
/// meaningful percentage deviation (division noise), so they are skipped.
const MIN_BASELINE_TRANSIT: f64 = 3.0;
/// Deviation thresholds: a DROP (closure/blockade — the alarm) plots from −25%; a
/// SURGE (rerouting influx — corroborating) plots from +40%, at lower severity.
const DROP_THRESHOLD: f64 = -0.25;
const SURGE_THRESHOLD: f64 = 0.40;

/// IMF PortWatch chokepoint-transit source (Path-A live ArcGIS feature service).
#[derive(Default)]
pub struct PortwatchChokepoints;

impl PortwatchChokepoints {
    /// The 2000 most-recent daily chokepoint rows (all 28 chokepoints, `date DESC`)
    /// with geometry — enough to derive each chokepoint's recent transit norm in a
    /// single fetch. Auth-free public hosted feature service.
    pub fn url(&self) -> &'static str {
        "https://services9.arcgis.com/weJ1QsnbMYJlCHdG/arcgis/rest/services/\
         Daily_Chokepoints_Data/FeatureServer/0/query\
         ?where=1%3D1&outFields=date,portid,portname,n_total\
         &orderByFields=date+DESC&resultRecordCount=2000&returnGeometry=true&outSR=4326&f=json"
    }
}

#[async_trait]
impl Source for PortwatchChokepoints {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "portwatch_chokepoints",
            name: "IMF PortWatch chokepoint transit",
            domain: EventKind::Vessel,
            // Upstream refreshes weekly (Tuesdays, ~4-day lag); 6h keeps the map fresh
            // without hammering the service.
            cadence: Duration::from_secs(21_600),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        // Page ~a YEAR of daily rows (the server caps each response at 1000 rows ≈ 36
        // days across the 28 chokepoints). The norm must span normal months: a short
        // window's median is whatever the recent weeks were, and during the 2026
        // Hormuz closure that baseline was the COLLAPSED state (7/day), which made
        // partial recovery read as a +300% "surge" while the strait still ran at HALF
        // its real norm. Against the year-scale median (64/day) the same day reads
        // "down 50%" — the honest alarm (the ishormuzopenyet "% of 1-year average"
        // convention). Pages fetched concurrently; a short page = end of data.
        let mut features: Vec<Value> = Vec::new();
        for page in 0..10u32 {
            let url = format!("{}&resultOffset={}", self.url(), page * 1000);
            let text = crate::http::fetch_text(&url).await?;
            let root: Value = serde_json::from_str(&text)?;
            if root.get("error").is_some() {
                anyhow::bail!("portwatch: ArcGIS error response (page {page})");
            }
            let Some(arr) = root.get("features").and_then(Value::as_array) else { break };
            let n = arr.len();
            features.extend(arr.iter().cloned());
            if n < 1000 {
                break; // short page = end of the dataset
            }
        }
        let merged = serde_json::json!({ "features": features }).to_string();
        parse_portwatch(&merged)
    }
}

/// Case-insensitive attribute lookup (ArcGIS may echo field names in any case).
fn attr<'a>(obj: &'a Value, key: &str) -> Option<&'a Value> {
    obj.as_object()?
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v)
}

/// A JSON value that may be a number or numeric string -> f64.
fn as_f64_loose(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
}

/// ArcGIS `f=json` encodes date fields as epoch milliseconds; tolerate an ISO string too.
/// The LIVE Daily_Chokepoints_Data service (verified 2026-07-09) actually returns bare
/// `"YYYY-MM-DD"` strings — not the epoch-ms the schema anchor showed — so that form is
/// first-class here: without it every row silently dropped and the connector shipped
/// fetched=0 on day one.
fn parse_arcgis_date(v: &Value) -> Option<DateTime<Utc>> {
    if let Some(ms) = v.as_i64() {
        return Utc.timestamp_millis_opt(ms).single();
    }
    if let Some(f) = v.as_f64() {
        return Utc.timestamp_millis_opt(f as i64).single();
    }
    if let Some(s) = v.as_str() {
        if let Ok(dt) = DateTime::parse_from_rfc3339(s.trim()) {
            return Some(dt.with_timezone(&Utc));
        }
        if let Ok(d) = chrono::NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d") {
            return d.and_hms_opt(0, 0, 0).map(|ndt| Utc.from_utc_datetime(&ndt));
        }
    }
    None
}

/// Fixed locations of the 28 PortWatch chokepoints, keyed by `portid`. The live daily
/// layer returns NO geometry (verified 2026-07-09: `returnGeometry=true` still yields
/// attribute-only features), so rows geolocate through this table. Coordinates pulled
/// from IMF's own `PortWatch_chokepoints_database` FeatureServer (same ArcGIS org,
/// lat/lon attributes) on 2026-07-09 — chokepoints are fixed geography, so a committed
/// table is exact, not an approximation.
fn chokepoint_coords(portid: &str) -> Option<(f64, f64)> {
    match portid {
        "chokepoint1" => Some((30.5933, 32.4369)),    // Suez Canal
        "chokepoint2" => Some((9.1205, -79.7672)),    // Panama Canal
        "chokepoint3" => Some((41.1693, 29.0915)),    // Bosporus Strait
        "chokepoint4" => Some((12.7886, 43.3495)),    // Bab el-Mandeb Strait
        "chokepoint5" => Some((1.5170, 102.6651)),    // Malacca Strait
        "chokepoint6" => Some((26.2969, 56.8598)),    // Strait of Hormuz
        "chokepoint7" => Some((-34.9273, 20.8827)),   // Cape of Good Hope
        "chokepoint8" => Some((35.9423, -5.7549)),    // Gibraltar Strait
        "chokepoint9" => Some((51.0302, 1.5058)),     // Dover Strait
        "chokepoint10" => Some((55.5078, 12.8508)),   // Oresund Strait
        "chokepoint11" => Some((24.7235, 119.8314)),  // Taiwan Strait
        "chokepoint12" => Some((34.1308, 129.2092)),  // Korea Strait
        "chokepoint13" => Some((41.3280, 140.3533)),  // Tsugaru Strait
        "chokepoint14" => Some((20.4889, 121.3523)),  // Luzon Strait
        "chokepoint15" => Some((-8.4191, 115.8014)),  // Lombok Strait
        "chokepoint16" => Some((-8.3985, 125.0910)),  // Ombai Strait
        "chokepoint17" => Some((38.3730, 120.9000)),  // Bohai Strait
        "chokepoint18" => Some((-9.8625, 142.2475)),  // Torres Strait
        "chokepoint19" => Some((-5.9668, 105.7752)),  // Sunda Strait
        "chokepoint20" => Some((0.3523, 119.2571)),   // Makassar Strait
        "chokepoint21" => Some((-52.6403, -69.5948)), // Magellan Strait
        "chokepoint22" => Some((21.8153, -85.6473)),  // Yucatan Channel
        "chokepoint23" => Some((19.9862, -73.6975)),  // Windward Passage
        "chokepoint24" => Some((18.4487, -67.7114)),  // Mona Passage
        "chokepoint25" => Some((7.4136, 117.1146)),   // Balabac Strait
        "chokepoint26" => Some((65.9665, -165.5498)), // Bering Strait
        "chokepoint27" => Some((12.4683, 120.4034)),  // Mindoro Strait
        "chokepoint28" => Some((45.2668, 36.5439)),   // Kerch Strait
        _ => None,
    }
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

/// Median of a slice (sorted copy); `None` if empty.
fn median(vals: &[f64]) -> Option<f64> {
    if vals.is_empty() {
        return None;
    }
    let mut v = vals.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    Some(if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 })
}

/// Severity from the transit deviation. A DROP is the alarm (a closed chokepoint
/// saturates); a SURGE is a lower-severity corroborating signal. Within-normal
/// deviations return `None` and drop.
fn severity_for_deviation(dev: f64) -> Option<f64> {
    if dev <= -0.75 {
        Some(1.0) // effectively closed (< 25% of norm)
    } else if dev <= -0.50 {
        Some(0.85)
    } else if dev <= -0.33 {
        Some(0.65)
    } else if dev <= DROP_THRESHOLD {
        Some(0.50)
    } else if dev >= 0.50 {
        Some(0.40) // strong rerouting surge
    } else if dev >= SURGE_THRESHOLD {
        Some(0.30)
    } else {
        None
    }
}

/// Operator chip behind a chokepoint dot: direction + magnitude vs the chokepoint's
/// own transit norm + the raw daily rates, e.g. "Transit down 62% vs norm
/// (15 vs 40/day)". Baseline-relative + unit-bearing (the signal-meaningfulness bar).
pub fn transit_chip(raw: &Value) -> Option<String> {
    let dev = raw.get("deviation").and_then(Value::as_f64)?;
    let current = raw.get("current").and_then(Value::as_f64)?;
    let baseline = raw.get("baseline").and_then(Value::as_f64)?;
    let dir = if dev < 0.0 { "down" } else { "up" };
    let pct = (dev.abs() * 100.0).round();
    Some(format!("Transit {dir} {pct:.0}% vs norm ({current:.0} vs {baseline:.0}/day)"))
}

/// One chokepoint's rows accumulated from the response.
struct Choke {
    name: String,
    lat: f64,
    lon: f64,
    /// (date, transits) pairs.
    rows: Vec<(DateTime<Utc>, f64)>,
}

/// Pure parser: PortWatch daily-chokepoints ArcGIS JSON -> one [`EventKind::Vessel`]
/// event per chokepoint whose transit deviates significantly from its own recent
/// norm. Offline-tested. A payload with no `features` array (or an ArcGIS `error`
/// object) is an error; normally-flowing / low-history / low-volume chokepoints are
/// dropped, so an all-normal world is Ok/empty.
pub fn parse_portwatch(json: &str) -> anyhow::Result<Vec<Event>> {
    let root: Value = serde_json::from_str(json)?;
    // ArcGIS returns application errors with HTTP 200 and an {"error":{...}} body.
    if root.get("error").is_some() {
        anyhow::bail!("portwatch: ArcGIS error response");
    }
    let features = root
        .get("features")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("portwatch: missing 'features' array"))?;

    let mut groups: BTreeMap<String, Choke> = BTreeMap::new();
    for f in features {
        let Some(a) = f.get("attributes") else { continue };
        // portid may be a string ("chokepoint6") or numeric; normalize to a string key.
        let portid = attr(a, "portid")
            .and_then(|v| {
                v.as_str()
                    .map(str::to_string)
                    .or_else(|| v.as_i64().map(|i| i.to_string()))
            })
            .filter(|s| !s.is_empty());
        let Some(portid) = portid else { continue };
        let Some(date) = attr(a, "date").and_then(parse_arcgis_date) else { continue };
        let Some(n) = attr(a, "n_total").and_then(as_f64_loose) else { continue };
        if n < 0.0 {
            continue;
        }
        let name = attr(a, "portname")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| portid.clone());
        let (lon, lat) = f
            .get("geometry")
            .map(|g| {
                (
                    attr(g, "x").and_then(as_f64_loose),
                    attr(g, "y").and_then(as_f64_loose),
                )
            })
            .unwrap_or((None, None));

        let entry = groups.entry(portid).or_insert_with(|| Choke {
            name: name.clone(),
            lat: f64::NAN,
            lon: f64::NAN,
            rows: Vec::new(),
        });
        // Capture geometry the first time it's present (constant per chokepoint).
        if entry.lat.is_nan() {
            if let (Some(lo), Some(la)) = (lon, lat) {
                entry.lon = lo;
                entry.lat = la;
            }
        }
        entry.rows.push((date, n));
    }

    let mut out = Vec::new();
    for (portid, mut c) in groups {
        // The live daily layer carries no geometry (attribute-only features, verified
        // 2026-07-09) — geolocate through the fixed chokepoint table. Response geometry,
        // when the service does send it, still wins (captured above).
        if c.lat.is_nan() {
            if let Some((la, lo)) = chokepoint_coords(&portid) {
                c.lat = la;
                c.lon = lo;
            }
        }
        let Some(geo) = Geo::new(c.lat, c.lon) else { continue };
        if c.rows.len() < MIN_BASELINE_DAYS + 1 {
            continue;
        }
        // Newest first; drop same-date duplicates (paged offsets can shift between
        // requests when the upstream updates mid-pagination — a duplicated day must
        // not double-weight the recent mean).
        c.rows.sort_by(|a, b| b.0.cmp(&a.0));
        c.rows.dedup_by(|a, b| a.0 == b.0);
        let latest = c.rows[0].0;
        let recent_cut = latest - ChronoDuration::days(RECENT_DAYS);

        let mut recent: Vec<f64> = Vec::new();
        let mut base: Vec<f64> = Vec::new();
        for (d, n) in &c.rows {
            if *d > recent_cut {
                recent.push(*n);
            } else {
                base.push(*n);
            }
        }
        if recent.is_empty() || base.len() < MIN_BASELINE_DAYS {
            continue;
        }
        let current = recent.iter().sum::<f64>() / recent.len() as f64;
        let Some(baseline) = median(&base) else { continue };
        if baseline < MIN_BASELINE_TRANSIT {
            continue;
        }
        let deviation = (current - baseline) / baseline;
        let Some(sev) = severity_for_deviation(deviation) else { continue };

        let dir = if deviation < 0.0 { "down" } else { "up" };
        let pct = (deviation.abs() * 100.0).round();
        let title =
            format!("{} — transit {dir} {pct:.0}% ({current:.0} vs {baseline:.0}/day)", c.name);

        out.push(Event {
            id: format!("portwatch-{}", slug(&portid)),
            source_id: "portwatch_chokepoints".to_string(),
            kind: EventKind::Vessel,
            title,
            time: latest,
            geo: Some(geo),
            severity: Severity::new(sev),
            url: Some("https://portwatch.imf.org/pages/chokepoints".to_string()),
            raw: serde_json::json!({
                "portid": portid,
                "portname": c.name,
                "current": (current * 10.0).round() / 10.0,
                "baseline": (baseline * 10.0).round() / 10.0,
                "deviation": (deviation * 1000.0).round() / 1000.0,
                "baseline_days": base.len(),
                "latest_date": latest.format("%Y-%m-%d").to_string(),
            }),
        });
    }

    // Most-disrupted first, so a downstream cap keeps the hottest chokepoints.
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

    /// Build an ArcGIS `f=json` query response from (portid, name, lon, lat, N daily
    /// rows ending `end`, per-day transit generator). Dates are real epoch-ms so the
    /// parser is exercised against the true wire encoding.
    fn feature_rows(
        portid: &str,
        name: &str,
        lon: f64,
        lat: f64,
        days: i64,
        end: &str,
        n_for_offset: impl Fn(i64) -> f64,
    ) -> Vec<Value> {
        let end = chrono::NaiveDate::parse_from_str(end, "%Y-%m-%d").unwrap();
        (0..days)
            .map(|off| {
                let d = end - ChronoDuration::days(off);
                let ms = Utc
                    .from_utc_datetime(&d.and_hms_opt(0, 0, 0).unwrap())
                    .timestamp_millis();
                serde_json::json!({
                    "attributes": {"date": ms, "portid": portid, "portname": name, "n_total": n_for_offset(off)},
                    "geometry": {"x": lon, "y": lat}
                })
            })
            .collect()
    }

    fn wrap(features: Vec<Value>) -> String {
        serde_json::json!({
            "objectIdFieldName": "FID",
            "features": features
        })
        .to_string()
    }

    #[test]
    fn live_wire_shape_string_dates_no_geometry() {
        // The REAL wire shape served in production (captured 2026-07-09): bare
        // "YYYY-MM-DD" date strings and NO geometry key at all — both DIFFERENT from
        // the epoch-ms + Point-geometry shape the GitHub schema anchor showed. That
        // double mismatch shipped a fetched=0 connector on day one. Rows must parse
        // via the bare-date branch and geolocate via the fixed chokepoint table.
        let end = chrono::NaiveDate::parse_from_str("2026-07-05", "%Y-%m-%d").unwrap();
        let feats: Vec<Value> = (0..36i64)
            .map(|off| {
                let d = end - ChronoDuration::days(off);
                let n = if off < 7 { 15.0 } else { 40.0 }; // recent collapse vs norm
                serde_json::json!({
                    "attributes": {
                        "date": d.format("%Y-%m-%d").to_string(),
                        "portid": "chokepoint6",
                        "portname": "Strait of Hormuz",
                        "n_total": n
                    }
                    // deliberately NO "geometry" key — the live shape
                })
            })
            .collect();
        let ev = parse_portwatch(&wrap(feats)).unwrap();
        assert_eq!(ev.len(), 1, "string-dated, geometry-less rows must still parse");
        let hz = &ev[0];
        assert_eq!(hz.id, "portwatch-chokepoint6");
        let g = hz.geo.as_ref().expect("geo from the fixed chokepoint table");
        assert!(
            (g.lat - 26.2969).abs() < 0.01 && (g.lon - 56.8598).abs() < 0.01,
            "must geolocate to the real Strait of Hormuz: got ({}, {})", g.lat, g.lon
        );
        assert_eq!(hz.time.format("%Y-%m-%d").to_string(), "2026-07-05");
        assert!(hz.severity.value() >= 0.85, "a 62% transit collapse is a loud alarm");
    }

    #[test]
    fn drop_surge_and_normal() {
        let mut feats = Vec::new();
        // Hormuz: ~40/day for 40 baseline days, then ~15/day for the last 7 -> big DROP.
        feats.extend(feature_rows("chokepoint6", "Strait of Hormuz", 56.25, 26.57, 47, "2026-07-01", |off| {
            if off < 7 { 15.0 } else { 40.0 }
        }));
        // Suez: steady ~50/day throughout -> within normal -> NOT plotted.
        feats.extend(feature_rows("chokepoint1", "Suez Canal", 32.35, 30.42, 47, "2026-07-01", |_| 50.0));
        // Cape of Good Hope: ~30/day baseline, ~48/day recent -> SURGE (+60%).
        feats.extend(feature_rows("chokepoint2", "Cape of Good Hope", 20.0, -34.35, 47, "2026-07-01", |off| {
            if off < 7 { 48.0 } else { 30.0 }
        }));
        // Malacca: only 5 days of history -> insufficient baseline -> dropped.
        feats.extend(feature_rows("chokepoint13", "Malacca Strait", 100.6, 2.0, 5, "2026-07-01", |_| 20.0));

        let ev = parse_portwatch(&wrap(feats)).unwrap();
        // Hormuz (drop) + Cape (surge); Suez normal and Malacca low-history dropped.
        assert_eq!(ev.len(), 2);

        // Deadliest first: Hormuz drop dominates.
        let hz = &ev[0];
        assert_eq!(hz.id, "portwatch-chokepoint6");
        assert_eq!(hz.kind, EventKind::Vessel);
        assert_eq!(hz.source_id, "portwatch_chokepoints");
        // current 15 vs baseline 40 -> deviation -0.625 -> severity 0.85.
        assert!((hz.severity.value() - 0.85).abs() < 1e-9);
        assert_eq!(
            transit_chip(&hz.raw).as_deref(),
            Some("Transit down 63% vs norm (15 vs 40/day)")
        );
        assert_eq!(hz.title, "Strait of Hormuz — transit down 63% (15 vs 40/day)");
        assert_eq!(hz.time.format("%Y-%m-%d").to_string(), "2026-07-01");
        let g = hz.geo.unwrap();
        assert!((g.lat - 26.57).abs() < 1e-6 && (g.lon - 56.25).abs() < 1e-6);

        let cape = &ev[1];
        assert_eq!(cape.id, "portwatch-chokepoint2");
        // current 48 vs baseline 30 -> deviation +0.6 -> severity 0.40.
        assert!((cape.severity.value() - 0.40).abs() < 1e-9);
        assert_eq!(
            transit_chip(&cape.raw).as_deref(),
            Some("Transit up 60% vs norm (48 vs 30/day)")
        );
    }

    #[test]
    fn all_normal_is_ok_empty() {
        // Two chokepoints both flowing at their norm -> zero events, not an error.
        let mut feats = Vec::new();
        feats.extend(feature_rows("chokepoint1", "Suez Canal", 32.35, 30.42, 40, "2026-07-01", |_| 50.0));
        feats.extend(feature_rows("chokepoint6", "Strait of Hormuz", 56.25, 26.57, 40, "2026-07-01", |_| 40.0));
        assert!(parse_portwatch(&wrap(feats)).unwrap().is_empty());
    }

    #[test]
    fn low_volume_chokepoint_is_skipped() {
        // A norm below MIN_BASELINE_TRANSIT: a swing there is percentage noise, dropped.
        let feats = feature_rows("chokepoint9", "Tiny Passage", 10.0, 10.0, 40, "2026-07-01", |off| {
            if off < 7 { 0.0 } else { 2.0 }
        });
        assert!(parse_portwatch(&wrap(feats)).unwrap().is_empty());
    }

    #[test]
    fn missing_geometry_is_skipped_not_fatal() {
        // A chokepoint with a real drop but no geometry can't be placed -> dropped,
        // and the parse still succeeds for the others.
        let mut feats: Vec<Value> = feature_rows("chokepoint6", "Strait of Hormuz", 56.25, 26.57, 47, "2026-07-01", |off| {
            if off < 7 { 15.0 } else { 40.0 }
        });
        // Strip geometry from a placeless chokepoint's rows.
        let mut placeless = feature_rows("chokepoint99", "Nowhere", 0.0, 0.0, 47, "2026-07-01", |off| {
            if off < 7 { 5.0 } else { 40.0 }
        });
        for r in &mut placeless {
            r.as_object_mut().unwrap().remove("geometry");
        }
        feats.extend(placeless);
        let ev = parse_portwatch(&wrap(feats)).unwrap();
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].id, "portwatch-chokepoint6");
    }

    #[test]
    fn errors_on_bad_input() {
        // Not JSON.
        assert!(parse_portwatch("<html>403</html>").is_err());
        // ArcGIS application error (HTTP 200 body).
        assert!(parse_portwatch(r#"{"error":{"code":400,"message":"Invalid query"}}"#).is_err());
        // Missing features array.
        assert!(parse_portwatch(r#"{"objectIdFieldName":"FID"}"#).is_err());
        // Present but empty features -> Ok/empty (a quiet, all-normal world).
        assert!(parse_portwatch(r#"{"features":[]}"#).unwrap().is_empty());
    }

    #[test]
    fn severity_ladder() {
        assert_eq!(severity_for_deviation(-0.80), Some(1.0));
        assert_eq!(severity_for_deviation(-0.60), Some(0.85));
        assert_eq!(severity_for_deviation(-0.40), Some(0.65));
        assert_eq!(severity_for_deviation(-0.28), Some(0.50));
        assert_eq!(severity_for_deviation(-0.10), None);
        assert_eq!(severity_for_deviation(0.20), None);
        assert_eq!(severity_for_deviation(0.45), Some(0.30));
        assert_eq!(severity_for_deviation(0.70), Some(0.40));
    }
}
