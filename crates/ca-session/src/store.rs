//! Saved sessions, workspaces, and persistence to disk.
//!
//! The stored tree holds session folders and saved sessions, alongside a
//! bounded list of automatically saved recent sessions and the named
//! workspaces. Documents are versioned JSON written by filling a uniquely named
//! temporary file and renaming it into place, so an interrupted write leaves
//! the previous document intact.
//!
//! Loading is tolerant by design. Fields this build does not know are carried
//! through untouched, and every serialized enum has an unknown arm holding raw
//! JSON, so a node, a kind, a settings tag or a map key written by a newer
//! build survives a load and a save unchanged instead of failing the whole
//! document.

use crate::error::{Error, Result};
use crate::kind::SessionKind;
use crate::layer::{ResolvedSettings, SettingsLayers};
use crate::settings::SessionSettingsOverride;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Highest document schema version this build writes and reads.
pub const SCHEMA_VERSION: u32 = 1;

/// File holding the sessions tree, workspaces and per-kind defaults.
pub const SESSIONS_FILE: &str = "sessions.json";

/// File holding window placement and recently used lists. Its presence beside
/// the executable is what selects portable mode.
pub const PROGRAM_STATE_FILE: &str = "program-state.json";

/// File whose operating system lock claims the settings directory for one
/// running instance. The file itself stays on disk and holds no content.
pub const LOCK_FILE: &str = "settings.lock";

/// File describing whoever holds [`LOCK_FILE`], for the message a second
/// instance shows. It is a separate file because a locked file cannot be read
/// on every platform.
pub const LOCK_HOLDER_FILE: &str = "settings.holder";

/// A location in a stored JSON document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StoredPathSegment {
    Key(String),
    Index(usize),
}

/// A field this build could not interpret but must write back unchanged.
pub(crate) type PreservedStoredField = (Vec<StoredPathSegment>, Value);

/// Restore fields removed temporarily so a known struct can be loaded with its
/// built-in default for a value written using a newer type.
pub(crate) fn restore_preserved_fields(document: &mut Value, fields: &[PreservedStoredField]) {
    for (path, original) in fields {
        restore_at(document, path, original);
    }
}

fn restore_at(document: &mut Value, path: &[StoredPathSegment], original: &Value) {
    let Some((segment, rest)) = path.split_first() else {
        return;
    };
    if rest.is_empty() {
        match segment {
            StoredPathSegment::Key(key) => {
                if let Some(object) = document.as_object_mut() {
                    object.insert(key.clone(), original.clone());
                }
            }
            StoredPathSegment::Index(index) => {
                if let Some(slot) = document
                    .as_array_mut()
                    .and_then(|items| items.get_mut(*index))
                {
                    *slot = original.clone();
                }
            }
        }
        return;
    }
    let next = match segment {
        StoredPathSegment::Key(key) => document.get_mut(key),
        StoredPathSegment::Index(index) => document.get_mut(*index),
    };
    if let Some(next) = next {
        restore_at(next, rest, original);
    }
}

/// Greatest number of candidate names tried before a unique suffix is used
/// instead. A name generator that never gives up hangs the caller on a folder
/// holding pathological names.
const NAME_ATTEMPTS: u32 = 1_000;

/// Greatest number of quarantine names tried before the attempt is reported as
/// an I/O failure.
const QUARANTINE_ATTEMPTS: u32 = 1_000;

/// Identifier of a node in the sessions tree, unique within one store.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SessionId(String);

impl SessionId {
    /// Builds an identifier from stored text.
    pub fn from_raw(raw: impl Into<String>) -> Self {
        SessionId(raw.into())
    }

    /// The identifier as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The counter an identifier this build generated carries, if it carries
    /// one.
    fn counter(&self) -> Option<u64> {
        self.0.strip_prefix('n')?.parse().ok()
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Seconds since the Unix epoch, saturating at zero before it.
fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// Compares two sibling names the way the tree treats them.
///
/// Names differing only in capitalization are the same name on every platform,
/// so a tree built on one machine keeps its shape on another.
fn same_name(left: &str, right: &str) -> bool {
    left.to_lowercase() == right.to_lowercase()
}

/// A saved comparison: its kind, its name and the settings it overrides.
#[derive(Debug, Clone, PartialEq)]
pub struct SavedSession {
    /// Identifier, unique within the store.
    pub id: SessionId,
    /// Name shown in the tree.
    pub name: String,
    /// Kind of comparison. The settings carry the same kind as their tag; a
    /// document disagreeing with itself is repaired on load in favor of the
    /// settings, which hold the typed payload.
    pub kind: SessionKind,
    /// Settings this session overrides on top of its kind's defaults.
    pub settings: SessionSettingsOverride,
    /// A locked session ignores every change made while it is open, so its
    /// stored settings and sides are not overwritten by accident.
    pub locked: bool,
    /// Seconds since the epoch when the session was last opened.
    last_used_epoch_seconds: i64,
    /// Fields written by another build, preserved verbatim.
    pub unknown: BTreeMap<String, Value>,
    /// An unfamiliar timestamp travels with the session, independent of its
    /// position in recent history or the named tree.
    last_used_original: Option<Value>,
}

impl Serialize for SavedSession {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap as _;
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("id", &self.id)?;
        map.serialize_entry("name", &self.name)?;
        map.serialize_entry("kind", &self.kind)?;
        map.serialize_entry("settings", &self.settings)?;
        map.serialize_entry("locked", &self.locked)?;
        if let Some(original) = &self.last_used_original {
            map.serialize_entry("lastUsedEpochSeconds", original)?;
        } else {
            map.serialize_entry("lastUsedEpochSeconds", &self.last_used_epoch_seconds)?;
        }
        for (key, value) in &self.unknown {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for SavedSession {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Stored {
            id: SessionId,
            #[serde(default)]
            name: String,
            kind: SessionKind,
            settings: SessionSettingsOverride,
            #[serde(default)]
            locked: bool,
            #[serde(default = "zero", alias = "lastUsed")]
            last_used_epoch_seconds: Value,
            #[serde(flatten)]
            unknown: BTreeMap<String, Value>,
        }
        fn zero() -> Value {
            Value::from(0)
        }
        let stored = Stored::deserialize(deserializer)?;
        let seconds = stored.last_used_epoch_seconds.as_i64();
        Ok(Self {
            id: stored.id,
            name: stored.name,
            kind: stored.kind,
            settings: stored.settings,
            locked: stored.locked,
            last_used_epoch_seconds: seconds.unwrap_or_default(),
            last_used_original: seconds.is_none().then_some(stored.last_used_epoch_seconds),
            unknown: stored.unknown,
        })
    }
}

impl SavedSession {
    /// The usable timestamp, or zero when the stored value is unfamiliar.
    #[must_use]
    pub fn last_used_epoch_seconds(&self) -> i64 {
        self.last_used_epoch_seconds
    }

    /// An explicit timestamp edit replaces any unfamiliar stored value.
    pub fn set_last_used_epoch_seconds(&mut self, seconds: i64) {
        self.last_used_epoch_seconds = seconds;
        self.last_used_original = None;
    }

    /// A session of `kind` named `name`, overriding nothing.
    #[must_use]
    pub fn new(id: SessionId, name: impl Into<String>, kind: SessionKind) -> Self {
        let settings = SessionSettingsOverride::empty_for(&kind);
        Self {
            id,
            name: name.into(),
            kind,
            settings,
            locked: false,
            last_used_epoch_seconds: now_seconds(),
            last_used_original: None,
            unknown: BTreeMap::new(),
        }
    }

    /// Resolves this session's settings against the given layers.
    ///
    /// # Errors
    /// Returns [`Error::KindMismatch`] when the stored overrides do not match
    /// the session's kind.
    pub fn resolve(&self, layers: &SettingsLayers) -> Result<ResolvedSettings> {
        layers.resolve(&self.kind, &self.settings)
    }

    /// Makes the declared kind agree with the settings, reporting whether a
    /// correction was needed.
    fn repair_kind(&mut self) -> bool {
        let from_settings = self.settings.kind();
        if self.kind == from_settings {
            return false;
        }
        self.kind = from_settings;
        true
    }
}

/// A tree node written by a build this one does not understand.
///
/// The whole node is held as raw JSON and written back unchanged. Its
/// identifier and name are read out of that JSON so the node still occupies a
/// place in the tree, takes part in name collision checks, and cannot have its
/// identifier handed to a new node.
#[derive(Debug, Clone, PartialEq)]
pub struct UnknownNode {
    id: SessionId,
    name: String,
    raw: Value,
}

impl UnknownNode {
    /// The raw JSON, exactly as it was read.
    #[must_use]
    pub fn raw(&self) -> &Value {
        &self.raw
    }

    fn from_raw(raw: Value) -> Self {
        let id = SessionId::from_raw(
            raw.get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        );
        let name = raw
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        Self { id, name, raw }
    }

    fn set_id(&mut self, id: SessionId) {
        if let Some(object) = self.raw.as_object_mut() {
            object.insert("id".to_owned(), Value::from(id.as_str()));
        }
        self.id = id;
    }
}

impl Serialize for UnknownNode {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        self.raw.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for UnknownNode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        Ok(UnknownNode::from_raw(Value::deserialize(deserializer)?))
    }
}

/// A node of the sessions tree: either a folder or a saved session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "node",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
#[allow(clippy::large_enum_variant)]
#[non_exhaustive]
pub enum TreeNode {
    /// A folder grouping further nodes.
    Folder {
        /// Identifier, unique within the store.
        id: SessionId,
        /// Name shown in the tree.
        #[serde(default)]
        name: String,
        /// Contained nodes, in display order.
        #[serde(default)]
        children: Vec<TreeNode>,
        /// Fields written by another build, preserved verbatim.
        #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
        unknown: BTreeMap<String, Value>,
    },
    /// A saved session.
    Session(SavedSession),
    /// A node this build does not understand, carried through unchanged.
    #[serde(untagged)]
    Unknown(UnknownNode),
}

impl TreeNode {
    /// Identifier of this node. An unknown node carrying none reports the empty
    /// identifier.
    #[must_use]
    pub fn id(&self) -> &SessionId {
        match self {
            TreeNode::Folder { id, .. } => id,
            TreeNode::Session(session) => &session.id,
            TreeNode::Unknown(node) => &node.id,
        }
    }

    /// Name of this node.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            TreeNode::Folder { name, .. } => name,
            TreeNode::Session(session) => &session.name,
            TreeNode::Unknown(node) => &node.name,
        }
    }

    /// Renames this node. An unknown node cannot be renamed without rewriting
    /// JSON this build does not understand, so its name is left alone.
    pub fn set_name(&mut self, new_name: impl Into<String>) {
        match self {
            TreeNode::Folder { name, .. } => *name = new_name.into(),
            TreeNode::Session(session) => session.name = new_name.into(),
            TreeNode::Unknown(_) => {}
        }
    }

    /// True when this node can hold children.
    #[must_use]
    pub fn is_folder(&self) -> bool {
        matches!(self, TreeNode::Folder { .. })
    }

    /// Contained nodes, empty for anything that is not a folder.
    #[must_use]
    pub fn children(&self) -> &[TreeNode] {
        match self {
            TreeNode::Folder { children, .. } => children,
            TreeNode::Session(_) | TreeNode::Unknown(_) => &[],
        }
    }

    /// True when `id` names this node or anything beneath it.
    #[must_use]
    pub fn contains(&self, id: &SessionId) -> bool {
        self.id() == id || self.children().iter().any(|child| child.contains(id))
    }
}

/// What one tab of a saved workspace shows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "content",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
#[non_exhaustive]
pub enum WorkspaceTab {
    /// The launcher.
    ///
    /// A tagged variant with no data of its own still serializes as an object,
    /// so it carries the flattened `unknown` map: without one, a field a newer
    /// build writes beside the tag is dropped on load.
    Home {
        /// Fields written by another build, preserved verbatim.
        #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
        unknown: BTreeMap<String, Value>,
    },
    /// A session stored in the tree.
    Saved {
        /// Identifier of the saved session.
        session: SessionId,
        /// Fields written by another build, preserved verbatim.
        #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
        unknown: BTreeMap<String, Value>,
    },
    /// A session that was never named, carried inside the workspace.
    Unsaved {
        /// The session itself.
        session: Box<SavedSession>,
        /// Fields written by another build, preserved verbatim.
        #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
        unknown: BTreeMap<String, Value>,
    },
    /// A tab this build does not understand, carried through unchanged.
    #[serde(untagged)]
    Unknown(Value),
}

impl WorkspaceTab {
    /// A tab showing the launcher.
    #[must_use]
    pub fn home() -> Self {
        WorkspaceTab::Home {
            unknown: BTreeMap::new(),
        }
    }

    /// A tab showing a session stored in the tree.
    #[must_use]
    pub fn saved(session: SessionId) -> Self {
        WorkspaceTab::Saved {
            session,
            unknown: BTreeMap::new(),
        }
    }

    /// A tab holding a session that was never named.
    #[must_use]
    pub fn unsaved(session: SavedSession) -> Self {
        WorkspaceTab::Unsaved {
            session: Box::new(session),
            unknown: BTreeMap::new(),
        }
    }
}

/// Where one window of a workspace sits on screen.
///
/// Each edge is named, so a field added later cannot shift the meaning of the
/// ones already stored.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WindowBounds {
    /// Distance from the left edge of the desktop, in logical pixels.
    #[serde(default)]
    pub x: i32,
    /// Distance from the top edge of the desktop, in logical pixels.
    #[serde(default)]
    pub y: i32,
    /// Width in logical pixels.
    #[serde(default)]
    pub width: u32,
    /// Height in logical pixels.
    #[serde(default)]
    pub height: u32,
    /// Monitor the window was placed on, absent when it was not recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitor: Option<String>,
    /// Scale factor the logical pixels were measured at, absent when it was not
    /// recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f64>,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown: BTreeMap<String, Value>,
}

impl WindowBounds {
    /// Bounds at a position and size, on no recorded monitor.
    #[must_use]
    pub fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
            ..Self::default()
        }
    }
}

/// One window of a saved workspace.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WorkspaceWindow {
    /// Position and size, absent when the window was not placed.
    #[serde(default)]
    pub bounds: Option<WindowBounds>,
    /// Tabs in display order.
    #[serde(default)]
    pub tabs: Vec<WorkspaceTab>,
    /// Index of the tab that had focus.
    #[serde(default)]
    pub active_tab: usize,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown: BTreeMap<String, Value>,
}

/// A named set of open windows and tabs, restorable later.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Workspace {
    /// Name, unique within the store.
    #[serde(default)]
    pub name: String,
    /// Windows in the order they were opened.
    #[serde(default)]
    pub windows: Vec<WorkspaceWindow>,
    /// Key combination that loads the workspace, if one is bound.
    #[serde(default)]
    pub shortcut: Option<String>,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown: BTreeMap<String, Value>,
}

/// Size and modification time of the document a store was read from.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: Option<SystemTime>,
}

