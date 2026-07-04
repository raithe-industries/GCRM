//! NGA Anti-Shipping Activity Messages (ASAM) — reported hostile acts against ships
//! and mariners worldwide (piracy, armed robbery, boarding, hijacking, drone/missile
//! attacks, kidnapping), each carrying a decimal latitude/longitude.
//!
//! ASAM is compiled and published **daily** by the U.S. National Geospatial-Intelligence
//! Agency (NGA) as part of its Maritime Safety Information (MSI) service. Unlike the
//! [`super::digitraffic_ais`] Vessel feed (live AIS positions, Baltic-only) this is a
//! **maritime-security incident** modality — a hostile act at a place and time — with
//! **global** reach, so it densifies the Vessel layer over the theatres a war-risk
//! operator actually watches: the Red Sea / Bab-el-Mandeb, the Gulf of Aden, the Strait
//! of Hormuz, the Gulf of Guinea, the Singapore/Malacca Straits and the South China Sea.
//!
//! ## Ingestion — Path A (live JSON)
//! The connector fetches the public MSI endpoint
//! `https://msi.nga.mil/api/publications/asam?output=json&sort=date&minOccurDate=<date>`
//! (auth-free; US-Gov public domain). The host 403s from the build sandbox like every
//! gov host, so the wire schema is anchored to genuine committed bytes: the NGA reference
//! consumers `ngageoint/anti-piracy-{iOS,android}-app` (which read `json["asam"]` from the
//! same endpoint) and the `hrbrmstr/asam` R package, whose documented record columns are
//! `reference, date, latitude, longitude, navArea, subreg, hostility, victim, description`
//! (sample row `2019-73 | 2019-09-30 | 1.04 | 104. | XI | 71 | Five Armed robbers |
//! Bulk Carrier | SINGAPORE STRAITS ...`). Prod (full network) fetches it live.

use async_trait::async_trait;
use chrono::{Duration as ChronoDuration, NaiveDate, TimeZone, Utc};
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use serde_json::Value;
use std::time::Duration;

/// How far back to request incidents, so the Vessel layer shows a current
/// maritime-security picture (attacks on shipping are sparse enough that a rolling
/// year reads as "the state of play" rather than history) rather than the full archive.
const LOOKBACK_DAYS: i64 = 365;

/// NGA Anti-Shipping Activity Messages source (Path-A live JSON).
#[derive(Default)]
pub struct Asam;

#[async_trait]
impl Source for Asam {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "asam",
            name: "NGA Anti-Shipping Activity Messages",
            domain: EventKind::Vessel,
            // ASAM is refreshed daily upstream; no need to poll hard.
            cadence: Duration::from_secs(21_600),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        let min = (Utc::now().date_naive() - ChronoDuration::days(LOOKBACK_DAYS))
            .format("%Y-%m-%d");
        let url = format!(
            "https://msi.nga.mil/api/publications/asam?output=json&sort=date&minOccurDate={min}"
        );
        let body = crate::http::fetch_text(&url).await?;
        parse_asam(&body)
    }
}

/// A number that may arrive as a JSON number OR a JSON string (some MSI endpoints
/// stringify everything); tolerant of a stray trailing dot ("104.").
fn num(v: Option<&Value>) -> Option<f64> {
    v.and_then(|x| {
        x.as_f64().or_else(|| {
            x.as_str()
                .map(|s| s.trim().trim_end_matches(','))
                .and_then(|s| s.parse::<f64>().ok())
        })
    })
}

/// A string field that may arrive as a JSON string or number.
fn text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
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

/// The escalation class + normalized severity of an incident, read from the aggressor
/// (`hostility`) + narrative (`description`). ASAM carries no numeric severity, so the
/// signal is the *kind of hostile act* — a real, baseline-relative maritime-security
/// ladder (an armed attack that discharges weapons or takes a hostage outranks a
/// boarding, which outranks petty theft, which outranks a merely attempted approach).
/// Returns `(class label, severity)`.
fn classify(hostility: &str, description: &str) -> (String, f64) {
    let t = format!("{} {}", hostility, description).to_lowercase();
    let has = |k: &str| t.contains(k);
    // An attempt that did not land reads a notch lower than the same act completed.
    let attempted = has("attempt") || has("unsuccessful") || has("evaded") || has("thwarted")
        || has("prevented") || has("foiled") || has("aborted") || has("chased away")
        || has("repelled") || has("failed to board");

    // Violent armed attack: weapons discharged, people harmed, or the vessel taken.
    if has("hijack") || has("kidnap") || has("abduct") || has("hostage") || has("fired")
        || has("gunfire") || has("opened fire") || has("shot") || has("rocket")
        || has("missile") || has("rpg") || has("grenade") || has("explos") || has("mine ")
        || has("uav") || has("drone") || has("usv") || has("killed") || has("wounded")
        || has("injured") || has("seized") || has("sank") || has("sunk")
    {
        let (label, sev) = if attempted { ("Attempted attack", 0.7) } else { ("Armed attack", 0.9) };
        return (label.to_string(), sev);
    }
    // Boarding / armed robbery: intruders got aboard, often armed.
    if has("board") || has("armed") || has("assault") || has("pipe") || has("knife")
        || has("machete") || has("kidnapp")
    {
        let (label, sev) = if attempted { ("Attempted boarding", 0.45) } else { ("Boarding", 0.65) };
        return (label.to_string(), sev);
    }
    // Theft / robbery: property taken, no armed boarding noted.
    if has("theft") || has("robber") || has("robery") || has("stole") || has("stolen")
        || has("robbery") || has("pilfer")
    {
        let (label, sev) = if attempted { ("Attempted theft", 0.35) } else { ("Robbery", 0.5) };
        return (label.to_string(), sev);
    }
    if attempted {
        return ("Attempted approach".to_string(), 0.3);
    }
    ("Anti-shipping incident".to_string(), 0.45)
}

