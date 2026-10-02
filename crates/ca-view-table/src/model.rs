//! The grid model: what a visual row is, which rows and columns a viewport
//! covers, and where navigation lands.
//!
//! Nothing here paints or reads a file. Every answer is a pure function of the
//! comparison the worker posted plus the display settings, so the whole module
//! is testable without a frame.
//!
//! The comparison is reached through [`Source`] rather than through the engine
//! types directly. A view over a real comparison and a view over a generated
//! one then share one painting path, and a cost measurement over a very large
//! model does not have to build that model in memory first.

use ca_table::compare::{CellStatus, RowStatus, Totals};
use ca_ui::theme::table::CellClass;
use std::borrow::Cow;
#[cfg(test)]
use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::sync::Arc;

/// No column may be dragged narrower than this, in points.
pub const MINIMUM_COLUMN: f32 = 28.0;

/// Width a column starts at, in points.
pub const DEFAULT_COLUMN: f32 = 120.0;

/// Which side of the comparison a cell belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    /// The left grid.
    Left,
    /// The right grid.
    Right,
}

impl Side {
    /// The other side.
    #[must_use]
    pub const fn other(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }

    /// What the status bar calls this side.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Left => "Left",
            Self::Right => "Right",
        }
    }
}

/// One comparison column, as the grid needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnInfo {
    /// Heading shown above both grids.
    pub name: String,
    /// Header name mapped from the left file, empty when none was mapped.
    pub left_name: String,
    /// Header name mapped from the right file, empty when none was mapped.
    pub right_name: String,
    /// Left file column letter, empty when the column is unmapped there.
    pub left_letter: String,
    /// Right file column letter, empty when the column is unmapped there.
    pub right_letter: String,
    /// Zero based column of the left source, when paired.
    pub left_index: Option<u32>,
    /// Zero based column of the right source, when paired.
    pub right_index: Option<u32>,
    /// True when the column controls sorting and alignment.
    pub key: bool,
    /// True when the column's differences do not matter.
    pub unimportant: bool,
    /// How the column's cells are read.
    pub type_label: &'static str,
}

impl ColumnInfo {
    fn identity(&self) -> ColumnIdentity {
        if !self.left_name.is_empty() || !self.right_name.is_empty() {
            let (first, second) = if self.left_name <= self.right_name {
                (&self.left_name, &self.right_name)
            } else {
                (&self.right_name, &self.left_name)
            };
            ColumnIdentity::Mapped(first.clone(), second.clone())
        } else {
            ColumnIdentity::Named(self.name.clone())
        }
    }
}

#[derive(Debug, Clone)]
enum ColumnIdentity {
    Mapped(String, String),
    Named(String),
}

#[cfg(test)]
thread_local! {
    static IDENTITY_EQUALITY_COMPARISONS: Cell<usize> = const { Cell::new(0) };
}

impl PartialEq for ColumnIdentity {
    fn eq(&self, other: &Self) -> bool {
        #[cfg(test)]
        IDENTITY_EQUALITY_COMPARISONS.with(|count| count.set(count.get() + 1));

        match (self, other) {
            (Self::Mapped(left_a, right_a), Self::Mapped(left_b, right_b)) => {
                left_a == left_b && right_a == right_b
            }
            (Self::Named(left), Self::Named(right)) => left == right,
            _ => false,
        }
    }
}

impl Eq for ColumnIdentity {}

impl Hash for ColumnIdentity {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            Self::Mapped(left, right) => {
                0_u8.hash(state);
                left.hash(state);
                right.hash(state);
            }
            Self::Named(name) => {
                1_u8.hash(state);
                name.hash(state);
            }
        }
    }
}

fn match_identities<K: Eq + Hash>(
    old: impl IntoIterator<Item = K>,
    new: impl IntoIterator<Item = K>,
) -> Vec<Option<usize>> {
    let mut old_indices: HashMap<K, VecDeque<usize>> = HashMap::new();
    for (index, identity) in old.into_iter().enumerate() {
        old_indices.entry(identity).or_default().push_back(index);
    }
    new.into_iter()
        .map(|identity| old_indices.get_mut(&identity).and_then(VecDeque::pop_front))
        .collect()
}

/// The comparison a grid paints.
pub trait Source: Send + Sync {
    /// How many aligned rows the comparison holds.
    fn rows(&self) -> usize;

    /// The comparison columns, in display order.
    fn columns(&self) -> &[ColumnInfo];

    /// What the comparison found in one row.
    fn row_status(&self, row: usize) -> RowStatus;

    /// The file row numbers behind one aligned row, one per side.
    fn row_numbers(&self, row: usize) -> (Option<u32>, Option<u32>);

    /// What the comparison found in one cell.
    fn cell_status(&self, row: usize, column: usize) -> CellStatus;

    /// The text of one cell on one side.
    fn cell_text(&self, row: usize, column: usize, side: Side) -> Cow<'_, str>;

    /// The row and cell counts of the whole comparison.
    fn totals(&self) -> Totals;

    /// How many cells of a column did not read as the column's type.
    fn type_fallbacks(&self, column: usize) -> u64;

