// ------------------------------------------------------------
// RAiTHE INDUSTRIES INCORPORATED
// Copyright (c) 2026 All Rights Reserved.
// ------------------------------------------------------------

//! OSINT world-map + Finance Radar surface for the dashboard.
//!
//! Thin GCRM-side glue over the `engineering-effects` modules (World Monitor /
//! SitDeck parity): it pulls the live `ee-sources` feeds, turns them into GeoJSON
//! via `ee-view`, overlays GCRM's own theater flashpoints, and exposes the layer
//! registry + base-map catalogue the dashboard map renders. A second entry point
//! computes the `ee-correlate` Finance Radar from the Yahoo market stream.
//!
//! All upstream I/O is best-effort: each feed is time-boxed and a failure is
//! reported in `errors[]` rather than failing the whole response, so one slow
//! provider can never blank the map.

use std::collections::HashMap;
use std::time::{Duration as StdDuration, Instant};

use crate::models::EscalationRung;
use ee_core::{Event, Source};
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tokio::time::timeout;

/// Server-side TTL cache for one upstream-heavy payload. The dashboard polls
/// `/api/map` and `/api/finance` every 60s *per client*, and each miss fans out
/// to several rate-limited upstreams (OpenSky/Yahoo/USGS/GDACS/NWS/EONET).
/// Coalescing those behind a short TTL keeps concurrent viewers — and
/// back-to-back polls — from re-hitting (and getting throttled by) the
/// providers, while staleness stays well under the feeds' own cadence.
struct PayloadCache {
    inner: Mutex<Option<(Instant, Value)>>,
    ttl: StdDuration,
}

impl PayloadCache {
    const fn new(ttl: StdDuration) -> Self {
        Self {
            inner: Mutex::const_new(None),
            ttl,
        }
    }

    /// Return the cached value if still fresh, else recompute via `build` while
    /// holding the lock so only one refresh runs at a time — concurrent callers
    /// wait and reuse that single fresh result instead of each hitting upstream.
    async fn get_or_refresh<F, Fut>(&self, build: F) -> Value
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Value>,
    {
        let mut g = self.inner.lock().await;
        if let Some((at, v)) = g.as_ref() {
            if at.elapsed() < self.ttl {
                return v.clone();
            }
        }
        let fresh = build().await;
        *g = Some((Instant::now(), fresh.clone()));
        fresh
    }
}

/// Upstream feeds change slowly; coalesce them well above the 60s client poll. The
/// map TTL is longer (3 min) to stay within OpenSky's anonymous daily credit budget.
static MAP_FEEDS_CACHE: PayloadCache = PayloadCache::new(StdDuration::from_secs(180));
static FINANCE_CACHE: PayloadCache = PayloadCache::new(StdDuration::from_secs(50));

/// Per-feed last-good event batches. `None` until first use (so it can be a `const`
/// static); filled lazily under the lock in `feeds_payload`. Lets a transient empty
/// upstream fall back to its most recent good data instead of a deceptive zero.
type LastGoodBatches = HashMap<String, (Instant, Vec<Event>)>;
static FEED_LAST_GOOD: Mutex<Option<LastGoodBatches>> = Mutex::const_new(None);
/// How long a feed's last-good batch may stand in for an empty live pull (30 min).
const LAST_GOOD_MAX_AGE: StdDuration = StdDuration::from_secs(1800);

/// A representative coordinate (lat, lon) for each canonical GCRM theater id, so the
/// abstract flashpoints can be placed on the map. `other` has no fixed location.
fn theater_coord(theater_id: &str) -> Option<(f64, f64)> {
    let p = match theater_id {
        "nato_russia" => (49.0, 32.0),       // Ukraine / eastern front
        "us_iran" => (26.6, 56.3),           // Strait of Hormuz
        "us_china_taiwan" => (24.0, 119.5),  // Taiwan Strait
        "india_pakistan" => (34.0, 74.5),    // Kashmir line of control
        "korea" => (38.0, 127.0),            // Korean peninsula / DMZ
        _ => return None,                    // "other" / unknown -> not placed
    };
    Some(p)
}

/// Authoritative escalation rung → marker colour. Keyed off the engine's OWN rung,
/// NOT a re-derivation from raw heat: `theater::rung_for` can raise a theater's rung
/// ABOVE its heat-implied band (great-power involvement, WMD use, nuclear use all
/// force a higher rung), so colouring by heat would understate exactly those apex
/// cases and could disagree with the marker's own `rung_label`. Deriving from `rung`
/// keeps the colour consistent with the label, gives the apex Systemic rung its own
/// distinct colour, and removes a third hard-coded copy of the rung heat thresholds
/// (the boundaries now live only in `theater.rs`).
fn rung_color(rung: EscalationRung) -> &'static str {
    match rung {
        EscalationRung::Stable        => "#1D9E75",
        EscalationRung::Tension       => "#d4962a",
        EscalationRung::Crisis        => "#e67e22",
        EscalationRung::LimitedWar    => "#c0392b",
        EscalationRung::GreatPowerWar => "#7a0000",
        EscalationRung::Systemic      => "#b5179e", // apex (nuclear use) — distinct from the red ramp
    }
}

/// Turn the snapshot's `theaters` array into placed GeoJSON flashpoint features.
/// Theaters with no known coordinate (e.g. `other`) are skipped. Pure — no I/O.
fn build_theater_features(snapshot: &Option<Value>) -> Vec<Value> {
    let mut out = Vec::new();
    let Some(theaters) = snapshot
        .as_ref()
        .and_then(|s| s.get("theaters"))
        .and_then(|t| t.as_array())
    else {
        return out;
    };
    for t in theaters {
        let id = t.get("theater_id").and_then(|v| v.as_str()).unwrap_or("");
        let Some((lat, lon)) = theater_coord(id) else { continue };
        let heat = t.get("heat").and_then(|v| v.as_f64()).unwrap_or(0.0);
        // Colour by the engine's authoritative rung (carried in the snapshot), not by
        // heat — so the marker can't understate a great-power / nuclear rung override.
        let rung: EscalationRung = t
            .get("rung")
            .and_then(|r| serde_json::from_value(r.clone()).ok())
            .unwrap_or(EscalationRung::Stable);
        out.push(json!({
            "type": "Feature",
            "geometry": { "type": "Point", "coordinates": [lon, lat] },
            "properties": {
                "id": id,
                "label": t.get("label").and_then(|v| v.as_str()).unwrap_or(id),
                "rung_label": t.get("rung_label").and_then(|v| v.as_str()).unwrap_or(""),
                "heat": heat,
                "trend": t.get("trend").and_then(|v| v.as_str()).unwrap_or(""),
                "event_count": t.get("event_count").and_then(|v| v.as_u64()).unwrap_or(0),
                // Persistence-floor honesty: the marker must not paint a remembered war-state
                // (held through a multi-day news gap) identical to a live-hot flashpoint. Carry
                // the engine's own flags so the popup can flag a held read + how far the fresh
                // read has decayed below it (same contract as the ladder chip / hero caveat).
                "held_by_floor": t.get("held_by_floor").and_then(|v| v.as_bool()).unwrap_or(false),
                "fresh_rung_label": t.get("fresh_rung_label").and_then(|v| v.as_str()).unwrap_or(""),
                "color": rung_color(rung),
                "layer": "theaters",
            }
        }));
    }
    out
}

