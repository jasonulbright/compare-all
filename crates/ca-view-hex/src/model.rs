//! The row layout of a byte comparison: the mapping between the two files'
//! byte offsets and the visual rows they are painted on.
//!
//! A row holds a fixed number of bytes from each side. A row never spans two
//! aligned regions, so the first byte of a region always starts a row and
//! counterpart bytes stay level with each other. Where a region gives one side
//! more bytes than the other, the shorter side's rows are gaps.
//!
//! Nothing here is materialized per row. A file of several hundred megabytes
//! reaches tens of millions of rows, so a row is computed from the aligned
//! regions on demand and the stored state is proportional to the number of
//! regions instead. Everything is free of painting, so the alignment, the
//! filters and the navigation are tested directly.

use ca_diff::{ByteHunk, HunkKind};
use std::ops::Range;

/// Smallest number of bytes a row may show.
pub const MIN_BYTES_PER_ROW: u32 = 1;

/// Largest number of bytes a row may show.
pub const MAX_BYTES_PER_ROW: u32 = 256;

/// Bytes a row shows until the view is told otherwise.
pub const DEFAULT_BYTES_PER_ROW: u32 = 16;

/// What a row shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowClass {
    /// Both sides carry the same bytes.
    Same,
    /// Both sides carry bytes and the two regions differ.
    Changed,
    /// Only the left side carries bytes.
    LeftOnly,
    /// Only the right side carries bytes.
    RightOnly,
}

impl RowClass {
    /// True for every class that counts as a difference.
    #[must_use]
    pub const fn is_difference(self) -> bool {
        !matches!(self, Self::Same)
    }
}

/// The bytes one side of a row shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowSpan {
    /// Offset of the first byte in the file.
    pub start: u64,
    /// How many bytes the row shows.
    pub len: u32,
}

impl RowSpan {
    /// One past the last byte the row shows.
    #[must_use]
    pub const fn end(self) -> u64 {
        self.start.saturating_add(self.len as u64)
    }

    /// True when `offset` falls inside the span.
    #[must_use]
    pub const fn contains(self, offset: u64) -> bool {
        offset >= self.start && offset < self.end()
    }

    /// The span as a byte range.
    #[must_use]
    pub const fn range(self) -> Range<u64> {
        self.start..self.end()
    }
}

/// One visual row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HexRow {
    /// What the row shows.
    pub class: RowClass,
    /// Left side bytes, or `None` when the left side is a gap here.
    pub left: Option<RowSpan>,
    /// Right side bytes, or `None` when the right side is a gap here.
    pub right: Option<RowSpan>,
    /// Index into [`RowModel::sections`], or `None` on a matching row.
    pub section: Option<u32>,
}

impl HexRow {
    /// The bytes one side of the row shows.
    #[must_use]
    pub const fn side(&self, side: Side) -> Option<RowSpan> {
        match side {
            Side::Left => self.left,
            Side::Right => self.right,
        }
    }
}

/// Which pane a position belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Side {
    /// The left pane.
    #[default]
    Left,
    /// The right pane.
    Right,
}

impl Side {
    /// The pane on the other side.
    #[must_use]
    pub const fn other(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }

    /// The label a status line or a button shows.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Left => "Left",
            Self::Right => "Right",
        }
    }
}

/// How much a comparison found.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    /// Rows in the layout.
    pub rows: usize,
    /// Rows that differ, of any class.
    pub difference_rows: usize,
    /// Contiguous runs of differing rows.
    pub sections: usize,
    /// Bytes present on both sides that differ.
    pub changed_bytes: u64,
    /// Bytes the left side has and the right side does not.
    pub left_only_bytes: u64,
    /// Bytes the right side has and the left side does not.
    pub right_only_bytes: u64,
}

impl Counts {
    /// Every byte the comparison reports as a difference, on either side.
    #[must_use]
    pub const fn difference_bytes(&self) -> u64 {
        self.changed_bytes
            .saturating_add(self.left_only_bytes)
            .saturating_add(self.right_only_bytes)
    }
}

