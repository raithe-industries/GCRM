//! Pre-built decks — one-click dashboard presets.
//!
//! A *deck* is a saved layout: a curated set of map layers plus a fixed grid of
//! widgets, tuned for one mission. SitDeck ships "**6 pre-built decks (9 widgets
//! each, one-click): Command Center · War & Conflict · Maritime & Trade · Elections
//! & Politics · Humanitarian · Cyber & Technology**" (`sitdeck-features.md` *Alerting
//! / export / decks*; capability-map: *Decks & alerting → Pre-built deck templates*).
//! This module reproduces that surface as frontend-agnostic data.
//!
//! Each [`Deck`] is a pure descriptor: a focus set of [`EventKind`]s (its enabled map
//! layers) and exactly nine [`DeckWidget`] slots. A deck composes the rest of
//! `ee-view`: it yields a [`crate::layers::LayerSet`] (which layers to switch on), an
//! [`crate::filter::EventFilter`] (restrict the stream to the deck's domains), and —
//! via [`Deck::render`] — populated widget data for every slot by reusing the
//! [`crate::widgets`] builders. Nothing here touches I/O; the only clock input is the
//! explicit `now` a timeline needs, so the whole module is unit-testable offline.

use chrono::{DateTime, Duration, Utc};
use ee_core::{Event, EventKind};
use serde::Serialize;

use crate::filter::EventFilter;
use crate::layers::{descriptor_for, tally, LayerGroup, LayerReport, LayerSet};
use crate::widgets::{gauge, table, ticker, timeline, Gauge, TableRow, TableSort, TickerItem, Timeline};

/// The six pre-built decks. Stable order — used wherever decks are listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeckId {
    CommandCenter,
    WarAndConflict,
    MaritimeAndTrade,
    ElectionsAndPolitics,
    Humanitarian,
    CyberAndTechnology,
}

impl DeckId {
    /// Canonical order — the catalog's deck list.
    pub const ALL: [DeckId; 6] = [
        DeckId::CommandCenter,
        DeckId::WarAndConflict,
        DeckId::MaritimeAndTrade,
        DeckId::ElectionsAndPolitics,
        DeckId::Humanitarian,
        DeckId::CyberAndTechnology,
    ];

    /// Stable machine id (config key / serialization).
    pub fn id(self) -> &'static str {
        match self {
            DeckId::CommandCenter => "command_center",
            DeckId::WarAndConflict => "war_and_conflict",
            DeckId::MaritimeAndTrade => "maritime_and_trade",
            DeckId::ElectionsAndPolitics => "elections_and_politics",
            DeckId::Humanitarian => "humanitarian",
            DeckId::CyberAndTechnology => "cyber_and_technology",
        }
    }
}

/// The widget archetypes a deck slot can hold. These map onto the [`crate::widgets`]
/// builders (plus a map panel backed by [`crate::layers`]) — the few data shapes the
/// catalog's many named widgets reduce to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WidgetArchetype {
    /// A grouped map-layer panel (a [`LayerReport`]).
    Map,
    /// A severity-ranked scrolling ticker.
    Ticker,
    /// A sortable event table.
    Table,
    /// A trailing-window activity histogram.
    Timeline,
    /// A single composite "how hot is this" needle.
    Gauge,
}

/// One slot in a deck's nine-widget grid.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct DeckWidget {
    /// Caption shown on the panel.
    pub title: &'static str,
    pub archetype: WidgetArchetype,
    /// Event domains feeding this slot. An empty slice means "the whole deck focus".
    pub kinds: &'static [EventKind],
}

/// A pre-built deck: a curated layer focus plus a fixed nine-widget layout.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Deck {
    pub id: DeckId,
    /// Human-readable deck name.
    pub name: &'static str,
    /// One-line description of the deck's mission.
    pub description: &'static str,
    /// The event domains this deck foregrounds — its enabled map layers, in priority
    /// order. Every widget's explicit `kinds` is a subset of this.
    pub focus: &'static [EventKind],
    /// Exactly nine widget slots.
    pub widgets: &'static [DeckWidget],
}

impl Deck {
    /// The deck's layer toggle state: only its [`Deck::focus`] domains switched on.
    pub fn layer_set(&self) -> LayerSet {
        let mut set = LayerSet::none_visible();
        for &k in self.focus {
            set.set(k, true);
        }
        set
    }

    /// A filter that keeps only the deck's focus domains.
    pub fn filter(&self) -> EventFilter {
        EventFilter::new().kinds(self.focus.iter().copied())
    }

