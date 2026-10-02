//! Three way merge of two changed versions against a common ancestor.
//!
//! The ancestor is what removes the ambiguity of a two way comparison: an item
//! present on one side and absent on the other is an addition when the
//! ancestor lacks it and a deletion when the ancestor has it.

use crate::cancel::{check, Cancel, NeverCancel};
use crate::lines::{
    diff_line_slices_cancellable, normalize_line, split_lines, Hunk, HunkKind, LineCompareOptions,
};
use crate::DiffError;
use serde::{Deserialize, Serialize};
use std::ops::Range;

/// Merge status of one output region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MergeKind {
    /// Neither side changed the ancestor here.
    Unchanged,
    /// Only the left side changed the ancestor here.
    LeftChange,
    /// Only the right side changed the ancestor here.
    RightChange,
    /// Both sides made the same change here.
    SameChange,
    /// Both sides changed here and their changes differ.
    Conflict,
}

/// Which input supplies a region's output content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MergeSide {
    /// The common ancestor.
    Base,
    /// The left version.
    Left,
    /// The right version.
    Right,
}

/// When two changes on opposite sides count as a conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConflictScope {
    /// Only changes that land on the same ancestor lines conflict.
    ChangedLinesOnly,
    /// Changes separated by at most this many ancestor lines also conflict, so
    /// nearby edits are reviewed together.
    Separation {
        /// Largest gap, in ancestor lines, that still counts as a conflict.
        lines: u32,
    },
}

impl Default for ConflictScope {
    fn default() -> Self {
        Self::Separation { lines: 2 }
    }
}

/// Settings for a three way merge.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeOptions {
    /// Differences the comparison disregards on all three inputs.
    pub compare: LineCompareOptions,
    /// When opposing changes conflict.
    pub conflict_scope: ConflictScope,
}

/// One region of the merged result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeRegion {
    /// Merge status.
    pub kind: MergeKind,
    /// Line range in the ancestor (zero-based, end exclusive).
    pub base: Range<u32>,
    /// Corresponding line range in the left version.
    pub left: Range<u32>,
    /// Corresponding line range in the right version.
    pub right: Range<u32>,
    /// Input the auto-merge took this region's output from.
    pub output: MergeSide,
    /// Line range this region occupies in the merged output.
    pub out: Range<u32>,
}

/// Result of an automatic merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeResult {
    /// Regions in ancestor order, covering all three inputs without gaps.
    pub regions: Vec<MergeRegion>,
    /// The merged lines, with their original terminators.
    pub output: Vec<String>,
    /// How many regions need manual review.
    pub conflicts: u32,
}

impl MergeResult {
    /// Whether the merge completed without anything needing review.
    #[must_use]
    pub const fn is_clean(&self) -> bool {
        self.conflicts == 0
    }

    /// The merged lines joined back into one string.
    #[must_use]
    pub fn to_text(&self) -> String {
        self.output.concat()
    }
}

/// A change one side made, in ancestor coordinates and its own coordinates.
#[derive(Debug, Clone)]
struct SideChange {
    base: Range<u32>,
    side: Range<u32>,
    from_left: bool,
}

fn len_u32<T>(slice: &[T]) -> u32 {
    u32::try_from(slice.len()).unwrap_or(u32::MAX)
}

fn changes(hunks: &[Hunk], from_left: bool) -> Vec<SideChange> {
    hunks
        .iter()
        .filter(|h| h.kind != HunkKind::Same)
        .map(|h| SideChange {
            base: h.left.clone(),
            side: h.right.clone(),
            from_left,
        })
        .collect()
}

/// Half-open ancestor span a change occupies for overlap testing. A pure
/// insertion has an empty range, so it is widened to one line to keep it from
/// slipping between its neighbours.
fn touch(change: &SideChange) -> Range<u32> {
    change.base.start..change.base.end.max(change.base.start + 1)
}

/// Whether two changes are close enough to be reviewed together.
///
/// The separation is the number of unchanged ancestor lines that may sit
/// between an opposing pair, so it is measured once across the gap between
/// them, not added to each change's own span. Two changes on the same side
/// join only when their spans actually meet.
fn overlaps(a: &SideChange, b: &SideChange, separation: u32) -> bool {
    let ra = touch(a);
    let rb = touch(b);
    if ra.start < rb.end && rb.start < ra.end {
        return true;
    }
    if a.from_left == b.from_left || separation == 0 {
        return false;
    }
    let gap = if ra.end <= rb.start {
        rb.start - ra.end
    } else {
        ra.start.saturating_sub(rb.end)
    };
    gap <= separation
}

