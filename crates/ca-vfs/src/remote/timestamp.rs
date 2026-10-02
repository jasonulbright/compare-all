//! Calendar arithmetic and the timestamp spellings the remote protocols use.
//!
//! Every protocol here states a time as digits rather than as an instant, so
//! the conversion lives in one place. Seconds are counted from the Unix epoch
//! and may be negative.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Seconds in one day.
const DAY: i64 = 86_400;

/// Month names as a directory listing spells them.
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Seconds from the Unix epoch for a civil date and time in UTC.
///
/// The algorithm is the days-from-civil conversion: it holds for any
/// proleptic Gregorian date and needs no table.
/// Widest year the conversion accepts. A value outside it is clamped, because
/// the arithmetic below would otherwise overflow on a date a server made up.
const YEAR_RANGE: std::ops::RangeInclusive<i64> = -9999..=9999;

/// Seconds from the Unix epoch for a civil date and time in UTC.
///
/// Every field is clamped into the range the conversion holds for, so a date a
/// server made up gives a wrong instant rather than an arithmetic overflow.
#[must_use]
pub fn civil_to_unix(year: i64, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> i64 {
    let year = year.clamp(*YEAR_RANGE.start(), *YEAR_RANGE.end());
    let month = i64::from(month.clamp(1, 12));
    let day = i64::from(day.clamp(1, 31));
    let hour = hour.min(23);
    let minute = minute.min(59);
    let second = second.min(59);
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    days * DAY + i64::from(hour) * 3600 + i64::from(minute) * 60 + i64::from(second)
}

/// Year, month, day, hour, minute and second in UTC for `seconds`.
#[must_use]
pub fn unix_to_civil(seconds: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = seconds.div_euclid(DAY);
    let rest = seconds.rem_euclid(DAY);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = if month <= 2 { year + 1 } else { year };
    #[allow(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        reason = "month, day and the time of day are bounded by the conversion above"
    )]
    (
        year,
        month as u32,
        day as u32,
        (rest / 3600) as u32,
        ((rest % 3600) / 60) as u32,
        (rest % 60) as u32,
    )
}

/// A [`SystemTime`] for `seconds` from the Unix epoch.
#[must_use]
pub fn system_time(seconds: i64) -> SystemTime {
    if seconds >= 0 {
        #[allow(clippy::cast_sign_loss, reason = "the branch proves the sign")]
        let magnitude = seconds as u64;
        UNIX_EPOCH
            .checked_add(Duration::from_secs(magnitude))
            .unwrap_or(UNIX_EPOCH)
    } else {
        let magnitude = seconds.unsigned_abs();
        UNIX_EPOCH
            .checked_sub(Duration::from_secs(magnitude))
            .unwrap_or(UNIX_EPOCH)
    }
}

/// Seconds from the Unix epoch for `time`.
#[must_use]
pub fn unix_seconds(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(span) => i64::try_from(span.as_secs()).unwrap_or(i64::MAX),
        Err(error) => -i64::try_from(error.duration().as_secs()).unwrap_or(i64::MAX),
    }
}

/// Parse `n` decimal digits starting at `at`.
fn digits(text: &str, at: usize, n: usize) -> Option<i64> {
    let slice = text.get(at..at + n)?;
    if !slice.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    slice.parse().ok()
}

/// Parse the `YYYYMMDDHHMMSS` reply of the FTP modification time command.
///
/// A fractional part is accepted and dropped; the protocol states the value
/// in UTC.
#[must_use]
pub fn parse_ftp_stamp(text: &str) -> Option<i64> {
    let text = text.trim();
    if text.len() < 14 {
        return None;
    }
    let year = digits(text, 0, 4)?;
    let month = u32::try_from(digits(text, 4, 2)?).ok()?;
    let day = u32::try_from(digits(text, 6, 2)?).ok()?;
    let hour = u32::try_from(digits(text, 8, 2)?).ok()?;
    let minute = u32::try_from(digits(text, 10, 2)?).ok()?;
    let second = u32::try_from(digits(text, 12, 2)?).ok()?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    Some(civil_to_unix(
        year,
        month,
        day,
        hour,
        minute,
        second.min(59),
    ))
}

/// Render `seconds` as the `YYYYMMDDHHMMSS` argument of the FTP set-time
/// command.
#[must_use]
pub fn format_ftp_stamp(seconds: i64) -> String {
    let (year, month, day, hour, minute, second) = unix_to_civil(seconds);
    format!("{year:04}{month:02}{day:02}{hour:02}{minute:02}{second:02}")
}

/// Parse an ISO 8601 instant in UTC, as an object listing states it.
///
/// Accepts `YYYY-MM-DDTHH:MM:SS` with an optional fractional part and an
/// optional trailing `Z`.
#[must_use]
pub fn parse_iso8601_utc(text: &str) -> Option<i64> {
    let text = text.trim();
    if text.len() < 19 {
        return None;
    }
    let bytes = text.as_bytes();
    if bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
    {
        return None;
    }
    let year = digits(text, 0, 4)?;
    let month = u32::try_from(digits(text, 5, 2)?).ok()?;
    let day = u32::try_from(digits(text, 8, 2)?).ok()?;
    let hour = u32::try_from(digits(text, 11, 2)?).ok()?;
    let minute = u32::try_from(digits(text, 14, 2)?).ok()?;
    let second = u32::try_from(digits(text, 17, 2)?).ok()?;
    Some(civil_to_unix(
        year,
        month,
        day,
        hour,
        minute,
        second.min(59),
    ))
}

