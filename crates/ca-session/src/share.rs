//! Export and import of settings and sessions as a single file.
//!
//! One package carries whichever categories the user selected. The same file
//! moves settings to another machine and distributes a common set of sessions
//! to a team: a package placed in a shared folder and named as the shared
//! sessions file appears as an extra, read-only branch of the sessions tree.

use crate::error::{Error, Result};
use crate::layer::SettingsLayers;
use crate::store::{
    atomic_write, free_name, restore_preserved_fields, PreservedStoredField, SavedSession,
    SessionId, SessionStore, StoredPathSegment, TreeNode, Workspace, WorkspaceTab,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Highest package schema version this build writes and reads.
pub const PACKAGE_SCHEMA_VERSION: u32 = 1;

/// Most bytes a package may hold. A package can come from another machine or a
/// shared folder, and it is held whole while it is parsed.
pub const MAX_PACKAGE_BYTES: u64 = 64 * 1024 * 1024;

/// Which categories a package carries.
/// A selection is a list of check boxes, so the count of flags is the shape of
/// the dialog rather than a sign of a type doing several jobs.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ExportSelection {
    /// Include the per-kind defaults for new sessions.
    pub session_defaults: bool,
    /// Include named sessions. An empty list means every named session.
    pub sessions: bool,
    /// Identifiers of the sessions to include, empty for all of them.
    pub session_ids: Vec<SessionId>,
    /// Include named workspaces.
    pub workspaces: bool,
    /// Include the application options.
    pub program_options: bool,
}

impl ExportSelection {
    /// Selects every category and every session.
    #[must_use]
    pub fn everything() -> Self {
        Self {
            session_defaults: true,
            sessions: true,
            session_ids: Vec::new(),
            workspaces: true,
            program_options: true,
        }
    }

    /// Selects only the named sessions.
    #[must_use]
    pub fn only_sessions(ids: Vec<SessionId>) -> Self {
        Self {
            session_defaults: false,
            sessions: true,
            session_ids: ids,
            workspaces: false,
            program_options: false,
        }
    }
}

/// How an import treats what is already stored.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportOptions {
    /// Remove every stored session before importing.
    pub delete_existing_sessions: bool,
    /// Remove every stored workspace before importing.
    pub delete_existing_workspaces: bool,
    /// Replace a stored session whose name collides, instead of importing the
    /// incoming one under a free name. A locked session is never replaced.
    pub overwrite_on_name_collision: bool,
}

/// One packaged item the import could not place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportFailure {
    /// Name of the session or workspace that was left out.
    pub item: String,
    /// Why it was left out.
    pub reason: String,
}

/// What an import changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    /// Sessions added to the tree.
    pub sessions_added: usize,
    /// Stored sessions replaced by an incoming one of the same name.
    pub sessions_replaced: usize,
    /// Incoming sessions left out because the stored session of that name is
    /// locked.
    pub sessions_skipped_locked: usize,
    /// Workspaces added or replaced.
    pub workspaces_imported: usize,
    /// Workspace tabs dropped because they named a session the package did not
    /// carry.
    pub tabs_dropped: usize,
    /// Kinds whose defaults the package supplied.
    pub session_defaults_imported: usize,
    /// Items the import could not place. The rest of the package was still
    /// imported.
    pub failures: Vec<ImportFailure>,
    /// Set when the package carried a schema version higher than this build
    /// writes. Everything in it is preserved, but this build cannot know what
    /// the newer version means, so a caller that wants to be safe warns before
    /// it commits the import.
    pub newer_schema: Option<u32>,
    /// Packaged fields that have no place in the sessions tree and are
    /// therefore not stored by this import. The package file still holds them.
    /// A field a newer build added inside a session travels with the session
    /// and is not counted here.
    pub unplaced_fields: usize,
    /// Unknown session-tree nodes kept in the package but not imported into
    /// this build's session tree.
    pub unknown_tree_nodes_not_imported: usize,
}

