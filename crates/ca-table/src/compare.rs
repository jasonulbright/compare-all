//! Classifying the cells of aligned row pairs.
//!
//! Every mapped comparison column of every aligned row pair gets a
//! [`CellStatus`]. The statuses of a row roll up into a [`RowStatus`], and the
//! row statuses total up for the gutter and the summary.
//!
//! Comparison is typed. A number column compares values, so `1.0` equals `1`.
//! A date column compares instants, so the same moment written two ways is
//! equal. A text column compares characters under the column's case and
//! whitespace settings.
//!
//! A difference is unimportant when the column is marked unimportant, when the
//! two values are within the column's tolerance, or when the only difference
//! is one the column is told to disregard. A cell that is empty on one side
//! and absent on the other is an unimportant difference.

use crate::align::{Alignment, RowPair};
use crate::decimal::Decimal;
use crate::parse::Table;
use crate::schema::{ColumnType, ComparisonColumn, Schema};
use crate::value::{has_leading_zero_integer, parse_date, parse_number, text_equal};
use crate::{Result, TableError, Unknown};
use ca_diff::{Cancel, NeverCancel};
use serde::{Deserialize, Serialize};

/// Largest rectangular comparison result retained in memory.
///
/// This accommodates a million aligned rows of five columns, including a row
/// that exists on only one side.
pub const MAX_COMPARISON_CELLS: usize = 6_000_000;

/// Rows checked between two cancellation polls.
const CANCEL_STRIDE: usize = 4_096;

/// What a comparison found in one cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum CellStatus {
    /// The two cells hold the same value.
    Same,
    /// The two cells differ in a way that matters.
    Different,
    /// The two cells differ in a way that does not matter.
    Unimportant,
    /// The cell exists only on the left.
    LeftOnly,
    /// The cell exists only on the right.
    RightOnly,
}

impl CellStatus {
    /// Whether the status is a difference that matters.
    #[must_use]
    pub fn is_important_difference(self) -> bool {
        matches!(self, Self::Different | Self::LeftOnly | Self::RightOnly)
    }

    /// Whether the status is any kind of difference.
    #[must_use]
    pub fn is_difference(self) -> bool {
        !matches!(self, Self::Same)
    }
}

/// What a comparison found in one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum RowStatus {
    /// Every compared cell holds the same value.
    Same,
    /// At least one cell differs in a way that matters.
    Different,
    /// Cells differ, but no difference matters.
    Unimportant,
    /// The row exists only on the left.
    LeftOnly,
    /// The row exists only on the right.
    RightOnly,
}

/// One cell's place in the comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CellComparison {
    /// Zero based comparison column.
    pub column: u32,
    /// What the comparison found.
    pub status: CellStatus,
}

/// One aligned row pair and what the comparison found in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowComparison {
    /// The rows this line holds.
    pub pair: RowPair,
    /// The row's rolled up status.
    pub status: RowStatus,
}

/// How many rows and cells fell into each class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Totals {
    /// Rows whose cells all match.
    pub same: u64,
    /// Rows with at least one difference that matters.
    pub different: u64,
    /// Rows that differ only in ways that do not matter.
    pub unimportant: u64,
    /// Rows present only on the left.
    pub left_only: u64,
    /// Rows present only on the right.
    pub right_only: u64,
    /// Cells that match.
    pub cells_same: u64,
    /// Cells that hold a value on both sides and differ in a way that matters.
    pub cells_different: u64,
    /// Cells that differ in a way that does not matter.
    pub cells_unimportant: u64,
    /// Cells that exist only on the left.
    pub cells_left_only: u64,
    /// Cells that exist only on the right.
    pub cells_right_only: u64,
}

impl Totals {
    /// Rows with any difference at all, important or not.
    #[must_use]
    pub fn rows_with_differences(&self) -> u64 {
        self.different
            .saturating_add(self.unimportant)
            .saturating_add(self.left_only)
            .saturating_add(self.right_only)
    }

    /// Every cell the comparison classified. The five cell buckets are
    /// disjoint and cover every cell, so this equals the number of comparison
    /// lines times the number of comparison columns.
    #[must_use]
    pub fn cells_total(&self) -> u64 {
        self.cells_same
            .saturating_add(self.cells_different)
            .saturating_add(self.cells_unimportant)
            .saturating_add(self.cells_left_only)
            .saturating_add(self.cells_right_only)
    }

    /// Cells that differ in any way, important or not.
    #[must_use]
    pub fn cells_with_differences(&self) -> u64 {
        self.cells_different
            .saturating_add(self.cells_unimportant)
            .saturating_add(self.cells_left_only)
            .saturating_add(self.cells_right_only)
    }
}

/// Controls over classification.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CompareOptions {
    /// Treat unimportant cell differences as matching, so a row that differs
    /// only in unimportant ways reads as the same.
    pub ignore_unimportant_differences: bool,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, skip_serializing_if = "Unknown::is_empty")]
    pub unknown: Unknown,
}

