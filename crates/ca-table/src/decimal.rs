//! Exact decimal values.
//!
//! A cell that holds a number is read into a sign, a string of significant
//! digits and a power of ten. Two such values compare without any rounding, so
//! two identifiers that differ in their twentieth digit stay different and
//! `007` stays distinct from `7` only where the reader keeps the leading zero.
//!
//! Binary floating point cannot hold every decimal a file can write, so it is
//! never used to decide equality or ordering. It is used only to move a
//! configured tolerance into this representation and to hand a value to a
//! caller that wants one.

use std::cmp::Ordering;
use std::fmt::Write as _;

/// Largest number of significant digits a value keeps. Longer input is not a
/// number and compares as characters.
const MAX_DIGITS: usize = 4_096;

/// Largest exponent magnitude a value accepts.
const MAX_EXPONENT: i32 = 6_000;

/// Largest number of digits an exact subtraction produces.
const MAX_SPAN: usize = 16_384;

/// Fractional digits printed when a double is written out in full. A finite
/// double needs at most 1074 of them, so the text is the value itself and not
/// a rounding of it.
const EXACT_FRACTION_DIGITS: usize = 1_080;

/// A decimal number held exactly.
///
/// The value is `digits * 10^exponent`, negated when `negative` is set. The
/// form is canonical: there are no leading or trailing zero digits, and zero
/// carries no digits, no exponent and no sign.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Decimal {
    negative: bool,
    digits: Vec<u8>,
    exponent: i32,
}

impl Decimal {
    /// The value zero.
    #[must_use]
    pub fn zero() -> Self {
        Self::default()
    }

    /// Whether the value is zero.
    #[must_use]
    pub fn is_zero(&self) -> bool {
        self.digits.is_empty()
    }

    /// Whether the value is below zero.
    #[must_use]
    pub fn is_negative(&self) -> bool {
        self.negative
    }

    /// Number of significant digits.
    #[must_use]
    pub fn significant_digits(&self) -> usize {
        self.digits.len()
    }

    /// Build a value from a sign, the digits before the separator, the digits
    /// after it and a power of ten.
    ///
    /// Returns `None` when the value needs more digits or a wider exponent
    /// than the representation holds.
    #[must_use]
    pub fn from_parts(
        negative: bool,
        integer: &[u8],
        fraction: &[u8],
        exponent: i32,
    ) -> Option<Self> {
        let mut digits = Vec::with_capacity(integer.len() + fraction.len());
        digits.extend_from_slice(integer);
        digits.extend_from_slice(fraction);
        let shift = i32::try_from(fraction.len()).ok()?;
        normalize(negative, digits, exponent.checked_sub(shift)?)
    }

    /// Read a double exactly.
    ///
    /// Returns `None` for a value that is not finite.
    #[must_use]
    pub fn from_f64(value: f64) -> Option<Self> {
        if !value.is_finite() {
            return None;
        }
        let text = format!("{value:.EXACT_FRACTION_DIGITS$}");
        let body = text.strip_prefix('-');
        let negative = body.is_some();
        let body = body.unwrap_or(&text);
        let (integer, fraction) = match body.split_once('.') {
            Some((head, tail)) => (head, tail),
            None => (body, ""),
        };
        let integer: Vec<u8> = integer.bytes().filter_map(digit_of).collect();
        let fraction: Vec<u8> = fraction.bytes().filter_map(digit_of).collect();
        Self::from_parts(negative, &integer, &fraction, 0)
    }

    /// The nearest double to this value.
    #[must_use]
    pub fn to_f64(&self) -> f64 {
        if self.is_zero() {
            return 0.0;
        }
        self.scientific().parse::<f64>().unwrap_or(f64::NAN)
    }

    /// A text form that is equal for equal values and different for different
    /// ones, for use as a lookup key.
    #[must_use]
    pub fn key(&self) -> String {
        if self.is_zero() {
            // The sign of zero carries no value, so both writings share a key.
            return "0".to_owned();
        }
        self.scientific()
    }

    /// The distance between two values, never negative.
    ///
    /// Returns `None` when the two values are too far apart in magnitude for
    /// the difference to be written out.
    #[must_use]
    pub fn abs_difference(&self, other: &Self) -> Option<Self> {
        if self == other {
            return Some(Self::zero());
        }
        let (left, right, exponent) = align_pair(self, other)?;
        let digits = if self.negative == other.negative {
            subtract_magnitude(&left, &right)
        } else {
            add_magnitude(&left, &right)
        };
        normalize(false, digits, exponent)
    }

