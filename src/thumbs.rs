// ------------------------------------------------------------
// RAiTHE INDUSTRIES INCORPORATED
// Copyright (c) 2026 All Rights Reserved.
//
// This file is part of a proprietary system. Unauthorized use,
// reproduction, or distribution is strictly prohibited.
// ------------------------------------------------------------

// src/thumbs.rs — page-view thumbnails for the timeline driver cards.
//
// A timeline knock's card used to be text only ("source · title"). This captures a real
// screenshot of the driver's source page, stores a TINY version, and serves it so the card
// shows a mini pageview the operator can recognise at a glance and click through to.
//
// Design constraints, in priority order:
//   1. NEVER on the request path. Captures run in ONE background worker off a bounded queue;
//      a full queue drops the request (thumbnails are best-effort decoration, never a blocker).
//   2. NEVER unbounded on disk. Storage is a rolling cap (THUMB_CAP files, oldest deleted), so
//      the feature cannot grow the disk — the "eventually overridden" the operator asked for.
//   3. NEVER starve prod. The browser subprocess is `nice`d and hard-timed-out; the CPU-bound
//      decode/resize runs on the blocking pool.
//   4. Degrade silently. No browser binary, a dead site, a timeout — all just mean no thumbnail;
//      the card falls back to its text rows exactly as before.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// Thumbnail directory, relative to the service CWD (same convention as `logs/`). Gitignored.
const THUMB_DIR: &str = "thumbs";
/// Rolling cap on stored thumbnails — the oldest are deleted past this, so disk use is bounded
/// at roughly THUMB_CAP × ~20 KB (≈6 MB) no matter how long the service runs.
const THUMB_CAP: usize = 300;
/// Capture viewport. Wide enough to look like the real page; downscaled before storage.
const SHOT_W: u32 = 1024;
const SHOT_H: u32 = 640;
/// Stored thumbnail width (height follows the capture aspect → 240px).
const THUMB_W: u32 = 384;
/// Stored JPEG quality — tuned for a recognisable pageview at ~15-25 KB.
const THUMB_QUALITY: u8 = 72;
/// Hard ceiling on one capture, including page load. Measured real pages span 1–28s
/// (aljazeera.com/news is the slow end), so a tighter bound just discards good captures.
const CAPTURE_TIMEOUT_SECS: u64 = 40;
/// Pending-capture queue depth. Full ⇒ drop (best-effort).
const QUEUE_DEPTH: usize = 64;
/// Circuit-breaker ceiling on one capture's process group. A normal page runs ~10–15 procs;
/// only a pathological ad/tracker page swarms past this (and times out with nothing anyway), so
/// tripping here caps the transient spike instead of riding it out for the whole timeout.
const MAX_CAPTURE_PROCS: usize = 40;
/// Quiet gap between finishing one capture and starting the next. Chromium tears its helper
/// tree (renderer/gpu/zygote) down ASYNCHRONOUSLY after the main process exits, so spawning the
/// next browser immediately stacks the dying tree on top of the new one — on a heavy page
/// (YouTube) that briefly showed ~80 procs on the shared box. This lets the previous tree fully
/// exit first, so the live process count stays at roughly ONE browser's worth.
const INTER_CAPTURE_SETTLE_MS: u64 = 750;
/// Cap on the render-fingerprint set (see `is_boilerplate_render`) before it is cleared —
/// it is a dedup hint, not a ledger, so forgetting is always safe.
const SEEN_CAP: usize = THUMB_CAP * 4;

static TX: OnceLock<mpsc::Sender<String>> = OnceLock::new();
/// Keys currently queued or being captured — so the same url enqueued by several concurrent
/// serves (multiple browser tabs bootstrapping at once) is captured ONCE, not once per serve.
/// Inserted on enqueue, removed when the capture finishes; `exists()` covers the after-the-fact
/// case, this covers the before-first-capture window.
static INFLIGHT: OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> = OnceLock::new();
/// md5 of every thumbnail this process has stored, used to spot pages that render
/// IDENTICALLY for different URLs — cookie walls, paywalls, "prove you're human"
/// interstitials, 404 templates. Those are worse than no thumbnail: they show the
/// operator a wall instead of the article they're about to open.
static SEEN: OnceLock<std::sync::Mutex<std::collections::HashMap<[u8; 16], String>>> = OnceLock::new();

