//! Real frames that check which application options the text comparison reads.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ca_session::options::ProgramOptions;
use ca_session::AdminPolicies;
use ca_ui::command::Command;
use ca_ui::options::AppOptions;
use ca_ui::theme::Variant;
use ca_ui::view::{SessionView, ViewContext};
use ca_view_text::TextView;
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Harness {
    view: TextView,
    ctx: egui::Context,
    context: ViewContext,
    options: ProgramOptions,
    dir: tempfile::TempDir,
}

impl Harness {
    fn new(left: &str, right: &str) -> Self {
        Self::configured(left, right, |_| {})
    }

    /// A view whose first frame already runs under the stated options.
    fn configured(left: &str, right: &str, edit: impl FnOnce(&mut ProgramOptions)) -> Self {
        Self::named(("left.txt", left), ("right.txt", right), edit)
    }

    /// A view over two files with the given names and contents.
    fn named(
        (left_name, left): (&str, &str),
        (right_name, right): (&str, &str),
        edit: impl FnOnce(&mut ProgramOptions),
    ) -> Self {
        let context = ca_ui::testing::context();
        let dir = tempfile::tempdir().unwrap();
        let left_path = dir.path().join(left_name);
        let right_path = dir.path().join(right_name);
        std::fs::write(&left_path, left.as_bytes()).unwrap();
        std::fs::write(&right_path, right.as_bytes()).unwrap();
        let view = TextView::new(left_path, right_path, &context, 1);
        let mut options = ProgramOptions::default();
        edit(&mut options);
        let mut harness = Self {
            view,
            ctx: egui::Context::default(),
            context,
            options,
            dir,
        };
        harness.run_until_ready();
        harness
    }

    /// State the options the next frames run under.
    fn set_options(&mut self, edit: impl FnOnce(&mut ProgramOptions)) {
        edit(&mut self.options);
    }

