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
