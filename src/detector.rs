// ------------------------------------------------------------
// RAiTHE INDUSTRIES INCORPORATED
// Copyright (c) 2026 All Rights Reserved.
//
// This file is part of a proprietary system. Unauthorized use,
// reproduction, or distribution is strictly prohibited.
// ------------------------------------------------------------

// src/detector.rs — Seismic Anomaly Detection System
//
// IMPORTANT — HONEST LABELLING:
//   This system detects seismic events with profiles consistent with
//   underground nuclear tests. It does NOT confirm nuclear detonations.
//   Only CTBTO, national seismological agencies, and governments can
//   make that determination. All alerts are labelled "SEISMIC ANOMALY"
//   until official confirmation is received.
//
// Architecture:
//   SeismicMonitor      — polls 5 parallel FDSN-standard APIs every 60s
//   AftershockChecker   — re-queries 2h after anomaly for sequence absence
//   CtbtoMonitor        — scrapes CTBTO public RSS for official statements
//   NuclearNewsMonitor  — watches article store for nuclear escalation spikes
//   SeismicAlertFusion  — combines signals into a single alert state
//
// Data latency note:
//   USGS and partner networks detect seismic events ~1-3 minutes after
//   occurrence. Data appears in the API ~2-5 minutes later. Polling more
//   frequently than every 30-60s provides no benefit and risks rate-limiting.
//   The dashboard displays this latency transparently.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio::time::{interval, sleep};
use tracing::{debug, info};

use crate::aggregator::AppState;

// ── Known nuclear test site registry ─────────────────────────────────────────
// Sources: SIPRI, NTI Nuclear Threat Initiative, CTBTO site survey data.
// Each entry includes a detection radius — tighter for isolated sites,
// wider for tectonically active regions where false positives are likely.

#[derive(Debug, Clone)]
#[allow(dead_code)] // last_test and active are metadata fields — displayed in operator panel Phase 3
pub struct TestSite {
    pub id:        &'static str,
    pub name:      &'static str,
    pub actor:     &'static str,
    pub lat:       f64,
    pub lon:       f64,
    pub radius_km: f64,
    pub last_test: &'static str,   // ISO date of last confirmed test
    pub active:    bool,           // Currently assessed as operational
}

pub const KNOWN_TEST_SITES: &[TestSite] = &[
    TestSite {
        id: "punggye_ri", name: "Punggye-ri Nuclear Test Site",
        actor: "north_korea", lat: 41.2833, lon: 129.0833, radius_km: 150.0,
        last_test: "2017-09-03", active: true,
        // All 6 DPRK tests 2006-2017. Tunnels assessed operational. DoD: 7th test
        // is "postured to conduct at a time of its choosing."
    },
    TestSite {
        id: "novaya_zemlya", name: "Novaya Zemlya Test Site",
        actor: "russia", lat: 73.4000, lon: 54.6000, radius_km: 300.0,
        last_test: "1990-10-24", active: true,
        // Russia's primary nuclear test site. Remote Arctic archipelago — very
        // low background seismicity. Any event here is highly anomalous.
    },
    TestSite {
        id: "lop_nur", name: "Lop Nur Test Site",
        actor: "china", lat: 41.7700, lon: 89.4000, radius_km: 200.0,
        last_test: "1996-07-29", active: false,
        // China declared moratorium 1996. Site is maintained but assessed inactive.
    },
    TestSite {
        id: "nevada_nts", name: "Nevada National Security Site",
        actor: "united_states", lat: 37.1200, lon: -116.0500, radius_km: 150.0,
        last_test: "1992-09-23", active: false,
        // US declared moratorium 1992. Sub-critical tests continue but no yield.
    },
    TestSite {
        id: "semipalatinsk", name: "Semipalatinsk Test Site",
        actor: "kazakhstan", lat: 50.0700, lon: 78.4300, radius_km: 200.0,
        last_test: "1989-10-19", active: false,
        // Soviet/Kazakh site. Closed 1991. No assessed operational capability.
    },
    TestSite {
        id: "pokhran", name: "Pokhran Test Range",
        actor: "india", lat: 27.0700, lon: 71.7700, radius_km: 150.0,
        last_test: "1998-05-13", active: true,
        // India has not declared a moratorium. Assessed capable of further tests.
    },
    TestSite {
        id: "chagai", name: "Chagai Test Site",
        actor: "pakistan", lat: 28.9000, lon: 64.9000, radius_km: 200.0,
        last_test: "1998-05-28", active: true,
        // Pakistan's primary site. Region has moderate natural seismicity —
        // wider radius needed but also more background noise.
    },
    TestSite {
        id: "ras_koh", name: "Ras Koh Hills",
        actor: "pakistan", lat: 28.0000, lon: 65.2000, radius_km: 150.0,
        last_test: "1998-05-30", active: true,
        // Pakistan secondary site. 2 of 6 May 1998 tests conducted here.
    },
    TestSite {
        id: "ekker_reggane", name: "Reggane / In Ekker Test Site",
        actor: "france", lat: 26.3200, lon: 0.1500, radius_km: 200.0,
        last_test: "1966-02-16", active: false,
        // French Algerian Sahara sites. Historically relevant, not operational.
    },
    TestSite {
        id: "mururoa", name: "Mururoa Atoll",
        actor: "france", lat: -21.8400, lon: -138.8100, radius_km: 100.0,
        last_test: "1996-01-27", active: false,
        // French Pacific test site. Closed after last underground test 1996.
    },
];

// ── Haversine distance calculation ────────────────────────────────────────────

pub fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    const R: f64 = 6371.0; // Earth radius in km
    let d_lat = (lat2 - lat1).to_radians();
    let d_lon = (lon2 - lon1).to_radians();
    let a = (d_lat / 2.0).sin().powi(2)
        + lat1.to_radians().cos()
        * lat2.to_radians().cos()
        * (d_lon / 2.0).sin().powi(2);
    2.0 * R * a.sqrt().atan2((1.0 - a).sqrt())
}

/// Returns the closest test site and distance if within any site's radius.
pub fn nearest_test_site(lat: f64, lon: f64) -> Option<(&'static TestSite, f64)> {
    KNOWN_TEST_SITES.iter()
        .map(|site| (site, haversine_km(lat, lon, site.lat, site.lon)))
        .filter(|(site, dist)| *dist <= site.radius_km)
        // unwrap_or(Equal), not unwrap(): a NaN distance would panic the bare unwrap. Matches
        // the codebase convention used at every other partial_cmp site. (audit xcut_err-3)
        .min_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
}

// ── FDSN seismic source registry ──────────────────────────────────────────────
// All use the identical FDSN Web Services standard — same query format,
// same response format. One client handles all of them.

#[derive(Debug, Clone)]
#[allow(dead_code)] // name and region are metadata — displayed in sources tab Phase 3
pub struct FdsnSource {
    pub id:      &'static str,
    pub name:    &'static str,
    pub base:    &'static str,
    pub region:  &'static str,  // geographic strength
}

pub const FDSN_SOURCES: &[FdsnSource] = &[
    FdsnSource {
        id: "usgs", name: "USGS (United States Geological Survey)",
        base: "https://earthquake.usgs.gov/fdsnws/event/1",
        region: "Global — authoritative",
    },
    FdsnSource {
        id: "emsc", name: "EMSC (Euro-Med Seismological Centre)",
        base: "https://www.seismicportal.eu/fdsnws/event/1",
        region: "Europe / Middle East — often faster",
    },
    FdsnSource {
        id: "gfz", name: "GFZ Potsdam (German Research Centre)",
        base: "https://geofon.gfz-potsdam.de/fdsnws/event/1",
        region: "Eurasia — excellent Russia/Central Asia coverage",
    },
    FdsnSource {
        id: "ingv", name: "INGV (Istituto Nazionale di Geofisica e Vulcanologia)",
        base: "https://webservices.ingv.it/fdsnws/event/1",
        region: "Mediterranean / Middle East",
    },
    FdsnSource {
        id: "iris", name: "IRIS/FDSN (Incorporated Research Institutions)",
        base: "https://service.iris.edu/fdsnws/event/1",
        region: "Global — aggregates multiple networks",
    },
];