/// The rows of one comparison plus the indexes navigation needs.
#[derive(Debug, Clone)]
pub struct RowModel {
    hunks: Vec<ByteHunk>,
    /// Row index at which each region begins, in the same order as `hunks`.
    starts: Vec<usize>,
    /// The section each region belongs to, or `None` when it matches.
    hunk_sections: Vec<Option<u32>>,
    bytes_per_row: u32,
    rows: usize,
    sections: Vec<Range<usize>>,
    counts: Counts,
    left_len: u64,
    right_len: u64,
}

impl Default for RowModel {
    fn default() -> Self {
        Self::build(Vec::new(), DEFAULT_BYTES_PER_ROW)
    }
}

/// Rows a region of `bytes` bytes takes at `bytes_per_row` bytes each.
fn rows_for(bytes: u64, bytes_per_row: u32) -> usize {
    if bytes == 0 {
        return 0;
    }
    let per_row = u64::from(bytes_per_row.max(MIN_BYTES_PER_ROW));
    let rows = bytes.div_ceil(per_row);
    usize::try_from(rows).unwrap_or(usize::MAX)
}

fn span_len(range: &Range<u64>) -> u64 {
    range.end.saturating_sub(range.start)
}

impl RowModel {
    /// Lay out the aligned regions at `bytes_per_row` bytes per row.
    #[must_use]
    pub fn build(hunks: Vec<ByteHunk>, bytes_per_row: u32) -> Self {
        let bytes_per_row = bytes_per_row.clamp(MIN_BYTES_PER_ROW, MAX_BYTES_PER_ROW);
        let mut model = Self {
            hunks,
            starts: Vec::new(),
            hunk_sections: Vec::new(),
            bytes_per_row,
            rows: 0,
            sections: Vec::new(),
            counts: Counts::default(),
            left_len: 0,
            right_len: 0,
        };
        model.lay_out();
        model
    }

    /// Lay the same regions out again at a different row width.
    pub fn set_bytes_per_row(&mut self, bytes_per_row: u32) {
        let bytes_per_row = bytes_per_row.clamp(MIN_BYTES_PER_ROW, MAX_BYTES_PER_ROW);
        if bytes_per_row == self.bytes_per_row {
            return;
        }
        self.bytes_per_row = bytes_per_row;
        self.lay_out();
    }

    fn lay_out(&mut self) {
        self.starts = Vec::with_capacity(self.hunks.len());
        self.hunk_sections = Vec::with_capacity(self.hunks.len());
        self.sections.clear();
        let mut counts = Counts::default();
        let mut row = 0usize;
        // A run of neighbouring differing regions is one section, because the
        // engine splits a single edit into a changed region and an orphan tail.
        let mut open: Option<Range<usize>> = None;
        for hunk in &self.hunks {
            let left = span_len(&hunk.left);
            let right = span_len(&hunk.right);
            let taken = rows_for(left.max(right), self.bytes_per_row);
            self.starts.push(row);
            if hunk.kind == HunkKind::Same {
                self.hunk_sections.push(None);
                if let Some(range) = open.take() {
                    self.sections.push(range);
                }
            } else {
                // The open section has not been pushed yet, so its index is the
                // one the next push will take.
                #[allow(clippy::cast_possible_truncation)]
                let index = self.sections.len() as u32;
                self.hunk_sections.push(Some(index));
                counts.difference_rows = counts.difference_rows.saturating_add(taken);
                match hunk.kind {
                    HunkKind::LeftOnly => {
                        counts.left_only_bytes = counts.left_only_bytes.saturating_add(left);
                    }
                    HunkKind::RightOnly => {
                        counts.right_only_bytes = counts.right_only_bytes.saturating_add(right);
                    }
                    HunkKind::Changed | HunkKind::Same => {
                        let paired = left.min(right);
                        counts.changed_bytes = counts.changed_bytes.saturating_add(paired);
                        counts.left_only_bytes =
                            counts.left_only_bytes.saturating_add(left - paired);
                        counts.right_only_bytes =
                            counts.right_only_bytes.saturating_add(right - paired);
                    }
                }
                open = Some(match open {
                    Some(range) => range.start..row + taken,
                    None => row..row + taken,
                });
            }
            row = row.saturating_add(taken);
            self.left_len = self.left_len.max(hunk.left.end);
            self.right_len = self.right_len.max(hunk.right.end);
        }
        if let Some(range) = open.take() {
            self.sections.push(range);
        }
        self.rows = row;
        counts.rows = row;
        counts.sections = self.sections.len();
        self.counts = counts;
    }

