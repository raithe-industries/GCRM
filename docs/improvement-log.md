# GCRM Improvement Log

Append-only record of what each self-improvement run changed, the scorecard metric it moved,
and the green-proof. This is the anti-thrash memory: **read it before you act** so you build
on prior runs instead of repeating or reverting them. Newest entries at the top.

Format per entry:
```
## YYYY-MM-DD — <axis> — <one-line what>
- Item: <roadmap id or "ad-hoc">
- Change: <what changed and why it advances honesty/legibility/awareness>
- Metric moved: <metric: before → after>  (or: invariant held / new test added)
- Proof: <cargo test summary / counts / key output>
- Notes / decisions future runs must respect: <…>
```

---

## 2026-06-12 — awareness — systemic "why": name the dominant coupling amplifier (what turns a regional crisis into a world-war risk) (roadmap 3.4)
- Item: roadmap 3.4 (new, now checked). Awareness axis (pillar 3, "show WHERE and WHY, not just HOW
  MUCH"). Axis rotation: reading the log, awareness was the least-recently-advanced axis — its last real
  advance was 2026-06-11 (secondary_driver), while honesty/legibility/robustness all advanced 2026-06-12.
  The two open awareness items (3.2 GDELT) need live-network verification the cloud sandbox lacks, so I
  added a provable-green awareness capability at the SYSTEMIC level instead of the theater level the recent
  3.3 extensions covered.
- Verified-open-first (read the running surfaces): the dashboard's `systemic.driver` names the hottest
  theater + rung + hot-count ("Ukraine-Russia at Limited War; 2 theaters hot"), and the model-state footer
  shows the coupling multiplier as ONE opaque number ("coupling ×1.45"). Neither answers the systemic "why":
  is the world close to a great-power war because of a single-theater nuclear BRINK, great powers ENTANGLED
  across theaters, MANY theaters hot at once, or an ALLIANCE invocation? Each implies a different operator
  response, and the model already computes all four lifts internally (`l_sys = max_heat · brink_mult ·
  coupling_multiplier · concurrency_mult`) — they were just never surfaced. A real gap on the mission's
  "where & why" leg, at the systemic scale the per-theater drivers can't address.
- Change (one coherent change, models.rs + theater.rs + dashboard.html + indicators.rs test + server.rs
  test): (a) added the pure `theater::dominant_coupling_amplifier(brink_lift, gp_lift, breadth_lift,
  alliance_lift)` — picks the channel with the largest multiplicative lift, with a tiny `AMPLIFIER_FLOOR`
  (1e-6) so float dust on an uncoupled world names nothing, and apex-severity tie-breaking (brink ≻
  gp-entanglement ≻ concurrency ≻ alliance); (b) `theater::compute` feeds it the SAME excesses it already
  builds (`brink_mult−1`, `COUPLING_GP_WEIGHT·gp_entanglement`, `concurrency_mult−1`,
  `COUPLING_ALLIANCE_WEIGHT·alliance_activation`) and stores the label in the new
  `SystemicCouplers.coupling_driver` (serde-default ""); (c) the dashboard model-state footer appends
  "· led by X" sourced from the live `d.couplers.coupling_driver` (anti-drift — no hand-typed label). Honest
  by construction: a read-out of the engine's own terms, can never disagree with the math, NO model/
  calibration constant touched.
- Metric moved: test count 380 → 382 by the scorecard grep (two new locks:
  `coupling_driver_names_the_dominant_systemic_amplifier`, `dashboard_surfaces_the_systemic_coupling_driver`);
  new awareness capability — the systemic-level "why" (dominant coupling channel) is now legible, where
  before only the magnitude was. Calibration evidence UNCHANGED — backtest 9/9 (quiet/Ukraine/current/Cuba +
  evidence), no model constant touched.
- Proof: `cargo build --release` clean; `cargo clippy --release` 0 warnings; `cargo test --release` = 381
  passed / 0 failed / 3 ignored (382 by the grep incl. both new tests); `cargo test --release backtest` = 9
  passed. The model lock unit-checks the decomposition + the tie (a 4-way tie resolves to the apex brink) +
  the floor (all-zero → ""), then drives the LIVE engine through four isolated worlds: a US–Russia nuclear
  standoff → "single-theater nuclear brink" (its 0.70 lift outranks the 0.30 gp lift it also carries); a
  conventional US–Russia single theater → "great-power entanglement" (no brink, no breadth); three non-GP
  conventional theaters hot → "multi-theater concurrency" (gp/brink/alliance all 0); a single non-GP theater
  hot → "" (regional, not yet coupled); quiet → "". The dashboard lock proves the footer reads
  `d.couplers.coupling_driver` and labels it "led by".
- Notes / decisions future runs must respect: `coupling_driver` is a pure READ-OUT of the lifts that build
  `l_sys` (`dominant_coupling_amplifier`), NOT a new model lever — do not give it its own constant or let it
  feed back into the computation. It deliberately covers only the four theater-coupling channels (brink / GP
  entanglement / concurrency / alliance), NOT the guardrail-collapse term (that is set by the caller from the
  regime multiplier in bayesian.rs and is separately surfaced as `f-guard`); keep the label scoped to "what
  turns a *regional* crisis systemic" so it stays honest. The dashboard must keep sourcing the label from
  `d.couplers.coupling_driver` (never re-derive or hand-type it). Empty string is the honest
  "regional, not yet systemically coupled" read — don't paper over it with a default channel name.

## 2026-06-12 — robustness/honesty — finite-safe LLM output sanitation boundary (the single clamp between untrusted model output and the risk engine) (roadmap 4.4)
- Item: roadmap 4.4 (new, now checked). Robustness axis (pillar 4): per the log, robustness was the
  least-recently-advanced axis (last real advance 2026-06-10, the 4.3 shutdown fix; awareness 06-11,
  honesty/legibility 06-12), so this rotates coverage back onto it. The open robustness item 4.2
  (unwrap/expect audit) has been re-audited clean across several prior runs (all production unwrap/expect
  sites are infallible-by-construction or startup expects — forcing a "fix" to non-broken code is
  forbidden), so rather than churn that I looked for a genuinely fallible boundary that was hardened in
  one place but not locked. Found one that also serves pillar-1 HONESTY directly.
- Verified-open-first (read the running code end-to-end): `LlmEnricher::classify` parsed the model's JSON
  into `LlmExtraction` and clamped the five modality scores + severity to [0,1] and escalation_step to
  [-1,1] via an INLINE loop inside the async network path (llm_enricher.rs:167). Two real defects: (1) that
  inline clamp is the SINGLE point of defense — confirmed by reading the two consumers, `merge_llm_scores`
  (nlp_sidecar.rs:360) and `make_event_from_llm` (:400), which copy `x.modality_pairs()` and `x.severity`
  straight into `event.domain_signals` / `event.severity` and re-clamp ONLY `escalation_step`; so a modality
  score that escaped the classify clamp would flow unfiltered into the systemic read; (2) `f64::clamp`
  returns NaN UNCHANGED (for `self=NaN`, both `self<lo` and `self>hi` are false), so a non-finite score (a
  NaN/Inf from an overflowing model token) would survive the inline loop and poison the engine. And the
  clamp was UNTESTED for out-of-range input — buried in the network path, it could not be exercised without
  a live LLM, so a regression dropping it would pass CI silently. Confirmed `classify` (nlp_sidecar.rs:267)
  is the SOLE production producer of an `LlmExtraction` (other construct sites are tests), so a sanitize at
  that ingress covers every production extraction.
- Change (one coherent change, llm_enricher.rs only): (a) extracted a pure `LlmExtraction::sanitize(&mut
  self)` with a `finite_clamp` helper that maps any non-finite value to 0.0 (absent — the honest default
  when the model returns garbage) and otherwise clamps; modalities+severity→[0,1], escalation_step→[-1,1];
  documented as the single point of defense and why the explicit finiteness check is load-bearing (not a
  decorative refactor — it is strictly stronger than the old bare-clamp loop, which let NaN through);
  (b) `classify` now calls `x.sanitize()` in place of the inline loop; (c) locked by
  `sanitize_clamps_out_of_range_and_neutralizes_non_finite_scores`. NO model/calibration constant touched —
  in-range scores are preserved bit-identically (the test asserts 0.42/0.8/-0.6 pass through untouched), so
  the live read is unchanged; the only behavior change is that out-of-range/non-finite garbage is now
  neutralized where before NaN/Inf would have leaked.
- Metric moved: test count 379 → 380 by the scorecard grep (new
  `sanitize_clamps_out_of_range_and_neutralizes_non_finite_scores`); a new robustness capability (a finite-
  safe, lockable sanitation boundary) + a latent honesty hazard (non-finite LLM score reaching the engine)
  closed. Calibration evidence UNCHANGED — backtest 9/9 (quiet/Ukraine/current/Cuba + evidence), no model
  constant touched.
- Proof: `cargo build --release` clean; `cargo clippy --release` 0 warnings; `cargo test --release` = 379
  passed / 0 failed / 3 ignored (380th is the scorecard grep incl. the new test); `cargo test --release
  backtest` = 9 passed. The lock drives sanitize with out-of-range finite values (1.7→1.0, -0.4→0.0,
  2.0→1.0, 9.9→1.0) AND non-finite ones (NaN→0.0, +Inf→0.0 NOT 1.0, -Inf→0.0), asserts every field is
  finite and in-band afterward, and asserts in-range values pass through untouched — a revert to the bare
  `f64::clamp` loop fails it (NaN/Inf would survive).
- Notes / decisions future runs must respect: `LlmExtraction::sanitize()` is the SINGLE clamp between
  untrusted model output and the risk engine — keep `classify` calling it, and do NOT re-introduce a bare
  `f64::clamp` (it lets NaN/Inf through). The consumers (`merge_llm_scores`/`make_event_from_llm`) rely on
  the extraction already being sanitized; if a future path constructs an `LlmExtraction` from external
  input OUTSIDE `classify`, it must call `sanitize()` too. Robustness 4.2 (unwrap/expect audit) remains
  the only open robustness item and is still clean per repeated audits.

## 2026-06-12 — honesty/legibility — regime INSPECTOR panel now reports structural pressure → guardrail collapse, not a v1 "Adjusted P₀" (roadmap 2.3)
- Item: roadmap 2.3 — the regime-factor INSPECTOR sibling the SAME-DAY dashboard-footer entry (below)
  explicitly left open: "the regime-factor INSPECTOR panel (dashboard.html:1120, `api.rs::regime_summary`)
  still labels `baseline × regime_product` as 'Adjusted P₀' — honestly reframe as 'structural pressure'."
  Honesty axis (pillar 1, top mission priority: "the number must mean what it says"), on the operator
  surface (pillar 2). Axis rotation: robustness (4.x) was least-recent (2026-06-10), but 4.2 STILL has no
  clean must-fix — re-audited every production `unwrap()/expect()` this run (the full `src` grep): all are
  infallible-by-construction (static-regex compile in processor.rs:854; NaN-filtered `partial_cmp` in
  detector.rs:146; `nearest_site.unwrap()` after a `None`-returning guard at detector.rs:491;
  never-closed-semaphore acquires in ingestor/nlp_sidecar; `position().unwrap()` over the source collection
  in models.rs:221/243) or startup `expect`s (signal handlers, HTTP-client builds) — forcing a "fix" to
  non-broken code is forbidden. So the highest-mission-value provable-green lever was this honesty defect.
- Verified-open-first: `api.rs::regime_summary` computed `adjusted_prior = HISTORICAL_ANCHOR × product` and
  served it as `adjusted_prior`/`adjusted_prior_pct`; the dashboard inspector (dashboard.html:1120) rendered
  it as "Adjusted P₀: X%/yr". This is the SUPERSEDED v1 multiplicative form — it tells an operator that
  toggling a regime factor moves the forecast PRIOR. The v2 engine holds the prior FLAT (`HISTORICAL_ANCHOR`,
  bayesian.rs:721) and the regime product enters ONLY through `guardrail_from_regime` as a bounded
  guardrail-collapse amplifier on the systemic likelihood (`l_sys × (1 + GUARDRAIL_AMPLIFIER·guardrail)`, max
  +12%, Step 6b — locked by `guardrail_collapse_is_live_in_compute_and_only_amplifies_the_likelihood`). Same
  defect class as the same-day footer fix, on the panel an operator uses to reason about their toggles. Worse,
  `regime_warnings` told the operator stacked multipliers "may place the model above ELEVATION_THRESHOLD with
  zero event signal" — FALSE in v2: with zero signal `l_sys ≈ 0`, so even at full guardrail collapse the
  forecast stays at the flat prior, well below elevation.
- Change (one coherent change, bayesian.rs + api.rs + dashboard.html + main.rs + tests): (a) made
  `bayesian::guardrail_from_regime`, `GUARDRAIL_AMPLIFIER`, `GUARDRAIL_REGIME_SPAN` `pub` so the inspector
  reports EXACTLY the coupler the engine computes (anti-drift, single source of truth — the same discipline
  as the alert-band thresholds); (b) `regime_summary` drops `adjusted_prior`/`adjusted_prior_pct` and adds
  `guardrail_collapse` (= `guardrail_from_regime(product)`, 0..1) and `likelihood_amplifier_pct`
  (= `100·GUARDRAIL_AMPLIFIER·guardrail`, the bounded 0..12% lift on the systemic likelihood); `get_regime`
  passes the new fields through; (c) dashboard inspector now reads "Structural pressure: N× → guardrail
  collapse G (+X% on systemic L, prior unaffected)"; (d) reframed `regime_warnings` to the v2 reality (a
  product past the 5× saturation point means guardrails are modeled as fully collapsed at the max +12% lift,
  further stacking does nothing, prior unaffected — keeps mentioning the 20× threshold) and the
  `REGIME_PRODUCT_WARN_THRESHOLD` rationale comment; (e) reframed the startup log line (was "adjusted prior
  X%/yr"). NO model/calibration constant touched — `adjusted_prior` is no longer fabricated for this panel;
  the SNAPSHOT's separate `adjusted_prior` field (a different surface) is untouched.
- Metric moved: test count 378 → 379 by the scorecard grep (net: replaced the v1
  `regime_summary_adjusted_prior_uses_historical_anchor` test with the v2 honesty lock, +1 new dashboard
  lock); a pillar-1 honesty defect on an operator surface closed. Calibration evidence UNCHANGED — backtest
  9/9 (quiet/Ukraine/current/Cuba + evidence), no model constant touched.
- Proof: `cargo build --release` clean; `cargo clippy --release` 0 warnings; `cargo test --release` = 378
  passed / 0 failed / 3 ignored (379th is the scorecard grep incl. the new test); `cargo test --release
  backtest` = 9 passed. `regime_summary_reports_guardrail_collapse_not_an_adjusted_prior` proves the v1
  `adjusted_prior`/`adjusted_prior_pct` fields are GONE (null), that a neutral regime reports 0 collapse / 0
  lift, that an elevated regime (product 3.0×) reports EXACTLY the engine's `guardrail_from_regime` and its
  bounded lift, and that past saturation the collapse clamps at 1.0 / +12% — a revert to the v1 adjusted
  prior fails it. `dashboard_regime_inspector_shows_structural_pressure_not_adjusted_prior` proves the panel
  no longer says "Adjusted P₀"/reads `adjusted_prior_pct` and now sources the live `guardrail_collapse`.
- Notes / decisions future runs must respect: the regime product is STRUCTURAL PRESSURE driving guardrail
  collapse, NOT an adjusted prior — do NOT re-introduce an `adjusted_prior`-style "%/yr" figure for the
  regime panel (it resurrects the v1 lie that a regime toggle moves the prior). `guardrail_from_regime` /
  `GUARDRAIL_AMPLIFIER` / `GUARDRAIL_REGIME_SPAN` are now `pub` and are the single source of truth the
  inspector reports from — keep the panel sourcing the engine coupler, never a re-derived number. The
  SNAPSHOT's `adjusted_prior` field is a separate concern (still computed/serialized) and was NOT touched
  here. Remaining under 2.3: regime ×/GP internals in the methodology view.

## 2026-06-12 — honesty/legibility — dashboard now explains the v2 flat-prior + guardrail-collapse mechanism (removed the v1 "regime-adjusted prior × likelihood" story) (roadmap 1.2/2.3)
- Item: roadmap 1.2/2.3 — a v1 vestige on the PRIMARY operator surface, the same class as the
  2026-06-11 dead-`gp_bonus` finding but in the dashboard's headline explanation rather than the
  engine. Honesty axis (pillar 1, top mission priority: "the number must mean what it says"), applied
  to legibility (pillar 2, the operator-facing surface). Axis rotation: robustness (4.x) was
  least-recent (2026-06-10), but 4.2 has no clean must-fix — re-audited this run: the production
  unwrap/expect sites are infallible-by-construction (`detector::nearest_test_site`'s
  `partial_cmp().unwrap()` — NaN distances filtered before `min_by`; `detector.rs:491`
  `nearest_site.unwrap()` — the `None` arm returns, so it is always `Some`; `ingestor.rs:547`/
  nlp_sidecar `acquire().await.unwrap()` — the semaphore is never closed; `models.rs` position
  unwraps — drawn from the same collection), and forcing a "fix" to non-broken code is forbidden. So
  the highest-mission-value provable-green lever was this honesty defect, which the unwrap audit
  surfaced en route.
- Verified-open-first (the payoff of reading the running code): the v2 engine computes the forecast
  from a FLAT prior — `bayesian.rs:721` `let prior = HISTORICAL_ANCHOR` (= `BASELINE_ANNUAL`, 1.5%),
  with the regime multiplier entering ONLY as a bounded guardrail-collapse amplifier on the systemic
  likelihood (`l_sys × (1 + GUARDRAIL_AMPLIFIER·guardrail)`, max +12%, Step 6b), combined on the
  log-odds scale (Step 7). This is LOCKED by the 2026-06-10
  `guardrail_collapse_is_live_in_compute_and_only_amplifies_the_likelihood` test. But `snap.adjusted_prior`
  = `HISTORICAL_ANCHOR × regime_multiplier` (≈ 0.015×5.46 ≈ 8.2%) is a v1 leftover the engine NO LONGER
  USES in the forecast — yet the dashboard (a) drew it in the model-state footer as "structural-adjusted"
  BETWEEN the baseline and the systemic L, presenting it as a chain step toward P(WWIII|E), and (b) the
  "how it's built" modal told the operator the headline is "a regime-adjusted prior … multiplied by a
  coupling likelihood" — verbatim the SUPERSEDED v1 multiplicative `P₀_adj × (1 + L·k)` form the v2
  comment (bayesian.rs:578) explicitly says was replaced. The number did not mean what the dashboard
  said it meant — a real pillar-1 defect on the surface an operator actually watches.
- Change (one coherent change, dashboard.html + server.rs test): (a) footer formula now reads
  `Baseline P₀ = {{BASELINE_ANNUAL_PCT}}%/yr (modern, flat) · systemic L = … · guardrail collapse = …`
  then `P(WWIII|E) = σ(logit P₀ + β·L) = …` — the honest v2 chain (flat prior, the likelihood, the
  regime's only forecast channel shown as guardrail collapse, and the log-odds fold); (b) the removed
  "structural-adjusted = f-adj" readout is replaced by a guardrail-collapse readout that reads the LIVE
  `d.couplers.guardrail_collapse` coupler (anti-drift, same discipline as the alert bands — no
  hardcoded number); the now-orphaned `d.prior.adjusted_prior` JS reference is gone; (c) the "how it's
  built" modal rewritten to describe the flat baseline, the log-odds fold, and the three real systemic
  amplifiers (great-power entanglement, multi-theater concurrency, guardrail collapse), stating plainly
  "the regime no longer inflates the prior; it enters the likelihood through guardrail collapse." NO
  model/calibration constant touched — `adjusted_prior` is still computed/serialized (the regime-factor
  inspector panel uses `adjusted_prior_pct` as a structural-pressure figure); only the misleading
  headline explanation changed.
- Metric moved: test count 377 → 378 by the scorecard grep (new
  `dashboard_explains_the_v2_flat_prior_not_the_v1_adjusted_prior`); a pillar-1 honesty defect on the
  primary operator surface closed (the headline explanation now matches the engine). Calibration
  evidence UNCHANGED — backtest 9/9 (quiet/Ukraine/current/Cuba + evidence), no model constant touched.
- Proof: `cargo build --release` clean; `cargo clippy --release` 0 warnings; `cargo test --release` =
  377 passed / 0 failed / 3 ignored (378th is the scorecard grep incl. the new test); `cargo test
  --release backtest` = 9 passed. The lock proves the v1 story is GONE from the dashboard
  (no "structural-adjusted", no "regime-adjusted prior", no `f-adj`, no `d.prior`) and the v2 story is
  present and honest (the "(modern, flat)" prior, "log-odds" fold, "guardrail collapse" channel) AND
  the guardrail readout sources `d.couplers.guardrail_collapse` live — a revert to the v1 explanation
  fails it.
- Notes / decisions future runs must respect: the dashboard headline explanation now matches the v2
  engine — the prior is FLAT, the regime enters ONLY via the bounded guardrail-collapse amplifier on
  `l_sys`. Do NOT re-introduce a "regime-adjusted prior" / "structural-adjusted" chain step or a
  "prior × likelihood" description (it resurrects the v1 form the engine abandoned). Remaining sibling
  (still open): the regime-factor INSPECTOR panel (dashboard.html:1120, driven by `api.rs`
  `regime_summary`) still labels `baseline × regime_product` as "Adjusted P₀" — a legitimately-computed
  structural-pressure figure, but a future run could honestly reframe it as "structural pressure" so it
  isn't mistaken for the forecast prior either (would touch api.rs + its tests; left out of this commit
  for scope discipline).

## 2026-06-11 — honesty/legibility — templated P₀ on the DASHBOARD (the operator surface the methodology fix missed), anti-drift from BASELINE_ANNUAL (roadmap 2.3)
- Item: roadmap 2.3 (progressed) — extends the same-day methodology P₀ fix to the DASHBOARD itself.
  Honesty axis (pillar 1) as much as legibility: a hand-typed model constant on the operator-facing
  surface is a drift hazard ("the number must mean what it says"). Axis note: robustness (4.2) is
  least-recent but has no clean must-fix — re-verified `detector::nearest_test_site`'s
  `partial_cmp().unwrap()` this run: it is genuinely safe because any NaN haversine distance (NaN
  lat/lon, or near-antipodal float-rounding pushing `a` slightly above 1.0) fails the `dist <= radius`
  filter and is dropped before `min_by`, so the comparator only ever sees finite values — forcing a
  "fix" to non-broken code is forbidden. So the highest-value provable-green lever was this anti-drift
  completeness fix on the primary surface.
- Verified-open-first: grepped the dashboard for the baseline literal. The 2026-06-11 methodology run
  templated P₀ on the WHITEPAPER but the DASHBOARD — the surface an operator actually watches — still
  hand-typed the quiet-year baseline in TWO places: the model-state footer's live Bayesian chain
  (`Baseline P₀ = 1.5%/yr (modern, backtested)`, dashboard.html:487) and the "what this means"
  info-modal calibration line (`<code>~1.5%</code> modern quiet-year baseline`, :730). Recalibrating
  `models::BASELINE_ANNUAL` (0.015) would silently leave both quoting a stale 1.5% — the exact
  stale-prose class the methodology, ceiling, and alert-band templating fixes exist to kill, left open
  on the most-seen surface.
- Change (one coherent change, dashboard.html + server.rs): replaced both hand-typed references with
  `{{BASELINE_ANNUAL_PCT}}`, and `server.rs::generate_dashboard_html` now substitutes it from
  `format!("{:.1}", models::BASELINE_ANNUAL * 100.0)` — the same anti-drift template mechanism as the
  dashboard's existing `{{ELEVATION_THRESHOLD}}` and the methodology's `{{BASELINE_ANNUAL_PCT}}`. NO
  model/calibration constant touched (BASELINE_ANNUAL unchanged at 0.015).
- Metric moved: test count 376 → 377 by the scorecard grep (new
  `dashboard_renders_baseline_prior_from_the_model_constant`); a latent prose-drift hazard on the
  model's foundational prior closed on the PRIMARY operator surface. Calibration evidence UNCHANGED —
  backtest 9/9 (quiet/Ukraine/current/Cuba + evidence), no calibration constant touched.
- Proof: `cargo build --release` clean; `cargo clippy --release` 0 warnings; `cargo test --release` =
  376 passed / 0 failed / 3 ignored; `cargo test backtest` = 9 passed. The lock proves BOTH dashboard
  references carry the placeholder (`.matches(...).count() == 2`), that it is substituted at render
  time, and that the rendered footer (`1.5%/yr`) and calibration line (`~1.5%`) both equal
  `BASELINE_ANNUAL * 100` — a revert to a hand-typed number fails it.
- Notes / decisions future runs must respect: the dashboard baseline-prior digits are now templated
  from `BASELINE_ANNUAL` — edit the CONSTANT, never the HTML. P₀ is now anti-drift on BOTH operator
  surfaces (dashboard + methodology); it joins the forecast ceiling, alert bands, and elevation
  threshold as model values that render anti-drift everywhere they appear. Remaining under 2.3: the
  regime × factors and GP internals in the methodology view (regime factors are operator-tunable
  runtime config, not a static constant — needs the live config, a different mechanism).

## 2026-06-11 — legibility — templated P₀ (the baseline prior) in the methodology, anti-drift from BASELINE_ANNUAL (roadmap 2.3)
- Item: roadmap 2.3 (progressed) — "model internals (regime ×, P₀, GP, elevated) belong in the
  methodology view … keep methodology honest and current with the model." This run closes the **P₀**
  leg. Legibility axis (pillar 2). Axis rotation: robustness (4.2) is least-recent but has no clean
  must-fix — its production unwrap/expect sites are infallible-by-construction (re-verified this run:
  `models.rs::theater_for_event`'s two `position().unwrap()`s are safe because `Theater::primary()`
  always contains the theater and `max` is drawn from `counts`; detector/ingestor/nlp_sidecar sites are
  startup `expect`s or never-closed-semaphore acquires), and forcing a "fix" to non-broken code is
  forbidden. Awareness's only open item (3.2 GDELT) needs live network the sandbox lacks. So the
  highest-value provable-green lever was this anti-drift completeness fix on the methodology.
- Verified-open-first: read the methodology baseline-prior section against the current model. The flat
  quiet-year prior — the single most foundational number in v2 (the logistic prior the whole forecast is
  folded onto) — was a HAND-TYPED `<code>BASELINE_ANNUAL ≈ 1.5%/yr</code>`, even though the forecast
  ceiling literally one paragraph below was already `{{FORECAST_PROB_CEILING}}` and the alert bands were
  `{{ALERT_*}}`. A real drift hazard: recalibrating `models::BASELINE_ANNUAL` (0.015) would silently
  leave the operator-facing whitepaper quoting a stale 1.5% — exactly the stale-prose class prior runs
  fixed for the ceiling (1.2, the old 0.85 comments), the calibration table (1.1b) and the alert bands.
- Change (one coherent change, methodology.html + server.rs): (a) replaced the hand-typed `1.5%/yr` with
  `{{BASELINE_ANNUAL_PCT}}%/yr` and added the same "rendered from the model's own constant, so this page
  cannot drift" note the ceiling carries; (b) `server.rs::ServerState::new` substitutes the placeholder
  from `format!("{:.1}", models::BASELINE_ANNUAL * 100.0)` — the same anti-drift template mechanism as the
  ceiling/alert bands. NO model/calibration constant touched (BASELINE_ANNUAL unchanged at 0.015).
- Metric moved: test count 375 → 376 by the scorecard grep (new
  `methodology_renders_baseline_prior_from_the_model_constant`); a latent prose-drift hazard on the
  model's foundational prior closed. Calibration evidence UNCHANGED — backtest 9/9 (quiet/Ukraine/current/
  Cuba + evidence), no calibration constant touched.
