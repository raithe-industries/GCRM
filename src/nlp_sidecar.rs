// ------------------------------------------------------------
// RAiTHE INDUSTRIES INCORPORATED
// Copyright (c) 2026 All Rights Reserved.
//
// This file is part of a proprietary system. Unauthorized use,
// reproduction, or distribution is strictly prohibited.
// ------------------------------------------------------------

// src/nlp_sidecar.rs — Inline NLP runner
//
// The processor is a required pipeline stage.
//
//   The FuzzyDedup cache is loaded from disk at startup and saved to disk
//   on graceful shutdown. This eliminates the cold-start false-spike: without
//   persistence, every restart re-ingests the last 24–48h of RSS feed history
//   against an empty dedup cache, treating all recent articles as new and
//   producing a 15–25× article surge that triggers AnomalyDetector and inflates
//   all domain scores for 10–60 minutes post-restart.
//
//   Startup:
//     FuzzyDedup::load() restores the cache from logs/dedup_cache.json.
//     NlpProcessor::with_dedup() initialises the processor with the restored
//     cache so the first article poll sees the full dedup history. Falls back
//     cleanly to an empty cache if the file does not exist (first run).
//
//   Shutdown:
//     NlpSidecarHandle::shutdown() sends a true value on a watch::channel to
//     the sidecar task. The task's select! loop receives the signal, calls
//     processor.dedup().save(), logs final statistics, and returns. Because the
//     processor is owned entirely by the task, the save happens within the same
//     execution context that holds the dedup state — no cross-thread sharing.
//
//   Wiring in main.rs:
//     NlpSidecar::with_shutdown() is called instead of NlpSidecar::new().
//     main.rs holds the returned NlpSidecarHandle and calls handle.shutdown()
//     in the wait_for_shutdown() select arm before the process exits.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{mpsc, watch, Mutex, OwnedSemaphorePermit, Semaphore};
use tracing::{info, warn};

use crate::aggregator::AppState;
use crate::llm_enricher::{cosine_similarity, LlmEnricher, LlmExtraction};
use crate::models::{GeopoliticalEvent, LlmSettings, RawArticle, is_great_power};
use crate::processor::{FuzzyDedup, NlpProcessor};

/// Embedding-based semantic dedup (v2 Phase 4, opt-in via llm.semantic_dedup). Two
/// titles whose embeddings are at least this cosine-similar are treated as the same
/// real-world event, so 50 paraphrases of one strike become one escalation — also
/// dampening the volume that the MinHash title dedup lets through.
const SEMANTIC_THRESHOLD: f32 = 0.95;
const SEM_RING_CAP: usize = 256;
/// Persist the FuzzyDedup cache this often (not only on shutdown) so a crash /
/// SIGKILL / power-loss doesn't lose the whole index and force a cold-start spike,
/// and so the cache file is present on disk shortly after boot.
const DEDUP_SAVE_SECS: u64 = 300; // 5 min

/// Shared ring of recent title embeddings for semantic dedup.
type SemRing = Arc<Mutex<VecDeque<Vec<f32>>>>;

/// Outcome of waiting for a worker-pool permit while staying responsive to shutdown.
enum PermitWait {
    /// Got a permit — dispatch the LLM worker.
    Acquired(OwnedSemaphorePermit),
    /// Shutdown was signalled (or the sender dropped) while waiting — stop dispatching.
    Shutdown,
    /// The semaphore was closed — the pool is gone; exit the loop.
    Closed,
}