/// Operator kill-switch: `GCRM_THUMBS=0` disables capture entirely (serving still works for
/// whatever is already on disk).
pub fn enabled() -> bool {
    std::env::var("GCRM_THUMBS").map(|v| v != "0").unwrap_or(true)
}

/// Stable short key for a URL — the on-disk filename and the `/api/thumb/<key>` path.
pub fn key_for(url: &str) -> String {
    format!("{:x}", md5::compute(url.as_bytes())).chars().take(20).collect()
}

/// Path a key's thumbnail occupies. Keys are hex-only (see `is_valid_key`), so this can never
/// escape THUMB_DIR via a crafted `/api/thumb/..%2f` request.
pub fn path_for(key: &str) -> PathBuf {
    Path::new(THUMB_DIR).join(format!("{key}.jpg"))
}

/// Keys are lowercase hex of bounded length — anything else is rejected before touching the
/// filesystem (path-traversal guard for the public serving endpoint).
pub fn is_valid_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= 32 && key.bytes().all(|b| b.is_ascii_hexdigit())
}

/// True when this key already has a stored thumbnail.
pub fn exists(key: &str) -> bool {
    path_for(key).is_file()
}

/// Locate a Chromium-family binary: explicit override, then Playwright's cache (already present
/// for the deploy eyes gate), then anything on PATH. `None` ⇒ the feature stays dormant.
fn browser_bin() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("GCRM_CHROME") {
        let p = PathBuf::from(p);
        if p.is_file() { return Some(p); }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let cache = Path::new(&home).join(".cache/ms-playwright");
        if let Ok(rd) = std::fs::read_dir(&cache) {
            let mut cands: Vec<PathBuf> = rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.file_name().and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("chromium")))
                .collect();
            cands.sort();               // deterministic pick across versioned dirs
            for dir in cands.into_iter().rev() {
                for rel in ["chrome-linux64/chrome", "chrome-linux/chrome", "chrome-linux/headless_shell"] {
                    let c = dir.join(rel);
                    if c.is_file() { return Some(c); }
                }
            }
        }
    }
    for name in ["chromium", "chromium-browser", "google-chrome", "google-chrome-stable"] {
        if let Ok(out) = std::process::Command::new("which").arg(name).output() {
            if out.status.success() {
                let p = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
                if p.is_file() { return Some(p); }
            }
        }
    }
    None
}

/// Queue a source URL for thumbnail capture. Cheap and non-blocking: no-ops when the feature is
/// off, the worker never started, the URL is not http(s), the thumbnail already exists, or the
/// same url is already queued/in-flight (so N concurrent bootstraps queue it once, not N times).
pub fn enqueue(url: &str) {
    if !enabled() || url.is_empty() { return; }
    if !(url.starts_with("http://") || url.starts_with("https://")) { return; }
    let key = key_for(url);
    if exists(&key) { return; }
    // Claim the key: only the caller that inserts it (not already present) proceeds to queue.
    {
        let set = INFLIGHT.get_or_init(Default::default);
        let Ok(mut g) = set.lock() else { return };
        if !g.insert(key.clone()) { return; }               // already queued/capturing — skip
    }
    if let Some(tx) = TX.get() {
        // try_send: a saturated queue drops the request rather than stalling the caller.
        if tx.try_send(url.to_string()).is_err() {
            // Dropped (queue full): release the claim so a later serve can re-queue it.
            if let Some(set) = INFLIGHT.get() { if let Ok(mut g) = set.lock() { g.remove(&key); } }
        }
    } else if let Some(set) = INFLIGHT.get() {              // no worker — don't strand the claim
        if let Ok(mut g) = set.lock() { g.remove(&key); }
    }
}

/// Release an in-flight claim once its capture attempt has finished (success or failure), so a
/// future re-capture (e.g. after the file is evicted past the cap) can be queued again.
fn clear_inflight(key: &str) {
    if let Some(set) = INFLIGHT.get() {
        if let Ok(mut g) = set.lock() { g.remove(key); }
    }
}

/// How many uncaptured urls one timeline serve may queue. Below QUEUE_DEPTH so a bootstrap
/// leaves headroom, yet large enough to cover every knock a decimated timeline actually shows.
const HYDRATE_ENQUEUE_BUDGET: usize = 48;

