//! `ee-sources` — pluggable data-source connectors. One self-contained file per
//! provider, each implementing [`ee_core::Source`].
//!
//! ## Adding a source
//! 1. Create `src/<provider>.rs` with a struct that `impl ee_core::Source`.
//! 2. Keep the wire-format parsing in a pure `fn parse_*(&str) -> Result<Vec<Event>>`
//!    so it can be unit-tested without the network.
//! 3. Add the module below and register a default instance in [`registry`].

pub mod acled;
pub mod alberta511;
pub mod cbsa_bwt;
pub mod cccs;
pub mod cisa_kev;
pub mod cwfis;
pub mod cwfis_activefires;
pub mod digitraffic_ais;
pub mod drivebc;
pub mod navcanada;
pub mod nhc;
pub mod ucdp_ged;
pub mod eccc_alerts;
pub mod eccc_aqhi;
pub mod eccc_marine;
pub mod emsc;
pub mod eonet;
pub mod firms;
pub mod eqcanada;
pub mod gdacs;
pub mod gvp_volcano;
pub mod healthmap;
pub mod jma_typhoon;
pub mod nws;
pub mod ontario511;
pub mod opensky;
pub mod quebec511;
pub mod usgs;
pub mod yahoo;

use ee_core::Source;

/// All key-free, ready-to-use sources. Extend as connectors land.
pub fn registry() -> Vec<Box<dyn Source>> {
    vec![
        Box::new(usgs::Usgs::default()),
        Box::new(cisa_kev::CisaKev),
        Box::new(cccs::Cccs),
        Box::new(gdacs::Gdacs),
        Box::new(nws::Nws),
        Box::new(opensky::OpenSky::default()),
        Box::new(yahoo::Yahoo::default()),
        Box::new(eonet::Eonet::default()),
        // Canada-specific geocoded feeds (NWS/USGS leave Canada sparse).
        Box::new(eccc_alerts::EcccAlerts),
        Box::new(eccc_aqhi::EcccAqhi),
        Box::new(eccc_marine::EcccMarine),
        Box::new(cwfis::Cwfis::default()),
        Box::new(cwfis_activefires::CwfisActiveFires),
        Box::new(eqcanada::EqCanada::default()),
        Box::new(ontario511::Ontario511),
        Box::new(drivebc::DriveBc),
        Box::new(alberta511::Alberta511),
        Box::new(quebec511::Quebec511),
        Box::new(cbsa_bwt::CbsaBwt),
        Box::new(navcanada::NavCanada),
        // Global feeds to densify the whole map (not just North America).
        Box::new(emsc::Emsc::default()),
        Box::new(gvp_volcano::GvpVolcano::default()),
        Box::new(healthmap::HealthMap::default()),
        Box::new(digitraffic_ais::DigitrafficAis), // Vessel layer (Baltic AIS)
        Box::new(ucdp_ged::UcdpGed),               // Conflict layer (georeferenced events)
        Box::new(nhc::Nhc),                        // Tropical cyclones (NOAA NHC, Atlantic/E-Pacific)
        Box::new(jma_typhoon::JmaTyphoon),         // Typhoons (JMA RSMC Tokyo, W-Pacific)
        // Credentialed global feeds (dormant until their key/account env is set).
        Box::new(firms::Firms::default()),
        Box::new(acled::Acled::default()),
    ]
}
