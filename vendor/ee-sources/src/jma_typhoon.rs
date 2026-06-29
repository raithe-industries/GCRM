//! Japan Meteorological Agency — RSMC Tokyo Typhoon Center: active tropical cyclones
//! over the Western North Pacific and the South China Sea. Free, no API key.
//!
//! JMA is Japan's national meteorological service and the WMO-designated Regional
//! Specialized Meteorological Centre (RSMC) for the NW-Pacific basin — the official
//! tropical-cyclone authority there, the basin NOAA's NHC does NOT cover (NHC is
//! Atlantic + E/C Pacific only). This fills that storm-coverage gap with live data.
//!
//! The `bosai` portal serves the data as a small index plus one file per system:
//! - `targetTc.json` — an array of the currently-active systems, each carrying a
//!   `tropicalCyclone` directory id (e.g. `"TC2105"`). Empty array = quiet basin.
//! - `{tropicalCyclone}/forecast.json` — an array of "part" objects: a *title* part
//!   (storm `name`, issue time), an *analysis* part (the current observed position:
//!   `center` `[lat, lon]`, `pressure` hPa, `maximumWind.sustained.knots`,
//!   `category.en`), and several *forecast* parts (each tagged `advancedHours`).
//!
//! [`Source::fetch`] reads the index, then each system's forecast, and emits one
//! normalized [`EventKind::Weather`] [`Event`] per system from its analysis part. An
//! empty index — the normal quiet-basin state — yields zero events, not an error.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use serde_json::Value;
use std::time::Duration;

/// JMA RSMC-Tokyo active-typhoon source.
#[derive(Default)]
pub struct JmaTyphoon;

impl JmaTyphoon {
    pub fn base_url(&self) -> &'static str {
        "https://www.jma.go.jp/bosai/typhoon/data"
    }
}

#[async_trait]
impl Source for JmaTyphoon {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "jma_typhoon",
            name: "JMA Typhoons (RSMC Tokyo)",
            domain: EventKind::Weather,
            cadence: Duration::from_secs(1800),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        let base = self.base_url();
        let index = crate::http::fetch_text(&format!("{base}/targetTc.json")).await?;
        let ids = parse_targets(&index)?;

        // At most a handful of simultaneous systems; cap defensively so a malformed
        // index can't fan out unbounded. One bad per-system fetch is skipped, not fatal.
        let mut out = Vec::new();
        for id in ids.into_iter().take(12) {
            let url = format!("{base}/{id}/forecast.json");
            let body = match crate::http::fetch_text(&url).await {
                Ok(t) => t,
                Err(_) => continue,
            };
            if let Ok(mut evs) = parse_jma(&body) {
                out.append(&mut evs);
            }
        }
        Ok(out)
    }
}

/// Human label for a JMA `category.en` code (the chip/title shouldn't show a raw code).
/// Unknown codes pass through so a new code still reads.
pub fn category_label(code: &str) -> &str {
    match code {
        "TD" => "Tropical Depression",
        "TS" => "Tropical Storm",
        "STS" => "Severe Tropical Storm",
        "TY" => "Typhoon",
        "L" | "LO" => "Extratropical Low",
        other => other,
    }
}

/// JMA typhoon intensity grade for a ≥64 kt system (Strong / Very Strong / Violent),
/// the qualifier JMA attaches to the "Typhoon" class; `None` below typhoon strength.
fn typhoon_grade(kt: Option<f64>) -> Option<&'static str> {
    let kt = kt?;
    Some(match kt {
        k if k >= 105.0 => "Violent",     // ≥54 m/s
        k if k >= 85.0 => "Very Strong",  // ≥44 m/s
        k if k >= 64.0 => "Strong",       // ≥33 m/s
        _ => return None,
    })
}

/// Normalized 0–1 severity from JMA 10-min max-sustained wind (kt), falling back to
/// the category class when the wind is absent.
fn severity_for(cat: &str, kt: Option<f64>) -> f64 {
    if let Some(kt) = kt {
        return match kt {
            k if k >= 105.0 => 0.95,
            k if k >= 85.0 => 0.85,
            k if k >= 64.0 => 0.70,
            k if k >= 48.0 => 0.55,
            k if k >= 34.0 => 0.45,
            _ => 0.30,
        };
    }
    match cat {
        "TY" => 0.70,
        "STS" => 0.55,
        "TS" => 0.45,
        "TD" => 0.30,
        "L" | "LO" => 0.25,
        _ => 0.40,
    }
}

/// Operator chip for an active system: the category (with JMA intensity grade for
/// typhoons) plus max sustained wind (kt) and central pressure (hPa) — e.g.
/// "Strong Typhoon · 80 kt · 950 hPa". Each component is dropped if absent.
pub fn typhoon_chip(raw: &Value) -> Option<String> {
    let cat = raw.get("category").and_then(Value::as_str).unwrap_or("");
    let kt = raw.get("knots").and_then(Value::as_f64);
    let pressure = raw.get("pressure").and_then(Value::as_f64);

    let mut head = category_label(cat).to_string();
    if cat == "TY" {
        if let Some(grade) = typhoon_grade(kt) {
            head = format!("{grade} {head}");
        }
    }

    let mut parts: Vec<String> = Vec::new();
    if !head.is_empty() {
        parts.push(head);
    }
    if let Some(kt) = kt {
        if kt > 0.0 {
            parts.push(format!("{kt:.0} kt"));
        }
    }
    if let Some(p) = pressure {
        if p > 0.0 {
            parts.push(format!("{p:.0} hPa"));
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" · "))
    }
}

