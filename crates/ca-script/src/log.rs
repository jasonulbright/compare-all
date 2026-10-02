//! The run log.
//!
//! Logging is off until a `log` command turns it on. Each line carries a
//! timestamp, a tag naming what the line records, and the text:
//!
//! ```text
//! 2001-02-03 04:05:06  command  copy left->right
//! 2001-02-03 04:05:06  note     3 items copied
//! 2001-02-03 04:05:06  error    cannot read a.txt
//! ```
//!
//! `normal` writes the command lines, the notes that summarise a command, and
//! every error. `verbose` adds one line for each item a command touched.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::ast::{LogLevel, LogSpec};
use crate::clock;

/// The file name used when a `log` command names a level but no file.
pub const DEFAULT_LOG_NAME: &str = "Log.txt";

/// What a line records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tag {
    /// The command about to run.
    Command,
    /// A summary of what a command did.
    Note,
    /// One item a command touched.
    Item,
    /// A failure.
    Error,
}

impl Tag {
    fn word(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::Note => "note",
            Self::Item => "item",
            Self::Error => "error",
        }
    }

    fn level(self) -> LogLevel {
        match self {
            Self::Item => LogLevel::Verbose,
            _ => LogLevel::Normal,
        }
    }
}

/// Where the run writes its log.
#[derive(Debug, Default)]
pub struct Log {
    level: LogLevel,
    file: Option<File>,
    path: Option<PathBuf>,
    offset_seconds: i64,
    now: Option<i64>,
}

impl Log {
    /// The level in force.
    #[must_use]
    pub fn level(&self) -> LogLevel {
        self.level
    }

    /// The file the log is written to, once one is open.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Read timestamps in a zone this many seconds ahead of UTC.
    pub fn set_offset_seconds(&mut self, offset_seconds: i64) {
        self.offset_seconds = offset_seconds;
    }

    /// Write one fixed timestamp on every line. Tests use it so a log file
    /// compares byte for byte.
    pub fn set_fixed_time(&mut self, seconds: Option<i64>) {
        self.now = seconds;
    }

    /// Apply one `log` command.
    ///
    /// # Errors
    /// Returns the failure that stopped the file being opened.
    pub fn configure(&mut self, spec: &LogSpec, working_directory: &Path) -> std::io::Result<()> {
        if let Some(level) = spec.level {
            self.level = level;
        }
        if self.level == LogLevel::None {
            self.file = None;
            self.path = None;
            return Ok(());
        }
        let (append, name) = if let Some(target) = &spec.target {
            (target.append, PathBuf::from(&target.file))
        } else {
            if self.file.is_some() {
                return Ok(());
            }
            (false, PathBuf::from(DEFAULT_LOG_NAME))
        };
        let path = if name.is_absolute() {
            name
        } else {
            working_directory.join(name)
        };
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let file = ca_io::open_log(&path, append)?;
        self.file = Some(file);
        self.path = Some(path);
        Ok(())
    }

    /// Write one line, when the level asks for it.
    pub fn write(&mut self, tag: Tag, text: &str) {
        if self.level == LogLevel::None || tag.level() > self.level {
            return;
        }
        let Some(file) = self.file.as_mut() else {
            return;
        };
        let seconds = self
            .now
            .unwrap_or_else(|| clock::unix_seconds(std::time::SystemTime::now()))
            + self.offset_seconds;
        // A log line that cannot be written must not stop the run: the run's
        // own result is reported through its exit code, not through the log.
        let _ = writeln!(
            file,
            "{}  {:<7}  {}",
            clock::format_stamp(seconds),
            tag.word(),
            text
        );
        let _ = file.flush();
    }

    /// Write the command about to run.
    pub fn command(&mut self, text: &str) {
        self.write(Tag::Command, text);
    }

    /// Write a summary of what a command did.
    pub fn note(&mut self, text: &str) {
        self.write(Tag::Note, text);
    }

    /// Write one item a command touched.
    pub fn item(&mut self, text: &str) {
        self.write(Tag::Item, text);
    }

    /// Write a failure.
    pub fn error(&mut self, text: &str) {
        self.write(Tag::Error, text);
    }
}
