//! Alignment of two scanned trees and the status of every resulting pair.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::mpsc::sync_channel;

use rayon::prelude::*;
use unicode_normalization::UnicodeNormalization;

use crate::cancel::Cancel;
use crate::criteria::{
    compare_contents, compare_source_contents_with, quick_compare_with, CompareOptions,
    ContentError, ContentOutcome, ContentSide, QuickDifference, QuickResult, RulesComparer, Side,
};
use crate::filter::{wildcard_match, CaseSensitivity, FilterContext, NameFilters, OtherFilters};
use crate::scan::{Entry, ScanResult};
use crate::source::{EntryFacts, Source};

/// Updates handed to the tree in one batch, so a long content pass shows
/// results while it runs without holding one update per pair in memory.
const APPLY_BATCH: usize = 8_192;

/// Finished comparisons waiting to be applied.
const CHANNEL_DEPTH: usize = 1_024;

/// Left entries between cancellation checks during alignment.
const ALIGN_CANCEL_STRIDE: usize = 1_024;

/// Status of one aligned pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NodeStatus {
    /// No comparison has run yet.
    #[default]
    NotCompared,
    /// The two sides match.
    Same,
    /// The two sides differ, with no single side newer.
    Different,
    /// The two sides differ and the left side is newer.
    LeftNewer,
    /// The two sides differ and the right side is newer.
    RightNewer,
    /// The item exists only on the left.
    LeftOrphan,
    /// The item exists only on the right.
    RightOrphan,
    /// The name is a file on one side and a folder on the other.
    KindMismatch,
    /// The item could not be compared.
    Error,
}

impl NodeStatus {
    /// True for the two orphan statuses.
    #[must_use]
    pub fn is_orphan(self) -> bool {
        matches!(self, Self::LeftOrphan | Self::RightOrphan)
    }

    /// True for every status that counts as a mismatch.
    #[must_use]
    pub fn is_difference(self) -> bool {
        !matches!(self, Self::Same | Self::NotCompared)
    }

    /// True for the statuses a later pass must not overwrite, because they
    /// describe the pairing itself rather than the contents.
    #[must_use]
    pub fn is_structural(self) -> bool {
        self.is_orphan() || self == Self::KindMismatch
    }
}

/// What a folder's descendants contain, so one folder can report several hints
/// at once.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StatusFlags {
    /// A descendant matches.
    pub same: bool,
    /// A descendant differs without a newer side.
    pub different: bool,
    /// A descendant is newer on the left.
    pub left_newer: bool,
    /// A descendant is newer on the right.
    pub right_newer: bool,
    /// A descendant exists only on the left.
    pub left_orphan: bool,
    /// A descendant exists only on the right.
    pub right_orphan: bool,
    /// A descendant could not be compared.
    pub error: bool,
    /// A descendant has not been compared yet.
    pub not_compared: bool,
}

impl StatusFlags {
    /// Record one status.
    pub fn record(&mut self, status: NodeStatus) {
        match status {
            NodeStatus::NotCompared => self.not_compared = true,
            NodeStatus::Same => self.same = true,
            NodeStatus::Different | NodeStatus::KindMismatch => self.different = true,
            NodeStatus::LeftNewer => self.left_newer = true,
            NodeStatus::RightNewer => self.right_newer = true,
            NodeStatus::LeftOrphan => self.left_orphan = true,
            NodeStatus::RightOrphan => self.right_orphan = true,
            NodeStatus::Error => self.error = true,
        }
    }

    /// Merge another set of flags into this one.
    pub fn merge(&mut self, other: Self) {
        self.same |= other.same;
        self.different |= other.different;
        self.left_newer |= other.left_newer;
        self.right_newer |= other.right_newer;
        self.left_orphan |= other.left_orphan;
        self.right_orphan |= other.right_orphan;
        self.error |= other.error;
        self.not_compared |= other.not_compared;
    }
}

/// One aligned pair in the merged tree.
#[derive(Debug, Clone)]
pub struct Node {
    /// Display name, taken from the left side when both sides exist.
    pub name: String,
    /// Path relative to the base folders. Unique among siblings.
    pub rel: PathBuf,
    /// True when the pair is a folder on either side.
    pub is_dir: bool,
    /// Left side entry, absent for a right orphan.
    pub left: Option<Entry>,
    /// Right side entry, absent for a left orphan.
    pub right: Option<Entry>,
    /// What each side's listing proves about its entry's size and time stamp.
    pub facts: PairFacts,
    /// Status of this pair.
    pub status: NodeStatus,
    /// What the descendants contain; empty for files.
    pub flags: StatusFlags,
    /// Outcome of the quick tests, when they have run.
    pub quick: Option<QuickResult>,
    /// Outcome of the content test, when it has run.
    pub content: Option<ContentOutcome>,
    /// Why the pair could not be compared.
    pub error: Option<String>,
    /// Set when either side's listing under this node is partial, and on every
    /// descendant of such a node. The absence of a name under an incomplete
    /// node is not evidence that the name does not exist.
    pub incomplete: bool,
    /// Set on the root only, when a scan of either side was cancelled.
    pub scan_cancelled: bool,
    /// Child pairs, sorted folders first then by name.
    pub children: Vec<Node>,
}

/// What the two listings of one pair prove about their entries.
///
/// A pair whose side lies in a local folder carries [`EntryFacts::default`]
/// for that side.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PairFacts {
    /// Facts of the left entry.
    pub left: EntryFacts,
    /// Facts of the right entry.
    pub right: EntryFacts,
}

/// Releasing a deep tree must not recurse, because tree depth follows the file
/// system and the stack does not grow with it.
impl Drop for Node {
    fn drop(&mut self) {
        let mut stack = std::mem::take(&mut self.children);
        while let Some(mut node) = stack.pop() {
            stack.append(&mut node.children);
        }
    }
}

impl Node {
    fn new(name: String, rel: PathBuf, left: Option<Entry>, right: Option<Entry>) -> Self {
        let is_dir = left.as_ref().is_some_and(|entry| entry.is_dir)
            || right.as_ref().is_some_and(|entry| entry.is_dir);
        let status = match (&left, &right) {
            (Some(_), None) => NodeStatus::LeftOrphan,
            (None, Some(_)) => NodeStatus::RightOrphan,
            _ => NodeStatus::NotCompared,
        };
        Self {
            name,
            rel,
            is_dir,
            left,
            right,
            facts: PairFacts::default(),
            status,
            flags: StatusFlags::default(),
            quick: None,
            content: None,
            error: None,
            incomplete: false,
            scan_cancelled: false,
            children: Vec::new(),
        }
    }

    /// Visit this node and every descendant, parents before children.
    pub fn walk<'a>(&'a self, visit: &mut dyn FnMut(&'a Self)) {
        let mut stack: Vec<&'a Self> = vec![self];
        while let Some(node) = stack.pop() {
            visit(node);
            stack.extend(node.children.iter().rev());
        }
    }

    /// Count this node and every descendant.
    #[must_use]
    pub fn count(&self) -> usize {
        let mut total = 0;
        let mut stack: Vec<&Self> = vec![self];
        while let Some(node) = stack.pop() {
            total += 1;
            stack.extend(node.children.iter());
        }
        total
    }
}

/// Visit every node, parents before children, with mutable access.
fn walk_mut(root: &mut Node, visit: &mut dyn FnMut(&mut Node)) {
    visit(root);
    let mut stack: Vec<std::slice::IterMut<'_, Node>> = vec![root.children.iter_mut()];
    while !stack.is_empty() {
        let next = stack.last_mut().and_then(Iterator::next);
        match next {
            Some(node) => {
                visit(node);
                stack.push(node.children.iter_mut());
            }
            None => {
                stack.pop();
            }
        }
    }
}

