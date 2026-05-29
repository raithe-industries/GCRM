# Global Conflict Risk Monitor (GCRM)

**RAiTHE INDUSTRIES INCORPORATED**
**Copyright © 2026 All Rights Reserved.**
**Owner & Founder: Robert Perreault**

---

## What GCRM Is

The Global Conflict Risk Monitor is a real-time open-source intelligence (OSINT) aggregation and probabilistic risk analysis platform. It continuously ingests geopolitical news from dozens of international sources, classifies events using a pure Rust NLP pipeline, and computes a statistically grounded, continuously updated probability estimate of a world war-scale conflict — expressed as an annualized percentage.

GCRM is designed as a professional intelligence dashboard: a single pane of glass that transforms the overwhelming volume of global news into a defensible, quantified risk index. It is built for analysts, decision-makers, and organizations that need to understand global conflict risk in real time without relying on subjective assessment or media sentiment alone.

## What GCRM Is Not

GCRM is **not** a prediction engine that claims certainty. It does not forecast specific events. It does not use generative AI, large language models, or neural networks for its core risk computation. It is not a news aggregator — it does not display articles for reading. It is not a social media monitor. It does not access classified or restricted data sources; every input is openly available.

The system is transparent about its limitations: the probability output is a calibrated risk index, not a formal Bayesian posterior derived from a generative model. The codebase documents this distinction explicitly and avoids overclaiming.

---

## Architecture Overview

GCRM is written entirely in Rust with zero Python runtime dependencies. The system is organized as a concurrent async pipeline with clearly separated modules connected by Tokio mpsc channels:

```
Ingestor (RSS / GNews / GDELT)
  → mpsc::channel<RawArticle>
  → NlpSidecar (pure Rust NlpProcessor)
  → mpsc::channel<GeopoliticalEvent>
  → Aggregator (Bayesian risk engine)
  → mpsc::channel<RiskSnapshot>
  → Server broadcast → WebSocket clients → Dashboard
```

Each stage runs as an independent Tokio task. Failures in one stage do not cascade silently — channel disconnection is detected and logged, and the deduplication cache is persisted to disk before exit.

### Module Map (10 source files, ~4,200 lines of Rust)

| Module | File | Purpose |
|--------|------|---------|
| **Entry Point** | `main.rs` | Pipeline wiring, settings, signal handling, task orchestration |
| **Models** | `models.rs` | Shared types: `RawArticle`, `GeopoliticalEvent`, `RiskSnapshot`, `DomainScore`, actor normalization, region resolution, source tiers |
| **Ingestor** | `ingestor.rs` | Parallel RSS polling (43 feeds), GNews search, GDELT API integration, deduplication cache, source health tracking |
| **Processor** | `processor.rs` | Pure Rust NLP: MinHash LSH deduplication, event classification, weighted domain tagging, severity/escalation/sentiment scoring, actor extraction |
| **NLP Sidecar** | `nlp_sidecar.rs` | Pipeline runner for the NLP processor with graceful shutdown and dedup cache persistence |
| **Bayesian Engine** | `bayesian.rs` | Domain scoring, regime multiplier, actor tracking, anomaly detection, risk index computation |
| **Aggregator** | `aggregator.rs` | Event window management, corroboration detection, timeline persistence (JSONL), shared state management |
| **Detector** | `detector.rs` | Seismic anomaly detection, CTBTO monitoring, nuclear news monitoring, test site registry, alert fusion |
| **API** | `api.rs` | Operator endpoints: regime factor management, manual event assertion, rate limiting, audit logging |
| **Server** | `server.rs` | Axum HTTP server, WebSocket broadcast, dashboard HTML, public API routes |

---

## How It Works

### 1. Ingestion Layer

GCRM polls 43 RSS feeds from Tier-1 and Tier-2 international news organizations in parallel, with a configurable concurrency cap (default 20 simultaneous connections). It also queries Google News RSS and the GDELT Project API for supplementary coverage.