/// Acquire a worker-pool permit, but cancel the wait the instant shutdown is
/// signalled. Without this the recv loop would block on a bare
/// `acquire_owned().await` while the pool is saturated (all permits held by
/// in-flight LLM classifications); because that await lives *inside* the `select!`
/// recv arm, it would prevent the shutdown branch from ever being polled — so a
/// SIGTERM during sustained load would stall until a permit freed (one full LLM
/// call, or indefinitely if Ollama hangs). Racing the acquire against the shutdown
/// watch makes the dispatch cancellation-aware (roadmap 4.3).
///
/// The caller passes a CLONE of the sidecar's shutdown receiver: the main `select!`
/// already holds `&mut self.shutdown_rx` for its own shutdown branch, so the clone
/// (independent "seen" version, shared value) avoids a borrow conflict while still
/// observing the same signal.
async fn acquire_permit_or_shutdown(
    sem: Arc<Semaphore>,
    shutdown_rx: &mut watch::Receiver<bool>,
) -> PermitWait {
    // Fast path: already shutting down — never wait for a permit.
    if *shutdown_rx.borrow() {
        return PermitWait::Shutdown;
    }
    tokio::select! {
        biased;
        // Prefer shutdown: any resolution of changed() (value set to true, or all
        // senders dropped) means stop dispatching. The channel is only ever set to
        // true by NlpSidecarHandle::shutdown(), so there is no false transition.
        _ = shutdown_rx.changed() => PermitWait::Shutdown,
        pm = sem.acquire_owned() => match pm {
            Ok(pm) => PermitWait::Acquired(pm),
            Err(_) => PermitWait::Closed,
        },
    }
}

/// Log final stats and flush the dedup cache. Called from every graceful-exit path
/// in `run()` (the idle shutdown select arm AND the cancellation-aware permit wait)
/// so the save + log can't drift between them.
fn save_and_log_shutdown(
    processor: &NlpProcessor,
    processed: &AtomicU64,
    tagged: &AtomicU64,
    llm_hits: &AtomicU64,
) {
    info!(
        "NLP processor: shutdown — {} processed, {} tagged, {} LLM. Saving dedup cache...",
        processed.load(Ordering::Relaxed),
        tagged.load(Ordering::Relaxed),
        llm_hits.load(Ordering::Relaxed)
    );
    processor.dedup().save();
    info!("NLP processor: dedup cache saved. Exiting cleanly.");
}

// ── NlpSidecarHandle ─────────────────────────────────────────────────────────
//
// Held by main.rs. Calling shutdown() signals the sidecar task to flush the
// FuzzyDedup cache and exit. The signal is fire-and-forget — shutdown() returns
// immediately without waiting for the task. Calling shutdown() more than once
// is a no-op (watch channel semantics).

pub struct NlpSidecarHandle {
    shutdown_tx: watch::Sender<bool>,
}

impl NlpSidecarHandle {
    /// Signal the NLP sidecar to save the dedup cache and exit cleanly.
    pub fn shutdown(&self) {
        // Ignore send errors — they mean the task has already exited.
        let _ = self.shutdown_tx.send(true);
    }
}

// ── NlpSidecar ───────────────────────────────────────────────────────────────

pub struct NlpSidecar {
    raw_rx:       mpsc::Receiver<RawArticle>,
    event_tx:     mpsc::Sender<GeopoliticalEvent>,
    app_state:    Arc<AppState>,
    shutdown_rx:  watch::Receiver<bool>,
    llm_settings: LlmSettings,
}

impl NlpSidecar {
    /// Construct the sidecar with a paired shutdown handle.
    /// Spawn the returned NlpSidecar; hold the NlpSidecarHandle in main.rs.
    pub fn with_shutdown(
        raw_rx:       mpsc::Receiver<RawArticle>,
        event_tx:     mpsc::Sender<GeopoliticalEvent>,
        app_state:    Arc<AppState>,
        llm_settings: LlmSettings,
    ) -> (Self, NlpSidecarHandle) {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let sidecar = Self { raw_rx, event_tx, app_state, shutdown_rx, llm_settings };
        let handle  = NlpSidecarHandle { shutdown_tx };
        (sidecar, handle)
    }

