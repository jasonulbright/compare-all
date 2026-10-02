//! Real frames that type, move the caret and copy a section, with every
//! assertion made on the model rather than on pixels.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ca_ui::command::Command;
use ca_ui::save::CloseChoice;
use ca_ui::theme::{palette, Variant};
use ca_ui::view::{SessionView, ViewContext};
use ca_view_text::editor::Caret;
use ca_view_text::sidecopy::Side;
use ca_view_text::TextView;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn context() -> ViewContext {
    ViewContext {
        options: Arc::default(),
        palette: palette(Variant::Light),
        notify: Arc::new(|| {}),
    }
}

fn raw_input(events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1_280.0, 800.0),
        )),
        events,
        ..egui::RawInput::default()
    }
}

struct Harness {
    view: TextView,
    ctx: egui::Context,
    context: ViewContext,
    left_path: PathBuf,
    right_path: PathBuf,
    _dir: tempfile::TempDir,
}

impl Harness {
    fn new(left: &str, right: &str) -> Self {
        Self::from_bytes(left.as_bytes(), right.as_bytes())
    }

    fn from_bytes(left: &[u8], right: &[u8]) -> Self {
        let mut harness = Self::from_bytes_unchecked(left, right);
        harness.run_until_ready();
        harness
    }

    fn from_bytes_unchecked(left: &[u8], right: &[u8]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let left_path = dir.path().join("left.txt");
        let right_path = dir.path().join("right.txt");
        std::fs::write(&left_path, left).unwrap();
        std::fs::write(&right_path, right).unwrap();
        let context = context();
        let view = TextView::new(left_path.clone(), right_path.clone(), &context, 1);
        Self {
            view,
            ctx: egui::Context::default(),
            context,
            left_path,
            right_path,
            _dir: dir,
        }
    }

    /// Run frames until `condition` holds, failing the test at the deadline.
    fn until(&mut self, mut condition: impl FnMut(&TextView) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            self.frame(Vec::new());
            if condition(&self.view) {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("the view never reached the state");
    }

    fn frame(&mut self, events: Vec<egui::Event>) {
        self.view.tick();
        let view = &mut self.view;
        let context = &self.context;
        let _ = self.ctx.run(raw_input(events), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                view.ui(ui, context);
            });
        });
    }

    fn run_until_ready(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            self.frame(Vec::new());
            if self.view.is_ready() {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("the comparison never became ready");
    }

    fn type_text(&mut self, text: &str) {
        for character in text.chars() {
            self.frame(vec![egui::Event::Text(character.to_string())]);
        }
    }

    fn press(&mut self, key: egui::Key, shift: bool) {
        let modifiers = egui::Modifiers {
            shift,
            ..egui::Modifiers::default()
        };
        self.frame(vec![egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }]);
    }

    fn left(&self) -> String {
        self.view.pane(Side::Left).buffer().text()
    }

    fn right(&self) -> String {
        self.view.pane(Side::Right).buffer().text()
    }
}

#[test]
fn typed_characters_reach_the_active_pane() {
    let mut harness = Harness::new("alpha\nbeta\n", "alpha\nbeta\n");
    assert_eq!(harness.view.active_side(), Side::Left);
    harness.type_text("XY");
    assert_eq!(harness.left(), "XYalpha\nbeta\n");
    assert_eq!(harness.right(), "alpha\nbeta\n");
    assert!(harness.view.pane(Side::Left).buffer().is_modified());
}

#[test]
fn the_caret_moves_with_the_arrow_keys() {
    let mut harness = Harness::new("alpha\nbeta\n", "alpha\nbeta\n");
    harness.press(egui::Key::ArrowRight, false);
    harness.press(egui::Key::ArrowRight, false);
    harness.press(egui::Key::ArrowDown, false);
    assert_eq!(harness.view.pane(Side::Left).caret(), Caret::new(1, 2));
    harness.press(egui::Key::ArrowRight, true);
    assert_eq!(
        harness.view.pane(Side::Left).selected_text().as_deref(),
        Some("t")
    );
}

#[test]
fn enter_and_backspace_change_the_line_count() {
    let mut harness = Harness::new("ab\n", "ab\n");
    harness.press(egui::Key::ArrowRight, false);
    harness.press(egui::Key::Enter, false);
    assert_eq!(harness.left(), "a\nb\n");
    harness.press(egui::Key::Backspace, false);
    assert_eq!(harness.left(), "ab\n");
}