/// Backfill thumbnail keys onto the timeline the client is about to receive, and queue their
/// captures. The live-tick path mints keys only for FRESH knocks, but almost every hoverable
/// knock is seeded from durable history (the ring), whose refs predate this feature and carry
/// no key — so their cards render no image at all. This walks the served entries and, for each
/// driver ref with an http(s) url, sets `thumb = key_for(url)` (so the `<img>` renders) and
/// enqueues a capture. Enqueue is NEWEST-FIRST and budget-bounded: the knocks an operator
/// actually explores get captured before the queue fills; older ones still render a key and
/// fall back to text if never captured. Idempotent across serves — `enqueue` skips urls already
/// on disk, so repeated bootstraps are cheap once a capture lands. No-op when capture is off.
pub fn hydrate_timeline(entries: &mut [serde_json::Value]) {
    if !enabled() { return; }
    let mut budget = HYDRATE_ENQUEUE_BUDGET;
    for entry in entries.iter_mut().rev() {                 // rev() = newest first (entries are chronological)
        let Some(refs) = entry.get_mut("driver_refs").and_then(|v| v.as_array_mut()) else { continue };
        for r in refs.iter_mut() {
            // Own the url first so the follow-up mutation of `r` doesn't fight the borrow.
            let url = match r.get("url").and_then(|u| u.as_str()) {
                Some(u) if u.starts_with("http://") || u.starts_with("https://") => u.to_string(),
                _ => continue,
            };
            let has_thumb = r.get("thumb").and_then(|t| t.as_str()).is_some_and(|s| !s.is_empty());
            if !has_thumb {
                if let Some(obj) = r.as_object_mut() {
                    obj.insert("thumb".into(), serde_json::Value::String(key_for(&url)));
                }
            }
            if budget > 0 { enqueue(&url); budget -= 1; }
        }
    }
}

/// Start the single capture worker. Safe to call once at boot; a second call is ignored.
pub fn spawn_worker() {
    if !enabled() {
        info!("Thumbs: disabled (GCRM_THUMBS=0)");
        return;
    }
    let Some(bin) = browser_bin() else {
        info!("Thumbs: no chromium binary found — page thumbnails stay dormant");
        return;
    };
    let (tx, mut rx) = mpsc::channel::<String>(QUEUE_DEPTH);
    if TX.set(tx).is_err() { return; }          // already started
    if let Err(e) = std::fs::create_dir_all(THUMB_DIR) {
        warn!("Thumbs: cannot create {THUMB_DIR}: {e}");
        return;
    }
    seed_seen_from_disk();
    info!("Thumbs: worker up (browser {}, cap {} files)", bin.display(), THUMB_CAP);
    tokio::spawn(async move {
        // ONE at a time on purpose: a browser per driver event, serialised, so a burst of
        // knocks can never fan out into concurrent Chromium processes on the prod box.
        while let Some(url) = rx.recv().await {
            let key = key_for(&url);
            if exists(&key) { clear_inflight(&key); continue; }
            match capture_one(&bin, &url, &key).await {
                Ok(bytes) => {
                    debug!("Thumbs: captured {key} ({bytes} B) for {}", &url[..url.len().min(80)]);
                    evict_over_cap();
                }
                Err(e) => debug!("Thumbs: capture failed for {}: {e}",
                                 &url[..url.len().min(80)]),
            }
            clear_inflight(&key);
            // Let the just-finished browser's helper tree exit before spawning the next, so the
            // live process count stays at ~one browser's worth instead of stacking teardowns.
            tokio::time::sleep(std::time::Duration::from_millis(INTER_CAPTURE_SETTLE_MS)).await;
        }
    });
}

