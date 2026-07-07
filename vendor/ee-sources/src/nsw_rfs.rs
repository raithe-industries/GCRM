//! NSW Rural Fire Service — Major Incidents. Free, no API key. The official
//! GeoJSON feed of current MAJOR fire/emergency incidents the NSW RFS is
//! responding to, published by a state government emergency service.
//! Attribution: "NSW Rural Fire Service".
//!
//! Reads the RFS `majorIncidents.json` product — a GeoJSON `FeatureCollection`,
//! one feature per active major incident. The operational signal is each
//! incident's official **Alert Level** — Emergency Warning / Watch and Act /
//! Advice / Not Applicable — a defined public-warning scale (each level a named
//! call to action), carried in the feature's `category` and inside the
//! `description` HTML blob ("ALERT LEVEL: … <br />LOCATION: … <br />STATUS: …
//! <br />TYPE: … <br />SIZE: … ha <br />…"). One normalized [`EventKind::Wildfire`]
//! [`Event`] per incident, plotted at its representative point (RFS ships a point
//! plus, for larger fires, a `GeometryCollection` of a representative point and
//! the fire-extent polygons — we take the representative point, falling back to
//! the polygon centroid).
//!
//! This is the operational **emergency-warning** modality and **Australian**
//! geography the global thermal-hotspot wildfire feeds don't carry: FIRMS/CWFIS
//! detect heat pixels (satellite thermal), EONET catalogues events — none carry
//! the human-facing alert level a fire authority has declared for people on the
//! ground. Severity is driven by that alert level (Emergency Warning 0.95 →
//! Watch and Act 0.7 → Advice 0.45 → Not Applicable/other 0.25), so a bad-fire-day
//! Emergency Warning dominates the severity-sorted cap. An empty feed (no current
//! major incidents — the common quiet/off-season state) yields zero events, not
//! an error.

use async_trait::async_trait;
use chrono::Utc;
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use serde_json::Value;
use std::time::Duration;

/// NSW RFS Major Incidents source.
#[derive(Default)]
pub struct NswRfs;

impl NswRfs {
    pub fn url(&self) -> &'static str {
        "https://www.rfs.nsw.gov.au/feeds/majorIncidents.json"
    }
}

#[async_trait]
impl Source for NswRfs {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "nsw_rfs",
            name: "NSW RFS Major Incidents",
            domain: EventKind::Wildfire,
            cadence: Duration::from_secs(1800),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        let body = crate::http::fetch_text(self.url()).await?;
        parse_nsw_rfs(&body)
    }
}

/// Normalized 0–1 severity from the official NSW RFS Alert Level. The alert level
/// is the warning tier a fire authority has declared for people on the ground —
/// not a raw scalar — so it maps directly to operator severity.
fn severity_for_alert(level: &str) -> f64 {
    let l = level.trim().to_ascii_lowercase();
    if l.contains("emergency warning") {
        0.95
    } else if l.contains("watch and act") {
        0.7
    } else if l.contains("advice") {
        0.45
    } else {
        // "Not Applicable" (a major incident with no active public warning) or an
        // unrecognized tier: still a real active incident, but the lowest severity
        // so warnings win the cap.
        0.25
    }
}

/// True when an alert level is an active public warning (leads the chip); a blank
/// or "Not Applicable" level is not, so the chip then leads with the incident type.
fn is_active_warning(level: &str) -> bool {
    let l = level.trim();
    !l.is_empty() && !l.eq_ignore_ascii_case("not applicable")
}

/// Pull a `"<LABEL>: value <br />"` field out of the RFS description HTML blob.
/// Case-sensitive on the uppercase labels the feed uses; value runs to the next
/// `<br`. Returns the trimmed value, or `None` if absent/empty.
fn desc_field(desc: &str, label: &str) -> Option<String> {
    let key = format!("{label}:");
    let i = desc.find(&key)? + key.len();
    let rest = desc[i..].trim_start();
    let end = rest.find("<br").unwrap_or(rest.len());
    let val = rest[..end].trim();
    (!val.is_empty()).then(|| val.to_string())
}

/// The incident's alert level: prefer the description's "ALERT LEVEL:" (always
/// present, even when the top-level `category` is missing), fall back to `category`.
fn alert_level(props: &Value) -> Option<String> {
    let desc = props.get("description").and_then(Value::as_str).unwrap_or("");
    desc_field(desc, "ALERT LEVEL").or_else(|| {
        props
            .get("category")
            .and_then(Value::as_str)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    })
}

