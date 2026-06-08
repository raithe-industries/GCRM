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

use std::time::Duration as StdDuration;

use ee_core::{Event, Source};
use serde_json::{json, Value};
use tokio::time::timeout;

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
pub async fn map_payload(snapshot: Option<Value>) -> Value {
    use ee_sources::{eonet::Eonet, gdacs::Gdacs, nws::Nws, opensky::OpenSky, usgs::Usgs};

    // Pull the geocoded feeds concurrently, each time-boxed.
    let (quakes, disasters, weather, aircraft, natural) = tokio::join!(
        fetch_one("usgs", Usgs { feed: "all_day".into() }, 8),
        fetch_one("gdacs", Gdacs, 10),
        fetch_one("nws", Nws, 10),
        // Aircraft scoped to the Europe→Middle-East corridor (the live theaters),
        // so the payload stays bounded and on-theme.
        fetch_one("opensky", OpenSky { bbox: Some((25.0, -12.0, 60.0, 60.0)) }, 8),
        // NASA EONET natural events (wildfires / storms / volcanoes), last 30 days.
        fetch_one("eonet", Eonet { days: 30 }, 10),
    );

    let mut errors: Vec<String> = Vec::new();
    let mut counts = serde_json::Map::new();
    let mut feed_events: Vec<Event> = Vec::new();
    for (mut evs, err, key) in [
        (quakes.0, quakes.1, "usgs"),
        (disasters.0, disasters.1, "gdacs"),
        (weather.0, weather.1, "nws"),
        (aircraft.0, aircraft.1, "opensky"),
        (natural.0, natural.1, "eonet"),
    ] {
        // Cap any single feed so the map payload can't balloon (aircraft especially).
        evs.truncate(600);
        counts.insert(key.to_string(), json!(evs.len()));
        if let Some(e) = err {
            errors.push(e);
        }
        feed_events.extend(evs);
    }

    let feeds = ee_view::geojson::to_feature_collection(&feed_events);

    // GCRM theater flashpoints from the live snapshot → their own feature set.
    let theater_features = build_theater_features(&snapshot);
    counts.insert("theaters".to_string(), json!(theater_features.len()));

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
        "theaters": { "type": "FeatureCollection", "features": theater_features },
        "counts": counts,
        "errors": errors,
    })
}

/// Compute the Finance Radar from the live Yahoo market stream, enriched with the
/// labels/colours the dashboard panel needs.
pub async fn finance_payload() -> Value {
    use ee_correlate::{radar, RadarParams};
    use ee_sources::yahoo::Yahoo;

    let (events, err) = fetch_one("yahoo", Yahoo::default(), 12).await;
    let r = radar(&events, &RadarParams::default());

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
        "segments": segments,
        "errors": err.map(|e| vec![e]).unwrap_or_default(),
    })
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
        assert!(map["basemap"]["tiles"].as_array().unwrap().len() >= 1);
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