/// Visit every node, children before parents, with mutable access.
///
/// Subtrees are moved onto an explicit stack rather than recursed through, so
/// depth costs heap rather than stack.
fn post_order_mut(root: &mut Node, finish: &mut dyn FnMut(&mut Node)) {
    struct Frame {
        node: Node,
        pending: Vec<Node>,
        done: Vec<Node>,
    }

    let mut pending: Vec<Node> = std::mem::take(&mut root.children);
    pending.reverse();
    let mut done: Vec<Node> = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();

    loop {
        let next = match stack.last_mut() {
            Some(frame) => frame.pending.pop(),
            None => pending.pop(),
        };
        if let Some(mut child) = next {
            if child.children.is_empty() {
                finish(&mut child);
                match stack.last_mut() {
                    Some(frame) => frame.done.push(child),
                    None => done.push(child),
                }
            } else {
                let mut grandchildren = std::mem::take(&mut child.children);
                grandchildren.reverse();
                stack.push(Frame {
                    node: child,
                    pending: grandchildren,
                    done: Vec::new(),
                });
            }
        } else if let Some(mut frame) = stack.pop() {
            frame.node.children = std::mem::take(&mut frame.done);
            finish(&mut frame.node);
            match stack.last_mut() {
                Some(parent) => parent.done.push(frame.node),
                None => done.push(frame.node),
            }
        } else {
            break;
        }
    }

    root.children = done;
    finish(root);
}

/// One rule pairing two differently named items.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlignmentOverride {
    /// Wildcard mask the left name must match.
    pub left: String,
    /// Wildcard mask the right name must match.
    pub right: String,
    /// Relative folder the rule is confined to; empty covers the whole tree.
    pub limit_to_folder: String,
}

impl AlignmentOverride {
    /// True when the rule applies to a pair sitting in `parent`.
    fn covers(&self, parent: &Path) -> bool {
        if self.limit_to_folder.trim().is_empty() {
            return true;
        }
        let wanted = Path::new(self.limit_to_folder.trim());
        parent == wanted || parent.starts_with(wanted)
    }
}

/// How names on the two sides are lined up.
#[derive(Debug, Clone)]
pub struct AlignmentOptions {
    /// Case rule for matching names.
    pub case: CaseSensitivity,
    /// Line up names that match except for the extension.
    pub align_different_extensions: bool,
    /// Line up names that are equivalent under a different Unicode
    /// normalization form.
    pub align_unicode_normalization: bool,
    /// Rules pairing differently named items, tried after the exact passes.
    pub overrides: Vec<AlignmentOverride>,
    /// Read the contents of a top-level folder that exists on one side only.
    /// With this off such a folder is reported without its children.
    pub scan_top_level_orphans: bool,
}

impl Default for AlignmentOptions {
    fn default() -> Self {
        Self {
            case: CaseSensitivity::default(),
            align_different_extensions: false,
            align_unicode_normalization: false,
            overrides: Vec::new(),
            scan_top_level_orphans: true,
        }
    }
}

/// Line up two scanned trees by name and return the merged root.
///
/// Names are matched exactly first; the normalization and extension fallbacks
/// then run in that order over what is left, so an exact match is never given
/// up for a looser one. A name that is a file on one side and a folder on the
/// other becomes a single pair carrying [`NodeStatus::KindMismatch`].
///
/// A cancelled alignment returns a root with no children.
#[must_use]
pub fn align_trees(
    left: &ScanResult,
    right: &ScanResult,
    options: &AlignmentOptions,
    cancel: &Cancel,
) -> Node {
    let left_children = group_by_parent(&left.entries);
    let right_children = group_by_parent(&right.entries);
    let root_path = PathBuf::new();
    let mut root = Node::new(String::new(), root_path.clone(), None, None);
    root.is_dir = true;
    root.status = NodeStatus::NotCompared;
    root.children = align_all(&left_children, &right_children, options, cancel);
    if !options.scan_top_level_orphans {
        for child in &mut root.children {
            if child.is_dir && child.status.is_orphan() {
                child.children.clear();
            }
        }
    }
    root.scan_cancelled = left.cancelled || right.cancelled || cancel.is_cancelled();
    root.incomplete = left.root_incomplete || right.root_incomplete || root.scan_cancelled;
    propagate_incomplete(&mut root);
    if !left.facts.is_empty() || !right.facts.is_empty() {
        walk_mut(&mut root, &mut |node| {
            if let Some(entry) = node.left.as_ref() {
                node.facts.left = left.facts_of(&entry.rel);
            }
            if let Some(entry) = node.right.as_ref() {
                node.facts.right = right.facts_of(&entry.rel);
            }
        });
    }
    root
}

/// Carry a partial listing down to every pair under it.
///
/// A pair inherits incompleteness from its ancestors as well as from its own
/// two entries, because a name missing anywhere under a partial listing may
/// exist on disk after all.
fn propagate_incomplete(root: &mut Node) {
    let mut stack: Vec<(&mut Node, bool)> = vec![(root, false)];
    while let Some((node, inherited)) = stack.pop() {
        let own = node
            .left
            .as_ref()
            .is_some_and(|entry| entry.listing_incomplete)
            || node
                .right
                .as_ref()
                .is_some_and(|entry| entry.listing_incomplete);
        node.incomplete = node.incomplete || inherited || own;
        let carry = node.incomplete;
        for child in &mut node.children {
            stack.push((child, carry));
        }
    }
}

type Grouped<'a> = BTreeMap<PathBuf, Vec<&'a Entry>>;

fn group_by_parent(entries: &BTreeMap<PathBuf, Entry>) -> Grouped<'_> {
    let mut grouped: Grouped<'_> = BTreeMap::new();
    for entry in entries.values() {
        let parent = entry
            .rel
            .parent()
            .map_or_else(PathBuf::new, Path::to_path_buf);
        grouped.entry(parent).or_default().push(entry);
    }
    grouped
}

/// Expand the whole merged tree without recursion.
fn align_all(
    left: &Grouped<'_>,
    right: &Grouped<'_>,
    options: &AlignmentOptions,
    cancel: &Cancel,
) -> Vec<Node> {
    struct Frame {
        nodes: Vec<Node>,
        next: usize,
    }

    let root_path = PathBuf::new();
    let Some(nodes) = align_directory(
        left,
        right,
        Some(&root_path),
        Some(&root_path),
        options,
        cancel,
    ) else {
        return Vec::new();
    };
    let mut stack: Vec<Frame> = vec![Frame { nodes, next: 0 }];

    loop {
        let Some(frame) = stack.last_mut() else {
            return Vec::new();
        };
        while frame.next < frame.nodes.len() && !frame.nodes[frame.next].is_dir {
            frame.next += 1;
        }
        if frame.next < frame.nodes.len() {
            let node = &frame.nodes[frame.next];
            let left_dir = node.left.as_ref().map(|entry| entry.rel.clone());
            let right_dir = node.right.as_ref().map(|entry| entry.rel.clone());
            let Some(children) = align_directory(
                left,
                right,
                left_dir.as_deref(),
                right_dir.as_deref(),
                options,
                cancel,
            ) else {
                return Vec::new();
            };
            stack.push(Frame {
                nodes: children,
                next: 0,
            });
        } else {
            let done = stack.pop().unwrap_or(Frame {
                nodes: Vec::new(),
                next: 0,
            });
            match stack.last_mut() {
                Some(parent) => {
                    if let Some(node) = parent.nodes.get_mut(parent.next) {
                        node.children = done.nodes;
                    }
                    parent.next += 1;
                }
                None => return done.nodes,
            }
        }
    }
}

/// Index of right entries for one pass, keyed by the pass's key and the kind.
type RightIndex = HashMap<(String, bool), Vec<usize>>;

