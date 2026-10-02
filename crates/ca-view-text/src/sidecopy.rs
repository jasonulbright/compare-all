//! Copying lines from one pane to the other.
//!
//! A copy is worked out as a plan before anything is edited: which line range
//! of the target is replaced, and with what text. The plan is a pure function
//! of the row model and the two line counts, so the exact result of every case
//! is testable, including the ones where the target side has no lines at all in
//! the covered rows and the copy becomes an insertion.
//!
//! Applying a plan is one edit, so one undo reverses the whole copy.

use crate::editor::Pane;
use crate::model::RowModel;
use ca_text::LineRange;
use std::ops::Range;

/// Which pane a copy reads from or writes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The left pane.
    Left,
    /// The right pane.
    Right,
}

impl Side {
    /// The other pane.
    #[must_use]
    pub const fn other(self) -> Self {
        match self {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
        }
    }

    /// The line number this side carries on a row.
    #[must_use]
    pub const fn line_of(self, row: &crate::model::Row) -> Option<u32> {
        match self {
            Side::Left => row.left,
            Side::Right => row.right,
        }
    }
}

/// What a copy will do to the target pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyPlan {
    /// The target lines the copy replaces. An empty range is an insertion.
    pub target_lines: LineRange,
    /// The source lines the copy reads, for a caller that wants to report them.
    pub source_lines: LineRange,
}

/// The rows of the difference section containing or following `row`.
#[must_use]
pub fn section_rows(model: &RowModel, row: usize) -> Option<Range<usize>> {
    let index = model.section_of(row)?;
    let span = model.sections().get(index as usize)?;
    Some(span.start as usize..span.end as usize)
}

/// Work out what copying `rows` from `from` to the other side would do.
///
/// Returns `None` when the rows carry no lines on either side, which is the
/// only case where a copy has nothing to say.
#[must_use]
pub fn plan(model: &RowModel, rows: Range<usize>, from: Side) -> Option<CopyPlan> {
    let to = from.other();
    let mut source: Option<(u32, u32)> = None;
    let mut target: Option<(u32, u32)> = None;
    for index in rows.clone() {
        let Some(row) = model.row(index) else {
            continue;
        };
        if let Some(line) = from.line_of(row) {
            source = Some(extend(source, line));
        }
        if let Some(line) = to.line_of(row) {
            target = Some(extend(target, line));
        }
    }
    let source_lines = match source {
        Some((first, last)) => LineRange::new(first, last.saturating_add(1)),
        None => LineRange::new(0, 0),
    };
    let target_lines = if let Some((first, last)) = target {
        LineRange::new(first, last.saturating_add(1))
    } else {
        let at = insertion_line(model, &rows, to);
        LineRange::new(at, at)
    };
    if source.is_none() && target.is_none() {
        return None;
    }
    Some(CopyPlan {
        target_lines,
        source_lines,
    })
}

fn extend(span: Option<(u32, u32)>, line: u32) -> (u32, u32) {
    match span {
        None => (line, line),
        Some((first, last)) => (first.min(line), last.max(line)),
    }
}

/// Where an insertion goes when the covered rows carry no target line.
///
/// The line after the nearest target line above the rows, or the nearest target
/// line below them, or the start of the file.
fn insertion_line(model: &RowModel, rows: &Range<usize>, to: Side) -> u32 {
    for index in (0..rows.start).rev() {
        if let Some(line) = model.row(index).and_then(|row| to.line_of(row)) {
            return line.saturating_add(1);
        }
    }
    for index in rows.end..model.row_count() {
        if let Some(line) = model.row(index).and_then(|row| to.line_of(row)) {
            return line;
        }
    }
    0
}

/// The text a plan writes into the target, with one terminator per line.
#[must_use]
pub fn copied_text(source: &Pane, plan: &CopyPlan, ending: &str) -> String {
    let mut out = String::new();
    for line in plan.source_lines.start..plan.source_lines.end {
        out.push_str(&source.line_text(line));
        out.push_str(ending);
    }
    out
}

/// Apply a plan to the target pane as one undo group.
///
/// A copy that lands past the last terminator of the target first closes that
/// line, so the copied text starts on a line of its own rather than joining the
/// last line of the file.
pub fn apply(target: &mut Pane, source: &Pane, plan: &CopyPlan, ending: &str) {
    if target.is_read_only() {
        return;
    }
    let mut text = copied_text(source, plan, ending);
    if needs_leading_break(target, plan) {
        text.insert_str(0, ending);
    }
    target.replace_lines(plan.target_lines, &text);
}

