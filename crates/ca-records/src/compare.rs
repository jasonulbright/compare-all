//! Comparison of two record trees.
//!
//! Groups and records align by name. The merged tree carries a status on every
//! node, and a group's status rolls up from the records and groups under it: a
//! group holding one difference is different, a group present on one side only
//! is an orphan.

use crate::limits::Unknown;
use crate::record::{compare_names, Record, RecordTree, RecordValue};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::BTreeSet;

/// How one merged node relates to its counterpart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    /// Both sides hold the same content.
    #[default]
    Same,
    /// Both sides hold content and it differs.
    Different,
    /// Only the left side holds the node.
    LeftOnly,
    /// Only the right side holds the node.
    RightOnly,
}

impl Status {
    /// True for every status except [`Status::Same`].
    #[must_use]
    pub const fn is_difference(self) -> bool {
        !matches!(self, Self::Same)
    }

    /// True when the node exists on one side only.
    #[must_use]
    pub const fn is_orphan(self) -> bool {
        matches!(self, Self::LeftOnly | Self::RightOnly)
    }

    fn merge(self, other: Self) -> Self {
        match (self, other) {
            (Self::Same, value) | (value, Self::Same) => value,
            (a, b) if a == b => a,
            _ => Self::Different,
        }
    }
}

/// Whether a difference counts against the comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Importance {
    /// The difference counts.
    #[default]
    Important,
    /// The difference is filtered out by the ignore-unimportant option.
    Unimportant,
}

/// Rules shared by every record comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
// The struct is a settings record: each switch is one independent option, and
// grouping them into sub structures would change the settings document.
#[allow(clippy::struct_excessive_bools)]
pub struct AlignOptions {
    /// Match names letter for letter. Off aligns names ignoring case.
    pub case_sensitive_names: bool,
    /// Compare text data letter for letter. Off compares ignoring case.
    pub case_sensitive_data: bool,
    /// Count a type change as a difference even when the data matches.
    pub compare_types: bool,
    /// Report unimportant differences as matches.
    pub ignore_unimportant: bool,
    /// Record names whose differences do not count.
    ///
    /// A name matches either the record name or the group path and record name
    /// joined by a backslash. Matching ignores case.
    pub unimportant_fields: BTreeSet<String>,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub unknown: Unknown,
}

impl Default for AlignOptions {
    fn default() -> Self {
        Self {
            case_sensitive_names: false,
            case_sensitive_data: true,
            compare_types: true,
            ignore_unimportant: false,
            unimportant_fields: BTreeSet::new(),
            unknown: Unknown::new(),
        }
    }
}

impl AlignOptions {
    fn names_equal(&self, left: &str, right: &str) -> bool {
        if self.case_sensitive_names {
            left == right
        } else {
            left.eq_ignore_ascii_case(right) || fold(left) == fold(right)
        }
    }

    fn name_order(&self, left: &str, right: &str) -> Ordering {
        if self.case_sensitive_names {
            left.cmp(right)
        } else {
            compare_names(left, right)
        }
    }

    fn importance_of(&self, path: &str, name: &str) -> Importance {
        if self.unimportant_fields.is_empty() {
            return Importance::Important;
        }
        let qualified = if path.is_empty() {
            name.to_owned()
        } else {
            format!("{path}\\{name}")
        };
        let hit = self.unimportant_fields.iter().any(|field| {
            field.eq_ignore_ascii_case(name) || field.eq_ignore_ascii_case(&qualified)
        });
        if hit {
            Importance::Unimportant
        } else {
            Importance::Important
        }
    }

    fn values_equal(&self, left: &Record, right: &Record) -> bool {
        if self.compare_types && !left.type_name.eq_ignore_ascii_case(&right.type_name) {
            return false;
        }
        match (&left.value, &right.value) {
            (RecordValue::Text(a), RecordValue::Text(b)) => self.text_equal(a, b),
            (RecordValue::TextList(a), RecordValue::TextList(b)) => {
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| self.text_equal(x, y))
            }
            (a, b) => a == b,
        }
    }

    fn text_equal(&self, left: &str, right: &str) -> bool {
        if self.case_sensitive_data {
            left == right
        } else {
            fold(left) == fold(right)
        }
    }
}

