//! Distance on a sphere. Used by `/v1/earthquake/nearby`.

const EARTH_RADIUS_KM: f64 = 6371.0088;

/// Great-circle distance in kilometres.
///
/// Haversine rather than a flat approximation: Indonesia spans 5,000 km of longitude,
/// and a flat formula is wrong by tens of kilometres at the eastern end.
pub fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dp = (lat2 - lat1).to_radians();
    let dl = (lon2 - lon1).to_radians();

    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_KM * a.sqrt().asin()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jakarta_to_surabaya_is_about_660km() {
        let d = haversine_km(-6.2088, 106.8456, -7.2575, 112.7521);
        assert!((d - 660.0).abs() < 15.0, "got {d}");
    }

    #[test]
    fn zero_distance() {
        assert!(haversine_km(-6.2, 106.8, -6.2, 106.8) < 1e-9);
    }
}
