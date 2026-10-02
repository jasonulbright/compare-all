//! One error type for every record source.
//!
//! A parser reports the position it refused at, so a caller can point at the
//! offending bytes without re-reading the source.

use std::fmt;

/// Result of a record operation.
pub type Result<T> = std::result::Result<T, RecordError>;

/// Why a record source cannot be read, written or compared.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RecordError {
    /// The bytes do not match the format the parser was asked for.
    #[error("{context}: malformed input at byte {offset}")]
    Malformed {
        /// Structure the parser was reading.
        context: String,
        /// Byte offset the parser refused at.
        offset: u64,
    },

    /// The structure claims more bytes than the source holds.
    #[error("{context}: needs {needed} bytes at byte {offset}, {available} available")]
    Truncated {
        /// Structure the parser was reading.
        context: String,
        /// Byte offset of the read.
        offset: u64,
        /// Bytes the structure claims.
        needed: u64,
        /// Bytes left in the source.
        available: u64,
    },

    /// A configured limit refuses the work before any allocation happens.
    #[error("{limit} limit of {allowed} exceeded by {requested}")]
    LimitExceeded {
        /// Name of the limit field.
        limit: &'static str,
        /// Configured ceiling.
        allowed: u64,
        /// Value the source asked for.
        requested: u64,
    },

    /// The operation is not available on this platform or through this reader.
    #[error("unsupported: {0}")]
    Unsupported(String),

    /// The operating system refused the read.
    #[error("access denied: {path}")]
    AccessDenied {
        /// Key or file the reader was opening.
        path: String,
    },

    /// The key or file does not exist.
    #[error("not found: {path}")]
    NotFound {
        /// Key or file the reader was opening.
        path: String,
    },

    /// An edit would create a key or a value that is already there.
    #[error("already exists: {path}")]
    AlreadyExists {
        /// Key or value the edit named.
        path: String,
    },

    /// A write was refused before anything changed, for the stated reason.
    #[error("refused: {0}")]
    Refused(String),

    /// The address the caller gave does not name a registry location.
    #[error("invalid specification: {0}")]
    InvalidSpec(String),

    /// The operating system failed the read for another reason.
    #[error("{context}: {message}")]
    Io {
        /// Operation that failed.
        context: String,
        /// Message from the operating system.
        message: String,
    },
}

impl RecordError {
    /// Build a [`RecordError::Malformed`] for `context` at `offset`.
    #[must_use]
    pub fn malformed(context: impl fmt::Display, offset: u64) -> Self {
        Self::Malformed {
            context: context.to_string(),
            offset,
        }
    }

    /// Build a [`RecordError::Truncated`] for `context` at `offset`.
    #[must_use]
    pub fn truncated(context: impl fmt::Display, offset: u64, needed: u64, available: u64) -> Self {
        Self::Truncated {
            context: context.to_string(),
            offset,
            needed,
            available,
        }
    }

    /// Build a [`RecordError::Unsupported`].
    #[must_use]
    pub fn unsupported(reason: impl fmt::Display) -> Self {
        Self::Unsupported(reason.to_string())
    }

    /// Build a [`RecordError::Refused`].
    #[must_use]
    pub fn refused(reason: impl fmt::Display) -> Self {
        Self::Refused(reason.to_string())
    }

    /// True when the error reports a limit rather than bad input.
    #[must_use]
    pub const fn is_limit(&self) -> bool {
        matches!(self, Self::LimitExceeded { .. })
    }
}
