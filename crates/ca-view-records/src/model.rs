//! The display model: a merged record tree flattened into rows.
//!
//! The tree is flattened once, on the worker, into an arena in listing order:
//! a key, then its values, then the keys under it. Each key knows where its
//! subtree ends, so collapsing a key skips its subtree in one step. The rolled
//! up class of every key is worked out in the same pass, once with unimportant
//! differences counted and once with them ignored, so the ignore switch never
//! walks the tree again.
//!
//! The rows on screen are a list of arena positions, rebuilt only when the
//! display filter, the ignore switch or an expansion changes.

use ca_records::{Importance, Record, Status, TreeDiff};
use ca_ui::report::{Importance as ReportImportance, RecordRow, RowKind};
use ca_ui::theme::records::RecordClass;
use ca_ui::thumbnail::Severity;
use std::sync::Arc;

/// Which rows the listing shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DisplayFilter {
    /// Every row.
    #[default]
    All,
    /// Rows that differ, and the keys that hold them.
    Differences,
    /// Rows that match, and the keys that hold them.
    Same,
    /// No row.
    None,
}

impl DisplayFilter {
    /// What the filter control shows.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "Show all",
            Self::Differences => "Show differences",
            Self::Same => "Show same",
            Self::None => "Show none",
        }
    }

    /// Every filter, in the order the control lists them.
    pub const ALL: [Self; 4] = [Self::All, Self::Differences, Self::Same, Self::None];
}

/// Which side of the comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The left pane.
    Left,
    /// The right pane.
    Right,
}

/// One row of the arena: a key or a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// Indent level. Top level keys and values sit at zero.
    pub depth: usize,
    /// Arena position of the key that holds this row.
    pub parent: Option<usize>,
    /// One past the last arena position under this row.
    pub end: usize,
    /// Name of the key or the value.
    pub name: String,
    /// Full path of the key, or of the key that holds the value.
    pub path: String,
    /// True for a key, false for a value.
    pub is_group: bool,
    /// How the two sides relate.
    pub status: Status,
    /// Whether a difference counts.
    pub importance: Importance,
    /// The left value, for a value row present on the left.
    pub left: Option<Record>,
    /// The right value, for a value row present on the right.
    pub right: Option<Record>,
    strict: RecordClass,
    lenient: RecordClass,
}

impl Node {
    fn group(
        path: String,
        name: String,
        status: Status,
        depth: usize,
        parent: Option<usize>,
    ) -> Self {
        let class = if status.is_orphan() {
            RecordClass::Orphan
        } else {
            RecordClass::Same
        };
        Self {
            depth,
            parent,
            end: 0,
            name,
            path,
            is_group: true,
            status,
            importance: Importance::Important,
            left: None,
            right: None,
            strict: class,
            lenient: class,
        }
    }

    fn value(diff: ca_records::RecordDiff, depth: usize, parent: Option<usize>) -> Self {
        let (strict, lenient) = match (diff.status, diff.importance) {
            (Status::Same, _) => (RecordClass::Same, RecordClass::Same),
            (Status::Different, Importance::Unimportant) => {
                (RecordClass::Unimportant, RecordClass::Same)
            }
            (Status::Different, _) => (RecordClass::Different, RecordClass::Different),
            (Status::LeftOnly | Status::RightOnly, _) => (RecordClass::Orphan, RecordClass::Orphan),
        };
        Self {
            depth,
            parent,
            end: 0,
            name: diff.name,
            path: diff.path,
            is_group: false,
            status: diff.status,
            importance: diff.importance,
            left: diff.left,
            right: diff.right,
            strict,
            lenient,
        }
    }

    /// The class the row is painted as.
    #[must_use]
    pub const fn class(&self, ignore_unimportant: bool) -> RecordClass {
        if ignore_unimportant {
            self.lenient
        } else {
            self.strict
        }
    }

    /// True when the row exists on `side`.
    #[must_use]
    pub const fn is_on(&self, side: Side) -> bool {
        match (self.is_group, side) {
            (true, Side::Left) => !matches!(self.status, Status::RightOnly),
            (true, Side::Right) => !matches!(self.status, Status::LeftOnly),
            (false, Side::Left) => self.left.is_some(),
            (false, Side::Right) => self.right.is_some(),
        }
    }

    /// The record on `side`, for a value row present there.
    #[must_use]
    pub const fn record(&self, side: Side) -> Option<&Record> {
        match side {
            Side::Left => self.left.as_ref(),
            Side::Right => self.right.as_ref(),
        }
    }

