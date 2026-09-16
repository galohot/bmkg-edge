//! Indonesian administrative regions, out of D1.
//!
//! Source: `cahyadsn/wilayah`, tracking Kepmendagri 300.2.2-2430/2025 — 38 provinces.
//! The reference project this replaces ships a 34-province list, which BMKG no longer
//! accepts: its Merauke code `91.01.01.2001` returns 404 while the current
//! `93.01.01.2001` returns the same village. See DECISIONS.md D3.
//!
//! Every query here is an index hit. A `LIKE '%x%'` scan would read 91,599 rows per
//! search, which on D1's free tier is about 55 searches a day — hence the token table.

use serde::{Deserialize, Serialize};
use worker::wasm_bindgen::JsValue;
use worker::{D1Database, Env};

use crate::api::{ApiError, ApiResult};

pub const TTL: u32 = 86_400;

#[derive(Serialize, Deserialize, Clone)]
pub struct Region {
    pub code: String,
    pub name: String,
    /// 1 province · 2 kabupaten/kota · 3 kecamatan · 4 desa/kelurahan
    pub level: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// "DKI Jakarta › Kota Adm. Jakarta Selatan › Pasar Minggu › Pejaten Barat".
    /// Only filled by search, where a bare village name is ambiguous.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

const COLS: &str = "kode AS code, nama AS name, level, parent";

pub fn db(env: &Env) -> ApiResult<D1Database> {
    env.d1("DB")
        .map_err(|e| ApiError::Internal(format!("D1 binding 'DB' unavailable: {e}")))
}

async fn query(db: &D1Database, sql: &str, args: &[JsValue]) -> ApiResult<Vec<Region>> {
    let stmt = db
        .prepare(sql)
        .bind(args)
        .map_err(|e| ApiError::Internal(format!("binding query args: {e}")))?;
    let res = stmt
        .all()
        .await
        .map_err(|e| ApiError::Internal(format!("D1 query failed: {e}")))?;
    res.results::<Region>()
        .map_err(|e| ApiError::Internal(format!("decoding D1 rows: {e}")))
}

pub async fn provinces(db: &D1Database) -> ApiResult<Vec<Region>> {
    query(
        db,
        &format!("SELECT {COLS} FROM wilayah WHERE level = 1 ORDER BY nama"),
        &[],
    )
    .await
}

/// Direct children of `parent_code`. Serves districts, subdistricts and villages —
/// they differ only in which level the caller passes in, so one query covers all three.
pub async fn children(db: &D1Database, parent_code: &str, expect_level: u8) -> ApiResult<Vec<Region>> {
    validate_code(parent_code)?;
    let rows = query(
        db,
        &format!("SELECT {COLS} FROM wilayah WHERE parent = ?1 ORDER BY nama"),
        &[JsValue::from_str(parent_code)],
    )
    .await?;
    if rows.is_empty() {
        return Err(ApiError::NotFound(format!(
            "no level-{expect_level} regions under {parent_code}"
        )));
    }
    Ok(rows)
}

pub async fn get(db: &D1Database, code: &str) -> ApiResult<Region> {
    validate_code(code)?;
    query(
        db,
        &format!("SELECT {COLS} FROM wilayah WHERE kode = ?1"),
        &[JsValue::from_str(code)],
    )
    .await?
    .into_iter()
    .next()
    .ok_or_else(|| ApiError::NotFound(format!("no region with code {code}")))
}

/// Word-prefix search across every level, best match first.
///
/// Matching is per word, so "pejaten barat" finds "Pejaten Barat" and "barat" alone
/// finds it too. The range bound (`>= q AND < q+\u{FFFF}`) is deliberate: SQLite only
/// uses an index for `LIKE` under conditions D1 does not guarantee, but a range
/// comparison always does.
pub async fn search(db: &D1Database, q: &str, limit: u32, level: Option<u8>) -> ApiResult<Vec<Region>> {
    let norm = normalise(q);
    if norm.len() < 2 {
        return Err(ApiError::BadRequest(
            "q must be at least 2 characters".into(),
        ));
    }
    let limit = limit.clamp(1, 100);
    let hi = format!("{norm}\u{FFFF}");

    let level_filter = match level {
        Some(l) if (1..=4).contains(&l) => format!("AND w.level = {l} "),
        Some(l) => return Err(ApiError::BadRequest(format!("level must be 1..4, got {l}"))),
        None => String::new(),
    };

    let sql = format!(
        "SELECT w.kode AS code, w.nama AS name, w.level AS level, w.parent AS parent \
         FROM wilayah_token t JOIN wilayah w ON w.kode = t.kode \
         WHERE t.token >= ?1 AND t.token < ?2 {level_filter}\
         GROUP BY w.kode \
         ORDER BY (w.nama_norm = ?1) DESC, MIN(length(t.token)) ASC, w.level ASC, w.nama ASC \
         LIMIT ?3"
    );

    let mut rows = query(
        db,
        &sql,
        &[
            JsValue::from_str(&norm),
            JsValue::from_str(&hi),
            JsValue::from_f64(limit as f64),
        ],
    )
    .await?;

    attach_paths(db, &mut rows).await?;
    Ok(rows)
}

/// Fill in `path` for each row with one extra query over all ancestors at once.
async fn attach_paths(db: &D1Database, rows: &mut [Region]) -> ApiResult<()> {
    let mut wanted: Vec<String> = Vec::new();
    for r in rows.iter() {
        for a in ancestors(&r.code) {
            if !wanted.contains(&a) {
                wanted.push(a);
            }
        }
    }
    if wanted.is_empty() {
        return Ok(());
    }

    let placeholders = (1..=wanted.len())
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let args: Vec<JsValue> = wanted.iter().map(|c| JsValue::from_str(c)).collect();
    let found = query(
        db,
        &format!("SELECT {COLS} FROM wilayah WHERE kode IN ({placeholders})"),
        &args,
    )
    .await?;

    for r in rows.iter_mut() {
        let mut parts: Vec<String> = ancestors(&r.code)
            .into_iter()
            .filter_map(|c| found.iter().find(|f| f.code == c).map(|f| f.name.clone()))
            .collect();
        parts.push(r.name.clone());
        r.path = Some(parts.join(" › "));
    }
    Ok(())
}

/// Every ancestor code of `code`, outermost first. "31.74.04.1006" -> ["31","31.74","31.74.04"].
fn ancestors(code: &str) -> Vec<String> {
    let segs: Vec<&str> = code.split('.').collect();
    (1..segs.len()).map(|n| segs[..n].join(".")).collect()
}

/// Uppercase, strip accents and punctuation — mirrors `tools/build-wilayah.py`.
/// If these two ever disagree, search silently returns nothing, so they are tested together.
pub fn normalise(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_space = true;
    for ch in s.chars() {
        let c = ch.to_ascii_uppercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
            last_space = false;
        } else if !last_space {
            out.push(' ');
            last_space = true;
        }
    }
    out.trim().to_string()
}

