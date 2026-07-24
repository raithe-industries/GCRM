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
/// Cap on the render-fingerprint set (see `is_boilerplate_render`) before it is cleared —
/// it is a dedup hint, not a ledger, so forgetting is always safe.
const SEEN_CAP: usize = THUMB_CAP * 4;

static TX: OnceLock<mpsc::Sender<String>> = OnceLock::new();
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
/// off, the worker never started, the URL is not http(s), or the thumbnail already exists.
pub fn enqueue(url: &str) {
    if !enabled() || url.is_empty() { return; }
    if !(url.starts_with("http://") || url.starts_with("https://")) { return; }
    if exists(&key_for(url)) { return; }
    if let Some(tx) = TX.get() {
        // try_send: a saturated queue drops the request rather than stalling the caller.
        let _ = tx.try_send(url.to_string());
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
            if exists(&key) { continue; }
            match capture_one(&bin, &url, &key).await {
                Ok(bytes) => {
                    debug!("Thumbs: captured {key} ({bytes} B) for {}", &url[..url.len().min(80)]);
                    evict_over_cap();
                }
                Err(e) => debug!("Thumbs: capture failed for {}: {e}",
                                 &url[..url.len().min(80)]),
            }
        }
    });
}

/// Capture one URL → downscaled JPEG on disk. Returns the stored byte size.
async fn capture_one(bin: &Path, url: &str, key: &str) -> anyhow::Result<u64> {
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
    let waited = tokio::time::timeout(
        std::time::Duration::from_secs(CAPTURE_TIMEOUT_SECS),
        child.wait(),
    ).await;
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
    fn disabled_by_env_kill_switch() {
        std::env::set_var("GCRM_THUMBS", "0");
        assert!(!enabled(), "GCRM_THUMBS=0 must disable capture");
        std::env::set_var("GCRM_THUMBS", "1");
        assert!(enabled(), "any other value leaves it on");
        std::env::remove_var("GCRM_THUMBS");
        assert!(enabled(), "default is on");
    }
}