fn cluster(mut all: Vec<SideChange>, separation: u32) -> Vec<Vec<SideChange>> {
    all.sort_by_key(|c| (c.base.start, c.base.end));
    let mut out: Vec<Vec<SideChange>> = Vec::new();
    for change in all {
        let joins = out
            .last()
            .is_some_and(|group| group.iter().any(|m| overlaps(m, &change, separation)));
        if joins {
            if let Some(group) = out.last_mut() {
                group.push(change);
            }
        } else {
            out.push(vec![change]);
        }
    }
    out
}

fn keys(lines: &[&str], options: &LineCompareOptions) -> Vec<String> {
    lines.iter().map(|l| normalize_line(l, options)).collect()
}

fn key_slice<'a>(keys: &'a [String], range: &Range<u32>) -> &'a [String] {
    keys.get(range.start as usize..range.end as usize)
        .unwrap_or_default()
}

fn take_lines(lines: &[&str], range: &Range<u32>, out: &mut Vec<String>) {
    for i in range.start..range.end {
        out.push(
            lines
                .get(i as usize)
                .copied()
                .unwrap_or_default()
                .to_owned(),
        );
    }
}

/// Merge `left` and `right` against their common ancestor `base`.
///
/// Every non-conflicting change is taken automatically; a conflicting region
/// keeps the ancestor's lines so the output is a complete file that a reviewer
/// can then resolve.
#[must_use]
pub fn merge3(left: &[&str], base: &[&str], right: &[&str], options: &MergeOptions) -> MergeResult {
    merge3_cancellable(left, base, right, options, &NeverCancel).unwrap_or_else(|_| MergeResult {
        regions: Vec::new(),
        output: Vec::new(),
        conflicts: 0,
    })
}

/// Merge three inputs, abandoning the work when `cancel` is raised.
///
/// # Errors
///
/// Returns [`DiffError::Cancelled`] when the flag is raised before the merge
/// finishes.
pub fn merge3_cancellable(
    left: &[&str],
    base: &[&str],
    right: &[&str],
    options: &MergeOptions,
    cancel: &dyn Cancel,
) -> Result<MergeResult, DiffError> {
    let base_len = len_u32(base);
    let left_hunks = diff_line_slices_cancellable(base, left, &options.compare, cancel)?;
    let right_hunks = diff_line_slices_cancellable(base, right, &options.compare, cancel)?;

    let left_keys = keys(left, &options.compare);
    let right_keys = keys(right, &options.compare);

    let separation = match options.conflict_scope {
        ConflictScope::ChangedLinesOnly => 0,
        ConflictScope::Separation { lines } => lines,
    };
    let mut all = changes(&left_hunks, true);
    all.extend(changes(&right_hunks, false));
    let groups = cluster(all, separation);

    let mut regions: Vec<MergeRegion> = Vec::new();
    let mut output: Vec<String> = Vec::new();
    let mut conflicts = 0u32;
    let mut cursor = 0u32;
    // Side cursors advance with the emitted regions, so each side's ranges are
    // contiguous even where a side's change occupies no ancestor lines.
    let mut left_cursor = 0u32;
    let mut right_cursor = 0u32;

    for group in groups {
        check(cancel)?;
        let start = group.iter().map(|c| c.base.start).min().unwrap_or(cursor);
        let end = group
            .iter()
            .map(|c| c.base.end)
            .max()
            .unwrap_or(start)
            .max(start);
        if start > cursor {
            let span = start - cursor;
            let lr = left_cursor..left_cursor + span;
            let rr = right_cursor..right_cursor + span;
            let out_start = len_u32(&output);
            take_lines(base, &(cursor..start), &mut output);
            regions.push(MergeRegion {
                kind: MergeKind::Unchanged,
                base: cursor..start,
                left: lr,
                right: rr,
                output: MergeSide::Base,
                out: out_start..len_u32(&output),
            });
            left_cursor += span;
            right_cursor += span;
            cursor = start;
        }
        let has_left = group.iter().any(|c| c.from_left);
        let has_right = group.iter().any(|c| !c.from_left);
        let lr = left_cursor..side_end(&group, true, left_cursor, cursor, end);
        let rr = right_cursor..side_end(&group, false, right_cursor, cursor, end);
        let (kind, side) = match (has_left, has_right) {
            (true, false) => (MergeKind::LeftChange, MergeSide::Left),
            (false, true) => (MergeKind::RightChange, MergeSide::Right),
            _ => {
                if key_slice(&left_keys, &lr) == key_slice(&right_keys, &rr) {
                    (MergeKind::SameChange, MergeSide::Left)
                } else {
                    conflicts += 1;
                    (MergeKind::Conflict, MergeSide::Base)
                }
            }
        };
        let out_start = len_u32(&output);
        match side {
            MergeSide::Base => take_lines(base, &(cursor..end), &mut output),
            MergeSide::Left => take_lines(left, &lr, &mut output),
            MergeSide::Right => take_lines(right, &rr, &mut output),
        }
        regions.push(MergeRegion {
            kind,
            base: cursor..end,
            left: lr.clone(),
            right: rr.clone(),
            output: side,
            out: out_start..len_u32(&output),
        });
        left_cursor = lr.end;
        right_cursor = rr.end;
        cursor = end.max(cursor);
    }
    if cursor < base_len {
        let span = base_len - cursor;
        let out_start = len_u32(&output);
        take_lines(base, &(cursor..base_len), &mut output);
        regions.push(MergeRegion {
            kind: MergeKind::Unchanged,
            base: cursor..base_len,
            left: left_cursor..left_cursor + span,
            right: right_cursor..right_cursor + span,
            output: MergeSide::Base,
            out: out_start..len_u32(&output),
        });
    }
    Ok(MergeResult {
        regions,
        output,
        conflicts,
    })
}