- Proof: `cargo build --release` clean; `cargo clippy --release` 0 warnings; `cargo test --release` =
  375 passed / 0 failed / 3 ignored (376th is the scorecard grep incl. the new test); `cargo test backtest`
  = 9 passed. The lock proves the `{{BASELINE_ANNUAL_PCT}}` placeholder is substituted at startup, that the
  rendered `1.5%/yr` matches `BASELINE_ANNUAL * 100`, and that the raw template still carries the
  placeholder (a revert to a hand-typed number fails it).
- Notes / decisions future runs must respect: the methodology baseline-prior number is now templated from
  `BASELINE_ANNUAL` — edit the CONSTANT, never the HTML digits. P₀ joins the forecast ceiling, alert bands,
  and elevation threshold as model values that render anti-drift into operator-facing surfaces. Remaining
  under 2.3: the regime × factors and GP internals in the methodology view (the regime factors are
  operator-tunable runtime config from settings.yml, not a static constant — surfacing them honestly needs
  the live config, a different mechanism than constant-templating).

## 2026-06-11 — honesty/model — unified the intra-theater co-occurrence ramp with the systemic one + locked "sub-threshold modality contributes 0 co-occurrence" (roadmap 1.2/1.3)
- Item: roadmap 1.2/1.3 — the open honesty sibling the 2026-06-09 "quiet theater never leaks" entry
  explicitly flagged: "the same no-leak property for the intra-theater co-occurrence ramp (`ELEV_RAMP`
  around `ELEVATION_THRESHOLD`) — a sub-threshold modality must contribute 0 co-occurrence; not yet
  locked." Honesty axis (pillar 1, top mission priority). Axis rotation: today already advanced
  awareness/honesty/legibility; robustness (4.2) has no clean must-fix (its production unwrap/expect
  sites are infallible-by-construction — re-verified detector/ingestor this run, confirming the
  2026-06-10 finding), and forcing a "fix" to non-broken code is forbidden — so the highest-value
  provable-green lever was this flagged honesty lock.