    fn scientific(&self) -> String {
        let mut out = String::with_capacity(self.digits.len() + 8);
        if self.negative {
            out.push('-');
        }
        for digit in &self.digits {
            out.push(char::from(b'0'.saturating_add(*digit)));
        }
        let _ = write!(out, "e{}", self.exponent);
        out
    }
}

impl PartialOrd for Decimal {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Decimal {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.is_zero(), other.is_zero()) {
            (true, true) => Ordering::Equal,
            (true, false) => {
                if other.negative {
                    Ordering::Greater
                } else {
                    Ordering::Less
                }
            }
            (false, true) => {
                if self.negative {
                    Ordering::Less
                } else {
                    Ordering::Greater
                }
            }
            (false, false) => match (self.negative, other.negative) {
                (true, false) => Ordering::Less,
                (false, true) => Ordering::Greater,
                (true, true) => compare_magnitude(self, other).reverse(),
                (false, false) => compare_magnitude(self, other),
            },
        }
    }
}

fn digit_of(byte: u8) -> Option<u8> {
    byte.is_ascii_digit().then(|| byte.wrapping_sub(b'0'))
}

fn normalize(negative: bool, mut digits: Vec<u8>, mut exponent: i32) -> Option<Decimal> {
    match digits.iter().position(|digit| *digit != 0) {
        Some(at) => {
            digits.drain(..at);
        }
        None => return Some(Decimal::zero()),
    }
    while digits.last() == Some(&0) {
        digits.pop();
        exponent = exponent.checked_add(1)?;
    }
    if digits.len() > MAX_DIGITS || exponent.abs() > MAX_EXPONENT {
        return None;
    }
    Some(Decimal {
        negative,
        digits,
        exponent,
    })
}

/// Compares two non-zero values by magnitude alone.
fn compare_magnitude(left: &Decimal, right: &Decimal) -> Ordering {
    let left_top = leading_place(left);
    let right_top = leading_place(right);
    if left_top != right_top {
        return left_top.cmp(&right_top);
    }
    let len = left.digits.len().max(right.digits.len());
    for index in 0..len {
        let a = left.digits.get(index).copied().unwrap_or(0);
        let b = right.digits.get(index).copied().unwrap_or(0);
        if a != b {
            return a.cmp(&b);
        }
    }
    Ordering::Equal
}

/// The power of ten of the most significant digit, plus one.
fn leading_place(value: &Decimal) -> i64 {
    i64::from(value.exponent) + i64::try_from(value.digits.len()).unwrap_or(i64::MAX)
}

/// Writes both values over the same power of ten and pads them to one width.
fn align_pair(left: &Decimal, right: &Decimal) -> Option<(Vec<u8>, Vec<u8>, i32)> {
    let exponent = left.exponent.min(right.exponent);
    let left_shift = usize::try_from(left.exponent.checked_sub(exponent)?).ok()?;
    let right_shift = usize::try_from(right.exponent.checked_sub(exponent)?).ok()?;
    let left_len = left.digits.len().checked_add(left_shift)?;
    let right_len = right.digits.len().checked_add(right_shift)?;
    let len = left_len.max(right_len);
    if len > MAX_SPAN {
        return None;
    }
    let mut a = vec![0u8; len];
    let mut b = vec![0u8; len];
    place(&mut a, &left.digits, len - left_len);
    place(&mut b, &right.digits, len - right_len);
    Some((a, b, exponent))
}

fn place(target: &mut [u8], digits: &[u8], at: usize) {
    for (index, digit) in digits.iter().enumerate() {
        if let Some(slot) = target.get_mut(at + index) {
            *slot = *digit;
        }
    }
}

fn add_magnitude(left: &[u8], right: &[u8]) -> Vec<u8> {
    let len = left.len().max(right.len());
    let mut out = vec![0u8; len + 1];
    let mut carry = 0u8;
    for index in (0..len).rev() {
        let sum =
            left.get(index).copied().unwrap_or(0) + right.get(index).copied().unwrap_or(0) + carry;
        if let Some(slot) = out.get_mut(index + 1) {
            *slot = sum % 10;
        }
        carry = sum / 10;
    }
    if let Some(slot) = out.first_mut() {
        *slot = carry;
    }
    out
}

