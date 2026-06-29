//! Yahoo Finance markets — free, no API key.
//!
//! Reads Yahoo's open chart endpoint
//! (`https://query1.finance.yahoo.com/v8/finance/chart/<SYMBOL>`) for a basket of
//! instruments and emits one [`EventKind::Market`] [`Event`] per symbol, carrying
//! the day's move. This is the market stream that feeds `ee-correlate`'s Finance
//! Radar (equities / crypto / commodities / energy / bonds / forex / macro).
//!
//! (Stooq was the original plan, but it now gates its CSV behind a JavaScript
//! proof-of-work challenge — unusable headless — so we read Yahoo's open chart API.)

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use ee_core::{Event, EventKind, Severity, Source, SourceMeta};
use std::time::Duration;

/// One tracked instrument: a Yahoo symbol plus a human label fallback.
#[derive(Clone)]
pub struct Symbol {
    pub ticker: &'static str,
    pub label: &'static str,
}

/// The default cross-segment basket — one representative per Finance Radar spoke,
/// so a single fetch lights up every market segment.
pub const DEFAULT_BASKET: &[Symbol] = &[
    Symbol { ticker: "^GSPC", label: "S&P 500 (equities)" },
    Symbol { ticker: "^IXIC", label: "Nasdaq Composite (equities)" },
    Symbol { ticker: "BTC-USD", label: "Bitcoin (crypto)" },
    Symbol { ticker: "ETH-USD", label: "Ethereum (crypto)" },
    Symbol { ticker: "CL=F", label: "Crude Oil (energy)" },
    Symbol { ticker: "GC=F", label: "Gold (commodities)" },
    Symbol { ticker: "^TNX", label: "10Y Treasury Yield (bonds)" },
    Symbol { ticker: "EURUSD=X", label: "EUR/USD (forex)" },
    // (No volatility index here: its % move isn't comparable to a cash instrument's,
    // and the Finance Radar's Macro spoke is better fed by macro *news* — see the
    // `finance_radar` example and `SegmentLexicon::with_keywords`.)
];

/// Yahoo Finance markets source over a configurable basket of symbols.
pub struct Yahoo {
    pub basket: Vec<Symbol>,
}

impl Default for Yahoo {
    fn default() -> Self {
        Self { basket: DEFAULT_BASKET.to_vec() }
    }
}

impl Yahoo {
    pub fn chart_url(ticker: &str) -> String {
        // URL-encode the few symbols that carry reserved characters (^, =).
        let enc = ticker.replace('^', "%5E").replace('=', "%3D");
        format!("https://query1.finance.yahoo.com/v8/finance/chart/{enc}?interval=1d&range=1d")
    }
}

#[async_trait]
impl Source for Yahoo {
    fn meta(&self) -> SourceMeta {
        SourceMeta {
            id: "yahoo",
            name: "Yahoo Finance Markets",
            domain: EventKind::Market,
            cadence: Duration::from_secs(120),
            needs_key: false,
        }
    }

    async fn fetch(&self) -> anyhow::Result<Vec<Event>> {
        let mut out = Vec::with_capacity(self.basket.len());
        for sym in &self.basket {
            let body = crate::http::fetch_text(&Yahoo::chart_url(sym.ticker)).await?;
            // One bad symbol must not sink the whole basket.
            if let Ok(Some(ev)) = parse_yahoo_chart(&body, sym.label) {
                out.push(ev);
            }
        }
        Ok(out)
    }
}

/// Pure parser: one symbol's Yahoo chart JSON -> at most one Market event.
/// `label` is a human-readable fallback/segment hint used when Yahoo omits a name.
/// Unit-tested offline.
pub fn parse_yahoo_chart(json: &str, label: &str) -> anyhow::Result<Option<Event>> {
    let root: serde_json::Value = serde_json::from_str(json)?;
    let meta = match root
        .get("chart")
        .and_then(|c| c.get("result"))
        .and_then(|r| r.as_array())
        .and_then(|r| r.first())
        .and_then(|r| r.get("meta"))
    {
        Some(m) => m,
        None => return Ok(None), // empty/again-later response
    };

    let symbol = meta.get("symbol").and_then(|v| v.as_str()).unwrap_or(label);
    let price = match meta.get("regularMarketPrice").and_then(|v| v.as_f64()) {
        Some(p) => p,
        None => return Ok(None),
    };
    let prev = meta
        .get("chartPreviousClose")
        .or_else(|| meta.get("previousClose"))
        .and_then(|v| v.as_f64())
        .unwrap_or(price);

    let pct = if prev != 0.0 { (price - prev) / prev * 100.0 } else { 0.0 };
    let name = meta
        .get("shortName")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(label);

    let time = meta
        .get("regularMarketTime")
        .and_then(|v| v.as_i64())
        .and_then(|s| Utc.timestamp_opt(s, 0).single())
        .unwrap_or_else(Utc::now);

    // The radar derives its own composite; per-event severity tracks move size
    // (a 10% daily move saturates to 1.0).
    let severity = Severity::new((pct.abs() / 10.0).min(1.0));

    // Title carries the label (segment keywords) + signed move, e.g.
    // "S&P 500 (equities) -2.64% [7383.74]".
    let title = format!("{name} · {label} {pct:+.2}% [{price}]");

    Ok(Some(Event {
        id: format!("yahoo-{symbol}"),
        source_id: "yahoo".to_string(),
        kind: EventKind::Market,
        title,
        time,
        geo: None,
        severity,
        url: Some(format!("https://finance.yahoo.com/quote/{symbol}")),
        raw: meta.clone(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
      "chart": { "result": [ { "meta": {
        "symbol": "^GSPC",
        "shortName": "S&P 500",
        "regularMarketPrice": 7383.74,
        "chartPreviousClose": 7584.31,
        "regularMarketTime": 1780922631,
        "currency": "USD"
      } } ], "error": null }
    }"#;

    #[test]
    fn parses_fixture() {
        let ev = parse_yahoo_chart(FIXTURE, "S&P 500 (equities)").unwrap().unwrap();
        assert_eq!(ev.id, "yahoo-^GSPC");
        assert_eq!(ev.kind, EventKind::Market);
        assert!(ev.geo.is_none());
        assert!(ev.title.contains("S&P 500"));
        assert!(ev.title.contains("equities"));
        // (7383.74 - 7584.31) / 7584.31 * 100 = -2.644%
        assert!(ev.title.contains("-2.64%"), "title was {}", ev.title);
        assert!((ev.severity.value() - 0.2644).abs() < 0.01);
        assert_eq!(
            ev.url.as_deref(),
            Some("https://finance.yahoo.com/quote/^GSPC")
        );
    }

    #[test]
    fn empty_result_is_none() {
        assert!(parse_yahoo_chart(r#"{"chart":{"result":[],"error":null}}"#, "x")
            .unwrap()
            .is_none());
    }

    #[test]
    fn url_encodes_reserved_symbols() {
        assert!(Yahoo::chart_url("^GSPC").contains("%5EGSPC"));
        assert!(Yahoo::chart_url("EURUSD=X").contains("EURUSD%3DX"));
    }
}
