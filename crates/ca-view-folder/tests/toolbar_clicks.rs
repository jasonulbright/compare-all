//! Every folder and folder merge toolbar control, pressed through pointer
//! input, does what it does today.
//!
//! Controls are found by their accessible label. A command item is checked on
//! the shared bar, where it has to report its own command; a control the view
//! draws into a slot is checked on the whole view, where its effect has to be
//! visible in the view's state.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

use ca_session::SessionKind;
use ca_ui::command::Command;
use ca_ui::testing::context;
use ca_ui::testing::probe::{
    self, all_enabled, every_command_reports, Expected, Probe, Reach, NARROW_ROOM, WIDE_ROOM,
};
use ca_ui::toolbar::{self, Item, ToolbarView};
use ca_ui::view::{OpenRequest, SessionView, ViewAction};
use ca_view_folder::{FolderMergeView, FolderView};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// How long a test waits for background work before giving up.
const PATIENCE: Duration = Duration::from_secs(30);

/// A window wide enough to hold the whole toolbar on one line.
const WINDOW: [f32; 2] = [2_400.0, 900.0];

/// Each command item of the folder toolbar: name, label, and the command it
/// runs.
const FOLDER_COMMANDS: &[Expected] = &[
    ("copy", "Copy", Command::CopyToOtherSide),
    (
        "compare-contents",
        "Compare Contents",
        Command::CompareContents,
    ),
    ("refresh", "Refresh", Command::Reload),
    ("swap", "Swap", Command::SwapSides),
    ("stop", "Stop", Command::Cancel),
    ("report", "Report", Command::CompareReport),
];

/// Each command item of the folder merge toolbar.
const FOLDER_MERGE_COMMANDS: &[Expected] = &[
    (
        "previous-conflict",
        "Prev Conflict",
        Command::PreviousConflict,
    ),
    ("next-conflict", "Next Conflict", Command::NextConflict),
    ("take-left", "Take Left", Command::TakeLeft),
    ("take-center", "Take Center", Command::TakeCenter),
    ("take-right", "Take Right", Command::TakeRight),
    ("merge", "Merge", Command::MergeFolders),
    ("copy-to-output", "Copy to Output", Command::CopyToOutput),
    ("text-merge", "Text Merge", Command::OpenTextMerge),
    ("center-pane", "Center Pane", Command::ToggleCenterPane),
    (
        "ignore-same",
        "Ignore Same",
        Command::ToggleIgnoreSameChanges,
    ),
    ("swap", "Swap", Command::SwapSides),
    ("reload", "Reload", Command::Reload),
    ("report", "Report", Command::CompareReport),
];

/// What each slot of the folder toolbar draws.
///
/// A slot with pressable controls names their labels; a slot that holds a
/// drop down or a text field names none.
const FOLDER_SLOTS: &[(&str, &[&str])] = &[
    ("home", &["Home"]),
    ("sessions", &["Sessions"]),
    ("all", &["All"]),
    ("diffs", &["Diffs"]),
    ("same", &["Same"]),
    ("filter", &[]),
    ("structure", &[]),
    ("minor", &["Minor"]),
    ("rules", &["Rules"]),
    ("expand", &["Expand"]),
    ("collapse", &["Collapse"]),
    ("select", &["Select"]),
    ("files", &["Files"]),
    ("method", &[]),
    ("name-filter", &[]),
    ("peek", &["Peek"]),
];

/// What each slot of the folder merge toolbar draws.
const FOLDER_MERGE_SLOTS: &[(&str, &[&str])] = &[("filter", &[])];

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
        assert!(poll_until(&mut view, SessionView::is_ready));
        view.form_mut().options.use_recycle_bin = false;
        view
    }

    fn sync_view(&self) -> FolderView {
        let mut view = FolderView::sync_with_journal_directory(
            self.path("left"),
            self.path("right"),
            &context(),
            2,
            self.path("journals"),
        );
        view.form_mut().options.use_recycle_bin = false;
        view
    }

    fn merge_view(&self) -> FolderMergeView {
        let request = OpenRequest::new(
            SessionKind::FolderMerge,
            self.path("left"),
            self.path("right"),
        )
        .with_center(Some(self.path("center")))
        .with_output(Some(self.path("output")));
        let mut view =
            FolderMergeView::with_journal_directory(&request, &context(), 3, self.path("journals"));
        view.operation_options_mut().use_recycle_bin = false;
        view
    }
}

/// A frame body that ticks the view and draws it.
fn frame<'a>(
    view: &'a mut FolderView,
    actions: &'a mut Vec<ViewAction>,
) -> impl FnMut(&egui::Context) + 'a {
    let shared = context();
    move |ctx| {
        view.tick();
        egui::CentralPanel::default().show(ctx, |ui| {
            actions.extend(view.ui(ui, &shared));
        });
    }
}

/// A probe that has laid the view out twice, so every control has a place.
fn probe_over(view: &mut FolderView) -> Probe {
    let mut probe = Probe::new(WINDOW[0], WINDOW[1]);
    let mut actions = Vec::new();
    let mut draw = frame(view, &mut actions);
    probe.idle(&mut draw);
    probe.idle(&mut draw);
    probe
}

