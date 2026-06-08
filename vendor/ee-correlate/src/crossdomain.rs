//! Cross-domain correlation — the *same situation* surfacing across *different* data
//! domains.
//!
//! World Monitor's headline differentiator is relating the feeds, not just plotting them:
//! a quake strikes, the wires report it, and the markets move — three events, three
//! different domains, **one real-world situation**. This module reconstructs that link.
//!
//! ## Relationship to [`crate::convergence`]
//! Convergence answers "are several streams hot in the same *place*?" — it is purely
//! **spatial** (it reuses [`crate::cluster`], so it can only relate events that carry a
//! location). That is exactly the wrong tool for the flagship example: a tsunami-warning
//! headline and a Nikkei sell-off have **no coordinates at all**, yet they are obviously
//! about the same earthquake. Cross-domain correlation links events the way a reader does
//! — by **shared subject matter and timing**, not geography:
//!
//! - two events link when they belong to **different** [`SignalDomain`]s, fall within a
//!   `window` of each other, and their titles **share enough significant keywords**
//!   (place names, actors, terms — stopwords and short filler removed);
//! - links are stitched into connected components (single-linkage, mirroring `cluster`),
//!   and a component that spans at least `min_domains` distinct domains becomes a
//!   [`CrossCorrelation`] — a "this situation is being seen across N domains" finding.
//!
//! Because the link is lexical rather than spatial, geo-less events (most news and market
//! feeds) are first-class citizens here — the whole point.
//!
//! Each correlation reports its member events **in chronological order** (so the cascade
//! reads quake → headline → market), the distinct domains it spans, the **theme** (the
//! keywords common to its members), and a composite [`CrossCorrelation::score`] blending
//! breadth (how many domains), coherence (how tightly the members share one theme), and
//! intensity (peak member severity). Everything is pure: a slice of events in, derived
//! structs out, no I/O.

use crate::convergence::{DomainSignal, SignalDomain};
use chrono::{DateTime, Duration, Utc};
use ee_core::Event;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// Function words and news filler that carry no linking signal — excluded from the
/// keyword set so two titles only link on *substantive* shared terms.
const STOPWORDS: &[&str] = &[
    "the", "and", "for", "are", "but", "not", "you", "all", "any", "can", "had", "her", "was",
    "one", "our", "out", "day", "get", "has", "him", "his", "how", "man", "new", "now", "old",
    "see", "two", "way", "who", "did", "its", "let", "put", "say", "she", "too", "use", "that",
    "with", "this", "from", "they", "will", "would", "there", "their", "what", "which", "when",
    "were", "been", "have", "into", "your", "near", "over", "off", "after", "amid", "says",
    "said", "report", "reports", "latest", "update", "breaking", "as", "at", "by", "in", "of",
    "on", "to", "up", "a", "an", "is", "it", "be", "or", "no",
];

/// The smallest token length kept; shorter strings are almost always filler ("m6", "us").
const MIN_TOKEN_LEN: usize = 3;

/// Tunables for [`correlate`].
#[derive(Debug, Clone)]
pub struct CrossParams {
    /// Maximum time gap between two events for them to be considered the same situation.
    pub window: Duration,
    /// Minimum number of shared significant keywords for two cross-domain events to link.
    /// Clamped to a floor of 1.
    pub min_shared: usize,
    /// Minimum number of **distinct** [`SignalDomain`]s a component must span to be a
    /// cross-domain correlation. Clamped to a floor of 2 (the definition of "cross").
    pub min_domains: usize,
    /// Weight on breadth (distinct domains / taxonomy size).
    pub breadth_weight: f64,
    /// Weight on coherence (fraction of members sharing the dominant theme keyword).
    pub coherence_weight: f64,
    /// Weight on intensity (peak member severity).
    pub intensity_weight: f64,
}

impl Default for CrossParams {
    /// A 24 h window catches the lag from a physical event to its market/news echo;
    /// a single shared keyword is enough to link (the stopword filter keeps that honest);
    /// two distinct domains is the definitional floor; breadth/coherence/intensity weigh
    /// 0.4 / 0.3 / 0.3 so the score stays in `[0, 1]`.
    fn default() -> Self {
        Self {
            window: Duration::hours(24),
            min_shared: 1,
            min_domains: 2,
            breadth_weight: 0.4,
            coherence_weight: 0.3,
            intensity_weight: 0.3,
        }
    }
}