    /// The aligned regions behind the layout.
    #[must_use]
    pub fn hunks(&self) -> &[ByteHunk] {
        &self.hunks
    }

    /// Bytes each row shows.
    #[must_use]
    pub const fn bytes_per_row(&self) -> u32 {
        self.bytes_per_row
    }

    /// Number of rows.
    #[must_use]
    pub const fn row_count(&self) -> usize {
        self.rows
    }

    /// Length of one side, in bytes.
    #[must_use]
    pub const fn side_len(&self, side: Side) -> u64 {
        match side {
            Side::Left => self.left_len,
            Side::Right => self.right_len,
        }
    }

    /// The difference sections, as ranges over row indexes.
    #[must_use]
    pub fn sections(&self) -> &[Range<usize>] {
        &self.sections
    }

    /// The comparison totals.
    #[must_use]
    pub const fn counts(&self) -> Counts {
        self.counts
    }

    /// The index of the region a row falls in.
    fn hunk_of_row(&self, row: usize) -> Option<usize> {
        if row >= self.rows {
            return None;
        }
        let found = self.starts.partition_point(|start| *start <= row);
        found.checked_sub(1)
    }

    /// One row, computed from the region it falls in.
    #[must_use]
    pub fn row(&self, index: usize) -> Option<HexRow> {
        let position = self.hunk_of_row(index)?;
        let hunk = self.hunks.get(position)?;
        let start = *self.starts.get(position)?;
        let offset = u64::try_from(index - start).unwrap_or(0) * u64::from(self.bytes_per_row);
        let left = side_span(&hunk.left, offset, self.bytes_per_row);
        let right = side_span(&hunk.right, offset, self.bytes_per_row);
        let class = match (left, right, hunk.kind) {
            (None, None, _) => return None,
            (Some(_), None, _) | (Some(_), Some(_), HunkKind::LeftOnly) => RowClass::LeftOnly,
            (None, Some(_), _) | (Some(_), Some(_), HunkKind::RightOnly) => RowClass::RightOnly,
            (Some(_), Some(_), HunkKind::Same) => RowClass::Same,
            (Some(_), Some(_), HunkKind::Changed) => RowClass::Changed,
        };
        Some(HexRow {
            class,
            left,
            right,
            section: self.hunk_sections.get(position).copied().flatten(),
        })
    }

    /// The row showing a byte of one side.
    ///
    /// An offset inside a region the side has no bytes in resolves to the row
    /// the region starts at, which is where the caret belongs.
    #[must_use]
    pub fn row_of_offset(&self, side: Side, offset: u64) -> Option<usize> {
        if self.hunks.is_empty() {
            return None;
        }
        let end = |hunk: &ByteHunk| match side {
            Side::Left => hunk.left.end,
            Side::Right => hunk.right.end,
        };
        let start = |hunk: &ByteHunk| match side {
            Side::Left => hunk.left.start,
            Side::Right => hunk.right.start,
        };
        let position = self.hunks.partition_point(|hunk| end(hunk) <= offset);
        let position = position.min(self.hunks.len().saturating_sub(1));
        let hunk = self.hunks.get(position)?;
        let row_start = *self.starts.get(position)?;
        let inside = offset.saturating_sub(start(hunk));
        let step = inside / u64::from(self.bytes_per_row);
        let row = row_start.saturating_add(usize::try_from(step).unwrap_or(0));
        Some(row.min(self.rows.saturating_sub(1)))
    }