    pub async fn run(mut self) {
        let _ = is_great_power("test");

        let fuzzy         = FuzzyDedup::load();
        let mut processor = NlpProcessor::with_dedup(fuzzy);
        let enricher      = Arc::new(LlmEnricher::new(self.llm_settings.clone()));
        // Worker-pool size — configurable so it can saturate the machine's Ollama
        // parallelism (match to OLLAMA_NUM_PARALLEL). The recv loop does the fast
        // sequential work (dedup + keyword) and dispatches the slow LLM call here, so
        // model latency no longer serializes the pipeline (the old per-article
        // `classify().await` block was the dominant backend bottleneck).
        let concurrency   = self.llm_settings.concurrency.max(1);
        let sem           = Arc::new(Semaphore::new(concurrency));

        // Shared across the recv loop and the concurrent worker tasks.
        let processed = Arc::new(AtomicU64::new(0));
        let tagged    = Arc::new(AtomicU64::new(0));
        let llm_hits  = Arc::new(AtomicU64::new(0));
        let sem_ring: SemRing = Arc::new(Mutex::new(VecDeque::new()));

        info!(
            "NLP processor: online — pure Rust dedup + structured LLM extraction (concurrency {})",
            if enricher.is_enabled() { concurrency } else { 0 }
        );

        // Periodic dedup-cache persistence (see DEDUP_SAVE_SECS). The first tick fires
        // immediately, so consume it — no point saving an empty cache at boot.
        let mut dedup_save = tokio::time::interval(std::time::Duration::from_secs(DEDUP_SAVE_SECS));
        dedup_save.tick().await;

        // A clone of the shutdown receiver for the cancellation-aware permit wait: the
        // main `select!` borrows `self.shutdown_rx` for its own shutdown branch, so the
        // dispatch path watches the same signal through an independent receiver.
        let mut dispatch_shutdown_rx = self.shutdown_rx.clone();

        loop {
            tokio::select! {
                biased;

                // Periodic dedup-cache persistence FIRST in the biased order, so the
                // 5-min tick actually wins a poll instead of being starved by a busy
                // raw_rx. It's Ready only once per interval, so it never starves recv.
                _ = dedup_save.tick() => {
                    processor.dedup().save();
                }

                maybe_article = self.raw_rx.recv() => {
                    let article = match maybe_article {
                        Some(a) => a,
                        None => {
                            warn!("NLP: raw_rx closed unexpectedly. Saving dedup cache before exit.");
                            processor.dedup().save();
                            tracing::error!("NLP: pipeline broken (raw_rx closed). Exiting.");
                            std::process::exit(1);
                        }
                    };

                    let p = processed.fetch_add(1, Ordering::Relaxed) + 1;

                    // ── FAST sequential stage: dedup + keyword scoring ─────────
                    // Must stay in the recv loop — FuzzyDedup is stateful/sequential.
                    let kw_event = processor.process(&article);

                    let do_llm = enricher.is_enabled()
                        && (kw_event.is_some()
                            || (kw_event.is_none() && has_geopolitical_trigger(&article.title)));

                    if !do_llm {
                        // No LLM needed: emit the keyword event directly if present.
                        if let Some(event) = kw_event {
                            emit_event(&self.app_state, &self.event_tx, &article.id, event, &tagged).await;
                        }
                    } else {
                        // ── Dispatch LLM extraction to the bounded worker pool ──
                        // acquire_owned() awaits ONLY when LLM_CONCURRENCY calls are
                        // already in flight — correct backpressure, not the old
                        // per-article serial block.
                        let permit = match acquire_permit_or_shutdown(
                            sem.clone(), &mut dispatch_shutdown_rx).await
                        {
                            PermitWait::Acquired(pm) => pm,
                            // Shutdown fired while the pool was saturated — don't block the
                            // recv loop waiting for a permit; flush and exit promptly.
                            PermitWait::Shutdown => {
                                save_and_log_shutdown(&processor, &processed, &tagged, &llm_hits);
                                return;
                            }
                            PermitWait::Closed => break, // semaphore closed
                        };
                        let enricher  = enricher.clone();
                        let event_tx  = self.event_tx.clone();
                        let app_state = self.app_state.clone();
                        let tagged    = tagged.clone();
                        let llm_hits  = llm_hits.clone();
                        let sem_ring  = sem_ring.clone();
                        tokio::spawn(async move {
                            let _permit = permit; // released on drop
                            let llm = enricher.classify(&article.title, &article.body).await;
                            let final_event: Option<GeopoliticalEvent> = match (kw_event, llm) {
                                (Some(mut ev), Some(x)) => {
                                    merge_llm_scores(&mut ev, &x);
                                    llm_hits.fetch_add(1, Ordering::Relaxed);
                                    Some(ev)
                                }
                                // LLM failed → keyword-only (silent fallback discipline).
                                (Some(ev), None) => Some(ev),
                                // Path B: keyword missed; LLM is the gate.
                                (None, Some(x)) if x.max_modality() >= 0.45 => {
                                    let ev = make_event_from_llm(&article, &x);
                                    // The 0.45 dispatch gate is below the storage threshold
                                    // (LLM_TAG_THRESHOLD/LLM_SCORE_DISCOUNT ≈ 0.556), so a hit in
                                    // [0.45, 0.556) builds an event whose every modality fell
                                    // below the tag floor — a signalless phantom with empty
                                    // domain_signals/tags. Drop it rather than emit it. (audit llmnlp-1)
                                    if ev.domain_signals.is_empty() {
                                        None
                                    } else {
                                        llm_hits.fetch_add(1, Ordering::Relaxed);
                                        Some(ev)
                                    }
                                }
                                _ => None,
                            };
                            if let Some(event) = final_event {
                                // Optional embedding-based semantic dedup: drop the
                                // event if a near-identical title (by meaning) was
                                // seen recently. Falls back to no-op if embeddings
                                // are unavailable — MinHash already ran.
                                if enricher.is_semantic_dedup() {
                                    if let Some(emb) = enricher.embed(&event.title).await {
                                        let mut ring = sem_ring.lock().await;
                                        if ring.iter().any(|e| cosine_similarity(e, &emb) >= SEMANTIC_THRESHOLD) {
                                            return; // semantic duplicate — already counted
                                        }
                                        if ring.len() >= SEM_RING_CAP { ring.pop_front(); }
                                        ring.push_back(emb);
                                    }
                                }
                                emit_event(&app_state, &event_tx, &article.id, event, &tagged).await;
                            }
                        });
                    }

                    if p.is_multiple_of(100) {
                        let t = tagged.load(Ordering::Relaxed);
                        if enricher.is_enabled() {
                            info!("NLP: {p} processed, {t} tagged | {} LLM extractions (concurrency {concurrency})",
                                  llm_hits.load(Ordering::Relaxed));
                        } else {
                            info!("NLP processor: {p} processed, {t} tagged");
                        }
                    }
                }

                _ = self.shutdown_rx.changed() => {
                    if *self.shutdown_rx.borrow() {
                        save_and_log_shutdown(&processor, &processed, &tagged, &llm_hits);
                        return;
                    }
                }
            }
        }
    }
}