/// A detected cross-domain correlation: events from multiple domains telling one story.
#[derive(Debug, Clone, Serialize)]
pub struct CrossCorrelation {
    /// Member events in chronological order — the cascade, lead first.
    pub events: Vec<Event>,
    /// The distinct domains present, strongest-first (peak severity, then volume).
    pub domains: Vec<DomainSignal>,
    /// The keywords common to two or more members, most-shared-first — what links them.
    pub theme: Vec<String>,
    /// Earliest member time.
    pub start: DateTime<Utc>,
    /// Latest member time.
    pub end: DateTime<Utc>,
    /// Peak member severity in `[0, 1]`.
    pub peak: f64,
    /// Fraction of members sharing the single most common theme keyword, in `(0, 1]`.
    pub coherence: f64,
    /// Composite score in `[0, 1]` = breadth ⊕ coherence ⊕ intensity.
    pub score: f64,
}

impl CrossCorrelation {
    /// Number of distinct domains spanned.
    pub fn domain_count(&self) -> usize {
        self.domains.len()
    }

    /// Total member events.
    pub fn size(&self) -> usize {
        self.events.len()
    }

    /// Time spanned from the lead event to the latest echo.
    pub fn span(&self) -> Duration {
        self.end - self.start
    }

    /// The lead event — the earliest in the cascade (the likely trigger).
    pub fn trigger(&self) -> &Event {
        // `events` is always non-empty for a correlation and chronologically sorted.
        &self.events[0]
    }

    /// The domain of the lead event.
    pub fn trigger_domain(&self) -> SignalDomain {
        SignalDomain::of(self.trigger().kind)
    }

    /// The strongest contributing domain (by peak severity).
    pub fn dominant(&self) -> SignalDomain {
        self.domains.first().map(|d| d.domain).unwrap_or(SignalDomain::Other)
    }
}

/// The full cross-domain report over an event stream.
#[derive(Debug, Clone, Serialize)]
pub struct CrossReport {
    /// Detected correlations, strongest-first (score desc; ties by domain count desc,
    /// then size desc, then earliest start for determinism).
    pub correlations: Vec<CrossCorrelation>,
    /// Linked components that did **not** span `min_domains` distinct domains.
    pub below_threshold: usize,
    /// Events that formed no cross-domain link at all (isolated singletons).
    pub isolated: usize,
}

impl CrossReport {
    /// The highest-scoring correlation, if any.
    pub fn top(&self) -> Option<&CrossCorrelation> {
        self.correlations.first()
    }

    /// Number of detected correlations.
    pub fn count(&self) -> usize {
        self.correlations.len()
    }
}

/// Correlate an event stream across domains.
///
/// Two events link when they belong to different [`SignalDomain`]s, occur within
/// `window` of each other, and share at least `min_shared` significant title keywords.
/// Links are stitched into single-linkage components; each component spanning at least
/// `min_domains` distinct domains becomes a [`CrossCorrelation`], scored and ranked
/// strongest-first. Geo-less events participate fully (the link is lexical, not spatial).
pub fn correlate(events: &[Event], params: &CrossParams) -> CrossReport {
    let min_shared = params.min_shared.max(1);
    let min_domains = params.min_domains.max(2);

    // Tokenize once per event.
    let toks: Vec<BTreeSet<String>> = events.iter().map(|e| tokens(&e.title)).collect();

    // Build single-linkage components over cross-domain, near-in-time, shared-keyword links.
    let mut uf = UnionFind::new(events.len());
    for i in 0..events.len() {
        for j in (i + 1)..events.len() {
            if SignalDomain::of(events[i].kind) == SignalDomain::of(events[j].kind) {
                continue; // same domain — not "cross"
            }
            if (events[i].time - events[j].time).abs() > params.window {
                continue; // too far apart in time
            }
            if shared(&toks[i], &toks[j]) >= min_shared {
                uf.union(i, j);
            }
        }
    }

    // Gather members per component root.
    let mut comps: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..events.len() {
        comps.entry(uf.find(i)).or_default().push(i);
    }

    let mut correlations = Vec::new();
    let mut below_threshold = 0usize;
    let mut isolated = 0usize;
    for members in comps.values() {
        if members.len() < 2 {
            isolated += 1;
            continue;
        }
        let mut evs: Vec<Event> = members.iter().map(|&i| events[i].clone()).collect();
        // Chronological order, id as a stable tie-break.
        evs.sort_by(|a, b| a.time.cmp(&b.time).then(a.id.cmp(&b.id)));

        let domains = domain_breakdown(&evs);
        if domains.len() < min_domains {
            below_threshold += 1;
            continue;
        }

        let (theme, coherence) = theme_and_coherence(&evs);
        let peak = evs.iter().map(|e| e.severity.value()).fold(0.0_f64, f64::max);
        let start = evs.first().unwrap().time;
        let end = evs.last().unwrap().time;
        let score = score(domains.len(), coherence, peak, params);

        correlations.push(CrossCorrelation {
            events: evs,
            domains,
            theme,
            start,
            end,
            peak,
            coherence,
            score,
        });
    }

    correlations.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.domain_count().cmp(&a.domain_count()))
            .then(b.size().cmp(&a.size()))
            .then(a.start.cmp(&b.start))
    });

    CrossReport { correlations, below_threshold, isolated }
}