// ── FDSN response structs ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct FdsnResponse {
    features: Vec<FdsnFeature>,
}

#[derive(Debug, Deserialize)]
struct FdsnFeature {
    id:         String,
    properties: FdsnProperties,
    geometry:   FdsnGeometry,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)] // depth read via geometry.coordinates[2]; event_type reserved for future filtering
struct FdsnProperties {
    mag:    Option<f64>,
    place:  Option<String>,
    time:   Option<i64>,   // epoch milliseconds
    depth:  Option<f64>,   // km — provided in geometry.coordinates[2]
    #[serde(rename = "type")]
    event_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FdsnGeometry {
    coordinates: Vec<f64>,  // [lon, lat, depth_km]
}

// ── Alert level ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeismicAlertLevel {
    /// Single network detection only — awaiting corroboration
    Anomaly,
    /// Confirmed by 2+ independent networks
    MultiNetwork,
    /// No aftershock sequence detected at 2h re-query — consistent with
    /// explosion source (natural quakes produce aftershock sequences)
    AftershockAbsent,
    /// CTBTO has issued a public statement about this event
    CtbtoStatement,
}

impl std::fmt::Display for SeismicAlertLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SeismicAlertLevel::Anomaly           => write!(f, "SEISMIC ANOMALY — UNVERIFIED"),
            SeismicAlertLevel::MultiNetwork      => write!(f, "SEISMIC ANOMALY — MULTI-NETWORK"),
            SeismicAlertLevel::AftershockAbsent  => write!(f, "SEISMIC ANOMALY — NO AFTERSHOCK SEQUENCE"),
            SeismicAlertLevel::CtbtoStatement    => write!(f, "SEISMIC ANOMALY — CTBTO STATEMENT ISSUED"),
        }
    }
}

// ── Seismic alert ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeismicAlert {
    pub id:               String,
    pub level:            SeismicAlertLevel,
    pub detected_at:      DateTime<Utc>,
    pub event_time:       DateTime<Utc>,

    // Seismic parameters
    pub magnitude:        f64,
    pub depth_km:         f64,
    pub lat:              f64,
    pub lon:              f64,
    pub place:            String,

    // Test site correlation
    pub nearest_site:     String,   // site id
    pub nearest_site_name: String,
    pub actor:            String,
    pub distance_km:      f64,
    pub within_radius:    bool,

    // Multi-network corroboration
    pub networks:         Vec<String>,  // which FDSN sources confirmed
    pub corroboration:    usize,        // number of confirming networks

    // Confidence scoring
    pub confidence:       f64,   // 0-1
    pub description:      String,

    // Aftershock check
    pub aftershock_checked: bool,
    pub aftershock_count:   usize,
    pub aftershock_check_at: Option<DateTime<Utc>>,

    // CTBTO
    pub ctbto_statement:  bool,
    pub ctbto_text:       Option<String>,

    // Nuclear news correlation
    pub news_escalation_score: f64,
}

impl SeismicAlert {
    /// True when this event has cleared the natural-earthquake discriminator and
    /// sits inside a known test site's radius — the detector's own "consistent with
    /// an explosion source" determination. Deterministic (level + proximity, never
    /// the language model): trips only at `AftershockAbsent` (no aftershock sequence
    /// at the 2h re-query) or `CtbtoStatement` (a CTBTO public statement), so a raw
    /// single-network `Anomaly` or a merely multi-network detection that has NOT yet
    /// passed the aftershock test does NOT over-claim a test. Surfaced onto the I&W
    /// board via `RiskSnapshot::seismic_test_consistent`; it does not feed P(WWIII).
    pub fn is_test_consistent(&self) -> bool {
        self.within_radius
            && matches!(
                self.level,
                SeismicAlertLevel::AftershockAbsent | SeismicAlertLevel::CtbtoStatement
            )
    }

    fn compute_confidence(&self) -> f64 {
        let mut score: f64 = 0.0;

        // Depth component — shallower = more suspicious (surface explosion)
        // Nuclear tests are typically 0-2km. Tectonic events rarely < 5km.
        score += if self.depth_km < 2.0      { 0.35 }
                 else if self.depth_km < 5.0  { 0.25 }
                 else if self.depth_km < 10.0 { 0.15 }
                 else                          { 0.05 };

        // Magnitude component — tests typically 4.0-6.5
        score += if self.magnitude >= 4.5 && self.magnitude <= 6.5 { 0.15 } else { 0.05 };

        // Proximity to known site
        score += if self.within_radius && self.distance_km < 50.0  { 0.25 }
                 else if self.within_radius && self.distance_km < 100.0 { 0.15 }
                 else if self.within_radius                          { 0.08 }
                 else                                                { 0.0  };

        // Multi-network corroboration
        score += match self.corroboration {
            0 | 1 => 0.0,
            2     => 0.10,
            3     => 0.15,
            _     => 0.20,
        };

        // Aftershock absence (strong signal — natural quakes produce sequences)
        if self.aftershock_checked {
            score += if self.aftershock_count == 0 { 0.20 } else { 0.0 };
        }

        // CTBTO statement (near-definitive)
        if self.ctbto_statement { score += 0.30; }

        // News escalation correlation (I-18: capped input from tightened scorer)
        score += (self.news_escalation_score * 0.10).min(0.10);

        score.min(1.0)
    }
}

// ── Seen event cache — deduplication across 5 networks ───────────────────────

#[derive(Debug, Default)]
struct SeenEvents {
    // event_id → list of network IDs that have reported it
    events: HashMap<String, Vec<String>>,
    // sorted by detection time for pruning
    order:  Vec<(DateTime<Utc>, String)>,
}

impl SeenEvents {
    fn record(&mut self, event_id: &str, network_id: &str) -> usize {
        let networks = self.events.entry(event_id.to_string()).or_default();
        // First time we see this event id: register it in the time-ordered list
        // so prune_older_than_hours can age it out. Without this the order list
        // stays empty and pruning wipes the entire dedup cache on every poll.
        if networks.is_empty() {
            self.order.push((Utc::now(), event_id.to_string()));
        }
        if !networks.contains(&network_id.to_string()) {
            networks.push(network_id.to_string());
        }
        networks.len()
    }

    fn is_new(&self, event_id: &str) -> bool {
        !self.events.contains_key(event_id)
    }

    fn prune_older_than_hours(&mut self, hours: f64) {
        let cutoff = Utc::now() - chrono::Duration::seconds((hours * 3600.0) as i64);
        self.order.retain(|(t, _)| *t > cutoff);
        let live: std::collections::HashSet<String> = self.order.iter()
            .map(|(_, id)| id.clone()).collect();
        self.events.retain(|k, _| live.contains(k));
    }
}

// ── Seismic monitor ───────────────────────────────────────────────────────────

pub struct SeismicMonitor {
    client:     Client,
    state:      Arc<AppState>,
    seen:       Arc<Mutex<SeenEvents>>,
}

