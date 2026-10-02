//! Table comparison engine: delimited and fixed width data, key alignment, cell diff.
//!
//! The crate turns two tabular text files into an aligned, cell by cell
//! comparison. It is organised as five layers that can be used on their own:
//!
//! - [`parse`] reads delimited or fixed width text into a [`Table`] of rows of
//!   cells. Every cell keeps the byte range it came from, so a view can map a
//!   cell back to a position in the file.
//! - [`schema`] holds per column settings (type, case, whitespace, tolerance,
//!   key flag, importance) and pairs left columns with right columns.
//! - [`align`] pairs left rows with right rows, either by key columns or by
//!   running a sequence alignment over a per row normalization key.
//! - [`compare`] classifies each mapped cell of each aligned row pair, rolls
//!   the result up to a row status, and totals the rows.
//! - [`detect`] guesses delimiter, text qualifier, header presence and column
//!   types from a sample of the data.
//!
//! # Settings and forward compatibility
//!
//! Every settings struct carries an `unknown` map and every settings enum has
//! an `Unknown` arm holding raw JSON. A document written by a later build
//! survives a load and a save unchanged.
//!
//! # Limits
//!
//! A side is limited to 64 MiB of decoded text, one million cells and one
//! million rows. A comparison retains at most four million cell statuses.
//! These bounds keep the parser and the rectangular result from expanding a
//! small input into an unbounded allocation.
//!
//! # Example
//!
//! ```
//! use ca_table::{align, compare, parse, schema};
//!
//! let options = parse::ParseOptions::comma_separated();
//! let left = parse::parse("id,name\n1,Ann\n2,Bob\n", &options)?;
//! let right = parse::parse("id,name\n1,ANN\n3,Cy\n", &options)?;
//!
//! let mut settings = schema::SchemaSettings::default();
//! settings.default_handling.ignore_case = true;
//! let schema = schema::Schema::build(&left, &right, &settings);
//!
//! let alignment = align::align(&left, &right, &schema, &align::RowAlignOptions::default())?;
//! let result = compare::compare(&left, &right, &schema, &alignment)?;
//! // `Ann` against `ANN` differs only in case, which this schema disregards.
//! assert_eq!(result.totals.unimportant, 1);
//! assert_eq!(result.totals.rows_with_differences(), 2);
//! # Ok::<(), ca_table::TableError>(())
//! ```

pub mod align;
pub mod compare;
pub mod decimal;
pub mod detect;
pub mod parse;
pub mod regional;
pub mod schema;
pub mod value;
pub mod write;

pub use align::{Alignment, DuplicateKey, RowAlignOptions, RowAlignmentMode, RowPair};
pub use compare::{CellComparison, CellStatus, RowComparison, RowStatus, TableComparison, Totals};
pub use decimal::Decimal;
pub use detect::{detect_format, DetectOptions, DetectedFormat, SampledColumnType};
pub use parse::{
    parse, CellRef, FieldSyntax, FirstLineContains, ParseOptions, ParseWarning, Table,
};
pub use regional::{DateOrder, Regional};
pub use schema::{
    ColumnAlignment, ColumnHandling, ColumnType, ComparisonColumn, Schema, SchemaSettings,
    SchemaWarning, TypeSample,
};
pub use value::{CellValue, DateTime};
pub use write::{cell_write, CellWrite, WriteError};

/// Raw JSON carried through from a document this build does not understand.
pub type Unknown = std::collections::BTreeMap<String, serde_json::Value>;

/// Errors raised while parsing, aligning or comparing tabular data.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TableError {
    /// The decoded text is larger than the table engine's input limit.
    #[error("input is {size} bytes, past the {limit} byte offset limit")]
    TooLarge {
        /// Size of the offered input in bytes.
        size: usize,
        /// Largest input size the engine addresses.
        limit: usize,
    },
    /// The parsed table would retain more cells than the engine can bound.
    #[error("table has {count} cells, past the {limit} cell limit")]
    TooManyCells {
        /// Number of cells the parser was about to retain.
        count: usize,
        /// Largest table the parser retains.
        limit: usize,
    },
    /// The parsed table would retain more rows than the engine can bound.
    #[error("table has more than the {limit} row limit")]
    TooManyRows {
        /// Largest table row count.
        limit: usize,
    },
    /// The rectangular comparison result would exceed its memory budget.
    #[error("comparison has {count} cells, past the {limit} cell limit")]
    ComparisonTooLarge {
        /// Number of comparison cells requested.
        count: usize,
        /// Largest comparison result retained.
        limit: usize,
    },
    /// A settings value cannot describe any input.
    #[error("invalid {what}: {detail}")]
    InvalidSettings {
        /// The setting that failed validation.
        what: &'static str,
        /// What is wrong with it.
        detail: String,
    },
    /// The caller raised the cancellation flag before the work finished.
    #[error("comparison cancelled")]
    Cancelled,
}

impl TableError {
    pub(crate) fn invalid(what: &'static str, detail: impl Into<String>) -> Self {
        Self::InvalidSettings {
            what,
            detail: detail.into(),
        }
    }
}

impl From<ca_diff::DiffError> for TableError {
    fn from(error: ca_diff::DiffError) -> Self {
        match error {
            ca_diff::DiffError::Cancelled => Self::Cancelled,
            other => Self::invalid("alignment", other.to_string()),
        }
    }
}

/// Result alias for the engine's fallible operations.
pub type Result<T> = std::result::Result<T, TableError>;