- Verified-open-first: `theater.rs::score_theater` computed its intra-theater co-occurrence soft count
  with its OWN `const ELEV_RAMP = 0.08` and an inline `smoothstep(d.score, THRESHOLD−ELEV_RAMP,
  THRESHOLD+ELEV_RAMP)`, duplicating `bayesian::ELEVATION_RAMP` + `soft_elevation_weight` (the systemic
  co-occurrence path). The comment claimed it "mirrors bayesian::ELEVATION_RAMP" but NOTHING enforced
  it: a future tweak to either ramp would silently make "elevated" mean two different things in two
  places — a real dishonesty hazard — and the property the systemic side guards (a sub-threshold
  modality adds exactly 0) was unguarded on the intra-theater side.
- Change (one coherent change, bayesian.rs + theater.rs): (a) made `soft_elevation_weight` pub and
  documented it as the SINGLE source of truth for "how elevated, smoothly, is one modality" — used by
  BOTH the systemic co-occurrence (`compute` Step 5) and the intra-theater co-occurrence; (b) replaced
  theater's inline duplicate with `soft_elevation_weight(d.score)` and removed the now-dead `ELEV_RAMP`
  constant. Behavior-preserving: both ramps were 0.08 with an identical smoothstep formula, so the
  computation is bit-identical today; this collapses two definitions into one so they can never drift.
  NO calibration constant touched.
- Metric moved: test count 374 → 375 by the scorecard grep (new
  `intra_theater_co_occurrence_uses_the_shared_ramp_and_ignores_sub_threshold_modalities`); a
  drift-prone duplicated ramp removed and a previously-unguarded honesty invariant now locked.
  Calibration evidence UNCHANGED — backtest 9/9, no model constant touched.
- Proof: `cargo build --release` clean; `cargo clippy --release` 0 warnings; `cargo test --release` =
  375 passed / 0 failed / 3 ignored; `cargo test backtest` = 9 passed. The lock reconstructs the
  co-occurrence multiplier the engine actually applied (`cooc = heat·max_weighted_sum/weighted`, heat
  uncapped) and proves: (1) for a theater with ONE elevated modality plus a faint sub-threshold blip
  (its `soft_elevation_weight` asserted == 0), the applied cooc is exactly neutral (1.0) AND equals
  `co_occurrence_boost(shared soft-elevation sum)` — a revert to a divergent ramp breaks the equality;
  (2) promoting that second modality above the ramp lifts cooc above 1.0 — proving the boundary is
  exactly the shared elevation ramp.
