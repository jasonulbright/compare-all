//! Reading a cell's text as a typed value.
//!
//! A column's declared or detected type decides how its cells are read. Two
//! cells of a number column compare by value, so `1.0` and `1` are equal. Two
//! cells of a date column compare by instant, so the same day written in two
//! formats is equal. Text compares as characters under the column's case and
//! whitespace settings.

use crate::decimal::Decimal;
use crate::regional::Regional;
use crate::schema::ColumnType;

/// A date and time reduced to a single instant.
///
/// The instant is seconds from the start of 1970 in the calendar the text was
/// written in. No time zone is applied: a comparison of two sides only needs
/// the two values to be on the same scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DateTime {
    seconds: i64,
}

impl DateTime {
    /// Seconds from the start of 1970.
    #[must_use]
    pub fn seconds(self) -> i64 {
        self.seconds
    }

    /// Distance between two instants in seconds, never negative.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn distance(self, other: Self) -> f64 {
        // Second counts for any representable date stay far below the range
        // f64 holds exactly, so the conversion is lossless here.
        self.seconds.saturating_sub(other.seconds).unsigned_abs() as f64
    }
}

/// A cell read under a column's type.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum CellValue<'a> {
    /// The cell is absent from its row.
    Missing,
    /// The cell holds no characters.
    Empty,
    /// The cell parsed as a number.
    Number(Decimal),
    /// The cell parsed as a date and time.
    Date(DateTime),
    /// The cell is compared as characters.
    Text(&'a str),
    /// The cell does not read as the column's type. It compares as characters.
    Invalid(&'a str),
}

impl<'a> CellValue<'a> {
    /// Read a cell's text under a column type.
    ///
    /// Text that the column's type cannot read comes back as
    /// [`CellValue::Invalid`], which compares as characters.
    #[must_use]
    pub fn read(text: &'a str, column_type: &ColumnType, regional: &Regional) -> Self {
        if text.is_empty() {
            return Self::Empty;
        }
        match column_type {
            ColumnType::Number => {
                parse_number(text.trim(), regional).map_or(Self::Invalid(text), Self::Number)
            }
            ColumnType::DateTime => {
                parse_date(text.trim(), regional).map_or(Self::Invalid(text), Self::Date)
            }
            _ => Self::Text(text),
        }
    }

    /// Whether the cell holds nothing, whether because the row is short or
    /// because the cell is present and empty.
    #[must_use]
    pub fn is_blank(&self) -> bool {
        matches!(self, Self::Missing | Self::Empty)
    }

    /// Whether the cell does not read as its column's type.
    #[must_use]
    pub fn is_invalid(&self) -> bool {
        matches!(self, Self::Invalid(_))
    }
}

/// Read text as a number under the given conventions.
///
/// The text may carry a leading minus sign, grouped digits, one decimal
/// separator and an exponent. Surrounding whitespace is ignored. Grouping is
/// accepted only in the documented form: a first group of one to three digits
/// and every later group of exactly three. A leading plus sign is not a
/// documented form and is rejected, so text that carries one compares as
/// characters. Returns `None` when the whole text is not a number.
#[must_use]
pub fn parse_number(text: &str, regional: &Regional) -> Option<Decimal> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut chars = trimmed.chars().peekable();
    let negative = chars.next_if_eq(&'-').is_some();

    let mut integer: Vec<u8> = Vec::new();
    let mut groups: Vec<usize> = Vec::new();
    let mut group = 0usize;
    let mut grouped = false;
    while let Some(ch) = chars.peek().copied() {
        if let Some(digit) = ascii_digit(ch) {
            integer.push(digit);
            group += 1;
        } else if Some(ch) == regional.thousands_separator {
            // A grouping character can only stand between digit groups.
            if group == 0 {
                return None;
            }
            groups.push(group);
            group = 0;
            grouped = true;
        } else {
            break;
        }
        chars.next();
    }
    if grouped {
        groups.push(group);
        let mut sizes = groups.iter();
        if !sizes.next().is_some_and(|size| (1..=3).contains(size)) {
            return None;
        }
        if !sizes.all(|size| *size == 3) {
            return None;
        }
    }

    let mut fraction: Vec<u8> = Vec::new();
    if chars.next_if_eq(&regional.decimal_separator).is_some() {
        while let Some(ch) = chars.peek().copied() {
            let Some(digit) = ascii_digit(ch) else { break };
            fraction.push(digit);
            chars.next();
        }
    }
    if integer.is_empty() && fraction.is_empty() {
        return None;
    }

    let mut exponent = 0i32;
    if chars.next_if(|ch| *ch == 'e' || *ch == 'E').is_some() {
        let exponent_negative = match chars.peek().copied() {
            Some('-') => {
                chars.next();
                true
            }
            Some('+') => {
                chars.next();
                false
            }
            _ => false,
        };
        let mut seen = false;
        let mut value = 0i32;
        while let Some(ch) = chars.peek().copied() {
            let Some(digit) = ascii_digit(ch) else { break };
            seen = true;
            value = value.checked_mul(10)?.checked_add(i32::from(digit))?;
            if value > MAX_WRITTEN_EXPONENT {
                return None;
            }
            chars.next();
        }
        if !seen {
            return None;
        }
        exponent = if exponent_negative { -value } else { value };
    }
    if chars.next().is_some() {
        return None;
    }
    Decimal::from_parts(negative, &integer, &fraction, exponent)
}