fn subtract_magnitude(left: &[u8], right: &[u8]) -> Vec<u8> {
    let (high, low) = if left < right {
        (right, left)
    } else {
        (left, right)
    };
    let len = high.len();
    let mut out = vec![0u8; len];
    let mut borrow = 0i16;
    for index in (0..len).rev() {
        let mut value = i16::from(high.get(index).copied().unwrap_or(0))
            - i16::from(low.get(index).copied().unwrap_or(0))
            - borrow;
        if value < 0 {
            value += 10;
            borrow = 1;
        } else {
            borrow = 0;
        }
        if let Some(slot) = out.get_mut(index) {
            *slot = u8::try_from(value).unwrap_or(0);
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;

    fn decimal(text: &str) -> Decimal {
        let body = text.strip_prefix('-');
        let negative = body.is_some();
        let body = body.unwrap_or(text);
        let (integer, fraction) = body.split_once('.').unwrap_or((body, ""));
        let integer: Vec<u8> = integer.bytes().filter_map(digit_of).collect();
        let fraction: Vec<u8> = fraction.bytes().filter_map(digit_of).collect();
        Decimal::from_parts(negative, &integer, &fraction, 0).unwrap()
    }

    #[test]
    fn equal_values_written_differently_share_one_form() {
        assert_eq!(decimal("1"), decimal("1.0"));
        assert_eq!(decimal("2.50"), decimal("2.5"));
        assert_eq!(decimal("0"), decimal("-0"));
        assert_eq!(decimal("0").key(), decimal("-0").key());
        assert_eq!(decimal("1").key(), decimal("1.000").key());
    }

    #[test]
    fn long_identifiers_stay_distinct() {
        let a = decimal("12345678901234567890");
        let b = decimal("12345678901234567891");
        assert_ne!(a, b);
        assert_ne!(a.key(), b.key());
        assert!(a < b);
    }

    #[test]
    fn tiny_values_stay_distinct() {
        assert_ne!(
            decimal("0.00000000000000001"),
            decimal("0.00000000000000002")
        );
        assert!(decimal("0.00000000000000001") < decimal("0.00000000000000002"));
        assert_ne!(decimal("0.000000000000000000001"), decimal("0"));
    }

    #[test]
    fn ordering_covers_both_signs() {
        let mut values = [
            decimal("1"),
            decimal("-1"),
            decimal("0"),
            decimal("-10"),
            decimal("0.5"),
            decimal("-0.5"),
        ];
        values.sort();
        let written: Vec<String> = values.iter().map(Decimal::key).collect();
        assert_eq!(
            written,
            vec!["-1e1", "-1e0", "-5e-1", "0", "5e-1", "1e0"]
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<String>>()
        );
    }

    #[test]
    fn differences_are_exact() {
        assert_eq!(
            decimal("3").abs_difference(&decimal("1")),
            Some(decimal("2"))
        );
        assert_eq!(
            decimal("1").abs_difference(&decimal("3")),
            Some(decimal("2"))
        );
        assert_eq!(
            decimal("1").abs_difference(&decimal("-1")),
            Some(decimal("2"))
        );
        assert_eq!(
            decimal("-1").abs_difference(&decimal("-4")),
            Some(decimal("3"))
        );
        assert_eq!(
            decimal("0.1").abs_difference(&decimal("0.2")),
            Some(decimal("0.1"))
        );
        assert_eq!(
            decimal("100000000000000000000").abs_difference(&decimal("100000000000000000001")),
            Some(decimal("1"))
        );
        assert_eq!(
            decimal("5").abs_difference(&decimal("5")),
            Some(Decimal::zero())
        );
    }

    #[test]
    fn a_double_reads_as_its_exact_value() {
        let tenth = Decimal::from_f64(0.1).unwrap();
        assert_ne!(tenth, decimal("0.1"));
        assert!(tenth > decimal("0.1"));
        assert_eq!(Decimal::from_f64(0.5), Some(decimal("0.5")));
        assert_eq!(Decimal::from_f64(3_600.0), Some(decimal("3600")));
        assert_eq!(Decimal::from_f64(0.0), Some(Decimal::zero()));
        assert_eq!(Decimal::from_f64(f64::NAN), None);
        assert_eq!(Decimal::from_f64(f64::INFINITY), None);
    }

    #[test]
    fn a_value_converts_back_to_a_double() {
        assert_eq!(decimal("1.5").to_f64(), 1.5);
        assert_eq!(decimal("-2").to_f64(), -2.0);
        assert_eq!(Decimal::zero().to_f64(), 0.0);
    }

    #[test]
    fn oversized_input_is_rejected() {
        let digits = vec![1u8; MAX_DIGITS + 1];
        assert_eq!(Decimal::from_parts(false, &digits, &[], 0), None);
        assert_eq!(
            Decimal::from_parts(false, &[1], &[], MAX_EXPONENT + 1),
            None
        );
    }
}
