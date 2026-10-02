//! Settings groups and value types shared by more than one session kind.

use super::macros::settings_group;
use crate::location::SideLocation;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Which file format reads a side.
///
/// A tagged variant with no data of its own still serializes as an object, so
/// it carries the flattened `unknown` map: without one, a field a newer build
/// writes beside the tag is dropped on load.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "mode",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
#[non_exhaustive]
pub enum FileFormatChoice {
    /// Resolve the format by matching the file name against format masks.
    Detected {
        /// Fields written by another build, preserved verbatim.
        #[serde(
            flatten,
            default,
            skip_serializing_if = "std::collections::BTreeMap::is_empty"
        )]
        unknown: std::collections::BTreeMap<String, Value>,
    },
    /// Use the named format regardless of the file name.
    Named {
        /// Name of the format to use.
        name: String,
        /// Fields written by another build, preserved verbatim.
        #[serde(
            flatten,
            default,
            skip_serializing_if = "std::collections::BTreeMap::is_empty"
        )]
        unknown: std::collections::BTreeMap<String, Value>,
    },
    /// A choice this build does not understand, carried through unchanged.
    #[serde(untagged)]
    Unknown(Value),
}

impl Default for FileFormatChoice {
    fn default() -> Self {
        FileFormatChoice::detected()
    }
}

impl FileFormatChoice {
    /// Resolve the format from the file name.
    #[must_use]
    pub fn detected() -> Self {
        FileFormatChoice::Detected {
            unknown: std::collections::BTreeMap::new(),
        }
    }
}

/// Per-side override of the character encoding the file format would pick.
///
/// A tagged variant with no data of its own still serializes as an object, so
/// it carries the flattened `unknown` map.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "mode",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
#[non_exhaustive]
pub enum EncodingChoice {
    /// Leave the choice to the file format.
    FromFormat {
        /// Fields written by another build, preserved verbatim.
        #[serde(
            flatten,
            default,
            skip_serializing_if = "std::collections::BTreeMap::is_empty"
        )]
        unknown: std::collections::BTreeMap<String, Value>,
    },
    /// Read the side with the named encoding.
    Named {
        /// Name of the encoding to use.
        name: String,
        /// Fields written by another build, preserved verbatim.
        #[serde(
            flatten,
            default,
            skip_serializing_if = "std::collections::BTreeMap::is_empty"
        )]
        unknown: std::collections::BTreeMap<String, Value>,
    },
    /// A choice this build does not understand, carried through unchanged.
    #[serde(untagged)]
    Unknown(Value),
}

impl Default for EncodingChoice {
    fn default() -> Self {
        EncodingChoice::from_format()
    }
}

impl EncodingChoice {
    /// Leave the choice to the file format.
    #[must_use]
    pub fn from_format() -> Self {
        EncodingChoice::FromFormat {
            unknown: std::collections::BTreeMap::new(),
        }
    }
}

/// Algorithm pairing lines or rows between the two sides.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum AlignmentAlgorithm {
    /// Position N on the left is paired with position N on the right.
    Unaligned,
    /// Successively smaller sections are matched, so partial results display
    /// before the comparison finishes.
    #[default]
    Standard,
    /// Longest common subsequence. Produces no output until it completes and
    /// groups mismatches as blocks rather than pairing them.
    MyersOnd,
    /// Patience diff.
    Patience,
    /// An algorithm this build does not understand, carried through unchanged.
    #[serde(untagged)]
    Unknown(Value),
}

/// How a side is picked when a rule can apply to one side only.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum RuleSide {
    /// Apply to both sides.
    #[default]
    Both,
    /// Apply to the left side only.
    Left,
    /// Apply to the right side only.
    Right,
    /// A side this build does not understand, carried through unchanged.
    #[serde(untagged)]
    Unknown(Value),
}

/// A substitution declared equivalent, so matching text on the two sides is an
/// unimportant difference rather than a change.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ReplacementItem {
    /// Pattern searched for.
    #[serde(default)]
    pub find: String,
    /// Text treated as its equivalent on the other side.
    #[serde(default)]
    pub replace_with: String,
    /// Only match text with identical capitalization.
    #[serde(default)]
    pub match_case: bool,
    /// Reject matches that fall inside a longer word.
    #[serde(default)]
    pub whole_words_only: bool,
    /// Read both fields as replacement-style regular expressions.
    #[serde(default)]
    pub regular_expression: bool,
    /// Side the find text is searched in.
    #[serde(default)]
    pub side: RuleSide,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub unknown: std::collections::BTreeMap<String, Value>,
}

settings_group! {
    /// Sides of the comparison and the free text stored with the session.
    SpecsSettings / SpecsOverride {
        /// Left side, empty until the session is given one.
        left: Option<SideLocation> = None,
        /// Right side, empty until the session is given one.
        right: Option<SideLocation> = None,
        /// Common ancestor of the other two sides, used by merge kinds.
        ancestor: Option<SideLocation> = None,
        /// Destination the merge result is written to, used by merge kinds.
        output: Option<SideLocation> = None,
        /// Protect loaded content from being written back to disk. Child
        /// sessions inherit the restriction.
        disable_editing: bool = false,
        /// Free text stored with the session.
        description: String = String::new(),
    }
}

settings_group! {
    /// Which file format and encoding read each side.
    FormatSettings / FormatOverride {
        /// Format reading the left side.
        left_format: FileFormatChoice = FileFormatChoice::detected(),
        /// Format reading the right side.
        right_format: FileFormatChoice = FileFormatChoice::detected(),
        /// Encoding override for the left side.
        left_encoding: EncodingChoice = EncodingChoice::from_format(),
        /// Encoding override for the right side.
        right_encoding: EncodingChoice = EncodingChoice::from_format(),
    }
}

settings_group! {
    /// Substitutions treated as unimportant differences.
    ReplacementSettings / ReplacementOverride {
        /// Declared substitutions, applied in list order.
        items: Vec<ReplacementItem> = Vec::new(),
    }
}