    /// The distinct [`LayerGroup`]s the deck's focus spans, in canonical order.
    pub fn groups(&self) -> Vec<LayerGroup> {
        LayerGroup::ALL
            .into_iter()
            .filter(|g| self.focus.iter().any(|&k| descriptor_for(k).group == *g))
            .collect()
    }

    /// Effective kinds for a slot: its own `kinds`, or the whole focus if empty.
    fn slot_kinds(&self, w: &DeckWidget) -> Vec<EventKind> {
        if w.kinds.is_empty() {
            self.focus.to_vec()
        } else {
            w.kinds.to_vec()
        }
    }

    /// Populate every widget slot from `events`, reusing the `ee-view` builders.
    ///
    /// Each slot draws from the subset of `events` matching its effective kinds, then
    /// renders through the matching archetype builder (ticker / table / timeline /
    /// gauge / map). Pure and deterministic — `now` is the only clock input (the
    /// timeline's trailing window). `matched` counts events in the deck's focus.
    pub fn render(&self, events: &[Event], now: DateTime<Utc>) -> RenderedDeck {
        let matched = self.filter().apply(events).len();
        let panels = self
            .widgets
            .iter()
            .map(|w| {
                let kinds = self.slot_kinds(w);
                let filter = EventFilter::new().kinds(kinds.iter().copied());
                let subset: Vec<Event> =
                    events.iter().filter(|e| filter.matches(e)).cloned().collect();
                let data = match w.archetype {
                    WidgetArchetype::Map => {
                        let mut set = LayerSet::none_visible();
                        for &k in &kinds {
                            set.set(k, true);
                        }
                        RenderedWidget::Map(tally(&set, &subset))
                    }
                    WidgetArchetype::Ticker => {
                        RenderedWidget::Ticker(ticker(&subset, TICKER_MAX))
                    }
                    WidgetArchetype::Table => {
                        RenderedWidget::Table(table(&subset, TableSort::SeverityDesc))
                    }
                    WidgetArchetype::Timeline => RenderedWidget::Timeline(timeline(
                        &subset,
                        Duration::minutes(TIMELINE_BUCKET_MINS),
                        Duration::hours(TIMELINE_SPAN_HOURS),
                        now,
                    )),
                    WidgetArchetype::Gauge => {
                        RenderedWidget::Gauge(gauge(w.title, &subset))
                    }
                };
                RenderedPanel { title: w.title, archetype: w.archetype, count: subset.len(), data }
            })
            .collect();
        RenderedDeck { deck: self.id, matched, total: events.len(), panels }
    }
}

/// Default ticker depth for a deck slot.
const TICKER_MAX: usize = 12;
/// Default timeline bin width (minutes) and trailing window (hours).
const TIMELINE_BUCKET_MINS: i64 = 30;
const TIMELINE_SPAN_HOURS: i64 = 6;

/// A single populated widget panel within a [`RenderedDeck`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RenderedPanel {
    pub title: &'static str,
    pub archetype: WidgetArchetype,
    /// Events that fed this panel (after its kind filter).
    pub count: usize,
    pub data: RenderedWidget,
}

/// The data shape behind a rendered panel — one variant per archetype.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderedWidget {
    Map(LayerReport),
    Ticker(Vec<TickerItem>),
    Table(Vec<TableRow>),
    Timeline(Timeline),
    Gauge(Gauge),
}

/// A deck fully populated from an event stream — the data behind one-click "open deck".
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RenderedDeck {
    pub deck: DeckId,
    /// Events matching the deck's focus domains.
    pub matched: usize,
    /// Total events considered.
    pub total: usize,
    /// Nine populated panels, in layout order.
    pub panels: Vec<RenderedPanel>,
}

// --- The six decks ----------------------------------------------------------

use EventKind::*;
use WidgetArchetype::{Gauge as GaugeW, Map as MapW, Table as TableW, Ticker as TickerW, Timeline as TimelineW};

/// Convenience for a deck-focus-wide slot (no explicit kinds).
const ALL_FOCUS: &[EventKind] = &[];