/// Whether a number-like value starts its integer part with zero followed by
/// another digit. Such spellings are identifiers and must not be normalized
/// into the same key or comparison value as the spelling without the zero.
#[must_use]
pub fn has_leading_zero_integer(text: &str) -> bool {
    let body = text.trim().strip_prefix('-').unwrap_or(text.trim());
    let mut chars = body.chars();
    chars.next() == Some('0') && chars.next().is_some_and(|ch| ch.is_ascii_digit())
}

/// Largest written exponent a number may carry.
const MAX_WRITTEN_EXPONENT: i32 = 100_000;

#[inline]
fn ascii_digit(ch: char) -> Option<u8> {
    ch.is_ascii_digit().then(|| {
        u8::try_from(u32::from(ch))
            .unwrap_or(b'0')
            .wrapping_sub(b'0')
    })
}

/// Read text as a date, with an optional time of day.
///
/// The three date parts are separated by the regional date separator and read
/// in the regional order. A two digit year below 70 is read as the twenty
/// first century, otherwise as the twentieth. An `ISO 8601` style date with a
/// dash separator is accepted whatever the regional separator is. A time of
/// day may follow after a space or a `T`, written `hh:mm` or `hh:mm:ss`.
/// Returns `None` when the whole text is not a date.
#[must_use]
pub fn parse_date(text: &str, regional: &Regional) -> Option<DateTime> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (date_part, time_part) = split_date_and_time(trimmed);
    let (parts, order) = split_date_parts(date_part, regional)?;
    let (year_at, month_at, day_at) = order;
    let year = parts.get(year_at)?.parse::<i64>().ok()?;
    let month = parts.get(month_at)?.parse::<u32>().ok()?;
    let day = parts.get(day_at)?.parse::<u32>().ok()?;
    let year = if parts.get(year_at)?.len() <= 2 {
        if year < 70 {
            2000 + year
        } else {
            1900 + year
        }
    } else {
        year
    };
    if !(1..=12).contains(&month) {
        return None;
    }
    // A day past the length of its month names no instant. Normalizing it into
    // the next month would make two different dates one value.
    if day < 1 || day > days_in_month(year, month) {
        return None;
    }
    let days = days_from_civil(year, month, day)?;
    let seconds_of_day = match time_part {
        Some(time) => parse_time(time)?,
        None => 0,
    };
    Some(DateTime {
        seconds: days.checked_mul(86_400)?.checked_add(seconds_of_day)?,
    })
}

fn split_date_and_time(text: &str) -> (&str, Option<&str>) {
    if let Some(at) = text.find(['T', ' ']) {
        let (head, tail) = text.split_at(at);
        let tail = tail.get(1..).unwrap_or("").trim();
        if tail.is_empty() {
            (head, None)
        } else {
            (head, Some(tail))
        }
    } else {
        (text, None)
    }
}

