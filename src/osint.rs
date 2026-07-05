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
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
    /// Single-flight guard: true while a background refresh is in flight, so a stale read
    /// triggers at most one upstream rebuild no matter how many viewers arrive.
    refreshing: AtomicBool,
}

impl PayloadCache {
    const fn new(ttl: StdDuration) -> Self {
        Self {
            inner: Mutex::const_new(None),
            ttl,
            refreshing: AtomicBool::new(false),
        }
    }

    /// Stale-while-revalidate. A FRESH entry returns immediately. A STALE entry returns the
    /// stale value immediately and kicks off a single background refresh — so only the very
    /// first COLD load ever blocks a caller. The old design held the lock across the entire
    /// multi-second upstream fan-out, so on every TTL expiry one unlucky viewer (and every
    /// other concurrent viewer queued behind the lock) ate the full fan-out latency — a
    /// periodic head-of-line stall for all viewers. (audit osint-1 / xcut_net-3)
    /// Reset-on-drop guard for the `refreshing` single-flight flag: the winning
    /// builder can be CANCELLED (an axum handler future is dropped when its client
    /// disconnects mid-fan-out — the eyes-gate timeout does exactly this) or panic;
    /// either used to leave the flag stuck true forever, wedging every subsequent
    /// cold caller in the poll loop with a takeover branch that could never fire.
    /// Drop runs on success, error, panic-unwind AND future-drop, so the flag is
    /// always released. (xhigh review finding 2)
    fn flight_guard(&'static self) -> impl Drop {
        struct FlightGuard(&'static std::sync::atomic::AtomicBool);
        impl Drop for FlightGuard {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        FlightGuard(&self.refreshing)
    }

    async fn get_or_refresh<F, Fut>(&'static self, build: F) -> Value
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Value> + Send + 'static,
    {
        let snapshot = { self.inner.lock().await.clone() };
        match snapshot {
            Some((at, v)) if at.elapsed() < self.ttl => v, // fresh hit
            Some((_, v)) => {
                self.spawn_refresh(build); // serve stale now, revalidate in the background
                v
            }
            None => {
                // Cold start: exactly ONE caller runs the build (reusing the `refreshing`
                // single-flight flag); every other cold caller waits for its value instead
                // of launching a concurrent full fan-out. A deploy boot used to run 2-3
                // of them at once (prewarm task + eyes-gate page load + first viewer),
                // tripling the load and burning rate-limited upstreams (opensky 429s).
                if self
                    .refreshing
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    let _flight = self.flight_guard(); // releases on cancel/panic too
                    let fresh = build().await;
                    *self.inner.lock().await = Some((Instant::now(), fresh.clone()));
                    fresh
                } else {
                    // Another cold build is in flight — poll for its result. Bounded by
                    // that build's own per-feed timeouts; the interval is far below any
                    // caller's patience and costs nothing measurable.
                    loop {
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                        if let Some((_, v)) = self.inner.lock().await.clone() {
                            return v;
                        }
                        if !self.refreshing.load(Ordering::Acquire) {
                            // Builder finished without storing (cannot happen today) or
                            // panicked: take over the build rather than spin forever.
                            if self
                                .refreshing
                                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                                .is_ok()
                            {
                                let _flight = self.flight_guard();
                                let fresh = build().await;
                                *self.inner.lock().await = Some((Instant::now(), fresh.clone()));
                                return fresh;
                            }
                        }
                    }
                }
            }
        }
    }

    /// Spawn at most one background rebuild (single-flight via `refreshing`), swapping the
    /// fresh value in when it completes. Never blocks the caller.
    fn spawn_refresh<F, Fut>(&'static self, build: F)
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Value> + Send + 'static,
    {
        if self
            .refreshing
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return; // a refresh is already in flight
        }
        tokio::spawn(async move {
            let _flight = self.flight_guard(); // a panicking build must not wedge refreshes
            let fresh = build().await;
            *self.inner.lock().await = Some((Instant::now(), fresh));
        });
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

/// OpenSky fetch window: (last-good key, (lamin, lomin, lamax, lomax), event cap).
type OpenskyWindow = (&'static str, (f64, f64, f64, f64), usize);
/// The four aircraft windows the map cares about. Anonymous OpenSky credits only
/// afford two boxed requests per rebuild, but the flashpoint theaters span four
/// regions — the old fixed NA + EU/ME pair left Korea and the Taiwan Strait (3 of
/// 5 theaters) with zero aircraft coverage. Each 180s SWR rebuild now fetches ONE
/// pair ([`opensky_phase_windows`]: even = Atlantic, odd = Asian) and the payload
/// unions every window's most recent batch, so all four stay populated.
const OPENSKY_WINDOWS: [OpenskyWindow; 4] = [
    ("opensky@na", (24.0, -140.0, 72.0, -52.0), 500), // North America (incl. Canada)
    ("opensky@eu", (24.0, -11.0, 60.0, 60.0), 300),   // Europe / Middle East
    ("opensky@kr", (32.0, 122.0, 44.0, 134.0), 300),  // Korean peninsula / Sea of Japan
    ("opensky@tw", (20.0, 116.0, 27.5, 124.0), 300),  // Taiwan / Taiwan Strait
];
/// How long an off-phase window's last-good batch stays on the map (~2 rebuild
/// cycles + slack). Deliberately far below [`LAST_GOOD_MAX_AGE`]: aircraft move,
/// so a 30-minute-old "position" would be a lie, not resilience — and every dot's
/// popup shows its own fix time regardless.
const OPENSKY_WINDOW_MAX_AGE: StdDuration = StdDuration::from_secs(480);
/// Monotone rebuild counter driving the OpenSky window rotation.
static OPENSKY_PHASE: AtomicUsize = AtomicUsize::new(0);

/// The pair of OpenSky windows fetched on a given rebuild (counter taken mod 2:
/// even = Atlantic pair, odd = Asian pair). Pure — coverage is test-locked.
fn opensky_phase_windows(counter: usize) -> (&'static OpenskyWindow, &'static OpenskyWindow) {
    if counter.is_multiple_of(2) {
        (&OPENSKY_WINDOWS[0], &OPENSKY_WINDOWS[1])
    } else {
        (&OPENSKY_WINDOWS[2], &OPENSKY_WINDOWS[3])
    }
}

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
/// Marker colour for a missing/unparseable rung: a conspicuous neutral grey, NOT the
/// safest Stable green. A snapshot is always internally generated with a known rung today,
/// so this is a latent guard — but defaulting a parse miss to "calm" would contradict this
/// module's anti-understatement thesis (the whole reason the marker colours by rung). (audit osint-2)
const UNKNOWN_RUNG_COLOR: &str = "#6b7280";

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
        // A missing/unparseable rung is coloured a conspicuous UNKNOWN grey (never the
        // safest Stable green) so a parse miss can never read as "calm". (audit osint-2)
        let rung: Option<EscalationRung> = t
            .get("rung")
            .and_then(|r| serde_json::from_value(r.clone()).ok());
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
                "color": rung.map(rung_color).unwrap_or(UNKNOWN_RUNG_COLOR),
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
        // NGA ASAM hostile-act report: the escalation class + vessel targeted, e.g.
        // "Boarding · Bulk Carrier" / "Armed attack · Chemical Tanker".
        "asam" => ee_sources::asam::asam_chip(&e.raw),
        "ucdp_ged" => {
            let best = e.raw.get("best").and_then(Value::as_f64).unwrap_or(0.0);
            let ty = e.raw.get("type").and_then(Value::as_str).unwrap_or("Conflict");
            Some(if best >= 1.0 { format!("{best:.0} killed · {ty}") } else { ty.to_string() })
        }
        // ACLED Admin-1 trailing-window intensity, e.g. "41 events · 66 fatalities ·
        // Air/drone strike" — the regional conflict-heat read behind the centroid dot.
        "acled_aggregated" => ee_sources::acled_aggregated::intensity_chip(&e.raw),
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
            // Flood-class events carry a literal "Magnitude 0" (floods have no magnitude)
            // — a nonsense number, so the level + hazard type stand alone instead.
            let detail = pf("severitydata")
                .and_then(|s| s.get("severitytext"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty() && s.chars().count() <= 60)
                .filter(|s| !matches!(*s, "Magnitude 0" | "Magnitude 0M"));
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
        // Avalanche Canada current-day danger rating per elevation band, e.g.
        // "Alpine Considerable · Treeline Moderate · Below Low" (North American scale).
        "avalanche_ca" => ee_sources::avalanche_ca::danger_chip(&e.raw),
        // AWC international SIGMET: qualified en-route aviation hazard + flight-level
        // band, e.g. "Severe Turbulence · FL170–330" / "Embedded Thunderstorms · to FL430".
        "awc_sigmet" => ee_sources::awc_sigmet::sigmet_chip(&e.raw),
        // SPC confirmed severe-storm report: the hazard + magnitude with units, e.g.
        // "EF2 Tornado" / "2.75 in hail" / "70 mph wind" — the ground-truth read.
        "spc_storm_reports" => ee_sources::spc_storm_reports::report_chip(&e.raw),
        // BMKG felt-earthquake: peak MMI intensity + magnitude (+ tsunami potential),
        // e.g. "Felt MMI IV · M4.8" / "Felt MMI VI · M6.2 · Tsunami Siaga".
        "bmkg_quake" => ee_sources::bmkg_quake::felt_chip(&e.raw),
        // JMA earthquake: peak JMA seismic-intensity (Shindo) + magnitude, e.g.
        // "Shindo 5+ · M6.1" — the human-impact read the raw catalogues don't carry.
        "jma_quake" => ee_sources::jma_quake::quake_chip(&e.raw),
        // GeoNet felt earthquake: computed MMI shaking + magnitude, e.g.
        // "Felt MMI 5 · M5.9" — the human-impact read for NZ / the SW-Pacific.
        "geonet_quake" => ee_sources::geonet_quake::quake_chip(&e.raw),
        // BfS ODL gamma dose rate above natural background: the µSv/h reading + band,
        // e.g. "0.45 µSv/h · Above normal" / "3.10 µSv/h · High" — a universal-baseline
        // radiation read, not a raw scalar.
        "odlinfo" => ee_sources::odlinfo::dose_chip(&e.raw),
        // STUK/FMI (Finland) external radiation dose rate above background: the µSv/h
        // reading + band, e.g. "0.45 µSv/h · Above normal" — same universal-baseline
        // read as odlinfo, over the NATO/Russia frontline.
        "stuk_radiation" => ee_sources::stuk_radiation::dose_chip(&e.raw),
        // IRSN/ASNR Téléray (France) ambient gamma dose rate above natural background:
        // the µSv/h reading + band, e.g. "0.45 µSv/h · Above normal" — same
        // universal-baseline read as odlinfo/stuk, over Europe's largest nuclear power.
        "teleray" => ee_sources::teleray::dose_chip(&e.raw),
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

/// Order a feed's batch so a cap keeps the dots that MATTER: severity descending,
/// ties broken newest-first. `evs.truncate(cap)` used to cut in raw provider order,
/// silently dropping arbitrary — possibly highest-severity — tails on every feed
/// that runs at its cap (7 feeds live at audit time).
fn sort_for_cap(evs: &mut [Event]) {
    evs.sort_by(|a, b| {
        b.severity
            .value()
            .partial_cmp(&a.severity.value())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.time.cmp(&a.time))
    });
}

// ── Cross-feed earthquake dedup ──────────────────────────────────────────────
// The same physical quake arrives from up to four catalogues at once (live audit:
// an M6.1 near Taiwan plotted 4× — USGS + EMSC + JMA + GDACS — plus 44 identical
// USGS↔EMSC pairs). One quake must be one dot.

/// Dedup rank for a quake catalogue (lower wins). Intensity-graded national feeds
/// beat the global detection catalogues — their Shindo/MMI grade is the human-impact
/// read the raw catalogues don't carry — then USGS > EMSC > GDACS (whose quake entry
/// duplicates the others and its chip adds nothing). Non-quake sources: None.
fn quake_feed_rank(source_id: &str) -> Option<u8> {
    match source_id {
        "jma_quake" | "geonet_quake" | "bmkg_quake" | "eqcanada" => Some(0),
        "usgs" => Some(1),
        "emsc" => Some(2),
        "gdacs" => Some(3),
        _ => None,
    }
}

/// Two catalogue entries describe the same quake when their origin times are within
/// 90 s and their epicentres within ~0.3° (agency hypocentre solutions differ a
/// little; real distinct quakes essentially never coincide this tightly).
const QUAKE_DEDUP_TIME_S: i64 = 90;
const QUAKE_DEDUP_DEG: f64 = 0.3;

fn same_quake(a: &Event, b: &Event) -> bool {
    if (a.time - b.time).num_seconds().abs() > QUAKE_DEDUP_TIME_S {
        return false;
    }
    let (Some(ga), Some(gb)) = (a.geo, b.geo) else { return false };
    // Longitude difference wraps at ±180° (Fiji/Kermadec quakes straddle it).
    let mut dlon = (ga.lon - gb.lon).abs();
    if dlon > 180.0 {
        dlon = 360.0 - dlon;
    }
    (ga.lat - gb.lat).abs() <= QUAKE_DEDUP_DEG && dlon <= QUAKE_DEDUP_DEG
}

/// Magnitude carried by a global detection-catalogue entry, for the merged chip.
fn quake_magnitude(e: &Event) -> Option<f64> {
    match e.source_id.as_str() {
        "usgs" | "emsc" => e.raw.get("properties").and_then(|p| p.get("mag")).and_then(Value::as_f64),
        _ => None,
    }
}

/// Magnitude for the DEDUP GUARD — read from every catalogue's raw shape: usgs/emsc
/// (`properties.mag`), the intensity-graded national feeds (`magnitude`), eqcanada
/// (`mag`), gdacs (`properties.severitydata.severity` — the magnitude on EQ features).
/// Unlike [`quake_magnitude`] (global-chip source only) this exists to VETO merges:
/// an unreadable magnitude means the pair cannot be verified as one event.
fn quake_guard_magnitude(e: &Event) -> Option<f64> {
    quake_magnitude(e)
        .or_else(|| e.raw.get("magnitude").and_then(Value::as_f64))
        .or_else(|| e.raw.get("mag").and_then(Value::as_f64))
        .or_else(|| {
            e.raw
                .get("properties")
                .and_then(|p| p.get("severitydata"))
                .and_then(|s| s.get("severity"))
                .and_then(Value::as_f64)
        })
}

/// The intensity segments of a national feed's chip — its own magnitude reading
/// removed ("Shindo 3 · M6.4" -> "Shindo 3"; "Felt MMI VI · M6.2 · Tsunami Siaga"
/// -> "Felt MMI VI · Tsunami Siaga") so it can be recombined with the global
/// catalogue's magnitude. None when nothing but a magnitude remains (eqcanada).
fn quake_intensity_part(e: &Event) -> Option<String> {
    let chip = feed_detail(e)?;
    let parts: Vec<&str> = chip
        .split(" · ")
        .filter(|s| !(s.starts_with('M') && s[1..].starts_with(|c: char| c.is_ascii_digit())))
        .collect();
    if parts.is_empty() { None } else { Some(parts.join(" · ")) }
}

/// Post-join cross-feed quake dedup over the assembled event list. Groups quake
/// entries within [`QUAKE_DEDUP_TIME_S`] / [`QUAKE_DEDUP_DEG`] of each other, keeps
/// the best-ranked entry per group ([`quake_feed_rank`] — so the surviving dot
/// carries the human-impact read and its own source link) and drops the rest
/// (GDACS quake entries survive only when no other catalogue saw the event).
/// Returns chip overrides for kept NATIONAL entries that had a global sibling:
/// "M6.1 · Shindo 3" — magnitude from the global catalogue (USGS over EMSC),
/// intensity from the national one. Pure; test-locked below.
fn dedup_earthquakes(events: &mut Vec<Event>) -> HashMap<String, String> {
    // Dedup-able quake entries, best rank first (stable, so input order breaks ties).
    let mut idx: Vec<usize> = (0..events.len())
        .filter(|&i| {
            events[i].kind == ee_core::EventKind::Earthquake
                && events[i].geo.is_some()
                && quake_feed_rank(&events[i].source_id).is_some()
        })
        .collect();
    idx.sort_by_key(|&i| quake_feed_rank(&events[i].source_id).unwrap_or(u8::MAX));

    let mut claimed = vec![false; events.len()]; // true = dropped as a duplicate
    let mut overrides: HashMap<String, String> = HashMap::new();
    for (n, &i) in idx.iter().enumerate() {
        if claimed[i] {
            continue;
        }
        // `i` is this group's keeper; claim every later-ranked sibling that matches,
        // remembering the best global magnitude among them for the merged chip.
        let mut global_mag: Option<(u8, f64)> = None; // (rank, magnitude), lower rank wins
        for &j in idx.iter().skip(n + 1) {
            if claimed[j] || !same_quake(&events[i], &events[j]) {
                continue;
            }
            // A catalogue does not duplicate itself: two rows from ONE feed inside the
            // time/distance window are a mainshock + immediate aftershock, not the same
            // event twice — both stay. And the magnitudes must agree (±0.7) before
            // merging, so a real M5.6 aftershock is not swallowed by an M7.0 mainshock
            // that happens to sit within 0.3°/90 s. The guard reads EVERY catalogue's
            // magnitude (it used to see only usgs/emsc, so any national keeper merged
            // its siblings unchecked) and fails CLOSED on an unreadable one — an
            // unverifiable pair keeps both dots rather than risking a swallowed event.
            if events[j].source_id == events[i].source_id {
                continue;
            }
            // Magnitude scales differ across agencies (JMA Mj / GeoNet ML / NRCan
            // MN vs USGS-EMSC Mw): great events diverge by ~1 unit between scales
            // (Tohoku: Mj 8.4 vs Mw 9.0), so a strict ±0.7 across catalogues
            // re-introduced the double-plot for exactly the biggest quakes (xhigh
            // review finding 7). Same-family pairs (both global Mw-reporting
            // catalogues) keep ±0.7; cross-scale pairs get ±1.2 — still far below
            // the mainshock/aftershock gaps the guard exists to keep apart.
            let global = |e: &Event| matches!(e.source_id.as_str(), "usgs" | "emsc" | "gdacs");
            let tol = if global(&events[i]) && global(&events[j]) { 0.7 } else { 1.2 };
            match (quake_guard_magnitude(&events[i]), quake_guard_magnitude(&events[j])) {
                (Some(mi), Some(mj)) if (mi - mj).abs() <= tol => {}
                _ => continue,
            }
            claimed[j] = true;
            if let Some(m) = quake_magnitude(&events[j]) {
                let r = quake_feed_rank(&events[j].source_id).unwrap_or(u8::MAX);
                if global_mag.is_none_or(|(br, _)| r < br) {
                    global_mag = Some((r, m));
                }
            }
        }
        // Merged chip only when a national keeper has both a global magnitude and an
        // intensity read of its own — otherwise its native chip already says it all.
        if quake_feed_rank(&events[i].source_id) == Some(0) {
            if let (Some((_, mag)), Some(intensity)) = (global_mag, quake_intensity_part(&events[i])) {
                overrides.insert(events[i].id.clone(), format!("M{mag:.1} · {intensity}"));
            }
        }
    }
    let mut k = 0;
    events.retain(|_| {
        let keep = !claimed[k];
        k += 1;
        keep
    });
    overrides
}

/// Per-`source_id` count of features that actually made it into the GeoJSON — the
/// post-drop / post-dedup complement to the raw fetch `counts`. Pure.
fn plotted_counts(feeds: &Value) -> serde_json::Map<String, Value> {
    let mut plotted = serde_json::Map::new();
    let Some(arr) = feeds.get("features").and_then(|f| f.as_array()) else {
        return plotted;
    };
    for feat in arr {
        let sid = feat
            .get("properties")
            .and_then(|p| p.get("source_id"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if sid.is_empty() {
            continue;
        }
        let e = plotted.entry(sid.to_string()).or_insert(json!(0u64));
        *e = json!(e.as_u64().unwrap_or(0) + 1);
    }
    plotted
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
    // Theater markers always render (they're built with coordinates), so their
    // plotted count equals the fetched count — kept in both maps for symmetry.
    if let Some(plotted) = payload.get_mut("plotted").and_then(|c| c.as_object_mut()) {
        plotted.insert("theaters".to_string(), json!(theater_features.len()));
    }
    payload["theaters"] = json!({ "type": "FeatureCollection", "features": theater_features });
    payload
}

/// The snapshot-independent half of the map payload: the live upstream feeds,
/// layer registry, and base-map catalogue. This is the expensive, cacheable
/// part — it performs all upstream I/O and never touches the live snapshot.
async fn feeds_payload() -> Value {
    use ee_sources::{
        acled_aggregated::AcledAggregated, alberta511::Alberta511, avalanche_ca::AvalancheCa, awc_sigmet::AwcSigmet, bmkg_quake::BmkgQuake, cbsa_bwt::CbsaBwt, cwfis::Cwfis,
        cwfis_activefires::CwfisActiveFires, digitraffic_ais::DigitrafficAis, drivebc::DriveBc,
        eccc_alerts::EcccAlerts, eccc_aqhi::EcccAqhi, eccc_marine::EcccMarine, emsc::Emsc,
        eonet::Eonet, eqcanada::EqCanada, firms::Firms, gdacs::Gdacs,
        geonet_quake::GeonetQuake, geonet_volcano::GeonetVolcano, gvp_volcano::GvpVolcano,
        healthmap::HealthMap, jma_quake::JmaQuake, jma_typhoon::JmaTyphoon, magma_volcano::MagmaVolcano,
        navcanada::NavCanada, nhc::Nhc,
        nwps_flood::NwpsFlood, nws::Nws, odlinfo::Odlinfo,
        ontario511::Ontario511,
        opensky::OpenSky, quebec511::Quebec511, spc_storm_reports::SpcStormReports, stuk_radiation::StukRadiation,
        teleray::Teleray,
        ucdp_ged::UcdpGed, usgs::Usgs,
        usgs_volcano::UsgsVolcano,
    };

    // Pull the geocoded feeds concurrently, each time-boxed. Aircraft rotate across
    // the four OPENSKY_WINDOWS (two per rebuild — Atlantic pair / Asian pair — see
    // opensky_phase_windows), so Korea and the Taiwan Strait are covered, not just
    // NA + EU/ME. NWS/USGS leave Canada nearly blank, so four Canada-native feeds
    // (ECCC alerts, ECCC air-quality, CWFIS wildfire hotspots, NRCan earthquakes)
    // fill the North-American gap; three global feeds (EMSC quakes, GVP volcanoes,
    // HealthMap outbreaks) populate the rest of the world.
    let (win_a, win_b) = opensky_phase_windows(OPENSKY_PHASE.fetch_add(1, Ordering::Relaxed));
    let (quakes, disasters, weather, ac_a, ac_b, natural, ca_alerts, ca_fires, ca_quakes, ca_air, gl_quakes, gl_volc, gl_health, gl_fires, on_roads, ca_marine, ca_active_fires, bc_roads, ab_roads, qc_roads, ca_borders, ca_notams, vessels, conflict, conflict_agg, storms, typhoons, nz_volc, us_volc, id_volc, floods, avalanche, sigmets, storm_reports, id_felt, jp_felt, nz_felt, de_radiation, fi_radiation, fr_radiation) = tokio::join!(
        fetch_one("usgs", Usgs { feed: "all_day".into() }, 8),
        fetch_one("gdacs", Gdacs, 10),
        fetch_one("nws", Nws, 10),
        fetch_one("opensky", OpenSky { bbox: Some(win_a.1) }, 9),
        fetch_one("opensky", OpenSky { bbox: Some(win_b.1) }, 9),
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
        // (The live `acled` fetch was removed 2026-07-03: ACLED's event API is
        //  license-gated — confirmed permanent 2026-06-14 — so it burned a 12s fetch
        //  slot and a perpetual errors[] "HTTP 403" every rebuild for a guaranteed
        //  failure. The free aggregated Path-B snapshot below carries the modality.)
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
        // (The live `asam` fetch was removed 2026-07-04, hours after adoption: NGA's
        //  MSI API no longer serves the product — `/api/publications/asam` returns an
        //  application-level 404 with a valid session, and the Esri Living Atlas
        //  partner archive froze at 2024-06-25 — so it burned a 12s fetch slot and a
        //  perpetual errors[] "HTTP 404" every rebuild for a guaranteed failure. The
        //  connector + chip stay for the day a live successor surfaces.)
        // UCDP candidate GED — georeferenced conflict events (fills the Conflict layer).
        fetch_one("ucdp_ged", UcdpGed, 15),
        // ACLED Aggregated — weekly Admin-1 conflict intensity (events + fatalities,
        // centroid-mapped), ACLED's free no-key product; a regional-heat complement to
        // UCDP's discrete events (Path-B committed snapshot; refresh re-downloads).
        fetch_one("acled_aggregated", AcledAggregated, 9),
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
        // Avalanche Canada — public avalanche-forecast danger ratings (joins bulletins
        // to region polygons), the snow-avalanche hazard no other feed carries. Seasonal:
        // off-season regions carry no rating and drop, so summer yields 0 events.
        fetch_one("avalanche_ca", AvalancheCa, 14),
        // NOAA AWC international SIGMETs — en-route aviation hazards (convective,
        // turbulence, icing, volcanic ash, tropical cyclone) per FIR worldwide, the
        // aviation-weather modality NWS/ECCC ground warnings and NHC/JMA tracks don't carry.
        fetch_one("awc_sigmet", AwcSigmet, 10),
        // NOAA SPC — confirmed severe-storm reports today (tornado / large hail /
        // damaging wind), plotted at each report's lat/lon: the ground-truth
        // severe-convective occurrences NWS warnings (forecast) and the cyclone /
        // flood / aviation feeds don't carry. Three small CSVs, so allow a little time.
        fetch_one("spc_storm_reports", SpcStormReports, 12),
        // BMKG (Indonesia / InaTEWS) — recent FELT earthquakes with their Modified-Mercalli
        // (MMI) intensity + the national tsunami-potential flag, plotted at each quake's
        // lat/lon: the human-impact + tsunami modality the raw USGS/EMSC detection
        // catalogues don't carry, over Indonesia/SE-Asia.
        fetch_one("bmkg_quake", BmkgQuake, 10),
        // JMA (Japan) — recent earthquakes graded by the national Shindo seismic-intensity
        // scale (0–7), deduped by event id to the loudest bulletin: the human-impact /
        // intensity modality the raw USGS/EMSC detection catalogues don't carry, over
        // Japan / the NW-Pacific. Quakes with no observed Shindo or no hypocentre drop.
        fetch_one("jma_quake", JmaQuake, 10),
        // GeoNet (GNS Science) — recent NZ earthquakes with a computed felt MMI ≥ 3,
        // plotted at each quake's lat/lon: the human-impact / intensity modality the raw
        // USGS/EMSC detection catalogues don't carry, over New Zealand / the SW-Pacific.
        // Sub-felt (MMI < 3) and retracted quakes drop, so a quiet window = 0 events.
        fetch_one("geonet_quake", GeonetQuake, 10),
        // BfS ODL (Germany) — ambient gamma dose rate; only stations elevated above
        // natural background (µSv/h, a universal baseline) plot, so an all-normal
        // network = 0 events. A radiation/nuclear-monitoring modality no other feed
        // carries (reactor release / detonation / dispersal) over a NATO frontline state.
        fetch_one("odlinfo", Odlinfo, 12),
        // STUK / FMI (Finland) — external radiation dose rate; only stations elevated
        // above natural background (µSv/h, a universal baseline) plot, so an all-normal
        // network = 0 events. Extends the radiation/nuclear-monitoring modality to the
        // NATO/Russia frontline (Finland's eastern border + Loviisa/Olkiluoto NPPs).
        fetch_one("stuk_radiation", StukRadiation, 12),
        // IRSN / ASNR Téléray (France) — ambient gamma dose rate; only stations elevated
        // above natural background (µSv/h, a universal baseline; reported nSv/h) plot, so
        // an all-normal network = 0 events. Extends the radiation/nuclear-monitoring
        // modality to Europe's largest nuclear power (56 reactors + La Hague reprocessing).
        fetch_one("teleray", Teleray, 12),
    );

    let mut errors: Vec<String> = Vec::new();
    let mut counts = serde_json::Map::new();
    let mut feed_events: Vec<Event> = Vec::new();
    // Last-good store, so a transient empty/failed upstream doesn't silently blank a
    // whole layer (a CWFIS GeoServer hiccup used to zero out all of Canada's wildfires).
    let mut lg_guard = FEED_LAST_GOOD.lock().await;
    let last_good = lg_guard.get_or_insert_with(HashMap::new);
    let now = Instant::now();
    // Cap each feed so the payload can't balloon; the two fetched OpenSky windows
    // land in window-keyed slots and are unioned with the off-phase windows below.
    // (events, optional error, source key, per-feed cap)
    for (mut evs, err, key, cap) in [
        (quakes.0, quakes.1, "usgs", 600usize),
        (disasters.0, disasters.1, "gdacs", 400),
        (weather.0, weather.1, "nws", 400),
        (ac_a.0, ac_a.1, win_a.0, win_a.2),
        (ac_b.0, ac_b.1, win_b.0, win_b.2),
        (natural.0, natural.1, "eonet", 600),
        (ca_alerts.0, ca_alerts.1, "eccc_alerts", 300),
        (ca_fires.0, ca_fires.1, "cwfis", 700),
        (ca_quakes.0, ca_quakes.1, "eqcanada", 400),
        (ca_air.0, ca_air.1, "eccc_aqhi", 200),
        (gl_quakes.0, gl_quakes.1, "emsc", 600),
        (gl_volc.0, gl_volc.1, "gvp_volcano", 200),
        (gl_health.0, gl_health.1, "healthmap", 300),
        (gl_fires.0, gl_fires.1, "firms", 1800),
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
        (conflict_agg.0, conflict_agg.1, "acled_aggregated", 500),
        (storms.0, storms.1, "nhc", 60),
        (typhoons.0, typhoons.1, "jma_typhoon", 60),
        (nz_volc.0, nz_volc.1, "geonet_volcano", 60),
        (us_volc.0, us_volc.1, "usgs_volcano", 60),
        (id_volc.0, id_volc.1, "magma_volcano", 150),
        (floods.0, floods.1, "nwps_flood", 400),
        (avalanche.0, avalanche.1, "avalanche_ca", 200),
        (sigmets.0, sigmets.1, "awc_sigmet", 200),
        (storm_reports.0, storm_reports.1, "spc_storm_reports", 400),
        (id_felt.0, id_felt.1, "bmkg_quake", 60),
        (jp_felt.0, jp_felt.1, "jma_quake", 60),
        (nz_felt.0, nz_felt.1, "geonet_quake", 60),
        // Normally ~0 (all-normal network); a real radiological event can light up many
        // stations at once, so allow generous headroom before the cap truncates.
        (de_radiation.0, de_radiation.1, "odlinfo", 400),
        // Finland's ~255-station network; a real event can light up many at once, so
        // allow the same generous headroom as the German network before truncation.
        (fi_radiation.0, fi_radiation.1, "stuk_radiation", 400),
        // France's ~470-station Téléray network; a real event can light up many at once,
        // so allow the same generous headroom as the other radiation networks.
        (fr_radiation.0, fr_radiation.1, "teleray", 400),
    ] {
        // Keep the dots that MATTER when a feed overflows its cap (severity, then
        // recency) — plain truncation cut in arbitrary provider order.
        sort_for_cap(&mut evs);
        evs.truncate(cap);
        if let Some(e) = err {
            errors.push(e);
        }
        // The fetched OpenSky windows park their batch in a window-keyed slot; the
        // union across ALL four windows is assembled after the loop, so they skip
        // the generic fallback / count / extend below.
        if key.starts_with("opensky@") {
            if !evs.is_empty() {
                last_good.insert(key.to_string(), (now, evs));
            }
            continue;
        }
        // Resilience: refresh last-good on a non-empty pull; on an empty one, reuse the
        // recent last-good (flagged stale) instead of caching a deceptive zero.
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
        counts.insert(key.to_string(), json!(evs.len()));
        feed_events.extend(evs);
    }
    // Aircraft layer = union of all four OpenSky rotation windows: the pair fetched
    // this rebuild (age 0) plus each off-phase window still younger than
    // OPENSKY_WINDOW_MAX_AGE, deduped to each airframe's newest fix (an aircraft
    // can sit in two batches at a window seam or across phases).
    {
        let mut newest_fix: HashMap<String, Event> = HashMap::new();
        for (win, _bbox, _cap) in &OPENSKY_WINDOWS {
            let Some((at, batch)) = last_good.get(*win) else { continue };
            if now.duration_since(*at) >= OPENSKY_WINDOW_MAX_AGE {
                continue;
            }
            for e in batch {
                if newest_fix.get(&e.id).is_none_or(|prev| e.time > prev.time) {
                    newest_fix.insert(e.id.clone(), e.clone());
                }
            }
        }
        counts.insert("opensky".to_string(), json!(newest_fix.len()));
        feed_events.extend(newest_fix.into_values());
    }
    drop(lg_guard);

    // Cross-feed earthquake dedup: keep one dot per physical quake (the audit's M6.1
    // near Taiwan plotted 4× — USGS + EMSC + JMA + GDACS), remembering the merged
    // magnitude+intensity chips to apply below.
    let chip_overrides = dedup_earthquakes(&mut feed_events);

    let mut feeds = ee_view::geojson::to_feature_collection(&feed_events);
    // Enrich each plotted feature with a human-readable value chip ("M2.7", "24 MW
    // fire power", "Warning", "AQHI 7 · High risk") pulled from the provider's raw
    // payload and matched back by event id — so the map popup conveys real meaning,
    // not an opaque normalized 0–1 severity.
    let mut details: HashMap<&str, String> = feed_events
        .iter()
        .filter_map(|e| feed_detail(e).map(|d| (e.id.as_str(), d)))
        .collect();
    // Merged quake chips ("M6.1 · Shindo 3": global-catalogue magnitude + national
    // intensity) replace the kept event's own single-feed chip.
    for (id, chip) in &chip_overrides {
        details.insert(id.as_str(), chip.clone());
    }
    if let Some(arr) = feeds.get_mut("features").and_then(|f| f.as_array_mut()) {
        for feat in arr {
            let id = feat.get("properties").and_then(|p| p.get("id")).and_then(|i| i.as_str());
            if let Some(d) = id.and_then(|id| details.get(id)) {
                feat["properties"]["detail"] = json!(d);
            }
        }
    }
    // Post-conversion per-source dot counts: `counts` above is the raw fetch total
    // (the feed-health read existing consumers key on), but geo-less events drop at
    // GeoJSON conversion and the quake dedup removes cross-feed duplicates — so
    // `plotted` reports what actually renders. The payload must never claim more
    // dots than it shows. (meaningful-number rule)
    let plotted = plotted_counts(&feeds);

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
        "plotted": plotted,
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
    fn acled_aggregated_chip_surfaces_regional_intensity() {
        use chrono::Utc;
        use ee_core::{EventKind, Geo, Severity};
        let mk = |raw: Value| Event {
            id: "acled-agg-x".into(),
            source_id: "acled_aggregated".into(),
            kind: EventKind::Conflict,
            title: "t".into(),
            time: Utc::now(),
            geo: Geo::new(0.0, 0.0),
            severity: Severity::new(0.6),
            url: None,
            raw,
        };
        // `raw` is the aggregated Admin-1 record: events + fatalities + dominant label.
        let e = mk(json!({"events": 41.0, "fatalities": 66.0, "label": "Air/drone strike"}));
        assert_eq!(feed_detail(&e).as_deref(), Some("41 events · 66 fatalities · Air/drone strike"));
        // No fatalities -> the fatalities clause is omitted.
        let e = mk(json!({"events": 7.0, "fatalities": 0.0, "label": "Peaceful protest"}));
        assert_eq!(feed_detail(&e).as_deref(), Some("7 events · Peaceful protest"));
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
    fn avalanche_ca_chip_surfaces_per_band_danger_rating() {
        use chrono::Utc;
        use ee_core::{EventKind, Geo, Severity};
        let mk = |raw: Value| Event {
            id: "avalanche-ca-x".into(),
            source_id: "avalanche_ca".into(),
            kind: EventKind::Weather,
            title: "t".into(),
            time: Utc::now(),
            geo: Geo::new(0.0, 0.0),
            severity: Severity::new(0.65),
            url: None,
            raw,
        };
        // `raw` is the stored forecast report: today's danger ratings per band.
        let e = mk(json!({"dangerRatings": [{"ratings": {
            "alp": {"rating": {"value": "considerable"}},
            "tln": {"rating": {"value": "moderate"}},
            "btl": {"rating": {"value": "low"}}}}]}));
        assert_eq!(
            feed_detail(&e).as_deref(),
            Some("Alpine Considerable · Treeline Moderate · Below Low")
        );
    }

    #[test]
    fn awc_sigmet_chip_surfaces_qualified_hazard_and_flight_levels() {
        use chrono::Utc;
        use ee_core::{EventKind, Geo, Severity};
        let mk = |raw: Value| Event {
            id: "awc-sigmet-x".into(),
            source_id: "awc_sigmet".into(),
            kind: EventKind::Weather,
            title: "t".into(),
            time: Utc::now(),
            geo: Geo::new(0.0, 0.0),
            severity: Severity::new(0.85),
            url: None,
            raw,
        };
        // `raw` is the SIGMET feature's properties: hazard + qualifier + flight band.
        let e = mk(json!({"hazard": "TURB", "qualifier": "SEV", "base": 17000, "top": 33000}));
        assert_eq!(feed_detail(&e).as_deref(), Some("Severe Turbulence · FL170–330"));
        // Coverage-only qualifier + a top-only band.
        let e = mk(json!({"hazard": "TS", "qualifier": "EMBD", "top": 43000}));
        assert_eq!(feed_detail(&e).as_deref(), Some("Embedded Thunderstorms · to FL430"));
    }

    #[test]
    fn spc_storm_reports_chip_surfaces_hazard_and_magnitude() {
        use chrono::Utc;
        use ee_core::{EventKind, Geo, Severity};
        let mk = |raw: Value| Event {
            id: "spc-x".into(),
            source_id: "spc_storm_reports".into(),
            kind: EventKind::Weather,
            title: "t".into(),
            time: Utc::now(),
            geo: Geo::new(0.0, 0.0),
            severity: Severity::new(0.7),
            url: None,
            raw,
        };
        // `raw` is the flat report payload this connector stores: type + magnitude.
        let e = mk(json!({"type": "tornado", "fscale": 2}));
        assert_eq!(feed_detail(&e).as_deref(), Some("EF2 Tornado"));
        let e = mk(json!({"type": "tornado"}));
        assert_eq!(feed_detail(&e).as_deref(), Some("Tornado"));
        let e = mk(json!({"type": "hail", "size_in": 2.75}));
        assert_eq!(feed_detail(&e).as_deref(), Some("2.75 in hail"));
        let e = mk(json!({"type": "wind", "speed_mph": 70.0}));
        assert_eq!(feed_detail(&e).as_deref(), Some("70 mph wind"));
        let e = mk(json!({"type": "wind"}));
        assert_eq!(feed_detail(&e).as_deref(), Some("Damaging wind"));
    }

    #[test]
    fn asam_chip_surfaces_class_and_vessel() {
        use chrono::Utc;
        use ee_core::{EventKind, Geo, Severity};
        let mk = |raw: Value| Event {
            id: "asam-x".into(),
            source_id: "asam".into(),
            kind: EventKind::Vessel,
            title: "t".into(),
            time: Utc::now(),
            geo: Geo::new(0.0, 0.0),
            severity: Severity::new(0.7),
            url: None,
            raw,
        };
        // `raw` is the flat ASAM payload this connector stores: escalation class + victim.
        let e = mk(json!({"class": "Armed attack", "victim": "Chemical Tanker"}));
        assert_eq!(feed_detail(&e).as_deref(), Some("Armed attack · Chemical Tanker"));
        let e = mk(json!({"class": "Boarding", "victim": ""}));
        assert_eq!(feed_detail(&e).as_deref(), Some("Boarding"));
    }

    #[test]
    fn bmkg_quake_chip_surfaces_felt_intensity_and_tsunami() {
        use chrono::Utc;
        use ee_core::{EventKind, Geo, Severity};
        let mk = |raw: Value| Event {
            id: "bmkg-x".into(),
            source_id: "bmkg_quake".into(),
            kind: EventKind::Earthquake,
            title: "t".into(),
            time: Utc::now(),
            geo: Geo::new(0.0, 0.0),
            severity: Severity::new(0.5),
            url: None,
            raw,
        };
        // `raw` is the flat felt-quake payload this connector stores.
        let e = mk(json!({"magnitude": 4.8, "mmi": 4}));
        assert_eq!(feed_detail(&e).as_deref(), Some("Felt MMI IV · M4.8"));
        let e = mk(json!({"magnitude": 6.2, "mmi": 6, "tsunami": "Siaga"}));
        assert_eq!(feed_detail(&e).as_deref(), Some("Felt MMI VI · M6.2 · Tsunami Siaga"));
        // No MMI parsed → magnitude only.
        let e = mk(json!({"magnitude": 5.1, "mmi": Value::Null}));
        assert_eq!(feed_detail(&e).as_deref(), Some("M5.1"));
    }

    #[test]
    fn jma_quake_chip_surfaces_shindo_intensity() {
        use chrono::Utc;
        use ee_core::{EventKind, Geo, Severity};
        let mk = |raw: Value| Event {
            id: "jma-x".into(),
            source_id: "jma_quake".into(),
            kind: EventKind::Earthquake,
            title: "t".into(),
            time: Utc::now(),
            geo: Geo::new(0.0, 0.0),
            severity: Severity::new(0.5),
            url: None,
            raw,
        };
        // `raw` is the flat quake payload this connector stores.
        let e = mk(json!({"magnitude": 6.1, "shindo": "5+"}));
        assert_eq!(feed_detail(&e).as_deref(), Some("Shindo 5+ · M6.1"));
        let e = mk(json!({"magnitude": 4.2, "shindo": "3"}));
        assert_eq!(feed_detail(&e).as_deref(), Some("Shindo 3 · M4.2"));
    }

    #[test]
    fn geonet_quake_chip_surfaces_felt_mmi() {
        use chrono::Utc;
        use ee_core::{EventKind, Geo, Severity};
        let mk = |raw: Value| Event {
            id: "geonet-x".into(),
            source_id: "geonet_quake".into(),
            kind: EventKind::Earthquake,
            title: "t".into(),
            time: Utc::now(),
            geo: Geo::new(0.0, 0.0),
            severity: Severity::new(0.5),
            url: None,
            raw,
        };
        // `raw` is the GeoNet feature's `properties` object this connector stores.
        let e = mk(json!({"magnitude": 5.94, "mmi": 7}));
        assert_eq!(feed_detail(&e).as_deref(), Some("Felt MMI 7 · M5.9"));
        let e = mk(json!({"magnitude": 5.02, "mmi": 5}));
        assert_eq!(feed_detail(&e).as_deref(), Some("Felt MMI 5 · M5.0"));
    }

    #[test]
    fn odlinfo_chip_surfaces_dose_rate_and_band() {
        use chrono::Utc;
        use ee_core::{EventKind, Geo, Severity};
        let mk = |raw: Value| Event {
            id: "odlinfo-x".into(),
            source_id: "odlinfo".into(),
            kind: EventKind::Other,
            title: "t".into(),
            time: Utc::now(),
            geo: Geo::new(0.0, 0.0),
            severity: Severity::new(0.4),
            url: None,
            raw,
        };
        // `raw` is the WFS feature's `properties` object this connector stores.
        let e = mk(json!({"value": 0.45, "unit": "µSv/h"}));
        assert_eq!(feed_detail(&e).as_deref(), Some("0.45 µSv/h · Above normal"));
        let e = mk(json!({"value": 3.1, "unit": "µSv/h"}));
        assert_eq!(feed_detail(&e).as_deref(), Some("3.10 µSv/h · High"));
    }

    #[test]
    fn stuk_radiation_chip_surfaces_dose_rate_and_band() {
        use chrono::Utc;
        use ee_core::{EventKind, Geo, Severity};
        let mk = |raw: Value| Event {
            id: "stuk_radiation-x".into(),
            source_id: "stuk_radiation".into(),
            kind: EventKind::Other,
            title: "t".into(),
            time: Utc::now(),
            geo: Geo::new(60.0, 25.0),
            severity: Severity::new(0.5),
            url: None,
            raw,
        };
        // `raw` is the flat properties object the STUK connector stores.
        let e = mk(json!({"value": 0.62, "unit": "µSv/h"}));
        assert_eq!(feed_detail(&e).as_deref(), Some("0.62 µSv/h · Elevated"));
        let e = mk(json!({"value": 0.45, "unit": "µSv/h"}));
        assert_eq!(feed_detail(&e).as_deref(), Some("0.45 µSv/h · Above normal"));
    }

    #[test]
    fn teleray_chip_surfaces_dose_rate_and_band() {
        use chrono::Utc;
        use ee_core::{EventKind, Geo, Severity};
        let mk = |raw: Value| Event {
            id: "teleray-x".into(),
            source_id: "teleray".into(),
            kind: EventKind::Other,
            title: "t".into(),
            time: Utc::now(),
            geo: Geo::new(48.85, 2.35),
            severity: Severity::new(0.7),
            url: None,
            raw,
        };
        // `raw` is the flat properties object the Téléray connector stores (value in µSv/h).
        let e = mk(json!({"value": 3.1, "unit": "µSv/h"}));
        assert_eq!(feed_detail(&e).as_deref(), Some("3.10 µSv/h · High"));
        let e = mk(json!({"value": 0.45, "unit": "µSv/h"}));
        assert_eq!(feed_detail(&e).as_deref(), Some("0.45 µSv/h · Above normal"));
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
    async fn payload_cache_is_stale_while_revalidate() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        // 'static counter + cache so the SWR background refresh (which spawns a task that
        // borrows the cache) satisfies the Send + 'static bounds; matches the production
        // statics (MAP_FEEDS_CACHE / FINANCE_CACHE). A tiny test-only leak.
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        async fn bump() -> Value { json!(CALLS.fetch_add(1, Ordering::SeqCst)) }

        // Long TTL: first miss builds (blocks); the second is a fresh cache hit (no rebuild).
        let cache: &'static PayloadCache =
            Box::leak(Box::new(PayloadCache::new(StdDuration::from_secs(60))));
        assert_eq!(cache.get_or_refresh(bump).await, json!(0));
        assert_eq!(cache.get_or_refresh(bump).await, json!(0));
        assert_eq!(CALLS.load(Ordering::SeqCst), 1, "a fresh hit must not rebuild");

        // Zero TTL: the entry is immediately stale. SWR returns the STALE value synchronously
        // and rebuilds in the BACKGROUND (single-flight), so the rebuild count rises after a
        // brief yield — NOT on the synchronous return.
        static CALLS2: AtomicUsize = AtomicUsize::new(0);
        async fn bump2() -> Value { json!(CALLS2.fetch_add(1, Ordering::SeqCst)) }
        let swr: &'static PayloadCache =
            Box::leak(Box::new(PayloadCache::new(StdDuration::from_secs(0))));
        assert_eq!(swr.get_or_refresh(bump2).await, json!(0)); // cold → blocks → 0
        let _ = swr.get_or_refresh(bump2).await;               // stale → serves stale (0) + bg rebuild
        tokio::time::sleep(StdDuration::from_millis(80)).await; // let the background rebuild run
        assert!(CALLS2.load(Ordering::SeqCst) >= 2, "a stale read must trigger a background rebuild");
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

    // ── Locks for the 2026-07-03 map fixes (quake dedup / caps / rotation / counts) ──

    fn quake(id: &str, source_id: &str, secs: i64, lat: f64, lon: f64, raw: Value) -> Event {
        Event {
            id: id.to_string(),
            source_id: source_id.to_string(),
            kind: ee_core::EventKind::Earthquake,
            title: format!("test quake {id}"),
            time: chrono::DateTime::from_timestamp(1_780_000_000 + secs, 0).unwrap(),
            geo: Some(ee_core::Geo { lat, lon }),
            severity: ee_core::Severity::new(0.5),
            url: None,
            raw,
        }
    }

    #[test]
    fn cross_feed_quake_dedup_keeps_one_dot_with_the_merged_intensity_chip() {
        // The live audit found one M6.1 near Taiwan plotted FOUR times (usgs + emsc +
        // jma + gdacs). The intensity-graded national entry must win, the rest drop,
        // and the survivor's chip must recombine the global magnitude with the
        // national intensity read: "M6.1 · Shindo 3".
        let mut evs = vec![
            quake("u1", "usgs", 0, 25.95, 125.79, json!({"properties":{"mag":6.1}})),
            quake("e1", "emsc", 12, 25.98, 125.75, json!({"properties":{"mag":6.1}})),
            quake("j1", "jma_quake", 30, 25.9, 125.8, json!({"shindo":"3","magnitude":6.4})),
            quake("g1", "gdacs", 45, 25.96, 125.77,
                json!({"properties":{"severitydata":{"severity":6.1}}})),
        ];
        let overrides = dedup_earthquakes(&mut evs);
        assert_eq!(evs.len(), 1, "four catalogue entries for one quake must become one dot");
        assert_eq!(evs[0].id, "j1", "the intensity-graded national entry outranks the catalogues");
        assert_eq!(overrides.get("j1").map(String::as_str), Some("M6.1 · Shindo 3"),
            "the merged chip carries the global magnitude + the national intensity");
    }

    #[test]
    fn quake_dedup_never_merges_same_catalogue_or_disagreeing_magnitudes() {
        // A catalogue does not duplicate itself: a mainshock and an immediate
        // aftershock from ONE feed inside the 90 s / 0.3° window are two real events.
        let mut same_src = vec![
            quake("m", "usgs", 0, 35.0, 140.0, json!({"properties":{"mag":7.0}})),
            quake("a", "usgs", 70, 35.1, 140.1, json!({"properties":{"mag":5.6}})),
        ];
        assert!(dedup_earthquakes(&mut same_src).is_empty());
        assert_eq!(same_src.len(), 2, "same-catalogue pair must both survive");
        // Cross-catalogue entries with known but disagreeing magnitudes are two real
        // events too (an M7.0 mainshock must not swallow a separately-solved M5.6).
        let mut cross = vec![
            quake("u", "usgs", 0, 35.0, 140.0, json!({"properties":{"mag":7.0}})),
            quake("e", "emsc", 70, 35.1, 140.1, json!({"properties":{"mag":5.6}})),
        ];
        dedup_earthquakes(&mut cross);
        assert_eq!(cross.len(), 2, "magnitude disagreement > 0.7 must block the merge");
    }

    #[test]
    fn quake_dedup_national_keeper_cannot_swallow_a_distinct_aftershock() {
        // The guard used to read magnitudes only off usgs/emsc, so a NATIONAL keeper
        // (rank 0) merged every same_quake sibling unchecked: a JMA M7.0 mainshock
        // swallowed a separately-solved USGS M5.6 aftershock 70 s / 0.15° away, and the
        // real aftershock vanished from the map. National magnitudes now feed the guard.
        let mut evs = vec![
            quake("j", "jma_quake", 0, 35.0, 140.0, json!({"shindo":"6+","magnitude":7.0})),
            quake("u", "usgs", 70, 35.1, 140.1, json!({"properties":{"mag":5.6}})),
        ];
        dedup_earthquakes(&mut evs);
        assert_eq!(evs.len(), 2, "a distinct M5.6 aftershock must survive a national M7.0 keeper");

        // Same magnitudes still merge through the national keeper (the designed path).
        let mut same = vec![
            quake("j", "jma_quake", 0, 35.0, 140.0, json!({"shindo":"3","magnitude":6.2})),
            quake("u", "usgs", 30, 35.05, 140.05, json!({"properties":{"mag":6.1}})),
        ];
        dedup_earthquakes(&mut same);
        assert_eq!(same.len(), 1, "agreeing magnitudes keep collapsing to the national dot");

        // Unreadable magnitude ⇒ fail closed: keep both dots rather than risk a swallow.
        let mut unk = vec![
            quake("j", "jma_quake", 0, 35.0, 140.0, json!({"shindo":"3","magnitude":6.2})),
            quake("x", "usgs", 30, 35.05, 140.05, json!({})),
        ];
        dedup_earthquakes(&mut unk);
        assert_eq!(unk.len(), 2, "an unverifiable pair must not merge");
    }

    #[test]
    fn quake_dedup_matches_across_the_antimeridian() {
        // Fiji/Kermadec hypocentre solutions straddle ±180°; the longitude delta must
        // wrap or the same quake stays plotted twice out there forever.
        let mut evs = vec![
            quake("u", "usgs", 0, -23.0, 179.9, json!({"properties":{"mag":5.8}})),
            quake("e", "emsc", 20, -23.05, -179.95, json!({"properties":{"mag":5.9}})),
        ];
        dedup_earthquakes(&mut evs);
        assert_eq!(evs.len(), 1, "a ±180° wrap pair is one quake");
        assert_eq!(evs[0].id, "u", "usgs outranks emsc");
    }

    #[test]
    fn sort_for_cap_keeps_the_most_severe_then_the_newest() {
        // evs.truncate(cap) used to cut in provider order, silently dropping arbitrary
        // (possibly highest-severity) tails on every feed at its cap. After sorting,
        // a cap keeps the dots that matter.
        let mut evs = vec![
            quake("low", "usgs", 0, 0.0, 0.0, json!({})),
            quake("hi_old", "usgs", 10, 1.0, 1.0, json!({})),
            quake("hi_new", "usgs", 500, 2.0, 2.0, json!({})),
        ];
        evs[0].severity = ee_core::Severity::new(0.2);
        evs[1].severity = ee_core::Severity::new(0.9);
        evs[2].severity = ee_core::Severity::new(0.9);
        sort_for_cap(&mut evs);
        let order: Vec<&str> = evs.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(order, vec!["hi_new", "hi_old", "low"],
            "severity desc, ties newest-first — so truncate(cap) drops the least important");
    }

    #[test]
    fn opensky_rotation_covers_all_four_windows_in_two_rebuilds() {
        // Even rebuilds fetch the Atlantic pair, odd the Asian pair — so Korea and the
        // Taiwan Strait (3 of 5 flashpoint theaters) are genuinely fetched, not just
        // declared, without raising the anonymous request count per rebuild.
        let (a0, b0) = opensky_phase_windows(0);
        let (a1, b1) = opensky_phase_windows(1);
        let (a2, b2) = opensky_phase_windows(2);
        assert_eq!((a0.0, b0.0), ("opensky@na", "opensky@eu"));
        assert_eq!((a1.0, b1.0), ("opensky@kr", "opensky@tw"));
        assert_eq!((a2.0, b2.0), (a0.0, b0.0), "rotation is period-2 over the rebuild counter");
    }

    #[test]
    fn plotted_counts_reports_only_features_that_made_the_geojson() {
        // counts{} reports raw fetch totals while geo-less events are dropped at the
        // GeoJSON conversion — plotted{} must say what is actually on the map, so the
        // payload never claims more than it shows (audit: nws counted 400, plotted 110).
        let feeds = json!({"features": [
            {"properties": {"source_id": "nws"}},
            {"properties": {"source_id": "nws"}},
            {"properties": {"source_id": "usgs"}},
            {"properties": {}}
        ]});
        let plotted = plotted_counts(&feeds);
        assert_eq!(plotted.get("nws").and_then(Value::as_u64), Some(2));
        assert_eq!(plotted.get("usgs").and_then(Value::as_u64), Some(1));
        assert_eq!(plotted.len(), 2, "a feature with no source_id counts toward no feed");
    }
}
