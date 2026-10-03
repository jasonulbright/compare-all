//! The merge itself, with no interface attached.
//!
//! The model holds three inputs, one section per merged region, the state of
//! each section and the output text those states produce. Nothing here paints,
//! reads a file or blocks, so every rule about what the output holds after an
//! action is testable without a frame.
//!
//! Lines keep their terminators unless composing two source lines would join
//! them. Those seams receive a separator. An unterminated line
//! at the end of the output stays unterminated.

use ca_diff::merge3::{merge3_cancellable, MergeKind, MergeOptions, MergeSide};
use ca_diff::{
    classify_hunks, diff_line_slices, diff_line_slices_cancellable, Cancel, HunkKind, Importance,
    LineCompareOptions, RuleSet, WhitespaceClassifier,
};
use ca_ui::theme::merge::MergeClass;
use std::collections::HashMap;
use std::ops::Range;

#[cfg(test)]
thread_local! {
    static OUTPUT_TEXT_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static TOTAL_RECOUNT_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static ROW_OFFSET_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static EDITED_OUTPUT_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static SEQUENCE_ITEMS_TOUCHED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static TAKE_LINE_ITEMS_READ: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static SECTION_OFFSET_CELLS_UPDATED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static SEQUENCE_NODES_VISITED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

const MERGE_SEQUENCE_CHUNK_SIZE: usize = 1024;

/// Per-section lengths with logarithmic prefix and point-update operations.
#[derive(Debug, Clone, Default)]
struct PrefixLengths {
    values: Vec<usize>,
    tree: Vec<usize>,
}

impl PrefixLengths {
    fn from_values(values: Vec<usize>) -> Self {
        let mut tree = vec![0usize; values.len() + 1];
        for (index, value) in values.iter().copied().enumerate() {
            let node = index + 1;
            tree[node] = tree[node].saturating_add(value);
            let parent = node.saturating_add(node & (!node + 1));
            if parent < tree.len() {
                tree[parent] = tree[parent].saturating_add(tree[node]);
            }
        }
        Self { values, tree }
    }

    fn len(&self) -> usize {
        self.values.len()
    }

    fn total(&self) -> usize {
        self.prefix(self.len())
    }

    fn prefix(&self, end: usize) -> usize {
        let mut index = end.min(self.len());
        let mut total = 0usize;
        while index > 0 {
            total = total.saturating_add(self.tree[index]);
            index &= index - 1;
        }
        total
    }

    fn range(&self, index: usize) -> Option<Range<usize>> {
        let length = *self.values.get(index)?;
        let start = self.prefix(index);
        Some(start..start.saturating_add(length))
    }

    fn set(&mut self, index: usize, value: usize) -> bool {
        let Some(previous) = self.values.get_mut(index) else {
            return false;
        };
        let old_value = *previous;
        if old_value == value {
            return true;
        }
        *previous = value;
        let mut node = index + 1;
        while node < self.tree.len() {
            self.tree[node] = if value >= old_value {
                self.tree[node].saturating_add(value - old_value)
            } else {
                self.tree[node].saturating_sub(old_value - value)
            };
            #[cfg(test)]
            SECTION_OFFSET_CELLS_UPDATED.with(|count| count.set(count.get().saturating_add(1)));
            node = node.saturating_add(node & (!node + 1));
        }
        true
    }

    fn index_at(&self, position: usize) -> Option<usize> {
        if position >= self.total() {
            return None;
        }
        let mut bit = 1usize;
        while bit <= self.len() / 2 {
            bit <<= 1;
        }
        let mut index = 0usize;
        let mut total = 0usize;
        while bit > 0 {
            let next = index.saturating_add(bit);
            if next <= self.len() {
                let candidate = total.saturating_add(self.tree[next]);
                if candidate <= position {
                    index = next;
                    total = candidate;
                }
            }
            bit >>= 1;
        }
        (index < self.len()).then_some(index)
    }
}

/// Which of the four panes a row or an action names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Pane {
    /// One of the two changed versions.
    Left,
    /// The common ancestor.
    Center,
    /// The other changed version.
    Right,
    /// The editable result.
    Output,
}

impl Pane {
    /// The four panes, top row first.
    pub const ALL: [Self; 4] = [Self::Left, Self::Center, Self::Right, Self::Output];

    /// The three inputs, left to right.
    pub const INPUTS: [Self; 3] = [Self::Left, Self::Center, Self::Right];

    /// The name a pane header shows.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Left => "Left",
            Self::Center => "Center",
            Self::Right => "Right",
            Self::Output => "Output",
        }
    }
}

/// Where one section's output content comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// The section needs review and holds the baseline content.
    Unresolved,
    /// The left version.
    Left,
    /// The common ancestor.
    Center,
    /// The right version.
    Right,
    /// Both versions, left first.
    LeftThenRight,
    /// Both versions, right first.
    RightThenLeft,
    /// Text the user typed.
    Edited,
}

impl Resolution {
    /// True when the left version is part of the output here.
    #[must_use]
    pub const fn holds_left(self) -> bool {
        matches!(self, Self::Left | Self::LeftThenRight | Self::RightThenLeft)
    }

    /// True when the right version is part of the output here.
    #[must_use]
    pub const fn holds_right(self) -> bool {
        matches!(
            self,
            Self::Right | Self::LeftThenRight | Self::RightThenLeft
        )
    }

    /// The same decision with the left and right inputs exchanged.
    #[must_use]
    pub const fn mirrored(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
            Self::LeftThenRight => Self::RightThenLeft,
            Self::RightThenLeft => Self::LeftThenRight,
            other => other,
        }
    }

    /// The name the status bar shows.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Unresolved => "unresolved conflict",
            Self::Left => "taken from the left",
            Self::Center => "taken from the center",
            Self::Right => "taken from the right",
            Self::LeftThenRight => "taken left then right",
            Self::RightThenLeft => "taken right then left",
            Self::Edited => "edited",
        }
    }
}

/// How the lines of one section stand in the merge.
///
/// The display filters select on this status rather than on the colors, so a
/// filter keeps a line whatever the output pane now holds for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineStatus {
    /// Neither side changed the ancestor, or the change is not counted.
    Unchanged,
    /// Both sides made the same change.
    SameChange,
    /// Only the left side changed.
    LeftChange,
    /// Only the right side changed.
    RightChange,
    /// Both sides changed, differently, and the lines do not wait for review.
    DifferentChange,
    /// The lines wait for review.
    Conflict,
}

/// Which differences the view counts as no difference at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct DisplayRules {
    /// A section whose every change the importance rules call unimportant
    /// counts as unchanged.
    pub ignore_unimportant: bool,
    /// A change both sides made the same way counts as unchanged.
    pub ignore_same_changes: bool,
    /// The output pane paints a change only the left side made as unchanged.
    /// Counting, filters and navigation do not read it.
    pub favor_left: bool,
    /// The output pane paints a change only the right side made as unchanged.
    /// Counting, filters and navigation do not read it.
    pub favor_right: bool,
}

/// A section as one undo step recorded it, with the output lines it covered.
#[derive(Debug, Clone)]
pub(crate) struct HistorySection {
    pub(crate) index: usize,
    pub(crate) output: Range<u32>,
    pub(crate) section: Section,
}

/// One merged region across the three inputs and the output.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct Section {
    /// What the merge made of this region.
    pub kind: MergeKind,
    /// Line range in the left input.
    pub left: Range<u32>,
    /// Line range in the common ancestor.
    pub center: Range<u32>,
    /// Line range in the right input.
    pub right: Range<u32>,
    /// Where the output content comes from.
    pub resolution: Resolution,
    /// The typed text, when the resolution is [`Resolution::Edited`].
    pub edited: Vec<String>,
    edited_from_output: bool,
    /// Number of output lines contributed by this section.
    pub output_len: u32,
    /// True while the section waits for review. The engine sets it on a
    /// conflict; a take clears it and the Conflict command sets or clears it.
    pub conflict: bool,
    /// True when the user excluded this section's differences.
    pub ignored: bool,
    /// True when the importance rules call every change here unimportant.
    pub unimportant: bool,
    /// True when the display rules or the ignored mark count this section as
    /// unchanged. Derived from the fields above and the model's rules.
    suppressed: bool,
    /// Leading lines this section gave to the edited text of an earlier
    /// section, oldest first. A take that replaces the holder's text gives
    /// them back; without them that take drops this section's text.
    lent: Vec<Lent>,
    /// The last later section that lent lines to this section's text.
    joined_through: Option<usize>,
}

/// Lines one section lent to the edited text of an earlier section.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Lent {
    holder: usize,
    lines: Vec<String>,
}

impl Section {
    fn new(kind: MergeKind, left: Range<u32>, center: Range<u32>, right: Range<u32>) -> Self {
        Self {
            kind,
            left,
            center,
            right,
            resolution: automatic(kind),
            edited: Vec::new(),
            edited_from_output: false,
            output_len: 0,
            conflict: matches!(kind, MergeKind::Conflict),
            ignored: false,
            unimportant: false,
            suppressed: false,
            lent: Vec::new(),
            joined_through: None,
        }
    }

    /// True when an edit moved lines of later sections into this section's
    /// text, so a take here gives them back to those sections.
    #[must_use]
    pub fn holds_joined_lines(&self) -> bool {
        self.joined_through.is_some()
    }

    /// True when this section still needs review.
    #[must_use]
    pub const fn is_unresolved_conflict(&self) -> bool {
        self.conflict && !self.suppressed
    }

    /// True when the display counts this section's differences as none.
    #[must_use]
    pub const fn is_suppressed(&self) -> bool {
        self.suppressed
    }

    /// True when either side changed the ancestor here and the change counts,
    /// or the section waits for review.
    #[must_use]
    pub const fn is_difference(&self) -> bool {
        (!matches!(self.kind, MergeKind::Unchanged) && !self.suppressed)
            || self.is_unresolved_conflict()
    }

    /// How the lines of this section stand.
    #[must_use]
    pub const fn status(&self) -> LineStatus {
        if self.is_unresolved_conflict() {
            return LineStatus::Conflict;
        }
        if self.suppressed {
            return LineStatus::Unchanged;
        }
        match self.kind {
            MergeKind::Unchanged => LineStatus::Unchanged,
            MergeKind::SameChange => LineStatus::SameChange,
            MergeKind::LeftChange => LineStatus::LeftChange,
            MergeKind::RightChange => LineStatus::RightChange,
            MergeKind::Conflict => LineStatus::DifferentChange,
        }
    }

    /// The class the three input panes paint this section in.
    #[must_use]
    pub const fn input_class(&self) -> MergeClass {
        if self.is_unresolved_conflict() {
            return MergeClass::Conflict;
        }
        if self.suppressed {
            return MergeClass::Unchanged;
        }
        match self.kind {
            MergeKind::Unchanged => MergeClass::Unchanged,
            MergeKind::LeftChange => MergeClass::LeftChange,
            MergeKind::RightChange => MergeClass::RightChange,
            MergeKind::SameChange => MergeClass::SameChange,
            MergeKind::Conflict => MergeClass::Conflict,
        }
    }

    /// The class the output pane paints this section in.
    ///
    /// The output follows where its content came from rather than what the
    /// merge found, so a resolved conflict stops reading as a conflict.
    #[must_use]
    pub const fn output_class(&self) -> MergeClass {
        if self.is_unresolved_conflict() {
            return MergeClass::Conflict;
        }
        if matches!(self.resolution, Resolution::Edited) {
            return MergeClass::Edited;
        }
        if self.suppressed || matches!(self.kind, MergeKind::Unchanged) {
            return MergeClass::Unchanged;
        }
        match self.resolution {
            Resolution::Left => MergeClass::LeftChange,
            Resolution::Right => MergeClass::RightChange,
            _ => MergeClass::SameChange,
        }
    }

    /// The class the output pane paints this section in under the favor
    /// switches of `rules`.
    #[must_use]
    pub const fn output_class_under(&self, rules: DisplayRules) -> MergeClass {
        let class = self.output_class();
        let left_only =
            matches!(class, MergeClass::LeftChange) && matches!(self.kind, MergeKind::LeftChange);
        let right_only =
            matches!(class, MergeClass::RightChange) && matches!(self.kind, MergeKind::RightChange);
        if (left_only && rules.favor_left) || (right_only && rules.favor_right) {
            return MergeClass::Unchanged;
        }
        class
    }

    /// The line range this section covers in one pane. An output range is
    /// relative to this section; use [`MergeModel::output_range`] for its
    /// absolute position in the full output.
    #[must_use]
    pub fn range(&self, pane: Pane) -> Range<u32> {
        match pane {
            Pane::Left => self.left.clone(),
            Pane::Center => self.center.clone(),
            Pane::Right => self.right.clone(),
            Pane::Output => 0..self.output_len,
        }
    }

    fn refresh(&mut self, rules: DisplayRules) {
        self.suppressed = self.ignored
            || (rules.ignore_unimportant && self.unimportant)
            || (rules.ignore_same_changes && matches!(self.kind, MergeKind::SameChange));
    }
}

/// One aligned row across the four panes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    /// The section this row belongs to.
    pub section: u32,
    /// Left input line, or a gap.
    pub left: Option<u32>,
    /// Ancestor line, or a gap.
    pub center: Option<u32>,
    /// Right input line, or a gap.
    pub right: Option<u32>,
}

impl Row {
    /// The line one pane shows on this row.
    #[must_use]
    pub const fn line(&self, pane: Pane, output_line: Option<u32>) -> Option<u32> {
        match pane {
            Pane::Left => self.left,
            Pane::Center => self.center,
            Pane::Right => self.right,
            Pane::Output => output_line,
        }
    }
}

/// What the status bar counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Totals {
    /// Sections the merge found a difference in.
    pub differences: u32,
    /// Sections the merge could not resolve on its own.
    pub conflicts: u32,
    /// Conflicts still waiting for review.
    pub conflicts_remaining: u32,
    /// Sections whose output holds the left version.
    pub taken_left: u32,
    /// Sections whose output holds the right version.
    pub taken_right: u32,
    /// Sections whose output holds the ancestor although a side changed it.
    pub taken_center: u32,
    /// Sections the user typed into.
    pub edited: u32,
}

