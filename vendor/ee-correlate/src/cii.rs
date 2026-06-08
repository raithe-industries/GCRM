//! Country Intelligence Index — a composite per-country risk score across signal
//! categories.
//!
//! World Monitor's flagship analytic ranks countries by a single composite risk number
//! built from many *signal categories* at once (conflict, cyber, seismic, …). The
//! point is cross-domain: a country that is hot on several fronts at once is more at
//! risk than one with a single loud feed, and a country quiet across every category is
//! genuinely safer. That is what separates this from [`crate::rollup`], which reduces a
//! region to peak/mean/volume *regardless of kind* — the index here is **weighted by
//! category salience** and divides by the **whole** category taxonomy, so silence on a
//! high-salience front (no conflict, no cyber) is rewarded with a lower score.
//!
//! ## How the score is built
//! Events are bucketed into caller-defined countries (labelled [`Region`] boxes, exactly
//! like the rollup, so overlapping/ custom regions work). For each country:
//!
//! 1. Its events are split by [`EventKind`] into **categories**.
//! 2. Each active category gets an `intensity` in `[0, 1]` blending its **peak severity**
//!    and a **saturating volume** term (`1 - exp(-count/scale)`) — acute severity leads,
//!    sustained volume tops it up. The two within-category weights sum to 1, so
//!    `intensity ∈ [0, 1]`.
//! 3. The composite **score** is the salience-weighted mean of category intensities over
//!    the *entire* weight table:
//!    `score = Σ_c (wₖ · intensityₖ) / Σ_all wₖ ∈ [0, 1]`.
//!    Dividing by the full weight total (not just the active categories) is the
//!    cross-domain property: a country needs trouble across several salient categories to
//!    climb, and a single hot feed only moves it a little.
//!
//! Each country is binned into a [`RiskLevel`] (Low → Critical) for a colour-coded panel
//! and the report is ordered **worst-first**. Every configured country appears — even
//! quiet ones (score 0) — so a country panel stays stable across refreshes (same
//! philosophy as the freshness monitor and rollup: reported == configured). Geo-less
//! events and located events outside all countries are tallied separately. Everything is
//! pure: slices in, derived structs out, no I/O.

use ee_core::{Event, EventKind, Region};
use serde::Serialize;

/// Per-category salience weights — how much each [`EventKind`] counts toward a country's
/// composite risk. The default set is security-led (conflict highest, generic `Other`
/// lowest); callers can retune for a finance- or humanitarian-flavoured index.
#[derive(Debug, Clone)]
pub struct CategoryWeights {
    /// `(kind, weight)`; weights are expected to be `>= 0`. The full set is the
    /// denominator of the composite, so it defines the index's category taxonomy.
    entries: Vec<(EventKind, f64)>,
}

impl Default for CategoryWeights {
    /// Security-salience defaults: armed conflict dominates, cyber and acute natural
    /// hazards next, generic signals lowest. Sums to 5.6.
    fn default() -> Self {
        Self {
            entries: vec![
                (EventKind::Conflict, 1.0),
                (EventKind::Cyber, 0.8),
                (EventKind::Weather, 0.7),
                (EventKind::Earthquake, 0.7),
                (EventKind::Wildfire, 0.6),
                (EventKind::Market, 0.5),
                (EventKind::Vessel, 0.4),
                (EventKind::Aircraft, 0.4),
                (EventKind::News, 0.3),
                (EventKind::Other, 0.2),
            ],
        }
    }
}

impl CategoryWeights {
    /// Build from explicit `(kind, weight)` pairs. A later pair for the same kind wins.
    pub fn from_pairs(pairs: impl IntoIterator<Item = (EventKind, f64)>) -> Self {
        let mut entries: Vec<(EventKind, f64)> = Vec::new();
        for (k, w) in pairs {
            if let Some(slot) = entries.iter_mut().find(|(ek, _)| *ek == k) {
                slot.1 = w;
            } else {
                entries.push((k, w));
            }
        }
        Self { entries }
    }

