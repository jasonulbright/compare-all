//! The flattened row model of a folder comparison.
//!
//! The engine hands back a tree whose depth follows the file system, so every
//! traversal here runs on an explicit stack. The tree is copied once into an
//! arena of indexed nodes; after that a row is eight bytes, which is what keeps
//! a half million entry comparison cheap to filter, flatten and scroll.

use ca_fs::{
    ContentOutcome, ContentUpdate, DisplayFilter, FolderDisplayFilter, Node, NodeStatus,
    StatusFlags,
};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// What one side of a pair carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SideInfo {
    /// Size in bytes; zero for a folder.
    pub size: u64,
    /// Last modified time, where the file system reported one.
    pub modified: Option<SystemTime>,
}

/// One node of the comparison.
#[derive(Debug, Clone)]
pub struct ArenaNode {
    /// Name as it appears in the tree.
    pub name: String,
    /// Path relative to the two base folders.
    pub rel: PathBuf,
    /// True for a folder on either side.
    pub is_dir: bool,
    /// Current comparison status.
    pub status: NodeStatus,
    /// Left side facts, absent when the item is a right orphan.
    pub left: Option<SideInfo>,
    /// Right side facts, absent when the item is a left orphan.
    pub right: Option<SideInfo>,
    /// Result of a content comparison, once one has run.
    pub content: Option<ContentOutcome>,
    /// Indexes of this node's children, in display order.
    pub children: Vec<u32>,
    /// Distance from a base folder.
    pub depth: u16,
    /// True when this folder's listing was not read in full: unreadable,
    /// stopped early, or standing in for content that is not local.
    pub incomplete: bool,
    /// True when the folder itself carries an error of its own, such as a
    /// name its source refuses, which the roll up keeps.
    pub own_error: bool,
}

/// Which column a tree is ordered by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortColumn {
    /// The name column, which is the order the engine produced.
    #[default]
    Name,
    /// The size column, taking the larger of the two sides.
    Size,
    /// The modified column, taking the later of the two sides.
    Modified,
}

/// Which way a column is ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortDirection {
    /// Smallest, earliest or first alphabetically at the top.
    #[default]
    Ascending,
    /// The reverse.
    Descending,
}

impl SortDirection {
    /// The other direction.
    #[must_use]
    pub const fn flipped(self) -> Self {
        match self {
            SortDirection::Ascending => SortDirection::Descending,
            SortDirection::Descending => SortDirection::Ascending,
        }
    }

    /// The indicator drawn beside a sorted column's heading.
    #[must_use]
    pub const fn indicator(self) -> &'static str {
        match self {
            SortDirection::Ascending => "\u{23f6}",
            SortDirection::Descending => "\u{23f7}",
        }
    }
}

/// How a tree is ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Sort {
    /// The column ordered by.
    pub column: SortColumn,
    /// The direction that column runs in.
    pub direction: SortDirection,
}

impl Sort {
    /// The order a click on `column` produces, given the order in force.
    ///
    /// Clicking the column already sorted by reverses it; clicking another
    /// starts that one ascending.
    #[must_use]
    pub fn clicked(self, column: SortColumn) -> Self {
        if self.column == column {
            Self {
                column,
                direction: self.direction.flipped(),
            }
        } else {
            Self {
                column,
                direction: SortDirection::Ascending,
            }
        }
    }
}

/// One row of the flattened tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlatRow {
    /// Index of the node the row shows.
    pub node: u32,
    /// Indent level.
    pub depth: u16,
}

/// Which folders are open.
#[derive(Debug, Clone, Default)]
pub struct Expanded(HashSet<u32>);

impl Expanded {
    /// Nothing open.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// True when the folder is open.
    #[must_use]
    pub fn is_expanded(&self, node: u32) -> bool {
        self.0.contains(&node)
    }

    /// Open a folder.
    pub fn expand(&mut self, node: u32) {
        self.0.insert(node);
    }

    /// Close a folder.
    pub fn collapse(&mut self, node: u32) {
        self.0.remove(&node);
    }

    /// Open a closed folder, close an open one.
    pub fn toggle(&mut self, node: u32) {
        if !self.0.remove(&node) {
            self.0.insert(node);
        }
    }

    /// Open every folder of `arena`.
    pub fn expand_all(&mut self, arena: &Arena) {
        for (index, node) in arena.nodes().iter().enumerate() {
            if node.is_dir {
                #[allow(clippy::cast_possible_truncation)]
                self.0.insert(index as u32);
            }
        }
    }

