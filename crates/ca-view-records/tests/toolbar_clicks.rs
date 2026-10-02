//! Every registry, version and media toolbar control, pressed through
//! pointer input, does what it does today.
//!
//! Controls are found by their accessible label. A command item is checked on
//! the shared bar, where it has to report its own command; a control the view
//! draws into a slot is checked on the whole view, where its effect has to be
//! visible in the view's state.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

use ca_ui::command::Command;
use ca_ui::testing::context;
use ca_ui::testing::probe::{
    self, all_enabled, every_command_reports, settle, toggle_view, Expected, Probe, Reach,
    NARROW_ROOM, WIDE_ROOM,
};
use ca_ui::toolbar::{self, Item, ToolbarView};
use ca_ui::view::SessionView;
use ca_view_records::{Flavor, RecordsView};
use std::path::Path;
use std::time::{Duration, Instant};

/// A window wide enough to hold the whole toolbar on one line.
const WINDOW: [f32; 2] = [2_400.0, 900.0];

/// Each command item of the records toolbar: name, label, and the command it runs.
const COMMANDS: &[Expected] = &[
    (
        "previous-difference",
        "Previous",
        Command::PreviousDifference,
    ),
    ("next-difference", "Next", Command::NextDifference),
    ("expand", "Expand", Command::ExpandAll),
    ("collapse", "Collapse", Command::CollapseAll),
    ("reload", "Reload", Command::Reload),
    ("recompare", "Recompare", Command::Recompare),
    ("swap", "Swap", Command::SwapSides),
    ("stop", "Stop", Command::Cancel),
    ("report", "Report", Command::CompareReport),
];

/// What each slot of the records toolbar draws.
///
/// A slot with pressable controls names their labels; a slot that holds only
/// a drop down names none.
const SLOTS: &[(&str, &[&str])] = &[
    ("filter", &[]),
    ("minor", &["Minor"]),
    ("strip", &["Thumbnail"]),
    ("details", &["Details"]),
    ("hex", &["Hex details"]),
];

fn tick_until(view: &mut RecordsView, ready: impl Fn(&RecordsView) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(30);
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

/// A finished comparison of two registry export files.
fn registry_view(dir: &Path) -> RecordsView {
    let header =
        "Windows Registry Editor Version 5.00\r\n\r\n[HKEY_CURRENT_USER\\Software\\Probe]\r\n";
    let left = dir.join("left.reg");
    let right = dir.join("right.reg");
    std::fs::write(&left, format!("{header}\"Name\"=\"one\"\r\n")).unwrap();
    std::fs::write(&right, format!("{header}\"Name\"=\"two\"\r\n")).unwrap();
    let mut view = RecordsView::new(Flavor::Registry, left, right, &context(), 1);
    assert!(tick_until(&mut view, RecordsView::has_comparison));
    view
}

/// A version comparison, whose flavor weighs differences by importance.
fn version_view(dir: &Path) -> RecordsView {
    let left = dir.join("left.dll");
    let right = dir.join("right.dll");
    std::fs::write(&left, b"not a binary").unwrap();
    std::fs::write(&right, b"not a binary either").unwrap();
    let mut view = RecordsView::new(Flavor::Version, left, right, &context(), 2);
    assert!(tick_until(&mut view, |view| !view.is_busy()));
    view
}

fn probe_over(view: &mut RecordsView) -> Probe {
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
fn every_records_toolbar_command_reports_itself() {
    let dir = tempfile::tempdir().unwrap();
    let items = all_enabled(&registry_view(dir.path()).toolbar_items());
    every_command_reports(&items, COMMANDS, WIDE_ROOM, Reach::Bar).unwrap();
    every_command_reports(&items, COMMANDS, NARROW_ROOM, Reach::Overflow).unwrap();
}

#[test]
fn the_records_toolbar_draws_the_controls_the_options_page_lists() {
    let dir = tempfile::tempdir().unwrap();
    let drawn: Vec<&str> = registry_view(dir.path())
        .toolbar_items()
        .iter()
        .map(Item::name)
        .collect();
    let listed: Vec<&str> = toolbar::defaults(ToolbarView::Records)
        .iter()
        .map(|item| item.name)
        .collect();
    assert_eq!(drawn, listed);
}

fn toggle(
    open: fn(&Path) -> RecordsView,
    label: &str,
    read: impl Fn(&RecordsView) -> bool,
) -> Result<(), String> {
    let dir = tempfile::tempdir().unwrap();
    let mut view = open(dir.path());
    let mut probe = probe_over(&mut view);
    toggle_view(&mut probe, &mut view, label, read)
}

/// Minor is drawn disabled where the flavor weighs nothing by importance.
fn minor_is_off_for_the_registry() -> Result<(), String> {
    let dir = tempfile::tempdir().unwrap();
    let mut view = registry_view(dir.path());
    let probe = probe_over(&mut view);
    (!probe.find("Minor")?.enabled)
        .then_some(())
        .ok_or_else(|| "Minor is enabled for the registry".to_owned())
}

fn check_slot(slot: &str) -> Result<(), String> {
    match slot {
        "minor" => {
            minor_is_off_for_the_registry()?;
            toggle(version_view, "Minor", |view| {
                view.listing().ignores_unimportant()
            })
        }
        "strip" => toggle(registry_view, "Thumbnail", RecordsView::shows_strip),
        "details" => toggle(registry_view, "Details", RecordsView::shows_details),
        "hex" => toggle(registry_view, "Hex details", RecordsView::shows_hex),
        // Holds no pressable control, which the untested button test enforces.
        "filter" => Ok(()),
        other => Err(format!("{other} has no click test")),
    }
}

#[test]
fn every_records_toolbar_slot_is_pressed_and_acts() {
    let dir = tempfile::tempdir().unwrap();
    let items = registry_view(dir.path()).toolbar_items();
    let listed: Vec<&str> = SLOTS.iter().map(|(slot, _)| *slot).collect();
    assert_eq!(slots(&items), listed, "the slot table is out of date");
    let failures: Vec<String> = slots(&items)
        .into_iter()
        .filter_map(|slot| {
            check_slot(slot)
                .err()
                .map(|error| format!("{slot}: {error}"))
        })
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn the_records_toolbar_holds_no_untested_button() {
    let dir = tempfile::tempdir().unwrap();
    let mut view = registry_view(dir.path());
    let items = view.toolbar_items();
    let labels: Vec<&str> = SLOTS
        .iter()
        .flat_map(|(_, labels)| labels.iter().copied())
        .collect();
    let probe = probe_over(&mut view);
    let strangers = probe::toolbar_strangers(&probe, &items, &labels).unwrap();
    assert!(
        strangers.is_empty(),
        "toolbar controls with no click test: {strangers:?}"
    );
}
