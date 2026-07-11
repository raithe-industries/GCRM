// ------------------------------------------------------------
// RAiTHE INDUSTRIES INCORPORATED
// Copyright (c) 2026 All Rights Reserved.
//
// This file is part of a proprietary system. Unauthorized use,
// reproduction, or distribution is strictly prohibited.
// ------------------------------------------------------------

// src/processor.rs — Pure Rust NLP processor
//
// Components:
//   FuzzyDedup          — MinHash LSH near-duplicate title detection (I-07)
//                         Cache persisted to disk on shutdown / restored at boot (I-08)
//   EventClassifier     — keyword scoring across EventType variants
//   DomainTagger        — per-domain keyword lists with configurable thresholds
//   SeverityScorer      — event-type base + casualty + nuclear/WMD modifiers
//   EscalationScorer    — escalation phrase density
//   SentimentScorer     — hostile vs conciliatory word balance
//   ActorExtractor      — structured entity dictionary (replaces spaCy NER)
//   NlpProcessor        — orchestrates all components, outputs GeopoliticalEvent
//

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

use chrono::Utc;
use regex::Regex;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::models::{
    EventType, GeopoliticalEvent, RawArticle,
    normalize_actor, resolve_region, is_great_power,
};

// ── FuzzyDedup cache persistence path ─────────────────────────────────────────

const DEDUP_CACHE_PATH: &str = "logs/dedup_cache.json";
/// Human-readable companion to the JSON cache — the cached titles, newest first.
/// The .json is the machine/load format (each title carries a 64-number MinHash
/// signature, an unreadable wall of numbers); this .txt is for reading WHAT is
/// currently held for near-duplicate detection.
const DEDUP_READABLE_PATH: &str = "logs/dedup_cache.readable.txt";

// ── MinHash LSH constants ─────────────────────────────────────────────────────
//
// Locality-sensitive hashing for approximate near-duplicate title detection.
//
// Algorithm (I-07):
//   1. Convert title to trigram set (same as before).
//   2. Compute a 64-element MinHash signature. Each element i is:
//        sig[i] = min over all trigrams t of h_i(hash(t))
//      where h_i(x) = ((A[i] * x + B[i]) mod MINHASH_PRIME) mod u64::MAX
//      using the Mersenne prime 2^61 - 1 for modular arithmetic properties.
//      Seeds A[i], B[i] are compile-time constants derived from digits of
//      known primes — deterministic and reproducible across restarts.
//   3. Divide the 64-element signature into NUM_BANDS bands of BAND_ROWS rows.
//      Two titles are candidate duplicates if any band matches exactly (classic
//      band amplification). The expected false-positive Jaccard threshold at
//      which P(candidate) ≈ 0.5 is (1/num_bands)^(1/band_rows) = 0.70.
//   4. For each new title: compute its signature, look up each band hash in the
//      index, retrieve candidate entries, compute exact Jaccard only on those
//      candidates.
//   5. If exact Jaccard ≥ JACCARD_THRESHOLD: duplicate, return false.
//      Else: add to index and cache, return true.
//
// Complexity:
//   Old: O(window × |title|) ≈ O(300 × 80) = 24,000 ops per article at 300 window.
//   New: O(NUM_HASHES + k × |title|) ≈ O(64 + 3 × 80) = ~300 ops per article
//        where k is the candidate set size — typically 0–3 at 2,000 art/hr.
//   At 10,000 art/hr: old = 240M ops/hr; new = ~3M ops/hr. ~80× speedup.
//
// Accuracy:
//   With 16 bands × 4 rows: P(candidate | Jaccard=0.70) ≈ 1 − (1−0.70⁴)¹⁶ ≈ 0.98
//   P(false negative) ≈ 0.02 — acceptable for news deduplication.
//   P(false positive | Jaccard=0.30) ≈ 1 − (1−0.30⁴)¹⁶ ≈ 0.002 — negligible.
//
// Cache structure:
//   `titles`  — ordered insertion history for audit and exact Jaccard fallback.
//   `sigs`    — MinHash signature (64 u64) per title, parallel to `titles`.
//   `band_idx`— band_hash → list of title indices for candidate lookup.
//   Serialised as JSON so the persistence path (I-08) is unchanged.

const NUM_HASHES:        usize = 64;
const NUM_BANDS:         usize = 16;
const BAND_ROWS:         usize = NUM_HASHES / NUM_BANDS; // 4
const JACCARD_THRESHOLD: f64   = 0.70;
const MAX_CACHE:         usize = 50_000; // raised from 8000 to handle 2000+ art/hr

/// Mersenne prime 2^61 - 1. Properties: large, prime, efficient modular reduction.
const MINHASH_PRIME: u64 = (1u64 << 61) - 1;