    /// Open every folder whose own status or contained-status hints report a
    /// difference, leaving the folders that match closed.
    pub fn expand_differences(&mut self, arena: &Arena) {
        let nodes = arena.nodes();
        // A parent always precedes its children, so one reverse pass settles
        // every folder before the folder that holds it is read.
        let mut holds: Vec<bool> = vec![false; nodes.len()];
        for index in (0..nodes.len()).rev() {
            let Some(node) = nodes.get(index) else {
                continue;
            };
            let mut found = node.status.is_difference();
            for child in &node.children {
                let child = *child as usize;
                found |= holds.get(child).copied().unwrap_or(false)
                    || nodes
                        .get(child)
                        .is_some_and(|child| child.status.is_difference());
            }
            if let Some(slot) = holds.get_mut(index) {
                *slot = found;
            }
            if node.is_dir && found {
                #[allow(clippy::cast_possible_truncation)]
                self.0.insert(index as u32);
            }
        }
    }

    /// Close every folder.
    pub fn collapse_all(&mut self) {
        self.0.clear();
    }
}

/// The comparison as indexed nodes.
#[derive(Debug, Clone, Default)]
pub struct Arena {
    nodes: Vec<ArenaNode>,
    roots: Vec<u32>,
    by_rel: HashMap<PathBuf, u32>,
    /// Counting every node is a pass over the whole comparison, so the answer
    /// is kept until something changes it.
    totals: std::cell::Cell<Option<Totals>>,
}

impl Arena {
    /// Copy an engine tree into an arena, dropping the synthetic root.
    #[must_use]
    pub fn from_root(root: &Node) -> Self {
        let mut arena = Self::default();
        // Children are pushed before they are visited, so the arena holds the
        // tree in pre-order and a reverse scan visits every child before its
        // parent.
        let mut stack: Vec<(&Node, u16, Option<u32>)> = root
            .children
            .iter()
            .rev()
            .map(|child| (child, 0u16, None))
            .collect();
        while let Some((node, depth, parent)) = stack.pop() {
            #[allow(clippy::cast_possible_truncation)]
            let index = arena.nodes.len() as u32;
            arena.nodes.push(ArenaNode {
                name: node.name.clone(),
                rel: node.rel.clone(),
                is_dir: node.is_dir,
                status: node.status,
                left: node.left.as_ref().map(side_info),
                right: node.right.as_ref().map(side_info),
                content: node.content,
                children: Vec::new(),
                depth,
                incomplete: node.incomplete
                    || node
                        .left
                        .as_ref()
                        .is_some_and(|entry| entry.listing_incomplete)
                    || node
                        .right
                        .as_ref()
                        .is_some_and(|entry| entry.listing_incomplete),
                own_error: node.is_dir && node.error.is_some(),
            });
            arena.by_rel.insert(node.rel.clone(), index);
            match parent {
                Some(parent) => arena.nodes[parent as usize].children.push(index),
                None => arena.roots.push(index),
            }
            for child in node.children.iter().rev() {
                stack.push((child, depth.saturating_add(1), Some(index)));
            }
        }
        arena
    }

    /// Every node, in pre-order.
    #[must_use]
    pub fn nodes(&self) -> &[ArenaNode] {
        &self.nodes
    }

    /// One node.
    #[must_use]
    pub fn node(&self, index: u32) -> Option<&ArenaNode> {
        self.nodes.get(index as usize)
    }

    /// The top level nodes.
    #[must_use]
    pub fn roots(&self) -> &[u32] {
        &self.roots
    }

    /// How many nodes the comparison holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// True when the comparison holds nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The node at a relative path.
    #[must_use]
    pub fn index_of(&self, rel: &Path) -> Option<u32> {
        self.by_rel.get(rel).copied()
    }

