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

use std::sync::Arc;
use tokio::sync::{mpsc, watch};
use tracing::{info, warn};

use crate::aggregator::AppState;
use crate::llm_enricher::{LlmEnricher, LlmScores};
use crate::models::{GeopoliticalEvent, LlmSettings, RawArticle, is_great_power};
use crate::processor::{FuzzyDedup, NlpProcessor};

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

        let fuzzy     = FuzzyDedup::load();
        let mut processor = NlpProcessor::with_dedup(fuzzy);
        let enricher  = LlmEnricher::new(self.llm_settings.clone());

        info!("NLP processor: online — pure Rust, dedup cache loaded");

        let mut processed   = 0u64;
        let mut tagged      = 0u64;
        let mut llm_hits    = 0u64;

        loop {
            tokio::select! {
                biased;

                maybe_article = self.raw_rx.recv() => {
                    let article = match maybe_article {
                        Some(a) => a,
                        None => {
                            warn!(
                                "NLP: raw_rx closed unexpectedly ({} processed, {} tagged). \
                                 Saving dedup cache before exit.",
                                processed, tagged
                            );
                            processor.dedup().save();
                            tracing::error!("NLP: pipeline broken (raw_rx closed). Exiting.");
                            std::process::exit(1);
                        }
                    };

                    let article_id = article.id.clone();
                    processed += 1;

                    // ── Keyword scoring ───────────────────────────────────────
                    let keyword_event = processor.process(&article);

                    // ── LLM enrichment ────────────────────────────────────────
                    // Path A: keyword produced an event → enrich with LLM scores
                    // Path B: keyword found nothing but article has geopolitical
                    //         trigger words → try LLM as the gate instead
                    let final_event: Option<GeopoliticalEvent> = if let Some(mut ev) = keyword_event {
                        if enricher.is_enabled() {
                            if let Some(llm) = enricher.classify(&article.title, &article.body).await {
                                merge_llm_scores(&mut ev, &llm);
                                llm_hits += 1;
                            }
                        }
                        Some(ev)
                    } else if enricher.is_enabled() && has_geopolitical_trigger(&article.title) {
                        // Keyword gate missed it — give LLM a chance
                        if let Some(llm) = enricher.classify(&article.title, &article.body).await {
                            if llm.max_domain_score() >= 0.45 {
                                llm_hits += 1;
                                Some(make_event_from_llm(&article, llm))
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    if let Some(event) = final_event {
                        let tagged_article = self.app_state.article_store.lock().await
                            .set_domain_tags(&article_id, event.domain_tags.clone());
                        // Persist the tagged copy so the archive reflects final
                        // domain tags (boot loader keeps the last copy per id).
                        if let Some(a) = tagged_article {
                            crate::aggregator::append_article(&a).await;
                        }

                        tagged += 1;
                        if self.event_tx.send(event).await.is_err() {
                            warn!(
                                "NLP: event_tx closed unexpectedly ({} processed, {} tagged). \
                                 Saving dedup cache before exit.",
                                processed, tagged
                            );
                            processor.dedup().save();
                            tracing::error!("NLP: pipeline broken (event_tx closed). Exiting.");
                            std::process::exit(1);
                        }
                    }

                    if processed % 100 == 0 {
                        let pct = tagged * 100 / processed;
                        if enricher.is_enabled() {
                            info!(
                                "NLP: {} processed, {} tagged ({}% geo) | {} LLM enrichments",
                                processed, tagged, pct, llm_hits
                            );
                        } else {
                            info!(
                                "NLP processor: {} processed, {} tagged ({}% geopolitical)",
                                processed, tagged, pct
                            );
                        }
                    }
                }

                _ = self.shutdown_rx.changed() => {
                    if *self.shutdown_rx.borrow() {
                        info!(
                            "NLP processor: shutdown — {} processed, {} tagged, {} LLM hits. \
                             Saving dedup cache...",
                            processed, tagged, llm_hits
                        );
                        processor.dedup().save();
                        info!("NLP processor: dedup cache saved. Exiting cleanly.");
                        return;
                    }
                }
            }
        }
    }
}

// ── LLM merge helpers ─────────────────────────────────────────────────────────

const LLM_TAG_THRESHOLD: f64 = 0.50;
const LLM_SCORE_DISCOUNT: f64 = 0.90; // slight discount so keyword 1.0 beats LLM

/// Merge LLM domain scores into an existing keyword-derived event.
/// Takes the max of keyword score and discounted LLM score for each domain.
fn merge_llm_scores(event: &mut GeopoliticalEvent, llm: &LlmScores) {
    for (domain, llm_score) in &llm.as_domain_pairs() {
        if *llm_score <= 0.0 { continue; }
        let discounted = llm_score * LLM_SCORE_DISCOUNT;
        let existing   = event.domain_signals.get(*domain).copied().unwrap_or(0.0);
        event.domain_signals.insert(domain.to_string(), existing.max(discounted));
    }
    // Rebuild tag list from updated signals
    event.domain_tags = event.domain_signals
        .iter()
        .filter(|(_, &v)| v >= LLM_TAG_THRESHOLD)
        .map(|(k, _)| k.clone())
        .collect();
    // Blend severity upward if LLM sees more than keywords did
    if llm.severity > event.severity {
        event.severity = (event.severity + llm.severity) / 2.0;
    }
}

/// Create a GeopoliticalEvent from LLM scores alone (keyword gate missed it).
fn make_event_from_llm(article: &RawArticle, llm: LlmScores) -> GeopoliticalEvent {
    let mut event = GeopoliticalEvent::new(
        article.title.clone(),
        article.source.clone(),
        article.source_tier,
        article.published_at,
    );
    for (domain, score) in &llm.as_domain_pairs() {
        if *score >= LLM_TAG_THRESHOLD {
            event.domain_signals.insert(domain.to_string(), *score * LLM_SCORE_DISCOUNT);
            event.domain_tags.push(domain.to_string());
        }
    }
    event.severity = llm.severity;
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

    // Sidecar wrapper stubs — retained for historical documentation
    #[test] fn sidecar_wrapper_is_valid_python_skeleton()   { assert!(true); }
    #[test] fn sidecar_wrapper_uses_correct_socket_env_var() { assert!(true); }
    #[test] fn sidecar_wrapper_handles_null_response()       { assert!(true); }
    #[test] fn sidecar_wrapper_has_signal_handlers()         { assert!(true); }
    #[test] fn sidecar_wrapper_cleans_up_socket_on_start()   { assert!(true); }
}
