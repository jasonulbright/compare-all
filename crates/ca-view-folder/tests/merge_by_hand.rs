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

    fn view(&self) -> FolderMergeView {
        let request = OpenRequest::new(
            SessionKind::FolderMerge,
            self.path("left"),
            self.path("right"),
        )
        .with_center(Some(self.path("center")))
        .with_output(Some(self.path("output")));
        let mut view =
            FolderMergeView::with_journal_directory(&request, &context(), 1, self.path("journals"));
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
