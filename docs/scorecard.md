# GCRM Scorecard — the fitness function for self-improvement

A change is only "improvement" if it moves the mission forward — and the mission is to make the
operator SEE MORE (how close / where / why), not to accumulate tests or annotations. This file is
the fitness function. The self-improvement routine reads it at the start of every run and obeys the
prime directive below. The gates (`cargo build --release`, `cargo test`, and — at deploy time —
`deploy/eyes`) are the FLOOR; this scorecard is the direction.

## Prime directive
Rank every candidate change by its VALUE TIER (below), pick the **highest tier you can do well
today**, and prove it with that tier's **falsifiable gate** — NOT by how easily you can turn it
green. A run is a **success** only if it:
- advances the highest value tier it could do well today (T1 > T2 > T3), **and**
- passes that tier's objective gate, **and**
- regresses none of the **Hold** invariants.

"+N tests" or "one more display caveat" is **not, by itself, success.** Expanding the frontier means
expanding what the system can **SEE or COMPUTE** — a new live source, a new gauge computed from new
input, a new theater/coverage, an engine-behavior or calibration improvement, or a monitor/platform
rung — **never a new way to annotate a number already shown.**

A clean **no-op is acceptable** only as a bounded last resort (see Anti-incrementalism): when, after
honestly trying, no value-tier item can be implemented correctly and proven today. Never manufacture
cosmetic churn to look busy — a forced marginal commit against pillar-1 HONESTY is worse than an
honest no-op.

## Value tiers — rank by VALUE TO THE MISSION, not by provability
Claim a tier only by passing its GATE; declaring it is not enough. `raithe-watchdog.sh` audits the
claim out-of-band (it can reach the running services and the git diffs the cloud sandbox cannot).

**T1 — expands what the system can SEE or COMPUTE (do first).** Any one of:
- a **NEW live source** feeding a modality or the map. GATE: a new connector/parser + a checked-in
  REAL-RESPONSE fixture test + an `#[ignore]`d probe in `feed_roster_liveness` + a synthetic-scenario
  test where the source's presence CHANGES an output. NOT T1: an epsilon-weight dead input, or a 2nd
  feed for an already-ingested observable.
- a **NEW computed gauge**: a quantity newly computed FROM NEW INPUT, with units/purpose + a locking
  test. NOT T1: relocating/restyling an existing engine field (`coupling_driver`, `l_sys`,
  `concurrency`…) — that is T3 annotation.
- a **NEW theater / map coverage** the operator could not see before. GATE: new geographic/theater
  input wired into the read or map, locked by a test.
- an **engine-behavior or calibration improvement**. GATE: a written data/literature rationale + a
  test that FAILS when the change is `git stash`ed + the four anchors stay in-band. Constants on the
  honesty-firewall no-touch list are Robert-gated.
- a **monitor/platform rung** (Resource/Markets/Hotzones/Climate or the unified page). GATE: advances
  the next uncompleted Definition-of-Done rung; re-padding a completed rung does NOT count.

**T2 — first-time honesty surface for a genuinely UNSHOWN state (last resort before T3).** GATE:
grep-prove the state's flag/string is on ZERO existing operator surfaces, AND it is a DISTINCT
model/data condition implying a DISTINCT operator action — not a finer partition of an
already-surfaced state, not a relocation/restyle. **The blind/thin/stale/capped/held/saturated/pegged
caveat family is CLOSED — no further T2 credit from it.**

**T2 (retirement lane) — retiring a dead indicator/caveat.** Retiring an indicator/caveat that can no
longer fire, together with its lock tests and dashboard plumbing, is a creditable T2 honesty repair.
GATE: prove UNREACHABILITY (a test or measurement showing the trigger cannot occur — subjective
"no longer meaningful" does NOT qualify) + re-base the test-count floor DOWNWARD in the same log
entry. Deleting the lock tests of a removed feature is not "weakening a test". (First use: the
config-only "guardrails" board light, retired 2026-07-03.) Anything still REACHABLE — and any surface
whose dormancy is Robert-gated (e.g. `breadth_saturated`) — needs Robert's sign-off before retirement.

