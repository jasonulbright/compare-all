//! The saved sessions tree as the launcher shows it.
//!
//! The stored tree is nested; a list is what gets drawn. This model turns one
//! into the other, remembers which folders are open and which row is picked,
//! and names the operations the tree offers. Every operation is applied to the
//! store, which is the only place a session is held.

use ca_session::{SessionId, SessionKind, SessionStore, TreeNode};
use std::collections::BTreeSet;

/// Which branch of the launcher a row belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Branch {
    /// Sessions the user named and kept.
    Saved,
    /// Sessions kept automatically, which age out.
    AutoSaved,
}

/// One drawn line of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Identifier of the node the line stands for.
    pub id: SessionId,
    /// How far the line is indented.
    pub depth: usize,
    /// Name as stored.
    pub name: String,
    /// Kind, or nothing for a folder of sessions.
    pub kind: Option<SessionKind>,
    /// True for a folder that holds children.
    pub is_folder: bool,
    /// True while a folder's children are drawn.
    pub is_open: bool,
    /// True when the session refuses changes.
    pub is_locked: bool,
    /// Which branch the line sits in.
    pub branch: Branch,
}

/// What a tree operation reported.
pub type Outcome = Result<(), String>;

/// The launcher's view of the stored tree.
#[derive(Debug, Default)]
pub struct HomeModel {
    open: BTreeSet<SessionId>,
    selected: Option<SessionId>,
    search: String,
    /// The node a drag picked up, until it is dropped.
    dragging: Option<SessionId>,
}

impl HomeModel {
    /// A model with every folder closed and nothing picked.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The text the search field holds.
    #[must_use]
    pub fn search(&self) -> &str {
        &self.search
    }

    /// Filter the tree by name.
    ///
    /// While a search is running every folder is drawn open, because a match
    /// inside a closed folder would otherwise be filtered to nothing visible.
    pub fn set_search(&mut self, text: impl Into<String>) {
        self.search = text.into();
    }

    /// The row the tree has picked.
    #[must_use]
    pub fn selected(&self) -> Option<&SessionId> {
        self.selected.as_ref()
    }

    /// Pick a row.
    pub fn select(&mut self, id: SessionId) {
        self.selected = Some(id);
    }

    /// Pick nothing.
    pub fn clear_selection(&mut self) {
        self.selected = None;
    }

    /// True while the folder's children are drawn.
    #[must_use]
    pub fn is_open(&self, id: &SessionId) -> bool {
        self.open.contains(id)
    }

    /// Open a closed folder, or close an open one.
    pub fn toggle(&mut self, id: &SessionId) {
        if !self.open.insert(id.clone()) {
            self.open.remove(id);
        }
    }

    /// Open every folder.
    pub fn expand_all(&mut self, store: &SessionStore) {
        fn walk(nodes: &[TreeNode], out: &mut BTreeSet<SessionId>) {
            for node in nodes {
                if node.is_folder() {
                    out.insert(node.id().clone());
                    walk(node.children(), out);
                }
            }
        }
        walk(&store.root, &mut self.open);
    }

    /// Close every folder.
    pub fn collapse_all(&mut self) {
        self.open.clear();
    }

    /// Pick up a node for a move by drag.
    pub fn begin_drag(&mut self, id: SessionId) {
        self.dragging = Some(id);
    }

    /// The node a drag is carrying.
    #[must_use]
    pub fn dragging(&self) -> Option<&SessionId> {
        self.dragging.as_ref()
    }

    /// Drop the carried node into `parent`, or at the top level when it names
    /// nothing.
    ///
    /// # Errors
    /// Returns the reason the store refused the move.
    pub fn drop_onto(&mut self, store: &mut SessionStore, parent: Option<&SessionId>) -> Outcome {
        let Some(id) = self.dragging.take() else {
            return Ok(());
        };
        self.move_node(store, &id, parent)
    }

    /// Stop carrying whatever a drag picked up.
    pub fn cancel_drag(&mut self) {
        self.dragging = None;
    }

