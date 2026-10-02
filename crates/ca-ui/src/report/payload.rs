//! What a view hands the report engine.
//!
//! The rows of a text or hex comparison name positions in the buffers the view
//! already holds, and those buffers are shared by reference count. Building a
//! report therefore costs one small vector per comparison, never a second copy
//! of either file.

use ca_report::input::{
    FolderRow, HexRow, Importance, PictureFacts, RecordRow, RowKind, TableHeader, TableRow,
    TextCell, TextRow,
};
use ca_report::RecordKind;
use std::sync::Arc;

/// One row of a text comparison, as two line numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextRowRef {
    /// Classification of the row.
    pub kind: RowKind,
    /// Importance of the row. `None` on a row both sides share.
    pub importance: Option<Importance>,
    /// Zero based left line number, absent on a right-only row.
    pub left: Option<u32>,
    /// Zero based right line number, absent on a left-only row.
    pub right: Option<u32>,
}

/// A text comparison as the view displays it.
#[derive(Debug, Clone, Default)]
pub struct TextPayload {
    /// The left file's lines, shared with the view.
    pub left: Arc<Vec<String>>,
    /// The right file's lines, shared with the view.
    pub right: Arc<Vec<String>>,
    /// The rows the view displays, in display order.
    pub rows: Vec<TextRowRef>,
}

impl TextPayload {
    /// The row at `index` as the report engine reads it.
    ///
    /// The two line strings are copied here and nowhere else, one row at a
    /// time, so the document streams without a second copy of either file.
    #[must_use]
    pub fn row(&self, index: usize) -> Option<TextRow> {
        let held = self.rows.get(index)?;
        Some(TextRow {
            kind: held.kind,
            importance: held.importance,
            left: held.left.map(|line| cell(&self.left, line)),
            right: held.right.map(|line| cell(&self.right, line)),
        })
    }

    /// Every row, one at a time.
    pub fn iter(&self) -> impl Iterator<Item = TextRow> + '_ {
        (0..self.rows.len()).filter_map(|index| self.row(index))
    }
}

/// One line as a report cell.
///
/// A view may hold its lines with their terminators. A report row carries the
/// content alone, so the terminator is taken off here rather than in each view.
fn cell(lines: &[String], line: u32) -> TextCell {
    let text = lines
        .get(line as usize)
        .map(|held| held.trim_end_matches(['\n', '\r']).to_owned())
        .unwrap_or_default();
    TextCell::new(u64::from(line) + 1, text)
}

/// A hex comparison as the view displays it.
#[derive(Debug, Clone, Default)]
pub struct HexPayload {
    /// The left file's bytes, shared with the view.
    pub left: Arc<Vec<u8>>,
    /// The right file's bytes, shared with the view.
    pub right: Arc<Vec<u8>>,
    /// The byte hunks the comparison produced.
    pub hunks: Arc<Vec<ca_diff::ByteHunk>>,
}

impl HexPayload {
    /// Every row, one hunk's worth at a time.
    #[must_use]
    pub fn rows(&self, bytes_per_row: u32) -> Vec<HexRow> {
        let mut rows = Vec::new();
        for hunk in self.hunks.iter() {
            rows.extend(ca_report::input::hex_rows(
                std::slice::from_ref(hunk),
                &self.left,
                &self.right,
                bytes_per_row,
            ));
        }
        rows
    }
}

/// The comparison a report is written from.
#[derive(Debug, Clone)]
pub enum Payload {
    /// A text or merge comparison.
    Text(TextPayload),
    /// A folder comparison.
    Folder(Vec<FolderRow>),
    /// A hex comparison.
    Hex(HexPayload),
    /// A table comparison.
    Table(TableHeader, Vec<TableRow>),
    /// A picture comparison.
    Picture(Box<PictureFacts>),
    /// A registry, version or media comparison.
    Record(RecordKind, Vec<RecordRow>),
}

impl Payload {
    /// How many rows the report will carry, for the summary the dialog shows.
    #[must_use]
    pub fn row_count(&self) -> usize {
        match self {
            Self::Text(text) => text.rows.len(),
            Self::Folder(rows) => rows.len(),
            Self::Hex(hex) => hex.hunks.len(),
            Self::Table(_, rows) => rows.len(),
            Self::Picture(_) => 1,
            Self::Record(_, rows) => rows.len(),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::{HexPayload, Payload, TextPayload, TextRowRef};
    use ca_report::input::{Importance, RowKind};
    use std::sync::Arc;

    fn text() -> TextPayload {
        TextPayload {
            left: Arc::new(vec!["one".to_owned(), "two".to_owned()]),
            right: Arc::new(vec!["one".to_owned(), "TWO".to_owned()]),
            rows: vec![
                TextRowRef {
                    kind: RowKind::Same,
                    importance: None,
                    left: Some(0),
                    right: Some(0),
                },
                TextRowRef {
                    kind: RowKind::Changed,
                    importance: Some(Importance::Important),
                    left: Some(1),
                    right: Some(1),
                },
            ],
        }
    }

    #[test]
    fn a_row_carries_one_based_line_numbers() {
        let row = text().row(1).unwrap();
        assert_eq!(row.left.unwrap().number, 2);
        assert_eq!(row.right.unwrap().text, "TWO");
        assert_eq!(row.kind, RowKind::Changed);
    }

    #[test]
    fn a_row_past_the_end_is_absent() {
        assert!(text().row(9).is_none());
    }

    #[test]
    fn hex_rows_cut_on_the_requested_width() {
        let payload = HexPayload {
            left: Arc::new(vec![1, 2, 3, 4]),
            right: Arc::new(vec![9, 9, 9, 9]),
            hunks: Arc::new(vec![ca_diff::ByteHunk {
                kind: ca_diff::HunkKind::Changed,
                left: 0..4,
                right: 0..4,
            }]),
        };
        assert_eq!(payload.rows(2).len(), 2);
    }

    #[test]
    fn a_payload_reports_how_many_rows_it_carries() {
        assert_eq!(Payload::Text(text()).row_count(), 2);
        assert_eq!(Payload::Folder(Vec::new()).row_count(), 0);
    }
}
