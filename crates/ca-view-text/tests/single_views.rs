//! Real frames over the text editor and the patch view, with every assertion
//! made on the view state or on the files written.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ca_ui::command::Command;
use ca_ui::testing::{context, event_input, wait_until};
use ca_ui::view::{SessionView, ViewContext};
use ca_view_text::{TextEditView, TextPatchView};
use std::path::{Path, PathBuf};
use std::time::Duration;

const BUDGET: Duration = Duration::from_secs(20);

struct Frames {
    ctx: egui::Context,
    context: ViewContext,
}

impl Frames {
    fn new() -> Self {
        Self {
            ctx: egui::Context::default(),
            context: context(),
        }
    }

    fn frame(&self, view: &mut dyn SessionView, events: Vec<egui::Event>) {
        view.tick();
        let context = &self.context;
        let _ = self.ctx.run(event_input(1_000.0, 600.0, events), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = view.ui(ui, context);
            });
        });
    }

    fn until<V: SessionView>(&self, view: &mut V, mut condition: impl FnMut(&V) -> bool) {
        let reached = wait_until(BUDGET, || {
            self.frame(view, Vec::new());
            condition(view)
        });
        assert!(reached, "the view never reached the state");
    }
}

fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, text.as_bytes()).unwrap();
    path
}

fn typed(text: &str) -> Vec<egui::Event> {
    vec![egui::Event::Text(text.to_owned())]
}

fn key(key: egui::Key) -> Vec<egui::Event> {
    vec![egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    }]
}

fn editor(frames: &Frames, path: PathBuf) -> TextEditView {
    let mut view = TextEditView::new(path, &frames.context, 1);
    frames.until(&mut view, TextEditView::is_loaded);
    view
}

#[test]
fn typing_and_saving_writes_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "notes.txt", "one\ntwo\n");
    let frames = Frames::new();
    let mut view = editor(&frames, path.clone());
    frames.frame(&mut view, typed("zero "));
    assert!(view.pane().is_modified());
    assert!(view.title().starts_with("* "));
    assert!(view.accepts(Command::SaveFile));
    view.run(Command::SaveFile);
    frames.until(&mut view, |view| {
        !view.is_saving() && !view.pane().is_modified()
    });
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "zero one\ntwo\n");
    assert_eq!(view.message(), Some("Saved."));
}

#[test]
fn a_click_far_along_a_plain_line_types_at_the_glyph_painted_under_it() {
    let line = "0123456789".repeat(9);
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "wide.txt", &format!("{line}\n"));
    for scale in [1.0_f32, 1.25, 1.5] {
        let frames = Frames::new();
        frames.ctx.set_pixels_per_point(scale);
        let mut view = editor(&frames, path.clone());
        let run = |view: &mut TextEditView, events: Vec<egui::Event>| {
            let context = &frames.context;
            frames.ctx.run(event_input(1_400.0, 600.0, events), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let _ = view.ui(ui, context);
                });
            })
        };
        let output = run(&mut view, Vec::new());
        let painted = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text.starts_with("0123") => {
                    Some(text.clone())
                }
                _ => None,
            })
            .unwrap();
        let glyph = &painted.galley.rows[0].glyphs[70];
        let at = egui::pos2(
            painted.pos.x + glyph.pos.x + glyph.advance_width * 0.25,
            painted.pos.y + 2.0,
        );
        let _ = run(
            &mut view,
            vec![
                egui::Event::PointerMoved(at),
                egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        let _ = run(
            &mut view,
            vec![egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        let _ = run(&mut view, typed("X"));
        let expected = format!("{}X{}", &line[..70], &line[70..]);
        assert_eq!(view.pane().line_text(0), expected, "{scale}");
    }
}

#[test]
fn enter_keeps_the_line_ending_style_of_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "dos.txt", "a\r\nb\r\n");
    let frames = Frames::new();
    let mut view = editor(&frames, path.clone());
    frames.frame(&mut view, key(egui::Key::Enter));
    view.run(Command::SaveFile);
    frames.until(&mut view, |view| {
        !view.is_saving() && !view.pane().is_modified()
    });
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "\r\na\r\nb\r\n");
}

#[test]
fn a_new_editor_starts_empty_and_saves_under_a_chosen_name() {
    let dir = tempfile::tempdir().unwrap();
    let frames = Frames::new();
    let mut view = editor(&frames, PathBuf::new());
    assert_eq!(view.title(), "Untitled");
    assert!(!view.accepts(Command::Reload));
    frames.frame(&mut view, typed("hello"));
    let target = dir.path().join("new.txt");
    view.save_as(&target);
    frames.until(&mut view, |view| {
        !view.is_saving() && !view.pane().is_modified()
    });
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello");
    assert_eq!(view.path(), target.as_path());
    assert_eq!(view.title(), "new.txt");
}

