//! Live-stream transcription — 24/7 broadcast news streams become a rolling text
//! source: each cycle, a bounded audio window is captured from the channel's live
//! stream (yt-dlp resolves the rotating live URL; ffmpeg captures ~2 minutes of
//! 16 kHz mono), transcribed by a LOCAL CPU Whisper (faster-whisper int8 `base`
//! model — ~15-20 s of CPU per 2-minute window, nice'd, never touching the GPU the
//! LLM enricher owns), relevance-gated, and ingested through the NORMAL article
//! pipeline. Anchors say things MINUTES before a clip is cut and HOURS before wire
//! copy — this is the earliest text form of broadcast signal available.
//!
//! Feed shape: each stream keeps ONE article (the plain watch URL), updated in
//! place per window by the store's live-blog update path — a rolling "what the
//! channel is saying right now" row, not a spam of near-duplicates. Every updated
//! window still flows through NLP/enricher as fresh evidence.
//!
//! DORMANT BY DEFAULT (keyed-feed pattern): runs only when
//! `GCRM_LIVESTREAM_SOURCES=1` (secrets.env) AND a whisper binary is reachable
//! (`GCRM_WHISPER_BIN`, then the gcrm-video venv, then PATH). Proven end-to-end
//! 2026-07-05: 60 s of live Al Jazeera transcribed accurately on CPU. Local-only;
//! cargo tests are offline.

use std::path::PathBuf;
use std::time::Duration;

use crate::models::SourceTier;

/// One watched live stream. `page` is the channel's stable /live page (the live
/// video id rotates — it is re-resolved every cycle).
pub struct LiveStream {
    pub page:   &'static str,
    pub source: &'static str,
    pub tier:   SourceTier,
}

/// Streams verified live-resolvable 2026-07-05. Both channels' text/video feeds
/// already hold roster slots — the trust call is inherited.
pub const LIVE_STREAMS: &[LiveStream] = &[
    LiveStream { page: "https://www.youtube.com/@aljazeeraenglish/live", source: "aljazeera-live", tier: SourceTier::Tier1 },
    LiveStream { page: "https://www.youtube.com/@dwnews/live",           source: "dwnews-live",    tier: SourceTier::Tier1 },
];

/// Cycle cadence. Each cycle serially captures+transcribes every stream
/// (~2.5 min/stream), so 10 minutes gives comfortable headroom for 2 streams.
pub const LIVESTREAM_POLL_SECS: u64 = 600;
/// Audio window captured per stream per cycle.
pub const CAPTURE_SECS: u32 = 120;
/// Whisper CPU threads — deliberately small; the box also runs the monitor.
pub const WHISPER_THREADS: u32 = 4;
/// Budget for the whole capture+transcribe of one stream.
pub const STREAM_BUDGET_SECS: u64 = 240;

/// Permanently disabled (operator directive 2026-07-23): the 24/7 whisper-livestream
/// tier produced misleading "[LIVE] {label}:" rows whose transcription was mostly
/// nonsense ("Pufferfish don't generally attack humans", "violated the MOU"). Robert
/// judged the live-stream axis not worth its noise — recent uploaded videos stay, live
/// streams go. Hard-off in code (not just the env flag) so it can't be re-enabled by a
/// stray `GCRM_LIVESTREAM_SOURCES=1`; flip this back only on his explicit say-so.
pub fn enabled() -> bool {
    false
}

/// The whisper CLI to run: `GCRM_WHISPER_BIN` → the gcrm-video venv → PATH.
pub fn whisper_bin() -> PathBuf {
    if let Ok(p) = std::env::var("GCRM_WHISPER_BIN") {
        return PathBuf::from(p);
    }
    if let Some(home) = std::env::var_os("HOME") {
        let venv = PathBuf::from(home).join(".local/share/gcrm-video/venv/bin/whisper-ctranslate2");
        if venv.exists() {
            return venv;
        }
    }
    PathBuf::from("whisper-ctranslate2")
}

/// Synthesize the rolling article's title for one transcribed window: the first
/// trigger-bearing sentence (the operator-relevant lead), else the first sentence.
/// Prefixed so the row is instantly recognizable as a live transcript. Pure.
pub fn live_title(source_label: &str, transcript: &str) -> String {
    let first_sentence = |s: &str| -> String {
        let end = s.find(['.', '?', '!']).map(|i| i + 1).unwrap_or_else(|| s.len().min(120));
        s[..end].trim().chars().take(120).collect()
    };
    let lead = transcript
        .split_inclusive(['.', '?', '!'])
        .map(str::trim)
        .find(|s| !s.is_empty() && crate::nlp_sidecar::has_geopolitical_trigger(s))
        .map(|s| s.chars().take(120).collect::<String>())
        .unwrap_or_else(|| first_sentence(transcript));
    format!("[LIVE] {source_label}: {lead}")
}