impl SeismicMonitor {
    pub fn new(state: Arc<AppState>) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(12))
                .user_agent("GCRM/1.0 (seismic-monitor; research; contact: gcrm@raithe.ca)")
                .build()
                .expect("HTTP client"),
            state,
            seen: Arc::new(Mutex::new(SeenEvents::default())),
        }
    }

    pub async fn run(self) {
        let monitor = Arc::new(self);
        info!(
            "Seismic monitor: {} FDSN sources, 60s poll interval",
            FDSN_SOURCES.len()
        );
        info!(
            "Seismic monitor: {} known test sites, latency ~3-8 min (USGS processing)",
            KNOWN_TEST_SITES.len()
        );

        let mut tick = interval(Duration::from_secs(60));
        loop {
            tick.tick().await;
            // Poll all 5 sources in parallel
            let handles: Vec<_> = FDSN_SOURCES.iter().map(|src| {
                let m = Arc::clone(&monitor);
                let src_id   = src.id;
                let src_base = src.base;
                tokio::spawn(async move { m.query_source(src_id, src_base).await })
            }).collect();

            for h in handles {
                if let Err(e) = h.await {
                    debug!("Seismic worker join error: {e}");
                }
            }

            // Prune events older than 48h from seen cache
            monitor.seen.lock().await.prune_older_than_hours(48.0);

            // Prune resolved / stale alerts so the dashboard does not flash forever.
            {
                let now = Utc::now();
                let mut alerts = monitor.state.nuclear_alerts.lock().await;
                alerts.retain(|a| alert_should_retain(a, now));
            }
        }
    }

    async fn query_source(&self, source_id: &str, base: &str) {
        // Query last 30 minutes of events matching nuclear test profile:
        //   magnitude ≥ 4.5, depth ≤ 10km
        let start = (Utc::now() - chrono::Duration::minutes(30))
            .format("%Y-%m-%dT%H:%M:%S").to_string();
        let url = format!(
            "{}/query?format=geojson&minmagnitude=4.5&maxdepth=10\
             &starttime={}&orderby=time&limit=20",
            base, start
        );

        let resp = match self.client.get(&url).send().await {
            Ok(r)  => r,
            Err(e) => { debug!("FDSN {source_id}: {e}"); return; }
        };
        if !resp.status().is_success() {
            debug!("FDSN {source_id}: HTTP {}", resp.status());
            return;
        }
        let data: FdsnResponse = match resp.json().await {
            Ok(d)  => d,
            Err(e) => { debug!("FDSN {source_id} parse: {e}"); return; }
        };

        for feat in &data.features {
            self.process_event(feat, source_id).await;
        }
    }

    async fn process_event(&self, feat: &FdsnFeature, source_id: &str) {
        let lon   = feat.geometry.coordinates.first().copied().unwrap_or(0.0);
        let lat   = feat.geometry.coordinates.get(1).copied().unwrap_or(0.0);
        let depth = feat.geometry.coordinates.get(2).copied().unwrap_or(999.0);
        let mag   = feat.properties.mag.unwrap_or(0.0);

        // Only care about shallow, reasonably-sized events
        if depth > 10.0 || mag < 4.5 { return; }

        // Check proximity to known test sites
        let site_match = nearest_test_site(lat, lon);
        let (nearest_site, distance_km, within_radius) = match &site_match {
            Some((site, dist)) => (Some(*site), *dist, true),
            None => {
                // Not near any known site — no further processing
                return;
            }
        };
        let site = nearest_site.unwrap();

        let event_id = &feat.id;
        // Compute the news-escalation score BEFORE taking is_new, so building+pushing the new
        // alert is the first await after the seen-record. Otherwise a 2nd network reporting the
        // SAME event during this (in-memory but non-trivial) await saw is_new==false yet found
        // no alert to upgrade — the 1st task hadn't pushed it yet — and its corroboration was
        // silently dropped. Seismic events are infrequent, so computing this for the occasional
        // duplicate is negligible. (audit detector-4)
        let news_score = self.news_escalation_score(site.actor).await;

        // Take is_new and record under a single lock so two networks reporting the
        // same event concurrently cannot both observe is_new == true.
        let (is_new, networks) = {
            let mut seen = self.seen.lock().await;
            let is_new   = seen.is_new(event_id);
            let networks = seen.record(event_id, source_id);
            (is_new, networks)
        };

        let event_time = feat.properties.time
            .map(|ms| {
                DateTime::from_timestamp(ms / 1000, 0).unwrap_or_else(Utc::now)
            })
            .unwrap_or_else(Utc::now);

        if is_new {
            info!(
                "SEISMIC ANOMALY detected: M{:.1} depth={:.1}km near {} ({:.0}km from {})",
                mag, depth,
                feat.properties.place.as_deref().unwrap_or("unknown"),
                distance_km, site.name
            );

            // news_score was computed above (before is_new) to keep the push race-free.
            let mut alert = SeismicAlert {
                id:                   event_id.clone(),
                level:                SeismicAlertLevel::Anomaly,
                detected_at:          Utc::now(),
                event_time,
                magnitude:            mag,
                depth_km:             depth,
                lat, lon,
                place:                feat.properties.place.clone().unwrap_or_default(),
                nearest_site:         site.id.to_string(),
                nearest_site_name:    site.name.to_string(),
                actor:                site.actor.to_string(),
                distance_km,
                within_radius,
                networks:             vec![source_id.to_string()],
                corroboration:        1,
                confidence:           0.0,
                description:          String::new(),
                aftershock_checked:   false,
                aftershock_count:     0,
                aftershock_check_at:  None,
                ctbto_statement:      false,
                ctbto_text:           None,
                news_escalation_score: news_score,
            };
            alert.confidence  = alert.compute_confidence();
            alert.description = Self::build_description(&alert);

            self.state.nuclear_alerts.lock().await.push(alert.clone());

            // Schedule aftershock check in 2 hours
            let state  = Arc::clone(&self.state);
            let client = self.client.clone();
            let ev_id  = event_id.clone();
            let ev_lat = lat;
            let ev_lon = lon;
            tokio::spawn(async move {
                sleep(Duration::from_secs(7200)).await; // 2 hours
                check_aftershocks(&client, &state, &ev_id, ev_lat, ev_lon).await;
            });

        } else if networks >= 2 {
            // Upgrade to multi-network if we haven't already
            let mut alerts = self.state.nuclear_alerts.lock().await;
            if let Some(a) = alerts.iter_mut().find(|a| a.id == *event_id) {
                // Track corroboration on EVERY confirming network, not just the first.
                // Previously this whole block was gated on `level == Anomaly`, so the 2nd
                // network promoted Anomaly→MultiNetwork and froze corroboration at 2 — the
                // 3rd/4th/5th networks were silently dropped and confidence under-stated.
                a.corroboration = networks;
                if !a.networks.contains(&source_id.to_string()) {
                    a.networks.push(source_id.to_string());
                }
                // One-time level promotion only; never regress AftershockAbsent/CtbtoStatement.
                if a.level == SeismicAlertLevel::Anomaly {
                    a.level = SeismicAlertLevel::MultiNetwork;
                }
                a.confidence  = a.compute_confidence();
                a.description = if a.aftershock_checked {
                    Self::build_description_static(a)
                } else {
                    Self::build_description(a)
                };
                info!(
                    "SEISMIC ANOMALY corroboration: {} network(s) M{:.1} near {}",
                    networks, mag, site.name
                );
            }
        }
    }

    /// Compute a news escalation score for a given actor using the article store.
        async fn news_escalation_score(&self, actor: &str) -> f64 {
        let store = self.state.article_store.lock().await;
        let now   = Utc::now();
        let name  = actor.replace('_', " ");
        let relevant: Vec<_> = store.articles.iter()
            .filter(|a| {
                let age_h = (now - chrono::DateTime::parse_from_rfc3339(&a.published_at)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or(now))
                    .num_hours() as f64;
                age_h < 72.0
                    && a.domain_tags.contains(&"nuclear_posture".to_string())
                    && mentions(&a.body.to_lowercase(), &name)
            })
            .collect();
        (relevant.len() as f64 / 20.0).min(1.0)
    }

    fn build_description(a: &SeismicAlert) -> String {
        format!(
            "M{:.1} depth={:.1}km | {:.0}km from {} ({}) | {} network(s) | \
             confidence={:.0}% | {}",
            a.magnitude, a.depth_km, a.distance_km, a.nearest_site_name,
            a.actor.replace('_', " "),
            a.corroboration,
            a.confidence * 100.0,
            a.level,
        )
    }
}

