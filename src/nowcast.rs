//! Severe-weather warnings: BMKG's RSS index and its CAP 1.2 detail files.
//!
//! Two XML formats, both parsed with `quick-xml` in streaming mode — no DOM, because
//! the index can carry dozens of alerts each with a paragraph of description.
//!
//! Note on quick-xml 0.42: entity references arrive as their own `GeneralRef` events
//! rather than inside the text, so text is accumulated across events until the closing
//! tag. Miss that and every `&amp;` silently truncates a description.

use quick_xml::events::Event;
use quick_xml::Reader;
use serde::Serialize;

use crate::api::{fetch_text, ApiError, ApiResult};

const BASE: &str = "https://www.bmkg.go.id/alerts/nowcast";
pub const TTL_INDEX: u32 = 120;
pub const TTL_DETAIL: u32 = 900;

#[derive(Serialize, Clone)]
pub struct Alert {
    /// The id used by `/v1/nowcast/{code}`, taken from the item link.
    pub alert_code: String,
    pub headline: String,
    /// Best effort, read off the headline ("… in Jambi"). Absent when the wording
    /// does not carry it — the authoritative area is in the detail response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub area: Option<String>,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published: Option<String>,
    pub detail_url: String,
}

#[derive(Serialize)]
pub struct AlertDetail {
    pub alert_code: String,
    pub identifier: String,
    pub sender: String,
    pub sent: String,
    pub status: String,
    pub message_type: String,
    pub language: String,
    pub category: String,
    pub event: String,
    pub urgency: String,
    pub severity: String,
    pub certainty: String,
    pub effective: String,
    pub expires: String,
    pub headline: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub infographic_url: Option<String>,
    pub areas: Vec<Area>,
}

#[derive(Serialize)]
pub struct Area {
    pub description: String,
    /// GeoJSON order — `[longitude, latitude]`. CAP publishes `lat,lon`; the swap
    /// happens here, once, so a caller can hand this straight to a map library.
    pub polygon: Vec<[f64; 2]>,
}

fn lang(code: &str) -> ApiResult<&'static str> {
    match code {
        "en" | "" => Ok("en"),
        "id" => Ok("id"),
        other => Err(ApiError::BadRequest(format!(
            "lang must be 'en' or 'id', got {other:?}"
        ))),
    }
}

/// Named entities XML defines without a DTD. Numeric refs are resolved by quick-xml.
fn entity(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => None,
    }
}

/// The tag name without its namespace prefix. CAP files namespace everything,
/// RSS namespaces some of it, and we only ever match on the local part.
fn local(name: &str) -> String {
    name.rsplit(':').next().unwrap_or("").to_string()
}

// ---------------------------------------------------------------- index

pub async fn index(language: &str) -> ApiResult<Vec<Alert>> {
    let language = lang(language)?;
    let xml = fetch_text(&format!("{BASE}/{language}"), TTL_INDEX).await?;
    parse_rss(&xml, language)
}

