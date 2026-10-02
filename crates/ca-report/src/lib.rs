//! Report engine: comparison results as HTML, text, XML, and CSV.
//!
//! The crate turns the result of a comparison into a document a person reads
//! later or a tool consumes. It draws nothing and owns no window.
//!
//! # Shape
//!
//! - [`input`] holds the report-side input types. An engine result converts
//!   into them; the report reads nothing else.
//! - [`options`] holds the layouts and options, one type per report command.
//! - [`palette`] holds the colors an HTML report paints with.
//! - [`escape`] escapes every value for the format that carries it.
//! - [`text`], [`folder`], [`hex`], [`table`], [`picture`] and [`record`] write
//!   one report each.
//!
//! # Streaming
//!
//! Every report takes its rows as an iterator and writes straight to a
//! [`std::io::Write`]. The memory a report holds is a small window: the row it
//! is writing, plus the rows a context filter holds back, plus the rows of one
//! patch hunk. Nothing collects the whole document.
//!
//! # Cancellation
//!
//! Every loop whose length grows with the input polls a [`cancel::Cancel`] flag
//! and returns [`error::ReportError::Cancelled`]. The partial document on the
//! writer is the caller's to discard.
//!
//! # Determinism
//!
//! The same rows and the same options write the same bytes. No report reads a
//! clock, a locale or an environment variable; a time stamp appears only when
//! the caller puts one in [`options::ReportMeta`].
//!
//! # Escaping
//!
//! File names and file content are untrusted. An HTML or XML document escapes
//! the five markup characters and replaces every control code point the format
//! forbids. A comma separated document quotes every field, doubles embedded
//! quotes and guards a leading formula character.

pub mod cancel;
mod doc;
pub mod error;
pub mod escape;
pub mod folder;
mod gate;
pub mod hex;
pub mod input;
pub mod options;
pub mod palette;
mod patch;
pub mod picture;
mod plain;
pub mod record;
pub mod table;
pub mod text;

pub use cancel::{AtomicCancel, Cancel, NeverCancel};
pub use error::{ReportError, Result};
pub use folder::{write_folder_report, FolderCounts};
pub use hex::{write_hex_report, HexCounts};
pub use options::{
    DisplayFilter, FolderColumn, FolderColumns, FolderDisplayFilter, FolderLayout,
    FolderReportOptions, HexLayout, HexReportOptions, HtmlScheme, OutputFormat, OutputOptions,
    PageOrientation, PairLayout, PatchFormat, PictureReportOptions, PrintOptions, PrintScheme,
    RecordReportOptions, ReportMeta, TableLayout, TableReportOptions, TextDisplayFilter,
    TextLayout, TextReportOptions, Wrap,
};
pub use palette::{Color, ReportPalette};
pub use picture::write_picture_report;
pub use record::{write_record_report, RecordCounts, RecordKind};
pub use table::{write_table_report, TableCounts};
pub use text::{write_text_report, TextCounts};
