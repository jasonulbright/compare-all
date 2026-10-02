//! A pressed toolbar button runs its own command.
//!
//! Every control is found by its accessible label and pressed through pointer
//! input, so a redrawn button that keeps its label and its wiring passes and a
//! button wired to the wrong command fails.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

use ca_session::options::CommandOptions;
use ca_session::SessionStore;
use ca_ui::command::Command;
use ca_ui::report::{
    ReportAction, ReportDialog, ReportKind, ReportMessage, ReportSettings, Target,
};
use ca_ui::testing::probe::{
    self, click_toolbar, every_command_reports, Expected, Probe, Reach, NARROW_ROOM, WIDE_ROOM,
};
use ca_ui::toolbar::{self, Item, Layout, ToolbarView};
use ca_ui::widgets;
use ca_ui::worker::{Cancel, Emitter, Job};
use ca_ui::workspace::{WorkspaceAction, WorkspaceManager};
use std::cell::Cell;
use std::sync::Arc;

/// What each command item of the sample bar is.
const SAMPLE: &[Expected] = &[
    ("reload", "Reload", Command::Reload),
    ("swap", "Swap", Command::SwapSides),
    ("favor-left", "Favor Left", Command::FavorLeft),
    ("favor-right", "Favor Right", Command::FavorRight),
    ("report", "Report", Command::CompareReport),
];

/// A bar that carries every kind of item the builder draws.
fn sample() -> Vec<Item> {
    vec![
        Item::command("reload", Command::Reload, "Reload", true, ""),
        Item::separator("separator-1"),
        Item::widget("slot", 60.0),
        Item::command("swap", Command::SwapSides, "Swap", true, ""),
        Item::toggle("favor-left", Command::FavorLeft, "Favor Left", true, false),
        Item::toggle(
            "favor-right",
            Command::FavorRight,
            "Favor Right",
            true,
            true,
        ),
        Item::command("report", Command::CompareReport, "Report", true, ""),
    ]
}

#[test]
fn every_command_item_on_the_bar_reports_its_own_command() {
    every_command_reports(&sample(), SAMPLE, WIDE_ROOM, Reach::Bar).unwrap();
}

#[test]
fn every_command_item_in_the_overflow_menu_reports_its_own_command() {
    every_command_reports(&sample(), SAMPLE, NARROW_ROOM, Reach::Overflow).unwrap();
}

#[test]
fn two_items_that_trade_commands_fail_the_check() {
    let mut items = sample();
    for item in &mut items {
        if let Item::Command { command, .. } = item {
            *command = match *command {
                Command::Reload => Command::SwapSides,
                Command::SwapSides => Command::Reload,
                other => other,
            };
        }
    }
    let error = every_command_reports(&items, SAMPLE, WIDE_ROOM, Reach::Bar).unwrap_err();
    assert!(
        error.contains("pressing \"Reload\" has to run Reload") && error.contains("item 1"),
        "{error}"
    );
}

#[test]
fn an_item_the_test_does_not_state_fails_the_check() {
    let mut items = sample();
    items.push(Item::command("save", Command::SaveFile, "Save", true, ""));
    let error = every_command_reports(&items, SAMPLE, WIDE_ROOM, Reach::Bar).unwrap_err();
    assert!(error.contains("SaveFile"), "{error}");
}

#[test]
fn a_stored_order_keeps_each_button_on_its_own_command() {
    let mut options = CommandOptions::default();
    options.set_toolbar(
        ToolbarView::Merge.id(),
        vec!["report".to_owned(), "swap".to_owned(), "reload".to_owned()],
    );
    let layout = Layout::from_options(&options, ToolbarView::Merge);
    for (label, command) in [
        ("Report", Command::CompareReport),
        ("Swap", Command::SwapSides),
        ("Reload", Command::Reload),
    ] {
        let click = click_toolbar(&sample(), &layout, WIDE_ROOM, label).unwrap();
        assert_eq!(click.commands, vec![command], "{label}");
    }
}

