//! Weather forecast routes.
//!
//! BMKG keys forecasts by `adm4`, the Kemendagri village code, and nests the payload
//! three deep: `data[0].cuaca` is an array of days, each an array of ~3-hourly slots.
//! We flatten that once, here, so no caller has to.

use serde::{Deserialize, Serialize};
use worker::D1Database;

use crate::api::{fetch_json, ApiError, ApiResult};
use crate::timeutil::iso_to_epoch_ms;
use crate::wilayah::{self, Region};

const BASE: &str = "https://api.bmkg.go.id/publik/prakiraan-cuaca";
pub const TTL: u32 = 600;

// ---------------------------------------------------------------- upstream shape

#[derive(Deserialize)]
struct RawResponse {
    lokasi: RawLocation,
    data: Vec<RawData>,
}

#[derive(Deserialize)]
struct RawData {
    cuaca: Vec<Vec<RawSlot>>,
}

#[derive(Deserialize)]
struct RawLocation {
    adm1: String,
    adm2: String,
    adm3: String,
    adm4: String,
    provinsi: String,
    kotkab: String,
    kecamatan: String,
    desa: String,
    lon: f64,
    lat: f64,
    timezone: String,
}

#[derive(Deserialize)]
struct RawSlot {
    datetime: String,
    local_datetime: Option<String>,
    t: f64,
    tcc: Option<f64>,
    tp: Option<f64>,
    weather: Option<i64>,
    weather_desc: Option<String>,
    weather_desc_en: Option<String>,
    wd_deg: Option<f64>,
    wd: Option<String>,
    wd_to: Option<String>,
    ws: Option<f64>,
    hu: Option<f64>,
    vs: Option<f64>,
    vs_text: Option<String>,
    image: Option<String>,
}

// ---------------------------------------------------------------- our shape

#[derive(Serialize, Clone)]
pub struct Location {
    pub adm1: String,
    pub adm2: String,
    pub adm3: String,
    pub adm4: String,
    pub province: String,
    pub city: String,
    pub district: String,
    pub village: String,
    pub latitude: f64,
    pub longitude: f64,
    pub timezone: String,
}

#[derive(Serialize, Clone)]
pub struct Slot {
    /// UTC, ISO 8601.
    pub datetime: String,
    /// The same instant in the location's own timezone, as BMKG prints it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub datetime_local: Option<String>,
    /// 0 = today, 1 = tomorrow, 2 = the day after — BMKG publishes three.
    pub day_index: usize,
    pub temperature_c: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub humidity_percent: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cloud_cover_percent: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub precipitation_mm: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wind_speed_kmh: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wind_from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wind_to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wind_degrees: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility_m: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weather_code: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weather: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weather_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon_url: Option<String>,
}

#[derive(Serialize)]
pub struct Forecast {
    pub location: Location,
    pub slots: Vec<Slot>,
}

#[derive(Serialize)]
pub struct Current {
    pub location: Location,
    /// The slot covering right now — or the next one, if BMKG's window has moved on.
    pub now: Slot,
    /// The following slots, so a caller can say "and later today".
    pub next: Vec<Slot>,
}

#[derive(Serialize)]
pub struct Resolved {
    /// Which region the free-text query was understood as.
    pub matched: Region,
    /// Other regions that matched, so a caller can tell a wrong guess from a right one.
    pub alternatives: Vec<Region>,
    pub forecast: Forecast,
}

impl From<RawLocation> for Location {
    fn from(l: RawLocation) -> Self {
        Location {
            adm1: l.adm1,
            adm2: l.adm2,
            adm3: l.adm3,
            adm4: l.adm4,
            province: l.provinsi,
            city: l.kotkab,
            district: l.kecamatan,
            village: l.desa,
            latitude: l.lat,
            longitude: l.lon,
            timezone: l.timezone,
        }
    }
}

