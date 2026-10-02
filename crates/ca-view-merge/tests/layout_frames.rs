//! Where the merge view puts things, measured at real window sizes.
//!
//! A headless frame reports the same rectangles a window draws, so every check
//! here is geometry: what is inside what, what overlaps, and what still fits
//! when the window is small.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ca_ui::testing::{context, sized_input};
use ca_ui::view::{SessionView, Titles};
use ca_view_merge::jobs::MergePaths;
use ca_view_merge::model::Pane;
use ca_view_merge::MergeView;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A window the four panes have room in.
const WIDE: [f32; 2] = [1_280.0, 800.0];

/// The smallest window every control must still fit in.
const NARROW: [f32; 2] = [640.0, 480.0];

/// How far a measurement may differ before it counts as wrong.
const SLACK: f32 = 1.0;

struct Harness {
    view: MergeView,
    ctx: egui::Context,
    _dir: tempfile::TempDir,
}

fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, text.as_bytes()).unwrap();
    path
}

impl Harness {
    fn open(left: &str, center: Option<&str>, right: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let paths = MergePaths {
            left: write(dir.path(), "left.txt", left),
            center: center.map(|text| write(dir.path(), "center.txt", text)),
            right: write(dir.path(), "right.txt", right),
            output: Some(dir.path().join("merged.txt")),
        };
        let mut harness = Self {
            view: MergeView::over(paths, Titles::default(), &context(), 3),
            ctx: egui::Context::default(),
            _dir: dir,
        };
        harness.run_until_ready();
        harness
    }

    fn frame(&mut self, size: [f32; 2]) {
        self.view.tick();
        let view = &mut self.view;
        let held = context();
        let _ = self.ctx.run(sized_input(size[0], size[1]), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                view.ui(ui, &held);
            });
        });
    }

    fn run_until_ready(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            self.frame(WIDE);
            if self.view.is_ready() {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("the merge never became ready");
    }
}

fn sample() -> Harness {
    Harness::open(
        "alpha\nLEFT\ngamma\ndelta\n",
        Some("alpha\nbeta\ngamma\n"),
        "alpha\nRIGHT\ngamma\n",
    )
}

#[test]
fn every_painted_rectangle_sits_inside_the_window() {
    let mut harness = sample();
    harness.frame(WIDE);
    let window = harness.view.window();
    assert!(window.width() > 0.0);
    for widget in harness.view.widgets() {
        assert!(
            window.expand(SLACK).contains_rect(widget.rect),
            "{} at {:?} leaves the window {window:?}",
            widget.name,
            widget.rect
        );
    }
}

#[test]
fn the_three_inputs_sit_above_the_output_and_never_overlap() {
    let mut harness = sample();
    harness.frame(WIDE);
    let output = harness.view.pane_rect(Pane::Output).unwrap();
    let mut inputs = Vec::new();
    for pane in [Pane::Left, Pane::Center, Pane::Right] {
        let rect = harness.view.pane_rect(pane).unwrap();
        assert!(
            rect.bottom() <= output.top() + SLACK,
            "{} reaches into the output pane",
            pane.label()
        );
        inputs.push((pane, rect));
    }
    for (index, (pane, rect)) in inputs.iter().enumerate() {
        for (other, second) in inputs.iter().skip(index + 1) {
            assert!(
                !rect.shrink(SLACK).intersects(second.shrink(SLACK)),
                "{} overlaps {}",
                pane.label(),
                other.label()
            );
        }
    }
}

#[test]
fn the_panes_share_the_width_evenly() {
    let mut harness = sample();
    harness.frame(WIDE);
    let widths: Vec<f32> = [Pane::Left, Pane::Center, Pane::Right]
        .into_iter()
        .map(|pane| harness.view.pane_rect(pane).unwrap().width())
        .collect();
    let first = widths[0];
    for width in &widths {
        assert!(
            (width - first).abs() <= SLACK,
            "the input panes differ in width: {widths:?}"
        );
    }
}

#[test]
fn hiding_the_ancestor_gives_its_room_to_the_other_two() {
    let mut harness = sample();
    harness.frame(WIDE);
    let before = harness.view.pane_rect(Pane::Left).unwrap().width();
    harness.view.run(ca_ui::command::Command::ToggleCenterPane);
    harness.frame(WIDE);
    assert!(harness.view.pane_rect(Pane::Center).is_none());
    let after = harness.view.pane_rect(Pane::Left).unwrap().width();
    assert!(after > before, "{after} is not wider than {before}");
}

#[test]
fn a_merge_without_an_ancestor_draws_two_input_panes() {
    let mut harness = Harness::open("alpha\nLEFT\n", None, "alpha\nRIGHT\n");
    harness.frame(WIDE);
    assert_eq!(harness.view.visible_inputs().len(), 2);
    assert!(harness.view.pane_rect(Pane::Center).is_none());
    assert!(harness.view.pane_rect(Pane::Output).is_some());
}

#[test]
fn the_toolbar_and_the_status_bar_fit_a_small_window() {
    let mut harness = sample();
    harness.frame(NARROW);
    harness.frame(NARROW);
    let window = harness.view.window();
    let toolbar = harness.view.toolbar_rect();
    assert!(
        toolbar.right() <= window.right() + SLACK,
        "the toolbar is cut off: {toolbar:?} in {window:?}"
    );
    assert_eq!(harness.view.toolbar_rows(), 1);
    let status = harness.view.status_rect();
    assert!(status.bottom() <= window.bottom() + SLACK);
    assert!(toolbar.bottom() <= status.top() + SLACK);
}

#[test]
fn every_pane_stays_inside_the_comparison_area_of_a_small_window() {
    let mut harness = sample();
    harness.frame(NARROW);
    let window = harness.view.window();
    for pane in Pane::ALL {
        let Some(rect) = harness.view.pane_rect(pane) else {
            continue;
        };
        assert!(
            rect.width() > 0.0 && rect.height() > 0.0,
            "{}",
            pane.label()
        );
        assert!(
            window.expand(SLACK).contains_rect(rect),
            "{} leaves the window",
            pane.label()
        );
    }
}

#[test]
fn the_overview_strip_keeps_its_own_column() {
    let mut harness = sample();
    harness.frame(WIDE);
    let strip = harness
        .view
        .widgets()
        .iter()
        .find(|widget| widget.name == "thumbnail")
        .expect("the strip was drawn")
        .rect;
    for pane in Pane::ALL {
        let Some(rect) = harness.view.pane_rect(pane) else {
            continue;
        };
        assert!(
            !strip.shrink(SLACK).intersects(rect.shrink(SLACK)),
            "the strip overlaps {}",
            pane.label()
        );
    }
}
