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

// ── Baseline annual prior ──────────────────────────────────────────────────────
// GCRM v2: the old "2 world wars / 2026 years" anchor (0.0987%/yr) was a
// non-stationary frequentist error — it modelled systemic war as a Poisson
// process running since 1 AD, ignoring that the nuclear-armed multipolar system
// did not exist before 1945. It pinned the prior sub-0.1% so the whole model
// fought to climb out of a hole.
//
// BASELINE_ANNUAL is the calibrated quiet-year baseline for the MODERN system:
// the annualized probability of a systemic (great-power) war in a structurally
// calm year, anchored to expert/forecaster priors and backtested against analogs.
// v2 uses it as the FLAT logistic prior (the structural regime no longer inflates
// the prior — it enters the systemic likelihood as a guardrail amplifier). Fitted
// in the Phase-3 backtest so quiet ≈ 2%; the final value is Robert's calibration.
pub const BASELINE_ANNUAL: f64 = 0.015;

// Back-compat alias — some call sites/tests still reference the old name. Points
// at the new baseline so there is a single source of truth. Prefer BASELINE_ANNUAL.
pub const HISTORICAL_ANCHOR: f64 = BASELINE_ANNUAL;

// FORECAST_PROB_CEILING is the hard upper clamp on the model's annual P(WWIII).
// It is an ENGINEERING ceiling (epistemic humility), NOT a probabilistic prior:
// the model has no access to ground truth — no classified intelligence, no read on
// intentions, no view of the future — so it must never emit a value near certainty,
// however strong the open-source signal. The appropriate ceiling for extreme
// scenarios (e.g. a confirmed nuclear detonation) is a design decision belonging to
// Robert Perreault, not something derived from the model. Raised 0.85 → 0.90 in v2.
// This is the SINGLE source of truth for that clamp: it is applied in
// `bayesian.rs::compute` and rendered into the methodology page, so the code, the
// computation, and the operator-facing prose can never silently drift apart.
// Distinct from FORECAST_INDEX_CEILING (95, theater.rs), which caps the public
// systemic INDEX (a 0–100 display scale), not this annual probability.
pub const FORECAST_PROB_CEILING: f64 = 0.90;

// ── Honesty layer: headline uncertainty interval + epistemic posture (2026-06-21) ──
// A point estimate ("75.328%") projects a precision the model does not have — a forecast of an
// effectively unprecedented event, calibrated against a handful of historical analogs, cannot be
// known to five decimals. So the headline is published as an INTERVAL, never a bare point, and
// carries a plain statement of its reference-class limits and its error posture. These are the
// honest-forecasting dials (Robert's call, like the calibration bands) — named + justified here,
// rendered on the dashboard, so the choice is visible and defensible, never buried.

/// Minimum half-width of the headline interval, in probability units. The deliberate floor of
/// humility: GCRM never claims to know P(WWIII) to better than ±this, no matter how stable the
/// live read looks — because short-term stability is not the same as being right about an
/// out-of-distribution future. Widened (never narrowed) by observed volatility and thin data.
pub const HUMILITY_FLOOR_HW: f64 = 0.07;

/// How much low data-quality widens the interval: half-width ×= 1 + this × (1 − confidence).
/// Thin/stale coverage should make the band visibly wider, not silently keep its apparent precision.
pub const DATA_QUALITY_WIDENING: f64 = 1.0;

/// Plain-language reference-class limit — what the calibration is anchored to and what it may miss.
pub const EPISTEMIC_REFERENCE_CLASS: &str =
    "Forecast under deep uncertainty, calibrated to historical analogs (Cuba 1962, Ukraine 2022, a quiet year). \
     Novel dynamics — AI-enabled, cyber, multipolar — may be out-of-distribution and unmodeled.";

/// The model's error posture — which mistake it is tuned to prefer. Every choice has consequences;
/// this names ours so a reader knows how to weigh the number. MUST be kept in sync with the actual
/// tuning (the persistence floor holds an active war through news gaps → it errs toward false alarm).
pub const ERROR_POSTURE_NOTE: &str =
    "Tuned to hold an active war through news gaps — this model errs toward false alarm over false calm.";

// ── Source tier ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceTier {
    Tier1 = 1,
    Tier2 = 2,
    #[default]
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

// ── Modality (domain) IDs ───────────────────────────────────────────────────────
//
// GCRM v2: the scored axes are now FIVE genuinely orthogonal *modalities* — the
// kind of force in play — measured per theater and globally. The id strings are
// kept stable (reusing five of the legacy eight) so storage, the operator API and
// the dashboard keep working; the three dropped axes were not modalities:
//   • great_power_conflict / alliance_activation → describe WHO, not what kind of
//     force. They become SYSTEMIC COUPLERS (Phase 2), not scored domains.
//   • wmd_mass_casualty → an OUTCOME threshold. It becomes an escalation-ladder
//     RUNG override driven by event.wmd_indicator, not a continuous axis.
// Dropping them removes the collinearity where one great-power strike lit up four
// buckets at once and was counted ~4×.
//
//   military_escalation  → KINETIC          (armed force actually in use)
//   nuclear_posture      → NUCLEAR          (posture / signaling / doctrine / use)
//   economic_warfare     → COERCIVE-ECONOMIC (blockade, energy weaponization, SWIFT)
//   cyber_info_ops       → CYBER / INFO
//   diplomatic_breakdown → DIPLOMATIC        (off-ramp / channel breakdown risk)
pub const DOMAIN_IDS: &[&str] = &[
    "military_escalation",
    "nuclear_posture",
    "economic_warfare",
    "cyber_info_ops",
    "diplomatic_breakdown",
];

// ── Theater (dyad) ──────────────────────────────────────────────────────────────
//
// A systemic war is a regional war in a theater that couples to great powers while
// other theaters are also hot. v2 scores each theater independently. theater_of()
// maps an event's canonical actor_ids (+ region) to a theater so per-theater
// escalation can be tracked instead of a single global average.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theater {
    NatoRussia,
    UsIran,
    UsChinaTaiwan,
    IndiaPakistan,
    Korea,
    ChinaIndia,
    Other,
}

impl Theater {
    pub fn id(&self) -> &'static str {
        match self {
            Theater::NatoRussia    => "nato_russia",
            Theater::UsIran        => "us_iran",
            Theater::UsChinaTaiwan => "us_china_taiwan",
            Theater::IndiaPakistan => "india_pakistan",
            Theater::Korea         => "korea",
            Theater::ChinaIndia    => "china_india",
            Theater::Other         => "other",
        }
    }

    #[allow(dead_code)] // consumed by the theater UI / snapshot in Phase 2
    pub fn label(&self) -> &'static str {
        match self {
            Theater::NatoRussia    => "NATO–Russia",
            Theater::UsIran        => "US/Israel–Iran",
            Theater::UsChinaTaiwan => "US–China / Taiwan",
            Theater::IndiaPakistan => "India–Pakistan",
            Theater::Korea         => "Korean Peninsula",
            Theater::ChinaIndia    => "China–India (LAC)",
            Theater::Other         => "Other / Diffuse",
        }
    }

    /// All theaters except the catch-all, in display priority order.
    pub fn primary() -> [Theater; 6] {
        [Theater::NatoRussia, Theater::UsIran, Theater::UsChinaTaiwan,
         Theater::IndiaPakistan, Theater::Korea, Theater::ChinaIndia]
    }
}

