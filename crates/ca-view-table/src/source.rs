//! The grid's view of a finished comparison.
//!
//! Everything the grid asks for is answered from the tables, the schema and the
//! comparison the worker posted. Nothing is recomputed, so an answer costs one
//! or two indexed lookups.

use crate::jobs::TableData;
use crate::model::{ColumnInfo, Side, Source};
use ca_table::compare::{CellStatus, RowStatus, Totals};
use ca_table::schema::{column_letter, ColumnType, SchemaWarning};
use std::borrow::Cow;

/// A finished comparison, as the grid reads it.
#[derive(Debug)]
pub struct ComparisonSource {
    data: Box<TableData>,
    columns: Vec<ColumnInfo>,
    notices: Vec<String>,
}

impl ComparisonSource {
    /// Wrap a comparison the worker posted.
    #[must_use]
    pub fn new(data: Box<TableData>) -> Self {
        let columns = data
            .schema
            .columns
            .iter()
            .map(|column| ColumnInfo {
                name: column.name.clone(),
                left_name: column
                    .left
                    .and_then(|index| data.left.table.header()?.get(index as usize))
                    .cloned()
                    .unwrap_or_default(),
                right_name: column
                    .right
                    .and_then(|index| data.right.table.header()?.get(index as usize))
                    .cloned()
                    .unwrap_or_default(),
                left_letter: column.left.map(column_letter).unwrap_or_default(),
                right_letter: column.right.map(column_letter).unwrap_or_default(),
                left_index: column.left,
                right_index: column.right,
                key: column.handling.key,
                unimportant: column.handling.unimportant,
                type_label: type_label(&column.effective_type),
            })
            .collect();
        let notices = build_notices(&data);
        Self {
            data,
            columns,
            notices,
        }
    }

    /// The comparison behind the view.
    #[must_use]
    pub fn data(&self) -> &TableData {
        &self.data
    }

    /// The file rows one aligned row pairs.
    fn pair(&self, row: usize) -> Option<ca_table::align::RowPair> {
        self.data.comparison.rows.get(row).map(|entry| entry.pair)
    }
}

/// What a column type is called on screen.
#[must_use]
pub const fn type_label(column_type: &ColumnType) -> &'static str {
    match column_type {
        ColumnType::General => "General",
        ColumnType::Text => "Text",
        ColumnType::Number => "Number",
        ColumnType::DateTime => "Date and time",
        _ => "Unrecognized",
    }
}

/// Everything about the two files the view should say without blocking.
fn build_notices(data: &TableData) -> Vec<String> {
    let mut notices = Vec::new();
    for warning in &data.schema.warnings {
        match warning {
            SchemaWarning::FirstLineDisagrees { .. } => notices.push(
                "The two files disagree on whether line one names the columns. \
                 One side compares a header row against data."
                    .to_string(),
            ),
            SchemaWarning::UnpairedColumns { left, right } => {
                let mut sides = Vec::new();
                if !left.is_empty() {
                    sides.push(format!("left: {}", left.join(", ")));
                }
                if !right.is_empty() {
                    sides.push(format!("right: {}", right.join(", ")));
                }
                notices.push(format!(
                    "Custom alignment leaves columns unpaired ({}) and those values are not compared.",
                    sides.join("; ")
                ));
            }
            _ => notices.push("The two files disagree about their columns.".to_string()),
        }
    }
    for (index, column) in data.schema.columns.iter().enumerate() {
        let fallbacks = data.comparison.type_fallbacks(index);
        if fallbacks > 0 {
            notices.push(format!(
                "Column {}: {fallbacks} cells did not read as {} and were compared as characters.",
                column.name,
                type_label(&column.effective_type).to_lowercase()
            ));
        }
    }
    for (label, side) in [("Left", &data.left), ("Right", &data.right)] {
        if side.facts.had_errors {
            notices.push(format!(
                "{label} file: some bytes could not be decoded and were replaced."
            ));
        }
        if !side.facts.warnings.is_empty() {
            notices.push(format!(
                "{label} file: {} field problems were recovered from.",
                side.facts.warnings.len()
            ));
        }
    }
    let duplicates = data.alignment.duplicate_keys.len();
    if duplicates > 0 {
        notices.push(format!(
            "{duplicates} key values occur more than once. Duplicates pair in file order."
        ));
    }
    notices
}

impl Source for ComparisonSource {
    fn rows(&self) -> usize {
        self.data.comparison.rows.len()
    }

    fn columns(&self) -> &[ColumnInfo] {
        &self.columns
    }

    fn row_status(&self, row: usize) -> RowStatus {
        self.data
            .comparison
            .rows
            .get(row)
            .map_or(RowStatus::Same, |entry| entry.status)
    }

    fn row_numbers(&self, row: usize) -> (Option<u32>, Option<u32>) {
        match self.pair(row) {
            // File rows are counted from zero; the gutter counts from one.
            Some(pair) => (
                pair.left.map(|line| line.saturating_add(1)),
                pair.right.map(|line| line.saturating_add(1)),
            ),
            None => (None, None),
        }
    }

    fn cell_status(&self, row: usize, column: usize) -> CellStatus {
        self.data
            .comparison
            .cell(row, column)
            .unwrap_or(CellStatus::Same)
    }

    fn cell_text(&self, row: usize, column: usize, side: Side) -> Cow<'_, str> {
        let Some(pair) = self.pair(row) else {
            return Cow::Borrowed("");
        };
        let Some(mapping) = self.data.schema.columns.get(column) else {
            return Cow::Borrowed("");
        };
        let (file_row, file_column, table) = match side {
            Side::Left => (pair.left, mapping.left, &self.data.left.table),
            Side::Right => (pair.right, mapping.right, &self.data.right.table),
        };
        let (Some(file_row), Some(file_column)) = (file_row, file_column) else {
            return Cow::Borrowed("");
        };
        table.cell_text(file_row as usize, file_column as usize)
    }

    fn totals(&self) -> Totals {
        self.data.comparison.totals
    }

    fn type_fallbacks(&self, column: usize) -> u64 {
        self.data.comparison.type_fallbacks(column)
    }

    fn notices(&self) -> &[String] {
        &self.notices
    }
}