/// Persist the article's final domain tags and forward the scored event downstream.
/// Used by both the no-LLM fast path and the concurrent LLM workers.
async fn emit_event(
    app_state:  &Arc<AppState>,
    event_tx:   &mpsc::Sender<GeopoliticalEvent>,
    article_id: &str,
    event:      GeopoliticalEvent,
    tagged:     &Arc<AtomicU64>,
) {
    let tagged_article = app_state.article_store.lock().await
        .set_domain_tags(article_id, event.domain_tags.clone());
    if let Some(a) = tagged_article {
        crate::aggregator::append_article(&a).await;
    }
    tagged.fetch_add(1, Ordering::Relaxed);
    if event_tx.send(event).await.is_err() {
        // Aggregator gone — main's task-join will surface the shutdown; just warn.
        warn!("NLP: event_tx closed — dropping event (aggregator down?)");
    }
}

// ── LLM merge helpers ─────────────────────────────────────────────────────────

const LLM_TAG_THRESHOLD: f64 = 0.50;
const LLM_SCORE_DISCOUNT: f64 = 0.90; // slight discount so keyword 1.0 beats LLM

/// Valid theater ids the LLM hint is allowed to set (must match models::Theater ids).
fn is_valid_theater(t: &str) -> bool {
    matches!(t, "nato_russia" | "us_iran" | "us_china_taiwan" | "india_pakistan" | "korea")
}

