//! A folder merge leaves every mergeable and conflicting row for a merge by
//! hand. The view names those rows wherever it tells the user what a merge
//! did or will do, and never calls the output finished while one waits.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![allow(clippy::disallowed_methods, reason = "test setup writes fixture files")]

use ca_fs::{MergeStatus, Pane, Resolution};
use ca_session::SessionKind;
use ca_ui::command::Command;
use ca_ui::testing::context;
use ca_ui::testing::probe::{draw_view, painted_texts, Probe};
use ca_ui::view::{OpenRequest, SessionView};
use ca_view_folder::FolderMergeView;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// How long a test waits for background work before giving up.
const PATIENCE: Duration = Duration::from_secs(30);

/// The time of the ancestor copies. The sides are stamped four and eight
/// seconds later, a gap a file system with two second times keeps.
const T: u64 = 1_700_000_000;

/// A window that holds a whole dialog.
const WINDOW: [f32; 2] = [1_400.0, 1_000.0];

const FINISHED: &str = "The output already holds the merge result.";

fn stamp(path: &Path, secs: u64) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
        .unwrap();
}

fn poll_until(view: &mut FolderMergeView, ready: impl Fn(&FolderMergeView) -> bool) -> bool {
    let deadline = Instant::now() + PATIENCE;
    loop {
        view.tick();
        if ready(view) {
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
    fn empty() -> Self {
        let dir = tempfile::tempdir().unwrap();
        for side in ["left", "center", "right", "output", "journals"] {
            std::fs::create_dir_all(dir.path().join(side)).unwrap();
        }
        Self { dir }
    }

    /// One file only the left side added; one text file the right side edited
    /// and the left side only touched, which the quick test counts as a change
    /// on both sides; and one both sides changed on the same line.
    fn mixed() -> Self {
        let fixture = Self::empty();
        fixture.write("left", "added.txt", b"from the left\n", T + 4);
        fixture.write("center", "both.txt", b"one\ntwo\nthree\n", T);
        fixture.write("left", "both.txt", b"one\ntwo\nthree\n", T + 4);
        fixture.write("right", "both.txt", b"one\ntwo\nTHREE\n", T + 8);
        fixture.write("center", "clash.txt", b"one\ntwo\nthree\n", T);
        fixture.write("left", "clash.txt", b"one\nLEFT\nthree\n", T + 4);
        fixture.write("right", "clash.txt", b"one\nRIGHT SIDE\nthree\n", T + 8);
        fixture
    }

    fn path(&self, side: &str) -> PathBuf {
        self.dir.path().join(side)
    }

    fn write(&self, side: &str, rel: &str, body: &[u8], secs: u64) {
        let path = self.path(side).join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, body).unwrap();
        stamp(&path, secs);
    }

    fn read(&self, side: &str, rel: &str) -> Option<Vec<u8>> {
        std::fs::read(self.path(side).join(rel)).ok()
    }

    fn output_is_empty(&self) -> bool {
        std::fs::read_dir(self.path("output"))
            .unwrap()
            .next()
            .is_none()
    }

    fn request(&self) -> OpenRequest {
        OpenRequest::new(
            SessionKind::FolderMerge,
            self.path("left"),
            self.path("right"),
        )
        .with_center(Some(self.path("center")))
        .with_output(Some(self.path("output")))
    }

    fn view(&self) -> FolderMergeView {
        self.open(&self.request())
    }

    fn open(&self, request: &OpenRequest) -> FolderMergeView {
        let mut view =
            FolderMergeView::with_journal_directory(request, &context(), 1, self.path("journals"));
        // A test never reaches the real recycle bin: on some platforms that
        // call asks the desktop shell for permission and blocks on a dialog.
        view.operation_options_mut().use_recycle_bin = false;
        assert!(poll_until(&mut view, |view| view.tree().is_some()));
        view
    }
}

fn status_of(view: &FolderMergeView, rel: &str) -> MergeStatus {
    view.tree()
        .and_then(|tree| tree.rows.iter().find(|row| row.rel == Path::new(rel)))
        .unwrap_or_else(|| panic!("no row for {rel}"))
        .status
}

/// Run a merge with no confirmation and wait for its result or its message.
fn merge_at_once(view: &mut FolderMergeView) {
    view.set_confirm_merge(false);
    assert!(view.accepts(Command::MergeFolders));
    view.run(Command::MergeFolders);
    assert!(
        poll_until(view, |view| view.has_summary() || view.message().is_some()),
        "the merge produced neither a result nor a message"
    );
}

fn message(view: &FolderMergeView) -> String {
    view.message().unwrap_or_default().to_owned()
}

/// Every text one frame of the view painted, joined into one string.
fn frame_text(probe: &mut Probe, view: &mut FolderMergeView) -> String {
    let output = probe.frame(Vec::new(), &mut |ctx: &egui::Context| {
        let _ = draw_view(view, ctx);
    });
    painted_texts(&output).join("\n")
}

#[test]
fn three_copies_of_one_tree_made_at_different_times_name_every_file_left_for_a_merge_by_hand() {
    let fixture = Fixture::empty();
    for (side, secs) in [("center", T), ("left", T + 4), ("right", T + 8)] {
        for index in 0..5 {
            fixture.write(side, &format!("file{index}.txt"), b"unchanged body\n", secs);
        }
    }
    fixture.write("right", "file4.txt", b"edited on the right only\n", T + 8);
    let mut view = fixture.view();
    for index in 0..5 {
        assert_eq!(
            status_of(&view, &format!("file{index}.txt")),
            MergeStatus::Mergeable
        );
    }

    merge_at_once(&mut view);

    assert!(!view.has_summary(), "a plan with no step ran a batch");
    assert!(fixture.output_is_empty(), "the merge wrote a mergeable row");
    let text = message(&view);
    assert_ne!(
        text, FINISHED,
        "the output is called finished while it is empty"
    );
    assert!(text.starts_with("Nothing is copied. "), "{text}");
    assert!(
        text.contains("5 items need a merge by hand (5 mergeable, 0 conflicts)."),
        "{text}"
    );
    assert!(
        text.contains(
            "Items: file0.txt (mergeable, not in the output), \
             file1.txt (mergeable, not in the output), \
             file2.txt (mergeable, not in the output), \
             file3.txt (mergeable, not in the output), \
             file4.txt (mergeable, not in the output)."
        ),
        "{text}"
    );
}

#[test]
fn a_lone_conflict_and_an_empty_output_wait_for_a_merge_by_hand() {
    let fixture = Fixture::empty();
    fixture.write("center", "c.txt", b"one\ntwo\nthree\n", T);
    fixture.write("left", "c.txt", b"one\nLEFT\nthree\n", T + 4);
    fixture.write("right", "c.txt", b"one\nRIGHT SIDE\nthree\n", T + 8);
    let mut view = fixture.view();
    assert_eq!(status_of(&view, "c.txt"), MergeStatus::Conflict);

    merge_at_once(&mut view);

    assert!(fixture.read("output", "c.txt").is_none());
    let text = message(&view);
    assert_eq!(
        text,
        "Nothing is copied. 1 item needs a merge by hand (0 mergeable, 1 conflict). \
         The merge does not write it to the output. \
         To put its merge result in the output, merge it in Text Merge and save it. \
         To keep one input's copy instead, choose Take Left, Take Center or Take Right \
         and merge again. \
         Item: c.txt (conflict, not in the output)."
    );
}

#[test]
fn more_rows_than_a_message_lists_are_counted() {
    let fixture = Fixture::empty();
    for (side, secs) in [("center", T), ("left", T + 4), ("right", T + 8)] {
        for index in 0..7 {
            fixture.write(side, &format!("file{index}.txt"), b"unchanged body\n", secs);
        }
    }
    let mut view = fixture.view();

    merge_at_once(&mut view);

    let text = message(&view);
    assert!(
        text.contains("7 items need a merge by hand (7 mergeable, 0 conflicts)."),
        "{text}"
    );
    assert!(
        text.ends_with("file4.txt (mergeable, not in the output) and 2 more."),
        "{text}"
    );
    assert!(!text.contains("file5.txt"), "{text}");
}

#[test]
fn the_output_is_called_finished_only_when_no_row_waits_for_a_merge_by_hand() {
    let fixture = Fixture::empty();
    for side in ["center", "left", "right", "output"] {
        fixture.write(side, "same.txt", b"same\n", T);
    }
    let mut view = fixture.view();
    assert_eq!(status_of(&view, "same.txt"), MergeStatus::Unchanged);

    merge_at_once(&mut view);

    assert_eq!(message(&view), FINISHED);
}

#[test]
fn the_confirmation_the_result_and_the_status_text_name_the_rows_left_for_a_merge_by_hand() {
    let fixture = Fixture::mixed();
    let mut view = fixture.view();
    assert_eq!(status_of(&view, "both.txt"), MergeStatus::Mergeable);
    assert_eq!(status_of(&view, "clash.txt"), MergeStatus::Conflict);
    let mut probe = Probe::new(WINDOW[0], WINDOW[1]);
    let _ = frame_text(&mut probe, &mut view);
    let _ = frame_text(&mut probe, &mut view);

    probe::press_toolbar_merge(&mut probe, &mut view);
    assert!(poll_until(&mut view, FolderMergeView::is_confirming));
    let _ = frame_text(&mut probe, &mut view);
    let shown = frame_text(&mut probe, &mut view);
    for wanted in [
        "2 items need a merge by hand (1 mergeable, 1 conflict).",
        "The merge does not write them to the output.",
        "To put their merge result in the output, merge each one in Text Merge and save it.",
        "To keep one input's copy instead, choose Take Left, Take Center or Take Right and merge again.",
        "both.txt (mergeable, not in the output)",
        "clash.txt (conflict, not in the output)",
    ] {
        assert!(shown.contains(wanted), "the confirmation lacks {wanted:?}:\n{shown}");
    }

    probe::press_dialog(&mut probe, &mut view, "Merge");
    assert!(poll_until(&mut view, FolderMergeView::has_summary));
    let _ = frame_text(&mut probe, &mut view);
    let result = frame_text(&mut probe, &mut view);
    for wanted in [
        "1 of 1 steps completed",
        "Every step completed.",
        "2 items need a merge by hand (1 mergeable, 1 conflict).",
        "The merge does not write them to the output.",
        "both.txt (mergeable, not in the output)",
        "clash.txt (conflict, not in the output)",
    ] {
        assert!(
            result.contains(wanted),
            "the result lacks {wanted:?}:\n{result}"
        );
    }
    assert_eq!(
        fixture.read("output", "added.txt").as_deref(),
        Some(&b"from the left\n"[..])
    );
    assert!(fixture.read("output", "both.txt").is_none());
    assert!(fixture.read("output", "clash.txt").is_none());

    probe::press_dialog(&mut probe, &mut view, "Close");
    assert!(!view.has_summary());
    let text = message(&view);
    assert!(
        text.starts_with("2 items need a merge by hand (1 mergeable, 1 conflict). "),
        "{text}"
    );
    assert!(
        text.ends_with(
            "Items: both.txt (mergeable, not in the output), \
             clash.txt (conflict, not in the output)."
        ),
        "{text}"
    );
    assert!(poll_until(&mut view, |view| view.tree().is_some()));
    let status = frame_text(&mut probe, &mut view);
    assert!(
        status.contains(&text),
        "the status text lacks the notice:\n{status}"
    );
}

#[test]
fn the_report_marks_each_row_left_for_a_merge_by_hand() {
    let fixture = Fixture::mixed();
    let mut view = fixture.view();
    let (_, payload) = view.report_payload();
    let ca_ui::report::Payload::Folder(rows) = payload else {
        panic!("a folder merge writes a folder report");
    };
    let word = |name: &str| {
        rows.iter()
            .find(|row| row.relative_path == name)
            .unwrap_or_else(|| panic!("no report row for {name}"))
            .status
            .label()
    };
    assert_eq!(word("both.txt"), "Merge by hand");
    assert_eq!(word("clash.txt"), "Merge by hand");
    assert_eq!(word("added.txt"), "Left only");

    assert!(view.select(Path::new("clash.txt")));
    view.run(Command::TakeRight);
    assert_eq!(
        view.overrides().get(Path::new("clash.txt")),
        Some(&Resolution::Take(Pane::Right))
    );
    let (_, payload) = view.report_payload();
    let ca_ui::report::Payload::Folder(rows) = payload else {
        panic!("a folder merge writes a folder report");
    };
    let clash = rows
        .iter()
        .find(|row| row.relative_path == "clash.txt")
        .unwrap();
    assert_eq!(clash.status.label(), "Different");
}

#[test]
fn the_exit_code_reports_rows_left_for_a_merge_by_hand() {
    let fixture = Fixture::mixed();
    let mut view = fixture.view();
    assert_eq!(view.exit_code(), Some(101), "nothing is merged yet");

    merge_at_once(&mut view);
    assert!(poll_until(&mut view, FolderMergeView::has_summary));
    assert_eq!(
        view.exit_code(),
        Some(14),
        "two rows wait for a merge by hand"
    );
    view.close_summary();
    assert!(view.tree().is_none());
    assert_eq!(view.exit_code(), Some(14), "the comparison runs again");
    assert!(poll_until(&mut view, |view| view.tree().is_some()));
    assert_eq!(view.exit_code(), Some(14));

    view.run(Command::SelectAll);
    view.run(Command::TakeLeft);
    merge_at_once(&mut view);
    assert!(poll_until(&mut view, FolderMergeView::has_summary));
    assert_eq!(view.exit_code(), Some(0), "a Take resolved every row");
    assert!(view.message().is_none(), "{:?}", view.message());
    assert_eq!(
        fixture.read("output", "clash.txt").as_deref(),
        Some(&b"one\nLEFT\nthree\n"[..])
    );
}

#[test]
fn a_merge_with_no_row_left_for_a_person_exits_with_success() {
    let fixture = Fixture::empty();
    fixture.write("left", "added.txt", b"from the left\n", T);
    let mut view = fixture.view();
    assert_eq!(view.exit_code(), Some(0));
    merge_at_once(&mut view);
    assert!(poll_until(&mut view, FolderMergeView::has_summary));
    assert_eq!(view.exit_code(), Some(0));
    assert!(view.message().is_none(), "{:?}", view.message());
}

fn close_and_rescan(view: &mut FolderMergeView) {
    view.close_summary();
    assert!(poll_until(view, |view| view.tree().is_some()));
}

/// The state word the report gives the row at `rel`.
fn report_word(view: &FolderMergeView, rel: &str) -> &'static str {
    let (_, payload) = view.report_payload();
    let ca_ui::report::Payload::Folder(rows) = payload else {
        panic!("a folder merge writes a folder report");
    };
    rows.iter()
        .find(|row| row.relative_path == rel)
        .unwrap_or_else(|| panic!("no report row for {rel}"))
        .status
        .label()
}

