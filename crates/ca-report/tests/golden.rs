//! Golden documents, one per format and layout.
//!
//! Set `CA_REPORT_BLESS=1` to write the files instead of comparing them. Read
//! the difference before blessing: a changed golden file is a changed document
//! for every caller.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use ca_report::input::{FolderRow, HexRow, RecordRow, TableRow, TextRow};
use ca_report::options::{
    DisplayFilter, FolderColumns, FolderLayout, FolderReportOptions, HexLayout, HexReportOptions,
    OutputOptions, PairLayout, PatchFormat, PictureReportOptions, RecordReportOptions, TableLayout,
    TableReportOptions, TextLayout, TextReportOptions,
};
use ca_report::record::RecordKind;
use ca_report::NeverCancel;
use std::path::PathBuf;

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
        .join(name)
}

fn check(name: &str, produced: &[u8]) {
    let path = golden_path(name);
    let produced = String::from_utf8(produced.to_vec()).expect("a report is valid utf-8");
    if std::env::var_os("CA_REPORT_BLESS").is_some() {
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("create the folder");
        std::fs::write(&path, produced.as_bytes()).expect("write the golden file");
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} is missing: {error}", path.display()));
    assert_eq!(
        produced,
        expected.replace("\r\n", "\n"),
        "{name} changed; run with CA_REPORT_BLESS=1 after reading the difference"
    );
}

fn text(options: &TextReportOptions, output: &OutputOptions, rows: Vec<TextRow>) -> Vec<u8> {
    let mut out = Vec::new();
    ca_report::write_text_report(
        &mut out,
        &common::meta(),
        options,
        output,
        rows,
        &NeverCancel,
    )
    .expect("render");
    normalize(out)
}

fn folder(options: &FolderReportOptions, output: &OutputOptions, rows: Vec<FolderRow>) -> Vec<u8> {
    let mut out = Vec::new();
    ca_report::write_folder_report(
        &mut out,
        &common::meta(),
        options,
        output,
        rows,
        &NeverCancel,
    )
    .expect("render");
    normalize(out)
}

fn hex(options: &HexReportOptions, output: &OutputOptions, rows: Vec<HexRow>) -> Vec<u8> {
    let mut out = Vec::new();
    ca_report::write_hex_report(
        &mut out,
        &common::meta(),
        options,
        output,
        rows,
        &NeverCancel,
    )
    .expect("render");
    normalize(out)
}

fn table(options: &TableReportOptions, output: &OutputOptions, rows: Vec<TableRow>) -> Vec<u8> {
    let mut out = Vec::new();
    ca_report::write_table_report(
        &mut out,
        &common::meta(),
        &common::table_header(),
        options,
        output,
        rows,
        &NeverCancel,
    )
    .expect("render");
    normalize(out)
}

fn record(
    kind: RecordKind,
    options: &RecordReportOptions,
    output: &OutputOptions,
    rows: Vec<RecordRow>,
) -> Vec<u8> {
    let mut out = Vec::new();
    ca_report::write_record_report(
        &mut out,
        &common::meta(),
        kind,
        options,
        output,
        rows,
        &NeverCancel,
    )
    .expect("render");
    normalize(out)
}

/// A golden file is stored with one line ending, so the comma separated
/// documents, which carry their own pair, are folded before comparison.
fn normalize(bytes: Vec<u8>) -> Vec<u8> {
    String::from_utf8(bytes)
        .expect("utf-8")
        .replace("\r\n", "\n")
        .into_bytes()
}

#[test]
fn text_side_by_side_html() {
    let options = TextReportOptions {
        line_numbers: true,
        ..TextReportOptions::default()
    };
    check(
        "text-side-by-side.html",
        &text(&options, &OutputOptions::html_color(), common::text_rows()),
    );
}

#[test]
fn text_side_by_side_plain() {
    let options = TextReportOptions {
        line_numbers: true,
        ..TextReportOptions::default()
    };
    check(
        "text-side-by-side.txt",
        &text(&options, &OutputOptions::plain_text(), common::text_rows()),
    );
}