/// Line up the children of one directory. `None` means the run was cancelled.
fn align_directory(
    left: &Grouped<'_>,
    right: &Grouped<'_>,
    left_dir: Option<&Path>,
    right_dir: Option<&Path>,
    options: &AlignmentOptions,
    cancel: &Cancel,
) -> Option<Vec<Node>> {
    if cancel.is_cancelled() {
        return None;
    }
    let empty: Vec<&Entry> = Vec::new();
    let left_entries = left_dir.and_then(|dir| left.get(dir)).unwrap_or(&empty);
    let right_entries = right_dir.and_then(|dir| right.get(dir)).unwrap_or(&empty);

    let mut right_taken = vec![false; right_entries.len()];
    let mut matched_left = vec![false; left_entries.len()];
    let mut nodes: Vec<Node> = Vec::new();

    let passes: [fn(&Entry, &AlignmentOptions) -> Option<String>; 3] = [
        |entry, options| Some(case_key(&entry.name, options)),
        |entry, options| {
            options
                .align_unicode_normalization
                .then(|| normalization_key(&case_key(&entry.name, options)))
        },
        |entry, options| {
            options.align_different_extensions.then(|| {
                let name = case_key(&entry.name, options);
                stem_of(&name)
            })
        },
    ];

    for key_of in passes {
        // Keys are computed once per side per pass; a linear scan per left
        // entry is quadratic once the two sides share no names.
        let index = build_index(right_entries, &right_taken, key_of, options);
        if index.is_empty() {
            continue;
        }
        for (left_index, left_entry) in left_entries.iter().enumerate() {
            if left_index % ALIGN_CANCEL_STRIDE == 0 && cancel.is_cancelled() {
                return None;
            }
            if matched_left[left_index] {
                continue;
            }
            let Some(left_key) = key_of(left_entry, options) else {
                continue;
            };
            let Some(right_index) =
                take_candidate(&index, &(left_key, left_entry.is_dir), &right_taken)
            else {
                continue;
            };
            right_taken[right_index] = true;
            matched_left[left_index] = true;
            nodes.push(Node::new(
                left_entry.name.clone(),
                left_entry.rel.clone(),
                Some((*left_entry).clone()),
                Some(right_entries[right_index].clone()),
            ));
        }
    }

    apply_overrides(
        left_entries,
        right_entries,
        &mut matched_left,
        &mut right_taken,
        &mut nodes,
        options,
        left_dir,
    );

    pair_kind_mismatches(
        left_entries,
        right_entries,
        &mut matched_left,
        &mut right_taken,
        &mut nodes,
        options,
    );

    for (index, entry) in left_entries.iter().enumerate() {
        if index % ALIGN_CANCEL_STRIDE == 0 && cancel.is_cancelled() {
            return None;
        }
        if !matched_left[index] {
            nodes.push(Node::new(
                entry.name.clone(),
                entry.rel.clone(),
                Some((*entry).clone()),
                None,
            ));
        }
    }
    for (index, entry) in right_entries.iter().enumerate() {
        if !right_taken[index] {
            nodes.push(Node::new(
                entry.name.clone(),
                entry.rel.clone(),
                None,
                Some((*entry).clone()),
            ));
        }
    }

    nodes.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
    Some(nodes)
}

/// Pair the names an override rule names, after the exact passes have taken
/// every pair they can.
///
/// A rule pairs items of the same kind only, so a rule written for files never
/// swallows a folder of the same name.
fn apply_overrides(
    left_entries: &[&Entry],
    right_entries: &[&Entry],
    matched_left: &mut [bool],
    right_taken: &mut [bool],
    nodes: &mut Vec<Node>,
    options: &AlignmentOptions,
    left_dir: Option<&Path>,
) {
    if options.overrides.is_empty() {
        return;
    }
    let parent = left_dir.unwrap_or_else(|| Path::new(""));
    let ignore_case = options.case.ignores_case();
    let rules: Vec<&AlignmentOverride> = options
        .overrides
        .iter()
        .filter(|rule| rule.covers(parent) && !rule.left.is_empty() && !rule.right.is_empty())
        .collect();
    if rules.is_empty() {
        return;
    }
    for (left_index, left_entry) in left_entries.iter().enumerate() {
        if matched_left[left_index] {
            continue;
        }
        let Some(rule) = rules
            .iter()
            .find(|rule| wildcard_match(&rule.left, &left_entry.name, ignore_case))
        else {
            continue;
        };
        let Some(right_index) = right_entries
            .iter()
            .enumerate()
            .filter(|(position, _)| !right_taken[*position])
            .find(|(_, candidate)| {
                candidate.is_dir == left_entry.is_dir
                    && wildcard_match(&rule.right, &candidate.name, ignore_case)
            })
            .map(|(position, _)| position)
        else {
            continue;
        };
        right_taken[right_index] = true;
        matched_left[left_index] = true;
        nodes.push(Node::new(
            left_entry.name.clone(),
            left_entry.rel.clone(),
            Some((*left_entry).clone()),
            Some(right_entries[right_index].clone()),
        ));
    }
}

/// Pair a name that is a file on one side and a folder on the other, so the
/// merged tree never holds two siblings with the same path.
fn pair_kind_mismatches(
    left_entries: &[&Entry],
    right_entries: &[&Entry],
    matched_left: &mut [bool],
    right_taken: &mut [bool],
    nodes: &mut Vec<Node>,
    options: &AlignmentOptions,
) {
    let exact = build_index(
        right_entries,
        right_taken,
        |entry, options| Some(case_key(&entry.name, options)),
        options,
    );
    for (left_index, left_entry) in left_entries.iter().enumerate() {
        if matched_left[left_index] {
            continue;
        }
        let key = (case_key(&left_entry.name, options), !left_entry.is_dir);
        let Some(right_index) = take_candidate(&exact, &key, right_taken) else {
            continue;
        };
        right_taken[right_index] = true;
        matched_left[left_index] = true;
        let mut node = Node::new(
            left_entry.name.clone(),
            left_entry.rel.clone(),
            Some((*left_entry).clone()),
            Some(right_entries[right_index].clone()),
        );
        node.status = NodeStatus::KindMismatch;
        nodes.push(node);
    }
}

fn build_index(
    right_entries: &[&Entry],
    right_taken: &[bool],
    key_of: fn(&Entry, &AlignmentOptions) -> Option<String>,
    options: &AlignmentOptions,
) -> RightIndex {
    let mut index: RightIndex = HashMap::new();
    for (position, entry) in right_entries.iter().enumerate() {
        if right_taken[position] {
            continue;
        }
        if let Some(key) = key_of(entry, options) {
            index.entry((key, entry.is_dir)).or_default().push(position);
        }
    }
    index
}

/// The lowest numbered candidate still free, so the result does not depend on
/// hash order.
fn take_candidate(index: &RightIndex, key: &(String, bool), right_taken: &[bool]) -> Option<usize> {
    index
        .get(key)?
        .iter()
        .copied()
        .find(|position| !right_taken[*position])
}

fn case_key(name: &str, options: &AlignmentOptions) -> String {
    if options.case.ignores_case() {
        name.to_lowercase()
    } else {
        name.to_owned()
    }
}

fn stem_of(name: &str) -> String {
    match name.rfind('.') {
        Some(0) | None => name.to_owned(),
        Some(index) => name[..index].to_owned(),
    }
}

/// Fold a name to its composed normalization form.
///
/// Only names that are canonically equivalent fold together, so two spellings
/// of the same letter pair while a letter and its unaccented counterpart stay
/// apart.
#[must_use]
pub fn normalization_key(name: &str) -> String {
    name.nfc().collect()
}

/// Run the quick tests over every paired file and roll folder statuses up.
///
/// A pair where either side's source refuses its entry cannot be ruled on,
/// whatever the metadata says. A file pair of that kind reports
/// [`NodeStatus::Error`] with the source's reason; a folder pair carries the
/// reason as its own error, which the roll up reports the same way.
///
/// Each pair is tested under the facts its two listings state in
/// [`Node::facts`]: a size either side could not prove settles nothing, and
/// time stamps compare at the coarser of the two precisions. A pair the quick
/// tests cannot settle at all stays [`NodeStatus::NotCompared`], and a
/// content test settles it.
pub fn compare_quick(root: &mut Node, options: &CompareOptions) {
    walk_mut(root, &mut |node| {
        if node.status == NodeStatus::KindMismatch {
            return;
        }
        if let Some(reason) = refusal(node) {
            node.error = Some(reason);
            if !node.is_dir {
                node.status = NodeStatus::Error;
            }
            return;
        }
        if node.is_dir {
            return;
        }
        if let (Some(left), Some(right)) = (node.left.as_ref(), node.right.as_ref()) {
            let result = quick_compare_with(
                left,
                node.facts.left,
                right,
                node.facts.right,
                &options.quick,
            );
            node.status = status_from_quick(&result);
            node.quick = Some(result);
        }
    });
    rollup(root);
}