#[test]
fn a_modified_editor_asks_before_it_closes() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "keep.txt", "text\n");
    let frames = Frames::new();
    let mut view = editor(&frames, path.clone());
    assert!(view.may_close());
    frames.frame(&mut view, typed("x"));
    assert!(!view.may_close());
    assert!(view.open_question().is_some());
    assert!(!view.wants_close());
    view.discard_and_close();
    assert!(view.wants_close());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "text\n");
}

#[test]
fn save_and_close_writes_then_closes() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "close.txt", "text\n");
    let frames = Frames::new();
    let mut view = editor(&frames, path.clone());
    frames.frame(&mut view, typed("x"));
    assert!(!view.may_close());
    view.answer(true);
    frames.until(&mut view, SessionView::wants_close);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "xtext\n");
}

#[test]
fn a_file_changed_on_disk_is_not_overwritten_without_asking() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "race.txt", "old\n");
    let frames = Frames::new();
    let mut view = editor(&frames, path.clone());
    frames.frame(&mut view, typed("mine "));
    std::fs::write(&path, b"theirs, and longer\n").unwrap();
    view.run(Command::SaveFile);
    frames.until(&mut view, |view| {
        !view.is_saving() && view.open_question().is_some()
    });
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "theirs, and longer\n"
    );
    view.answer(true);
    frames.until(&mut view, |view| {
        !view.is_saving() && !view.pane().is_modified()
    });
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "mine old\n");
}

/// Agreeing to lose characters is not agreeing to overwrite a file another
/// program changed, so a change made while the first question is open raises
/// the second question, and the file stays as the other program left it.
#[test]
fn a_lossy_save_still_asks_about_a_change_made_on_disk_meanwhile() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lossy.txt");
    std::fs::write(&path, [0xEF, 0xBB, 0xBF, 0xF0, 0x28, b'\n']).unwrap();
    let frames = Frames::new();
    let mut view = editor(&frames, path.clone());
    frames.frame(&mut view, typed("x"));
    view.run(Command::SaveFile);
    frames.until(&mut view, |view| {
        !view.is_saving() && view.open_question().is_some()
    });
    let loss = view.open_question().unwrap();
    assert!(loss.starts_with("WouldLose"), "{loss}");
    std::fs::write(&path, b"changed by another program\n").unwrap();

    view.answer(true);
    frames.until(&mut view, |view| !view.is_saving());
    assert_eq!(
        view.open_question().as_deref(),
        Some("DiskChanged"),
        "the change on disk was not put to the user"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"changed by another program\n"
    );
    assert!(view.pane().is_modified());

    // The second answer agrees to the change on disk, and the loss agreed to
    // first still holds, so the save completes.
    view.answer(true);
    frames.until(&mut view, |view| {
        !view.is_saving() && !view.pane().is_modified()
    });
    assert_eq!(
        std::fs::read(&path).unwrap(),
        [0xEF, 0xBB, 0xBF, b'x', 0xEF, 0xBF, 0xBD, b'(', b'\n']
    );
}

#[test]
fn replace_all_edits_the_pane_and_undo_reverses_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "words.txt", "cat dog cat\n");
    let frames = Frames::new();
    let mut view = editor(&frames, path);
    view.set_find_pattern("cat");
    view.set_replacement("cow");
    view.replace_all();
    assert_eq!(view.pane().buffer().text(), "cat dog cat\n");
    frames.until(&mut view, |view| view.message() == Some("2 replaced."));
    assert_eq!(view.pane().buffer().text(), "cow dog cow\n");
    assert_eq!(view.message(), Some("2 replaced."));
    assert!(view.accepts(Command::NextEdit));
    view.run(Command::Undo);
    assert_eq!(view.pane().buffer().text(), "cat dog cat\n");
}

#[test]
fn a_source_file_takes_its_format_colors() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "main.rs", "fn main() {}\n");
    let frames = Frames::new();
    let mut view = editor(&frames, path);
    frames.until(&mut view, |_| true);
    assert_ne!(view.format_name(), "");
    let spans = view.syntax_spans(0).expect("a Rust line is colored");
    assert!(!spans.is_empty());
}

#[test]
fn a_missing_file_reports_itself() {
    let dir = tempfile::tempdir().unwrap();
    let frames = Frames::new();
    let mut view = TextEditView::new(dir.path().join("absent.txt"), &frames.context, 1);
    frames.until(&mut view, SessionView::is_ready);
    assert!(!view.is_loaded());
    assert!(!view.accepts(Command::SaveFile));
}

const PATCH: &str = "\
--- a/file.txt
+++ b/file.txt
@@ -1,3 +1,3 @@
 one
-two
+TWO
 three
";

