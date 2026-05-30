# Global Conflict Risk Monitor (GCRM)

**RAiTHE INDUSTRIES INCORPORATED** <br>
**Copyright © 2026 All Rights Reserved.** <br>
**Owner & Founder: Robert Perreault** <br>

**Live Service: [raithe.ca/risk](https://raithe.ca/risk)**

---

## What GCRM Is

The Global Conflict Risk Monitor is a real-time open-source intelligence (OSINT) aggregation and probabilistic risk analysis platform. It continuously ingests geopolitical news from dozens of international sources, classifies events using a pure Rust NLP pipeline, and computes a statistically grounded, continuously updated probability estimate of a world war-scale conflict — expressed as an annualized percentage.

GCRM is a professional intelligence dashboard: a single pane of glass that transforms the overwhelming volume of global news into a defensible, quantified risk index. It is built for analysts, researchers, and anyone who needs to understand global conflict risk in real time without relying on subjective assessment or media sentiment alone.

## What GCRM Is Not

GCRM is **not** a prediction engine that claims certainty. It does not forecast specific events. It does not use generative AI, large language models, or neural networks for its core risk computation. It is not a news aggregator — articles are shown as signal evidence, not for casual reading. It does not access classified or restricted data sources; every input is openly available.

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

Each stage runs as an independent Tokio task. Failures in one stage do not cascade silently — channel disconnection is detected and logged.

### Module Map

| Module | File | Purpose |
|--------|------|---------|
| **Entry Point** | `main.rs` | Pipeline wiring, settings, signal handling, task orchestration |
| **Models** | `models.rs` | Shared types: `RawArticle`, `GeopoliticalEvent`, `RiskSnapshot`, `DomainScore`, actor normalization, region resolution, source tiers |
| **Ingestor** | `ingestor.rs` | Parallel RSS polling (42 feeds, all simultaneous), GNews search, GDELT API integration, deduplication cache, source health tracking |
| **Processor** | `processor.rs` | Pure Rust NLP: MinHash LSH deduplication, event classification, weighted domain tagging, severity/escalation/sentiment scoring, actor extraction |
| **NLP Sidecar** | `nlp_sidecar.rs` | Pipeline runner for the NLP processor with graceful shutdown and dedup cache persistence |
| **Bayesian Engine** | `bayesian.rs` | Domain scoring, regime multiplier, actor tracking, anomaly detection, risk index computation |
| **Aggregator** | `aggregator.rs` | Event window management, corroboration detection, timeline persistence (JSONL), warmup gate, shared state management |
| **Detector** | `detector.rs` | Seismic anomaly detection, CTBTO monitoring, nuclear news monitoring, test site registry, alert fusion |
| **API** | `api.rs` | Operator endpoints: regime factor management, manual event assertion, rate limiting, audit logging, calibration timestamping |
| **Server** | `server.rs` | Axum HTTP server, WebSocket broadcast, dashboard HTML, public API routes, base-path routing |

---

## How It Works

### 1. Ingestion Layer

GCRM polls 42 RSS feeds from Tier-1 and Tier-2 international news organizations fully in parallel (all feeds fetched simultaneously, 8-second timeout). It also queries Google News RSS (every 12 seconds) and the GDELT Project API (every 20 seconds) for supplementary coverage.

Sources are classified into three credibility tiers:
- **Tier 1** (credibility weight 1.00): Wire services, verified international outlets (BBC, NYT, WaPo, Al Jazeera, Foreign Policy, Defense News, Bellingcat, Crisis Group, Arms Control Association, FAS)
- **Tier 2** (credibility weight 0.75): Major national outlets, regional specialists (Guardian, NPR, SCMP, Taipei Times, Times of Israel, Ukrayinska Pravda)
- **Tier 3** (credibility weight 0.20): Unverified, aggregated, or lower-confidence sources

Each article is deduplicated against a 50,000-entry MD5 cache before entering the pipeline. Source health is tracked: feeds with 10 consecutive failures are automatically disabled.

### 2. NLP Processing (Pure Rust)

Every article passes through a pure Rust NLP processor with no external model dependencies:

- **MinHash LSH Deduplication**: Near-duplicate titles are detected using a 64-element MinHash signature divided into 16 bands of 4 rows, providing ~80× speedup over naive O(n²) trigram comparison.
- **Event Classification**: Keyword scoring across 14 event types (MilitaryStrike, NuclearTest, MissileLaunch, CyberAttack, AllianceInvocation, WmdUse, etc.).
- **Weighted Domain Tagging**: Articles are scored against 8 risk domains using a weighted keyword dictionary. Definitive keywords carry high weight (e.g. "nuclear test" = 0.90); ambient keywords carry low weight (e.g. "military" = 0.10). A minimum signal threshold (0.035) prevents noise articles from tagging domains.
- **Actor Extraction**: A 65+ entry entity dictionary maps raw text mentions to canonical actor IDs using longest-match-wins substring search. Great-power involvement (US, Russia, China, NATO) is flagged for elevated scoring.
- **Severity, Escalation, and Sentiment Scoring**: Each event receives a composite severity score based on event type, casualties, nuclear/WMD indicators, escalation language density, and hostile-vs-conciliatory word balance.

### 3. Aggregation and Corroboration

Events enter a time-windowed buffer (up to 500,000 events, 4-year max age). A corroboration detector identifies when multiple outlets report the same event using trigram Jaccard similarity (threshold 0.40) — corroborated events receive credibility boosts rather than creating duplicate signals.

### 4. Bayesian Risk Engine

The core computation:

```
P_risk = P₀_adj × (1 + L × SCALING_FACTOR)   clamped to [0, 0.85]
```

Where:
- **P₀_adj** = HISTORICAL_ANCHOR × regime_multiplier = (2/2026) × Π(active regime factors)
- **L** = weighted_domain_sum / max_weighted_sum × co_occurrence_boost

The historical anchor (2 world wars / 2026 years ≈ 0.0987%/yr) provides the Bayesian prior. Regime factors are operator-adjustable multipliers reflecting structural conditions (active wars, arms control collapse, nuclear posture changes, deterrence status). The likelihood ratio L is computed from domain scores weighted by strategic importance (nuclear posture weighted 3.0×, great-power conflict 2.0×, etc.).

Co-occurrence amplification applies non-linear boosts when multiple domains are simultaneously elevated: 2 elevated → 1.3×, 3 → 2.0×, 5+ → 5.0×. This captures the compounding danger of simultaneous crises.

Domain-specific exponential decay ensures recent events matter more: military escalation decays with a 24-hour half-life, nuclear posture changes persist at 72 hours, economic warfare at 96 hours.

The 0.85 ceiling is an explicit engineering decision — the model has no access to ground truth and must never emit near-certainty values.

### 5. Warmup Gate

Timeline history is not recorded for the first 90 seconds after startup. This suppresses the non-stationary warmup transient (model starts from 0 events, first RSS batch lands, scores spike, then settle) from the public-facing historical chart. Live gauge and metrics are unaffected — only the chart record is gated, keeping displayed history clean and mathematically sound.

### 6. Nuclear Detection System

A dedicated detector subsystem monitors for seismic anomalies consistent with underground nuclear tests:

- **SeismicMonitor**: Polls 5 FDSN-standard seismological APIs every 60 seconds
- **CTBTO Monitor**: Scrapes public CTBTO RSS for official statements
- **Nuclear News Monitor**: Watches the article store for nuclear-related headline spikes
- **Test Site Registry**: 10 known nuclear test sites (Punggye-ri, Novaya Zemlya, Lop Nur, Nevada NTS, Semipalatinsk, Pokhran, Chagai Hills, Reggane/In Ecker, and others)
- **Alert Fusion**: Combines seismic, official, and news signals into a confidence-weighted alert

All alerts are honestly labeled "SEISMIC ANOMALY" until official confirmation.

### 7. Dashboard and API

An Axum web server serves a real-time dashboard via WebSocket. The dashboard is publicly accessible at **raithe.ca/risk** and displays:

- Live P(WWIII) annualized probability with trend delta
- 8 domain risk scores with elevation indicators
- Historical timeline chart (Chart.js, starts clean on restart)
- Nuclear alert status
- Article feed sorted by publication time, newest first
- Regime factor panel with operator controls (key-protected)
- Model calibration indicator — shows when an operator has updated model parameters

All timestamps on the dashboard are displayed in Eastern Time (ET / America/Toronto) with UTC shown as secondary reference.

The operator API (key-protected, rate-limited at 60 req/min) allows runtime adjustments: toggling regime factors, asserting manual events, dismissing seismic alerts. All operator actions are logged to an audit trail and surfaced to users via the model calibration indicator.

---

## Public API

The following endpoints are publicly accessible (no key required):

| Endpoint | Description |
|----------|-------------|
| `GET /risk/` | Live dashboard |
| `GET /risk/api/latest` | Current snapshot JSON |
| `GET /risk/api/timeline` | In-memory timeline (recent entries) |
| `GET /risk/api/epoch` | Full timeline with `?limit=N` and `?since=<rfc3339>` |
| `GET /risk/api/articles` | Article store, newest first, with `?limit=N&source=X&domain=Y` |
| `GET /risk/api/sources` | Feed registry and delivery counts |
| `GET /risk/api/nuclear` | Seismic alert status |
| `GET /risk/api/health` | Process health check |
| `WS  /risk/ws` | WebSocket: live snapshots + article updates |

---

## Deployment

GCRM runs as a systemd service on the host server, proxied through a Cloudflare Tunnel. The tunnel handles TLS termination; the service itself binds to `localhost:8000`.

### Service

```
/etc/systemd/system/gcrm.service
WorkingDirectory=/home/st0n3/Desktop/GCRM
ExecStart=target/release/gcrm
Restart=always
```

### Configuration (`settings.yml`)

Key settings:
- `dashboard.host` / `dashboard.port` — bind address (default `0.0.0.0:8000`)
- `dashboard.base_path` — URL prefix for subpath serving (set to `/risk` for raithe.ca/risk)
- `dashboard.operator_key` — required for all operator API endpoints; must be a strong random string
- `alerts.elevated` / `alerts.critical` — P(WWIII) thresholds for alert banners
- `regime_factors` — structural multipliers reflecting current geopolitical conditions

### Build

```bash
cargo build --release
sudo systemctl restart gcrm.service
```

### Environment

- `RUST_LOG=gcrm=info,warn` — log level control

---

## Test Suite

GCRM ships with **284 unit tests** covering every module. All tests pass.

```
cargo test → 284 passed; 0 failed; 0 ignored
```

Tests cover: model constants, actor normalization, region resolution, deduplication, domain tagging, Bayesian engine, corroboration, API key validation and rate limiting, seismic detector geometry, server routes and dashboard HTML content.

---

## Project Structure

```
GCRM/
├── src/
│   ├── main.rs           — Entry point, pipeline wiring
│   ├── models.rs         — Shared types and constants
│   ├── ingestor.rs       — RSS/GNews/GDELT ingestion
│   ├── processor.rs      — Pure Rust NLP processor
│   ├── nlp_sidecar.rs    — NLP pipeline runner
│   ├── bayesian.rs       — Risk computation engine
│   ├── aggregator.rs     — Event window, timeline, warmup gate
│   ├── detector.rs       — Seismic/nuclear monitoring
│   ├── api.rs            — Operator API
│   └── server.rs         — HTTP server, WebSocket, dashboard
├── docs/                 — Documentation
├── logs/                 — Runtime timeline JSONL (gitignored)
├── settings.yml          — Runtime configuration
├── Cargo.toml            — Dependencies
├── Cargo.lock            — Locked dependency versions
└── LICENSE               — Proprietary license
```

---

## License

Proprietary. All rights reserved. Copyright © 2026 RAiTHE INDUSTRIES INCORPORATED. 
<br>This software and associated files are the proprietary property of Robert Perreault and RAiTHE INDUSTRIES INCORPORATED. Unauthorized copying, modification, distribution, or use of this software, in whole or in part, is strictly prohibited.
