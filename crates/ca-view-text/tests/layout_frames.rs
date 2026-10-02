//! Real frames that check the measured layout and colors: which characters
//! take the difference color, which pane owns the darker background, where the
//! copy arrows sit, what the details area shows, and that a narrow window still
//! carries a whole toolbar.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ca_ui::command::Command;
use ca_ui::theme::{palette, Palette, TextClass, Variant};
use ca_ui::view::{SessionView, ViewContext};
use ca_view_text::sidecopy::Side;
use ca_view_text::TextView;
use std::sync::Arc;
use std::time::{Duration, Instant};

const TABLE: Variant = Variant::Dark;

fn context() -> ViewContext {
    ViewContext {
        options: Arc::default(),
        palette: palette(TABLE),
        notify: Arc::new(|| {}),
    }
}

struct Harness {
    view: TextView,
    ctx: egui::Context,
    context: ViewContext,
    width: f32,
    _dir: tempfile::TempDir,
}

impl Harness {
    fn new(left: &str, right: &str) -> Self {
        Self::sized(left, right, 1_280.0)
    }

    fn sized(left: &str, right: &str, width: f32) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let left_path = dir.path().join("left.txt");
        let right_path = dir.path().join("right.txt");
        std::fs::write(&left_path, left.as_bytes()).unwrap();
        std::fs::write(&right_path, right.as_bytes()).unwrap();
        let context = context();
        let view = TextView::new(left_path, right_path, &context, 1);
        let mut harness = Self {
            view,
            ctx: egui::Context::default(),
            context,
            width,
            _dir: dir,
        };
        harness.run_until_ready();
        harness.frame();
        harness
    }

    fn frame(&mut self) {
        self.view.tick();
        let view = &mut self.view;
        let context = &self.context;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(self.width, 800.0),
            )),
            ..egui::RawInput::default()
        };
        let _ = self.ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                view.ui(ui, context);
            });
        });
    }

    fn run_until_ready(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            self.frame();
            if self.view.is_ready() {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("the comparison never became ready");
    }

    fn table() -> Palette {
        palette(TABLE)
    }

    fn color_at(&self, side: Side, row: usize, column: u32) -> Option<egui::Color32> {
        self.view
            .painted_runs(side, row, &Self::table())
            .into_iter()
            .find(|run| run.start <= column && column < run.end)
            .map(|run| run.color)
    }
}

#[test]
fn only_the_differing_characters_take_the_difference_color() {
    let harness = Harness::new("one alpha two\n", "one alpho two\n");
    let table = Harness::table();
    // Only the ninth character differs; every other one keeps the same-text
    // color, and no separate background is painted behind the difference.
    assert_eq!(
        harness.color_at(Side::Left, 0, 8),
        Some(table.important_text)
    );
    for column in [0, 4, 7, 9, 11] {
        assert_eq!(
            harness.color_at(Side::Left, 0, column),
            Some(table.same_text),
            "column {column}"
        );
    }
}

#[test]
fn an_orphan_line_is_coloured_all_through() {
    let harness = Harness::new("one\ntwo\nthree\n", "one\nthree\n");
    let table = Harness::table();
    let row = harness.view.model().row_of_left_line(1).unwrap();
    for column in 0..3 {
        assert_eq!(
            harness.color_at(Side::Left, row, column),
            Some(table.orphan_text)
        );
    }
}

#[test]
fn the_two_panes_paint_matching_text_on_different_backgrounds() {
    let harness = Harness::new("one\ntwo\n", "one\ntwo\n");
    let table = Harness::table();
    assert_eq!(harness.view.active_side(), Side::Left);
    let active = harness.view.pane_background(Side::Left, 0, &table).unwrap();
    let other = harness
        .view
        .pane_background(Side::Right, 0, &table)
        .unwrap();
    assert_ne!(active, other);
    assert_eq!(active, table.text_row_in(TextClass::Same, true).0);
    assert_eq!(other, table.text_row_in(TextClass::Same, false).0);
}

#[test]
fn a_copy_arrow_sits_at_the_left_edge_of_each_pane_gutter() {
    let harness = Harness::new("one\nTWO\nthree\n", "one\ntwo two\nthree\n");
    let arrows = harness.view.section_arrows();
    assert_eq!(arrows.len(), 2, "one arrow per pane for one section");
    for arrow in arrows {
        assert!(
            (arrow.rect.left() - harness.view.gutter_x(arrow.side)).abs() < 0.5,
            "{:?} arrow is not at the gutter's left edge",
            arrow.side
        );
        assert!(arrow.rect.width() > 0.0 && arrow.rect.height() > 0.0);
    }
    assert!(arrows.iter().any(|arrow| arrow.side == Side::Left));
    assert!(arrows.iter().any(|arrow| arrow.side == Side::Right));
}

#[test]
fn one_arrow_is_drawn_for_each_visible_section() {
    let harness = Harness::new("a\nX\nb\nY\nc\n", "a\nx1\nb\ny1\nc\n");
    let sections = harness.view.model().counts().sections;
    assert_eq!(sections, 2);
    let arrows = harness.view.section_arrows();
    assert_eq!(arrows.len(), sections * 2);
    let mut listed: Vec<u32> = arrows
        .iter()
        .filter(|arrow| arrow.side == Side::Left)
        .map(|arrow| arrow.section)
        .collect();
    listed.sort_unstable();
    assert_eq!(listed, vec![0, 1]);
}

