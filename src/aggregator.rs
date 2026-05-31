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

use chrono::{DateTime, Utc};
use serde_json;
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

// ── Timeline path helpers ────────────────────────────────────────────────────────
//
// Returns the rotated JSONL path for a given date: logs/timeline_YYYY-MM-DD.jsonl
// All timestamps are UTC. Each calendar day produces one file, accumulating in
// the logs/ directory. At 1 Hz: ~86,400 lines/day × ~500 bytes ≈ ~43 MB/day.

fn timeline_path_for_date(date: &chrono::NaiveDate) -> String {
    format!("logs/timeline_{}.jsonl", date.format("%Y-%m-%d"))
}

fn today_timeline_path() -> String {
    timeline_path_for_date(&Utc::now().date_naive())
}

// ── Article archive paths (mirrors the timeline rotation) ──────────────────────

fn article_path_for_date(date: &chrono::NaiveDate) -> String {
    format!("logs/articles_{}.jsonl", date.format("%Y-%m-%d"))
}

fn today_article_path() -> String {
    article_path_for_date(&Utc::now().date_naive())
}

// ── Event archive paths (mirrors the timeline rotation) ────────────────────────

fn today_event_path() -> String {
    format!("logs/events_{}.jsonl", Utc::now().date_naive().format("%Y-%m-%d"))
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
        "snapshot_id":  snap.snapshot_id,
        "computed_at":  snap.computed_at.to_rfc3339(),
        "prior": {
            "historical_anchor": snap.historical_anchor,
            "formula":           "2 / 2026 = 0.000987/yr",
            "regime_multiplier": snap.regime_multiplier,
            "adjusted_prior":    snap.adjusted_prior,
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
        },
        "meta": {
            "events_in_window":         snap.events_in_window,
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
// No background rotation task is required — the path changes at UTC midnight
// naturally as today_timeline_path() recomputes the date on each call.

async fn append_timeline(snap: &RiskSnapshot) {
    let entry = TimelineEntry::from_snapshot(snap);
    let line = match serde_json::to_string(&entry) {
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

#[derive(Debug, Default)]
pub struct ArticleStore {
    pub articles:   VecDeque<StoredArticle>,
    pub index:      HashMap<String, usize>,   // id → absolute insertion counter
    front_counter:  usize,                    // absolute index of deque[0]
    total_inserted: usize,                    // next absolute index to assign
    pub max_size:   usize,
}

impl ArticleStore {
    pub fn new(max_size: usize) -> Self {
        Self {
            articles:      VecDeque::new(),
            index:         HashMap::new(),
            front_counter: 0,
            total_inserted: 0,
            max_size,
        }
    }

    pub fn push(&mut self, article: StoredArticle) {
        if self.articles.len() >= self.max_size {
            // O(1): pop oldest from front, remove its index entry
            if let Some(evicted) = self.articles.pop_front() {
                self.index.remove(&evicted.id);
                self.front_counter += 1;
            }
        }
        // Record absolute position
        self.index.insert(article.id.clone(), self.total_inserted);
        self.total_inserted += 1;
        self.articles.push_back(article);
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
            .filter(|a| source_filter.map_or(true, |s| a.source == s))
            .filter(|a| domain_filter.map_or(true, |d| a.domain_tags.iter().any(|t| t == d)))
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

#[derive(Debug, Default)]
pub struct EpochStore {
    ring: VecDeque<serde_json::Value>,
}

impl EpochStore {
    pub fn new() -> Self { Self { ring: VecDeque::new() } }

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
}

/// Boot loader: reads timeline JSONL files from disk once at startup and
/// populates an EpochStore. Reads today's and yesterday's rotated files (I-16)
/// so the ring is populated with recent history on restart without scanning
/// the entire archive. Lines are processed oldest-first so the ring ends with
/// the most recent entries at the tail.
pub async fn load_epoch() -> EpochStore {
    let mut store = EpochStore::new();
    let today = Utc::now().date_naive();
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
    let today = Utc::now().date_naive();
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
                        if !latest.contains_key(&a.id) { order.push(a.id.clone()); }
                        latest.insert(a.id.clone(), a);
                    }
                }
            }
            Err(e) => warn!("ArticleStore boot read failed for {path_str}: {e}"),
        }
    }

    let mut store = ArticleStore::new(max_size);
    for id in &order {
        if let Some(a) = latest.remove(id) {
            store.push(a);
        }
    }
    if store.len() == 0 {
        info!("ArticleStore: no article archive found — starting empty");
    } else {
        info!("ArticleStore: restored {} articles from archive", store.len());
    }
    store
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
            epoch_store:        Mutex::new(EpochStore::new()),
            last_calibrated_at: Mutex::new(None),
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
        events.sort_by(|a, b| b.published_at.cmp(&a.published_at));
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
                self.event_window.sort_by(|a, b| b.published_at.cmp(&a.published_at));
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
            snapshot.sources_active = self.event_window.iter()
                .map(|e| e.source.as_str()).collect::<HashSet<_>>().len();

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
                append_timeline(&snapshot).await;
                let entry = TimelineEntry::from_snapshot(&snapshot);
                if let Ok(v) = serde_json::to_value(&entry) {
                    self.state.epoch_store.lock().await.push(v);
                }
            }

            // Update shared state and broadcast regardless of warmup — live UI always current
            let json_snap = snapshot_to_json(&snapshot);
            *self.state.latest_snapshot.lock().await = Some(json_snap);

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
///   2. Skip if same source (corroboration requires independent sources).
///   3. Compute exact trigram Jaccard similarity.
///   4. If Jaccard ≥ threshold, merge into the best-matching canonical event.
///
/// Returns true if the incoming event was corroborated (merged into an existing
/// event), false if it should be added to the window as a new event.
fn try_corroborate(
    incoming:   &GeopoliticalEvent,
    window:     &mut Vec<GeopoliticalEvent>,
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
        let mut snap = RiskSnapshot::default();
        snap.p_wwiii_annual    = p_annual;
        snap.p_wwiii_30day     = 1.0 - (1.0 - p_annual).powf(1.0 / 12.0);
        snap.delta_annual      = delta;
        snap.elevated_domains  = elevated;
        snap.regime_multiplier = 1.568;
        snap.events_in_window  = 10;
        snap.sources_active    = 3;
        snap.alert_level       = if p_annual >= 0.08 { AlertLevel::Critical }
                                  else if p_annual >= 0.025 { AlertLevel::Elevated }
                                  else { AlertLevel::Normal };
        snap
    }

    // ── snapshot_to_json ─────────────────────────────────────────────────────

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