    /// The key path joined to the value name, or the key path of a key.
    #[must_use]
    pub fn qualified_name(&self) -> String {
        if self.is_group || self.path.is_empty() {
            return if self.is_group {
                self.path.clone()
            } else {
                self.name.clone()
            };
        }
        format!("{}\\{}", self.path, self.name)
    }
}

/// The more severe of two classes.
fn worst(a: RecordClass, b: RecordClass) -> RecordClass {
    if b.rank() > a.rank() {
        b
    } else {
        a
    }
}

/// Flatten a merged tree into the arena the listing reads.
///
/// The root group itself is not a row: its values and its keys sit at depth
/// zero. The walk uses an explicit stack, so a deep tree costs heap, not stack.
#[must_use]
pub fn flatten(tree: TreeDiff) -> Vec<Node> {
    let TreeDiff {
        records, children, ..
    } = tree;
    let mut nodes: Vec<Node> = Vec::new();
    for record in records {
        nodes.push(Node::value(record, 0, None));
    }
    let mut stack: Vec<(TreeDiff, usize, Option<usize>)> = children
        .into_iter()
        .rev()
        .map(|child| (child, 0, None))
        .collect();
    while let Some((node, depth, parent)) = stack.pop() {
        let index = nodes.len();
        let TreeDiff {
            path,
            name,
            status,
            records,
            children,
        } = node;
        nodes.push(Node::group(path, name, status, depth, parent));
        for record in records {
            nodes.push(Node::value(record, depth + 1, Some(index)));
        }
        for child in children.into_iter().rev() {
            stack.push((child, depth + 1, Some(index)));
        }
    }
    for (index, node) in nodes.iter_mut().enumerate() {
        node.end = index + 1;
    }
    // Every descendant sits after its key, so a reverse pass folds each row
    // into its key after the row has received all of its own descendants.
    for index in (0..nodes.len()).rev() {
        let Some(parent) = nodes[index].parent else {
            continue;
        };
        let (end, strict, lenient) = (nodes[index].end, nodes[index].strict, nodes[index].lenient);
        let key = &mut nodes[parent];
        key.end = key.end.max(end);
        if !key.status.is_orphan() {
            key.strict = worst(key.strict, strict);
            key.lenient = worst(key.lenient, lenient);
        }
    }
    nodes
}

/// Counts over the value rows of a comparison.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    /// Values both sides share.
    pub same: u64,
    /// Values that differ in a way that counts.
    pub different: u64,
    /// Values that differ in a way that does not count.
    pub unimportant: u64,
    /// Values present on the left only.
    pub left_only: u64,
    /// Values present on the right only.
    pub right_only: u64,
}

/// Counts over key rows, separate from the values they contain.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyCounts {
    /// Keys the two sides share.
    pub same: u64,
    /// Keys that differ in a way that counts.
    pub different: u64,
    /// Keys whose differences do not count.
    pub unimportant: u64,
    /// Keys present on the left only.
    pub left_only: u64,
    /// Keys present on the right only.
    pub right_only: u64,
}

/// The rows on screen over one arena.
#[derive(Debug, Clone, Default)]
pub struct Listing {
    nodes: Arc<Vec<Node>>,
    collapsed: Vec<bool>,
    filter: DisplayFilter,
    ignore_unimportant: bool,
    visible: Vec<usize>,
    cursor: usize,
}

impl Listing {
    /// A listing over `nodes`, every key expanded.
    #[must_use]
    pub fn new(nodes: Arc<Vec<Node>>) -> Self {
        let mut listing = Self::default();
        listing.set_nodes(nodes);
        listing
    }

    /// Show a new arena, keeping the filter and the ignore switch.
    ///
    /// Keys the old arena held collapsed stay collapsed where the new arena
    /// holds the same path.
    pub fn set_nodes(&mut self, nodes: Arc<Vec<Node>>) {
        let closed: std::collections::HashSet<String> = self
            .nodes
            .iter()
            .zip(&self.collapsed)
            .filter(|(node, collapsed)| node.is_group && **collapsed)
            .map(|(node, _)| node.path.to_ascii_lowercase())
            .collect();
        self.collapsed = nodes
            .iter()
            .map(|node| node.is_group && closed.contains(&node.path.to_ascii_lowercase()))
            .collect();
        self.nodes = nodes;
        self.cursor = 0;
        self.rebuild(None);
    }

