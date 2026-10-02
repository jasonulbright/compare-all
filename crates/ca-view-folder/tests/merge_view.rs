//! What the folder merge view has to keep: the comparison classifies against
//! the ancestor, the filters and the take commands act on the rows shown, a
//! cancelled confirmation writes nothing, a confirmed plan writes only the
//! output folder, and a text conflict opens in the text merge.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ca_fs::{Change, MergeStatus, Pane, Resolution};
use ca_session::SessionKind;
use ca_ui::command::Command;
use ca_ui::testing::{context, sized_input};
use ca_ui::view::{OpenRequest, SessionView, ViewAction};
use ca_view_folder::merge_view::MergeFilter;
use ca_view_folder::FolderMergeView;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long a test waits for background work before giving up.
const PATIENCE: Duration = Duration::from_secs(30);

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
    /// Left, center and right, with one item of each kind of change.
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        for side in ["left", "center", "right", "output", "journals"] {
            std::fs::create_dir_all(dir.path().join(side)).unwrap();
        }
        let fixture = Self { dir };
        for side in ["left", "center", "right"] {
            fixture.write(side, "same.txt", b"same");
        }
        fixture.write("left", "added.txt", b"from the left");
        fixture.write("center", "edited.txt", b"old");
        fixture.write("left", "edited.txt", b"old");
        fixture.write("right", "edited.txt", b"edited on the right");
        fixture.write("center", "clash.txt", b"one\ntwo\nthree\n");
        fixture.write("left", "clash.txt", b"one\nLEFT\nthree\n");
        fixture.write("right", "clash.txt", b"one\nRIGHT SIDE\nthree\n");
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
        let mut view = FolderMergeView::with_journal_directory(
            &self.request(),
            &context(),
            1,
            self.path("journals"),
        );
        // A test never reaches the real recycle bin: on some platforms that
        // call asks the desktop shell for permission and blocks on a dialog.
        view.operation_options_mut().use_recycle_bin = false;
        assert!(poll_until(&mut view, |view| view.tree().is_some()));
        view
    }

    fn read(&self, side: &str, rel: &str) -> Option<Vec<u8>> {
        std::fs::read(self.path(side).join(rel)).ok()
    }
}

fn status_of(view: &FolderMergeView, rel: &str) -> MergeStatus {
    view.tree()
        .and_then(|tree| tree.rows.iter().find(|row| row.rel == Path::new(rel)))
        .unwrap_or_else(|| panic!("no row for {rel}"))
        .status
}

/// Start a merge and wait until it is either on screen or finished.
fn merge(view: &mut FolderMergeView) {
    assert!(view.accepts(Command::MergeFolders));
    view.run(Command::MergeFolders);
    assert!(
        poll_until(view, |view| view.is_confirming() || view.has_summary()),
        "the merge produced neither a plan nor a result: {:?}",
        view.message()
    );
}

#[test]
fn the_view_classifies_every_row_against_the_ancestor() {
    let fixture = Fixture::new();
    let view = fixture.view();
    assert_eq!(status_of(&view, "same.txt"), MergeStatus::Unchanged);
    assert_eq!(
        status_of(&view, "added.txt"),
        MergeStatus::LeftChange(Change::Added)
    );
    assert_eq!(
        status_of(&view, "edited.txt"),
        MergeStatus::RightChange(Change::Modified)
    );
    assert_eq!(status_of(&view, "clash.txt"), MergeStatus::Conflict);
    assert_eq!(view.title(), "left - right (merge)");
}

#[test]
fn a_display_filter_narrows_the_rows_and_next_conflict_finds_the_conflict() {
    let fixture = Fixture::new();
    let mut view = fixture.view();
    assert_eq!(view.visible_rows().len(), 4);
    view.run(Command::ShowConflicts);
    assert_eq!(view.filter(), MergeFilter::Conflicts);
    let shown: Vec<PathBuf> = view
        .visible_rows()
        .iter()
        .map(|row| row.rel.clone())
        .collect();
    assert_eq!(shown, vec![PathBuf::from("clash.txt")]);
    view.run(Command::ShowLeftChanges);
    assert_eq!(view.visible_rows().len(), 1);
    view.run(Command::ShowAll);

    assert!(view.accepts(Command::NextConflict));
    view.run(Command::NextConflict);
    assert_eq!(
        view.cursor_row().map(|row| row.rel.clone()),
        Some(PathBuf::from("clash.txt"))
    );
}

