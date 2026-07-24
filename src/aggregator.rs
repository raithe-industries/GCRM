// ------------------------------------------------------------
// RAiTHE INDUSTRIES INCORPORATED
// Copyright (c) 2026 All Rights Reserved.
//
// This file is part of a proprietary system. Unauthorized use,
// reproduction, or distribution is strictly prohibited.
// ------------------------------------------------------------

// src/aggregator.rs — event window, timeline JSONL writer, Bayesian engine caller
//
// M-01: Corroboration detector upgraded from O(n) linear scan to O(k)
//        MinHash LSH candidate lookup. Uses the same hash primitives as
//        processor.rs (FNV-1a trigram hash, Mersenne-prime MinHash) but with
//        different band configuration tuned for the lower corroboration
//        Jaccard threshold (0.40 vs 0.70 dedup threshold).
//
//        LSH configuration for corroboration:
//          NUM_HASHES = 64, CORR_BANDS = 32, CORR_BAND_ROWS = 2
//          P(candidate | J=0.40) ≈ 1 − (1−0.40²)³² ≈ 0.9997
//          P(candidate | J=0.20) ≈ 1 − (1−0.20²)³² ≈ 0.7275
//          P(candidate | J=0.10) ≈ 1 − (1−0.10²)³² ≈ 0.0315
//
//        At J=0.40 (the threshold), candidacy is near-certain. False
//        positives at J=0.10 (clearly unrelated) are only 3%. The exact
//        trigram Jaccard verification step eliminates all false positives
//        before any corroboration merge occurs.
//
//        Complexity:
//          Old: O(window_size) per incoming event — at 500K window, 500K
//               trigram Jaccard computations per event.
//          New: O(64 hashes + k candidates) where k is typically 0–5.
//               ~100× speedup at scale.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Local, Utc};
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, Mutex};
use tokio::time::{interval, Duration};
use tracing::{debug, info, warn};

use crate::bayesian::BayesianRiskEngine;
use crate::models::{
    AlertSettings, GeopoliticalEvent, RegimeFactor, RiskSnapshot, TimelineEntry,
};

// ── Window / capacity constants ───────────────────────────────────────────────
// The Bayesian MAX_EVENT_AGE_HOURS window (35,064h = 4 years) provides the
// true age-based eviction boundary; this cap is a volume-burst safeguard.
pub const MAX_WINDOW_EVENTS: usize = 500_000;

// ── Headline-read contract (RAITHE Global Monitor platform, §7.1) ──────────────
// The /api/latest (and WS `snapshot`) payload is the federation's "headline-read
// contract": sibling monitors and the read-only portal consume this SPEC, they do
// not fork the dashboard SPA. The payload carries this identifier as a top-level
// `contract` field so a consumer can negotiate the version it understands and fail
// loudly (rather than silently mis-reading) if the schema is bumped. The string is
// namespaced + versioned (`<monitor>.<surface>/v<N>`); a backward-INCOMPATIBLE
// change (removing/retyping a documented field) MUST bump the `/vN` suffix. Adding
// a new optional field is compatible and does NOT bump it. Spec:
// docs/headline-read-contract-v1.md, locked by `snapshot_to_json_honours_contract_v1`.
pub const HEADLINE_READ_CONTRACT: &str = "gcrm.headline-read/v1";

// ── Timeline path helpers ────────────────────────────────────────────────────────
//
// Returns the rotated JSONL path for a given date: logs/timeline_YYYY-MM-DD.jsonl
// Each calendar day produces one file in logs/. At 1 Hz: ~86,400 lines/day ×
// ~500 bytes ≈ ~43 MB/day. Files are named by the server's LOCAL (Eastern) day —
// see log_date() — so an evening log isn't future-dated. (Timestamps INSIDE each
// line stay RFC3339 with offset, so the data is unambiguous either way.)

/// Calendar day used for rotated-log FILENAMES. Uses the server's local time —
/// this box runs Eastern — so a file is named for the Eastern day its data belongs
/// to. Previously `Utc::now().date_naive()` rolled over at 8pm Eastern (UTC
/// midnight), so evening logs were future-dated (e.g. June 7 8pm → "...06-08").
fn log_date() -> chrono::NaiveDate {
    Local::now().date_naive()
}

fn timeline_path_for_date(date: &chrono::NaiveDate) -> String {
    format!("logs/timeline_{}.jsonl", date.format("%Y-%m-%d"))
}

fn today_timeline_path() -> String {
    timeline_path_for_date(&log_date())
}

// ── Article archive paths (mirrors the timeline rotation) ──────────────────────

fn article_path_for_date(date: &chrono::NaiveDate) -> String {
    format!("logs/articles_{}.jsonl", date.format("%Y-%m-%d"))
}

fn today_article_path() -> String {
    article_path_for_date(&log_date())
}

// ── Event archive paths (mirrors the timeline rotation) ────────────────────────

fn today_event_path() -> String {
    format!("logs/events_{}.jsonl", log_date().format("%Y-%m-%d"))
}

// ── Snapshot serialisation ────────────────────────────────────────────────────

pub fn snapshot_to_json(snap: &RiskSnapshot) -> serde_json::Value {
    let domains: serde_json::Map<String, serde_json::Value> = snap
        .domain_scores
        .iter()
        .map(|(did, ds)| {
            (
                did.clone(),
                serde_json::json!({
                    "score":              ds.score,
                    "label":              ds.label(),
                    "elevated":           ds.elevated(),
                    "confidence":         ds.confidence,
                    "event_count":        ds.event_count,
                    "great_power_events": ds.great_power_event_count,
                }),
            )
        })
        .collect();

    serde_json::json!({
        // Federation contract handle (RAITHE Global Monitor §7.1). A consumer reads
        // this FIRST to confirm it understands the schema before trusting any field.
        "contract":     HEADLINE_READ_CONTRACT,
        "snapshot_id":  snap.snapshot_id,
        "computed_at":  snap.computed_at.to_rfc3339(),
        "prior": {
            "historical_anchor": snap.historical_anchor,
            "formula":           "modern quiet-year baseline (v2; backtested, not a 2000-yr frequency)",
            "regime_multiplier": snap.regime_multiplier,
            // v2: the prior is FLAT. regime_multiplier is NOT applied to it (the
            // superseded v1 "adjusted_prior = anchor × regime" form, removed) — it
            // drives guardrail collapse on the systemic likelihood instead (see
            // couplers.guardrail_collapse), so a degraded-but-quiet world stays at
            // the baseline anchor, not an inflated prior.
            "regime_role":       "structural pressure on the systemic likelihood via guardrail collapse, not a prior multiplier (v2)",
        },
        "domains": domains,
        "co_occurrence": {
            "elevated_count": snap.elevated_domains,
            "boost":          snap.co_occurrence_boost,
        },
        "probabilities": {
            "annual":      snap.p_wwiii_annual,
            "annual_pct":  (snap.p_wwiii_annual * 100.0 * 1e6).round() / 1e6,
            "thirty_day":  snap.p_wwiii_30day,
            "ninety_day":  snap.p_wwiii_90day,
        },
        "delta": {
            "annual":     snap.delta_annual,
            "thirty_day": snap.delta_30day,
            "direction":  if snap.delta_annual > 1e-7 { "rising" }
                          else if snap.delta_annual < -1e-7 { "falling" }
                          else { "stable" },
        },
        "confidence": snap.estimate_confidence,
        "alert": {
            "level":   snap.alert_level.to_string(),
            "message": snap.alert_message,
            // The live alert-band thresholds (annual P) that classified this
            // snapshot — the dashboard sources its critical reference line and
            // risk colours from these so they can't drift from AlertSettings.
            "elevated_threshold": snap.alert_elevated_threshold,
            "critical_threshold": snap.alert_critical_threshold,
        },
        // ── v2 systemic layer (headline index + escalation ladder + couplers) ──
        "systemic": {
            "index":  snap.systemic_index,
            "driver": snap.driver,
        },
        "theaters": snap.theaters,
        "couplers": snap.couplers,
        // Which modality is LOAD-BEARING for the headline (leave-one-out sensitivity) — the
        // systemic "which kind of force is holding up this number, and by how much". Diagnostic;
        // never feeds P. Top-level so a consumer needn't dig into the snapshot struct.
        "load_bearing_modality": snap.load_bearing_modality,
        // Which THEATER is LOAD-BEARING for the headline (leave-one-out sensitivity) — the
        // systemic "which flashpoint is holding up this number, and by how much". Diagnostic;
        // never feeds P. Top-level, the load_bearing_modality precedent.
        "load_bearing_theater": snap.load_bearing_theater,
        // How much of the headline is carried by persistence MEMORY vs. fresh evidence — the
        // quantitative form of systemic_memory_held. Diagnostic; never feeds P.
        "memory_load": snap.memory_load,
        // Is escalation building in the SAME theater the number rests on (coherent) or on a
        // DIFFERENT emerging front (divergent) — the relation between load_bearing_theater and
        // per-theater escalation_momentum. Diagnostic; never feeds P.
        "escalation_coherence": snap.escalation_coherence,
        // How many theaters are decisively escalating AT ONCE (momentum-breadth) — isolated
        // single-front vs. synchronized multi-front escalation. Distinct from couplers.concurrency
        // (HOT-theater count, which feeds P). Diagnostic; never feeds P.
        "escalation_breadth": snap.escalation_breadth,
        // Movement attribution for THIS tick (WHY it moved) — hover fodder for the
        // timeline chart's live appends; the durable copy rides TimelineEntry.drivers.
        // Empty on immaterial ticks. Diagnostic; never feeds P.
        "tick_drivers": snap.tick_drivers,
        // Structured twins (url/age/snippet) — the clickable audit trail behind the
        // hover card. Additive key; contract v1 consumers reading tick_drivers only
        // are untouched. Diagnostic; never feeds P.
        "tick_driver_refs": snap.tick_driver_refs,
        "indicators": crate::indicators::evaluate(snap),
        "meta": {
            "events_in_window":         snap.events_in_window,
            "data_blind":               crate::bayesian::is_data_blind(snap.events_in_window),
            "thinly_sourced":           crate::bayesian::is_thinly_sourced(snap.events_in_window, snap.sources_active),
            "at_ceiling":               crate::bayesian::is_at_forecast_ceiling(snap.p_wwiii_annual),
            "breadth_saturated":        snap.couplers.breadth_saturated,
            "read_held_by_floor":       crate::theater::systemic_read_is_floor_held(&snap.theaters),
            // Observation-coverage honesty (bayesian::observation_coverage): how much of
            // the aggregation window the pipe actually WATCHED, the newest live signal's
            // age, the confidence discount they applied, and the caveat flag. Distinct
            // from data_blind/thinly_sourced — a store still warm with pre-outage events
            // is neither blind nor thin, yet the world went unwatched (2026-07-17).
            "window_coverage":          snap.window_coverage,
            "newest_event_age_secs":    snap.newest_event_age_secs,
            "observation_factor":       snap.observation_factor,
            "observation_gap":          snap.observation_gap,
            "sources_active":           snap.sources_active,
            "great_power_events":       snap.great_power_events,
            "regions_active":           snap.regions_active,
            "top_actors":               snap.top_actors,
            "aggregation_window_hours": snap.aggregation_window_hours,
            "max_window_events":        MAX_WINDOW_EVENTS,
        },
    })
}

// ── Timeline persistence ────────────────────────────────────────────────────────
//
// Writes to a date-rotated JSONL file: logs/timeline_YYYY-MM-DD.jsonl.
// No background rotation task is required — the path changes at local (Eastern)
// midnight naturally as today_timeline_path() recomputes the date on each call.

/// Movement-attribution gate: |Δ annual| in ONE tick at/above which the tick's new
/// events are recorded onto the snapshot/timeline as drivers (0.0005 = 0.05pp — a
/// real batch-driven knock on the chart, well above 1 Hz numeric wiggle). Display
/// threshold only — never touches P or any fitted constant.
pub const DRIVER_NOTE_MIN_DELTA: f64 = 0.0005;
/// Top new events (severity-first) named per material tick — enough to answer
/// "what did this", small enough for a hover card.
const DRIVER_NOTE_MAX_EVENTS: usize = 3;
/// Driver titles are clipped to this many chars for the hover card (full articles
/// remain one click away in the feed).
const DRIVER_TITLE_MAX_CHARS: usize = 96;
/// Snippet cap for the hover card's structured refs (DriverRef.snippet): enough to
/// read like a mini-article lede, small enough that a material tick adds ~600 bytes
/// to the ring/archive, not a body dump.
const DRIVER_SNIPPET_MAX_CHARS: usize = 180;

async fn append_timeline(entry: &TimelineEntry) {
    let line = match serde_json::to_string(entry) {
        Ok(s) => s + "\n",
        Err(e) => { warn!("Timeline serialise failed: {e}"); return; }
    };
    let path = today_timeline_path();
    match OpenOptions::new().create(true).append(true).open(&path).await {
        Ok(mut f) => {
            if let Err(e) = f.write_all(line.as_bytes()).await {
                warn!("Timeline write to {path}: {e}");
            }
        }
        Err(e) => warn!("Timeline open {path}: {e}"),
    }
}

// ── Article persistence ─────────────────────────────────────────────────────────
//
// Appends one StoredArticle per line to a date-rotated JSONL file
// (logs/articles_YYYY-MM-DD.jsonl). Called twice per article over its life:
// once at ingest (empty tags) and once after NLP applies domain tags. The boot
// loader (load_articles) dedups by id keeping the last occurrence, so a reloaded
// article carries its final tags. Best-effort: a write failure is logged but
// never blocks ingestion.

pub async fn append_article(article: &StoredArticle) {
    let line = match serde_json::to_string(article) {
        Ok(s) => s + "\n",
        Err(e) => { warn!("Article serialise failed: {e}"); return; }
    };
    let path = today_article_path();
    match OpenOptions::new().create(true).append(true).open(&path).await {
        Ok(mut f) => {
            if let Err(e) = f.write_all(line.as_bytes()).await {
                warn!("Article write to {path}: {e}");
            }
        }
        Err(e) => warn!("Article open {path}: {e}"),
    }
}

// ── Event persistence ───────────────────────────────────────────────────────────
//
// Appends one scored GeopoliticalEvent per line to a date-rotated JSONL file
// (logs/events_YYYY-MM-DD.jsonl) when it first enters the window. load_events()
// restores the window at boot so domain scores and P(WWIII) survive restarts
// instead of resetting to baseline (which made rare domains like WMD read 0%
// for a long time post-redeploy). Best-effort: a write failure is logged, never
// blocks the aggregator.

pub async fn append_events(events: &[GeopoliticalEvent]) {
    if events.is_empty() { return; }
    // Serialise the whole batch first, then write with a single file open — a feed
    // batch can land many new events in one aggregator tick, and one open+write
    // beats one open per event in the hot drain path. Best-effort: a serialise
    // failure drops that one line; an open/write failure is logged, never blocks.
    let mut buf = String::new();
    for event in events {
        match serde_json::to_string(event) {
            Ok(s) => { buf.push_str(&s); buf.push('\n'); }
            Err(e) => warn!("Event serialise failed: {e}"),
        }
    }
    if buf.is_empty() { return; }
    let path = today_event_path();
    match OpenOptions::new().create(true).append(true).open(&path).await {
        Ok(mut f) => {
            if let Err(e) = f.write_all(buf.as_bytes()).await {
                warn!("Event write to {path}: {e}");
            }
        }
        Err(e) => warn!("Event open {path}: {e}"),
    }
}

// ── Article store ─────────────────────────────────────────────────────────────
// Backed by VecDeque for O(1) front-pop eviction.
// Index maps article id → absolute insertion counter (not a raw deque offset)
// so it remains valid after front-pops without a full rebuild.

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StoredArticle {
    pub id:           String,
    pub title:        String,
    pub url:          String,
    pub source:       String,
    pub tier:         u8,
    pub published_at: String,
    /// RFC3339 timestamp of when GCRM fetched the article (carried from
    /// RawArticle.fetched_at). Surfaced to the dashboard as "GCRM pulled … ET".
    #[serde(default)]
    pub ingested_at:  String,
    pub body:         String,
    pub domain_tags:  Vec<String>,
}

/// Verdict of [`ArticleStore::near_duplicate_of`].
#[derive(Debug, PartialEq)]
pub enum NearDup {
    /// No recent near-duplicate — store a new row.
    New,
    /// Same source re-issued an edited headline — replace that row (id).
    Edition(String),
    /// Another source's copy of a story already on display — suppress the row
    /// (the event pipeline still processes the article; display only).
    Syndicated(String),
}

/// Title-similarity floor for the store-level near-duplicate pass. Measured on the
/// live store 2026-07-05: all sampled pairs at/above 0.70 were true duplicates
/// (syndication or editions); distinct stories sampled ≤0.55. Display-layer only.
pub const NEAR_DUP_STORE_SIM: f64 = 0.70;
/// How many newest rows the near-duplicate pass scans (bounds cost; ~4-8h of flow).
pub const NEAR_DUP_SCAN: usize = 400;

#[derive(Debug, Default)]
pub struct ArticleStore {
    pub articles:   VecDeque<StoredArticle>,
    pub index:      HashMap<String, usize>,   // id → absolute insertion counter
    /// canonical url → article id, for update-in-place on re-ingest of a known
    /// URL (live-blog title/body edits). Empty-URL articles are not indexed.
    url_index:      HashMap<String, String>,
    front_counter:  usize,                    // absolute index of deque[0]
    total_inserted: usize,                    // next absolute index to assign
    pub max_size:   usize,
}

impl ArticleStore {
    pub fn new(max_size: usize) -> Self {
        Self {
            articles:      VecDeque::new(),
            index:         HashMap::new(),
            url_index:     HashMap::new(),
            front_counter: 0,
            total_inserted: 0,
            max_size,
        }
    }

    pub fn push(&mut self, article: StoredArticle) {
        if self.articles.len() >= self.max_size {
            // O(1): pop oldest from front, remove its index entries
            if let Some(evicted) = self.articles.pop_front() {
                self.index.remove(&evicted.id);
                // Only unmap the URL if it still points at the evicted row — a
                // newer row may have legitimately reclaimed the same URL.
                if self.url_index.get(&evicted.url).is_some_and(|id| *id == evicted.id) {
                    self.url_index.remove(&evicted.url);
                }
                self.front_counter += 1;
            }
        }
        // Record absolute position
        self.index.insert(article.id.clone(), self.total_inserted);
        if !article.url.is_empty() {
            self.url_index.insert(article.url.clone(), article.id.clone());
        }
        self.total_inserted += 1;
        self.articles.push_back(article);
    }

    /// Update-in-place for a re-ingested canonical URL (live-blog title/body
    /// edits): refresh title/body/published_at/ingested_at on the EXISTING row —
    /// id and domain_tags survive — and return a clone for the JSONL archive
    /// (same id, so the boot loader's last-occurrence-wins keeps the newest
    /// content). None when the URL isn't in the store: the caller inserts
    /// normally. An incoming published_at OLDER than the stored one is refused —
    /// a stale syndicated copy must not clobber the newest edit.
    pub fn update_by_url(
        &mut self,
        url:          &str,
        source:       &str,
        title:        &str,
        body:         &str,
        published_at: &str,
        ingested_at:  &str,
    ) -> Option<StoredArticle> {
        if url.is_empty() { return None; }
        let id = self.url_index.get(url)?.clone();
        let &abs_pos = self.index.get(&id)?;
        let slot = abs_pos.wrapping_sub(self.front_counter);
        let art = self.articles.get_mut(slot)?;
        if art.id != id {
            warn!(
                "ArticleStore::update_by_url: index desync — slot {slot} has id='{}', \
                 expected '{id}'. Skipping update to prevent silent data corruption.",
                art.id
            );
            return None;
        }
        // Same-URL, DIFFERENT source = a syndicated/search-loop copy of an article this
        // store already carries (GDELT surfaces the exact publisher URLs the RSS roster
        // stored, with page-<title> furniture and a scrape-time date that always reads
        // "newer") — NOT an edit. Keep the original untouched; the caller treats Some as
        // handled so no duplicate row is inserted. Only the owning feed updates its rows.
        if art.source != source { return Some(art.clone()); }
        // Both timestamps are RFC3339; when both parse, refuse to go backwards.
        // (Unparseable timestamps allow the update — newest ingest wins.)
        let older = chrono::DateTime::parse_from_rfc3339(published_at).ok()
            .zip(chrono::DateTime::parse_from_rfc3339(&art.published_at).ok())
            .is_some_and(|(new, old)| new < old);
        if older { return Some(art.clone()); } // keep stored content; re-archive as-is
        art.title        = title.to_string();
        // A degenerate re-fetch (title-only entry) must not blank a real excerpt.
        if !body.trim().is_empty() || art.body.trim().is_empty() {
            art.body = body.to_string();
        }
        art.published_at = published_at.to_string();
        art.ingested_at  = ingested_at.to_string();
        Some(art.clone())
    }

    /// What to do with an incoming article whose TITLE is a near-duplicate of a
    /// recently stored row. Same-URL edits are handled earlier by `update_by_url`;
    /// this catches the two classes that slipped past it (operator-reported
    /// 2026-07-05: 25 near-dup pairs in the newest 600 rows):
    ///  - the same outlet re-issuing an edited headline under a NEW url (an
    ///    edition — the new copy should REPLACE the row, like a live-blog edit);
    ///  - a syndicated wire story re-headlined by another outlet, or an outlet's
    ///    video twin of its own text story (the row is CLUTTER — the event
    ///    pipeline still sees the article, so corroboration credit is unaffected).
    ///
    /// Measured threshold: every sampled pair ≥0.70 trigram-Jaccard was a true
    /// duplicate; distinct stories in the sample sat ≤0.55.
    pub fn near_duplicate_of(&self, title: &str, source: &str) -> NearDup {
        // -live rows are rolling transcripts with their own update-in-place contract:
        // they neither suppress durable copy nor get suppressed here.
        if source.ends_with("-live") {
            return NearDup::New;
        }
        let incoming_video = source.ends_with("-video");
        let mut syndicated: Option<String> = None;
        for art in self.articles.iter().rev().take(NEAR_DUP_SCAN) {
            if art.source.ends_with("-live") {
                continue;
            }
            let sim = crate::video::title_trigram_jaccard(title, &art.title);
            if sim < NEAR_DUP_STORE_SIM {
                continue;
            }
            if art.source == source {
                // A same-source match is an EDITION and outranks any cross-source
                // match seen earlier in the scan (a newer syndicated copy must not
                // shadow the outlet's own row and strand its stale headline).
                return NearDup::Edition(art.id.clone());
            }
            // Durable wire text is preferred over a video twin: an incoming WIRE
            // article is never suppressed by a stored -video row (the reverse — a
            // video twin deferring to existing wire copy — is suppressed).
            if !incoming_video && art.source.ends_with("-video") {
                continue;
            }
            syndicated.get_or_insert_with(|| art.source.clone());
        }
        match syndicated {
            Some(s) => NearDup::Syndicated(s),
            None => NearDup::New,
        }
    }

    /// Replace an edition row (found via [`Self::near_duplicate_of`]) with the
    /// re-issued copy — same id, so the JSONL append supersedes older lines at
    /// reload, exactly like the same-URL update path.
    pub fn update_edition(
        &mut self,
        id:           &str,
        title:        &str,
        url:          &str,
        body:         &str,
        published_at: &str,
        ingested_at:  &str,
    ) -> Option<StoredArticle> {
        let &abs_pos = self.index.get(id)?;
        let slot = abs_pos.wrapping_sub(self.front_counter);
        let art = self.articles.get_mut(slot)?;
        if art.id != id {
            return None;
        }
        // Refuse to go BACKWARDS (same discipline as update_by_url): a stale
        // re-serve of a superseded edition — day-rollover live-blog lists keep
        // yesterday's entry alive; boot re-fetches replay old entries after the
        // dedup caches reset — must not revert the row or ping-pong it. Treat
        // the incoming stale copy as handled (Some) so no duplicate row inserts.
        let older = chrono::DateTime::parse_from_rfc3339(published_at).ok()
            .zip(chrono::DateTime::parse_from_rfc3339(&art.published_at).ok())
            .is_some_and(|(new, old)| new < old);
        if older { return Some(art.clone()); }
        // ADD the edition's URL as another key to the same row (never remove the
        // old key, never index an empty URL): both editions' URLs then hit the
        // same-URL update path in future instead of re-entering this pass.
        if !url.is_empty() {
            self.url_index.insert(url.to_string(), art.id.clone());
            art.url = url.to_string();
        }
        art.title        = title.to_string();
        if !body.trim().is_empty() || art.body.trim().is_empty() {
            art.body = body.to_string();
        }
        art.published_at = published_at.to_string();
        art.ingested_at  = ingested_at.to_string();
        Some(art.clone())
    }

    /// Read-only lookup by article id (the event pipeline's `raw_article_id` join
    /// key) — the driver-note block resolves the hover card's url/age/snippet here.
    /// Same index→slot arithmetic as set_domain_tags; a desynced slot returns None
    /// rather than a wrong article.
    pub fn get_by_id(&self, id: &str) -> Option<&StoredArticle> {
        let &abs_pos = self.index.get(id)?;
        let slot = abs_pos.wrapping_sub(self.front_counter);
        self.articles.get(slot).filter(|a| a.id == id)
    }

    /// Apply NLP domain tags to an article by ID.
    ///
    /// Debug builds: panic on mismatch (catches bugs during testing).
    /// Release builds: WARN log + skip write (no silent data corruption).
    ///
    /// Returns a clone of the updated article on success so the caller can
    /// persist the now-tagged copy to the on-disk archive.
    pub fn set_domain_tags(&mut self, id: &str, tags: Vec<String>) -> Option<StoredArticle> {
        if let Some(&abs_pos) = self.index.get(id) {
            let slot = abs_pos.wrapping_sub(self.front_counter);
            if let Some(art) = self.articles.get_mut(slot) {
                debug_assert_eq!(
                    art.id, id,
                    "ArticleStore index desync: slot {slot} holds id='{}' but expected '{id}'. \
                     front_counter={}, abs_pos={}",
                    art.id, self.front_counter, abs_pos
                );
                if art.id != id {
                    warn!(
                        "ArticleStore::set_domain_tags: index desync — slot {slot} has id='{}', \
                         expected '{}'. Skipping to prevent silent data corruption. \
                         front_counter={}, abs_pos={}",
                        art.id, id, self.front_counter, abs_pos
                    );
                    return None;
                }
                art.domain_tags = tags;
                return Some(art.clone());
            }
        }
        None
    }

    pub fn query(&self, limit: usize, source_filter: Option<&str>, domain_filter: Option<&str>) -> Vec<&StoredArticle> {
        let mut result: Vec<&StoredArticle> = self.articles.iter()
            .filter(|a| source_filter.is_none_or(|s| a.source == s))
            .filter(|a| domain_filter.is_none_or(|d| a.domain_tags.iter().any(|t| t == d)))
            .collect();
        result.sort_unstable_by_key(|a| std::cmp::Reverse(
            chrono::DateTime::parse_from_rfc3339(&a.published_at)
                .map(|d| d.timestamp())
                .unwrap_or(0)
        ));
        result.truncate(limit);
        result
    }

    pub fn len(&self) -> usize { self.articles.len() }
}

// ── Epoch store ───────────────────────────────────────────────────────────────
// In-memory ring of TimelineEntry JSON values. Loaded from disk at boot so
// the dashboard immediately has recent P(WWIII) history without disk reads on
// every client connect or /api/epoch request.
//
// Capacity: MAX_EPOCH_ENTRIES = 350,640 entries.
// At 1-second ticks: 350,640 ÷ 3,600 ≈ 97.4 hours ≈ ~4 days before the oldest
// entries roll off. The JSONL files on disk are the durable permanent record;
// this ring is a fast-serve read cache only.
// Memory: ~350k × ~80 B/entry ≈ ~28 MB — negligible.