    fn frame(&mut self) {
        self.view.tick();
        let view = &mut self.view;
        let context = &self.context;
        let resolved = Arc::new(AppOptions::resolve(
            self.options.clone(),
            Variant::Light,
            AdminPolicies::default(),
        ));
        let _ = self.ctx.run(ca_ui::testing::raw_input(), |ctx| {
            ca_ui::options::install(ctx, Arc::clone(&resolved));
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
}

#[test]
fn the_first_difference_is_reached_on_load_only_when_the_page_says_so() {
    let stopped = Harness::configured("a\nb\nc\nD\n", "a\nb\nc\nX\n", |options| {
        options.next_difference.go_to_first_difference_on_load = false;
    });
    assert_eq!(stopped.view.current_row(), 0);

    let moved = Harness::configured("a\nb\nc\nD\n", "a\nb\nc\nX\n", |options| {
        options.next_difference.go_to_first_difference_on_load = true;
    });
    assert_eq!(moved.view.current_row(), 3);
}

#[test]
fn navigation_continues_from_the_first_difference_only_when_wrapping_is_asked_for() {
    let mut stopped = Harness::configured("A\nb\nC\n", "X\nb\nY\n", |options| {
        options.next_difference.go_to_first_difference_on_load = false;
        options.next_difference.wrap_around = false;
        options.next_difference.show_message_panel = true;
    });
    stopped.view.run(Command::NextDifference);
    assert_eq!(stopped.view.current_row(), 2);
    stopped.view.run(Command::NextDifference);
    assert_eq!(
        stopped.view.current_row(),
        2,
        "navigation left the last row"
    );
    assert!(
        stopped.view.message().is_some(),
        "nothing said why it stopped"
    );

    let mut wrapping = Harness::configured("A\nb\nC\n", "X\nb\nY\n", |options| {
        options.next_difference.go_to_first_difference_on_load = false;
        options.next_difference.wrap_around = true;
    });
    wrapping.view.run(Command::NextDifference);
    assert_eq!(wrapping.view.current_row(), 2);
    wrapping.view.run(Command::NextDifference);
    assert_eq!(wrapping.view.current_row(), 0);
}

#[test]
fn a_cleared_message_panel_says_nothing_when_navigation_stops() {
    let mut harness = Harness::configured("A\nb\n", "X\nb\n", |options| {
        options.next_difference.go_to_first_difference_on_load = false;
        options.next_difference.wrap_around = false;
        options.next_difference.show_message_panel = false;
    });
    harness.view.run(Command::NextDifference);
    harness.view.run(Command::NextDifference);
    assert_eq!(harness.view.message(), None);
}

/// A copy moves on to the next difference only when the page asks for it.
#[test]
fn a_copy_moves_to_the_next_difference_only_when_the_page_says_so() {
    let mut staying = Harness::configured("A\nb\nC\n", "X\nb\nY\n", |options| {
        options.next_difference.go_to_first_difference_on_load = true;
        options.next_difference.go_to_next_after_copy = false;
    });
    assert_eq!(staying.view.current_row(), 0);
    staying.view.run(Command::CopyToRight);
    assert_eq!(staying.view.current_row(), 0);

    let mut moving = Harness::configured("A\nb\nC\n", "X\nb\nY\n", |options| {
        options.next_difference.go_to_first_difference_on_load = true;
        options.next_difference.go_to_next_after_copy = true;
    });
    assert_eq!(moving.view.current_row(), 0);
    moving.view.run(Command::CopyToRight);
    assert_eq!(moving.view.current_row(), 2);
}

#[test]
fn the_row_height_follows_the_stored_editor_font_size() {
    let mut harness = Harness::new("one\ntwo\n", "one\nTWO\n");
    harness.frame();
    let before = harness.view.row_pixels();

    harness.set_options(|options| options.appearance.fonts.editor_point_size = 24.0);
    harness.frame();
    let after = harness.view.row_pixels();
    assert!(
        after > before * 1.5,
        "the row height stayed at {before} while the font size doubled"
    );
}

#[test]
fn the_row_height_follows_the_extra_line_spacing_tweak() {
    let mut harness = Harness::new("one\n", "one\n");
    harness.frame();
    let before = harness.view.row_pixels();

    harness.set_options(|options| options.tweaks.extra_line_spacing = 6);
    harness.frame();
    assert!((harness.view.row_pixels() - (before + 6.0)).abs() < f32::EPSILON);
}

/// A zoom moves the size the option states; the reset command returns to it.
#[test]
fn the_zoom_commands_move_the_same_size_and_the_reset_returns_to_it() {
    let mut harness = Harness::new("one\n", "one\n");
    harness.frame();
    let configured = harness.view.row_pixels();

    harness.view.run(Command::IncreaseFontSize);
    harness.frame();
    let larger = harness.view.row_pixels();
    assert!(larger > configured);

    // A frame that resolves the same stored value leaves the zoom in place.
    harness.frame();
    assert!((harness.view.row_pixels() - larger).abs() < f32::EPSILON);

    harness.view.run(Command::ResetFontSize);
    harness.frame();
    assert!((harness.view.row_pixels() - configured).abs() < f32::EPSILON);
}

/// An edit in the options dialog wins over a zoom of the same view, so a size
/// typed into the page is never hidden behind an earlier keystroke.
#[test]
fn a_stored_size_that_changes_replaces_what_a_zoom_reached() {
    let mut harness = Harness::new("one\n", "one\n");
    harness.frame();
    harness.view.run(Command::IncreaseFontSize);
    harness.frame();

    harness.set_options(|options| options.appearance.fonts.editor_point_size = 30.0);
    harness.frame();
    let stored_height = harness.view.row_pixels();

    harness.view.run(Command::ResetFontSize);
    harness.frame();
    assert!((harness.view.row_pixels() - stored_height).abs() < f32::EPSILON);
}

/// A load that finishes before the first frame still obeys the page. The worker
/// is waited on with `tick` alone, so the load is installed before any frame
/// has read the options, which is the order a busy machine can produce.
#[test]
fn a_load_finished_before_the_first_frame_still_obeys_the_page() {
    let context = ca_ui::testing::context();
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("left.txt");
    let right = dir.path().join("right.txt");
    std::fs::write(&left, b"a\nb\nc\nD\n").unwrap();
    std::fs::write(&right, b"a\nb\nc\nX\n").unwrap();
    let mut view = TextView::new(left, right, &context, 1);
    let ready = ca_ui::testing::wait_until(Duration::from_secs(20), || {
        view.tick();
        view.is_ready()
    });
    assert!(ready, "the comparison never became ready");
    let mut options = ProgramOptions::default();
    options.next_difference.go_to_first_difference_on_load = false;
    let resolved = Arc::new(AppOptions::resolve(
        options,
        Variant::Light,
        AdminPolicies::default(),
    ));
    let ctx = egui::Context::default();
    let _ = ctx.run(ca_ui::testing::raw_input(), |ctx| {
        ca_ui::options::install(ctx, Arc::clone(&resolved));
        egui::CentralPanel::default().show(ctx, |ui| {
            view.ui(ui, &context);
        });
    });
    assert_eq!(view.current_row(), 0);
}

impl Harness {
    fn path(&self, name: &str) -> std::path::PathBuf {
        self.dir.path().join(name)
    }

    /// Run frames until `done` holds or a deadline passes.
    fn frames_until(&mut self, mut done: impl FnMut(&Self) -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            self.frame();
            if done(self) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        false
    }
}

#[test]
fn a_save_writes_the_line_ending_the_page_names() {
    let mut harness = Harness::new("a\r\nb\r\n", "a\r\nb\r\n");
    harness.set_options(|options| {
        options.text_editing.line_endings_on_save =
            ca_session::options::LineEndingsOnSave::from_id("lf");
    });
    harness.frame();
    harness.view.run(Command::SaveFile);
    let left = harness.path("left.txt");
    assert!(
        harness.frames_until(|_| std::fs::read(&left).is_ok_and(|bytes| bytes == b"a\nb\n")),
        "the save kept the old line endings"
    );
    assert_eq!(
        std::fs::read(harness.path("right.txt")).unwrap(),
        b"a\r\nb\r\n"
    );
}

#[test]
fn a_save_keeps_a_numbered_copy_when_the_page_asks_for_one() {
    let mut harness = Harness::new("one\n", "two\n");
    harness.set_options(|options| {
        options.backups.before_save = true;
        options.backups.suffix = ".old".to_owned();
    });
    harness.frame();
    harness.view.run(Command::SaveFile);
    let copy = harness.path("left.txt.old");
    assert!(
        harness.frames_until(|_| std::fs::read(&copy).is_ok_and(|bytes| bytes == b"one\n")),
        "no copy of the old text was taken before the save"
    );
}

#[test]
fn find_starts_from_the_word_under_the_caret_only_while_the_page_says_so() {
    let mut seeded = Harness::new("alpha beta\n", "alpha beta\n");
    seeded.frame();
    seeded.view.run(Command::Find);
    assert_eq!(seeded.view.find_pattern(), "alpha");

    let mut plain = Harness::new("alpha beta\n", "alpha beta\n");
    plain.set_options(|options| options.text_editing.find_uses_current_word = false);
    plain.frame();
    plain.view.run(Command::Find);
    assert_eq!(plain.view.find_pattern(), "");
}

#[test]
fn a_column_ruler_is_drawn_in_both_panes_only_when_a_column_is_named() {
    let mut harness = Harness::new("one\n", "one\n");
    harness.frame();
    assert!(harness.view.painted_rulers().is_empty());

    harness.set_options(|options| options.tweaks.column_line_at = 2);
    harness.frame();
    assert_eq!(harness.view.painted_rulers().len(), 2);
}

#[test]
fn the_pane_without_focus_is_darkened_only_when_the_page_says_so() {
    let mut harness = Harness::new("one\n", "one\n");
    harness.frame();
    assert_eq!(harness.view.dimmed_pane(), None);

    harness.set_options(|options| options.tweaks.dim_inactive_pane_percent = 40);
    harness.frame();
    let active = harness.view.active_side();
    assert_eq!(harness.view.dimmed_pane(), Some(active.other()));
}

#[test]
fn a_difference_line_keeps_its_syntax_colors_only_when_the_tweak_says_so() {
    let left = ("a.rs", "fn main() { let alpha = 1; }\n");
    let right = ("b.rs", "fn main() { let alphx = 1; }\n");
    let palette = ca_ui::theme::palette(Variant::Light);
    let colors = |harness: &Harness| -> Vec<egui::Color32> {
        harness
            .view
            .painted_runs(ca_view_text::sidecopy::Side::Left, 0, &palette)
            .into_iter()
            .map(|run| run.color)
            .collect()
    };
    let mut plain = Harness::named(left, right, |_| {});
    plain.frame();
    let mut colored = Harness::named(left, right, |options| {
        options.tweaks.syntax_highlighting_on_difference_lines = true;
    });
    colored.frame();
    let plain_colors = colors(&plain);
    let colored_colors = colors(&colored);
    assert_ne!(plain_colors, colored_colors);
    assert!(
        colored_colors.len() > plain_colors.len(),
        "{plain_colors:?} {colored_colors:?}"
    );
}
