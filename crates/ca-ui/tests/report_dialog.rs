//! The report dialog fits its window, and a stopped write leaves no file.

#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use ca_ui::report::{
    Payload, ReportDialog, ReportKind, ReportMessage, ReportPlan, ReportSettings, Target,
    TextPayload, TextRowRef,
};
use ca_ui::report::{ReportMeta, RowKind};
use ca_ui::testing::sized_input;
use std::sync::Arc;
use std::time::Duration;

/// The smallest window width the program opens at.
const NARROW: f32 = 640.0;

/// The width the window opens at by default.
const WIDE: f32 = 1_280.0;

/// The area every shape one frame painted covers.
fn painted_area(output: &egui::FullOutput) -> egui::Rect {
    let mut area = egui::Rect::NOTHING;
    for shape in &output.shapes {
        // A field clips its own text, so what reaches the screen is the part
        // inside the clip rectangle and not the whole galley.
        let bounds = shape
            .shape
            .visual_bounding_rect()
            .intersect(shape.clip_rect);
        if bounds.is_finite() && bounds.is_positive() {
            area = area.union(bounds);
        }
    }
    area
}

fn draw(kind: ReportKind, width: f32, path: &str) -> egui::Rect {
    let mut settings = ReportSettings::new(kind);
    path.clone_into(&mut settings.path);
    let mut dialog = ReportDialog::new(egui::Id::new("report"), settings, Arc::new(|| {}));
    let ctx = egui::Context::default();
    // The first frames lay out with nothing measured; a later frame is judged.
    for _ in 0..2 {
        let _ = ctx.run(sized_input(width, 900.0), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = dialog.ui(ui);
            });
        });
    }
    let output = ctx.run(sized_input(width, 900.0), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let _ = dialog.ui(ui);
        });
    });
    painted_area(&output)
}

#[test]
fn every_kind_of_report_dialog_fits_both_widths() {
    for kind in [
        ReportKind::Text,
        ReportKind::Folder,
        ReportKind::Hex,
        ReportKind::Table,
        ReportKind::Picture,
        ReportKind::Merge,
    ] {
        for width in [NARROW, WIDE] {
            let area = draw(kind, width, "report.html");
            assert!(
                area.right() <= width + 1.0,
                "{} drew to {} at {width} points",
                kind.id(),
                area.right()
            );
        }
    }
}

#[test]
fn a_long_path_does_not_widen_the_dialog() {
    let long = format!("C:\\{}\\report.html", "folder".repeat(80));
    let area = draw(ReportKind::Text, NARROW, &long);
    assert!(area.right() <= NARROW + 1.0, "drew to {}", area.right());
}

/// A payload large enough that a write takes more than one poll of the flag.
fn large_payload() -> Payload {
    let lines: Vec<String> = (0..60_000).map(|index| format!("line {index}")).collect();
    let other: Vec<String> = (0..60_000).map(|index| format!("LINE {index}")).collect();
    let rows = (0..60_000u32)
        .map(|index| TextRowRef {
            kind: RowKind::Changed,
            importance: None,
            left: Some(index),
            right: Some(index),
        })
        .collect();
    Payload::Text(TextPayload {
        left: Arc::new(lines),
        right: Arc::new(other),
        rows,
    })
}

#[test]
fn a_stopped_write_leaves_no_file_under_the_target_name() {
    let folder = tempfile::tempdir().expect("temporary folder");
    let target = folder.path().join("report.html");
    let mut settings = ReportSettings::new(ReportKind::Text);
    settings.target = Target::File;
    settings.path = target.display().to_string();
    let plan = ReportPlan {
        settings,
        meta: ReportMeta::new("left", "right"),
        payload: large_payload(),
        bytes_per_row: 0,
    };
    let mut job = ca_ui::report::spawn(plan, Arc::new(|| {}));
    job.cancel();
    let mut ending = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while ending.is_none() && std::time::Instant::now() < deadline {
        for message in job.drain() {
            if !matches!(message, ReportMessage::Progress { .. }) {
                ending = Some(message);
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        matches!(ending, Some(ReportMessage::Cancelled)),
        "the run reported {ending:?}"
    );
    assert!(!target.exists(), "a partial report was left behind");
    let left: Vec<_> = std::fs::read_dir(folder.path())
        .expect("read the folder")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .collect();
    assert!(left.is_empty(), "the folder still holds {left:?}");
}

#[test]
fn a_finished_write_names_the_target_and_its_size() {
    let folder = tempfile::tempdir().expect("temporary folder");
    let target = folder.path().join("report.txt");
    let mut settings = ReportSettings::new(ReportKind::Text);
    settings.html = false;
    settings.path = target.display().to_string();
    let plan = ReportPlan {
        settings,
        meta: ReportMeta::new("left", "right"),
        payload: Payload::Text(TextPayload {
            left: Arc::new(vec!["one".to_owned()]),
            right: Arc::new(vec!["ONE".to_owned()]),
            rows: vec![TextRowRef {
                kind: RowKind::Changed,
                importance: None,
                left: Some(0),
                right: Some(0),
            }],
        }),
        bytes_per_row: 0,
    };
    let mut job = ca_ui::report::spawn(plan, Arc::new(|| {}));
    let mut written = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while written.is_none() && std::time::Instant::now() < deadline {
        for message in job.drain() {
            if let ReportMessage::Written { path, bytes } = message {
                written = Some((path, bytes));
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let (path, bytes) = written.expect("the report was written");
    assert_eq!(path, target);
    assert!(bytes > 0);
    assert_eq!(std::fs::metadata(&target).expect("the file").len(), bytes);
}