    /// Show a new arena and keep the cursor on the same key or value, or on
    /// the nearest key above it that the new arena still shows.
    pub fn set_nodes_keeping_cursor(&mut self, nodes: Arc<Vec<Node>>) {
        let held = self
            .node_at(self.cursor)
            .map(|node| (node.path.clone(), node.name.clone(), node.is_group));
        self.set_nodes(nodes);
        let Some((path, name, is_group)) = held else {
            return;
        };
        let find = |listing: &Self, path: &str, name: Option<&str>| {
            (0..listing.visible.len()).find(|row| {
                listing.node_at(*row).is_some_and(|node| {
                    node.path.eq_ignore_ascii_case(path)
                        && match name {
                            Some(name) => !node.is_group && node.name.eq_ignore_ascii_case(name),
                            None => node.is_group,
                        }
                })
            })
        };
        let exact = if is_group {
            find(self, &path, None)
        } else {
            find(self, &path, Some(&name))
        };
        let mut row = exact;
        let mut key = path.as_str();
        while row.is_none() {
            if !is_group || key != path {
                row = find(self, key, None);
            }
            if row.is_some() {
                break;
            }
            let Some((parent, _)) = key.rsplit_once('\\') else {
                break;
            };
            key = parent;
        }
        if let Some(row) = row {
            self.set_cursor(row);
        }
    }

    /// Every row of the arena, shown or not.
    #[must_use]
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    /// Share the row arena with a worker that only reads it.
    pub(crate) fn shared_nodes(&self) -> Arc<Vec<Node>> {
        Arc::clone(&self.nodes)
    }

    /// Number of rows on screen.
    #[must_use]
    pub fn rows(&self) -> usize {
        self.visible.len()
    }

    /// Arena position of the row on screen at `row`.
    #[must_use]
    pub fn index_at(&self, row: usize) -> Option<usize> {
        self.visible.get(row).copied()
    }

    /// The row on screen at `row`.
    #[must_use]
    pub fn node_at(&self, row: usize) -> Option<&Node> {
        self.index_at(row).and_then(|index| self.nodes.get(index))
    }

    /// The class the row on screen at `row` is painted as.
    #[must_use]
    pub fn class_at(&self, row: usize) -> Option<RecordClass> {
        self.node_at(row)
            .map(|node| node.class(self.ignore_unimportant))
    }

    /// The display filter in force.
    #[must_use]
    pub const fn filter(&self) -> DisplayFilter {
        self.filter
    }

    /// Show a different set of rows.
    pub fn set_filter(&mut self, filter: DisplayFilter) {
        if filter != self.filter {
            self.filter = filter;
            self.rebuild_keeping_cursor();
        }
    }

    /// True when unimportant differences show as matches.
    #[must_use]
    pub const fn ignores_unimportant(&self) -> bool {
        self.ignore_unimportant
    }

    /// Show unimportant differences as matches, or stop doing so.
    pub fn set_ignore_unimportant(&mut self, ignore: bool) {
        if ignore != self.ignore_unimportant {
            self.ignore_unimportant = ignore;
            self.rebuild_keeping_cursor();
        }
    }

    /// True when the key at arena position `index` is collapsed.
    #[must_use]
    pub fn is_collapsed(&self, index: usize) -> bool {
        self.collapsed.get(index).copied().unwrap_or(false)
    }

    /// True when the key on screen at `row` holds rows under it.
    #[must_use]
    pub fn has_children(&self, row: usize) -> bool {
        self.index_at(row)
            .and_then(|index| self.nodes.get(index).map(|node| (index, node)))
            .is_some_and(|(index, node)| node.is_group && node.end > index + 1)
    }

    /// Open or close the key on screen at `row`.
    pub fn toggle(&mut self, row: usize) {
        if let Some(index) = self.index_at(row) {
            let collapsed = self.is_collapsed(index);
            self.set_collapsed(index, !collapsed);
        }
    }

    /// Open the key on screen at `row`.
    pub fn expand(&mut self, row: usize) {
        if let Some(index) = self.index_at(row) {
            self.set_collapsed(index, false);
        }
    }

    /// Close the key on screen at `row`.
    pub fn collapse(&mut self, row: usize) {
        if let Some(index) = self.index_at(row) {
            self.set_collapsed(index, true);
        }
    }

    fn set_collapsed(&mut self, index: usize, collapsed: bool) {
        let is_group = self.nodes.get(index).is_some_and(|node| node.is_group);
        if !is_group {
            return;
        }
        if let Some(slot) = self.collapsed.get_mut(index) {
            if *slot != collapsed {
                *slot = collapsed;
                self.rebuild_keeping_cursor();
            }
        }
    }

