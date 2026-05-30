// ------------------------------------------------------------
// RAiTHE INDUSTRIES INCORPORATED
// Copyright (c) 2026 All Rights Reserved.
//
// This file is part of a proprietary system. Unauthorized use,
// reproduction, or distribution is strictly prohibited.
// ------------------------------------------------------------

// src/models.rs — shared data structures, enums, constants

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

// ── Elevation threshold — single source of truth ──────────────────────────────
// bayesian.rs imports this so both modules always agree on "elevated".
pub const ELEVATION_THRESHOLD: f64 = 0.32;

// ── Historical anchor ─────────────────────────────────────────────────────────
// Two world wars over 2026 years → P₀ = 0.000987 / yr
pub const HISTORICAL_ANCHOR: f64 = 2.0 / 2026.0;

// ── Source tier ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceTier {
    Tier1 = 1,
    Tier2 = 2,
    Tier3 = 3,
}

impl SourceTier {
    pub fn credibility_weight(&self) -> f64 {
        match self {
            SourceTier::Tier1 => 1.00,
            SourceTier::Tier2 => 0.75,
            SourceTier::Tier3 => 0.20,
        }
    }
}

impl Default for SourceTier {
    fn default() -> Self {
        SourceTier::Tier3
    }
}

// ── Event type ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    MilitaryStrike,
    TroopDeployment,
    NuclearTest,
    MissileLaunch,
    DiplomaticExpulsion,
    SanctionsImposed,
    CyberAttack,
    AllianceInvocation,
    Ceasefire,
    PeaceTalks,
    PoliticalStatement,
    CivilianCasualty,
    WmdUse,
    #[default]
    Unknown,
}

// ── Alert level ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AlertLevel {
    #[default]
    Normal,
    Elevated,
    Critical,
}

impl std::fmt::Display for AlertLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AlertLevel::Normal   => write!(f, "normal"),
            AlertLevel::Elevated => write!(f, "elevated"),
            AlertLevel::Critical => write!(f, "critical"),
        }
    }
}

// ── Domain IDs ────────────────────────────────────────────────────────────────

pub const DOMAIN_IDS: &[&str] = &[
    "military_escalation",
    "nuclear_posture",
    "diplomatic_breakdown",
    "economic_warfare",
    "cyber_info_ops",
    "alliance_activation",
    "great_power_conflict",
    "wmd_mass_casualty",
];

// ── Region map ────────────────────────────────────────────────────────────────

pub fn resolve_region(location: &str) -> Option<String> {
    let loc = location.to_lowercase();
    let map: &[(&str, &str)] = &[
        ("united states", "north_america"), ("canada", "north_america"), ("mexico", "north_america"),
        ("russia", "europe_eurasia"),       ("ukraine", "europe_eurasia"), ("belarus", "europe_eurasia"),
        ("germany", "europe_eurasia"),      ("france", "europe_eurasia"),  ("uk", "europe_eurasia"),
        ("united kingdom", "europe_eurasia"),("poland", "europe_eurasia"),
        ("china", "asia_pacific"),          ("taiwan", "asia_pacific"),    ("japan", "asia_pacific"),
        ("south korea", "asia_pacific"),    ("north korea", "asia_pacific"),
        ("india", "south_asia"),            ("pakistan", "south_asia"),    ("afghanistan", "south_asia"),
        ("iran", "middle_east"),            ("israel", "middle_east"),     ("saudi arabia", "middle_east"),
        ("syria", "middle_east"),           ("iraq", "middle_east"),       ("lebanon", "middle_east"),
        ("egypt", "africa"),                ("ethiopia", "africa"),        ("sudan", "africa"),
        ("brazil", "latin_america"),        ("venezuela", "latin_america"),("colombia", "latin_america"),
    ];

    map.iter()
        .filter(|(country, _)| loc.contains(country))
        .max_by_key(|(country, _)| country.len())
        .map(|(_, region)| region.to_string())
}

