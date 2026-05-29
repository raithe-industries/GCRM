# GCRM Engineering Audit Report

**Prepared for: Robert Perreault, Owner & Founder, RAiTHE INDUSTRIES INCORPORATED**
**Date: April 2, 2026**
**Scope: Full codebase review — 10 source modules, 259 unit tests, release build**
**Auditor: Engineering Agent (Claude, Anthropic)**

---

## Executive Summary

GCRM is a well-engineered, production-grade Rust application with strong test coverage, clean module separation, and honest self-documentation. The codebase compiles cleanly (1 warning), passes all 259 tests in 0.01 seconds, and builds a release binary in under 44 seconds. The architecture is sound, the risk model is mathematically coherent, and the code quality is significantly above average for a system of this complexity.

This audit identifies **no critical defects** that would prevent deployment. It identifies **7 high-priority items** that should be addressed before or shortly after production deployment, **12 medium-priority items** for engineering improvement, and **6 low-priority items** for long-term refinement.

---

## Section 1: Build Health

### 1.1 Compilation

The release build completes with a single warning:

```
warning: associated function `new` is never used
   --> src/processor.rs:889:12
```

`NlpProcessor::new()` is defined but only `NlpProcessor::with_dedup()` is called at runtime. The constructor exists for test convenience.

**Recommendation**: Either annotate with `#[allow(dead_code)]` with a comment explaining the test-only usage, or remove it and have tests use `with_dedup(FuzzyDedup::new())` directly.

### 1.2 Test Suite

259 tests, all passing, covering every module. Test execution time is 0.01 seconds — all tests are pure-logic unit tests with no I/O, network, or async runtime dependencies (with the exception of a few that use `tempfile`). This is excellent for CI/CD integration.

**Gap identified**: There are no integration tests that exercise the full pipeline (Ingestor → NLP → Aggregator → Server). The `tests/` directory exists but its contents were not provided for review. End-to-end tests with mock HTTP servers and synthetic articles would significantly increase confidence in pipeline integrity.

### 1.3 Dependencies

The dependency tree is reasonable (~170 crates). Key dependencies are well-chosen: Tokio for async, Axum for HTTP, reqwest for HTTP client, feed-rs for RSS parsing, chrono for timestamps, serde for serialization. No experimental or unmaintained crates were identified.

**Note**: `serde_yaml v0.9.34` is marked `+deprecated`. The maintainer recommends migrating to the `serde_yml` crate. This is not urgent but should be tracked.

---

## Section 2: High-Priority Findings

### H-01: Default Operator Key Is "CHANGE_ME_BEFORE_DEPLOY"

**Location**: `main.rs:109`

The hard-coded default operator key `"CHANGE_ME_BEFORE_DEPLOY"` will be active if no `settings.yml` is present. While `api.rs` does reject empty keys, this string is a non-empty valid key. An attacker who knows the default can toggle regime factors, assert manual events, and manipulate the risk output.

**Recommendation**: Either refuse to start if the operator key is the default value, or disable all operator endpoints when the default is detected. Log a prominent warning at startup.

### H-02: No TLS/HTTPS on Dashboard Server

**Location**: `server.rs`

The Axum server binds to `0.0.0.0:8000` with plain HTTP. WebSocket connections carry live risk data and the operator key is transmitted in headers over cleartext. In any network-accessible deployment, this exposes the system to interception.

**Recommendation**: Add native TLS support (via `axum-server` with `rustls`) or document that a reverse proxy (nginx, Caddy) with TLS termination is required for production deployment.

### H-03: `std::process::exit(1)` in NLP Sidecar

**Location**: `nlp_sidecar.rs:118, nlp_sidecar.rs:136`

When the raw article channel or event channel disconnects, the NLP sidecar calls `std::process::exit(1)` after saving the dedup cache. This is an ungraceful hard exit that bypasses Tokio's shutdown machinery, drops all other tasks without cleanup, and may leave the JSONL timeline file in a partially written state.