/// Write the merged texts of the mixed fixture into the output, after every
/// input was written, as a save from Text Merge does.
fn merge_mixed_by_hand(fixture: &Fixture) {
    fixture.write("output", "both.txt", b"one\ntwo\nTHREE, merged\n", T + 30);
    fixture.write(
        "output",
        "clash.txt",
        b"one\nLEFT and RIGHT SIDE\nthree\n",
        T + 30,
    );
}

fn reload(view: &mut FolderMergeView) {
    view.run(Command::Reload);
    assert!(poll_until(view, |view| view.tree().is_some()));
}

fn saved_file(path: PathBuf, conflicts: u32) -> ca_ui::view::SavedFile {
    ca_ui::view::SavedFile { path, conflicts }
}

/// Only `both.txt`: the ancestor at T, the left side the same text touched
/// at T + 4, the right side edited at T + 8.
fn one_mergeable_row() -> Fixture {
    let fixture = Fixture::empty();
    fixture.write("center", "both.txt", b"one\ntwo\nthree\n", T);
    fixture.write("left", "both.txt", b"one\ntwo\nthree\n", T + 4);
    fixture.write("right", "both.txt", b"one\ntwo\nTHREE\n", T + 8);
    fixture
}

/// The report word and the exit code with `body` in the output at `secs`.
fn state_with_output(body: &[u8], secs: u64) -> (&'static str, Option<i32>) {
    let fixture = one_mergeable_row();
    fixture.write("output", "both.txt", body, secs);
    let view = fixture.view();
    assert_eq!(status_of(&view, "both.txt"), MergeStatus::Mergeable);
    (report_word(&view, "both.txt"), view.exit_code())
}