// ── Actor normalisation ───────────────────────────────────────────────────────
//
// Maps any raw actor string — whether from article text or from the controlled
// actor_entity_patterns dictionary in processor.rs — to a canonical snake_case
// actor ID. The ID is used for deduplication and diversity scoring in the
// Bayesian engine.
//
// Design: longest-match-wins substring search. Ordering of the alias list is
// irrelevant — the match with the longest key always wins, ensuring the most
// specific canonical form. "pentagon" → united_states_military, not
// united_states. "us military exercises" → united_states_military, not
// united_states.
//
// All short tokens are safe. Future alias additions must repeat check.
//
// Alignment requirement: every pattern in actor_entity_patterns() in
// processor.rs must have a corresponding entry here that produces the correct
// actor_id for its display name. The two tables must agree.

pub fn normalize_actor(raw: &str) -> String {
    let lower = raw.to_lowercase();
    let trimmed = lower.trim();

    // Aliases ordered here for readability only — longest-match logic makes
    // ordering irrelevant for correctness.
    const ALIASES: &[(&str, &str)] = &[
        // ── United States ───────────────────────────────────────────────────
        ("united states of america",   "united_states"),
        ("united states military",     "united_states_military"),
        ("united states",              "united_states"),
        ("u.s. military",              "united_states_military"),
        ("us military",                "united_states_military"),
        ("u.s.",                       "united_states"),
        ("america",                    "united_states"),
        ("pentagon",                   "united_states_military"),
        ("white house",                "united_states"),
        ("washington",                 "united_states"),
        ("cia",                        "united_states"),
        ("fbi",                        "united_states"),
        ("us",                         "united_states"),
        // ── Russia ─────────────────────────────────────────────────────────
        ("russian federation",         "russia"),
        ("russian military",           "russia_military"),
        ("russian forces",             "russia_military"),
        ("russia",                     "russia"),
        ("kremlin",                    "russia"),
        ("moscow",                     "russia"),
        ("wagner group",               "russia_wagner"),
        ("wagner",                     "russia_wagner"),
        // ── China ──────────────────────────────────────────────────────────
        ("people's liberation army",   "china_military"),
        ("people's republic of china", "china"),
        ("chinese military",           "china_military"),
        ("china",                      "china"),
        ("beijing",                    "china"),
        ("prc",                        "china"),
        ("pla",                        "china_military"),
        // ── NATO / alliances ────────────────────────────────────────────────
        ("un security council",        "un_security_council"),
        ("united nations",             "united_nations"),
        ("european union",             "european_union"),
        ("nato",                       "nato"),
        ("aukus",                      "aukus"),
        ("quad",                       "quad"),
        // ── Ukraine ────────────────────────────────────────────────────────
        ("ukrainian forces",           "ukraine_military"),
        ("ukraine",                    "ukraine"),
        ("kyiv",                       "ukraine"),
        ("zelensky",                   "ukraine"),
        // ── Israel ─────────────────────────────────────────────────────────
        ("israel military",            "israel_military"),
        ("israel",                     "israel"),
        ("idf",                        "israel_military"),
        ("tel aviv",                   "israel"),
        ("mossad",                     "israel"),
        ("netanyahu",                  "israel"),
        // ── Iran ───────────────────────────────────────────────────────────
        ("iran military",              "iran_military"),
        ("iran",                       "iran"),
        ("irgc",                       "iran_military"),
        ("tehran",                     "iran"),
        ("khamenei",                   "iran"),
        // ── North Korea ────────────────────────────────────────────────────
        ("north korea",                "north_korea"),
        ("dprk",                       "north_korea"),
        ("pyongyang",                  "north_korea"),
        ("kim jong",                   "north_korea"),
        // ── South Korea ────────────────────────────────────────────────────
        ("south korea",                "south_korea"),
        // ── United Kingdom ─────────────────────────────────────────────────
        ("united kingdom",             "united_kingdom"),
        ("mi6",                        "united_kingdom"),
        // ── Other state actors ─────────────────────────────────────────────
        ("saudi arabia",               "saudi_arabia"),
        ("india",                      "india"),
        ("pakistan",                   "pakistan"),
        ("france",                     "france"),
        ("germany",                    "germany"),
        ("japan",                      "japan"),
        ("turkey",                     "turkey"),
        ("taiwan",                     "taiwan"),
        ("syria",                      "syria"),
        ("iraq",                       "iraq"),
        ("afghanistan",                "afghanistan"),
        ("venezuela",                  "venezuela"),
        ("cuba",                       "cuba"),
        // ── Non-state actors ───────────────────────────────────────────────
        ("hezbollah",                  "hezbollah"),
        ("hamas",                      "hamas"),
        ("houthis",                    "houthis"),
        ("isis",                       "isis"),
        ("isil",                       "isis"),
        // ── Leaders ────────────────────────────────────────────────────────
        ("xi jinping",                 "china"),
        ("putin",                      "russia"),
        ("zelensky",                   "ukraine"),
        ("netanyahu",                  "israel"),
        ("kim jong",                   "north_korea"),
        ("khamenei",                   "iran"),
        ("biden",                      "united_states"),
        ("trump",                      "united_states"),
        // ── Locations that are not actors — explicit no-match via fallthrough
        // "south china sea" must not match "china" — it is a location string,
        // not an actor, and should not appear as input to normalize_actor.
        // No alias entry; it falls through to snake_case: south_china_sea.

        ("comprehensive nuclear-test-ban treaty", "ctbto"),
        ("iaea",                       "iaea"),
    ];

    // Longest-match-wins: find all matching aliases and take the one with the
    // longest key. This ensures "us military" beats "us", "russian forces"
    // beats "russia", etc., regardless of list ordering.
    ALIASES.iter()
        .filter(|(alias, _)| trimmed.contains(alias))
        .max_by_key(|(alias, _)| alias.len())
        .map(|(_, norm)| norm.to_string())
        .unwrap_or_else(|| trimmed.replace(' ', "_"))
}