**Recommendation**: Replace `process::exit(1)` with returning from the task and signaling the main select loop to perform a coordinated shutdown. The dedup save should still happen, but other tasks (especially the aggregator's timeline writer) should also get a chance to flush.

### H-04: Unbounded Event Window Growth Under Sustained Load

**Location**: `aggregator.rs:419-422`

The event window has a 500,000 event cap, but the volume safeguard sorts and truncates the entire Vec every tick when the cap is exceeded. At 1Hz ticks and 500K events, this is an O(n log n) sort on every cycle during overload conditions.

**Recommendation**: Use a more efficient eviction strategy — a BinaryHeap or BTreeMap keyed by timestamp would provide O(log n) eviction. Alternatively, maintain a sorted insertion invariant so truncation is O(1) from the front.

### H-05: Regex Compilation on Every Call in `extract_casualties`

**Location**: `processor.rs:868`

`extract_casualties()` compiles a new `Regex` object on every invocation. Regex compilation is expensive (~microseconds). At 2,000 articles/hour, this is wasted work.

**Recommendation**: Use `lazy_static!` or `std::sync::OnceLock` to compile the regex once and reuse it.

### H-06: Lock Contention on Shared State Under High Throughput

**Location**: `aggregator.rs:315-339`

`AppState` uses `tokio::sync::Mutex` for all shared state (latest_snapshot, article_store, source_registry, nuclear_alerts, operator_events, shared_regime, epoch_store). The ingestor, NLP sidecar, aggregator, server, and detector all acquire these locks. Under high throughput, the article_store lock in particular is contended by both the ingestor (pushing) and the NLP sidecar (setting domain tags) and the server (querying).

**Recommendation**: Consider replacing the article_store Mutex with a `tokio::sync::RwLock` (readers don't block each other) or moving to a lock-free structure. The source_registry could use a `DashMap` (already a dependency, used in `api.rs`).

### H-07: No Authentication on Public API Routes

**Location**: `server.rs`

Routes like `/api/latest`, `/api/timeline`, `/api/epoch`, `/api/articles`, `/api/sources`, and `/api/nuclear` are completely unauthenticated. While operator routes require a key, the public routes expose the full risk assessment, article inventory, nuclear alert status, and source registry to anyone who can reach the server.

**Recommendation**: If GCRM is intended for restricted access, add at minimum a read-only API key or IP whitelist for public routes. If intentionally public, document this as a design decision.

---

## Section 3: Medium-Priority Findings

### M-01: Corroboration Uses O(n) Linear Scan

**Location**: `aggregator.rs:501-541`

`try_corroborate()` computes trigram Jaccard similarity against every event in the window within the 72-hour corroboration window. With a large window (tens of thousands of events), this becomes expensive. The processor's MinHash LSH optimization was applied to deduplication but not to corroboration.

**Recommendation**: Apply the same MinHash LSH band-index approach used in `processor.rs` to the corroboration detector. This would reduce corroboration cost from O(n) to O(k) where k is the candidate set size.

### M-02: FuzzyDedup Eviction Is O(n)

**Location**: `processor.rs:327-344`

`evict_oldest()` calls `Vec::remove(0)` which is O(n) due to element shifting, then rebuilds the entire band index. This happens when the cache exceeds 50,000 entries.

**Recommendation**: Use a `VecDeque` instead of `Vec` for `titles` and `sigs` to get O(1) front eviction. The band index rebuild can be incremental (remove old entry's bands, don't rebuild the entire index).

### M-03: `candidates.contains(&idx)` Is O(k) in Dedup Inner Loop

**Location**: `processor.rs:369`

Inside the MinHash LSH candidate collection loop, `candidates.contains(&idx)` performs a linear scan. For articles with many band collisions, this could degrade.

**Recommendation**: Use a `HashSet<usize>` for the candidate set instead of a `Vec<usize>`.

### M-04: Hardcoded `Utc::now()` Breaks Deterministic Testing

**Location**: Multiple modules (models.rs, bayesian.rs, aggregator.rs, processor.rs)

Many functions call `Utc::now()` internally, making them non-deterministic. While the test suite works around this by testing relative properties, this prevents exact output reproduction from the same input events.

**Recommendation**: Introduce a `Clock` trait or pass `now: DateTime<Utc>` as a parameter to functions that need the current time. This enables deterministic testing and event replay.

### M-05: No Graceful Degradation When All RSS Feeds Fail

**Location**: `ingestor.rs`

If all 43 feeds fail simultaneously (network outage, DNS failure), the ingestor continues cycling with zero articles. The aggregator continues ticking, and domain scores decay to zero over time. There is no alert or dashboard indication that ingestion has stopped.

**Recommendation**: Add an ingestion health metric (last successful fetch timestamp, articles/minute rate) exposed on `/api/health` and displayed on the dashboard. Alert when ingestion rate drops below a threshold.

### M-06: Dashboard HTML Is Inlined in server.rs

**Location**: `server.rs` (~700+ lines of HTML/CSS/JS as a string literal)

The entire dashboard is a single HTML string literal embedded in `server.rs`. This makes the file 71KB and makes dashboard iteration difficult.

**Recommendation**: Move the dashboard HTML to an external file loaded at startup (or embedded with `include_str!` for single-binary deployment). This separates frontend concerns from server logic.

### M-07: GDELT Body Extraction Is Minimal

**Location**: `ingestor.rs:571-576`

GDELT articles use the `seendate` field as the body text — essentially no body content. This means GDELT articles are classified using title-only NLP, which is significantly less accurate than title+body classification.

**Recommendation**: Implement GDELT V2 content API integration to retrieve article text, or use the article URL to fetch and extract the first paragraph from the source page.

### M-08: Actor Extraction Overlap Handling Is Fragile

**Location**: `processor.rs:1023-1046`

The actor extractor uses span overlap detection to avoid double-counting, but the pattern list order affects which actor is found first. The longest-match-wins normalization in `models.rs` is correct, but the extraction loop iterates in pattern list order and stops at the first non-overlapping match per position.

**Recommendation**: Sort actor patterns by length (longest first) before iteration, ensuring the most specific match is always preferred regardless of list order.

### M-09: No Rate Limiting on WebSocket Connections

**Location**: `server.rs`

Any client can open a WebSocket connection to `/ws` and receive live updates. There is no connection limit, no authentication, and no backpressure mechanism. A malicious actor could open thousands of connections to exhaust server resources.

**Recommendation**: Add a maximum WebSocket connection limit (e.g., 100) and consider requiring the operator key for WebSocket access.

### M-10: Timeline JSONL Files Grow Without Rotation Cleanup

**Location**: `aggregator.rs:39-41`

Timeline files are date-rotated (`timeline_YYYY-MM-DD.jsonl`), but old files are never cleaned up. At ~43MB/day, a month of operation produces ~1.3GB of timeline data.

**Recommendation**: Add a retention policy (e.g., keep 30 days, compress older files, or delete after N days).

### M-11: Epoch Store Boot Loader Only Reads Today and Yesterday

**Location**: `aggregator.rs:271-308`

The `load_epoch()` function only loads today's and yesterday's JSONL files. If the system was down for more than 1 day, the epoch store starts with a gap in history.

**Recommendation**: Scan the `logs/` directory for all `timeline_*.jsonl` files and load the most recent N entries up to the ring capacity (350K entries). This handles multi-day outages.

### M-12: `serde_yaml` Deprecation

**Location**: `Cargo.toml`

The `serde_yaml` crate used for settings parsing is deprecated. The maintainer has archived the repository.

**Recommendation**: Migrate to `serde_yml` or consider switching to TOML format (`serde_toml`), which is more idiomatic for Rust projects.

---

## Section 4: Low-Priority Findings

### L-01: Stub Tests in nlp_sidecar.rs

Five tests (`sidecar_wrapper_is_valid_python_skeleton`, etc.) are stubs that unconditionally assert `true`. These are documented as "retained for historical documentation" from a previous Python sidecar architecture.

**Recommendation**: Remove stub tests to keep the test suite clean and the 259 count meaningful.

### L-02: Duplicate Alias Entries in models.rs

Entries like `("zelensky", "ukraine")` and `("netanyahu", "israel")` appear twice in the ALIASES array — once in the country block and once in the leaders block. This is harmless (longest-match-wins produces the same result) but adds unnecessary table size.

**Recommendation**: Deduplicate the alias table.

### L-03: `approx` Crate in Dependencies

The `approx` crate is compiled but no `assert_abs_diff_eq!` or similar macros appear in the test suite. Tests use manual `(x - y).abs() < epsilon` comparisons.

**Recommendation**: Either use `approx` macros for cleaner test assertions, or remove the dependency.

### L-04: Magic Numbers in Bayesian Scoring

**Location**: `bayesian.rs:231-234`

The signal composition formula uses unexplained weight constants:
```rust
let signal = (event.severity * 0.55
    + event.escalation_language_score * 0.25
    + nlp_signal * 0.08
    + gp_bonus)
    .min(1.0);
```

The 0.55, 0.25, 0.08 weights and 0.12 great power bonus are not documented or configurable.

**Recommendation**: Extract these as named constants with documentation explaining the calibration rationale.

### L-05: `unsafe` Usage in Tests

**Location**: `nlp_sidecar.rs:292-295`

Two tests use `unsafe { std::env::set_var(...) }` and `unsafe { std::env::remove_var(...) }`. While harmless in single-threaded test contexts, these are marked unsafe in recent Rust editions because they are not thread-safe.

**Recommendation**: Use a test environment isolation approach or accept the unsafe annotation with a safety comment.

### L-06: No `cargo clippy` Evidence

The build log shows `cargo test` and `cargo build --release` but no `cargo clippy` run. Clippy would likely identify additional style improvements and potential issues.

**Recommendation**: Add `cargo clippy -- -D warnings` to the CI pipeline.

---

## Section 5: Architectural Strengths

This section documents what the codebase does well, for the record.

### S-01: Honest Self-Documentation

The codebase is exceptionally well-documented. Comments explain not just *what* the code does but *why* it does it, including design decisions, calibration rationale, and known limitations. The Bayesian engine's documentation explicitly states that its formula is a calibrated risk index, not a formal posterior — this intellectual honesty is rare and valuable.

### S-02: Clean Module Separation

Each module has a well-defined responsibility. The channel-based pipeline architecture means modules can be tested, replaced, or upgraded independently. The shared state is concentrated in `AppState` with clear ownership boundaries.

### S-03: Comprehensive Test Coverage

259 tests across all modules, with particular attention to edge cases: eviction behavior, cap enforcement, boundary conditions on thresholds, credibility caps, longest-match-wins correctness, and cross-module consistency (e.g., actor patterns in `processor.rs` must align with normalization in `models.rs`).

### S-04: Noise Resilience Layering

The system applies deduplication, credibility weighting, domain signal thresholds, corroboration, time decay, and anomaly detection in separate layers — each independently testable and tunable. This defense-in-depth approach is well-suited to the noisy nature of OSINT data.

### S-05: Graceful Degradation Design

Source health tracking, automatic feed disabling, exponential backoff for GDELT, dedup cache persistence, and the regime-only fallback mode (warning when no events are in the window) all demonstrate thoughtful consideration of failure modes.

### S-06: Performance-Conscious Implementation

The MinHash LSH upgrade (documented in processor.rs comments with complexity analysis), parallel RSS polling with semaphore-bounded concurrency, and the O(1) article store eviction demonstrate active performance engineering rather than premature optimization.

---

## Section 6: Summary of Recommendations by Priority

| ID | Priority | Summary | Effort |
|----|----------|---------|--------|
| H-01 | High | Reject default operator key at startup | Low |
| H-02 | High | Add TLS or document reverse proxy requirement | Medium |
| H-03 | High | Replace `process::exit(1)` with coordinated shutdown | Medium |
| H-04 | High | Optimize event window eviction under overload | Medium |
| H-05 | High | Cache compiled regex in `extract_casualties` | Low |
| H-06 | High | Reduce lock contention on shared state | Medium |
| H-07 | High | Add authentication to public API routes or document as intentional | Low |
| M-01 | Medium | Apply MinHash LSH to corroboration detector | High |
| M-02 | Medium | Use VecDeque for FuzzyDedup storage | Medium |
| M-03 | Medium | Use HashSet for dedup candidate collection | Low |
| M-04 | Medium | Inject clock for deterministic testing | High |
| M-05 | Medium | Add ingestion health monitoring and alerting | Medium |
| M-06 | Medium | Extract dashboard HTML from server.rs | Low |
| M-07 | Medium | Implement GDELT V2 content API | High |
| M-08 | Medium | Sort actor patterns by length before extraction | Low |
| M-09 | Medium | Add WebSocket connection limits | Low |
| M-10 | Medium | Add timeline file retention/cleanup | Low |
| M-11 | Medium | Expand epoch store boot loader to scan all files | Medium |
| M-12 | Medium | Migrate from deprecated serde_yaml | Low |
| L-01 | Low | Remove stub tests | Low |
| L-02 | Low | Deduplicate alias table entries | Low |
| L-03 | Low | Remove or use `approx` crate | Low |
| L-04 | Low | Extract and document signal weight constants | Low |
| L-05 | Low | Address unsafe env var usage in tests | Low |
| L-06 | Low | Add clippy to CI pipeline | Low |

---

## Conclusion

GCRM is a well-constructed system that reflects engineering rigor appropriate for professional deployment. The codebase is clean, well-tested, honestly documented, and architecturally sound. The high-priority items identified above are primarily hardening concerns (security, graceful shutdown, performance under load) rather than correctness defects — the core risk computation logic is solid.

The system is ready for supervised production deployment with the H-01 (default key rejection) and H-02 (TLS) findings addressed. The remaining items represent a natural engineering roadmap for hardening, performance optimization, and feature expansion.

**Overall Assessment: Production-ready with targeted hardening required.**

---

*End of audit report.*