/// Run one source with a timeout; returns its events and an optional error label.
async fn fetch_one(name: &'static str, src: impl Source, secs: u64) -> (Vec<Event>, Option<String>) {
    match timeout(StdDuration::from_secs(secs), src.fetch()).await {
        Ok(Ok(evs)) => (evs, None),
        Ok(Err(e)) => (Vec::new(), Some(format!("{name}: {e}"))),
        Err(_) => (Vec::new(), Some(format!("{name}: timeout"))),
    }
}

/// A short, human-readable value behind a feed dot — the real-world quantity
/// (magnitude, fire power, alert class, air-quality index) lifted from the provider's
/// raw payload. `None` when the source has no obvious scalar worth surfacing; the
/// popup then just shows the signal type and time.
fn feed_detail(e: &Event) -> Option<String> {
    let props = e.raw.get("properties");
    let pf = |k: &str| props.and_then(|p| p.get(k));
    match e.source_id.as_str() {
        "usgs" => pf("mag").and_then(Value::as_f64).map(|m| format!("M{m:.1}")),
        "eqcanada" => e.raw.get("mag").and_then(Value::as_f64).map(|m| format!("M{m:.1}")),
        "emsc" => pf("mag").and_then(Value::as_f64).map(|m| format!("M{m:.1}")),
        "cwfis" => pf("frp").and_then(Value::as_f64).map(|f| format!("{f:.0} MW fire power")),
        "firms" => e.raw.get("frp").and_then(Value::as_f64).map(|f| format!("{f:.0} MW fire power")),
        "acled" => {
            let etype = e.raw.get("event_type").and_then(Value::as_str).unwrap_or("Conflict");
            let dead = e.raw.get("fatalities").and_then(|v| {
                v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            }).unwrap_or(0.0);
            Some(if dead > 0.0 { format!("{etype} · {dead:.0} killed") } else { etype.to_string() })
        }
        "gvp_volcano" => {
            let ongoing = pf("ContinuingEruption").is_some_and(|v| {
                v.as_str() == Some("Yes") || v.as_bool() == Some(true) || v.as_i64() == Some(1)
            });
            Some(if ongoing { "Ongoing eruption".into() } else { "Recent eruption".into() })
        }
        "healthmap" => e.raw.get("label").and_then(Value::as_str).filter(|s| !s.is_empty()).map(String::from),
        "ontario511" | "alberta511" => {
            let full = e.raw.get("IsFullClosure").and_then(Value::as_bool).unwrap_or(false);
            if full { Some("Full closure".to_string()) } else { e.raw.get("LanesAffected").and_then(Value::as_str).filter(|s| !s.is_empty()).map(String::from) }
        }
        "drivebc" => {
            // "Major" / "Minor" — the Open511 severity enum (all-caps on the wire), the
            // real road-impact read; title-cased so the chip isn't shouting.
            e.raw.get("severity").and_then(Value::as_str).filter(|s| !s.is_empty()).map(title_case)
        }
        // Short English chip from the French `entrave` (no raw French sentences on chips).
        "quebec511" => e
            .raw
            .get("entrave")
            .and_then(Value::as_str)
            .and_then(ee_sources::quebec511::entrave_chip),
        "cwfis_activefires" => {
            let stage = match e.raw.get("stage_of_control_status").and_then(Value::as_str).unwrap_or("") {
                "OC" => "Out of control",
                "BH" => "Being held",
                "UC" => "Under control",
                _ => "Active",
            };
            let size = e.raw.get("fire_size").and_then(Value::as_f64).unwrap_or(0.0);
            Some(format!("{stage} · {size:.0} ha"))
        }
        "cbsa_bwt" => e.raw.get("max_wait_min").and_then(Value::as_u64).map(|m| {
            if m == 0 { "No delay".to_string() } else { format!("{m} min wait") }
        }),
        "navcanada" => e.raw.get("tag").and_then(Value::as_str).filter(|s| !s.is_empty()).map(String::from),
        "digitraffic_ais" => {
            let sog = e.raw.get("sog").and_then(Value::as_f64).unwrap_or(0.0);
            let status = e.raw.get("status").and_then(Value::as_str).unwrap_or("Under way");
            Some(format!("{sog:.1} kn · {status}"))
        }
        "ucdp_ged" => {
            let best = e.raw.get("best").and_then(Value::as_f64).unwrap_or(0.0);
            let ty = e.raw.get("type").and_then(Value::as_str).unwrap_or("Conflict");
            Some(if best >= 1.0 { format!("{best:.0} killed · {ty}") } else { ty.to_string() })
        }
        "gdacs" => {
            // GDACS stores the whole feature in `raw`, so the authoritative read sits in
            // `properties`: the Red/Orange/Green alert level + the hazard type, plus GDACS's
            // own `severitydata.severitytext` — a human-readable, unit-carrying summary
            // ("Magnitude 6.1M, Depth:10km", "max wind 92 km/h"). Without this arm a major
            // global disaster plotted as a bare dot with no severity at all.
            let level = pf("alertlevel").and_then(Value::as_str).filter(|s| !s.is_empty());
            let etype = pf("eventtype").and_then(Value::as_str).map(gdacs_hazard).unwrap_or("Disaster");
            let head = match level {
                Some(l) => format!("{l} · {etype}"),
                None => etype.to_string(),
            };
            // Append GDACS's severity text only when it's short enough for a chip (some
            // entries carry a full sentence); otherwise the alert level + type stands alone.
            let detail = pf("severitydata")
                .and_then(|s| s.get("severitytext"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty() && s.chars().count() <= 60);
            Some(match detail {
                Some(d) => format!("{head} · {d}"),
                None => head,
            })
        }
        // Tropical-cyclone classification + Saffir–Simpson category + max wind (kt),
        // e.g. "Hurricane Cat 1 · 75 kt" — the operational read behind the storm dot.
        "nhc" => ee_sources::nhc::storm_chip(&e.raw),
        // JMA typhoon category (with intensity grade) + max wind (kt) + central
        // pressure (hPa), e.g. "Strong Typhoon · 80 kt · 950 hPa".
        "jma_typhoon" => ee_sources::jma_typhoon::typhoon_chip(&e.raw),
        // NZ Volcanic Alert Level (0–5) + ICAO aviation colour code, e.g.
        // "Alert Level 2 · Aviation Orange" — the official operational read.
        "geonet_volcano" => ee_sources::geonet_volcano::val_chip(&e.raw),
        // USGS Volcano Alert Level + ICAO aviation colour code, e.g.
        // "Alert Watch · Aviation Orange" — the operational read for US volcanoes.
        "usgs_volcano" => ee_sources::usgs_volcano::alert_chip(&e.raw),
        // PVMBG alert level (Waspada/Siaga/Awas) + latest VONA aviation colour, e.g.
        // "Alert Siaga (Watch) · Aviation Yellow" — the operational read for Indonesia.
        "magma_volcano" => ee_sources::magma_volcano::alert_chip(&e.raw),
        // NWS observed flood category, e.g. "Major flooding" / "Near flood stage" —
        // the baseline-relative read (stage already compared to the gauge's thresholds).
        "nwps_flood" => ee_sources::nwps_flood::flood_chip(&e.raw),
        // Marine warning name → the standardized ECCC mean-wind band it denotes, with
        // units ("Gale warning" → "34–47 kn winds"); non-wind hazards fall to the tier.
        "eccc_marine" => ee_sources::eccc_marine::warning_chip(&e.raw),
        "eccc_alerts" => pf("alert_type").and_then(Value::as_str).map(capitalize_first),
        "nws" => pf("severity")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && *s != "Unknown")
            .map(str::to_string),
        "eccc_aqhi" => pf("aqhi")
            .and_then(Value::as_f64)
            .map(|a| format!("AQHI {a:.0} · {}", ee_sources::eccc_aqhi::aqhi_risk(a))),
        "eonet" => pf("categories")
            .and_then(|c| c.as_array())
            .and_then(|c| c.first())
            .and_then(|c| c.get("title"))
            .and_then(Value::as_str)
            .map(str::to_string),
        "opensky" => {
            // `raw` is OpenSky's 17-element state vector (not a `properties` object), so
            // the operational read is by index: 7 baro-altitude (m), 8 on-ground, 9
            // velocity (m/s), 14 squawk. Without this arm every aircraft was a bare dot.
            // An emergency squawk is the only intrinsic alert, so it takes precedence;
            // otherwise altitude + ground speed in aviation units (ft / kn), or "On ground".
            let arr = e.raw.as_array();
            let at = |i: usize| arr.and_then(|a| a.get(i));
            let emerg = at(14).and_then(Value::as_str).and_then(|sq| match sq {
                "7500" => Some("Squawk 7500 · hijack"),
                "7600" => Some("Squawk 7600 · radio failure"),
                "7700" => Some("Squawk 7700 · emergency"),
                _ => None,
            });
            if let Some(label) = emerg {
                Some(label.to_string())
            } else if at(8).and_then(Value::as_bool).unwrap_or(false) {
                Some("On ground".to_string())
            } else {
                let alt = at(7).and_then(Value::as_f64).map(|m| format!("{:.0} ft", m * 3.28084));
                let spd = at(9).and_then(Value::as_f64).map(|ms| format!("{:.0} kn", ms * 1.94384));
                match (alt, spd) {
                    (Some(a), Some(s)) => Some(format!("{a} · {s}")),
                    (Some(a), None) => Some(a),
                    (None, Some(s)) => Some(s),
                    (None, None) => None,
                }
            }
        }
        _ => None,
    }
}