/// A save from Text Merge into an output file older than the inputs, so
/// only the save marks the row; then `side` changes and the view reloads.
fn a_save_is_dropped_when_an_input_changes(side: &str) {
    let fixture = one_mergeable_row();
    let mut view = fixture.view();
    fixture.write("output", "both.txt", b"one\ntwo\nThree\n", T + 2);
    view.file_saved(&saved_file(fixture.path("output").join("both.txt"), 0));
    assert_eq!(report_word(&view, "both.txt"), "Merged by hand");
    reload(&mut view);
    assert_eq!(
        report_word(&view, "both.txt"),
        "Merged by hand",
        "no input changed"
    );

    fixture.write(side, "both.txt", b"one\ntwo\nthree, edited\n", T + 60);
    reload(&mut view);

    assert!(status_of(&view, "both.txt").needs_person());
    assert_eq!(
        report_word(&view, "both.txt"),
        "Merge by hand",
        "the save outlived a change of the {side} input"
    );
    assert_eq!(view.exit_code(), Some(101));
}

#[test]
fn a_save_is_dropped_when_the_left_input_changes() {
    a_save_is_dropped_when_an_input_changes("left");
}

#[test]
fn a_save_is_dropped_when_the_ancestor_changes() {
    a_save_is_dropped_when_an_input_changes("center");
}

