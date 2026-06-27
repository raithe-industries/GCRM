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

**Sandbox + ingestion paths.** The cloud Signal-Hunter sandbox allowlists only GitHub +
crates — most live gov/OSINT hosts 403 *in-sandbox*, so the routine **live-verifies via
web fetch** (out-of-band), not curl. Two ways a source lands:
- **Path A — live feed:** the connector fetches the live endpoint; prod (full network)
  serves it after deploy. Default for publicly-reachable feeds.
- **Path B — mirrored snapshot:** for data NOT freely live-reachable (licensed / manual /
  awkward), incorporate it as a GitHub-mirrored snapshot — a connector that reads a
  `raw.githubusercontent.com` mirror (reachable from both sandbox and prod) or a committed
  snapshot file, refreshed by a noted local/manual job. This is the "download + incorporate
  if I can't access it live" path.

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
| `ucdp_ged` | Conflict | UCDP / Uppsala Univ. | georeferenced conflict events (candidate GED), fatalities→severity. Auth-free direct CSV (the live API is now token-gated); version-discovered from the downloads page. Monthly cadence. Fills the Conflict layer ACLED can't. |
| `digitraffic_ais` | Vessel | Fintraffic (Finland) | live Baltic AIS — vessels in abnormal nav state (aground/NUC/restricted) loud, moving commercial traffic faint; routine moored/anchored dropped. Auth-free (Digitraffic-User header + gzip). Fills the previously-empty Vessel layer; Baltic = on-mission (NATO/Russia maritime). |
| `nhc` | Weather | NOAA NHC | active tropical cyclones (Atlantic / E+C Pacific) from `CurrentStorms.json` — live position, classification (HU/TS/TD), max wind (kt)→Saffir-Simpson category + severity, pressure. Auth-free JSON, U.S. public domain. Empty `activeStorms` (off-season) = 0 events, not an error. Fills the storm/cyclone gap EONET (lagging catalog) and GDACS (alert level only) don't cover operationally. |
| `jma_typhoon` | Weather | JMA / RSMC Tokyo | active typhoons over the **Western North Pacific + South China Sea** — the basin NHC does NOT cover (NHC = Atlantic/E-Pacific only). JMA is the WMO-designated RSMC for this basin. `bosai` JSON: `targetTc.json` index → per-system `{tcId}/forecast.json`; the connector emits the *analysis* part (current fix: `center` [lat,lon], `pressure` hPa, `maximumWind.sustained.knots`, `category.en`). Chip = category + JMA intensity grade (Strong/Very Strong/Violent Typhoon) + wind (kt) + pressure (hPa). Auth-free, multi-fetch (index + per-TC), empty index off-season = 0 events not an error. |
| `geonet_volcano` | Volcano | GeoNet / GNS Science | New Zealand **Volcanic Alert Levels** — the `volcano/val` GeoJSON: per-volcano official VAL (0–5) + ICAO aviation colour code (`acc`: Green/Yellow/Orange/Red) + plain-language activity/hazards. Connector drops VAL 0 ("no unrest") and plots only volcanoes at level ≥ 1, so an all-quiet network = 0 events (not an error). Auth-free GeoJSON (`Accept: application/vnd.geo+json;version=2`). Fills the **operational alert-level** modality and **NZ / SW-Pacific** geography that the global GVP eruption catalogue and EONET (event-based) don't carry. Chip = "Alert Level {n} · Aviation {colour}". CC BY 3.0 NZ. |
| `usgs_volcano` | Volcano | USGS VHP / HANS | **US + Alaska Volcanic Alert Levels** — the HANS `getElevatedVolcanoes` notice product (ground alert level NORMAL/ADVISORY/WATCH/WARNING + ICAO aviation colour GREEN/YELLOW/ORANGE/RED) **joined by `vnum`** to the `getUSVolcanoes` catalogue for coordinates (the elevated notice carries no lat/lon). Drops the all-clear state (NORMAL/GREEN/UNASSIGNED) so only volcanoes above background plot; all-quiet network = 0 events, not an error. Auth-free JSON, US-Gov public domain. Chip = "Alert {level} · Aviation {colour}". Fills the **US/Alaska** operational alert-level geography GeoNet (NZ) doesn't cover and that the GVP eruption catalogue / EONET (event-based) don't carry as a live alert state. AVO (most active US volcanoes), HVO (Kīlauea/Mauna Loa), Cascades, Yellowstone. |
| `nwps_flood` | Weather | NOAA / NWS NWPS | **river flooding** — the NWS `water/riv_gauges` "Observed River Stages" GeoJSON layer (id 0), one Point per AHPS gauge with its observed **flood category** in `status`. Connector keeps only gauges **at/above action stage** (`action`/`minor`/`moderate`/`major`); drops the all-clear `no_flooding` + undefined states, so a no-flood network = 0 events, not an error. Auth-free GeoJSON, US-Gov public domain. **Signal-meaningful where a raw gauge level isn't:** `status` is the *baseline-relative* category (NWS already compared live stage to that gauge's own thresholds) — resolves the `ECCC hydrometric` "nonsense number" rejection at its root. Severity major 1.0→action 0.35; chip = "Major flooding" / "Near flood stage". Fills the river-flooding hazard no current feed carries. |
| `magma_volcano` | Volcano | PVMBG / MAGMA Indonesia | **Indonesian volcano operational alert levels** — PVMBG (Geological Agency, Ministry of Energy & Mineral Resources) is the authoritative national monitor for the world's most volcanically active country (~127 monitored volcanoes). Each volcano carries a ground alert level `ga_status` on the 4-step scale (1 Normal / 2 Waspada / 3 Siaga / 4 Awas) plus the latest VONA aviation colour (`vona[].cu_avcode` GREEN/YELLOW/ORANGE/RED). Connector emits one event per volcano **above background** (status ≥ 2); Normal (1) dropped, so an all-quiet snapshot = 0 events. Severity = max(status rank, aviation-colour rank) — an ash-erupting Waspada volcano flagged ORANGE plots at 0.8, not 0.55. Chip = "Alert Siaga (Watch) · Aviation Yellow". **Path B (committed snapshot):** MAGMA's home-map volcano list is embedded server-side (no clean public full-list JSON endpoint) and the host 403s in-sandbox, so a real captured PVMBG payload ships `include_str!`-embedded (`magma_volcano_snapshot.json`), refreshed by a local/manual re-capture job. Schema confirmed against PVMBG's own `magma-indonesia/magma-indonesia` source (`HomeController::gunungApi()` + `VonaApiService`). Fills the **Indonesia / SE-Asia** volcano geography GeoNet (NZ) and USGS (US/Alaska) don't cover. |
| `avalanche_ca` | Weather | Avalanche Canada | **snow-avalanche danger ratings** — Canada's national public-avalanche body. Joins `/forecasts/en/products` (bulletins, by `area.id`) to `/forecasts/en/areas` (region polygons, GeoJSON, same id) and plots one dot per region **with a numeric rating today**, at the polygon centroid. Severity = peak elevation-band rating on the North American scale (Low 0.2 → Extreme 1.0). **Signal-meaningful** (the danger scale is baseline-relative — each level a defined likelihood/size, not a raw number). **Seasonal, handled honestly:** off-season bands read `norating`/spring → dropped, so summer = 0 events not an error (layer lights up ~late-Nov→Apr). Resolves the source's deferral via an off-season-tolerant parser. Auth-free JSON+GeoJSON. Chip = "Alpine Considerable · Treeline Moderate · Below Low". Fills the snow-avalanche hazard no other feed carries. |
| `acled` | Conflict | ACLED | global armed conflict — **PERMANENTLY DORMANT as a live feed**: Open access has NO API (confirmed by ACLED 2026-06-14; API needs a paid license). Only *aggregated weekly* data is public → a **Path-B snapshot** candidate, superseded for now by `ucdp_ged` (which gives live georeferenced conflict). |

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
  **Superseded for the flood domain (2026-06-26) by `nwps_flood`** — NOAA's NWS already ships
  the baseline-relative *flood category* (`status`), so the river-flooding hazard now plots
  meaningfully without precomputing a baseline. (ECCC hydrometric itself stays rejected: it's
  Canadian raw level with no category; reconsider only via the precomputed-quantile route above.)
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
- ~~**Avalanche Canada**~~ — **ADOPTED 2026-06-27** as `avalanche_ca` (LIVE). The deferral
  caveat is resolved: the connector is off-season-tolerant (drops `norating`/spring → 0 events
  in summer; lights up ~late-Nov→Apr). Joins product `area.id` ↔ areas feature `id`, centroid.
- **CCCS cyber** (`cccs`) — already a registry connector; lift onto a UI surface (a cyber
  panel), not the map.

---

## COVERAGE GAPS & HUNTING IDEAS — where to look next

Bias each run toward the least-covered axis below.

- **Vessel / AIS** — SEEDED 2026-06-14 with `digitraffic_ais` (Fintraffic, Baltic). Gap
  now: extend coverage beyond the Baltic — other authoritative auth-free AIS regions
  (NOAA, port authorities) or a GitHub-mirrored snapshot. Two leads ruled out: **NOAA/USCG
  marinecadastre** (authoritative but data on Azure blob, GeoParquet bulk historical — not
  GitHub-raw, not live, no hand-parse) and **Danish Maritime Authority** (live AIS is *paid*,
  DKK 1,800–5,600/yr; only historical 2006–2016 bulk CSV is free, on `web.ais.dk` not GitHub;
  the `dma-ais` GitHub org is Java *software* libraries, no data feed). Neither is a Path-A or
  Path-B fit.
- **Conflict** — SEEDED 2026-06-14 with `ucdp_ged` (Uppsala, live CSV). `acled` stays
  dormant (no Open API). Remaining: a higher-frequency conflict signal if one exists
  auth-free, or the ACLED aggregated-weekly Path-B snapshot.
- **Storm / tropical cyclone** — SEEDED 2026-06-24 with `nhc` (NOAA NHC, Atlantic/E-Pacific)
  and EXTENDED 2026-06-25 with `jma_typhoon` (JMA RSMC Tokyo, W-Pacific/South China Sea, Path A).
  Gap now: **Indian Ocean + Southern Hemisphere** basins — IMD (`mausam.imd.gov.in`), BoM
  (`bom.gov.au`), Météo-France La Réunion, or Fiji RSMC, if an auth-free geocoded product
  exists. JTWC (`metoc.navy.mil`) publishes HTML/RSS only — no clean auth-free JSON/GeoJSON
  (confirmed 2026-06-25; the JSON wrappers found are all keyed third parties: Xweather/DTN).