/// Capitalize the first character (provider labels often arrive all-lowercase).
fn capitalize_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// Friendly hazard name for a GDACS `eventtype` code (the popup chip shouldn't show
/// a raw two-letter code). Unknown codes pass through so a new GDACS type still reads.
fn gdacs_hazard(eventtype: &str) -> &str {
    match eventtype {
        "EQ" => "Earthquake",
        "TC" => "Cyclone",
        "FL" => "Flood",
        "DR" => "Drought",
        "VO" => "Volcano",
        "WF" => "Wildfire",
        "TS" => "Tsunami",
        other => other,
    }
}

/// Title-case an ALL-CAPS provider token ("MINOR" -> "Minor").
fn title_case(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase(),
        None => String::new(),
    }
}

/// Build the full map payload: live feeds (GeoJSON), GCRM theater flashpoints, the
/// toggleable layer registry, and the base-map catalogue.
///
/// The upstream feeds + layer/basemap catalogue are cached (TTL) and shared
/// across requests; the snapshot-derived flashpoints are merged in fresh on
/// every call, so live theater heat is never stale even on a cache hit.
pub async fn map_payload(snapshot: Option<Value>) -> Value {
    let mut payload = MAP_FEEDS_CACHE.get_or_refresh(feeds_payload).await;

    // Merge the live GCRM theater flashpoints over the cached feed base.
    let theater_features = build_theater_features(&snapshot);
    if let Some(counts) = payload.get_mut("counts").and_then(|c| c.as_object_mut()) {
        counts.insert("theaters".to_string(), json!(theater_features.len()));
    }
    payload["theaters"] = json!({ "type": "FeatureCollection", "features": theater_features });
    payload
}

