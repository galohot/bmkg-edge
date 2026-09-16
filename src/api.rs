//! Response envelope, error taxonomy and the cached upstream fetch.
//!
//! Every route returns the same shape, so a caller writes one parser:
//!   success -> {"ok":true,"data":…,"meta":{…}}
//!   failure -> {"ok":false,"error":{"code":…,"message":…}}
//! A 404 from BMKG becomes `upstream_not_found`, never an empty 200.

use serde::Serialize;
use serde_json::Value;
use worker::{CfProperties, Fetch, Headers, Method, Request, RequestInit, Response, Result};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const USER_AGENT: &str = "bmkg-edge (+https://github.com/galohot/bmkg-edge)";

/// Wall-clock now as ISO 8601, straight from the runtime's clock.
pub fn now_iso() -> String {
    worker::js_sys::Date::new_0().to_iso_string().into()
}

#[derive(Serialize)]
pub struct Meta {
    pub source: &'static str,
    pub cached_seconds: u32,
    pub generated_at: String,
}

#[derive(Serialize)]
struct Ok_<T: Serialize> {
    ok: bool,
    data: T,
    meta: Meta,
}

#[derive(Serialize)]
struct ErrBody<'a> {
    ok: bool,
    error: ErrDetail<'a>,
}

#[derive(Serialize)]
struct ErrDetail<'a> {
    code: &'a str,
    message: &'a str,
}

/// Everything that can go wrong, with the status and machine-readable code it maps to.
#[derive(Debug)]
pub enum ApiError {
    /// The path does not exist on this service.
    NotFound(String),
    /// The caller's parameters are wrong.
    BadRequest(String),
    /// BMKG has no record for this identifier. The identifier is echoed back.
    UpstreamNotFound(String),
    /// BMKG is unreachable or returned 5xx.
    UpstreamUnavailable(String),
    /// BMKG answered, but not in the shape we know. This is the one that means
    /// *our* parser needs updating, so it must never be silently swallowed.
    UpstreamParse(String),
    /// A binding or the runtime failed.
    Internal(String),
}

impl ApiError {
    pub fn status(&self) -> u16 {
        match self {
            ApiError::NotFound(_) => 404,
            ApiError::BadRequest(_) => 400,
            ApiError::UpstreamNotFound(_) => 404,
            ApiError::UpstreamUnavailable(_) => 502,
            ApiError::UpstreamParse(_) => 502,
            ApiError::Internal(_) => 500,
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            ApiError::NotFound(_) => "not_found",
            ApiError::BadRequest(_) => "bad_request",
            ApiError::UpstreamNotFound(_) => "upstream_not_found",
            ApiError::UpstreamUnavailable(_) => "upstream_unavailable",
            ApiError::UpstreamParse(_) => "upstream_parse",
            ApiError::Internal(_) => "internal",
        }
    }

    pub fn message(&self) -> &str {
        match self {
            ApiError::NotFound(m)
            | ApiError::BadRequest(m)
            | ApiError::UpstreamNotFound(m)
            | ApiError::UpstreamUnavailable(m)
            | ApiError::UpstreamParse(m)
            | ApiError::Internal(m) => m,
        }
    }

    /// The same payload the REST layer returns, as a bare value — so an MCP tool
    /// call reports the identical error without a second formatting path.
    pub fn to_value(&self) -> Value {
        serde_json::json!({
            "ok": false,
            "error": { "code": self.code(), "message": self.message() }
        })
    }
}

impl From<worker::Error> for ApiError {
    fn from(e: worker::Error) -> Self {
        ApiError::Internal(e.to_string())
    }
}

pub type ApiResult<T> = std::result::Result<T, ApiError>;

/// Wrap a payload in the success envelope and attach cache headers.
pub fn json_ok<T: Serialize>(data: T, source: &'static str, ttl: u32) -> Result<Response> {
    let body = Ok_ {
        ok: true,
        data,
        meta: Meta {
            source,
            cached_seconds: ttl,
            generated_at: now_iso(),
        },
    };
    let mut res = Response::from_json(&body)?;
    decorate(res.headers_mut(), ttl)?;
    Ok(res)
}

pub fn json_err(err: &ApiError) -> Result<Response> {
    let body = ErrBody {
        ok: false,
        error: ErrDetail {
            code: err.code(),
            message: err.message(),
        },
    };
    let mut res = Response::from_json(&body)?.with_status(err.status());
    decorate(res.headers_mut(), 0)?;
    Ok(res)
}

fn decorate(h: &mut Headers, ttl: u32) -> Result<()> {
    h.set("access-control-allow-origin", "*")?;
    h.set("access-control-allow-headers", "content-type")?;
    h.set("access-control-allow-methods", "GET, POST, OPTIONS")?;
    h.set("x-bmkg-edge-version", VERSION)?;
    if ttl > 0 {
        h.set(
            "cache-control",
            &format!("public, max-age={ttl}, stale-while-revalidate={}", ttl * 4),
        )?;
    } else {
        h.set("cache-control", "no-store")?;
    }
    Ok(())
}

/// GET an upstream URL as text, letting Cloudflare's edge hold the response for `ttl`
/// seconds. This is the whole caching story: one subrequest per URL per TTL per colo,
/// so a traffic spike lands on the cache and never on BMKG.
pub async fn fetch_text(url: &str, ttl: u32) -> ApiResult<String> {
    let headers = Headers::new();
    headers
        .set("user-agent", USER_AGENT)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    headers
        .set("accept", "application/json, text/xml, */*")
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let mut init = RequestInit::new();
    init.with_method(Method::Get)
        .with_headers(headers)
        .with_cf_properties(CfProperties {
            cache_ttl: Some(ttl as i32),
            cache_everything: Some(true),
            ..Default::default()
        });

    let req = Request::new_with_init(url, &init)
        .map_err(|e| ApiError::Internal(format!("building request for {url}: {e}")))?;
    let mut res = Fetch::Request(req)
        .send()
        .await
        .map_err(|e| ApiError::UpstreamUnavailable(format!("{url}: {e}")))?;

    match res.status_code() {
        200..=299 => res
            .text()
            .await
            .map_err(|e| ApiError::UpstreamUnavailable(format!("reading {url}: {e}"))),
        404 => Err(ApiError::UpstreamNotFound(format!(
            "BMKG has no record at {url}"
        ))),
        s => Err(ApiError::UpstreamUnavailable(format!(
            "{url} returned HTTP {s}"
        ))),
    }
}

/// GET an upstream URL and deserialise it. A shape mismatch is an `upstream_parse`
/// error naming the URL — the signal that BMKG changed and this crate must follow.
pub async fn fetch_json<T: serde::de::DeserializeOwned>(url: &str, ttl: u32) -> ApiResult<T> {
    let body = fetch_text(url, ttl).await?;
    serde_json::from_str(&body)
        .map_err(|e| ApiError::UpstreamParse(format!("{url}: {e} (BMKG changed its payload shape)")))
}