**T3 — annotation of an already-visible observable.** Mirror a caveat to an Nth surface, restyle,
relabel, methodology prose for something already shown. The I&W light BOARD (CLOSED at 12 lights of
ANY class — per-modality, coupler, velocity, physical alike; "guardrails" retired 2026-07-03; a new
light of any kind needs Robert's explicit sign-off) and the blind/thin/stale/capped/held/saturated/
pegged caveat family are **CLOSED: mining them is FORBIDDEN, not even T3.** Genuine T3 polish is allowed ONLY when no T1/T2 can be done well
today AND neither display-only cap (below) is breached.

Tie-break WITHIN a tier: prefer the least-recently-advanced axis and the weakest current frontier
(now: new signal & platform/monitors). A T1 you can do adequately beats a T3 you can do perfectly.
The tie-break is within-tier only — it can never justify a T3 when a T1/T2 gate is passable.

## Anti-incrementalism — stops indicator/caveat farming
- **Substantive = behavior-changing, proven by a fails-without-it test.** A streak-breaking run must
  land a new live source, an engine-behavior change locked by a test that FAILS when stashed, or a
  monitor/platform rung. A rename, a dead clamp that can never fire, a redundant assert, or a lone
  fixture is **streak-laundering** — it counts as display-only and the watchdog flags it.
- **Dual display-only cap:** at most **2 consecutive** AND at most **2 of any trailing 7** runs may
  be display-only. The cap-breaking run is T1/T2 or a structured NO-OP — never a 3rd caveat.
- **Closed veins are FORBIDDEN, not last-resort:** the I&W light board (closed at 12 lights of ANY
  class) and the blind/thin/stale/capped/held/saturated/pegged caveat family yield NO credit at any
  tier. A run that mines them FAILS.
- **NO-OP is bounded and costly:** at most **1 in any trailing 3** runs. A NO-OP must (a) name the
  specific T1 item, (b) decompose its next shippable step, (c) state the concrete blocker. A 2nd NO-OP
  in 3 is a FAIL that fires a Robert alert (`raithe-notify.sh`).
- **Frontier-debt escalation:** if the Live-sources / Map-layers / Monitors counts haven't moved in 7
  days, the next run MUST land a T1 source/monitor; at 14 days NO-OP is disallowed → escalate to Robert.

## Honesty firewall — the value push never lowers the honesty bar
No-touch without Robert: the four calibration anchor centres+bands (quiet / Ukraine /
current_2026 = 60% / Cuba), the saturating-breadth asymptote, `FORECAST_INDEX_CEILING = 95`, the 0.90
forecast ceiling, and the live `DOMAIN_WEIGHTS`. A new source ADDS a term with a pre-registered,
data/literature-grounded weight rationale; it may NOT retune an existing calibrated constant. Prove
the term works on a synthetic fixture — never force the live headline to move and never lift the
asymptote to make headroom (2026-06-09: that re-erodes the off-peg headroom). A wired source that
legitimately moves nothing is reported as "wired, low current influence," never tuned up. **Bold +
honest, never bold + fabricated. A prettier lie is worse than an ugly truth.**

## Forbidden (these are not "improvement")
- Cosmetic churn to look busy; renaming/reformatting with no behavioral or clarity gain.
- **Annotation inflation** — mirroring an existing caveat/flag/observable onto an Nth surface, or
  adding an Nth I&W light. The board is CLOSED at 12 lights of ANY class (per-modality, coupler,
  velocity, physical alike; the config-only "guardrails" light was retired 2026-07-03, do not re-add
  — a new light of any kind needs Robert's explicit sign-off) and the
  blind/thin/stale/capped/held/saturated/pegged caveat family is DECLARED CLOSED: mining them is
  forbidden.
- **Relabel-as-capability** — surfacing/relocating an EXISTING engine field (`coupling_driver`,
  `l_sys`, `concurrency`…) as a "new gauge" or "where/why". A gauge is T1 only if newly computed from
  NEW input.
- **Streak-laundering** — a no-op engine touch (rename, dead clamp, redundant assert, lone fixture)
  made to flip the `Touched` field and reset the consecutive-display-only counter.
- Fitting noise, hard-coding a desired number, or cosmetically reassuring.
- Raising `FORECAST_INDEX_CEILING` (95) or blind-tweaking calibration constants.
- Reverting a prior run's deliberate decision without reading `improvement-log.md` first.
- "Fixing" something you did not verify is actually broken in the **current** code. (The enricher was
  "reworked" three times in memory before someone checked it was already done.)

## Metrics (baselines as of 2026-06-09; test-count / calibration / feed-liveness baselines re-based 2026-07-03)

| Metric | Meaning | How to measure | Baseline | Direction |
|---|---|---|---|---|
| Build | release builds | `cargo build --release` | green | **Hold** |
| Tests green | full suite passes | `cargo test` | green | **Hold** |
| Test count | locks behavior — **NEVER evidence of improvement** | `cargo test --release` on the gcrm target (passed + ignored); the grep proxy `grep -rhoE '#\[(tokio::)?test\]' src \| wc -l` undercounts by missing `#[tokio::test(flavor…)]` forms | **536 passed + 3 ignored** (re-based 2026-07-03; incl. d2f616e's 2 landed mid-session) | **Floor (Hold)** — a run whose ONLY positive effect is +N tests is an automatic FAIL; the required lock test must FAIL when the change is `git stash`ed (attach that proof). The floor re-bases DOWNWARD in the same log entry when a retirement removes lock tests (see the T2 retirement lane) — that is not a regression |
| **Live signal sources** | new sight feeding the read | sources feeding a modality/map | (current roster) | **↑** — counts ONLY with a real-response fixture test + a `feed_roster_liveness` probe + a synthetic test that changes an output; epsilon-weight dead inputs do NOT count |
| **Map layers** | new geographic sight | map layers on `/api/map` | (current) | **↑** — counts ONLY when backed by a NEW live source not already on the map; slicing/relabeling an existing feed or a static table does NOT count |
| **Monitors shipped** | platform breadth | monitors at Definition-of-Done | 1 (GCRM) | **↑** — +1 ONLY at the DoD rung (≥1 live connector + honest headline from real input + ladder + where/why + uncertainty + renders under eyes); partial steps are progress, not +1 |
| Calibration bands | honesty floor | `cargo test backtest` (quiet/Ukraine/current/Cuba) | green | **Hold** |
| Calibration evidence | the number is earned | `cargo test calibration_evidence_report -- --nocapture` | **Brier 0.00092 / RMSE 3.04pp / in-band 4/4** (re-based after the 2026-06-28 de-saturation; the pre-de-saturation ~2e-6 fit is NOT a target — do not retune toward it) | **↓ Brier** (lower = better-fit; never regress upward without a documented live-targeted reason — and never "improve" it by retuning toward the superseded pre-de-saturation fit) |
| Index ceiling | no saturation theater | `grep FORECAST_INDEX_CEILING src/theater.rs` | 95 | **Hold** |
| 6h-trend contract | server-computed, not client | `cargo test trend_6h` + `epoch_store_trend_*` | green | **Hold** |
| Eyes (deploy-time) | renders + headline credible | `deploy/eyes/smoke.mjs` (local deploy only) | green | **Hold** |
| Feed liveness | every source live or replaced | `cargo test --release feed_roster_liveness -- --ignored --nocapture` (local only) | **103/103** (2026-07-03, after the globaltimes→cgtn / xinhua→ecns / rnz world→pacific swaps; roster locked at 103 in `src/ingestor.rs`) | **↑** / never ↓ |
| Enricher latency | classify p50 | live `journalctl` (GPU-bound, cap=2 by VRAM) | ~6s | informational — not a lever |

The cloud routine can verify everything except **Eyes**, **Enricher latency**, and any **live feed
leg** (no running service; the sandbox allowlists only GitHub+crates). Lower verifiability is NOT
lower value — a new live source outranks a fully-cloud-verifiable caveat. New-source work lands behind
the SAME gates everyone uses (offline parser test on a checked-in REAL-RESPONSE fixture + an
`#[ignore]`d live probe), and the **two-phase rule** applies: the cloud run ships
parser + fixture + ignored-probe and marks the roadmap item **STAGED**; only the local deploy
(`raithe-sync-deploy.sh`) + watchdog (`raithe-watchdog.sh`), which actually run the live leg, promote
**STAGED → DONE**. This prevents a dead feed from being declared live in a sandbox that cannot reach it.

## Recording
Every run that ships a change appends to `improvement-log.md`: date, axis, what changed, the metric it
moved (before→after), the green-proof — PLUS a disclosure line:
`- Tier: T1|T2|T3 · Touched: new-source|engine-behavior|calibration|display-only|noop · Lock-fails-without-change: yes/no (+proof) · Counts: <frontier metric/streak this run moved, if any>`
(`engine-behavior` requires a fails-without-it test; a no-op engine edit is `display-only` for streak
purposes.) Carry a running `consecutive_display_only` / `display_only_in_last_7` count in the newest
entry so the cap is auditable.

## Enforcement (outside the graded agent)
The improvement-log Tier/Touched claims are AUDITED by `raithe-watchdog.sh` (local, hourly — the cloud
sandbox cannot reach or edit it). It cross-checks the recent entries against their diffs and fires a
`raithe-notify.sh` alert when: a run tagged T1/T2 touched only `dashboard.html` for a string already
present elsewhere; a claimed new-source has no fixture test or no `feed_roster_liveness` probe; a
STAGED source failed its live leg; or the 2-of-7 display-only cap or the NO-OP cap is breached.
Self-graded tiers are advisory; **the watchdog audit is the gate.** That log + this scorecard are how
the daily runs compound into a trajectory instead of thrashing.