/// Map the canonical actor_ids present in an event (plus its resolved region) to a
/// theater. Counts actor hits per theater and takes the max; ties break by the
/// Theater::primary() priority order. Returns Other when nothing matches — a story
/// with no tracked dyad does not belong to a named theater.
pub fn theater_of(actor_ids: &[String], region: Option<&str>) -> Theater {
    // Actor → theater membership. An actor can belong to multiple theaters (e.g.
    // united_states appears in several); the per-theater count + region tiebreak
    // resolves which one a given story is actually about.
    fn theaters_for(actor: &str) -> &'static [Theater] {
        match actor {
            "russia" | "russia_military" | "russia_wagner" | "ukraine" | "ukraine_military"
                => &[Theater::NatoRussia],
            "nato"  => &[Theater::NatoRussia],
            "iran"  | "iran_military" | "israel" | "israel_military"
            | "hezbollah" | "hamas" | "houthis" | "saudi_arabia" | "syria"
                => &[Theater::UsIran],
            "china" | "china_military" | "taiwan"
                => &[Theater::UsChinaTaiwan],
            "india" | "pakistan"
                => &[Theater::IndiaPakistan],
            "north_korea" | "south_korea"
                => &[Theater::Korea],
            // Great powers that span theaters — counted toward whichever theater
            // their co-actors already implicate (handled via region tiebreak below).
            _ => &[],
        }
    }

    // China–India border clashes (e.g. Galwan 2020 — two nuclear great powers) are their own
    // dyad. Both `china` and `india` independently map to *named* theaters (UsChinaTaiwan /
    // IndiaPakistan), so without this guard the per-actor count + region tiebreak would absorb a
    // China–India standoff into Taiwan or Kashmir — fabricating heat in a flashpoint the event is
    // NOT about and able to name the wrong *lead* theater (telling the operator "US–China/Taiwan"
    // while the fighting is on the Himalayan border). The 2026-06-29 interim routed this to Other
    // (invisible but no longer misattributed); now the dedicated `ChinaIndia` theater gives the
    // clash a home of its own — its own heat, ladder chip, and entanglement contribution. Guarded
    // on the ABSENCE of taiwan/pakistan so a genuine China–Taiwan or India–Pakistan story (where
    // that partner IS present) still resolves to its own theater.
    let has = |a: &str| actor_ids.iter().any(|x| x == a);
    if (has("china") || has("china_military")) && has("india")
        && !has("taiwan") && !has("pakistan")
    {
        return Theater::ChinaIndia;
    }

    let mut counts: [usize; 6] = [0; 6];
    let idx = |t: Theater| Theater::primary().iter().position(|x| *x == t).unwrap();
    for aid in actor_ids {
        for t in theaters_for(aid) {
            counts[idx(*t)] += 1;
        }
    }

    // Region as a soft tiebreak / weak signal when actors are ambiguous.
    if let Some(r) = region {
        let bump = match r {
            "europe_eurasia" => Some(Theater::NatoRussia),
            "middle_east"    => Some(Theater::UsIran),
            "asia_pacific"   => Some(Theater::UsChinaTaiwan),
            "south_asia"     => Some(Theater::IndiaPakistan),
            _ => None,
        };
        if let Some(t) = bump { counts[idx(t)] += 1; }
    }

    let max = *counts.iter().max().unwrap_or(&0);
    if max == 0 { return Theater::Other; }
    // First theater (in priority order) achieving the max wins.
    let pos = counts.iter().position(|&c| c == max).unwrap();
    Theater::primary()[pos]
}

// ── Escalation ladder ───────────────────────────────────────────────────────────
//
// Kahn / CrisisWatch-style discrete, legible rungs. Each theater sits on one rung,
// derived from its modality scores (Phase 2). WMD/nuclear use force a rung override
// rather than being a continuous domain.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EscalationRung {
    #[default]
    Stable,
    Tension,
    Crisis,
    LimitedWar,
    GreatPowerWar,
    Systemic,
}

impl EscalationRung {
    /// 0..5 numeric level for ordering, charts, and the systemic index.
    pub fn level(&self) -> u8 {
        match self {
            EscalationRung::Stable        => 0,
            EscalationRung::Tension       => 1,
            EscalationRung::Crisis        => 2,
            EscalationRung::LimitedWar    => 3,
            EscalationRung::GreatPowerWar => 4,
            EscalationRung::Systemic      => 5,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            EscalationRung::Stable        => "Stable",
            EscalationRung::Tension       => "Tension",
            EscalationRung::Crisis        => "Crisis",
            EscalationRung::LimitedWar    => "Limited War",
            EscalationRung::GreatPowerWar => "Great-Power War",
            EscalationRung::Systemic      => "Systemic War",
        }
    }
}

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
        ("houthi",                     "houthis"),
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

    /// v2: theater id this event belongs to (from theater_of()). Option/String
    /// rather than the enum so older serialised events (no field) deserialize via
    /// serde default, and the on-disk archive stays human-readable.
    #[serde(default)]
    pub theater: Option<String>,

    /// v2: signed Goldstein-style escalation step in [-1.0, 1.0]. Negative =
    /// de-escalatory (ceasefire/talks), positive = escalatory. A keyword-derived
    /// placeholder in Phase 1; the LLM extractor produces the real value in Phase 4.
    #[serde(default)]
    pub escalation_step: f64,

    /// v2: true if the article invokes a mutual-defense alliance (Article 5, NATO
    /// invoked, collective defence). Feeds the alliance-chain systemic coupler
    /// (Phase 2) instead of being its own scored domain.
    #[serde(default)]
    pub alliance_indicator: bool,

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

    /// The set of ADDITIONAL sources (beyond the canonical `source`) that have already
    /// corroborated this event. Used by the aggregator to ensure the SAME source can't
    /// inflate corroboration_count / credibility_weight more than once with reworded
    /// headlines — only a genuinely new source counts. serde-default so older persisted
    /// events deserialize cleanly. (audit aggregator-4)
    #[serde(default)]
    pub corroborating_sources: Vec<String>,

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
            theater: None, escalation_step: 0.0, alliance_indicator: false,
            casualties: None, civilian_impact: false,
            severity: 0.0, nuclear_indicator: false, wmd_indicator: false,
            source, source_tier: tier, credibility_weight: weight,
            corroboration_count: 1,
            corroborating_sources: Vec::new(),
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