fn fold(value: &str) -> String {
    value.chars().flat_map(char::to_lowercase).collect()
}

/// One merged record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordDiff {
    /// Path of the group that holds the record.
    pub path: String,
    /// Name of the record.
    pub name: String,
    /// How the two sides relate.
    pub status: Status,
    /// Whether the difference counts.
    pub importance: Importance,
    /// Left side record, absent for a right only record.
    pub left: Option<Record>,
    /// Right side record, absent for a left only record.
    pub right: Option<Record>,
}

impl RecordDiff {
    /// True when the difference survives the ignore-unimportant option.
    #[must_use]
    pub const fn counts(&self, ignore_unimportant: bool) -> bool {
        self.status.is_difference()
            && !(ignore_unimportant && matches!(self.importance, Importance::Unimportant))
    }
}

/// One merged group with the records and groups under it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TreeDiff {
    /// Path of the group.
    pub path: String,
    /// Name of the group.
    pub name: String,
    /// Status rolled up from the records and groups under it.
    pub status: Status,
    /// Merged records of this group.
    pub records: Vec<RecordDiff>,
    /// Merged child groups.
    pub children: Vec<TreeDiff>,
}

/// Counts over a merged tree.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiffCounts {
    /// Records both sides share.
    pub same: u64,
    /// Records that differ in a way that counts.
    pub different: u64,
    /// Records that differ in a way that does not count.
    pub unimportant: u64,
    /// Records present on the left only.
    pub left_only: u64,
    /// Records present on the right only.
    pub right_only: u64,
}

impl DiffCounts {
    /// Total number of merged records.
    #[must_use]
    pub const fn total(self) -> u64 {
        self.same
            .saturating_add(self.different)
            .saturating_add(self.unimportant)
            .saturating_add(self.left_only)
            .saturating_add(self.right_only)
    }
}

impl TreeDiff {
    /// Count the merged records under this group.
    #[must_use]
    pub fn counts(&self) -> DiffCounts {
        let mut counts = DiffCounts::default();
        let mut stack = vec![self];
        while let Some(node) = stack.pop() {
            for record in &node.records {
                match record.status {
                    Status::Same => counts.same = counts.same.saturating_add(1),
                    Status::LeftOnly => counts.left_only = counts.left_only.saturating_add(1),
                    Status::RightOnly => counts.right_only = counts.right_only.saturating_add(1),
                    Status::Different => {
                        if matches!(record.importance, Importance::Unimportant) {
                            counts.unimportant = counts.unimportant.saturating_add(1);
                        } else {
                            counts.different = counts.different.saturating_add(1);
                        }
                    }
                }
            }
            stack.extend(node.children.iter());
        }
        counts
    }

    /// Flatten the merged tree into report rows, groups in listing order.
    ///
    /// The rows carry the same fields a record report reads, so the caller
    /// copies them field for field into the report's own row type.
    #[must_use]
    pub fn rows(&self) -> Vec<RecordRow> {
        let mut rows = Vec::new();
        let mut stack = vec![self];
        let mut order = Vec::new();
        while let Some(node) = stack.pop() {
            order.push(node);
            for child in node.children.iter().rev() {
                stack.push(child);
            }
        }
        for node in order {
            for record in &node.records {
                rows.push(RecordRow::from_diff(record));
            }
        }
        rows
    }
}

/// How a report row classifies one record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RowKind {
    /// Both sides hold the same content.
    #[default]
    Same,
    /// Both sides hold content and it differs.
    Changed,
    /// Only the left side holds content.
    LeftOnly,
    /// Only the right side holds content.
    RightOnly,
}

impl From<Status> for RowKind {
    fn from(status: Status) -> Self {
        match status {
            Status::Same => Self::Same,
            Status::Different => Self::Changed,
            Status::LeftOnly => Self::LeftOnly,
            Status::RightOnly => Self::RightOnly,
        }
    }
}