/// A package of exported settings and sessions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsPackage {
    /// Schema version of this package.
    pub schema_version: u32,
    /// Exported sessions, flattened out of the tree with their folder path.
    #[serde(default)]
    pub sessions: Vec<PackagedSession>,
    /// Session-tree nodes this build cannot interpret, preserved as raw JSON
    /// with the folder path they occupied.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unknown_tree_nodes: Vec<PackagedUnknownTreeNode>,
    /// Exported per-kind defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_defaults: Option<SettingsLayers>,
    /// Exported workspaces.
    #[serde(default)]
    pub workspaces: Vec<Workspace>,
    /// The exported application options, when the export carried them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program_options: Option<Box<crate::options::ProgramOptions>>,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown: BTreeMap<String, Value>,
    /// Original values for known fields written using a newer type.
    #[serde(skip)]
    preserved_invalid_fields: Vec<PreservedStoredField>,
}

/// One exported session together with the folder path it sat under.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackagedSession {
    /// Folder names from the top level down to the session's own folder.
    #[serde(default)]
    pub folder_path: Vec<String>,
    /// The session itself.
    pub session: SavedSession,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown: BTreeMap<String, Value>,
}

/// One session-tree node unknown to this build, retained in an export.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackagedUnknownTreeNode {
    /// Folder names from the top level down to the node's parent.
    #[serde(default)]
    pub folder_path: Vec<String>,
    /// The original tree node, unchanged.
    pub node: Value,
}

impl Default for SettingsPackage {
    fn default() -> Self {
        Self {
            schema_version: PACKAGE_SCHEMA_VERSION,
            sessions: Vec::new(),
            unknown_tree_nodes: Vec::new(),
            session_defaults: None,
            workspaces: Vec::new(),
            program_options: None,
            unknown: BTreeMap::new(),
            preserved_invalid_fields: Vec::new(),
        }
    }
}

/// Collects sessions and the folder path each one sits under.
fn collect(
    nodes: &[TreeNode],
    path: &mut Vec<String>,
    sessions: &mut Vec<PackagedSession>,
    unknown_nodes: &mut Vec<PackagedUnknownTreeNode>,
) {
    for node in nodes {
        match node {
            TreeNode::Session(session) => sessions.push(PackagedSession {
                folder_path: path.clone(),
                session: session.clone(),
                unknown: BTreeMap::new(),
            }),
            TreeNode::Folder { name, children, .. } => {
                path.push(name.clone());
                collect(children, path, sessions, unknown_nodes);
                path.pop();
            }
            TreeNode::Unknown(unknown) => unknown_nodes.push(PackagedUnknownTreeNode {
                folder_path: path.clone(),
                node: unknown.raw().clone(),
            }),
        }
    }
}

impl SettingsPackage {
    /// Builds a package from a store.
    ///
    /// A license or credential is never part of a package: only the categories
    /// named by `selection` are copied.
    #[must_use]
    pub fn export(store: &SessionStore, selection: &ExportSelection) -> Self {
        let mut package = SettingsPackage::default();

        if selection.sessions {
            let mut all = Vec::new();
            let mut unknown_nodes = Vec::new();
            collect(&store.root, &mut Vec::new(), &mut all, &mut unknown_nodes);
            package.sessions = if selection.session_ids.is_empty() {
                all
            } else {
                all.into_iter()
                    .filter(|packaged| selection.session_ids.contains(&packaged.session.id))
                    .collect()
            };
            package.unknown_tree_nodes = if selection.session_ids.is_empty() {
                unknown_nodes
            } else {
                unknown_nodes
                    .into_iter()
                    .filter(|packaged| {
                        packaged
                            .node
                            .get("id")
                            .and_then(Value::as_str)
                            .is_some_and(|id| {
                                selection.session_ids.contains(&SessionId::from_raw(id))
                            })
                    })
                    .collect()
            };
        }
        if selection.session_defaults {
            package.session_defaults = Some(store.layers.clone());
        }
        if selection.workspaces {
            package.workspaces.clone_from(&store.workspaces);
        }
        package
    }

