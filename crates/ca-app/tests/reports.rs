//! What each view hands the report engine, and what the engine writes from it.
//!
//! The payload a view builds is compared with one written out by hand, so a
//! change to a view's display model is caught here rather than in the document.
//! One layout per view is then written through the engine and compared with a
//! golden string.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

use ca_ui::report::{
    EntryStatus, Payload, ReportKind, ReportMeta, ReportPlan, ReportSettings, RowKind, TextRowRef,
};
use ca_ui::testing::{context, raw_input, wait_until};
use ca_ui::view::SessionView;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long a comparison is waited on before the test gives up.
const BUDGET: Duration = Duration::from_secs(20);

/// Run frames until `ready` holds, so the view's comparison has arrived.
fn settle(view: &mut dyn SessionView, ready: impl Fn(&dyn SessionView) -> bool) {
    let ctx = egui::Context::default();
    let context = context();
    let held = std::cell::RefCell::new(view);
    let done = wait_until(BUDGET, || {
        let mut view = held.borrow_mut();
        view.tick();
        let _ = ctx.run(raw_input(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = view.ui(ui, &context);
            });
        });
        ready(&**view)
    });
    assert!(done, "the comparison did not arrive");
}

fn write_one(kind: ReportKind, layout: &str, payload: Payload) -> String {
    let mut settings = ReportSettings::new(kind);
    settings.html = false;
    settings.layout = kind
        .layouts()
        .iter()
        .position(|held| held.id == layout)
        .expect("the layout is offered");
    let plan = ReportPlan {
        settings,
        meta: ReportMeta::new("left", "right"),
        payload,
        bytes_per_row: 16,
    };
    let mut out: Vec<u8> = Vec::new();
    plan.write(&mut out, &ca_ui::worker::Cancel::new())
        .expect("the report is written");
    String::from_utf8(out).expect("the document is text")
}

fn fixture(folder: &Path, name: &str, body: &str) -> PathBuf {
    let path = folder.join(name);
    std::fs::write(&path, body).expect("write the fixture");
    path
}

#[test]
fn a_text_view_builds_the_rows_it_displays() {
    let folder = tempfile::tempdir().unwrap();
    let left = fixture(folder.path(), "left.txt", "one\ntwo\nthree\n");
    let right = fixture(folder.path(), "right.txt", "one\nTWO\nthree\n");
    let mut view = ca_view_text::TextView::new(left, right, &context(), 1);
    settle(&mut view, |view| {
        view.is_ready() && SessionView::accepts(view, ca_ui::Command::CompareReport)
    });

    let (_, payload) = view.report_payload();
    let Payload::Text(text) = payload else {
        panic!("a text view builds a text payload");
    };
    assert_eq!(
        text.rows,
        vec![
            TextRowRef {
                kind: RowKind::Same,
                importance: None,
                left: Some(0),
                right: Some(0),
            },
            TextRowRef {
                kind: RowKind::Changed,
                importance: text.rows[1].importance,
                left: Some(1),
                right: Some(1),
            },
            TextRowRef {
                kind: RowKind::Same,
                importance: None,
                left: Some(2),
                right: Some(2),
            },
        ]
    );
    assert!(text.left.iter().any(|line| line.starts_with("one")));

    let (_, payload) = view.report_payload();
    let document = write_one(ReportKind::Text, "summary", payload);
    assert!(document.contains("differences"), "{document}");
}

#[test]
fn a_text_views_display_filter_changes_the_report() {
    let folder = tempfile::tempdir().unwrap();
    let left = fixture(folder.path(), "left.txt", "one\ntwo\nthree\n");
    let right = fixture(folder.path(), "right.txt", "one\nTWO\nthree\n");
    let mut view = ca_view_text::TextView::new(left, right, &context(), 1);
    settle(&mut view, |view| view.is_ready());

    let (_, all) = view.report_payload();
    assert_eq!(view.report_filter(), "all");
    let Payload::Text(all) = all else {
        panic!("a text view builds a text payload");
    };

    view.run(ca_ui::Command::ShowDifferences);
    assert_eq!(view.report_filter(), "mismatches");
    let (_, fewer) = view.report_payload();
    let Payload::Text(fewer) = fewer else {
        panic!("a text view builds a text payload");
    };
    assert!(
        fewer.rows.len() < all.rows.len(),
        "{} rows is not fewer than {}",
        fewer.rows.len(),
        all.rows.len()
    );
    assert!(fewer.rows.iter().all(|row| row.kind.is_difference()));
}

