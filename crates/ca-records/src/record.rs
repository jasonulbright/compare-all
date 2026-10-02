//! The output shape every engine in this crate produces.
//!
//! Registry keys, version resource blocks and media tag containers all reduce
//! to the same thing: a tree of groups, each holding named values that carry a
//! type, a display form and, where the source is a file, the byte range the
//! value came from. One renderer and one comparison therefore serve all three.

use crate::bytes::hex_display;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Fields written by another build, preserved verbatim.
pub type Unknown = BTreeMap<String, serde_json::Value>;

/// Longest binary payload rendered byte by byte in a display form.
const HEX_DISPLAY_LIMIT: usize = 64;

/// A half open byte range in the source the record came from.
///
/// For a file source the range indexes the file. For a `.reg` file the range
/// indexes the decoded text, which equals the file offset only when the file
/// used a byte oriented encoding with no byte order mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ByteRange {
    /// First byte of the range.
    pub start: u64,
    /// Length of the range in bytes.
    pub len: u64,
}

impl ByteRange {
    /// Build a range from a start and a length.
    #[must_use]
    pub const fn new(start: u64, len: u64) -> Self {
        Self { start, len }
    }

    /// One past the last byte of the range.
    #[must_use]
    pub const fn end(self) -> u64 {
        self.start.saturating_add(self.len)
    }

    /// True when the range covers no bytes.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }
}

/// The typed data of one record.
///
/// The type name on [`Record`] keeps the source's own vocabulary, for example
/// `REG_DWORD` or `TPE1`. This enum is the shape a comparison reasons over.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RecordValue {
    /// The record exists but carries no data.
    #[default]
    Empty,
    /// Text.
    Text(String),
    /// A list of strings, in source order.
    TextList(Vec<String>),
    /// A whole number.
    Integer(i128),
    /// Opaque bytes.
    Bytes(Vec<u8>),
}

impl RecordValue {
    /// A single line rendering of the value.
    ///
    /// Binary data is abbreviated after a fixed number of bytes, so a hostile
    /// payload cannot force a huge string.
    #[must_use]
    pub fn to_display(&self) -> String {
        match self {
            Self::Empty => String::new(),
            Self::Text(text) => text.clone(),
            Self::TextList(items) => items.join(" | "),
            Self::Integer(value) => value.to_string(),
            Self::Bytes(bytes) => hex_display(bytes, HEX_DISPLAY_LIMIT),
        }
    }

    /// Number of bytes the value holds, for binary data only.
    #[must_use]
    pub fn byte_len(&self) -> Option<usize> {
        match self {
            Self::Bytes(bytes) => Some(bytes.len()),
            _ => None,
        }
    }

    /// True when the value is binary data a viewer can open on its own.
    #[must_use]
    pub const fn is_binary(&self) -> bool {
        matches!(self, Self::Bytes(_))
    }
}

/// One named value inside a group.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Record {
    /// Path of the group that holds the record.
    pub path: String,
    /// Name of the record inside its group.
    pub name: String,
    /// Type name in the source's own vocabulary.
    pub type_name: String,
    /// Typed data.
    pub value: RecordValue,
    /// Single line rendering of the data.
    pub display: String,
    /// Byte range of the record in its source, where the source has one.
    pub source: Option<ByteRange>,
}

impl Record {
    /// Build a record and derive its display form from the value.
    #[must_use]
    pub fn new(
        path: impl Into<String>,
        name: impl Into<String>,
        type_name: impl Into<String>,
        value: RecordValue,
    ) -> Self {
        let display = value.to_display();
        Self {
            path: path.into(),
            name: name.into(),
            type_name: type_name.into(),
            value,
            display,
            source: None,
        }
    }

    /// Replace the derived display form.
    #[must_use]
    pub fn with_display(mut self, display: impl Into<String>) -> Self {
        self.display = display.into();
        self
    }

    /// Attach the byte range the record was read from.
    #[must_use]
    pub const fn with_source(mut self, source: ByteRange) -> Self {
        self.source = Some(source);
        self
    }

    /// Full name of the record, group path first.
    #[must_use]
    pub fn qualified_name(&self) -> String {
        if self.path.is_empty() {
            return self.name.clone();
        }
        format!("{}\\{}", self.path, self.name)
    }
}

/// A group of records, with child groups under it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecordTree {
    /// Full path of this group from the root of the source.
    pub path: String,
    /// Name of this group inside its parent.
    pub name: String,
    /// Records this group holds.
    pub records: Vec<Record>,
    /// Groups nested under this one.
    pub children: Vec<RecordTree>,
    /// Byte range of the group in its source, where the source has one.
    pub source: Option<ByteRange>,
}

impl RecordTree {
    /// Build an empty group.
    #[must_use]
    pub fn new(path: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            name: name.into(),
            records: Vec::new(),
            children: Vec::new(),
            source: None,
        }
    }

    /// Add a record to this group.
    pub fn push(&mut self, record: Record) {
        self.records.push(record);
    }

    /// Add a child group.
    pub fn push_child(&mut self, child: Self) {
        self.children.push(child);
    }

    /// Find a child group by name, ignoring case.
    #[must_use]
    pub fn child(&self, name: &str) -> Option<&Self> {
        self.children
            .iter()
            .find(|child| child.name.eq_ignore_ascii_case(name))
    }

    /// Find a record by name, ignoring case.
    #[must_use]
    pub fn record(&self, name: &str) -> Option<&Record> {
        self.records
            .iter()
            .find(|record| record.name.eq_ignore_ascii_case(name))
    }

    /// Total number of records in this group and every group under it.
    #[must_use]
    pub fn record_count(&self) -> usize {
        let mut total = 0usize;
        let mut stack = vec![self];
        while let Some(node) = stack.pop() {
            total = total.saturating_add(node.records.len());
            stack.extend(node.children.iter());
        }
        total
    }

    /// Sort records and child groups by name, ignoring case.
    ///
    /// Sorting is iterative, so a deep tree cannot overflow the stack.
    pub fn sort_by_name(&mut self) {
        let mut stack = vec![self];
        while let Some(node) = stack.pop() {
            node.records.sort_by(|a, b| compare_names(&a.name, &b.name));
            node.children
                .sort_by(|a, b| compare_names(&a.name, &b.name));
            stack.extend(node.children.iter_mut());
        }
    }
}

/// Order two names the way a record listing shows them: case insensitive
/// first, then case sensitive so the order is total.
pub(crate) fn compare_names(left: &str, right: &str) -> std::cmp::Ordering {
    let folded = left
        .chars()
        .flat_map(char::to_lowercase)
        .cmp(right.chars().flat_map(char::to_lowercase));
    folded.then_with(|| left.cmp(right))
}