    /// The same package carrying the application options.
    ///
    /// The options live in a document of their own rather than in the store, so
    /// the caller supplies them instead of the export reading them.
    #[must_use]
    pub fn with_program_options(mut self, options: crate::options::ProgramOptions) -> Self {
        self.program_options = Some(Box::new(options));
        self
    }

    /// The application options the package carries, where it carries any.
    #[must_use]
    pub fn program_options(&self) -> Option<&crate::options::ProgramOptions> {
        self.program_options.as_deref()
    }

    /// Merges a package into a store.
    ///
    /// The merge runs against a copy and is committed only once every item has
    /// been placed, so a package that cannot be imported whole leaves the store
    /// exactly as it was rather than half merged. An item that cannot be placed
    /// is counted in [`ImportReport::failures`] and the rest still arrives.
    ///
    /// Imported sessions are given fresh identifiers, and a workspace tab
    /// naming one of them is rewritten to the new identifier. A tab naming a
    /// session the package did not carry would otherwise bind to whichever
    /// stored session happened to hold that identifier, so it is dropped and
    /// counted.
    ///
    /// A package from a newer build imports. [`ImportReport::newer_schema`]
    /// carries the version it was written against and
    /// [`ImportReport::unplaced_fields`] counts the packaged fields that have
    /// no place in the sessions tree, so no part of the package is dropped
    /// without a count.
    ///
    /// # Errors
    /// Returns [`Error::KindMismatch`] when packaged defaults name a kind they
    /// do not belong to.
    pub fn import(
        &self,
        store: &mut SessionStore,
        options: &ImportOptions,
    ) -> Result<ImportReport> {
        let mut report = ImportReport {
            newer_schema: self.is_newer_schema().then_some(self.schema_version),
            unplaced_fields: self.unknown.len()
                + self
                    .sessions
                    .iter()
                    .map(|packaged| packaged.unknown.len())
                    .sum::<usize>(),
            unknown_tree_nodes_not_imported: self.unknown_tree_nodes.len(),
            ..ImportReport::default()
        };
        let mut staged = store.clone();

        if options.delete_existing_sessions {
            staged.root.clear();
        }
        if options.delete_existing_workspaces {
            staged.workspaces.clear();
        }

        let mut remapped: BTreeMap<SessionId, SessionId> = BTreeMap::new();

        for packaged in &self.sessions {
            match import_one(&mut staged, packaged, options, &mut report) {
                Ok(Some((old, new))) => {
                    remapped.insert(old, new);
                }
                Ok(None) => {}
                Err(err) => report.failures.push(ImportFailure {
                    item: packaged.session.name.clone(),
                    reason: err.to_string(),
                }),
            }
        }

        if let Some(defaults) = &self.session_defaults {
            for kind in defaults.edited_kinds() {
                if let Some(over) = defaults.session_defaults(kind) {
                    staged.layers.set_session_defaults(kind, over.clone())?;
                    report.session_defaults_imported += 1;
                }
            }
        }

        for workspace in &self.workspaces {
            let mut workspace = workspace.clone();
            for window in &mut workspace.windows {
                let before = window.tabs.len();
                window.tabs.retain_mut(|tab| match tab {
                    WorkspaceTab::Saved { session, .. } => match remapped.get(session) {
                        Some(new) => {
                            *session = new.clone();
                            true
                        }
                        None => false,
                    },
                    _ => true,
                });
                report.tabs_dropped += before - window.tabs.len();
                window.active_tab = window.active_tab.min(window.tabs.len().saturating_sub(1));
            }
            staged.save_workspace(workspace);
            report.workspaces_imported += 1;
        }

        *store = staged;
        Ok(report)
    }

    /// Writes the package, replacing any previous file.
    ///
    /// # Errors
    /// Returns [`Error::Io`] or [`Error::Parse`].
    pub fn write(&self, path: &Path) -> Result<()> {
        atomic_write(path, |buffer| {
            let document = if self.preserved_invalid_fields.is_empty() {
                None
            } else {
                let mut document = serde_json::to_value(self).map_err(|source| Error::Parse {
                    path: path.to_path_buf(),
                    source,
                })?;
                restore_preserved_fields(&mut document, &self.preserved_invalid_fields);
                Some(document)
            };
            if let Some(document) = document {
                serde_json::to_writer_pretty(buffer, &document)
            } else {
                serde_json::to_writer_pretty(buffer, self)
            }
            .map_err(|source| Error::Parse {
                path: path.to_path_buf(),
                source,
            })
        })
    }