#[test]
fn copying_a_section_makes_the_other_side_match() {
    let mut harness = Harness::new("one\nTWO\nthree\n", "one\ntwo\nthree\n");
    harness.view.run(Command::CopyToRight);
    harness.frame(Vec::new());
    assert_eq!(harness.right(), "one\nTWO\nthree\n");
    assert_eq!(harness.left(), harness.right());
}

#[test]
fn classic_mac_line_endings_keep_copy_rows_aligned_with_the_panes() {
    let mut harness = Harness::new("a\rb\rc\r", "a\rX\rc\r");
    assert_eq!(harness.view.pane(Side::Left).line_count(), 4);
    assert_eq!(harness.view.model().row_count(), 3);
    assert_eq!(
        harness.view.model().row(1).and_then(|row| row.left),
        Some(1)
    );

    harness.view.run(Command::CopyToRight);

    assert_eq!(harness.right(), "a\rb\rc\r");
}

#[test]
fn undecodable_files_with_different_bytes_are_refused_until_an_encoding_is_chosen() {
    let mut harness =
        Harness::from_bytes_unchecked(b"\xef\xbb\xbfcaf\xe9\n", b"\xef\xbb\xbfcaf\xe8\n");
    harness.until(|view| ca_ui::view::SessionView::is_ready(view) && !view.is_ready());
    assert!(!harness.view.is_ready());
}

#[test]
fn a_copy_to_the_other_side_undoes_in_one_step() {
    let mut harness = Harness::new("one\nTWO\nthree\n", "one\ntwo\nthree\n");
    harness.view.run(Command::CopyToRight);
    harness.frame(Vec::new());
    harness.view.run(Command::Undo);
    harness.frame(Vec::new());
    assert_eq!(harness.right(), "one\ntwo\nthree\n");
}

#[test]
fn an_edit_is_compared_again_and_the_alignment_catches_up() {
    let mut harness = Harness::new("one\ntwo\n", "one\ntwo\n");
    assert_eq!(harness.view.model().counts().differences, 0);
    harness.type_text("X");
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        harness.frame(Vec::new());
        if harness.view.model().counts().differences > 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(harness.view.model().counts().differences, 1);
    assert_eq!(harness.left(), "Xone\ntwo\n");
}

#[test]
fn find_moves_the_caret_to_a_match() {
    let mut harness = Harness::new("alpha\nbeta\ngamma\n", "alpha\nbeta\ngamma\n");
    harness.view.run(Command::Find);
    harness.frame(Vec::new());
    harness.view.set_find_pattern("gamma");
    harness.view.run(Command::FindNext);
    assert!(harness.view.pane(Side::Left).selection().is_none());
    harness.until(|view| view.pane(Side::Left).selected_text().as_deref() == Some("gamma"));
    assert_eq!(
        harness.view.pane(Side::Left).selected_text().as_deref(),
        Some("gamma")
    );
}

#[test]
fn a_modified_tab_refuses_to_close_until_it_is_answered() {
    let mut harness = Harness::new("a\n", "a\n");
    assert!(harness.view.may_close());
    harness.type_text("z");
    assert!(!harness.view.may_close());
    harness.view.mark_saved();
    assert!(harness.view.may_close());
}

#[test]
fn a_page_move_is_bounded_by_the_document() {
    let text = "line\n".repeat(200);
    let mut harness = Harness::new(&text, &text);
    harness.press(egui::Key::PageDown, false);
    let after_page = harness.view.pane(Side::Left).caret().line;
    assert!(after_page > 0);
    for _ in 0..50 {
        harness.press(egui::Key::PageDown, false);
    }
    let pane = harness.view.pane(Side::Left);
    assert_eq!(pane.caret().line, pane.line_count() - 1);
}

#[test]
fn the_title_marks_an_edited_comparison() {
    let mut harness = Harness::new("a\n", "a\n");
    let clean = harness.view.title();
    harness.type_text("z");
    let dirty = harness.view.title();
    assert_ne!(clean, dirty);
    assert!(dirty.starts_with('*'));
}

