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
| `acled_aggregated` | Conflict | ACLED | **weekly Admin-1 conflict-intensity** — ACLED's free, no-key **Aggregated Data** product (the licensed event API stays dormant; see `acled`). One dot per first-level admin region at its **centroid** (the file ships `CENTROID_LATITUDE/LONGITUDE`, so no external centroid table is needed), coloured by the **trailing-window sum** (~4 weeks ending at the file's latest `WEEK`) of events + fatalities — so a multi-year file plots *current* regional heat, not history. A regional-intensity complement to `ucdp_ged`'s discrete events: weekly cadence + ACLED's broad taxonomy (political violence / explosions-remote / demonstrations / strategic developments). **Path-B committed snapshot** (`acled_aggregated_snapshot.csv`, `include_str!`): `acleddata.com`/HDX 403s in-sandbox and the download is a manual/registered step, so a real ACLED Middle-East weekly aggregate (Jan–Mar 2026; 14 countries, 104 admin1s incl. Iran/Israel/Palestine/Lebanon/Syria/Yemen/Iraq) ships embedded, refreshed by a local re-download job. Canonical 13-col schema (`WEEK,REGION,COUNTRY,ADMIN1,EVENT_TYPE,SUB_EVENT_TYPE,EVENTS,FATALITIES,POPULATION_EXPOSURE,DISORDER_TYPE,ID,CENTROID_LATITUDE,CENTROID_LONGITUDE`) confirmed against 15+ independent public copies. Severity = log(fatalities) (UCDP ladder); zero-fatality-but-active region floors at 0.12. **Signal-meaningful** (event + fatality counts are inherently unit-bearing conflict measures). Chip = "41 events · 66 fatalities · Air/drone strike". |
| `digitraffic_ais` | Vessel | Fintraffic (Finland) | live Baltic AIS — vessels in abnormal nav state (aground/NUC/restricted) loud, moving commercial traffic faint; routine moored/anchored dropped. Auth-free (Digitraffic-User header + gzip). Fills the previously-empty Vessel layer; Baltic = on-mission (NATO/Russia maritime). |
| `asam` | Vessel | NGA (US) | **worldwide anti-shipping hostile-act reports** — **DORMANT: the upstream is dead** (local live-verification 2026-07-04, hours after adoption). NGA's MSI API no longer serves the product: `msi.nga.mil/api/publications/asam` (and `/asam/areas`) return an **application-level 404** even with a valid WAF session — while the sibling `publications/broadcast-warn` returns 200 on the same session, so the API stack itself is alive and ASAM specifically is gone (NGA's own SPA still calls the removed path, i.e. their Piracy page is broken too). Corroboration: the Esri Living Atlas partner mirror (`esri_livefeeds2` `ASAM_events_V1` FeatureServer, "sourced from NGA") **froze at newest incident 2024-06-25** (9,182 records), and the NGA reference apps are archived — the product appears to have stopped updating ~mid-2024, long before adoption; the in-sandbox 403 masked this (schema anchoring proved the *shape*, not *liveness*). The connector (`vendor/ee-sources/src/asam.rs`), its escalation-class severity ladder (armed attack 0.9 > boarding 0.65 > robbery 0.5 > attempted 0.3), chip, and fixture tests all stay green and registered, ready for a live successor; only the `src/osint.rs` fan-out fetch was removed (it burned a 12s slot + a perpetual errors[] "HTTP 404" per rebuild). |
| `nhc` | Weather | NOAA NHC | active tropical cyclones (Atlantic / E+C Pacific) from `CurrentStorms.json` — live position, classification (HU/TS/TD), max wind (kt)→Saffir-Simpson category + severity, pressure. Auth-free JSON, U.S. public domain. Empty `activeStorms` (off-season) = 0 events, not an error. Fills the storm/cyclone gap EONET (lagging catalog) and GDACS (alert level only) don't cover operationally. |
| `jma_typhoon` | Weather | JMA / RSMC Tokyo | active typhoons over the **Western North Pacific + South China Sea** — the basin NHC does NOT cover (NHC = Atlantic/E-Pacific only). JMA is the WMO-designated RSMC for this basin. `bosai` JSON: `targetTc.json` index → per-system `{tcId}/forecast.json`; the connector emits the *analysis* part (current fix: `center` [lat,lon], `pressure` hPa, `maximumWind.sustained.knots`, `category.en`). Chip = category + JMA intensity grade (Strong/Very Strong/Violent Typhoon) + wind (kt) + pressure (hPa). Auth-free, multi-fetch (index + per-TC), empty index off-season = 0 events not an error. |
| `geonet_volcano` | Volcano | GeoNet / GNS Science | New Zealand **Volcanic Alert Levels** — the `volcano/val` GeoJSON: per-volcano official VAL (0–5) + ICAO aviation colour code (`acc`: Green/Yellow/Orange/Red) + plain-language activity/hazards. Connector drops VAL 0 ("no unrest") and plots only volcanoes at level ≥ 1, so an all-quiet network = 0 events (not an error). Auth-free GeoJSON (`Accept: application/vnd.geo+json;version=2`). Fills the **operational alert-level** modality and **NZ / SW-Pacific** geography that the global GVP eruption catalogue and EONET (event-based) don't carry. Chip = "Alert Level {n} · Aviation {colour}". CC BY 3.0 NZ. |
| `usgs_volcano` | Volcano | USGS VHP / HANS | **US + Alaska Volcanic Alert Levels** — the HANS `getElevatedVolcanoes` notice product (ground alert level NORMAL/ADVISORY/WATCH/WARNING + ICAO aviation colour GREEN/YELLOW/ORANGE/RED) **joined by `vnum`** to the `getUSVolcanoes` catalogue for coordinates (the elevated notice carries no lat/lon). Drops the all-clear state (NORMAL/GREEN/UNASSIGNED) so only volcanoes above background plot; all-quiet network = 0 events, not an error. Auth-free JSON, US-Gov public domain. Chip = "Alert {level} · Aviation {colour}". Fills the **US/Alaska** operational alert-level geography GeoNet (NZ) doesn't cover and that the GVP eruption catalogue / EONET (event-based) don't carry as a live alert state. AVO (most active US volcanoes), HVO (Kīlauea/Mauna Loa), Cascades, Yellowstone. |
| `nwps_flood` | Weather | NOAA / NWS NWPS | **river flooding** — the NWS `water/riv_gauges` "Observed River Stages" GeoJSON layer (id 0), one Point per AHPS gauge with its observed **flood category** in `status`. Connector keeps only gauges **at/above action stage** (`action`/`minor`/`moderate`/`major`); drops the all-clear `no_flooding` + undefined states, so a no-flood network = 0 events, not an error. Auth-free GeoJSON, US-Gov public domain. **Signal-meaningful where a raw gauge level isn't:** `status` is the *baseline-relative* category (NWS already compared live stage to that gauge's own thresholds) — resolves the `ECCC hydrometric` "nonsense number" rejection at its root. Severity major 1.0→action 0.35; chip = "Major flooding" / "Near flood stage". Fills the river-flooding hazard no current feed carries. |
| `magma_volcano` | Volcano | PVMBG / MAGMA Indonesia | **Indonesian volcano operational alert levels** — PVMBG (Geological Agency, Ministry of Energy & Mineral Resources) is the authoritative national monitor for the world's most volcanically active country (~127 monitored volcanoes). Each volcano carries a ground alert level `ga_status` on the 4-step scale (1 Normal / 2 Waspada / 3 Siaga / 4 Awas) plus the latest VONA aviation colour (`vona[].cu_avcode` GREEN/YELLOW/ORANGE/RED). Connector emits one event per volcano **above background** (status ≥ 2); Normal (1) dropped, so an all-quiet snapshot = 0 events. Severity = max(status rank, aviation-colour rank) — an ash-erupting Waspada volcano flagged ORANGE plots at 0.8, not 0.55. Chip = "Alert Siaga (Watch) · Aviation Yellow". **Path B (committed snapshot):** MAGMA's home-map volcano list is embedded server-side (no clean public full-list JSON endpoint) and the host 403s in-sandbox, so a real captured PVMBG payload ships `include_str!`-embedded (`magma_volcano_snapshot.json`), refreshed by a local/manual re-capture job. Schema confirmed against PVMBG's own `magma-indonesia/magma-indonesia` source (`HomeController::gunungApi()` + `VonaApiService`). Fills the **Indonesia / SE-Asia** volcano geography GeoNet (NZ) and USGS (US/Alaska) don't cover. |
| `avalanche_ca` | Weather | Avalanche Canada | **snow-avalanche danger ratings** — Canada's national public-avalanche body. Joins `/forecasts/en/products` (bulletins, by `area.id`) to `/forecasts/en/areas` (region polygons, GeoJSON, same id) and plots one dot per region **with a numeric rating today**, at the polygon centroid. Severity = peak elevation-band rating on the North American scale (Low 0.2 → Extreme 1.0). **Signal-meaningful** (the danger scale is baseline-relative — each level a defined likelihood/size, not a raw number). **Seasonal, handled honestly:** off-season bands read `norating`/spring → dropped, so summer = 0 events not an error (layer lights up ~late-Nov→Apr). Resolves the source's deferral via an off-season-tolerant parser. Auth-free JSON+GeoJSON. Chip = "Alpine Considerable · Treeline Moderate · Below Low". Fills the snow-avalanche hazard no other feed carries. |
| `awc_sigmet` | Weather | NOAA / NWS Aviation Weather Center | **international SIGMETs** — the en-route aviation hazard warnings each Meteorological Watch Office issues for its Flight Information Region, aggregated by AWC at `api/data/isigmet?format=geojson`. One `Polygon` feature per active SIGMET, plotted at the **hazard-polygon centroid**, carrying the hazard type (`hazard`: TS/TC/VA/TURB/ICE/DS/SS/MTW/GR/IFR…), an intensity/coverage qualifier (`qualifier`: SEV/EMBD/ISOL/OCNL/FRQ…), the affected flight-level band (`base`/`top`, feet MSL), the issuing FIR (`firName`), and the raw SIGMET text. Opens an **en-route aviation-hazard modality** no feed carried (distinct from NWS/ECCC ground warnings, NHC/JMA cyclone *tracks*, and NWPS river flooding) with **global FIR geography**; pairs with the live aircraft (`opensky`) + NOTAM (`navcanada`) layers. **Signal-meaningful:** every value is a named WMO aviation phenomenon + standardized intensity + flight levels (no raw scalar). Severity = max(hazard base [VA/TC 0.9 → IFR 0.4], qualifier bump [SEV 0.85 / HVY 0.7]). Chip = "Severe Turbulence · FL170–330" / "Embedded Thunderstorms · to FL430". Empty `FeatureCollection` (no active intl SIGMETs in scope) = 0 events, not an error. **Path B (mirrored-snapshot verification):** the live host 403s in-sandbox, so the parser + fixture are anchored to a **real captured AWC intl-SIGMET payload** committed on GitHub (`thomasdubdub/sigmet-sectors/20200316.json` — genuine SBRE RECIFE EMBD-TS / LFRR BREST SEV-TURB records); prod (full network) fetches the live `format=geojson` endpoint. Auth-free, US-Gov public domain. |
| `spc_storm_reports` | Weather | NOAA / NWS Storm Prediction Center | **confirmed severe-storm reports** — SPC's daily Local Storm Reports, the **ground-truth severe-convective occurrences** (the touchdown/impact, not a forecast) no feed carried: NWS/ECCC ship *warnings* (what may happen), NHC/JMA ship cyclone *tracks*, NWPS ships river flooding, AWC ships en-route aviation hazards. Three small CSVs (`today_torn.csv` / `today_hail.csv` / `today_wind.csv`), each the same 8-column layout `Time,<F_Scale\|Size\|Speed>,Location,County,State,Lat,Lon,Comments`; first line is a header, the free-text `Comments` may contain commas (parsed `splitn(8,',')`). One [`EventKind::Weather`] event per report at its own lat/lon. **Signal-meaningful** (every value unit-bearing + baseline): a confirmed **tornado** (EF rating when assessed → EF0 0.6…EF5 1.0; unrated touchdown 0.85), **hail** diameter in inches (severe ≥1.0", significant ≥2.0"; the daily `Size` is hundredths-of-inch — `175`=1.75" — handled tolerant of either hundredths or decimal-inch encoding), **wind** gust in mph (severe ≥58, significant ≥75, extreme ≥90; unknown-speed damaging-wind 0.5). Chip = "EF2 Tornado" / "2.75 in hail" / "70 mph wind" / "Damaging wind". Empty report day (header only — common early in the UTC day / quiet weather) = 0 events, not an error; if NONE of the three is a recognizable report CSV (e.g. all HTML 403 pages) → error so last-good takes over. **Path A (prod fetches live):** SPC `today_*.csv` is auth-free US-Gov public domain; the host 403s web fetch in-sandbox (as every gov host does), so the format was **anchored to real consumer-code bytes** — `garrettrayj/storm-reports` `src/downloader.py` (URL `…/climo/reports/{YYMMDD}_rpts_{torn,hail,wind}.csv`) + `src/preprocessing.py` (8-field row regex), corroborated by multiple independent sources for the per-type headers + units; prod (full network) fetches the live `today_*.csv`. US/CONUS geography. |
| `bmkg_quake` | Earthquake | BMKG / InaTEWS (Indonesia) | **felt earthquakes** — BMKG's open `gempadirasakan.json` (the ~15 most recent quakes actually reported felt). NOT another USGS/EMSC *detection* catalogue: this is a **human-impact** product — only felt quakes, each graded by the **Modified-Mercalli felt intensity** (`Dirasakan`, e.g. "IV Denpasar, III Mataram") plus Indonesia's national **tsunami-potential** flag (`Potensi`). One dot per quake at its inline `Coordinates` ("lat,lon" — no geometry join). Severity = MMI ladder (II 0.25 → VI 0.7 → IX+ 1.0), with a raw-magnitude fallback when `Dirasakan` is blank, floored by any tsunami potential (Waspada 0.9 / Siaga 0.95 / Awas 1.0). **Signal-meaningful** (MMI is a defined ground-shaking scale, each level a named effect — baseline-relative, not a raw number; the tsunami flag is the official InaTEWS assessment). Chip = "Felt MMI IV · M4.8" / "Felt MMI VI · M6.2 · Tsunami Siaga". Auth-free JSON (attribution "BMKG"); empty quiet-window list = 0 events, not an error; `gempa` tolerated as array (felt list) or single object (latest). **Path A** (prod fetches the live `gempadirasakan.json`; the host 403s web fetch in-sandbox so the schema is anchored to the official `infoBMKG/data-gempabumi` spec + 5+ independent public copies). Fills the **felt-intensity / tsunami modality** and **Indonesia / SE-Asia** seismic geography the raw global quake catalogues (USGS/EMSC/eqcanada) don't carry. |
| `jma_quake` | Earthquake | JMA (Japan Meteorological Agency) | **seismic-intensity earthquakes** — JMA's open `bosai/quake/data/list.json` (the rolling list of recent quake bulletins), filtered to events with an observed **JMA Shindo intensity** (`maxi`) on Japan's national 0–7 scale (`1,2,3,4,5-,5+,6-,6+,7`). NOT another USGS/EMSC *detection* catalogue: filtered to a Shindo it's a **human-impact** product — only quakes that produced measurable shaking — over **Japan / the NW-Pacific** (a key non-North-America theatre). One dot per quake at its inline `cod` (an ISO-6709 string `+lat+lon-depth/` — no geometry join). **Deduped by `eid`** (JMA issues several bulletins per quake: intensity flash → hypocentre+intensity → updates), keeping the loudest Shindo. Bulletins with no hypocentre (`cod` empty — the `震度速報` flash) or no observed Shindo (a hypocentre-only notice for an unfelt quake) are dropped — exactly what USGS/EMSC already carry. Severity = Shindo ladder (1 → 0.15, 5+ → 0.75, 7 → 1.0). **Signal-meaningful** (Shindo is a defined ground-shaking scale, each level a named effect — baseline-relative, not a raw number; a distinct national scale from Indonesia's MMI). Chip = "Shindo 5+ · M6.1". Auth-free JSON (attribution "気象庁/JMA"); empty array (quiet window) = 0 events, not an error. **Path A** (prod fetches the live `list.json`; the host 403s web fetch in-sandbox so the schema is anchored to committed GitHub bytes — the `nehemiaharchives/jma-quake-api` `JmaQuakeData.kt` data class: `cod/mag/maxi/anm/en_anm/ttl/en_ttl/eid/at` fields confirmed). Complements `bmkg_quake` (Indonesia MMI) — same `bosai` host already proven live by `jma_typhoon`. |
| `geonet_quake` | Earthquake | GeoNet / GNS Science (New Zealand) | **felt earthquakes** — GeoNet's open `quake?MMI=3` GeoJSON, filtered server-side to quakes whose **computed Modified-Mercalli intensity** (`mmi`, the calculated shaking at the closest locality) reaches the felt threshold. NOT another USGS/EMSC *detection* catalogue: filtered to a felt MMI it's a **human-impact** product — only quakes that actually shook people — over **New Zealand / the SW-Pacific** (a seismically very active plate boundary the global catalogues carry only sparsely at small magnitudes). One dot per quake at its inline `Point` `[lon,lat]` (no geometry join). Retracted quakes (`quality == "deleted"`) and any feature below the MMI-3 floor / without geometry are dropped, so a quiet window (empty `features`) = 0 events, not an error. Records may omit `time` (real GeoNet behaviour) → "now" fallback so a live-but-timeless quake still plots. Severity = MMI ladder aligned with `bmkg_quake` (3 → 0.3, 6 → 0.7, 8 → 0.95, 9+ → 1.0). **Signal-meaningful** (MMI is a defined ground-shaking scale, each level a named human effect — baseline-relative, not a raw number; same scale as Indonesia's `bmkg_quake`, distinct national body + geography). Chip = "Felt MMI 5 · M5.9". Auth-free GeoJSON (CC BY 3.0 NZ, credit "GeoNet / GNS Science"; `Accept: application/vnd.geo+json;version=2`). **Path A** (prod fetches the live `quake?MMI=3`; the host 403s web fetch in-sandbox so the schema is anchored to committed GitHub bytes — the real `exxamalte/python-aio-geojson-geonetnz-quakes` `tests/fixtures/quakes-1.json` capture: `publicID/time/depth/magnitude/mmi/locality/quality` fields + a record that omits `time`, corroborated by GeoNet's official API docs). Same `api.geonet.org.nz` host already proven live in prod by `geonet_volcano`. Completes the felt-intensity seismic trio: `bmkg_quake` (Indonesia MMI) + `jma_quake` (Japan Shindo) + `geonet_quake` (NZ MMI). |
| `odlinfo` | Other (Radiation) | BfS (Germany) | **ambient gamma dose rate** — the Bundesamt für Strahlenschutz ODL-Info **OGC WFS opendata** layer `odlinfo_odl_1h_latest` (`imis.bfs.de/ogc/opendata/ows`, `outputFormat=application/json`): a GeoJSON `FeatureCollection`, one `Point` per one of ~1,700 fixed stations across Germany carrying its latest 1-hour mean dose rate in `value` (µSv/h, `Gamma-ODL-Brutto` = cosmic + terrestrial), an ISO `end_measure`, `id`/`kenn`/`name`/`plz`, and `site_status` (1 in operation / 2 defective / 3 test). Opens a **radiation / nuclear-monitoring modality no other feed carries** — a first-order WWIII-risk observable (reactor release / detonation / dispersal) over a NATO frontline state. **Signal-meaningful where a raw gauge isn't** (resolves the ECCC-hydrometric trap at its root): a dose rate in µSv/h has a **universal natural-background baseline** (~0.05–0.20 µSv/h everywhere), so an elevation is interpretable *without* a per-station table. The connector plots **only stations elevated above background** (`value` ≥ 0.3 µSv/h, clearing normal + local geology); all background stations drop, so an all-normal network — the healthy peacetime state — is 0 events, not an error, and the layer lights up precisely when radiation rises (the `usgs_volcano` / `nwps_flood` drop-the-all-clear pattern). Non-operational stations (defective/test) drop so a stuck/garbage reading can't false-alarm. Severity ladder: above-normal 0.4 (≥0.3) → elevated 0.5 (≥0.5) → high 0.7 (≥1.0) → very high 0.9 (≥10) → extreme 1.0 (≥100). Chip = "0.45 µSv/h · Above normal" / "3.10 µSv/h · High". `EventKind::Other` (the catch-all for a new modality before it earns a first-class variant → renders in the "Other Signals" layer; promoting it to a first-class Radiation layer is the self-improvement routine's lane). **Path A** (prod fetches the live WFS; the host 403s web fetch in-sandbox so endpoint + schema are anchored to committed GitHub bytes — the authoritative `bundesAPI/strahlenschutz-api` `openapi.yaml`: server `imis.bfs.de/ogc/opendata/ows`, **no security scheme (auth-free)**, FeatureCollection of ExtendedFeature with `value`/`unit "µSv/h"`/`id`/`kenn`/`name`/`end_measure`/`site_status`/`nuclide "Gamma-ODL-Brutto"`/`duration "1h"` + Point `[lon,lat]`, example value 0.124). Auth-free open data, Datenlizenz Deutschland – Namensnennung 2.0 (credit "© Bundesamt für Strahlenschutz (BfS)"). |
| `stuk_radiation` | Other (Radiation) | STUK / FMI (Finland) | **external radiation dose rate** — Finland's ~255-station automatic monitoring network (10-min cadence), served through the **Finnish Meteorological Institute open-data WFS** (`opendata.fmi.fi`, producer STUK). Reads the stored query `stuk::observations::external-radiation::multipointcoverage` — a WFS 2.0 GML **multipoint coverage**: `gml:Point` members give each station's name + `gml:pos` ("lat lon"), `gmlcov:positions` lists "lat lon epoch" per measurement, `gml:doubleOrNilReasonTupleList` the dose-rate value (µSv/h, NaN-aware), index-aligned. Extends the **radiation / nuclear-monitoring modality** (opened by `odlinfo`, Germany) to a **NATO frontline state with the EU's longest Russia border + two operating NPPs (Loviisa, Olkiluoto)** — a first-order WWIII-risk geography. **Signal-meaningful via the universal baseline** (same argument as `odlinfo`): Finnish natural background is 0.05–0.30 µSv/h (STUK), so an elevation is interpretable without a per-station table. Plots **only stations elevated above background** (`value` ≥ 0.3 µSv/h, at the top of Finnish background, one notch below STUK's own 0.4 µSv/h automatic-network **alarm level**); per station the **newest** in-window reading wins (a station that was elevated but is now normal correctly drops), background/NaN readings drop, so an all-normal network — healthy peacetime — is 0 events, not an error. **Identical severity ladder + chip to `odlinfo`** (above-normal 0.4 → elevated 0.5 → high 0.7 → very high 0.9 → extreme 1.0; chip "0.62 µSv/h · Elevated"). `EventKind::Other` ("Other Signals" layer; a first-class `Radiation` EventKind is the self-improvement routine's lane). **Path A** (prod fetches the live WFS; the host 403s web fetch in-sandbox so endpoint + stored-query id + wire schema + **auth model** are anchored to committed GitHub bytes — STUK's own official client `StukFi/opendata` `wfs_scripts/{fmi_utils,process_data}.py`, off `raw.githubusercontent.com`: the exact URL, a **plain keyless `urlopen`** (auth-free), and the gml:Point/gmlcov:positions/doubleOrNilReasonTupleList parse; unit + background + alarm level from STUK's public "Radiation today" docs). Auth-free open data, credit "STUK / Ilmatieteenlaitos (FMI)". |
| `teleray` | Other (Radiation) | IRSN / ASNR (France) | **ambient gamma dose rate** — France's national **Téléray** dose-rate alert network (~470 beacons across mainland France + the DROM-COM overseas territories, 10-min cadence), read through the **OGC API - Features** endpoint `api.teleray.asnr.fr/wfs/collections/measures/items` (`f=json&limit=2000&sortby=-time`) — a GeoJSON `FeatureCollection`, one `Point` feature per recent per-station reading carrying the ambient gamma **dose-equivalent rate in nSv/h** (`doseRateNet`, net of the probe's own `bruitdefond`; `doseRateRaw` the raw reading), station `irsnId`/`libelle`, ISO `measurementDate`, and a `measState`/`validation` flag. **Third national network in the radiation / nuclear-monitoring modality** (after `odlinfo` Germany + `stuk_radiation` Finland), extending it to **Europe's largest nuclear power — 56 operating reactors (~70% of French electricity) plus the La Hague reprocessing complex**: a first-order WWIII-risk observable (reactor release / strike on nuclear infrastructure / detonation / dispersal). Distinct authority (IRSN/ASNR) + geography (all of France + overseas), no overlap with the German/Finnish networks. **Signal-meaningful via the universal µSv/h baseline** (same call as `odlinfo`/`stuk_radiation`): French natural background ~0.06–0.12 µSv/h (60–120 nSv/h); the connector converts nSv/h→µSv/h and plots **only stations elevated above background** (≥ 0.3 µSv/h), **deduped per station keeping the newest reading** (a station back to background correctly drops even if an older reading in the batch was elevated), defective probes (`measState` defect tokens) dropped, so an all-normal network — healthy peacetime — is 0 events, not an error. **Identical severity ladder + chip to `odlinfo`/`stuk_radiation`** (above-normal 0.4 → extreme 1.0; chip "0.45 µSv/h · Above normal" / "3.10 µSv/h · High"). `EventKind::Other` ("Other Signals" layer; a first-class `Radiation` EventKind is the self-improvement routine's lane). **Path A** (prod fetches the live OGC API; the host 403s web fetch in-sandbox so endpoint + query params + **auth model** + field schema are anchored to committed GitHub bytes — the open-source `kalisio/k-teleray` client `jobfile.js`: the exact `…/measures/items?limit=2000&sortby=-time` request via a **keyless HTTP call** (auth-free), reading GeoJSON features whose properties carry `irsnId`/`measurementDate`/`doseRateRaw`/`doseRateNet`/`bruitdefond`/`validation`/`measState`/`libelle` and whose geometry is the station `Point`; unit/station-count/10-min cadence from IRSN/ASNR public Téléray docs). Auth-free open data, credit "IRSN / ASNR — Téléray". |
| `nsw_rfs` | Wildfire | NSW Rural Fire Service | **Australian major fire/emergency incidents with their official ALERT LEVEL** — the NSW RFS `majorIncidents.json` GeoJSON feed (a state-government emergency service's own current-incident product). The operational signal is each incident's public-warning tier — **Emergency Warning / Watch and Act / Advice / Not Applicable** — a defined call-to-action scale (not a raw number), carried in the feature `category` and inside the `description` HTML blob (`ALERT LEVEL: … <br />LOCATION: … <br />STATUS: … <br />TYPE: … <br />SIZE: … ha <br />…`). One [`EventKind::Wildfire`] event per incident at its **representative point** (RFS ships a Point, or for larger fires a `GeometryCollection` of a representative Point + the fire-extent polygons — the connector takes the representative Point, falling back to the polygon centroid via a recursive coordinate walk). **Non-duplicative:** the global thermal-hotspot wildfire feeds (FIRMS/CWFIS/EONET) detect *heat pixels* or catalogue *events* — none carry the **human-facing alert level a fire authority has declared** for people on the ground, and Australia was otherwise blank on the map. **Signal-meaningful** (alert level is a baseline scale, each tier a named public action). Severity = alert ladder (Emergency Warning 0.95 → Watch and Act 0.7 → Advice 0.45 → Not Applicable/other 0.25), so a bad-fire-day Emergency Warning dominates the severity-sorted cap; a Not-Applicable major incident still plots (RFS pre-filters to *major* incidents) at the lowest severity. Chip = "Watch and Act · Bush Fire · 315512 ha" / "Advice · Bush Fire · 2 ha" / (Not Applicable) "Bush Fire · 10 ha". Empty feed (no current major incidents — the common quiet/off-season state) = 0 events, not an error. **Path A** (prod fetches the live `majorIncidents.json`; the host 403s web fetch in-sandbox, as every non-GitHub host does this session, so the exact raw wire schema — the `title`/`category`/`guid`/`pubDate`/`description`-HTML keys, the `%d/%m/%Y %I:%M:%S %p` pubDate format, and the Point-or-GeometryCollection geometry incl. the real "Badja Forest Rd, Countegany" Advice bush fire — is anchored to committed GitHub bytes: `exxamalte/python-aio-geojson-nsw-rfs-incidents` `consts.py` (URL + `ATTR_`/`REGEXP_ATTR_` field definitions) + its real `tests/fixtures/incidents-1.json` capture). Auth-free open feed (Data.NSW), credit "NSW Rural Fire Service". Fills the **operational emergency-warning modality** + **Australia** geography. |
| `ea_flood` | Weather | Environment Agency (UK) | **active flood warnings for England** — the EA Real-Time flood-monitoring `id/floods` endpoint, one item per warning/alert **in force**, graded by the national **flood-warning level** (`severityLevel`): 1 Severe Flood Warning (danger to life) → 2 Flood Warning (act now) → 3 Flood Alert (be prepared) → 4 no-longer-in-force (stand down). Connector queries `?min-severity=3` and re-filters, so only the three active tiers plot and a no-warnings day = 0 events, not an error. **Signal-meaningful where a raw river level isn't** (resolves the ECCC-hydrometric "nonsense number" at its root, same argument as `nwps_flood`): `severityLevel` is a **baseline-relative public-action category** — the EA has already compared conditions to each area's own flood thresholds — so the dot means "Severe Flood Warning", not an incomparable "2.79 m". Extends the flood-with-baselines modality (opened by the **US-only** `nwps_flood`) to **England / the UK** — new geography (Europe was the biggest blank), no overlap. The `floods` item's `floodArea` sub-object carries only a link (`@id`) + `polygon` URL, **no inline coords**, so the connector **joins each warning by `floodAreaID`** against the `id/floodAreas` catalogue (`{ notation, fwdCode, lat, long, riverOrSea, label }`) for the point — the two-fetch join pattern of `usgs_volcano`. Severity = level ladder (1 → 1.0, 2 → 0.7, 3 → 0.4). Chip = "Severe Flood Warning · River Teme" / "Flood Alert · River Nene". `EventKind::Weather` (renders in the existing Weather layer). **Path A** (prod fetches the live `id/floods` + `id/floodAreas`; `environment.data.gov.uk` 403s web fetch in-sandbox — the standing egress wall — so endpoint URLs + JSON schema are anchored to committed GitHub bytes: the `alicebarbe/England-Flood-Warnings-and-Visualizations` `floodsystem` client — `datafetcher.py` (the `id/floods?min-severity={}` URL + per-area `floodArea['@id']` fetch, confirming the default floods item carries NO inline coords), `warningdata.py`/`warning.py` (item keys `floodAreaID`/`severityLevel`/`isTidal`/`message`/`description` + area keys `notation`/`lat`/`long`/`label`), corroborated by the EA reference docs for the level 1–4 meanings + the real `061FWF10Witney` Witney area). Auth-free, Open Government Licence v3 (credit "Environment Agency"). |
| `portwatch_chokepoints` | Vessel | IMF PortWatch | **maritime chokepoint transit disruption** — IMF PortWatch (IMF + Univ. of Oxford) estimates daily ship transits through the world's **28 strategic maritime chokepoints** (Strait of Hormuz, Taiwan Strait, Bab-el-Mandeb, Malacca, Suez/Panama, Cape of Good Hope, …) from satellite AIS on ~90k vessels. Lands in [`EventKind::Vessel`], **extending the previously Baltic-only Vessel layer (`digitraffic_ais`) to the Asian/Middle-East theaters** — a first-order WWIII-risk observable (a sustained transit collapse at Hormuz/Taiwan Strait = blockade/mining/closure/war disruption; a surge at an alternate corroborates a disruption elsewhere). **Signal-meaningful where a raw transit count isn't** (the ECCC-hydrometric trap): PortWatch ships only raw daily counts, so the connector **computes the meaning itself** — the endorsed level→anomaly route — per chokepoint: the mean of the most recent 7 days is the **current** rate, the **median** of the older days in the fetched window is that chokepoint's own **transit norm**, and it plots the **deviation**. Only chokepoints **abnormally low** (drop ≥25% — closure/blockade, the alarm, severity 0.5→1.0 as the drop deepens) or **abnormally high** (surge ≥40% — rerouting, severity 0.3–0.4) plot; a chokepoint flowing at its norm drops, so an all-normal world = 0 events, not an error (the `nwps_flood`/`odlinfo` drop-the-all-clear pattern). Every value carries direction + magnitude + baseline + raw units: chip = "Transit down 63% vs norm (15 vs 40/day)". Non-duplicative with `digitraffic_ais` (individual Baltic AIS positions vs. aggregate global chokepoint anomaly). **Path A** (prod fetches the live public ArcGIS hosted feature service `services9.arcgis.com/…/Daily_Chokepoints_Data/FeatureServer/0/query`, the 2000 most-recent daily rows `orderByFields=date DESC` — enough to derive each chokepoint's ~4–8-week norm in one fetch; the host 403s web fetch in-sandbox, so the endpoint + attribute schema (`date` epoch-ms / `portid` / `portname` / `n_total` + Point geometry) + **auth-free access** are anchored to committed GitHub bytes: the World Bank `alternative-data-for-crisis` chokepoints-monitor notebook (exact FeatureServer URL + fields + the 7-day-MA-vs-historical-trend disruption method) and `amanid/imf-portwatch-analytics` (same keyless ArcGIS access); the "% of the 1-year average" closure threshold matches `montanaflynn/ishormuzopenyet`). Confirmed live/current in 2026 (also consumed by IEA + straits.live). Auth-free, credit "IMF PortWatch". |
| `acled` | Conflict | ACLED | global armed conflict — **DORMANT as a live event feed**: Open access has NO event API (confirmed by ACLED 2026-06-14; the event API needs a paid license). The free *aggregated weekly* slice is now **LANDED as `acled_aggregated`** (Path-B snapshot, see above); this `acled` connector stays dormant for the day a paid event key is set. |

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
- **NTWC / NOAA tsunami (tsunami.gov CAP-TSU)** — usable + geocoded, but **now ruled out as
  DUPLICATIVE (re-evaluated 2026-07-08).** The original "US-NOAA-authoritative, not Canadian"
  caveat is moot — the map is fully global (US NWS/NHC/SPC/AWC/NWPS, DE/FI/FR radiation, JMA,
  GeoNet, etc.), so NOAA clears bar 1. But re-checked against the fan-out: the live `nws` feed
  fetches `api.weather.gov/alerts/active` and `parse_nws` ingests **every** alert by its `event`
  field with **no type filter** — so US **Tsunami Warning / Advisory / Watch** are ALREADY on the
  map (as `EventKind::Weather`). The tsunami.gov CAP files (`/events/xml/PAAQCAP.xml` NTWC,
  `PHEBCAP.xml` PTWC; epicenter inline as the `EventLatLon` param, magnitude as
  `EventPreliminaryMagnitude`) cover the same US areas → **fails bar 6 (non-duplicative)**; the
  ocean-wide *international* products (Japan/Chile/Pacific) are separate **text bulletins**, not
  in these geocoded CAP files, so the CAP path does NOT reach the Asian theater. **JMA tsunami**
  (`bosai/tsunami`) is issued per **coastal forecast region (66 region codes, no inline geometry)**
  → same geometry-anchoring wall as MeteoAlarm; blocked without an EMMA-style region table. Net: no
  clean, non-duplicative, geocoded tsunami-warning layer available — do not re-chase either path.
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
- **FEWS NET / IPC acute food insecurity** (`fdw.fews.net/api/ipcphasemap/?…&format=geojson`)
  — **STRONG deferral, blocked 2026-07-06 only on real-bytes schema anchoring.** Opens a
  genuinely new **humanitarian / instability early-warning** modality a WWIII-risk operator
  tracks (famine zones are first-order conflict amplifiers): the current-2026 map lists **Sudan
  IPC 5 Catastrophe / credible Famine, Gaza confirmed Famine, and Nigeria/Somalia/South Sudan/
  Yemen at IPC 4 Emergency**. Clears essentially the whole bar — **authoritative** (FEWS NET, the
  USAID→State famine-early-warning system; IPC-compatible 5-phase scale), **fresh** (verified
  back online + publishing: June-2026 Food Assistance Outlook Brief, Sudan outlook update Apr-2026;
  the "portal offline Jan-2025" note in `prio-data/FEWSNet_to_PG` is RETIRED — it reactivated in
  2025), **geocoded** (admin-area polygons → centroid), **machine-readable** (GeoJSON via the FDW
  REST API; `scenario` = CS/ML1/ML2), **non-duplicative** (no food-security/humanitarian layer
  exists), **signal-meaningful** (IPC Phase 1 Minimal → 5 Famine is a defined baseline scale, each
  level a named severity — not a nonsense number), **auth-free** (FDW docs: anonymous requests
  return public data; the AFI classifications are public). **The one unmet requirement:** anchor the
  exact `ipcphasemap` GeoJSON **feature-property keys + confirm inline polygon geometry** to real
  committed bytes. `fdw.fews.net` 403s every web fetch (the standard gov-host egress wall — not a
  browser-only WAF like NGA), and no committed GitHub sample/consumer quotes the *map* product's
  property schema: `prio-data/FEWSNet_to_PG` uses the **geometryless** `ipcphase.csv` + an external
  boundary merge (fields seen: `country_code`/`country`/`reporting_date`/`geometry`/`value`→`IPC_value`),
  and `nutriverse/ipctools` targets the **keyed** IPC (`ipcinfo.org`) API, not FDW. Docs consistently
  name the `ipcphase.geojson` datapoint properties (`id,scenario,start_date,end_date,collection_date,value`)
  but do NOT confirm whether the geometry-bearing `ipcphasemap` variant carries inline polygons or its
  exact keys — too many load-bearing unknowns to write an honest fixture. **Landable next run** if
  EITHER a committed real `ipcphasemap` GeoJSON sample surfaces on `raw.githubusercontent.com`, OR the
  live endpoint becomes web fetch-reachable (200), OR a consumer quoting the map product's exact property
  keys is found. (The IPC `ipcinfo.org` API is the keyed alternative → would ship DORMANT; IPC also
  publishes GeoJSON to auth-free **HDX** `data.humdata.org` — a possible Path-B mirror if a stable
  per-region file + schema can be pinned.)