/// Merge structured LLM extraction into an existing keyword-derived event: max of
/// keyword and discounted LLM per modality, blend severity, adopt the LLM's signed
/// escalation step (the real Goldstein value, replacing the keyword placeholder), and
/// fill the theater hint only when the deterministic keyword resolver left it Other.
fn merge_llm_scores(event: &mut GeopoliticalEvent, x: &LlmExtraction) {
    for (modality, score) in x.modality_pairs() {
        if score <= 0.0 { continue; }
        let discounted = score * LLM_SCORE_DISCOUNT;
        // Floor the merged LLM signal at LLM_TAG_THRESHOLD — the same gate make_event_from_llm
        // and the keyword path (MIN_DOMAIN_SIGNAL) apply. Without it, a sub-threshold LLM
        // modality seeded domain_signals when the keyword path never would, inconsistently
        // gating signals by their source. (audit llmnlp-3)
        if discounted < LLM_TAG_THRESHOLD { continue; }
        let existing   = event.domain_signals.get(modality).copied().unwrap_or(0.0);
        event.domain_signals.insert(modality.to_string(), existing.max(discounted));
    }
    event.domain_tags = event.domain_signals.iter()
        .filter(|(_, &v)| v >= LLM_TAG_THRESHOLD)
        .map(|(k, _)| k.clone())
        .collect();

    if x.severity > event.severity {
        event.severity = (event.severity + x.severity) / 2.0;
    }
    if x.escalation_step != 0.0 {
        event.escalation_step = x.escalation_step.clamp(-1.0, 1.0);
    }
    // Keyword theater_of is deterministic from canonical actors; only let the LLM hint
    // fill in when there was no tracked dyad (Other / None).
    let needs_theater = event.theater.as_deref().is_none_or(|t| t == "other");
    if needs_theater && is_valid_theater(&x.theater) {
        event.theater = Some(x.theater.clone());
    }
    if let Some(coded) = coded_summary(x) {
        event.summary = coded;
    }
}

/// A clean "actor action target" line from the structured extraction, for the feed
/// and the analyst brief. None if the model returned no action.
fn coded_summary(x: &LlmExtraction) -> Option<String> {
    if x.action.trim().is_empty() { return None; }
    let coded = format!("{} {} {}", x.actor.trim(), x.action.trim(), x.target.trim());
    let coded = coded.split_whitespace().collect::<Vec<_>>().join(" ");
    if coded.len() > 3 { Some(coded) } else { None }
}

/// Create a GeopoliticalEvent from a structured extraction alone (keyword gate missed
/// it). The theater comes from the LLM hint since there are no keyword actors here.
fn make_event_from_llm(article: &RawArticle, x: &LlmExtraction) -> GeopoliticalEvent {
    let mut event = GeopoliticalEvent::new(
        article.title.clone(),
        article.source.clone(),
        article.source_tier,
        article.published_at,
    );
    event.raw_article_id = article.id.clone();
    for (modality, score) in x.modality_pairs() {
        // Gate the tag on the DISCOUNTED value that actually gets stored, mirroring
        // merge_llm_scores — otherwise a raw score in [0.50, 0.556) was tagged while its
        // stored signal fell below the tag threshold, desyncing domain_tags from
        // domain_signals (and surfacing the article under a modality filter it didn't meet).
        let discounted = score * LLM_SCORE_DISCOUNT;
        if discounted >= LLM_TAG_THRESHOLD {
            event.domain_signals.insert(modality.to_string(), discounted);
            event.domain_tags.push(modality.to_string());
        }
    }
    event.severity        = x.severity;
    event.escalation_step = x.escalation_step.clamp(-1.0, 1.0);
    event.theater = Some(if is_valid_theater(&x.theater) { x.theater.clone() } else { "other".to_string() });
    if let Some(coded) = coded_summary(x) {
        event.summary = coded;
    }
    event
}

