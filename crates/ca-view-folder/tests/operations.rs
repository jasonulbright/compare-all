//! What the file operations have to keep: the plan shown is the plan that
//! runs, a setting changed in the confirmation dialog reaches the plan, a
//! cancelled confirmation touches nothing, a declined conflict leaves its item
//! alone, a disk that moved on is reported rather than acted on, and an
//! unfinished batch is offered back to the user.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ca_fs::{Conflict, Node, NodeStatus, Side, StepAction};
use ca_ui::testing::{context, sized_input};
use ca_ui::view::SessionView;
use ca_view_folder::operations::{conflicts_by_kind, Operation};
use ca_view_folder::opjobs::Answer;
use ca_view_folder::selection::{Scope, SelectRule};
use ca_view_folder::FolderView;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long a test waits for background work before giving up.
const PATIENCE: Duration = Duration::from_secs(30);

fn poll_until(view: &mut FolderView, ready: impl Fn(&FolderView) -> bool) -> bool {
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
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        for side in ["left", "right", "journals"] {
            std::fs::create_dir_all(dir.path().join(side)).unwrap();
        }
        Self { dir }
    }

    fn left(&self) -> PathBuf {
        self.dir.path().join("left")
    }

    fn right(&self) -> PathBuf {
        self.dir.path().join("right")
    }

    fn journals(&self) -> PathBuf {
        self.dir.path().join("journals")
    }

    fn write(&self, side: &str, name: &str, body: &[u8]) {
        let path = self.dir.path().join(side).join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, body).unwrap();
    }

    fn view(&self) -> FolderView {
        let mut view = FolderView::with_journal_directory(
            self.left(),
            self.right(),
            &context(),
            1,
            self.journals(),
        );
        assert!(poll_until(&mut view, SessionView::is_ready));
        // A test never reaches the real recycle bin: on some platforms that
        // call asks the desktop shell for permission and blocks on a dialog.
        view.form_mut().options.use_recycle_bin = false;
        view
    }
}

/// Select everything on one side and start one operation, waiting for its plan.
fn plan_for(view: &mut FolderView, operation: Operation, scope: Scope) {
    view.select(SelectRule::All, scope);
    view.start_operation(operation);
    assert!(
        poll_until(view, FolderView::is_confirming),
        "no plan reached the confirmation"
    );
}

/// Run the confirmed batch, answering every question the same way.
fn run_answering(view: &mut FolderView, answer: Answer) {
    let finished = poll_until(view, |view| {
        if let Some(question) = view.pending_question() {
            view.answer(&question, answer);
        }
        view.has_summary()
    });
    assert!(finished, "the batch never reported a result");
}

#[test]
fn the_plan_shown_is_the_plan_that_runs() {
    let fixture = Fixture::new();
    fixture.write("left", "one.txt", b"alpha");
    fixture.write("left", "two.txt", b"beta");
    let mut view = fixture.view();

    plan_for(&mut view, Operation::CopyToOtherSide, Scope::Left);
    let shown = view.pending_plan().expect("a plan is on screen").clone();
    view.confirm_pending();
    assert!(poll_until(&mut view, FolderView::has_summary));
    let ran = view.pending_plan().expect("the plan is still held").clone();
    assert_eq!(shown, ran);
    assert_eq!(
        view.last_report().map(ca_fs::ExecutionReport::completed),
        Some(shown.steps.len())
    );
    assert_eq!(
        std::fs::read(fixture.right().join("one.txt")).unwrap(),
        b"alpha"
    );
}

/// A change that alters the steps puts the new steps on screen, and nothing
/// runs until they are confirmed.
#[test]
fn a_recycle_bin_choice_made_in_the_dialog_is_shown_before_it_runs() {
    let fixture = Fixture::new();
    fixture.write("left", "one.txt", b"alpha");
    let mut view = fixture.view();

    plan_for(&mut view, Operation::Delete, Scope::Left);
    let shown = view.pending_plan().expect("a plan is on screen").clone();
    assert!(!shown.steps.is_empty());
    assert!(shown
        .steps
        .iter()
        .all(|step| matches!(step.action, StepAction::DeleteFile { .. })));
    view.form_mut().options.use_recycle_bin = true;
    view.confirm_pending();
    assert!(
        poll_until(&mut view, |view| view.is_confirming() || view.has_summary()),
        "the confirmation led nowhere"
    );
    assert!(
        view.is_confirming(),
        "the batch ran the steps shown before the dialog changed"
    );
    let revised = view.pending_plan().expect("the new plan is on screen");
    assert!(revised.options.use_recycle_bin);
    assert!(!revised.steps.is_empty());
    assert!(revised
        .steps
        .iter()
        .all(|step| matches!(step.action, StepAction::Trash { .. })));
    // A test never reaches the real recycle bin, so the new plan is not run.
    view.cancel_pending();
    assert!(fixture.left().join("one.txt").exists());
}

/// A change that leaves the steps as shown runs at once, under the settings
/// the dialog now holds.
#[test]
fn a_backup_chosen_in_the_dialog_is_taken_when_the_batch_replaces_a_file() {
    let fixture = Fixture::new();
    // The source is written last so it is the newer of the two, and the
    // replacement raises no question of its own.
    fixture.write("right", "one.txt", b"old");
    std::thread::sleep(Duration::from_millis(1100));
    fixture.write("left", "one.txt", b"new");
    let mut view = fixture.view();

    plan_for(&mut view, Operation::CopyToOtherSide, Scope::Left);
    assert_eq!(view.pending_plan().unwrap().options.backup, None);
    let names = view.form_mut().backup_names.clone();
    view.form_mut().options.backup = Some(names);
    view.confirm_pending();
    run_answering(&mut view, Answer::Skip);

    let ran = view.pending_plan().expect("the plan is still held");
    assert!(
        ran.options.backup.is_some(),
        "the batch ran without the backup the dialog chose"
    );
    assert_eq!(
        std::fs::read(fixture.right().join("one.txt")).unwrap(),
        b"new"
    );
    let backup = fixture.right().join("one.txt.bak");
    assert!(backup.exists(), "the replaced file was not kept");
    assert_eq!(std::fs::read(backup).unwrap(), b"old");
}

