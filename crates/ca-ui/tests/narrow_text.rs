//! Text that carries a path has to fit the space it is given, at any width.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ca_ui::testing::sized_input;
use ca_ui::widgets::{path_line, path_text, wrapped_text};
use std::path::PathBuf;

/// Characters in the path the tests draw.
const LONG_PATH_CHARACTERS: usize = 300;

/// A path with no break a wrapping layout can use.
fn long_path() -> PathBuf {
    PathBuf::from(format!(
        "/{}/only-right.txt",
        "d".repeat(LONG_PATH_CHARACTERS)
    ))
}

/// The area every shape one frame painted covers.
fn painted_area(output: &egui::FullOutput) -> egui::Rect {
    let mut area = egui::Rect::NOTHING;
    for shape in &output.shapes {
        let bounds = shape.shape.visual_bounding_rect();
        if bounds.is_finite() && bounds.is_positive() {
            area = area.union(bounds);
        }
    }
    area
}

/// Draw `add` in a panel of `width` points and report what it painted.
fn draw(width: f32, add: impl FnOnce(&mut egui::Ui)) -> egui::Rect {
    let ctx = egui::Context::default();
    // The first frame lays out with nothing measured, so a second frame is the
    // one judged.
    let _ = ctx.run(sized_input(width, 400.0), |_| {});
    let mut once = Some(add);
    let output = ctx.run(sized_input(width, 400.0), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(add) = once.take() {
                add(ui);
            }
        });
    });
    painted_area(&output)
}

#[test]
fn a_long_path_on_one_line_fits_every_width() {
    for width in [200.0_f32, 640.0, 1_280.0] {
        let area = draw(width, |ui| {
            path_line(ui, "", &long_path());
        });
        assert!(
            area.right() <= width + 1.0,
            "at {width} points the path drew {area:?}"
        );
    }
}

#[test]
fn a_sentence_holding_a_long_path_fits_every_width() {
    let text = format!(
        "2 unfinished file operation batches were found. The first names {}.",
        long_path().display()
    );
    for width in [200.0_f32, 640.0, 1_280.0] {
        let area = draw(width, |ui| {
            wrapped_text(ui, &text);
        });
        assert!(
            area.right() <= width + 1.0,
            "at {width} points the notice drew {area:?}"
        );
    }
}

#[test]
fn an_indented_step_line_fits_every_width() {
    let text = format!(
        "    Copy {} to {}",
        long_path().display(),
        long_path().display()
    );
    for width in [200.0_f32, 640.0, 1_280.0] {
        let area = draw(width, |ui| {
            path_text(ui, &text);
        });
        assert!(
            area.right() <= width + 1.0,
            "at {width} points the step drew {area:?}"
        );
    }
}

/// The whole path stays reachable: only the middle is dropped.
#[test]
fn an_elided_path_keeps_its_head_and_its_tail() {
    let path = long_path().display().to_string();
    let elided = ca_ui::widgets::elide_middle(&path, 40);
    assert_eq!(elided.chars().count(), 40);
    assert!(elided.starts_with("/dd"));
    assert!(elided.ends_with("only-right.txt"));
}
