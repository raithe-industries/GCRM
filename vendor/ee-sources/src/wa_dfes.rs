//! WA Department of Fire and Emergency Services (DFES) — EmergencyWA warnings.
//! Free, no API key. The official RSS feed of current public warnings the WA
//! state emergency service has issued for Western Australia, across ALL hazards
//! it coordinates: bushfire, cyclone, flood, storm, earthquake, hazardous
//! material and more. Attribution: "Department of Fire and Emergency Services".
//!
//! Reads the warnings product `message.rss` — an RSS 2.0 feed, one `<item>` per
//! active warning. The operational signal is each warning's official **Australian
//! Warning System level** — Emergency Warning (red) / Watch and Act (orange) /
//! Advice (yellow) — a defined call-to-action scale (each level a named public
//! action), carried in the item description as a `Category:` field, alongside the
//! affected `dfes:region` and a `geo:lat`/`geo:long` point. One normalized
//! [`Event`] per warning at that point.
//!
//! This is the operational **emergency-warning** modality extended to a second
//! Australian state and, unlike the fire-only `nsw_rfs`, an **all-hazard** feed:
//! Western Australia includes the cyclone-prone Pilbara (LNG / iron-ore export
//! infrastructure) and its floods and storms, so a WA warning is not duplicative
//! of the NSW fire feed or the global thermal-hotspot wildfire feeds (which detect
//! heat pixels, not the human-facing warning level an authority has declared).
//! Severity is driven by the warning level (Emergency Warning 0.95 → Watch and Act
//! 0.7 → Advice 0.45 → other/stand-down 0.25). An empty feed (no current warnings
//! — the common quiet state) yields zero events, not an error.
//!
//! ## Ingestion — Path A (prod fetches the live feed)
//! `www.emergency.wa.gov.au/data/message.rss` is auth-free open data; the host
//! 403s web fetch in-sandbox (the standing egress wall), so the exact wire schema
//! — the RSS `<item>` layout, the `dfes:` / `geo:` namespaces, the `<b>Category:
//! </b>` warning-level field inside the description CDATA, `<dfes:region>`, and
//! the `<geo:lat>`/`<geo:long>` point — is anchored to real committed GitHub
//! bytes: the `exxamalte/python-georss-wa-dfes-client` library (`consts.py` feed
//! URL + attribute constants, `feed_entry.py` warnings entry, and its real
//! `tests/fixtures/wa_dfes_warnings_feed.xml` capture). Prod (full network)
//! fetches the live feed.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ee_core::{Event, EventKind, Geo, Severity, Source, SourceMeta};
use std::time::Duration;

/// WA DFES EmergencyWA warnings source.
#[derive(Default)]
pub struct WaDfes;

impl WaDfes {
    pub fn url(&self) -> &'static str {
        "https://www.emergency.wa.gov.au/data/message.rss"
    }
}

#[async_trait]
impl Source for WaDfes {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "wa_dfes",
            // Umbrella domain for the all-hazard warning mix (per-event kind is set
            // precisely below): most non-fire DFES warnings are weather-driven.
            name: "WA DFES Emergency Warnings",
            domain: EventKind::Weather,
            cadence: Duration::from_secs(600),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        let body = crate::http::fetch_text(self.url()).await?;
        parse_wa_dfes(&body)
    }
}

/// Normalized 0–1 severity from the official Australian Warning System level. The
/// level is the warning tier a state emergency service has declared for people on
/// the ground — a defined public-action scale, not a raw scalar. Matched on
/// substrings so a decorated category ("Bushfire Emergency Warning") still grades.
fn severity_for_level(category: &str) -> f64 {
    let c = category.trim().to_ascii_lowercase();
    if c.contains("emergency warning") {
        0.95
    } else if c.contains("watch and act") {
        0.7
    } else if c.contains("advice") {
        0.45
    } else {
        // An all-clear / stand-down / unrecognized tier: still a real active warning
        // item, but the lowest severity so live warnings win the severity-sorted cap.
        0.25
    }
}