#[test]
fn a_save_is_dropped_when_the_right_input_changes() {
    a_save_is_dropped_when_an_input_changes("right");
}

/// The output is the left input: Text Merge saves into the left folder, and
/// `side` changes after the save.
fn a_save_into_the_left_input_is_dropped_when_another_input_changes(side: &str) {
    let fixture = one_mergeable_row();
    let mut view = fixture.view();
    let Some(ca_session::settings::SessionSettings::FolderMerge(mut merge)) = view.settings()
    else {
        panic!("no folder merge settings")
    };
    merge.merge.target = ca_session::settings::folder::MergeTarget::Left;
    view.apply_settings(&ca_session::settings::SessionSettings::FolderMerge(merge));
    assert!(poll_until(&mut view, |view| view.tree().is_some()));
    fixture.write("left", "both.txt", b"one\ntwo\nTHREE, merged\n", T + 30);
    view.file_saved(&saved_file(fixture.path("left").join("both.txt"), 0));
    reload(&mut view);
    assert_eq!(report_word(&view, "both.txt"), "Merged by hand");

    fixture.write(side, "both.txt", b"one\ntwo\nthree, edited\n", T + 60);
    reload(&mut view);

    assert_eq!(
        report_word(&view, "both.txt"),
        "Merge by hand",
        "the save outlived a change of the {side} input"
    );
}

#[test]
fn a_save_into_the_left_input_is_dropped_when_the_ancestor_changes() {
    a_save_into_the_left_input_is_dropped_when_another_input_changes("center");
}

#[test]
fn a_save_into_the_left_input_is_dropped_when_the_right_input_changes() {
    a_save_into_the_left_input_is_dropped_when_another_input_changes("right");
}

#[test]
fn a_copy_of_one_input_with_a_new_time_is_not_a_merge_by_hand() {
    assert_eq!(
        state_with_output(b"one\ntwo\nTHREE\n", T + 30),
        ("Merge by hand", Some(101)),
        "a copy of the right input"
    );
    assert_eq!(
        state_with_output(b"one\ntwo\nthree\n", T + 30),
        ("Merge by hand", Some(101)),
        "a copy of the left input"
    );
}

#[test]
fn a_merge_result_with_the_size_of_an_input_and_other_bytes_is_a_merge_by_hand() {
    assert_eq!(
        state_with_output(b"one\ntwo\nThree\n", T + 30),
        ("Merged by hand", Some(0))
    );
}

#[test]
fn an_output_file_with_conflict_markers_is_not_a_merge_by_hand() {
    assert_eq!(
        state_with_output(
            b"one\ntwo\n<<<<<<< left\nthree\n=======\nTHREE\n>>>>>>> right\n",
            T + 30
        ),
        ("Merge by hand", Some(101))
    );
}

#[test]
fn a_save_with_conflicts_left_is_not_a_merge_by_hand() {
    let fixture = Fixture::empty();
    fixture.write("center", "clash.txt", b"one\ntwo\nthree\n", T);
    fixture.write("left", "clash.txt", b"one\nLEFT\nthree\n", T + 4);
    fixture.write("right", "clash.txt", b"one\nRIGHT SIDE\nthree\n", T + 8);
    let mut view = fixture.view();
    let marked = b"one\n<<<<<<< left\nLEFT\n=======\nRIGHT SIDE\n>>>>>>> right\nthree\n";
    fixture.write("output", "clash.txt", marked, T + 30);

    view.file_saved(&saved_file(fixture.path("output").join("clash.txt"), 1));

    assert_eq!(report_word(&view, "clash.txt"), "Merge by hand");
    assert_eq!(view.exit_code(), Some(101));
    let mut probe = Probe::new(WINDOW[0], WINDOW[1]);
    let _ = frame_text(&mut probe, &mut view);
    let shown = frame_text(&mut probe, &mut view);
    assert!(
        shown.lines().any(|line| line == "Saved with conflicts"),
        "the Action column does not say the save kept conflicts:\n{shown}"
    );
    reload(&mut view);
    assert_eq!(report_word(&view, "clash.txt"), "Merge by hand");
    merge_at_once(&mut view);
    assert!(
        message(&view).ends_with("Item: clash.txt (conflict, saved with conflicts)."),
        "{}",
        message(&view)
    );

    fixture.write(
        "output",
        "clash.txt",
        b"one\nLEFT and RIGHT\nthree\n",
        T + 40,
    );
    view.file_saved(&saved_file(fixture.path("output").join("clash.txt"), 0));
    assert_eq!(report_word(&view, "clash.txt"), "Merged by hand");
    assert_eq!(view.exit_code(), Some(0));
}

#[cfg(windows)]
#[test]
fn a_save_spelled_in_other_letter_case_marks_its_row() {
    let fixture = one_mergeable_row();
    let mut view = fixture.view();
    fixture.write("output", "both.txt", b"one\ntwo\nThree\n", T + 2);
    let upper = PathBuf::from(
        fixture
            .path("output")
            .join("BOTH.TXT")
            .to_string_lossy()
            .to_uppercase(),
    );
    view.file_saved(&saved_file(upper, 0));
    assert_eq!(report_word(&view, "both.txt"), "Merged by hand");
    assert_eq!(view.exit_code(), Some(0));
}

