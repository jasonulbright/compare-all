//! What a whole merge session does: editing the output, writing it, and what
//! happens when a newer request arrives while an older one runs.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ca_ui::command::Command;
use ca_ui::testing::{context, event_input, sized_input};
use ca_ui::view::{SessionView, Titles};
use ca_view_merge::jobs::MergePaths;
use ca_view_merge::model::Pane;
use ca_view_merge::{MergeView, Question};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const WINDOW: [f32; 2] = [1_280.0, 800.0];

struct Session {
    view: MergeView,
    ctx: egui::Context,
    dir: tempfile::TempDir,
}

fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, text.as_bytes()).unwrap();
    path
}

impl Session {
    fn open(left: &str, center: Option<&str>, right: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let paths = MergePaths {
            left: write(dir.path(), "left.txt", left),
            center: center.map(|text| write(dir.path(), "center.txt", text)),
            right: write(dir.path(), "right.txt", right),
            output: Some(dir.path().join("merged.txt")),
        };
        let mut session = Self {
            view: MergeView::over(paths, Titles::default(), &context(), 11),
            ctx: egui::Context::default(),
            dir,
        };
        session.run_until_ready();
        session
    }

    fn frame(&mut self) {
        self.frame_with(Vec::new());
    }

    fn frame_with(&mut self, events: Vec<egui::Event>) {
        self.view.tick();
        let view = &mut self.view;
        let held = context();
        let input = if events.is_empty() {
            sized_input(WINDOW[0], WINDOW[1])
        } else {
            event_input(WINDOW[0], WINDOW[1], events)
        };
        let _ = self.ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                view.ui(ui, &held);
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
        panic!("the merge never became ready");
    }