/// YouTube video id from the common url shapes (`watch?v=`, `youtu.be/`, `/shorts/`, `/live/`,
/// `/embed/`), or `None` if this is not a YouTube link. Ids are exactly 11 url-safe chars.
fn youtube_id(url: &str) -> Option<String> {
    let u = url.split('#').next().unwrap_or(url);
    let ok = |s: &str| -> Option<String> {
        let s = s.trim();
        (s.len() == 11 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
            .then(|| s.to_string())
    };
    if let Some(rest) = u.strip_prefix("https://youtu.be/").or_else(|| u.strip_prefix("http://youtu.be/")) {
        return ok(rest.split(['?', '/', '&']).next().unwrap_or(""));
    }
    if !u.contains("youtube.com/") { return None; }
    if let Some(q) = u.split('?').nth(1) {
        for kv in q.split('&') {
            if let Some(v) = kv.strip_prefix("v=") { return ok(v); }
        }
    }
    for seg in ["/shorts/", "/live/", "/embed/", "/v/"] {
        if let Some(i) = u.find(seg) {
            return ok(u[i + seg.len()..].split(['?', '/', '&']).next().unwrap_or(""));
        }
    }
    None
}

/// Store a YouTube video's poster frame as the thumbnail — a lightweight HTTP GET, no browser.
/// A headless YouTube watch/live page spawns a huge media-decode process tree (~88 procs for a
/// live stream) and usually renders a consent wall; its poster is lighter AND a truer preview.
async fn youtube_poster(vid: &str, key: &str) -> anyhow::Result<u64> {
    // maxres (1280×720) is sharp but 404s for some videos; mqdefault (320×180) is 16:9 and
    // ALWAYS present — both crop cleanly to the wide card (unlike hq/sd, which are 4:3 with bars).
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let mut bytes = None;
    for q in ["maxresdefault", "mqdefault"] {
        let u = format!("https://img.youtube.com/vi/{vid}/{q}.jpg");
        if let Ok(resp) = client.get(&u).send().await {
            if resp.status().is_success() {
                if let Ok(b) = resp.bytes().await {
                    if b.len() > 1024 { bytes = Some(b); break; }   // >1KB ⇒ a real frame, not a stub
                }
            }
        }
    }
    let Some(bytes) = bytes else { anyhow::bail!("no youtube poster for {vid}") };
    let buf = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<u8>> {
        let img = image::load_from_memory(&bytes)?;
        let thumb = img.resize(THUMB_W, u32::MAX, image::imageops::FilterType::Triangle);
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, THUMB_QUALITY).encode_image(&thumb.to_rgb8())?;
        Ok(out)
    }).await??;
    if is_boilerplate_render(&buf, key) { anyhow::bail!("duplicate youtube poster"); }
    let len = buf.len() as u64;
    tokio::fs::write(path_for(key), &buf).await?;
    Ok(len)
}

/// Count processes in a given process group by scanning `/proc/<pid>/stat` (field 5 = pgrp).
/// Cheap enough at 1 Hz during a single serialized capture; `comm` can contain spaces/parens so
/// the fields are read AFTER the final ')'.
#[cfg(unix)]
fn count_process_group(pgid: u32) -> usize {
    let mut n = 0;
    let Ok(rd) = std::fs::read_dir("/proc") else { return 0 };
    for e in rd.flatten() {
        let name = e.file_name();
        let Some(s) = name.to_str() else { continue };
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) { continue; }
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{s}/stat")) else { continue };
        let Some((_, after)) = stat.rsplit_once(')') else { continue };
        // after = " <state> <ppid> <pgrp> ..." → pgrp is the 3rd whitespace field.
        if after.split_whitespace().nth(2).and_then(|p| p.parse::<u32>().ok()) == Some(pgid) {
            n += 1;
        }
    }
    n
}
#[cfg(not(unix))]
fn count_process_group(_pgid: u32) -> usize { 0 }