    /// What the view should tell the user without blocking them.
    fn notices(&self) -> &[String];
}

/// An empty comparison, which is what a view shows before its first result.
#[derive(Debug, Default)]
pub struct EmptySource {
    columns: Vec<ColumnInfo>,
    notices: Vec<String>,
}

impl Source for EmptySource {
    fn rows(&self) -> usize {
        0
    }

    fn columns(&self) -> &[ColumnInfo] {
        &self.columns
    }

    fn row_status(&self, _row: usize) -> RowStatus {
        RowStatus::Same
    }

    fn row_numbers(&self, _row: usize) -> (Option<u32>, Option<u32>) {
        (None, None)
    }

    fn cell_status(&self, _row: usize, _column: usize) -> CellStatus {
        CellStatus::Same
    }

    fn cell_text(&self, _row: usize, _column: usize, _side: Side) -> Cow<'_, str> {
        Cow::Borrowed("")
    }

    fn totals(&self) -> Totals {
        Totals::default()
    }

    fn type_fallbacks(&self, _column: usize) -> u64 {
        0
    }

    fn notices(&self) -> &[String] {
        &self.notices
    }
}

/// The class a cell is painted as.
///
/// A row present on one side only paints filler on the other side, so the
/// missing grid reads as a gap rather than as an empty value.
#[must_use]
pub const fn cell_class(row: RowStatus, cell: CellStatus, side: Side) -> CellClass {
    match row {
        RowStatus::LeftOnly => match side {
            Side::Left => CellClass::LeftOnly,
            Side::Right => CellClass::Gap,
        },
        RowStatus::RightOnly => match side {
            Side::Left => CellClass::Gap,
            Side::Right => CellClass::RightOnly,
        },
        _ => match cell {
            CellStatus::Same => CellClass::Same,
            CellStatus::Unimportant => CellClass::Unimportant,
            CellStatus::LeftOnly => match side {
                Side::Left => CellClass::LeftOnly,
                Side::Right => CellClass::Gap,
            },
            CellStatus::RightOnly => match side {
                Side::Left => CellClass::Gap,
                Side::Right => CellClass::RightOnly,
            },
            // A difference, including one a later build names, paints in the
            // important color, which is the one that must not be missed.
            _ => CellClass::Different,
        },
    }
}

/// The class the gutter spot of a row is drawn in.
#[must_use]
pub const fn row_class(status: RowStatus) -> CellClass {
    match status {
        RowStatus::Same => CellClass::Same,
        RowStatus::Unimportant => CellClass::Unimportant,
        RowStatus::LeftOnly => CellClass::LeftOnly,
        RowStatus::RightOnly => CellClass::RightOnly,
        _ => CellClass::Different,
    }
}

/// Which rows the grid shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DisplayFilter {
    /// Every row.
    #[default]
    All,
    /// Only rows that differ.
    Differences,
    /// Only rows that match.
    Same,
    /// No rows.
    None,
}

impl DisplayFilter {
    /// The label a menu or button shows.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "Show All",
            Self::Differences => "Show Differences",
            Self::Same => "Show Same",
            Self::None => "Show None",
        }
    }

    /// True when a row of this status is kept.
    #[must_use]
    pub const fn keeps(self, status: RowStatus, ignore_unimportant: bool) -> bool {
        let differs = match status {
            RowStatus::Same => false,
            RowStatus::Unimportant => !ignore_unimportant,
            _ => true,
        };
        match self {
            Self::All => true,
            Self::Differences => differs,
            Self::Same => !differs,
            Self::None => false,
        }
    }
}

/// The current cell, as a visual row and a display column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cursor {
    /// Row of the display, not of either file.
    pub row: usize,
    /// Index into the columns the grid shows.
    pub column: usize,
}

/// The grid's whole display state.
pub struct Grid {
    source: Arc<dyn Source>,
    filter: DisplayFilter,
    ignore_unimportant: bool,
    hide_same_columns: bool,
    /// Source row of each visual row. Empty when every row is kept, which is
    /// what keeps a very large comparison from paying for an index it does not
    /// need.
    visible: Vec<u32>,
    /// Width of each comparison column, indexed as the source indexes them.
    widths: Vec<f32>,
    /// Comparison columns the display hides, indexed the same way.
    hidden: Vec<bool>,
    /// Comparison columns the display shows, in display order.
    order: Vec<usize>,
    /// Left edge of each shown column, plus the total width at the end.
    edges: Vec<f32>,
    cursor: Cursor,
}

impl Default for Grid {
    fn default() -> Self {
        Self::new(Arc::new(EmptySource::default()))
    }
}

impl Grid {
    /// A grid over one comparison, showing every row and column.
    #[must_use]
    pub fn new(source: Arc<dyn Source>) -> Self {
        let mut grid = Self {
            widths: vec![DEFAULT_COLUMN; source.columns().len()],
            hidden: vec![false; source.columns().len()],
            source,
            filter: DisplayFilter::All,
            ignore_unimportant: false,
            hide_same_columns: false,
            visible: Vec::new(),
            order: Vec::new(),
            edges: Vec::new(),
            cursor: Cursor::default(),
        };
        grid.rebuild();
        grid
    }