/// Clearing the replace option in the dialog takes back the approval of an
/// occupied target, so the batch asks before it replaces the file.
#[test]
fn clearing_replace_in_the_dialog_puts_an_occupied_target_to_the_user() {
    let fixture = Fixture::new();
    fixture.write("right", "one.txt", b"old");
    std::thread::sleep(Duration::from_millis(1100));
    fixture.write("left", "one.txt", b"new");
    let mut view = fixture.view();

    plan_for(&mut view, Operation::CopyToOtherSide, Scope::Left);
    assert!(view.pending_plan().unwrap().options.overwrite);
    view.form_mut().options.overwrite = false;
    view.confirm_pending();

    let asked = std::cell::Cell::new(0usize);
    let answered = std::cell::Cell::new(None);
    let finished = poll_until(&mut view, |view| {
        if let Some(question) = view.pending_question() {
            if answered.get() != Some(question.sequence) {
                asked.set(asked.get() + 1);
                view.answer(&question, Answer::Skip);
                answered.set(Some(question.sequence));
            }
        }
        view.has_summary()
    });
    assert!(finished, "the batch never reported a result");
    assert_eq!(
        asked.get(),
        1,
        "the occupied target was not put to the user"
    );
    assert_eq!(
        std::fs::read(fixture.right().join("one.txt")).unwrap(),
        b"old",
        "the declined replacement was carried out anyway"
    );
}

#[test]
fn cancelling_the_confirmation_changes_nothing_on_disk() {
    let fixture = Fixture::new();
    fixture.write("left", "one.txt", b"alpha");
    let mut view = fixture.view();

    plan_for(&mut view, Operation::CopyToOtherSide, Scope::Left);
    view.cancel_pending();
    view.tick();
    assert!(!view.is_confirming());
    assert!(!fixture.right().join("one.txt").exists());
    // The journal directory holds nothing either: no batch was ever opened.
    let journals = std::fs::read_dir(fixture.journals()).unwrap().count();
    assert_eq!(journals, 0);
}

#[test]
fn a_conflict_answered_with_skip_leaves_its_item_alone() {
    let fixture = Fixture::new();
    // The target is written last so it is the newer of the two. Replacing a
    // newer item is a conflict the batch confirmation does not answer, so it
    // is the one that reaches the per-item prompt.
    fixture.write("left", "one.txt", b"new");
    std::thread::sleep(Duration::from_millis(1100));
    fixture.write("right", "one.txt", b"old");
    let mut view = fixture.view();

    plan_for(&mut view, Operation::CopyToOtherSide, Scope::Left);
    let plan = view.pending_plan().unwrap().clone();
    assert!(plan.conflicts().contains(&Conflict::OverwriteNewer));
    view.confirm_pending();
    run_answering(&mut view, Answer::Skip);
    assert_eq!(
        std::fs::read(fixture.right().join("one.txt")).unwrap(),
        b"old",
        "the declined copy was carried out anyway"
    );
}

#[test]
fn a_disk_that_moved_on_is_reported_rather_than_acted_on() {
    let fixture = Fixture::new();
    fixture.write("left", "one.txt", b"alpha");
    let mut view = fixture.view();

    plan_for(&mut view, Operation::CopyToOtherSide, Scope::Left);
    // The plan recorded the source's size; changing it after the confirmation
    // is exactly the drift execution has to notice.
    fixture.write("left", "one.txt", b"a much longer body than before");
    view.confirm_pending();

    run_answering(&mut view, Answer::Skip);
    let report = view.last_report().expect("a result is on screen");
    let drifted = report.results.iter().any(|result| {
        matches!(&result.outcome, ca_fs::StepOutcome::Skipped { reason }
            if reason.contains("when the plan was built"))
    });
    assert!(drifted, "the drift was not surfaced");
    assert!(
        !fixture.right().join("one.txt").exists(),
        "a step nobody approved wrote its target"
    );
}

/// A folder whose listing was not read in full is never removed wholesale, and
/// the refusal reaches the dialog rather than being silently dropped.
#[test]
fn an_incomplete_listing_reaches_the_confirmation() {
    let fixture = Fixture::new();
    let mut tree = folder_tree();
    tree.children[0].incomplete = true;
    let mut view = FolderView::from_tree(fixture.left(), fixture.right(), &context(), 2, tree);
    view.set_journal_directory(fixture.journals());

    // Only the folder is selected: a selected child would cancel the folder's
    // own selection and the refusal would never be reached.
    view.run(ca_ui::command::Command::CollapseAll);
    view.select(SelectRule::All, Scope::Left);
    view.start_operation(Operation::Delete);
    assert!(poll_until(&mut view, FolderView::is_confirming));
    let plan = view.pending_plan().unwrap();
    let grouped = conflicts_by_kind(plan);
    assert!(
        grouped
            .iter()
            .any(|(conflict, _)| *conflict == Conflict::CounterpartUnreadable),
        "the refusal never reached the dialog"
    );
    assert!(
        plan.steps
            .iter()
            .all(|step| !matches!(step.action, StepAction::Trash { .. })),
        "a partial listing was carried away"
    );
}

