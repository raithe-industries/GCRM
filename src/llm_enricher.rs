// ------------------------------------------------------------
// RAiTHE INDUSTRIES INCORPORATED
// Copyright (c) 2026 All Rights Reserved.
// ------------------------------------------------------------

// src/llm_enricher.rs — Local LLM structured event extractor (Ollama)  [GCRM v2, Phase 4]
//
// v2 upgrade: the LLM no longer just re-scores eight domains. It performs STRUCTURED
// EVENT EXTRACTION — actor → action → target, theater, a signed Goldstein-style
// escalation step, severity, and the FIVE orthogonal modality scores — returning one
// rich JSON record per article. This is the difference between keyword soup and real
// signal, and it feeds both the risk engine and the analyst brief.
//
// Design (unchanged discipline):
//   • Falls back silently (returns None) on timeout, connection error, or malformed
//     JSON — keyword scores are used alone in that case.
//   • Scores clamped; modality scores are discounted before merging so a keyword
//     definitive hit (1.0) still outweighs an LLM estimate.
//   • Temperature 0.05 (near-deterministic); format: "json" forces valid JSON.

use std::time::Duration;
use tracing::{debug, info, warn};

use crate::models::LlmSettings;

// ── Structured extraction returned by the model ───────────────────────────────────

#[derive(Debug, Default, serde::Deserialize)]
pub struct LlmExtraction {
    // Five orthogonal modality scores (0..1).
    #[serde(default)] pub military_escalation:  f64,
    #[serde(default)] pub nuclear_posture:      f64,
    #[serde(default)] pub economic_warfare:     f64,
    #[serde(default)] pub cyber_info_ops:       f64,
    #[serde(default)] pub diplomatic_breakdown: f64,

    /// Signed Goldstein-style escalation step in [-1, 1]: positive = escalatory
    /// (an attack, a threat, a withdrawal from talks), negative = de-escalatory
    /// (a ceasefire, a successful negotiation).
    #[serde(default)] pub escalation_step: f64,
    /// Overall event severity (0..1).
    #[serde(default)] pub severity: f64,

    // Structured coding — used by the analyst brief and for insight, not scoring.
    #[serde(default)] pub actor:   String,
    #[serde(default)] pub action:  String,
    #[serde(default)] pub target:  String,
    /// Theater hint: nato_russia | us_iran | us_china_taiwan | india_pakistan | korea | other
    #[serde(default)] pub theater: String,
}

impl LlmExtraction {
    /// The five modality (id, score) pairs.
    pub fn modality_pairs(&self) -> [(&'static str, f64); 5] {
        [
            ("military_escalation",  self.military_escalation),
            ("nuclear_posture",      self.nuclear_posture),
            ("economic_warfare",     self.economic_warfare),
            ("cyber_info_ops",       self.cyber_info_ops),
            ("diplomatic_breakdown", self.diplomatic_breakdown),
        ]
    }

    pub fn max_modality(&self) -> f64 {
        self.modality_pairs().iter().map(|(_, s)| *s).fold(0.0_f64, f64::max)
    }
}

// ── LlmEnricher ───────────────────────────────────────────────────────────────────

pub struct LlmEnricher {
    client:   reqwest::Client,
    settings: LlmSettings,
}

impl LlmEnricher {
    pub fn new(settings: LlmSettings) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(settings.timeout_secs))
            .build()
            .expect("LlmEnricher: failed to build HTTP client");

