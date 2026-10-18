//! Moving the cursor of a folder comparison to a difference or to a name.
//!
//! A search walks the rows in the order they paint with every folder open,
//! so it follows the sort in force and reaches rows inside folders that are
//! closed. A match opens the folders above it. Rows the display filters
//! hide, and rows below a folder they hide, are never visited.

use crate::find::{self, Request};
use crate::selection::ClickKind;
use crate::tree::{Arena, ArenaNode};
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

/// Where a node stands in the order of shown nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    /// The node is shown at this position.
    Shown(u32),
    /// The node is hidden, and this many shown nodes come before it in the
    /// sorted order with nothing hidden.
    Hidden(u32),
}

/// The shown nodes in row order with every folder open, and the place of
/// every node, shown or hidden, in that order.
///
/// Built with the rows, because the menus ask every frame whether a move has
/// a target and a walk of the tree per question is a pass over every node.
#[derive(Debug, Clone, Default)]
pub(crate) struct DisplayOrder {
    order: Vec<u32>,
    place: Vec<Option<Place>>,
}

impl DisplayOrder {
    /// The order of `arena` under the visibility `visible`.
    pub(crate) fn of(arena: &Arena, visible: &[bool]) -> Self {
        let order = arena.display_order(visible);
        let mut place = vec![None; arena.len()];
        for (position, index) in order.iter().enumerate() {
            if let (Some(slot), Ok(position)) =
                (place.get_mut(*index as usize), u32::try_from(position))
            {
                *slot = Some(Place::Shown(position));
            }
        }
        // The shown order is a subsequence of the unfiltered walk, because
        // both walks visit children in the same sorted order.
        let mut before = 0u32;
        for index in arena.display_order(&[]) {
            match place.get_mut(index as usize) {
                Some(Some(Place::Shown(_))) => before = before.saturating_add(1),
                Some(slot) => *slot = Some(Place::Hidden(before)),
                None => {}
            }
        }
        Self { order, place }
    }

    /// Forget the order, as when the comparison starts again.
    pub(crate) fn clear(&mut self) {
        self.order.clear();
        self.place.clear();
    }

    /// The place of the node `index`, where the arena holds it.
    fn place_of(&self, index: u32) -> Option<Place> {
        self.place.get(index as usize).copied().flatten()
    }
}

impl FolderView {
    /// True when some row the filters show passes `test`.
    fn any_shown(&self, test: &impl Fn(&ArenaNode) -> bool) -> bool {
        self.order
            .order
            .iter()
            .any(|index| self.arena.node(*index).is_some_and(test))
    }

