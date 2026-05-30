// ------------------------------------------------------------
// RAiTHE INDUSTRIES INCORPORATED
// Copyright (c) 2026 All Rights Reserved.
// ------------------------------------------------------------

// src/llm_enricher.rs — Local LLM article classifier (Ollama)
//
// Calls any OpenAI-chat-compatible endpoint (Ollama by default) to score a
// news article against the 8 geopolitical domains.  Runs in the NlpSidecar
// pipeline and merges its output with the keyword-based processor scores.
//
// Design:
//   • Falls back silently (returns None) on timeout, connection error, or
//     malformed JSON — keyword scores are used alone in that case.
//   • All LLM scores are clamped to [0.0, 1.0] and discounted 10% before
//     merging, so keyword definitive hits (1.0) always beat LLM estimates.
//   • Temperature 0.05 — near-deterministic for classification.
//   • format: "json" forces Ollama to guarantee valid JSON output.

use std::time::Duration;
use tracing::{debug, info, warn};

use crate::models::LlmSettings;

// ── Domain score payload returned by the model ────────────────────────────────

#[derive(Debug, Default, serde::Deserialize)]
pub struct LlmScores {
    #[serde(default)] pub military_escalation:  f64,
    #[serde(default)] pub nuclear_posture:       f64,
    #[serde(default)] pub diplomatic_breakdown:  f64,
    #[serde(default)] pub economic_warfare:      f64,
    #[serde(default)] pub cyber_info_ops:        f64,
    #[serde(default)] pub alliance_activation:   f64,
    #[serde(default)] pub great_power_conflict:  f64,
    #[serde(default)] pub wmd_mass_casualty:     f64,
    #[serde(default)] pub severity:              f64,
}

impl LlmScores {
    pub fn max_domain_score(&self) -> f64 {
        self.as_domain_pairs()
            .iter()
            .map(|(_, s)| *s)
            .fold(0.0_f64, f64::max)
    }

    pub fn as_domain_pairs(&self) -> [(&'static str, f64); 8] {
        [
            ("military_escalation",  self.military_escalation),
            ("nuclear_posture",      self.nuclear_posture),
            ("diplomatic_breakdown", self.diplomatic_breakdown),
            ("economic_warfare",     self.economic_warfare),
            ("cyber_info_ops",       self.cyber_info_ops),
            ("alliance_activation",  self.alliance_activation),
            ("great_power_conflict", self.great_power_conflict),
            ("wmd_mass_casualty",    self.wmd_mass_casualty),
        ]
    }
}

// ── LlmEnricher ───────────────────────────────────────────────────────────────

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
            info!(
                "LLM enricher: ENABLED — endpoint={} model={}",
                settings.endpoint, settings.model
            );
        } else {
            info!("LLM enricher: disabled (set llm.enabled: true in settings.yml to activate)");
        }

        Self { client, settings }
    }

    pub fn is_enabled(&self) -> bool {
        self.settings.enabled
    }

    /// Classify a news article against the 8 geopolitical domains.
    /// Returns None if disabled, unreachable, or timed out.
    pub async fn classify(&self, title: &str, body: &str) -> Option<LlmScores> {
        if !self.settings.enabled {
            return None;
        }

        let excerpt = if body.len() > 500 { &body[..500] } else { body };

        let prompt = format!(
            "Classify this news article for geopolitical conflict risk. \
             Score each domain 0.0–1.0 (0=absent, 1=maximum escalation). \
             Return ONLY a JSON object with exactly these keys.\n\n\
             Article: \"{title}. {excerpt}\"\n\n\
             {{\"military_escalation\":0.0,\"nuclear_posture\":0.0,\
             \"diplomatic_breakdown\":0.0,\"economic_warfare\":0.0,\
             \"cyber_info_ops\":0.0,\"alliance_activation\":0.0,\
             \"great_power_conflict\":0.0,\"wmd_mass_casualty\":0.0,\
             \"severity\":0.0}}"
        );

        let payload = serde_json::json!({
            "model": self.settings.model,
            "messages": [
                {
                    "role": "system",
                    "content": "You are a precise geopolitical risk classifier. \
                                Output only the requested JSON object. \
                                Score domains only if clearly indicated by the article. \
                                Use 0.0 for absent domains."
                },
                { "role": "user", "content": prompt }
            ],
            "stream": false,
            "format": "json",
            "options": {
                "temperature":  0.05,
                "top_p":        0.9,
                "num_predict":  160
            }
        });

        let url = format!("{}/api/chat", self.settings.endpoint.trim_end_matches('/'));

        let resp = match self.client.post(&url).json(&payload).send().await {
            Ok(r)  => r,
            Err(e) => {
                if e.is_timeout() {
                    debug!("LLM: timeout ({}s)", self.settings.timeout_secs);
                } else {
                    warn!("LLM: connection error — {e}");
                }
                return None;
            }
        };

        let body: serde_json::Value = match resp.json().await {
            Ok(v)  => v,
            Err(e) => { warn!("LLM: response parse error — {e}"); return None; }
        };

        let content = body
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .unwrap_or("");

        match serde_json::from_str::<LlmScores>(content) {
            Ok(mut s) => {
                // Clamp all fields to [0.0, 1.0]
                for v in [
                    &mut s.military_escalation, &mut s.nuclear_posture,
                    &mut s.diplomatic_breakdown, &mut s.economic_warfare,
                    &mut s.cyber_info_ops, &mut s.alliance_activation,
                    &mut s.great_power_conflict, &mut s.wmd_mass_casualty,
                    &mut s.severity,
                ] { *v = v.clamp(0.0, 1.0); }
                Some(s)
            }
            Err(e) => {
                debug!("LLM: JSON parse failed — {e} | content={content:.80}");
                None
            }
        }
    }
}