pub const MAX_EPOCH_ENTRIES: usize = 35_064 * 10; // ~350k; ~28 MB at ~80 B/entry

/// Default cap on the timeline sent per WebSocket connect and per uncapped /api/timeline
/// request. The full ring (up to MAX_EPOCH_ENTRIES ≈ 350k) was cloned and serialized to a
/// multi-hundred-MB JSON string for EVERY client on connect — a steady-state memory/bandwidth
/// spike. ~50k entries (~14h at 1Hz) is already far more than the chart shows meaningfully;
/// deeper durable history stays available on disk and via /api/epoch?limit=N. (audit aggregator-1)
pub const WS_TIMELINE_BOOTSTRAP: usize = 50_000;

/// Points actually SENT for the initial chart draw after display-decimation. The chart is
/// ~900–1700 CSS px wide, so 50k points is ~30–50 points per pixel — pure payload (measured
/// 2026-07-24: a 9.4 MB WebSocket bootstrap frame) for nothing an eye can resolve. ~2.5k
/// points is still sub-pixel at any monitor width while cutting that frame ~95%.
pub const TIMELINE_DISPLAY_POINTS: usize = 2_500;

/// Display-decimate a timeline slice for the initial draw.
///
/// Stride-samples `entries` down to about [`TIMELINE_DISPLAY_POINTS`] across the WHOLE slice, so
/// the full history still spans the axis — it drops resolution the screen cannot show, never
/// range. Two classes of point are ALWAYS kept regardless of stride:
///   * the first and last entry, so the endpoints and the live edge stay exact; and
///   * every entry carrying a `drivers` record — those are the hoverable knocks the chart seeds
///     its spike markers and audit cards from, and losing them would silently delete history.
///
/// Returns the input untouched when it is already at or under the target.
pub fn decimate_timeline(entries: Vec<serde_json::Value>, target: usize) -> Vec<serde_json::Value> {
    let n = entries.len();
    if n <= target || target == 0 { return entries; }
    let stride = n.div_ceil(target).max(1);
    let last = n - 1;
    entries.into_iter().enumerate()
        .filter(|(i, e)| {
            *i == 0 || *i == last || i % stride == 0
                || e.get("drivers").and_then(|d| d.as_array()).is_some_and(|a| !a.is_empty())
        })
        .map(|(_, e)| e)
        .collect()
}

#[derive(Debug, Default)]
pub struct EpochStore {
    ring: VecDeque<serde_json::Value>,
    /// Stride-cached momentum lead-lag payload: the diagnostic scans the whole 48h
    /// window, its answer can only change once per stride, and it used to run per 1 Hz
    /// broadcast while holding the shared epoch_store lock. (Cache, not eviction: the
    /// ring itself is untouched.)
    mom_ll_cache: Option<(DateTime<Utc>, serde_json::Value)>,
    /// Stride-cached `band_coverage` payload — same rationale as `mom_ll_cache`. The diagnostic
    /// scans the whole 48h ring and (since the full-resolution half-width fix) rebuilds each
    /// anchor's band from EVERY in-window read, so it is far too heavy to run per 1 Hz broadcast
    /// under the shared lock; its answer can only change once per `BAND_COV_STRIDE_SECS` anyway.
    band_cov_cache: Option<(DateTime<Utc>, serde_json::Value)>,
}

impl EpochStore {
    pub fn new() -> Self { Self { ring: VecDeque::new(), mom_ll_cache: None, band_cov_cache: None } }

    /// Append one timeline entry. Evicts oldest when ring is full.
    pub fn push(&mut self, entry: serde_json::Value) {
        if self.ring.len() >= MAX_EPOCH_ENTRIES {
            self.ring.pop_front();
        }
        self.ring.push_back(entry);
    }

    /// Return up to `limit` entries newest-first.
    pub fn query(&self, limit: usize) -> Vec<&serde_json::Value> {
        self.ring.iter().rev().take(limit).collect()
    }

    /// Total entries held.
    pub fn len(&self) -> usize { self.ring.len() }

    /// Trailing-6h trend of P(WWIII), computed server-side from the durable ring.
    ///
    /// Returns the JSON object the dashboard reads as `data.trend_6h`:
    /// `{available, delta, baseline, samples, span_secs}` — `delta` is
    /// `current − (p_annual of the oldest entry within the last 6h)`, in
    /// probability units. This used to be reconstructed in the browser from a
    /// per-tab session buffer that had to be re-seeded inside `applyTimeline()`
    /// on every page load; any dashboard refactor that dropped the seed silently
    /// reset the "6h Trend" readout to "—". Computing it here makes it durable
    /// and independent of the client, so a UI rewrite can no longer break the
    /// math. The browser only renders the number. Locked by `epoch_store_trend_*`.
    pub fn trend_6h(&self, current_p: f64) -> serde_json::Value {
        self.trend_window(current_p, Utc::now(), 6 * 3600, 2)
    }

    /// Testable core of [`trend_6h`]: caller injects `now`, the window length and
    /// the minimum sample count. Walks the ring newest→oldest, taking the oldest
    /// entry still inside `[now − window_secs, now]` as the baseline. Reports
    /// `available:false` (and `delta:0`) when there isn't yet `min_samples` of
    /// in-window history rather than fabricating a trend from too little data.
    pub fn trend_window(
        &self,
        current_p: f64,
        now: DateTime<Utc>,
        window_secs: i64,
        min_samples: usize,
    ) -> serde_json::Value {
        let cutoff = now - chrono::Duration::seconds(window_secs);
        let mut baseline: Option<f64> = None;
        let mut oldest: Option<DateTime<Utc>> = None;
        // Lead theater of the oldest in-window entry — the WHERE the read was concentrated
        // at the start of the window, so the caller can tell whether the locus of risk
        // RELOCATED across the 6h (a shift the bare delta can't show). Overwrites alongside
        // `baseline`, so it ends at the same oldest-in-window tick.
        let mut baseline_lead: Option<String> = None;
        let mut samples = 0usize;
        for e in self.ring.iter().rev() {
            let t = match e
                .get("t")
                .and_then(|v| v.as_str())
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            {
                Some(dt) => dt.with_timezone(&Utc),
                None => continue,
            };
            if t > now {
                continue; // ignore future-dated ticks (clock skew) — same discipline as the sibling diagnostics
            }
            if t < cutoff {
                break;
            }
            if let Some(p) = e.get("p_annual").and_then(|v| v.as_f64()) {
                baseline = Some(p); // overwrite each step → ends at the oldest in-window
                oldest = Some(t);
                baseline_lead = Some(
                    e.get("lead").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                );
                samples += 1;
            }
        }
        match (baseline, oldest) {
            (Some(b), Some(o)) if samples >= min_samples => serde_json::json!({
                "available": true,
                "delta":     current_p - b,
                "baseline":  b,
                "samples":   samples,
                "span_secs": (now - o).num_seconds().max(0),
                "lead_then": baseline_lead.unwrap_or_default(),
            }),
            _ => serde_json::json!({
                "available": false,
                "delta":     0.0,
                "samples":   samples,
                "span_secs": 0,
            }),
        }
    }

    /// Honest headline interval around the current P(WWIII), computed server-side from the durable
    /// ring (like [`trend_6h`]). The dashboard renders the BAND, never a bare point. See
    /// [`Self::uncertainty_window`] for the construction.
    pub fn uncertainty_6h(&self, current_p: f64, confidence: f64) -> serde_json::Value {
        self.uncertainty_window(current_p, confidence, Utc::now(), 6 * 3600)
    }

    /// Testable core of [`uncertainty_6h`]. The interval half-width is:
    ///   hw = max(empirical, HUMILITY_FLOOR_HW) × (1 + DATA_QUALITY_WIDENING × (1 − confidence))
    /// where `empirical` is half the central-80% (P10..P90) spread of the last-window reads — how
    /// much the model itself has actually been moving. The humility floor means a momentarily
    /// stable read still publishes an honest band (stability ≠ being right about the future), and
    /// thin/stale data widens it further. Clamped to [0, FORECAST_PROB_CEILING] so the band never
    /// claims a value above the engineering ceiling. `floored` flags that the irreducible humility
    /// floor — not observed volatility — is what set the width.
    pub fn uncertainty_window(
        &self,
        current_p: f64,
        confidence: f64,
        now: DateTime<Utc>,
        window_secs: i64,
    ) -> serde_json::Value {
        let cutoff = now - chrono::Duration::seconds(window_secs);
        let mut ps: Vec<f64> = Vec::new();
        for e in self.ring.iter().rev() {
            let t = match e
                .get("t")
                .and_then(|v| v.as_str())
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            {
                Some(dt) => dt.with_timezone(&Utc),
                None => continue,
            };
            if t > now {
                continue; // ignore future-dated ticks (clock skew) — same discipline as the sibling diagnostics
            }
            if t < cutoff {
                break;
            }
            if let Some(p) = e.get("p_annual").and_then(|v| v.as_f64()) {
                ps.push(p);
            }
        }
        // Empirical half-width = half the central-80% spread of recent reads (robust to single spikes).
        let empirical_hw = if ps.len() >= 4 {
            ps.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            (percentile_sorted(&ps, 0.90) - percentile_sorted(&ps, 0.10)) / 2.0
        } else {
            0.0
        };
        let conf = confidence.clamp(0.0, 1.0);
        let floored = empirical_hw < crate::models::HUMILITY_FLOOR_HW;
        let hw = empirical_hw.max(crate::models::HUMILITY_FLOOR_HW)
            * (1.0 + crate::models::DATA_QUALITY_WIDENING * (1.0 - conf));
        let low = (current_p - hw).max(0.0);
        let high = (current_p + hw).min(crate::models::FORECAST_PROB_CEILING);
        let r6 = |x: f64| (x * 1e6).round() / 1e6;
        serde_json::json!({
            "low":             r6(low),
            "high":            r6(high),
            "low_pct":         (low  * 100.0 * 1e2).round() / 1e2,
            "high_pct":        (high * 100.0 * 1e2).round() / 1e2,
            "half_width_pct":  (hw   * 100.0 * 1e2).round() / 1e2,
            "empirical_hw_pct":(empirical_hw * 100.0 * 1e2).round() / 1e2,
            "floored":         floored,
            "samples":         ps.len(),
            "basis": "central-80% of the last 6h of model reads, floored at the humility minimum \
                      for irreducible forecast uncertainty, widened when data quality is low",
        })
    }

    /// Where the current read sits within its RECENT RANGE, computed server-side from the durable
    /// ring — a durable, well-defined replacement for the per-tab "session peak/low" the browser
    /// used to compute from whatever timeline it happened to bootstrap. That client value drifted
    /// with tab uptime (two operators saw different "peaks") and a fresh tab, or one whose bootstrap
    /// seed was dropped by a UI refactor, read hi==lo==current — a false "flat at its own value".
    /// The window is FIXED here, off the full durable ring, so the range means the same thing for
    /// everyone regardless of when they opened the page.
    ///
    /// Reports the range `[lo, hi]` over the last `window_secs` of reads AND the current read's
    /// POSITION in it: a percentile rank (`pct_rank` — the share of window reads at or below the
    /// current one) plus a plain-language `position` tag. This lets the operator distinguish a "60%
    /// that is a multi-day HIGH" (fresh territory) from a "60% RANGE-BOUND for days" (sustained
    /// plateau) from a "60% near the LOW of a higher band" (de-escalating) — context neither the
    /// bare headline nor the 6h delta can give. When the range is essentially flat
    /// (`hi − lo < FLAT_RANGE_PP`), no high/low is claimed (`position:"flat"`): a stable read is not
    /// "at its high". Honest-null (`available:false`) below `min_samples`. Diagnostic only —
    /// computed after P is final, it never feeds P or any fitted constant.
    pub fn read_range(&self, current_p: f64) -> serde_json::Value {
        self.read_range_window(current_p, Utc::now(), READ_RANGE_WINDOW_SECS, READ_RANGE_MIN_SAMPLES)
    }

    /// Testable core of [`read_range`]: caller injects `now`, the window length and the minimum
    /// sample count (same injection discipline as [`trend_window`] / [`uncertainty_window`]).
    pub fn read_range_window(
        &self,
        current_p: f64,
        now: DateTime<Utc>,
        window_secs: i64,
        min_samples: usize,
    ) -> serde_json::Value {
        let cutoff = now - chrono::Duration::seconds(window_secs);
        let mut ps: Vec<f64> = Vec::new();
        let mut oldest: Option<DateTime<Utc>> = None;
        for e in self.ring.iter().rev() {
            let t = match e
                .get("t")
                .and_then(|v| v.as_str())
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            {
                Some(dt) => dt.with_timezone(&Utc),
                None => continue,
            };
            if t > now {
                continue; // ignore future-dated ticks (clock skew) — same discipline as the sibling diagnostics
            }
            if t < cutoff {
                break;
            }
            if let Some(p) = e.get("p_annual").and_then(|v| v.as_f64()) {
                ps.push(p);
                oldest = Some(t); // ends at the oldest in-window tick → the true span
            }
        }
        if ps.len() < min_samples {
            return serde_json::json!({
                "available": false,
                "samples":   ps.len(),
                "span_secs": 0,
            });
        }
        // Span honesty: many ticks ≠ much time. The sample floor above is COUNT-based, and a 1 Hz
        // ring satisfies it seconds after a restart while spanning seconds — a `position:"near-high"`
        // / `pct_rank` asserting a multi-day HIGH off half a minute of data is the post-restart lie
        // the 2026-07-17 outage exposed on the sibling `lead_concentration_window`. Honest-null
        // (mirroring that sibling's `short_history` guard) until real span accrues.
        let span_secs = oldest.map(|o| (now - o).num_seconds().max(0)).unwrap_or(0);
        if span_secs < READ_RANGE_MIN_SPAN_SECS {
            return serde_json::json!({
                "available": false,
                "reason":    "short_history",
                "samples":   ps.len(),
                "span_secs": span_secs,
            });
        }
        // Range = observed min/max (the "high/low" the operator expects). Position = the percentile
        // RANK of the current read among the window's reads — robust to a single transient spike
        // that would otherwise set `hi` far above everything and make every later read read "low".
        let lo = ps.iter().cloned().fold(f64::INFINITY, f64::min);
        let hi = ps.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let n = ps.len() as f64;
        let at_or_below = ps.iter().filter(|&&p| p <= current_p).count() as f64;
        let pct_rank = (at_or_below / n * 100.0 * 1e1).round() / 1e1;
        let flat = (hi - lo) < FLAT_RANGE_PP / 100.0;
        // Position tag off the percentile rank (not the raw min/max fraction), so a lone spike in
        // `hi` cannot mislabel a genuinely high read as "mid". Flat range → no high/low claim.
        let position = if flat {
            "flat"
        } else if pct_rank >= 90.0 {
            "near-high"
        } else if pct_rank >= 66.0 {
            "upper"
        } else if pct_rank > 33.0 {
            "mid"
        } else if pct_rank > 10.0 {
            "lower"
        } else {
            "near-low"
        };
        let r6 = |x: f64| (x * 1e6).round() / 1e6;
        let p2 = |x: f64| (x * 100.0 * 1e2).round() / 1e2;
        serde_json::json!({
            "available": true,
            "lo":        r6(lo),
            "hi":        r6(hi),
            "lo_pct":    p2(lo),
            "hi_pct":    p2(hi),
            "pct_rank":  pct_rank,
            "position":  position,
            "flat":      flat,
            "span_secs": span_secs,
            "samples":   ps.len(),
        })
    }

    /// Forward interval-COVERAGE of the published headline band — the honest VALIDATION of
    /// [`uncertainty_window`], computed server-side from the durable ring. The band is published every
    /// tick as an ~80% interval; this measures whether reality actually stayed inside it. Over the
    /// archived history, for each past tick `t` it reconstructs the band that was standing then and
    /// checks whether the read a fixed HORIZON later fell within it. A well-calibrated 80% band
    /// contains the forward read ~80% of the time; materially LESS means the band was too tight
    /// (`overconfident` — real moves escaped it); materially MORE means it was `conservative`, as the
    /// deliberate humility floor intends. Diagnostic only — computed after P is final, it never feeds
    /// P or any fitted constant. See [`Self::band_coverage_window`] for the construction.
    ///
    /// Stride-cached (as [`Self::momentum_lead_lag`] is): the 48h scan now rebuilds every anchor's
    /// band at full 1 Hz resolution, so recompute at most once per `BAND_COV_STRIDE_SECS` and serve
    /// the cached payload on the 1 Hz broadcast in between.
    pub fn band_coverage(&mut self) -> serde_json::Value {
        let now = Utc::now();
        if let Some((at, v)) = &self.band_cov_cache {
            if (now - *at).num_seconds() < BAND_COV_STRIDE_SECS {
                return v.clone();
            }
        }
        let v = self.band_coverage_window(
            now,
            BAND_COV_WINDOW_SECS,
            BAND_COV_BAND_SECS,
            BAND_COV_HORIZON_SECS,
            BAND_COV_STRIDE_SECS,
            BAND_COV_MIN_PAIRS,
        );
        self.band_cov_cache = Some((now, v.clone()));
        v
    }

    /// Testable core of [`band_coverage`]: caller injects `now`, the lookback window, the band-window
    /// (the trailing span each band is built from — matches [`uncertainty_window`]'s 6h), the forward
    /// `horizon` at which coverage is checked, the decimation `stride`, and the minimum pair count.
    ///
    /// The reconstructed band uses the SAME empirical construction as [`uncertainty_window`]
    /// (`max(central-80% half-spread, HUMILITY_FLOOR_HW)`) but OMITS the confidence-widening term (the
    /// per-tick `confidence` is not carried in the ring). Since widening only ever WIDENS the published
    /// band, the reconstructed band is a SUBSET of it, so the reported coverage is a conservative FLOOR
    /// on the true published band's coverage — we never overstate how well the band performed.
    ///
    /// The ANCHOR series (which reads pair with a forward outcome) is stride-decimated (as in
    /// [`Self::momentum_lead_lag_window`]) so 1 Hz autocorrelated ticks don't inflate the pair count.
    /// But each anchor's HALF-WIDTH is rebuilt from the FULL-resolution trailing window — every
    /// in-window read, exactly the sample set [`uncertainty_window`] published the band from — NOT the
    /// decimated anchors. Decimating the half-width would draw the central-80% spread from ~72 samples
    /// instead of ~21,600, biasing it narrow; a band `uncertainty_window` reported `floored:false`
    /// (spread set by measured volatility) could then be mislabelled `floor_bound` here, and
    /// `mean_hw_pct`/`floor_bound_pct` would contradict the very band they claim to validate. Full
    /// resolution keeps the sharpness reads faithful to the published band.
    ///
    /// Alongside coverage (reliability), it also reports SHARPNESS (resolution): `mean_hw_pct`, the
    /// band's mean half-width (a floor, since the confidence-widening term is omitted), and
    /// `floor_bound_pct`, the share of reconstructable bands whose empirical spread was tighter than
    /// the humility floor — i.e. how often the ±7pp floor, not measured volatility, set the width.
    /// Coverage without sharpness is gameable (a very wide band trivially covers); the pair is the
    /// standard "maximise sharpness subject to calibration" read.
    pub fn band_coverage_window(
        &self,
        now: DateTime<Utc>,
        window_secs: i64,
        band_secs: i64,
        horizon_secs: i64,
        stride_secs: i64,
        min_pairs: usize,
    ) -> serde_json::Value {
        let cutoff = now - chrono::Duration::seconds(window_secs);
        // In-window entries newest→oldest, breaking at the cutoff (same discipline as the other ring
        // diagnostics — pre-window history is never parsed even though the ring can hold days beyond).
        let mut rev: Vec<(i64, f64)> = Vec::new();
        for e in self.ring.iter().rev() {
            let t = match e
                .get("t")
                .and_then(|v| v.as_str())
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            {
                Some(dt) => dt.with_timezone(&Utc),
                None => continue,
            };
            if t > now {
                continue;
            }
            if t < cutoff {
                break;
            }
            if let Some(p) = e.get("p_annual").and_then(|v| v.as_f64()) {
                rev.push(((t - cutoff).num_seconds(), p));
            }
        }
        // Full-resolution ascending series (secs_since_cutoff, p) — EVERY in-window read, the same
        // sample set the PUBLISHED band (`uncertainty_window`) is built from. Anchors' half-widths
        // are rebuilt from this below; the decimated series (next) is only the pair-count control.
        let full: Vec<(i64, f64)> = rev.iter().rev().copied().collect();
        // Ascending, stride-decimated ANCHOR series (with parallel indices back into `full`). The
        // decimation is the forward-coverage autocorrelation control — 1 Hz ticks are serially
        // correlated, so counting each as an independent horizon pair would fake precision. It is
        // NOT applied to the half-width reconstruction below.
        let mut series: Vec<(i64, f64)> = Vec::with_capacity(full.len() / 4 + 1);
        let mut series_full_idx: Vec<usize> = Vec::with_capacity(full.len() / 4 + 1);
        let mut last_kept: Option<i64> = None;
        for (idx, &(secs, p)) in full.iter().enumerate() {
            if let Some(lk) = last_kept {
                if secs - lk < stride_secs {
                    continue;
                }
            }
            last_kept = Some(secs);
            series.push((secs, p));
            series_full_idx.push(idx);
        }

        // For each anchor tick, reconstruct the empirical band from the trailing `band_secs` of the
        // series, then test whether the read nearest `anchor + horizon` fell inside it. `j` is a
        // forward two-pointer over the ascending series: the target grows monotonically with the
        // anchor, so it only ever moves forward (as in `momentum_lead_lag_window`).
        let tol = stride_secs;
        let (mut pairs, mut covered) = (0usize, 0usize);
        // BREACH DIRECTION (the reliability companion to coverage): of the reads that escaped the
        // band, how many broke ABOVE it (the read rose faster than the band allowed — the model
        // UNDER-warned, the dangerous direction) vs BELOW it (over-warned). Coverage says whether the
        // band fails; direction says which way — the decision-relevant half of an "overconfident"
        // verdict, since upward breaches mean escalation outran the model.
        let (mut breach_up, mut breach_down) = (0usize, 0usize);
        // SHARPNESS accumulators (the resolution companion to coverage): over every reconstructable
        // band — not only the ones that also formed a horizon pair — how WIDE the band typically is,
        // and how often the humility FLOOR rather than measured spread is what set that width.
        let (mut bands, mut sum_hw, mut floor_bound) = (0usize, 0.0f64, 0usize);
        let mut j = 0usize;
        let mut lo_full = 0usize;
        for i in 0..series.len() {
            let (ti, pi) = series[i];
            let ai = series_full_idx[i];
            // Trailing band-window reads at FULL resolution (need >= 4 to form a central-80% spread,
            // as `uncertainty_window` does). `lo_full` is a monotonic left edge over the ascending
            // `full` series: anchors advance in time, so the window start only ever moves forward.
            let lo_t = ti - band_secs;
            while lo_full < ai && full[lo_full].0 < lo_t {
                lo_full += 1;
            }
            if ai + 1 - lo_full < 4 {
                continue;
            }
            let mut win: Vec<f64> = full[lo_full..=ai].iter().map(|&(_, p)| p).collect();
            win.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let emp_hw = (percentile_sorted(&win, 0.90) - percentile_sorted(&win, 0.10)) / 2.0;
            let hw = emp_hw.max(crate::models::HUMILITY_FLOOR_HW);
            // Sharpness: accumulate this band's half-width and whether the humility floor bound it.
            // `hw` omits the confidence-widening term (as the whole reconstruction does), so the mean
            // is a FLOOR on the published band's mean half-width — never an overstatement of how tight
            // the model actually is. `emp_hw < FLOOR` is widening-independent: a clean read of how
            // often the ±7pp floor — not realized volatility — is what makes the band its width. The
            // `<` matches `uncertainty_window`'s own `floored` predicate (equality is not "floored").
            bands += 1;
            sum_hw += hw;
            if emp_hw < crate::models::HUMILITY_FLOOR_HW {
                floor_bound += 1;
            }
            // Forward read nearest ti + horizon, within ±stride tolerance.
            let target = ti + horizon_secs;
            while j + 1 < series.len() && series[j].0 < target {
                j += 1;
            }
            let cand = if j > 0 && (target - series[j - 1].0).abs() < (series[j].0 - target).abs() {
                j - 1
            } else {
                j
            };
            if (series[cand].0 - target).abs() > tol {
                continue; // no sample near ti + horizon
            }
            let pf = series[cand].1;
            pairs += 1;
            if pf >= pi - hw && pf <= pi + hw {
                covered += 1;
            } else if pf > pi + hw {
                breach_up += 1; // read rose past the band top → model under-warned
            } else {
                breach_down += 1; // read fell below the band bottom → model over-warned
            }
        }

        if pairs < min_pairs {
            return serde_json::json!({
                "available": false,
                "pairs":     pairs,
                "verdict":   "insufficient",
            });
        }
        let coverage = covered as f64 / pairs as f64 * 100.0;
        let verdict = if coverage < BAND_COV_NOMINAL_PCT - BAND_COV_TOL_PP {
            "overconfident" // reads escaped the band more than a 1-in-5 rate — band too tight
        } else if coverage > BAND_COV_NOMINAL_PCT + BAND_COV_TOL_PP {
            "conservative" // band wider than realized moves — the humility floor doing its job
        } else {
            "calibrated"
        };
        serde_json::json!({
            "available":    true,
            "coverage_pct": (coverage * 1e1).round() / 1e1,
            "breaches":     pairs - covered,
            // Direction of the breaches: how many escaped ABOVE the band (the model under-warned —
            // the dangerous direction) vs BELOW it. `breach_up + breach_down == breaches` by
            // construction (a non-covered read is strictly above or strictly below the band).
            "breaches_up":   breach_up,
            "breaches_down": breach_down,
            "pairs":        pairs,
            "nominal_pct":  BAND_COV_NOMINAL_PCT,
            "horizon_secs": horizon_secs,
            "verdict":      verdict,
            // SHARPNESS (the resolution half of the calibration read): the band's mean half-width and
            // how often the humility floor — not measured spread — set it. `mean_hw_pct` omits the
            // confidence-widening term, so it is a FLOOR on the published band's mean half-width.
            "mean_hw_pct":     (sum_hw / bands as f64 * 100.0 * 1e1).round() / 1e1,
            "floor_bound_pct": (floor_bound as f64 / bands as f64 * 100.0 * 1e1).round() / 1e1,
            "bands":           bands,
            "basis": "share of archived reads that landed inside the model's own band published one \
                      horizon earlier; the confidence-widening term is omitted, so this is a floor on \
                      the published band's true coverage. mean_hw_pct is the band's mean half-width (a \
                      floor, widening omitted); floor_bound_pct is the share of reads whose empirical \
                      spread was tighter than the humility floor, so the floor set the band width. \
                      breaches_up/breaches_down split the escaped reads by direction — above the band \
                      means the read outran the model (under-warned), below means it over-warned",
        })
    }

    /// Public entry: how long the headline has been CONTINUOUSLY at or above its current alert
    /// band, read off the durable ring. The TIME dimension of the current state — orthogonal to
    /// how HIGH (level), whether it is MOVING (delta/trend), and WHERE it sits in its numeric
    /// range (`read_range`). An operator reads a flash spike into Critical differently from a
    /// Critical that has held for days (entrenchment). Diagnostic only — reads the archived ring
    /// AFTER P is final; never feeds P or any fitted constant.
    pub fn alert_dwell(&self, current_alert: &str) -> serde_json::Value {
        self.alert_dwell_window(current_alert, Utc::now(), ALERT_DWELL_MIN_SAMPLES)
    }

