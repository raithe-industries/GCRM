//! Finance Radar — a multi-signal market-stress composite across market segments.
//!
//! World Monitor's finance variant carries a "Finance Radar": a single market-stress
//! reading built from **seven market segments at once** (equities, crypto, commodities,
//! energy, bonds/rates, forex, macro) rather than a single ticker. The point is the same
//! cross-domain idea as the [`crate::cii`] country index, but the axes are *market
//! segments* instead of *countries*: a market wobbling on one front (just crypto) is less
//! systemically stressed than one lit up across several fronts at once (equities + bonds +
//! forex + macro all moving), and a market quiet on every segment is genuinely calm.
//!
//! ## Relationship to the other primitives
//! - [`crate::cii`] is **geographic** — it buckets *all* event kinds into *countries*.
//! - Finance Radar is **segment-wise and market-only** — it takes the [`EventKind::Market`]
//!   stream and splits it into the seven [`MarketSegment`] spokes of a radar/spider chart,
//!   so the output is a fixed set of axes a dashboard can plot directly.
//!
//! ## How the composite is built
//! Each [`EventKind::Market`] event is classified into a segment by a tunable keyword
//! [`SegmentLexicon`] over its title (non-market events are tallied and skipped). For each
//! of the seven segments:
//!
//! 1. Its events give a **count** and a **peak severity** (here severity reads as *market
//!    stress* — the size of an abnormal move).
//! 2. Each segment gets an `intensity` in `[0, 1]` blending **peak severity** with a
//!    **saturating volume** term (`1 - exp(-count/scale)`) — an acute shock leads,
//!    sustained churn tops it up. The two within-segment weights sum to 1, so
//!    `intensity ∈ [0, 1]`.
//! 3. The composite **stress score** is the salience-weighted mean of segment intensities
//!    over the *whole* seven-segment taxonomy:
//!    `score = Σ_s (wₛ · intensityₛ) / Σ_all wₛ ∈ [0, 1]`.
//!    Dividing by the full weight total (not just the active segments) is the systemic
//!    property: broad stress across segments climbs, a lone hot segment only nudges it.
//!
//! Every one of the seven segments appears in the report — even calm ones (intensity 0) —
//! so the radar has a stable set of spokes across refreshes (same philosophy as the
//! country index reporting every configured country). The composite is binned into a
//! colour-coded [`StressLevel`] (Calm → Panic). Everything is pure: a slice of events in,
//! derived structs out, no I/O.

use ee_core::{Event, EventKind};
use serde::Serialize;

/// One spoke of the radar — a market segment the composite is measured on. The seven
/// variants are the fixed axes of the chart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MarketSegment {
    /// Stock indices and single names (S&P, Nasdaq, FTSE, Nikkei, …).
    Equities,
    /// Digital assets (BTC, ETH, stablecoins, tokens).
    Crypto,
    /// Hard/soft commodities (metals, grains) — energy is its own spoke.
    Commodities,
    /// Energy complex (crude, gas, OPEC).
    Energy,
    /// Sovereign debt & rates (yields, treasuries, bunds, gilts).
    Bonds,
    /// Currencies / FX (dollar, euro, yen, pegs).
    Forex,
    /// Macro indicators & central banks (CPI, GDP, Fed/ECB, recession).
    Macro,
}

impl MarketSegment {
    /// The seven spokes in canonical (chart-axis) order.
    pub const ALL: [MarketSegment; 7] = [
        MarketSegment::Equities,
        MarketSegment::Crypto,
        MarketSegment::Commodities,
        MarketSegment::Energy,
        MarketSegment::Bonds,
        MarketSegment::Forex,
        MarketSegment::Macro,
    ];

    /// Short UI label.
    pub fn label(&self) -> &'static str {
        match self {
            MarketSegment::Equities => "Equities",
            MarketSegment::Crypto => "Crypto",
            MarketSegment::Commodities => "Commodities",
            MarketSegment::Energy => "Energy",
            MarketSegment::Bonds => "Bonds & Rates",
            MarketSegment::Forex => "Forex",
            MarketSegment::Macro => "Macro",
        }
    }

    /// Canonical-order rank, used as a deterministic tie-breaker.
    fn rank(&self) -> u8 {
        match self {
            MarketSegment::Equities => 0,
            MarketSegment::Crypto => 1,
            MarketSegment::Commodities => 2,
            MarketSegment::Energy => 3,
            MarketSegment::Bonds => 4,
            MarketSegment::Forex => 5,
            MarketSegment::Macro => 6,
        }
    }
}

