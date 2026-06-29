//! Geospatial primitives: a validated coordinate, a bounding box, and a named region.

use serde::{Deserialize, Serialize};

/// A WGS84 coordinate. Construct via [`Geo::new`], which validates the ranges.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Geo {
    pub lat: f64,
    pub lon: f64,
}

impl Geo {
    /// Returns `Some` only for in-range coordinates: lat ∈ [-90, 90], lon ∈ [-180, 180].
    pub fn new(lat: f64, lon: f64) -> Option<Self> {
        if (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon) {
            Some(Self { lat, lon })
        } else {
            None
        }
    }

    /// Great-circle distance to another point, in kilometres (haversine).
    pub fn haversine_km(&self, other: &Geo) -> f64 {
        const R: f64 = 6371.0088; // mean Earth radius, km
        let (la1, la2) = (self.lat.to_radians(), other.lat.to_radians());
        let dla = (other.lat - self.lat).to_radians();
        let dlo = (other.lon - self.lon).to_radians();
        let a = (dla / 2.0).sin().powi(2) + la1.cos() * la2.cos() * (dlo / 2.0).sin().powi(2);
        // atan2 form, not 2*R*asin(sqrt(a)): floating error can push `a` just above 1.0 for
        // near-antipodal points, and asin(>1) is NaN — which would then NaN-poison any
        // distance-keyed proximity/clustering math. atan2 is total over all inputs. (audit ee_core_cargo-2)
        2.0 * R * a.sqrt().atan2((1.0 - a).max(0.0).sqrt())
    }
}

/// An axis-aligned bounding box (inclusive bounds).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BBox {
    pub min_lat: f64,
    pub min_lon: f64,
    pub max_lat: f64,
    pub max_lon: f64,
}

impl BBox {
    pub fn contains(&self, g: &Geo) -> bool {
        (self.min_lat..=self.max_lat).contains(&g.lat)
            && (self.min_lon..=self.max_lon).contains(&g.lon)
    }
}

/// A named region (a labelled bounding box) — used for filtering and briefings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Region {
    pub name: String,
    pub bbox: BBox,
}

impl Region {
    pub fn contains(&self, g: &Geo) -> bool {
        self.bbox.contains(g)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_out_of_range_coords() {
        assert!(Geo::new(91.0, 0.0).is_none());
        assert!(Geo::new(0.0, 181.0).is_none());
        assert!(Geo::new(45.0, -120.0).is_some());
    }

    #[test]
    fn haversine_is_reasonable() {
        // Paris -> London is ~344 km.
        let paris = Geo::new(48.8566, 2.3522).unwrap();
        let london = Geo::new(51.5074, -0.1278).unwrap();
        let d = paris.haversine_km(&london);
        assert!((300.0..400.0).contains(&d), "got {d}");
    }

    #[test]
    fn haversine_finite_at_antipode() {
        // Near-antipodal points can push the haversine `a` term just above 1.0; the asin
        // form would return NaN there. atan2 stays finite ~half Earth's circumference. (audit ee_core_cargo-2)
        let a = Geo::new(0.0, 0.0).unwrap();
        let b = Geo::new(0.0, 180.0).unwrap();
        let d = a.haversine_km(&b);
        assert!(d.is_finite(), "antipodal distance must be finite, got {d}");
        assert!((19_000.0..21_000.0).contains(&d), "got {d}");
    }

    #[test]
    fn bbox_contains_works() {
        let b = BBox { min_lat: 40.0, min_lon: -10.0, max_lat: 55.0, max_lon: 10.0 };
        assert!(b.contains(&Geo::new(48.0, 2.0).unwrap()));
        assert!(!b.contains(&Geo::new(60.0, 2.0).unwrap()));
    }
}
