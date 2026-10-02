//! The row layout of a text comparison: the mapping between the two files'
//! line numbers and the visual rows they are painted on.
//!
//! A row holds at most one line from each side. Where one side has lines the
//! other does not, the missing side is `None` and the row is painted as a gap,
//! which is what keeps counterpart lines level with each other.
//!
//! Everything here is free of painting so the alignment, the filters and the
//! navigation can be tested directly.

use ca_diff::{ClassifiedHunk, HunkKind, Importance};
use ca_ui::filter;
pub use ca_ui::filter::Visible;
use std::ops::Range;

/// What a row shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowClass {
    /// Both sides carry the same line.
    Same,
    /// Both sides carry a line and the two differ.
    Changed,
    /// Only the left side carries a line.
    LeftOnly,
    /// Only the right side carries a line.
    RightOnly,
}

impl RowClass {
    /// True for every class that counts as a difference.
    #[must_use]
    pub const fn is_difference(self) -> bool {
        !matches!(self, Self::Same)
    }
}

/// One visual row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    /// What the row shows.
    pub class: RowClass,
    /// How the row's difference is classified. `None` on a matching row.
    pub importance: Option<Importance>,
    /// Zero based line number on the left, or `None` for a gap.
    pub left: Option<u32>,
    /// Zero based line number on the right, or `None` for a gap.
    pub right: Option<u32>,
    /// Index into [`RowModel::sections`], or `None` on a matching row.
    pub section: Option<u32>,
}

/// How many differences a comparison found.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    /// Rows that differ, of any class.
    pub differences: usize,
    /// Contiguous runs of differing rows.
    pub sections: usize,
    /// Rows whose difference is classified important.
    pub important: usize,
    /// Rows whose difference is classified unimportant.
    pub unimportant: usize,
    /// Unimportant changed rows omitted because the user chose to ignore them.
    pub ignored_unimportant: usize,
}

/// What a row is reported as on screen and in the status bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineStatus {
    /// The two sides carry the same line.
    Same,
    /// The two sides differ and the difference is important.
    Important,
    /// The two sides differ and every difference is unimportant.
    Unimportant,
    /// The left side has a line the right side has not.
    LeftOrphan,
    /// The right side has a line the left side has not.
    RightOrphan,
}

impl LineStatus {
    /// The wording the status bar shows.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Same => "Same",
            Self::Important => "Important difference",
            Self::Unimportant => "Unimportant difference",
            Self::LeftOrphan => "Left orphan",
            Self::RightOrphan => "Right orphan",
        }
    }
}

/// How a row reads, given whether unimportant differences are ignored.
///
/// Pure, so the classification the status bar reports is testable without a
/// frame.
#[must_use]
pub const fn line_status(row: &Row, ignore_unimportant: bool) -> LineStatus {
    match row.class {
        RowClass::Same => LineStatus::Same,
        RowClass::LeftOnly => LineStatus::LeftOrphan,
        RowClass::RightOnly => LineStatus::RightOrphan,
        RowClass::Changed => match row.importance {
            Some(Importance::Unimportant) => {
                if ignore_unimportant {
                    LineStatus::Same
                } else {
                    LineStatus::Unimportant
                }
            }
            _ => LineStatus::Important,
        },
    }
}

/// The rows of one comparison plus the indexes navigation needs.
#[derive(Debug, Clone, Default)]
pub struct RowModel {
    rows: Vec<Row>,
    sections: Vec<Range<u32>>,
    left_to_row: Vec<u32>,
    right_to_row: Vec<u32>,
    counts: Counts,
    /// Which hunk each row came out of, so two hunks that touch stay two
    /// difference sections. Matching rows carry [`u32::MAX`].
    groups: Vec<u32>,
    next_group: u32,
    ignore_unimportant: bool,
}

/// Build the rows for a classified comparison.
///
/// Within a changed hunk the two sides are paired positionally, which is what
/// puts the first changed line of the left beside the first changed line of the
/// right; the longer side's tail becomes one sided rows inside the same
/// difference section.
#[must_use]
pub fn build(hunks: &[ClassifiedHunk]) -> RowModel {
    let mut model = RowModel::default();
    for classified in hunks {
        match classified.hunk.kind {
            HunkKind::Same => model.push_same(&classified.hunk.left, &classified.hunk.right),
            HunkKind::Changed | HunkKind::LeftOnly | HunkKind::RightOnly => {
                model.push_difference(classified);
            }
        }
    }
    model.finish();
    model
}

