//! Map-layer registry — turn each event domain into a toggleable map layer.
//!
//! Every dashboard that plots feeds onto a map needs a *layer model*: a stable set
//! of named, styled, groupable layers the user can switch on and off, plus the logic
//! that routes each event to its layer and tallies what is currently visible. This
//! module is that model, frontend-agnostic.
//!
//! Reproduces World Monitor / SitDeck's layer system foundation — "**95 toggleable
//! map layers (8 groups)**" (`sitdeck-features.md` *Map layers (95, by group)*;
//! capability-map: *Map layers & presentation → Layer registry: each Source domain →
//! a toggleable map-layer descriptor (group, style, icon)* and *8 layer groups*).
//!
//! Each [`crate::EventKind`] maps to exactly one [`LayerDescriptor`] (id, label,
//! [`LayerGroup`], color, icon, default visibility). A [`LayerSet`] holds toggle
//! state; [`tally`] partitions an event slice into the visible layers and reports
//! per-layer / per-group counts (and what was hidden) — the data shape behind a
//! layer-toggle panel.

use std::collections::HashMap;

use ee_core::{Event, EventKind};
use serde::Serialize;

/// The eight top-level layer groups every layer belongs to. Mirrors the catalog's
/// grouping (environment / security / infra / aviation-maritime / military /
/// humanitarian / space / political). Some groups have no [`EventKind`] yet — the
/// model is deliberately forward-looking toward the full layer surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerGroup {
    Environment,
    Security,
    Infrastructure,
    AviationMaritime,
    Military,
    Humanitarian,
    Space,
    Political,
}

impl LayerGroup {
    /// Canonical order — used everywhere groups are listed, so panels stay stable.
    pub const ALL: [LayerGroup; 8] = [
        LayerGroup::Environment,
        LayerGroup::Security,
        LayerGroup::Infrastructure,
        LayerGroup::AviationMaritime,
        LayerGroup::Military,
        LayerGroup::Humanitarian,
        LayerGroup::Space,
        LayerGroup::Political,
    ];

    /// Stable machine id (for config keys / serialization).
    pub fn id(self) -> &'static str {
        match self {
            LayerGroup::Environment => "environment",
            LayerGroup::Security => "security",
            LayerGroup::Infrastructure => "infrastructure",
            LayerGroup::AviationMaritime => "aviation_maritime",
            LayerGroup::Military => "military",
            LayerGroup::Humanitarian => "humanitarian",
            LayerGroup::Space => "space",
            LayerGroup::Political => "political",
        }
    }

    /// Human-readable label for a group header.
    pub fn label(self) -> &'static str {
        match self {
            LayerGroup::Environment => "Environment",
            LayerGroup::Security => "Security & Intelligence",
            LayerGroup::Infrastructure => "Data & Infrastructure",
            LayerGroup::AviationMaritime => "Aviation & Maritime",
            LayerGroup::Military => "Military & Defense",
            LayerGroup::Humanitarian => "Humanitarian & Crisis",
            LayerGroup::Space => "Space & Aerospace",
            LayerGroup::Political => "Political",
        }
    }

    /// Index of this group in [`LayerGroup::ALL`] (its canonical sort rank).
    fn order(self) -> usize {
        LayerGroup::ALL.iter().position(|g| *g == self).unwrap_or(usize::MAX)
    }
}

/// A single toggleable map layer: how one event domain is named, grouped, and styled.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct LayerDescriptor {
    /// Stable layer id (e.g. `"quakes"`), suitable as a config / toggle key.
    pub id: &'static str,
    /// Human-readable layer name.
    pub label: &'static str,
    /// The group this layer lives under.
    pub group: LayerGroup,
    /// The event domain this layer renders.
    pub kind: EventKind,
    /// Suggested marker color as a `#rrggbb` hex string (a style hint, not a mandate).
    pub color: &'static str,
    /// Short icon token a frontend maps to a glyph/sprite.
    pub icon: &'static str,
    /// Whether the layer is on by default in a fresh layout.
    pub default_visible: bool,
}