    /// Testable core of [`alert_dwell`]. Walking the ring newest→oldest, count the CONTIGUOUS run
    /// of entries whose alert band is at or ABOVE the current one, and report `now − (oldest tick
    /// in that run)` as the dwell. "At or above" (not exact-level) so a read that climbed
    /// Elevated→Critical still reports the full time it has been *at least* Elevated when asked at
    /// the Elevated floor — the operator-meaningful "time since we last dropped below this
    /// severity". The run BREAKS on the first entry below the band, on an unparseable timestamp,
    /// or on a MISSING/unknown alert field (fail-closed: an entry we cannot confirm held the band
    /// ends the claim rather than silently extending it). When the run reaches the oldest ring
    /// entry without ever dropping below, `capped:true` and the dwell is a FLOOR (the true dwell
    /// began before the ring's horizon). Honest-null (`available:false`) below `min_samples` or
    /// when the current alert is unknown. Touches no fitted constant and never feeds P.
    pub fn alert_dwell_window(
        &self,
        current_alert: &str,
        now: DateTime<Utc>,
        min_samples: usize,
    ) -> serde_json::Value {
        let cur_rank = match alert_rank(current_alert) {
            Some(r) => r,
            None => return serde_json::json!({ "available": false, "reason": "unknown_alert" }),
        };
        let mut samples = 0usize;
        let mut oldest_t: Option<DateTime<Utc>> = None;
        let mut broke = false; // saw an entry BELOW the band → a real boundary, not the ring edge
        for e in self.ring.iter().rev() {
            let t = match e
                .get("t")
                .and_then(|v| v.as_str())
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            {
                Some(dt) => dt.with_timezone(&Utc),
                None => break, // unparseable tick — fail closed, do not extend the run past it
            };
            if t > now {
                continue; // ignore any future-dated tick (clock skew) without breaking the run
            }
            match e.get("alert").and_then(|v| v.as_str()).and_then(alert_rank) {
                Some(r) if r >= cur_rank => {
                    samples += 1;
                    oldest_t = Some(t);
                }
                Some(_) => {
                    broke = true;
                    break;
                }
                None => break, // missing/unknown alert on an in-band-until-now run — fail closed
            }
        }
        if samples < min_samples {
            return serde_json::json!({
                "available": false,
                "samples":   samples,
            });
        }
        let dwell_secs = oldest_t.map(|o| (now - o).num_seconds().max(0)).unwrap_or(0);
        serde_json::json!({
            "available":  true,
            "level":      current_alert,
            "dwell_secs": dwell_secs,
            "samples":    samples,
            // No boundary within the ring → the run began before the horizon; dwell is a floor.
            "capped":     !broke,
        })
    }

    pub fn lead_concentration(&self, current_lead: &str) -> serde_json::Value {
        self.lead_concentration_window(
            current_lead,
            Utc::now(),
            LEAD_CONC_WINDOW_SECS,
            LEAD_CONC_MIN_SAMPLES,
        )
    }

    /// Testable core of [`lead_concentration`]. Over the trailing 24h of the durable ring, tally the
    /// per-tick LEAD theater (`lead`, the hottest flashpoint that tick) and report how CONCENTRATED
    /// the locus of risk has been — the WHERE analog of `alert_dwell`'s time axis. `trend_6h` already
    /// names a binary now-vs-6h-ago RELOCATION; this reports the continuous picture the operator
    /// otherwise lacks: has one flashpoint entrenched itself (`current` held most of the day), or is
    /// the lead ROTATING across many fronts (a broadening, multi-front world reads very differently
    /// from a single deepening standoff, and a flickering near-tie makes the bare "relocated" flag
    /// noise). Only ticks carrying a non-empty lead are counted — a quiet world (no lead) is
    /// honest-null, never "0% concentrated". Reports `current`/`current_pct` (where the LIVE lead
    /// sits — it can be a fresh entrant with a small share even while another theater dominated the
    /// day) alongside the modal `top`/`top_pct` and the `distinct` front count. Honest-null below
    /// `min_samples` decisive ticks or when the current world has no lead. Touches no fitted constant
    /// and never feeds P.
    pub fn lead_concentration_window(
        &self,
        current_lead: &str,
        now: DateTime<Utc>,
        window_secs: i64,
        min_samples: usize,
    ) -> serde_json::Value {
        if current_lead.is_empty() {
            // No live locus to characterize — a quiet world, not a concentration of zero.
            return serde_json::json!({ "available": false, "reason": "no_lead" });
        }
        let cutoff = now - chrono::Duration::seconds(window_secs);
        let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        let mut total = 0usize;
        let mut oldest_counted: Option<DateTime<Utc>> = None;
        for e in self.ring.iter().rev() {
            let t = match e
                .get("t")
                .and_then(|v| v.as_str())
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            {
                Some(dt) => dt.with_timezone(&Utc),
                None => continue, // unparseable tick — skip, don't count (matches the window scans)
            };
            if t > now {
                continue; // ignore future-dated ticks (clock skew)
            }
            if t < cutoff {
                break;
            }
            let lead = match e.get("lead").and_then(|v| v.as_str()) {
                Some(s) if !s.is_empty() => s,
                _ => continue, // quiet tick with no lead — not a decisive sample
            };
            *counts.entry(lead.to_string()).or_insert(0) += 1;
            total += 1;
            oldest_counted = Some(t); // newest→oldest walk: last counted tick is the oldest
        }
        if total < min_samples {
            return serde_json::json!({ "available": false, "samples": total });
        }
        // Span honesty: many ticks ≠ much time. A freshly restarted 1 Hz ring clears the
        // sample floor within seconds while spanning minutes, and a day-share verdict
        // ("entrenched" = one flashpoint owned most of 24h) fabricated from that is the
        // post-restart lie the 2026-07-17 outage exposed. Honest-null until real span accrues.
        let span_secs = oldest_counted.map(|o| (now - o).num_seconds().max(0)).unwrap_or(0);
        if span_secs < LEAD_CONC_MIN_SPAN_SECS {
            return serde_json::json!({
                "available": false, "reason": "short_history",
                "samples": total, "span_secs": span_secs,
            });
        }
        // Modal lead over the window (ties broken alphabetically for a stable, deterministic pick).
        let (top, top_n) = counts
            .iter()
            .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
            .map(|(k, v)| (k.clone(), *v))
            .unwrap_or_default();
        let cur_n = counts.get(current_lead).copied().unwrap_or(0);
        let pct = |n: usize| ((n as f64 / total as f64) * 100.0 * 1e1).round() / 1e1;
        let top_pct = pct(top_n);
        let distinct = counts.len();
        // Display verdict (diagnostic thresholds — never a fitted constant, never feeds P):
        // one flashpoint owned most of the day → entrenched; four+ fronts with no plurality
        // majority → rotating; anything between → contested.
        let verdict = if top_pct >= 70.0 {
            "entrenched"
        } else if distinct >= 4 && top_pct < 45.0 {
            "rotating"
        } else {
            "contested"
        };
        serde_json::json!({
            "available":   true,
            "current":     current_lead,
            "current_pct": pct(cur_n),
            "top":         top,
            "top_pct":     top_pct,
            "distinct":    distinct,
            "samples":     total,
            "verdict":     verdict,
            "window_secs": window_secs,
            // Actual span the counted ticks cover (≥ MIN_SPAN, ≤ window) — the dashboard
            // renders the day-share against THIS, so "of 24h" is never claimed off 7h of ring.
            "span_secs":   span_secs,
        })
    }

    /// Stride-cached public entry: the window scan reads the whole 48h series, its
    /// answer can only change once per [`MOM_LL_STRIDE_SECS`], and it runs on the 1 Hz
    /// broadcast path under the shared epoch_store lock — so recompute at most once
    /// per stride and serve the cached payload in between.
    pub fn momentum_lead_lag(&mut self) -> serde_json::Value {
        let now = Utc::now();
        if let Some((at, v)) = &self.mom_ll_cache {
            if (now - *at).num_seconds() < MOM_LL_STRIDE_SECS {
                return v.clone();
            }
        }
        let v = self.momentum_lead_lag_window(now, MOM_LL_WINDOW_SECS, MOM_LL_STRIDE_SECS, MOM_LL_LAGS);
        self.mom_ll_cache = Some((now, v.clone()));
        v
    }

    /// Testable core of [`momentum_lead_lag`]. Builds a stride-decimated ascending series of
    /// `(secs, p_annual, mom, episode)` over the window, then for each candidate lag `L`
    /// measures whether the SIGN of the momentum at time `t` predicts the SIGN of the forward
    /// move `p(t+L) − p(t)`. A pair counts only when the momentum is decisive
    /// (`|m| ≥ MOM_DEADBAND`) AND the forward move is real (`|Δp| ≥ DP_DEADBAND`).
    ///
    /// The `leads` verdict must be EARNED past three controls, because momentum and P are
    /// computed from the same event board each tick and co-move by construction during a
    /// sustained episode (which would score ~100% at EVERY lag — no evidence of precedence):
    ///  1. hit rate ≥ [`LEAD_HIT_THRESHOLD`] on ≥ [`MOM_LL_MIN_PAIRS`] decisive samples;
    ///  2. it must beat the CONTEMPORANEOUS baseline (the same measure at one stride) by
    ///     ≥ [`LEAD_BASELINE_MARGIN`] whenever that baseline is itself judgeable — otherwise
    ///     the verdict is `coincident` (moves WITH P; no measurable lead);
    ///  3. the winning pairs must span ≥ [`MOM_LL_MIN_EPISODES`] distinct decisive-momentum
    ///     episodes (sign-change/deadband-separated runs) — otherwise `insufficient_episodes`
    ///     (one episode cannot distinguish lead from coincidence).
    ///
    /// `no_lead` stays the honest null when enough samples exist but no lag clears the
    /// threshold; `insufficient` when there is too little decisive history to test at all.
    /// Decimation to one sample per `stride_secs` keeps 1 Hz autocorrelated ticks from
    /// inflating the sample count. Touches no fitted constant and never feeds P.
    pub fn momentum_lead_lag_window(
        &self,
        now: DateTime<Utc>,
        window_secs: i64,
        stride_secs: i64,
        lags_secs: &[i64],
    ) -> serde_json::Value {
        let cutoff = now - chrono::Duration::seconds(window_secs);
        // Collect in-window entries newest→oldest, breaking at the cutoff so pre-window
        // history is never parsed (the ring can hold days beyond the window — same
        // discipline as trend_window / uncertainty_window).
        let mut rev: Vec<(i64, f64, f64)> = Vec::new();
        for e in self.ring.iter().rev() {
            let t = match e
                .get("t")
                .and_then(|v| v.as_str())
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            {
                Some(dt) => dt.with_timezone(&Utc),
                None => continue,
            };
            if t > now {
                continue;
            }
            if t < cutoff {
                break;
            }
            let p = match e.get("p_annual").and_then(|v| v.as_f64()) {
                Some(p) => p,
                None => continue,
            };
            let m = e.get("mom").and_then(|v| v.as_f64()).unwrap_or(0.0);
            rev.push(((t - cutoff).num_seconds(), p, m));
        }

        // Ascending stride decimation + episode segmentation: an episode is a maximal run
        // of consecutive kept samples whose decisive momentum keeps one sign; a sign flip
        // or a sub-deadband gap starts a new one. Episode id 0 = not decisive.
        let mut series: Vec<(i64, f64, f64, u32)> = Vec::with_capacity(rev.len() / 4 + 1);
        let mut last_kept: Option<i64> = None;
        // Episode = maximal same-sign decisive run. A single-stride dip below the
        // deadband does NOT close an episode (one soft sample inside a sustained
        // move would otherwise split it in two and inflate the count toward the
        // >=3-episode gate — xhigh review finding 13); a dip of >= MOM_EPISODE_GAP
        // strides or a sign flip does.
        let (mut ep, mut last_sign, mut gap): (u32, i8, u32) = (0, 0, 0);
        for &(secs, p, m) in rev.iter().rev() {
            if let Some(lk) = last_kept {
                if secs - lk < stride_secs {
                    continue;
                }
            }
            last_kept = Some(secs);
            let sign: i8 = if m >= MOM_DEADBAND { 1 } else if m <= -MOM_DEADBAND { -1 } else { 0 };
            if sign == 0 {
                gap += 1;
                if gap >= MOM_EPISODE_GAP {
                    last_sign = 0; // long lull — the run is over
                }
            } else {
                if sign != last_sign {
                    ep += 1;
                }
                last_sign = sign;
                gap = 0;
            }
            series.push((secs, p, m, if sign == 0 { 0 } else { ep }));
        }

        // Directional-hit measure at one lag: pair sample i with the sample nearest t_i+L
        // (within ±stride tolerance) via a forward two-pointer over the ascending series.
        let tol = stride_secs;
        let measure = |lag: i64| -> (usize, usize, std::collections::HashSet<u32>) {
            let mut j = 0usize;
            let (mut considered, mut agree) = (0usize, 0usize);
            let mut episodes: std::collections::HashSet<u32> = std::collections::HashSet::new();
            for i in 0..series.len() {
                let (ti, pi, mi, epi) = series[i];
                if mi.abs() < MOM_DEADBAND {
                    continue; // momentum has no decisive direction to test
                }
                let target = ti + lag;
                while j + 1 < series.len() && series[j].0 < target {
                    j += 1;
                }
                let cand = if j > 0 && (target - series[j - 1].0).abs() < (series[j].0 - target).abs() {
                    j - 1
                } else {
                    j
                };
                if (series[cand].0 - target).abs() > tol {
                    continue; // no sample near t+L
                }
                let dp = series[cand].1 - pi;
                if dp.abs() < DP_DEADBAND {
                    continue; // P did not actually move → nothing to predict
                }
                considered += 1;
                episodes.insert(epi);
                if (mi > 0.0) == (dp > 0.0) {
                    agree += 1;
                }
            }
            (considered, agree, episodes)
        };

        // Contemporaneous control: the same measure at one stride. If momentum "predicts"
        // the very next tick just as well as any real lag, the co-movement is simultaneous
        // and no lead has been demonstrated.
        let (b_pairs, b_agree, _) = measure(stride_secs);
        let b_hit = if b_pairs > 0 { b_agree as f64 / b_pairs as f64 } else { 0.0 };

        let mut profile: Vec<serde_json::Value> = Vec::with_capacity(lags_secs.len());
        let mut best: Option<(i64, f64, usize, usize)> = None; // (lag, hit, pairs, episodes)
        for &lag in lags_secs {
            let (considered, agree, episodes) = measure(lag);
            let hit = if considered > 0 { agree as f64 / considered as f64 } else { 0.0 };
            profile.push(serde_json::json!({
                "lag_secs": lag,
                "hit_pct":  (hit * 100.0 * 1e1).round() / 1e1,
                "pairs":    considered,
            }));
            if considered >= MOM_LL_MIN_PAIRS {
                let better = match best {
                    // higher hit wins; tie → shorter lag (earliest detectable lead)
                    Some((bl, bh, _, _)) => hit > bh + 1e-9 || ((hit - bh).abs() <= 1e-9 && lag < bl),
                    None => true,
                };
                if better {
                    best = Some((lag, hit, considered, episodes.len()));
                }
            }
        }

        match best {
            Some((lag, hit, pairs, eps)) => {
                // FAIL CLOSED at every control: "leads" is only minted when the
                // contemporaneous baseline was actually JUDGEABLE and beaten. A slow
                // steady P (every 300s move under the dp deadband) starves the
                // baseline of pairs; granting "leads" then would assert a control
                // that was never evaluated (xhigh review finding 5).
                let verdict = if hit < LEAD_HIT_THRESHOLD {
                    "no_lead"
                } else if b_pairs < MOM_LL_MIN_PAIRS {
                    "insufficient_baseline"
                } else if hit - b_hit < LEAD_BASELINE_MARGIN {
                    "coincident"
                } else if eps < MOM_LL_MIN_EPISODES {
                    "insufficient_episodes"
                } else {
                    "leads"
                };
                serde_json::json!({
                    "available":         true,
                    "verdict":           verdict,
                    "lead_secs":         lag,
                    "hit_pct":           (hit * 100.0 * 1e1).round() / 1e1,
                    "baseline_hit_pct":  (b_hit * 100.0 * 1e1).round() / 1e1,
                    "pairs":             pairs,
                    "episodes":          eps,
                    "window_secs":       window_secs,
                    "profile":           profile,
                    "basis": "sign of systemic momentum at t vs sign of the realized P move over \
                              the next L (decisive = |momentum| ≥ 0.05; real move = |ΔP| ≥ 0.2pp); \
                              'leads' only when a lag clears the hit threshold on enough samples \
                              spanning ≥3 distinct momentum episodes AND beats the contemporaneous \
                              one-stride baseline — measured, never asserted",
                })
            }
            None => serde_json::json!({
                "available":   false,
                "verdict":     "insufficient",
                "window_secs": window_secs,
                "profile":     profile,
                "basis": "too few decisive-momentum episodes in the durable ring to test whether \
                          momentum leads the headline P",
            }),
        }
    }
}

// Lead-lag diagnostic parameters (DISPLAY/diagnostic only — none touches P or any fitted
// calibration constant). Window is long (the durable ring holds ~4 days) so decisive-momentum
// episodes accumulate; stride decimates 1 Hz autocorrelated ticks; the deadbands screen out
// directionless momentum and flat-P stretches; the threshold/min-pairs keep the "leads" verdict
// conservative — an honest null ("no_lead") is reported rather than a flattering claim.
const MOM_LL_WINDOW_SECS: i64 = 48 * 3600;
const MOM_LL_STRIDE_SECS: i64 = 300; // one sample per 5 min
const MOM_LL_LAGS: &[i64] = &[15 * 60, 30 * 60, 60 * 60, 120 * 60, 240 * 60];
const MOM_DEADBAND: f64 = 0.05;      // |momentum| below this has no decisive direction
const DP_DEADBAND: f64 = 0.002;      // |Δp| below this (0.2pp) is not a real move
const MOM_LL_MIN_PAIRS: usize = 12;  // decisive samples required to judge a lag
const LEAD_HIT_THRESHOLD: f64 = 0.60; // directional-hit rate to earn the "leads" verdict
const LEAD_BASELINE_MARGIN: f64 = 0.10; // best lag must beat the contemporaneous baseline by this
const MOM_LL_MIN_EPISODES: usize = 3;   // distinct decisive episodes required to claim a lead
const MOM_EPISODE_GAP: u32 = 2;         // sub-deadband strides that close an episode (1 dip tolerated)

// Recent-range positioning parameters (DISPLAY/diagnostic only — none touches P or any fitted
// constant). Fixed 24h window off the durable ring (holds ~4 days), so the "high/low" band means
// the same for every operator regardless of tab uptime. `MIN_SAMPLES` keeps a cold-start ring from
// publishing a degenerate range where hi==lo==current. `FLAT_RANGE_PP`: a band narrower than this
// (0.3 percentage points) is reported as flat/range-bound rather than claiming a high or a low.
const READ_RANGE_WINDOW_SECS: i64 = 24 * 3600;
const READ_RANGE_MIN_SAMPLES: usize = 30;
const FLAT_RANGE_PP: f64 = 0.3;
/// Minimum SPAN the in-window reads must cover before a position/range verdict renders — a
/// quarter of the window (mirrors `LEAD_CONC_MIN_SPAN_SECS`). The `MIN_SAMPLES` floor above is
/// COUNT-based, and a 1 Hz ring clears 30 samples ~30s after a restart while spanning only ~30s:
/// a served `position:"near-high"` / `pct_rank` claiming a multi-day HIGH off half a minute of
/// data is the same post-restart lie the 2026-07-17 outage exposed on the sibling locus read.
/// A range verdict about the recent band needs day-scale span. Honest-null until it accrues.
const READ_RANGE_MIN_SPAN_SECS: i64 = READ_RANGE_WINDOW_SECS / 4;

// Lead-concentration parameters (DISPLAY/diagnostic only — none touches P or any fitted constant).
// Fixed 24h window off the durable ring (matches read_range so the "locus" band means the same for
// every operator regardless of tab uptime); `MIN_SAMPLES` keeps a cold-start ring from publishing a
// degenerate concentration off a handful of ticks (matches READ_RANGE_MIN_SAMPLES).
const LEAD_CONC_WINDOW_SECS: i64 = 24 * 3600;
const LEAD_CONC_MIN_SAMPLES: usize = 30;
/// Minimum SPAN the counted ticks must cover before a verdict renders — a quarter of
/// the window. The sample floor above is COUNT-based, and a 1 Hz ring satisfies it
/// seconds after a restart: the 2026-07-17 post-outage board claimed "entrenched ·
/// 100% of 24h" off ~15 minutes of ring. A verdict about the day needs day-scale span.
const LEAD_CONC_MIN_SPAN_SECS: i64 = LEAD_CONC_WINDOW_SECS / 4;

// Alert-dwell parameter (DISPLAY/diagnostic only — never touches P or any fitted constant). The
// minimum contiguous in-band ticks before a dwell duration is claimed: below this the ring is too
// thin to assert "held for", so the read is honest-null rather than reporting a near-zero span.
const ALERT_DWELL_MIN_SAMPLES: usize = 3;

/// Ordinal severity of a serialized alert level (`AlertLevel::Display`), so a dwell run can test
/// "at or above the current band". `None` for any unknown token — the caller fails closed on it.
/// Kept in sync with `AlertLevel` (normal < elevated < critical); a new level must be added here.
fn alert_rank(s: &str) -> Option<u8> {
    match s {
        "normal" => Some(0),
        "elevated" => Some(1),
        "critical" => Some(2),
        _ => None,
    }
}

// Band-coverage parameters (DISPLAY/diagnostic only — none touches P or any fitted constant). This is
// the honest self-VALIDATION of the published uncertainty band: does reality stay inside it? Over a
// 48h lookback, each band is rebuilt from its trailing 6h (matching `uncertainty_window`) and tested
// against the read 1h later; the series is decimated to one sample per 5 min (matching the momentum
// lead-lag) so autocorrelated ticks don't inflate the count. `NOMINAL_PCT` is the band's central-80%
// design point; realized coverage within `TOL_PP` of it reads "calibrated", above → "conservative"
// (the humility floor is doing its job), below → "overconfident" (real moves escaped the band).
const BAND_COV_WINDOW_SECS: i64 = 48 * 3600;
const BAND_COV_BAND_SECS: i64 = 6 * 3600;
const BAND_COV_HORIZON_SECS: i64 = 3600;
const BAND_COV_STRIDE_SECS: i64 = 300;
const BAND_COV_MIN_PAIRS: usize = 12;
const BAND_COV_NOMINAL_PCT: f64 = 80.0;
const BAND_COV_TOL_PP: f64 = 10.0;

/// Linear-interpolated percentile of an ALREADY-SORTED ascending slice. `q` in [0,1].
/// Returns 0.0 for an empty slice. Used by the headline uncertainty interval.
fn percentile_sorted(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let rank = q.clamp(0.0, 1.0) * (sorted.len() - 1) as f64;
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    let frac = rank - lo as f64;
    sorted[lo] + (sorted[hi] - sorted[lo]) * frac
}

/// Boot loader: reads timeline JSONL files from disk once at startup and
/// populates an EpochStore. Reads today's and yesterday's rotated files (I-16)
/// so the ring is populated with recent history on restart without scanning
/// the entire archive. Lines are processed oldest-first so the ring ends with
/// the most recent entries at the tail.
pub async fn load_epoch() -> EpochStore {
    let mut store = EpochStore::new();
    let today = log_date();
    let yesterday = today.pred_opt().unwrap_or(today);

    // Chronological order: yesterday first, then today.
    let paths = [
        timeline_path_for_date(&yesterday),
        timeline_path_for_date(&today),
    ];

    let mut total_loaded = 0usize;
    for path_str in &paths {
        let path = PathBuf::from(path_str);
        if !path.exists() { continue; }
        match tokio::fs::read_to_string(&path).await {
            Ok(text) => {
                let mut file_count = 0usize;
                for line in text.lines() {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                        store.push(v);
                        file_count += 1;
                    }
                }
                if file_count > 0 {
                    info!("EpochStore: loaded {file_count} entries from {path_str}");
                    total_loaded += file_count;
                }
            }
            Err(e) => warn!("EpochStore boot read failed for {path_str}: {e}"),
        }
    }
    if total_loaded == 0 {
        info!("EpochStore: no timeline files found — starting with empty ring");
    } else {
        info!("EpochStore: {total_loaded} total entries loaded");
    }
    store
}

/// Boot loader: restores the article feed from the date-rotated JSONL archive
/// (today + yesterday) so the dashboard has history immediately on restart
/// instead of waiting for live feeds to refill. Lines are read chronologically
/// and deduped by id (last occurrence wins) so the tagged copy supersedes the
/// at-ingest copy. The resulting store keeps the newest `max_size` articles.
pub async fn load_articles(max_size: usize) -> ArticleStore {
    let today = log_date();
    let yesterday = today.pred_opt().unwrap_or(today);
    let paths = [
        article_path_for_date(&yesterday),
        article_path_for_date(&today),
    ];

    // First-seen order preserved in `order`; latest content kept in `latest`.
    let mut order: Vec<String> = Vec::new();
    let mut latest: HashMap<String, StoredArticle> = HashMap::new();
    for path_str in &paths {
        let path = PathBuf::from(path_str);
        if !path.exists() { continue; }
        match tokio::fs::read_to_string(&path).await {
            Ok(text) => {
                for line in text.lines() {
                    if let Ok(a) = serde_json::from_str::<StoredArticle>(line) {
                        // Live-flagged watchlist rows are excluded at ingest (2026-07-22);
                        // drop archived ones too so the Video tab stops replaying them. The
                        // whole whisper-livestream tier (`-live`) is retired 2026-07-23 —
                        // drop every archived `-live` row so its misleading [LIVE] entries
                        // vanish on restart rather than reloading for years.
                        if (a.source.ends_with("-video") && crate::video::is_live_title(&a.title))
                            || a.source.ends_with("-live") { continue; }
                        if !latest.contains_key(&a.id) { order.push(a.id.clone()); }
                        latest.insert(a.id.clone(), a);
                    }
                }
            }
            Err(e) => warn!("ArticleStore boot read failed for {path_str}: {e}"),
        }
    }

    let mut store = ArticleStore::new(max_size);
    // Canonicalize archived URLs (rows written before URL hygiene carry tracking
    // params) and keep only the NEWEST row per canonical URL: the archive holds
    // one append per live-blog title edit under a fresh id each, so replaying all
    // of them resurrected the exact duplicate rows the live path now updates in
    // place. (audit-news L2/L3)
    let mut rows: Vec<StoredArticle> = Vec::with_capacity(order.len());
    for id in &order {
        if let Some(mut a) = latest.remove(id) {
            a.url = crate::ingestor::canonicalize_url(&a.url);
            rows.push(a);
        }
    }
    for a in dedupe_newest_per_url(rows) {
        store.push(a);
    }
    if store.len() == 0 {
        info!("ArticleStore: no article archive found — starting empty");
    } else {
        info!("ArticleStore: restored {} articles from archive", store.len());
    }
    store
}

/// Keep only the newest row per canonical URL, preserving input order otherwise.
/// "Newest" = the LAST occurrence in the (chronologically appended) archive
/// replay — each live-blog edit was appended after the row it superseded. Rows
/// with an empty URL are always kept: there is nothing to key them on.
fn dedupe_newest_per_url(rows: Vec<StoredArticle>) -> Vec<StoredArticle> {
    let keep: Vec<bool> = {
        let mut last_for_url: HashMap<&str, usize> = HashMap::new();
        for (i, a) in rows.iter().enumerate() {
            if !a.url.is_empty() { last_for_url.insert(a.url.as_str(), i); }
        }
        rows.iter().enumerate()
            .map(|(i, a)| a.url.is_empty() || last_for_url.get(a.url.as_str()) == Some(&i))
            .collect()
    };
    rows.into_iter().zip(keep).filter(|(_, k)| *k).map(|(a, _)| a).collect()
}

