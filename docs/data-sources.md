# GCRM Data-Source Ledger

The authoritative record of every external data source **evaluated** for GCRM's
situational-awareness map — what's live, what was rejected (and why), what's deferred,
and where to hunt next. The twice-daily **Signal Hunter** routine reads this FIRST every
run and updates it every run, so the program *compounds* and never re-chases a source
that's already live or already rejected.

**Acceptance bar** (a candidate must clear all six to go on the map):
1. **Authoritative** — a government agency / recognized national body / established
   scientific org. No scrapers, no unofficial mirrors.
2. **Auth-free or trivially keyed** — public endpoint; if a key is needed it goes in the
   gitignored `secrets.env` (never committed). Prefer key-free.
3. **Machine-readable** — JSON / GeoJSON / XML / CAP / RSS / CSV that parses deterministically.
4. **Geocoded** — lat/lon per record so it plots as a map dot. A NON-geo source does NOT
   go on the map (it would be a count with zero dots — see `cisa_kev`); it may live in
   the `ee_sources::registry()` catalog for a future non-map surface.
5. **Fresh** — actually updating; note the recency of the newest record.
6. **Non-duplicative** — adds coverage (geography / domain / modality) the live feeds lack.

Plus the **signal-meaningfulness** rule: every value plotted must carry real-world meaning
and units. A raw internal/absolute number with no operator meaning (e.g. an absolute river
gauge level with no per-station flood baseline) is a "nonsense number" and must NOT ship.

---

## LIVE — wired into the map (`src/osint.rs` fan-out) — do not re-add

| source_id | EventKind | Authority | Notes |
|-----------|-----------|-----------|-------|
| `usgs` | Earthquake | USGS | global quakes |
| `eqcanada` | Earthquake | NRCan | dense Canadian seismicity |
| `emsc` | Earthquake | EMSC | global, denser outside Americas |
| `cwfis` | Wildfire | NRCan | satellite thermal hotspots (FRP) |
| `firms` | Wildfire | NASA | global thermal hotspots (key in secrets.env) |
| `cwfis_activefires` | Wildfire | NRCan/CIFFC | national agency fire ground-state (stage+size) |
| `gvp_volcano` | Volcano | Smithsonian GVP | recent/ongoing eruptions |
| `nws` | Weather | US NWS | US-only alerts |
| `eccc_alerts` | Weather | ECCC | Canada weather warnings/watches |
| `eccc_marine` | Weather | ECCC | Great-Lakes marine warnings |
| `eccc_aqhi` | AirQuality | ECCC | air-quality stations (smoke proxy) |
| `healthmap` | Health | HealthMap | global disease clusters |
| `eonet` | (natural) | NASA EONET | wildfires/storms/volcanoes |
| `gdacs` | (disaster) | GDACS | global disaster alerts |
| `opensky` | Aircraft | OpenSky | live aircraft positions (anon often rate-limited) |
| `navcanada` | Aircraft | NAV CANADA | NOTAM airspace/aerodrome hazards |
| `ontario511` | Transport | Ontario MTO | provincial road events |
| `drivebc` | Transport | BC MoTI | provincial road events (Open511) |
| `alberta511` | Transport | Gov of Alberta | provincial road events |
| `quebec511` | Transport | Transports Québec | provincial road events (MTMD WFS) |
| `cbsa_bwt` | Transport | CBSA | 29 land border-crossing wait times |
| `acled` | Conflict | ACLED | global armed conflict — **dormant** until creds in secrets.env |

**Registry catalog only (NON-geo, deliberately NOT on the map):**
`cisa_kev` (US CISA known-exploited CVEs, Cyber), `cccs` (Canadian Centre for Cyber
Security advisories, Cyber). Both are non-geo → they would plot as a count with zero
dots. Ready for a future cyber-advisories panel/surface, not the map.

**Finance panel (not the map):** `yahoo` (Market) → Finance Radar.

---

## REJECTED — do NOT re-chase (recon 2026-06-14)

