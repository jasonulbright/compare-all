//! Input the view cannot compare, and what it says about it.
//!
//! None of these makes the view fail. The pane that cannot be filled carries
//! the sentence, and every control stays where it was.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use ca_image::compare::Side;
use ca_image::Limits;
use ca_ui::command::Command;
use ca_ui::view::SessionView;
use ca_view_picture::jobs::{self, Message, Settings};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn side_failure(left: &Path, right: &Path, settings: Settings) -> Option<(Side, String)> {
    let mut job = jobs::spawn_load(
        left.to_path_buf(),
        right.to_path_buf(),
        settings,
        Arc::new(|| {}),
    );
    let deadline = Instant::now() + common::BUDGET;
    let mut found = None;
    while Instant::now() < deadline {
        for message in job.drain() {
            if let Message::SideFailed(side, text) = message {
                found = Some((side, text));
            }
        }
        if job.is_finished() {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    found
}

#[test]
fn a_corrupt_file_names_the_file_and_the_damage() {
    let dir = tempfile::tempdir().unwrap();
    let (left, _) = common::pair(dir.path());
    let broken = dir.path().join("broken.png");
    let mut bytes = std::fs::read(&left).unwrap();
    // Keep the signature so the format still resolves, and ruin the data after
    // it, which is what a truncated or damaged file looks like.
    bytes.truncate(40);
    std::fs::write(&broken, &bytes).unwrap();

    let (side, text) = side_failure(&left, &broken, Settings::default())
        .expect("the damaged side reported a failure");
    assert_eq!(side, Side::Right);
    assert!(text.contains("broken.png"), "{text}");
}

#[test]
fn an_unsupported_format_says_so_rather_than_calling_it_damaged() {
    let dir = tempfile::tempdir().unwrap();
    let (left, _) = common::pair(dir.path());
    let other = dir.path().join("notes.txt");
    std::fs::write(&other, b"this is not an image at all").unwrap();

    let (side, text) =
        side_failure(&left, &other, Settings::default()).expect("the text file was refused");
    assert_eq!(side, Side::Right);
    assert!(text.contains("notes.txt"), "{text}");
    assert!(
        text.contains("format") || text.contains("decoder"),
        "{text}"
    );
}

#[test]
fn an_image_above_the_limits_reports_the_limit_it_crossed() {
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = common::pair(dir.path());
    // A dimension the decoder's own guard also knows about.
    let by_size = Settings {
        decode_limits: Limits {
            max_width: 4,
            max_height: 4,
            ..Limits::default()
        },
        ..Settings::default()
    };
    let (_, text) = side_failure(&left, &right, by_size).expect("the limit was reported");
    assert!(text.contains("limit"), "{text}");
    assert!(text.contains(".png"), "{text}");

    // A pixel count only this crate's limits carry, which the header check
    // catches before anything is allocated.
    let by_pixels = Settings {
        decode_limits: Limits {
            max_pixels: 4,
            ..Limits::default()
        },
        ..Settings::default()
    };
    let (_, text) = side_failure(&left, &right, by_pixels).expect("the limit was reported");
    assert!(text.contains("48 pixels"), "{text}");
    assert!(text.contains("above the limit of 4"), "{text}");
}

#[test]
fn a_failing_side_shows_its_message_in_its_own_pane() {
    let dir = tempfile::tempdir().unwrap();
    let (left, _) = common::pair(dir.path());
    let missing = dir.path().join("absent.png");
    let (mut view, ctx) = common::open(left, missing, 20);
    assert!(common::run_until(&mut view, &ctx, |view| view
        .side_error(Side::Right)
        .is_some()));
    let text = view.side_error(Side::Right).unwrap().to_owned();
    assert!(text.contains("absent.png"), "{text}");
    assert!(view.side_error(Side::Left).is_none());
    // The view is still a view: it paints, it declares its commands, and it
    // answers a reload.
    assert!(!view.commands().is_empty());
    assert!(view.failure().is_some());
    common::frame(&mut view, &ctx, 1_280.0, 800.0);
    assert!(!view.is_ready());
}

#[test]
fn replacing_a_failing_side_clears_its_message() {
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = common::pair(dir.path());
    let missing = dir.path().join("absent.png");
    let (mut view, ctx) = common::open(left, missing, 21);
    assert!(common::run_until(&mut view, &ctx, |view| view
        .side_error(Side::Right)
        .is_some()));
    std::fs::copy(&right, dir.path().join("absent.png")).unwrap();
    view.run(Command::Reload);
    assert!(common::run_until_ready(&mut view, &ctx));
    assert!(view.side_error(Side::Right).is_none());
    assert!(view.failure().is_none());
}

#[test]
fn a_result_above_the_result_limits_reports_a_failure_not_a_crash() {
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = common::pair(dir.path());
    let settings = Settings {
        result_limits: Limits {
            max_width: 8,
            max_height: 8,
            ..Limits::default()
        },
        offset: ca_image::compare::Offset::new(40, 0),
        ..Settings::default()
    };
    let mut job = jobs::spawn_load(left, right, settings, Arc::new(|| {}));
    let deadline = Instant::now() + common::BUDGET;
    let mut failure = None;
    while Instant::now() < deadline {
        for message in job.drain() {
            if let Message::Failed(text) = message {
                failure = Some(text);
            }
        }
        if job.is_finished() {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let text = failure.expect("the oversized result was reported");
    assert!(text.contains("above the limit"), "{text}");
}