/// Keyword classifier mapping a market event's title to a [`MarketSegment`]. The default
/// set is a compact, distinctive English lexicon; callers can replace it for another
/// language or a narrower finance variant. Segments are tried in canonical order, so the
/// first segment with a matching keyword wins (deterministic).
#[derive(Debug, Clone)]
pub struct SegmentLexicon {
    /// `(segment, keywords)`; keywords are lowercase and matched whole-word/phrase against
    /// the lowercased title (so `eth` will not fire inside `bethlehem`, and `s&p` matches).
    entries: Vec<(MarketSegment, Vec<&'static str>)>,
}

impl Default for SegmentLexicon {
    fn default() -> Self {
        Self {
            entries: vec![
                (
                    MarketSegment::Equities,
                    vec![
                        "s&p", "sp500", "nasdaq", "dow", "ftse", "nikkei", "dax", "cac",
                        "hang seng", "stoxx", "stock", "stocks", "equity", "equities",
                        "shares", "index", "etf", "ipo",
                    ],
                ),
                (
                    MarketSegment::Crypto,
                    vec![
                        "bitcoin", "btc", "ethereum", "eth", "crypto", "coin", "token",
                        "stablecoin", "defi", "altcoin", "solana", "xrp",
                    ],
                ),
                (
                    MarketSegment::Commodities,
                    vec![
                        "gold", "silver", "copper", "platinum", "wheat", "corn", "soybean",
                        "commodity", "commodities", "metal", "metals", "bullion",
                    ],
                ),
                (
                    MarketSegment::Energy,
                    vec![
                        "oil", "wti", "brent", "crude", "gas", "natgas", "lng", "energy",
                        "opec", "barrel", "diesel",
                    ],
                ),
                (
                    MarketSegment::Bonds,
                    vec![
                        "bond", "bonds", "yield", "yields", "treasury", "treasuries",
                        "bund", "gilt", "rates", "coupon", "spread",
                    ],
                ),
                (
                    MarketSegment::Forex,
                    vec![
                        "forex", "fx", "currency", "currencies", "dollar", "euro", "yen",
                        "sterling", "usd", "eur", "jpy", "gbp", "yuan", "peg",
                    ],
                ),
                (
                    MarketSegment::Macro,
                    vec![
                        "inflation", "cpi", "gdp", "unemployment", "payroll", "payrolls",
                        "fed", "ecb", "central bank", "recession", "pmi", "rate hike",
                        "rate cut", "jobless",
                    ],
                ),
            ],
        }
    }
}

impl SegmentLexicon {
    /// Build from explicit `(segment, keywords)` rows. A later row for the same segment
    /// replaces the earlier one. Rows are evaluated in the order supplied.
    pub fn from_rows(rows: impl IntoIterator<Item = (MarketSegment, Vec<&'static str>)>) -> Self {
        let mut entries: Vec<(MarketSegment, Vec<&'static str>)> = Vec::new();
        for (seg, keys) in rows {
            if let Some(slot) = entries.iter_mut().find(|(s, _)| *s == seg) {
                slot.1 = keys;
            } else {
                entries.push((seg, keys));
            }
        }
        Self { entries }
    }

    /// Append extra keywords to an existing segment (creating it if absent); returns
    /// `self` for chaining. Lets callers extend the default lexicon — e.g. teaching the
    /// `Macro` spoke about a volatility gauge — without restating every default row.
    pub fn with_keywords(
        mut self,
        segment: MarketSegment,
        extra: impl IntoIterator<Item = &'static str>,
    ) -> Self {
        let extra: Vec<&'static str> = extra.into_iter().collect();
        if let Some(slot) = self.entries.iter_mut().find(|(s, _)| *s == segment) {
            slot.1.extend(extra);
        } else {
            self.entries.push((segment, extra));
        }
        self
    }

    /// Classify an event's title into a segment, or `None` if no keyword matches.
    pub fn classify(&self, event: &Event) -> Option<MarketSegment> {
        let title = event.title.to_lowercase();
        for (seg, keys) in &self.entries {
            if keys.iter().any(|k| word_match(&title, k)) {
                return Some(*seg);
            }
        }
        None
    }
}

/// Whole-word/phrase match: `needle` occurs in `haystack` with non-alphanumeric (or string
/// edge) boundaries on both sides. Keeps short tickers (`fx`, `eth`, `usd`) from firing
/// inside unrelated words while still matching symbols like `s&p`. Both inputs lowercase.
fn word_match(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let mut from = 0;
    while let Some(rel) = haystack[from..].find(needle) {
        let start = from + rel;
        let end = start + needle.len();
        let before_ok = start == 0
            || !haystack[..start].chars().next_back().is_some_and(|c| c.is_alphanumeric());
        let after_ok = end == haystack.len()
            || !haystack[end..].chars().next().is_some_and(|c| c.is_alphanumeric());
        if before_ok && after_ok {
            return true;
        }
        from = start + 1;
    }
    false
}

/// Per-segment salience weights — how much each spoke counts toward the composite stress.
/// The default set is uniform (each spoke 1.0), so the radar is a symmetric seven-axis
/// chart; callers can retune (e.g. an equities-led variant).
#[derive(Debug, Clone)]
pub struct SegmentWeights {
    entries: Vec<(MarketSegment, f64)>,
}

impl Default for SegmentWeights {
    fn default() -> Self {
        Self { entries: MarketSegment::ALL.iter().map(|s| (*s, 1.0)).collect() }
    }
}

impl SegmentWeights {
    /// Set (or insert) a segment's weight; returns `self` for chaining.
    pub fn with(mut self, segment: MarketSegment, weight: f64) -> Self {
        if let Some(slot) = self.entries.iter_mut().find(|(s, _)| *s == segment) {
            slot.1 = weight;
        } else {
            self.entries.push((segment, weight));
        }
        self
    }

    /// Weight for a segment (`0.0` if absent).
    pub fn weight(&self, segment: MarketSegment) -> f64 {
        self.entries
            .iter()
            .find(|(s, _)| *s == segment)
            .map(|(_, w)| *w)
            .unwrap_or(0.0)
    }

    /// Sum of all segment weights — the composite's denominator.
    pub fn total(&self) -> f64 {
        self.entries.iter().map(|(_, w)| *w).sum()
    }
}

/// Tunables for [`radar`].
#[derive(Debug, Clone)]
pub struct RadarParams {
    /// Title→segment classifier.
    pub lexicon: SegmentLexicon,
    /// Per-segment salience weights (the radar's denominator).
    pub weights: SegmentWeights,
    /// Within a segment: weight on the peak (worst) stress. With `volume_weight` this
    /// should sum to 1 so each segment intensity stays in `[0, 1]`.
    pub peak_weight: f64,
    /// Within a segment: weight on the saturating event-volume term.
    pub volume_weight: f64,
    /// Event count at which a segment's volume term reaches ~63% of its max.
    pub volume_scale: f64,
}

impl Default for RadarParams {
    fn default() -> Self {
        Self {
            lexicon: SegmentLexicon::default(),
            weights: SegmentWeights::default(),
            peak_weight: 0.7,
            volume_weight: 0.3,
            volume_scale: 4.0,
        }
    }
}

/// Colour-coded market-stress band derived from the composite score.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StressLevel {
    Calm,
    Steady,
    Choppy,
    Stressed,
    Panic,
}

impl StressLevel {
    /// Bin a composite `[0, 1]` score. Calibrated to the full seven-segment denominator:
    /// reaching `Stressed`/`Panic` takes broad stress, not a single hot spoke.
    pub fn from_score(score: f64) -> Self {
        match score {
            s if s >= 0.35 => StressLevel::Panic,
            s if s >= 0.20 => StressLevel::Stressed,
            s if s >= 0.10 => StressLevel::Choppy,
            s if s >= 0.03 => StressLevel::Steady,
            _ => StressLevel::Calm,
        }
    }