impl RowModel {
    fn push_same(&mut self, left: &Range<u32>, right: &Range<u32>) {
        // A matching hunk covers the same number of lines on both sides, but a
        // malformed one must not shift every later row, so the pairing is
        // bounded by the shorter side and any excess falls through as gaps.
        let paired = left.len().min(right.len());
        for offset in 0..paired {
            #[allow(clippy::cast_possible_truncation)]
            let offset = offset as u32;
            self.push(
                Row {
                    class: RowClass::Same,
                    importance: None,
                    left: Some(left.start + offset),
                    right: Some(right.start + offset),
                    section: None,
                },
                u32::MAX,
            );
        }
    }

    fn push_difference(&mut self, classified: &ClassifiedHunk) {
        let group = self.next_group;
        self.next_group = self.next_group.saturating_add(1);
        let left = &classified.hunk.left;
        let right = &classified.hunk.right;
        let rows = left.len_u32().max(right.len_u32());
        for offset in 0..rows {
            let left_line = (offset < left.len_u32()).then(|| left.start + offset);
            let right_line = (offset < right.len_u32()).then(|| right.start + offset);
            let class = match (left_line, right_line) {
                (Some(_), Some(_)) => RowClass::Changed,
                (Some(_), None) => RowClass::LeftOnly,
                (None, Some(_)) => RowClass::RightOnly,
                (None, None) => continue,
            };
            let importance = row_importance(classified, offset);
            self.push(
                Row {
                    class,
                    importance: Some(importance),
                    left: left_line,
                    right: right_line,
                    section: None,
                },
                group,
            );
        }
    }

    fn push(&mut self, row: Row, group: u32) {
        #[allow(clippy::cast_possible_truncation)]
        let index = self.rows.len() as u32;
        self.groups.push(group);
        if let Some(line) = row.left {
            grow_to(&mut self.left_to_row, line, index);
        }
        if let Some(line) = row.right {
            grow_to(&mut self.right_to_row, line, index);
        }
        self.rows.push(row);
    }

    /// Number the difference sections and total the differences.
    ///
    /// A section is a run of adjacent counted rows out of one hunk, so the
    /// numbering follows whatever the current importance setting counts and two
    /// hunks that touch stay two sections. The scan is linear and runs when the
    /// model is built or the setting changes, never per frame.
    fn finish(&mut self) {
        let ignore = self.ignore_unimportant;
        self.sections.clear();
        let mut counts = Counts::default();
        let mut open: Option<u32> = None;
        let mut group = u32::MAX;
        for index in 0..self.rows.len() {
            #[allow(clippy::cast_possible_truncation)]
            let index = index as u32;
            let row = &self.rows[index as usize];
            let counted = counts_as_difference(row, ignore);
            if !counted
                && row.class == RowClass::Changed
                && matches!(row.importance, Some(Importance::Unimportant))
            {
                counts.ignored_unimportant += 1;
            }
            let here = self.groups.get(index as usize).copied().unwrap_or(u32::MAX);
            if here != group {
                if let Some(start) = open.take() {
                    self.sections.push(start..index);
                }
                group = here;
            }
            if counted {
                counts.differences += 1;
                match row.importance {
                    Some(Importance::Unimportant) => counts.unimportant += 1,
                    _ => counts.important += 1,
                }
                if open.is_none() {
                    open = Some(index);
                }
            } else if let Some(start) = open.take() {
                self.sections.push(start..index);
            }
            let section = if counted {
                #[allow(clippy::cast_possible_truncation)]
                Some(self.sections.len() as u32)
            } else {
                None
            };
            self.rows[index as usize].section = section;
        }
        if let Some(start) = open.take() {
            #[allow(clippy::cast_possible_truncation)]
            self.sections.push(start..self.rows.len() as u32);
        }
        counts.sections = self.sections.len();
        self.counts = counts;
    }

    /// Count unimportant differences as matching text, or stop doing so.
    ///
    /// Renumbering the sections is linear in the rows, so the call belongs to
    /// the toggle rather than to a frame.
    pub fn set_ignore_unimportant(&mut self, ignore: bool) {
        if self.ignore_unimportant == ignore {
            return;
        }
        self.ignore_unimportant = ignore;
        self.finish();
    }

    /// True when unimportant differences read as matching text.
    #[must_use]
    pub const fn ignores_unimportant(&self) -> bool {
        self.ignore_unimportant
    }

