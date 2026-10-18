//! The folder views answer the keys the menus print: next and previous
//! difference, find a file name, full refresh and select all files.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

use ca_session::SessionKind;
use ca_ui::command::{route, Command, Keystroke, MenuView};
use ca_ui::testing::context;
use ca_ui::testing::probe::{self, Probe};
use ca_ui::view::{OpenRequest, SessionView};
use ca_view_folder::{FolderMergeView, FolderSyncView, FolderView};
use egui::Key;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const PATIENCE: Duration = Duration::from_secs(30);
const WINDOW: [f32; 2] = [2_400.0, 900.0];

fn wait(mut ready: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + PATIENCE;
    loop {
        if ready() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        for side in ["left", "right", "center", "output", "journals"] {
            std::fs::create_dir_all(dir.path().join(side)).unwrap();
        }
        let fixture = Self { dir };
        fixture.write("left", "same.txt", b"alpha\n");
        fixture.write("right", "same.txt", b"alpha\n");
        fixture.write("left", "sub/differs.txt", b"one\n");
        fixture.write("right", "sub/differs.txt", b"two\n");
        fixture.write("left", "sub/deeper/orphan.txt", b"only here\n");
        fixture.write("left", "newer.txt", b"left body\n");
        fixture
    }

    fn path(&self, side: &str) -> PathBuf {
        self.dir.path().join(side)
    }

    fn write(&self, side: &str, rel: &str, body: &[u8]) {
        let path = self.path(side).join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn view(&self) -> FolderView {
        let mut view = FolderView::with_journal_directory(
            self.path("left"),
            self.path("right"),
            &context(),
            1,
            self.path("journals"),
        );
        ready(&mut view);
        view
    }

    fn sync_view(&self) -> FolderSyncView {
        let mut view = FolderView::sync_with_journal_directory(
            self.path("left"),
            self.path("right"),
            &context(),
            2,
            self.path("journals"),
        );
        ready(&mut view);
        FolderSyncView::from(view)
    }

    fn merge_view(&self) -> FolderMergeView {
        for side in ["left", "center", "right"] {
            self.write(side, "dir/inner.txt", b"same");
            self.write(side, "Report.txt", b"same");
        }
        self.write("left", "added.txt", b"from the left");
        let request = OpenRequest::new(
            SessionKind::FolderMerge,
            self.path("left"),
            self.path("right"),
        )
        .with_center(Some(self.path("center")))
        .with_output(Some(self.path("output")));
        let mut view =
            FolderMergeView::with_journal_directory(&request, &context(), 3, self.path("journals"));
        assert!(wait(|| {
            view.tick();
            view.tree().is_some()
        }));
        view
    }
}

fn ready(view: &mut FolderView) {
    assert!(wait(|| {
        view.tick();
        SessionView::is_ready(view)
    }));
}

fn cursor(view: &FolderView) -> Option<PathBuf> {
    view.selection().cursor().map(|(_, rel)| rel.to_path_buf())
}

fn expect(view: &mut impl SessionView, command: Command) {
    assert!(view.accepts(command), "{command:?} was refused");
    view.run(command);
}

fn refused(view: &impl SessionView, command: Command) {
    assert!(!view.accepts(command), "{command:?} was accepted");
    assert!(
        view.refusal(command)
            .is_some_and(|reason| !reason.is_empty()),
        "{command:?} gave no reason"
    );
}

#[test]
fn the_folder_keys_reach_the_commands() {
    let key = |stroke, command| {
        assert_eq!(route(MenuView::Folder, stroke), Some(command), "{stroke:?}");
    };
    key(Keystroke::with_command(Key::N), Command::NextDifference);
    key(Keystroke::with_command(Key::P), Command::PreviousDifference);
    key(Keystroke::with_command(Key::F), Command::Find);
    key(Keystroke::plain(Key::F3), Command::FindNext);
    key(Keystroke::with_shift(Key::F3), Command::FindPrevious);
    key(Keystroke::with_command(Key::F5), Command::Recompare);
    key(
        Keystroke::with_command_shift(Key::A),
        Command::SelectAllFiles,
    );
    for (stroke, command) in [
        (Keystroke::with_command(Key::F), Command::Find),
        (Keystroke::plain(Key::F3), Command::FindNext),
        (Keystroke::with_shift(Key::F3), Command::FindPrevious),
        (Keystroke::with_command(Key::F5), Command::Recompare),
        (
            Keystroke::with_command_shift(Key::A),
            Command::SelectAllFiles,
        ),
    ] {
        assert_eq!(route(MenuView::Merge, stroke), Some(command), "{stroke:?}");
    }
}

#[test]
fn next_and_previous_difference_walk_the_differences_and_open_folders() {
    let fixture = Fixture::new();
    let mut view = fixture.view();
    expect(&mut view, Command::CompareContents);
    ready(&mut view);
    let closed = view.rows().len();
    let mut seen = Vec::new();
    for _ in 0..8 {
        expect(&mut view, Command::NextDifference);
        let at = cursor(&view).unwrap();
        if seen.last() == Some(&at) {
            break;
        }
        seen.push(at);
    }
    let names: Vec<&str> = seen.iter().map(|rel| rel.to_str().unwrap()).collect();
    assert!(names.contains(&"newer.txt"), "{names:?}");
    assert!(names.iter().any(|name| name.ends_with("differs.txt")));
    assert!(names.iter().any(|name| name.ends_with("orphan.txt")));
    assert!(!names.contains(&"same.txt") && !names.contains(&"sub"));
    assert!(view.rows().len() > closed, "no folder was opened");

    let last = cursor(&view).unwrap();
    expect(&mut view, Command::PreviousDifference);
    let before = cursor(&view).unwrap();
    assert_ne!(last, before);
    assert!(seen.contains(&before));
}

#[test]
fn difference_moves_skip_rows_the_filter_hides() {
    let fixture = Fixture::new();
    let mut view = fixture.view();
    let mut known = Probe::new(WINDOW[0], WINDOW[1]);
    probe::settle(&mut known, &mut view);
    probe::press_view(&mut known, &mut view, "Same").unwrap();
    assert_eq!(view.report_filter(), "matches");
    refused(&view, Command::NextDifference);
    refused(&view, Command::PreviousDifference);
    assert_eq!(
        view.refusal(Command::NextDifference),
        Some("There is no difference to move to")
    );
    probe::press_view(&mut known, &mut view, "Diffs").unwrap();
    expect(&mut view, Command::NextDifference);
    assert_ne!(cursor(&view).unwrap(), Path::new("same.txt"));
}

#[test]
fn difference_moves_refuse_an_empty_comparison() {
    let dir = tempfile::tempdir().unwrap();
    for side in ["left", "right", "journals"] {
        std::fs::create_dir_all(dir.path().join(side)).unwrap();
    }
    let mut view = FolderView::with_journal_directory(
        dir.path().join("left"),
        dir.path().join("right"),
        &context(),
        4,
        dir.path().join("journals"),
    );
    ready(&mut view);
    for command in [
        Command::NextDifference,
        Command::PreviousDifference,
        Command::Find,
        Command::FindNext,
        Command::FindPrevious,
    ] {
        refused(&view, command);
    }
}

#[test]
fn find_filename_finds_ignoring_case_and_wraps() {
    let fixture = Fixture::new();
    fixture.write("left", "sub/Second-DIFF.txt", b"x");
    fixture.write("right", "sub/Second-DIFF.txt", b"x");
    let mut view = fixture.view();
    refused(&view, Command::FindNext);
    assert_eq!(
        view.refusal(Command::FindNext),
        Some("Enter a name with Find Filename first")
    );
    expect(&mut view, Command::Find);
    assert!(view.find_is_open());
    view.set_find_text("diff");
    expect(&mut view, Command::FindNext);
    let first = cursor(&view).unwrap();
    assert!(first.to_str().unwrap().to_lowercase().contains("diff"));
    expect(&mut view, Command::FindNext);
    let second = cursor(&view).unwrap();
    assert_ne!(first, second);
    expect(&mut view, Command::FindNext);
    assert_eq!(cursor(&view).unwrap(), first, "the search did not wrap");
    expect(&mut view, Command::FindPrevious);
    assert_eq!(cursor(&view).unwrap(), second);

    view.set_find_text("no such name");
    refused(&view, Command::FindNext);
    assert_eq!(
        view.refusal(Command::FindPrevious),
        Some("No file name matches the search text")
    );
}

#[test]
fn the_find_bar_controls_are_pressed_by_label() {
    let fixture = Fixture::new();
    let mut view = fixture.view();
    let mut known = Probe::new(WINDOW[0], WINDOW[1]);
    probe::settle(&mut known, &mut view);
    assert!(known.find("Find Next Filename").is_err(), "the bar is shut");
    expect(&mut view, Command::Find);
    probe::settle(&mut known, &mut view);
    for label in [
        "Find filename",
        "Find Next Filename",
        "Find Previous Filename",
        "Close Find",
    ] {
        known.find(label).unwrap();
    }
    view.set_find_text("newer");
    probe::press_view(&mut known, &mut view, "Find Next Filename").unwrap();
    assert_eq!(cursor(&view).unwrap(), Path::new("newer.txt"));
    probe::press_view(&mut known, &mut view, "Find Previous Filename").unwrap();
    assert_eq!(cursor(&view).unwrap(), Path::new("newer.txt"));
    probe::press_view(&mut known, &mut view, "Close Find").unwrap();
    assert!(!view.find_is_open());
    assert_eq!(view.find_text(), "newer");
    assert!(view.accepts(Command::FindNext));
}

#[test]
fn full_refresh_reads_both_folders_again() {
    let fixture = Fixture::new();
    let mut view = fixture.view();
    fixture.write("right", "arrived.txt", b"new");
    view.set_find_text("arrived");
    refused(&view, Command::FindNext);
    expect(&mut view, Command::Recompare);
    refused(&view, Command::Recompare);
    ready(&mut view);
    assert!(view.accepts(Command::Recompare));
    expect(&mut view, Command::FindNext);
    assert_eq!(cursor(&view).unwrap(), Path::new("arrived.txt"));
}

#[test]
fn the_sync_view_answers_the_same_keys() {
    let fixture = Fixture::new();
    let mut view = fixture.sync_view();
    expect(&mut view, Command::NextDifference);
    let first = cursor(view.view_mut()).unwrap();
    assert_ne!(first, Path::new("same.txt"));
    expect(&mut view, Command::Find);
    view.view_mut().set_find_text("same");
    expect(&mut view, Command::FindNext);
    assert_eq!(cursor(view.view_mut()).unwrap(), Path::new("same.txt"));
    expect(&mut view, Command::FindPrevious);
    expect(&mut view, Command::Recompare);
    refused(&view, Command::Recompare);
    assert!(wait(|| {
        view.tick();
        view.is_ready()
    }));
    assert!(view.accepts(Command::Recompare));
    view.view_mut().set_find_text("no such name");
    refused(&view, Command::FindNext);
}

#[test]
fn the_merge_view_finds_names_selects_files_and_refreshes() {
    let fixture = Fixture::new();
    let mut view = fixture.merge_view();
    refused(&view, Command::FindNext);
    expect(&mut view, Command::Find);
    assert!(view.find_is_open());
    view.set_find_text("report");
    expect(&mut view, Command::FindNext);
    assert_eq!(
        view.cursor_row().map(|row| row.name.clone()),
        Some("Report.txt".to_owned())
    );
    expect(&mut view, Command::FindNext);
    assert_eq!(
        view.cursor_row().map(|row| row.name.clone()),
        Some("Report.txt".to_owned()),
        "the only match must stay selected after wrapping"
    );
    expect(&mut view, Command::FindPrevious);
    view.set_find_text("no such name");
    refused(&view, Command::FindPrevious);

    expect(&mut view, Command::SelectAllFiles);
    let files: Vec<_> = view
        .visible_rows()
        .iter()
        .filter(|row| !row.is_dir)
        .map(|row| row.rel.clone())
        .collect();
    assert!(!files.is_empty());
    assert_eq!(view.selected().len(), files.len());
    assert!(!view.selected().contains(Path::new("dir")));

    fixture.write("right", "arrived.txt", b"new");
    expect(&mut view, Command::Recompare);
    refused(&view, Command::Recompare);
    assert!(wait(|| {
        view.tick();
        view.tree().is_some()
    }));
    assert!(view
        .visible_rows()
        .iter()
        .any(|row| row.name == "arrived.txt"));
}

#[test]
fn the_merge_find_bar_controls_are_pressed_by_label() {
    let fixture = Fixture::new();
    let mut view = fixture.merge_view();
    expect(&mut view, Command::Find);
    let mut known = Probe::new(WINDOW[0], WINDOW[1]);
    probe::settle(&mut known, &mut view);
    for label in [
        "Find filename",
        "Find Next Filename",
        "Find Previous Filename",
        "Close Find",
    ] {
        known.find(label).unwrap();
    }
    view.set_find_text("added");
    probe::press_view(&mut known, &mut view, "Find Next Filename").unwrap();
    assert_eq!(
        view.cursor_row().map(|row| row.name.clone()),
        Some("added.txt".to_owned())
    );
    probe::press_view(&mut known, &mut view, "Close Find").unwrap();
    assert!(!view.find_is_open());
}