// ── Great power check ─────────────────────────────────────────────────────────
// Operates on the raw display actor string (e.g. "US Military", "Russia").
// Uses substring matching so it correctly handles any display name variant.
// Aligned with the actor_entity_patterns display names in processor.rs.

pub fn is_great_power(actor: &str) -> bool {
    const GREAT_POWERS: &[&str] = &[
        // United States — state and military
        "united states", "us military", "pentagon", "washington", "white house",
        // Russia — state and military
        "russia", "kremlin", "moscow",
        // China — state and military
        "china", "pla", "chinese military", "beijing",
        // NATO — structural great power collective
        "nato",
    ];
    let lower = actor.to_lowercase();
    GREAT_POWERS.iter().any(|gp| lower.contains(gp))
}

// ── Raw article ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawArticle {
    pub id:           String,
    pub url:          String,
    pub title:        String,
    pub body:         String,
    pub source:       String,
    pub source_tier:  SourceTier,
    pub published_at: DateTime<Utc>,
    pub fetched_at:   DateTime<Utc>,
    pub language:     String,
}

impl RawArticle {
    pub fn new(
        url: String, title: String, body: String,
        source: String, tier: SourceTier, published_at: DateTime<Utc>,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            url, title, body, source,
            source_tier: tier,
            published_at,
            fetched_at: Utc::now(),
            language: "en".into(),
        }
    }
}