/// The three inputs a merge reads.
#[derive(Debug, Clone, Default)]
pub struct Inputs {
    /// The left version, lines with their terminators.
    pub left: Vec<String>,
    /// The common ancestor, empty in a two way merge.
    pub center: Vec<String>,
    /// The right version.
    pub right: Vec<String>,
    /// True when no ancestor was supplied.
    pub two_way: bool,
}

impl Inputs {
    /// The lines of one input pane.
    #[must_use]
    pub fn lines(&self, pane: Pane) -> &[String] {
        match pane {
            Pane::Left => &self.left,
            Pane::Center => &self.center,
            Pane::Right => &self.right,
            Pane::Output => &[],
        }
    }
}

#[derive(Debug, Clone)]
struct SequenceNode<T> {
    priority: u64,
    left: Option<Box<Self>>,
    items: Vec<T>,
    right: Option<Box<Self>>,
    len: usize,
}

type SequenceTree<T> = Option<Box<SequenceNode<T>>>;

impl<T> SequenceNode<T> {
    fn new(items: Vec<T>) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT_PRIORITY: AtomicU64 = AtomicU64::new(0);

        let mut priority = NEXT_PRIORITY
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(0x9e37_79b9_7f4a_7c15);
        priority = (priority ^ (priority >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        priority = (priority ^ (priority >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        priority ^= priority >> 31;
        let len = items.len();
        Self {
            priority,
            left: None,
            items,
            right: None,
            len,
        }
    }

    fn refresh(&mut self) {
        self.len = node_len(self.left.as_deref())
            .saturating_add(self.items.len())
            .saturating_add(node_len(self.right.as_deref()));
    }
}

fn node_len<T>(node: Option<&SequenceNode<T>>) -> usize {
    node.map_or(0, |node| node.len)
}

fn merge_nodes<T>(left: SequenceTree<T>, right: SequenceTree<T>) -> SequenceTree<T> {
    #[cfg(test)]
    SEQUENCE_NODES_VISITED.with(|count| count.set(count.get().saturating_add(1)));
    match (left, right) {
        (None, tree) | (tree, None) => tree,
        (Some(mut left), Some(right)) if left.priority >= right.priority => {
            left.right = merge_nodes(left.right.take(), Some(right));
            left.refresh();
            Some(left)
        }
        (Some(left), Some(mut right)) => {
            right.left = merge_nodes(Some(left), right.left.take());
            right.refresh();
            Some(right)
        }
    }
}

fn split_nodes<T>(mut tree: SequenceTree<T>, index: usize) -> (SequenceTree<T>, SequenceTree<T>) {
    #[cfg(test)]
    SEQUENCE_NODES_VISITED.with(|count| count.set(count.get().saturating_add(1)));
    let Some(mut node) = tree.take() else {
        return (None, None);
    };
    let left_len = node_len(node.left.as_deref());
    let items_end = left_len.saturating_add(node.items.len());
    if index < left_len {
        let (before, after) = split_nodes(node.left.take(), index);
        node.left = after;
        node.refresh();
        (before, Some(node))
    } else if index > items_end {
        let (before, after) = split_nodes(node.right.take(), index - items_end);
        node.right = before;
        node.refresh();
        (Some(node), after)
    } else if index == left_len {
        let before = node.left.take();
        node.refresh();
        (before, Some(node))
    } else if index == items_end {
        let after = node.right.take();
        node.refresh();
        (Some(node), after)
    } else {
        let local = index - left_len;
        let suffix = node.items.split_off(local);
        let prefix = std::mem::take(&mut node.items);
        #[cfg(test)]
        SEQUENCE_ITEMS_TOUCHED.with(|items| {
            items.set(
                items
                    .get()
                    .saturating_add(prefix.len())
                    .saturating_add(suffix.len()),
            );
        });
        let left = merge_nodes(node.left.take(), Some(Box::new(SequenceNode::new(prefix))));
        let right = merge_nodes(Some(Box::new(SequenceNode::new(suffix))), node.right.take());
        (left, right)
    }
}

fn pop_last<T>(mut tree: Box<SequenceNode<T>>) -> (SequenceTree<T>, Vec<T>) {
    #[cfg(test)]
    SEQUENCE_NODES_VISITED.with(|count| count.set(count.get().saturating_add(1)));
    if let Some(right) = tree.right.take() {
        let (right, items) = pop_last(right);
        tree.right = right;
        tree.refresh();
        (Some(tree), items)
    } else {
        (tree.left.take(), tree.items)
    }
}

fn pop_first<T>(mut tree: Box<SequenceNode<T>>) -> (Vec<T>, SequenceTree<T>) {
    #[cfg(test)]
    SEQUENCE_NODES_VISITED.with(|count| count.set(count.get().saturating_add(1)));
    if let Some(left) = tree.left.take() {
        let (items, left) = pop_first(left);
        tree.left = left;
        tree.refresh();
        (items, Some(tree))
    } else {
        (tree.items, tree.right.take())
    }
}

fn join_nodes<T>(left: SequenceTree<T>, right: SequenceTree<T>) -> SequenceTree<T> {
    match (left, right) {
        (None, tree) | (tree, None) => tree,
        (Some(left), Some(right)) => {
            let left_edge = rightmost_items(&left);
            let right_edge = leftmost_items(&right);
            if left_edge.saturating_add(right_edge) <= MERGE_SEQUENCE_CHUNK_SIZE {
                let (left, mut items) = pop_last(left);
                let (right_items, right) = pop_first(right);
                #[cfg(test)]
                SEQUENCE_ITEMS_TOUCHED.with(|count| {
                    count.set(
                        count
                            .get()
                            .saturating_add(items.len())
                            .saturating_add(right_items.len()),
                    );
                });
                items.extend(right_items);
                merge_nodes(
                    merge_nodes(left, Some(Box::new(SequenceNode::new(items)))),
                    right,
                )
            } else {
                merge_nodes(Some(left), Some(right))
            }
        }
    }
}

fn rightmost_items<T>(tree: &SequenceNode<T>) -> usize {
    let mut node = tree;
    while let Some(right) = node.right.as_deref() {
        #[cfg(test)]
        SEQUENCE_NODES_VISITED.with(|count| count.set(count.get().saturating_add(1)));
        node = right;
    }
    node.items.len()
}

fn leftmost_items<T>(tree: &SequenceNode<T>) -> usize {
    let mut node = tree;
    while let Some(left) = node.left.as_deref() {
        #[cfg(test)]
        SEQUENCE_NODES_VISITED.with(|count| count.set(count.get().saturating_add(1)));
        node = left;
    }
    node.items.len()
}

/// A line or row sequence with expected logarithmic insertion and lookup.
#[derive(Debug, Clone)]
pub struct ChunkedVec<T> {
    root: Option<Box<SequenceNode<T>>>,
    len: usize,
}

impl<T> Default for ChunkedVec<T> {
    fn default() -> Self {
        Self { root: None, len: 0 }
    }
}

impl<T> ChunkedVec<T> {
    fn from_vec(values: Vec<T>) -> Self {
        let mut values = values.into_iter();
        let mut result = Self::default();
        loop {
            let items: Vec<T> = values.by_ref().take(MERGE_SEQUENCE_CHUNK_SIZE).collect();
            if items.is_empty() {
                break;
            }
            result.root = join_nodes(result.root, Some(Box::new(SequenceNode::new(items))));
        }
        result.len = node_len(result.root.as_deref());
        result
    }

    /// Number of lines or rows in the sequence.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether the sequence has no lines or rows.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The line or row at a flat sequence index.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&T> {
        let mut node = self.root.as_deref()?;
        if index >= self.len {
            return None;
        }
        let mut index = index;
        loop {
            let left_len = node_len(node.left.as_deref());
            if index < left_len {
                node = node.left.as_deref()?;
            } else if index < left_len + node.items.len() {
                return node.items.get(index - left_len);
            } else {
                index -= left_len + node.items.len();
                node = node.right.as_deref()?;
            }
        }
    }

    /// Iterate over every line or row in sequence order.
    pub fn iter(&self) -> impl Iterator<Item = &T> + '_ {
        ChunkedIter::new(self.root.as_deref())
    }

    fn get_mut(&mut self, index: usize) -> Option<&mut T> {
        if index >= self.len {
            return None;
        }
        let mut node = self.root.as_deref_mut()?;
        let mut index = index;
        loop {
            let left_len = node_len(node.left.as_deref());
            if index < left_len {
                node = node.left.as_deref_mut()?;
            } else if index < left_len + node.items.len() {
                return node.items.get_mut(index - left_len);
            } else {
                index -= left_len + node.items.len();
                node = node.right.as_deref_mut()?;
            }
        }
    }

    fn copy_range(&self, range: Range<usize>) -> Vec<T>
    where
        T: Clone,
    {
        let start = range.start.min(self.len);
        let end = range.end.min(self.len).max(start);
        (start..end)
            .filter_map(|index| self.get(index).cloned())
            .collect()
    }

    /// Replace a flat range with new lines or rows.
    fn splice(&mut self, range: Range<usize>, replacement: Vec<T>) {
        debug_assert!(range.start <= range.end);
        debug_assert!(range.end <= self.len);
        let (before, tail) = split_nodes(self.root.take(), range.start);
        let (removed, after) = split_nodes(tail, range.end - range.start);
        drop(removed);
        #[cfg(test)]
        SEQUENCE_ITEMS_TOUCHED.with(|items| {
            items.set(items.get().saturating_add(replacement.len()));
        });
        let inserted = Self::from_vec(replacement);
        self.root = join_nodes(join_nodes(before, inserted.root), after);
        self.len = node_len(self.root.as_deref());
    }
}

struct ChunkedIter<'a, T> {
    stack: Vec<&'a SequenceNode<T>>,
    items: Option<std::slice::Iter<'a, T>>,
}

impl<'a, T> ChunkedIter<'a, T> {
    fn new(root: Option<&'a SequenceNode<T>>) -> Self {
        let mut result = Self {
            stack: Vec::new(),
            items: None,
        };
        result.push_left(root);
        result
    }

    fn push_left(&mut self, mut node: Option<&'a SequenceNode<T>>) {
        while let Some(current) = node {
            self.stack.push(current);
            node = current.left.as_deref();
        }
    }
}

impl<'a, T> Iterator for ChunkedIter<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(item) = self.items.as_mut().and_then(Iterator::next) {
                return Some(item);
            }
            let node = self.stack.pop()?;
            self.items = Some(node.items.iter());
            self.push_left(node.right.as_deref());
        }
    }
}

impl<T> std::ops::Index<usize> for ChunkedVec<T> {
    type Output = T;

    #[allow(clippy::panic)]
    fn index(&self, index: usize) -> &Self::Output {
        // Index follows `Vec` by panicking when the requested position is absent.
        self.get(index)
            .unwrap_or_else(|| panic!("index {index} out of bounds"))
    }
}

impl ChunkedVec<String> {
    fn concat(&self) -> String {
        let capacity = self.iter().map(String::len).sum();
        let mut result = String::with_capacity(capacity);
        for line in self.iter() {
            result.push_str(line);
        }
        result
    }

    fn concat_range(&self, range: Range<usize>) -> String {
        let start = range.start.min(self.len);
        let end = range.end.min(self.len).max(start);
        let capacity = (start..end)
            .filter_map(|index| self.get(index))
            .map(String::len)
            .sum();
        let mut result = String::with_capacity(capacity);
        for index in start..end {
            if let Some(line) = self.get(index) {
                result.push_str(line);
            }
        }
        result
    }
}

/// A merge and the state of every one of its sections.
#[derive(Debug, Clone, Default)]
pub struct MergeModel {
    inputs: Inputs,
    sections: Vec<Section>,
    rows: ChunkedVec<Row>,
    section_rows: PrefixLengths,
    section_output: PrefixLengths,
    output: ChunkedVec<String>,
    /// Suffix bytes inserted solely to separate composed source lines.
    output_repairs: ChunkedVec<usize>,
    output_ending: Option<ca_text::EolStyle>,
    section_totals: Vec<Totals>,
    rules: DisplayRules,
    totals: Totals,
}

/// The left range, center range and right range of a section as text.
///
/// Two merges of different input files still describe the same region when all
/// three of its texts match, which is what lets a manual resolution survive a
/// reload that did not touch that region.
type Fingerprint = (String, String, String);

/// What a user decided about one section, carried across a reload.
#[derive(Debug, Clone)]
struct Decision {
    index: usize,
    resolution: Resolution,
    edited: Vec<String>,
    conflict: bool,
    ignored: bool,
    lent: Vec<Lent>,
}

/// How many sections before this one hold `print`, counting this one in.
fn next_occurrence(seen: &mut HashMap<Fingerprint, usize>, print: &Fingerprint) -> usize {
    let count = seen.entry(print.clone()).or_insert(0);
    let occurrence = *count;
    *count += 1;
    occurrence
}

fn slice(lines: &[String], range: &Range<u32>) -> Vec<String> {
    let start = range.start as usize;
    let end = (range.end as usize).min(lines.len());
    if start >= end {
        return Vec::new();
    }
    lines[start..end].to_vec()
}

fn joined(lines: &[String], range: &Range<u32>) -> String {
    slice(lines, range).concat()
}

/// The resolution the automatic merge gives a region of this kind.
const fn automatic(kind: MergeKind) -> Resolution {
    match kind {
        MergeKind::Unchanged => Resolution::Center,
        MergeKind::LeftChange | MergeKind::SameChange => Resolution::Left,
        MergeKind::RightChange => Resolution::Right,
        MergeKind::Conflict => Resolution::Unresolved,
    }
}