fn stamp_of(path: &Path) -> Result<Option<FileStamp>> {
    match fs::metadata(path) {
        Ok(data) => Ok(Some(FileStamp {
            len: data.len(),
            modified: data.modified().ok(),
        })),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(Error::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Records which document a store was read from, so a save can tell whether
/// anything replaced it since.
///
/// Two stores are equal when their contents are equal; where a store came from
/// takes no part in that, so every stamp compares equal to every other.
#[derive(Debug, Clone, Default)]
pub struct DocumentStamp(Option<FileStamp>);

impl PartialEq for DocumentStamp {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl DocumentStamp {
    /// The stamp of the document at `path`, absent when no file is there.
    ///
    /// # Errors
    /// Returns [`Error::Io`] when the path exists but cannot be examined.
    pub fn of(path: &Path) -> Result<Self> {
        Ok(Self(stamp_of(path)?))
    }

    /// True when both stamps name the same document.
    ///
    /// [`PartialEq`] answers a different question: it compares two stores by
    /// their contents and ignores where each came from. A save that has to tell
    /// whether something replaced the file asks this instead.
    #[must_use]
    pub fn is_same_document(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

/// Everything persisted about sessions, workspaces and per-kind defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStore {
    /// Schema version of the document this store was read from or is written
    /// as. A version higher than [`SCHEMA_VERSION`] is kept rather than lowered,
    /// so a save by this build never downgrades a document a newer build wrote.
    #[serde(default)]
    pub schema_version: u32,
    /// Counter backing identifier generation; never reused within a store.
    #[serde(default)]
    next_id: u64,
    /// Top level of the sessions tree.
    #[serde(default)]
    pub root: Vec<TreeNode>,
    /// Recent sessions saved without being named, newest first.
    #[serde(default)]
    pub auto_saved: Vec<SavedSession>,
    /// Cap on automatically saved sessions; zero disables automatic saving and
    /// discards the ones already held.
    #[serde(default = "default_max_auto_saved")]
    pub max_auto_saved: usize,
    /// Named workspaces.
    #[serde(default)]
    pub workspaces: Vec<Workspace>,
    /// Name of the workspace the program last had open.
    ///
    /// Left out of a document that names none, so a document written before
    /// workspaces were restored at start re-saves unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_workspace: Option<String>,
    /// Reopen [`SessionStore::last_workspace`] when the program starts.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub restore_last_workspace: bool,
    /// Per-kind defaults for new sessions.
    #[serde(default)]
    pub layers: SettingsLayers,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown: BTreeMap<String, Value>,
    /// The document this store was read from, if it was read from one.
    #[serde(skip)]
    stamp: DocumentStamp,
    /// Node identifiers the document held when it was read.
    ///
    /// A merge tells a node another writer added from a node this store
    /// deleted by whether the identifier was present at load.
    #[serde(skip)]
    baseline_nodes: BTreeSet<SessionId>,
    /// Workspace names the document held when it was read.
    #[serde(skip)]
    baseline_workspaces: BTreeSet<String>,
    /// Original values for known fields a newer build changed to another type.
    #[serde(skip)]
    preserved_invalid_fields: Vec<PreservedStoredField>,
}

/// What a save had to do to keep another writer's changes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SaveOutcome {
    /// The document had been replaced since it was read, so it was read again
    /// and the two sets of changes were combined.
    pub merged: bool,
    /// Nodes another writer added that this save carried forward.
    pub nodes_kept: usize,
    /// Workspaces another writer added that this save carried forward.
    pub workspaces_kept: usize,
}

fn default_max_auto_saved() -> usize {
    crate::settings::defaults::provisional::MAX_AUTO_SAVED_SESSIONS
}

impl Default for SessionStore {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            next_id: 1,
            root: Vec::new(),
            auto_saved: Vec::new(),
            max_auto_saved: default_max_auto_saved(),
            workspaces: Vec::new(),
            last_workspace: None,
            restore_last_workspace: false,
            layers: SettingsLayers::new(),
            unknown: BTreeMap::new(),
            stamp: DocumentStamp::default(),
            baseline_nodes: BTreeSet::new(),
            baseline_workspaces: BTreeSet::new(),
            preserved_invalid_fields: Vec::new(),
        }
    }
}

/// Corrections a load had to make to a document that contradicted itself.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LoadRepairs {
    /// Nodes that shared an identifier with an earlier one and were given a new
    /// one, with every reference to them rewritten.
    pub duplicate_ids: usize,
    /// Sessions whose declared kind disagreed with their settings.
    pub kinds_corrected: usize,
}

impl LoadRepairs {
    /// True when the document needed no correction.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        *self == LoadRepairs::default()
    }
}

/// Outcome of loading a document.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadOutcome {
    /// The store, which is the built-in empty store when the document was
    /// missing or unreadable.
    pub store: SessionStore,
    /// Set when the document was unreadable: the path the damaged document was
    /// moved to before an empty store was returned.
    pub recovered_backup: Option<PathBuf>,
    /// Set when the document carried an older schema version that was migrated.
    pub migrated_from: Option<u32>,
    /// Set when the document carried a schema version higher than this build
    /// writes. Everything in it is preserved and a save re-emits that version,
    /// but this build cannot know what the newer one means, so a caller that
    /// wants to be safe opens the document without offering to change it.
    pub newer_schema: Option<u32>,
    /// Corrections made to a document that contradicted itself.
    pub repairs: LoadRepairs,
}

impl SessionStore {
    /// Allocates an unused identifier.
    pub fn next_id(&mut self) -> SessionId {
        self.preserved_invalid_fields
            .retain(|(path, _)| path.as_slice() != [StoredPathSegment::Key("nextId".to_owned())]);
        self.next_id = self.next_id.max(1);
        let id = SessionId(format!("n{}", self.next_id));
        self.next_id += 1;
        id
    }

    /// True when the document this store came from was written against a schema
    /// version higher than this build writes.
    #[must_use]
    pub fn is_newer_schema(&self) -> bool {
        self.schema_version > SCHEMA_VERSION
    }