- **Volcano** — SEEDED globally with `gvp_volcano` (Smithsonian eruption catalogue) + EONET,
  and EXTENDED 2026-06-25 with `geonet_volcano` (GeoNet/GNS NZ **Volcanic Alert Levels** —
  the operational alert state, not an eruption record), and EXTENDED 2026-06-26 with
  `usgs_volcano` (**USGS HANS** US/Alaska Volcanic Alert Levels + aviation colour, Path A via the
  GitHub-verified schema technique), and EXTENDED 2026-06-27 with `magma_volcano` (**PVMBG / MAGMA
  Indonesia** alert levels Waspada/Siaga/Awas + VONA aviation colour, Path-B committed snapshot) —
  the modality now covers the world's most active volcanic region (Indonesia, ~127 volcanoes).
  Next: **Italian INGV** (Etna/Stromboli) or **PHIVOLCS** (Philippines) if an auth-free geocoded
  product exists (neither exposed a clean machine-readable alert API on 2026-06-27 recon — INGV
  publishes bulletins/webcams only, PHIVOLCS only HTML bulletins + a third-party GeoJSON);
  **Icelandic Met Office** (IMO) volcano alerts for the N-Atlantic. **MAGMA snapshot refresh** is a
  local re-capture job (origin host unreachable in-sandbox).
- **Geography** — feeds are Canada/US-dense; SW-Pacific seeded via `geonet_volcano` (NZ),
  W-Pacific via `jma_typhoon`, and **SE-Asia / Indonesia** now seeded via `magma_volcano` (PVMBG).
  Hunt authoritative regional feeds for Europe (Copernicus EMS, MeteoAlarm if it geocodes),
  Asia/Pacific (JMA quakes/tsunami, Australia BoM/GA), Latin America (e.g. Chile SERNAGEOMIN
  volcanoes), Africa.
- **Domains under-covered** — power-grid stress (other ISOs), rail/pipeline incidents
  (TSB Canada, NTSB), dam/reservoir, drought, lightning (if a geocoded near-real-time
  product exists), methane/industrial (GHGSat/Sentinel). **flood-WITH-baselines SEEDED
  2026-06-26** with `nwps_flood` (NOAA NWPS observed flood category, US/CONUS). Gap now:
  flood-with-baselines OUTSIDE the US — a Canadian/European product that ships a category
  (not a raw level); ECCC hydrometric stays rejected for lacking one. **snow-avalanche
  hazard SEEDED 2026-06-27** with `avalanche_ca` (Avalanche Canada danger ratings, Canadian
  mountains, seasonal). Gap now: avalanche/mountain hazard outside Canada (e.g. US NWAC/CAIC,
  the European EAWS/`avalanche.report` if an auth-free geocoded product ships a danger level).
- **Cyber surface** — `cisa_kev` + `cccs` exist but aren't surfaced; a non-map cyber panel
  would unlock them.

---

## Run log

Newest first. One short entry per run: date, what was evaluated, what was adopted/rejected/
deferred, and the green-proof. Append; never rewrite history.