    /// The offset of the byte in column `column` of one side of a row.
    #[must_use]
    pub fn offset_at(&self, row: usize, side: Side, column: u32) -> Option<u64> {
        let span = self.row(row)?.side(side)?;
        (column < span.len).then(|| span.start + u64::from(column))
    }

    /// The section a row belongs to.
    #[must_use]
    pub fn section_of(&self, row: usize) -> Option<u32> {
        self.row(row).and_then(|row| row.section)
    }

    /// The first differing row strictly after `from`.
    #[must_use]
    pub fn next_difference(&self, from: usize) -> Option<usize> {
        let wanted = from.saturating_add(1);
        for section in &self.sections {
            if section.end <= wanted {
                continue;
            }
            return Some(section.start.max(wanted));
        }
        None
    }

    /// The last differing row strictly before `from`.
    #[must_use]
    pub fn previous_difference(&self, from: usize) -> Option<usize> {
        for section in self.sections.iter().rev() {
            if section.start >= from {
                continue;
            }
            return Some(section.end.min(from).saturating_sub(1));
        }
        None
    }

    /// The first row of the next difference section.
    #[must_use]
    pub fn next_section(&self, from: usize) -> Option<usize> {
        self.sections
            .iter()
            .find(|range| range.start > from)
            .map(|range| range.start)
    }

    /// The first row of the previous difference section.
    #[must_use]
    pub fn previous_section(&self, from: usize) -> Option<usize> {
        self.sections
            .iter()
            .rev()
            .find(|range| range.start < from)
            .map(|range| range.start)
    }

    /// The rows a display filter shows.
    #[must_use]
    pub fn visible(&self, filter: DisplayFilter) -> Visible {
        match filter {
            DisplayFilter::All => Visible::All(self.rows),
            DisplayFilter::Differences => Visible::ranges(self.sections.clone()),
            DisplayFilter::Same => Visible::ranges(self.complement()),
            DisplayFilter::Context(rows) => Visible::ranges(self.padded(rows as usize)),
        }
    }

    /// The rows no difference section covers.
    fn complement(&self) -> Vec<Range<usize>> {
        let mut out = Vec::with_capacity(self.sections.len() + 1);
        let mut cursor = 0usize;
        for section in &self.sections {
            if section.start > cursor {
                out.push(cursor..section.start);
            }
            cursor = cursor.max(section.end);
        }
        if cursor < self.rows {
            out.push(cursor..self.rows);
        }
        out
    }

    /// The difference sections widened by `reach` rows and merged where the
    /// widening made two of them touch.
    fn padded(&self, reach: usize) -> Vec<Range<usize>> {
        let mut out: Vec<Range<usize>> = Vec::with_capacity(self.sections.len());
        for section in &self.sections {
            let start = section.start.saturating_sub(reach);
            let end = section.end.saturating_add(reach).min(self.rows);
            match out.last_mut() {
                Some(last) if last.end >= start => last.end = last.end.max(end),
                _ => out.push(start..end),
            }
        }
        out
    }
}

/// The bytes one side shows on the row starting `offset` bytes into a region.
fn side_span(range: &Range<u64>, offset: u64, bytes_per_row: u32) -> Option<RowSpan> {
    let len = span_len(range);
    if offset >= len {
        return None;
    }
    let remaining = len - offset;
    let taken = remaining.min(u64::from(bytes_per_row));
    Some(RowSpan {
        start: range.start.saturating_add(offset),
        len: u32::try_from(taken).unwrap_or(bytes_per_row),
    })
}

/// Which rows the view shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DisplayFilter {
    /// Every row.
    #[default]
    All,
    /// Only rows that differ.
    Differences,
    /// Only rows that match.
    Same,
    /// Rows that differ plus this many matching rows around each section.
    Context(u32),
}

