//! Per-layer GeoJSON export — split a single event stream into one standard
//! `FeatureCollection` per visible map layer, each carrying its layer's styling
//! metadata, so a frontend can render styled, independently-toggleable layers with
//! no knowledge of provider formats.
//!
//! Where [`crate::geojson`] flattens *every* located event into one collection, a
//! real map renders feeds as separate, separately-styled, separately-toggleable
//! overlays. This module is that split: it routes events through a [`LayerSet`]
//! (same visibility model as [`crate::layers::tally`]) and emits a
//! [`LayerFeatures`] per visible layer — the layer descriptor (id, label, group,
//! colour, icon) paired with an RFC 7946 `FeatureCollection` of just that layer's
//! located events.
//!
//! Reproduces World Monitor / SitDeck's per-layer rendering — "**GeoJSON/feature
//! export per layer (frontend-agnostic rendering)**" (capability-map: *Map layers &
//! presentation*; `sitdeck-features.md` *Map layers (95, by group)*). Pure and
//! deterministic; the network never enters here.

use ee_core::Event;
use serde::Serialize;
use serde_json::Value;

use crate::geojson::to_feature_collection;
use crate::layers::{LayerDescriptor, LayerSet};

/// One visible layer's renderable payload: its style/grouping descriptor plus a
/// standalone GeoJSON `FeatureCollection` of that layer's located events.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LayerFeatures {
    /// Styling + grouping metadata (id, label, group, colour, icon, …).
    pub layer: LayerDescriptor,
    /// RFC 7946 `FeatureCollection` for this layer (gains a `bbox` when non-empty).
    pub geojson: Value,
    /// Located events placed into this layer (== `geojson.features.len()`).
    pub located: usize,
    /// Events of this layer's kind that carry no coordinate and so were omitted.
    pub omitted: usize,
}

/// An event stream split into per-layer GeoJSON — the data shape behind a styled,
/// toggleable multi-layer map.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LayeredGeoJson {
    /// One entry per *visible* layer, in registry order. Quiet visible layers are
    /// included (empty `FeatureCollection`) so the layer set is stable across refreshes.
    pub layers: Vec<LayerFeatures>,
    /// Events whose layer is toggled off (never rendered).
    pub hidden: usize,
    /// Located-less events across visible layers (counted, not plotted).
    pub omitted: usize,
    /// Total events considered.
    pub total: usize,
}