#[test]
fn a_disabled_item_reports_nothing_on_the_bar_or_in_the_overflow_menu() {
    let items = [
        Item::command(
            "reload",
            Command::Reload,
            "Reload",
            false,
            "Work is running",
        ),
        Item::toggle("favor-left", Command::FavorLeft, "Favor Left", false, true),
        Item::command("swap", Command::SwapSides, "Swap", true, ""),
    ];
    for room in [WIDE_ROOM, NARROW_ROOM] {
        for label in ["Reload", "Favor Left"] {
            let click = click_toolbar(&items, &Layout::built_in(), room, label).unwrap();
            assert!(
                click.commands.is_empty(),
                "{label} reported {:?} at {room}",
                click.commands
            );
        }
        let click = click_toolbar(&items, &Layout::built_in(), room, "Swap").unwrap();
        assert_eq!(click.commands, vec![Command::SwapSides]);
    }
}

#[test]
fn a_disabled_item_announces_that_it_is_disabled() {
    let items = [
        Item::command(
            "reload",
            Command::Reload,
            "Reload",
            false,
            "Work is running",
        ),
        Item::command("swap", Command::SwapSides, "Swap", true, ""),
    ];
    let mut probe = Probe::new(1_280.0, 400.0);
    let mut draw = |ctx: &egui::Context| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let _ = toolbar::show(
                ui,
                egui::Id::new("bar"),
                &items,
                &Layout::built_in(),
                |_, _| {},
            );
        });
    };
    probe.idle(&mut draw);
    assert!(!probe.find("Reload").unwrap().enabled);
    assert!(probe.find("Swap").unwrap().enabled);
}

#[test]
fn a_hidden_item_cannot_be_pressed_and_the_others_still_can() {
    let mut options = CommandOptions::default();
    options.set_hidden_from_toolbar(ToolbarView::Merge.id(), "swap", true);
    let layout = Layout::from_options(&options, ToolbarView::Merge);
    for room in [WIDE_ROOM, NARROW_ROOM] {
        let error = click_toolbar(&sample(), &layout, room, "Swap").unwrap_err();
        assert!(error.contains("no control is labelled"), "{error}");
        let click = click_toolbar(&sample(), &layout, room, "Report").unwrap();
        assert_eq!(click.commands, vec![Command::CompareReport]);
    }
}

#[test]
fn a_toggle_announces_its_state() {
    let items = sample();
    let mut probe = Probe::new(1_280.0, 400.0);
    let mut draw = |ctx: &egui::Context| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let _ = toolbar::show(
                ui,
                egui::Id::new("bar"),
                &items,
                &Layout::built_in(),
                |_, _| {},
            );
        });
    };
    probe.idle(&mut draw);
    assert_eq!(probe.find("Favor Left").unwrap().toggled, Some(false));
    assert_eq!(probe.find("Favor Right").unwrap().toggled, Some(true));
}

/// Every call of the shared button outside the builder: the file that makes
/// it, the label it passes, and the test file that presses it.
const DIRECT_BUTTONS: &[(&str, &str, &str)] = &[
    ("ca-ui/src/report/dialog.rs", "Write", THIS_FILE),
    ("ca-ui/src/report/dialog.rs", "Stop", THIS_FILE),
    ("ca-ui/src/workspace.rs", "Save Workspace", THIS_FILE),
    ("ca-view-folder/src/lib.rs", "Home", FOLDER),
    ("ca-view-folder/src/lib.rs", "Sessions", FOLDER),
    ("ca-view-folder/src/lib.rs", "Minor", FOLDER),
    ("ca-view-folder/src/lib.rs", "Rules", FOLDER),
    ("ca-view-folder/src/lib.rs", "Expand", FOLDER),
    ("ca-view-folder/src/lib.rs", "Collapse", FOLDER),
    ("ca-view-folder/src/lib.rs", "Files", FOLDER),
    ("ca-view-folder/src/lib.rs", "Peek", FOLDER),
    ("ca-view-folder/src/lib.rs", "Sync Now", FOLDER),
    ("ca-view-picture/src/lib.rs", "Home", PICTURE),
    ("ca-view-picture/src/lib.rs", "Zoom in", PICTURE),
    ("ca-view-picture/src/lib.rs", "Zoom out", PICTURE),
    ("ca-view-picture/src/lib.rs", "1:1", PICTURE),
    ("ca-view-picture/src/lib.rs", "Fit", PICTURE),
    ("ca-view-picture/src/lib.rs", "Rotate right", PICTURE),
    ("ca-view-picture/src/lib.rs", "Rotate left", PICTURE),
    ("ca-view-picture/src/lib.rs", "Flip across y", PICTURE),
    ("ca-view-picture/src/lib.rs", "Flip across x", PICTURE),
    ("ca-view-picture/src/lib.rs", "Swap", PICTURE),
    ("ca-view-picture/src/lib.rs", "Reload", PICTURE),
    ("ca-view-picture/src/lib.rs", "Stop", PICTURE),
    ("ca-view-picture/src/lib.rs", "Compare as hex", PICTURE),
    ("ca-view-text/src/lib.rs", "Sessions", TEXT),
    ("ca-view-text/src/lib.rs", "Format", TEXT),
];

