//! Which rows are selected, on which side.
//!
//! A selection names nodes by their relative path rather than by row position,
//! so a sort, a filter change or a rescan does not move it. Each side carries
//! its own set, because the operations act on one side's items or on both.

use crate::tree::{Arena, FlatRow};
use ca_fs::{NodeStatus, Side};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// A named group of rows a command selects at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectRule {
    /// Every visible row, files and folders.
    All,
    /// Every visible file.
    AllFiles,
    /// Every visible file that is newer on the chosen side.
    Newer,
    /// Every visible item present only on the chosen side.
    Orphans,
    /// Every visible item whose two sides differ.
    Differences,
}

impl SelectRule {
    /// The label a menu entry shows.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "Select All",
            Self::AllFiles => "Select All Files",
            Self::Newer => "Select Newer",
            Self::Orphans => "Select Orphans",
            Self::Differences => "Select Differences",
        }
    }

    /// True when the rule takes a node on `side`.
    fn takes(self, side: Side, is_dir: bool, status: NodeStatus, present: bool) -> bool {
        if !present {
            return false;
        }
        match self {
            Self::All => true,
            Self::AllFiles => !is_dir,
            Self::Newer => match side {
                Side::Left => status == NodeStatus::LeftNewer,
                Side::Right => status == NodeStatus::RightNewer,
            },
            Self::Orphans => match side {
                Side::Left => status == NodeStatus::LeftOrphan,
                Side::Right => status == NodeStatus::RightOrphan,
            },
            Self::Differences => !matches!(status, NodeStatus::Same | NodeStatus::NotCompared),
        }
    }
}

/// Which sides a select command or an operation covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The left side only.
    Left,
    /// The right side only.
    Right,
    /// Both sides at once.
    Both,
}

impl Scope {
    /// The individual sides this scope covers.
    #[must_use]
    pub const fn sides(self) -> &'static [Side] {
        match self {
            Self::Left => &[Side::Left],
            Self::Right => &[Side::Right],
            Self::Both => &[Side::Left, Side::Right],
        }
    }

    /// The label a menu entry shows.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Left => "Left Side",
            Self::Right => "Right Side",
            Self::Both => "Both Sides",
        }
    }
}

/// What the pointer or the keyboard asked of a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickKind {
    /// Replace the selection with this row.
    Replace,
    /// Add this row to the selection, or take it out again.
    Toggle,
    /// Extend the selection from the anchor to this row.
    Range,
}

/// The rows selected on each side.
#[derive(Debug, Clone, Default)]
pub struct Selection {
    left: BTreeSet<PathBuf>,
    right: BTreeSet<PathBuf>,
    /// Row the next range extends from, with the side it was chosen on.
    anchor: Option<(Side, PathBuf)>,
    /// Row the keyboard sits on.
    cursor: Option<(Side, PathBuf)>,
}

impl Selection {
    /// Nothing selected.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The set for one side.
    #[must_use]
    pub fn side(&self, side: Side) -> &BTreeSet<PathBuf> {
        match side {
            Side::Left => &self.left,
            Side::Right => &self.right,
        }
    }

    fn side_mut(&mut self, side: Side) -> &mut BTreeSet<PathBuf> {
        match side {
            Side::Left => &mut self.left,
            Side::Right => &mut self.right,
        }
    }

    /// True when the node at `rel` is selected on `side`.
    #[must_use]
    pub fn contains(&self, side: Side, rel: &Path) -> bool {
        self.side(side).contains(rel)
    }

    /// True when the node at `rel` is selected on either side.
    #[must_use]
    pub fn contains_either(&self, rel: &Path) -> bool {
        self.left.contains(rel) || self.right.contains(rel)
    }

    /// How many rows are selected across both sides.
    #[must_use]
    pub fn len(&self) -> usize {
        self.left.len() + self.right.len()
    }