/// The snapshot-independent half of the map payload: the live upstream feeds,
/// layer registry, and base-map catalogue. This is the expensive, cacheable
/// part — it performs all upstream I/O and never touches the live snapshot.
async fn feeds_payload() -> Value {
    use ee_sources::{
        acled::Acled, alberta511::Alberta511, cbsa_bwt::CbsaBwt, cwfis::Cwfis,
        cwfis_activefires::CwfisActiveFires, digitraffic_ais::DigitrafficAis, drivebc::DriveBc,
        eccc_alerts::EcccAlerts, eccc_aqhi::EcccAqhi, eccc_marine::EcccMarine, emsc::Emsc,
        eonet::Eonet, eqcanada::EqCanada, firms::Firms, gdacs::Gdacs,
        geonet_volcano::GeonetVolcano, gvp_volcano::GvpVolcano,
        healthmap::HealthMap, jma_typhoon::JmaTyphoon, magma_volcano::MagmaVolcano,
        navcanada::NavCanada, nhc::Nhc,
        nwps_flood::NwpsFlood, nws::Nws,
        ontario511::Ontario511,
        opensky::OpenSky, quebec511::Quebec511, ucdp_ged::UcdpGed, usgs::Usgs,
        usgs_volcano::UsgsVolcano,
    };

    // Pull the geocoded feeds concurrently, each time-boxed. Aircraft over BOTH
    // North America (incl. Canada) and Europe/Middle-East (the live theaters), for
    // dense, honest coverage on both sides of the Atlantic. NWS/USGS leave Canada
    // nearly blank, so four Canada-native feeds (ECCC alerts, ECCC air-quality, CWFIS
    // wildfire hotspots, NRCan earthquakes) fill the North-American gap; three global
    // feeds (EMSC quakes, GVP volcanoes, HealthMap outbreaks) populate the rest of the world.
    let (quakes, disasters, weather, ac_na, ac_eu, natural, ca_alerts, ca_fires, ca_quakes, ca_air, gl_quakes, gl_volc, gl_health, gl_fires, gl_conflict, on_roads, ca_marine, ca_active_fires, bc_roads, ab_roads, qc_roads, ca_borders, ca_notams, vessels, conflict, storms, typhoons, nz_volc, us_volc, id_volc, floods) = tokio::join!(
        fetch_one("usgs", Usgs { feed: "all_day".into() }, 8),
        fetch_one("gdacs", Gdacs, 10),
        fetch_one("nws", Nws, 10),
        fetch_one("opensky", OpenSky { bbox: Some((24.0, -140.0, 72.0, -52.0)) }, 9),
        fetch_one("opensky", OpenSky { bbox: Some((24.0, -11.0, 60.0, 60.0)) }, 9),
        // NASA EONET natural events (wildfires / storms / volcanoes), last 45 days.
        fetch_one("eonet", Eonet { days: 45 }, 10),
        // Environment Canada weather warnings/watches — the Canadian NWS equivalent.
        fetch_one("eccc_alerts", EcccAlerts, 9),
        // CWFIS satellite wildfire hotspots over Canada (last 24h).
        fetch_one("cwfis", Cwfis::default(), 10),
        // Earthquakes Canada (NRCan) — dense Canadian seismicity USGS drops, last 7d.
        fetch_one("eqcanada", EqCanada::default(), 9),
        // Environment Canada AQHI — air-quality stations (a live wildfire-smoke proxy).
        fetch_one("eccc_aqhi", EcccAqhi, 9),
        // EMSC global earthquakes — denser than USGS outside the Americas (last 24h, M2+).
        fetch_one("emsc", Emsc::default(), 9),
        // Smithsonian GVP — recent/ongoing volcanic eruptions worldwide.
        fetch_one("gvp_volcano", GvpVolcano::default(), 10),
        // HealthMap — global disease-outbreak clusters (fills Africa/Asia/S-America).
        // 2-day window keeps the response small/fast (the full window is several MB).
        fetch_one("healthmap", HealthMap { days: 2 }, 10),
        // NASA FIRMS — global satellite wildfire detections (dormant until FIRMS_MAP_KEY).
        fetch_one("firms", Firms::default(), 12),
        // ACLED — global armed-conflict events (dormant until ACLED_USERNAME/PASSWORD).
        fetch_one("acled", Acled::default(), 12),
        // Ontario 511 — provincial-highway road events (closures, collisions, roadwork).
        fetch_one("ontario511", Ontario511, 9),
        // Environment Canada marine warnings (Great Lakes — rings Ontario).
        fetch_one("eccc_marine", EcccMarine, 9),
        // CWFIS national active fires (NRCan/CIFFC) — agency fire ground-state (stage +
        // size), the incident complement to the satellite thermal hotspots above.
        fetch_one("cwfis_activefires", CwfisActiveFires, 15),
        // Provincial 511 road events — BC / Alberta / Québec highways (Ontario already
        // covered above), filling the Transport layer across the populous provinces.
        fetch_one("drivebc", DriveBc, 9),
        fetch_one("alberta511", Alberta511, 9),
        fetch_one("quebec511", Quebec511, 12),
        // CBSA land-border wait times (29 federal crossings, NB→BC).
        fetch_one("cbsa_bwt", CbsaBwt, 9),
        // NAV CANADA NOTAMs — airspace/aerodrome hazards at major Canadian airports.
        fetch_one("navcanada", NavCanada, 14),
        // Fintraffic AIS — live Baltic vessel positions (fills the Vessel layer).
        fetch_one("digitraffic_ais", DigitrafficAis, 15),
        // UCDP candidate GED — georeferenced conflict events (fills the Conflict layer).
        fetch_one("ucdp_ged", UcdpGed, 15),
        // NOAA NHC — active tropical cyclones (Atlantic/E-Pacific), live position + category.
        fetch_one("nhc", Nhc, 10),
        // JMA RSMC Tokyo — active typhoons (W-Pacific/South China Sea), the basin NHC
        // doesn't cover; index + per-system forecast, so allow a little more time.
        fetch_one("jma_typhoon", JmaTyphoon, 14),
        // GeoNet (GNS Science) — NZ Volcanic Alert Levels; official alert state for the
        // SW-Pacific volcanoes the global GVP eruption catalogue doesn't operationally cover.
        fetch_one("geonet_volcano", GeonetVolcano, 9),
        // USGS HANS — US/Alaska volcanic alert levels (joins elevated notices to the
        // US volcano catalogue for coords), the operational state GVP/EONET don't carry.
        fetch_one("usgs_volcano", UsgsVolcano, 12),
        // PVMBG / MAGMA Indonesia — Indonesian volcano alert levels (status ≥ Waspada)
        // + latest VONA aviation colour, the operational state for the world's most
        // volcanically active country (Path-B committed snapshot; refresh re-captures).
        fetch_one("magma_volcano", MagmaVolcano, 9),
        // NOAA NWPS — river gauges at/above flood stage (observed flood category, the
        // baseline-relative read), filling the river-flooding hazard no other feed carries.
        fetch_one("nwps_flood", NwpsFlood, 12),
    );

    let mut errors: Vec<String> = Vec::new();
    let mut counts = serde_json::Map::new();
    let mut feed_events: Vec<Event> = Vec::new();
    // Last-good store, so a transient empty/failed upstream doesn't silently blank a
    // whole layer (a CWFIS GeoServer hiccup used to zero out all of Canada's wildfires).
    let mut lg_guard = FEED_LAST_GOOD.lock().await;
    let last_good = lg_guard.get_or_insert_with(HashMap::new);
    let now = Instant::now();
    // Cap each feed so the payload can't balloon; the two OpenSky regions sum into
    // one "opensky" count. (events, optional error, source key, per-feed cap)
    let mut opensky_total = 0usize;
    for (mut evs, err, key, cap) in [
        (quakes.0, quakes.1, "usgs", 600usize),
        (disasters.0, disasters.1, "gdacs", 400),
        (weather.0, weather.1, "nws", 400),
        (ac_na.0, ac_na.1, "opensky", 500),
        (ac_eu.0, ac_eu.1, "opensky", 300),
        (natural.0, natural.1, "eonet", 600),
        (ca_alerts.0, ca_alerts.1, "eccc_alerts", 300),
        (ca_fires.0, ca_fires.1, "cwfis", 700),
        (ca_quakes.0, ca_quakes.1, "eqcanada", 400),
        (ca_air.0, ca_air.1, "eccc_aqhi", 200),
        (gl_quakes.0, gl_quakes.1, "emsc", 600),
        (gl_volc.0, gl_volc.1, "gvp_volcano", 200),
        (gl_health.0, gl_health.1, "healthmap", 300),
        (gl_fires.0, gl_fires.1, "firms", 1800),
        (gl_conflict.0, gl_conflict.1, "acled", 800),
        (on_roads.0, on_roads.1, "ontario511", 500),
        (ca_marine.0, ca_marine.1, "eccc_marine", 100),
        (ca_active_fires.0, ca_active_fires.1, "cwfis_activefires", 400),
        (bc_roads.0, bc_roads.1, "drivebc", 500),
        (ab_roads.0, ab_roads.1, "alberta511", 500),
        (qc_roads.0, qc_roads.1, "quebec511", 300),
        (ca_borders.0, ca_borders.1, "cbsa_bwt", 60),
        (ca_notams.0, ca_notams.1, "navcanada", 600),
        (vessels.0, vessels.1, "digitraffic_ais", 800),
        (conflict.0, conflict.1, "ucdp_ged", 800),
        (storms.0, storms.1, "nhc", 60),
        (typhoons.0, typhoons.1, "jma_typhoon", 60),
        (nz_volc.0, nz_volc.1, "geonet_volcano", 60),
        (us_volc.0, us_volc.1, "usgs_volcano", 60),
        (id_volc.0, id_volc.1, "magma_volcano", 150),
        (floods.0, floods.1, "nwps_flood", 400),
    ] {
        evs.truncate(cap);
        if let Some(e) = err {
            errors.push(e);
        }
        // Resilience: refresh last-good on a non-empty pull; on an empty one, reuse the
        // recent last-good (flagged stale) instead of caching a deceptive zero. Skipped
        // for the double-keyed "opensky" so its two regions don't fight over one slot.
        if key != "opensky" {
            if !evs.is_empty() {
                last_good.insert(key.to_string(), (now, evs.clone()));
            } else if let Some((at, prev)) = last_good.get(key) {
                if now.duration_since(*at) < LAST_GOOD_MAX_AGE {
                    let age = now.duration_since(*at).as_secs();
                    evs = prev.clone();
                    errors.push(format!(
                        "{key}: live feed empty — showing last-good {} ({age}s old)",
                        evs.len()
                    ));
                }
            }
        }
        if key == "opensky" {
            opensky_total += evs.len();
            counts.insert("opensky".to_string(), json!(opensky_total));
        } else {
            counts.insert(key.to_string(), json!(evs.len()));
        }
        feed_events.extend(evs);
    }
    drop(lg_guard);

    let mut feeds = ee_view::geojson::to_feature_collection(&feed_events);
    // Enrich each plotted feature with a human-readable value chip ("M2.7", "24 MW
    // fire power", "Warning", "AQHI 7 · High risk") pulled from the provider's raw
    // payload and matched back by event id — so the map popup conveys real meaning,
    // not an opaque normalized 0–1 severity.
    let details: HashMap<&str, String> = feed_events
        .iter()
        .filter_map(|e| feed_detail(e).map(|d| (e.id.as_str(), d)))
        .collect();
    if let Some(arr) = feeds.get_mut("features").and_then(|f| f.as_array_mut()) {
        for feat in arr {
            let id = feat.get("properties").and_then(|p| p.get("id")).and_then(|i| i.as_str());
            if let Some(d) = id.and_then(|id| details.get(id)) {
                feat["properties"]["detail"] = json!(d);
            }
        }
    }

    // Layer registry (ee-view) + a synthetic descriptor for the GCRM flashpoint layer.
    let mut layers: Vec<Value> = ee_view::layers::registry()
        .iter()
        .map(|d| serde_json::to_value(d).unwrap_or(Value::Null))
        .collect();
    layers.insert(
        0,
        json!({
            "id": "theaters", "label": "GCRM Flashpoints", "group": "security",
            "kind": "conflict", "color": "#e74c3c", "icon": "flashpoint",
            "default_visible": true
        }),
    );

    // Base-map catalogue (ee-view) + MapLibre-ready CARTO dark raster tiles.
    let dark = ee_view::basemap::STYLES
        .iter()
        .find(|s| s.id == "carto-dark-matter")
        .or_else(|| ee_view::basemap::STYLES.first());
    let tiles: Vec<String> = match dark {
        Some(s) => ["a", "b", "c", "d"]
            .iter()
            .map(|sub| s.url_template.replace("{s}", sub))
            .collect(),
        None => Vec::new(),
    };
    let styles: Vec<Value> = ee_view::basemap::STYLES
        .iter()
        .map(|s| serde_json::to_value(s).unwrap_or(Value::Null))
        .collect();

    json!({
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "basemap": {
            "default": "carto-dark-matter",
            "tiles": tiles,
            "attribution": dark.map(|s| s.attribution).unwrap_or(""),
            "max_zoom": dark.map(|s| s.max_zoom).unwrap_or(19),
            "styles": styles,
        },
        "layers": layers,
        "feeds": feeds,
        "counts": counts,
        "errors": errors,
    })
}

