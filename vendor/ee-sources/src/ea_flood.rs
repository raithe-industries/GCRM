//! UK Environment Agency — Real Time flood-monitoring API: active flood warnings.
//! Free, no API key. Open Government Licence v3 (credit "Environment Agency").
//!
//! Reads the `id/floods` endpoint — one item per **active flood warning / alert**
//! the Environment Agency (the authoritative flood body for England) has in force,
//! each carrying a `severityLevel` on the national 1–4 scale:
//!   1 Severe Flood Warning (danger to life) · 2 Flood Warning (act now) ·
//!   3 Flood Alert (be prepared) · 4 Warning no longer in force (stand down).
//! The connector queries `?min-severity=3`, so only the three active tiers come
//! back, and re-filters defensively; the "no longer in force" tier is dropped.
//! A day with no active warnings therefore yields zero events, not an error.
//!
//! Why this clears the signal-meaningfulness bar where a raw river level can't
//! (the reason ECCC hydrometric is rejected): `severityLevel` is a **baseline-
//! relative public-action category** — the EA has already compared conditions
//! against each area's own flood thresholds — so the plotted value carries real
//! operator meaning ("Severe Flood Warning") rather than an incomparable absolute
//! gauge reading. It extends the baseline-relative flood modality (opened by the
//! US-only `nwps_flood`) to **England / the UK** — new geography, no overlap.
//!
//! The `floods` item's `floodArea` sub-object carries only a link (`@id`) and a
//! `polygon` URL — no inline coordinates — so this connector joins each warning by
//! its `floodAreaID` against the `id/floodAreas` catalogue (`{ notation, fwdCode,
//! lat, long, riverOrSea, label }`) to place the dot at the flood area's point.
//! Emits one normalized [`EventKind::Weather`] [`Event`] per active warning.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

/// UK EA flood-warning source.
#[derive(Default)]
pub struct EaFlood;

impl EaFlood {
    /// Active warnings, severity 1–3 (the "no longer in force" tier 4 is excluded
    /// by `min-severity=3`; lower number = more severe on the EA scale).
    pub fn floods_url(&self) -> &'static str {
        "https://environment.data.gov.uk/flood-monitoring/id/floods?min-severity=3"
    }
    /// The flood-area catalogue, one record per warning/alert area with its
    /// centroid `lat`/`long` (the `floods` items carry only a link, no coords).
    pub fn areas_url(&self) -> &'static str {
        "https://environment.data.gov.uk/flood-monitoring/id/floodAreas"
    }
}

#[async_trait]
impl Source for EaFlood {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "ea_flood",
            name: "UK EA flood warnings",
            domain: EventKind::Weather,
            cadence: Duration::from_secs(900),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        // Two fetches: the active warnings (severity, no coords) + the flood-area
        // catalogue (coords by area code). The catalogue is large but cacheable.
        let floods = crate::http::fetch_text(self.floods_url()).await?;
        let areas = crate::http::fetch_text(self.areas_url()).await?;
        parse_ea_flood(&floods, &areas)
    }
}

/// Severity from the national flood-warning level (1 = most severe). Levels 1–3
/// are the active tiers; anything else (4 "no longer in force" / missing) drops.
fn severity_for_level(level: i64) -> Option<f64> {
    match level {
        1 => Some(1.0),  // Severe Flood Warning — danger to life
        2 => Some(0.7),  // Flood Warning — immediate action required
        3 => Some(0.4),  // Flood Alert — be prepared
        _ => None,
    }
}

/// Canonical human label for a flood-warning level — the operator read behind the
/// dot. Deterministic (not dependent on the upstream free-text `severity` string).
fn level_label(level: i64) -> &'static str {
    match level {
        1 => "Severe Flood Warning",
        2 => "Flood Warning",
        3 => "Flood Alert",
        _ => "Flood",
    }
}

/// A JSON value that may be a number or a numeric string -> i64.
fn as_i64_loose(v: &Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_f64().map(|f| f as i64))
        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<i64>().ok()))
}

/// A JSON value that may be a number or a numeric string -> f64.
fn as_f64_loose(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
}

fn str_of<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

