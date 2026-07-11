//! `ee-sources` — pluggable data-source connectors. One self-contained file per
//! provider, each implementing [`ee_core::Source`].
//!
//! ## Adding a source
//! 1. Create `src/<provider>.rs` with a struct that `impl ee_core::Source`.
//! 2. Keep the wire-format parsing in a pure `fn parse_*(&str) -> Result<Vec<Event>>`
//!    so it can be unit-tested without the network.
//! 3. Add the module below and register a default instance in [`registry`].

pub mod http;

pub mod acled;
pub mod acled_aggregated;
pub mod alberta511;
pub mod asam;
pub mod avalanche_ca;
pub mod awc_sigmet;
pub mod bmkg_quake;
pub mod cbsa_bwt;
pub mod cccs;
pub mod cisa_kev;
pub mod cwfis;
pub mod cwfis_activefires;
pub mod digitraffic_ais;
pub mod drivebc;
pub mod ea_flood;
pub mod navcanada;
pub mod nhc;
pub mod nsw_rfs;
pub mod nwps_flood;
pub mod portwatch_chokepoints;
pub mod spc_storm_reports;
pub mod stuk_radiation;
pub mod teleray;
pub mod ucdp_ged;
pub mod usgs_volcano;
pub mod vigicrues;
pub mod wa_dfes;
pub mod eccc_alerts;
pub mod eccc_aqhi;
pub mod eccc_marine;
pub mod emsc;
pub mod eonet;
pub mod firms;
pub mod eqcanada;
pub mod gdacs;
pub mod geonet_quake;
pub mod geonet_volcano;
pub mod gvp_volcano;
pub mod healthmap;
pub mod jma_quake;
pub mod jma_typhoon;
pub mod magma_volcano;
pub mod nws;
pub mod odlinfo;
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
        Box::new(bmkg_quake::BmkgQuake), // Indonesia felt earthquakes + tsunami potential (BMKG/InaTEWS)
        Box::new(jma_quake::JmaQuake), // Japan seismic-intensity (Shindo) earthquakes (JMA)
        Box::new(geonet_quake::GeonetQuake), // NZ felt earthquakes graded by MMI (GeoNet/GNS)
        Box::new(ontario511::Ontario511),
        Box::new(drivebc::DriveBc),
        Box::new(alberta511::Alberta511),
        Box::new(quebec511::Quebec511),
        Box::new(cbsa_bwt::CbsaBwt),
        Box::new(navcanada::NavCanada),
        // Global feeds to densify the whole map (not just North America).
        Box::new(emsc::Emsc::default()),
        Box::new(gvp_volcano::GvpVolcano::default()),
        Box::new(geonet_volcano::GeonetVolcano), // NZ volcanic alert levels (GeoNet/GNS)
        Box::new(usgs_volcano::UsgsVolcano), // US/Alaska volcanic alert levels (USGS HANS)
        Box::new(magma_volcano::MagmaVolcano), // Indonesia volcanic alert levels (PVMBG/MAGMA, Path-B snapshot)
        Box::new(healthmap::HealthMap::default()),
        Box::new(digitraffic_ais::DigitrafficAis), // Vessel layer (Baltic AIS)
        Box::new(portwatch_chokepoints::PortwatchChokepoints), // Vessel layer (IMF PortWatch chokepoint transit disruption, global)
        Box::new(asam::Asam), // Vessel layer (NGA anti-shipping hostile-act reports, global)
        Box::new(ucdp_ged::UcdpGed),               // Conflict layer (georeferenced events)
        Box::new(acled_aggregated::AcledAggregated), // Conflict layer (ACLED weekly Admin-1 intensity, Path-B snapshot)
        Box::new(nhc::Nhc),                        // Tropical cyclones (NOAA NHC, Atlantic/E-Pacific)
        Box::new(jma_typhoon::JmaTyphoon),         // Typhoons (JMA RSMC Tokyo, W-Pacific)
        Box::new(nwps_flood::NwpsFlood),           // River flooding (NOAA NWPS, observed flood category)
        Box::new(ea_flood::EaFlood),               // UK flood warnings (EA, national severity level 1–3, England)
        Box::new(vigicrues::Vigicrues),            // France flood-vigilance levels (Vigicrues, national 1–4 scale)
        Box::new(avalanche_ca::AvalancheCa),       // Avalanche danger ratings (Avalanche Canada, seasonal)
        Box::new(awc_sigmet::AwcSigmet),           // International SIGMETs (NOAA AWC, en-route aviation hazards)
        Box::new(spc_storm_reports::SpcStormReports), // Severe-storm reports (NOAA SPC, confirmed tornado/hail/wind)
        Box::new(odlinfo::Odlinfo), // Radiation: gamma dose rate above natural background (BfS ODL, Germany)
        Box::new(stuk_radiation::StukRadiation), // Radiation: external dose rate above background (STUK/FMI, Finland)
        Box::new(teleray::Teleray), // Radiation: ambient gamma dose rate above background (IRSN/ASNR Téléray, France)
        Box::new(nsw_rfs::NswRfs), // Wildfire layer: NSW RFS major fire/emergency incidents + official alert levels (Australia)
        Box::new(wa_dfes::WaDfes), // Emergency-warning layer: WA DFES all-hazard warnings + AWS warning levels (Western Australia)
        // Credentialed global feeds (dormant until their key/account env is set).
        Box::new(firms::Firms::default()),
        Box::new(acled::Acled::default()),
    ]
}