    /// Replace the comparison, keeping the display settings.
    ///
    /// Widths and hidden state follow their mapped headers where they persist.
    pub fn set_source(&mut self, source: Arc<dyn Source>) {
        let old_columns = self.source.columns();
        let mut widths = vec![DEFAULT_COLUMN; source.columns().len()];
        let mut hidden = vec![false; source.columns().len()];
        let matches = match_identities(
            old_columns.iter().map(ColumnInfo::identity),
            source.columns().iter().map(ColumnInfo::identity),
        );
        for (new_index, old_index) in matches.into_iter().enumerate() {
            let Some(old_index) = old_index else {
                continue;
            };
            widths[new_index] = self.widths[old_index];
            hidden[new_index] = self.hidden[old_index];
        }
        self.widths = widths;
        self.hidden = hidden;
        self.source = source;
        self.rebuild();
        self.clamp_cursor();
    }

    /// The comparison behind the grid.
    #[must_use]
    pub fn source(&self) -> &Arc<dyn Source> {
        &self.source
    }

    /// The filter in force.
    #[must_use]
    pub const fn filter(&self) -> DisplayFilter {
        self.filter
    }

    /// Show a different set of rows.
    pub fn set_filter(&mut self, filter: DisplayFilter) {
        if self.filter == filter {
            return;
        }
        self.filter = filter;
        self.rebuild_rows();
        self.clamp_cursor();
    }

    /// True when unimportant differences count as matching.
    #[must_use]
    pub const fn ignores_unimportant(&self) -> bool {
        self.ignore_unimportant
    }

    /// Treat unimportant differences as matching, or stop doing so.
    pub fn set_ignore_unimportant(&mut self, ignore: bool) {
        if self.ignore_unimportant == ignore {
            return;
        }
        self.ignore_unimportant = ignore;
        self.rebuild_rows();
        self.clamp_cursor();
    }

    /// True when columns holding no difference are hidden.
    #[must_use]
    pub const fn hides_same_columns(&self) -> bool {
        self.hide_same_columns
    }

    /// Hide columns holding no difference, or show them again.
    pub fn set_hide_same_columns(&mut self, hide: bool) {
        if self.hide_same_columns == hide {
            return;
        }
        self.hide_same_columns = hide;
        self.rebuild_columns();
        self.clamp_cursor();
    }

    /// Hide or show one comparison column by its source index.
    pub fn set_column_hidden(&mut self, column: usize, hidden: bool) {
        if let Some(slot) = self.hidden.get_mut(column) {
            *slot = hidden;
            self.rebuild_columns();
            self.clamp_cursor();
        }
    }

    /// True when the comparison column is hidden.
    #[must_use]
    pub fn is_column_hidden(&self, column: usize) -> bool {
        self.hidden.get(column).copied().unwrap_or(false)
    }

