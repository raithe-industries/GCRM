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
        .min_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap())
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
                alerts.retain(|a| {
                    // Aftershock sequence detected → natural earthquake, not a test.
                    if a.aftershock_checked && a.aftershock_count > 0 { return false; }
                    // Single-network anomaly that never corroborated → expire after 24h.
                    if a.level == SeismicAlertLevel::Anomaly
                        && (now - a.detected_at) > chrono::Duration::hours(24) {
                        return false;
                    }
                    // Escalated alerts persist longer but still expire after 7 days.
                    if (now - a.detected_at) > chrono::Duration::days(7) { return false; }
                    true
                });
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

            // Read current news escalation score for this actor (I-18: tightened)
            let news_score = self.news_escalation_score(site.actor).await;

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
                if a.level == SeismicAlertLevel::Anomaly {
                    a.level         = SeismicAlertLevel::MultiNetwork;
                    a.corroboration = networks;
                    if !a.networks.contains(&source_id.to_string()) {
                        a.networks.push(source_id.to_string());
                    }
                    a.confidence  = a.compute_confidence();
                    a.description = Self::build_description(a);
                    info!(
                        "SEISMIC ANOMALY upgraded: multi-network ({}) M{:.1} near {}",
                        networks, mag, site.name
                    );
                }
            }
        }
    }

    /// Compute a news escalation score for a given actor using the article store.
        async fn news_escalation_score(&self, actor: &str) -> f64 {
        let store = self.state.article_store.lock().await;
        let now   = Utc::now();
        let relevant: Vec<_> = store.articles.iter()
            .filter(|a| {
                let age_h = (now - chrono::DateTime::parse_from_rfc3339(&a.published_at)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or(now))
                    .num_hours() as f64;
                age_h < 72.0
                    && a.domain_tags.contains(&"nuclear_posture".to_string())
                    && a.body.to_lowercase().contains(&actor.replace('_', " "))
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

    let aftershock_count = match client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => {
            match resp.json::<FdsnResponse>().await {
                Ok(data) => data.features.iter()
                    .filter(|f| f.id != event_id)
                    .count(),
                Err(_) => return,
            }
        }
        _ => return,
    };

    let mut alerts = state.nuclear_alerts.lock().await;
    if aftershock_count > 0 {
        // Aftershock sequence present — natural tectonic earthquake, not an
        // explosion. Clear the alert outright so it stops driving the banner.
        let before = alerts.len();
        alerts.retain(|a| a.id != event_id);
        if alerts.len() < before {
            info!(
                "Seismic event {}: {} aftershock(s) detected — tectonic source, alert cleared",
                event_id, aftershock_count
            );
        }
        return;
    }

    if let Some(alert) = alerts.iter_mut().find(|a| a.id == event_id) {
        alert.aftershock_checked  = true;
        alert.aftershock_count    = 0;
        alert.aftershock_check_at = Some(Utc::now());

        // No aftershock sequence — consistent with explosion source
        alert.level       = SeismicAlertLevel::AftershockAbsent;
        alert.confidence  = alert.compute_confidence();
        alert.description = SeismicMonitor::build_description_static(alert);
        info!(
            "SEISMIC ANOMALY — no aftershock sequence at 2h: {} (confidence {:.0}%)",
            alert.id, alert.confidence * 100.0
        );
    }
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

                    // Correlate with the most recent alert from the last 7 days
                    // rather than the oldest in the list. A statement with no recent
                    // alert to attach to is logged but escalates nothing.
                    let now = Utc::now();
                    let mut alerts = self.state.nuclear_alerts.lock().await;
                    let recent = alerts.iter_mut()
                        .filter(|a| (now - a.detected_at) < chrono::Duration::days(7))
                        .max_by_key(|a| a.detected_at);
                    if let Some(alert) = recent {
                        alert.ctbto_statement = true;
                        alert.ctbto_text      = Some(title.clone());
                        alert.level           = SeismicAlertLevel::CtbtoStatement;
                        alert.confidence      = alert.compute_confidence();
                        info!("CTBTO statement correlated with seismic alert {}", alert.id);
                    } else {
                        info!("CTBTO statement '{title}' — no recent seismic alert to correlate");
                    }
                }
            }
        }
    }
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