    /// Finds a node anywhere in the tree.
    #[must_use]
    pub fn find(&self, id: &SessionId) -> Option<&TreeNode> {
        fn walk<'a>(nodes: &'a [TreeNode], id: &SessionId) -> Option<&'a TreeNode> {
            for node in nodes {
                if node.id() == id {
                    return Some(node);
                }
                if let Some(found) = walk(node.children(), id) {
                    return Some(found);
                }
            }
            None
        }
        walk(&self.root, id)
    }

    /// Finds a saved session anywhere in the tree.
    #[must_use]
    pub fn find_session(&self, id: &SessionId) -> Option<&SavedSession> {
        match self.find(id) {
            Some(TreeNode::Session(session)) => Some(session),
            _ => None,
        }
    }

    /// Every saved session in the tree, depth first.
    #[must_use]
    pub fn sessions(&self) -> Vec<&SavedSession> {
        fn walk<'a>(nodes: &'a [TreeNode], out: &mut Vec<&'a SavedSession>) {
            for node in nodes {
                match node {
                    TreeNode::Session(session) => out.push(session),
                    TreeNode::Folder { children, .. } => walk(children, out),
                    TreeNode::Unknown(_) => {}
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.root, &mut out);
        out
    }

    /// Mutable access to a node's parent list and its index within it.
    fn locate_mut(&mut self, id: &SessionId) -> Option<(&mut Vec<TreeNode>, usize)> {
        fn walk<'a>(
            nodes: &'a mut Vec<TreeNode>,
            id: &SessionId,
        ) -> Option<(&'a mut Vec<TreeNode>, usize)> {
            let direct = nodes.iter().position(|node| node.id() == id);
            if let Some(index) = direct {
                return Some((nodes, index));
            }
            for node in nodes.iter_mut() {
                if let TreeNode::Folder { children, .. } = node {
                    if let Some(found) = walk(children, id) {
                        return Some(found);
                    }
                }
            }
            None
        }
        walk(&mut self.root, id)
    }

    /// The child list of a folder, or the top level when `parent` is `None`.
    fn children_mut(&mut self, parent: Option<&SessionId>) -> Result<&mut Vec<TreeNode>> {
        fn walk<'a>(nodes: &'a mut [TreeNode], id: &SessionId) -> Option<&'a mut Vec<TreeNode>> {
            for node in nodes.iter_mut() {
                if let TreeNode::Folder {
                    id: folder_id,
                    children,
                    ..
                } = node
                {
                    if folder_id == id {
                        return Some(children);
                    }
                    if let Some(found) = walk(children, id) {
                        return Some(found);
                    }
                }
            }
            None
        }
        let Some(parent) = parent else {
            return Ok(&mut self.root);
        };
        walk(&mut self.root, parent).ok_or_else(|| Error::NoSuchNode(parent.to_string()))
    }

    /// The names already used directly under `parent`.
    fn sibling_names(&self, parent: Option<&SessionId>) -> Result<Vec<String>> {
        let nodes = match parent {
            None => &self.root[..],
            Some(parent) => {
                let node = self
                    .find(parent)
                    .ok_or_else(|| Error::NoSuchNode(parent.to_string()))?;
                if !node.is_folder() {
                    return Err(Error::InvalidMove("destination is not a folder"));
                }
                node.children()
            }
        };
        Ok(nodes.iter().map(|node| node.name().to_owned()).collect())
    }

    /// Creates a session folder and returns its identifier.
    ///
    /// # Errors
    /// Returns [`Error::NoSuchNode`] for an unknown parent and
    /// [`Error::DuplicateName`] when a sibling already carries the name.
    pub fn create_folder(
        &mut self,
        parent: Option<&SessionId>,
        name: impl Into<String>,
    ) -> Result<SessionId> {
        let name = name.into();
        let id = self.next_id();
        let children = self.children_mut(parent)?;
        if children.iter().any(|node| same_name(node.name(), &name)) {
            return Err(Error::DuplicateName(name));
        }
        children.push(TreeNode::Folder {
            id: id.clone(),
            name,
            children: Vec::new(),
            unknown: BTreeMap::new(),
        });
        Ok(id)
    }

    /// Adds a session to the tree and returns its identifier.
    ///
    /// # Errors
    /// Returns [`Error::NoSuchNode`] for an unknown parent and
    /// [`Error::DuplicateName`] when a sibling already carries the name.
    pub fn add_session(
        &mut self,
        parent: Option<&SessionId>,
        session: SavedSession,
    ) -> Result<SessionId> {
        let id = session.id.clone();
        let children = self.children_mut(parent)?;
        if children
            .iter()
            .any(|node| same_name(node.name(), &session.name))
        {
            return Err(Error::DuplicateName(session.name));
        }
        children.push(TreeNode::Session(session));
        Ok(id)
    }

    /// Renames a node.
    ///
    /// # Errors
    /// Returns [`Error::NoSuchNode`], [`Error::DuplicateName`] when a sibling
    /// already carries the name, or [`Error::ReadOnly`] for a locked session.
    pub fn rename(&mut self, id: &SessionId, new_name: impl Into<String>) -> Result<()> {
        let new_name = new_name.into();
        let (siblings, index) = self
            .locate_mut(id)
            .ok_or_else(|| Error::NoSuchNode(id.to_string()))?;
        if siblings
            .iter()
            .enumerate()
            .any(|(i, node)| i != index && same_name(node.name(), &new_name))
        {
            return Err(Error::DuplicateName(new_name));
        }
        if let TreeNode::Session(session) = &siblings[index] {
            if session.locked {
                return Err(Error::ReadOnly("session is locked"));
            }
        }
        siblings[index].set_name(new_name);
        Ok(())
    }

    /// Moves a node into another folder, or to the top level.
    ///
    /// Where a session sits is part of what is saved about it, so a locked
    /// session refuses a move for the same reason it refuses a rename. Every
    /// check runs before anything is removed: a move that fails leaves the tree
    /// exactly as it was.
    ///
    /// # Errors
    /// Returns [`Error::NoSuchNode`], [`Error::InvalidMove`] when the
    /// destination is the node itself, one of its descendants or not a folder,
    /// [`Error::DuplicateName`] on a name collision at the destination, and
    /// [`Error::ReadOnly`] for a locked session.
    pub fn move_node(&mut self, id: &SessionId, new_parent: Option<&SessionId>) -> Result<()> {
        let node = self
            .find(id)
            .ok_or_else(|| Error::NoSuchNode(id.to_string()))?;
        let name = node.name().to_owned();
        if let TreeNode::Session(session) = node {
            if session.locked {
                return Err(Error::ReadOnly("session is locked"));
            }
        }
        if let Some(parent) = new_parent {
            if node.contains(parent) {
                return Err(Error::InvalidMove("destination is inside the moved node"));
            }
        }

        let taken = self.sibling_names(new_parent)?;
        if taken
            .iter()
            .any(|other| same_name(other, &name) && self.parent_of(id).as_ref() != new_parent)
        {
            return Err(Error::DuplicateName(name));
        }

        let node = {
            let (siblings, index) = self
                .locate_mut(id)
                .ok_or_else(|| Error::NoSuchNode(id.to_string()))?;
            siblings.remove(index)
        };
        // The destination was resolved above, so this lookup cannot fail; a
        // failure here would leave the node held outside the tree.
        match self.children_mut(new_parent) {
            Ok(destination) => destination.push(node),
            Err(err) => {
                self.root.push(node);
                return Err(err);
            }
        }
        Ok(())
    }

    /// The folder holding `id`, or `None` when it sits at the top level.
    fn parent_of(&self, id: &SessionId) -> Option<SessionId> {
        fn walk(
            nodes: &[TreeNode],
            id: &SessionId,
            parent: Option<&SessionId>,
        ) -> Option<SessionId> {
            for node in nodes {
                if node.id() == id {
                    return Some(parent.cloned().unwrap_or_else(|| SessionId::from_raw("")));
                }
                if let Some(found) = walk(node.children(), id, Some(node.id())) {
                    return Some(found);
                }
            }
            None
        }
        let found = walk(&self.root, id, None)?;
        if found.as_str().is_empty() {
            None
        } else {
            Some(found)
        }
    }

    /// Removes a node and everything beneath it.
    ///
    /// # Errors
    /// Returns [`Error::NoSuchNode`] or [`Error::ReadOnly`] for a locked
    /// session.
    pub fn delete(&mut self, id: &SessionId) -> Result<TreeNode> {
        let (siblings, index) = self
            .locate_mut(id)
            .ok_or_else(|| Error::NoSuchNode(id.to_string()))?;
        if let TreeNode::Session(session) = &siblings[index] {
            if session.locked {
                return Err(Error::ReadOnly("session is locked"));
            }
        }
        Ok(siblings.remove(index))
    }

    /// Copies a node next to itself under a free name and returns the copy's
    /// identifier. Descendants are copied too, each with a fresh identifier.
    ///
    /// # Errors
    /// Returns [`Error::NoSuchNode`] when `id` is not in the tree.
    pub fn duplicate(&mut self, id: &SessionId) -> Result<SessionId> {
        let original = self
            .find(id)
            .ok_or_else(|| Error::NoSuchNode(id.to_string()))?
            .clone();
        let taken: Vec<String> = {
            let (siblings, _) = self
                .locate_mut(id)
                .ok_or_else(|| Error::NoSuchNode(id.to_string()))?;
            siblings.iter().map(|node| node.name().to_owned()).collect()
        };
        let mut copy = self.reidentify(original);
        copy.set_name(free_name(copy.name(), &taken));
        let new_id = copy.id().clone();
        let (siblings, index) = self
            .locate_mut(id)
            .ok_or_else(|| Error::NoSuchNode(id.to_string()))?;
        siblings.insert(index + 1, copy);
        Ok(new_id)
    }

    /// Rewrites a subtree's identifiers so a copy shares none with the original.
    fn reidentify(&mut self, node: TreeNode) -> TreeNode {
        match node {
            TreeNode::Folder {
                name,
                children,
                unknown,
                ..
            } => {
                let id = self.next_id();
                let children = children
                    .into_iter()
                    .map(|child| self.reidentify(child))
                    .collect();
                TreeNode::Folder {
                    id,
                    name,
                    children,
                    unknown,
                }
            }
            TreeNode::Session(mut session) => {
                session.id = self.next_id();
                session.locked = false;
                TreeNode::Session(session)
            }
            TreeNode::Unknown(mut node) => {
                let id = self.next_id();
                node.set_id(id);
                TreeNode::Unknown(node)
            }
        }
    }

    /// Sets or clears the lock on a saved session.
    ///
    /// # Errors
    /// Returns [`Error::NoSuchNode`] when `id` does not name a saved session.
    pub fn set_locked(&mut self, id: &SessionId, locked: bool) -> Result<()> {
        let (siblings, index) = self
            .locate_mut(id)
            .ok_or_else(|| Error::NoSuchNode(id.to_string()))?;
        match &mut siblings[index] {
            TreeNode::Session(session) => {
                session.locked = locked;
                Ok(())
            }
            TreeNode::Folder { .. } | TreeNode::Unknown(_) => {
                Err(Error::NoSuchNode(id.to_string()))
            }
        }
    }

    /// True when `id` names a locked session.
    #[must_use]
    pub fn is_locked(&self, id: &SessionId) -> bool {
        self.find_session(id).is_some_and(|session| session.locked)
    }

    /// Replaces the settings of a saved session.
    ///
    /// # Errors
    /// Returns [`Error::NoSuchNode`], [`Error::ReadOnly`] for a locked session,
    /// or [`Error::KindMismatch`] when the settings belong to another kind.
    pub fn update_session_settings(
        &mut self,
        id: &SessionId,
        settings: SessionSettingsOverride,
    ) -> Result<()> {
        let (siblings, index) = self
            .locate_mut(id)
            .ok_or_else(|| Error::NoSuchNode(id.to_string()))?;
        let TreeNode::Session(session) = &mut siblings[index] else {
            return Err(Error::NoSuchNode(id.to_string()));
        };
        if session.locked {
            return Err(Error::ReadOnly("session is locked"));
        }
        if settings.kind() != session.kind {
            return Err(Error::KindMismatch {
                expected: session.kind.clone(),
                found: settings.kind(),
            });
        }
        session.settings = settings;
        Ok(())
    }

    /// Records a session that was used without being named.
    ///
    /// The newest entry is kept first and the list is trimmed to
    /// [`SessionStore::max_auto_saved`]. A cap of zero disables automatic
    /// saving and discards whatever was already held. An entry whose identifier
    /// is already present is moved to the front rather than duplicated.
    pub fn record_auto_saved(&mut self, session: SavedSession) {
        if self.max_auto_saved == 0 {
            self.auto_saved.clear();
            self.drop_preserved_auto_saved_from(0);
            return;
        }
        if let Some(existing) = self
            .auto_saved
            .iter()
            .position(|held| held.id == session.id)
        {
            self.shift_preserved_auto_saved_after_remove(existing);
        }
        self.auto_saved.retain(|held| held.id != session.id);
        self.auto_saved.insert(0, session);
        self.auto_saved.truncate(self.max_auto_saved);
        self.shift_preserved_auto_saved_after_insert();
    }

    /// Changes the automatic save cap, trimming the held list to match.
    pub fn set_max_auto_saved(&mut self, max: usize) {
        self.max_auto_saved = max;
        self.auto_saved.truncate(max);
        self.preserved_invalid_fields.retain(|(path, _)| {
            !matches!(path.as_slice(), [StoredPathSegment::Key(key), StoredPathSegment::Index(index), ..] if key == "autoSaved" && *index >= max)
                && path.as_slice() != [StoredPathSegment::Key("maxAutoSaved".to_owned())]
        });
    }

    fn drop_preserved_auto_saved_from(&mut self, first: usize) {
        self.preserved_invalid_fields.retain(|(path, _)| {
            !matches!(path.as_slice(), [StoredPathSegment::Key(key), StoredPathSegment::Index(index), ..] if key == "autoSaved" && *index >= first)
        });
    }

    fn shift_preserved_auto_saved_after_insert(&mut self) {
        self.shift_preserved_auto_saved_after_insert_at(0);
    }

    fn shift_preserved_auto_saved_after_insert_at(&mut self, inserted: usize) {
        for (path, _) in &mut self.preserved_invalid_fields {
            if let [StoredPathSegment::Key(key), StoredPathSegment::Index(index), ..] =
                path.as_mut_slice()
            {
                if key == "autoSaved" && *index >= inserted {
                    *index = index.saturating_add(1);
                }
            }
        }
        self.drop_preserved_auto_saved_from(self.auto_saved.len());
    }

    fn shift_preserved_auto_saved_after_remove(&mut self, removed: usize) {
        self.preserved_invalid_fields.retain_mut(|(path, _)| {
            let [StoredPathSegment::Key(key), StoredPathSegment::Index(index), ..] =
                path.as_mut_slice()
            else {
                return true;
            };
            if key != "autoSaved" {
                return true;
            }
            if *index == removed {
                return false;
            }
            if *index > removed {
                *index -= 1;
            }
            true
        });
    }

    fn drop_preserved_workspace(&mut self, index: usize) {
        self.preserved_invalid_fields.retain(|(path, _)| {
            !matches!(path.as_slice(), [StoredPathSegment::Key(key), StoredPathSegment::Index(workspace), ..] if key == "workspaces" && *workspace == index)
        });
    }

    fn shift_preserved_workspaces_after_remove(&mut self, removed: usize) {
        self.preserved_invalid_fields.retain_mut(|(path, _)| {
            let [StoredPathSegment::Key(key), StoredPathSegment::Index(index), ..] =
                path.as_mut_slice()
            else {
                return true;
            };
            if key != "workspaces" {
                return true;
            }
            if *index == removed {
                return false;
            }
            if *index > removed {
                *index -= 1;
            }
            true
        });
    }

    /// Moves an automatically saved session into the named tree under a new
    /// name.
    ///
    /// # Errors
    /// Returns [`Error::NoSuchNode`] when the identifier is not in the
    /// automatically saved list, or [`Error::DuplicateName`] on a collision.
    pub fn promote_auto_saved(
        &mut self,
        id: &SessionId,
        name: impl Into<String>,
        parent: Option<&SessionId>,
    ) -> Result<SessionId> {
        self.promote_auto_saved_inner(id, name.into(), parent, None)
    }

    /// Promotes a recent session with its current settings as one mutation.
    ///
    /// # Errors
    /// Returns the promotion errors, [`Error::ReadOnly`] for a locked record,
    /// or [`Error::KindMismatch`] for settings of another kind.
    pub fn promote_auto_saved_with_settings(
        &mut self,
        id: &SessionId,
        name: impl Into<String>,
        parent: Option<&SessionId>,
        settings: SessionSettingsOverride,
    ) -> Result<SessionId> {
        self.promote_auto_saved_inner(id, name.into(), parent, Some(settings))
    }

    fn promote_auto_saved_inner(
        &mut self,
        id: &SessionId,
        name: String,
        parent: Option<&SessionId>,
        settings: Option<SessionSettingsOverride>,
    ) -> Result<SessionId> {
        let index = self
            .auto_saved
            .iter()
            .position(|session| &session.id == id)
            .ok_or_else(|| Error::NoSuchNode(id.to_string()))?;
        if let Some(settings) = &settings {
            let session = &self.auto_saved[index];
            if session.locked {
                return Err(Error::ReadOnly("session is locked"));
            }
            if settings.kind() != session.kind {
                return Err(Error::KindMismatch {
                    expected: session.kind.clone(),
                    found: settings.kind(),
                });
            }
        }
        let preserved_invalid_fields = self.preserved_invalid_fields.clone();
        let original = self.auto_saved.remove(index);
        let mut session = original.clone();
        self.shift_preserved_auto_saved_after_remove(index);
        session.name = name;
        if let Some(settings) = settings {
            session.settings = settings;
        }
        match self.add_session(parent, session.clone()) {
            Ok(new_id) => Ok(new_id),
            Err(err) => {
                self.auto_saved.insert(index, original);
                self.preserved_invalid_fields = preserved_invalid_fields;
                Err(err)
            }
        }
    }

    /// Stores a workspace, replacing one of the same name.
    pub fn save_workspace(&mut self, workspace: Workspace) {
        match self
            .workspaces
            .iter()
            .position(|held| same_name(&held.name, &workspace.name))
        {
            Some(index) => {
                self.drop_preserved_workspace(index);
                self.workspaces[index] = workspace;
            }
            None => self.workspaces.push(workspace),
        }
    }

    /// A workspace by name.
    #[must_use]
    pub fn workspace(&self, name: &str) -> Option<&Workspace> {
        self.workspaces
            .iter()
            .find(|held| same_name(&held.name, name))
    }

    /// Removes a workspace by name, reporting whether one was removed.
    pub fn delete_workspace(&mut self, name: &str) -> bool {
        let Some(index) = self
            .workspaces
            .iter()
            .position(|held| same_name(&held.name, name))
        else {
            return false;
        };
        self.workspaces.remove(index);
        self.shift_preserved_workspaces_after_remove(index);
        true
    }

    /// Renames a workspace.
    ///
    /// # Errors
    /// Returns [`Error::NoSuchNode`] when no workspace carries `name`, or
    /// [`Error::DuplicateName`] when `new_name` is already in use.
    pub fn rename_workspace(&mut self, name: &str, new_name: impl Into<String>) -> Result<()> {
        let new_name = new_name.into();
        if self
            .workspaces
            .iter()
            .any(|held| same_name(&held.name, &new_name))
        {
            return Err(Error::DuplicateName(new_name));
        }
        let workspace = self
            .workspaces
            .iter_mut()
            .find(|held| same_name(&held.name, name))
            .ok_or_else(|| Error::NoSuchNode(name.to_owned()))?;
        workspace.name = new_name;
        Ok(())
    }

    /// Every identifier the store holds, in tree order then automatically saved
    /// order.
    fn collect_ids(&self) -> Vec<SessionId> {
        fn walk(nodes: &[TreeNode], out: &mut Vec<SessionId>) {
            for node in nodes {
                out.push(node.id().clone());
                walk(node.children(), out);
            }
        }
        let mut out = Vec::new();
        walk(&self.root, &mut out);
        out.extend(self.auto_saved.iter().map(|session| session.id.clone()));
        out
    }

    /// Raises the generator above every identifier already present, gives a new
    /// identifier to each later holder of a duplicate. References to the
    /// original identifier continue to name its first holder.
    ///
    /// A generator left below an identifier already in use hands the next new
    /// node an identifier another node answers to, so a later delete or rename
    /// reaches the wrong session.
    fn repair_ids(&mut self) -> usize {
        let highest = self
            .collect_ids()
            .iter()
            .filter_map(SessionId::counter)
            .max()
            .unwrap_or(0);
        self.next_id = self.next_id.max(highest.saturating_add(1)).max(1);

        let mut seen: BTreeSet<SessionId> = BTreeSet::new();
        let mut remapped: Vec<(SessionId, SessionId)> = Vec::new();
        let mut pending: Vec<SessionId> = Vec::new();
        for id in self.collect_ids() {
            if id.as_str().is_empty() {
                continue;
            }
            if seen.contains(&id) {
                pending.push(id);
            } else {
                seen.insert(id);
            }
        }
        for duplicate in pending {
            let fresh = self.next_id();
            remapped.push((duplicate, fresh));
        }
        if remapped.is_empty() {
            return 0;
        }
        let count = remapped.len();
        for (old, new) in remapped {
            let mut seen_first = false;
            if !replace_second_id_inner(&mut self.root, &old, &new, &mut seen_first) {
                for session in &mut self.auto_saved {
                    if session.id == old {
                        if seen_first {
                            session.id = new;
                            break;
                        }
                        seen_first = true;
                    }
                }
            }
        }
        count
    }

    /// Makes each session's declared kind agree with its settings.
    fn repair_kinds(&mut self) -> usize {
        fn walk(nodes: &mut [TreeNode], count: &mut usize) {
            for node in nodes.iter_mut() {
                match node {
                    TreeNode::Session(session) => {
                        if session.repair_kind() {
                            *count += 1;
                        }
                    }
                    TreeNode::Folder { children, .. } => walk(children, count),
                    TreeNode::Unknown(_) => {}
                }
            }
        }
        let mut count = 0;
        walk(&mut self.root, &mut count);
        for session in &mut self.auto_saved {
            if session.repair_kind() {
                count += 1;
            }
        }
        for workspace in &mut self.workspaces {
            for window in &mut workspace.windows {
                for tab in &mut window.tabs {
                    if let WorkspaceTab::Unsaved { session, .. } = tab {
                        if session.repair_kind() {
                            count += 1;
                        }
                    }
                }
            }
        }
        count
    }

    /// Reads a store from disk.
    ///
    /// A missing document yields an empty store. A document that cannot be
    /// parsed at all is moved aside and reported in
    /// [`LoadOutcome::recovered_backup`] rather than being overwritten, so
    /// nothing is lost to a later save. A document naming kinds, nodes or
    /// settings this build has no variant for is not damaged: those are held as
    /// raw JSON and written back unchanged.
    ///
    /// # Errors
    /// Returns [`Error::Io`] when the document exists but cannot be read or
    /// moved aside.
    pub fn load(path: &Path) -> Result<LoadOutcome> {
        let stamp = DocumentStamp(stamp_of(path)?);
        let bytes = match crate::document::read(path) {
            Ok(bytes) => bytes,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(LoadOutcome {
                    store: SessionStore::default(),
                    recovered_backup: None,
                    migrated_from: None,
                    newer_schema: None,
                    repairs: LoadRepairs::default(),
                });
            }
            Err(source) => {
                return Err(Error::Io {
                    path: path.to_path_buf(),
                    source,
                })
            }
        };

        // Bytes that are not text are as unreadable as text that is not JSON,
        // and are moved aside by the same rule.
        let text = String::from_utf8(bytes).ok();
        let Some(Ok(mut document)) = text.as_deref().map(serde_json::from_str::<Value>) else {
            let backup = quarantine(path)?;
            return Ok(LoadOutcome {
                store: SessionStore::default(),
                recovered_backup: Some(backup),
                migrated_from: None,
                newer_schema: None,
                repairs: LoadRepairs::default(),
            });
        };

        // A document without the field predates versioning rather than matching
        // the current version, so every migration step runs over it.
        let version = document.get("schemaVersion");
        let found = version.and_then(stored_schema_version).unwrap_or_else(|| {
            if version.and_then(Value::as_u64).is_some() {
                u32::MAX
            } else if version.is_some() {
                SCHEMA_VERSION
            } else {
                0
            }
        });
        let newer_schema = (found > SCHEMA_VERSION).then_some(found);
        let preserved_invalid_fields = sanitize_known_field_types(&mut document);
        let migrated_from = migrate(&mut document, found);
        // Read the original JSON directly when no migration changed it. This
        // keeps arbitrary precision numbers intact in unknown flattened fields;
        // `from_value` buffers tagged/flattened values through serde Content,
        // which cannot represent every such number.
        let decoded = if migrated_from.is_none() && preserved_invalid_fields.is_empty() {
            serde_json::from_str::<SessionStore>(text.as_deref().unwrap_or_default())
        } else {
            serde_json::to_string(&document)
                .and_then(|migrated| serde_json::from_str::<SessionStore>(&migrated))
        };
        let Ok(mut store) = decoded else {
            let backup = quarantine(path)?;
            return Ok(LoadOutcome {
                store: SessionStore::default(),
                recovered_backup: Some(backup),
                migrated_from: None,
                newer_schema: None,
                repairs: LoadRepairs::default(),
            });
        };
        store.schema_version = found.max(SCHEMA_VERSION);
        store.stamp = stamp;
        store.preserved_invalid_fields = preserved_invalid_fields;
        let repairs = LoadRepairs {
            duplicate_ids: store.repair_ids(),
            kinds_corrected: store.repair_kinds(),
        };
        store.record_baseline();
        Ok(LoadOutcome {
            store,
            recovered_backup: None,
            migrated_from,
            newer_schema,
            repairs,
        })
    }

    /// Writes the store, replacing the document it was read from.
    ///
    /// A document replaced by something else since this store was loaded is not
    /// overwritten: the caller is told instead, so it can merge or ask, rather
    /// than losing whatever the other writer stored.
    ///
    /// # Errors
    /// Returns [`Error::DocumentChanged`] when the file on disk is no longer
    /// the one this store was read from, [`Error::Io`] when the document cannot
    /// be written or renamed into place, and [`Error::Parse`] when the store
    /// cannot be serialized.
    pub fn save(&mut self, path: &Path) -> Result<()> {
        let current = DocumentStamp(stamp_of(path)?);
        if current.0 != self.stamp.0 {
            return Err(Error::DocumentChanged {
                path: path.to_path_buf(),
            });
        }
        self.save_replacing(path)
    }

    /// Writes the store whatever is on disk, discarding another writer's
    /// document.
    ///
    /// Only for a caller that has already resolved the conflict [`Self::save`]
    /// reports.
    ///
    /// # Errors
    /// Returns [`Error::Io`] or [`Error::Parse`].
    pub fn save_replacing(&mut self, path: &Path) -> Result<()> {
        atomic_write(path, |buffer| {
            let buffer = crate::document::Writer::new(buffer);
            if self.preserved_invalid_fields.is_empty() {
                serde_json::to_writer_pretty(buffer, self)
            } else {
                let mut document = serde_json::to_value(&*self).map_err(|source| Error::Parse {
                    path: path.to_path_buf(),
                    source,
                })?;
                restore_preserved_fields(&mut document, &self.preserved_invalid_fields);
                serde_json::to_writer_pretty(buffer, &document)
            }
            .map_err(|source| Error::Parse {
                path: path.to_path_buf(),
                source,
            })
        })?;
        self.stamp = DocumentStamp(stamp_of(path)?);
        self.record_baseline();
        Ok(())
    }

    /// Writes the store, combining it with another writer's document rather
    /// than refusing or overwriting.
    ///
    /// A plain [`Self::save`] succeeds when nothing replaced the document. When
    /// something did, the document is read again and whatever the other writer
    /// added is carried into this store before the write, so neither writer's
    /// work is dropped.
    ///
    /// # Errors
    /// Returns [`Error::Io`] or [`Error::Parse`].
    pub fn save_merging(&mut self, path: &Path) -> Result<SaveOutcome> {
        match self.save(path) {
            Ok(()) => Ok(SaveOutcome::default()),
            Err(Error::DocumentChanged { .. }) => {
                let disk = SessionStore::load(path)?.store;
                let mut outcome = self.merge_additions(&disk);
                outcome.merged = true;
                self.stamp = disk.stamp.clone();
                self.save(path)?;
                Ok(outcome)
            }
            Err(other) => Err(other),
        }
    }

    /// Notes what the document held, so a later merge can tell an addition by
    /// another writer from a deletion by this one.
    fn record_baseline(&mut self) {
        self.baseline_nodes = self.collect_ids().into_iter().collect();
        self.baseline_workspaces = self
            .workspaces
            .iter()
            .map(|workspace| workspace.name.clone())
            .collect();
    }

    /// Takes from `disk` everything this store has never seen.
    ///
    /// A node this store still holds keeps this store's version: the two
    /// writers edited it and the one saving now is the later of them. A node
    /// absent here but present at load was deleted here, so it stays deleted.
    fn merge_additions(&mut self, disk: &SessionStore) -> SaveOutcome {
        let mut outcome = SaveOutcome::default();
        let mut grafts: Vec<(Option<SessionId>, TreeNode)> = Vec::new();
        collect_additions(&disk.root, None, &self.baseline_nodes, &mut grafts);
        for (parent, mut node) in grafts {
            self.reissue_ids(&mut node);
            let target = parent.filter(|id| matches!(self.find(id), Some(TreeNode::Folder { .. })));
            let siblings = match target.as_ref().and_then(|id| self.locate_children(id)) {
                Some(children) => children,
                None => &mut self.root,
            };
            let taken: Vec<String> = siblings.iter().map(|held| held.name().to_owned()).collect();
            let name = free_name(node.name(), &taken);
            node.set_name(name);
            siblings.push(node);
            outcome.nodes_kept += 1;
        }

        for workspace in &disk.workspaces {
            if self.baseline_workspaces.contains(&workspace.name)
                || self.workspace(&workspace.name).is_some()
            {
                continue;
            }
            self.workspaces.push(workspace.clone());
            outcome.workspaces_kept += 1;
        }

        for session in disk.auto_saved.iter().rev() {
            if self.auto_saved.iter().any(|held| held.id == session.id) {
                continue;
            }
            self.auto_saved.push(session.clone());
        }
        self.auto_saved.truncate(self.max_auto_saved);

        for kind in disk.layers.edited_kinds() {
            if self.layers.session_defaults(kind).is_none() {
                if let Some(defaults) = disk.layers.session_defaults(kind) {
                    let _ = self.layers.set_session_defaults(kind, defaults.clone());
                }
            }
        }

        for (key, value) in &disk.unknown {
            self.unknown
                .entry(key.clone())
                .or_insert_with(|| value.clone());
        }
        self.repair_ids();
        outcome
    }

    /// Gives a grafted subtree identifiers this store does not already use.
    ///
    /// Two instances generate identifiers from counters of their own, so the
    /// same identifier can name a different node in each document.
    fn reissue_ids(&mut self, node: &mut TreeNode) {
        if self.find(node.id()).is_some() {
            let fresh = self.next_id();
            set_node_id(node, fresh);
        }
        if let TreeNode::Folder { children, .. } = node {
            let mut taken = std::mem::take(children);
            for child in &mut taken {
                self.reissue_ids(child);
            }
            if let TreeNode::Folder { children, .. } = node {
                *children = taken;
            }
        }
    }

    /// The child list of a folder, for grafting a node into it.
    fn locate_children(&mut self, id: &SessionId) -> Option<&mut Vec<TreeNode>> {
        fn walk<'a>(nodes: &'a mut [TreeNode], id: &SessionId) -> Option<&'a mut Vec<TreeNode>> {
            for node in nodes.iter_mut() {
                if let TreeNode::Folder {
                    id: folder,
                    children,
                    ..
                } = node
                {
                    if folder == id {
                        return Some(children);
                    }
                    if let Some(found) = walk(children, id) {
                        return Some(found);
                    }
                }
            }
            None
        }
        walk(&mut self.root, id)
    }
}