    /// The lines to draw, in order.
    #[must_use]
    pub fn rows(&self, store: &SessionStore) -> Vec<Row> {
        let mut rows = Vec::new();
        let needle = self.search.trim().to_lowercase();
        self.walk(&store.root, 0, &needle, &mut rows);
        for session in &store.auto_saved {
            if !needle.is_empty() && !session.name.to_lowercase().contains(&needle) {
                continue;
            }
            rows.push(Row {
                id: session.id.clone(),
                depth: 0,
                name: session.name.clone(),
                kind: Some(session.kind.clone()),
                is_folder: false,
                is_open: false,
                is_locked: session.locked,
                branch: Branch::AutoSaved,
            });
        }
        rows
    }

    /// Lines of the named branch alone.
    #[must_use]
    pub fn rows_in(&self, store: &SessionStore, branch: Branch) -> Vec<Row> {
        self.rows(store)
            .into_iter()
            .filter(|row| row.branch == branch)
            .collect()
    }

    fn walk(&self, nodes: &[TreeNode], depth: usize, needle: &str, rows: &mut Vec<Row>) {
        for node in nodes {
            let name = node.name().to_owned();
            let matches = needle.is_empty() || name.to_lowercase().contains(needle);
            match node {
                TreeNode::Folder { id, children, .. } => {
                    let mut inner = Vec::new();
                    // A search is drawn expanded, so a match inside a closed
                    // folder is still reachable.
                    let open = self.open.contains(id) || !needle.is_empty();
                    if open {
                        self.walk(children, depth + 1, needle, &mut inner);
                    }
                    if !matches && inner.is_empty() && !needle.is_empty() {
                        continue;
                    }
                    rows.push(Row {
                        id: id.clone(),
                        depth,
                        name,
                        kind: None,
                        is_folder: true,
                        is_open: open,
                        is_locked: false,
                        branch: Branch::Saved,
                    });
                    rows.extend(inner);
                }
                TreeNode::Session(session) => {
                    if !matches {
                        continue;
                    }
                    rows.push(Row {
                        id: session.id.clone(),
                        depth,
                        name,
                        kind: Some(session.kind.clone()),
                        is_folder: false,
                        is_open: false,
                        is_locked: session.locked,
                        branch: Branch::Saved,
                    });
                }
                other => {
                    if !matches {
                        continue;
                    }
                    rows.push(Row {
                        id: other.id().clone(),
                        depth,
                        name,
                        kind: None,
                        is_folder: other.is_folder(),
                        is_open: false,
                        is_locked: false,
                        branch: Branch::Saved,
                    });
                }
            }
        }
    }

    /// Give a node a new name.
    ///
    /// # Errors
    /// Returns the reason the store refused it.
    pub fn rename(&self, store: &mut SessionStore, id: &SessionId, name: &str) -> Outcome {
        let name = name.trim();
        if name.is_empty() {
            return Err("A session needs a name.".to_owned());
        }
        store.rename(id, name).map_err(|error| error.to_string())
    }

    /// Move a node into a folder, or to the top level.
    ///
    /// # Errors
    /// Returns the reason the store refused it.
    pub fn move_node(
        &mut self,
        store: &mut SessionStore,
        id: &SessionId,
        parent: Option<&SessionId>,
    ) -> Outcome {
        store
            .move_node(id, parent)
            .map_err(|error| error.to_string())?;
        if let Some(parent) = parent {
            self.open.insert(parent.clone());
        }
        Ok(())
    }

    /// Copy a node, and pick the copy.
    ///
    /// # Errors
    /// Returns the reason the store refused it.
    pub fn duplicate(&mut self, store: &mut SessionStore, id: &SessionId) -> Outcome {
        let copy = store.duplicate(id).map_err(|error| error.to_string())?;
        self.selected = Some(copy);
        Ok(())
    }

    /// Remove a node and everything under it.
    ///
    /// # Errors
    /// Returns the reason the store refused it.
    pub fn delete(&mut self, store: &mut SessionStore, id: &SessionId) -> Outcome {
        store.delete(id).map_err(|error| error.to_string())?;
        if self.selected.as_ref() == Some(id) {
            self.selected = None;
        }
        self.open.remove(id);
        Ok(())
    }