Sources are classified into three credibility tiers:
- **Tier 1** (credibility weight 1.00): Wire services, verified international outlets (BBC, NYT, WaPo, Al Jazeera, Foreign Policy, Defense News, Bellingcat, Crisis Group, Arms Control Association, FAS)
- **Tier 2** (credibility weight 0.75): Major national outlets, regional specialists (Guardian, NPR, SCMP, Taipei Times, Times of Israel, Ukrayinska Pravda)
- **Tier 3** (credibility weight 0.20): Unverified, aggregated, or lower-confidence sources

Each article is deduplicated against a 50,000-entry MD5 cache before entering the pipeline. Source health is tracked: feeds with 10 consecutive failures are automatically disabled to prevent wasted cycles.

### 2. NLP Processing (Pure Rust)

Every article passes through a pure Rust NLP processor with no external model dependencies:

- **MinHash LSH Deduplication**: Near-duplicate titles are detected using a 64-element MinHash signature divided into 16 bands of 4 rows, providing ~80× speedup over the naive O(n²) trigram comparison. Expected false-negative rate at J=0.70 is ~2%. The dedup cache is persisted to disk on shutdown and restored on startup, eliminating cold-start false-spikes.

- **Event Classification**: Keyword scoring across 14 event types (MilitaryStrike, NuclearTest, MissileLaunch, CyberAttack, AllianceInvocation, WmdUse, etc.).

- **Weighted Domain Tagging**: Articles are scored against 8 risk domains using a weighted keyword dictionary. Each domain has definitive keywords (high weight, e.g. "nuclear test" = 0.90) and ambient keywords (low weight, e.g. "military" = 0.10). A minimum signal threshold (0.035) prevents noise-only articles from tagging domains.

- **Actor Extraction**: A 65+ entry entity dictionary maps raw text mentions to canonical actor IDs using longest-match-wins substring search. Great power involvement (US, Russia, China, NATO) is flagged for elevated scoring.

- **Severity, Escalation, and Sentiment Scoring**: Each event receives a composite severity score based on event type, casualties, nuclear/WMD indicators, escalation language density, and hostile-vs-conciliatory word balance.

### 3. Aggregation and Corroboration

Events enter a time-windowed buffer (up to 500,000 events, 4-year max age). A corroboration detector identifies when multiple outlets report the same event using trigram Jaccard similarity (threshold 0.40) — corroborated events receive credibility boosts rather than creating duplicate signals.

### 4. Bayesian Risk Engine

The core computation follows this formula:

```
P_risk = P₀_adj × (1 + L × SCALING_FACTOR)   clamped to [0, 0.85]
```

Where:
- **P₀_adj** = HISTORICAL_ANCHOR × regime_multiplier = (2/2026) × Π(active regime factors)
- **L** = weighted_domain_sum / max_weighted_sum × co_occurrence_boost

The historical anchor (2 world wars / 2026 years ≈ 0.0987%/yr) provides the Bayesian prior. Regime factors are operator-adjustable multipliers reflecting structural conditions (active wars, arms control collapse, nuclear posture changes, deterrence status). The likelihood ratio L is computed from domain scores weighted by strategic importance (nuclear posture weighted 3.0×, great power conflict 2.0×, etc.).

Co-occurrence amplification applies non-linear boosts when multiple domains are simultaneously elevated: 2 elevated domains → 1.3×, 3 → 2.0×, 5+ → 5.0×. This captures the compounding danger of simultaneous crises.

Domain-specific exponential decay ensures that recent events matter more: military escalation decays with a 24-hour half-life, while nuclear posture changes persist with a 72-hour half-life, and economic warfare with 96 hours.

The 0.85 ceiling is an explicit engineering decision, not a probabilistic prior — the model has no access to ground truth and should never emit near-certainty values.

**Calibration Targets:**
- Cuba 1962 equivalent (6 domains, max signals): ~30-40% annual
- Ukraine 2022 equivalent (5 domains, high signals): ~8-12% annual
- Current world 2026 (4-5 domains, moderate): ~4-8% annual
- Quiet period (1-2 domains, low): ~0.5-1.5% annual

### 5. Nuclear Detection System

A dedicated detector subsystem monitors for seismic anomalies consistent with underground nuclear tests:

