//! Queensland Fire Department (QFES) — Current Bushfire Incidents & Warnings.
//! Free, no API key. The official GeoJSON feed of current bushfire incidents and
//! public warnings the Queensland Fire Department is responding to, published on
//! the Queensland Government Open Data Portal (CC BY 4.0). Attribution:
//! "Queensland Fire Department".
//!
//! Reads the `bushfireAlert.json` product — a GeoJSON `FeatureCollection`, one
//! feature per active bushfire incident/warning. The operational signal is each
//! incident's official **warning level** — Emergency Warning / Watch and Act /
//! Advice / Information — the Australian Warning System's defined public
//! call-to-action scale (each level a named action), carried in the feature's
//! `WarningLevel` property. One normalized [`EventKind::Wildfire`] [`Event`] per
//! incident, plotted at its own point — the feed carries explicit `Latitude`/
//! `Longitude` properties on every feature (points and the occasional impact
//! polygon alike), so no geometry centroid is needed (a geometry fallback is
//! kept for robustness).
//!
//! This is the operational **emergency-warning** modality extended to a third
//! Australian state (after NSW `nsw_rfs` and WA `wa_dfes`) and **Queensland**
//! geography — the cyclone/monsoon-belt north-east, otherwise blank. It is not
//! duplicative of the global thermal-hotspot wildfire feeds (FIRMS/CWFIS/EONET),
//! which detect heat pixels or catalogue events but carry no human-facing
//! **warning level** a fire authority has declared for people on the ground.
//! Severity is driven by that warning level (Emergency Warning 0.95 → Watch and
//! Act 0.7 → Advice 0.45 → Information 0.25), so a bad-fire-day Emergency Warning
//! dominates the severity-sorted cap while routine Information notices still plot
//! at the lowest severity. An empty feed (no current incidents — the common
//! quiet/off-season state) yields zero events, not an error.
//!
//! ## Ingestion — Path A (prod fetches the live feed)
//! `publiccontent-gis-psba-qld-gov-au.s3.amazonaws.com/content/Feeds/BushfireCurrentIncidents/bushfireAlert.json`
//! is an auth-free public S3 object (no WAF), updated ~every 30 minutes. It was
//! **live-verified** (web fetch returned the real current FeatureCollection): the
//! `WarningLevel` / `WarningTitle` / `WarningArea` / `Latitude` / `Longitude` /
//! `UniqueID` / `EventType` / `CurrentStatus` / `ItemDateTimeLocal_ISO` schema
//! below is the real wire shape, with features dated the day of verification.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use serde_json::Value;
use std::time::Duration;

/// QFES Current Bushfire Incidents source.
#[derive(Default)]
pub struct QfesBushfire;

impl QfesBushfire {
    pub fn url(&self) -> &'static str {
        "https://publiccontent-gis-psba-qld-gov-au.s3.amazonaws.com/content/Feeds/BushfireCurrentIncidents/bushfireAlert.json"
    }
}

#[async_trait]
impl Source for QfesBushfire {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "qfes_bushfire",
            name: "QFES Bushfire Incidents",
            domain: EventKind::Wildfire,
            cadence: Duration::from_secs(1800),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        let body = crate::http::fetch_text(self.url()).await?;
        parse_qfes_bushfire(&body)
    }
}

/// Normalized 0–1 severity from the official Australian Warning System level the
/// Queensland Fire Department has declared for an incident — a defined
/// public-action scale, not a raw scalar. Matched on substrings so a decorated
/// level ("Advice Fire") still grades.
fn severity_for_level(level: &str) -> f64 {
    let l = level.trim().to_ascii_lowercase();
    if l.contains("emergency warning") {
        0.95
    } else if l.contains("watch and act") {
        0.7
    } else if l.contains("advice") {
        0.45
    } else if l.contains("information") {
        0.25
    } else {
        // An unrecognized / stand-down tier: still a real active incident, but the
        // lowest severity so declared warnings win the severity-sorted cap.
        0.2
    }
}

/// True when a warning level is an active public warning (leads the chip). The
/// routine "Information" notice is not, so the chip then leads with the place.
fn is_active_warning(level: &str) -> bool {
    let l = level.trim().to_ascii_lowercase();
    l.contains("emergency warning") || l.contains("watch and act") || l.contains("advice")
}

