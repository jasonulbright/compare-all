//! Column text for sizes and timestamps.
//!
//! Sizes are byte counts with a thousands separator. Timestamps are civil dates
//! in the machine's own time zone. The zone offset is not available from the
//! standard library, so it is probed once on a worker and passed in; a caller
//! that has not probed it yet renders at zero offset rather than waiting.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds in one day.
const DAY: i64 = 86_400;

/// A byte count with a thousands separator.
#[must_use]
pub fn format_bytes(bytes: u64) -> String {
    let digits = bytes.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    let lead = digits.len() % 3;
    for (position, digit) in digits.chars().enumerate() {
        if position > 0 && position % 3 == lead {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// A timestamp as month, day, year and a twelve hour clock.
///
/// `offset_seconds` shifts the value from coordinated universal time to the
/// zone the column is read in.
#[must_use]
pub fn format_stamp(time: Option<SystemTime>, offset_seconds: i32) -> String {
    let Some(seconds) = unix_seconds(time) else {
        return String::new();
    };
    let shifted = seconds.saturating_add(i64::from(offset_seconds));
    let days = shifted.div_euclid(DAY);
    let rest = shifted.rem_euclid(DAY);
    let (year, month, day) = civil_from_days(days);
    let hour24 = rest / 3_600;
    let minute = (rest % 3_600) / 60;
    let second = rest % 60;
    let meridiem = if hour24 < 12 { "AM" } else { "PM" };
    let hour12 = match hour24 % 12 {
        0 => 12,
        other => other,
    };
    format!("{month}/{day}/{year} {hour12}:{minute:02}:{second:02} {meridiem}")
}

/// Seconds since the epoch, negative for times before it.
fn unix_seconds(time: Option<SystemTime>) -> Option<i64> {
    let time = time?;
    match time.duration_since(UNIX_EPOCH) {
        Ok(elapsed) => i64::try_from(elapsed.as_secs()).ok(),
        Err(error) => i64::try_from(error.duration().as_secs())
            .ok()
            .map(|value| -value),
    }
}

/// Days since the epoch to a civil date, by the usual era based algorithm.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
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
    (year, month, day)
}

/// The machine's current offset from coordinated universal time, in seconds.
///
/// Off Windows the first call runs a platform command, so it belongs on a
/// worker. A read that fails yields zero, which renders the column in
/// coordinated universal time rather than leaving it blank. The engine reads
/// a container's DOS stamps in the same offset.
#[must_use]
pub fn local_offset_seconds() -> i32 {
    ca_fs::local_offset_seconds()
}

/// The offset already probed, without running the probe.
#[must_use]
pub fn probed_offset() -> Option<i32> {
    ca_fs::probed_offset()
}

/// Read the offset on a worker, once per process, then call `notify`.
///
/// A frame drawn before the read ends shows coordinated universal time, and
/// `notify` asks for the frame that corrects it.
pub fn probe_offset(notify: &Arc<dyn Fn() + Send + Sync>) {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let notify = Arc::clone(notify);
    std::thread::spawn(move || {
        let _ = ca_fs::local_offset_seconds();
        notify();
    });
}

#[cfg(test)]
mod tests {
    use super::{civil_from_days, format_bytes, format_stamp, probe_offset, probed_offset};
    use std::sync::Arc;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn a_probe_makes_the_offset_known() {
        let notify: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
        probe_offset(&notify);
        assert!(crate::testing::wait_until(Duration::from_secs(5), || {
            probed_offset().is_some()
        }));
    }

    #[test]
    fn a_size_carries_a_thousands_separator() {
        assert_eq!(format_bytes(0), "0");
        assert_eq!(format_bytes(999), "999");
        assert_eq!(format_bytes(1_000), "1,000");
        assert_eq!(format_bytes(45_739), "45,739");
        assert_eq!(format_bytes(1_234_567_890), "1,234,567,890");
    }

    #[test]
    fn a_timestamp_uses_a_twelve_hour_clock() {
        assert_eq!(format_stamp(Some(UNIX_EPOCH), 0), "1/1/1970 12:00:00 AM");
        let noon = UNIX_EPOCH + Duration::from_secs(12 * 3_600);
        assert_eq!(format_stamp(Some(noon), 0), "1/1/1970 12:00:00 PM");
        let afternoon = UNIX_EPOCH + Duration::from_secs(13 * 3_600 + 5 * 60 + 9);
        assert_eq!(format_stamp(Some(afternoon), 0), "1/1/1970 1:05:09 PM");
    }

    #[test]
    fn a_missing_timestamp_renders_empty() {
        assert_eq!(format_stamp(None, 0), "");
    }

    #[test]
    fn an_offset_moves_the_value_into_its_own_zone() {
        let time = UNIX_EPOCH + Duration::from_secs(3_600);
        assert_eq!(format_stamp(Some(time), -3_600), "1/1/1970 12:00:00 AM");
        assert_eq!(format_stamp(Some(time), -7_200), "12/31/1969 11:00:00 PM");
    }

    #[test]
    fn a_leap_day_renders_correctly() {
        let time = UNIX_EPOCH + Duration::from_secs(1_709_164_800);
        assert_eq!(format_stamp(Some(time), 0), "2/29/2024 12:00:00 AM");
    }

    #[test]
    fn civil_dates_run_before_the_epoch_too() {
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }
}
