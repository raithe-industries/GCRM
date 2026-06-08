//! Base-map style set + 3D-globe-ready coordinate output.
//!
//! Every map frontend needs two things this module supplies, frontend-agnostically:
//!
//! 1. **A catalog of base-map styles** — the backdrop the data layers ride on. We
//!    reproduce the dashboards' base-style set entirely from *open* tile/render
//!    providers (CARTO, Esri, OpenTopoMap, OpenFreeMap, OpenStreetMap, Natural Earth
//!    projections, plus a 3D globe), each as a pure [`BaseStyle`] descriptor (id,
//!    label, provider, kind, theme, URL template, attribution, max zoom). No network
//!    here — just the descriptors a renderer needs to load a backdrop.
//!
//! 2. **Coordinate output** — the math to place data on those backdrops:
//!    - [`tile_index`] / [`BaseStyle::tile_url`]: the standard Web-Mercator "slippy
//!      map" transform (lat/lon/zoom → `{z}/{x}/{y}` tile) for the 2D raster styles;
//!    - [`project_to_globe`]: lat/lon → unit-sphere XYZ for the 3D globe styles
//!      (globe.gl / Three.js).
//!
//! Reproduces World Monitor / SitDeck's base-style layer — "**18 base-map styles incl.
//! 3D globe + non-Mercator projections** … all from open tile providers, reproducible
//! directly" (`sitdeck-features.md`; capability-map: *Map layers & presentation →
//! Base-map style set (open tile providers) + 3D-globe-ready coordinate output*).

use ee_core::Geo;
use serde::Serialize;

/// The open provider a [`BaseStyle`] comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Carto,
    Esri,
    OpenTopoMap,
    OpenFreeMap,
    OpenStreetMap,
    NaturalEarth,
}

/// How a style is rendered — which determines how its coordinates are produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StyleKind {
    /// Web-Mercator XYZ raster tiles (`{z}/{x}/{y}` PNG). Use [`tile_index`].
    Raster,
    /// Vector-tile style document (a MapLibre/Mapbox style JSON URL); the renderer
    /// fetches the style, not individual `{z}/{x}/{y}` tiles.
    Vector,
    /// A non-Mercator 2D projection (Equal Earth / Robinson / Mollweide) drawn
    /// client-side from Natural Earth data — no tile pyramid.
    Projection,
    /// A 3D globe textured backdrop (globe.gl / Three.js). Use [`project_to_globe`].
    Globe,
}

/// Visual theme — lets a UI group/sort styles (dark vs. light vs. imagery, …).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    Dark,
    Light,
    Neutral,
    Satellite,
    Terrain,
    Physical,
    Reference,
}

/// A single base-map style: everything a renderer needs to load a backdrop, with no
/// knowledge of any particular frontend.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct BaseStyle {
    /// Stable machine id (config key / serialization).
    pub id: &'static str,
    /// Human-readable label for a style picker.
    pub label: &'static str,
    pub provider: Provider,
    pub kind: StyleKind,
    pub theme: Theme,
    /// Resource template. For [`StyleKind::Raster`] this is an XYZ template with
    /// `{z}`/`{x}`/`{y}` (and optional `{s}` subdomain) placeholders; for
    /// [`StyleKind::Vector`] a style-JSON URL; empty for projections / globe (those
    /// have no remote tile resource of their own).
    pub url_template: &'static str,
    /// Required attribution string.
    pub attribution: &'static str,
    /// Maximum useful zoom level for the source.
    pub max_zoom: u8,
}

impl BaseStyle {
    /// Fill an XYZ raster template for one tile, returning `None` for non-raster
    /// styles (vector / projection / globe have no `{z}/{x}/{y}` tile URL).
    ///
    /// Placeholders are substituted *by name*, so providers that order their path as
    /// `{z}/{y}/{x}` (e.g. Esri) come out correct. `{s}` (subdomain) is filled with
    /// the first entry of [`Self::subdomains`].
    pub fn tile_url(&self, z: u32, x: u32, y: u32) -> Option<String> {
        if self.kind != StyleKind::Raster || self.url_template.is_empty() {
            return None;
        }
        let sub = self.subdomains().first().copied().unwrap_or("a");
        let url = self
            .url_template
            .replace("{s}", sub)
            .replace("{z}", &z.to_string())
            .replace("{x}", &x.to_string())
            .replace("{y}", &y.to_string());
        Some(url)
    }