#[test]
fn a_save_into_an_output_named_with_a_trailing_separator_marks_its_row() {
    let fixture = one_mergeable_row();
    let output = PathBuf::from(format!(
        "{}{}",
        fixture.path("output").display(),
        std::path::MAIN_SEPARATOR
    ));
    let request = OpenRequest::new(
        SessionKind::FolderMerge,
        fixture.path("left"),
        fixture.path("right"),
    )
    .with_center(Some(fixture.path("center")))
    .with_output(Some(output));
    let mut view = fixture.open(&request);
    fixture.write("output", "both.txt", b"one\ntwo\nThree\n", T + 2);
    view.file_saved(&saved_file(fixture.path("output").join("both.txt"), 0));
    assert_eq!(report_word(&view, "both.txt"), "Merged by hand");
}

/// A link to the output folder: a junction on Windows, a symbolic link
/// elsewhere.
fn link_to(target: &Path, link: &Path) -> bool {
    #[cfg(windows)]
    {
        std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .is_ok_and(|out| out.status.success())
    }
    #[cfg(not(windows))]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
}

#[test]
fn a_save_through_a_link_to_the_output_marks_its_row() {
    let fixture = one_mergeable_row();
    let link = fixture.dir.path().join("output-link");
    assert!(link_to(&fixture.path("output"), &link), "no link was made");
    let mut view = fixture.view();
    fixture.write("output", "both.txt", b"one\ntwo\nThree\n", T + 2);

    view.file_saved(&saved_file(link.join("both.txt"), 0));

    assert!(
        poll_until(&mut view, |view| report_word(view, "both.txt")
            == "Merged by hand"),
        "a save through the link is not matched"
    );
}

#[test]
fn the_exit_code_follows_the_resolutions_and_not_the_output() {
    let fixture = Fixture::empty();
    fixture.write("left", "added.txt", b"from the left\n", T + 4);
    let mut view = fixture.view();
    let Some(ca_session::settings::SessionSettings::FolderMerge(mut merge)) = view.settings()
    else {
        panic!("no folder merge settings")
    };
    merge.merge.automatic_merge = false;
    view.apply_settings(&ca_session::settings::SessionSettings::FolderMerge(merge));
    assert!(poll_until(&mut view, |view| view.tree().is_some()));
    merge_at_once(&mut view);
    assert!(fixture.output_is_empty());
    assert_eq!(view.exit_code(), Some(0), "no row needs a merge by hand");

    let fixture = Fixture::mixed();
    let mut view = fixture.view();
    view.run(Command::SelectAll);
    view.run(Command::TakeLeft);
    assert!(fixture.output_is_empty());
    assert_eq!(
        view.exit_code(),
        Some(0),
        "a Take resolves every row before any merge runs"
    );
}

#[test]
fn a_merge_of_a_selected_row_names_the_rows_left_for_a_merge_by_hand_in_its_result() {
    let fixture = Fixture::mixed();
    let mut view = fixture.view();
    assert!(view.select(Path::new("added.txt")));
    let mut probe = Probe::new(WINDOW[0], WINDOW[1]);
    let _ = frame_text(&mut probe, &mut view);

    merge_at_once(&mut view);
    assert!(poll_until(&mut view, FolderMergeView::has_summary));
    let _ = frame_text(&mut probe, &mut view);
    let result = frame_text(&mut probe, &mut view);

    assert!(result.contains("Every step completed."), "{result}");
    assert!(
        result.contains("2 items need a merge by hand (1 mergeable, 1 conflict)."),
        "the result of a selection merge is silent on the rows that wait:\n{result}"
    );
    assert!(
        message(&view).starts_with("2 items need a merge by hand"),
        "{}",
        message(&view)
    );
}

#[test]
fn a_second_merge_of_the_same_selection_does_not_call_the_output_finished_while_rows_wait() {
    let fixture = Fixture::mixed();
    let mut view = fixture.view();
    assert!(view.select(Path::new("added.txt")));
    merge_at_once(&mut view);
    assert!(poll_until(&mut view, FolderMergeView::has_summary));
    close_and_rescan(&mut view);
    assert!(view.select(Path::new("added.txt")));

    merge_at_once(&mut view);

    assert!(fixture.read("output", "both.txt").is_none());
    let text = message(&view);
    assert_ne!(
        text, FINISHED,
        "the output is called finished while both.txt and clash.txt are absent"
    );
    assert!(
        text.starts_with("Nothing is copied. 2 items need a merge by hand"),
        "{text}"
    );
}

#[test]
fn a_merge_of_a_selection_does_not_call_the_output_finished_while_other_items_are_not_written() {
    let fixture = Fixture::empty();
    fixture.write("left", "chosen.txt", b"chosen\n", T);
    fixture.write("left", "other.txt", b"other\n", T);
    let mut view = fixture.view();
    assert!(view.select(Path::new("chosen.txt")));
    merge_at_once(&mut view);
    assert!(poll_until(&mut view, FolderMergeView::has_summary));
    close_and_rescan(&mut view);
    assert!(view.select(Path::new("chosen.txt")));

    merge_at_once(&mut view);

    assert!(fixture.read("output", "other.txt").is_none());
    assert_eq!(
        message(&view),
        "Nothing is copied. Only the selected items take part in the merge. \
         To write the other items to the output, select them and merge again."
    );
}

#[test]
fn with_automatic_merge_off_the_output_is_not_called_finished_while_it_lacks_an_item() {
    let fixture = Fixture::empty();
    fixture.write("left", "added.txt", b"from the left\n", T + 4);
    let mut view = fixture.view();
    let Some(ca_session::settings::SessionSettings::FolderMerge(mut merge)) = view.settings()
    else {
        panic!("no folder merge settings")
    };
    merge.merge.automatic_merge = false;
    view.apply_settings(&ca_session::settings::SessionSettings::FolderMerge(merge));
    assert!(poll_until(&mut view, |view| view.tree().is_some()));

    merge_at_once(&mut view);

    assert!(fixture.read("output", "added.txt").is_none());
    assert_eq!(
        message(&view),
        "Nothing is copied. Merge automatically is off in the session settings, so 1 item \
         has no action and the merge leaves it out. To write it, choose Take Left, \
         Take Center or Take Right and merge again."
    );
}