/// Split a title into the set of significant lowercase keywords (length ≥ [`MIN_TOKEN_LEN`],
/// stopwords removed). Deduplicated by construction (a `BTreeSet`).
fn tokens(title: &str) -> BTreeSet<String> {
    title
        .split(|c: char| !c.is_alphanumeric())
        .filter_map(|w| {
            if w.len() < MIN_TOKEN_LEN {
                return None;
            }
            let w = w.to_lowercase();
            if STOPWORDS.contains(&w.as_str()) {
                None
            } else {
                Some(w)
            }
        })
        .collect()
}

/// Number of shared keywords between two token sets.
fn shared(a: &BTreeSet<String>, b: &BTreeSet<String>) -> usize {
    a.intersection(b).count()
}

/// Reduce a component's members to its per-domain breakdown, strongest-first.
fn domain_breakdown(events: &[Event]) -> Vec<DomainSignal> {
    let mut out: Vec<DomainSignal> = Vec::new();
    for e in events {
        let domain = SignalDomain::of(e.kind);
        let sev = e.severity.value();
        if let Some(slot) = out.iter_mut().find(|s| s.domain == domain) {
            slot.count += 1;
            slot.peak = slot.peak.max(sev);
        } else {
            out.push(DomainSignal { domain, count: 1, peak: sev });
        }
    }
    out.sort_by(|a, b| {
        b.peak
            .partial_cmp(&a.peak)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.count.cmp(&a.count))
            .then(a.domain.label().cmp(b.domain.label()))
    });
    out
}

/// The theme (keywords shared by ≥2 members, most-shared-first, top 5) and coherence
/// (fraction of members carrying the single most common keyword).
fn theme_and_coherence(events: &[Event]) -> (Vec<String>, f64) {
    let mut freq: BTreeMap<String, usize> = BTreeMap::new();
    for e in events {
        for t in tokens(&e.title) {
            *freq.entry(t).or_default() += 1;
        }
    }
    let top = freq.values().copied().max().unwrap_or(0);
    let coherence = if events.is_empty() { 0.0 } else { top as f64 / events.len() as f64 };

    let mut common: Vec<(String, usize)> =
        freq.into_iter().filter(|(_, c)| *c >= 2).collect();
    common.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let theme = common.into_iter().take(5).map(|(t, _)| t).collect();
    (theme, coherence)
}

/// Composite cross-domain score in `[0, 1]`.
fn score(distinct_domains: usize, coherence: f64, peak: f64, params: &CrossParams) -> f64 {
    let breadth = (distinct_domains as f64 / SignalDomain::ALL.len() as f64).clamp(0.0, 1.0);
    (params.breadth_weight * breadth
        + params.coherence_weight * coherence.clamp(0.0, 1.0)
        + params.intensity_weight * peak.clamp(0.0, 1.0))
    .clamp(0.0, 1.0)
}