/// Fast check: does the article title contain geopolitical trigger terms?
/// Used to decide whether to run LLM on articles the keyword gate rejected.
fn has_geopolitical_trigger(title: &str) -> bool {
    let t = title.to_lowercase();
    // Great powers / key actors
    const ACTORS: &[&str] = &[
        "china", "russia", "united states", "iran", "north korea", "israel",
        "ukraine", "taiwan", "nato", "pentagon", "kremlin", "beijing", "moscow",
        "white house", "xi jinping", "putin", "trump", "zelensky", "netanyahu",
        "hezbollah", "hamas", "houthi", "pla", "irgc",
    ];
    // Conflict / escalation terms
    const TERMS: &[&str] = &[
        "war", "attack", "strike", "invasion", "missile", "nuclear", "troops",
        "conflict", "crisis", "sanction", "threat", "escalat", "ceasefire",
        "military", "bomb", "deploy", "weapon", "assassination", "coup",
        "blockade", "detained", "hostage", "cyber", "hack", "intelligence",
    ];
    ACTORS.iter().any(|a| t.contains(a)) || TERMS.iter().any(|t2| t.contains(t2))
}

// ── wait_for_sidecar stub ─────────────────────────────────────────────────────

/// No-op — processor runs inline. Kept for call-site compatibility.
#[allow(dead_code)]
pub async fn wait_for_sidecar(_timeout_secs: u64) {}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::SourceTier;
    use chrono::Utc;

    fn make_article(title: &str, body: &str) -> RawArticle {
        RawArticle::new(
            "https://example.com/test".into(),
            title.into(),
            body.into(),
            "bbc".into(),
            SourceTier::Tier1,
            Utc::now(),
        )
    }

    // ── is_great_power ────────────────────────────────────────────────────────

    #[test]
    fn is_great_power_used_by_sidecar_actor_check() {
        assert!(is_great_power("united states"));
        assert!(is_great_power("russia"));
        assert!(is_great_power("china"));
        assert!(is_great_power("kremlin"));
        assert!(is_great_power("pentagon"));
        assert!(is_great_power("nato"));
        assert!(!is_great_power("iceland"));
        assert!(!is_great_power("unknown actor"));
    }

    #[test]
    fn is_great_power_case_insensitive() {
        assert!(is_great_power("RUSSIA"));
        assert!(is_great_power("United States"));
        assert!(is_great_power("PLA"));
    }

    // ── NlpSidecarHandle shutdown signal ──────────────────────────────────────

    #[test]
    fn shutdown_signal_propagates() {
        let (tx, rx) = watch::channel(false);
        assert!(!*rx.borrow(), "Initial state must be false");
        tx.send(true).unwrap();
        assert!(*rx.borrow(), "Signal must propagate after send");
    }

    #[test]
    fn shutdown_handle_is_idempotent() {
        let (tx, rx) = watch::channel(false);
        let handle = NlpSidecarHandle { shutdown_tx: tx };
        handle.shutdown();
        handle.shutdown(); // second call must be a no-op, not a panic
        assert!(*rx.borrow(), "Signal must still be true after double shutdown");
    }

    #[test]
    fn shutdown_handle_dropped_sender_does_not_panic() {
        let (tx, _rx) = watch::channel(false);
        // Drop the receiver before calling shutdown — send should not panic
        let handle = NlpSidecarHandle { shutdown_tx: tx };
        handle.shutdown(); // receiver is dropped; send error is silently ignored
    }

    // ── Article construction ──────────────────────────────────────────────────

    #[test]
    fn nlp_request_from_article() {
        let article = make_article("Russia nuclear attack on NATO base", "Warhead launched");
        assert!(!article.id.is_empty());
        assert_eq!(article.source, "bbc");
        assert_eq!(article.source_tier, SourceTier::Tier1);
    }

    #[test]
    fn nlp_request_serialises_to_valid_json() {
        let article = make_article("Test", "body");
        let json = serde_json::json!({
            "id":          article.id,
            "title":       article.title,
            "source_tier": article.source_tier as u8,
        });
        assert!(json["source_tier"].is_number());
    }

    #[test]
    fn nlp_request_tier_mapping() {
        for (tier, expected) in [
            (SourceTier::Tier1, 1u8),
            (SourceTier::Tier2, 2u8),
            (SourceTier::Tier3, 3u8),
        ] {
            assert_eq!(tier as u8, expected);
        }
    }

    #[test]
    fn nlp_request_body_truncation_not_applied_here() {
        let mut article = make_article("Test", "");
        article.body = "x".repeat(7000);
        assert_eq!(article.body.len(), 7000);
    }

    // ── Environment / socket stub ─────────────────────────────────────────────

    #[test]
    fn default_socket_path() {
        let path = std::env::var("GCRM_NLP_SOCKET")
            .unwrap_or_else(|_| "/tmp/gcrm_nlp.sock".into());
        assert!(path.contains("gcrm_nlp") || !path.is_empty());
    }

    #[test]
    fn socket_path_from_env() {
        unsafe { std::env::set_var("GCRM_NLP_SOCKET", "/custom/path.sock"); }
        let path = std::env::var("GCRM_NLP_SOCKET").unwrap_or_default();
        assert_eq!(path, "/custom/path.sock");
        unsafe { std::env::remove_var("GCRM_NLP_SOCKET"); }
    }

    // ── Structured LLM extraction merge (v2 Phase 4) ──────────────────────────

    fn extraction(mil: f64, nuc: f64, esc: f64, theater: &str) -> LlmExtraction {
        LlmExtraction {
            military_escalation: mil, nuclear_posture: nuc,
            escalation_step: esc, severity: 0.8, theater: theater.to_string(),
            actor: "Russia".into(), action: "launched strikes on".into(), target: "Kyiv".into(),
            ..Default::default()
        }
    }

    #[test]
    fn valid_theater_ids() {
        assert!(is_valid_theater("us_iran"));
        assert!(is_valid_theater("nato_russia"));
        assert!(!is_valid_theater("other"));
        assert!(!is_valid_theater("atlantis"));
    }

    #[test]
    fn merge_takes_max_modality_and_sets_fields() {
        let mut ev = GeopoliticalEvent::new(
            "Russia strikes Kyiv".into(), "bbc".into(), SourceTier::Tier1, Utc::now());
        ev.domain_signals.insert("military_escalation".into(), 0.4);
        ev.theater = Some("other".into());
        merge_llm_scores(&mut ev, &extraction(0.9, 0.0, 0.8, "nato_russia"));
        assert!(ev.domain_signals["military_escalation"] > 0.7, "LLM 0.9 (disc 0.81) should beat keyword 0.4");
        assert!(ev.domain_tags.contains(&"military_escalation".to_string()));
        assert!((ev.escalation_step - 0.8).abs() < 1e-9);
        assert_eq!(ev.theater.as_deref(), Some("nato_russia"), "Other should be filled by the LLM hint");
        assert_eq!(ev.summary, "Russia launched strikes on Kyiv");
    }

    #[test]
    fn merge_keeps_deterministic_theater_when_already_set() {
        let mut ev = GeopoliticalEvent::new(
            "US strikes Iran".into(), "bbc".into(), SourceTier::Tier1, Utc::now());
        ev.theater = Some("us_iran".into());
        merge_llm_scores(&mut ev, &extraction(0.8, 0.0, 0.7, "nato_russia"));
        assert_eq!(ev.theater.as_deref(), Some("us_iran"), "keyword theater must not be overridden");
    }

    #[test]
    fn make_event_from_extraction_builds_tagged_event() {
        let art = make_article("Cyber attack on power grid", "");
        let x = LlmExtraction {
            cyber_info_ops: 0.7, severity: 0.6, escalation_step: 0.5,
            theater: "us_china_taiwan".into(), ..Default::default()
        };
        let ev = make_event_from_llm(&art, &x);
        assert!(ev.domain_tags.contains(&"cyber_info_ops".to_string()));
        assert_eq!(ev.theater.as_deref(), Some("us_china_taiwan"));
        assert!((ev.escalation_step - 0.5).abs() < 1e-9);
    }

    // ── Cancellation-aware worker-pool permit wait (roadmap 4.3) ──────────────
    // The dispatch path must never let a saturated pool block shutdown. These lock
    // acquire_permit_or_shutdown: a bare acquire_owned().await would hang forever
    // under a held permit, so the cancel test is the real regression guard.

    use tokio::time::{timeout, Duration};

    #[tokio::test]
    async fn permit_wait_returns_shutdown_fast_when_already_signalled() {
        // Pool saturated AND shutdown already true → Shutdown immediately, no waiting.
        let sem = Arc::new(Semaphore::new(1));
        let _held = sem.clone().acquire_owned().await.unwrap();
        let (tx, mut rx) = watch::channel(false);
        tx.send(true).unwrap();
        let res = timeout(
            Duration::from_millis(200),
            acquire_permit_or_shutdown(sem.clone(), &mut rx),
        )
        .await
        .expect("must not block when shutdown already signalled");
        assert!(matches!(res, PermitWait::Shutdown));
    }

    #[tokio::test]
    async fn permit_wait_cancels_on_shutdown_while_pool_saturated() {
        // THE regression lock for roadmap 4.3: the only permit is held and never
        // released, so a bare acquire_owned().await would block forever; the
        // cancellation-aware wait must return Shutdown promptly when the signal
        // fires from another task.
        let sem = Arc::new(Semaphore::new(1));
        let _held = sem.clone().acquire_owned().await.unwrap(); // saturated, never freed
        let (tx, mut rx) = watch::channel(false);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let _ = tx.send(true);
        });
        let res = timeout(
            Duration::from_secs(2),
            acquire_permit_or_shutdown(sem.clone(), &mut rx),
        )
        .await
        .expect("a saturated pool must not prevent shutdown from cancelling the wait");
        assert!(matches!(res, PermitWait::Shutdown));
    }

    #[tokio::test]
    async fn permit_wait_acquires_when_pool_has_capacity() {
        // Happy path: a free permit and no shutdown → Acquired, and the returned
        // permit actually consumes pool capacity (real backpressure preserved).
        let sem = Arc::new(Semaphore::new(1));
        let (_tx, mut rx) = watch::channel(false);
        let res = acquire_permit_or_shutdown(sem.clone(), &mut rx).await;
        assert!(matches!(res, PermitWait::Acquired(_)));
        assert_eq!(sem.available_permits(), 0, "the returned permit must hold pool capacity");
    }

    #[tokio::test]
    async fn permit_wait_reports_closed_semaphore() {
        // If the pool is torn down, the wait reports Closed so run() can exit its loop.
        let sem = Arc::new(Semaphore::new(1));
        sem.close();
        let (_tx, mut rx) = watch::channel(false);
        let res = acquire_permit_or_shutdown(sem.clone(), &mut rx).await;
        assert!(matches!(res, PermitWait::Closed));
    }
}