    /// True when this package was written against a schema version higher than
    /// this build writes.
    #[must_use]
    pub fn is_newer_schema(&self) -> bool {
        self.schema_version > PACKAGE_SCHEMA_VERSION
    }

    /// Reads a package.
    ///
    /// A package written by a newer build is read, not refused: unknown fields
    /// at every level are held as raw JSON, [`Self::is_newer_schema`] reports
    /// the higher version, and a write re-emits that version rather than
    /// lowering it.
    ///
    /// # Errors
    /// Returns [`Error::Io`] or [`Error::Parse`].
    pub fn read(path: &Path) -> Result<Self> {
        let text = read_package_text(path).map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let mut document = serde_json::from_str::<Value>(&text).map_err(|source| Error::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        let mut preserved_invalid_fields = Vec::new();
        sanitize_package_fields(&mut document, &mut preserved_invalid_fields);
        let package_text = if preserved_invalid_fields.is_empty() {
            text
        } else {
            serde_json::to_string(&document).map_err(|source| Error::Parse {
                path: path.to_path_buf(),
                source,
            })?
        };
        let mut package =
            serde_json::from_str::<SettingsPackage>(&package_text).map_err(|source| {
                Error::Parse {
                    path: path.to_path_buf(),
                    source,
                }
            })?;
        package.preserved_invalid_fields = preserved_invalid_fields;
        Ok(package)
    }
}

fn sanitize_package_fields(document: &mut Value, preserved: &mut Vec<PreservedStoredField>) {
    if let Some(sessions) = document.get_mut("sessions").and_then(Value::as_array_mut) {
        for (index, item) in sessions.iter_mut().enumerate() {
            let Some(session) = item.get_mut("session").and_then(Value::as_object_mut) else {
                continue;
            };
            if session
                .get("lastUsedEpochSeconds")
                .is_some_and(|value| value.as_i64().is_none())
            {
                if let Some(original) = session.remove("lastUsedEpochSeconds") {
                    preserved.push((
                        vec![
                            StoredPathSegment::Key("sessions".to_owned()),
                            StoredPathSegment::Index(index),
                            StoredPathSegment::Key("session".to_owned()),
                            StoredPathSegment::Key("lastUsedEpochSeconds".to_owned()),
                        ],
                        original,
                    ));
                }
            }
        }
    }
    if let Some(workspaces) = document.get_mut("workspaces").and_then(Value::as_array_mut) {
        for (workspace_index, workspace) in workspaces.iter_mut().enumerate() {
            let Some(windows) = workspace.get_mut("windows").and_then(Value::as_array_mut) else {
                continue;
            };
            for (window_index, window) in windows.iter_mut().enumerate() {
                let Some(object) = window.as_object_mut() else {
                    continue;
                };
                if object.get("activeTab").is_some_and(|value| {
                    value
                        .as_u64()
                        .and_then(|raw| usize::try_from(raw).ok())
                        .is_none()
                }) {
                    if let Some(original) = object.remove("activeTab") {
                        preserved.push((
                            vec![
                                StoredPathSegment::Key("workspaces".to_owned()),
                                StoredPathSegment::Index(workspace_index),
                                StoredPathSegment::Key("windows".to_owned()),
                                StoredPathSegment::Index(window_index),
                                StoredPathSegment::Key("activeTab".to_owned()),
                            ],
                            original,
                        ));
                    }
                }
            }
        }
    }
}

/// Nodes directly under `parent`, or the top level when it is `None`.
fn children_of<'a>(store: &'a SessionStore, parent: Option<&SessionId>) -> Result<&'a [TreeNode]> {
    match parent {
        None => Ok(&store.root),
        Some(parent) => store
            .find(parent)
            .map(TreeNode::children)
            .ok_or_else(|| Error::NoSuchNode(parent.to_string())),
    }
}

