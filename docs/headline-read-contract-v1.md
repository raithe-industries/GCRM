# GCRM headline-read contract — `gcrm.headline-read/v1`

The frozen schema of GCRM's headline read, served at `GET /api/latest` and pushed
over the WebSocket as `{"type":"snapshot","data":{…}}`. This is the **federation
contract** for the RAITHE Global Monitor platform (roadmap §7.1): sibling monitors
and the read-only `/intel` portal consume *this spec*, they do not fork the
dashboard SPA. GCRM stays a pure read-only data source; its engine/runtime are
untouched by the platform.

## Versioning

Every payload carries a top-level `contract` string, namespaced and versioned as
`<monitor>.<surface>/v<N>` — currently **`gcrm.headline-read/v1`** (the constant
`HEADLINE_READ_CONTRACT` in `src/aggregator.rs`, the single source of truth). A
consumer reads `contract` **first** and refuses to trust the rest if it does not
recognise the version, rather than silently mis-reading a bumped schema.

Compatibility rule:
- **Adding** a new optional field is backward-compatible → **no** version bump.
- **Removing or retyping** a documented field is breaking → bump the `/vN` suffix
  and keep this doc's prior version section for the old consumers.

The shape is locked by `snapshot_to_json_honours_contract_v1`
(`src/aggregator.rs` tests): it fails if the `contract` handle changes, if a
documented core key disappears or changes type, or if a v1 cross-field invariant
breaks. Treat a red there as "you are making a breaking change — bump the version
on purpose," never as "delete the assert."

## Core fields (built by `snapshot_to_json`)

| Field | Type | Meaning |
|---|---|---|
| `contract` | string | `"gcrm.headline-read/v1"` — version handle (read first). |
| `snapshot_id` | string | Unique id of this snapshot. |
| `computed_at` | string (RFC3339) | When the engine produced it. |
| `prior` | object | `historical_anchor` (number), `formula` (string), `regime_multiplier` (number), `regime_role` (string). v2 prior is FLAT — regime drives guardrail collapse, not a prior multiply. |
| `domains` | object | Map `domain_id → { score, label, elevated, confidence, event_count, great_power_events }`. |
| `co_occurrence` | object | `elevated_count` (int), `boost` (number). |
| `probabilities` | object | `annual`, `annual_pct`, `thirty_day`, `ninety_day` (all numbers). `annual_pct == round(annual·100, 6dp)`; horizons are constant-hazard folds of `annual` so `thirty_day ≤ ninety_day ≤ annual`. |
| `delta` | object | `annual`, `thirty_day` (numbers), `direction` ∈ `{rising, falling, stable}`. |
| `confidence` | number | Data-quality confidence ∈ [0,1] — evidence the read rests on, NOT part of the forecast. |
| `alert` | object | `level` (string), `message` (string), `elevated_threshold`, `critical_threshold` (numbers, the live annual-P bands). |
| `systemic` | object | `index` (number, the 0–`FORECAST_INDEX_CEILING` headline index), `driver` (string, the dominant coupling channel). |
| `theaters` | array | Per-theater state (heat, rung, why…), incl. `escalation_momentum` (number ∈ [−1,+1], the recency-weighted direction of the news flow). |
| `couplers` | object | Systemic amplifiers (gp-entanglement, alliance, concurrency, breadth, guardrail_collapse, breadth_saturated…). |
| `indicators` | array/object | I&W board lights (`crate::indicators::evaluate`). |
| `meta` | object | `events_in_window`, `data_blind`, `thinly_sourced`, `at_ceiling`, `breadth_saturated`, `read_held_by_floor`, `sources_active`, `great_power_events`, `regions_active`, `top_actors`, `aggregation_window_hours`, `max_window_events`. The honesty-posture flags an operator/consumer must respect. |

## Server-augmented fields (merged in `broadcast_snapshots`)

These are part of the served contract but added after `snapshot_to_json`, each with
its own dedicated lock test:
- `model_calibrated_at` — string (RFC3339) or null.
- `trend_6h` — durable server-computed 6h trend ring; includes `lead`, `lead_shifted`, `pegged`. (`epoch_store_trend_*`, `dashboard_html_renders_6h_trend_from_server_field`.)
- `uncertainty` — server-computed interval + error posture (`empirical_hw_pct`, low/high…).
- `epistemic` — `reference_class`, `error_posture` (the `models.rs` single-source strings).

## Honesty note for consumers

The headline is a **fitted forecast under epistemic humility**, not a frequency.
Respect `meta.data_blind` / `thinly_sourced` / `at_ceiling` / `read_held_by_floor`
and the `uncertainty` interval — a bare point read off `probabilities.annual`
without these flags is a mis-read. The number is capped (`at_ceiling`) and the index
is capped at `FORECAST_INDEX_CEILING`; neither prints certainty.