/// Every [`EventKind`], in the order layers are listed.
const ALL_KINDS: [EventKind; 13] = [
    EventKind::Earthquake,
    EventKind::Wildfire,
    EventKind::Volcano,
    EventKind::Weather,
    EventKind::AirQuality,
    EventKind::Aircraft,
    EventKind::Vessel,
    EventKind::Conflict,
    EventKind::Cyber,
    EventKind::Market,
    EventKind::Health,
    EventKind::News,
    EventKind::Other,
];

/// The layer descriptor for an event domain. Total: every [`EventKind`] has a layer.
pub fn descriptor_for(kind: EventKind) -> LayerDescriptor {
    use EventKind::*;
    use LayerGroup::*;
    let (id, label, group, color, icon, default_visible) = match kind {
        Earthquake => ("quakes", "Earthquakes", Environment, "#d7263d", "quake", true),
        Wildfire => ("wildfires", "Wildfires & Thermal", Environment, "#ff6b35", "fire", true),
        Volcano => ("volcanoes", "Volcanoes", Environment, "#ff5630", "volcano", true),
        Weather => ("weather", "Weather & Storms", Environment, "#3a86ff", "storm", true),
        AirQuality => ("air_quality", "Air Quality", Environment, "#a06cd5", "haze", true),
        Aircraft => ("flights", "Aircraft", AviationMaritime, "#8338ec", "plane", true),
        Vessel => ("vessels", "Vessels (AIS)", AviationMaritime, "#0077b6", "ship", true),
        Conflict => ("conflicts", "Armed Conflict", Security, "#6a040f", "conflict", true),
        Cyber => ("cyber", "Cyber Threats", Security, "#b5179e", "shield", true),
        Market => ("markets", "Markets & Finance", Infrastructure, "#2a9d8f", "chart", true),
        Health => ("health", "Disease & Health", Humanitarian, "#06d6a0", "health", true),
        News => ("news", "News & Media", Infrastructure, "#fca311", "news", true),
        Other => ("other", "Other Signals", Infrastructure, "#6c757d", "dot", false),
    };
    LayerDescriptor { id, label, group, kind, color, icon, default_visible }
}

/// The full layer registry — one descriptor per event domain, in stable order.
pub fn registry() -> Vec<LayerDescriptor> {
    ALL_KINDS.into_iter().map(descriptor_for).collect()
}

/// Mutable on/off state over the layer registry. Defaults follow each descriptor's
/// `default_visible`; toggles are explicit so a saved layout round-trips.
#[derive(Debug, Clone)]
pub struct LayerSet {
    visible: HashMap<EventKind, bool>,
}

impl Default for LayerSet {
    /// Visibility taken from each layer's `default_visible`.
    fn default() -> Self {
        Self {
            visible: ALL_KINDS
                .into_iter()
                .map(|k| (k, descriptor_for(k).default_visible))
                .collect(),
        }
    }
}

impl LayerSet {
    /// A layout with every layer turned on.
    pub fn all_visible() -> Self {
        Self { visible: ALL_KINDS.into_iter().map(|k| (k, true)).collect() }
    }

    /// A layout with every layer turned off.
    pub fn none_visible() -> Self {
        Self { visible: ALL_KINDS.into_iter().map(|k| (k, false)).collect() }
    }

    /// Whether the layer for `kind` is currently on.
    pub fn is_visible(&self, kind: EventKind) -> bool {
        self.visible.get(&kind).copied().unwrap_or(false)
    }

    /// Set the layer for `kind` on or off.
    pub fn set(&mut self, kind: EventKind, on: bool) {
        self.visible.insert(kind, on);
    }

    /// Flip the layer for `kind`; returns its new state.
    pub fn toggle(&mut self, kind: EventKind) -> bool {
        let next = !self.is_visible(kind);
        self.visible.insert(kind, next);
        next
    }

    /// The descriptors of the currently-visible layers, in registry order.
    pub fn visible_layers(&self) -> Vec<LayerDescriptor> {
        registry().into_iter().filter(|d| self.is_visible(d.kind)).collect()
    }
}