/// One flattened row of a record comparison.
///
/// The field set matches what a record report consumes: a group, a name, a
/// classification, an importance and the two display forms.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecordRow {
    /// Name of the record.
    pub name: String,
    /// Group the record belongs to, absent for a root level record.
    pub group: Option<String>,
    /// Classification of the row.
    pub kind: RowKind,
    /// Importance of a changed row, absent for the other kinds.
    pub importance: Option<Importance>,
    /// Left side display form, absent when the left side has no record.
    pub left: Option<String>,
    /// Right side display form, absent when the right side has no record.
    pub right: Option<String>,
}

impl RecordRow {
    fn from_diff(diff: &RecordDiff) -> Self {
        Self {
            name: diff.name.clone(),
            group: if diff.path.is_empty() {
                None
            } else {
                Some(diff.path.clone())
            },
            kind: RowKind::from(diff.status),
            importance: match diff.status {
                Status::Different => Some(diff.importance),
                _ => None,
            },
            left: diff.left.as_ref().map(|record| record.display.clone()),
            right: diff.right.as_ref().map(|record| record.display.clone()),
        }
    }
}

/// Compare two groups of records.
#[must_use]
pub fn compare_records(
    path: &str,
    left: &[Record],
    right: &[Record],
    options: &AlignOptions,
) -> Vec<RecordDiff> {
    let mut left_sorted: Vec<&Record> = left.iter().collect();
    let mut right_sorted: Vec<&Record> = right.iter().collect();
    left_sorted.sort_by(|a, b| options.name_order(&a.name, &b.name));
    right_sorted.sort_by(|a, b| options.name_order(&a.name, &b.name));

    let mut out = Vec::with_capacity(left_sorted.len().max(right_sorted.len()));
    let mut li = 0usize;
    let mut ri = 0usize;
    while li < left_sorted.len() || ri < right_sorted.len() {
        let order = match (left_sorted.get(li), right_sorted.get(ri)) {
            (Some(a), Some(b)) => {
                if options.names_equal(&a.name, &b.name) {
                    Ordering::Equal
                } else {
                    options.name_order(&a.name, &b.name)
                }
            }
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => break,
        };
        match order {
            Ordering::Equal => {
                let a = left_sorted[li];
                let b = right_sorted[ri];
                li += 1;
                ri += 1;
                let same = options.values_equal(a, b) && options.names_equal(&a.name, &b.name);
                let status = if same {
                    Status::Same
                } else {
                    Status::Different
                };
                out.push(RecordDiff {
                    path: path.to_owned(),
                    name: a.name.clone(),
                    status,
                    importance: options.importance_of(path, &a.name),
                    left: Some(a.clone()),
                    right: Some(b.clone()),
                });
            }
            Ordering::Less => {
                let a = left_sorted[li];
                li += 1;
                out.push(RecordDiff {
                    path: path.to_owned(),
                    name: a.name.clone(),
                    status: Status::LeftOnly,
                    importance: options.importance_of(path, &a.name),
                    left: Some(a.clone()),
                    right: None,
                });
            }
            Ordering::Greater => {
                let b = right_sorted[ri];
                ri += 1;
                out.push(RecordDiff {
                    path: path.to_owned(),
                    name: b.name.clone(),
                    status: Status::RightOnly,
                    importance: options.importance_of(path, &b.name),
                    left: None,
                    right: Some(b.clone()),
                });
            }
        }
    }
    out
}

/// Compare two record trees into one merged tree.
#[must_use]
pub fn compare_trees(left: &RecordTree, right: &RecordTree, options: &AlignOptions) -> TreeDiff {
    merge(Some(left), Some(right), options)
}

/// Build the merged tree for a group that exists on one side only.
#[must_use]
pub fn only_tree(tree: &RecordTree, side_is_left: bool, options: &AlignOptions) -> TreeDiff {
    if side_is_left {
        merge(Some(tree), None, options)
    } else {
        merge(None, Some(tree), options)
    }
}

type ChildPair<'a> = (Option<&'a RecordTree>, Option<&'a RecordTree>);

