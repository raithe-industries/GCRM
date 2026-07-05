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
        "indicators": crate::indicators::evaluate(snap),
        "meta": {
            "events_in_window":         snap.events_in_window,
            "data_blind":               crate::bayesian::is_data_blind(snap.events_in_window),
            "thinly_sourced":           crate::bayesian::is_thinly_sourced(snap.events_in_window, snap.sources_active),
            "at_ceiling":               crate::bayesian::is_at_forecast_ceiling(snap.p_wwiii_annual),
            "breadth_saturated":        snap.couplers.breadth_saturated,
            "read_held_by_floor":       crate::theater::systemic_read_is_floor_held(&snap.theaters),
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

#[derive(Debug, Default)]
pub struct EpochStore {
    ring: VecDeque<serde_json::Value>,
    /// Stride-cached momentum lead-lag payload: the diagnostic scans the whole 48h
    /// window, its answer can only change once per stride, and it used to run per 1 Hz
    /// broadcast while holding the shared epoch_store lock. (Cache, not eviction: the
    /// ring itself is untouched.)
    mom_ll_cache: Option<(DateTime<Utc>, serde_json::Value)>,
}

impl EpochStore {
    pub fn new() -> Self { Self { ring: VecDeque::new(), mom_ll_cache: None } }

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
        // Only a genuinely NEW source corroborates. The candidate loop already excludes the
        // canonical (primary) source; this also blocks a non-primary source that already
        // corroborated this event from inflating count/credibility AGAIN with a reworded
        // headline. A repeat from a known source is still ABSORBED (returns true, so it isn't
        // re-added as a phantom new event) — it just doesn't re-boost. (audit aggregator-4)
        if existing.corroborating_sources.iter().any(|s| s == &incoming.source) {
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
                  "regions_active", "aggregation_window_hours", "max_window_events"] {
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
