//! Source freshness monitoring — per-source data-recency tracking.
//!
//! A live dashboard is only as trustworthy as its feeds: a layer that silently
//! stopped updating is worse than no layer at all. World Monitor surfaces this with
//! a *Freshness Monitor* that tracks dozens of source groups for data recency; this
//! module is the reusable engine behind that capability.
//!
//! The idea is simple and pure: every source declares an expected
//! [`cadence`](ee_core::SourceMeta::cadence). As data arrives you
//! [`observe`](FreshnessMonitor::observe) it (each successful fetch, or each event's
//! timestamp), which bumps that source's last-seen time. At report time the monitor
//! compares how long each source has been quiet against its cadence and classifies
//! it into a [`Freshness`] tier — so the dashboard can grey out a stalled layer and
//! raise the silent ones to the top of a status panel.
//!
//! Everything here is deterministic: recency is measured against a caller-supplied
//! `now`, with no hidden wall-clock reads, so it is fully unit-testable offline.

use chrono::{DateTime, Utc};
use ee_core::{EventKind, Event, SourceMeta};
use serde::Serialize;
use std::collections::HashMap;
use std::time::Duration;

/// Health tier of a source, by how long it has been quiet relative to its cadence.
///
/// Ordering runs healthiest → worst: [`Fresh`](Freshness::Fresh) <
/// [`Lagging`](Freshness::Lagging) < [`Stale`](Freshness::Stale) <
/// [`Silent`](Freshness::Silent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Freshness {
    /// Updated within `lagging_factor` cadences — current.
    Fresh,
    /// Overdue but still recent (within `stale_factor` cadences) — watch it.
    Lagging,
    /// Has produced data before, but is now far past schedule — likely broken.
    Stale,
    /// Never produced any data since tracking began — nothing to go on.
    Silent,
}

impl Freshness {
    /// Severity rank, higher = worse. Used to sort problem sources to the top.
    pub fn rank(&self) -> u8 {
        match self {
            Freshness::Fresh => 0,
            Freshness::Lagging => 1,
            Freshness::Stale => 2,
            Freshness::Silent => 3,
        }
    }

    /// `true` for tiers that warrant operator attention ([`Stale`](Freshness::Stale)
    /// or [`Silent`](Freshness::Silent)).
    pub fn is_degraded(&self) -> bool {
        matches!(self, Freshness::Stale | Freshness::Silent)
    }

    /// Localized status label for a UI badge. `lang` is a two-letter code
    /// (`en`, `es`, `fr`, `de`, `ar`); unknown codes fall back to English. RTL
    /// rendering (e.g. `ar`) is a frontend concern — the text is returned as-is.
    pub fn label(&self, lang: &str) -> &'static str {
        match (lang, self) {
            ("es", Freshness::Fresh) => "Al día",
            ("es", Freshness::Lagging) => "Retrasado",
            ("es", Freshness::Stale) => "Obsoleto",
            ("es", Freshness::Silent) => "Sin datos",
            ("fr", Freshness::Fresh) => "À jour",
            ("fr", Freshness::Lagging) => "En retard",
            ("fr", Freshness::Stale) => "Périmé",
            ("fr", Freshness::Silent) => "Aucune donnée",
            ("de", Freshness::Fresh) => "Aktuell",
            ("de", Freshness::Lagging) => "Verzögert",
            ("de", Freshness::Stale) => "Veraltet",
            ("de", Freshness::Silent) => "Keine Daten",
            ("ar", Freshness::Fresh) => "محدّث",
            ("ar", Freshness::Lagging) => "متأخر",
            ("ar", Freshness::Stale) => "قديم",
            ("ar", Freshness::Silent) => "لا بيانات",
            (_, Freshness::Fresh) => "Fresh",
            (_, Freshness::Lagging) => "Lagging",
            (_, Freshness::Stale) => "Stale",
            (_, Freshness::Silent) => "Silent",
        }
    }
}

/// Tunables for freshness classification: a source quiet for longer than
/// `lagging_factor × cadence` is [`Lagging`](Freshness::Lagging), and longer than
/// `stale_factor × cadence` is [`Stale`](Freshness::Stale).
#[derive(Debug, Clone, Copy)]
pub struct FreshnessParams {
    pub lagging_factor: f64,
    pub stale_factor: f64,
}

