//! Canadian Wildland Fire Information System (CWFIS) — satellite-detected active-fire
//! hotspots from Natural Resources Canada. Free, no API key.
//!
//! Reads the CWFIS GeoServer WFS `public:hotspots_last24hrs` layer as GeoJSON
//! (<https://cwfis.cfs.nrcan.gc.ca/geoserver>) into normalized [`EventKind::Wildfire`]
//! [`Event`]s. The continental layer is bounded to a [`Self::bbox`] (Canada by default)
//! so the map fills with Canadian fire activity rather than the whole hemisphere.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use std::time::Duration;

/// CWFIS active-fire hotspot source. `bbox` is `(min_lat, min_lon, max_lat, max_lon)`;
/// `None` returns the full continental layer.
pub struct Cwfis {
    pub bbox: Option<(f64, f64, f64, f64)>,
}

impl Default for Cwfis {
    /// Canada (incl. the southern border): lat 41.7..84, lon -141..-52.
    fn default() -> Self {
        Self { bbox: Some((41.7, -141.0, 84.0, -52.0)) }
    }
}

impl Cwfis {
    pub fn url(&self) -> String {
        let base = "https://cwfis.cfs.nrcan.gc.ca/geoserver/public/ows?service=WFS&version=2.0.0\
            &request=GetFeature&typeNames=public:hotspots_last24hrs&outputFormat=application/json\
            &srsName=EPSG:4326&count=2000";
        match self.bbox {
            // Filter on the layer's own numeric lat/lon columns — robust against the
            // WFS 2.0 / EPSG:4326 axis-order ambiguity that a `bbox=` param invites.
            Some((min_lat, min_lon, max_lat, max_lon)) => {
                let cql = format!(
                    "lat BETWEEN {min_lat} AND {max_lat} AND lon BETWEEN {min_lon} AND {max_lon}"
                );
                format!("{base}&CQL_FILTER={}", urlencode(&cql))
            }
            None => base.to_string(),
        }
    }
}

/// Minimal percent-encoding for the CQL filter (spaces, commas, comparison glyphs).
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[async_trait]
impl Source for Cwfis {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "cwfis",
            name: "CWFIS Wildfire Hotspots (Canada)",
            domain: EventKind::Wildfire,
            cadence: Duration::from_secs(1800),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        let body = crate::http::fetch_text(&self.url()).await?;
        parse_cwfis(&body)
    }
}

/// Pure parser: CWFIS GeoServer GeoJSON -> events. Unit-tested offline.
///
/// Severity scales with Fire Radiative Power (`frp`, MW): a ~60 MW hotspot saturates
/// the marker. Coordinates come from the `[lon, lat]` geometry (validated by
/// [`Geo::new`], which also rejects any feature that slipped through in a non-WGS84 CRS).
pub fn parse_cwfis(json: &str) -> anyhow::Result<Vec<Event>> {
    // GeoServer returns an XML `ExceptionReport` (not JSON) on a bad request or hiccup,
    // which is not parseable as JSON — and a healthy-but-quiet response may parse yet
    // carry no `features`. Treat either as "no hotspots" so a transient blip never
    // surfaces as a hard feed error on the map (matching the `eonet` tolerance).
    let Ok(root) = serde_json::from_str::<serde_json::Value>(json) else {
        return Ok(Vec::new());
    };
    let Some(features) = root.get("features").and_then(|f| f.as_array()) else {
        return Ok(Vec::new());
    };

    let mut out = Vec::with_capacity(features.len());
    for f in features {
        let props = f.get("properties").cloned().unwrap_or(serde_json::Value::Null);

        let geo = f
            .get("geometry")
            .filter(|g| g.get("type").and_then(|t| t.as_str()) == Some("Point"))
            .and_then(|g| g.get("coordinates"))
            .and_then(|c| c.as_array())
            .filter(|c| c.len() >= 2)
            .and_then(|c| match (c[0].as_f64(), c[1].as_f64()) {
                (Some(lon), Some(lat)) => Geo::new(lat, lon),
                _ => None,
            });
        // A hotspot with no plottable coordinate is not a map signal — skip it.
        let Some(g) = geo else { continue };

        let frp = props.get("frp").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let agency = props.get("agency").and_then(|v| v.as_str()).unwrap_or("");
        // FRP rides in the map popup's value chip (see osint::feed_detail); keep the
        // title to the place so the card doesn't show the wattage twice.
        let title = if agency.is_empty() {
            "Wildfire hotspot".to_string()
        } else {
            format!("Wildfire hotspot — {agency}")
        };

        let time = props
            .get("rep_date")
            .and_then(|d| d.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        // Stable-enough id from position + report time (GeoServer fids are not stable).
        let id = format!("cwfis-{:.4}-{:.4}-{}", g.lon, g.lat, time.timestamp());

        out.push(Event {
            id,
            source_id: "cwfis".to_string(),
            kind: EventKind::Wildfire,
            title,
            time,
            geo: Some(g),
            severity: Severity::new(frp / 60.0),
            url: Some("https://cwfis.cfs.nrcan.gc.ca/interactive-map".to_string()),
            raw: f.clone(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
      "type": "FeatureCollection",
      "features": [
        {"type":"Feature","id":"hotspots.1",
         "geometry":{"type":"Point","coordinates":[-120.5,55.2]},
         "properties":{"frp":60.0,"agency":"BC","fuel":"C2","rep_date":"2026-06-13T18:41:00Z"}},
        {"type":"Feature","id":"hotspots.2",
         "geometry":{"type":"Point","coordinates":[-95.0,49.5]},
         "properties":{"frp":15.0,"agency":"MB","rep_date":"2026-06-13T18:41:00Z"}},
        {"type":"Feature","id":"hotspots.3",
         "geometry":{"type":"Point","coordinates":[-999.0,49.5]},
         "properties":{"frp":5.0,"agency":"XX"}}
      ]
    }"#;

    #[test]
    fn parses_fixture() {
        let ev = parse_cwfis(FIXTURE).unwrap();
        // The out-of-range third feature has no valid geo -> dropped.
        assert_eq!(ev.len(), 2);

        assert_eq!(ev[0].kind, EventKind::Wildfire);
        assert_eq!(ev[0].title, "Wildfire hotspot — BC");
        let g = ev[0].geo.unwrap();
        assert!((g.lat - 55.2).abs() < 1e-9 && (g.lon + 120.5).abs() < 1e-9);
        // frp 60 -> severity saturates at 1.0.
        assert!((ev[0].severity.value() - 1.0).abs() < 1e-9);
        // frp 15 -> 0.25.
        assert!((ev[1].severity.value() - 0.25).abs() < 1e-9);
    }

    #[test]
    fn tolerates_geoserver_exception() {
        // The real GeoServer failure mode is an XML ExceptionReport — not JSON. It must
        // degrade to "no hotspots", not a hard error that lands in the map's errors[].
        let xml = r#"<?xml version="1.0"?><ows:ExceptionReport><ows:Exception exceptionCode="InvalidParameterValue"/></ows:ExceptionReport>"#;
        assert_eq!(parse_cwfis(xml).unwrap().len(), 0);
        // A valid-JSON-but-no-features response is also tolerated.
        assert_eq!(parse_cwfis(r#"{"type":"x"}"#).unwrap().len(), 0);
    }

    #[test]
    fn url_bounds_to_canada_by_default() {
        let u = Cwfis::default().url();
        assert!(u.contains("hotspots_last24hrs"));
        assert!(u.contains("CQL_FILTER="));
        // Unbounded variant carries no filter.
        assert!(!Cwfis { bbox: None }.url().contains("CQL_FILTER"));
    }
}
