//! The reports stream: the writer sees bytes long before the input ends.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ca_report::input::{EntryStatus, FolderRow, Importance, RowKind, SideFacts, TextCell, TextRow};
use ca_report::options::{
    FolderReportOptions, OutputOptions, ReportMeta, TextDisplayFilter, TextLayout,
    TextReportOptions,
};
use ca_report::NeverCancel;
use std::cell::Cell;
use std::io::Write;
use std::rc::Rc;

/// Rows one streaming test feeds. The count is a work count, not a time.
const ROWS: u64 = 200_000;

/// Bytes the sink must hold before it records how far the input had run.
const SAMPLE_AFTER: u64 = 64 * 1024;

/// A writer that counts bytes and records how many rows the input had produced
/// when the count first passed [`SAMPLE_AFTER`].
struct CountingSink {
    bytes: u64,
    produced: Rc<Cell<u64>>,
    produced_at_sample: Option<u64>,
}

impl CountingSink {
    fn new(produced: Rc<Cell<u64>>) -> Self {
        Self {
            bytes: 0,
            produced,
            produced_at_sample: None,
        }
    }
}

impl Write for CountingSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.bytes += buf.len() as u64;
        if self.produced_at_sample.is_none() && self.bytes >= SAMPLE_AFTER {
            self.produced_at_sample = Some(self.produced.get());
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn counting_text_rows(produced: &Rc<Cell<u64>>) -> impl Iterator<Item = TextRow> + use<> {
    let produced = Rc::clone(produced);
    (0..ROWS).map(move |index| {
        produced.set(index + 1);
        let kind = if index % 7 == 0 {
            RowKind::Changed
        } else {
            RowKind::Same
        };
        TextRow {
            kind,
            importance: (kind == RowKind::Changed).then_some(Importance::Important),
            left: Some(TextCell::new(index + 1, format!("left line {index}"))),
            right: Some(TextCell::new(index + 1, format!("right line {index}"))),
        }
    })
}

fn assert_streamed(sink: &CountingSink) {
    let produced = sink
        .produced_at_sample
        .expect("the writer received the sample before the report ended");
    assert!(
        produced < ROWS,
        "the writer saw {SAMPLE_AFTER} bytes only after the input was exhausted"
    );
    assert!(
        produced * 4 < ROWS,
        "the writer waited for {produced} of {ROWS} rows before the first bytes"
    );
}

#[test]
fn a_long_text_report_reaches_the_writer_before_the_input_ends() {
    let produced = Rc::new(Cell::new(0u64));
    let mut sink = CountingSink::new(Rc::clone(&produced));
    ca_report::write_text_report(
        &mut sink,
        &ReportMeta::new("left", "right"),
        &TextReportOptions::default(),
        &OutputOptions::html_color(),
        counting_text_rows(&produced),
        &NeverCancel,
    )
    .expect("render");
    assert_streamed(&sink);
}

#[test]
fn a_context_filtered_report_still_streams() {
    let produced = Rc::new(Cell::new(0u64));
    let mut sink = CountingSink::new(Rc::clone(&produced));
    let options = TextReportOptions {
        display: TextDisplayFilter::Context,
        context_lines: 2,
        ..TextReportOptions::default()
    };
    ca_report::write_text_report(
        &mut sink,
        &ReportMeta::new("left", "right"),
        &options,
        &OutputOptions::plain_text(),
        counting_text_rows(&produced),
        &NeverCancel,
    )
    .expect("render");
    assert_streamed(&sink);
}

#[test]
fn a_long_patch_reaches_the_writer_before_the_input_ends() {
    let produced = Rc::new(Cell::new(0u64));
    let mut sink = CountingSink::new(Rc::clone(&produced));
    let options = TextReportOptions {
        layout: TextLayout::Patch,
        context_lines: 3,
        ..TextReportOptions::default()
    };
    ca_report::write_text_report(
        &mut sink,
        &ReportMeta::new("left", "right"),
        &options,
        &OutputOptions::plain_text(),
        counting_text_rows(&produced),
        &NeverCancel,
    )
    .expect("render");
    assert_streamed(&sink);
}

#[test]
fn a_long_folder_report_reaches_the_writer_before_the_input_ends() {
    let produced = Rc::new(Cell::new(0u64));
    let counter = Rc::clone(&produced);
    let mut sink = CountingSink::new(Rc::clone(&produced));
    let rows = (0..ROWS).map(move |index| {
        counter.set(index + 1);
        FolderRow {
            depth: 1,
            name: format!("file-{index}.txt"),
            relative_path: format!("src/file-{index}.txt"),
            is_dir: false,
            status: if index % 3 == 0 {
                EntryStatus::Different
            } else {
                EntryStatus::Same
            },
            left: SideFacts {
                present: true,
                size: Some(index),
                timestamp: Some("2024-01-01 00:00:00".into()),
                ..SideFacts::default()
            },
            right: SideFacts {
                present: true,
                size: Some(index),
                timestamp: Some("2024-01-01 00:00:00".into()),
                ..SideFacts::default()
            },
            link: None,
        }
    });
    ca_report::write_folder_report(
        &mut sink,
        &ReportMeta::new("left", "right"),
        &FolderReportOptions::default(),
        &OutputOptions::html_color(),
        rows,
        &NeverCancel,
    )
    .expect("render");
    assert_streamed(&sink);
}