/// Capture one URL → downscaled JPEG on disk. Returns the stored byte size.
async fn capture_one(bin: &Path, url: &str, key: &str) -> anyhow::Result<u64> {
    // Videos never touch the browser — fetch the poster frame instead (see `youtube_poster`).
    if let Some(vid) = youtube_id(url) {
        return youtube_poster(&vid, key).await;
    }
    let tmp_dir = std::env::temp_dir().join(format!("gcrm-thumb-{key}"));
    tokio::fs::create_dir_all(&tmp_dir).await.ok();
    let raw = tmp_dir.join("shot.png");

    // `nice` keeps the browser off prod's back; --virtual-time-budget bounds page settling so a
    // slow/never-idle site still yields a frame instead of hanging to the timeout.
    let mut cmd = tokio::process::Command::new("nice");
    cmd.arg("-n").arg("15")
        .arg(bin)
        .arg("--headless")
        .arg("--disable-gpu")
        .arg("--no-sandbox")
        .arg("--disable-dev-shm-usage")
        .arg("--hide-scrollbars")
        .arg("--disable-extensions")
        .arg("--mute-audio")
        .arg("--no-first-run")
        // Cap the renderer fan-out. A heavy page (a YouTube watch page above all) otherwise
        // spawns a renderer per origin/iframe/ad plus media-decode helpers — measured ~80
        // processes for a single capture on the shared box. `--renderer-process-limit=1` +
        // no site isolation holds one capture to ~one browser's worth of processes while the
        // screenshot stays byte-identical to the unconstrained render (flag experiment
        // 2026-07-24). `--single-process` was rejected — it SIGKILLs modern headless.
        .arg("--renderer-process-limit=1")
        .arg("--disable-features=site-per-process,IsolateOrigins,Translate")
        .arg("--disable-software-rasterizer")
        .arg("--disable-background-networking")
        .arg(format!("--screenshot={}", raw.display()))
        .arg(format!("--window-size={SHOT_W},{SHOT_H}"))
        .arg("--virtual-time-budget=7000")
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    // The browser runs in its OWN process group so a timeout can kill the WHOLE tree.
    // This is the difference between one failed capture and a leak: `timeout(cmd.status())`
    // alone merely drops the future — the child keeps running, un-reaped, forever. One slow
    // site then strands a chromium per attempt, and a few minutes of that put 135 orphaned
    // chromium processes on the box during pre-deploy verification.
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd.kill_on_drop(true).spawn()?;
    let pid = child.id();
    // Race the wait against TWO ceilings: wall-clock, and a PROCESS-COUNT circuit breaker.
    // A rare ad/tracker-heavy page (independent.co.uk measured 85 procs) spawns a swarm of
    // utility/network subprocesses that `--renderer-process-limit` can't cap, and it times
    // out anyway — so a group that blows past MAX_CAPTURE_PROCS is pathological: trip early
    // rather than let it sit at 85 procs for the full timeout. cgroup pids.max can't do this
    // (it counts THREADS, so any limit that lets chrome run also lets it explode).
    let breaker = async {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
            match pid {
                Some(p) if count_process_group(p) > MAX_CAPTURE_PROCS => break,
                _ => {}
            }
        }
    };
    let waited = tokio::select! {
        w = child.wait() => Ok(w),                              // finished on its own
        _ = tokio::time::sleep(std::time::Duration::from_secs(CAPTURE_TIMEOUT_SECS)) => Err("timeout"),
        _ = breaker => Err("process-swarm"),                    // pathological page — abort early
    };
    if waited.is_err() {
        // SIGKILL the negative pid = the whole group, so chromium's zygote/renderer/gpu
        // helpers die with the browser instead of outliving it.
        if let Some(pid) = pid {
            let _ = std::process::Command::new("kill")
                .arg("-KILL").arg(format!("-{pid}"))
                .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
                .status();
        }
        let _ = child.kill().await;                     // reap the direct child too
    }

    let ok = matches!(&waited, Ok(Ok(s)) if s.success());
    if !ok || !raw.is_file() {
        let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
        anyhow::bail!("browser produced no screenshot");
    }

    // Decode + resize + JPEG-encode is CPU-bound → off the async runtime.
    let raw_c = raw.clone();
    let buf = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<u8>> {
        let img = image::open(&raw_c)?;
        let thumb = img.resize(THUMB_W, u32::MAX, image::imageops::FilterType::Triangle);
        let mut buf = Vec::new();
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, THUMB_QUALITY);
        enc.encode_image(&thumb.to_rgb8())?;
        Ok(buf)
    }).await??;
    let _ = tokio::fs::remove_dir_all(&tmp_dir).await;

    // A wall is worse than a blank card: it shows the operator a cookie banner where the
    // article should be. Walls give themselves away by rendering pixel-identically for every
    // URL on the site (two Guardian articles produced byte-identical thumbnails), so an
    // exact render match against an ALREADY-STORED page is the tell — near-zero false
    // positives, because two genuinely different pages never encode to the same bytes.
    if is_boilerplate_render(&buf, key) {
        anyhow::bail!("identical render already stored for a different url (cookie wall / paywall / error page)");
    }

    let len = buf.len() as u64;
    tokio::fs::write(path_for(key), &buf).await?;
    Ok(len)
}