/// Read a JSON value as f64 whether it's a number or a numeric string ("950").
fn num(v: Option<&Value>) -> Option<f64> {
    v.and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())))
}

/// Wrap a longitude into [-180, 180] (JMA reports east-positive degrees; a system
/// recurving past the dateline can read just over 180).
fn wrap_lon(lon: f64) -> f64 {
    if lon > 180.0 {
        lon - 360.0
    } else if lon < -180.0 {
        lon + 360.0
    } else {
        lon
    }
}

/// Pure parser: the active-storm index `targetTc.json` -> the `tropicalCyclone`
/// directory ids to fetch. Elements have been observed as plain strings and as objects
/// keyed `tropicalCyclone`; both are handled. An empty array (the normal quiet-basin
/// state) yields an empty list, not an error.
pub fn parse_targets(json: &str) -> anyhow::Result<Vec<String>> {
    let root: Value = serde_json::from_str(json)?;
    let arr = root
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("jma_typhoon: targetTc.json is not a JSON array"))?;
    let mut ids = Vec::new();
    for e in arr {
        let id = e
            .as_str()
            .map(str::to_string)
            .or_else(|| e.get("tropicalCyclone").and_then(Value::as_str).map(str::to_string));
        if let Some(id) = id {
            if !id.is_empty() {
                ids.push(id);
            }
        }
    }
    Ok(ids)
}

