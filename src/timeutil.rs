//! Just enough date handling to answer "which forecast slot is happening now".
//!
//! Hand-rolled rather than pulling in `chrono`: this needs one function, and a date
//! library is tens of kilobytes of WASM on every cold start (GRANDPLAN §9).

/// Milliseconds since the Unix epoch for an ISO 8601 UTC timestamp.
///
/// Accepts `2026-09-16T04:00:00Z`, `2026-09-16T04:00:00+00:00` and the space-separated
/// `2026-09-16 04:00:00` that BMKG also emits. A non-zero offset is honoured.
pub fn iso_to_epoch_ms(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<i64> { s.get(from..to)?.parse::<i64>().ok() };

    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, sec) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }

    let mut ms = (days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + sec) * 1000;

    // Trailing offset, if any: "+07:00" / "-0300" / "Z".
    if let Some(rest) = s.get(19..) {
        let rest = rest.trim();
        if let Some(sign) = rest.chars().next() {
            if sign == '+' || sign == '-' {
                let digits: String = rest[1..].chars().filter(|c| c.is_ascii_digit()).collect();
                if digits.len() >= 4 {
                    let oh: i64 = digits[0..2].parse().ok()?;
                    let om: i64 = digits[2..4].parse().ok()?;
                    let offset = (oh * 3600 + om * 60) * 1000;
                    ms += if sign == '-' { offset } else { -offset };
                }
            }
        }
    }
    Some(ms)
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
/// Howard Hinnant's `days_from_civil` — exact for every date this service will see.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_itself() {
        assert_eq!(iso_to_epoch_ms("1970-01-01T00:00:00Z"), Some(0));
    }

    #[test]
    fn a_bmkg_slot() {
        // 2026-09-16T04:00:00Z
        assert_eq!(
            iso_to_epoch_ms("2026-09-16T04:00:00Z"),
            Some(1_789_531_200_000)
        );
    }

    #[test]
    fn space_separated_form() {
        assert_eq!(
            iso_to_epoch_ms("2026-09-16 04:00:00"),
            iso_to_epoch_ms("2026-09-16T04:00:00Z")
        );
    }

    #[test]
    fn offsets_are_applied() {
        // 11:00 in WIB is 04:00 UTC.
        assert_eq!(
            iso_to_epoch_ms("2026-09-16T11:00:00+07:00"),
            iso_to_epoch_ms("2026-09-16T04:00:00Z")
        );
    }

    #[test]
    fn leap_day() {
        assert_eq!(
            iso_to_epoch_ms("2024-02-29T00:00:00Z"),
            Some(1_709_164_800_000)
        );
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(iso_to_epoch_ms("not a date").is_none());
        assert!(iso_to_epoch_ms("2026-13-01T00:00:00Z").is_none());
    }
}