    /// Set (or insert) a category's weight; returns `self` for chaining.
    pub fn with(mut self, kind: EventKind, weight: f64) -> Self {
        if let Some(slot) = self.entries.iter_mut().find(|(k, _)| *k == kind) {
            slot.1 = weight;
        } else {
            self.entries.push((kind, weight));
        }
        self
    }

    /// Weight for a kind (`0.0` if it is not part of the taxonomy).
    pub fn weight(&self, kind: EventKind) -> f64 {
        self.entries
            .iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, w)| *w)
            .unwrap_or(0.0)
    }

    /// Sum of all category weights — the composite's denominator.
    pub fn total(&self) -> f64 {
        self.entries.iter().map(|(_, w)| *w).sum()
    }
}

/// Tunables for [`cii`].
#[derive(Debug, Clone)]
pub struct CiiParams {
    /// Category salience weights (the index taxonomy + its denominator).
    pub weights: CategoryWeights,
    /// Within a category: weight on the peak (worst) severity. With `volume_weight`
    /// this should sum to 1 so each category intensity stays in `[0, 1]`.
    pub peak_weight: f64,
    /// Within a category: weight on the saturating event-volume term.
    pub volume_weight: f64,
    /// Event count at which a category's volume term reaches ~63% of its max. Smaller
    /// than the rollup's default because a handful of events in one category is already
    /// a strong signal.
    pub volume_scale: f64,
}

impl Default for CiiParams {
    fn default() -> Self {
        Self {
            weights: CategoryWeights::default(),
            peak_weight: 0.7,
            volume_weight: 0.3,
            volume_scale: 5.0,
        }
    }
}

/// Colour-coded risk band for a country, derived from its composite score.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Low,
    Guarded,
    Elevated,
    High,
    Critical,
}

impl RiskLevel {
    /// Bin a composite `[0, 1]` score. Thresholds are calibrated to the full-taxonomy
    /// denominator: reaching `High`/`Critical` takes trouble across several salient
    /// categories, not a single loud feed.
    pub fn from_score(score: f64) -> Self {
        match score {
            s if s >= 0.35 => RiskLevel::Critical,
            s if s >= 0.20 => RiskLevel::High,
            s if s >= 0.10 => RiskLevel::Elevated,
            s if s >= 0.03 => RiskLevel::Guarded,
            _ => RiskLevel::Low,
        }
    }

    /// Short UI label.
    pub fn label(&self) -> &'static str {
        match self {
            RiskLevel::Low => "Low",
            RiskLevel::Guarded => "Guarded",
            RiskLevel::Elevated => "Elevated",
            RiskLevel::High => "High",
            RiskLevel::Critical => "Critical",
        }
    }

    /// `#rrggbb` colour for a panel chip (green → red ramp).
    pub fn color(&self) -> &'static str {
        match self {
            RiskLevel::Low => "#2ecc71",
            RiskLevel::Guarded => "#a3cb38",
            RiskLevel::Elevated => "#f1c40f",
            RiskLevel::High => "#e67e22",
            RiskLevel::Critical => "#e74c3c",
        }
    }
}

/// One category's contribution to a country's composite score.
#[derive(Debug, Clone, Serialize)]
pub struct CategoryScore {
    pub kind: EventKind,
    /// Salience weight applied to this category.
    pub weight: f64,
    /// Number of events of this kind in the country.
    pub count: usize,
    /// Peak member severity in `[0, 1]`.
    pub peak: f64,
    /// Within-category intensity in `[0, 1]` (peak/volume blend).
    pub intensity: f64,
    /// This category's share of the composite score (`weight·intensity / Σ weights`).
    pub contribution: f64,
}

/// A country's composite intelligence index.
#[derive(Debug, Clone, Serialize)]
pub struct CountryIndex {
    /// Country name (from [`Region::name`]).
    pub country: String,
    /// Composite risk score in `[0, 1]`.
    pub score: f64,
    /// Risk band derived from the score.
    pub level: RiskLevel,
    /// Total located events placed in this country (all categories).
    pub total_events: usize,
    /// Active categories, highest-contribution-first.
    pub categories: Vec<CategoryScore>,
    /// The top-contributing category, if any.
    pub dominant: Option<EventKind>,
}