    /// True when the row counts as a difference under the current setting.
    #[must_use]
    pub const fn is_difference(&self, row: &Row) -> bool {
        counts_as_difference(row, self.ignore_unimportant)
    }

    /// How the row reads under the current setting.
    #[must_use]
    pub const fn status(&self, row: &Row) -> LineStatus {
        line_status(row, self.ignore_unimportant)
    }

    /// Every row, in display order.
    #[must_use]
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Number of rows.
    #[must_use]
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// One row.
    #[must_use]
    pub fn row(&self, index: usize) -> Option<&Row> {
        self.rows.get(index)
    }

    /// The difference sections, as ranges over row indexes.
    #[must_use]
    pub fn sections(&self) -> &[Range<u32>] {
        &self.sections
    }

    /// The difference totals.
    #[must_use]
    pub const fn counts(&self) -> Counts {
        self.counts
    }

    /// The row showing a given left line.
    #[must_use]
    pub fn row_of_left_line(&self, line: u32) -> Option<usize> {
        self.left_to_row.get(line as usize).map(|row| *row as usize)
    }

    /// The row showing a given right line.
    #[must_use]
    pub fn row_of_right_line(&self, line: u32) -> Option<usize> {
        self.right_to_row
            .get(line as usize)
            .map(|row| *row as usize)
    }

    /// The first differing row strictly after `from`.
    #[must_use]
    pub fn next_difference(&self, from: usize) -> Option<usize> {
        let start = from.saturating_add(1);
        self.rows
            .iter()
            .enumerate()
            .skip(start)
            .find(|(_, row)| self.is_difference(row))
            .map(|(index, _)| index)
    }

    /// The last differing row strictly before `from`.
    #[must_use]
    pub fn previous_difference(&self, from: usize) -> Option<usize> {
        self.rows
            .iter()
            .enumerate()
            .take(from.min(self.rows.len()))
            .rev()
            .find(|(_, row)| self.is_difference(row))
            .map(|(index, _)| index)
    }

    /// The first row of the next difference section after the one holding
    /// `from`, or after `from` when it sits on a matching row.
    #[must_use]
    pub fn next_section(&self, from: usize) -> Option<usize> {
        #[allow(clippy::cast_possible_truncation)]
        let from = from as u32;
        self.sections
            .iter()
            .find(|range| range.start > from)
            .map(|range| range.start as usize)
    }

    /// The first row of the previous difference section.
    #[must_use]
    pub fn previous_section(&self, from: usize) -> Option<usize> {
        #[allow(clippy::cast_possible_truncation)]
        let from = from as u32;
        self.sections
            .iter()
            .rev()
            .find(|range| range.start < from)
            .map(|range| range.start as usize)
    }

    /// The first differing row, where the comparison has one.
    #[must_use]
    pub fn first_difference(&self) -> Option<usize> {
        self.rows.iter().position(|row| self.is_difference(row))
    }

    /// The last differing row, where the comparison has one.
    #[must_use]
    pub fn last_difference(&self) -> Option<usize> {
        self.previous_difference(self.rows.len())
    }

    /// The first row of the first difference section.
    #[must_use]
    pub fn first_section(&self) -> Option<usize> {
        self.sections.first().map(|range| range.start as usize)
    }

    /// The first row of the last difference section.
    #[must_use]
    pub fn last_section(&self) -> Option<usize> {
        self.sections.last().map(|range| range.start as usize)
    }

    /// The section a row belongs to.
    #[must_use]
    pub fn section_of(&self, row: usize) -> Option<u32> {
        self.rows.get(row).and_then(|row| row.section)
    }

    /// The rows a display filter shows.
    #[must_use]
    pub fn visible(&self, filter: DisplayFilter) -> Visible {
        match filter {
            DisplayFilter::All => Visible::All(self.rows.len()),
            DisplayFilter::None => Visible::Subset(Vec::new()),
            DisplayFilter::Differences => {
                filter::select(self.rows.len(), |row| self.is_difference(&self.rows[row]))
            }
            DisplayFilter::Same => {
                filter::select(self.rows.len(), |row| !self.is_difference(&self.rows[row]))
            }
            DisplayFilter::Context(lines) => {
                let sections: Vec<Range<usize>> = self
                    .sections
                    .iter()
                    .map(|range| range.start as usize..range.end as usize)
                    .collect();
                filter::context(&sections, lines as usize, self.rows.len())
            }
        }
    }
}

