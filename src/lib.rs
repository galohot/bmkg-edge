//! bmkg-edge — BMKG's open data as a typed REST + MCP API, on one Cloudflare Worker.
//!
//! Why this exists: the Python project everyone points at (`dhanyyudi/bmkg-api`) is fine
//! code behind hosting that lapsed. Its demo answers `503 DEPLOYMENT_PAUSED` and its MCP
//! server proxies every tool call to that dead demo. This is the same API with nothing
//! that can lapse: no origin server, no paid plan, no machine to keep awake.

mod api;
mod earthquake;
mod geo;
mod landing;
mod mcp;
mod nowcast;
mod openapi;
mod timeutil;
mod weather;
mod wilayah;

use serde::Serialize;
use worker::{event, Cache, Context, Env, Headers, Method, Request, Response, Result};

use api::{json_err, json_ok, ApiError, ApiResult, VERSION};

#[event(fetch)]
async fn fetch(req: Request, env: Env, ctx: Context) -> Result<Response> {
    if req.method() == Method::Options {
        return preflight();
    }

    // Edge cache for our own responses. Without it every request would re-query D1 and
    // re-parse XML even though the answer is identical — and D1's free tier is billed in
    // rows read, so a popular region search would be the thing that runs out, not traffic.
    let key = req.url()?.to_string();
    let is_get = req.method() == Method::Get;
    if is_get {
        if let Some(hit) = Cache::default().get(&key, false).await? {
            return Ok(hit);
        }
    }

    let mut res = route(req, env).await?;

    if is_get && res.status_code() == 200 && is_storable(&res) {
        let stored = res.cloned()?;
        ctx.wait_until(async move {
            let _ = Cache::default().put(key, stored).await;
        });
    }
    Ok(res)
}

/// Cache only what we marked cacheable. `decorate()` writes `no-store` for anything
/// with a zero TTL, so that header is the single source of truth.
fn is_storable(res: &Response) -> bool {
    res.headers()
        .get("cache-control")
        .ok()
        .flatten()
        .is_some_and(|cc| !cc.contains("no-store"))
}

