//! Every merge toolbar control, pressed through pointer input, does what it
//! does today.
//!
//! Controls are found by their accessible label. A command item is checked on
//! the shared bar, where it has to report its own command, and each toggle is
//! also pressed on the whole view, where its rule has to change.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

use ca_ui::command::Command;
use ca_ui::testing::context;
use ca_ui::testing::probe::{
    self, all_enabled, every_command_reports, settle, toggle_view, Expected, Probe, Reach,
    NARROW_ROOM, WIDE_ROOM,
};
use ca_ui::toolbar::{self, Item, ToolbarView};
use ca_ui::view::{SessionView, Titles};
use ca_view_merge::jobs::MergePaths;
use ca_view_merge::MergeView;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A window wide enough to hold the whole toolbar on one line.
const WINDOW: [f32; 2] = [2_600.0, 900.0];

#[test]
fn gutter_take_icons_select_the_correct_output_source() {
    for (label, expected) in [
        ("Take Left into Output", "LEFT"),
        ("Take Right into Output", "RIGHT"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut view = ready_view(dir.path());
        let mut probe = probe_over(&mut view);
        ca_ui::testing::probe::press_view(&mut probe, &mut view, label).unwrap();
        assert_eq!(view.output_text(), format!("one\n{expected}\nthree\n"));
    }
}

/// Each command item of the merge toolbar: name, label, and the command it runs.
const COMMANDS: &[Expected] = &[
    (
        "previous-conflict",
        "Prev Conflict",
        Command::PreviousConflict,
    ),
    ("next-conflict", "Next Conflict", Command::NextConflict),
    ("take-left", "Take Left", Command::TakeLeft),
    ("take-center", "Take Center", Command::TakeCenter),
    ("take-right", "Take Right", Command::TakeRight),
    ("take-both", "Take Both", Command::TakeLeftThenRight),
    ("take-all", "Take All", Command::TakeAllNonConflicting),
    ("favor-left", "Favor Left", Command::FavorLeft),
    ("favor-right", "Favor Right", Command::FavorRight),
    (
        "ignore-same",
        "Ignore Same",
        Command::ToggleIgnoreSameChanges,
    ),
    ("center-pane", "Center Pane", Command::ToggleCenterPane),
    ("save", "Save", Command::SaveFile),
    ("reload", "Reload", Command::Reload),
    ("report", "Report", Command::CompareReport),
    ("minor", "Minor", Command::ToggleIgnoreUnimportant),
];

/// What each slot of the merge toolbar draws.
const SLOTS: &[(&str, &[&str])] = &[("filter", &[])];

fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, text.as_bytes()).unwrap();
    path
}

/// A finished three way merge with one conflict.
fn ready_view(dir: &Path) -> MergeView {
    let paths = MergePaths {
        left: write(dir, "left.txt", "one\nLEFT\nthree\n"),
        center: Some(write(dir, "center.txt", "one\ntwo\nthree\n")),
        right: write(dir, "right.txt", "one\nRIGHT\nthree\n"),
        output: Some(dir.join("merged.txt")),
    };
    let mut view = MergeView::over(paths, Titles::default(), &context(), 3);
    let deadline = Instant::now() + Duration::from_secs(30);
    while !view.is_ready() {
        assert!(Instant::now() < deadline, "the merge never finished");
        view.tick();
        std::thread::sleep(Duration::from_millis(2));
    }
    view
}

fn probe_over(view: &mut MergeView) -> Probe {
    let mut probe = Probe::new(WINDOW[0], WINDOW[1]);
    settle(&mut probe, view);
    probe
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
fn every_merge_toolbar_command_reports_itself() {
    let dir = tempfile::tempdir().unwrap();
    let items = all_enabled(&ready_view(dir.path()).toolbar_items());
    every_command_reports(&items, COMMANDS, WIDE_ROOM, Reach::Bar).unwrap();
    every_command_reports(&items, COMMANDS, NARROW_ROOM, Reach::Overflow).unwrap();
}

#[test]
fn the_merge_toolbar_draws_the_controls_the_options_page_lists() {
    let dir = tempfile::tempdir().unwrap();
    let drawn: Vec<&str> = ready_view(dir.path())
        .toolbar_items()
        .iter()
        .map(Item::name)
        .collect();
    let listed: Vec<&str> = toolbar::defaults(ToolbarView::Merge)
        .iter()
        .map(|item| item.name)
        .collect();
    assert_eq!(drawn, listed);
}

#[test]
fn every_merge_toolbar_slot_is_listed() {
    let dir = tempfile::tempdir().unwrap();
    let items = ready_view(dir.path()).toolbar_items();
    let listed: Vec<&str> = SLOTS.iter().map(|(slot, _)| *slot).collect();
    assert_eq!(slots(&items), listed, "the slot table is out of date");
}

#[test]
fn every_merge_toolbar_toggle_changes_its_rule() {
    type Read = fn(&MergeView) -> bool;
    let toggles: [(&str, Read); 5] = [
        ("Favor Left", |view| view.rules().favor_left),
        ("Favor Right", |view| view.rules().favor_right),
        ("Ignore Same", |view| view.rules().ignore_same_changes),
        ("Center Pane", MergeView::shows_center),
        ("Minor", |view| view.rules().ignore_unimportant),
    ];
    let dir = tempfile::tempdir().unwrap();
    let items = ready_view(dir.path()).toolbar_items();
    let declared: Vec<&str> = items
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
        let dir = tempfile::tempdir().unwrap();
        let mut view = ready_view(dir.path());
        let mut probe = probe_over(&mut view);
        if let Err(error) = toggle_view(&mut probe, &mut view, label, read) {
            failures.push(error);
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn the_merge_toolbar_holds_no_untested_button() {
    let dir = tempfile::tempdir().unwrap();
    let mut view = ready_view(dir.path());
    let items = view.toolbar_items();
    let probe = probe_over(&mut view);
    let strangers = probe::toolbar_strangers(&probe, &items, &[]).unwrap();
    assert!(
        strangers.is_empty(),
        "toolbar controls with no click test: {strangers:?}"
    );
}