#[test]
fn a_cancelled_confirmation_writes_nothing_and_a_confirmed_one_writes_the_output() {
    let fixture = Fixture::new();
    let mut view = fixture.view();
    view.set_confirm_merge(true);

    merge(&mut view);
    assert!(view.is_confirming());
    view.cancel_pending();
    std::thread::sleep(Duration::from_millis(50));
    view.tick();
    assert!(
        std::fs::read_dir(fixture.path("output"))
            .unwrap()
            .next()
            .is_none(),
        "a cancelled merge wrote into the output"
    );

    merge(&mut view);
    let shown = view.pending_plan().expect("a plan is on screen").clone();
    view.confirm_pending();
    assert!(poll_until(&mut view, FolderMergeView::has_summary));
    assert_eq!(view.pending_plan(), Some(&shown));
    assert!(view
        .last_report()
        .is_some_and(ca_fs::ExecutionReport::is_clean));
    assert_eq!(
        fixture.read("output", "same.txt").as_deref(),
        Some(&b"same"[..])
    );
    assert_eq!(
        fixture.read("output", "added.txt").as_deref(),
        Some(&b"from the left"[..])
    );
    assert_eq!(
        fixture.read("output", "edited.txt").as_deref(),
        Some(&b"edited on the right"[..])
    );
    assert!(
        fixture.read("output", "clash.txt").is_none(),
        "a conflict was written with nobody deciding"
    );
    assert_eq!(
        fixture.read("left", "clash.txt").as_deref(),
        Some(&b"one\nLEFT\nthree\n"[..])
    );
}

#[test]
fn changing_archive_masks_drops_a_merge_plan_built_with_the_old_masks() {
    let fixture = Fixture::new();
    let mut view = fixture.view();
    view.set_confirm_merge(true);

    merge(&mut view);
    assert!(view.is_confirming());

    let mut masks = ca_session::options::ArchiveOptions::default();
    masks.masks.insert("zip".to_owned(), "*.special".to_owned());
    view.follow_archive_masks(&masks);

    assert!(
        view.pending_plan().is_none(),
        "a plan based on the old archive masks stayed confirmable"
    );
    view.confirm_pending();
    assert!(!view.is_confirming());
    assert!(poll_until(&mut view, |view| view.tree().is_some()));
    assert!(
        std::fs::read_dir(fixture.path("output"))
            .unwrap()
            .next()
            .is_none(),
        "the invalidated merge plan wrote into the output"
    );
}

#[test]
fn with_the_confirmation_off_a_merge_runs_at_once() {
    let fixture = Fixture::new();
    let mut view = fixture.view();
    view.set_confirm_merge(false);
    merge(&mut view);
    assert!(!view.is_confirming());
    assert!(poll_until(&mut view, FolderMergeView::has_summary));
    assert!(fixture.read("output", "added.txt").is_some());
}

#[test]
fn a_take_resolves_the_conflict_under_the_cursor() {
    let fixture = Fixture::new();
    let mut view = fixture.view();
    view.set_confirm_merge(false);
    assert!(view.select(Path::new("clash.txt")));
    view.run(Command::TakeRight);
    assert_eq!(
        view.overrides().get(Path::new("clash.txt")),
        Some(&Resolution::Take(Pane::Right))
    );
    merge(&mut view);
    assert!(poll_until(&mut view, FolderMergeView::has_summary));
    assert_eq!(
        fixture.read("output", "clash.txt").as_deref(),
        Some(&b"one\nRIGHT SIDE\nthree\n"[..])
    );
}

#[test]
fn copy_to_output_takes_the_selection_from_the_active_input() {
    let fixture = Fixture::new();
    let mut view = fixture.view();
    view.set_confirm_merge(false);
    assert!(view.select(Path::new("clash.txt")));
    view.set_active(Pane::Center);
    view.run(Command::CopyToOutput);
    assert!(poll_until(&mut view, FolderMergeView::has_summary));
    assert_eq!(
        fixture.read("output", "clash.txt").as_deref(),
        Some(&b"one\ntwo\nthree\n"[..])
    );
    assert!(
        fixture.read("output", "same.txt").is_none(),
        "an item outside the selection was copied"
    );
}

#[test]
fn a_text_conflict_opens_in_the_text_merge_with_the_output_file() {
    let fixture = Fixture::new();
    let mut view = fixture.view();
    assert!(view.select(Path::new("clash.txt")));
    assert!(view.accepts(Command::OpenTextMerge));
    view.run(Command::OpenTextMerge);
    let actions = view.take_actions();
    let [ViewAction::Open(request)] = actions.as_slice() else {
        panic!("expected one open request, received {actions:?}");
    };
    assert_eq!(request.kind, SessionKind::TextMerge);
    assert_eq!(request.left, fixture.path("left").join("clash.txt"));
    assert_eq!(request.right, fixture.path("right").join("clash.txt"));
    assert_eq!(
        request.center.as_deref(),
        Some(fixture.path("center").join("clash.txt").as_path())
    );
    assert_eq!(
        request.output.as_deref(),
        Some(fixture.path("output").join("clash.txt").as_path())
    );
    assert!(view.select(Path::new("same.txt")));
    assert!(!view.accepts(Command::OpenTextMerge));
}