#[test]
fn the_details_area_shows_the_current_line_from_both_sides() {
    let mut harness = Harness::new("one\nTWO\nthree\n", "one\ntwo two\nthree\n");
    assert!(harness.view.shows_details());
    let row = harness.view.model().row_of_left_line(1).unwrap();
    assert_eq!(row, 1);
    let (left, right) = harness.view.details_lines().unwrap();
    assert_eq!(left, "TWO");
    assert_eq!(right, "two two");

    harness.view.run(Command::ToggleLineDetails);
    harness.frame();
    assert!(!harness.view.shows_details());
    assert!(harness.view.details_lines().is_none());
}

/// A row that one side has no line on paints the pane's normal background under
/// the hatch, and the hatch uses the recorded grey.
#[test]
fn a_gap_row_keeps_the_pane_background_and_a_grey_hatch() {
    let harness = Harness::new("one\ntwo\nthree\n", "one\nthree\n");
    let table = Harness::table();
    let row = harness.view.model().row_of_left_line(1).unwrap();
    let gap = harness
        .view
        .pane_background(Side::Right, row, &table)
        .unwrap();
    assert_eq!(gap, table.text_row_in(TextClass::Same, false).0);
    assert_ne!(gap, table.orphan_line);
    assert_ne!(gap, table.important_line);
    // The side that does hold the line keeps the orphan background.
    assert_eq!(
        harness.view.pane_background(Side::Left, row, &table),
        Some(table.orphan_line)
    );
    assert_eq!(TextView::gap_hatch_color(&table), table.gap_pattern);
}

/// The room between the panes is a rule, not a column.
#[test]
fn the_panes_meet_at_a_narrow_splitter_of_the_separator_colors() {
    let harness = Harness::new("one\n", "one\n");
    let (x, width) = harness.view.splitter();
    assert!(x > 0.0);
    assert!(
        width <= 4.0,
        "the splitter took {width} points between the panes"
    );
    let (near, far) = TextView::splitter_colors(&Harness::table());
    assert_eq!(near, Harness::table().separator_strong);
    assert_eq!(far, Harness::table().separator);
    assert_ne!(near, Harness::table().chrome);
}

#[test]
fn the_toolbar_fits_the_narrowest_supported_window() {
    let mut harness = Harness::sized("one\n", "one\n", 640.0);
    harness.frame();
    let (used, room) = harness.view.toolbar_extent();
    assert!(room > 0.0);
    assert!(
        used <= room + 0.5,
        "the toolbar needed {used} points in {room}"
    );
}

#[test]
fn a_row_is_fifteen_points_high_at_the_default_font() {
    let mut harness = Harness::new("one\n", "one\n");
    assert!((harness.view.row_pixels() - 15.0).abs() < f32::EPSILON);
    harness.view.run(Command::IncreaseFontSize);
    assert!(harness.view.row_pixels() > 15.0);
    harness.view.run(Command::ResetFontSize);
    assert!((harness.view.row_pixels() - 15.0).abs() < f32::EPSILON);
    harness.view.run(Command::DecreaseFontSize);
    assert!(harness.view.row_pixels() < 15.0);
}

#[test]
fn line_numbers_are_shown_by_default_and_can_be_hidden() {
    let mut harness = Harness::new("one\n", "one\n");
    assert!(harness.view.shows_line_numbers());
    let wide = harness.view.gutter_width();
    harness.view.run(Command::ToggleLineNumbers);
    harness.frame();
    assert!(!harness.view.shows_line_numbers());
    assert!(harness.view.gutter_width() < wide, "the panes gained room");
}

#[test]
fn ignoring_unimportant_differences_hides_a_case_only_change() {
    let mut harness = Harness::new("one\nBETA\n", "one\nbeta\n");
    assert_eq!(harness.view.model().counts().unimportant, 1);
    assert_eq!(harness.view.model().counts().sections, 1);
    assert_eq!(harness.view.status_fields()[0], "1 difference section(s)");

    harness.view.run(Command::ToggleIgnoreUnimportant);
    harness.frame();
    assert!(harness.view.ignores_unimportant());
    assert_eq!(harness.view.model().counts().sections, 0);
    assert_eq!(harness.view.status_fields()[0], "0 difference section(s)");
    assert!(harness.view.section_arrows().is_empty());
}

#[test]
fn the_status_bar_reports_the_section_count_the_line_and_the_mode() {
    let harness = Harness::new("one\ntwo\n", "one\nTWO two\n");
    let fields = harness.view.status_fields();
    assert_eq!(fields[0], "1 difference section(s)");
    assert_eq!(fields[1], "Important difference");
    assert_eq!(fields[2], "Insert");
    assert!(fields[3].starts_with("Load time "));
    assert!(
        !fields.iter().any(|field| field.contains("UTF-8")),
        "encoding belongs to the file information line"
    );
}