    /// Fold one streamed content result into the tree.
    ///
    /// Folder statuses are left stale; [`Arena::roll_up`] refreshes them once
    /// per batch, because rolling up costs a pass over every node and results
    /// arrive one file at a time.
    ///
    /// Returns false when the result names a pair this comparison does not
    /// hold, which happens when a result arrives after a rescan replaced the
    /// tree it was computed against.
    pub fn apply_content(&mut self, update: &ContentUpdate, ignore_unimportant: bool) -> bool {
        let Some(index) = self.index_of(&update.rel) else {
            return false;
        };
        self.totals.set(None);
        let node = &mut self.nodes[index as usize];
        match update.outcome {
            Ok(outcome) => {
                node.content = Some(outcome);
                if !outcome.is_not_compared() {
                    node.status = if outcome.is_same(ignore_unimportant) {
                        NodeStatus::Same
                    } else {
                        NodeStatus::Different
                    };
                }
            }
            Err(_) => {
                node.status = NodeStatus::Error;
            }
        }
        true
    }

    /// Recompute every folder's status from its descendants, by the rule the
    /// engine's roll up applies ([`ca_fs::folder_status`]).
    ///
    /// The flags of every descendant decide, not the statuses of the direct
    /// children alone: a child folder that reports an error hides whether a
    /// file under it is newer on one side, and that file with a sibling newer
    /// on the other side makes the parent different.
    pub fn roll_up(&mut self) {
        self.totals.set(None);
        let mut below = vec![StatusFlags::default(); self.nodes.len()];
        for index in (0..self.nodes.len()).rev() {
            let node = &self.nodes[index];
            if !node.is_dir || node.children.is_empty() {
                continue;
            }
            let mut flags = StatusFlags::default();
            for child in &node.children {
                let child = *child as usize;
                flags.record(self.nodes[child].status);
                flags.merge(below[child]);
            }
            below[index] = flags;
            if node.status.is_structural() {
                continue;
            }
            let own_error = node.own_error;
            self.nodes[index].status = ca_fs::folder_status(flags, own_error);
        }
    }

    /// Which nodes the two display filters show.
    ///
    /// A folder is shown when its own status passes, or when it still holds
    /// something visible, so the route to a visible file is never hidden.
    #[must_use]
    pub fn visibility(&self, filter: DisplayFilter, folder: FolderDisplayFilter) -> Vec<bool> {
        let mut visible = vec![false; self.nodes.len()];
        for index in (0..self.nodes.len()).rev() {
            let node = &self.nodes[index];
            visible[index] = if node.is_dir {
                let has_visible_child = node.children.iter().any(|child| visible[*child as usize]);
                ca_fs::display::folder_visible(folder, filter, node.status, has_visible_child)
            } else {
                filter.shows(node.status)
            };
        }
        visible
    }

    /// The rows to paint, given which folders are open and which nodes pass the
    /// filters.
    #[must_use]
    pub fn flatten(&self, expanded: &Expanded, visible: &[bool]) -> Vec<FlatRow> {
        let mut rows = Vec::new();
        let mut stack: Vec<u32> = self.roots.iter().rev().copied().collect();
        while let Some(index) = stack.pop() {
            if !visible.get(index as usize).copied().unwrap_or(true) {
                continue;
            }
            let node = &self.nodes[index as usize];
            rows.push(FlatRow {
                node: index,
                depth: node.depth,
            });
            if node.is_dir && expanded.is_expanded(index) {
                for child in node.children.iter().rev() {
                    stack.push(*child);
                }
            }
        }
        rows
    }

    /// Row totals for the status bar.
    ///
    /// Counted once and kept until a mutation drops the answer, because the
    /// status bar asks every frame and the count is a pass over every node.
    #[must_use]
    pub fn totals(&self) -> Totals {
        if let Some(totals) = self.totals.get() {
            return totals;
        }
        let mut totals = Totals::default();
        for node in &self.nodes {
            if node.incomplete {
                totals.incomplete += 1;
            }
            if node.is_dir {
                totals.folders += 1;
                continue;
            }
            totals.files += 1;
            match node.status {
                NodeStatus::Same => totals.same += 1,
                NodeStatus::NotCompared => totals.not_compared += 1,
                NodeStatus::LeftOrphan | NodeStatus::RightOrphan => totals.orphans += 1,
                NodeStatus::Error => totals.errors += 1,
                NodeStatus::KindMismatch => {
                    totals.errors += 1;
                    totals.kind_mismatches += 1;
                }
                _ => totals.differences += 1,
            }
        }
        self.totals.set(Some(totals));
        totals
    }

    /// True when the totals would have to be counted again.
    #[must_use]
    pub fn totals_are_stale(&self) -> bool {
        self.totals.get().is_none()
    }