/// The full index over a set of countries, plus events that could not be placed.
#[derive(Debug, Clone, Serialize)]
pub struct CiiReport {
    /// One entry per configured country, worst-first (descending score; ties by
    /// descending event count, then country name for determinism).
    pub countries: Vec<CountryIndex>,
    /// Located events that fell inside no configured country.
    pub unassigned: usize,
    /// Events with no location (cannot be placed).
    pub geoless: usize,
}

impl CiiReport {
    /// The highest-scoring country, if any is configured.
    pub fn worst(&self) -> Option<&CountryIndex> {
        self.countries.first()
    }

    /// Count of configured countries at or above the given risk band.
    pub fn count_at_least(&self, level: RiskLevel) -> usize {
        self.countries.iter().filter(|c| c.level >= level).count()
    }
}

// Order Low < Guarded < … < Critical so `>=` comparisons read naturally.
impl PartialOrd for RiskLevel {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for RiskLevel {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.rank().cmp(&other.rank())
    }
}
impl RiskLevel {
    fn rank(&self) -> u8 {
        match self {
            RiskLevel::Low => 0,
            RiskLevel::Guarded => 1,
            RiskLevel::Elevated => 2,
            RiskLevel::High => 3,
            RiskLevel::Critical => 4,
        }
    }
}

/// Compute the Country Intelligence Index over located events, ranked worst-first.
///
/// Each located event is added to **every** country whose bbox contains it (overlapping
/// regions handled). Geo-less events and located events outside all countries are
/// tallied in [`CiiReport::geoless`] / [`CiiReport::unassigned`].
pub fn cii(countries: &[Region], events: &[Event], params: &CiiParams) -> CiiReport {
    let mut buckets: Vec<Vec<&Event>> = vec![Vec::new(); countries.len()];
    let mut unassigned = 0usize;
    let mut geoless = 0usize;

    for e in events {
        let Some(g) = e.geo else {
            geoless += 1;
            continue;
        };
        let mut matched = false;
        for (i, c) in countries.iter().enumerate() {
            if c.contains(&g) {
                buckets[i].push(e);
                matched = true;
            }
        }
        if !matched {
            unassigned += 1;
        }
    }

    let mut indexed: Vec<CountryIndex> = countries
        .iter()
        .zip(buckets)
        .map(|(c, members)| build_index(c.name.clone(), &members, params))
        .collect();

    indexed.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.total_events.cmp(&a.total_events))
            .then(a.country.cmp(&b.country))
    });

    CiiReport { countries: indexed, unassigned, geoless }
}