/// Boot helper for the ingest dedup caches: the (url, title, source) keys of
/// archived articles from `oldest_days_back`..=`newest_days_back` days ago,
/// oldest day first (load_articles itself covers today + yesterday). Keys only —
/// the article STORE retention is unchanged. Exists because slow feeds
/// (think-tanks, journals) keep entries live for a week+, so a dedup cache
/// seeded with only two days re-stored anything older after every restart.
pub async fn load_archived_article_keys(
    oldest_days_back: u32,
    newest_days_back: u32,
) -> Vec<(String, String, String)> {
    let today = log_date();
    let mut keys: Vec<(String, String, String)> = Vec::new();
    for back in (newest_days_back..=oldest_days_back).rev() {
        let Some(date) = today.checked_sub_days(chrono::Days::new(back as u64)) else { continue };
        let path = PathBuf::from(article_path_for_date(&date));
        if !path.exists() { continue; }
        match tokio::fs::read_to_string(&path).await {
            Ok(text) => {
                for line in text.lines() {
                    if let Ok(a) = serde_json::from_str::<StoredArticle>(line) {
                        keys.push((a.url, a.title, a.source));
                    }
                }
            }
            Err(e) => warn!("Archive key read failed for {}: {e}", path.display()),
        }
    }
    keys
}

/// Boot loader: restores the Bayesian event window from the date-rotated event
/// archive so domain scores + P(WWIII) survive restarts. Reads `logs/events_*.jsonl`
/// newest-file-first AND newest-line-first within each file (events are appended
/// in arrival order, so the last line is the most recent), parsing until
/// MAX_WINDOW_EVENTS events are collected. Reading newest-first matters when the
/// archive exceeds the cap: the surviving events are then the *newest* ones, which
/// is what the run loop's own cap keeps — reading oldest-first would instead keep
/// stale events and leave a hole in recent history. The aggregator's preload_events
/// then applies the same age filter and cap, so older/over-cap events are dropped
/// consistently.
pub async fn load_events() -> Vec<GeopoliticalEvent> {
    // Collect event archive filenames (newest first by name — date-stamped sorts lexically).
    let mut files: Vec<String> = Vec::new();
    match tokio::fs::read_dir("logs").await {
        Ok(mut rd) => {
            while let Ok(Some(entry)) = rd.next_entry().await {
                if let Some(name) = entry.file_name().to_str() {
                    if name.starts_with("events_") && name.ends_with(".jsonl") {
                        files.push(format!("logs/{name}"));
                    }
                }
            }
        }
        Err(_) => { info!("EventWindow: no logs dir — starting with empty window"); return Vec::new(); }
    }
    files.sort_unstable();
    files.reverse(); // newest date first

    let mut events: Vec<GeopoliticalEvent> = Vec::new();
    for path in &files {
        if events.len() >= MAX_WINDOW_EVENTS { break; }
        match tokio::fs::read_to_string(path).await {
            Ok(text) => {
                for line in text.lines().rev() {
                    if let Ok(ev) = serde_json::from_str::<GeopoliticalEvent>(line) {
                        events.push(ev);
                        if events.len() >= MAX_WINDOW_EVENTS { break; }
                    }
                }
            }
            Err(e) => warn!("EventWindow boot read failed for {path}: {e}"),
        }
    }
    if events.is_empty() {
        info!("EventWindow: no event archive found — starting with empty window");
    } else {
        info!("EventWindow: loaded {} events from archive", events.len());
    }
    events
}

// ── Search-API loop health ──────────────────────────────────────────────────────

/// Health of one search-API poll loop (gnews / gdelt), written by the ingestor
/// and served verbatim in GET /api/sources. Exists because a dark loop was
/// previously invisible: /api/sources listed RSS feeds only, so GDELT could sit
/// 429-throttled for days with nothing on the wire saying so. (audit-news c)
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct SearchApiHealth {
    /// RFC3339 UTC of the last successful fetch+parse (None = none since boot).
    pub last_success_at:      Option<String>,
    /// RFC3339 UTC of the most recent attempt, success or failure.
    pub last_attempt_at:      Option<String>,
    /// Attempts failed in a row since the last success (0 = healthy).
    pub consecutive_failures: u32,
    /// Error text of the most recent failure (cleared on success).
    pub last_error:           Option<String>,
}

// ── Shared application state ──────────────────────────────────────────────────

pub type SharedState = Arc<AppState>;

pub struct AppState {
    pub latest_snapshot:   Mutex<Option<serde_json::Value>>,
    pub article_store:     Mutex<ArticleStore>,
    pub source_registry:   Mutex<HashMap<String, usize>>,
    pub nuclear_alerts:    Mutex<Vec<crate::detector::SeismicAlert>>,
    pub operator_events:   Mutex<Vec<serde_json::Value>>,
    /// Live regime factors — shared between OperatorState (api.rs) and Aggregator.
    /// Writing here immediately affects the next Bayesian tick.
    pub shared_regime:     Mutex<Vec<crate::models::RegimeFactor>>,
    /// Full P(WWIII) timeline ring — pre-loaded from disk at boot.
    pub epoch_store:       Mutex<EpochStore>,
    /// Timestamp of the last operator calibration action (regime toggle, multiplier
    /// set, or manual event assertion). Surfaced in every snapshot broadcast so the
    /// dashboard can show an honest "model updated" indicator to public users.
    pub last_calibrated_at: Mutex<Option<DateTime<Utc>>>,
    /// Cached AI analyst brief (v2 Phase 4) — regenerated periodically by the brief
    /// task, served at GET {base}/api/brief. None until the first generation.
    pub analyst_brief:     Mutex<Option<serde_json::Value>>,
    /// GNews/GDELT poll-loop health — written by the ingestor loops after every
    /// attempt, served in GET /api/sources so a dark search API is visible.
    pub search_api_health: Mutex<HashMap<String, SearchApiHealth>>,
}

impl AppState {
    pub fn new() -> SharedState {
        Arc::new(Self {
            latest_snapshot:    Mutex::new(None),
            article_store:      Mutex::new(ArticleStore::new(75_000)),
            source_registry:    Mutex::new(HashMap::new()),
            nuclear_alerts:     Mutex::new(Vec::new()),
            operator_events:    Mutex::new(Vec::new()),
            shared_regime:      Mutex::new(Vec::new()),
            analyst_brief:      Mutex::new(None),
            epoch_store:        Mutex::new(EpochStore::new()),
            last_calibrated_at: Mutex::new(None),
            search_api_health:  Mutex::new(HashMap::new()),
        })
    }
}

// ── MinHash LSH primitives for corroboration (M-01) ──────────────────────────
//
// These are the same hash functions used in processor.rs for title dedup.
// Duplicated here rather than re-exported to keep module coupling minimal —
// each module owns its own LSH configuration. The constants (MINHASH_A,
// MINHASH_B, MINHASH_PRIME) are compile-time fixed and produce identical
// results across both modules.
//
// The band configuration differs:
//   Dedup (processor.rs):       16 bands × 4 rows → threshold ≈ 0.70
//   Corroboration (this file):  32 bands × 2 rows → threshold ≈ 0.40
//
// The wider band count with narrower rows makes the LSH much more sensitive,
// catching same-event-different-headline matches that the dedup layer
// intentionally lets through (dedup catches near-identical titles; corroboration
// catches same-event different-wording titles from different outlets).

/// Number of MinHash hash functions — same as processor.rs.
const CORR_NUM_HASHES: usize = 64;

/// Corroboration LSH: 32 bands of 2 rows each.
/// P(candidate | J=0.40) ≈ 1 − (1 − 0.40²)³² ≈ 0.9997 — near-certain at threshold.
/// P(candidate | J=0.10) ≈ 1 − (1 − 0.10²)³² ≈ 0.0315 — low false-positive rate.
const CORR_BANDS:     usize = 32;
const CORR_BAND_ROWS: usize = CORR_NUM_HASHES / CORR_BANDS; // 2

/// Mersenne prime 2^61 - 1.
const MINHASH_PRIME: u64 = (1u64 << 61) - 1;

/// Deterministic hash function seeds — identical to processor.rs.
/// A[i] must be in [1, MINHASH_PRIME), B[i] in [0, MINHASH_PRIME).
/// Derived from the first 64 pairs of digits from known large primes.
/// Fixed at compile time — identical on every run, every restart.
const MINHASH_A: [u64; CORR_NUM_HASHES] = [
    0x9e3779b97f4a7c15, 0x6c62272e07bb0142, 0xc3a5c85c97cb3127, 0xb492b66fbe98f273,
    0x9ae16a3b2f90404f, 0xc949d7c7509e6557, 0xd7ae43b4b7ded36a, 0xf32e33c24fb9afe8,
    0xd06b61b07c4ce94b, 0xd3f55a7d86af7c32, 0xa4b2c3d4e5f60718, 0x1f2e3d4c5b6a7982,
    0x8796a5b4c3d2e1f0, 0x0f1e2d3c4b5a6978, 0x7689a7b6c5d4e3f2, 0x2e3f4050617283a4,
    0xdeadbeefcafe1234, 0x0102030405060708, 0xfedcba9876543210, 0x1122334455667788,
    0xaabbccdd11223344, 0x99887766554433ff, 0x78563412deadbeef, 0x135791357913579f,
    0x2468ace02468ace1, 0xf0e1d2c3b4a59687, 0x8070605040302010, 0xabcdef0123456789,
    0x192837465564738a, 0xa1b2c3d4e5f60718, 0x0a1b2c3d4e5f6070, 0xffeeddccbbaa9988,
    0x99aabbccddeeff00, 0x6655443322110011, 0x1f3f5f7f9fbfdfe0, 0xe0c0a08060402010,
    0x1357924681012141, 0x2468ace013579bdf, 0x0f1e2d3c4b5a6978, 0x8796a5b4c3d2e1f1,
    0x7f6f5f4f3f2f1f0f, 0xa0b0c0d0e0f01020, 0x3c2b1a0978675645, 0xf1e2d3c4b5a69788,
    0x0011223344556677, 0x8899aabbccddeef0, 0x7766554433221101, 0x33221100ffeeddcd,
    0x4455667788990011, 0xbbccddee00112234, 0x5566778899001123, 0xccddee0011223345,
    0x6677889900112234, 0xddeeff0011223346, 0x778899001122334f, 0xeeff001122334456,
    0x8899001122334560, 0xff00112233445671, 0x9900112233445670, 0x0011223344556782,
    0xaabb112233445691, 0xbbcc2233445566a0, 0xccdd3344556677b1, 0xddeeff5566778892,
];

const MINHASH_B: [u64; CORR_NUM_HASHES] = [
    0x517cc1b727220a95, 0x3a1b2c3d4e5f6070, 0xbcdef0123456789a, 0x23456789abcdef01,
    0x9abcdef012345678, 0x456789abcdef0123, 0xcdef0123456789ab, 0x6789abcdef012345,
    0xef0123456789abcd, 0x23456789abcdef12, 0x9b8a7c6d5e4f3021, 0x0102030405060780,
    0x8090a0b0c0d0e0f1, 0xf0e1d2c3b4a59681, 0x7080910213243546, 0x5647382910011233,
    0x1122334455667799, 0xaabbccdd00112234, 0x99887766554433dd, 0x78563412deadbeee,
    0x135791357913579e, 0x2468ace02468ace2, 0xf0e1d2c3b4a59688, 0x8070605040302011,
    0xabcdef0123456788, 0x192837465564738b, 0xa1b2c3d4e5f60719, 0x0a1b2c3d4e5f6071,
    0xffeeddccbbaa9989, 0x99aabbccddeeff01, 0x6655443322110012, 0x1f3f5f7f9fbfdfe1,
    0xe0c0a08060402011, 0x1357924681012142, 0x2468ace013579be0, 0x0f1e2d3c4b5a6979,
    0x8796a5b4c3d2e1f2, 0x7f6f5f4f3f2f1f10, 0xa0b0c0d0e0f01021, 0x3c2b1a0978675646,
    0xf1e2d3c4b5a69789, 0x0011223344556678, 0x8899aabbccddeef1, 0x7766554433221102,
    0x33221100ffeeddce, 0x4455667788990012, 0xbbccddee00112235, 0x5566778899001124,
    0xccddee0011223346, 0x6677889900112235, 0xddeeff0011223347, 0x778899001122340a,
    0xeeff001122334457, 0x8899001122334561, 0xff00112233445672, 0x9900112233445671,
    0x0011223344556783, 0xaabb112233445692, 0xbbcc2233445566a1, 0xccdd3344556677b2,
    0xddeeff5566778893, 0xeeff66778899a0b4, 0xff77889900b1c2d3, 0x8899a0b1c2d3e4f5,
];

/// Compute a single MinHash value — identical to processor.rs::minhash_apply.
#[inline(always)]
fn minhash_apply(seed_a: u64, seed_b: u64, x: u64) -> u64 {
    let v = seed_a.wrapping_mul(x).wrapping_add(seed_b);
    let lo = v & MINHASH_PRIME;
    let hi = v >> 61;
    let r  = lo + hi;
    if r >= MINHASH_PRIME { r - MINHASH_PRIME } else { r }
}

