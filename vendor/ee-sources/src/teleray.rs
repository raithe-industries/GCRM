//! IRSN / ASNR **Téléray** — France's national **ambient gamma dose-rate alert
//! network** (~470 beacons across mainland France and the DROM-COM overseas
//! territories, each reporting every 10 minutes). Free, no key, open data
//! (credit "IRSN / ASNR — Téléray").
//!
//! Reads the Téléray **OGC API - Features** endpoint
//! `api.teleray.asnr.fr/wfs/collections/measures/items` (`f=json`, `sortby=-time`,
//! `limit=2000`) — a GeoJSON `FeatureCollection`, one `Point` feature per recent
//! per-station reading carrying the ambient gamma **dose equivalent rate in nSv/h**
//! (`doseRateNet`, net of the probe's own background `bruitdefond`; `doseRateRaw` the
//! raw reading), the station id `irsnId`, name `libelle`, an ISO `measurementDate`, and
//! a measurement-state flag `measState`/`validation`. `sortby=-time` returns the newest
//! readings first; per-station dedup below keeps each station's newest.
//!
//! ## Why it's on the map (and not duplicative)
//! This extends the **radiation / nuclear-monitoring modality** (opened by
//! [`super::odlinfo`], Germany; extended by [`super::stuk_radiation`], Finland) to
//! **France — Europe's largest nuclear power (56 operating reactors, ~70% of national
//! electricity), plus the La Hague reprocessing complex**. A dose-rate network is a
//! first-order WWIII-risk observable: a reactor release, a strike on nuclear
//! infrastructure, a detonation or a dispersal event lights it up before almost
//! anything else. Distinct national authority (IRSN/ASNR), distinct geography (all of
//! France + overseas), no overlap with the German or Finnish networks.
//!
//! **Signal-meaningfulness (same universal-baseline argument as `odlinfo`/`stuk`):** an
//! ambient dose rate has a *universal* natural-background baseline — in France roughly
//! 0.06–0.12 µSv/h (60–120 nSv/h), essentially everywhere on Earth 0.05–0.20 µSv/h — so
//! a reading above it is interpretable anywhere without a per-station table (unlike a
//! river gauge). Téléray reports in **nSv/h**; the connector converts to µSv/h and plots
//! **only stations elevated above natural background** (≥ [`ELEVATED_FLOOR`] µSv/h);
//! every background station drops, so an all-normal network — the healthy peacetime
//! state — is Ok/empty (0 events, not an error), and the layer lights up precisely when
//! radiation rises. The 0.3 µSv/h floor is shared with `odlinfo`/`stuk`, so all three
//! radiation feeds calibrate on the identical severity ladder.
//!
//! One [`EventKind::Other`] [`Event`] (the catch-all for a new modality before it earns
//! a first-class variant) per elevated station at its own lat/lon (inline Point
//! geometry — no external join).
//!
//! ## Path A (prod fetches live) — GitHub-anchored schema
//! The live host 403s web fetch in-sandbox (as every gov host does), so endpoint, query
//! params, **auth model** and field schema are anchored to committed bytes: the Kalisio
//! `kalisio/k-teleray` open-source client (`jobfile.js`, fetched off
//! `raw.githubusercontent.com`), which downloads the network with a **plain keyless HTTP
//! request** (no key, no header → auth-free) to
//! `…/wfs/collections/measures/items?limit=2000&sortby=-time`, reading GeoJSON features
//! whose `properties` carry `irsnId`, `measurementDate`, `doseRateRaw`, `doseRateNet`,
//! `bruitdefond`, `validation`, `measState`, `libelle` and whose geometry is the station
//! `Point` (k-teleray derives its station catalogue by de-duplicating these features on
//! `irsnId`, confirming the measure features carry geometry — no join needed). Units
//! (nSv/h), station count and the 10-minute cadence are confirmed from IRSN/ASNR's
//! public Téléray documentation.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

