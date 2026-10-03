//! Session model, settings layering and persistence.
//!
//! This crate holds plain data: the kinds of comparison a session can perform,
//! the locators naming each side, the typed settings each kind owns, the three
//! layer resolution that turns partial overrides into fully populated settings,
//! the saved session tree and workspaces, import and export packages, and
//! administrator policies. Comparison engines convert from these types; nothing
//! here performs a comparison.

mod document;
pub mod error;
pub mod kind;
pub mod layer;
pub mod location;
pub mod options;
pub mod policy;
pub mod settings;
pub mod share;
pub mod store;

pub use document::MAX_DOCUMENT_BYTES;
pub use error::{Error, Result};
pub use kind::{KindParseError, SessionKind};
pub use layer::{ResolvedSettings, SettingsLayers};
pub use location::{LocationParseError, Password, ProfileLocation, SideLocation, StoredPath};
pub use options::{
    OptionsLoad, ProgramOptions, RestoreCategory, RestoreSelection, OPTIONS_FILE,
    OPTIONS_SCHEMA_VERSION,
};
pub use policy::{AdminPolicies, InMemoryPolicySource, PolicyError, PolicyKey, PolicyLoader};
pub use settings::{SessionSettings, SessionSettingsOverride};
pub use share::{
    ExportSelection, ImportFailure, ImportOptions, ImportReport, SettingsPackage, SharedSessions,
};
pub use store::{
    DocumentStamp, LoadOutcome, LoadRepairs, LockHolder, LockOutcome, PlatformEnvironment,
    PlatformFamily, SaveOutcome, SavedSession, SessionId, SessionStore, SettingsDirectory,
    SettingsLock, SettingsPaths, TreeNode, UnknownNode, WindowBounds, Workspace, WorkspaceTab,
    WorkspaceWindow, SETTINGS_DIRECTORY_VARIABLE,
};
