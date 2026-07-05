//! Video-news transcript ingestion — a curated YouTube channel watchlist becomes a
//! news source: new uploads are discovered through the channels' auth-free Atom feeds
//! (`youtube.com/feeds/videos.xml?channel_id=…`), their auto-captions pulled with a
//! local `yt-dlp` (subtitles only — the video itself is never downloaded), flattened
//! to plain text, and fed into the NORMAL article pipeline as `RawArticle`s — so video
//! signal gets the same dedup, NLP scoring, LLM-enricher semantic read, storage and
//! dashboard rendering as wire copy. Operators see it as a plain article row: title
//! linking to the YouTube URL, the channel as source, the upload time as timestamp.
//!
//! Why: broadcast/analyst video routinely carries signal HOURS before (or instead of)
//! wire text — the 2026-07-04 case was a BNN Bloomberg analyst disputing "Hormuz has
//! reopened" headlines with satellite-checked "traffic has not normalized" claims, a
//! chokepoint-status contradiction present in no wire story in the window. Keyword
//! lexicons cannot read that register (the same transcript scores zero domain-keyword
//! hits); the LLM enricher can — which is exactly the pipeline this module feeds.
//!
//! DORMANT BY DEFAULT (the keyed-feed pattern): the loop runs only when
//! `GCRM_VIDEO_SOURCES=1` is set (env or secrets.env) AND a `yt-dlp` binary is
//! reachable (`GCRM_YTDLP_BIN`, then `~/.local/bin/yt-dlp`, then `yt-dlp` on PATH).
//! The cloud routines' sandbox cannot reach YouTube, so this is a LOCAL-only source;
//! cargo tests are offline (fixture-based) like every other connector.

use std::path::PathBuf;
use std::time::Duration;

use crate::models::SourceTier;

/// One watched channel. `source` is the article-store source id (also what the
/// dashboard shows); keep the `-video` suffix so a channel is never confused with
/// the outlet's text feed and per-source stats stay separable.
pub struct VideoChannel {
    pub channel_id: &'static str,
    pub source:     &'static str,
    pub tier:       SourceTier,
}

/// Starter watchlist — channels whose TEXT outlets already sit in the Tier-1/2 roster,
/// so the credibility call is inherited, not invented. Extend deliberately: every
/// channel added is an editorial-trust decision, not a scrape target.
pub const VIDEO_CHANNELS: &[VideoChannel] = &[
    VideoChannel { channel_id: "UC5aNPmKYwbudeNngDMTY3lw", source: "bnnbloomberg-video", tier: SourceTier::Tier2 },
    VideoChannel { channel_id: "UCoMdktPbSTixAyNGwb-UYkQ", source: "skynews-video",      tier: SourceTier::Tier1 },
    VideoChannel { channel_id: "UCknLrEdhRCp1aegoMqRaCZg", source: "dwnews-video",       tier: SourceTier::Tier1 },
    VideoChannel { channel_id: "UCNye-wNBqNL5ZzHSJj3l8Bg", source: "aljazeera-video",    tier: SourceTier::Tier1 },
    VideoChannel { channel_id: "UC16niRr50-MSBwiO3YDb3RA", source: "bbc-video",          tier: SourceTier::Tier1 },
    VideoChannel { channel_id: "UCIALMKvObZNtJ6AmdCLP7Lg", source: "bloombergtv-video",  tier: SourceTier::Tier2 },
    VideoChannel { channel_id: "UCSPEjw8F2nQDtmUKPFNF7_A", source: "nhkworld-video",     tier: SourceTier::Tier1 },
    VideoChannel { channel_id: "UChqUTb7kYRX8-EiaN3XFrSQ", source: "reuters-video",      tier: SourceTier::Tier1 },
    VideoChannel { channel_id: "UC52X5wxOL_s5yw0dQk7NtgA", source: "ap-video",           tier: SourceTier::Tier1 },
    VideoChannel { channel_id: "UCQfwfsi5VrQ8yKZ-UWmAEFg", source: "france24-video",     tier: SourceTier::Tier1 },
    VideoChannel { channel_id: "UC83jt4dlz1Gjl58fzQrrKZg", source: "cna-video",          tier: SourceTier::Tier1 },
    VideoChannel { channel_id: "UC7fWeaHhqgM4Ry-RMpM2YYw", source: "trtworld-video",     tier: SourceTier::Tier2 },
    VideoChannel { channel_id: "UC_gUM8rL-Lrg6O3adPW9K1g", source: "wion-video",         tier: SourceTier::Tier2 },
];

