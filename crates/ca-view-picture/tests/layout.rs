//! Where the view puts things, measured at real window sizes.
//!
//! A headless frame reports the same rectangles the window draws, so every
//! check here is geometry: what is inside what, what is centered, and what
//! overlaps.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use ca_view_picture::model::Pane;

/// The window the defects were observed in.
const WIDE: [f32; 2] = [1_150.0, 520.0];

/// A small window every control must still fit in.
const NARROW: [f32; 2] = [640.0, 480.0];

/// How far a measurement may differ before it counts as wrong.
const SLACK: f32 = 1.0;

#[test]
fn every_pane_shows_the_whole_image_centered() {
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = common::shapes(dir.path());
    let (mut view, ctx) = common::open(left, right, 40);
    assert!(common::run_until_ready_at(
        &mut view, &ctx, WIDE[0], WIDE[1]
    ));
    let geometry = common::layout(&mut view, &ctx, WIDE[0], WIDE[1]);

    let mut offsets = Vec::new();
    for pane in Pane::ALL {
        let rect = geometry.pane(pane);
        let image = geometry.image(pane);
        assert!(image.width() > 1.0 && image.height() > 1.0);
        let left_margin = image.min.x - rect.min.x;
        let right_margin = rect.max.x - image.max.x;
        let top_margin = image.min.y - rect.min.y;
        let bottom_margin = rect.max.y - image.max.y;
        assert!(
            (left_margin - right_margin).abs() <= SLACK,
            "{} is not centered across: {left_margin} against {right_margin}",
            pane.label()
        );
        assert!(
            (top_margin - bottom_margin).abs() <= SLACK,
            "{} is not centered down: {top_margin} against {bottom_margin}",
            pane.label()
        );
        offsets.push((pane, image.min - rect.min));
    }

    // One image pixel lands at the same place in every pane, which is what
    // makes the three panes readable side by side.
    let first = offsets[0].1;
    for (pane, offset) in &offsets {
        assert!(
            (offset.x - first.x).abs() <= SLACK && (offset.y - first.y).abs() <= SLACK,
            "{} places the image differently",
            pane.label()
        );
    }
}

#[test]
fn the_fit_is_computed_from_the_smallest_pane() {
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = common::shapes(dir.path());
    let (mut view, ctx) = common::open(left, right, 41);
    assert!(common::run_until_ready_at(
        &mut view, &ctx, WIDE[0], WIDE[1]
    ));
    let geometry = common::layout(&mut view, &ctx, WIDE[0], WIDE[1]);
    let smallest = Pane::ALL
        .into_iter()
        .map(|pane| geometry.pane(pane))
        .min_by(|a, b| (a.area()).partial_cmp(&b.area()).unwrap())
        .unwrap();
    let wanted = (smallest.width() / 160.0).min(smallest.height() / 100.0);
    assert!(
        (view.camera().zoom - wanted).abs() < 1e-2,
        "the fit is {} and the smallest pane wants {wanted}",
        view.camera().zoom
    );
    assert!(view.fits_to_panes());
}

#[test]
fn a_resize_refits_and_a_manual_zoom_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = common::shapes(dir.path());
    let (mut view, ctx) = common::open(left, right, 42);
    assert!(common::run_until_ready_at(
        &mut view, &ctx, WIDE[0], WIDE[1]
    ));
    common::layout(&mut view, &ctx, WIDE[0], WIDE[1]);
    let wide = view.camera().zoom;

    common::layout(&mut view, &ctx, NARROW[0], NARROW[1]);
    let narrow = view.camera().zoom;
    assert!(narrow < wide, "a smaller window did not refit");

    view.zoom_in();
    let manual = view.camera().zoom;
    assert!(!view.fits_to_panes());
    common::layout(&mut view, &ctx, WIDE[0], WIDE[1]);
    assert!(
        (view.camera().zoom - manual).abs() < 1e-4,
        "a resize undid a manual zoom"
    );

    view.zoom_to_fit();
    common::layout(&mut view, &ctx, WIDE[0], WIDE[1]);
    assert!((view.camera().zoom - wide).abs() < 1e-2);
}

#[test]
fn the_status_bar_stays_inside_the_window() {
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = common::shapes(dir.path());
    let (mut view, ctx) = common::open(left, right, 43);
    assert!(common::run_until_ready_at(
        &mut view, &ctx, WIDE[0], WIDE[1]
    ));
    for size in [WIDE, NARROW] {
        // The first frame measures the toolbar, the second lays it out.
        common::layout(&mut view, &ctx, size[0], size[1]);
        let geometry = common::layout(&mut view, &ctx, size[0], size[1]);
        assert!(
            geometry.status.max.y <= size[1] + SLACK,
            "the status bar ends at {} in a window {} tall",
            geometry.status.max.y,
            size[1]
        );
        assert!(geometry.status.height() > 0.0);
    }
}

#[test]
fn the_controls_beside_the_panes_never_cover_one_another() {
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = common::shapes(dir.path());
    let (mut view, ctx) = common::open(left, right, 44);
    assert!(common::run_until_ready_at(
        &mut view, &ctx, WIDE[0], WIDE[1]
    ));
    for size in [WIDE, NARROW] {
        // The overlap check itself lives in the helper.
        let geometry = common::layout(&mut view, &ctx, size[0], size[1]);
        assert!(
            geometry.widgets.len() >= 6,
            "too few rectangles were reported to mean anything"
        );
    }
}

#[test]
fn the_toolbar_keeps_to_two_rows_and_fits_a_narrow_window() {
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = common::shapes(dir.path());
    let (mut view, ctx) = common::open(left, right, 45);
    assert!(common::run_until_ready_at(
        &mut view, &ctx, WIDE[0], WIDE[1]
    ));
    common::layout(&mut view, &ctx, WIDE[0], WIDE[1]);
    let geometry = common::layout(&mut view, &ctx, WIDE[0], WIDE[1]);
    assert!(
        geometry.toolbar_rows <= 2,
        "the toolbar took {} rows",
        geometry.toolbar_rows
    );

    common::layout(&mut view, &ctx, NARROW[0], NARROW[1]);
    let narrow = common::layout(&mut view, &ctx, NARROW[0], NARROW[1]);
    assert!(narrow.toolbar.max.x <= NARROW[0] + SLACK);
}

#[test]
fn the_panes_fill_the_room_between_the_bars() {
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = common::shapes(dir.path());
    let (mut view, ctx) = common::open(left, right, 46);
    assert!(common::run_until_ready_at(
        &mut view, &ctx, WIDE[0], WIDE[1]
    ));
    common::layout(&mut view, &ctx, WIDE[0], WIDE[1]);
    let geometry = common::layout(&mut view, &ctx, WIDE[0], WIDE[1]);
    let room = geometry.status.min.y - geometry.toolbar.max.y;
    assert!(room > 100.0, "there is no room to fill");
    for pane in Pane::ALL {
        let rect = geometry.pane(pane);
        assert!(
            rect.height() >= room - 2.0 * SLACK,
            "{} is {} tall in {room} of room",
            pane.label(),
            rect.height()
        );
    }
    let width: f32 = Pane::ALL
        .into_iter()
        .map(|pane| geometry.pane(pane).width())
        .sum();
    assert!(
        width >= geometry.toolbar.width() - 230.0,
        "the panes cover only {width} of the width"
    );
}