/// Press `label` on the whole view and return the actions it asked for.
fn press(probe: &mut Probe, view: &mut FolderView, label: &str) -> Vec<ViewAction> {
    let mut actions = Vec::new();
    {
        let mut draw = frame(view, &mut actions);
        probe.idle(&mut draw);
        probe.click(label, &mut draw).unwrap();
    }
    actions
}

fn names(items: &[Item]) -> Vec<&'static str> {
    items.iter().map(Item::name).collect()
}

fn declared(view: ToolbarView) -> Vec<&'static str> {
    toolbar::defaults(view)
        .iter()
        .map(|item| item.name)
        .collect()
}

fn slots(items: &[Item]) -> Vec<&'static str> {
    items
        .iter()
        .filter_map(|item| match item {
            Item::Widget { name, .. } => Some(*name),
            _ => None,
        })
        .collect()
}

#[test]
fn every_folder_toolbar_command_reports_itself() {
    let fixture = Fixture::new();
    let items = all_enabled(&fixture.view().toolbar_items());
    every_command_reports(&items, FOLDER_COMMANDS, WIDE_ROOM, Reach::Bar).unwrap();
    every_command_reports(&items, FOLDER_COMMANDS, NARROW_ROOM, Reach::Overflow).unwrap();
}

#[test]
fn every_folder_merge_toolbar_command_reports_itself() {
    let fixture = Fixture::new();
    let items = all_enabled(&fixture.merge_view().toolbar_items());
    every_command_reports(&items, FOLDER_MERGE_COMMANDS, WIDE_ROOM, Reach::Bar).unwrap();
    every_command_reports(&items, FOLDER_MERGE_COMMANDS, NARROW_ROOM, Reach::Overflow).unwrap();
}

#[test]
fn both_toolbars_draw_what_the_options_page_lists() {
    let fixture = Fixture::new();
    assert_eq!(
        names(&fixture.view().toolbar_items()),
        declared(ToolbarView::Folder)
    );
    assert_eq!(
        names(&fixture.merge_view().toolbar_items()),
        declared(ToolbarView::FolderMerge)
    );
}

/// A pending control stays on the bar, refuses the press, and changes nothing.
fn pending(label: &str) -> Result<(), String> {
    let fixture = Fixture::new();
    let mut view = fixture.view();
    let mut probe = probe_over(&mut view);
    if probe.find(label)?.enabled {
        return Err(format!("{label} is enabled but does nothing yet"));
    }
    let rebuilt = view.rebuild_count();
    let rows = view.rows().len();
    let actions = press(&mut probe, &mut view, label);
    if !actions.is_empty() || view.rebuild_count() != rebuilt || view.rows().len() != rows {
        return Err(format!("the disabled {label} acted"));
    }
    Ok(())
}

/// A display filter button shows its filter.
fn filter_button(label: &str, shown: &str) -> Result<(), String> {
    let fixture = Fixture::new();
    let mut view = fixture.view();
    let mut probe = probe_over(&mut view);
    let start = if label == "All" { "Diffs" } else { "All" };
    press(&mut probe, &mut view, start);
    let before = view.report_filter();
    press(&mut probe, &mut view, label);
    if before == shown || view.report_filter() != shown {
        return Err(format!(
            "{label} left the filter at {:?}, not {shown:?}",
            view.report_filter()
        ));
    }
    Ok(())
}

/// Expand opens every folder that Collapse closed, and Collapse closes them.
fn expand_or_collapse(label: &str) -> Result<(), String> {
    let fixture = Fixture::new();
    let mut view = fixture.view();
    view.run(Command::ExpandAll);
    let open = view.rows().len();
    view.run(Command::CollapseAll);
    let closed = view.rows().len();
    if open <= closed {
        return Err(format!("the fixture has no folder to open: {open} rows"));
    }
    if label == "Expand" {
        let mut probe = probe_over(&mut view);
        press(&mut probe, &mut view, "Expand");
        return (view.rows().len() == open)
            .then_some(())
            .ok_or_else(|| format!("Expand left {} rows, not {open}", view.rows().len()));
    }
    view.run(Command::ExpandAll);
    let mut probe = probe_over(&mut view);
    press(&mut probe, &mut view, "Collapse");
    (view.rows().len() == closed)
        .then_some(())
        .ok_or_else(|| format!("Collapse left {} rows, not {closed}", view.rows().len()))
}

/// The Select menu selects every row.
fn select_menu() -> Result<(), String> {
    let fixture = Fixture::new();
    let mut view = fixture.view();
    let mut probe = probe_over(&mut view);
    press(&mut probe, &mut view, "Select");
    if !view.selection().is_empty() {
        return Err("opening the menu selected rows".to_owned());
    }
    press(&mut probe, &mut view, "Select All");
    if view.selection().is_empty() {
        return Err("Select All selected nothing".to_owned());
    }
    Ok(())
}