impl MergeModel {
    pub(crate) fn history_sections(&self, indices: &[usize]) -> Vec<HistorySection> {
        indices
            .iter()
            .filter_map(|&index| {
                let mut section = self.sections.get(index)?.clone();
                if section.edited_from_output {
                    section.edited = self.edited_content(index, &section);
                    section.edited_from_output = false;
                }
                Some(HistorySection {
                    index,
                    output: self.output_range(index)?,
                    section,
                })
            })
            .collect()
    }

    /// True while every recorded section covers the output lines it covered
    /// when recorded. After an edit moved lines between sections, a restore
    /// drops or repeats output lines.
    pub(crate) fn history_applies(&self, sections: &[HistorySection]) -> bool {
        sections
            .iter()
            .all(|entry| self.output_range(entry.index).as_ref() == Some(&entry.output))
    }

    /// Put back the sections of `restored`, leaving the state `left` recorded.
    ///
    /// Toggle Ignored and Toggle Conflict are not undo steps. A flag that
    /// differs from its value in `left` was set after that step and stays.
    pub(crate) fn restore_history_sections(
        &mut self,
        restored: &[HistorySection],
        left: &[HistorySection],
    ) {
        let mut changed = Vec::new();
        for (entry, leaving) in restored.iter().zip(left) {
            if let Some(target) = self.sections.get_mut(entry.index) {
                let mut section = entry.section.clone();
                if target.ignored != leaving.section.ignored {
                    section.ignored = target.ignored;
                }
                if target.conflict != leaving.section.conflict {
                    section.conflict = target.conflict;
                }
                *target = section;
                target.refresh(self.rules);
                changed.push(entry.index);
            }
        }
        self.rebuild_changed_sections(&changed, true);
    }
    /// Merge three inputs, taking every non-conflicting change.
    ///
    /// An empty ancestor with `two_way` set compares the two versions directly
    /// and marks every difference a conflict, because two versions alone cannot
    /// say whether a line was added on one side or removed on the other.
    ///
    /// # Errors
    ///
    /// Returns the engine's error when the comparison is cancelled or fails.
    pub fn build(
        inputs: Inputs,
        options: &MergeOptions,
        cancel: &dyn Cancel,
    ) -> Result<Self, ca_diff::DiffError> {
        let mut sections = if inputs.two_way {
            two_way_sections(&inputs, &options.compare, cancel)?
        } else {
            three_way_sections(&inputs, options, cancel)?
        };
        // Keep an insertion target when all three inputs are empty. Without
        // one, edits in the output pane have no section to absorb them into.
        if sections.is_empty() {
            sections.push(Section::new(MergeKind::Unchanged, 0..0, 0..0, 0..0));
        }
        let mut model = Self {
            output_ending: Some(input_ending(&inputs.left)),
            inputs,
            sections,
            rows: ChunkedVec::default(),
            section_rows: PrefixLengths::default(),
            section_output: PrefixLengths::default(),
            output: ChunkedVec::default(),
            output_repairs: ChunkedVec::default(),
            section_totals: Vec::new(),
            rules: DisplayRules::default(),
            totals: Totals::default(),
        };
        model.rebuild();
        Ok(model)
    }

    /// The three inputs.
    #[must_use]
    pub const fn inputs(&self) -> &Inputs {
        &self.inputs
    }

    /// Every section, in input order.
    #[must_use]
    pub fn sections(&self) -> &[Section] {
        &self.sections
    }

    /// Every aligned row.
    #[must_use]
    pub fn rows(&self) -> &ChunkedVec<Row> {
        &self.rows
    }

    /// The row range occupied by one section.
    #[must_use]
    pub fn section_row_range(&self, section: usize) -> Option<Range<usize>> {
        self.section_rows.range(section)
    }

    /// The output line range occupied by one section.
    #[must_use]
    pub fn output_range(&self, section: usize) -> Option<Range<u32>> {
        let range = self.section_output.range(section)?;
        Some(u32::try_from(range.start).ok()?..u32::try_from(range.end).ok()?)
    }

    /// The output lines, with their terminators.
    #[must_use]
    pub fn output_lines(&self) -> &ChunkedVec<String> {
        &self.output
    }

    /// The output as one string.
    #[must_use]
    pub fn output_text(&self) -> String {
        #[cfg(test)]
        OUTPUT_TEXT_CALLS.with(|calls| calls.set(calls.get().saturating_add(1)));
        self.output.concat()
    }

    /// The output text in a flat line range.
    #[must_use]
    pub fn output_text_range(&self, range: Range<u32>) -> String {
        self.output
            .concat_range(range.start as usize..range.end as usize)
    }

    #[cfg(test)]
    pub(crate) fn output_text_call_count() -> usize {
        OUTPUT_TEXT_CALLS.with(std::cell::Cell::get)
    }

    /// The lines one pane shows.
    #[must_use]
    pub fn line(&self, pane: Pane, index: usize) -> Option<&str> {
        match pane {
            Pane::Output => self.output.get(index).map(String::as_str),
            other => self.inputs.lines(other).get(index).map(String::as_str),
        }
    }

    /// Which differences the display counts as none.
    #[must_use]
    pub const fn rules(&self) -> DisplayRules {
        self.rules
    }

    /// Count differences under new display rules.
    ///
    /// The output text does not change: a suppressed section keeps what it
    /// holds, and an unresolved one keeps showing its baseline.
    pub fn set_rules(&mut self, rules: DisplayRules) {
        self.rules = rules;
        for section in &mut self.sections {
            section.refresh(rules);
        }
        self.refresh_totals();
    }

    /// What the status bar counts.
    #[must_use]
    pub fn totals(&self) -> Totals {
        self.totals
    }

    fn refresh_totals(&mut self) {
        let mut totals = Totals::default();
        self.section_totals.clear();
        self.section_totals.reserve(self.sections.len());
        for section in &self.sections {
            #[cfg(test)]
            TOTAL_RECOUNT_VISITS.with(|visits| visits.set(visits.get().saturating_add(1)));
            let contribution = totals_for(section);
            totals.add(contribution);
            self.section_totals.push(contribution);
        }
        self.totals = totals;
    }

    #[cfg(test)]
    pub(crate) fn reset_rebuild_visit_counts() {
        TOTAL_RECOUNT_VISITS.with(|visits| visits.set(0));
        ROW_OFFSET_VISITS.with(|visits| visits.set(0));
        EDITED_OUTPUT_READS.with(|reads| reads.set(0));
        SEQUENCE_ITEMS_TOUCHED.with(|items| items.set(0));
        TAKE_LINE_ITEMS_READ.with(|reads| reads.set(0));
        SECTION_OFFSET_CELLS_UPDATED.with(|count| count.set(0));
        SEQUENCE_NODES_VISITED.with(|count| count.set(0));
    }

    #[cfg(test)]
    pub(crate) fn sequence_items_touched() -> usize {
        SEQUENCE_ITEMS_TOUCHED.with(std::cell::Cell::get)
    }

    #[cfg(test)]
    pub(crate) fn take_line_items_read() -> usize {
        TAKE_LINE_ITEMS_READ.with(std::cell::Cell::get)
    }

    #[cfg(test)]
    fn section_offset_cells_updated() -> usize {
        SECTION_OFFSET_CELLS_UPDATED.with(std::cell::Cell::get)
    }

    #[cfg(test)]
    fn sequence_nodes_visited() -> usize {
        SEQUENCE_NODES_VISITED.with(std::cell::Cell::get)
    }

    #[cfg(test)]
    fn rebuild_visit_counts() -> (usize, usize) {
        (
            TOTAL_RECOUNT_VISITS.with(std::cell::Cell::get),
            ROW_OFFSET_VISITS.with(std::cell::Cell::get),
        )
    }

    #[cfg(test)]
    pub(crate) fn edited_output_read_count() -> usize {
        EDITED_OUTPUT_READS.with(std::cell::Cell::get)
    }

    /// The section a row belongs to.
    #[must_use]
    pub fn section_of_row(&self, row: usize) -> Option<usize> {
        self.rows.get(row).map(|row| row.section as usize)
    }

    /// The first row of a section.
    #[must_use]
    pub fn row_of_section(&self, section: usize) -> Option<usize> {
        let rows = self.section_rows.range(section)?;
        (!rows.is_empty()).then_some(rows.start)
    }

    /// The absolute output line shown on one model row.
    #[must_use]
    pub fn output_line_for_row(&self, row: usize) -> Option<u32> {
        let entry = self.rows.get(row)?;
        let section = entry.section as usize;
        let first_row = self.section_rows.range(section)?.start;
        let output_start = self.output_range(section)?.start;
        let offset = u32::try_from(row.checked_sub(first_row)?).ok()?;
        let line = output_start.checked_add(offset)?;
        (line < self.output_range(section)?.end).then_some(line)
    }

    /// The row an output line is drawn on.
    #[must_use]
    pub fn row_of_output_line(&self, line: u32) -> Option<usize> {
        let section = self.section_of_output_line(line)?;
        let first = self.section_rows.range(section)?.start;
        let step = line.checked_sub(self.output_range(section)?.start)?;
        let row = first + step as usize;
        (row < self.section_rows.range(section)?.end).then_some(row)
    }

    /// The section holding an output line.
    #[must_use]
    pub fn section_of_output_line(&self, line: u32) -> Option<usize> {
        self.section_output
            .index_at(line as usize)
            // A caret past the last line belongs to the last section, so an
            // edit at the end of the output has somewhere to land.
            .or_else(|| self.sections.len().checked_sub(1))
    }

    /// The resolution a section of this model takes on load.
    ///
    /// A two way merge has no ancestor, so its unchanged regions come from the
    /// left version rather than from an empty ancestor.
    fn automatic_for(&self, kind: MergeKind) -> Resolution {
        if self.inputs.two_way && matches!(kind, MergeKind::Unchanged) {
            Resolution::Left
        } else {
            automatic(kind)
        }
    }

    /// Set the resolution of one section.
    ///
    /// Taking a side again on a section that was typed into restores it from
    /// that input, so a take is always a return to known content. A take also
    /// ends the section's wait for review.
    pub fn set_resolution(&mut self, section: usize, resolution: Resolution) {
        let decides = !matches!(resolution, Resolution::Unresolved);
        self.set_resolutions_impl(&[section], resolution, decides);
    }

    /// Resolve several sections with one output rebuild.
    pub fn set_resolutions(&mut self, sections: &[usize], resolution: Resolution) {
        self.set_resolutions_impl(
            sections,
            resolution,
            !matches!(resolution, Resolution::Unresolved),
        );
    }

    fn set_resolutions_impl(&mut self, sections: &[usize], resolution: Resolution, decides: bool) {
        let mut changed = false;
        let mut changed_sections = Vec::new();
        for &section in sections {
            let Some(entry) = self.sections.get_mut(section) else {
                continue;
            };
            if entry.resolution == resolution
                && !matches!(resolution, Resolution::Edited)
                && !(decides && entry.conflict)
            {
                continue;
            }
            entry.resolution = resolution;
            if decides {
                entry.conflict = false;
            }
            if !matches!(resolution, Resolution::Edited) {
                entry.edited.clear();
                entry.edited_from_output = false;
                entry.lent.clear();
            }
            changed = true;
            changed_sections.push(section);
        }
        if changed {
            if !matches!(resolution, Resolution::Edited) {
                let returned = self.give_back_all(&changed_sections);
                changed_sections.extend(returned);
            }
            self.rebuild_changed_sections(&changed_sections, true);
        }
    }

    /// The sections a take of `sections` changes: those sections and every
    /// later section whose lent lines one of them holds.
    pub(crate) fn sections_a_take_changes(&self, sections: &[usize]) -> Vec<usize> {
        let mut all = sections.to_vec();
        all.sort_unstable();
        all.dedup();
        let taken = all.clone();
        for &holder in &taken {
            all.extend(
                self.borrowers(holder)
                    .filter(|index| taken.binary_search(index).is_err()),
            );
        }
        all.sort_unstable();
        all.dedup();
        all
    }