/// Finds every node of `nodes` that the document held no copy of when it was
/// read, with the folder it sat in.
///
/// A whole folder another writer added is taken as one node, so its children
/// are not grafted a second time.
fn collect_additions(
    nodes: &[TreeNode],
    parent: Option<&SessionId>,
    baseline: &BTreeSet<SessionId>,
    out: &mut Vec<(Option<SessionId>, TreeNode)>,
) {
    for node in nodes {
        let id = node.id();
        if !baseline.contains(id) {
            out.push((parent.cloned(), node.clone()));
            continue;
        }
        collect_additions(node.children(), Some(id), baseline, out);
    }
}

/// Replaces the next duplicate in traversal order, preserving the first holder.
fn replace_second_id_inner(
    nodes: &mut [TreeNode],
    old: &SessionId,
    new: &SessionId,
    seen_first: &mut bool,
) -> bool {
    for node in nodes.iter_mut() {
        if node.id() == old {
            if *seen_first {
                set_node_id(node, new.clone());
                return true;
            }
            *seen_first = true;
        }
        if let TreeNode::Folder { children, .. } = node {
            if replace_second_id_inner(children, old, new, seen_first) {
                return true;
            }
        }
    }
    false
}

fn set_node_id(node: &mut TreeNode, new: SessionId) {
    match node {
        TreeNode::Folder { id, .. } => *id = new,
        TreeNode::Session(session) => session.id = new,
        TreeNode::Unknown(unknown) => unknown.set_id(new),
    }
}

/// Appends a numeric suffix until the name is free among `taken`.
///
/// Capitalization does not make a name free, and the search gives up after
/// [`NAME_ATTEMPTS`] candidates rather than counting without bound.
pub(crate) fn free_name(base: &str, taken: &[String]) -> String {
    let is_taken = |candidate: &str| taken.iter().any(|name| same_name(name, candidate));
    if !is_taken(base) {
        return base.to_owned();
    }
    for counter in 2..=NAME_ATTEMPTS {
        let candidate = format!("{base} ({counter})");
        if !is_taken(&candidate) {
            return candidate;
        }
    }
    format!("{base} ({})", unique_suffix())
}

/// A suffix no other writer in this process or another produces at the same
/// moment.
fn unique_suffix() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{}-{nanos}-{counter}", std::process::id())
}

/// Moves an unreadable document aside under a name no earlier quarantine holds
/// and returns that path.
///
/// The name is reserved by creating it before the rename, so two instances
/// quarantining at the same moment cannot write over each other's copy.
pub(crate) fn quarantine(path: &Path) -> Result<PathBuf> {
    let stamp = now_seconds();
    for counter in 0..QUARANTINE_ATTEMPTS {
        let mut name = path.as_os_str().to_os_string();
        name.push(format!(".corrupt.{stamp}.{}.{counter}", std::process::id()));
        let candidate = PathBuf::from(name);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(_) => {
                fs::rename(path, &candidate).map_err(|source| Error::Io {
                    path: candidate.clone(),
                    source,
                })?;
                return Ok(candidate);
            }
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(Error::Io {
                    path: candidate,
                    source,
                })
            }
        }
    }
    Err(Error::Io {
        path: path.to_path_buf(),
        source: std::io::Error::new(std::io::ErrorKind::AlreadyExists, "no free quarantine name"),
    })
}

/// Brings a document forward from an older schema version.
///
/// Returns the version migrated from, or `None` when the document was already
/// current or newer. Steps run oldest first and rewrite the document in place,
/// so fields written by another build survive the migration.
fn migrate(document: &mut Value, found: u32) -> Option<u32> {
    if found >= SCHEMA_VERSION {
        return None;
    }
    // Version 0 is a document written before the version field existed. It is
    // shaped like version 1 apart from carrying no version, so the step is to
    // stamp one on.
    if let Some(object) = document.as_object_mut() {
        object.insert(
            "schemaVersion".to_owned(),
            Value::from(u64::from(SCHEMA_VERSION)),
        );
    }
    Some(found)
}

fn stored_schema_version(value: &Value) -> Option<u32> {
    value
        .as_u64()
        .and_then(|version| u32::try_from(version).ok())
}

fn capture_invalid_field(
    object: &mut serde_json::Map<String, Value>,
    key: &str,
    valid: impl FnOnce(&Value) -> bool,
    path: &mut Vec<StoredPathSegment>,
    preserved: &mut Vec<PreservedStoredField>,
) {
    if object.get(key).is_some_and(valid) {
        return;
    }
    if let Some(original) = object.remove(key) {
        path.push(StoredPathSegment::Key(key.to_owned()));
        preserved.push((path.clone(), original));
        path.pop();
    }
}

/// Removes known values that this version cannot interpret before serde reads
/// the document. Their exact JSON values are restored when the store is saved.
fn sanitize_known_field_types(document: &mut Value) -> Vec<PreservedStoredField> {
    let mut preserved = Vec::new();
    let mut path = Vec::new();
    let Some(root) = document.as_object_mut() else {
        return preserved;
    };
    capture_invalid_field(
        root,
        "schemaVersion",
        |value| stored_schema_version(value).is_some(),
        &mut path,
        &mut preserved,
    );
    capture_invalid_field(
        root,
        "nextId",
        |value| value.as_u64().is_some(),
        &mut path,
        &mut preserved,
    );
    capture_invalid_field(
        root,
        "maxAutoSaved",
        |value| {
            value
                .as_u64()
                .and_then(|raw| usize::try_from(raw).ok())
                .is_some()
        },
        &mut path,
        &mut preserved,
    );

    if let Some(workspaces) = root.get_mut("workspaces").and_then(Value::as_array_mut) {
        path.push(StoredPathSegment::Key("workspaces".to_owned()));
        for (workspace_index, workspace) in workspaces.iter_mut().enumerate() {
            let Some(windows) = workspace.get_mut("windows").and_then(Value::as_array_mut) else {
                continue;
            };
            path.push(StoredPathSegment::Index(workspace_index));
            path.push(StoredPathSegment::Key("windows".to_owned()));
            for (window_index, window) in windows.iter_mut().enumerate() {
                let Some(object) = window.as_object_mut() else {
                    continue;
                };
                path.push(StoredPathSegment::Index(window_index));
                capture_invalid_field(
                    object,
                    "activeTab",
                    |value| {
                        value
                            .as_u64()
                            .and_then(|raw| usize::try_from(raw).ok())
                            .is_some()
                    },
                    &mut path,
                    &mut preserved,
                );
                path.pop();
            }
            path.pop();
            path.pop();
        }
    }
    preserved
}

/// Writes a file by filling a temporary file beside it and renaming it into
/// place.
///
/// The destination is either the previous document or the new one: a failure
/// while serializing or writing leaves the previous document untouched and
/// removes the temporary file. The temporary name is unique to this writer, so
/// two instances writing at once never share one, and only the writer's own
/// temporary file is removed.
///
/// # Errors
/// Returns whatever `serialize` returns, or [`Error::Io`] when the temporary
/// file cannot be written, flushed or renamed.
pub fn atomic_write<F>(path: &Path, serialize: F) -> Result<()>
where
    F: FnOnce(&mut Vec<u8>) -> Result<()>,
{
    let mut buffer = Vec::new();
    serialize(&mut buffer)?;

    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    if let Some(parent) = parent {
        fs::create_dir_all(parent).map_err(|source| Error::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    ca_io::write_atomic(path, &buffer).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Who holds the settings directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockHolder {
    /// Process identifier written by the holder.
    pub pid: Option<u32>,
    /// Seconds since the epoch when the lock was taken.
    pub acquired_epoch_seconds: Option<i64>,
}

/// Result of trying to claim the settings directory.
#[derive(Debug)]
pub enum LockOutcome {
    /// The directory is now claimed by this instance.
    Acquired(SettingsLock),
    /// Another instance holds it.
    Held {
        /// The lock file itself.
        path: PathBuf,
        /// What that file says about the holder.
        holder: LockHolder,
    },
}

/// A claim on the settings directory, held for as long as this instance intends
/// to write.
///
/// The claim is an advisory lock the operating system holds on an open file
/// handle. The kernel releases it when the handle closes, which includes a
/// process that crashes or is killed, so a claim never outlives its holder. The
/// lock file itself stays on disk and its existence means nothing; only the
/// lock on the handle does. A sibling file records who took the claim and when,
/// for the message shown to a second instance.
///
/// The claim does not stop a writer that ignores it, which is why
/// [`SessionStore::save`] also compares the document on disk against the one it
/// loaded. Dropping the value releases the claim.
#[derive(Debug)]
pub struct SettingsLock {
    path: PathBuf,
    /// The locked handle. Closing it releases the operating system lock, so the
    /// field is held for its lifetime rather than read.
    file: fs::File,
}

impl SettingsLock {
    /// Claims the settings directory for write access.
    ///
    /// # Errors
    /// Returns [`Error::Io`] when the directory or the lock file cannot be
    /// created, or when the lock cannot be tested.
    pub fn acquire(directory: &Path) -> Result<LockOutcome> {
        fs::create_dir_all(directory).map_err(|source| Error::Io {
            path: directory.to_path_buf(),
            source,
        })?;
        let path = directory.join(LOCK_FILE);
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|source| Error::Io {
                path: path.clone(),
                source,
            })?;
        let holder_path = directory.join(LOCK_HOLDER_FILE);
        match file.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => {
                let holder = read_holder(&holder_path);
                return Ok(LockOutcome::Held { path, holder });
            }
            Err(fs::TryLockError::Error(source)) => return Err(Error::Io { path, source }),
        }
        describe_holder(&holder_path)?;
        Ok(LockOutcome::Acquired(SettingsLock { path, file }))
    }

    /// The lock file this claim holds.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Releases the claim.
    pub fn release(self) {
        drop(self);
    }
}

impl Drop for SettingsLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// Records who holds the claim, replacing whatever the last holder wrote.
fn describe_holder(path: &Path) -> Result<()> {
    let body = serde_json::json!({
        "pid": std::process::id(),
        "acquiredEpochSeconds": now_seconds(),
    })
    .to_string();
    ca_io::write_atomic(path, body.as_bytes()).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn read_holder(path: &Path) -> LockHolder {
    use std::io::Read as _;
    const MAX_HOLDER_BYTES: u64 = 64 * 1024;
    let parsed = fs::File::open(path).ok().and_then(|file| {
        if file.metadata().ok()?.len() > MAX_HOLDER_BYTES {
            return None;
        }
        let mut bytes = Vec::new();
        file.take(MAX_HOLDER_BYTES + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() as u64 > MAX_HOLDER_BYTES {
            return None;
        }
        serde_json::from_slice::<Value>(&bytes).ok()
    });
    LockHolder {
        pid: parsed
            .as_ref()
            .and_then(|value| value.get("pid"))
            .and_then(Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok()),
        acquired_epoch_seconds: parsed
            .as_ref()
            .and_then(|value| value.get("acquiredEpochSeconds"))
            .and_then(Value::as_i64),
    }
}

/// Where settings are read from and written to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsDirectory {
    /// The per-user location for the running platform.
    PerUser(PathBuf),
    /// A folder beside the executable, shared by every user of the machine.
    Portable(PathBuf),
}

impl SettingsDirectory {
    /// The folder itself.
    #[must_use]
    pub fn path(&self) -> &Path {
        match self {
            SettingsDirectory::PerUser(path) | SettingsDirectory::Portable(path) => path,
        }
    }

    /// True when settings live beside the executable.
    #[must_use]
    pub fn is_portable(&self) -> bool {
        matches!(self, SettingsDirectory::Portable(_))
    }
}

/// Environment variable that names the settings directory.
///
/// When it holds a non-empty value, that directory replaces both the per-user
/// location and the folder beside the executable.
pub const SETTINGS_DIRECTORY_VARIABLE: &str = "COMPARE_ALL_SETTINGS_DIR";

/// The directory the environment names, if it names one.
fn named_settings_directory() -> Option<PathBuf> {
    let value = std::env::var_os(SETTINGS_DIRECTORY_VARIABLE)?;
    if value.is_empty() {
        return None;
    }
    Some(PathBuf::from(value))
}

/// Name of the application's folder inside a per-user folder.
const APPLICATION_FOLDER: &str = "compare-all";

/// The operating system conventions that name the per-user folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformFamily {
    /// `%APPDATA%`.
    Windows,
    /// `~/Library/Application Support`.
    MacOs,
    /// The XDG Base Directory layout.
    OtherUnix,
}

impl PlatformFamily {
    /// The family of the running build.
    #[must_use]
    pub fn running() -> Self {
        if cfg!(windows) {
            PlatformFamily::Windows
        } else if cfg!(target_os = "macos") {
            PlatformFamily::MacOs
        } else {
            PlatformFamily::OtherUnix
        }
    }

    /// True when `value` is an absolute path under this family's rules.
    ///
    /// The text decides, not the host: a Windows host judges Unix values the
    /// way a Unix host would, so the choice is testable everywhere.
    fn is_absolute(self, value: &std::ffi::OsStr) -> bool {
        let bytes = value.as_encoded_bytes();
        match self {
            PlatformFamily::Windows => {
                matches!(bytes, [letter, b':', b'\\' | b'/', ..] if letter.is_ascii_alphabetic())
                    || bytes.starts_with(br"\\")
            }
            PlatformFamily::MacOs | PlatformFamily::OtherUnix => bytes.first() == Some(&b'/'),
        }
    }
}

/// The environment values the per-user folder is named from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformEnvironment {
    /// Whose conventions apply.
    pub family: PlatformFamily,
    /// `HOME`.
    pub home: Option<std::ffi::OsString>,
    /// `XDG_CONFIG_HOME`.
    pub config_home: Option<std::ffi::OsString>,
    /// `APPDATA`.
    pub app_data: Option<std::ffi::OsString>,
    /// `XDG_RUNTIME_DIR`.
    pub runtime_directory: Option<std::ffi::OsString>,
}