#[test]
fn text_merge_allows_an_existing_output_file_and_an_input_as_the_output_folder() {
    let fixture = Fixture::new();
    fixture.write("output", "clash.txt", b"previous output\n");
    let mut view = fixture.view();
    assert!(view.select(Path::new("clash.txt")));
    assert!(view.accepts(Command::OpenTextMerge));
    view.run(Command::OpenTextMerge);
    let actions = view.take_actions();
    let [ViewAction::Open(request)] = actions.as_slice() else {
        panic!("expected one open request, received {actions:?}");
    };
    assert_eq!(
        request.output.as_deref(),
        Some(fixture.path("output").join("clash.txt").as_path())
    );

    let fixture = Fixture::new();
    let request = fixture.request().with_output(Some(fixture.path("left")));
    let mut view =
        FolderMergeView::with_journal_directory(&request, &context(), 1, fixture.path("journals"));
    assert!(poll_until(&mut view, |view| view.tree().is_some()));
    assert!(view.select(Path::new("clash.txt")));
    assert!(view.accepts(Command::OpenTextMerge));
    view.run(Command::OpenTextMerge);
    let actions = view.take_actions();
    let [ViewAction::Open(request)] = actions.as_slice() else {
        panic!("expected one open request, received {actions:?}");
    };
    assert_eq!(
        request.output.as_deref(),
        Some(fixture.path("left").join("clash.txt").as_path())
    );
}

#[cfg(windows)]
#[test]
fn text_merge_refuses_an_output_path_under_a_junction() {
    let fixture = Fixture::new();
    for (side, text) in [
        ("left", "one\nLEFT\nthree\n"),
        ("center", "one\ntwo\nthree\n"),
        ("right", "one\nRIGHT\nthree\n"),
    ] {
        fixture.write(side, "sub/clash.txt", text.as_bytes());
    }
    let elsewhere = fixture.dir.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("clash.txt"), b"outside\n").unwrap();
    let junction = fixture.path("output").join("sub");
    // `TempDir` paths on Windows have no spaces. Passing the command without
    // nested quotes avoids cmd.exe's /C quote-stripping rules.
    let command = format!("mklink /J {} {}", junction.display(), elsewhere.display());
    let result = std::process::Command::new("cmd.exe")
        .args(["/D", "/C", &command])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "command {command:?} failed: {}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );

    let mut view = fixture.view();
    assert!(view.select(&Path::new("sub").join("clash.txt")));

    assert!(view.text_merge_request().is_none());
    assert!(!view.accepts(Command::OpenTextMerge));
}

#[test]
fn the_session_settings_carry_the_four_folders_and_the_target() {
    let fixture = Fixture::new();
    let mut view = fixture.view();
    let Some(ca_session::settings::SessionSettings::FolderMerge(mut settings)) = view.settings()
    else {
        panic!("a folder merge states folder merge settings");
    };
    assert_eq!(
        settings.specs.ancestor,
        Some(ca_session::SideLocation::local(fixture.path("center")))
    );
    assert_eq!(
        settings.specs.output,
        Some(ca_session::SideLocation::local(fixture.path("output")))
    );
    settings.merge.target = ca_session::settings::folder::MergeTarget::Left;
    view.apply_settings(&ca_session::settings::SessionSettings::FolderMerge(
        settings,
    ));
    assert_eq!(view.output_folder(), Some(fixture.path("left").as_path()));
    assert!(poll_until(&mut view, |view| view.tree().is_some()));
}

#[test]
fn a_merge_with_no_output_folder_is_not_offered() {
    let fixture = Fixture::new();
    let request = OpenRequest::new(
        SessionKind::FolderMerge,
        fixture.path("left"),
        fixture.path("right"),
    );
    let mut view =
        FolderMergeView::with_journal_directory(&request, &context(), 2, fixture.path("journals"));
    assert!(poll_until(&mut view, |view| view.tree().is_some()));
    assert!(view.output_folder().is_none());
    assert!(!view.accepts(Command::MergeFolders));
    assert!(
        !view.accepts(Command::TakeCenter),
        "no ancestor to take from"
    );
}