async fn route(mut req: Request, env: Env) -> Result<Response> {
    let path = req.path();
    let segments: Vec<&str> = path
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();

    if req.method() == Method::Post {
        return match segments.as_slice() {
            ["mcp"] => mcp::handle(&mut req, &env).await,
            _ => json_err(&ApiError::NotFound(format!("POST {path} is not a route"))),
        };
    }

    if req.method() != Method::Get {
        return json_err(&ApiError::BadRequest(format!(
            "{} is not supported; this API is GET-only except POST /mcp",
            req.method()
        )));
    }

    match segments.as_slice() {
        [] => landing::page(),
        ["health"] => health(),
        ["health", "deep"] => health_deep().await,
        ["mcp"] => mcp_probe(),
        ["v1", "openapi.json"] => openapi::document(),

        // ---- earthquake
        ["v1", "earthquake", "latest"] => respond(
            earthquake::latest().await,
            "BMKG autogempa",
            earthquake::TTL_LATEST,
        ),
        ["v1", "earthquake", "recent"] => respond(
            earthquake::recent().await,
            "BMKG gempaterkini",
            earthquake::TTL_RECENT,
        ),
        ["v1", "earthquake", "felt"] => respond(
            earthquake::felt().await,
            "BMKG gempadirasakan",
            earthquake::TTL_FELT,
        ),
        ["v1", "earthquake", "nearby"] => {
            let lat = num_param(&req, "lat");
            let lon = num_param(&req, "lon");
            let radius = num_param(&req, "radius_km").unwrap_or(Ok(500.0));
            match (lat, lon, radius) {
                (Some(Ok(lat)), Some(Ok(lon)), Ok(r)) => respond(
                    earthquake::nearby(lat, lon, r).await,
                    "BMKG gempaterkini + gempadirasakan",
                    earthquake::TTL_RECENT,
                ),
                (None, _, _) | (_, None, _) => json_err(&ApiError::BadRequest(
                    "lat and lon are required, e.g. /v1/earthquake/nearby?lat=-6.2&lon=106.8"
                        .into(),
                )),
                _ => json_err(&ApiError::BadRequest(
                    "lat, lon and radius_km must be numbers".into(),
                )),
            }
        }

        // ---- weather. `search` is matched first so it is never read as a region code.
        ["v1", "weather", "search"] => match param(&req, "q") {
            Some(q) => {
                let db = match wilayah::db(&env) {
                    Ok(db) => db,
                    Err(e) => return json_err(&e),
                };
                respond(
                    weather::search(&db, &q).await,
                    "BMKG prakiraan-cuaca",
                    weather::TTL,
                )
            }
            None => json_err(&ApiError::BadRequest(
                "q is required, e.g. /v1/weather/search?q=tebet".into(),
            )),
        },
        ["v1", "weather", adm4] => respond(
            weather::forecast(adm4).await,
            "BMKG prakiraan-cuaca",
            weather::TTL,
        ),
        ["v1", "weather", adm4, "current"] => respond(
            weather::current(adm4).await,
            "BMKG prakiraan-cuaca",
            weather::TTL,
        ),

        // ---- nowcast. `check` before the code, same reason.
        ["v1", "nowcast"] => respond(
            nowcast::index(&lang(&req)).await,
            "BMKG nowcast RSS",
            nowcast::TTL_INDEX,
        ),
        ["v1", "nowcast", "check"] => match param(&req, "location").or_else(|| param(&req, "q")) {
            Some(loc) => respond(
                nowcast::check(&loc, &lang(&req)).await,
                "BMKG nowcast RSS",
                nowcast::TTL_INDEX,
            ),
            None => json_err(&ApiError::BadRequest(
                "location is required, e.g. /v1/nowcast/check?location=jambi".into(),
            )),
        },
        ["v1", "nowcast", code] => respond(
            nowcast::detail(code, &lang(&req)).await,
            "BMKG CAP alert",
            nowcast::TTL_DETAIL,
        ),

        // ---- wilayah
        ["v1", "wilayah", rest @ ..] => wilayah_route(&req, &env, rest).await,

        _ => json_err(&ApiError::NotFound(format!(
            "{path} is not a route. See / for the endpoint list."
        ))),
    }
}

async fn wilayah_route(req: &Request, env: &Env, rest: &[&str]) -> Result<Response> {
    let db = match wilayah::db(env) {
        Ok(db) => db,
        Err(e) => return json_err(&e),
    };
    const SRC: &str = "Kemendagri via cahyadsn/wilayah";

    match rest {
        ["provinces"] => respond(wilayah::provinces(&db).await, SRC, wilayah::TTL),
        ["districts"] => child_route(req, &db, "province_code", 2).await,
        ["subdistricts"] => child_route(req, &db, "district_code", 3).await,
        ["villages"] => child_route(req, &db, "subdistrict_code", 4).await,
        ["search"] => match param(req, "q") {
            Some(q) => {
                let limit = param(req, "limit")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(10);
                let level = param(req, "level").and_then(|s| s.parse().ok());
                respond(
                    wilayah::search(&db, &q, limit, level).await,
                    SRC,
                    wilayah::TTL,
                )
            }
            None => json_err(&ApiError::BadRequest(
                "q is required, e.g. /v1/wilayah/search?q=tebet".into(),
            )),
        },
        [code] => respond(wilayah::get(&db, code).await, SRC, wilayah::TTL),
        _ => json_err(&ApiError::NotFound(
            "try /v1/wilayah/provinces, /districts, /subdistricts, /villages, /search or /{code}"
                .into(),
        )),
    }
}

async fn child_route(
    req: &Request,
    db: &worker::D1Database,
    param_name: &str,
    level: u8,
) -> Result<Response> {
    match param(req, param_name) {
        Some(code) => respond(
            wilayah::children(db, &code, level).await,
            "Kemendagri via cahyadsn/wilayah",
            wilayah::TTL,
        ),
        None => json_err(&ApiError::BadRequest(format!(
            "{param_name} is required, e.g. ?{param_name}=31"
        ))),
    }
}