const COMMAND_CENTER: Deck = Deck {
    id: DeckId::CommandCenter,
    name: "Command Center",
    description: "All-domain situational overview — the default watch floor.",
    focus: &[Earthquake, Wildfire, Weather, Conflict, Cyber, Vessel, Aircraft, Market, News],
    widgets: &[
        DeckWidget { title: "Global Situation Map", archetype: MapW, kinds: ALL_FOCUS },
        DeckWidget { title: "Overall Threat Level", archetype: GaugeW, kinds: ALL_FOCUS },
        DeckWidget { title: "Priority Wire", archetype: TickerW, kinds: ALL_FOCUS },
        DeckWidget { title: "Latest Events", archetype: TableW, kinds: ALL_FOCUS },
        DeckWidget { title: "Activity (6h)", archetype: TimelineW, kinds: ALL_FOCUS },
        DeckWidget { title: "Seismic", archetype: TickerW, kinds: &[Earthquake] },
        DeckWidget { title: "Conflict & Cyber", archetype: TableW, kinds: &[Conflict, Cyber] },
        DeckWidget { title: "Markets", archetype: GaugeW, kinds: &[Market] },
        DeckWidget { title: "Headlines", archetype: TickerW, kinds: &[News] },
    ],
};

const WAR_AND_CONFLICT: Deck = Deck {
    id: DeckId::WarAndConflict,
    name: "War & Conflict",
    description: "Armed conflict, military movement, and cyber operations.",
    focus: &[Conflict, Aircraft, Vessel, Cyber, News],
    widgets: &[
        DeckWidget { title: "Conflict Map", archetype: MapW, kinds: ALL_FOCUS },
        DeckWidget { title: "Escalation Gauge", archetype: GaugeW, kinds: &[Conflict] },
        DeckWidget { title: "Conflict Wire", archetype: TickerW, kinds: &[Conflict] },
        DeckWidget { title: "Incidents", archetype: TableW, kinds: &[Conflict] },
        DeckWidget { title: "Operational Tempo (6h)", archetype: TimelineW, kinds: &[Conflict, Aircraft, Vessel] },
        DeckWidget { title: "Military Air", archetype: TableW, kinds: &[Aircraft] },
        DeckWidget { title: "Naval Movements", archetype: TableW, kinds: &[Vessel] },
        DeckWidget { title: "Cyber Operations", archetype: TickerW, kinds: &[Cyber] },
        DeckWidget { title: "War Headlines", archetype: TickerW, kinds: &[News] },
    ],
};

const MARITIME_AND_TRADE: Deck = Deck {
    id: DeckId::MaritimeAndTrade,
    name: "Maritime & Trade",
    description: "Shipping, air cargo, sea-state weather, and market impact.",
    focus: &[Vessel, Aircraft, Weather, Market, News],
    widgets: &[
        DeckWidget { title: "Maritime Map", archetype: MapW, kinds: ALL_FOCUS },
        DeckWidget { title: "Trade Disruption", archetype: GaugeW, kinds: &[Vessel, Market] },
        DeckWidget { title: "Vessel Wire", archetype: TickerW, kinds: &[Vessel] },
        DeckWidget { title: "Vessel Traffic", archetype: TableW, kinds: &[Vessel] },
        DeckWidget { title: "Air Cargo", archetype: TableW, kinds: &[Aircraft] },
        DeckWidget { title: "Weather at Sea", archetype: TickerW, kinds: &[Weather] },
        DeckWidget { title: "Markets", archetype: GaugeW, kinds: &[Market] },
        DeckWidget { title: "Activity (6h)", archetype: TimelineW, kinds: &[Vessel, Aircraft] },
        DeckWidget { title: "Trade Headlines", archetype: TickerW, kinds: &[News] },
    ],
};

const ELECTIONS_AND_POLITICS: Deck = Deck {
    id: DeckId::ElectionsAndPolitics,
    name: "Elections & Politics",
    description: "Political risk, unrest, and information operations.",
    focus: &[News, Conflict, Cyber],
    widgets: &[
        DeckWidget { title: "Political Map", archetype: MapW, kinds: ALL_FOCUS },
        DeckWidget { title: "Instability Gauge", archetype: GaugeW, kinds: &[Conflict, Cyber] },
        DeckWidget { title: "Political Wire", archetype: TickerW, kinds: &[News] },
        DeckWidget { title: "Headlines", archetype: TableW, kinds: &[News] },
        DeckWidget { title: "Unrest & Protests", archetype: TableW, kinds: &[Conflict] },
        DeckWidget { title: "Influence Operations", archetype: TickerW, kinds: &[Cyber] },
        DeckWidget { title: "Activity (6h)", archetype: TimelineW, kinds: &[News, Conflict] },
        DeckWidget { title: "Hotspots", archetype: TickerW, kinds: &[Conflict] },
        DeckWidget { title: "Coverage Volume", archetype: GaugeW, kinds: &[News] },
    ],
};

