//! Diff engines. Every engine takes two (or three) inputs and returns aligned
//! regions; rendering, editing, and grammar analysis live elsewhere.
//!
//! The crate is organised as six independent layers:
//!
//! - [`lines`] aligns two texts line by line under a set of ignore options.
//! - [`importance`] classifies the resulting difference hunks as important or
//!   unimportant so the two classes can be colored separately.
//! - [`inline`] locates the differing character or token spans inside a pair of
//!   changed lines.
//! - [`bytes`] aligns two byte streams for hexadecimal comparison.
//! - [`merge3`] combines two changed versions against a common ancestor.
//! - [`patch`] reads unified and context diff files and applies them.

// Type names intentionally repeat their module name because the whole public
// surface is re-exported flat from the crate root.
#![allow(clippy::module_name_repetitions)]

pub mod bytes;
pub mod cancel;
pub mod importance;
pub mod inline;
pub mod lines;
pub mod merge3;
pub mod patch;

pub use bytes::{diff_bytes, diff_bytes_cancellable, ByteAlignment, ByteHunk};
pub use cancel::{Cancel, NeverCancel};
pub use importance::{
    classify_hunks, classify_hunks_cancellable, classify_hunks_indexed,
    classify_hunks_indexed_cancellable, ClassifiedHunk, ClassifiedToken, ClassifierSide,
    Importance, IndexedLineClassifier, LineClassifier, ReplacementOptions, ReplacementRule,
    ReplacementSide, RuleSet, TokenCategory, WhitespaceClassifier,
};
pub use inline::{
    diff_chars, diff_chars_cancellable, diff_tokens, diff_tokens_cancellable, tokenize, InlineDiff,
    InlineOptions, Span,
};
pub use lines::{
    diff_line_slices, diff_line_slices_anchored, diff_line_slices_anchored_cancellable,
    diff_line_slices_cancellable, diff_lines, diff_lines_with, normalize_line, split_lines,
    AlignAnchor, AlignmentMode, AlignmentOptions, Hunk, HunkKind, LineCompareOptions,
};
pub use merge3::{
    merge3, merge3_cancellable, merge3_text, ConflictScope, MergeKind, MergeOptions, MergeRegion,
    MergeResult, MergeSide,
};
pub use patch::{
    apply_patch, parse_patch, parse_patch_with, Applied, FilePatch, Patch, PatchError, PatchHunk,
    PatchLimits, PatchLine, PatchLineKind,
};

/// Errors raised while building diff configuration or running a comparison.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DiffError {
    /// A user supplied pattern could not be compiled.
    #[error("invalid regular expression `{pattern}`: {message}")]
    Regex {
        /// The pattern as the user wrote it.
        pattern: String,
        /// The compiler's complaint.
        message: String,
    },
    /// The caller raised the cancellation flag before the work finished.
    #[error("comparison cancelled")]
    Cancelled,
    /// A caller supplied region does not describe a position in its input.
    #[error("malformed {what}: {detail}")]
    Malformed {
        /// The kind of value that failed validation.
        what: &'static str,
        /// What is wrong with it.
        detail: String,
    },
}

impl DiffError {
    pub(crate) fn malformed(what: &'static str, detail: impl Into<String>) -> Self {
        Self::Malformed {
            what,
            detail: detail.into(),
        }
    }
}
