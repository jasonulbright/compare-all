//! Calendar arithmetic and the timestamp formats the script language uses.
//!
//! Every conversion here is plain arithmetic on a count of seconds since the
//! Unix epoch. No local time zone is read, so a caller that wants local time
//! passes the offset it already knows.

use std::time::{SystemTime, UNIX_EPOCH};

const SECONDS_PER_DAY: i64 = 86_400;

/// Seconds since the Unix epoch, negative before it.
#[must_use]
pub fn unix_seconds(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(span) => i64::try_from(span.as_secs()).unwrap_or(i64::MAX),
        Err(error) => -i64::try_from(error.duration().as_secs()).unwrap_or(i64::MAX),
    }
}

/// A count of seconds since the Unix epoch as a [`SystemTime`].
#[must_use]
pub fn system_time(seconds: i64) -> Option<SystemTime> {
    if seconds >= 0 {
        UNIX_EPOCH.checked_add(std::time::Duration::from_secs(seconds.unsigned_abs()))
    } else {
        UNIX_EPOCH.checked_sub(std::time::Duration::from_secs(seconds.unsigned_abs()))
    }
}

/// Split a day number into a year, a month and a day of the month.
///
/// Day zero is 1970-01-01.
#[must_use]
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = if month <= 2 { year + 1 } else { year };
    (
        year,
        u32::try_from(month).unwrap_or(1),
        u32::try_from(day).unwrap_or(1),
    )
}

/// Join a year, a month and a day of the month into a day number.
///
/// The result saturates at the `i64` limits when the civil date is outside
/// that range. [`parse_timestamp`] rejects a saturated day count when it
/// converts it to seconds.
#[must_use]
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let month = i128::from(month);
    let day = i128::from(day);
    let year = if month <= 2 {
        i128::from(year) - 1
    } else {
        i128::from(year)
    };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let shifted_month = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    i64::try_from(days).unwrap_or(if days < 0 { i64::MIN } else { i64::MAX })
}

/// Year, month, day, hour, minute and second of a count of seconds.
#[must_use]
pub fn parts(seconds: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = seconds.div_euclid(SECONDS_PER_DAY);
    let rest = seconds.rem_euclid(SECONDS_PER_DAY);
    let (year, month, day) = civil_from_days(days);
    let hour = u32::try_from(rest / 3600).unwrap_or(0);
    let minute = u32::try_from(rest % 3600 / 60).unwrap_or(0);
    let second = u32::try_from(rest % 60).unwrap_or(0);
    (year, month, day, hour, minute, second)
}

/// The date as `yyyy-mm-dd`.
#[must_use]
pub fn format_date(seconds: i64) -> String {
    let (year, month, day) = parts_date(seconds);
    format!("{year:04}-{month:02}-{day:02}")
}

fn parts_date(seconds: i64) -> (i64, u32, u32) {
    let (year, month, day, _, _, _) = parts(seconds);
    (year, month, day)
}

/// The time as `hh:mm:ss`.
#[must_use]
pub fn format_time(seconds: i64) -> String {
    let (_, _, _, hour, minute, second) = parts(seconds);
    format!("{hour:02}:{minute:02}:{second:02}")
}

/// The time as `hh-mm-ss`, which every file system accepts in a name.
#[must_use]
pub fn format_filename_time(seconds: i64) -> String {
    let (_, _, _, hour, minute, second) = parts(seconds);
    format!("{hour:02}-{minute:02}-{second:02}")
}

/// The date and time as `yyyy-mm-dd hh:mm:ss`.
#[must_use]
pub fn format_stamp(seconds: i64) -> String {
    format!("{} {}", format_date(seconds), format_time(seconds))
}

/// Read a timestamp written as `yyyy-mm-dd`, with an optional `hh:mm` or
/// `hh:mm:ss` after it. `/` is accepted in place of `-` between the date parts.
///
/// Returns the count of seconds since the Unix epoch, reading the text as a
/// time in the zone the caller's offset describes.
#[must_use]
pub fn parse_timestamp(text: &str, offset_seconds: i64) -> Option<i64> {
    let text = text.trim();
    let (date, time) = match text.split_once(' ') {
        Some((date, time)) => (date, Some(time.trim())),
        None => (text, None),
    };
    let mut date_parts = date.split(['-', '/']);
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let mut seconds = days_from_civil(year, month, day).checked_mul(SECONDS_PER_DAY)?;
    if let Some(time) = time {
        let mut time_parts = time.split(':');
        let hour: i64 = time_parts.next()?.parse().ok()?;
        let minute: i64 = time_parts.next()?.parse().ok()?;
        let second: i64 = match time_parts.next() {
            Some(value) => value.parse().ok()?,
            None => 0,
        };
        if time_parts.next().is_some()
            || !(0..24).contains(&hour)
            || !(0..60).contains(&minute)
            || !(0..61).contains(&second)
        {
            return None;
        }
        seconds = seconds.checked_add(hour * 3600 + minute * 60 + second)?;
    }
    seconds.checked_sub(offset_seconds)
}

#[cfg(test)]
mod tests {
    use super::{days_from_civil, parse_timestamp};

    #[test]
    fn an_extreme_year_is_rejected_without_overflowing_calendar_arithmetic() {
        for year in [i64::MIN, i64::MAX] {
            let timestamp = format!("{year}-01-01");
            assert_eq!(parse_timestamp(&timestamp, 0), None);
        }

        assert_eq!(days_from_civil(i64::MAX, 1, 1), i64::MAX);
        assert_eq!(days_from_civil(i64::MIN, 1, 1), i64::MIN);
    }
}
