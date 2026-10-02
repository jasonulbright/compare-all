//! Error type shared by persistence, sharing and policy loading.

use std::path::PathBuf;

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Everything that can go wrong while reading or writing stored settings.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An I/O operation against a settings file failed.
    #[error("input/output failure on {path}: {source}")]
    Io {
        /// File the operation targeted.
        path: PathBuf,
        /// Underlying failure.
        #[source]
        source: std::io::Error,
    },
    /// A stored document could not be parsed as the expected schema.
    #[error("cannot parse {path}: {source}")]
    Parse {
        /// File that failed to parse.
        path: PathBuf,
        /// Underlying failure.
        #[source]
        source: serde_json::Error,
    },
    /// The document on disk is no longer the one the store was read from, so
    /// writing it would discard whatever the other writer stored.
    #[error("{path} changed since it was loaded")]
    DocumentChanged {
        /// File that changed.
        path: PathBuf,
    },
    /// A tree operation named an identifier that is not in the tree.
    #[error("no such node: {0}")]
    NoSuchNode(String),
    /// A tree operation would place a node inside itself or inside a file node.
    #[error("invalid move: {0}")]
    InvalidMove(&'static str),
    /// A name collides with a sibling in the same folder.
    #[error("name already used in this folder: {0}")]
    DuplicateName(String),
    /// A write targeted a session or branch that is read-only.
    #[error("read-only: {0}")]
    ReadOnly(&'static str),
    /// Settings of one kind were applied to a session of another.
    #[error("settings for {found} cannot be applied to {expected}")]
    KindMismatch {
        /// Kind of the target.
        expected: crate::kind::SessionKind,
        /// Kind of the value being applied.
        found: crate::kind::SessionKind,
    },
    /// A recognized session kind contains settings fields this build cannot
    /// deserialize, so applying them would lose their stored meaning.
    #[error("settings for {kind} contain fields this build cannot read")]
    UnreadableSettings {
        /// Kind named by the settings document.
        kind: crate::kind::SessionKind,
    },
    /// The settings directory could not be determined from the environment.
    #[error("cannot determine settings directory: {0}")]
    NoSettingsDirectory(&'static str),
}