/// Why a pair cannot be compared, when a side's source refuses its entry.
fn refusal(node: &Node) -> Option<String> {
    let (Some(left), Some(right)) = (node.left.as_ref(), node.right.as_ref()) else {
        return None;
    };
    let refused = [left, right].into_iter().find(|entry| entry.refused)?;
    Some(
        refused
            .error
            .clone()
            .unwrap_or_else(|| "the source refuses to open this entry".to_owned()),
    )
}

fn status_from_quick(result: &QuickResult) -> NodeStatus {
    if result.is_unsettled() {
        return NodeStatus::NotCompared;
    }
    if result.is_same() {
        return NodeStatus::Same;
    }
    if result.differences.contains(&QuickDifference::Timestamp) {
        return match result.newer {
            Some(Side::Left) => NodeStatus::LeftNewer,
            Some(Side::Right) => NodeStatus::RightNewer,
            None => NodeStatus::Different,
        };
    }
    NodeStatus::Different
}

/// Recompute every folder status from its children.
///
/// A folder's own timestamp never affects its status. A folder that exists on
/// one side only, or whose two sides are of different kinds, keeps that status;
/// otherwise mixed contents report [`NodeStatus::Different`] and the detail
/// stays in [`Node::flags`].
pub fn rollup(root: &mut Node) {
    post_order_mut(root, &mut |node| {
        if !node.is_dir {
            return;
        }
        let mut flags = StatusFlags::default();
        for child in &node.children {
            flags.record(child.status);
            flags.merge(child.flags);
        }
        node.flags = flags;
        if node.status.is_structural() {
            return;
        }
        node.status = folder_status(flags, node.error.is_some());
    });
}

/// The status of a folder whose descendants hold `flags`, where `own_error`
/// says the folder itself could not be read or opened.
///
/// A folder that holds an unreadable child reports the error rather than the
/// direction of a sibling's timestamp, so the failure is not hidden. A folder
/// whose children are the same or not compared yet is not compared.
#[must_use]
pub fn folder_status(flags: StatusFlags, own_error: bool) -> NodeStatus {
    let both_directions = flags.left_newer && flags.right_newer;
    if flags.different || flags.left_orphan || flags.right_orphan || both_directions {
        return NodeStatus::Different;
    }
    if own_error || flags.error {
        return NodeStatus::Error;
    }
    if flags.left_newer {
        return NodeStatus::LeftNewer;
    }
    if flags.right_newer {
        return NodeStatus::RightNewer;
    }
    if flags.not_compared {
        return NodeStatus::NotCompared;
    }
    NodeStatus::Same
}

/// Why one content comparison produced no outcome.
///
/// A cancelled pair produces no update at all, so cancellation has no variant
/// here.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContentFailure {
    /// A file could not be read.
    #[error("cannot read {path}: {message}")]
    Io {
        /// The file being read.
        path: String,
        /// Text of the underlying I/O error.
        message: String,
    },
    /// A rules based comparison was requested without an engine to run it.
    #[error("no rules engine supplied")]
    RulesUnavailable,
}

/// One finished content comparison, streamed as it completes.
#[derive(Debug, Clone)]
pub struct ContentUpdate {
    /// Path of the pair relative to the base folders.
    pub rel: PathBuf,
    /// Outcome, or why the comparison produced none.
    pub outcome: Result<ContentOutcome, ContentFailure>,
}

/// Run the content test over every paired file and fold the results into the
/// tree.
///
/// Work runs on the rayon pool and results are streamed to `on_update` and into
/// the tree in batches as they finish, so neither the work list nor the pending
/// results grow with the number of pairs. Setting `cancel` stops the remaining
/// pairs; a pair that never ran keeps the status the quick tests gave it.
/// Returns false when the run was cancelled before every pair finished.
pub fn compare_contents_parallel(
    root: &mut Node,
    left_base: &Path,
    right_base: &Path,
    options: &CompareOptions,
    rules: Option<&dyn RulesComparer>,
    cancel: &Cancel,
    on_update: &(dyn Fn(&ContentUpdate) + Sync),
) -> bool {
    if !options.content.enabled {
        return true;
    }
    let pairs = collect_pairs(root, left_base, right_base, options);
    run_pairs(root, &pairs, options, cancel, on_update, &|left, right| {
        compare_contents(left, right, &options.content, rules, cancel)
    })
}

/// Run the content test over every paired file of two sources of any kind
/// and fold the results into the tree.
///
/// Two local folders take [`compare_contents_parallel`]. A pair where either
/// side is another source is read through the file system trait, for every
/// method.
pub fn compare_source_contents_parallel(
    root: &mut Node,
    left: &Source,
    right: &Source,
    options: &CompareOptions,
    rules: Option<&dyn RulesComparer>,
    cancel: &Cancel,
    on_update: &(dyn Fn(&ContentUpdate) + Sync),
) -> bool {
    if left.is_local_folder() && right.is_local_folder() {
        return compare_contents_parallel(
            root,
            left.origin(),
            right.origin(),
            options,
            rules,
            cancel,
            on_update,
        );
    }
    if !options.content.enabled {
        return true;
    }
    let pairs = collect_pairs(root, Path::new(""), Path::new(""), options);
    run_pairs(
        root,
        &pairs,
        options,
        cancel,
        on_update,
        &|left_rel, right_rel| {
            let left_path = source_path(left, left_rel)?;
            let right_path = source_path(right, right_rel)?;
            compare_source_contents_with(
                ContentSide {
                    source: left,
                    path: &left_path,
                    facts: EntryFacts::default(),
                },
                ContentSide {
                    source: right,
                    path: &right_path,
                    facts: EntryFacts::default(),
                },
                &options.content,
                rules,
                cancel,
            )
        },
    )
}

/// The path inside `source` of an entry the scan keyed by `rel`.
pub(crate) fn source_path(source: &Source, rel: &Path) -> Result<ca_vfs::VfsPath, ContentError> {
    source
        .entry_path(rel)
        .map_err(|error| ContentError::Source {
            path: rel.display().to_string(),
            detail: error.to_string(),
        })
}

type PairComparer<'a> = dyn Fn(&Path, &Path) -> Result<ContentOutcome, ContentError> + Sync + 'a;

fn run_pairs(
    root: &mut Node,
    pairs: &[(PathBuf, PathBuf, PathBuf)],
    options: &CompareOptions,
    cancel: &Cancel,
    on_update: &(dyn Fn(&ContentUpdate) + Sync),
    compare: &PairComparer<'_>,
) -> bool {
    let (sender, receiver) = sync_channel::<ContentUpdate>(CHANNEL_DEPTH);

    std::thread::scope(|scope| {
        scope.spawn(move || {
            pairs.par_iter().for_each(|(rel, left, right)| {
                if cancel.is_cancelled() {
                    return;
                }
                let outcome = match compare(left, right) {
                    Ok(outcome) => Ok(outcome),
                    Err(ContentError::Cancelled) => return,
                    Err(ContentError::Io { path, source }) => Err(ContentFailure::Io {
                        path,
                        message: source.to_string(),
                    }),
                    Err(ContentError::RulesUnavailable) => Err(ContentFailure::RulesUnavailable),
                    Err(
                        error @ (ContentError::ContentNotStored { .. }
                        | ContentError::Source { .. }),
                    ) => Err(ContentFailure::Io {
                        path: rel.display().to_string(),
                        message: error.to_string(),
                    }),
                };
                let update = ContentUpdate {
                    rel: rel.clone(),
                    outcome,
                };
                on_update(&update);
                let _ = sender.send(update);
            });
            drop(sender);
        });

        let mut batch: Vec<ContentUpdate> = Vec::with_capacity(APPLY_BATCH);
        while let Ok(update) = receiver.recv() {
            batch.push(update);
            if batch.len() >= APPLY_BATCH {
                apply_updates(root, &batch, options);
                batch.clear();
            }
        }
        if !batch.is_empty() {
            apply_updates(root, &batch, options);
        }
    });

    rollup(root);
    !cancel.is_cancelled()
}