/// End index on one side for a cluster spanning ancestor lines `start..end`.
///
/// Ancestor lines the side did not change contribute one line each, so the
/// side range stays aligned with the ancestor past the last change.
fn side_end(group: &[SideChange], from_left: bool, cursor: u32, start: u32, end: u32) -> u32 {
    let mut last_base = start;
    let mut last_side = cursor;
    let mut seen = false;
    for change in group.iter().filter(|c| c.from_left == from_left) {
        if !seen || change.base.end >= last_base {
            last_base = change.base.end.max(last_base);
            last_side = change.side.end.max(last_side);
            seen = true;
        }
    }
    if seen {
        last_side + end.saturating_sub(last_base)
    } else {
        cursor + end.saturating_sub(start)
    }
}

/// Convenience wrapper that splits three texts into lines and merges them.
#[must_use]
pub fn merge3_text(left: &str, base: &str, right: &str, options: &MergeOptions) -> MergeResult {
    merge3(
        &split_lines(left),
        &split_lines(base),
        &split_lines(right),
        options,
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn kinds(result: &MergeResult) -> Vec<MergeKind> {
        result.regions.iter().map(|r| r.kind).collect()
    }

    #[test]
    fn no_changes_reproduces_the_ancestor() {
        let r = merge3_text("a\nb\n", "a\nb\n", "a\nb\n", &MergeOptions::default());
        assert_eq!(r.to_text(), "a\nb\n");
        assert_eq!(kinds(&r), [MergeKind::Unchanged]);
        assert!(r.is_clean());
    }

    #[test]
    fn left_only_change_is_taken() {
        let r = merge3_text(
            "a\nX\nc\n",
            "a\nb\nc\n",
            "a\nb\nc\n",
            &MergeOptions::default(),
        );
        assert_eq!(r.to_text(), "a\nX\nc\n");
        assert!(r.regions.iter().any(|x| x.kind == MergeKind::LeftChange));
        assert!(r.is_clean());
    }

    #[test]
    fn right_only_change_is_taken() {
        let r = merge3_text(
            "a\nb\nc\n",
            "a\nb\nc\n",
            "a\nb\nY\n",
            &MergeOptions::default(),
        );
        assert_eq!(r.to_text(), "a\nb\nY\n");
        assert!(r.regions.iter().any(|x| x.kind == MergeKind::RightChange));
    }

    #[test]
    fn both_sides_change_different_places() {
        let base = "1\n2\n3\n4\n5\n6\n7\n8\n";
        let left = "1\nL\n3\n4\n5\n6\n7\n8\n";
        let right = "1\n2\n3\n4\n5\n6\nR\n8\n";
        let r = merge3_text(left, base, right, &MergeOptions::default());
        assert_eq!(r.to_text(), "1\nL\n3\n4\n5\n6\nR\n8\n");
        assert!(r.is_clean());
    }

    #[test]
    fn identical_change_on_both_sides_is_not_a_conflict() {
        let r = merge3_text(
            "a\nZ\nc\n",
            "a\nb\nc\n",
            "a\nZ\nc\n",
            &MergeOptions::default(),
        );
        assert!(kinds(&r).contains(&MergeKind::SameChange));
        assert!(r.is_clean());
        assert_eq!(r.to_text(), "a\nZ\nc\n");
    }

    #[test]
    fn different_change_on_the_same_line_conflicts() {
        let r = merge3_text(
            "a\nL\nc\n",
            "a\nb\nc\n",
            "a\nR\nc\n",
            &MergeOptions::default(),
        );
        assert_eq!(r.conflicts, 1);
        assert!(kinds(&r).contains(&MergeKind::Conflict));
        assert_eq!(r.to_text(), "a\nb\nc\n");
    }

    #[test]
    fn nearby_changes_conflict_under_the_separation_rule() {
        let base = "1\n2\n3\n4\n5\n";
        let left = "1\nL\n3\n4\n5\n";
        let right = "1\n2\n3\nR\n5\n";
        let near = merge3_text(left, base, right, &MergeOptions::default());
        assert_eq!(near.conflicts, 1);
        let strict = MergeOptions {
            conflict_scope: ConflictScope::ChangedLinesOnly,
            ..MergeOptions::default()
        };
        let far = merge3_text(left, base, right, &strict);
        assert_eq!(far.conflicts, 0);
        assert_eq!(far.to_text(), "1\nL\n3\nR\n5\n");
    }

    /// `n` numbered lines, with line `i` carrying `tag` in front of its number.
    fn numbered(n: u32, i: u32, tag: &str) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        for k in 0..n {
            if k == i {
                let _ = writeln!(out, "{tag}{k}");
            } else {
                let _ = writeln!(out, "{k}");
            }
        }
        out
    }

    fn changed_at(n: u32, i: u32) -> String {
        numbered(n, i, "C")
    }

    #[test]
    fn the_separation_is_counted_once_across_the_gap() {
        let n = 12;
        let base = numbered(n, n, "");
        // The default scope allows two unchanged ancestor lines between an
        // opposing pair, so a distance of three still conflicts and four does
        // not.
        for (distance, expected) in [(1u32, 1u32), (2, 1), (3, 1), (4, 0), (5, 0)] {
            let left = changed_at(n, 2);
            let right = changed_at(n, 2 + distance);
            let r = merge3_text(&left, &base, &right, &MergeOptions::default());
            assert_eq!(r.conflicts, expected, "distance {distance}");
        }
    }

    #[test]
    fn changed_lines_only_ignores_neighbours() {
        let n = 8;
        let base = numbered(n, n, "");
        let strict = MergeOptions {
            conflict_scope: ConflictScope::ChangedLinesOnly,
            ..MergeOptions::default()
        };
        let r = merge3_text(&changed_at(n, 2), &base, &changed_at(n, 3), &strict);
        assert_eq!(r.conflicts, 0);
        let opposed = numbered(n, 2, "D");
        let same_line = merge3_text(&changed_at(n, 2), &base, &opposed, &strict);
        assert_eq!(same_line.conflicts, 1);
    }

    #[test]
    fn left_equal_to_base_yields_right() {
        let base = "a\nb\nc\nd\n";
        let right = "a\nX\nc\nd\ne\n";
        let r = merge3_text(base, base, right, &MergeOptions::default());
        assert_eq!(r.to_text(), right);
        assert!(r.is_clean());
    }

    #[test]
    fn right_equal_to_base_yields_left() {
        let base = "a\nb\nc\n";
        let left = "z\na\nb\n";
        let r = merge3_text(left, base, base, &MergeOptions::default());
        assert_eq!(r.to_text(), left);
    }

    #[test]
    fn insertion_on_one_side_only() {
        let r = merge3_text("a\nnew\nb\n", "a\nb\n", "a\nb\n", &MergeOptions::default());
        assert_eq!(r.to_text(), "a\nnew\nb\n");
        assert!(r.is_clean());
    }

    #[test]
    fn deletion_on_one_side_only() {
        let r = merge3_text("a\nc\n", "a\nb\nc\n", "a\nb\nc\n", &MergeOptions::default());
        assert_eq!(r.to_text(), "a\nc\n");
    }

    #[test]
    fn regions_cover_the_ancestor_and_the_output() {
        let r = merge3_text(
            "a\nL\nc\nd\n",
            "a\nb\nc\nd\n",
            "a\nb\nc\nR\n",
            &MergeOptions::default(),
        );
        let mut base_cursor = 0;
        let mut out_cursor = 0;
        for region in &r.regions {
            assert_eq!(region.base.start, base_cursor);
            assert_eq!(region.out.start, out_cursor);
            base_cursor = region.base.end;
            out_cursor = region.out.end;
        }
        assert_eq!(base_cursor, 4);
        assert_eq!(out_cursor as usize, r.output.len());
    }

    #[test]
    fn options_apply_to_all_three_inputs() {
        let opts = MergeOptions {
            compare: LineCompareOptions {
                ignore_case: true,
                ..LineCompareOptions::default()
            },
            ..MergeOptions::default()
        };
        let r = merge3_text("A\nb\n", "a\nb\n", "a\nB\n", &opts);
        assert_eq!(kinds(&r), [MergeKind::Unchanged]);
        assert!(r.is_clean());
    }

    #[test]
    fn empty_inputs() {
        let r = merge3_text("", "", "", &MergeOptions::default());
        assert!(r.regions.is_empty());
        assert!(r.output.is_empty());
        assert!(r.is_clean());
    }
}