/// Pure parser: one system's `forecast.json` -> at most one event (the current
/// observed position from the *analysis* part). Unit-tested offline. A payload with no
/// analysis part (e.g. only forecast lead times) or an out-of-range centre yields zero
/// events, not an error; non-array / non-JSON input is an error.
pub fn parse_jma(json: &str) -> anyhow::Result<Vec<Event>> {
    let root: Value = serde_json::from_str(json)?;
    let parts = root
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("jma_typhoon: forecast.json is not a JSON array"))?;

    // Title part: storm identity (name + numbers + issue time).
    let title = parts.iter().find(|p| p.get("name").is_some());
    let tc_id = title.and_then(|t| t.get("tropicalCyclone")).and_then(Value::as_str).unwrap_or("");
    let number = title.and_then(|t| t.get("typhoonNumber")).and_then(Value::as_str).unwrap_or("");
    let name = title
        .and_then(|t| t.get("name"))
        .and_then(|n| n.get("en").or_else(|| n.get("jp")))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();

    // Analysis part: the current observed fix — a centre, but no forecast lead time.
    let Some(analysis) = parts
        .iter()
        .find(|p| p.get("center").is_some() && p.get("advancedHours").is_none())
    else {
        return Ok(Vec::new());
    };

    let center = analysis.get("center").and_then(Value::as_array);
    let lat = center.and_then(|c| c.first()).and_then(Value::as_f64);
    let lon = center.and_then(|c| c.get(1)).and_then(Value::as_f64).map(wrap_lon);
    let (Some(lat), Some(lon)) = (lat, lon) else { return Ok(Vec::new()) };
    let Some(geo) = Geo::new(lat, lon) else { return Ok(Vec::new()) };

    let cat = analysis.get("category").and_then(|c| c.get("en")).and_then(Value::as_str).unwrap_or("");
    let knots = num(analysis.get("maximumWind").and_then(|w| w.get("sustained")).and_then(|s| s.get("knots")));
    let pressure = num(analysis.get("pressure"));

    // Stable id: prefer the directory id, else the typhoon number; otherwise skip.
    let id = if !tc_id.is_empty() {
        tc_id.to_string()
    } else if !number.is_empty() {
        format!("n{number}")
    } else {
        return Ok(Vec::new());
    };

    let label = category_label(cat);
    let base = if label.is_empty() { "Tropical cyclone" } else { label };
    let title_str = if name.is_empty() {
        base.to_string()
    } else {
        format!("{base} {name}")
    };

    let time = analysis
        .get("validtime")
        .and_then(|v| v.get("UTC"))
        .and_then(Value::as_str)
        .or_else(|| title.and_then(|t| t.get("issue")).and_then(|i| i.get("UTC")).and_then(Value::as_str))
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(Utc::now);

    Ok(vec![Event {
        id: format!("jma-{id}"),
        source_id: "jma_typhoon".to_string(),
        kind: EventKind::Weather,
        title: title_str,
        time,
        geo: Some(geo),
        severity: Severity::new(severity_for(cat, knots)),
        url: Some("https://www.jma.go.jp/bosai/map.html#contents=typhoon&lang=en".to_string()),
        raw: serde_json::json!({
            "category": cat,
            "category_label": label,
            "knots": knots,
            "pressure": pressure,
            "name": name,
            "typhoonNumber": number,
        }),
    }])
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real captured JMA bosai output (typhoon TC2105 / IN-FA, 2021-07-21): the active
    // index plus the system's forecast.json — a title part, an analysis part (current
    // fix), then forecast parts tagged `advancedHours`.
    const TARGETS: &str = r#"[{"tropicalCyclone":"TC2105","typhoonNumber":"2106"}]"#;
    const FORECAST: &str = r#"[
      {
        "issue": { "JST": "2021-07-21T22:10:00+09:00", "UTC": "2021-07-21T13:10:00Z" },
        "typhoonNumber": "2106",
        "tropicalCyclone": "TC2105",
        "name": { "jp": "インファ", "en": "IN-FA" }
      },
      {
        "validtime": { "JST": "2021-07-21T21:00:00+09:00", "UTC": "2021-07-21T12:00:00Z" },
        "category": { "jp": "台風", "en": "TY" },
        "center": [23.3, 126.6],
        "pressure": "950",
        "maximumWind": { "sustained": { "mps": 41, "knots": 80 } },
        "galeWarningArea": { "center": [23.3, 126.6], "radius": 330000 }
      },
      {
        "advancedHours": 12,
        "validtime": { "UTC": "2021-07-22T00:00:00Z" },
        "category": { "jp": "台風", "en": "TY" },
        "center": [23.9, 124.9],
        "pressure": "945",
        "maximumWind": { "sustained": { "mps": 43, "knots": 85 } },
        "probabilityCircle": { "center": [23.9, 124.9], "radius": 70000 }
      },
      {
        "advancedHours": 24,
        "validtime": { "UTC": "2021-07-22T12:00:00Z" },
        "category": { "jp": "台風", "en": "TY" },
        "center": [24.6, 122.9],
        "pressure": "940",
        "probabilityCircle": { "center": [24.6, 122.9], "radius": 110000 }
      }
    ]"#;

    #[test]
    fn parses_target_index() {
        assert_eq!(parse_targets(TARGETS).unwrap(), vec!["TC2105".to_string()]);
        // Plain-string element form is also accepted.
        assert_eq!(parse_targets(r#"["TC2110"]"#).unwrap(), vec!["TC2110".to_string()]);
    }

    #[test]
    fn parses_forecast_to_analysis_position() {
        let ev = parse_jma(FORECAST).unwrap();
        assert_eq!(ev.len(), 1);

        let e = &ev[0];
        assert_eq!(e.id, "jma-TC2105");
        assert_eq!(e.kind, EventKind::Weather);
        assert_eq!(e.title, "Typhoon IN-FA");
        // The analysis fix (23.3N, 126.6E) — NOT the +12h/+24h forecast centres.
        let g = e.geo.unwrap();
        assert!((g.lat - 23.3).abs() < 1e-6 && (g.lon - 126.6).abs() < 1e-6);
        // 80 kt -> typhoon band 0.70.
        assert!((e.severity.value() - 0.70).abs() < 1e-9);
        // Time is the analysis validtime, not the title issue time.
        assert_eq!(e.time, DateTime::parse_from_rfc3339("2021-07-21T12:00:00Z").unwrap());
        // Chip: 80 kt -> Strong Typhoon grade, with wind + central pressure.
        assert_eq!(typhoon_chip(&e.raw).as_deref(), Some("Strong Typhoon · 80 kt · 950 hPa"));
    }

    #[test]
    fn empty_index_is_ok_not_error() {
        // Quiet basin: an empty active list is the normal state, not a failure.
        assert!(parse_targets("[]").unwrap().is_empty());
    }

    #[test]
    fn forecast_without_analysis_yields_nothing() {
        // Only a title + a forecast part (advancedHours) -> no current fix to plot.
        let only_forecast = r#"[
          {"name":{"en":"TEST"},"tropicalCyclone":"TC9999"},
          {"advancedHours":12,"center":[10.0,130.0],"category":{"en":"TS"}}
        ]"#;
        assert!(parse_jma(only_forecast).unwrap().is_empty());
    }

    #[test]
    fn chip_grades_and_labels() {
        // Intensity grade rises with wind for typhoons.
        let violent = serde_json::json!({"category":"TY","knots":110.0,"pressure":905.0});
        assert_eq!(typhoon_chip(&violent).as_deref(), Some("Violent Typhoon · 110 kt · 905 hPa"));
        // Severe tropical storm carries no typhoon grade, just label + wind.
        let sts = serde_json::json!({"category":"STS","knots":55.0});
        assert_eq!(typhoon_chip(&sts).as_deref(), Some("Severe Tropical Storm · 55 kt"));
    }

    #[test]
    fn errors_on_bad_input() {
        // Not a JSON array.
        assert!(parse_jma(r#"{"foo":1}"#).is_err());
        assert!(parse_targets(r#"{"foo":1}"#).is_err());
        // Not JSON at all (e.g. an HTML error page).
        assert!(parse_jma("<html>403</html>").is_err());
    }
}
