//! `ee-core` — shared domain primitives plus the pluggable [`Source`] contract for
//! the engineering-effects situational-awareness module library.
//!
//! Everything else in the workspace (every connector, the ingest engine, the AI
//! briefer, and the future dashboard) depends only on these types — so they stay
//! decoupled and individually testable.

pub mod event;
pub mod geo;
pub mod source;

pub use event::{Event, EventKind, Severity};
pub use geo::{BBox, Geo, Region};
pub use source::{Source, SourceMeta};
