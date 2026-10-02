//! Errors the parser and the executor raise.

use std::fmt;

/// A syntax error, with the place it was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// One based line number in the source.
    pub line: u32,
    /// One based column number, counted in characters.
    pub column: u32,
    /// What is wrong, in plain words.
    pub message: String,
}

impl ParseError {
    /// An error at a place.
    #[must_use]
    pub fn new(line: u32, column: u32, message: impl Into<String>) -> Self {
        Self {
            line,
            column,
            message: message.into(),
        }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "line {}, column {}: {}",
            self.line, self.column, self.message
        )
    }
}

impl std::error::Error for ParseError {}

/// A command form the text encoder cannot write back out.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EncodeError {
    /// The script syntax has no way to write a quotation mark, a line break or
    /// a NUL byte inside an argument.
    #[error("the argument {value:?} holds a character the script syntax cannot write")]
    Unwritable {
        /// The argument.
        value: String,
    },
    /// The command contains a state the script syntax cannot represent.
    #[error("the command has no script representation: {detail}")]
    Unrepresentable {
        /// Why the command cannot be represented.
        detail: &'static str,
    },
}

/// A command failed while the script ran.
#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    /// The command is understood but the engine behind it is not built yet.
    #[error("{command} is not supported yet: {detail}")]
    NotSupported {
        /// The command word.
        command: String,
        /// What part is missing.
        detail: String,
    },
    /// A command needs base folders and none are loaded.
    #[error("{command} needs base folders; run load first")]
    NoComparison {
        /// The command word.
        command: String,
    },
    /// A command needs a selection and none is set.
    #[error("{command} needs a selection; run select first")]
    NoSelection {
        /// The command word.
        command: String,
    },
    /// The command's arguments cannot be carried out as written.
    #[error("{0}")]
    Refused(String),
    /// A file or folder could not be read or written.
    #[error("{context}: {source}")]
    Io {
        /// What was being done.
        context: String,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },
}

impl ExecError {
    /// A typed "not built yet" failure.
    #[must_use]
    pub fn not_supported(command: &str, detail: impl Into<String>) -> Self {
        Self::NotSupported {
            command: command.to_string(),
            detail: detail.into(),
        }
    }

    /// True for a failure that names a missing engine rather than a bad run.
    #[must_use]
    pub fn is_not_supported(&self) -> bool {
        matches!(self, Self::NotSupported { .. })
    }
}

/// Why a whole run stopped.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// The script file could not be read.
    #[error("the script file could not be read: {0}")]
    Load(#[source] std::io::Error),
    /// The script file does not parse.
    #[error("{0}")]
    Syntax(#[from] ParseError),
    /// A `load` command could not open the folders or files it names.
    #[error("{0}")]
    LoadFailed(String),
    /// A command failed and the run was told to stop at the first failure.
    #[error("{0}")]
    Stopped(String),
}