/// True when the target's last line has no terminator and the copy appends
/// after it.
fn needs_leading_break(target: &Pane, plan: &CopyPlan) -> bool {
    let lines = target.line_count();
    if plan.target_lines.start < lines.saturating_sub(1) || lines == 0 {
        return false;
    }
    let last = lines.saturating_sub(1);
    if plan.target_lines.start <= last && !plan.target_lines.is_empty() {
        return false;
    }
    let text = target.line_text(last);
    !text.is_empty() && plan.target_lines.start > last
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{apply, copied_text, plan, section_rows, CopyPlan, Side};
    use crate::editor::Pane;
    use crate::model::{self, RowModel};
    use ca_diff::{
        classify_hunks, diff_line_slices_cancellable, LineCompareOptions, RuleSet,
        WhitespaceClassifier,
    };
    use ca_text::LineRange;

    struct NoCancel;
    impl ca_diff::Cancel for NoCancel {
        fn is_cancelled(&self) -> bool {
            false
        }
    }

    fn model_of(left: &str, right: &str) -> RowModel {
        let left_lines = ca_diff::split_lines(left);
        let right_lines = ca_diff::split_lines(right);
        let hunks = diff_line_slices_cancellable(
            &left_lines,
            &right_lines,
            &LineCompareOptions::default(),
            &NoCancel,
        )
        .unwrap();
        let classified = classify_hunks(
            &left_lines,
            &right_lines,
            &hunks,
            &RuleSet::all_important(),
            &WhitespaceClassifier,
        )
        .unwrap();
        model::build(&classified)
    }

    fn copy_all(left: &str, right: &str, from: Side) -> (String, String) {
        let model = model_of(left, right);
        let mut left_pane = Pane::from_text(left);
        let mut right_pane = Pane::from_text(right);
        let rows = 0..model.row_count();
        let plan = plan(&model, rows, from).unwrap();
        match from {
            Side::Left => apply(&mut right_pane, &left_pane, &plan, "\n"),
            Side::Right => apply(&mut left_pane, &right_pane, &plan, "\n"),
        }
        (left_pane.buffer().text(), right_pane.buffer().text())
    }

    #[test]
    fn copying_a_changed_section_replaces_the_target_lines() {
        let left = "a\nB\nc\n";
        let right = "a\nx\nc\n";
        let model = model_of(left, right);
        let row = model.next_difference(0).unwrap_or(1);
        let rows = section_rows(&model, row).unwrap();
        let plan = plan(&model, rows, Side::Left).unwrap();
        let mut right_pane = Pane::from_text(right);
        apply(&mut right_pane, &Pane::from_text(left), &plan, "\n");
        assert_eq!(right_pane.buffer().text(), "a\nB\nc\n");
    }

    #[test]
    fn copying_everything_makes_the_two_sides_equal() {
        let (left, right) = copy_all("one\ntwo\nthree\n", "one\nTWO\n", Side::Left);
        assert_eq!(left, right);
        assert_eq!(right, "one\ntwo\nthree\n");
    }

    #[test]
    fn copying_the_other_way_makes_the_two_sides_equal() {
        let (left, right) = copy_all("one\ntwo\nthree\n", "one\nTWO\n", Side::Right);
        assert_eq!(left, right);
        assert_eq!(left, "one\nTWO\n");
    }

    #[test]
    fn an_orphan_on_the_source_side_becomes_an_insertion() {
        let left = "a\nb\nc\n";
        let right = "a\nc\n";
        let model = model_of(left, right);
        let row = model.row_of_left_line(1).unwrap();
        let plan = plan(&model, row..row + 1, Side::Left).unwrap();
        assert!(plan.target_lines.is_empty());
        assert_eq!(plan.target_lines.start, 1);
        let mut right_pane = Pane::from_text(right);
        apply(&mut right_pane, &Pane::from_text(left), &plan, "\n");
        assert_eq!(right_pane.buffer().text(), "a\nb\nc\n");
    }

    #[test]
    fn an_orphan_on_the_target_side_is_removed_by_the_copy() {
        let left = "a\nc\n";
        let right = "a\nb\nc\n";
        let model = model_of(left, right);
        let row = model.row_of_right_line(1).unwrap();
        let plan = plan(&model, row..row + 1, Side::Left).unwrap();
        assert_eq!(plan.target_lines, LineRange::new(1, 2));
        let mut right_pane = Pane::from_text(right);
        apply(&mut right_pane, &Pane::from_text(left), &plan, "\n");
        assert_eq!(right_pane.buffer().text(), "a\nc\n");
    }

    #[test]
    fn a_copy_undoes_in_one_step() {
        let left = "a\nB\nc\n";
        let right = "a\nx\nc\n";
        let model = model_of(left, right);
        let row = model.next_difference(0).unwrap_or(1);
        let rows = section_rows(&model, row).unwrap();
        let plan = plan(&model, rows, Side::Left).unwrap();
        let mut right_pane = Pane::from_text(right);
        apply(&mut right_pane, &Pane::from_text(left), &plan, "\n");
        assert!(right_pane.undo());
        assert_eq!(right_pane.buffer().text(), right);
        assert!(!right_pane.undo());
    }

    #[test]
    fn a_line_copy_touches_only_that_line() {
        let left = "a\nB\nC\n";
        let right = "a\nx\ny\n";
        let model = model_of(left, right);
        let row = model.row_of_left_line(1).unwrap();
        let plan = plan(&model, row..row + 1, Side::Left).unwrap();
        let mut right_pane = Pane::from_text(right);
        apply(&mut right_pane, &Pane::from_text(left), &plan, "\n");
        assert_eq!(right_pane.buffer().text(), "a\nB\ny\n");
    }

    #[test]
    fn the_copied_text_carries_one_terminator_per_line() {
        let source = Pane::from_text("one\ntwo\n");
        let plan = CopyPlan {
            target_lines: LineRange::new(0, 0),
            source_lines: LineRange::new(0, 2),
        };
        assert_eq!(copied_text(&source, &plan, "\r\n"), "one\r\ntwo\r\n");
    }

    #[test]
    fn a_read_only_target_refuses_the_copy() {
        let mut target = Pane::from_text("x\n");
        target.set_read_only(true);
        let plan = CopyPlan {
            target_lines: LineRange::new(0, 1),
            source_lines: LineRange::new(0, 1),
        };
        apply(&mut target, &Pane::from_text("y\n"), &plan, "\n");
        assert_eq!(target.buffer().text(), "x\n");
    }

    #[test]
    fn rows_with_no_lines_on_either_side_plan_nothing() {
        let model = RowModel::default();
        assert!(plan(&model, 0..0, Side::Left).is_none());
    }

    #[test]
    fn a_side_has_an_opposite() {
        assert_eq!(Side::Left.other(), Side::Right);
        assert_eq!(Side::Right.other(), Side::Left);
    }
}
