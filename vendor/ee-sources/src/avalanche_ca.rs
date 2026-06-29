//! Avalanche Canada — public avalanche-forecast danger ratings. Free, no API key.
//!
//! Avalanche Canada is Canada's national public-avalanche-safety body. Its forecast
//! API exposes two companion products:
//!   * `/forecasts/en/products` — the current forecast bulletins (JSON). Each product
//!     carries an `area.id` plus a `report` whose `dangerRatings[0]` holds the
//!     **current-day** North American danger rating (1–5: Low / Moderate /
//!     Considerable / High / Extreme) for the three elevation bands `alp`/`tln`/`btl`
//!     (alpine / treeline / below-treeline).
//!   * `/forecasts/en/areas` — the forecast-region polygons (GeoJSON), one Feature per
//!     region keyed by the same id, so a bulletin can be placed on the map.
//!
//! This connector joins the two by area id and emits one normalized
//! [`EventKind::Weather`] [`Event`] per region **with a real danger rating today**,
//! plotted at the region polygon's centroid. Severity is the peak band rating.
//!
//! **Seasonal, handled honestly.** Outside the winter season Avalanche Canada issues
//! no numeric rating — bands read `norating` (or a "spring"/early-season statement),
//! which carries no operator signal. Those are dropped, so an off-season network
//! yields **zero events, not an error** (the layer simply lights up ~late-Nov→Apr).
//! That off-season tolerance is the resolution of the source's deferral.
//!
//! The North American danger scale is baseline-relative and unit-bearing (each level
//! has a defined likelihood/size meaning), so a "Considerable" dot is signal-meaningful
//! — not a raw absolute number. Non-duplicative: no current feed carries snow-avalanche
//! hazard.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

/// Avalanche Canada forecast source.
#[derive(Default)]
pub struct AvalancheCa;

impl AvalancheCa {
    pub fn products_url(&self) -> &'static str {
        "https://api.avalanche.ca/forecasts/en/products"
    }
    pub fn areas_url(&self) -> &'static str {
        "https://api.avalanche.ca/forecasts/en/areas"
    }
}

#[async_trait]
impl Source for AvalancheCa {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "avalanche_ca",
            name: "Avalanche Canada Forecasts",
            domain: EventKind::Weather,
            cadence: Duration::from_secs(1800),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        // Two fetches: the bulletins (danger ratings, by area id) + the region
        // polygons (geometry, by the same id). Join them into placed events.
        let products = crate::http::fetch_text(self.products_url()).await?;
        let areas = crate::http::fetch_text(self.areas_url()).await?;
        parse_avalanche_ca(&products, &areas)
    }
}

/// Centroid (mean vertex) of a Polygon/MultiPolygon coordinate tree.
fn centroid(geometry: &Value) -> Option<Geo> {
    fn collect(v: &Value, acc: &mut Vec<(f64, f64)>) {
        if let Some(arr) = v.as_array() {
            if arr.len() == 2 && arr[0].is_number() && arr[1].is_number() {
                if let (Some(lon), Some(lat)) = (arr[0].as_f64(), arr[1].as_f64()) {
                    acc.push((lon, lat));
                }
            } else {
                for x in arr {
                    collect(x, &mut *acc);
                }
            }
        }
    }
    let mut pts = Vec::new();
    collect(geometry.get("coordinates")?, &mut pts);
    if pts.is_empty() {
        return None;
    }
    let (slon, slat) = pts.iter().fold((0.0, 0.0), |(a, b), (lo, la)| (a + lo, b + la));
    let n = pts.len() as f64;
    Geo::new(slat / n, slon / n)
}