impl PlatformEnvironment {
    /// The values of the running process.
    #[must_use]
    pub fn from_process() -> Self {
        Self {
            family: PlatformFamily::running(),
            home: std::env::var_os("HOME"),
            config_home: std::env::var_os("XDG_CONFIG_HOME"),
            app_data: std::env::var_os("APPDATA"),
            runtime_directory: std::env::var_os("XDG_RUNTIME_DIR"),
        }
    }

    /// A variable's value when it names an absolute path. An empty or
    /// relative value counts as not set.
    fn absolute(&self, value: Option<&std::ffi::OsString>) -> Option<PathBuf> {
        value
            .filter(|value| self.family.is_absolute(value))
            .map(PathBuf::from)
    }
}

/// The per-user folder, or why the environment names none.
fn per_user_folder(
    environment: &PlatformEnvironment,
) -> std::result::Result<PathBuf, &'static str> {
    const NO_HOME: &str = "HOME is not set to an absolute folder";
    match environment.family {
        PlatformFamily::Windows => environment
            .absolute(environment.app_data.as_ref())
            .map(|roaming| roaming.join(APPLICATION_FOLDER))
            .ok_or("APPDATA is not set to an absolute folder"),
        PlatformFamily::MacOs => environment
            .absolute(environment.home.as_ref())
            .map(|home| {
                home.join("Library")
                    .join("Application Support")
                    .join(APPLICATION_FOLDER)
            })
            .ok_or(NO_HOME),
        PlatformFamily::OtherUnix => environment
            .absolute(environment.config_home.as_ref())
            .or_else(|| {
                environment
                    .absolute(environment.home.as_ref())
                    .map(|home| home.join(".config"))
            })
            .map(|config| config.join(APPLICATION_FOLDER))
            .ok_or(NO_HOME),
    }
}

/// The folder a run keeps its state in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateDirectory {
    /// The folder.
    pub path: PathBuf,
    /// What a person has to be told about it: set when state kept there does
    /// not last.
    pub notice: Option<String>,
}

/// The private folder made for this run, once one was needed. One per process,
/// so every caller in the process keeps its state in the same place.
static RUN_FOLDER: std::sync::OnceLock<std::result::Result<PathBuf, String>> =
    std::sync::OnceLock::new();

/// A folder for this run, used when the environment names no per-user
/// folder. `reason` says why it names none.
fn run_directory(environment: &PlatformEnvironment, reason: &str) -> StateDirectory {
    if let Some(path) = SettingsPaths::runtime_directory_in(environment) {
        let notice = format!(
            "No home folder is known: {reason}. Settings are kept in {} only until you log out.",
            path.display()
        );
        return StateDirectory {
            path,
            notice: Some(notice),
        };
    }
    let created = RUN_FOLDER.get_or_init(|| {
        let parent = temporary_parent(environment.family)?;
        ca_io::private::create_private_folder_in(&parent, "compare-all-")
            .map_err(|error| format!("{}: {error}", parent.display()))
    });
    match created {
        Ok(path) => StateDirectory {
            path: path.clone(),
            notice: Some(format!(
                "No home folder is known: {reason}. Settings are not kept after the program closes. This run keeps them in {}.",
                path.display()
            )),
        },
        Err(detail) => StateDirectory {
            path: unusable_directory(),
            notice: Some(unsaved_notice(reason, detail)),
        },
    }
}

/// The one line shown when a run has nowhere private to write. `detail` names
/// the temporary folder and why no folder could be made in it.
fn unsaved_notice(reason: &str, detail: &str) -> String {
    format!(
        "Settings are not saved, because no home folder is known ({reason}) and no private folder could be made in {detail}."
    )
}

/// The folder the private folder of a run is made in.
///
/// A relative temporary folder would resolve against the working directory,
/// so it is replaced by `/tmp` on Unix and refused elsewhere.
fn temporary_parent(family: PlatformFamily) -> std::result::Result<PathBuf, String> {
    let temporary = std::env::temp_dir();
    if family.is_absolute(temporary.as_os_str()) {
        return Ok(temporary);
    }
    match family {
        PlatformFamily::Windows => Err(format!(
            "{}: the temporary folder is not absolute",
            temporary.display()
        )),
        PlatformFamily::MacOs | PlatformFamily::OtherUnix => Ok(PathBuf::from("/tmp")),
    }
}

/// A folder no file system call accepts, so a run that has nowhere private
/// to write writes nothing: the standard library refuses every path that
/// holds a NUL byte before it reaches the operating system.
fn unusable_directory() -> PathBuf {
    let root = if cfg!(windows) { r"C:\" } else { "/" };
    PathBuf::from(root).join("compare-all\0")
}

/// Resolved locations of the stored documents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsPaths {
    directory: SettingsDirectory,
}

impl SettingsPaths {
    /// Uses a directory chosen by the caller. Tests inject their own.
    #[must_use]
    pub fn at(directory: SettingsDirectory) -> Self {
        Self { directory }
    }

    /// Chooses between the folder beside the executable and the per-user
    /// folder.
    ///
    /// Portable mode is selected by the presence of the program state document
    /// beside the executable, so copying the documents next to the executable
    /// is all that switches a machine over.
    /// A directory named by [`SETTINGS_DIRECTORY_VARIABLE`] overrides both, so
    /// a caller that resolves its own paths still cannot reach the per-user
    /// folder while the variable is set.
    #[must_use]
    pub fn resolve(executable_directory: &Path, per_user: PathBuf) -> Self {
        Self::resolve_with(Some(executable_directory), || per_user)
    }

    /// Chooses as [`SettingsPaths::resolve`] does, asking for the per-user
    /// folder only when neither the environment nor the executable's folder
    /// decides.
    #[must_use]
    pub fn resolve_with<F>(executable_directory: Option<&Path>, per_user: F) -> Self
    where
        F: FnOnce() -> PathBuf,
    {
        if let Some(named) = named_settings_directory() {
            return Self {
                directory: SettingsDirectory::PerUser(named),
            };
        }
        if let Some(directory) = executable_directory {
            if directory.join(PROGRAM_STATE_FILE).exists() {
                return Self {
                    directory: SettingsDirectory::Portable(directory.to_path_buf()),
                };
            }
        }
        Self {
            directory: SettingsDirectory::PerUser(per_user()),
        }
    }

    /// The folder a run keeps its state in: the per-user folder, or a folder
    /// for this run when the environment names none.
    ///
    /// The fallback is `$XDG_RUNTIME_DIR/compare-all` when that variable names
    /// an absolute folder private to the running user, and otherwise a folder with an
    /// unpredictable name in the temporary folder, made once per process and
    /// open only to the running user. Either way the result carries the
    /// notice a person has to see.
    #[must_use]
    pub fn state_directory() -> StateDirectory {
        if let Some(named) = named_settings_directory() {
            return StateDirectory {
                path: named,
                notice: None,
            };
        }
        let environment = PlatformEnvironment::from_process();
        match per_user_folder(&environment) {
            Ok(path) => StateDirectory { path, notice: None },
            Err(reason) => run_directory(&environment, reason),
        }
    }

    /// True when `path` is the folder a run uses when it has nowhere private
    /// to write, which no file system call accepts.
    #[must_use]
    pub fn is_unusable(path: &Path) -> bool {
        path == unusable_directory()
    }

    /// Delete the private folder [`SettingsPaths::state_directory`] made for
    /// this run, if it made one. For the end of the process: a later call to
    /// [`SettingsPaths::state_directory`] still names the deleted folder.
    pub fn remove_run_directory() {
        if let Some(Ok(path)) = RUN_FOLDER.get() {
            let _ = fs::remove_dir_all(path);
        }
    }

    /// `$XDG_RUNTIME_DIR/compare-all` when the variable names an absolute
    /// folder the running user owns and no one else can open, as the XDG
    /// Base Directory rule requires of that folder.
    ///
    /// The application folder inside it is created or accepted only as a
    /// folder of the running user, never a link, with access narrowed to that
    /// user. A runtime folder that is open to others, such as the shared
    /// temporary folder, can hold an application folder another user made in
    /// advance, so it is refused before anything inside it is looked at.
    #[must_use]
    pub fn runtime_directory_in(environment: &PlatformEnvironment) -> Option<PathBuf> {
        if environment.family == PlatformFamily::Windows {
            return None;
        }
        let runtime = environment.absolute(environment.runtime_directory.as_ref())?;
        ca_io::private::check_private_folder(&runtime).ok()?;
        let folder = runtime.join(APPLICATION_FOLDER);
        ca_io::private::create_owned_folder(&folder).ok()?;
        Some(folder)
    }

    /// Chooses between the two directories without reading the environment.
    #[must_use]
    pub fn choose(executable_directory: &Path, per_user: PathBuf) -> Self {
        if executable_directory.join(PROGRAM_STATE_FILE).exists() {
            return Self {
                directory: SettingsDirectory::Portable(executable_directory.to_path_buf()),
            };
        }
        Self {
            directory: SettingsDirectory::PerUser(per_user),
        }
    }

    /// The per-user settings folder for the running platform.
    ///
    /// [`SETTINGS_DIRECTORY_VARIABLE`] replaces the platform folder when it
    /// holds a non-empty value. An unset variable leaves the result unchanged.
    ///
    /// # Errors
    /// Returns [`Error::NoSettingsDirectory`] when the environment names no
    /// home or configuration folder.
    pub fn platform_per_user_directory() -> Result<PathBuf> {
        if let Some(named) = named_settings_directory() {
            return Ok(named);
        }
        Self::platform_directory()
    }

    /// The per-user settings folder for the running platform, with the
    /// environment override ignored.
    ///
    /// A caller compares against this to find out whether a resolved directory
    /// is the real one.
    ///
    /// # Errors
    /// Returns [`Error::NoSettingsDirectory`] when the environment names no
    /// home or configuration folder.
    pub fn platform_directory() -> Result<PathBuf> {
        Self::per_user_directory_in(&PlatformEnvironment::from_process())
    }

    /// The per-user settings folder `environment` names.
    ///
    /// # Errors
    /// Returns [`Error::NoSettingsDirectory`] when the environment names no
    /// home or configuration folder.
    pub fn per_user_directory_in(environment: &PlatformEnvironment) -> Result<PathBuf> {
        per_user_folder(environment).map_err(Error::NoSettingsDirectory)
    }

    /// The chosen directory.
    #[must_use]
    pub fn directory(&self) -> &SettingsDirectory {
        &self.directory
    }

    /// Path of a document inside the settings folder.
    #[must_use]
    pub fn file(&self, name: &str) -> PathBuf {
        self.directory.path().join(name)
    }

    /// Path of the sessions document.
    #[must_use]
    pub fn sessions_file(&self) -> PathBuf {
        self.file(SESSIONS_FILE)
    }

    /// Path of the application options document.
    #[must_use]
    pub fn options_file(&self) -> PathBuf {
        self.file(crate::options::OPTIONS_FILE)
    }

    /// Path of the program state document.
    #[must_use]
    pub fn program_state_file(&self) -> PathBuf {
        self.file(PROGRAM_STATE_FILE)
    }

    /// Path of the lock file claiming the settings folder.
    #[must_use]
    pub fn lock_file(&self) -> PathBuf {
        self.file(LOCK_FILE)
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// A save that meets a replaced document combines the two rather than
    /// refusing or overwriting.
    #[test]
    fn a_merging_save_keeps_what_the_other_writer_stored() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(SESSIONS_FILE);
        let mut first = SessionStore::default();
        first.save_replacing(&path).unwrap();
        let mut second = SessionStore::load(&path).unwrap().store;

        let id = first.next_id();
        first
            .add_session(
                None,
                SavedSession::new(id, "theirs", SessionKind::HexCompare),
            )
            .unwrap();
        first.save(&path).unwrap();

        let mine = second.next_id();
        second
            .add_session(
                None,
                SavedSession::new(mine, "mine", SessionKind::TextCompare),
            )
            .unwrap();
        let outcome = second.save_merging(&path).unwrap();
        assert!(outcome.merged);
        assert_eq!(outcome.nodes_kept, 1);

        let written = SessionStore::load(&path).unwrap().store;
        let mut names: Vec<&str> = written
            .sessions()
            .iter()
            .map(|session| session.name.as_str())
            .collect();
        names.sort_unstable();
        assert_eq!(names, vec!["mine", "theirs"]);
    }

