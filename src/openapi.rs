//! A hand-written OpenAPI 3.1 document.
//!
//! Hand-written rather than derived: deriving it would mean annotating every model with
//! a schema crate, which costs more WASM than the document costs bytes.

use worker::{Headers, Response, Result};

use crate::api::VERSION;

pub fn document() -> Result<Response> {
    let doc = SPEC.replace("{{VERSION}}", VERSION);
    let h = Headers::new();
    h.set("content-type", "application/json; charset=utf-8")?;
    h.set("access-control-allow-origin", "*")?;
    h.set("cache-control", "public, max-age=3600")?;
    Ok(Response::ok(doc)?.with_headers(h))
}

const SPEC: &str = r##"{
  "openapi": "3.1.0",
  "info": {
    "title": "bmkg-edge",
    "version": "{{VERSION}}",
    "description": "BMKG open data as a typed JSON API. Earthquakes, weather forecasts, severe-weather warnings and Indonesian region codes. No key, no account, CORS open. Every response is {ok,data,meta} or {ok,error}.",
    "license": { "name": "MIT" }
  },
  "servers": [{ "url": "https://bmkg.irawan.dev" }],
  "paths": {
    "/health": { "get": { "summary": "Liveness and version", "responses": { "200": { "description": "ok" } } } },
    "/v1/earthquake/latest": { "get": { "summary": "Most recent earthquake", "responses": { "200": { "description": "ok" } } } },
    "/v1/earthquake/recent": { "get": { "summary": "Last 15 quakes of M5.0+", "responses": { "200": { "description": "ok" } } } },
    "/v1/earthquake/felt": { "get": { "summary": "Quakes people reported feeling", "responses": { "200": { "description": "ok" } } } },
    "/v1/earthquake/nearby": {
      "get": {
        "summary": "Quakes within a radius of a point, nearest first",
        "parameters": [
          { "name": "lat", "in": "query", "required": true, "schema": { "type": "number" } },
          { "name": "lon", "in": "query", "required": true, "schema": { "type": "number" } },
          { "name": "radius_km", "in": "query", "schema": { "type": "number", "default": 500 } }
        ],
        "responses": { "200": { "description": "ok" }, "400": { "description": "bad parameters" } }
      }
    },
    "/v1/weather/search": {
      "get": {
        "summary": "Forecast for a place named in plain text",
        "parameters": [{ "name": "q", "in": "query", "required": true, "schema": { "type": "string" } }],
        "responses": { "200": { "description": "ok" }, "404": { "description": "no village matched" } }
      }
    },
    "/v1/weather/{adm4}": {
      "get": {
        "summary": "Three-day forecast for one village",
        "parameters": [{ "name": "adm4", "in": "path", "required": true, "schema": { "type": "string", "example": "31.74.04.1006" } }],
        "responses": { "200": { "description": "ok" }, "404": { "description": "BMKG has no forecast for this code" } }
      }
    },
    "/v1/weather/{adm4}/current": {
      "get": {
        "summary": "The slot covering now, plus the next three",
        "parameters": [{ "name": "adm4", "in": "path", "required": true, "schema": { "type": "string", "example": "31.74.04.1006" } }],
        "responses": { "200": { "description": "ok" } }
      }
    },
    "/v1/nowcast": {
      "get": {
        "summary": "Active severe-weather warnings",
        "parameters": [{ "name": "lang", "in": "query", "schema": { "type": "string", "enum": ["en", "id"], "default": "en" } }],
        "responses": { "200": { "description": "ok" } }
      }
    },
    "/v1/nowcast/check": {
      "get": {
        "summary": "Active warnings mentioning a place",
        "parameters": [
          { "name": "location", "in": "query", "required": true, "schema": { "type": "string" } },
          { "name": "lang", "in": "query", "schema": { "type": "string", "enum": ["en", "id"] } }
        ],
        "responses": { "200": { "description": "ok" } }
      }
    },
    "/v1/nowcast/{alert_code}": {
      "get": {
        "summary": "Full CAP detail with the affected polygon",
        "parameters": [
          { "name": "alert_code", "in": "path", "required": true, "schema": { "type": "string", "example": "CJI20260911005" } },
          { "name": "lang", "in": "query", "schema": { "type": "string", "enum": ["en", "id"] } }
        ],
        "responses": { "200": { "description": "ok" } }
      }
    },
    "/v1/wilayah/provinces": { "get": { "summary": "All 38 provinces", "responses": { "200": { "description": "ok" } } } },
    "/v1/wilayah/districts": {
      "get": {
        "summary": "Kabupaten/kota in a province",
        "parameters": [{ "name": "province_code", "in": "query", "required": true, "schema": { "type": "string", "example": "31" } }],
        "responses": { "200": { "description": "ok" } }
      }
    },
    "/v1/wilayah/subdistricts": {
      "get": {
        "summary": "Kecamatan in a kabupaten/kota",
        "parameters": [{ "name": "district_code", "in": "query", "required": true, "schema": { "type": "string", "example": "31.74" } }],
        "responses": { "200": { "description": "ok" } }
      }
    },
    "/v1/wilayah/villages": {
      "get": {
        "summary": "Desa/kelurahan in a kecamatan",
        "parameters": [{ "name": "subdistrict_code", "in": "query", "required": true, "schema": { "type": "string", "example": "31.74.04" } }],
        "responses": { "200": { "description": "ok" } }
      }
    },
    "/v1/wilayah/search": {
      "get": {
        "summary": "Search regions by name, with full path",
        "parameters": [
          { "name": "q", "in": "query", "required": true, "schema": { "type": "string", "minLength": 2 } },
          { "name": "limit", "in": "query", "schema": { "type": "integer", "default": 10, "maximum": 100 } },
          { "name": "level", "in": "query", "schema": { "type": "integer", "minimum": 1, "maximum": 4 } }
        ],
        "responses": { "200": { "description": "ok" } }
      }
    },
    "/mcp": {
      "post": {
        "summary": "Model Context Protocol endpoint (JSON-RPC 2.0, streamable HTTP)",
        "responses": { "200": { "description": "event-stream with the JSON-RPC response" } }
      }
    }
  }
}"##;