// ── Theater state (v2) ──────────────────────────────────────────────────────────
//
// Per-theater escalation snapshot. Each named theater is scored independently from
// the events assigned to it, producing a heat (0..1 weighted modality intensity), a
// discrete escalation rung, a trend vs the previous tick, and its dominant actors.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TheaterState {
    pub theater_id:       String,
    pub label:            String,
    pub rung:             EscalationRung,
    pub rung_label:       String,
    pub heat:             f64,                    // 0..1
    pub modality_scores:  HashMap<String, f64>,   // modality_id → 0..1
    pub trend:            String,                 // "rising" | "falling" | "stable"
    pub delta:            f64,
    pub event_count:      usize,
    pub gp_involved:      bool,
    pub alliance_invoked: bool,
    pub top_actors:       Vec<String>,
    /// The modality id (e.g. "nuclear_posture") contributing the most weighted heat
    /// to this theater — the "why" behind its level. Empty for a Stable theater.
    pub top_driver:       String,
    /// The modality id with the largest POSITIVE weighted change since the previous
    /// tick — the "why" behind this theater HEATING UP, as opposed to `top_driver`'s
    /// "why" behind its level. Populated only when the theater is rising; empty
    /// otherwise. Surfaces *where risk is concentrating, early enough to act*.
    pub rising_driver:    String,
    /// The SECOND-largest weighted contributor among the modalities the model
    /// considers *elevated* (raw score ≥ `ELEVATION_THRESHOLD`) — the second active
    /// force dimension, i.e. the co-occurrence story `top_driver` (a single dominant
    /// level) cannot tell. Empty unless at least two modalities are elevated, so it
    /// surfaces only when a flashpoint is genuinely multi-dimensional (which is what
    /// the intra-theater co-occurrence amplifier keys on).
    pub secondary_driver: String,
    /// True when this theater's displayed `heat` is being HELD UP by the persistence
    /// floor (a slowly-decaying remembered war-state) rather than fresh evidence —
    /// i.e. the floor strictly exceeds the current fast read. It marks a war the model
    /// is holding through a multi-day news gap (silence ≠ peace): the number is a
    /// memory of a sustained war, not a live measurement, and it releases on genuine
    /// de-escalation evidence. Honest by construction (`floor > fast_heat`), so the
    /// operator can tell a live-hot flashpoint from one held quiet. `#[serde(default)]`
    /// keeps older persisted snapshots loadable.
    #[serde(default)]
    pub held_by_floor:    bool,
    /// The escalation rung computed from the FRESH read alone (`fast_heat`, current
    /// evidence) rather than the displayed `heat` (which the persistence floor may be
    /// holding up). When `held_by_floor`, comparing this to `rung_label` shows the
    /// operator HOW FAR the live read has fallen below the held value — in the same
    /// rung vocabulary they already read ("held at Limited War · fresh evidence: Crisis").
    /// Equal to `rung_label` whenever the theater is not floor-held. Honest by
    /// construction (the model's own `rung_for(fast_heat, …)`). `#[serde(default)]`
    /// keeps older persisted snapshots loadable.
    #[serde(default)]
    pub fresh_rung_label: String,
    /// Escalation momentum: the recency-weighted mean signed `escalation_step` of this
    /// theater's recent events, in [−1, +1]. −1 = the news flow is dominated by
    /// de-escalation (ceasefires, talks, deals); +1 = dominated by escalatory moves;
    /// 0 = neutral / no qualifying coverage. This is the Goldstein-style
    /// conflict↔cooperation DIRECTION of the news flow — a LEADING signal distinct from
    /// `heat` (the magnitude of conflict) and `delta`/`trend` (the change in the heat
    /// SCORE): a theater can sit at high, flat heat while its momentum turns sharply
    /// negative (a peace process underway before heat decays) or positive (escalatory
    /// rhetoric building before it shows up as kinetic heat). It is the same
    /// recency-weighted mean the persistence floor already thresholds for de-escalation
    /// (`theater_is_deescalating`), surfaced as a magnitude rather than collapsed to a
    /// boolean. Honest by construction (the model's own weighted mean of an input it
    /// already ingests). `#[serde(default)]` keeps older persisted snapshots loadable.
    #[serde(default)]
    pub escalation_momentum: f64,
}

/// The label of the hottest theater that has risen above Stable — *where* the systemic
/// read is concentrated right now. Empty when every theater is Stable (a quiet world has
/// no lead). This is the SINGLE source of truth for the "lead theater", used by both the
/// durable timeline entry (`TimelineEntry::lead`, the historical baseline) and the live
/// 6h-trend payload (the current read), so a trend window can honestly report whether the
/// locus of risk RELOCATED — a shift the bare headline delta cannot show.
pub fn lead_theater(theaters: &[TheaterState]) -> String {
    theaters.iter()
        .filter(|t| t.rung != EscalationRung::Stable)
        .max_by(|a, b| a.heat.partial_cmp(&b.heat).unwrap_or(std::cmp::Ordering::Equal))
        .map(|t| t.label.clone())
        .unwrap_or_default()
}

/// Whether the systemic read is **pegged at the top of the model's dynamic range**: the
/// headline P(WWIII) is clamped at the forecast ceiling (`bayesian::is_at_forecast_ceiling`)
/// AND the trailing-window reads have shown *zero* empirical movement (`empirical_hw_pct == 0`,
/// i.e. every read in the window was identical). In that state a "6h Trend = +0.000%" is not a
/// calm flat line or a freeze/bug — the headline genuinely cannot move up because it is
/// hard-clamped at the engineering ceiling — so it must be surfaced as PEGGED. Pure honesty
/// layer: it names WHY the trend is flat; it never changes the math.
///
/// Keys on the HEADLINE ceiling, not a per-theater heat clamp. Since the heat de-saturation
/// (`theater::heat_from_scores` now ends in `1 − exp(−γ·raw)`, which asymptotes strictly below
/// 1.0) no theater's heat ever rails at a hard 1.0, so the former `max_heat >= 1.0` gate could
/// never fire and this caveat was silently dead — a frozen ceiling-pinned trend read as a calm
/// flat line. The genuine remaining peg is P at `FORECAST_PROB_CEILING`. Distinct from the hero
/// `at_ceiling` caveat (which fires on any capped read, including one that jumped INTO the
/// ceiling this window): pegged additionally requires the window to be empirically flat, so it
/// answers the trend cell's own question — is this +0.000% informative, or simply pinned at the
/// top?
///
/// `samples >= 2` guards against declaring "pegged" on a cold ring with no real window yet.
/// Mirrors the discipline of [`lead_theater`] — a small, unit-tested, single-source-of-truth
/// read off the snapshot that the durable payload and the browser both consume.
pub fn systemic_pegged(p_annual: f64, empirical_hw_pct: f64, samples: usize) -> bool {
    crate::bayesian::is_at_forecast_ceiling(p_annual) && empirical_hw_pct <= 0.0 && samples >= 2
}

// ── Systemic couplers (v2) ──────────────────────────────────────────────────────
//
// The factors that turn a regional war into a *world* war. These replace the flat
// regime-multiplier product as the conceptual driver of the systemic index: a
// regional crisis that couples to great powers, activates alliance chains, runs
// concurrently with other hot theaters, and has lost its guardrails is the systemic
// signature. (The operator-tunable regime multiplier still feeds the structural /
// guardrail component until settings migration in a later pass.)

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SystemicCouplers {
    pub gp_entanglement:     f64,   // 0..1 — distinct great powers active across hot theaters
    pub alliance_activation: f64,   // 0..1 — mutual-defense invocation signal
    pub concurrency:         f64,   // fractional count of simultaneously-hot theaters
    pub guardrail_collapse:  f64,   // 0..1 — arms-control / deterrence guardrails gone
    pub coupling_multiplier: f64,   // combined multiplier applied to the systemic likelihood
    // The coupling channel contributing the LARGEST multiplicative lift to the systemic
    // likelihood — the model's own answer to "what is turning this regional crisis into a
    // *world*-war risk right now": a single-theater nuclear brink, great-power
    // entanglement, multi-theater concurrency, or alliance activation. Empty when no
    // channel lifts above the floor (the risk is purely single-theater heat — regional,
    // not yet systemically coupled). A read-out of the same lifts that build
    // `coupling_multiplier`, never a new lever.
    #[serde(default)]
    pub coupling_driver:     String,
    // HONESTY flag — every BREADTH/coupling amplifier of the systemic likelihood is at its
    // structural rail (hottest theater's heat clamped at the model maximum, great-power
    // entanglement and alliance activation both maxed, ≥2 theaters hot) AND no single-theater
    // nuclear brink is live. In that state the read has run out of resolution to further
    // escalation of the crises already on the board — intensifying them cannot raise it; the
    // only remaining lever is a direct nuclear brink. Names the ceiling-saturation the
    // de-saturation backtest thread measured (`backtest::live_pegged_*`), so a ~83% breadth
    // peg below the 0.90 forecast ceiling can't read as a precise, still-climbing point
    // estimate. Computed purely from the rails — touches no fitted constant, never changes P.
    #[serde(default)]
    pub breadth_saturated:   bool,
    /// Systemic escalation momentum in [−1, +1] — the heat-weighted mean of the per-theater
    /// `escalation_momentum` (news-flow direction) across theaters above baseline. Each
    /// theater's momentum is weighted by its heat, so a calming backwater can't outvote a
    /// heating flashpoint and a quiet world (no theater above baseline) reads 0. Floor-held
    /// theaters are EXCLUDED from the weight: their displayed heat is a remembered war-state
    /// (memory, not live evidence) and their momentum is stale, so they must not dilute the live
    /// news-flow direction — a board of only silent, memory-held wars reads 0. Negative =
    /// the board's coverage is dominated by de-escalation (ceasefires, talks, deals);
    /// positive = by escalatory moves. This is the SINGLE systemic LEADING read — which way
    /// the whole picture is tilting RIGHT NOW — distinct from the headline `delta` (a LAGGING
    /// change in the already-realized P) and from `coupling_driver`/`breadth_saturated` (which
    /// describe the level, not its direction of travel): the news flow turns before the
    /// realized probability does. A heat-weighted mean of a gauge the model already computes —
    /// it is display/awareness only and NEVER feeds `l_sys` or P. `#[serde(default)]` keeps
    /// older persisted snapshots loadable.
    #[serde(default)]
    pub systemic_momentum:   f64,
}