    /// The later sections holding lines lent to `holder`'s text.
    fn borrowers(&self, holder: usize) -> impl Iterator<Item = usize> + '_ {
        let through = self
            .sections
            .get(holder)
            .and_then(|section| section.joined_through)
            .map_or(0, |through| through.saturating_add(1))
            .min(self.sections.len());
        (holder.saturating_add(1)..through).filter(move |&index| {
            self.sections[index]
                .lent
                .iter()
                .any(|entry| entry.holder == holder)
        })
    }

    /// Give the lines lent to each of `retaken`, whose text a take just
    /// replaced, back to the sections that lent them. A lender that is itself
    /// retaken gets its whole input back and needs none. Returns the
    /// sections given lines.
    ///
    /// Reads output ranges, so it runs before the changed sections rebuild.
    fn give_back_all(&mut self, retaken: &[usize]) -> Vec<usize> {
        let mut sorted = retaken.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        let mut returned = Vec::new();
        for &holder in &sorted {
            returned.extend(
                self.borrowers(holder)
                    .filter(|index| sorted.binary_search(index).is_err()),
            );
            if let Some(section) = self.sections.get_mut(holder) {
                section.joined_through = None;
            }
        }
        returned.sort_unstable();
        returned.dedup();
        for &index in &returned {
            let entries = std::mem::take(&mut self.sections[index].lent);
            let (back, kept): (Vec<Lent>, Vec<Lent>) = entries
                .into_iter()
                .partition(|entry| sorted.binary_search(&entry.holder).is_ok());
            let mut lines: Vec<String> = back.into_iter().flat_map(|entry| entry.lines).collect();
            lines.extend(self.contribution(index, &self.sections[index]));
            let section = &mut self.sections[index];
            section.lent = kept;
            section.resolution = Resolution::Edited;
            section.edited = lines;
            section.edited_from_output = false;
        }
        returned
    }

    /// Replace one section's output with typed text.
    ///
    /// The lines are kept exactly as given, because they are the pane's own
    /// lines; no separator is added at either edge.
    pub fn set_edited(&mut self, section: usize, lines: Vec<String>) {
        if section >= self.sections.len() {
            return;
        }
        let Some(entry) = self.sections.get_mut(section) else {
            return;
        };
        entry.resolution = Resolution::Edited;
        entry.edited = lines;
        entry.edited_from_output = false;
        entry.conflict = false;
        self.rebuild_changed_sections(&[section], false);
    }

    /// Replace a section-relative range with output lines already edited in
    /// the pane. Equal-length edits touch only the changed lines.
    pub fn edit_output_range(
        &mut self,
        index: usize,
        range: Range<u32>,
        replacement: Vec<String>,
    ) -> bool {
        let Some(section) = self.sections.get(index) else {
            return false;
        };
        let Some(old_range) = self.output_range(index) else {
            return false;
        };
        let old_length = section.output_len;
        if range.start > range.end || range.end > old_length {
            return false;
        }
        let Some(start) = old_range.start.checked_add(range.start) else {
            return false;
        };
        let Some(end) = old_range.start.checked_add(range.end) else {
            return false;
        };
        let Some(new_length) = old_length
            .checked_sub(range.end.saturating_sub(range.start))
            .and_then(|length| {
                u32::try_from(replacement.len())
                    .ok()
                    .and_then(|inserted| length.checked_add(inserted))
            })
        else {
            return false;
        };
        if old_range.start.checked_add(new_length).is_none() {
            return false;
        }
        let Some(old_rows) = self.section_rows.range(index) else {
            return false;
        };
        let input_ranges = {
            let section = &self.sections[index];
            (
                section.left.clone(),
                section.center.clone(),
                section.right.clone(),
            )
        };
        let row_count = input_ranges
            .0
            .len()
            .max(input_ranges.1.len())
            .max(input_ranges.2.len())
            .max(usize::try_from(new_length).unwrap_or(usize::MAX));
        let row_delta = i64::try_from(row_count).unwrap_or(i64::MAX)
            - i64::try_from(old_rows.end.saturating_sub(old_rows.start)).unwrap_or(i64::MAX);
        let replaced_length = range.end.saturating_sub(range.start);
        if usize::try_from(replaced_length).ok() == Some(replacement.len()) {
            for (index, text) in (start as usize..end as usize).zip(replacement) {
                if let Some(line) = self.output.get_mut(index) {
                    *line = text;
                }
                if let Some(repair) = self.output_repairs.get_mut(index) {
                    *repair = 0;
                }
            }
        } else {
            self.output_repairs
                .splice(start as usize..end as usize, vec![0; replacement.len()]);
            self.output
                .splice(start as usize..end as usize, replacement);
        }
        let section = &mut self.sections[index];
        section.resolution = Resolution::Edited;
        section.edited.clear();
        section.edited_from_output = true;
        section.conflict = false;
        section.output_len = new_length;
        let _ = self.section_output.set(index, new_length as usize);

        if row_delta > 0 {
            let ordinal = u32::try_from(index).unwrap_or(u32::MAX);
            let added: Vec<Row> = (old_rows.end.saturating_sub(old_rows.start)..row_count)
                .map(|step| {
                    let step = u32::try_from(step).unwrap_or(u32::MAX);
                    Row {
                        section: ordinal,
                        left: at(&input_ranges.0, step),
                        center: at(&input_ranges.1, step),
                        right: at(&input_ranges.2, step),
                    }
                })
                .collect();
            self.rows.splice(old_rows.end..old_rows.end, added);
        } else if row_delta < 0 {
            let new_row_end = shift_usize(old_rows.end, row_delta);
            self.rows.splice(new_row_end..old_rows.end, Vec::new());
        }
        if row_delta != 0 {
            let new_row_end = shift_usize(old_rows.end, row_delta);
            let _ = self.section_rows.set(index, new_row_end - old_rows.start);
        }
        let previous = self.section_totals[index];
        let current = totals_for(&self.sections[index]);
        self.totals.replace(previous, current);
        self.section_totals[index] = current;
        true
    }

    /// Replace output lines `first..last` with `lines`, the pane's lines in
    /// their place after an edit that reached more than one section. Returns
    /// the section that holds the changed lines.
    ///
    /// Lines equal at either end of the window stay with the sections that
    /// hold them, so the comparison reads only the window. The changed lines
    /// between go to the section of the first of them. A later section whose
    /// leading lines are among them keeps a copy of those lines and takes it
    /// back when a take replaces the holder's text.
    pub(crate) fn absorb_output_window(
        &mut self,
        first: u32,
        last: u32,
        mut lines: Vec<String>,
    ) -> Option<usize> {
        let last = (last as usize).min(self.output.len());
        let first = (first as usize).min(last);
        let old_len = last - first;
        let mut prefix = 0;
        while prefix < old_len.min(lines.len())
            && self.output.get(first + prefix) == lines.get(prefix)
        {
            prefix += 1;
        }
        let mut suffix = 0;
        while suffix < (old_len - prefix).min(lines.len() - prefix)
            && self.output.get(last - 1 - suffix) == lines.get(lines.len() - 1 - suffix)
        {
            suffix += 1;
        }
        lines.truncate(lines.len() - suffix);
        let middle = lines.split_off(prefix);
        let start = first + prefix;
        let end = last - suffix;
        let line = |value: usize| u32::try_from(value).ok();
        let owner = if start < end || start == first {
            self.section_of_output_line(line(start)?)?
        } else {
            self.section_of_output_line(line(start - 1)?)?
        };
        let lender_last = if start < end {
            self.section_of_output_line(line(end - 1)?)?.max(owner)
        } else {
            owner
        };
        for index in (owner + 1..=lender_last).rev() {
            let range = self.output_range(index)?;
            let taken_end = range.end.min(line(end)?);
            if taken_end <= range.start {
                continue;
            }
            let lent = self.unrepaired_lines(range.start as usize..taken_end as usize);
            if !self.edit_output_range(index, 0..taken_end - range.start, Vec::new()) {
                return None;
            }
            self.sections[index].lent.push(Lent {
                holder: owner,
                lines: lent,
            });
        }
        let range = self.output_range(owner)?;
        let local_start = line(start)?.checked_sub(range.start)?;
        let local_end = line(end)?.min(range.end).checked_sub(range.start)?;
        if !self.edit_output_range(owner, local_start..local_end.max(local_start), middle) {
            return None;
        }
        if lender_last > owner {
            let through = &mut self.sections[owner].joined_through;
            *through = Some(through.map_or(lender_last, |through| through.max(lender_last)));
        }
        Some(owner)
    }

    /// Replace one output line with the line an input shows on the same row.
    ///
    /// The section becomes an edited section. Where the input has no line on
    /// that row the output line is removed, and where the output has none the
    /// input's line is added at the end of the section. Returns false when the
    /// row names no section or the output already holds that text.
    pub fn take_line(&mut self, row: usize, pane: Pane) -> bool {
        self.take_line_with_range(row, pane).is_some()
    }

    pub(crate) fn take_line_with_range(
        &mut self,
        row: usize,
        pane: Pane,
    ) -> Option<(usize, Range<u32>, Range<u32>)> {
        if pane == Pane::Output {
            return None;
        }
        let section_index = self.section_of_row(row)?;
        let first = self.row_of_section(section_index)?;
        let step = row - first;
        let source = self.rows[row]
            .line(pane, None)
            .and_then(|line| self.inputs.lines(pane).get(line as usize))
            .cloned();
        let old_section = self.output_range(section_index)?;
        let old_length = self.sections.get(section_index)?.output_len as usize;
        let exists = step < old_length;
        let (local, replacement) = match source {
            Some(text) if exists => {
                let output_index = old_section.start as usize + step;
                #[cfg(test)]
                TAKE_LINE_ITEMS_READ.with(|reads| reads.set(reads.get().saturating_add(1)));
                if self
                    .output
                    .get(output_index)
                    .is_some_and(|line| line == &text)
                {
                    return None;
                }
                let start = u32::try_from(step).ok()?;
                (start..start.checked_add(1)?, vec![text])
            }
            Some(text) => {
                let end = u32::try_from(old_length).ok()?;
                (end..end, vec![text])
            }
            None if exists => {
                let start = u32::try_from(step).ok()?;
                (start..start.checked_add(1)?, Vec::new())
            }
            None => return None,
        };
        let global_start = old_section.start.checked_add(local.start)?;
        let old_global = global_start..global_start.checked_add(local.end - local.start)?;
        let new_global =
            global_start..global_start.checked_add(u32::try_from(replacement.len()).ok()?)?;
        if !self.edit_output_range(section_index, local, replacement) {
            return None;
        }
        self.repair_output_seams(new_global.start as usize..new_global.end as usize);
        Some((section_index, old_global, new_global))
    }

    /// Set or clear the wait for review on a set of sections.
    ///
    /// When any of them is not waiting, every one of them is set; otherwise
    /// every one is cleared. Returns the state the sections now have.
    pub fn toggle_conflict(&mut self, sections: &[usize]) -> bool {
        let set = sections
            .iter()
            .filter_map(|index| self.sections.get(*index))
            .any(|section| !section.is_unresolved_conflict());
        for index in sections {
            if let Some(section) = self.sections.get_mut(*index) {
                section.conflict = set;
                if set {
                    section.ignored = false;
                }
                section.refresh(self.rules);
            }
        }
        self.refresh_totals();
        set
    }

    /// Stop waiting for review on one unresolved conflict without changing
    /// the output text or its selected resolution.
    pub fn clear_conflict(&mut self, section: usize) -> bool {
        let Some(entry) = self.sections.get_mut(section) else {
            return false;
        };
        if !entry.is_unresolved_conflict() {
            return false;
        }
        entry.conflict = false;
        entry.refresh(self.rules);
        self.refresh_totals();
        true
    }

    /// Ignore or stop ignoring the differences of a set of sections.
    ///
    /// When any of them is not ignored, every one of them is ignored;
    /// otherwise every one is restored. Returns the state the sections now
    /// have.
    pub fn toggle_ignored(&mut self, sections: &[usize]) -> bool {
        let set = sections
            .iter()
            .filter_map(|index| self.sections.get(*index))
            .any(|section| !section.ignored);
        for index in sections {
            if let Some(section) = self.sections.get_mut(*index) {
                section.ignored = set;
                section.refresh(self.rules);
            }
        }
        self.refresh_totals();
        set
    }

    /// Mark every section whose changes the rules call unimportant.
    ///
    /// Each changed side is compared with the ancestor, or with the other side
    /// in a two way merge, and the section is unimportant only when no side
    /// holds an important difference.
    ///
    /// # Errors
    ///
    /// Returns the engine's error when a classification is refused.
    pub fn classify(&mut self, rules: &RuleSet) -> Result<(), ca_diff::DiffError> {
        for index in 0..self.sections.len() {
            let section = &self.sections[index];
            let pairs: Vec<(Pane, Pane)> = if self.inputs.two_way {
                vec![(Pane::Left, Pane::Right)]
            } else {
                match section.kind {
                    MergeKind::Unchanged => Vec::new(),
                    MergeKind::LeftChange | MergeKind::SameChange => {
                        vec![(Pane::Center, Pane::Left)]
                    }
                    MergeKind::RightChange => vec![(Pane::Center, Pane::Right)],
                    MergeKind::Conflict => {
                        vec![(Pane::Center, Pane::Left), (Pane::Center, Pane::Right)]
                    }
                }
            };
            let mut unimportant = !pairs.is_empty() && section.is_difference_kind();
            for (from, to) in pairs {
                if !unimportant {
                    break;
                }
                let before = slice(self.inputs.lines(from), &section.range(from));
                let after = slice(self.inputs.lines(to), &section.range(to));
                unimportant = changes_are_unimportant(&before, &after, rules)?;
            }
            let entry = &mut self.sections[index];
            entry.unimportant = unimportant;
            entry.refresh(self.rules);
        }
        self.refresh_totals();
        Ok(())
    }

    /// Resolve every section that does not wait for review from its own side.
    pub fn take_all_non_conflicting(&mut self) -> Vec<(usize, Range<u32>)> {
        self.take_all_non_conflicting_counted().0
    }

    /// [`Self::take_all_non_conflicting`], with the number of sections that
    /// were given back lines they lent to a taken section.
    pub(crate) fn take_all_non_conflicting_counted(&mut self) -> (Vec<(usize, Range<u32>)>, usize) {
        let mut changed = Vec::new();
        for index in 0..self.sections.len() {
            let section = &self.sections[index];
            if matches!(section.kind, MergeKind::Conflict) || section.conflict {
                continue;
            }
            let resolution = self.automatic_for(section.kind);
            if section.resolution == resolution && section.edited.is_empty() {
                continue;
            }
            let Some(output) = self.output_range(index) else {
                continue;
            };
            let section = &mut self.sections[index];
            section.resolution = resolution;
            section.edited.clear();
            section.edited_from_output = false;
            section.lent.clear();
            changed.push((index, output));
        }
        let mut indices: Vec<_> = changed.iter().map(|(index, _)| *index).collect();
        let returned = self.give_back_all(&indices);
        for &index in &returned {
            if let Some(output) = self.output_range(index) {
                changed.push((index, output));
            }
        }
        indices.extend(returned.iter().copied());
        self.rebuild_changed_sections(&indices, true);
        (changed, returned.len())
    }

    /// Resolve every unresolved conflict from one side.
    pub fn favor(&mut self, pane: Pane) {
        let resolution = match pane {
            Pane::Left => Resolution::Left,
            Pane::Right => Resolution::Right,
            Pane::Center => Resolution::Center,
            Pane::Output => return,
        };
        let mut retaken = Vec::new();
        for (index, section) in self.sections.iter_mut().enumerate() {
            if section.is_unresolved_conflict() {
                section.resolution = resolution;
                section.edited.clear();
                section.edited_from_output = false;
                section.conflict = false;
                section.lent.clear();
                retaken.push(index);
            }
        }
        let _ = self.give_back_all(&retaken);
        self.rebuild();
    }

    fn next_matching(&self, from: usize, keep: impl Fn(&Section) -> bool) -> Option<usize> {
        self.sections
            .iter()
            .enumerate()
            .skip(from.saturating_add(1))
            .find(|(_, section)| keep(section))
            .map(|(index, _)| index)
    }

    fn previous_matching(&self, from: usize, keep: impl Fn(&Section) -> bool) -> Option<usize> {
        self.sections
            .iter()
            .enumerate()
            .take(from)
            .rev()
            .find(|(_, section)| keep(section))
            .map(|(index, _)| index)
    }

    /// The next section after `from` that still needs review.
    #[must_use]
    pub fn next_conflict(&self, from: usize) -> Option<usize> {
        self.next_matching(from, Section::is_unresolved_conflict)
    }

    /// The previous section before `from` that still needs review.
    #[must_use]
    pub fn previous_conflict(&self, from: usize) -> Option<usize> {
        self.previous_matching(from, Section::is_unresolved_conflict)
    }

    /// The next section after `from` that either side changed.
    #[must_use]
    pub fn next_difference(&self, from: usize) -> Option<usize> {
        self.next_matching(from, Section::is_difference)
    }

    /// The previous section before `from` that either side changed.
    #[must_use]
    pub fn previous_difference(&self, from: usize) -> Option<usize> {
        self.previous_matching(from, Section::is_difference)
    }

    /// The next section after `from` that was typed into.
    #[must_use]
    pub fn next_edit(&self, from: usize) -> Option<usize> {
        self.next_matching(from, |section| {
            matches!(section.resolution, Resolution::Edited)
        })
    }

    /// The previous section before `from` that was typed into.
    #[must_use]
    pub fn previous_edit(&self, from: usize) -> Option<usize> {
        self.previous_matching(from, |section| {
            matches!(section.resolution, Resolution::Edited)
        })
    }

    /// True when a run of sections taken from `pane` starts at `index`.
    fn starts_run(&self, index: usize, pane: Pane) -> bool {
        let taken = |at: usize| {
            self.sections
                .get(at)
                .is_some_and(|section| taken_from(section, pane))
        };
        taken(index) && (index == 0 || !taken(index - 1))
    }

    /// The first section of the next run after `from` whose output holds
    /// `pane`'s version.
    ///
    /// Neighbouring sections taken from the same side are one run.
    #[must_use]
    pub fn next_taken(&self, from: usize, pane: Pane) -> Option<usize> {
        (from.saturating_add(1)..self.sections.len()).find(|index| self.starts_run(*index, pane))
    }

    /// The first section of the previous run before the one holding `from`
    /// whose output holds `pane`'s version.
    #[must_use]
    pub fn previous_taken(&self, from: usize, pane: Pane) -> Option<usize> {
        let mut own = from.min(self.sections.len());
        while own > 0
            && self
                .sections
                .get(own)
                .is_some_and(|section| taken_from(section, pane))
            && !self.starts_run(own, pane)
        {
            own -= 1;
        }
        (0..own).rev().find(|index| self.starts_run(*index, pane))
    }

    /// Adopt the resolutions of an earlier merge of the same files.
    ///
    /// A section keeps its resolution only when all three of its input texts
    /// are unchanged, so a reload that altered a region drops the decision that
    /// was made about the old content rather than applying it to new content.
    /// Among sections that hold the same three texts, the first takes the
    /// decision of the first, the second that of the second, and so on.
    pub fn carry_over(&mut self, previous: &Self) {
        self.carry(previous, false);
    }

    /// Adopt the resolutions of an earlier merge whose left and right inputs
    /// were the other way round.
    ///
    /// A take from one side becomes a take from the other side, so the output
    /// text stays the same.
    pub fn carry_over_swapped(&mut self, previous: &Self) {
        self.carry(previous, true);
    }

    fn carry(&mut self, previous: &Self, swapped: bool) {
        let untouched = |section: &Section| {
            section.resolution == previous.automatic_for(section.kind)
                && section.conflict == matches!(section.kind, MergeKind::Conflict)
                && !section.ignored
        };
        if previous.sections.iter().all(untouched) {
            return;
        }
        // Several sections can hold the same three texts. A decision is kept
        // under its fingerprint and the order of its section among those that
        // share it, so it lands on the matching section alone.
        let mut kept: HashMap<(Fingerprint, usize), Decision> = HashMap::new();
        let mut seen: HashMap<Fingerprint, usize> = HashMap::new();
        for (index, section) in previous.sections.iter().enumerate() {
            let (left, center, right) = previous.fingerprint(section);
            let print = if swapped {
                (right, center, left)
            } else {
                (left, center, right)
            };
            let occurrence = next_occurrence(&mut seen, &print);
            if untouched(section) {
                continue;
            }
            let resolution = if swapped {
                section.resolution.mirrored()
            } else {
                section.resolution
            };
            kept.insert(
                (print, occurrence),
                Decision {
                    index,
                    resolution,
                    edited: previous.edited_content(index, section),
                    conflict: section.conflict,
                    ignored: section.ignored,
                    lent: section.lent.clone(),
                },
            );
        }
        let mut changed = false;
        let mut moved: HashMap<usize, usize> = HashMap::new();
        let mut seen: HashMap<Fingerprint, usize> = HashMap::new();
        for index in 0..self.sections.len() {
            let print = self.fingerprint(&self.sections[index]);
            let occurrence = next_occurrence(&mut seen, &print);
            let Some(decision) = kept.get(&(print, occurrence)) else {
                continue;
            };
            moved.insert(decision.index, index);
            let rules = self.rules;
            let section = &mut self.sections[index];
            section.resolution = decision.resolution;
            section.edited.clone_from(&decision.edited);
            section.edited_from_output = false;
            section.conflict = decision.conflict;
            section.ignored = decision.ignored;
            section.lent.clone_from(&decision.lent);
            section.refresh(rules);
            changed = true;
        }
        if changed {
            self.carry_lent_lines(&moved);
            self.rebuild();
        }
    }

    /// Point lent lines at the carried sections that hold them. Lines whose
    /// holder was not carried go back to their section at once, because the
    /// reload replaced the holder's text with its input.
    fn carry_lent_lines(&mut self, moved: &HashMap<usize, usize>) {
        for index in 0..self.sections.len() {
            if self.sections[index].lent.is_empty() {
                continue;
            }
            let entries = std::mem::take(&mut self.sections[index].lent);
            let mut back = Vec::new();
            let mut kept = Vec::new();
            for entry in entries {
                match moved.get(&entry.holder) {
                    Some(&holder) if holder < index => kept.push(Lent {
                        holder,
                        lines: entry.lines,
                    }),
                    _ => back.extend(entry.lines),
                }
            }
            for entry in &kept {
                let through = &mut self.sections[entry.holder].joined_through;
                *through = Some(through.map_or(index, |through| through.max(index)));
            }
            if !back.is_empty() {
                back.extend(self.contribution(index, &self.sections[index]));
                let section = &mut self.sections[index];
                section.resolution = Resolution::Edited;
                section.edited = back;
            }
            self.sections[index].lent = kept;
        }
    }

    fn fingerprint(&self, section: &Section) -> Fingerprint {
        (
            joined(&self.inputs.left, &section.left),
            joined(&self.inputs.center, &section.center),
            joined(&self.inputs.right, &section.right),
        )
    }

    /// The output lines one section contributes.
    fn contribution(&self, index: usize, section: &Section) -> Vec<String> {
        match section.resolution {
            Resolution::Left => slice(&self.inputs.left, &section.left),
            Resolution::Right => slice(&self.inputs.right, &section.right),
            Resolution::Center => slice(&self.inputs.center, &section.center),
            Resolution::LeftThenRight => {
                let mut lines = slice(&self.inputs.left, &section.left);
                lines.extend(slice(&self.inputs.right, &section.right));
                lines
            }
            Resolution::RightThenLeft => {
                let mut lines = slice(&self.inputs.right, &section.right);
                lines.extend(slice(&self.inputs.left, &section.left));
                lines
            }
            Resolution::Edited => self.edited_content(index, section),
            Resolution::Unresolved => self.baseline(section),
        }
    }

    /// Output lines without the separators a source composition added.
    fn unrepaired_lines(&self, range: Range<usize>) -> Vec<String> {
        let start = range.start;
        let mut lines = self.output.copy_range(range);
        for (offset, line) in lines.iter_mut().enumerate() {
            let repair = self
                .output_repairs
                .get(start + offset)
                .copied()
                .unwrap_or(0);
            line.truncate(line.len().saturating_sub(repair));
        }
        lines
    }

    fn edited_content(&self, index: usize, section: &Section) -> Vec<String> {
        if section.edited_from_output {
            let lines = self.output_range(index).map_or_else(Vec::new, |range| {
                self.unrepaired_lines(range.start as usize..range.end as usize)
            });
            #[cfg(test)]
            EDITED_OUTPUT_READS.with(|reads| {
                reads.set(reads.get().saturating_add(lines.len()));
            });
            lines
        } else {
            section.edited.clone()
        }
    }

    /// What an unresolved section shows until it is decided.
    ///
    /// The ancestor is the baseline, which is what makes the output on load the
    /// ancestor except where a change was taken. A two way merge has no
    /// ancestor, so the left version stands in for it.
    fn baseline(&self, section: &Section) -> Vec<String> {
        if self.inputs.two_way {
            slice(&self.inputs.left, &section.left)
        } else {
            slice(&self.inputs.center, &section.center)
        }
    }

    /// Recompute the output text, the output ranges and the aligned rows.
    fn rebuild(&mut self) {
        self.materialize_output_edits();
        let mut output: Vec<String> = Vec::new();
        let mut rows: Vec<Row> = Vec::new();
        let mut row_counts = Vec::with_capacity(self.sections.len());
        let mut output_counts = Vec::with_capacity(self.sections.len());
        for index in 0..self.sections.len() {
            let section = self.sections[index].clone();
            let lines = self.contribution(index, &section);
            let output_count = lines.len();
            output.extend(lines);
            self.sections[index].output_len = u32::try_from(output_count).unwrap_or(u32::MAX);
            let ordinal = u32::try_from(index).unwrap_or(u32::MAX);
            let height = section
                .left
                .len()
                .max(section.center.len())
                .max(section.right.len())
                .max(output_count);
            for step in 0..height {
                let step = u32::try_from(step).unwrap_or(u32::MAX);
                rows.push(Row {
                    section: ordinal,
                    left: at(&section.left, step),
                    center: at(&section.center, step),
                    right: at(&section.right, step),
                });
            }
            row_counts.push(height);
            output_counts.push(output_count);
        }
        self.output = ChunkedVec::from_vec(output);
        self.output_repairs = ChunkedVec::from_vec(vec![0; self.output.len()]);
        self.repair_output_seams(0..self.output.len());
        self.rows = ChunkedVec::from_vec(rows);
        self.section_rows = PrefixLengths::from_values(row_counts);
        self.section_output = PrefixLengths::from_values(output_counts);
        self.refresh_totals();
    }

    fn materialize_output_edits(&mut self) {
        for index in 0..self.sections.len() {
            if !self.sections[index].edited_from_output {
                continue;
            }
            let lines = self.edited_content(index, &self.sections[index]);
            let section = &mut self.sections[index];
            section.edited = lines;
            section.edited_from_output = false;
        }
    }

    /// Rebuild only the named sections and splice their rows and output into
    /// the cached vectors. Output row offsets are relative to their section.
    fn rebuild_changed_sections(&mut self, changed: &[usize], repair_seams: bool) {
        let mut changed = changed.to_vec();
        changed.sort_unstable();
        changed.dedup();
        for &index in &changed {
            self.rebuild_section(index);
            let Some(section) = self.sections.get(index) else {
                continue;
            };
            let Some(previous) = self.section_totals.get_mut(index) else {
                continue;
            };
            let current = totals_for(section);
            self.totals.replace(*previous, current);
            *previous = current;
        }
        if !repair_seams {
            return;
        }
        for index in changed {
            if let Some(range) = self.output_range(index) {
                self.repair_output_seams(range.start as usize..range.end as usize);
            }
        }
    }

    /// Keep each contributed line distinct, including the seams on either
    /// side of a changed range. Pane edits already have actual line boundaries
    /// and do not use this source-composition repair.
    fn repair_output_seams(&mut self, range: Range<usize>) {
        let ending = self
            .output_ending
            .unwrap_or_else(ca_text::EolStyle::platform)
            .as_str();
        let end = range.end.saturating_add(1).min(self.output.len());
        for index in range.start.saturating_sub(1)..end {
            let has_next = index + 1 < self.output.len();
            let next_starts_lf = self
                .output
                .get(index + 1)
                .is_some_and(|line| line.starts_with('\n'));
            let previous_repair = self.output_repairs.get(index).copied().unwrap_or(0);
            if let Some(line) = self.output.get_mut(index) {
                line.truncate(line.len().saturating_sub(previous_repair));
                let original_len = line.len();
                if has_next && !line.ends_with(['\r', '\n']) {
                    line.push_str(ending);
                }
                // CR + a blank LF line otherwise becomes one CRLF terminator.
                if line.ends_with('\r') && next_starts_lf {
                    line.push('\n');
                }
                if let Some(repair) = self.output_repairs.get_mut(index) {
                    *repair = line.len() - original_len;
                }
            }
        }
    }

    fn rebuild_section(&mut self, index: usize) {
        let Some(section) = self.sections.get(index).cloned() else {
            return;
        };
        let Some(old_output) = self.output_range(index) else {
            return;
        };
        let Some(old_rows) = self.section_rows.range(index) else {
            return;
        };
        let output = self.contribution(index, &section);
        let output_len = output.len();
        let row_count = section
            .left
            .len()
            .max(section.center.len())
            .max(section.right.len())
            .max(output.len());
        let ordinal = u32::try_from(index).unwrap_or(u32::MAX);
        let rows: Vec<Row> = (0..row_count)
            .map(|step| {
                let step = u32::try_from(step).unwrap_or(u32::MAX);
                Row {
                    section: ordinal,
                    left: at(&section.left, step),
                    center: at(&section.center, step),
                    right: at(&section.right, step),
                }
            })
            .collect();
        let new_row_count = rows.len();

        self.output_repairs.splice(
            old_output.start as usize..old_output.end as usize,
            vec![0; output_len],
        );
        self.output
            .splice(old_output.start as usize..old_output.end as usize, output);
        self.rows.splice(old_rows, rows);
        self.sections[index].output_len = u32::try_from(output_len).unwrap_or(u32::MAX);
        let _ = self.section_output.set(index, output_len);
        let _ = self.section_rows.set(index, new_row_count);
    }
}

