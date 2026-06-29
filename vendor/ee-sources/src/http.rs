//! Shared HTTP client + helpers for all connectors: one pooled, timeout-bounded reqwest
//! Client, a hard status check, and a response-body size cap. Replaces the per-call
//! Client::builder() each connector used to construct (no pooling, no timeout) and the bare
//! reqwest::get() in usgs/cisa_kev. (audit xcut_net-1/2/5, ee_sources_net-1, xcut_err-4)
use std::sync::OnceLock;
use std::time::Duration;
use reqwest::Client;

const UA: &str = "engineering-effects/0.1 (+https://raithe.ca)";
/// Hard ceiling on any single feed response body (declared via Content-Length). 32 MB is
/// generous for these XML/JSON/CSV feeds and bounds memory against a misbehaving upstream.
const MAX_BODY_BYTES: u64 = 32 * 1024 * 1024;

/// Process-wide pooled client: connection reuse + a connect timeout + a total timeout, so no
/// fetch can hang forever even off the map path.
pub fn client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .user_agent(UA)
            .build()
            .expect("ee-sources shared HTTP client")
    })
}

/// Status-check + body-size-cap a response. Returns the response for the caller to read, or a
/// clear `HTTP <code>` error that the feed-health/last-good layer can distinguish from an
/// empty window. Use this when you need custom request headers (build the request from
/// `client()` yourself, then pass the response here).
pub fn checked(resp: reqwest::Response) -> anyhow::Result<reqwest::Response> {
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("HTTP {status}");
    }
    if resp.content_length().is_some_and(|l| l > MAX_BODY_BYTES) {
        anyhow::bail!("response body too large ({} bytes)", resp.content_length().unwrap_or(0));
    }
    Ok(resp)
}

/// GET → status/body check → body text. The common path for connectors with no custom headers.
pub async fn fetch_text(url: &str) -> anyhow::Result<String> {
    let resp = checked(client().get(url).send().await?)?;
    Ok(resp.text().await?)
}

/// GET → status/body check → parsed JSON.
pub async fn fetch_json<T: serde::de::DeserializeOwned>(url: &str) -> anyhow::Result<T> {
    let resp = checked(client().get(url).send().await?)?;
    Ok(resp.json().await?)
}
