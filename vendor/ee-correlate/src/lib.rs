//! `ee-correlate` — correlation & analysis over normalized [`ee_core::Event`]s.
//!
//! Dashboards surface *individual* feeds; the analytical value is in relating them.
//! This crate is the home for those cross-event computations. The first primitive is
//! **spatial-temporal clustering**: grouping events that are close in both space and
//! time into incident clusters, so a swarm of aftershocks (or a burst of fires, or a
//! cluster of strikes) reads as one situation instead of fifty disconnected pins.
//!
//! A second primitive, [`freshness`], watches the feeds themselves rather than the
//! events: it tracks each source's data recency against its declared cadence and
//! flags the ones that have gone quiet — the "is this layer still live?" signal a
//! dashboard needs.
//!
//! A third primitive, [`rollup`], answers "how hot is each place I care about?": it
//! buckets located events into caller-defined regions and reduces each to a single
//! comparable composite severity score, ranked worst-first — the regional-risk panel
//! a dashboard puts beside the map.
//!
//! A fourth primitive, [`cii`], is the **Country Intelligence Index**: a composite
//! per-country risk score that, unlike the rollup, is weighted by *category salience*
//! and divided by the whole category taxonomy — so a country in trouble across several
//! salient fronts (conflict + cyber + disaster) ranks above one with a single loud feed.
//!
//! A fifth primitive, [`convergence`], is **signal-convergence detection**: it reuses the
//! spatial-temporal cluster as the unit of "co-located in space and time", then surfaces
//! only the clusters that span *multiple distinct signal streams* (military + disaster +
//! cyber + …) — the cross-stream alignment that marks a developing crisis rather than a
//! single loud feed.
//!
//! A sixth group, [`maptools`], reproduces the interactive **map tools** — [`locate`]
//! (nearest events to a point), [`track`]/[`tracks`] (a moving entity's path over
//! time), and [`area_intel`] (a single-viewport bbox summary) — the click-on-the-map
//! queries a dashboard runs against the events it has plotted.
//!
//! A seventh primitive, [`finance`], is the **Finance Radar**: the market-only counterpart
//! to the country index. It splits the [`ee_core::EventKind::Market`] stream into seven
//! market-segment spokes (equities / crypto / commodities / energy / bonds / forex / macro)
//! and reduces them to one systemic market-stress composite — broad stress across segments
//! ranks above a single loud ticker.
//!
//! An eighth primitive, [`crossdomain`], is **cross-domain correlation**: the lexical-and-
//! temporal counterpart to the spatial [`convergence`]. It links the *same situation*
//! surfacing across *different* domains — the quake ↔ headline ↔ market cascade — by shared
//! title keywords within a time window, so geo-less news and market events (which
//! convergence must drop) are first-class.
//!
//! A ninth primitive, [`nexus`], is the **Event Nexus** causal graph: it extracts named
//! entities (people / orgs / places / assets) from titles, draws directed, recency-weighted
//! causal edges *forward in time* between related events, and lets an analyst walk a focus event
//! backward to its **root cause** and forward to its **consequences** — plus the actor
//! co-occurrence network and the topical threads that fall out of the graph.
//!
//! Everything here is pure: it operates on a slice of events and returns derived
//! structures, with no I/O — so it is fully unit-testable offline.

pub mod cii;
pub mod cluster;
pub mod convergence;
pub mod crossdomain;
pub mod finance;
pub mod freshness;
pub mod maptools;
pub mod nexus;
pub mod rollup;

pub use cii::{cii, CategoryScore, CategoryWeights, CiiParams, CiiReport, CountryIndex, RiskLevel};
pub use cluster::{cluster, Cluster, ClusterParams};
pub use convergence::{
    convergence, Convergence, ConvergenceParams, ConvergenceReport, DomainSignal, SignalDomain,
};
pub use crossdomain::{correlate, CrossCorrelation, CrossParams, CrossReport};
pub use finance::{
    radar, FinanceRadar, MarketSegment, RadarParams, SegmentLexicon, SegmentReading,
    SegmentWeights, StressLevel,
};
pub use freshness::{
    Freshness, FreshnessMonitor, FreshnessParams, FreshnessSummary, SourceClock, SourceReport,
};
pub use maptools::{
    area_intel, locate, track, tracks, AreaReport, Located, LocateParams, TrackPath, TrackPoint,
};
pub use nexus::{
    build as nexus, extract_entities, ActorLink, ActorNetwork, ActorNode, Cascade, CausalEdge,
    Entity, EntityKind, Nexus, NexusParams, Thread,
};
pub use rollup::{rollup, RegionRollup, RollupParams, RollupReport};
