//! A comparison built in memory, for the tests.
//!
//! The grid reads its data through [`Source`], so a test can state row and cell
//! statuses directly instead of writing files and running the pipeline. A very
//! large model is then cheap enough to build that the cost of one frame can be
//! measured on its own.

use crate::model::{ColumnInfo, Side, Source};
use ca_table::compare::{CellStatus, RowStatus, Totals};
use std::borrow::Cow;
use std::collections::HashMap;

/// A comparison stated row by row.
#[derive(Debug, Clone)]
pub struct FakeSource {
    rows: Vec<RowStatus>,
    columns: Vec<ColumnInfo>,
    cells: HashMap<(usize, usize), CellStatus>,
    texts: HashMap<(usize, usize, Side), String>,
    notices: Vec<String>,
}

impl FakeSource {
    /// A comparison of `rows.len()` rows by `columns` columns.
    ///
    /// Every cell takes the status its row implies, which a test overrides one
    /// cell at a time with [`FakeSource::with_cell`].
    #[must_use]
    pub fn new(rows: &[RowStatus], columns: usize) -> Self {
        Self {
            rows: rows.to_vec(),
            columns: (0..columns)
                .map(|index| ColumnInfo {
                    name: format!("Column {}", index + 1),
                    left_name: String::new(),
                    right_name: String::new(),
                    left_letter: ca_table::schema::column_letter(index_as_u32(index)),
                    right_letter: ca_table::schema::column_letter(index_as_u32(index)),
                    left_index: Some(index_as_u32(index)),
                    right_index: Some(index_as_u32(index)),
                    key: false,
                    unimportant: false,
                    type_label: "General",
                })
                .collect(),
            cells: HashMap::new(),
            texts: HashMap::new(),
            notices: Vec::new(),
        }
    }

    /// A comparison whose row statuses repeat a pattern.
    #[must_use]
    pub fn repeating(rows: usize, columns: usize, pattern: &[RowStatus]) -> Self {
        let statuses: Vec<RowStatus> = (0..rows).map(|row| pattern[row % pattern.len()]).collect();
        Self::new(&statuses, columns)
    }

    /// Give one cell a status of its own.
    #[must_use]
    pub fn with_cell(mut self, row: usize, column: usize, status: CellStatus) -> Self {
        self.cells.insert((row, column), status);
        self
    }

    /// Mark one column as a key.
    #[must_use]
    pub fn with_key(mut self, column: usize) -> Self {
        if let Some(info) = self.columns.get_mut(column) {
            info.key = true;
        }
        self
    }

    /// Give one column a different heading.
    #[must_use]
    pub fn with_column_name(mut self, column: usize, name: &str) -> Self {
        if let Some(info) = self.columns.get_mut(column) {
            info.name = name.to_string();
        }
        self
    }

    /// Give one cell side explicit text.
    #[must_use]
    pub fn with_text(mut self, row: usize, column: usize, side: Side, text: &str) -> Self {
        self.texts.insert((row, column, side), text.to_string());
        self
    }

    /// Add something the view should tell the user.
    #[must_use]
    pub fn with_notice(mut self, notice: &str) -> Self {
        self.notices.push(notice.to_string());
        self
    }

    fn implied(&self, row: usize) -> CellStatus {
        match self.rows.get(row) {
            Some(RowStatus::LeftOnly) => CellStatus::LeftOnly,
            Some(RowStatus::RightOnly) => CellStatus::RightOnly,
            _ => CellStatus::Same,
        }
    }
}

fn index_as_u32(index: usize) -> u32 {
    u32::try_from(index).unwrap_or(u32::MAX)
}

impl Source for FakeSource {
    fn rows(&self) -> usize {
        self.rows.len()
    }

    fn columns(&self) -> &[ColumnInfo] {
        &self.columns
    }

    fn row_status(&self, row: usize) -> RowStatus {
        self.rows.get(row).copied().unwrap_or(RowStatus::Same)
    }

    fn row_numbers(&self, row: usize) -> (Option<u32>, Option<u32>) {
        let number = index_as_u32(row);
        match self.row_status(row) {
            RowStatus::LeftOnly => (Some(number), None),
            RowStatus::RightOnly => (None, Some(number)),
            _ => (Some(number), Some(number)),
        }
    }

    fn cell_status(&self, row: usize, column: usize) -> CellStatus {
        self.cells
            .get(&(row, column))
            .copied()
            .unwrap_or_else(|| self.implied(row))
    }

    fn cell_text(&self, row: usize, column: usize, side: Side) -> Cow<'_, str> {
        self.texts.get(&(row, column, side)).map_or_else(
            || Cow::Owned(format!("{}{row}-{column}", side.label())),
            |text| Cow::Borrowed(text.as_str()),
        )
    }

    fn totals(&self) -> Totals {
        let mut totals = Totals::default();
        for row in 0..self.rows.len() {
            match self.row_status(row) {
                RowStatus::Same => totals.same += 1,
                RowStatus::Unimportant => totals.unimportant += 1,
                RowStatus::LeftOnly => totals.left_only += 1,
                RowStatus::RightOnly => totals.right_only += 1,
                _ => totals.different += 1,
            }
        }
        totals
    }

    fn type_fallbacks(&self, _column: usize) -> u64 {
        0
    }

    fn notices(&self) -> &[String] {
        &self.notices
    }
}