/// Elevated floor in **µSv/h**. French natural background is ~0.06–0.12 µSv/h (60–120
/// nSv/h); 0.3 clears normal background and local geology, so only genuinely elevated
/// readings — 2–5× typical and up — plot. A real radiological event runs far higher
/// (≥1, often ≫10) and saturates the ladder. Shared with `odlinfo`/`stuk_radiation`.
const ELEVATED_FLOOR: f64 = 0.3;

/// Téléray reports in nSv/h; divide by this to get µSv/h (the ladder/chip unit).
const NSV_PER_USV: f64 = 1000.0;

/// IRSN/ASNR Téléray ambient gamma dose-rate source.
#[derive(Default)]
pub struct Teleray;

impl Teleray {
    pub fn url(&self) -> &'static str {
        // OGC API - Features: the most recent readings first, capped at 2000 (enough for
        // one reading per ~470 stations within the last ~40 min); per-station dedup keeps
        // each station's newest. Auth-free (k-teleray hits it with a keyless request).
        "https://api.teleray.asnr.fr/wfs/collections/measures/items?f=json&limit=2000&sortby=-time"
    }
}

#[async_trait]
impl Source for Teleray {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "teleray",
            name: "IRSN Téléray Gamma Dose Rate (France)",
            domain: EventKind::Other,
            cadence: Duration::from_secs(600), // stations report every 10 minutes
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        let body = crate::http::fetch_text(self.url()).await?;
        parse_teleray(&body)
    }
}

/// Normalized 0–1 severity from the dose rate (µSv/h). Identical ladder to
/// `odlinfo`/`stuk_radiation`: below [`ELEVATED_FLOOR`] is dropped upstream, so the
/// lowest rung is "above normal".
fn severity_for_dose(v: f64) -> f64 {
    if v >= 100.0 {
        1.0 // extreme — severe radiological emergency
    } else if v >= 10.0 {
        0.9 // very high
    } else if v >= 1.0 {
        0.7 // high — well beyond any natural level
    } else if v >= 0.5 {
        0.5 // elevated
    } else {
        0.4 // above normal (≥ 0.3)
    }
}

/// Plain-language band for a dose rate (µSv/h), for the operator chip.
fn dose_band(v: f64) -> &'static str {
    if v >= 100.0 {
        "Extreme"
    } else if v >= 10.0 {
        "Very high"
    } else if v >= 1.0 {
        "High"
    } else if v >= 0.5 {
        "Elevated"
    } else {
        "Above normal"
    }
}

/// Operator chip for an elevated station: the dose rate with units + the band, e.g.
/// "0.45 µSv/h · Above normal" / "3.10 µSv/h · High". µSv/h is a defined unit against a
/// universal natural background, so the value is meaningful — not a raw scalar. `raw` is
/// the flat properties object this connector stores (value already in µSv/h).
pub fn dose_chip(raw: &Value) -> Option<String> {
    let v = raw.get("value").and_then(Value::as_f64)?;
    let unit = raw
        .get("unit")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("µSv/h");
    Some(format!("{v:.2} {unit} · {}", dose_band(v)))
}

/// True if a measurement-state string flags a defective / non-valid probe whose reading
/// must not raise a false alarm. Best-effort: the exact `measState`/`validation` enum
/// isn't in the committed anchor, so this drops only *recognizable* defect states
/// (French/English) and keeps unknown ones — it can't over-drop a working network, and
/// the ≥ floor gate already removes all normal readings. `validation:false` (an explicit
/// "not validated" flag) is NOT treated as defective: Téléray shows real-time data
/// before IRSN formally validates it, so requiring validation would wrongly blank the
/// live layer.
fn is_defective(props: &Value) -> bool {
    let state = props
        .get("measState")
        .and_then(Value::as_str)
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    const BAD: [&str; 7] = [
        "defaut",
        "défaut",
        "panne",
        "invalide",
        "invalid",
        "maintenance",
        "hors service",
    ];
    BAD.iter().any(|b| state.contains(b))
}

/// A station's newest elevated-candidate state.
struct Latest {
    time: DateTime<Utc>,
    lat: f64,
    lon: f64,
    value_usv: f64,
    site: String,
}