/// A Kemendagri code is 2, 5, 8 or 13 characters of digits and dots. Checking the shape
/// here means a malformed code never becomes an upstream request.
pub fn validate_code(code: &str) -> ApiResult<()> {
    let ok = !code.is_empty()
        && code.len() <= 13
        && code.chars().all(|c| c.is_ascii_digit() || c == '.')
        && !code.starts_with('.')
        && !code.ends_with('.')
        && code.split('.').count() <= 4
        && code.split('.').all(|s| !s.is_empty());
    if ok {
        Ok(())
    } else {
        Err(ApiError::BadRequest(format!(
            "{code:?} is not a Kemendagri region code (e.g. 31, 31.74, 31.74.04, 31.74.04.1006)"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ancestors_of_a_village() {
        assert_eq!(ancestors("31.74.04.1006"), ["31", "31.74", "31.74.04"]);
        assert!(ancestors("31").is_empty());
    }

    #[test]
    fn normalise_matches_the_seed_builder() {
        assert_eq!(normalise("Pejaten Barat"), "PEJATEN BARAT");
        assert_eq!(normalise("Lubuk Pakam I,II"), "LUBUK PAKAM I II");
        assert_eq!(normalise("  kota  adm.  "), "KOTA ADM");
    }

    #[test]
    fn codes_are_shape_checked() {
        assert!(validate_code("31.74.04.1006").is_ok());
        assert!(validate_code("31").is_ok());
        assert!(validate_code("31..74").is_err());
        assert!(validate_code("31.74.04.1006.9").is_err());
        assert!(validate_code("'; DROP TABLE wilayah--").is_err());
    }
}
