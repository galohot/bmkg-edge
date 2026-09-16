//! The documentation page, embedded in the binary.
//!
//! `include_str!` rather than a Workers asset binding: the page is a few KB, it changes
//! with the API it documents, and an asset binding would be a second thing to deploy.

use worker::{Headers, Response, Result};

pub fn page() -> Result<Response> {
    let h = Headers::new();
    h.set("content-type", "text/html; charset=utf-8")?;
    h.set("cache-control", "public, max-age=300")?;
    h.set("x-content-type-options", "nosniff")?;
    Ok(Response::ok(include_str!("../assets/landing.html"))?.with_headers(h))
}
