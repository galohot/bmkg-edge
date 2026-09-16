//! Earthquake routes over BMKG's three TEWS feeds.
//!
//! BMKG sends every number as a string: `"Magnitude":"6.2"`, `"Kedalaman":"145 km"`,
//! and one `"Coordinates":"-7.22,107.62"` field holding both latitude and longitude.
//! Everything here exists to turn that into numbers exactly once.

use serde::{Deserialize, Serialize};

use crate::api::{fetch_json, ApiError, ApiResult};
use crate::geo::haversine_km;

const BASE: &str = "https://data.bmkg.go.id/DataMKG/TEWS";

pub const TTL_LATEST: u32 = 60;
pub const TTL_RECENT: u32 = 300;
pub const TTL_FELT: u32 = 60;

// ---------------------------------------------------------------- upstream shape

#[derive(Deserialize)]
struct OneWrapper {
    #[serde(rename = "Infogempa")]
    info: OneInfo,
}
#[derive(Deserialize)]
struct OneInfo {
    gempa: Raw,
}

#[derive(Deserialize)]
struct ManyWrapper {
    #[serde(rename = "Infogempa")]
    info: ManyInfo,
}
#[derive(Deserialize)]
struct ManyInfo {
    gempa: Vec<Raw>,
}

#[derive(Deserialize)]
struct Raw {
    #[serde(rename = "Tanggal")]
    tanggal: String,
    #[serde(rename = "Jam")]
    jam: String,
    #[serde(rename = "DateTime")]
    datetime: String,
    #[serde(rename = "Coordinates")]
    coordinates: String,
    #[serde(rename = "Magnitude")]
    magnitude: String,
    #[serde(rename = "Kedalaman")]
    kedalaman: String,
    #[serde(rename = "Wilayah")]
    wilayah: String,
    #[serde(rename = "Potensi")]
    potensi: Option<String>,
    #[serde(rename = "Dirasakan")]
    dirasakan: Option<String>,
    #[serde(rename = "Shakemap")]
    shakemap: Option<String>,
}

// ---------------------------------------------------------------- our shape

#[derive(Serialize, Clone)]
pub struct Quake {
    /// ISO 8601 with offset, straight from BMKG's own `DateTime`.
    pub datetime: String,
    /// The same instant as BMKG prints it for an Indonesian reader.
    pub date_local: String,
    pub time_local: String,
    pub latitude: f64,
    pub longitude: f64,
    pub magnitude: f64,
    pub depth_km: f64,
    pub region: String,
    /// Present on the latest/recent feeds: BMKG's tsunami wording.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tsunami_potential: Option<String>,
    /// Present on the felt feed: MMI intensities per place.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub felt_intensity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shakemap_url: Option<String>,
    /// Only set by `/nearby`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance_km: Option<f64>,
}

impl Raw {
    fn parse(self) -> ApiResult<Quake> {
        let (lat, lon) = self
            .coordinates
            .split_once(',')
            .ok_or_else(|| {
                ApiError::UpstreamParse(format!(
                    "Coordinates was {:?}, expected \"lat,lon\"",
                    self.coordinates
                ))
            })
            .and_then(|(a, b)| {
                let lat = a.trim().parse::<f64>().map_err(|e| {
                    ApiError::UpstreamParse(format!("latitude {:?}: {e}", a.trim()))
                })?;
                let lon = b.trim().parse::<f64>().map_err(|e| {
                    ApiError::UpstreamParse(format!("longitude {:?}: {e}", b.trim()))
                })?;
                Ok((lat, lon))
            })?;

        let magnitude = self
            .magnitude
            .trim()
            .parse::<f64>()
            .map_err(|e| ApiError::UpstreamParse(format!("Magnitude {:?}: {e}", self.magnitude)))?;

        // "145 km" -> 145.0. BMKG has also used "10 Km" and "5  km" over the years,
        // so take the leading number rather than stripping a fixed suffix.
        let depth_km = self
            .kedalaman
            .trim()
            .split_whitespace()
            .next()
            .and_then(|n| n.replace(',', ".").parse::<f64>().ok())
            .ok_or_else(|| {
                ApiError::UpstreamParse(format!("Kedalaman {:?} has no leading number", self.kedalaman))
            })?;

        Ok(Quake {
            datetime: self.datetime,
            date_local: self.tanggal,
            time_local: self.jam,
            latitude: lat,
            longitude: lon,
            magnitude,
            depth_km,
            region: self.wilayah,
            tsunami_potential: self.potensi,
            felt_intensity: self.dirasakan,
            shakemap_url: self.shakemap.map(|f| format!("{BASE}/{f}")),
            distance_km: None,
        })
    }
}

// ---------------------------------------------------------------- handlers

pub async fn latest() -> ApiResult<Quake> {
    let w: OneWrapper = fetch_json(&format!("{BASE}/autogempa.json"), TTL_LATEST).await?;
    w.info.gempa.parse()
}

pub async fn recent() -> ApiResult<Vec<Quake>> {
    let w: ManyWrapper = fetch_json(&format!("{BASE}/gempaterkini.json"), TTL_RECENT).await?;
    w.info.gempa.into_iter().map(Raw::parse).collect()
}

pub async fn felt() -> ApiResult<Vec<Quake>> {
    let w: ManyWrapper = fetch_json(&format!("{BASE}/gempadirasakan.json"), TTL_FELT).await?;
    w.info.gempa.into_iter().map(Raw::parse).collect()
}

/// Everything within `radius_km` of a point, nearest first.
///
/// The search set is recent ∪ felt, de-duplicated by timestamp: BMKG publishes the
/// same event to both feeds when a large quake is also felt, and a caller asking
/// "what happened near me" should see it once.
pub async fn nearby(lat: f64, lon: f64, radius_km: f64) -> ApiResult<Vec<Quake>> {
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return Err(ApiError::BadRequest(format!(
            "lat must be -90..90 and lon -180..180; got {lat},{lon}"
        )));
    }
    if radius_km <= 0.0 || radius_km > 20_000.0 {
        return Err(ApiError::BadRequest(format!(
            "radius_km must be between 0 and 20000; got {radius_km}"
        )));
    }

    let mut all = recent().await?;
    let mut seen: Vec<String> = all.iter().map(|q| q.datetime.clone()).collect();
    for q in felt().await? {
        if !seen.contains(&q.datetime) {
            seen.push(q.datetime.clone());
            all.push(q);
        }
    }

    let mut hits: Vec<Quake> = all
        .into_iter()
        .filter_map(|mut q| {
            let d = haversine_km(lat, lon, q.latitude, q.longitude);
            if d <= radius_km {
                q.distance_km = Some((d * 10.0).round() / 10.0);
                Some(q)
            } else {
                None
            }
        })
        .collect();

    hits.sort_by(|a, b| {
        a.distance_km
            .unwrap_or(f64::MAX)
            .partial_cmp(&b.distance_km.unwrap_or(f64::MAX))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(hits)
}