fn collect_pairs(
    root: &Node,
    left_base: &Path,
    right_base: &Path,
    options: &CompareOptions,
) -> Vec<(PathBuf, PathBuf, PathBuf)> {
    let mut out: Vec<(PathBuf, PathBuf, PathBuf)> = Vec::new();
    root.walk(&mut |node| {
        if node.is_dir || node.status == NodeStatus::KindMismatch {
            return;
        }
        let (Some(left), Some(right)) = (node.left.as_ref(), node.right.as_ref()) else {
            return;
        };
        if left.refused || right.refused {
            return;
        }
        let quick_same = node.quick.as_ref().is_some_and(QuickResult::is_same);
        if options.content.skip_if_quick_same && quick_same {
            return;
        }
        out.push((
            node.rel.clone(),
            left_base.join(&left.rel),
            right_base.join(&right.rel),
        ));
    });
    out
}

fn apply_updates(root: &mut Node, batch: &[ContentUpdate], options: &CompareOptions) {
    let index: HashMap<&Path, &ContentUpdate> = batch
        .iter()
        .map(|update| (update.rel.as_path(), update))
        .collect();
    walk_mut(root, &mut |node| {
        if let Some(update) = index.get(node.rel.as_path()) {
            apply_one(node, update, options);
        }
    });
}

fn apply_one(node: &mut Node, update: &ContentUpdate, options: &CompareOptions) {
    match &update.outcome {
        Ok(outcome) => {
            node.content = Some(*outcome);
            if outcome.is_not_compared() {
                return;
            }
            node.error = None;
            if outcome.is_same(options.content.ignore_unimportant) {
                if options.content.override_quick || node.status == NodeStatus::NotCompared {
                    node.status = NodeStatus::Same;
                }
            } else if !matches!(node.status, NodeStatus::LeftNewer | NodeStatus::RightNewer) {
                node.status = NodeStatus::Different;
            }
        }
        Err(failure) => {
            node.error = Some(failure.to_string());
            node.status = NodeStatus::Error;
        }
    }
}

/// Remove every node the session's filters exclude.
///
/// A folder include mask names the folders that take part, not the route to
/// them, so a folder that is not itself included survives while it still holds
/// something that is.
pub fn apply_filters(
    root: &mut Node,
    names: &NameFilters,
    others: &OtherFilters,
    context: &FilterContext,
) {
    post_order_mut(root, &mut |node| {
        node.children
            .retain(|child| keep_node(child, names, others, context));
    });
    rollup(root);
}

