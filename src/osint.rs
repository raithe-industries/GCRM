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

use std::time::{Duration as StdDuration, Instant};

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

/// Escalation-heat → marker colour, matching the dashboard's rung palette.
fn heat_color(heat: f64) -> &'static str {
    match heat {
        h if h >= 0.62 => "#7a0000", // Great-Power War
        h if h >= 0.38 => "#c0392b", // Limited War
        h if h >= 0.18 => "#e67e22", // Crisis
        h if h >= 0.06 => "#d4962a", // Tension
        _ => "#1D9E75",              // Stable
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
                "color": heat_color(heat),
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
        cwfis::Cwfis, eccc_alerts::EcccAlerts, eonet::Eonet, eqcanada::EqCanada, gdacs::Gdacs,
        nws::Nws, opensky::OpenSky, usgs::Usgs,
    };

    // Pull the geocoded feeds concurrently, each time-boxed. Aircraft over BOTH
    // North America (incl. Canada) and Europe/Middle-East (the live theaters), for
    // dense, honest coverage on both sides of the Atlantic. NWS/USGS leave Canada
    // nearly blank, so three Canada-native feeds (ECCC alerts, CWFIS wildfire
    // hotspots, NRCan earthquakes) fill the North-American signal gap.
    let (quakes, disasters, weather, ac_na, ac_eu, natural, ca_alerts, ca_fires, ca_quakes) = tokio::join!(
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
        fetch_one("cwfis", Cwfis::default(), 12),
        // Earthquakes Canada (NRCan) — dense Canadian seismicity USGS drops, last 7d.
        fetch_one("eqcanada", EqCanada::default(), 9),
    );

    let mut errors: Vec<String> = Vec::new();
    let mut counts = serde_json::Map::new();
    let mut feed_events: Vec<Event> = Vec::new();
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
    ] {
        evs.truncate(cap);
        if key == "opensky" {
            opensky_total += evs.len();
            counts.insert("opensky".to_string(), json!(opensky_total));
        } else {
            counts.insert(key.to_string(), json!(evs.len()));
        }
        if let Some(e) = err {
            errors.push(e);
        }
        feed_events.extend(evs);
    }

    let feeds = ee_view::geojson::to_feature_collection(&feed_events);

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
    fn heat_colors_ramp_by_rung() {
        assert_eq!(heat_color(0.01), "#1D9E75"); // stable
        assert_eq!(heat_color(0.5), "#c0392b"); // limited war
        assert_eq!(heat_color(0.9), "#7a0000"); // great-power war
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
    fn theater_features_placed_from_snapshot() {
        let snap = json!({
            "theaters": [
                {"theater_id": "us_iran", "label": "US/Iran", "rung_label": "Crisis",
                 "heat": 0.45, "trend": "rising", "event_count": 12},
                {"theater_id": "korea", "label": "Korea", "rung_label": "Tension",
                 "heat": 0.10, "trend": "stable", "event_count": 3},
                {"theater_id": "other", "label": "Other", "heat": 0.2}
            ]
        });
        let feats = build_theater_features(&Some(snap));
        // "other" has no coordinate -> dropped; the two placed theaters remain.
        assert_eq!(feats.len(), 2);
        let iran = &feats[0];
        assert_eq!(iran["properties"]["id"], "us_iran");
        assert_eq!(iran["properties"]["color"], "#c0392b"); // heat 0.45 -> Limited War red
        // GeoJSON coordinate order is [lon, lat].
        let c = iran["geometry"]["coordinates"].as_array().unwrap();
        assert!((c[0].as_f64().unwrap() - 56.3).abs() < 1e-6);
        assert!((c[1].as_f64().unwrap() - 26.6).abs() < 1e-6);
        // No snapshot -> no features.
        assert!(build_theater_features(&None).is_empty());
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