/// Minimal union-find with path halving and union by size — for single-linkage components.
struct UnionFind {
    parent: Vec<usize>,
    size: Vec<usize>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        Self { parent: (0..n).collect(), size: vec![1; n] }
    }

    fn find(&mut self, mut x: usize) -> usize {
        while self.parent[x] != x {
            self.parent[x] = self.parent[self.parent[x]];
            x = self.parent[x];
        }
        x
    }

    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra == rb {
            return;
        }
        let (big, small) = if self.size[ra] >= self.size[rb] { (ra, rb) } else { (rb, ra) };
        self.parent[small] = big;
        self.size[big] += self.size[small];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use ee_core::{EventKind, Geo, Severity};

    fn ev(kind: EventKind, title: &str, mins: i64, sev: f64, geo: Option<Geo>) -> Event {
        Event {
            id: format!("{kind:?}-{mins}"),
            source_id: "test".into(),
            kind,
            title: title.into(),
            time: Utc.timestamp_opt(1_700_000_000 + mins * 60, 0).single().unwrap(),
            geo,
            severity: Severity::new(sev),
            url: None,
            raw: serde_json::Value::Null,
        }
    }

    #[test]
    fn tokenizes_dropping_stopwords_and_short_words() {
        let t = tokens("M7.1 earthquake strikes off Tokyo Bay");
        assert!(t.contains("earthquake"));
        assert!(t.contains("tokyo"));
        assert!(t.contains("strikes"));
        assert!(t.contains("bay"));
        assert!(!t.contains("off")); // stopword
        assert!(!t.contains("m7")); // too short
    }

    #[test]
    fn quake_news_market_cascade_correlates() {
        // The flagship example: a quake, its headline echo, and the market move — three
        // domains, geo-less news/market included, linked by "tokyo".
        let events = vec![
            ev(EventKind::Earthquake, "M7.1 earthquake strikes off Tokyo", 0, 0.78, Geo::new(35.6, 139.7)),
            ev(EventKind::News, "Tokyo earthquake triggers tsunami warning", 20, 0.60, None),
            ev(EventKind::Market, "Nikkei tumbles as Tokyo quake disrupts factories", 120, 0.55, None),
        ];
        let report = correlate(&events, &CrossParams::default());
        assert_eq!(report.count(), 1);
        let c = report.top().unwrap();
        assert_eq!(c.domain_count(), 3);
        assert_eq!(c.size(), 3);
        // Cascade order: the quake leads.
        assert_eq!(c.trigger().kind, EventKind::Earthquake);
        assert_eq!(c.trigger_domain(), SignalDomain::Disaster);
        // "tokyo" is the linking theme, shared by all three.
        assert_eq!(c.theme.first().map(String::as_str), Some("tokyo"));
        assert!((c.coherence - 1.0).abs() < 1e-9);
    }

    #[test]
    fn same_domain_events_do_not_link() {
        // Two quakes sharing keywords are the *same* domain -> never a cross-correlation.
        let events = vec![
            ev(EventKind::Earthquake, "Tokyo earthquake aftershock", 0, 0.7, Geo::new(35.6, 139.7)),
            ev(EventKind::Earthquake, "Tokyo earthquake felt widely", 30, 0.6, Geo::new(35.7, 139.8)),
        ];
        let report = correlate(&events, &CrossParams::default());
        assert_eq!(report.count(), 0);
        assert_eq!(report.isolated, 2);
    }

    #[test]
    fn unrelated_titles_do_not_link_even_cross_domain() {
        // Different domains, near in time, but no shared keyword.
        let events = vec![
            ev(EventKind::Earthquake, "Quake near Petrolia California", 0, 0.6, Geo::new(40.3, -124.4)),
            ev(EventKind::Market, "Copper futures rally in London", 30, 0.5, None),
        ];
        let report = correlate(&events, &CrossParams::default());
        assert_eq!(report.count(), 0);
        assert_eq!(report.isolated, 2);
    }

    #[test]
    fn time_window_gates_the_link() {
        let events = vec![
            ev(EventKind::Earthquake, "Tokyo earthquake strikes", 0, 0.7, Geo::new(35.6, 139.7)),
            ev(EventKind::Market, "Tokyo stocks slide sharply", 60, 0.5, None),
        ];
        // Within a 2h window they correlate...
        let lax = correlate(&events, &CrossParams { window: Duration::hours(2), ..Default::default() });
        assert_eq!(lax.count(), 1);
        // ...but a 30-min window separates them.
        let strict =
            correlate(&events, &CrossParams { window: Duration::minutes(30), ..Default::default() });
        assert_eq!(strict.count(), 0);
    }

    #[test]
    fn min_shared_tightens_linking() {
        // Share exactly one keyword ("tokyo").
        let events = vec![
            ev(EventKind::Earthquake, "Tokyo earthquake strikes", 0, 0.7, Geo::new(35.6, 139.7)),
            ev(EventKind::Market, "Tokyo bourse falls", 30, 0.5, None),
        ];
        let lax = correlate(&events, &CrossParams { min_shared: 1, ..Default::default() });
        assert_eq!(lax.count(), 1);
        let strict = correlate(&events, &CrossParams { min_shared: 2, ..Default::default() });
        assert_eq!(strict.count(), 0);
    }

    #[test]
    fn min_domains_threshold_gates() {
        // A 2-domain correlation: kept at the floor, gated when demanding 3 domains.
        let events = vec![
            ev(EventKind::Cyber, "Ransomware hits major bank", 0, 0.7, None),
            ev(EventKind::Market, "Bank shares slide after ransomware breach", 30, 0.55, None),
        ];
        let lax = correlate(&events, &CrossParams::default());
        assert_eq!(lax.count(), 1);
        assert_eq!(lax.top().unwrap().domain_count(), 2);

        let strict = correlate(&events, &CrossParams { min_domains: 3, ..Default::default() });
        assert_eq!(strict.count(), 0);
        assert_eq!(strict.below_threshold, 1);
    }

    #[test]
    fn geoless_events_are_first_class() {
        // All three geo-less — convergence would drop every one; cross-domain keeps them.
        let events = vec![
            ev(EventKind::Conflict, "Strikes reported in Hodeidah port", 0, 0.8, None),
            ev(EventKind::Market, "Oil jumps after Hodeidah port strikes", 45, 0.6, None),
            ev(EventKind::News, "Shipping reroutes around Hodeidah", 90, 0.5, None),
        ];
        let report = correlate(&events, &CrossParams::default());
        assert_eq!(report.count(), 1);
        let c = report.top().unwrap();
        assert_eq!(c.size(), 3);
        assert!(c.theme.iter().any(|t| t == "hodeidah"));
    }

    #[test]
    fn breadth_outranks_at_equal_coherence_and_intensity() {
        // Theme A spans 3 domains; theme B spans 2 — both fully coherent, equal peak.
        let events = vec![
            ev(EventKind::Earthquake, "Lima earthquake strikes", 0, 0.6, Geo::new(-12.0, -77.0)),
            ev(EventKind::News, "Lima earthquake aftermath", 10, 0.6, None),
            ev(EventKind::Market, "Lima earthquake rattles markets", 20, 0.6, None),
            // Separate, unrelated 2-domain story (no shared keyword with the Lima set).
            ev(EventKind::Cyber, "Phishing campaign targets utilities", 0, 0.6, None),
            ev(EventKind::Market, "Utilities dip on phishing campaign", 15, 0.6, None),
        ];
        let report = correlate(&events, &CrossParams::default());
        assert_eq!(report.count(), 2);
        let top = report.top().unwrap();
        assert_eq!(top.domain_count(), 3);
        assert!(top.score > report.correlations[1].score);
    }

    #[test]
    fn coherence_drops_for_chained_off_theme_links() {
        // Single-linkage chain: A-B share "delta", B-C share "ridge", A & C share nothing.
        // The component is held together by B, so no single keyword covers all three ->
        // coherence < 1.
        let events = vec![
            ev(EventKind::Earthquake, "Delta region earthquake", 0, 0.6, Geo::new(30.0, 31.0)),
            ev(EventKind::Market, "Delta airlines ridge route cut", 30, 0.5, None),
            ev(EventKind::News, "Ridge wildfire advisory", 60, 0.5, None),
        ];
        let report = correlate(&events, &CrossParams::default());
        assert_eq!(report.count(), 1);
        let c = report.top().unwrap();
        assert_eq!(c.size(), 3);
        assert!(c.coherence < 1.0, "chained links should lower coherence, got {}", c.coherence);
    }

    #[test]
    fn empty_input_yields_empty_report() {
        let report = correlate(&[], &CrossParams::default());
        assert_eq!(report.count(), 0);
        assert_eq!(report.isolated, 0);
        assert_eq!(report.below_threshold, 0);
    }
}