    /// Paint frames until the view has taken the outcome of the write.
    ///
    /// Waiting on the file alone is not enough: the bytes reach the disk
    /// before the view is told the write ended, so everything the view reports
    /// about the save is still the state from before it.
    fn wait_for_save(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            self.frame();
            if !self.view.is_saving() {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("the save never finished");
    }

    fn wait_for_output(&mut self) -> String {
        self.wait_for_save();
        std::fs::read_to_string(self.output_path()).expect("the output was never written")
    }

    fn output_path(&self) -> PathBuf {
        self.dir.path().join("merged.txt")
    }
}

#[test]
fn a_resolved_merge_writes_the_output_the_pane_shows() {
    let mut session = Session::open("a\nL\nc\n", Some("a\nb\nc\n"), "a\nR\nc\n");
    session.view.run(Command::NextConflict);
    session.view.run(Command::TakeLeft);
    assert_eq!(session.view.model().totals().conflicts_remaining, 0);
    session.view.run(Command::SaveFile);
    assert_eq!(session.wait_for_output(), "a\nL\nc\n");
    assert_eq!(session.view.exit_code(), Some(0));
}

#[test]
fn saving_with_conflicts_writes_markers_only_after_the_question_is_answered() {
    let mut session = Session::open("a\nL\nc\n", Some("a\nb\nc\n"), "a\nR\nc\n");
    session.view.run(Command::SaveFile);
    assert_eq!(session.view.question(), Some(&Question::SaveWithConflicts));
    session.frame();
    assert!(!session.output_path().exists());
    session.view.answer(true);
    let written = session.wait_for_output();
    assert!(written.contains("<<<<<<<"), "{written}");
    assert!(written.contains(">>>>>>>"), "{written}");
    assert_eq!(session.view.exit_code(), Some(14));
}

#[test]
fn an_output_written_by_someone_else_before_the_first_save_is_not_overwritten() {
    let mut session = Session::open("a\nL\nc\n", Some("a\nb\nc\n"), "a\nR\nc\n");
    session.view.run(Command::NextConflict);
    session.view.run(Command::TakeLeft);
    // The output did not exist when the session started, so a file under that
    // name is someone else's work and the save must ask first.
    std::fs::write(session.output_path(), b"someone else wrote this\n").unwrap();
    session.view.run(Command::SaveFile);
    session.wait_for_save();
    assert_eq!(session.view.question(), Some(&Question::OverwriteChanged));
    assert_eq!(
        std::fs::read_to_string(session.output_path()).unwrap(),
        "someone else wrote this\n"
    );
    session.view.answer(true);
    assert_eq!(session.wait_for_output(), "a\nL\nc\n");
}

#[test]
fn a_session_that_never_writes_its_conflicts_reports_that_it_did_not() {
    let session = Session::open("a\nL\nc\n", Some("a\nb\nc\n"), "a\nR\nc\n");
    assert_eq!(session.view.exit_code(), Some(101));
}

#[test]
fn typing_in_the_output_pane_replaces_the_section_it_lands_in() {
    let mut session = Session::open("a\nL\nc\n", Some("a\nb\nc\n"), "a\nR\nc\n");
    session.view.run(Command::NextConflict);
    session.frame();
    assert_eq!(session.view.focused_pane(), Pane::Output);
    session.frame_with(vec![egui::Event::Text("X".to_owned())]);
    assert!(
        session.view.output_text().contains('X'),
        "{}",
        session.view.output_text()
    );
    assert_eq!(session.view.model().totals().edited, 1);
    assert!(session.view.is_modified());
}

#[test]
fn taking_a_side_again_restores_the_section_from_that_input() {
    let mut session = Session::open("a\nL\nc\n", Some("a\nb\nc\n"), "a\nR\nc\n");
    session.view.run(Command::NextConflict);
    session.frame();
    session.frame_with(vec![egui::Event::Text("X".to_owned())]);
    assert_eq!(session.view.model().totals().edited, 1);
    session.view.run(Command::TakeRight);
    assert_eq!(session.view.output_text(), "a\nR\nc\n");
    assert_eq!(session.view.model().totals().edited, 0);
}

#[test]
fn a_reload_keeps_a_decision_whose_region_did_not_change() {
    let mut session = Session::open(
        "a\nL\nc\np\nq\nr\ns\n",
        Some("a\nb\nc\np\nq\nr\ns\n"),
        "a\nR\nc\np\nq\nr\ns\n",
    );
    session.view.run(Command::NextConflict);
    session.view.run(Command::TakeRight);
    assert_eq!(session.view.model().totals().conflicts_remaining, 0);
    write(session.dir.path(), "left.txt", "a\nL\nc\np\nq\nr\ns\nt\n");
    session.view.run(Command::Reload);
    session.run_until_ready();
    assert_eq!(session.view.model().totals().conflicts_remaining, 0);
    assert!(session.view.output_text().contains("\nR\n"));
}

#[test]
fn a_newer_request_supersedes_the_one_running() {
    let mut session = Session::open("a\nOLD\n", Some("a\nb\n"), "a\nb\n");
    write(session.dir.path(), "left.txt", &"old row\n".repeat(100_000));
    session.view.run(Command::Reload);
    assert!(!session.view.is_ready());
    std::thread::sleep(Duration::from_millis(10));
    write(session.dir.path(), "left.txt", "a\nLATEST\n");
    session.view.run(Command::Reload);
    session.run_until_ready();
    assert_eq!(session.view.output_text(), "a\nLATEST\n");
}

#[test]
fn cancelling_a_run_does_not_apply_the_cancelled_input() {
    let mut session = Session::open("a\nL\n", Some("a\nb\n"), "a\nb\n");
    let previous_output = session.view.output_text();
    write(
        session.dir.path(),
        "left.txt",
        &"cancelled row\n".repeat(100_000),
    );
    session.view.run(Command::Reload);
    assert!(!session.view.is_ready());
    std::thread::sleep(Duration::from_millis(10));
    session.view.run(Command::Cancel);
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && !SessionView::is_ready(&session.view) {
        session.frame();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(SessionView::is_ready(&session.view));
    let actual = session.view.output_text();
    assert!(
        actual == previous_output,
        "cancelled reload applied {} output lines",
        actual.lines().count()
    );
}

#[test]
fn a_tab_with_an_unwritten_output_asks_before_it_closes() {
    let mut session = Session::open("a\nL\nc\n", Some("a\nb\nc\n"), "a\nR\nc\n");
    session.view.run(Command::NextConflict);
    session.view.run(Command::TakeLeft);
    assert!(session.view.is_modified());
    assert!(!session.view.may_close());
    assert_eq!(session.view.question(), Some(&Question::CloseModified));
    session.view.answer(true);
    assert!(session.view.may_close());
}

#[test]
fn an_output_that_changed_on_disk_is_not_overwritten_without_an_answer() {
    let mut session = Session::open("a\nL\nc\n", Some("a\nb\nc\n"), "a\nR\nc\n");
    session.view.run(Command::NextConflict);
    session.view.run(Command::TakeLeft);
    session.view.run(Command::SaveFile);
    assert_eq!(session.wait_for_output(), "a\nL\nc\n");
    std::fs::write(session.output_path(), b"someone else wrote this\n").unwrap();
    session.view.run(Command::TakeRight);
    session.view.run(Command::SaveFile);
    session.wait_for_save();
    assert_eq!(session.view.question(), Some(&Question::OverwriteChanged));
    assert_eq!(
        std::fs::read_to_string(session.output_path()).unwrap(),
        "someone else wrote this\n"
    );
}

impl Session {
    /// A merge with no output path.
    fn open_without_output(left: &str, center: Option<&str>, right: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let paths = MergePaths {
            left: write(dir.path(), "left.txt", left),
            center: center.map(|text| write(dir.path(), "center.txt", text)),
            right: write(dir.path(), "right.txt", right),
            output: None,
        };
        let mut session = Self {
            view: MergeView::over(paths, Titles::default(), &context(), 12),
            ctx: egui::Context::default(),
            dir,
        };
        session.run_until_ready();
        session
    }
}

/// A session where nothing was taken and nothing was typed holds nothing a
/// close could lose, so it closes without a question, with an output path or
/// without one.
#[test]
fn an_untouched_session_closes_without_a_question() {
    for mut session in [
        Session::open("a\nL\nc\n", Some("a\nb\nc\n"), "a\nR\nc\n"),
        Session::open_without_output("a\nL\nc\n", Some("a\nb\nc\n"), "a\nR\nc\n"),
    ] {
        assert!(!session.view.is_modified());
        assert!(session.view.may_close());
        assert_eq!(session.view.question(), None);
    }
}

/// A decision a reload carries over is still not written, so the session
/// still asks before it closes.
#[test]
fn a_decision_carried_over_a_reload_still_asks_before_it_closes() {
    let mut session = Session::open("a\nL\nc\n", Some("a\nb\nc\n"), "a\nR\nc\n");
    session.view.run(Command::NextConflict);
    session.view.run(Command::TakeLeft);
    session.view.run(Command::Reload);
    session.run_until_ready();
    assert_eq!(session.view.output_text(), "a\nL\nc\n");
    assert!(session.view.is_modified());
    assert!(!session.view.may_close());
    assert_eq!(session.view.question(), Some(&Question::CloseModified));
}

/// A reload of a session that holds no decision leaves nothing to ask about.
#[test]
fn an_untouched_session_stays_unmodified_after_a_reload() {
    let mut session = Session::open("a\nL\nc\n", Some("a\nb\nc\n"), "a\nR\nc\n");
    session.view.run(Command::Reload);
    session.run_until_ready();
    assert!(!session.view.is_modified());
    assert!(session.view.may_close());
}

/// With the editing switch of the Specs page on, no take, typing or save
/// reaches the output, and nothing is written. With the switch off again, a
/// take lands and the view reports the unwritten change.
#[test]
fn the_editing_switch_keeps_the_output_as_it_is() {
    let mut session = Session::open("a\nL\nc\n", Some("a\nb\nc\n"), "a\nR\nc\n");
    let mut settings = session.view.settings().unwrap();
    settings.specs_mut().unwrap().disable_editing = true;
    session.view.apply_settings(&settings);
    session.run_until_ready();
    let before = session.view.output_text();
    session.view.run(Command::NextConflict);
    for command in [Command::TakeLeft, Command::Paste, Command::SaveFile] {
        assert!(!session.view.accepts(command), "{command:?}");
    }
    session.view.run(Command::TakeLeft);
    session.frame();
    session.frame_with(vec![egui::Event::Text("X".to_owned())]);
    session.view.run(Command::SaveFile);
    session.frame();
    assert_eq!(session.view.output_text(), before);
    assert!(!session.view.is_modified());
    assert!(!session.view.holds_unwritten_edits());
    assert!(!session.output_path().exists());

    settings.specs_mut().unwrap().disable_editing = false;
    session.view.apply_settings(&settings);
    session.run_until_ready();
    session.view.run(Command::NextConflict);
    session.view.run(Command::TakeLeft);
    assert_eq!(session.view.model().totals().conflicts_remaining, 0);
    assert!(session.view.holds_unwritten_edits());
}
