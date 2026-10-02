//! Every table toolbar control, pressed through pointer input, does what it
//! does today.
//!
//! Controls are found by their accessible label. A command item is checked on
//! the shared bar, where it has to report its own command; a control the view
//! draws into a slot is checked on the whole view, where its effect has to be
//! visible in the view's state.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

use ca_ui::command::Command;
use ca_ui::testing::context;
use ca_ui::testing::probe::{
    self, all_enabled, every_command_reports, press_view, settle, toggle_view, Expected, Probe,
    Reach, NARROW_ROOM, WIDE_ROOM,
};
use ca_ui::toolbar::{self, Item, ToolbarView};
use ca_ui::view::SessionView;
use ca_view_table::TableView;
use std::path::Path;
use std::time::{Duration, Instant};

/// A window wide enough to hold the whole toolbar on one line.
const WINDOW: [f32; 2] = [2_400.0, 900.0];

/// Each command item of the table toolbar: name, label, and the command it runs.
const COMMANDS: &[Expected] = &[
    (
        "previous-difference",
        "Previous",
        Command::PreviousDifference,
    ),
    ("next-difference", "Next", Command::NextDifference),
    ("settings", "Settings", Command::SessionSettings),
    ("recompare", "Recompare", Command::Recompare),
    ("swap", "Swap", Command::SwapSides),
    ("stop", "Stop", Command::Cancel),
    ("report", "Report", Command::CompareReport),
];

/// The column the unhide test hides.
const HIDDEN: &str = "size";

/// What each slot of the table toolbar draws.
///
/// A slot with pressable controls names their labels; a slot that holds only
/// a drop down names none.
const SLOTS: &[(&str, &[&str])] = &[
    ("filter", &[]),
    ("minor", &["Minor"]),
    ("hide-same", &["Hide same columns"]),
    ("unhide-column", &["Unhide Column"]),
    ("row-numbers", &["Row numbers"]),
    ("strip", &["Thumbnail"]),
    ("details", &["Details"]),
];

fn tick_until(view: &mut TableView, ready: impl Fn(&TableView) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(60);
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

/// A finished comparison of two short tables.
fn ready_view(dir: &Path) -> TableView {
    let left = dir.join("left.csv");
    let right = dir.join("right.csv");
    std::fs::write(&left, "name,size,note\nalpha,10,first\nbeta,20,second\n").unwrap();
    std::fs::write(&right, "name,size,note\nalpha,10,first\nbeta,21,second\n").unwrap();
    let mut view = TableView::new(left, right, &context(), 1);
    assert!(tick_until(&mut view, TableView::has_comparison));
    view
}

fn probe_over(view: &mut TableView) -> Probe {
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
fn every_table_toolbar_command_reports_itself() {
    let dir = tempfile::tempdir().unwrap();
    let items = all_enabled(&ready_view(dir.path()).toolbar_items());
    every_command_reports(&items, COMMANDS, WIDE_ROOM, Reach::Bar).unwrap();
    every_command_reports(&items, COMMANDS, NARROW_ROOM, Reach::Overflow).unwrap();
}

#[test]
fn the_table_toolbar_draws_the_controls_the_options_page_lists() {
    let dir = tempfile::tempdir().unwrap();
    let drawn: Vec<&str> = ready_view(dir.path())
        .toolbar_items()
        .iter()
        .map(Item::name)
        .collect();
    let listed: Vec<&str> = toolbar::defaults(ToolbarView::Table)
        .iter()
        .map(|item| item.name)
        .collect();
    assert_eq!(drawn, listed);
}

fn toggle(label: &str, read: impl Fn(&TableView) -> bool) -> Result<(), String> {
    let dir = tempfile::tempdir().unwrap();
    let mut view = ready_view(dir.path());
    let mut probe = probe_over(&mut view);
    toggle_view(&mut probe, &mut view, label, read)
}

fn hidden_columns(view: &TableView) -> Vec<String> {
    view.grid()
        .hidden_columns()
        .map(|(_, column)| column.name.clone())
        .collect()
}

fn unhide_column() -> Result<(), String> {
    let dir = tempfile::tempdir().unwrap();
    let mut view = ready_view(dir.path());
    let mut probe = probe_over(&mut view);
    if probe.find("Unhide Column")?.enabled {
        return Err("Unhide Column is enabled with no column hidden".to_owned());
    }
    let column = view
        .grid()
        .source()
        .columns()
        .iter()
        .position(|column| column.name == HIDDEN)
        .ok_or("the fixture has no size column")?;
    view.grid_mut().set_column_hidden(column, true);
    if hidden_columns(&view) != [HIDDEN] {
        return Err(format!("hiding left {:?} hidden", hidden_columns(&view)));
    }
    press_view(&mut probe, &mut view, "Unhide Column")?;
    press_view(&mut probe, &mut view, HIDDEN)?;
    hidden_columns(&view)
        .is_empty()
        .then_some(())
        .ok_or_else(|| format!("{:?} is still hidden", hidden_columns(&view)))
}

fn check_slot(slot: &str) -> Result<(), String> {
    match slot {
        "minor" => toggle("Minor", |view| view.grid().ignores_unimportant()),
        "hide-same" => toggle("Hide same columns", |view| view.grid().hides_same_columns()),
        "row-numbers" => toggle("Row numbers", TableView::shows_row_numbers),
        "strip" => toggle("Thumbnail", TableView::shows_strip),
        "details" => toggle("Details", TableView::shows_details),
        "unhide-column" => unhide_column(),
        // Holds no pressable control, which the untested button test enforces.
        "filter" => Ok(()),
        other => Err(format!("{other} has no click test")),
    }
}

#[test]
fn every_table_toolbar_slot_is_pressed_and_acts() {
    let dir = tempfile::tempdir().unwrap();
    let items = ready_view(dir.path()).toolbar_items();
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
fn the_table_toolbar_holds_no_untested_button() {
    let dir = tempfile::tempdir().unwrap();
    let mut view = ready_view(dir.path());
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
