//! CWFIS National Active Fires — agency-reported wildfire ground-state across Canada,
//! from Natural Resources Canada's Canadian Wildland Fire Information System (Canadian
//! Forest Service), aggregating provincial/territorial + Parks Canada reports via CIFFC.
//! Free, no API key.
//!
//! This is the *incident* layer — named fires with a containment stage (out-of-control /
//! being-held / under-control), cumulative size in hectares, and cause — which is
//! orthogonal to the raw satellite thermal HOTSPOTS the [`crate::cwfis`] and
//! [`crate::firms`] feeds plot. Hotspots say "a sensor saw heat here"; this says "a crew
//! is fighting a 1,000 ha out-of-control burn". Normalized to [`EventKind::Wildfire`].

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use std::time::Duration;

/// CWFIS national active-fires source (NRCan / Canadian Forest Service + CIFFC).
#[derive(Default)]
pub struct CwfisActiveFires;

impl CwfisActiveFires {
    pub fn url(&self) -> &'static str {
        // The CQL `now()` window is MANDATORY: unfiltered the layer returns the full
        // ~178k-row multi-year archive; this narrows to the ~dozens currently active.
        // NB: the host really is spelled `cwfif` (not `cwfis`) — do not "correct" it.
        "https://geoserver.cwfif.nrcan.gc.ca/geoserver/wfs?service=WFS&version=2.0.1\
         &request=GetFeature&outputFormat=application/json&typeName=public:cwfif_national_activefires\
         &CQL_FILTER=now()%3E=record_start%20AND%20now()%3C=record_end"
    }
}

#[async_trait]
impl Source for CwfisActiveFires {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "cwfis_activefires",
            name: "CWFIS National Active Fires (NRCan)",
            domain: EventKind::Wildfire,
            cadence: Duration::from_secs(600),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        let client = reqwest::Client::builder()
            .user_agent("engineering-effects/0.1 (+https://raithe.ca)")
            .build()?;
        let body = client.get(self.url()).send().await?.text().await?;
        parse_cwfis_activefires(&body)
    }
}

/// Human label for a CIFFC stage-of-control code.
fn stage_label(stage: &str) -> &str {
    match stage {
        "OC" => "Out-of-control",
        "BH" => "Being-held",
        "UC" => "Under-control",
        _ => "Active",
    }
}

/// Severity from containment stage (the dominant term) lifted by fire size. An
/// out-of-control burn outranks a held one; large active fires push toward 1.0.
/// `percent_contained` is ignored — it is a `-1` "not reported" sentinel on every record.
fn severity_for(stage: &str, size_ha: f64) -> f64 {
    let base = match stage {
        "OC" => 0.85,
        "BH" => 0.55,
        "UC" => 0.30,
        _ => 0.40,
    };
    // Diminishing log boost, capped so stage stays the dominant signal.
    let boost = ((1.0 + size_ha.max(0.0)).ln() / 40.0).min(0.15);
    base + boost
}

/// Pure parser: CWFIS active-fires GeoJSON -> events. Unit-tested offline.
///
/// IMPORTANT: the GeoJSON `geometry` is in EPSG:3978 (metres), so we read the decimal
/// `latitude`/`longitude` PROPERTIES for the map dot, not `geometry.coordinates`.
pub fn parse_cwfis_activefires(json: &str) -> anyhow::Result<Vec<Event>> {
    let root: serde_json::Value = serde_json::from_str(json)?;
    let features = root
        .get("features")
        .and_then(|f| f.as_array())
        .ok_or_else(|| anyhow::anyhow!("cwfis_activefires: missing 'features' array"))?;

    let mut out = Vec::with_capacity(features.len());
    for f in features {
        let props = f.get("properties").cloned().unwrap_or(serde_json::Value::Null);

        let (Some(lat), Some(lon)) = (
            props.get("latitude").and_then(serde_json::Value::as_f64),
            props.get("longitude").and_then(serde_json::Value::as_f64),
        ) else {
            continue;
        };
        let Some(geo) = Geo::new(lat, lon) else { continue };

        let fire_id = props.get("national_fire_id").and_then(|v| v.as_str()).unwrap_or("");
        if fire_id.is_empty() {
            continue;
        }

        let stage = props.get("stage_of_control_status").and_then(|v| v.as_str()).unwrap_or("");
        let size_ha = props.get("fire_size").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
        let agency = props.get("agency_code").and_then(|v| v.as_str()).unwrap_or("");

        let title = format!("{} wildfire — {size_ha:.0} ha ({agency})", stage_label(stage));

        let time = props
            .get("status_date")
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        out.push(Event {
            id: format!("cwfis-fire-{fire_id}"),
            source_id: "cwfis_activefires".to_string(),
            kind: EventKind::Wildfire,
            title,
            time,
            geo: Some(geo),
            severity: Severity::new(severity_for(stage, size_ha)),
            url: Some("https://cwfis.cfs.nrcan.gc.ca/interactive-map".to_string()),
            raw: serde_json::json!({
                "stage_of_control_status": stage, "fire_size": size_ha, "agency_code": agency,
                "national_fire_cause": props.get("national_fire_cause").cloned().unwrap_or(serde_json::Value::Null),
                "national_fire_id": fire_id,
            }),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
      "type":"FeatureCollection",
      "features":[
        {"type":"Feature","geometry":{"type":"Point","coordinates":[-1408500.6,2110741.1]},
         "properties":{"national_fire_id":"2026_NT_VQ002-26","agency_code":"NT","national_fire_cause":"N",
           "fire_size":100,"stage_of_control_status":"OC","status_date":"2026-06-14T19:45:00Z",
           "latitude":65.15337,"longitude":-127.26272,"percent_contained":-1}},
        {"type":"Feature","geometry":{"type":"Point","coordinates":[0,0]},
         "properties":{"national_fire_id":"2026_BC_K12345","agency_code":"BC","national_fire_cause":"H",
           "fire_size":12,"stage_of_control_status":"UC","status_date":"2026-06-14T18:00:00Z",
           "latitude":54.0,"longitude":-122.0,"percent_contained":-1}},
        {"type":"Feature","properties":{"national_fire_id":"","latitude":50.0,"longitude":-100.0}}
      ]
    }"#;

    #[test]
    fn parses_fixture() {
        let ev = parse_cwfis_activefires(FIXTURE).unwrap();
        // The id-less third feature is dropped.
        assert_eq!(ev.len(), 2);

        assert_eq!(ev[0].id, "cwfis-fire-2026_NT_VQ002-26");
        assert_eq!(ev[0].kind, EventKind::Wildfire);
        assert_eq!(ev[0].title, "Out-of-control wildfire — 100 ha (NT)");
        // Coords come from the lat/lon PROPERTIES, not the EPSG:3978 geometry.
        let g = ev[0].geo.unwrap();
        assert!((g.lat - 65.15337).abs() < 1e-5 && (g.lon + 127.26272).abs() < 1e-5);
        // OC base 0.85 + a (sub-cap) size boost; above the UC fire, not yet saturated.
        assert!(ev[0].severity.value() > 0.85 && ev[0].severity.value() < 1.0);
        assert!(ev[0].severity.value() > ev[1].severity.value());
        // Under-control fire sits near its 0.30 base.
        assert!((ev[1].severity.value() - 0.30).abs() < 0.1);
    }

    #[test]
    fn errors_on_missing_array() {
        assert!(parse_cwfis_activefires(r#"{"x":1}"#).is_err());
    }
}