    /// Subdomains a raster template's `{s}` may cycle through (for client-side load
    /// balancing). Empty when the template has no `{s}`.
    pub fn subdomains(&self) -> &'static [&'static str] {
        if self.url_template.contains("{s}") {
            &["a", "b", "c", "d"]
        } else {
            &[]
        }
    }

    /// Whether the style is the 3D globe backdrop (coordinates via [`project_to_globe`]).
    pub fn is_globe(&self) -> bool {
        self.kind == StyleKind::Globe
    }
}

/// The full open base-style catalog — 18 styles, mirroring the dashboards' set.
///
/// CARTO (5) · Esri (4) · OpenTopoMap (1) · OpenFreeMap (3) · OpenStreetMap (1) ·
/// Natural Earth projections (3) · 3D Globe (1).
pub const STYLES: &[BaseStyle] = &[
    // ── CARTO basemaps (Web-Mercator raster, free) ──────────────────────────────
    BaseStyle {
        id: "carto-dark-matter",
        label: "CARTO Dark Matter",
        provider: Provider::Carto,
        kind: StyleKind::Raster,
        theme: Theme::Dark,
        url_template: "https://{s}.basemaps.cartocdn.com/dark_all/{z}/{x}/{y}.png",
        attribution: "© OpenStreetMap contributors © CARTO",
        max_zoom: 20,
    },
    BaseStyle {
        id: "carto-dark",
        label: "CARTO Dark (no labels)",
        provider: Provider::Carto,
        kind: StyleKind::Raster,
        theme: Theme::Dark,
        url_template: "https://{s}.basemaps.cartocdn.com/dark_nolabels/{z}/{x}/{y}.png",
        attribution: "© OpenStreetMap contributors © CARTO",
        max_zoom: 20,
    },
    BaseStyle {
        id: "carto-positron",
        label: "CARTO Positron",
        provider: Provider::Carto,
        kind: StyleKind::Raster,
        theme: Theme::Light,
        url_template: "https://{s}.basemaps.cartocdn.com/light_all/{z}/{x}/{y}.png",
        attribution: "© OpenStreetMap contributors © CARTO",
        max_zoom: 20,
    },
    BaseStyle {
        id: "carto-light",
        label: "CARTO Light (no labels)",
        provider: Provider::Carto,
        kind: StyleKind::Raster,
        theme: Theme::Light,
        url_template: "https://{s}.basemaps.cartocdn.com/light_nolabels/{z}/{x}/{y}.png",
        attribution: "© OpenStreetMap contributors © CARTO",
        max_zoom: 20,
    },
    BaseStyle {
        id: "carto-voyager",
        label: "CARTO Voyager",
        provider: Provider::Carto,
        kind: StyleKind::Raster,
        theme: Theme::Neutral,
        url_template: "https://{s}.basemaps.cartocdn.com/rastertiles/voyager/{z}/{x}/{y}.png",
        attribution: "© OpenStreetMap contributors © CARTO",
        max_zoom: 20,
    },
    // ── Esri ArcGIS Online (raster; note {z}/{y}/{x} path order) ─────────────────
    BaseStyle {
        id: "esri-gray-canvas",
        label: "Esri Gray Canvas",
        provider: Provider::Esri,
        kind: StyleKind::Raster,
        theme: Theme::Neutral,
        url_template: "https://server.arcgisonline.com/ArcGIS/rest/services/Canvas/World_Light_Gray_Base/MapServer/tile/{z}/{y}/{x}",
        attribution: "Esri, HERE, Garmin, © OpenStreetMap contributors",
        max_zoom: 16,
    },
    BaseStyle {
        id: "esri-satellite",
        label: "Esri Satellite (Maxar)",
        provider: Provider::Esri,
        kind: StyleKind::Raster,
        theme: Theme::Satellite,
        url_template: "https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{z}/{y}/{x}",
        attribution: "Esri, Maxar, Earthstar Geographics, and the GIS User Community",
        max_zoom: 19,
    },
    BaseStyle {
        id: "esri-physical",
        label: "Esri Physical (NPS)",
        provider: Provider::Esri,
        kind: StyleKind::Raster,
        theme: Theme::Physical,
        url_template: "https://server.arcgisonline.com/ArcGIS/rest/services/World_Physical_Map/MapServer/tile/{z}/{y}/{x}",
        attribution: "Esri, US National Park Service",
        max_zoom: 8,
    },
    BaseStyle {
        id: "esri-natgeo",
        label: "Esri Nat Geo",
        provider: Provider::Esri,
        kind: StyleKind::Raster,
        theme: Theme::Reference,
        url_template: "https://server.arcgisonline.com/ArcGIS/rest/services/NatGeo_World_Map/MapServer/tile/{z}/{y}/{x}",
        attribution: "Esri, National Geographic, and the GIS User Community",
        max_zoom: 16,
    },
    // ── OpenTopoMap (raster) ─────────────────────────────────────────────────────
    BaseStyle {
        id: "opentopomap-terrain",
        label: "OpenTopoMap Terrain",
        provider: Provider::OpenTopoMap,
        kind: StyleKind::Raster,
        theme: Theme::Terrain,
        url_template: "https://{s}.tile.opentopomap.org/{z}/{x}/{y}.png",
        attribution: "© OpenStreetMap contributors, SRTM — map © OpenTopoMap (CC-BY-SA)",
        max_zoom: 17,
    },
    // ── OpenFreeMap (vector style JSON) ──────────────────────────────────────────
    BaseStyle {
        id: "openfreemap-positron",
        label: "OpenFreeMap Positron",
        provider: Provider::OpenFreeMap,
        kind: StyleKind::Vector,
        theme: Theme::Light,
        url_template: "https://tiles.openfreemap.org/styles/positron",
        attribution: "© OpenMapTiles © OpenStreetMap contributors",
        max_zoom: 20,
    },
    BaseStyle {
        id: "openfreemap-bright",
        label: "OpenFreeMap Bright",
        provider: Provider::OpenFreeMap,
        kind: StyleKind::Vector,
        theme: Theme::Light,
        url_template: "https://tiles.openfreemap.org/styles/bright",
        attribution: "© OpenMapTiles © OpenStreetMap contributors",
        max_zoom: 20,
    },
    BaseStyle {
        id: "openfreemap-liberty",
        label: "OpenFreeMap Liberty",
        provider: Provider::OpenFreeMap,
        kind: StyleKind::Vector,
        theme: Theme::Neutral,
        url_template: "https://tiles.openfreemap.org/styles/liberty",
        attribution: "© OpenMapTiles © OpenStreetMap contributors",
        max_zoom: 20,
    },
    // ── OpenStreetMap standard (raster) ──────────────────────────────────────────
    BaseStyle {
        id: "osm-standard",
        label: "OpenStreetMap Standard",
        provider: Provider::OpenStreetMap,
        kind: StyleKind::Raster,
        theme: Theme::Reference,
        url_template: "https://tile.openstreetmap.org/{z}/{x}/{y}.png",
        attribution: "© OpenStreetMap contributors",
        max_zoom: 19,
    },
    // ── Natural Earth non-Mercator projections (client-side, no tiles) ───────────
    BaseStyle {
        id: "natural-earth-equal-earth",
        label: "Natural Earth — Equal Earth",
        provider: Provider::NaturalEarth,
        kind: StyleKind::Projection,
        theme: Theme::Physical,
        url_template: "",
        attribution: "Made with Natural Earth",
        max_zoom: 8,
    },
    BaseStyle {
        id: "natural-earth-robinson",
        label: "Natural Earth — Robinson",
        provider: Provider::NaturalEarth,
        kind: StyleKind::Projection,
        theme: Theme::Physical,
        url_template: "",
        attribution: "Made with Natural Earth",
        max_zoom: 8,
    },
    BaseStyle {
        id: "natural-earth-mollweide",
        label: "Natural Earth — Mollweide",
        provider: Provider::NaturalEarth,
        kind: StyleKind::Projection,
        theme: Theme::Physical,
        url_template: "",
        attribution: "Made with Natural Earth",
        max_zoom: 8,
    },
    // ── 3D globe (globe.gl / Three.js) ───────────────────────────────────────────
    BaseStyle {
        id: "globe-3d",
        label: "3D Globe",
        provider: Provider::NaturalEarth,
        kind: StyleKind::Globe,
        theme: Theme::Satellite,
        url_template: "",
        attribution: "Made with Natural Earth · globe.gl / Three.js",
        max_zoom: 8,
    },
];