/// An id that may arrive as a JSON string or number; normalize to the string key.
fn id_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// Normalized 0–1 severity from a single band's danger value. The North American
/// public avalanche danger scale runs 1 (Low) → 5 (Extreme); anything else
/// (`norating`, a spring/early-season statement, blank) carries no signal → 0.0,
/// which drops the band/region. Tolerates the value arriving as the word
/// ("considerable"), the numbered display ("3 - Considerable"), or a bare number.
fn danger_rank(v: &Value) -> f64 {
    if let Some(n) = v.as_f64() {
        return match n.round() as i64 {
            5 => 1.0,
            4 => 0.85,
            3 => 0.65,
            2 => 0.45,
            1 => 0.2,
            _ => 0.0,
        };
    }
    let s = v.as_str().unwrap_or("").to_ascii_lowercase();
    if s.contains("extreme") {
        1.0
    } else if s.contains("high") {
        0.85
    } else if s.contains("considerable") {
        0.65
    } else if s.contains("moderate") {
        0.45
    } else if s.contains("low") {
        0.2
    } else {
        0.0 // norating / spring / no-forecast / blank
    }
}

/// Plain-language label for a band's danger value, or `None` when there is no rating.
fn danger_label(v: &Value) -> Option<&'static str> {
    match danger_rank(v) {
        r if r >= 1.0 => Some("Extreme"),
        r if r >= 0.85 => Some("High"),
        r if r >= 0.65 => Some("Considerable"),
        r if r >= 0.45 => Some("Moderate"),
        r if r >= 0.2 => Some("Low"),
        _ => None,
    }
}

/// The product's forecast report (where the danger ratings live), tolerating either a
/// nested `report` object or the ratings sitting on the product itself.
fn report_of(product: &Value) -> &Value {
    product.get("report").filter(|r| r.is_object()).unwrap_or(product)
}

/// Today's `ratings` object (`dangerRatings[0].ratings`) within a report, tolerating
/// an `attributes` wrapper some mirrors interpose.
fn today_ratings(report: &Value) -> Option<&Value> {
    let dr = report
        .get("dangerRatings")
        .or_else(|| report.get("attributes").and_then(|a| a.get("dangerRatings")))
        .and_then(Value::as_array)?;
    dr.first()?.get("ratings")
}

/// A band's danger value (`ratings.<key>.rating.value`).
fn band_value<'a>(ratings: &'a Value, key: &str) -> Option<&'a Value> {
    ratings.get(key)?.get("rating")?.get("value")
}

/// Operator chip: the current-day danger rating per elevation band, e.g.
/// "Alpine Considerable · Treeline Moderate · Below Low". `raw` is the stored report.
/// Bands with no rating are omitted; an all-norating report yields `None`.
pub fn danger_chip(raw: &Value) -> Option<String> {
    let ratings = today_ratings(raw)?;
    let parts: Vec<String> = [("Alpine", "alp"), ("Treeline", "tln"), ("Below", "btl")]
        .iter()
        .filter_map(|(label, key)| {
            let v = band_value(ratings, key)?;
            danger_label(v).map(|d| format!("{label} {d}"))
        })
        .collect();
    if parts.is_empty() {
        return None;
    }
    Some(parts.join(" · "))
}

/// Build the `area id -> centroid` lookup from the `/forecasts/en/areas` GeoJSON.
fn parse_areas(json: &str) -> anyhow::Result<HashMap<String, Geo>> {
    let root: Value = serde_json::from_str(json)?;
    let features = root
        .get("features")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("avalanche_ca: areas missing 'features' array"))?;
    let mut map = HashMap::with_capacity(features.len());
    for f in features {
        // The area id is the feature's top-level `id` (matches product `area.id`).
        let id = f.get("id").map(id_of).unwrap_or_default();
        if id.is_empty() {
            continue;
        }
        if let Some(geo) = f.get("geometry").filter(|g| !g.is_null()).and_then(centroid) {
            map.insert(id, geo);
        }
    }
    Ok(map)
}