/// Render `seconds` as `YYYYMMDDTHHMMSSZ`, the long form of the request
/// signing stamp.
#[must_use]
pub fn format_basic_iso8601(seconds: i64) -> String {
    let (year, month, day, hour, minute, second) = unix_to_civil(seconds);
    format!("{year:04}{month:02}{day:02}T{hour:02}{minute:02}{second:02}Z")
}

/// Render `seconds` as `YYYYMMDD`, the scope date of the request signing
/// stamp.
#[must_use]
pub fn format_basic_date(seconds: i64) -> String {
    let (year, month, day, ..) = unix_to_civil(seconds);
    format!("{year:04}{month:02}{day:02}")
}

/// Parse an HTTP date in the preferred `Day, DD Mon YYYY HH:MM:SS GMT` form.
#[must_use]
pub fn parse_http_date(text: &str) -> Option<i64> {
    let text = text.trim();
    let rest = text.split_once(", ").map_or(text, |(_, tail)| tail);
    let mut parts = rest.split_whitespace();
    let day: u32 = parts.next()?.parse().ok()?;
    let month = month_number(parts.next()?)?;
    let year: i64 = parts.next()?.parse().ok()?;
    let clock = parts.next()?;
    let mut clock_parts = clock.split(':');
    let hour: u32 = clock_parts.next()?.parse().ok()?;
    let minute: u32 = clock_parts.next()?.parse().ok()?;
    let second: u32 = clock_parts.next()?.parse().ok()?;
    if !(1..=31).contains(&day) || !YEAR_RANGE.contains(&year) || hour > 23 || minute > 59 {
        return None;
    }
    Some(civil_to_unix(
        year,
        month,
        day,
        hour,
        minute,
        second.min(59),
    ))
}

/// Render `seconds` as an HTTP date.
#[must_use]
pub fn format_http_date(seconds: i64) -> String {
    const WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    let (year, month, day, hour, minute, second) = unix_to_civil(seconds);
    let weekday = WEEKDAYS
        .get(usize::try_from(seconds.div_euclid(DAY).rem_euclid(7)).unwrap_or(0))
        .copied()
        .unwrap_or("Thu");
    let name = month_name(month);
    format!("{weekday}, {day:02} {name} {year:04} {hour:02}:{minute:02}:{second:02} GMT")
}

/// The one-based month number for a three-letter month name.
#[must_use]
pub fn month_number(name: &str) -> Option<u32> {
    let wanted = name.get(..3)?;
    MONTHS
        .iter()
        .position(|month| month.eq_ignore_ascii_case(wanted))
        .map(|index| u32::try_from(index).unwrap_or(0) + 1)
}

/// The three-letter name of a one-based month number.
#[must_use]
pub fn month_name(month: u32) -> &'static str {
    MONTHS
        .get(usize::try_from(month.saturating_sub(1)).unwrap_or(0))
        .copied()
        .unwrap_or("Jan")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    #[test]
    fn the_epoch_and_a_leap_day_round_trip() {
        assert_eq!(civil_to_unix(1970, 1, 1, 0, 0, 0), 0);
        assert_eq!(unix_to_civil(0), (1970, 1, 1, 0, 0, 0));
        let leap = civil_to_unix(2024, 2, 29, 12, 30, 15);
        assert_eq!(unix_to_civil(leap), (2024, 2, 29, 12, 30, 15));
        let before = civil_to_unix(1969, 7, 20, 20, 17, 40);
        assert!(before < 0);
        assert_eq!(unix_to_civil(before), (1969, 7, 20, 20, 17, 40));
    }

    #[test]
    fn the_protocol_spellings_parse() {
        assert_eq!(
            parse_ftp_stamp("20240102030405"),
            Some(civil_to_unix(2024, 1, 2, 3, 4, 5))
        );
        assert_eq!(
            format_ftp_stamp(civil_to_unix(2024, 1, 2, 3, 4, 5)),
            "20240102030405"
        );
        assert_eq!(
            parse_iso8601_utc("2024-01-02T03:04:05.000Z"),
            Some(civil_to_unix(2024, 1, 2, 3, 4, 5))
        );
        assert_eq!(
            parse_http_date("Tue, 15 Nov 1994 12:45:26 GMT"),
            Some(civil_to_unix(1994, 11, 15, 12, 45, 26))
        );
        assert_eq!(
            format_basic_iso8601(civil_to_unix(2024, 1, 2, 3, 4, 5)),
            "20240102T030405Z"
        );
        assert!(parse_ftp_stamp("not a stamp").is_none());
        assert!(parse_iso8601_utc("2024-01-02").is_none());
    }

    #[test]
    fn the_http_date_weekday_is_right() {
        let stamp = civil_to_unix(1994, 11, 15, 12, 45, 26);
        assert_eq!(format_http_date(stamp), "Tue, 15 Nov 1994 12:45:26 GMT");
    }
}
