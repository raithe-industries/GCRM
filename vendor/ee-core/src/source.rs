//! The pluggable [`Source`] contract. Implement it for any data provider and the
//! ingest engine — and the future dashboard — can consume it with no other changes.

use crate::event::{Event, EventKind};
use async_trait::async_trait;
use std::time::Duration;

/// Static description of a source. Keep `id` stable — emitted [`Event`]s reference it.
#[derive(Debug, Clone)]
pub struct SourceMeta {
    pub id: &'static str,
    pub name: &'static str,
    /// The primary domain this source covers.
    pub domain: EventKind,
    /// Suggested poll interval.
    pub cadence: Duration,
    /// Whether the provider requires an API key/credential.
    pub needs_key: bool,
}

/// A pluggable data provider. One implementation per provider, each in its own file.
#[async_trait]
pub trait Source: Send + Sync {
    /// Static metadata describing this source.
    fn meta(&self) -> SourceMeta;

    /// Fetch the current batch of events, normalized to [`Event`]. Should be
    /// side-effect free beyond the network read.
    async fn fetch(&self) -> anyhow::Result<Vec<Event>>;
}