        if settings.enabled {
            info!("LLM extractor: ENABLED — endpoint={} model={}", settings.endpoint, settings.model);
        } else {
            info!("LLM extractor: disabled (set llm.enabled: true in settings.yml to activate)");
        }
        Self { client, settings }
    }

    pub fn is_enabled(&self) -> bool {
        self.settings.enabled
    }

    /// Whether embedding-based semantic dedup is enabled (adds an embeddings call).
    pub fn is_semantic_dedup(&self) -> bool {
        self.settings.enabled && self.settings.semantic_dedup
    }

    /// Structured extraction of a news article. Returns None if disabled, unreachable,
    /// or timed out (caller falls back to keyword scores).
    pub async fn classify(&self, title: &str, body: &str) -> Option<LlmExtraction> {
        if !self.settings.enabled {
            return None;
        }

        // Walk back to a UTF-8 char boundary — international bodies are multi-byte.
        let excerpt = if body.len() > 600 {
            let end = (0..=600usize).rev().find(|&i| body.is_char_boundary(i)).unwrap_or(0);
            &body[..end]
        } else {
            body
        };

        let prompt = format!(
            "Extract the geopolitical conflict signal from this article as JSON.\n\
             Article: \"{title}. {excerpt}\"\n\n\
             Return ONLY this JSON object:\n\
             {{\
             \"actor\":\"primary actor\",\"action\":\"what they did\",\"target\":\"against whom\",\
             \"theater\":\"one of: nato_russia|us_iran|us_china_taiwan|india_pakistan|korea|other\",\
             \"military_escalation\":0.0,\"nuclear_posture\":0.0,\"economic_warfare\":0.0,\
             \"cyber_info_ops\":0.0,\"diplomatic_breakdown\":0.0,\
             \"escalation_step\":0.0,\"severity\":0.0}}\n\n\
             Modality scores are 0.0-1.0 (0=absent, 1=maximum). military_escalation=armed force in \
             use; nuclear_posture=nuclear signaling/doctrine/use; economic_warfare=blockade, energy \
             weaponization, sanctions-as-war; cyber_info_ops=cyber/info attacks; diplomatic_breakdown=\
             collapse of off-ramps/talks. escalation_step is -1.0 (de-escalatory: ceasefire, deal) to \
             +1.0 (escalatory: strike, threat). Score only what the article clearly indicates."
        );

        let payload = serde_json::json!({
            "model": self.settings.model,
            "messages": [
                { "role": "system",
                  "content": "You are a precise geopolitical event extractor. Output only the requested \
                              JSON object. Use 0.0 for absent modalities and \"\" for unknown fields." },
                { "role": "user", "content": prompt }
            ],
            "stream": false,
            "format": "json",
            "options": { "temperature": 0.05, "top_p": 0.9, "num_predict": 220 }
        });

        let url = format!("{}/api/chat", self.settings.endpoint.trim_end_matches('/'));

        let resp = match self.client.post(&url).json(&payload).send().await {
            Ok(r)  => r,
            Err(e) => {
                if e.is_timeout() { debug!("LLM: timeout ({}s)", self.settings.timeout_secs); }
                else { warn!("LLM: connection error — {e}"); }
                return None;
            }
        };

        let body: serde_json::Value = match resp.json().await {
            Ok(v)  => v,
            Err(e) => { warn!("LLM: response parse error — {e}"); return None; }
        };

        let content = body
            .get("message").and_then(|m| m.get("content")).and_then(|c| c.as_str())
            .unwrap_or("");

        match serde_json::from_str::<LlmExtraction>(content) {
            Ok(mut x) => {
                for v in [
                    &mut x.military_escalation, &mut x.nuclear_posture, &mut x.economic_warfare,
                    &mut x.cyber_info_ops, &mut x.diplomatic_breakdown, &mut x.severity,
                ] { *v = v.clamp(0.0, 1.0); }
                x.escalation_step = x.escalation_step.clamp(-1.0, 1.0);
                Some(x)
            }
            Err(e) => {
                debug!("LLM: JSON parse failed — {e} | content={content:.80}");
                None
            }
        }
    }

    /// Free-text analyst brief generation (v2 Phase 4). Given a compact context of the
    /// current systemic state, returns a short measured prose brief, or None on
    /// disabled/unreachable/empty (the caller falls back to a templated brief).
    pub async fn brief(&self, context: &str) -> Option<String> {
        if !self.settings.enabled {
            return None;
        }
        let payload = serde_json::json!({
            "model": self.settings.model,
            "messages": [
                { "role": "system",
                  "content": "You are a precise, measured geopolitical risk analyst writing for an \
                              intelligence dashboard. Write 2-3 short paragraphs of plain factual prose \
                              (no markdown, no headers, no preamble). Explain the current systemic risk \
                              reading and the main drivers by theater. Do not invent specifics beyond \
                              the data provided." },
                { "role": "user", "content": context }
            ],
            "stream": false,
            "options": { "temperature": 0.3, "top_p": 0.9, "num_predict": 360 }
        });
        let url = format!("{}/api/chat", self.settings.endpoint.trim_end_matches('/'));
        let resp = match self.client.post(&url).json(&payload).send().await {
            Ok(r)  => r,
            Err(e) => { debug!("LLM brief: request error — {e}"); return None; }
        };
        let body: serde_json::Value = resp.json().await.ok()?;
        let content = body.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_str())?;
        let t = content.trim();
        if t.is_empty() { None } else { Some(t.to_string()) }
    }

    /// Embedding vector for semantic dedup (v2 Phase 4). Uses Ollama's embeddings
    /// endpoint with `embed_model`. None on disabled/unreachable — callers fall back
    /// to MinHash/trigram dedup, so this never blocks the pipeline.
    pub async fn embed(&self, text: &str) -> Option<Vec<f32>> {
        if !self.settings.enabled {
            return None;
        }
        let payload = serde_json::json!({
            "model": self.settings.embed_model,
            "prompt": text,
        });
        let url = format!("{}/api/embeddings", self.settings.endpoint.trim_end_matches('/'));
        let resp = self.client.post(&url).json(&payload).send().await.ok()?;
        let body: serde_json::Value = resp.json().await.ok()?;
        let arr = body.get("embedding")?.as_array()?;
        let v: Vec<f32> = arr.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect();
        if v.is_empty() { None } else { Some(v) }
    }
}

/// Cosine similarity of two equal-length embedding vectors. Returns 0.0 for
/// mismatched/empty inputs.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || a.len() != b.len() { return 0.0; }
    let mut dot = 0.0f32; let mut na = 0.0f32; let mut nb = 0.0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i]; na += a[i] * a[i]; nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 { return 0.0; }
    dot / (na.sqrt() * nb.sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_identical_is_one() {
        let v = vec![0.1, 0.2, 0.3, 0.4];
        assert!((cosine_similarity(&v, &v) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn cosine_orthogonal_is_zero() {
        assert!(cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
    }

    #[test]
    fn cosine_mismatched_or_empty_is_zero() {
        assert_eq!(cosine_similarity(&[1.0, 2.0], &[1.0]), 0.0);
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
    }

    #[test]
    fn extraction_clamps_and_max_modality() {
        let x = LlmExtraction {
            military_escalation: 0.9, nuclear_posture: 0.3, escalation_step: 0.7, ..Default::default()
        };
        assert!((x.max_modality() - 0.9).abs() < 1e-9);
        assert_eq!(x.modality_pairs().len(), 5);
    }
}