/// True when this exact render is already on disk under a DIFFERENT key. Records the
/// fingerprint when it is new, so the first page to render a given wall is kept and every
/// later URL that hits the same wall is dropped.
fn is_boilerplate_render(jpeg: &[u8], key: &str) -> bool {
    let seen = SEEN.get_or_init(Default::default);
    let Ok(mut map) = seen.lock() else { return false };   // a poisoned hint-set must not block capture
    let digest: [u8; 16] = md5::compute(jpeg).into();
    match map.get(&digest) {
        Some(owner) if owner != key => true,
        Some(_) => false,                                   // same url re-captured — allowed
        None => {
            if map.len() >= SEEN_CAP { map.clear(); }       // bounded: it is a hint, not a ledger
            map.insert(digest, key.to_string());
            false
        }
    }
}

/// Seed the render-fingerprint set from whatever survived the last run, so a restart does
/// not re-admit one copy of every wall it had already learned to reject.
fn seed_seen_from_disk() {
    let Ok(rd) = std::fs::read_dir(THUMB_DIR) else { return };
    let seen = SEEN.get_or_init(Default::default);
    let Ok(mut map) = seen.lock() else { return };
    for e in rd.filter_map(|e| e.ok()) {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("jpg") { continue; }
        let (Some(stem), Ok(bytes)) = (
            p.file_stem().and_then(|s| s.to_str()).map(str::to_string),
            std::fs::read(&p),
        ) else { continue };
        map.entry(md5::compute(&bytes).into()).or_insert(stem);
    }
}