    /// The hidden comparison columns, in source order.
    pub fn hidden_columns(&self) -> impl Iterator<Item = (usize, &ColumnInfo)> + '_ {
        self.source
            .columns()
            .iter()
            .enumerate()
            .filter(|(column, _)| self.hidden[*column])
    }

    /// How many rows the display holds.
    #[must_use]
    pub fn visual_rows(&self) -> usize {
        match self.filter {
            DisplayFilter::All => self.source.rows(),
            DisplayFilter::None => 0,
            _ => self.visible.len(),
        }
    }

    /// The comparison row a display row stands for.
    #[must_use]
    pub fn source_row(&self, visual: usize) -> Option<usize> {
        match self.filter {
            DisplayFilter::All => (visual < self.source.rows()).then_some(visual),
            DisplayFilter::None => None,
            _ => self.visible.get(visual).map(|row| *row as usize),
        }
    }

    /// The display row a comparison row sits on, when it is shown at all.
    #[must_use]
    pub fn visual_of_source(&self, source: usize) -> Option<usize> {
        match self.filter {
            DisplayFilter::All => (source < self.source.rows()).then_some(source),
            DisplayFilter::None => None,
            _ => {
                let wanted = u32::try_from(source).ok()?;
                self.visible.binary_search(&wanted).ok()
            }
        }
    }

    /// The comparison columns the display shows, in display order.
    #[must_use]
    pub fn shown_columns(&self) -> &[usize] {
        &self.order
    }

    /// The comparison column behind a display column.
    #[must_use]
    pub fn column_at(&self, display: usize) -> Option<usize> {
        self.order.get(display).copied()
    }

    /// The width of a comparison column.
    #[must_use]
    pub fn width(&self, column: usize) -> f32 {
        self.widths.get(column).copied().unwrap_or(DEFAULT_COLUMN)
    }

    /// Set the width of a comparison column, never below the minimum.
    pub fn set_width(&mut self, column: usize, width: f32) {
        if let Some(slot) = self.widths.get_mut(column) {
            *slot = width.max(MINIMUM_COLUMN);
            self.rebuild_edges();
        }
    }

    /// Set every comparison column width at once, never below the minimum.
    ///
    /// A bulk update rebuilds the horizontal layout once rather than once per
    /// column, which matters when a wide table is resized to fit.
    pub fn set_widths(&mut self, widths: &[f32]) {
        if widths.len() != self.widths.len() {
            return;
        }
        for (slot, width) in self.widths.iter_mut().zip(widths) {
            *slot = width.max(MINIMUM_COLUMN);
        }
        self.rebuild_edges();
    }

    /// The left edge of a display column, in points from the first column.
    #[must_use]
    pub fn column_x(&self, display: usize) -> f32 {
        self.edges.get(display).copied().unwrap_or(0.0)
    }

    /// The width of every shown column together.
    #[must_use]
    pub fn total_width(&self) -> f32 {
        self.edges.last().copied().unwrap_or(0.0)
    }

    /// The display columns a viewport covers, with one column of overscan on
    /// each side so a partly visible column is still laid out.
    #[must_use]
    pub fn visible_columns(&self, offset: f32, viewport: f32) -> std::ops::Range<usize> {
        let count = self.order.len();
        if count == 0 || viewport <= 0.0 {
            return 0..0;
        }
        let left = offset.max(0.0);
        let right = left + viewport;
        // `edges` is ascending, so the first column whose right edge is past
        // the viewport's left edge is the first one that can be seen.
        let first = self.edges[1..]
            .partition_point(|edge| *edge <= left)
            .min(count);
        let last = self.edges[1..]
            .partition_point(|edge| *edge < right)
            .saturating_add(1)
            .min(count);
        first..last.max(first)
    }

    /// The display column a point sits in, measured from the first column.
    #[must_use]
    pub fn column_at_x(&self, x: f32) -> Option<usize> {
        if self.order.is_empty() || x < 0.0 || x >= self.total_width() {
            return None;
        }
        let index = self.edges[1..].partition_point(|edge| *edge <= x);
        (index < self.order.len()).then_some(index)
    }

    /// The current cell.
    #[must_use]
    pub const fn cursor(&self) -> Cursor {
        self.cursor
    }

    /// Put the current cell somewhere, clamped to the display.
    pub fn set_cursor(&mut self, cursor: Cursor) {
        self.cursor = cursor;
        self.clamp_cursor();
    }

    /// Move the current cell by whole steps, clamped to the display.
    pub fn move_cursor(&mut self, rows: i64, columns: i64) {
        let row = saturating_step(self.cursor.row, rows);
        let column = saturating_step(self.cursor.column, columns);
        self.cursor = Cursor { row, column };
        self.clamp_cursor();
    }

    /// Hold the current cell inside the display.
    pub fn clamp_cursor(&mut self) {
        let rows = self.visual_rows();
        self.cursor.row = self.cursor.row.min(rows.saturating_sub(1));
        self.cursor.column = self.cursor.column.min(self.order.len().saturating_sub(1));
        if rows == 0 {
            self.cursor.row = 0;
        }
        if self.order.is_empty() {
            self.cursor.column = 0;
        }
    }

    /// What the comparison found in a display row.
    #[must_use]
    pub fn row_status(&self, visual: usize) -> Option<RowStatus> {
        self.source_row(visual)
            .map(|row| self.source.row_status(row))
    }

    /// True when a display row holds a difference the filter counts.
    #[must_use]
    pub fn row_differs(&self, visual: usize) -> bool {
        match self.row_status(visual) {
            Some(RowStatus::Same) | None => false,
            Some(RowStatus::Unimportant) => !self.ignore_unimportant,
            Some(_) => true,
        }
    }

    /// The first differing row at or after `from`.
    #[must_use]
    pub fn next_difference(&self, from: usize) -> Option<usize> {
        (from..self.visual_rows()).find(|row| self.row_differs(*row))
    }

    /// The last differing row at or before `from`.
    #[must_use]
    pub fn previous_difference(&self, from: usize) -> Option<usize> {
        let limit = from.min(self.visual_rows().saturating_sub(1));
        (0..=limit).rev().find(|row| self.row_differs(*row))
    }

    /// The first row of the next run of differing rows after `from`.
    #[must_use]
    pub fn next_section(&self, from: usize) -> Option<usize> {
        let rows = self.visual_rows();
        let mut index = from;
        // Step off the run the cursor already sits on, otherwise the answer is
        // the row the cursor is already on.
        while index < rows && self.row_differs(index) {
            index += 1;
        }
        self.next_difference(index)
    }

    /// The first row of the previous run of differing rows before `from`.
    #[must_use]
    pub fn previous_section(&self, from: usize) -> Option<usize> {
        let mut index = from;
        while index > 0 && self.row_differs(index.saturating_sub(1)) {
            index -= 1;
        }
        let end = self.previous_difference(index.checked_sub(1)?)?;
        let mut start = end;
        while start > 0 && self.row_differs(start - 1) {
            start -= 1;
        }
        Some(start)
    }

    /// The first differing cell at or after the current one, scanning rows in
    /// display order.
    #[must_use]
    pub fn next_difference_cell(&self, from: Cursor) -> Option<Cursor> {
        let rows = self.visual_rows();
        let columns = self.order.len();
        if columns == 0 {
            return None;
        }
        let mut row = from.row;
        let mut column = from.column.saturating_add(1);
        while row < rows {
            while column < columns {
                if self.cell_differs(row, column) {
                    return Some(Cursor { row, column });
                }
                column += 1;
            }
            row += 1;
            column = 0;
        }
        None
    }

    /// The last differing cell before the current one.
    #[must_use]
    pub fn previous_difference_cell(&self, from: Cursor) -> Option<Cursor> {
        let columns = self.order.len();
        if columns == 0 {
            return None;
        }
        let mut row = from.row.min(self.visual_rows().saturating_sub(1));
        let mut column = from.column;
        loop {
            while column > 0 {
                column -= 1;
                if self.cell_differs(row, column) {
                    return Some(Cursor { row, column });
                }
            }
            if row == 0 {
                return None;
            }
            row -= 1;
            column = columns;
        }
    }

    /// True when a display cell holds a difference the filter counts.
    #[must_use]
    pub fn cell_differs(&self, visual: usize, display_column: usize) -> bool {
        let (Some(row), Some(column)) = (self.source_row(visual), self.column_at(display_column))
        else {
            return false;
        };
        match self.source.cell_status(row, column) {
            CellStatus::Same => false,
            CellStatus::Unimportant => !self.ignore_unimportant,
            _ => true,
        }
    }

    /// The text of a display cell on one side.
    #[must_use]
    pub fn cell_text(&self, visual: usize, display_column: usize, side: Side) -> Cow<'_, str> {
        let (Some(row), Some(column)) = (self.source_row(visual), self.column_at(display_column))
        else {
            return Cow::Borrowed("");
        };
        let status = self.source.row_status(row);
        let gap = matches!(
            (status, side),
            (RowStatus::LeftOnly, Side::Right) | (RowStatus::RightOnly, Side::Left)
        );
        if gap {
            return Cow::Borrowed("");
        }
        self.source.cell_text(row, column, side)
    }

    /// The class a display cell is painted as.
    #[must_use]
    pub fn cell_class(&self, visual: usize, display_column: usize, side: Side) -> CellClass {
        let (Some(row), Some(column)) = (self.source_row(visual), self.column_at(display_column))
        else {
            return CellClass::Gap;
        };
        let status = self.source.row_status(row);
        let cell = self.source.cell_status(row, column);
        let class = cell_class(status, cell, side);
        if self.ignore_unimportant && class == CellClass::Unimportant {
            CellClass::Same
        } else {
            class
        }
    }

    /// How many display rows hold a difference the filter counts.
    #[must_use]
    pub fn difference_rows(&self) -> usize {
        (0..self.visual_rows())
            .filter(|row| self.row_differs(*row))
            .count()
    }

    /// Lay the rows and the columns out again.
    pub fn rebuild(&mut self) {
        self.rebuild_rows();
        self.rebuild_columns();
    }

    fn rebuild_rows(&mut self) {
        if matches!(self.filter, DisplayFilter::All | DisplayFilter::None) {
            self.visible = Vec::new();
            return;
        }
        let ignore = self.ignore_unimportant;
        let filter = self.filter;
        self.visible = (0..self.source.rows())
            .filter(|row| filter.keeps(self.source.row_status(*row), ignore))
            .filter_map(|row| u32::try_from(row).ok())
            .collect();
    }

    fn rebuild_columns(&mut self) {
        let count = self.source.columns().len();
        self.order = (0..count)
            .filter(|column| !self.hidden.get(*column).copied().unwrap_or(false))
            .filter(|column| !(self.hide_same_columns && self.column_is_same(*column)))
            .collect();
        self.rebuild_edges();
    }

    fn rebuild_edges(&mut self) {
        self.edges = Vec::with_capacity(self.order.len() + 1);
        let mut x = 0.0;
        self.edges.push(0.0);
        for column in &self.order {
            x += self.widths.get(*column).copied().unwrap_or(DEFAULT_COLUMN);
            self.edges.push(x);
        }
    }

    /// True when no row of the column holds a difference.
    ///
    /// The scan is over the whole comparison, so it runs when the setting or
    /// the data changes and never inside a frame.
    fn column_is_same(&self, column: usize) -> bool {
        !(0..self.source.rows()).any(|row| self.source.cell_status(row, column).is_difference())
    }
}