impl DisplayFilter {
    /// The label a menu or a drop down shows.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Differences => "Differences",
            Self::Same => "Same",
            Self::Context(_) => "Context",
        }
    }
}

/// The result of applying a display filter.
///
/// The unfiltered case carries only a length, and a filtered one carries the
/// runs of rows it keeps rather than the rows themselves, so a filter over tens
/// of millions of rows costs the number of difference sections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Visible {
    /// Every row, in order.
    All(usize),
    /// The rows the listed runs cover, in order.
    Runs(Runs),
}

/// Ordered, disjoint runs of rows with the running total in front of each.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Runs {
    ranges: Vec<Range<usize>>,
    before: Vec<usize>,
    total: usize,
}

impl Runs {
    fn new(ranges: Vec<Range<usize>>) -> Self {
        let mut before = Vec::with_capacity(ranges.len());
        let mut total = 0usize;
        for range in &ranges {
            before.push(total);
            total = total.saturating_add(range.end.saturating_sub(range.start));
        }
        Self {
            ranges,
            before,
            total,
        }
    }
}

impl Visible {
    fn ranges(ranges: Vec<Range<usize>>) -> Self {
        Self::Runs(Runs::new(ranges))
    }

    /// How many rows are shown.
    #[must_use]
    pub const fn len(&self) -> usize {
        match self {
            Self::All(count) => *count,
            Self::Runs(runs) => runs.total,
        }
    }

    /// True when nothing is shown.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The model row shown at a display position.
    #[must_use]
    pub fn row_at(&self, position: usize) -> Option<usize> {
        match self {
            Self::All(count) => (position < *count).then_some(position),
            Self::Runs(runs) => {
                if position >= runs.total {
                    return None;
                }
                let index = runs.before.partition_point(|start| *start <= position);
                let index = index.checked_sub(1)?;
                let range = runs.ranges.get(index)?;
                let inside = position - runs.before.get(index)?;
                Some(range.start + inside)
            }
        }
    }

    /// The display position of a model row, or the position of the first row
    /// after it when the row itself is filtered out.
    #[must_use]
    pub fn position_of(&self, row: usize) -> Option<usize> {
        match self {
            Self::All(count) => (row < *count).then_some(row),
            Self::Runs(runs) => {
                let index = runs.ranges.partition_point(|range| range.end <= row);
                let range = runs.ranges.get(index)?;
                let before = *runs.before.get(index)?;
                if row <= range.start {
                    return Some(before);
                }
                Some(before + (row - range.start))
            }
        }
    }
}