// ── Modality sensitivity (load-bearing modality) ───────────────────────────────

/// Systemic modality-SENSITIVITY read: which of the five orthogonal modalities is
/// LOAD-BEARING for the headline right now, measured by leave-one-out. The engine
/// recomputes the systemic likelihood with each modality's evidence suppressed and maps
/// it back to P the same way; the modality whose removal drops the headline P the most is
/// the load-bearing one, and `p_drop_pp` is that drop in percentage points. This answers a
/// question no existing field does — `coupling_driver` names the dominant COUPLING channel
/// (brink/entanglement/breadth/alliance) and per-theater `top_driver` names each theater's
/// largest weighted modality, but neither says which KIND of force is holding up the
/// SYSTEMIC number, nor by how much. A pure diagnostic read-out of a counterfactual over the
/// already-scored board: it never feeds `l_sys`/P and touches no fitted constant. Empty /
/// `available = false` when the board is too quiet for any modality to carry a meaningful
/// share of the headline. `#[serde(default)]` keeps older persisted snapshots loadable.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModalitySensitivity {
    /// The load-bearing modality id (e.g. "nuclear_posture"). Empty when unavailable.
    pub modality:   String,
    /// Headline annual-P drop, in percentage points, if that modality's evidence vanished.
    pub p_drop_pp:  f64,
    /// Per-modality leave-one-out profile: (modality id, headline-P drop in pp), so the
    /// operator can see the full attribution, not just the top term. Sorted largest-first.
    #[serde(default)]
    pub profile:    Vec<(String, f64)>,
    /// SUPPORT BREADTH: the effective number of modalities the headline actually leans on —
    /// the participation ratio `(Σ dᵢ)² / Σ dᵢ²` of the leave-one-out drop vector. It says
    /// whether the load-bearing modality named above is the WHOLE story or just first among
    /// several: ≈1.0 means the number is single-sourced (fragile — that one channel vanishing
    /// collapses it), a larger value means it rests on many kinds of force at once (a broadly
    /// supported, more robust escalation). A quantity the single-leader read cannot carry: a
    /// 60% resting entirely on economic warfare and a 60% resting evenly across five modalities
    /// name the same leader but differ here. Zero-drop modalities contribute nothing, so the
    /// value is bounded by the count of modalities with real leverage. 0.0 when `available` is
    /// false (no leader carries the display floor). `#[serde(default)]` keeps older snapshots
    /// loadable. Diagnostic only — computed after P is final, never feeds `l_sys`/P.
    #[serde(default)]
    pub support_breadth: f64,
    /// False when no modality's removal moves the headline by the display floor — the read
    /// is diffuse / the board is quiet, so naming one load-bearing modality would overclaim.
    pub available:  bool,
}

// ── Theater sensitivity (load-bearing theater) ─────────────────────────────────

/// Systemic theater-SENSITIVITY read: which flashpoint is LOAD-BEARING for the headline
/// right now, measured by leave-one-out. The engine recomputes the systemic likelihood with
/// each theater REMOVED FROM THE BOARD and maps it back to P the same way; the theater whose
/// absence drops the headline P the most is the load-bearing one, and `p_drop_pp` is that drop
/// in percentage points. This answers WHERE the number is really coming from — a question the
/// existing fields do NOT answer. `driver` names the HOTTEST theater by raw heat, but the
/// couplers (concurrency, great-power entanglement, nuclear brink) are non-linear, so the
/// loudest theater is not always the highest-LEVERAGE one: a slightly cooler theater that is
/// the sole nuclear-brink or the sole second-great-power contributor can drop P more when it
/// leaves. `load_bearing_modality` names which KIND of force carries the read; this names which
/// PLACE — a distinct operator question (which flashpoint to de-escalate/watch, not which force
/// dimension). A pure diagnostic read-out of a counterfactual over the already-scored board: it
/// never feeds `l_sys`/P and touches no fitted constant. Empty / `available = false` when the
/// board is too quiet for any single theater to carry a meaningful share of the headline.
/// `#[serde(default)]` keeps older persisted snapshots loadable.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TheaterSensitivity {
    /// The load-bearing theater's human label (e.g. "US/Israel–Iran"). Empty when unavailable.
    pub theater:    String,
    /// The load-bearing theater's stable id (e.g. "us_israel_iran"), for keying. Empty when
    /// unavailable.
    pub theater_id: String,
    /// Headline annual-P drop, in percentage points, if that theater were absent from the board.
    pub p_drop_pp:  f64,
    /// Per-theater leave-one-out profile: (theater label, headline-P drop in pp), so the operator
    /// can see the full attribution, not just the top term. Sorted largest-first.
    #[serde(default)]
    pub profile:    Vec<(String, f64)>,
    /// False when no theater's removal moves the headline by the display floor — the read is
    /// diffuse / spread across theaters, so naming one load-bearing theater would overclaim.
    pub available:  bool,
}

// ── Memory load (headline carried by remembered war-state) ─────────────────────

/// How much of the headline is propped up by PERSISTENCE MEMORY rather than fresh evidence — the
/// quantitative form of `systemic_memory_held` (a bare bool: is the LEAD floor-held). The engine
/// recomputes the systemic likelihood scoring EVERY theater on its current evidence, IGNORING the
/// persistence floor that keeps a memory-hot theater's heat up through a news gap, and maps it back
/// to P the same way; `lift_pp` is how many percentage points the live headline sits ABOVE that
/// fresh-evidence read. A 60% earned by current fighting means something different from a 60%
/// coasting on remembered war-state during a coverage blackout, and no other field quantified that
/// gap: `systemic_memory_held` is only a yes/no about the lead theater. `held_count`/`held_theaters`
/// name the flashpoints doing the carrying. A pure diagnostic over the already-scored board: it
/// never feeds `l_sys`/P and touches no fitted constant. `available = false` (nothing floor-held →
/// the lift is honestly 0) hides the read. `#[serde(default)]` keeps older snapshots loadable.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryLoad {
    /// Headline annual-P lift, in percentage points, carried by memory above fresh evidence.
    pub lift_pp: f64,
    /// How many theaters on the board are currently floor-held (memory, not fresh evidence).
    pub held_count: usize,
    /// Their human labels, so the operator sees WHICH wars are being remembered.
    #[serde(default)]
    pub held_theaters: Vec<String>,
    /// False when no theater is floor-held — the headline rests entirely on fresh evidence, so
    /// there is no memory lift to surface.
    pub available: bool,
}