---

## COVERAGE GAPS & HUNTING IDEAS — where to look next

Bias each run toward the least-covered axis below.

- **Vessel / AIS** — SEEDED 2026-06-14 with `digitraffic_ais` (Fintraffic, Baltic), and
  the **Asian/Middle-East chokepoint** axis of this gap CLOSED 2026-07-09 with
  `portwatch_chokepoints` (**IMF PortWatch daily chokepoint transit disruption**, Path A
  via the committed-GitHub-bytes technique) — the Vessel layer now carries **global
  chokepoint transit anomalies** (Hormuz / Taiwan Strait / Bab-el-Mandeb / Malacca /
  Suez, deviation vs each chokepoint's own recent norm), so a transit collapse at a
  flashpoint chokepoint lights up the map. Note this is an **aggregate transit-disruption**
  signal, NOT a maritime-security *incident* feed and NOT live per-vessel AIS outside the
  Baltic — those two sub-gaps remain (below).
  The 2026-07-04 `asam` adoption did NOT close the maritime-security gap — the NGA
  upstream is dead (see the DORMANT `asam` row: API removed server-side; partner mirror
  frozen 2024-06-25), so the **maritime-security incident** modality (piracy / boarding /
  hijacking / drone-missile attack over Red Sea/Hormuz/Gulf of Guinea/Singapore Strait/
  South China Sea) **remains open** (distinct from the chokepoint-transit signal now landed). Successor leads,
  best first: (a) **ONI Worldwide Threat to Shipping (WTS)** — the U.S. Office of Naval
  Intelligence weekly report that superseded ASAM operationally; check for a
  machine-readable form (the PDF/para text needs a geometry anchor — same failure mode
  as `broadcast-warn` unless a KML/shapefile/API exists); (b) **IMB Piracy Reporting
  Centre live map** (icc-ccs.org — licensing/scrape-ban must be verified first);
  (c) **ReCAAP ISC** (Asia piracy, official reports; check for JSON — probed 2026-07-06 but
  UNVERIFIED: `recaap.org` egress-policy-403 + web search outage that run, schema/auth not confirmed).
  **NGA `broadcast-warn` (NAVAREA) ruled out 2026-07-06 as a Path-A feed:** though the MSI stack is
  alive (unlike ASAM's removed product) it 200s only *with a WAF/browser session*, so a plain-client
  connector would WAF-403 in prod — same wall that keeps `asam` dormant; needs a session-free path or a
  non-WAF NAVAREA coordinator. The dormant
  `asam` connector's parser/severity ladder is reusable if any successor speaks a
  compatible schema. Also remaining: **live AIS traffic** outside the Baltic
  (positions, not incidents) — other
  authoritative auth-free regional AIS if one surfaces. Three AIS-traffic leads stay ruled out:
  **NOAA/USCG marinecadastre** (authoritative but data on Azure blob, GeoParquet bulk
  historical — not GitHub-raw, not live, no hand-parse); **Danish Maritime Authority**
  (live AIS is *paid*, DKK 1,800–5,600/yr; only historical 2006–2016 bulk CSV is free, on
  `web.ais.dk` not GitHub; the `dma-ais` GitHub org is Java *software* libraries, no data
  feed); and **Norwegian Coastal Administration / Kystverket** (checked 2026-07-08 — the
  real-time feed is **auth-free (NLOD, no registration)** BUT delivered as a **raw TCP socket**
  of IEC-62320 AIVDM/AIVDO sentences at `153.44.253.27:5631`, NOT an HTTP JSON/GeoJSON endpoint,
  so it does NOT fit the connector's `fetch_text` hand-parse model — a persistent-socket ingestor
  is out of lane; `ais-public.kystverket.no` is *historical* only, and `kystdatahuset.no/ws`
  is an analytics web-service, not a live-positions GeoJSON). None is a Path-A or Path-B fit.
  (The only clean HTTP AIS on the map remains `digitraffic_ais`, Fintraffic/Baltic — an
  equivalent auth-free HTTP AIS for an **Asian chokepoint** theater has not surfaced.)
- **Conflict** — SEEDED 2026-06-14 with `ucdp_ged` (Uppsala, live CSV) and EXTENDED
  2026-06-28 with `acled_aggregated` (**ACLED weekly Admin-1 intensity, Path-B snapshot**) —
  the ACLED aggregated-weekly product is now LANDED (the licensed event `acled` stays dormant).
  Two complementary conflict modalities now live: UCDP discrete georeferenced events + ACLED
  admin-region weekly intensity (events + fatalities). Remaining: (a) **refresh `acled_aggregated`
  to global** — the shipped seed is the real ACLED Middle-East aggregate; the documented local
  re-download job should pull all six regional files (Africa, Asia-Pacific, Europe & Central Asia,
  Latin America & Caribbean, Middle East, US & Canada) for worldwide coverage; (b) a higher-frequency
  auth-free conflict signal if one surfaces.
- **Storm / tropical cyclone** — SEEDED 2026-06-24 with `nhc` (NOAA NHC, Atlantic/E-Pacific)
  and EXTENDED 2026-06-25 with `jma_typhoon` (JMA RSMC Tokyo, W-Pacific/South China Sea, Path A).
  Gap now: **Indian Ocean + Southern Hemisphere** basins — IMD (`mausam.imd.gov.in`), BoM
  (`bom.gov.au`), Météo-France La Réunion, or Fiji RSMC, if an auth-free geocoded product
  exists. JTWC (`metoc.navy.mil`) publishes HTML/RSS only — no clean auth-free JSON/GeoJSON
  (confirmed 2026-06-25; the JSON wrappers found are all keyed third parties: Xweather/DTN).
  **BoM triaged 2026-07-07:** a JSON cyclone product exists (`api.weather.bom.gov.au`) with a committed
  client (`tonyallan/weather-au`), but **BoM open-data delivery is currently SUSPENDED for a platform
  upgrade** → not fresh; revisit when BoM resumes open data. Best remaining leads: IMD / Météo-France
  La Réunion (RSMC SW-Indian) / Fiji RSMC Nadi, if an auth-free geocoded product surfaces.
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
  W-Pacific via `jma_typhoon`, **SE-Asia / Indonesia** via `magma_volcano` (PVMBG volcanoes)
  and now `bmkg_quake` (**BMKG felt earthquakes + InaTEWS tsunami potential**, 2026-06-30) —
  Indonesia now carries both a volcano and a seismic/tsunami feed. **Japan / NW-Pacific** seismic
  intensity now via `jma_quake` (**JMA Shindo-graded earthquakes**, 2026-06-30) — the NW-Pacific
  flashpoint region (Japan / Korea / Russia Far East) now has a national felt-intensity feed.
  **New Zealand / the SW-Pacific** now carries a seismic feed too via `geonet_quake` (**GeoNet
  MMI-graded felt earthquakes**, 2026-07-03) — the felt-intensity seismic modality now spans a
  trio of national bodies + scales (Indonesia MMI / Japan Shindo / NZ MMI). **Australia SEEDED
  2026-07-07** with `nsw_rfs` (**NSW Rural Fire Service major incidents + official alert levels**) — the
  first Australian feed on the map (BoM open data is suspended for a platform upgrade; NSW RFS is the
  state emergency service's own auth-free GeoJSON, anchored to the exxamalte HA client's committed bytes).
  Remaining Australia hunts: other state fire/emergency services (**WA DFES, Qld, Tasmania, EMV Victoria**
  — same GeoRSS/GeoJSON family, committed HA fixtures exist), **Geoscience Australia** hazards, **BoM**
  (when it resumes open data). **Europe SEEDED 2026-07-08** with `ea_flood` (**UK Environment Agency
  flood warnings**, England) — the first England/UK feed on the map — but Europe remains sparse:
  **MeteoAlarm investigated 2026-06-30 — deferred (geometry-anchoring blocked).** Its
  auth-free public legacy feed (`feeds.meteoalarm.org/feeds/meteoalarm-legacy-rss-<country>`)
  carries only an EMMA region *code/name* + awareness level/type — NO inline geometry; the
  geometry-bearing GeoJSON product is the registration-gated `api.meteoalarm.org` (403 in
  web fetch, can't confirm read-auth). Mapping EMMA region codes → polygons needs a region
  geometry table not anchorable to `raw.githubusercontent.com` bytes this run (the EAWS/SLF
  failure mode) — landable when EITHER a committed EMMA-region GeoJSON surfaces on GitHub-raw OR
  the keyed read-API is confirmed open (then ship the inline-geometry GeoJSON, centroid it).
  Other hunts: Asia/Pacific (JMA quakes/tsunami, Australia BoM/GA), Latin America (Chile
  SERNAGEOMIN volcanoes), Africa.
- **Domains under-covered** — power-grid stress (other ISOs), rail/pipeline incidents
  (TSB Canada, NTSB), dam/reservoir, drought, lightning (if a geocoded near-real-time
  product exists), methane/industrial (GHGSat/Sentinel). **flood-WITH-baselines SEEDED
  2026-06-26** with `nwps_flood` (NOAA NWPS observed flood category, US/CONUS) and
  **EXTENDED 2026-07-08** with `ea_flood` (**UK Environment Agency flood warnings**, England,
  national `severityLevel` 1–3, Path A via the committed-GitHub-bytes technique) — the
  baseline-relative flood modality now spans the US **and** England (opening UK/Europe, the
  biggest blank). Gap now: flood-with-baselines **elsewhere** — a **Canadian** product that
  ships a category (ECCC hydrometric stays rejected for lacking one), **Scotland (SEPA)** /
  **Wales (NRW)** to complete Great Britain, or a broader **European** flood-category feed
  (Copernicus EFAS / GloFAS if an auth-free geocoded per-area category surfaces). **GB-completion
  triaged 2026-07-08 — both blocked this run:** **NRW (Wales)** does expose a "Live Flood Warnings
  & Alerts" **GeoJSON by severity** (same Severe/Warning/Alert scale as EA, 15-min cadence) but it
  lives on an **Azure API-Management portal (`api-portal.naturalresources.wales`) behind a
  subscription key** → would ship DORMANT (keyed, no live value). **SEPA (Scotland)** publishes on
  an **ArcGIS Hub (`opendata-scottishepa.hub.arcgis.com`)** but the hosted content is **static
  flood-*risk*/extent maps**, not a live flood-*warning* FeatureServer with current status — the
  live warnings run through Floodline, whose auth-free machine-readable status endpoint isn't
  exposed/confirmable (SEPA host 403s web fetch, no committed client to anchor). **Landable when
  EITHER** an auth-free SEPA live-warning ArcGIS FeatureServer (queryable `f=geojson`) is pinned,
  **OR** NRW confirms a keyless open-data tier, **OR** a committed client quoting either live-warning
  schema surfaces. **snow-avalanche
  hazard SEEDED 2026-06-27** with `avalanche_ca` (Avalanche Canada danger ratings, Canadian
  mountains, seasonal). Gap now: avalanche/mountain hazard outside Canada (e.g. US NWAC/CAIC,
  the European EAWS/`avalanche.report` if an auth-free geocoded product ships a danger level).
  **EAWS / SLF investigated 2026-06-28 — blocked, deferred:** authoritative + auth-free + a
  documented CAAML-v6 JSON/GeoJSON Open-Data product (the right shape), BUT not anchorable to
  real bytes in-sandbox this run — the canonical aggregator/model (`pyAvaCore`, `albina-caaml`)
  live on **GitLab** (egress-blocked), the live hosts (`avalanche.report`, `aws.slf.ch`) **403**
  in web fetch, and the GitHub geometry/example repos (`eaws/eaws-regions`, `caaml/caaml`,
  `simon04/eaws-bulletin-map`) **can't be enumerated** (GitHub MCP scoped to `raithe-industries/gcrm`;
  `github.com` 403s; `raw.githubusercontent.com` serves only known paths) and web search surfaced
  **no indexed committed sample/geometry path**. Plus June = **off-season** (NH services dormant →
  0 events even live). Landable when EITHER a search-indexed real CAAML-GeoJSON sample surfaces on
  `raw.githubusercontent.com` OR external-GitHub read/search scope is granted (then fetch the
  `eaws-regions` geometry + a winter bulletin); re-attempt in winter regardless.
- **Severe convective (ground-truth)** — SEEDED 2026-06-29 with `spc_storm_reports` (**NOAA SPC**
  confirmed tornado / large-hail / damaging-wind reports, US/CONUS) — the *occurrence* layer (touchdown/
  impact) distinct from NWS warnings (forecast), cyclone tracks, flooding, and aviation hazards. Gap now:
  (a) **outside the US** — a Canadian (ECCC/CIFFC storm reports) or European equivalent confirmed-event
  feed if an auth-free geocoded one exists; (b) **lightning** as a near-real-time geocoded product still
  open. SPC Path-A refresh is automatic (prod fetches `today_*.csv` live).
- **Aviation hazards** — SEEDED 2026-06-29 with `awc_sigmet` (**NOAA AWC international SIGMETs** —
  en-route convective / turbulence / icing / volcanic-ash / tropical-cyclone hazards per FIR,
  worldwide; pairs with the live `opensky` aircraft + `navcanada` NOTAM layers). Gap now: **CWAs**
  (Center Weather Advisories, the sub-SIGMET US product) and **G-AIRMETs** are also AWC GeoJSON
  products if a lower-severity aviation layer is wanted; the live SIGMET host 403s in-sandbox so a
  Path-B snapshot refresh re-downloads the `format=geojson` endpoint (prod fetches it live).
- **Radiation / nuclear monitoring** — SEEDED 2026-07-04 with `odlinfo` (**BfS Germany ambient
  gamma dose rate**, WFS opendata, Path A) — a genuinely new war-risk modality (reactor release /
  detonation / dispersal detection) over a NATO frontline state; plots only stations elevated above
  the universal µSv/h natural-background baseline, so it's dark in peacetime and lights up on a real
  rise. **EXTENDED 2026-07-05 with `stuk_radiation`** (**Finland STUK external dose rate via the FMI
  open-data WFS**, Path A) — the prior run's adoption condition ("verify a direct auth-free
  STUK/`ilmatieteenlaitos` feed" — the `Apitalks` wrapper was the disqualified path) is now met: the
  **FMI / Ilmatieteenlaitos open-data WFS** (`opendata.fmi.fi`, producer STUK) IS that direct feed, and
  STUK's own official `StukFi/opendata` client confirms the keyless request + GML schema. **EXTENDED
  2026-07-05 with `teleray`** (**France IRSN/ASNR Téléray ambient gamma dose rate**, OGC API - Features,
  Path A, anchored to the `kalisio/k-teleray` client). Radiation now spans **three** national networks
  (Germany BfS + Finland STUK + France IRSN/ASNR), adding **Europe's largest nuclear power** (56 reactors
  + La Hague reprocessing; ~470 beacons over France + overseas). Gaps now:
  (a) **radiation still OUTSIDE Germany + Finland + France** — leads triaged 2026-07-05: **Ireland EPA**
  `data.epa.ie/radmon/api/v1` is the WRONG product (ERIC **lab-sample activity** in REM format — `is_mda`/
  `Value`/`Uncertainty` in Bq — NOT a real-time gamma **dose-rate** network; the live dose-rate map "MapMon"
  has no documented open API) → deferred until a MapMon backend surfaces; **Sweden SSM** (`karttjanst.ssm.se/
  gammastationer`, 28-station Baltic/Russia-frontier net — HIGH value) and **Norway DSA** Radnett (33 stations,
  Kola frontier) both have an **opaque map backend + no committed consumer to anchor** → blocked, re-attempt
  if a backend JSON/committed client surfaces. **Norway RADNETT lead UPGRADED 2026-07-07:** RADNETT is
  **published on Geonorge** (Norway's national spatial-data infrastructure; metadata record
  `e379ef5e-8851-4305-b900-44a4587cf14c`), which typically serves auth-free OGC **WFS/GeoJSON** — so a real
  machine-readable per-station endpoint very likely exists (no longer "opaque backend"). Still unanchorable
  *this* run: `geonorge.no` 403s web fetch (the session egress wall — see the 2026-07-07 run-log entry) and no
  committed client quotes the RADNETT dose-rate feature schema, so the exact `GetFeature` property keys can't
  be pinned to real bytes yet. **Landable when EITHER the Geonorge WFS becomes web fetch-reachable (200) OR a
  committed consumer quoting the feature schema surfaces.** **Asian-theatre radiation triaged 2026-07-07**
  (would double-hit the radiation gap + Asian-theatre geography): **Japan NRA** (`radioactivity.nra.go.jp`
  RAMDAS, real-time monitoring-post dose rate — high value, Fukushima + Asian theatre), **South Korea KINS**
  (`iernet.kins.re.kr` IERNet, 171 sites, DPRK-frontier), and **Taiwan AEC/NuSC** (real-time net, Taiwan-Strait
  flashpoint — but data.gov.tw dataset 31542 is a *report*, not a real-time geocoded per-station API) — all
  real authoritative networks, **all blocked the same way: no committed GitHub client to anchor the schema +
  web fetch dead to every gov host** → deferred until a committed consumer or a web fetch-reachable endpoint
  surfaces. Netherlands **RIVM** WMS-only (no per-station µSv/h
  API); the EU JRC **EURDEP** aggregate is access-restricted (not auth-free). (b) **promote to a first-class
  `Radiation` EventKind + map layer** — currently rides `EventKind::Other` ("Other Signals", default-off);
  that promotion (ee_core enum + ee_view layer + colour/label) is the **self-improvement routine's lane**,
  not this one. Note: **US EPA RadNet is REJECTED for the map** — it publishes gamma **gross count rate**
  (detector-specific, no universal baseline) not µSv/h, so its raw value is a "nonsense number" (same
  failure mode as ECCC hydrometric); revisit only if a baseline-relative RadNet product surfaces.
- **Humanitarian / food-insecurity (instability)** — **NO layer yet; lead identified 2026-07-06.**
  Acute food insecurity (IPC Phase 4/5 zones: Sudan, Gaza, Yemen, Somalia, South Sudan, Sahel) is a
  recognized conflict early-warning indicator an operator tracks. Lead: **FEWS NET / IPC** (see the
  DEFERRED entry) — authoritative, fresh-2026, geocoded admin polygons, IPC 1–5 scale; blocked only
  on anchoring the `ipcphasemap` GeoJSON schema to real committed bytes. Adjacent leads if FEWS/IPC
  stays blocked: **FAO–WFP Hunger Hotspots** (PDF/report — geometry-anchoring risk), the **Global
  Report on Food Crises** (annual, too lagged), and **HDX**-mirrored IPC GeoJSON (Path-B snapshot).
- **Cyber surface** — `cisa_kev` + `cccs` exist but aren't surfaced; a non-map cyber panel
  would unlock them.

---

## Run log

Newest first. One short entry per run: date, what was evaluated, what was adopted/rejected/
deferred, and the green-proof. Append; never rewrite history.

- **2026-07-09 (Signal Hunter, later run)** — **HONEST NO-OP — every ranked mission gap
  re-evaluated; all blocked this run by a total live-host web fetch WAF wall, no committed
  real-bytes anchor available for any strong candidate, so any landing would be
  documentation-guesswork (forbidden).** Root cause (control-tested): web fetch reaches ONLY
  `raw.githubusercontent.com` (→ real file content); **every** live gov/OSINT host 403s —
  verified against `earthquake.usgs.gov`, `acleddata.com`, `data.humdata.org`, `tfr.faa.gov`,
  and the `fdw.fews.net/api/ipcphasemap` endpoint directly. web search works; raw-GitHub works;
  arbitrary live fetch does not → Path-A liveness verification impossible this run. Ranked-gap
  triage: (a) **Conflict-freshness / `acled_aggregated` refresh** — read the connector:
  `MAX_ROW_AGE_DAYS = 42` age-gates against *today*, and the snapshot's newest `WEEK` is
  `2026-03-07` (~124 d stale), so **every row already drops → the ACLED intensity layer
  self-empties honestly** (no misleading data, no in-lane fix needed). Refresh still not
  obtainable: `acleddata.com` + `data.humdata.org` both 403 web fetch (registration/ToS wall),
  no fresh 2026 weekly-aggregate mirror on GitHub-raw → stays the local re-download job. (b)
  **Military-posture / global airspace (top gap, navcanada = Canada-only)** — all NOTAM APIs
  are **keyed/commercial** (Cirium/Laminar Data, FAA SWIM); the one auth-free airspace product,
  **FAA TFRs**, is WAF-blocked (can't verify freshness), dominated by VIP/sporting/hazard
  low-WWIII-signal restrictions, and carries deeply-nested DMS geometry — clears the 6-point bar
  only technically, fails the "question an operator actually has" spirit → not a strong honest
  landing. (c) **Maritime-security incident (asam-successor gap)** — re-searched: IMB (icc-ccs.org)
  and ReCAAP publish **PDF reports only**; IMO GISIS needs registration → still no auth-free
  machine-readable geocoded feed. (d) **FEWS NET / IPC acute food-insecurity** (the STRONG deferred
  new modality — Sudan/Gaza famine, a first-order conflict amplifier) — endpoint re-confirmed
  (`fdw.fews.net/api/ipcphasemap/?country=..&scenario=CS&collection_date=..&format=geojson`) but
  the API **403s web fetch directly**, HDX 403s, and **no committed `ipcphasemap` GeoJSON sample
  exists on GitHub-raw** to anchor the feature-property keys / confirm inline polygons → unchanged
  from the 2026-07-06 block. (e) **Norway DSA RADNETT radiation** (would extend the nuclear-monitoring
  modality to the Kola/Barents frontier — Russia's Northern Fleet & strategic-submarine base) — no
  committed client/spec surfaced to anchor the Geonorge WFS schema; "blocked on anchoring" stands.
  (f) **GDELT geo** (higher-frequency conflict) — ruled out again on the **authority / no-scrapers**
  bar (auto-coded from global media, not a gov/national/scientific ground-truth source). No code
  touched; tree left clean. Next run with live-host web fetch restored: FEWS NET IPC (Path-A schema
  anchor) and the airspace gap are the priorities.

- **2026-07-09 (Signal Hunter)** — **ADOPTED `portwatch_chokepoints` (IMF PortWatch daily
  maritime chokepoint transit disruption) — closes the Asian/Middle-East chokepoint axis of the
  #1 (maritime) gap; extends the Baltic-only Vessel layer to Hormuz / Taiwan Strait / Bab-el-Mandeb /
  Malacca / Suez.** First triaged the two freshest gaps: (a) **`acled_aggregated` refresh** — snapshot
  newest WEEK still `2026-03-07` (~124 days stale, past the 42-day age gate → the ACLED intensity layer
  is honestly dark), but re-confirmed the refresh is **NOT web fetch-obtainable this run**: `acleddata.com`
  AND `data.humdata.org` (incl. the CKAN `package_show` API) both **403 web fetch** (registration/ToS wall),
  no fresh 2026 weekly-aggregate mirror on GitHub-raw, live ACLED event fetch permanently barred → stays a
  local re-download job (unchanged). (b) **Maritime-security incident** (ReCAAP/IMB) — still no auth-free
  machine-readable API. **Picked PortWatch** instead: IMF-authoritative, auth-free public ArcGIS hosted
  feature service, machine-readable JSON, geocoded (Point per chokepoint), fresh (weekly Tue, ~4-day lag;
  confirmed live 2026 — also consumed by IEA + straits.live), non-duplicative (no chokepoint/transit layer
  existed; `digitraffic_ais` is Baltic AIS *positions*). **Verification:** web fetch is egress-limited to
  `raw.githubusercontent.com` this run (control test: raw-github → real 404; every other host → 403), so
  the endpoint + attribute schema (`date` epoch-ms / `portid` / `portname` / `n_total` + Point geometry) +
  auth-free access were anchored to committed GitHub bytes — the **World Bank `alternative-data-for-crisis`
  chokepoints-monitor notebook** (exact FeatureServer URL + fields + the 7-day-MA-vs-historical-trend
  method) and `amanid/imf-portwatch-analytics` (same keyless ArcGIS access); the "% of 1-yr average"
  closure threshold matches `montanaflynn/ishormuzopenyet`. **Signal-meaningfulness** solved by the
  endorsed level→anomaly route: the connector computes a per-chokepoint deviation (current 7-day mean vs
  the median-of-older-days norm in the fetched window) and plots only significant drops (≥25%, the
  closure/blockade alarm) or surges (≥40%, rerouting) — an all-normal world = 0 events; chip carries
  direction+magnitude+baseline+units ("Transit down 63% vs norm (15 vs 40/day)"). Implemented the
  ee-sources way (`portwatch_chokepoints.rs`: struct impl `Source` + pure `parse_portwatch` + 6 offline
  fixture tests, incl. drop/surge/normal, all-normal-Ok-empty, low-volume-skip, missing-geometry-skip,
  error-on-bad-input; registered in `lib.rs`; wired `osint.rs` fetch/join/tuple/cap-row/`feed_detail`
  arm; `IMF PortWatch` SRC_LABEL + vessel empty-state hint in `dashboard.html`). **Green-proof:**
  `cargo build --release` clean; `cargo test` root **609 passed / 0 failed**, `cargo test -p ee-sources`
  **177 passed / 0 failed** (the 6 new). (Note: the vendored third-party `feed-rs` crate has 13
  pre-existing `parser::rss0`/`rss2` failures — identical 63/13 split on clean `origin/main` via
  `git stash`, unrelated to this diff and outside the feed-acquisition lane.) Live-verify of the actual
  ArcGIS bytes is the deploy's job (full network); the World Bank/amanid committed clients are the
  Path-A liveness anchor.

- **2026-07-08 (Signal Hunter, later run)** — **HONEST NO-OP — every ranked mission gap re-verified
  walled from the sandbox this run; ledger updated (no code change).** Worked the gap ranking top-down,
  each candidate genuinely evaluated (web search works; web fetch still 403s every non-GitHub host —
  `raw.githubusercontent.com` only): (1) **Maritime / Asian-theater vessel (top gap)** — ReCAAP has no
  machine-readable API (re-confirmed); **Kystverket (Norway)** real-time AIS is auth-free but a **raw TCP
  socket** (AIVDM sentences, `153.44.253.27:5631`), not an HTTP JSON feed → out of the `fetch_text`
  connector model (recorded in the Vessel gap); no auth-free HTTP AIS for a Hormuz/Taiwan-Strait/SCS
  chokepoint surfaced → **blocked**. (2) **Conflict-freshness / `acled_aggregated` refresh** (snapshot
  newest WEEK `2026-03-07`, ~123 days stale → the intensity layer is honestly dark): re-verified myself —
  `acleddata.com` + `data.humdata.org` both 403 web fetch, **no fresh 2026 ACLED weekly-aggregate mirror on
  GitHub-raw** (redistribution is registration/ToS-gated), and re-adding a live ACLED fetch is permanently
  barred → **not refreshable from here** (local re-download job only). (3) **Military-posture / global
  NOTAM airspace** — FAA SWIM keyed; openAIP is static airspace (not live danger-area *activation*); FAA
  TFRs are US-only/low-signal → **blocked**. (4) **Asian-theatre radiation** (Japan NRA RAMDAS / Korea KINS
  / Taiwan) — real authoritative networks but **no committed GitHub client quotes any dose-rate schema** to
  anchor, and only citizen-science aggregators (OpenRadiation/Safecast) surfaced (fail no-scrapers) →
  **blocked**. (5) **Food-insecurity (FEWS NET / IPC, strong deferral)** — re-attempted the HDX mirror path:
  the IPC API (`api.ipcinfo.org`) is **keyed**, HDX (`data.humdata.org/organization/ipc`) **403s web fetch**,
  and **no committed IPC/FEWS GeoJSON sample** surfaced on GitHub-raw → still blocked on real-bytes schema
  anchoring (stays a strong deferral). (6) **Tsunami warnings** — NEW ruling: NOAA `tsunami.gov` CAP-TSU is
  **duplicative** — the live `nws` feed already ingests US Tsunami Warning/Advisory/Watch (`parse_nws` takes
  every `api.weather.gov/alerts/active` event with no type filter); JMA `bosai/tsunami` is region-code (no
  inline geometry). Both recorded under the updated **NTWC/NOAA tsunami** rejection → do not re-chase.
  (7) **GB-flood completion (Scotland SEPA / Wales NRW)** — NRW live GeoJSON is **key-gated** (Azure portal
  → DORMANT); SEPA's ArcGIS Hub carries **static flood-risk maps**, not a live-warning FeatureServer →
  blocked (both recorded under the flood gap). No candidate cleared all six bars with an honest anchor, and
  the sole anchorable option (a 2nd Australian-state fire feed, same alert-level modality as `nsw_rfs`) closes
  **no** ranked gap — landing it would be low-value densification the mission deprioritizes ("one source that
  fills a mission gap beats three trivia layers"), especially hours after `ea_flood` landed. **Green-proof:
  N/A (ledger-only commit, no connector/`osint.rs` change — tree otherwise clean).** The maritime/Asian-AIS,
  military-posture/NOTAM, Asian-radiation, ACLED-refresh, food-insecurity, and GB-flood lanes remain open per
  the (now-sharpened) gap notes.
- **2026-07-08 (Signal Hunter)** — **ADOPTED `ea_flood` (UK Environment Agency active flood warnings,
  England), Path A** — the **first England/UK feed on the map**, extending the baseline-relative
  flood-category modality (opened by the US-only `nwps_flood`) to **England** and opening **Europe**
  geography (the biggest blank). **Egress re-probed first:** web fetch still 403s every non-GitHub host —
  USGS `significant_week.geojson`, `environment.data.gov.uk` (both `id/floods` and `id/floodAreas`) all
  403 — so Path-A *live* verification is impossible and the only honest landing is the
  **committed-GitHub-bytes anchoring** technique (how `odlinfo`/`stuk`/`teleray`/`jma_quake`/`nsw_rfs`
  all landed on this same wall). web search works. **Candidate ranking this run, worked top-down, each
  genuinely evaluated:** (1) **Conflict freshness / `acled_aggregated` refresh** (snapshot newest WEEK
  `2026-03-07`, ~123 days stale → layer honestly dark): web fetch 403s `acleddata.com` + `data.humdata.org`
  and no fresh 2026 ACLED weekly-aggregate mirror exists on GitHub-raw (redistribution registration-gated)
  → **not refreshable from here** (local job). (2) **Global NOTAM / military-posture airspace**: leads
  (FAA SWIM = keyed; openAIP = static airspace not live danger-area activation; NOTAM parsers parse text,
  no auth-free global geocoded feed) → **blocked**. (3) **Asian-theatre radiation** (Japan NRA RAMDAS): no
  committed GitHub client quotes its dose-rate schema; only citizen-science aggregators (OpenRadiation/
  Safecast) surfaced → **fail the no-scrapers bar, blocked**. (4) **SH / Indian-Ocean cyclone** (Météo-France
  La Réunion / IMD / Fiji RSMC): no committed geocoded-JSON client to anchor → **blocked** (BoM still
  suspended). (5) **UK EA flood** — cleared **all six bars** with a clean committed anchor, so it landed:
  **authoritative** (Environment Agency, England's flood body); **auth-free** (public flood-monitoring API,
  no key, OGL v3); **machine-readable** JSON; **geocoded** (two-fetch join `id/floods` → `id/floodAreas`
  lat/long, the `usgs_volcano` pattern — the default floods item carries only a `floodArea` link, confirmed
  from committed bytes); **fresh** (15-min cadence; empty = 0 events, not an error); **non-duplicative**
  (England geography + flood-category outside the US); **signal-meaningful** (`severityLevel` 1–3 is a defined
  public-action scale — Severe Flood Warning / Flood Warning / Flood Alert — not a raw number, resolving the
  ECCC-hydrometric trap the same way `nwps_flood` does). **Anchoring (GitHub-bytes technique):** the exact
  endpoint URLs + JSON keys — `id/floods?min-severity={}`, per-warning `floodArea['@id']` fetch (proving the
  default floods item has NO inline coords), item keys `floodAreaID`/`severityLevel`/`isTidal`/`message`/
  `description`, area keys `notation`/`fwdCode`/`lat`/`long`/`label`/`riverOrSea`, and the real `061FWF10Witney`
  Witney area — confirmed from committed bytes of the `alicebarbe/England-Flood-Warnings-and-Visualizations`
  `floodsystem` client (`datafetcher.py` + `warningdata.py`/`warning.py`), corroborated by the EA reference
  docs for the level 1–4 meanings. New `vendor/ee-sources/src/ea_flood.rs` (two `fetch_text` calls → pure
  `parse_ea_flood(floods, areas)` — hand-parsed JSON, no heavy dep — with a notation/fwdCode-keyed area map,
  the level ladder, a `floodAreaID`→`floodArea.@id`-tail code resolver, river merged into `raw` for the chip,
  and a missing-`items` error; 5 offline tests: real-shape join dropping inactive-tier + placeless warnings,
  quiet-day/level-4-only = Ok/empty, error-on-bad-input, severity ladder, code-fallback). Registered in
  `lib.rs`; wired `src/osint.rs` (`fetch_one("ea_flood", …, 14)` + tuple + count/cap row cap 300 + `feed_detail`
  arm + an osint chip test); `SRC_LABEL` `UK Environment Agency` in `dashboard.html`. `EventKind::Weather`
  (renders in the existing Weather layer). **`cargo build --release` green; full `cargo test` green (gcrm 604/0
  failed/5 ignored — incl. the new osint chip test; ee-sources 166 incl. ea_flood 5/5); clippy clean on
  ee-sources.** Next: flood-with-baselines elsewhere (Canada; Scotland SEPA / Wales NRW to complete GB;
  European EFAS/GloFAS); the Asian-theatre radiation, ACLED-refresh, maritime/military-posture, and global-NOTAM
  lanes remain open per the gap notes.
- **2026-07-07 (Signal Hunter, later run)** — **ADOPTED `nsw_rfs` (NSW Rural Fire Service Major Incidents
  + official alert levels), Path A** — the **first Australian feed on the map**, opening the operational
  **emergency-warning** modality (a fire authority's declared Emergency Warning / Watch and Act / Advice for
  people on the ground) that the global thermal-hotspot wildfire feeds (FIRMS/CWFIS/EONET, which detect heat
  pixels or catalogue events) don't carry. **Egress re-probed first, decisively:** web fetch still 403s every
  non-GitHub host — USGS `significant_week.geojson`, `api.weather.gov`, **and GDACS + ReliefWeb** (both normally
  WAF-free) all 403 — confirming a **blanket egress wall** at the fetch layer this session, not host-specific
  WAF; only `raw.githubusercontent.com` serves. web search works. So Path-A *live* verification is impossible;
  the only honest landing is the **committed-GitHub-bytes anchoring technique** (how `odlinfo`/`stuk`/`teleray`/
  `jma_quake` all landed). **Candidate ranking this run, worked top-down, each genuinely evaluated:**
  (1) **Conflict freshness / `acled_aggregated` refresh** (snapshot's newest WEEK `2026-03-07`, ~122 days stale →
  layer honestly dark): web fetch 403s `acleddata.com` + `data.humdata.org`, and a targeted search found **no
  fresh (2026) ACLED weekly-aggregate mirror on GitHub-raw** (redistribution registration-gated) — **not
  refreshable from here** (local re-download job). (2) **Asian-theatre radiation** (double-hit: radiation +
  Asia geography — Japan NRA RAMDAS / Korea KINS IERNet / Taiwan MOENV·NuSC): all are real authoritative
  networks but **no committed GitHub client quotes any of their dose-rate schemas**, and the only radiation
  clients that surfaced are **citizen-science aggregators** (OpenRadiation / Safecast — fail the no-scrapers
  bar) → unanchorable, **blocked**. (3) **Ukraine air-raid alerts** (a top-tier active-conflict/freshness
  signal): both `alerts.in.ua` and `api.ukrainealarm.com` are **API-key-gated** (→ would ship DORMANT, no live
  value) *and* need an external oblast-geometry table *and* are volunteer aggregators of official data
  (shaky on no-scrapers) — too many unmet bars for a clean landing, **passed**. (4) **SH / Indian-Ocean
  cyclone** (Météo-France La Réunion / IMD / Fiji RSMC): no committed geocoded-JSON client to anchor;
  **blocked** (BoM still suspended). (5) **NSW RFS** — cleared **all six bars** with a clean committed anchor,
  so it landed: **authoritative** (NSW state emergency service's own feed); **auth-free** (public Data.NSW
  `majorIncidents.json`, no key); **machine-readable** GeoJSON; **geocoded** (representative Point per incident,
  or the fire-extent `GeometryCollection` centroided); **fresh** (30-min cadence; empty feed = 0 events, not an
  error); **non-duplicative** (Australian geography + the human-facing alert-level modality no thermal feed
  carries); **signal-meaningful** (alert level is a defined public-action scale, not a raw number). **Anchoring
  (GitHub-bytes technique):** the exact raw wire schema — `title`/`category`/`guid`/`pubDate`/`description`-HTML
  keys, the `%d/%m/%Y %I:%M:%S %p` pubDate format, the `ALERT LEVEL: … <br />TYPE: … <br />SIZE: … ha <br />`
  description blob, and the Point-or-GeometryCollection geometry incl. the real "Badja Forest Rd, Countegany"
  Advice bush fire — confirmed from committed bytes: `exxamalte/python-aio-geojson-nsw-rfs-incidents` `consts.py`
  (URL + `ATTR_`/`REGEXP_ATTR_` definitions) + its real `tests/fixtures/incidents-1.json` capture (embedded
  verbatim as the connector's fixture). New `vendor/ee-sources/src/nsw_rfs.rs` (plain `fetch_text`; pure
  `parse_nsw_rfs` hand-parses the GeoJSON — no heavy dep, no regex crate — with a recursive representative-point/
  centroid resolver, a `<LABEL>: … <br` description-field extractor, the alert-level ladder, and a missing-
  `features` error; 5 offline tests: real-feed-shape (GeometryCollection→representative Point, Advice→0.45, chip),
  severity+chip ladder across all four real tiers, GeometryCollection centroid fallback, empty-feed = Ok/empty,
  error-on-bad-input). Registered in `lib.rs`; wired `src/osint.rs` (`fetch_one("nsw_rfs", …, 12)` + count/cap
  row cap 300 + `feed_detail` arm + an osint chip test); `SRC_LABEL` `NSW Rural Fire Service` in `dashboard.html`.
  `EventKind::Wildfire` (renders in the existing Wildfire layer). **`cargo build --release` green; full workspace
  `cargo test` green (gcrm 596 / 0 failed / 5 ignored; ee-sources 166 incl. nsw_rfs 5/5); clippy clean on the
  touched crates.** Next: sibling Australian state emergency feeds (WA DFES / Qld / Tasmania / EMV — same family,
  committed HA fixtures) to densify Australia; the Asian-theatre radiation, Ukraine-alert (needs auth-free +
  geometry), ACLED-refresh, and maritime/military-posture lanes remain open per the gap notes.
- **2026-07-07 (Signal Hunter)** — **HONEST NO-OP: verified web fetch egress degradation blocks every
  live-verification + snapshot-refresh path this run; one real lead advanced (Norway RADNETT → Geonorge).**
  **Tool-health probed first, decisively:** web fetch **403s every non-GitHub host — including USGS**
  (`earthquake.usgs.gov/.../significant_week.geojson`), a known auth-free public gov feed — so the standing
  "web fetch routes outside the sandbox and works" assumption does **not** hold this session; only
  `raw.githubusercontent.com` is reachable. web search works (prose summaries, not verbatim bytes/schema).
  Worked the ranked gaps top-down, each genuinely evaluated:
  (1) **Conflict freshness / `acled_aggregated` refresh** (flagged high-value; snapshot's newest `WEEK`
  `2026-03-07` = **122 days stale**, so the age gate keeps the ACLED contribution honestly **dark**). The
  mission greenlights a web fetch refresh, but web fetch **403s acleddata.com AND data.humdata.org (HDX)**, and
  a targeted search found **no GitHub mirror of a current (2026) ACLED weekly aggregate** on
  `raw.githubusercontent.com` (ACLED redistribution is registration-gated; the committed mirrors are stale).
  Transcribing exact event/fatality/centroid rows from web search prose would fabricate the bytes the honesty
  bar forbids → **not refreshed** (re-light needs the local re-download job, operator's lane).
  (2) **Radiation outside DE+FI+FR** (double-hit lead: Asian-theatre geography). Triaged **Japan NRA RAMDAS**,
  **South Korea KINS IERNet**, **Taiwan AEC/NuSC**, plus the Baltic/Kola-frontier **Sweden SSM** + **Norway
  DSA RADNETT**. **One genuine advance:** RADNETT is on **Geonorge** (national SDI, metadata record noted) →
  upgraded from "opaque backend" to a likely auth-free WFS/GeoJSON endpoint (see COVERAGE GAPS radiation (a)).
  But all remain unanchorable this run — **no committed GitHub client quotes any of their schemas** and web fetch
  can't reach the endpoints, so an honest offline fixture can't be written (guessing field names = fabrication).
  (3) **SH / Indian-Ocean tropical cyclone** (Storm gap) — **Australia BoM** has a JSON cyclone product
  (`api.weather.bom.gov.au`) + a committed client (`tonyallan/weather-au`), **but BoM open-data delivery is
  currently SUSPENDED for a platform upgrade** (per BoM notices) → not fresh, deferred until it resumes.
  (4) **Asian maritime / military-posture / global NOTAM airspace** — top gaps re-confirmed walled (WAF/keyed/
  no-machine-readable-feed), unchanged from prior runs; no new auth-free geocoded feed surfaced.
  **Root cause:** the same egress wall as the last several runs — web fetch dead to all gov hosts, so no live
  Path-A verification and no non-GitHub snapshot refresh is possible; the bar forbids faking a liveness proof.
  **No code touched; tree clean; ledger-only commit.** UCDP still carries the Conflict layer live; the ACLED
  contribution stays dark pending a local refresh. Re-attempt RADNETT-via-Geonorge + the ACLED refresh next
  run if web fetch egress recovers.
- **2026-07-06 (Signal Hunter, later run)** — **HONEST NO-OP: no source clears the real-bytes
  schema-anchoring rule this run; strong FEWS NET/IPC lead captured as DEFERRED.** web search + web fetch
  BOTH recovered from the prior run's outage (web search queries all returned; web fetch reaches
  `raw.githubusercontent.com`, 403s every non-GitHub gov host — the normal egress wall). Worked the
  ranked gaps top-down:
  (1) **Asian-theatre maritime / maritime-security** — re-probed the reopened gap. **ReCAAP ISC** now
  403s even via web fetch (WAF, not just in-sandbox) — its Re-VAMP dashboard has no documented public API;
  **commercial AIS** (AISHub/VesselFinder/aisstream/VesselAPI) all **keyed**; **UKMTO** (Gulf/Hormuz
  incidents+warnings) publishes **no machine-readable feed** (web views only); **Norway Kystverket** live
  AIS needs **registration + a TCP/IEC socket** (not auth-free JSON; BarentsWatch is keyed); **Taiwan
  ADIZ** data exists only as **third-party compilations** (PLATracker / Ben Lewis Google Sheet — fails the
  no-scrapers bar; Taiwan MND itself ships text/PDF daily bulletins, not geocoded). Maritime top-gap stays
  walled.
  (2) **Military-posture / airspace closures** — no new **auth-free geocoded NOTAM-class / danger-area**
  feed surfaced; NGA `broadcast-warn`/NAVAREA stays WAF-ruled-out (prior run), FAA NOTAM is keyed, NAV
  CANADA remains Canada-only.
  (3) **FEWS NET / IPC acute food insecurity** (secondary, tractable) — evaluated in depth and it clears
  **six of the seven checks**: authoritative (USAID→State famine-early-warning system), **fresh —
  confirmed BACK ONLINE + publishing 2026** (June-2026 Outlook Brief, Sudan update Apr-2026: IPC 5
  Catastrophe/Famine risk; Gaza confirmed Famine), geocoded (admin polygons), machine-readable (FDW
  GeoJSON), non-duplicative (new humanitarian/instability modality), signal-meaningful (IPC 1–5 defined
  scale), auth-free (public AFI data, anonymous FDW requests). **Not shipped — the one unmet requirement is
  the mission's hard one: anchor the exact `ipcphasemap` GeoJSON property keys + confirm inline geometry to
  real committed bytes.** `fdw.fews.net` 403s every web fetch (all endpoints/formats/countries — host-level
  egress), and no committed consumer quotes the *map* product's schema (`prio-data/FEWSNet_to_PG` uses the
  geometryless `ipcphase.csv`+boundary-merge; `nutriverse/ipctools` targets the keyed IPC API). Writing a
  fixture from guessed field names would fabricate the liveness proof the bar forbids, so it goes to
  **DEFERRED** (strong) with the precise unblock condition, not onto the map. Retired the stale "FEWS portal
  offline Jan-2025" concern (it reactivated). Added a "Humanitarian / food-insecurity" coverage gap.
  **No code touched; tree clean; ledger-only commit.** UCDP still carries the Conflict layer live (ACLED
  aggregate snapshot remains 121 days old → its contribution honestly dark; refresh is a local job).
- **2026-07-06 (Signal Hunter)** — **HONEST NO-OP: dual verification-tool outage; nothing shippable
  clears the six-point bar this run.** Ranked the mission gaps (military-posture observables >
  Asian-theatre maritime > global NOTAM-class airspace > conflict freshness) and worked the top
  candidates, each blocked by tooling, not by the source itself:
  (1) **NGA Broadcast Warnings / NAVAREA** (`msi.nga.mil/api/publications/broadcast-warn`) — the
  strongest lead (naval-exercise / missile-firing danger areas + live-fire closures worldwide, closes
  BOTH military-posture and Asian-maritime). **Ruled out as a Path-A feed:** the host sits behind the
  same NGA WAF that stranded `asam` (ledger-confirmed: `broadcast-warn` only 200s *with a valid WAF
  session*), so a plain-client fetch (all our connectors use one) would WAF-403 in prod exactly as it
  does here — same failure class as the dormant `asam`. Not adopted; revisit only if a browser-session-free
  MSI path or an authoritative non-WAF NAVAREA coordinator surfaces.
  (2) **ReCAAP ISC** (Asia piracy/sea-robbery, official intergovernmental body) — could not verify:
  `recaap.org` is egress-policy-403 from this sandbox and **web search was in a sustained 529 outage all
  run** (~20 min, ~18 attempts, every query), so its schema / auth-model / geometry could not be
  confirmed and no committed consumer could be located to anchor. **Left unevaluated (not rejected)** —
  re-attempt when web search recovers; the dormant `asam` parser + escalation ladder are reusable if the
  schema is compatible.
  (3) **ACLED `acled_aggregated` refresh** — the committed snapshot's newest `WEEK` is `2026-03-07`
  (121 days old today); the connector's `MAX_ROW_AGE_DAYS = 42` gate against *today* is doing its job, so
  the ACLED contribution to the Conflict layer is **currently dark (honestly empty), not painting stale
  March heat as current**. Re-lighting it needs a fresh ACLED aggregate, but that download is a documented
  **local** re-download job (acleddata.com / HDX are license-gated and egress-policy-403 from the cloud
  sandbox) — not doable from here this run.
  **Root cause of the no-op:** dual tool degradation — web search 529-overloaded the entire run (blocking
  candidate discovery + committed-consumer anchoring) and the session egress policy 403s every non-GitHub
  host via web fetch (only `raw.githubusercontent.com` + package registries reachable — confirmed via
  `$HTTPS_PROXY/__agentproxy/status` `noProxy` list). Neither is fakeable around, and the bar forbids
  fabricating a liveness check. No code touched; tree left clean; ledger-only commit. UCDP still carries
  the Conflict layer live. Retry the NAVAREA-successor + ReCAAP leads next run once web search is back.
- **2026-07-05 (local watch, evening)** — **RUSTSEC-2026-0194/0195 RESOLVED (operator-approved):**
  feed-rs 1.5.3 → **vendored 2.3.1** (`vendor/feed-rs`) with quick-xml **0.31 → 0.41** (the patched
  line). Two surgical vendor patches restore feed-rs 1.x text semantics: (1) quick-xml ≥0.36 emits
  entities as GeneralRef events that feed-rs silently dropped ("&lt;p&gt;x" → "px" — encoded markup
  destroyed in every RSS description); the vendored xml layer now resolves predefined + numeric
  entities to text. (2) item `description` uses plain text capture (handle_text), not
  handle_encoded's re-parse. Green-proof: 575/0 unit; **103/103 live RSS feeds parse** under the
  migrated parser (ignored liveness test, full network); cargo audit **0 vulnerabilities** (only
  the 3 known leave-as-is warnings) — the daily 00:47 audit alert retires. Upstream feed-rs is
  dormant (2024-12); revisit the vendor patch if it revives.
- **2026-07-05 (local watch, evening)** — **Analyst channels ADOPTED (operator sign-off): roster
  17→21**: perun-video, caspianreport-video, wardcarroll-video, anderspuck-video (all Tier2,
  weekly-cadence defense/geopolitics depth). **Labeled-pair collection STARTED** for the
  cross-modal corroboration threshold (roadmap candidate): each roster-video ingest logs
  ambiguous-band (0.25–0.55 trigram) video↔wire title pairs to `logs/video-pairs-<date>.jsonl`
  for operator labeling — data collection only, never feeds the model.
- **2026-07-05 (local watch, morning)** — **`teleray` LIVE → DORMANT: upstream TLS/auth broken**
  (hours after adoption; the ASAM/first-rebuild lesson applied). The adopted host
  `api.teleray.asnr.fr` serves a Kubernetes ingress FAKE certificate (subject "Acme Co /
  Kubernetes Ingress Controller Fake Certificate" — TLS-invalid from any verifying client);
  the SPA's real data path runs through a JWT-gated Kalisio gateway (`gatewayJwt` in the app
  bundle) — NOT auth-free. `teleray.asnr.fr` itself has a valid cert but 404s every probed
  API shape. Action: fan-out fetch removed (tombstone in osint.rs), connector/chip/tests kept;
  RETRY when ASNR fixes the public host (the agency is mid IRSN→ASNR migration — likely
  transient misconfiguration, unlike ASAM's dead product). Radiation modality stays covered:
  odlinfo (DE) + stuk_radiation (FI) both live.
- **2026-07-05 (local watch, morning)** — **VIDEO ROSTER 7→15** (operator-directed): Reuters, AP,
  France 24 EN, CNA, TRT World, WION (wire/state-broadcast; weekend/overnight coverage no longer
  clusters on one 24/7 channel) + Democracy Now! and Zeteo (operator-nominated independent
  analysis/interview outlets, Tier2). **LIVE-STREAM TRANSCRIPTION lands DORMANT**
  (`src/livestream.rs` + `Ingestor::livestream_loop`, `GCRM_LIVESTREAM_SOURCES=1` to enable):
  Al Jazeera EN + DW 24/7 streams → yt-dlp live-URL resolve → ffmpeg 120s window → CPU
  faster-whisper (int8 base, nice'd, GPU untouched) → relevance gate → ONE rolling article per
  stream (update-in-place). Proven end-to-end 2026-07-05 (60s of live AJ transcribed accurately;
  ~17s CPU per minute of audio). ANALYST-CHANNEL SHORTLIST (pending operator sign-off, weekly
  cadence Tier2/3 candidates): Perun (defense economics), CaspianReport (geopolitics), Ward
  Carroll (US naval), Anders Puck Nielsen (maritime/Russia) — each an editorial-trust decision.
  DEFERRED: Telegram/X war-channel video — highest raw signal, but needs a source-trust and
  verification framework first (unverified combat footage scored as evidence would violate
  pillar-1); revisit with a design, not ad-hoc adoption.
- **2026-07-05 (Signal Hunter, second run)** — **adopted `teleray` (France IRSN/ASNR Téléray ambient gamma
  dose rate via the OGC API - Features endpoint), Path A** — a **third national radiation network**, extending
  the radiation / nuclear-monitoring modality to **Europe's largest nuclear power: France (56 operating reactors
  ~70% of electricity + the La Hague reprocessing complex), ~470 beacons over mainland France + the DROM-COM**.
  **Candidate ranking this run (biased to the top mission gaps), all genuinely evaluated before landing:**
  (1) **military-posture observables (#1 gap) via NGA MSI `broadcast-warn`** (NAVAREA/HYDROLANT/HYDROPAC —
  naval exercises / live-fire / missile-test / GPS-interference / closure warnings): re-verified to the SOURCE
  level this run. A new lead — NGA MSI now offers a **GeoJSON** message export (SeaLagom confirms "structured
  geometry in GeoJSON") — suggested the prior "geometry-in-free-text" block might be resolved, so I chased an
  anchorable structured-geometry schema: the **official `ngageoint/mage-server` NGA-MSI plugin** (`src/nga-msi.ts`
  + `src/topics/`) implements **only `asam` + `modu`** topics — **no broadcast-warn transformer exists** — and no
  other committed consumer parses broadcast-warn geometry (its geometry is by NAVAREA subregion polygon join, coarse
  ocean centroids). So broadcast-warn stays **unanchorable this run** (can't build an honest offline fixture without
  real structured-geometry bytes) — the prior non-binding ruling holds. **BLOCKED.** (2) **Asian-theater maritime
  (Taiwan/Hormuz):** ReCAAP (no JSON incident feed), IMB PRC (license), no auth-free national AIS outside the Baltic
  — unchanged, **BLOCKED.** (3) **global NOTAM:** FAA/ICAO keyed — **BLOCKED.** (4) **conflict freshness / the
  `acled_aggregated` refresh** (snapshot's newest WEEK 2026-03-07 now ~17 weeks stale → aged out, layer DARK): NOT
  taken — a faithful refresh needs the real ~100-row aggregate as **verbatim bytes** and the ACLED download stays
  registration-gated while web fetch summarizes CSVs; fabricating rows breaks the honesty bar (same binding judgment
  as the four prior runs). Open lane for a run with real byte access. (5) **radiation OUTSIDE Germany+Finland**
  (the explicit `odlinfo`/`stuk` follow-up gap): triaged **Ireland EPA** (documented `data.epa.ie/radmon` API turned
  out to be ERIC **lab-sample activity**, not the live dose-rate network — wrong product; MapMon has no open API),
  **Sweden SSM** (Baltic-frontier, high value, but opaque map backend + no committed anchor), **Norway DSA** (Kola
  frontier, same opacity) — and pivoted to **France IRSN/ASNR Téléray**, which cleared all six bars with a clean
  committed anchor. **Network re-probed fresh:** the egress block is unchanged — the live OGC API `api.teleray.asnr.fr`,
  `msi.nga.mil`, `data.epa.ie`, `karttjanst.ssm.se`, unpkg/jsdelivr/api.github.com and every gov host **403 via
  web fetch**; only `raw.githubusercontent.com` serves. **Anchoring (the GitHub-bytes technique, same as `odlinfo`/
  `stuk`):** endpoint + query params + **auth model** + field schema confirmed from committed bytes — the open-source
  `kalisio/k-teleray` client (`README.md` + `jobfile.js`, off GitHub-raw): request
  `…/wfs/collections/measures/items?limit=2000&sortby=-time` via a **keyless HTTP call** (no key/header → auth-free),
  GeoJSON features whose `properties` carry `irsnId`/`measurementDate`/`doseRateRaw`/`doseRateNet`/`bruitdefond`/
  `validation`/`measState`/`libelle` and whose geometry is the station `Point` (k-teleray derives its station catalogue
  by de-duping these features on `irsnId` → confirms measure features carry geometry, so a **single fetch, no join**).
  Unit (**nSv/h**), ~470 stations and the 10-min cadence from IRSN/ASNR public Téléray docs. Clears all six bars:
  **authoritative** (IRSN/ASNR = France's Radiation Protection & Nuclear Safety authority); **auth-free** (keyless,
  per k-teleray); **machine-readable** GeoJSON (OGC API - Features); **geocoded** (inline Point per station — no join);
  **fresh** (10-min cadence; all-normal network = 0 events, not an error); **non-duplicative** (France/nuclear-Europe
  radiation — distinct authority + geography from BfS/STUK). **Signal-meaningful (universal-baseline call as `odlinfo`/
  `stuk`):** converts nSv/h→µSv/h and plots ONLY stations ≥ 0.3 µSv/h (French background ~0.06–0.12), **deduped per
  station keeping the NEWEST reading** (a station back to background correctly drops even if an older reading in the
  batch was elevated — the load-bearing dedup-before-floor fix, caught by the fixture test), defective probes dropped;
  **identical severity ladder + chip to `odlinfo`/`stuk`** (above-normal 0.4 → extreme 1.0; chip "0.45 µSv/h · Above
  normal"). New `vendor/ee-sources/src/teleray.rs` (single OGC-API fetch; pure `parse_teleray` hand-parses the GeoJSON —
  no heavy dep — with a missing-`features` error, an ISO-`measurementDate` drop with a total-drift tripwire, net→raw
  dose fallback, and a best-effort defect-state guard; 7 offline tests: real-shape fixture keeps 3 elevated stations and
  drops normal + dedup-now-normal + defective + no-geometry, all-normal = Ok/empty, raw fallback, mixed/total
  timestamp-drift, error-on-bad-input, severity/band ladder, chip). Registered in `lib.rs`; wired `src/osint.rs`
  (`fetch_one("teleray", …, 12)` + count/cap row cap 400 + `feed_detail` arm + osint chip test); SRC_LABEL
  `IRSN · Téléray (France)` in `dashboard.html`. `EventKind::Other` ("Other Signals" layer; a first-class `Radiation`
  EventKind is the self-improvement routine's lane). **`cargo build --release` green; full workspace `cargo test` green
  (gcrm 571 / 0 failed / 4 ignored; ee-sources 161 incl. teleray 7/7; ee-correlate 79; ee-view 60; ee-core 9); clippy
  clean on the touched crates.** Next: radiation outside DE+FI+FR (Sweden SSM / Norway DSA if a backend JSON or committed
  client surfaces; Ireland MapMon backend); the military-posture (broadcast-warn geometry), Asian-maritime, global-NOTAM,
  and ACLED-refresh lanes remain open per the gap notes above.

- **2026-07-05 (Signal Hunter)** — **adopted `stuk_radiation` (STUK Finland external radiation dose rate
  via the FMI open-data WFS), Path A** — extends the radiation / nuclear-monitoring modality to
  **Finland: a NATO frontline state with the EU's longest Russia border + two operating NPPs (Loviisa,
  Olkiluoto)** — arguably the single most nuclear-relevant NATO frontier for WWIII risk.
  **Candidate ranking this run (biased to the top mission gaps), all genuinely evaluated:** (1)
  **Asian-theater maritime security (Taiwan Strait / Hormuz — the #1 gap after `asam` died):** ReCAAP ISC
  (web search confirms **reports + Re-VAMP dashboard only, no JSON/machine-readable incident feed**), IMB
  PRC (licensing/scrape-ban), no auth-free national AIS outside the Baltic (all hits — AISHub/VesselFinder/
  VesselAPI — are keyed or require registration) — all still blocked. (2) **Taiwan military-posture:** MND
  publishes daily PLA incursion counts but **no official geocoded JSON API** (only PDFs + third-party
  trackers ChinaPower/PLATracker); geometry-anchoring failure mode — blocked. (3) **conflict freshness /
  the `acled_aggregated` refresh (the layer is DARK — snapshot's newest WEEK 2026-03-07 is ~17 weeks stale,
  so the 42-day age gate empties it):** NOT taken — a faithful refresh needs the real ~100-row regional
  aggregate as verbatim recent bytes, and web fetch **summarizes** (won't return exact CSV); the ACLED
  aggregated download stays registration-gated. Fabricating counts would break the honesty bar (same
  binding judgment as the two prior runs). Remains an open lane for a run with real byte access. (4)
  **radiation OUTSIDE Germany (explicit `odlinfo` follow-up gap):** **Finland STUK** cleared all six bars —
  landed. **Network re-probed fresh:** the egress block is unchanged — the live WFS
  `opendata.fmi.fi/wfs`, `stuk.fi`, and every gov host **403 via web fetch**; only `raw.githubusercontent.com`
  serves (curl in-sandbox). **Anchoring (the GitHub-bytes technique, same as `odlinfo`):** endpoint +
  stored-query id + wire schema + **auth model** confirmed from committed bytes — STUK's own official
  open-data client `StukFi/opendata` (`wfs_scripts/fmi_utils.py` + `process_data.py`, fetched off
  GitHub-raw): request URL
  `https://opendata.fmi.fi/wfs/eng?request=GetFeature&storedquery_id=stuk::observations::external-radiation::multipointcoverage&starttime=…&endtime=…`,
  a **plain keyless `urlopen`** (no key/header → auth-free), and the exact GML parse — `gml:Point`
  members (name + `gml:pos` "lat lon"), `gmlcov:positions` ("lat lon epoch" per measurement),
  `gml:doubleOrNilReasonTupleList` (dose-rate value, NaN-aware, index-aligned). **Unit + baseline
  confirmed** from STUK's public "Radiation today" docs: **µSv/h**, Finnish natural background
  **0.05–0.30 µSv/h**, STUK automatic-network **alarm level 0.4 µSv/h**, ~255 stations at 10-min cadence.
  Clears all six bars: **authoritative** (STUK = Finland's Radiation and Nuclear Safety Authority; served
  by FMI, the national met institute); **auth-free** (FMI open data, keyless — proven by STUK's own client);
  **machine-readable** GML multipoint coverage; **geocoded** (inline `gml:pos` per station — no external
  join); **fresh** (10-min cadence; all-normal network = 0 events, not an error); **non-duplicative**
  (Finland/Russia frontier radiation — distinct authority + geography from the German BfS network).
  **Signal-meaningful (same universal-baseline call as `odlinfo`):** µSv/h has a universal natural
  background, so the connector plots ONLY stations elevated above it (`value` ≥ 0.3, at the top of Finnish
  background, one notch below STUK's 0.4 alarm) and, per station, keeps the **newest** in-window reading
  (a station that was elevated but is now normal correctly drops); **identical severity ladder + chip to
  `odlinfo`** (above-normal 0.4 → extreme 1.0; chip "0.62 µSv/h · Elevated"). New
  `vendor/ee-sources/src/stuk_radiation.rs` (fetch builds a 1-hour window at call time; pure
  `parse_stuk_radiation` hand-parses the GML — no heavy XML dep — with an ExceptionReport/malformed error,
  a positions↔values misalignment error, and a no-parseable-timestamp drift tripwire; 6 offline tests:
  real-shape fixture keeps 2 elevated stations and drops normal + dedup-now-normal + NaN, all-normal =
  Ok/empty, exception+403-HTML error, misalignment error, severity/band ladder, chip). Registered in
  `lib.rs`; wired `src/osint.rs` (`fetch_one("stuk_radiation", …, 12)` + count/cap row cap 400 +
  `feed_detail` arm + osint chip test); SRC_LABEL `STUK · FMI (Finland)` in `dashboard.html`.
  `EventKind::Other` ("Other Signals" layer; a first-class `Radiation` EventKind is the self-improvement
  routine's lane). **`cargo build --release` green; full workspace `cargo test` green (gcrm 564 / 0 failed /
  4 ignored; ee-sources 154 incl. stuk_radiation 6/6; ee-correlate 79; ee-view 60; ee-core 9).** Next:
  radiation outside Germany+Finland (Ireland EPA `data.epa.ie` if a direct auth-free µSv/h product; Norway
  DSA / Sweden SSM / Netherlands RIVM); the Asian-maritime / Taiwan-posture / global-NOTAM / ACLED-refresh
  lanes remain open per the gap notes above.

- **2026-07-04 (Signal Hunter)** — **adopted `odlinfo` (BfS Germany ambient gamma dose rate), Path A** —
  a new authoritative geocoded layer opening a **radiation / nuclear-monitoring modality no feed carried**,
  a first-order WWIII-risk observable (reactor release / detonation / dispersal) over a NATO frontline state.
  **Candidate ranking this run (biased to the top mission gaps), all genuinely evaluated before landing:**
  (1) **maritime-security (Asian/Hormuz — the #1 REOPENED gap after `asam` died):** ONI WTS (PDF only, no
  machine-readable geometry), IMB Piracy Reporting Centre (licensing/scrape-ban), **ReCAAP ISC** (PDF +
  Re-VAMP dashboard, no JSON API), **UKMTO** (PDF advisory notes; the site 403s web fetch, no data feed),
  **BarentsWatch/Kystverket AIS** (Arctic/Barents — but OpenID-Connect-gated → would ship dormant) — all
  blocked. (2) **military-posture / global NOTAM:** NGA `broadcast-warn` (geometry-in-free-text, ruled out
  the prior run), **FAA NOTAM API** (GeoJSON/AIXM but credentials are email-request to NOTAMS@faa.gov → keyed
  + manual, dormant at best), NASA DIP NOTAM (account-gated) — blocked. (3) **conflict freshness / the
  `acled_aggregated` refresh:** the snapshot has aged out (latest WEEK 2026-03-07, ~17 weeks, plots 0) but a
  faithful refresh needs the real ~100-row regional aggregate as **verbatim bytes** — the ACLED aggregated
  download is registration-gated and web fetch *summarizes* (won't return exact CSV); fabricating values would
  break the honesty bar, so it stays an open lane for a run with real byte access (NOT taken here). (4)
  **radiation:** **US EPA RadNet REJECTED** (gamma gross **count rate**, detector-specific, no universal
  baseline → "nonsense number"). **Pivoted to `odlinfo`, which clears all six bars cleanly.** **Network
  re-probed fresh:** the egress block on non-GitHub hosts is unchanged — the live WFS
  `imis.bfs.de/ogc/opendata/ows`, `odlinfo.bfs.de/json`, `api.bund.dev`, and the NTRS/FAA doc pages ALL
  **403 via web fetch**; only `raw.githubusercontent.com` serves. **Anchoring (the GitHub-bytes technique):**
  endpoint + wire schema + **auth model** were confirmed from committed bytes — the authoritative
  `bundesAPI/strahlenschutz-api` `openapi.yaml` (fetched off GitHub-raw): server
  `https://www.imis.bfs.de/ogc/opendata/ows`, **no `security` scheme declared (auth-free open data)**, a
  GeoJSON `FeatureCollection` (`totalFeatures` ~1722) of ExtendedFeature — Point `[lon,lat]` + properties
  `value`/`unit "µSv/h"`/`id "DEZ…"`/`kenn`/`name`/`plz`/`start_measure`/`end_measure`/`validated`/
  `nuclide "Gamma-ODL-Brutto"`/`duration "1h"`/`site_status 1|2|3`/`site_status_text`, example `value 0.124`
  — corroborated by the `bundesAPI/deutschland` strahlenschutz model docs (`Station.mw` = "Aktueller Messwert
  in µSv/h"). Clears all six bars: **authoritative** (BfS = Germany's Federal Office for Radiation Protection);
  **auth-free** (WFS opendata, no security scheme); **machine-readable** GeoJSON; **geocoded** (inline Point per
  station — no join, the trap that defers MeteoAlarm/EAWS); **fresh** (`_1h_latest`, hourly, ~1,700 24/7
  stations; all-normal network = 0 events, not an error); **non-duplicative** (a radiation modality nothing else
  carries). **Signal-meaningful (the key call):** unlike a river gauge, a dose rate in µSv/h has a **universal**
  natural-background baseline (~0.05–0.20 everywhere), so the connector plots ONLY stations elevated above it
  (`value` ≥ 0.3 µSv/h, clearing normal + local geology) and drops non-operational stations; severity ladder
  above-normal 0.4 → elevated 0.5 → high 0.7 → very high 0.9 → extreme 1.0; chip "0.45 µSv/h · Above normal" /
  "3.10 µSv/h · High". New `vendor/ee-sources/src/odlinfo.rs` (single WFS-GeoJSON fetch via the shared client;
  pure `parse_odlinfo` + `dose_chip` + `severity_for_dose` + `dose_band`; drops below-floor/non-operational/
  no-geometry/no-value/no-id records; 6 offline tests: real-shape fixture keeps 3 elevated operational stations
  and drops a normal-background + a defective + a no-geometry record, all-normal-network-is-OK,
  error-on-bad-input incl. non-JSON 403 body, drops-no-geometry/value/id, severity+band ladder, chip). Registered
  in `lib.rs`; wired `src/osint.rs` (`fetch_one("odlinfo", …, 12)` + count/cap row cap 400 + `feed_detail` arm +
  osint chip test); SRC_LABEL `BfS · ODL Gamma Dose (Germany)` in `dashboard.html`. `EventKind::Other` ("Other
  Signals" layer, default-off) — a first-class `Radiation` EventKind + layer is the self-improvement routine's
  lane. **`cargo build --release` green; full workspace `cargo test` green (gcrm 544 / 0 failed / 3 ignored;
  ee-sources 144 incl. odlinfo 6/6; ee-correlate 79; ee-view 60; ee-core 9).** Next: radiation OUTSIDE Germany
  (Finland STUK if a STUK-direct auth-free feed exists; Ireland EPA; EURDEP is access-restricted); the
  maritime-security / global-NOTAM / ACLED-refresh lanes remain open per the gap notes above.

- **2026-07-04 (local watch, night)** — **VIDEO-NEWS TRANSCRIPT INGESTION lands, DORMANT (operator-directed).**
  New `src/video.rs` + `Ingestor::video_loop`: a curated YouTube channel watchlist (4 channels whose
  text outlets already hold Tier-1/2 roster slots: BNN Bloomberg, Sky News, DW News, Al Jazeera EN)
  is discovered via the auth-free channel Atom feeds, new uploads' auto-captions pulled with a local
  `yt-dlp` (subtitles only, never the video), VTT-flattened, and fed through the NORMAL article
  pipeline — same dedup/NLP/LLM-enricher/store/dashboard row (title → YouTube link, channel as
  source, upload time as timestamp; no dashboard changes needed). Rationale: analyst/broadcast video
  carries signal wire text lacks — proven same-day by a BNN transcript disputing "Hormuz reopened"
  headlines with satellite-checked "traffic has not normalized" claims, scoring ZERO domain-keyword
  hits (the enricher is the classifier for this register). **Dormant by default** (keyed-feed
  pattern): enable with `GCRM_VIDEO_SOURCES=1` in `secrets.env`; needs `yt-dlp` (installed at
  `~/.local/bin/yt-dlp` → `~/.local/share/gcrm-video/venv`). Local-only (cloud sandbox cannot reach
  YouTube). Green-proof: 5 offline fixture tests + `#[ignore]` live test (1,079-word transcript
  end-to-end in 3s); full suite green. Age gate 24h, 3 videos/channel/cycle, 15-min cadence,
  90s yt-dlp budget, no-caption uploads retried until captioned or aged out.
- **2026-07-04 (local watch, evening)** — **`awc_sigmet` duplicate-issuance collapse.** A live map
  duplicate audit (896 quakes across 7 seismic feeds: ZERO same-time cross-feed duplicates — the
  cross-feed dedup verified clean) surfaced exactly one duplicate class: the AWC international-SIGMET
  aggregate itself carried three issuances as byte-identical twin features (WIII/FAOR/SAME series,
  verified upstream), which plotted stacked twin dots with colliding ids. Fix: `parse_awc_sigmet`
  now collapses on the synthetic identity key (fir+series+hazard+from), first record wins; locked by
  `upstream_duplicate_issuance_collapses_to_one_event`. Green-proof: ee-sources 6/6, full suite green.

- **2026-07-04 (local watch, same day)** — **`asam` LIVE → DORMANT: upstream verified dead
  from the full-network side.** First live map rebuild after deploy reported `asam: HTTP 404`
  / 0 events. Local verification (no sandbox limits): `msi.nga.mil/api/publications/asam`
  returns an application-level 404 (`{"status":404,"error":"Not Found"}`) with a valid
  `akam_nga_msi` session while sibling `publications/broadcast-warn` returns 200 with real
  NAVAREA JSON on the same session — the product is removed, not WAF-blocked; NGA's own SPA
  still calls the dead path. Esri Living Atlas partner mirror (`ASAM_events_V1` FeatureServer)
  frozen at newest incident **2024-06-25**. Action: removed the `src/osint.rs` fan-out fetch
  (acled-style tombstone comment), kept connector/chip/tests + registry, corrected this
  ledger's LIVE row + Vessel gap (REOPENED, with ONI WTS / IMB PRC / ReCAAP successor leads).
  Green-proof: `cargo build --release` + full `cargo test` green locally before push.
  Lesson recorded: **schema anchoring proves shape, not liveness** — a Path-A source whose
  host can't be web fetch-verified needs a first-rebuild follow-up check before its gap is
  declared closed.
- **2026-07-04** — **adopted `asam` (NGA Anti-Shipping Activity Messages), Path A** — a new
  authoritative geocoded layer opening a **maritime-security incident** modality no feed carried,
  extending the Vessel layer **beyond the Baltic** (the ledger's named Vessel gap: "Asian-theater
  maritime coverage — Taiwan Strait / Hormuz — the Vessel layer is Baltic-only"). ASAM plots
  reported hostile acts against ships — piracy, armed robbery, boarding, hijacking, kidnapping,
  and drone/missile/USV attacks — worldwide, over exactly the theatres a war-risk operator watches
  (Red Sea / Bab-el-Mandeb, Gulf of Aden, **Strait of Hormuz**, Gulf of Guinea, Singapore/Malacca
  Straits, **South China Sea**). Directly on-mission: attacks on shipping (Houthi Red Sea strikes,
  Hormuz incidents) are first-order escalation indicators. **Candidate ranking this run (biased to
  the top gaps):** LED with the #1 gap **military-posture observables** via **NGA MSI broadcast
  warnings** (NAVAREA/HYDROLANT/HYDROPAC — carry naval exercises / live-fire / missile-test /
  closure warnings). **Ruled out THIS run (not a binding rejection):** the `broadcast-warn` product
  hits the geometry-anchoring failure mode — positions live in the free-text `text` field, no
  structured lat/lon in the JSON (confirmed via web search + the reference consumers); a free-text
  position parser is fragile/scraper-ish, so deferred. **Pivoted within the same gap cluster** to
  ASAM, NGA MSI's sibling product that DOES ship a clean decimal `latitude`/`longitude` per record —
  no geometry join (the trap that defers MeteoAlarm/EAWS). Also weighed the explicitly-blessed
  **`acled_aggregated` snapshot refresh** (it has aged out — latest WEEK 2026-03-07, ~17 weeks old,
  so the ACLED half plots 0): NOT taken because a faithful refresh needs the real ~100-row regional
  aggregate as verbatim bytes, and web fetch **summarizes** (can't return the exact CSV); fabricating
  values would violate the honesty bar, so the refresh stays an open lane for a run with real byte
  access, not a fabricated one. **Network re-probed fresh:** the egress block on non-GitHub hosts is
  unchanged — the live `msi.nga.mil/api/publications/asam` and `openepi.io`/`postman.com` doc pages
  all **403 via web fetch**; only `raw.githubusercontent.com` serves. **Anchoring (the GitHub-bytes
  technique):** the endpoint + wire schema were confirmed from committed bytes — the NGA reference
  consumers `ngageoint/anti-piracy-iOS-app` `AsamResource.swift` + `anti-piracy-android-app`
  (`AsamWebService`/`AsamBean`: base URL `…/api/publications/asam`, `sort`/`output`/`minOccurDate`/
  `maxOccurDate` params, top-level `json["asam"]` array, fields id/latitude/longitude/occurrenceDate/
  referenceNumber/geographicalSubregion/navArea/aggressor/victim/description), corroborated by the
  `hrbrmstr/asam` R package's documented 9 record columns (`reference,date,latitude,longitude,navArea,
  subreg,hostility,victim,description`) + a genuine sample row (`2019-73 | 2019-09-30 | 1.04 | 104. |
  XI | 71 | Five Armed robbers | Bulk Carrier | SINGAPORE STRAITS`). Clears all six bars:
  **authoritative** (NGA MSI, compiled daily); **auth-free** JSON (US-Gov public domain, the apps
  use no key); **machine-readable** (`{"asam":[…]}`); **geocoded** (inline decimal lat/lon per
  record); **fresh** (daily; last-year `minOccurDate` window; empty array = 0 events, not an error);
  **non-duplicative** (maritime-security incidents — piracy/attacks on shipping — over global waters;
  distinct from Fintraffic's Baltic AIS *positions* and from UCDP/ACLED *land* conflict).
  **Signal-meaningful:** ASAM carries no numeric severity, so severity is graded by the **escalation
  class** read from the aggressor (`hostility`) + narrative (`description`) — a real maritime-security
  ladder (armed attack 0.9 > boarding 0.65 > robbery 0.5 > attempted 0.3; an *attempted* act
  de-escalates its tier), and the chip surfaces class + vessel ("Boarding · Bulk Carrier" / "Armed
  attack · Chemical Tanker"), not a raw scalar. New `vendor/ee-sources/src/asam.rs` (single JSON fetch
  with a `minOccurDate=today−365d` window; pure `parse_asam` + `classify` act-ladder + `asam_chip` +
  number-or-string tolerant `num`/`text` helpers incl. the trailing-dot "104." form; drops
  out-of-range centroids; 4 offline tests: real-shape fixture keeps a Red-Sea UAV attack / Singapore
  boarding / attempted-approach and drops the bad-centroid record + grades the class ladder,
  empty-window-is-OK, error-on-bad-input incl. non-JSON 403 body, classify ladder incl. attempted
  de-escalation). Registered in `lib.rs`; wired `src/osint.rs` (`fetch_one("asam", …, 12)` +
  count/cap row cap 400 + `feed_detail` arm + osint chip test); SRC_LABEL `NGA ASAM · Anti-Shipping`
  in `dashboard.html`. **`cargo build --release` green; full workspace `cargo test` green (gcrm 537 /
  0 failed / 3 ignored; ee-sources 138 incl. asam 4/4; ee-correlate 79; ee-view 60; ee-core 9).**
  EventKind::Vessel (a dedicated MaritimeSecurity variant is the self-improvement routine's lane).
  Next Vessel target: live AIS *traffic* outside the Baltic if an auth-free regional feed surfaces;
  the NGA broadcast-warn military-posture layer becomes landable if structured geometry (or a
  committed positions parser anchor) surfaces; the `acled_aggregated` refresh remains open for a run
  with verbatim-byte access to the ACLED regional aggregate.

- **2026-07-03 (OPERATOR SESSION — not a hunter run; lane-relevant map changes recorded here)** —
  Robert-directed session touched this ledger's lane: **(1)** the permanently-403 live `acled` fetch
  is REMOVED from `src/osint.rs` — ACLED live access is license-gated for good (confirmed 2026-06-14);
  do NOT re-add a live connector. Path-B **`acled_aggregated` remains the only ACLED lane** and
  **(2)** now AGE-GATES its snapshot (`MAX_ROW_AGE_DAYS` ~6 weeks vs the real clock): an abandoned
  snapshot self-empties instead of painting March data as current — it currently plots **0 rows
  (aged out honestly; the layer hint explains)**. The snapshot needs a refresh job or the ACLED half
  of the Conflict layer stays empty — that refresh is an open hunter-lane candidate. **(3)** `eonet`
  drops the `seaLakeIce` (icebergs) category outright (489 junk dots, ~5% of the map payload) and
  keeps only the newest geometry per event id; `gdacs` keeps the newest feature per event id
  (multi-polygon 2-3× dups gone); +3 vendor lock tests, ee-sources 134/0. **(4)** NEW cross-feed
  earthquake dedup in `src/osint.rs` (`dedup_earthquakes`: 90s/0.3° match, ±180° lon wrap): feed-rank
  priority **national-intensity (`jma_quake`/`geonet_quake`/`bmkg_quake`/`eqcanada`) > `usgs` >
  `emsc` > `gdacs`** — ANY new quake source must be slotted into `quake_feed_rank` (osint.rs) or its
  events won't dedup against the existing catalogues; national intensity feeds keep the dot and get
  a merged chip ("M6.1 · Shindo 3") from the best global sibling.

- **2026-07-03** — **adopted `geonet_quake` (GeoNet / GNS Science NZ felt earthquakes, MMI), Path A** —
  a new authoritative geocoded layer extending the **felt-intensity seismic modality** to **New Zealand /
  the SW-Pacific**, a coverage gap the raw global quake catalogues carry only sparsely. Per the
  SUSTAINED-BLOCK DIRECTIVE I LED with a landable real source biased to a coverage gap (non-NA geography +
  the felt-intensity axis), not a no-op. **Why not "another quake feed":** USGS/EMSC/eqcanada are raw
  *detection* catalogues (every instrument-detected event, magnitude only); GeoNet's `quake?MMI=3`,
  **filtered to a computed felt MMI**, is a **human-impact** product — only quakes that actually shook
  people, graded on the Modified-Mercalli scale (`mmi` = calculated shaking at the closest locality). Same
  justification that landed `bmkg_quake` (Indonesia MMI) and `jma_quake` (Japan Shindo): a national
  felt-intensity feed, different authoritative body (GNS Science) + geography (NZ). GeoNet is the SAME
  `api.geonet.org.nz` host already proven live in prod by `geonet_volcano`, so this is a safe Path A.
  **Network re-probed fresh:** the egress block on non-GitHub hosts is unchanged — the live
  `api.geonet.org.nz/quake?MMI=4` endpoint **403s via web fetch**; only `raw.githubusercontent.com` serves.
  **Anchoring (the GitHub-bytes technique):** the `quake` GeoJSON schema was confirmed from committed bytes
  — the real captured GeoNet response in `exxamalte/python-aio-geojson-geonetnz-quakes`
  `tests/fixtures/quakes-1.json` (web fetched off `raw.githubusercontent.com`): a `FeatureCollection` of
  `Point` features with props `publicID/time/depth/magnitude/mmi/locality/quality`, INCLUDING a record that
  **omits `time`** (drove the "now"-fallback) — corroborated by GeoNet's official API-property docs
  (`publicID`, `time`, `depth`, `magnitude`, `mmi` = MMI at closest locality, `locality`, `quality` =
  best/preliminary/automatic/deleted). Clears all six bars: **authoritative** (GeoNet / GNS Science = NZ's
  official geological-hazard monitor); **auth-free** GeoJSON; **machine-readable**; **geocoded** (inline
  per-quake `Point` — no geometry join, the failure mode that deferred MeteoAlarm/EAWS); **fresh** (10-min
  cadence real-time list; empty `features` = 0 events, not an error); **non-duplicative** (felt-MMI
  human-impact over NZ/SW-Pacific; retracted `deleted` quakes + sub-felt MMI < 3 dropped so it doesn't
  re-plot the detection catalogues). **Signal-meaningful:** MMI is a defined ground-shaking scale (each
  level a named human effect) — a "Felt MMI 5" dot is real, unit-bearing signal, not a raw number; severity
  = MMI ladder aligned with `bmkg_quake` (3 → 0.3, 6 → 0.7, 8 → 0.95, 9+ → 1.0). New
  `vendor/ee-sources/src/geonet_quake.rs` (single GeoJSON fetch with the `vnd.geo+json;version=2` Accept
  header GeoNet expects; pure `parse_geonet_quake` + `quake_chip` + `severity_for_mmi` + `capitalize_first`;
  drops `deleted`/sub-felt/no-geometry/no-id records; RFC3339 `time` with "now" fallback; 6 offline tests:
  real-shape fixture keeps the felt MMI-7/MMI-5 quakes and drops the MMI-2 sub-felt + the `deleted` one,
  quiet-window-is-OK incl. a sub-felt-only window, error-on-bad-input, drops-no-geometry/no-id, MMI severity
  ladder incl. saturation, chip with + without magnitude). Registered in `lib.rs`; wired `src/osint.rs`
  (`fetch_one("geonet_quake", …, 10)` + count/cap row cap 60 + `feed_detail` arm + osint chip test);
  SRC_LABEL `GeoNet · GNS Science` in `dashboard.html` (CC BY 3.0 NZ credit). **`cargo build --release`
  green; full workspace `cargo test` green (gcrm 493 / 0 failed / 3 ignored; ee-sources 131 incl.
  geonet_quake 6/6; ee-correlate 79; ee-view 60; ee-core 9).** EventKind::Earthquake. Next: Europe/MeteoAlarm
  still geometry-blocked; the Indian-Ocean/SH cyclone basin remains open (no auth-free geocoded product
  surfaced); other Asia/Pacific (Australia BoM/GA), Latin America (Chile CSN/SERNAGEOMIN), Africa.

- **2026-06-30** (second run) — **adopted `jma_quake` (JMA Japan seismic-intensity earthquakes), Path A** —
  a new authoritative geocoded layer over **Japan / the NW-Pacific** (Japan / Korea / Russia Far East — a
  key non-North-America flashpoint theatre the seismic feeds left thin). Per the SUSTAINED-BLOCK DIRECTIVE I
  LED with a landable real source biased to a coverage gap (non-NA geography), not a no-op. **Why not "another
  quake feed":** USGS/EMSC/eqcanada are raw *detection* catalogues (every instrument-detected event, magnitude
  only); JMA `bosai/quake/data/list.json` **filtered to events carrying an observed `maxi` (JMA Shindo
  intensity)** is a **human-impact** product — only quakes that produced measurable ground shaking, graded on
  Japan's national 0–7 Shindo scale (`1,2,3,4,5-,5+,6-,6+,7`), a distinct national intensity scale from
  Indonesia's MMI (`bmkg_quake`). Same justification that landed BMKG, different geography + scale. **Candidate
  hunt this run (per the directive, biased to coverage gaps):** LED with the documented **Storm / cyclone gap —
  Indian Ocean + Southern Hemisphere basins** (BoM Australia / IMD / Météo-France La Réunion / WMO SWIC). Ruled
  out: BoM exposes **no clean auth-free JSON/GeoJSON** cyclone product (warnings are XML/FTP + KMZ track maps;
  `bom.gov.au/catalogue/data-feeds` 403s web fetch and search confirmed no JSON API), and the **WMO Severe
  Weather Information Centre** aggregates RSMC advisories as HTML/CAP with no confirmed auth-free GeoJSON
  feature service — both dry hunts (JTWC already ruled out HTML/RSS-only). Per the directive I did not burn the
  run on a dry hunt and pivoted to a landable gap-filler. **Network re-probed fresh:** the egress block on
  non-GitHub hosts is unchanged — `bom.gov.au`, `www.jma.go.jp/bosai/quake/data/list.json`, and a raw README
  ALL **403 via web fetch**; only `raw.githubusercontent.com` serves. **Anchoring (the GitHub-bytes technique
  that broke the 20-run stall, now NHC→…→BMKG):** the `list.json` schema was confirmed from committed bytes —
  the `nehemiaharchives/jma-quake-api` `src/main/kotlin/JmaQuakeData.kt` data class (web fetched off
  `raw.githubusercontent.com`): fields `cod` (ISO-6709 coord string), `mag`, `maxi` (Shindo), `anm`/`en_anm`
  (epicentre name), `ttl`/`en_ttl` (bulletin type), `eid` (event id), `at` (time) — and the same `bosai` host
  is already proven live in prod by `jma_typhoon`. Clears all six bars: **authoritative** (JMA = Japan's
  national met/seismo agency, WMO RSMC); **auth-free** JSON; **machine-readable** (top-level array);
  **geocoded** (inline ISO-6709 `cod` per quake — no geometry join, the failure mode that deferred
  MeteoAlarm/EAWS); **fresh** (real-time bulletin list; empty array = 0 events, not an error);
  **non-duplicative** (Shindo human-impact intensity over Japan/NW-Pacific; pure-detection + unfelt-hypocentre
  bulletins dropped so it doesn't just re-plot USGS/EMSC). **Signal-meaningful:** Shindo is a defined
  ground-shaking scale (each level a named effect) — a "Shindo 5+" dot is real, unit-bearing signal, not a raw
  number; severity = Shindo ladder (1 → 0.15, 5+ → 0.75, 7 → 1.0). New `vendor/ee-sources/src/jma_quake.rs`
  (single JSON fetch; pure `parse_jma` + `quake_chip` + `shindo_rank` (lower/upper split + 弱/強 forms) +
  `severity_for` + `parse_iso6709` signed-token coord parser; **dedup by `eid` keeping the loudest bulletin**;
  drops no-hypocentre + no-Shindo records; 6 offline tests: real-shape fixture dedups two E1 bulletins to the
  louder 5+ and drops the unfelt-hypocentre + no-hypocentre records, empty-array-is-OK, error-on-bad-input,
  dedup-keeps-highest-regardless-of-order, Shindo rank/severity ladder incl Japanese 弱/強, ISO-6709 parsing
  incl southern/western signs + no-depth + malformed). Registered in `lib.rs`; wired `src/osint.rs`
  (`fetch_one("jma_quake", …, 10)` + count/cap row cap 60 + `feed_detail` arm + osint chip test); SRC_LABEL
  `JMA · Japan` in `dashboard.html`. **`cargo build --release` green; full workspace `cargo test` green
  (gcrm 491 / 0 failed / 3 ignored; ee-sources 125 incl jma_quake 6/6; ee-correlate 79; ee-view 60;
  ee-core 9).** EventKind::Earthquake. Next: the Indian-Ocean/SH cyclone basin remains open (needs an
  auth-free geocoded product — none surfaced); Europe/MeteoAlarm still geometry-blocked; other Asia/Pacific
  (JMA tsunami warnings are area-coded not point so geometry-blocked; Australia BoM/GA), Latin America, Africa.
- **2026-06-30** — **adopted `bmkg_quake` (BMKG / InaTEWS Indonesia felt earthquakes), Path A** — a new
  authoritative geocoded layer opening a **felt-intensity + tsunami-potential seismic modality** the raw
  global quake catalogues (USGS/EMSC/eqcanada) don't carry, over **Indonesia / SE-Asia** (the world's most
  seismically and tsunami-exposed region). Per the SUSTAINED-BLOCK DIRECTIVE I LED with a landable real
  source, not a no-op. **Why not "another quake feed":** USGS/EMSC are raw *detection* catalogues (every
  instrument-detected event, magnitude only); BMKG `gempadirasakan.json` is a **human-impact** product — only
  quakes actually reported FELT, each graded by the **Modified-Mercalli intensity** (`Dirasakan`, e.g. "IV
  Denpasar, III Mataram") plus Indonesia's national **tsunami-potential** flag (`Potensi`), neither of which
  the catalogues carry. **Candidate hunt this run (per the directive, biased to coverage gaps):** LED with the
  biggest blank — **Europe / MeteoAlarm** (EUMETNET pan-European weather warnings). It clears the *quality*
  bars on paper (authoritative, auth-free public legacy RSS/ATOM, baseline-relative awareness levels) BUT hit
  the **EAWS/SLF geometry-anchoring failure mode**: the auth-free feed
  (`feeds.meteoalarm.org/feeds/meteoalarm-legacy-rss-poland`, confirmed via the `SQ9MDD/meteoalarm` source on
  GitHub-raw) carries only an EMMA region *code/name* + `awt`/`level` — **NO inline geometry**; the
  geometry-bearing GeoJSON is the registration-gated `api.meteoalarm.org` (403 in web fetch — can't distinguish
  egress-block from read-auth), and no committed EMMA-region polygon table is reachable on
  `raw.githubusercontent.com`. Mapping region codes → centroids would be guesswork → deferred (see Geography
  gap). **Pivoted to a clean inline-geocoded source** to avoid that trap: BMKG ships `Coordinates` ("lat,lon")
  per record — no geometry join. **Network re-probed fresh:** `api.meteoalarm.org`, `data.bmkg.go.id`
  (`gempadirasakan.json`), and `feeds.meteoalarm.org`-via-page all **403 via web fetch** (egress-wide non-GitHub
  block unchanged); only `raw.githubusercontent.com` serves. **The unlock (same GitHub-anchored-schema
  technique that landed NHC→…→SPC):** BMKG maintains an **official spec repo** `infoBMKG/data-gempabumi`
  (web fetched its README off GitHub-raw — confirms the three open products `autogempa`/`gempaterkini`/
  `gempadirasakan`, the `https://data.bmkg.go.id/DataMKG/TEWS/<file>` endpoints, auth-free, attribution
  "BMKG"), and the canonical schema (`Infogempa.gempa[]` with `Tanggal/Jam/DateTime`, `Coordinates` "lat,lon",
  `Magnitude`, `Kedalaman`, `Wilayah`, `Potensi`, `Dirasakan`) is corroborated by 5+ independent public copies
  (`SlavyanDesu/bmkg-wrapper`, `salambae/Python-data-terbuka-bmkg`, the `muhammadhanif` XML→GeoJSON converters,
  the `okibayu` dev.to dashboard). Clears all six bars: **authoritative** (BMKG = Indonesia's national met/geo
  agency, operator of InaTEWS); **auth-free** JSON; **machine-readable**; **geocoded** (inline per-quake
  `Coordinates`); **fresh** (real-time felt list; an empty quiet window = 0 events, not an error);
  **non-duplicative** (felt-MMI human-impact + national tsunami assessment, neither in any quake catalogue).
  **Signal-meaningful:** MMI is a defined ground-shaking scale (each level a named effect) — a "Felt MMI V"
  dot is real, unit-bearing signal, not a raw number; severity = MMI ladder (II 0.25 → VI 0.7 → IX+ 1.0) with
  a raw-magnitude fallback when `Dirasakan` is blank, floored by tsunami potential (Waspada 0.9 / Siaga 0.95 /
  Awas 1.0); chip = "Felt MMI IV · M4.8" / "Felt MMI VI · M6.2 · Tsunami Siaga". New
  `vendor/ee-sources/src/bmkg_quake.rs` (single JSON fetch; pure `parse_bmkg` + `felt_chip` + `max_mmi`
  Roman-numeral peak-intensity scanner + `roman_to_int`/`int_to_roman` + `tsunami_level` + `parse_coords`;
  `gempa` tolerated as array OR single object; 6 offline tests: real-shape fixture drops the blank-Coordinates
  record + grades MMI IV/VI and the Siaga tsunami floor + magnitude fallback, empty-list-is-OK,
  single-object-tolerated, error-on-bad-input, MMI/tsunami parsing incl. hyphenated range + mixed-case-region
  safety, severity ladder). Registered in `lib.rs`; wired `src/osint.rs` (`fetch_one("bmkg_quake", …, 10)` +
  count/cap row cap 60 + `feed_detail` arm + osint chip test); SRC_LABEL `BMKG · InaTEWS (Indonesia)` in
  `dashboard.html`. **`cargo build --release` green; full workspace `cargo test` green (gcrm 490 / 0 failed /
  3 ignored; ee-sources 119 incl. bmkg_quake 6/6; ee-correlate 79; ee-view 60; ee-core 9).**
  EventKind::Earthquake. Next: Europe/MeteoAlarm becomes landable if a committed EMMA-region GeoJSON surfaces
  on GitHub-raw or the read-API is confirmed open; other Asia/Pacific (JMA quakes/tsunami, Australia BoM/GA),
  Latin America, Africa feeds if an auth-free geocoded product surfaces.
- **2026-06-29** (second run) — **adopted `spc_storm_reports` (NOAA SPC confirmed severe-storm reports), Path A** —
  a new authoritative geocoded layer opening a **severe-convective ground-truth modality** no current feed
  carried: confirmed **tornado / large-hail / damaging-wind** reports — the *occurrence* (touchdown/impact),
  not a forecast. Distinct from NWS/ECCC *warnings* (what may happen), NHC/JMA cyclone *tracks*, NWPS river
  flooding, and the just-landed AWC en-route aviation hazards. Per the SUSTAINED-BLOCK DIRECTIVE I LED with a
  landable real source rather than a no-op; SPC is a genuinely-new auth-free **live** feed, so Path A applies
  (prod fetches it live), and per the directive I did not burn the run hunting once it surfaced. **Network
  re-probed fresh:** the egress/UA block on non-GitHub hosts is unchanged — `spc.noaa.gov`, the third-party
  daculaweather archive, `api.github.com`, and readthedocs ALL **403 via web fetch**; only
  `raw.githubusercontent.com` serves. **Anchoring (the technique that broke the 20-run stall):** the SPC daily
  report format was confirmed from **real consumer-code bytes on GitHub** — `garrettrayj/storm-reports`
  `src/downloader.py` (URL pattern `…/climo/reports/{YYMMDD}_rpts_{torn,hail,wind}.csv`, also the no-date
  `today_*.csv` alias) and `src/preprocessing.py` (the 8-field row regex: time, magnitude, location, county,
  state, lat, lon, comments; first line skipped as header) — corroborated independently by web search for the
  per-type headers (`Time,F_Scale,…` / `Time,Size,…` / `Time,Speed,…`) and the units (**hail in inches /
  hundredths-of-inch**, severe ≥1.0"; **wind gust**, severe ≥58 mph / significant ≥75). The `jcharrell`
  node parser was ruled out as an anchor (it parses NWS LSR text, a different product). Clears all six bars:
  **authoritative** (SPC = the US national severe-convective body); **auth-free** CSV; **machine-readable**
  (8-col CSV, comma-in-comments handled via `splitn(8,',')`); **geocoded** (per-report lat/lon Point);
  **fresh** (daily real-time `today_*.csv`; header-only quiet day = 0 events, not an error); **non-duplicative**
  (no feed carries confirmed severe-storm occurrences). **Signal-meaningful:** every plotted value is unit-
  bearing + baseline-relative — tornado (EF rating when assessed → EF0 0.6…EF5 1.0, unrated touchdown 0.85),
  hail diameter in inches (1.0 severe → 3"+ destructive; `Size` dual-decoded so `175`=1.75" hundredths and a
  decimal `1.75` both read 1.75"), wind gust mph (58→0.4 … 90+→0.95; unknown-speed damaging-wind 0.5); chip =
  "EF2 Tornado" / "2.75 in hail" / "70 mph wind" / "Damaging wind". New
  `vendor/ee-sources/src/spc_storm_reports.rs` (three-fetch `Source::fetch`; pure
  `parse_spc_reports(torn,hail,wind)` + `report_chip` + per-type severity ladders + `hail_inches` dual-decoder;
  header-tolerant section parser that treats header-only as empty and only errors when NO section is a real
  report CSV; 6 offline tests: all-three-sections parse incl comma-laden comment + EF2/hundredths-hail/
  unknown-wind, empty-day-is-OK, error-when-no-report-CSV + one-good-among-bad, hail both-encodings + drop-
  unsized, drop-bad-coords, severity ladders). Registered in `lib.rs`; wired `src/osint.rs`
  (`fetch_one("spc_storm_reports", …, 12)` + count/cap row cap 400 + `feed_detail` arm + osint chip test);
  SRC_LABEL `NOAA SPC Storm Reports` in `dashboard.html`. **`cargo build --release` green; full workspace
  `cargo test` green (gcrm 488 / 0 failed / 3 ignored; ee-sources 113 incl spc_storm_reports 6/6;
  ee-correlate 79; ee-view 60; ee-core 9).** EventKind::Weather (the hydromet/severe-weather convention
  NHC/JMA/NWPS/avalanche/SIGMET follow; a dedicated severe-convective variant is the self-improvement
  routine's lane). Next: a non-US confirmed-severe-event feed (ECCC/CIFFC or European) or lightning if an
  auth-free geocoded near-real-time product surfaces.

- **2026-06-29** — **adopted `awc_sigmet` (NOAA AWC international SIGMETs), Path B** — a new
  authoritative geocoded layer opening an **en-route aviation-hazard modality** no current feed
  carried (convective / turbulence / icing / volcanic-ash / tropical-cyclone warnings per Flight
  Information Region), with **global FIR geography**, pairing with the live aircraft (`opensky`) +
  NOTAM (`navcanada`) layers. Per the SUSTAINED-BLOCK DIRECTIVE I LED with a Path-B snapshot-anchored
  ingestion via the GitHub-captured-bytes technique, NOT a no-op block record. **Network re-probed
  fresh:** web fetch positive control on `raw.githubusercontent.com` correct (`facebook/react`
  `package.json` → `private:true`/no `name`); the live AWC endpoint
  `aviationweather.gov/api/data/isigmet?format=geojson` **403s via web fetch** (egress-wide non-GitHub
  block unchanged), and the glama OpenAPI mirror + AWC help host both 403 — exactly why this is Path B.
  **The unlock (same technique that landed NHC→JMA→GeoNet→USGS-volcano→MAGMA→NWPS→avalanche-CA):**
  confirmed via **web search** that the AWC Data API isigmet/airsigmet products are open/no-auth/
  JSON+GeoJSON (no key), then **anchored to genuine committed bytes** — a real captured AWC
  international-SIGMET payload in `thomasdubdub/sigmet-sectors/20200316.json` (web fetched off
  `raw.githubusercontent.com`, the one reachable channel): a GeoJSON `FeatureCollection`, one
  `Polygon` `Feature` per SIGMET with `properties.{icaoId,firId,firName,hazard,qualifier,geom,coords,
  base,top,validTimeFrom,validTimeTo,rawSigmet}` + GeoJSON `geometry`. Real records confirm the shape
  and the global reach: **SBRE RECIFE** (Brazil, EMBD TS, TOP FL430) and **LFRR BREST** (France, SEV
  TURB, FL170–330). Schema corroborated independently by the AWC documented ISIGMET-JSON example
  (NZ Auckland Oceanic) and a second consumer (`fvalka/sigmet-map`). Clears all six bars:
  **authoritative** (NOAA AWC = the US aviation-weather body + intl-SIGMET aggregator); **auth-free**
  GeoJSON; **machine-readable**; **geocoded** (hazard-polygon centroid; `coords`-string fallback);
  **fresh** (real-time issuances; empty collection = 0 events, not an error); **non-duplicative** (no
  feed carries en-route aviation hazards — ground warnings, cyclone tracks, and river flooding are all
  distinct). **Signal-meaningful:** every value is a named WMO aviation phenomenon + standardized
  intensity/coverage + flight-level band — no raw scalar; severity = max(hazard base [VA/TC 0.9 → IFR
  0.4], qualifier bump [SEV 0.85 / HVY 0.7]); chip = "Severe Turbulence · FL170–330" / "Embedded
  Thunderstorms · to FL430". New `vendor/ee-sources/src/awc_sigmet.rs` (pure `parse_awc_sigmet` +
  `sigmet_chip` + hazard/qualifier severity & labels + `polygon_centroid`/`coords_centroid`/
  `level_band` helpers; case-insensitive props; number-or-string tolerant; rejects `NaN` levels;
  synthesizes a stable id from firId+seriesId+hazard+validTimeFrom since the GeoJSON output carries no
  feature id; 5 offline tests: real-fixture parse drops the no-hazard + no-geometry features and grades
  EMBD-TS/SEV-TURB/VA, empty-collection-is-OK, coords-string fallback, error-on-bad-input, severity &
  band/qualifier ladder incl. NaN-band). Registered in `lib.rs`; wired `src/osint.rs`
  (`fetch_one("awc_sigmet", …, 10)` + count/cap row cap 200 + `feed_detail` arm + osint chip test);
  SRC_LABEL `NOAA AWC SIGMET` in `dashboard.html`. **`cargo build --release` green; full workspace
  `cargo test` green (gcrm 480 / 0 failed / 3 ignored; ee-sources 107 incl. awc_sigmet 5/5;
  ee-correlate 79; ee-view 60; ee-core 5).** EventKind::Weather (the hydromet/aviation-weather
  convention NHC/JMA/NWPS/avalanche follow; a dedicated Aviation-hazard variant is the
  self-improvement routine's lane). **Path-B refresh (documented):** a local job re-downloads the live
  `isigmet?format=geojson` endpoint and re-commits the snapshot — prod fetches it live directly. Next
  aviation target: AWC **CWAs** / **G-AIRMETs** (lower-severity products, same GeoJSON API) if a
  lower-severity layer is wanted.
- **2026-06-28** (second run) — **adopted `acled_aggregated` (ACLED weekly Admin-1 conflict intensity), Path B** —
  the ledger's and the SUSTAINED-BLOCK DIRECTIVE's explicitly-named top target (the "ACLED-aggregated weekly
  conflict snapshot — admin-centroid dots, fatalities→severity"). Per the directive I **led with a Path-B
  snapshot of a high-value deferred source**, not a block record. **The geocoding problem dissolved on contact
  with real bytes:** ACLED's free **Aggregated Data** product already ships a per-Admin-1 **centroid lat/lon**
  in every row (`CENTROID_LATITUDE/LONGITUDE`), so no external centroid table is needed — confirmed first from
  ACLED's own docs (via web search) and then **anchored to genuine committed bytes**: the canonical 13-column
  schema (`WEEK,REGION,COUNTRY,ADMIN1,EVENT_TYPE,SUB_EVENT_TYPE,EVENTS,FATALITIES,POPULATION_EXPOSURE,
  DISORDER_TYPE,ID,CENTROID_LATITUDE,CENTROID_LONGITUDE`) appears verbatim in **15+ independent public copies**
  (real data rows in `Equipe-003/Hackathon_iSHEEROXDatacamp/data/raw/acled_benin.csv`,
  `matteogrifone22/Data-Visualization-Project`, `Scala40/DV`, schema dictionaries in `SkyTruth/shared-datasets-1`,
  `Yitzchak-Holtzberg/scenario-risk-calculator`, etc.). **The shipped snapshot is real recent data:**
  `acled_aggregated_snapshot.csv` (230 rows, 10 weeks **2026-01-03 → 2026-03-07**, 14 Middle-East countries,
  104 admin1s incl. Iran/Israel/Palestine/Lebanon/Syria/Yemen/Iraq) built from the real ACLED weekly-aggregate
  values committed in `Grumpylenard/Iran_EF-TP4_2026_DS4E` (an Iran-theater study), reshaped into the canonical
  ACLED layout — genuine bytes, not documentation guesswork, on a **live war-risk theater**. **Network re-probed
  fresh:** `raw.githubusercontent.com` reachable (in-sandbox curl of the snapshot source succeeded); `acleddata.com`
  **403s even via web fetch** (its own bot-protection, separate from the egress block) — exactly why this is Path B,
  not Path A. **Non-duplicative** vs `ucdp_ged`: a different modality (admin-region weekly *intensity* surface, not
  discrete events), a different source (ACLED vs Uppsala), broader taxonomy (incl. demonstrations/strategic
  developments), weekly vs monthly cadence. Clears all six bars + signal-meaningfulness (event + fatality counts
  are inherently unit-bearing conflict measures; severity = log(fatalities), UCDP ladder; an active-but-no-deaths
  region floors at 0.12). Connector **windows** to the ~4 weeks ending at the file's latest `WEEK` and **aggregates
  per (country, Admin 1)**, so a multi-year refresh plots current heat not stacked history; dominant ACLED label
  (most events) names the driver; chip = "41 events · 66 fatalities · Air/drone strike". New
  `vendor/ee-sources/src/acled_aggregated.rs` (quote-aware CSV parser; pure `parse_acled_aggregated` +
  `intensity_chip` + window/aggregate; case-insensitive headers; `LATITUDE/LONGITUDE` fallback; 4 offline tests:
  windows-and-aggregates drops the out-of-window January row, zero-fatality floor + omitted-fatalities chip,
  errors-on-bad-input + header-only-is-empty, committed-snapshot parses + windowing held). Registered in `lib.rs`;
  wired `src/osint.rs` (`fetch_one("acled_aggregated", …, 9)` + count/cap row cap 500 + `feed_detail` arm + osint
  chip test); SRC_LABEL `ACLED Aggregated` in `dashboard.html`; `acled` row updated (the dormant live connector
  now points to this landed snapshot). **`cargo build --release` green; full workspace `cargo test` green (gcrm
  478 / 0 failed / 3 ignored; ee-sources 102 incl. acled_aggregated 4/4; ee-correlate 79; ee-view 60; ee-core 5).**
  EventKind::Conflict. **Path-B refresh (documented):** a local job re-downloads ACLED's six aggregated regional
  files and re-commits the CSV — extend the seed from Middle-East to **global** that way (the sandbox can't reach
  the origin). Next conflict target: a higher-frequency auth-free signal if one surfaces.
- **2026-06-28** — honest **NO-OP** after a wide hunt. Per the SUSTAINED-BLOCK DIRECTIVE I LED with a
  Path-B / new-geography target rather than a block record: the ledger's explicit **next avalanche gap —
  European EAWS / SLF** (extend the snow-avalanche domain from Canada to the Alps/Europe). It clears the
  *quality* bars on paper — authoritative (SLF = Swiss federal WSL/SLF; EAWS national services), auth-free,
  a documented **CAAML v6** JSON/GeoJSON Open-Data product, geocoded, baseline-relative danger scale — but
  it could **not be anchored to real bytes** in this environment, so building it would be fabrication, not
  verification, and is refused. **Network re-probed fresh:** web fetch positive control on
  `raw.githubusercontent.com` correct (`facebook/react` `package.json` → `private:true`/no `name`);
  `avalanche.report/more/open-data` **and** `aws.slf.ch/api/bulletin/caaml/en/geojson` **both 403**
  (egress-wide web fetch block on non-GitHub hosts unchanged — only `raw.githubusercontent.com` resolves).
  **Why no real-bytes anchor (the difference from the last 7 successful runs):** NHC→JMA→GeoNet→USGS-volcano→
  MAGMA→NWPS→avalanche-CA each had a **search-indexed committed real sample** on `raw.githubusercontent.com`
  to build the parser+fixture against. For EAWS/SLF there is none reachable: the canonical aggregator + model
  (`pyAvaCore`, `albina-caaml`) live on **GitLab** (egress-blocked, non-GitHub); the GitHub repos that DO hold
  the geometry + examples (`eaws/eaws-regions`, `caaml/caaml`, `simon04/eaws-bulletin-map`,
  `fridlmue/harbour-avarisk`) **cannot be enumerated** — the GitHub MCP is scoped to `raithe-industries/gcrm`
  (external browse/search denied), `github.com` tree pages 403, and `raw` only serves *known* paths, all of
  which 404'd on every educated guess (`master`/`main` READMEs, `public/micro-regions/AT-07…`, `caaml/caaml`
  examples); web search surfaced **no** concrete indexed sample/geometry file URL. The CAAML v6 field shape was
  confirmable from the standard (`dangerRatings[].mainValue` low→very_high, `elevation`, `validTimePeriod`),
  but the live-endpoint date-dir/GeoJSON wrapping AND the region polygon geometry could only be **guessed** —
  the exact SERNAGEOMIN failure mode the program forbids. **Compounding factor:** June is **off-season** in the
  NH, so every avalanche service is dormant — 0 events live even in prod — meaning no current real data exists
  anywhere to anchor against, only archived winter samples (none reachable). **Other candidates ruled out this
  run:** **Iceland IMO** (the named N-Atlantic volcano gap, live year-round, currently active) — no clean
  machine-readable aviation-colour product (the ACC is a daily-updated *image* map + third-party `apis.is`),
  fails bar 3; **INGV / PHIVOLCS / SERNAGEOMIN** unchanged (no auth-free geocoded alert API / only HTML/third-party).
  **DEFERRED list re-checked, none Path-B-viable:** `IESO` Ontario grid = a single province-wide MW scalar with
  no per-record lat/lon and no baseline → "nonsense number" (signal-meaningfulness fail); `CCCS` cyber = non-geo,
  belongs on a UI panel = the self-improvement routine's lane, not the map; `avalanche_ca` already LIVE. Non-Baltic
  **AIS** still has no authoritative auth-free source (NOAA historical-only, BarentsWatch keyed, Danish paid).
  **No code change**; `cargo build --release` green; full workspace `cargo test` green (gcrm 474 / 0 failed /
  3 ignored; ee-sources 98; ee-correlate 79; ee-view 60; ee-core 5). Tree left clean; ledger run-log only.
  **Did NOT send a push notification:** the egress/network block is unchanged and owner-side, already escalated
  many times — another identical alert is noise. **Next:** EAWS/SLF avalanche is landable the moment EITHER a
  search-indexed committed real CAAML-GeoJSON sample appears on `raw.githubusercontent.com`, OR external-GitHub
  read/search scope is granted (then fetch `eaws-regions` geometry + a real winter bulletin); re-attempt in the
  Nov→Apr season regardless.
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
