# GCRM Improvement Roadmap

The shared backlog the self-improvement routine pulls from. **Read this at the start of
every run.** Pick the highest-value UNCHECKED item you can do *well* today, implement it,
check it off, and append a proof entry to `improvement-log.md`. If you find a better lever,
ADD it here (don't silently drift). If an item turns out already-done or wrong, mark it and
move on — **verify the current code before assuming an item is still open.**

Items are tagged **[verified]** (confirmed real against the code) or **[candidate]** (a lead
worth investigating — confirm before acting). Axes are in mission-priority order; the axis a
run touches should rotate (read `improvement-log.md` to see which axis is least-recently
advanced and bias there, so coverage stays even).

The mission, against which every item is judged: give an operator ONE honest, legible,
real-time read on how close the world is to systemic / great-power war, and *where* it's
concentrating. **Honesty > Legibility > Awareness**, then the enablers.

---

## 1. Honesty — model / math / calibration  (the number must mean what it says)
- [x] **1.1 Calibration evidence harness** — **DONE 2026-06-09.** `src/backtest.rs` now
  scores the live model against Robert's anchored band CENTRES with proper scoring rules
  (Brier + cross-entropy), printed reproducibly via `cargo test calibration_evidence_report
  -- --nocapture` and locked by 3 tests. Baseline: **Brier 0.00060, RMSE 2.45pp, in-band
  4/4.** Deliberately evidence, not a tighter-than-band gate (that would fight legit
  live-targeted recalibration). See improvement-log 2026-06-09.
- [ ] **1.1a current_2026 calibration gap** [verified] — the evidence harness shows
  `current_2026` reads 60.1% vs its 65% centre (−4.9pp) — the model's weakest anchor and the
  sole driver of the RMSE (the other three are within 0.2pp). Known side-effect of the
  2026-06-03 saturating-breadth fix. Decide principledly whether the idealised current-full
  analog should sit at 65% or the centre should move; if the model should rise, find the
  defensible lever (NOT a blind constant tweak) and prove it by LOWERING Brier while keeping
  all bands + ordering green.
- [ ] **1.1b expose calibration evidence at runtime** [candidate] — the harness is test-only
  (`backtest` is `#[cfg(test)]`). Consider surfacing Brier/RMSE/in-band on the methodology
  view so calibration fitness is visible to an operator, not just in CI.
- [ ] **1.2 Calibration-constant provenance** [candidate] — for each fitted constant
  (regime ×, P₀, breadth, coupler weights), ensure a one-line written rationale + the test
  that pins it exists near the definition. Where one is missing, add it. Never change a
  value without evidence + a test; this item is documentation/traceability, not tuning.
- [ ] **1.3 Coupler / theater cross-checks** [candidate] — sanity invariants as tests:
  monotonicity (more escalation never lowers the index, all else equal), bounded outputs,
  de-escalation actually de-escalates. Each invariant you can prove → a new locked test.

## 2. Legibility — dashboard / UX  (grasp the state at a glance)
- [ ] **2.1 Small/short-viewport pass** [candidate] — the landing left rail must SCROLL
  rather than crush the methodology button off-screen; controls reachable on a laptop and a
  phone. Verify against `src/dashboard.html`; eyes will judge this at deploy.
- [ ] **2.2 Annotation render audit** [verified-lead] — `chartjs-plugin-annotation` renders
  nothing under Chart.js v4 (the v4 resolver swallows the annotations map); the calibration
  band uses an inline `calibBand` canvas plugin and the P(WWIII) spike arrows were recently
  moved to a canvas plugin for the same reason. Audit for any remaining annotation-based
  overlay that is silently invisible and port it to a canvas plugin.
- [ ] **2.3 Methodology completeness** [candidate] — model internals (regime ×, P₀, GP,
  elevated) belong in the methodology view, NOT the landing rail (rail stays 30d/90d/
  last-computed). Keep methodology honest and current with the model as it evolves.

## 3. Awareness — theaters / feeds / map  (show where & why)
- [ ] **3.1 Feed-liveness guard** [candidate] — every news source must be live or replaced,
  never left silently broken. A test/check that fails when a source stops parsing is high
  value (ties directly to the "feed roster must work" invariant). Count of live sources is
  a scorecard ↑ metric.
- [ ] **3.2 GDELT** [candidate] — verify it is live, then wire it as an awareness layer.
  Do NOT add geo-less sources to the map (e.g. CISA KEV has no geo). Confirm live before
  committing a connector.
- [ ] **3.3 Per-theater "why"** [candidate] — strengthen the where/why surfacing: the
  drivers behind each theater's contribution, legibly. Awareness is the third pillar and is
  the least-developed.

## 4. Robustness / performance  (enablers)
- [x] **4.1 LLM enricher: serial → bounded-concurrent worker pool** — **DONE (pre-2026-06-09).**
  `nlp_sidecar.rs` dispatches `classify()` to a `Semaphore`-gated `tokio::spawn` pool with
  `acquire_owned()` backpressure. `concurrency: 2` is a **deliberate GTX-1080 (8GB) VRAM
  calibration** — above 2, qwen2.5:7b's KV cache spills to CPU and *doubles* latency. **Do
  NOT "re-optimize" this or raise the cap.** (See improvement-log 2026-06-09.)
- [ ] **4.2 Risky `unwrap()/expect()` audit** [candidate] — find `unwrap()/expect()` on
  genuinely fallible runtime paths (network, parse, lock-poisoning) that could panic the
  service; convert to graceful handling. Skip the legitimately-infallible ones. Lock each
  fix with a test that exercises the error path.
- [ ] **4.3 Shutdown responsiveness under backpressure** [candidate] — in
  `nlp_sidecar.rs::run`, the permit `acquire_owned().await` happens *inside* the recv arm,
  so while the pool is saturated the `select!` can't poll the shutdown branch. Investigate
  whether shutdown latency under sustained load is acceptable; if not, make the dispatch
  cancellation-aware. Verify the claim before acting.

## 5. Toward v2  (the approved factored rebuild)
- [ ] **5.1** Sensible standalone steps toward theaters × orthogonal modalities × couplers,
  each shippable and test-locked on its own. See the v2 plan; don't land half-states.

---

## How to use / maintain this file
1. Read this + `improvement-log.md` + `scorecard.md` + recent `git log`.
2. Pick ONE unchecked item (highest mission value you can do well + prove today), biasing to
   the least-recently-touched axis. Re-verify it's still open against the code.
3. Implement; get `cargo build --release` + `cargo test` green; add/strengthen a test.
4. Check the box, append to `improvement-log.md` (what + metric moved + proof), commit, push.
5. If you discover a better item, add it under the right axis with a `[candidate]` tag and a
   one-line rationale. Keep this list honest and current — it's the spine of the program.