#[test]
fn text_interleaved_html_with_strikeout() {
    let options = TextReportOptions {
        layout: TextLayout::Interleaved,
        strikeout_left_diffs: true,
        strikeout_right_diffs: true,
        ..TextReportOptions::default()
    };
    check(
        "text-interleaved.html",
        &text(&options, &OutputOptions::html_color(), common::text_rows()),
    );
}

#[test]
fn text_interleaved_monochrome_html() {
    let options = TextReportOptions {
        layout: TextLayout::Interleaved,
        ..TextReportOptions::default()
    };
    check(
        "text-interleaved-mono.html",
        &text(&options, &OutputOptions::html_mono(), common::text_rows()),
    );
}

#[test]
fn text_summary_plain() {
    let options = TextReportOptions {
        layout: TextLayout::Summary,
        ..TextReportOptions::default()
    };
    check(
        "text-summary.txt",
        &text(&options, &OutputOptions::plain_text(), common::text_rows()),
    );
}

#[test]
fn text_statistics_csv() {
    let options = TextReportOptions {
        layout: TextLayout::Statistics,
        ..TextReportOptions::default()
    };
    check(
        "text-statistics.csv",
        &text(&options, &OutputOptions::plain_text(), common::text_rows()),
    );
}

#[test]
fn text_xml() {
    let options = TextReportOptions {
        layout: TextLayout::Xml,
        ..TextReportOptions::default()
    };
    check(
        "text-report.xml",
        &text(&options, &OutputOptions::plain_text(), common::text_rows()),
    );
}

#[test]
fn text_patch_normal() {
    let options = TextReportOptions {
        layout: TextLayout::Patch,
        patch_format: PatchFormat::Normal,
        ..TextReportOptions::default()
    };
    check(
        "text-patch-normal.diff",
        &text(&options, &OutputOptions::plain_text(), common::text_rows()),
    );
}

#[test]
fn text_patch_context() {
    let options = TextReportOptions {
        layout: TextLayout::Patch,
        patch_format: PatchFormat::Context,
        context_lines: 1,
        ..TextReportOptions::default()
    };
    check(
        "text-patch-context.diff",
        &text(&options, &OutputOptions::plain_text(), common::text_rows()),
    );
}

#[test]
fn text_patch_unified() {
    let options = TextReportOptions {
        layout: TextLayout::Patch,
        patch_format: PatchFormat::Unified,
        context_lines: 1,
        ..TextReportOptions::default()
    };
    check(
        "text-patch-unified.diff",
        &text(&options, &OutputOptions::plain_text(), common::text_rows()),
    );
}

#[test]
fn folder_side_by_side_html() {
    let options = FolderReportOptions {
        include_file_links: true,
        ..FolderReportOptions::default()
    };
    check(
        "folder-side-by-side.html",
        &folder(
            &options,
            &OutputOptions::html_color(),
            common::folder_rows(),
        ),
    );
}

#[test]
fn folder_side_by_side_plain_with_every_column() {
    let options = FolderReportOptions {
        columns: FolderColumns {
            crc: true,
            attributes: true,
            ..FolderColumns::default()
        },
        ..FolderReportOptions::default()
    };
    check(
        "folder-side-by-side.txt",
        &folder(
            &options,
            &OutputOptions::plain_text(),
            common::folder_rows(),
        ),
    );
}

#[test]
fn folder_summary_plain() {
    let options = FolderReportOptions {
        layout: FolderLayout::Summary,
        ..FolderReportOptions::default()
    };
    check(
        "folder-summary.txt",
        &folder(
            &options,
            &OutputOptions::plain_text(),
            common::folder_rows(),
        ),
    );
}

#[test]
fn folder_xml() {
    let options = FolderReportOptions {
        layout: FolderLayout::Xml,
        ..FolderReportOptions::default()
    };
    check(
        "folder-report.xml",
        &folder(
            &options,
            &OutputOptions::plain_text(),
            common::folder_rows(),
        ),
    );
}