    /// True when nothing is selected.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.left.is_empty() && self.right.is_empty()
    }

    /// The sides that hold at least one selected row.
    #[must_use]
    pub fn occupied_scope(&self) -> Option<Scope> {
        match (self.left.is_empty(), self.right.is_empty()) {
            (true, true) => None,
            (false, true) => Some(Scope::Left),
            (true, false) => Some(Scope::Right),
            (false, false) => Some(Scope::Both),
        }
    }

    /// The row the keyboard sits on.
    #[must_use]
    pub fn cursor(&self) -> Option<(Side, &Path)> {
        self.cursor
            .as_ref()
            .map(|(side, rel)| (*side, rel.as_path()))
    }

    /// Put the keyboard on one row without changing what is selected.
    pub fn set_cursor(&mut self, side: Side, rel: PathBuf) {
        self.cursor = Some((side, rel));
    }

    /// Drop every selected row.
    pub fn clear(&mut self) {
        self.left.clear();
        self.right.clear();
        self.anchor = None;
    }

    /// Keep only the rows the comparison still holds.
    ///
    /// A rescan replaces the tree, so a path that is gone would otherwise stay
    /// selected and be planned against.
    pub fn retain_known(&mut self, arena: &Arena) {
        self.left.retain(|rel| arena.index_of(rel).is_some());
        self.right.retain(|rel| arena.index_of(rel).is_some());
        if let Some((_, rel)) = &self.anchor {
            if arena.index_of(rel).is_none() {
                self.anchor = None;
            }
        }
        if let Some((_, rel)) = &self.cursor {
            if arena.index_of(rel).is_none() {
                self.cursor = None;
            }
        }
    }

    /// Apply one click on the row at `position` of `rows`.
    pub fn click(
        &mut self,
        side: Side,
        position: usize,
        rows: &[FlatRow],
        arena: &Arena,
        kind: ClickKind,
    ) {
        let Some(rel) = rel_at(rows, arena, position) else {
            return;
        };
        match kind {
            ClickKind::Replace => {
                self.clear();
                self.side_mut(side).insert(rel.clone());
                self.anchor = Some((side, rel.clone()));
            }
            ClickKind::Toggle => {
                if !self.side_mut(side).insert(rel.clone()) {
                    self.side_mut(side).remove(&rel);
                }
                self.anchor = Some((side, rel.clone()));
            }
            ClickKind::Range => {
                let anchor = self
                    .anchor
                    .clone()
                    .and_then(|(_, anchor_rel)| position_of(rows, arena, &anchor_rel))
                    .unwrap_or(position);
                let (first, last) = if anchor <= position {
                    (anchor, position)
                } else {
                    (position, anchor)
                };
                self.side_mut(side).clear();
                for step in first..=last {
                    if let Some(rel) = rel_at(rows, arena, step) {
                        self.side_mut(side).insert(rel);
                    }
                }
            }
        }
        self.cursor = Some((side, rel));
    }

    /// Select every visible row a rule takes, on the sides a scope covers.
    pub fn select_rule(&mut self, rule: SelectRule, scope: Scope, rows: &[FlatRow], arena: &Arena) {
        self.clear();
        for row in rows {
            let Some(node) = arena.node(row.node) else {
                continue;
            };
            for side in scope.sides() {
                let present = match side {
                    Side::Left => node.left.is_some(),
                    Side::Right => node.right.is_some(),
                };
                if rule.takes(*side, node.is_dir, node.status, present) {
                    self.side_mut(*side).insert(node.rel.clone());
                }
            }
        }
    }

    /// Narrow an existing selection to one side.
    pub fn restrict_to(&mut self, side: Side) {
        match side {
            Side::Left => self.right.clear(),
            Side::Right => self.left.clear(),
        }
    }

    /// Move the keyboard row by `delta`, optionally extending the selection.
    pub fn move_cursor(&mut self, delta: isize, extend: bool, rows: &[FlatRow], arena: &Arena) {
        if rows.is_empty() {
            return;
        }
        let side = self.cursor.as_ref().map_or(Side::Left, |(side, _)| *side);
        let current = self
            .cursor
            .as_ref()
            .and_then(|(_, rel)| position_of(rows, arena, rel));
        let next = match current {
            Some(position) => {
                let candidate = isize::try_from(position).unwrap_or(0).saturating_add(delta);
                usize::try_from(candidate.max(0)).unwrap_or(0)
            }
            None => 0,
        }
        .min(rows.len().saturating_sub(1));
        let kind = if extend {
            ClickKind::Range
        } else {
            ClickKind::Replace
        };
        self.click(side, next, rows, arena, kind);
    }

    /// The row the cursor sits on, as a position in `rows`.
    #[must_use]
    pub fn cursor_position(&self, rows: &[FlatRow], arena: &Arena) -> Option<usize> {
        let (_, rel) = self.cursor.as_ref()?;
        position_of(rows, arena, rel)
    }
}