    #[test]
    fn a_merging_save_with_no_other_writer_reports_no_merge() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(SESSIONS_FILE);
        let mut store = SessionStore::default();
        let outcome = store.save_merging(&path).unwrap();
        assert_eq!(outcome, SaveOutcome::default());
    }

    #[test]
    fn the_workspace_reopened_at_start_survives_a_round_trip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(SESSIONS_FILE);
        let mut store = SessionStore::default();
        store.restore_last_workspace = true;
        store.last_workspace = Some("Daily".to_owned());
        store.save_replacing(&path).unwrap();

        let back = SessionStore::load(&path).unwrap().store;
        assert!(back.restore_last_workspace);
        assert_eq!(back.last_workspace.as_deref(), Some("Daily"));
    }

    fn temporary_files(directory: &Path) -> Vec<PathBuf> {
        fs::read_dir(directory)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(".compare-all-"))
            })
            .collect()
    }

    fn store_with_two_sessions() -> (SessionStore, SessionId, SessionId) {
        let mut store = SessionStore::default();
        let id_a = store.next_id();
        let id_b = store.next_id();
        store
            .add_session(
                None,
                SavedSession::new(id_a.clone(), "Nightly", SessionKind::FolderCompare),
            )
            .unwrap();
        store
            .add_session(
                None,
                SavedSession::new(id_b.clone(), "Release", SessionKind::TextCompare),
            )
            .unwrap();
        (store, id_a, id_b)
    }

    #[test]
    fn folders_group_sessions_and_names_stay_unique() {
        let mut store = SessionStore::default();
        let folder = store.create_folder(None, "Team").unwrap();
        let id = store.next_id();
        store
            .add_session(
                Some(&folder),
                SavedSession::new(id.clone(), "Nightly", SessionKind::FolderSync),
            )
            .unwrap();

        assert_eq!(store.sessions().len(), 1);
        assert_eq!(store.find_session(&id).unwrap().name, "Nightly");
        assert!(store.create_folder(None, "Team").is_err());

        let clash = store.next_id();
        assert!(store
            .add_session(
                Some(&folder),
                SavedSession::new(clash, "Nightly", SessionKind::FolderSync)
            )
            .is_err());
    }

    #[test]
    fn sibling_names_collide_regardless_of_capitalization() {
        let mut store = SessionStore::default();
        store.create_folder(None, "Team").unwrap();
        assert!(
            store.create_folder(None, "TEAM").is_err(),
            "a tree keeps its shape on a machine that folds case"
        );

        let id = store.next_id();
        assert!(store
            .add_session(
                None,
                SavedSession::new(id, "team", SessionKind::TextCompare)
            )
            .is_err());
    }

    #[test]
    fn a_duplicate_name_is_free_only_when_no_sibling_folds_to_it() {
        let taken = vec!["Nightly".to_owned(), "NIGHTLY (2)".to_owned()];
        assert_eq!(free_name("Nightly", &taken), "Nightly (3)");
        assert_eq!(free_name("Other", &taken), "Other");
    }

    #[test]
    fn rename_move_delete_and_duplicate_work_on_the_tree() {
        let (mut store, id_a, id_b) = store_with_two_sessions();
        let folder = store.create_folder(None, "Archive").unwrap();

        store.rename(&id_a, "Nightly build").unwrap();
        assert_eq!(store.find_session(&id_a).unwrap().name, "Nightly build");

        store.move_node(&id_a, Some(&folder)).unwrap();
        assert_eq!(store.find(&folder).unwrap().children().len(), 1);

        let copy = store.duplicate(&id_b).unwrap();
        assert_ne!(copy, id_b);
        assert_eq!(store.find_session(&copy).unwrap().name, "Release (2)");

        store.delete(&id_b).unwrap();
        assert!(store.find(&id_b).is_none());
        assert!(store.find(&copy).is_some());
    }

    #[test]
    fn a_folder_cannot_be_moved_into_itself() {
        let mut store = SessionStore::default();
        let outer = store.create_folder(None, "Outer").unwrap();
        let inner = store.create_folder(Some(&outer), "Inner").unwrap();
        assert!(store.move_node(&outer, Some(&inner)).is_err());
        assert!(store.find(&inner).is_some());
    }

    #[test]
    fn a_move_that_collides_leaves_the_tree_untouched() {
        let mut store = SessionStore::default();
        let folder = store.create_folder(None, "Archive").unwrap();
        let held = store.next_id();
        store
            .add_session(
                Some(&folder),
                SavedSession::new(held, "Nightly", SessionKind::TextCompare),
            )
            .unwrap();
        let moving = store.next_id();
        store
            .add_session(
                None,
                SavedSession::new(moving.clone(), "Nightly", SessionKind::TextCompare),
            )
            .unwrap();

        assert!(matches!(
            store.move_node(&moving, Some(&folder)),
            Err(Error::DuplicateName(_))
        ));
        assert_eq!(
            store.find(&folder).unwrap().children().len(),
            1,
            "the collision did not create a second holder of the name"
        );
        assert_eq!(store.root.len(), 2, "the moved node stayed where it was");
        assert!(store.parent_of(&moving).is_none());
    }

    #[test]
    fn a_move_into_a_session_leaves_the_tree_untouched() {
        let (mut store, id_a, id_b) = store_with_two_sessions();
        assert!(matches!(
            store.move_node(&id_a, Some(&id_b)),
            Err(Error::InvalidMove(_))
        ));
        assert_eq!(store.root.len(), 2, "the node was not dumped at the root");
        assert!(store.parent_of(&id_a).is_none());
        assert_eq!(store.sessions().len(), 2);
    }

    #[test]
    fn a_move_to_an_unknown_parent_leaves_the_tree_untouched() {
        let (mut store, id_a, _) = store_with_two_sessions();
        assert!(matches!(
            store.move_node(&id_a, Some(&SessionId::from_raw("absent"))),
            Err(Error::NoSuchNode(_))
        ));
        assert_eq!(store.root.len(), 2);
    }

    #[test]
    fn a_move_within_the_same_parent_keeps_its_own_name() {
        let (mut store, id_a, _) = store_with_two_sessions();
        store.move_node(&id_a, None).unwrap();
        assert_eq!(store.sessions().len(), 2);
    }

    #[test]
    fn a_locked_session_refuses_edits() {
        let (mut store, id_a, _) = store_with_two_sessions();
        let folder = store.create_folder(None, "Archive").unwrap();
        store.set_locked(&id_a, true).unwrap();
        assert!(store.rename(&id_a, "Other").is_err());
        assert!(store.delete(&id_a).is_err());
        assert!(store
            .update_session_settings(
                &id_a,
                SessionSettingsOverride::empty_for(&SessionKind::FolderCompare)
            )
            .is_err());
        assert!(
            matches!(
                store.move_node(&id_a, Some(&folder)),
                Err(Error::ReadOnly(_))
            ),
            "where a session sits is part of what a lock protects"
        );
        assert!(store.parent_of(&id_a).is_none());

        store.set_locked(&id_a, false).unwrap();
        store.rename(&id_a, "Other").unwrap();
        store.move_node(&id_a, Some(&folder)).unwrap();
    }

    #[test]
    fn settings_of_another_kind_are_refused() {
        let (mut store, id_a, _) = store_with_two_sessions();
        assert!(store
            .update_session_settings(
                &id_a,
                SessionSettingsOverride::empty_for(&SessionKind::HexCompare)
            )
            .is_err());
    }

    #[test]
    fn auto_saved_sessions_are_bounded_newest_first() {
        let mut store = SessionStore::default();
        store.set_max_auto_saved(3);
        for index in 0..5 {
            let id = store.next_id();
            store.record_auto_saved(SavedSession::new(
                id,
                format!("session {index}"),
                SessionKind::TextCompare,
            ));
        }
        assert_eq!(store.auto_saved.len(), 3);
        assert_eq!(store.auto_saved[0].name, "session 4");
        assert_eq!(store.auto_saved[2].name, "session 2");
    }

    #[test]
    fn a_cap_of_zero_disables_automatic_saving() {
        let mut store = SessionStore::default();
        let id = store.next_id();
        store.record_auto_saved(SavedSession::new(id, "held", SessionKind::TextCompare));
        store.set_max_auto_saved(0);
        assert!(store.auto_saved.is_empty());

        let id = store.next_id();
        store.record_auto_saved(SavedSession::new(id, "ignored", SessionKind::TextCompare));
        assert!(store.auto_saved.is_empty());
    }

    #[test]
    fn re_recording_a_session_moves_it_to_the_front() {
        let mut store = SessionStore::default();
        let first = store.next_id();
        let second = store.next_id();
        store.record_auto_saved(SavedSession::new(
            first.clone(),
            "first",
            SessionKind::TextCompare,
        ));
        store.record_auto_saved(SavedSession::new(
            second,
            "second",
            SessionKind::TextCompare,
        ));
        store.record_auto_saved(SavedSession::new(
            first.clone(),
            "first",
            SessionKind::TextCompare,
        ));

        assert_eq!(store.auto_saved.len(), 2);
        assert_eq!(store.auto_saved[0].id, first);
    }

    #[test]
    fn an_explicit_timestamp_edit_replaces_unfamiliar_storage() {
        let raw = serde_json::json!({
            "id": "n1", "name": "future", "kind": "text-compare",
            "settings": {"kind": "text-compare"},
            "lastUsedEpochSeconds": {"futureClock": "precise"}
        });
        let mut session: SavedSession = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(session.last_used_epoch_seconds(), 0);
        session.name = "edited".into();
        assert_eq!(
            serde_json::to_value(&session).unwrap()["lastUsedEpochSeconds"],
            raw["lastUsedEpochSeconds"]
        );
        session.set_last_used_epoch_seconds(42);
        assert_eq!(session.last_used_epoch_seconds(), 42);
        assert_eq!(
            serde_json::to_value(&session).unwrap()["lastUsedEpochSeconds"],
            42
        );
    }

    #[test]
    fn promotion_with_settings_is_atomic_on_refusal() {
        let mut store = SessionStore::default();
        let id = store.next_id();
        store.record_auto_saved(SavedSession::new(
            id.clone(),
            "recent",
            SessionKind::TextCompare,
        ));
        let named = store.next_id();
        store
            .add_session(
                None,
                SavedSession::new(named, "taken", SessionKind::TextCompare),
            )
            .unwrap();
        let before = serde_json::to_value(&store).unwrap();
        let settings = store.auto_saved[0].settings.clone();
        assert!(store
            .promote_auto_saved_with_settings(&id, "taken", None, settings.clone())
            .is_err());
        assert_eq!(serde_json::to_value(&store).unwrap(), before);
        store
            .promote_auto_saved_with_settings(&id, "kept", None, settings.clone())
            .unwrap();
        assert!(store.auto_saved.is_empty());
        assert_eq!(store.find_session(&id).unwrap().settings, settings);
    }

    #[test]
    fn an_auto_saved_session_can_be_promoted_to_a_named_one() {
        let mut store = SessionStore::default();
        let id = store.next_id();
        store.record_auto_saved(SavedSession::new(
            id.clone(),
            "untitled",
            SessionKind::TextCompare,
        ));
        let named = store
            .promote_auto_saved(&id, "Release notes", None)
            .unwrap();
        assert!(store.auto_saved.is_empty());
        assert_eq!(store.find_session(&named).unwrap().name, "Release notes");
    }

    #[test]
    fn workspaces_are_saved_renamed_and_deleted_by_name() {
        let mut store = SessionStore::default();
        let id = store.next_id();
        store.save_workspace(Workspace {
            name: "Daily".to_owned(),
            windows: vec![WorkspaceWindow {
                bounds: Some(WindowBounds::new(0, 0, 1200, 800)),
                tabs: vec![WorkspaceTab::home(), WorkspaceTab::saved(id)],
                active_tab: 1,
                ..WorkspaceWindow::default()
            }],
            ..Workspace::default()
        });
        assert_eq!(store.workspace("Daily").unwrap().windows.len(), 1);

        store.rename_workspace("Daily", "Morning").unwrap();
        assert!(store.workspace("Daily").is_none());
        assert!(store.delete_workspace("Morning"));
        assert!(!store.delete_workspace("Morning"));
    }

    #[test]
    fn window_bounds_are_stored_by_name() {
        let bounds = WindowBounds {
            monitor: Some("DISPLAY1".to_owned()),
            scale: Some(1.5),
            ..WindowBounds::new(10, 20, 1200, 800)
        };
        let json = serde_json::to_string(&bounds).unwrap();
        assert!(json.contains(r#""width":1200"#), "{json}");
        assert_eq!(serde_json::from_str::<WindowBounds>(&json).unwrap(), bounds);

        let partial: WindowBounds = serde_json::from_str(r#"{"x":1,"y":2}"#).unwrap();
        assert_eq!(partial.width, 0);
        assert_eq!(partial.monitor, None);
    }

    #[test]
    fn a_store_round_trips_through_a_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(SESSIONS_FILE);
        let (mut store, id_a, _) = store_with_two_sessions();
        store
            .layers
            .update_session_defaults_from(
                &SessionKind::TextCompare,
                &crate::settings::SessionSettings::defaults_for(&SessionKind::TextCompare),
            )
            .unwrap();
        store.save(&path).unwrap();

        let outcome = SessionStore::load(&path).unwrap();
        assert_eq!(outcome.store, store);
        assert!(outcome.recovered_backup.is_none());
        assert!(outcome.repairs.is_clean());
        assert_eq!(outcome.store.find_session(&id_a).unwrap().name, "Nightly");
    }

    #[test]
    fn a_missing_document_loads_as_an_empty_store() {
        let dir = TempDir::new().unwrap();
        let outcome = SessionStore::load(&dir.path().join("absent.json")).unwrap();
        assert_eq!(outcome.store, SessionStore::default());
        assert!(outcome.recovered_backup.is_none());
    }

    #[test]
    fn unknown_fields_survive_a_load_and_save() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(SESSIONS_FILE);
        let document = r#"{
            "schemaVersion": 1,
            "nextId": 4,
            "root": [
                {
                    "node": "session",
                    "id": "n1",
                    "name": "Nightly",
                    "kind": "folder-compare",
                    "settings": { "kind": "folder-compare" },
                    "futureSessionField": [1, 2, 3]
                }
            ],
            "futureStoreField": { "retained": true }
        }"#;
        fs::write(&path, document).unwrap();

        let outcome = SessionStore::load(&path).unwrap();
        assert!(outcome.recovered_backup.is_none());
        let mut store = outcome.store;
        store.save(&path).unwrap();

        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["futureStoreField"]["retained"], Value::Bool(true));
        assert_eq!(
            written["root"][0]["futureSessionField"],
            serde_json::json!([1, 2, 3])
        );
    }

    #[test]
    fn an_unknown_kind_does_not_cost_the_known_sessions() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(SESSIONS_FILE);
        let document = r#"{
            "schemaVersion": 1,
            "nextId": 3,
            "root": [
                {
                    "node": "session",
                    "id": "n1",
                    "name": "Nightly",
                    "kind": "folder-compare",
                    "settings": { "kind": "folder-compare" }
                },
                {
                    "node": "session",
                    "id": "n2",
                    "name": "Chart",
                    "kind": "chart-compare",
                    "settings": { "kind": "chart-compare", "axes": { "x": "time" } }
                }
            ]
        }"#;
        fs::write(&path, document).unwrap();

        let outcome = SessionStore::load(&path).unwrap();
        assert!(
            outcome.recovered_backup.is_none(),
            "an unknown kind is not damage"
        );
        assert_eq!(outcome.store.sessions().len(), 2);
        let unknown = outcome
            .store
            .find_session(&SessionId::from_raw("n2"))
            .unwrap();
        assert_eq!(
            unknown.kind,
            SessionKind::Unknown("chart-compare".to_owned())
        );

        let mut store = outcome.store;
        store.save(&path).unwrap();
        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["root"][1]["kind"], Value::from("chart-compare"));
        assert_eq!(
            written["root"][1]["settings"]["axes"]["x"],
            Value::from("time")
        );
    }

    #[test]
    fn an_unknown_node_keeps_its_place_and_its_content() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(SESSIONS_FILE);
        let document = r#"{
            "schemaVersion": 1,
            "nextId": 3,
            "root": [
                { "node": "shortcut", "id": "n1", "name": "Pinned", "target": "elsewhere" },
                {
                    "node": "session",
                    "id": "n2",
                    "name": "Nightly",
                    "kind": "folder-compare",
                    "settings": { "kind": "folder-compare" }
                }
            ]
        }"#;
        fs::write(&path, document).unwrap();

        let outcome = SessionStore::load(&path).unwrap();
        assert!(outcome.recovered_backup.is_none());
        assert_eq!(outcome.store.root.len(), 2);
        assert!(matches!(outcome.store.root[0], TreeNode::Unknown(_)));
        assert_eq!(outcome.store.root[0].name(), "Pinned");

        let mut store = outcome.store;
        store.save(&path).unwrap();
        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            written["root"][0],
            serde_json::json!({
                "node": "shortcut", "id": "n1", "name": "Pinned", "target": "elsewhere"
            }),
            "an unknown node keeps its position and every field"
        );
    }

    #[test]
    fn an_unknown_session_settings_tag_keeps_the_rest_of_the_document() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(SESSIONS_FILE);
        fs::write(
            &path,
            r#"{
                "schemaVersion": 1,
                "layers": {
                    "sessionDefaults": {
                        "chart-compare": { "kind": "chart-compare", "axes": 2 },
                        "text-compare": { "kind": "text-compare" }
                    }
                }
            }"#,
        )
        .unwrap();

        let outcome = SessionStore::load(&path).unwrap();
        assert!(outcome.recovered_backup.is_none());
        assert!(outcome
            .store
            .layers
            .session_defaults(&SessionKind::TextCompare)
            .is_some());
        let mut store = outcome.store;
        store.save(&path).unwrap();
        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            written["layers"]["sessionDefaults"]["chart-compare"]["axes"],
            Value::from(2)
        );
    }

    #[test]
    fn a_document_from_a_newer_schema_loads_and_keeps_its_version() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(SESSIONS_FILE);
        fs::write(
            &path,
            r#"{"schemaVersion": 9999, "root": [], "futureTopLevel": 7}"#,
        )
        .unwrap();

        let outcome = SessionStore::load(&path).unwrap();
        assert_eq!(outcome.newer_schema, Some(9999));
        assert!(outcome.store.is_newer_schema());
        assert!(outcome.recovered_backup.is_none());

        let mut store = outcome.store;
        store.save(&path).unwrap();
        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            written["schemaVersion"],
            Value::from(9999),
            "a save never lowers the version a newer build wrote"
        );
        assert_eq!(written["futureTopLevel"], Value::from(7));
    }

    #[test]
    fn a_document_without_a_version_is_migrated_from_zero() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(SESSIONS_FILE);
        fs::write(&path, r#"{"root": []}"#).unwrap();

        let outcome = SessionStore::load(&path).unwrap();
        assert_eq!(
            outcome.migrated_from,
            Some(0),
            "a document predating the version field is not assumed current"
        );
        assert_eq!(outcome.store.schema_version, SCHEMA_VERSION);
    }

    #[test]
    fn a_version_number_beyond_the_version_range_is_a_newer_schema() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(SESSIONS_FILE);
        let document = serde_json::json!({"schemaVersion": 4_294_967_296_u64, "root": []});
        fs::write(&path, document.to_string()).unwrap();
        let mut outcome = SessionStore::load(&path).unwrap();
        assert!(outcome.newer_schema.is_some());
        assert!(outcome.store.is_newer_schema());
        outcome.store.save(&path).unwrap();
        let written: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(written["schemaVersion"], document["schemaVersion"]);
    }

    #[test]
    fn an_unrecognized_schema_version_is_preserved_when_known_fields_are_saved() {
        for version in [
            serde_json::json!("future"),
            serde_json::json!(-1),
            serde_json::json!(1.5),
            serde_json::json!(4_294_967_296_u64),
            serde_json::json!(null),
            serde_json::json!({"major": 2}),
        ] {
            let dir = TempDir::new().unwrap();
            let path = dir.path().join(SESSIONS_FILE);
            let document =
                serde_json::json!({"schemaVersion": version, "root": [], "maxAutoSaved": 7});
            fs::write(&path, document.to_string()).unwrap();
            let mut outcome = SessionStore::load(&path).unwrap();
            assert!(outcome.recovered_backup.is_none());
            outcome.store.max_auto_saved = 11;
            outcome.store.save(&path).unwrap();
            let written: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            assert_eq!(written["schemaVersion"], document["schemaVersion"]);
            assert_eq!(written["maxAutoSaved"], 11);
            let outcome = SessionStore::load(&path).unwrap();
            assert!(outcome.recovered_backup.is_none());
            assert_eq!(outcome.store.max_auto_saved, 11);
        }
    }

    #[test]
    fn a_corrupted_document_is_kept_as_a_backup() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(SESSIONS_FILE);
        fs::write(&path, "{ this is not json").unwrap();

        let outcome = SessionStore::load(&path).unwrap();
        let backup = outcome.recovered_backup.unwrap();
        assert_eq!(outcome.store, SessionStore::default());
        assert!(!path.exists());
        assert_eq!(fs::read_to_string(&backup).unwrap(), "{ this is not json");
    }

    #[test]
    fn a_document_that_is_not_utf8_is_kept_as_a_backup() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(SESSIONS_FILE);
        let damaged = b"{\"schemaVersion\": 1, \"root\": [\xff\xfe]}".to_vec();
        fs::write(&path, &damaged).unwrap();

        let outcome = SessionStore::load(&path).unwrap();
        let backup = outcome.recovered_backup.unwrap();
        assert!(!path.exists());
        assert_eq!(fs::read(&backup).unwrap(), damaged);
    }

    #[test]
    fn a_second_corruption_keeps_both_backups() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(SESSIONS_FILE);
        fs::write(&path, "first damage").unwrap();
        let first = SessionStore::load(&path).unwrap().recovered_backup.unwrap();
        fs::write(&path, "second damage").unwrap();
        let second = SessionStore::load(&path).unwrap().recovered_backup.unwrap();

        assert_ne!(first, second);
        assert_eq!(fs::read_to_string(&first).unwrap(), "first damage");
        assert_eq!(fs::read_to_string(&second).unwrap(), "second damage");
    }

    #[test]
    fn recent_identifiers_are_repaired_against_tree_and_each_other() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(SESSIONS_FILE);
        let mut store = SessionStore::default();
        let id = store.next_id();
        let session = SavedSession::new(id.clone(), "named", SessionKind::TextCompare);
        store.add_session(None, session.clone()).unwrap();
        store.auto_saved = vec![session.clone(), session];
        store.auto_saved[0]
            .unknown
            .insert("future".into(), serde_json::json!({"value": 7}));
        store.save(&path).unwrap();
        let mut loaded = SessionStore::load(&path).unwrap();
        assert_eq!(loaded.repairs.duplicate_ids, 2);
        assert_eq!(loaded.store.find_session(&id).unwrap().name, "named");
        let ids = loaded.store.collect_ids();
        assert_eq!(ids.iter().collect::<BTreeSet<_>>().len(), 3);
        assert_eq!(
            loaded.store.auto_saved[0].unknown["future"],
            serde_json::json!({"value": 7})
        );
        loaded.store.save(&path).unwrap();
        let reloaded = SessionStore::load(&path).unwrap();
        assert_eq!(reloaded.repairs.duplicate_ids, 0);
        assert_eq!(reloaded.store.collect_ids(), ids);
    }

    #[test]
    fn duplicate_identifiers_are_repaired_on_load() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(SESSIONS_FILE);
        fs::write(
            &path,
            r#"{
                "schemaVersion": 1,
                "nextId": 1,
                "root": [
                    {
                        "node": "session", "id": "n1", "name": "First",
                        "kind": "text-compare", "settings": {"kind": "text-compare"}
                    },
                    {
                        "node": "session", "id": "n1", "name": "Second",
                        "kind": "text-compare", "settings": {"kind": "text-compare"}
                    }
                ]
            }"#,
        )
        .unwrap();

        let outcome = SessionStore::load(&path).unwrap();
        assert_eq!(outcome.repairs.duplicate_ids, 1);
        let ids: Vec<&str> = outcome
            .store
            .sessions()
            .iter()
            .map(|session| session.id.as_str())
            .collect();
        assert_ne!(ids[0], ids[1], "no two nodes answer to one identifier");

        let mut store = outcome.store;
        let removed = store.delete(&SessionId::from_raw("n1")).unwrap();
        assert_eq!(removed.name(), "First");
    }

    #[test]
    fn the_generator_never_reissues_a_stored_identifier() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(SESSIONS_FILE);
        fs::write(
            &path,
            r#"{
                "schemaVersion": 1,
                "root": [
                    {
                        "node": "session", "id": "n7", "name": "Held",
                        "kind": "text-compare", "settings": {"kind": "text-compare"}
                    }
                ]
            }"#,
        )
        .unwrap();

        let mut store = SessionStore::load(&path).unwrap().store;
        let fresh = store.next_id();
        assert_ne!(
            fresh,
            SessionId::from_raw("n7"),
            "a document without nextId does not restart the counter at zero"
        );
        assert!(store.find(&fresh).is_none());
    }

    #[test]
    fn a_session_kind_disagreeing_with_its_settings_is_repaired() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(SESSIONS_FILE);
        fs::write(
            &path,
            r#"{
                "schemaVersion": 1,
                "root": [
                    {
                        "node": "session", "id": "n1", "name": "Confused",
                        "kind": "hex-compare", "settings": {"kind": "text-compare"}
                    }
                ]
            }"#,
        )
        .unwrap();

        let outcome = SessionStore::load(&path).unwrap();
        assert_eq!(outcome.repairs.kinds_corrected, 1);
        let session = outcome
            .store
            .find_session(&SessionId::from_raw("n1"))
            .unwrap();
        assert_eq!(session.kind, SessionKind::TextCompare);
        session.resolve(&outcome.store.layers).unwrap();
    }

    #[test]
    fn the_time_a_session_was_last_used_carries_its_unit() {
        let session = SavedSession::new(
            SessionId::from_raw("n1"),
            "Nightly",
            SessionKind::TextCompare,
        );
        let json = serde_json::to_string(&session).unwrap();
        assert!(json.contains("lastUsedEpochSeconds"), "{json}");

        let older: SavedSession = serde_json::from_str(
            r#"{"id":"n1","name":"Nightly","kind":"text-compare",
                "settings":{"kind":"text-compare"},"lastUsed":42}"#,
        )
        .unwrap();
        assert_eq!(
            older.last_used_epoch_seconds, 42,
            "the earlier spelling still loads"
        );
    }

    #[test]
    fn a_document_replaced_since_the_load_is_not_overwritten() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(SESSIONS_FILE);
        let (mut first, _, _) = store_with_two_sessions();
        first.save(&path).unwrap();

        let mut mine = SessionStore::load(&path).unwrap().store;
        let mut theirs = SessionStore::load(&path).unwrap().store;
        theirs.create_folder(None, "Theirs").unwrap();
        theirs.save(&path).unwrap();

        mine.create_folder(None, "Mine").unwrap();
        assert!(
            matches!(mine.save(&path), Err(Error::DocumentChanged { .. })),
            "the other writer's document is reported, not replaced"
        );
        let on_disk = SessionStore::load(&path).unwrap().store;
        assert!(on_disk.root.iter().any(|node| node.name() == "Theirs"));

        mine.save_replacing(&path).unwrap();
        let on_disk = SessionStore::load(&path).unwrap().store;
        assert!(on_disk.root.iter().any(|node| node.name() == "Mine"));
    }

    #[test]
    fn saving_twice_in_a_row_is_not_a_conflict() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(SESSIONS_FILE);
        let (mut store, _, _) = store_with_two_sessions();
        store.save(&path).unwrap();
        store.create_folder(None, "Later").unwrap();
        store.save(&path).unwrap();
    }

    #[test]
    fn a_failed_write_leaves_the_previous_document_intact() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(SESSIONS_FILE);
        fs::write(&path, "previous contents").unwrap();

        let result = atomic_write(&path, |buffer| {
            buffer.extend_from_slice(b"partial");
            Err(Error::ReadOnly("serialization failed"))
        });

        assert!(result.is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "previous contents");
        assert!(
            temporary_files(dir.path()).is_empty(),
            "the temporary file is removed"
        );
    }

    #[test]
    fn concurrent_writers_do_not_share_a_temporary_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(SESSIONS_FILE);
        let barrier = std::sync::Barrier::new(4);
        let contents: Vec<_> = (0..4).map(|i| i.to_string().repeat(65_536)).collect();
        std::thread::scope(|scope| {
            for content in &contents {
                let barrier = &barrier;
                let path = &path;
                scope.spawn(move || {
                    barrier.wait();
                    atomic_write(path, |buffer| {
                        buffer.extend_from_slice(content.as_bytes());
                        Ok(())
                    })
                    .unwrap();
                });
            }
        });
        assert!(contents.contains(&fs::read_to_string(&path).unwrap()));
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn a_windows_sharing_violation_preserves_the_previous_document() {
        use std::os::windows::fs::OpenOptionsExt;

        let dir = TempDir::new().unwrap();
        let path = dir.path().join(SESSIONS_FILE);
        fs::write(&path, b"previous contents").unwrap();
        let _held = fs::OpenOptions::new()
            .read(true)
            .share_mode(3)
            .open(&path)
            .unwrap();
        let result = atomic_write(&path, |buffer| {
            buffer.extend_from_slice(b"replacement");
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(fs::read(&path).unwrap(), b"previous contents");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn a_successful_write_replaces_the_previous_document() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(SESSIONS_FILE);
        fs::write(&path, "previous contents").unwrap();

        atomic_write(&path, |buffer| {
            buffer.extend_from_slice(b"new contents");
            Ok(())
        })
        .unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "new contents");
    }

    #[test]
    fn a_second_instance_is_told_who_holds_the_directory() {
        let dir = TempDir::new().unwrap();
        let LockOutcome::Acquired(first) = SettingsLock::acquire(dir.path()).unwrap() else {
            panic!("the first claim should succeed");
        };
        assert!(first.path().exists());

        let LockOutcome::Held { path, holder } = SettingsLock::acquire(dir.path()).unwrap() else {
            panic!("the second claim should be refused");
        };
        assert_eq!(path, first.path());
        assert_eq!(holder.pid, Some(std::process::id()));

        first.release();
        assert!(matches!(
            SettingsLock::acquire(dir.path()).unwrap(),
            LockOutcome::Acquired(_)
        ));
    }

    /// A process that crashed leaves its lock file on disk. The file alone
    /// claims nothing, so the next process takes the directory.
    #[test]
    fn a_lock_file_left_behind_by_a_dead_process_claims_nothing() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(LOCK_FILE),
            br#"{"pid":1,"acquiredEpochSeconds":1}"#,
        )
        .unwrap();
        assert!(matches!(
            SettingsLock::acquire(dir.path()).unwrap(),
            LockOutcome::Acquired(_)
        ));
    }

    /// The holder is described for the message a second instance shows, and the
    /// previous holder's text is replaced rather than appended to.
    #[test]
    fn the_holder_file_names_the_current_holder() {
        let dir = TempDir::new().unwrap();
        let holder_path = dir.path().join(LOCK_HOLDER_FILE);
        fs::write(&holder_path, vec![b'x'; 4096]).unwrap();
        let LockOutcome::Acquired(_lock) = SettingsLock::acquire(dir.path()).unwrap() else {
            panic!("the claim should succeed");
        };
        let text = fs::read_to_string(&holder_path).unwrap();
        let holder = read_holder(&holder_path);
        assert_eq!(holder.pid, Some(std::process::id()));
        assert!(holder.acquired_epoch_seconds.is_some());
        assert!(!text.contains('x'), "{text}");
    }

    /// An oversized diagnostic is ignored without altering it. The exact
    /// boundary remains readable, so the limit is not an off-by-one refusal.
    #[test]
    fn holder_diagnostics_are_bounded_and_left_untouched() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(LOCK_HOLDER_FILE);
        let mut note = br#"{"pid":42,"acquiredEpochSeconds":7}"#.to_vec();
        note.resize(64 * 1024, b' ');
        fs::write(&path, &note).unwrap();
        assert_eq!(read_holder(&path).pid, Some(42));
        note.push(b' ');
        fs::write(&path, &note).unwrap();
        let ignored = read_holder(&path);
        assert_eq!(ignored.pid, None);
        assert_eq!(ignored.acquired_epoch_seconds, None);
        assert_eq!(fs::read(&path).unwrap(), note);
    }

    /// Future timestamp spellings follow their session when its position or
    /// branch changes. A destination-name refusal changes neither record.
    #[test]
    fn recent_refresh_and_promotion_preserve_unknown_timestamp_values() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(SESSIONS_FILE);
        let mut store = SessionStore::default();
        let id = store.next_id();
        store.record_auto_saved(SavedSession::new(
            id.clone(),
            "recent",
            SessionKind::TextCompare,
        ));
        let mut document = serde_json::to_value(&store).unwrap();
        let future = serde_json::json!({"futureClock": "raw"});
        document["autoSaved"][0]["lastUsedEpochSeconds"] = future.clone();
        fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        let mut store = SessionStore::load(&path).unwrap().store;
        let recent = store.auto_saved[0].clone();
        let other = store.next_id();
        store.record_auto_saved(SavedSession::new(other, "other", SessionKind::TextCompare));
        store.record_auto_saved(recent);
        store.save_replacing(&path).unwrap();
        let refreshed: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(refreshed["autoSaved"][0]["lastUsedEpochSeconds"], future);
        let parent = store.create_folder(None, "Kept").unwrap();
        let named = store
            .promote_auto_saved(&id, "named", Some(&parent))
            .unwrap();
        assert_eq!(named, id);
        store.save_replacing(&path).unwrap();
        let promoted: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            promoted["root"][0]["children"][0]["lastUsedEpochSeconds"],
            future
        );
        let mut reloaded = SessionStore::load(&path).unwrap().store;
        assert!(
            reloaded.find_session(&id).is_some(),
            "a preserved future timestamp hid the named session"
        );
        reloaded
            .update_session_settings(
                &id,
                SessionSettingsOverride::empty_for(&SessionKind::TextCompare),
            )
            .unwrap();
        reloaded.move_node(&id, None).unwrap();
        let copy = reloaded.duplicate(&id).unwrap();
        reloaded.save_replacing(&path).unwrap();
        let moved: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        for id in [&id, &copy] {
            let record = moved["root"]
                .as_array()
                .unwrap()
                .iter()
                .find(|node| node["id"] == id.as_str())
                .unwrap();
            assert_eq!(record["lastUsedEpochSeconds"], future);
        }
    }

    /// The read-mostly path still works while a live holder has the directory.
    #[test]
    fn a_live_holder_leaves_the_second_instance_read_mostly() {
        let dir = TempDir::new().unwrap();
        let LockOutcome::Acquired(_first) = SettingsLock::acquire(dir.path()).unwrap() else {
            panic!("the first claim should succeed");
        };
        let path = dir.path().join(SESSIONS_FILE);
        SessionStore::default().save_replacing(&path).unwrap();
        let LockOutcome::Held { .. } = SettingsLock::acquire(dir.path()).unwrap() else {
            panic!("the second claim should be refused");
        };
        assert!(SessionStore::load(&path).is_ok());
    }

    #[test]
    fn portable_mode_is_selected_by_the_state_file_beside_the_executable() {
        let program = TempDir::new().unwrap();
        let profile = TempDir::new().unwrap();

        let paths = SettingsPaths::choose(program.path(), profile.path().to_path_buf());
        assert!(!paths.directory().is_portable());
        assert_eq!(paths.sessions_file(), profile.path().join(SESSIONS_FILE));

        fs::write(program.path().join(PROGRAM_STATE_FILE), "{}").unwrap();
        let paths = SettingsPaths::choose(program.path(), profile.path().to_path_buf());
        assert!(paths.directory().is_portable());
        assert_eq!(paths.sessions_file(), program.path().join(SESSIONS_FILE));
        assert_eq!(paths.lock_file(), program.path().join(LOCK_FILE));
    }

    /// A test that injects no directory of its own still has to stay out of
    /// the real per-user folder.
    #[test]
    fn the_resolved_settings_directory_is_not_the_real_per_user_one() {
        let resolved = SettingsPaths::platform_per_user_directory().unwrap();
        let real = SettingsPaths::platform_directory().unwrap();
        assert!(
            !resolved.starts_with(&real),
            "{} sits under the real per-user folder {}",
            resolved.display(),
            real.display()
        );
        let program = TempDir::new().unwrap();
        fs::write(program.path().join(PROGRAM_STATE_FILE), "{}").unwrap();
        let paths = SettingsPaths::resolve(program.path(), real.clone());
        assert!(!paths.directory().path().starts_with(&real));
        assert!(!paths.directory().is_portable());
    }

    fn environment(family: PlatformFamily) -> PlatformEnvironment {
        PlatformEnvironment {
            family,
            home: None,
            config_home: None,
            app_data: None,
            runtime_directory: None,
        }
    }

    #[allow(clippy::unnecessary_wraps)]
    fn os(value: &str) -> Option<std::ffi::OsString> {
        Some(std::ffi::OsString::from(value))
    }

    /// Existing users keep their folder: an ordinary environment of each
    /// platform names the same folder it always named.
    #[test]
    fn an_ordinary_environment_names_the_folder_it_always_named() {
        let linux = PlatformEnvironment {
            home: os("/home/user"),
            ..environment(PlatformFamily::OtherUnix)
        };
        assert_eq!(
            SettingsPaths::per_user_directory_in(&linux).unwrap(),
            Path::new("/home/user").join(".config").join("compare-all")
        );
        let linux_with_config = PlatformEnvironment {
            config_home: os("/home/user/settings"),
            ..linux.clone()
        };
        assert_eq!(
            SettingsPaths::per_user_directory_in(&linux_with_config).unwrap(),
            Path::new("/home/user/settings").join("compare-all")
        );
        let mac = PlatformEnvironment {
            home: os("/Users/user"),
            config_home: os("/Users/user/.config"),
            ..environment(PlatformFamily::MacOs)
        };
        assert_eq!(
            SettingsPaths::per_user_directory_in(&mac).unwrap(),
            Path::new("/Users/user")
                .join("Library")
                .join("Application Support")
                .join("compare-all")
        );
        let windows = PlatformEnvironment {
            app_data: os(r"C:\Users\user\AppData\Roaming"),
            home: os(r"C:\Users\user"),
            ..environment(PlatformFamily::Windows)
        };
        assert_eq!(
            SettingsPaths::per_user_directory_in(&windows).unwrap(),
            Path::new(r"C:\Users\user\AppData\Roaming").join("compare-all")
        );
        let windows_share = PlatformEnvironment {
            app_data: os(r"\\server\profiles\user"),
            ..environment(PlatformFamily::Windows)
        };
        assert_eq!(
            SettingsPaths::per_user_directory_in(&windows_share).unwrap(),
            Path::new(r"\\server\profiles\user").join("compare-all")
        );
    }

    /// The XDG Base Directory rule: a relative or empty value is ignored.
    #[test]
    fn an_empty_or_relative_config_home_falls_back_to_the_home_config_folder() {
        for config_home in ["", "relative-config", "./config"] {
            let linux = PlatformEnvironment {
                home: os("/home/user"),
                config_home: os(config_home),
                ..environment(PlatformFamily::OtherUnix)
            };
            assert_eq!(
                SettingsPaths::per_user_directory_in(&linux).unwrap(),
                Path::new("/home/user").join(".config").join("compare-all"),
                "XDG_CONFIG_HOME={config_home:?}"
            );
        }
    }

    #[test]
    fn an_empty_or_relative_home_counts_as_not_set() {
        for family in [PlatformFamily::OtherUnix, PlatformFamily::MacOs] {
            for home in ["", "home/user", "~"] {
                let unix = PlatformEnvironment {
                    home: os(home),
                    ..environment(family)
                };
                assert!(
                    matches!(
                        SettingsPaths::per_user_directory_in(&unix),
                        Err(Error::NoSettingsDirectory(_))
                    ),
                    "{family:?} HOME={home:?}"
                );
            }
        }
        for app_data in ["", r"AppData\Roaming", r"C:AppData", r"\AppData"] {
            let windows = PlatformEnvironment {
                app_data: os(app_data),
                ..environment(PlatformFamily::Windows)
            };
            assert!(
                matches!(
                    SettingsPaths::per_user_directory_in(&windows),
                    Err(Error::NoSettingsDirectory(_))
                ),
                "APPDATA={app_data:?}"
            );
        }
    }

    #[test]
    fn an_absolute_config_home_is_used_without_a_home() {
        let linux = PlatformEnvironment {
            config_home: os("/srv/config"),
            ..environment(PlatformFamily::OtherUnix)
        };
        assert_eq!(
            SettingsPaths::per_user_directory_in(&linux).unwrap(),
            Path::new("/srv/config").join("compare-all")
        );
    }

    #[test]
    fn the_runtime_folder_counts_only_when_it_is_an_absolute_folder_of_this_user() {
        let runtime = TempDir::new().unwrap();
        for value in ["", "runtime", "/no/such/runtime/folder"] {
            let linux = PlatformEnvironment {
                runtime_directory: os(value),
                ..environment(PlatformFamily::OtherUnix)
            };
            assert_eq!(
                SettingsPaths::runtime_directory_in(&linux),
                None,
                "{value:?}"
            );
        }
        let windows = PlatformEnvironment {
            runtime_directory: Some(runtime.path().as_os_str().to_owned()),
            ..environment(PlatformFamily::Windows)
        };
        assert_eq!(SettingsPaths::runtime_directory_in(&windows), None);
        let file = runtime.path().join("file");
        fs::write(&file, b"x").unwrap();
        let running = PlatformEnvironment {
            runtime_directory: Some(file.into_os_string()),
            ..environment(PlatformFamily::running())
        };
        assert_eq!(SettingsPaths::runtime_directory_in(&running), None);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(runtime.path(), fs::Permissions::from_mode(0o700)).unwrap();
            let linux = PlatformEnvironment {
                runtime_directory: Some(runtime.path().as_os_str().to_owned()),
                ..environment(PlatformFamily::OtherUnix)
            };
            assert_eq!(
                SettingsPaths::runtime_directory_in(&linux),
                Some(runtime.path().join("compare-all"))
            );
        }
    }

    #[cfg(unix)]
    fn runtime_environment(runtime: &Path) -> PlatformEnvironment {
        PlatformEnvironment {
            runtime_directory: Some(runtime.as_os_str().to_owned()),
            ..environment(PlatformFamily::OtherUnix)
        }
    }

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt as _;
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    /// The XDG rule: the runtime folder is owned by the user and open to no
    /// one else, and the application folder inside it is a private folder of
    /// the user, never a link or a file.
    #[cfg(unix)]
    #[test]
    fn the_runtime_folder_counts_only_when_it_is_private_to_this_user() {
        use std::os::unix::fs::PermissionsExt as _;
        let runtime = TempDir::new().unwrap();
        let application = runtime.path().join("compare-all");
        let set_mode = |path: &Path, mode: u32| {
            fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
        };

        set_mode(runtime.path(), 0o777);
        assert_eq!(
            SettingsPaths::runtime_directory_in(&runtime_environment(runtime.path())),
            None,
            "a runtime folder open to others"
        );
        assert!(!application.exists(), "made inside an open runtime folder");

        set_mode(runtime.path(), 0o700);
        fs::write(&application, b"x").unwrap();
        assert_eq!(
            SettingsPaths::runtime_directory_in(&runtime_environment(runtime.path())),
            None,
            "a file in place of the application folder"
        );
        fs::remove_file(&application).unwrap();

        let elsewhere = TempDir::new().unwrap();
        set_mode(elsewhere.path(), 0o700);
        std::os::unix::fs::symlink(elsewhere.path(), &application).unwrap();
        assert_eq!(
            SettingsPaths::runtime_directory_in(&runtime_environment(runtime.path())),
            None,
            "a link in place of the application folder"
        );
        fs::remove_file(&application).unwrap();

        fs::create_dir(&application).unwrap();
        set_mode(&application, 0o777);
        assert_eq!(
            SettingsPaths::runtime_directory_in(&runtime_environment(runtime.path())),
            Some(application.clone())
        );
        assert_eq!(mode_of(&application), 0o700, "an open folder of the user");
        fs::remove_dir(&application).unwrap();

        assert_eq!(
            SettingsPaths::runtime_directory_in(&runtime_environment(runtime.path())),
            Some(application.clone())
        );
        assert_eq!(mode_of(&application), 0o700, "a new folder");
    }

    /// Only a privileged run can hand a folder to another user, so the case
    /// checks nothing unless the test runs with that privilege.
    #[cfg(unix)]
    #[test]
    fn a_runtime_child_of_another_user_is_refused_when_run_as_root() {
        use std::os::unix::fs::PermissionsExt as _;
        let runtime = TempDir::new().unwrap();
        fs::set_permissions(runtime.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let application = runtime.path().join("compare-all");
        fs::create_dir(&application).unwrap();
        fs::set_permissions(&application, fs::Permissions::from_mode(0o777)).unwrap();
        if std::os::unix::fs::chown(&application, Some(65534), Some(65534)).is_err() {
            println!("skipped: handing a folder to another user needs root");
            return;
        }
        assert_eq!(
            SettingsPaths::runtime_directory_in(&runtime_environment(runtime.path())),
            None
        );
        assert_eq!(
            mode_of(&application),
            0o777,
            "a folder of another user changed"
        );
    }

    #[test]
    fn a_run_with_nowhere_to_write_says_so_in_one_sentence() {
        let notice = unsaved_notice(
            "HOME is not set to an absolute folder",
            "/missing: No such file or directory (os error 2)",
        );
        assert_eq!(notice.matches("not saved").count(), 1, "{notice}");
        assert!(notice.ends_with('.') && !notice.contains(". "), "{notice}");
        assert!(notice.contains("/missing"), "{notice}");
        assert!(!notice.contains('\0'), "{notice:?}");
        assert!(SettingsPaths::is_unusable(&unusable_directory()));
        assert!(!SettingsPaths::is_unusable(Path::new("/tmp")));
    }

    #[test]
    fn the_folder_of_a_run_with_nowhere_to_write_cannot_be_created() {
        let path = unusable_directory();
        assert!(path.is_absolute());
        assert!(fs::create_dir_all(&path).is_err());
        assert!(SettingsLock::acquire(&path).is_err());
        assert!(SessionStore::default()
            .save(&path.join(SESSIONS_FILE))
            .is_err());
    }

    #[test]
    fn a_relative_temporary_folder_is_never_the_parent_of_a_run_folder() {
        for family in [
            PlatformFamily::Windows,
            PlatformFamily::MacOs,
            PlatformFamily::OtherUnix,
        ] {
            if let Ok(parent) = temporary_parent(family) {
                assert!(family.is_absolute(parent.as_os_str()), "{family:?}");
            }
        }
    }

    #[test]
    fn an_injected_directory_is_used_verbatim() {
        let dir = TempDir::new().unwrap();
        let paths = SettingsPaths::at(SettingsDirectory::PerUser(dir.path().to_path_buf()));
        assert_eq!(
            paths.program_state_file(),
            dir.path().join(PROGRAM_STATE_FILE)
        );
    }

    #[test]
    fn saving_creates_the_settings_directory() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nested").join(SESSIONS_FILE);
        SessionStore::default().save(&path).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn a_session_holding_a_path_that_is_not_text_still_saves() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(SESSIONS_FILE);
        let mut store = SessionStore::default();
        let id = store.next_id();
        let mut session = SavedSession::new(id.clone(), "Odd", SessionKind::TextCompare);
        let mut settings = crate::settings::TextCompareSettings::default();
        settings.specs.left = Some(crate::location::SideLocation::local(odd_path()));
        session.settings = SessionSettingsOverride::from_full(
            &crate::settings::SessionSettings::TextCompare(settings),
        );
        store.add_session(None, session).unwrap();

        store.save(&path).unwrap();
        let outcome = SessionStore::load(&path).unwrap();
        assert!(outcome.recovered_backup.is_none());
        assert_eq!(outcome.store.find_session(&id).unwrap().settings, {
            store.find_session(&id).unwrap().settings.clone()
        });
    }

    #[cfg(windows)]
    fn odd_path() -> PathBuf {
        use std::os::windows::ffi::OsStringExt;
        PathBuf::from(std::ffi::OsString::from_wide(&[0x0043, 0x003a, 0xd800]))
    }

    #[cfg(not(windows))]
    fn odd_path() -> PathBuf {
        use std::os::unix::ffi::OsStringExt;
        PathBuf::from(std::ffi::OsString::from_vec(vec![0x2f, 0xff]))
    }
}
