//! MCP endpoint — JSON-RPC 2.0 over streamable HTTP, on the same origin as the REST API.
//!
//! The project this replaces died here. Its MCP server was a separate deployment whose
//! `BASE_URL` was hardcoded to the public API (`mcp_server/client.py:5`); when that URL
//! was paused, every tool kept answering and every answer was an error. So: the tools
//! below call the route handlers **directly**, in this same isolate. There is no base
//! URL to go stale, because there is no second service.
//!
//! Stateless by design — no `Mcp-Session-Id` is issued. The spec allows it and it means
//! any isolate can serve any request.

use serde_json::{json, Value};
use worker::{Env, Headers, Request, Response, Result};

use crate::api::{ApiError, VERSION};
use crate::{earthquake, nowcast, weather, wilayah};

const PROTOCOL_VERSION: &str = "2025-06-18";

pub async fn handle(req: &mut Request, env: &Env) -> Result<Response> {
    let body: Value = match req.json().await {
        Ok(v) => v,
        Err(e) => return rpc_error(Value::Null, -32700, &format!("parse error: {e}")),
    };

    // A batch is a JSON array. Clients rarely send one; answering each in turn is cheap.
    if let Some(items) = body.as_array() {
        let mut out = Vec::new();
        for item in items {
            if let Some(v) = dispatch(item, env).await {
                out.push(v);
            }
        }
        return if out.is_empty() {
            accepted()
        } else {
            sse(&Value::Array(out))
        };
    }

    match dispatch(&body, env).await {
        Some(v) => sse(&v),
        // A notification gets no body, only an acknowledgement.
        None => accepted(),
    }
}

async fn dispatch(msg: &Value, env: &Env) -> Option<Value> {
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let id = msg.get("id").cloned();
    let params = msg.get("params").cloned().unwrap_or(json!({}));

    // No id means a notification: act, answer nothing.
    let id = id?;

    let result: std::result::Result<Value, Value> = match method {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": "bmkg-edge", "version": VERSION },
            "instructions": "Indonesian weather, earthquake and severe-weather-warning data \
                             from BMKG. Region codes are Kemendagri codes; when you only have a \
                             place name, call find_weather rather than looking the code up first."
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_definitions() })),
        "tools/call" => return Some(call_tool(id, &params, env).await),
        other => {
            return Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("method not found: {other}") }
            }))
        }
    };

    Some(match result {
        Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
        Err(e) => json!({ "jsonrpc": "2.0", "id": id, "error": e }),
    })
}

async fn call_tool(id: Value, params: &Value, env: &Env) -> Value {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    let outcome = run_tool(name, &args, env).await;
    let (payload, is_error) = match outcome {
        Ok(v) => (v, false),
        Err(e) => (e.to_value(), true),
    };

    let text = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".into());
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "content": [{ "type": "text", "text": text }],
            "structuredContent": payload,
            "isError": is_error
        }
    })
}

fn str_arg<'a>(args: &'a Value, key: &str) -> std::result::Result<&'a str, ApiError> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| ApiError::BadRequest(format!("missing required argument {key:?}")))
}

fn num_arg(args: &Value, key: &str) -> std::result::Result<f64, ApiError> {
    args.get(key)
        .and_then(Value::as_f64)
        .ok_or_else(|| ApiError::BadRequest(format!("missing numeric argument {key:?}")))
}

fn lang_arg(args: &Value) -> String {
    args.get("lang")
        .and_then(Value::as_str)
        .unwrap_or("en")
        .to_string()
}