/// The three text parts of a date and where the year, month and day sit.
type DateParts<'a> = (Vec<&'a str>, (usize, usize, usize));

fn split_date_parts<'a>(text: &'a str, regional: &Regional) -> Option<DateParts<'a>> {
    let mut separator = regional.date_separator;
    let mut order = regional.date_order.positions()?;
    if !text.contains(separator) && text.contains('-') {
        // A dash separated date is written year first whatever the regional
        // separator is, so it reads unambiguously on any side.
        separator = '-';
        order = (0, 1, 2);
    }
    let parts: Vec<&str> = text.split(separator).collect();
    if parts.len() != 3 || parts.iter().any(|part| part.is_empty()) {
        return None;
    }
    if parts
        .iter()
        .any(|part| !part.chars().all(|ch| ch.is_ascii_digit()))
    {
        return None;
    }
    Some((parts, order))
}

fn parse_time(text: &str) -> Option<i64> {
    let text = text.trim();
    let parts: Vec<&str> = text.split(':').collect();
    if parts.len() < 2 || parts.len() > 3 {
        return None;
    }
    let hours = parts.first()?.parse::<i64>().ok()?;
    let minutes = parts.get(1)?.parse::<i64>().ok()?;
    let seconds = match parts.get(2) {
        Some(part) => part.parse::<i64>().ok()?,
        None => 0,
    };
    // Second 60 is not a documented time of day. Accepting it would roll the
    // value into the next minute and make two different times one value.
    if !(0..24).contains(&hours) || !(0..60).contains(&minutes) || !(0..60).contains(&seconds) {
        return None;
    }
    Some(hours * 3_600 + minutes * 60 + seconds)
}