/// Reduce one country's member events to a [`CountryIndex`].
fn build_index(country: String, members: &[&Event], params: &CiiParams) -> CountryIndex {
    let total_events = members.len();
    let denom = params.weights.total();

    // Aggregate per category: count + peak severity.
    let mut cats: Vec<(EventKind, usize, f64)> = Vec::new();
    for e in members {
        let sev = e.severity.value();
        if let Some(slot) = cats.iter_mut().find(|(k, _, _)| *k == e.kind) {
            slot.1 += 1;
            slot.2 = slot.2.max(sev);
        } else {
            cats.push((e.kind, 1, sev));
        }
    }

    let mut categories: Vec<CategoryScore> = cats
        .into_iter()
        .filter_map(|(kind, count, peak)| {
            let weight = params.weights.weight(kind);
            if weight <= 0.0 {
                // No salience (or not in the taxonomy): present in the feed but it does
                // not move the composite, so it is not a scored category.
                return None;
            }
            let volume = if params.volume_scale > 0.0 {
                1.0 - (-(count as f64) / params.volume_scale).exp()
            } else {
                1.0
            };
            let intensity =
                (params.peak_weight * peak + params.volume_weight * volume).clamp(0.0, 1.0);
            let contribution = if denom > 0.0 { weight * intensity / denom } else { 0.0 };
            Some(CategoryScore { kind, weight, count, peak, intensity, contribution })
        })
        .collect();

    // Highest-contribution first; ties by weight then count for determinism.
    categories.sort_by(|a, b| {
        b.contribution
            .partial_cmp(&a.contribution)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.weight.partial_cmp(&a.weight).unwrap_or(std::cmp::Ordering::Equal))
            .then(b.count.cmp(&a.count))
    });

    // `+ 0.0` collapses an empty-country -0.0 to a clean +0.0.
    let score = categories.iter().map(|c| c.contribution).sum::<f64>().clamp(0.0, 1.0) + 0.0;
    let dominant = categories.first().map(|c| c.kind);

    CountryIndex {
        country,
        score,
        level: RiskLevel::from_score(score),
        total_events,
        categories,
        dominant,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use ee_core::{BBox, Geo, Severity};

    fn ev(kind: EventKind, lat: f64, lon: f64, sev: f64) -> Event {
        Event {
            id: "e".into(),
            source_id: "test".into(),
            kind,
            title: "e".into(),
            time: Utc.timestamp_opt(1_700_000_000, 0).single().unwrap(),
            geo: Geo::new(lat, lon),
            severity: Severity::new(sev),
            url: None,
            raw: serde_json::Value::Null,
        }
    }

    fn country(name: &str, min_lat: f64, min_lon: f64, max_lat: f64, max_lon: f64) -> Region {
        Region { name: name.into(), bbox: BBox { min_lat, min_lon, max_lat, max_lon } }
    }

    #[test]
    fn multi_category_country_outranks_single_loud_feed() {
        let countries = vec![
            country("Multi", 0.0, 0.0, 10.0, 10.0),
            country("Single", 20.0, 20.0, 30.0, 30.0),
        ];
        let events = vec![
            // Multi: trouble across several salient categories (moderate each).
            ev(EventKind::Conflict, 5.0, 5.0, 0.6),
            ev(EventKind::Cyber, 5.0, 5.0, 0.6),
            ev(EventKind::Earthquake, 5.0, 5.0, 0.6),
            ev(EventKind::Weather, 5.0, 5.0, 0.6),
            // Single: one very loud category only.
            ev(EventKind::Earthquake, 25.0, 25.0, 1.0),
            ev(EventKind::Earthquake, 25.0, 25.0, 1.0),
            ev(EventKind::Earthquake, 25.0, 25.0, 1.0),
        ];
        let report = cii(&countries, &events, &CiiParams::default());
        // The cross-domain country is the composite-risk leader.
        assert_eq!(report.worst().unwrap().country, "Multi");
        let multi = &report.countries[0];
        // Dominant contributor is the highest-salience active category (conflict).
        assert_eq!(multi.dominant, Some(EventKind::Conflict));
        assert!(multi.score > report.countries[1].score);
    }

    #[test]
    fn quiet_country_is_low_and_still_reported() {
        let countries = vec![
            country("Hot", 0.0, 0.0, 10.0, 10.0),
            country("Quiet", -80.0, -179.0, -70.0, -170.0),
        ];
        let events = vec![ev(EventKind::Conflict, 5.0, 5.0, 0.9)];
        let report = cii(&countries, &events, &CiiParams::default());
        let quiet = report.countries.iter().find(|c| c.country == "Quiet").unwrap();
        assert_eq!(quiet.total_events, 0);
        assert_eq!(quiet.score, 0.0);
        assert_eq!(quiet.level, RiskLevel::Low);
        assert!(quiet.dominant.is_none());
        assert!(quiet.categories.is_empty());
    }

    #[test]
    fn score_stays_in_unit_range_and_levels_bin() {
        // Saturate every category at peak 1.0 with high volume; score must not exceed 1.
        let c = country("Max", 0.0, 0.0, 10.0, 10.0);
        let mut events = Vec::new();
        for k in [
            EventKind::Conflict,
            EventKind::Cyber,
            EventKind::Weather,
            EventKind::Earthquake,
            EventKind::Wildfire,
            EventKind::Market,
            EventKind::Vessel,
            EventKind::Aircraft,
            EventKind::News,
            EventKind::Other,
        ] {
            for _ in 0..50 {
                events.push(ev(k, 5.0, 5.0, 1.0));
            }
        }
        let report = cii(&[c], &events, &CiiParams::default());
        let m = &report.countries[0];
        assert!(m.score <= 1.0 + 1e-9);
        assert!(m.score > 0.9, "all-category saturation should be near 1, got {}", m.score);
        assert_eq!(m.level, RiskLevel::Critical);
    }

    #[test]
    fn intensity_blends_peak_and_volume() {
        // One severe conflict vs many mild conflicts: peak leads, volume tops up.
        let acute = country("Acute", 0.0, 0.0, 10.0, 10.0);
        let chronic = country("Chronic", 20.0, 20.0, 30.0, 30.0);
        let mut events = vec![ev(EventKind::Conflict, 5.0, 5.0, 0.9)];
        for _ in 0..10 {
            events.push(ev(EventKind::Conflict, 25.0, 25.0, 0.3));
        }
        let report = cii(&[acute, chronic], &events, &CiiParams::default());
        let a = report.countries.iter().find(|c| c.country == "Acute").unwrap();
        let ch = report.countries.iter().find(|c| c.country == "Chronic").unwrap();
        // Peak-led: the single 0.9 event beats ten 0.3 events.
        assert!(a.score > ch.score);
        // But volume still moved the chronic country off its bare peak.
        let chronic_cat = &ch.categories[0];
        assert!(chronic_cat.intensity > 0.7 * 0.3);
    }

    #[test]
    fn geoless_and_unassigned_tallied_separately() {
        let countries = vec![country("Box", 0.0, 0.0, 10.0, 10.0)];
        let mut headline = ev(EventKind::News, 5.0, 5.0, 0.2);
        headline.geo = None;
        let events = vec![
            ev(EventKind::Wildfire, 5.0, 5.0, 0.4),
            ev(EventKind::Wildfire, 50.0, 50.0, 0.4),
            headline,
        ];
        let report = cii(&countries, &events, &CiiParams::default());
        assert_eq!(report.countries[0].total_events, 1);
        assert_eq!(report.unassigned, 1);
        assert_eq!(report.geoless, 1);
    }

    #[test]
    fn zero_weight_category_does_not_score() {
        // Drop Market to weight 0: its events count toward totals but not the score.
        let params = CiiParams {
            weights: CategoryWeights::default().with(EventKind::Market, 0.0),
            ..CiiParams::default()
        };
        let c = country("Box", 0.0, 0.0, 10.0, 10.0);
        let events = vec![
            ev(EventKind::Market, 5.0, 5.0, 0.9),
            ev(EventKind::Market, 5.0, 5.0, 0.9),
        ];
        let report = cii(&[c], &events, &params);
        let idx = &report.countries[0];
        assert_eq!(idx.total_events, 2);
        assert!(idx.categories.is_empty());
        assert_eq!(idx.score, 0.0);
    }

    #[test]
    fn risk_levels_order_and_count() {
        assert!(RiskLevel::Critical > RiskLevel::High);
        assert!(RiskLevel::High > RiskLevel::Elevated);
        assert!(RiskLevel::Low < RiskLevel::Guarded);
        assert_eq!(RiskLevel::from_score(0.5), RiskLevel::Critical);
        assert_eq!(RiskLevel::from_score(0.0), RiskLevel::Low);

        let c = country("Box", 0.0, 0.0, 10.0, 10.0);
        let events = vec![ev(EventKind::Conflict, 5.0, 5.0, 0.9)];
        let report = cii(&[c], &events, &CiiParams::default());
        assert_eq!(report.count_at_least(RiskLevel::Low), 1);
    }

    #[test]
    fn overlapping_countries_both_score_the_event() {
        let countries = vec![
            country("Wide", 0.0, 0.0, 20.0, 20.0),
            country("Narrow", 4.0, 4.0, 6.0, 6.0),
        ];
        let events = vec![ev(EventKind::Conflict, 5.0, 5.0, 0.5)];
        let report = cii(&countries, &events, &CiiParams::default());
        assert_eq!(report.unassigned, 0);
        for c in &report.countries {
            assert_eq!(c.total_events, 1);
        }
    }
}