impl ca_ui::thumbnail::Severity for RowClass {
    fn rank(self) -> u8 {
        match self {
            RowClass::Same => 0,
            RowClass::LeftOnly | RowClass::RightOnly => 1,
            RowClass::Changed => 2,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{
        DisplayFilter, RowClass, RowModel, RowSpan, Side, Visible, DEFAULT_BYTES_PER_ROW,
        MAX_BYTES_PER_ROW,
    };
    use ca_diff::{ByteHunk, HunkKind};
    use std::ops::Range;
    use std::time::Instant;

    fn hunk(kind: HunkKind, left: Range<u64>, right: Range<u64>) -> ByteHunk {
        ByteHunk { kind, left, right }
    }

    fn rows_of(model: &RowModel) -> Vec<(RowClass, Option<RowSpan>, Option<RowSpan>)> {
        (0..model.row_count())
            .filter_map(|index| model.row(index))
            .map(|row| (row.class, row.left, row.right))
            .collect()
    }

    #[test]
    fn matching_bytes_sit_level_and_fill_whole_rows() {
        let model = RowModel::build(vec![hunk(HunkKind::Same, 0..40, 0..40)], 16);
        assert_eq!(model.row_count(), 3);
        let rows = rows_of(&model);
        assert_eq!(rows[0].1, Some(RowSpan { start: 0, len: 16 }));
        assert_eq!(rows[2].1, Some(RowSpan { start: 32, len: 8 }));
        assert_eq!(rows[2].2, Some(RowSpan { start: 32, len: 8 }));
        assert!(rows.iter().all(|row| row.0 == RowClass::Same));
        assert_eq!(model.counts().difference_rows, 0);
    }

    /// An insertion shifts one side. Every mode has to keep the bytes after it
    /// level, which is what the gap rows are for.
    #[test]
    fn an_insertion_gives_the_short_side_gap_rows() {
        let model = RowModel::build(
            vec![
                hunk(HunkKind::Same, 0..16, 0..16),
                hunk(HunkKind::RightOnly, 16..16, 16..40),
                hunk(HunkKind::Same, 16..32, 40..56),
            ],
            16,
        );
        let rows = rows_of(&model);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[1].0, RowClass::RightOnly);
        assert_eq!(rows[1].1, None);
        assert_eq!(rows[2].1, None);
        assert_eq!(rows[2].2, Some(RowSpan { start: 32, len: 8 }));
        // The matching region after the insertion starts a fresh row on both
        // sides, so its first bytes are level.
        assert_eq!(rows[3].1, Some(RowSpan { start: 16, len: 16 }));
        assert_eq!(rows[3].2, Some(RowSpan { start: 40, len: 16 }));
    }

    #[test]
    fn a_region_always_starts_a_new_row() {
        let model = RowModel::build(
            vec![
                hunk(HunkKind::Same, 0..3, 0..3),
                hunk(HunkKind::Changed, 3..4, 3..4),
                hunk(HunkKind::Same, 4..20, 4..20),
            ],
            16,
        );
        let rows = rows_of(&model);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].1, Some(RowSpan { start: 0, len: 3 }));
        assert_eq!(rows[1].1, Some(RowSpan { start: 3, len: 1 }));
        assert_eq!(rows[2].1, Some(RowSpan { start: 4, len: 16 }));
    }

    #[test]
    fn a_changed_region_of_uneven_length_ends_in_one_sided_rows() {
        let model = RowModel::build(vec![hunk(HunkKind::Changed, 0..4, 0..20)], 8);
        let rows = rows_of(&model);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].0, RowClass::Changed);
        assert_eq!(rows[1].0, RowClass::RightOnly);
        assert_eq!(rows[1].1, None);
        assert_eq!(rows[2].2, Some(RowSpan { start: 16, len: 4 }));
        assert_eq!(model.sections().len(), 1);
        assert_eq!(model.sections().first(), Some(&(0..3)));
    }

    #[test]
    fn a_truncated_tail_is_one_left_only_section() {
        let model = RowModel::build(
            vec![
                hunk(HunkKind::Same, 0..16, 0..16),
                hunk(HunkKind::LeftOnly, 16..48, 16..16),
            ],
            16,
        );
        assert_eq!(model.row_count(), 3);
        assert_eq!(model.sections().first(), Some(&(1..3)));
        assert_eq!(model.counts().left_only_bytes, 32);
        assert_eq!(model.counts().right_only_bytes, 0);
    }

    #[test]
    fn neighbouring_differing_regions_form_one_section() {
        let model = RowModel::build(
            vec![
                hunk(HunkKind::Same, 0..16, 0..16),
                hunk(HunkKind::Changed, 16..32, 16..32),
                hunk(HunkKind::LeftOnly, 32..48, 32..32),
                hunk(HunkKind::Same, 48..64, 32..48),
            ],
            16,
        );
        assert_eq!(model.sections().first(), Some(&(1..3)));
        assert_eq!(model.counts().sections, 1);
    }

    #[test]
    fn totals_split_changed_bytes_from_orphans() {
        let model = RowModel::build(
            vec![
                hunk(HunkKind::Same, 0..8, 0..8),
                hunk(HunkKind::Changed, 8..12, 8..20),
                hunk(HunkKind::LeftOnly, 12..15, 20..20),
            ],
            16,
        );
        let counts = model.counts();
        assert_eq!(counts.changed_bytes, 4);
        assert_eq!(counts.right_only_bytes, 8);
        assert_eq!(counts.left_only_bytes, 3);
        assert_eq!(counts.difference_bytes(), 15);
    }

    #[test]
    fn offsets_map_to_rows_and_back_across_a_gap() {
        let model = RowModel::build(
            vec![
                hunk(HunkKind::Same, 0..16, 0..16),
                hunk(HunkKind::RightOnly, 16..16, 16..48),
                hunk(HunkKind::Same, 16..32, 48..64),
            ],
            16,
        );
        assert_eq!(model.row_of_offset(Side::Left, 0), Some(0));
        assert_eq!(model.row_of_offset(Side::Left, 20), Some(3));
        assert_eq!(model.row_of_offset(Side::Right, 40), Some(2));
        assert_eq!(model.offset_at(3, Side::Left, 4), Some(20));
        assert_eq!(model.offset_at(3, Side::Right, 4), Some(52));
        // The left side has no bytes in the inserted region, so an offset there
        // resolves through the region that does hold it.
        assert_eq!(model.offset_at(1, Side::Left, 0), None);
        assert_eq!(model.offset_at(0, Side::Left, 99), None);
    }

    #[test]
    fn navigation_steps_by_row_and_by_section() {
        let model = RowModel::build(
            vec![
                hunk(HunkKind::Same, 0..16, 0..16),
                hunk(HunkKind::Changed, 16..48, 16..48),
                hunk(HunkKind::Same, 48..64, 48..64),
                hunk(HunkKind::Changed, 64..80, 64..80),
            ],
            16,
        );
        assert_eq!(model.sections(), &[1..3, 4..5]);
        assert_eq!(model.next_difference(0), Some(1));
        assert_eq!(model.next_difference(1), Some(2));
        assert_eq!(model.next_difference(2), Some(4));
        assert_eq!(model.next_difference(4), None);
        assert_eq!(model.previous_difference(4), Some(2));
        assert_eq!(model.previous_difference(2), Some(1));
        assert_eq!(model.previous_difference(0), None);
        assert_eq!(model.next_section(0), Some(1));
        assert_eq!(model.next_section(1), Some(4));
        assert_eq!(model.previous_section(4), Some(1));
        assert_eq!(model.previous_section(0), None);
    }

    fn filter_model() -> RowModel {
        RowModel::build(
            vec![
                hunk(HunkKind::Same, 0..80, 0..80),
                hunk(HunkKind::Changed, 80..96, 80..96),
                hunk(HunkKind::Same, 96..176, 96..176),
                hunk(HunkKind::Changed, 176..192, 176..192),
                hunk(HunkKind::Same, 192..272, 192..272),
            ],
            16,
        )
    }

    fn shown(model: &RowModel, filter: DisplayFilter) -> Vec<usize> {
        let visible = model.visible(filter);
        (0..visible.len())
            .filter_map(|position| visible.row_at(position))
            .collect()
    }

    #[test]
    fn the_unfiltered_view_allocates_nothing() {
        let model = filter_model();
        assert_eq!(model.visible(DisplayFilter::All), Visible::All(17));
    }

    #[test]
    fn the_difference_filter_keeps_only_differing_rows() {
        let model = filter_model();
        assert_eq!(shown(&model, DisplayFilter::Differences), vec![5, 11]);
    }

    #[test]
    fn the_same_filter_is_the_complement_of_the_difference_filter() {
        let model = filter_model();
        let differences = shown(&model, DisplayFilter::Differences);
        let same = shown(&model, DisplayFilter::Same);
        assert_eq!(same.len() + differences.len(), model.row_count());
        assert!(same.iter().all(|row| !differences.contains(row)));
        assert_eq!(same.first(), Some(&0));
        assert_eq!(same.last(), Some(&16));
    }

    #[test]
    fn the_context_filter_keeps_neighbours_without_repeating_rows() {
        let model = filter_model();
        let rows = shown(&model, DisplayFilter::Context(2));
        assert_eq!(rows, vec![3, 4, 5, 6, 7, 9, 10, 11, 12, 13]);
        let mut deduped = rows.clone();
        deduped.dedup();
        assert_eq!(deduped, rows);
    }

    #[test]
    fn widening_two_close_sections_merges_them() {
        let model = filter_model();
        let rows = shown(&model, DisplayFilter::Context(6));
        assert_eq!(rows, (0..17).collect::<Vec<usize>>());
    }

    #[test]
    fn a_filtered_position_lands_on_the_next_visible_row() {
        let model = filter_model();
        let visible = model.visible(DisplayFilter::Differences);
        assert_eq!(visible.position_of(5), Some(0));
        assert_eq!(visible.position_of(0), Some(0));
        assert_eq!(visible.position_of(11), Some(1));
        assert_eq!(visible.position_of(16), None);
        assert_eq!(visible.row_at(2), None);
    }

    #[test]
    fn changing_the_row_width_relays_the_same_regions() {
        let mut model = RowModel::build(vec![hunk(HunkKind::Same, 0..64, 0..64)], 16);
        assert_eq!(model.row_count(), 4);
        model.set_bytes_per_row(8);
        assert_eq!(model.row_count(), 8);
        assert_eq!(
            model.row(7).unwrap().left,
            Some(RowSpan { start: 56, len: 8 })
        );
        model.set_bytes_per_row(MAX_BYTES_PER_ROW);
        assert_eq!(model.row_count(), 1);
        model.set_bytes_per_row(0);
        assert_eq!(model.bytes_per_row(), 1);
        assert_eq!(model.row_count(), 64);
    }

    #[test]
    fn an_empty_comparison_has_no_rows() {
        let model = RowModel::default();
        assert_eq!(model.row_count(), 0);
        assert_eq!(model.row(0), None);
        assert_eq!(model.row_of_offset(Side::Left, 0), None);
        assert_eq!(model.next_difference(0), None);
        assert!(model.visible(DisplayFilter::Differences).is_empty());
    }

    #[test]
    fn an_empty_side_is_one_orphan_region() {
        let model = RowModel::build(vec![hunk(HunkKind::LeftOnly, 0..24, 0..0)], 16);
        assert_eq!(model.row_count(), 2);
        assert_eq!(model.row(0).unwrap().right, None);
        assert_eq!(model.side_len(Side::Right), 0);
    }

    /// Five hundred megabytes a side reaches tens of millions of rows. Nothing
    /// in the layout may be proportional to that, so the build and every query
    /// have to stay proportional to the number of regions.
    #[test]
    fn a_five_hundred_megabyte_pair_lays_out_without_materializing_rows() {
        const SIZE: u64 = 500 * 1024 * 1024;
        let mut hunks = Vec::new();
        let mut at = 0u64;
        while at + 1_024 < SIZE {
            hunks.push(hunk(HunkKind::Same, at..at + 1_020, at..at + 1_020));
            at += 1_020;
            hunks.push(hunk(HunkKind::Changed, at..at + 4, at..at + 4));
            at += 4;
        }
        hunks.push(hunk(HunkKind::Same, at..SIZE, at..SIZE));
        let started = Instant::now();
        let model = RowModel::build(hunks, DEFAULT_BYTES_PER_ROW);
        assert!(model.row_count() > 30_000_000, "{}", model.row_count());
        let last = model.row_count() - 1;
        assert!(model.row(last).is_some());
        assert!(model.row(model.row_count()).is_none());
        assert!(model.next_difference(last / 2).is_some());
        assert!(model.row_of_offset(Side::Left, SIZE - 1).is_some());
        let visible = model.visible(DisplayFilter::Differences);
        assert!(visible.row_at(visible.len() - 1).is_some());
        assert!(
            started.elapsed().as_secs() < 30,
            "the layout took {:?}",
            started.elapsed()
        );
    }
}
