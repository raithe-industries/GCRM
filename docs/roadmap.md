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
- [x] **1.1a current_2026 calibration gap** — **RESOLVED 2026-06-09 (Robert's call).** The
  −4.9pp gap was a STALE ANCHOR, not a model flaw. Mechanism analysis showed raising the model
  to the old 65% centre means lifting the breadth-saturation asymptote (~0.26→~0.34), which
  also pushes the *real live read* ~82%→~85-86% — eroding the off-the-0.90-peg headroom the
  2026-06-03 brink>breadth fix created (the saturation curve is monotonic, so no lever isolates
  current_2026's breadth-2 from the live read's breadth-3). So the centre was corrected 65→60
  to match the documented design intent; model untouched, zero peg risk. Brier 0.00060→~2e-6,
  RMSE 2.45pp→0.14pp, all four anchors within 0.2pp. **Do NOT re-raise current_2026 to 65%.**
- [x] **1.1b expose calibration evidence at runtime** — **DONE 2026-06-09.** `mod backtest` is
  no longer `#[cfg(test)]`; `calibration_evidence_html()` renders the live per-analog table +
  aggregate Brier/RMSE/in-band, substituted into the methodology page's `{{CALIBRATION_EVIDENCE}}`
  placeholder at startup (same mechanism as `{{BASE_PATH}}`). This also replaced the hand-written
  `~65%` calibration table that had itself gone stale — the readout is now computed from the
  running model and can't drift. Locked by `methodology_renders_live_calibration_evidence`.
- [ ] **1.2 Calibration-constant provenance** [candidate] — for each fitted constant
  (regime ×, P₀, breadth, coupler weights), ensure a one-line written rationale + the test
  that pins it exists near the definition. Where one is missing, add it. Never change a
  value without evidence + a test; this item is documentation/traceability, not tuning.
  - PROGRESS 2026-06-09: named the previously-magic `0.06` Stable-rung floor as
    `STABLE_HEAT_CEILING` (theater.rs, used in `rung_for` + the driver text) with a rationale,
    and added `quiet_theater_never_leaks_into_couplers` locking the honesty relationship that a
    Stable theater contributes ZERO to the concurrency / gp-entanglement / alliance amplifiers
    (ramp lower edge `HOT_HEAT−HOT_RAMP` and the `heat≥HOT_HEAT` gate both stay above the
    ceiling). Remaining un-pinned: regime ×, P₀, breadth asymptote, coupler weights — still open.
  - PROGRESS 2026-06-09: named the P(WWIII) forecast ceiling — previously a bare `.min(0.90)`
    literal in `bayesian.rs::compute` sitting next to STALE doc comments that still claimed 0.85 —
    as `models::FORECAST_PROB_CEILING = 0.90` with a rationale (epistemic humility, no ground
    truth). It is now the single source of truth: applied in the computation, fixed the stale 0.85
    comments to reference it, and rendered into the methodology page via a `{{FORECAST_PROB_CEILING}}`
    placeholder so the operator-facing prose can't drift (same anti-drift pattern as 1.1b). Locked by
    `forecast_prob_ceiling_is_the_named_honesty_clamp` (constant value + the clamp is LIVE, not
    vestigial + no real-engine world exceeds it) and `methodology_renders_forecast_ceiling_from_the_model_constant`.
  - PROGRESS 2026-06-10: named the **systemic coupler weights and the breadth asymptote** — the
    bulk of the remaining un-pinned fitted constants. The five bare literals in `theater.rs::compute`
    (`0.45` GP-entanglement weight, `0.30` alliance weight, `3.0` GP saturation, `0.26`/`1.7` breadth
    asymptote+e-fold, `0.70` brink amplifier) are now `COUPLING_GP_WEIGHT` / `COUPLING_ALLIANCE_WEIGHT`
    / `GP_ENTANGLEMENT_SATURATION` / `BREADTH_ASYMPTOTE` / `BREADTH_EFOLD` / `BRINK_AMPLIFIER`, each
    with a rationale comment. Crucially the 2026-06-03 design intent that prevents the worst
    dishonesty here — *breadth must never swamp the single-theater nuclear brink* (a regression that
    once pegged a no-brink four-theater world flat at the 0.90 ceiling) — was PROSE only; now locked by
    `breadth_never_swamps_the_nuclear_brink` (structural `BRINK_AMPLIFIER > BREADTH_ASYMPTOTE` + a
    live-engine bound proving the breadth amplifier stays strictly below `1+BREADTH_ASYMPTOTE`, hence
    below the brink). No value changed — calibration evidence identical (Brier ~2e-6, in-band 4/4).
    Remaining un-pinned: the guardrail-coupler magic in `bayesian.rs::compute` (the `/4.0`
    normalization and `0.12` guardrail amplifier) and the operator-tunable regime × factor defaults
    (already labeled `RegimeFactor`s in settings, not blind literals). P₀ is `BASELINE_ANNUAL`
    (named + const-asserted in models.rs).
  - PROGRESS 2026-06-10: named the **guardrail-collapse coupler** in `bayesian.rs::compute` — the
    two flagged bare literals are now `GUARDRAIL_REGIME_SPAN = 4.0` (the regime-multiplier excess
    above neutral at which collapse saturates: `1+SPAN = 5.0×` → guardrail 1.0) and
    `GUARDRAIL_AMPLIFIER = 0.12` (the max +12% lift full collapse adds to `l_sys`), each with a
    rationale, plus a pure `guardrail_from_regime()` helper. Honesty finding recorded: the seeded
    acute factor set already compounds to ~5.46×, so the LIVE coupler sits at FULL collapse (a design
    point of the current factors, NOT a knob to chase). Locked by two tests:
    `guardrail_coupler_is_a_bounded_soft_subordinate_amplifier` (the regime→guardrail map + the
    bounded `[1, 1+AMP]` soft amplifier) and `guardrail_collapse_is_live_in_compute_and_only_amplifies_the_likelihood`
    (two engines, same events, differing only in regime → l_sys scales by exactly `1+AMP·guardrail`,
    proving the coupler is live and touches only the likelihood, never the flat prior). No value
    changed — backtest 9/9, calibration evidence identical. Remaining un-pinned now: the `gp_bonus`
    `0.12` great-power scoring bonus (a DIFFERENT 0.12, in `score_all`) and the regime × factor
    defaults (config surface, labeled `RegimeFactor`s).
  - PROGRESS 2026-06-11: the flagged `gp_bonus` `0.12` in `score_all` turned out to be **DEAD CODE**,
    not merely un-named — a v1 vestige. It keyed on `domain == "great_power_conflict"`, but v2 removed
    that domain from `DOMAIN_WEIGHTS` (it became the `gp_entanglement` systemic coupler), and `score_all`
    `continue`s past any domain not in `DOMAIN_WEIGHTS`, so the branch could never fire — `gp_bonus` was
    provably always `0.0`. Worse, its comment actively claimed a per-domain great-power lift that the v2
    design deliberately abolished (the "one strike counted ~4×" collinearity). Removed the dead branch +
    stale comment (and fixed the adjacent stale "all 8 domains" → five-modalities comment), behavior-
    preserving (backtest 9/9, calibration evidence bit-identical). Locked the v2 honesty property by
    `great_power_involvement_does_not_add_a_per_domain_score_bonus`: identical events scored with
    `great_power_involved` true vs false produce byte-identical modality scores (GP enters ONLY via the
    coupler), while the display-only `great_power_event_count` still tracks the flag — so a future run
    can't "re-add" a per-domain GP bonus. The last flagged `compute`/`score_all` literal is now resolved;
    remaining un-pinned for 1.2: only the regime × factor defaults (config surface, labeled
    `RegimeFactor`s, not blind literals).
  - PROGRESS 2026-06-11: unified the **intra-theater co-occurrence elevation ramp** with the systemic
    one and locked the flagged "sub-threshold modality contributes 0 co-occurrence" invariant (the
    2026-06-09 entry's open sibling). `theater.rs` had its OWN `ELEV_RAMP` constant + inline smoothstep
    duplicating `bayesian::ELEVATION_RAMP`/`soft_elevation_weight`, with a comment claiming it "mirrors"
    the systemic ramp but nothing enforcing it — a drift hazard where "elevated" could come to mean two
    different things model-wide. Made `soft_elevation_weight` pub (the single source of truth for "how
    elevated, smoothly, is one modality") and used it in `score_theater`; removed the duplicate
    `ELEV_RAMP`. Behavior-preserving (both ramps were 0.08, identical formula — calibration evidence
    bit-identical). Locked by `intra_theater_co_occurrence_uses_the_shared_ramp_and_ignores_sub_threshold_modalities`.
- [x] **1.3 Coupler / theater cross-checks** — **DONE 2026-06-09.** Added 5 invariant tests in
  `src/theater.rs` that LOCK the model's core honesty properties, none of which were guarded
  before: bounded outputs over a 400-world deterministic fuzz (index ∈ [0,95], l_sys ≥ 0, heat
  ∈ [0,1], couplers in range); systemic-level monotonicity (a second hot theater never lowers
  the index, raises l_sys); intra-theater monotonicity (a superset of hot modalities never cools
  a theater or the index); de-escalation actually lowers index+l_sys; and the apex (nuclear-use)
  Systemic rung pegging the index at exactly FORECAST_INDEX_CEILING (95), never 100. These pin
  RELATIONSHIPS the model must always satisfy, not fitted magnitudes — they don't freeze the
  calibration. See improvement-log 2026-06-09.

## 2. Legibility — dashboard / UX  (grasp the state at a glance)
- [ ] **2.1 Small/short-viewport pass** [candidate] — the landing left rail must SCROLL
  rather than crush the methodology button off-screen; controls reachable on a laptop and a
  phone. Verify against `src/dashboard.html`; eyes will judge this at deploy.
- [x] **2.2 Annotation render audit** — **DONE 2026-06-10.** Audited every Chart.js instance
  (only two: timeline `tlChart`, domain bar `dmChart`) plus the methodology page (no charts).
  No annotation-plugin overlay remained — `calibBand` and `spikeMarks` were already the only
  overlays and both are canvas plugins. The audit's payoff: the domain bar chart had NO
  elevation reference, so an operator couldn't see at a glance which force domains had crossed
  the model's `ELEVATION_THRESHOLD` (the cutoff that feeds the co-occurrence amplifier). Added
  the `elevLine` canvas plugin — a dashed "elevated" line at the threshold, with its value
  templated from `models::ELEVATION_THRESHOLD` (`{{ELEVATION_THRESHOLD}}` server substitution,
  same anti-drift pattern as `{{BASE_PATH}}`/`{{FORECAST_PROB_CEILING}}`) so it can never drift
  from the engine. Canvas-drawn precisely because a naive `chartjs-plugin-annotation` line would
  be silently invisible under v4 — the exact failure this item guards. Locked by
  `dashboard_html_renders_elevation_threshold_from_model`. See improvement-log 2026-06-10.
- [x] **2.4 Critical-band reference lines on the timeline** — **DONE 2026-06-10.** The timeline
  (`tlChart`, annual P(WWIII) over time) had NO reference for the alert bands, so an operator
  couldn't see at a glance how close the live read was to "elevated"/"critical" — only the hero
  colour and the alert bar said so, after the fact. Added the `alertBands` canvas plugin: dashed
  amber "elevated" + red "critical" horizontal lines, each drawn only when its value falls inside
  the chart's auto-scaled y-range (hidden at a quiet ~1-2% read; they surface as risk climbs). The
  values are NOT hardcoded — each snapshot now carries `alert.elevated_threshold` /
  `alert.critical_threshold` (the engine's configured `AlertSettings`, recorded in
  `bayesian::compute` Step 10, serialized in `aggregator::snapshot_to_json`), and the dashboard
  adopts them live in `applyData`. This also killed the drift-prone hardcoded `.08`/`.025` literals
  in `pc()` (hero/rail risk colour) and the activity-log colour — they now read the live
  `ALERT_CRIT`/`ALERT_ELEV`. Canvas-drawn for the same reason as `elevLine`/`calibBand`/`spikeMarks`
  (chartjs-plugin-annotation renders nothing under v4). Locked by 3 tests. See improvement-log
  2026-06-10.
- [ ] **2.3 Methodology completeness** [candidate] — model internals (regime ×, P₀, GP,
  elevated) belong in the methodology view, NOT the landing rail (rail stays 30d/90d/
  last-computed). Keep methodology honest and current with the model as it evolves.
  - PROGRESS 2026-06-11: added the **Alert bands** section (`#alerts`) — the methodology
    previously documented the index/likelihood but never told the operator what P(WWIII)
    triggers the elevated/critical/30-day alert states. The three thresholds are TEMPLATED
    (`{{ALERT_ELEVATED}}`/`{{ALERT_CRITICAL}}`/`{{ALERT_30D}}`) from the engine's
    `AlertSettings` in `server.rs` (same anti-drift pattern as `{{FORECAST_PROB_CEILING}}`)
    — the same source the dashboard hero/timeline read live, so prose/colour/chart can't
    disagree. This is the 2.4-flagged sibling. Locked by
    `methodology_renders_alert_bands_from_alert_settings`. Remaining: regime ×/P₀/GP internals
    in the methodology view.
  - PROGRESS 2026-06-11: templated **P₀ (the baseline prior)** in the methodology. The
    baseline-prior section quoted the flat quiet-year prior as a HAND-TYPED `≈ 1.5%/yr` while
    the forecast ceiling right below it was already `{{FORECAST_PROB_CEILING}}` — a drift
    hazard (recalibrating `BASELINE_ANNUAL` would leave the whitepaper quoting a stale number).
    Now `{{BASELINE_ANNUAL_PCT}}`, substituted in `server.rs::ServerState::new` from
    `models::BASELINE_ANNUAL * 100` (same anti-drift pattern as the ceiling/alert bands), with
    a note that the value is rendered from the constant. Locked by
    `methodology_renders_baseline_prior_from_the_model_constant`. Remaining: regime ×/GP
    internals in the methodology view.

## 3. Awareness — theaters / feeds / map  (show where & why)
- [x] **3.1 Feed-liveness guard** — **DONE 2026-06-09.** Two `#[ignore]`d live-network
  tests in `src/ingestor.rs`: `feed_roster_liveness` probes EVERY RSS_FEEDS entry
  (HTTP 200 + feed-rs parse + ≥1 entry — the exact path `fetch_rss_feed` needs), with a
  concurrent first pass and a 30s-delayed serial retry so minute-scale edge blips don't
  read as dead; `search_api_liveness` probes GNews + GDELT (429 = alive: prod shares this
  IP). Run deliberately: `cargo test --release feed_roster_liveness -- --ignored
  --nocapture`. First audit immediately paid for itself: breakingdefense + nationalinterest
  were hard-403 dead (Cloudflare bot-fight) → replaced with defensescoop + lowy_interpreter
  (probed live, same niche/tier); cbc's cmlink endpoint was retired → moved to the
  canonical webfeed URL. 103/103 live. See improvement-log 2026-06-09.
- [ ] **3.2 GDELT** [candidate] — verify it is live, then wire it as an awareness layer.
  Do NOT add geo-less sources to the map (e.g. CISA KEV has no geo). Confirm live before
  committing a connector.
- [x] **3.3 Per-theater "why"** — **DONE 2026-06-09.** Each `TheaterState` now carries
  `top_driver`: the modality id with the largest WEIGHTED heat contribution (score ×
  domain_weight) — the model's own dominant term, empty for a Stable theater. Computed in
  `theater.rs::score_theater`, serialized in the snapshot, and surfaced in the theater-ladder
  chips (sub-line "X% heat · Nuclear" + tooltip "driven by …", reusing the dashboard
  `domainLabel`). Locked by `theater::top_driver_names_the_dominant_weighted_modality`.
  - PROGRESS 2026-06-10: added the **delta-driver** the original entry flagged. Each
    `TheaterState` now also carries `rising_driver`: the modality with the largest POSITIVE
    weighted change since the previous tick (computed from a new `TheaterEngine.prev_scores`
    history), populated only when the theater is rising. This answers *why a flashpoint is
    HEATING UP*, which `top_driver` (the dominant LEVEL) cannot — a theater can be hottest on
    nuclear posture yet rising because military escalation just jumped. Surfaced in the ladder
    chips as "↑ X" beside the rising arrow + in the tooltip ("rising on X"). Honest by
    construction (largest `Δscore × weight` term), no model constant touched. Locked by
    `theater::rising_driver_names_the_modality_that_moved_not_the_dominant_level`. Remaining
    awareness extension: a 2nd LEVEL contributor on the chip. See improvement-log 2026-06-10.
  - PROGRESS 2026-06-11: added the **2nd LEVEL contributor** the prior line flagged.
    `TheaterState.secondary_driver` = the second-largest WEIGHTED contributor among the
    modalities the model considers *elevated* (raw score ≥ `ELEVATION_THRESHOLD`) — the
    second active KIND of force, the co-occurrence story `top_driver` (one dominant level)
    cannot tell. Gated on elevation (the same cutoff that feeds the intra-theater
    co-occurrence amplifier), so it surfaces only when a flashpoint is genuinely
    multi-dimensional; empty otherwise. Surfaced on the ladder chip as "Nuclear + Military"
    (sub-line + "driven by …" tooltip), reusing `domainLabel`. Honest by construction, no
    model constant touched. Locked by
    `theater::secondary_driver_names_the_second_elevated_force_dimension`. See improvement-log
    2026-06-11.

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
- [x] **4.3 Shutdown responsiveness under backpressure** — **DONE 2026-06-10.** Confirmed the
  claim: the bare `sem.acquire_owned().await` lived *inside* the `select!` recv arm, so a
  saturated pool (all permits held by in-flight LLM calls) blocked that await and the `select!`
  could not poll the shutdown branch — a SIGTERM under sustained load stalled until a permit
  freed (one full classify, or indefinitely if Ollama hangs). Fixed by extracting
  `acquire_permit_or_shutdown` (races `acquire_owned()` against a clone of the shutdown watch,
  `biased` toward shutdown) so the dispatch wait is cancellation-aware; both graceful-exit paths
  now share `save_and_log_shutdown` (no drift). Locked by 4 tests, the key one
  (`permit_wait_cancels_on_shutdown_while_pool_saturated`) holding the only permit forever so a
  regression to a bare await would hang. See improvement-log 2026-06-10. **Do NOT** revert to a
  bare `acquire_owned().await` in the recv arm, and do not change `llm.concurrency` (see 4.1).

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
