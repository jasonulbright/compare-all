//! Every picture toolbar control, pressed through pointer input, does what it
//! does today, including the controls behind the toolbar's own More button.
//!
//! Controls are found by their accessible label. The command item is checked
//! on the shared bar, where it has to report its own command; a control the
//! view draws into a slot is checked on the whole view, where its effect has
//! to be visible in the view's state or in the actions it returns.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

use ca_image::compare::Side;
use ca_session::SessionKind;
use ca_ui::command::Command;
use ca_ui::testing::context;
use ca_ui::testing::probe::{
    self, all_enabled, every_command_reports, press_view, press_view_within, settle, toggle_view,
    Expected, Probe, Reach, NARROW_ROOM, WIDE_ROOM,
};
use ca_ui::toolbar::{self, Item, ToolbarView};
use ca_ui::view::{OpenRequest, SessionView, ViewAction};
use ca_view_picture::model::Pane;
use ca_view_picture::PictureView;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A window wide enough to hold the whole toolbar on one line.
const WINDOW: [f32; 2] = [2_400.0, 900.0];

/// Each command item of the picture toolbar: name, label, and the command it runs.
const COMMANDS: &[Expected] = &[("report", "Report", Command::CompareReport)];

/// The label of the view's own More button.
const MORE: &str = "More";

/// What each slot of the picture toolbar draws on the bar.
///
/// A slot with pressable controls names their labels; a slot that holds only
/// a drop down or a slider names none.
const SLOTS: &[(&str, &[&str])] = &[
    ("home", &["Home"]),
    ("mode", &[]),
    ("tolerance", &[]),
    ("zoom-in", &["Zoom in"]),
    ("zoom-out", &["Zoom out"]),
    ("actual-size", &["1:1"]),
    ("fit", &["Fit"]),
    ("panes", &["Left", "Right", "Difference"]),
    ("more", &[MORE]),
];

/// The pressable controls behind the More button.
const MORE_CONTROLS: &[&str] = &[
    "Blend toggle",
    "Minor",
    "Ignore alpha",
    "Transparent equal",
    "Auto scale",
    "Rotate right",
    "Rotate left",
    "Flip across y",
    "Flip across x",
    "Checkerboard",
    "Metadata",
    "Swap",
    "Reload",
    "Stop",
    "Compare as hex",
];

fn write_png(path: &Path, mark: bool) {
    let mut buffer = image::RgbaImage::from_pixel(400, 300, image::Rgba([10, 20, 30, 255]));
    if mark {
        buffer.put_pixel(3, 2, image::Rgba([255, 0, 0, 255]));
    }
    buffer.save(path).unwrap();
}

fn paths(dir: &Path) -> (PathBuf, PathBuf) {
    (dir.join("left.png"), dir.join("right.png"))
}

/// A finished comparison of two small pictures that differ in one pixel.
fn ready_view(dir: &Path) -> (PictureView, Probe) {
    let (left, right) = paths(dir);
    write_png(&left, true);
    write_png(&right, false);
    let mut view = PictureView::new(left, right, &context(), 1);
    let mut probe = Probe::new(WINDOW[0], WINDOW[1]);
    wait_until(&mut probe, &mut view, SessionView::is_ready);
    settle(&mut probe, &mut view);
    (view, probe)
}