/// The whole open base-style catalog.
pub fn registry() -> &'static [BaseStyle] {
    STYLES
}

/// Look up a style by its stable id.
pub fn by_id(id: &str) -> Option<&'static BaseStyle> {
    STYLES.iter().find(|s| s.id == id)
}

/// The default backdrop — CARTO Dark Matter, the dashboards' canonical dark base.
pub fn default_style() -> &'static BaseStyle {
    &STYLES[0]
}

/// Number of tiles per axis at zoom `z` (`2^z`). Capped at `z = 31` to stay in range.
pub fn tile_count(z: u32) -> u64 {
    1u64 << z.min(31)
}

/// Web-Mercator "slippy map" transform: geographic `(lat, lon)` → `(x, y)` tile index
/// at zoom `z`.
///
/// Latitude is clamped to the Web-Mercator limit (±85.0511°) before projection;
/// longitude wraps into `[-180, 180)`. The returned indices are clamped to
/// `[0, 2^z − 1]`, so the result is always a valid tile address.
pub fn tile_index(lat: f64, lon: f64, z: u32) -> (u32, u32) {
    const MERC_MAX_LAT: f64 = 85.051_128_779_806_59;
    let n = tile_count(z) as f64;

    // Wrap longitude into [-180, 180).
    let mut lon = (lon + 180.0).rem_euclid(360.0) - 180.0;
    if lon == 180.0 {
        lon = -180.0;
    }
    let lat = lat.clamp(-MERC_MAX_LAT, MERC_MAX_LAT);
    let lat_rad = lat.to_radians();

    let x = (lon + 180.0) / 360.0 * n;
    let y = (1.0 - lat_rad.tan().asinh() / std::f64::consts::PI) / 2.0 * n;

    let max = (n as u32).saturating_sub(1);
    (
        (x.floor() as i64).clamp(0, max as i64) as u32,
        (y.floor() as i64).clamp(0, max as i64) as u32,
    )
}