    /// Short UI label.
    pub fn label(&self) -> &'static str {
        match self {
            StressLevel::Calm => "Calm",
            StressLevel::Steady => "Steady",
            StressLevel::Choppy => "Choppy",
            StressLevel::Stressed => "Stressed",
            StressLevel::Panic => "Panic",
        }
    }

    /// `#rrggbb` colour for a panel chip (green → red ramp).
    pub fn color(&self) -> &'static str {
        match self {
            StressLevel::Calm => "#2ecc71",
            StressLevel::Steady => "#a3cb38",
            StressLevel::Choppy => "#f1c40f",
            StressLevel::Stressed => "#e67e22",
            StressLevel::Panic => "#e74c3c",
        }
    }

    fn rank(&self) -> u8 {
        match self {
            StressLevel::Calm => 0,
            StressLevel::Steady => 1,
            StressLevel::Choppy => 2,
            StressLevel::Stressed => 3,
            StressLevel::Panic => 4,
        }
    }
}

// Order Calm < Steady < … < Panic so `>=` comparisons read naturally.
impl PartialOrd for StressLevel {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for StressLevel {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.rank().cmp(&other.rank())
    }
}

/// One spoke's reading on the radar.
#[derive(Debug, Clone, Serialize)]
pub struct SegmentReading {
    pub segment: MarketSegment,
    /// Salience weight applied to this segment.
    pub weight: f64,
    /// Number of market events classified into this segment.
    pub count: usize,
    /// Peak member stress in `[0, 1]`.
    pub peak: f64,
    /// Within-segment intensity in `[0, 1]` (peak/volume blend).
    pub intensity: f64,
    /// This segment's share of the composite (`weight·intensity / Σ weights`).
    pub contribution: f64,
}

