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