const HUMANITARIAN: Deck = Deck {
    id: DeckId::Humanitarian,
    name: "Humanitarian",
    description: "Natural disasters, severe weather, and crisis impact.",
    focus: &[Earthquake, Wildfire, Weather, Conflict, News],
    widgets: &[
        DeckWidget { title: "Crisis Map", archetype: MapW, kinds: ALL_FOCUS },
        DeckWidget { title: "Crisis Severity", archetype: GaugeW, kinds: ALL_FOCUS },
        DeckWidget { title: "Disaster Wire", archetype: TickerW, kinds: &[Earthquake, Wildfire, Weather] },
        DeckWidget { title: "Active Disasters", archetype: TableW, kinds: &[Earthquake, Wildfire, Weather] },
        DeckWidget { title: "Seismic", archetype: TickerW, kinds: &[Earthquake] },
        DeckWidget { title: "Wildfires", archetype: TickerW, kinds: &[Wildfire] },
        DeckWidget { title: "Severe Weather", archetype: TableW, kinds: &[Weather] },
        DeckWidget { title: "Conflict Impact", archetype: TableW, kinds: &[Conflict] },
        DeckWidget { title: "Activity (6h)", archetype: TimelineW, kinds: ALL_FOCUS },
    ],
};

const CYBER_AND_TECHNOLOGY: Deck = Deck {
    id: DeckId::CyberAndTechnology,
    name: "Cyber & Technology",
    description: "Exploited vulnerabilities, tech markets, and sector news.",
    focus: &[Cyber, Market, News],
    widgets: &[
        DeckWidget { title: "Threat Map", archetype: MapW, kinds: ALL_FOCUS },
        DeckWidget { title: "Cyber Threat Level", archetype: GaugeW, kinds: &[Cyber] },
        DeckWidget { title: "Vulnerability Wire", archetype: TickerW, kinds: &[Cyber] },
        DeckWidget { title: "Active Threats", archetype: TableW, kinds: &[Cyber] },
        DeckWidget { title: "Exploitation Tempo (6h)", archetype: TimelineW, kinds: &[Cyber] },
        DeckWidget { title: "Tech Markets", archetype: GaugeW, kinds: &[Market] },
        DeckWidget { title: "Market Movers", archetype: TableW, kinds: &[Market] },
        DeckWidget { title: "Tech Headlines", archetype: TickerW, kinds: &[News] },
        DeckWidget { title: "Signal Volume", archetype: GaugeW, kinds: ALL_FOCUS },
    ],
};

/// Resolve a [`DeckId`] to its [`Deck`] descriptor.
pub fn deck(id: DeckId) -> Deck {
    match id {
        DeckId::CommandCenter => COMMAND_CENTER,
        DeckId::WarAndConflict => WAR_AND_CONFLICT,
        DeckId::MaritimeAndTrade => MARITIME_AND_TRADE,
        DeckId::ElectionsAndPolitics => ELECTIONS_AND_POLITICS,
        DeckId::Humanitarian => HUMANITARIAN,
        DeckId::CyberAndTechnology => CYBER_AND_TECHNOLOGY,
    }
}