- **2026-06-27** (second run) — **adopted `avalanche_ca` (Avalanche Canada danger ratings), Path A** —
  a new authoritative geocoded layer opening the **snow-avalanche hazard domain** no current feed
  carries, AND the conversion of a long-standing **DEFERRED** source into a working connector (the
  directive prioritizes converting a deferred source over another block record). The deferral caveat —
  "seasonal: off-season returns spring/norating; implement an off-season-tolerant parser" — is resolved
  exactly as specified. Per the SUSTAINED-BLOCK DIRECTIVE I did NOT lead with a no-op. **Network
  re-probed fresh:** web fetch positive control on `raw.githubusercontent.com` correct (`facebook/react`
  `package.json` → `private:true`/no `name`); the egress-wide web fetch 403 on non-GitHub hosts is
  unchanged (geoportal.cl ArcGIS + sernageomin both 403), so live verification stays via the
  GitHub-captured-schema technique. **Ruled out this run before landing avalanche:** **SERNAGEOMIN**
  (Chile volcano alerts, the Latin-America geography gap) — authoritative but NO clean auth-free
  machine-readable alert endpoint surfaced and NO real captured bytes on GitHub to anchor a schema
  (only a static ArcGIS volcano-locations MapServer + an ArcGIS-Hub dataset page, neither web fetch-able);
  building on guessed fields would risk a fabricated connector, so deferred. **Météo-France La Réunion**
  (SW-Indian-Ocean RSMC) — HTML/bulletins only, no auth-free JSON (consistent with the standing
  storm-basin finding). **The avalanche unlock:** confirmed via **web search** that the Avalanche Canada
  API is open/no-auth (`docs.avalanche.ca`), then pulled the **real schema + bytes off GitHub** —
  `bcgov/geobc-bier`'s `avalanche_canada_forecasts.py` (a BC-government consumer) does the exact join I
  need: fetch `/forecasts/en/areas` (GeoJSON, region polygons keyed by feature `id`) + `/forecasts/en/products`
  (bulletins with `area.id` + `report.dangerRatings`), match `product.area.id` ↔ areas feature `id`; the
  dangerRatings shape (`ratings.{alp,tln,btl}.rating.{value,display}`, value like "considerable", display
  "3 - Considerable") is confirmed by a real sample in `GenerationSoftware/avalanche-canada-sms` plus 6+
  independent consumers (`rodrigo-barraza/tools-service`, `weberam2/avytext`, …). So the connector +
  offline fixtures are built against **real Avalanche Canada bytes**, not docs guesswork; prod (full
  network) fetches the live URLs. Clears all six bars: **authoritative** (Avalanche Canada, the national
  public-avalanche body); **auth-free** JSON+GeoJSON; **machine-readable**; **geocoded** (region polygon
  centroid via the area-id join); **fresh** (daily in season; off-season `norating`/spring → 0 events,
  not an error); **non-duplicative** (no feed carries snow-avalanche hazard). **Signal-meaningful:** the
  North American danger scale is baseline-relative (each level a defined likelihood/size), so a
  "Considerable" dot is real signal, not a raw number; severity = peak band Low 0.2 → Extreme 1.0; chip =
  "Alpine Considerable · Treeline Moderate · Below Low". New `vendor/ee-sources/src/avalanche_ca.rs`
  (two-fetch `Source::fetch`; pure `parse_avalanche_ca(products, areas)` + `danger_chip` + `danger_rank`/
  `danger_label`/centroid/`report_of`/`today_ratings`/`band_value` helpers; tolerates value-as-word/
  numbered-display/bare-number, `report` nesting vs flat, array vs `{products}`/`{data}` wrapper; 4
  offline tests: parse drops off-season + unplaceable, all-norating-is-OK, error-on-bad-input, danger
  ladder + chip omits unrated bands). Registered in `lib.rs`; wired `src/osint.rs`
  (`fetch_one("avalanche_ca", …, 14)` + count/cap row cap 200 + `feed_detail` arm + osint chip test);
  SRC_LABEL `Avalanche Canada` in `dashboard.html`. **`cargo build --release` green; full workspace
  `cargo test` green (gcrm 472 / 0 failed / 3 ignored; ee-sources 98 incl. avalanche_ca 4/4;
  ee-correlate 79; ee-view 60; ee-core 5).** EventKind::Weather (the hydromet/seasonal-hazard convention
  NHC/JMA/NWPS follow; adding a dedicated variant is the self-improvement routine's lane). Note: in June
  this layer is off-season and plots 0 events live — by design; it lights up for the Nov→Apr season.
  Next avalanche/mountain target: US (NWAC/CAIC) or European EAWS (`avalanche.report`) if an auth-free
  geocoded danger-level product surfaces; next geography: Latin America (Chile SERNAGEOMIN volcanoes, if
  a machine-readable alert endpoint or a mirrorable snapshot surfaces).
- **2026-06-27** — **adopted `magma_volcano` (PVMBG / MAGMA Indonesia volcano alert levels), Path B** —
  a new authoritative geocoded layer extending the **operational volcanic-alert modality** to **Indonesia**,
  the world's most volcanically active country (~127 monitored volcanoes), the largest single geographic
  gap the modality had after NZ (`geonet_volcano`) and US/Alaska (`usgs_volcano`). Per the SUSTAINED-BLOCK
  DIRECTIVE I led with a **Path-B snapshot ingestion**, not a no-op block record. Re-probed the network
  fresh: **web fetch positive control** on `raw.githubusercontent.com` correct (`facebook/react`
  `package.json` → `private:true`/no `name`); the live MAGMA host **`magma.esdm.go.id/v1/vona` 403s**
  (egress-wide web fetch block on non-GitHub hosts unchanged). Volcano targets named in the ledger ruled
  out this run for lack of a clean auth-free machine-readable alert product: **INGV** (Italy) publishes
  bulletins/seismograms/webcams, no alert JSON; **PHIVOLCS** (Philippines) only HTML bulletins + a
  third-party GeoJSON (fails bar 1). **The unlock (same GitHub-captured-schema technique that landed
  NHC→JMA→GeoNet→USGS-volcano→NWPS):** confirmed via **web search** that PVMBG/MAGMA is auth-free, then
  pulled the **real schema + bytes off GitHub** — PVMBG's **own** source repo `magma-indonesia/magma-indonesia`
  (`HomeController::gunungApi()` selects `ga_code/ga_nama_gapi/ga_lat_gapi/ga_lon_gapi/ga_status…`;
  `VonaApiService` maps the VONA colour code), plus a captured copy of the home-map feed in
  `mandalateknologi/demo-peta` (`public/data/vsi-gunung-api.json`) giving real records with coords +
  `ga_status` (1 Normal / 2 Waspada / 3 Siaga / 4 Awas) + `vona[].cu_avcode` (GREEN/YELLOW/ORANGE/RED).
  Because MAGMA's full-list volcano data is embedded server-side (no clean public JSON endpoint) and the
  host 403s in-sandbox, this is a **Path-B committed snapshot**: 17 real captured records (16 elevated +
  Agung at Normal to exercise the drop) ship `include_str!`-embedded as `magma_volcano_snapshot.json`,
  refreshed by a documented local re-capture job; prod serves the refreshed state. Clears all six bars:
  **authoritative** (PVMBG, Indonesia's national volcano monitor); **auth-free**; **machine-readable** JSON;
  **geocoded** (per-volcano `ga_lat_gapi/lon`); **fresh** (operational alert state; an all-Normal snapshot →
  0 events, not an error; refresh cadence documented per Path B); **non-duplicative** (GVP = weekly
  *eruption* catalogue, EONET = *events*, GeoNet = NZ, USGS = US/Alaska — none carries Indonesia's
  standardized PVMBG alert level + VONA colour as a live state). **Signal-meaningful:** drops Normal (1);
  severity = max(status rank Awas 1.0→Waspada 0.55, aviation-colour rank RED 1.0→YELLOW 0.55) — an
  ash-erupting Waspada volcano flagged ORANGE plots at 0.8, not 0.55; chip = "Alert {Waspada/Siaga/Awas}
  (gloss) · Aviation {colour}". New `vendor/ee-sources/src/magma_volcano.rs` (pure `parse_magma_volcano` +
  `alert_chip` + status/colour rank + latest-VONA-by-`no` helpers; tolerates number-or-string `ga_status`,
  bare-array vs `{volcanoes}`/`{data}` wrappers; 5 offline tests incl. drop-Normal/no-coords, colour raises
  severity above ground level, all-Normal-is-OK, error-on-bad-input, and the committed snapshot parses).
  Registered in `lib.rs`; wired `src/osint.rs` (`fetch_one("magma_volcano", …, 9)` + count/cap row cap 150
  + `feed_detail` arm + osint chip test); SRC_LABEL `PVMBG · MAGMA Indonesia` in `dashboard.html`.
  **`cargo build --release` green; full workspace `cargo test` green (gcrm 470 / 0 failed / 3 ignored;
  ee-sources 94 incl. magma_volcano 5/5; ee-correlate 79; ee-view 60; ee-core 5).** EventKind::Volcano.
  Next volcano target: Italian INGV / PHIVOLCS / Icelandic IMO if an auth-free geocoded product surfaces;
  next geography: Latin America (Chile SERNAGEOMIN).
- **2026-06-26** (second run) — **adopted `nwps_flood` (NOAA NWPS observed river flooding)** — a new
  authoritative geocoded layer opening the **river-flooding domain** no current feed carries, AND the
  source that **resolves the long-standing `ECCC hydrometric` rejection at its root**: the connector
  plots the NWS *observed flood category* (`status`), not a raw gauge level, so every dot is the
  baseline-relative read ("Major flooding") the signal-meaningfulness rule demands — NWS has already
  compared the live stage to that gauge's own action/minor/moderate/major thresholds. Per the
  SUSTAINED-BLOCK DIRECTIVE I did NOT lead with a no-op: I converted the ledger-flagged
  "flood-WITH-baselines" coverage gap into a working connector using the **GitHub-verified-schema
  technique** that broke the 20-run stall. Re-probed the network fresh: **web fetch positive control**
  on `raw.githubusercontent.com` correct (`facebook/react` `package.json` → `private:true`/no `name`);
  the egress-wide web fetch 403 on non-GitHub hosts is unchanged (so the live NWS map service still
  can't be hit in-sandbox; prod's full network fetches it). **The unlock:** confirmed via **web search**
  that the NWS `water/riv_gauges` MapServer (layer 0, "Observed River Stages") + the NWPS API are
  open/no-auth/JSON+GeoJSON, and pulled the **confirmed field schema off NOAA's own service metadata**
  — Point features with `properties.{gaugelid, status, location, waterbody, obstime, units, wfo, url}`,
  `status` being the AHPS flood-category legend (`major`/`moderate`/`minor`/`action`/`no_flooding`/…),
  the standardized enum every NWS AHPS map has used for 15+ years. The offline fixture is built to that
  exact shape (Red River at Fargo `major`, Mississippi at St. Louis `moderate`, Arkansas at Tulsa
  `action`, Potomac `no_flooding`→dropped). Clears all six bars: **authoritative** (NOAA OWP, the U.S.
  flood-forecast body); **auth-free** GeoJSON; **machine-readable**; **geocoded** (per-gauge Point);
  **fresh** (observed real-time state; a no-flood network → 0 events, not an error); **non-duplicative**
  (no existing feed carries river-stage flooding — GDACS gives only an alert level for major events).
  **Signal-meaningful:** drops the all-clear/undefined states; severity major 1.0 → action 0.35; chip =
  "Major flooding"/"Moderate flooding"/"Minor flooding"/"Near flood stage". New
  `vendor/ee-sources/src/nwps_flood.rs` (pure `parse_nwps_flood` + `flood_chip` + `severity_for_status`/
  `status_label`/case-insensitive `prop_str`; the `where`-clause filter is re-applied in the parser so a
  mirror that ignores it still drops non-flood gauges; 5 offline tests: parse+drop-all-clear,
  no-flood-is-OK, error-on-bad-input, status/field casing tolerated, severity ladder). Registered in
  `lib.rs`; wired `src/osint.rs` (`fetch_one("nwps_flood", …, 12)` + count/cap row cap 400 + `feed_detail`
  arm + osint chip test); SRC_LABEL `NOAA NWPS` in `dashboard.html`. **`cargo build --release` green; full
  workspace `cargo test` green (gcrm 468 / 0 failed / 3 ignored; ee-sources 89 incl. nwps_flood 5/5;
  ee-correlate 79; ee-view 60; ee-core 5).** EventKind::Weather (the hydromet convention NHC/JMA follow).
  Next flood target: a Canadian/European product that ships a flood *category* (not a raw level).
- **2026-06-26** — **adopted `usgs_volcano` (USGS HANS US/Alaska Volcanic Alert Levels)** — a new
  authoritative geocoded layer extending the **operational volcanic-alert modality** from NZ
  (`geonet_volcano`) to **US + Alaska** geography, where most US active volcanoes sit (Alaska
  Volcano Observatory) plus Hawaii (Kīlauea/Mauna Loa), the Cascades and Yellowstone. Per the
  SUSTAINED-BLOCK DIRECTIVE I did NOT lead with a no-op block record: I converted the explicitly
  ledger-flagged next volcano target into a working connector using the **same GitHub-verified-schema
  technique that broke the 20-run stall** (NHC → JMA → GeoNet). Re-probed the network fresh:
  **web fetch positive control** on `raw.githubusercontent.com` correct (`facebook/react`
  `package.json` → `private:true`/no `name`); the egress-wide web fetch 403 on non-GitHub hosts is
  unchanged (so the live HANS origin still can't be hit in-sandbox; prod's full network fetches it).
  **The unlock:** confirmed via **web search** that HANS `getElevatedVolcanoes` is open/no-auth/JSON,
  then pulled the **real captured schema + bytes off GitHub** (the one reachable channel): the
  endpoint returns notice records `{vnum, volcano_name, alert_level, color_code, obs_abbr, sent_utc,
  notice_url}` with **NO coordinates** (confirmed by `jeffrwatts/GeoMonitor`'s
  `USGSVolcanoesElevated` Gson data class + multiple independent consumers), so coords must be
  **joined by `vnum`** against `getUSVolcanoes` (`{vnum, volcano_name, latitude, longitude}`) — the
  exact pattern in 3+ independent repos (`kotaronishiwaki/earthquake-globe`,
  `OUTCOMELLC/Pacific-Ring-of-Fire-Hazard-Map`). Real values anchoring the offline fixture came from
  `MN755/11Writer`'s committed `usgs_volcano_status_fixture.json`: **Great Sitkin** vnum 311120
  WATCH/ORANGE @ 52.0764,-176.1317 (AVO) and **Kīlauea** vnum 332010 ADVISORY/YELLOW (HVO). Clears
  all six bars: **authoritative** (USGS VHP, the US volcano monitor); **auth-free** JSON;
  **machine-readable**; **geocoded** (via the vnum→catalogue join); **fresh** (operational alert
  state; an all-clear network → 0 events, not an error); **non-duplicative** (GVP = weekly *eruption*
  catalogue, EONET = *events*, GeoNet = NZ only — none carries the US standardized VAL + aviation
  colour as a live alert state). **Signal-meaningful:** drops NORMAL/GREEN/UNASSIGNED (all-clear);
  severity = max(alert-level rank, colour rank) laddered WARNING/RED 1.0 → ADVISORY/YELLOW 0.55;
  chip = "Alert {level} · Aviation {colour}" (or colour alone when the ground level is unassigned).
  New `vendor/ee-sources/src/usgs_volcano.rs` (pure `parse_usgs_volcano(elevated, catalog)` +
  `alert_chip` + rank/titlecase helpers; tolerates number-or-string `vnum`/lat/lon and bare-array vs
  `{items}`/`{data}` wrappers; 4 offline tests: join+drop-all-clear+drop-no-coords, all-clear-is-OK,
  error-on-bad-input, severity/colour-only chip). Registered in `lib.rs`; wired `src/osint.rs`
  (two-fetch `Source::fetch`, join + count/cap row cap 60 + `feed_detail` arm + osint chip test);
  SRC_LABEL `USGS Volcano Hazards` in `dashboard.html`. **`cargo build --release` green; full
  workspace `cargo test` green (gcrm 466 / 0 failed / 3 ignored; ee-sources 84 incl. usgs_volcano
  4/4; ee-correlate / ee-view / ee-core unchanged).** EventKind::Volcano. Next volcano target:
  Italian INGV (Etna/Stromboli) or PHIVOLCS (Philippines) if auth-free + geocoded.
- **2026-06-25** (second run) — **adopted `geonet_volcano` (GeoNet NZ Volcanic Alert Levels)** —
  a new authoritative geocoded layer that adds the **operational volcanic-alert modality** (official
  VAL 0–5 + ICAO aviation colour code) and **NZ / SW-Pacific** geography the global GVP eruption
  catalogue and EONET don't carry. Re-probed the network fresh (did not trust the 20-run block
  history): **web fetch positive control** on `raw.githubusercontent.com` correct (`facebook/react`
  `package.json` → `private:true`/no `name`); **JTWC** best-track (`metoc.navy.mil`) and **GeoNet's own
  live `api.geonet.org.nz/volcano/val`** both **403** → the egress-wide web fetch block on non-GitHub
  hosts is unchanged, so Path-A *live* verification stays impossible in-sandbox. Storm-basin extension
  ruled out this run: **IMD** (North Indian Ocean RSMC) is **auth-gated** (`api.imd.gov.in` requires
  onboarding) and **BoM** (Australian region) publishes cyclone tracks as GIS shapefiles/IDW text, not
  auth-free JSON — neither a clean fit, and the SH/IO basins are off-season anyway. **The unlock (same
  technique that landed NHC + JMA):** confirmed via **web search** that GeoNet's API is open/no-auth,
  then pulled the **real captured schema off GitHub** — `clemensv/real-time-sources`
  (`tools/candidates/volcanic/geonet-nz-volcanic.md`) carries a real `volcano/val` FeatureCollection
  example (White Island, level 2 / Orange), and `carolinaisslaying/geonet` independently asserts the
  same shape (`feature.properties.volcanoID:string`, `level:number`). So the connector + offline fixture
  are built against **real GeoNet bytes**, not docs guesswork; prod (full network) fetches the live
  `volcano/val` URL with the `Accept: application/vnd.geo+json;version=2` header. Clears all six bars:
  **authoritative** (GeoNet = GNS Science, NZ's official geological-hazard monitor); **auth-free** GeoJSON;
  **machine-readable**; **geocoded** (per-volcano Point); **fresh** (operational VAL state; an all-quiet
  network → 0 events, not an error); **non-duplicative** (GVP = a weekly *eruption* catalogue, EONET =
  *events*; neither carries NZ's standardized alert level + aviation colour). **Signal-meaningful:** drops
  VAL 0 ("no volcanic unrest", the all-clear) so only volcanoes at minor-unrest-or-above plot; severity
  ladders VAL 1→5 (0.35→1.0); chip = "Alert Level {n} · Aviation {colour}". New
  `vendor/ee-sources/src/geonet_volcano.rs` (pure `parse_geonet_val` + `val_chip` + `severity_for_level`,
  4 offline tests: real-fixture parse drops the level-0 entry, all-quiet-is-OK, error-on-bad-input,
  severity ladder). Registered in `lib.rs`; wired `src/osint.rs` (join + count/cap row cap 60 + `feed_detail`
  arm + osint chip test); SRC_LABEL `GeoNet · GNS Science` in `dashboard.html`. **`cargo build --release`
  green; full workspace `cargo test` green (gcrm 462 / 0 failed / 4 ignored; ee-sources 80 incl.
  geonet_volcano 4/4; ee-correlate 79; ee-view 60; ee-core 5).** EventKind::Volcano. Next volcano target:
  USGS HANS / VolcanoesByStatus (US/Alaska alert levels) if auth-free JSON; next storm basin still
  Indian Ocean / Southern Hemisphere if an auth-free geocoded product surfaces.
- **2026-06-25** — **adopted `jma_typhoon` (JMA RSMC Tokyo typhoons), Path A** — extends the
  storm domain from NHC's Atlantic/E-Pacific to the **Western North Pacific + South China Sea**,
  the world's most active TC basin and the one NHC structurally does not cover. Re-probed the
  network fresh (did not trust the 20-run "egress block" history): **web fetch positive control**
  on `raw.githubusercontent.com` correct (`facebook/react` `package.json` → `private:true`/no
  `name`); a four-host batch — `api.weather.gov`, `api.open-meteo.com`, USGS
  `significant_week.geojson`, GDACS `xml/rss.xml` — **all 403**, so the egress-wide web fetch
  block on non-GitHub hosts is unchanged and Path-A *live* verification stays impossible
  in-sandbox. **The unlock (same technique that landed NHC):** verified via **web search**
  (out-of-band, works) that the JMA `bosai` typhoon JSON is auth-free and widely consumed,
  then pulled the **real captured schema off GitHub** — `silenthooligan/localsky`
  `src/api/tropical.rs` carries a real archived `forecast.json` payload (typhoon IN-FA/TC2105,
  2021) plus `targetTc.json`/`pastTracks.json`, and 6+ independent repos (`skotm/wis-viewer`,
  `kumi0708/typhoon-croquette`, `aki0429/tool.yql.jp`, …) confirm the same endpoint shape. So the
  connector + offline fixtures are built against **real JMA bytes**, not docs guesswork; prod
  (full network) fetches the live `bosai` URLs. Clears all six bars: **authoritative** (JMA =
  Japan's national met service + the WMO-designated RSMC for NW-Pacific TCs); **auth-free** JSON;
  **machine-readable**; **geocoded** (per-system analysis `center` [lat,lon]); **fresh** (advisory
  cadence; empty `targetTc.json` off-season → 0 events, not an error); **non-duplicative** (NHC
  covers disjoint basins; EONET lags + lacks live category; GDACS gives only an alert level).
  **Signal-meaningful chip:** category + JMA intensity grade + max wind (kt) + central pressure
  (hPa), e.g. "Strong Typhoon · 80 kt · 950 hPa"; severity laddered off 10-min sustained wind.
  Multi-fetch handled in `Source::fetch` (index → per-TC forecast, capped at 12, a bad per-system
  fetch skipped not fatal); pure parsers `parse_targets` + `parse_jma` are offline-tested (6 tests:
  real-fixture parse picks the analysis fix over the +12/+24h forecast centres, plain-string index
  form, empty-index-is-OK, no-analysis-yields-nothing, chip grading, error-on-bad-input). Registered
  in `lib.rs`; wired `src/osint.rs` (join + count/cap row cap 60 + `feed_detail` arm + osint chip
  test); SRC_LABEL `JMA · RSMC Tokyo` in `dashboard.html`. **`cargo build --release` green; full
  workspace `cargo test` green (gcrm 459 / 0 failed / 4 ignored; ee-sources 76 incl. jma_typhoon
  6/6; ee-correlate 79; ee-view 60; ee-core 5).** EventKind::Weather (matches the NHC convention).
  Next storm target: Indian Ocean / Southern Hemisphere basins (IMD / BoM / La Réunion / Fiji RSMC)
  if auth-free + geocoded. JTWC ruled out this run (HTML/RSS only; the JSON wrappers are keyed
  third parties — Xweather, DTN).
- **2026-06-24** (second run) — **BROKE THE 20-RUN STALL: adopted `nhc` (NOAA NHC tropical
  cyclones), Path A.** The standing first pick across 15+ blocked runs finally cleared verification
  via a new technique. Re-probed the network fresh: web fetch positive control on
  `raw.githubusercontent.com` correct (`facebook/react` `package.json` → `private:true`/no name);
  NHC `CurrentStorms.json` **and** `api.open-meteo.com` (Ottawa) both **403** → egress-wide web fetch
  block unchanged, so the live endpoint still can't be hit in-sandbox. **The unlock:** instead of
  web fetch-ing the live origin (impossible), I found NHC's *real captured output committed on GitHub*
  — `jkeefe/workshops/hurricane-data/examples/CurrentStorms.json` (web fetched the raw mirror, the one
  reachable channel) and `JasonPrice70/stormcast-pro/lambda-response.json` (a full real activeStorms
  response). Both confirm the exact real-world schema: `{ "activeStorms": [{ id, name, classification
  (HU/TS/TD), intensity (kt string), pressure, latitude/latitudeNumeric, longitude/longitudeNumeric,
  movementDir, movementSpeed, lastUpdate } ] }`. So the connector + offline fixture are built against
  **real NHC bytes**, not documentation guesswork; prod (full network) fetches the live URL. Clears
  all six bars: authoritative (NOAA NHC, the primary tropical-cyclone authority), auth-free JSON,
  geocoded (per-storm lat/lon), fresh (6-hourly advisories; empty `activeStorms` off-season → 0 events,
  not an error), non-duplicative (EONET severe-storm catalog lags and lacks live category; GDACS gives
  only an alert level — neither carries live position + Saffir-Simpson category + max wind). Signal-
  meaningful chip: classification + Saffir-Simpson category + max wind (kt), e.g. "Hurricane Cat 1 ·
  75 kt", "Tropical Storm · 45 kt"; severity laddered off intensity. New `vendor/ee-sources/src/nhc.rs`
  (pure `parse_nhc` + `storm_chip`/`saffir_category`, 5 offline tests incl. empty-season-is-OK and
  error-on-bad-input); registered in `lib.rs`; wired `src/osint.rs` (join + count/cap row, cap 60, +
  `feed_detail` arm + osint chip test); SRC_LABEL `NOAA NHC` in `dashboard.html`. **`cargo build
  --release` green; full workspace `cargo test` green (gcrm 454 / 0 failed / 4 ignored; ee-sources nhc
  5/5).** EventKind::Weather (matches EONET's severeStorms convention). Next target: non-NHC basins
  (JTWC/JMA/BoM W-Pacific) if auth-free + geocoded.
- **2026-06-24** — environmental block a **TWENTIETH** consecutive session; honest **NO-OP**.
  Re-probed fresh (did not trust the prior nineteen lines). **web fetch positive control** on
  `raw.githubusercontent.com` correct (`facebook/react` `package.json` → `private:true`, no `name`);
  **NHC `CurrentStorms.json`** and the normally bot-friendly **`api.open-meteo.com`** (Ottawa
  current-temp) **both 403** → egress-wide web fetch block unchanged, **Path A still structurally
  impossible** (owner-side network policy). **Path B re-hunted via the GitHub MCP** (the one reachable
  channel) on the open gaps with fresh queries — AIS/vessel (`vessel AIS positions geojson
  pushed:>2026-05`), non-NA geography (`copernicus emergency meteoalarm flood alerts geojson
  pushed:>2026-04`; code `FeatureCollection org:eea`), conflict (`ACLED weekly aggregated conflict
  fatalities csv pushed:>2026-03`), storms (`tropical cyclone hurricane geojson github actions hourly
  pushed:>2026-05 stars:>2`), and a generic auto-refreshed feed probe (`FeatureCollection
  filename:latest.geojson pushed:>2026-06`) — **all returned 0 hits**. No authoritative body
  self-publishes a fresh geocoded event feed to `raw.githubusercontent.com`. **Chip lever re-audited
  independently** (read `feed_detail` end-to-end, lines 170–304): all 22 LIVE map feeds carry a
  meaningful, unit-bearing arm; the `_ => None` tail is reached only by the non-geo catalog
  `cisa_kev`/`cccs` and finance-panel `yahoo` — no honest offline chip edit remains, and no live data
  to verify a new band against. No code change; build green + full suite green (gcrm 450 / 0 failed /
  4 ignored); tree left clean; ledger run-log only. **Did NOT re-send a push notification**: the
  env-network block is unchanged and owner-side, already escalated 6+ times — a 20th identical alert
  is noise. Standing first pick the moment web fetch reaches gov hosts: **NHC tropical cyclones**
  (Path A, storm-domain win).
- **2026-06-23** (second run) — environmental block a **NINETEENTH** consecutive session; honest
  **NO-OP**. Re-probed fresh and **wider** (did not trust the prior eighteen lines). **web fetch
  positive control** on `raw.githubusercontent.com` correct (`facebook/react` `package.json` →
  `private:true`, no `name`); a four-host batch on distinct CDNs — **NHC `CurrentStorms.json`**,
  **USGS `significant_week.geojson`**, **Wikipedia REST** (`en.wikipedia.org/api/rest_v1`), and
  **GDACS `xml/rss.xml`** — plus normally-bot-friendly **`api.open-meteo.com`** (Ottawa current-temp)
  **all 403**. The breadth (even Wikipedia/open-meteo, which don't bot-block) re-confirms the
  restriction is **egress-wide on web fetch**, not per-host bot-protection → **Path A still
  structurally impossible** (owner-side network policy). **web search works** (out-of-band) and
  re-mapped the AIS gap: authoritative gov AIS (NOAA/USCG `marinecadastre`) is **historical
  GeoParquet bulk on GitHub, no live feed**; every real-time AIS GeoJSON is **commercial**
  (aisstream/vesselfinder/aishub) — no free authoritative live AIS exists, confirming the standing
  finding. **Path B re-hunted via the GitHub MCP** (the one reachable channel) on the open gaps —
  AIS/vessel, conflict, non-NA geography: code searches (`extension:geojson FeatureCollection
  vessel|ais|conflict pushed:>2026-05`, `filename:latest.geojson FeatureCollection pushed:>2026-05`)
  and repo searches (`live AIS vessel positions geojson github actions`, `copernicus emergency|
  meteoalarm|floodlist geojson alerts`, `JMA|BoM|geoscience australia geojson realtime`, `ACLED
  weekly aggregated conflict fatalities csv mirror`) all returned **0 hits** — no authoritative body
  self-publishes a fresh geocoded event feed to `raw.githubusercontent.com`. **Chip lever** stays
  exhausted (all 22 LIVE map feeds carry a meaningful unit-bearing `feed_detail` arm; `_ => None`
  reached only by non-geo `cisa_kev`/`cccs` and finance-panel `yahoo`) — no honest offline edit and
  no live data to verify a new band against. No code change; tree left clean; ledger run-log only.
  **Did NOT re-send a push notification**: the env-network block is unchanged and owner-side, already
  escalated 6+ times — a 19th identical alert is noise. Standing first pick the moment web fetch
  reaches gov hosts: **NHC tropical cyclones** (Path A, storm-domain win).
- **2026-06-23** — environmental block an **EIGHTEENTH** consecutive session; honest **NO-OP**.
  Re-probed fresh (did not trust the prior seventeen lines). **web fetch positive control** on
  `raw.githubusercontent.com` correct (`facebook/react` `package.json` → `private:true`, no `name`);
  **NHC `CurrentStorms.json`** and **`api.open-meteo.com`** (Ottawa current-temp) **both 403** →
  egress-wide web fetch block unchanged, **Path A still structurally impossible** (owner-side network
  policy). **Path B re-hunted via the GitHub MCP** (the one reachable channel) biased to the open
  gaps — AIS/vessel, conflict, non-NA geography: repo searches (`conflict events fatalities geojson
  auto-update pushed:>2026-05`, `AIS vessel positions geojson realtime pushed:>2026-05`, `ACLED
  weekly aggregated conflict csv pushed:>2026-04`, `meteoalarm/copernicus emergency geojson
  pushed:>2026-04`, `disaster alerts geojson hourly github actions pushed:>2026-05 stars:>3`) →
  **zero authoritative hits** (only a personal Spain risk-platform `javierdejesusda/TrueRisk` and
  awesome-lists — fail bar 1). Code searches (`filename:latest.geojson FeatureCollection
  pushed:>2026-05`; `FeatureCollection geojson org:owid`; `FeatureCollection geojson
  org:GlobalFishingWatch`) → only static boundary/AOI polygons (GFW survey AOIs), no fresh geocoded
  *events*. No authoritative body self-publishes a fresh geocoded event feed to
  `raw.githubusercontent.com`. **Chip lever** stays exhausted (all 22 LIVE map feeds carry a
  meaningful unit-bearing `feed_detail` arm; `_ => None` reached only by non-geo `cisa_kev`/`cccs`
  and finance-panel `yahoo`) — no honest offline edit and no live data to verify a new band against.
  No code change; build green + full suite green (gcrm 448 / 0 failed / 3 ignored); tree left clean;
  ledger run-log only. **Did NOT re-send a push notification**: the env-network block is unchanged
  and owner-side, already escalated 6+ times — an 18th identical alert is noise. Standing first pick
  the moment web fetch reaches gov hosts: **NHC tropical cyclones** (Path A, storm-domain win).
- **2026-06-22** (second run) — environmental block a **SEVENTEENTH** consecutive session; honest
  **NO-OP**. Re-probed fresh (did not trust the prior sixteen lines). **web fetch positive control**
  on `raw.githubusercontent.com` correct (`facebook/react` `package.json` → `private:true`, no
  `name`); **NHC `CurrentStorms.json` and `api.open-meteo.com`** (Ottawa current-temp) **both 403**
  → egress-wide web fetch block unchanged, **Path A still structurally impossible** (owner-side
  network policy). **Path B re-hunted via web search + the GitHub MCP** on the open gaps. (1)
  AIS-beyond-Baltic: chased **BarentsWatch / Kystverket** (Norwegian Coastal Admin open AIS, NLOD,
  free) — but live access is **OAuth client-credentials keyed** (register an API client on MyPage),
  so it would ship **DORMANT**, and `developer.barentswatch.no` is unreachable in-sandbox to verify
  shape/fields → not a clean Path-A win this run. (2) GitHub code search for `ais.geojson`/
  `vessels.geojson` (pushed >2026-05) → **0 hits**; repo search `conflict events fatalities geojson
  auto-update pushed:>2026-05` → **0 hits**; authoritative-org code search (`"FeatureCollection"`
  geojson in `noaa-onms`/`GlobalFishingWatch`) → only **static boundary polygons** (NOAA sanctuary
  outlines, GFW survey AOIs) — no lat/lon *events*, no freshness, no risk signal (fails the
  freshness + signal-meaningfulness bars). No authoritative body self-publishes a fresh geocoded
  event feed to `raw.githubusercontent.com`. **Chip lever re-audited independently** (read
  `feed_detail` end-to-end, lines 164–298): all 22 LIVE map feeds carry a meaningful, unit-bearing
  arm; the `_ => None` tail is reached only by the non-geo catalog `cisa_kev`/`cccs` and
  finance-panel `yahoo` — no honest offline chip edit remains, and no live data to verify a new band
  against. No code change; tree left clean; ledger run-log only. **Did NOT re-send a push
  notification**: the env-network block is unchanged and owner-side, already escalated 6+ times — a
  17th identical alert is noise. Standing first pick the moment web fetch reaches gov hosts: **NHC
  tropical cyclones** (Path A, storm-domain win).
- **2026-06-22** — environmental block a **SIXTEENTH** consecutive session; honest **NO-OP**.
  Re-probed fresh (did not trust the prior fifteen lines). **web fetch positive control** on
  `raw.githubusercontent.com` correct (`facebook/react` `package.json` → `private:true`, no
  `name`); **NHC `CurrentStorms.json` and `api.open-meteo.com`** (Ottawa current-temp) **both
  403** → egress-wide web fetch block unchanged, **Path A still structurally impossible**
  (owner-side network policy). **Path B re-hunted via the GitHub MCP** (the one reachable
  channel) across the open gaps — AIS/vessel + conflict + non-NA geography: repo searches
  (`vessel AIS positions geojson pushed:>2026-05`, `earthquake/conflict/flood geojson
  auto-update pushed:>2026-06 stars:>5`) and an authoritative-org code search
  (`"FeatureCollection" extension:geojson org:noaa-gsl path:data`) returned **zero
  authoritative hits** — only OSINT aggregators/scrapers (`BigBodyCobain/Shadowbroker`,
  `eli-labz/Third-Eye`), static boundary databases (`dr5hn/countries-states-cities-database`),
  awesome-lists and GIS tooling. Every one fails **bar 1** (authoritative gov/scientific, no
  scrapers/mirrors); no authoritative body self-publishes a fresh geocoded feed to
  `raw.githubusercontent.com`. **Chip lever re-audited independently** (read `feed_detail`
  end-to-end, lines 164–298): all 22 LIVE map feeds carry a meaningful, unit-bearing arm; the
  `_ => None` tail is reached only by the non-geo catalog `cisa_kev`/`cccs` and finance-panel
  `yahoo` — no honest offline chip edit remains, and no live data to verify a new band against
  (fabricating one would risk the "nonsense number" the signal rule forbids). No code change;
  tree left clean; ledger run-log only. **Did NOT re-send a push notification**: the env-network
  block is unchanged and owner-side, already escalated 6+ times — a 16th identical alert is
  noise, not signal. Standing first pick the moment web fetch reaches gov hosts: **NHC tropical
  cyclones** (Path A, storm-domain win).
- **2026-06-21** (second run) — environmental block a **FIFTEENTH** consecutive session; honest
  **NO-OP**. Re-probed fresh: web fetch positive control on `raw.githubusercontent.com` correct
  (`facebook/react` `package.json` → `private:true`/no name); NHC `CurrentStorms.json` **and**
  the normally bot-friendly `api.open-meteo.com` (Ottawa current-temp) both **403** → egress-wide
  web fetch block unchanged, Path A still structurally impossible (owner-side). **Path B re-hunted
  via the GitHub MCP** across both open gaps (AIS/vessel + conflict) and the geography gap: repo
  searches (`AIS vessel positions geojson`, `conflict events fatalities geojson auto-update`,
  `earthquake feed geojson updated hourly`, `meteoalarm/copernicus emergency geojson`) and code
  searches (`"FeatureCollection"` vessel/earthquake geojson) returned **zero hits** — no
  authoritative gov/scientific body self-publishes a fresh geocoded feed to
  `raw.githubusercontent.com`. **Chip lever re-audited independently** (read `feed_detail`
  end-to-end, lines 164–298): all 22 LIVE map feeds carry a meaningful, unit-bearing arm; the
  `_ => None` tail is reached only by the non-geo catalog `cisa_kev`/`cccs` and finance-panel
  `yahoo` — no honest offline chip edit remains. No code change; tree left clean; ledger run-log
  only. **Did NOT re-send a push notification**: the env-network block is unchanged and already
  escalated 6+ times — a 15th identical alert is noise. Standing first pick the moment web fetch
  reaches gov hosts: **NHC tropical cyclones** (Path A, storm-domain win).
- **2026-06-21** — environmental block a **FOURTEENTH** consecutive session; honest **NO-OP**.
  Re-probed fresh (did not trust the prior thirteen lines). **web fetch 403 on every non-GitHub
  host** — NHC `CurrentStorms.json`, `api.open-meteo.com` (Ottawa current-temp), USGS
  `significant_week` GeoJSON, **and a `*.github.io` GitHub-Pages URL (`w3c.github.io`)** — all
  403; only `raw.githubusercontent.com` resolved (positive control: `facebook/react`
  `package.json`, correctly read as `private:true`/no name). The github.io 403 newly pins the
  allowlist to **`raw.githubusercontent.com` specifically**, not "GitHub broadly" — so even
  authoritative data served from GitHub Pages is out of reach; only raw repo files are. Path A
  stays structurally impossible (egress-wide web fetch block, owner-side). **Path B re-hunted via
  the GitHub MCP** (the one reachable channel) on the two open gaps — AIS/vessel and conflict:
  repo searches (`AIS vessel positions geojson pushed:>2026-05`, `conflict events fatalities
  geojson auto-update`, `vessel tracking AIS realtime`) returned **zero authoritative hits** —
  only SDR-hobbyist feeders (`sdr-enthusiasts/docker-shipfeeder`), awesome-lists, and public-API
  indexes. A code search for `"FeatureCollection" "fatalities"` GeoJSON surfaced **237 files, all
  personal/academic/data-journalism or static historical** — OSINT scrapers/aggregators
  (`Skytuhua/SIGINT`, `AlfonsoCifuentes/riskmap`, `danielrosehill/Iran-Israel-War-2026-OSINT-Data`),
  road-crash/tornado/shooting datasets, and a 2008 WITS-derived Iraq file — every one fails
  **bar 1** (authoritative gov/scientific, no scrapers/mirrors). No authoritative body
  self-publishes a fresh geocoded feed to `raw.githubusercontent.com`. **Chip lever re-audited
  independently** (read `feed_detail` end-to-end, not trusted from the ledger): all 22 LIVE map
  feeds carry a meaningful, unit-bearing arm; the `_ => None` tail is reached only by the non-geo
  catalog `cisa_kev`/`cccs` and finance-panel `yahoo` — no honest offline chip edit remains, and
  with no live data to verify against, fabricating a band would risk the "nonsense number" the
  signal rule forbids. No code change; tree left clean; ledger run-log only. **Did NOT re-send a
  push notification** this run: the env-network block is unchanged and already escalated to the
  owner six+ times across the prior blocks — a 14th identical alert is noise, not signal
  (re-spamming degrades the channel). Standing first pick the moment web fetch reaches gov hosts:
  **NHC tropical cyclones** (Path A, storm-domain win).
- **2026-06-20** (second run) — environmental block a **THIRTEENTH** consecutive session;
  honest **NO-OP**. Re-probed fresh (did not trust the prior twelve lines). **web fetch 403 on
  every non-GitHub host** — NHC `CurrentStorms.json` *and* the normally bot-friendly
  `api.open-meteo.com` (Ottawa current-temp) both 403; only `raw.githubusercontent.com`
  resolved (positive control: `facebook/react` `package.json`, correctly read as
  `private:true`/no name). The open-meteo 403 re-confirms the restriction is **egress-wide on
  web fetch**, not per-host bot-protection → **Path A stays structurally impossible** until the
  env network policy is changed (owner-side). **Path B re-hunted via the GitHub MCP** (the one
  reachable channel): repo searches (`earthquake/conflict/flood geojson auto-update`,
  `AIS vessel positions geojson`, gov-org `geojson data`) and a code search for
  `"FeatureCollection" "fatalities"` GeoJSON returned **only personal/academic/data-journalism
  projects and static historical files** — the conflict hits (`Skytuhua/SIGINT`,
  `AlfonsoCifuentes/riskmap`, `danielrosehill/Iran-Israel-War-2026-OSINT-Data`) are OSINT
  scrapers/aggregators, exactly what **bar 1** excludes; none is an authoritative gov/scientific
  body self-publishing a fresh geocoded feed to `raw.githubusercontent.com`. **Chip lever
  re-audited independently** (read `feed_detail` end-to-end, not trusted from the ledger): all 22
  LIVE map feeds carry a meaningful, unit-bearing arm; the `_ => None` tail is reached only by the
  non-geo catalog `cisa_kev`/`cccs` and finance-panel `yahoo` — no honest offline chip edit
  remains. (Considered a third OpenSky bbox for the Asia-Pacific theaters (Taiwan/Korea, currently
  uncovered) but **rejected this run**: the existing 3-min TTL is explicitly sized to OpenSky's
  anonymous daily credit budget, so a +50% call volume risks rate-limiting and *degrading* the
  working aircraft layer — net-negative, and unverifiable here. Logged as a coverage idea, not
  shipped.) No code change; build green + full suite green (gcrm 414 / ee-correlate 79 /
  ee-sources 65 / ee-view 60 / ee-core 5; 3 ignored live tests); tree left clean; ledger run-log
  only. **Escalated to owner via push notification**: thirteen straight structurally-idle runs —
  the env network policy must allowlist gov/OSINT hosts (or unblock web fetch egress) to resume
  Path A. Standing first pick the moment web fetch reaches gov hosts: **NHC tropical cyclones**
  (Path A, storm-domain win).
- **2026-06-20** — environmental block a **TWELFTH** consecutive session; honest **NO-OP**.
  Re-probed fresh (did not trust the prior eleven lines). **web fetch 403 on every non-GitHub
  host** — NHC `CurrentStorms.json`, USGS `significant_week` GeoJSON, **and the normally
  bot-friendly `api.open-meteo.com`** — all 403; only `raw.githubusercontent.com` resolved
  (positive control returned real content: `facebook/react` `package.json`, correctly read as
  `private:true`/no name). The open-meteo 403 again confirms the restriction is **egress-wide on
  web fetch**, not per-host bot-protection → **Path A stays structurally impossible** until the env
  network policy is changed (owner-side). **web search + the GitHub MCP** (the reachable channels)
  used to re-hunt the two open gaps. **AIS-beyond-Baltic:** ran down the Danish Maritime Authority
  lead end-to-end — its live AIS is **paid** (DKK 1,800–5,600/yr), only historical 2006–2016 bulk
  CSV is free and it lives on `web.ais.dk` (403, not GitHub); the `dma-ais` GitHub org is **Java
  software libraries**, no data feed. NOAA marinecadastre (prior run) stays out (Azure GeoParquet
  bulk). **Conflict / generic geocoded feeds:** GitHub repo searches for `conflict events geojson`,
  `official AIS geojson live`, and `earthquake/flood/storm geojson auto-update` (pushed >2026-05/06)
  returned **zero** authoritative hits — no gov/scientific body self-publishes a fresh geocoded feed
  to `raw.githubusercontent.com`. `cisagov/dotgov-data` is non-geo (a .gov domain list, no lat/lon).
  **Chip lever** remains exhausted (six prior independent `feed_detail` audits all concur; no live
  data this run to verify a new band against → fabricating one would risk the "nonsense number" the
  signal rule forbids). No code change; tree left clean; ledger run-log + AIS-gap note only.
  **Escalated to owner via push notification**: twelve straight structurally-idle runs — the env
  network policy must allowlist gov/OSINT hosts (or unblock web fetch egress) to resume Path A.
  Standing first pick the moment web fetch reaches gov hosts: **NHC tropical cyclones** (Path A,
  storm-domain win).
- **2026-06-19** (second run) — environmental block an **ELEVENTH** consecutive session;
  honest **NO-OP**. Re-probed fresh (did not trust the prior ten lines). **web fetch 403 on
  every non-GitHub host** across a mixed batch — NHC `CurrentStorms.json`, USGS
  `significant_week` GeoJSON, **and the normally bot-friendly `api.open-meteo.com`** — all 403;
  only `raw.githubusercontent.com` resolved (positive control returned real content). The
  open-meteo 403 again confirms the restriction is **egress-wide on web fetch**, not per-host
  bot-protection → **Path A is structurally impossible** until the env network policy is changed
  (owner-side). **web search works** (out-of-band) and was used to re-hunt the open gaps:
  for AIS-beyond-Baltic the strongest lead surfaced was **NOAA/USCG `ocm-marinecadastre/
  ais-vessel-traffic`** — authoritative, but its data files live on **Azure blob**
  (`*.blob.core.windows.net`, 403 in-sandbox), **not** GitHub raw, in heavy **GeoParquet**
  daily/monthly bulk (historical, not live, no hand-parse) → fails the no-heavy-deps + GitHub-raw
  + freshness bars; not a Path-B fit. Conflict/flood/storm GitHub searches again returned only
  Wikipedia event articles, awesome-topic lists, and R-package wrappers — no authoritative body
  self-publishing a fresh geocoded feed to `raw.githubusercontent.com`. **Chip lever
  independently re-audited** (read `feed_detail` end-to-end, not trusted from the ledger): every
  LIVE map feed carries a meaningful, unit-bearing arm; the `_ => None` tail is reached only by
  the non-geo catalog `cisa_kev`/`cccs` and finance-panel `yahoo`. No defensible offline
  coverage/severity edit without live data to verify a band against (would risk the "nonsense
  number" the signal rule forbids). No code change; tree left clean; ledger run-log only.
  **Escalated to owner via push notification**: the routine has now been structurally idle eleven
  straight runs and needs the env network policy to allowlist gov/OSINT hosts (or web fetch egress
  unblocked) to resume. Standing first pick the moment web fetch reaches gov hosts: **NHC tropical
  cyclones** (Path A, storm-domain win).
- **2026-06-19** — environmental block a **TENTH** consecutive session; honest **NO-OP**.
  Did NOT trust the prior nine entries — re-probed fresh. **web fetch 403 on every non-GitHub
  host** across a deliberately mixed batch: NHC `CurrentStorms.json`, USGS `significant_week`
  GeoJSON, GDACS `xml/rss.xml`, an ArcGIS Hub search API, **and the normally bot-friendly
  `api.open-meteo.com`** — all 403. Only `raw.githubusercontent.com` resolved (positive
  control: `facebook/react` raw file returned real content). The open-meteo 403 again confirms
  the restriction is **egress-wide on web fetch**, not per-host CDN bot-protection → **Path A is
  structurally impossible** until the env network policy is changed (owner-side). **Path B**
  channel re-searched directly via the GitHub MCP this run (the one reachable channel), biased
  to the two open gaps — AIS-beyond-Baltic and a fresher conflict feed: repo search returned
  only **personal OSINT projects / aggregators** (`s0914712/taiwan-grayzone-monitor`,
  `BigBodyCobain/Shadowbroker`, `Wishop21/sentinel`) and World-Monitor clones (the conflict-
  dataset query returned **zero** results) — none an authoritative gov/scientific body
  self-publishing a fresh geocoded feed to `raw.githubusercontent.com` (all fail bar 1). No
  GitHub-native authoritative source to ingest. **Chip lever re-audited independently** (not
  trusted from the ledger): read `feed_detail` end-to-end — every LIVE map feed carries a
  meaningful, unit-bearing arm; the `_ => None` tail is reached only by the non-geo catalog
  `cisa_kev`/`cccs` and finance-panel `yahoo`. No defensible offline coverage/severity edit
  without live data to verify a band against (would risk the "nonsense number" the signal rule
  forbids). No code change; tree left clean; ledger run-log only. **Escalated to owner via
  push notification** (not just this ledger line, which nobody reads live): the routine has now
  been structurally idle ten straight runs and needs the env network policy to allowlist
  gov/OSINT hosts (or web fetch egress unblocked) to resume. Standing first pick the moment
  web fetch reaches gov hosts: **NHC tropical cyclones** (Path A, storm-domain win).
- **2026-06-18** (second run) — environmental block a **NINTH** consecutive session; honest
  **NO-OP**. Re-probed the network fresh and **wider** to test whether the block is per-host
  bot-protection or egress-wide: **web fetch 403 on every non-GitHub host** across two batches —
  NHC `CurrentStorms.json`, GDACS `xml/rss.xml`, MeteoAlarm legacy-atom, USGS `significant_week`
  GeoJSON, ReliefWeb `api.reliefweb.int`, NDBC `latest_obs`, **plus normally bot-friendly
  open APIs** `api.open-meteo.com`, `en.wikipedia.org` REST, an ArcGIS `services.arcgis.com`
  FeatureServer, and EMSC `seismicportal.eu` FDSNWS — **all 403**. Only `raw.githubusercontent.com`
  resolved (positive control: `facebook/react` raw file returned real content). The breadth
  (even open-meteo/Wikipedia, which don't bot-block) confirms the restriction is **egress-wide
  on web fetch to all non-GitHub hosts**, not per-host CDN protection — so **Path A is
  structurally impossible** until the environment's network policy is changed (owner-side).
  **Path B** stays dry: the one reachable channel (GitHub) was exhaustively searched across the
  prior eight runs (only personal mirrors / aggregators / awesome-lists — none an authoritative
  body self-publishing a fresh geocoded feed to `raw.githubusercontent.com`); no new authoritative
  GitHub-native source to ingest. **Chip lever re-confirmed exhausted by independent audit this
  run** (not trusted from the ledger): read `feed_detail` end-to-end — all LIVE map arms carry a
  meaningful, unit-bearing read; the `_ => None` tail is reached only by the non-geo catalog
  `cisa_kev`/`cccs` and finance-panel `yahoo`. No defensible offline coverage/severity edit
  without live data to verify a band against (would risk the "nonsense number" the signal rule
  forbids). No code change; tree left clean; ledger run-log only. **Escalated to owner** that the
  routine has now been structurally idle nine straight runs and needs the env network policy to
  allowlist gov/OSINT hosts (or web fetch egress unblocked) to resume. Standing first pick the
  moment web fetch reaches gov hosts: **NHC tropical cyclones** (Path A, storm-domain win).
- **2026-06-18** — environmental block an **EIGHTH** consecutive session; honest **NO-OP**.
  Re-probed the network fresh across four distinct hosts: **web fetch 403 on every non-GitHub
  host** — NHC `CurrentStorms.json`, ReliefWeb `api.reliefweb.int` disasters, GDACS
  `gdacs.org/xml/rss.xml`, USGS HANS `getElevatedVolcanoes` — all 403; only
  `raw.githubusercontent.com` resolved (positive control: fetched `facebook/react`
  `package.json`, got real content). So no **Path-A** gov feed was live-verifiable (NHC
  tropical cyclones — the standing first pick — 403s again). For **Path B** this run I went
  past web search and used the **GitHub repo + code search** directly (the one reachable
  channel), targeting the two stated gaps — AIS/vessel and conflict: results were entirely
  **personal OSINT projects / aggregators** (`BigBodyCobain/Shadowbroker`, `tg12/phantomtide`,
  `oliv3561/hormuz-tracker`, `s0914712/taiwan-grayzone-monitor`) and **awesome-lists**
  (`awesomedata/awesome-public-datasets`) — none is an authoritative gov/scientific body
  self-publishing a fresh geocoded feed to `raw.githubusercontent.com` (all fail bar 1). No
  authoritative GitHub-native mirror exists to ingest. **Chip lever confirmed exhausted**:
  re-audited `feed_detail` end-to-end — all 24 LIVE feeds carry a meaningful, unit-bearing arm;
  the `_ => None` tail is reached only by the non-geo catalog `cisa_kev`/`cccs` and finance-panel
  `yahoo`. No defensible coverage/severity edit without live data to verify against (fabricating
  a band risks the "nonsense number" the signal rule forbids). No code change; tree left clean;
  ledger run-log only. Standing first pick the moment web fetch reaches gov hosts: **NHC tropical
  cyclones** (Path A, storm-domain win).
- **2026-06-17** (second run) — environmental block a **SEVENTH** consecutive session; honest
  **NO-OP**. Re-probed the network fresh: **web fetch 403 on every non-GitHub host** — NHC
  `CurrentStorms.json`, GDACS `gdacsapi` event list, USGS `significant_week` GeoJSON, NGDC
  `hazel` tsunami-events service, ReliefWeb `api.reliefweb.int` disasters — all 403; only
  `raw.githubusercontent.com` resolved (positive control: fetched `facebook/react` `package.json`
  and got real content). So no **Path-A** gov feed could be live-verified (NHC tropical cyclones —
  the standing first pick — 403s as before). The **Path-B** channel again yielded **no clean
  source**: web searches for authoritative-org GitHub-hosted geocoded fresh feeds returned only
  GitHub-org/marketplace meta pages, generic dataset indexes (`awesomedata/awesome-public-datasets`),
  and software repos (`usgs/strec` ships a *static* `volcanic.geojson`, not a fresh event feed) —
  none authoritative + fresh + geocoded + non-duplicative. **Chip lever confirmed exhausted**:
  audited `feed_detail` end-to-end this run — all 25 LIVE feed arms carry meaningful, unit-bearing
  reads; the `_ => None` tail is reached only by the non-geo catalog `cisa_kev`/`cccs` and
  finance-panel `yahoo`. No defensible coverage/severity edit either — without live data to verify
  against, fabricating a band would risk the "nonsense number" the signal rule forbids. No code
  change; tree left clean; ledger run-log only. Standing first pick the moment web fetch reaches gov
  hosts: **NHC tropical cyclones** (Path A, storm-domain win).
- **2026-06-17** — environmental block a **SIXTH** consecutive session; honest **NO-OP**.
  **web fetch 403 on every non-GitHub host again**, across five distinct hosts/CDNs — NHC
  `CurrentStorms.json`, `api.weather.gov` active alerts, USGS `significant_week` GeoJSON,
  GDACS `gdacsapi` event list, MeteoAlarm `feeds.meteoalarm.org` legacy atom — all 403;
  only `raw.githubusercontent.com` resolved (positive control: fetched a known public raw
  `package.json` and got real content). So no **Path-A** gov feed could be live-verified
  (NHC tropical cyclones — the standing first pick — 403s as before; Atlantic season is open
  so there may be live storms, but I can't confirm shape/freshness from here). The **Path-B**
  channel (`raw.githubusercontent.com`) again yielded **no clean source**: fresh web searches
  for authoritative-org GitHub-hosted geocoded event feeds returned only **personal
  aggregators / mirrors** (`beyondtracks/act-esa-incidents-geojson`, `zhukovyuri/VIINA`
  academic news-scraped Ukraine ML events — all fail bar 1: authoritative, no scrapers/
  mirrors), **retrospective/stale** databases (`cghss/dons` — WHO DON *retrospective* archive,
  not fresh), and **already-live / duplicative** coverage (GDACS, UCDP conflict). None cleared
  authoritative + fresh + geocoded + non-duplicative. **No chip improvement was available
  either** — re-confirmed the chip lever stays exhausted (documented across the prior five
  runs): every LIVE map layer carries a meaningful, unit-bearing `feed_detail` arm, and the
  `_ => None` tail is reached only by the non-geo catalog `cisa_kev`/`cccs` and finance-panel
  `yahoo`. The #1 stated gap (AIS beyond the Baltic) still needs a *different* authoritative
  source and is blocked by the same network reality. No code change; tree left clean;
  ledger run-log only. Standing first pick the moment web fetch reaches gov hosts: **NHC
  tropical cyclones** (Path A, storm-domain win).
- **2026-06-16** (second run) — environmental block a **FIFTH** consecutive session; honest
  **NO-OP**. **web fetch 403 on every non-GitHub host again** — NHC `CurrentStorms.json`,
  `api.weather.gov` active alerts, GDACS `gdacsapi` event list, USGS `significant_week`
  GeoJSON — all 403; only `raw.githubusercontent.com` resolved (positive control against a
  known public raw file). So no Path-A feed live-verifiable. Path B (`raw.githubusercontent.com`)
  again yielded **no clean source**: fresh web searches for authoritative-org GitHub-hosted
  geocoded event feeds returned only **personal mirrors** (`beyondtracks/act-esa-incidents-geojson`,
  `PetraLee2019/...`, assorted USGS-feed visualizers — all fail bar 1), **static boundary
  files** (`georgique/world-geojson`, `gregoiredavid/france-geojson` — not events), and the
  official USGS/NOAA/NASA feeds living only on their 403-ing origin hosts. None authoritative +
  fresh + geocoded + non-duplicative. Standing first pick unchanged: **NHC tropical cyclones**
  (Path A) the moment web fetch reaches gov hosts. Re-confirmed the **chip lever stays exhausted**:
  audited `feed_detail` again end-to-end — every LIVE map layer carries a meaningful, unit-bearing
  arm (the `_ => None` tail is hit only by the non-geo catalog `cisa_kev`/`cccs` and finance-panel
  `yahoo`), so no honest signal-meaningfulness fix remained either. No code change; tree clean;
  ledger-only commit.
- **2026-06-16** — environmental block a FOURTH consecutive session, and this run found the
  signal-meaningfulness lever **exhausted** too, so an honest **NO-OP** (per the routine's "do
  not half-wire a source to look busy"). **web fetch was 403 on every non-GitHub host again**:
  NHC `CurrentStorms.json`, MeteoAlarm `live/rss`, GDACS `gdacsapi` event list, ECCC
  `api.weather.gc.ca` OGC SWOB — all 403; only `raw.githubusercontent.com` resolved (confirmed
  positive against a known public raw file). So no Path-A feed could be live-verified. The
  Path-B channel (`raw.githubusercontent.com`) again yielded **no clean source**: web searches
  for authoritative-org GitHub-hosted fresh geocoded feeds returned only static boundary files
  (`UK-GeoJSON`, `world-geojson` — not events), **personal UCDP mirrors** (`optgeo/ucdp-*` —
  fail bar 1 *and* duplicate the live `ucdp_ged`), **non-geo** CISA data (`cisagov/dotgov-data`),
  and licensed **ACLED**. None fresh + authoritative + geocoded + non-duplicative. Candidates
  ruled out *this run only* (re-evaluate when web fetch reaches gov hosts; none REJECTED): **NHC**
  tropical cyclones (still the top Path-A pick), **MeteoAlarm** Europe, **ECCC SWOB-realtime**
  (OGC API — but raw obs would need a per-station baseline to mean anything, cf. the hydrometric
  rejection). Unlike the prior three runs, **no chip improvement was available**: audited
  `feed_detail` end-to-end — every LIVE map layer now has a meaningful arm (the `_ => None` tail
  is hit only by the non-geo catalog sources `cisa_kev`/`cccs` and the finance-panel `yahoo`),
  and the two newest connectors already surface human labels not raw codes (`ucdp_ged`
  type_of_violence→State-based/Non-state/One-sided; `digitraffic_ais` navStat→Aground/NUC/…).
  The #1 stated gap (AIS beyond the Baltic) needs a *different* authoritative source — Fintraffic
  returns its whole coverage area, no bbox to widen — so it's blocked by the same network reality.
  No code change; tree left clean; ledger run-log updated. Next run, if web fetch reaches gov
  hosts: **NHC tropical cyclones** (Path A, storm-domain win) is the standing first pick.
- **2026-06-15** (third run) — environmental block a THIRD consecutive session, so a verified
  signal-meaningfulness fix on an existing layer rather than a half-wired source. **web fetch
  was 403 on EVERY non-GitHub host again** (NHC `CurrentStorms.json`, USGS `significant_week`
  GeoJSON, GDACS API, MeteoAlarm `feed.meteoalarm.org`, NOAA NGDC tsunami service) — only
  `raw.githubusercontent.com` resolved (confirmed positive against a known public raw file).
  So no Path-A feed could be live-verified, and the only Path-B-eligible channel
  (`raw.githubusercontent.com`) yielded **no clean source**: searches for GitHub-Actions-
  refreshed authoritative geocoded feeds surfaced only **personal/aggregator mirrors**
  (`beyondtracks/act-esa-incidents-geojson`, `jalbertbowden/us-data`, `simonhuwiler/
  russo-ukrainian-data-ressources`) — all fail bar 1 (authoritative, no scrapers/mirrors) —
  plus already-live UCDP and licensed ACLED. None fresh + authoritative + geocoded +
  non-duplicative. Candidates ruled out *this run only* (re-evaluate when web fetch reaches
  gov hosts; none REJECTED): **NHC** tropical cyclones (still the top Path-A pick),
  **MeteoAlarm** Europe, **NGDC** tsunami events. Instead, **closed the last `feed_detail`
  gap among LIVE feeds**: `eccc_marine` (Great-Lakes / Canadian marine warnings) was the
  only live source with no chip arm → it hit `_ => None` and plotted bare dots. Its title
  already names the hazard ("Gale warning — Lake Ontario"), but the *wind speed that name
  denotes* is not something a watch-floor operator carries by heart, so the new arm maps the
  warning name → its **standardized ECCC mean-wind band with units** (Strong wind 20–33 kn /
  Gale 34–47 kn / Storm 48–63 kn / Hurricane-force ≥64 kn; "storm surge" excluded as a
  water-level, not wind, hazard); non-wind hazards (freezing spray, etc.) degrade to the
  alert tier (Warning/Watch). Bands verified against ECCC's published Canadian Marine Warning
  Program definitions. Offline test added (`wind_chip_maps_named_warning_to_its_band`);
  `cargo build --release` + full workspace suite green (gcrm 394 / 0 failed / 3 ignored;
  ee-sources 65; ee-view 60; ee-correlate 79; ee-core 5). Every LIVE map layer now carries a
  meaningful popup chip. Next run, if web fetch reaches gov hosts: pick up **NHC tropical
  cyclones** (Path A) as the new-domain (storm) win.
- **2026-06-15** (second run) — environmental block again, so a verified signal-meaningfulness
  fix on an existing layer rather than a half-wired source. **web fetch was 403 on EVERY
  non-GitHub host this session** — not just CDN-fronted gov hosts but normally bot-friendly
  ones too (NHC `CurrentStorms.json`, GDACS API, MeteoAlarm, JMA quake list, USGS GeoJSON,
  ReliefWeb API, EMSC `seismicportal.eu`, `api.open-meteo.com`, Wikipedia API). Only
  `raw.githubusercontent.com` resolved (confirmed positive against a known public raw file).
  So **no Path-A feed could be live-verified** (NHC, teed up last run, 403s) and **no Path-B
  snapshot could be built** (can't reach any origin to mirror it; the GitHub-mirrored
  conflict datasets a search surfaced were licensed (ACLED) or already-live (UCDP) — none
  fresh + authoritative + geocoded + non-duplicative). Candidates ruled out *this run only*
  (re-evaluate when web fetch reaches gov hosts; none REJECTED): **NHC** tropical cyclones
  (Path-A storm-domain win, still the top pick), **MeteoAlarm** Europe, **JMA** quakes
  (duplicative). Instead, **closed a signal gap on the OpenSky Aircraft layer** (up to 800
  plotted dots): it had **no `feed_detail` arm**, so every aircraft showed only "Aircraft" +
  time — a bare dot with no identifying read. Added a chip from OpenSky's state vector:
  emergency squawk first (`7500` hijack / `7600` radio-failure / `7700` emergency — the only
  intrinsic alert), else barometric altitude + ground speed in aviation units (`"36089 ft ·
  447 kn"`), else `"On ground"`. Offline test added; `cargo build --release` + full workspace
  suite green (gcrm 393 passed / 0 failed / 3 ignored; ee-sources 64; ee-view 60; ee-correlate
  79; ee-core 5). Next run, if web fetch reaches gov hosts: pick up **NHC tropical cyclones** (Path A).
- **2026-06-15** — no new source cleared the bar (environmental block), so a verified
  signal-meaningfulness fix instead. **web fetch was broadly 403 this session**: every
  CDN-fronted gov/OSINT host the fetcher tried returned HTTP 403 (NHC `CurrentStorms.json`,
  JMA quake list, NOAA `api.weather.gov`, USGS feeds, NASA EONET, EMSC `seismicportal.eu`,
  GDACS API, ReliefWeb API, GDELT geo, Wikipedia) — only `raw.githubusercontent.com`
  resolved. So no new **Path-A** gov feed could be live-verified, and the GitHub-hosted
  (**Path-B**-eligible) datasets found were either stale (GDIS ends 2018) or duplicative
  (USGS/quake mirrors) — none fresh + non-duplicative. Candidates ruled out *this run only*
  (re-evaluate when web fetch can reach gov hosts; none are REJECTED): **NHC** active
  tropical cyclones (`CurrentStorms.json` — new domain, would fill a storm gap; can't
  verify today), **JMA** quakes (duplicative of USGS/EMSC), **MeteoAlarm** Europe (geocode
  risk — region codes, not per-record lat/lon; unverified), **ReliefWeb** disasters
  (UN OCHA but country-centroid + duplicative of GDACS), **GDELT** geo conflict (geocode/
  meaningfulness risk, not strictly authoritative). Instead, **closed a signal gap on an
  existing top-tier layer**: `gdacs` (the global multi-hazard layer) had **no `feed_detail`
  arm**, so every disaster plotted as a bare dot with no severity. Added a chip surfacing
  the authoritative **alert level + hazard type + GDACS `severitydata.severitytext`**
  (e.g. "Orange · Earthquake · Magnitude 6.1M, Depth:10km"); long severity sentences are
  dropped so the chip can't dump a paragraph, degrading gracefully to "Red · Cyclone".
  Offline test added; `cargo build --release` + full suite green (391 passed, 0 failed, 3
  ignored). Next run, if web fetch reaches gov hosts: pick up **NHC tropical cyclones**
  (Path A) as the new-domain win.
- **2026-06-14** — filled the two biggest gaps with verified live feeds (Path A):
  `digitraffic_ais` (Fintraffic Baltic AIS → the empty Vessel layer; abnormal-nav-state
  loud, moving commercial faint, routine dropped; join locations+vessels by MMSI) and
  `ucdp_ged` (UCDP candidate-GED CSV → the Conflict layer ACLED can't fill; fatalities→
  severity, version-discovered URL, quote-aware CSV parser). Both live `errors=[]`
  (digitraffic_ais 800 / ucdp_ged 800); 64 ee-sources + 389 workspace tests green; clippy clean.
- **2026-06-14** (prompt optimization, not a hunt) — recorded two facts that reshape the
  hunt: (1) the cloud sandbox is GitHub-only, so live-verify via web fetch + added the
  Path-A/Path-B ingestion model above; (2) ACLED Open access has no API (paid license only)
  — `acled` is permanently dormant as a live feed; aggregated-weekly is a Path-B candidate.
  Next-target priority set to the empty Vessel/AIS layer.
- **2026-06-14** — Ledger seeded from the manual Canadian-feed recon (15 candidates,
  11 confirmed). Adopted to the map: `drivebc`, `alberta511`, `quebec511`,
  `cwfis_activefires`, `cbsa_bwt`, `navcanada`. Added `cccs` as a non-map registry
  connector. Rejected Alert Ready, hydrometric, NTWC tsunami, space weather (reasons above).
  All live with `errors=[]`; build + full test suite green.
