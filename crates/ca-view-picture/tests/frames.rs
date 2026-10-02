//! Whole frames of the picture view, run without a window.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use ca_image::compare::{DisplayMode, Offset, PixelClass, Side};
use ca_ui::command::Command;
use ca_ui::view::SessionView;
use ca_view_picture::model::Pane;

#[test]
fn two_images_open_and_reach_a_comparison() {
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = common::pair(dir.path());
    let (mut view, ctx) = common::open(left, right, 1);
    assert!(common::run_until_ready(&mut view, &ctx));
    let outcome = view.outcome().expect("a comparison is on screen");
    assert_eq!(outcome.totals.different, 1);
    assert!(view.failure().is_none());
    assert!(view.side_error(Side::Left).is_none());
    assert!(view.side_error(Side::Right).is_none());
}

#[test]
fn switching_the_mode_produces_a_new_result() {
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = common::pair(dir.path());
    let (mut view, ctx) = common::open(left, right, 2);
    assert!(common::run_until_ready(&mut view, &ctx));
    for mode in [
        DisplayMode::MismatchRange,
        DisplayMode::Blend,
        DisplayMode::ChannelXor,
        DisplayMode::Tolerance,
    ] {
        view.set_mode(mode);
        assert!(
            common::run_until(&mut view, &ctx, |view| view.settings().mode == mode
                && view.is_ready()),
            "{mode:?} never produced a result"
        );
        assert_eq!(view.settings().mode, mode);
    }
}

#[test]
fn raising_the_tolerance_turns_a_difference_unimportant() {
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("a.png");
    let right = dir.path().join("b.png");
    common::write_png(&left, 6, 6, [80, 80, 80, 255], None);
    common::write_png(&right, 6, 6, [86, 80, 80, 255], None);
    let (mut view, ctx) = common::open(left, right, 3);
    assert!(common::run_until_ready(&mut view, &ctx));
    assert_eq!(view.outcome().unwrap().totals.different, 36);

    view.set_tolerance(16);
    assert!(common::run_until(&mut view, &ctx, |view| view
        .outcome()
        .is_some_and(|outcome| outcome.totals.similar == 36)));
    assert_eq!(view.outcome().unwrap().totals.different, 0);

    view.set_ignore_unimportant(true);
    assert!(common::run_until(&mut view, &ctx, |view| view
        .outcome()
        .is_some_and(|outcome| outcome.totals.is_identical(true))));
}

#[test]
fn a_drag_of_the_tolerance_leaves_one_result_not_one_per_value() {
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = common::pair(dir.path());
    let (mut view, ctx) = common::open(left, right, 4);
    assert!(common::run_until_ready(&mut view, &ctx));
    // A burst of values in one frame stands for a slider being dragged. Only
    // the value the drag ended on reaches a run.
    for value in 1..=40u8 {
        view.set_tolerance(value);
    }
    assert!(common::run_until(&mut view, &ctx, |view| view
        .outcome()
        .is_some()
        && view.settings().tolerance == 40));
    assert_eq!(view.settings().tolerance, 40);
}

#[test]
fn an_arrow_key_nudges_the_offset() {
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = common::pair(dir.path());
    let (mut view, ctx) = common::open(left, right, 5);
    assert!(common::run_until_ready(&mut view, &ctx));
    assert_eq!(view.settings().offset, Offset::zero());

    let press = |key: egui::Key, modifiers: egui::Modifiers| egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    };
    common::frame_with(
        &mut view,
        &ctx,
        1_280.0,
        800.0,
        vec![press(egui::Key::ArrowRight, egui::Modifiers::NONE)],
    );
    assert_eq!(view.settings().offset, Offset::new(1, 0));

    common::frame_with(
        &mut view,
        &ctx,
        1_280.0,
        800.0,
        vec![press(egui::Key::ArrowDown, egui::Modifiers::COMMAND)],
    );
    assert_eq!(view.settings().offset.x, 1);
    assert!(
        view.settings().offset.y > 1,
        "a modified press moves further"
    );

    view.reset_difference_offset();
    assert_eq!(view.settings().offset, Offset::zero());
    assert!(common::run_until_ready(&mut view, &ctx));
}

#[test]
fn the_offset_moves_which_pixels_are_compared() {
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("whole.png");
    let right = dir.path().join("crop.png");
    common::write_png(&left, 8, 8, [50, 50, 50, 255], None);
    common::write_png(&right, 4, 4, [50, 50, 50, 255], None);
    let (mut view, ctx) = common::open(left, right, 6);
    assert!(common::run_until_ready(&mut view, &ctx));

    view.set_offset(Offset::new(2, 2));
    assert!(common::run_until(&mut view, &ctx, |view| view
        .outcome()
        .is_some_and(|outcome| outcome.offset == Offset::new(2, 2))));
    let outcome = view.outcome().unwrap();
    assert_eq!(outcome.totals.same, 16);
    assert_eq!(outcome.mask.get(2, 2), Some(PixelClass::Same));
    assert_eq!(outcome.mask.get(0, 0), Some(PixelClass::LeftOnly));
}

#[test]
fn the_pointer_reports_the_pixel_it_rests_on() {
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = common::pair(dir.path());
    let (mut view, ctx) = common::open(left, right, 7);
    assert!(common::run_until_ready(&mut view, &ctx));
    assert!(view.pixel_details().is_none(), "nothing is pointed at yet");

    // Point at the middle of the difference pane, which is the pane the fit
    // centered the content in.
    let center = view
        .pane_rect(Pane::Difference)
        .expect("the difference pane was laid out")
        .center();
    common::frame_with(
        &mut view,
        &ctx,
        1_280.0,
        800.0,
        vec![egui::Event::PointerMoved(center)],
    );
    common::frame_with(
        &mut view,
        &ctx,
        1_280.0,
        800.0,
        vec![egui::Event::PointerMoved(center)],
    );
    let details = view.pixel_details().expect("a pixel is under the pointer");
    assert!(details.left.is_some() || details.right.is_some());
}