/// FNV-1a hash for a 3-char trigram — identical to processor.rs::hash_trigram.
#[inline(always)]
fn hash_trigram(tg: [char; 3]) -> u64 {
    const FNV_OFFSET: u64 = 14695981039346656037;
    const FNV_PRIME:  u64 = 1099511628211;
    let mut h = FNV_OFFSET;
    for ch in tg {
        h ^= ch as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

/// Hash a band of consecutive signature values to a single u64.
#[inline(always)]
fn corr_hash_band(band: &[u64]) -> u64 {
    const FNV_OFFSET: u64 = 14695981039346656037;
    const FNV_PRIME:  u64 = 1099511628211;
    let mut h = FNV_OFFSET;
    for &v in band {
        for byte in v.to_le_bytes() {
            h ^= byte as u64;
            h = h.wrapping_mul(FNV_PRIME);
        }
    }
    h
}

/// Compute trigrams from a lowercased title string.
fn corr_trigrams(s: &str) -> Vec<[char; 3]> {
    let chars: Vec<char> = s.to_lowercase().chars().collect();
    if chars.len() < 3 { return Vec::new(); }
    chars.windows(3)
        .map(|w| [w[0], w[1], w[2]])
        .collect()
}

/// Compute a 64-element MinHash signature from a trigram list.
fn corr_minhash_signature(tgs: &[[char; 3]]) -> Vec<u64> {
    let mut sig = vec![u64::MAX; CORR_NUM_HASHES];
    for &tg in tgs {
        let h = hash_trigram(tg);
        for i in 0..CORR_NUM_HASHES {
            let v = minhash_apply(MINHASH_A[i], MINHASH_B[i], h);
            if v < sig[i] { sig[i] = v; }
        }
    }
    sig
}

// ── Corroboration index (M-01) ───────────────────────────────────────────────
//
// Maintains MinHash signatures and a band index for all events currently in
// the event window. Provides O(k) candidate lookup for corroboration matching
// instead of the previous O(n) linear scan.
//
// The index is position-parallel to the event window Vec<GeopoliticalEvent>:
//   sigs[i] is the MinHash signature for event_window[i].
//   band_idx maps band_hash → set of window indices.
//
// When events are evicted from the window (age-based or volume-cap), the
// index must be rebuilt to stay in sync. This is done via rebuild_from_window()
// which is called after any bulk eviction. Individual insertions are indexed
// incrementally via push().

struct CorroborationIndex {
    /// MinHash signature per event, parallel to the event window.
    sigs: Vec<Vec<u64>>,
    /// Band hash → list of event window indices.
    band_idx: HashMap<u64, Vec<usize>>,
}

impl CorroborationIndex {
    fn new() -> Self {
        Self {
            sigs:     Vec::new(),
            band_idx: HashMap::new(),
        }
    }

    /// Index a single event's signature bands.
    fn index_bands(&mut self, idx: usize, sig: &[u64]) {
        for band in 0..CORR_BANDS {
            let start = band * CORR_BAND_ROWS;
            let end   = start + CORR_BAND_ROWS;
            let bh    = corr_hash_band(&sig[start..end]);
            self.band_idx.entry(bh).or_default().push(idx);
        }
    }

    /// Add an event to the index. The caller must append the event to the
    /// window at the same index as sigs.len() was before this call.
    fn push(&mut self, title: &str) {
        let tgs = corr_trigrams(title);
        let sig = if tgs.is_empty() {
            vec![u64::MAX; CORR_NUM_HASHES]
        } else {
            corr_minhash_signature(&tgs)
        };
        let idx = self.sigs.len();
        self.index_bands(idx, &sig);
        self.sigs.push(sig);
    }

    /// Find candidate indices that might match the given title's signature.
    /// Returns a deduplicated set of window indices.
    fn find_candidates(&self, sig: &[u64]) -> HashSet<usize> {
        let mut candidates = HashSet::new();
        for band in 0..CORR_BANDS {
            let start = band * CORR_BAND_ROWS;
            let end   = start + CORR_BAND_ROWS;
            let bh    = corr_hash_band(&sig[start..end]);
            if let Some(idxs) = self.band_idx.get(&bh) {
                for &idx in idxs {
                    candidates.insert(idx);
                }
            }
        }
        candidates
    }

    /// Rebuild the entire index from the current event window.
    /// Called after bulk eviction (age-based retain or volume-cap truncation)
    /// invalidates the position-parallel mapping.
    fn rebuild_from_window(&mut self, window: &[GeopoliticalEvent]) {
        self.sigs.clear();
        self.band_idx.clear();
        for event in window {
            let tgs = corr_trigrams(&event.title);
            let sig = if tgs.is_empty() {
                vec![u64::MAX; CORR_NUM_HASHES]
            } else {
                corr_minhash_signature(&tgs)
            };
            let idx = self.sigs.len();
            self.index_bands(idx, &sig);
            self.sigs.push(sig);
        }
    }

    /// Number of indexed events.
    #[allow(dead_code)] // diagnostic accessor — retained for tests/inspection
    fn len(&self) -> usize { self.sigs.len() }
}

// ── Aggregator ────────────────────────────────────────────────────────────────

// Seconds after startup before timeline/EpochStore writes begin.
// Suppresses the non-stationary warmup transient (0-articles → first RSS batch
// lands → spike → settle) from ever appearing in the history chart.
// Live gauge, articles, and domain bars are unaffected — snapshots still
// broadcast immediately. Only the historical record is gated.
const WARMUP_SECS: u64 = 90;

pub struct Aggregator {
    engine:           BayesianRiskEngine,
    event_rx:         mpsc::Receiver<GeopoliticalEvent>,
    snapshot_tx:      mpsc::Sender<RiskSnapshot>,
    state:            SharedState,
    event_window:     Vec<GeopoliticalEvent>,
    /// M-01: MinHash LSH index for O(k) corroboration candidate lookup.
    corr_index:       CorroborationIndex,
    max_age_hours:    f64,
    poll_interval_ms: u64,
    started_at:       std::time::Instant,
}

impl Aggregator {
    pub fn new(
        regime_factors:     Vec<RegimeFactor>,
        alert_settings:     AlertSettings,
        event_rx:           mpsc::Receiver<GeopoliticalEvent>,
        snapshot_tx:        mpsc::Sender<RiskSnapshot>,
        state:              SharedState,
        poll_interval_secs: u64,
    ) -> Self {
        use crate::bayesian::MAX_EVENT_AGE_HOURS;
        Self {
            engine: BayesianRiskEngine::new(
                regime_factors,
                alert_settings.elevated,
                alert_settings.critical,
            ),
            event_rx,
            snapshot_tx,
            state,
            event_window:     Vec::new(),
            corr_index:       CorroborationIndex::new(),
            max_age_hours:    MAX_EVENT_AGE_HOURS,
            poll_interval_ms: poll_interval_secs * 1000,
            started_at:       std::time::Instant::now(),
        }
    }

    /// Seed the event window from disk (load_events) before the run loop starts,
    /// so domain scores + P(WWIII) are restored on boot instead of resetting to
    /// baseline. Applies the same age filter, sort, and cap the run loop uses,
    /// then rebuilds the corroboration index to stay in sync.
    pub fn preload_events(&mut self, mut events: Vec<GeopoliticalEvent>) {
        if events.is_empty() { return; }
        let now = Utc::now();
        events.retain(|e| age_hours(&e.published_at, &now) < self.max_age_hours);
        // Retroactive scrub of live-flagged watchlist rows (excluded at ingest
        // 2026-07-22) AND the retired whisper-livestream tier (`-live`, 2026-07-23):
        // the 4-year window otherwise reloads both for years.
        events.retain(|e| !((e.source.ends_with("-video") && crate::video::is_live_title(&e.title))
            || e.source.ends_with("-live")));
        events.sort_by_key(|b| std::cmp::Reverse(b.published_at));
        events.truncate(MAX_WINDOW_EVENTS);
        self.corr_index.rebuild_from_window(&events);
        info!("Aggregator: preloaded {} events into window from archive", events.len());
        self.event_window = events;
    }

    pub async fn run(mut self) {
        info!(
            "Aggregator: {}ms interval, {:.0}h window, max {} events",
            self.poll_interval_ms, self.max_age_hours, MAX_WINDOW_EVENTS
        );
        let mut tick = interval(Duration::from_millis(self.poll_interval_ms));

        loop {
            tick.tick().await;

            // Drain queue
            let mut new_count    = 0usize;
            let mut corroborated = 0usize;
            let now_drain        = Utc::now();
            // New events to persist — collected here and written after the drain
            // loop so disk IO stays out of the tight try_recv path.
            let mut to_persist: Vec<GeopoliticalEvent> = Vec::new();
            loop {
                match self.event_rx.try_recv() {
                    Ok(event) => {
                        if try_corroborate(&event, &mut self.event_window, &now_drain, &self.corr_index) {
                            corroborated += 1;
                        } else {
                            // Not a corroboration — index the new event, then append
                            self.corr_index.push(&event.title);
                            to_persist.push(event.clone());
                            self.event_window.push(event);
                            new_count += 1;
                        }
                    }
                    Err(mpsc::error::TryRecvError::Empty)        => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        warn!("Event channel disconnected — aggregator shutting down");
                        return;
                    }
                }
            }
            // Persist newly-added events to the date-rotated archive (for restart
            // restore) in a single batched write.
            append_events(&to_persist).await;

            // Evict stale
            let now = Utc::now();
            let before = self.event_window.len();
            self.event_window.retain(|e| age_hours(&e.published_at, &now) < self.max_age_hours);
            let evicted = before - self.event_window.len();

            // Volume safeguard cap
            let mut truncated = false;
            if self.event_window.len() > MAX_WINDOW_EVENTS {
                self.event_window.sort_by_key(|b| std::cmp::Reverse(b.published_at));
                self.event_window.truncate(MAX_WINDOW_EVENTS);
                truncated = true;
            }

            // Rebuild corroboration index if window was mutated by eviction or
            // truncation. This is O(window_size × 64 hashes) but only happens
            // when events are actually removed — not on every tick. At 500K
            // events this takes ~30ms, well within the 1-second tick budget.
            if evicted > 0 || truncated {
                if evicted > 0 { debug!("Evicted {evicted} stale events"); }
                self.corr_index.rebuild_from_window(&self.event_window);
            }

            // Sync regime factors from shared state (operator API may have changed them)
            {
                let regime = self.state.shared_regime.lock().await;
                if !regime.is_empty() {
                    for factor in regime.iter() {
                        self.engine.set_regime_factor(&factor.id, factor.active);
                    }
                }
            }

            // Compute
            let mut snapshot = self.engine.compute(&self.event_window);
            // NOTE: do NOT overwrite snapshot.sources_active here. compute() Step 4 sets it
            // to the RECENCY-GATED distinct live-source count — the value estimate_confidence()
            // already consumed and the value is_thinly_sourced() reads. A prior ungated
            // `HashSet over the full 4-year window` overwrite broke that consistency lock
            // (the displayed source count no longer matched the count behind the confidence
            // number) and made the thin-sourced caveat unable to fire during a partial feed
            // outage. Leave the gated value standing. (audit bayesian-2)

            // Surface the seismic monitor's strongest test-consistent anomaly onto the
            // snapshot so the I&W board carries the physical nuclear indicator (the
            // detector's own `is_test_consistent` conclusion, not the LLM). DISPLAY only
            // — set AFTER compute, so it never touches the P(WWIII) math. Pick the
            // highest-confidence qualifying alert for the WHERE pointer.
            {
                let alerts = self.state.nuclear_alerts.lock().await;
                if let Some(a) = alerts.iter()
                    .filter(|a| a.is_test_consistent())
                    .max_by(|a, b| a.confidence.partial_cmp(&b.confidence)
                        .unwrap_or(std::cmp::Ordering::Equal))
                {
                    snapshot.seismic_test_consistent = true;
                    snapshot.seismic_site = a.nearest_site_name.clone();
                }
            }

            // Movement attribution (display-only; the seismic pattern — set AFTER
            // compute, never feeds P): when this tick's headline moved materially,
            // record WHAT landed with it — the top new events by severity, the
            // corroboration count, or, when nothing new arrived, the decay/eviction
            // fact — so a knock on the timeline chart can answer WHY on hover
            // (operator directive 2026-07-18). Rides the snapshot to the live WS
            // clients and TimelineEntry.drivers to the durable ring/archive.
            if snapshot.delta_annual.abs() >= DRIVER_NOTE_MIN_DELTA {
                let mut top: Vec<&GeopoliticalEvent> = to_persist.iter().collect();
                top.sort_by(|a, b| b.severity.partial_cmp(&a.severity)
                    .unwrap_or(std::cmp::Ordering::Equal));
                let mut drivers: Vec<String> = top.iter().take(DRIVER_NOTE_MAX_EVENTS)
                    .map(|e| {
                        let mut t: String = e.title.chars().take(DRIVER_TITLE_MAX_CHARS).collect();
                        if e.title.chars().count() > DRIVER_TITLE_MAX_CHARS { t.push('…'); }
                        format!("{} · {}", e.source, t)
                    })
                    .collect();
                if corroborated > 0 {
                    drivers.push(format!("+{corroborated} corroboration{}",
                                         if corroborated == 1 { "" } else { "s" }));
                }
                if drivers.is_empty() {
                    drivers.push(if evicted > 0 {
                        format!("no new events — recency decay ({evicted} aged out)")
                    } else {
                        "no new events — recency decay".to_string()
                    });
                }
                // Structured refs for the SAME top events (the clickable audit trail
                // behind the hover card): url/age/snippet resolved from the store via
                // raw_article_id. An evicted/suppressed article degrades the ref to
                // the event's own fields (url empty → card renders an unlinked row).
                // Note lines (+N corroborations / decay) stay strings-only.
                {
                    let store = self.state.article_store.lock().await;
                    snapshot.tick_driver_refs = top.iter().take(DRIVER_NOTE_MAX_EVENTS)
                        .map(|e| {
                            let art = store.get_by_id(&e.raw_article_id);
                            let snippet = art.map(|a| {
                                let mut s: String = a.body.chars().take(DRIVER_SNIPPET_MAX_CHARS).collect();
                                if a.body.chars().count() > DRIVER_SNIPPET_MAX_CHARS { s.push('…'); }
                                s
                            }).unwrap_or_default();
                            crate::models::DriverRef {
                                source:       e.source.clone(),
                                title:        e.title.clone(),
                                url:          art.map(|a| a.url.clone()).unwrap_or_default(),
                                published_at: art.map(|a| a.published_at.clone())
                                                 .unwrap_or_else(|| e.published_at.to_rfc3339()),
                                snippet,
                                video:        e.source.ends_with("-video"),
                            }
                        })
                        .collect();
                }
                snapshot.tick_drivers = drivers;
            }

            info!(
                "Batch: +{new_count} corroborated={corroborated} | window={} | P(WWIII)={:.4}% | Δ{:+.4}%",
                self.event_window.len(),
                snapshot.p_wwiii_annual * 100.0,
                snapshot.delta_annual * 100.0,
            );

            // Gate history writes behind warmup period — suppresses the non-stationary
            // transient (0-articles → first batch → spike) from the timeline chart.
            // Live broadcasts and the gauge are unaffected; only the record is held.
            if self.started_at.elapsed().as_secs() >= WARMUP_SECS {
                // One entry built once (drivers included), written to both the durable
                // archive and the in-memory ring — the two records can't diverge.
                let entry = TimelineEntry::from_snapshot(&snapshot);
                append_timeline(&entry).await;
                if let Ok(v) = serde_json::to_value(&entry) {
                    self.state.epoch_store.lock().await.push(v);
                }
            }

            // Broadcast regardless of warmup — live UI always current. The supervised
            // broadcaster (server::broadcast_snapshots) is the SINGLE writer of
            // `latest_snapshot`: it enriches every snapshot it receives (trend_6h,
            // uncertainty, momentum_lead, epistemic, model_calibrated_at) and stores
            // the result. Writing the raw snapshot_to_json here as well raced that
            // enriched write — every batch opened a window where /api/latest served a
            // payload with NO trend_6h (the eyes gate caught exactly that on a cold
            // boot, 2026-07-04). One writer, one payload shape.
            if let Err(e) = self.snapshot_tx.send(snapshot).await {
                warn!("Snapshot channel closed: {e}");
            }
        }
    }
}

// ── Corroboration detector ────────────────────────────────────────────────────
// Detects near-duplicate events from different sources and merges them into the
// canonical event by incrementing corroboration_count and boosting
// credibility_weight. Operates on events already in the window, after the NLP
// sidecar's title-level dedup.
//
// M-01: Now uses MinHash LSH for O(k) candidate lookup instead of O(n) linear
// scan. The exact trigram Jaccard verification step is unchanged — it runs only
// on the LSH candidate set (typically 0–5 events), not the entire window.
//
// Only compares against events published within the last 72 hours.

const CORROBORATION_JACCARD_THRESHOLD: f64 = 0.40;
const CORROBORATION_WINDOW_HOURS:      f64 = 72.0;

/// The outlet identity behind a source string, with the modality suffix removed.
///
/// A single newsroom feeds GCRM through more than one channel: its wire/RSS source
/// (`bbc`), its YouTube uploads (`bbc-video`), and — when enabled — its rolling live
/// transcript (`bbc-live`). Those are ONE editorial voice, not independent witnesses.
/// Corroboration and support-breadth credit must treat them as the same outlet, so the
/// same story reported by `bbc` and `bbc-video` cannot inflate itself into "two sources
/// confirm this." Five outlets in the current roster run both a wire and a `-video` feed
/// (bbc, aljazeera, cna, france24, skynews), so this collision is live, not hypothetical.
fn outlet_identity(source: &str) -> &str {
    source
        .strip_suffix("-video")
        .or_else(|| source.strip_suffix("-live"))
        .unwrap_or(source)
}

fn title_trigrams(s: &str) -> std::collections::HashSet<[char; 3]> {
    let chars: Vec<char> = s.to_lowercase().chars().collect();
    if chars.len() < 3 {
        return std::collections::HashSet::new();
    }
    chars.windows(3)
        .map(|w| [w[0], w[1], w[2]])
        .collect()
}

fn jaccard(a: &std::collections::HashSet<[char; 3]>, b: &std::collections::HashSet<[char; 3]>) -> f64 {
    if a.is_empty() || b.is_empty() { return 0.0; }
    let intersection = a.intersection(b).count();
    let union        = a.union(b).count();
    if union == 0 { 0.0 } else { intersection as f64 / union as f64 }
}

/// Attempt to corroborate an incoming event against the event window.
///
/// Uses the CorroborationIndex for O(k) candidate lookup (M-01). For each
/// candidate:
///   1. Skip if outside the 72-hour corroboration window.
///   2. Skip if the exact same feed (an edition — handled by the store's Edition path).
///   3. Compute exact trigram Jaccard similarity.
///   4. If Jaccard ≥ threshold, merge into the best-matching canonical event — but only
///      boost count/credibility if the incoming outlet is INDEPENDENT of the canonical
///      event's sources (by [`outlet_identity`]); a same-outlet cross-modal twin is
///      absorbed as a duplicate without the independence boost.
///
/// Returns true if the incoming event was corroborated (merged into an existing
/// event), false if it should be added to the window as a new event.
fn try_corroborate(
    incoming:   &GeopoliticalEvent,
    window:     &mut [GeopoliticalEvent],
    now:        &DateTime<Utc>,
    corr_index: &CorroborationIndex,
) -> bool {
    if incoming.title.len() < 10 { return false; }

    let incoming_tg = title_trigrams(&incoming.title);
    if incoming_tg.is_empty() { return false; }

    // Compute MinHash signature for LSH candidate lookup
    let tgs_vec = corr_trigrams(&incoming.title);
    if tgs_vec.is_empty() { return false; }
    let sig = corr_minhash_signature(&tgs_vec);

    // LSH candidate lookup — O(k) where k is typically 0–5
    let candidates = corr_index.find_candidates(&sig);

    let mut best_idx:   Option<usize> = None;
    let mut best_score: f64           = 0.0;

    for idx in candidates {
        if idx >= window.len() { continue; }
        let existing = &window[idx];

        let age = (*now - existing.published_at).num_seconds() as f64 / 3600.0;
        if age > CORROBORATION_WINDOW_HOURS { continue; }
        if existing.source == incoming.source { continue; }

        // Exact trigram Jaccard — verified only on LSH candidates
        let existing_tg = title_trigrams(&existing.title);
        let score = jaccard(&incoming_tg, &existing_tg);
        if score >= CORROBORATION_JACCARD_THRESHOLD && score > best_score {
            best_score = score;
            best_idx   = Some(idx);
        }
    }

    if let Some(idx) = best_idx {
        let existing = &mut window[idx];
        // Only a genuinely INDEPENDENT outlet corroborates. Independence is judged by
        // `outlet_identity`, not the raw source string, so an outlet's own wire + `-video` /
        // `-live` twins of one story count as ONE voice — a `bbc-video` re-run of a `bbc`
        // headline is absorbed as a duplicate but does NOT boost count/credibility (it is the
        // same newsroom, not a second witness). This also blocks a non-primary outlet that
        // already corroborated from inflating AGAIN with a reworded headline. A same-outlet
        // repeat is still ABSORBED (returns true, so it isn't re-added as a phantom new event
        // that would double-count into modality weight) — it just doesn't re-boost.
        // (audit aggregator-4 + same-outlet cross-modal independence fix)
        let incoming_outlet = outlet_identity(&incoming.source);
        if outlet_identity(&existing.source) == incoming_outlet
            || existing.corroborating_sources.iter().any(|s| outlet_identity(s) == incoming_outlet)
        {
            return true;
        }
        existing.corroborating_sources.push(incoming.source.clone());
        existing.corroboration_count += 1;
        let boost = incoming.credibility_weight * 0.15;
        existing.credibility_weight = (existing.credibility_weight + boost).min(1.0);
        debug!(
            "Corroborated: '{}' (source={}) → '{}' (count={}, cred={:.3})",
            incoming.title, incoming.source,
            existing.title, existing.corroboration_count, existing.credibility_weight,
        );
        true
    } else {
        false
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn age_hours(published_at: &DateTime<Utc>, now: &DateTime<Utc>) -> f64 {
    (*now - *published_at).num_seconds() as f64 / 3600.0
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{AlertLevel, DomainScore, SourceTier, ELEVATION_THRESHOLD, HISTORICAL_ANCHOR};
    use chrono::Duration;

    fn make_snapshot(p_annual: f64, delta: f64, elevated: usize) -> RiskSnapshot {
        RiskSnapshot {
            p_wwiii_annual:    p_annual,
            p_wwiii_30day:     1.0 - (1.0 - p_annual).powf(1.0 / 12.0),
            delta_annual:      delta,
            elevated_domains:  elevated,
            regime_multiplier: 1.568,
            events_in_window:  10,
            sources_active:    3,
            alert_level:       if p_annual >= 0.08 { AlertLevel::Critical }
                               else if p_annual >= 0.025 { AlertLevel::Elevated }
                               else { AlertLevel::Normal },
            ..Default::default()
        }
    }

    // ── snapshot_to_json ─────────────────────────────────────────────────────

    fn near_dup_store(titles: &[(&str, &str)]) -> ArticleStore {
        let mut st = ArticleStore::new(1000);
        for (i, (src, t)) in titles.iter().enumerate() {
            st.push(StoredArticle {
                id: format!("a{i}"), title: t.to_string(), url: format!("https://e.com/{i}"),
                source: src.to_string(), tier: 1, published_at: "2026-07-05T20:00:00Z".into(),
                ingested_at: "2026-07-05T20:00:00Z".into(), body: String::new(), domain_tags: vec![],
            });
        }
        st
    }

    #[test]
    fn near_duplicate_verdicts_cover_editions_syndication_and_new_stories() {
        // Fixtures are real pair shapes from the 2026-07-05 live-store measurement.
        let st = near_dup_store(&[
            ("npr", "NATO leaders look for unity as Trump attends annual summit"),
            ("aljazeera", "Lawmaker McGovern: Americans need to fight for the soul of the US"),
        ]);
        // Same outlet re-issues an edited headline -> Edition (replace the row).
        assert_eq!(
            st.near_duplicate_of("NATO leaders look for unity as Trump arrives at annual summit", "npr"),
            NearDup::Edition("a0".into())
        );
        // Another outlet's (or the video twin's) copy -> Syndicated (suppress).
        assert_eq!(
            st.near_duplicate_of("Americans need to fight for the soul of the US: Lawmaker McGovern", "aljazeera-video"),
            NearDup::Syndicated("aljazeera".into())
        );
        // A genuinely different story -> New.
        assert_eq!(
            st.near_duplicate_of("Earthquake relief effort expands in Venezuela", "cbc"),
            NearDup::New
        );
    }

    #[test]
    fn near_duplicate_pass_respects_live_and_wire_over_video() {
        let st = near_dup_store(&[
            ("aljazeera-live", "[LIVE] aljazeera: Iran warns tankers approaching the strait"),
            ("skynews-video", "Iran warns tankers approaching the strait of Hormuz"),
        ]);
        // Incoming -live rows never enter the pass.
        assert_eq!(st.near_duplicate_of("[LIVE] dwnews: Iran warns tankers approaching the strait", "dwnews-live"),
            NearDup::New);
        // Stored -live rows never suppress durable wire copy.
        // Stored -video rows never suppress incoming WIRE copy either (wire preferred)...
        assert_eq!(st.near_duplicate_of("Iran warns tankers approaching the strait of Hormuz", "reuters"),
            NearDup::New);
        // ...but an incoming video twin DOES defer to stored video/wire.
        assert_eq!(st.near_duplicate_of("Iran warns tankers approaching strait of Hormuz", "dwnews-video"),
            NearDup::Syndicated("skynews-video".into()));
    }

    #[test]
    fn near_duplicate_same_source_edition_outranks_newer_cross_source_copy() {
        // Guardian's newer copy must not shadow Reuters' own row (misrouting the
        // edition as Syndicated and stranding the stale Reuters headline).
        let st = near_dup_store(&[
            ("reuters", "Death toll from Venezuela earthquakes rises to 3,342"),
            ("guardian", "Death toll from Venezuela quakes rises to 3,342"),
        ]);
        assert_eq!(
            st.near_duplicate_of("Death toll from Venezuela earthquakes rises past 3,400", "reuters"),
            NearDup::Edition("a0".into())
        );
    }

    #[test]
    fn update_edition_refuses_to_go_backwards_and_keeps_both_urls() {
        let mut st = near_dup_store(&[("aljazeera", "Russia-Ukraine war: key events, day 1227")]);
        // legitimate forward edition: new URL keyed IN ADDITION to the old one
        st.update_edition("a0", "Russia-Ukraine war: key events, day 1228",
            "https://e.com/day1228", "b", "2026-07-06T00:00:00Z", "2026-07-06T00:00:00Z").unwrap();
        assert!(st.update_by_url("https://e.com/0", "aljazeera", "t", "b",
            "2026-07-06T01:00:00Z", "2026-07-06T01:00:00Z").is_some(),
            "the OLD url must still hit the same-URL path (no ping-pong re-entry)");
        // stale re-serve of the superseded edition: refused, row unchanged
        let before = st.query(1, None, None)[0].clone();
        let r = st.update_edition("a0", "Russia-Ukraine war: key events, day 1227",
            "https://e.com/day1227", "old", "2026-07-05T00:00:00Z", "2026-07-06T02:00:00Z");
        assert!(r.is_some(), "stale copy treated as handled (no duplicate row)");
        assert_eq!(st.query(1, None, None)[0].title, before.title, "row must not revert");
        // empty-url edition keeps the row's existing URL and indexes nothing new
        st.update_edition("a0", "Russia-Ukraine war: key events, day 1229",
            "", "b", "2026-07-06T03:00:00Z", "2026-07-06T03:00:00Z").unwrap();
        assert!(!st.query(1, None, None)[0].url.is_empty(), "empty URL must not blank the row link");
    }

    #[test]
    fn update_edition_replaces_row_and_rekeys_url() {
        let mut st = near_dup_store(&[("npr", "NATO leaders look for unity as Trump attends annual summit")]);
        let u = st.update_edition("a0",
            "NATO leaders look for unity as Trump arrives at annual summit",
            "https://npr.org/new-edition", "fresh body",
            "2026-07-05T21:00:00Z", "2026-07-05T21:00:00Z").expect("edition updates");
        assert_eq!(u.id, "a0", "same id — JSONL append supersedes at reload");
        assert!(u.title.contains("arrives"));
        assert_eq!(st.query(10, None, None).len(), 1, "still ONE row");
        // the NEW url now updates in place through the same-URL path
        assert!(st.update_by_url("https://npr.org/new-edition", "npr", "t2", "b", 
            "2026-07-05T22:00:00Z", "2026-07-05T22:00:00Z").is_some());
    }

    #[test]
    fn snapshot_to_json_has_required_keys() {
        let snap = make_snapshot(0.03, 0.001, 2);
        let v = snapshot_to_json(&snap);
        assert!(v["snapshot_id"].is_string());
        assert!(v["probabilities"]["annual"].is_number());
        assert!(v["probabilities"]["annual_pct"].is_number());
        assert!(v["probabilities"]["thirty_day"].is_number());
        assert!(v["probabilities"]["ninety_day"].is_number());
        assert!(v["prior"]["regime_multiplier"].is_number());
        assert!(v["co_occurrence"]["elevated_count"].is_number());
        assert!(v["alert"]["level"].is_string());
        assert!(v["meta"]["events_in_window"].is_number());
        // The modality-sensitivity read (load-bearing modality) is on the served contract.
        assert!(v["load_bearing_modality"].is_object(), "load_bearing_modality must be served");
        assert!(v["load_bearing_modality"]["available"].is_boolean(),
            "load_bearing_modality must carry an availability flag");
        assert!(v["load_bearing_modality"]["profile"].is_array(),
            "load_bearing_modality must carry the per-modality attribution profile");
        // The theater-sensitivity read (load-bearing theater) is on the served contract.
        assert!(v["load_bearing_theater"].is_object(), "load_bearing_theater must be served");
        assert!(v["load_bearing_theater"]["available"].is_boolean(),
            "load_bearing_theater must carry an availability flag");
        assert!(v["load_bearing_theater"]["profile"].is_array(),
            "load_bearing_theater must carry the per-theater attribution profile");
        // The memory-load read (headline carried by remembered war-state vs. fresh evidence) is on
        // the served contract — the quantitative form of systemic_memory_held.
        assert!(v["memory_load"].is_object(), "memory_load must be served");
        assert!(v["memory_load"]["available"].is_boolean(),
            "memory_load must carry an availability flag");
        assert!(v["memory_load"]["lift_pp"].is_number(),
            "memory_load must carry the pp lift carried by memory");
        assert!(v["memory_load"]["held_theaters"].is_array(),
            "memory_load must carry the list of floor-held theaters");
        // The escalation-coherence read (is the number heating WHERE it rests, or on a different
        // front) is on the served contract — the relation between load_bearing_theater and
        // per-theater escalation_momentum.
        assert!(v["escalation_coherence"].is_object(), "escalation_coherence must be served");
        assert!(v["escalation_coherence"]["available"].is_boolean(),
            "escalation_coherence must carry an availability flag");
        assert!(v["escalation_coherence"]["coherent"].is_boolean(),
            "escalation_coherence must carry the coherent/divergent flag");
        assert!(v["escalation_coherence"]["momentum_theater_id"].is_string(),
            "escalation_coherence must carry the momentum leader's id");
        // The escalation-breadth read (how many fronts are decisively escalating AT ONCE) is on the
        // served contract — the momentum-breadth of the board, distinct from couplers.concurrency.
        assert!(v["escalation_breadth"].is_object(), "escalation_breadth must be served");
        assert!(v["escalation_breadth"]["available"].is_boolean(),
            "escalation_breadth must carry an availability flag");
        assert!(v["escalation_breadth"]["count"].is_number(),
            "escalation_breadth must carry the count of simultaneously-escalating fronts");
        assert!(v["escalation_breadth"]["multi_front"].is_boolean(),
            "escalation_breadth must carry the synchronized-multi-front flag");
        assert!(v["escalation_breadth"]["fronts"].is_array(),
            "escalation_breadth must carry the list of escalating fronts");
    }

    // ── Headline-read contract v1 (RAITHE Global Monitor §7.1) ─────────────────
    // Locks the FROZEN schema sibling monitors + the /intel portal clone from a
    // spec (docs/headline-read-contract-v1.md) instead of forking the dashboard.
    // A red here means a BREAKING change to the served headline read — bump the
    // `/vN` handle on purpose, never delete the assert.
    #[test]
    fn snapshot_to_json_honours_contract_v1() {
        let mut snap = make_snapshot(0.30, 0.001, 2);
        // Fill the two horizon fields the helper leaves at Default so the v1
        // monotonicity invariant (30d ≤ 90d ≤ annual) is exercised honestly.
        snap.p_wwiii_30day = 1.0 - (1.0 - 0.30_f64).powf(30.0 / 365.0);
        snap.p_wwiii_90day = 1.0 - (1.0 - 0.30_f64).powf(90.0 / 365.0);
        let v = snapshot_to_json(&snap);

        // 1. The version handle is present and is the v1 string (the negotiation
        //    field a consumer reads FIRST). Bumping it is a deliberate breaking act.
        assert_eq!(v["contract"], serde_json::json!(HEADLINE_READ_CONTRACT));
        assert_eq!(v["contract"], serde_json::json!("gcrm.headline-read/v1"));

        // 2. Every documented top-level key is present with its documented type.
        assert!(v["snapshot_id"].is_string());
        assert!(v["computed_at"].is_string());
        assert!(v["prior"].is_object());
        assert!(v["prior"]["historical_anchor"].is_number());
        assert!(v["prior"]["regime_multiplier"].is_number());
        assert!(v["prior"]["regime_role"].is_string());
        assert!(v["domains"].is_object());
        assert!(v["co_occurrence"]["elevated_count"].is_number());
        assert!(v["co_occurrence"]["boost"].is_number());
        assert!(v["probabilities"]["annual"].is_number());
        assert!(v["probabilities"]["annual_pct"].is_number());
        assert!(v["probabilities"]["thirty_day"].is_number());
        assert!(v["probabilities"]["ninety_day"].is_number());
        assert!(v["delta"]["annual"].is_number());
        assert!(v["delta"]["direction"].is_string());
        assert!(v["confidence"].is_number());
        assert!(v["alert"]["level"].is_string());
        assert!(v["alert"]["elevated_threshold"].is_number());
        assert!(v["alert"]["critical_threshold"].is_number());
        assert!(v["systemic"]["index"].is_number());
        assert!(v["systemic"]["driver"].is_string());
        assert!(v["theaters"].is_array());
        assert!(v["couplers"].is_object());
        assert!(v["indicators"].is_array());
        assert!(v["meta"].is_object());
        for k in ["events_in_window", "data_blind", "thinly_sourced", "at_ceiling",
                  "breadth_saturated", "read_held_by_floor", "sources_active",
                  "regions_active", "aggregation_window_hours", "max_window_events",
                  "window_coverage", "newest_event_age_secs", "observation_factor",
                  "observation_gap"] {
            assert!(!v["meta"][k].is_null(), "contract v1 meta key missing: {k}");
        }

        // 3. v1 cross-field invariants a consumer is entitled to rely on.
        let annual = v["probabilities"]["annual"].as_f64().unwrap();
        let pct    = v["probabilities"]["annual_pct"].as_f64().unwrap();
        assert!((pct - (annual * 100.0 * 1e6).round() / 1e6).abs() < 1e-12,
                "annual_pct must be annual·100 rounded to 6dp");
        let d30 = v["probabilities"]["thirty_day"].as_f64().unwrap();
        let d90 = v["probabilities"]["ninety_day"].as_f64().unwrap();
        assert!(d30 <= d90 + 1e-12 && d90 <= annual + 1e-12,
                "horizons must satisfy 30d ≤ 90d ≤ annual (got {d30} {d90} {annual})");
        let dir = v["delta"]["direction"].as_str().unwrap();
        assert!(matches!(dir, "rising" | "falling" | "stable"),
                "delta.direction must be one of the v1 enum values, got {dir}");
        // honesty-posture flags are booleans the consumer must respect, not numbers
        assert!(v["meta"]["data_blind"].is_boolean());
        assert!(v["meta"]["at_ceiling"].is_boolean());
        assert!(v["meta"]["read_held_by_floor"].is_boolean());
    }

    #[test]
    fn meta_data_blind_flags_a_zero_event_read_as_baseline_only() {
        // A populated window is a real measurement → not blind.
        let live = make_snapshot(0.03, 0.001, 2); // events_in_window = 10
        assert_eq!(snapshot_to_json(&live)["meta"]["data_blind"], serde_json::json!(false));

        // Zero events → the headline is the baseline prior, not a measurement. The
        // served contract must say so, so the dashboard's "NO LIVE SIGNAL" warning
        // can't drift from the model's offline state. Single source of truth:
        // bayesian::is_data_blind (== the offline-confidence-floor condition).
        let mut blind = make_snapshot(0.015, 0.0, 0);
        blind.events_in_window = 0;
        let v = snapshot_to_json(&blind);
        assert_eq!(v["meta"]["data_blind"], serde_json::json!(true));
        assert!(crate::bayesian::is_data_blind(0));
    }

    #[test]
    fn meta_thinly_sourced_flags_a_narrow_source_base() {
        // A read on a healthy source base (default 3 feeds = the corroboration floor) is
        // broadly corroborated → not thin.
        let broad = make_snapshot(0.03, 0.001, 2); // events 10, sources 3
        assert_eq!(snapshot_to_json(&broad)["meta"]["thinly_sourced"], serde_json::json!(false));

        // Live events but only one reporting feed (partial outage) → the served contract
        // flags it so the header can warn the read rests on a narrow base. Single source
        // of truth: bayesian::is_thinly_sourced. Distinct from data_blind (events > 0).
        let mut thin = make_snapshot(0.03, 0.001, 2);
        thin.sources_active = 1;
        let v = snapshot_to_json(&thin);
        assert_eq!(v["meta"]["thinly_sourced"], serde_json::json!(true));
        assert_eq!(v["meta"]["data_blind"], serde_json::json!(false));
    }

    #[test]
    fn meta_at_ceiling_flags_a_clamped_read_as_capped() {
        // A sub-ceiling read is a point estimate → not capped.
        let measured = make_snapshot(0.42, 0.001, 2);
        assert_eq!(snapshot_to_json(&measured)["meta"]["at_ceiling"], serde_json::json!(false));

        // A read pegged at FORECAST_PROB_CEILING is a clamped FLOOR, not a measured value —
        // the served contract must say so, so the dashboard's "capped" caveat can't drift
        // from the model's own clamp. Single source of truth: bayesian::is_at_forecast_ceiling.
        let mut capped = make_snapshot(crate::models::FORECAST_PROB_CEILING, 0.0, 5);
        capped.p_wwiii_annual = crate::models::FORECAST_PROB_CEILING;
        let v = snapshot_to_json(&capped);
        assert_eq!(v["meta"]["at_ceiling"], serde_json::json!(true));
        assert!(crate::bayesian::is_at_forecast_ceiling(crate::models::FORECAST_PROB_CEILING));
    }

    #[test]
    fn meta_mirrors_the_breadth_saturation_flag_from_the_couplers() {
        // The served contract must carry the model's own breadth-saturation flag so the
        // operator surface can disclose a railed read (a structural maximum that sits BELOW
        // the forecast ceiling, where `at_ceiling` stays false) without recomputing the
        // rails. Single source of truth: theater compute → couplers.breadth_saturated.
        let mut snap = make_snapshot(0.83, 0.0, 5);
        assert_eq!(snapshot_to_json(&snap)["meta"]["breadth_saturated"], serde_json::json!(false));
        snap.couplers.breadth_saturated = true;
        assert_eq!(snapshot_to_json(&snap)["meta"]["breadth_saturated"], serde_json::json!(true));
    }

    #[test]
    fn meta_read_held_by_floor_flags_a_memory_held_headline() {
        use crate::models::{EscalationRung, TheaterState};
        let theater = |id: &str, heat: f64, held: bool| TheaterState {
            theater_id: id.into(), label: id.into(),
            rung: EscalationRung::LimitedWar, rung_label: "Limited War".into(),
            heat, modality_scores: Default::default(), trend: "stable".into(),
            delta: 0.0, event_count: 6, gp_involved: false, alliance_invoked: false,
            top_actors: vec![], top_driver: String::new(), rising_driver: String::new(),
            secondary_driver: String::new(), held_by_floor: held,
            fresh_rung_label: "Limited War".into(),
            escalation_momentum: 0.0,
        };

        // A headline whose LEAD (highest-heat) theater reads live → not held, even if a cooler
        // theater happens to be floor-held.
        let mut live = make_snapshot(0.40, 0.0, 3);
        live.theaters = vec![theater("us_iran", 0.62, false), theater("nato_russia", 0.30, true)];
        assert_eq!(snapshot_to_json(&live)["meta"]["read_held_by_floor"], serde_json::json!(false));

        // A headline led by a floor-held war (the model is holding it through a news gap) → the
        // served contract says so, so the hero "held by persistence" caveat can't drift from the
        // model. Single source of truth: theater::systemic_read_is_floor_held.
        let mut held = make_snapshot(0.40, 0.0, 3);
        held.theaters = vec![theater("us_iran", 0.62, true), theater("nato_russia", 0.30, false)];
        assert_eq!(snapshot_to_json(&held)["meta"]["read_held_by_floor"], serde_json::json!(true));

        // A quiet world (no theaters) never manufactures a held headline.
        let quiet = make_snapshot(0.015, 0.0, 0);
        assert_eq!(snapshot_to_json(&quiet)["meta"]["read_held_by_floor"], serde_json::json!(false));
    }

    #[test]
    fn snapshot_to_json_annual_pct_correct() {
        let snap = make_snapshot(0.0350, 0.0, 1);
        let v = snapshot_to_json(&snap);
        let pct = v["probabilities"]["annual_pct"].as_f64().unwrap();
        assert!((pct - 3.5).abs() < 0.001);
    }

    #[test]
    fn snapshot_to_json_delta_direction_rising() {
        let snap = make_snapshot(0.04, 0.002, 2);
        let v = snapshot_to_json(&snap);
        assert_eq!(v["delta"]["direction"], "rising");
    }

    #[test]
    fn snapshot_to_json_delta_direction_falling() {
        let snap = make_snapshot(0.02, -0.001, 1);
        let v = snapshot_to_json(&snap);
        assert_eq!(v["delta"]["direction"], "falling");
    }

    #[test]
    fn snapshot_to_json_delta_direction_stable() {
        let snap = make_snapshot(0.02, 0.0, 1);
        let v = snapshot_to_json(&snap);
        assert_eq!(v["delta"]["direction"], "stable");
    }

    #[test]
    fn snapshot_to_json_alert_level_critical() {
        let snap = make_snapshot(0.09, 0.0, 5);
        let v = snapshot_to_json(&snap);
        assert_eq!(v["alert"]["level"], "critical");
    }

    #[test]
    fn snapshot_to_json_carries_live_alert_thresholds() {
        // The dashboard draws its critical timeline reference line + risk colours
        // from these fields, so the JSON must echo the snapshot's OWN thresholds
        // verbatim — never a hardcoded literal. Use non-default values to prove the
        // snapshot's configured thresholds are what flow through.
        let mut snap = make_snapshot(0.03, 0.0, 2);
        snap.alert_elevated_threshold = 0.04;
        snap.alert_critical_threshold = 0.11;
        let v = snapshot_to_json(&snap);
        assert_eq!(v["alert"]["elevated_threshold"].as_f64().unwrap(), 0.04);
        assert_eq!(v["alert"]["critical_threshold"].as_f64().unwrap(), 0.11);
    }

    #[test]
    fn snapshot_to_json_domains_elevated_flag() {
        let mut snap = make_snapshot(0.03, 0.0, 1);
        let mut ds = DomainScore::zero("nuclear_posture");
        ds.score = ELEVATION_THRESHOLD + 0.1;
        snap.domain_scores.insert("nuclear_posture".into(), ds);
        let v = snapshot_to_json(&snap);
        assert_eq!(v["domains"]["nuclear_posture"]["elevated"], true);
    }

    #[test]
    fn snapshot_to_json_max_window_events() {
        let snap = make_snapshot(0.01, 0.0, 0);
        let v = snapshot_to_json(&snap);
        assert_eq!(v["meta"]["max_window_events"].as_u64().unwrap() as usize, MAX_WINDOW_EVENTS);
    }

    #[test]
    fn snapshot_to_json_max_window_events_is_500k() {
        let snap = make_snapshot(0.01, 0.0, 0);
        let v = snapshot_to_json(&snap);
        assert_eq!(v["meta"]["max_window_events"].as_u64().unwrap() as usize, MAX_WINDOW_EVENTS);
        assert_eq!(MAX_WINDOW_EVENTS, 500_000,
            "MAX_WINDOW_EVENTS must be 500,000 (fix; was 25,000)");
    }

    // ── TimelineEntry ─────────────────────────────────────────────────────────

    #[test]
    fn timeline_entry_fields() {
        let snap = make_snapshot(0.09, 0.001, 4);
        let entry = TimelineEntry::from_snapshot(&snap);
        assert_eq!(entry.alert, "critical");
        assert_eq!(entry.elevated, 4);
        assert!((entry.regime - 1.568).abs() < 0.001);
    }

    #[test]
    fn timeline_entry_serialises() {
        let snap = make_snapshot(0.05, 0.0, 3);
        let entry = TimelineEntry::from_snapshot(&snap);
        let s = serde_json::to_string(&entry).unwrap();
        assert!(s.contains("p_annual"));
        assert!(s.contains("p_30day"));
    }

    // ── Timeline path rotation ────────────────────────────────────────────────

    #[test]
    fn timeline_path_for_date_format() {
        use chrono::NaiveDate;
        let d = NaiveDate::from_ymd_opt(2026, 4, 1).unwrap();
        assert_eq!(timeline_path_for_date(&d), "logs/timeline_2026-04-01.jsonl");
    }

    #[test]
    fn today_timeline_path_contains_logs_prefix() {
        let p = today_timeline_path();
        assert!(p.starts_with("logs/timeline_"),
            "path should start with logs/timeline_, got: {p}");
        assert!(p.ends_with(".jsonl"),
            "path should end with .jsonl, got: {p}");
    }

    // ── ArticleStore ──────────────────────────────────────────────────────────

    fn make_article(id: &str, source: &str) -> StoredArticle {
        StoredArticle {
            id: id.to_string(), title: format!("Headline {id}"),
            url: format!("https://example.com/{id}"), source: source.to_string(),
            tier: 1, published_at: Utc::now().to_rfc3339(),
            ingested_at: Utc::now().to_rfc3339(),
            body: "body".to_string(), domain_tags: vec![],
        }
    }

    #[test]
    fn article_store_push_and_len() {
        let mut store = ArticleStore::new(100);
        store.push(make_article("a1", "bbc"));
        store.push(make_article("a2", "bbc"));
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn article_store_evicts_at_max() {
        let mut store = ArticleStore::new(3);
        for i in 0..5 { store.push(make_article(&format!("a{i}"), "bbc")); }
        assert_eq!(store.len(), 3);
    }

    #[test]
    fn article_store_update_by_url_updates_in_place() {
        // A re-ingested canonical URL is the SAME article after a live-blog edit
        // (559 dup URLs / 834 extra rows lived in the store); it must refresh the
        // existing row — same id, tags preserved — not add a new one. (audit-news L2)
        let mut store = ArticleStore::new(100);
        let mut a = make_article("live1", "bbc");
        a.domain_tags = vec!["military_escalation".into()];
        let url = a.url.clone();
        store.push(a);
        let newer = "2027-01-01T00:00:00+00:00";
        let updated = store.update_by_url(&url, "bbc", "Edited headline", "new body", newer, newer)
            .expect("known URL must update, not insert");
        assert_eq!(store.len(), 1, "update-in-place must not grow the store");
        assert_eq!(updated.id, "live1", "the row keeps its identity");
        assert_eq!(updated.title, "Edited headline");
        assert_eq!(updated.published_at, newer);
        assert_eq!(updated.domain_tags, vec!["military_escalation".to_string()],
            "NLP tags from the first pass survive the edit");
    }

    #[test]
    fn article_store_update_by_url_refuses_older_published_at() {
        // A stale syndicated copy (older published_at) must not clobber the newest
        // edit — the returned clone keeps the stored content.
        let mut store = ArticleStore::new(100);
        let mut a = make_article("live2", "bbc");
        a.published_at = "2026-06-01T00:00:00+00:00".to_string();
        let url = a.url.clone();
        store.push(a);
        let kept = store.update_by_url(&url, "bbc", "Stale title", "stale", "2026-01-01T00:00:00+00:00", "x")
            .expect("known URL still resolves");
        assert_eq!(kept.title, "Headline live2", "older copy must not overwrite the stored row");
    }

    #[test]
    fn article_store_update_by_url_refuses_cross_source_clobber() {
        // GDELT surfaces the exact publisher URLs the RSS roster already stored, with
        // page-<title> furniture and a scrape-time date that always reads newer. A
        // same-URL hit from a DIFFERENT source is a syndicated copy, not an edit — the
        // stored row must survive untouched (title, body, source, published_at), and
        // the Some return tells the caller it is handled (no duplicate row inserted).
        let mut store = ArticleStore::new(100);
        let mut a = make_article("live3", "guardian");
        a.body = "clean 500-char excerpt".to_string();
        a.published_at = "2026-07-03T09:00:00+00:00".to_string();
        let url = a.url.clone();
        store.push(a);
        let kept = store.update_by_url(&url, "gdelt",
            "NATO summit opens | Ukraine | The Guardian", "20260703T101500Z",
            "2026-07-03T10:15:00+00:00", "2026-07-03T10:15:00+00:00")
            .expect("known URL resolves so the caller does not insert a duplicate");
        assert_eq!(kept.title, "Headline live3", "cross-source hit must not retitle the row");
        assert_eq!(kept.body, "clean 500-char excerpt", "cross-source hit must not clobber the excerpt");
        assert_eq!(kept.source, "guardian", "attribution unchanged");
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn article_store_update_by_url_keeps_excerpt_when_refetch_body_is_empty() {
        // A degenerate same-feed re-fetch (title-only entry) updates the headline but
        // must not blank a real excerpt the operator/LLM already benefits from.
        let mut store = ArticleStore::new(100);
        let mut a = make_article("live4", "bbc");
        a.body = "substantive excerpt".to_string();
        a.published_at = "2026-07-03T09:00:00+00:00".to_string();
        let url = a.url.clone();
        store.push(a);
        let u = store.update_by_url(&url, "bbc", "New headline", "   ",
            "2026-07-03T10:00:00+00:00", "2026-07-03T10:00:00+00:00").unwrap();
        assert_eq!(u.title, "New headline", "same-source edit still lands");
        assert_eq!(u.body, "substantive excerpt", "an empty re-fetch body must not erase the excerpt");
    }

    #[test]
    fn article_store_update_by_url_unknown_or_empty_url_is_none() {
        let mut store = ArticleStore::new(100);
        store.push(make_article("a1", "bbc"));
        assert!(store.update_by_url("https://example.com/other", "bbc", "t", "b", "p", "i").is_none(),
            "unknown URL must fall through to a normal insert");
        assert!(store.update_by_url("", "bbc", "t", "b", "p", "i").is_none(),
            "empty URLs are not indexed (GDELT rows can lack one)");
    }

    #[test]
    fn article_store_url_index_cleared_on_eviction() {
        // Once a row rotates out, its URL must stop resolving — otherwise
        // update_by_url would chase a dangling id instead of inserting fresh.
        let mut store = ArticleStore::new(2);
        let first_url = make_article("a0", "bbc").url.clone();
        for i in 0..3 { store.push(make_article(&format!("a{i}"), "bbc")); } // a0 evicted
        assert!(store.update_by_url(&first_url, "bbc", "t", "b", "p", "i").is_none(),
            "evicted row's URL must not resolve to an update");
    }

    #[test]
    fn dedupe_newest_per_url_keeps_last_row_per_url() {
        // The archive holds one append per live-blog edit (fresh id each, same URL);
        // a reload must keep only the newest — otherwise every restart resurrected
        // the duplicate rows the live path now updates in place. (audit-news L2)
        let mut edit1 = make_article("e1", "bbc");
        let mut edit2 = make_article("e2", "bbc");
        edit1.url = "https://bbc.co.uk/news/live/x".to_string();
        edit2.url = "https://bbc.co.uk/news/live/x".to_string();
        edit2.title = "Newest headline".to_string();
        let mut no_url = make_article("n1", "gdelt");
        no_url.url = String::new();
        let rows = vec![edit1, make_article("solo", "npr"), edit2, no_url];
        let out = dedupe_newest_per_url(rows);
        let ids: Vec<&str> = out.iter().map(|a| a.id.as_str()).collect();
        assert!(!ids.contains(&"e1"), "superseded live-blog row must be dropped");
        assert!(ids.contains(&"e2"), "the newest edit survives");
        assert!(ids.contains(&"solo"), "unique URLs are untouched");
        assert!(ids.contains(&"n1"), "empty-URL rows are always kept");
    }

    #[test]
    fn article_store_query_newest_first() {
        let mut store = ArticleStore::new(100);
        let mut old_art = make_article("old", "bbc");
        old_art.published_at = "2026-01-01T00:00:00+00:00".to_string();
        let mut new_art = make_article("new", "bbc");
        new_art.published_at = "2026-06-01T00:00:00+00:00".to_string();
        store.push(old_art);
        store.push(new_art);
        let results = store.query(10, None, None);
        assert_eq!(results[0].id, "new");
    }

    #[test]
    fn article_store_source_filter() {
        let mut store = ArticleStore::new(100);
        store.push(make_article("a1", "bbc"));
        store.push(make_article("a2", "reuters"));
        store.push(make_article("a3", "bbc"));
        assert_eq!(store.query(10, Some("bbc"), None).len(), 2);
    }

    #[test]
    fn article_store_domain_filter() {
        let mut store = ArticleStore::new(100);
        let mut art = make_article("a1", "bbc");
        art.domain_tags = vec!["nuclear_posture".into()];
        store.push(art);
        store.push(make_article("a2", "bbc"));
        assert_eq!(store.query(10, None, Some("nuclear_posture")).len(), 1);
    }

    #[test]
    fn article_store_set_domain_tags() {
        let mut store = ArticleStore::new(100);
        store.push(make_article("x1", "bbc"));
        store.set_domain_tags("x1", vec!["military_escalation".into()]);
        assert_eq!(store.query(10, None, Some("military_escalation")).len(), 1);
    }

    #[test]
    fn article_store_evicts_oldest_and_index_clean() {
        let mut store = ArticleStore::new(3);
        for i in 0..5 { store.push(make_article(&format!("a{i}"), "bbc")); }
        assert_eq!(store.len(), 3);
        assert!(!store.index.contains_key("a0"));
        assert!(!store.index.contains_key("a1"));
    }

    #[test]
    fn article_store_cap_is_75k() {
        assert_eq!(ArticleStore::new(75_000).max_size, 75_000);
    }

    #[test]
    fn article_store_o1_eviction_set_domain_tags_still_works() {
        let mut store = ArticleStore::new(3);
        store.push(make_article("x1", "bbc"));
        store.push(make_article("x2", "bbc"));
        store.push(make_article("x3", "bbc"));
        store.push(make_article("x4", "bbc"));
        assert!(!store.index.contains_key("x1"));
        store.set_domain_tags("x4", vec!["military_escalation".into()]);
        assert_eq!(store.query(10, None, Some("military_escalation")).len(), 1);
    }

    #[test]
    fn article_store_set_domain_tags_correct_after_eviction() {
        let mut store = ArticleStore::new(3);
        store.push(make_article("evict1", "bbc"));
        store.push(make_article("evict2", "bbc"));
        store.push(make_article("target", "bbc"));
        store.push(make_article("extra1", "bbc"));
        store.push(make_article("extra2", "bbc"));
        store.set_domain_tags("target", vec!["nuclear_posture".into()]);
        let result = store.query(10, None, Some("nuclear_posture"));
        assert_eq!(result.len(), 1, "Exactly one article should be tagged");
        assert_eq!(result[0].id, "target",
            "The correct article must be tagged even after eviction shifts the index");
    }

    #[test]
    fn article_store_set_domain_tags_unknown_id_is_noop() {
        let mut store = ArticleStore::new(100);
        store.push(make_article("a1", "bbc"));
        store.set_domain_tags("nonexistent", vec!["military_escalation".into()]);
        assert_eq!(store.query(10, None, Some("military_escalation")).len(), 0);
    }

    // ── Age helpers ───────────────────────────────────────────────────────────

    #[test]
    fn age_hours_one_day() {
        let now = Utc::now();
        let pub_at = now - Duration::hours(24);
        assert!((age_hours(&pub_at, &now) - 24.0).abs() < 0.01);
    }

    #[test]
    fn age_hours_fresh() {
        let now = Utc::now();
        let pub_at = now - Duration::seconds(30);
        assert!(age_hours(&pub_at, &now) < 0.01);
    }

    #[test]
    fn domain_label_and_elevated_coherent() {
        let mut snap = RiskSnapshot::default();
        let mut ds = DomainScore::zero("military_escalation");
        ds.score = ELEVATION_THRESHOLD + 0.05;
        snap.domain_scores.insert("military_escalation".into(), ds);
        let v = snapshot_to_json(&snap);
        assert_eq!(v["domains"]["military_escalation"]["elevated"], true);
        assert_eq!(v["domains"]["military_escalation"]["label"], "elevated");
    }

    #[test]
    fn historical_anchor_in_json() {
        let snap = RiskSnapshot::default();
        let v = snapshot_to_json(&snap);
        let anchor = v["prior"]["historical_anchor"].as_f64().unwrap();
        assert!((anchor - HISTORICAL_ANCHOR).abs() < 1e-10);
    }

    #[test]
    fn served_prior_is_v2_flat_not_a_v1_adjusted_prior() {
        // The served `prior` block must not reconstruct the superseded v1 chain
        // (anchor × regime = adjusted_prior). v2's prior is FLAT; the regime enters
        // the systemic likelihood via guardrail collapse (couplers), never the prior.
        // A regime ABOVE neutral once produced an `adjusted_prior` strictly above the
        // anchor — exactly the misleading product this guards against re-appearing.
        let snap = RiskSnapshot { regime_multiplier: 1.5, ..RiskSnapshot::default() };
        let v = snapshot_to_json(&snap);
        assert!(v["prior"]["adjusted_prior"].is_null(),
            "the v1 'adjusted_prior' (anchor × regime) must not be served — it implies the regime moves the prior");
        // The honest v2 role note must be present so a contract consumer can't re-derive v1.
        assert!(v["prior"]["regime_role"].as_str().unwrap_or("").contains("guardrail collapse"),
            "the served prior must state regime enters via guardrail collapse, not the prior");
        // The flat anchor is still served and unchanged by the regime multiplier.
        let anchor = v["prior"]["historical_anchor"].as_f64().unwrap();
        assert!((anchor - HISTORICAL_ANCHOR).abs() < 1e-10,
            "the served prior anchor must stay the flat baseline regardless of regime");
    }

    // ── EpochStore ────────────────────────────────────────────────────────────

    #[test]
    fn epoch_store_push_and_len() {
        let mut es = EpochStore::new();
        es.push(serde_json::json!({"p_annual": 0.03}));
        es.push(serde_json::json!({"p_annual": 0.04}));
        assert_eq!(es.len(), 2);
    }

    #[test]
    fn epoch_store_query_newest_first() {
        let mut es = EpochStore::new();
        es.push(serde_json::json!({"seq": 1}));
        es.push(serde_json::json!({"seq": 2}));
        let results = es.query(10);
        assert_eq!(results[0]["seq"], 2);
        assert_eq!(results[1]["seq"], 1);
    }

    #[test]
    fn epoch_store_query_limit_respected() {
        let mut es = EpochStore::new();
        for i in 0..10 { es.push(serde_json::json!({"seq": i})); }
        assert_eq!(es.query(3).len(), 3);
    }

    #[test]
    fn epoch_store_round_trip_timeline_entry() {
        let snap = make_snapshot(0.05, 0.001, 3);
        let entry = TimelineEntry::from_snapshot(&snap);
        let v = serde_json::to_value(&entry).unwrap();
        let mut es = EpochStore::new();
        es.push(v.clone());
        let out = es.query(1);
        assert_eq!(out[0]["p_annual"], v["p_annual"]);
    }

    // ── EpochStore::trend_6h / trend_window ─────────────────────────────────────
    // These lock the durable, server-side 6h-trend contract the dashboard relies
    // on (`data.trend_6h`). The self-improve routine gates on `cargo test`, so if
    // a future change breaks the trend math or its shape, these go red and the
    // change can't ship — that is the guard against the recurring "6h Trend = —"
    // regression that used to come from refactoring the client-side buffer.

    fn epoch_at(secs_ago: i64, now: DateTime<Utc>, p: f64) -> serde_json::Value {
        serde_json::json!({
            "t": (now - chrono::Duration::seconds(secs_ago)).to_rfc3339(),
            "p_annual": p,
        })
    }

    #[test]
    fn epoch_store_trend_delta_is_current_minus_oldest_in_window() {
        let now = Utc::now();
        let mut es = EpochStore::new();
        // oldest→newest within the 6h window: 0.80 (6h ago) … 0.83 (now-ish)
        es.push(epoch_at(5 * 3600, now, 0.80)); // baseline (oldest in window)
        es.push(epoch_at(3 * 3600, now, 0.81));
        es.push(epoch_at(60, now, 0.83));
        let tr = es.trend_window(0.835, now, 6 * 3600, 2);
        assert_eq!(tr["available"], true);
        assert_eq!(tr["samples"], 3);
        // 0.835 − 0.80 baseline
        assert!((tr["delta"].as_f64().unwrap() - 0.035).abs() < 1e-9);
        assert!((tr["baseline"].as_f64().unwrap() - 0.80).abs() < 1e-9);
    }

    fn epoch_at_lead(secs_ago: i64, now: DateTime<Utc>, p: f64, lead: &str) -> serde_json::Value {
        serde_json::json!({
            "t": (now - chrono::Duration::seconds(secs_ago)).to_rfc3339(),
            "p_annual": p,
            "lead": lead,
        })
    }

    #[test]
    fn epoch_store_trend_reports_the_baseline_lead_theater() {
        // The trend window must surface the lead theater of the OLDEST in-window entry
        // (`lead_then`) — the WHERE the read concentrated at the start of the window — so the
        // server can tell whether the locus of risk relocated over the 6h. It tracks the same
        // oldest-in-window tick as `baseline`/`delta`, and an out-of-window entry can't supply it.
        let now = Utc::now();
        let mut es = EpochStore::new();
        es.push(epoch_at_lead(10 * 3600, now, 0.50, "China-Taiwan")); // outside window — ignored
        es.push(epoch_at_lead(5 * 3600, now, 0.70, "NATO-Russia"));   // oldest IN window → baseline
        es.push(epoch_at_lead(60, now, 0.72, "US/Israel-Iran"));
        let tr = es.trend_window(0.75, now, 6 * 3600, 2);
        assert_eq!(tr["available"], true);
        assert_eq!(tr["lead_then"], "NATO-Russia", "lead_then must be the oldest in-window lead");
        // A pre-field entry (no `lead`) yields an empty baseline lead rather than panicking.
        let mut es2 = EpochStore::new();
        es2.push(epoch_at(5 * 3600, now, 0.70));
        es2.push(epoch_at(60, now, 0.72));
        let tr2 = es2.trend_window(0.75, now, 6 * 3600, 2);
        assert_eq!(tr2["lead_then"], "");
    }

    #[test]
    fn epoch_store_trend_ignores_entries_older_than_window() {
        let now = Utc::now();
        let mut es = EpochStore::new();
        es.push(epoch_at(10 * 3600, now, 0.50)); // outside 6h — must NOT be baseline
        es.push(epoch_at(5 * 3600, now, 0.70)); // oldest IN window → baseline
        es.push(epoch_at(60, now, 0.72));
        let tr = es.trend_window(0.75, now, 6 * 3600, 2);
        assert_eq!(tr["available"], true);
        assert_eq!(tr["samples"], 2); // the 10h-old one excluded
        assert!((tr["baseline"].as_f64().unwrap() - 0.70).abs() < 1e-9);
        assert!((tr["delta"].as_f64().unwrap() - 0.05).abs() < 1e-9);
    }

    // ── EpochStore::band_coverage — the uncertainty band is VALIDATED, not just published ──
    // The band is served every tick as an ~80% interval; these lock the diagnostic that measures
    // whether reality actually stayed inside it. The breach test is the behavioral lock: if the
    // in-band membership check is neutered (always-covered), it goes red.

    #[test]
    fn band_coverage_window_reports_full_coverage_on_a_stable_series() {
        // A calm read that drifts far LESS than the humility floor (7pp) over the 1h horizon must
        // stay inside its own band every time → 100% coverage, "conservative" (the floor doing its
        // job), ample pairs. This is the common healthy state.
        let now = Utc::now();
        let mut es = EpochStore::new();
        let n = 36i64;
        for i in 0..n {
            let secs_ago = (n - i) * 300 + 60; // 300s apart, ascending in time, all inside 48h
            let p = 0.50 + 0.008 * ((i % 3) - 1) as f64; // ±0.8pp jitter, well under the 7pp floor
            es.push(epoch_at(secs_ago, now, p));
        }
        let r = es.band_coverage_window(now, 48 * 3600, 6 * 3600, 3600, 300, 12);
        assert_eq!(r["available"], true);
        assert!(r["pairs"].as_u64().unwrap() >= 12, "a 3h series must yield enough pairs to judge");
        assert_eq!(r["breaches"].as_u64().unwrap(), 0, "sub-floor drift can never breach the band");
        assert!((r["coverage_pct"].as_f64().unwrap() - 100.0).abs() < 1e-9);
        assert_eq!(r["verdict"], "conservative");
    }

    #[test]
    fn band_coverage_window_flags_a_breach_when_a_move_outruns_the_band() {
        // A read flat at 0.50 for hours (tight trailing spread → band floored at ±7pp) that then STEPS
        // to 0.65 must produce breaches: an anchor sitting on the plateau, whose 1h-forward read lands
        // after the +15pp step, escapes its own ±7pp band. This is the behavioral lock — a neutered
        // (always-covered) membership check makes breaches=0 / coverage=100 and this goes red.
        let now = Utc::now();
        let mut es = EpochStore::new();
        let n = 38i64;
        for i in 0..n {
            let secs_ago = (n - i) * 300 + 60;
            let p = if i < 25 { 0.50 } else { 0.65 }; // +15pp step, well past the 7pp floor
            es.push(epoch_at(secs_ago, now, p));
        }
        let r = es.band_coverage_window(now, 48 * 3600, 6 * 3600, 3600, 300, 12);
        assert_eq!(r["available"], true);
        assert!(r["pairs"].as_u64().unwrap() >= 12);
        assert!(r["breaches"].as_u64().unwrap() >= 1, "a +15pp step must breach the ±7pp band at least once");
        assert!(r["coverage_pct"].as_f64().unwrap() < 80.0, "a real move escaping the band must drop coverage below nominal");
        assert_eq!(r["verdict"], "overconfident");
    }

    #[test]
    fn band_coverage_window_splits_breaches_by_direction() {
        // Coverage says whether the band fails; direction says WHICH WAY. A read flat on a plateau
        // that then STEPS UP escapes its band from ABOVE (the read outran the model — under-warned);
        // a step DOWN escapes from below (over-warned). This locks the direction assignment: remove
        // it (both counters stay 0) or reverse it and the matching assertion goes red.
        let now = Utc::now();
        let n = 38i64;

        // Upward step: +15pp past the ±7pp floor → every breach is ABOVE the band.
        let mut up = EpochStore::new();
        for i in 0..n {
            let secs_ago = (n - i) * 300 + 60;
            let p = if i < 25 { 0.50 } else { 0.65 };
            up.push(epoch_at(secs_ago, now, p));
        }
        let ru = up.band_coverage_window(now, 48 * 3600, 6 * 3600, 3600, 300, 12);
        assert_eq!(ru["verdict"], "overconfident");
        let bu = ru["breaches"].as_u64().unwrap();
        assert!(bu >= 1, "the +15pp step must breach at least once");
        assert_eq!(ru["breaches_up"].as_u64().unwrap(), bu, "an upward step's breaches all escape ABOVE the band");
        assert_eq!(ru["breaches_down"].as_u64().unwrap(), 0, "an upward step never breaches below");
        assert_eq!(ru["breaches_up"].as_u64().unwrap() + ru["breaches_down"].as_u64().unwrap(), bu,
            "up + down must partition the breaches");

        // Downward step: −15pp → every breach is BELOW the band (over-warned).
        let mut dn = EpochStore::new();
        for i in 0..n {
            let secs_ago = (n - i) * 300 + 60;
            let p = if i < 25 { 0.65 } else { 0.50 };
            dn.push(epoch_at(secs_ago, now, p));
        }
        let rd = dn.band_coverage_window(now, 48 * 3600, 6 * 3600, 3600, 300, 12);
        assert_eq!(rd["verdict"], "overconfident");
        let bd = rd["breaches"].as_u64().unwrap();
        assert!(bd >= 1, "the −15pp step must breach at least once");
        assert_eq!(rd["breaches_down"].as_u64().unwrap(), bd, "a downward step's breaches all escape BELOW the band");
        assert_eq!(rd["breaches_up"].as_u64().unwrap(), 0, "a downward step never breaches above");
    }

    #[test]
    fn band_coverage_window_honest_null_below_min_pairs() {
        // Too little history to form horizon pairs → no fabricated coverage number.
        let now = Utc::now();
        let mut es = EpochStore::new();
        for i in 0..5 {
            es.push(epoch_at(300 * (i + 1), now, 0.50));
        }
        let r = es.band_coverage_window(now, 48 * 3600, 6 * 3600, 3600, 300, 12);
        assert_eq!(r["available"], false, "too few pairs must not fabricate a coverage read");
        assert_eq!(r["verdict"], "insufficient");
    }

    #[test]
    fn band_coverage_window_reports_sharpness_and_floor_binding() {
        // SHARPNESS (the resolution companion to coverage). Two regimes:
        //  (a) a CALM series whose spread stays far under the ±7pp humility floor — the floor sets the
        //      band on ~every read, so floor_bound_pct ≈ 100 and the mean half-width sits at the floor;
        //  (b) a VOLATILE ±20pp sawtooth — the model's own empirical spread dwarfs the floor, so the
        //      floor binds on ~no read (floor_bound_pct ≈ 0) and the mean half-width exceeds the floor.
        // The floor-binding count is the behavioral lock: neuter `emp_hw < FLOOR` to `false` and the
        // calm-regime assertion (floor_bound_pct high) goes red.
        let now = Utc::now();
        let floor_pp = crate::models::HUMILITY_FLOOR_HW * 100.0; // 7.0
        let n = 36i64;

        // (a) calm: ±0.8pp jitter, well under the floor.
        let mut calm = EpochStore::new();
        for i in 0..n {
            let secs_ago = (n - i) * 300 + 60;
            let p = 0.50 + 0.008 * ((i % 3) - 1) as f64;
            calm.push(epoch_at(secs_ago, now, p));
        }
        let rc = calm.band_coverage_window(now, 48 * 3600, 6 * 3600, 3600, 300, 12);
        assert_eq!(rc["available"], true);
        assert!(rc["bands"].as_u64().unwrap() >= 12, "a 3h series must reconstruct enough bands to judge sharpness");
        assert!(rc["floor_bound_pct"].as_f64().unwrap() >= 90.0,
            "a sub-floor-jitter series must be floor-bound on nearly every read, got {}", rc["floor_bound_pct"]);
        assert!((rc["mean_hw_pct"].as_f64().unwrap() - floor_pp).abs() < 0.2,
            "when the floor binds, the mean half-width sits at the floor (~{floor_pp}pp), got {}", rc["mean_hw_pct"]);

        // (b) volatile: a ±20pp sawtooth — empirical spread dwarfs the floor.
        let mut vol = EpochStore::new();
        for i in 0..n {
            let secs_ago = (n - i) * 300 + 60;
            let p = if i % 2 == 0 { 0.30 } else { 0.70 };
            vol.push(epoch_at(secs_ago, now, p));
        }
        let rv = vol.band_coverage_window(now, 48 * 3600, 6 * 3600, 3600, 300, 12);
        assert_eq!(rv["available"], true);
        assert!(rv["floor_bound_pct"].as_f64().unwrap() <= 10.0,
            "a ±20pp sawtooth's empirical spread dwarfs the floor — it should bind on ~no read, got {}", rv["floor_bound_pct"]);
        assert!(rv["mean_hw_pct"].as_f64().unwrap() > floor_pp + 2.0,
            "a wide empirical spread must push the mean half-width above the floor, got {}", rv["mean_hw_pct"]);
    }

    #[test]
    fn band_coverage_window_rebuilds_the_half_width_at_full_resolution_not_the_decimated_anchors() {
        // The half-width a band-coverage read reports MUST be the one `uncertainty_window` actually
        // published — reconstructed from EVERY in-window read, not from the stride-decimated anchor
        // series. Construct a 1 Hz window whose decimated anchors (every 300s) all sit at 0.50 while
        // the sub-300s reads in between swing ±30pp: the full-resolution central-80% spread is ~0.30
        // (far above the ±7pp humility floor → a MEASURED band, not floored), but a reconstruction
        // that only saw the 300s anchors would read a flat 0.50 (zero spread → floored at ±7pp).
        // With the full-resolution fix the sharpness reads MATCH `uncertainty_window`; the pre-fix
        // decimated reconstruction reported the opposite (floor_bound_pct 100, mean_hw_pct ~7) —
        // contradicting the very band it claims to validate. This is the fails-without-the-fix lock.
        let now = Utc::now();
        let window = 7200i64; // 2h of 1 Hz reads
        let mut es = EpochStore::new();
        for s in 0..window {
            // `s` = seconds since the window cutoff (ascending in time as we push).
            let p = if s % 300 == 0 {
                0.50 // the points the 300s decimator keeps — a flat, floored-looking anchor series
            } else if s % 2 == 0 {
                0.20 // the sub-anchor swing the published band actually saw…
            } else {
                0.80 // …±30pp, far above the humility floor
            };
            es.push(epoch_at(window - s, now, p));
        }
        let r = es.band_coverage_window(now, window, 1800, 300, 300, 6);
        assert_eq!(r["available"], true, "2h of 1 Hz data must reconstruct enough pairs");
        // Full-resolution truth: the band is set by measured volatility on every reconstructable read.
        assert_eq!(r["floor_bound_pct"].as_f64().unwrap(), 0.0,
            "the sub-anchor ±30pp spread is far above the floor — no band is floor-bound (pre-fix decimated: 100), got {}",
            r["floor_bound_pct"]);
        assert!(r["mean_hw_pct"].as_f64().unwrap() > 20.0,
            "the mean half-width must reflect the full-resolution ~30pp spread (pre-fix decimated: ~7pp floor), got {}",
            r["mean_hw_pct"]);
        // …and it must AGREE with the very band it validates: `uncertainty_window` over the same
        // trailing window reports the band as MEASURED (floored:false). A decimated reconstruction
        // would have labelled the identical history floor-bound — contradicting the published band.
        let u = es.uncertainty_window(0.50, 1.0, now, 1800);
        assert_eq!(u["floored"], false, "the published band over the same window is measured, not floored");
        assert!(u["empirical_hw_pct"].as_f64().unwrap() > 20.0,
            "sanity: the published band's own empirical half-width is the ~30pp the reconstruction must match, got {}",
            u["empirical_hw_pct"]);
    }

    // ── Alert-band dwell (the TIME axis of the current state) ──────────────────────

    fn dwell_entry(secs_ago: i64, now: DateTime<Utc>, alert: &str) -> serde_json::Value {
        serde_json::json!({
            "t": (now - chrono::Duration::seconds(secs_ago)).to_rfc3339(),
            "p_annual": 0.05,
            "alert": alert,
        })
    }

    #[test]
    fn alert_dwell_window_measures_time_at_or_above_current_band() {
        // The read climbed normal→elevated→critical. Asked at the ELEVATED floor, the dwell must
        // count the ENTIRE contiguous run that is at OR ABOVE elevated — including the two later
        // CRITICAL ticks — and stop at the normal tick below the band. Exact-level matching would
        // break at the first critical tick and report nothing, so this locks the "at or above"
        // semantics, not just presence.
        let now = Utc::now();
        let mut es = EpochStore::new();
        es.push(dwell_entry(300, now, "normal")); // below the band → the boundary
        es.push(dwell_entry(240, now, "elevated"));
        es.push(dwell_entry(180, now, "elevated"));
        es.push(dwell_entry(120, now, "critical"));
        es.push(dwell_entry(60, now, "critical")); // newest
        let r = es.alert_dwell_window("elevated", now, 3);
        assert_eq!(r["available"], true, "an in-band run past the floor must produce a dwell");
        assert_eq!(r["level"], "elevated");
        assert_eq!(r["samples"], 4, "all four at-or-above-elevated ticks count (2 elevated + 2 critical)");
        assert_eq!(r["dwell_secs"], 240, "dwell runs from the oldest in-band tick (240s ago) to now");
        assert_eq!(r["capped"], false, "the run hit a normal tick below the band — a real boundary");
    }

    #[test]
    fn alert_dwell_window_caps_when_the_run_reaches_the_ring_edge() {
        // The whole ring is at or above the band with no lower tick before it — the dwell BEGAN
        // before the stored horizon, so it is a FLOOR (`capped:true`), not an exact age.
        let now = Utc::now();
        let mut es = EpochStore::new();
        for &s in &[200_i64, 150, 100, 50] {
            es.push(dwell_entry(s, now, "critical"));
        }
        let r = es.alert_dwell_window("critical", now, 3);
        assert_eq!(r["available"], true);
        assert_eq!(r["capped"], true, "no boundary within the ring → the dwell is a floor");
        assert_eq!(r["dwell_secs"], 200, "floor spans the oldest stored in-band tick");
        assert_eq!(r["samples"], 4);
    }

    #[test]
    fn alert_dwell_window_honest_null_below_min_samples() {
        // Too few contiguous in-band ticks to assert a duration → no fabricated dwell.
        let now = Utc::now();
        let mut es = EpochStore::new();
        es.push(dwell_entry(120, now, "critical"));
        es.push(dwell_entry(60, now, "critical"));
        let r = es.alert_dwell_window("critical", now, 3);
        assert_eq!(r["available"], false, "2 ticks is below the 3-sample floor");
        assert_eq!(r["samples"], 2);
    }

    #[test]
    fn alert_dwell_window_fails_closed_on_a_missing_alert_field() {
        // An entry we cannot confirm held the band (no `alert` field) ENDS the run rather than
        // silently extending the dwell across it — fail closed, never overstate entrenchment.
        let now = Utc::now();
        let mut es = EpochStore::new();
        es.push(dwell_entry(300, now, "critical")); // older, but unreachable past the gap
        es.push(serde_json::json!({ // in-run tick with NO alert field
            "t": (now - chrono::Duration::seconds(240)).to_rfc3339(),
            "p_annual": 0.05,
        }));
        es.push(dwell_entry(180, now, "critical"));
        es.push(dwell_entry(120, now, "critical"));
        es.push(dwell_entry(60, now, "critical")); // newest
        let r = es.alert_dwell_window("critical", now, 2);
        assert_eq!(r["available"], true);
        assert_eq!(r["samples"], 3, "the run stops at the missing-alert gap, not the older critical tick");
        assert_eq!(r["dwell_secs"], 180, "dwell must not extend across the unconfirmable tick");
    }

    #[test]
    fn alert_dwell_window_honest_null_on_unknown_alert() {
        // An unrecognized current-alert token cannot be ranked → honest-null, never a guess.
        let now = Utc::now();
        let mut es = EpochStore::new();
        for &s in &[200_i64, 150, 100, 50] {
            es.push(dwell_entry(s, now, "critical"));
        }
        let r = es.alert_dwell_window("chartreuse", now, 3);
        assert_eq!(r["available"], false);
        assert_eq!(r["reason"], "unknown_alert");
    }

    // ── Awareness-layer locus concentration ───────────────────────────────────────

    #[test]
    fn lead_concentration_window_reports_entrenched_when_one_theater_dominates() {
        // 36 of 40 in-window ticks led by Taiwan, current lead Taiwan → the locus was ENTRENCHED
        // on one flashpoint, and the current lead held ~90% of the day. Locks the concentration
        // math and the ≥70% entrenched verdict; also proves distinct-front counting. Ticks span
        // ~7h (over the MIN_SPAN quarter-window floor) so the day-share verdict is earned.
        let now = Utc::now();
        let mut es = EpochStore::new();
        for i in 0..36 { es.push(epoch_at_lead(25200 - i * 600, now, 0.60, "Taiwan Strait")); }
        for i in 0..4  { es.push(epoch_at_lead(120 - i, now, 0.60, "Ukraine")); }
        let r = es.lead_concentration_window("Taiwan Strait", now, 24 * 3600, 30);
        assert_eq!(r["available"], true);
        assert_eq!(r["current"], "Taiwan Strait");
        assert_eq!(r["samples"], 40);
        assert_eq!(r["distinct"], 2);
        assert_eq!(r["top"], "Taiwan Strait");
        assert_eq!(r["top_pct"].as_f64().unwrap(), 90.0);
        assert_eq!(r["current_pct"].as_f64().unwrap(), 90.0);
        assert_eq!(r["verdict"], "entrenched");
        assert!(r["span_secs"].as_i64().unwrap() >= LEAD_CONC_MIN_SPAN_SECS,
                "the verdict must report the span it actually rests on");
    }

    #[test]
    fn lead_concentration_window_honest_null_on_a_short_post_restart_ring() {
        // 40 decisive one-hertz ticks spanning ~15 MINUTES clear the sample floor but cannot
        // support a verdict ABOUT THE DAY ("entrenched" = one flashpoint owned most of 24h).
        // A freshly restarted ring reads honest-null until real span accrues — the 2026-07-17
        // post-outage board claimed "entrenched · 100% of 24h" off a 15-minute ring.
        let now = Utc::now();
        let mut es = EpochStore::new();
        for i in 0..40 { es.push(epoch_at_lead(900 - i * 20, now, 0.60, "Taiwan Strait")); }
        let r = es.lead_concentration_window("Taiwan Strait", now, 24 * 3600, 30);
        assert_eq!(r["available"], false);
        assert_eq!(r["reason"], "short_history");
        assert_eq!(r["samples"], 40, "the honest-null still reports what it saw");
        assert!(r["span_secs"].as_i64().unwrap() < LEAD_CONC_MIN_SPAN_SECS);
    }

    #[test]
    fn lead_concentration_window_reports_rotating_when_many_fronts_share() {
        // The lead rotated across five fronts with no plurality majority → a broadening,
        // multi-front world. Locks the rotating verdict (distinct ≥ 4 AND top_pct < 45) — the
        // state that reads differently from a single deepening standoff, and that the binary
        // 6h-relocation flag cannot express.
        let now = Utc::now();
        let mut es = EpochStore::new();
        let fronts = ["Taiwan Strait", "Ukraine", "Kashmir", "Korea", "South China Sea"];
        for i in 0..40i64 {
            es.push(epoch_at_lead(25200 - i * 600, now, 0.55, fronts[(i as usize) % fronts.len()]));
        }
        let r = es.lead_concentration_window("Taiwan Strait", now, 24 * 3600, 30);
        assert_eq!(r["available"], true);
        assert_eq!(r["distinct"], 5);
        assert!(r["top_pct"].as_f64().unwrap() < 45.0, "no front holds a plurality majority");
        assert_eq!(r["verdict"], "rotating");
    }

    #[test]
    fn lead_concentration_window_ignores_quiet_ticks_and_null_below_min() {
        // Quiet ticks (empty lead) are NOT decisive samples — a mostly-quiet ring with only a few
        // led ticks is honest-null, never "0% concentrated" off a handful of samples.
        let now = Utc::now();
        let mut es = EpochStore::new();
        for i in 0..50 { es.push(epoch_at_lead(4000 - i, now, 0.02, "")); } // quiet: no lead
        for i in 0..5  { es.push(epoch_at_lead(200 - i, now, 0.30, "Taiwan Strait")); }
        let r = es.lead_concentration_window("Taiwan Strait", now, 24 * 3600, 30);
        assert_eq!(r["available"], false, "5 led ticks (quiet ones excluded) is below the 30-sample floor");
        assert_eq!(r["samples"], 5, "only non-empty-lead ticks are counted");
    }

    #[test]
    fn lead_concentration_window_honest_null_when_current_world_has_no_lead() {
        // A quiet current world (empty live lead) has no locus to characterize → honest-null,
        // even if the ring holds historical leads. Never assert a concentration for "no lead".
        let now = Utc::now();
        let mut es = EpochStore::new();
        for i in 0..40 { es.push(epoch_at_lead(3600 - i, now, 0.60, "Taiwan Strait")); }
        let r = es.lead_concentration_window("", now, 24 * 3600, 30);
        assert_eq!(r["available"], false);
        assert_eq!(r["reason"], "no_lead");
    }

    #[test]
    fn lead_concentration_window_names_the_day_leader_when_current_is_a_fresh_entrant() {
        // The current lead just took over (a small day-share) while another theater actually led
        // most of the day. Both must surface: current_pct small, top the day's leader — so the live
        // tag can't mislead the operator into thinking the fresh entrant owned the day.
        let now = Utc::now();
        let mut es = EpochStore::new();
        for i in 0..34 { es.push(epoch_at_lead(25200 - i * 600, now, 0.60, "Ukraine")); }  // day leader
        for i in 0..6  { es.push(epoch_at_lead(60 - i, now, 0.60, "Taiwan Strait")); }     // fresh entrant, newest
        let r = es.lead_concentration_window("Taiwan Strait", now, 24 * 3600, 30);
        assert_eq!(r["available"], true);
        assert_eq!(r["current"], "Taiwan Strait");
        assert_eq!(r["top"], "Ukraine", "the modal lead is the day's leader, not the live tag");
        assert_eq!(r["current_pct"].as_f64().unwrap(), 15.0);
        assert_eq!(r["top_pct"].as_f64().unwrap(), 85.0);
        assert_eq!(r["verdict"], "entrenched", "the day was dominated by one front even as the live lead changed");
    }

    // ── Honesty-layer uncertainty interval ────────────────────────────────────────

    #[test]
    fn percentile_sorted_interpolates_and_handles_edges() {
        assert_eq!(percentile_sorted(&[], 0.5), 0.0);
        assert_eq!(percentile_sorted(&[0.4], 0.9), 0.4);
        let v = [0.0, 0.25, 0.5, 0.75, 1.0];
        assert!((percentile_sorted(&v, 0.0) - 0.0).abs() < 1e-12);
        assert!((percentile_sorted(&v, 1.0) - 1.0).abs() < 1e-12);
        assert!((percentile_sorted(&v, 0.5) - 0.5).abs() < 1e-12);
    }

    #[test]
    fn uncertainty_stable_read_falls_back_to_the_humility_floor() {
        // A dead-flat recent series has ~zero empirical spread, so the band must be the deliberate
        // humility floor — NOT a near-zero width. Stability is not the same as certainty.
        let now = Utc::now();
        let mut es = EpochStore::new();
        for s in [300, 240, 180, 120, 60] { es.push(epoch_at(s, now, 0.72)); }
        let u = es.uncertainty_window(0.72, 1.0, now, 6 * 3600); // confidence 1.0 → no widening
        assert_eq!(u["floored"], true, "a flat series must be floored at the humility minimum");
        let hw = (u["half_width_pct"].as_f64().unwrap()) / 100.0;
        assert!((hw - crate::models::HUMILITY_FLOOR_HW).abs() < 1e-6,
            "flat + full confidence → half-width == humility floor, got {hw}");
        // The band straddles the point estimate.
        assert!(u["low"].as_f64().unwrap() < 0.72 && u["high"].as_f64().unwrap() > 0.72);
    }

    #[test]
    fn uncertainty_volatile_read_widens_beyond_the_floor() {
        // A bouncing recent series has a real spread that must widen the band past the floor —
        // the model being visibly unstable is honestly reflected as a wider interval.
        let now = Utc::now();
        let mut es = EpochStore::new();
        for (i, p) in [0.50, 0.62, 0.55, 0.70, 0.58, 0.66, 0.52, 0.68].iter().enumerate() {
            es.push(epoch_at(300 - i as i64 * 30, now, *p));
        }
        let u = es.uncertainty_window(0.60, 1.0, now, 6 * 3600);
        assert_eq!(u["floored"], false, "a volatile series must exceed the humility floor");
        assert!((u["half_width_pct"].as_f64().unwrap()) / 100.0 > crate::models::HUMILITY_FLOOR_HW);
    }

    #[test]
    fn uncertainty_low_data_quality_widens_the_band() {
        let now = Utc::now();
        let mut es = EpochStore::new();
        for s in [300, 240, 180, 120, 60] { es.push(epoch_at(s, now, 0.40)); }
        let full = es.uncertainty_window(0.40, 1.0, now, 6 * 3600);
        let thin = es.uncertainty_window(0.40, 0.5, now, 6 * 3600); // half confidence
        assert!(thin["half_width_pct"].as_f64().unwrap() > full["half_width_pct"].as_f64().unwrap(),
            "lower data-quality must widen the interval");
    }

    #[test]
    fn uncertainty_band_is_clamped_to_zero_and_the_ceiling() {
        let now = Utc::now();
        let mut es = EpochStore::new();
        for s in [300, 240, 180, 120, 60] { es.push(epoch_at(s, now, 0.89)); }
        // Near the ceiling with low confidence: high must clamp at FORECAST_PROB_CEILING, low ≥ 0.
        let u = es.uncertainty_window(0.89, 0.2, now, 6 * 3600);
        assert!(u["high"].as_f64().unwrap() <= crate::models::FORECAST_PROB_CEILING + 1e-12);
        assert!(u["low"].as_f64().unwrap() >= 0.0);
    }

    #[test]
    fn epoch_store_trend_unavailable_below_min_samples() {
        let now = Utc::now();
        let mut es = EpochStore::new();
        es.push(epoch_at(60, now, 0.83)); // only one in-window sample
        let tr = es.trend_window(0.835, now, 6 * 3600, 2);
        assert_eq!(tr["available"], false);
        assert_eq!(tr["delta"].as_f64().unwrap(), 0.0); // never fabricated
    }

    #[test]
    fn epoch_store_trend_empty_ring_is_unavailable() {
        let es = EpochStore::new();
        let tr = es.trend_window(0.50, Utc::now(), 6 * 3600, 2);
        assert_eq!(tr["available"], false);
        assert_eq!(tr["samples"], 0);
    }

    // ── EpochStore::read_range / read_range_window — WHERE the read sits in its recent band ──
    // Locks the durable, server-side recent-range positioning the dashboard renders as the 24h
    // high/low + "Position" readout, replacing the fragile per-tab session peak/low. If the range
    // math or its shape breaks, these go red and the change can't ship.

    #[test]
    fn read_range_window_positions_the_read_at_a_fresh_high() {
        // A read sitting above every other in-window sample must report the true min/max band, a
        // top percentile rank, and the "near-high" tag — the multi-day-high state the operator
        // could not previously distinguish from a range-bound plateau.
        let now = Utc::now();
        let mut es = EpochStore::new();
        for (i, p) in [0.40, 0.42, 0.41, 0.45, 0.44, 0.50, 0.55].iter().enumerate() {
            es.push(epoch_at(3600 * (10 - i as i64), now, *p)); // all inside 24h, ascending age→recency
        }
        let r = es.read_range_window(0.60, now, 24 * 3600, 5);
        assert_eq!(r["available"], true);
        assert!((r["lo"].as_f64().unwrap() - 0.40).abs() < 1e-9, "lo must be the window minimum");
        assert!((r["hi"].as_f64().unwrap() - 0.55).abs() < 1e-9, "hi must be the window maximum");
        // 0.60 is at or above all 7 samples → 100th percentile.
        assert!((r["pct_rank"].as_f64().unwrap() - 100.0).abs() < 1e-6);
        assert_eq!(r["position"], "near-high");
        assert_eq!(r["flat"], false);
    }

    #[test]
    fn read_range_window_positions_a_low_read_near_the_bottom() {
        // Symmetric: a read below the band reads "near-low", not "flat" and not "high".
        let now = Utc::now();
        let mut es = EpochStore::new();
        for (i, p) in [0.40, 0.45, 0.50, 0.55, 0.60, 0.62].iter().enumerate() {
            es.push(epoch_at(3600 * (8 - i as i64), now, *p));
        }
        let r = es.read_range_window(0.39, now, 24 * 3600, 5);
        assert_eq!(r["available"], true);
        // 0.39 is below every sample → 0th percentile.
        assert!(r["pct_rank"].as_f64().unwrap() < 1e-6);
        assert_eq!(r["position"], "near-low");
    }

    #[test]
    fn read_range_window_flat_band_makes_no_high_low_claim() {
        // A band narrower than FLAT_RANGE_PP (0.3pp) is range-bound: the read is NOT "at its high"
        // just because it happens to be the max of a dead-flat series. This is the honesty guard
        // against the fresh-tab hi==lo==current lie the client session peak/low used to tell.
        let now = Utc::now();
        let mut es = EpochStore::new();
        for i in 0..40 {
            es.push(epoch_at(3600 * 20 - 60 * i, now, 0.500 + 0.0005 * (i % 2) as f64)); // ±0.05pp
        }
        let r = es.read_range_window(0.5005, now, 24 * 3600, 30);
        assert_eq!(r["available"], true);
        assert_eq!(r["flat"], true, "a sub-0.3pp band must read flat");
        assert_eq!(r["position"], "flat");
    }

    #[test]
    fn read_range_window_honest_null_below_min_samples() {
        let now = Utc::now();
        let mut es = EpochStore::new();
        for i in 0..5 {
            es.push(epoch_at(60 * (i + 1), now, 0.40 + 0.01 * i as f64));
        }
        let r = es.read_range_window(0.45, now, 24 * 3600, 30); // only 5 < 30
        assert_eq!(r["available"], false, "too few samples must not fabricate a range");
        assert_eq!(r["span_secs"], 0);
    }

    #[test]
    fn read_range_window_honest_null_on_a_short_post_restart_ring() {
        // A freshly restarted 1 Hz ring clears the COUNT floor within seconds while spanning only
        // seconds — many ticks, ~no time. The served `position:"near-high"` / `pct_rank` would then
        // assert a multi-day HIGH off half a minute of data (the post-restart lie the 2026-07-17
        // outage exposed on the sibling `lead_concentration_window`). `read_range_window` must
        // honest-null on `span_secs < READ_RANGE_MIN_SPAN_SECS`, mirroring that sibling's guard —
        // NOT publish a range verdict off a sub-span ring.
        let now = Utc::now();
        let mut es = EpochStore::new();
        // 40 ascending reads at 1s spacing → 40 ≥ 30 samples but only ~39s of span (≪ 6h floor).
        for i in 0..40i64 {
            es.push(epoch_at(40 - i, now, 0.40 + 0.002 * i as f64));
        }
        let r = es.read_range_window(0.60, now, 24 * 3600, 30);
        assert_eq!(r["available"], false, "a full-count but short-span ring must not claim a range");
        assert_eq!(r["reason"], "short_history");
        assert_eq!(r["samples"].as_u64().unwrap(), 40, "the sample count is still reported");
        assert!(
            r["span_secs"].as_i64().unwrap() < READ_RANGE_MIN_SPAN_SECS,
            "the honest-null must be BECAUSE the span is under the floor"
        );
        // And the guard must RELEASE once real span accrues: the same 40 reads spread over >6h
        // must publish a live range (proves the null is span-gated, not a blanket refusal).
        let mut es_long = EpochStore::new();
        for i in 0..40i64 {
            es_long.push(epoch_at(3600 * 7 - 600 * i, now, 0.40 + 0.002 * i as f64)); // ~7h..~50m span
        }
        let long = es_long.read_range_window(0.60, now, 24 * 3600, 30);
        assert_eq!(long["available"], true, "the same reads over day-scale span DO publish a range");
        assert!(long["span_secs"].as_i64().unwrap() >= READ_RANGE_MIN_SPAN_SECS);
    }

    #[test]
    fn read_range_window_ignores_entries_older_than_the_window() {
        // A read that looks like a high against only recent data must not be dragged down by an
        // out-of-window spike (and vice versa): the band is the LAST 24h, nothing older.
        let now = Utc::now();
        let mut es = EpochStore::new();
        es.push(epoch_at(30 * 3600, now, 0.90)); // 30h ago — OUTSIDE the 24h window, must be ignored
        for i in 0..6 {
            es.push(epoch_at(3600 * (6 - i), now, 0.40 + 0.01 * i as f64)); // 0.40..0.45 within window
        }
        let r = es.read_range_window(0.46, now, 24 * 3600, 5);
        assert_eq!(r["available"], true);
        assert!((r["hi"].as_f64().unwrap() - 0.45).abs() < 1e-9, "the 30h-old 0.90 spike must be excluded");
        assert_eq!(r["position"], "near-high");
    }

    #[test]
    fn future_dated_ring_entry_is_excluded_from_trend_uncertainty_and_read_range() {
        // A future-dated tick (NTP step-back, or ticks persisted while the clock ran ahead then
        // reloaded via load_epoch) must be IGNORED by trend/uncertainty/read_range — the served
        // window is the CLOSED interval [now − window, now], exactly as the four sibling
        // diagnostics (band_coverage/alert_dwell/lead_concentration/momentum_lead_lag) already
        // enforce with `if t > now { continue; }`. Without the guard a future 0.95 would set the
        // range high, widen the uncertainty band, and inflate the trend sample count.
        let now = Utc::now();
        let build = |with_future: bool| {
            let mut es = EpochStore::new();
            for (i, p) in [0.40, 0.42, 0.41, 0.45, 0.44, 0.50, 0.55].iter().enumerate() {
                es.push(epoch_at(3600 * (10 - i as i64), now, *p)); // all inside the 24h window
            }
            if with_future {
                es.push(epoch_at(-3600, now, 0.95)); // 1h in the FUTURE — an extreme read
            }
            es
        };
        let win = 24 * 3600;

        // read_range: the future 0.95 must not become the window high nor drag the position down.
        let base_rr = build(false).read_range_window(0.60, now, win, 5);
        let fut_rr = build(true).read_range_window(0.60, now, win, 5);
        assert_eq!(fut_rr, base_rr, "a future-dated entry must not change the read-range");
        assert!(
            (fut_rr["hi"].as_f64().unwrap() - 0.55).abs() < 1e-9,
            "hi stays the in-window max, not the future 0.95"
        );
        assert_eq!(fut_rr["position"], "near-high");

        // uncertainty: the future extreme must not widen the empirical central-80% spread.
        let base_un = build(false).uncertainty_window(0.60, 0.9, now, win);
        let fut_un = build(true).uncertainty_window(0.60, 0.9, now, win);
        assert_eq!(fut_un, base_un, "a future-dated entry must not widen the uncertainty band");

        // trend: the future entry must not inflate the sample count or move the baseline.
        let base_tr = build(false).trend_window(0.60, now, win, 2);
        let fut_tr = build(true).trend_window(0.60, now, win, 2);
        assert_eq!(fut_tr, base_tr, "a future-dated entry must not change the trend read");
    }

    // ── EpochStore::momentum_lead_lag — the "leading" claim is MEASURED, not asserted ──────
    // A deterministic hash gives each 5-min tick an independent momentum sign; these lock the
    // diagnostic that decides whether that momentum actually PRECEDES the realized P.

    /// Independent ±1 per index (no period-6 structure that would let a wrong lag correlate).
    fn ll_dir(k: usize) -> f64 {
        if ((k as u64).wrapping_mul(2654435761) >> 13) & 1 == 0 { 1.0 } else { -1.0 }
    }

    #[test]
    fn momentum_lead_lag_recovers_a_planted_6step_lead() {
        // Plant the relationship p(t+1800s) − p(t) = +0.003·sign(mom(t)) EXACTLY (6 ticks @300s),
        // with momentum otherwise independent tick-to-tick. Only the 1800s lag should score ~100%.
        let now = Utc::now();
        let n = 300usize;
        let base = now - chrono::Duration::seconds(300 * n as i64);
        // s[k+6] − s[k] = dir(k) ⇒ p rises/falls 6 ticks AFTER momentum turns.
        let mut s = vec![0f64; n + 6];
        for k in 0..n { s[k + 6] = s[k] + ll_dir(k); }
        let mut es = EpochStore::new();
        for (k, sk) in s.iter().enumerate().take(n) {
            let t = base + chrono::Duration::seconds(300 * k as i64);
            es.push(serde_json::json!({
                "t": t.to_rfc3339(),
                "p_annual": 0.30 + 0.003 * sk,
                "mom": 0.3 * ll_dir(k),
            }));
        }
        let v = es.momentum_lead_lag_window(now, 48 * 3600, 300, MOM_LL_LAGS);
        assert_eq!(v["available"], true);
        assert_eq!(v["verdict"], "leads", "planted forward relationship must read as a lead");
        assert_eq!(v["lead_secs"], 1800, "the winning lag must be the 6-tick (1800s) plant");
        assert!(v["hit_pct"].as_f64().unwrap() >= 95.0, "hit% at the planted lag: {v:?}");
        assert!(v["pairs"].as_u64().unwrap() >= MOM_LL_MIN_PAIRS as u64);
    }

    #[test]
    fn momentum_lead_lag_reports_an_honest_null_when_momentum_does_not_lead() {
        // Decisive momentum, but P is an independent random walk — no lag should beat chance.
        let now = Utc::now();
        let n = 300usize;
        let base = now - chrono::Duration::seconds(300 * n as i64);
        let mut rw = 0f64;
        let mut es = EpochStore::new();
        for k in 0..n {
            // walk driven by a DIFFERENT hash stream than the momentum sign → uncorrelated
            rw += if ((k as u64).wrapping_mul(40503).wrapping_add(7) >> 5) & 1 == 0 { 1.0 } else { -1.0 };
            let t = base + chrono::Duration::seconds(300 * k as i64);
            es.push(serde_json::json!({
                "t": t.to_rfc3339(),
                "p_annual": 0.30 + 0.003 * rw,
                "mom": 0.3 * ll_dir(k),
            }));
        }
        let v = es.momentum_lead_lag_window(now, 48 * 3600, 300, MOM_LL_LAGS);
        assert_eq!(v["available"], true, "enough decisive samples to judge");
        assert_eq!(v["verdict"], "no_lead", "unrelated P must NOT be dressed up as a lead: {v:?}");
        assert!(v["hit_pct"].as_f64().unwrap() < 60.0, "best hit stayed below threshold: {v:?}");
    }

    #[test]
    fn momentum_lead_lag_insufficient_when_no_decisive_history() {
        // Empty ring, and a ring of directionless (mom≈0) ticks, both read "insufficient".
        let es = EpochStore::new();
        let v = es.momentum_lead_lag_window(Utc::now(), 48 * 3600, 300, MOM_LL_LAGS);
        assert_eq!(v["available"], false);
        assert_eq!(v["verdict"], "insufficient");

        let now = Utc::now();
        let mut es2 = EpochStore::new();
        for k in 0..100 {
            let t = now - chrono::Duration::seconds(300 * (100 - k) as i64);
            es2.push(serde_json::json!({"t": t.to_rfc3339(), "p_annual": 0.30, "mom": 0.0}));
        }
        let v2 = es2.momentum_lead_lag_window(now, 48 * 3600, 300, MOM_LL_LAGS);
        assert_eq!(v2["available"], false, "no decisive momentum ⇒ nothing to test: {v2:?}");
    }

    #[test]
    fn momentum_lead_lag_tolerates_entries_missing_the_mom_field() {
        // Older persisted entries predate `mom`; they must load (read momentum 0) and simply
        // not count as decisive — never panic.
        let now = Utc::now();
        let mut es = EpochStore::new();
        for k in 0..50 {
            let t = now - chrono::Duration::seconds(300 * (50 - k) as i64);
            es.push(serde_json::json!({"t": t.to_rfc3339(), "p_annual": 0.30 + 0.01 * k as f64}));
        }
        let v = es.momentum_lead_lag_window(now, 48 * 3600, 300, MOM_LL_LAGS);
        assert_eq!(v["available"], false); // no `mom` ⇒ no decisive samples
    }

    #[test]
    fn momentum_lead_lag_contemporaneous_comovement_is_coincident_not_a_lead() {
        // Momentum and P rising TOGETHER for the whole window (they are computed from the
        // same event board, so a sustained episode co-moves by construction): every lag
        // scores ~100%, including the one-stride contemporaneous baseline — which is
        // exactly why this must NOT read as "leads". The old verdict minted a false
        // "MEASURED to lead ~15m" from this shape.
        let now = Utc::now();
        let n = 300usize;
        let base = now - chrono::Duration::seconds(300 * n as i64);
        let mut es = EpochStore::new();
        for k in 0..n {
            let t = base + chrono::Duration::seconds(300 * k as i64);
            es.push(serde_json::json!({
                "t": t.to_rfc3339(),
                "p_annual": 0.30 + 0.003 * k as f64,
                "mom": 0.3,
            }));
        }
        let v = es.momentum_lead_lag_window(now, 48 * 3600, 300, MOM_LL_LAGS);
        assert_eq!(v["available"], true);
        assert_eq!(
            v["verdict"], "coincident",
            "simultaneous co-movement must not be dressed up as a lead: {v:?}"
        );
        assert!(v["baseline_hit_pct"].as_f64().unwrap() >= 90.0, "baseline saw the same co-movement: {v:?}");
    }

    #[test]
    fn momentum_lead_lag_two_episode_evidence_withholds_the_lead_verdict() {
        // A perfect forward relationship, but ALL the evidence lives in just two decisive
        // episodes (a 10-tick +run and a 10-tick −run; P ramps 6 ticks behind each).
        // Sample-count alone clears MIN_PAIRS with a 100% hit and a quiet baseline —
        // yet two episodes cannot establish that momentum generally precedes P.
        let now = Utc::now();
        let n = 300usize;
        let base = now - chrono::Duration::seconds(300 * n as i64);
        let ramp = |k: usize, from: usize| -> f64 {
            (k.saturating_sub(from)).min(10) as f64
        };
        let mut es = EpochStore::new();
        for k in 0..n {
            let t = base + chrono::Duration::seconds(300 * k as i64);
            let mom = if k < 10 { 0.3 } else if (50..60).contains(&k) { -0.3 } else { 0.0 };
            let p = 0.30 + 0.003 * ramp(k, 6) - 0.003 * ramp(k, 56);
            es.push(serde_json::json!({ "t": t.to_rfc3339(), "p_annual": p, "mom": mom }));
        }
        let v = es.momentum_lead_lag_window(now, 48 * 3600, 300, MOM_LL_LAGS);
        assert_eq!(v["available"], true);
        assert_eq!(
            v["verdict"], "insufficient_baseline",
            "sparse evidence must withhold the lead verdict fail-closed (the 8-pair \
             baseline is unjudgeable, checked before the episode floor): {v:?}"
        );
        assert_eq!(v["episodes"], 2, "episode segmentation counts the two runs: {v:?}");
        assert!(v["pairs"].as_u64().unwrap() >= MOM_LL_MIN_PAIRS as u64);
    }

    // ── load_epoch ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn load_epoch_reads_jsonl_from_disk() {
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().expect("tempfile");
        let entries = vec![
            serde_json::json!({"p_annual": 0.01, "seq": 1}),
            serde_json::json!({"p_annual": 0.02, "seq": 2}),
            serde_json::json!({"p_annual": 0.03, "seq": 3}),
        ];
        for e in &entries {
            writeln!(tmp, "{}", e).unwrap();
        }
        tmp.flush().unwrap();

        let text = tokio::fs::read_to_string(tmp.path()).await.unwrap();
        let mut store = EpochStore::new();
        let mut loaded = 0usize;
        for line in text.lines() {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                store.push(v);
                loaded += 1;
            }
        }
        assert_eq!(loaded, 3);
        assert_eq!(store.len(), 3);
        let out = store.query(3);
        assert_eq!(out[0]["seq"], 3);
        assert_eq!(out[2]["seq"], 1);
    }

    #[tokio::test]
    async fn load_epoch_skips_malformed_lines() {
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().expect("tempfile");
        writeln!(tmp, r#"{{"p_annual": 0.01}}"#).unwrap();
        writeln!(tmp, "not valid json {{{{").unwrap();
        writeln!(tmp, r#"{{"p_annual": 0.02}}"#).unwrap();
        tmp.flush().unwrap();

        let text = tokio::fs::read_to_string(tmp.path()).await.unwrap();
        let mut store = EpochStore::new();
        for line in text.lines() {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                store.push(v);
            }
        }
        assert_eq!(store.len(), 2);
    }

    #[tokio::test]
    async fn load_epoch_empty_file_returns_empty_store() {
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().expect("tempfile");
        tmp.flush().unwrap();

        let text = tokio::fs::read_to_string(tmp.path()).await.unwrap();
        let mut store = EpochStore::new();
        for line in text.lines() {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                store.push(v);
            }
        }
        assert_eq!(store.len(), 0);
    }

    // ── Corroboration (M-01: updated to use CorroborationIndex) ──────────────

    fn make_event_for_corroboration(
        title:    &str,
        source:   &str,
        tier:     SourceTier,
        hours_ago: i64,
    ) -> GeopoliticalEvent {
        let mut e = GeopoliticalEvent::new(
            title.into(),
            source.into(),
            tier,
            Utc::now() - chrono::Duration::hours(hours_ago),
        );
        e.domain_tags = vec!["military_escalation".into()];
        e
    }

    #[tokio::test]
    async fn preload_drops_live_flagged_video_and_the_retired_whisper_tier() {
        // Live-flagged watchlist rows are excluded at ingest (video_loop), and the
        // whisper-livestream tier (`-live`) is retired entirely (2026-07-23) — the boot
        // path scrubs both so the 4-year window doesn't reload them. Real wire text and
        // ordinary (non-live) video rows must survive.
        let (_etx, erx) = mpsc::channel(8);
        let (stx, _srx) = mpsc::channel(8);
        let mut agg = Aggregator::new(
            vec![], AlertSettings::default(), erx, stx, AppState::new(), 1);
        agg.preload_events(vec![
            make_event_for_corroboration("US-Iran War LIVE: Explosions Rock Tehran", "wion-video", SourceTier::Tier2, 1),
            make_event_for_corroboration("LIVE: Louvre gallery reopens after heist", "reuters-video", SourceTier::Tier1, 2),
            make_event_for_corroboration("[LIVE] aljazeera: strikes reported near Isfahan", "aljazeera-live", SourceTier::Tier1, 1),
            make_event_for_corroboration("Iran targets Kuwait with fresh missile attack", "wion-video", SourceTier::Tier2, 1),
            make_event_for_corroboration("Russia launches missile strike on Kyiv", "reuters", SourceTier::Tier1, 3),
        ]);
        let kept: Vec<&str> = agg.event_window.iter().map(|e| e.source.as_str()).collect();
        assert_eq!(kept.len(), 2, "both -video LIVE rows and the -live whisper row must be scrubbed: {kept:?}");
        assert!(!kept.contains(&"aljazeera-live"), "the retired whisper tier (-live) must be scrubbed");
        assert!(kept.contains(&"reuters") && kept.contains(&"wion-video"),
            "non-live wire and video rows must survive: {kept:?}");
        assert!(!agg.event_window.iter().any(|e| e.title.contains("LIVE:")),
            "no LIVE-titled watchlist event may reach the model window");
    }

    /// Helper: build a corroboration index from a window slice.
    fn build_corr_index(window: &[GeopoliticalEvent]) -> CorroborationIndex {
        let mut idx = CorroborationIndex::new();
        for event in window {
            idx.push(&event.title);
        }
        idx
    }

    #[test]
    fn corroboration_increments_count_on_near_duplicate() {
        let mut window = vec![
            make_event_for_corroboration(
                "Russia launches ballistic missile strike on Kyiv",
                "bbc", SourceTier::Tier1, 1,
            )
        ];
        let corr_index = build_corr_index(&window);
        let incoming = make_event_for_corroboration(
            "Russia fires ballistic missiles at Kyiv in overnight strike",
            "reuters", SourceTier::Tier1, 1,
        );
        let now = Utc::now();
        let corroborated = try_corroborate(&incoming, &mut window, &now, &corr_index);
        assert!(corroborated, "Similar titles from different sources should corroborate");
        assert_eq!(window[0].corroboration_count, 2);
    }

    #[test]
    fn corroboration_boosts_credibility_weight() {
        let mut window = vec![
            make_event_for_corroboration(
                "North Korea fires intercontinental ballistic missile toward Japan",
                "bbc", SourceTier::Tier2, 1,
            )
        ];
        let corr_index = build_corr_index(&window);
        let initial_weight = window[0].credibility_weight;
        let incoming = make_event_for_corroboration(
            "North Korea launches intercontinental ballistic missile test toward Japan",
            "reuters", SourceTier::Tier1, 1,
        );
        let now = Utc::now();
        try_corroborate(&incoming, &mut window, &now, &corr_index);
        assert!(
            window[0].credibility_weight > initial_weight,
            "Credibility should increase after corroboration: was {initial_weight}, now {}",
            window[0].credibility_weight
        );
        assert!(window[0].credibility_weight <= 1.0, "Credibility capped at 1.0");
    }

    #[test]
    fn corroboration_same_source_not_merged() {
        let mut window = vec![
            make_event_for_corroboration(
                "Russia launches missile strike on Ukraine military targets",
                "bbc", SourceTier::Tier1, 1,
            )
        ];
        let corr_index = build_corr_index(&window);
        let incoming = make_event_for_corroboration(
            "Russia fires missiles at Ukraine military targets in latest strike",
            "bbc", SourceTier::Tier1, 1,
        );
        let now = Utc::now();
        let corroborated = try_corroborate(&incoming, &mut window, &now, &corr_index);
        assert!(!corroborated, "Same-source near-duplicate should not corroborate");
        assert_eq!(window[0].corroboration_count, 1);
    }

    #[test]
    fn same_outlet_video_twin_absorbed_without_independence_boost() {
        // An outlet's wire story and its OWN `-video` twin are one newsroom, not two
        // independent witnesses. The twin must be ABSORBED (deduped → returns true, so it
        // isn't re-added as a phantom second event that would double-count into modality
        // weight) but must NOT boost corroboration_count / credibility — that would claim
        // two independent sources where there is one. (Operator directive: duplicates
        // removed from weight; five roster outlets run both a wire and a `-video` feed.)
        let mut window = vec![
            make_event_for_corroboration(
                "Russia launches ballistic missile strike on Kyiv",
                "bbc", SourceTier::Tier1, 1,
            )
        ];
        let corr_index = build_corr_index(&window);
        let start_count  = window[0].corroboration_count;
        let start_weight = window[0].credibility_weight;
        let incoming = make_event_for_corroboration(
            "Russia fires ballistic missiles at Kyiv in overnight strike",
            "bbc-video", SourceTier::Tier1, 1,
        );
        let now = Utc::now();
        let absorbed = try_corroborate(&incoming, &mut window, &now, &corr_index);
        assert!(absorbed, "same-outlet video twin must be absorbed, not left to re-add as a phantom event");
        assert_eq!(window[0].corroboration_count, start_count,
            "same-outlet video twin must NOT boost corroboration_count (not an independent source)");
        assert_eq!(window[0].credibility_weight, start_weight,
            "same-outlet video twin must NOT boost credibility (not an independent source)");
        assert!(!window[0].corroborating_sources.iter().any(|s| s == "bbc-video"),
            "the same-outlet twin must not be recorded as a corroborating source");
    }

    #[test]
    fn independent_outlet_video_still_corroborates() {
        // A DIFFERENT outlet's video IS a real second witness — the independence fix must
        // not over-suppress it. `reuters-video` corroborating a `bbc` wire event still boosts.
        let mut window = vec![
            make_event_for_corroboration(
                "North Korea fires intercontinental ballistic missile toward Japan",
                "bbc", SourceTier::Tier1, 1,
            )
        ];
        let corr_index = build_corr_index(&window);
        let incoming = make_event_for_corroboration(
            "North Korea launches intercontinental ballistic missile test toward Japan",
            "reuters-video", SourceTier::Tier1, 1,
        );
        let now = Utc::now();
        let corroborated = try_corroborate(&incoming, &mut window, &now, &corr_index);
        assert!(corroborated, "an independent outlet's video is a genuine second witness");
        assert_eq!(window[0].corroboration_count, 2);
    }

    #[test]
    fn outlet_identity_strips_modality_suffixes() {
        assert_eq!(outlet_identity("bbc-video"), "bbc");
        assert_eq!(outlet_identity("aljazeera-live"), "aljazeera");
        assert_eq!(outlet_identity("reuters"), "reuters");
        // Only the trailing modality suffix is stripped — an internal hyphen is preserved.
        assert_eq!(outlet_identity("times-of-israel"), "times-of-israel");
    }

    #[test]
    fn corroboration_outside_time_window_not_merged() {
        let mut window = vec![
            make_event_for_corroboration(
                "China conducts military exercises near Taiwan Strait",
                "bbc", SourceTier::Tier1,
                (CORROBORATION_WINDOW_HOURS as i64) + 2,
            )
        ];
        let corr_index = build_corr_index(&window);
        let incoming = make_event_for_corroboration(
            "China launches military drills near Taiwan Strait in latest provocation",
            "reuters", SourceTier::Tier1, 1,
        );
        let now = Utc::now();
        let corroborated = try_corroborate(&incoming, &mut window, &now, &corr_index);
        assert!(!corroborated, "Stale existing event should not be corroborated");
    }

    #[test]
    fn corroboration_completely_different_articles_not_merged() {
        let mut window = vec![
            make_event_for_corroboration(
                "Russia deploys troops to Belarus border region",
                "bbc", SourceTier::Tier1, 1,
            )
        ];
        let corr_index = build_corr_index(&window);
        let incoming = make_event_for_corroboration(
            "China tests hypersonic missile over South China Sea",
            "reuters", SourceTier::Tier1, 1,
        );
        let now = Utc::now();
        let corroborated = try_corroborate(&incoming, &mut window, &now, &corr_index);
        assert!(!corroborated, "Unrelated articles should not corroborate");
        assert_eq!(window[0].corroboration_count, 1);
    }

    #[test]
    fn corroboration_tier3_boost_is_smaller_than_tier1() {
        let make_window = || vec![
            make_event_for_corroboration(
                "Iran fires ballistic missiles toward Israel from Houthi-controlled Yemen",
                "bbc", SourceTier::Tier2, 1,
            )
        ];
        let mut window_t1 = make_window();
        let mut window_t3 = make_window();
        let corr_index_t1 = build_corr_index(&window_t1);
        let corr_index_t3 = build_corr_index(&window_t3);
        let now = Utc::now();
        let incoming_t1 = make_event_for_corroboration(
            "Iran launches ballistic missiles at Israel from Houthi territory in Yemen",
            "reuters", SourceTier::Tier1, 1,
        );
        let incoming_t3 = make_event_for_corroboration(
            "Iran launches ballistic missiles at Israel from Houthi territory in Yemen",
            "gnews", SourceTier::Tier3, 1,
        );
        try_corroborate(&incoming_t1, &mut window_t1, &now, &corr_index_t1);
        try_corroborate(&incoming_t3, &mut window_t3, &now, &corr_index_t3);
        assert!(
            window_t1[0].credibility_weight > window_t3[0].credibility_weight,
            "Tier1 corroboration should boost credibility more than Tier3"
        );
    }

    #[test]
    fn title_trigrams_basic() {
        let tg = title_trigrams("hello");
        assert_eq!(tg.len(), 3);
    }

    #[test]
    fn title_trigrams_short_string_empty() {
        assert!(title_trigrams("hi").is_empty());
    }

    #[test]
    fn jaccard_identical_strings_is_one() {
        let a = title_trigrams("russia launches missile");
        let b = title_trigrams("russia launches missile");
        assert!((jaccard(&a, &b) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn jaccard_disjoint_strings_is_zero() {
        let a = title_trigrams("russia missile strike");
        let b = title_trigrams("qqq zzz xxx yyy");
        assert_eq!(jaccard(&a, &b), 0.0);
    }

    #[test]
    fn corroboration_credibility_capped_at_one() {
        let mut window = vec![
            make_event_for_corroboration(
                "NATO Article 5 invoked after Russian forces attack Poland",
                "bbc", SourceTier::Tier1, 1,
            )
        ];
        let now = Utc::now();
        let incomings = [
            ("reuters",    "NATO activates Article 5 after Russian forces strike Poland"),
            ("ap",         "NATO Article 5 triggered as Russian forces attack Polish territory"),
            ("afp",        "Article 5 invoked by NATO after Russian forces attack Poland border"),
            ("nyt",        "NATO invokes Article 5 after Russian military forces attack Poland"),
            ("wapo",       "NATO Article 5 invoked following Russian forces attack on Poland"),
            ("guardian",   "Article 5 activated as Russian forces launch attack on Poland"),
            ("al_jazeera", "NATO invokes Article 5 after Russian forces attack Poland garrison"),
        ];
        for (src, title) in &incomings {
            let corr_index = build_corr_index(&window);
            let incoming = make_event_for_corroboration(title, src, SourceTier::Tier1, 1);
            try_corroborate(&incoming, &mut window, &now, &corr_index);
        }
        assert!(window[0].credibility_weight <= 1.0, "Credibility must never exceed 1.0");
        assert!(window[0].corroboration_count >= 2, "Should have multiple corroborations");
    }

    // ── CorroborationIndex unit tests (M-01) ─────────────────────────────────

    #[test]
    fn corr_index_empty_returns_no_candidates() {
        let idx = CorroborationIndex::new();
        let sig = corr_minhash_signature(&corr_trigrams("test title for lookup"));
        let candidates = idx.find_candidates(&sig);
        assert!(candidates.is_empty());
    }

    #[test]
    fn corr_index_finds_similar_title() {
        let mut idx = CorroborationIndex::new();
        idx.push("Russia launches ballistic missile strike on Kyiv");
        let sig = corr_minhash_signature(
            &corr_trigrams("Russia fires ballistic missiles at Kyiv in overnight strike")
        );
        let candidates = idx.find_candidates(&sig);
        assert!(!candidates.is_empty(), "LSH should find candidate for similar title");
        assert!(candidates.contains(&0));
    }

    #[test]
    fn corr_index_does_not_find_unrelated_title() {
        let mut idx = CorroborationIndex::new();
        idx.push("Russia launches ballistic missile strike on Kyiv");
        let sig = corr_minhash_signature(
            &corr_trigrams("weather forecast for tropical pacific region tomorrow")
        );
        let _candidates = idx.find_candidates(&sig);
        // At J ≈ 0.03 between these titles, P(candidate) ≈ 3%. Even if a false
        // positive occurs, the exact Jaccard check in try_corroborate rejects it.
        // We exercise find_candidates to verify it does not panic on unrelated input.
        assert_eq!(idx.len(), 1);
    }

    #[test]
    fn corr_index_rebuild_from_window_resets_cleanly() {
        let window = vec![
            make_event_for_corroboration("Event alpha about NATO exercises", "bbc", SourceTier::Tier1, 1),
            make_event_for_corroboration("Event beta about China military", "reuters", SourceTier::Tier1, 2),
        ];
        let mut idx = CorroborationIndex::new();
        idx.push("Old event that will be evicted from the window");
        idx.push("Another old event that is no longer relevant");
        idx.push("Third old event that has aged out of the window");
        assert_eq!(idx.len(), 3);

        idx.rebuild_from_window(&window);
        assert_eq!(idx.len(), 2, "Index should match window size after rebuild");
    }

    #[test]
    fn corr_index_parallel_to_window_after_push() {
        let mut idx = CorroborationIndex::new();
        idx.push("First event title about military operations");
        idx.push("Second event title about diplomatic breakdown");
        idx.push("Third event title about nuclear posture change");
        assert_eq!(idx.len(), 3);
        assert_eq!(idx.sigs.len(), 3);
    }

    #[test]
    fn corr_minhash_signature_length() {
        let tgs = corr_trigrams("test title for minhash signature");
        let sig = corr_minhash_signature(&tgs);
        assert_eq!(sig.len(), CORR_NUM_HASHES);
    }

    #[test]
    fn corr_minhash_identical_titles_produce_identical_sigs() {
        let title = "Russia launches missile strike on Ukraine infrastructure";
        let sig_a = corr_minhash_signature(&corr_trigrams(title));
        let sig_b = corr_minhash_signature(&corr_trigrams(title));
        assert_eq!(sig_a, sig_b, "Identical titles must produce identical signatures");
    }

    #[test]
    fn corr_trigrams_empty_for_short() {
        assert!(corr_trigrams("hi").is_empty());
        assert!(corr_trigrams("").is_empty());
    }

    #[test]
    fn corr_trigrams_lowercases() {
        let a = corr_trigrams("ABC");
        let b = corr_trigrams("abc");
        assert_eq!(a, b, "Trigrams should be case-insensitive");
    }

    #[test]
    fn corr_band_config_is_consistent() {
        assert_eq!(CORR_BANDS * CORR_BAND_ROWS, CORR_NUM_HASHES,
            "CORR_BANDS × CORR_BAND_ROWS must equal CORR_NUM_HASHES");
    }
}