/// Split `events` into one GeoJSON `FeatureCollection` per layer that `set` has
/// switched on.
///
/// Pure and deterministic. Events routed to a hidden layer count toward `hidden`;
/// geo-less events of a visible kind count toward that layer's (and the report's)
/// `omitted`; every visible layer appears, even when empty; layers follow the
/// registry's canonical order.
pub fn export_layers(set: &LayerSet, events: &[Event]) -> LayeredGeoJson {
    let mut hidden = 0usize;

    let layers: Vec<LayerFeatures> = set
        .visible_layers()
        .into_iter()
        .map(|layer| {
            // Located events of this kind feed the collection; geo-less ones are tallied.
            let mut located_events: Vec<Event> = Vec::new();
            let mut omitted = 0usize;
            for e in events {
                if e.kind != layer.kind {
                    continue;
                }
                if e.geo.is_some() {
                    located_events.push(e.clone());
                } else {
                    omitted += 1;
                }
            }
            LayerFeatures {
                geojson: to_feature_collection(&located_events),
                located: located_events.len(),
                omitted,
                layer,
            }
        })
        .collect();

    for e in events {
        if !set.is_visible(e.kind) {
            hidden += 1;
        }
    }

    let omitted = layers.iter().map(|l| l.omitted).sum();
    LayeredGeoJson { layers, hidden, omitted, total: events.len() }
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
            url: None,
            raw: Value::Null,
        }
    }

    fn layer(out: &LayeredGeoJson, kind: EventKind) -> &LayerFeatures {
        out.layers.iter().find(|l| l.layer.kind == kind).expect("layer present")
    }

    #[test]
    fn splits_stream_into_one_collection_per_kind() {
        let set = LayerSet::all_visible();
        let events = vec![
            ev("q1", EventKind::Earthquake, Geo::new(10.0, 20.0)),
            ev("q2", EventKind::Earthquake, Geo::new(11.0, 21.0)),
            ev("f1", EventKind::Wildfire, Geo::new(-5.0, 30.0)),
        ];
        let out = export_layers(&set, &events);

        // Each kind's features land only in its own layer.
        let quakes = layer(&out, EventKind::Earthquake);
        assert_eq!(quakes.located, 2);
        assert_eq!(quakes.geojson["features"].as_array().unwrap().len(), 2);

        let fires = layer(&out, EventKind::Wildfire);
        assert_eq!(fires.located, 1);
        assert_eq!(fires.geojson["features"][0]["properties"]["id"], "f1");

        assert_eq!(out.total, 3);
        assert_eq!(out.hidden, 0);
        assert_eq!(out.omitted, 0);
    }

    #[test]
    fn geoless_events_are_omitted_and_counted() {
        let set = LayerSet::all_visible();
        let events = vec![
            ev("c1", EventKind::Cyber, None), // CVEs never carry a coordinate
            ev("q1", EventKind::Earthquake, Geo::new(1.0, 2.0)),
            ev("q2", EventKind::Earthquake, None), // a quake with no fix
        ];
        let out = export_layers(&set, &events);

        let cyber = layer(&out, EventKind::Cyber);
        assert_eq!((cyber.located, cyber.omitted), (0, 1));
        // An empty collection has no bbox.
        assert!(cyber.geojson.get("bbox").is_none());
        assert_eq!(cyber.geojson["features"].as_array().unwrap().len(), 0);

        let quakes = layer(&out, EventKind::Earthquake);
        assert_eq!((quakes.located, quakes.omitted), (1, 1));

        // Report-level omitted is the sum across layers.
        assert_eq!(out.omitted, 2);
    }

    #[test]
    fn hidden_layers_drop_out_and_count_toward_hidden() {
        let mut set = LayerSet::all_visible();
        set.set(EventKind::News, false);
        let events = vec![
            ev("n1", EventKind::News, Geo::new(0.0, 0.0)),
            ev("n2", EventKind::News, None),
            ev("q1", EventKind::Earthquake, Geo::new(5.0, 5.0)),
        ];
        let out = export_layers(&set, &events);

        // No News layer is emitted at all.
        assert!(!out.layers.iter().any(|l| l.layer.kind == EventKind::News));
        // Both news events (located + geo-less) are hidden, not omitted.
        assert_eq!(out.hidden, 2);
        assert_eq!(out.omitted, 0);
    }

    #[test]
    fn each_collection_is_valid_geojson_with_style_metadata_and_bbox() {
        let set = LayerSet::all_visible();
        let events = vec![
            ev("a", EventKind::Earthquake, Geo::new(10.0, -20.0)),
            ev("b", EventKind::Earthquake, Geo::new(-5.0, 30.0)),
        ];
        let out = export_layers(&set, &events);
        let quakes = layer(&out, EventKind::Earthquake);

        // Standard FeatureCollection envelope.
        assert_eq!(quakes.geojson["type"], "FeatureCollection");
        // bbox = [min_lon, min_lat, max_lon, max_lat] across the two points.
        let bbox = quakes.geojson["bbox"].as_array().unwrap();
        assert_eq!(bbox[0].as_f64().unwrap(), -20.0);
        assert_eq!(bbox[1].as_f64().unwrap(), -5.0);
        assert_eq!(bbox[2].as_f64().unwrap(), 30.0);
        assert_eq!(bbox[3].as_f64().unwrap(), 10.0);

        // The layer's own descriptor rides alongside for styling.
        assert_eq!(quakes.layer.id, "quakes");
        assert_eq!(quakes.layer.color, "#d7263d");
        assert_eq!(quakes.layer.icon, "quake");
    }

    #[test]
    fn visible_layers_are_total_and_follow_registry_order() {
        // Default set hides `Other`; everything else is on.
        let out = export_layers(&LayerSet::default(), &[]);
        assert!(out.layers.iter().all(|l| l.located == 0 && l.omitted == 0));
        assert!(!out.layers.iter().any(|l| l.layer.kind == EventKind::Other));

        // Order matches the layer registry's order, filtered to visible layers.
        let want: Vec<_> = crate::layers::registry()
            .into_iter()
            .filter(|d| d.kind != EventKind::Other)
            .map(|d| d.id)
            .collect();
        let got: Vec<_> = out.layers.iter().map(|l| l.layer.id).collect();
        assert_eq!(got, want);
    }

    #[test]
    fn report_serializes_to_json() {
        let out = export_layers(
            &LayerSet::default(),
            &[ev("q", EventKind::Earthquake, Geo::new(1.0, 2.0))],
        );
        let json = serde_json::to_string(&out).unwrap();
        assert!(json.contains("\"quakes\""));
        assert!(json.contains("\"FeatureCollection\""));
        assert!(json.contains("\"hidden\""));
    }
}
