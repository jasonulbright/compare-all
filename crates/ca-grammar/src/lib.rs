//! File formats and syntax grammars for compare-all.
//!
//! The crate has five layers that stack:
//!
//! - [`format`] holds the persisted description of a class of files: which
//!   filenames it claims, which comparison view handles them, how they are
//!   converted before comparing, and the editor settings that apply.
//! - [`grammar`] holds the syntax definition a text format carries: an ordered
//!   list of items, each naming the element it contributes to.
//! - [`lexer`] turns a grammar plus one line of text plus the state carried in
//!   from the previous line into tokens that tile the line.
//! - [`classify`] adapts the lexer to the diff engine's [`LineClassifier`], so
//!   element names drive the important/unimportant split.
//! - [`highlight`] maps element names to a small set of color roles without
//!   naming any concrete color or depending on a UI toolkit.
//!
//! [`builtin`] supplies stock formats built through the same public model a
//! user-authored format uses, so an editor dialog can round-trip them.
//!
//! [`LineClassifier`]: ca_diff::importance::LineClassifier
//!
//! # Forward compatibility
//!
//! Every persisted struct carries a flattened [`UnknownFields`] map and every
//! persisted enum is wrapped in [`Extensible`]. A settings file written by a
//! newer build therefore survives a load followed by a save with its unknown
//! keys and unknown enum spellings intact.

#![allow(
    clippy::must_use_candidate,
    clippy::module_name_repetitions,
    clippy::missing_panics_doc
)]

pub mod builtin;
pub mod classify;
pub mod format;
pub mod grammar;
pub mod highlight;
pub mod lexer;
pub mod mask;
pub mod pattern;

mod compat;

pub use classify::{GrammarClassifier, IndexedGrammarClassifier};
pub use compat::{Extensible, UnknownFields};
pub use format::{
    ConversionCommand, ConversionMethod, ConversionPaths, ConversionSettings, ExternalSettings,
    FileFormat, FormatKind, FormatRegistry, MiscSettings, TableSettings, TextEncodingDefault,
};
pub use grammar::{ColumnEnd, Grammar, GrammarItem, ItemKind, MatchOptions};
pub use highlight::{StyleMap, StyleSlot};
pub use lexer::{LexOutput, LexToken, Lexer, LineState, StateCache};
pub use mask::MaskList;
pub use pattern::CompiledPattern;

/// Anything that can go wrong while compiling or running a grammar.
#[derive(Debug, thiserror::Error)]
pub enum GrammarError {
    /// A pattern in a grammar item or file mask did not compile.
    #[error("pattern `{pattern}` does not compile: {message}")]
    Pattern {
        /// The pattern as the user wrote it.
        pattern: String,
        /// The engine's complaint.
        message: String,
    },
    /// Matching a pattern exceeded the backtracking budget. The budget exists
    /// so that a user-authored pattern cannot hang the comparison; exceeding
    /// it is reported rather than treated as "no match", because a silent
    /// no-match would show up later as an unexplained coloring difference.
    #[error("pattern `{pattern}` exceeded its matching budget")]
    Backtrack {
        /// The pattern as the user wrote it.
        pattern: String,
    },
    /// A grammar item's fields are inconsistent with its kind.
    #[error("grammar item `{element}` is not usable: {message}")]
    Item {
        /// Element name the item contributes to.
        element: String,
        /// What is wrong.
        message: String,
    },
    /// A file mask could not be turned into a matcher.
    #[error("file mask `{mask}` is not usable: {message}")]
    Mask {
        /// The offending mask.
        mask: String,
        /// What is wrong.
        message: String,
    },
}