/// Run frames until `ready` holds; the view reads its pictures between them.
fn wait_until(probe: &mut Probe, view: &mut PictureView, ready: impl Fn(&PictureView) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !ready(view) {
        assert!(Instant::now() < deadline, "the view never got there");
        settle(probe, view);
        std::thread::sleep(Duration::from_millis(2));
    }
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
fn every_picture_toolbar_command_reports_itself() {
    let dir = tempfile::tempdir().unwrap();
    let (view, _) = ready_view(dir.path());
    let items = all_enabled(&view.toolbar_items());
    every_command_reports(&items, COMMANDS, WIDE_ROOM, Reach::Bar).unwrap();
    every_command_reports(&items, COMMANDS, NARROW_ROOM, Reach::Overflow).unwrap();
}

#[test]
fn the_picture_toolbar_draws_the_controls_the_options_page_lists() {
    let dir = tempfile::tempdir().unwrap();
    let (view, _) = ready_view(dir.path());
    let drawn: Vec<&str> = view.toolbar_items().iter().map(Item::name).collect();
    let listed: Vec<&str> = toolbar::defaults(ToolbarView::Picture)
        .iter()
        .map(|item| item.name)
        .collect();
    assert_eq!(drawn, listed);
}

fn home() -> Result<(), String> {
    let dir = tempfile::tempdir().unwrap();
    let (mut view, mut probe) = ready_view(dir.path());
    let actions = press_view(&mut probe, &mut view, "Home")?;
    (actions == [ViewAction::OpenHome])
        .then_some(())
        .ok_or_else(|| format!("Home asked for {actions:?}"))
}

fn zoom() -> Result<(), String> {
    let dir = tempfile::tempdir().unwrap();
    let (mut view, mut probe) = ready_view(dir.path());
    let start = view.camera().zoom;
    press_view(&mut probe, &mut view, "Zoom in")?;
    let closer = view.camera().zoom;
    if closer <= start {
        return Err(format!("Zoom in went from {start} to {closer}"));
    }
    if view.fits_to_panes() {
        return Err("Zoom in kept following the pane size".to_owned());
    }
    press_view(&mut probe, &mut view, "Zoom out")?;
    let further = view.camera().zoom;
    if further >= closer {
        return Err(format!("Zoom out went from {closer} to {further}"));
    }
    Ok(())
}

fn actual_size() -> Result<(), String> {
    let dir = tempfile::tempdir().unwrap();
    let (mut view, mut probe) = ready_view(dir.path());
    view.zoom_in();
    view.zoom_in();
    press_view(&mut probe, &mut view, "1:1")?;
    let zoom = view.camera().zoom;
    ((zoom - 1.0).abs() < f32::EPSILON && !view.fits_to_panes())
        .then_some(())
        .ok_or_else(|| format!("1:1 left the zoom at {zoom}"))
}

fn fit() -> Result<(), String> {
    let dir = tempfile::tempdir().unwrap();
    let (mut view, mut probe) = ready_view(dir.path());
    view.zoom_in();
    if view.fits_to_panes() {
        return Err("the zoom kept following the pane size".to_owned());
    }
    press_view(&mut probe, &mut view, "Fit")?;
    view.fits_to_panes()
        .then_some(())
        .ok_or_else(|| "Fit did not follow the pane size".to_owned())
}

fn panes() -> Result<(), String> {
    let mut failures = Vec::new();
    for pane in Pane::ALL {
        let dir = tempfile::tempdir().unwrap();
        let (mut view, mut probe) = ready_view(dir.path());
        if let Err(error) = toggle_view(&mut probe, &mut view, pane.label(), move |view| {
            view.panes().shows(pane)
        }) {
            failures.push(error);
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

/// Open the More menu and return the rectangle around what it holds.
fn open_more(probe: &mut Probe, view: &mut PictureView) -> Result<egui::Rect, String> {
    press_view(probe, view, MORE)?;
    probe::region_of(probe.controls(), MORE_CONTROLS)
        .ok_or_else(|| "the More menu did not open".to_owned())
}

/// Open the More menu and press `label` in it.
fn press_more(label: &str) -> Result<(PictureView, Vec<ViewAction>, tempfile::TempDir), String> {
    let dir = tempfile::tempdir().unwrap();
    let (mut view, mut probe) = ready_view(dir.path());
    let menu = open_more(&mut probe, &mut view)?;
    let actions = press_view_within(&mut probe, &mut view, label, menu)?;
    Ok((view, actions, dir))
}

fn more_toggle(label: &str, read: fn(&PictureView) -> bool) -> Result<(), String> {
    let dir = tempfile::tempdir().unwrap();
    let (mut view, mut probe) = ready_view(dir.path());
    press_view(&mut probe, &mut view, MORE)?;
    toggle_view(&mut probe, &mut view, label, read)
}

fn rotate(label: &str, turns: u8) -> Result<(), String> {
    let (view, _, _dir) = press_more(label)?;
    let got = view.settings().transform(Side::Left).quarter_turns;
    (got == turns)
        .then_some(())
        .ok_or_else(|| format!("{label} left {got} quarter turns, not {turns}"))
}

fn flip(label: &str, horizontal: bool) -> Result<(), String> {
    let (view, _, _dir) = press_more(label)?;
    let transform = view.settings().transform(Side::Left);
    (transform.flip_horizontal == horizontal && transform.flip_vertical != horizontal)
        .then_some(())
        .ok_or_else(|| format!("{label} left {transform:?}"))
}

fn swap() -> Result<(), String> {
    let (view, _, dir) = press_more("Swap")?;
    let (left, right) = paths(dir.path());
    (view.left() == right && view.right() == left)
        .then_some(())
        .ok_or_else(|| format!("Swap left {} on the left", view.left().display()))
}

fn reload() -> Result<(), String> {
    let dir = tempfile::tempdir().unwrap();
    let (mut view, mut probe) = ready_view(dir.path());
    let different = |view: &PictureView| view.outcome().map(|outcome| outcome.totals.different);
    if different(&view) == Some(0) {
        return Err("the fixture pictures do not differ".to_owned());
    }
    write_png(&paths(dir.path()).0, false);
    let menu = open_more(&mut probe, &mut view)?;
    press_view_within(&mut probe, &mut view, "Reload", menu)?;
    wait_until(&mut probe, &mut view, |view| {
        view.is_ready() && different(view) == Some(0)
    });
    Ok(())
}

fn stop() -> Result<(), String> {
    let dir = tempfile::tempdir().unwrap();
    let (mut view, mut probe) = ready_view(dir.path());
    press_view(&mut probe, &mut view, MORE)?;
    (!probe.find("Stop")?.enabled)
        .then_some(())
        .ok_or_else(|| "Stop is enabled while nothing runs".to_owned())
}

fn compare_as_hex() -> Result<(), String> {
    let (_, actions, dir) = press_more("Compare as hex")?;
    let (left, right) = paths(dir.path());
    let wanted = ViewAction::Open(OpenRequest::new(SessionKind::HexCompare, left, right));
    (actions == [wanted.clone()])
        .then_some(())
        .ok_or_else(|| format!("Compare as hex asked for {actions:?}"))
}

fn blend_toggle() -> Result<(), String> {
    more_toggle("Blend toggle", |view| view.settings().side == Side::Right)
}

fn check_more(label: &str) -> Result<(), String> {
    match label {
        "Rotate right" => rotate(label, 1),
        "Rotate left" => rotate(label, 3),
        "Flip across y" => flip(label, true),
        "Flip across x" => flip(label, false),
        "Swap" => swap(),
        "Reload" => reload(),
        "Stop" => stop(),
        "Compare as hex" => compare_as_hex(),
        "Blend toggle" => blend_toggle(),
        "Minor" => more_toggle(label, |view| view.settings().ignore_unimportant),
        "Ignore alpha" => more_toggle(label, |view| view.settings().ignore_alpha),
        "Transparent equal" => more_toggle(label, |view| view.settings().transparent_pixels_equal),
        "Auto scale" => more_toggle(label, |view| view.settings().auto_scale),
        "Checkerboard" => more_toggle(label, PictureView::shows_checkerboard),
        "Metadata" => more_toggle(label, PictureView::shows_metadata),
        other => Err(format!("{other} has no click test")),
    }
}

fn more() -> Result<(), String> {
    let dir = tempfile::tempdir().unwrap();
    let (mut view, mut probe) = ready_view(dir.path());
    press_view(&mut probe, &mut view, MORE)?;
    let shown: Vec<String> = probe
        .controls()
        .iter()
        .filter(|control| control.is_pressable())
        .filter(|control| MORE_CONTROLS.contains(&control.label.as_str()))
        .map(|control| control.label.clone())
        .collect();
    let missing: Vec<&&str> = MORE_CONTROLS
        .iter()
        .filter(|label| !shown.iter().any(|held| held == *label))
        .collect();
    if !missing.is_empty() {
        return Err(format!("the More menu does not show {missing:?}"));
    }
    let failures: Vec<String> = MORE_CONTROLS
        .iter()
        .filter_map(|label| check_more(label).err())
        .collect();
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

fn check_slot(slot: &str) -> Result<(), String> {
    match slot {
        "home" => home(),
        "zoom-in" | "zoom-out" => zoom(),
        "actual-size" => actual_size(),
        "fit" => fit(),
        "panes" => panes(),
        "more" => more(),
        // Holds no pressable control, which the untested button test enforces.
        "mode" | "tolerance" => Ok(()),
        other => Err(format!("{other} has no click test")),
    }
}

#[test]
fn every_picture_toolbar_slot_is_pressed_and_acts() {
    let dir = tempfile::tempdir().unwrap();
    let (view, _) = ready_view(dir.path());
    let items = view.toolbar_items();
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
fn the_picture_toolbar_holds_no_untested_button() {
    let dir = tempfile::tempdir().unwrap();
    let (mut view, mut probe) = ready_view(dir.path());
    let items = view.toolbar_items();
    let labels: Vec<&str> = SLOTS
        .iter()
        .flat_map(|(_, labels)| labels.iter().copied())
        .collect();
    let strangers = probe::toolbar_strangers(&probe, &items, &labels).unwrap();
    assert!(
        strangers.is_empty(),
        "toolbar controls with no click test: {strangers:?}"
    );
    press_view(&mut probe, &mut view, MORE).unwrap();
    let region = probe::region_of(probe.controls(), MORE_CONTROLS).unwrap();
    let mut known = MORE_CONTROLS.to_vec();
    known.extend(labels);
    let hidden = probe::strangers(probe.controls(), &known, region);
    assert!(
        hidden.is_empty(),
        "controls behind More with no click test: {hidden:?}"
    );
}