/// The classified comparison of two tables.
///
/// Cell statuses are held in one block rather than one vector per row, so a
/// comparison of a large file costs one allocation for the cells.
#[derive(Debug, Clone, PartialEq)]
pub struct TableComparison {
    /// One entry per comparison line, in display order.
    pub rows: Vec<RowComparison>,
    /// Row and cell counts by class.
    pub totals: Totals,
    cells: Vec<CellStatus>,
    column_count: usize,
    type_fallbacks: Vec<u64>,
}

impl TableComparison {
    /// Number of comparison columns each row carries.
    #[must_use]
    pub fn column_count(&self) -> usize {
        self.column_count
    }

    /// How many cells of a comparison column did not read as the column's
    /// type and were compared as characters instead.
    #[must_use]
    pub fn type_fallbacks(&self, column: usize) -> u64 {
        self.type_fallbacks.get(column).copied().unwrap_or(0)
    }

    /// Statuses of one row's cells, empty when the row does not exist.
    #[must_use]
    pub fn cells(&self, row: usize) -> &[CellStatus] {
        let Some(start) = row.checked_mul(self.column_count) else {
            return &[];
        };
        let Some(end) = start.checked_add(self.column_count) else {
            return &[];
        };
        self.cells.get(start..end).unwrap_or_default()
    }

    /// One cell's status.
    #[must_use]
    pub fn cell(&self, row: usize, column: usize) -> Option<CellStatus> {
        self.cells(row).get(column).copied()
    }

    /// One row's cells with their column numbers.
    pub fn row_cells(&self, row: usize) -> impl Iterator<Item = CellComparison> + '_ {
        self.cells(row)
            .iter()
            .enumerate()
            .map(|(column, status)| CellComparison {
                column: u32::try_from(column).unwrap_or(u32::MAX),
                status: *status,
            })
    }

    /// Comparison columns that hold no difference at all, for hiding columns
    /// whose data matches everywhere.
    #[must_use]
    pub fn columns_without_differences(&self) -> Vec<u32> {
        (0..self.column_count)
            .filter(|column| {
                (0..self.rows.len()).all(|row| self.cell(row, *column) == Some(CellStatus::Same))
            })
            .map(|column| u32::try_from(column).unwrap_or(u32::MAX))
            .collect()
    }
}

/// Compare two tables over an alignment.
///
/// # Errors
///
/// Returns [`TableError::InvalidSettings`] when the alignment names a row
/// neither table holds.
pub fn compare(
    left: &Table,
    right: &Table,
    schema: &Schema,
    alignment: &Alignment,
) -> Result<TableComparison> {
    compare_with(
        left,
        right,
        schema,
        alignment,
        &CompareOptions::default(),
        &NeverCancel,
    )
}

/// Compare two tables over an alignment, with options and cancellation.
///
/// # Errors
///
/// Returns [`TableError::Cancelled`] when the flag is raised before the work
/// finishes, and [`TableError::InvalidSettings`] when the alignment names a
/// row neither table holds.
pub fn compare_with(
    left: &Table,
    right: &Table,
    schema: &Schema,
    alignment: &Alignment,
    options: &CompareOptions,
    cancel: &dyn Cancel,
) -> Result<TableComparison> {
    let column_count = schema.columns.len();
    let cell_count = alignment.pairs.len().saturating_mul(column_count);
    if cell_count > MAX_COMPARISON_CELLS {
        return Err(TableError::ComparisonTooLarge {
            count: cell_count,
            limit: MAX_COMPARISON_CELLS,
        });
    }
    let mut rows = Vec::with_capacity(alignment.pairs.len());
    let mut cells = Vec::with_capacity(cell_count);
    let mut totals = Totals::default();
    let mut type_fallbacks = vec![0u64; column_count];
    // An exact tolerance costs a conversion, so each column pays for it once
    // rather than once per cell.
    let tolerances: Vec<Option<Decimal>> = schema
        .columns
        .iter()
        .map(|column| exact_tolerance(column.handling.numeric_tolerance))
        .collect();

    for (index, pair) in alignment.pairs.iter().enumerate() {
        if index % CANCEL_STRIDE == 0 && cancel.is_cancelled() {
            return Err(TableError::Cancelled);
        }
        if pair.left.is_none() && pair.right.is_none() {
            return Err(TableError::invalid(
                "alignment",
                "a comparison line holds no row on either side",
            ));
        }
        check_row(pair.left, left.row_count(), "left")?;
        check_row(pair.right, right.row_count(), "right")?;
        let status = match (pair.left, pair.right) {
            (Some(left_row), Some(right_row)) => {
                let start = cells.len();
                for (position, column) in schema.columns.iter().enumerate() {
                    let outcome = classify_cell(
                        left,
                        right,
                        schema,
                        column,
                        tolerances.get(position).and_then(Option::as_ref),
                        left_row as usize,
                        right_row as usize,
                    );
                    if outcome.type_fallback {
                        if let Some(slot) = type_fallbacks.get_mut(position) {
                            *slot = slot.saturating_add(1);
                        }
                    }
                    cells.push(outcome.status);
                }
                let row_cells = cells.get(start..).unwrap_or_default();
                for status in row_cells {
                    match status {
                        CellStatus::Same => totals.cells_same += 1,
                        CellStatus::Unimportant => totals.cells_unimportant += 1,
                        CellStatus::Different => totals.cells_different += 1,
                        CellStatus::LeftOnly => totals.cells_left_only += 1,
                        CellStatus::RightOnly => totals.cells_right_only += 1,
                    }
                }
                roll_up(row_cells, options.ignore_unimportant_differences)
            }
            (Some(_), None) => {
                cells.extend(std::iter::repeat_n(CellStatus::LeftOnly, column_count));
                totals.cells_left_only += column_count as u64;
                RowStatus::LeftOnly
            }
            (None, Some(_)) => {
                cells.extend(std::iter::repeat_n(CellStatus::RightOnly, column_count));
                totals.cells_right_only += column_count as u64;
                RowStatus::RightOnly
            }
            (None, None) => RowStatus::Same,
        };
        match status {
            RowStatus::Same => totals.same += 1,
            RowStatus::Different => totals.different += 1,
            RowStatus::Unimportant => totals.unimportant += 1,
            RowStatus::LeftOnly => totals.left_only += 1,
            RowStatus::RightOnly => totals.right_only += 1,
        }
        rows.push(RowComparison {
            pair: *pair,
            status,
        });
    }
    Ok(TableComparison {
        rows,
        totals,
        cells,
        column_count,
        type_fallbacks,
    })
}