struct Frame<'a> {
    left: Option<&'a RecordTree>,
    right: Option<&'a RecordTree>,
    pairs: Vec<ChildPair<'a>>,
    next: usize,
    done: Vec<TreeDiff>,
}

impl<'a> Frame<'a> {
    fn new(
        left: Option<&'a RecordTree>,
        right: Option<&'a RecordTree>,
        options: &AlignOptions,
    ) -> Self {
        const NO_CHILDREN: &[RecordTree] = &[];
        let pairs = align_children(
            left.map_or(NO_CHILDREN, |node| &node.children[..]),
            right.map_or(NO_CHILDREN, |node| &node.children[..]),
            options,
        );
        Self {
            left,
            right,
            pairs,
            next: 0,
            done: Vec::new(),
        }
    }

    fn close(self, options: &AlignOptions) -> TreeDiff {
        const NO_RECORDS: &[Record] = &[];
        let path = self
            .left
            .or(self.right)
            .map(|node| node.path.clone())
            .unwrap_or_default();
        let name = self
            .left
            .or(self.right)
            .map(|node| node.name.clone())
            .unwrap_or_default();
        let records = compare_records(
            &path,
            self.left.map_or(NO_RECORDS, |node| &node.records[..]),
            self.right.map_or(NO_RECORDS, |node| &node.records[..]),
            options,
        );
        let mut status = match (self.left.is_some(), self.right.is_some()) {
            (true, false) => Status::LeftOnly,
            (false, true) => Status::RightOnly,
            _ => Status::Same,
        };
        if !status.is_orphan() {
            let child_differs = self.done.iter().any(|child| child.status.is_difference());
            let record_differs = records
                .iter()
                .any(|record| record.counts(options.ignore_unimportant));
            if child_differs || record_differs {
                status = status.merge(Status::Different);
            }
        }
        TreeDiff {
            path,
            name,
            status,
            records,
            children: self.done,
        }
    }
}

/// Merge two trees with an explicit stack, so depth costs heap, not stack.
fn merge(
    left: Option<&RecordTree>,
    right: Option<&RecordTree>,
    options: &AlignOptions,
) -> TreeDiff {
    let mut stack = vec![Frame::new(left, right, options)];
    loop {
        let Some(top) = stack.last_mut() else {
            return TreeDiff::default();
        };
        if let Some(pair) = top.pairs.get(top.next).copied() {
            top.next += 1;
            stack.push(Frame::new(pair.0, pair.1, options));
            continue;
        }
        let Some(frame) = stack.pop() else {
            return TreeDiff::default();
        };
        let finished = frame.close(options);
        match stack.last_mut() {
            Some(parent) => parent.done.push(finished),
            None => return finished,
        }
    }
}

fn align_children<'a>(
    left: &'a [RecordTree],
    right: &'a [RecordTree],
    options: &AlignOptions,
) -> Vec<ChildPair<'a>> {
    let mut left_sorted: Vec<&RecordTree> = left.iter().collect();
    let mut right_sorted: Vec<&RecordTree> = right.iter().collect();
    left_sorted.sort_by(|a, b| options.name_order(&a.name, &b.name));
    right_sorted.sort_by(|a, b| options.name_order(&a.name, &b.name));
    let mut out = Vec::new();
    let mut li = 0usize;
    let mut ri = 0usize;
    while li < left_sorted.len() || ri < right_sorted.len() {
        let order = match (left_sorted.get(li), right_sorted.get(ri)) {
            (Some(a), Some(b)) => {
                if options.names_equal(&a.name, &b.name) {
                    Ordering::Equal
                } else {
                    options.name_order(&a.name, &b.name)
                }
            }
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => break,
        };
        match order {
            Ordering::Equal => {
                out.push((Some(left_sorted[li]), Some(right_sorted[ri])));
                li += 1;
                ri += 1;
            }
            Ordering::Less => {
                out.push((Some(left_sorted[li]), None));
                li += 1;
            }
            Ordering::Greater => {
                out.push((None, Some(right_sorted[ri])));
                ri += 1;
            }
        }
    }
    out
}