/// Enforce the rolling cap: delete oldest-by-mtime thumbnails past THUMB_CAP. This is the
/// "eventually overridden so it never eats the drive" guarantee.
fn evict_over_cap() {
    let Ok(rd) = std::fs::read_dir(THUMB_DIR) else { return };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("jpg"))
        .filter_map(|e| {
            let md = e.metadata().ok()?;
            Some((md.modified().ok()?, e.path()))
        })
        .collect();
    if files.len() <= THUMB_CAP { return; }
    files.sort_by_key(|(t, _)| *t);                       // oldest first
    let excess = files.len() - THUMB_CAP;
    for (_, p) in files.into_iter().take(excess) {
        let _ = std::fs::remove_file(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_is_stable_hex_and_path_confined() {
        let k = key_for("https://example.com/a?b=1");
        assert_eq!(k, key_for("https://example.com/a?b=1"), "key must be stable for a URL");
        assert_ne!(k, key_for("https://example.com/a?b=2"), "different URLs must not collide");
        assert!(is_valid_key(&k) && k.len() == 20, "key should be bounded lowercase hex, got {k:?}");
        assert!(path_for(&k).starts_with(THUMB_DIR));
    }

    #[test]
    fn traversal_keys_are_rejected_before_touching_disk() {
        // /api/thumb/<key> is public: only hex keys may ever reach the filesystem, so a crafted
        // key cannot walk out of the thumbs directory.
        for bad in ["../etc/passwd", "..", "a/b", "abc.jpg", "", "zz", "../../secrets.env", "a b"] {
            assert!(!is_valid_key(bad), "must reject key {bad:?}");
        }
        assert!(is_valid_key("0a1b2c3d4e5f"), "plain hex must be accepted");
    }

    #[test]
    fn enqueue_is_inert_for_non_http_and_empty_urls() {
        // No worker is running in tests; these must simply not panic, and non-http schemes are
        // filtered before ever reaching the queue.
        enqueue("");
        enqueue("ftp://example.com/x");
        enqueue("javascript:alert(1)");
        enqueue("https://example.com/real");
    }

    #[test]
    fn identical_renders_for_different_urls_are_rejected_as_walls() {
        // The wall guard: the FIRST page to produce a given render is kept; any DIFFERENT
        // key that later produces the byte-identical render (the cookie-wall / paywall tell)
        // is rejected. Same key re-captured stays allowed (a real page refreshing).
        let wall = b"\xff\xd8 pretend-jpeg bytes of a cookie wall \xff\xd9";
        let article = b"\xff\xd8 a genuinely different article render \xff\xd9";
        assert!(!is_boilerplate_render(wall, "aaaa1111"), "first render of a page must be kept");
        assert!(is_boilerplate_render(wall, "bbbb2222"),
            "a different url with an identical render is a wall — must be rejected");
        assert!(!is_boilerplate_render(wall, "aaaa1111"),
            "the SAME url re-rendering identically is a refresh, not a wall");
        assert!(!is_boilerplate_render(article, "cccc3333"),
            "a distinct render must pass even after a wall was learned");
    }

    #[test]
    fn hydrate_backfills_keys_for_historical_refs_with_urls() {
        // The fix for "no images on the cards": refs seeded from durable history carry a url but
        // no thumb key, so their <img> never renders. hydrate must mint the key in place for any
        // http(s) url, leave non-http/keyed refs alone, and never panic on odd shapes.
        let mut entries = vec![
            serde_json::json!({ "t": "2026-07-24T00:00:00Z", "driver_refs": [
                { "source": "a", "title": "hist", "url": "https://example.com/story" },   // needs key
                { "source": "b", "title": "vid",  "url": "not-a-url" },                    // skip
                { "source": "c", "title": "kept", "url": "https://ex.com/x", "thumb": "cafebabe" }, // keep
            ]}),
            serde_json::json!({ "t": "2026-07-24T00:00:01Z" }),                            // no refs — must not panic
        ];
        hydrate_timeline(&mut entries);
        let refs = entries[0]["driver_refs"].as_array().unwrap();
        let minted = refs[0]["thumb"].as_str().unwrap();
        assert_eq!(minted, key_for("https://example.com/story"), "http ref must get its stable key");
        assert!(is_valid_key(minted));
        assert!(refs[1].get("thumb").is_none(), "a non-http url must not be given a key");
        assert_eq!(refs[2]["thumb"].as_str().unwrap(), "cafebabe", "an existing key must be preserved");
    }

    #[test]
    fn youtube_ids_parse_from_every_url_shape() {
        assert_eq!(youtube_id("https://www.youtube.com/watch?v=8OUPvOqr4kw").as_deref(), Some("8OUPvOqr4kw"));
        assert_eq!(youtube_id("https://www.youtube.com/watch?a=1&v=hzHTskkwqyo&t=2").as_deref(), Some("hzHTskkwqyo"));
        assert_eq!(youtube_id("https://youtu.be/8OUPvOqr4kw?si=x").as_deref(), Some("8OUPvOqr4kw"));
        assert_eq!(youtube_id("https://www.youtube.com/live/abcdefghijk").as_deref(), Some("abcdefghijk"));
        assert_eq!(youtube_id("https://www.youtube.com/shorts/ABCDE_-1234").as_deref(), Some("ABCDE_-1234"));
        // Not YouTube, or not a real id → None (falls through to the browser capture path).
        assert_eq!(youtube_id("https://taskandpurpose.com/news/x"), None);
        assert_eq!(youtube_id("https://www.youtube.com/watch?v=short"), None);   // wrong length
        assert_eq!(youtube_id("https://www.youtube.com/feed/subscriptions"), None);
    }

    #[test]
    fn inflight_claim_dedupes_then_releases() {
        // The in-flight set: the first claim of a key succeeds, a second (concurrent serve) is
        // rejected, and clearing it lets a later re-capture claim again. Exercised directly since
        // enqueue's queue side needs a running worker.
        let set = INFLIGHT.get_or_init(Default::default);
        let key = "aa11bb22cc33";
        { let mut g = set.lock().unwrap(); g.remove(key); }        // clean slate
        assert!(set.lock().unwrap().insert(key.to_string()), "first claim must win");
        assert!(!set.lock().unwrap().insert(key.to_string()), "second concurrent claim must be rejected");
        clear_inflight(key);
        assert!(set.lock().unwrap().insert(key.to_string()), "after clear, the key can be claimed again");
        clear_inflight(key);
    }

    #[test]
    fn disabled_by_env_kill_switch() {
        std::env::set_var("GCRM_THUMBS", "0");
        assert!(!enabled(), "GCRM_THUMBS=0 must disable capture");
        std::env::set_var("GCRM_THUMBS", "1");
        assert!(enabled(), "any other value leaves it on");
        std::env::remove_var("GCRM_THUMBS");
        assert!(enabled(), "default is on");
    }
}