/// Rejects an alignment that names a row the file does not hold.
fn check_row(row: Option<u32>, count: usize, side: &str) -> Result<()> {
    match row {
        Some(row) if row as usize >= count => Err(TableError::invalid(
            "alignment",
            format!("a comparison line names {side} row {row}, past the end of the file"),
        )),
        _ => Ok(()),
    }
}

/// The configured tolerance as an exact value, or `None` when it is zero.
///
/// A zero tolerance means exact equality: all slack a comparison allows comes
/// from a tolerance the caller set.
fn exact_tolerance(tolerance: f64) -> Option<Decimal> {
    if tolerance == 0.0 || tolerance.is_nan() {
        return None;
    }
    Decimal::from_f64(tolerance.abs())
}

fn roll_up(cells: &[CellStatus], ignore_unimportant: bool) -> RowStatus {
    if cells.is_empty() {
        return RowStatus::Different;
    }
    let mut unimportant = false;
    for status in cells {
        if status.is_important_difference() {
            return RowStatus::Different;
        }
        if matches!(status, CellStatus::Unimportant) {
            unimportant = true;
        }
    }
    if unimportant && !ignore_unimportant {
        RowStatus::Unimportant
    } else {
        RowStatus::Same
    }
}

/// What one cell's comparison found, and whether the cell had to fall back to
/// comparing characters because it does not read as its column's type.
struct CellOutcome {
    status: CellStatus,
    type_fallback: bool,
}

/// Classifies one cell and applies the column's importance at the single exit,
/// so an unimportant column never forces an important row difference.
#[allow(clippy::too_many_arguments)]
fn classify_cell(
    left: &Table,
    right: &Table,
    schema: &Schema,
    column: &ComparisonColumn,
    tolerance: Option<&Decimal>,
    left_row: usize,
    right_row: usize,
) -> CellOutcome {
    let mut outcome = raw_cell(left, right, schema, column, tolerance, left_row, right_row);
    if column.handling.unimportant && outcome.status != CellStatus::Same {
        outcome.status = CellStatus::Unimportant;
    }
    outcome
}

#[allow(clippy::too_many_arguments)]
fn raw_cell(
    left: &Table,
    right: &Table,
    schema: &Schema,
    column: &ComparisonColumn,
    tolerance: Option<&Decimal>,
    left_row: usize,
    right_row: usize,
) -> CellOutcome {
    let plain = |status| CellOutcome {
        status,
        type_fallback: false,
    };
    let (Some(left_column), Some(right_column)) = (column.left, column.right) else {
        return plain(match (column.left, column.right) {
            (Some(_), None) => CellStatus::LeftOnly,
            (None, Some(_)) => CellStatus::RightOnly,
            _ => CellStatus::Same,
        });
    };
    let left_cell = left.cell(left_row, left_column as usize);
    let right_cell = right.cell(right_row, right_column as usize);
    let (left_text, right_text) = match (&left_cell, &right_cell) {
        (None, None) => return plain(CellStatus::Same),
        // A cell that is empty on one side and absent on the other carries no
        // data either way, so the difference does not matter.
        (None, Some(cell)) if cell.text().is_empty() => return plain(CellStatus::Unimportant),
        (Some(cell), None) if cell.text().is_empty() => return plain(CellStatus::Unimportant),
        (None, Some(_)) => return plain(CellStatus::RightOnly),
        (Some(_), None) => return plain(CellStatus::LeftOnly),
        (Some(a), Some(b)) => (a.text(), b.text()),
    };
    let handling = &column.handling;
    let typed = match column.effective_type {
        ColumnType::Number => {
            compare_numbers(left_text.trim(), right_text.trim(), schema, tolerance)
        }
        ColumnType::DateTime => compare_dates(
            left_text.trim(),
            right_text.trim(),
            schema,
            handling.date_tolerance_seconds,
        ),
        _ => None,
    };
    let type_fallback = typed.is_none()
        && matches!(
            column.effective_type,
            ColumnType::Number | ColumnType::DateTime
        );
    let status = typed.unwrap_or_else(|| {
        if left_text == right_text {
            CellStatus::Same
        } else if text_equal(
            &left_text,
            &right_text,
            handling.ignore_case,
            handling.ignore_whitespace,
        ) {
            CellStatus::Unimportant
        } else {
            CellStatus::Different
        }
    });
    CellOutcome {
        status,
        type_fallback,
    }
}

