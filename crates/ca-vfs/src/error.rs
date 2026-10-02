//! The error type every file system operation returns.

use crate::path::{PathError, VfsPath};

/// Result alias for this crate.
pub type VfsResult<T> = Result<T, VfsError>;

/// Which of the streaming limits was hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitKind {
    /// One entry expanded past the per-entry byte ceiling.
    EntrySize,
    /// The entries read so far expanded past the per-archive byte ceiling.
    ArchiveSize,
    /// A container declared more entries than the reader can safely index.
    ArchiveEntries,
    /// One entry expanded by more than the allowed multiple of its stored size.
    ExpansionRatio,
    /// Archives are nested deeper than the allowed number of levels.
    NestingDepth,
}

impl LimitKind {
    /// Short description used in the error message.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EntrySize => "entry size",
            Self::ArchiveSize => "archive size",
            Self::ArchiveEntries => "archive entry count",
            Self::ExpansionRatio => "expansion ratio",
            Self::NestingDepth => "nesting depth",
        }
    }

    fn unit(self) -> &'static str {
        match self {
            Self::ArchiveEntries => " entries",
            Self::EntrySize | Self::ArchiveSize => " bytes",
            Self::ExpansionRatio | Self::NestingDepth => "",
        }
    }
}

/// Everything that can go wrong in a virtual file system.
#[derive(Debug, thiserror::Error)]
pub enum VfsError {
    /// No entry with that path.
    #[error("not found: {path}")]
    NotFound {
        /// Path that was looked up.
        path: VfsPath,
    },
    /// A directory was required and the entry is a file.
    #[error("not a directory: {path}")]
    NotADirectory {
        /// Path that was looked up.
        path: VfsPath,
    },
    /// A file was required and the entry is a directory.
    #[error("is a directory: {path}")]
    IsADirectory {
        /// Path that was looked up.
        path: VfsPath,
    },
    /// The target already exists and the operation does not replace it.
    #[error("already exists: {path}")]
    AlreadyExists {
        /// Path that was written to.
        path: VfsPath,
    },
    /// The file system does not accept writes at all.
    #[error("read-only file system")]
    ReadOnly,
    /// The operation, format or feature is not implemented here.
    #[error("unsupported: {what}")]
    Unsupported {
        /// What was asked for.
        what: String,
    },
    /// The entry is encrypted and no password was supplied.
    #[error("password required: {path}")]
    NeedsPassword {
        /// Entry that is encrypted.
        path: VfsPath,
    },
    /// The supplied password did not decrypt the entry.
    #[error("incorrect password: {path}")]
    WrongPassword {
        /// Entry that is encrypted.
        path: VfsPath,
    },
    /// The listing records the entry but the container holds no content for it.
    #[error("content is not stored for {path}")]
    ContentNotStored {
        /// Entry whose content is absent.
        path: VfsPath,
    },
    /// The listing names the entry, and the source refuses to open or extract
    /// it. The reason is the text on the entry's error field.
    #[error("refused: {path}: {reason}")]
    Refused {
        /// Entry that was asked for.
        path: VfsPath,
        /// Why the source refuses it.
        reason: String,
    },
    /// The content read back did not match the checksum the container stores.
    #[error("checksum mismatch for {path}: container records {expected:#010x}, content hashes to {actual:#010x}")]
    ChecksumMismatch {
        /// Entry whose content did not verify.
        path: VfsPath,
        /// What the container's metadata claimed.
        expected: u32,
        /// What the content actually hashed to.
        actual: u32,
    },
    /// The container changed on disk since it was opened.
    #[error("container changed on disk since it was opened: {detail}")]
    ContainerChanged {
        /// What differed.
        detail: String,
    },
    /// The container is damaged or is not the format it claimed to be.
    #[error("damaged container: {detail}")]
    Corrupt {
        /// What did not parse.
        detail: String,
    },
    /// A streaming limit tripped, most likely on a crafted container.
    #[error("{kind} limit exceeded: {limit}{unit}", kind = kind.as_str(), unit = kind.unit())]
    LimitExceeded {
        /// Which ceiling was hit.
        kind: LimitKind,
        /// The ceiling that was in force.
        limit: u64,
    },
    /// A remote protocol refused work after reaching a client-side resource cap.
    #[error("resource limit exceeded: {resource}")]
    ResourceLimit {
        /// The bounded resource that was exhausted.
        resource: String,
    },
    /// The caller raised the cancellation flag.
    #[error("operation cancelled")]
    Cancelled,
    /// A connection could not be made, or was lost part way through.
    #[error("network error: {detail}")]
    Network {
        /// What failed, with no credential in it.
        detail: String,
    },
    /// A call passed its deadline.
    #[error("timed out: {operation}")]
    Timeout {
        /// Which call ran out of time.
        operation: String,
    },
    /// The server refused the credentials.
    ///
    /// The text is built from the server's own reply and the account name.
    /// No secret ever reaches it.
    #[error("authentication failed: {detail}")]
    AuthFailed {
        /// What the server said.
        detail: String,
    },
    /// The transport could not be secured, or the certificate did not verify.
    #[error("tls error: {detail}")]
    Tls {
        /// What failed.
        detail: String,
    },
    /// The host is not in the known-hosts store yet.
    ///
    /// The caller shows the fingerprint, asks whether it is the right one, and
    /// records the answer before retrying.
    #[error("unknown host key for {host}: {fingerprint}")]
    UnknownHostKey {
        /// Host and port the key was offered for.
        host: String,
        /// Fingerprint of the offered key.
        fingerprint: String,
    },
    /// The host offered a key that differs from the recorded one.
    ///
    /// This is never retried automatically. Either the server was rebuilt or
    /// the connection is not reaching the server it claims to.
    #[error(
        "host key for {host} changed: the store records {known}, the server offered {offered}"
    )]
    HostKeyChanged {
        /// Host and port the key was offered for.
        host: String,
        /// Fingerprint the store records.
        known: String,
        /// Fingerprint the server offered.
        offered: String,
    },
    /// The server's reply does not follow the protocol.
    #[error("protocol error: {detail}")]
    Protocol {
        /// What did not parse, or what reply arrived out of turn.
        detail: String,
    },
    /// A name in the container is not a usable relative path.
    #[error("invalid path")]
    InvalidPath(#[from] PathError),
    /// Anything the host file system reported.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

impl VfsError {
    /// Build an [`VfsError::Unsupported`] from anything printable.
    pub fn unsupported(what: impl Into<String>) -> Self {
        Self::Unsupported { what: what.into() }
    }

    /// Build a [`VfsError::Corrupt`] from anything printable.
    pub fn corrupt(detail: impl Into<String>) -> Self {
        Self::Corrupt {
            detail: detail.into(),
        }
    }

    /// Build a [`VfsError::Network`] from anything printable.
    pub fn network(detail: impl Into<String>) -> Self {
        Self::Network {
            detail: detail.into(),
        }
    }

    /// Build a [`VfsError::Protocol`] from anything printable.
    pub fn protocol(detail: impl Into<String>) -> Self {
        Self::Protocol {
            detail: detail.into(),
        }
    }

    /// Build a [`VfsError::Timeout`] from anything printable.
    pub fn timeout(operation: impl Into<String>) -> Self {
        Self::Timeout {
            operation: operation.into(),
        }
    }

    /// Build a [`VfsError::Tls`] from anything printable.
    pub fn tls(detail: impl Into<String>) -> Self {
        Self::Tls {
            detail: detail.into(),
        }
    }

    /// Build a [`VfsError::AuthFailed`] from anything printable.
    pub fn auth_failed(detail: impl Into<String>) -> Self {
        Self::AuthFailed {
            detail: detail.into(),
        }
    }

    /// True when a retry can succeed only after the caller records a host key.
    #[must_use]
    pub fn needs_host_key(&self) -> bool {
        matches!(self, Self::UnknownHostKey { .. })
    }

    /// True when the caller can retry after supplying or correcting a password.
    #[must_use]
    pub fn needs_password(&self) -> bool {
        matches!(
            self,
            Self::NeedsPassword { .. } | Self::WrongPassword { .. }
        )
    }
}