/// A point on the unit (or radius-`r`) globe — right-handed ECEF-style frame:
/// `+x` through `(lat 0, lon 0)`, `+y` through `(lat 0, lon 90°E)`, `+z` through the
/// north pole. Ready to feed a globe.gl / Three.js scene.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct GlobePoint {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

/// Project `(lat, lon)` onto a sphere of the given `radius`, for 3D-globe rendering.
pub fn project_to_globe(geo: Geo, radius: f64) -> GlobePoint {
    let lat = geo.lat.to_radians();
    let lon = geo.lon.to_radians();
    let (clat, slat) = (lat.cos(), lat.sin());
    let (clon, slon) = (lon.cos(), lon.sin());
    GlobePoint {
        x: radius * clat * clon,
        y: radius * clat * slon,
        z: radius * slat,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_eighteen_unique_styles() {
        assert_eq!(STYLES.len(), 18);
        let mut ids: Vec<&str> = STYLES.iter().map(|s| s.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 18, "style ids must be unique");
    }

    #[test]
    fn catalog_covers_the_distinctive_styles() {
        // The non-Mercator projections and the 3D globe are the catalog's headline
        // differentiators — make sure they survive.
        let kinds = |k: StyleKind| STYLES.iter().filter(|s| s.kind == k).count();
        assert_eq!(kinds(StyleKind::Projection), 3); // Equal Earth / Robinson / Mollweide
        assert_eq!(kinds(StyleKind::Globe), 1);
        assert!(by_id("globe-3d").unwrap().is_globe());
        assert_eq!(default_style().id, "carto-dark-matter");
    }

    #[test]
    fn every_style_has_attribution_and_a_resource_when_expected() {
        for s in STYLES {
            assert!(!s.attribution.is_empty(), "{} missing attribution", s.id);
            match s.kind {
                // Raster/vector styles must carry a resource template;
                // projections/globe legitimately have none.
                StyleKind::Raster | StyleKind::Vector => {
                    assert!(!s.url_template.is_empty(), "{} missing url", s.id)
                }
                StyleKind::Projection | StyleKind::Globe => {
                    assert!(s.url_template.is_empty(), "{} should have no url", s.id)
                }
            }
        }
    }

    #[test]
    fn tile_url_only_for_raster_and_substitutes_by_name() {
        // Raster: placeholders filled, subdomain resolved.
        let carto = by_id("carto-dark-matter").unwrap();
        assert_eq!(
            carto.tile_url(5, 9, 12).unwrap(),
            "https://a.basemaps.cartocdn.com/dark_all/5/9/12.png"
        );

        // Esri orders the path {z}/{y}/{x} — substitution by name must respect it.
        let esri = by_id("esri-satellite").unwrap();
        let url = esri.tile_url(3, 4, 6).unwrap();
        assert!(url.ends_with("/tile/3/6/4"), "got {url}");

        // Non-raster styles have no XYZ tile URL.
        assert!(by_id("openfreemap-liberty").unwrap().tile_url(1, 1, 1).is_none());
        assert!(by_id("natural-earth-robinson").unwrap().tile_url(1, 1, 1).is_none());
        assert!(by_id("globe-3d").unwrap().tile_url(1, 1, 1).is_none());
    }

    #[test]
    fn subdomains_present_only_when_template_uses_them() {
        assert_eq!(by_id("carto-positron").unwrap().subdomains(), ["a", "b", "c", "d"]);
        // OSM standard template has no {s}.
        assert!(by_id("osm-standard").unwrap().subdomains().is_empty());
    }

    #[test]
    fn tile_count_is_power_of_two() {
        assert_eq!(tile_count(0), 1);
        assert_eq!(tile_count(1), 2);
        assert_eq!(tile_count(10), 1024);
    }

    #[test]
    fn tile_index_anchors() {
        // At z=0 the whole world is one tile.
        assert_eq!(tile_index(0.0, 0.0, 0), (0, 0));

        // z=1 splits into a 2×2 grid. The NW quadrant is (0,0); a point clearly in
        // the SE quadrant (south of equator, east of prime meridian) is (1,1).
        assert_eq!(tile_index(45.0, -90.0, 1), (0, 0));
        assert_eq!(tile_index(-45.0, 90.0, 1), (1, 1));

        // The equator/prime-meridian crossing sits on the (1,1) corner at z=1.
        assert_eq!(tile_index(0.0, 0.0, 1), (1, 1));
    }

    #[test]
    fn tile_index_clamps_poles_and_wraps_longitude() {
        let z = 4;
        let max = (tile_count(z) - 1) as u32;
        // Beyond the Mercator limit the y index saturates rather than blowing up.
        let (_, y_np) = tile_index(89.9, 0.0, z);
        let (_, y_sp) = tile_index(-89.9, 0.0, z);
        assert_eq!(y_np, 0);
        assert_eq!(y_sp, max);
        // Longitude wraps: +180 and -180 address the same column (0).
        assert_eq!(tile_index(0.0, 180.0, z).0, tile_index(0.0, -180.0, z).0);
        assert_eq!(tile_index(0.0, -180.0, z).0, 0);
    }

    #[test]
    fn globe_projection_anchors() {
        let r = 1.0;
        let approx = |a: f64, b: f64| (a - b).abs() < 1e-9;
        let p = |lat, lon| project_to_globe(Geo::new(lat, lon).unwrap(), r);

        // (0,0) → +x axis.
        let a = p(0.0, 0.0);
        assert!(approx(a.x, 1.0) && approx(a.y, 0.0) && approx(a.z, 0.0));
        // (0, 90E) → +y axis.
        let b = p(0.0, 90.0);
        assert!(approx(b.x, 0.0) && approx(b.y, 1.0) && approx(b.z, 0.0));
        // North pole → +z axis.
        let c = p(90.0, 0.0);
        assert!(approx(c.x, 0.0) && approx(c.y, 0.0) && approx(c.z, 1.0));
    }

    #[test]
    fn globe_projection_preserves_radius() {
        let r = 6371.0; // km — every projected point stays on the sphere.
        for &(lat, lon) in &[(12.3, 45.6), (-51.7, 170.0), (80.0, -120.0)] {
            let pt = project_to_globe(Geo::new(lat, lon).unwrap(), r);
            let mag = (pt.x * pt.x + pt.y * pt.y + pt.z * pt.z).sqrt();
            assert!((mag - r).abs() < 1e-6, "point off sphere: {mag}");
        }
    }
}