impl Default for FreshnessParams {
    /// A forgiving general-purpose default: a source is fresh until it has missed
    /// ~2 expected updates, and only stale once it has missed ~6 — wide enough to
    /// tolerate the natural jitter of sporadic feeds without crying wolf.
    fn default() -> Self {
        Self { lagging_factor: 2.0, stale_factor: 6.0 }
    }
}

/// A tracked source: identity plus the cadence its freshness is judged against.
#[derive(Debug, Clone)]
pub struct SourceClock {
    pub id: String,
    pub name: String,
    pub domain: EventKind,
    pub cadence: Duration,
}

impl SourceClock {
    /// Build a clock straight from a [`SourceMeta`], so the registered sources and
    /// the monitored sources stay in lock-step.
    pub fn from_meta(meta: &SourceMeta) -> Self {
        Self {
            id: meta.id.to_string(),
            name: meta.name.to_string(),
            domain: meta.domain,
            cadence: meta.cadence,
        }
    }
}

/// A per-source freshness verdict at a point in time.
#[derive(Debug, Clone, Serialize)]
pub struct SourceReport {
    pub id: String,
    pub name: String,
    pub domain: EventKind,
    /// The cadence this source was judged against.
    pub cadence: Duration,
    /// When the source last produced data, if ever observed.
    pub last_seen: Option<DateTime<Utc>>,
    /// How long the source has been quiet (`now - last_seen`), if ever observed.
    pub age: Option<Duration>,
    pub status: Freshness,
}

/// Aggregate counts across all tracked sources — a one-glance health summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FreshnessSummary {
    pub fresh: usize,
    pub lagging: usize,
    pub stale: usize,
    pub silent: usize,
    pub total: usize,
}

impl FreshnessSummary {
    /// Count of sources needing attention (stale + silent).
    pub fn degraded(&self) -> usize {
        self.stale + self.silent
    }

    /// `true` when every tracked source is fresh (and at least one is tracked).
    pub fn all_fresh(&self) -> bool {
        self.total > 0 && self.fresh == self.total
    }
}

/// Tracks data recency for a set of sources and classifies each one's freshness.
///
/// Register the sources you care about (typically from each source's
/// [`SourceMeta`]), then call [`observe`](Self::observe) as data arrives. Recency is
/// monotonic — observing an older timestamp than one already recorded is ignored —
/// so feeding a refreshed or out-of-order batch never regresses a source's status.
#[derive(Debug, Clone, Default)]
pub struct FreshnessMonitor {
    params: FreshnessParams,
    /// Registration order is preserved; ids are unique (re-registering replaces).
    clocks: Vec<SourceClock>,
    last_seen: HashMap<String, DateTime<Utc>>,
}

impl FreshnessMonitor {
    /// A monitor with default [`FreshnessParams`].
    pub fn new() -> Self {
        Self::default()
    }

    /// A monitor with custom thresholds.
    pub fn with_params(params: FreshnessParams) -> Self {
        Self { params, ..Self::default() }
    }

    /// Register (or replace, by id) a source to track.
    pub fn register(&mut self, clock: SourceClock) -> &mut Self {
        if let Some(slot) = self.clocks.iter_mut().find(|c| c.id == clock.id) {
            *slot = clock;
        } else {
            self.clocks.push(clock);
        }
        self
    }

    /// Convenience: register a source straight from its [`SourceMeta`].
    pub fn track(&mut self, meta: &SourceMeta) -> &mut Self {
        self.register(SourceClock::from_meta(meta))
    }

    /// Record that `source_id` produced data at `at`. Keeps the most recent
    /// timestamp seen (older observations are ignored). Recording for a source that
    /// has not been registered is harmless — it is simply remembered until/if that
    /// source is registered.
    pub fn observe(&mut self, source_id: &str, at: DateTime<Utc>) -> &mut Self {
        self.last_seen
            .entry(source_id.to_string())
            .and_modify(|cur| {
                if at > *cur {
                    *cur = at;
                }
            })
            .or_insert(at);
        self
    }

    /// Record one event's timestamp against its `source_id`.
    pub fn observe_event(&mut self, event: &Event) -> &mut Self {
        self.observe(&event.source_id, event.time)
    }

    /// Record a batch of events, bumping each source to its newest event's time.
    pub fn observe_events(&mut self, events: &[Event]) -> &mut Self {
        for e in events {
            self.observe_event(e);
        }
        self
    }

