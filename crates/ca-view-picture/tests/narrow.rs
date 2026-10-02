//! The toolbar in a narrow window.
//!
//! A control that runs past the edge of the window cannot be reached, so the
//! bar either wraps onto further lines or moves its controls behind one button.
//! Either way nothing is drawn outside the room the bar was given.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use ca_ui::view::SessionView;

/// The window width the narrow case is measured at.
const NARROW: f32 = 640.0;

#[test]
fn no_toolbar_control_is_clipped_in_a_narrow_window() {
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = common::pair(dir.path());
    let (mut view, ctx) = common::open(left, right, 30);
    // Two frames: the first measures what the controls need, the second lays
    // them out with that measurement in hand.
    common::frame(&mut view, &ctx, NARROW, 700.0);
    common::frame(&mut view, &ctx, NARROW, 700.0);
    let bar = view.toolbar_rect();
    assert!(bar.width() <= NARROW, "the toolbar is {} wide", bar.width());
    assert!(bar.max.x <= NARROW + 1.0, "the toolbar runs past the edge");
}

#[test]
fn the_toolbar_carries_its_controls_when_the_window_is_wide() {
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = common::pair(dir.path());
    let (mut view, ctx) = common::open(left, right, 31);
    common::frame(&mut view, &ctx, 1_600.0, 900.0);
    common::frame(&mut view, &ctx, 1_600.0, 900.0);
    assert!(view.toolbar_rect().max.x <= 1_601.0);
}

#[test]
fn a_narrow_window_still_reaches_a_comparison() {
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = common::pair(dir.path());
    let (mut view, ctx) = common::open(left, right, 32);
    let deadline = std::time::Instant::now() + common::BUDGET;
    loop {
        common::frame(&mut view, &ctx, NARROW, 700.0);
        if view.is_ready() {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "no result arrived");
        std::thread::sleep(std::time::Duration::from_millis(4));
    }
    assert!(view.toolbar_rect().max.x <= NARROW + 1.0);
    assert!(view.outcome().is_some());
}

#[test]
fn a_very_narrow_window_moves_the_controls_behind_one_button() {
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = common::pair(dir.path());
    let (mut view, ctx) = common::open(left, right, 33);
    common::frame(&mut view, &ctx, 60.0, 700.0);
    common::frame(&mut view, &ctx, 60.0, 700.0);
    assert!(view.toolbar_collapsed());
    assert!(view.toolbar_rect().max.x <= 61.0);
}