/// All six pre-built decks, in canonical order.
pub fn registry() -> Vec<Deck> {
    DeckId::ALL.into_iter().map(deck).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use ee_core::{Geo, Severity};

    fn now() -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000, 0).single().unwrap()
    }

    fn ev(id: &str, kind: EventKind, sev: f64, secs_ago: i64, located: bool) -> Event {
        Event {
            id: id.into(),
            source_id: "test".into(),
            kind,
            title: format!("{id} {kind:?}"),
            time: now() - Duration::seconds(secs_ago),
            geo: if located { Geo::new(10.0, 20.0) } else { None },
            severity: Severity::new(sev),
            url: None,
            raw: serde_json::Value::Null,
        }
    }

    #[test]
    fn registry_has_six_decks_with_unique_ids() {
        let reg = registry();
        assert_eq!(reg.len(), 6);
        let mut ids: Vec<_> = reg.iter().map(|d| d.id.id()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 6);
        // by-id resolution round-trips.
        for id in DeckId::ALL {
            assert_eq!(deck(id).id, id);
        }
    }

    #[test]
    fn every_deck_is_well_formed() {
        for d in registry() {
            // Catalog invariant: nine widgets each, non-empty focus & metadata.
            assert_eq!(d.widgets.len(), 9, "{} widget count", d.id.id());
            assert!(!d.focus.is_empty());
            assert!(!d.name.is_empty() && !d.description.is_empty());
            // A focus has no duplicate domains.
            let mut f = d.focus.to_vec();
            f.sort_by_key(|k| *k as u8);
            f.dedup();
            assert_eq!(f.len(), d.focus.len(), "{} focus dup", d.id.id());
            // Every widget's explicit kinds are a subset of the deck focus.
            for w in d.widgets {
                for k in w.kinds {
                    assert!(d.focus.contains(k), "{}: {:?} not in focus", w.title, k);
                }
            }
            // Each deck spans at least one layer group.
            assert!(!d.groups().is_empty());
        }
    }

    #[test]
    fn layer_set_enables_only_focus() {
        let d = deck(DeckId::CyberAndTechnology);
        let set = d.layer_set();
        assert!(set.is_visible(Cyber) && set.is_visible(Market) && set.is_visible(News));
        // Earthquake is not in this deck's focus.
        assert!(!set.is_visible(Earthquake));
        // visible_layers matches the focus size.
        assert_eq!(set.visible_layers().len(), d.focus.len());
    }

    #[test]
    fn filter_restricts_to_focus() {
        let d = deck(DeckId::WarAndConflict);
        let f = d.filter();
        assert!(f.matches(&ev("c", Conflict, 0.5, 0, true)));
        assert!(!f.matches(&ev("q", Earthquake, 0.5, 0, true)));
    }

    #[test]
    fn groups_are_canonical_and_deduped() {
        // Command Center spans many domains; groups come back in canonical order.
        let g = deck(DeckId::CommandCenter).groups();
        let mut sorted = g.clone();
        sorted.sort_by_key(|x| LayerGroup::ALL.iter().position(|y| y == x).unwrap());
        assert_eq!(g, sorted);
        // No duplicates.
        let mut d = g.clone();
        d.dedup();
        assert_eq!(d.len(), g.len());
    }

    #[test]
    fn render_populates_all_nine_panels() {
        let n = now();
        let events = vec![
            ev("cve1", Cyber, 0.9, 60, false),
            ev("cve2", Cyber, 0.6, 1800, false),
            ev("mkt", Market, 0.4, 120, true),
            ev("news", News, 0.3, 30, false),
            ev("quake", Earthquake, 0.8, 90, true), // outside Cyber deck focus
        ];
        let d = deck(DeckId::CyberAndTechnology);
        let r = d.render(&events, n);

        assert_eq!(r.deck, DeckId::CyberAndTechnology);
        assert_eq!(r.total, 5);
        // matched = events in focus (cyber, market, news) -> 4 (quake excluded).
        assert_eq!(r.matched, 4);
        assert_eq!(r.panels.len(), 9);

        // Slot 0 is the map; cyber events are geo-less so located is 0 but total counts.
        match &r.panels[0].data {
            RenderedWidget::Map(report) => {
                // Two cyber + one market + one news fed the map (focus subset).
                assert_eq!(report.total, 4);
            }
            other => panic!("expected map, got {other:?}"),
        }

        // The "Cyber Threat Level" gauge sees only the two cyber events.
        let gauge_panel = r
            .panels
            .iter()
            .find(|p| p.title == "Cyber Threat Level")
            .unwrap();
        assert_eq!(gauge_panel.count, 2);
        match &gauge_panel.data {
            RenderedWidget::Gauge(g) => {
                assert!((g.peak - 0.9).abs() < 1e-9);
            }
            other => panic!("expected gauge, got {other:?}"),
        }

        // The vulnerability ticker is severity-ranked: the 0.9 CVE leads.
        let wire = r.panels.iter().find(|p| p.title == "Vulnerability Wire").unwrap();
        match &wire.data {
            RenderedWidget::Ticker(items) => {
                assert_eq!(items.len(), 2);
                assert_eq!(items[0].id, "cve1");
            }
            other => panic!("expected ticker, got {other:?}"),
        }
    }

    #[test]
    fn rendered_deck_serializes_to_json() {
        let n = now();
        let events = vec![ev("c", Cyber, 0.85, 10, false)];
        let r = deck(DeckId::CyberAndTechnology).render(&events, n);
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"deck\":\"cyber_and_technology\""));
        // External-tagged widget variants surface their archetype key.
        assert!(json.contains("\"gauge\""));
        assert!(json.contains("\"ticker\""));
    }
}