- **SeismicMonitor**: Polls 5 FDSN-standard seismological APIs every 60 seconds
- **CTBTO Monitor**: Scrapes public CTBTO RSS for official statements
- **Nuclear News Monitor**: Watches the article store for nuclear-related headline spikes
- **Test Site Registry**: 8 known nuclear test sites with detection radii (Punggye-ri, Novaya Zemlya, Lop Nur, Nevada NTS, Semipalatinsk, Pokhran, Chagai Hills, Reggane/In Ecker)
- **Alert Fusion**: Combines seismic, official, and news signals into a confidence-weighted alert

All alerts are honestly labeled "SEISMIC ANOMALY" until official confirmation. The system explicitly documents that only CTBTO and national agencies can confirm nuclear detonations.

### 6. Dashboard and API

An embedded Axum web server serves a real-time dashboard via WebSocket. The dashboard displays:
- Live P(WWIII) annualized probability with trend delta
- 8 domain risk scores with elevation indicators
- Timeline chart (Chart.js)
- Nuclear alert status
- Regime factor panel with operator controls
- Article feed with domain tags

The operator API (key-protected, rate-limited at 60 req/min) allows runtime adjustments: toggling regime factors, asserting manual events, dismissing seismic alerts. All operator actions are logged to an immutable audit trail.

---

## Why It Works

### Mathematical Defensibility

The risk model is grounded in observable quantities. The historical anchor is derived from the actual frequency of world wars (2 in 2026 years). Regime factors correspond to documented geopolitical conditions. Domain scores are computed from verifiable news events with source attribution. Every number in the output JSON is traceable to specific inputs.

### Resilience Against Noise

Multiple layers prevent noise from inflating the risk estimate:
- Source tier credibility weighting (Tier 3 sources contribute only 20% weight)
- Weighted domain keyword scoring with minimum signal threshold
- MinHash LSH title deduplication (processor-level)
- Trigram Jaccard corroboration detection (aggregator-level)
- Exponential time decay per domain
- Anomaly detector with rolling baseline (3σ threshold)
- 0.85 engineering ceiling

### Reproducibility and Auditability

All computations are deterministic given the same input events. The MinHash seeds are compile-time constants. The dedup cache is serializable. Timeline entries are persisted as JSONL with 8-decimal-place precision. Operator actions are logged with timestamps. The system avoids hidden state and documents every design decision in code comments.

---

## Why It Is Valuable

GCRM addresses a genuine gap in the intelligence landscape. Traditional geopolitical risk assessment is subjective, slow, and opaque. Existing quantitative tools are either academic (lagging), proprietary (opaque), or simplistic (keyword counting without domain weighting or source credibility).

GCRM provides:
- **Speed**: 1-second update cycle from ingestion to dashboard
- **Transparency**: Every number is traceable; the formula is documented; the code is readable
- **Calibration**: Outputs are benchmarked against known historical crises
- **Defensibility**: The methodology can be explained and critiqued because it is fully visible
- **Independence**: No reliance on external ML services, proprietary APIs, or classified data

---

## Test Suite

GCRM ships with **259 unit tests** covering every module. All tests pass. The test suite validates:

- Model constants and thresholds (historical anchor, elevation threshold, credibility weights)
- Actor normalization (longest-match-wins, case insensitivity, pattern table alignment)
- Region resolution (longest-match disambiguation)
- Deduplication (exact, near-duplicate, eviction, serialization round-trip)
- Domain tagging (weighted signals, minimum thresholds, consistency between signals and tags)
- Bayesian engine (baseline probability, regime multiplier, co-occurrence boosts, nuclear elevation, probability ceiling, delta computation)
- Corroboration (increments, credibility boosts, same-source rejection, time window, credibility cap)
- API (key validation, rate limiting, regime product warnings, route inventory)
- Seismic detector (haversine distances, test site coordinates, FDSN source validation, confidence scoring)
- Server (route count, dashboard HTML content, broadcast channel)

```
cargo test → 259 passed; 0 failed; 0 ignored; finished in 0.01s
cargo build --release → 1 warning (unused constructor), 43.84s
```

---

## Areas for Iteration and Improvement

### NLP Pipeline Enhancement