/// Names already used directly under `parent`.
fn taken_names(store: &SessionStore, parent: Option<&SessionId>) -> Result<Vec<String>> {
    Ok(children_of(store, parent)?
        .iter()
        .map(|node| node.name().to_owned())
        .collect())
}

/// The node directly under `parent` carrying `name`, ignoring capitalization.
fn sibling_named(
    store: &SessionStore,
    parent: Option<&SessionId>,
    name: &str,
) -> Result<Option<SessionId>> {
    Ok(children_of(store, parent)?
        .iter()
        .find(|node| node.name().to_lowercase() == name.to_lowercase())
        .map(|node| node.id().clone()))
}

/// Places one packaged session, reporting the identifier it was given so a
/// workspace tab naming the old one can be rewritten.
fn import_one(
    store: &mut SessionStore,
    packaged: &PackagedSession,
    options: &ImportOptions,
    report: &mut ImportReport,
) -> Result<Option<(SessionId, SessionId)>> {
    let mut parent = None;
    for folder in &packaged.folder_path {
        parent = Some(existing_or_new_folder(store, parent.as_ref(), folder)?);
    }

    let mut session = packaged.session.clone();
    let old_id = session.id.clone();
    session.id = store.next_id();

    let collision = sibling_named(store, parent.as_ref(), &session.name)?;
    match collision {
        Some(existing) if options.overwrite_on_name_collision => {
            if store.is_locked(&existing) {
                report.sessions_skipped_locked += 1;
                return Ok(None);
            }
            store.delete(&existing)?;
            let new_id = store.add_session(parent.as_ref(), session)?;
            report.sessions_replaced += 1;
            Ok(Some((old_id, new_id)))
        }
        Some(_) => {
            let taken = taken_names(store, parent.as_ref())?;
            session.name = free_name(&session.name, &taken);
            let new_id = store.add_session(parent.as_ref(), session)?;
            report.sessions_added += 1;
            Ok(Some((old_id, new_id)))
        }
        None => {
            let new_id = store.add_session(parent.as_ref(), session)?;
            report.sessions_added += 1;
            Ok(Some((old_id, new_id)))
        }
    }
}

/// Finds a folder by name under `parent`, creating it when absent.
///
/// A session already carrying the name is not a folder and cannot hold
/// children, so the folder takes the next free name instead of colliding.
fn existing_or_new_folder(
    store: &mut SessionStore,
    parent: Option<&SessionId>,
    name: &str,
) -> Result<SessionId> {
    let existing = children_of(store, parent)?
        .iter()
        .find(|node| node.name().to_lowercase() == name.to_lowercase() && node.is_folder())
        .map(|node| node.id().clone());
    if let Some(id) = existing {
        return Ok(id);
    }
    let taken = taken_names(store, parent)?;
    store.create_folder(parent, free_name(name, &taken))
}

/// A package other users publish, surfaced as a read-only branch of the
/// sessions tree.
///
/// Nothing here writes: changing a shared session means republishing the
/// package it came from.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SharedSessions {
    path: Option<PathBuf>,
    sessions: Vec<PackagedSession>,
}

impl SharedSessions {
    /// Loads the shared package named by the setting, if one is named.
    ///
    /// # Errors
    /// Returns whatever [`SettingsPackage::read`] returns.
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let Some(path) = path else {
            return Ok(Self::default());
        };
        let package = SettingsPackage::read(path)?;
        Ok(Self {
            path: Some(path.to_path_buf()),
            sessions: package.sessions,
        })
    }

    /// The file the shared sessions were read from.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The shared sessions, which cannot be edited in place.
    #[must_use]
    pub fn sessions(&self) -> &[PackagedSession] {
        &self.sessions
    }

    /// True when no shared package is configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
}