#[test]
fn the_panes_can_be_shown_and_hidden_but_never_all_hidden() {
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = common::pair(dir.path());
    let (mut view, ctx) = common::open(left, right, 8);
    assert!(common::run_until_ready(&mut view, &ctx));
    assert_eq!(view.panes().count(), 3);
    view.show_pane(Pane::Left, false);
    view.show_pane(Pane::Right, false);
    common::frame(&mut view, &ctx, 1_280.0, 800.0);
    assert_eq!(view.panes().count(), 1);
    view.show_pane(Pane::Difference, false);
    assert_eq!(view.panes().count(), 1);
    common::frame(&mut view, &ctx, 1_280.0, 800.0);
}

#[test]
fn the_zoom_commands_change_the_magnification() {
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = common::pair(dir.path());
    let (mut view, ctx) = common::open(left, right, 9);
    assert!(common::run_until_ready(&mut view, &ctx));
    view.actual_size();
    common::frame(&mut view, &ctx, 1_280.0, 800.0);
    let actual = view.camera().zoom;
    assert!((actual - 1.0).abs() < 1e-4);
    view.zoom_in();
    assert!(view.camera().zoom > actual);
    view.zoom_out();
    view.zoom_out();
    assert!(view.camera().zoom < actual);
    view.zoom_to_fit();
    common::frame(&mut view, &ctx, 1_280.0, 800.0);
    assert!(view.camera().zoom > 1.0, "a small image fills the pane");
}

#[test]
fn swapping_the_sides_reverses_the_offset() {
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = common::pair(dir.path());
    let (mut view, ctx) = common::open(left, right, 10);
    assert!(common::run_until_ready(&mut view, &ctx));
    view.set_offset(Offset::new(3, -2));
    assert!(common::run_until(&mut view, &ctx, |view| view
        .outcome()
        .is_some_and(|outcome| outcome.offset == Offset::new(3, -2))));
    let title = view.title();
    view.run(Command::SwapSides);
    assert_eq!(view.settings().offset, Offset::new(-3, 2));
    assert_ne!(view.title(), title);
    assert!(common::run_until_ready(&mut view, &ctx));
}

#[test]
fn a_rotation_of_one_side_changes_the_shown_image() {
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("a.png");
    let right = dir.path().join("b.png");
    common::write_png(&left, 6, 2, [40, 40, 40, 255], None);
    common::write_png(&right, 6, 2, [40, 40, 40, 255], None);
    let (mut view, ctx) = common::open(left, right, 11);
    assert!(common::run_until_ready(&mut view, &ctx));
    view.rotate(Side::Right, true);
    assert!(common::run_until(&mut view, &ctx, |view| view
        .outcome()
        .is_some_and(|outcome| outcome.right.width() == 2)));
    let outcome = view.outcome().unwrap();
    assert_eq!((outcome.right.width(), outcome.right.height()), (2, 6));
    assert_eq!((outcome.left.width(), outcome.left.height()), (6, 2));
}

#[test]
fn automatic_scaling_is_a_toggle_the_view_answers_to() {
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("small.png");
    let right = dir.path().join("large.png");
    common::write_png(&left, 3, 3, [60, 60, 60, 255], None);
    common::write_png(&right, 9, 9, [60, 60, 60, 255], None);
    let (mut view, ctx) = common::open(left, right, 12);
    assert!(common::run_until_ready(&mut view, &ctx));
    view.set_auto_scale(true);
    assert!(common::run_until(&mut view, &ctx, |view| view
        .outcome()
        .is_some_and(|outcome| outcome.left.width() == 9)));
    assert!(view.outcome().unwrap().totals.is_identical(false));
}

#[test]
fn cancelling_stops_the_work_and_leaves_the_view_usable() {
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = common::pair(dir.path());
    let (mut view, ctx) = common::open(left, right, 13);
    common::frame(&mut view, &ctx, 1_280.0, 800.0);
    view.run(Command::Cancel);
    common::frame(&mut view, &ctx, 1_280.0, 800.0);
    assert!(view.failure().is_none());
    view.run(Command::Reload);
    assert!(common::run_until_ready(&mut view, &ctx));
}

/// At fit zoom the panes draw the reduced copies, so the textures held follow
/// the viewport rather than the source size.
#[test]
fn fitted_panes_upload_the_reduced_copies_and_not_the_sources() {
    const WIDE: u32 = 4_096;
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("wide-left.png");
    let right = dir.path().join("wide-right.png");
    common::write_png(&left, WIDE, 16, [10, 20, 30, 255], Some((5, 5)));
    common::write_png(&right, WIDE, 16, [10, 20, 30, 255], None);
    let (mut view, ctx) = common::open(left, right, 44);
    assert!(common::run_until_ready(&mut view, &ctx));
    for _ in 0..60 {
        common::frame(&mut view, &ctx, 1_200.0, 800.0);
    }
    assert!(view.fits_to_panes());

    let one_source = u64::from(WIDE) * 16 * 4;
    let held: u64 = ctx
        .tex_manager()
        .read()
        .allocated()
        .filter(|(_, meta)| {
            ["Left-", "Right-", "Difference-"]
                .iter()
                .any(|pane| meta.name.starts_with(pane))
        })
        .map(|(_, meta)| u64::try_from(meta.bytes_used()).unwrap())
        .sum();
    assert!(held > 0, "the panes drew no texture");
    assert!(
        held < one_source,
        "{held} texture bytes held at fit; one full source is {one_source}"
    );
}