/// Agreeing to lose characters is not agreeing to overwrite a file another
/// program changed, so a change made while the first question is open raises
/// the second question, and the file stays as the other program left it.
#[test]
fn a_lossy_save_still_asks_about_a_change_made_on_disk_meanwhile() {
    let mut harness = Harness::from_bytes(&[0xF0, 0x28, b'\n'], b"other\n");
    std::fs::write(&harness.right_path, [0xF0, 0x28, b'\n']).unwrap();
    let mut settings = harness.view.session_settings().clone();
    settings.format.left_encoding = ca_session::settings::common::EncodingChoice::Named {
        name: "UTF-8".to_owned(),
        unknown: std::collections::BTreeMap::new(),
    };
    harness.view.apply_session_settings(settings);
    harness.run_until_ready();
    harness.type_text("x");
    harness.view.run(Command::SaveFile);
    harness.until(|view| !view.is_saving() && view.prompt_text().is_some());
    let loss = harness.view.prompt_text().unwrap();
    assert!(loss.contains("did not decode cleanly"), "{loss}");
    std::fs::write(&harness.left_path, b"changed by another program\n").unwrap();

    harness.view.answer_prompt(true);
    harness.until(|view| !view.is_saving());
    assert_eq!(
        harness.view.prompt_text().as_deref(),
        Some("The file changed on disk since it was read. Overwrite it?"),
        "the change on disk was not put to the user"
    );
    assert_eq!(
        std::fs::read(&harness.left_path).unwrap(),
        b"changed by another program\n"
    );
    assert!(harness.view.pane(Side::Left).is_modified());

    // The second answer agrees to the change on disk, and the loss agreed to
    // first still holds, so the save completes.
    harness.view.answer_prompt(true);
    harness.until(|view| !view.is_saving() && !view.pane(Side::Left).is_modified());
    assert_eq!(
        std::fs::read_to_string(&harness.left_path).unwrap(),
        "x\u{FFFD}(\n"
    );
}

/// Reload reads both files again, so an edit that is not written raises the
/// question a close raises, and the edit stays until it is answered.
#[test]
fn reload_with_an_edit_asks_before_it_reads_the_files_again() {
    let mut harness = Harness::new("alpha\nbeta\n", "alpha\nbeta\n");
    harness.type_text("XY");
    assert!(harness.view.accepts(Command::Reload));
    harness.view.run(Command::Reload);
    harness.frame(Vec::new());
    assert_eq!(
        harness.view.prompt_text().as_deref(),
        Some("This tab has edits that are not written.")
    );
    assert_eq!(harness.left(), "XYalpha\nbeta\n");
    assert!(harness.view.pane(Side::Left).is_modified());

    harness.view.answer_prompt(false);
    harness.frame(Vec::new());
    assert_eq!(harness.view.prompt_text(), None);
    assert_eq!(harness.left(), "XYalpha\nbeta\n");
    assert_eq!(
        std::fs::read_to_string(&harness.left_path).unwrap(),
        "alpha\nbeta\n"
    );

    harness.view.run(Command::Reload);
    harness.frame(Vec::new());
    harness.view.answer_unsaved(CloseChoice::Discard);
    harness.until(|view| view.is_ready() && !view.pane(Side::Left).is_modified());
    assert_eq!(harness.left(), "alpha\nbeta\n");
    assert_eq!(harness.view.prompt_text(), None);
}

/// Swap Sides reads both files again as well, so it asks the same question.
/// Saving first writes the edit, then the sides change places.
#[test]
fn swap_sides_with_an_edit_saves_first_when_asked_to() {
    let mut harness = Harness::new("alpha\nbeta\n", "gamma\n");
    harness.type_text("Q");
    let title = harness.view.title();
    harness.view.run(Command::SwapSides);
    harness.frame(Vec::new());
    assert_eq!(
        harness.view.prompt_text().as_deref(),
        Some("This tab has edits that are not written.")
    );
    assert_eq!(
        harness.view.title(),
        title,
        "the sides moved before the answer"
    );
    assert_eq!(harness.left(), "Qalpha\nbeta\n");

    harness.view.answer_prompt(true);
    harness.until(|view| {
        view.is_ready() && !view.is_saving() && view.title() == "right.txt - left.txt"
    });
    assert_eq!(
        std::fs::read_to_string(&harness.left_path).unwrap(),
        "Qalpha\nbeta\n"
    );
    assert_eq!(
        std::fs::read_to_string(&harness.right_path).unwrap(),
        "gamma\n"
    );
    assert_eq!(harness.left(), "gamma\n");
    assert_eq!(harness.right(), "Qalpha\nbeta\n");
    assert!(!harness.view.pane(Side::Left).is_modified());
    assert!(!harness.view.pane(Side::Right).is_modified());
}