/// Whether a year carries a leap day.
fn is_leap_year(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// Number of days in a month of a year.
fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

/// Days from the start of 1970 to a proleptic Gregorian date.
fn days_from_civil(year: i64, month: u32, day: u32) -> Option<i64> {
    let month = i64::from(month);
    let day = i64::from(day);
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_shift = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * month_shift + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era.checked_mul(146_097)?
        .checked_add(day_of_era)?
        .checked_sub(719_468)
}

/// Compare two texts as characters under case and whitespace settings.
///
/// With `ignore_whitespace`, runs of whitespace collapse to one space and
/// leading and trailing whitespace is dropped, so the number of blanks before,
/// after or between words stops mattering.
#[must_use]
pub fn text_equal(left: &str, right: &str, ignore_case: bool, ignore_whitespace: bool) -> bool {
    if !ignore_case && !ignore_whitespace {
        return left == right;
    }
    let mut left_parts = normalized_chars(left, ignore_case, ignore_whitespace);
    let mut right_parts = normalized_chars(right, ignore_case, ignore_whitespace);
    loop {
        match (left_parts.next(), right_parts.next()) {
            (None, None) => return true,
            (a, b) if a == b => {}
            _ => return false,
        }
    }
}

/// The normalized form of a text under case and whitespace settings.
#[must_use]
pub fn normalize_text(text: &str, ignore_case: bool, ignore_whitespace: bool) -> String {
    normalized_chars(text, ignore_case, ignore_whitespace).collect()
}

fn normalized_chars(
    text: &str,
    ignore_case: bool,
    ignore_whitespace: bool,
) -> impl Iterator<Item = char> + '_ {
    let source: &str = if ignore_whitespace { text.trim() } else { text };
    let mut in_space = false;
    source.chars().filter_map(move |ch| {
        if ignore_whitespace && ch.is_whitespace() {
            if in_space {
                return None;
            }
            in_space = true;
            return Some(' ');
        }
        in_space = false;
        if ignore_case {
            // Case folding one character at a time keeps the iterator lazy;
            // multi-character foldings are rare enough in tabular data that
            // the simple mapping is the right trade.
            Some(ch.to_lowercase().next().unwrap_or(ch))
        } else {
            Some(ch)
        }
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;
    use crate::regional::DateOrder;

    fn value(text: &str) -> Decimal {
        parse_number(text, &Regional::dot_decimal()).unwrap()
    }

    #[test]
    fn dot_decimal_numbers_parse() {
        let regional = Regional::dot_decimal();
        assert_eq!(parse_number("1", &regional), Some(value("1.0")));
        assert_eq!(
            parse_number(" -1,234.50 ", &regional),
            Some(value("-1234.5"))
        );
        assert_eq!(parse_number("1e3", &regional), Some(value("1000")));
        assert_eq!(parse_number("1E+3", &regional), Some(value("1000")));
        assert_eq!(parse_number("1e-3", &regional), Some(value("0.001")));
        assert_eq!(parse_number(".5", &regional), Some(value("0.5")));
        assert_eq!(parse_number("", &regional), None);
        assert_eq!(parse_number("abc", &regional), None);
        assert_eq!(parse_number("1.2.3", &regional), None);
        assert_eq!(parse_number(",100", &regional), None);
        assert_eq!(parse_number("1e", &regional), None);
        assert_eq!(parse_number("1-2", &regional), None);
        assert_eq!(parse_number("1 2", &regional), None);
    }

    #[test]
    fn comma_decimal_numbers_parse() {
        let regional = Regional::comma_decimal();
        assert_eq!(
            parse_number("1.234,50", &regional),
            Some(parse_number("1234,5", &regional).unwrap())
        );
        assert_eq!(
            parse_number("0,5", &regional),
            Some(parse_number("0,50", &regional).unwrap())
        );
    }

    #[test]
    fn grouping_must_use_three_digit_groups() {
        let regional = Regional::dot_decimal();
        assert_eq!(parse_number("1,5", &regional), None);
        assert_eq!(parse_number("1,23,456", &regional), None);
        assert_eq!(parse_number("12,34", &regional), None);
        assert_eq!(parse_number("1,0000", &regional), None);
        assert_eq!(parse_number("1,", &regional), None);
        assert_eq!(parse_number("1.5,5", &regional), None);
        assert_eq!(parse_number("1,000", &regional), Some(value("1000")));
        assert_eq!(parse_number("12,000", &regional), Some(value("12000")));
        assert_eq!(
            parse_number("123,000,000", &regional),
            Some(value("123000000"))
        );
    }

    #[test]
    fn a_leading_plus_sign_is_not_a_number() {
        let regional = Regional::dot_decimal();
        assert_eq!(parse_number("+1", &regional), None);
        assert_eq!(parse_number("+12345", &regional), None);
    }

    #[test]
    fn long_identifiers_do_not_collapse() {
        let regional = Regional::dot_decimal();
        let a = parse_number("12345678901234567890", &regional).unwrap();
        let b = parse_number("12345678901234567891", &regional).unwrap();
        assert_ne!(a, b);
        assert_ne!(a.key(), b.key());
    }

    #[test]
    fn tiny_numbers_do_not_collapse() {
        let regional = Regional::dot_decimal();
        let a = parse_number("1e-17", &regional).unwrap();
        let b = parse_number("2e-17", &regional).unwrap();
        assert_ne!(a, b);
        assert_ne!(
            parse_number("1e-300", &regional),
            parse_number("0", &regional)
        );
    }

    #[test]
    fn a_signed_zero_reads_as_zero() {
        let regional = Regional::dot_decimal();
        let minus = parse_number("-0", &regional).unwrap();
        let plain = parse_number("0.00", &regional).unwrap();
        assert_eq!(minus, plain);
        assert_eq!(minus.key(), plain.key());
    }

    #[test]
    fn dates_parse_in_the_regional_order() {
        let mdy = Regional::dot_decimal();
        let dmy = Regional {
            date_order: DateOrder::Dmy,
            ..Regional::dot_decimal()
        };
        let a = parse_date("03/04/2020", &mdy).unwrap();
        let b = parse_date("04/03/2020", &dmy).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn a_dash_date_reads_year_first_on_any_side() {
        let regional = Regional::dot_decimal();
        let iso = parse_date("2020-03-04", &regional).unwrap();
        let slash = parse_date("03/04/2020", &regional).unwrap();
        assert_eq!(iso, slash);
    }

    #[test]
    fn a_time_of_day_is_read() {
        let regional = Regional::dot_decimal();
        let midnight = parse_date("01/02/2020", &regional).unwrap();
        let noon = parse_date("01/02/2020 12:00:00", &regional).unwrap();
        assert_eq!(noon.seconds() - midnight.seconds(), 43_200);
        assert_eq!(noon.distance(midnight), 43_200.0);
    }

    #[test]
    fn two_digit_years_pivot_at_seventy() {
        let regional = Regional::dot_decimal();
        let low = parse_date("01/01/69", &regional).unwrap();
        let high = parse_date("01/01/70", &regional).unwrap();
        assert!(low > high);
        assert_eq!(high.seconds(), 0);
    }

    #[test]
    fn non_dates_are_rejected() {
        let regional = Regional::dot_decimal();
        assert_eq!(parse_date("hello", &regional), None);
        assert_eq!(parse_date("13/45/2020", &regional), None);
        assert_eq!(parse_date("1/2", &regional), None);
        assert_eq!(parse_date("1//2020", &regional), None);
    }

    #[test]
    fn an_impossible_day_is_not_a_date() {
        let regional = Regional::dot_decimal();
        assert_eq!(parse_date("02/30/2020", &regional), None);
        assert_eq!(parse_date("2020-02-30", &regional), None);
        assert!(parse_date("2020-02-29", &regional).is_some());
        assert_eq!(parse_date("2021-02-29", &regional), None);
        assert_eq!(parse_date("1900-02-29", &regional), None);
        assert!(parse_date("2000-02-29", &regional).is_some());
        assert_eq!(parse_date("04/31/2020", &regional), None);
        assert_eq!(parse_date("2020-04-31", &regional), None);
        assert!(parse_date("2020-01-31", &regional).is_some());
        assert_eq!(parse_date("02/00/2020", &regional), None);
    }

    #[test]
    fn second_sixty_is_not_a_time() {
        let regional = Regional::dot_decimal();
        assert_eq!(parse_date("01/02/2020 00:00:60", &regional), None);
        assert!(parse_date("01/02/2020 00:59:59", &regional).is_some());
    }

    #[test]
    fn a_cell_reads_under_its_column_type() {
        let regional = Regional::dot_decimal();
        assert!(matches!(
            CellValue::read("1.5", &ColumnType::Number, &regional),
            CellValue::Number(_)
        ));
        assert!(CellValue::read("n/a", &ColumnType::Number, &regional).is_invalid());
        assert!(CellValue::read("2020-02-30", &ColumnType::DateTime, &regional).is_invalid());
        assert!(CellValue::read("", &ColumnType::Number, &regional).is_blank());
        assert!(matches!(
            CellValue::read("x", &ColumnType::Text, &regional),
            CellValue::Text("x")
        ));
    }

    #[test]
    fn epoch_is_the_start_of_nineteen_seventy() {
        assert_eq!(days_from_civil(1970, 1, 1), Some(0));
        assert_eq!(days_from_civil(1970, 1, 2), Some(1));
        assert_eq!(days_from_civil(1969, 12, 31), Some(-1));
        assert_eq!(days_from_civil(2000, 3, 1), Some(11_017));
    }

    #[test]
    fn text_comparison_honors_its_settings() {
        assert!(text_equal("abc", "abc", false, false));
        assert!(!text_equal("abc", "ABC", false, false));
        assert!(text_equal("abc", "ABC", true, false));
        assert!(text_equal(" a  b ", "a b", false, true));
        assert!(!text_equal("ab", "a b", false, true));
        assert!(text_equal(" A  B ", "a b", true, true));
    }

    #[test]
    fn normalization_matches_the_comparison() {
        assert_eq!(normalize_text(" A  B ", true, true), "a b");
        assert_eq!(normalize_text(" A  B ", false, false), " A  B ");
    }
}