/// Operator chip behind an ASAM dot: the escalation class + the vessel targeted, e.g.
/// "Boarding · Bulk Carrier" / "Armed attack · Chemical Tanker". Signal-meaningful — the
/// class is a defined maritime-security tier and the victim is a real vessel type. `None`
/// only if the record somehow carries neither a class nor a victim (never, in practice).
pub fn asam_chip(raw: &Value) -> Option<String> {
    let class = raw.get("class").and_then(Value::as_str).unwrap_or("").trim();
    let victim = raw.get("victim").and_then(Value::as_str).unwrap_or("").trim();
    match (class.is_empty(), victim.is_empty()) {
        (true, true) => None,
        (false, true) => Some(class.to_string()),
        (true, false) => Some(victim.to_string()),
        (false, false) => Some(format!("{class} · {victim}")),
    }
}

/// Pure parser: ASAM JSON (`{"asam":[ … ]}`) -> one [`EventKind::Vessel`] event per
/// hostile-act report at its own lat/lon, newest first. Offline-tested. A payload that
/// isn't an object with an `asam` array is an error; records without a usable centroid
/// are dropped, so an empty `asam` array is Ok/empty (a quiet window), not an error.
pub fn parse_asam(body: &str) -> anyhow::Result<Vec<Event>> {
    let root: Value = serde_json::from_str(body)?;
    let items = root
        .get("asam")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("asam: missing `asam` array"))?;

    let mut out = Vec::with_capacity(items.len());
    for it in items {
        let (Some(lat), Some(lon)) = (num(it.get("latitude")), num(it.get("longitude"))) else {
            continue;
        };
        let Some(geo) = Geo::new(lat, lon) else { continue };

        let reference = text(it.get("reference"));
        let hostility = text(it.get("hostility"));
        let victim = text(it.get("victim"));
        let description = text(it.get("description"));
        let nav_area = text(it.get("navArea"));

        // date is "YYYY-MM-DD"; anchor at UTC midnight. Missing/garbled → now.
        let date_s = text(it.get("date"));
        let time = date_s
            .get(0..10)
            .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .map(|dt| Utc.from_utc_datetime(&dt))
            .unwrap_or_else(Utc::now);

        let (class, severity) = classify(&hostility, &description);

        let vessel = if victim.is_empty() { "Vessel".to_string() } else { victim.clone() };
        let aggressor = if hostility.is_empty() { class.clone() } else { hostility.clone() };
        let title = format!("{vessel} — {aggressor}");

        // Stable id: the ASAM reference is unique ("2024-123"); fall back to date+coords.
        let id = if reference.is_empty() {
            format!("asam-{}-{:.3}-{:.3}", date_s.get(0..10).unwrap_or(""), lat, lon)
        } else {
            format!("asam-{}", slug(&reference))
        };

        out.push(Event {
            id,
            source_id: "asam".to_string(),
            kind: EventKind::Vessel,
            title,
            time,
            geo: Some(geo),
            severity: Severity::new(severity),
            url: Some("https://msi.nga.mil/Piracy".to_string()),
            raw: serde_json::json!({
                "reference": reference,
                "hostility": hostility,
                "victim": victim,
                "navArea": nav_area,
                "class": class,
                "date": date_s,
            }),
        });
    }

    // Most severe first (then newest), so a downstream cap keeps the worst incidents.
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

    // Real-shape ASAM payload: a Red Sea drone/missile attack (armed attack, high),
    // a Singapore Strait armed boarding (boarding, med), an attempted approach off
    // Somalia (attempted, low), and a bad-centroid record that must be dropped.
    // Coordinates arrive as a JSON number in some rows and a string in others (both
    // MSI encodings), and one longitude carries the trailing-dot form "43.".
    const FIXTURE: &str = r#"{
      "asam": [
        {"reference":"2026-118","date":"2026-05-30","latitude":13.5,"longitude":"43.",
         "navArea":"IX","subreg":"57","hostility":"Unknown","victim":"Chemical Tanker",
         "description":"Vessel reported an explosion after being struck by a UAV; fire on deck."},
        {"reference":"2026-092","date":"2026-04-11","latitude":"1.23","longitude":"104.30",
         "navArea":"XI","subreg":"71","hostility":"Four robbers","victim":"Bulk Carrier",
         "description":"Four armed robbers boarded the underway vessel in the Singapore Strait."},
        {"reference":"2026-070","date":"2026-03-02","latitude":2.1,"longitude":49.8,
         "navArea":"IX","subreg":"62","hostility":"Suspicious skiff","victim":"Container Ship",
         "description":"A skiff approached but the attempted approach was evaded after the master increased speed."},
        {"reference":"2026-BAD","date":"2026-02-01","latitude":999.0,"longitude":0.0,
         "navArea":"IX","subreg":"62","hostility":"Unknown","victim":"Tanker",
         "description":"bad coordinates"}
      ]
    }"#;

    #[test]
    fn parses_fixture() {
        let ev = parse_asam(FIXTURE).unwrap();
        // Bad-centroid dropped; three plottable incidents, most severe (the UAV attack) first.
        assert_eq!(ev.len(), 3);
        assert!(ev.iter().all(|e| e.kind == EventKind::Vessel && e.geo.is_some()));

        let attack = ev.iter().find(|e| e.id == "asam-2026-118").unwrap();
        // Armed attack (UAV/explosion) — high severity, near the top of the ladder.
        assert!(attack.severity.value() >= 0.9 - 1e-9);
        assert_eq!(asam_chip(&attack.raw).as_deref(), Some("Armed attack · Chemical Tanker"));
        assert_eq!(attack.title, "Chemical Tanker — Unknown");
        // Trailing-dot longitude "43." parsed as 43.0.
        let g = attack.geo.unwrap();
        assert!((g.lat - 13.5).abs() < 1e-9 && (g.lon - 43.0).abs() < 1e-9);

        let boarding = ev.iter().find(|e| e.id == "asam-2026-092").unwrap();
        assert!((boarding.severity.value() - 0.65).abs() < 1e-9);
        assert_eq!(asam_chip(&boarding.raw).as_deref(), Some("Boarding · Bulk Carrier"));
        // String-encoded lat/lon parsed.
        let g = boarding.geo.unwrap();
        assert!((g.lat - 1.23).abs() < 1e-9 && (g.lon - 104.30).abs() < 1e-9);

        let approach = ev.iter().find(|e| e.id == "asam-2026-070").unwrap();
        // Attempted + evaded reads lowest.
        assert!((approach.severity.value() - 0.3).abs() < 1e-9);
        assert_eq!(asam_chip(&approach.raw).as_deref(), Some("Attempted approach · Container Ship"));

        // Severity-sorted: attack > boarding > approach.
        assert!(ev[0].severity.value() >= ev[1].severity.value());
        assert!(ev[1].severity.value() >= ev[2].severity.value());
    }

    #[test]
    fn empty_window_is_ok_not_error() {
        // A quiet window (no incidents in range) is a valid empty result, not an error.
        assert!(parse_asam(r#"{"asam":[]}"#).unwrap().is_empty());
    }

    #[test]
    fn errors_on_bad_input() {
        // Not JSON at all.
        assert!(parse_asam("<html>403</html>").is_err());
        // JSON but no `asam` array.
        assert!(parse_asam(r#"{"error":"nope"}"#).is_err());
    }

    #[test]
    fn classify_ladder() {
        // Armed attack outranks boarding outranks robbery outranks attempted.
        assert_eq!(classify("Pirates", "opened fire with an RPG").0, "Armed attack");
        assert!(classify("Pirates", "opened fire").1 > classify("Robbers", "boarded the vessel").1);
        assert_eq!(classify("Robbers", "boarded the vessel").0, "Boarding");
        assert_eq!(classify("Thieves", "stole ship stores").0, "Robbery");
        // Attempted de-escalates each tier.
        assert!(classify("Skiff", "attempted to board but was repelled").1 < 0.65);
        assert_eq!(classify("Skiff", "suspicious approach, evaded").0, "Attempted approach");
        // Attempted armed attack still ranks above a completed boarding.
        assert!(
            classify("Militants", "attempted missile strike, missed").1
                > classify("Robbers", "boarded the vessel").1
        );
    }
}