/// Pure parser: Téléray measures/items GeoJSON -> events. Offline-tested. A missing
/// `features` array is malformed (error, routes to feed-health/last-good). Per station
/// the *newest* reading wins; stations at/below [`ELEVATED_FLOOR`] µSv/h, defective, or
/// lacking geometry / a finite value / a parseable time / an id drop — so an all-normal
/// network (the healthy peacetime state) parses to Ok/empty.
pub fn parse_teleray(json: &str) -> anyhow::Result<Vec<Event>> {
    let root: Value = serde_json::from_str(json)?;
    let features = root
        .get("features")
        .and_then(|f| f.as_array())
        .ok_or_else(|| anyhow::anyhow!("teleray: missing 'features' array"))?;

    // Dedup FIRST (newest reading per station wins, whatever its value), THEN floor —
    // so a station whose newest reading is back to background correctly drops even if an
    // older reading in the batch was elevated (a stale elevated value must never plot).
    let mut latest: HashMap<String, Latest> = HashMap::new();
    let mut had_candidate = 0usize; // usable value+geometry+id readings
    let mut had_time = 0usize; // of those, ones whose measurementDate parsed
    for f in features {
        let props = f.get("properties").cloned().unwrap_or(Value::Null);

        // Prefer the net (background-subtracted) ambient dose rate; fall back to raw.
        // Both are in nSv/h → convert to µSv/h.
        let Some(nsv) = props
            .get("doseRateNet")
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite())
            .or_else(|| {
                props
                    .get("doseRateRaw")
                    .and_then(Value::as_f64)
                    .filter(|v| v.is_finite())
            })
        else {
            continue;
        };
        let value_usv = nsv / NSV_PER_USV;
        if !value_usv.is_finite() {
            continue;
        }

        // Drop a probe flagged defective so a stuck/garbage reading can't false-alarm.
        if is_defective(&props) {
            continue;
        }

        let geo_ll = f
            .get("geometry")
            .filter(|g| g.get("type").and_then(|t| t.as_str()) == Some("Point"))
            .and_then(|g| g.get("coordinates"))
            .and_then(|c| c.as_array())
            .filter(|c| c.len() >= 2)
            .and_then(|c| match (c[0].as_f64(), c[1].as_f64()) {
                (Some(lon), Some(lat)) => Some((lat, lon)),
                _ => None,
            });
        let Some((lat, lon)) = geo_ll else { continue };
        if Geo::new(lat, lon).is_none() {
            continue;
        }

        // Stable station id.
        let sid = props
            .get("irsnId")
            .and_then(|v| v.as_str().map(str::to_string).or_else(|| v.as_i64().map(|n| n.to_string())))
            .filter(|s| !s.is_empty());
        let Some(sid) = sid else { continue };

        had_candidate += 1;

        // ISO timestamp; a reading whose time can't be read is DROPPED, not stamped
        // "now" — an unknown-age elevated gamma value rendered "just now" would be a
        // false freshness claim on exactly the signal where staleness matters most.
        let Some(time) = props
            .get("measurementDate")
            .and_then(Value::as_str)
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|dt| dt.with_timezone(&Utc))
        else {
            continue;
        };
        had_time += 1;

        let site = props
            .get("libelle")
            .and_then(Value::as_str)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_default();

        let slot = latest.entry(sid).or_insert(Latest {
            time,
            lat,
            lon,
            value_usv,
            site: site.clone(),
        });
        if time >= slot.time {
            *slot = Latest { time, lat, lon, value_usv, site };
        }
    }

    // Format-drift tripwire: usable readings existed but EVERY one had an unreadable
    // measurementDate — the upstream timestamp encoding has drifted. Erroring routes it
    // to feed-health/last-good rather than silently blanking the (normally-empty)
    // radiation layer forever. A MIX of readable and unreadable stays a partial success.
    if had_candidate > 0 && had_time == 0 {
        anyhow::bail!(
            "teleray: {had_candidate} reading(s) with unreadable measurementDate and none readable — upstream timestamp format drift?"
        );
    }

    let mut out: Vec<Event> = Vec::with_capacity(latest.len());
    for (sid, l) in latest {
        // Floor applied AFTER dedup: only stations whose NEWEST reading is elevated plot.
        if l.value_usv < ELEVATED_FLOOR {
            continue; // background — the all-clear state
        }
        let Some(geo) = Geo::new(l.lat, l.lon) else { continue };
        let title = if l.site.is_empty() {
            "Elevated gamma dose rate (France)".to_string()
        } else {
            format!("Elevated gamma dose rate · {}", l.site)
        };
        out.push(Event {
            id: format!("teleray-{sid}"),
            source_id: "teleray".to_string(),
            kind: EventKind::Other,
            title,
            time: l.time,
            geo: Some(geo),
            severity: Severity::new(severity_for_dose(l.value_usv)),
            url: Some("https://teleray.asnr.fr/".to_string()),
            raw: serde_json::json!({
                "value": l.value_usv,
                "unit": "µSv/h",
                "dose_rate_nSv_h": l.value_usv * NSV_PER_USV,
                "site": l.site,
                "id": sid,
                "timestamp": l.time.to_rfc3339(),
            }),
        });
    }

    // Hottest first, so a downstream cap keeps the most elevated stations.
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

    // Real Téléray measures/items GeoJSON shape, anchored to kalisio/k-teleray's own
    // client (jobfile.js): FeatureCollection of Point features whose properties carry
    // irsnId / measurementDate / doseRateRaw / doseRateNet / bruitdefond / validation /
    // measState / libelle, values in nSv/h. Rows exercise: a normal-background station
    // (dropped), an above-normal one, a station read twice where the NEWER reading is
    // elevated (dedup → kept at the newer value), a station read twice where the newer
    // reading is normal (dedup → dropped), a high one, a very-high one, a defective probe
    // with a garbage-high value (dropped: measState defect), and a no-geometry record.
    const FIXTURE: &str = r#"{
      "type": "FeatureCollection",
      "numberReturned": 9,
      "features": [
        {"type":"Feature","geometry":{"type":"Point","coordinates":[2.3522,48.8566]},
         "properties":{"irsnId":"75001","libelle":"Paris","measurementDate":"2026-07-05T10:30:00Z",
           "doseRateRaw":95.0,"doseRateNet":88.0,"bruitdefond":7.0,"validation":false,"measState":"normal"}},
        {"type":"Feature","geometry":{"type":"Point","coordinates":[5.4474,43.5297]},
         "properties":{"irsnId":"13001","libelle":"Aix-en-Provence","measurementDate":"2026-07-05T10:30:00Z",
           "doseRateRaw":460.0,"doseRateNet":450.0,"bruitdefond":10.0,"validation":false,"measState":"normal"}},
        {"type":"Feature","geometry":{"type":"Point","coordinates":[1.4442,43.6047]},
         "properties":{"irsnId":"31001","libelle":"Toulouse","measurementDate":"2026-07-05T10:20:00Z",
           "doseRateRaw":110.0,"doseRateNet":100.0,"bruitdefond":10.0,"validation":false,"measState":"normal"}},
        {"type":"Feature","geometry":{"type":"Point","coordinates":[1.4442,43.6047]},
         "properties":{"irsnId":"31001","libelle":"Toulouse","measurementDate":"2026-07-05T10:30:00Z",
           "doseRateRaw":640.0,"doseRateNet":620.0,"bruitdefond":20.0,"validation":false,"measState":"normal"}},
        {"type":"Feature","geometry":{"type":"Point","coordinates":[-1.6778,48.1173]},
         "properties":{"irsnId":"35001","libelle":"Rennes","measurementDate":"2026-07-05T10:20:00Z",
           "doseRateRaw":900.0,"doseRateNet":880.0,"bruitdefond":20.0,"validation":false,"measState":"normal"}},
        {"type":"Feature","geometry":{"type":"Point","coordinates":[-1.6778,48.1173]},
         "properties":{"irsnId":"35001","libelle":"Rennes","measurementDate":"2026-07-05T10:30:00Z",
           "doseRateRaw":105.0,"doseRateNet":95.0,"bruitdefond":10.0,"validation":false,"measState":"normal"}},
        {"type":"Feature","geometry":{"type":"Point","coordinates":[-1.8639,46.8]},
         "properties":{"irsnId":"85001","libelle":"La Hague area","measurementDate":"2026-07-05T10:30:00Z",
           "doseRateRaw":3100.0,"doseRateNet":3100.0,"bruitdefond":0.0,"validation":false,"measState":"normal"}},
        {"type":"Feature","geometry":{"type":"Point","coordinates":[7.75,48.58]},
         "properties":{"irsnId":"67001","libelle":"Strasbourg (defective)","measurementDate":"2026-07-05T10:30:00Z",
           "doseRateRaw":9000.0,"doseRateNet":9000.0,"bruitdefond":0.0,"validation":false,"measState":"En défaut"}},
        {"type":"Feature","geometry":null,
         "properties":{"irsnId":"00000","libelle":"No geometry","measurementDate":"2026-07-05T10:30:00Z",
           "doseRateRaw":5000.0,"doseRateNet":5000.0,"bruitdefond":0.0,"validation":false,"measState":"normal"}}
      ]
    }"#;

    #[test]
    fn keeps_elevated_dedups_to_newest_and_drops_normal_defective_and_no_geometry() {
        let ev = parse_teleray(FIXTURE).unwrap();
        // Kept: Aix (0.45), Toulouse (newest 0.62), La Hague (3.1). Dropped: Paris
        // (0.088 normal), Toulouse older (0.10 — loses dedup), Rennes (newest 0.095
        // normal — the older 0.88 loses dedup), Strasbourg (defective), no-geometry.
        assert_eq!(ev.len(), 3, "three elevated stations after dedup");

        // Hottest first: La Hague (3.1 → 0.7) before Toulouse (0.62 → 0.5) before Aix
        // (0.45 → 0.4).
        assert_eq!(ev[0].id, "teleray-85001");
        assert_eq!(ev[0].kind, EventKind::Other);
        assert_eq!(ev[0].source_id, "teleray");
        assert_eq!(ev[0].title, "Elevated gamma dose rate · La Hague area");
        assert!((ev[0].severity.value() - 0.7).abs() < 1e-9);
        assert_eq!(dose_chip(&ev[0].raw).as_deref(), Some("3.10 µSv/h · High"));

        // Toulouse: the NEWER 0.62 reading won the dedup, not the older 0.10.
        assert_eq!(ev[1].id, "teleray-31001");
        assert!((ev[1].severity.value() - 0.5).abs() < 1e-9);
        assert_eq!(dose_chip(&ev[1].raw).as_deref(), Some("0.62 µSv/h · Elevated"));
        assert_eq!(ev[1].time.to_rfc3339(), "2026-07-05T10:30:00+00:00");
        let g = ev[1].geo.unwrap();
        assert!((g.lat - 43.6047).abs() < 1e-6 && (g.lon - 1.4442).abs() < 1e-6);

        // Aix: above normal.
        assert_eq!(ev[2].id, "teleray-13001");
        assert!((ev[2].severity.value() - 0.4).abs() < 1e-9);
        assert_eq!(dose_chip(&ev[2].raw).as_deref(), Some("0.45 µSv/h · Above normal"));

        // No dropped station leaked through.
        assert!(ev.iter().all(|e| e.id != "teleray-75001")); // Paris normal
        assert!(ev.iter().all(|e| e.id != "teleray-35001")); // Rennes now normal
        assert!(ev.iter().all(|e| e.id != "teleray-67001")); // Strasbourg defective
        assert!(ev.iter().all(|e| e.id != "teleray-00000")); // no geometry
    }

    #[test]
    fn all_normal_network_is_ok_not_error() {
        // Empty collection -> zero events, not a failure.
        let empty = r#"{"type":"FeatureCollection","numberReturned":0,"features":[]}"#;
        assert!(parse_teleray(empty).unwrap().is_empty());
        // Every station reads natural background -> nothing plots (healthy peacetime).
        let normal = r#"{"type":"FeatureCollection","features":[
          {"type":"Feature","geometry":{"type":"Point","coordinates":[2.35,48.85]},
           "properties":{"irsnId":"a","libelle":"Quiet","measurementDate":"2026-07-05T10:30:00Z","doseRateNet":90.0,"measState":"normal"}},
          {"type":"Feature","geometry":{"type":"Point","coordinates":[3.0,45.0]},
           "properties":{"irsnId":"b","libelle":"Also quiet","measurementDate":"2026-07-05T10:30:00Z","doseRateNet":120.0,"measState":"normal"}}
        ]}"#;
        assert!(parse_teleray(normal).unwrap().is_empty());
    }

    #[test]
    fn falls_back_to_raw_when_net_missing() {
        // No doseRateNet but an elevated doseRateRaw -> plotted off the raw value.
        let json = r#"{"type":"FeatureCollection","features":[
          {"type":"Feature","geometry":{"type":"Point","coordinates":[2.35,48.85]},
           "properties":{"irsnId":"r1","libelle":"RawOnly","measurementDate":"2026-07-05T10:30:00Z","doseRateRaw":520.0,"measState":"normal"}}
        ]}"#;
        let ev = parse_teleray(json).unwrap();
        assert_eq!(ev.len(), 1);
        assert_eq!(dose_chip(&ev[0].raw).as_deref(), Some("0.52 µSv/h · Elevated"));
    }

    #[test]
    fn unreadable_time_drops_the_record_but_total_drift_errors() {
        // One elevated reading with a bad time is dropped (not aged "now")...
        let mixed = r#"{"type":"FeatureCollection","features":[
          {"type":"Feature","geometry":{"type":"Point","coordinates":[2.35,48.85]},
           "properties":{"irsnId":"x","libelle":"BadTime","measurementDate":"hier","doseRateNet":620.0,"measState":"normal"}},
          {"type":"Feature","geometry":{"type":"Point","coordinates":[3.0,45.0]},
           "properties":{"irsnId":"y","libelle":"Good","measurementDate":"2026-07-05T10:30:00Z","doseRateNet":620.0,"measState":"normal"}}
        ]}"#;
        let ev = parse_teleray(mixed).unwrap();
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].id, "teleray-y");
        // ...but if EVERY elevated reading has an unreadable time, that's format drift → error.
        let all_bad = r#"{"type":"FeatureCollection","features":[
          {"type":"Feature","geometry":{"type":"Point","coordinates":[2.35,48.85]},
           "properties":{"irsnId":"x","libelle":"BadTime","measurementDate":"hier","doseRateNet":620.0,"measState":"normal"}}
        ]}"#;
        assert!(parse_teleray(all_bad).is_err());
    }

    #[test]
    fn errors_on_bad_input() {
        // Missing the features array is malformed.
        assert!(parse_teleray(r#"{"type":"FeatureCollection"}"#).is_err());
        // Not JSON at all (e.g. a 403 HTML body).
        assert!(parse_teleray("<html>403 Forbidden</html>").is_err());
    }

    #[test]
    fn severity_and_band_ladder_with_dose() {
        assert!((severity_for_dose(0.3) - 0.4).abs() < 1e-9);
        assert!((severity_for_dose(0.5) - 0.5).abs() < 1e-9);
        assert!((severity_for_dose(1.0) - 0.7).abs() < 1e-9);
        assert!((severity_for_dose(10.0) - 0.9).abs() < 1e-9);
        assert!((severity_for_dose(100.0) - 1.0).abs() < 1e-9);
        assert_eq!(dose_band(0.35), "Above normal");
        assert_eq!(dose_band(0.6), "Elevated");
        assert_eq!(dose_band(2.0), "High");
        assert_eq!(dose_band(50.0), "Very high");
        assert_eq!(dose_band(200.0), "Extreme");
    }

    #[test]
    fn chip_handles_missing_value_and_unit() {
        assert_eq!(dose_chip(&serde_json::json!({"unit":"µSv/h"})), None);
        assert_eq!(
            dose_chip(&serde_json::json!({"value": 0.42})).as_deref(),
            Some("0.42 µSv/h · Above normal")
        );
    }
}