/// Press every pressable control of `slot` and check what it did.
fn check_folder_slot(slot: &str) -> Result<(), String> {
    match slot {
        "home" => pending("Home"),
        "sessions" => pending("Sessions"),
        "minor" => pending("Minor"),
        "rules" => pending("Rules"),
        "files" => pending("Files"),
        "peek" => pending("Peek"),
        "all" => filter_button("All", "all"),
        "diffs" => filter_button("Diffs", "mismatches"),
        "same" => filter_button("Same", "matches"),
        "expand" => expand_or_collapse("Expand"),
        "collapse" => expand_or_collapse("Collapse"),
        "select" => select_menu(),
        // Holds no pressable control, which the untested button test enforces.
        "filter" | "structure" | "method" | "name-filter" => Ok(()),
        other => Err(format!("{other} has no click test")),
    }
}

#[test]
fn every_folder_toolbar_slot_is_pressed_and_acts() {
    let fixture = Fixture::new();
    let items = fixture.view().toolbar_items();
    let listed: Vec<&str> = FOLDER_SLOTS.iter().map(|(slot, _)| *slot).collect();
    assert_eq!(slots(&items), listed, "the slot table is out of date");
    let failures: Vec<String> = slots(&items)
        .into_iter()
        .filter_map(|slot| check_folder_slot(slot).err())
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn every_folder_merge_toolbar_slot_is_listed() {
    let fixture = Fixture::new();
    let items = fixture.merge_view().toolbar_items();
    let listed: Vec<&str> = FOLDER_MERGE_SLOTS.iter().map(|(slot, _)| *slot).collect();
    assert_eq!(slots(&items), listed, "the slot table is out of date");
}

#[test]
fn every_folder_merge_toolbar_toggle_changes_its_setting() {
    type Read = fn(&FolderMergeView) -> bool;
    let toggles: [(&str, Read); 2] = [
        ("Center Pane", FolderMergeView::shows_center),
        ("Ignore Same", FolderMergeView::ignores_same_changes),
    ];
    let fixture = Fixture::new();
    fixture.write("center", "same.txt", b"alpha\n");
    let declared: Vec<&str> = fixture
        .merge_view()
        .toolbar_items()
        .iter()
        .filter_map(|item| match item {
            Item::Command {
                label,
                checked: Some(_),
                ..
            } => Some(*label),
            _ => None,
        })
        .collect();
    let tested: Vec<&str> = toggles.iter().map(|(label, _)| *label).collect();
    assert_eq!(declared, tested, "a toggle has no click test");
    let mut failures = Vec::new();
    for (label, read) in toggles {
        let mut view = fixture.merge_view();
        let deadline = Instant::now() + PATIENCE;
        while view.tree().is_none() {
            assert!(Instant::now() < deadline, "the merge never finished");
            view.tick();
            std::thread::sleep(Duration::from_millis(2));
        }
        let mut probe = Probe::new(WINDOW[0], WINDOW[1]);
        probe::settle(&mut probe, &mut view);
        if let Err(error) = probe::toggle_view(&mut probe, &mut view, label, read) {
            failures.push(error);
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn the_folder_merge_toolbar_holds_no_untested_button() {
    let fixture = Fixture::new();
    let mut view = fixture.merge_view();
    let deadline = Instant::now() + PATIENCE;
    while view.tree().is_none() {
        assert!(Instant::now() < deadline, "the merge never finished");
        view.tick();
        std::thread::sleep(Duration::from_millis(2));
    }
    let items = view.toolbar_items();
    let mut probe = Probe::new(WINDOW[0], WINDOW[1]);
    probe::settle(&mut probe, &mut view);
    let strangers = probe::toolbar_strangers(&probe, &items, &[]).unwrap();
    assert!(
        strangers.is_empty(),
        "toolbar controls with no click test: {strangers:?}"
    );
}

#[test]
fn the_folder_toolbar_holds_no_untested_button() {
    let fixture = Fixture::new();
    let mut view = fixture.view();
    let items = view.toolbar_items();
    let mut known: Vec<&str> = probe::command_items(&items)
        .into_iter()
        .map(|(_, label, _)| label)
        .collect();
    known.extend(FOLDER_SLOTS.iter().flat_map(|(_, labels)| labels.iter()));
    let probe = probe_over(&mut view);
    let region = probe::region_of(probe.controls(), &known).expect("the toolbar was drawn");
    let strangers = probe::strangers(probe.controls(), &known, region);
    assert!(
        strangers.is_empty(),
        "toolbar controls with no click test: {strangers:?}"
    );
}

#[test]
fn the_sync_now_button_plans_the_synchronisation() {
    let fixture = Fixture::new();
    let mut view = fixture.sync_view();
    view.set_sync_method(ca_view_folder::sync_mode::Method::UpdateRight);
    assert!(poll_until(&mut view, |view| {
        view.sync().preview.is_some() && !view.sync().stale && view.sync_pending() > 0
    }));
    let mut probe = probe_over(&mut view);
    assert!(probe.find("Sync Now").unwrap().enabled);
    assert!(!view.is_confirming());
    press(&mut probe, &mut view, "Sync Now");
    assert!(poll_until(&mut view, FolderView::is_confirming));
    let plan = view.pending_plan().expect("a plan is on screen");
    assert!(!plan.steps.is_empty());
}