/// True when a candidate title, after trailing punctuation is stripped, is just a
/// bare AWS-level word with no location — the feed's "Information - " stub. Such a
/// title carries no more than the level (already in the chip), so we derive a
/// place-based title instead.
fn is_bare_level_title(t: &str) -> bool {
    matches!(
        t.trim().to_ascii_lowercase().as_str(),
        "information" | "advice" | "watch and act" | "emergency warning" | ""
    )
}

/// A trimmed non-empty string property, treating the feed's "Unknown" placeholder
/// (seen in `Location`) as absent.
fn str_prop(props: &Value, key: &str) -> Option<String> {
    props
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("unknown"))
        .map(str::to_string)
}

/// The best available place name for an incident: the warning area, else the
/// locality, else the free-text location, else the QFES jurisdiction/region.
fn place_of(props: &Value) -> Option<String> {
    str_prop(props, "WarningArea")
        .or_else(|| str_prop(props, "Locality"))
        .or_else(|| str_prop(props, "Location"))
        .or_else(|| str_prop(props, "Jurisdiction"))
}

/// Operator chip behind a QFES bushfire dot: the warning level (when it's an active
/// warning) + the place + the incident status, e.g.
/// "Watch and Act · Julago · Going" or, for a routine notice, "Information · Starcke".
/// `raw` is the feature's stored `{WarningLevel, WarningArea, Locality, Location,
/// Jurisdiction, CurrentStatus, WarningTitle}`. `None` only when nothing is present.
pub fn incident_chip(props: &Value) -> Option<String> {
    let level = str_prop(props, "WarningLevel");
    let place = place_of(props);
    let status = str_prop(props, "CurrentStatus");

    let mut parts: Vec<String> = Vec::new();
    if let Some(l) = &level {
        if is_active_warning(l) {
            parts.push(l.clone());
        }
    }
    if let Some(p) = &place {
        parts.push(p.clone());
    }
    if let Some(s) = &status {
        parts.push(s.clone());
    }
    if parts.is_empty() {
        // Nothing else to say -> surface the (routine) level, else the title.
        return level.or_else(|| str_prop(props, "WarningTitle"));
    }
    Some(parts.join(" · "))
}

/// A `[lon, lat]` position from a GeoJSON coordinate array (ignoring any trailing
/// elevation the feed appends to polygon vertices).
fn point_xy(coords: &Value) -> Option<(f64, f64)> {
    let a = coords.as_array()?;
    match (a.first()?.as_f64(), a.get(1)?.as_f64()) {
        (Some(lon), Some(lat)) if lon.is_finite() && lat.is_finite() => Some((lon, lat)),
        _ => None,
    }
}

/// Centroid (mean vertex) of every position found anywhere under a geometry — the
/// fallback when a feature lacks explicit `Latitude`/`Longitude` properties and is
/// a polygon rather than a bare point.
fn geometry_point(geom: &Value) -> Option<(f64, f64)> {
    fn walk(v: &Value, acc: &mut (f64, f64, u64)) {
        match v {
            Value::Array(a) => {
                if a.len() >= 2 && a[0].is_number() && a[1].is_number() {
                    if let (Some(lon), Some(lat)) = (a[0].as_f64(), a[1].as_f64()) {
                        if lon.is_finite() && lat.is_finite() {
                            acc.0 += lon;
                            acc.1 += lat;
                            acc.2 += 1;
                            return;
                        }
                    }
                }
                for e in a {
                    walk(e, acc);
                }
            }
            Value::Object(o) => {
                if let Some(c) = o.get("coordinates") {
                    walk(c, acc);
                }
                if let Some(g) = o.get("geometries") {
                    walk(g, acc);
                }
            }
            _ => {}
        }
    }
    // A bare point first (cheapest, exact); else the centroid of all vertices.
    if geom.get("type").and_then(Value::as_str) == Some("Point") {
        if let Some(p) = geom.get("coordinates").and_then(point_xy) {
            return Some(p);
        }
    }
    let mut acc = (0.0, 0.0, 0u64);
    walk(geom, &mut acc);
    (acc.2 > 0).then(|| (acc.0 / acc.2 as f64, acc.1 / acc.2 as f64))
}