/// EA API responses wrap results in `items` — usually an array, but a single
/// result can arrive as a bare object. Normalize both to a slice-able Vec.
fn items_of(root: &Value) -> Option<Vec<Value>> {
    match root.get("items") {
        Some(Value::Array(a)) => Some(a.clone()),
        Some(obj @ Value::Object(_)) => Some(vec![obj.clone()]),
        _ => None,
    }
}

/// A flood area's coordinates + descriptive fields, keyed by its area code.
#[derive(Clone)]
struct Area {
    lat: f64,
    lon: f64,
    river: Option<String>,
    label: Option<String>,
}

/// Build the `areaCode -> Area` lookup from the `floodAreas` catalogue. Each area
/// is indexed under both `notation` and `fwdCode` (either can match a warning's
/// `floodAreaID`).
fn parse_areas(json: &str) -> anyhow::Result<HashMap<String, Area>> {
    let root: Value = serde_json::from_str(json)?;
    let items =
        items_of(&root).ok_or_else(|| anyhow::anyhow!("ea_flood: floodAreas missing 'items'"))?;
    let mut map = HashMap::with_capacity(items.len() * 2);
    for a in &items {
        let (Some(lat), Some(lon)) =
            (a.get("lat").and_then(as_f64_loose), a.get("long").and_then(as_f64_loose))
        else {
            continue;
        };
        let area = Area {
            lat,
            lon,
            river: str_of(a, "riverOrSea").map(str::to_string),
            label: str_of(a, "label").or_else(|| str_of(a, "description")).map(str::to_string),
        };
        for code_key in ["notation", "fwdCode"] {
            if let Some(code) = str_of(a, code_key) {
                map.entry(code.to_string()).or_insert_with(|| area.clone());
            }
        }
    }
    Ok(map)
}