async fn run_tool(name: &str, args: &Value, env: &Env) -> std::result::Result<Value, ApiError> {
    match name {
        "get_latest_earthquake" => to_value(earthquake::latest().await?),
        "get_recent_earthquakes" => to_value(earthquake::recent().await?),
        "get_felt_earthquakes" => to_value(earthquake::felt().await?),
        "get_nearby_earthquakes" => {
            let radius = args
                .get("radius_km")
                .and_then(Value::as_f64)
                .unwrap_or(500.0);
            to_value(
                earthquake::nearby(num_arg(args, "lat")?, num_arg(args, "lon")?, radius).await?,
            )
        }
        "get_weather_forecast" => to_value(weather::forecast(str_arg(args, "adm4")?).await?),
        "get_current_weather" => to_value(weather::current(str_arg(args, "adm4")?).await?),
        "find_weather" => {
            let db = wilayah::db(env)?;
            to_value(weather::search(&db, str_arg(args, "query")?).await?)
        }
        "get_weather_warnings" => to_value(nowcast::index(&lang_arg(args)).await?),
        "check_location_warnings" => {
            to_value(nowcast::check(str_arg(args, "location")?, &lang_arg(args)).await?)
        }
        "get_warning_detail" => {
            to_value(nowcast::detail(str_arg(args, "alert_code")?, &lang_arg(args)).await?)
        }
        "search_regions" => {
            let db = wilayah::db(env)?;
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(10) as u32;
            let level = args.get("level").and_then(Value::as_u64).map(|l| l as u8);
            to_value(wilayah::search(&db, str_arg(args, "q")?, limit, level).await?)
        }
        "list_regions" => {
            let db = wilayah::db(env)?;
            match args.get("parent_code").and_then(Value::as_str) {
                Some(p) if !p.trim().is_empty() => {
                    let level = (p.split('.').count() + 1) as u8;
                    to_value(wilayah::children(&db, p, level).await?)
                }
                _ => to_value(wilayah::provinces(&db).await?),
            }
        }
        other => Err(ApiError::NotFound(format!("no tool named {other:?}"))),
    }
}

fn to_value<T: serde::Serialize>(v: T) -> std::result::Result<Value, ApiError> {
    serde_json::to_value(v).map_err(|e| ApiError::Internal(format!("serialising result: {e}")))
}