/// The affected map layer for a warning. DFES is an all-hazard service; the hazard
/// is named in the warning title. Fire warnings plot in the Wildfire layer; every
/// other hazard (cyclone / flood / storm / hazmat …) falls to the Weather umbrella.
fn kind_for_title(title: &str) -> EventKind {
    if title.to_ascii_lowercase().contains("fire") {
        EventKind::Wildfire
    } else {
        EventKind::Weather
    }
}

/// Minimal XML entity unescape for the few entities these feeds carry in text
/// nodes (region / title may contain `&amp;`). CDATA is stripped before this runs.
fn unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
}

/// Inner text of the first `<name ...>…</name>` element in `item`, CDATA-stripped,
/// entity-unescaped and trimmed. Tolerant of attributes on the opening tag and of
/// namespaced names (e.g. `dfes:region`, `geo:lat`). `None` if absent/empty.
fn tag_text(item: &str, name: &str) -> Option<String> {
    let open = item.find(&format!("<{name}"))?;
    let content_start = open + item[open..].find('>')? + 1;
    let close = format!("</{name}>");
    let end = item[content_start..].find(&close)? + content_start;
    let mut inner = item[content_start..end].trim();
    if let Some(rest) = inner.strip_prefix("<![CDATA[") {
        inner = rest.strip_suffix("]]>").unwrap_or(rest);
    }
    let val = unescape(inner).trim().to_string();
    (!val.is_empty()).then_some(val)
}

/// The warning level from the description's `Category:` field. In the feed the
/// label sits inside a tag (`<b>Category: </b>Watch and Act`), so after the label
/// we skip one immediately-following markup tag, then read the value up to the next
/// `<`. Handles both `Category: </b>Value<` and a bare `Category: Value<`.
pub fn category_of(desc: &str) -> Option<String> {
    let i = desc.find("Category:")? + "Category:".len();
    let mut rest = desc[i..].trim_start();
    if rest.starts_with('<') {
        let gt = rest.find('>')?;
        rest = rest[gt + 1..].trim_start();
    }
    let end = rest.find('<').unwrap_or(rest.len());
    let val = unescape(rest[..end].trim()).trim().to_string();
    (!val.is_empty()).then_some(val)
}