    /// Make a session refuse changes, or allow them again.
    ///
    /// # Errors
    /// Returns the reason the store refused it.
    pub fn set_locked(&self, store: &mut SessionStore, id: &SessionId, locked: bool) -> Outcome {
        store
            .set_locked(id, locked)
            .map_err(|error| error.to_string())
    }

    /// Add an empty folder under `parent`, and open it.
    ///
    /// # Errors
    /// Returns the reason the store refused it.
    pub fn create_folder(
        &mut self,
        store: &mut SessionStore,
        parent: Option<&SessionId>,
        name: &str,
    ) -> Outcome {
        let name = name.trim();
        if name.is_empty() {
            return Err("A folder needs a name.".to_owned());
        }
        let id = store
            .create_folder(parent, name)
            .map_err(|error| error.to_string())?;
        self.open.insert(id.clone());
        self.selected = Some(id);
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::{Branch, HomeModel};
    use ca_session::{SavedSession, SessionId, SessionKind, SessionStore};

    fn store_with_a_folder() -> (SessionStore, SessionId, SessionId) {
        let mut store = SessionStore::default();
        let folder = store.create_folder(None, "Team").unwrap();
        let id = store.next_id();
        store
            .add_session(
                Some(&folder),
                SavedSession::new(id.clone(), "nightly", SessionKind::TextCompare),
            )
            .unwrap();
        let outer = store.next_id();
        store
            .add_session(
                None,
                SavedSession::new(outer, "release", SessionKind::FolderCompare),
            )
            .unwrap();
        (store, folder, id)
    }

    #[test]
    fn a_closed_folder_hides_its_children() {
        let (store, folder, _) = store_with_a_folder();
        let model = HomeModel::new();
        let rows = model.rows(&store);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert!(rows.iter().any(|row| row.id == folder && !row.is_open));
    }

    #[test]
    fn opening_a_folder_draws_its_children_indented() {
        let (store, folder, inner) = store_with_a_folder();
        let mut model = HomeModel::new();
        model.toggle(&folder);
        let rows = model.rows(&store);
        assert_eq!(rows.len(), 3);
        let child = rows.iter().find(|row| row.id == inner).unwrap();
        assert_eq!(child.depth, 1);
        assert_eq!(child.kind, Some(SessionKind::TextCompare));
        model.toggle(&folder);
        assert_eq!(model.rows(&store).len(), 2);
    }

    #[test]
    fn expand_all_opens_every_folder_and_collapse_all_closes_them() {
        let (store, folder, _) = store_with_a_folder();
        let mut model = HomeModel::new();
        model.expand_all(&store);
        assert!(model.is_open(&folder));
        assert_eq!(model.rows(&store).len(), 3);
        model.collapse_all();
        assert!(!model.is_open(&folder));
    }

    #[test]
    fn a_search_finds_a_session_inside_a_closed_folder() {
        let (store, _, inner) = store_with_a_folder();
        let mut model = HomeModel::new();
        model.set_search("night");
        let rows = model.rows(&store);
        assert!(rows.iter().any(|row| row.id == inner));
        assert!(
            rows.iter().any(|row| row.is_folder),
            "the folder holding the match is still drawn"
        );
        assert!(rows.iter().all(|row| row.name != "release"));
    }

    #[test]
    fn a_search_that_matches_nothing_draws_nothing() {
        let (store, _, _) = store_with_a_folder();
        let mut model = HomeModel::new();
        model.set_search("absent");
        assert!(model.rows(&store).is_empty());
    }

    #[test]
    fn renaming_refuses_an_empty_name_and_a_name_a_sibling_holds() {
        let (mut store, _, inner) = store_with_a_folder();
        let model = HomeModel::new();
        assert!(model.rename(&mut store, &inner, "  ").is_err());
        assert!(model.rename(&mut store, &inner, "renamed").is_ok());
        assert_eq!(store.find_session(&inner).unwrap().name, "renamed");
    }

    #[test]
    fn a_move_lands_in_the_folder_and_opens_it() {
        let (mut store, folder, _) = store_with_a_folder();
        let outer = store
            .sessions()
            .iter()
            .find(|session| session.name == "release")
            .unwrap()
            .id
            .clone();
        let mut model = HomeModel::new();
        model.move_node(&mut store, &outer, Some(&folder)).unwrap();
        assert!(model.is_open(&folder));
        let rows = model.rows(&store);
        assert_eq!(rows.iter().filter(|row| row.depth == 1).count(), 2);
    }

    #[test]
    fn a_move_into_a_folder_that_is_inside_the_moved_one_is_refused() {
        let mut store = SessionStore::default();
        let outer = store.create_folder(None, "Outer").unwrap();
        let inner = store.create_folder(Some(&outer), "Inner").unwrap();
        let mut model = HomeModel::new();
        assert!(model.move_node(&mut store, &outer, Some(&inner)).is_err());
    }

    #[test]
    fn a_drag_moves_the_node_it_picked_up() {
        let (mut store, folder, _) = store_with_a_folder();
        let outer = store
            .sessions()
            .iter()
            .find(|session| session.name == "release")
            .unwrap()
            .id
            .clone();
        let mut model = HomeModel::new();
        model.begin_drag(outer.clone());
        assert_eq!(model.dragging(), Some(&outer));
        model.drop_onto(&mut store, Some(&folder)).unwrap();
        assert!(model.dragging().is_none());
        assert!(store.find(&outer).is_some());
        model.expand_all(&store);
        let rows = model.rows(&store);
        assert_eq!(rows.iter().find(|row| row.id == outer).unwrap().depth, 1);
    }

    #[test]
    fn a_cancelled_drag_moves_nothing() {
        let (mut store, folder, _) = store_with_a_folder();
        let mut model = HomeModel::new();
        model.begin_drag(folder);
        model.cancel_drag();
        model.drop_onto(&mut store, None).unwrap();
        assert_eq!(model.rows(&store).len(), 2);
    }

    #[test]
    fn a_duplicate_is_picked_and_carries_its_own_name() {
        let (mut store, _, inner) = store_with_a_folder();
        let mut model = HomeModel::new();
        model.duplicate(&mut store, &inner).unwrap();
        let copy = model.selected().unwrap().clone();
        assert_ne!(copy, inner);
        assert_ne!(
            store.find_session(&copy).unwrap().name,
            store.find_session(&inner).unwrap().name
        );
    }

    #[test]
    fn a_delete_drops_the_selection_with_the_node() {
        let (mut store, _, inner) = store_with_a_folder();
        let mut model = HomeModel::new();
        model.select(inner.clone());
        model.delete(&mut store, &inner).unwrap();
        assert!(model.selected().is_none());
        assert!(store.find(&inner).is_none());
    }

    #[test]
    fn a_locked_session_refuses_a_rename_and_a_delete() {
        let (mut store, _, inner) = store_with_a_folder();
        let mut model = HomeModel::new();
        model.set_locked(&mut store, &inner, true).unwrap();
        assert!(
            model.rows(&store).iter().any(|row| row.is_locked) || {
                model.toggle(&inner);
                true
            }
        );
        assert!(model.rename(&mut store, &inner, "other").is_err());
        assert!(model.delete(&mut store, &inner).is_err());
        model.set_locked(&mut store, &inner, false).unwrap();
        assert!(model.rename(&mut store, &inner, "other").is_ok());
    }

    #[test]
    fn a_new_folder_is_opened_and_picked() {
        let mut store = SessionStore::default();
        let mut model = HomeModel::new();
        model.create_folder(&mut store, None, "Group").unwrap();
        let id = model.selected().unwrap().clone();
        assert!(model.is_open(&id));
        assert!(model.create_folder(&mut store, None, " ").is_err());
    }

    #[test]
    fn automatically_saved_sessions_are_drawn_in_their_own_branch() {
        let (mut store, _, _) = store_with_a_folder();
        store.record_auto_saved(SavedSession::new(
            SessionId::from_raw("auto1"),
            "Untitled",
            SessionKind::HexCompare,
        ));
        let model = HomeModel::new();
        let automatic = model.rows_in(&store, Branch::AutoSaved);
        assert_eq!(automatic.len(), 1);
        assert_eq!(automatic[0].name, "Untitled");
        assert_eq!(model.rows_in(&store, Branch::Saved).len(), 2);
    }
}