/// A change to the format settings reads both files again, so it asks first
/// while a pane holds an edit, and the settings apply after the answer.
#[test]
fn a_format_change_with_an_edit_asks_before_it_reads_the_files_again() {
    let mut harness = Harness::new("alpha\nbeta\n", "alpha\nbeta\n");
    harness.type_text("XY");
    let before = harness.view.session_settings().clone();
    let mut changed = before.clone();
    changed.format.left_encoding = ca_session::settings::common::EncodingChoice::Named {
        name: "windows-1252".to_owned(),
        unknown: std::collections::BTreeMap::default(),
    };
    harness.view.apply_session_settings(changed.clone());
    harness.frame(Vec::new());
    assert_eq!(
        harness.view.prompt_text().as_deref(),
        Some("This tab has edits that are not written.")
    );
    assert_eq!(harness.left(), "XYalpha\nbeta\n");
    assert_eq!(harness.view.session_settings(), &before);

    harness.view.answer_unsaved(CloseChoice::Cancel);
    assert_eq!(harness.view.session_settings(), &before);
    assert_eq!(harness.left(), "XYalpha\nbeta\n");

    harness.view.apply_session_settings(changed.clone());
    harness.view.answer_unsaved(CloseChoice::Discard);
    harness.until(|view| view.is_ready() && !view.pane(Side::Left).is_modified());
    assert_eq!(harness.view.session_settings(), &changed);
    assert_eq!(harness.left(), "alpha\nbeta\n");
}

/// Find, Replace and Go To open their strip with its field holding the
/// keyboard and its text selected, so what is typed next replaces the text of
/// the field and the text of the panes stays as it is.
#[test]
fn typing_after_find_replace_or_go_to_goes_into_the_field() {
    for command in [Command::Find, Command::Replace, Command::GoTo] {
        let mut harness = Harness::new("alpha\nbeta\n", "alpha\nbeta\n");
        harness.view.set_find_pattern("earlier phrase");
        harness.view.run(command);
        harness.frame(Vec::new());
        harness.frame(Vec::new());
        harness.type_text("12");
        harness.frame(Vec::new());
        assert_eq!(harness.left(), "alpha\nbeta\n", "{command:?}");
        assert!(
            !harness.view.pane(Side::Left).buffer().is_modified(),
            "{command:?}"
        );
        if command != Command::GoTo {
            assert_eq!(harness.view.find_pattern(), "12", "{command:?}");
        }
    }
}

/// A save in flight keeps the tab from closing without a question, and the
/// tab says it is busy until the save ends.
#[test]
fn a_save_in_flight_keeps_the_tab_busy() {
    let mut harness = Harness::new("alpha\n", "alpha\n");
    harness.type_text("X");
    assert!(!SessionView::is_busy(&harness.view));
    harness.view.run(Command::SaveFile);
    assert!(harness.view.is_saving());
    assert!(SessionView::is_busy(&harness.view));
    assert!(!harness.view.may_close());
    harness.until(|view| !view.is_saving());
    assert!(!SessionView::is_busy(&harness.view));
}

/// With the editing switch of the Specs page on, typing reaches no pane, no
/// copy to the other side runs, and no save is offered. With the switch off
/// again, the panes take edits. The view reports the unwritten edit.
#[test]
fn the_editing_switch_turns_every_edit_and_save_off() {
    let mut harness = Harness::new("one\nTWO\n", "one\ntwo\n");
    let mut settings = harness.view.session_settings().clone();
    settings.specs.disable_editing = true;
    harness.view.apply_session_settings(settings.clone());
    harness.frame(Vec::new());
    for command in [
        Command::CopyToRight,
        Command::CopyToLeft,
        Command::Cut,
        Command::Paste,
        Command::SaveFileAs,
    ] {
        assert!(!harness.view.accepts(command), "{command:?}");
    }
    harness.type_text("XY");
    harness.view.run(Command::CopyToRight);
    harness.frame(Vec::new());
    assert_eq!(harness.left(), "one\nTWO\n");
    assert_eq!(harness.right(), "one\ntwo\n");
    assert!(!harness.view.holds_unwritten_edits());
    assert_eq!(
        harness
            .view
            .settings()
            .and_then(|settings| settings.specs().map(|specs| specs.disable_editing)),
        Some(true)
    );

    settings.specs.disable_editing = false;
    harness.view.apply_session_settings(settings);
    harness.until(TextView::is_settled);
    let active = harness.view.active_side();
    let before = harness.view.pane(active).buffer().text();
    harness.type_text("XY");
    let after = harness.view.pane(active).buffer().text();
    assert_eq!(after.len(), before.len() + 2, "{before:?} became {after:?}");
    assert!(after.contains("XY"), "{after:?}");
    assert!(harness.view.holds_unwritten_edits());
}