// ── Escalation coherence (is the number heating WHERE it rests, or elsewhere?) ──

/// Whether the board's escalation is building in the SAME theater the headline rests on
/// (coherent) or on a DIFFERENT, emerging front (divergent). Two things the operator already
/// sees separately — WHERE the number rests (`load_bearing_theater`, the leave-one-out
/// leverage leader) and WHERE the news flow is turning up (per-theater `escalation_momentum`,
/// the recency-weighted signed step) — but no field RELATED them, and the relation is the
/// decision: if the flashpoint holding up the number is ALSO the one escalating, the read is
/// likely to rise there and the operator watches that same place (coherent); if escalation is
/// decisively building in a theater OTHER than the load-bearing one, risk is accumulating on a
/// second front the headline does not yet rest on, and the operator has a NEW place to watch
/// (divergent). `systemic_momentum` gives only the heat-weighted board-wide DIRECTION; it cannot
/// say whether that momentum coincides with the leverage. A pure diagnostic over the
/// already-scored board: it never feeds `l_sys`/P and touches no fitted constant. `available =
/// false` (no load-bearing theater, or no theater is decisively escalating) hides the read.
/// `#[serde(default)]` keeps older persisted snapshots loadable.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EscalationCoherence {
    /// True when a load-bearing theater is named AND at least one theater is decisively
    /// escalating (its momentum clears the escalation mirror of the de-escalation floor gate).
    pub available: bool,
    /// True when the most-escalating theater IS the load-bearing one — the number is heating
    /// where it rests. False = divergent: escalation is building on a different front.
    pub coherent: bool,
    /// Human label of the most-escalating theater (the momentum leader). Empty when unavailable.
    pub momentum_theater: String,
    /// Stable id of the most-escalating theater, for keying. Empty when unavailable.
    pub momentum_theater_id: String,
    /// The momentum leader's escalation momentum in [−1, +1] (signed; positive = escalatory).
    pub momentum: f64,
}

// ── Escalation breadth (how many fronts are escalating AT ONCE?) ──

/// How many theaters are DECISIVELY ESCALATING simultaneously — the momentum-breadth of the board,
/// distinct from every read already shown. `couplers.concurrency` counts theaters that are HOT
/// (heat ≥ Crisis) and feeds P; `escalation_coherence` names only the single momentum LEADER and
/// relates it to the leverage leader; `systemic_momentum` is a heat-weighted board-wide DIRECTION
/// magnitude. None of them answer "is escalation isolated on one front, or building across several
/// at the same time" — and that distinction is decision-relevant: a synchronized multi-front
/// escalation is the historical signature of a systemic crisis (1914, 1938–39), materially more
/// dangerous than the same magnitude on a single contained front, and escalation-breadth can be
/// broad even when heat is concentrated (a cool theater turning up fast), or narrow when heat is
/// broad (many hot-but-stable standoffs). Counts theaters whose `escalation_momentum` STRICTLY clears
/// the escalation mirror of the de-escalation floor gate (`m > +0.30`, the true reflection of the
/// strict `m < -0.30` de-escalation gate — the same decisive bar, at the same strictness, that
/// `escalation_coherence` uses). A pure diagnostic over the already-scored board:
/// it never feeds `l_sys`/P and touches no fitted constant. `available = false` (nothing decisively
/// escalating) hides the read. `#[serde(default)]` keeps older persisted snapshots loadable.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EscalationBreadth {
    /// True when at least one theater is decisively escalating.
    pub available: bool,
    /// How many theaters are decisively escalating at the same time.
    pub count: usize,
    /// True when `count >= 2` — escalation is synchronized across multiple fronts, not isolated.
    pub multi_front: bool,
    /// The escalating fronts as (label, momentum), sorted by momentum descending, so the operator
    /// sees WHICH theaters are turning up together and how hard. Empty when unavailable.
    pub fronts: Vec<(String, f64)>,
}

// ── Risk snapshot ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskSnapshot {
    pub snapshot_id:    String,
    pub computed_at:    DateTime<Utc>,
    pub aggregation_window_hours: f64,

    pub historical_anchor:  f64,
    pub regime_multiplier:  f64,

    pub domain_scores:     HashMap<String, DomainScore>,
    pub elevated_domains:  usize,
    pub co_occurrence_boost: f64,

    pub likelihood_ratio:    f64,
    pub weighted_domain_sum: f64,

    pub p_wwiii_annual: f64,
    pub p_wwiii_30day:  f64,
    pub p_wwiii_90day:  f64,

    pub estimate_confidence: f64,

    /// Observation-coverage honesty (staleness): fraction of the aggregation window's
    /// time-buckets that actually saw live signal (1.0 = continuously observed), age of
    /// the newest live event, the multiplicative discount those two applied to
    /// `estimate_confidence` (and every per-modality confidence), and the operator
    /// caveat flag. Set in `compute` Step 9 (bayesian::observation_coverage);
    /// DISPLAY-ONLY — never feeds P. `#[serde(default)]` keeps older persisted
    /// snapshots loadable (they read 0/false and claim no gap).
    #[serde(default)]
    pub window_coverage: f64,
    #[serde(default)]
    pub newest_event_age_secs: i64,
    #[serde(default)]
    pub observation_factor: f64,
    #[serde(default)]
    pub observation_gap: bool,

    pub alert_level:   AlertLevel,
    pub alert_message: String,
    /// The annual-P(WWIII) alert-band thresholds that classified THIS snapshot,
    /// recorded so the JSON is self-describing and the dashboard can draw/colour
    /// the critical & elevated bands from the live configured value instead of a
    /// hardcoded literal that could silently drift from `AlertSettings`. Set by
    /// the engine from its configured thresholds in `compute` (Step 10).
    #[serde(default)]
    pub alert_elevated_threshold: f64,
    #[serde(default)]
    pub alert_critical_threshold: f64,

    pub events_in_window:  usize,
    pub sources_active:    usize,
    pub great_power_events: usize,
    pub regions_active:    Vec<String>,
    pub top_actors:        Vec<String>,

    pub delta_annual: f64,
    pub delta_30day:  f64,

    // ── v2 systemic layer ──
    /// 0..100 legible headline index (Doomsday-Clock / DEFCON style), derived from
    /// the systemic likelihood. The new public hero metric; p_wwiii_* is secondary.
    #[serde(default)]
    pub systemic_index: f64,
    /// Per-theater escalation states (NATO–Russia, US/Israel–Iran, …).
    #[serde(default)]
    pub theaters: Vec<TheaterState>,
    /// The couplers that drive systemic escalation.
    #[serde(default)]
    pub couplers: SystemicCouplers,
    /// Human-readable driver string, e.g. "US/Israel–Iran at Great-Power War; 2 theaters hot".
    #[serde(default)]
    pub driver: String,

    /// Which modality is LOAD-BEARING for the headline (leave-one-out sensitivity) — the
    /// systemic "which kind of force is holding up this number, and by how much". Computed in
    /// `compute` AFTER P is final, purely from the already-scored board; never feeds P.
    #[serde(default)]
    pub load_bearing_modality: ModalitySensitivity,

    /// Which THEATER is LOAD-BEARING for the headline (leave-one-out sensitivity) — the
    /// systemic "which flashpoint is holding up this number, and by how much". Computed in
    /// `compute` AFTER P is final, purely from the already-scored board; never feeds P.
    #[serde(default)]
    pub load_bearing_theater: TheaterSensitivity,

    /// How much of the headline is carried by persistence MEMORY vs. fresh evidence — the
    /// quantitative form of `systemic_memory_held`. Computed in `compute` AFTER P is final,
    /// purely from the already-scored board; never feeds P.
    #[serde(default)]
    pub memory_load: MemoryLoad,

    /// Whether escalation is building in the SAME theater the headline rests on (coherent) or
    /// on a DIFFERENT, emerging front (divergent) — the relation between the leverage leader
    /// (`load_bearing_theater`) and the momentum leader (per-theater `escalation_momentum`).
    /// Computed in `compute` AFTER P is final, purely from the already-scored board; never feeds P.
    #[serde(default)]
    pub escalation_coherence: EscalationCoherence,

    /// How many theaters are decisively escalating AT ONCE (momentum-breadth of the board) —
    /// isolated single-front escalation vs. a synchronized multi-front one. Distinct from
    /// `couplers.concurrency` (HOT-theater count, which feeds P) and from `escalation_coherence`
    /// (single momentum leader). Computed in `compute` AFTER P is final, purely from the
    /// already-scored board; never feeds P.
    #[serde(default)]
    pub escalation_breadth: EscalationBreadth,

    /// True when the seismic monitor holds a live event the detector judges
    /// consistent with a nuclear test — near a known test site AND past the
    /// natural-earthquake discriminator (no aftershock sequence, or a CTBTO
    /// statement). A DISPLAY flag only: it surfaces the detector's own
    /// `SeismicAlert::is_test_consistent` conclusion onto the I&W board so the
    /// strongest physical nuclear indicator is on the consolidated warning board,
    /// not just the standalone banner. It does NOT enter the P(WWIII) math — set
    /// in the aggregator AFTER `compute`, so backtests are bit-identical.
    #[serde(default)]
    pub seismic_test_consistent: bool,
    /// The test site behind that determination (e.g. "Punggye-ri") — the WHERE for
    /// the I&W seismic light. Empty when no test-consistent event is live.
    #[serde(default)]
    pub seismic_site: String,

    /// Movement attribution for THIS tick: when the headline moved materially this
    /// snapshot (|Δ| ≥ aggregator::DRIVER_NOTE_MIN_DELTA), the top new events (plus
    /// corroboration/decay notes) that landed with it — "source · title" strings the
    /// timeline chart shows on hover, so a knock on the graph can answer WHY it moved.
    /// Set in the aggregator run loop AFTER compute (the `seismic_test_consistent`
    /// pattern), so it never touches the P(WWIII) math and backtests are bit-identical.
    /// Empty on immaterial ticks.
    #[serde(default)]
    pub tick_drivers: Vec<String>,

    /// Structured twins of `tick_drivers` — the same top events with url/age/snippet
    /// so the chart's hover mini-card can link straight to the underlying article or
    /// video for audit. Same lifecycle and guarantees: set AFTER compute, display-only,
    /// never feeds P, empty on immaterial ticks. Note strings and refs are built from
    /// the same events but refs skip the "+N corroborations"/decay NOTE lines.
    #[serde(default)]
    pub tick_driver_refs: Vec<DriverRef>,
}