/// Compute the Finance Radar from the live Yahoo market stream, enriched with the
/// labels/colours the dashboard panel needs. Cached (TTL) so concurrent clients
/// share one Yahoo fetch rather than each tripping its rate limit.
pub async fn finance_payload() -> Value {
    FINANCE_CACHE.get_or_refresh(finance_payload_uncached).await
}

async fn finance_payload_uncached() -> Value {
    use ee_correlate::{radar, RadarParams};
    use ee_sources::yahoo::Yahoo;

    let (events, err) = fetch_one("yahoo", Yahoo::default(), 12).await;
    let r = radar(&events, &RadarParams::default());

    // Per-instrument movers — the actual market read the panel shows: each tracked
    // symbol's live price and signed daily move, biggest move first. The radar composite
    // is a useful one-line stress gauge, but on its own it told the operator nothing
    // actionable; these rows are the substance. The numbers are already in each Yahoo
    // event's `raw` meta — we just surface them instead of collapsing to a stress score.
    let mut instruments: Vec<Value> = events
        .iter()
        .filter_map(|e| {
            let m = &e.raw;
            let price = m.get("regularMarketPrice").and_then(Value::as_f64)?;
            let prev = m
                .get("chartPreviousClose")
                .or_else(|| m.get("previousClose"))
                .and_then(Value::as_f64)
                .unwrap_or(price);
            let pct = if prev != 0.0 { (price - prev) / prev * 100.0 } else { 0.0 };
            let symbol = m.get("symbol").and_then(Value::as_str).unwrap_or("");
            let name = m
                .get("shortName")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or(e.title.as_str());
            Some(json!({
                "name": instrument_name(symbol, name),
                "symbol": symbol,
                "price": price,
                "pct": pct,
                "url": e.url,
            }))
        })
        .collect();
    // Biggest absolute move first — the panel leads with whatever is actually moving.
    instruments.sort_by(|a, b| {
        let pa = a["pct"].as_f64().unwrap_or(0.0).abs();
        let pb = b["pct"].as_f64().unwrap_or(0.0).abs();
        pb.partial_cmp(&pa).unwrap_or(std::cmp::Ordering::Equal)
    });

    let segments: Vec<Value> = r
        .segments
        .iter()
        .map(|s| {
            json!({
                "segment": s.segment.label(),
                "intensity": s.intensity,
                "count": s.count,
                "peak": s.peak,
                "contribution": s.contribution,
            })
        })
        .collect();

    json!({
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "composite": r.composite,
        "level": r.level.label(),
        "level_color": r.level.color(),
        "dominant": r.dominant.map(|s| s.label()),
        "active_segments": r.active_segments(),
        "total_events": r.total_events,
        "instruments": instruments,
        "segments": segments,
        "errors": err.map(|e| vec![e]).unwrap_or_default(),
    })
}