// ── Geopolitical event ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeopoliticalEvent {
    pub id:             String,
    pub raw_article_id: String,

    pub event_type:  EventType,
    pub title:       String,
    pub summary:     String,

    pub location:      String,
    pub region:        Option<String>,
    pub latitude:      Option<f64>,
    pub longitude:     Option<f64>,
    pub country_codes: Vec<String>,

    pub actors:           Vec<String>,
    pub actor_ids:        Vec<String>,
    pub target_actors:    Vec<String>,
    pub target_actor_ids: Vec<String>,
    pub great_power_involved: bool,

    pub casualties:       Option<u32>,
    pub civilian_impact:  bool,
    pub severity:         f64,
    pub nuclear_indicator: bool,
    pub wmd_indicator:    bool,

    pub source:             String,
    pub source_tier:        SourceTier,
    pub credibility_weight: f64,

    /// Number of independent sources corroborating this event.
    /// Initialized to 1 (the canonical source). Incremented by the aggregator
    /// corroboration layer when a near-duplicate event arrives from a different
    /// source. Each corroboration also boosts credibility_weight directly.
    pub corroboration_count: u32,

    pub escalation_language_score: f64,
    pub sentiment_score:           f64,

    /// Weighted NLP domain signals: domain_id → signal strength [0.0, 1.0].
    /// Produced by the weighted keyword scorer in processor.rs. Signal strength
    /// reflects keyword quality — a single definitive keyword like "nuclear test"
    /// scores higher than five ambient keywords like "nuclear" + "military" etc.
    /// This flows directly into the Bayesian domain scorer as a quality multiplier.
    pub domain_signals: HashMap<String, f64>,

    /// Flat tag list derived from domain_signals — present iff signal > 0.0.
    /// Retained for article store backfill (nlp_sidecar.rs) and actor tracker
    /// (bayesian.rs). Always kept in sync with domain_signals by processor.rs.
    pub domain_tags: Vec<String>,

    pub published_at: DateTime<Utc>,
    pub ingested_at:  DateTime<Utc>,
}

impl GeopoliticalEvent {
    pub fn new(title: String, source: String, tier: SourceTier, published_at: DateTime<Utc>) -> Self {
        let weight = tier.credibility_weight();
        Self {
            id:             Uuid::new_v4().to_string(),
            raw_article_id: String::new(),
            event_type:  EventType::Unknown,
            title, summary: String::new(),
            location: String::new(), region: None,
            latitude: None, longitude: None, country_codes: vec![],
            actors: vec![], actor_ids: vec![],
            target_actors: vec![], target_actor_ids: vec![],
            great_power_involved: false,
            casualties: None, civilian_impact: false,
            severity: 0.0, nuclear_indicator: false, wmd_indicator: false,
            source, source_tier: tier, credibility_weight: weight,
            corroboration_count: 1,
            escalation_language_score: 0.0, sentiment_score: 0.0,
            domain_signals: HashMap::new(),
            domain_tags: vec![],
            published_at, ingested_at: Utc::now(),
        }
    }
}

// ── Domain score ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainScore {
    pub domain_id:   String,
    pub score:       f64,       // 0–1
    pub confidence:  f64,       // 0–1
    pub event_count: usize,
    pub great_power_event_count: usize,
    pub contributing_events: Vec<String>,
    pub computed_at: DateTime<Utc>,
}

impl DomainScore {
    pub fn zero(domain_id: &str) -> Self {
        Self {
            domain_id: domain_id.to_string(),
            score: 0.0, confidence: 0.05,
            event_count: 0, great_power_event_count: 0,
            contributing_events: vec![],
            computed_at: Utc::now(),
        }
    }

    /// True if score meets or exceeds the elevation threshold.
    pub fn elevated(&self) -> bool {
        self.score >= ELEVATION_THRESHOLD
    }

    /// Human-readable label — aligned with ELEVATION_THRESHOLD.
    pub fn label(&self) -> &'static str {
        if self.score >= 0.70        { "critical"  }
        else if self.score >= ELEVATION_THRESHOLD { "elevated"  }
        else if self.score >= 0.20   { "moderate"  }
        else                         { "low"       }
    }
}

// ── Risk snapshot ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskSnapshot {
    pub snapshot_id:    String,
    pub computed_at:    DateTime<Utc>,
    pub aggregation_window_hours: f64,

    pub historical_anchor:  f64,
    pub regime_multiplier:  f64,
    pub adjusted_prior:     f64,

    pub domain_scores:     HashMap<String, DomainScore>,
    pub elevated_domains:  usize,
    pub co_occurrence_boost: f64,

    pub likelihood_ratio:    f64,
    pub weighted_domain_sum: f64,

    pub p_wwiii_annual: f64,
    pub p_wwiii_30day:  f64,
    pub p_wwiii_90day:  f64,

    pub estimate_confidence: f64,

    pub alert_level:   AlertLevel,
    pub alert_message: String,

    pub events_in_window:  usize,
    pub sources_active:    usize,
    pub great_power_events: usize,
    pub regions_active:    Vec<String>,
    pub top_actors:        Vec<String>,

    pub delta_annual: f64,
    pub delta_30day:  f64,
}