- **Alert Ready / NAAD CAP (Pelmorex)** — authoritative + auth-free, but overwhelmingly
  re-ships the `eccc_alerts` stream; no single feed URL (must list a day-folder, fetch
  each signed CAP-XML, dedupe en/fr + Update/Cancel), and the live push is a TCP socket.
  Large ingest for a ~few-% unique residue. Skip unless the civil/EMO slice is specifically
  wanted (then filter status=Actual, sender≠ECCC).
- **ECCC hydrometric-realtime** — authoritative/geocoded/fresh, but carries NO severity
  signal: it's raw absolute gauge level/discharge, incomparable across rivers without each
  station's flood-stage baseline (which the API doesn't provide). A plotted "2.79 m" dot is
  exactly the "nonsense number" the signal-meaningfulness rule forbids. Revisit ONLY if a
  per-station historical-quantile baseline is precomputed (turns level→anomaly). v2-scale.
- **NTWC tsunami messages (tsunami.gov)** — usable + geocoded, but US-NOAA-authoritative,
  not a Canadian agency (fails bar 1 for the Canada focus). Reconsider only if the program
  scope broadens to "official-issuer-for-Canada" sources.
- **NRCan space weather** — no clean machine-readable, geocoded, per-record product (K-index
  is a JS-drawn image; F10.7 is a single-site global scalar). No map fit.

---

## DEFERRED — verified OK, adopt when the caveat is resolved

- **IESO Ontario grid demand** (`reports-public.ieso.ca/public/RealtimeTotals/PUB_RealtimeTotals.xml`)
  — live + authoritative, but a single province-wide scalar (no per-record lat/lon). Only
  fits as ONE static Ontario marker; marginal map value. Adopt only if a single grid-load
  marker is explicitly wanted.
- **Avalanche Canada** (`api.avalanche.ca/forecasts/en/{products,areas}`) — authoritative,
  geocoded (join product.area.id↔areas feature.id, centroid). **Seasonal**: off-season
  returns "spring"/"norating" (0 plottable). Implement with an off-season-tolerant parser
  and gate the layer to light up ~late-Nov→Apr.
- **CCCS cyber** (`cccs`) — already a registry connector; lift onto a UI surface (a cyber
  panel), not the map.

---

## COVERAGE GAPS & HUNTING IDEAS — where to look next

Bias each run toward the least-covered axis below.

- **Vessel / AIS (EMPTY layer)** — the map's biggest hole. Look for an authoritative,
  auth-free AIS or vessel feed (e.g. NOAA marine, Canadian Coast Guard, port authorities,
  StatCan/DFO). Most AIS needs a key — find a public one or note it as keyed.
- **Conflict** — `acled` is wired but dormant (needs creds). Look for an auth-free conflict/
  unrest signal (e.g. GDELT events with real geo, UN/OCHA, ReliefWeb if it geocodes).
- **Geography** — feeds are Canada/US-dense. Hunt authoritative regional feeds for
  Europe (Copernicus EMS, MeteoAlarm if it geocodes), Asia/Pacific (JMA quakes/tsunami,
  Australia BoM/GA), Latin America, Africa.
- **Domains under-covered** — power-grid stress (other ISOs), rail/pipeline incidents
  (TSB Canada, NTSB), dam/reservoir, drought, flood-WITH-baselines, lightning (if a geocoded
  near-real-time product exists), methane/industrial (GHGSat/Sentinel).
- **Cyber surface** — `cisa_kev` + `cccs` exist but aren't surfaced; a non-map cyber panel
  would unlock them.

---

## Run log

Newest first. One short entry per run: date, what was evaluated, what was adopted/rejected/
deferred, and the green-proof. Append; never rewrite history.

- **2026-06-14** — Ledger seeded from the manual Canadian-feed recon (15 candidates,
  11 confirmed). Adopted to the map: `drivebc`, `alberta511`, `quebec511`,
  `cwfis_activefires`, `cbsa_bwt`, `navcanada`. Added `cccs` as a non-map registry
  connector. Rejected Alert Ready, hydrometric, NTWC tsunami, space weather (reasons above).
  All live with `errors=[]`; build + full test suite green.
