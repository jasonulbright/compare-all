//! Input built to make a bounded operation cost far more than its file size.

#![allow(clippy::unwrap_used)]

mod fixtures;

use ca_image::{decode_bytes, decode_bytes_with_cancel, CancelFlag, DecodeOptions, Error, Limits};
use std::time::{Duration, Instant};

/// Frames in the hostile file. It is the frame count cap, so the count also
/// proves the cap reports the true number.
const FRAMES: u32 = 4096;

/// Edge of the logical screen. A counter that composites every frame onto the
/// screen does this many pixels of work per frame.
const SCREEN: u16 = 4096;

/// A structural count reads the container only, so it finishes well inside
/// this bound on any machine that runs the test suite.
const COUNT_BOUND: Duration = Duration::from_secs(30);

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "wall-clock budget holds for release builds"
)]
fn a_small_gif_with_many_frames_counts_within_the_bound() {
    let bytes = fixtures::gif_many_small_frames(FRAMES, SCREEN);
    assert!(bytes.len() < 100_000, "the fixture is not a small file");

    let options = DecodeOptions {
        count_frames: true,
        ..DecodeOptions::default()
    };
    let started = Instant::now();
    let decoded = decode_bytes(&bytes, &options).unwrap();
    let elapsed = started.elapsed();

    assert_eq!(decoded.metadata.frame_count, Some(FRAMES));
    assert!(!decoded.metadata.count_is_partial);
    assert!(
        elapsed < COUNT_BOUND,
        "counting {FRAMES} frames took {elapsed:?}, above the bound of {COUNT_BOUND:?}"
    );
    println!(
        "counted {FRAMES} frames of a {} byte file in {elapsed:?}",
        bytes.len()
    );
}

#[test]
fn a_spent_scan_budget_stops_the_count() {
    let bytes = fixtures::gif_many_small_frames(FRAMES, 64);
    let options = DecodeOptions {
        count_frames: true,
        max_frame_scan_bytes: 512,
        ..DecodeOptions::default()
    };
    let decoded = decode_bytes(&bytes, &options).unwrap();
    let counted = decoded.metadata.frame_count.unwrap_or(u32::MAX);
    assert!(counted < FRAMES, "the budget did not stop the count");
    assert!(decoded.metadata.count_is_partial);
}

#[test]
fn a_fired_cancel_stops_the_count() {
    let bytes = fixtures::gif_many_small_frames(FRAMES, 64);
    let options = DecodeOptions {
        count_frames: true,
        ..DecodeOptions::default()
    };
    let flag = CancelFlag::new();
    flag.cancel();
    let decoded = decode_bytes_with_cancel(&bytes, &options, &flag).unwrap();
    let counted = decoded.metadata.frame_count.unwrap_or(u32::MAX);
    assert!(counted < FRAMES, "the cancellation did not stop the count");
    assert!(decoded.metadata.count_is_partial);
}

#[test]
fn a_count_past_the_ceiling_is_marked_as_a_lower_bound() {
    let options = DecodeOptions {
        count_frames: true,
        ..DecodeOptions::default()
    };
    let at_ceiling = decode_bytes(&fixtures::gif_many_small_frames(FRAMES, 16), &options).unwrap();
    assert_eq!(at_ceiling.metadata.frame_count, Some(FRAMES));
    assert!(!at_ceiling.metadata.count_is_partial);

    let past = decode_bytes(&fixtures::gif_many_small_frames(FRAMES + 1, 16), &options).unwrap();
    assert_eq!(past.metadata.frame_count, Some(FRAMES));
    assert!(past.metadata.count_is_partial);
}

#[test]
fn a_gif_holding_no_frames_reports_no_frames() {
    let bytes = fixtures::gif_many_small_frames(0, 16);
    let options = DecodeOptions {
        count_frames: true,
        ..DecodeOptions::default()
    };
    // The still image cannot decode, so only the count is under test here.
    let counted = decode_bytes(&bytes, &options);
    assert!(counted.is_err(), "a GIF without an image decoded");

    let with_one = fixtures::gif_many_small_frames(1, 16);
    let decoded = decode_bytes(&with_one, &options).unwrap();
    assert_eq!(decoded.metadata.frame_count, Some(1));
}

#[test]
fn a_native_buffer_and_the_rgba_buffer_are_bounded_together() {
    // Three channels of 16384 by 16384 hold 805 306 368 bytes and the RGBA8
    // buffer holds 1 073 741 824, so the pair crosses a limit that either one
    // alone stays under.
    let bytes = fixtures::png_declaring_rgb(16_384, 16_384);
    let outcome = decode_bytes(&bytes, &DecodeOptions::default());
    assert!(
        matches!(outcome, Err(Error::TooLarge { unit: "bytes", .. })),
        "unexpected outcome: {outcome:?}"
    );
}

#[test]
fn a_format_that_decodes_straight_to_rgba_counts_one_buffer() {
    // Four channels of 16384 by 16384 hold exactly the default byte limit, so
    // the sum check must not count that buffer twice.
    let bytes = fixtures::png_declaring(16_384, 16_384);
    let outcome = decode_bytes(&bytes, &DecodeOptions::default());
    assert!(
        !matches!(outcome, Err(Error::TooLarge { .. })),
        "a single buffer was counted twice: {outcome:?}"
    );
}

#[test]
fn an_allocation_above_the_limits_reports_an_error() {
    // The byte count of this size is above the default limit. The process must
    // report it, not commit the memory and abort.
    let outcome = ca_image::RgbaImage::new(65_535, 65_535);
    assert!(
        matches!(outcome, Err(Error::TooLarge { .. })),
        "unexpected outcome: {outcome:?}"
    );

    let resized = ca_image::transform::resize(
        &ca_image::RgbaImage::filled(2, 2, [1, 2, 3, 4]).unwrap(),
        65_535,
        65_535,
        ca_image::transform::Filter::Nearest,
    );
    assert!(matches!(resized, Err(Error::TooLarge { .. })));

    let limits = Limits {
        max_pixels: 16,
        ..Limits::default()
    };
    let filled = ca_image::RgbaImage::filled_within(8, 8, [0, 0, 0, 0], &limits);
    assert!(matches!(filled, Err(Error::TooLarge { .. })));
}

/// Correctness half of the frame count case, on a fixture small enough for a
/// debug build: the count is the true number of frames.
#[test]
fn a_small_gif_reports_the_number_of_frames_it_holds() {
    let frames = 64;
    let bytes = fixtures::gif_many_small_frames(frames, SCREEN);
    let options = DecodeOptions {
        count_frames: true,
        ..DecodeOptions::default()
    };
    let decoded = decode_bytes(&bytes, &options).unwrap();
    assert_eq!(decoded.metadata.frame_count, Some(frames));
}