    /// The last time `source_id` was observed, if ever.
    pub fn last_seen(&self, source_id: &str) -> Option<DateTime<Utc>> {
        self.last_seen.get(source_id).copied()
    }

    /// Freshness of a single registered source at `now`, or `None` if untracked.
    pub fn status_of(&self, source_id: &str, now: DateTime<Utc>) -> Option<Freshness> {
        let clock = self.clocks.iter().find(|c| c.id == source_id)?;
        Some(classify(self.age_of(source_id, now), clock.cadence, &self.params))
    }

    /// Freshness report for every registered source at `now`, worst-first.
    ///
    /// Sorted by status severity (silent → stale → lagging → fresh), then by
    /// descending age, then by id, so the most concerning feeds land at the top and
    /// the order is deterministic.
    pub fn report(&self, now: DateTime<Utc>) -> Vec<SourceReport> {
        let mut out: Vec<SourceReport> = self
            .clocks
            .iter()
            .map(|c| {
                let last_seen = self.last_seen.get(&c.id).copied();
                let age = self.age_of(&c.id, now);
                SourceReport {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    domain: c.domain,
                    cadence: c.cadence,
                    last_seen,
                    age,
                    status: classify(age, c.cadence, &self.params),
                }
            })
            .collect();

        out.sort_by(|a, b| {
            b.status
                .rank()
                .cmp(&a.status.rank())
                .then_with(|| {
                    let aa = a.age.unwrap_or(Duration::MAX);
                    let ba = b.age.unwrap_or(Duration::MAX);
                    ba.cmp(&aa)
                })
                .then_with(|| a.id.cmp(&b.id))
        });
        out
    }

    /// Tier counts across all registered sources at `now`.
    pub fn summary(&self, now: DateTime<Utc>) -> FreshnessSummary {
        let mut s = FreshnessSummary { fresh: 0, lagging: 0, stale: 0, silent: 0, total: 0 };
        for c in &self.clocks {
            s.total += 1;
            match classify(self.age_of(&c.id, now), c.cadence, &self.params) {
                Freshness::Fresh => s.fresh += 1,
                Freshness::Lagging => s.lagging += 1,
                Freshness::Stale => s.stale += 1,
                Freshness::Silent => s.silent += 1,
            }
        }
        s
    }

    /// Time since a source was last observed at `now`, clamped at zero for
    /// timestamps in the future. `None` if never observed.
    fn age_of(&self, source_id: &str, now: DateTime<Utc>) -> Option<Duration> {
        let last = self.last_seen.get(source_id)?;
        let ms = now.signed_duration_since(*last).num_milliseconds().max(0);
        Some(Duration::from_millis(ms as u64))
    }
}