impl Default for RiskSnapshot {
    fn default() -> Self {
        Self {
            snapshot_id: Uuid::new_v4().to_string(),
            computed_at: Utc::now(),
            aggregation_window_hours: 72.0,
            historical_anchor: HISTORICAL_ANCHOR,
            regime_multiplier: 1.0,
            domain_scores: HashMap::new(),
            elevated_domains: 0,
            co_occurrence_boost: 1.0,
            likelihood_ratio: 0.0,
            weighted_domain_sum: 0.0,
            p_wwiii_annual: 0.0,
            p_wwiii_30day: 0.0,
            p_wwiii_90day: 0.0,
            estimate_confidence: 0.5,
            // Cold seed: NOTHING has been observed yet, and the seed must say so — a
            // pre-first-compute snapshot claiming full coverage would be the same lie
            // the 2026-07-17 outage exposed, one tick earlier.
            window_coverage: 0.0,
            newest_event_age_secs: 0,
            observation_factor: 0.0,
            observation_gap: true,
            alert_level: AlertLevel::Normal,
            alert_message: String::new(),
            alert_elevated_threshold: AlertSettings::default().elevated,
            alert_critical_threshold: AlertSettings::default().critical,
            events_in_window: 0,
            sources_active: 0,
            great_power_events: 0,
            regions_active: vec![],
            top_actors: vec![],
            delta_annual: 0.0,
            delta_30day: 0.0,
            systemic_index: 0.0,
            theaters: vec![],
            couplers: SystemicCouplers::default(),
            driver: String::new(),
            load_bearing_modality: ModalitySensitivity::default(),
            load_bearing_theater: TheaterSensitivity::default(),
            memory_load: MemoryLoad::default(),
            escalation_coherence: EscalationCoherence::default(),
            escalation_breadth: EscalationBreadth::default(),
            seismic_test_consistent: false,
            seismic_site: String::new(),
            tick_drivers: vec![],
            tick_driver_refs: vec![],
        }
    }
}

// ── Driver reference ──────────────────────────────────────────────────────────

/// One movement-attribution driver in structured form — the audit trail behind a
/// timeline knock. `tick_drivers` keeps the compact "source · title" strings; this
/// carries what the hover mini-card needs to be CLICKABLE (url) and self-explanatory
/// (published_at → age, snippet). Resolved from the ArticleStore via the event's
/// raw_article_id at driver-note time; url/snippet may be empty when the article is
/// already evicted or suppressed — the card must degrade to an unlinked text row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriverRef {
    pub source:       String,
    pub title:        String,
    #[serde(default)]
    pub url:          String,
    #[serde(default)]
    pub published_at: String,
    #[serde(default)]
    pub snippet:      String,
    /// True for watchlist video rows (source `-video`) — the card badges these ▶.
    #[serde(default)]
    pub video:        bool,
    /// Key of this source page's captured thumbnail, served at `/api/thumb/<thumb>` and drawn
    /// in the hover card as a mini pageview. Empty until (or unless) a capture lands — the card
    /// falls back to its text rows. Display-only; never feeds P.
    #[serde(default)]
    pub thumb:        String,
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
    /// Lead theater label at this tick (`lead_theater`) — the WHERE, stored in the durable
    /// ring so the 6h-trend window can report whether the locus of risk shifted. Empty in a
    /// quiet world. `#[serde(default)]` keeps older persisted entries (pre-field) loadable.
    #[serde(default)]
    pub lead:      String,
    /// Systemic escalation momentum at this tick (`couplers.systemic_momentum` ∈ [−1,+1]) —
    /// stored in the durable ring so the lead-lag diagnostic can measure whether this
    /// "leading" gauge actually PRECEDES the realized headline P over history, rather than
    /// merely asserting it does. `#[serde(default)]` keeps older persisted entries (pre-field)
    /// loadable — they read momentum 0 and are simply not counted as decisive samples.
    #[serde(default)]
    pub mom:       f64,
    /// Movement attribution (`snap.tick_drivers`) — WHY this tick moved, rendered by
    /// the chart's hover. Skipped when empty so the 350k-entry ring and the JSONL
    /// archive stay lean (the overwhelming majority of 1 Hz ticks are immaterial);
    /// `default` keeps older persisted entries loadable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub drivers:   Vec<String>,
    /// Structured twin of `drivers` (`snap.tick_driver_refs`) — url/age/snippet for
    /// the clickable hover card. Same skip-when-empty ring/archive discipline;
    /// `default` keeps every pre-field persisted entry loadable (unlinked text rows).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub driver_refs: Vec<DriverRef>,
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
            lead:     lead_theater(&snap.theaters),
            mom:      (snap.couplers.systemic_momentum * 1e3).round() / 1e3,
            drivers:  snap.tick_drivers.clone(),
            driver_refs: snap.tick_driver_refs.clone(),
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