// ── Aftershock checker ────────────────────────────────────────────────────────
// Nuclear detonations produce a single seismic event (the explosion) with
// no aftershock sequence. Natural earthquakes produce a characteristic
// Gutenberg-Richter aftershock sequence within 2-6 hours.
// This is one of the strongest discriminating signals available.

/// The verdict of a COMPLETED 2h aftershock re-query. `returned` is the TOTAL number of
/// features the source (USGS) returned for the region/window — its COVERAGE proof; and
/// `aftershock_count` is that set minus the original mainshock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AftershockVerdict {
    /// ≥ `AFTERSHOCK_SEQUENCE_MIN` nearby M≥2.5 events — a Gutenberg-Richter/Omori sequence,
    /// i.e. a natural tectonic source → clear the alert.
    Sequence,
    /// The source COVERED the region (returned ≥1 feature) yet found no aftershock sequence —
    /// consistent with an explosion source → `AftershockAbsent`.
    Absent,
    /// A single nearby event — background OR the start of a sequence; leave the level as-is.
    Ambiguous,
    /// The source returned NO data for the region/window — no coverage, so "no aftershocks" is
    /// UNPROVEN. Absence of coverage is not evidence of absence: an empty response must NOT
    /// confirm an explosion signature. `check_aftershocks` hardcodes a USGS-only query, but the
    /// detector's own FDSN registry notes USGS ComCat completeness for small events in the
    /// Arctic / Central-Asian test-site regions (Novaya Zemlya, Lop Nur, Semipalatinsk) is poor
    /// and GFZ/EMSC cover those better — so an empty USGS response there means "USGS has no
    /// catalog here", not "the quake had no aftershocks". Leave the alert untouched (no verdict).
    Inconclusive,
}

/// Classify a completed aftershock re-query. Keyed on `AFTERSHOCK_SEQUENCE_MIN` so it shares the
/// natural-earthquake boundary with `alert_should_retain`. The `returned == 0` guard is the
/// honesty rule: an EMPTY single-source response is `Inconclusive` (no coverage), never a
/// confirmed absence — so the strongest physical nuclear indicator cannot light off USGS simply
/// lacking catalog data for a remote test-site region. (A non-empty response with zero
/// aftershocks proves the source SAW the region, so that is a genuine confirmed `Absent`.)
fn aftershock_verdict(returned: usize, aftershock_count: usize) -> AftershockVerdict {
    if aftershock_count >= AFTERSHOCK_SEQUENCE_MIN {
        AftershockVerdict::Sequence
    } else if returned == 0 {
        AftershockVerdict::Inconclusive
    } else if aftershock_count == 0 {
        AftershockVerdict::Absent
    } else {
        AftershockVerdict::Ambiguous
    }
}

/// Apply a non-clearing aftershock verdict (`Absent` or `Ambiguous`) to a live alert: record the
/// completed check and, for `Absent` only, promote the level to `AftershockAbsent`. `Sequence`
/// (clear) and `Inconclusive` (leave untouched — no verdict) are handled by the caller and must
/// NOT reach here.
fn apply_aftershock_verdict(
    alert:   &mut SeismicAlert,
    verdict: AftershockVerdict,
    count:   usize,
    now:     DateTime<Utc>,
) {
    alert.aftershock_checked  = true;
    alert.aftershock_count    = count;
    alert.aftershock_check_at = Some(now);
    if verdict == AftershockVerdict::Absent {
        // No aftershocks over a region the source demonstrably covered — explosion-consistent.
        alert.level = SeismicAlertLevel::AftershockAbsent;
    }
    // Ambiguous (count == 1): background OR the start of a sequence — leave the level as-is.
    // compute_confidence already withholds the +0.20 absence bonus when count > 0.
    alert.confidence  = alert.compute_confidence();
    alert.description = SeismicMonitor::build_description_static(alert);
}

async fn check_aftershocks(
    client:   &Client,
    state:    &Arc<AppState>,
    event_id: &str,
    lat:      f64,
    lon:      f64,
) {
    // Query for events within 50km of the original event in last 2h
    let start = (Utc::now() - chrono::Duration::hours(2))
        .format("%Y-%m-%dT%H:%M:%S").to_string();
    let url = format!(
        "https://earthquake.usgs.gov/fdsnws/event/1/query\
         ?format=geojson&minmagnitude=2.5&maxradiuskm=50\
         &latitude={lat:.4}&longitude={lon:.4}\
         &starttime={start}&orderby=time&limit=50"
    );

    // Capture BOTH the TOTAL features USGS returned (coverage proof) and the aftershock count
    // (that set minus the mainshock). An empty response is coverage-absent, NOT aftershock-absent.
    let (returned, aftershock_count) = match client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => {
            match resp.json::<FdsnResponse>().await {
                Ok(data) => (
                    data.features.len(),
                    data.features.iter().filter(|f| f.id != event_id).count(),
                ),
                Err(_) => return,
            }
        }
        _ => return,
    };

    let verdict = aftershock_verdict(returned, aftershock_count);

    let mut alerts = state.nuclear_alerts.lock().await;
    if verdict == AftershockVerdict::Sequence {
        // A genuine aftershock SEQUENCE (multiple nearby events, Omori-style) — natural
        // tectonic source, not an explosion. Clear the alert. A SINGLE coincidental nearby
        // M≥2.5 is background seismicity, not a sequence, and no longer clears a real
        // explosion-consistent anomaly (the old `> 0` was a false-calm bias). (audit detector-3)
        let before = alerts.len();
        alerts.retain(|a| a.id != event_id);
        if alerts.len() < before {
            info!(
                "Seismic event {}: {} aftershocks (sequence) — tectonic source, alert cleared",
                event_id, aftershock_count
            );
        }
        return;
    }
    if verdict == AftershockVerdict::Inconclusive {
        // Empty USGS response: no catalog coverage for this region/window, so "no aftershocks"
        // is unproven. Do NOT promote to AftershockAbsent (which lights the strongest physical
        // nuclear indicator) or award the +0.20 absence bonus — absence of coverage is not
        // evidence of absence. Leave the alert at its pre-check level; log for ops.
        info!(
            "SEISMIC ANOMALY aftershock check at 2h: USGS returned no data near {} \
             ({:.2},{:.2}) — discriminator INCONCLUSIVE (no coverage), level unchanged",
            event_id, lat, lon
        );
        return;
    }

    if let Some(alert) = alerts.iter_mut().find(|a| a.id == event_id) {
        // Absent or Ambiguous — a real verdict over a region USGS demonstrably covered.
        apply_aftershock_verdict(alert, verdict, aftershock_count, Utc::now());
        info!(
            "SEISMIC ANOMALY aftershock check at 2h: {} aftershock(s) → {} (confidence {:.0}%)",
            aftershock_count, alert.id, alert.confidence * 100.0
        );
    }
}