/// Tool descriptions are what an LLM reads to choose. They say what the data is and
/// how fresh it is, because "get weather" alone does not tell it when to call this.
fn tool_definitions() -> Value {
    json!([
      {
        "name": "get_latest_earthquake",
        "description": "The single most recent earthquake recorded anywhere in Indonesia, updated by BMKG within minutes of the event. Includes magnitude, depth, coordinates, the affected region, tsunami wording and felt intensities.",
        "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
      },
      {
        "name": "get_recent_earthquakes",
        "description": "The last 15 earthquakes of magnitude 5.0 or greater in Indonesia. Use for 'has there been a big quake recently'.",
        "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
      },
      {
        "name": "get_felt_earthquakes",
        "description": "Recent earthquakes that people actually reported feeling, with MMI intensity per place. Smaller than M5 but locally significant.",
        "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
      },
      {
        "name": "get_nearby_earthquakes",
        "description": "Recent earthquakes within a radius of a point, nearest first, each with its distance in kilometres.",
        "inputSchema": {
          "type": "object",
          "properties": {
            "lat": { "type": "number", "description": "Latitude, -90 to 90." },
            "lon": { "type": "number", "description": "Longitude, -180 to 180." },
            "radius_km": { "type": "number", "description": "Search radius in km. Default 500." }
          },
          "required": ["lat", "lon"],
          "additionalProperties": false
        }
      },
      {
        "name": "find_weather",
        "description": "Weather forecast for a place named in plain text, for example 'Tebet' or 'Pejaten Barat'. PREFER THIS over get_weather_forecast when you do not already have a region code: it resolves the name and returns the forecast in one call, and reports what else the name could have meant.",
        "inputSchema": {
          "type": "object",
          "properties": { "query": { "type": "string", "description": "Village, subdistrict or town name." } },
          "required": ["query"],
          "additionalProperties": false
        }
      },
      {
        "name": "get_weather_forecast",
        "description": "Three-day forecast in roughly 3-hourly slots for one village, keyed by its Kemendagri level-4 code such as 31.74.04.1006. Use find_weather instead if you only have a name.",
        "inputSchema": {
          "type": "object",
          "properties": { "adm4": { "type": "string", "description": "Level-4 region code, e.g. 31.74.04.1006." } },
          "required": ["adm4"],
          "additionalProperties": false
        }
      },
      {
        "name": "get_current_weather",
        "description": "The forecast slot covering right now for one village, plus the next three slots.",
        "inputSchema": {
          "type": "object",
          "properties": { "adm4": { "type": "string", "description": "Level-4 region code, e.g. 31.74.04.1006." } },
          "required": ["adm4"],
          "additionalProperties": false
        }
      },
      {
        "name": "get_weather_warnings",
        "description": "All active BMKG severe-weather warnings across Indonesia — thunderstorms, heavy rain, strong winds — with the affected subdistricts named in the text.",
        "inputSchema": {
          "type": "object",
          "properties": { "lang": { "type": "string", "enum": ["en", "id"], "description": "Default en." } },
          "additionalProperties": false
        }
      },
      {
        "name": "check_location_warnings",
        "description": "Active severe-weather warnings mentioning a place. This is a text match over the live warning list, so it finds a place that BMKG named in the warning text.",
        "inputSchema": {
          "type": "object",
          "properties": {
            "location": { "type": "string", "description": "Place name, at least 3 characters." },
            "lang": { "type": "string", "enum": ["en", "id"] }
          },
          "required": ["location"],
          "additionalProperties": false
        }
      },
      {
        "name": "get_warning_detail",
        "description": "Full CAP detail for one warning: severity, urgency, certainty, effective and expiry times, and the affected area as a GeoJSON-ordered polygon.",
        "inputSchema": {
          "type": "object",
          "properties": {
            "alert_code": { "type": "string", "description": "Code from get_weather_warnings, e.g. CJI20260911005." },
            "lang": { "type": "string", "enum": ["en", "id"] }
          },
          "required": ["alert_code"],
          "additionalProperties": false
        }
      },
      {
        "name": "search_regions",
        "description": "Search Indonesian administrative regions by name across all four levels. Returns the region code plus its full path, e.g. 'DKI Jakarta › Kota Adm. Jakarta Selatan › Pasar Minggu › Pejaten Barat'.",
        "inputSchema": {
          "type": "object",
          "properties": {
            "q": { "type": "string", "description": "At least 2 characters. Matches whole words." },
            "limit": { "type": "integer", "description": "1-100, default 10." },
            "level": { "type": "integer", "description": "Restrict to 1 province, 2 kabupaten/kota, 3 kecamatan, 4 desa/kelurahan." }
          },
          "required": ["q"],
          "additionalProperties": false
        }
      },
      {
        "name": "list_regions",
        "description": "List regions one level down from a parent code — omit the code for the 38 provinces, pass '31' for Jakarta's cities, '31.74' for its subdistricts, and so on.",
        "inputSchema": {
          "type": "object",
          "properties": { "parent_code": { "type": "string", "description": "Omit for provinces." } },
          "additionalProperties": false
        }
      }
    ])
}

// ---------------------------------------------------------------- transport

fn headers() -> Result<Headers> {
    let h = Headers::new();
    h.set("content-type", "text/event-stream")?;
    h.set("cache-control", "no-store")?;
    h.set("access-control-allow-origin", "*")?;
    h.set(
        "access-control-allow-headers",
        "content-type, mcp-session-id, mcp-protocol-version",
    )?;
    h.set("access-control-allow-methods", "POST, GET, OPTIONS")?;
    Ok(h)
}

fn sse(value: &Value) -> Result<Response> {
    let body = format!(
        "event: message\ndata: {}\n\n",
        serde_json::to_string(value).unwrap_or_else(|_| "{}".into())
    );
    Ok(Response::ok(body)?.with_headers(headers()?))
}

fn accepted() -> Result<Response> {
    Ok(Response::empty()?.with_status(202).with_headers(headers()?))
}

fn rpc_error(id: Value, code: i32, message: &str) -> Result<Response> {
    sse(&json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }))
}