/// The rule for a row merged by hand reads the output item's time: a file
/// written after every input item, and not a copy of one, holds the merge
/// result. The batch writes `added.txt` first in one order and nothing in
/// the other, which decides between exit codes 14 and 101 before the fix.
fn rows_merged_by_hand_and_reloaded_count_as_merged(batch_first: bool) {
    let fixture = Fixture::mixed();
    if !batch_first {
        std::fs::remove_file(fixture.path("left").join("added.txt")).unwrap();
    }
    let mut view = fixture.view();
    merge_at_once(&mut view);
    if view.has_summary() {
        close_and_rescan(&mut view);
    }
    merge_mixed_by_hand(&fixture);

    reload(&mut view);

    assert_eq!(
        view.exit_code(),
        Some(0),
        "both rows hold their merge result"
    );
    assert_eq!(report_word(&view, "both.txt"), "Merged by hand");
    assert_eq!(report_word(&view, "clash.txt"), "Merged by hand");
    let mut probe = Probe::new(WINDOW[0], WINDOW[1]);
    let _ = frame_text(&mut probe, &mut view);
    let shown = frame_text(&mut probe, &mut view);
    assert_eq!(
        shown
            .lines()
            .filter(|line| *line == "Merged by hand")
            .count(),
        2,
        "the Action column does not mark both rows:\n{shown}"
    );
    merge_at_once(&mut view);
    assert_eq!(message(&view), FINISHED);
    assert_eq!(
        fixture.read("output", "both.txt").as_deref(),
        Some(&b"one\ntwo\nTHREE, merged\n"[..]),
        "the merge wrote over a row merged by hand"
    );
}

#[test]
fn rows_merged_by_hand_after_a_merge_that_wrote_other_items_count_as_merged() {
    rows_merged_by_hand_and_reloaded_count_as_merged(true);
}

#[test]
fn rows_merged_by_hand_when_no_merge_wrote_anything_count_as_merged() {
    rows_merged_by_hand_and_reloaded_count_as_merged(false);
}

#[test]
fn an_older_output_item_or_a_copy_of_an_input_is_not_a_merge_by_hand() {
    let fixture = Fixture::mixed();
    fixture.write("output", "both.txt", b"one\ntwo\nthree\n", T);
    fixture.write("output", "clash.txt", b"one\nRIGHT SIDE\nthree\n", T + 8);
    let view = fixture.view();
    assert_eq!(report_word(&view, "both.txt"), "Merge by hand");
    assert_eq!(report_word(&view, "clash.txt"), "Merge by hand");
    assert_eq!(view.exit_code(), Some(101));
}

#[test]
fn a_take_on_a_row_merged_by_hand_replaces_the_output_item_and_the_confirmation_says_so() {
    let fixture = Fixture::mixed();
    merge_mixed_by_hand(&fixture);
    let mut view = fixture.view();
    assert_eq!(report_word(&view, "both.txt"), "Merged by hand");
    assert!(view.select(Path::new("both.txt")));
    view.run(Command::TakeRight);
    let mut probe = Probe::new(WINDOW[0], WINDOW[1]);
    let _ = frame_text(&mut probe, &mut view);
    view.set_confirm_merge(true);
    view.run(Command::MergeFolders);
    assert!(poll_until(&mut view, FolderMergeView::is_confirming));
    let _ = frame_text(&mut probe, &mut view);
    let shown = frame_text(&mut probe, &mut view);
    for wanted in [
        "The merge replaces 1 item merged by hand in the output.",
        "both.txt (merged by hand, replaced)",
    ] {
        assert!(
            shown.contains(wanted),
            "the confirmation lacks {wanted:?}:\n{shown}"
        );
    }

    view.confirm_pending();
    while !view.has_summary() {
        assert!(poll_until(&mut view, |view| view.has_summary()
            || view.pending_question().is_some()));
        if let Some(question) = view.pending_question() {
            view.answer(&question, ca_view_folder::opjobs::Answer::Proceed);
        }
    }
    assert_eq!(
        fixture.read("output", "both.txt").as_deref(),
        Some(&b"one\ntwo\nTHREE\n"[..])
    );
    assert_eq!(
        view.last_report().map(ca_fs::ExecutionReport::completed),
        Some(1),
        "the merge of the selected row copies both.txt"
    );
}

#[test]
fn a_binary_conflict_is_not_sent_to_text_merge() {
    let fixture = Fixture::empty();
    fixture.write("center", "pic.bin", b"\x00base", T);
    fixture.write("left", "pic.bin", b"\x00left!", T + 4);
    fixture.write("right", "pic.bin", b"\x00right!!", T + 8);
    let mut view = fixture.view();
    assert_eq!(status_of(&view, "pic.bin"), MergeStatus::Conflict);
    assert!(view.select(Path::new("pic.bin")));
    assert!(!view.accepts(Command::OpenTextMerge));

    merge_at_once(&mut view);

    assert_eq!(
        message(&view),
        "Nothing is copied. 1 item needs a merge by hand (0 mergeable, 1 conflict). \
         The merge does not write it to the output. Text Merge cannot open it. \
         To put one input's copy in the output, choose Take Left, Take Center or \
         Take Right and merge again. Item: pic.bin (conflict, Take only, not in the output)."
    );
}

#[test]
fn a_deletion_against_an_edit_is_not_sent_to_text_merge() {
    let fixture = Fixture::empty();
    fixture.write("center", "gone.txt", b"one\ntwo\nthree\n", T);
    fixture.write("right", "gone.txt", b"one\ntwo\nTHREE\n", T + 8);
    let mut view = fixture.view();
    assert!(view.select(Path::new("gone.txt")));
    assert!(!view.accepts(Command::OpenTextMerge));

    merge_at_once(&mut view);

    let text = message(&view);
    assert!(!text.contains("in Text Merge and save it"), "{text}");
    assert!(text.contains("Text Merge cannot open it."), "{text}");
    assert!(
        text.ends_with("Item: gone.txt (conflict, Take only, not in the output)."),
        "{text}"
    );
}

fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
    const EMPTY_ZIP: &[u8] = &[
        0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    std::fs::write(path, EMPTY_ZIP).unwrap();
    let options = ca_vfs::ArchiveOptions {
        zone_offset_seconds: ca_fs::local_offset_seconds(),
        ..ca_vfs::ArchiveOptions::default()
    };
    let source = ca_fs::Source::archive(path, options).unwrap();
    let cancel = ca_vfs::Cancel::new();
    for (name, bytes) in entries {
        let mut reader = *bytes;
        source
            .file_system()
            .write_file(&ca_vfs::VfsPath::parse(name).unwrap(), &mut reader, &cancel)
            .unwrap();
    }
}

#[test]
fn a_row_of_an_archive_input_is_not_sent_to_text_merge() {
    let fixture = Fixture::empty();
    let right = fixture.dir.path().join("right.zip");
    write_zip(&right, &[("m.txt", b"one\ntwo\nTHREE\n")]);
    fixture.write("center", "m.txt", b"one\ntwo\nthree\n", T);
    fixture.write("left", "m.txt", b"ONE\ntwo\nthree\n", T + 4);
    let request = OpenRequest::new(SessionKind::FolderMerge, fixture.path("left"), right)
        .with_center(Some(fixture.path("center")))
        .with_output(Some(fixture.path("output")));
    let mut view = fixture.open(&request);
    assert!(view.select(Path::new("m.txt")));
    assert!(!view.accepts(Command::OpenTextMerge));

    merge_at_once(&mut view);

    let text = message(&view);
    assert!(!text.contains("in Text Merge and save it"), "{text}");
    assert!(text.contains("Text Merge cannot open it."), "{text}");
}

#[test]
fn the_notice_tells_rows_text_merge_opens_from_rows_it_cannot_open() {
    let fixture = Fixture::mixed();
    std::fs::remove_file(fixture.path("left").join("added.txt")).unwrap();
    fixture.write("center", "pic.bin", b"\x00base", T);
    fixture.write("left", "pic.bin", b"\x00left!", T + 4);
    fixture.write("right", "pic.bin", b"\x00right!!", T + 8);
    let mut view = fixture.view();

    merge_at_once(&mut view);

    assert_eq!(
        message(&view),
        "Nothing is copied. 3 items need a merge by hand (1 mergeable, 2 conflicts). \
         The merge does not write them to the output. \
         2 of them open in Text Merge. To put their merge result in the output, merge \
         each one there and save it. Text Merge cannot open the other one. \
         To keep one input's copy instead, choose Take Left, Take Center or Take Right \
         and merge again. Items: both.txt (mergeable, not in the output), \
         clash.txt (conflict, not in the output), \
         pic.bin (conflict, Take only, not in the output)."
    );
}

/// Change `added.txt` after the confirmation, so the step drifts, then answer
/// every question with `answer`.
fn merge_with_a_drifted_step(
    with_waiting_rows: bool,
    after_confirmation: impl Fn(&Fixture),
    answer: ca_view_folder::opjobs::Answer,
) -> (Fixture, FolderMergeView) {
    let fixture = if with_waiting_rows {
        Fixture::mixed()
    } else {
        let fixture = Fixture::empty();
        fixture.write("left", "added.txt", b"from the left\n", T + 4);
        fixture
    };
    let mut view = fixture.view();
    view.set_confirm_merge(true);
    view.run(Command::MergeFolders);
    assert!(poll_until(&mut view, FolderMergeView::is_confirming));
    after_confirmation(&fixture);
    view.confirm_pending();
    while !view.has_summary() {
        assert!(poll_until(&mut view, |view| view.has_summary()
            || view.pending_question().is_some()));
        if let Some(question) = view.pending_question() {
            view.answer(&question, answer);
        }
    }
    assert!(fixture.read("output", "added.txt").is_none());
    (fixture, view)
}

fn change_added(fixture: &Fixture) {
    fixture.write("left", "added.txt", b"from the left, longer now\n", T + 20);
}

fn remove_added(fixture: &Fixture) {
    std::fs::remove_file(fixture.path("left").join("added.txt")).unwrap();
}

#[test]
fn a_merge_with_a_skipped_step_and_no_waiting_row_does_not_exit_with_success() {
    let (_fixture, mut view) =
        merge_with_a_drifted_step(false, change_added, ca_view_folder::opjobs::Answer::Skip);
    assert_eq!(
        view.last_report().map(ca_fs::ExecutionReport::completed),
        Some(0)
    );
    assert_eq!(view.exit_code(), Some(100));
    close_and_rescan(&mut view);
    assert_eq!(
        view.exit_code(),
        Some(100),
        "the output still lacks added.txt"
    );
}

#[test]
fn a_merge_with_a_skipped_step_and_waiting_rows_reports_the_step_first() {
    let (_fixture, view) =
        merge_with_a_drifted_step(true, change_added, ca_view_folder::opjobs::Answer::Skip);
    assert_eq!(view.exit_code(), Some(100));
}

#[test]
fn a_merge_with_a_failed_step_does_not_exit_with_success() {
    let (fixture, mut view) =
        merge_with_a_drifted_step(false, remove_added, ca_view_folder::opjobs::Answer::Proceed);
    let report = view.last_report().unwrap();
    assert!(
        !report.failures().is_empty() || !report.is_clean(),
        "{report:?}"
    );
    assert_eq!(view.exit_code(), Some(100));

    fixture.write("left", "added.txt", b"from the left\n", T + 4);
    close_and_rescan(&mut view);
    merge_at_once(&mut view);
    assert!(poll_until(&mut view, FolderMergeView::has_summary));
    assert_eq!(
        view.exit_code(),
        Some(0),
        "a later merge wrote the item the failed step left out"
    );
}