impl Default for RiskSnapshot {
    fn default() -> Self {
        Self {
            snapshot_id: Uuid::new_v4().to_string(),
            computed_at: Utc::now(),
            aggregation_window_hours: 72.0,
            historical_anchor: HISTORICAL_ANCHOR,
            regime_multiplier: 1.0,
            adjusted_prior: HISTORICAL_ANCHOR,
            domain_scores: HashMap::new(),
            elevated_domains: 0,
            co_occurrence_boost: 1.0,
            likelihood_ratio: 0.0,
            weighted_domain_sum: 0.0,
            p_wwiii_annual: 0.0,
            p_wwiii_30day: 0.0,
            p_wwiii_90day: 0.0,
            estimate_confidence: 0.5,
            alert_level: AlertLevel::Normal,
            alert_message: String::new(),
            events_in_window: 0,
            sources_active: 0,
            great_power_events: 0,
            regions_active: vec![],
            top_actors: vec![],
            delta_annual: 0.0,
            delta_30day: 0.0,
        }
    }
}

// ── Timeline entry ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineEntry {
    pub t:         String,
    pub p_annual:  f64,
    pub p_30day:   f64,
    pub alert:     String,
    pub elevated:  usize,
    pub regime:    f64,
    pub events:    usize,
    pub delta:     f64,
}

impl TimelineEntry {
    pub fn from_snapshot(snap: &RiskSnapshot) -> Self {
        Self {
            t:        snap.computed_at.to_rfc3339(),
            p_annual: (snap.p_wwiii_annual * 1e8).round() / 1e8,
            p_30day:  (snap.p_wwiii_30day  * 1e8).round() / 1e8,
            alert:    snap.alert_level.to_string(),
            elevated: snap.elevated_domains,
            regime:   (snap.regime_multiplier * 1e4).round() / 1e4,
            events:   snap.events_in_window,
            delta:    (snap.delta_annual * 1e8).round() / 1e8,
        }
    }
}

// ── Regime factor ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegimeFactor {
    pub id:         String,
    pub label:      String,
    pub multiplier: f64,
    pub active:     bool,
}

// ── Settings ──────────────────────────────────────────────────────────────────
// Mirrors the structure of config/settings.yml

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub regime_factors: Vec<RegimeFactor>,
    pub alerts:         AlertSettings,
    pub ingestion:      IngestionSettings,
    pub dashboard:      DashboardSettings,
    #[serde(default)]
    pub llm:            LlmSettings,
}

// ── LlmSettings ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmSettings {
    #[serde(default)]
    pub enabled:      bool,
    #[serde(default = "LlmSettings::default_endpoint")]
    pub endpoint:     String,
    #[serde(default = "LlmSettings::default_model")]
    pub model:        String,
    #[serde(default = "LlmSettings::default_timeout")]
    pub timeout_secs: u64,
}

impl LlmSettings {
    fn default_endpoint() -> String { "http://localhost:11434".into() }
    fn default_model()    -> String { "qwen2.5:7b".into() }
    fn default_timeout()  -> u64   { 10 }
}

