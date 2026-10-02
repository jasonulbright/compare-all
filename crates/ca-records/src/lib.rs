//! Record sources: registry data, version resources, and media tags.
//!
//! # What this crate holds
//!
//! Three engines read named values out of a source and reduce them to one
//! shape:
//!
//! - [`registry`] reads registry export files on every platform and the live
//!   registry on Windows.
//! - [`version`] reads the version resource of a Windows binary from its bytes,
//!   on every platform.
//! - [`media`] reads tags and stream facts out of audio files.
//!
//! All three produce a [`record::RecordTree`]: groups of named values, each
//! value carrying a path, a name, a typed value with a display form, and the
//! byte range in the source where the source has one. [`compare`] merges two
//! trees into one with a status per node, and [`compare::TreeDiff::rows`]
//! flattens the merge into rows a record report reads field for field.
//!
//! # Untrusted input
//!
//! Every parser reads files it did not write. Every length, count and offset
//! that comes from a file is range checked before use, and every allocation is
//! checked against [`limits::Limits`] before it happens. No parser panics, runs
//! unbounded recursion, or loops forever on hostile input.

mod bytes;

pub mod compare;
pub mod error;
pub mod limits;
pub mod media;
pub mod record;
pub mod registry;
pub mod version;

pub use compare::{
    AlignOptions, DiffCounts, Importance, RecordDiff, RecordRow, RowKind, Status, TreeDiff,
};
pub use error::{RecordError, Result};
pub use limits::Limits;
pub use record::{ByteRange, Record, RecordTree, RecordValue};
