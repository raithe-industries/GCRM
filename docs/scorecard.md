# GCRM Scorecard — the fitness function for self-improvement

A change is only "improvement" if it moves a real metric. This is that set of metrics.
The self-improvement routine reads this at the start of every run and obeys the prime
directive below. The gates (`cargo build --release`, `cargo test`, and — at deploy time —
`deploy/eyes`) are the FLOOR; this scorecard is the direction.

## Prime directive
A run is a **success** only if it does **at least one** of:
- moves a **Direction: ↑** metric the right way (with proof), or
- adds a genuinely new capability/test that expands the frontier (a new row here),

…**while regressing none** of the **Hold** invariants.

A clean **no-op is acceptable** when, after honestly trying, no roadmap item can be
implemented correctly and proven green today — then print `NO-OP: <honest reason>`. It should
be rare in a system this young, but never manufacture cosmetic churn to look busy: a forced,
marginal commit against pillar-1 HONESTY is worse than an honest no-op.

**Forbidden (these are not "improvement"):**
- Cosmetic churn to look busy; renaming/reformatting with no behavioral or clarity gain.
- Fitting noise, hard-coding a desired number, or cosmetically reassuring. A prettier lie
  is worse than an ugly truth.
- Raising `FORECAST_INDEX_CEILING` (95) or blind-tweaking calibration constants.
- Reverting a prior run's deliberate decision without reading `improvement-log.md` first.
- "Fixing" something you did not verify is actually broken in the **current** code. (The
  enricher was "reworked" three times in memory before someone checked it was already done.)

## Metrics (baselines as of 2026-06-09)

| Metric | Meaning | How to measure | Baseline | Direction |
|---|---|---|---|---|
| Build | release builds | `cargo build --release` | green | **Hold** |
| Tests green | full suite passes | `cargo test` | green | **Hold** |
| Test count | locked behavior | `grep -rhoE '#\[(tokio::)?test\]' src \| wc -l` | **389** (2026-06-14) | **↑** (never ↓ without deleting dead code + noting it) |
| Calibration bands | honesty floor | `cargo test backtest` (quiet/Ukraine/current/Cuba) | green | **Hold** |
| Calibration evidence | the number is earned | `cargo test calibration_evidence_report -- --nocapture` (Brier vs Robert's anchored band centres) | **Brier ~0.000002 / RMSE 0.14pp / in-band 4/4** (2026-06-09; all 4 anchors within 0.2pp) | **↓ Brier** (lower = better-fit; don't regress upward without a documented live-targeted reason) |
| Index ceiling | no saturation theater | `grep FORECAST_INDEX_CEILING src/theater.rs` | 95 | **Hold** |
| 6h-trend contract | server-computed, not client | `cargo test trend_6h` + `epoch_store_trend_*` | green | **Hold** |
| Eyes (deploy-time) | renders + headline credible | `deploy/eyes/smoke.mjs` (local deploy only) | green | **Hold** |
| Feed liveness | every source live or replaced | `cargo test --release feed_roster_liveness -- --ignored --nocapture` (live network — local only) | **102/103** (2026-06-09 audit; anadolu in a transient upstream 502 — same-day ingest 16:24Z, watch not replace) | **↑** / never ↓ |
| Enricher latency | classify p50 | live `journalctl` (GPU-bound, cap=2 by VRAM) | ~6s | informational — not a lever (see improvement-log 2026-06-09) |

The cloud routine can verify everything except **Eyes** and **Enricher latency** (no live
service). Eyes is enforced locally at deploy; treat dashboard edits as if eyes will judge
them (legible on small/short viewports, no broken/clipped/saturated render).

## Recording
Every run that ships a change appends to `improvement-log.md`: date, axis, what changed,
the **metric it moved** (before→after), and the green-proof. That log + this scorecard are
how the daily runs compound into a trajectory instead of thrashing.