/// True when a row counts as a difference.
const fn counts_as_difference(row: &Row, ignore_unimportant: bool) -> bool {
    match row.class {
        RowClass::Same => false,
        RowClass::Changed => {
            !(ignore_unimportant && matches!(row.importance, Some(Importance::Unimportant)))
        }
        RowClass::LeftOnly | RowClass::RightOnly => true,
    }
}

fn row_importance(classified: &ClassifiedHunk, offset: u32) -> Importance {
    let left = classified.left_lines.get(offset as usize).copied();
    let right = classified.right_lines.get(offset as usize).copied();
    let combined = match (left, right) {
        (Some(Importance::Important), _) | (_, Some(Importance::Important)) => {
            Some(Importance::Important)
        }
        (left, right) => left.or(right),
    };
    combined
        .or(classified.importance)
        .unwrap_or(Importance::Important)
}

fn grow_to(index: &mut Vec<u32>, line: u32, row: u32) {
    let wanted = line as usize;
    if index.len() <= wanted {
        index.resize(wanted + 1, row);
    }
    index[wanted] = row;
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
    /// No rows at all.
    None,
}

trait RangeLen {
    fn len_u32(&self) -> u32;
}

impl RangeLen for Range<u32> {
    fn len_u32(&self) -> u32 {
        self.end.saturating_sub(self.start)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{build, line_status, DisplayFilter, LineStatus, RowClass, Visible};
    use ca_diff::{ClassifiedHunk, Hunk, HunkKind, Importance};
    use std::ops::Range;
    use std::time::Instant;

    fn hunk(kind: HunkKind, left: Range<u32>, right: Range<u32>) -> ClassifiedHunk {
        let importance = (kind != HunkKind::Same).then_some(Importance::Important);
        ClassifiedHunk {
            left_lines: vec![Importance::Important; left.len()],
            right_lines: vec![Importance::Important; right.len()],
            hunk: Hunk { kind, left, right },
            importance,
        }
    }

    #[test]
    fn matching_lines_sit_level() {
        let model = build(&[hunk(HunkKind::Same, 0..3, 0..3)]);
        assert_eq!(model.row_count(), 3);
        for (index, row) in model.rows().iter().enumerate() {
            #[allow(clippy::cast_possible_truncation)]
            let index = index as u32;
            assert_eq!(row.left, Some(index));
            assert_eq!(row.right, Some(index));
            assert_eq!(row.class, RowClass::Same);
        }
    }

    #[test]
    fn an_uneven_change_pads_the_short_side_with_gaps() {
        let model = build(&[hunk(HunkKind::Changed, 0..1, 0..3)]);
        assert_eq!(model.row_count(), 3);
        assert_eq!(model.rows()[0].class, RowClass::Changed);
        assert_eq!(model.rows()[1].class, RowClass::RightOnly);
        assert_eq!(model.rows()[1].left, None);
        assert_eq!(model.rows()[2].left, None);
        assert_eq!(model.sections().len(), 1);
        assert_eq!(model.sections()[0], 0..3);
    }

    #[test]
    fn an_orphan_run_keeps_the_other_side_in_place() {
        let model = build(&[
            hunk(HunkKind::Same, 0..1, 0..1),
            hunk(HunkKind::LeftOnly, 1..3, 1..1),
            hunk(HunkKind::Same, 3..4, 1..2),
        ]);
        assert_eq!(model.row_count(), 4);
        assert_eq!(model.rows()[1].class, RowClass::LeftOnly);
        assert_eq!(model.rows()[2].right, None);
        assert_eq!(model.rows()[3].left, Some(3));
        assert_eq!(model.rows()[3].right, Some(1));
    }

    #[test]
    fn line_numbers_map_back_to_rows() {
        let model = build(&[
            hunk(HunkKind::Same, 0..2, 0..2),
            hunk(HunkKind::LeftOnly, 2..4, 2..2),
            hunk(HunkKind::Same, 4..5, 2..3),
        ]);
        assert_eq!(model.row_of_left_line(4), Some(4));
        assert_eq!(model.row_of_right_line(2), Some(4));
        assert_eq!(model.row_of_left_line(99), None);
    }

    #[test]
    fn navigation_steps_by_row_and_by_section() {
        let model = build(&[
            hunk(HunkKind::Same, 0..2, 0..2),
            hunk(HunkKind::Changed, 2..4, 2..4),
            hunk(HunkKind::Same, 4..6, 4..6),
            hunk(HunkKind::Changed, 6..7, 6..7),
        ]);
        assert_eq!(model.next_difference(0), Some(2));
        assert_eq!(model.next_difference(2), Some(3));
        assert_eq!(model.next_difference(6), None);
        assert_eq!(model.previous_difference(6), Some(3));
        assert_eq!(model.previous_difference(0), None);
        assert_eq!(model.next_section(0), Some(2));
        assert_eq!(model.next_section(2), Some(6));
        assert_eq!(model.next_section(6), None);
        assert_eq!(model.previous_section(6), Some(2));
        assert_eq!(model.previous_section(3), Some(2));
        assert_eq!(model.previous_section(0), None);
    }

    #[test]
    fn counts_split_important_from_unimportant() {
        let mut unimportant = hunk(HunkKind::Changed, 2..3, 2..3);
        unimportant.importance = Some(Importance::Unimportant);
        unimportant.left_lines = vec![Importance::Unimportant];
        unimportant.right_lines = vec![Importance::Unimportant];
        let model = build(&[
            hunk(HunkKind::Same, 0..2, 0..2),
            unimportant,
            hunk(HunkKind::Changed, 3..4, 3..4),
        ]);
        let counts = model.counts();
        assert_eq!(counts.differences, 2);
        assert_eq!(counts.sections, 2);
        assert_eq!(counts.important, 1);
        assert_eq!(counts.unimportant, 1);
    }

    /// A model with one case-only pair, one important pair and one orphan.
    fn mixed_model() -> super::RowModel {
        let mut unimportant = hunk(HunkKind::Changed, 2..3, 2..3);
        unimportant.importance = Some(Importance::Unimportant);
        unimportant.left_lines = vec![Importance::Unimportant];
        unimportant.right_lines = vec![Importance::Unimportant];
        build(&[
            hunk(HunkKind::Same, 0..2, 0..2),
            unimportant,
            hunk(HunkKind::Same, 3..4, 3..4),
            hunk(HunkKind::Changed, 4..5, 4..5),
            hunk(HunkKind::Same, 5..6, 5..6),
            hunk(HunkKind::LeftOnly, 6..7, 6..6),
        ])
    }

    #[test]
    fn every_row_reports_its_own_status() {
        let model = mixed_model();
        let status = |index: usize| model.status(model.row(index).unwrap());
        assert_eq!(status(0), LineStatus::Same);
        assert_eq!(status(2), LineStatus::Unimportant);
        assert_eq!(status(4), LineStatus::Important);
        assert_eq!(status(6), LineStatus::LeftOrphan);
        let right_only = build(&[hunk(HunkKind::RightOnly, 0..0, 0..1)]);
        assert_eq!(
            right_only.status(right_only.row(0).unwrap()),
            LineStatus::RightOrphan
        );
    }

    #[test]
    fn ignoring_unimportant_differences_makes_them_read_as_same() {
        let unimportant = super::Row {
            class: RowClass::Changed,
            importance: Some(Importance::Unimportant),
            left: Some(0),
            right: Some(0),
            section: None,
        };
        assert_eq!(line_status(&unimportant, false), LineStatus::Unimportant);
        assert_eq!(line_status(&unimportant, true), LineStatus::Same);
    }

    #[test]
    fn ignoring_unimportant_differences_renumbers_the_sections() {
        let mut model = mixed_model();
        assert_eq!(model.counts().sections, 3);
        assert_eq!(model.row(2).unwrap().section, Some(0));
        assert_eq!(model.row(4).unwrap().section, Some(1));
        assert_eq!(model.next_difference(0), Some(2));

        model.set_ignore_unimportant(true);
        assert!(model.ignores_unimportant());
        assert_eq!(model.counts().sections, 2);
        assert_eq!(model.counts().differences, 2);
        assert_eq!(model.counts().unimportant, 0);
        assert_eq!(model.counts().ignored_unimportant, 1);
        assert_eq!(model.row(2).unwrap().section, None);
        assert_eq!(model.row(4).unwrap().section, Some(0));
        assert_eq!(model.row(6).unwrap().section, Some(1));
        // The ignored row is no longer a destination for difference navigation.
        assert_eq!(model.next_difference(0), Some(4));
        assert_eq!(model.next_section(0), Some(4));
        assert_eq!(model.visible(DisplayFilter::Differences).len(), 2);

        model.set_ignore_unimportant(false);
        assert_eq!(model.counts().sections, 3);
        assert_eq!(model.counts().ignored_unimportant, 0);
        assert_eq!(model.next_difference(0), Some(2));
    }

    #[test]
    fn an_important_line_outranks_an_unimportant_counterpart() {
        let mut mixed = hunk(HunkKind::Changed, 0..1, 0..1);
        mixed.left_lines = vec![Importance::Unimportant];
        mixed.right_lines = vec![Importance::Important];
        let model = build(&[mixed]);
        assert_eq!(model.rows()[0].importance, Some(Importance::Important));
    }

    #[test]
    fn the_unfiltered_view_allocates_nothing() {
        let model = build(&[hunk(HunkKind::Same, 0..10, 0..10)]);
        assert_eq!(model.visible(DisplayFilter::All), Visible::All(10));
    }

    #[test]
    fn filters_select_the_expected_rows() {
        let model = build(&[
            hunk(HunkKind::Same, 0..2, 0..2),
            hunk(HunkKind::Changed, 2..3, 2..3),
            hunk(HunkKind::Same, 3..6, 3..6),
        ]);
        let differences = model.visible(DisplayFilter::Differences);
        assert_eq!(differences.len(), 1);
        assert_eq!(differences.row_at(0), Some(2));
        let same = model.visible(DisplayFilter::Same);
        assert_eq!(same.len(), 5);
        assert!(model.visible(DisplayFilter::None).is_empty());
    }

    #[test]
    fn context_filters_keep_neighbours_without_repeating_rows() {
        let model = build(&[
            hunk(HunkKind::Same, 0..5, 0..5),
            hunk(HunkKind::Changed, 5..6, 5..6),
            hunk(HunkKind::Same, 6..8, 6..8),
            hunk(HunkKind::Changed, 8..9, 8..9),
            hunk(HunkKind::Same, 9..14, 9..14),
        ]);
        let visible = model.visible(DisplayFilter::Context(2));
        let rows: Vec<usize> = (0..visible.len())
            .filter_map(|position| visible.row_at(position))
            .collect();
        assert_eq!(rows, vec![3, 4, 5, 6, 7, 8, 9, 10]);
        let mut sorted = rows.clone();
        sorted.dedup();
        assert_eq!(sorted, rows);
    }

    #[test]
    fn a_filtered_position_lands_on_the_next_visible_row() {
        let model = build(&[
            hunk(HunkKind::Same, 0..3, 0..3),
            hunk(HunkKind::Changed, 3..4, 3..4),
        ]);
        let visible = model.visible(DisplayFilter::Differences);
        assert_eq!(visible.position_of(3), Some(0));
        assert_eq!(visible.position_of(0), Some(0));
        assert_eq!(visible.position_of(9), None);
    }

    /// A model of `sections` changed rows, each preceded by nine same rows.
    fn striped_hunks(sections: u32) -> Vec<ClassifiedHunk> {
        let mut hunks = Vec::new();
        let mut left = 0u32;
        let mut right = 0u32;
        for _ in 0..sections {
            hunks.push(hunk(HunkKind::Same, left..left + 9, right..right + 9));
            left += 9;
            right += 9;
            hunks.push(hunk(HunkKind::Changed, left..left + 1, right..right + 1));
            left += 1;
            right += 1;
        }
        hunks
    }

    /// The counts a striped model reports, whatever its size.
    fn check_striped(model: &super::RowModel, sections: u32) {
        let rows = sections as usize * 10;
        assert_eq!(model.row_count(), rows);
        assert_eq!(model.counts().sections, sections as usize);
        let visible = model.visible(DisplayFilter::Differences);
        assert_eq!(visible.len(), sections as usize);
        assert_eq!(model.next_section(0), Some(9));
        assert_eq!(model.row_of_left_line(sections * 10 - 1), Some(rows - 1));
    }

    #[test]
    fn a_striped_model_counts_its_rows_and_sections() {
        let model = build(&striped_hunks(1_000));
        check_striped(&model, 1_000);
    }

    #[test]
    #[cfg_attr(
        debug_assertions,
        ignore = "wall-clock budget holds for release builds"
    )]
    fn a_million_rows_build_and_filter_within_the_budget() {
        let hunks = striped_hunks(100_000);
        let started = Instant::now();
        let model = build(&hunks);
        check_striped(&model, 100_000);
        assert!(
            started.elapsed().as_secs() < 10,
            "one million rows took {:?}",
            started.elapsed()
        );
    }
}