/// Operator chip for an incident: the alert level (when it's an active warning) +
/// the incident type + fire size, e.g. "Watch and Act · Bush Fire · 315512 ha" or,
/// for a Not-Applicable incident, "Bush Fire · 10 ha". `raw` is the feature's
/// `properties`. `None` only when nothing meaningful is present.
pub fn incident_chip(props: &Value) -> Option<String> {
    let desc = props.get("description").and_then(Value::as_str).unwrap_or("");
    let alert = alert_level(props);
    let typ = desc_field(desc, "TYPE");
    let size = desc_field(desc, "SIZE").filter(|s| s != "0 ha");

    let mut parts: Vec<String> = Vec::new();
    if let Some(a) = &alert {
        if is_active_warning(a) {
            parts.push(a.clone());
        }
    }
    if let Some(t) = &typ {
        parts.push(t.clone());
    }
    if let Some(s) = &size {
        parts.push(s.clone());
    }
    // Nothing else to say -> at least surface the (non-warning) alert level if any.
    if parts.is_empty() {
        return alert.filter(|a| !a.is_empty());
    }
    Some(parts.join(" · "))
}

/// A `[lon, lat]` position from a GeoJSON coordinate array.
fn point_xy(coords: &Value) -> Option<(f64, f64)> {
    let a = coords.as_array()?;
    match (a.first()?.as_f64(), a.get(1)?.as_f64()) {
        (Some(lon), Some(lat)) if lon.is_finite() && lat.is_finite() => Some((lon, lat)),
        _ => None,
    }
}

/// The first `Point` anywhere in a geometry (RFS puts a representative location
/// point first in each incident's `GeometryCollection`).
fn find_point(geom: &Value) -> Option<(f64, f64)> {
    match geom.get("type").and_then(Value::as_str)? {
        "Point" => point_xy(geom.get("coordinates")?),
        "GeometryCollection" => geom
            .get("geometries")?
            .as_array()?
            .iter()
            .find_map(find_point),
        _ => None,
    }
}

/// Average all positions found anywhere under `v` (recursing arrays / a geometry's
/// `coordinates` + `geometries`) into `(sum_lon, sum_lat, count)`. The centroid
/// fallback when a geometry carries no representative `Point`.
fn collect_positions(v: &Value, acc: &mut (f64, f64, u64)) {
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
                collect_positions(e, acc);
            }
        }
        Value::Object(o) => {
            if let Some(c) = o.get("coordinates") {
                collect_positions(c, acc);
            }
            if let Some(g) = o.get("geometries") {
                collect_positions(g, acc);
            }
        }
        _ => {}
    }
}

/// A representative `(lon, lat)` for an incident geometry: the RFS point if any,
/// else the centroid of all its coordinates.
fn representative_point(geom: &Value) -> Option<(f64, f64)> {
    if let Some(p) = find_point(geom) {
        return Some(p);
    }
    let mut acc = (0.0, 0.0, 0u64);
    collect_positions(geom, &mut acc);
    (acc.2 > 0).then(|| (acc.0 / acc.2 as f64, acc.1 / acc.2 as f64))
}

