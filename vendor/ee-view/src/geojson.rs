//! GeoJSON export — turn located [`Event`]s into a standard GeoJSON
//! `FeatureCollection` (RFC 7946) so any map frontend can render them with no
//! knowledge of provider formats.
//!
//! Each located event becomes a `Point` feature; the event's normalized fields
//! (kind, title, time, severity, url, …) ride along as feature `properties`.
//! Events with no coordinate (`geo == None`, e.g. headlines or CVEs) cannot be
//! placed on a map and are omitted. When at least one point is included, the
//! collection carries a top-level `bbox` covering all of them.

use ee_core::Event;
use serde_json::{json, Value};

/// Build a GeoJSON `FeatureCollection` from located events.
///
/// Geo-less events are skipped. The result always has `type` and `features`;
/// it gains a `bbox` (`[min_lon, min_lat, max_lon, max_lat]`) when one or more
/// points are present.
pub fn to_feature_collection(events: &[Event]) -> Value {
    let mut features = Vec::new();
    // GeoJSON bbox order is [west, south, east, north] = [min_lon, min_lat, max_lon, max_lat].
    let mut bbox: Option<[f64; 4]> = None;

    for e in events {
        let Some(g) = e.geo else { continue };

        bbox = Some(match bbox {
            None => [g.lon, g.lat, g.lon, g.lat],
            Some([min_lon, min_lat, max_lon, max_lat]) => [
                min_lon.min(g.lon),
                min_lat.min(g.lat),
                max_lon.max(g.lon),
                max_lat.max(g.lat),
            ],
        });

        features.push(json!({
            "type": "Feature",
            // GeoJSON coordinate order is [longitude, latitude].
            "geometry": { "type": "Point", "coordinates": [g.lon, g.lat] },
            "properties": feature_properties(e),
        }));
    }

    let mut fc = json!({
        "type": "FeatureCollection",
        "features": features,
    });
    if let Some(b) = bbox {
        fc["bbox"] = json!(b);
    }
    fc
}

/// Convenience: [`to_feature_collection`] serialized to a compact JSON string.
pub fn to_geojson_string(events: &[Event]) -> String {
    to_feature_collection(events).to_string()
}

/// The normalized event fields exposed as GeoJSON feature properties.
fn feature_properties(e: &Event) -> Value {
    json!({
        "id": e.id,
        "source_id": e.source_id,
        // `EventKind` serializes to its snake_case tag, e.g. "earthquake".
        "kind": serde_json::to_value(e.kind).unwrap_or(Value::Null),
        "title": e.title,
        "time": e.time.to_rfc3339(),
        "severity": e.severity.value(),
        "url": e.url,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use ee_core::{EventKind, Geo, Severity};

    fn ev(id: &str, kind: EventKind, geo: Option<Geo>) -> Event {
        Event {
            id: id.to_string(),
            source_id: "test".to_string(),
            kind,
            title: format!("event {id}"),
            time: Utc.timestamp_opt(1_700_000_000, 0).single().unwrap(),
            geo,
            severity: Severity::new(0.5),
            url: Some("https://example.com".to_string()),
            raw: Value::Null,
        }
    }

    #[test]
    fn skips_geoless_events_and_keeps_located_ones() {
        let events = vec![
            ev("a", EventKind::Earthquake, Geo::new(38.1, -122.5)),
            ev("b", EventKind::Cyber, None), // no location -> omitted
        ];
        let fc = to_feature_collection(&events);

        assert_eq!(fc["type"], "FeatureCollection");
        let feats = fc["features"].as_array().unwrap();
        assert_eq!(feats.len(), 1);
        assert_eq!(feats[0]["properties"]["id"], "a");
    }

    #[test]
    fn coordinates_are_lon_lat_order() {
        let fc = to_feature_collection(&[ev("a", EventKind::Earthquake, Geo::new(38.1, -122.5))]);
        let coords = fc["features"][0]["geometry"]["coordinates"].as_array().unwrap();
        // [lon, lat] per RFC 7946 — not [lat, lon].
        assert_eq!(coords[0].as_f64().unwrap(), -122.5);
        assert_eq!(coords[1].as_f64().unwrap(), 38.1);
    }

    #[test]
    fn properties_carry_normalized_fields() {
        let fc = to_feature_collection(&[ev("a", EventKind::Wildfire, Geo::new(10.0, 20.0))]);
        let props = &fc["features"][0]["properties"];
        assert_eq!(props["source_id"], "test");
        assert_eq!(props["kind"], "wildfire");
        assert_eq!(props["severity"].as_f64().unwrap(), 0.5);
        assert_eq!(props["url"], "https://example.com");
        // chrono RFC3339 of 1_700_000_000s UTC.
        assert_eq!(props["time"], "2023-11-14T22:13:20+00:00");
    }

    #[test]
    fn bbox_spans_all_points() {
        let events = vec![
            ev("a", EventKind::Earthquake, Geo::new(10.0, -20.0)),
            ev("b", EventKind::Earthquake, Geo::new(-5.0, 30.0)),
            ev("c", EventKind::Earthquake, Geo::new(40.0, 5.0)),
        ];
        let fc = to_feature_collection(&events);
        let bbox = fc["bbox"].as_array().unwrap();
        // [min_lon, min_lat, max_lon, max_lat]
        assert_eq!(bbox[0].as_f64().unwrap(), -20.0);
        assert_eq!(bbox[1].as_f64().unwrap(), -5.0);
        assert_eq!(bbox[2].as_f64().unwrap(), 30.0);
        assert_eq!(bbox[3].as_f64().unwrap(), 40.0);
    }

    #[test]
    fn empty_or_geoless_input_has_no_bbox() {
        let fc = to_feature_collection(&[ev("b", EventKind::Cyber, None)]);
        assert_eq!(fc["features"].as_array().unwrap().len(), 0);
        assert!(fc.get("bbox").is_none());
    }

    #[test]
    fn string_helper_roundtrips_to_same_value() {
        let events = [ev("a", EventKind::Earthquake, Geo::new(1.0, 2.0))];
        let s = to_geojson_string(&events);
        let back: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(back, to_feature_collection(&events));
    }
}
