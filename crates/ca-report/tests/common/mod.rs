//! Fixtures the integration tests share.

#![allow(dead_code)]

use ca_report::input::{
    CellStatus, EntryStatus, FolderRow, HexRow, Importance, PictureFacts, PictureSide, PixelTotals,
    RecordRow, RowKind, SideFacts, Span, TableCell, TableHeader, TableRow, TextCell, TextRow,
};
use ca_report::options::ReportMeta;

/// Heading of every golden report.
#[must_use]
pub fn meta() -> ReportMeta {
    ReportMeta::new("left", "right").with_title("Golden")
}

/// Six text rows covering each kind and both importances.
#[must_use]
pub fn text_rows() -> Vec<TextRow> {
    vec![
        TextRow {
            kind: RowKind::Same,
            importance: None,
            left: Some(TextCell::new(1, "the first line")),
            right: Some(TextCell::new(1, "the first line")),
        },
        TextRow {
            kind: RowKind::Changed,
            importance: Some(Importance::Important),
            left: Some(TextCell {
                number: 2,
                text: "value = 10".into(),
                spans: vec![Span { start: 8, end: 10 }],
            }),
            right: Some(TextCell {
                number: 2,
                text: "value = 20".into(),
                spans: vec![Span { start: 8, end: 10 }],
            }),
        },
        TextRow {
            kind: RowKind::Changed,
            importance: Some(Importance::Unimportant),
            left: Some(TextCell::new(3, "  indented")),
            right: Some(TextCell::new(3, "indented")),
        },
        TextRow {
            kind: RowKind::Same,
            importance: None,
            left: Some(TextCell::new(4, "shared")),
            right: Some(TextCell::new(4, "shared")),
        },
        TextRow {
            kind: RowKind::LeftOnly,
            importance: Some(Importance::Important),
            left: Some(TextCell::new(5, "removed")),
            right: None,
        },
        TextRow {
            kind: RowKind::RightOnly,
            importance: Some(Importance::Important),
            left: None,
            right: Some(TextCell::new(5, "added")),
        },
    ]
}

fn facts(size: u64, stamp: &str) -> SideFacts {
    SideFacts {
        present: true,
        size: Some(size),
        timestamp: Some(stamp.into()),
        crc: Some("0BAD1DEA".into()),
        attributes: Some("RA".into()),
        ..SideFacts::default()
    }
}

/// Four folder rows covering a folder, a match, a newer side and an orphan.
#[must_use]
pub fn folder_rows() -> Vec<FolderRow> {
    vec![
        FolderRow {
            depth: 0,
            name: "src".into(),
            relative_path: "src".into(),
            is_dir: true,
            status: EntryStatus::Different,
            left: facts(0, "2024-01-01 00:00:00"),
            right: facts(0, "2024-01-01 00:00:00"),
            link: None,
        },
        FolderRow {
            depth: 1,
            name: "same.txt".into(),
            relative_path: "src/same.txt".into(),
            is_dir: false,
            status: EntryStatus::Same,
            left: facts(120, "2024-01-01 00:00:00"),
            right: facts(120, "2024-01-01 00:00:00"),
            link: Some("same.html".into()),
        },
        FolderRow {
            depth: 1,
            name: "newer.txt".into(),
            relative_path: "src/newer.txt".into(),
            is_dir: false,
            status: EntryStatus::LeftNewer,
            left: facts(240, "2024-03-01 00:00:00"),
            right: facts(200, "2024-01-01 00:00:00"),
            link: Some("newer.html".into()),
        },
        FolderRow {
            depth: 1,
            name: "only-left.txt".into(),
            relative_path: "src/only-left.txt".into(),
            is_dir: false,
            status: EntryStatus::LeftOrphan,
            left: facts(9, "2024-01-01 00:00:00"),
            right: SideFacts::absent(),
            link: None,
        },
    ]
}