/// Capture `CAPTURE_SECS` of a live stream's audio and transcribe it on CPU.
/// `Ok(None)` = the page has no live stream up right now (channels do pause) or
/// the window transcribed to nothing; `Err` = a real failure (binary/network).
pub async fn capture_transcript(page: &str) -> anyhow::Result<Option<String>> {
    let dir = std::env::temp_dir().join(format!(
        "gcrm-live-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    tokio::fs::create_dir_all(&dir).await?;

    // Everything below funnels through one result so the temp dir is always removed.
    let work = async {
        // 1) Resolve the rotating live audio URL (bounded).
        let ytdlp = tokio::process::Command::new(crate::video::ytdlp_bin())
            .args(["-q", "--no-warnings", "-g", "-f", "bestaudio/best"])
            .arg(page)
            .kill_on_drop(true)
            .output();
        let out = tokio::time::timeout(Duration::from_secs(45), ytdlp).await;
        let url = match out {
            Ok(Ok(o)) if o.status.success() => {
                String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or("").trim().to_string()
            }
            Ok(Ok(_)) => return Ok(None), // no live stream up — a normal state
            Ok(Err(e)) => anyhow::bail!("yt-dlp spawn failed: {e}"),
            Err(_) => anyhow::bail!("yt-dlp live-resolve timed out"),
        };
        if url.is_empty() {
            return Ok(None);
        }

        // 2) Capture a bounded mono window (ffmpeg reads the live HLS in real time).
        let wav = dir.join("win.wav");
        let ff = tokio::process::Command::new("ffmpeg")
            .args(["-loglevel", "error", "-i", &url, "-t", &CAPTURE_SECS.to_string(),
                   "-ac", "1", "-ar", "16000"])
            .arg(&wav)
            .arg("-y")
            .kill_on_drop(true)
            .output();
        match tokio::time::timeout(Duration::from_secs(STREAM_BUDGET_SECS), ff).await {
            Ok(Ok(o)) if o.status.success() => {}
            Ok(Ok(o)) => anyhow::bail!("ffmpeg exit {}: {}", o.status, String::from_utf8_lossy(&o.stderr)),
            Ok(Err(e)) => anyhow::bail!("ffmpeg spawn failed: {e}"),
            Err(_) => anyhow::bail!("ffmpeg capture timed out"),
        }

        // 3) Transcribe on CPU, nice'd below the monitor's own work.
        let wh = tokio::process::Command::new("nice")
            .args(["-n", "15"])
            .arg(whisper_bin())
            .arg(&wav)
            .args(["--model", "base", "--device", "cpu", "--compute_type", "int8"])
            .args(["--threads", &WHISPER_THREADS.to_string()])
            .args(["--output_dir"]).arg(&dir)
            .args(["--output_format", "txt", "--language", "en"])
            .kill_on_drop(true)
            .output();
        match tokio::time::timeout(Duration::from_secs(STREAM_BUDGET_SECS), wh).await {
            Ok(Ok(o)) if o.status.success() => {}
            Ok(Ok(o)) => anyhow::bail!("whisper exit {}: {}", o.status, String::from_utf8_lossy(&o.stderr)),
            Ok(Err(e)) => anyhow::bail!("whisper spawn failed: {e}"),
            Err(_) => anyhow::bail!("whisper timed out"),
        }

        let txt = dir.join("win.txt");
        let text = tokio::fs::read_to_string(&txt).await.unwrap_or_default();
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        anyhow::Ok((!text.trim().is_empty()).then_some(text))
    }
    .await;

    let _ = tokio::fs::remove_dir_all(&dir).await; // every path cleans up
    work
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permanently_disabled_regardless_of_env() {
        // Operator directive 2026-07-23: the whisper-livestream tier is hard-off in code,
        // so even a stray GCRM_LIVESTREAM_SOURCES=1 cannot resurrect the misleading [LIVE]
        // rows. Set the flag in-process and confirm enabled() stays false.
        std::env::set_var("GCRM_LIVESTREAM_SOURCES", "1");
        assert!(!enabled(), "the live-stream tier must stay off even with the env flag set");
        std::env::remove_var("GCRM_LIVESTREAM_SOURCES");
        assert!(!enabled(), "the live-stream tier must be off by default");
    }

    #[test]
    fn stream_sources_are_live_suffixed_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for s in LIVE_STREAMS {
            assert!(s.source.ends_with("-live"), "{} must carry the -live suffix", s.source);
            assert!(seen.insert(s.source), "duplicate source id {}", s.source);
            assert!(s.page.ends_with("/live"), "{} must be a stable /live page", s.page);
        }
    }

    #[test]
    fn live_title_leads_with_the_trigger_sentence() {
        let t = "Good morning and welcome back. Markets opened quietly today. \
                 Iran has warned tankers approaching the strait. More after the break.";
        let title = live_title("Al Jazeera", t);
        assert!(title.starts_with("[LIVE] Al Jazeera: Iran has warned tankers"),
            "trigger sentence must lead: {title:?}");
        // No trigger anywhere → first sentence, honestly.
        let none = live_title("DW", "The weather is mild across the region today. Sports next.");
        assert!(none.starts_with("[LIVE] DW: The weather is mild"), "{none:?}");
    }

    /// Live end-to-end proof (network + ffmpeg + whisper) — run manually:
    /// cargo test -- --ignored live_stream
    #[tokio::test]
    #[ignore]
    async fn live_stream_transcript_end_to_end() {
        let got = capture_transcript(LIVE_STREAMS[0].page).await.expect("capture ran");
        match got {
            Some(t) => {
                assert!(t.split_whitespace().count() > 40, "suspiciously short: {t:?}");
                println!("live window OK: {} words: {}…", t.split_whitespace().count(),
                         t.chars().take(140).collect::<String>());
            }
            None => println!("machinery OK; channel not streaming right now"),
        }
    }
}