/// Compares two cells as numbers, or returns `None` when either side is not a
/// number so the caller falls back to comparing characters.
///
/// Equality is exact: two values match only when the decimals they are written
/// as are the same value. A configured tolerance is the only source of slack,
/// and the distance it is measured against is an exact decimal difference.
fn compare_numbers(
    left: &str,
    right: &str,
    schema: &Schema,
    tolerance: Option<&Decimal>,
) -> Option<CellStatus> {
    let a = parse_number(left, &schema.left_regional)?;
    let b = parse_number(right, &schema.right_regional)?;
    if a == b
        && (left.trim() == right.trim()
            || !has_leading_zero_integer(left) && !has_leading_zero_integer(right))
    {
        return Some(CellStatus::Same);
    }
    if a == b {
        return Some(CellStatus::Different);
    }
    let Some(tolerance) = tolerance else {
        return Some(CellStatus::Different);
    };
    match a.abs_difference(&b) {
        Some(distance) if distance <= *tolerance => Some(CellStatus::Unimportant),
        // A distance too wide to write out is past any tolerance a double
        // holds.
        _ => Some(CellStatus::Different),
    }
}

/// Compares two cells as instants, or returns `None` when either side is not a
/// date so the caller falls back to comparing characters.
fn compare_dates(left: &str, right: &str, schema: &Schema, tolerance: f64) -> Option<CellStatus> {
    let a = parse_date(left, &schema.left_regional)?;
    let b = parse_date(right, &schema.right_regional)?;
    let distance = a.distance(b);
    Some(within(distance, tolerance, a == b))
}

fn within(difference: f64, tolerance: f64, equal: bool) -> CellStatus {
    if equal {
        CellStatus::Same
    } else if difference.abs() <= tolerance.abs() {
        CellStatus::Unimportant
    } else {
        CellStatus::Different
    }
}

