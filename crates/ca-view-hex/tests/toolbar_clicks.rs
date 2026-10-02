//! Every hex toolbar control, pressed through pointer input, does what it
//! does today.
//!
//! Controls are found by their accessible label. A command item is checked on
//! the shared bar, where it has to report its own command; a control the view
//! draws into a slot is checked on the whole view, where its effect has to be
//! visible in the view's state or in the actions it returns.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

use ca_session::SessionKind;
use ca_ui::command::Command;
use ca_ui::testing::context;
use ca_ui::testing::probe::{
    self, all_enabled, every_command_reports, press_view, settle, toggle_view, Expected, Probe,
    Reach, NARROW_ROOM, WIDE_ROOM,
};
use ca_ui::toolbar::{self, Item, ToolbarView};
use ca_ui::view::{OpenRequest, SessionView, ViewAction};
use ca_view_hex::HexView;
use std::path::Path;
use std::time::{Duration, Instant};

/// A window wide enough to hold the whole toolbar on one line.
const WINDOW: [f32; 2] = [2_600.0, 900.0];

/// Each command item of the hex toolbar: name, label, and the command it runs.
const COMMANDS: &[Expected] = &[
    (
        "previous-section",
        "Previous Difference Section",
        Command::PreviousSection,
    ),
    (
        "previous-difference",
        "Previous Difference",
        Command::PreviousDifference,
    ),
    (
        "next-difference",
        "Next Difference",
        Command::NextDifference,
    ),
    (
        "next-section",
        "Next Difference Section",
        Command::NextSection,
    ),
    ("copy-left", "Copy to Left", Command::CopyToLeft),
    ("copy-right", "Copy to Right", Command::CopyToRight),
    ("swap", "Swap Sides", Command::SwapSides),
    ("reload", "Reload Files", Command::Reload),
    ("report", "Report", Command::CompareReport),
];

/// What each slot of the hex toolbar draws while no comparison runs.
///
/// A slot with pressable controls names their labels; a slot that holds only
/// a drop down or a number field names none. The font slot adds a Cancel
/// button while a comparison runs, which a frame of a finished view does not
/// show.
const SLOTS: &[(&str, &[&str])] = &[
    ("filter", &[]),
    ("alignment", &[]),
    ("encoding", &[]),
    ("width", &["Fit row"]),
    ("hex-addresses", &["Hex addresses"]),
    ("addresses", &["Addresses"]),
    ("thumbnail", &["Thumbnail"]),
    ("file-info", &["File info"]),
    ("text-compare", &["Text Compare"]),
    ("parent-folders", &["Parent Folders"]),
    ("font", &["A-", "A+"]),
];

fn tick_until(view: &mut HexView, ready: impl Fn(&HexView) -> bool) -> bool {
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

/// A finished comparison of two files in their own folders.
fn ready_view(dir: &Path) -> HexView {
    for side in ["left", "right"] {
        std::fs::create_dir_all(dir.join(side)).unwrap();
    }
    let left = dir.join("left").join("a.bin");
    let right = dir.join("right").join("a.bin");
    std::fs::write(&left, [0_u8, 1, 2, 3, 4, 5, 6, 7, 8, 9]).unwrap();
    std::fs::write(&right, [0_u8, 1, 2, 9, 4, 5, 6, 7, 8, 9]).unwrap();
    let mut view = HexView::new(left, right, &context(), 1);
    assert!(tick_until(&mut view, SessionView::is_ready));
    view
}

fn probe_over(view: &mut HexView) -> Probe {
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
fn every_hex_toolbar_command_reports_itself() {
    let dir = tempfile::tempdir().unwrap();
    let items = all_enabled(&ready_view(dir.path()).toolbar_items());
    every_command_reports(&items, COMMANDS, WIDE_ROOM, Reach::Bar).unwrap();
    every_command_reports(&items, COMMANDS, NARROW_ROOM, Reach::Overflow).unwrap();
}

#[test]
fn the_hex_toolbar_draws_the_controls_the_options_page_lists() {
    let dir = tempfile::tempdir().unwrap();
    let drawn: Vec<&str> = ready_view(dir.path())
        .toolbar_items()
        .iter()
        .map(Item::name)
        .collect();
    let listed: Vec<&str> = toolbar::defaults(ToolbarView::Hex)
        .iter()
        .map(|item| item.name)
        .collect();
    assert_eq!(drawn, listed);
}

fn toggle(label: &str, read: impl Fn(&HexView) -> bool) -> Result<(), String> {
    let dir = tempfile::tempdir().unwrap();
    let mut view = ready_view(dir.path());
    let mut probe = probe_over(&mut view);
    toggle_view(&mut probe, &mut view, label, read)
}

fn opens(label: &str, expected: impl Fn(&Path) -> OpenRequest) -> Result<(), String> {
    let dir = tempfile::tempdir().unwrap();
    let mut view = ready_view(dir.path());
    let mut probe = probe_over(&mut view);
    let actions = press_view(&mut probe, &mut view, label)?;
    let wanted = ViewAction::Open(expected(dir.path()));
    (actions == [wanted.clone()])
        .then_some(())
        .ok_or_else(|| format!("{label} asked for {actions:?}, not {wanted:?}"))
}

fn font() -> Result<(), String> {
    let dir = tempfile::tempdir().unwrap();
    let mut view = ready_view(dir.path());
    let mut probe = probe_over(&mut view);
    let start = view.font_size();
    press_view(&mut probe, &mut view, "A+")?;
    let larger = view.font_size();
    if larger <= start {
        return Err(format!("A+ left the font at {larger} from {start}"));
    }
    press_view(&mut probe, &mut view, "A-")?;
    let smaller = view.font_size();
    (smaller < larger)
        .then_some(())
        .ok_or_else(|| format!("A- left the font at {smaller} from {larger}"))
}

fn check_slot(slot: &str) -> Result<(), String> {
    match slot {
        "width" => toggle("Fit row", HexView::fits_row),
        "hex-addresses" => toggle("Hex addresses", HexView::hex_addresses),
        "addresses" => toggle("Addresses", HexView::shows_addresses),
        "thumbnail" => toggle("Thumbnail", HexView::shows_thumbnail),
        "file-info" => toggle("File info", HexView::shows_file_info),
        "text-compare" => opens("Text Compare", |dir| {
            OpenRequest::new(
                SessionKind::TextCompare,
                dir.join("left").join("a.bin"),
                dir.join("right").join("a.bin"),
            )
        }),
        "parent-folders" => opens("Parent Folders", |dir| {
            OpenRequest::new(
                SessionKind::FolderCompare,
                dir.join("left"),
                dir.join("right"),
            )
        }),
        "font" => font(),
        // Holds no pressable control, which the untested button test enforces.
        "filter" | "alignment" | "encoding" => Ok(()),
        other => Err(format!("{other} has no click test")),
    }
}

#[test]
fn every_hex_toolbar_slot_is_pressed_and_acts() {
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
fn the_hex_toolbar_holds_no_untested_button() {
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
