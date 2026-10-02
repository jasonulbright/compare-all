//! The worker pipeline, driven with images built in the test.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use ca_image::compare::{DisplayMode, Offset, PixelClass, Side};
use ca_view_picture::jobs::{self, Message, Outcome, Settings, SideTransform};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn collect(left: &Path, right: &Path, settings: Settings) -> Vec<Message> {
    let mut job = jobs::spawn_load(
        left.to_path_buf(),
        right.to_path_buf(),
        settings,
        Arc::new(|| {}),
    );
    let deadline = Instant::now() + common::BUDGET;
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        seen.extend(job.drain());
        if job.is_finished() {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    seen
}

fn outcome(messages: Vec<Message>) -> Outcome {
    messages
        .into_iter()
        .find_map(|message| match message {
            Message::Ready(outcome) => Some(*outcome),
            _ => None,
        })
        .expect("the comparison finished")
}

fn failure(messages: &[Message]) -> Option<String> {
    messages.iter().find_map(|message| match message {
        Message::Failed(text) | Message::SideFailed(_, text) => Some(text.clone()),
        _ => None,
    })
}

#[test]
fn two_images_compare_to_a_result_and_totals() {
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = common::pair(dir.path());
    let ready = outcome(collect(&left, &right, Settings::default()));
    assert_eq!((ready.result.width(), ready.result.height()), (8, 6));
    assert_eq!(ready.totals.compared_total(), 48);
    assert_eq!(ready.totals.different, 1);
    assert_eq!(ready.totals.same, 47);
    assert_eq!(ready.mask.get(3, 2), Some(PixelClass::Different));
    assert!(!ready.totals.is_identical(false));
}

#[test]
fn a_tolerance_above_the_difference_makes_it_unimportant() {
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("a.png");
    let right = dir.path().join("b.png");
    common::write_png(&left, 4, 4, [100, 100, 100, 255], None);
    common::write_png(&right, 4, 4, [104, 100, 100, 255], None);

    let tight = outcome(collect(&left, &right, Settings::default()));
    assert_eq!(tight.totals.different, 16);

    let loose = outcome(collect(
        &left,
        &right,
        Settings {
            tolerance: 8,
            ..Settings::default()
        },
    ));
    assert_eq!(loose.totals.different, 0);
    assert_eq!(loose.totals.similar, 16);
    assert!(!loose.totals.is_identical(false));
    assert!(loose.totals.is_identical(true));
}

#[test]
fn an_offset_aligns_a_cropped_image_with_the_one_it_came_from() {
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("whole.png");
    let right = dir.path().join("cropped.png");
    common::write_png(&left, 8, 8, [10, 10, 10, 255], None);
    common::write_png(&right, 4, 4, [10, 10, 10, 255], None);

    let aligned = outcome(collect(
        &left,
        &right,
        Settings {
            offset: Offset::new(2, 2),
            ..Settings::default()
        },
    ));
    assert_eq!(aligned.totals.same, 16);
    assert_eq!(aligned.totals.left_only, 48);
    assert_eq!(aligned.totals.right_only, 0);
    assert_eq!((aligned.origin.x, aligned.origin.y), (0, 0));

    let before = outcome(collect(
        &left,
        &right,
        Settings {
            offset: Offset::new(-2, -2),
            ..Settings::default()
        },
    ));
    assert_eq!((before.origin.x, before.origin.y), (-2, -2));
    assert_eq!((before.result.width(), before.result.height()), (10, 10));
}

#[test]
fn every_mode_produces_a_result_of_the_same_size() {
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = common::pair(dir.path());
    for mode in [
        DisplayMode::Tolerance,
        DisplayMode::MismatchRange,
        DisplayMode::Blend,
        DisplayMode::SingleSide,
        DisplayMode::ChannelDifference,
        DisplayMode::ChannelXor,
    ] {
        let ready = outcome(collect(
            &left,
            &right,
            Settings {
                mode,
                ..Settings::default()
            },
        ));
        assert_eq!(
            (ready.result.width(), ready.result.height()),
            (8, 6),
            "{mode:?}"
        );
        assert_eq!(ready.totals.different, 1, "{mode:?}");
    }
}

#[test]
fn single_side_mode_renders_the_side_it_names() {
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("l.png");
    let right = dir.path().join("r.png");
    common::write_png(&left, 2, 2, [1, 2, 3, 255], None);
    common::write_png(&right, 2, 2, [9, 8, 7, 255], None);
    for (side, expected) in [(Side::Left, [1, 2, 3, 255]), (Side::Right, [9, 8, 7, 255])] {
        let ready = outcome(collect(
            &left,
            &right,
            Settings {
                mode: DisplayMode::SingleSide,
                side,
                ..Settings::default()
            },
        ));
        assert_eq!(ready.result.pixel(0, 0), Some(expected));
    }
}

#[test]
fn the_blend_percentage_shifts_the_mix_from_the_right_image_to_the_left() {
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("l.png");
    let right = dir.path().join("r.png");
    common::write_png(&left, 2, 2, [200, 0, 0, 255], None);
    common::write_png(&right, 2, 2, [0, 200, 0, 255], None);

    let blended_at = |percent: u8| {
        let ready = outcome(collect(
            &left,
            &right,
            Settings {
                mode: DisplayMode::Blend,
                blend_percent: percent,
                ..Settings::default()
            },
        ));
        ready.result.pixel(0, 0).expect("the pixel is covered")
    };

    // Full weight on one side reproduces that side's color exactly; the
    // in-between percentage lands strictly between the two.
    assert_eq!(blended_at(100), [200, 0, 0, 255]);
    assert_eq!(blended_at(0), [0, 200, 0, 255]);
    let half = blended_at(50);
    assert!(
        half[0] > 0 && half[0] < 200,
        "the red channel should sit between the two sources, got {half:?}"
    );
    assert!(
        half[1] > 0 && half[1] < 200,
        "the green channel should sit between the two sources, got {half:?}"
    );
}

#[test]
fn a_quarter_turn_makes_a_rotated_copy_match() {
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("upright.png");
    let right = dir.path().join("turned.png");
    common::write_png(&left, 4, 2, [20, 20, 20, 255], Some((0, 0)));
    // The same content a quarter turn counterclockwise: the marked pixel moves
    // to the bottom-left corner of a two by four image.
    common::write_png(&right, 2, 4, [20, 20, 20, 255], Some((0, 3)));

    let turned = outcome(collect(
        &left,
        &right,
        Settings {
            right_transform: SideTransform {
                quarter_turns: 1,
                ..SideTransform::default()
            },
            ..Settings::default()
        },
    ));
    assert_eq!(turned.right.width(), 4);
    assert_eq!(turned.right.height(), 2);
    assert!(turned.totals.is_identical(false), "{:?}", turned.totals);
}

#[test]
fn automatic_scaling_brings_two_sizes_to_one() {
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("small.png");
    let right = dir.path().join("large.png");
    common::write_png(&left, 4, 4, [30, 30, 30, 255], None);
    common::write_png(&right, 12, 12, [30, 30, 30, 255], None);

    let plain = outcome(collect(&left, &right, Settings::default()));
    assert!(plain.totals.right_only > 0);

    let scaled = outcome(collect(
        &left,
        &right,
        Settings {
            auto_scale: true,
            ..Settings::default()
        },
    ));
    assert_eq!((scaled.left.width(), scaled.left.height()), (12, 12));
    assert!(scaled.totals.is_identical(false));
}

#[test]
fn a_very_large_result_reports_a_reduced_copy() {
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("wide.png");
    let right = dir.path().join("wide2.png");
    let side = jobs::PREVIEW_MAX_SIDE + 64;
    common::write_png(&left, side, 8, [5, 5, 5, 255], None);
    common::write_png(&right, side, 8, [5, 5, 5, 255], None);
    let ready = outcome(collect(&left, &right, Settings::default()));
    let preview = ready.preview.as_ref().expect("a reduced copy was made");
    assert_eq!(preview.width(), jobs::PREVIEW_MAX_SIDE);
    assert!(ready
        .left_preview
        .as_ref()
        .is_some_and(|preview| preview.width() <= jobs::PREVIEW_MAX_SIDE));
    assert!(ready
        .right_preview
        .as_ref()
        .is_some_and(|preview| preview.width() <= jobs::PREVIEW_MAX_SIDE));
}

#[test]
fn a_missing_file_reports_which_side_failed() {
    let dir = tempfile::tempdir().unwrap();
    let (left, _) = common::pair(dir.path());
    let messages = collect(&left, &dir.path().join("absent.png"), Settings::default());
    assert!(messages
        .iter()
        .any(|message| matches!(message, Message::SideFailed(Side::Right, _))));
    assert!(failure(&messages).is_some());
    assert!(messages.iter().any(ca_ui::worker::Terminal::is_terminal));
}

#[test]
fn a_cancelled_run_reaches_a_terminal_state() {
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = common::pair(dir.path());
    let mut job = jobs::spawn_load(left, right, Settings::default(), Arc::new(|| {}));
    job.cancel();
    let deadline = Instant::now() + common::BUDGET;
    while Instant::now() < deadline {
        job.drain();
        if job.is_finished() {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("the cancelled job never finished");
}
