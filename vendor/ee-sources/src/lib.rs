//! `ee-sources` — pluggable data-source connectors. One self-contained file per
//! provider, each implementing [`ee_core::Source`].
//!
//! ## Adding a source
//! 1. Create `src/<provider>.rs` with a struct that `impl ee_core::Source`.
//! 2. Keep the wire-format parsing in a pure `fn parse_*(&str) -> Result<Vec<Event>>`
//!    so it can be unit-tested without the network.
//! 3. Add the module below and register a default instance in [`registry`].

pub mod cisa_kev;
pub mod cwfis;
pub mod eccc_alerts;
pub mod eccc_aqhi;
pub mod emsc;
pub mod eonet;
pub mod eqcanada;
pub mod gdacs;
pub mod gvp_volcano;
pub mod healthmap;
pub mod nws;
pub mod opensky;
pub mod usgs;
pub mod yahoo;

use ee_core::Source;

/// All key-free, ready-to-use sources. Extend as connectors land.
pub fn registry() -> Vec<Box<dyn Source>> {
    vec![
        Box::new(usgs::Usgs::default()),
        Box::new(cisa_kev::CisaKev),
        Box::new(gdacs::Gdacs),
        Box::new(nws::Nws),
        Box::new(opensky::OpenSky::default()),
        Box::new(yahoo::Yahoo::default()),
        Box::new(eonet::Eonet::default()),
        // Canada-specific geocoded feeds (NWS/USGS leave Canada sparse).
        Box::new(eccc_alerts::EcccAlerts),
        Box::new(eccc_aqhi::EcccAqhi),
        Box::new(cwfis::Cwfis::default()),
        Box::new(eqcanada::EqCanada::default()),
        // Global feeds to densify the whole map (not just North America).
        Box::new(emsc::Emsc::default()),
        Box::new(gvp_volcano::GvpVolcano::default()),
        Box::new(healthmap::HealthMap::default()),
    ]
}