/// The full Finance Radar reading.
#[derive(Debug, Clone, Serialize)]
pub struct FinanceRadar {
    /// Composite market-stress score in `[0, 1]`.
    pub composite: f64,
    /// Stress band derived from the composite.
    pub level: StressLevel,
    /// Total market events classified into a segment.
    pub total_events: usize,
    /// All seven spokes, highest-contribution-first (ties by canonical order). Always the
    /// full set, so the chart axes stay stable across refreshes.
    pub segments: Vec<SegmentReading>,
    /// The top-contributing segment with at least one event, if any.
    pub dominant: Option<MarketSegment>,
    /// Market events whose title matched no segment keyword.
    pub unclassified: usize,
    /// Non-market events in the input (skipped — the radar is market-only).
    pub non_market: usize,
}

impl FinanceRadar {
    /// The spoke reading for a given segment (always present).
    pub fn segment(&self, segment: MarketSegment) -> &SegmentReading {
        self.segments
            .iter()
            .find(|s| s.segment == segment)
            .expect("radar always reports all seven segments")
    }

    /// How many spokes are currently active (have at least one event).
    pub fn active_segments(&self) -> usize {
        self.segments.iter().filter(|s| s.count > 0).count()
    }
}

/// Compute the Finance Radar over an event stream. Non-market events are tallied in
/// [`FinanceRadar::non_market`] and ignored; market events that match no segment keyword
/// are tallied in [`FinanceRadar::unclassified`]. All seven spokes are always reported.
pub fn radar(events: &[Event], params: &RadarParams) -> FinanceRadar {
    // Per-segment (count, peak), keyed by canonical order.
    let mut agg: Vec<(usize, f64)> = vec![(0, 0.0); MarketSegment::ALL.len()];
    let mut total_events = 0usize;
    let mut unclassified = 0usize;
    let mut non_market = 0usize;

    for e in events {
        if e.kind != EventKind::Market {
            non_market += 1;
            continue;
        }
        match params.lexicon.classify(e) {
            Some(seg) => {
                let slot = &mut agg[seg.rank() as usize];
                slot.0 += 1;
                slot.1 = slot.1.max(e.severity.value());
                total_events += 1;
            }
            None => unclassified += 1,
        }
    }

    let denom = params.weights.total();
    let mut segments: Vec<SegmentReading> = MarketSegment::ALL
        .iter()
        .map(|seg| {
            let (count, peak) = agg[seg.rank() as usize];
            let weight = params.weights.weight(*seg);
            let intensity = if count == 0 {
                0.0
            } else {
                let volume = if params.volume_scale > 0.0 {
                    1.0 - (-(count as f64) / params.volume_scale).exp()
                } else {
                    1.0
                };
                (params.peak_weight * peak + params.volume_weight * volume).clamp(0.0, 1.0)
            };
            let contribution = if denom > 0.0 { weight * intensity / denom } else { 0.0 };
            SegmentReading { segment: *seg, weight, count, peak, intensity, contribution }
        })
        .collect();

    // Highest-contribution first; ties by canonical spoke order for determinism.
    segments.sort_by(|a, b| {
        b.contribution
            .partial_cmp(&a.contribution)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.segment.rank().cmp(&b.segment.rank()))
    });

    // `+ 0.0` collapses an all-calm -0.0 to a clean +0.0.
    let composite = segments.iter().map(|s| s.contribution).sum::<f64>().clamp(0.0, 1.0) + 0.0;
    let dominant = segments.iter().find(|s| s.count > 0).map(|s| s.segment);

    FinanceRadar {
        composite,
        level: StressLevel::from_score(composite),
        total_events,
        segments,
        dominant,
        unclassified,
        non_market,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use ee_core::Severity;

    fn mkt(title: &str, sev: f64) -> Event {
        ev(EventKind::Market, title, sev)
    }

    fn ev(kind: EventKind, title: &str, sev: f64) -> Event {
        Event {
            id: title.into(),
            source_id: "test".into(),
            kind,
            title: title.into(),
            time: Utc.timestamp_opt(1_700_000_000, 0).single().unwrap(),
            geo: None,
            severity: Severity::new(sev),
            url: None,
            raw: serde_json::Value::Null,
        }
    }

    #[test]
    fn classifies_each_segment() {
        let lex = SegmentLexicon::default();
        let cases = [
            ("S&P 500 tumbles 3%", MarketSegment::Equities),
            ("Bitcoin slides below 60k", MarketSegment::Crypto),
            ("Gold hits record high", MarketSegment::Commodities),
            ("Brent crude spikes on supply fears", MarketSegment::Energy),
            ("10-year Treasury yield jumps", MarketSegment::Bonds),
            ("Dollar surges against the yen", MarketSegment::Forex),
            ("US CPI inflation hotter than expected", MarketSegment::Macro),
        ];
        for (title, want) in cases {
            assert_eq!(lex.classify(&mkt(title, 0.5)), Some(want), "title: {title}");
        }
    }

    #[test]
    fn with_keywords_extends_a_segment() {
        // The default Macro spoke doesn't know "vix"; teach it without restating rows.
        let base = SegmentLexicon::default();
        assert_eq!(base.classify(&mkt("VIX volatility spikes", 0.9)), None);
        let tuned = SegmentLexicon::default().with_keywords(MarketSegment::Macro, ["vix", "volatility"]);
        assert_eq!(
            tuned.classify(&mkt("VIX volatility spikes", 0.9)),
            Some(MarketSegment::Macro)
        );
        // Existing defaults still fire.
        assert_eq!(tuned.classify(&mkt("S&P 500 dips", 0.4)), Some(MarketSegment::Equities));
    }

    #[test]
    fn word_match_respects_boundaries() {
        // Short tickers must not fire inside unrelated words.
        assert!(!word_match("a quiet day in bethlehem", "eth"));
        assert!(word_match("eth rallies", "eth"));
        // Symbol keyword with punctuation still matches.
        assert!(word_match("s&p 500 closes lower", "s&p"));
        // `fx` should not match `affix`.
        assert!(!word_match("a new affix policy", "fx"));
        assert!(word_match("fx volatility rises", "fx"));
    }

    #[test]
    fn always_reports_all_seven_spokes() {
        let events = vec![mkt("Bitcoin crashes", 0.9)];
        let r = radar(&events, &RadarParams::default());
        assert_eq!(r.segments.len(), 7);
        // Every canonical segment present exactly once.
        for seg in MarketSegment::ALL {
            assert_eq!(r.segments.iter().filter(|s| s.segment == seg).count(), 1);
        }
        assert_eq!(r.active_segments(), 1);
        assert_eq!(r.dominant, Some(MarketSegment::Crypto));
    }

    #[test]
    fn broad_stress_outranks_single_loud_segment() {
        // Market A: moderate stress across four segments. Market B: one very loud segment.
        let broad = vec![
            mkt("S&P 500 falls", 0.6),
            mkt("Treasury yields spike", 0.6),
            mkt("Dollar jumps", 0.6),
            mkt("CPI inflation surprise", 0.6),
        ];
        let narrow = vec![
            mkt("Bitcoin crashes", 1.0),
            mkt("Ethereum crashes", 1.0),
            mkt("Solana crashes", 1.0),
        ];
        let a = radar(&broad, &RadarParams::default());
        let b = radar(&narrow, &RadarParams::default());
        // Systemic breadth scores higher than a single saturated spoke.
        assert!(a.composite > b.composite, "{} !> {}", a.composite, b.composite);
        assert!(a.active_segments() > b.active_segments());
    }

    #[test]
    fn composite_stays_in_unit_range_and_bins() {
        // Saturate every spoke at peak 1.0 with high volume.
        let mut events = Vec::new();
        let per_seg = [
            "S&P crash", "Bitcoin crash", "Gold crash", "Oil crash", "Yield crash",
            "Dollar crash", "CPI shock",
        ];
        for t in per_seg {
            for _ in 0..30 {
                events.push(mkt(t, 1.0));
            }
        }
        let r = radar(&events, &RadarParams::default());
        assert!(r.composite <= 1.0 + 1e-9);
        assert!(r.composite > 0.9, "all-spoke saturation should be near 1, got {}", r.composite);
        assert_eq!(r.level, StressLevel::Panic);
        assert_eq!(r.active_segments(), 7);
    }

    #[test]
    fn intensity_blends_peak_and_volume() {
        // One acute shock vs many mild moves in the same segment: peak leads.
        let acute = radar(&[mkt("Bitcoin crashes", 0.9)], &RadarParams::default());
        let chronic = {
            let evs: Vec<Event> = (0..10).map(|_| mkt("Bitcoin dips", 0.3)).collect();
            radar(&evs, &RadarParams::default())
        };
        assert!(acute.composite > chronic.composite);
        // But volume moved the chronic segment off its bare 0.3 peak.
        let chronic_crypto = chronic.segment(MarketSegment::Crypto);
        assert!(chronic_crypto.intensity > 0.7 * 0.3);
        assert_eq!(chronic_crypto.count, 10);
    }

    #[test]
    fn non_market_and_unclassified_tallied() {
        let events = vec![
            mkt("S&P 500 dips", 0.4),         // equities
            mkt("Tulip futures go wild", 0.5), // market, no keyword -> unclassified
            ev(EventKind::Earthquake, "M6 quake", 0.8), // non-market
            ev(EventKind::Cyber, "CVE exploited", 0.7), // non-market
        ];
        let r = radar(&events, &RadarParams::default());
        assert_eq!(r.total_events, 1);
        assert_eq!(r.unclassified, 1);
        assert_eq!(r.non_market, 2);
    }

    #[test]
    fn calm_market_is_calm_and_zero() {
        let r = radar(&[], &RadarParams::default());
        assert_eq!(r.composite, 0.0);
        assert_eq!(r.level, StressLevel::Calm);
        assert!(r.dominant.is_none());
        assert_eq!(r.segments.len(), 7);
        assert_eq!(r.active_segments(), 0);
    }

    #[test]
    fn weights_can_retune_the_radar() {
        // An equities-led variant: equities weighted up, everything else down.
        let mut weights = SegmentWeights::default();
        for seg in MarketSegment::ALL {
            weights = weights.with(seg, if seg == MarketSegment::Equities { 4.0 } else { 0.5 });
        }
        let params = RadarParams { weights, ..RadarParams::default() };
        // Same intensity in equities vs crypto -> equities contributes far more.
        let events = vec![mkt("S&P 500 dives", 0.8), mkt("Bitcoin dives", 0.8)];
        let r = radar(&events, &params);
        let eq = r.segment(MarketSegment::Equities);
        let cr = r.segment(MarketSegment::Crypto);
        assert!(eq.contribution > cr.contribution);
        assert_eq!(r.dominant, Some(MarketSegment::Equities));
    }

    #[test]
    fn stress_levels_order() {
        assert!(StressLevel::Panic > StressLevel::Stressed);
        assert!(StressLevel::Choppy > StressLevel::Steady);
        assert!(StressLevel::Steady > StressLevel::Calm);
        assert_eq!(StressLevel::from_score(0.5), StressLevel::Panic);
        assert_eq!(StressLevel::from_score(0.0), StressLevel::Calm);
    }
}