fn totals_for(section: &Section) -> Totals {
    if !section.is_difference() {
        return Totals::default();
    }
    Totals {
        differences: 1,
        conflicts: u32::from(matches!(section.kind, MergeKind::Conflict) || section.conflict),
        conflicts_remaining: u32::from(section.is_unresolved_conflict()),
        edited: u32::from(matches!(section.resolution, Resolution::Edited)),
        taken_center: u32::from(matches!(section.resolution, Resolution::Center)),
        taken_left: u32::from(section.resolution.holds_left()),
        taken_right: u32::from(section.resolution.holds_right()),
    }
}

impl Totals {
    fn add(&mut self, other: Self) {
        self.differences += other.differences;
        self.conflicts += other.conflicts;
        self.conflicts_remaining += other.conflicts_remaining;
        self.taken_left += other.taken_left;
        self.taken_right += other.taken_right;
        self.taken_center += other.taken_center;
        self.edited += other.edited;
    }

    fn replace(&mut self, old: Self, new: Self) {
        self.differences = self
            .differences
            .saturating_sub(old.differences)
            .saturating_add(new.differences);
        self.conflicts = self
            .conflicts
            .saturating_sub(old.conflicts)
            .saturating_add(new.conflicts);
        self.conflicts_remaining = self
            .conflicts_remaining
            .saturating_sub(old.conflicts_remaining)
            .saturating_add(new.conflicts_remaining);
        self.taken_left = self
            .taken_left
            .saturating_sub(old.taken_left)
            .saturating_add(new.taken_left);
        self.taken_right = self
            .taken_right
            .saturating_sub(old.taken_right)
            .saturating_add(new.taken_right);
        self.taken_center = self
            .taken_center
            .saturating_sub(old.taken_center)
            .saturating_add(new.taken_center);
        self.edited = self
            .edited
            .saturating_sub(old.edited)
            .saturating_add(new.edited);
    }
}

