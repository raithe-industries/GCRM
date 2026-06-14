//! Canadian Centre for Cyber Security (CCCS) — alerts & advisories from CCCS, part of
//! the Communications Security Establishment (CSE), Canada's federal cyber authority.
//! Free, no API key.
//!
//! Parses the CCCS Atom feed into normalized [`EventKind::Cyber`] [`Event`]s — the
//! Canadian-authority counterpart to the US [`crate::cisa_kev`] catalog (broad vendor
//! advisories + ICS/control-systems items, which KEV's curated CVE list lacks). Like
//! KEV these are non-geographic signals, so `geo` is always `None`; this source is a
//! registry-catalog connector for a cyber surface, NOT a map feed (a geo-less feed
//! plotted on the map is a count with zero dots — see the map's no-cisa_kev rule).

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ee_core::{Event, EventKind, Severity, Source, SourceMeta};
use std::time::Duration;

/// CCCS alerts & advisories source.
#[derive(Default)]
pub struct Cccs;

impl Cccs {
    pub fn url(&self) -> &'static str {
        // Canonical Atom endpoint (the /rss/ path 301s then 404s — hardcode this).
        "https://www.cyber.gc.ca/api/cccs/atom/v1/get?feed=alerts_advisories&lang=en"
    }
}

#[async_trait]
impl Source for Cccs {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "cccs",
            name: "Canadian Centre for Cyber Security",
            domain: EventKind::Cyber,
            cadence: Duration::from_secs(3600),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        let client = reqwest::Client::builder()
            .user_agent("engineering-effects/0.1 (+https://raithe.ca)")
            .build()?;
        let body = client.get(self.url()).send().await?.text().await?;
        parse_cccs(&body)
    }
}

/// The text between the first `open` and the following `close` in `s`.
fn between<'a>(s: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = s.find(open)? + open.len();
    let rest = &s[start..];
    let end = rest.find(close)?;
    Some(&rest[..end])
}

/// Unwrap a CDATA section and trim.
fn strip_cdata(s: &str) -> &str {
    s.trim()
        .strip_prefix("<![CDATA[")
        .and_then(|x| x.strip_suffix("]]>"))
        .unwrap_or_else(|| s.trim())
        .trim()
}

/// Severity from the advisory title: alerts (`(AL…)`) outrank advisories (`(AV…)`);
/// control-systems (ICS) items and active "Update"s nudge it up. CCCS carries no CVSS.
fn severity_for(title: &str) -> f64 {
    let mut s = if title.contains("(AL") {
        0.8
    } else if title.contains("(AV") {
        0.4
    } else {
        0.4
    };
    let lower = title.to_lowercase();
    if lower.contains("control system") {
        s += 0.1;
    }
    if lower.contains("update") {
        s += 0.1;
    }
    s
}

/// Pure parser: CCCS Atom feed -> events. Unit-tested offline. The feed's top-level
/// `<title>` precedes the first `<entry>`, so splitting on `<entry>` and skipping the
/// head chunk leaves one chunk per advisory.
pub fn parse_cccs(xml: &str) -> anyhow::Result<Vec<Event>> {
    if !xml.contains("<feed") || !xml.contains("<entry>") {
        return Err(anyhow::anyhow!("cccs: not an Atom feed with entries"));
    }
    let mut out = Vec::new();
    for chunk in xml.split("<entry>").skip(1) {
        let Some(id) = between(chunk, "<id>", "</id>").map(str::trim).filter(|s| !s.is_empty()) else {
            continue;
        };
        let title = between(chunk, "<title>", "</title>").map(strip_cdata).unwrap_or("Advisory");
        let time = between(chunk, "<updated>", "</updated>")
            .map(str::trim)
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        out.push(Event {
            id: id.to_string(),
            source_id: "cccs".to_string(),
            kind: EventKind::Cyber,
            title: format!("CCCS: {title}"),
            time,
            geo: None,
            severity: Severity::new(severity_for(title)),
            url: Some(id.to_string()),
            raw: serde_json::json!({ "title": title }),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"<feed xmlns="http://www.w3.org/2005/Atom" xml:lang="en">
      <id>https://cyber.gc.ca/api/cccs/atom/v1/get?feed=alerts_advisories&amp;lang=en</id>
      <title>Alerts and advisories</title>
      <updated>2026-06-12T19:27:44Z</updated>
      <entry>
        <id>https://cyber.gc.ca/en/alerts-advisories/freepbx-security-advisory-av26-596</id>
        <link rel="alternate" href="https://cyber.gc.ca/en/alerts-advisories/freepbx-security-advisory-av26-596"/>
        <title><![CDATA[FreePBX security advisory (AV26-596)]]></title>
        <updated>2026-06-12T19:27:44Z</updated>
      </entry>
      <entry>
        <id>https://cyber.gc.ca/en/alerts-advisories/control-systems-moxa-al26-001</id>
        <title><![CDATA[[Control Systems] Moxa security advisory (AL26-001)]]></title>
        <updated>2026-06-10T12:00:00Z</updated>
      </entry>
    </feed>"#;

    #[test]
    fn parses_fixture() {
        let ev = parse_cccs(FIXTURE).unwrap();
        assert_eq!(ev.len(), 2);

        assert_eq!(ev[0].id, "https://cyber.gc.ca/en/alerts-advisories/freepbx-security-advisory-av26-596");
        assert_eq!(ev[0].kind, EventKind::Cyber);
        assert!(ev[0].geo.is_none());
        assert_eq!(ev[0].title, "CCCS: FreePBX security advisory (AV26-596)");
        // Plain advisory baseline.
        assert!((ev[0].severity.value() - 0.4).abs() < 1e-9);

        // Alert + control-systems -> 0.8 base + 0.1 ICS = 0.9.
        assert!(ev[1].title.contains("Moxa"));
        assert!((ev[1].severity.value() - 0.9).abs() < 1e-9);
    }

    #[test]
    fn errors_on_non_atom() {
        assert!(parse_cccs(r#"{"json":true}"#).is_err());
    }
}