/// Minimum nearby M≥2.5 events (within 50 km / 2 h, excluding the original) that constitute a
/// real aftershock SEQUENCE — the Omori signature of a natural tectonic source. A single event
/// is background seismicity, not a sequence, so it must not clear an explosion-consistent
/// anomaly. (audit detector-3)
const AFTERSHOCK_SEQUENCE_MIN: usize = 2;

/// Whether a live nuclear-seismic alert should be RETAINED on the board (true) or pruned
/// (false) at wall-clock `now`. This is the SAME natural-earthquake boundary `check_aftershocks`
/// enforces: a real aftershock SEQUENCE (≥ `AFTERSHOCK_SEQUENCE_MIN` nearby M≥2.5 events) marks a
/// tectonic source → prune; a SINGLE coincidental nearby quake is background seismicity, NOT a
/// sequence, and must not clear an explosion-consistent anomaly. The prune previously used a bare
/// `aftershock_count > 0`, which deleted the exact `count == 1` alert `check_aftershocks`
/// deliberately KEEPS as ambiguous — resurrecting, one poll later, the false-calm bias
/// detector-3 removed (and, for a CTBTO-confirmed within-radius event carrying count==1, silently
/// flipping the served `seismic_test_consistent` I&W light true→false). Keyed on the named
/// constant so the two paths can never drift again. (audit detector-3)
fn alert_should_retain(a: &SeismicAlert, now: DateTime<Utc>) -> bool {
    // Aftershock SEQUENCE detected → natural earthquake, not a test.
    if a.aftershock_checked && a.aftershock_count >= AFTERSHOCK_SEQUENCE_MIN { return false; }
    // Single-network anomaly that never corroborated → expire after 24h.
    if a.level == SeismicAlertLevel::Anomaly
        && (now - a.detected_at) > chrono::Duration::hours(24) {
        return false;
    }
    // Escalated alerts persist longer but still expire after 7 days.
    if (now - a.detected_at) > chrono::Duration::days(7) { return false; }
    true
}

impl SeismicMonitor {
    fn build_description_static(a: &SeismicAlert) -> String {
        format!(
            "M{:.1} depth={:.1}km | {:.0}km from {} ({}) | {} network(s) | \
             confidence={:.0}% | {} aftershocks at 2h | {}",
            a.magnitude, a.depth_km, a.distance_km, a.nearest_site_name,
            a.actor.replace('_', " "),
            a.corroboration,
            a.confidence * 100.0,
            a.aftershock_count,
            a.level,
        )
    }
}

// ── CTBTO RSS monitor ─────────────────────────────────────────────────────────
// CTBTO publishes press statements via RSS. These lag by hours to days but
// are the closest thing to an official confirmation available publicly.

pub struct CtbtoMonitor {
    client: Client,
    state:  Arc<AppState>,
    seen:   std::collections::HashSet<String>,
}

impl CtbtoMonitor {
    pub fn new(state: Arc<AppState>) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(15))
                .user_agent("GCRM/1.0")
                .build()
                .expect("HTTP client"),
            state,
            seen: std::collections::HashSet::new(),
        }
    }

    pub async fn run(mut self) {
        info!("CTBTO RSS monitor: polling every 300s (statements are hours-to-days delayed)");
        let mut tick = interval(Duration::from_secs(300));
        loop {
            tick.tick().await;
            self.poll().await;
        }
    }

    async fn poll(&mut self) {
        let url = "https://www.ctbto.org/press-centre/rss";
        let text = match self.client.get(url).send().await {
            Ok(r) if r.status().is_success() => match r.text().await {
                Ok(t) => t,
                Err(_) => return,
            },
            _ => return,
        };

        // Simple XML scan — look for detection-specific phrases in titles. The old
        // list included generic words ("monitoring", "event", "test", "nuclear")
        // that appear in nearly every CTBTO press release, causing routine posts to
        // falsely escalate alerts. These require an actual detection context.
        let nuclear_keywords = [
            "seismic event", "unusual event", "explosion", "detonation",
            "nuclear test", "anomal", "detection of",
        ];
        let lines: Vec<&str> = text.lines().collect();

        for (i, line) in lines.iter().enumerate() {
            if line.contains("<item>") {
                let chunk = lines[i..].iter().take(10).cloned().collect::<Vec<_>>().join("\n");
                let title = extract_xml_text(&chunk, "title").unwrap_or_default();
                let guid  = extract_xml_text(&chunk, "guid").unwrap_or_else(|| title.clone());
                let lower = title.to_lowercase();

                if nuclear_keywords.iter().any(|kw| lower.contains(kw))
                    && !self.seen.contains(&guid)
                {
                    self.seen.insert(guid.clone());
                    info!("CTBTO statement detected: {title}");

                    // Correlate only with a recent alert the statement PLAUSIBLY refers to:
                    // the alert's actor (country) or nearest-site name must appear in the CTBTO
                    // title. Mere recency is NOT correlation — a generic detection-keyword press
                    // item must not blindly escalate the most-recent alert (anywhere on Earth) to
                    // the highest CtbtoStatement level with no geographic/actor link, flipping the
                    // board's test-consistency light off a coincidence. No confident match → log
                    // only, mutate nothing (same discipline as the no-recent-alert branch). (audit detector-1)
                    let now = Utc::now();
                    let mut alerts = self.state.nuclear_alerts.lock().await;
                    let matched = alerts.iter_mut()
                        .filter(|a| (now - a.detected_at) < chrono::Duration::days(7))
                        .filter(|a| {
                            let actor = a.actor.replace('_', " ");
                            let site  = a.nearest_site_name.to_lowercase();
                            mentions(&lower, &actor) || mentions(&lower, &site)
                        })
                        .max_by_key(|a| a.detected_at);
                    if let Some(alert) = matched {
                        alert.ctbto_statement = true;
                        alert.ctbto_text      = Some(title.clone());
                        alert.level           = SeismicAlertLevel::CtbtoStatement;
                        alert.confidence      = alert.compute_confidence();
                        info!("CTBTO statement correlated with seismic alert {} (actor/site match)", alert.id);
                    } else {
                        info!("CTBTO statement '{title}' — no actor/site-matching recent seismic alert to correlate");
                    }
                }
            }
        }
    }
}

/// Whole-word actor/site match for the nuclear cross-check correlation paths (the
/// `news_escalation_score` news filter and the CTBTO↔seismic-alert correlation). A bare
/// `str::contains` let an actor/site name match INSIDE an ordinary word — `"india"⊂"indian
/// ocean"`, `"china"⊂"indochina"` — so a nuclear-posture-tagged "Indian Ocean" story inflated
/// India's seismic-alert confidence and a coincidental CTBTO press item could correlate to the
/// wrong actor's alert, flipping the board's nuclear test-consistency read off a coincidence.
/// Routes through the crate's boundary-aware matcher (the 1.7/1.8 substring→word-boundary honesty
/// fix) so the name must appear as a WHOLE word. Both args must already be lowercased; an empty
/// `name` never matches (the callers used to guard this explicitly). Internal hyphens/spaces in
/// multi-word site names ("lop nur", "punggye-ri") are fine — only the ends are boundary-checked.
fn mentions(text: &str, name: &str) -> bool {
    !name.is_empty() && crate::processor::contains_word(text, name)
}

fn extract_xml_text(xml: &str, tag: &str) -> Option<String> {
    let open        = format!("<{tag}>");
    let close       = format!("</{tag}>");
    let cdata_open  = format!("<{tag}><![CDATA[");
    let cdata_close = "]]>";

    if let Some(start) = xml.find(&cdata_open) {
        let rest = &xml[start + cdata_open.len()..];
        if let Some(end) = rest.find(cdata_close) {
            return Some(rest[..end].trim().to_string());
        }
    }
    if let Some(start) = xml.find(&open) {
        let rest = &xml[start + open.len()..];
        if let Some(end) = rest.find(&close) {
            return Some(rest[..end].trim().to_string());
        }
    }
    None
}