#[test]
fn an_unfinished_batch_is_reported_when_the_view_opens() {
    let fixture = Fixture::new();
    let planted = fixture.journals().join("batch-1-1-0.jsonl");
    std::fs::write(
        &planted,
        concat!(
            r#"{"record":"batch_start","kind":"copy","steps":2,"unix_seconds":1}"#,
            "\n",
            r#"{"record":"step_begin","index":0,"action":{"CopyFile":{"source":"a","target":"b"}},"temporary":null,"backup":null}"#,
            "\n"
        ),
    )
    .unwrap();

    let mut view = FolderView::with_journal_directory(
        fixture.left(),
        fixture.right(),
        &context(),
        3,
        fixture.journals(),
    );
    assert!(poll_until(&mut view, |view| {
        !view.recovery_notices().is_empty()
    }));
    let notices = view.recovery_notices();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].journal, planted);
    // Nothing was removed: recovery only reports until a choice is made.
    assert!(planted.exists());
}

#[test]
fn every_dialog_fits_a_narrow_window_and_a_wide_one() {
    for width in [640.0_f32, 1_280.0] {
        let fixture = Fixture::new();
        fixture.write("left", "one.txt", b"alpha");
        fixture.write("right", "one.txt", b"other");
        let mut view = fixture.view();
        plan_for(&mut view, Operation::CopyToOtherSide, Scope::Left);

        let ctx = egui::Context::default();
        for _ in 0..3 {
            let _ = ctx.run(sized_input(width, 800.0), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    view.ui(ui, &context());
                });
            });
        }
        let screen = ctx.screen_rect();
        let id = egui::Id::new(("folder-compare", 1u64)).with("confirm");
        let area = ctx
            .memory(|memory| memory.area_rect(id))
            .expect("the confirmation is on screen");
        assert!(
            screen.contains_rect(area),
            "the dialog ran outside a {width} point window: {area:?} against {screen:?}"
        );
    }
}

/// A tree of two folders and a file, with paths that need not exist because
/// planning never reads the disk.
fn folder_tree() -> Box<Node> {
    Box::new(node(
        "",
        Path::new(""),
        true,
        NodeStatus::NotCompared,
        vec![node(
            "deep",
            Path::new("deep"),
            true,
            NodeStatus::Same,
            vec![node(
                "inner.txt",
                &PathBuf::from("deep").join("inner.txt"),
                false,
                NodeStatus::Same,
                Vec::new(),
            )],
        )],
    ))
}

fn node(name: &str, rel: &Path, is_dir: bool, status: NodeStatus, children: Vec<Node>) -> Node {
    Node {
        name: name.to_string(),
        rel: rel.to_path_buf(),
        is_dir,
        left: Some(entry(rel, is_dir)),
        right: Some(entry(rel, is_dir)),
        facts: ca_fs::PairFacts::default(),
        status,
        flags: ca_fs::StatusFlags::default(),
        quick: None,
        content: None,
        error: None,
        incomplete: false,
        scan_cancelled: false,
        children,
    }
}

fn entry(rel: &Path, is_dir: bool) -> ca_fs::Entry {
    ca_fs::Entry {
        rel: rel.to_path_buf(),
        name: rel
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        is_dir,
        size: if is_dir { 0 } else { 8 },
        modified: None,
        created: None,
        attributes: ca_fs::Attributes::default(),
        link: None,
        error: None,
        listing_incomplete: false,
        refused: false,
    }
}

/// Planning a copy of two hundred thousand items runs on a worker, so the
/// frames painted while it runs cost what they always cost.
#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "wall-clock budget holds for release builds"
)]
fn planning_a_large_selection_keeps_the_frames_cheap() {
    const ITEMS: usize = 200_000;
    /// What one frame is allowed while the plan is being built. The ceiling is
    /// loose; it catches work that grows with the selection.
    const CEILING: Duration = Duration::from_millis(250);

    let fixture = Fixture::new();
    let mut children = Vec::with_capacity(ITEMS);
    for index in 0..ITEMS {
        let name = format!("f{index}.bin");
        let rel = PathBuf::from(&name);
        children.push(node(&name, &rel, false, NodeStatus::LeftOrphan, Vec::new()));
    }
    for child in &mut children {
        child.right = None;
    }
    let tree = Box::new(node(
        "",
        Path::new(""),
        true,
        NodeStatus::NotCompared,
        children,
    ));

    let mut view = FolderView::from_tree(fixture.left(), fixture.right(), &context(), 4, tree);
    view.set_journal_directory(fixture.journals());
    view.select(SelectRule::All, Scope::Left);
    assert_eq!(view.selection().side(Side::Left).len(), ITEMS);
    view.start_operation(Operation::CopyToOtherSide);

    let ctx = egui::Context::default();
    // Two warm-up frames, so font rasterization is not charged to the budget.
    for _ in 0..2 {
        let _ = ctx.run(sized_input(1_280.0, 800.0), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                view.ui(ui, &context());
            });
        });
    }
    let mut slowest = Duration::ZERO;
    let mut frames = 0usize;
    while !view.is_confirming() && frames < 400 {
        view.tick();
        let started = Instant::now();
        let _ = ctx.run(sized_input(1_280.0, 800.0), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                view.ui(ui, &context());
            });
        });
        slowest = slowest.max(started.elapsed());
        frames += 1;
    }
    assert!(frames > 0, "the plan was already finished");
    assert!(
        slowest < CEILING,
        "the slowest frame while planning took {slowest:?}"
    );
}