#[test]
fn hex_side_by_side_plain() {
    let options = HexReportOptions {
        line_numbers: true,
        bytes_per_row: 4,
        ..HexReportOptions::default()
    };
    check(
        "hex-side-by-side.txt",
        &hex(&options, &OutputOptions::plain_text(), common::hex_rows()),
    );
}

#[test]
fn hex_interleaved_html() {
    let options = HexReportOptions {
        layout: HexLayout::Interleaved,
        bytes_per_row: 4,
        ..HexReportOptions::default()
    };
    check(
        "hex-interleaved.html",
        &hex(&options, &OutputOptions::html_color(), common::hex_rows()),
    );
}

#[test]
fn hex_summary_plain() {
    let options = HexReportOptions {
        layout: HexLayout::Summary,
        bytes_per_row: 4,
        ..HexReportOptions::default()
    };
    check(
        "hex-summary.txt",
        &hex(&options, &OutputOptions::plain_text(), common::hex_rows()),
    );
}

#[test]
fn table_side_by_side_html() {
    let options = TableReportOptions {
        line_numbers: true,
        ..TableReportOptions::default()
    };
    check(
        "table-side-by-side.html",
        &table(&options, &OutputOptions::html_color(), common::table_rows()),
    );
}

#[test]
fn table_interleaved_plain() {
    let options = TableReportOptions {
        layout: TableLayout::Interleaved,
        current_sheet_only: true,
        ..TableReportOptions::default()
    };
    check(
        "table-interleaved.txt",
        &table(&options, &OutputOptions::plain_text(), common::table_rows()),
    );
}

#[test]
fn table_summary_plain() {
    let options = TableReportOptions {
        layout: TableLayout::Summary,
        display: DisplayFilter::Mismatches,
        ..TableReportOptions::default()
    };
    check(
        "table-summary.txt",
        &table(&options, &OutputOptions::plain_text(), common::table_rows()),
    );
}

#[test]
fn picture_side_by_side_html() {
    let mut out = Vec::new();
    ca_report::write_picture_report(
        &mut out,
        &common::meta(),
        &PictureReportOptions::default(),
        &OutputOptions::html_color(),
        &common::picture_facts(),
        &NeverCancel,
    )
    .expect("render");
    check("picture-side-by-side.html", &normalize(out));
}

#[test]
fn picture_summary_plain() {
    let options = PictureReportOptions {
        layout: PairLayout::Summary,
        ignore_unimportant: true,
        ..PictureReportOptions::default()
    };
    let mut out = Vec::new();
    ca_report::write_picture_report(
        &mut out,
        &common::meta(),
        &options,
        &OutputOptions::plain_text(),
        &common::picture_facts(),
        &NeverCancel,
    )
    .expect("render");
    check("picture-summary.txt", &normalize(out));
}

#[test]
fn registry_side_by_side_html() {
    check(
        "registry-side-by-side.html",
        &record(
            RecordKind::Registry,
            &RecordReportOptions::default(),
            &OutputOptions::html_color(),
            common::record_rows(),
        ),
    );
}

#[test]
fn version_summary_plain() {
    let options = RecordReportOptions {
        layout: PairLayout::Summary,
        ..RecordReportOptions::default()
    };
    check(
        "version-summary.txt",
        &record(
            RecordKind::Version,
            &options,
            &OutputOptions::plain_text(),
            common::record_rows(),
        ),
    );
}

#[test]
fn media_side_by_side_plain() {
    check(
        "media-side-by-side.txt",
        &record(
            RecordKind::Media,
            &RecordReportOptions::default(),
            &OutputOptions::plain_text(),
            common::record_rows(),
        ),
    );
}

#[test]
fn a_custom_stylesheet_document_loads_the_sheet() {
    check(
        "text-side-by-side-custom.html",
        &text(
            &TextReportOptions::default(),
            &OutputOptions::html_custom("report.css"),
            common::text_rows(),
        ),
    );
}
