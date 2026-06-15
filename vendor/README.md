# Vendored `engineering-effects` crates — drift policy

GCRM vendors four crates from the `engineering-effects` (`ee-*`) project so the
whole service builds from this one repository with no external/private
dependency:

| Crate | What GCRM uses it for |
|---|---|
| `ee-core` | shared `Event` / geo / source primitives |
| `ee-sources` | the OSINT/map signal connectors (RSS, FDSN, AIS, conflict, fires, …) |
| `ee-correlate` | cross-source correlation helpers |
| `ee-view` | GeoJSON layer assembly for the world map |

All four are workspace members in the root `Cargo.toml` and are depended on by
`src/` — none is dead weight.

## The decision: this tree is a PINNED, GCRM-owned snapshot (not a live mirror)

Upstream `engineering-effects` self-improves several times a day, so a vendored
copy and its upstream **will** diverge. We make that divergence a *decision*, not
an accident:

- **The vendored tree is the source of truth for GCRM.** We do **not** track or
  auto-sync upstream. The committed copy here is what builds, ships, and is
  gated by `cargo build --release` + `cargo test`.
- **Divergence is expected and intentional.** GCRM edits these crates in place
  when the service needs it — e.g. the map connectors in `ee-sources` are
  actively curated for GCRM (see `docs/data-sources.md`), and GCRM-local fixes
  live here permanently (e.g. the `ee-view` `layer_geojson` lifetime cleanup).
  These local edits would be **clobbered by a blind re-vendor**, which is exactly
  why we do not auto-pull.
- **Adopting an upstream change is a deliberate, gated re-vendor**, never a
  wholesale overwrite:
  1. copy in only the upstream change you actually want,
  2. re-apply / preserve every GCRM-local edit to the touched files,
  3. prove `cargo build --release` + full `cargo test` green before committing,
  4. record what was pulled (and why) in the commit message.

Re-vendoring *all four crates* from upstream HEAD is **not** a maintenance task —
it discards GCRM-local work. Treat upstream as a reference to cherry-pick from,
not a branch to fast-forward to.

## Lanes

- `ee-sources` connectors + the data-source ledger (`docs/data-sources.md`) are
  owned by the **signal-hunter** routine. Model/UX/robustness changes elsewhere
  must not edit map feeds or that ledger.
- This policy (the vendoring decision itself) is roadmap item **4.5**, owned by
  the model/UX self-improvement routine and logged in `docs/improvement-log.md`.

A test (`vendor_policy_documents_every_vendored_member` in `src/main.rs`) keeps
this file honest: it fails if a `vendor/ee-*` crate is added to the workspace
without being documented here, so a new vendored dependency can't slip in
undecided.