// deny_unknown_fields: a typo'd / unknown config key now becomes a parse ERROR that
// load_settings surfaces (it logs "Config parse error … using defaults"), instead of being
// silently dropped — which would leave that setting on its built-in default with no warning.
// Verified safe against the committed settings.yml by real_settings_yml_parses_*. (audit ops-4)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct LlmSettings {
    #[serde(default)]
    pub enabled:      bool,
    #[serde(default = "LlmSettings::default_endpoint")]
    pub endpoint:     String,
    #[serde(default = "LlmSettings::default_model")]
    pub model:        String,
    /// Embedding model for semantic dedup (v2 Phase 4).
    #[serde(default = "LlmSettings::default_embed_model")]
    pub embed_model:  String,
    /// Enable embedding-based semantic dedup (catches same-event paraphrases the
    /// MinHash title dedup misses). Off by default — it adds one embeddings call per
    /// LLM-gated article on top of extraction. Requires the embed_model pulled.
    #[serde(default)]
    pub semantic_dedup: bool,
    /// Number of concurrent LLM extraction calls (worker-pool size). Higher values
    /// use more of the machine's GPU/CPU; match it to Ollama's OLLAMA_NUM_PARALLEL.
    #[serde(default = "LlmSettings::default_concurrency")]
    pub concurrency: usize,
    #[serde(default = "LlmSettings::default_timeout")]
    pub timeout_secs: u64,
}

impl LlmSettings {
    fn default_endpoint()    -> String { "http://localhost:11434".into() }
    fn default_model()       -> String { "qwen2.5:7b".into() }
    fn default_embed_model() -> String { "nomic-embed-text".into() }
    // Match the hand-tuned hardware-safe values in settings.yml (GTX-1080 VRAM cap). A code
    // default must never be MORE aggressive than the calibrated config — otherwise a missing
    // key silently falls back to a value that can OOM the GPU. (audit ops-3)
    fn default_concurrency() -> usize  { 2 }
    fn default_timeout()     -> u64    { 20 }
}