/// Operator chip behind a WA DFES warning dot: the warning level + the affected
/// region, e.g. "Watch and Act · Goldfields Midlands". The level is a defined
/// public-action scale (signal-meaningful, not a raw number); the region names
/// where. `raw` is the stored `{category, region, title}`. Falls back to the title
/// when no category is present; `None` only if nothing meaningful is present.
pub fn warning_chip(raw: &serde_json::Value) -> Option<String> {
    let get = |k: &str| {
        raw.get(k)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    let category = get("category");
    let region = get("region");
    match (category, region) {
        (Some(c), Some(r)) => Some(format!("{c} · {r}")),
        (Some(c), None) => Some(c.to_string()),
        (None, Some(r)) => Some(r.to_string()),
        (None, None) => get("title").map(str::to_string),
    }
}

/// Pure parser: WA DFES `message.rss` warnings RSS -> one [`Event`] per warning at
/// its `geo:lat`/`geo:long` point. Offline-tested. A body that isn't an RSS feed
/// (e.g. an HTML 403 page) is an error so the last-good layer takes over; a valid
/// feed with no `<item>`s (no current warnings) is Ok/empty; items without a usable
/// point are skipped.
pub fn parse_wa_dfes(xml: &str) -> anyhow::Result<Vec<Event>> {
    if !xml.contains("<rss") && !xml.contains("<channel") {
        anyhow::bail!("wa_dfes: not an RSS feed");
    }

    let mut out = Vec::new();
    for chunk in xml.split("<item").skip(1) {
        // Bound each item to its own closing tag so tag_text can't leak into a sibling.
        let item = match chunk.find("</item>") {
            Some(e) => &chunk[..e],
            None => chunk,
        };

        let desc = tag_text(item, "description").unwrap_or_default();
        let category = category_of(&desc);
        let region = tag_text(item, "dfes:region");

        let (Some(lat), Some(lon)) = (
            tag_text(item, "geo:lat").and_then(|s| s.trim().parse::<f64>().ok()),
            tag_text(item, "geo:long").and_then(|s| s.trim().parse::<f64>().ok()),
        ) else {
            continue;
        };
        let Some(geo) = Geo::new(lat, lon) else { continue };

        let title = tag_text(item, "title").unwrap_or_else(|| "WA DFES warning".to_string());
        let guid = tag_text(item, "guid");
        let id = match &guid {
            Some(g) => format!("wa-dfes-{g}"),
            None => format!("wa-dfes-{title}"),
        };

        // pubDate is RFC-822 ("Sun, 30 Sep 2018 08:30:00 GMT") and optional; a live
        // current-warnings feed reads "as observed this fetch" when it's absent.
        let time = tag_text(item, "pubDate")
            .and_then(|d| DateTime::parse_from_rfc2822(d.trim()).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        let severity = severity_for_level(category.as_deref().unwrap_or(""));

        out.push(Event {
            id,
            source_id: "wa_dfes".to_string(),
            kind: kind_for_title(&title),
            title,
            time,
            geo: Some(geo),
            severity: Severity::new(severity),
            url: Some("https://www.emergency.wa.gov.au/".to_string()),
            raw: serde_json::json!({
                "category": category,
                "region": region,
            }),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The REAL upstream shape, verbatim from the WA DFES warnings feed (captured in
    // exxamalte/python-georss-wa-dfes-client tests/fixtures/wa_dfes_warnings_feed.xml):
    // the dfes:/geo: namespaces, the "<b>Category: </b>…" description CDATA, the
    // <dfes:region> tag, an optional <pubDate>, and the geo:lat/geo:long point.
    const REAL_FIXTURE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss xmlns:fn="http://www.w3.org/2005/xpath-functions"
     xmlns:datetime="http://exslt.org/dates-and-times"
     xmlns:dfes="http://emergency.wa.gov.au/xmlns/dfes"
     xmlns:dc="http://purl.org/dc/elements/1.1/"
     xmlns:geo="http://www.w3.org/2003/01/geo/wgs84_pos#" version="2.0">
    <channel>
        <title>DFES Warnings (All Regions)</title>
        <item>
            <title>Title 1</title>
            <description><![CDATA[
<div>
<b>Category: </b>Category 1</div>
<div>
]]></description>
            <dfes:region>Region 1</dfes:region>
            <pubDate>Sun, 30 Sep 2018 08:30:00 GMT</pubDate>
            <guid>1234</guid>
            <geo:long>121.30196</geo:long>
            <geo:lat>-30.97304</geo:lat>
        </item>
        <item>
            <title>Title 2</title>
            <description><![CDATA[
<div>
<b>Category: </b>Category 2</div>
<div>
]]></description>
            <dfes:region>Region 2</dfes:region>
            <guid>130805106</guid>
            <geo:long>128.98376</geo:long>
            <geo:lat>-15.62504</geo:lat>
        </item>
    </channel>
</rss>"#;

    #[test]
    fn parses_real_feed_shape() {
        let ev = parse_wa_dfes(REAL_FIXTURE).unwrap();
        assert_eq!(ev.len(), 2);

        let e0 = &ev[0];
        assert_eq!(e0.id, "wa-dfes-1234");
        assert_eq!(e0.source_id, "wa_dfes");
        assert_eq!(e0.title, "Title 1");
        // "Category 1" is a placeholder tier (not an AWS level) -> lowest severity,
        // proving the description Category field is extracted past the `</b>`.
        assert_eq!(
            warning_chip(&e0.raw).as_deref(),
            Some("Category 1 · Region 1")
        );
        assert!((e0.severity.value() - 0.25).abs() < 1e-9);
        let g = e0.geo.unwrap();
        assert!((g.lat + 30.97304).abs() < 1e-9, "lat {}", g.lat);
        assert!((g.lon - 121.30196).abs() < 1e-9, "lon {}", g.lon);
        // pubDate parsed from RFC-822.
        assert_eq!(e0.time.format("%Y-%m-%d").to_string(), "2018-09-30");

        // Second item has no pubDate (real feed behaviour) -> "now" fallback, still plots.
        let e1 = &ev[1];
        assert_eq!(e1.id, "wa-dfes-130805106");
        let g1 = e1.geo.unwrap();
        assert!((g1.lat + 15.62504).abs() < 1e-9 && (g1.lon - 128.98376).abs() < 1e-9);
    }

    #[test]
    fn severity_and_chip_ladder_over_real_aws_levels() {
        // The three Australian Warning System levels DFES actually issues, exercising
        // the ladder + the "level · region" chip. Decorated categories still grade.
        let feed = r#"<rss xmlns:dfes="http://emergency.wa.gov.au/xmlns/dfes"
             xmlns:geo="http://www.w3.org/2003/01/geo/wgs84_pos#"><channel>
            <item><title>Bushfire warning A</title>
              <description><![CDATA[<b>Category: </b>Emergency Warning]]></description>
              <dfes:region>Goldfields Midlands</dfes:region><guid>a</guid>
              <geo:long>121.0</geo:long><geo:lat>-30.0</geo:lat></item>
            <item><title>Cyclone warning B</title>
              <description><![CDATA[<b>Category: </b>Watch and Act]]></description>
              <dfes:region>Pilbara</dfes:region><guid>b</guid>
              <geo:long>118.6</geo:long><geo:lat>-20.7</geo:lat></item>
            <item><title>Flood warning C</title>
              <description><![CDATA[<b>Category: </b>Advice]]></description>
              <dfes:region>Kimberley</dfes:region><guid>c</guid>
              <geo:long>128.7</geo:long><geo:lat>-17.9</geo:lat></item>
        </channel></rss>"#;
        let ev = parse_wa_dfes(feed).unwrap();
        assert_eq!(ev.len(), 3);

        let by = |t: &str| ev.iter().find(|e| e.title == t).unwrap();
        assert!((by("Bushfire warning A").severity.value() - 0.95).abs() < 1e-9);
        assert!((by("Cyclone warning B").severity.value() - 0.7).abs() < 1e-9);
        assert!((by("Flood warning C").severity.value() - 0.45).abs() < 1e-9);

        // Fire warnings land in the Wildfire layer; other hazards in the Weather umbrella.
        assert_eq!(by("Bushfire warning A").kind, EventKind::Wildfire);
        assert_eq!(by("Cyclone warning B").kind, EventKind::Weather);
        assert_eq!(by("Flood warning C").kind, EventKind::Weather);

        assert_eq!(
            warning_chip(&by("Cyclone warning B").raw).as_deref(),
            Some("Watch and Act · Pilbara")
        );
    }

    #[test]
    fn empty_feed_is_ok_not_error() {
        // A valid feed with no current warnings (the common quiet state) -> zero events.
        let ev = parse_wa_dfes(
            r#"<rss version="2.0"><channel><title>DFES Warnings</title></channel></rss>"#,
        )
        .unwrap();
        assert!(ev.is_empty());
    }

    #[test]
    fn errors_on_bad_input() {
        // An HTML 403 page (not RSS) is an error so the last-good layer takes over.
        assert!(parse_wa_dfes("<html><body>403 Forbidden</body></html>").is_err());
        assert!(parse_wa_dfes("not xml at all").is_err());
    }

    #[test]
    fn item_without_point_is_skipped_not_fatal() {
        // A warning item missing coordinates is dropped; the geocoded one still plots.
        let feed = r#"<rss xmlns:dfes="http://emergency.wa.gov.au/xmlns/dfes"
             xmlns:geo="http://www.w3.org/2003/01/geo/wgs84_pos#"><channel>
            <item><title>No coords</title>
              <description><![CDATA[<b>Category: </b>Advice]]></description>
              <dfes:region>Perth</dfes:region><guid>x</guid></item>
            <item><title>Has coords</title>
              <description><![CDATA[<b>Category: </b>Advice]]></description>
              <dfes:region>Perth</dfes:region><guid>y</guid>
              <geo:long>115.86</geo:long><geo:lat>-31.95</geo:lat></item>
        </channel></rss>"#;
        let ev = parse_wa_dfes(feed).unwrap();
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].id, "wa-dfes-y");
    }
}