    /// The nearest node after or before the cursor, in row order, that passes
    /// `test`.
    fn next_node(
        &self,
        forward: bool,
        wrap: bool,
        inclusive: bool,
        test: &impl Fn(&ArenaNode) -> bool,
    ) -> Option<usize> {
        let order = &self.order.order;
        let count = order.len();
        let place = self
            .selection
            .cursor()
            .and_then(|(_, rel)| self.arena.index_of(rel))
            .and_then(|index| self.order.place_of(index));
        let (start, end) = match place {
            Some(Place::Shown(at)) => (at as usize + usize::from(!inclusive), at as usize),
            Some(Place::Hidden(before)) => (before as usize, before as usize),
            None => (0, count),
        };
        let positions: Box<dyn Iterator<Item = usize>> = if forward {
            let start = start.min(count);
            if wrap {
                Box::new((start..count).chain(0..start))
            } else {
                Box::new(start..count)
            }
        } else {
            let end = end.min(count);
            if wrap {
                Box::new((0..end).rev().chain((end..count).rev()))
            } else {
                Box::new((0..end).rev())
            }
        };
        positions
            .filter_map(|position| order.get(position).copied())
            .find(|index| self.arena.node(*index).is_some_and(test))
            .map(|index| index as usize)
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

    /// True when a move to the next or previous difference has a target. The
    /// move does not wrap, so each direction runs out on its own.
    pub(crate) fn has_difference(&self, forward: bool) -> bool {
        self.next_node(forward, false, false, &is_difference)
            .is_some()
    }

    /// Move the cursor to the next or previous difference.
    pub(crate) fn step_difference(&mut self, forward: bool) {
        if let Some(index) = self.next_node(forward, false, false, &is_difference) {
            self.reveal_node(index);
        }
    }

    /// True when the search text matches a name the filters show.
    pub(crate) fn has_name_match(&self) -> bool {
        self.find.has_text() && self.any_shown(&|node| self.find.matches(&node.name))
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use crate::selection::ClickKind;
    use crate::tree::SortColumn;
    use crate::FolderView;
    use ca_fs::{DisplayFilter, Side};
    use ca_ui::command::Command;
    use ca_ui::view::SessionView;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    const PATIENCE: Duration = Duration::from_secs(30);

    fn poll_until(view: &mut FolderView, ready: impl Fn(&FolderView) -> bool) {
        let deadline = Instant::now() + PATIENCE;
        loop {
            view.tick();
            if ready(view) {
                return;
            }
            assert!(Instant::now() < deadline, "the view never settled");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Three differing files whose name order and size order disagree,
    /// sorted by size.
    fn sized_view(dir: &Path) -> FolderView {
        for side in ["left", "right", "journals"] {
            std::fs::create_dir_all(dir.join(side)).unwrap();
        }
        for (name, size) in [
            ("a-match.txt", 30),
            ("b-match.txt", 10),
            ("c-match.txt", 20),
        ] {
            std::fs::write(dir.join("left").join(name), vec![b'l'; size]).unwrap();
            std::fs::write(dir.join("right").join(name), vec![b'r'; size + 1]).unwrap();
        }
        let mut view = FolderView::with_journal_directory(
            dir.join("left"),
            dir.join("right"),
            &ca_ui::testing::context(),
            1,
            dir.join("journals"),
        );
        poll_until(&mut view, FolderView::is_ready);
        view.set_sort(SortColumn::Size);
        poll_until(&mut view, |view| view.sort_job.is_none());
        view
    }

    fn cursor(view: &FolderView) -> PathBuf {
        view.selection().cursor().unwrap().1.to_path_buf()
    }

    fn shown(view: &FolderView) -> Vec<PathBuf> {
        view.rows()
            .iter()
            .map(|row| view.arena.node(row.node).unwrap().rel.clone())
            .collect()
    }

    #[test]
    fn difference_moves_follow_the_sorted_rows() {
        let dir = tempfile::tempdir().unwrap();
        let mut view = sized_view(dir.path());
        let rows = shown(&view);
        assert_eq!(
            rows,
            ["b-match.txt", "c-match.txt", "a-match.txt"].map(PathBuf::from)
        );
        let mut seen = Vec::new();
        while view.accepts(Command::NextDifference) && seen.len() < 4 {
            view.run(Command::NextDifference);
            seen.push(cursor(&view));
        }
        assert_eq!(seen, rows);
        view.run(Command::PreviousDifference);
        assert_eq!(cursor(&view), rows[1]);
    }

    /// Four files sorted by size into b, c, a, d. The files b and a differ;
    /// c and d match.
    fn mixed_view(dir: &Path, sync: bool) -> FolderView {
        for side in ["left", "right", "journals"] {
            std::fs::create_dir_all(dir.join(side)).unwrap();
        }
        for (name, size, differs) in [
            ("a-match.txt", 30, true),
            ("b-match.txt", 10, true),
            ("c-match.txt", 20, false),
            ("d-match.txt", 40, false),
        ] {
            std::fs::write(dir.join("left").join(name), vec![b'l'; size]).unwrap();
            let right = if differs {
                vec![b'r'; size + 1]
            } else {
                vec![b'l'; size]
            };
            std::fs::write(dir.join("right").join(name), right).unwrap();
        }
        let (left, right, journals) = (dir.join("left"), dir.join("right"), dir.join("journals"));
        let context = ca_ui::testing::context();
        let mut view = if sync {
            FolderView::sync_with_journal_directory(left, right, &context, 1, journals)
        } else {
            FolderView::with_journal_directory(left, right, &context, 1, journals)
        };
        poll_until(&mut view, FolderView::is_ready);
        view.set_sort(SortColumn::Size);
        poll_until(&mut view, |view| view.sort_job.is_none());
        assert_eq!(
            shown(&view),
            ["b-match.txt", "c-match.txt", "a-match.txt", "d-match.txt"].map(PathBuf::from)
        );
        view
    }

    /// Put the cursor on `name` with every row shown, then apply `filter`.
    fn hide_cursor(view: &mut FolderView, name: &str, filter: DisplayFilter) {
        view.filter = DisplayFilter::ShowAll;
        view.rebuild();
        let position = shown(view)
            .iter()
            .position(|rel| rel == Path::new(name))
            .unwrap();
        view.selection.click(
            Side::Left,
            position,
            &view.rows,
            &view.arena,
            ClickKind::Replace,
        );
        view.filter = filter;
        view.rebuild();
        assert!(!shown(view).contains(&PathBuf::from(name)));
        assert_eq!(cursor(view), PathBuf::from(name));
    }

    enum Tab {
        Compare(FolderView),
        Sync(crate::FolderSyncView),
    }

    impl Tab {
        fn view(&mut self) -> &mut FolderView {
            match self {
                Self::Compare(view) => view,
                Self::Sync(tab) => tab.view_mut(),
            }
        }

        fn run(&mut self, command: Command) {
            assert!(self.view().accepts(command), "{command:?} was refused");
            match self {
                Self::Compare(view) => view.run(command),
                Self::Sync(tab) => tab.run(command),
            }
        }
    }

    fn moves_continue_from_a_hidden_cursor(sync: bool) {
        let dir = tempfile::tempdir().unwrap();
        let view = mixed_view(dir.path(), sync);
        let mut tab = if sync {
            Tab::Sync(crate::FolderSyncView::from(view))
        } else {
            Tab::Compare(view)
        };

        hide_cursor(tab.view(), "c-match.txt", DisplayFilter::ShowDifferences);
        tab.run(Command::NextDifference);
        assert_eq!(cursor(tab.view()), PathBuf::from("a-match.txt"));
        hide_cursor(tab.view(), "c-match.txt", DisplayFilter::ShowDifferences);
        tab.run(Command::PreviousDifference);
        assert_eq!(cursor(tab.view()), PathBuf::from("b-match.txt"));

        tab.view().set_find_text("match");
        hide_cursor(tab.view(), "a-match.txt", DisplayFilter::ShowSame);
        tab.run(Command::FindNext);
        assert_eq!(cursor(tab.view()), PathBuf::from("d-match.txt"));
        hide_cursor(tab.view(), "a-match.txt", DisplayFilter::ShowSame);
        tab.run(Command::FindPrevious);
        assert_eq!(cursor(tab.view()), PathBuf::from("c-match.txt"));
    }

    #[test]
    fn compare_moves_continue_from_a_cursor_the_filter_hides() {
        moves_continue_from_a_hidden_cursor(false);
    }

    #[test]
    fn sync_moves_continue_from_a_cursor_the_filter_hides() {
        moves_continue_from_a_hidden_cursor(true);
    }

    #[test]
    fn name_search_follows_the_sorted_rows_and_wraps() {
        let dir = tempfile::tempdir().unwrap();
        let mut view = sized_view(dir.path());
        let rows = shown(&view);
        view.set_find_text("match");
        let mut seen = Vec::new();
        for _ in 0..4 {
            view.run(Command::FindNext);
            seen.push(cursor(&view));
        }
        assert_eq!(seen[..3], rows[..]);
        assert_eq!(seen[3], rows[0], "the search did not wrap to the top row");
        view.run(Command::FindPrevious);
        assert_eq!(cursor(&view), rows[2], "the search did not wrap back");
    }
}