#[test]
fn a_synchronisation_previews_every_operation_and_honours_an_override() {
    let fixture = Fixture::new();
    fixture.write("left", "newer.txt", b"left body");
    fixture.write("left", "only-left.txt", b"left only");
    fixture.write("right", "only-right.txt", b"right only");

    let mut view = FolderView::sync_with_journal_directory(
        fixture.left(),
        fixture.right(),
        &context(),
        5,
        fixture.journals(),
    );
    view.form_mut().options.use_recycle_bin = false;
    assert!(poll_until(&mut view, |view| view.sync().preview.is_some()));
    view.set_sync_method(ca_view_folder::sync_mode::Method::UpdateRight);
    assert!(poll_until(&mut view, |view| {
        !view.sync().stale
            && view
                .sync()
                .preview
                .as_ref()
                .is_some_and(|preview| !preview.rows.is_empty())
    }));
    let before = view.sync().pending();
    assert!(before > 0, "the method chose nothing to do");

    view.override_sync_row(
        PathBuf::from("only-left.txt"),
        ca_fs::SyncAction::LeaveAlone,
    );
    assert_eq!(view.sync().pending(), before - 1);

    view.start_synchronisation();
    assert!(poll_until(&mut view, FolderView::is_confirming));
    let plan = view.pending_plan().expect("a plan is on screen");
    assert!(
        !plan
            .steps
            .iter()
            .any(|step| step.rel == Path::new("only-left.txt")),
        "an excluded row reached the plan"
    );
}