fn patch_view(frames: &Frames, patch: PathBuf, target: PathBuf) -> TextPatchView {
    let mut view = TextPatchView::new(patch, target, &frames.context, 1);
    frames.until(&mut view, |view| {
        view.is_shown() || view.failure().is_some()
    });
    view
}

#[test]
fn a_patch_applied_to_a_file_shows_the_original_and_the_result() {
    let dir = tempfile::tempdir().unwrap();
    let patch = write(dir.path(), "fix.diff", PATCH);
    let target = write(dir.path(), "file.txt", "one\ntwo\nthree\n");
    let frames = Frames::new();
    let mut view = patch_view(&frames, patch, target);
    assert_eq!(view.failure(), None);
    assert_eq!(view.original_text(), Some("one\ntwo\nthree\n"));
    assert_eq!(view.patched_text(), Some("one\nTWO\nthree\n"));
    let comparison = view.comparison().unwrap();
    assert!(comparison.is_locked());
    assert!(comparison.model().first_difference().is_some());
    assert!(view.accepts(Command::NextDifference));
    assert!(view.accepts(Command::Copy));
    assert!(view.accepts(Command::SelectAll));
    for refused in [
        Command::Cut,
        Command::Paste,
        Command::SwapSides,
        Command::SaveFile,
        Command::CopyToOtherSide,
    ] {
        assert!(!view.accepts(refused), "{refused:?} is offered");
    }
    frames.frame(&mut view, typed("typed"));
    assert_eq!(view.patched_text(), Some("one\nTWO\nthree\n"));
}

#[test]
fn apply_patch_writes_the_result_over_the_target() {
    let dir = tempfile::tempdir().unwrap();
    let patch = write(dir.path(), "fix.patch", PATCH);
    let target = write(dir.path(), "file.txt", "one\r\ntwo\r\nthree\r\n");
    let frames = Frames::new();
    let mut view = patch_view(&frames, patch, target.clone());
    assert!(view.accepts(Command::ApplyPatch));
    view.run(Command::ApplyPatch);
    frames.until(&mut view, |view| !view.is_saving());
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "one\r\nTWO\r\nthree\r\n"
    );
}

#[test]
fn the_result_saves_under_another_name_and_the_target_stays() {
    let dir = tempfile::tempdir().unwrap();
    let patch = write(dir.path(), "fix.diff", PATCH);
    let target = write(dir.path(), "file.txt", "one\ntwo\nthree\n");
    let frames = Frames::new();
    let mut view = patch_view(&frames, patch, target.clone());
    let out = dir.path().join("out.txt");
    view.save_result_as(&out);
    frames.until(&mut view, |view| !view.is_saving());
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "one\nTWO\nthree\n");
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "one\ntwo\nthree\n"
    );
}

#[test]
fn a_hunk_that_does_not_match_is_reported_and_apply_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let patch = write(dir.path(), "fix.diff", PATCH);
    let target = write(dir.path(), "file.txt", "alpha\nbeta\n");
    let frames = Frames::new();
    let view = patch_view(&frames, patch, target);
    assert_eq!(view.rejected_hunks(), &[0]);
    assert!(view.message().is_some());
    assert!(!view.accepts(Command::ApplyPatch));
}

#[test]
fn a_patch_alone_shows_the_sides_its_hunks_carry() {
    let dir = tempfile::tempdir().unwrap();
    let patch = write(dir.path(), "fix.diff", PATCH);
    let frames = Frames::new();
    let view = patch_view(&frames, patch, PathBuf::new());
    assert_eq!(view.original_text(), Some("one\ntwo\nthree\n"));
    assert_eq!(view.patched_text(), Some("one\nTWO\nthree\n"));
    assert!(!view.accepts(Command::ApplyPatch));
    assert!(view.accepts(Command::SaveFileAs));
}

#[test]
fn next_difference_files_moves_through_the_file_sections() {
    let dir = tempfile::tempdir().unwrap();
    let text = format!("{PATCH}--- a/second.txt\n+++ b/second.txt\n@@ -1 +1 @@\n-p\n+q\n");
    let patch = write(dir.path(), "two.diff", &text);
    let frames = Frames::new();
    let mut view = patch_view(&frames, patch, PathBuf::new());
    assert_eq!(view.file_names().len(), 2);
    assert!(!view.accepts(Command::PreviousDifferenceFiles));
    assert!(view.accepts(Command::NextDifferenceFiles));
    view.run(Command::NextDifferenceFiles);
    frames.until(&mut view, |view| {
        view.is_shown() && view.patched_text() == Some("q\n")
    });
    assert_eq!(view.file_index(), 1);
    assert!(view.accepts(Command::PreviousDifferenceFiles));
    assert!(!view.accepts(Command::NextDifferenceFiles));
}

