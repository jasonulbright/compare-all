//! Number and date conventions used when interpreting cell text.
//!
//! A side of a comparison carries its own conventions, so a file written with
//! a comma decimal separator can be compared against one written with a dot.

use crate::Unknown;
use serde::{Deserialize, Serialize};

/// Order of the three parts of a written date.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum DateOrder {
    /// Month, then day, then year.
    #[default]
    Mdy,
    /// Day, then month, then year.
    Dmy,
    /// Year, then month, then day.
    Ymd,
    /// An order written by another build, carried through unchanged.
    #[serde(untagged)]
    Unknown(serde_json::Value),
}

impl DateOrder {
    /// Positions of year, month and day in a three part date, or `None` for an
    /// order this build does not understand.
    #[must_use]
    pub fn positions(&self) -> Option<(usize, usize, usize)> {
        match self {
            Self::Mdy => Some((2, 0, 1)),
            Self::Dmy => Some((2, 1, 0)),
            Self::Ymd => Some((0, 1, 2)),
            Self::Unknown(_) => None,
        }
    }
}

/// Conventions for reading numeric and date fields on one side.
///
/// `use_system` records the user's intent to follow the operating system. The
/// engine itself never reads the operating system: the caller resolves the
/// system values and writes them into the separator fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Regional {
    /// Take the conventions from the operating system's regional settings.
    pub use_system: bool,
    /// Character separating the whole part of a number from its fraction.
    pub decimal_separator: char,
    /// Character grouping the digits of the whole part, or `None` for no
    /// grouping.
    pub thousands_separator: Option<char>,
    /// Order of the parts of a written date.
    pub date_order: DateOrder,
    /// Character separating the parts of a written date.
    pub date_separator: char,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, skip_serializing_if = "Unknown::is_empty")]
    pub unknown: Unknown,
}

impl Default for Regional {
    fn default() -> Self {
        Self {
            use_system: true,
            decimal_separator: '.',
            thousands_separator: Some(','),
            date_order: DateOrder::Mdy,
            date_separator: '/',
            unknown: Unknown::new(),
        }
    }
}

impl Regional {
    /// Conventions with a dot decimal separator and a comma thousands
    /// separator, not following the operating system.
    #[must_use]
    pub fn dot_decimal() -> Self {
        Self {
            use_system: false,
            ..Self::default()
        }
    }

    /// Conventions with a comma decimal separator and a dot thousands
    /// separator, not following the operating system.
    #[must_use]
    pub fn comma_decimal() -> Self {
        Self {
            use_system: false,
            decimal_separator: ',',
            thousands_separator: Some('.'),
            date_order: DateOrder::Dmy,
            date_separator: '.',
            unknown: Unknown::new(),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn known_orders_report_positions() {
        assert_eq!(DateOrder::Mdy.positions(), Some((2, 0, 1)));
        assert_eq!(DateOrder::Dmy.positions(), Some((2, 1, 0)));
        assert_eq!(DateOrder::Ymd.positions(), Some((0, 1, 2)));
    }

    #[test]
    fn an_unknown_order_survives_a_round_trip() {
        let order: DateOrder = serde_json::from_str("\"ydm\"").unwrap();
        assert!(matches!(order, DateOrder::Unknown(_)));
        assert_eq!(order.positions(), None);
        assert_eq!(serde_json::to_string(&order).unwrap(), "\"ydm\"");
    }

    #[test]
    fn unknown_fields_survive_a_round_trip() {
        let text = r#"{"useSystem":false,"decimalSeparator":",","currencyPrefix":"kr"}"#;
        let regional: Regional = serde_json::from_str(text).unwrap();
        assert_eq!(regional.decimal_separator, ',');
        assert!(regional.unknown.contains_key("currencyPrefix"));
        let back = serde_json::to_string(&regional).unwrap();
        assert!(back.contains("\"currencyPrefix\":\"kr\""));
    }
}