#[test]
fn a_cancelled_scan_refuses_to_synchronise() {
    let fixture = Fixture::new();
    let mut tree = folder_tree();
    tree.scan_cancelled = true;
    let mut job = ca_view_folder::opjobs::spawn_sync_plan(
        tree,
        ca_fs::SyncPreset::MirrorToRight,
        ca_fs::SyncPreview::default(),
        fixture.left(),
        fixture.right(),
        ca_fs::OperationOptions::default(),
        std::sync::Arc::new(|| {}),
    );
    let deadline = Instant::now() + PATIENCE;
    let mut refusal = None;
    while Instant::now() < deadline {
        for message in job.drain() {
            if let ca_view_folder::opjobs::PlanMessage::Done {
                outcome: ca_view_folder::opjobs::PlanOutcome::Refused(reason),
                ..
            } = message
            {
                refusal = Some(reason);
            }
        }
        if job.is_finished() {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let refusal = refusal.expect("the refusal was reported");
    assert!(
        refusal.contains("cancelled"),
        "the message does not say why: {refusal}"
    );
}

/// A target that merely exists is one the batch confirmation already answered
/// through its replace option, so execution replaces it without a second
/// question.
#[test]
fn a_target_the_batch_dialog_approved_is_replaced_without_a_second_prompt() {
    let fixture = Fixture::new();
    // The source is written last so it is the newer of the two: an older item
    // replacing a newer one is its own conflict and is not what this covers.
    fixture.write("right", "one.txt", b"old");
    std::thread::sleep(Duration::from_millis(1100));
    fixture.write("left", "one.txt", b"new");
    let mut view = fixture.view();

    plan_for(&mut view, Operation::CopyToOtherSide, Scope::Left);
    let plan = view.pending_plan().unwrap().clone();
    assert!(plan.options.overwrite, "the replace option is not set");
    assert!(plan.conflicts().contains(&Conflict::TargetExists));
    assert_eq!(plan.conflicts().len(), 1);
    view.confirm_pending();

    let asked = std::cell::Cell::new(0usize);
    let answered = std::cell::Cell::new(None);
    let finished = poll_until(&mut view, |view| {
        if let Some(question) = view.pending_question() {
            if answered.get() != Some(question.sequence) {
                asked.set(asked.get() + 1);
                view.answer(&question, Answer::Skip);
                answered.set(Some(question.sequence));
            }
        }
        view.has_summary()
    });
    assert!(finished, "the batch never reported a result");
    assert_eq!(
        asked.get(),
        0,
        "the approved conflict was put to the user again"
    );
    assert_eq!(
        std::fs::read(fixture.right().join("one.txt")).unwrap(),
        b"new",
        "the approved replacement did not run"
    );
}

/// The batch confirmation answers for what the plan was built from. A disk that
/// moved on afterwards is not covered by it, so the per-item prompt still runs.
#[test]
fn a_disk_that_moved_on_still_raises_a_prompt() {
    let fixture = Fixture::new();
    // The source has to be the newer of the two, or replacing a newer target
    // raises its own prompt and the item never reaches the drift check. File
    // times are coarse, so the two writes are kept a second apart.
    fixture.write("right", "one.txt", b"old");
    std::thread::sleep(Duration::from_millis(1100));
    fixture.write("left", "one.txt", b"alpha");
    let mut view = fixture.view();

    plan_for(&mut view, Operation::CopyToOtherSide, Scope::Left);
    assert!(view.pending_plan().unwrap().options.overwrite);
    fixture.write("left", "one.txt", b"a much longer body than before");
    view.confirm_pending();

    let drift_questions = std::cell::Cell::new(0usize);
    let answered = std::cell::Cell::new(None);
    let finished = poll_until(&mut view, |view| {
        if let Some(question) = view.pending_question() {
            if answered.get() != Some(question.sequence) {
                if matches!(
                    question.question,
                    ca_view_folder::opjobs::Question::Drifted { .. }
                ) {
                    drift_questions.set(drift_questions.get() + 1);
                }
                view.answer(&question, Answer::Skip);
                answered.set(Some(question.sequence));
            }
        }
        view.has_summary()
    });
    assert!(finished, "the batch never reported a result");
    assert_eq!(
        drift_questions.get(),
        1,
        "the drift was not put to the user"
    );
    assert_eq!(
        std::fs::read(fixture.right().join("one.txt")).unwrap(),
        b"old",
        "a step nobody approved wrote its target"
    );
}

// --------------------------------------------- one run per operation kind

/// Confirms the plan on screen, runs it, and returns the plan that ran.
///
/// The equality assertion is the point: the dialog showed these steps and no
/// others, and execution reached every one of them.
fn confirm_and_run(view: &mut FolderView) -> ca_fs::OperationPlan {
    let shown = view.pending_plan().expect("a plan is on screen").clone();
    view.confirm_pending();
    run_answering(view, Answer::Skip);
    let ran = view.pending_plan().expect("the plan is still held").clone();
    assert_eq!(shown, ran, "the plan that ran is not the plan shown");
    let report = view.last_report().expect("a result is on screen");
    assert_eq!(
        report.completed(),
        shown.steps.len(),
        "{:?}",
        report.results
    );
    ran
}

#[test]
fn a_move_to_the_other_side_writes_the_target_and_removes_the_source() {
    let fixture = Fixture::new();
    fixture.write("left", "one.txt", b"alpha");
    let mut view = fixture.view();

    plan_for(&mut view, Operation::MoveToOtherSide, Scope::Left);
    confirm_and_run(&mut view);

    assert_eq!(
        std::fs::read(fixture.right().join("one.txt")).unwrap(),
        b"alpha"
    );
    assert!(!fixture.left().join("one.txt").exists());
}

#[test]
fn a_rename_gives_the_selection_the_names_the_dialog_computed() {
    let fixture = Fixture::new();
    fixture.write("left", "one.txt", b"alpha");
    let mut view = fixture.view();
    {
        let form = view.form_mut();
        form.rename_mode = ca_view_folder::operations::RenameMode::Regex;
        form.rename_find = "one".to_string();
        form.rename_replace = "two".to_string();
    }

    plan_for(&mut view, Operation::Rename, Scope::Left);
    let plan = confirm_and_run(&mut view);

    assert!(plan
        .steps
        .iter()
        .any(|step| matches!(step.action, StepAction::Rename { .. })));
    assert!(fixture.left().join("two.txt").exists());
    assert!(!fixture.left().join("one.txt").exists());
}

#[test]
fn a_touch_writes_the_timestamp_the_dialog_holds() {
    let fixture = Fixture::new();
    fixture.write("left", "one.txt", b"alpha");
    let mut view = fixture.view();
    view.form_mut().touch_choice = ca_view_folder::operations::TouchChoice::Explicit;
    view.form_mut().touch_text = "2001-02-03 04:05:06".to_string();
    let wanted =
        ca_view_folder::operations::parse_stamp("2001-02-03 04:05:06").expect("a readable stamp");

    plan_for(&mut view, Operation::Touch, Scope::Left);
    confirm_and_run(&mut view);

    let landed = std::fs::metadata(fixture.left().join("one.txt"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(landed, wanted);
}

#[test]
fn an_attribute_change_writes_the_flags_the_dialog_holds() {
    let fixture = Fixture::new();
    fixture.write("left", "one.txt", b"alpha");
    let mut view = fixture.view();
    view.form_mut().read_only = ca_view_folder::operations::TriState::On;

    plan_for(&mut view, Operation::Attributes, Scope::Left);
    confirm_and_run(&mut view);

    let path = fixture.left().join("one.txt");
    assert!(std::fs::metadata(&path).unwrap().permissions().readonly());
    // A read-only file blocks the removal of the temporary tree around it.
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    std::fs::set_permissions(&path, permissions).unwrap();
}

#[test]
fn new_folder_creates_the_named_folder_on_the_chosen_side() {
    let fixture = Fixture::new();
    fixture.write("left", "one.txt", b"alpha");
    let mut view = fixture.view();
    view.form_mut().new_folder_name = "Made Here".to_string();
    view.form_mut().scope = Scope::Left;

    view.start_operation(Operation::NewFolder);
    assert!(poll_until(&mut view, FolderView::is_confirming));
    confirm_and_run(&mut view);

    assert!(fixture.left().join("Made Here").is_dir());
    assert!(!fixture.right().join("Made Here").exists());
}

#[test]
fn an_exchange_swaps_the_two_selections() {
    let fixture = Fixture::new();
    fixture.write("left", "left-only.txt", b"from the left");
    fixture.write("right", "right-only.txt", b"from the right");
    let mut view = fixture.view();

    plan_for(&mut view, Operation::Exchange, Scope::Both);
    confirm_and_run(&mut view);

    assert_eq!(
        std::fs::read(fixture.right().join("left-only.txt")).unwrap(),
        b"from the left"
    );
    assert_eq!(
        std::fs::read(fixture.left().join("right-only.txt")).unwrap(),
        b"from the right"
    );
    assert!(!fixture.left().join("left-only.txt").exists());
    assert!(!fixture.right().join("right-only.txt").exists());
}

#[test]
fn a_copy_to_folder_writes_into_the_named_folder_and_leaves_the_source() {
    let fixture = Fixture::new();
    fixture.write("left", "one.txt", b"alpha");
    let target = fixture.dir.path().join("elsewhere");
    std::fs::create_dir(&target).unwrap();
    let mut view = fixture.view();
    view.form_mut().target_folder = target.to_string_lossy().into_owned();

    plan_for(&mut view, Operation::CopyToFolder, Scope::Left);
    confirm_and_run(&mut view);

    assert_eq!(std::fs::read(target.join("one.txt")).unwrap(), b"alpha");
    assert!(fixture.left().join("one.txt").exists());
}

#[test]
fn a_move_to_folder_writes_into_the_named_folder_and_removes_the_source() {
    let fixture = Fixture::new();
    fixture.write("left", "one.txt", b"alpha");
    let target = fixture.dir.path().join("elsewhere");
    std::fs::create_dir(&target).unwrap();
    let mut view = fixture.view();
    view.form_mut().target_folder = target.to_string_lossy().into_owned();

    plan_for(&mut view, Operation::MoveToFolder, Scope::Left);
    confirm_and_run(&mut view);

    assert_eq!(std::fs::read(target.join("one.txt")).unwrap(), b"alpha");
    assert!(!fixture.left().join("one.txt").exists());
}

#[test]
fn exclude_adds_the_masks_to_the_name_filter_and_writes_nothing() {
    let fixture = Fixture::new();
    fixture.write("left", "one.txt", b"alpha");
    let mut view = fixture.view();

    view.select(SelectRule::All, Scope::Left);
    view.start_operation(Operation::Exclude);
    assert!(poll_until(&mut view, |view| !view
        .name_filter()
        .trim()
        .is_empty()));

    assert!(
        view.name_filter().contains("-*.txt"),
        "{}",
        view.name_filter()
    );
    assert!(fixture.left().join("one.txt").exists());
    assert!(!view.is_confirming(), "exclude asked for a confirmation");
}

#[test]
fn every_synchronisation_preset_runs_the_plan_it_previewed() {
    use ca_view_folder::sync_mode::Method;

    for (index, method) in [
        Method::UpdateLeft,
        Method::UpdateRight,
        Method::UpdateBoth,
        Method::MirrorToLeft,
        Method::MirrorToRight,
    ]
    .into_iter()
    .enumerate()
    {
        let fixture = Fixture::new();
        fixture.write("left", "only-left.txt", b"from the left");
        fixture.write("right", "only-right.txt", b"from the right");

        let instance = 100 + u64::try_from(index).unwrap();
        let mut view = FolderView::sync_with_journal_directory(
            fixture.left(),
            fixture.right(),
            &context(),
            instance,
            fixture.journals(),
        );
        view.form_mut().options.use_recycle_bin = false;
        assert!(poll_until(&mut view, |view| view.sync().preview.is_some()));
        view.set_sync_method(method);
        assert!(poll_until(&mut view, |view| !view.sync().stale
            && view.sync().preview.is_some()));
        assert!(view.sync().pending() > 0, "{method:?} chose nothing to do");

        view.start_synchronisation();
        assert!(
            poll_until(&mut view, FolderView::is_confirming),
            "{method:?} never reached the confirmation"
        );
        confirm_and_run(&mut view);

        match method {
            Method::UpdateLeft => {
                assert!(fixture.left().join("only-right.txt").exists());
                assert!(fixture.left().join("only-left.txt").exists());
            }
            Method::UpdateRight => {
                assert!(fixture.right().join("only-left.txt").exists());
                assert!(fixture.right().join("only-right.txt").exists());
            }
            Method::UpdateBoth => {
                assert!(fixture.left().join("only-right.txt").exists());
                assert!(fixture.right().join("only-left.txt").exists());
            }
            Method::MirrorToLeft => {
                assert!(fixture.left().join("only-right.txt").exists());
                assert!(!fixture.left().join("only-left.txt").exists());
            }
            Method::MirrorToRight => {
                assert!(fixture.right().join("only-left.txt").exists());
                assert!(!fixture.right().join("only-right.txt").exists());
            }
        }
    }
}

#[test]
fn a_batch_with_no_journal_directory_changes_nothing_on_disk() {
    let fixture = Fixture::new();
    fixture.write("left", "one.txt", b"alpha");
    // A file stands where the journal directory has to go, so creating it
    // fails.
    let blocked = fixture.dir.path().join("blocked");
    std::fs::write(&blocked, b"not a directory").unwrap();

    let mut view = FolderView::with_journal_directory(
        fixture.left(),
        fixture.right(),
        &context(),
        7,
        blocked.join("journals"),
    );
    assert!(poll_until(&mut view, SessionView::is_ready));

    plan_for(&mut view, Operation::CopyToOtherSide, Scope::Left);
    view.confirm_pending();
    assert!(
        poll_until(&mut view, |view| view.message().is_some()),
        "the batch neither ran nor reported why"
    );

    let (_, body) = view.message().unwrap();
    assert!(body.contains("no journal"), "{body}");
    assert!(
        !view.has_summary(),
        "a batch with no journal reported a run"
    );
    assert!(
        !fixture.right().join("one.txt").exists(),
        "a batch with no journal wrote to the disk"
    );
}

/// A confirmation whose option is cleared does not show: the plan runs at once.
#[test]
fn an_operation_whose_confirmation_is_cleared_runs_without_asking() {
    let fixture = Fixture::new();
    fixture.write("left", "one.txt", b"alpha");
    let mut view = fixture.view();

    view.set_confirmations(ca_session::options::FileOperationOptions {
        confirm_copy: false,
        ..ca_session::options::FileOperationOptions::default()
    });
    assert!(!view.asks_before(Operation::CopyToOtherSide));
    assert!(view.asks_before(Operation::Delete), "the others still ask");

    view.select(SelectRule::All, Scope::Left);
    view.start_operation(Operation::CopyToOtherSide);
    let finished = poll_until(&mut view, |view| {
        assert!(!view.is_confirming(), "a confirmation was raised");
        view.has_summary()
    });
    assert!(finished, "the batch never reported a result");
    assert!(fixture.right().join("one.txt").exists());
}

/// With the option set, the same operation stops for a confirmation.
#[test]
fn the_same_operation_asks_while_its_option_is_set() {
    let fixture = Fixture::new();
    fixture.write("left", "one.txt", b"alpha");
    let mut view = fixture.view();
    view.set_confirmations(ca_session::options::FileOperationOptions::default());

    view.select(SelectRule::All, Scope::Left);
    view.start_operation(Operation::CopyToOtherSide);
    assert!(poll_until(&mut view, FolderView::is_confirming));
    view.cancel_pending();
    assert!(!fixture.right().join("one.txt").exists());
}

/// The backup page reaches the operation form: a replaced file is copied under
/// the suffix the page names, and a later frame keeps a choice the dialog made.
#[test]
fn the_backup_page_sets_the_copy_a_replacing_operation_takes() {
    let fixture = Fixture::new();
    fixture.write("left", "one.txt", b"alpha");
    let mut view = fixture.view();
    assert_eq!(view.form_mut().options.backup, None);

    let mut options = ca_session::options::ProgramOptions::default();
    options.backups.before_overwrite = true;
    options.backups.suffix = ".prev".to_owned();
    let resolved = std::sync::Arc::new(ca_ui::options::AppOptions::resolve(
        options,
        ca_ui::theme::Variant::Light,
        ca_session::AdminPolicies::default(),
    ));
    let ctx = egui::Context::default();
    let held = context();
    let frame = |view: &mut FolderView| {
        let _ = ctx.run(sized_input(1_280.0, 800.0), |ctx| {
            ca_ui::options::install(ctx, std::sync::Arc::clone(&resolved));
            egui::CentralPanel::default().show(ctx, |ui| {
                view.ui(ui, &held);
            });
        });
    };
    frame(&mut view);
    let backup = view.form_mut().options.backup.clone().unwrap();
    assert_eq!(backup.suffix, ".prev");
    assert_eq!(backup.folder, None);

    view.form_mut().options.backup = None;
    frame(&mut view);
    assert_eq!(view.form_mut().options.backup, None);
}

/// Stop given while a setting changed in the dialog rebuilds the plan: the
/// rebuilt plan never runs, and the view says so.
#[test]
fn stop_while_the_plan_is_built_again_runs_nothing() {
    let fixture = Fixture::new();
    fixture.write("left", "one.txt", b"alpha");
    let mut view = fixture.view();

    plan_for(&mut view, Operation::Delete, Scope::Left);
    view.form_mut().options.clear_read_only_targets = true;
    view.confirm_pending();
    assert!(!view.is_confirming());
    assert!(view.accepts(ca_ui::command::Command::Cancel));
    view.run(ca_ui::command::Command::Cancel);

    assert!(poll_until(&mut view, |view| !view
        .accepts(ca_ui::command::Command::Cancel)));
    assert!(fixture.left().join("one.txt").exists(), "the batch ran");
    assert!(!view.has_summary() && !view.is_running_batch());
    assert!(!view.is_confirming());
    assert!(view.message().is_some(), "the stop is not reported");
}

/// Stop given while the first plan is built, with the confirmation of the
/// operation turned off: the plan never runs.
#[test]
fn stop_while_the_first_plan_is_built_runs_nothing() {
    let fixture = Fixture::new();
    fixture.write("left", "one.txt", b"alpha");
    let mut view = fixture.view();
    view.set_confirmations(ca_session::options::FileOperationOptions {
        confirm_delete: false,
        ..ca_session::options::FileOperationOptions::default()
    });

    view.select(SelectRule::All, Scope::Left);
    view.start_operation(Operation::Delete);
    assert!(view.accepts(ca_ui::command::Command::Cancel));
    view.run(ca_ui::command::Command::Cancel);

    assert!(poll_until(&mut view, |view| !view
        .accepts(ca_ui::command::Command::Cancel)));
    assert!(fixture.left().join("one.txt").exists(), "the batch ran");
    assert!(!view.has_summary() && !view.is_running_batch());
}

/// A settings change while a plan holds the comparison reads both sides
/// again. The plan built from the comparison it replaced is dropped, and the
/// next plan is built from the new one.
#[test]
fn a_settings_change_while_a_plan_is_built_drops_that_plan() {
    let fixture = Fixture::new();
    for side in ["left", "right"] {
        fixture.write(side, "sub/a.txt", b"a");
        fixture.write(side, "sub/x.log", b"x");
    }
    let mut view = fixture.view();

    view.select(SelectRule::All, Scope::Left);
    view.start_operation(Operation::Delete);
    let mut settings = view.session_settings();
    settings.name_filters.exclude_files = vec!["*.log".to_owned()];
    view.apply_session_settings(&settings);
    assert!(poll_until(&mut view, FolderView::is_ready));
    assert!(
        !view.is_confirming(),
        "a plan built from the replaced comparison reached the dialog"
    );

    view.select(SelectRule::All, Scope::Left);
    view.start_operation(Operation::Delete);
    assert!(poll_until(&mut view, FolderView::is_confirming));
    let plan = view.pending_plan().unwrap();
    assert!(
        plan.steps
            .iter()
            .all(|step| step.rel.file_name() != Some(std::ffi::OsStr::new("x.log"))),
        "the plan names an item the filter hides: {:?}",
        plan.steps
    );
    assert!(plan.steps.iter().any(|step| step.rel.ends_with("a.txt")));
}

/// A new name that a Windows path does not reach is refused while the plan is
/// built. When no step is left, the message gives that reason.
#[cfg(windows)]
#[test]
fn a_rename_refused_for_its_new_name_says_why() {
    let fixture = Fixture::new();
    fixture.write("left", "one.txt", b"alpha");
    fixture.write("left", "one", b"kept");
    let mut view = fixture.view();
    {
        let form = view.form_mut();
        form.rename_mode = ca_view_folder::operations::RenameMode::Regex;
        form.rename_find = r"^one\.txt$".to_string();
        form.rename_replace = "one.".to_string();
    }

    view.select(SelectRule::All, Scope::Left);
    view.start_operation(Operation::Rename);
    assert!(poll_until(&mut view, |view| view.message().is_some()
        || view.is_confirming()));
    let (_, body) = view.message().expect("the refusal is reported");
    assert!(body.contains("\"one.\""), "{body}");
    assert!(body.contains("ends with a dot or a space"), "{body}");
    assert_eq!(std::fs::read(fixture.left().join("one")).unwrap(), b"kept");
    assert_eq!(
        std::fs::read(fixture.left().join("one.txt")).unwrap(),
        b"alpha"
    );
}

/// A folder name that a Windows path does not reach is refused while the plan
/// is built, and the message gives the reason.
#[cfg(windows)]
#[test]
fn a_new_folder_refused_for_its_name_says_why() {
    let fixture = Fixture::new();
    fixture.write("left", "one.txt", b"alpha");
    let mut view = fixture.view();
    view.form_mut().new_folder_name = "fresh.".to_string();
    view.form_mut().scope = Scope::Left;

    view.start_operation(Operation::NewFolder);
    assert!(poll_until(&mut view, |view| view.message().is_some()
        || view.is_confirming()));
    let (_, body) = view.message().expect("the refusal is reported");
    assert!(body.contains("\"fresh.\""), "{body}");
    assert!(body.contains("ends with a dot or a space"), "{body}");
    assert!(!fixture.left().join("fresh").exists());
}

/// A tab whose batch writes files refuses to close without a question and
/// says it is busy, so a close that waits tries again when the batch ends.
#[test]
fn a_running_batch_keeps_the_tab_open_and_says_it_is_busy() {
    let fixture = Fixture::new();
    fixture.write("left", "one.txt", b"alpha");
    let mut view = fixture.view();
    assert!(!view.is_busy());

    plan_for(&mut view, Operation::CopyToOtherSide, Scope::Left);
    view.confirm_pending();
    assert!(view.is_busy(), "a running batch is not reported as busy");
    assert!(!view.may_close());
    assert!(poll_until(&mut view, FolderView::has_summary));
    assert!(!view.is_busy());
    assert!(view.may_close());
}

/// The synchronisation tab refuses to close while its batch writes files, as
/// the comparison tab does.
#[test]
fn a_synchronisation_tab_stays_open_while_its_batch_runs() {
    let fixture = Fixture::new();
    fixture.write("left", "one.txt", b"alpha");
    let mut sync = ca_view_folder::FolderSyncView::from(fixture.view());
    plan_for(sync.view_mut(), Operation::CopyToOtherSide, Scope::Left);
    sync.view_mut().confirm_pending();
    assert!(sync.is_busy());
    assert!(!sync.may_close(), "the tab closed while its batch ran");
    assert!(poll_until(sync.view_mut(), FolderView::has_summary));
    assert!(!sync.is_busy());
    assert!(sync.may_close());
}

/// Copy to Folder into a folder that holds the name lists the item among the
/// conflicts the confirmation shows. With Replace off the item keeps its
/// content.
#[test]
fn a_copy_to_folder_onto_an_occupied_name_lists_the_conflict() {
    let fixture = Fixture::new();
    fixture.write("left", "one.txt", b"alpha");
    let target = fixture.dir.path().join("elsewhere");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("one.txt"), b"kept").unwrap();
    let mut view = fixture.view();
    view.form_mut().target_folder = target.to_string_lossy().into_owned();
    view.form_mut().options.overwrite = false;

    plan_for(&mut view, Operation::CopyToFolder, Scope::Left);
    let plan = view.pending_plan().expect("a plan is on screen").clone();
    let grouped = conflicts_by_kind(&plan);
    assert!(
        grouped.iter().any(|(conflict, paths)| {
            *conflict == Conflict::TargetExists && paths.contains(&target.join("one.txt"))
        }),
        "{grouped:?}"
    );
    view.confirm_pending();
    run_answering(&mut view, Answer::Skip);
    assert_eq!(std::fs::read(target.join("one.txt")).unwrap(), b"kept");
}

/// Move to Folder into the base folder of the side, with verification on,
/// gives no step for an item whose destination is the item itself, and the
/// item stays.
#[test]
fn a_move_to_folder_onto_the_base_folder_leaves_the_item() {
    let fixture = Fixture::new();
    fixture.write("left", "one.txt", b"alpha");
    let mut view = fixture.view();
    view.form_mut().target_folder = fixture.left().to_string_lossy().into_owned();
    view.form_mut().path_option = ca_fs::PathOption::KeepBase;
    view.form_mut().options.verify = ca_fs::Verify::Hash;

    plan_for(&mut view, Operation::MoveToFolder, Scope::Left);
    let plan = view.pending_plan().expect("a plan is on screen").clone();
    assert!(
        plan.steps
            .iter()
            .all(|step| !matches!(step.action, StepAction::MoveFile { .. })),
        "{:?}",
        plan.steps
    );
    assert!(
        plan.skipped
            .iter()
            .any(|skip| skip.reason.contains("itself")),
        "{:?}",
        plan.skipped
    );
    view.confirm_pending();
    assert!(poll_until(&mut view, FolderView::has_summary));
    assert_eq!(
        std::fs::read(fixture.left().join("one.txt")).unwrap(),
        b"alpha"
    );
}