    /// Open every key.
    pub fn expand_all(&mut self) {
        self.collapsed.iter_mut().for_each(|slot| *slot = false);
        self.rebuild_keeping_cursor();
    }

    /// Close every key.
    pub fn collapse_all(&mut self) {
        for (slot, node) in self.collapsed.iter_mut().zip(self.nodes.iter()) {
            *slot = node.is_group;
        }
        self.rebuild_keeping_cursor();
    }

    /// The row on screen the cursor sits on.
    #[must_use]
    pub const fn cursor(&self) -> usize {
        self.cursor
    }

    /// Put the cursor on `row`, held inside the rows on screen.
    pub fn set_cursor(&mut self, row: usize) {
        self.cursor = row.min(self.visible.len().saturating_sub(1));
    }

    /// Move the cursor by `delta` rows.
    pub fn move_cursor(&mut self, delta: i64) {
        let magnitude = usize::try_from(delta.unsigned_abs()).unwrap_or(usize::MAX);
        let row = if delta < 0 {
            self.cursor.saturating_sub(magnitude)
        } else {
            self.cursor.saturating_add(magnitude)
        };
        self.set_cursor(row);
    }

    /// The row on screen that holds the key of the cursor row.
    #[must_use]
    pub fn parent_row(&self, row: usize) -> Option<usize> {
        let parent = self.node_at(row)?.parent?;
        self.visible.binary_search(&parent).ok()
    }

    /// True when the row on screen at `row` is a place a difference search
    /// stops: a differing value, an orphan key, or a closed key holding a
    /// difference.
    #[must_use]
    pub fn is_stop(&self, row: usize) -> bool {
        let Some(index) = self.index_at(row) else {
            return false;
        };
        let Some(node) = self.nodes.get(index) else {
            return false;
        };
        node.class(self.ignore_unimportant) != RecordClass::Same
            && (!node.is_group || node.status.is_orphan() || self.is_collapsed(index))
    }

    /// The first stop at or after `from`.
    #[must_use]
    pub fn next_difference(&self, from: usize) -> Option<usize> {
        (from..self.visible.len()).find(|row| self.is_stop(*row))
    }

    /// The last stop at or before `from`.
    #[must_use]
    pub fn previous_difference(&self, from: usize) -> Option<usize> {
        let last = from.min(self.visible.len().checked_sub(1)?);
        (0..=last).rev().find(|row| self.is_stop(*row))
    }

    /// Counts over every value row, whatever the filter shows.
    #[must_use]
    pub fn counts(&self) -> Counts {
        let mut counts = Counts::default();
        for node in self.nodes.iter().filter(|node| !node.is_group) {
            let slot = match node.status {
                Status::Same => &mut counts.same,
                Status::LeftOnly => &mut counts.left_only,
                Status::RightOnly => &mut counts.right_only,
                Status::Different if node.importance == Importance::Unimportant => {
                    &mut counts.unimportant
                }
                Status::Different => &mut counts.different,
            };
            *slot = slot.saturating_add(1);
        }
        counts
    }

    /// Counts over every key row, whatever the filter shows.
    #[must_use]
    pub fn key_counts(&self) -> KeyCounts {
        let mut counts = KeyCounts::default();
        for node in self.nodes.iter().filter(|node| node.is_group) {
            let slot = match node.status {
                Status::Same => &mut counts.same,
                Status::LeftOnly => &mut counts.left_only,
                Status::RightOnly => &mut counts.right_only,
                Status::Different if node.importance == Importance::Unimportant => {
                    &mut counts.unimportant
                }
                Status::Different => &mut counts.different,
            };
            *slot = slot.saturating_add(1);
        }
        counts
    }

    /// Every value row in listing order, as a record report reads it.
    ///
    /// The report dialog applies its own display filter, so every value is
    /// handed over.
    #[must_use]
    pub fn report_rows(&self) -> Vec<RecordRow> {
        self.nodes
            .iter()
            .filter(|node| !node.is_group)
            .map(|node| RecordRow {
                name: node.name.clone(),
                group: (!node.path.is_empty()).then(|| node.path.clone()),
                kind: match node.status {
                    Status::Same => RowKind::Same,
                    Status::Different => RowKind::Changed,
                    Status::LeftOnly => RowKind::LeftOnly,
                    Status::RightOnly => RowKind::RightOnly,
                },
                importance: (node.status == Status::Different).then_some(match node.importance {
                    Importance::Unimportant => ReportImportance::Unimportant,
                    Importance::Important => ReportImportance::Important,
                }),
                left: node.left.as_ref().map(|record| record.display.clone()),
                right: node.right.as_ref().map(|record| record.display.clone()),
            })
            .collect()
    }