/// The relative path of the node one row shows.
fn rel_at(rows: &[FlatRow], arena: &Arena, position: usize) -> Option<PathBuf> {
    let row = rows.get(position)?;
    arena.node(row.node).map(|node| node.rel.clone())
}

/// Where a node sits in the rows on screen, when it is on screen at all.
fn position_of(rows: &[FlatRow], arena: &Arena, rel: &Path) -> Option<usize> {
    let index = arena.index_of(rel)?;
    rows.iter().position(|row| row.node == index)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{ClickKind, Scope, SelectRule, Selection};
    use crate::tree::{Arena, Expanded};
    use ca_fs::{
        Attributes, DisplayFilter, Entry, FolderDisplayFilter, Node, NodeStatus, Side, StatusFlags,
    };
    use std::path::{Path, PathBuf};

    fn entry(rel: &str) -> Entry {
        Entry {
            rel: PathBuf::from(rel),
            name: rel.rsplit('/').next().unwrap_or(rel).to_string(),
            is_dir: false,
            size: 4,
            modified: None,
            created: None,
            attributes: Attributes::default(),
            link: None,
            error: None,
            listing_incomplete: false,
            refused: false,
        }
    }

    fn file(rel: &str, status: NodeStatus) -> Node {
        Node {
            name: rel.rsplit('/').next().unwrap_or(rel).to_string(),
            rel: PathBuf::from(rel),
            is_dir: false,
            left: (status != NodeStatus::RightOrphan).then(|| entry(rel)),
            right: (status != NodeStatus::LeftOrphan).then(|| entry(rel)),
            facts: ca_fs::PairFacts::default(),
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

    fn root() -> Node {
        Node {
            name: String::new(),
            rel: PathBuf::new(),
            is_dir: true,
            left: None,
            right: None,
            facts: ca_fs::PairFacts::default(),
            status: NodeStatus::NotCompared,
            flags: StatusFlags::default(),
            quick: None,
            content: None,
            error: None,
            incomplete: false,
            scan_cancelled: false,
            children: vec![
                file("a.txt", NodeStatus::Same),
                file("b.txt", NodeStatus::LeftNewer),
                file("c.txt", NodeStatus::LeftOrphan),
                file("d.txt", NodeStatus::RightOrphan),
            ],
        }
    }

    fn model() -> (Arena, Vec<crate::tree::FlatRow>) {
        let arena = Arena::from_root(&root());
        let visible = arena.visibility(DisplayFilter::ShowAll, FolderDisplayFilter::default());
        let mut expanded = Expanded::new();
        expanded.expand_all(&arena);
        let rows = arena.flatten(&expanded, &visible);
        (arena, rows)
    }

    #[test]
    fn a_plain_click_replaces_the_selection_on_one_side() {
        let (arena, rows) = model();
        let mut selection = Selection::new();
        selection.click(Side::Left, 0, &rows, &arena, ClickKind::Replace);
        selection.click(Side::Left, 1, &rows, &arena, ClickKind::Replace);
        assert_eq!(selection.len(), 1);
        assert!(selection.contains(Side::Left, Path::new("b.txt")));
        assert!(!selection.contains(Side::Right, Path::new("b.txt")));
    }

    #[test]
    fn a_toggle_click_adds_and_removes() {
        let (arena, rows) = model();
        let mut selection = Selection::new();
        selection.click(Side::Left, 0, &rows, &arena, ClickKind::Replace);
        selection.click(Side::Left, 2, &rows, &arena, ClickKind::Toggle);
        assert_eq!(selection.len(), 2);
        selection.click(Side::Left, 2, &rows, &arena, ClickKind::Toggle);
        assert_eq!(selection.len(), 1);
    }

    #[test]
    fn a_range_click_covers_every_row_between_the_anchor_and_the_click() {
        let (arena, rows) = model();
        let mut selection = Selection::new();
        selection.click(Side::Right, 3, &rows, &arena, ClickKind::Replace);
        selection.click(Side::Right, 1, &rows, &arena, ClickKind::Range);
        assert_eq!(selection.side(Side::Right).len(), 3);
        assert!(selection.contains(Side::Right, Path::new("b.txt")));
        assert!(selection.contains(Side::Right, Path::new("d.txt")));
        assert!(!selection.contains(Side::Right, Path::new("a.txt")));
    }

    #[test]
    fn select_all_covers_both_sides_and_restricting_drops_one() {
        let (arena, rows) = model();
        let mut selection = Selection::new();
        selection.select_rule(SelectRule::All, Scope::Both, &rows, &arena);
        // Each side only takes the rows it actually holds, so the orphans
        // count once rather than twice.
        assert_eq!(selection.side(Side::Left).len(), 3);
        assert_eq!(selection.side(Side::Right).len(), 3);
        selection.restrict_to(Side::Left);
        assert_eq!(selection.side(Side::Right).len(), 0);
    }

    #[test]
    fn select_newer_takes_only_the_chosen_side() {
        let (arena, rows) = model();
        let mut selection = Selection::new();
        selection.select_rule(SelectRule::Newer, Scope::Left, &rows, &arena);
        assert_eq!(selection.side(Side::Left).len(), 1);
        assert!(selection.contains(Side::Left, Path::new("b.txt")));
        selection.select_rule(SelectRule::Newer, Scope::Right, &rows, &arena);
        assert!(selection.is_empty());
    }

    #[test]
    fn select_orphans_takes_the_side_the_item_is_on() {
        let (arena, rows) = model();
        let mut selection = Selection::new();
        selection.select_rule(SelectRule::Orphans, Scope::Both, &rows, &arena);
        assert!(selection.contains(Side::Left, Path::new("c.txt")));
        assert!(selection.contains(Side::Right, Path::new("d.txt")));
        assert_eq!(selection.len(), 2);
    }

    #[test]
    fn keyboard_range_selection_extends_from_the_cursor() {
        let (arena, rows) = model();
        let mut selection = Selection::new();
        selection.click(Side::Left, 0, &rows, &arena, ClickKind::Replace);
        selection.move_cursor(1, true, &rows, &arena);
        selection.move_cursor(1, true, &rows, &arena);
        assert_eq!(selection.side(Side::Left).len(), 3);
        assert_eq!(selection.cursor_position(&rows, &arena), Some(2));
    }

    #[test]
    fn a_rescan_drops_paths_the_comparison_no_longer_holds() {
        let (arena, rows) = model();
        let mut selection = Selection::new();
        selection.select_rule(SelectRule::All, Scope::Both, &rows, &arena);
        let mut smaller = root();
        smaller.children = vec![file("a.txt", NodeStatus::Same)];
        let narrower = Arena::from_root(&smaller);
        selection.retain_known(&narrower);
        assert_eq!(
            selection.side(Side::Left),
            &std::collections::BTreeSet::from([PathBuf::from("a.txt")])
        );
    }

    #[test]
    fn an_occupied_scope_names_the_sides_holding_rows() {
        let (arena, rows) = model();
        let mut selection = Selection::new();
        assert_eq!(selection.occupied_scope(), None);
        selection.click(Side::Left, 0, &rows, &arena, ClickKind::Replace);
        assert_eq!(selection.occupied_scope(), Some(Scope::Left));
        selection.click(Side::Right, 1, &rows, &arena, ClickKind::Toggle);
        assert_eq!(selection.occupied_scope(), Some(Scope::Both));
    }
}