/// Classify an age against a cadence. `None` age (never observed) → `Silent`.
fn classify(age: Option<Duration>, cadence: Duration, params: &FreshnessParams) -> Freshness {
    match age {
        None => Freshness::Silent,
        Some(age) => {
            let a = age.as_secs_f64();
            // Guard a zero/degenerate cadence so every factor stays meaningful.
            let c = cadence.as_secs_f64().max(1.0);
            if a <= c * params.lagging_factor {
                Freshness::Fresh
            } else if a <= c * params.stale_factor {
                Freshness::Lagging
            } else {
                Freshness::Stale
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000 + secs, 0).single().unwrap()
    }

    fn clock(id: &str, cadence_secs: u64) -> SourceClock {
        SourceClock {
            id: id.into(),
            name: id.into(),
            domain: EventKind::Other,
            cadence: Duration::from_secs(cadence_secs),
        }
    }

    #[test]
    fn classifies_by_age_against_cadence() {
        // 100 s cadence, defaults: fresh <=200s, lagging <=600s, else stale.
        let p = FreshnessParams::default();
        let c = Duration::from_secs(100);
        assert_eq!(classify(Some(Duration::from_secs(50)), c, &p), Freshness::Fresh);
        assert_eq!(classify(Some(Duration::from_secs(200)), c, &p), Freshness::Fresh);
        assert_eq!(classify(Some(Duration::from_secs(201)), c, &p), Freshness::Lagging);
        assert_eq!(classify(Some(Duration::from_secs(600)), c, &p), Freshness::Lagging);
        assert_eq!(classify(Some(Duration::from_secs(601)), c, &p), Freshness::Stale);
        assert_eq!(classify(None, c, &p), Freshness::Silent);
    }

    #[test]
    fn observe_keeps_most_recent() {
        let mut m = FreshnessMonitor::new();
        m.register(clock("s", 100));
        m.observe("s", t(500));
        m.observe("s", t(100)); // older — ignored
        assert_eq!(m.last_seen("s"), Some(t(500)));
    }

    #[test]
    fn report_is_sorted_worst_first() {
        let mut m = FreshnessMonitor::new();
        m.register(clock("fresh", 100))
            .register(clock("stale", 100))
            .register(clock("silent", 100))
            .register(clock("lagging", 100));
        // now = t(1000)
        m.observe("fresh", t(950)); // age 50  -> Fresh
        m.observe("lagging", t(600)); // age 400 -> Lagging
        m.observe("stale", t(100)); // age 900 -> Stale
        // "silent" never observed -> Silent

        let r = m.report(t(1000));
        let order: Vec<&str> = r.iter().map(|x| x.id.as_str()).collect();
        assert_eq!(order, vec!["silent", "stale", "lagging", "fresh"]);

        let silent = &r[0];
        assert_eq!(silent.status, Freshness::Silent);
        assert!(silent.last_seen.is_none() && silent.age.is_none());

        let stale = &r[1];
        assert_eq!(stale.status, Freshness::Stale);
        assert_eq!(stale.age, Some(Duration::from_secs(900)));
    }

    #[test]
    fn observe_events_uses_newest_per_source() {
        use ee_core::{Geo, Severity};
        let mk = |src: &str, secs: i64| Event {
            id: format!("{src}-{secs}"),
            source_id: src.into(),
            kind: EventKind::Earthquake,
            title: "x".into(),
            time: t(secs),
            geo: Geo::new(0.0, 0.0),
            severity: Severity::new(0.1),
            url: None,
            raw: serde_json::Value::Null,
        };
        let mut m = FreshnessMonitor::new();
        m.register(clock("usgs", 300));
        m.observe_events(&[mk("usgs", 100), mk("usgs", 900), mk("usgs", 500)]);
        assert_eq!(m.last_seen("usgs"), Some(t(900)));
        // unregistered source observed but not reported
        m.observe_events(&[mk("ghost", 999)]);
        assert_eq!(m.report(t(1000)).len(), 1);
    }

    #[test]
    fn summary_counts_each_tier() {
        let mut m = FreshnessMonitor::new();
        m.register(clock("a", 100)).register(clock("b", 100)).register(clock("c", 100));
        m.observe("a", t(990)); // Fresh
        m.observe("b", t(100)); // Stale
        // c silent
        let s = m.summary(t(1000));
        assert_eq!(s, FreshnessSummary { fresh: 1, lagging: 0, stale: 1, silent: 1, total: 3 });
        assert_eq!(s.degraded(), 2);
        assert!(!s.all_fresh());
    }

    #[test]
    fn future_timestamp_clamps_to_fresh() {
        let mut m = FreshnessMonitor::new();
        m.register(clock("s", 100));
        m.observe("s", t(2000)); // ahead of `now`
        assert_eq!(m.status_of("s", t(1000)), Some(Freshness::Fresh));
        assert_eq!(m.report(t(1000))[0].age, Some(Duration::from_secs(0)));
    }

    #[test]
    fn track_from_meta_and_status_of_unknown() {
        let meta = SourceMeta {
            id: "usgs",
            name: "USGS Earthquakes",
            domain: EventKind::Earthquake,
            cadence: Duration::from_secs(300),
            needs_key: false,
        };
        let mut m = FreshnessMonitor::new();
        m.track(&meta);
        assert_eq!(m.status_of("usgs", t(0)), Some(Freshness::Silent));
        assert_eq!(m.status_of("nope", t(0)), None);
    }

    #[test]
    fn localized_labels_fall_back_to_english() {
        assert_eq!(Freshness::Stale.label("fr"), "Périmé");
        assert_eq!(Freshness::Silent.label("es"), "Sin datos");
        assert_eq!(Freshness::Fresh.label("ar"), "محدّث");
        assert_eq!(Freshness::Lagging.label("xx"), "Lagging"); // unknown -> English
    }
}