/// Poll cadence. Broadcast channels upload a handful of clips per hour at most; 15
/// minutes keeps "real-time as they hit YouTube" honest without hammering the feeds.
pub const VIDEO_POLL_SECS: u64 = 900;
/// Only ingest uploads younger than this — the monitor wants the live picture, and
/// the article window/dedup already hold recent history.
pub const VIDEO_MAX_AGE_HOURS: i64 = 24;
/// Transcript cap fed into the article body. The NLP path reads at most ~6000 chars
/// (processor truncation), so storing more is dead weight — but the cap is applied
/// via [`condense_transcript`], which packs the budget with SIGNAL-bearing sentences
/// rather than blindly keeping the first five minutes of a long video.
pub const TRANSCRIPT_MAX_CHARS: usize = 6000;
/// Bound on the raw flattened transcript read from disk (memory hygiene; ~40 min of
/// speech). Condensation reduces it to [`TRANSCRIPT_MAX_CHARS`].
pub const TRANSCRIPT_RAW_CAP: usize = 40_000;
/// yt-dlp subprocess budget per video (subtitle-only fetches run 2-10s normally).
pub const YTDLP_TIMEOUT_SECS: u64 = 90;
/// Per-cycle ceiling on NEW videos transcribed per channel, so a backfill or a
/// livestream-clip flood cannot monopolize the cycle (they will drain over
/// subsequent cycles while the age gate still holds).
pub const VIDEOS_PER_CHANNEL_PER_CYCLE: usize = 3;

/// Dormancy gate: explicit operator opt-in AND a usable yt-dlp.
pub fn enabled() -> bool {
    std::env::var("GCRM_VIDEO_SOURCES").map(|v| v == "1").unwrap_or(false)
}

/// The yt-dlp binary to run: `GCRM_YTDLP_BIN` → `~/.local/bin/yt-dlp` → PATH lookup.
pub fn ytdlp_bin() -> PathBuf {
    if let Ok(p) = std::env::var("GCRM_YTDLP_BIN") {
        return PathBuf::from(p);
    }
    if let Some(home) = std::env::var_os("HOME") {
        let local = PathBuf::from(home).join(".local/bin/yt-dlp");
        if local.exists() {
            return local;
        }
    }
    PathBuf::from("yt-dlp")
}

/// Strip a trailing "| Channel Name" (or "– Channel") suffix broadcast channels
/// append to video titles ("… | DW News"). Wire copy of the same story carries no
/// such tail, so the suffix depressed video↔wire corroboration similarity and made
/// same-story pairs double-count into modality weight (measured 2026-07-05: 11 of
/// 38 live video rows sat just under the merge bar). Mirrors the gnews outlet-
/// suffix strip at text ingest. Conservative: only a short trailing segment goes.
pub fn strip_channel_suffix(title: &str) -> &str {
    for sep in [" | ", " – ", " — "] {
        if let Some(pos) = title.rfind(sep) {
            let tail = &title[pos + sep.len()..];
            let looks_like_channel = !tail.is_empty()
                && tail.chars().count() <= 24
                && !tail.contains('?')
                && tail.chars().next().is_some_and(|c| c.is_uppercase());
            if looks_like_channel && pos > title.len() / 2 {
                return title[..pos].trim_end();
            }
        }
    }
    title
}

/// YouTube Shorts are sub-minute vertical clips/teasers — transcript value near
/// zero, feed-clutter value high (the first live cycle ingested a football short).
/// The full story, when there is one, arrives as a normal upload. Skipped pre-fetch.
pub fn is_short(url: &str) -> bool {
    url.contains("/shorts/")
}

/// A channel's auth-free Atom feed (no API key; the same URL YouTube has served
/// since 2015, also used by podcast apps — a stable public contract).
pub fn channel_feed_url(channel_id: &str) -> String {
    format!("https://www.youtube.com/feeds/videos.xml?channel_id={channel_id}")
}