/// Three hex rows of four bytes each.
#[must_use]
pub fn hex_rows() -> Vec<HexRow> {
    vec![
        HexRow {
            kind: RowKind::Same,
            left_offset: Some(0),
            right_offset: Some(0),
            left: b"HEAD".to_vec(),
            right: b"HEAD".to_vec(),
        },
        HexRow {
            kind: RowKind::Changed,
            left_offset: Some(4),
            right_offset: Some(4),
            left: vec![0x00, 0x01, 0x02, 0x03],
            right: vec![0x00, 0xff, 0x02, 0x03],
        },
        HexRow {
            kind: RowKind::LeftOnly,
            left_offset: Some(8),
            right_offset: None,
            left: b"TAIL".to_vec(),
            right: Vec::new(),
        },
    ]
}

/// The header of the golden table comparison.
#[must_use]
pub fn table_header() -> TableHeader {
    TableHeader {
        sheet: Some("Orders".into()),
        columns: vec!["id".into(), "customer".into(), "total".into()],
    }
}

/// Three table rows covering a match, a change and an orphan.
#[must_use]
pub fn table_rows() -> Vec<TableRow> {
    vec![
        TableRow {
            left_number: Some(1),
            right_number: Some(1),
            kind: RowKind::Same,
            importance: None,
            cells: vec![
                TableCell {
                    status: CellStatus::Same,
                    left: "1".into(),
                    right: "1".into(),
                },
                TableCell {
                    status: CellStatus::Same,
                    left: "ann".into(),
                    right: "ann".into(),
                },
                TableCell {
                    status: CellStatus::Same,
                    left: "10.00".into(),
                    right: "10.00".into(),
                },
            ],
        },
        TableRow {
            left_number: Some(2),
            right_number: Some(2),
            kind: RowKind::Changed,
            importance: Some(Importance::Important),
            cells: vec![
                TableCell {
                    status: CellStatus::Same,
                    left: "2".into(),
                    right: "2".into(),
                },
                TableCell {
                    status: CellStatus::Same,
                    left: "bob".into(),
                    right: "bob".into(),
                },
                TableCell {
                    status: CellStatus::Different,
                    left: "20.00".into(),
                    right: "25.00".into(),
                },
            ],
        },
        TableRow {
            left_number: None,
            right_number: Some(3),
            kind: RowKind::RightOnly,
            importance: Some(Importance::Important),
            cells: vec![
                TableCell {
                    status: CellStatus::RightOnly,
                    left: String::new(),
                    right: "3".into(),
                },
                TableCell {
                    status: CellStatus::RightOnly,
                    left: String::new(),
                    right: "cid".into(),
                },
                TableCell {
                    status: CellStatus::RightOnly,
                    left: String::new(),
                    right: "30.00".into(),
                },
            ],
        },
    ]
}

/// The golden picture comparison.
#[must_use]
pub fn picture_facts() -> PictureFacts {
    PictureFacts {
        left: PictureSide {
            width: 32,
            height: 32,
            format: "png".into(),
            bits_per_channel: 8,
            metadata: vec![("author".into(), "left".into())],
            ..PictureSide::default()
        },
        right: PictureSide {
            width: 32,
            height: 40,
            format: "png".into(),
            bits_per_channel: 16,
            precision_reduced: true,
            icc_profile: true,
            metadata: vec![("author".into(), "right".into())],
            ..PictureSide::default()
        },
        totals: PixelTotals {
            same: 900,
            similar: 24,
            different: 100,
            left_only: 0,
            right_only: 256,
        },
        tolerance: 4,
        difference_png: None,
    }
}

/// Three records covering a match, a change and an orphan.
#[must_use]
pub fn record_rows() -> Vec<RecordRow> {
    vec![
        RecordRow {
            name: "CompanyName".into(),
            group: Some("StringFileInfo".into()),
            kind: RowKind::Same,
            importance: None,
            left: Some("acme".into()),
            right: Some("acme".into()),
        },
        RecordRow {
            name: "FileVersion".into(),
            group: Some("StringFileInfo".into()),
            kind: RowKind::Changed,
            importance: Some(Importance::Important),
            left: Some("1.0.0.0".into()),
            right: Some("1.1.0.0".into()),
        },
        RecordRow {
            name: "Comments".into(),
            group: Some("StringFileInfo".into()),
            kind: RowKind::RightOnly,
            importance: Some(Importance::Important),
            left: None,
            right: Some("built later".into()),
        },
    ]
}