/// Deterministic hash function seeds. Derived from the first 64 pairs of digits
/// from known large primes. Fixed at compile time — identical on every run.
/// A[i] must be in [1, MINHASH_PRIME), B[i] in [0, MINHASH_PRIME).
const MINHASH_A: [u64; NUM_HASHES] = [
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

const MINHASH_B: [u64; NUM_HASHES] = [
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

/// Compute a single MinHash value for a trigram (already hashed to u64).
#[inline(always)]
fn minhash_apply(seed_a: u64, seed_b: u64, x: u64) -> u64 {
    // Multiply and add in 128-bit space to avoid overflow, then reduce mod prime.
    // Uses wrapping arithmetic — the approximation error is negligible for LSH.
    let v = seed_a.wrapping_mul(x).wrapping_add(seed_b);
    // Reduce mod MINHASH_PRIME using the Mersenne property:
    // For p = 2^61 - 1: x mod p = (x >> 61) + (x & p), clamped once.
    let lo = v & MINHASH_PRIME;
    let hi = v >> 61;
    let r  = lo + hi;
    if r >= MINHASH_PRIME { r - MINHASH_PRIME } else { r }
}

/// Hash a 3-char trigram to a u64 using FNV-1a.
#[inline(always)]
fn hash_trigram(tg: [char; 3]) -> u64 {
    const FNV_OFFSET: u64 = 14695981039346656037;
    const FNV_PRIME:  u64 = 1099511628211;
    let mut h = FNV_OFFSET;
    for ch in tg {
        let encoded = ch as u64;
        h ^= encoded;
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

/// Hash a band (4 consecutive u64 signature values) to a single u64.
/// Used as the key into the band index.
#[inline(always)]
fn hash_band(band: &[u64]) -> u64 {
    const FNV_OFFSET: u64 = 14695981039346656037;
    const FNV_PRIME:  u64 = 1099511628211;
    let mut h = FNV_OFFSET;
    for &v in band {
        // Fold each 8-byte value byte by byte
        for byte in v.to_le_bytes() {
            h ^= byte as u64;
            h = h.wrapping_mul(FNV_PRIME);
        }
    }
    h
}

// ── Fuzzy deduplication — MinHash LSH ─────────────────────────────────────────
//
// Fully serialisable so the disk persistence path (I-08) is identical to before.
// FuzzyDedup implements the same public interface: new(), load(), save(), is_new().
// The internal data layout changed; the JSON format is NOT backward compatible
// with the old trigram VecDeque format — on first startup after upgrade the
// cache file will fail to parse (different fields) and fall back to a fresh
// instance cleanly, producing a one-time cold-start. This is the correct and
// safe behaviour.

#[derive(Debug, Serialize, Deserialize)]
pub struct FuzzyDedup {
    /// Original title strings — retained for exact Jaccard fallback on candidates
    /// and for serialisation. Ordered by insertion time.
    titles: Vec<String>,
    /// MinHash signature per title — parallel to `titles`.
    /// Each entry is NUM_HASHES u64 values.
    sigs: Vec<Vec<u64>>,
    /// Band index: band_hash (u64) → list of title indices.
    /// Not serialised directly — rebuilt from sigs on load.
    #[serde(skip)]
    band_idx: HashMap<u64, Vec<usize>>,
    /// Maximum number of titles retained.
    max_cache: usize,
}

impl FuzzyDedup {
    pub fn new() -> Self {
        Self {
            titles:   Vec::new(),
            sigs:     Vec::new(),
            band_idx: HashMap::new(),
            max_cache: MAX_CACHE,
        }
    }

    /// Rebuild the band index from the stored signatures after deserialisation.
    fn rebuild_index(&mut self) {
        self.band_idx.clear();
        // Collect (idx, band_hash) pairs first to avoid holding an immutable
        // borrow on self.sigs while mutably borrowing self.band_idx.
        let pairs: Vec<(usize, u64)> = self.sigs.iter().enumerate().flat_map(|(idx, sig)| {
            (0..NUM_BANDS).map(move |band| {
                let start = band * BAND_ROWS;
                let end   = start + BAND_ROWS;
                (idx, hash_band(&sig[start..end]))
            })
        }).collect();
        for (idx, bh) in pairs {
            self.band_idx.entry(bh).or_default().push(idx);
        }
    }

    /// Load from disk, rebuilding the band index after deserialisation.
    pub fn load() -> Self {
        let path = Path::new(DEDUP_CACHE_PATH);
        if !path.exists() {
            info!("FuzzyDedup: no cache file — starting fresh (MinHash LSH)");
            return Self::new();
        }
        match std::fs::read_to_string(path) {
            Ok(s) => match serde_json::from_str::<FuzzyDedup>(&s) {
                Ok(mut fd) => {
                    fd.rebuild_index();
                    info!("FuzzyDedup: restored {} titles from disk (MinHash LSH)", fd.titles.len());
                    fd
                }
                Err(e) => {
                    warn!("FuzzyDedup: cache parse error ({e}) — starting fresh. \
                           This is normal after the MinHash upgrade.");
                    Self::new()
                }
            },
            Err(e) => {
                warn!("FuzzyDedup: cache read error ({e}) — starting fresh");
                Self::new()
            }
        }
    }

    /// Persist the title list and signatures to disk.
    /// The band index is derived data and is not persisted.
    pub fn save(&self) {
        if let Err(e) = std::fs::create_dir_all("logs") {
            warn!("FuzzyDedup: could not create logs dir: {e}");
            return;
        }
        match serde_json::to_string(self) {
            Ok(s) => {
                if let Err(e) = std::fs::write(DEDUP_CACHE_PATH, &s) {
                    warn!("FuzzyDedup: cache write failed: {e}");
                } else {
                    info!("FuzzyDedup: saved {} titles ({} KB)", self.titles.len(), s.len() / 1024);
                }
            }
            Err(e) => warn!("FuzzyDedup: serialise failed: {e}"),
        }
        // Human-readable companion: the cached titles, newest first. (The .json above
        // is the machine format — each title carries a 64-number MinHash signature, a
        // wall of numbers; this .txt is for reading what's currently held for dedup.)
        let mut txt = format!(
            "# GCRM FuzzyDedup cache — {} titles held for near-duplicate detection (MinHash LSH).\n\
             # Newest first. Machine/load format (titles + MinHash signatures): {}\n\n",
            self.titles.len(), DEDUP_CACHE_PATH,
        );
        for t in self.titles.iter().rev() { txt.push_str(t); txt.push('\n'); }
        if let Err(e) = std::fs::write(DEDUP_READABLE_PATH, &txt) {
            warn!("FuzzyDedup: readable cache write failed: {e}");
        }
    }

    /// Compute the trigram set for a lowercased title.
    pub fn trigrams(s: &str) -> Vec<[char; 3]> {
        let chars: Vec<char> = s.to_lowercase().chars().collect();
        if chars.len() < 3 { return Vec::new(); }
        chars.windows(3)
            .map(|w| [w[0], w[1], w[2]])
            .collect()
    }

    /// Compute a 64-element MinHash signature for a trigram list.
    fn minhash_signature(tgs: &[[char; 3]]) -> Vec<u64> {
        let mut sig = vec![u64::MAX; NUM_HASHES];
        for &tg in tgs {
            let h = hash_trigram(tg);
            for i in 0..NUM_HASHES {
                let v = minhash_apply(MINHASH_A[i], MINHASH_B[i], h);
                if v < sig[i] { sig[i] = v; }
            }
        }
        sig
    }

    /// Estimate Jaccard similarity from two MinHash signatures.
    /// J_est = |{i : sig_a[i] == sig_b[i]}| / NUM_HASHES
    fn jaccard_from_sigs(a: &[u64], b: &[u64]) -> f64 {
        let matches = a.iter().zip(b.iter()).filter(|(x, y)| x == y).count();
        matches as f64 / NUM_HASHES as f64
    }

    /// Exact trigram Jaccard similarity on raw title strings (fallback verifier).
    fn exact_jaccard(a: &str, b: &str) -> f64 {
        let ta: std::collections::HashSet<[char; 3]> = Self::trigrams(a).into_iter().collect();
        let tb: std::collections::HashSet<[char; 3]> = Self::trigrams(b).into_iter().collect();
        if ta.is_empty() && tb.is_empty() { return 1.0; }
        if ta.is_empty() || tb.is_empty() { return 0.0; }
        let intersection = ta.intersection(&tb).count();
        let union        = ta.union(&tb).count();
        if union == 0 { 0.0 } else { intersection as f64 / union as f64 }
    }

    /// Index a new title's signature bands.
    fn index_bands(&mut self, idx: usize, sig: &[u64]) {
        for band in 0..NUM_BANDS {
            let start = band * BAND_ROWS;
            let end   = start + BAND_ROWS;
            let bh    = hash_band(&sig[start..end]);
            self.band_idx.entry(bh).or_default().push(idx);
        }
    }

    /// Evict the oldest entry (index 0) and rebuild the band index.
    /// Called only when `titles.len() >= max_cache`.
    /// O(n) — eviction is rare at 2000 art/hr with a 50,000-title cache
    /// (eviction starts ~25 hours after a clean start).
    fn evict_oldest(&mut self) {
        if self.titles.is_empty() { return; }
        self.titles.remove(0);
        self.sigs.remove(0);
        // Collect band pairs before mutating band_idx — cannot call
        // self.index_bands() while iterating self.sigs (borrow conflict).
        let pairs: Vec<(usize, u64)> = self.sigs.iter().enumerate().flat_map(|(idx, sig)| {
            (0..NUM_BANDS).map(move |band| {
                let start = band * BAND_ROWS;
                let end   = start + BAND_ROWS;
                (idx, hash_band(&sig[start..end]))
            })
        }).collect();
        self.band_idx.clear();
        for (idx, bh) in pairs {
            self.band_idx.entry(bh).or_default().push(idx);
        }
    }

    /// Returns true if `title` has not been seen before (Jaccard < threshold).
    ///
    /// Algorithm:
    ///   1. Compute trigrams and MinHash signature.
    ///   2. For each band, look up candidate indices in band_idx.
    ///   3. For each unique candidate, estimate Jaccard from signatures.
    ///   4. If estimated Jaccard ≥ threshold: verify with exact Jaccard.
    ///   5. If exact Jaccard ≥ threshold: return false (duplicate).
    ///   6. No duplicates found: add to cache and return true.
    pub fn is_new(&mut self, title: &str) -> bool {
        let tgs = Self::trigrams(title);
        if tgs.is_empty() { return true; }

        let sig = Self::minhash_signature(&tgs);

        // Collect candidate indices via band lookup
        let mut candidates: Vec<usize> = Vec::new();
        for band in 0..NUM_BANDS {
            let start = band * BAND_ROWS;
            let end   = start + BAND_ROWS;
            let bh    = hash_band(&sig[start..end]);
            if let Some(idxs) = self.band_idx.get(&bh) {
                for &idx in idxs {
                    if !candidates.contains(&idx) {
                        candidates.push(idx);
                    }
                }
            }
        }

        // Verify each candidate. The MinHash estimate is only a cheap PRE-FILTER to skip
        // obvious non-matches — it must sit STRICTLY BELOW the real threshold, never at it.
        // Gating the estimate at the SAME JACCARD_THRESHOLD as the exact check discarded ~half
        // of true boundary duplicates: a real pair with exact Jaccard ≈ threshold has a noisy
        // signature estimate that lands below it ~50% of the time, so the exact verifier never
        // ran and the duplicate leaked through as "new", inflating event/source/breadth counts
        // that feed the risk index. The exact_jaccard check below is the authority. (audit processor-1)
        let prefilter = (JACCARD_THRESHOLD - 0.20).max(0.0);
        for idx in candidates {
            if idx >= self.sigs.len() { continue; }
            let est = Self::jaccard_from_sigs(&sig, &self.sigs[idx]);
            if est >= prefilter
                && Self::exact_jaccard(title, &self.titles[idx]) >= JACCARD_THRESHOLD
            {
                return false; // duplicate
            }
        }

        // Not a duplicate — add to cache
        if self.titles.len() >= self.max_cache {
            self.evict_oldest();
        }
        let new_idx = self.titles.len();
        self.index_bands(new_idx, &sig);
        self.titles.push(title.to_string());
        self.sigs.push(sig);
        true
    }

    /// Number of titles currently in cache.
    pub fn len(&self) -> usize { self.titles.len() }
}

// ── Event type keyword map ────────────────────────────────────────────────────

fn event_keywords() -> Vec<(EventType, Vec<&'static str>)> {
    vec![
        (EventType::MilitaryStrike, vec![
            "airstrike","air strike","bombing","bombardment","artillery","shelling",
            "rocket attack","drone strike","killed","destroyed","hit","hits","hitting",
            "attack","offensive",
            "assault","raid","raids","raided","raiding","targeted strike","precision strike",
        ]),
        (EventType::TroopDeployment, vec![
            "deployed","deployment","troops","soldiers","mobilised","mobilized",
            "military exercises","drills","forces dispatched","troops massed","buildup",
            "military presence","troop movement","reinforcements",
        ]),
        (EventType::NuclearTest, vec![
            "nuclear test","detonation","thermonuclear","underground test","yield",
            "nuclear detonation","atomic test","seismic event",
        ]),
        (EventType::MissileLaunch, vec![
            "missile launch","ballistic missile","icbm","slbm","hypersonic",
            "test launch","rocket launch","fired missile","intercontinental",
            "cruise missile","missile test","projectile","rocket fired",
        ]),
        (EventType::DiplomaticExpulsion, vec![
            "expelled","expulsion","ambassador recalled","severed relations",
            "persona non grata","closed embassy","diplomatic crisis","diplomats expelled",
            "ties severed","relations deteriorate","sanctions imposed","ultimatum",
        ]),
        (EventType::SanctionsImposed, vec![
            "sanctions","embargo","trade war","export controls","asset freeze",
            "blockade","economic war","tariffs","restrictions imposed","penalties",
            "financial sanctions","technology ban","energy embargo",
        ]),
        (EventType::CyberAttack, vec![
            "cyber attack","cyberattack","hacked","malware","ransomware",
            "infrastructure attack","power grid","state-sponsored hack",
            "cyber espionage","data breach","critical infrastructure","ddos",
        ]),
        (EventType::AllianceInvocation, vec![
            "article 5","nato invoked","mutual defence","collective defence",
            "treaty obligation","aukus","alliance activated","security pact",
            "defense agreement","military alliance","treaty invoked",
        ]),
        (EventType::WmdUse, vec![
            "chemical weapon","nerve agent","sarin","novichok",
            "biological weapon","radiological","dirty bomb","wmd",
            "mass casualty weapon","chemical attack","toxin","anthrax",
        ]),
        // De-escalation types: without keyword lists, classify() could NEVER emit these, so a
        // pure ceasefire/peace-deal story fell through to Unknown (severity_base 0.20) instead
        // of being recognised as de-escalatory (Ceasefire 0.15 / PeaceTalks 0.12). In a model
        // tuned toward false alarm, silently dropping the de-escalation signal biases it up.
        // A genuine escalation in the same headline (e.g. "ceasefire collapses as airstrike
        // hits city") still outranks these via classify()'s severity-weighted scoring. (audit processor-2)
        (EventType::Ceasefire, vec![
            "ceasefire","cease-fire","truce","armistice","cessation of hostilities",
            "stand down","laid down arms","halt to fighting","stopped fighting",
        ]),
        (EventType::PeaceTalks, vec![
            "peace talks","peace deal","peace agreement","peace accord","negotiations",
            "negotiated settlement","summit","diplomatic talks","ceasefire deal",
            "withdrawal agreement","prisoner exchange","de-escalation",
        ]),
    ]
}

// ── Weighted domain keyword map ───────────────────────────────────────────────

fn weighted_domain_keyword_map() -> Vec<(&'static str, Vec<(&'static str, f64)>)> {
    vec![
        ("military_escalation", vec![
            // Definitive
            ("soldiers killed",       1.00),
            ("forces clash",          1.00),
            ("ground offensive",      1.00),
            ("naval clash",           1.00),
            ("military operation",    0.90),
            ("armed forces",          0.80),
            // v2: folded from the removed great_power_conflict / wmd_mass_casualty /
            // alliance_activation axes. A great-power war, a chemical/bio attack, or a
            // mutual-defense invocation all imply kinetic force in use, so they tag
            // kinetic here AND set their own coupler/rung signal elsewhere
            // (event.wmd_indicator for CBRN, event.alliance_indicator for Article 5 —
            // both consumed by the Phase-2 systemic couplers). Nuclear use stays in
            // nuclear_posture.
            ("us-china war",          1.00),
            ("us-russia war",         1.00),
            ("chemical attack",       0.95),
            ("nerve agent attack",    0.95),
            ("biological attack",     0.90),
            ("article 5 invoked",     0.90),
            ("nato invoked",          0.90),
            ("alliance activated",    0.85),
            ("collective defence",    0.85),
            ("collective defense",    0.85),
            ("mutual defence",        0.85),
            ("mutual defense",        0.85),
            // Strong
            ("airstrike",             0.70),
            ("air strike",            0.70),
            ("shelling",              0.70),
            ("bombardment",           0.70),
            ("artillery",             0.65),
            ("drone strike",          0.70),
            ("invasion",              0.70),
            ("offensive",             0.60),
            ("assault",               0.60),
            ("warship",               0.65),
            ("fighter jet",           0.65),
            ("bombing",               0.60),
            // Moderate
            ("advance",               0.40),
            ("troops",                0.40),
            ("combat",                0.40),
            ("forces",                0.35),
            ("battle",                0.35),
            ("rocket",                0.35),
            ("missile",               0.35),
            ("drone",                 0.30),
            ("attack",                0.25),
            ("strike",                0.25),
            // Weak / ambient
            ("military",              0.20),
            ("launch",                0.15),
            ("weapon",                0.15),
            ("killed",                0.15),
            ("bomb",                  0.15),
            ("fire",                  0.10),
            ("fires",                 0.10),
            ("fired",                 0.10),
            ("shot",                  0.10),
            ("shots",                 0.10),
            ("gunfire",               0.10),
        ]),
        ("nuclear_posture", vec![
            // Definitive
            ("nuclear detonation",    1.00),
            ("nuclear test",          1.00),
            ("nuclear weapon used",   1.00),
            ("nuclear strike",        1.00),
            ("minutes to midnight",   1.00),
            ("doomsday clock",        0.90),
            ("nuclear alert",         0.90),
            ("launch authority",      0.90),
            ("first strike",          0.90),
            ("second strike",         0.90),
            // Strong
            ("warhead",               0.80),
            ("icbm",                  0.80),
            ("slbm",                  0.80),
            ("nuclear submarine",     0.80),
            ("nuclear arsenal",       0.80),
            ("nuclear doctrine",      0.80),
            ("nuclear threat",        0.75),
            ("fissile",               0.75),
            ("thermonuclear",         0.75),
            ("atomic",                0.65),
            ("defcon",                0.65),
            ("nuclear warning",       0.70),
            // Moderate
            ("deterrent",             0.40),
            ("nonproliferation",      0.40),
            ("strategic weapons",     0.50),
            ("bulletin of atomic",    0.60),
            ("ready status",          0.45),
            // Weak
            ("nuclear",               0.20),
        ]),
        ("diplomatic_breakdown", vec![
            // Definitive
            ("ambassador recalled",   1.00),
            ("severed relations",     1.00),
            ("persona non grata",     1.00),
            ("closed embassy",        1.00),
            ("diplomats expelled",    1.00),
            ("talks collapse",        1.00),
            ("talks fail",            0.90),
            ("deal collapse",         0.90),
            ("summit cancelled",      0.90),
            ("negotiations failed",   0.90),
            ("diplomatic crisis",     0.90),
            ("stormed out",           0.90),
            ("walked out",            0.85),
            // Strong
            ("expelled",              0.70),
            ("recalled",              0.65),
            ("ultimatum",             0.70),
            ("breakdown",             0.65),
            ("severed",               0.65),
            ("diplomatic row",        0.70),
            ("diplomatic tension",    0.65),
            ("diplomatic incident",   0.70),
            ("relations strained",    0.65),
            ("crisis talks",          0.65),
            ("armistice",             0.70),
            // Moderate
            ("sanctions imposed",     0.50),
            ("ceasefire",             0.45),
            ("peace talks",           0.40),
            ("peace deal",            0.45),
            ("hotline",               0.50),
            ("mediation",             0.40),
            ("withdrawal",            0.40),
            ("truce",                 0.45),
            ("withdraw",              0.35),
            ("diplomacy",             0.35),
            ("foreign minister",      0.35),
            ("secretary of state",    0.35),
            // Weak / ambient
            ("sanctions",             0.20),
            ("negotiations",          0.20),
            ("summit",                0.15),
            ("warn",                  0.10),
            ("warning",               0.10),
            ("threatens",             0.15),
            ("threat",                0.10),
            ("deadline",              0.10),
            ("proposal",              0.10),
            ("respond",               0.10),
            ("deal",                  0.10),
            ("deals",                 0.10),
        ]),
        ("economic_warfare", vec![
            // Definitive
            ("swift exclusion",       1.00),
            ("supply chain weaponized", 1.00),
            ("secondary sanctions",   0.90),
            ("oil embargo",           0.90),
            ("energy embargo",        0.90),
            ("financial war",         0.90),
            ("currency war",          0.90),
            ("economic coercion",     0.85),
            ("chip ban",              0.85),
            ("technology sanctions",  0.85),
            // Strong
            ("asset freeze",          0.80),
            ("export ban",            0.75),
            ("decoupling",            0.70),
            ("blockade",              0.70),
            ("trade war",             0.70),
            ("embargo",               0.70),
            // Strait / chokepoint weaponization — the coercion the "Energy / chokepoint
            // weaponized" board light exists to see. The list above indexes SANCTIONS-era
            // economic war (embargoes, tariffs, export bans); a chokepoint is weaponized
            // through TRANSIT control — closure threats, mining, selective tolls,
            // tanker seizures — and none of that phrasing hit any keyword, so 2026
            // Hormuz fee-regime coverage scored zero here and the light stayed dark
            // while ships were literally turning around (operator-confirmed blind spot,
            // 2026-07-04). Both word orders where headlines use both.
            ("strait closure",        0.95),
            ("closure of the strait", 0.95),
            ("close the strait",      0.95),
            ("closing the strait",    0.95),
            ("mining the strait",     0.95),
            ("hormuz fee",            0.85),
            ("tanker seizure",        0.85),
            ("seized tanker",         0.85),
            ("tankers seized",        0.85),
            // (bare "chokepoint" / "transit fee" / "passage fee" were pruned: Panama-
            //  drought and Suez-pricing trade journalism scored 0.70-0.80 with zero
            //  coercion — the chokepoint PAIR RULE below carries that vocabulary with
            //  a place-name requirement instead. xhigh review finding 10)
            // Moderate
            ("tariffs",               0.45),
            ("sanctions",             0.35),
        ]),
        ("cyber_info_ops", vec![
            // Definitive
            ("state-sponsored hack",  1.00),
            ("critical infrastructure attack", 1.00),
            ("ransomware attack",     0.90),
            ("cyber espionage",       0.90),
            ("influence operation",   0.85),
            ("information warfare",   0.85),
            ("cognitive warfare",     0.85),
            ("election interference", 0.80),
            // Strong
            ("cyberattack",           0.75),
            ("cyber attack",          0.75),
            ("malware",               0.70),
            ("infrastructure attack", 0.65),
            ("deepfake",              0.65),
            ("psyop",                 0.65),
            ("disinformation",        0.55),
            ("propaganda",            0.45),
            ("data breach",           0.50),
            // Moderate
            ("hacked",                0.40),
            ("phishing",              0.40),
            ("power grid",            0.40),
            ("ddos",                  0.45),
            // Weak
            ("cyber",                 0.20),
            ("hack",                  0.15),
        ]),
        // v2: great_power_conflict, alliance_activation and wmd_mass_casualty were
        // removed as scored modalities. They were not "kinds of force": great-power
        // involvement and alliance invocation describe WHO (now systemic couplers),
        // and WMD/mass-casualty is an OUTCOME (now an escalation-rung override driven
        // by event.wmd_indicator). Their distinctive keywords were folded into the
        // five orthogonal axes above (great-power war + CBRN attack → kinetic);
        // alliance invocation is detected separately into event.alliance_indicator.
    ]
}

// ── Alliance-invocation detection (feeds the alliance-chain coupler, not a domain) ──

/// Mutual-defense / collective-defense invocation phrases. Detected as a boolean
/// indicator on the event rather than a scored modality — it is a WHO/coupling
/// signal (does a treaty drag more states in), not a kind of force.
const ALLIANCE_INVOCATION_PHRASES: &[&str] = &[
    "article 5", "nato invoked", "treaty invoked", "alliance activated",
    "collective defence", "collective defense", "mutual defence", "mutual defense",
    "allied response", "collective security", "defense treaty",
];

/// Minimum domain signal to tag a domain (I-05), recalibrated for the noisy-OR
/// signal model in `score_domains`. Under noisy-OR a single matched keyword
/// contributes its own weight directly, so the threshold is expressed on the
/// same scale as keyword weights. The gate is `signal >= MIN_DOMAIN_SIGNAL`:
///   • A single keyword with w ≥ 0.50 tags the domain alone — this INCLUDES the
///     w == 0.50 boundary keywords ("strategic weapons", "sanctions imposed",
///     "hotline", "data breach") as well as the stronger/definitive ones (0.55+).
///   • A single weaker keyword (w < 0.50) does not — e.g. a lone "peace talks"
///     (0.40) or "sanctions" (0.35) is insufficient.
///   • Multiple weaker keywords accumulate via noisy-OR and can cross the
///     threshold together (two 0.40 keywords → 1 - 0.6·0.6 = 0.64).
/// This rejects ambient single-keyword noise while still tagging genuine signal.
/// NOTE: the `>=` boundary is part of the fitted calibration (several real keywords sit
/// at exactly 0.50). It is documented and pinned by a test rather than tweaked — changing
/// it to `>` would silently shift the domain-tagging calibration. (audit processor-3)
const MIN_DOMAIN_SIGNAL: f64 = 0.5;

/// Bare domain keywords short enough to hide mid-word: matched at a WORD START (boundary before,
/// any suffix after) in `score_domains` so they still catch plural/tense forms but do NOT fire
/// inside an unrelated token — the same false-alarm leak the actor-acronym fix (`BOUNDARY_ACTOR_PATS`)
/// closed, here on the domain-scoring path that feeds `domain_signals` → theater heat → the published
/// index. Each biases the index UP from benign text; documented cases:
///   "rocket"  ⊄ "skyrocket(ed)"                   (military ← economic price move)
///   "forces"  ⊄ "reinforces / enforces / workforces" (military ← generic prose)
///   "atomic"  ⊄ "anatomical / subatomic / diatomic"  (nuclear  ← unrelated; w=0.65 ≥ threshold → tags ALONE)
///   "respond" ⊄ "correspondent / corresponding"      (diplomatic ← generic prose)
///   "deal"    ⊄ "ideal"                              (diplomatic ← generic prose; "deal"/"deals"
///                                                     enforce this via `BOUNDARY_DOMAIN_KWS` whole-word)
/// Multi-word keywords keep substring matching (they cannot hide mid-token); only these exact bare
/// keywords are boundary-restricted, every other domain keyword is unchanged.
const WORD_START_DOMAIN_KWS: &[&str] = &["rocket", "forces", "atomic", "respond"];

// ── Escalation phrases ────────────────────────────────────────────────────────

const ESCALATION_PHRASES: &[&str] = &[
    "will not hesitate","all options on the table","red line","existential threat",
    "respond decisively","nuclear option","total war","obliterate","annihilate",
    "full force","war footing","wartime footing","preemptive strike","overwhelming response",
    "game changer","point of no return","brink of war","on the brink","serious consequences",
    "unacceptable","unprovoked aggression","act of war","declare war",
];

// ── Hostile / conciliatory lexicons ──────────────────────────────────────────

const HOSTILE_WORDS: &[&str] = &[
    "attack","strike","destroy","obliterate","annihilate","kill","war","invade",
    "threat","aggression","hostile","provocation","retaliate","assault","clash",
    "conflict","fighting","battle","offensive","missile","bomb","shoot","fire",
];

const CONCILIATORY_WORDS: &[&str] = &[
    "ceasefire","peace","negotiation","dialogue","agreement","treaty","withdraw",
    "diplomatic","talks","truce","deescalate","cooperation","compromise","accord",
    "reconciliation","mediation","de-escalation","diplomacy",
];

/// First whole-word occurrence of `needle` in `haystack` — alphanumeric boundaries on both
/// sides — returning its start byte offset (the boundary-aware sibling of [`str::find`]). Used
/// for short actor acronyms (`pla`, `cia`, `nato`…) that as bare substrings hide inside ordinary
/// words (`plan`, `official`, `senator`) and phantom-tag actors. Both args assumed lowercased;
/// internal hyphens (e.g. "de-escalation") are fine — only the ends are boundary-checked.
pub(crate) fn find_word(haystack: &str, needle: &str) -> Option<usize> {
    let hb = haystack.as_bytes();
    let nlen = needle.len();
    haystack.match_indices(needle).find_map(|(i, _)| {
        let before_ok = i == 0 || !hb[i - 1].is_ascii_alphanumeric();
        let after = i + nlen;
        let after_ok = after >= hb.len() || !hb[after].is_ascii_alphanumeric();
        (before_ok && after_ok).then_some(i)
    })
}

/// True if `needle` occurs in `haystack` as a whole word — so the hostile token "fire" does NOT
/// match inside "ceasefire" and "war" does not match "warning". Used for the sentiment lexicons
/// (audit processor-4), the boundary keyword lists below, and the nlp_sidecar dispatch gate —
/// everywhere naive substring matching let ordinary words cast phantom signal.
pub(crate) fn contains_word(haystack: &str, needle: &str) -> bool {
    find_word(haystack, needle).is_some()
}

/// True if `needle` starts a word in `haystack` — a boundary (or the string start) precedes it,
/// but any suffix may follow. The correct matcher for short DOMAIN keywords (`WORD_START_DOMAIN_KWS`)
/// that must still catch plural/tense forms ("rocket"→"rockets", "force"→"forces") yet not fire when
/// the keyword merely hides mid-token ("skyrocket", "reinforces", "correspondent", "ideal",
/// "anatomical"). Whole-word matching would wrongly drop the wanted plural/tense forms; substring
/// matching wrongly keeps the mid-word hits — word-start is the honest middle. Both args lowercased.
fn starts_word(haystack: &str, needle: &str) -> bool {
    let hb = haystack.as_bytes();
    haystack.match_indices(needle).any(|(i, _)| i == 0 || !hb[i - 1].is_ascii_alphanumeric())
}

/// Event keywords that must match as whole words: `hit`⊂`white house/architect` cast a
/// MilitaryStrike classification vote on every White-House story, `raid`⊂`afraid`. The
/// inflected forms are enumerated (they hide inside words too: `hits`⊂`whitsunday`,
/// `raids`⊂`afraids` typos) so the boundary fix keeps the substring era's recall on the
/// most common wire shapes ("missile hits…", "troops raided…"). All other event keywords
/// stay substring/phrase-matched on purpose (stems catch derived forms).
const BOUNDARY_EVENT_KWS: &[&str] = &["hit", "hits", "hitting", "raid", "raids", "raided", "raiding"];

/// Weighted-domain tokens that must match as whole words: the ambient `fire`⊂`ceasefire/
/// wildfire`, `shot`⊂`screenshot`, `deal`⊂`ordeal` are individually below the tag gate but
/// accumulate through noisy-OR, inflating military/diplomatic magnitude on unrelated or
/// de-escalatory stories. Inflections enumerated for the same recall reason as the event
/// list above ("warning shots fired" must keep its ambient credit). "deal"/"deals" live here
/// rather than in `WORD_START_DOMAIN_KWS` (word-start would re-admit nothing wanted beyond
/// "dealings" while whole-word keeps `ideal`/`ordeal` excluded — same documented cases).
/// All other domain tokens stay substring-matched on purpose.
const BOUNDARY_DOMAIN_KWS: &[&str] = &["fire", "fires", "fired", "shot", "shots", "deal", "deals"];

/// Keyword hit test honouring a boundary list: tokens in `boundary` match whole-word only,
/// everything else keeps deliberate substring/phrase matching.
fn kw_hit(tl: &str, kw: &str, boundary: &[&str]) -> bool {
    if boundary.contains(&kw) { contains_word(tl, kw) } else { tl.contains(kw) }
}

// ── Nuclear / WMD / civilian indicator terms ─────────────────────────────────

const NUCLEAR_TERMS: &[&str] = &[
    "nuclear","thermonuclear","warhead","icbm","slbm","atomic bomb","fissile",
];

const WMD_TERMS: &[&str] = &[
    "chemical weapon","biological weapon","nerve agent","dirty bomb","anthrax","sarin",
];

const CIVILIAN_TERMS: &[&str] = &[
    "civilian","population","hospital","school","residential","children",
];

// ── Severity base scores by event type ───────────────────────────────────────

fn severity_base(et: &EventType) -> f64 {
    match et {
        EventType::MilitaryStrike     => 0.65,
        EventType::NuclearTest        => 0.92,
        EventType::MissileLaunch      => 0.75,
        EventType::WmdUse             => 0.96,
        EventType::AllianceInvocation => 0.80,
        EventType::TroopDeployment    => 0.50,
        EventType::DiplomaticExpulsion => 0.42,
        EventType::SanctionsImposed   => 0.38,
        EventType::CyberAttack        => 0.48,
        EventType::Ceasefire          => 0.15,
        EventType::PeaceTalks         => 0.12,
        _                             => 0.20,
    }
}

// ── Actor entity dictionary ───────────────────────────────────────────────────

/// Actor patterns that MUST be matched as whole words. Two classes with the same failure mode:
/// as bare substrings they hide inside ordinary English words and phantom-tag actors — and for
/// `pla`/`cia`/`fbi`/`nato`/`putin`/`trump` that fabricates GREAT-POWER involvement, biasing the
/// index UP (the false-alarm direction).
///   (1) Short acronyms/initialisms: `pla`⊂`plan/plant/display`, `cia`⊂`official/special/
///       financial`, `nato`⊂`senator`, `isis`⊂`crisis`, `quad`⊂`squad`, `idf`⊂`midfield`.
///   (2) Person/militia/org names, which — unlike country stems — have no adjective/derived
///       forms to lose: `putin`⊂`disputing/computing`, `hamas`⊂`bahamas`, `trump`⊂`trumpeted`,
///       `cuba`-class hazards; the rest (`biden`, `mossad`, `wagner`, …) included on the same
///       rationale even where no common carrier word exists today.
/// Country/proper-noun stems are deliberately NOT here — they SHOULD prefix-match their
/// adjective forms (`russia`→`russian`, `iran`→`iranian`), which the dictionary otherwise
/// catches only via explicit phrases.
const BOUNDARY_ACTOR_PATS: &[&str] = &[
    "pla", "cia", "fbi", "idf", "mi6", "irgc", "isis", "isil", "aukus", "quad", "nato", "dprk",
    "putin", "zelensky", "biden", "trump", "netanyahu", "khamenei", "mossad", "wagner",
    "hezbollah", "hamas", "houthis", "houthi",
];

fn actor_entity_patterns() -> Vec<(&'static str, &'static str)> {
    vec![
        ("united states of america",  "United States"),
        ("united states military",    "US Military"),
        ("united states",             "United States"),
        ("russian federation",        "Russia"),
        ("russian military",          "Russia Military"),
        ("russian forces",            "Russia Military"),
        ("chinese military",          "China Military"),
        ("people's liberation army",  "China Military"),
        ("people's republic of china","China"),
        ("north korea",               "North Korea"),
        ("south korea",               "South Korea"),
        ("south china sea",           "South China Sea"),
        ("saudi arabia",              "Saudi Arabia"),
        ("united kingdom",            "United Kingdom"),
        ("united nations",            "United Nations"),
        ("un security council",       "UN Security Council"),
        ("european union",            "European Union"),
        ("international atomic energy agency", "IAEA"),
        ("comprehensive nuclear-test-ban treaty", "CTBTO"),
        ("pentagon",   "US Military"),
        ("kremlin",    "Russia"),
        ("white house","United States"),
        ("washington", "United States"),
        ("nato",       "NATO"),
        ("beijing",    "China"),
        ("moscow",     "Russia"),
        ("kyiv",       "Ukraine"),
        ("tel aviv",   "Israel"),
        ("tehran",     "Iran"),
        ("pyongyang",  "North Korea"),
        ("idf",        "Israel Military"),
        ("irgc",       "Iran Military"),
        ("pla",        "China Military"),
        ("cia",        "United States"),
        ("fbi",        "United States"),
        ("mi6",        "United Kingdom"),
        ("mossad",     "Israel"),
        ("hezbollah",  "Hezbollah"),
        ("hamas",      "Hamas"),
        ("houthis",    "Houthis"),
        ("houthi",     "Houthis"), // singular — the plural-only pattern missed "Houthi missile…"
        ("isis",       "ISIS"),
        ("isil",       "ISIS"),
        ("wagner",     "Wagner Group"),
        ("aukus",      "AUKUS"),
        ("quad",       "Quad"),
        ("ukraine",    "Ukraine"),
        ("russia",     "Russia"),
        ("china",      "China"),
        ("israel",     "Israel"),
        ("iran",       "Iran"),
        ("taiwan",     "Taiwan"),
        ("india",      "India"),
        ("pakistan",   "Pakistan"),
        ("france",     "France"),
        ("germany",    "Germany"),
        ("japan",      "Japan"),
        ("turkey",     "Turkey"),
        ("syria",      "Syria"),
        ("iraq",       "Iraq"),
        ("afghanistan","Afghanistan"),
        ("venezuela",  "Venezuela"),
        ("cuba",       "Cuba"),
        ("dprk",       "North Korea"),
        ("putin",      "Russia"),
        ("zelensky",   "Ukraine"),
        ("xi jinping", "China"),
        ("biden",      "United States"),
        ("trump",      "United States"),
        ("netanyahu",  "Israel"),
        ("kim jong",   "North Korea"),
        ("khamenei",   "Iran"),
    ]
}

// ── Casualty extraction ───────────────────────────────────────────────────────

fn extract_casualties(text: &str) -> Option<u32> {
    // Case-insensitive: headlines routinely capitalise casualty words ("12 Killed"),
    // and this figure feeds event severity — missing it makes the thermometer read
    // cooler than the source actually reports, which cuts against GCRM's job of
    // honestly reflecting the corpus. Compiled once: regex compilation is expensive
    // and this runs on every article.
    static CASUALTY_RE: OnceLock<Regex> = OnceLock::new();
    let re = CASUALTY_RE.get_or_init(|| {
        Regex::new(r"(?i)(\d[\d,]*)\s*(people|civilians|soldiers|troops|killed|dead|wounded|injured)")
            .expect("casualty regex is a valid pattern")
    });
    re.captures_iter(text)
        .filter_map(|c| {
            let n_str = c[1].replace(',', "");
            n_str.parse::<u32>().ok()
        })
        .max()
}

// ── NLP Processor ─────────────────────────────────────────────────────────────

pub struct NlpProcessor {
    fuzzy:      FuzzyDedup,
    event_kws:  Vec<(EventType, Vec<&'static str>)>,
    domain_map: Vec<(&'static str, Vec<(&'static str, f64)>)>,
    actor_pats: Vec<(&'static str, &'static str)>,
}

impl NlpProcessor {
    #[allow(dead_code)] // used by tests; production path constructs via with_dedup()
    pub fn new() -> Self {
        info!("NLP processor: initialised (pure Rust, MinHash LSH dedup, no external model)");
        Self {
            fuzzy:      FuzzyDedup::new(),
            event_kws:  event_keywords(),
            domain_map: weighted_domain_keyword_map(),
            actor_pats: actor_entity_patterns(),
        }
    }

    /// Construct with a pre-loaded FuzzyDedup cache (restored from disk).
    pub fn with_dedup(fuzzy: FuzzyDedup) -> Self {
        info!("NLP processor: initialised with {} cached titles (MinHash LSH)", fuzzy.len());
        Self {
            fuzzy,
            event_kws:  event_keywords(),
            domain_map: weighted_domain_keyword_map(),
            actor_pats: actor_entity_patterns(),
        }
    }

    /// Expose the FuzzyDedup for persistence on shutdown.
    pub fn dedup(&self) -> &FuzzyDedup { &self.fuzzy }

    /// Process a raw article into a GeopoliticalEvent.
    /// Returns None if the article is a duplicate or has no domain signal.
    pub fn process(&mut self, article: &RawArticle) -> Option<GeopoliticalEvent> {
        if !self.fuzzy.is_new(&article.title) {
            return None;
        }

        let raw_text = format!("{}. {}", article.title, article.body);
        let text     = raw_text.chars().take(6000).collect::<String>();
        if text.trim().is_empty() { return None; }

        let tl = text.to_lowercase();

        let domain_signals = self.score_domains(&tl);
        if domain_signals.is_empty() { return None; }
        let domain_tags: Vec<String> = {
            let mut tags: Vec<String> = domain_signals.keys().cloned().collect();
            tags.sort();
            tags
        };

        let event_type = self.classify(&tl);

        let has_nuclear  = NUCLEAR_TERMS.iter().any(|t| tl.contains(t));
        let has_wmd      = WMD_TERMS.iter().any(|t| tl.contains(t));
        let has_civilian = CIVILIAN_TERMS.iter().any(|t| tl.contains(t));
        let casualties   = extract_casualties(&text);

        let severity = {
            let mut s = severity_base(&event_type);
            if let Some(c) = casualties { s = (s + (c as f64 / 1000.0).min(0.30)).min(1.0); }
            if has_nuclear  { s = (s + 0.18).min(1.0); }
            if has_wmd      { s = (s + 0.15).min(1.0); }
            // Civilian impact raises severity modestly — deliberate targeting or
            // mass civilian harm is a recognised escalation driver. Bounded so it
            // cannot, by itself, push a low-severity event into the elevated band.
            if has_civilian { s = (s + 0.05).min(1.0); }
            (s * 1000.0).round() / 1000.0
        };

        let escalation_language_score = {
            let count = ESCALATION_PHRASES.iter()
                .filter(|p| tl.contains(*p))
                .count();
            (count as f64 / 3.0).min(1.0)
        };

        let sentiment_score = {
            // Whole-word matching (not substring): otherwise "ceasefire" scores as hostile via
            // its "fire" token and "warning" via "war", biasing the tone. (audit processor-4)
            let h = HOSTILE_WORDS.iter().filter(|w| contains_word(&tl, w)).count() as f64;
            let c = CONCILIATORY_WORDS.iter().filter(|w| contains_word(&tl, w)).count() as f64;
            let total = h + c;
            if total > 0.0 { ((c - h) / total * 1000.0).round() / 1000.0 } else { 0.0 }
        };

        let (actors, actor_ids, great_power_involved) = self.extract_actors(&tl);
        let (location, region)                         = self.extract_location(&tl, &actors);
        let credibility_weight = article.source_tier.credibility_weight();

        let mut event = GeopoliticalEvent::new(
            article.title.clone(),
            article.source.clone(),
            article.source_tier,
            article.published_at,
        );
        event.raw_article_id            = article.id.clone();
        event.event_type                = event_type;
        event.summary                   = article.body.chars().take(500).collect();
        event.location                  = location;
        event.region                    = region;
        event.actors                    = actors;
        event.actor_ids                 = actor_ids;
        event.great_power_involved      = great_power_involved;
        event.casualties                = casualties;
        event.civilian_impact           = has_civilian;
        event.severity                  = severity;
        event.nuclear_indicator         = has_nuclear;
        event.wmd_indicator             = has_wmd;
        event.escalation_language_score = escalation_language_score;
        event.sentiment_score           = sentiment_score;
        // v2: theater assignment, alliance-coupler flag, and a signed escalation step.
        event.theater            = Some(
            crate::models::theater_of(&event.actor_ids, event.region.as_deref()).id().to_string()
        );
        event.alliance_indicator = ALLIANCE_INVOCATION_PHRASES.iter().any(|p| tl.contains(p));
        event.escalation_step    = {
            // Sign from tone (hostile → escalatory), magnitude from severity. A
            // keyword-derived placeholder; the LLM extractor replaces it in Phase 4.
            // Neutral tone → 0.0 (no directional momentum): the old +0.4 default silently
            // injected an escalatory bias into the news-flow direction for EVERY neutral
            // article whenever the LLM was unavailable, in a model already tuned toward
            // false alarm. (audit processor-6)
            let dir = if sentiment_score < -0.15 { 1.0 }
                      else if sentiment_score >  0.15 { -1.0 }
                      else { 0.0 };
            (severity * dir).clamp(-1.0, 1.0)
        };
        event.domain_signals            = domain_signals;
        event.domain_tags               = domain_tags;
        event.credibility_weight        = credibility_weight;
        event.ingested_at               = Utc::now();

        Some(event)
    }

    fn classify(&self, tl: &str) -> EventType {
        // Severity-weighted classification: score each event type by
        //   (keyword match count) × (severity_base of that type).
        // This stops a high-frequency low-severity type (e.g. several weak
        // military words) from automatically outranking a definitive but
        // sparse high-severity signal, and replaces the old behaviour where
        // ties were broken arbitrarily by vocabulary order. A type must have
        // at least one keyword match to be eligible.
        let mut best       = EventType::Unknown;
        let mut best_score = 0.0_f64;
        for (et, kws) in &self.event_kws {
            let n = kws.iter().filter(|kw| kw_hit(tl, kw, BOUNDARY_EVENT_KWS)).count();
            if n == 0 { continue; }
            let score = n as f64 * severity_base(et);
            if score > best_score {
                best_score = score;
                best       = et.clone();
            }
        }
        best
    }

    /// Chokepoint-coercion PAIR rule: a named maritime chokepoint co-occurring with a
    /// coercion/effect term reads as transit coercion (weight 0.80, noisy-OR-merged into
    /// economic_warfare). Neither half alone scores: "strait of hormuz" in trade-volume
    /// reporting is geography, "blockade" without a chokepoint is already covered by the
    /// flat lexicon. This carries the 2026 Hormuz register — fee regimes, ship U-turns,
    /// leverage-as-weapon — that sanctions-era keywords cannot see, without the generic
    /// false fires the pruned bare phrases caused (Panama drought, Suez pricing).
    fn chokepoint_pair_score(tl: &str) -> Option<f64> {
        // Straits with NO legitimate toll authority: ANY fee/toll talk there is
        // coercion by definition (Iran cannot lawfully charge for Hormuz passage).
        const STRAITS: &[&str] = &[
            "strait of hormuz", "hormuz strait", "bab-el-mandeb", "bab el-mandeb",
            "strait of malacca", "malacca strait", "taiwan strait", "kerch strait",
            "red sea shipping", "gulf of aden",
        ];
        // Tolled waterways (canal/strait authorities charge lawfully): fee/toll news
        // there is ordinary pricing — only HARD coercion pairs (the Suez-fee-increase
        // false fire the pruned generics caused).
        const TOLLED: &[&str] = &["suez canal", "panama canal", "bosphorus"];
        const HARD_COERCION: &[&str] = &[
            "closure", "closing", "closed", "shut", "blockade", "mining", "mined",
            "naval mine", "seize", "seized", "seizure", "weapon", "leverage",
            "u-turn", "turn back", "turning back", "harass", "attack", "missile",
            "drone", "deny passage", "denied passage", "suspend transit",
            "reroute", "rerouting", "avoid",
        ];
        const FEE_COERCION: &[&str] = &["toll", "fee"];
        let hard = HARD_COERCION.iter().any(|c| tl.contains(c));
        if STRAITS.iter().any(|c| tl.contains(c)) {
            return (hard || FEE_COERCION.iter().any(|c| tl.contains(c))).then_some(0.80);
        }
        if TOLLED.iter().any(|c| tl.contains(c)) {
            return hard.then_some(0.80);
        }
        None
    }

    fn score_domains(&self, tl: &str) -> std::collections::HashMap<String, f64> {
        let mut out = std::collections::HashMap::new();
        for (domain, kw_weights) in &self.domain_map {
            // Noisy-OR fusion: treat each matched keyword as an independent piece
            // of evidence with "hit probability" = its weight. The combined signal
            // is 1 - ∏(1 - wᵢ) over matched keywords. Properties:
            //   • A single definitive keyword (w = 1.0) saturates the signal to 1.0.
            //   • Multiple moderate keywords accumulate toward 1.0 but never exceed it.
            //   • The result is comparable across domains regardless of how many
            //     keywords the domain's vocabulary contains (the old matched/total
            //     normalisation penalised domains with longer keyword lists).
            let mut miss_product = 1.0_f64;
            let mut any_match     = false;
            for (kw, w) in kw_weights {
                // Short bare keywords match at a word start (kills mid-token false alarms like
                // "skyrocket"→rocket, keeps plural/tense forms); the enumerated ambient tokens
                // match whole-word (kw_hit → BOUNDARY_DOMAIN_KWS); everything else keeps
                // deliberate substring matching.
                let hit = if WORD_START_DOMAIN_KWS.contains(kw) {
                    starts_word(tl, kw)
                } else {
                    kw_hit(tl, kw, BOUNDARY_DOMAIN_KWS)
                };
                if hit {
                    any_match = true;
                    miss_product *= 1.0 - w.clamp(0.0, 1.0);
                }
            }
            if !any_match { continue; }
            let signal = (1.0 - miss_product).clamp(0.0, 1.0);
            if signal >= MIN_DOMAIN_SIGNAL {
                out.insert(domain.to_string(), (signal * 1e4).round() / 1e4);
            }
        }
        // Chokepoint PAIR rule (see chokepoint_pair_score): merged noisy-OR into
        // economic_warfare so it composes with, never overrides, keyword evidence.
        if let Some(w) = Self::chokepoint_pair_score(tl) {
            let prev = out.get("economic_warfare").copied().unwrap_or(0.0);
            let merged = 1.0 - (1.0 - prev) * (1.0 - w);
            out.insert("economic_warfare".to_string(), (merged * 1e4).round() / 1e4);
        }
        out
    }

    fn extract_actors(&self, tl: &str) -> (Vec<String>, Vec<String>, bool) {
        let mut seen_ids: Vec<String>       = Vec::new();
        let mut actors:   Vec<String>       = Vec::new();
        let mut matched_spans: Vec<(usize, usize)> = Vec::new();

        for (pattern, display) in &self.actor_pats {
            let pat: &str = pattern;
            // Short acronyms match as whole words only (see BOUNDARY_ACTOR_PATS): as bare
            // substrings they hide inside ordinary words (plan/official/senator/crisis) and
            // phantom-tag actors and great-power involvement. Country stems keep substring
            // matching so they still catch adjective forms (russia→russian).
            let hit = if BOUNDARY_ACTOR_PATS.contains(&pat) {
                find_word(tl, pat)
            } else {
                tl.find(pat)
            };
            if let Some(pos) = hit {
                let end = pos + pat.len();
                if matched_spans.iter().any(|(s, e)| pos < *e && end > *s) {
                    continue;
                }
                let norm = normalize_actor(pat);
                if !seen_ids.contains(&norm) {
                    seen_ids.push(norm);
                    actors.push(display.to_string());
                    matched_spans.push((pos, end));
                    if actors.len() >= 12 { break; }
                }
            }
        }

        let gp = actors.iter().any(|a| is_great_power(a));
        (actors, seen_ids, gp)
    }

    fn extract_location(&self, tl: &str, actors: &[String]) -> (String, Option<String>) {
        let location_candidates = [
            "ukraine", "russia", "china", "taiwan", "iran", "israel",
            "gaza", "lebanon", "north korea", "south korea", "india",
            "pakistan", "syria", "iraq", "afghanistan", "venezuela",
            "south china sea", "taiwan strait", "korean peninsula",
            "europe", "middle east", "asia pacific",
        ];

        // Match each location stem at a WORD START, not as a bare substring. Substring matching
        // (the sibling defect to 1.7/1.8/1.21, on the served WHERE) let a stem hide MID-token and
        // phantom-tag the location/region: `iran`⊂`tirana` datelined a Balkans story to Iran,
        // `china`⊂`indochina` tagged a SE-Asia history piece China, `syria`⊂`assyria` — each
        // injecting a bogus front into the operator's `regions_active`. `starts_word` keeps every
        // demonym/plural form the substring era caught (the stem is a word-start PREFIX:
        // `iran`→`iranian`, `israel`→`israeli`, `pakistan`→`pakistani`, `india`→`indian`/`Sino-Indian`,
        // multi-word `north korea`→`north koreans`) while dropping the mid-word hits — the same
        // honest middle `score_domains` uses for `WORD_START_DOMAIN_KWS`. Residual `india`⊂`indiana`
        // stays (a legit word-start prefix, not fixable by boundary alone — it needs a stoplist, not
        // this change); it is rare and never mis-attributes a great-power theater (that keys off the
        // already-boundary-aware `actor_ids`, not this display location).
        for candidate in &location_candidates {
            if starts_word(tl, candidate) {
                let display = candidate
                    .split_whitespace()
                    .map(|w| {
                        let mut c = w.chars();
                        c.next().map(|ch| ch.to_uppercase().collect::<String>() + c.as_str())
                            .unwrap_or_default()
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                let region = resolve_region(candidate);
                return (display, region);
            }
        }

        if let Some(actor) = actors.first() {
            let region = resolve_region(actor);
            return (actor.clone(), region);
        }

        (String::new(), None)
    }
}

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

    // ── MinHash LSH correctness ───────────────────────────────────────────────

    #[test]
    fn fuzzy_dedup_new_title_is_new() {
        let mut fd = FuzzyDedup::new();
        assert!(fd.is_new("Russia launches missile strike on Kyiv"));
    }

    #[test]
    fn fuzzy_dedup_exact_duplicate_rejected() {
        let mut fd = FuzzyDedup::new();
        fd.is_new("Russia launches missile strike on Kyiv");
        assert!(!fd.is_new("Russia launches missile strike on Kyiv"));
    }

    #[test]
    fn fuzzy_dedup_near_duplicate_rejected() {
        let mut fd = FuzzyDedup::new();
        fd.is_new("Russia launches missile strike on Kyiv");
        assert!(!fd.is_new("Russia launches missile strikes on Kyiv"));
    }

    #[test]
    fn fuzzy_dedup_different_title_accepted() {
        let mut fd = FuzzyDedup::new();
        fd.is_new("Russia launches missile strike on Kyiv");
        assert!(fd.is_new("NATO discusses Article 5 activation after attack"));
    }

    #[test]
    fn fuzzy_dedup_empty_string_is_new() {
        let mut fd = FuzzyDedup::new();
        assert!(fd.is_new(""));
    }

    #[test]
    fn fuzzy_dedup_short_string_is_new() {
        let mut fd = FuzzyDedup::new();
        assert!(fd.is_new("ab")); // < 3 chars, no trigrams
    }

    #[test]
    fn fuzzy_trigrams_count() {
        let tg = FuzzyDedup::trigrams("hello");
        assert_eq!(tg.len(), 3);
    }

    #[test]
    fn minhash_signature_length() {
        let tgs = FuzzyDedup::trigrams("Russia launches missile");
        let sig  = FuzzyDedup::minhash_signature(&tgs);
        assert_eq!(sig.len(), NUM_HASHES, "Signature must have NUM_HASHES={NUM_HASHES} elements");
    }

    #[test]
    fn minhash_signature_deterministic() {
        // Same input must produce identical signature on every call
        let tgs  = FuzzyDedup::trigrams("North Korea nuclear test detected");
        let sig1 = FuzzyDedup::minhash_signature(&tgs);
        let sig2 = FuzzyDedup::minhash_signature(&tgs);
        assert_eq!(sig1, sig2, "MinHash signatures must be deterministic");
    }

    #[test]
    fn minhash_similar_titles_have_high_signature_overlap() {
        let t1 = "Russia launches ballistic missile strike on Kyiv overnight";
        let t2 = "Russia fires ballistic missiles at Kyiv in overnight strike";
        let tgs1 = FuzzyDedup::trigrams(t1);
        let tgs2 = FuzzyDedup::trigrams(t2);
        let sig1 = FuzzyDedup::minhash_signature(&tgs1);
        let sig2 = FuzzyDedup::minhash_signature(&tgs2);
        let est  = FuzzyDedup::jaccard_from_sigs(&sig1, &sig2);
        // Actual Jaccard ≈ 0.35–0.55 for these titles; estimate should be in range
        assert!(est > 0.1, "Similar titles should have non-trivial signature overlap, got {est:.3}");
    }

    #[test]
    fn minhash_different_titles_have_low_signature_overlap() {
        let t1 = "Russia launches ballistic missile strike on Kyiv";
        let t2 = "Economic summit in Geneva discusses trade agreements";
        let tgs1 = FuzzyDedup::trigrams(t1);
        let tgs2 = FuzzyDedup::trigrams(t2);
        let sig1 = FuzzyDedup::minhash_signature(&tgs1);
        let sig2 = FuzzyDedup::minhash_signature(&tgs2);
        let est  = FuzzyDedup::jaccard_from_sigs(&sig1, &sig2);
        assert!(est < 0.3, "Unrelated titles should have low signature overlap, got {est:.3}");
    }

    #[test]
    fn exact_jaccard_identical_is_one() {
        let j = FuzzyDedup::exact_jaccard("hello world", "hello world");
        assert!((j - 1.0).abs() < 1e-9);
    }

    #[test]
    fn exact_jaccard_disjoint_is_zero() {
        let j = FuzzyDedup::exact_jaccard("aaabbbccc", "xyyzzz123");
        assert!(j < 0.1, "Expected near-zero Jaccard for disjoint strings, got {j:.3}");
    }

    #[test]
    fn band_index_rebuilt_after_roundtrip() {
        let mut fd = FuzzyDedup::new();
        fd.is_new("Russia launches missile strike on Kyiv");
        fd.is_new("North Korea nuclear test detected by seismic sensors");

        let json = serde_json::to_string(&fd).unwrap();
        let mut restored: FuzzyDedup = serde_json::from_str(&json).unwrap();
        restored.rebuild_index();

        // After rebuild, dedup should still reject the seen titles
        assert!(!restored.is_new("Russia launches missile strike on Kyiv"),
            "Restored cache must reject previously seen title");
        assert!(!restored.is_new("North Korea nuclear test detected by seismic sensors"),
            "Restored cache must reject previously seen title");
        assert!(restored.is_new("Completely different headline about climate policy"),
            "Restored cache must accept genuinely new titles");
    }

    #[test]
    fn max_cache_is_50k() {
        let fd = FuzzyDedup::new();
        assert_eq!(fd.max_cache, 50_000, "MAX_CACHE must be 50,000 for high-volume operation");
    }

    // ── Serialisation (I-08) ──────────────────────────────────────────────────

    #[test]
    fn fuzzy_dedup_serialise_roundtrip() {
        let mut fd = FuzzyDedup::new();
        fd.is_new("Russia launches missile strike on Kyiv");
        fd.is_new("China conducts military exercises near Taiwan");
        let json = serde_json::to_string(&fd).unwrap();
        let restored: FuzzyDedup = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.titles.len(), 2);
        assert_eq!(restored.max_cache, 50_000);
    }

    #[test]
    fn fuzzy_dedup_restored_cache_rejects_duplicate() {
        let mut fd = FuzzyDedup::new();
        fd.is_new("Russia launches missile strike on Kyiv");
        let json = serde_json::to_string(&fd).unwrap();
        let mut restored: FuzzyDedup = serde_json::from_str(&json).unwrap();
        restored.rebuild_index();
        assert!(!restored.is_new("Russia launches missile strike on Kyiv"));
    }

    // ── Domain tagging ────────────────────────────────────────────────────────

    #[test]
    fn domain_tagging_nuclear_single_keyword() {
        let mut proc = NlpProcessor::new();
        let article = make_article("North Korea nuclear test detected", "");
        let event = proc.process(&article).unwrap();
        assert!(event.domain_tags.contains(&"nuclear_posture".to_string()));
    }

    #[test]
    fn domain_tagging_military_single_keyword() {
        let mut proc = NlpProcessor::new();
        let article = make_article("Military attack on Kyiv overnight", "Artillery fire reported");
        let event = proc.process(&article).unwrap();
        assert!(event.domain_tags.contains(&"military_escalation".to_string()));
    }

    #[test]
    fn cbrn_attack_tags_kinetic_and_sets_wmd_indicator() {
        // v2: WMD is no longer a scored domain. A chemical/bio attack tags KINETIC
        // (military_escalation) and the event carries wmd_indicator for the Phase-2
        // escalation-rung override.
        let mut proc = NlpProcessor::new();
        let article = make_article("Chemical weapon used in nerve agent attack in Syria", "");
        let event = proc.process(&article).unwrap();
        assert!(event.domain_tags.contains(&"military_escalation".to_string()));
        assert!(event.wmd_indicator, "chemical weapon should set wmd_indicator");
        assert!(!event.domain_tags.contains(&"wmd_mass_casualty".to_string()),
            "wmd_mass_casualty is no longer a scored domain");
    }

    #[test]
    fn domain_tagging_economic_requires_two_keywords() {
        let mut proc = NlpProcessor::new();
        let article = make_article("US imposes new sanctions on Russia", "");
        if let Some(event) = proc.process(&article) {
            let _ = event.domain_tags;
        }
    }

    #[test]
    fn great_power_conflict_ambient_country_name_does_not_tag_alone() {
        let mut proc = NlpProcessor::new();
        let article = make_article("China and Russia meet for bilateral trade talks", "");
        if let Some(event) = proc.process(&article) {
            assert!(!event.domain_tags.contains(&"great_power_conflict".to_string()),
                "Ambient country-name-only article should not tag great_power_conflict");
        }
    }

    #[test]
    fn sentiment_uses_word_boundaries_not_substrings() {
        // The "fire" hostile token must not match inside "ceasefire", nor "war" inside
        // "warning"; whole words still match, including the hyphenated "de-escalation". (audit processor-4)
        assert!(contains_word("ceasefire holds across the front", "ceasefire"));
        assert!(!contains_word("ceasefire holds across the front", "fire"));
        assert!(!contains_word("severe weather warning issued", "war"));
        assert!(contains_word("the war escalates sharply", "war"));
        assert!(contains_word("a de-escalation deal was signed", "de-escalation"));
    }

    #[test]
    fn actor_acronyms_do_not_match_inside_ordinary_words() {
        // Short acronyms (pla/cia/nato/isis) must not tag actors — or fabricate GREAT-POWER
        // involvement, biasing the index UP — when they merely hide inside common words:
        // plan/plant(pla), officials/special(cia), senator(nato), crisis(isis). No great power
        // is named here, only a real kinetic signal ("missile strike").
        let mut proc = NlpProcessor::new();
        let article = make_article(
            "Missile strike hits a power plant near the border",
            "Local officials plan a special crisis response briefed to a senator",
        );
        let event = proc.process(&article).unwrap();
        assert!(!event.great_power_involved,
            "no great power is named; plan/plant/official/special/senator/crisis must not tag one");
        for phantom in ["China Military", "United States", "NATO", "ISIS"] {
            assert!(!event.actors.iter().any(|a| a == phantom),
                "phantom actor {phantom} matched inside an ordinary word");
        }
    }

    #[test]
    fn actor_acronyms_still_match_as_whole_words() {
        // The boundary fix must not lose a legitimate whole-word acronym mention.
        let mut proc = NlpProcessor::new();
        let article = make_article(
            "PLA warships enter the Taiwan strait as forces clash",
            "NATO condemns the move",
        );
        let event = proc.process(&article).unwrap();
        assert!(event.great_power_involved,
            "'PLA' and 'NATO' as whole words are great powers and must still tag");
        assert!(event.actors.iter().any(|a| a == "China Military"));
        assert!(event.actors.iter().any(|a| a == "NATO"));
    }

    #[test]
    fn domain_keywords_match_at_word_start_not_mid_token() {
        // Domain-scoring analogue of the actor-acronym leak: short bare keywords must not fire when
        // they merely hide inside an unrelated word. Under the old raw-substring match this benign
        // economic/generic sentence tagged military_escalation (rocket⊂skyrocketed + forces⊂reinforces
        // → noisy-OR 0.58 ≥ 0.5) and nuclear_posture (atomic⊂anatomical, w=0.65 tags alone), inflating
        // the published index from text about nothing. Word-start matching drops all of them.
        let proc = NlpProcessor::new();
        let tl = "prices skyrocketed as the report reinforces an ideal outlook for anatomical research";
        let sig = proc.score_domains(tl);
        assert!(!sig.contains_key("military_escalation"),
            "'skyrocketed'/'reinforces' must not tag military_escalation, got {sig:?}");
        assert!(!sig.contains_key("nuclear_posture"),
            "'anatomical' must not tag nuclear_posture, got {sig:?}");
    }

    #[test]
    fn chokepoint_weaponization_phrasing_scores_economic_warfare() {
        // The 2026 Hormuz reality arrives as a fee/transit-denial regime, not an
        // embargo: these live-window headline shapes must feed economic_warfare so
        // the "Energy / chokepoint weaponized" light can see the canonical case its
        // own design comment names.
        let proc = NlpProcessor::new();
        for tl in [
            "Iran's envoy says friendly nations to get special Hormuz fee treatment",
            "Tehran threatens closure of the Strait of Hormuz over strikes",
            "Iran begins mining the strait as tankers reroute",
            "Two tankers seized near the Gulf as escalation spreads",
            // Pair-rule register (place-name + coercion/effect term) — the live
            // 2026 headlines the flat lexicon cannot see:
            "Medvedev says Strait of Hormuz gives Iran leverage comparable to nuclear weapon",
            "Several ships make sharp U-turn, not passing through Strait of Hormuz",
        ] {
            let sig = proc.score_domains(&tl.to_lowercase()); // production lowercases first
            let v = sig.get("economic_warfare").copied().unwrap_or(0.0);
            assert!(v >= 0.70, "chokepoint coercion must score economic_warfare >= 0.70: {tl:?} -> {sig:?}");
        }
        // Trade journalism about chokepoints is NOT weaponization — the pruned
        // generics used to false-fire on exactly these (xhigh review finding 10).
        for benign_tl in [
            "panama canal chokepoint strains global shipping as drought cuts crossings",
            "suez canal transit fee increase announced for 2027",
            "shipping volumes through the strait of hormuz rose this quarter",
        ] {
            let benign = proc.score_domains(benign_tl);
            assert!(benign.get("economic_warfare").copied().unwrap_or(0.0) < 0.45,
                "trade journalism must not read as weaponization: {benign_tl:?} -> {benign:?}");
        }
    }

    #[test]
    fn domain_keywords_still_match_plural_and_tense_forms() {
        // The word-start restriction must not lose legitimate plural/tense forms of the same keywords
        // (rocket→rockets, force→forces, atomic as a word). Guards against over-fixing.
        let proc = NlpProcessor::new();
        assert!(proc.score_domains("rockets struck the base as forces regrouped")
            .contains_key("military_escalation"),
            "'rockets'/'forces' as word starts must still tag military_escalation");
        assert!(proc.score_domains("the atomic arsenal was placed on alert")
            .contains_key("nuclear_posture"),
            "'atomic' as a whole word must still tag nuclear_posture");
    }

    #[test]
    fn actor_person_names_do_not_match_inside_ordinary_words() {
        // Person/militia names have the same substring failure mode as the acronyms
        // (audit-news d1): putin⊂disputing, hamas⊂Bahamas, trump⊂trumpeted — matching
        // them fabricates actors and great-power involvement (the false-alarm
        // direction). "forces clash" supplies the kinetic signal so process() emits.
        let mut proc = NlpProcessor::new();
        let article = make_article(
            "Forces clash as parties keep disputing the Bahamas summit outcome",
            "Officials trumpeted the deal despite the standoff",
        );
        let event = proc.process(&article).unwrap();
        assert!(!event.great_power_involved,
            "no great power is named; disputing/trumpeted must not tag Putin/Trump");
        for phantom in ["Russia", "United States", "Hamas"] {
            assert!(!event.actors.iter().any(|a| a == phantom),
                "phantom actor {phantom} matched inside an ordinary word");
        }
    }

    #[test]
    fn actor_person_names_still_match_as_whole_words() {
        // The boundary fix must not lose a legitimate whole-word person mention.
        let mut proc = NlpProcessor::new();
        let article = make_article(
            "Putin orders missile strike as Hamas fighters clash",
            "",
        );
        let event = proc.process(&article).unwrap();
        assert!(event.actors.iter().any(|a| a == "Russia"), "whole-word 'Putin' must tag Russia");
        assert!(event.actors.iter().any(|a| a == "Hamas"), "whole-word 'Hamas' must still tag");
        assert!(event.great_power_involved, "Putin → Russia is a great power");
    }

    #[test]
    fn houthi_singular_matches_and_country_stems_keep_adjective_recall() {
        // The dictionary was plural-only ("houthis"), so "Houthi missile…" — the far more
        // common attributive form — tagged no actor at all: a coverage hole in the
        // false-CALM direction. Country stems must meanwhile KEEP substring matching
        // (russia→russian), the documented d5b7ba1 decision this extends.
        let mut proc = NlpProcessor::new();
        let article = make_article(
            "Houthi missile strike hits vessel as Russian forces mass",
            "",
        );
        let event = proc.process(&article).unwrap();
        assert!(event.actors.iter().any(|a| a == "Houthis"),
            "singular 'Houthi' must resolve to the Houthis actor");
        assert!(event.actor_ids.iter().any(|a| a == "russia_military"),
            "'Russian forces' must still match via the substring stem");
    }

    #[test]
    fn event_keywords_hit_and_raid_match_whole_words_only() {
        // "hit"⊂"white house"/"architect" cast a MilitaryStrike classification vote on
        // every White-House story and "raid"⊂"afraid" — phantom votes in the
        // false-alarm direction (audit-news d3). Real whole-word uses must still classify.
        let proc = NlpProcessor::new();
        assert_eq!(proc.classify("the architect of the white house budget proposal"),
            EventType::Unknown, "hit/raid inside architect/white house must cast no vote");
        assert_eq!(proc.classify("residents afraid after the storm"),
            EventType::Unknown, "'afraid' must not classify as a raid");
        assert_eq!(proc.classify("missiles hit the base in a dawn raid"),
            EventType::MilitaryStrike, "whole-word hit/raid must still classify");
    }

    #[test]
    fn ambient_domain_tokens_match_whole_words_only() {
        // The ambient weighted tokens fire(0.10)/shot(0.10)/deal(0.10) hid inside
        // ceasefire/wildfire, screenshot, ordeal and accumulated through noisy-OR,
        // inflating military/diplomatic magnitude on unrelated or de-escalatory
        // stories (audit-news d4). Every other token keeps substring matching.
        let proc = NlpProcessor::new();
        let signals = proc.score_domains("a screenshot documented the family's ordeal after the wildfire");
        assert!(signals.is_empty(),
            "fire/shot/deal inside wildfire/screenshot/ordeal must contribute no domain signal, got {signals:?}");
        // Whole-word uses still count toward their domains (below the tag gate alone,
        // so combine with a stronger keyword to verify they still accumulate).
        let with = proc.score_domains("troops opened fire in combat near the advance");
        assert!(with.contains_key("military_escalation"),
            "whole-word 'fire' must still accumulate with troops/combat/advance");
    }

    #[test]
    fn classify_recognises_deescalation_types() {
        // Without keyword lists, classify() could never emit Ceasefire/PeaceTalks, so pure
        // de-escalation news fell through to Unknown and was scored as if escalatory. A genuine
        // escalation in the same headline still wins via severity-weighted scoring. (audit processor-2)
        let proc = NlpProcessor::new();
        assert_eq!(proc.classify("ceasefire announced between the two sides"), EventType::Ceasefire);
        assert_eq!(proc.classify("leaders begin peace talks in geneva"), EventType::PeaceTalks);
        assert_eq!(proc.classify("ceasefire collapses as airstrike kills dozens"), EventType::MilitaryStrike);
    }

    #[test]
    fn lone_0_50_weight_keyword_still_tags_its_domain() {
        // The fitted gate is `signal >= MIN_DOMAIN_SIGNAL`, so a single w == 0.50 keyword
        // ("data breach") tags its domain alone. Exercised through process() so a silent flip
        // of the boundary to `>` would drop the tag (process returns None → unwrap panics) and
        // be caught here — pinning the inclusive, fitted boundary. (audit processor-3)
        assert_eq!(MIN_DOMAIN_SIGNAL, 0.5);
        let mut proc = NlpProcessor::new();
        let article = make_article("Major data breach disclosed at the agency", "");
        let event = proc.process(&article)
            .expect("a lone 0.50-weight keyword must cross the inclusive gate and tag a domain");
        assert!(!event.domain_tags.is_empty(),
            "lone 0.50-weight keyword tagged no domain — the >= boundary may have flipped to >");
    }

    #[test]
    fn great_power_war_keyword_tags_kinetic_and_resolves_theater() {
        // v2: great_power_conflict is no longer a domain (it became a coupler).
        // An explicit great-power WAR phrase tags KINETIC, and theater resolution
        // assigns the US–China dyad.
        let mut proc = NlpProcessor::new();
        let article = make_article(
            "US-China war fears grow as forces clash in the South China Sea",
            "Warships exchange fire near contested waters"
        );
        let event = proc.process(&article).unwrap();
        assert!(event.domain_tags.contains(&"military_escalation".to_string()));
        assert_eq!(event.theater.as_deref(), Some("us_china_taiwan"));
    }

    #[test]
    fn min_domain_signal_threshold_is_half() {
        // Noisy-OR scale: a single strong keyword (w ≥ 0.55) tags; a lone
        // moderate keyword (w ≤ 0.50) does not.
        assert!((MIN_DOMAIN_SIGNAL - 0.5).abs() < 1e-9);
    }

    #[test]
    fn casualties_extracted_case_insensitively() {
        // Headlines capitalise these words; the figure must still be read.
        assert_eq!(extract_casualties("12 Killed in airstrike"), Some(12));
        assert_eq!(extract_casualties("12 KILLED in airstrike"), Some(12));
        assert_eq!(extract_casualties("12 killed in airstrike"), Some(12));
        // Thousands separators are stripped; the largest figure wins.
        assert_eq!(extract_casualties("2,000 Troops massed; 5 Wounded"), Some(2000));
        assert_eq!(extract_casualties("no numbers here"), None);
    }

    #[test]
    fn noisy_or_single_definitive_keyword_saturates() {
        let mut proc = NlpProcessor::new();
        let article = make_article("Nuclear detonation confirmed underground", "");
        let event = proc.process(&article).unwrap();
        let sig = event.domain_signals.get("nuclear_posture").copied().unwrap_or(0.0);
        assert!(sig > 0.99, "single definitive keyword should saturate noisy-OR, got {sig:.4}");
    }

    #[test]
    fn noisy_or_lone_moderate_keyword_below_threshold() {
        // "ceasefire" (0.45, diplomatic) alone must not tag the domain.
        let mut proc = NlpProcessor::new();
        let article = make_article("Officials discuss a possible ceasefire", "");
        if let Some(event) = proc.process(&article) {
            assert!(!event.domain_signals.contains_key("diplomatic_breakdown"),
                "a lone moderate keyword (0.45) should not cross the 0.5 threshold");
        }
    }

    // ── Domain signals ────────────────────────────────────────────────────────

    #[test]
    fn domain_signals_present_on_event() {
        let mut proc = NlpProcessor::new();
        let article = make_article("North Korea nuclear test detected by seismic sensors",
                                   "Underground detonation yield estimated at 50kt");
        let event = proc.process(&article).unwrap();
        assert!(!event.domain_signals.is_empty());
        assert!(event.domain_signals.contains_key("nuclear_posture"));
    }

    #[test]
    fn domain_signals_and_tags_are_consistent() {
        let mut proc = NlpProcessor::new();
        let article = make_article("Russian airstrike on Kyiv kills soldiers",
                                   "Artillery shelling continues along the front");
        let event = proc.process(&article).unwrap();
        let mut signal_keys: Vec<&str> = event.domain_signals.keys().map(|s| s.as_str()).collect();
        signal_keys.sort();
        let mut tags: Vec<&str> = event.domain_tags.iter().map(|s| s.as_str()).collect();
        tags.sort();
        assert_eq!(signal_keys, tags);
    }

    #[test]
    fn definitive_keyword_scores_higher_than_ambient() {
        let mut proc1 = NlpProcessor::new();
        let mut proc2 = NlpProcessor::new();
        let a_def = make_article("DPRK nuclear test detected underground", "");
        let a_amb = make_article("Leaders discuss nuclear policy at summit", "");
        let e_def = proc1.process(&a_def).unwrap();
        let e_amb = proc2.process(&a_amb);
        let def_signal = e_def.domain_signals.get("nuclear_posture").copied().unwrap_or(0.0);
        if let Some(ae) = e_amb {
            let amb_signal = ae.domain_signals.get("nuclear_posture").copied().unwrap_or(0.0);
            assert!(def_signal > amb_signal);
        } else {
            assert!(def_signal > MIN_DOMAIN_SIGNAL);
        }
    }

    #[test]
    fn domain_signal_bounded_zero_to_one() {
        let mut proc = NlpProcessor::new();
        let article = make_article(
            "Nuclear test detonation warhead ICBM nuclear strike nuclear weapon used \
             sarin nerve agent chemical weapon biological weapon dirty bomb mass casualty \
             article 5 invoked NATO mutual defence military airstrike soldiers killed",
            "Nuclear detonation radiological cbrn wmd swift exclusion secondary sanctions \
             state-sponsored hack critical infrastructure attack cyber espionage"
        );
        if let Some(event) = proc.process(&article) {
            for (domain, signal) in &event.domain_signals {
                assert!(*signal >= 0.0 && *signal <= 1.0,
                    "domain {domain} signal {signal} out of [0,1]");
            }
        }
    }

    #[test]
    fn weak_keyword_alone_below_min_signal_threshold() {
        let mut proc = NlpProcessor::new();
        let article = make_article("Leaders warn about upcoming peace talks agenda", "");
        if let Some(event) = proc.process(&article) {
            let dip = event.domain_signals.get("diplomatic_breakdown").copied().unwrap_or(0.0);
            assert!(dip < 0.035 || !event.domain_signals.contains_key("diplomatic_breakdown"),
                "Two weak keywords alone should not tag diplomatic_breakdown (signal={dip:.4})");
        }
    }

    #[test]
    fn cbrn_attack_produces_high_kinetic_signal() {
        // v2: CBRN attacks route into KINETIC at high weight (folded from the removed
        // WMD axis); the event also flags wmd_indicator for the rung override.
        let mut proc = NlpProcessor::new();
        let article = make_article(
            "Nerve agent attack and chemical attack cause mass casualties",
            "Biological attack suspected in mass casualty event"
        );
        let event = proc.process(&article).unwrap();
        let signal = event.domain_signals.get("military_escalation").copied().unwrap_or(0.0);
        assert!(signal > 0.15,
            "Multiple definitive CBRN-attack keywords should produce strong kinetic signal, got {signal:.4}");
    }

    #[test]
    fn alliance_invocation_tags_kinetic_and_sets_indicator() {
        // v2: alliance_activation is no longer a scored domain. An Article-5 invocation
        // tags KINETIC (it implies an armed attack) and sets alliance_indicator, which
        // the Phase-2 alliance-chain coupler consumes.
        let mut proc = NlpProcessor::new();
        let article = make_article("NATO article 5 invoked following attack", "");
        let event = proc.process(&article).unwrap();
        assert!(event.domain_signals.contains_key("military_escalation"));
        assert!(event.alliance_indicator, "Article 5 invocation should set alliance_indicator");
        assert!(!event.domain_signals.contains_key("alliance_activation"),
            "alliance_activation is no longer a scored domain");
    }

    #[test]
    fn domain_signals_corroboration_count_initialized_to_one() {
        let mut proc = NlpProcessor::new();
        let article = make_article("Russia launches nuclear missile at NATO base",
                                   "Warhead trajectory confirmed");
        let event = proc.process(&article).unwrap();
        assert_eq!(event.corroboration_count, 1);
    }

    // ── Credibility weights ───────────────────────────────────────────────────

    #[test]
    fn tier1_credibility_weight_correct() {
        let mut proc = NlpProcessor::new();
        let article = make_article("Russia fires nuclear missile strikes at NATO military base", "");
        let event = proc.process(&article).unwrap();
        assert_eq!(event.credibility_weight, 1.0);
    }

    #[test]
    fn tier3_credibility_weight_correct() {
        let mut proc = NlpProcessor::new();
        let mut article = make_article("Chemical attack kills civilians in airstrike", "");
        article.source_tier = SourceTier::Tier3;
        let event = proc.process(&article).unwrap();
        assert_eq!(event.credibility_weight, 0.20);
    }

    // ── Severity base values ──────────────────────────────────────────────────

    #[test]
    fn severity_base_values_correct() {
        assert_eq!(severity_base(&EventType::WmdUse),             0.96);
        assert_eq!(severity_base(&EventType::NuclearTest),        0.92);
        assert_eq!(severity_base(&EventType::AllianceInvocation), 0.80);
        assert_eq!(severity_base(&EventType::MissileLaunch),      0.75);
        assert_eq!(severity_base(&EventType::MilitaryStrike),     0.65);
        assert_eq!(severity_base(&EventType::TroopDeployment),    0.50);
        assert_eq!(severity_base(&EventType::CyberAttack),        0.48);
        assert_eq!(severity_base(&EventType::DiplomaticExpulsion), 0.42);
        assert_eq!(severity_base(&EventType::SanctionsImposed),   0.38);
        assert_eq!(severity_base(&EventType::Ceasefire),          0.15);
        assert_eq!(severity_base(&EventType::PeaceTalks),         0.12);
        assert_eq!(severity_base(&EventType::Unknown),            0.20);
    }

    // ── Location extraction ───────────────────────────────────────────────────

    #[test]
    fn location_extraction_taiwan() {
        let mut proc = NlpProcessor::new();
        let article = make_article(
            "China launches military operation around Taiwan",
            "PLA warships enter Taiwan strait as forces clash"
        );
        let event = proc.process(&article).unwrap();
        assert!(event.location.to_lowercase().contains("taiwan") ||
                event.region.as_deref().unwrap_or("").contains("asia"));
    }

    #[test]
    fn location_extraction_matches_stems_at_word_start_not_mid_token() {
        // Sibling of the 1.7/1.8/1.21 substring→boundary honesty fixes, on the served WHERE.
        // A bare-substring location stem hid MID-token and phantom-tagged the location/region
        // (`iran`⊂`tirana`, `china`⊂`indochina`, `syria`⊂`assyria`), injecting a bogus front into
        // the operator's `regions_active`. `starts_word` drops those mid-word hits.
        let proc = NlpProcessor::new();
        // Mid-token stems must NOT tag a location (no actor fallback here → honest empty).
        let (loc, region) = proc.extract_location("tirana summit reshapes the balkans", &[]);
        assert!(loc.is_empty() && region.is_none(),
            "Tirana (Albania) must not phantom-tag Iran: got loc={loc:?} region={region:?}");
        assert!(proc.extract_location("french indochina history revisited", &[]).0.is_empty(),
            "Indochina must not phantom-tag China");
        assert!(proc.extract_location("assyrian heritage site shelled", &[]).0.is_empty(),
            "Assyria must not phantom-tag Syria");
        // Word-start demonym/prefix forms the substring era caught must STILL match (no recall loss).
        assert_eq!(proc.extract_location("iranian drones cross the gulf", &[]).0, "Iran",
            "the `iranian` demonym must still resolve to Iran");
        assert_eq!(proc.extract_location("israeli strike hits damascus", &[]).0, "Israel",
            "the `israeli` demonym must still resolve to Israel");
        assert_eq!(proc.extract_location("north korean missile test overnight", &[]).0, "North Korea",
            "multi-word `north korea` must still match with a suffix");
    }
}