/// The rows a display filter keeps.
///
/// `show_same` keeps rows whose cells all match, `show_differences` keeps rows
/// with any difference. Unimportant differences count as differences unless
/// `ignore_unimportant` is set, which moves those rows into the same class.
#[must_use]
pub fn filter_rows(
    comparison: &TableComparison,
    show_same: bool,
    show_differences: bool,
    ignore_unimportant: bool,
) -> Vec<u32> {
    comparison
        .rows
        .iter()
        .enumerate()
        .filter(|(_, row)| {
            let differs = match row.status {
                RowStatus::Same => false,
                RowStatus::Unimportant => !ignore_unimportant,
                RowStatus::Different | RowStatus::LeftOnly | RowStatus::RightOnly => true,
            };
            if differs {
                show_differences
            } else {
                show_same
            }
        })
        .map(|(index, _)| u32::try_from(index).unwrap_or(u32::MAX))
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::align::{align, RowAlignOptions};
    use crate::parse::{parse, FirstLineContains, ParseOptions};
    use crate::regional::Regional;
    use crate::schema::{ColumnHandling, SchemaSettings};
    use std::collections::BTreeMap;

    fn headed(text: &str) -> Table {
        let mut options = ParseOptions::comma_separated();
        options.first_line_contains = FirstLineContains::ColumnNames;
        parse(text, &options).unwrap()
    }

    fn one_column(left_text: &str, right_text: &str, handling: ColumnHandling) -> CellStatus {
        let left = headed(&format!("v\n{left_text}\n"));
        let right = headed(&format!("v\n{right_text}\n"));
        let mut map = BTreeMap::new();
        map.insert(0, handling);
        let settings = SchemaSettings {
            handling: map,
            left_regional: Regional::dot_decimal(),
            right_regional: Regional::dot_decimal(),
            ..SchemaSettings::default()
        };
        let schema = Schema::build(&left, &right, &settings);
        let alignment = crate::align::Alignment {
            pairs: vec![RowPair::both(0, 0)],
            keyed: false,
            duplicate_keys: Vec::new(),
            duplicate_keys_dropped: 0,
        };
        let result = compare(&left, &right, &schema, &alignment).unwrap();
        result.cell(0, 0).unwrap()
    }

    fn typed(column_type: ColumnType) -> ColumnHandling {
        ColumnHandling {
            use_default: false,
            column_type,
            ..ColumnHandling::default()
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn typed_comparison_cases() {
        let cases: &[(&str, &str, ColumnHandling, CellStatus)] = &[
            ("1.0", "1", typed(ColumnType::Number), CellStatus::Same),
            ("1.0", "1", typed(ColumnType::Text), CellStatus::Different),
            ("2.50", "2.5", typed(ColumnType::Number), CellStatus::Same),
            (
                "\"1,000\"",
                "1000",
                typed(ColumnType::Number),
                CellStatus::Same,
            ),
            ("1", "2", typed(ColumnType::Number), CellStatus::Different),
            ("abc", "abc", typed(ColumnType::Text), CellStatus::Same),
            ("abc", "ABC", typed(ColumnType::Text), CellStatus::Different),
            (
                "abc",
                "ABC",
                ColumnHandling {
                    ignore_case: true,
                    ..typed(ColumnType::Text)
                },
                CellStatus::Unimportant,
            ),
            (
                "\"a  b\"",
                "\"a b\"",
                ColumnHandling {
                    ignore_whitespace: true,
                    ..typed(ColumnType::Text)
                },
                CellStatus::Unimportant,
            ),
            (
                "\"a  b\"",
                "\"a b\"",
                typed(ColumnType::Text),
                CellStatus::Different,
            ),
            (
                "1",
                "2",
                ColumnHandling {
                    numeric_tolerance: 1.0,
                    ..typed(ColumnType::Number)
                },
                CellStatus::Unimportant,
            ),
            (
                "1",
                "3",
                ColumnHandling {
                    numeric_tolerance: 1.0,
                    ..typed(ColumnType::Number)
                },
                CellStatus::Different,
            ),
            (
                "01/02/2020",
                "2020-01-02",
                typed(ColumnType::DateTime),
                CellStatus::Same,
            ),
            (
                "01/02/2020 00:00",
                "01/02/2020 00:30",
                ColumnHandling {
                    date_tolerance_seconds: 3_600.0,
                    ..typed(ColumnType::DateTime)
                },
                CellStatus::Unimportant,
            ),
            (
                "01/02/2020 00:00",
                "01/02/2020 02:00",
                ColumnHandling {
                    date_tolerance_seconds: 3_600.0,
                    ..typed(ColumnType::DateTime)
                },
                CellStatus::Different,
            ),
            (
                "x",
                "y",
                ColumnHandling {
                    unimportant: true,
                    ..typed(ColumnType::Text)
                },
                CellStatus::Unimportant,
            ),
            (
                "x",
                "x",
                ColumnHandling {
                    unimportant: true,
                    ..typed(ColumnType::Text)
                },
                CellStatus::Same,
            ),
            // A cell a number column cannot read falls back to characters.
            ("n/a", "n/a", typed(ColumnType::Number), CellStatus::Same),
            ("n/a", "1", typed(ColumnType::Number), CellStatus::Different),
        ];
        for (left, right, handling, expected) in cases {
            let status = one_column(left, right, handling.clone());
            assert_eq!(status, *expected, "{left:?} against {right:?}");
        }
    }

    #[test]
    fn sides_may_use_different_number_conventions() {
        let left = headed("v\n\"1.234,50\"\n");
        let right = headed("v\n1234.5\n");
        let mut map = BTreeMap::new();
        map.insert(0, typed(ColumnType::Number));
        let settings = SchemaSettings {
            handling: map,
            left_regional: Regional::comma_decimal(),
            right_regional: Regional::dot_decimal(),
            ..SchemaSettings::default()
        };
        let schema = Schema::build(&left, &right, &settings);
        let alignment = crate::align::Alignment {
            pairs: vec![RowPair::both(0, 0)],
            keyed: false,
            duplicate_keys: Vec::new(),
            duplicate_keys_dropped: 0,
        };
        let result = compare(&left, &right, &schema, &alignment).unwrap();
        assert_eq!(result.cell(0, 0), Some(CellStatus::Same));
    }

    #[test]
    fn an_empty_cell_against_an_absent_one_is_unimportant() {
        let left = headed("a,b\nx,\np,q\n");
        let right = headed("a,b\nx\np,q\n");
        let schema = Schema::build(&left, &right, &SchemaSettings::default());
        assert!(schema.columns[1].is_mapped());
        let alignment = crate::align::Alignment {
            pairs: vec![RowPair::both(0, 0)],
            keyed: false,
            duplicate_keys: Vec::new(),
            duplicate_keys_dropped: 0,
        };
        let result = compare(&left, &right, &schema, &alignment).unwrap();
        assert_eq!(result.cell(0, 1), Some(CellStatus::Unimportant));
        assert_eq!(result.rows[0].status, RowStatus::Unimportant);
    }

    #[test]
    fn a_missing_cell_holding_data_is_an_important_difference() {
        let left = headed("a,b\nx,y\np,q\n");
        let right = headed("a,b\nx\np,q\n");
        let schema = Schema::build(&left, &right, &SchemaSettings::default());
        let alignment = crate::align::Alignment {
            pairs: vec![RowPair::both(0, 0)],
            keyed: false,
            duplicate_keys: Vec::new(),
            duplicate_keys_dropped: 0,
        };
        let result = compare(&left, &right, &schema, &alignment).unwrap();
        assert_eq!(result.cell(0, 1), Some(CellStatus::LeftOnly));
        assert_eq!(result.rows[0].status, RowStatus::Different);
    }

    #[test]
    fn row_status_rolls_up_from_the_cells() {
        let left = headed("a,b,c\n1,x,p\n2,y,q\n3,z,r\n");
        let right = headed("a,b,c\n1,x,p\n2,Y,q\n3,z,R2\n");
        let settings = SchemaSettings {
            default_handling: ColumnHandling {
                ignore_case: true,
                ..ColumnHandling::default()
            },
            ..SchemaSettings::default()
        };
        let schema = Schema::build(&left, &right, &settings);
        let alignment = align(&left, &right, &schema, &RowAlignOptions::default()).unwrap();
        let result = compare(&left, &right, &schema, &alignment).unwrap();
        let by_row: Vec<RowStatus> = result.rows.iter().map(|row| row.status).collect();
        assert_eq!(
            by_row,
            vec![
                RowStatus::Same,
                RowStatus::Unimportant,
                RowStatus::Different
            ]
        );
        assert_eq!(result.totals.same, 1);
        assert_eq!(result.totals.unimportant, 1);
        assert_eq!(result.totals.different, 1);
    }

    #[test]
    fn ignoring_unimportant_differences_moves_rows_into_the_same_class() {
        let left = headed("a\nx\n");
        let right = headed("a\nX\n");
        let settings = SchemaSettings {
            default_handling: ColumnHandling {
                ignore_case: true,
                ..ColumnHandling::default()
            },
            ..SchemaSettings::default()
        };
        let schema = Schema::build(&left, &right, &settings);
        let alignment = crate::align::Alignment {
            pairs: vec![RowPair::both(0, 0)],
            keyed: false,
            duplicate_keys: Vec::new(),
            duplicate_keys_dropped: 0,
        };
        let options = CompareOptions {
            ignore_unimportant_differences: true,
            unknown: Unknown::new(),
        };
        let result =
            compare_with(&left, &right, &schema, &alignment, &options, &NeverCancel).unwrap();
        assert_eq!(result.rows[0].status, RowStatus::Same);
        assert_eq!(result.cell(0, 0), Some(CellStatus::Unimportant));
    }

    #[test]
    fn orphan_rows_mark_every_cell() {
        let left = headed("a,b\n1,x\n2,y\n");
        let right = headed("a,b\n1,x\n");
        let schema = Schema::build(&left, &right, &SchemaSettings::default());
        let alignment = align(&left, &right, &schema, &RowAlignOptions::default()).unwrap();
        let result = compare(&left, &right, &schema, &alignment).unwrap();
        assert_eq!(result.totals.left_only, 1);
        let orphan = result
            .rows
            .iter()
            .position(|row| row.status == RowStatus::LeftOnly)
            .unwrap();
        assert!(result
            .cells(orphan)
            .iter()
            .all(|status| *status == CellStatus::LeftOnly));
    }

    #[test]
    fn an_unmapped_column_is_one_sided_everywhere() {
        let left = headed("id,extra\n1,x\n");
        let right = headed("id\n1\n");
        let schema = Schema::build(&left, &right, &SchemaSettings::default());
        let alignment = crate::align::Alignment {
            pairs: vec![RowPair::both(0, 0)],
            keyed: false,
            duplicate_keys: Vec::new(),
            duplicate_keys_dropped: 0,
        };
        let result = compare(&left, &right, &schema, &alignment).unwrap();
        assert_eq!(result.cell(0, 1), Some(CellStatus::LeftOnly));
    }

    #[test]
    fn columns_without_differences_are_listed() {
        let left = headed("a,b\n1,x\n2,y\n");
        let right = headed("a,b\n1,X\n2,Y\n");
        let schema = Schema::build(&left, &right, &SchemaSettings::default());
        let alignment = align(&left, &right, &schema, &RowAlignOptions::default()).unwrap();
        let result = compare(&left, &right, &schema, &alignment).unwrap();
        assert_eq!(result.columns_without_differences(), vec![0]);
    }

    #[test]
    fn the_row_filter_selects_by_status() {
        let left = headed("a\n1\n2\n");
        let right = headed("a\n1\n9\n");
        let schema = Schema::build(&left, &right, &SchemaSettings::default());
        let alignment = crate::align::Alignment {
            pairs: vec![RowPair::both(0, 0), RowPair::both(1, 1)],
            keyed: false,
            duplicate_keys: Vec::new(),
            duplicate_keys_dropped: 0,
        };
        let result = compare(&left, &right, &schema, &alignment).unwrap();
        assert_eq!(filter_rows(&result, false, true, false), vec![1]);
        assert_eq!(filter_rows(&result, true, false, false), vec![0]);
        assert_eq!(filter_rows(&result, true, true, false), vec![0, 1]);
        assert!(filter_rows(&result, false, false, false).is_empty());
    }

    #[test]
    fn an_empty_comparison_line_is_rejected() {
        let left = headed("a\n1\n");
        let right = headed("a\n1\n");
        let schema = Schema::build(&left, &right, &SchemaSettings::default());
        let alignment = crate::align::Alignment {
            pairs: vec![RowPair {
                left: None,
                right: None,
            }],
            keyed: false,
            duplicate_keys: Vec::new(),
            duplicate_keys_dropped: 0,
        };
        assert!(matches!(
            compare(&left, &right, &schema, &alignment),
            Err(TableError::InvalidSettings { .. })
        ));
    }

    fn one_pair() -> crate::align::Alignment {
        crate::align::Alignment {
            pairs: vec![RowPair::both(0, 0)],
            keyed: false,
            duplicate_keys: Vec::new(),
            duplicate_keys_dropped: 0,
        }
    }

    #[test]
    fn zero_tolerance_means_exact_equality() {
        let number = typed(ColumnType::Number);
        assert_eq!(
            one_column("1e-17", "2e-17", number.clone()),
            CellStatus::Different
        );
        assert_eq!(
            one_column("1e-300", "0", number.clone()),
            CellStatus::Different
        );
        assert_eq!(
            one_column("0.000000000000000001", "0", number.clone()),
            CellStatus::Different
        );
        assert_eq!(one_column("0.10", "0.1", number), CellStatus::Same);
    }

    #[test]
    fn long_identifiers_stay_different() {
        let number = typed(ColumnType::Number);
        assert_eq!(
            one_column("12345678901234567890", "12345678901234567891", number),
            CellStatus::Different
        );
    }

    #[test]
    fn leading_zero_identifier_stays_different_when_forced_to_number() {
        assert_eq!(
            one_column("02134", "2134", typed(ColumnType::Number)),
            CellStatus::Different
        );
    }

    #[test]
    fn an_empty_custom_pair_list_never_calls_rows_equal() {
        let left = headed("id,name\n1,Ann\n");
        let right = headed("id,name\n1,Ann\n");
        let settings = SchemaSettings {
            alignment: crate::schema::ColumnAlignment::Custom,
            ..SchemaSettings::default()
        };
        let schema = Schema::build(&left, &right, &settings);
        let result = compare(&left, &right, &schema, &one_pair()).unwrap();
        assert_eq!(
            result.rows.first().map(|row| row.status),
            Some(RowStatus::Different)
        );
    }

    #[test]
    fn a_tolerance_measures_an_exact_distance() {
        let with = |tolerance: f64| ColumnHandling {
            numeric_tolerance: tolerance,
            ..typed(ColumnType::Number)
        };
        assert_eq!(
            one_column("100000000000000000000", "100000000000000000002", with(1.0)),
            CellStatus::Different
        );
        assert_eq!(
            one_column("100000000000000000000", "100000000000000000001", with(1.0)),
            CellStatus::Unimportant
        );
        assert_eq!(
            one_column("0.0000000000000000001", "0", with(0.5)),
            CellStatus::Unimportant
        );
        // A negative tolerance names the same width as its magnitude.
        assert_eq!(one_column("1", "2", with(-1.0)), CellStatus::Unimportant);
    }

    #[test]
    fn an_unimportant_column_never_forces_an_important_row() {
        let unimportant = ColumnHandling {
            use_default: false,
            unimportant: true,
            ..ColumnHandling::default()
        };

        // An unmapped column.
        let left = headed("id,extra\n1,x\n");
        let right = headed("id\n1\n");
        let mut map = BTreeMap::new();
        map.insert(1, unimportant.clone());
        let settings = SchemaSettings {
            handling: map,
            ..SchemaSettings::default()
        };
        let schema = Schema::build(&left, &right, &settings);
        let result = compare(&left, &right, &schema, &one_pair()).unwrap();
        assert_eq!(result.cell(0, 1), Some(CellStatus::Unimportant));
        assert_eq!(result.rows[0].status, RowStatus::Unimportant);

        // A row that is short on one side.
        let left = headed("a,b\nx,y\np,q\n");
        let right = headed("a,b\nx\np,q\n");
        let mut map = BTreeMap::new();
        map.insert(1, unimportant);
        let settings = SchemaSettings {
            handling: map,
            ..SchemaSettings::default()
        };
        let schema = Schema::build(&left, &right, &settings);
        let result = compare(&left, &right, &schema, &one_pair()).unwrap();
        assert_eq!(result.cell(0, 1), Some(CellStatus::Unimportant));
        assert_eq!(result.rows[0].status, RowStatus::Unimportant);
    }

    #[test]
    fn an_alignment_past_the_end_of_a_file_is_rejected() {
        let left = headed("a\n1\n");
        let right = headed("a\n1\n");
        let schema = Schema::build(&left, &right, &SchemaSettings::default());
        for pair in [
            RowPair::both(0, 5),
            RowPair::both(5, 0),
            RowPair::left_only(9),
            RowPair::right_only(9),
        ] {
            let alignment = crate::align::Alignment {
                pairs: vec![pair],
                keyed: false,
                duplicate_keys: Vec::new(),
                duplicate_keys_dropped: 0,
            };
            assert!(
                matches!(
                    compare(&left, &right, &schema, &alignment),
                    Err(TableError::InvalidSettings { .. })
                ),
                "{pair:?}"
            );
        }
    }

    #[test]
    fn a_rectangular_comparison_over_the_cell_budget_is_refused_before_allocation() {
        const ROWS: usize = 2_450;
        const COLUMNS: usize = 2_450;

        let mut options = ParseOptions::comma_separated();
        options.first_line_contains = FirstLineContains::CellData;
        let left_text = "x\n".repeat(ROWS);
        let right_text = std::iter::repeat_n("x", COLUMNS)
            .collect::<Vec<_>>()
            .join(",");
        let left = parse(&left_text, &options).unwrap();
        let right = parse(&right_text, &options).unwrap();
        let schema = Schema::build(&left, &right, &SchemaSettings::default());
        let alignment = crate::align::Alignment {
            pairs: (0..ROWS)
                .map(|row| {
                    if row == 0 {
                        RowPair::both(0, 0)
                    } else {
                        RowPair::left_only(u32::try_from(row).unwrap())
                    }
                })
                .collect(),
            keyed: false,
            duplicate_keys: Vec::new(),
            duplicate_keys_dropped: 0,
        };

        assert!(compare(&left, &right, &schema, &alignment).is_err());
    }

    #[test]
    fn the_cell_buckets_cover_every_cell() {
        let left = headed("a,b,c\n1,x,p\n2,y,q\n3,z,r\n");
        let right = headed("a,b,c\n1,x,p\n2,Y,q\n9,w,s\n");
        let settings = SchemaSettings {
            default_handling: ColumnHandling {
                ignore_case: true,
                ..ColumnHandling::default()
            },
            ..SchemaSettings::default()
        };
        let schema = Schema::build(&left, &right, &settings);
        let alignment = crate::align::Alignment {
            pairs: vec![
                RowPair::both(0, 0),
                RowPair::left_only(1),
                RowPair::right_only(1),
                RowPair::both(2, 2),
            ],
            keyed: false,
            duplicate_keys: Vec::new(),
            duplicate_keys_dropped: 0,
        };
        let result = compare(&left, &right, &schema, &alignment).unwrap();
        let emitted = u64::try_from(result.rows.len() * result.column_count()).unwrap();
        assert_eq!(result.totals.cells_total(), emitted);
        assert!(result.totals.cells_left_only > 0);
        assert!(result.totals.cells_right_only > 0);
        let counted: u64 = (0..result.rows.len())
            .flat_map(|row| result.cells(row).iter())
            .filter(|status| status.is_difference())
            .count()
            .try_into()
            .unwrap();
        assert_eq!(result.totals.cells_with_differences(), counted);
    }

    #[test]
    fn a_cell_that_does_not_read_as_its_type_is_counted() {
        let left = headed("v\n1\nn/a\n2\n");
        let right = headed("v\n1\nn/a\n3\n");
        let mut map = BTreeMap::new();
        map.insert(0, typed(ColumnType::Number));
        let settings = SchemaSettings {
            handling: map,
            ..SchemaSettings::default()
        };
        let schema = Schema::build(&left, &right, &settings);
        let alignment = crate::align::Alignment {
            pairs: vec![
                RowPair::both(0, 0),
                RowPair::both(1, 1),
                RowPair::both(2, 2),
            ],
            keyed: false,
            duplicate_keys: Vec::new(),
            duplicate_keys_dropped: 0,
        };
        let result = compare(&left, &right, &schema, &alignment).unwrap();
        assert_eq!(result.type_fallbacks(0), 1);
        assert_eq!(result.cell(1, 0), Some(CellStatus::Same));
        assert_eq!(result.cell(2, 0), Some(CellStatus::Different));
    }

    #[test]
    fn options_round_trip_with_unknown_fields() {
        let text = r#"{"ignoreUnimportantDifferences":true,"futureFlag":3}"#;
        let options: CompareOptions = serde_json::from_str(text).unwrap();
        assert!(options.ignore_unimportant_differences);
        let back = serde_json::to_string(&options).unwrap();
        let again: CompareOptions = serde_json::from_str(&back).unwrap();
        assert_eq!(again, options);
        assert!(back.contains("futureFlag"));
    }
}