/// Pure parser: NSW RFS `majorIncidents.json` GeoJSON -> events. Unit-tested
/// offline. A missing `features` array is malformed (error); an empty feature
/// list (no current major incidents) is Ok/empty; features without usable
/// geometry are skipped.
pub fn parse_nsw_rfs(json: &str) -> anyhow::Result<Vec<Event>> {
    let root: Value = serde_json::from_str(json)?;
    let features = root
        .get("features")
        .and_then(|f| f.as_array())
        .ok_or_else(|| anyhow::anyhow!("nsw_rfs: missing 'features' array"))?;

    let mut out = Vec::with_capacity(features.len());
    for f in features {
        let Some(geom) = f.get("geometry") else { continue };
        let Some((lon, lat)) = representative_point(geom) else { continue };
        let Some(geo) = Geo::new(lat, lon) else { continue };

        let props = f.get("properties").cloned().unwrap_or(Value::Null);

        let alert = alert_level(&props).unwrap_or_default();
        let severity = severity_for_alert(&alert);

        let title = props
            .get("title")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| {
                props
                    .get("description")
                    .and_then(Value::as_str)
                    .and_then(|d| desc_field(d, "LOCATION"))
            })
            .unwrap_or_else(|| "NSW RFS incident".to_string());

        let guid = props
            .get("guid")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let id = match guid {
            Some(g) => format!("nsw-rfs-{g}"),
            None => format!("nsw-rfs-{title}"),
        };

        let url = props
            .get("link")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| s.starts_with("http"))
            .map(str::to_string)
            .or_else(|| guid.filter(|g| g.starts_with("http")).map(str::to_string))
            .unwrap_or_else(|| {
                "https://www.rfs.nsw.gov.au/fire-information/fires-near-me".to_string()
            });

        out.push(Event {
            id,
            source_id: "nsw_rfs".to_string(),
            kind: EventKind::Wildfire,
            title,
            // A live current-incidents feed: the reading is "as observed this fetch"
            // (matches geonet_volcano / the radiation networks). pubDate is local
            // Sydney time and not needed for the operational read.
            time: Utc::now(),
            geo: Some(geo),
            severity: Severity::new(severity),
            url: Some(url),
            raw: props,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The REAL upstream shape, verbatim from the NSW RFS majorIncidents feed
    // (captured in exxamalte/python-aio-geojson-nsw-rfs-incidents tests/fixtures/
    // incidents-1.json): three simple Point incidents plus "Badja Forest Rd,
    // Countegany" — a real Advice bush fire whose geometry is a GeometryCollection
    // of a representative Point followed by the fire-extent polygons.
    const REAL_FIXTURE: &str = r#"{
      "type": "FeatureCollection",
      "features": [
        {"type":"Feature","geometry":{"type":"Point","coordinates":[149.1234,-37.2345]},
         "properties":{"title":"Title 1","category":"Category 1","guid":"1234",
           "pubDate":"21/09/2018 6:30:00 AM",
           "description":"ALERT LEVEL: Alert Level 1 <br />LOCATION: Location 1 <br />COUNCIL AREA: Council 1 <br />STATUS: Status 1 <br />TYPE: Type 1 <br />FIRE: Yes <br />SIZE: 10 ha <br />RESPONSIBLE AGENCY: Agency 1 <br />UPDATED: 21 Sep 2018 16:45"}},
        {"type":"Feature","geometry":{"type":"Point","coordinates":[149.1234,-37.2345]},
         "properties":{"title":"Title 2","category":"Category 2","guid":"2345",
           "pubDate":"21/09/2018 6:35:00 AM",
           "description":"ALERT LEVEL: Alert Level 2 <br />LOCATION: Location 2 <br />COUNCIL AREA: Council 2 <br />STATUS: Status 2 <br />TYPE: Type 2 <br />FIRE: No <br />SIZE: 20 ha <br />RESPONSIBLE AGENCY: Agency 2 <br />UPDATED: 21 Sep 2018 16:50"}},
        {"type":"Feature","geometry":{"type":"Point","coordinates":[149.1234,-37.2345]},
         "properties":{"title":"Title 3","guid":"3456",
           "pubDate":"21/09/2018 6:40:00 AM",
           "description":"ALERT LEVEL: Alert Level 3 <br />LOCATION: Location 3 <br />COUNCIL AREA: Council 3 <br />STATUS: Status 3 <br />TYPE: Type 3 <br />FIRE: Yes <br />SIZE: 20 ha <br />RESPONSIBLE AGENCY: Agency 3 <br />UPDATED: 21 Sep 2018 16:55"}},
        {"type":"Feature",
         "geometry":{"type":"GeometryCollection","geometries":[
           {"type":"Point","coordinates":[149.92444759700004,-36.25492599599994]},
           {"type":"GeometryCollection","geometries":[
             {"type":"Polygon","coordinates":[[[149.485665224,-36.1511335279999],[149.483464222,-36.151490447],[149.483821141,-36.1508360949999],[149.485903169,-36.1503602029999],[149.485665224,-36.1511335279999]]]}
           ]}
         ]},
         "properties":{"title":"Badja Forest Rd, Countegany",
           "link":"http://www.rfs.nsw.gov.au/fire-information/fires-near-me",
           "category":"Advice",
           "guid":"https://incidents.rfs.nsw.gov.au/api/v1/incidents/366937",
           "guid_isPermaLink":"true","pubDate":"18/02/2018 12:11:00 AM",
           "description":"ALERT LEVEL: Advice <br />LOCATION: Badja Forest Rd, Countegany, NSW 2630 <br />COUNCIL AREA: Eurobodalla <br />STATUS: Under control <br />TYPE: Bush Fire <br />FIRE: Yes <br />SIZE: 315512 ha <br />RESPONSIBLE AGENCY: Rural Fire Service <br />UPDATED: 18 Feb 2018 11:11"}}
      ]
    }"#;

    #[test]
    fn parses_real_feed_shape() {
        let ev = parse_nsw_rfs(REAL_FIXTURE).unwrap();
        assert_eq!(ev.len(), 4);

        // The Badja incident: GeometryCollection resolves to its representative
        // Point (not the polygon), Advice -> 0.45, chip leads with the warning.
        let badja = ev.iter().find(|e| e.title.starts_with("Badja")).unwrap();
        assert_eq!(badja.kind, EventKind::Wildfire);
        assert_eq!(badja.source_id, "nsw_rfs");
        assert_eq!(
            badja.id,
            "nsw-rfs-https://incidents.rfs.nsw.gov.au/api/v1/incidents/366937"
        );
        let g = badja.geo.unwrap();
        assert!((g.lat + 36.25492599599994).abs() < 1e-9, "lat {}", g.lat);
        assert!((g.lon - 149.92444759700004).abs() < 1e-9, "lon {}", g.lon);
        assert!((badja.severity.value() - 0.45).abs() < 1e-9);
        assert_eq!(
            incident_chip(&badja.raw).as_deref(),
            Some("Advice · Bush Fire · 315512 ha")
        );
        assert_eq!(
            badja.url.as_deref(),
            Some("http://www.rfs.nsw.gov.au/fire-information/fires-near-me")
        );

        // Title 3 has no top-level `category`; the alert level still parses from
        // the description, and its guid drives the id.
        let t3 = ev.iter().find(|e| e.title == "Title 3").unwrap();
        assert_eq!(t3.id, "nsw-rfs-3456");
    }

    #[test]
    fn severity_and_chip_ladder() {
        // Realistic alert tiers exercise the ladder + the chip's warning-lead logic.
        let json = r#"{"type":"FeatureCollection","features":[
          {"type":"Feature","geometry":{"type":"Point","coordinates":[150.0,-33.0]},
           "properties":{"title":"A","category":"Emergency Warning","guid":"a",
             "description":"ALERT LEVEL: Emergency Warning <br />TYPE: Bush Fire <br />SIZE: 5000 ha <br />"}},
          {"type":"Feature","geometry":{"type":"Point","coordinates":[151.0,-32.0]},
           "properties":{"title":"B","category":"Watch and Act","guid":"b",
             "description":"ALERT LEVEL: Watch and Act <br />TYPE: Grass Fire <br />SIZE: 200 ha <br />"}},
          {"type":"Feature","geometry":{"type":"Point","coordinates":[152.0,-31.0]},
           "properties":{"title":"C","category":"Advice","guid":"c",
             "description":"ALERT LEVEL: Advice <br />TYPE: Bush Fire <br />SIZE: 2 ha <br />"}},
          {"type":"Feature","geometry":{"type":"Point","coordinates":[153.0,-30.0]},
           "properties":{"title":"D","category":"Not Applicable","guid":"d",
             "description":"ALERT LEVEL: Not Applicable <br />TYPE: Hazard Reduction <br />SIZE: 0 ha <br />"}}
        ]}"#;
        let ev = parse_nsw_rfs(json).unwrap();
        assert_eq!(ev.len(), 4);
        let sev = |t: &str| ev.iter().find(|e| e.title == t).unwrap().severity.value();
        assert!((sev("A") - 0.95).abs() < 1e-9);
        assert!((sev("B") - 0.7).abs() < 1e-9);
        assert!((sev("C") - 0.45).abs() < 1e-9);
        assert!((sev("D") - 0.25).abs() < 1e-9);

        let chip = |t: &str| {
            incident_chip(&ev.iter().find(|e| e.title == t).unwrap().raw).unwrap()
        };
        // Warnings lead with the alert level; the Not-Applicable incident leads with
        // its type and drops the meaningless "0 ha".
        assert_eq!(chip("A"), "Emergency Warning · Bush Fire · 5000 ha");
        assert_eq!(chip("C"), "Advice · Bush Fire · 2 ha");
        assert_eq!(chip("D"), "Hazard Reduction");
    }

    #[test]
    fn geometrycollection_centroid_when_no_point() {
        // A geometry with no representative Point falls back to the coordinate centroid.
        let json = r#"{"type":"FeatureCollection","features":[
          {"type":"Feature",
           "geometry":{"type":"Polygon","coordinates":[[[150.0,-33.0],[152.0,-33.0],[152.0,-35.0],[150.0,-35.0],[150.0,-33.0]]]},
           "properties":{"title":"Poly","category":"Advice","guid":"p",
             "description":"ALERT LEVEL: Advice <br />TYPE: Bush Fire <br />"}}
        ]}"#;
        let ev = parse_nsw_rfs(json).unwrap();
        assert_eq!(ev.len(), 1);
        let g = ev[0].geo.unwrap();
        // Mean of the 5 ring vertices (the closing vertex repeats the first).
        assert!((g.lon - 150.8).abs() < 1e-9, "lon {}", g.lon);
        assert!((g.lat + 33.8).abs() < 1e-9, "lat {}", g.lat);
    }

    #[test]
    fn empty_feed_is_ok_not_error() {
        // No current major incidents (quiet / off-season) -> zero events, not a failure.
        let ev = parse_nsw_rfs(r#"{"type":"FeatureCollection","features":[]}"#).unwrap();
        assert!(ev.is_empty());
    }

    #[test]
    fn errors_on_bad_input() {
        // Missing the features array is malformed.
        assert!(parse_nsw_rfs(r#"{"type":"FeatureCollection"}"#).is_err());
        // Not JSON at all (e.g. an HTML error page).
        assert!(parse_nsw_rfs("<html>403 Forbidden</html>").is_err());
    }
}