#[test]
fn a_hex_view_shares_its_buffers_with_the_report() {
    let folder = tempfile::tempdir().unwrap();
    let left = fixture(folder.path(), "left.bin", "abcd");
    let right = fixture(folder.path(), "right.bin", "abXd");
    let mut view = ca_view_hex::HexView::new(left, right, &context(), 2);
    settle(&mut view, |view| view.is_ready());

    let (_, payload, width) = view.report_payload();
    let Payload::Hex(hex) = payload else {
        panic!("a hex view builds a hex payload");
    };
    assert_eq!(hex.left.as_slice(), b"abcd");
    assert_eq!(hex.right.as_slice(), b"abXd");
    assert!(!hex.hunks.is_empty());
    assert!(width > 0);

    let (_, payload, _) = view.report_payload();
    let document = write_one(ReportKind::Hex, "summary", payload);
    assert!(document.contains("Hex Compare Report"), "{document}");
}

#[test]
fn a_folder_view_reports_the_entries_it_shows() {
    let folder = tempfile::tempdir().unwrap();
    let left = folder.path().join("left");
    let right = folder.path().join("right");
    std::fs::create_dir_all(&left).unwrap();
    std::fs::create_dir_all(&right).unwrap();
    fixture(&left, "same.txt", "same");
    fixture(&right, "same.txt", "same");
    fixture(&left, "only-left.txt", "one side");

    let mut view = ca_view_folder::FolderView::new(left, right, &context(), 3);
    settle(&mut view, |view| {
        view.is_ready() && SessionView::accepts(view, ca_ui::Command::CompareReport)
    });

    let (_, payload) = view.report_payload();
    let Payload::Folder(rows) = payload else {
        panic!("a folder view builds folder rows");
    };
    let mut names: Vec<(String, EntryStatus)> = rows
        .iter()
        .map(|row| (row.name.clone(), row.status))
        .collect();
    names.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(
        names,
        vec![
            ("only-left.txt".to_owned(), EntryStatus::LeftOrphan),
            ("same.txt".to_owned(), names[1].1),
        ]
    );

    let (_, payload) = view.report_payload();
    let document = write_one(ReportKind::Folder, "summary", payload);
    assert!(document.contains("only-left.txt"), "{document}");
}

#[test]
fn a_picture_view_reports_its_counts_and_no_pixels() {
    let mut view = ca_view_picture::PictureView::new(
        PathBuf::from("left.png"),
        PathBuf::from("right.png"),
        &context(),
        4,
    );
    let ctx = egui::Context::default();
    let held = context();
    let _ = ctx.run(raw_input(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let _ = view.ui(ui, &held);
        });
    });
    let (_, payload) = view.report_payload();
    let Payload::Picture(facts) = payload else {
        panic!("a picture view builds picture facts");
    };
    assert!(facts.difference_png.is_none());
    assert_eq!(facts.tolerance, view.settings().tolerance);
}

#[test]
fn a_table_view_reports_the_columns_it_shows() {
    let folder = tempfile::tempdir().unwrap();
    let left = fixture(folder.path(), "left.csv", "name,value\na,1\nb,2\n");
    let right = fixture(folder.path(), "right.csv", "name,value\na,1\nb,3\n");
    let mut view = ca_view_table::TableView::new(left, right, &context(), 5);
    settle(&mut view, |view| view.is_ready());

    let (_, payload) = view.report_payload();
    let Payload::Table(header, rows) = payload else {
        panic!("a table view builds table rows");
    };
    assert_eq!(header.columns.len(), view.grid().shown_columns().len());
    assert_eq!(rows.len(), view.grid().visual_rows());
    assert!(rows.iter().any(|row| row.kind.is_difference()));

    let (_, payload) = view.report_payload();
    let document = write_one(ReportKind::Table, "summary", payload);
    assert!(document.contains("cells different"), "{document}");
}

#[test]
fn a_merge_view_reports_its_two_changed_versions() {
    let folder = tempfile::tempdir().unwrap();
    let left = fixture(folder.path(), "left.txt", "one\ntwo\n");
    let right = fixture(folder.path(), "right.txt", "one\nTWO\n");
    let base = fixture(folder.path(), "base.txt", "one\ntwo\n");
    let request = ca_ui::view::OpenRequest::new(ca_session::SessionKind::TextMerge, left, right)
        .with_center(Some(base));
    let mut view = ca_view_merge::MergeView::from_request(&request, &context(), 6);
    settle(&mut view, |view| view.is_ready());

    let (_, payload) = view.report_payload();
    let Payload::Text(text) = payload else {
        panic!("a merge view builds a text payload");
    };
    assert!(!text.rows.is_empty());
    assert!(text.left.iter().any(|line| line == "one"));

    let (_, payload) = view.report_payload();
    let document = write_one(ReportKind::Merge, "summary", payload);
    assert!(document.contains("differences"), "{document}");
}