impl Section {
    const fn is_difference_kind(&self) -> bool {
        !matches!(self.kind, MergeKind::Unchanged)
    }
}

/// True when a changed section's output holds `pane`'s version.
fn taken_from(section: &Section, pane: Pane) -> bool {
    if !section.is_difference() {
        return false;
    }
    match pane {
        Pane::Left => section.resolution.holds_left(),
        Pane::Right => section.resolution.holds_right(),
        Pane::Center => matches!(section.resolution, Resolution::Center),
        Pane::Output => false,
    }
}

/// True when turning `before` into `after` changes nothing the rules count.
fn changes_are_unimportant(
    before: &[String],
    after: &[String],
    rules: &RuleSet,
) -> Result<bool, ca_diff::DiffError> {
    let before = borrowed(before);
    let after = borrowed(after);
    let hunks = diff_line_slices(&before, &after, &LineCompareOptions::default());
    let classified = classify_hunks(&before, &after, &hunks, rules, &WhitespaceClassifier)?;
    Ok(classified
        .iter()
        .all(|hunk| hunk.importance != Some(Importance::Important)))
}

/// The line a range holds `step` rows in, or nothing when the range is shorter.
fn at(range: &Range<u32>, step: u32) -> Option<u32> {
    let line = range.start.checked_add(step)?;
    (line < range.end).then_some(line)
}

fn shift_usize(value: usize, delta: i64) -> usize {
    usize::try_from(
        i64::try_from(value)
            .unwrap_or(i64::MAX)
            .saturating_add(delta),
    )
    .unwrap_or(usize::MAX)
}

fn borrowed(lines: &[String]) -> Vec<&str> {
    lines.iter().map(String::as_str).collect()
}

fn three_way_sections(
    inputs: &Inputs,
    options: &MergeOptions,
    cancel: &dyn Cancel,
) -> Result<Vec<Section>, ca_diff::DiffError> {
    let left = borrowed(&inputs.left);
    let center = borrowed(&inputs.center);
    let right = borrowed(&inputs.right);
    let result = merge3_cancellable(&left, &center, &right, options, cancel)?;
    Ok(result
        .regions
        .into_iter()
        .map(|region| {
            let mut section = Section::new(region.kind, region.left, region.base, region.right);
            if !matches!(region.kind, MergeKind::Conflict) {
                section.resolution = match region.output {
                    MergeSide::Left => Resolution::Left,
                    MergeSide::Right => Resolution::Right,
                    MergeSide::Base => Resolution::Center,
                };
            }
            section
        })
        .collect())
}

fn two_way_sections(
    inputs: &Inputs,
    options: &LineCompareOptions,
    cancel: &dyn Cancel,
) -> Result<Vec<Section>, ca_diff::DiffError> {
    let left = borrowed(&inputs.left);
    let right = borrowed(&inputs.right);
    let hunks = diff_line_slices_cancellable(&left, &right, options, cancel)?;
    Ok(hunks
        .into_iter()
        .map(|hunk| {
            let same = hunk.kind == HunkKind::Same;
            let kind = if same {
                MergeKind::Unchanged
            } else {
                MergeKind::Conflict
            };
            let mut section = Section::new(kind, hunk.left, 0..0, hunk.right);
            if same {
                section.resolution = Resolution::Left;
            }
            section
        })
        .collect())
}

/// Split a text into lines that keep their terminators.
#[must_use]
pub fn split(text: &str) -> Vec<String> {
    ca_diff::split_lines(text)
        .into_iter()
        .map(str::to_owned)
        .collect()
}