/// Pure parser: join the bulletins JSON to the region polygons -> events. Unit-tested
/// offline. A malformed products/areas payload is an error; regions with no current
/// danger rating (off-season `norating`/spring) or no matching polygon are filtered
/// out, so an off-season network is Ok/empty.
pub fn parse_avalanche_ca(products_json: &str, areas_json: &str) -> anyhow::Result<Vec<Event>> {
    let coords = parse_areas(areas_json)?;
    let root: Value = serde_json::from_str(products_json)?;
    // Tolerate a bare array or a `{products:[…]}` / `{data:[…]}` wrapper.
    let products = root
        .as_array()
        .or_else(|| root.get("products").and_then(Value::as_array))
        .or_else(|| root.get("data").and_then(Value::as_array))
        .ok_or_else(|| anyhow::anyhow!("avalanche_ca: products is not an array"))?;

    let mut out = Vec::with_capacity(products.len());
    for p in products {
        let area = p.get("area");
        let area_id = area.and_then(|a| a.get("id")).map(id_of).unwrap_or_default();
        if area_id.is_empty() {
            continue;
        }

        let report = report_of(p);
        let Some(ratings) = today_ratings(report) else { continue };
        // Peak band rating drives severity; 0 means no real rating today → drop.
        let severity = ["alp", "tln", "btl"]
            .iter()
            .filter_map(|k| band_value(ratings, k))
            .map(danger_rank)
            .fold(0.0_f64, f64::max);
        if severity <= 0.0 {
            continue; // off-season / no-rating region
        }

        // Place the bulletin at its region polygon's centroid.
        let Some(&geo) = coords.get(&area_id) else { continue };

        let name = area
            .and_then(|a| a.get("name"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .or_else(|| report.get("title").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()))
            .unwrap_or(area_id.as_str());

        let time = report
            .get("dateIssued")
            .and_then(Value::as_str)
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        let url = p
            .get("url")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| s.starts_with("http"))
            .map(str::to_string)
            .unwrap_or_else(|| "https://avalanche.ca/map".to_string());

        out.push(Event {
            id: format!("avalanche-ca-{area_id}"),
            source_id: "avalanche_ca".to_string(),
            kind: EventKind::Weather,
            title: format!("Avalanche forecast — {name}"),
            time,
            geo: Some(geo),
            severity: Severity::new(severity),
            url: Some(url),
            raw: report.clone(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Region polygons keyed by id — one box in the Rockies (Kananaskis), one in the
    // Coast range (a region that will be off-season this fixture), and one with no
    // geometry (must be skipped).
    const AREAS: &str = r#"{
      "type":"FeatureCollection",
      "features":[
        {"type":"Feature","id":"area-kananaskis",
         "geometry":{"type":"Polygon","coordinates":[[[-115.5,50.6],[-115.5,51.2],[-114.6,51.2],[-114.6,50.6],[-115.5,50.6]]]},
         "properties":{"name":"Kananaskis"}},
        {"type":"Feature","id":"area-southcoast",
         "geometry":{"type":"Polygon","coordinates":[[[-123.3,49.3],[-123.3,49.9],[-122.4,49.9],[-122.4,49.3],[-123.3,49.3]]]},
         "properties":{"name":"South Coast"}},
        {"type":"Feature","id":"area-nogeom","geometry":null,"properties":{"name":"No Geometry"}}
      ]
    }"#;

    // Bulletins: a real winter rating (Considerable/Moderate/Low), an off-season
    // region (all "norating"), and a rated region whose polygon has no geometry.
    const PRODUCTS: &str = r#"[
      {"id":"p1","url":"https://avalanche.ca/forecasts/kananaskis","area":{"id":"area-kananaskis","name":"Kananaskis"},
       "report":{"dateIssued":"2026-02-23T14:00:00Z","validUntil":"2026-02-24T14:00:00Z",
         "dangerRatings":[
           {"date":{"display":"Monday"},"ratings":{
             "alp":{"display":"Alpine","rating":{"value":"considerable","display":"3 - Considerable"}},
             "tln":{"display":"Treeline","rating":{"value":"moderate","display":"2 - Moderate"}},
             "btl":{"display":"Below Treeline","rating":{"value":"low","display":"1 - Low"}}}}]}},
      {"id":"p2","area":{"id":"area-southcoast","name":"South Coast"},
       "report":{"dateIssued":"2026-06-20T14:00:00Z",
         "dangerRatings":[
           {"date":{"display":"Saturday"},"ratings":{
             "alp":{"rating":{"value":"norating","display":"No rating"}},
             "tln":{"rating":{"value":"norating","display":"No rating"}},
             "btl":{"rating":{"value":"norating","display":"No rating"}}}}]}},
      {"id":"p3","area":{"id":"area-nogeom","name":"No Geometry"},
       "report":{"dangerRatings":[{"ratings":{"alp":{"rating":{"value":"high","display":"4 - High"}}}}]}}
    ]"#;

    #[test]
    fn parses_fixture_dropping_offseason_and_unplaceable() {
        let ev = parse_avalanche_ca(PRODUCTS, AREAS).unwrap();
        // South Coast (all norating) dropped; No-Geometry region (no polygon) dropped.
        assert_eq!(ev.len(), 1);

        assert_eq!(ev[0].id, "avalanche-ca-area-kananaskis");
        assert_eq!(ev[0].kind, EventKind::Weather);
        assert_eq!(ev[0].title, "Avalanche forecast — Kananaskis");
        // Peak band is Alpine "considerable" -> 0.65.
        assert!((ev[0].severity.value() - 0.65).abs() < 1e-9);
        // Centroid sits inside the Kananaskis box.
        let g = ev[0].geo.unwrap();
        assert!((g.lat - 50.9).abs() < 0.3 && (g.lon + 115.05).abs() < 0.3);
        assert_eq!(
            danger_chip(&ev[0].raw).as_deref(),
            Some("Alpine Considerable · Treeline Moderate · Below Low")
        );
        // dateIssued parsed.
        assert_eq!(ev[0].time.format("%Y-%m-%d").to_string(), "2026-02-23");
    }

    #[test]
    fn off_season_all_norating_is_ok_not_error() {
        // Every region with no numeric rating -> zero plotted events, not a failure.
        let products = r#"[
          {"area":{"id":"area-southcoast","name":"South Coast"},
           "report":{"dangerRatings":[{"ratings":{
             "alp":{"rating":{"value":"norating"}},"tln":{"rating":{"value":"norating"}},
             "btl":{"rating":{"value":"norating"}}}}]}}
        ]"#;
        assert!(parse_avalanche_ca(products, AREAS).unwrap().is_empty());
        // An empty bulletin list is fine too.
        assert!(parse_avalanche_ca("[]", AREAS).unwrap().is_empty());
    }

    #[test]
    fn errors_on_bad_input() {
        // Products payload not JSON (e.g. an HTML error page).
        assert!(parse_avalanche_ca("<html>403 Forbidden</html>", AREAS).is_err());
        // Areas payload has no features array.
        assert!(parse_avalanche_ca(PRODUCTS, r#"{"oops":true}"#).is_err());
        // Products is an object without a recognizable list.
        assert!(parse_avalanche_ca(r#"{"nope":1}"#, AREAS).is_err());
    }

    #[test]
    fn danger_scale_ladders_and_chip_omits_unrated_bands() {
        assert!((danger_rank(&Value::from("Extreme")) - 1.0).abs() < 1e-9);
        assert!((danger_rank(&Value::from("4 - High")) - 0.85).abs() < 1e-9);
        assert!((danger_rank(&Value::from(2)) - 0.45).abs() < 1e-9);
        assert!((danger_rank(&Value::from("norating"))).abs() < 1e-9);
        // Chip omits bands with no rating (here only alpine is rated).
        let report = serde_json::json!({"dangerRatings":[{"ratings":{
            "alp":{"rating":{"value":"high"}},
            "tln":{"rating":{"value":"norating"}}}}]});
        assert_eq!(danger_chip(&report).as_deref(), Some("Alpine High"));
        // A report with no ratings at all -> no chip.
        let empty = serde_json::json!({"dangerRatings":[{"ratings":{
            "alp":{"rating":{"value":"spring"}}}}]});
        assert_eq!(danger_chip(&empty), None);
    }
}