impl Default for LlmSettings {
    fn default() -> Self {
        Self {
            enabled:        false,
            endpoint:       Self::default_endpoint(),
            model:          Self::default_model(),
            embed_model:    Self::default_embed_model(),
            semantic_dedup: false,
            concurrency:    Self::default_concurrency(),
            timeout_secs:   Self::default_timeout(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
    fn timeline_entry_carries_the_tick_driver_attribution_leanly() {
        // The chart's "why did it knock here" hover rests on this plumbing: a snapshot's
        // tick_drivers ride into TimelineEntry.drivers verbatim — and an IMMATERIAL tick
        // (empty drivers) serializes WITHOUT the key at all, so the 350k-entry ring and
        // the JSONL archive don't grow by an empty array per 1 Hz tick.
        let snap = RiskSnapshot {
            tick_drivers: vec!["reuters · US launches strikes".into(), "+3 corroborations".into()],
            tick_driver_refs: vec![DriverRef {
                source: "reuters".into(), title: "US launches strikes".into(),
                url: "https://example.com/a".into(), published_at: "2026-07-22T01:00:00Z".into(),
                snippet: "Strikes began at…".into(), video: false, thumb: "deadbeef".into(),
            }],
            ..Default::default()
        };
        let entry = TimelineEntry::from_snapshot(&snap);
        assert_eq!(entry.drivers, snap.tick_drivers, "drivers must ride the entry verbatim");
        assert_eq!(entry.driver_refs.len(), 1, "structured refs must ride alongside");
        let with = serde_json::to_string(&entry).unwrap();
        assert!(with.contains("\"drivers\""), "material tick must serialize its drivers");
        assert!(with.contains("\"driver_refs\"") && with.contains("https://example.com/a"),
            "material tick must serialize its clickable refs");
        let quiet = TimelineEntry::from_snapshot(&RiskSnapshot::default());
        let without = serde_json::to_string(&quiet).unwrap();
        assert!(!without.contains("\"drivers\""),
            "an immaterial tick must skip the key entirely (ring/archive leanness)");
        assert!(!without.contains("\"driver_refs\""),
            "refs must obey the same skip-when-empty discipline");
        // And an OLD persisted line (pre-field) still loads, reading no drivers/refs.
        let old: TimelineEntry = serde_json::from_str(
            r#"{"t":"2026-07-01T00:00:00Z","p_annual":0.5,"p_30day":0.05,"alert":"normal",
                "elevated":0,"regime":1.0,"events":10,"delta":0.0}"#).unwrap();
        assert!(old.drivers.is_empty());
        assert!(old.driver_refs.is_empty(), "pre-field archive lines must read empty refs");
    }

    #[test]
    fn real_settings_yml_parses_with_deny_unknown_fields() {
        // The committed settings.yml must deserialize cleanly under deny_unknown_fields — this
        // both proves the attribute is safe for the live config AND guards against a future
        // stray/typo'd key being silently dropped to a (worse) default. (audit ops-4)
        let yml = include_str!("../settings.yml");
        let s: Settings = serde_yaml::from_str(yml).expect("settings.yml must parse");
        assert!(!s.regime_factors.is_empty(), "regime_factors should be populated");
        // The hardware-safe LLM values are present (and the code defaults now match them).
        assert_eq!(s.llm.concurrency, LlmSettings::default_concurrency());
        assert_eq!(s.llm.timeout_secs, LlmSettings::default_timeout());
    }

    fn theater_st(label: &str, rung: EscalationRung, heat: f64) -> TheaterState {
        TheaterState {
            theater_id: label.into(), label: label.into(), rung,
            rung_label: rung.label().into(), heat,
            modality_scores: std::collections::HashMap::new(),
            trend: "stable".into(), delta: 0.0, event_count: 0, gp_involved: false,
            alliance_invoked: false, top_actors: vec![], top_driver: String::new(),
            rising_driver: String::new(), secondary_driver: String::new(),
            held_by_floor: false, fresh_rung_label: rung.label().into(),
            escalation_momentum: 0.0,
        }
    }

    #[test]
    fn lead_theater_is_the_hottest_non_stable_theater() {
        // The lead is WHERE the systemic read concentrates: the hottest theater that has
        // risen above Stable. A quiet world (all Stable) has no lead, even if one theater
        // has marginally more (sub-threshold) heat — otherwise the trend would name a "lead"
        // for noise. The single source of truth shared by the timeline ring and the live
        // 6h-trend payload, so the historical and current reads are judged identically.
        assert_eq!(lead_theater(&[]), "");
        // All Stable → no lead, regardless of relative heat.
        let quiet = [
            theater_st("NATO-Russia", EscalationRung::Stable, 0.05),
            theater_st("China-Taiwan", EscalationRung::Stable, 0.04),
        ];
        assert_eq!(lead_theater(&quiet), "");
        // Hottest non-Stable wins (Crisis at higher heat beats a cooler Tension).
        let hot = [
            theater_st("NATO-Russia", EscalationRung::Tension, 0.15),
            theater_st("US/Israel-Iran", EscalationRung::Crisis, 0.40),
            theater_st("China-Taiwan", EscalationRung::Stable, 0.30), // Stable excluded even if "hot"
        ];
        assert_eq!(lead_theater(&hot), "US/Israel-Iran");
    }

    #[test]
    fn systemic_pegged_only_when_railed_and_flat() {
        // Pegged = the headline P is clamped at the forecast ceiling AND zero empirical
        // movement over the window. This is the honest "the read is pinned at the top" state
        // behind a frozen +0.000% trend — distinct from a genuinely calm or still-warming
        // world. Keys on the HEADLINE ceiling, not a per-theater heat clamp: post-de-saturation
        // no theater's heat rails at 1.0, so a heat-based gate would be permanently dead (the
        // exact regression this repairs). A revert to the old `&[TheaterState]`/`max_heat >= 1.0`
        // signature no longer compiles against these calls.
        use crate::bayesian::is_at_forecast_ceiling;
        // Precondition: the ceiling value registers as capped (single source of truth).
        assert!(is_at_forecast_ceiling(FORECAST_PROB_CEILING));
        // Clamped + flat + a real window → pegged.
        assert!(systemic_pegged(FORECAST_PROB_CEILING, 0.0, 21505));
        // Clamped but the read is still MOVING (empirical spread > 0) → not pegged: the trend
        // is informative even at the ceiling (it just climbed into it this window).
        assert!(!systemic_pegged(FORECAST_PROB_CEILING, 0.42, 21505));
        // Flat but the headline is BELOW the ceiling → a genuine plateau with headroom,
        // not a ceiling peg.
        assert!(!systemic_pegged(FORECAST_PROB_CEILING - 0.05, 0.0, 21505));
        // Cold ring (no real window yet) must never read as pegged.
        assert!(!systemic_pegged(FORECAST_PROB_CEILING, 0.0, 1));
    }

    #[test]
    fn timeline_entry_records_the_lead_theater() {
        // The durable ring entry must carry the lead, so a later trend window can read the
        // window's STARTING locus of risk (`lead_then`) off the persisted history.
        let snap = RiskSnapshot {
            theaters: vec![
                theater_st("NATO-Russia", EscalationRung::Tension, 0.15),
                theater_st("US/Israel-Iran", EscalationRung::Crisis, 0.40),
            ],
            ..RiskSnapshot::default()
        };
        assert_eq!(TimelineEntry::from_snapshot(&snap).lead, "US/Israel-Iran");
        // A quiet snapshot records an empty lead (round-trips through serde default).
        assert_eq!(TimelineEntry::from_snapshot(&RiskSnapshot::default()).lead, "");
    }

    #[test]
    fn timeline_entry_records_systemic_momentum_for_the_lead_lag_diagnostic() {
        // The durable ring must carry `mom` so EpochStore::momentum_lead_lag can MEASURE whether
        // momentum leads P, instead of the dashboard merely asserting it does.
        let mut snap = RiskSnapshot::default();
        snap.couplers.systemic_momentum = 0.4237;
        let e = TimelineEntry::from_snapshot(&snap);
        assert!((e.mom - 0.424).abs() < 1e-9, "mom rounded to 1e-3: {}", e.mom);
        // Older persisted entries predate the field → serde default 0.0, still loadable.
        let old: TimelineEntry =
            serde_json::from_str(r#"{"t":"2026-01-01T00:00:00Z","p_annual":0.3,"p_30day":0.01,"alert":"guarded","elevated":0,"regime":1.0,"events":0,"delta":0.0}"#).unwrap();
        assert_eq!(old.mom, 0.0);
    }

    #[test]
    fn baseline_annual_is_modern_not_2_over_2026() {
        // v2: the 2/2026 frequentist anchor was removed. The baseline is now a
        // modern quiet-year prior, and the old name aliases the new constant.
        assert!((BASELINE_ANNUAL - 0.015).abs() < 1e-9);
        assert_eq!(HISTORICAL_ANCHOR, BASELINE_ANNUAL);
        assert!((BASELINE_ANNUAL - 2.0 / 2026.0).abs() > 1e-4, "must no longer equal 2/2026");
    }

    #[test]
    fn baseline_annual_plausible_range() {
        // A modern quiet-year systemic-war prior: well under 5%, well over the old
        // sub-0.1% floor that pinned the model.
        const { assert!(BASELINE_ANNUAL > 0.001 && BASELINE_ANNUAL < 0.05) };
    }

    #[test]
    fn theater_of_resolves_known_dyads() {
        assert_eq!(theater_of(&["russia".into(), "ukraine".into()], Some("europe_eurasia")), Theater::NatoRussia);
        assert_eq!(theater_of(&["iran".into(), "israel".into()], Some("middle_east")), Theater::UsIran);
        assert_eq!(theater_of(&["china".into(), "taiwan".into()], Some("asia_pacific")), Theater::UsChinaTaiwan);
        assert_eq!(theater_of(&["india".into(), "pakistan".into()], Some("south_asia")), Theater::IndiaPakistan);
        assert_eq!(theater_of(&["north_korea".into()], None), Theater::Korea);
    }

    #[test]
    fn theater_of_unknown_is_other() {
        assert_eq!(theater_of(&["brazil".into()], Some("latin_america")), Theater::Other);
        assert_eq!(theater_of(&[], None), Theater::Other);
    }

    #[test]
    fn china_india_clash_routes_to_its_own_theater_not_taiwan_or_kashmir() {
        // A China–India border clash (both nuclear great powers) has its own dyad. It must NOT be
        // absorbed into a named flashpoint it is not about: without the guard the count+region
        // tiebreak routes it to Taiwan (region asia_pacific) or Kashmir (region south_asia),
        // fabricating heat in the wrong theater. It now resolves to the dedicated ChinaIndia
        // theater — visible as its own flashpoint, even when the region tag points elsewhere.
        assert_eq!(theater_of(&["china".into(), "india".into()], Some("asia_pacific")), Theater::ChinaIndia);
        assert_eq!(theater_of(&["china".into(), "india".into()], Some("south_asia")), Theater::ChinaIndia);
        assert_eq!(theater_of(&["china_military".into(), "india".into()], None), Theater::ChinaIndia);
        // The guard is NARROW: when the genuine dyad partner is present it does not fire —
        // china+taiwan stays a Taiwan story, india+pakistan stays Kashmir.
        assert_eq!(theater_of(&["china".into(), "india".into(), "taiwan".into()], Some("asia_pacific")), Theater::UsChinaTaiwan);
        assert_eq!(theater_of(&["china".into(), "india".into(), "pakistan".into()], Some("south_asia")), Theater::IndiaPakistan);
    }

    #[test]
    fn escalation_rung_levels_ordered() {
        assert_eq!(EscalationRung::Stable.level(), 0);
        assert_eq!(EscalationRung::Systemic.level(), 5);
        assert!(EscalationRung::LimitedWar.level() > EscalationRung::Crisis.level());
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
        assert_eq!(normalize_actor("houthi"),                 "houthis"); // singular pattern, same id
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
        let snap = RiskSnapshot {
            p_wwiii_annual: 0.123456789012,
            p_wwiii_30day:  0.010999,
            ..Default::default()
        };
        let entry = TimelineEntry::from_snapshot(&snap);
        // Should be rounded to 8 decimal places
        assert!((entry.p_annual - 0.12345679).abs() < 1e-7);
    }

    #[test]
    fn modality_ids_are_five_orthogonal_axes() {
        assert_eq!(DOMAIN_IDS.len(), 5);
        assert!(DOMAIN_IDS.contains(&"nuclear_posture"));
        assert!(DOMAIN_IDS.contains(&"military_escalation"));
        // The three non-orthogonal axes were dropped (now couplers / a rung).
        assert!(!DOMAIN_IDS.contains(&"wmd_mass_casualty"));
        assert!(!DOMAIN_IDS.contains(&"great_power_conflict"));
        assert!(!DOMAIN_IDS.contains(&"alliance_activation"));
    }

    #[test]
    fn alert_level_display() {
        assert_eq!(AlertLevel::Normal.to_string(),   "normal");
        assert_eq!(AlertLevel::Elevated.to_string(), "elevated");
        assert_eq!(AlertLevel::Critical.to_string(), "critical");
    }
}