fn keep_node(
    node: &Node,
    names: &NameFilters,
    others: &OtherFilters,
    context: &FilterContext,
) -> bool {
    if node.is_dir {
        // A surviving subfolder is one the folder masks kept, so it is the only
        // thing that can hold an ancestor in the session.
        return names.allows_folder_traversal(&node.rel)
            && (names.allows(&node.rel, true) || node.children.iter().any(|child| child.is_dir));
    }
    if !names.allows(&node.rel, false) {
        return false;
    }
    node.left
        .as_ref()
        .or(node.right.as_ref())
        .is_none_or(|entry| others.allows(entry, context))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{
        align_trees, apply_filters, compare_contents_parallel, compare_quick, normalization_key,
        rollup, AlignmentOptions, Node, NodeStatus, StatusFlags,
    };
    use crate::cancel::Cancel;
    use crate::criteria::{CompareOptions, ContentMethod, ContentTests, QuickTests};
    use crate::filter::{FilterContext, NameFilters, OtherFilters};
    use crate::scan::{scan_with, Entry, ScanOptions, ScanResult};
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant, SystemTime};

    fn scan(root: &Path) -> ScanResult {
        scan_with(root, &ScanOptions::default(), &Cancel::new(), &|_| {}).unwrap()
    }

    fn align(left: &ScanResult, right: &ScanResult, options: &AlignmentOptions) -> Node {
        align_trees(left, right, options, &Cancel::new())
    }

    fn set_modified(path: &Path, seconds: u64) {
        let time =
            filetime::FileTime::from_unix_time(i64::try_from(seconds).unwrap_or_default(), 0);
        filetime::set_file_mtime(path, time).unwrap();
    }

    fn find<'a>(node: &'a Node, rel: &str) -> &'a Node {
        let mut found: Option<&Node> = None;
        node.walk(&mut |candidate| {
            if candidate.rel == Path::new(rel) {
                found = Some(candidate);
            }
        });
        found.unwrap()
    }

    fn entry(rel: &str, name: &str, is_dir: bool) -> Entry {
        Entry {
            rel: PathBuf::from(rel),
            name: name.to_owned(),
            is_dir,
            size: 0,
            modified: None,
            created: None,
            attributes: crate::scan::Attributes::default(),
            link: None,
            error: None,
            listing_incomplete: false,
            refused: false,
        }
    }

    fn synthetic_result(entries: Vec<Entry>) -> ScanResult {
        let mut result = ScanResult::default();
        for entry in entries {
            result.entries.insert(entry.rel.clone(), entry);
        }
        result
    }

    /// A chain of folders `d0/d1/.../dN`, deep enough that a recursive walk
    /// would exhaust the stack.
    fn deep_chain(depth: usize) -> Node {
        let mut node = Node::new("leaf".to_owned(), PathBuf::from("leaf"), None, None);
        node.is_dir = true;
        node.status = NodeStatus::NotCompared;
        for index in 0..depth {
            let mut parent = Node::new(
                format!("d{index}"),
                PathBuf::from(format!("d{index}")),
                None,
                None,
            );
            parent.is_dir = true;
            parent.status = NodeStatus::NotCompared;
            parent.children.push(node);
            node = parent;
        }
        let mut root = Node::new(String::new(), PathBuf::new(), None, None);
        root.is_dir = true;
        root.children.push(node);
        root
    }

    struct Pair {
        left: tempfile::TempDir,
        right: tempfile::TempDir,
    }

    fn pair() -> Pair {
        Pair {
            left: tempfile::tempdir().unwrap(),
            right: tempfile::tempdir().unwrap(),
        }
    }

    #[test]
    fn same_names_align_and_orphans_are_marked() {
        let dirs = pair();
        std::fs::write(dirs.left.path().join("both.txt"), b"same").unwrap();
        std::fs::write(dirs.right.path().join("both.txt"), b"same").unwrap();
        std::fs::write(dirs.left.path().join("only-left.txt"), b"x").unwrap();
        std::fs::write(dirs.right.path().join("only-right.txt"), b"y").unwrap();
        set_modified(&dirs.left.path().join("both.txt"), 1_000);
        set_modified(&dirs.right.path().join("both.txt"), 1_000);

        let mut tree = align(
            &scan(dirs.left.path()),
            &scan(dirs.right.path()),
            &AlignmentOptions::default(),
        );
        compare_quick(&mut tree, &CompareOptions::default());

        assert_eq!(find(&tree, "both.txt").status, NodeStatus::Same);
        assert_eq!(find(&tree, "only-left.txt").status, NodeStatus::LeftOrphan);
        assert_eq!(
            find(&tree, "only-right.txt").status,
            NodeStatus::RightOrphan
        );
    }

    #[test]
    fn newer_side_is_reported_per_file() {
        let dirs = pair();
        std::fs::write(dirs.left.path().join("a.txt"), b"one").unwrap();
        std::fs::write(dirs.right.path().join("a.txt"), b"one").unwrap();
        set_modified(&dirs.left.path().join("a.txt"), 2_000);
        set_modified(&dirs.right.path().join("a.txt"), 1_000);

        let mut tree = align(
            &scan(dirs.left.path()),
            &scan(dirs.right.path()),
            &AlignmentOptions::default(),
        );
        compare_quick(&mut tree, &CompareOptions::default());
        assert_eq!(find(&tree, "a.txt").status, NodeStatus::LeftNewer);
        assert_eq!(tree.status, NodeStatus::LeftNewer);
    }

    #[test]
    fn folder_status_rolls_up_from_children() {
        let dirs = pair();
        std::fs::create_dir(dirs.left.path().join("sub")).unwrap();
        std::fs::create_dir(dirs.right.path().join("sub")).unwrap();
        std::fs::write(dirs.left.path().join("sub/a.txt"), b"one").unwrap();
        std::fs::write(dirs.right.path().join("sub/a.txt"), b"one").unwrap();
        std::fs::write(dirs.left.path().join("sub/left-only.txt"), b"x").unwrap();
        set_modified(&dirs.left.path().join("sub/a.txt"), 1_000);
        set_modified(&dirs.right.path().join("sub/a.txt"), 1_000);

        let mut tree = align(
            &scan(dirs.left.path()),
            &scan(dirs.right.path()),
            &AlignmentOptions::default(),
        );
        compare_quick(&mut tree, &CompareOptions::default());
        let sub = find(&tree, "sub");
        assert_eq!(sub.status, NodeStatus::Different);
        assert!(sub.flags.left_orphan);
        assert!(sub.flags.same);
        assert!(!sub.flags.right_orphan);
    }

    #[test]
    fn folder_with_only_matches_is_same() {
        let dirs = pair();
        std::fs::create_dir(dirs.left.path().join("sub")).unwrap();
        std::fs::create_dir(dirs.right.path().join("sub")).unwrap();
        std::fs::write(dirs.left.path().join("sub/a.txt"), b"one").unwrap();
        std::fs::write(dirs.right.path().join("sub/a.txt"), b"one").unwrap();
        set_modified(&dirs.left.path().join("sub/a.txt"), 5_000);
        set_modified(&dirs.right.path().join("sub/a.txt"), 5_000);

        let mut tree = align(
            &scan(dirs.left.path()),
            &scan(dirs.right.path()),
            &AlignmentOptions::default(),
        );
        compare_quick(&mut tree, &CompareOptions::default());
        assert_eq!(find(&tree, "sub").status, NodeStatus::Same);
    }

    #[test]
    fn orphan_folder_keeps_orphan_status() {
        let dirs = pair();
        std::fs::create_dir(dirs.left.path().join("only")).unwrap();
        std::fs::write(dirs.left.path().join("only/a.txt"), b"x").unwrap();

        let mut tree = align(
            &scan(dirs.left.path()),
            &scan(dirs.right.path()),
            &AlignmentOptions::default(),
        );
        compare_quick(&mut tree, &CompareOptions::default());
        let only = find(&tree, "only");
        assert_eq!(only.status, NodeStatus::LeftOrphan);
        assert!(only.flags.left_orphan);
    }

    #[test]
    fn opposite_newer_children_make_a_folder_different() {
        let dirs = pair();
        for side in [dirs.left.path(), dirs.right.path()] {
            std::fs::create_dir(side.join("sub")).unwrap();
            std::fs::write(side.join("sub/a.txt"), b"x").unwrap();
            std::fs::write(side.join("sub/b.txt"), b"x").unwrap();
        }
        set_modified(&dirs.left.path().join("sub/a.txt"), 2_000);
        set_modified(&dirs.right.path().join("sub/a.txt"), 1_000);
        set_modified(&dirs.left.path().join("sub/b.txt"), 1_000);
        set_modified(&dirs.right.path().join("sub/b.txt"), 2_000);

        let mut tree = align(
            &scan(dirs.left.path()),
            &scan(dirs.right.path()),
            &AlignmentOptions::default(),
        );
        compare_quick(&mut tree, &CompareOptions::default());
        let sub = find(&tree, "sub");
        assert_eq!(sub.status, NodeStatus::Different);
        assert!(sub.flags.left_newer && sub.flags.right_newer);
    }

    #[test]
    fn an_unreadable_child_outranks_a_newer_sibling() {
        let mut root = Node::new(String::new(), PathBuf::new(), None, None);
        root.is_dir = true;
        let mut newer = Node::new("a.txt".to_owned(), PathBuf::from("a.txt"), None, None);
        newer.status = NodeStatus::LeftNewer;
        let mut broken = Node::new("b.txt".to_owned(), PathBuf::from("b.txt"), None, None);
        broken.status = NodeStatus::Error;
        root.children.push(newer);
        root.children.push(broken);
        rollup(&mut root);
        assert_eq!(root.status, NodeStatus::Error);
        assert!(root.flags.left_newer && root.flags.error);
    }

    #[test]
    fn extension_alignment_pairs_matching_stems() {
        let dirs = pair();
        std::fs::write(dirs.left.path().join("report.gif"), b"x").unwrap();
        std::fs::write(dirs.right.path().join("report.png"), b"x").unwrap();

        let plain = align(
            &scan(dirs.left.path()),
            &scan(dirs.right.path()),
            &AlignmentOptions::default(),
        );
        assert_eq!(plain.children.len(), 2);

        let options = AlignmentOptions {
            align_different_extensions: true,
            ..AlignmentOptions::default()
        };
        let aligned = align(&scan(dirs.left.path()), &scan(dirs.right.path()), &options);
        assert_eq!(aligned.children.len(), 1);
        assert!(aligned.children[0].left.is_some() && aligned.children[0].right.is_some());
    }

    #[test]
    fn normalization_key_folds_composed_and_decomposed_forms() {
        assert_eq!(normalization_key("café"), normalization_key("cafe\u{301}"));
        assert_eq!(normalization_key("plain"), "plain");
    }

    #[test]
    fn normalization_alignment_keeps_accented_names_apart() {
        let left = synthetic_result(vec![entry("resume.txt", "resume.txt", false)]);
        let right = synthetic_result(vec![entry("résumé.txt", "résumé.txt", false)]);
        let options = AlignmentOptions {
            align_unicode_normalization: true,
            ..AlignmentOptions::default()
        };
        let tree = align(&left, &right, &options);
        assert_eq!(tree.children.len(), 2);
        assert!(tree.children.iter().all(|node| node.status.is_orphan()));

        let name = "re\u{301}sume\u{301}.txt";
        let decomposed = synthetic_result(vec![entry(name, name, false)]);
        let paired = align(&decomposed, &right, &options);
        assert_eq!(paired.children.len(), 1);
        assert!(paired.children[0].right.is_some());
    }

    #[test]
    fn a_file_facing_a_folder_is_one_node() {
        let left = synthetic_result(vec![entry("Source", "Source", false)]);
        let right = synthetic_result(vec![
            entry("Source", "Source", true),
            entry("Source/inner.txt", "inner.txt", false),
        ]);
        let mut tree = align(&left, &right, &AlignmentOptions::default());
        compare_quick(&mut tree, &CompareOptions::default());

        assert_eq!(tree.children.len(), 1);
        let node = &tree.children[0];
        assert_eq!(node.status, NodeStatus::KindMismatch);
        assert!(node.left.is_some() && node.right.is_some());
        assert!(node.status.is_difference());

        tree.walk(&mut |candidate| {
            let mut rels: BTreeSet<&Path> = BTreeSet::new();
            for child in &candidate.children {
                assert!(
                    rels.insert(child.rel.as_path()),
                    "sibling paths must be unique"
                );
            }
        });
    }

    #[test]
    #[cfg_attr(
        debug_assertions,
        ignore = "wall-clock budget holds for release builds"
    )]
    fn alignment_of_disjoint_names_is_not_quadratic() {
        let names: Vec<String> = (0..50_000).map(|index| format!("l{index}.txt")).collect();
        let left = synthetic_result(names.iter().map(|name| entry(name, name, false)).collect());
        let others: Vec<String> = (0..50_000).map(|index| format!("r{index}.txt")).collect();
        let right = synthetic_result(others.iter().map(|name| entry(name, name, false)).collect());
        let start = Instant::now();
        let tree = align(&left, &right, &AlignmentOptions::default());
        assert_eq!(tree.children.len(), 100_000);
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "alignment too slow"
        );
    }

    /// Correctness half of the disjoint name case, at a size a debug build
    /// handles: no name on either side pairs, so every one becomes its own
    /// child.
    #[test]
    fn disjoint_names_each_become_their_own_child() {
        let names: Vec<String> = (0..2_000).map(|index| format!("l{index}.txt")).collect();
        let left = synthetic_result(names.iter().map(|name| entry(name, name, false)).collect());
        let others: Vec<String> = (0..2_000).map(|index| format!("r{index}.txt")).collect();
        let right = synthetic_result(others.iter().map(|name| entry(name, name, false)).collect());
        let tree = align(&left, &right, &AlignmentOptions::default());
        assert_eq!(tree.children.len(), 4_000);
    }

    #[test]
    fn alignment_stops_when_cancelled() {
        let names: Vec<String> = (0..5_000).map(|index| format!("l{index}.txt")).collect();
        let left = synthetic_result(names.iter().map(|name| entry(name, name, false)).collect());
        let right = ScanResult::default();
        let cancel = Cancel::new();
        cancel.cancel();
        let tree = align_trees(&left, &right, &AlignmentOptions::default(), &cancel);
        assert!(tree.children.is_empty());
    }

    #[test]
    fn a_deep_tree_does_not_exhaust_the_stack() {
        let mut tree = deep_chain(50_000);
        assert_eq!(tree.count(), 50_002);
        let mut seen = 0_usize;
        tree.walk(&mut |_| seen += 1);
        assert_eq!(seen, 50_002);
        rollup(&mut tree);
        apply_filters(
            &mut tree,
            &NameFilters::default(),
            &OtherFilters::default(),
            &FilterContext::default(),
        );
        assert_eq!(tree.count(), 50_002);
        assert!(crate::display::visible(
            &tree,
            crate::display::DisplayFilter::ShowAll,
            crate::display::FolderDisplayFilter::default()
        ));
        drop(tree);
    }

    #[test]
    fn content_comparison_runs_in_parallel_and_streams() {
        let dirs = pair();
        for index in 0_u8..8 {
            let name = format!("f{index}.bin");
            std::fs::write(dirs.left.path().join(&name), vec![index; 1024]).unwrap();
            std::fs::write(dirs.right.path().join(&name), vec![index; 1024]).unwrap();
        }
        std::fs::write(dirs.right.path().join("f0.bin"), vec![99_u8; 1024]).unwrap();
        for index in 0_u8..8 {
            let name = format!("f{index}.bin");
            set_modified(&dirs.left.path().join(&name), 4_000);
            set_modified(&dirs.right.path().join(&name), 4_000);
        }

        let options = CompareOptions {
            quick: QuickTests::default(),
            content: ContentTests {
                enabled: true,
                method: ContentMethod::Binary,
                skip_if_quick_same: false,
                ..ContentTests::default()
            },
        };
        let mut tree = align(
            &scan(dirs.left.path()),
            &scan(dirs.right.path()),
            &AlignmentOptions::default(),
        );
        compare_quick(&mut tree, &options);
        let seen = AtomicUsize::new(0);
        let completed = compare_contents_parallel(
            &mut tree,
            dirs.left.path(),
            dirs.right.path(),
            &options,
            None,
            &Cancel::new(),
            &|_| {
                seen.fetch_add(1, Ordering::SeqCst);
            },
        );
        assert!(completed);
        assert_eq!(seen.load(Ordering::SeqCst), 8);
        assert_eq!(find(&tree, "f0.bin").status, NodeStatus::Different);
        assert_eq!(find(&tree, "f1.bin").status, NodeStatus::Same);
    }

    #[test]
    fn a_cancelled_content_pass_keeps_the_quick_status() {
        let dirs = pair();
        for index in 0..4 {
            let name = format!("f{index}.bin");
            std::fs::write(dirs.left.path().join(&name), b"a").unwrap();
            std::fs::write(dirs.right.path().join(&name), b"b").unwrap();
            set_modified(&dirs.left.path().join(&name), 2_000);
            set_modified(&dirs.right.path().join(&name), 1_000);
        }
        let options = CompareOptions {
            content: ContentTests {
                enabled: true,
                skip_if_quick_same: false,
                ..ContentTests::default()
            },
            ..CompareOptions::default()
        };
        let mut tree = align(
            &scan(dirs.left.path()),
            &scan(dirs.right.path()),
            &AlignmentOptions::default(),
        );
        compare_quick(&mut tree, &options);
        let cancel = Cancel::new();
        cancel.cancel();
        let seen = AtomicUsize::new(0);
        let completed = compare_contents_parallel(
            &mut tree,
            dirs.left.path(),
            dirs.right.path(),
            &options,
            None,
            &cancel,
            &|_| {
                seen.fetch_add(1, Ordering::SeqCst);
            },
        );
        assert!(!completed);
        assert_eq!(seen.load(Ordering::SeqCst), 0);
        for index in 0..4 {
            let node = find(&tree, &format!("f{index}.bin"));
            assert_eq!(node.status, NodeStatus::LeftNewer);
            assert!(node.error.is_none());
        }
    }

    #[test]
    fn content_match_overrides_a_timestamp_mismatch() {
        let dirs = pair();
        std::fs::write(dirs.left.path().join("a.txt"), b"same").unwrap();
        std::fs::write(dirs.right.path().join("a.txt"), b"same").unwrap();
        set_modified(&dirs.left.path().join("a.txt"), 9_000);
        set_modified(&dirs.right.path().join("a.txt"), 1_000);

        let options = CompareOptions {
            content: ContentTests {
                enabled: true,
                override_quick: true,
                ..ContentTests::default()
            },
            ..CompareOptions::default()
        };
        let mut tree = align(
            &scan(dirs.left.path()),
            &scan(dirs.right.path()),
            &AlignmentOptions::default(),
        );
        compare_quick(&mut tree, &options);
        assert_eq!(find(&tree, "a.txt").status, NodeStatus::LeftNewer);
        compare_contents_parallel(
            &mut tree,
            dirs.left.path(),
            dirs.right.path(),
            &options,
            None,
            &Cancel::new(),
            &|_| {},
        );
        assert_eq!(find(&tree, "a.txt").status, NodeStatus::Same);
    }

    #[test]
    fn override_can_be_switched_off() {
        let dirs = pair();
        std::fs::write(dirs.left.path().join("a.txt"), b"same").unwrap();
        std::fs::write(dirs.right.path().join("a.txt"), b"same").unwrap();
        set_modified(&dirs.left.path().join("a.txt"), 9_000);
        set_modified(&dirs.right.path().join("a.txt"), 1_000);

        let options = CompareOptions {
            content: ContentTests {
                enabled: true,
                override_quick: false,
                ..ContentTests::default()
            },
            ..CompareOptions::default()
        };
        let mut tree = align(
            &scan(dirs.left.path()),
            &scan(dirs.right.path()),
            &AlignmentOptions::default(),
        );
        compare_quick(&mut tree, &options);
        compare_contents_parallel(
            &mut tree,
            dirs.left.path(),
            dirs.right.path(),
            &options,
            None,
            &Cancel::new(),
            &|_| {},
        );
        assert_eq!(find(&tree, "a.txt").status, NodeStatus::LeftNewer);
    }

    #[test]
    fn filters_prune_the_tree() {
        let dirs = pair();
        for side in [dirs.left.path(), dirs.right.path()] {
            std::fs::create_dir(side.join("Backup")).unwrap();
            std::fs::write(side.join("Backup/old.txt"), b"x").unwrap();
            std::fs::write(side.join("keep.txt"), b"x").unwrap();
            std::fs::write(side.join("drop.dcu"), b"x").unwrap();
        }
        let mut tree = align(
            &scan(dirs.left.path()),
            &scan(dirs.right.path()),
            &AlignmentOptions::default(),
        );
        compare_quick(&mut tree, &CompareOptions::default());
        apply_filters(
            &mut tree,
            &NameFilters::parse("*.txt;-Backup\\"),
            &OtherFilters::default(),
            &FilterContext::default(),
        );
        let names: Vec<&str> = tree
            .children
            .iter()
            .map(|node| node.name.as_str())
            .collect();
        assert_eq!(names, vec!["keep.txt"]);
    }

    #[test]
    fn an_include_folder_mask_keeps_the_route_to_it() {
        let dirs = pair();
        for side in [dirs.left.path(), dirs.right.path()] {
            std::fs::create_dir_all(side.join("a/Source")).unwrap();
            std::fs::create_dir_all(side.join("b/Other")).unwrap();
            std::fs::write(side.join("a/Source/f.txt"), b"x").unwrap();
            std::fs::write(side.join("b/Other/f.txt"), b"x").unwrap();
        }
        let mut tree = align(
            &scan(dirs.left.path()),
            &scan(dirs.right.path()),
            &AlignmentOptions::default(),
        );
        compare_quick(&mut tree, &CompareOptions::default());
        apply_filters(
            &mut tree,
            &NameFilters::from_lists("", "", "Source", ""),
            &OtherFilters::default(),
            &FilterContext::default(),
        );
        let names: Vec<&str> = tree
            .children
            .iter()
            .map(|node| node.name.as_str())
            .collect();
        assert_eq!(names, vec!["a"]);
        assert_eq!(
            find(&tree, &Path::new("a").join("Source").to_string_lossy()).name,
            "Source"
        );
    }

    #[test]
    fn a_folder_exclude_mask_removes_the_files_inside_it() {
        let dirs = pair();
        for side in [dirs.left.path(), dirs.right.path()] {
            std::fs::create_dir(side.join("Backup")).unwrap();
            std::fs::write(side.join("Backup/a.txt"), b"x").unwrap();
        }
        let filters = NameFilters::parse("-Backup\\");
        assert!(!filters.allows(&Path::new("Backup").join("a.txt"), false));
        let mut tree = align(
            &scan(dirs.left.path()),
            &scan(dirs.right.path()),
            &AlignmentOptions::default(),
        );
        apply_filters(
            &mut tree,
            &filters,
            &OtherFilters::default(),
            &FilterContext::default(),
        );
        assert!(tree.children.is_empty());
    }

    #[test]
    fn other_filters_drop_entries_by_age() {
        let dirs = pair();
        std::fs::write(dirs.left.path().join("old.txt"), b"x").unwrap();
        std::fs::write(dirs.right.path().join("old.txt"), b"x").unwrap();
        set_modified(&dirs.left.path().join("old.txt"), 1_000);
        set_modified(&dirs.right.path().join("old.txt"), 1_000);

        let mut tree = align(
            &scan(dirs.left.path()),
            &scan(dirs.right.path()),
            &AlignmentOptions::default(),
        );
        let context = FilterContext {
            now: SystemTime::UNIX_EPOCH + Duration::from_secs(86_400 * 100),
            local_offset_seconds: 0,
        };
        let filters = OtherFilters {
            items: vec![crate::filter::OtherFilter::ModifiedOlderThan(
                crate::filter::FilterTime::DaysAgo(1),
            )],
            exclude_protected_system: false,
            exclude_hidden: false,
            ..OtherFilters::default()
        };
        apply_filters(&mut tree, &NameFilters::default(), &filters, &context);
        assert!(tree.children.is_empty());
    }

    #[test]
    fn an_alignment_override_pairs_two_differently_named_items() {
        let dirs = pair();
        std::fs::write(dirs.left.path().join("config.dev.ini"), b"x").unwrap();
        std::fs::write(dirs.right.path().join("config.live.ini"), b"x").unwrap();

        let plain = align(
            &scan(dirs.left.path()),
            &scan(dirs.right.path()),
            &AlignmentOptions::default(),
        );
        assert_eq!(plain.children.len(), 2, "no rule pairs nothing");

        let options = AlignmentOptions {
            overrides: vec![crate::compare::AlignmentOverride {
                left: "config.dev.*".to_owned(),
                right: "config.live.*".to_owned(),
                limit_to_folder: String::new(),
            }],
            ..AlignmentOptions::default()
        };
        let paired = align(&scan(dirs.left.path()), &scan(dirs.right.path()), &options);
        assert_eq!(paired.children.len(), 1);
        assert!(paired.children[0].left.is_some() && paired.children[0].right.is_some());
    }

    #[test]
    fn an_alignment_override_outside_its_folder_does_not_apply() {
        let dirs = pair();
        std::fs::write(dirs.left.path().join("a.txt"), b"x").unwrap();
        std::fs::write(dirs.right.path().join("b.txt"), b"x").unwrap();
        let options = AlignmentOptions {
            overrides: vec![crate::compare::AlignmentOverride {
                left: "a.txt".to_owned(),
                right: "b.txt".to_owned(),
                limit_to_folder: "elsewhere".to_owned(),
            }],
            ..AlignmentOptions::default()
        };
        let tree = align(&scan(dirs.left.path()), &scan(dirs.right.path()), &options);
        assert_eq!(tree.children.len(), 2);
    }

    #[test]
    fn a_top_level_orphan_folder_keeps_its_children_only_when_asked() {
        let dirs = pair();
        std::fs::create_dir(dirs.left.path().join("only")).unwrap();
        std::fs::write(dirs.left.path().join("only/inside.txt"), b"x").unwrap();

        let scanning = align(
            &scan(dirs.left.path()),
            &scan(dirs.right.path()),
            &AlignmentOptions::default(),
        );
        assert_eq!(find(&scanning, "only").children.len(), 1);

        let options = AlignmentOptions {
            scan_top_level_orphans: false,
            ..AlignmentOptions::default()
        };
        let skipping = align(&scan(dirs.left.path()), &scan(dirs.right.path()), &options);
        assert!(find(&skipping, "only").children.is_empty());
        assert_eq!(find(&skipping, "only").status, NodeStatus::LeftOrphan);
    }

    #[test]
    fn a_content_filter_drops_entries_by_what_they_hold() {
        let dirs = pair();
        std::fs::write(dirs.left.path().join("hit.txt"), b"alpha marker beta").unwrap();
        std::fs::write(dirs.right.path().join("hit.txt"), b"alpha marker beta").unwrap();
        std::fs::write(dirs.left.path().join("miss.txt"), b"nothing here").unwrap();
        std::fs::write(dirs.right.path().join("miss.txt"), b"nothing here").unwrap();

        let mut tree = align(
            &scan(dirs.left.path()),
            &scan(dirs.right.path()),
            &AlignmentOptions::default(),
        );
        let filters = OtherFilters {
            content: vec![crate::filter::ContentFilter {
                text: "marker".to_owned(),
                not_containing: false,
            }],
            exclude_protected_system: false,
            left_root: dirs.left.path().to_path_buf(),
            right_root: dirs.right.path().to_path_buf(),
            ..OtherFilters::default()
        };
        apply_filters(
            &mut tree,
            &NameFilters::default(),
            &filters,
            &FilterContext::default(),
        );
        let names: Vec<&str> = tree
            .children
            .iter()
            .map(|node| node.name.as_str())
            .collect();
        assert_eq!(names, vec!["miss.txt"]);
    }

    #[test]
    fn rollup_is_idempotent() {
        let dirs = pair();
        std::fs::write(dirs.left.path().join("a.txt"), b"x").unwrap();
        let mut tree = align(
            &scan(dirs.left.path()),
            &scan(dirs.right.path()),
            &AlignmentOptions::default(),
        );
        compare_quick(&mut tree, &CompareOptions::default());
        let first = tree.status;
        rollup(&mut tree);
        assert_eq!(first, tree.status);
    }

    #[test]
    fn flags_record_a_kind_mismatch_as_a_difference() {
        let mut flags = StatusFlags::default();
        flags.record(NodeStatus::KindMismatch);
        assert!(flags.different);
    }
}