const THIS_FILE: &str = "ca-ui/tests/toolbar_clicks.rs";
const FOLDER: &str = "ca-view-folder/tests/toolbar_clicks.rs";
const PICTURE: &str = "ca-view-picture/tests/toolbar_clicks.rs";
const TEXT: &str = "ca-view-text/tests/toolbar_clicks.rs";

/// The file the builder draws its command buttons from.
const BUILDER: &str = "ca-ui/src/toolbar.rs";

fn crates_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn rust_files(folder: &std::path::Path, found: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, found);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            found.push(path);
        }
    }
}

/// Every call of the shared button in the sources of every crate, as the file
/// and the literal label, or no label where a variable is passed.
fn direct_calls() -> Vec<(String, Option<String>)> {
    const CALL: &str = "toolbar_button(";
    let root = crates_root();
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&root).unwrap().filter_map(Result::ok) {
        rust_files(&entry.path().join("src"), &mut files);
    }
    let mut calls = Vec::new();
    for file in files {
        let source = std::fs::read_to_string(&file).unwrap();
        let name = file
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        for (at, _) in source.match_indices(CALL) {
            if source[..at].ends_with("fn ") {
                continue;
            }
            let arguments = &source[at + CALL.len()..];
            let label = arguments
                .split(',')
                .nth(1)
                .map(str::trim)
                .and_then(|argument| argument.strip_prefix('"'))
                .and_then(|rest| rest.split('"').next())
                .map(str::to_owned);
            calls.push((name.clone(), label));
        }
    }
    calls.sort();
    calls
}

#[test]
fn every_direct_toolbar_button_has_a_click_test() {
    let calls = direct_calls();
    let mut listed: Vec<(String, Option<String>)> = DIRECT_BUTTONS
        .iter()
        .map(|(file, label, _)| ((*file).to_owned(), Some((*label).to_owned())))
        .collect();
    listed.push((BUILDER.to_owned(), None));
    listed.sort();
    assert_eq!(
        calls, listed,
        "a toolbar button was added or removed; press it in a test and list it here"
    );
    for (file, label, test) in DIRECT_BUTTONS {
        let source = std::fs::read_to_string(crates_root().join(test)).unwrap();
        assert!(
            source.contains(&format!("\"{label}\"")),
            "{test} never presses {label:?} from {file}"
        );
    }
}

#[test]
fn every_toolbar_view_is_named_by_one_click_test_file() {
    // Exhaustive on purpose: a new toolbar does not compile here until its
    // click test file is named.
    for view in ToolbarView::ALL {
        let file = match view {
            ToolbarView::Text => "ca-view-text/tests/toolbar_clicks.rs",
            ToolbarView::Folder | ToolbarView::FolderMerge => {
                "ca-view-folder/tests/toolbar_clicks.rs"
            }
            ToolbarView::Hex => "ca-view-hex/tests/toolbar_clicks.rs",
            ToolbarView::Table => "ca-view-table/tests/toolbar_clicks.rs",
            ToolbarView::Picture => "ca-view-picture/tests/toolbar_clicks.rs",
            ToolbarView::Merge => "ca-view-merge/tests/toolbar_clicks.rs",
            ToolbarView::Records => "ca-view-records/tests/toolbar_clicks.rs",
        };
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join(file);
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let named = format!("ToolbarView::{view:?}");
        assert!(
            source.contains(&named) && source.contains("every_command_reports("),
            "{file} does not press every command of {named}"
        );
    }
}

