//! Version resources of Windows executables and libraries.
//!
//! The reader works from file bytes on every platform. It walks the DOS stub,
//! the PE headers, the resource directory and the `VS_VERSIONINFO` block, and
//! produces the fixed file information, one group per string table language,
//! the translation list and a group of facts taken from the headers.
//!
//! Every offset, length and count in those structures comes from the file, so
//! every one is range checked before use and every allocation is checked
//! against [`crate::limits::Limits`].

mod pe;
mod resource;

use crate::compare::{compare_trees, AlignOptions, TreeDiff};
use crate::error::Result;
use crate::limits::{Limits, Unknown};
use crate::record::RecordTree;
use serde::{Deserialize, Serialize};

pub use pe::{FileFacts, Machine, Subsystem};
pub use resource::{FixedFileInfo, StringTable, VersionInfo};

/// Which string table a listing prefers when a file carries several.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub enum LanguageChoice {
    /// The neutral language table, falling back to the first table.
    ///
    /// This matches how the operating system resolves version information.
    #[default]
    Neutral,
    /// The first table in the file.
    First,
    /// A named language and code page, both as the file writes them.
    Specific {
        /// Language identifier.
        language: u16,
        /// Code page identifier.
        code_page: u16,
    },
    /// A value this build does not understand.
    #[serde(untagged)]
    Unknown(serde_json::Value),
}

/// How a version resource is read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct VersionReadOptions {
    /// Which string table the preferred group holds.
    pub language: LanguageChoice,
    /// Add the header facts group: machine, word size, time stamp, subsystem.
    pub include_file_facts: bool,
    /// Add a record stating whether a signature block is present.
    pub include_signature_presence: bool,
    /// Allocation ceilings applied while reading.
    pub limits: Limits,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub unknown: Unknown,
}

impl Default for VersionReadOptions {
    fn default() -> Self {
        Self {
            language: LanguageChoice::Neutral,
            include_file_facts: true,
            include_signature_presence: true,
            limits: Limits::default(),
            unknown: Unknown::new(),
        }
    }
}

/// Options of a version comparison.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct VersionCompareOptions {
    /// Rules shared by every record comparison.
    pub align: AlignOptions,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub unknown: Unknown,
}

/// Read the version information of a Windows binary.
///
/// # Errors
///
/// Returns [`crate::error::RecordError::Malformed`] when the file is not a
/// Windows binary, [`crate::error::RecordError::NotFound`] when it carries no
/// version resource, [`crate::error::RecordError::Truncated`] when a structure
/// claims more bytes than the file holds, and
/// [`crate::error::RecordError::LimitExceeded`] when a limit refuses the work.
pub fn read(bytes: &[u8], options: &VersionReadOptions) -> Result<RecordTree> {
    resource::read_tree(bytes, options)
}

/// Compare two version trees.
#[must_use]
pub fn compare(left: &RecordTree, right: &RecordTree, options: &VersionCompareOptions) -> TreeDiff {
    compare_trees(left, right, &options.align)
}
