//! A bounded-time comparison of two large images.
//!
//! The bound is a guard against an accidental quadratic path, not a
//! performance target. The peak allocation is the two inputs, the result
//! buffer and the mask.

#![allow(clippy::unwrap_used)]

use ca_image::{compare, CompareOptions, NeverCancel, RgbaImage};
use std::time::{Duration, Instant};

const EDGE: u32 = 8_000;

/// The comparison of two images of this size must finish well inside this
/// bound on any machine that can hold them.
const BOUND: Duration = Duration::from_secs(600);

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "wall-clock budget holds for release builds"
)]
fn two_large_images_compare_within_the_bound() {
    let left = RgbaImage::filled(EDGE, EDGE, [10, 20, 30, 255]).unwrap();
    let mut right = RgbaImage::filled(EDGE, EDGE, [10, 20, 30, 255]).unwrap();
    // One differing row, so the run cannot be shortened by an equality check.
    for x in 0..EDGE {
        right.set_pixel(x, EDGE / 2, [200, 20, 30, 255]);
    }

    let started = Instant::now();
    let result = compare(&left, &right, &CompareOptions::default(), &NeverCancel).unwrap();
    let elapsed = started.elapsed();

    let pixels = u64::from(EDGE) * u64::from(EDGE);
    assert_eq!(result.totals.total(), pixels);
    assert_eq!(result.totals.different, u64::from(EDGE));
    assert_eq!(result.totals.same, pixels - u64::from(EDGE));
    assert_eq!(
        result.mask.packed().len(),
        (EDGE as usize / 2) * EDGE as usize
    );
    assert!(
        elapsed < BOUND,
        "the comparison took {elapsed:?}, above the bound of {BOUND:?}"
    );
    println!("compared {EDGE}x{EDGE} in {elapsed:?}");
}

/// Correctness half of the large comparison, at a size a debug build handles:
/// one differing row is counted as one differing pixel per column.
#[test]
fn two_images_differing_in_one_row_report_that_row() {
    let edge = 200u32;
    let left = RgbaImage::filled(edge, edge, [10, 20, 30, 255]).unwrap();
    let mut right = RgbaImage::filled(edge, edge, [10, 20, 30, 255]).unwrap();
    for x in 0..edge {
        right.set_pixel(x, edge / 2, [200, 20, 30, 255]);
    }
    let result = compare(&left, &right, &CompareOptions::default(), &NeverCancel).unwrap();
    let pixels = u64::from(edge) * u64::from(edge);
    assert_eq!(result.totals.total(), pixels);
    assert_eq!(result.totals.different, u64::from(edge));
    assert_eq!(result.totals.same, pixels - u64::from(edge));
    assert_eq!(
        result.mask.packed().len(),
        (edge as usize / 2) * edge as usize
    );
}