#[test]
fn the_shared_button_reports_one_press_and_a_disabled_one_reports_none() {
    for (enabled, expected) in [(true, 1), (false, 0)] {
        let pressed = Cell::new(0);
        let mut probe = Probe::new(800.0, 300.0);
        let mut draw = |ctx: &egui::Context| {
            egui::CentralPanel::default().show(ctx, |ui| {
                if widgets::toolbar_button(ui, "Apply", enabled, "Nothing to apply") {
                    pressed.set(pressed.get() + 1);
                }
            });
        };
        probe.idle(&mut draw);
        assert_eq!(probe.find("Apply").unwrap().enabled, enabled);
        probe.click("Apply", &mut draw).unwrap();
        assert_eq!(pressed.get(), expected, "enabled: {enabled}");
    }
}

fn dialog(path: &std::path::Path) -> ReportDialog {
    let mut settings = ReportSettings::new(ReportKind::Text);
    settings.target = Target::File;
    settings.path = path.display().to_string();
    ReportDialog::new(egui::Id::new("report"), settings, Arc::new(|| {}))
}

#[test]
fn the_report_write_button_asks_for_the_write() {
    let folder = tempfile::tempdir().unwrap();
    let mut report = dialog(&folder.path().join("report.html"));
    let mut actions = Vec::new();
    let mut probe = Probe::new(1_280.0, 900.0);
    {
        let mut draw = |ctx: &egui::Context| {
            egui::CentralPanel::default().show(ctx, |ui| {
                actions.push(report.ui(ui));
            });
        };
        probe.idle(&mut draw);
        probe.idle(&mut draw);
        probe.click("Write", &mut draw).unwrap();
    }
    let writes = actions
        .iter()
        .filter(|action| **action == ReportAction::Write)
        .count();
    assert_eq!(writes, 1, "the dialog returned {actions:?}");
}

#[test]
fn the_report_stop_button_stops_the_running_write() {
    let folder = tempfile::tempdir().unwrap();
    let mut report = dialog(&folder.path().join("report.html"));
    report.adopt(Job::spawn(|_: &Emitter<ReportMessage>, cancel: &Cancel| {
        while !cancel.is_cancelled() {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }));
    assert!(report.is_running());
    let mut probe = Probe::new(1_280.0, 900.0);
    {
        let mut draw = |ctx: &egui::Context| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = report.ui(ui);
            });
        };
        probe.idle(&mut draw);
        assert!(probe.find("Stop").unwrap().enabled);
        assert!(!probe.find("Write").unwrap().enabled);
        probe.click("Stop", &mut draw).unwrap();
    }
    assert!(!report.is_running());
    assert_eq!(report.status(), Some("The report was stopped."));
}

#[test]
fn the_report_stop_button_is_disabled_while_nothing_runs() {
    let folder = tempfile::tempdir().unwrap();
    let mut report = dialog(&folder.path().join("report.html"));
    let mut probe = Probe::new(1_280.0, 900.0);
    let mut draw = |ctx: &egui::Context| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let _ = report.ui(ui);
        });
    };
    probe.idle(&mut draw);
    assert!(!probe.find("Stop").unwrap().enabled);
}

#[test]
fn the_save_workspace_button_saves_under_the_typed_name() {
    let store = SessionStore::default();
    for (name, expected) in [
        (
            "  Evening  ",
            Some(WorkspaceAction::Save("Evening".to_owned())),
        ),
        ("", None),
    ] {
        let mut manager = WorkspaceManager::with_name(7, name);
        let mut actions = Vec::new();
        let mut probe = Probe::new(1_280.0, 900.0);
        {
            let mut draw = |ctx: &egui::Context| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    actions.extend(manager.body(ui, &store));
                });
            };
            probe.idle(&mut draw);
            assert_eq!(
                probe.find("Save Workspace").unwrap().enabled,
                expected.is_some()
            );
            probe.click("Save Workspace", &mut draw).unwrap();
        }
        assert_eq!(
            actions,
            expected.into_iter().collect::<Vec<_>>(),
            "{name:?}"
        );
    }
}

#[test]
fn the_probe_reads_labels_from_the_accessibility_tree() {
    let mut probe = Probe::new(400.0, 200.0);
    let mut draw = |ctx: &egui::Context| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let _ = ui.button("Plain");
        });
    };
    probe.idle(&mut draw);
    let control = probe.find("Plain").unwrap();
    assert!(control.is_pressable());
    assert!(probe::region_of(probe.controls(), &["Plain"]).is_some());
}
