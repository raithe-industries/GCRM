//! CISA KEV — the Known Exploited Vulnerabilities catalog. Free, no API key.
//!
//! Parses CISA's published catalog
//! (<https://www.cisa.gov/known-exploited-vulnerabilities-catalog>) into normalized
//! [`Event`]s of kind [`EventKind::Cyber`]. These are non-geographic signals, so
//! `geo` is always `None`.

use async_trait::async_trait;
use chrono::{NaiveDate, TimeZone, Utc};
use ee_core::{Event, EventKind, Severity, Source, SourceMeta};
use std::time::Duration;

/// CISA Known Exploited Vulnerabilities source.
#[derive(Default)]
pub struct CisaKev;

impl CisaKev {
    pub fn url(&self) -> &'static str {
        "https://www.cisa.gov/sites/default/files/feeds/known_exploited_vulnerabilities.json"
    }
}

#[async_trait]
impl Source for CisaKev {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "cisa_kev",
            name: "CISA Known Exploited Vulnerabilities",
            domain: EventKind::Cyber,
            // The catalog is updated at most a few times per day.
            cadence: Duration::from_secs(3600),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        let body = reqwest::get(self.url()).await?.text().await?;
        parse_cisa_kev(&body)
    }
}

/// Pure parser: CISA KEV catalog JSON -> events. Unit-tested offline.
pub fn parse_cisa_kev(json: &str) -> anyhow::Result<Vec<Event>> {
    let root: serde_json::Value = serde_json::from_str(json)?;
    let vulns = root
        .get("vulnerabilities")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("cisa_kev: missing 'vulnerabilities' array"))?;

    let mut out = Vec::with_capacity(vulns.len());
    for v in vulns {
        let cve = v.get("cveID").and_then(|c| c.as_str()).unwrap_or_default();
        if cve.is_empty() {
            // An entry with no CVE id has no stable identity; skip it.
            continue;
        }

        // dateAdded is a calendar date ("YYYY-MM-DD"); anchor it at UTC midnight.
        let time = v
            .get("dateAdded")
            .and_then(|d| d.as_str())
            .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .map(|dt| Utc.from_utc_datetime(&dt))
            .unwrap_or_else(Utc::now);

        // Prefer the human-readable name; fall back to vendor/product.
        let title = v
            .get("vulnerabilityName")
            .and_then(|n| n.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from)
            .unwrap_or_else(|| {
                let vendor = v.get("vendorProject").and_then(|x| x.as_str()).unwrap_or("");
                let product = v.get("product").and_then(|x| x.as_str()).unwrap_or("");
                format!("{vendor} {product}").trim().to_string()
            });

        // KEV carries no CVSS. Confirmed ransomware use is the strongest signal we
        // have that a vulnerability is being actively weaponized -> higher severity.
        let ransomware = v
            .get("knownRansomwareCampaignUse")
            .and_then(|r| r.as_str())
            .map(|s| s.eq_ignore_ascii_case("known"))
            .unwrap_or(false);
        let severity = Severity::new(if ransomware { 0.9 } else { 0.6 });

        out.push(Event {
            id: cve.to_string(),
            source_id: "cisa_kev".to_string(),
            kind: EventKind::Cyber,
            title,
            time,
            geo: None,
            severity,
            url: Some(format!("https://nvd.nist.gov/vuln/detail/{cve}")),
            raw: v.clone(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
      "title": "CISA Catalog of Known Exploited Vulnerabilities",
      "catalogVersion": "2024.01.01",
      "count": 3,
      "vulnerabilities": [
        {"cveID":"CVE-2021-27104","vendorProject":"Accellion","product":"FTA",
         "vulnerabilityName":"Accellion FTA OS Command Injection Vulnerability",
         "dateAdded":"2021-11-03","knownRansomwareCampaignUse":"Known"},
        {"cveID":"CVE-2022-0001","vendorProject":"Acme","product":"Widget",
         "vulnerabilityName":"","dateAdded":"2022-05-10",
         "knownRansomwareCampaignUse":"Unknown"},
        {"cveID":"","vendorProject":"Ghost","product":"NoId","dateAdded":"2023-01-01"}
      ]
    }"#;

    #[test]
    fn parses_fixture() {
        let ev = parse_cisa_kev(FIXTURE).unwrap();
        // The id-less third entry is dropped.
        assert_eq!(ev.len(), 2);

        assert_eq!(ev[0].id, "CVE-2021-27104");
        assert_eq!(ev[0].source_id, "cisa_kev");
        assert_eq!(ev[0].kind, EventKind::Cyber);
        assert!(ev[0].geo.is_none());
        assert_eq!(
            ev[0].url.as_deref(),
            Some("https://nvd.nist.gov/vuln/detail/CVE-2021-27104")
        );
        // Ransomware-linked -> elevated severity.
        assert!((ev[0].severity.value() - 0.9).abs() < 1e-9);
        assert_eq!(ev[0].time.format("%Y-%m-%d").to_string(), "2021-11-03");

        // Empty name falls back to "vendor product"; baseline severity.
        assert_eq!(ev[1].title, "Acme Widget");
        assert!((ev[1].severity.value() - 0.6).abs() < 1e-9);
    }

    #[test]
    fn errors_on_missing_array() {
        assert!(parse_cisa_kev(r#"{"title":"x"}"#).is_err());
    }
}