/// Short, clean display name for a tracked Yahoo symbol; falls back to Yahoo's own
/// short name (or event title) for anything outside the default basket.
fn instrument_name<'a>(symbol: &str, fallback: &'a str) -> &'a str {
    match symbol {
        "^GSPC" => "S&P 500",
        "^IXIC" => "Nasdaq",
        "BTC-USD" => "Bitcoin",
        "ETH-USD" => "Ethereum",
        "CL=F" => "Crude Oil",
        "GC=F" => "Gold",
        "^TNX" => "10Y Yield",
        "EURUSD=X" => "EUR/USD",
        _ => fallback,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theater_coords_cover_named_theaters_and_skip_other() {
        for id in ["nato_russia", "us_iran", "us_china_taiwan", "india_pakistan", "korea"] {
            assert!(theater_coord(id).is_some(), "missing coord for {id}");
        }
        assert!(theater_coord("other").is_none());
    }

    #[test]
    fn rung_colors_cover_every_rung_distinctly() {
        use EscalationRung::*;
        let cols = [Stable, Tension, Crisis, LimitedWar, GreatPowerWar, Systemic].map(rung_color);
        // Every rung must map to a distinct colour — including the apex Systemic rung,
        // which the old heat-keyed palette collapsed into Great-Power-War red.
        for i in 0..cols.len() {
            for j in (i + 1)..cols.len() {
                assert_ne!(cols[i], cols[j], "rung colours must be distinct ({i} vs {j})");
            }
        }
        assert_eq!(rung_color(Stable), "#1D9E75");
        assert_eq!(rung_color(GreatPowerWar), "#7a0000");
    }

    #[test]
    fn instrument_names_clean_known_symbols_and_fall_back() {
        assert_eq!(instrument_name("^GSPC", "S&P 500 Index"), "S&P 500");
        assert_eq!(instrument_name("BTC-USD", "Bitcoin USD"), "Bitcoin");
        assert_eq!(instrument_name("^TNX", "CBOE Interest Rate 10 Year"), "10Y Yield");
        // An unknown symbol keeps Yahoo's own short name (the fallback).
        assert_eq!(instrument_name("ZZZZ", "Some Future"), "Some Future");
    }

    #[test]
    fn gdacs_chip_surfaces_alert_level_type_and_severity_text() {
        use chrono::Utc;
        use ee_core::{EventKind, Geo, Severity};
        let mk = |raw: Value| Event {
            id: "gdacs-x".into(),
            source_id: "gdacs".into(),
            kind: EventKind::Weather,
            title: "t".into(),
            time: Utc::now(),
            geo: Geo::new(0.0, 0.0),
            severity: Severity::new(0.6),
            url: None,
            raw,
        };
        // Full read: alert level + friendly hazard name + the unit-carrying severity text.
        let e = mk(json!({"properties": {"eventtype": "EQ", "alertlevel": "Orange",
            "severitydata": {"severitytext": "Magnitude 6.1M, Depth:10km"}}}));
        assert_eq!(feed_detail(&e).as_deref(), Some("Orange · Earthquake · Magnitude 6.1M, Depth:10km"));
        // No severity text -> alert level + hazard type still carry meaning (not a bare dot).
        let e = mk(json!({"properties": {"eventtype": "TC", "alertlevel": "Red"}}));
        assert_eq!(feed_detail(&e).as_deref(), Some("Red · Cyclone"));
        // A long severity sentence is dropped so the chip can't dump a paragraph.
        let long = "Death(s) reported and a very large population is affected across multiple provinces and regions";
        let e = mk(json!({"properties": {"eventtype": "FL", "alertlevel": "Green",
            "severitydata": {"severitytext": long}}}));
        assert_eq!(feed_detail(&e).as_deref(), Some("Green · Flood"));
    }

    #[test]
    fn opensky_chip_surfaces_altitude_speed_and_emergency() {
        use chrono::Utc;
        use ee_core::{EventKind, Geo, Severity};
        let mk = |raw: Value| Event {
            id: "ac".into(),
            source_id: "opensky".into(),
            kind: EventKind::Aircraft,
            title: "t".into(),
            time: Utc::now(),
            geo: Geo::new(0.0, 0.0),
            severity: Severity::new(0.1),
            url: None,
            raw,
        };
        // Airborne: index 7 baro-altitude (m)->ft, index 9 velocity (m/s)->kn.
        // 11000 m ≈ 36089 ft, 230 m/s ≈ 447 kn.
        let e = mk(json!(["abc","DLH456","Germany",1,1,8.5,50.1,11000.0,false,230.0,180.0,0,null,11200.0,"1000",false,0]));
        assert_eq!(feed_detail(&e).as_deref(), Some("36089 ft · 447 kn"));
        // An emergency squawk (index 14) wins over the altitude/speed read.
        let e = mk(json!(["def","N99","US",1,1,-95.4,29.8,9000.0,false,200.0,90.0,0,null,9100.0,"7700",false,0]));
        assert_eq!(feed_detail(&e).as_deref(), Some("Squawk 7700 · emergency"));
        // On the ground (index 8 = true): no airborne figures to surface.
        let e = mk(json!(["ghi","TAXI","US",1,1,-80.0,25.0,0.0,true,5.0,0.0,0,null,0.0,"1200",false,0]));
        assert_eq!(feed_detail(&e).as_deref(), Some("On ground"));
    }

    #[test]
    fn nhc_chip_surfaces_classification_category_and_wind() {
        use chrono::Utc;
        use ee_core::{EventKind, Geo, Severity};
        let mk = |raw: Value| Event {
            id: "nhc-x".into(),
            source_id: "nhc".into(),
            kind: EventKind::Weather,
            title: "t".into(),
            time: Utc::now(),
            geo: Geo::new(0.0, 0.0),
            severity: Severity::new(0.6),
            url: None,
            raw,
        };
        // Hurricane carries its Saffir–Simpson category; tropical storm just the wind.
        let e = mk(json!({"classification": "HU", "intensity": 75.0}));
        assert_eq!(feed_detail(&e).as_deref(), Some("Hurricane Cat 1 · 75 kt"));
        let e = mk(json!({"classification": "TS", "intensity": 45.0}));
        assert_eq!(feed_detail(&e).as_deref(), Some("Tropical Storm · 45 kt"));
    }

    #[test]
    fn jma_typhoon_chip_surfaces_grade_wind_and_pressure() {
        use chrono::Utc;
        use ee_core::{EventKind, Geo, Severity};
        let mk = |raw: Value| Event {
            id: "jma-x".into(),
            source_id: "jma_typhoon".into(),
            kind: EventKind::Weather,
            title: "t".into(),
            time: Utc::now(),
            geo: Geo::new(0.0, 0.0),
            severity: Severity::new(0.7),
            url: None,
            raw,
        };
        // Typhoon carries its JMA intensity grade plus wind + central pressure.
        let e = mk(json!({"category": "TY", "knots": 80.0, "pressure": 950.0}));
        assert_eq!(feed_detail(&e).as_deref(), Some("Strong Typhoon · 80 kt · 950 hPa"));
        // Sub-typhoon system: label + wind, no grade.
        let e = mk(json!({"category": "TS", "knots": 40.0}));
        assert_eq!(feed_detail(&e).as_deref(), Some("Tropical Storm · 40 kt"));
    }

    #[test]
    fn geonet_volcano_chip_surfaces_alert_level_and_aviation_colour() {
        use chrono::Utc;
        use ee_core::{EventKind, Geo, Severity};
        let mk = |raw: Value| Event {
            id: "geonet-val-x".into(),
            source_id: "geonet_volcano".into(),
            kind: EventKind::Volcano,
            title: "t".into(),
            time: Utc::now(),
            geo: Geo::new(0.0, 0.0),
            severity: Severity::new(0.55),
            url: None,
            raw,
        };
        // `raw` is the GeoNet feature's properties: level + aviation colour code.
        let e = mk(json!({"level": 2, "acc": "Orange"}));
        assert_eq!(feed_detail(&e).as_deref(), Some("Alert Level 2 · Aviation Orange"));
        // No aviation colour assigned -> the alert level stands alone.
        let e = mk(json!({"level": 1, "acc": ""}));
        assert_eq!(feed_detail(&e).as_deref(), Some("Alert Level 1"));
    }

    #[test]
    fn usgs_volcano_chip_surfaces_alert_level_and_aviation_colour() {
        use chrono::Utc;
        use ee_core::{EventKind, Geo, Severity};
        let mk = |raw: Value| Event {
            id: "usgs-volcano-x".into(),
            source_id: "usgs_volcano".into(),
            kind: EventKind::Volcano,
            title: "t".into(),
            time: Utc::now(),
            geo: Geo::new(0.0, 0.0),
            severity: Severity::new(0.8),
            url: None,
            raw,
        };
        // `raw` is the HANS elevated notice: ground alert level + aviation colour.
        let e = mk(json!({"alert_level": "WATCH", "color_code": "ORANGE"}));
        assert_eq!(feed_detail(&e).as_deref(), Some("Alert Watch · Aviation Orange"));
        // Unassigned ground level -> the aviation colour stands alone.
        let e = mk(json!({"alert_level": "UNASSIGNED", "color_code": "YELLOW"}));
        assert_eq!(feed_detail(&e).as_deref(), Some("Aviation Yellow"));
    }

    #[test]
    fn magma_volcano_chip_surfaces_alert_level_and_aviation_colour() {
        use chrono::Utc;
        use ee_core::{EventKind, Geo, Severity};
        let mk = |raw: Value| Event {
            id: "magma-volcano-x".into(),
            source_id: "magma_volcano".into(),
            kind: EventKind::Volcano,
            title: "t".into(),
            time: Utc::now(),
            geo: Geo::new(0.0, 0.0),
            severity: Severity::new(0.8),
            url: None,
            raw,
        };
        // `raw` is the MAGMA volcano record: PVMBG alert level (ga_status) + VONA colour.
        let e = mk(json!({"ga_status": 3, "vona": [{"no": 1, "cu_avcode": "YELLOW"}]}));
        assert_eq!(feed_detail(&e).as_deref(), Some("Alert Siaga (Watch) · Aviation Yellow"));
        // Waspada with no VONA -> the alert level stands alone.
        let e = mk(json!({"ga_status": 2, "vona": []}));
        assert_eq!(feed_detail(&e).as_deref(), Some("Alert Waspada (Advisory)"));
    }

    #[test]
    fn nwps_flood_chip_surfaces_observed_flood_category() {
        use chrono::Utc;
        use ee_core::{EventKind, Geo, Severity};
        let mk = |raw: Value| Event {
            id: "nwps-flood-x".into(),
            source_id: "nwps_flood".into(),
            kind: EventKind::Weather,
            title: "t".into(),
            time: Utc::now(),
            geo: Geo::new(0.0, 0.0),
            severity: Severity::new(0.8),
            url: None,
            raw,
        };
        // `raw` is the gauge feature's properties: the observed flood category.
        let e = mk(json!({"gaugelid": "FGON8", "status": "major"}));
        assert_eq!(feed_detail(&e).as_deref(), Some("Major flooding"));
        // Action stage reads as near-flood; casing is tolerated.
        let e = mk(json!({"gaugelid": "TULO2", "status": "Action"}));
        assert_eq!(feed_detail(&e).as_deref(), Some("Near flood stage"));
    }

    #[test]
    fn theater_features_placed_from_snapshot() {
        let snap = json!({
            "theaters": [
                {"theater_id": "us_iran", "label": "US/Iran", "rung": "limited_war",
                 "rung_label": "Limited War", "heat": 0.45, "trend": "rising", "event_count": 12},
                {"theater_id": "korea", "label": "Korea", "rung": "tension",
                 "rung_label": "Tension", "heat": 0.10, "trend": "stable", "event_count": 3},
                {"theater_id": "other", "label": "Other", "heat": 0.2}
            ]
        });
        let feats = build_theater_features(&Some(snap));
        // "other" has no coordinate -> dropped; the two placed theaters remain.
        assert_eq!(feats.len(), 2);
        let iran = &feats[0];
        assert_eq!(iran["properties"]["id"], "us_iran");
        assert_eq!(iran["properties"]["color"], "#c0392b"); // Limited War rung red
        // GeoJSON coordinate order is [lon, lat].
        let c = iran["geometry"]["coordinates"].as_array().unwrap();
        assert!((c[0].as_f64().unwrap() - 56.3).abs() < 1e-6);
        assert!((c[1].as_f64().unwrap() - 26.6).abs() < 1e-6);
        // No snapshot -> no features.
        assert!(build_theater_features(&None).is_empty());
    }

    #[test]
    fn marker_color_follows_authoritative_rung_not_heat() {
        // The engine's rung can sit ABOVE the heat-implied band: e.g. a great power is
        // involved so `rung_for` forced Great-Power War even though raw heat (0.45) lands
        // in the Limited-War band. The marker must take the RUNG's colour (matching its
        // rung_label), NOT a lesser colour re-derived from heat — otherwise the map would
        // understate exactly the apex theaters that matter most. A revert to a heat-keyed
        // palette fails this (heat 0.45 -> Limited War #c0392b, not GP-War #7a0000).
        let snap = json!({"theaters": [
            {"theater_id": "nato_russia", "label": "NATO/Russia", "rung": "great_power_war",
             "rung_label": "Great-Power War", "heat": 0.45},
            {"theater_id": "us_china_taiwan", "label": "Taiwan", "rung": "systemic",
             "rung_label": "Systemic War", "heat": 0.95}
        ]});
        let f = build_theater_features(&Some(snap));
        assert_eq!(f[0]["properties"]["color"], "#7a0000"); // GP-War rung, despite heat 0.45
        assert_eq!(f[0]["properties"]["rung_label"], "Great-Power War");
        // The apex Systemic rung (nuclear use) gets its own colour, not GP-War red.
        assert_eq!(f[1]["properties"]["color"], "#b5179e");
        assert_ne!(f[1]["properties"]["color"], f[0]["properties"]["color"]);
    }

    #[test]
    fn theater_feature_carries_the_persistence_floor_flags() {
        // The map marker is the only operator surface that must not paint a floor-held
        // theater (a remembered war-state carried through a news gap) identical to a
        // live-hot one — the persistence-floor honesty contract the ladder chip and hero
        // already enforce. The engine's `held_by_floor` + `fresh_rung_label` must reach the
        // feature so the popup can flag it; a future edit dropping them fails this.
        let snap = json!({"theaters": [
            // Held: heat held above the fresh read; fresh evidence has decayed to Crisis.
            {"theater_id": "us_iran", "label": "US/Iran", "rung": "limited_war",
             "rung_label": "Limited War", "heat": 0.45, "trend": "stable", "event_count": 4,
             "held_by_floor": true, "fresh_rung_label": "Crisis"},
            // Live-hot: not floor-held, fresh read equals the displayed rung.
            {"theater_id": "korea", "label": "Korea", "rung": "crisis",
             "rung_label": "Crisis", "heat": 0.30, "trend": "rising", "event_count": 9,
             "held_by_floor": false, "fresh_rung_label": "Crisis"}
        ]});
        let f = build_theater_features(&Some(snap));
        assert_eq!(f[0]["properties"]["held_by_floor"], true);
        assert_eq!(f[0]["properties"]["fresh_rung_label"], "Crisis");
        assert_eq!(f[1]["properties"]["held_by_floor"], false);
        // A pre-floor snapshot (no flags) must default to not-held, never panic.
        let old = json!({"theaters": [
            {"theater_id": "korea", "label": "Korea", "rung": "tension",
             "rung_label": "Tension", "heat": 0.10}
        ]});
        let g = build_theater_features(&Some(old));
        assert_eq!(g[0]["properties"]["held_by_floor"], false);
        assert_eq!(g[0]["properties"]["fresh_rung_label"], "");
    }

    #[tokio::test]
    async fn payload_cache_coalesces_until_ttl_expires() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = AtomicUsize::new(0);
        let bump = || async { json!(calls.fetch_add(1, Ordering::SeqCst)) };

        // Long TTL: first miss builds, the next hit is served from cache.
        let cache = PayloadCache::new(StdDuration::from_secs(60));
        assert_eq!(cache.get_or_refresh(bump).await, json!(0));
        assert_eq!(cache.get_or_refresh(bump).await, json!(0));
        assert_eq!(calls.load(Ordering::SeqCst), 1, "second call should hit cache");

        // Zero TTL: every call is stale, so it rebuilds each time.
        let calls2 = AtomicUsize::new(0);
        let bump2 = || async { json!(calls2.fetch_add(1, Ordering::SeqCst)) };
        let fresh = PayloadCache::new(StdDuration::from_secs(0));
        assert_eq!(fresh.get_or_refresh(bump2).await, json!(0));
        assert_eq!(fresh.get_or_refresh(bump2).await, json!(1));
        assert_eq!(calls2.load(Ordering::SeqCst), 2, "expired entry must rebuild");
    }

    // Live smoke test (network) — run explicitly: `cargo test osint -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "hits live USGS/GDACS/NWS/OpenSky/Yahoo endpoints"]
    async fn live_map_and_finance_payloads() {
        let map = map_payload(None).await;
        let feeds = map["feeds"]["features"].as_array().unwrap();
        let layers = map["layers"].as_array().unwrap();
        println!(
            "MAP: {} feed features, {} layers, counts={}, errors={}",
            feeds.len(),
            layers.len(),
            map["counts"],
            map["errors"]
        );
        assert!(!map["basemap"]["tiles"].as_array().unwrap().is_empty());
        assert!(layers.len() >= 10);
        // The feed collection should carry geocoded points from at least one provider.
        assert!(!feeds.is_empty(), "no feed features returned");

        let fin = finance_payload().await;
        println!(
            "FINANCE: composite={} level={} dominant={} active={}/7",
            fin["composite"], fin["level"], fin["dominant"], fin["active_segments"]
        );
        assert_eq!(fin["segments"].as_array().unwrap().len(), 7);
    }
}