impl Default for LlmSettings {
    fn default() -> Self {
        Self {
            enabled:      false,
            endpoint:     Self::default_endpoint(),
            model:        Self::default_model(),
            timeout_secs: Self::default_timeout(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertSettings {
    pub elevated:      f64,
    pub critical:      f64,
    pub thirty_day_warn: f64,
}

impl Default for AlertSettings {
    fn default() -> Self {
        Self { elevated: 0.025, critical: 0.08, thirty_day_warn: 0.01 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestionSettings {
    pub poll_interval_seconds: u64,
    pub max_events_per_batch:  usize,
}

impl Default for IngestionSettings {
    fn default() -> Self {
        Self { poll_interval_seconds: 1, max_events_per_batch: 500 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardSettings {
    pub host:         String,
    pub port:         u16,
    /// API key required for all /api/operator/* and /api/regime/* endpoints.
    /// Set to a strong random string in settings.yml.
    /// If empty, operator endpoints are disabled entirely.
    #[serde(default)]
    pub operator_key: String,
    /// URL prefix under which the dashboard is served, e.g. "/risk".
    /// Empty or "/" means serve at root. Must start with "/" if non-empty.
    #[serde(default)]
    pub base_path:    String,
}

impl Default for DashboardSettings {
    fn default() -> Self {
        Self {
            host:         "0.0.0.0".into(),
            port:         8000,
            operator_key: String::new(),
            base_path:    String::new(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn historical_anchor_is_two_over_2026() {
        assert!((HISTORICAL_ANCHOR - 2.0 / 2026.0).abs() < 1e-12);
    }

    #[test]
    fn historical_anchor_less_than_one_percent() {
        assert!(HISTORICAL_ANCHOR < 0.01);
    }

    #[test]
    fn elevation_threshold_value() {
        assert_eq!(ELEVATION_THRESHOLD, 0.32);
    }

    #[test]
    fn domain_score_elevated_above_threshold() {
        let mut ds = DomainScore::zero("military_escalation");
        ds.score = ELEVATION_THRESHOLD + 0.01;
        assert!(ds.elevated());
    }

    #[test]
    fn domain_score_not_elevated_below_threshold() {
        let mut ds = DomainScore::zero("military_escalation");
        ds.score = ELEVATION_THRESHOLD - 0.01;
        assert!(!ds.elevated());
    }

    #[test]
    fn domain_score_label_elevated_at_threshold() {
        let mut ds = DomainScore::zero("military_escalation");
        ds.score = ELEVATION_THRESHOLD;
        assert_eq!(ds.label(), "elevated");
    }

    #[test]
    fn domain_score_no_ghost_band() {
        // 0.32–0.39 was previously a ghost band — must all be "elevated"
        for &score in &[0.32_f64, 0.33, 0.35, 0.38, 0.39] {
            let mut ds = DomainScore::zero("military_escalation");
            ds.score = score;
            assert!(ds.elevated(), "score={score} should be elevated");
            assert_eq!(ds.label(), "elevated", "score={score} label should be elevated");
        }
    }

    #[test]
    fn domain_score_label_critical_at_70() {
        let mut ds = DomainScore::zero("nuclear_posture");
        ds.score = 0.70;
        assert_eq!(ds.label(), "critical");
    }

    #[test]
    fn domain_score_label_low_below_20() {
        let mut ds = DomainScore::zero("cyber_info_ops");
        ds.score = 0.10;
        assert_eq!(ds.label(), "low");
    }

    #[test]
    fn source_tier_credibility_weights() {
        assert_eq!(SourceTier::Tier1.credibility_weight(), 1.00);
        assert_eq!(SourceTier::Tier2.credibility_weight(), 0.75);
        assert_eq!(SourceTier::Tier3.credibility_weight(), 0.20);
    }

    #[test]
    fn normalize_actor_basic_aliases() {
        assert_eq!(normalize_actor("us"),      "united_states");
        assert_eq!(normalize_actor("Kremlin"), "russia");
        assert_eq!(normalize_actor("DPRK"),    "north_korea");
        assert_eq!(normalize_actor("PLA"),     "china_military");
        assert_eq!(normalize_actor("NATO"),    "nato");
        assert_eq!(normalize_actor("IRGC"),    "iran_military");
        assert_eq!(normalize_actor("IDF"),     "israel_military");
    }

    #[test]
    fn normalize_actor_longest_match_beats_shorter() {
        // Previously broken with exact-match: these all fell through to snake_case fallback.
        // With longest-match they must resolve to the specific military ID, not the state ID.
        assert_eq!(normalize_actor("pentagon"),          "united_states_military");
        assert_eq!(normalize_actor("us military"),       "united_states_military");
        assert_eq!(normalize_actor("u.s. military"),     "united_states_military");
        assert_eq!(normalize_actor("russian military"),  "russia_military");
        assert_eq!(normalize_actor("russian forces"),    "russia_military");
        assert_eq!(normalize_actor("chinese military"),  "china_military");
        assert_eq!(normalize_actor("ukrainian forces"),  "ukraine_military");
        assert_eq!(normalize_actor("united states military"), "united_states_military");
    }

    #[test]
    fn normalize_actor_state_does_not_override_military() {
        // "us military exercises" contains both "us" and "us military" —
        // longest match must win and produce military ID.
        assert_eq!(normalize_actor("us military exercises"), "united_states_military");
        assert_eq!(normalize_actor("russian military forces"), "russia_military");
        assert_eq!(normalize_actor("pla military drills"),    "china_military");
    }

    #[test]
    fn normalize_actor_pattern_table_alignment() {
        // Every pattern key used in actor_entity_patterns() in processor.rs
        // must normalize to an ID consistent with its display name.
        assert_eq!(normalize_actor("pentagon"),               "united_states_military");
        assert_eq!(normalize_actor("white house"),            "united_states");
        assert_eq!(normalize_actor("washington"),             "united_states");
        assert_eq!(normalize_actor("cia"),                    "united_states");
        assert_eq!(normalize_actor("fbi"),                    "united_states");
        assert_eq!(normalize_actor("mi6"),                    "united_kingdom");
        assert_eq!(normalize_actor("mossad"),                 "israel");
        assert_eq!(normalize_actor("wagner"),                 "russia_wagner");
        assert_eq!(normalize_actor("tel aviv"),               "israel");
        assert_eq!(normalize_actor("tehran"),                 "iran");
        assert_eq!(normalize_actor("kyiv"),                   "ukraine");
        assert_eq!(normalize_actor("beijing"),                "china");
        assert_eq!(normalize_actor("moscow"),                 "russia");
        assert_eq!(normalize_actor("pyongyang"),              "north_korea");
        assert_eq!(normalize_actor("hezbollah"),              "hezbollah");
        assert_eq!(normalize_actor("hamas"),                  "hamas");
        assert_eq!(normalize_actor("houthis"),                "houthis");
        assert_eq!(normalize_actor("isis"),                   "isis");
        assert_eq!(normalize_actor("isil"),                   "isis");
        assert_eq!(normalize_actor("aukus"),                  "aukus");
        assert_eq!(normalize_actor("quad"),                   "quad");
        assert_eq!(normalize_actor("zelensky"),               "ukraine");
        assert_eq!(normalize_actor("netanyahu"),              "israel");
        assert_eq!(normalize_actor("khamenei"),               "iran");
        assert_eq!(normalize_actor("kim jong"),               "north_korea");
        assert_eq!(normalize_actor("putin"),                  "russia");
        assert_eq!(normalize_actor("xi jinping"),             "china");
        assert_eq!(normalize_actor("biden"),                  "united_states");
        assert_eq!(normalize_actor("trump"),                  "united_states");
    }

    #[test]
    fn normalize_actor_unknown_becomes_snake_case() {
        assert_eq!(normalize_actor("Some Unknown Actor"), "some_unknown_actor");
        assert_eq!(normalize_actor("Al-Shabaab"),         "al-shabaab");
    }

    #[test]
    fn normalize_actor_case_insensitive() {
        assert_eq!(normalize_actor("RUSSIA"),         "russia");
        assert_eq!(normalize_actor("United States"),  "united_states");
        assert_eq!(normalize_actor("Chinese Military"), "china_military");
    }

    #[test]
    fn is_great_power_covers_all_display_names() {
        assert!(is_great_power("United States"));
        assert!(is_great_power("US Military"));
        assert!(is_great_power("Russia"));
        assert!(is_great_power("China"));
        assert!(is_great_power("China Military"));
        assert!(is_great_power("NATO"));
        assert!(is_great_power("Washington"));
        assert!(is_great_power("White House"));
        assert!(is_great_power("Pentagon"));
    }

    #[test]
    fn is_great_power_non_great_power_returns_false() {
        assert!(!is_great_power("Hamas"));
        assert!(!is_great_power("Hezbollah"));
        assert!(!is_great_power("Iran"));
        assert!(!is_great_power("North Korea"));
        assert!(!is_great_power("Israel"));
        assert!(!is_great_power("Ukraine"));
    }

    // ── resolve_region (I-01 fix) ─────────────────────────────────────────────

    #[test]
    fn resolve_region_known_country() {
        assert_eq!(resolve_region("Ukraine"), Some("europe_eurasia".into()));
        assert_eq!(resolve_region("Iran"), Some("middle_east".into()));
        assert_eq!(resolve_region("China"), Some("asia_pacific".into()));
    }

    #[test]
    fn resolve_region_unknown_returns_none() {
        assert_eq!(resolve_region("Atlantis"), None);
    }

    #[test]
    fn resolve_region_longest_match_wins_iran_ukraine() {
        // I-01: "Iranian-backed forces in Ukraine" previously resolved to middle_east
        // (iran = 4 chars matched first). After fix: ukraine (7 chars) wins.
        let result = resolve_region("Iranian-backed forces in Ukraine");
        assert_eq!(result, Some("europe_eurasia".into()),
            "Longest key (ukraine, 7 chars) must beat shorter key (iran, 4 chars)");
    }

    #[test]
    fn resolve_region_longest_match_wins_south_korea() {
        // "south korea" (10 chars) must beat "korea" if it were in the map.
        // Currently the map has "south korea" and "north korea" — both 10 chars,
        // both resolve to asia_pacific, so the result is correct either way.
        let result = resolve_region("South Korea");
        assert_eq!(result, Some("asia_pacific".into()));
    }

    #[test]
    fn resolve_region_longest_match_wins_north_korea() {
        let result = resolve_region("North Korea");
        assert_eq!(result, Some("asia_pacific".into()));
    }

    #[test]
    fn resolve_region_single_known_entry() {
        assert_eq!(resolve_region("russia"), Some("europe_eurasia".into()));
        assert_eq!(resolve_region("japan"), Some("asia_pacific".into()));
        assert_eq!(resolve_region("india"), Some("south_asia".into()));
    }

    #[test]
    fn resolve_region_case_insensitive() {
        // to_lowercase() applied internally — uppercase input must work
        assert_eq!(resolve_region("UKRAINE"), Some("europe_eurasia".into()));
        assert_eq!(resolve_region("CHINA"), Some("asia_pacific".into()));
    }

    #[test]
    fn raw_article_constructor() {
        let a = RawArticle::new(
            "https://example.com".into(),
            "Test headline".into(),
            "Body text".into(),
            "testfeed".into(),
            SourceTier::Tier1,
            Utc::now(),
        );
        assert!(!a.id.is_empty());
        assert_eq!(a.source_tier, SourceTier::Tier1);
    }

    #[test]
    fn risk_snapshot_default_values() {
        let snap = RiskSnapshot::default();
        assert_eq!(snap.historical_anchor, HISTORICAL_ANCHOR);
        assert_eq!(snap.alert_level, AlertLevel::Normal);
        assert_eq!(snap.elevated_domains, 0);
        assert!(snap.p_wwiii_annual >= 0.0);
    }

    #[test]
    fn timeline_entry_from_snapshot_rounds_correctly() {
        let mut snap = RiskSnapshot::default();
        snap.p_wwiii_annual = 0.123456789012;
        snap.p_wwiii_30day  = 0.010999;
        let entry = TimelineEntry::from_snapshot(&snap);
        // Should be rounded to 8 decimal places
        assert!((entry.p_annual - 0.12345679).abs() < 1e-7);
    }

    #[test]
    fn all_domain_ids_present() {
        assert_eq!(DOMAIN_IDS.len(), 8);
        assert!(DOMAIN_IDS.contains(&"nuclear_posture"));
        assert!(DOMAIN_IDS.contains(&"wmd_mass_casualty"));
    }

    #[test]
    fn alert_level_display() {
        assert_eq!(AlertLevel::Normal.to_string(),   "normal");
        assert_eq!(AlertLevel::Elevated.to_string(), "elevated");
        assert_eq!(AlertLevel::Critical.to_string(), "critical");
    }
}
