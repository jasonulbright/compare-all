//! Moving the cursor of a folder comparison to a difference or to a name.
//!
//! A search walks the arena in pre-order, which is display order, so it
//! reaches rows inside folders that are closed. A match opens the folders
//! above it. Rows the display filters hide are never visited.

use crate::find::{self, Request};
use crate::selection::ClickKind;
use crate::tree::ArenaNode;
use crate::FolderView;
use ca_fs::Side;

/// Why a move to a difference has nowhere to go.
pub const NO_DIFFERENCE: &str = "There is no difference to move to";

/// True for a row that counts as a difference when moving between them.
///
/// A folder counts only when it exists on one side or has the wrong kind,
/// because every other folder differs only through rows below it.
fn is_difference(node: &ArenaNode) -> bool {
    node.status.is_difference() && (!node.is_dir || node.status.is_structural())
}

impl FolderView {
    /// True when some row the filters show passes `test`.
    fn any_reachable(&self, test: &impl Fn(&ArenaNode) -> bool) -> bool {
        self.arena
            .nodes()
            .iter()
            .enumerate()
            .any(|(index, node)| test(node) && self.is_reachable(index))
    }

    /// True when the filters show the node and every folder above it.
    fn is_reachable(&self, index: usize) -> bool {
        let shown = |index: usize| self.visible.get(index).copied().unwrap_or(false);
        if !shown(index) {
            return false;
        }
        let Some(node) = self.arena.nodes().get(index) else {
            return false;
        };
        let mut parent = node.rel.parent();
        while let Some(rel) = parent {
            if rel.as_os_str().is_empty() {
                break;
            }
            match self.arena.index_of(rel) {
                Some(above) if shown(above as usize) => {}
                _ => return false,
            }
            parent = rel.parent();
        }
        true
    }

    /// The nearest node after or before the cursor that passes `test`.
    fn next_node(
        &self,
        forward: bool,
        wrap: bool,
        inclusive: bool,
        test: &impl Fn(&ArenaNode) -> bool,
    ) -> Option<usize> {
        let count = self.arena.len();
        let at = self
            .selection
            .cursor()
            .and_then(|(_, rel)| self.arena.index_of(rel))
            .map(|index| index as usize);
        let order: Box<dyn Iterator<Item = usize>> = if forward {
            let start = at.map_or(0, |at| at + usize::from(!inclusive)).min(count);
            if wrap {
                Box::new((start..count).chain(0..start))
            } else {
                Box::new(start..count)
            }
        } else {
            let end = at.unwrap_or(count).min(count);
            if wrap {
                Box::new((0..end).rev().chain((end..count).rev()))
            } else {
                Box::new((0..end).rev())
            }
        };
        let mut order = order;
        order.find(|index| {
            self.arena.nodes().get(*index).is_some_and(test) && self.is_reachable(*index)
        })
    }

    /// Put the cursor on a node, opening the folders above it.
    fn reveal_node(&mut self, index: usize) {
        let Some(node) = self.arena.nodes().get(index) else {
            return;
        };
        let (left, right) = (node.left.is_some(), node.right.is_some());
        let mut opened = false;
        let mut parent = node.rel.parent();
        while let Some(rel) = parent {
            if rel.as_os_str().is_empty() {
                break;
            }
            if let Some(above) = self.arena.index_of(rel) {
                if !self.expanded.is_expanded(above) {
                    self.expanded.expand(above);
                    opened = true;
                }
            }
            parent = rel.parent();
        }
        if opened {
            self.rebuild();
        }
        let Ok(index) = u32::try_from(index) else {
            return;
        };
        let Some(position) = self.rows.iter().position(|row| row.node == index) else {
            return;
        };
        let preferred = self.selection.cursor().map_or(Side::Left, |(side, _)| side);
        let side = match (left, right) {
            (true, false) => Side::Left,
            (false, true) => Side::Right,
            _ => preferred,
        };
        self.selection
            .click(side, position, &self.rows, &self.arena, ClickKind::Replace);
        self.reveal_cursor();
    }

    /// True when a move to the next or previous difference has a target.
    pub(crate) fn has_difference(&self) -> bool {
        self.any_reachable(&is_difference)
    }

    /// Move the cursor to the next or previous difference.
    pub(crate) fn step_difference(&mut self, forward: bool) {
        if let Some(index) = self.next_node(forward, false, false, &is_difference) {
            self.reveal_node(index);
        }
    }

    /// True when the search text matches a name the filters show.
    pub(crate) fn has_name_match(&self) -> bool {
        self.find.has_text() && self.any_reachable(&|node| self.find.matches(&node.name))
    }

    /// Move the cursor to the next or previous name that holds the search
    /// text, wrapping at either end.
    pub(crate) fn step_name(&mut self, forward: bool, inclusive: bool) {
        let found = {
            let find = &self.find;
            self.next_node(forward, true, inclusive, &|node| find.matches(&node.name))
        };
        if let Some(index) = found {
            self.reveal_node(index);
        }
    }

    /// The reason a name search cannot move, where it cannot.
    pub(crate) fn find_refusal(&self, repeat: bool) -> Option<&'static str> {
        if self.arena.is_empty() {
            Some(find::NO_ITEMS)
        } else if !repeat {
            None
        } else if !self.find.has_text() {
            Some(find::NO_TEXT)
        } else if !self.has_name_match() {
            Some(find::NO_MATCH)
        } else {
            None
        }
    }

    /// Draw the search bar when it is open and act on what it asked for.
    pub(crate) fn find_bar(&mut self, ui: &mut egui::Ui) {
        if !self.find.is_open() {
            return;
        }
        let note = (self.find.has_text() && !self.has_name_match()).then_some(find::NO_MATCH);
        let id = self.id;
        let mut request = None;
        egui::TopBottomPanel::bottom(id.with("find-panel")).show_inside(ui, |ui| {
            request = find::show(ui, id, &mut self.find, note);
        });
        match request {
            Some(Request::Edited) => self.step_name(true, true),
            Some(Request::Next) => self.step_name(true, false),
            Some(Request::Previous) => self.step_name(false, false),
            Some(Request::Close) => self.find.close(),
            None => {}
        }
    }
}