/// A `(lon, lat)` for an incident: the explicit `Latitude`/`Longitude` properties
/// (present on every real feature, points and polygons alike), else the geometry.
fn incident_point(props: &Value, geom: &Value) -> Option<(f64, f64)> {
    let lat = props.get("Latitude").and_then(Value::as_f64);
    let lon = props.get("Longitude").and_then(Value::as_f64);
    if let (Some(lat), Some(lon)) = (lat, lon) {
        if lat.is_finite() && lon.is_finite() && (lat != 0.0 || lon != 0.0) {
            return Some((lon, lat));
        }
    }
    geometry_point(geom)
}

/// Parse an ISO-8601 timestamp (the feed's `+10:00`-offset local times) to UTC.
fn parse_time(props: &Value, key: &str) -> Option<DateTime<Utc>> {
    let s = props.get(key).and_then(Value::as_str)?.trim();
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// Pure parser: QFES `bushfireAlert.json` GeoJSON -> one [`Event`] per incident at
/// its point. Offline-tested. A body that isn't a GeoJSON FeatureCollection (e.g.
/// an HTML 403 page) is an error so the last-good layer takes over; a valid feed
/// with no features (no current incidents) is Ok/empty; a feature without a usable
/// point is skipped.
pub fn parse_qfes_bushfire(body: &str) -> anyhow::Result<Vec<Event>> {
    let root: Value = serde_json::from_str(body)
        .map_err(|e| anyhow::anyhow!("qfes_bushfire: not JSON: {e}"))?;
    if root.get("type").and_then(Value::as_str) != Some("FeatureCollection") {
        anyhow::bail!("qfes_bushfire: not a GeoJSON FeatureCollection");
    }
    let Some(features) = root.get("features").and_then(Value::as_array) else {
        anyhow::bail!("qfes_bushfire: FeatureCollection has no features array");
    };

    let mut out = Vec::new();
    for feat in features {
        let props = feat.get("properties").unwrap_or(&Value::Null);
        let geom = feat.get("geometry").unwrap_or(&Value::Null);

        let Some((lon, lat)) = incident_point(props, geom) else {
            continue;
        };
        let Some(geo) = Geo::new(lat, lon) else { continue };

        let level = str_prop(props, "WarningLevel").unwrap_or_default();
        let severity = severity_for_level(&level);

        let title = str_prop(props, "WarningTitle")
            .map(|t| t.trim_end_matches(['-', ' ']).trim().to_string())
            .filter(|t| !is_bare_level_title(t))
            .or_else(|| place_of(props).map(|p| format!("Bushfire — {p}")))
            .unwrap_or_else(|| "Queensland bushfire".to_string());

        let id = match str_prop(props, "UniqueID") {
            Some(u) => format!("qfes-{u}"),
            None => format!("qfes-{title}"),
        };

        // Prefer the incident time; fall back to publish time, then "now".
        let time = parse_time(props, "ItemDateTimeLocal_ISO")
            .or_else(|| parse_time(props, "PublishDateLocal_ISO"))
            .unwrap_or_else(Utc::now);

        out.push(Event {
            id,
            source_id: "qfes_bushfire".to_string(),
            kind: EventKind::Wildfire,
            title,
            time,
            geo: Some(geo),
            severity: Severity::new(severity),
            url: Some("https://www.fire.qld.gov.au/Current-Incidents".to_string()),
            raw: serde_json::json!({
                "WarningLevel": str_prop(props, "WarningLevel"),
                "WarningArea": str_prop(props, "WarningArea"),
                "Locality": str_prop(props, "Locality"),
                "Location": str_prop(props, "Location"),
                "Jurisdiction": str_prop(props, "Jurisdiction"),
                "CurrentStatus": str_prop(props, "CurrentStatus"),
                "WarningTitle": str_prop(props, "WarningTitle"),
            }),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The REAL upstream shape, verbatim from the live bushfireAlert.json (web fetch
    // live-verification): a GeoJSON FeatureCollection whose features carry explicit
    // Latitude/Longitude properties (points AND the occasional impact polygon), the
    // WarningLevel AWS tier, WarningTitle/WarningArea, EventType "Fire",
    // CurrentStatus, and the +10:00-offset ItemDateTimeLocal_ISO time.
    const REAL_FIXTURE: &str = r#"{
      "type": "FeatureCollection",
      "features": [
        {
          "type": "Feature",
          "geometry": { "type": "Polygon", "coordinates": [[[146.898,-19.371,0],[146.903,-19.366,0],[146.900,-19.361,0],[146.898,-19.371,0]]] },
          "properties": {
            "UniqueID": "WARN-54",
            "WarningLevel": "Advice",
            "WarningType_Level": "Advice Fire",
            "WarningTitle": "AVOID SMOKE - Julago (near Townsville) - fire as at 2:09pm Monday, 13 July 2026",
            "WarningArea": "Julago and surrounding areas",
            "Latitude": -19.353338174318008,
            "Longitude": 146.8820611359514,
            "EventType": "Fire",
            "CurrentStatus": null,
            "ItemDateTimeLocal_ISO": "2026-07-13T14:13:19+10:00",
            "PublishDateLocal_ISO": "2026-07-13T17:28:03+10:00"
          }
        },
        {
          "type": "Feature",
          "geometry": { "type": "Point", "coordinates": [142.618398, -11.358898] },
          "properties": {
            "UniqueID": "QF7-26-080611",
            "WarningLevel": "Information",
            "WarningTitle": "Information - ",
            "WarningArea": null,
            "Location": "Unknown",
            "Latitude": -11.358898,
            "Longitude": 142.618398,
            "EventType": "Fire",
            "CurrentStatus": "Going",
            "Jurisdiction": "7 Far Northern Region",
            "ItemDateTimeLocal_ISO": "2026-06-29T04:26:44+10:00",
            "PublishDateLocal_ISO": "2026-07-13T17:28:03+10:00"
          }
        }
      ]
    }"#;

    #[test]
    fn parses_real_feed_shape() {
        let ev = parse_qfes_bushfire(REAL_FIXTURE).unwrap();
        assert_eq!(ev.len(), 2);

        // Feature 1: an Advice-level polygon incident, geocoded from the explicit
        // Latitude/Longitude props (NOT the polygon centroid).
        let e0 = ev.iter().find(|e| e.id == "qfes-WARN-54").unwrap();
        assert_eq!(e0.source_id, "qfes_bushfire");
        assert_eq!(e0.kind, EventKind::Wildfire);
        assert!((e0.severity.value() - 0.45).abs() < 1e-9);
        let g = e0.geo.unwrap();
        assert!((g.lat + 19.353338174318008).abs() < 1e-9, "lat {}", g.lat);
        assert!((g.lon - 146.8820611359514).abs() < 1e-9, "lon {}", g.lon);
        assert_eq!(e0.time.format("%Y-%m-%d %H:%M").to_string(), "2026-07-13 04:13");
        // Active warning leads the chip with the level, then the area.
        assert_eq!(
            incident_chip(&e0.raw).as_deref(),
            Some("Advice · Julago and surrounding areas")
        );

        // Feature 2: a routine Information incident. WarningTitle is the bare
        // "Information - " stub -> rejected, so a place-derived title is used from
        // the QFES Jurisdiction (WarningArea/Locality are absent, Location is
        // "Unknown"). Chip leads with that place + status.
        let e1 = ev.iter().find(|e| e.id == "qfes-QF7-26-080611").unwrap();
        assert!((e1.severity.value() - 0.25).abs() < 1e-9);
        assert_eq!(e1.title, "Bushfire — 7 Far Northern Region");
        assert_eq!(
            incident_chip(&e1.raw).as_deref(),
            Some("7 Far Northern Region · Going")
        );
        let g1 = e1.geo.unwrap();
        assert!((g1.lat + 11.358898).abs() < 1e-9 && (g1.lon - 142.618398).abs() < 1e-9);
    }

    #[test]
    fn severity_ladder_over_real_aws_levels() {
        // The AWS levels QFES issues, exercising the ladder + the "level · place ·
        // status" chip. A decorated level still grades; place falls back sensibly.
        let feed = r#"{"type":"FeatureCollection","features":[
          {"type":"Feature","geometry":{"type":"Point","coordinates":[153.0,-27.5]},
           "properties":{"UniqueID":"a","WarningLevel":"Emergency Warning","WarningTitle":"Fire A",
             "WarningArea":"Beerburrum","Latitude":-27.5,"Longitude":153.0,"CurrentStatus":"Going"}},
          {"type":"Feature","geometry":{"type":"Point","coordinates":[145.7,-16.9]},
           "properties":{"UniqueID":"b","WarningLevel":"Watch and Act","WarningTitle":"Fire B",
             "Locality":"Cairns","Latitude":-16.9,"Longitude":145.7}},
          {"type":"Feature","geometry":{"type":"Point","coordinates":[151.0,-24.0]},
           "properties":{"UniqueID":"c","WarningLevel":"Advice Fire","WarningTitle":"Fire C",
             "WarningArea":"Gladstone","Latitude":-24.0,"Longitude":151.0}}
        ]}"#;
        let ev = parse_qfes_bushfire(feed).unwrap();
        assert_eq!(ev.len(), 3);
        let by = |id: &str| ev.iter().find(|e| e.id == format!("qfes-{id}")).unwrap();
        assert!((by("a").severity.value() - 0.95).abs() < 1e-9);
        assert!((by("b").severity.value() - 0.7).abs() < 1e-9);
        assert!((by("c").severity.value() - 0.45).abs() < 1e-9);
        assert_eq!(
            incident_chip(&by("a").raw).as_deref(),
            Some("Emergency Warning · Beerburrum · Going")
        );
        // "Advice Fire" is still an Advice-level active warning (substring match).
        assert_eq!(
            incident_chip(&by("c").raw).as_deref(),
            Some("Advice Fire · Gladstone")
        );
    }

    #[test]
    fn empty_feed_is_ok_not_error() {
        // A valid feed with no current incidents (quiet/off-season) -> zero events.
        let ev = parse_qfes_bushfire(r#"{"type":"FeatureCollection","features":[]}"#).unwrap();
        assert!(ev.is_empty());
    }

    #[test]
    fn errors_on_bad_input() {
        // An HTML 403 page (not GeoJSON) is an error so the last-good layer takes over.
        assert!(parse_qfes_bushfire("<html><body>403 Forbidden</body></html>").is_err());
        assert!(parse_qfes_bushfire(r#"{"type":"Something","features":[]}"#).is_err());
        assert!(parse_qfes_bushfire("not json at all").is_err());
    }

    #[test]
    fn feature_without_point_is_skipped_not_fatal() {
        // A feature with no coordinates and no Lat/Lon props is dropped; the
        // geocoded one still plots.
        let feed = r#"{"type":"FeatureCollection","features":[
          {"type":"Feature","geometry":null,
           "properties":{"UniqueID":"x","WarningLevel":"Advice","WarningTitle":"No point"}},
          {"type":"Feature","geometry":{"type":"Point","coordinates":[153.0,-27.5]},
           "properties":{"UniqueID":"y","WarningLevel":"Advice","WarningTitle":"Has point",
             "Latitude":-27.5,"Longitude":153.0}}
        ]}"#;
        let ev = parse_qfes_bushfire(feed).unwrap();
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].id, "qfes-y");
    }

    #[test]
    fn polygon_without_latlon_props_falls_back_to_centroid() {
        // If the explicit Lat/Lon props are absent, a polygon geocodes to its
        // vertex centroid rather than being dropped.
        let feed = r#"{"type":"FeatureCollection","features":[
          {"type":"Feature",
           "geometry":{"type":"Polygon","coordinates":[[[150.0,-25.0,0],[152.0,-25.0,0],[152.0,-27.0,0],[150.0,-27.0,0],[150.0,-25.0,0]]]},
           "properties":{"UniqueID":"p","WarningLevel":"Watch and Act","WarningTitle":"Poly fire","WarningArea":"Somewhere"}}
        ]}"#;
        let ev = parse_qfes_bushfire(feed).unwrap();
        assert_eq!(ev.len(), 1);
        let g = ev[0].geo.unwrap();
        // Centroid of the 5 listed vertices (the closing vertex repeats the first).
        assert!((g.lon - 150.8).abs() < 1e-6, "lon {}", g.lon);
        assert!((g.lat + 25.8).abs() < 1e-6, "lat {}", g.lat);
    }
}