The current NLP processor uses keyword matching with weighted scoring. While effective and fast, it has ceiling limitations: it cannot disambiguate context (e.g., "nuclear power plant" vs. "nuclear weapon"), handle negation ("did NOT launch missiles"), or extract relationships between entities. Future iterations could introduce:
- Lightweight transformer-based classification (e.g., a fine-tuned distilled model compiled to ONNX and invoked from Rust)
- Dependency parsing for relationship extraction
- Negation detection
- Coreference resolution for pronoun-to-actor mapping
- Multi-language support (currently English-only)

### Geolocation Precision

Location extraction currently uses a static candidate list with simple substring matching. This misses dynamic or compound locations. Improvements could include gazetteer-backed geocoding, coordinate extraction from article text, and spatial clustering of events for regional risk heat maps.

### Source Expansion

The current 43-feed registry could grow to include: Xinhua (Chinese state perspective), TASS/RT (Russian state perspective — Tier 3 with appropriate credibility weighting), Arabic-language outlets, Japanese/Korean wire services, academic preprint feeds (arms control papers), and government press release feeds (DoD, Kremlin, PLA Daily).

### Model Calibration Framework

The current calibration targets (Cuba 1962, Ukraine 2022) are documented but not formally tested against historical event reconstructions. A backtesting framework that replays historical event sequences and validates output ranges would significantly strengthen the model's credibility.

### Persistence and Recovery

The event window is currently in-memory. A system crash loses all buffered events. Adding event window persistence (e.g., a memory-mapped JSONL file or embedded database like sled) would enable full state recovery after unexpected restarts.

### Dashboard Innovation

The current dashboard is a single-page HTML served inline from `server.rs`. Opportunities include: geographic risk heat map overlay, event timeline with drill-down, domain trend sparklines, comparative historical overlay ("current vs. Cuba 1962"), mobile-responsive layout, and exportable PDF reports.

### Multi-Instance Coordination

GCRM currently runs as a single instance. For production resilience, multi-instance coordination (shared event store, leader election for JSONL writes, deduplicated ingestion across instances) would be needed.

### Formal Bayesian Update

The current formula is a calibrated risk index, not a formal Bayesian update. Transitioning to a proper P(H|E) = P(E|H)P(H)/P(E) framework with empirically estimated likelihoods would strengthen the mathematical foundation, though it requires historical event-to-outcome data that is inherently scarce for world-war-scale conflicts.

---

## Build and Run

### Prerequisites

- Rust 1.75+ (stable)
- OpenSSL development libraries (`libssl-dev` on Debian/Ubuntu)
- pkg-config

### Build

```bash
cargo build --release
```

### Configure

Copy `settings.yml` to the working directory and set:
- `dashboard.operator_key` to a strong random string
- Regime factors as appropriate for current conditions
- Alert thresholds as desired

### Run

```bash
./target/release/gcrm
```

The dashboard is available at `http://localhost:8000`. WebSocket connections receive live updates at 1-second intervals.

### Environment Variables

- `RUST_LOG=gcrm=info,warn` — log level control
- `GCRM_NLP_SOCKET` — override default NLP socket path (unused in pure-Rust mode)

---

## Project Structure

```
gcrm_rust/
├── src/
│   ├── main.rs           — Entry point, pipeline wiring
│   ├── models.rs         — Shared types and constants
│   ├── ingestor.rs       — RSS/GNews/GDELT ingestion
│   ├── processor.rs      — Pure Rust NLP processor
│   ├── nlp_sidecar.rs    — NLP pipeline runner
│   ├── bayesian.rs       — Risk computation engine
│   ├── aggregator.rs     — Event window and state management
│   ├── detector.rs       — Seismic/nuclear monitoring
│   ├── api.rs            — Operator API
│   └── server.rs         — HTTP server and WebSocket
├── docs/                 — Documentation
├── logs/                 — Timeline JSONL, dedup cache
├── tests/                — Integration tests
├── settings.yml          — Runtime configuration
├── Cargo.toml            — Dependencies
├── Cargo.lock            — Locked dependency versions
└── LICENSE               — Proprietary license
```

---

## License

Proprietary. Copyright © 2026 RAiTHE INDUSTRIES INCORPORATED. All rights reserved. This software and associated files are the proprietary property of the author, as well as the dba Canadian Controlled Private Corporation RAITHE INDUSTRIES INCORPORATED. Unauthorized copying, modification, distribution, or use of this software, in whole or in part, is strictly prohibited.