fn parse_rss(xml: &str, language: &str) -> ApiResult<Vec<Alert>> {
    let mut reader = Reader::from_str(xml);
    // Deliberately NOT trim_text(true): quick-xml splits text at every entity, so
    // trimming each fragment eats the spaces around them and "A &amp; B" arrives as
    // "A&B". Accumulate raw, trim once at the closing tag.

    let mut alerts = Vec::new();
    let mut in_item = false;
    let mut field = String::new();
    let mut text = String::new();
    let mut item = RawItem::default();

    loop {
        match reader.read_event().map_err(|e| {
            ApiError::UpstreamParse(format!("BMKG alert feed is not valid XML: {e}"))
        })? {
            Event::Start(e) => {
                let name = local(e.name().as_ref());
                if name == "item" {
                    in_item = true;
                    item = RawItem::default();
                } else if in_item {
                    field = name;
                    text.clear();
                }
            }
            Event::Text(t) if in_item && !field.is_empty() => {
                text.push_str(&t.xml10_content());
            }
            Event::CData(t) if in_item && !field.is_empty() => {
                text.push_str(t.as_ref());
            }
            Event::GeneralRef(r) if in_item && !field.is_empty() => {
                if let Ok(Some(c)) = r.resolve_char_ref() {
                    text.push(c);
                } else if let Some(c) = entity(&r.xml10_content()) {
                    text.push(c);
                }
            }
            Event::End(e) => {
                let name = local(e.name().as_ref());
                if name == "item" {
                    in_item = false;
                    if let Some(a) = std::mem::take(&mut item).build(language) {
                        alerts.push(a);
                    }
                } else if in_item && name == field {
                    item.set(&field, text.trim());
                    field.clear();
                    text.clear();
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(alerts)
}

#[derive(Default)]
struct RawItem {
    title: String,
    link: String,
    description: String,
    category: String,
    pub_date: String,
}

impl RawItem {
    fn set(&mut self, field: &str, value: &str) {
        match field {
            "title" => self.title = value.to_string(),
            "link" => self.link = value.to_string(),
            "description" => self.description = value.to_string(),
            "category" => self.category = value.to_string(),
            "pubDate" => self.pub_date = value.to_string(),
            _ => {}
        }
    }

    fn build(self, language: &str) -> Option<Alert> {
        let code = code_from_link(&self.link)?;
        Some(Alert {
            area: area_from_headline(&self.title),
            alert_code: code.clone(),
            headline: self.title,
            description: self.description,
            category: (!self.category.is_empty()).then_some(self.category),
            published: (!self.pub_date.is_empty()).then_some(self.pub_date),
            detail_url: format!("/v1/nowcast/{code}?lang={language}"),
        })
    }
}

/// ".../CJI20260911005_alert.xml" -> "CJI20260911005".
/// Read off the link rather than the guid: the guid is a CAP identifier, which is a
/// different string and does not address the detail file.
fn code_from_link(link: &str) -> Option<String> {
    let file = link.rsplit('/').next()?;
    let stem = file.strip_suffix("_alert.xml").unwrap_or(file);
    let ok = !stem.is_empty() && stem.chars().all(|c| c.is_ascii_alphanumeric());
    ok.then(|| stem.to_string())
}

/// "Thunderstorm Tonight in Jambi" -> "Jambi"; the Indonesian feed says "di Jambi".
fn area_from_headline(title: &str) -> Option<String> {
    for sep in [" in ", " di "] {
        if let Some(idx) = title.rfind(sep) {
            let tail = title[idx + sep.len()..].trim();
            if !tail.is_empty() {
                return Some(tail.to_string());
            }
        }
    }
    None
}

// ---------------------------------------------------------------- detail

pub async fn detail(code: &str, language: &str) -> ApiResult<AlertDetail> {
    let language = lang(language)?;
    if code.is_empty() || !code.chars().all(|c| c.is_ascii_alphanumeric()) || code.len() > 32 {
        return Err(ApiError::BadRequest(format!(
            "{code:?} is not an alert code (letters and digits, e.g. CJI20260911005)"
        )));
    }
    let url = format!("{BASE}/{language}/{code}_alert.xml");
    let xml = fetch_text(&url, TTL_DETAIL).await?;
    parse_cap(&xml, code)
}

fn parse_cap(xml: &str, code: &str) -> ApiResult<AlertDetail> {
    let mut reader = Reader::from_str(xml);
    // Deliberately NOT trim_text(true): quick-xml splits text at every entity, so
    // trimming each fragment eats the spaces around them and "A &amp; B" arrives as
    // "A&B". Accumulate raw, trim once at the closing tag.

    let mut f = CapFields::default();
    let mut areas: Vec<Area> = Vec::new();
    let mut current_area: Option<(String, Vec<[f64; 2]>)> = None;

    let mut path: Vec<String> = Vec::new();
    let mut text = String::new();

    loop {
        match reader.read_event().map_err(|e| {
            ApiError::UpstreamParse(format!("CAP file {code} is not valid XML: {e}"))
        })? {
            Event::Start(e) => {
                let name = local(e.name().as_ref());
                if name == "area" {
                    current_area = Some((String::new(), Vec::new()));
                }
                path.push(name);
                text.clear();
            }
            Event::Text(t) => text.push_str(&t.xml10_content()),
            Event::CData(t) => text.push_str(t.as_ref()),
            Event::GeneralRef(r) => {
                if let Ok(Some(c)) = r.resolve_char_ref() {
                    text.push(c);
                } else if let Some(c) = entity(&r.xml10_content()) {
                    text.push(c);
                }
            }
            Event::End(e) => {
                let name = local(e.name().as_ref());
                let value = text.trim().to_string();
                let in_area = current_area.is_some();

                match (name.as_str(), in_area) {
                    ("areaDesc", true) => {
                        if let Some(a) = current_area.as_mut() {
                            a.0 = value;
                        }
                    }
                    ("polygon", true) => {
                        if let Some(a) = current_area.as_mut() {
                            a.1 = parse_polygon(&value);
                        }
                    }
                    ("area", _) => {
                        if let Some((desc, poly)) = current_area.take() {
                            areas.push(Area {
                                description: desc,
                                polygon: poly,
                            });
                        }
                    }
                    _ if !in_area => f.set(&name, &value),
                    _ => {}
                }
                path.pop();
                text.clear();
            }
            Event::Eof => break,
            _ => {}
        }
    }

    if f.identifier.is_empty() && f.headline.is_empty() {
        return Err(ApiError::UpstreamParse(format!(
            "CAP file {code} carried neither an identifier nor a headline"
        )));
    }

    Ok(AlertDetail {
        alert_code: code.to_string(),
        identifier: f.identifier,
        sender: f.sender,
        sent: f.sent,
        status: f.status,
        message_type: f.msg_type,
        language: f.language,
        category: f.category,
        event: f.event,
        urgency: f.urgency,
        severity: f.severity,
        certainty: f.certainty,
        effective: f.effective,
        expires: f.expires,
        headline: f.headline,
        description: f.description,
        infographic_url: (!f.web.is_empty()).then_some(f.web),
        areas,
    })
}

#[derive(Default)]
struct CapFields {
    identifier: String,
    sender: String,
    sent: String,
    status: String,
    msg_type: String,
    language: String,
    category: String,
    event: String,
    urgency: String,
    severity: String,
    certainty: String,
    effective: String,
    expires: String,
    headline: String,
    description: String,
    web: String,
}

impl CapFields {
    fn set(&mut self, name: &str, v: &str) {
        if v.is_empty() {
            return;
        }
        let slot = match name {
            "identifier" => &mut self.identifier,
            "sender" => &mut self.sender,
            "sent" => &mut self.sent,
            "status" => &mut self.status,
            "msgType" => &mut self.msg_type,
            "language" => &mut self.language,
            "category" => &mut self.category,
            "event" => &mut self.event,
            "urgency" => &mut self.urgency,
            "severity" => &mut self.severity,
            "certainty" => &mut self.certainty,
            "effective" => &mut self.effective,
            "expires" => &mut self.expires,
            "headline" => &mut self.headline,
            "description" => &mut self.description,
            "web" => &mut self.web,
            _ => return,
        };
        if slot.is_empty() {
            *slot = v.to_string();
        }
    }
}

/// CAP polygons are whitespace-separated `lat,lon` pairs. GeoJSON wants `[lon,lat]`.
fn parse_polygon(s: &str) -> Vec<[f64; 2]> {
    s.split_whitespace()
        .filter_map(|pair| {
            let (lat, lon) = pair.split_once(',')?;
            Some([lon.trim().parse().ok()?, lat.trim().parse().ok()?])
        })
        .collect()
}

// ---------------------------------------------------------------- check

/// Which active warnings mention a place.
///
/// BMKG lists affected subdistricts inside the description text, not as structured
/// fields, so this is a text match over the live index — good enough to answer "is
/// there a warning for my town", and honest about being a text match.
pub async fn check(query: &str, language: &str) -> ApiResult<Vec<Alert>> {
    if query.trim().len() < 3 {
        return Err(ApiError::BadRequest(
            "location must be at least 3 characters".into(),
        ));
    }
    let needle = query.trim().to_uppercase();
    Ok(index(language)
        .await?
        .into_iter()
        .filter(|a| {
            a.headline.to_uppercase().contains(&needle)
                || a.description.to_uppercase().contains(&needle)
                || a.area
                    .as_deref()
                    .is_some_and(|x| x.to_uppercase().contains(&needle))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alert_code_comes_from_the_link() {
        assert_eq!(
            code_from_link("https://www.bmkg.go.id/alerts/nowcast/en/CJI20260911005_alert.xml"),
            Some("CJI20260911005".into())
        );
        assert_eq!(code_from_link(""), None);
    }

    #[test]
    fn area_read_off_both_languages() {
        assert_eq!(
            area_from_headline("Thunderstorm Tonight in Jambi"),
            Some("Jambi".into())
        );
        assert_eq!(
            area_from_headline("Hujan Lebat Malam Ini di Kalimantan Utara"),
            Some("Kalimantan Utara".into())
        );
        assert_eq!(area_from_headline("Peringatan Dini"), None);
    }

    #[test]
    fn polygon_is_swapped_into_geojson_order() {
        let p = parse_polygon("-2.008,102.331 -2.015,102.357");
        assert_eq!(p, vec![[102.331, -2.008], [102.357, -2.015]]);
    }

    #[test]
    fn rss_items_survive_entities() {
        let xml = r#"<rss><channel>
            <item>
              <title>Storm in A &amp; B</title>
              <link>https://x/EX1_alert.xml</link>
              <description>rain &lt;heavy&gt;</description>
              <category>Met</category>
            </item></channel></rss>"#;
        let out = parse_rss(xml, "en").unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].headline, "Storm in A & B");
        assert_eq!(out[0].description, "rain <heavy>");
        assert_eq!(out[0].alert_code, "EX1");
    }

    #[test]
    fn cap_detail_parses_area_and_polygon() {
        let xml = r#"<alert xmlns="urn:oasis:names:tc:emergency:cap:1.2">
          <identifier>2.49.0.1</identifier><sender>x@bmkg.go.id</sender>
          <sent>2026-09-12T01:55:00+07:00</sent><status>Actual</status><msgType>Alert</msgType>
          <info><language>en</language><category>Met</category><event>Thunderstorm</event>
            <urgency>Immediate</urgency><severity>Moderate</severity><certainty>Observed</certainty>
            <effective>2026-09-12T02:05:00+07:00</effective><expires>2026-09-12T05:00:00+07:00</expires>
            <headline>Thunderstorm Tonight in Jambi</headline><description>rain</description>
            <web>https://example/i.jpg</web>
            <area><areaDesc>Jambi</areaDesc><polygon>-2.0,102.3 -2.1,102.4 -2.0,102.3</polygon></area>
          </info></alert>"#;
        let d = parse_cap(xml, "EX1").unwrap();
        assert_eq!(d.severity, "Moderate");
        assert_eq!(d.areas.len(), 1);
        assert_eq!(d.areas[0].description, "Jambi");
        assert_eq!(d.areas[0].polygon.len(), 3);
        assert_eq!(d.areas[0].polygon[0], [102.3, -2.0]);
        assert_eq!(d.infographic_url.as_deref(), Some("https://example/i.jpg"));
    }
}
