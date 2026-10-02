//! Settings owned by the table comparison session kind.

use super::common::{
    AlignmentAlgorithm, FormatOverride, FormatSettings, SpecsOverride, SpecsSettings,
};
use super::defaults::provisional;
use super::macros::{settings_composite, settings_group};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How sheets or columns on the two sides are paired.
///
/// A tagged variant with no data of its own still serializes as an object, so
/// it carries the flattened `unknown` map.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "mode",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
#[non_exhaustive]
pub enum TablePairing {
    /// Pair by position in file order on both sides.
    Unaligned {
        /// Fields written by another build, preserved verbatim.
        #[serde(
            flatten,
            default,
            skip_serializing_if = "std::collections::BTreeMap::is_empty"
        )]
        unknown: std::collections::BTreeMap<String, Value>,
    },
    /// Keep left order and reorder the right side to match names. Name
    /// matching ignores capitalization and whitespace.
    ByLeftName {
        /// Fields written by another build, preserved verbatim.
        #[serde(
            flatten,
            default,
            skip_serializing_if = "std::collections::BTreeMap::is_empty"
        )]
        unknown: std::collections::BTreeMap<String, Value>,
    },
    /// Keep right order and reorder the left side to match names.
    ByRightName {
        /// Fields written by another build, preserved verbatim.
        #[serde(
            flatten,
            default,
            skip_serializing_if = "std::collections::BTreeMap::is_empty"
        )]
        unknown: std::collections::BTreeMap<String, Value>,
    },
    /// Explicit pairs, given as zero-based indexes; a missing side is `None`.
    Custom {
        /// Pairs in display order.
        pairs: Vec<(Option<u32>, Option<u32>)>,
        /// Fields written by another build, preserved verbatim.
        #[serde(
            flatten,
            default,
            skip_serializing_if = "std::collections::BTreeMap::is_empty"
        )]
        unknown: std::collections::BTreeMap<String, Value>,
    },
    /// A pairing rule this build does not understand, carried through
    /// unchanged.
    #[serde(untagged)]
    Unknown(Value),
}

impl Default for TablePairing {
    fn default() -> Self {
        TablePairing::unaligned()
    }
}

impl TablePairing {
    /// Pair by position in file order on both sides.
    #[must_use]
    pub fn unaligned() -> Self {
        TablePairing::Unaligned {
            unknown: std::collections::BTreeMap::new(),
        }
    }

    /// Pair by name, keeping the left order.
    #[must_use]
    pub fn by_left_name() -> Self {
        TablePairing::ByLeftName {
            unknown: std::collections::BTreeMap::new(),
        }
    }

    /// Pair by name, keeping the right order.
    #[must_use]
    pub fn by_right_name() -> Self {
        TablePairing::ByRightName {
            unknown: std::collections::BTreeMap::new(),
        }
    }
}

/// How a column's values are interpreted.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ColumnType {
    /// Determine the type from the data.
    #[default]
    General,
    /// Treat the values as text.
    Text,
    /// Treat the values as numbers.
    Numeric,
    /// Treat the values as dates or times.
    Date,
    /// A treatment this build does not understand, carried through unchanged.
    #[serde(untagged)]
    Unknown(Value),
}

/// Comparison treatment of one column, or of every column that inherits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[allow(clippy::struct_excessive_bools)]
pub struct ColumnHandling {
    /// Zero-based index of the comparison column, absent for the default that
    /// inheriting columns use.
    pub column: Option<u32>,
    /// The column controls sorting and alignment. With several keys the
    /// earlier comparison column takes precedence.
    pub key: bool,
    /// Inherit every remaining field from the session default handling.
    pub use_default: bool,
    /// How the values are interpreted.
    pub column_type: ColumnType,
    /// Differences anywhere in the column are unimportant.
    pub unimportant: bool,
    /// Capitalization differences are unimportant.
    pub ignore_character_case: bool,
    /// Differences in blanks before, after or between words are unimportant.
    pub ignore_whitespace: bool,
    /// Numbers may differ by this much before the difference is important.
    pub numeric_tolerance: f64,
    /// Dates may differ by this many seconds before the difference is
    /// important.
    pub date_tolerance_seconds: u32,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub unknown: std::collections::BTreeMap<String, Value>,
}

impl Default for ColumnHandling {
    fn default() -> Self {
        Self {
            column: None,
            key: false,
            use_default: true,
            column_type: ColumnType::General,
            unimportant: false,
            ignore_character_case: false,
            ignore_whitespace: false,
            numeric_tolerance: provisional::NUMERIC_TOLERANCE,
            date_tolerance_seconds: provisional::DATE_TOLERANCE_SECONDS,
            unknown: std::collections::BTreeMap::new(),
        }
    }
}

settings_group! {
    /// How sheets on the two sides are paired.
    SheetSettings / SheetOverride {
        /// Pairing rule applied to the sheet lists.
        pairing: TablePairing = TablePairing::unaligned(),
    }
}

settings_group! {
    /// How columns are paired and how their values are compared.
    ColumnSettings / ColumnOverride {
        /// Pairing rule applied to the column lists.
        pairing: TablePairing = TablePairing::unaligned(),
        /// Treatment inherited by every column that does not override it.
        default_handling: ColumnHandling = ColumnHandling::default(),
        /// Per-column treatment, keyed by the comparison column index.
        per_column: Vec<ColumnHandling> = Vec::new(),
    }
}

settings_group! {
    /// How rows are paired.
    RowSettings / RowOverride {
        /// Algorithm pairing the rows.
        algorithm: AlignmentAlgorithm = AlignmentAlgorithm::Standard,
        /// Show rows carrying important differences as separate added and
        /// deleted blocks instead of one changed pair.
        never_align_differences: bool = false,
        /// Rows scanned ahead and behind when looking for a match.
        skew_tolerance: u32 = provisional::SKEW_TOLERANCE_ROWS,
        /// Pair leftover unmatched rows by similarity.
        use_closeness_matching: bool = true,
        /// Reorder the rows of each side before pairing them.
        sort_before_alignment: bool = false,
    }
}

settings_composite! {
    /// Settings a table comparison session owns.
    TableCompareSettings / TableCompareOverride {
        /// Sides and description.
        specs: SpecsSettings => SpecsOverride,
        /// Format and encoding per side.
        format: FormatSettings => FormatOverride,
        /// Sheet pairing.
        sheets: SheetSettings => SheetOverride,
        /// Column pairing and per-column treatment.
        columns: ColumnSettings => ColumnOverride,
        /// Row pairing.
        rows: RowSettings => RowOverride,
    }
}