fn respond<T: Serialize>(r: ApiResult<T>, source: &'static str, ttl: u32) -> Result<Response> {
    match r {
        Ok(v) => json_ok(v, source, ttl),
        Err(e) => json_err(&e),
    }
}

fn param(req: &Request, key: &str) -> Option<String> {
    req.url()
        .ok()?
        .query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn num_param(req: &Request, key: &str) -> Option<std::result::Result<f64, ()>> {
    param(req, key).map(|v| v.parse::<f64>().map_err(|_| ()))
}

fn lang(req: &Request) -> String {
    param(req, "lang").unwrap_or_else(|| "en".into())
}

fn health() -> Result<Response> {
    json_ok(
        serde_json::json!({
            "status": "ok",
            "version": VERSION,
            "upstreams": ["data.bmkg.go.id", "api.bmkg.go.id", "www.bmkg.go.id"],
        }),
        "bmkg-edge",
        0,
    )
}

/// Exercise every upstream and report which ones answered.
///
/// `/health` says the Worker is alive; this says the *data* is alive. It exists because
/// other projects depend on this service, and the failure that matters is not the Worker
/// going down — it is BMKG changing a payload while every response stays HTTP 200. Point a
/// cron at this and read `ok`.
async fn health_deep() -> Result<Response> {
    let mut checks = Vec::new();
    let mut all_ok = true;

    macro_rules! probe {
        ($name:expr, $call:expr) => {{
            let started = worker::js_sys::Date::now();
            let (ok, detail) = match $call {
                Ok(v) => (true, format!("{} item(s)", v)),
                Err(e) => (false, format!("{}: {}", e.code(), e.message())),
            };
            all_ok &= ok;
            checks.push(serde_json::json!({
                "source": $name,
                "ok": ok,
                "detail": detail,
                "ms": (worker::js_sys::Date::now() - started).round(),
            }));
        }};
    }

    probe!("earthquake/latest", earthquake::latest().await.map(|_| 1));
    probe!(
        "earthquake/recent",
        earthquake::recent().await.map(|v| v.len())
    );
    probe!("earthquake/felt", earthquake::felt().await.map(|v| v.len()));
    probe!(
        "weather/forecast",
        weather::forecast("31.74.04.1006")
            .await
            .map(|f| f.slots.len())
    );
    probe!("nowcast/index", nowcast::index("en").await.map(|v| v.len()));

    // Stays inside the standard envelope — one contract, no exceptions. The health verdict
    // is `healthy`, not `ok`: `ok` already means "the request succeeded", and overloading it
    // to also mean "the data is fine" would make two different things read the same key.
    // A watchdog can read either `data.healthy` or the HTTP status.
    let payload = serde_json::json!({
        "healthy": all_ok,
        "version": VERSION,
        "checks": checks,
    });
    let mut res = json_ok(payload, "bmkg-edge", 0)?;
    if !all_ok {
        res = res.with_status(503);
    }
    Ok(res)
}

/// A GET on /mcp is a client checking whether the endpoint exists. Say what it is
/// rather than returning a bare 405, which reads as "broken" in a client's logs.
fn mcp_probe() -> Result<Response> {
    json_ok(
        serde_json::json!({
            "transport": "streamable-http",
            "protocolVersion": "2025-06-18",
            "usage": "POST JSON-RPC 2.0 to this URL",
        }),
        "bmkg-edge",
        0,
    )
}

fn preflight() -> Result<Response> {
    let h = Headers::new();
    h.set("access-control-allow-origin", "*")?;
    h.set("access-control-allow-methods", "GET, POST, OPTIONS")?;
    h.set(
        "access-control-allow-headers",
        "content-type, mcp-session-id, mcp-protocol-version",
    )?;
    h.set("access-control-max-age", "86400")?;
    Ok(Response::empty()?.with_status(204).with_headers(h))
}