/// A cursor position moved by a signed number of steps, without wrapping.
fn saturating_step(position: usize, steps: i64) -> usize {
    if steps >= 0 {
        position.saturating_add(usize::try_from(steps).unwrap_or(usize::MAX))
    } else {
        position.saturating_sub(usize::try_from(steps.saturating_neg()).unwrap_or(usize::MAX))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{cell_class, Cursor, DisplayFilter, Grid, Side, MINIMUM_COLUMN};
    use crate::testing::FakeSource;
    use ca_table::compare::{CellStatus, RowStatus};
    use ca_ui::theme::table::CellClass;
    use std::hash::{Hash, Hasher};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[derive(Clone)]
    struct CountedIdentity {
        value: usize,
        comparisons: Arc<AtomicUsize>,
    }

    impl PartialEq for CountedIdentity {
        fn eq(&self, other: &Self) -> bool {
            self.comparisons.fetch_add(1, Ordering::Relaxed);
            self.value == other.value
        }
    }

    impl Eq for CountedIdentity {}

    impl Hash for CountedIdentity {
        fn hash<H: Hasher>(&self, state: &mut H) {
            self.value.hash(state);
        }
    }

    fn grid(statuses: &[RowStatus], columns: usize) -> Grid {
        Grid::new(Arc::new(FakeSource::new(statuses, columns)))
    }

    #[test]
    fn an_unfiltered_grid_maps_rows_one_to_one() {
        let grid = grid(&[RowStatus::Same, RowStatus::Different], 3);
        assert_eq!(grid.visual_rows(), 2);
        assert_eq!(grid.source_row(1), Some(1));
        assert_eq!(grid.visual_of_source(1), Some(1));
        assert_eq!(grid.source_row(2), None);
    }

    #[test]
    fn a_difference_filter_renumbers_the_rows() {
        let mut grid = grid(
            &[
                RowStatus::Same,
                RowStatus::Different,
                RowStatus::Same,
                RowStatus::LeftOnly,
            ],
            2,
        );
        grid.set_filter(DisplayFilter::Differences);
        assert_eq!(grid.visual_rows(), 2);
        assert_eq!(grid.source_row(0), Some(1));
        assert_eq!(grid.source_row(1), Some(3));
        assert_eq!(grid.visual_of_source(3), Some(1));
        assert_eq!(grid.visual_of_source(2), None);
    }

    #[test]
    fn showing_same_rows_is_the_complement_of_showing_differences() {
        let mut grid = grid(&[RowStatus::Same, RowStatus::Different, RowStatus::Same], 1);
        grid.set_filter(DisplayFilter::Same);
        assert_eq!(grid.visual_rows(), 2);
        assert_eq!(grid.source_row(1), Some(2));
        grid.set_filter(DisplayFilter::None);
        assert_eq!(grid.visual_rows(), 0);
        assert_eq!(grid.source_row(0), None);
    }

    #[test]
    fn ignoring_unimportant_moves_those_rows_into_the_matching_set() {
        let mut grid = grid(&[RowStatus::Unimportant, RowStatus::Different], 1);
        grid.set_filter(DisplayFilter::Differences);
        assert_eq!(grid.visual_rows(), 2);
        grid.set_ignore_unimportant(true);
        assert_eq!(grid.visual_rows(), 1);
        assert_eq!(grid.source_row(0), Some(1));
    }

    #[test]
    fn navigation_finds_the_next_and_previous_differing_row() {
        let grid = grid(
            &[
                RowStatus::Same,
                RowStatus::Different,
                RowStatus::Same,
                RowStatus::Same,
                RowStatus::RightOnly,
            ],
            1,
        );
        assert_eq!(grid.next_difference(0), Some(1));
        assert_eq!(grid.next_difference(2), Some(4));
        assert_eq!(grid.next_difference(5), None);
        assert_eq!(grid.previous_difference(4), Some(4));
        assert_eq!(grid.previous_difference(3), Some(1));
        assert_eq!(grid.previous_difference(0), None);
    }

    #[test]
    fn sections_skip_over_the_run_the_cursor_sits_on() {
        let grid = grid(
            &[
                RowStatus::Different,
                RowStatus::Different,
                RowStatus::Same,
                RowStatus::Different,
            ],
            1,
        );
        assert_eq!(grid.next_section(0), Some(3));
        assert_eq!(grid.next_section(3), None);
        assert_eq!(grid.previous_section(3), Some(0));
        assert_eq!(grid.previous_section(0), None);
    }

    #[test]
    fn columns_lay_out_as_running_edges() {
        let mut grid = grid(&[RowStatus::Same], 4);
        assert_eq!(grid.shown_columns(), &[0, 1, 2, 3]);
        assert!((grid.column_x(0)).abs() < f32::EPSILON);
        assert!((grid.column_x(2) - grid.width(0) * 2.0).abs() < 0.01);
        grid.set_width(1, 40.0);
        assert!((grid.column_x(2) - (grid.width(0) + 40.0)).abs() < 0.01);
        assert!((grid.total_width() - (grid.width(0) * 3.0 + 40.0)).abs() < 0.01);
    }

    #[test]
    fn a_column_dragged_narrow_stops_at_its_minimum() {
        let mut grid = grid(&[RowStatus::Same], 2);
        grid.set_width(0, 1.0);
        assert!((grid.width(0) - MINIMUM_COLUMN).abs() < f32::EPSILON);
    }

    #[test]
    fn setting_widths_rebuilds_edges_and_refuses_a_mismatched_list() {
        let mut grid = grid(&[RowStatus::Same], 2);
        grid.set_widths(&[80.0, 1.0]);
        assert!((grid.width(0) - 80.0).abs() < f32::EPSILON);
        assert!((grid.width(1) - MINIMUM_COLUMN).abs() < f32::EPSILON);
        assert!((grid.column_x(1) - 80.0).abs() < f32::EPSILON);
        grid.set_widths(&[100.0]);
        assert!((grid.width(0) - 80.0).abs() < f32::EPSILON);
        assert!((grid.width(1) - MINIMUM_COLUMN).abs() < f32::EPSILON);
    }

    #[test]
    fn a_viewport_exposes_only_the_columns_it_covers() {
        let mut grid = grid(&[RowStatus::Same], 50);
        for column in 0..50 {
            grid.set_width(column, 100.0);
        }
        let range = grid.visible_columns(0.0, 640.0);
        assert_eq!(range.start, 0);
        assert!(range.len() <= 9, "laid out {} columns", range.len());
        let far = grid.visible_columns(2_000.0, 640.0);
        assert_eq!(far.start, 20);
        assert!(far.len() <= 9, "laid out {} columns", far.len());
        assert_eq!(grid.visible_columns(0.0, 0.0), 0..0);
    }

    #[test]
    fn a_point_resolves_to_the_column_under_it() {
        let mut grid = grid(&[RowStatus::Same], 3);
        for column in 0..3 {
            grid.set_width(column, 100.0);
        }
        assert_eq!(grid.column_at_x(0.0), Some(0));
        assert_eq!(grid.column_at_x(99.9), Some(0));
        assert_eq!(grid.column_at_x(100.0), Some(1));
        assert_eq!(grid.column_at_x(299.9), Some(2));
        assert_eq!(grid.column_at_x(300.0), None);
        assert_eq!(grid.column_at_x(-1.0), None);
    }

    #[test]
    fn hiding_a_column_renumbers_the_display_columns() {
        let mut grid = grid(&[RowStatus::Same], 3);
        grid.set_column_hidden(1, true);
        assert_eq!(grid.shown_columns(), &[0, 2]);
        assert_eq!(grid.column_at(1), Some(2));
        assert!(grid.is_column_hidden(1));
    }

    #[test]
    fn hidden_columns_are_listed_in_source_order_with_their_names() {
        let mut grid = grid(&[RowStatus::Same], 3);
        grid.set_column_hidden(2, true);
        grid.set_column_hidden(0, true);

        let hidden: Vec<_> = grid
            .hidden_columns()
            .map(|(index, column)| (index, column.name.as_str()))
            .collect();
        assert_eq!(hidden, [(0, "Column 1"), (2, "Column 3")]);
    }

    #[test]
    fn hidden_columns_and_widths_follow_their_headers_when_sources_reorder() {
        let old = FakeSource::new(&[RowStatus::Same], 3)
            .with_column_name(0, "a")
            .with_column_name(1, "b")
            .with_column_name(2, "c");
        let mut grid = Grid::new(Arc::new(old));
        grid.set_column_hidden(2, true);
        grid.set_width(2, 245.0);

        let reordered = FakeSource::new(&[RowStatus::Same], 3)
            .with_column_name(0, "a")
            .with_column_name(1, "c")
            .with_column_name(2, "b");
        grid.set_source(Arc::new(reordered));

        assert_eq!(grid.shown_columns(), &[0, 2]);
        assert_eq!(
            grid.hidden_columns()
                .map(|(_, column)| column.name.as_str())
                .collect::<Vec<_>>(),
            ["c"]
        );
        assert!((grid.width(1) - 245.0).abs() < f32::EPSILON);
    }

    #[test]
    fn matching_many_reordered_columns_uses_bounded_identity_comparisons() {
        const COUNT: usize = 16_384;
        let comparisons = Arc::new(AtomicUsize::new(0));
        let old = (0..COUNT).map(|value| CountedIdentity {
            value,
            comparisons: Arc::clone(&comparisons),
        });
        let new = (0..COUNT).rev().map(|value| CountedIdentity {
            value,
            comparisons: Arc::clone(&comparisons),
        });

        let matches = super::match_identities(old, new);

        assert_eq!(matches.len(), COUNT);
        for (display_index, old_index) in matches.iter().enumerate() {
            assert_eq!(*old_index, Some(COUNT - display_index - 1));
        }
        assert!(
            comparisons.load(Ordering::Relaxed) <= COUNT * 16,
            "identity matching performed {} comparisons for {COUNT} columns",
            comparisons.load(Ordering::Relaxed)
        );
    }

    #[test]
    fn set_source_maps_a_large_reordered_column_set() {
        const COUNT: usize = 16_384;
        let old = (0..COUNT).fold(
            FakeSource::new(&[RowStatus::Same], COUNT),
            |source, index| source.with_column_name(index, &format!("column-{index:05}")),
        );
        let mut grid = Grid::new(Arc::new(old));
        let tracked = [0, 17, COUNT / 2, COUNT - 1];
        for index in tracked {
            grid.set_width(index, width_for(index));
        }
        grid.set_column_hidden(2, true);
        grid.set_column_hidden(COUNT - 3, true);

        let reordered = (0..COUNT).rev().fold(
            FakeSource::new(&[RowStatus::Same], COUNT),
            |source, index| {
                source.with_column_name(index, &format!("column-{:05}", COUNT - index - 1))
            },
        );

        super::IDENTITY_EQUALITY_COMPARISONS.with(|count| count.set(0));
        grid.set_source(Arc::new(reordered));
        let comparisons = super::IDENTITY_EQUALITY_COMPARISONS.with(std::cell::Cell::get);
        assert!(
            comparisons <= COUNT * 16,
            "Grid::set_source performed {comparisons} column identity comparisons for {COUNT} columns"
        );

        for new_index in tracked.map(|old_index| COUNT - old_index - 1) {
            let old_index = COUNT - new_index - 1;
            let expected_width = width_for(old_index);
            assert!((grid.width(new_index) - expected_width).abs() < f32::EPSILON);
        }
        assert!(grid.is_column_hidden(COUNT - 3));
        assert!(grid.is_column_hidden(2));
    }

    fn width_for(index: usize) -> f32 {
        30.0 + f32::from(u16::try_from(index % 80).unwrap())
    }

    #[test]
    fn repeated_column_identities_keep_their_original_order() {
        assert_eq!(
            super::match_identities(
                ["duplicate", "duplicate", "other"],
                ["duplicate", "other", "duplicate", "missing"]
            ),
            [Some(0), Some(2), Some(1), None]
        );
    }

    #[test]
    fn the_cursor_stays_inside_the_display() {
        let mut grid = grid(&[RowStatus::Same, RowStatus::Different], 2);
        grid.set_cursor(Cursor {
            row: 99,
            column: 99,
        });
        assert_eq!(grid.cursor(), Cursor { row: 1, column: 1 });
        grid.move_cursor(-99, -99);
        assert_eq!(grid.cursor(), Cursor { row: 0, column: 0 });
        grid.set_filter(DisplayFilter::None);
        assert_eq!(grid.cursor(), Cursor { row: 0, column: 0 });
    }

    #[test]
    fn cell_navigation_walks_rows_in_display_order() {
        let source = FakeSource::new(&[RowStatus::Different, RowStatus::Different], 3)
            .with_cell(0, 2, CellStatus::Different)
            .with_cell(1, 0, CellStatus::Different);
        let grid = Grid::new(Arc::new(source));
        let first = grid.next_difference_cell(Cursor { row: 0, column: 0 });
        assert_eq!(first, Some(Cursor { row: 0, column: 2 }));
        let second = grid.next_difference_cell(first.unwrap());
        assert_eq!(second, Some(Cursor { row: 1, column: 0 }));
        assert_eq!(grid.next_difference_cell(second.unwrap()), None);
        assert_eq!(
            grid.previous_difference_cell(Cursor { row: 1, column: 1 }),
            Some(Cursor { row: 1, column: 0 })
        );
        assert_eq!(
            grid.previous_difference_cell(Cursor { row: 0, column: 0 }),
            None
        );
    }

    #[test]
    fn a_row_present_on_one_side_paints_filler_opposite_it() {
        assert_eq!(
            cell_class(RowStatus::LeftOnly, CellStatus::LeftOnly, Side::Right),
            CellClass::Gap
        );
        assert_eq!(
            cell_class(RowStatus::LeftOnly, CellStatus::LeftOnly, Side::Left),
            CellClass::LeftOnly
        );
        assert_eq!(
            cell_class(RowStatus::Different, CellStatus::Unimportant, Side::Left),
            CellClass::Unimportant
        );
    }

    #[test]
    fn ignoring_unimportant_paints_those_cells_as_matching() {
        let source =
            FakeSource::new(&[RowStatus::Unimportant], 2).with_cell(0, 1, CellStatus::Unimportant);
        let mut grid = Grid::new(Arc::new(source));
        assert_eq!(grid.cell_class(0, 1, Side::Left), CellClass::Unimportant);
        grid.set_ignore_unimportant(true);
        assert_eq!(grid.cell_class(0, 1, Side::Left), CellClass::Same);
        assert!(!grid.cell_differs(0, 1));
    }

    #[test]
    fn hiding_matching_columns_keeps_only_those_holding_a_difference() {
        let source = FakeSource::new(&[RowStatus::Different, RowStatus::Same], 3).with_cell(
            0,
            1,
            CellStatus::Different,
        );
        let mut grid = Grid::new(Arc::new(source));
        grid.set_hide_same_columns(true);
        assert_eq!(grid.shown_columns(), &[1]);
        grid.set_hide_same_columns(false);
        assert_eq!(grid.shown_columns(), &[0, 1, 2]);
    }

    #[test]
    fn a_gap_cell_reads_as_empty_whatever_the_file_holds() {
        let source = FakeSource::new(&[RowStatus::LeftOnly], 1);
        let grid = Grid::new(Arc::new(source));
        assert_eq!(grid.cell_text(0, 0, Side::Right), "");
        assert!(!grid.cell_text(0, 0, Side::Left).is_empty());
    }

    #[test]
    fn an_empty_comparison_answers_without_panicking() {
        let grid = Grid::default();
        assert_eq!(grid.visual_rows(), 0);
        assert_eq!(grid.shown_columns().len(), 0);
        assert_eq!(grid.next_difference(0), None);
        assert_eq!(grid.visible_columns(0.0, 100.0), 0..0);
        assert_eq!(grid.source().rows(), 0);
    }
}