/// Per-layer counts within a [`tally`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LayerTally {
    pub layer: LayerDescriptor,
    /// Events routed to this layer.
    pub total: usize,
    /// Of those, how many carry coordinates (i.e. are actually plottable).
    pub located: usize,
}

/// Per-group rollup within a [`tally`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GroupTally {
    pub group: LayerGroup,
    pub total: usize,
    pub located: usize,
}

/// What a slice of events looks like once routed into the visible layers — the data
/// shape behind a layer-toggle panel.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LayerReport {
    /// Visible layers, busiest-first within canonical group order. Includes visible
    /// layers with zero events (so the toggle panel is stable across refreshes).
    pub layers: Vec<LayerTally>,
    /// Visible groups (those with ≥1 visible layer), in canonical order, with totals.
    pub groups: Vec<GroupTally>,
    /// Events whose layer is toggled off.
    pub hidden: usize,
    /// Total events considered.
    pub total: usize,
}

/// Route `events` into the layers `set` has switched on and tally the result.
///
/// Pure and deterministic: events going to a hidden layer count toward `hidden`;
/// every visible layer appears (even at zero); layers sort by group order then by
/// `total` descending then id; groups appear only if they have a visible layer.
pub fn tally(set: &LayerSet, events: &[Event]) -> LayerReport {
    // Accumulate (total, located) per visible kind.
    let mut counts: HashMap<EventKind, (usize, usize)> = HashMap::new();
    let mut hidden = 0usize;
    for ev in events {
        if set.is_visible(ev.kind) {
            let entry = counts.entry(ev.kind).or_insert((0, 0));
            entry.0 += 1;
            if ev.geo.is_some() {
                entry.1 += 1;
            }
        } else {
            hidden += 1;
        }
    }

    // One tally per visible layer (including the quiet ones).
    let mut layers: Vec<LayerTally> = set
        .visible_layers()
        .into_iter()
        .map(|layer| {
            let (total, located) = counts.get(&layer.kind).copied().unwrap_or((0, 0));
            LayerTally { layer, total, located }
        })
        .collect();
    layers.sort_by(|a, b| {
        a.layer
            .group
            .order()
            .cmp(&b.layer.group.order())
            .then(b.total.cmp(&a.total))
            .then(a.layer.id.cmp(b.layer.id))
    });

    // Roll the visible layers up to their groups, canonical order.
    let mut groups: Vec<GroupTally> = Vec::new();
    for group in LayerGroup::ALL {
        let mut total = 0usize;
        let mut located = 0usize;
        let mut present = false;
        for t in &layers {
            if t.layer.group == group {
                present = true;
                total += t.total;
                located += t.located;
            }
        }
        if present {
            groups.push(GroupTally { group, total, located });
        }
    }

    LayerReport { layers, groups, hidden, total: events.len() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use ee_core::{Geo, Severity};

    fn ev(kind: EventKind, located: bool) -> Event {
        Event {
            id: format!("{kind:?}-{located}-{}", rand_ish(kind, located)),
            source_id: "test".into(),
            kind,
            title: format!("{kind:?}"),
            time: Utc.timestamp_opt(1_700_000_000, 0).single().unwrap(),
            geo: if located { Geo::new(10.0, 20.0) } else { None },
            severity: Severity::new(0.5),
            url: None,
            raw: serde_json::Value::Null,
        }
    }

    // Tiny id disambiguator so repeated events don't collide in any downstream set.
    fn rand_ish(kind: EventKind, located: bool) -> u64 {
        (kind as u64) * 2 + located as u64
    }

    #[test]
    fn registry_is_total_and_unique() {
        let reg = registry();
        assert_eq!(reg.len(), ALL_KINDS.len());
        // Ids are unique.
        let mut ids: Vec<_> = reg.iter().map(|d| d.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), reg.len());
        // Every kind resolves to a descriptor naming that same kind.
        for k in ALL_KINDS {
            assert_eq!(descriptor_for(k).kind, k);
        }
        // Every group has an id and label.
        for g in LayerGroup::ALL {
            assert!(!g.id().is_empty() && !g.label().is_empty());
        }
    }

    #[test]
    fn default_set_follows_descriptor_flags() {
        let set = LayerSet::default();
        assert!(set.is_visible(EventKind::Earthquake));
        // `Other` ships off by default.
        assert!(!set.is_visible(EventKind::Other));
    }

    #[test]
    fn toggle_and_set_round_trip() {
        let mut set = LayerSet::none_visible();
        assert!(!set.is_visible(EventKind::Cyber));
        assert!(set.toggle(EventKind::Cyber)); // -> on
        assert!(set.is_visible(EventKind::Cyber));
        assert!(!set.toggle(EventKind::Cyber)); // -> off
        set.set(EventKind::Cyber, true);
        assert!(set.is_visible(EventKind::Cyber));
        assert_eq!(LayerSet::all_visible().visible_layers().len(), ALL_KINDS.len());
    }

    #[test]
    fn tally_routes_counts_and_hides() {
        let mut set = LayerSet::all_visible();
        set.set(EventKind::News, false); // hide news
        let events = vec![
            ev(EventKind::Earthquake, true),
            ev(EventKind::Earthquake, false), // geo-less quake
            ev(EventKind::Wildfire, true),
            ev(EventKind::Cyber, false), // cyber is never located
            ev(EventKind::News, true),   // -> hidden (layer off)
        ];
        let report = tally(&set, &events);

        assert_eq!(report.total, 5);
        assert_eq!(report.hidden, 1); // the news event

        // Earthquake layer: 2 total, 1 located.
        let quake = report
            .layers
            .iter()
            .find(|t| t.layer.kind == EventKind::Earthquake)
            .unwrap();
        assert_eq!((quake.total, quake.located), (2, 1));

        // Cyber: 1 total, 0 located.
        let cyber = report
            .layers
            .iter()
            .find(|t| t.layer.kind == EventKind::Cyber)
            .unwrap();
        assert_eq!((cyber.total, cyber.located), (1, 0));

        // News layer is not present (hidden), even though news events existed.
        assert!(!report.layers.iter().any(|t| t.layer.kind == EventKind::News));

        // Environment group rolls up quake(2)+wildfire(1) = 3 total, 2 located.
        let env = report
            .groups
            .iter()
            .find(|g| g.group == LayerGroup::Environment)
            .unwrap();
        assert_eq!((env.total, env.located), (3, 2));
    }

    #[test]
    fn tally_orders_by_group_then_busiest() {
        // Two environment layers; wildfire busier than quakes -> wildfire first.
        let set = LayerSet::all_visible();
        let mut events = vec![ev(EventKind::Earthquake, true)];
        events.push(ev(EventKind::Wildfire, true));
        // make wildfire busier with a distinct id
        let mut extra = ev(EventKind::Wildfire, false);
        extra.id = "wildfire-extra".into();
        events.push(extra);

        let report = tally(&set, &events);
        let env_layers: Vec<_> = report
            .layers
            .iter()
            .filter(|t| t.layer.group == LayerGroup::Environment)
            .collect();
        assert_eq!(env_layers[0].layer.kind, EventKind::Wildfire); // busiest first
        assert_eq!(env_layers[0].total, 2);

        // Groups appear in canonical order: Environment before Security.
        let order: Vec<_> = report.groups.iter().map(|g| g.group).collect();
        let env_idx = order.iter().position(|g| *g == LayerGroup::Environment);
        let sec_idx = order.iter().position(|g| *g == LayerGroup::Security);
        if let (Some(e), Some(s)) = (env_idx, sec_idx) {
            assert!(e < s);
        }
    }

    #[test]
    fn report_serializes_to_json() {
        let report = tally(&LayerSet::default(), &[ev(EventKind::Earthquake, true)]);
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("\"environment\""));
        assert!(json.contains("\"quakes\""));
    }
}