    /// Reorder every folder's children by a column, folders before files.
    ///
    /// A pass over the whole comparison, so it belongs on a worker for a large
    /// tree; nothing here touches a frame.
    pub fn sort(&mut self, sort: Sort) {
        let keys: Vec<SortKey> = self.nodes.iter().map(SortKey::of).collect();
        let order = |left: &u32, right: &u32| {
            let (left, right) = (&keys[*left as usize], &keys[*right as usize]);
            left.compare(right, sort)
        };
        let mut roots = std::mem::take(&mut self.roots);
        roots.sort_by(order);
        self.roots = roots;
        for index in 0..self.nodes.len() {
            if self.nodes[index].children.len() < 2 {
                continue;
            }
            let mut children = std::mem::take(&mut self.nodes[index].children);
            children.sort_by(order);
            self.nodes[index].children = children;
        }
    }
}

/// What ordering a row compares on, lifted out of the node so a sort does not
/// pay for a string clone per comparison.
struct SortKey {
    is_dir: bool,
    name: String,
    size: u64,
    modified: Option<SystemTime>,
}

impl SortKey {
    fn of(node: &ArenaNode) -> Self {
        let size = node
            .left
            .map(|side| side.size)
            .max(node.right.map(|side| side.size))
            .unwrap_or(0);
        let modified = node
            .left
            .and_then(|side| side.modified)
            .max(node.right.and_then(|side| side.modified));
        Self {
            is_dir: node.is_dir,
            name: node.name.to_lowercase(),
            size,
            modified,
        }
    }

    /// Folders always precede files, whatever the column and direction, so a
    /// reversed sort never buries the tree's structure among its leaves.
    fn compare(&self, other: &Self, sort: Sort) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        match (self.is_dir, other.is_dir) {
            (true, false) => return Ordering::Less,
            (false, true) => return Ordering::Greater,
            _ => {}
        }
        let ordering = match sort.column {
            SortColumn::Name => self.name.cmp(&other.name),
            SortColumn::Size => self
                .size
                .cmp(&other.size)
                .then_with(|| self.name.cmp(&other.name)),
            SortColumn::Modified => self
                .modified
                .cmp(&other.modified)
                .then_with(|| self.name.cmp(&other.name)),
        };
        match sort.direction {
            SortDirection::Ascending => ordering,
            SortDirection::Descending => ordering.reverse(),
        }
    }
}

/// What the status bar reports about a comparison.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Totals {
    /// Folders in the comparison.
    pub folders: usize,
    /// Files in the comparison.
    pub files: usize,
    /// Files that match.
    pub same: usize,
    /// Files that differ.
    pub differences: usize,
    /// Files present on one side only.
    pub orphans: usize,
    /// Files no comparison has reached yet.
    pub not_compared: usize,
    /// Files that could not be compared.
    pub errors: usize,
    /// Entries whose listing was not read in full.
    pub incomplete: usize,
    /// Pairs that are a file on one side and a folder on the other.
    pub kind_mismatches: usize,
}

