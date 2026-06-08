//! `ee-view` — frontend-agnostic presentation primitives over normalized
//! [`ee_core::Event`]s, feeding the future dashboard.
//!
//! Two primitives so far:
//! - **GeoJSON export** ([`geojson`]): turn located events into a standard
//!   `FeatureCollection` (RFC 7946) that any map frontend can render without
//!   knowing anything about provider formats.
//! - **Filtering** ([`filter`]): a composable time-window / bounding-box / kind
//!   predicate ([`EventFilter`]) behind every time slider, map viewport, and
//!   layer toggle.
//! - **Cinema Mode** ([`cinema`]): a playback-ready, time-ordered reel of
//!   geo-located events (≤1 h window) for a spinning-globe frontend.
//! - **Layers** ([`layers`]): a registry of toggleable, grouped, styled map layers —
//!   one per event domain — plus the routing/tally behind a layer-toggle panel.
//! - **Per-layer GeoJSON** ([`layer_geojson`]): split an event stream into one
//!   RFC 7946 `FeatureCollection` per visible layer, each tagged with its layer's
//!   style metadata — the data shape behind a styled, toggleable multi-layer map.
//! - **Widgets** ([`widgets`]): the data shapes behind dashboard widgets — a ticker,
//!   a table, a timeline histogram, and a gauge — all sharing one severity colour code.
//! - **Base maps** ([`basemap`]): the open base-map style catalog (CARTO / Esri /
//!   OpenTopoMap / OpenFreeMap / OSM / Natural Earth projections / 3D globe) plus the
//!   coordinate output — Web-Mercator tile math for 2D, sphere projection for the globe.
//! - **Decks** ([`decks`]): the six one-click pre-built decks (Command Center, War &
//!   Conflict, Maritime & Trade, Elections & Politics, Humanitarian, Cyber &
//!   Technology) — each a curated layer focus plus a nine-widget layout, rendered from
//!   the event stream by composing [`layers`], [`filter`], and [`widgets`].

pub mod basemap;
pub mod cinema;
pub mod decks;
pub mod filter;
pub mod geojson;
pub mod layer_geojson;
pub mod layers;
pub mod widgets;

pub use basemap::{
    by_id as base_style, default_style, project_to_globe, registry as base_styles, tile_index,
    BaseStyle, GlobePoint, Provider, StyleKind, Theme,
};
pub use cinema::{CinemaConfig, CinemaFrame, CinemaReel};
pub use decks::{
    deck, registry as deck_registry, Deck, DeckId, DeckWidget, RenderedDeck, RenderedPanel,
    RenderedWidget, WidgetArchetype,
};
pub use filter::EventFilter;
pub use geojson::{to_feature_collection, to_geojson_string};
pub use layer_geojson::{export_layers, LayerFeatures, LayeredGeoJson};
pub use layers::{
    descriptor_for, registry as layer_registry, tally as tally_layers, GroupTally, LayerDescriptor,
    LayerGroup, LayerReport, LayerSet, LayerTally,
};
pub use widgets::{
    gauge, table, ticker, timeline, Gauge, SeverityBand, TableRow, TableSort, TickerItem, Timeline,
    TimelineBucket,
};