/// The view paints headless frames while its comparison runs and after it
/// lands, with no panel of its own left unpainted.
#[test]
fn the_view_paints_frames_headless() {
    let fixture = Fixture::new();
    let mut view = fixture.view();
    let ctx = egui::Context::default();
    for _ in 0..3 {
        let _ = ctx.run(sized_input(1_000.0, 600.0), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = view.ui(ui, &context());
            });
        });
    }
    view.run(Command::ToggleCenterPane);
    assert!(!view.shows_center());
    let _ = ctx.run(sized_input(400.0, 300.0), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let _ = view.ui(ui, &context());
        });
    });
}

#[test]
fn the_toolbar_declares_the_items_the_options_page_lists() {
    let names: Vec<&str> = ca_ui::toolbar::defaults(ca_ui::toolbar::ToolbarView::FolderMerge)
        .iter()
        .map(|item| item.name)
        .collect();
    for wanted in ["filter", "take-left", "merge", "report"] {
        assert!(names.contains(&wanted), "{wanted} is missing");
    }
}

/// A zip at `path` holding each named entry, stamped in the zone of this
/// machine as the view reads it.
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
fn an_archive_input_is_compared_and_merged_into_the_output_folder() {
    let fixture = Fixture::new();
    let right = fixture.dir.path().join("right.zip");
    write_zip(
        &right,
        &[
            ("same.txt", b"same"),
            ("edited.txt", b"edited on the right"),
            ("clash.txt", b"one\nRIGHT SIDE\nthree\n"),
        ],
    );
    let request = OpenRequest::new(SessionKind::FolderMerge, fixture.path("left"), right)
        .with_center(Some(fixture.path("center")))
        .with_output(Some(fixture.path("output")));
    let mut view =
        FolderMergeView::with_journal_directory(&request, &context(), 1, fixture.path("journals"));
    view.operation_options_mut().use_recycle_bin = false;
    assert!(poll_until(&mut view, |view| view.tree().is_some()));
    assert_eq!(
        status_of(&view, "edited.txt"),
        MergeStatus::RightChange(Change::Modified)
    );
    assert_eq!(status_of(&view, "clash.txt"), MergeStatus::Conflict);
    view.set_confirm_merge(false);
    merge(&mut view);
    assert!(poll_until(&mut view, FolderMergeView::has_summary));
    assert!(view
        .last_report()
        .is_some_and(ca_fs::ExecutionReport::is_clean));
    assert_eq!(
        fixture.read("output", "edited.txt").as_deref(),
        Some(&b"edited on the right"[..])
    );
}

#[test]
fn text_merge_is_disabled_for_archive_input_rows() {
    let fixture = Fixture::new();
    let right = fixture.dir.path().join("right.zip");
    write_zip(&right, &[("clash.txt", b"one\nRIGHT\nthree\n")]);
    fixture.write("center", "clash.txt", b"one\ntwo\nthree\n");
    fixture.write("left", "clash.txt", b"one\nLEFT\nthree\n");
    let request = OpenRequest::new(SessionKind::FolderMerge, fixture.path("left"), right)
        .with_center(Some(fixture.path("center")))
        .with_output(Some(fixture.path("output")));
    let mut view =
        FolderMergeView::with_journal_directory(&request, &context(), 1, fixture.path("journals"));
    assert!(poll_until(&mut view, |view| view.tree().is_some()));
    assert!(view.select(Path::new("clash.txt")));
    assert!(!view.accepts(Command::OpenTextMerge));
    assert!(view.text_merge_request().is_none());
}

#[test]
fn an_archive_output_is_refused_before_a_plan_is_built() {
    let fixture = Fixture::new();
    let output = fixture.dir.path().join("output.zip");
    write_zip(&output, &[("same.txt", b"same")]);
    let request = fixture.request().with_output(Some(output.clone()));
    let before = std::fs::read(&output).unwrap();
    let mut view =
        FolderMergeView::with_journal_directory(&request, &context(), 1, fixture.path("journals"));
    view.operation_options_mut().use_recycle_bin = false;
    assert!(poll_until(&mut view, |view| view.tree().is_some()));
    view.set_confirm_merge(false);
    view.run(Command::MergeFolders);
    assert!(poll_until(&mut view, |view| view.message().is_some()));
    assert!(view.pending_plan().is_none());
    assert!(!view.has_summary());
    assert_eq!(std::fs::read(&output).unwrap(), before);
}

/// A merge whose batch writes files refuses to close without a question and
/// says it is busy until the batch ends.
#[test]
fn a_running_merge_keeps_the_tab_open_and_says_it_is_busy() {
    let fixture = Fixture::new();
    let mut view = fixture.view();
    assert!(!view.is_busy());
    merge(&mut view);
    assert!(view.is_confirming());
    view.confirm_pending();
    assert!(view.is_busy(), "a running merge is not reported as busy");
    assert!(!view.may_close());
    assert!(poll_until(&mut view, FolderMergeView::has_summary));
    assert!(!view.is_busy());
}
