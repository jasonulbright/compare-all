//! The folder views answer the keys the menus print: next and previous
//! difference, find a file name, full refresh and select all files.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

use ca_session::SessionKind;
use ca_ui::command::{route, route_with, Command, Keystroke, MenuView, ShortcutTable};
use ca_ui::testing::context;
use ca_ui::testing::probe::{self, Probe};
use ca_ui::view::{OpenRequest, SessionView};
use ca_view_folder::{FolderMergeView, FolderSyncView, FolderView};
use egui::Key;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

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

/// The modified time of every file the tests write.
const FILE_TIME: Duration = Duration::from_secs(1_700_000_000);

/// Write `body` to `path` with the fixed modified time.
///
/// The default quick test compares modified times to the second, so two equal
/// files written on either side of a second boundary would compare as
/// different.
fn write_file(path: &Path, body: &[u8]) {
    std::fs::write(path, body).unwrap();
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(SystemTime::UNIX_EPOCH + FILE_TIME)
        .unwrap();
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
        write_file(&path, body);
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
fn ctrl_r_and_ctrl_l_copy_the_selection_in_a_folder_comparison() {
    let fixture = Fixture::new();
    let mut view = fixture.view();
    refused(&view, Command::CopyToRight);
    expect(&mut view, Command::SelectAll);
    for (stroke, command) in [
        (Keystroke::with_command(Key::R), Command::CopyToRight),
        (Keystroke::with_command(Key::L), Command::CopyToLeft),
    ] {
        assert_eq!(route(MenuView::Folder, stroke), Some(command), "{stroke:?}");
        assert!(view.accepts(command), "{command:?} was refused");
    }
    view.run(Command::CopyToRight);
    assert!(
        !view.accepts(Command::CopyToLeft),
        "the copy to the right is running or asking, so a second one waits"
    );
}

#[test]
fn a_folder_sync_has_no_copy_keys() {
    let fixture = Fixture::new();
    let sync = FolderSyncView::from(fixture.view());
    assert_eq!(sync.menu_view(), MenuView::FolderSync);
    for stroke in [
        Keystroke::with_command(Key::R),
        Keystroke::with_command(Key::L),
    ] {
        assert_eq!(route(sync.menu_view(), stroke), None, "{stroke:?}");
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
    while view.accepts(Command::NextDifference) && seen.len() < 8 {
        view.run(Command::NextDifference);
        seen.push(cursor(&view).unwrap());
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

/// Draw `view` for one frame that carries `events`.
fn type_into(known: &mut Probe, view: &mut impl SessionView, events: Vec<egui::Event>) {
    let mut run = |ctx: &egui::Context| {
        let _ = probe::draw_view(view, ctx);
    };
    let _ = known.frame(events, &mut run);
    known.idle(&mut run);
}

fn enter() -> egui::Event {
    egui::Event::Key {
        key: Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::default(),
    }
}

#[test]
fn the_find_field_is_clicked_and_typed_into_by_label() {
    let fixture = Fixture::new();
    let mut view = fixture.view();
    let mut known = Probe::new(WINDOW[0], WINDOW[1]);
    expect(&mut view, Command::Find);
    probe::settle(&mut known, &mut view);
    probe::press_view(&mut known, &mut view, "Find filename").unwrap();
    type_into(
        &mut known,
        &mut view,
        vec![egui::Event::Text("NEWER".to_owned())],
    );
    assert_eq!(view.find_text(), "NEWER");
    assert_eq!(cursor(&view).unwrap(), Path::new("newer.txt"));
    type_into(
        &mut known,
        &mut view,
        vec![egui::Event::Text("s".to_owned())],
    );
    assert_eq!(view.find_text(), "NEWERs");
    refused(&view, Command::FindNext);
    type_into(
        &mut known,
        &mut view,
        vec![egui::Event::Key {
            key: Key::Backspace,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        }],
    );
    assert_eq!(view.find_text(), "NEWER");
    type_into(&mut known, &mut view, vec![enter()]);
    assert!(view.accepts(Command::FindNext));
}

#[test]
fn the_merge_find_field_is_clicked_and_typed_into_by_label() {
    let fixture = Fixture::new();
    let mut view = fixture.merge_view();
    let mut known = Probe::new(WINDOW[0], WINDOW[1]);
    expect(&mut view, Command::Find);
    probe::settle(&mut known, &mut view);
    probe::press_view(&mut known, &mut view, "Find filename").unwrap();
    type_into(
        &mut known,
        &mut view,
        vec![egui::Event::Text("ADDED".to_owned())],
    );
    assert_eq!(view.find_text(), "ADDED");
    assert_eq!(
        view.cursor_row().map(|row| row.name.clone()),
        Some("added.txt".to_owned())
    );
}

#[test]
fn difference_moves_refuse_a_direction_with_no_target() {
    let fixture = Fixture::new();
    let mut view = fixture.view();
    expect(&mut view, Command::CompareContents);
    ready(&mut view);
    expect(&mut view, Command::NextDifference);
    refused(&view, Command::PreviousDifference);
    let mut moves = 0;
    while view.accepts(Command::NextDifference) {
        view.run(Command::NextDifference);
        moves += 1;
        assert!(moves < 16, "Next Difference never ran out of targets");
    }
    assert!(moves > 0);
    refused(&view, Command::NextDifference);
    expect(&mut view, Command::PreviousDifference);
}

#[test]
fn merge_difference_moves_refuse_a_direction_with_no_target() {
    let fixture = Fixture::new();
    let mut view = fixture.merge_view();
    expect(&mut view, Command::NextDifference);
    refused(&view, Command::PreviousDifference);
    let mut moves = 0;
    while view.accepts(Command::NextDifference) {
        view.run(Command::NextDifference);
        moves += 1;
        assert!(moves < 32, "Next Difference never ran out of targets");
    }
    refused(&view, Command::NextDifference);
    if moves > 0 {
        expect(&mut view, Command::PreviousDifference);
    }
}

#[test]
fn the_merge_view_moves_between_differences_by_key() {
    let fixture = Fixture::new();
    let mut view = fixture.merge_view();
    let table = ShortcutTable::built_in();
    let bar = view.menu_view();
    let next = route_with(bar, Keystroke::with_command(Key::N), &table);
    let previous = route_with(bar, Keystroke::with_command(Key::P), &table);
    assert_eq!(next, Some(Command::NextDifference));
    assert_eq!(previous, Some(Command::PreviousDifference));
    assert_eq!(Command::NextDifference.shortcut_in(bar), Some("Ctrl+N"));
    assert_eq!(Command::PreviousDifference.shortcut_in(bar), Some("Ctrl+P"));
    expect(&mut view, Command::NextDifference);
    assert!(view.cursor_row().is_some());

    assert_eq!(
        route_with(MenuView::Merge, Keystroke::with_command(Key::N), &table),
        Some(Command::NextSection)
    );
    assert_eq!(
        route_with(MenuView::Merge, Keystroke::with_command(Key::P), &table),
        Some(Command::PreviousSection)
    );
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

fn merge_cursor(view: &FolderMergeView) -> Option<PathBuf> {
    view.cursor_row().map(|row| row.rel.clone())
}

/// Put the cursor on `name` with every row shown, then apply `filter`.
fn hide_merge_cursor(view: &mut FolderMergeView, name: &str, filter: Command) {
    expect(view, Command::ShowAll);
    assert!(view.select(Path::new(name)));
    expect(view, filter);
    assert!(view.visible_rows().iter().all(|row| row.name != name));
}

#[test]
fn merge_moves_continue_from_a_cursor_the_filter_hides() {
    let dir = tempfile::tempdir().unwrap();
    for side in ["left", "center", "right", "output", "journals"] {
        std::fs::create_dir_all(dir.path().join(side)).unwrap();
    }
    for name in ["a.txt", "b.txt", "c.txt", "d.txt"] {
        for side in ["center", "right"] {
            write_file(&dir.path().join(side).join(name), b"base");
        }
        let left: &[u8] = if matches!(name, "a.txt" | "c.txt") {
            b"left change"
        } else {
            b"base"
        };
        write_file(&dir.path().join("left").join(name), left);
    }
    let request = OpenRequest::new(
        SessionKind::FolderMerge,
        dir.path().join("left"),
        dir.path().join("right"),
    )
    .with_center(Some(dir.path().join("center")))
    .with_output(Some(dir.path().join("output")));
    let mut view = FolderMergeView::with_journal_directory(
        &request,
        &context(),
        4,
        dir.path().join("journals"),
    );
    assert!(wait(|| {
        view.tick();
        view.tree().is_some()
    }));
    let names: Vec<_> = view
        .visible_rows()
        .iter()
        .map(|row| row.name.clone())
        .collect();
    assert_eq!(names, ["a.txt", "b.txt", "c.txt", "d.txt"]);

    hide_merge_cursor(&mut view, "b.txt", Command::ShowDifferences);
    expect(&mut view, Command::NextDifference);
    assert_eq!(merge_cursor(&view), Some(PathBuf::from("c.txt")));
    hide_merge_cursor(&mut view, "b.txt", Command::ShowDifferences);
    expect(&mut view, Command::PreviousDifference);
    assert_eq!(merge_cursor(&view), Some(PathBuf::from("a.txt")));

    view.set_find_text("txt");
    hide_merge_cursor(&mut view, "c.txt", Command::ShowSame);
    expect(&mut view, Command::FindNext);
    assert_eq!(merge_cursor(&view), Some(PathBuf::from("d.txt")));
    hide_merge_cursor(&mut view, "c.txt", Command::ShowSame);
    expect(&mut view, Command::FindPrevious);
    assert_eq!(merge_cursor(&view), Some(PathBuf::from("b.txt")));
}