impl RawSlot {
    fn parse(self, day_index: usize) -> Slot {
        Slot {
            datetime: self.datetime,
            datetime_local: self.local_datetime,
            day_index,
            temperature_c: self.t,
            humidity_percent: self.hu,
            cloud_cover_percent: self.tcc,
            precipitation_mm: self.tp,
            wind_speed_kmh: self.ws,
            wind_from: self.wd,
            wind_to: self.wd_to,
            wind_degrees: self.wd_deg,
            visibility_m: self.vs,
            visibility_text: self.vs_text,
            weather_code: self.weather,
            weather: self.weather_desc_en,
            weather_id: self.weather_desc,
            icon_url: self.image,
        }
    }
}

// ---------------------------------------------------------------- handlers

pub async fn forecast(adm4: &str) -> ApiResult<Forecast> {
    wilayah::validate_code(adm4)?;
    if adm4.split('.').count() != 4 {
        return Err(ApiError::BadRequest(format!(
            "weather needs a level-4 village code like 31.74.04.1006, got {adm4:?}"
        )));
    }

    let raw: RawResponse = fetch_json(&format!("{BASE}?adm4={adm4}"), TTL)
        .await
        .map_err(|e| match e {
            // BMKG answers 404 with a JSON body, so make the echo useful.
            ApiError::UpstreamNotFound(_) => ApiError::UpstreamNotFound(format!(
                "BMKG has no forecast for {adm4}. The code may be valid but unmonitored; \
                 try a neighbouring village from /v1/wilayah/villages"
            )),
            other => other,
        })?;

    let day_arrays = raw
        .data
        .into_iter()
        .next()
        .ok_or_else(|| {
            ApiError::UpstreamParse(format!("BMKG returned no forecast block for {adm4}"))
        })?
        .cuaca;

    let mut slots = Vec::new();
    for (day_index, day) in day_arrays.into_iter().enumerate() {
        for s in day {
            slots.push(s.parse(day_index));
        }
    }
    if slots.is_empty() {
        return Err(ApiError::UpstreamParse(format!(
            "BMKG returned an empty forecast for {adm4}"
        )));
    }

    Ok(Forecast {
        location: raw.lokasi.into(),
        slots,
    })
}

/// The slot covering now, plus the next three.
///
/// "Covering now" means the latest slot that has already started. If every slot is in
/// the future — BMKG republished and dropped the past ones — the earliest is used.
pub async fn current(adm4: &str) -> ApiResult<Current> {
    let f = forecast(adm4).await?;
    let now_ms = worker::js_sys::Date::now() as i64;

    let mut chosen = 0usize;
    let mut best_started: Option<(usize, i64)> = None;
    for (i, s) in f.slots.iter().enumerate() {
        if let Some(ms) = iso_to_epoch_ms(&s.datetime) {
            if ms <= now_ms && best_started.is_none_or(|(_, b)| ms > b) {
                best_started = Some((i, ms));
            }
        }
    }
    if let Some((i, _)) = best_started {
        chosen = i;
    }

    let next = f.slots.iter().skip(chosen + 1).take(3).cloned().collect();
    Ok(Current {
        location: f.location,
        now: f.slots[chosen].clone(),
        next,
    })
}

/// Free text in, weather out.
///
/// The two-step "search for a code, then fetch the forecast" dance costs an LLM a
/// whole turn every time, so this collapses it — and reports what it matched, plus
/// what else it could have matched, so a wrong guess is visible rather than silent.
pub async fn search(db: &D1Database, q: &str) -> ApiResult<Resolved> {
    let hits = wilayah::search(db, q, 8, Some(4)).await?;
    let matched = hits
        .first()
        .cloned()
        .ok_or_else(|| ApiError::NotFound(format!("no village matched {q:?}")))?;
    let forecast = forecast(&matched.code).await?;
    Ok(Resolved {
        alternatives: hits.into_iter().skip(1).collect(),
        matched,
        forecast,
    })
}