- Notes / decisions future runs must respect: `soft_elevation_weight` is now the ONE definition of
  smooth elevation — do NOT re-introduce a separate `ELEV_RAMP`/inline smoothstep in theater.rs (it
  resurrects the drift hazard). `ELEVATION_RAMP` (0.08) and `ELEVATION_THRESHOLD` (0.32) are FITTED
  (the bands depend on them) — move only with evidence + a test. A sub-threshold modality contributing
  0 co-occurrence is an honesty invariant now locked on both the systemic and intra-theater paths.

## 2026-06-11 — awareness — per-theater "second dimension": secondary_driver (2nd elevated force) on the ladder (roadmap 3.3 extension)
- Item: roadmap 3.3 (extended — the "2nd LEVEL contributor on the chip" the 2026-06-10 delta-driver
  entry explicitly flagged as the remaining awareness extension). Awareness axis (pillar 3): reading
  the recent log, honesty and legibility were each advanced 2026-06-11 (most recent), while awareness
  and robustness were tied least-recent at 2026-06-10 — this rotates coverage back onto awareness. Of
  the two open awareness items, 3.2 (GDELT) needs live-network verification the cloud sandbox can't do,
  so the provable-green awareness lever today was this 2nd contributor.
- Verified-open-first: `TheaterState` carried `top_driver` (dominant weighted LEVEL) and `rising_driver`
  (what MOVED this tick) but nothing about the COMPOSITION of a flashpoint's heat. The chip therefore
  reads a two-dimensional crisis — say a theater hot on nuclear posture that ALSO has elevated military
  escalation — as a one-dimensional "Nuclear" story. That's exactly the multi-modality co-occurrence the
  intra-theater amplifier responds to, yet the operator couldn't see it: a real awareness gap on the
  mission's "where & why" leg.