#[test]
fn a_file_that_is_not_a_patch_is_reported_rather_than_shown() {
    let dir = tempfile::tempdir().unwrap();
    let patch = write(dir.path(), "junk.diff", "@@ -1,99999999 +1 @@\n one\n");
    let frames = Frames::new();
    let view = patch_view(&frames, patch, PathBuf::new());
    assert!(view.failure().is_some());
    assert!(view.comparison().is_none());
    assert!(!view.accepts(Command::SaveFileAs));
}

#[test]
fn an_empty_patch_view_waits_for_a_file() {
    let frames = Frames::new();
    let mut view = TextPatchView::new(PathBuf::new(), PathBuf::new(), &frames.context, 1);
    frames.frame(&mut view, Vec::new());
    assert!(SessionView::is_ready(&view));
    assert_eq!(view.title(), "Text Patch");
    assert!(view.failure().is_none());
    assert!(!view.accepts(Command::Reload));
}

/// A save in flight keeps the editor and the patch tab from closing without a
/// question, and each says it is busy until the save ends.
#[test]
fn a_save_in_flight_keeps_the_editor_and_the_patch_tab_busy() {
    let dir = tempfile::tempdir().unwrap();
    let frames = Frames::new();

    let mut view = editor(&frames, write(dir.path(), "notes.txt", "one\n"));
    frames.frame(&mut view, typed("zero "));
    assert!(!view.is_busy());
    view.run(Command::SaveFile);
    assert!(view.is_saving());
    assert!(view.is_busy());
    assert!(!view.may_close());
    frames.until(&mut view, |view| !view.is_saving());
    assert!(!view.is_busy());

    let patch = write(dir.path(), "fix.patch", PATCH);
    let target = write(dir.path(), "file.txt", "one\r\ntwo\r\nthree\r\n");
    let mut view = patch_view(&frames, patch, target);
    assert!(!view.is_busy());
    view.run(Command::ApplyPatch);
    assert!(view.is_saving());
    assert!(view.is_busy());
    assert!(!view.may_close());
    frames.until(&mut view, |view| !view.is_saving());
    assert!(!view.is_busy());
}

/// With the editing switch of the Specs page on, the editor takes no typing
/// and offers no save. With the switch off again, typing lands, and the
/// editor reports the unwritten edit.
#[test]
fn the_editing_switch_turns_the_editor_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "notes.txt", "one\n");
    let frames = Frames::new();
    let mut view = editor(&frames, path);
    let mut settings = view.settings().unwrap();
    settings.specs_mut().unwrap().disable_editing = true;
    view.apply_settings(&settings);
    frames.frame(&mut view, typed("zero "));
    assert!(!view.pane().is_modified());
    for command in [
        Command::Cut,
        Command::Paste,
        Command::SaveFile,
        Command::SaveFileAs,
    ] {
        assert!(!view.accepts(command), "{command:?}");
    }
    assert!(!view.holds_unwritten_edits());

    settings.specs_mut().unwrap().disable_editing = false;
    view.apply_settings(&settings);
    frames.frame(&mut view, typed("zero "));
    assert!(view.pane().is_modified());
    assert!(view.holds_unwritten_edits());
}

/// A picker result opened before the editing switch changes cannot write its
/// chosen destination after the editor becomes read-only.
#[test]
fn an_editor_save_as_callback_is_refused_after_editing_is_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "notes.txt", "one\n");
    let target = dir.path().join("copy.txt");
    let frames = Frames::new();
    let mut view = editor(&frames, path);
    let mut settings = view.settings().unwrap();
    settings.specs_mut().unwrap().disable_editing = true;
    view.apply_settings(&settings);
    assert!(!view.accepts(Command::SaveFileAs));

    view.save_as(&target);
    assert!(!view.is_saving());
    assert!(!target.exists());
    assert_eq!(
        view.message(),
        Some("Editing is turned off for this session")
    );
}

/// With the editing switch of the Specs page on, the patch view neither
/// applies the patch over its target nor saves the result.
#[test]
fn the_editing_switch_keeps_the_patch_view_from_writing() {
    let dir = tempfile::tempdir().unwrap();
    let patch = write(dir.path(), "fix.diff", PATCH);
    let target = write(dir.path(), "file.txt", "one\ntwo\nthree\n");
    let frames = Frames::new();
    let mut view = patch_view(&frames, patch, target.clone());
    assert!(view.accepts(Command::ApplyPatch));
    let mut settings = view.settings().unwrap();
    settings.specs_mut().unwrap().disable_editing = true;
    view.apply_settings(&settings);
    assert!(!view.accepts(Command::ApplyPatch));
    assert!(!view.accepts(Command::SaveFileAs));
    view.run(Command::ApplyPatch);
    let out = dir.path().join("out.txt");
    view.save_result_as(&out);
    frames.frame(&mut view, Vec::new());
    assert!(!view.is_saving());
    assert!(!out.exists());
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "one\ntwo\nthree\n"
    );
}