/// Flatten a WebVTT auto-caption file to plain text. Auto-captions repeat lines as
/// the rolling window advances and carry inline timing tags — drop cue headers,
/// timing lines, tags, and consecutive/contained repeats, keeping one clean stream.
/// Pure; fixture-locked below.
pub fn flatten_vtt(vtt: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for raw in vtt.lines() {
        let line = raw.trim();
        if line.is_empty()
            || line.contains("-->")
            || line.starts_with("WEBVTT")
            || line.starts_with("Kind:")
            || line.starts_with("Language:")
            || line.starts_with("NOTE")
            || line.starts_with("STYLE")
            || line.chars().all(|c| c.is_ascii_digit())
        {
            continue;
        }
        // strip inline tags like <00:00:01.000><c> … </c>
        let mut clean = String::with_capacity(line.len());
        let mut in_tag = false;
        for ch in line.chars() {
            match ch {
                '<' => in_tag = true,
                '>' => in_tag = false,
                c if !in_tag => clean.push(c),
                _ => {}
            }
        }
        let clean = clean.trim().to_string();
        if clean.is_empty() {
            continue;
        }
        // rolling-window repeat: the same (or contained) line re-emitted
        if let Some(last) = out.last() {
            if *last == clean || last.contains(&clean) {
                continue;
            }
            if clean.contains(last.as_str()) {
                out.pop(); // the new line extends the previous fragment — replace it
            }
        }
        out.push(clean);
    }
    out.join(" ")
}