/// Reads a package as text, refusing one past [`MAX_PACKAGE_BYTES`] before
/// it is held.
fn read_package_text(path: &Path) -> std::io::Result<String> {
    use std::io::Read;

    let too_large = || {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("the package is larger than {MAX_PACKAGE_BYTES} bytes"),
        )
    };
    let file = fs::File::open(path)?;
    if file.metadata()?.len() > MAX_PACKAGE_BYTES {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    file.take(MAX_PACKAGE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_PACKAGE_BYTES {
        return Err(too_large());
    }
    String::from_utf8(bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
mod tests {
    use super::*;

    #[test]
    fn a_package_past_the_size_ceiling_is_refused_before_it_is_read() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("large.json");
        let file = fs::File::create(&path).unwrap();
        file.set_len(MAX_PACKAGE_BYTES + 1).unwrap();
        drop(file);
        let Err(error) = SettingsPackage::read(&path) else {
            panic!("a package past the ceiling was read");
        };
        match error {
            Error::Io { source, .. } => {
                assert_eq!(source.kind(), std::io::ErrorKind::InvalidData);
                assert!(source.to_string().contains("larger than"), "{source}");
            }
            other => panic!("unexpected error: {other}"),
        }
    }
    use crate::kind::SessionKind;
    use crate::settings::SessionSettings;
    use tempfile::TempDir;

    fn populated_store() -> SessionStore {
        let mut store = SessionStore::default();
        let folder = store.create_folder(None, "Team").unwrap();
        let id = store.next_id();
        store
            .add_session(
                Some(&folder),
                SavedSession::new(id, "Nightly", SessionKind::FolderCompare),
            )
            .unwrap();
        let id = store.next_id();
        store
            .add_session(
                None,
                SavedSession::new(id, "Release", SessionKind::TextCompare),
            )
            .unwrap();
        store
            .layers
            .update_session_defaults_from(
                &SessionKind::HexCompare,
                &SessionSettings::defaults_for(&SessionKind::HexCompare),
            )
            .unwrap();
        store.save_workspace(Workspace {
            name: "Daily".to_owned(),
            ..Workspace::default()
        });
        store
    }

    #[test]
    fn a_full_export_carries_every_selected_category() {
        let store = populated_store();
        let package = SettingsPackage::export(&store, &ExportSelection::everything());
        assert_eq!(package.sessions.len(), 2);
        assert_eq!(package.workspaces.len(), 1);
        assert!(package.session_defaults.is_some());
        assert_eq!(package.sessions[0].folder_path, vec!["Team".to_owned()]);
    }

    #[test]
    fn a_selective_export_carries_only_the_named_sessions() {
        let store = populated_store();
        let wanted = store.sessions()[1].id.clone();
        let package = SettingsPackage::export(
            &store,
            &ExportSelection::only_sessions(vec![wanted.clone()]),
        );
        assert_eq!(package.sessions.len(), 1);
        assert_eq!(package.sessions[0].session.id, wanted);
        assert!(package.session_defaults.is_none());
    }

    #[test]
    fn importing_into_an_empty_store_rebuilds_the_tree() {
        let package = SettingsPackage::export(&populated_store(), &ExportSelection::everything());
        let mut target = SessionStore::default();
        let report = package
            .import(&mut target, &ImportOptions::default())
            .unwrap();

        assert_eq!(report.sessions_added, 2);
        assert_eq!(report.workspaces_imported, 1);
        assert_eq!(report.session_defaults_imported, 1);
        assert_eq!(target.sessions().len(), 2);
        let team = target
            .root
            .iter()
            .find(|node| node.name() == "Team")
            .unwrap();
        assert_eq!(team.children().len(), 1);
    }

    #[test]
    fn a_name_collision_is_kept_by_default_and_replaced_on_request() {
        let package = SettingsPackage::export(&populated_store(), &ExportSelection::everything());

        let mut target = populated_store();
        package
            .import(&mut target, &ImportOptions::default())
            .unwrap();
        assert_eq!(target.sessions().len(), 4);

        let mut target = populated_store();
        let report = package
            .import(
                &mut target,
                &ImportOptions {
                    overwrite_on_name_collision: true,
                    ..ImportOptions::default()
                },
            )
            .unwrap();
        assert_eq!(report.sessions_replaced, 2);
        assert_eq!(target.sessions().len(), 2);
    }

    #[test]
    fn imported_workspace_tabs_follow_the_sessions_they_name() {
        let mut source = populated_store();
        let release = source.sessions()[1].id.clone();
        source.save_workspace(Workspace {
            name: "Bound".to_owned(),
            windows: vec![crate::store::WorkspaceWindow {
                tabs: vec![WorkspaceTab::home(), WorkspaceTab::saved(release.clone())],
                active_tab: 1,
                ..crate::store::WorkspaceWindow::default()
            }],
            ..Workspace::default()
        });
        let package = SettingsPackage::export(&source, &ExportSelection::everything());

        // A target already holding a session under the identifier the package
        // names is what makes a stale reference bind to the wrong session.
        let mut target = SessionStore::default();
        let mut decoy = target.next_id();
        while decoy != release {
            decoy = target.next_id();
        }
        target
            .add_session(
                None,
                SavedSession::new(decoy.clone(), "Unrelated", SessionKind::HexCompare),
            )
            .unwrap();

        let report = package
            .import(&mut target, &ImportOptions::default())
            .unwrap();
        assert!(report.failures.is_empty());

        let bound = target.workspace("Bound").unwrap();
        let WorkspaceTab::Saved { session, .. } = &bound.windows[0].tabs[1] else {
            panic!("the tab should still name a saved session");
        };
        assert_ne!(
            session, &decoy,
            "the tab does not bind to the unrelated stored session"
        );
        assert_eq!(target.find_session(session).unwrap().name, "Release");
    }

    #[test]
    fn a_tab_naming_a_session_outside_the_package_is_dropped_and_counted() {
        let mut source = populated_store();
        source.save_workspace(Workspace {
            name: "Dangling".to_owned(),
            windows: vec![crate::store::WorkspaceWindow {
                tabs: vec![
                    WorkspaceTab::home(),
                    WorkspaceTab::saved(SessionId::from_raw("n999")),
                ],
                active_tab: 1,
                ..crate::store::WorkspaceWindow::default()
            }],
            ..Workspace::default()
        });
        let package = SettingsPackage::export(
            &source,
            &ExportSelection {
                sessions: false,
                ..ExportSelection::everything()
            },
        );

        let mut target = SessionStore::default();
        let report = package
            .import(&mut target, &ImportOptions::default())
            .unwrap();
        assert_eq!(report.tabs_dropped, 1);
        let workspace = target.workspace("Dangling").unwrap();
        assert_eq!(workspace.windows[0].tabs, vec![WorkspaceTab::home()]);
        assert_eq!(workspace.windows[0].active_tab, 0);
    }

    #[test]
    fn a_locked_session_is_never_replaced_by_an_import() {
        let package = SettingsPackage::export(&populated_store(), &ExportSelection::everything());
        let mut target = populated_store();
        let release = target
            .sessions()
            .iter()
            .find(|session| session.name == "Release")
            .map(|session| session.id.clone())
            .unwrap();
        target.set_locked(&release, true).unwrap();

        let report = package
            .import(
                &mut target,
                &ImportOptions {
                    overwrite_on_name_collision: true,
                    ..ImportOptions::default()
                },
            )
            .unwrap();

        assert_eq!(report.sessions_skipped_locked, 1);
        assert_eq!(report.sessions_replaced, 1, "the unlocked one is replaced");
        let kept = target.find_session(&release).unwrap();
        assert!(kept.locked, "the lock is still in force after the import");
    }

    #[test]
    fn a_collision_takes_the_next_free_name() {
        let package = SettingsPackage::export(&populated_store(), &ExportSelection::everything());
        let mut target = populated_store();
        package
            .import(&mut target, &ImportOptions::default())
            .unwrap();
        package
            .import(&mut target, &ImportOptions::default())
            .unwrap();

        let names: Vec<String> = target
            .sessions()
            .iter()
            .map(|session| session.name.clone())
            .collect();
        assert!(names.contains(&"Release (2)".to_owned()), "{names:?}");
        assert!(names.contains(&"Release (3)".to_owned()), "{names:?}");
    }

    #[test]
    fn a_destination_folder_name_held_by_a_session_does_not_stop_the_import() {
        let package = SettingsPackage::export(&populated_store(), &ExportSelection::everything());
        let mut target = SessionStore::default();
        let blocker = target.next_id();
        target
            .add_session(
                None,
                SavedSession::new(blocker, "Team", SessionKind::HexCompare),
            )
            .unwrap();

        let report = package
            .import(&mut target, &ImportOptions::default())
            .unwrap();
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert_eq!(report.sessions_added, 2);
        let folder = target.root.iter().find(|node| node.is_folder()).unwrap();
        assert_eq!(
            folder.name(),
            "Team (2)",
            "the folder took a free name beside the session"
        );
        assert_eq!(folder.children().len(), 1);
    }

    #[test]
    fn a_root_level_collision_is_seen_without_a_sentinel_parent() {
        let package = SettingsPackage::export(
            &populated_store(),
            &ExportSelection {
                session_defaults: false,
                workspaces: false,
                ..ExportSelection::everything()
            },
        );
        let mut target = SessionStore::default();
        let id = target.next_id();
        target
            .add_session(
                None,
                SavedSession::new(id, "Release", SessionKind::HexCompare),
            )
            .unwrap();

        package
            .import(&mut target, &ImportOptions::default())
            .unwrap();
        let names: Vec<String> = target
            .root
            .iter()
            .map(|node| node.name().to_owned())
            .collect();
        assert!(names.contains(&"Release (2)".to_owned()), "{names:?}");
    }

    #[test]
    fn deleting_existing_sessions_first_leaves_only_the_package() {
        let package = SettingsPackage::export(&populated_store(), &ExportSelection::everything());
        let mut target = populated_store();
        package
            .import(
                &mut target,
                &ImportOptions {
                    delete_existing_sessions: true,
                    ..ImportOptions::default()
                },
            )
            .unwrap();
        assert_eq!(target.sessions().len(), 2);
    }

    #[test]
    fn a_package_round_trips_through_a_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("settings.package.json");
        let package = SettingsPackage::export(&populated_store(), &ExportSelection::everything());
        package.write(&path).unwrap();
        assert_eq!(SettingsPackage::read(&path).unwrap(), package);
    }

    #[test]
    fn unknown_package_fields_survive_a_read_and_write() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("settings.package.json");
        fs::write(
            &path,
            r#"{"schemaVersion":1,"sessions":[],"futureField":"kept"}"#,
        )
        .unwrap();
        let package = SettingsPackage::read(&path).unwrap();
        package.write(&path).unwrap();
        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["futureField"], Value::from("kept"));
    }

    #[test]
    fn a_newer_package_loads_and_is_flagged_by_the_import() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("settings.package.json");
        fs::write(&path, r#"{"schemaVersion":9999,"sessions":[]}"#).unwrap();

        let package = SettingsPackage::read(&path).unwrap();
        assert!(package.is_newer_schema());

        let mut target = SessionStore::default();
        let report = package
            .import(&mut target, &ImportOptions::default())
            .unwrap();
        assert_eq!(report.newer_schema, Some(9999));
    }

    #[test]
    fn a_newer_package_keeps_its_version_when_it_is_written_back() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("settings.package.json");
        fs::write(&path, r#"{"schemaVersion":9999,"sessions":[]}"#).unwrap();
        let package = SettingsPackage::read(&path).unwrap();
        package.write(&path).unwrap();
        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["schemaVersion"], Value::from(9999));
    }

    #[test]
    fn shared_sessions_are_read_only_and_optional() {
        assert!(SharedSessions::load(None).unwrap().is_empty());

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("shared.package.json");
        SettingsPackage::export(&populated_store(), &ExportSelection::everything())
            .write(&path)
            .unwrap();

        let shared = SharedSessions::load(Some(&path)).unwrap();
        assert_eq!(shared.sessions().len(), 2);
        assert_eq!(shared.path(), Some(path.as_path()));
    }
}