/// The left file's dominant ending, with first-seen ties as in `ca_text`.
/// Inspect terminator suffixes without allocating another copy of the file.
fn input_ending(lines: &[String]) -> ca_text::EolStyle {
    use ca_text::EolStyle;
    let mut counts = [0usize; 3];
    let mut first = None;
    for line in lines {
        let index = if line.ends_with("\r\n") {
            1
        } else if line.ends_with('\n') {
            0
        } else if line.ends_with('\r') {
            2
        } else {
            continue;
        };
        first.get_or_insert(index);
        counts[index] += 1;
    }
    let Some(mut chosen) = first else {
        return EolStyle::platform();
    };
    for index in 0..3 {
        if counts[index] > counts[chosen] {
            chosen = index;
        }
    }
    [EolStyle::Lf, EolStyle::CrLf, EolStyle::Cr][chosen]
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{
        split, ChunkedVec, DisplayRules, Inputs, LineStatus, MergeModel, Pane, PrefixLengths,
        Resolution, Section, Totals, MERGE_SEQUENCE_CHUNK_SIZE,
    };
    use ca_diff::merge3::{MergeKind, MergeOptions};
    use ca_ui::worker::Cancel;

    fn model(left: &str, center: &str, right: &str) -> MergeModel {
        MergeModel::build(
            Inputs {
                left: split(left),
                center: split(center),
                right: split(right),
                two_way: false,
            },
            &MergeOptions::default(),
            &Cancel::new(),
        )
        .unwrap()
    }

    fn large_merge_model(line_count: usize, left_change: Option<usize>) -> MergeModel {
        let mut center: Vec<String> = (0..line_count)
            .map(|index| format!("shared {index}\n"))
            .collect();
        let mut left = center.clone();
        let right = center.clone();
        let kind = if let Some(index) = left_change {
            left[index] = format!("changed {index}\n");
            super::MergeKind::LeftChange
        } else {
            super::MergeKind::Unchanged
        };
        let end = u32::try_from(line_count).unwrap();
        let sections = vec![Section::new(kind, 0..end, 0..end, 0..end)];
        let mut merged = MergeModel {
            inputs: Inputs {
                left,
                center: std::mem::take(&mut center),
                right,
                two_way: false,
            },
            sections,
            rows: ChunkedVec::default(),
            section_rows: PrefixLengths::default(),
            section_output: PrefixLengths::default(),
            output: ChunkedVec::default(),
            output_repairs: ChunkedVec::default(),
            output_ending: Some(ca_text::EolStyle::Lf),
            section_totals: Vec::new(),
            rules: DisplayRules::default(),
            totals: Totals::default(),
        };
        merged.rebuild();
        merged
    }

    fn many_section_model(section_count: usize) -> MergeModel {
        let lines: Vec<String> = (0..section_count)
            .map(|index| format!("line {index}\n"))
            .collect();
        let sections = (0..section_count)
            .map(|index| {
                let line = u32::try_from(index).unwrap();
                Section::new(
                    MergeKind::Unchanged,
                    line..line + 1,
                    line..line + 1,
                    line..line + 1,
                )
            })
            .collect();
        let mut merged = MergeModel {
            inputs: Inputs {
                left: lines.clone(),
                center: lines.clone(),
                right: lines,
                two_way: false,
            },
            sections,
            rows: ChunkedVec::default(),
            section_rows: PrefixLengths::default(),
            section_output: PrefixLengths::default(),
            output: ChunkedVec::default(),
            output_repairs: ChunkedVec::default(),
            output_ending: Some(ca_text::EolStyle::Lf),
            section_totals: Vec::new(),
            rules: DisplayRules::default(),
            totals: Totals::default(),
        };
        merged.rebuild();
        merged
    }

    #[test]
    fn prefix_lengths_find_ranges_across_empty_sections_and_updates() {
        let mut lengths = PrefixLengths::from_values(vec![0, 3, 0, 2, 1]);
        assert_eq!(lengths.range(0), Some(0..0));
        assert_eq!(lengths.range(1), Some(0..3));
        assert_eq!(lengths.range(2), Some(3..3));
        assert_eq!(lengths.range(3), Some(3..5));
        assert_eq!(lengths.range(4), Some(5..6));
        assert_eq!(
            (0..6)
                .map(|line| lengths.index_at(line))
                .collect::<Vec<_>>(),
            vec![Some(1), Some(1), Some(1), Some(3), Some(3), Some(4)]
        );
        assert_eq!(lengths.index_at(6), None);

        assert!(lengths.set(1, 1));
        assert_eq!(lengths.range(3), Some(1..3));
        assert_eq!(lengths.index_at(1), Some(3));
        assert_eq!(lengths.index_at(2), Some(3));
    }

    #[test]
    fn a_change_on_one_side_alone_is_taken_without_asking() {
        let merged = model("a\nB\nc\n", "a\nb\nc\n", "a\nb\nc\n");
        assert_eq!(merged.output_text(), "a\nB\nc\n");
        let totals = merged.totals();
        assert_eq!(totals.conflicts, 0);
        assert_eq!(totals.taken_left, 1);
        assert_eq!(totals.taken_right, 0);
    }

    #[test]
    fn opposing_changes_to_one_line_wait_for_review() {
        let merged = model("a\nL\nc\n", "a\nb\nc\n", "a\nR\nc\n");
        let totals = merged.totals();
        assert_eq!(totals.conflicts, 1);
        assert_eq!(totals.conflicts_remaining, 1);
        // An unresolved conflict shows the ancestor until it is decided.
        assert_eq!(merged.output_text(), "a\nb\nc\n");
    }

    #[test]
    fn every_action_on_a_conflict_writes_the_text_it_names() {
        let table = [
            (Resolution::Left, "a\nL\nc\n"),
            (Resolution::Right, "a\nR\nc\n"),
            (Resolution::Center, "a\nb\nc\n"),
            (Resolution::LeftThenRight, "a\nL\nR\nc\n"),
            (Resolution::RightThenLeft, "a\nR\nL\nc\n"),
        ];
        for (resolution, expected) in table {
            let mut merged = model("a\nL\nc\n", "a\nb\nc\n", "a\nR\nc\n");
            let section = merged.next_conflict(0).or_else(|| {
                merged
                    .sections()
                    .iter()
                    .position(super::Section::is_unresolved_conflict)
            });
            merged.set_resolution(section.unwrap(), resolution);
            assert_eq!(merged.output_text(), expected, "{resolution:?}");
            assert_eq!(merged.totals().conflicts_remaining, 0);
        }
    }

    #[test]
    fn typing_into_a_section_replaces_its_output_and_a_take_restores_it() {
        let mut merged = model("a\nL\nc\n", "a\nb\nc\n", "a\nR\nc\n");
        let section = merged
            .sections()
            .iter()
            .position(super::Section::is_unresolved_conflict)
            .unwrap();
        merged.set_edited(section, vec!["mine\n".to_owned()]);
        assert_eq!(merged.output_text(), "a\nmine\nc\n");
        assert_eq!(merged.totals().edited, 1);
        merged.set_resolution(section, Resolution::Left);
        assert_eq!(merged.output_text(), "a\nL\nc\n");
        assert_eq!(merged.totals().edited, 0);
    }

    #[test]
    fn same_line_count_edits_preserve_untouched_output_allocations() {
        let mut merged = model("a\nL\nc\n", "a\nb\nc\n", "a\nR\nc\n");
        let unaffected = merged.output_lines()[0].as_ptr();
        let section = merged
            .sections()
            .iter()
            .position(super::Section::is_unresolved_conflict)
            .unwrap();

        merged.set_edited(section, vec!["mine\n".to_owned()]);

        assert_eq!(merged.output_text(), "a\nmine\nc\n");
        assert_eq!(merged.output_lines()[0].as_ptr(), unaffected);
    }

    #[test]
    fn output_range_edits_copy_only_the_replaced_lines_until_a_rebuild() {
        let mut merged = model("a\nL\nshared\n", "a\nb\nshared\n", "a\nR\nshared\n");
        let section = merged
            .sections()
            .iter()
            .position(|section| section.kind == MergeKind::Conflict)
            .unwrap();
        let output = merged.output_range(section).unwrap();

        super::MergeModel::reset_rebuild_visit_counts();
        assert!(merged.edit_output_range(section, 0..1, vec!["typed\n".to_owned()],));

        assert_eq!(merged.output_lines()[output.start as usize], "typed\n");
        assert_eq!(super::MergeModel::edited_output_read_count(), 0);
        let mut reloaded = model("a\nL\nshared\n", "a\nb\nshared\n", "a\nR\nshared\n");
        reloaded.carry_over(&merged);
        assert_eq!(reloaded.output_text(), "a\ntyped\nshared\n");
    }

    #[test]
    fn line_count_edits_rebuild_only_the_changed_section() {
        let mut merged = model("a\nL\nc\n", "a\nb\nc\n", "a\nR\nc\n");
        let unaffected = merged.output_lines()[2].as_ptr();
        let section = merged
            .sections()
            .iter()
            .position(super::Section::is_unresolved_conflict)
            .unwrap();

        super::MergeModel::reset_rebuild_visit_counts();
        merged.set_edited(section, vec!["mine\n".to_owned(), "extra\n".to_owned()]);

        assert_eq!(merged.output_text(), "a\nmine\nextra\nc\n");
        assert_eq!(merged.output_lines()[3].as_ptr(), unaffected);
        assert_eq!(merged.row_of_output_line(3), Some(3));
        assert_eq!(merged.output_line_for_row(3), Some(3));
        assert_eq!(super::MergeModel::rebuild_visit_counts().1, 0);
    }

    #[test]
    fn inserting_in_a_large_output_moves_only_a_bounded_chunk() {
        let line_count = 200_000;
        let mut merged = large_merge_model(line_count, None);
        let insertion = line_count / 2;
        MergeModel::reset_rebuild_visit_counts();

        assert!(merged.edit_output_range(
            0,
            u32::try_from(insertion).unwrap()..u32::try_from(insertion).unwrap(),
            vec!["inserted\n".to_owned()],
        ));

        assert_eq!(merged.output_lines().len(), line_count + 1);
        assert_eq!(merged.output_lines()[insertion], "inserted\n");
        assert_eq!(merged.output_lines()[insertion + 1], "shared 100000\n");
        assert!(
            MergeModel::sequence_items_touched() <= 4 * MERGE_SEQUENCE_CHUNK_SIZE + 4,
            "inserting one line moved {} items between chunks",
            MergeModel::sequence_items_touched()
        );
        let chunks = line_count.div_ceil(MERGE_SEQUENCE_CHUNK_SIZE);
        let logarithmic_bound = 32 * (usize::BITS - chunks.leading_zeros()) as usize;
        assert!(
            MergeModel::sequence_nodes_visited() <= logarithmic_bound,
            "inserting one line visited {} sequence nodes",
            MergeModel::sequence_nodes_visited()
        );
    }

    #[test]
    fn chunked_sequences_match_flat_splices_at_chunk_boundaries() {
        let mut flat: Vec<usize> = (0..3 * MERGE_SEQUENCE_CHUNK_SIZE).collect();
        let mut chunked = ChunkedVec::from_vec(flat.clone());
        let operations = [
            (0..0, vec![900_000, 900_001]),
            (
                MERGE_SEQUENCE_CHUNK_SIZE - 2..MERGE_SEQUENCE_CHUNK_SIZE + 3,
                vec![800_000],
            ),
            (
                2 * MERGE_SEQUENCE_CHUNK_SIZE..2 * MERGE_SEQUENCE_CHUNK_SIZE,
                vec![],
            ),
        ];
        for (range, replacement) in operations {
            flat.splice(range.clone(), replacement.clone());
            chunked.splice(range, replacement);
            assert_eq!(chunked.len(), flat.len());
            assert_eq!(chunked.iter().copied().collect::<Vec<_>>(), flat);
        }
        let range = flat.len() - 2..flat.len();
        let replacement = vec![700_000, 700_001, 700_002];
        flat.splice(range.clone(), replacement.clone());
        chunked.splice(range, replacement);
        assert_eq!(chunked.iter().copied().collect::<Vec<_>>(), flat);

        let mut seed = 0x5eed_u64;
        for step in 0..128usize {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let start = usize::try_from(seed).unwrap_or(usize::MAX) % (flat.len() + 1);
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let end = start
                .saturating_add(usize::try_from(seed).unwrap_or(usize::MAX) % 5)
                .min(flat.len());
            let replacement_len = usize::try_from(seed >> 8).unwrap_or(usize::MAX) % 4;
            let replacement: Vec<_> = (0..replacement_len)
                .map(|index| 1_000_000 + step * 4 + index)
                .collect();
            flat.splice(start..end, replacement.clone());
            chunked.splice(start..end, replacement);
            assert_eq!(chunked.len(), flat.len());
            assert_eq!(chunked.iter().copied().collect::<Vec<_>>(), flat);
        }

        chunked.splice(0..chunked.len(), Vec::new());
        flat.clear();
        assert!(chunked.is_empty());
        assert_eq!(chunked.iter().count(), flat.len());
        chunked.splice(0..0, vec![1, 2, 3]);
        flat.splice(0..0, [1, 2, 3]);
        assert_eq!(chunked.iter().copied().collect::<Vec<_>>(), flat);
    }

    #[test]
    fn taking_one_line_from_a_large_section_reads_only_that_line() {
        let line_count = 200_000;
        let changed = line_count / 2;
        let mut merged = large_merge_model(line_count, Some(changed));
        MergeModel::reset_rebuild_visit_counts();

        assert!(merged.take_line(changed, Pane::Center));

        assert_eq!(merged.output_lines()[changed], "shared 100000\n");
        assert_eq!(
            MergeModel::take_line_items_read(),
            1,
            "taking one line read {} output lines",
            MergeModel::take_line_items_read()
        );
        assert_eq!(MergeModel::edited_output_read_count(), 0);
    }

    #[test]
    fn inserting_a_line_updates_many_section_offsets_logarithmically() {
        let section_count = 20_000;
        let mut merged = many_section_model(section_count);
        MergeModel::reset_rebuild_visit_counts();

        assert!(merged.edit_output_range(0, 0..0, vec!["inserted\n".to_owned()]));

        let last = section_count - 1;
        assert_eq!(merged.output_range(last), Some(20_000..20_001));
        assert_eq!(merged.section_row_range(last), Some(20_000..20_001));
        assert_eq!(merged.section_of_output_line(20_000), Some(last));
        assert_eq!(merged.row_of_output_line(20_000), Some(20_000));
        assert_eq!(merged.output_line_for_row(20_000), Some(20_000));
        assert!(
            MergeModel::section_offset_cells_updated() <= 2 * usize::BITS as usize,
            "updating one section touched {} offset cells",
            MergeModel::section_offset_cells_updated()
        );
    }

    #[test]
    fn a_same_length_edit_does_not_recount_or_shift_the_rest_of_the_model() {
        let mut merged = model("a\nL\nc\n", "a\nb\nc\n", "a\nR\nc\n");
        let section = merged
            .sections()
            .iter()
            .position(super::Section::is_unresolved_conflict)
            .unwrap();
        super::MergeModel::reset_rebuild_visit_counts();

        merged.set_edited(section, vec!["mine\n".to_owned()]);

        assert_eq!(merged.totals().edited, 1);
        assert_eq!(super::MergeModel::rebuild_visit_counts(), (0, 0));
    }

    #[test]
    fn favoring_one_side_resolves_every_waiting_conflict() {
        for (pane, expected) in [(Pane::Left, "a\nL\nc\n"), (Pane::Right, "a\nR\nc\n")] {
            let mut merged = model("a\nL\nc\n", "a\nb\nc\n", "a\nR\nc\n");
            merged.favor(pane);
            assert_eq!(merged.output_text(), expected);
            assert_eq!(merged.totals().conflicts_remaining, 0);
        }
    }

    #[test]
    fn taking_all_non_conflicting_leaves_the_conflicts_alone() {
        let mut merged = model(
            "A\np\nq\nr\ns\nL\nc\n",
            "a\np\nq\nr\ns\nb\nc\n",
            "a\np\nq\nr\ns\nR\nc\n",
        );
        let section = merged
            .sections()
            .iter()
            .position(super::Section::is_unresolved_conflict)
            .unwrap();
        merged.set_resolution(section, Resolution::Center);
        let _ = merged.take_all_non_conflicting();
        assert_eq!(merged.totals().conflicts_remaining, 0);
        assert!(merged.output_text().starts_with("A\n"));
    }

    #[test]
    fn two_versions_alone_make_every_difference_a_conflict() {
        let merged = MergeModel::build(
            Inputs {
                left: split("a\nL\n"),
                center: Vec::new(),
                right: split("a\nR\n"),
                two_way: true,
            },
            &MergeOptions::default(),
            &Cancel::new(),
        )
        .unwrap();
        assert_eq!(merged.totals().conflicts, 1);
        // With no ancestor the left version is the baseline.
        assert_eq!(merged.output_text(), "a\nL\n");
    }

    #[test]
    fn rows_align_the_four_panes_and_pad_the_shorter_ones() {
        let mut merged = model("a\nL\nc\n", "a\nb\nc\n", "a\nR\nc\n");
        let section = merged
            .sections()
            .iter()
            .position(super::Section::is_unresolved_conflict)
            .unwrap();
        merged.set_resolution(section, Resolution::LeftThenRight);
        let rows = merged.rows();
        assert_eq!(rows.len(), merged.output_lines().len());
        let conflict_rows: Vec<_> = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.section as usize == section)
            .map(|(index, _)| index)
            .collect();
        assert_eq!(conflict_rows.len(), 2);
        assert_eq!(rows[conflict_rows[1]].left, None);
        assert_eq!(merged.output_line_for_row(conflict_rows[1]), Some(2));
    }

    #[test]
    fn a_resolution_survives_a_merge_of_files_whose_region_did_not_change() {
        let mut first = model("a\nL\nc\n", "a\nb\nc\n", "a\nR\nc\n");
        let section = first
            .sections()
            .iter()
            .position(super::Section::is_unresolved_conflict)
            .unwrap();
        first.set_resolution(section, Resolution::Right);
        // The reload adds a line far from the conflict.
        let mut second = model(
            "a\nL\nc\np\nq\nr\ns\nd\n",
            "a\nb\nc\np\nq\nr\ns\n",
            "a\nR\nc\np\nq\nr\ns\n",
        );
        second.carry_over(&first);
        assert!(second.output_text().contains("\nR\n"));
        assert_eq!(second.totals().conflicts_remaining, 0);
    }

    /// Two conflicts with the same three texts, the second one decided.
    fn repeated_conflicts(left: &str, right: &str) -> (MergeModel, Vec<usize>) {
        let text = |middle: &str| format!("a\n{middle}\nc\nd\ne\nf\ng\n{middle}\nh\n");
        let merged = model(&text(left), &text("b"), &text(right));
        let conflicts: Vec<usize> = merged
            .sections()
            .iter()
            .enumerate()
            .filter(|(_, section)| section.is_unresolved_conflict())
            .map(|(index, _)| index)
            .collect();
        assert_eq!(conflicts.len(), 2);
        (merged, conflicts)
    }

    /// A decision about one of two sections that hold the same texts lands on
    /// that section alone after a reload, a typed edit included.
    #[test]
    fn a_carried_decision_lands_on_its_own_section_of_two_alike() {
        let (mut first, conflicts) = repeated_conflicts("L", "R");
        first.set_resolution(conflicts[1], Resolution::Left);
        let (mut second, _) = repeated_conflicts("L", "R");
        second.carry_over(&first);
        assert_eq!(second.output_text(), first.output_text());
        assert_eq!(second.totals().conflicts_remaining, 1);
        assert!(second.sections()[conflicts[0]].is_unresolved_conflict());

        let (mut first, conflicts) = repeated_conflicts("L", "R");
        first.set_edited(conflicts[0], vec!["X\n".to_owned()]);
        let (mut second, _) = repeated_conflicts("L", "R");
        second.carry_over(&first);
        assert_eq!(second.output_text(), first.output_text());
        assert_eq!(second.totals().edited, 1);
    }

    /// Swap Sides mirrors a carried decision onto the same section only.
    #[test]
    fn a_decision_carried_over_a_swap_lands_on_its_own_section_of_two_alike() {
        let (mut first, conflicts) = repeated_conflicts("L", "R");
        first.set_resolution(conflicts[1], Resolution::Left);
        let (mut swapped, _) = repeated_conflicts("R", "L");
        swapped.carry_over_swapped(&first);
        assert_eq!(swapped.output_text(), first.output_text());
        assert_eq!(swapped.totals().conflicts_remaining, 1);
        assert!(swapped.sections()[conflicts[0]].is_unresolved_conflict());
    }

    #[test]
    fn a_resolution_is_dropped_when_its_own_region_changed() {
        let mut first = model("a\nL\nc\n", "a\nb\nc\n", "a\nR\nc\n");
        let section = first
            .sections()
            .iter()
            .position(super::Section::is_unresolved_conflict)
            .unwrap();
        first.set_resolution(section, Resolution::Right);
        let mut second = model("a\nL2\nc\n", "a\nb\nc\n", "a\nR\nc\n");
        second.carry_over(&first);
        assert_eq!(second.totals().conflicts_remaining, 1);
    }

    #[test]
    fn navigation_steps_over_the_sections_that_need_review() {
        // The two conflicts sit further apart than the separation setting, so
        // they are two sections rather than one.
        let merged = model(
            "L1\np\nq\nr\ns\nL2\n",
            "b1\np\nq\nr\ns\nb2\n",
            "R1\np\nq\nr\ns\nR2\n",
        );
        assert_eq!(merged.totals().conflicts, 2);
        let first = merged
            .sections()
            .iter()
            .position(super::Section::is_unresolved_conflict)
            .unwrap();
        let second = merged.next_conflict(first).unwrap();
        assert!(second > first);
        assert_eq!(merged.previous_conflict(second), Some(first));
        assert_eq!(merged.next_conflict(second), None);
        assert!(merged.next_difference(0).is_some());
    }

    #[test]
    fn an_unchanged_region_names_no_difference() {
        let merged = model("a\n", "a\n", "a\n");
        assert_eq!(merged.totals().differences, 0);
        assert_eq!(merged.sections().len(), 1);
        assert_eq!(merged.sections()[0].kind, MergeKind::Unchanged);
        assert_eq!(merged.output_text(), "a\n");
    }

    /// Four regions far enough apart to stay four sections: a left change, a
    /// right change, a change both sides made, and a conflict.
    fn four_kinds() -> MergeModel {
        model(
            "L\np\nq\nr\ns\nb\np\nq\nr\ns\nS\np\nq\nr\ns\nX\n",
            "a\np\nq\nr\ns\nb\np\nq\nr\ns\nc\np\nq\nr\ns\nd\n",
            "a\np\nq\nr\ns\nR\np\nq\nr\ns\nS\np\nq\nr\ns\nY\n",
        )
    }

    fn status_of(merged: &MergeModel, kind: MergeKind) -> LineStatus {
        merged
            .sections()
            .iter()
            .find(|section| section.kind == kind)
            .map(super::Section::status)
            .unwrap()
    }

    fn section_of(merged: &MergeModel, kind: MergeKind) -> usize {
        merged
            .sections()
            .iter()
            .position(|section| section.kind == kind)
            .unwrap()
    }

    #[test]
    fn each_region_carries_the_status_the_filters_select_on() {
        let merged = four_kinds();
        assert_eq!(
            status_of(&merged, MergeKind::LeftChange),
            LineStatus::LeftChange
        );
        assert_eq!(
            status_of(&merged, MergeKind::RightChange),
            LineStatus::RightChange
        );
        assert_eq!(
            status_of(&merged, MergeKind::SameChange),
            LineStatus::SameChange
        );
        assert_eq!(
            status_of(&merged, MergeKind::Conflict),
            LineStatus::Conflict
        );
        assert_eq!(
            status_of(&merged, MergeKind::Unchanged),
            LineStatus::Unchanged
        );
    }

    #[test]
    fn a_conflict_cleared_by_hand_is_a_different_change_and_can_be_set_again() {
        let mut merged = four_kinds();
        let conflict = section_of(&merged, MergeKind::Conflict);
        let before = merged.output_text();
        assert!(!merged.toggle_conflict(&[conflict]));
        assert_eq!(
            merged.sections()[conflict].status(),
            LineStatus::DifferentChange
        );
        assert_eq!(merged.totals().conflicts_remaining, 0);
        assert_eq!(merged.output_text(), before, "clearing changed the output");
        assert!(merged.toggle_conflict(&[conflict]));
        assert_eq!(merged.totals().conflicts_remaining, 1);
    }

    #[test]
    fn marking_a_change_as_a_conflict_makes_it_wait_until_a_take() {
        let mut merged = four_kinds();
        let right = section_of(&merged, MergeKind::RightChange);
        assert!(merged.toggle_conflict(&[right]));
        assert_eq!(merged.sections()[right].status(), LineStatus::Conflict);
        assert_eq!(merged.totals().conflicts_remaining, 2);
        assert_eq!(merged.next_conflict(0), Some(right));
        merged.set_resolution(right, Resolution::Right);
        assert_eq!(merged.sections()[right].status(), LineStatus::RightChange);
        assert_eq!(merged.totals().conflicts_remaining, 1);
    }

    #[test]
    fn marking_several_sections_sets_them_all_when_one_is_not_marked() {
        let mut merged = four_kinds();
        let left = section_of(&merged, MergeKind::LeftChange);
        let conflict = section_of(&merged, MergeKind::Conflict);
        assert!(merged.toggle_conflict(&[left, conflict]));
        assert!(merged.sections()[conflict].is_unresolved_conflict());
        assert!(merged.sections()[left].is_unresolved_conflict());
        assert!(!merged.toggle_conflict(&[left, conflict]));
        assert_eq!(merged.totals().conflicts_remaining, 0);
    }

    #[test]
    fn taking_one_line_edits_only_that_line_of_the_output() {
        let mut merged = model("a\nL1\nL2\nc\n", "a\nb1\nb2\nc\n", "a\nR1\nR2\nc\n");
        let conflict = section_of(&merged, MergeKind::Conflict);
        let second_row = merged.row_of_section(conflict).unwrap() + 1;
        assert!(merged.take_line(second_row, Pane::Left));
        assert_eq!(merged.output_text(), "a\nb1\nL2\nc\n");
        assert_eq!(merged.sections()[conflict].resolution, Resolution::Edited);
        assert_eq!(merged.totals().conflicts_remaining, 0);
        assert!(merged.take_line(second_row, Pane::Right));
        assert_eq!(merged.output_text(), "a\nb1\nR2\nc\n");
        assert!(merged.take_line(second_row, Pane::Center));
        assert_eq!(merged.output_text(), "a\nb1\nb2\nc\n");
        assert!(
            !merged.take_line(second_row, Pane::Center),
            "nothing changed"
        );
    }

    #[test]
    fn taking_a_line_an_input_lacks_removes_it_and_one_the_output_lacks_adds_it() {
        let mut merged = model("a\nL1\nc\n", "a\nb1\nb2\nc\n", "a\nR1\nR2\nR3\nc\n");
        let conflict = section_of(&merged, MergeKind::Conflict);
        let first = merged.row_of_section(conflict).unwrap();
        assert!(merged.take_line(first + 1, Pane::Left));
        assert_eq!(merged.output_text(), "a\nb1\nc\n");
        assert!(merged.take_line(first + 2, Pane::Right));
        assert_eq!(merged.output_text(), "a\nb1\nR3\nc\n");
    }

    #[test]
    fn an_ignored_section_counts_as_unchanged_until_it_is_restored() {
        let mut merged = four_kinds();
        let right = section_of(&merged, MergeKind::RightChange);
        let differences = merged.totals().differences;
        assert!(merged.toggle_ignored(&[right]));
        assert_eq!(merged.sections()[right].status(), LineStatus::Unchanged);
        assert_eq!(merged.totals().differences, differences - 1);
        assert_ne!(merged.next_difference(0), Some(right));
        assert!(!merged.toggle_ignored(&[right]));
        assert_eq!(merged.totals().differences, differences);
    }

    #[test]
    fn ignoring_an_unresolved_conflict_takes_it_out_of_the_review_count() {
        let mut merged = four_kinds();
        let conflict = section_of(&merged, MergeKind::Conflict);
        merged.toggle_ignored(&[conflict]);
        assert_eq!(merged.totals().conflicts_remaining, 0);
        assert_eq!(merged.next_conflict(0), None);
    }

    #[test]
    fn ignoring_same_changes_counts_them_as_unchanged() {
        let mut merged = four_kinds();
        let same = section_of(&merged, MergeKind::SameChange);
        merged.set_rules(DisplayRules {
            ignore_same_changes: true,
            ..DisplayRules::default()
        });
        assert_eq!(merged.sections()[same].status(), LineStatus::Unchanged);
        assert!(!merged.sections()[same].is_difference());
        merged.set_rules(DisplayRules::default());
        assert_eq!(merged.sections()[same].status(), LineStatus::SameChange);
    }

    #[test]
    fn the_importance_rules_mark_a_whitespace_only_change_unimportant() {
        let mut merged = model(
            "a  \np\nq\nr\ns\nB\n",
            "a\np\nq\nr\ns\nb\n",
            "a\np\nq\nr\ns\nb\n",
        );
        let mut rules = ca_diff::RuleSet::all_important();
        rules.trailing_whitespace_important = false;
        merged.classify(&rules).unwrap();
        let unimportant: Vec<bool> = merged
            .sections()
            .iter()
            .filter(|section| section.kind == MergeKind::LeftChange)
            .map(|section| section.unimportant)
            .collect();
        assert_eq!(unimportant, vec![true, false]);
        let before = merged.totals().differences;
        merged.set_rules(DisplayRules {
            ignore_unimportant: true,
            ..DisplayRules::default()
        });
        assert_eq!(merged.totals().differences, before - 1);
    }

    #[test]
    fn taken_runs_are_found_forward_and_backward() {
        let mut merged = four_kinds();
        let left = section_of(&merged, MergeKind::LeftChange);
        let right = section_of(&merged, MergeKind::RightChange);
        let same = section_of(&merged, MergeKind::SameChange);
        let conflict = section_of(&merged, MergeKind::Conflict);
        merged.set_resolution(conflict, Resolution::Left);
        assert_eq!(merged.next_taken(left, Pane::Left), Some(same));
        assert_eq!(merged.next_taken(same, Pane::Left), Some(conflict));
        assert_eq!(merged.next_taken(conflict, Pane::Left), None);
        assert_eq!(merged.previous_taken(conflict, Pane::Left), Some(same));
        assert_eq!(merged.previous_taken(same, Pane::Left), Some(left));
        assert_eq!(merged.next_taken(left, Pane::Right), Some(right));
        assert_eq!(merged.previous_taken(right, Pane::Right), None);
    }

    #[test]
    fn neighbouring_sections_taken_from_one_side_are_one_run() {
        // Opposing changes one line apart stay separate under changed lines
        // only, so the two left-taken sections touch.
        let options = MergeOptions {
            conflict_scope: ca_diff::merge3::ConflictScope::ChangedLinesOnly,
            ..MergeOptions::default()
        };
        let merged = MergeModel::build(
            Inputs {
                left: split("A\nB\nc\np\nq\nr\ns\nD\n"),
                center: split("a\nb\nc\np\nq\nr\ns\nd\n"),
                right: split("a\nb\nc\np\nq\nr\ns\nd\n"),
                two_way: false,
            },
            &options,
            &Cancel::new(),
        )
        .unwrap();
        let taken: Vec<usize> = (0..merged.sections().len())
            .filter(|index| merged.sections()[*index].resolution.holds_left())
            .filter(|index| merged.sections()[*index].is_difference())
            .collect();
        assert_eq!(taken.len(), 2, "{:?}", merged.sections());
        assert_eq!(merged.next_taken(taken[0], Pane::Left), Some(taken[1]));
        assert_eq!(merged.previous_taken(taken[1], Pane::Left), Some(taken[0]));
    }

    #[test]
    fn a_manual_mark_survives_a_reload_of_unchanged_files() {
        let mut first = four_kinds();
        let left = section_of(&first, MergeKind::LeftChange);
        let same = section_of(&first, MergeKind::SameChange);
        first.toggle_conflict(&[left]);
        first.toggle_ignored(&[same]);
        let mut second = four_kinds();
        second.carry_over(&first);
        assert!(second.sections()[left].is_unresolved_conflict());
        assert!(second.sections()[same].ignored);
        assert!(!second.sections()[same].is_difference());
    }

    #[test]
    fn taking_all_non_conflicting_in_a_two_way_merge_keeps_the_shared_lines() {
        let mut merged = MergeModel::build(
            Inputs {
                left: split("a\nL\nc\n"),
                center: Vec::new(),
                right: split("a\nR\nc\n"),
                two_way: true,
            },
            &MergeOptions::default(),
            &Cancel::new(),
        )
        .unwrap();
        let _ = merged.take_all_non_conflicting();
        assert_eq!(merged.output_text(), "a\nL\nc\n");
        assert_eq!(
            merged.sections()[0].output_class(),
            super::MergeClass::Unchanged
        );
    }

    #[test]
    fn a_separator_takes_the_left_files_dominant_ending_whichever_side_is_unterminated() {
        let platform = ca_text::EolStyle::platform().as_str();
        assert_eq!(
            model("x", "x", "x\ny\n").output_text(),
            format!("x{platform}y\n")
        );
        assert_eq!(
            model("a\nb\r\nq", "a\nb\r\nq", "a\nb\r\nq\nr\n").output_text(),
            "a\nb\r\nq\nr\n"
        );
        assert_eq!(
            model("a\r\nb\nq", "a\r\nb\nq", "a\r\nb\nq\nr\n").output_text(),
            "a\r\nb\nq\r\nr\n"
        );

        let mut merged = model("", "p\nq", "p\nq\nr\n");
        merged.set_resolution(0, Resolution::Right);
        assert!(merged.take_line(1, Pane::Center));
        assert_eq!(merged.output_text(), format!("p\nq{platform}r\n"));
        merged.set_resolution(0, Resolution::Right);
        assert_eq!(merged.output_text(), "p\nq\nr\n");

        for (left, joined) in [
            ("a\nL\n", "a\nR\nL\n"),
            ("a\rL\r", "a\nR\rL\r"),
            ("a\r\nL\r\n", "a\nR\r\nL\r\n"),
        ] {
            let mut merged = model(left, "a\nB\n", "a\nR");
            merged.set_resolution(1, Resolution::RightThenLeft);
            assert_eq!(merged.output_text(), joined);
            merged.set_resolution(1, Resolution::Right);
            assert_eq!(merged.output_text(), "a\nR");
        }

        let mut merged = model("a\n\n", "a\nB\n", "a\nR\r");
        merged.set_resolution(1, Resolution::RightThenLeft);
        assert_eq!(merged.output_text(), "a\nR\r\n\n");
        merged.set_resolution(1, Resolution::Right);
        assert_eq!(merged.output_text(), "a\nR\r");

        let mut merged = model("a\nL", "a\nB\n", "a\nR\n");
        merged.set_resolution(1, Resolution::LeftThenRight);
        assert_eq!(merged.output_text(), "a\nL\nR\n");
        merged.set_resolution(1, Resolution::Left);
        assert_eq!(merged.output_text(), "a\nL");
    }
}