/// Fetch a video's English auto/manual captions via yt-dlp, flattened and capped.
/// `Ok(None)` = video reachable but no captions yet (fresh uploads caption within
/// minutes to hours) — the caller retries on a later cycle while the age gate holds.
/// `Err` = the fetch itself failed (binary missing, network, timeout).
pub async fn fetch_transcript(video_url: &str) -> anyhow::Result<Option<String>> {
    let dir = std::env::temp_dir().join(format!(
        "gcrm-video-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    tokio::fs::create_dir_all(&dir).await?;
    let out_tpl = dir.join("cap");

    let run = tokio::process::Command::new(ytdlp_bin())
        .arg("--skip-download")
        .arg("--write-auto-subs")
        .arg("--write-subs")
        .args(["--sub-langs", "en.*,en"])
        .args(["--sub-format", "vtt"])
        .args(["-o", &out_tpl.to_string_lossy()])
        .arg("--no-progress")
        .arg("--quiet")
        .arg(video_url)
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(Duration::from_secs(YTDLP_TIMEOUT_SECS), run).await;

    // Read inside an inner block so ANY error still reaches the cleanup below —
    // a bare `?` here leaked the per-video temp dir. (xhigh review finding 14)
    let read_vtt = async {
        let mut transcript: Option<String> = None;
        if let Ok(Ok(o)) = &out {
            if o.status.success() {
                // yt-dlp names the file cap.<lang>.vtt — take the first vtt produced.
                let mut rd = tokio::fs::read_dir(&dir).await?;
                while let Some(ent) = rd.next_entry().await? {
                    if ent.path().extension().and_then(|e| e.to_str()) == Some("vtt") {
                        let vtt = tokio::fs::read_to_string(ent.path()).await?;
                        let flat: String = flatten_vtt(&vtt).chars().take(TRANSCRIPT_RAW_CAP).collect();
                        if !flat.trim().is_empty() {
                            transcript = Some(flat);
                        }
                        break;
                    }
                }
            }
        }
        anyhow::Ok(transcript)
    }
    .await;
    let _ = tokio::fs::remove_dir_all(&dir).await; // best-effort cleanup, every path
    let transcript = read_vtt?;

    match out {
        Err(_) => anyhow::bail!("yt-dlp timed out after {YTDLP_TIMEOUT_SECS}s"),
        Ok(Err(e)) => anyhow::bail!("yt-dlp spawn failed: {e}"),
        Ok(Ok(o)) if !o.status.success() => {
            anyhow::bail!("yt-dlp exit {}: {}", o.status, String::from_utf8_lossy(&o.stderr))
        }
        Ok(Ok(_)) => Ok(transcript), // success; None = no captions yet
    }
}

/// Pack `max_chars` with the transcript's SIGNAL-bearing sentences. Long analyst
/// videos bury the load-bearing line mid-stream (the 2026-07-04 proof: "the Strait
/// of Hormuz is not open" arrived minutes into a market segment) — a blind head
/// truncation feeds the NLP/enricher the greeting instead. Sentences carrying a
/// geopolitical trigger (actors + conflict terms) are kept first, in order, with
/// elisions marked "…"; remaining budget fills with leading context. Pure.
pub fn condense_transcript(flat: &str, max_chars: usize) -> String {
    if flat.chars().count() <= max_chars {
        return flat.to_string();
    }
    // Sentence-ish split; auto-captions carry sparse punctuation, so fall back to
    // fixed windows when boundaries are rare.
    let mut sentences: Vec<&str> = Vec::new();
    let mut start = 0usize;
    let bytes = flat.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if matches!(b, b'.' | b'?' | b'!') && bytes.get(i + 1).is_none_or(|n| *n == b' ') {
            if let Some(s) = flat.get(start..=i) {
                if !s.trim().is_empty() { sentences.push(s.trim()); }
            }
            start = i + 1;
        }
    }
    if let Some(tail) = flat.get(start..) {
        if !tail.trim().is_empty() { sentences.push(tail.trim()); }
    }
    if sentences.len() < 4 {
        sentences = flat
            .as_bytes()
            .chunks(400)
            .filter_map(|c| std::str::from_utf8(c).ok())
            .collect();
    }
    let signal: Vec<bool> = sentences
        .iter()
        .map(|s| crate::nlp_sidecar::has_geopolitical_trigger(s))
        .collect();
    let mut kept: Vec<usize> = (0..sentences.len()).filter(|&i| signal[i]).collect();
    let mut used: usize = kept.iter().map(|&i| sentences[i].chars().count() + 2).sum();
    // Fill the remaining budget with leading context sentences (in order).
    for i in 0..sentences.len() {
        if used >= max_chars { break; }
        if !signal[i] {
            let cost = sentences[i].chars().count() + 2;
            if used + cost <= max_chars {
                kept.push(i);
                used += cost;
            }
        }
    }
    kept.sort_unstable();
    let mut out = String::with_capacity(max_chars.min(used) + 16);
    let mut prev: Option<usize> = None;
    for &i in &kept {
        if out.chars().count() + sentences[i].chars().count() + 2 > max_chars { break; }
        if let Some(p) = prev {
            out.push_str(if i == p + 1 { " " } else { " … " });
        }
        out.push_str(sentences[i]);
        prev = Some(i);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dormant_without_explicit_opt_in() {
        // The gate reads the env var directly; unless the operator exported
        // GCRM_VIDEO_SOURCES=1 into THIS test process, the loop must be off.
        if std::env::var("GCRM_VIDEO_SOURCES").map(|v| v == "1").unwrap_or(false) {
            return; // operator-enabled environment — nothing to assert here
        }
        assert!(!enabled(), "video sources must ship dormant");
    }

    #[test]
    fn channel_feed_url_is_the_stable_atom_contract() {
        assert_eq!(
            channel_feed_url("UC5aNPmKYwbudeNngDMTY3lw"),
            "https://www.youtube.com/feeds/videos.xml?channel_id=UC5aNPmKYwbudeNngDMTY3lw"
        );
    }

    #[test]
    fn watchlist_sources_are_video_suffixed_and_unique() {
        // The -video suffix keeps channel stats separable from the outlet's text feed;
        // duplicate source ids would merge per-source health/dedup accounting.
        let mut seen = std::collections::HashSet::new();
        for ch in VIDEO_CHANNELS {
            assert!(ch.source.ends_with("-video"), "{} must carry the -video suffix", ch.source);
            assert!(seen.insert(ch.source), "duplicate source id {}", ch.source);
            assert!(ch.channel_id.starts_with("UC") && ch.channel_id.len() == 24,
                "{} channel id malformed: {}", ch.source, ch.channel_id);
        }
    }

    #[test]
    fn flatten_vtt_drops_headers_timing_tags_and_rolling_repeats() {
        // Shape taken from a real YouTube auto-caption VTT (BNN Bloomberg, 2026-07-04):
        // rolling two-line windows where each cue re-emits the previous line, plus
        // inline word-timing tags inside <c> spans.
        let vtt = "WEBVTT\nKind: captions\nLanguage: en\n\n\
            00:00:00.000 --> 00:00:02.000\nhas reopened the Strait of Hormuz. Crude\n\n\
            00:00:02.000 --> 00:00:04.000\nhas reopened the Strait of Hormuz. Crude\nthe<00:00:02.500><c> Strait</c><00:00:03.000><c> of</c> Hormuz is not open.\n\n\
            00:00:04.000 --> 00:00:06.000\nthe Strait of Hormuz is not open.\nTraffic has not normalized\n";
        let flat = flatten_vtt(vtt);
        assert_eq!(
            flat,
            "has reopened the Strait of Hormuz. Crude the Strait of Hormuz is not open. Traffic has not normalized"
        );
        assert!(!flat.contains('<') && !flat.contains("-->"), "tags/timing must not survive");
    }

    #[test]
    fn channel_suffix_is_stripped_but_real_titles_survive() {
        assert_eq!(strip_channel_suffix("America's Independence Day celebrations | DW News"),
                   "America's Independence Day celebrations");
        assert_eq!(strip_channel_suffix("Divided we celebrate: America's 250th birthday | DW News"),
                   "Divided we celebrate: America's 250th birthday");
        // A pipe mid-title or a long/question tail is content, not a channel tag.
        assert_eq!(strip_channel_suffix("Unity | or political divide across the nation today"),
                   "Unity | or political divide across the nation today");
        assert_eq!(strip_channel_suffix("America's 250th birthday: Unity or political divide?"),
                   "America's 250th birthday: Unity or political divide?");
    }

    #[test]
    fn condense_keeps_the_buried_signal_sentence() {
        // The Nuttall property: the load-bearing line sits deep in a long transcript
        // — head-truncation must not lose it.
        let filler = "The market opened mixed today and analysts discussed portfolio balance. ".repeat(120);
        let signal = "The Strait of Hormuz is not open and Iran controls transit.";
        let long = format!("{filler}{signal} More closing remarks about earnings follow here.");
        let out = condense_transcript(&long, 2000);
        assert!(out.contains("Strait of Hormuz"), "buried signal sentence must survive: {out:?}");
        assert!(out.chars().count() <= 2000);
        assert!(out.contains(" … "), "elision must be marked, not silent");
    }

    #[test]
    fn condense_short_transcript_is_untouched() {
        let s = "Iran warns tankers near the strait. Traffic slows.";
        assert_eq!(condense_transcript(s, 6000), s);
    }

    #[test]
    fn shorts_urls_are_recognized() {
        assert!(is_short("https://www.youtube.com/shorts/IeMD9bNZFp4"));
        assert!(!is_short("https://www.youtube.com/watch?v=koCXfHeX6_k"));
    }

    #[test]
    fn flatten_vtt_empty_and_junk_inputs_flatten_to_empty() {
        assert_eq!(flatten_vtt(""), "");
        assert_eq!(flatten_vtt("WEBVTT\n\n1\n00:00:00.000 --> 00:00:01.000\n\n"), "");
    }

    /// Live end-to-end proof (network + yt-dlp): newest upload of the first watchlist
    /// channel yields a non-empty flattened transcript. #[ignore]d like the other
    /// feed-liveness tests — run manually: cargo test -- --ignored live_video
    #[tokio::test]
    #[ignore]
    async fn live_video_transcript_end_to_end() {
        let feed = channel_feed_url(VIDEO_CHANNELS[0].channel_id);
        let body = reqwest::get(&feed).await.expect("feed fetch").text().await.expect("feed body");
        let parsed = feed_rs::parser::parse(body.as_bytes()).expect("atom parse");
        let entry = parsed.entries.first().expect("channel has uploads");
        let url = entry.links.first().map(|l| l.href.clone()).expect("watch url");
        let transcript = fetch_transcript(&url).await.expect("yt-dlp ran");
        // A very fresh upload may not be captioned yet; that is a pass for the
        // MACHINERY (Ok(None) is the designed answer), but log it loudly.
        match transcript {
            Some(t) => {
                assert!(t.split_whitespace().count() > 50, "transcript suspiciously short: {t:?}");
                println!("live transcript OK: {} words, head: {}…", t.split_whitespace().count(),
                         t.chars().take(120).collect::<String>());
            }
            None => println!("machinery OK; newest upload not captioned yet (retry semantics engaged)"),
        }
    }
}
