//! Every report stops when the caller raises the flag.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use ca_report::input::{EntryStatus, FolderRow, HexRow, RowKind, SideFacts, TextCell, TextRow};
use ca_report::options::{
    FolderLayout, FolderReportOptions, HexReportOptions, OutputOptions, PairLayout,
    RecordReportOptions, ReportMeta, TableReportOptions, TextLayout, TextReportOptions,
};
use ca_report::record::RecordKind;
use ca_report::{AtomicCancel, Cancel, ReportError};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Rows the cancellation tests feed. The flag rises part way through.
const ROWS: u64 = 20_000;

/// Rows produced before the flag rises.
const RAISE_AFTER: u64 = 1_000;

/// A flag a test raises after a fixed count of rows.
struct RaiseAfter {
    produced: Rc<Cell<u64>>,
}

impl Cancel for RaiseAfter {
    fn is_cancelled(&self) -> bool {
        self.produced.get() >= RAISE_AFTER
    }
}

fn text_rows(produced: &Rc<Cell<u64>>) -> impl Iterator<Item = TextRow> + use<> {
    let produced = Rc::clone(produced);
    (0..ROWS).map(move |index| {
        produced.set(index + 1);
        TextRow {
            kind: RowKind::Same,
            importance: None,
            left: Some(TextCell::new(index + 1, "line")),
            right: Some(TextCell::new(index + 1, "line")),
        }
    })
}

fn assert_cancelled(result: &Result<(), ReportError>) {
    assert!(
        matches!(result, Err(ReportError::Cancelled)),
        "the report did not stop"
    );
}

#[test]
fn every_text_layout_stops() {
    for layout in [
        TextLayout::SideBySide,
        TextLayout::Interleaved,
        TextLayout::Summary,
        TextLayout::Statistics,
        TextLayout::Patch,
        TextLayout::Xml,
    ] {
        let produced = Rc::new(Cell::new(0u64));
        let cancel = RaiseAfter {
            produced: Rc::clone(&produced),
        };
        let options = TextReportOptions {
            layout: layout.clone(),
            ..TextReportOptions::default()
        };
        let mut out = Vec::new();
        let result = ca_report::write_text_report(
            &mut out,
            &ReportMeta::new("a", "b"),
            &options,
            &OutputOptions::plain_text(),
            text_rows(&produced),
            &cancel,
        );
        assert_cancelled(&result);
        assert!(
            produced.get() < ROWS,
            "{layout:?} read the whole input after the flag rose"
        );
    }
}

#[test]
fn a_stopped_report_leaves_a_partial_document() {
    let produced = Rc::new(Cell::new(0u64));
    let cancel = RaiseAfter {
        produced: Rc::clone(&produced),
    };
    let mut out = Vec::new();
    let result = ca_report::write_text_report(
        &mut out,
        &ReportMeta::new("a", "b"),
        &TextReportOptions::default(),
        &OutputOptions::html_color(),
        text_rows(&produced),
        &cancel,
    );
    assert_cancelled(&result);
    let html = String::from_utf8(out).expect("utf-8");
    assert!(html.contains("<table>"), "the heading was written");
    assert!(!html.contains("</html>"), "the document was not closed");
}

#[test]
fn every_folder_layout_stops() {
    for layout in [
        FolderLayout::SideBySide,
        FolderLayout::Summary,
        FolderLayout::Xml,
    ] {
        let produced = Rc::new(Cell::new(0u64));
        let counter = Rc::clone(&produced);
        let cancel = RaiseAfter {
            produced: Rc::clone(&produced),
        };
        let rows = (0..ROWS).map(move |index| {
            counter.set(index + 1);
            FolderRow {
                depth: 0,
                name: format!("file-{index}"),
                relative_path: format!("file-{index}"),
                is_dir: false,
                status: EntryStatus::Different,
                left: SideFacts {
                    present: true,
                    size: Some(1),
                    ..SideFacts::default()
                },
                right: SideFacts::absent(),
                link: None,
            }
        });
        let options = FolderReportOptions {
            layout: layout.clone(),
            ..FolderReportOptions::default()
        };
        let mut out = Vec::new();
        let result = ca_report::write_folder_report(
            &mut out,
            &ReportMeta::new("a", "b"),
            &options,
            &OutputOptions::plain_text(),
            rows,
            &cancel,
        );
        assert_cancelled(&result);
    }
}

#[test]
fn the_hex_table_and_record_reports_stop() {
    let produced = Rc::new(Cell::new(0u64));
    let counter = Rc::clone(&produced);
    let cancel = RaiseAfter {
        produced: Rc::clone(&produced),
    };
    let rows = (0..ROWS).map(move |index| {
        counter.set(index + 1);
        HexRow {
            kind: RowKind::Changed,
            left_offset: Some(index * 4),
            right_offset: Some(index * 4),
            left: vec![1, 2, 3, 4],
            right: vec![4, 3, 2, 1],
        }
    });
    let mut out = Vec::new();
    assert_cancelled(&ca_report::write_hex_report(
        &mut out,
        &ReportMeta::new("a", "b"),
        &HexReportOptions::default(),
        &OutputOptions::plain_text(),
        rows,
        &cancel,
    ));

    let produced = Rc::new(Cell::new(0u64));
    let counter = Rc::clone(&produced);
    let cancel = RaiseAfter {
        produced: Rc::clone(&produced),
    };
    let base = common::table_rows();
    let rows = (0..ROWS).map(move |index| {
        counter.set(index + 1);
        base[usize::try_from(index).unwrap_or(0) % base.len()].clone()
    });
    let mut out = Vec::new();
    assert_cancelled(&ca_report::write_table_report(
        &mut out,
        &ReportMeta::new("a", "b"),
        &common::table_header(),
        &TableReportOptions::default(),
        &OutputOptions::plain_text(),
        rows,
        &cancel,
    ));

    let produced = Rc::new(Cell::new(0u64));
    let counter = Rc::clone(&produced);
    let cancel = RaiseAfter {
        produced: Rc::clone(&produced),
    };
    let base = common::record_rows();
    let rows = (0..ROWS).map(move |index| {
        counter.set(index + 1);
        base[usize::try_from(index).unwrap_or(0) % base.len()].clone()
    });
    let mut out = Vec::new();
    assert_cancelled(&ca_report::write_record_report(
        &mut out,
        &ReportMeta::new("a", "b"),
        RecordKind::Media,
        &RecordReportOptions::default(),
        &OutputOptions::plain_text(),
        rows,
        &cancel,
    ));
}

#[test]
fn a_picture_report_stops_before_it_writes() {
    let flag = AtomicBool::new(true);
    flag.store(true, Ordering::Relaxed);
    let mut out = Vec::new();
    let options = ca_report::options::PictureReportOptions {
        layout: PairLayout::SideBySide,
        ..ca_report::options::PictureReportOptions::default()
    };
    assert_cancelled(&ca_report::write_picture_report(
        &mut out,
        &ReportMeta::new("a", "b"),
        &options,
        &OutputOptions::html_color(),
        &common::picture_facts(),
        &AtomicCancel::new(&flag),
    ));
    assert!(out.is_empty());
}

#[test]
fn a_clear_flag_lets_the_report_finish() {
    let flag = AtomicBool::new(false);
    let mut out = Vec::new();
    ca_report::write_text_report(
        &mut out,
        &ReportMeta::new("a", "b"),
        &TextReportOptions::default(),
        &OutputOptions::html_color(),
        common::text_rows(),
        &AtomicCancel::new(&flag),
    )
    .expect("render");
    assert!(String::from_utf8(out)
        .expect("utf-8")
        .ends_with("</html>\n"));
}