// ── Nuclear news monitor ──────────────────────────────────────────────────────
// Watches the article store for elevated nuclear-posture signal.
// A sustained spike in nuclear-tagged articles, especially involving
// DPRK/Russia/Pakistan, precedes historical test events.

pub struct NuclearNewsMonitor {
    state: Arc<AppState>,
}

const NUCLEAR_NEWS_ALERT_THRESHOLD: usize = 25;

#[allow(dead_code)]
const NEWS_ESCALATION_NORMALISER: usize = 20;

impl NuclearNewsMonitor {
    pub fn new(state: Arc<AppState>) -> Self {
        Self { state }
    }

    pub async fn run(self) {
        let mut tick = interval(Duration::from_secs(60));
        loop {
            tick.tick().await;
            self.check().await;
        }
    }

    async fn check(&self) {
        let store = self.state.article_store.lock().await;
        let now   = Utc::now();

        let nuclear_24h = store.articles.iter()
            .filter(|a| {
                let age_h = (now - chrono::DateTime::parse_from_rfc3339(&a.published_at)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or(now))
                    .num_hours();
                age_h < 24 && a.domain_tags.contains(&"nuclear_posture".to_string())
            })
            .count();
        drop(store);

        if nuclear_24h >= NUCLEAR_NEWS_ALERT_THRESHOLD {
            info!(
                "Nuclear news escalation: {} nuclear-tagged articles in 24h (threshold: {})",
                nuclear_24h, NUCLEAR_NEWS_ALERT_THRESHOLD
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Haversine ─────────────────────────────────────────────────────────────

    #[test]
    fn haversine_zero_distance() {
        assert!(haversine_km(41.28, 129.08, 41.28, 129.08) < 0.001);
    }

    #[test]
    fn haversine_london_paris_approx() {
        // London (51.5, -0.1) to Paris (48.85, 2.35) ≈ 341km
        let d = haversine_km(51.5, -0.1, 48.85, 2.35);
        assert!(d > 330.0 && d < 360.0, "London-Paris should be ~341km, got {d:.1}");
    }

    #[test]
    fn haversine_antipodal_approx() {
        // Antipodal points ≈ half Earth circumference ≈ 20015km
        let d = haversine_km(0.0, 0.0, 0.0, 180.0);
        assert!(d > 19900.0 && d < 20200.0);
    }

    // ── Test site registry ────────────────────────────────────────────────────

    #[test]
    fn test_sites_nonempty() {
        assert!(!KNOWN_TEST_SITES.is_empty());
    }

    #[test]
    fn test_sites_count() {
        assert_eq!(KNOWN_TEST_SITES.len(), 10);
    }

    #[test]
    fn test_sites_unique_ids() {
        let mut ids = std::collections::HashSet::new();
        for site in KNOWN_TEST_SITES {
            assert!(ids.insert(site.id), "Duplicate site id: {}", site.id);
        }
    }

    #[test]
    fn test_sites_valid_coordinates() {
        for site in KNOWN_TEST_SITES {
            assert!(site.lat >= -90.0  && site.lat <= 90.0,  "Invalid lat for {}", site.id);
            assert!(site.lon >= -180.0 && site.lon <= 180.0, "Invalid lon for {}", site.id);
            assert!(site.radius_km > 0.0, "Zero radius for {}", site.id);
        }
    }

    #[test]
    fn punggye_ri_coordinates_correct() {
        let site = KNOWN_TEST_SITES.iter().find(|s| s.id == "punggye_ri").unwrap();
        assert!(site.lat > 40.0 && site.lat < 43.0);
        assert!(site.lon > 128.0 && site.lon < 131.0);
        assert!(site.active);
    }

    #[test]
    fn novaya_zemlya_coordinates_correct() {
        let site = KNOWN_TEST_SITES.iter().find(|s| s.id == "novaya_zemlya").unwrap();
        assert!(site.lat > 70.0);
        assert!(site.active);
    }

    // ── nearest_test_site ────────────────────────────────────────────────────

    #[test]
    fn nearest_site_punggye_ri_direct_hit() {
        let result = nearest_test_site(41.2833, 129.0833);
        assert!(result.is_some());
        let (site, dist) = result.unwrap();
        assert_eq!(site.id, "punggye_ri");
        assert!(dist < 1.0);
    }

    #[test]
    fn nearest_site_100km_from_punggye_ri() {
        // 100km north of Punggye-ri — still within 150km radius
        let result = nearest_test_site(42.18, 129.08);
        assert!(result.is_some());
        let (site, _) = result.unwrap();
        assert_eq!(site.id, "punggye_ri");
    }

    #[test]
    fn nearest_site_far_from_all_sites() {
        // Mid-Pacific, nowhere near any test site
        let result = nearest_test_site(0.0, -160.0);
        assert!(result.is_none());
    }

    #[test]
    fn nearest_site_returns_closest() {
        let result = nearest_test_site(28.5, 65.0);
        // Near both Chagai and Ras Koh — should return whichever is closer
        assert!(result.is_some());
        let (site, dist) = result.unwrap();
        assert!(dist < 200.0);
        assert!(site.id == "chagai" || site.id == "ras_koh");
    }

    #[test]
    fn nearest_site_nevada_nts() {
        let result = nearest_test_site(37.12, -116.05);
        assert!(result.is_some());
        let (site, dist) = result.unwrap();
        assert_eq!(site.id, "nevada_nts");
        assert!(dist < 1.0);
    }

    // ── SeismicAlertLevel display ─────────────────────────────────────────────

    #[test]
    fn alert_level_display_strings() {
        assert!(SeismicAlertLevel::Anomaly.to_string().contains("SEISMIC ANOMALY"));
        assert!(SeismicAlertLevel::MultiNetwork.to_string().contains("MULTI-NETWORK"));
        assert!(SeismicAlertLevel::AftershockAbsent.to_string().contains("NO AFTERSHOCK"));
        assert!(SeismicAlertLevel::CtbtoStatement.to_string().contains("CTBTO"));
    }

    #[test]
    fn alert_level_display_never_says_nuclear_confirmed() {
        // Critical: we must never claim nuclear confirmation in alert labels
        for level in &[
            SeismicAlertLevel::Anomaly,
            SeismicAlertLevel::MultiNetwork,
            SeismicAlertLevel::AftershockAbsent,
            SeismicAlertLevel::CtbtoStatement,
        ] {
            let s = level.to_string();
            assert!(!s.to_lowercase().contains("nuclear detonation"),
                "Level display must not claim nuclear detonation: {s}");
            assert!(!s.to_lowercase().contains("confirmed nuclear"),
                "Level display must not claim confirmed nuclear: {s}");
        }
    }

    // ── FDSN source registry ─────────────────────────────────────────────────

    #[test]
    fn fdsn_sources_count() {
        assert_eq!(FDSN_SOURCES.len(), 5);
    }

    #[test]
    fn fdsn_sources_unique_ids() {
        let mut ids = std::collections::HashSet::new();
        for src in FDSN_SOURCES {
            assert!(ids.insert(src.id), "Duplicate FDSN source id: {}", src.id);
        }
    }

    #[test]
    fn fdsn_sources_all_https() {
        for src in FDSN_SOURCES {
            assert!(src.base.starts_with("https://"),
                "FDSN source {} should use HTTPS", src.id);
        }
    }

    // ── Confidence scoring ────────────────────────────────────────────────────

    fn make_alert(depth: f64, mag: f64, dist: f64, networks: usize) -> SeismicAlert {
        SeismicAlert {
            id: "test".into(),
            level: SeismicAlertLevel::Anomaly,
            detected_at: Utc::now(),
            event_time:  Utc::now(),
            magnitude:   mag,
            depth_km:    depth,
            lat: 41.28, lon: 129.08,
            place:             "Test".into(),
            nearest_site:      "punggye_ri".into(),
            nearest_site_name: "Punggye-ri".into(),
            actor:             "north_korea".into(),
            distance_km:       dist,
            within_radius:     dist < 150.0,
            networks:          vec!["usgs".into()],
            corroboration:     networks,
            confidence:        0.0,
            description:       String::new(),
            aftershock_checked: false,
            aftershock_count:   0,
            aftershock_check_at: None,
            ctbto_statement:   false,
            ctbto_text:        None,
            news_escalation_score: 0.0,
        }
    }

    #[test]
    fn confidence_increases_with_shallow_depth() {
        let shallow = make_alert(1.0, 5.0, 30.0, 1);
        let deep    = make_alert(9.0, 5.0, 30.0, 1);
        assert!(shallow.compute_confidence() > deep.compute_confidence());
    }

    #[test]
    fn confidence_increases_with_corroboration() {
        let single = make_alert(1.0, 5.0, 30.0, 1);
        let multi  = make_alert(1.0, 5.0, 30.0, 3);
        assert!(multi.compute_confidence() > single.compute_confidence());
    }

    #[test]
    fn confidence_increases_when_no_aftershocks() {
        let mut no_seq = make_alert(1.0, 5.0, 30.0, 2);
        no_seq.aftershock_checked = true;
        no_seq.aftershock_count   = 0;

        let mut has_seq = make_alert(1.0, 5.0, 30.0, 2);
        has_seq.aftershock_checked = true;
        has_seq.aftershock_count   = 5;

        assert!(no_seq.compute_confidence() > has_seq.compute_confidence());
    }

    #[test]
    fn aftershock_verdict_reads_an_empty_usgs_response_as_inconclusive_not_confirmed_absence() {
        // The honesty invariant: an EMPTY response (returned == 0) is INCONCLUSIVE — USGS had no
        // catalog coverage for the region, so "no aftershocks" is unproven. It must NOT read as a
        // confirmed absence (which would light the strongest physical nuclear indicator).
        assert_eq!(aftershock_verdict(0, 0), AftershockVerdict::Inconclusive);
        // A NON-empty response with zero aftershocks proves USGS SAW the region (the mainshock or
        // background) and found no sequence — a genuine confirmed absence.
        assert_eq!(aftershock_verdict(1, 0), AftershockVerdict::Absent);
        assert_eq!(aftershock_verdict(3, 0), AftershockVerdict::Absent);
        // A single nearby event — ambiguous, leave the level as-is.
        assert_eq!(aftershock_verdict(2, 1), AftershockVerdict::Ambiguous);
        // A real Gutenberg-Richter/Omori sequence — natural tectonic source, clear.
        assert_eq!(aftershock_verdict(2, 2), AftershockVerdict::Sequence);
        assert_eq!(aftershock_verdict(9, 5), AftershockVerdict::Sequence);
    }

    #[test]
    fn empty_usgs_response_does_not_light_the_seismic_test_consistent_indicator() {
        // A within-radius shallow anomaly at a remote test-site region whose 2h re-query hits
        // USGS and gets an EMPTY response (no small-event coverage there). Mirror the exact
        // verdict routing in `check_aftershocks`: Inconclusive leaves the alert untouched.
        let mut alert = make_alert(1.0, 5.0, 30.0, 3); // within_radius (dist 30 < 150), MultiNetwork-eligible
        alert.level = SeismicAlertLevel::MultiNetwork;
        let (returned, count) = (0usize, 0usize); // empty USGS response
        let verdict = aftershock_verdict(returned, count);
        assert_eq!(verdict, AftershockVerdict::Inconclusive);
        match verdict {
            AftershockVerdict::Absent | AftershockVerdict::Ambiguous =>
                apply_aftershock_verdict(&mut alert, verdict, count, Utc::now()),
            _ => {} // Inconclusive / Sequence: caller leaves the alert untouched
        }
        assert!(!alert.aftershock_checked,
            "an inconclusive (no-coverage) check must not mark the discriminator as run");
        assert_ne!(alert.level, SeismicAlertLevel::AftershockAbsent,
            "absence of USGS coverage must not promote to AftershockAbsent");
        assert!(!alert.is_test_consistent(),
            "absence of USGS coverage must not light the seismic-test-consistent indicator");

        // Contrast: a NON-empty response (USGS covered the region — returned the mainshock) with
        // zero aftershocks IS a genuine confirmed absence and legitimately lights the indicator.
        let mut covered = make_alert(1.0, 5.0, 30.0, 3);
        covered.level = SeismicAlertLevel::MultiNetwork;
        let verdict2 = aftershock_verdict(1, 0);
        assert_eq!(verdict2, AftershockVerdict::Absent);
        apply_aftershock_verdict(&mut covered, verdict2, 0, Utc::now());
        assert_eq!(covered.level, SeismicAlertLevel::AftershockAbsent);
        assert!(covered.is_test_consistent(),
            "a covered region with no aftershocks is explosion-consistent");
        // The +0.20 absence bonus applies only in the genuinely-covered case: the inconclusive
        // alert kept aftershock_checked=false (identical alert otherwise), so its confidence is
        // strictly lower — the honesty consequence, not just the label.
        assert!(covered.compute_confidence() > alert.compute_confidence(),
            "the confirmed-absence read carries the absence bonus the inconclusive read withholds");
    }

    #[test]
    fn confidence_ctbto_boosts_significantly() {
        let mut base = make_alert(8.0, 5.0, 140.0, 1);
        let c_base = base.compute_confidence();
        base.ctbto_statement = true;
        let c_ctbto = base.compute_confidence();
        assert!(c_ctbto > c_base, "CTBTO statement should raise confidence");
        assert!(c_ctbto > 0.40, "CTBTO-confirmed alert should have high confidence, got {c_ctbto:.3}");
    }

    #[test]
    fn confidence_never_exceeds_one() {
        let mut a = make_alert(0.5, 5.5, 10.0, 5);
        a.aftershock_checked    = true;
        a.aftershock_count      = 0;
        a.ctbto_statement       = true;
        a.news_escalation_score = 1.0;
        assert!(a.compute_confidence() <= 1.0);
    }

    #[test]
    fn is_test_consistent_requires_proximity_and_a_cleared_discriminator() {
        // The board's seismic light keys off this determination, so lock its honesty:
        // a raw single-network Anomaly — even shallow, near the site — is NOT yet
        // test-consistent (the natural-quake discriminator hasn't run); only an
        // aftershock-absent or CTBTO-confirmed event inside the radius qualifies.
        let raw = make_alert(1.0, 5.0, 30.0, 1);
        assert!(!raw.is_test_consistent(), "a bare Anomaly must not claim test-consistency");

        let mut multi = make_alert(1.0, 5.0, 30.0, 3);
        multi.level = SeismicAlertLevel::MultiNetwork;
        assert!(!multi.is_test_consistent(),
            "multi-network alone (no aftershock test) is not test-consistency");

        let mut absent = make_alert(1.0, 5.0, 30.0, 3);
        absent.level = SeismicAlertLevel::AftershockAbsent;
        assert!(absent.is_test_consistent(),
            "aftershock-absent inside the radius is the explosion-consistent state");

        // Same cleared discriminator but OUTSIDE any test-site radius → not attributable.
        let far = SeismicAlert { within_radius: false,
            level: SeismicAlertLevel::AftershockAbsent, ..make_alert(1.0, 5.0, 300.0, 3) };
        assert!(!far.is_test_consistent(), "outside every site radius is not test-consistent");

        let mut ctbto = make_alert(8.0, 5.0, 90.0, 2);
        ctbto.level = SeismicAlertLevel::CtbtoStatement;
        assert!(ctbto.is_test_consistent(), "a CTBTO statement inside the radius qualifies");
    }

    #[test]
    fn prune_keeps_a_single_aftershock_alert_but_clears_a_real_sequence() {
        // detector-3 boundary, enforced in the board-PRUNE path too (not only check_aftershocks):
        // a SINGLE coincidental nearby quake is background seismicity, not a sequence, and must NOT
        // delete an explosion-consistent anomaly. The prune previously used a bare `> 0`, which one
        // poll later deleted the exact count==1 alert check_aftershocks deliberately KEEPS as
        // ambiguous — resurrecting the false-calm bias detector-3 removed. Lock the aligned
        // `>= AFTERSHOCK_SEQUENCE_MIN` boundary shared by both paths.
        let now = Utc::now();

        // count == 1, checked → ambiguous, must be RETAINED (the case the old `> 0` wrongly pruned).
        let mut single = make_alert(1.0, 5.0, 30.0, 3);
        single.aftershock_checked = true;
        single.aftershock_count   = 1;
        assert!(alert_should_retain(&single, now),
            "a single coincidental aftershock is background, not a sequence — must be retained");

        // count >= AFTERSHOCK_SEQUENCE_MIN → real Omori sequence, natural source → prune.
        let mut sequence = make_alert(1.0, 5.0, 30.0, 3);
        sequence.aftershock_checked = true;
        sequence.aftershock_count   = AFTERSHOCK_SEQUENCE_MIN;
        assert!(!alert_should_retain(&sequence, now),
            "a real aftershock sequence marks a tectonic source — must be pruned");

        // A CTBTO-confirmed within-radius alert carrying count==1 stays test-consistent AND retained
        // — the served `seismic_test_consistent` light must not flip dark on one background quake.
        let mut ctbto = make_alert(8.0, 5.0, 90.0, 2);
        ctbto.level = SeismicAlertLevel::CtbtoStatement;
        ctbto.aftershock_checked = true;
        ctbto.aftershock_count   = 1;
        assert!(ctbto.is_test_consistent(), "CTBTO within-radius is explosion-consistent");
        assert!(alert_should_retain(&ctbto, now),
            "a single background quake must not prune a CTBTO-confirmed explosion-consistent alert");

        // Regression guard: the age-based expiries still fire.
        let mut stale_anomaly = make_alert(1.0, 5.0, 30.0, 1);
        stale_anomaly.level = SeismicAlertLevel::Anomaly;
        stale_anomaly.detected_at = now - chrono::Duration::hours(25);
        assert!(!alert_should_retain(&stale_anomaly, now),
            "an uncorroborated Anomaly older than 24h still expires");
        let mut ancient = make_alert(1.0, 5.0, 30.0, 3);
        ancient.level = SeismicAlertLevel::AftershockAbsent;
        ancient.detected_at = now - chrono::Duration::days(8);
        assert!(!alert_should_retain(&ancient, now),
            "any alert older than 7d still expires");
    }

    #[test]
    fn confidence_outside_radius_is_lower() {
        let inside  = make_alert(1.0, 5.0, 50.0, 1);
        let outside = SeismicAlert { within_radius: false, distance_km: 300.0,
            ..make_alert(1.0, 5.0, 300.0, 1) };
        assert!(inside.compute_confidence() > outside.compute_confidence());
    }

    #[test]
    fn news_escalation_score_contribution_capped_at_010() {
        let mut a = make_alert(2.0, 5.0, 30.0, 1);
        a.news_escalation_score = 1.0; // Maximum possible score
        let conf_with_news = a.compute_confidence();
        a.news_escalation_score = 0.0;
        let conf_no_news = a.compute_confidence();
        // The contribution is exactly 0.10 at score=1.0
        assert!((conf_with_news - conf_no_news - 0.10).abs() < 1e-9,
            "news_escalation contribution should be exactly 0.10 at score=1.0, \
             got {:.4}", conf_with_news - conf_no_news);
    }

    #[test]
    fn nuclear_cross_check_matches_actor_and_site_as_whole_words_not_substrings() {
        // Both nuclear cross-check paths — the news_escalation_score body filter and the
        // CTBTO↔seismic correlation — route actor/site matching through `mentions`. It must
        // match a WHOLE word: a bare `str::contains` let an actor name fire inside an ordinary
        // word ("india"⊂"indian ocean", "china"⊂"indochina"), casting phantom nuclear-news
        // escalation and phantom CTBTO correlation off a coincidence and skewing the board's
        // test-consistency read. Reverting `mentions` to `text.contains(name)` re-admits every
        // negative case below and FAILS this test (the fails-without-change lock).
        assert!(mentions("india test-fires a new missile", "india"));
        assert!(mentions("statement on north korea nuclear test", "north korea"));
        assert!(mentions("seismic event near lop nur test site", "lop nur"));
        assert!(mentions("aftershock check at punggye-ri", "punggye-ri"));
        // Substring-inside-a-word must NOT match (the phantom-correlation bug):
        assert!(!mentions("rising tension across the indian ocean", "india"));
        assert!(!mentions("naval movements off indiana's namesake", "india"));
        assert!(!mentions("cold-war indochina archives declassified", "china"));
        // Empty name never matches (callers relied on the old `!is_empty()` guard):
        assert!(!mentions("any text at all", ""));
    }

    #[test]
    fn nuclear_news_alert_threshold_is_25() {
        assert_eq!(NUCLEAR_NEWS_ALERT_THRESHOLD, 25,
            "NuclearNewsMonitor alert threshold must be 25 (I-18 fix; was 15)");
    }

    // ── Seen events cache ─────────────────────────────────────────────────────

    #[test]
    fn seen_events_new_item() {
        let seen = SeenEvents::default();
        assert!(seen.is_new("ev001"));
    }

    #[test]
    fn seen_events_after_record_not_new() {
        let mut seen = SeenEvents::default();
        seen.record("ev001", "usgs");
        assert!(!seen.is_new("ev001"));
    }

    #[test]
    fn seen_events_multiple_networks_counted() {
        let mut seen = SeenEvents::default();
        seen.record("ev001", "usgs");
        let n = seen.record("ev001", "emsc");
        assert_eq!(n, 2);
    }

    #[test]
    fn seen_events_duplicate_network_not_double_counted() {
        let mut seen = SeenEvents::default();
        seen.record("ev001", "usgs");
        let n = seen.record("ev001", "usgs");
        assert_eq!(n, 1);
    }

    // ── XML extraction ────────────────────────────────────────────────────────

    #[test]
    fn extract_xml_plain_tag() {
        let xml = "<title>Test event detected</title>";
        assert_eq!(extract_xml_text(xml, "title"), Some("Test event detected".into()));
    }

    #[test]
    fn extract_xml_cdata() {
        let xml = "<title><![CDATA[CTBTO seismic event]]></title>";
        assert_eq!(extract_xml_text(xml, "title"), Some("CTBTO seismic event".into()));
    }

    #[test]
    fn extract_xml_missing_tag_returns_none() {
        let xml = "<description>something</description>";
        assert_eq!(extract_xml_text(xml, "title"), None);
    }
}