/// The area code a warning points at: prefer `floodAreaID`, else the last path
/// segment of the `floodArea.@id` link.
fn warning_area_code(item: &Value) -> Option<String> {
    if let Some(id) = str_of(item, "floodAreaID") {
        return Some(id.to_string());
    }
    item.get("floodArea")
        .and_then(|fa| fa.get("@id"))
        .and_then(Value::as_str)
        .and_then(|u| u.rsplit('/').next())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Operator chip: the flood-warning tier plus the river/sea, e.g.
/// "Severe Flood Warning · River Teme" / "Flood Alert · Upper River Nene".
/// `raw` is the warning item (with `riverOrSea` merged in from the joined area).
pub fn flood_chip(raw: &Value) -> Option<String> {
    let level = raw.get("severityLevel").and_then(as_i64_loose)?;
    severity_for_level(level)?;
    let head = level_label(level);
    match str_of(raw, "riverOrSea") {
        Some(river) => Some(format!("{head} · {river}")),
        None => Some(head.to_string()),
    }
}

/// Pure parser: join the active-warnings JSON to the flood-area catalogue -> events.
/// Unit-tested offline. A payload missing its `items` array is malformed (error);
/// warnings not in an active tier (1–3) or with no catalogue coordinate are filtered
/// out, so a no-warnings day is Ok/empty.
pub fn parse_ea_flood(floods_json: &str, areas_json: &str) -> anyhow::Result<Vec<Event>> {
    let areas = parse_areas(areas_json)?;
    let root: Value = serde_json::from_str(floods_json)?;
    let items =
        items_of(&root).ok_or_else(|| anyhow::anyhow!("ea_flood: floods missing 'items'"))?;

    let mut out = Vec::with_capacity(items.len());
    for item in &items {
        let Some(level) = item.get("severityLevel").and_then(as_i64_loose) else { continue };
        let Some(sev) = severity_for_level(level) else { continue };

        let Some(code) = warning_area_code(item) else { continue };
        let Some(area) = areas.get(&code) else { continue };
        let Some(geo) = Geo::new(area.lat, area.lon) else { continue };

        // Title: the warning's own description ("River Teme at Tenbury Wells"),
        // falling back to the flood area's label.
        let title = str_of(item, "description")
            .map(str::to_string)
            .or_else(|| area.label.clone())
            .unwrap_or_else(|| code.clone());

        let time = str_of(item, "timeRaised")
            .or_else(|| str_of(item, "timeMessageChanged"))
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        let url = str_of(item, "@id")
            .map(str::to_string)
            .unwrap_or_else(|| format!("https://check-for-flooding.service.gov.uk/target-area/{code}"));

        // Merge the joined river name into the raw so the chip can surface it.
        let mut raw = item.clone();
        if let (Some(obj), Some(river)) = (raw.as_object_mut(), area.river.as_ref()) {
            obj.insert("riverOrSea".to_string(), Value::String(river.clone()));
        }

        out.push(Event {
            id: format!("ea-flood-{code}"),
            source_id: "ea_flood".to_string(),
            kind: EventKind::Weather,
            title,
            time,
            geo: Some(geo),
            severity: Severity::new(sev),
            url: Some(url),
            raw,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Built to the confirmed EA flood-monitoring shape (anchored to the committed
    // `alicebarbe/England-Flood-Warnings-and-Visualizations` floodsystem client:
    // `id/floods?min-severity=N` items carry floodAreaID/severity/severityLevel/
    // isTidal/message/description + a floodArea link with @id+polygon but NO coords;
    // the area's lat/long come from the joined floodAreas resource). One Severe
    // (level 1), one Warning (level 2), one Alert (level 3) — all kept — plus a
    // "no longer in force" (level 4, dropped) and a warning whose area is absent
    // from the catalogue (can't be placed, dropped). Witney carries real coords.
    const FLOODS: &str = r#"{
      "@context":"http://context.jsonld",
      "items":[
        {"@id":"http://environment.data.gov.uk/flood-monitoring/id/floods/062FWF46Tenbury",
         "description":"River Teme at Tenbury Wells","eaAreaName":"West Midlands",
         "floodAreaID":"062FWF46Tenbury",
         "floodArea":{"@id":"http://environment.data.gov.uk/flood-monitoring/id/floodAreas/062FWF46Tenbury",
           "county":"Worcestershire","notation":"062FWF46Tenbury","polygon":"http://environment.data.gov.uk/flood-monitoring/id/floodAreas/062FWF46Tenbury/polygon"},
         "isTidal":false,"message":"River levels are rising.","severity":"Severe Flood Warning",
         "severityLevel":1,"timeRaised":"2026-07-08T06:12:00Z"},
        {"@id":"http://environment.data.gov.uk/flood-monitoring/id/floods/061FWF10Witney",
         "description":"River Windrush at Witney","floodAreaID":"061FWF10Witney",
         "floodArea":{"@id":"http://environment.data.gov.uk/flood-monitoring/id/floodAreas/061FWF10Witney",
           "notation":"061FWF10Witney","polygon":"http://environment.data.gov.uk/flood-monitoring/id/floodAreas/061FWF10Witney/polygon"},
         "isTidal":false,"severity":"Flood Warning","severityLevel":2,
         "timeRaised":"2026-07-08T05:40:00Z"},
        {"@id":"http://environment.data.gov.uk/flood-monitoring/id/floods/053FAG30Nene",
         "description":"Lower River Nene","floodAreaID":"053FAG30Nene",
         "floodArea":{"@id":"http://environment.data.gov.uk/flood-monitoring/id/floodAreas/053FAG30Nene",
           "notation":"053FAG30Nene","polygon":"x"},
         "isTidal":false,"severity":"Flood Alert","severityLevel":"3",
         "timeMessageChanged":"2026-07-08T04:00:00Z"},
        {"@id":"http://environment.data.gov.uk/flood-monitoring/id/floods/099OLD",
         "description":"Old warning","floodAreaID":"099OLD",
         "floodArea":{"notation":"099OLD"},"severity":"Warning no longer in force","severityLevel":4},
        {"@id":"http://environment.data.gov.uk/flood-monitoring/id/floods/000NOAREA",
         "description":"Placeless warning","floodAreaID":"000NOAREA",
         "floodArea":{"notation":"000NOAREA"},"severity":"Flood Warning","severityLevel":2}
      ]
    }"#;

    const AREAS: &str = r#"{
      "items":[
        {"@id":"http://environment.data.gov.uk/flood-monitoring/id/floodAreas/062FWF46Tenbury",
         "county":"Worcestershire","description":"River Teme at Tenbury Wells","label":"River Teme at Tenbury Wells",
         "fwdCode":"062FWF46Tenbury","notation":"062FWF46Tenbury","lat":52.3097,"long":-2.5936,"riverOrSea":"River Teme"},
        {"@id":"http://environment.data.gov.uk/flood-monitoring/id/floodAreas/061FWF10Witney",
         "county":"Oxfordshire","label":"River Windrush at Witney",
         "fwdCode":"061FWF10Witney","notation":"061FWF10Witney","lat":51.7859,"long":-1.4851,"riverOrSea":"River Windrush"},
        {"@id":"http://environment.data.gov.uk/flood-monitoring/id/floodAreas/053FAG30Nene",
         "county":"Northamptonshire","label":"Lower River Nene",
         "fwdCode":"053FAG30Nene","notation":"053FAG30Nene","lat":52.5470,"long":-0.2510,"riverOrSea":"River Nene"}
      ]
    }"#;

    #[test]
    fn parses_fixture_joining_coords_and_dropping_inactive_and_placeless() {
        let ev = parse_ea_flood(FLOODS, AREAS).unwrap();
        // Level 4 (no longer in force) dropped; placeless level-2 (no area) dropped.
        assert_eq!(ev.len(), 3);

        assert_eq!(ev[0].id, "ea-flood-062FWF46Tenbury");
        assert_eq!(ev[0].kind, EventKind::Weather);
        assert_eq!(ev[0].title, "River Teme at Tenbury Wells");
        assert!((ev[0].severity.value() - 1.0).abs() < 1e-9); // Severe -> 1.0
        let g = ev[0].geo.unwrap();
        assert!((g.lat - 52.3097).abs() < 1e-6 && (g.lon + 2.5936).abs() < 1e-6);
        assert_eq!(flood_chip(&ev[0].raw).as_deref(), Some("Severe Flood Warning · River Teme"));
        assert_eq!(ev[0].time.format("%Y-%m-%d %H:%M").to_string(), "2026-07-08 06:12");

        // Witney: real coords, Flood Warning -> 0.7.
        assert_eq!(ev[1].title, "River Windrush at Witney");
        assert!((ev[1].severity.value() - 0.7).abs() < 1e-9);
        let g = ev[1].geo.unwrap();
        assert!((g.lat - 51.7859).abs() < 1e-6 && (g.lon + 1.4851).abs() < 1e-6);
        assert_eq!(flood_chip(&ev[1].raw).as_deref(), Some("Flood Warning · River Windrush"));

        // severityLevel arrived as the string "3" -> still parsed; Flood Alert -> 0.4.
        assert!((ev[2].severity.value() - 0.4).abs() < 1e-9);
        assert_eq!(flood_chip(&ev[2].raw).as_deref(), Some("Flood Alert · River Nene"));
    }

    #[test]
    fn no_active_warnings_is_ok_not_error() {
        // Empty items (the common quiet state) -> zero plotted events, not a failure.
        assert!(parse_ea_flood(r#"{"items":[]}"#, AREAS).unwrap().is_empty());
        // Only a "no longer in force" (level 4) warning -> also empty.
        let floods = r#"{"items":[{"floodAreaID":"099OLD","floodArea":{"notation":"099OLD"},
          "severity":"Warning no longer in force","severityLevel":4}]}"#;
        assert!(parse_ea_flood(floods, AREAS).unwrap().is_empty());
    }

    #[test]
    fn errors_on_bad_input() {
        // Floods payload missing 'items' is malformed.
        assert!(parse_ea_flood(r#"{"@context":"x"}"#, AREAS).is_err());
        // Areas catalogue missing 'items' is malformed.
        assert!(parse_ea_flood(r#"{"items":[]}"#, r#"{"oops":true}"#).is_err());
        // Not JSON at all (e.g. an HTML 403 page).
        assert!(parse_ea_flood("<html>403</html>", AREAS).is_err());
    }

    #[test]
    fn severity_ladders_with_level() {
        assert_eq!(severity_for_level(1), Some(1.0));
        assert_eq!(severity_for_level(2), Some(0.7));
        assert_eq!(severity_for_level(3), Some(0.4));
        assert_eq!(severity_for_level(4), None);
        assert_eq!(severity_for_level(0), None);
    }

    #[test]
    fn area_code_falls_back_to_floodarea_id_link() {
        // No floodAreaID, but a floodArea.@id link -> last path segment is the code.
        let item = serde_json::json!({
            "floodArea":{"@id":"http://environment.data.gov.uk/flood-monitoring/id/floodAreas/061FWF10Witney"}
        });
        assert_eq!(warning_area_code(&item).as_deref(), Some("061FWF10Witney"));
    }
}