- Change (one coherent change across model + engine + dashboard): (a) `TheaterState.secondary_driver` —
  the SECOND-largest WEIGHTED contributor (`score × domain_weight`) AMONG the modalities the model
  considers *elevated* (raw score ≥ `ELEVATION_THRESHOLD`, the same cutoff that feeds the co-occurrence
  amplifier). Computed in `theater.rs::score_theater` by sorting the elevated modalities' weighted terms
  and taking the 2nd; empty unless ≥2 modalities are elevated (a single-dimension flashpoint names
  nothing), empty for a Stable theater. The elevation gate is the honesty point: it is NOT merely "the
  2nd-largest weighted term" — a faint sub-threshold modality is excluded, so the chip only claims a
  second dimension when the model genuinely sees one. (b) dashboard ladder chip joins the two as
  "Nuclear + Military" on the sub-line and in the "driven by …" tooltip, reusing `domainLabel` (no new
  label table). Honest by construction (the model's own second elevated weighted term); NO calibration
  constant touched.
- Metric moved: test count 373 → 374 by the scorecard grep (new
  `secondary_driver_names_the_second_elevated_force_dimension`); new awareness capability (per-theater
  second/co-occurrence dimension). Calibration evidence UNCHANGED — backtest 9/9 (quiet/Ukraine/current/
  Cuba + evidence), no model constant touched.
- Proof: `cargo build --release` clean; `cargo clippy --release` 0 warnings; `cargo test --release` =
  373 passed / 0 failed / 3 ignored (374th is the scorecard grep incl. the new test); `cargo test
  backtest` = 9 passed. The lock drives four scenarios on the live engine: (a) equal-strength
  nuclear+military both elevated → top_driver nuclear (weight 3.0 dominant level), secondary_driver
  military (2nd elevated dimension), both asserted ≥ ELEVATION_THRESHOLD; (b) only-kinetic (one elevated
  dimension) → top_driver military but secondary EMPTY (the distinction from top_driver, which always
  names); (c) the GATE — strong nuclear + a faint kinetic blip whose score stays BELOW
  ELEVATION_THRESHOLD → secondary EMPTY even though kinetic is the 2nd-largest weighted term overall
  (a revert to "2nd largest weighted period" fails this); (d) quiet world → all empty.
- Notes / decisions future runs must respect: `secondary_driver` is the 2nd-largest weighted modality
  AMONG ELEVATED ones — do not drop the elevation gate (it is what keeps the chip from claiming a second
  dimension that doesn't exist) and do not conflate it with `top_driver` (dominant level) or
  `rising_driver` (what moved). All three are honest read-outs of existing scores, never new model
  levers. The chip reuses `domainLabel`. Remaining awareness item: 3.2 (GDELT) — still gated on
  live-network verification, not for the cloud routine.

## 2026-06-11 — honesty/model — removed dead `gp_bonus` (a v1 vestige in score_all) + locked "GP is a coupler, not a per-domain bonus" (roadmap 1.2)
- Item: roadmap 1.2 (progressed) — the last `score_all`/`compute` literal it flagged as un-pinned, the
  `gp_bonus` `0.12`. Honesty axis (pillar 1, top mission priority). Axis rotation: legibility was advanced
  twice running (06-10 2.2/2.4, 06-11 2.3), so it was over-represented; honesty/awareness/robustness were
  tied least-recent (06-10). Of the cloud-provable honesty levers, this was the highest-value because
  verifying it surfaced a real defect, not just a missing name.
- Verified-open-first (the payoff of actually checking the current code): `gp_bonus` was not merely an
  un-named fitted constant — it was **dead code**. The branch `domain == "great_power_conflict"` can never
  be true in `score_all`: v2 removed `great_power_conflict` from `DOMAIN_WEIGHTS` (it became the
  `gp_entanglement` systemic coupler in theater.rs), and `score_all` `continue`s past any domain not in
  `DOMAIN_WEIGHTS` (bayesian.rs:327). So `gp_bonus` was provably always `0.0`. The comment above it was
  actively MISLEADING — it described a per-domain great-power scoring lift that the v2 refactor
  deliberately abolished to kill the v1 collinearity where "one great-power strike lit four buckets and was
  counted ~4×". A dishonest piece of code: prose claiming a behavior the engine cannot perform.
- Change (one coherent change, bayesian.rs only): (a) removed the dead `gp_bonus` let-binding and its
  stale comment; simplified `base_signal` to `nlp_signal * (0.55 + 0.45 * intensity)` (the `+ gp_bonus`
  was `+ 0.0`); (b) replaced the comment with an accurate v2 note (GP is the `gp_entanglement` coupler,
  never a per-domain bonus; it only increments the display-only `great_power_event_count`); (c) fixed the
  adjacent stale "all 8 domains" comment → "all five modalities" (same v1-domain-count vestige in the same
  function). NO value changed — `great_power_involved` already had zero effect on any modality score.
- Metric moved: test count 372 → 373 (new `great_power_involvement_does_not_add_a_per_domain_score_bonus`);
  dead code removed + a v2 design-intent honesty property (GP scores via the coupler, never the domain) now
  LOCKED where before it was only prose. Calibration evidence UNCHANGED — backtest 9/9
  (quiet/Ukraine/current/Cuba + evidence), bit-identical (the removed term was always 0).
- Proof: `cargo build --release` clean; `cargo clippy --release` 0 warnings; `cargo test --release` =
  372 passed / 0 failed / 3 ignored (373rd is the scorecard grep incl. the new test); `cargo test backtest`
  = 9 passed. The lock scores identical events with `great_power_involved` true vs false and asserts
  byte-identical modality scores across all five `DOMAIN_WEIGHTS` domains, while `great_power_event_count`
  still reads 0 vs 1 — so re-adding any per-domain GP bonus (the v1 mistake) fails it, but awareness of GP
  involvement is preserved.
- Notes / decisions future runs must respect: great-power involvement is a SYSTEMIC COUPLER
  (`gp_entanglement`), NOT a per-domain scoring bonus — do not re-introduce a `gp_bonus`-style lift in
  `score_all` (it would resurrect the v1 ~4×-counting collinearity the v2 refactor removed). The five
  `DOMAIN_WEIGHTS` modalities measure the KIND of force, never WHO. 1.2's `compute`/`score_all` literal
  sweep is now complete; the only remaining un-pinned 1.2 surface is the regime × factor defaults (already
  labeled `RegimeFactor`s in config, not blind literals).

## 2026-06-11 — legibility — methodology "Alert bands" section, thresholds templated from AlertSettings (roadmap 2.3)
- Item: roadmap 2.3 (progressed) — the natural sibling the 2026-06-10 alert-band entry explicitly
  flagged: "surfacing these same band thresholds in the methodology prose (2.3) so the `≥8% critical`
  text there is templated from `AlertSettings`". Legibility axis (pillar 2): every axis advanced
  2026-06-10, so axis-rotation is neutral; picked the highest-value item provable green in the cloud
  sandbox (2.1 small-viewport is eyes-judged only; 3.2 GDELT needs live network; this one is fully
  testable here).
- Verified-open-first: read the current `methodology.html` end-to-end — it documented the baseline
  prior, modalities, theaters, couplers, likelihood, the systemic index, calibration, AI layer,
  confidence and the nuclear detector, but had **no mention of the alert bands at all**. An operator
  reading the whitepaper had no way to learn what P(WWIII) makes the hero go amber vs red, even though
  the dashboard timeline now draws those exact lines (2.4). And the only place those thresholds lived
  in prose-readable form was the dashboard's live JS — the methodology was silent, a real completeness
  gap on the Legibility pillar.
- Change (one coherent change, methodology.html + server.rs): (a) added an `#alerts` "Alert bands"
  section (plus its TOC link) explaining that the annual P(WWIII) is elevated at/above
  `{{ALERT_ELEVATED}}`, critical at/above `{{ALERT_CRITICAL}}`, with a separate 30-day warning at
  `{{ALERT_30D}}`, and stating explicitly that these are the same bands the hero colour and timeline
  lines use; (b) `server.rs::ServerState::new` substitutes the three placeholders from
  `models::AlertSettings::default()` (elevated 2.5%, critical 8.0%, 30-day 1.0%) — the same anti-drift
  template mechanism as `{{FORECAST_PROB_CEILING}}`/`{{CALIBRATION_EVIDENCE}}`, so the prose can never
  drift from the engine's configured classification. NO model/calibration constant touched.
- Metric moved: test count 371 → 372 (new `methodology_renders_alert_bands_from_alert_settings`); new
  legibility capability (the methodology now documents the alert bands) + a latent completeness gap
  closed, with the thresholds anti-drift-templated rather than hand-typed. Calibration evidence
  UNCHANGED — backtest 9/9 (quiet/Ukraine/current/Cuba + evidence), no calibration constant touched.
- Proof: `cargo build --release` clean; `cargo clippy --release` 0 warnings; `cargo test --release` =
  371 passed / 0 failed / 3 ignored (372nd is the scorecard grep incl. the new test);
  `cargo test backtest` = 9 passed. The lock proves all three `{{ALERT_*}}` placeholders are
  substituted at startup, that the rendered values match `AlertSettings::default()`, and that the raw
  template still carries `{{ALERT_CRITICAL}}` (a revert to a hand-typed number fails it). The
  completeness test `methodology_html_is_substantial_and_complete` now also requires the `#alerts`
  anchor.
- Notes / decisions future runs must respect: the alert-band prose is now templated from
  `AlertSettings` — edit the SETTINGS, never the HTML numbers (the template carries placeholders, not
  digits). The methodology renders from `AlertSettings::default()` at startup (matches the seeded
  config in `main.rs`, also 2.5%/8.0%); the dashboard timeline/hero read the LIVE per-snapshot
  thresholds — both honest, one is design-default prose, the other is the running classification.
  Remaining under 2.3: surfacing the model internals (regime ×/P₀/GP) in the methodology view.

## 2026-06-10 — honesty/model — named the guardrail-collapse coupler (the last flagged un-pinned constants in bayesian.rs) (roadmap 1.2)
- Item: roadmap 1.2 (progressed) — the guardrail-coupler magic in `bayesian.rs::compute` that the
  2026-06-10 coupler-weights entry explicitly flagged as "a natural next 1.2 sibling". Honesty axis
  (pillar 1, the top mission priority). Verified the two flagged literals were still bare against the
  current code before acting.
- Verified-open-first: Step 6b carried structural guardrail erosion (arms-control death, deterrence
  decay, doctrine shifts) as a soft amplifier of the systemic likelihood via two un-named literals —
  `((regime_multiplier − 1) / 4.0).clamp(0,1)` and `l_sys × (1 + 0.12·guardrail)`. The relationship
  they encode (guardrail is BOUNDED, SOFT, SUBORDINATE — and enters only the likelihood, never the
  flat prior) was prose-only, with no test guarding it; a future recalibration could have let the
  background structural term swamp acute theater signal or leak into the quiet-year floor.
- Change (one coherent change, `bayesian.rs` only): (a) named both literals —
  `GUARDRAIL_REGIME_SPAN = 4.0` (the regime-multiplier excess above neutral 1.0 at which collapse
  saturates: a regime product of `1+SPAN = 5.0×` → guardrail 1.0; risk-reducing regimes floor at 0)
  and `GUARDRAIL_AMPLIFIER = 0.12` (the max +12% full collapse adds to `l_sys`), each with a
  rationale comment; (b) extracted a pure `guardrail_from_regime()` helper used in `compute`; (c)
  recorded the honesty FINDING in the rationale: the seeded acute factor set already compounds to
  ~5.46×, so the LIVE coupler currently sits at FULL collapse — a deliberate property of the current
  factor set, NOT a knob to chase by blind-tweaking. NO value changed.
- Metric moved: test count 369 → 371 (two new locks); a previously prose-only honesty relationship +
  the last two flagged un-pinned `compute` constants now named and locked. Calibration evidence
  UNCHANGED — backtest 9/9 (quiet/Ukraine/current/Cuba bands + evidence), no calibration constant
  touched.
- Proof: `cargo build --release` clean; `cargo clippy --release` 0 warnings; `cargo test --release` =
  370 passed / 0 failed / 3 ignored (371st is the scorecard grep incl. the new tests);
  `cargo test backtest` = 9 passed. `guardrail_coupler_is_a_bounded_soft_subordinate_amplifier` pins
  the regime→guardrail map (0 at/below neutral, linear, saturating at 1.0) and the bounded
  `[l_sys, l_sys·(1+AMP)]` soft amplifier; `guardrail_collapse_is_live_in_compute_and_only_amplifies_the_likelihood`
  drives two engines on identical events differing only in regime (neutral → guardrail 0, product-3.0
  → guardrail 0.5) and proves `l_sys` scales by exactly `1+AMP·guardrail` (a revert to a bare
  literal or moving the term into the prior fails it).
- Notes / decisions future runs must respect: these are FITTED constants (the bands depend on them) —
  do NOT blind-tweak; move only with evidence + a test. The guardrail enters ONLY the systemic
  likelihood, never the flat prior (Step 7) — keep it that way. The live coupler being saturated at
  full collapse is a recorded finding, not a bug to "fix" by changing the SPAN. Remaining un-pinned
  for 1.2: the `gp_bonus` `0.12` in `score_all` (a DIFFERENT 0.12 — the great-power scoring bonus,
  not the guardrail amplifier) and the regime × factor defaults (config surface, labeled
  `RegimeFactor`s, not blind literals).

## 2026-06-10 — legibility — critical/elevated alert-band reference lines on the timeline, sourced from live thresholds (roadmap 2.4)
- Item: roadmap 2.4 (new, now checked) — the "threshold marker on the hero/timeline for the critical
  P(WWIII) band" flagged as a natural sibling in the 2026-06-10 elevation-line entry. Legibility axis
  (pillar 2): reading the 2026-06-10 batch newest→oldest (awareness, honesty, robustness, legibility),
  legibility was advanced earliest of the four, i.e. least-recently — so this rotates coverage back
  onto it. The other open legibility items are 2.1 (small-viewport, eyes-judged — not provable in the
  cloud sandbox) and 2.3 (methodology completeness); this reference-line lever is the one provable green
  today.
- Verified-open-first: the domain bar chart got its `elevLine` "elevated" reference on 2026-06-10, but
  the TIMELINE (`tlChart`, annual P(WWIII) over time — the hero trend) still had no alert-band reference
  at all. An operator watching the trend climb had nothing on the chart telling them where "elevated"
  (2.5%) or "critical" (8%) begins; that only showed up after the fact in the hero colour + alert bar.
  Worse, the dashboard HARDCODED those thresholds: `pc()` (hero/rail risk colour) and the activity-log
  colour both used bare `p>=.08`/`p>=.025` literals that could silently drift from the operator-tunable
  `AlertSettings` — a latent dishonesty (the chart/colour could disagree with the engine's actual alert
  classification).
- Change (one coherent change, model→engine→serializer→dashboard): (a) `RiskSnapshot` gains
  `alert_elevated_threshold` / `alert_critical_threshold`; (b) `bayesian::compute` Step 10 records the
  engine's configured `alert_elevated`/`alert_critical` onto the snapshot (so the JSON is
  self-describing about the band that classified it); (c) `aggregator::snapshot_to_json` serializes them
  under `alert.{elevated,critical}_threshold`; (d) the dashboard's new `alertBands` canvas plugin draws a
  dashed amber "elevated" + red "critical" line on `tlChart`, each only when its value falls inside the
  auto-scaled y-range (hidden at a quiet ~1-2% read — no clutter; surfaces as risk climbs toward the
  band — exactly when it matters); the thresholds come from live JS vars `ALERT_ELEV`/`ALERT_CRIT` that
  `applyData` adopts from `d.alert.*_threshold` each snapshot; (e) the drift-prone `.08`/`.025` literals
  in `pc()` and the activity-log colour now read those same live vars. Canvas-drawn for the same reason
  as `elevLine`/`calibBand`/`spikeMarks` (chartjs-plugin-annotation renders nothing under Chart.js v4 —
  the exact silent-invisible failure these plugins exist to avoid). NO model constant touched.
- Metric moved: test count 366 → 369 (3 new locks: `compute_records_the_configured_alert_thresholds_on_the_snapshot`,
  `snapshot_to_json_carries_live_alert_thresholds`, `dashboard_html_draws_alert_bands_from_live_thresholds`);
  new legibility capability (alert bands visible on the hero trend) + a latent drift hazard (hardcoded
  alert thresholds in the dashboard) removed. Calibration evidence UNCHANGED — backtest 9/9 green
  (quiet/Ukraine/current/Cuba bands + evidence), no calibration constant touched.
- Proof: `cargo build --release` clean; `cargo clippy --release` 0 warnings; `cargo test --release` =
  368 passed / 0 failed / 3 ignored (369th is the scorecard grep count incl. the new tests);
  `cargo test backtest` = 9 passed. The engine lock builds an engine with NON-default thresholds
  (0.03/0.11) and proves the snapshot carries exactly those (not a constant); the JSON lock proves the
  serializer echoes the snapshot's own thresholds verbatim; the dashboard lock proves `pc()` reads the
  live `ALERT_CRIT`/`ALERT_ELEV` and that the bare `p>=.08?...:p>=.025` literal is GONE — a revert to the
  hardcoded threshold fails it.
- Notes / decisions future runs must respect: the alert-band thresholds are now LIVE on the snapshot —
  the dashboard must keep sourcing them from `d.alert.*_threshold` (never re-hardcode `.08`/`.025`). The
  timeline lines intentionally hide when the threshold is off the auto-scaled y-range (don't force the
  y-axis to include 8% — that would crush the live ~1-2% signal into the chart floor and HURT
  legibility). Eyes will judge this render at deploy: the lines are thin/dashed/labeled and only drawn
  when their pixel falls inside the chart area, so they won't clip or saturate. Natural sibling still
  open: surfacing these same band thresholds in the methodology prose (2.3) so the `≥8% critical` text
  there is templated from `AlertSettings` rather than a hand-typed `<code>≥8%</code>`.

## 2026-06-10 — awareness — per-theater "what is heating up": rising-driver (delta-driver) on the ladder (roadmap 3.3 extension)
- Item: roadmap 3.3 (extended — the delta-driver the original entry explicitly flagged as a future
  extension). Awareness axis (pillar 3): it was the LEAST-recently-advanced axis (last advanced
  2026-06-09; honesty/legibility/robustness all advanced 2026-06-10), so this rotates coverage back
  onto it. The only other open awareness item (3.2 GDELT) needs live-network verification the cloud
  sandbox can't do — so the honest provable-green awareness lever today is this delta-driver.
- Verified-open-first: `TheaterState` carried `top_driver` (dominant weighted LEVEL) but nothing
  about CHANGE — the engine kept only `prev_heat` (aggregate), not per-modality history, so there
  was no way to say *which force is driving a rise*. The chip showed a ▲ arrow (direction) and the
  dominant-level driver, which can MISLEAD: a theater hottest on nuclear posture can be rising purely
  because military escalation just jumped — the ▲ + "Nuclear" reads as "nuclear is escalating" when
  it isn't. Real awareness gap, directly on the mission's "where risk is concentrating, early enough
  to act" leg.
- Change (one coherent change across model + engine + dashboard): (a) added
  `TheaterEngine.prev_scores: HashMap<theater_id, HashMap<modality_id, score>>` — previous-tick raw
  modality scores, kept separate from `prev_heat` because the rising driver is about *which modality
  moved*; (b) in `score_theater`, compute `rising_driver` = the modality with the largest POSITIVE
  `(now−was) × domain_weight` change, populated ONLY when the theater's overall `trend == "rising"`
  (a flat/cooling theater names nothing); record this tick's scores into `prev_scores`; the
  de-escalation (empty-events) branch resets `prev_scores` to a clean baseline and names nothing;
  (c) new `TheaterState.rising_driver` field (serialized → snapshot JSON automatically); (d)
  dashboard ladder chip surfaces it as "↑ X" beside the rising arrow (reusing `domainLabel`) + a
  "rising on X" tooltip clause, keeping the signal count visible. Honest by construction — it names
  the model's own largest positive weighted delta term, never a fitted value. NO calibration constant
  touched.
- Metric moved: test count 365 → 366 (new
  `rising_driver_names_the_modality_that_moved_not_the_dominant_level`); new awareness capability
  (per-theater delta-driver). Calibration evidence UNCHANGED (backtest 9/9 green, no model constant
  touched).
- Proof: `cargo build --release` clean; `cargo clippy --release` 0 warnings; `cargo test --release`
  = 365 passed / 0 failed / 3 ignored (366th is the scorecard grep count incl. the new test);
  `cargo test backtest` = 9 passed (quiet/Ukraine/current/Cuba bands + evidence). The lock drives
  TWO ticks on one engine: tick 1 (nuclear-only, rising from zero) → top_driver == rising_driver ==
  nuclear; tick 2 (hold nuclear, spike military) → top_driver stays nuclear (dominant level) but
  rising_driver == military_escalation (the mover) — the exact honesty distinction; tick 3 (identical
  → flat) → no rising_driver; quiet world → none. A revert to naming the level instead of the delta
  would flip tick 2 and fail.
- Notes / decisions future runs must respect: `rising_driver` is gated on `trend == "rising"` and is
  the largest positive weighted DELTA — do not conflate it with `top_driver` (the dominant level);
  both are honest read-outs of existing scores, not new model levers. The chip reuses `domainLabel`
  (no new label table). Natural remaining awareness extension (still open under 3.3): a 2nd LEVEL
  contributor on the chip. 3.2 (GDELT) remains gated on live-network verification — not for the cloud
  routine.

## 2026-06-10 — honesty/model — named the systemic coupler weights + breadth asymptote and locked "breadth never swamps the brink" (roadmap 1.2)
- Item: roadmap 1.2 (progressed — the systemic coupler weights + breadth asymptote, the bulk of the
  remaining un-pinned fitted constants, are now named + rationale'd + relationship-locked). Honesty
  axis (pillar 1): rotates back onto the model's calibration provenance after two days on
  robustness/legibility.
- Verified-open-first: the five most calibration-critical amplifier constants in `theater.rs::compute`
  were still bare literals — `0.45` (GP entanglement weight), `0.30` (alliance weight), `3.0` (GP
  saturation count), `0.26`/`1.7` (breadth asymptote + e-fold), `0.70` (single-theater brink
  amplifier). And the single most important honesty property among them — the 2026-06-03 design intent
  that *saturating breadth must never let a no-brink multi-theater world out-amplify the single-theater
  nuclear brink* (the regression that once drove a four-theater world above the Cuba apex and pegged
  P(WWIII) flat at 0.90) — lived ONLY in a code comment, with no test guarding it.
- Change (one coherent change, `theater.rs` only): (a) named all five literals — `COUPLING_GP_WEIGHT`,
  `COUPLING_ALLIANCE_WEIGHT`, `GP_ENTANGLEMENT_SATURATION`, `BREADTH_ASYMPTOTE`, `BREADTH_EFOLD`,
  `BRINK_AMPLIFIER` — each with a rationale comment, and used them in `compute` (gp_entanglement,
  coupling_multiplier, concurrency_mult, brink_mult); (b) added `breadth_never_swamps_the_nuclear_brink`,
  which locks the design intent two complementary ways: a STRUCTURAL guarantee `BRINK_AMPLIFIER >
  BREADTH_ASYMPTOTE` (survives any recalibration), and a BEHAVIOURAL bound driving the live engine with
  1..=5 identical conventional (no-GP, no-nuclear) hot theaters so max_heat/coupling are held constant
  and the l_sys ratio IS the breadth amplifier — proving it is 1.0 at one theater (no breadth bonus),
  monotone in theater count, and strictly below `1+BREADTH_ASYMPTOTE` (hence strictly below the
  `1+BRINK_AMPLIFIER` apex) no matter how many theaters are hot. NO value changed.
- Metric moved: test count 364 → 365 by the scorecard grep (new `breadth_never_swamps_the_nuclear_brink`);
  a previously prose-only honesty relationship + five unpinned fitted constants now named and locked.
  Calibration evidence UNCHANGED — Brier ~2e-6, RMSE 0.14pp, in-band 4/4, all four anchors bit-identical
  to baseline (quiet 2.03 / ukraine 38.84 / current_2026 60.10 / cuba 79.80) — proof this was pure
  naming + a relationship lock, not a tuning.
- Proof: `cargo build --release` clean; `cargo clippy --release` 0 warnings; `cargo test --release` =
  364 passed / 0 failed / 3 ignored; `cargo test theater::` = 19 passed; `cargo test backtest` = 9
  passed (quiet/Ukraine/current/Cuba bands + evidence). The lock uses `#[allow(clippy::assertions_on_constants)]`
  on the structural inequality (same precedent as the 1.3 invariant locks).
- Notes / decisions future runs must respect: these six are FITTED constants (backtest bands) — do NOT
  blind-tweak; move only with evidence + a test, and keep `BRINK_AMPLIFIER > BREADTH_ASYMPTOTE` intact
  (it is the structural guarantee against breadth swamping the brink). 1.2 still has small remainders:
  the guardrail-coupler magic in `bayesian.rs::compute` (the `/4.0` regime→guardrail normalization and
  the `0.12` guardrail amplifier) — a natural next 1.2 sibling. P₀ = `BASELINE_ANNUAL` is already
  named + const-asserted; the regime × factor defaults are labeled `RegimeFactor`s (config surface, not
  blind literals).

## 2026-06-10 — robustness — shutdown made cancellation-aware under worker-pool saturation (roadmap 4.3)
- Item: roadmap 4.3 (now checked). First real advance on the Robustness axis (pillar 4): prior
  4.x activity was only the 4.1 *correction* (no code), so robustness was the least-recently-
  advanced axis — this rotates coverage onto it. Serves the mission's "real-time" leg: a service
  that can't shut down promptly can't be redeployed promptly, and the deploy wrapper rolls back on
  a stuck health gate.
- Verified the claim first (the roadmap demands it): in `nlp_sidecar.rs::run` the LLM-dispatch
  permit was a bare `sem.clone().acquire_owned().await` sitting *inside* the `select!` recv arm.
  Once `select!` picks the recv branch and runs its body, its `.await`s do NOT re-poll the other
  branches — so while the pool is saturated (all `concurrency` permits held by in-flight ~6s LLM
  classifications) that await blocks and the shutdown branch is never polled. A SIGTERM during
  sustained load therefore stalled until a permit freed: bounded to one classify in the normal
  case, but unbounded if Ollama hangs. Real, not theoretical.
- Change (one coherent change, `nlp_sidecar.rs` only): extracted `acquire_permit_or_shutdown`,
  which races `sem.acquire_owned()` against a clone of the shutdown `watch::Receiver`
  (`biased` toward shutdown, plus an already-signalled fast-path) and returns
  `PermitWait::{Acquired,Shutdown,Closed}`. The recv arm now matches that: `Acquired` →
  dispatch as before (real backpressure preserved — the permit still gates the pool),
  `Shutdown` → flush + exit immediately instead of blocking, `Closed` → break. A clone of the
  receiver is used because the main `select!` already holds `&mut self.shutdown_rx` for its own
  shutdown branch (the clone shares the value, has an independent seen-version). Both
  graceful-exit paths (idle shutdown arm + saturated-dispatch shutdown) now call one
  `save_and_log_shutdown` helper so the save/log can't drift. NO change to `llm.concurrency` or
  the pool size — only the *wait* is now cancellation-aware (respects 4.1).
- Metric moved: test count 359 → 363 (4 new `permit_wait_*` tests); new robustness capability
  (cancellation-aware shutdown under backpressure). No model constant touched — calibration
  bands and systemic invariants untouched.
- Proof: `cargo build --release` clean; `cargo clippy --release` 0 warnings; `cargo test --release`
  = 363 passed / 0 failed / 3 ignored; `cargo test backtest` = 9 passed (quiet/Ukraine/current/
  Cuba bands + evidence green). The regression lock `permit_wait_cancels_on_shutdown_while_pool_
  saturated` holds the only permit forever and asserts the wait still returns `Shutdown` within a
  2s timeout — a revert to a bare `acquire_owned().await` would hang and fail it.
- Notes / decisions future runs must respect: do NOT move the permit acquire back to a bare
  `acquire_owned().await` in the recv arm (re-introduces the stall), and do NOT raise
  `llm.concurrency` (4.1 — hardware-calibrated to 2). Sibling open robustness item still 4.2
  (`unwrap()/expect()` audit) — note from this run: the production `unwrap()/expect()` sites are
  largely infallible-by-construction (e.g. `detector::nearest_test_site`'s `partial_cmp().unwrap()`
  is safe because NaN distances are filtered out before `min_by`) or startup-time `expect`s;
  4.2 has no clean must-fix target right now, so verify each candidate's error path is reachable
  before "fixing" it.

## 2026-06-10 — legibility/awareness — domain chart "elevated" threshold reference line (canvas plugin, model-templated)
- Item: roadmap 2.2 (now checked — annotation render audit). First advance on the Legibility
  axis (pillar 2): the whole prior log sat on honesty/model + awareness; legibility was the
  least-recently-touched axis, so this rotates coverage onto it.
- Audit result: enumerated every Chart.js instance — only two (`tlChart` timeline, `dmChart`
  domain bar) — plus the methodology page (no charts). No annotation-plugin overlay remained
  silently invisible; `calibBand` and `spikeMarks` were already canvas plugins (their
  `chartjs-plugin-annotation`-renders-nothing comments are historical, not live). So 2.2's
  literal target was exhausted — but the audit surfaced the real gap it points at.
- Change: the domain bar chart (`dmChart`, 5 force domains scored 0–1) had NO reference for the
  model's `ELEVATION_THRESHOLD` (0.32) — the score at/above which a domain is "elevated" and
  feeds the co-occurrence amplifier. An operator had to mentally hold 0.32 to read which bars
  mattered. Added the `elevLine` canvas plugin (`afterDatasetsDraw`): a dashed amber line at the
  threshold with an "elevated" label, drawn via `chart.scales.y.getPixelForValue`. Wired into
  `dmChart`'s `plugins:[elevLine]`. Critically for HONESTY, the value is NOT a hand-typed JS
  literal that could drift: `const ELEV_THRESH={{ELEVATION_THRESHOLD}}` is substituted in
  `server.rs::generate_dashboard_html` from `models::ELEVATION_THRESHOLD` (same anti-drift
  template pattern as `{{FORECAST_PROB_CEILING}}`). Canvas-drawn deliberately — a naive
  `chartjs-plugin-annotation` line would be silently invisible under Chart.js v4, the exact
  failure 2.2 exists to prevent.
- Metric moved: test count 358 → 359 (new `dashboard_html_renders_elevation_threshold_from_model`);
  new legibility/awareness capability (the operator now sees the elevation cutoff against the live
  domain bars). No model constant touched — `ELEVATION_THRESHOLD` is unchanged at 0.32, just
  surfaced; calibration bands and systemic invariants untouched.
- Proof: `cargo build --release` clean; `cargo clippy --release` 0 warnings; `cargo test --release`
  = 359 passed / 0 failed / 3 ignored; `cargo test backtest` = 9 passed (quiet/Ukraine/current/Cuba
  bands + evidence all green). The lock test proves the rendered JS embeds exactly
  `const ELEV_THRESH=0.32` (= `models::ELEVATION_THRESHOLD`) and that the placeholder is gone after
  render — so the line can never lie about where "elevated" begins.
- Notes / decisions future runs must respect: the elevation line's value is TEMPLATED from the
  model — edit `models::ELEVATION_THRESHOLD`, never the dashboard literal (the placeholder, not a
  number, lives in dashboard.html). A natural sibling legibility lever: a threshold marker on the
  hero/timeline for the critical P(WWIII) band (≥8%), drawn the same canvas-plugin way. The eyes
  gate will judge this render at deploy — the line is thin/dashed/labeled and only drawn when the
  threshold pixel falls inside the chart area, so it won't clip or saturate.

## 2026-06-09 — awareness — feed-roster liveness guard + first audit (2 dead feeds replaced, 1 URL fixed)
- Item: roadmap 3.1 (now checked).
- Change: added two `#[ignore]`d live-network tests to `src/ingestor.rs`.
  `feed_roster_liveness` probes EVERY `RSS_FEEDS` entry end-to-end (HTTP 200 + feed-rs
  parse + ≥1 entry — exactly what `fetch_rss_feed` needs to succeed): concurrent first
  pass over all 103, then a serial retry of failures after a 30s pause so a minute-scale
  edge incident or probe-induced throttle doesn't read as dead; HTTP 429 counts as ALIVE
  (the host is answering — prod polls from this same IP and compounds throttling).
  `search_api_liveness` probes the GNews search-RSS and GDELT doc API the same way (GDELT
  429 likewise = alive). Run deliberately: `cargo test --release feed_roster_liveness --
  --ignored --nocapture`. Runtime `SourceHealth` self-heals transient outages but cannot
  tell an operator "this feed has been dead for a month" — this can, and names them.
- The first audit immediately found real rot:
  - **breakingdefense** (T1) + **nationalinterest** (T2): hard-403 both passes — the
    Cloudflare bot-fight pattern (jamestown/longwarjournal precedent), unfixable by UA.
    Replaced with **defensescoop** (T1, same daily Pentagon/defense-tech beat) and
    **lowy_interpreter** (T2, same IR/strategy commentary niche) — both probed 200 + valid
    RSS with entries. Tier counts unchanged (33/70).
  - **cbc**: the `cmlink/rss-world` endpoint was retired (301 → webfeed, which served an
    empty 0-item shell during the audit window) — moved to the canonical
    `webfeed/rss/rss-world` URL, now consistently 20 items.
  - **anadolu**: 502 during part of the audit but confirmed live minutes earlier (and 82
    articles in prod's current window) — a transient edge incident, NOT dead; the
    30s-delayed retry pass exists exactly for this class. Watch, don't replace.
- Metric moved: scorecard "Feed liveness" — *unmeasured* → **measured by command**: 102/103
  at audit close (the one red is anadolu's transient incident above — it ingested 27
  articles the same day, last 16:24Z; mid-audit runs read 103/103); test count +2 ignored
  runtime tests (the scorecard grep misses `#[tokio::test(flavor=…)]` forms).
- Proof: `cargo test --release` green (the two probes are `#[ignore]`d, suite unaffected);
  `feed_roster_liveness` printed 103/103 on three consecutive mid-audit runs (anadolu's
  502 window opened during the audit); `search_api_liveness` green (GNews 100 entries,
  GDELT alive-by-429).
- Notes future runs must respect: these probes are LIVE-NETWORK and `#[ignore]`d — they are
  for deliberate local audits, NOT the cloud routine (its sandbox can't reach these hosts;
  a red there means nothing). Do not un-ignore them or wire them into the deploy gate
  blindly — a transient upstream outage must not block a deploy. A feed that fails the
  audit persistently across hours is dead: fix or replace it (same niche, probe before
  committing), never delete-without-replacement and never leave it silently broken.

## 2026-06-09 — honesty/model — named + pinned the P(WWIII) forecast ceiling (was a bare literal next to stale 0.85 comments)
- Item: roadmap 1.2 (progressed — another magic calibration constant named/pinned; regime ×, P₀,
  breadth asymptote, coupler weights still open).
- Change: the hard clamp that enforces the model's core honesty property — "never emit
  near-certainty" — was a bare `.min(0.90)` literal in `bayesian.rs::compute`, and worse, the doc
  comments directly above it (lines ~523, ~543) STILL said `0.85` / `.min(0.85)`: code and its own
  documentation actively contradicted each other (the ceiling was raised 0.85→0.90 in v2 but the
  prose was never updated — the exact stale-doc hazard that produced the stale calibration table
  1.1b fixed). Fixed in one coherent change: (a) defined `models::FORECAST_PROB_CEILING = 0.90`
  with a full rationale (engineering ceiling, not a probabilistic prior — the model has no ground
  truth; the apex-scenario ceiling is Robert's design call) as the SINGLE source of truth, and
  noted it is distinct from `FORECAST_INDEX_CEILING` (95, the display index); (b) used it in the
  computation; (c) rewrote the stale 0.85 doc comments to reference the named constant; (d) rendered
  it into the methodology page via a `{{FORECAST_PROB_CEILING}}` placeholder (server.rs substitution,
  same mechanism as `{{BASE_PATH}}`/`{{CALIBRATION_EVIDENCE}}`) so the operator-facing prose is
  computed from the model and can never drift. NO value changed — 0.90 stays 0.90; this names and
  locks what was already there.
- Metric moved: test count 357 → 359 (`forecast_prob_ceiling_is_the_named_honesty_clamp` +
  `methodology_renders_forecast_ceiling_from_the_model_constant`); a previously-unpinned honesty
  constant + its operator-facing prose now locked, and a live code/doc contradiction removed. No
  model constant CHANGED — calibration bands and systemic invariants untouched.
- Proof: `cargo build --release` clean; `cargo clippy --release` 0 warnings; `cargo test --release`
  = 358 passed / 0 failed / 1 ignored; `cargo test backtest` = 9 passed (quiet/Ukraine/current/Cuba
  bands + evidence all green). The new bayesian test proves the clamp is LIVE: a saturating
  likelihood (l_sys=100) gives unclamped sigmoid > 0.90, and `.min(FORECAST_PROB_CEILING)` pulls it
  to exactly 0.90 — so the clamp is not vestigial — while an apex real-engine world stays ≤ ceiling.
- Notes / decisions future runs must respect: `FORECAST_PROB_CEILING` (0.90) is the P(WWIII) clamp;
  `FORECAST_INDEX_CEILING` (95) is the systemic INDEX clamp — two different ceilings, don't conflate.
  Do NOT raise either toward certainty/100. The methodology ceiling prose is now templated; edit the
  CONSTANT, not the HTML. Note discovered: an apex per-domain test world only reaches ~0.149 — the
  0.90 clamp only binds at extreme systemic l_sys (nuclear brink × multi-theater), which is why the
  live-clamp proof drives the formula directly rather than relying on a synthetic event pile.

## 2026-06-09 — honesty/model — locked the "quiet theater never leaks into the systemic amplifiers" invariant
- Item: roadmap 1.2 (progressed, not fully checked — one constant named/pinned; others remain).
- Change: the systemic engine amplifies the headline via three couplers — concurrency
  (`smoothstep(heat, HOT_HEAT−HOT_RAMP, HOT_HEAT+HOT_RAMP)` summed over theaters),
  great-power entanglement and alliance activation (both gated on `heat ≥ HOT_HEAT`). The
  honesty property that a STABLE theater (heat below the Stable-rung floor) contributes
  EXACTLY ZERO to all three was true but UNGUARDED: the floor was a bare `0.06` literal in
  `rung_for`, and nothing stopped a future recalibration from widening `HOT_RAMP` or lowering
  `HOT_HEAT` until the ramp's lower edge (today 0.12) dipped to/below it — at which point a
  quiet world would silently inflate the systemic index with nothing to catch it. Fixed two
  ways in one coherent change: (a) named the floor `STABLE_HEAT_CEILING = 0.06` with a
  rationale and used it in both `rung_for` and the headline driver-text gate (provenance —
  roadmap 1.2); (b) added `quiet_theater_never_leaks_into_couplers`, which asserts the
  RELATIONSHIP `HOT_HEAT − HOT_RAMP > STABLE_HEAT_CEILING` and `HOT_HEAT > STABLE_HEAT_CEILING`,
  and that `smoothstep` returns 0 across the entire Stable band [0, 0.06]. Also strengthened
  `concurrency_raises_likelihood` with a behavioral lock: a world with one fully-hot theater
  (the other four eventless → Stable) yields concurrency EXACTLY 1.0 — proving the four quiet
  theaters leak nothing.
- Metric moved: test count 356 → 357 (new `quiet_theater_never_leaks_into_couplers` + a
  hardened assertion in `concurrency_raises_likelihood`); a previously-unguarded honesty
  invariant now locked. No model constant CHANGED — `STABLE_HEAT_CEILING` is the same 0.06,
  just named; so the calibration bands and the systemic invariants are untouched.
- Proof: `cargo build --release` clean; `cargo clippy --release` 0 warnings; `cargo test
  --release` = 356 passed / 0 failed / 1 ignored; `cargo test backtest` = 9 passed (bands
  quiet/Ukraine/current/Cuba + evidence all green); `cargo test theater::` = 18 passed.
- Notes for future runs: this is a RELATIONSHIP lock (like the 1.3 invariants), deliberately
  NOT a magnitude freeze — it survives legitimate ramp recalibration but trips a regression that
  would let stable theaters leak amplification. 1.2 is still OPEN for the regime ×, P₀, breadth
  asymptote and coupler-weight constants. A natural sibling: the same no-leak property for the
  intra-theater co-occurrence ramp (`ELEV_RAMP` around `ELEVATION_THRESHOLD`) — a sub-threshold
  modality must contribute 0 co-occurrence; not yet locked.

## 2026-06-09 — awareness — per-theater "why": dominant weighted-heat driver
- Item: roadmap 3.3 (now checked). First advance on the Awareness axis (pillar 3, previously
  the least-developed and least-recently-touched — prior runs all sat on honesty/model).
- Change: each `TheaterState` now carries `top_driver` — the modality id (e.g.
  `nuclear_posture`) with the largest WEIGHTED contribution (`score × domain_weight`) to that
  theater's heat. It is computed in `theater.rs::score_theater` as the single biggest term in
  the same sum that builds `heat`, so it is honest by construction (the model's own dominant
  signal, never a fitted/derived value); empty for a Stable theater where no signal is worth
  naming. Surfaced in the theater-ladder chips: the sub-line reads "X% heat · Nuclear" and the
  tooltip gains "· driven by …", reusing the existing dashboard `domainLabel` map (no new label
  table). Previously the chips showed only HOW MUCH (heat %, signal count, rung) — never WHY;
  the operator had to mentally apply DOMAIN_WEIGHTS to know what kind of force was driving a
  flashpoint. Now the "where & why" is one glance.
- Metric moved: test count 355 → 356 (new `top_driver_names_the_dominant_weighted_modality`);
  new awareness capability (per-theater driver) on the snapshot + dashboard. No model constant
  touched — `top_driver` is a read-out of existing heat terms, so calibration/backtest bands and
  the systemic invariants are untouched.
- Proof: `cargo build --release` clean (0 warnings); `cargo test --release` = 356 passed / 0
  failed / 1 ignored. The lock test asserts: an only-kinetic theater → `military_escalation`;
  equal-score kinetic+nuclear → `nuclear_posture` (proves the heavier 3.0 weight wins, i.e. it's
  the WEIGHTED term not the raw score); a quiet world → every `top_driver` empty.
- Notes for future runs: `top_driver` is the single largest weighted term — a natural extension
  is a 2nd contributor or a "what changed this tick" delta-driver. The gate is `rung == Stable`
  → empty; if a future change forces a rung above Stable with ~0 heat, top_driver may be empty
  (filter drops zero-contribution terms) — that's intended (nothing to name).

## 2026-06-09 — honesty/model — locked systemic cross-check invariants (monotonicity, bounds, ceiling)
- Item: roadmap 1.3 (now checked).
- Change: added 5 invariant tests to `src/theater.rs` that pin the systemic engine's core
  honesty properties — previously UNGUARDED, so a future calibration tweak could have silently
  broken monotonicity or let the headline exceed the 95 forecast ceiling (a dishonest number)
  with nothing to catch it. The tests assert RELATIONSHIPS the model must always satisfy, not
  fitted magnitudes, so they lock behaviour without freezing the calibration:
  1. `systemic_outputs_stay_bounded_over_many_worlds` — 400-world deterministic LCG fuzz:
     systemic_index ∈ [0, FORECAST_INDEX_CEILING], l_sys ≥ 0, every theater heat ∈ [0,1],
     delta ∈ [-1,1], couplers (gp_entanglement/alliance ∈ [0,1], concurrency ≤ 5,
     coupling_multiplier ≥ 1).
  2. `adding_a_hot_theater_never_lowers_systemic_outputs` — systemic-level monotonicity: a
     second hot theater never lowers the index and strictly raises l_sys.
  3. `adding_a_modality_never_cools_a_theater_or_the_index` — intra-theater monotonicity over a
     strict superset of hot modalities (robust because bayesian::score_all is per-domain with a
     fresh per-call scorer, so added modalities only add positive weighted terms + raise cooc).
  4. `de_escalation_lowers_the_systemic_index` — hot→quiet drops index (<1.0) and l_sys.
  5. `systemic_rung_pegs_index_at_forecast_ceiling_not_100` — apex nuclear-use rung (raw ladder
     100) clamps to exactly 95; locks the ceiling clamp to the actual apex output so the
     headline can never print certainty (100) on a news-inferred detonation.
- Metric moved: test count 350 → 355 (5 new locked invariants); new "frontier" rows for the
  monotonicity/boundedness/ceiling properties. No model constant touched — pure honesty locks.
- Proof: `cargo build --release` clean (0 warnings); `cargo test --release` = 354 passed / 0
  failed / 1 ignored. `cargo test theater::` = 16 passed.
- Notes: these are invariant (relational) tests, deliberately NOT magnitude gates — they survive
  legitimate live-targeted recalibration but trip on a sign/clamp/monotonicity regression.

## 2026-06-09 — legibility/honesty — surfaced live calibration evidence on the methodology page
- Item: roadmap 1.1b (now checked).
- Change: un-gated `mod backtest` (was `#[cfg(test)]`) and added `pub calibration_evidence_html()`,
  which renders the live per-analog table (model P vs anchor + Δ) and the aggregate Brier/RMSE/
  in-band. `server.rs` substitutes it into a new `{{CALIBRATION_EVIDENCE}}` placeholder in
  methodology.html at startup (mirrors `{{BASE_PATH}}`). Removed the hand-written calibration
  table — which had itself gone stale (it still showed ~65% for current-full) — so the page now
  shows numbers computed from the running model that cannot drift. Test-only helpers
  (`live_hot_2026`, `cross_entropy`) kept under `#[cfg(test)]` so the release binary stays clean.
- Metric moved: the calibration evidence is now ALSO operator-visible (not CI-only); test count
  349 → 350 (added `methodology_renders_live_calibration_evidence`). RUNTIME change this time
  (unlike 1.1/1.1a) — methodology page content changes; dashboard untouched.
- Proof: `cargo build --release` clean (no warnings); `cargo test --release` = 349 passed / 0
  failed / 1 ignored; the new test asserts the placeholder is substituted and Brier/in-band render.
- Notes: deploy gate is build+health+eyes (eyes covers the dashboard, which is unchanged); the
  readout runs at `ServerState::new` (4 deterministic scenario sims, negligible startup cost).

## 2026-06-09 — honesty/model — re-anchored current_2026 65%→60% (resolves the calibration gap)
- Item: roadmap 1.1a (now checked).
- Decision (Robert): the −4.9pp `current_2026` gap surfaced by the evidence harness was a STALE
  ANCHOR, not a model error. Mechanism analysis (`theater.rs` `concurrency_mult` + per-scenario
  l_sys): raising the model to the old 65% centre means lifting the breadth-saturation asymptote
  ~0.26→~0.34, which also pushes the REAL live read ~82%→~85-86% — eroding the off-the-0.90-peg
  resolution the 2026-06-03 saturating-breadth fix deliberately created. The saturation curve is
  monotonic, so no lever isolates current_2026 (breadth ~2) from the live read (breadth ~3);
  raising the model would partially REVERT that deliberate fix. Correct fix = reconcile the stale
  65% centre to the 60% the model produces by design (brink dominates breadth).
- Change: `src/backtest.rs` — current_2026 anchor centre 0.65→0.60 + reconciled the stale
  header/test comments (incl. dropping a pre-fix "live corpus ~45%" note). Band acceptance range
  left exactly as Robert set it (0.55–0.75). NO model constant touched.
- Metric moved: Brier 0.00060 → ~0.000002; RMSE 2.45pp → 0.14pp; in-band 4/4 (all anchors now
  within 0.2pp of centre).
- Proof: `cargo test --release` = 348 passed / 0 failed / 1 ignored. current_2026 60.10% vs
  60.0% centre (+0.10pp).
- Notes future runs MUST respect: current-full's intended centre is **~60% (NOT 65%)**. Do NOT
  "raise current_2026 to 65%" — it re-erodes the live-peg headroom. The live read itself is a
  separate question from this synthetic anchor.

## 2026-06-09 — honesty/model — calibration evidence harness (Brier/cross-entropy vs anchored centres)
- Item: roadmap 1.1 (now checked); spawned 1.1a + 1.1b.
- Change: added a proper-scoring calibration harness to `src/backtest.rs`. It scores the live
  model's P(WWIII) for the four hard-band analogs against Robert's expert-anchored band
  CENTRES (2 / 39 / 65 / 80 %) using Brier + cross-entropy, printed via `cargo test
  calibration_evidence_report -- --nocapture` and locked by 3 new tests (Brier math,
  cross-entropy math + clamping, and the in-band invariant). Deliberately NOT a
  tighter-than-band gate — that would fight legitimate live-targeted recalibration; it is
  evidence that the number is earned. No model behavior changed (no calibration constant touched).
- Metric moved: scorecard "Calibration evidence" *not measured* → **Brier 0.00060 / RMSE
  2.45pp / in-band 4/4**; test count **346 → 349**.
- Proof: `cargo test --release` = **348 passed / 0 failed / 1 ignored**. Evidence table:
  quiet 2.03% (+0.03pp), ukraine 38.84% (−0.16pp), current_2026 60.10% (−4.90pp),
  cuba 79.80% (−0.20pp).
- FINDING for future runs: **current_2026 is the calibration soft spot** (−4.9pp; it alone
  drives the RMSE — the other three anchors are within 0.2pp of centre). Captured as roadmap
  1.1a. This is the kind of thing the wide bands hid and the evidence number surfaces.
- Notes: harness is test-only (`backtest` is `#[cfg(test)]`); to surface it at runtime see 1.1b.

## 2026-06-09 — meta — installed the compounding self-improvement infrastructure
- Item: ad-hoc (program upgrade)
- Change: added `docs/roadmap.md` (prioritized, axis-organized backlog), `docs/scorecard.md`
  (the fitness function + prime directive), and this log. The routine now pulls from a shared
  backlog and records measured progress, instead of cold-starting and guessing each run. The
  cloud routine prompt was rewritten the same day to be mission-driven (honesty/legibility/
  awareness) with the safety rails intact, and to read+maintain these three files.
- Metric moved: new frontier — established the scorecard baseline (test count 346; build/
  tests/calibration-bands green; index ceiling 95).
- Proof: `cargo build --release` green (no-op rebuild, live binary already on `main`).
- Notes: axes rotate — bias each run toward the least-recently-advanced axis.

## 2026-06-09 — robustness — CORRECTION: the LLM enricher is already optimized (no rework)
- Item: roadmap 4.1
- Change: NONE — investigated and confirmed the "serial per-article enricher" is already a
  bounded-concurrent worker pool (`nlp_sidecar.rs`: `Semaphore` + `acquire_owned()` +
  `tokio::spawn`; the old serial `classify().await` is gone and the code comment says so).
  A standing memory and an earlier plan still described it as serial and "in scope to rework";
  that belief was **stale**. Corrected the memory; recorded the resolution here and in the
  roadmap so no future run re-chases it.
- Metric moved: none (invariant held) — but prevented future wasted runs.
- Proof: `nlp_sidecar.rs:118-225` (semaphore dispatch); `settings.yml:67-70` documents
  `concurrency: 2` as a GTX-1080 VRAM calibration (above 2 doubles latency).
- Notes future runs MUST respect: **do NOT "make the enricher concurrent" or raise
  `llm.concurrency`** — it's done, and the cap of 2 is hardware-correct on this box. This
  entry exists specifically so the loop stops re-discovering a solved problem.
