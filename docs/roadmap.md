# GCRM Improvement Roadmap

The shared backlog the self-improvement routine pulls from. **Read this at the start of
every run.** Pick the highest-value UNCHECKED item you can do *well* today, implement it,
check it off, and append a proof entry to `improvement-log.md`. If you find a better lever,
ADD it here (don't silently drift). If an item turns out already-done or wrong, mark it and
move on — **verify the current code before assuming an item is still open.**

Items are tagged **[verified]** (confirmed real against the code) or **[candidate]** (a lead
worth investigating — confirm before acting). Axes are in mission-priority order; the axis a
run touches should rotate WITHIN the chosen value tier (read `improvement-log.md` to see which
axis is least-recently advanced and bias there, so coverage stays even) — rotation never
justifies dropping a tier (scorecard: the tie-break is within-tier only).

The mission, against which every item is judged: give an operator ONE honest, legible,
real-time read on how close the world is to systemic / great-power war, and *where* it's
concentrating. **Honesty > Legibility > Awareness**, then the enablers.

> **Read `scorecard.md` first — work is ranked by VALUE TIER, not provability.** Pick the highest
> tier you can do well today (T1 new source / gauge / theater / calibration / monitor-rung > T2
> first-time honesty surface > T3 annotation) and prove it with that tier's falsifiable gate.
>
> **CLOSED VEINS — forbidden to mine, no credit at any tier:** the I&W indicator-light BOARD —
> CLOSED at 12 lights of ANY class (per-modality, coupler, velocity, physical alike; 5/5 modalities
> — military / nuclear / economic / diplomatic / cyber, complete as of 3.16; the config-only
> "guardrails" light was RETIRED 2026-07-03 — do not re-add it; no run adds a light of any kind
> without Robert's explicit sign-off) — and the blind / thin / stale / capped / held / saturated /
> pegged caveat family (complete across header / board / hero / map / chip). These are DONE. Adding
> the Nth light or mirroring a caveat onto another surface is annotation inflation, not improvement.
> The open frontier is **§6 new signal · §7 platform · §8 monitors** below.

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
    (Values as of 2026-06 — the 2026-06-28 de-saturation refit `BREADTH_ASYMPTOTE` to 0.10;
    methodology templates from the constant, so operator prose is unaffected.)
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
  - PROGRESS 2026-06-12: fixed a **v1 display vestige on the dashboard** (same class as the dead
    `gp_bonus`, but on the operator surface). The model-state footer drew `structural-adjusted =
    baseline × regime` (≈8%) as a chain step toward P(WWIII|E), and the "how it's built" modal called
    the headline "a regime-adjusted prior … multiplied by a coupling likelihood" — the SUPERSEDED v1
    multiplicative form. The v2 engine uses a FLAT prior with the regime entering ONLY as the bounded
    guardrail-collapse amplifier on `l_sys` (locked by
    `guardrail_collapse_is_live_in_compute_and_only_amplifies_the_likelihood`). Rewrote both surfaces to
    the honest v2 chain (flat P₀ · systemic L · guardrail collapse · log-odds fold), replaced the unused
    `adjusted_prior` footer readout with a live `couplers.guardrail_collapse` readout, and locked it by
    `dashboard_explains_the_v2_flat_prior_not_the_v1_adjusted_prior`. No model constant touched. Sibling
    still open (2.3): the regime-factor INSPECTOR panel (dashboard.html:1120, `api.rs::regime_summary`)
    still labels `baseline × regime_product` as "Adjusted P₀" — honestly reframe as "structural pressure".
    **RESOLVED 2026-06-12 — see the 2.3 PROGRESS line below.**
  - PROGRESS 2026-06-14: pinned the **operator-facing "data quality" CONFIDENCE** (snapshot
    `estimate_confidence`, the dashboard Confidence cell). NOTE this is provenance on a DISPLAY metric,
    not a calibration constant — it does NOT enter the P(WWIII) forecast (computed in Step 9, after the
    forecast is final), so backtests are bit-identical. But the operator reads it as "how much evidence
    this number rests on", so per pillar-1 it must mean what it says. It was built from six bare inline
    literals in `bayesian.rs::compute` (`0.05` floor, `0.1` no-domain fallback, `200.0` event saturation,
    `20.0` source saturation, `0.5/0.3/0.2` blend weights) with NO rationale and only a `[0,1]` bounds
    assert. Named them all (`CONFIDENCE_OFFLINE_FLOOR`, `CONFIDENCE_NO_DOMAIN_CONF`,
    `CONFIDENCE_EVENT_SATURATION`, `CONFIDENCE_SOURCE_SATURATION`, `CONF_W_DOMAIN/EVENTS/SOURCES`) with a
    rationale each, added a compile-time `const _: () = assert!` that the weights partition unity (so a
    future re-weight can't silently push confidence > 1), and extracted the pure
    `estimate_confidence(avg_domain_conf, events, sources)` so the contract is lockable. Behavior-preserving
    (in-range inputs produce the identical value). Locked by
    `estimate_confidence_is_a_bounded_monotone_blend_with_an_offline_floor` (offline floor, `[0,1]` over a
    grid, monotone non-decreasing in events AND sources, full corroboration → exactly 1.0, log-saturation
    of the volume term so an event flood can't fake certainty). Remaining un-pinned for 1.2: the regime ×
    factor defaults (config surface, labeled `RegimeFactor`s, not blind literals). NB the per-DOMAIN
    confidence (`DomainScorer::score_all`, Step ~) still has its own inline literals (`15.0`, `3.0`, tier
    weights) — a future provenance leg.
  - PROGRESS 2026-06-17: closed the **operator-facing drift hazard** on the snapshot-confidence
    formula (sibling to the 2026-06-14 pin, on the DISPLAY side). The dashboard **Confidence** info-modal
    HAND-TYPED the blend (`×0.5 + ×0.3 + ×0.2`, "200 events", "20 feeds") — the very `CONF_W_*`/
    `CONFIDENCE_*_SATURATION` constants `estimate_confidence` blends — so a re-weight would leave the
    modal silently misexplaining the operator's own Confidence number. Templated all five
    (`{{CONF_W_*}}`/`{{CONFIDENCE_*_SAT}}`, substituted in `server.rs::generate_dashboard_html`, same
    anti-drift mechanism as `{{BASELINE_ANNUAL_PCT}}`). Behaviour bit-identical; no constant touched.
    Locked by `dashboard_renders_confidence_formula_from_the_model_constants`. Remaining un-pinned for
    1.2: the regime × factor defaults (config surface) + the per-DOMAIN confidence literals (`15.0`,
    `3.0`, tier weights in `score_all`).
  - PROGRESS 2026-06-18: purged the **last v1 "regime-adjusts-the-prior" vestige**, sibling of the dead
    `gp_bonus` (2026-06-11) and v1-footer (2026-06-12) removals. v2's prior is FLAT and the regime enters
    only via guardrail collapse on `l_sys`, yet three places still spoke the abandoned `P₀_adj = anchor ×
    regime` form: the served snapshot JSON `prior.adjusted_prior` (a precomputed product in the public
    contract — any consumer could rebuild the v1 misconception), the authoritative Bayesian formula
    docstring, and the `Step 1` comment + the dead `RiskSnapshot.adjusted_prior` field (computed/stored/
    served but NEVER read by the math). Removed the field everywhere, dropped it from the served JSON with
    an honest `regime_role` note, and rewrote both stale comments to the flat-prior v2 form. No constant
    touched (display/contract + docs only; backtest + Brier identical). Locked by
    `served_prior_is_v2_flat_not_a_v1_adjusted_prior`.
- [x] **1.3 Coupler / theater cross-checks** — **DONE 2026-06-09.** Added 5 invariant tests in
  `src/theater.rs` that LOCK the model's core honesty properties, none of which were guarded
  before: bounded outputs over a 400-world deterministic fuzz (index ∈ [0,95], l_sys ≥ 0, heat
  ∈ [0,1], couplers in range); systemic-level monotonicity (a second hot theater never lowers
  the index, raises l_sys); intra-theater monotonicity (a superset of hot modalities never cools
  a theater or the index); de-escalation actually lowers index+l_sys; and the apex (nuclear-use)
  Systemic rung pegging the index at exactly FORECAST_INDEX_CEILING (95), never 100. These pin
  RELATIONSHIPS the model must always satisfy, not fitted magnitudes — they don't freeze the
  calibration. See improvement-log 2026-06-09.
- [x] **1.4 Honest disclosure of the breadth-saturated read** — **DONE 2026-06-24.** The
  de-saturation thread (`backtest::live_pegged_*`, `52a657d`) measured that the live railed peg
  reads ~83.6% with ~0.0pp resolution: every breadth amplifier of `l_sys` is at its rail, so
  intensifying the current crises can't move the number. But that peg sits BELOW the 0.90 forecast
  ceiling, so `meta.at_ceiling` stays false and the operator saw a bare 83.6% that read as a
  still-climbing point estimate (a pillar-1 overstatement of precision). The de-saturation
  RECALIBRATION is Robert-gated (value-laden, moves fitted constants) — but the honest DISCLOSURE
  is not. Added `couplers.breadth_saturated` (theater.rs): a purely-structural flag — hottest heat
  clamped at the model max, gp-entanglement + alliance both railed, ≥2 hot theaters, no live
  nuclear brink. Surfaced in `meta.breadth_saturated` (sibling to `at_ceiling`) and the analyst
  brief (deterministic prose + LLM context), which now names it a structural-maximum read whose
  only remaining lever is a direct nuclear brink. NO fitted constant touched, P unchanged. Locked
  by `breadth_saturation_is_flagged_at_the_railed_peg_and_nowhere_in_the_resolved_bands`,
  `meta_mirrors_the_breadth_saturation_flag_from_the_couplers`, and
  `templated_brief_discloses_a_breadth_saturated_read_as_a_structural_maximum`. See
  improvement-log 2026-06-24.
  - PROGRESS 2026-06-25: completed the flagged follow-up — surfaced `meta.breadth_saturated`
    on the OPERATOR DASHBOARD hero (it was previously only in the analyst brief / served meta).
    A new `#gauge-saturated` caveat ("◆ structural max · breadth railed, only a nuclear brink
    raises it") shows beside the `gauge-cap`/`gauge-held` caveats, gated on `d.meta.breadth_saturated`
    — so a railed peg below the forecast ceiling no longer reads as a still-climbing point estimate
    at a glance. HTML/CSS done in-sandbox; final visual verdict is the local eyes gate. Locked by
    `dashboard_flags_a_breadth_saturated_read_as_a_structural_max` (server.rs). No model constant
    touched. See improvement-log 2026-06-25.
  - **DORMANT SINCE 2026-06-28 (the de-saturation) — dormant-by-design latent guard, operator
    decision 2026-07-03.** `heat_from_scores` now ends in `1 − exp(−γ·raw)` and asymptotes < 1.0, so
    the rail gate (`max_heat >= 1.0 − 1e-3`) can NEVER fire: `couplers.breadth_saturated` /
    `meta.breadth_saturated` / the `#gauge-saturated` hero caveat are permanently quiet today. That
    dormancy is INTENDED and test-locked
    (`backtest::live_peg_resolves_after_desaturation_and_no_band_is_breadth_saturated`; the
    theater.rs comment records the stance) — the flag stays as a latent guard should a future
    recalibration ever re-rail the heat curve. Do NOT re-enable, extend, or mirror it to another
    surface. Full RETIREMENT (flag + hero caveat + lock tests) remains Robert-gated — a creditable
    T2 under the scorecard's retirement lane once he signs off, not before.
- [x] **1.5 First-tick delta is a cold-start artifact** — **DONE 2026-06-27.** The per-snapshot
  `delta_annual`/`delta_30day` (dashboard `#cmd-risk-delta` "▲ +N% last snap", the ▲/▼ rate, the
  event log) were differenced against the engine's seed values (`prev_annual = HISTORICAL_ANCHOR`,
  `prev_30day = 0.0`) on the FIRST `compute()` after every (re)start — so the operator saw a
  fabricated jump (~+1.5pp annual, the full 30-day value) that never happened. Gated Step 8 on a
  new `has_prev_snapshot` flag: the first tick reports delta 0 (a stable "─"); the second tick on
  is a true inter-snapshot move. Engine-behavior, no calibration constant touched (P, backtest
  bands, Brier identical). Locked by
  `first_snapshot_after_restart_reports_zero_delta_not_a_cold_start_jump` (fails without the gate:
  first delta = 0.0149… ≠ 0). See improvement-log 2026-06-27.
- [x] **1.6 `systemic_pegged` "flat trend = pinned at ceiling" caveat was silently dead** —
  **DONE 2026-07-03.** The de-saturation (ae70552) rewrote `heat_from_scores` to end in
  `1 − exp(−γ·raw)` (asymptotes strictly below 1.0), so the `max_heat >= HEAT_CLAMP (1.0)` gate in
  `systemic_pegged` could never fire — the trend-cell honesty caveat that distinguishes a genuinely
  ceiling-pinned "+0.000%" from a calm flat line became unreachable, and the `HEAT_CLAMP` doc still
  falsely claimed `heat_from_scores` "ends in `.min(1.0)`". Re-keyed the flag off the model's ACTUAL
  ceiling — the headline P clamped at `FORECAST_PROB_CEILING` (`bayesian::is_at_forecast_ceiling`) AND
  an empirically flat window — the faithful realization of the caveat's own stated purpose. Distinct
  from the hero `at_ceiling` caveat (pegged additionally requires the window to be flat, so it answers
  the trend cell's question: is this +0.000% informative or just pinned?). No math/calibration constant
  touched (P, backtest bands, Brier identical); removed the now-dead `HEAT_CLAMP` + its false doc. Locked
  by `systemic_pegged_only_when_railed_and_flat` (reworked; a revert to the heat-clamp signature no
  longer compiles). See improvement-log 2026-07-03.
- [x] **1.7 Actor acronyms substring-matched inside ordinary words → phantom great-power inflation** —
  **DONE 2026-07-03.** `extract_actors` (processor.rs) matched every actor pattern with raw
  `str::find`, so the short acronyms `pla`/`cia`/`fbi`/`nato` matched inside `plan`/`plant`,
  `official`/`special`, and `senator` — tagging China/US/NATO and setting `great_power_involved = true`
  on essentially any article, which feeds the great-power coupler and biases the systemic index UP (the
  exact false-alarm direction audit-P5 closed). The sibling sentiment path was already hardened with
  `contains_word` (audit processor-4); the actor path was left on raw `find`. Fix is surgical: word-
  boundary matching only for the acronym patterns (`BOUNDARY_ACTOR_PATS`) — country stems keep substring
  matching so they still catch adjective forms (`russia`→`russian`). Locked by
  `actor_acronyms_do_not_match_inside_ordinary_words` (+ `_still_match_as_whole_words`). See
  improvement-log 2026-07-03.
- [x] **1.8 Domain keywords substring-matched mid-token → phantom domain inflation** —
  **DONE 2026-07-04.** The sibling of 1.7 on the DOMAIN-scoring path: `score_domains` matched every
  keyword with raw `tl.contains`, so short bare keywords fired inside unrelated words — `rocket`⊂
  `skyrocket(ed)`, `forces`⊂`reinforces/enforces`, `atomic`⊂`anatomical/subatomic`, `respond`⊂
  `correspondent`, `deal`⊂`ideal`. A benign economic sentence ("prices skyrocketed as the report
  reinforces …") tagged `military_escalation` (0.58) AND `nuclear_posture` (0.65, tags alone) — phantom
  signal into `domain_signals` → theater heat → the published index, the false-alarm direction. Fix
  uses a WORD-START matcher (`starts_word`, boundary-before/any-suffix) on a curated `WORD_START_DOMAIN_KWS`
  set — strictly better than substring: it keeps the wanted plural/tense forms (`rockets`/`forces`/
  `atomic`) that whole-word matching would have dropped, but kills the mid-token hits. Multi-word
  keywords keep substring (can't hide mid-token). No calibration constant touched; backtest builds
  events directly so the four anchors are bit-identical. Locked by
  `domain_keywords_match_at_word_start_not_mid_token` (+ `_still_match_plural_and_tense_forms`). See
  improvement-log 2026-07-04.
- [x] **1.9 The "leading" momentum claim is MEASURED, not asserted** — **DONE 2026-07-04.**
  `systemic_momentum` (3.18) is labelled a LEADING read at four operator surfaces ("a leading
  signal, distinct from the lagging headline delta"), but nothing had ever tested whether it
  actually PRECEDES the realized P — an unearned pillar-1 claim. Added `mom` to the durable
  `TimelineEntry` (records momentum per tick) and `EpochStore::momentum_lead_lag` — a server-side
  lead-lag diagnostic that, over the durable ring, measures whether the SIGN of momentum at `t`
  predicts the sign of the realized P move over the next `L` (candidate lags 15m–4h), across
  decisive-momentum / real-move episodes only. Conservative verdict: `leads` (with the measured
  lead time + directional-hit %) only when a lag clears 60% on ≥12 samples; else an honest
  `no_lead` null or `insufficient`. **PROGRESS 2026-07-04 (fdb07f8): the verdict is now 5-valued
  and triple-gated — `leads` additionally requires beating the contemporaneous one-stride
  baseline by ≥10pp (else `coincident`) and ≥3 distinct sign-separated momentum episodes (else
  `insufficient_episodes`); payload +`baseline_hit_pct`/+`episodes`; the public entry is
  stride-cached (300s). New locks: `_contemporaneous_comovement_is_coincident_not_a_lead`,
  `_two_episode_evidence_withholds_the_lead_verdict`.** The dashboard momentum gauge now renders the MEASURED verdict
  (`leads P ~30m`) in place of the bare assertion. Diagnostic only — never feeds `l_sys`/P, touches
  no fitted constant. Locked by `momentum_lead_lag_recovers_a_planted_6step_lead` (fails-without:
  breaking the verdict threshold flips it off `leads`), `_reports_an_honest_null_when_momentum_does_not_lead`,
  `_insufficient_when_no_decisive_history`, `_tolerates_entries_missing_the_mom_field`,
  `timeline_entry_records_systemic_momentum_for_the_lead_lag_diagnostic`,
  `dashboard_renders_the_measured_momentum_lead_verdict`. FOLLOW-UP [candidate]: the per-theater ladder-chip tooltip *used to say* "a leading signal" —
  that unearned copy was REMOVED by fdb07f8 (2026-07-04; it now reads "the direction of coverage…
  measured only at the systemic gauge"). What remains open is only the optional per-theater
  lead-lag measurement itself. See
  improvement-log 2026-07-04.

## 2. Legibility — dashboard / UX  (grasp the state at a glance)
- [x] **2.7 The eyes gate can SEE the I&W "why" board** — **STAGED 2026-07-04.** The deploy-time
  eyes gate (`deploy/eyes/smoke.mjs`) verified the timeline, domain chart, gauge and ladder, but
  never looked at the I&W board — the densest awareness surface and, per the code itself, "the why
  behind the headline number." A client refactor that dropped cells, crashed `renderIndicators`, or
  left the "awaiting indicator data…" placeholder up would ship an EMPTY why-panel with the gate
  green. Added check #7: read the fixed 12-condition board off `api/latest.indicators`, poll the DOM
  for the board to populate (WS-race-safe, fillers excluded via `aria-hidden`), then assert it
  rendered exactly the indicators the server sent, each with a legible (non-empty) label, board not
  collapsed. Server side of the contract locked IN-SANDBOX by
  `every_indicator_carries_a_legible_nonempty_label_and_unique_id` (fails on a blank label or a
  duplicated id — both proven). STAGED→DONE on the local deploy that runs the browser gate. See
  improvement-log 2026-07-04.
- [x] **2.5 Live-read freshness watchdog** — **DONE 2026-06-13.** The header status hard-asserted
  `Live · <time>` from each snapshot's `computed_at`, set ONLY on snapshot arrival (dashboard.html
  `applyData`). If the model worker stalled or the WebSocket hung silently (TCP alive, no `onclose`,
  no frames), the readout froze — the dashboard kept claiming the read was **Live** with a stale
  timestamp, a pillar-1 honesty violation (a real-time read that silently freezes is a lie; the
  methodology even promised "if it goes stale, the feed or model worker has stalled" but nothing
  surfaced it). Added a `renderFreshness()` watchdog on a 5s timer: it gates the "Live" label on the
  actual data age (wall-clock receipt time `_lastSnapMs`) and, past `STALE_AFTER_MS` (45s ≈ 45 missed
  1s ticks), rewrites the header to `⚠ STALE · no update for Nm` in amber. Fires WITHOUT a new
  snapshot (the exact stall case). Locked by `dashboard_warns_when_the_live_read_goes_stale`. See
  improvement-log 2026-06-13.
- [x] **2.6 Blind-read honesty (no live signal ≠ calm world)** — **DONE 2026-06-19.** The
  freshness watchdog (2.5) catches a stalled *connection*, but a healthy server computing on a
  window of ZERO live events (total feed outage / cold start) keeps broadcasting snapshots — so
  the header stayed "Live" while the headline had silently collapsed to the BASELINE PRIOR
  (~1.5%, calm green), indistinguishable from a genuinely quiet world (pillar-1: cosmetic
  reassurance). Named the state at its source — `bayesian::is_data_blind(events)` (the exact
  offline-confidence-floor condition, single source of truth), served as `meta.data_blind`, and
  the watchdog now shows `⚠ NO LIVE SIGNAL · baseline only` (amber) when the read is blind but
  fresh (STALE still takes precedence). DISPLAY-only — P(WWIII) untouched. Locked by
  `is_data_blind_agrees_with_the_offline_confidence_floor` (bayesian),
  `meta_data_blind_flags_a_zero_event_read_as_baseline_only` (aggregator),
  `dashboard_flags_a_blind_read_instead_of_claiming_live` (server). See improvement-log 2026-06-19.
  - PROGRESS 2026-06-19: extended the blind-read honesty to the **I&W board** — the header
    watchdog (2.6) caught the headline, but the board is its own operator surface with its own
    summary line, and during a blind read every theater/coupler light derives from ZERO events,
    so the board read a calm grey "0 / 11 tripped" all-clear, indistinguishable from a quiet
    world (same pillar-1 cosmetic-reassurance failure). `renderIndicators` now consults the same
    `_dataBlind` flag and shows `no live signal · all-clear unconfirmed` (amber) when blind; a
    light still tripped (e.g. the independent seismic monitor) is surfaced as
    `N / 11 tripped · no live event signal`, not buried. Locked by
    `dashboard_iw_board_flags_a_blind_read_instead_of_a_calm_all_clear`. See improvement-log 2026-06-19.
  - PROGRESS 2026-06-20: added the **partial-outage sibling** of the blind state. `is_data_blind`
    is binary (zero events); but a window with live events from only ONE or TWO feeds (a feed-fleet
    partial outage, most sources dark) is a real measurement on a NARROW base — the header still
    said a flat "Live", overstating how broadly corroborated the read is. Added
    `bayesian::is_thinly_sourced(events, sources)` = `events > 0 && sources < MIN_CORROBORATING_SOURCES`
    (=3, the corroboration floor; below `CONFIDENCE_SOURCE_SATURATION`, mutually exclusive with blind),
    served as `meta.thinly_sourced`, and `renderFreshness` now shows `⚠ THIN COVERAGE · N feed(s)
    reporting` (amber) AFTER the blind check (blind is the stronger state). DISPLAY-only — P(WWIII)
    untouched. Locked by `is_thinly_sourced_is_a_narrow_base_distinct_from_blindness` (bayesian),
    `meta_thinly_sourced_flags_a_narrow_source_base` (aggregator),
    `dashboard_flags_a_thinly_sourced_read_instead_of_full_coverage_live` (server). See improvement-log 2026-06-20.
  - PROGRESS 2026-06-20: extended the thin-coverage state to the **I&W board** — the header carried
    the thin caveat but the board still showed a flat grey `0 / 11 tripped` all-clear during a thin
    read, overstating how broadly the quiet is corroborated (board analog of a flat "Live").
    `renderIndicators` now consults the same `_thinSourced` flag and shows
    `all-clear · thin coverage · N feed(s)` (amber) AFTER the stronger blind branch; trips stay
    visible. Locked by `dashboard_iw_board_flags_a_thinly_sourced_read_instead_of_a_full_coverage_all_clear`.
    The board and header now read the same blind/thin flags. See improvement-log 2026-06-20.
  - PROGRESS 2026-06-20: closed the **STALE state on the I&W board** — the third and last board
    honesty state (the 2.5 freshness-watchdog analog of the blind/thin board fixes above). The
    header watchdog flips to STALE during a connection stall, but `renderIndicators` (which writes
    the board summary) runs ONLY on snapshot arrival — by definition never during a stall — so the
    board kept a FROZEN `0 / N tripped` all-clear, presenting an old read as current (the board
    analog of the exact header lie 2.5 catches). `renderIndicators` now caches the trip/total/apex
    counts and `renderFreshness`'s age-gated STALE branch re-flags the board summary on the timer
    (`all-clear · STALE · last read Nm ago`, amber/red-on-apex). DISPLAY-only — P(WWIII) untouched.
    Board and header now agree on all three caveat states (stale/blind/thin). Locked by
    `dashboard_iw_board_flags_a_stale_read_instead_of_a_frozen_all_clear`. See improvement-log 2026-06-20.
  - PROGRESS 2026-06-21: closed the **capped-read sibling** — the same "displayed number doesn't
    mean what it says" failure as blind/thin/stale, but at the TOP of the scale. The forecast is
    hard-clamped to `FORECAST_PROB_CEILING` (0.90) for epistemic humility, yet a pegged read showed
    a bare `90.0%` — indistinguishable from a *measured* 90% when the unclamped systemic signal
    sits at or above the ceiling (the apex world that `forecast_prob_ceiling_is_the_named_honesty_clamp`
    proves reaches it). Named the state — `bayesian::is_at_forecast_ceiling(p_annual)` (single source
    of truth, `p ≥ FORECAST_PROB_CEILING − 1e-9`), served as `meta.at_ceiling` — and the hero now reads
    `≥90.0%` with a `▲ capped at ceiling · true read may be higher` caveat (the command-strip risk cell
    also gets the `≥`). DISPLAY-only — the clamp itself is untouched; no calibration constant moved.
    Locked by `is_at_forecast_ceiling_agrees_with_the_clamp` (bayesian),
    `meta_at_ceiling_flags_a_clamped_read_as_capped` (aggregator),
    `dashboard_flags_a_capped_read_instead_of_a_measured_ceiling` (server). See improvement-log 2026-06-21.
- [x] **2.1 Small/short-viewport pass** — **DONE 2026-06-15.** Root-caused the clipping: the
  left rail (`.left-panel`) is a CSS-grid item with `overflow-y:auto`, but had the default
  `min-height:auto` — which lets a grid item grow past its row track to fit content, so its own
  `overflow-y:auto` saw no overflow, never showed a scrollbar, and the methodology button + brand
  foot were clipped below the fold on short (laptop/landscape) viewports with no way to reach them.
  Added `min-height:0` so the item respects the track height and the scrollbar engages → the rail
  SCROLLS. Locked by `dashboard_left_rail_scrolls_instead_of_clipping_on_short_viewports` (asserts
  both halves of the contract on the live `.left-panel` rule). Phone (≤680px) already stacks/scrolls
  via the existing breakpoint. Final visual is the deploy-time eyes gate. **Sibling defect CLOSED
  2026-07-01:** the center column (`overflow:hidden`, charts on `flex:1`) had no short-viewport
  escape, so on a short/wide viewport the fixed strips crushed the charts to zero and the bottom
  card clipped with no scroll. Added a `@media(max-height:640px)` rule (the vertical twin of the
  ≤680px width rule) that lets the page scroll and pins the charts to explicit heights — which also
  defuses the chart-resize risk the 06-15 note deferred on. Scoped to short heights, so the
  normal-height render is byte-identical. Locked by
  `dashboard_center_column_scrolls_instead_of_clipping_on_short_viewports`. See improvement-log
  2026-06-15 + 2026-07-01.
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
- [x] **2.3 Methodology completeness** — **DONE 2026-06-15** ("2.3 is now fully addressed", see the
  2026-06-15 PROGRESS below; kept current by later runs as the model evolves). Model internals
  (regime ×, P₀, GP, elevated) belong in the methodology view, NOT the landing rail (rail stays
  30d/90d/last-computed). Further methodology PROSE for something already shown is T3 annotation and
  display-only-cap-bound — not an open item.
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
  - PROGRESS 2026-06-11: closed the **same P₀ drift hazard on the DASHBOARD** (the primary
    operator surface, which the methodology fix had missed). `dashboard.html` hand-typed the
    quiet-year baseline in TWO places — the model-state footer's Bayesian chain
    (`Baseline P₀ = 1.5%/yr`) and the "what this means" calibration line
    (`~1.5%` modern quiet-year baseline) — both of which would silently quote a stale prior if
    `BASELINE_ANNUAL` were recalibrated. Both are now `{{BASELINE_ANNUAL_PCT}}`, substituted in
    `server.rs::generate_dashboard_html` from `models::BASELINE_ANNUAL * 100` (same anti-drift
    mechanism as `{{ELEVATION_THRESHOLD}}` on the dashboard and `{{BASELINE_ANNUAL_PCT}}` on the
    methodology). Locked by `dashboard_renders_baseline_prior_from_the_model_constant` (both refs
    templated, placeholder substituted, rendered value == constant — a revert to a hardcoded
    `1.5%/yr` fails it). Remaining: regime ×/GP internals in the methodology view.
  - PROGRESS 2026-06-12: closed the **regime-factor INSPECTOR** sibling (flagged the same day under 1.2).
    The operator panel labeled `HISTORICAL_ANCHOR × regime_product` as "Adjusted P₀ … %/yr" — the
    superseded v1 form implying a regime toggle moves the forecast PRIOR. In v2 the prior is FLAT and the
    regime product enters ONLY as the bounded guardrail-collapse amplifier on the systemic likelihood.
    `api.rs::regime_summary` now reports v2-honest figures — `guardrail_collapse` (sourced from the
    engine's own `bayesian::guardrail_from_regime`, made `pub` as the single source of truth, anti-drift)
    and `likelihood_amplifier_pct` (the bounded +0..12% lift) — and the dropped v1
    `adjusted_prior`/`adjusted_prior_pct` fields are GONE; the dashboard reads "Structural pressure: N× →
    guardrail collapse G (+X% on systemic L, prior unaffected)". Also reframed the stale `regime_warnings`
    text (was "adjusted prior … above ELEVATION_THRESHOLD with zero event signal" — false in v2) and the
    startup log line. No model/calibration constant touched. Locked by
    `regime_summary_reports_guardrail_collapse_not_an_adjusted_prior` (api.rs) +
    `dashboard_regime_inspector_shows_structural_pressure_not_adjusted_prior` (server.rs). Remaining under
    2.3: regime ×/GP internals in the methodology view.
  - PROGRESS 2026-06-12: closed the **regime internals in the methodology view** (the standing remaining
    2.3 leg). The whitepaper's couplers section said guardrail collapse "carries the operator-tunable
    regime factors" but never explained HOW the regime enters — the bounded saturation mechanism the
    dashboard footer and the regime inspector now surface was absent from the authoritative document.
    Added a quantified paragraph: the structural regime factors multiply into a regime product that does
    NOT move the prior (the v1 form) but drives guardrail collapse — its excess above neutral 1.0× maps
    linearly to a 0–1 collapse fraction saturating at the regime product `{{GUARDRAIL_SATURATION_X}}`
    (= 1 + `GUARDRAIL_REGIME_SPAN`), adding at most `+{{GUARDRAIL_AMPLIFIER_PCT}}` to `L_sys`; because it
    touches only the likelihood, a degraded-but-quiet world (`L_sys ≈ 0`) stays at the baseline prior. Both
    figures TEMPLATED from `bayesian::GUARDRAIL_AMPLIFIER` / `GUARDRAIL_REGIME_SPAN` in `server.rs`
    (single source of truth, anti-drift — same pattern as the alert bands / ceiling), so the prose can
    never disagree with `guardrail_from_regime`. No model constant touched. Locked by
    `methodology_renders_guardrail_collapse_from_the_model_constants`. Remaining under 2.3: the GP /
    great-power involvement coupler (documented qualitatively in #couplers) — optional polish.
  - PROGRESS 2026-06-15: closed the **last 2.3 leg** — quantified the whole `#couplers` section. The
    bullets named the five systemic couplers but gave NO magnitudes, so an operator couldn't see how
    big each lift is or — crucially — that the nuclear brink (`+70%`) is *designed* to outweigh
    multi-theater breadth (`+26%`). Each bullet now shows its max lift, all TEMPLATED from `theater.rs`'s
    own constants (`COUPLING_GP_WEIGHT` `+45%` / `COUPLING_ALLIANCE_WEIGHT` `+30%` /
    `GP_ENTANGLEMENT_SATURATION` 3 / `BREADTH_ASYMPTOTE` `+26%` / `BRINK_AMPLIFIER` `+70%`), substituted
    in `server.rs` (made the five constants `pub`, single source of truth) — same anti-drift pattern as
    the guardrail figures. The brink bullet now states the locked honesty relationship in operator-facing
    terms: `+70% > +26%`, so breadth never swamps a single nuclear brink (the engine invariant
    `breadth_never_swamps_the_nuclear_brink`). No model constant value changed; backtest 9/9, calibration
    evidence identical. Locked by `methodology_renders_coupler_magnitudes_from_the_model_constants`. **2.3
    is now fully addressed.** (Coupler magnitudes as of 2026-06 — the 2026-06-28 de-saturation refit
    `BREADTH_ASYMPTOTE` to 0.10 (+10%); the methodology figures TEMPLATE from the constants, so the
    operator page stayed correct automatically.)
  - PROGRESS 2026-06-23: the model EVOLVED — the **persistence floor** (theater.rs, added 2026-06-21)
    holds an active war's heat through a multi-day news gap and is surfaced to the operator as the
    `⏸ held by persistence` caveat (chip 3.11 / hero 3.12 / map 3.23) — yet the methodology never
    documented it, so an operator seeing a held read had nowhere to learn what it means, how long a
    silent war is held, or when it releases (a pillar-1 gap: a material model mechanism behind a caveat,
    unexplained). Added a new `#persistence` section explaining the asymmetric fast-rise/slow-earned-fall
    floor, its two honesty gates (Limited-War rung; release on de-escalation), and that it never moves a
    full-freshness reading (calibration bands untouched). The two figures (`{{FLOOR_FRACTION_PCT}}` 85% /
    `{{WAR_STATE_HALF_LIFE_SCALE}}` 5×) are TEMPLATED from `theater.rs`'s own `FLOOR_FRACTION` /
    `WAR_STATE_HALF_LIFE_SCALE` — same anti-drift pattern as the couplers/guardrail figures. Locked by
    `methodology_renders_the_persistence_floor_from_the_model_constants`. **Keep this current as the floor
    evolves (it is still PROTOTYPE/provisional).**
  - PROGRESS 2026-06-24: corrected a **pillar-1 false reassurance** the 2026-06-15 note introduced. The
    `#couplers` bullets told the operator "breadth can never swamp a single nuclear brink" — but that is
    only the MULTIPLIER-level invariant (`BRINK_AMPLIFIER +70% > BREADTH_ASYMPTOTE +26%`,
    `breadth_never_swamps_the_nuclear_brink`), NOT a headline guarantee. The model's own live behaviour
    contradicts the absolute reading: yesterday's de-saturation measurement (`52a657d`) showed the
    no-brink live peg — 5 hot theaters, great powers entangled, alliance invoked, guardrails collapsed —
    reads **≈83.6% vs Cuba's single-theater brink apex ≈79.8%**, because the systemic couplers compound
    multiplicatively (the 1914 signature). Reworded both bullets to the honest, precise claim: the brink
    amplifier outranks pure breadth *at equal great-power coupling*, and disclosed that a broad,
    interlocked world can still out-read an isolated brink. Updated the test that ENSHRINED the falsehood
    (`methodology_renders_coupler_magnitudes_from_the_model_constants` asserted `contains("never swamp")`)
    to instead reject the absolute claim and require the qualified one. No model/calibration constant
    touched; backtest bands green, calibration evidence identical (Brier ~2e-6, in-band 4/4); suite 450
    green, clippy clean. (Does NOT touch the Robert-gated de-saturation recalibration — only stops the
    page from claiming the opposite of what the model now does.) (The `+26%` figure is as of 2026-06;
    refit to +10% (`BREADTH_ASYMPTOTE = 0.10`) by the 2026-06-28 de-saturation — the invariant and the
    templating are unaffected.)

## 3. Awareness — theaters / feeds / map  (show where & why)
- [x] **3.20 Dedicated China–India (LAC) theater** — **DONE 2026-06-30.** Promoted the 3.19 interim
  (china+india clash → `Other`, invisible) to a real 6th `Theater::primary()` entry (`Theater::ChinaIndia`,
  id `china_india`, label "China–India (LAC)"). `theater_of`'s china+india guard now routes to it, so a
  Galwan-style standoff scores its OWN heat / ladder chip / I&W contribution instead of being dropped
  (Other has no `primary()` slot → `compute` discarded it). Dashboard ladder grid widened `repeat(5→6,1fr)`
  (narrow breakpoint `repeat(2,1fr)` already wraps). Anchor-safe: the new theater is Stable (heat 0) in every
  backtest → couplers/hottest-theater bit-identical (bands 22/0, Brier/RMSE unchanged). Locked by
  `china_india_clash_is_a_visible_theater_with_its_own_heat` (theater, end-to-end: routes + scores; FAILS
  without the theater — the `china_india` state is absent) + the renamed
  `china_india_clash_routes_to_its_own_theater_not_taiwan_or_kashmir` (models). **MAP HAND-OFF (signal-hunter
  lane):** `osint.rs::theater_coord` has no `china_india` centroid yet, so the LAC flashpoint shows on the
  ladder/board but not (yet) as a map dot — `theater_coord` returns `None` and `build_theater_features` skips
  it gracefully (no breakage). Add `"china_india" => (34.0, 79.0)` (Ladakh / LAC) to complete map parity.
  See improvement-log 2026-06-30.
  - FOLLOW-UP 2026-06-30: completed the China–India wiring on the LLM-enriched path. `nlp_sidecar::
    is_valid_theater` — the allow-list gating the LLM's theater hint — was a hand-maintained 5-id literal
    that had silently gone stale: it was MISSING `china_india`, so an LLM that correctly classified a
    Galwan/LAC clash had its `china_india` hint REJECTED and the event routed to the invisible `Other`
    bucket, undercutting the new theater on exactly the path (the enricher) that catches clashes the
    keyword resolver misses. Re-derived the check from the single source of truth
    `models::Theater::primary()` so it is now DRIFT-PROOF (a future theater is covered automatically; this
    list had drifted twice). Engine-behavior, no calibration touched (Brier 0.00092 / in-band 4/4
    identical). Locked by the strengthened `valid_theater_ids` (asserts `china_india` accepted — FAILS on
    the stale literal — and that every `Theater::primary()` id is a valid hint).
- [x] **3.19 China–India clash no longer mis-attributed to Taiwan/Kashmir** — **DONE 2026-06-29 (superseded by 3.20).**
  An engine-behavior honesty/awareness fix in `theater_of` (`models.rs`). A China–India border clash
  (actors `china`+`india`, two nuclear great powers) has NO tracked dyad of its own, but BOTH actors map
  to *named* theaters, so the per-actor count + region tiebreak silently absorbed it into US–China/Taiwan
  (region `asia_pacific`) or India–Pakistan (region `south_asia`) — fabricating heat in a flashpoint the
  event is not about and able to name the wrong *lead* theater (operator reads "US–China/Taiwan" while the
  fighting is on the Himalayan border). Added a narrow guard: china(+/`china_military`)+india with NEITHER
  `taiwan` NOR `pakistan` present routes to `Other`, per this resolver's own contract ("a story with no
  tracked dyad does not belong to a named theater"). The guard does NOT fire when the genuine partner is
  present (china+taiwan stays Taiwan; india+pakistan stays Kashmir). Anchor-safe (backtests assign theater
  tags directly, never via `theater_of` → bit-identical, bands 22/0). Locked by
  `china_india_clash_is_not_mis_attributed_to_taiwan_or_kashmir` (FAILS without the guard: routes to
  UsChinaTaiwan). This is the honest INTERIM for the deferred dedicated **China–India (LAC) theater** (a
  6th `Theater::primary()` entry — still blocked: it is eyes-gated on the `repeat(5,1fr)` ladder grid in
  `dashboard.html` and cross-lane on the `osint.rs` centroid table; needs Robert sign-off / signal-hunter
  coordination). See improvement-log 2026-06-29.
- [x] **3.17 Per-theater escalation-MOMENTUM gauge** — **DONE 2026-06-28.** A NEW computed gauge
  (T1): each theater now reports `escalation_momentum` ∈ [−1,+1], the recency-weighted mean signed
  `escalation_step` of its events — the Goldstein-style conflict↔cooperation DIRECTION of the news
  flow. Distinct from `heat` (magnitude) and `delta`/`trend` (the heat SCORE's change): coverage can
  turn conciliatory (momentum < 0) while heat is still high or floor-held, or escalatory before heat
  rises — a LEADING signal. The input (`escalation_step`) was already ingested but only ever
  THRESHOLDED to the de-escalation floor boolean (`theater_is_deescalating`); this surfaces the
  magnitude behind that gate. Computed in `theater.rs` (`escalation_momentum()`, now the single source
  for both the gate and the gauge), served as an additive contract-v1 field on each theater, and
  rendered on the ladder chip as a green "⇩ talks" / red "⇧ escalatory" tag (shown only when |m| ≥
  0.25). No calibration constant touched — the de-escalation gate is bit-identical. Locked by
  `escalation_momentum_surfaces_the_signed_news_flow_direction` (theater) +
  `dashboard_renders_per_theater_escalation_momentum` (server). See improvement-log 2026-06-28.
- [x] **3.18 SYSTEMIC escalation-momentum aggregate** — **DONE 2026-06-28.** A NEW computed gauge (T1)
  building on 3.17: `couplers.systemic_momentum` ∈ [−1,+1] is the HEAT-WEIGHTED mean of the per-theater
  `escalation_momentum` across theaters above baseline — the single systemic LEADING read of which way
  the WHOLE board's coverage is tilting RIGHT NOW. Heat-weighting keeps a calming backwater from
  outvoting a heating flashpoint; a quiet world reads exactly 0. Distinct from the per-theater chips
  (which the operator must scan + integrate by eye) and from the headline `delta` (a LAGGING change in
  the already-realized P — the news flow turns before the probability does). Computed in
  `theater.rs::compute`, served on the existing `couplers` object (additive, contract-v1 compatible),
  rendered in the hero as a green "⇩ news flow de-escalating" / red "⇧ news flow escalating" readout
  (shown only when |m| ≥ 0.25). Display/awareness only — never feeds `l_sys`/P; no calibration constant
  touched. Locked by `systemic_momentum_is_the_heat_weighted_board_direction` (theater, incl. the
  heat-weighting proof) + `dashboard_renders_systemic_news_flow_direction` (server). See
  improvement-log 2026-06-28.
  - PROGRESS 2026-06-29: closed a **pillar-1 honesty defect** in the heat weight. The gauge reads the
    LIVE news-flow direction "right now", but it weighted each theater by its DISPLAYED `heat` — which
    for a floor-held theater is a remembered war-state carried through a news gap (memory, not live
    evidence), with a STALE momentum. So a silent, memory-held war voted at full memory-heat weight and
    could dilute or even INVERT the live direction. Adversarial proof: a 96h-silent escalatory war (held)
    alongside a fresh, strongly de-escalating theater read `systemic_momentum = +0.313` (escalating!)
    when the only live news was de-escalation. Excluded floor-held theaters from the weight
    (`!s.held_by_floor`): the gauge now follows the live signal (−0.4..−1), and a board of only silent
    held wars reads 0 (no live news flow → no current direction), consistent with the quiet-world case.
    Engine-behavior; anchor-safe (backtests carry no floor-held theaters → bit-identical, bands 22/0).
    Locked by `systemic_momentum_weights_live_evidence_not_a_floor_held_memory` (FAILS without the
    exclusion: reads +0.313). See improvement-log 2026-06-29.
- [x] **3.16 The I&W board gains a CYBER / CRITICAL-INFRASTRUCTURE warning condition** — **DONE 2026-06-25.**
  With 3.15 (diplomatic) added, four of the five tracked modalities had a NAMED board light
  (military→`gp_kinetic`, nuclear→`nuclear_signaling`, economic→`energy_chokepoint`,
  diplomatic→`diplomatic_breakdown`) — `cyber_info_ops` (a weight-0.9 modality that feeds the headline)
  was the ONLY one left unnamed, COUNTED by `cross_domain` but with no dedicated light, so a cyber /
  critical-infrastructure escalation (grid / C2 / financial / undersea-cable attack — the modern opening
  move of great-power conflict, routinely PRECEDING kinetic action) short of a 3-modality cross-domain
  trip went dark on the operator's at-a-glance board. Added `cyber_infrastructure` (indicators.rs), same
  global-max-over-theaters idiom and 0.45 signaling bar as the nuclear/energy/diplomatic lights, naming
  the hottest theater and a near-miss on a clear read; NOT apex. The board renders generically off
  `data.indicators`, so no frontend edit; the methodology advertised count is LOCKED to the live
  `evaluate().len()` (now "twelve" — the guardrails light was retired 2026-07-03). No engine/calibration path touched (bands 20/20 green, evidence
  bit-identical). Locked by `cyber_infrastructure_light_trips_and_names_the_hottest_theater` +
  `cyber_infrastructure_clear_surfaces_hottest_near_miss` (indicators) and the updated
  `empty_snapshot_trips_nothing` / `methodology_advertises_the_live_iw_board_count`. **All five tracked
  modalities now have a named I&W light.** See improvement-log 2026-06-25.
- [x] **3.15 The I&W board gains a DIPLOMATIC-BREAKDOWN warning condition** — **DONE 2026-06-25.**
  The board scored five modalities but NAMED only three (military via `gp_kinetic`, nuclear via
  `nuclear_signaling`, economic via `energy_chokepoint`); `diplomatic_breakdown` — the classic 1914
  "off-ramps closing" leading warning (recalled ambassadors, walked-out talks, severed crisis comms) —
  had no dedicated light. The `cross_domain` light merely COUNTED it, so a diplomatic collapse short of a
  3-modality cross-domain trip went dark on the operator's at-a-glance board. Added `ind_diplomatic`
  (indicators.rs), same global-max-over-theaters idiom and 0.45 signaling bar as the nuclear/energy lights,
  naming the hottest theater and a near-miss on a clear read; NOT apex. The board renders generically off
  `data.indicators`, so no frontend edit (12 lights = a clean 4×3 grid). Also corrected a pre-existing
  legibility drift: the methodology page said the board "tracks ten" conditions while it had eleven — now
  "twelve", LOCKED to the live `evaluate().len()` so the advertised count can never silently drift again.
  No engine/calibration path touched (bands 4/4, evidence bit-identical). Locked by
  `diplomatic_breakdown_light_trips_and_names_the_hottest_theater` +
  `diplomatic_breakdown_clear_surfaces_hottest_near_miss` (indicators) and
  `methodology_advertises_the_live_iw_board_count` (server). See improvement-log 2026-06-25.
- [x] **3.14 The 6h trend names a RELOCATION of the lead theater, not just a magnitude** — **DONE 2026-06-22.**
  The "6h Trend" cell reported only HOW MUCH P(WWIII) moved; it could not show WHERE — and a net-flat
  headline can hide one theater cooling as another heats (the locus of risk relocating with little net
  change). Named the lead theater at its source — `models::lead_theater(theaters)` = the hottest theater
  above Stable (single source of truth) — persisted it on each `TimelineEntry` (`lead`, `#[serde(default)]`)
  so the durable ring carries the window's STARTING locus; `EpochStore::trend_window` now emits `lead_then`
  (the oldest-in-window lead), and `server.rs` attaches `lead` (read from the live snapshot via the same
  SoT) + `lead_shifted`. The trend sub-line renders `lead→X (was Y)` ONLY on an actual shift (a stable
  leader adds no clutter / no clipping risk), and the trend info modal documents it. DISPLAY/awareness only
  — no engine path touched (calibration evidence Brier/RMSE/in-band bit-identical, bands 4/4). Locked by
  `lead_theater_is_the_hottest_non_stable_theater` + `timeline_entry_records_the_lead_theater` (models),
  `epoch_store_trend_reports_the_baseline_lead_theater` (aggregator), and
  `dashboard_renders_6h_trend_lead_shift` (server render-hook). See improvement-log 2026-06-22.
- [x] **3.13 A held chip names HOW FAR the read has decayed (fresh-evidence rung)** — **DONE 2026-06-22.**
  3.11/3.12 flag THAT a read is floor-held; they don't say how much memory vs measurement it is. A war
  held at "Limited War" whose fresh evidence alone reads "Crisis" is far more suspect than one whose fresh
  read is still "Limited War", but the chip showed both identically. Added `TheaterState.fresh_rung_label`
  = `rung_for(fast_heat, …)` (the rung the LIVE evidence supports, vs the displayed rung the floor may be
  lifting) — honest by construction and ≤ the displayed rung always (heat ≥ fast_heat). The ladder chip
  now appends `· fresh: <rung>` to the `⏸ held` tag when the floor strictly demotes the rung, in the same
  vocabulary the operator already reads. DISPLAY-only; bands/Brier bit-identical. Locked by
  `fresh_rung_label_shows_how_far_a_held_read_decayed_below_the_floor` (theater.rs: live→equal, never higher
  than displayed, strict demotion at some age across a silence) + extended
  `dashboard_flags_a_floor_held_theater_instead_of_a_live_read` (server render lock). See improvement-log 2026-06-22.
- [x] **3.12 The HEADLINE flags a memory-held read, not just the theater chip** — **DONE 2026-06-22.**
  3.11 flagged a floor-held theater on the ladder chip, but the operator's at-a-glance read is the hero
  P(WWIII) — and because the persistence floor lifts the lead theater's heat, it lifts the headline too
  (the `persistence_floor_holds_a_silent_war_through_a_multiday_gap` backtest proves a 4-day-silent war
  stays ~elevated). So the big number could rest on a *remembered* war-state with no fresh fighting while
  the hero said nothing (pillar-1). Named the aggregate state at its source —
  `theater::systemic_read_is_floor_held(&theaters)` = the highest-heat theater (the monotone index's
  dominant driver) is `held_by_floor` — served as `meta.read_held_by_floor`, and the hero now shows an
  amber `⏸ held by persistence · no fresh escalation in the lead theater` caveat (hidden in every normal
  state; sits beside the `▲ capped at ceiling` caveat). DISPLAY-only — P(WWIII) untouched, all four bands
  + Brier bit-identical. Locked by `systemic_read_is_floor_held_when_the_lead_theater_is_held` (theater.rs:
  fresh→false, 4-day-silent→true, de-escalation-released→false, quiet→false),
  `meta_read_held_by_floor_flags_a_memory_held_headline` (aggregator: lead-held→true, cooler-held→false,
  quiet→false) + `dashboard_flags_a_floor_held_headline_not_a_live_read` (server render lock). See
  improvement-log 2026-06-22.
- [x] **3.11 A floor-held theater is flagged, not shown as a live read** — **DONE 2026-06-21.**
  The persistence floor (2026-06-21 model change) holds a hot theater's heat up through a multi-day
  news gap (silence ≠ peace), so the displayed heat can be a *remembered* war-state rather than a
  fresh measurement — but the operator had no way to tell a live-hot flashpoint from one the model
  is holding quiet (pillar-1 "the number must mean what it says" + pillar-3 "show WHY"). Named the
  state at its source: `TheaterState.held_by_floor = floor > fast_heat` (the floor strictly outweighs
  the fresh read), honest by construction. Surfaced on the theater-ladder chip as an amber `⏸ held`
  tag + tooltip ("heat held by the persistence floor — no fresh escalation; released on de-escalation
  evidence"). DISPLAY-only — P(WWIII) untouched, all four calibration bands + Brier bit-identical.
  Locked by `held_by_floor_flags_a_war_carried_through_a_news_gap_not_a_fresh_read` (theater.rs:
  fresh→false, 4-day-silent→true, de-escalation-released→false, quiet-world→false) +
  `dashboard_flags_a_floor_held_theater_instead_of_a_live_read` (server.rs render lock). See
  improvement-log 2026-06-21.
  - PROGRESS 2026-06-23: extended the floor-held honesty to the **world-map flashpoint popup** —
    the LAST operator surface still painting a floor-held theater (a remembered war-state carried
    through a news gap) identical to a live-hot one (the chip had 3.11, the hero 3.12; the map
    popup had neither). `osint::build_theater_features` now carries the engine's `held_by_floor` +
    `fresh_rung_label` onto each theater feature (minimal 2-property add to the shared file), and
    the dashboard popup renders an amber `⏸ held by persistence · no fresh escalation · fresh: <rung>`
    line — same vocabulary/contract as the ladder chip. DISPLAY-only; bands 4/4, Brier 0.00000
    bit-identical. Locked by `theater_feature_carries_the_persistence_floor_flags` (osint — flags
    pass through; pre-floor snapshot defaults to not-held, no panic) +
    `dashboard_map_popup_flags_a_floor_held_theater_not_a_live_read` (server render lock). Map,
    chip, and hero now agree on the floor-held caveat.
- [x] **3.10 Seismic test-consistency reaches the I&W board** — **DONE 2026-06-18.** The
  strongest PHYSICAL nuclear indicator — a shallow event at a known test site that has cleared
  the natural-earthquake discriminator (no aftershock sequence, or a CTBTO statement) — lived
  ONLY on the standalone `pollNuclear` banner, absent from the consolidated I&W warning board an
  operator scans. Added an 11th deterministic light, **"Seismic event consistent with nuclear
  test"** (`seismic_test`), sourced from the detector's own `SeismicAlert::is_test_consistent`
  determination (within-radius AND level ∈ {AftershockAbsent, CtbtoStatement} — so a raw
  single-network Anomaly or a not-yet-aftershock-tested multi-network detection does NOT
  over-claim). The aggregator carries the strongest qualifying alert onto the snapshot
  (`seismic_test_consistent` + `seismic_site`) AFTER `compute`, so it is DISPLAY-only and never
  touches P(WWIII) (backtest 9/9, Brier identical). Amber (not apex): the apex set stays reserved
  for great-power-WAR states, and this is an explicitly "consistent with" heuristic. Still
  LLM-independent, so the board's honesty contract holds (prose updated to "theaters, couplers,
  and the seismic monitor"). Locked by `seismic_test_light_trips_off_the_snapshot_flag_and_names_the_site`
  + `is_test_consistent_requires_proximity_and_a_cleared_discriminator`. See improvement-log 2026-06-18.
- [x] **3.9 Headline "where" names the nuclear-brink theater, not the loudest one** — **DONE
  2026-06-17.** The systemic `driver` string (the dashboard's Primary Driver "where") named the
  hottest-by-heat theater. But the brink amplifier (the +70% apex lever, the single largest term
  in `l_sys`) is detected across ALL theaters and — per `brink_fires_in_a_non_hottest_theater` —
  need NOT live in the hottest one (a Cuba-style standoff has near-zero kinetic volume yet maximal
  nuclear danger). So in the most dangerous configuration the headline "where" pointed at a louder
  conventional theater while the actual apex sat unnamed. `theater::score_all` now captures the
  brink theater (most acute by nuclear posture; `any(theater_is_nuclear_brink)` ≡ `is_some()`, so
  the amplifier is unchanged) and the driver reads "{brink theater} at nuclear brink; N theaters
  hot" when a brink leads — the hottest theater stays visible in the dashboard sub-line + ladder
  strip, so the operator gets BOTH apex and hottest. No model/calibration constant touched. Locked
  by `driver_names_the_brink_theater_not_the_hottest_one`. See improvement-log 2026-06-17.
- [x] **3.8 I&W board gains a VELOCITY-at-altitude warning condition** — **DONE 2026-06-17.**
  All nine prior I&W lights were standing-LEVEL reads; none flagged a hot flashpoint *getting
  worse* — yet the IC I&W method is fundamentally about detecting CHANGE, so the consolidated
  warning board (its deterministic, LLM-independent summary) was missing its core leading
  indicator. Added a 10th condition, **"Active escalation at a flashpoint"** (`active_escalation`,
  `indicators.rs`): trips when a theater already at Crisis+ (rung ≥ Crisis = heat ≥ `HOT_HEAT`,
  the same hot boundary the concurrency coupler uses) is ALSO `trend == "rising"`. It reuses the
  model's own rung + rising classification — **no new calibrated constant** — so it can never
  disagree with the ladder strip, names the HOTTEST qualifying theater (same hottest-qualifying
  rule as the apex lights) and surfaces the rising driver as the WHY; the clear reading names the
  hottest theater rising at all (even sub-Crisis), so a flashpoint heating up stays visible. The
  dashboard renders it automatically (the board maps over `inds`); stale "nine"/"3×3" copy in the
  dashboard + methodology updated to "ten"/"3-column". Locked by
  `active_escalation_trips_on_a_hot_rising_theater_and_names_the_hottest` +
  `active_escalation_requires_velocity_not_just_level`. No headline math touched — calibration
  identical (Brier 0.00000, in-band 4/4). See improvement-log 2026-06-17.
- [x] **3.7 Map marker colour follows the authoritative rung, not raw heat** — **DONE 2026-06-14.**
  The world-map flashpoint markers (`osint.rs::build_theater_features`) coloured each theater via
  `heat_color(heat)` — a THIRD independent copy of the rung heat thresholds (0.06/0.18/0.38/0.62,
  duplicating `theater.rs::rung_for` + `within_band`) that re-derived the colour from raw heat. But
  `rung_for` can raise a theater's rung ABOVE its heat band (great-power involvement, WMD use, nuclear
  use force a higher rung), so a Great-Power-War theater at moderate heat was painted the lesser
  Limited-War colour — disagreeing with the marker's own `rung_label` — and the apex Systemic (nuclear-
  use) rung had no distinct colour at all (it collapsed into GP-War red). Replaced `heat_color` with
  `rung_color(EscalationRung)` keyed off the engine's authoritative `rung` (already carried in the
  snapshot), giving Systemic its own apex colour and removing the duplicated thresholds (the boundaries
  now live only in `theater.rs`). Honest by construction — the marker colour can no longer understate an
  apex rung. Locked by `rung_colors_cover_every_rung_distinctly` +
  `marker_color_follows_authoritative_rung_not_heat`. See improvement-log 2026-06-14.
  - FOLLOW-UP — **DONE 2026-06-14.** The rung heat boundaries are no longer duplicated: the upper two
    are now named `LIMITED_WAR_HEAT` (0.38) / `GREAT_POWER_WAR_HEAT` (0.62) and the lower two reuse the
    existing `STABLE_HEAT_CEILING` (0.06) / `HOT_HEAT` (0.18), so all four live in exactly one place and
    both `rung_for` (which rung) and `within_band` (where in it) read the same constants. Locked by
    `rung_for_and_within_band_share_one_contiguous_partition`, which proves the index position
    `(rung.level()+within_band)/6` stays continuous + monotone across every rung seam (a drift between the
    two functions would jump it). No model constant value changed. See improvement-log 2026-06-14.
    (HISTORICAL — the `(rung.level()+within_band)/6` staircase index, `within_band` itself, and that
    lock test were RETIRED by the 2026-06-28 de-saturation; the index is now the continuous
    `index_from_l_sys` (theater.rs:252-273) and the rung is a label only. The named heat-boundary
    constants remain live in `rung_for`. Do not search for the retired test.)
- [x] **3.6 Apex I&W lights attribute WHERE to the hottest qualifying theater** — **DONE 2026-06-13.**
  The two APEX I&W board lights (`gp_kinetic`, `nuclear_brink` — the red, highest-stakes
  great-power-war conditions, `IW_APEX` on the dashboard) attributed their WHERE pointer to the
  *first* qualifying theater in `theaters` Vec order, not the hottest: `gp_kinetic` used
  `gp_kinetic.first()` and `nuclear_brink` used `theaters.iter().find(...)`. When two great-power
  wars (or two nuclear brinks) are live, this could hand the apex attribution to the *lesser*
  theater — e.g. a LimitedWar listed before a GreatPowerWar — a pillar-3 "show WHERE" defect on the
  two conditions that matter most. The alliance light (`indicators.rs`) already enforces
  hottest-qualifying via `max_by(heat)` (locked by `alliance_light_names_the_hottest_invoking_theater`);
  this brings the two apex lights to the same rule: `gp_kinetic` now sorts its qualifiers most-escalated
  first (rung, then heat) so both the `theater` attribution and the detail list lead with the hottest,
  and `nuclear_brink` picks the hottest brink theater. No model/calibration constant touched. Locked by
  `apex_lights_name_the_hottest_qualifying_theater`. See improvement-log 2026-06-13.
- [x] **3.5 Analyst brief speaks the model's dominant coupling channel** — **DONE 2026-06-13.**
  The `/api/brief` analyst brief (the "why the number is where it is" insight layer) hard-coded its
  systemic-mechanism sentence ("Multiple concurrently-hot theaters coupled to nuclear-armed great
  powers … rather than any single regional war") for EVERY hot world — flatly wrong in a single-theater
  nuclear brink (Cuba-style), where the dominant amplifier IS a single theater. Replaced the canned claim
  in `templated_brief` with `coupling_sentence(coupling_driver)` — a per-channel account driven by the
  model's own `couplers.coupling_driver` (3.4) — and added the dominant channel to the LLM `build_context`
  so the narrative model is grounded in it too. Honest by construction (restates the engine's dominant
  amplifier, no new lever). Locked by `context_includes_the_dominant_coupling_channel` +
  `templated_brief_accounts_for_systemic_reading_from_the_live_coupling_driver`. See improvement-log 2026-06-13.
- [x] **3.4 Systemic "why": dominant coupling amplifier** — **DONE 2026-06-12.** The dashboard
  named WHICH theater is hottest (`systemic.driver`) and showed the coupling multiplier as one
  opaque number, but never WHICH coupling channel was turning a regional crisis into a *world*-war
  risk. Added `SystemicCouplers.coupling_driver` — the channel contributing the largest
  multiplicative lift to `l_sys`, read directly off the same excesses that build it
  (`brink_mult`/`coupling_multiplier`/`concurrency_mult`) via the pure
  `theater::dominant_coupling_amplifier`: "single-theater nuclear brink", "great-power
  entanglement", "multi-theater concurrency", or "alliance activation"; empty when no channel
  lifts (an honest "regional, not yet systemically coupled" read). Surfaced on the model-state
  footer ("coupling ×N · … · led by X") sourced from the live coupler (anti-drift). Honest by
  construction, no model constant touched. Locked by
  `coupling_driver_names_the_dominant_systemic_amplifier` (theater.rs) +
  `dashboard_surfaces_the_systemic_coupling_driver` (server.rs). See improvement-log 2026-06-12.
  - PROGRESS 2026-06-16: closed the **structural-coupler blind spot** in that read-out. The four
    candidates were only the ACUTE couplers — the fifth, `guardrail_collapse`, is derived in
    `bayesian::compute` from the regime multiplier (AFTER the theater engine names the driver), so
    `coupling_driver` could NEVER name it, even when eroded arms-control/deterrence was the single
    largest amplifier of a live crisis (a degraded-but-acutely-quiet world would read "regional, not
    yet systemically coupled" while structural collapse was the only lift). `dominant_coupling_amplifier`
    now returns `(label, lift)`; the Bayesian engine compares the guardrail lift (`GUARDRAIL_AMPLIFIER ×
    guardrail`, same units) and names "structural guardrail collapse" when it strictly outlifts the
    acute winner — gated on `tout.l_sys > floor` so guardrails amplify a live crisis but never get
    named in a calm world (honesty invariant: they never manufacture risk from calm). No model constant
    touched; backtest 9/9, evidence Brier identical. Locked by
    `guardrail_collapse_is_named_dominant_coupler_only_when_it_outlifts_the_acute_ones` (bayesian.rs, the
    acute-wins / guardrail-leads / calm-names-nothing trichotomy) + the brief sentence branch.
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
- [x] **3.2 GDELT** — **DONE at ingestion** (`GDELT_QUERIES` + `gdelt_loop` in `src/ingestor.rs`
  feed the event stream, liveness-probed by `search_api_liveness` per 3.1; loop health served in
  `/api/sources` `search_apis` since 2026-07-03). The sub-national "awareness layer" ambition is
  SUPERSEDED by §8.2 (the Hotzones monitor — out-of-repo). Standing rule kept: do NOT add geo-less
  sources to the map (e.g. CISA KEV has no geo).
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
- [x] **4.2 Risky `unwrap()/expect()` audit** — **CLOSED 2026-06-18, re-verified 2026-06-30**
  (src/ prod paths clean, see PROGRESS below + improvement-log 2026-06-30 — **do not re-chase the
  phantom counts**). Original brief: find `unwrap()/expect()` on
  genuinely fallible runtime paths (network, parse, lock-poisoning) that could panic the
  service; convert to graceful handling. Skip the legitimately-infallible ones. Lock each
  fix with a test that exercises the error path.
  - PROGRESS 2026-06-18: AUDITED src/ (the high counts cited in the routine prompt —
    aggregator ~27 / theater ~24 / processor ~21 — are almost entirely TEST-code unwraps).
    The production-path unwrap/expect set is small and each is provably safe: `detector.rs`
    `nearest_test_site` `partial_cmp().unwrap()` (the NaN-prone idiom) can't panic because the
    `dist <= radius` filter above it drops every NaN distance (NaN ≤ x is false → empty → None,
    no min_by call); `detector.rs:491 nearest_site.unwrap()` is guarded by an early `None` return;
    `models.rs:221/243 position().unwrap()` are called only with members of `Theater::primary()`;
    the HTTP-client/signal `.expect`s are infallible startup constructors. **No genuine prod-panic
    target remains in src/** (vendor/ee-* is the signal-hunter's lane). Recorded so a future run
    doesn't re-chase the phantom counts (cf. the enricher cautionary tale).
- [x] **4.4 LLM output sanitation boundary** — **DONE 2026-06-12.** The clamp that keeps an
  out-of-range or non-finite model score from reaching the risk engine was an inline loop buried
  in `LlmEnricher::classify`'s async network path — UNTESTED (no test exercised out-of-range LLM
  output) and NON-FINITE-UNSAFE (`f64::clamp` returns NaN unchanged, so a NaN/Inf score from an
  overflowing token would survive). And it is the SINGLE point of defense: `merge_llm_scores` /
  `make_event_from_llm` copy `modality_pairs()`+`severity` straight into `domain_signals` without
  re-clamping. Extracted a pure finite-safe `LlmExtraction::sanitize()` (modalities+severity→[0,1],
  escalation_step→[-1,1], any non-finite→0.0), called it in `classify`, and locked it by
  `sanitize_clamps_out_of_range_and_neutralizes_non_finite_scores`. Honesty payoff: a buggy/adversarial
  model can no longer inflate or poison the systemic read with a 1.7 or a NaN. See improvement-log 2026-06-12.
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
- [x] **4.5 Vendored ee-* crates can drift from `engineering-effects` upstream** — **DONE 2026-06-15.**
  Chose **(b): the vendored tree is a PINNED, GCRM-owned snapshot; divergence from upstream is
  intentional.** Option (a) blind re-vendoring is actively WRONG here — GCRM edits these crates in
  place (the `ee-sources` map connectors are curated daily by the signal-hunter; the `ee-view`
  `layer_geojson` lifetime fix is GCRM-local), so a wholesale re-vendor would clobber local work.
  Policy recorded in `vendor/README.md`: adopt upstream only via a deliberate, `cargo test`-gated
  cherry-pick that preserves GCRM-local edits, never a fast-forward. Locked by
  `vendor_policy_documents_every_vendored_member` (every `vendor/ee-*` workspace member must be
  documented in the policy, so a new vendored dep can't slip in undecided). See improvement-log 2026-06-15.

## 5. Toward v2  (the approved factored rebuild)
- [ ] **5.1** Sensible standalone steps toward theaters × orthogonal modalities × couplers,
  each shippable and test-locked on its own. See the v2 plan; don't land half-states.
  (Largely REALIZED by the v2 engine already in place — scope check with Robert before picking
  this up; do not invent v2 work to fill the checkbox.)

## 6. New signal — wire the ee-sources catalog into the read  (highest-value open frontier)
GCRM ingests news + OSINT, but the vendored `ee-sources` catalog (~35 connectors) does NOT yet feed
the headline. Each item ADDS a term to its modality with a pre-registered, grounded weight rationale —
it NEVER retunes a firewall constant (anchors / asymptote / ceilings / `DOMAIN_WEIGHTS` are
Robert-gated). Every source: parser test on a checked-in REAL-RESPONSE fixture + an `#[ignore]`d
`feed_roster_liveness` probe + a synthetic test proving its presence CHANGES an output; mark **STAGED**
(the local deploy promotes to DONE after the live leg). One source per run is a clean T1 that moves the
**Live signal sources** metric — the anti-nit lever.

> **ARCHITECTURE PREREQUISITE (found 2026-06-28 — read before attempting any of 6.1–6.5).** "Feed a
> modality" is NOT a simple source-wire in v2. The headline P(WWIII) is driven ONLY by `theater.rs::compute`,
> which partitions the window by `e.theater` and **drops every theater-less event** (`if let Some(t) =
> &e.theater`). The global modality weighted-sum (`bayesian.rs::compute` Step 6, `weighted_domain_sum`) is
> "compat display only" and does NOT enter `p_wwiii_annual`. So a GLOBAL signal (markets, CISA KEV, OFAC —
> none of which are theater-located) added as a `domain_signal` moves only a vestigial display field, not the
> read. The two honest ways to actually move the read are both **Robert-gated**: (a) attribute the global
> signal to a theater — a modeling claim that risks a crisis→markets→crisis feedback double-count; or (b) add
> a NEW bounded GLOBAL amplifier on `l_sys`, mirroring the `guardrail_collapse` overlay (`l_sys × (1 +
> AMP·x)`), gated on `l_sys > COUPLING_AMPLIFIER_FLOOR` so markets corroborate a live crisis but never
> manufacture risk from calm. (b) is architecturally clean and anchor-safe (the backtests carry no market
> events, so they stay bit-identical) — but the amplifier MAGNITUDE ("how much should a market panic raise the
> war read?") is a value-laden calibration decision in the same class as the Robert-gated de-saturation peg
> (see 1.1a / the honesty firewall), so a cloud run must NOT introduce it unattended. **Next shippable step for
> 6.1:** propose the `MARKET_STRESS_AMPLIFIER` design (const + gate + the `ee_correlate::finance` composite
> reused from `osint::finance_payload`, which already runs in prod) to Robert; on approval it ships as a clean
> STAGED T1. Until then 6.1–6.5 are blocked on this decision, not on code.

- [ ] **6.1** markets / yahoo → `economic_warfare` (commodity / energy / financial-stress term).
- [ ] **6.2** cisa_kev / cve_delta / ransomwatch → `cyber_info_ops`.
- [ ] **6.3** ofac → sanctions / economic-coercion term.
- [ ] **6.4** cables / ports / powerplants → critical-infrastructure exposure.
- [ ] **6.5** nuclear / nuclear_tests → nuclear-posture corroboration (keep DISPLAY/seismic-cross-check
  unless a calibration pass is Robert-approved).

## 7. Unified platform — the RAITHE Global Monitor surface
Full plan: `the platform plan (local)`. GCRM's engine/runtime stay UNTOUCHED;
the platform is product-unified, runtime-federated (each monitor its own binary/port/deploy-gate).
- [x] **7.1** Freeze GCRM's `/api/latest` JSON as documented "headline-read contract v1" + a contract
  test, so sibling monitors and the portal clone a SPEC, not a forked SPA. **DONE 2026-06-27.** Added
  the top-level `contract: "gcrm.headline-read/v1"` negotiation handle (`HEADLINE_READ_CONTRACT` in
  `aggregator.rs`, served by `snapshot_to_json`), documented the frozen schema +
  add-is-compatible/retype-is-breaking version policy in `docs/headline-read-contract-v1.md`, and
  locked the full core shape + v1 cross-field invariants with `snapshot_to_json_honours_contract_v1`.
- [ ] **7.2** Coordinate the read-only `/intel` portal tile contract (the portal lives in raisearch,
  not this repo) — keep GCRM a pure read-only data source.

## 8. New monitors — GCRM-class siblings on the ee substrate
Each is its own federated binary (NOT in this repo), but its DEFINITION-OF-DONE ladder lives here so a
"monitor" can't be claimed by scaffolding. +1 **Monitors shipped** only at the final rung. Order
(Robert, 2026-06-25): Markets → Hotzones → Resources → Climate. DoD ladder per monitor:
(a) scaffold + health endpoint → (b) ≥1 LIVE connector → (c) honest headline index from real input →
(d) named ladder rungs → (e) where/why map or theater view → (f) uncertainty posture (blind/thin/stale
+ interval) → (g) renders under an eyes gate.
- [ ] **8.1** Global Markets Monitor — SFSI on OFR FSI (build first; engine `ee-correlate::finance` exists).
- [ ] **8.2** Global Hotzones Monitor — GIL on GDELT 2.0 (fills GCRM's sub-national blind spot).
- [ ] **8.3** Global Resource Monitor — GRSI on IMF PortWatch chokepoint map (keyless, live-verified).
- [ ] **8.4** Global Climate Monitor — PCSI (build last; needs shared epoch store + areal Geo first).

---

## How to use / maintain this file
1. Read this + `improvement-log.md` + `scorecard.md` + recent `git log`.
2. Pick ONE unchecked item — the highest VALUE TIER (T1 > T2 > T3 per `scorecard.md`) you can do well
   + prove today, biasing toward new signal / platform / monitors (§6–8) and the least-recently-touched
   axis. Re-verify it's still open against the code. The CLOSED VEINS above are off-limits.
3. Implement; get `cargo build --release` + `cargo test` green; add/strengthen a test.
4. Check the box, append to `improvement-log.md` (what + metric moved + proof), commit, push.
5. If you discover a better item, add it under the right axis with a `[candidate]` tag and a
   one-line rationale. Keep this list honest and current — it's the spine of the program.