fn side_info(entry: &ca_fs::Entry) -> SideInfo {
    SideInfo {
        size: entry.size,
        modified: entry.modified,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{Arena, Expanded, Sort, SortColumn, SortDirection};
    use ca_fs::{
        Attributes, ContentOutcome, ContentUpdate, DisplayFilter, Entry, FolderDisplayFilter, Node,
        NodeStatus,
    };
    use std::path::PathBuf;
    use std::time::Instant;

    fn entry(rel: &str, is_dir: bool, size: u64) -> Entry {
        Entry {
            rel: PathBuf::from(rel),
            name: rel.rsplit('/').next().unwrap_or(rel).to_string(),
            is_dir,
            size,
            modified: None,
            created: None,
            attributes: Attributes::default(),
            link: None,
            error: None,
            listing_incomplete: false,
            refused: false,
        }
    }

    fn node(rel: &str, is_dir: bool, status: NodeStatus, children: Vec<Node>) -> Node {
        let one_sided = status == NodeStatus::LeftOrphan;
        Node {
            name: rel.rsplit('/').next().unwrap_or(rel).to_string(),
            rel: PathBuf::from(rel),
            is_dir,
            left: Some(entry(rel, is_dir, 10)),
            right: (!one_sided).then(|| entry(rel, is_dir, 10)),
            facts: ca_fs::PairFacts::default(),
            status,
            flags: ca_fs::StatusFlags::default(),
            quick: None,
            content: None,
            error: None,
            incomplete: false,
            scan_cancelled: false,
            children,
        }
    }

    fn file(rel: &str, status: NodeStatus) -> Node {
        node(rel, false, status, Vec::new())
    }

    fn folder(rel: &str, children: Vec<Node>) -> Node {
        node(rel, true, NodeStatus::NotCompared, children)
    }

    fn sample() -> Node {
        folder(
            "",
            vec![
                folder(
                    "src",
                    vec![
                        file("src/a.rs", NodeStatus::Same),
                        file("src/b.rs", NodeStatus::Different),
                    ],
                ),
                file("readme.md", NodeStatus::LeftOrphan),
            ],
        )
    }

    #[test]
    fn the_arena_holds_the_tree_in_pre_order() {
        let arena = Arena::from_root(&sample());
        let names: Vec<&str> = arena
            .nodes()
            .iter()
            .map(|node| node.name.as_str())
            .collect();
        assert_eq!(names, vec!["src", "a.rs", "b.rs", "readme.md"]);
        assert_eq!(arena.roots(), &[0, 3]);
        assert_eq!(arena.node(1).unwrap().depth, 1);
    }

    #[test]
    fn a_closed_folder_hides_its_children() {
        let arena = Arena::from_root(&sample());
        let visible = arena.visibility(DisplayFilter::ShowAll, FolderDisplayFilter::default());
        let expanded = Expanded::new();
        let rows = arena.flatten(&expanded, &visible);
        assert_eq!(rows.len(), 2);
        assert_eq!(arena.node(rows[0].node).unwrap().name, "src");
        assert_eq!(arena.node(rows[1].node).unwrap().name, "readme.md");
    }

    #[test]
    fn expanding_reveals_children_in_order() {
        let arena = Arena::from_root(&sample());
        let visible = arena.visibility(DisplayFilter::ShowAll, FolderDisplayFilter::default());
        let mut expanded = Expanded::new();
        expanded.expand(0);
        let rows = arena.flatten(&expanded, &visible);
        let names: Vec<&str> = rows
            .iter()
            .map(|row| arena.node(row.node).unwrap().name.as_str())
            .collect();
        assert_eq!(names, vec!["src", "a.rs", "b.rs", "readme.md"]);
        assert_eq!(rows[1].depth, 1);
    }

    #[test]
    fn expand_all_then_collapse_all_round_trips() {
        let arena = Arena::from_root(&sample());
        let visible = arena.visibility(DisplayFilter::ShowAll, FolderDisplayFilter::default());
        let mut expanded = Expanded::new();
        expanded.expand_all(&arena);
        assert_eq!(arena.flatten(&expanded, &visible).len(), 4);
        expanded.collapse_all();
        assert_eq!(arena.flatten(&expanded, &visible).len(), 2);
    }

    #[test]
    fn toggling_alternates() {
        let mut expanded = Expanded::new();
        expanded.toggle(3);
        assert!(expanded.is_expanded(3));
        expanded.toggle(3);
        assert!(!expanded.is_expanded(3));
    }

    #[test]
    fn a_filter_keeps_the_route_to_a_visible_file() {
        let arena = Arena::from_root(&sample());
        let visible = arena.visibility(
            DisplayFilter::ShowDifferences,
            FolderDisplayFilter::CompareFilesAndFolderStructure,
        );
        let mut expanded = Expanded::new();
        expanded.expand_all(&arena);
        let names: Vec<&str> = arena
            .flatten(&expanded, &visible)
            .iter()
            .map(|row| arena.node(row.node).unwrap().name.as_str())
            .collect();
        assert_eq!(names, vec!["src", "b.rs", "readme.md"]);
    }

    #[test]
    fn showing_only_matches_drops_the_folder_holding_none() {
        let arena = Arena::from_root(&folder(
            "",
            vec![folder("only", vec![file("only/x", NodeStatus::Different)])],
        ));
        let visible = arena.visibility(
            DisplayFilter::ShowSame,
            FolderDisplayFilter::OnlyCompareFiles,
        );
        let mut expanded = Expanded::new();
        expanded.expand_all(&arena);
        assert!(arena.flatten(&expanded, &visible).is_empty());
    }

    #[test]
    fn a_content_result_updates_the_pair_and_its_folder() {
        let mut arena = Arena::from_root(&sample());
        let index = arena.index_of(&PathBuf::from("src/b.rs")).unwrap();
        let applied = arena.apply_content(
            &ContentUpdate {
                rel: PathBuf::from("src/b.rs"),
                outcome: Ok(ContentOutcome::BinarySame),
            },
            false,
        );
        assert!(applied);
        arena.roll_up();
        assert_eq!(arena.node(index).unwrap().status, NodeStatus::Same);
        assert_eq!(arena.node(0).unwrap().status, NodeStatus::Same);
    }

    #[test]
    fn a_result_for_an_unknown_pair_is_refused() {
        let mut arena = Arena::from_root(&sample());
        let applied = arena.apply_content(
            &ContentUpdate {
                rel: PathBuf::from("gone.txt"),
                outcome: Ok(ContentOutcome::BinarySame),
            },
            false,
        );
        assert!(!applied);
    }

    #[test]
    fn totals_are_kept_until_something_changes_them() {
        let mut arena = Arena::from_root(&sample());
        assert!(arena.totals_are_stale());
        let first = arena.totals();
        assert!(!arena.totals_are_stale());
        assert_eq!(arena.totals(), first);
        arena.apply_content(
            &ContentUpdate {
                rel: PathBuf::from("src/b.rs"),
                outcome: Ok(ContentOutcome::BinarySame),
            },
            false,
        );
        assert!(arena.totals_are_stale());
        assert_eq!(arena.totals().same, 2);
    }

    #[test]
    fn a_refused_content_result_leaves_the_totals_alone() {
        let mut arena = Arena::from_root(&sample());
        let before = arena.totals();
        assert!(!arena.apply_content(
            &ContentUpdate {
                rel: PathBuf::from("gone.txt"),
                outcome: Ok(ContentOutcome::BinarySame),
            },
            false,
        ));
        assert!(!arena.totals_are_stale());
        assert_eq!(arena.totals(), before);
    }

    #[test]
    fn an_incomplete_listing_is_carried_onto_the_node_and_counted() {
        let mut tree = folder("", vec![folder("deep", Vec::new())]);
        tree.children[0].incomplete = true;
        let arena = Arena::from_root(&tree);
        assert!(arena.node(0).unwrap().incomplete);
        assert_eq!(arena.totals().incomplete, 1);
    }

    #[test]
    fn an_entry_marked_unreadable_by_the_scan_is_carried_too() {
        let mut tree = folder("", vec![folder("deep", Vec::new())]);
        if let Some(entry) = tree.children[0].left.as_mut() {
            entry.listing_incomplete = true;
        }
        let arena = Arena::from_root(&tree);
        assert!(arena.node(0).unwrap().incomplete);
    }

    #[test]
    fn a_kind_mismatch_is_counted_on_its_own() {
        let arena = Arena::from_root(&folder("", vec![file("clash", NodeStatus::KindMismatch)]));
        let totals = arena.totals();
        assert_eq!(totals.kind_mismatches, 1);
        assert_eq!(totals.errors, 1);
    }

    #[test]
    fn sorting_by_name_orders_folders_before_files() {
        let mut arena = Arena::from_root(&folder(
            "",
            vec![
                file("zebra", NodeStatus::Same),
                folder("mid", Vec::new()),
                file("alpha", NodeStatus::Same),
            ],
        ));
        arena.sort(Sort::default());
        let names: Vec<&str> = arena
            .roots()
            .iter()
            .map(|index| arena.node(*index).unwrap().name.as_str())
            .collect();
        assert_eq!(names, vec!["mid", "alpha", "zebra"]);
    }

    #[test]
    fn a_reversed_sort_still_keeps_folders_first() {
        let mut arena = Arena::from_root(&folder(
            "",
            vec![
                file("zebra", NodeStatus::Same),
                folder("mid", Vec::new()),
                file("alpha", NodeStatus::Same),
            ],
        ));
        arena.sort(Sort {
            column: SortColumn::Name,
            direction: SortDirection::Descending,
        });
        let names: Vec<&str> = arena
            .roots()
            .iter()
            .map(|index| arena.node(*index).unwrap().name.as_str())
            .collect();
        assert_eq!(names, vec!["mid", "zebra", "alpha"]);
    }

    #[test]
    fn sorting_reaches_every_level_of_the_tree() {
        let mut arena = Arena::from_root(&folder(
            "",
            vec![folder(
                "src",
                vec![
                    file("src/z.rs", NodeStatus::Same),
                    file("src/a.rs", NodeStatus::Same),
                ],
            )],
        ));
        arena.sort(Sort::default());
        let mut expanded = Expanded::new();
        expanded.expand_all(&arena);
        let visible = arena.visibility(DisplayFilter::ShowAll, FolderDisplayFilter::default());
        let names: Vec<&str> = arena
            .flatten(&expanded, &visible)
            .iter()
            .map(|row| arena.node(row.node).unwrap().name.as_str())
            .collect();
        assert_eq!(names, vec!["src", "a.rs", "z.rs"]);
    }

    #[test]
    fn a_click_reverses_the_column_already_sorted_and_starts_any_other() {
        let sort = Sort::default();
        let again = sort.clicked(SortColumn::Name);
        assert_eq!(again.direction, SortDirection::Descending);
        let other = again.clicked(SortColumn::Size);
        assert_eq!(other.column, SortColumn::Size);
        assert_eq!(other.direction, SortDirection::Ascending);
    }

    #[test]
    fn sorting_by_size_takes_the_larger_side() {
        let mut small = file("small", NodeStatus::Different);
        if let Some(entry) = small.left.as_mut() {
            entry.size = 1;
        }
        if let Some(entry) = small.right.as_mut() {
            entry.size = 2;
        }
        let mut large = file("large", NodeStatus::Different);
        if let Some(entry) = large.left.as_mut() {
            entry.size = 900;
        }
        if let Some(entry) = large.right.as_mut() {
            entry.size = 5;
        }
        let mut arena = Arena::from_root(&folder("", vec![small, large]));
        arena.sort(Sort {
            column: SortColumn::Size,
            direction: SortDirection::Ascending,
        });
        let names: Vec<&str> = arena
            .roots()
            .iter()
            .map(|index| arena.node(*index).unwrap().name.as_str())
            .collect();
        assert_eq!(names, vec!["small", "large"]);
    }

    #[test]
    fn totals_separate_folders_from_files() {
        let arena = Arena::from_root(&sample());
        let totals = arena.totals();
        assert_eq!(totals.folders, 1);
        assert_eq!(totals.files, 3);
        assert_eq!(totals.same, 1);
        assert_eq!(totals.differences, 1);
        assert_eq!(totals.orphans, 1);
    }

    /// Build a tree of `folders` times `each` files, flatten it twice and
    /// report the arena size, the row count and the filtered row count.
    fn flatten_counts(folders: usize, each: usize) -> (usize, usize, usize) {
        let mut roots = Vec::new();
        for outer in 0..folders {
            let children = (0..each)
                .map(|inner| {
                    file(
                        &format!("d{outer}/f{inner}"),
                        if inner % 2 == 0 {
                            NodeStatus::Same
                        } else {
                            NodeStatus::Different
                        },
                    )
                })
                .collect();
            roots.push(folder(&format!("d{outer}"), children));
        }
        let tree = folder("", roots);
        let arena = Arena::from_root(&tree);
        let visible = arena.visibility(DisplayFilter::ShowAll, FolderDisplayFilter::default());
        let mut expanded = Expanded::new();
        expanded.expand_all(&arena);
        let rows = arena.flatten(&expanded, &visible);
        let differences = arena.visibility(
            DisplayFilter::ShowDifferences,
            FolderDisplayFilter::CompareFilesAndFolderStructure,
        );
        let filtered = arena.flatten(&expanded, &differences);
        (arena.len(), rows.len(), filtered.len())
    }

    #[test]
    fn a_wide_tree_flattens_every_entry_and_filters_the_same_ones_out() {
        let (entries, rows, filtered) = flatten_counts(5, 100);
        assert_eq!(entries, 505);
        assert_eq!(rows, 505);
        assert_eq!(filtered, 255);
    }

    #[test]
    #[cfg_attr(
        debug_assertions,
        ignore = "wall-clock budget holds for release builds"
    )]
    fn half_a_million_entries_flatten_within_the_budget() {
        let started = Instant::now();
        let (entries, rows, filtered) = flatten_counts(500, 1_000);
        assert_eq!(entries, 500_500);
        assert_eq!(rows, 500_500);
        assert_eq!(filtered, 250_500);
        assert!(
            started.elapsed().as_secs() < 20,
            "half a million entries took {:?}",
            started.elapsed()
        );
    }
}
