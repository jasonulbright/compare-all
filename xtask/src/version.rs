//! Version string: `YYYY.MM.DD.NNNN`. Date is the build date in UTC. NNNN is
//! the cumulative build number read from `BUILD_NUMBER` at the repository
//! root, zero padded to four digits and never reset.
//!
//! `CA_BUILD_DATE=YYYY-MM-DD` replaces the build date, which is what makes a
//! build reproducible from a source archive alone.

use std::path::Path;

/// Name of the environment variable that fixes the build date.
pub const BUILD_DATE_VAR: &str = "CA_BUILD_DATE";

/// Compute the version string. Panics only when `BUILD_NUMBER` is missing or
/// malformed or when `CA_BUILD_DATE` is not a valid date, each of which is a
/// defect in the build inputs, not a runtime condition.
#[allow(clippy::expect_used, clippy::panic, dead_code)]
pub fn compute() -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .find(|p| p.join("BUILD_NUMBER").exists())
        .expect("BUILD_NUMBER not found in any ancestor of the crate");
    let raw = std::fs::read_to_string(root.join("BUILD_NUMBER")).expect("read BUILD_NUMBER");
    let n: u32 = raw.trim().parse().expect("BUILD_NUMBER must be an integer");
    assert!(n >= 1, "BUILD_NUMBER starts at 0001");
    let date = match std::env::var(BUILD_DATE_VAR) {
        Ok(text) => {
            parse_build_date(&text).unwrap_or_else(|reason| panic!("{BUILD_DATE_VAR}: {reason}"))
        }
        Err(_) => today_utc(),
    };
    version_string(date, n)
}

/// Assemble the version string from its two inputs.
#[allow(dead_code)]
fn version_string((y, m, d): (u32, u32, u32), build: u32) -> String {
    format!("{y:04}.{m:02}.{d:02}.{build:04}")
}

/// Parse a `CA_BUILD_DATE` value.
///
/// The form is exactly `YYYY-MM-DD` with ASCII digits. A date that does not
/// exist in the calendar is rejected rather than rolled over, so a typo cannot
/// silently stamp a different day.
#[allow(dead_code)]
fn parse_build_date(text: &str) -> Result<(u32, u32, u32), String> {
    let bytes = text.as_bytes();
    let shaped = bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && [0, 1, 2, 3, 5, 6, 8, 9]
            .iter()
            .all(|i| bytes[*i].is_ascii_digit());
    if !shaped {
        return Err(format!("{text:?} is not of the form YYYY-MM-DD"));
    }
    let number = |range: std::ops::Range<usize>| -> u32 {
        text.get(range)
            .and_then(|part| part.parse().ok())
            .unwrap_or(0)
    };
    let date = (number(0..4), number(5..7), number(8..10));
    if !is_real_date(date) {
        return Err(format!("{text:?} is not a date in the calendar"));
    }
    Ok(date)
}

/// True when the calendar holds the given day.
#[allow(dead_code)]
fn is_real_date((y, m, d): (u32, u32, u32)) -> bool {
    if !(1..=12).contains(&m) || d < 1 {
        return false;
    }
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let last = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if leap => 29,
        _ => 28,
    };
    d <= last
}

/// Today's UTC date in the form `CA_BUILD_DATE` takes.
#[allow(dead_code)]
pub fn today_iso() -> String {
    let (y, m, d) = today_utc();
    format!("{y:04}-{m:02}-{d:02}")
}

fn today_utc() -> (u32, u32, u32) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    civil_from_days(secs / 86_400)
}

// Valid for the proleptic Gregorian calendar; days counted from 1970-01-01.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]
fn civil_from_days(days: u64) -> (u32, u32, u32) {
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as u32, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::{civil_from_days, parse_build_date, version_string};

    #[test]
    fn known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_713), (2026, 9, 17));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
    }

    #[test]
    fn a_build_date_override_is_parsed() {
        assert_eq!(parse_build_date("2026-09-18"), Ok((2026, 9, 18)));
        assert_eq!(parse_build_date("2000-02-29"), Ok((2000, 2, 29)));
    }

    #[test]
    fn a_build_date_override_is_rejected_unless_it_is_a_real_date() {
        for bad in [
            "",
            "2026-9-18",
            "2026-09-18 ",
            "2026/09/18",
            "20260918",
            "2026-09-18T00:00:00Z",
            "2026-13-01",
            "2026-00-10",
            "2026-09-31",
            "2026-02-30",
            "1900-02-29",
            "202x-09-18",
        ] {
            assert!(parse_build_date(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn the_version_is_padded_in_every_field() {
        assert_eq!(version_string((2026, 9, 8), 1), "2026.09.08.0001");
        assert_eq!(version_string((2026, 12, 31), 12_345), "2026.12.31.12345");
    }
}
