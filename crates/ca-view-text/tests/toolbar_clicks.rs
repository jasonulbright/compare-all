//! Every text toolbar control, pressed through pointer input, does what it
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
    self, all_enabled, every_command_reports, Expected, Probe, Reach, NARROW_ROOM, WIDE_ROOM,
};
use ca_ui::toolbar::{self, Item, ToolbarView};
use ca_ui::view::{SessionView, ViewAction};
use ca_view_text::model::DisplayFilter;
use ca_view_text::TextView;
use std::mem::discriminant;
use std::path::Path;
use std::time::{Duration, Instant};

/// A window wide enough to hold the whole toolbar on one line.
const WINDOW: [f32; 2] = [2_400.0, 900.0];

#[test]
fn gutter_copy_icons_copy_their_section_in_both_directions() {
    for (label, from, to) in [
        (
            "Copy to Right",
            ca_view_text::sidecopy::Side::Left,
            ca_view_text::sidecopy::Side::Right,
        ),
        (
            "Copy to Left",
            ca_view_text::sidecopy::Side::Right,
            ca_view_text::sidecopy::Side::Left,
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut view = ready_view(dir.path());
        let mut probe = probe_over(&mut view);
        let expected = view.pane(from).line_text(1);
        press(&mut probe, &mut view, label);
        assert_eq!(view.pane(to).line_text(1), expected);
    }
}

#[test]
fn text_toolbar_items_follow_the_pinned_order() {
    let dir = tempfile::tempdir().unwrap();
    let items = ready_view(dir.path()).toolbar_items();
    let actual: Vec<_> = items.iter().map(Item::name).collect();
    let expected: Vec<_> = toolbar::defaults(ToolbarView::Text)
        .iter()
        .map(|item| item.name)
        .collect();
    assert_eq!(actual, expected);
}

/// Each command item of the text toolbar: name, label, and the command it runs.
const COMMANDS: &[Expected] = &[
    ("copy", "Copy", Command::CopyToOtherSide),
    ("next-section", "Next Section", Command::NextSection),
    ("previous-section", "Prev Section", Command::PreviousSection),
    ("swap", "Swap", Command::SwapSides),
    ("reload", "Reload", Command::Reload),
    ("report", "Report", Command::CompareReport),
];

/// What each slot of the text toolbar draws while no comparison runs.
///
/// A slot with pressable controls names their labels; a slot that holds only
/// a field names none. The format slot adds a Cancel button while a comparison
/// runs, which a frame of a finished view does not show.
const SLOTS: &[(&str, &[&str])] = &[
    ("home", &["Home"]),
    ("sessions", &["Sessions"]),
    ("all", &["All"]),
    ("diffs", &["Diffs"]),
    ("same", &["Same"]),
    ("context", &["Context"]),
    ("minor", &["Minor"]),
    ("rules", &["Rules"]),
    ("format", &["Format"]),
];

fn tick_until(view: &mut TextView, ready: impl Fn(&TextView) -> bool) -> bool {
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

/// A finished comparison of two short files that differ in one line.
fn ready_view(dir: &Path) -> TextView {
    let left = dir.join("left.txt");
    let right = dir.join("right.txt");
    std::fs::write(&left, "one\ntwo\nthree\nfour\n").unwrap();
    std::fs::write(&right, "one\nTWO\nthree\nfour\n").unwrap();
    let mut view = TextView::new(left, right, &context(), 1);
    assert!(tick_until(&mut view, SessionView::is_ready));
    view
}

fn frame<'a>(
    view: &'a mut TextView,
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

fn probe_over(view: &mut TextView) -> Probe {
    let mut probe = Probe::new(WINDOW[0], WINDOW[1]);
    let mut actions = Vec::new();
    let mut draw = frame(view, &mut actions);
    probe.idle(&mut draw);
    probe.idle(&mut draw);
    probe
}

fn press(probe: &mut Probe, view: &mut TextView, label: &str) -> Vec<ViewAction> {
    let mut actions = Vec::new();
    {
        let mut draw = frame(view, &mut actions);
        probe.idle(&mut draw);
        probe.click(label, &mut draw).unwrap();
    }
    actions
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
fn every_text_toolbar_command_reports_itself() {
    let dir = tempfile::tempdir().unwrap();
    let items = all_enabled(&ready_view(dir.path()).toolbar_items());
    every_command_reports(&items, COMMANDS, WIDE_ROOM, Reach::Bar).unwrap();
    every_command_reports(&items, COMMANDS, NARROW_ROOM, Reach::Overflow).unwrap();
}

#[test]
fn the_text_toolbar_draws_the_controls_the_options_page_lists() {
    let dir = tempfile::tempdir().unwrap();
    let drawn: Vec<&str> = ready_view(dir.path())
        .toolbar_items()
        .iter()
        .filter(|item| !matches!(item, Item::Separator { .. }))
        .map(Item::name)
        .collect();
    let listed: Vec<&str> = toolbar::defaults(ToolbarView::Text)
        .iter()
        .filter(|item| item.label != toolbar::SEPARATOR_LABEL)
        .map(|item| item.name)
        .collect();
    assert_eq!(drawn, listed);
}

fn pending(label: &str) -> Result<(), String> {
    let dir = tempfile::tempdir().unwrap();
    let mut view = ready_view(dir.path());
    let mut probe = probe_over(&mut view);
    if probe.find(label)?.enabled {
        return Err(format!("{label} is enabled but does nothing yet"));
    }
    let filter = view.filter();
    let actions = press(&mut probe, &mut view, label);
    if !actions.is_empty() || view.filter() != filter {
        return Err(format!("the disabled {label} acted"));
    }
    Ok(())
}

fn home() -> Result<(), String> {
    let dir = tempfile::tempdir().unwrap();
    let mut view = ready_view(dir.path());
    let mut probe = probe_over(&mut view);
    let actions = press(&mut probe, &mut view, "Home");
    let homes = actions
        .iter()
        .filter(|action| matches!(action, ViewAction::OpenHome))
        .count();
    (homes == 1)
        .then_some(())
        .ok_or_else(|| format!("Home asked for {homes} home tabs"))
}

fn filter_button(label: &str, shown: DisplayFilter) -> Result<(), String> {
    let dir = tempfile::tempdir().unwrap();
    let mut view = ready_view(dir.path());
    let mut probe = probe_over(&mut view);
    let start = if label == "All" { "Diffs" } else { "All" };
    press(&mut probe, &mut view, start);
    if discriminant(&view.filter()) == discriminant(&shown) {
        return Err(format!("{start} already shows what {label} shows"));
    }
    press(&mut probe, &mut view, label);
    (discriminant(&view.filter()) == discriminant(&shown))
        .then_some(())
        .ok_or_else(|| format!("{label} left the filter at {:?}", view.filter()))
}

fn minor() -> Result<(), String> {
    let dir = tempfile::tempdir().unwrap();
    let mut view = ready_view(dir.path());
    let mut probe = probe_over(&mut view);
    let before = view.ignores_unimportant();
    if probe.find("Minor")?.toggled != Some(before) {
        return Err("Minor does not announce the rule in force".to_owned());
    }
    press(&mut probe, &mut view, "Minor");
    if view.ignores_unimportant() == before {
        return Err("Minor did not change the rule".to_owned());
    }
    press(&mut probe, &mut view, "Minor");
    (view.ignores_unimportant() == before)
        .then_some(())
        .ok_or_else(|| "a second press of Minor did not restore the rule".to_owned())
}

fn rules() -> Result<(), String> {
    let dir = tempfile::tempdir().unwrap();
    let mut view = ready_view(dir.path());
    let mut probe = probe_over(&mut view);
    let before = view.rules().case_unimportant;
    press(&mut probe, &mut view, "Rules");
    press(&mut probe, &mut view, "Case unimportant");
    (view.rules().case_unimportant != before)
        .then_some(())
        .ok_or_else(|| "the Rules menu did not change the case rule".to_owned())
}

fn check_slot(slot: &str) -> Result<(), String> {
    match slot {
        "home" => home(),
        "sessions" => pending("Sessions"),
        "format" => pending("Format"),
        "all" => filter_button("All", DisplayFilter::All),
        "diffs" => filter_button("Diffs", DisplayFilter::Differences),
        "same" => filter_button("Same", DisplayFilter::Same),
        "context" => filter_button("Context", DisplayFilter::Context(0)),
        "minor" => minor(),
        "rules" => rules(),
        other => Err(format!("{other} has no click test")),
    }
}

#[test]
fn every_text_toolbar_slot_is_pressed_and_acts() {
    let dir = tempfile::tempdir().unwrap();
    let items = ready_view(dir.path()).toolbar_items();
    let listed: Vec<&str> = SLOTS.iter().map(|(slot, _)| *slot).collect();
    assert_eq!(slots(&items), listed, "the slot table is out of date");
    let failures: Vec<String> = slots(&items)
        .into_iter()
        .filter_map(|slot| check_slot(slot).err())
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn the_text_toolbar_holds_no_untested_button() {
    let dir = tempfile::tempdir().unwrap();
    let mut view = ready_view(dir.path());
    let items = view.toolbar_items();
    let mut known: Vec<&str> = probe::command_items(&items)
        .into_iter()
        .map(|(_, label, _)| label)
        .collect();
    known.extend(SLOTS.iter().flat_map(|(_, labels)| labels.iter()));
    let probe = probe_over(&mut view);
    let region = probe::region_of(probe.controls(), &known).expect("the toolbar was drawn");
    let strangers = probe::strangers(probe.controls(), &known, region);
    assert!(
        strangers.is_empty(),
        "toolbar controls with no click test: {strangers:?}"
    );
}