#[test]
fn the_status_text_writes_item_paths_as_the_report_does() {
    let fixture = Fixture::empty();
    let rel = "dir with space/sous-dossier \u{fc}n\u{ef}c\u{f6}d\u{e9}/\u{65e5}\u{672c} file.txt";
    let body = b"one\ntwo\nthree\nfour\nfive\nsix\nseven\n";
    fixture.write("center", rel, body, T);
    fixture.write(
        "left",
        rel,
        b"ONE\ntwo\nthree\nfour\nfive\nsix\nseven\n",
        T + 4,
    );
    fixture.write(
        "right",
        rel,
        b"one\ntwo\nthree\nfour\nfive\nsix\nSEVEN\n",
        T + 8,
    );
    let mut view = fixture.view();
    assert_eq!(report_word(&view, rel), "Merge by hand");

    merge_at_once(&mut view);

    let text = message(&view);
    assert!(
        text.ends_with(&format!("Item: {rel} (mergeable, not in the output).")),
        "the status text and the report write the same path differently: {text}"
    );
}

#[test]
fn a_save_from_text_merge_marks_its_row_merged_by_hand_without_a_reload() {
    let fixture = Fixture::mixed();
    std::fs::remove_file(fixture.path("left").join("added.txt")).unwrap();
    let mut view = fixture.view();
    merge_at_once(&mut view);
    assert_eq!(view.exit_code(), Some(101));

    merge_mixed_by_hand(&fixture);
    view.file_saved(&saved_file(fixture.path("output").join("both.txt"), 0));

    let text = message(&view);
    assert!(
        text.starts_with(
            "Nothing is copied. 1 item needs a merge by hand (0 mergeable, 1 conflict)."
        ),
        "{text}"
    );
    assert!(
        text.ends_with("Item: clash.txt (conflict, not in the output)."),
        "{text}"
    );
    assert_eq!(report_word(&view, "both.txt"), "Merged by hand");
    assert_eq!(view.exit_code(), Some(101), "clash.txt still waits");

    view.file_saved(&saved_file(fixture.path("output").join("clash.txt"), 0));
    assert_eq!(view.message(), None);
    assert_eq!(view.exit_code(), Some(0));
}

#[test]
fn a_save_into_an_output_that_is_the_left_input_keeps_its_row_merged_by_hand_after_a_reload() {
    let fixture = Fixture::mixed();
    let mut view = fixture.view();
    let Some(ca_session::settings::SessionSettings::FolderMerge(mut merge)) = view.settings()
    else {
        panic!("no folder merge settings")
    };
    merge.merge.target = ca_session::settings::folder::MergeTarget::Left;
    view.apply_settings(&ca_session::settings::SessionSettings::FolderMerge(merge));
    assert!(poll_until(&mut view, |view| view.tree().is_some()));
    assert_eq!(report_word(&view, "both.txt"), "Merge by hand");

    fixture.write("left", "both.txt", b"one\ntwo\nTHREE, merged\n", T + 30);
    view.file_saved(&saved_file(fixture.path("left").join("both.txt"), 0));
    reload(&mut view);

    assert_eq!(
        report_word(&view, "both.txt"),
        "Merged by hand",
        "the output is an input, so only the save tells a merge by hand from an edit"
    );
    assert_eq!(report_word(&view, "clash.txt"), "Merge by hand");
}

#[test]
fn a_take_without_a_new_merge_updates_the_notice_and_the_exit_code() {
    let fixture = Fixture::mixed();
    let mut view = fixture.view();
    merge_at_once(&mut view);
    assert!(poll_until(&mut view, FolderMergeView::has_summary));
    close_and_rescan(&mut view);
    assert!(message(&view).starts_with("2 items need a merge by hand"));
    assert_eq!(view.exit_code(), Some(14));

    assert!(view.select(Path::new("clash.txt")));
    view.run(Command::TakeLeft);
    let text = message(&view);
    assert!(
        text.starts_with("1 item needs a merge by hand (1 mergeable, 0 conflicts)."),
        "{text}"
    );
    assert!(
        text.ends_with("Item: both.txt (mergeable, not in the output)."),
        "{text}"
    );
    assert_eq!(view.exit_code(), Some(14));

    view.run(Command::SelectAll);
    view.run(Command::TakeLeft);
    assert_eq!(
        view.message(),
        None,
        "the notice names rows a Take resolved"
    );
    assert_eq!(view.exit_code(), Some(0));
}

/// Presses on the drawn view, found by accessible label.
mod probe {
    use super::{draw_view, FolderMergeView, Probe};

    fn run(view: &mut FolderMergeView) -> impl FnMut(&egui::Context) + '_ {
        move |ctx| {
            let _ = draw_view(view, ctx);
        }
    }

    /// Press the toolbar button labelled Merge: the one nearest the top.
    pub fn press_toolbar_merge(probe: &mut Probe, view: &mut FolderMergeView) {
        probe.idle(&mut run(view));
        let button = probe
            .find_all("Merge")
            .into_iter()
            .min_by(|a, b| a.rect.top().total_cmp(&b.rect.top()))
            .unwrap_or_else(|| panic!("no Merge button: {:?}", probe.labels()));
        assert!(button.enabled, "the Merge button is disabled");
        probe.press_at(button.rect.center(), &mut run(view));
    }

    /// Press the dialog button labelled `label`: the one nearest the bottom,
    /// below the toolbar button of the same name.
    pub fn press_dialog(probe: &mut Probe, view: &mut FolderMergeView, label: &str) {
        probe.idle(&mut run(view));
        let button = probe
            .find_all(label)
            .into_iter()
            .max_by(|a, b| a.rect.top().total_cmp(&b.rect.top()))
            .unwrap_or_else(|| panic!("no {label} button: {:?}", probe.labels()));
        probe.press_at(button.rect.center(), &mut run(view));
    }
}
