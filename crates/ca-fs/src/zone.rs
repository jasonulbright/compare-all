//! The offset of the machine's clock from coordinated universal time.
//!
//! The standard library does not report the zone. A DOS stamp in a zip or a
//! cabinet is a wall clock with no zone, and the machine's zone is the zone
//! such a stamp is read in; the views read the same offset to show a stamp as
//! a wall clock.

use std::sync::OnceLock;

/// The offset, read at most once per process.
static OFFSET: OnceLock<i32> = OnceLock::new();

/// The machine's current offset from coordinated universal time, in seconds
/// ahead of it.
///
/// Off Windows the first call runs a platform command, so it belongs on a
/// worker. A read that fails or answers something unreadable yields zero.
#[must_use]
pub fn local_offset_seconds() -> i32 {
    *OFFSET.get_or_init(read_offset)
}

/// The offset already read, without reading it.
#[must_use]
pub fn probed_offset() -> Option<i32> {
    OFFSET.get().copied()
}

#[cfg(windows)]
fn read_offset() -> i32 {
    let local = winsafe::GetLocalTime();
    let universal = winsafe::GetSystemTime();
    let seconds = |time: &winsafe::SYSTEMTIME| {
        ca_vfs::remote::timestamp::civil_to_unix(
            i64::from(time.wYear),
            u32::from(time.wMonth),
            u32::from(time.wDay),
            u32::from(time.wHour),
            u32::from(time.wMinute),
            u32::from(time.wSecond),
        )
    };
    whole_minutes(seconds(&local) - seconds(&universal))
}

#[cfg(not(windows))]
fn read_offset() -> i32 {
    let output = std::process::Command::new("date").arg("+%z").output();
    let Ok(output) = output else {
        return 0;
    };
    parse_numeric_zone(String::from_utf8_lossy(&output.stdout).trim())
}

/// The two clock reads can fall on each side of a second boundary, and every
/// zone is a whole number of minutes, so the difference is rounded to one.
#[cfg_attr(not(windows), allow(dead_code))]
fn whole_minutes(seconds: i64) -> i32 {
    i32::try_from((seconds + 30).div_euclid(60) * 60).unwrap_or(0)
}

/// Read a `+HHMM` zone designator into seconds.
#[cfg_attr(windows, allow(dead_code))]
fn parse_numeric_zone(text: &str) -> i32 {
    let (sign, digits) = match text.as_bytes().first() {
        Some(b'-') => (-1, &text[1..]),
        Some(b'+') => (1, &text[1..]),
        _ => (1, text),
    };
    let field = |range: std::ops::Range<usize>| -> i32 {
        digits
            .get(range)
            .and_then(|text| text.parse().ok())
            .unwrap_or(0)
    };
    sign * (field(0..2) * 3_600 + field(2..4) * 60)
}

#[cfg(test)]
mod tests {
    use super::{local_offset_seconds, parse_numeric_zone, probed_offset, whole_minutes};

    #[test]
    fn a_numeric_zone_reads_into_seconds() {
        assert_eq!(parse_numeric_zone("-0400"), -14_400);
        assert_eq!(parse_numeric_zone("+0530"), 19_800);
        assert_eq!(parse_numeric_zone("0000"), 0);
        assert_eq!(parse_numeric_zone("junk"), 0);
    }

    #[test]
    fn a_difference_one_second_off_a_whole_minute_rounds_to_it() {
        assert_eq!(whole_minutes(-14_399), -14_400);
        assert_eq!(whole_minutes(-14_401), -14_400);
        assert_eq!(whole_minutes(19_801), 19_800);
        assert_eq!(whole_minutes(0), 0);
    }

    #[test]
    fn the_offset_is_read_once_and_kept() {
        let first = local_offset_seconds();
        assert_eq!(probed_offset(), Some(first));
        assert_eq!(first % 60, 0);
        assert!(first.abs() <= 14 * 3_600, "{first}");
    }
}