    fn keeps(&self, node: &Node) -> bool {
        let class = node.class(self.ignore_unimportant);
        match self.filter {
            DisplayFilter::All => true,
            DisplayFilter::None => false,
            DisplayFilter::Differences => class != RecordClass::Same,
            DisplayFilter::Same => class == RecordClass::Same && !node.status.is_orphan(),
        }
    }

    fn rebuild_keeping_cursor(&mut self) {
        let held = self.index_at(self.cursor);
        self.rebuild(held);
    }

    /// Work out the rows on screen, and put the cursor back on the arena row
    /// `held` or on the nearest row still shown.
    fn rebuild(&mut self, held: Option<usize>) {
        let count = self.nodes.len();
        let mut keep: Vec<bool> = self.nodes.iter().map(|node| self.keeps(node)).collect();
        // A key stays when anything under it stays, so its rows keep a place
        // to hang from.
        for index in (0..count).rev() {
            if keep[index] {
                if let Some(parent) = self.nodes[index].parent {
                    keep[parent] = true;
                }
            }
        }
        let mut visible = Vec::new();
        let mut index = 0;
        while index < count {
            let node = &self.nodes[index];
            if !keep[index] {
                index = if node.is_group { node.end } else { index + 1 };
                continue;
            }
            visible.push(index);
            index = if node.is_group && self.is_collapsed(index) {
                node.end
            } else {
                index + 1
            };
        }
        self.visible = visible;
        let Some(mut target) = held else {
            self.set_cursor(0);
            return;
        };
        loop {
            if let Ok(row) = self.visible.binary_search(&target) {
                self.set_cursor(row);
                return;
            }
            match self.nodes.get(target).and_then(|node| node.parent) {
                Some(parent) => target = parent,
                None => break,
            }
        }
        let row = self.visible.partition_point(|shown| *shown < target);
        self.set_cursor(row);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{flatten, DisplayFilter, Listing, Side};
    use ca_records::compare::compare_trees;
    use ca_records::{AlignOptions, Record, RecordTree, RecordValue};
    use ca_ui::theme::records::RecordClass;
    use std::sync::Arc;

    fn value(path: &str, name: &str, text: &str) -> Record {
        Record::new(path, name, "REG_SZ", RecordValue::Text(text.to_owned()))
    }

    /// Two trees: `A` holds one change and one orphan, `B` matches, `C` is on
    /// the left only.
    fn sample() -> Listing {
        let mut left = RecordTree::new("", "root");
        let mut right = RecordTree::new("", "root");
        let mut a_left = RecordTree::new("A", "A");
        a_left.push(value("A", "Changed", "one"));
        a_left.push(value("A", "Kept", "same"));
        a_left.push(value("A", "Gone", "x"));
        let mut a_right = RecordTree::new("A", "A");
        a_right.push(value("A", "Changed", "two"));
        a_right.push(value("A", "Kept", "same"));
        let mut b = RecordTree::new("B", "B");
        b.push(value("B", "Same", "v"));
        let mut inner = RecordTree::new("B\\Inner", "Inner");
        inner.push(value("B\\Inner", "Deep", "d"));
        b.push_child(inner);
        let mut c = RecordTree::new("C", "C");
        c.push(value("C", "Only", "o"));
        left.push_child(a_left);
        left.push_child(b.clone());
        left.push_child(c);
        right.push_child(a_right);
        right.push_child(b);
        let diff = compare_trees(&left, &right, &AlignOptions::default());
        Listing::new(Arc::new(flatten(diff)))
    }

    fn names(listing: &Listing) -> Vec<String> {
        (0..listing.rows())
            .map(|row| listing.node_at(row).unwrap().name.clone())
            .collect()
    }

    #[test]
    fn keys_come_before_their_values_and_roll_up_their_status() {
        let listing = sample();
        assert_eq!(
            names(&listing),
            ["A", "Changed", "Gone", "Kept", "B", "Same", "Inner", "Deep", "C", "Only"]
        );
        assert_eq!(listing.class_at(0), Some(RecordClass::Different));
        assert_eq!(listing.class_at(4), Some(RecordClass::Same));
        assert_eq!(listing.class_at(8), Some(RecordClass::Orphan));
        let c = listing.node_at(8).unwrap();
        assert!(c.is_on(Side::Left));
        assert!(!c.is_on(Side::Right));
    }

    #[test]
    fn a_key_holding_only_an_orphan_is_painted_as_an_orphan() {
        let mut left = RecordTree::new("", "root");
        let mut right = RecordTree::new("", "root");
        let mut key = RecordTree::new("K", "K");
        key.push(value("K", "Both", "v"));
        right.push_child(key.clone());
        key.push(value("K", "Extra", "e"));
        left.push_child(key);
        let diff = compare_trees(&left, &right, &AlignOptions::default());
        let listing = Listing::new(Arc::new(flatten(diff)));
        assert_eq!(listing.class_at(0), Some(RecordClass::Orphan));
    }

    #[test]
    fn the_difference_filter_keeps_the_keys_that_hold_a_difference() {
        let mut listing = sample();
        listing.set_filter(DisplayFilter::Differences);
        assert_eq!(names(&listing), ["A", "Changed", "Gone", "C", "Only"]);
        listing.set_filter(DisplayFilter::Same);
        assert_eq!(names(&listing), ["A", "Kept", "B", "Same", "Inner", "Deep"]);
        listing.set_filter(DisplayFilter::None);
        assert_eq!(listing.rows(), 0);
        assert!(listing.next_difference(0).is_none());
    }

    #[test]
    fn collapsing_a_key_hides_its_subtree_and_keeps_the_cursor_on_it() {
        let mut listing = sample();
        listing.set_cursor(7);
        assert_eq!(listing.node_at(7).unwrap().name, "Deep");
        listing.collapse(4);
        assert_eq!(
            names(&listing),
            ["A", "Changed", "Gone", "Kept", "B", "C", "Only"]
        );
        assert_eq!(listing.cursor(), 4);
        listing.expand_all();
        assert_eq!(listing.rows(), 10);
        listing.collapse_all();
        assert_eq!(names(&listing), ["A", "B", "C"]);
        assert!(
            listing.is_stop(0),
            "a closed key holding a change is a stop"
        );
        assert!(!listing.is_stop(1));
    }

    #[test]
    fn the_difference_search_walks_values_and_orphans() {
        let listing = sample();
        assert_eq!(listing.next_difference(0), Some(1));
        assert_eq!(listing.next_difference(2), Some(2));
        assert_eq!(listing.next_difference(3), Some(8));
        assert_eq!(listing.previous_difference(7), Some(2));
        assert_eq!(listing.previous_difference(0), None);
    }

    #[test]
    fn the_report_rows_carry_every_value() {
        let listing = sample();
        let rows = listing.report_rows();
        assert_eq!(rows.len(), 6);
        assert_eq!(rows[0].group.as_deref(), Some("A"));
        assert_eq!(rows[0].left.as_deref(), Some("one"));
        assert_eq!(rows[0].right.as_deref(), Some("two"));
        let counts = listing.counts();
        assert_eq!(counts.different, 1);
        assert_eq!(counts.left_only, 2);
        assert_eq!(counts.same, 3);
    }

    #[test]
    fn empty_orphan_keys_are_counted_separately_from_values() {
        let mut left = RecordTree::new("", "root");
        let right = RecordTree::new("", "root");
        left.push_child(RecordTree::new("OnlyLeft", "OnlyLeft"));
        let diff = compare_trees(&left, &right, &AlignOptions::default());
        let listing = Listing::new(Arc::new(flatten(diff)));

        assert_eq!(listing.counts(), super::Counts::default());
        assert_eq!(listing.key_counts().left_only, 1);
    }

    #[test]
    fn a_deep_tree_flattens_without_recursion() {
        let mut node = RecordTree::new("leaf", "leaf");
        node.push(value("leaf", "v", "1"));
        for depth in 0..1_000 {
            let mut parent = RecordTree::new(format!("k{depth}"), format!("k{depth}"));
            parent.push_child(node);
            node = parent;
        }
        let mut root = RecordTree::new("", "root");
        root.push_child(node);
        let diff = compare_trees(
            &root,
            &RecordTree::new("", "root"),
            &AlignOptions::default(),
        );
        let nodes = flatten(diff);
        assert_eq!(nodes.len(), 1_002);
        assert_eq!(nodes[0].end, 1_002);
    }
}
