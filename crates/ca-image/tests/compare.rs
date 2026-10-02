//! Comparison: every display mode, the tolerance edges, offsets, alpha,
//! replacements and cancellation, on images whose expected pixels are exact.

#![allow(clippy::unwrap_used)]

use ca_image::cancel::{Cancel, CancelFlag, NeverCancel};
use ca_image::{
    compare, CompareOptions, DisplayMode, Error, Offset, PixelClass, Replacement, RgbaImage, Side,
};

const BLACK: [u8; 4] = [0, 0, 0, 255];
const WHITE: [u8; 4] = [255, 255, 255, 255];

fn image(width: u32, height: u32, pixels: &[[u8; 4]]) -> RgbaImage {
    let mut flat = Vec::with_capacity(pixels.len() * 4);
    for pixel in pixels {
        flat.extend_from_slice(pixel);
    }
    RgbaImage::from_pixels(width, height, flat).unwrap()
}

fn solid(width: u32, height: u32, color: [u8; 4]) -> RgbaImage {
    RgbaImage::filled(width, height, color).unwrap()
}

#[test]
fn tolerance_mode_tints_each_class() {
    let left = image(3, 1, &[BLACK, WHITE, WHITE]);
    let right = image(3, 1, &[BLACK, WHITE, BLACK]);
    let options = CompareOptions::default();
    let result = compare(&left, &right, &options, &NeverCancel).unwrap();

    assert_eq!(result.mask.get(0, 0), Some(PixelClass::Same));
    assert_eq!(result.mask.get(1, 0), Some(PixelClass::Same));
    assert_eq!(result.mask.get(2, 0), Some(PixelClass::Different));

    // A match tints white, so it renders as the source brightness in gray.
    assert_eq!(result.image.pixel(0, 0), Some([0, 0, 0, 255]));
    assert_eq!(result.image.pixel(1, 0), Some([255, 255, 255, 255]));
    // An important difference tints red at the brightness of the left pixel.
    assert_eq!(result.image.pixel(2, 0), Some([255, 0, 0, 255]));

    assert_eq!(result.totals.same, 2);
    assert_eq!(result.totals.different, 1);
    assert!((result.totals.percent_different(false) - 100.0 / 3.0).abs() < 1e-9);
}

#[test]
fn a_tolerance_of_zero_treats_one_step_as_important() {
    let left = solid(1, 1, [100, 100, 100, 255]);
    let right = solid(1, 1, [101, 100, 100, 255]);
    let options = CompareOptions {
        tolerance: 0,
        ..CompareOptions::default()
    };
    let result = compare(&left, &right, &options, &NeverCancel).unwrap();
    assert_eq!(result.mask.get(0, 0), Some(PixelClass::Different));
    assert_eq!(result.totals.different, 1);
}

#[test]
fn a_tolerance_of_one_treats_one_step_as_unimportant() {
    let left = solid(1, 1, [100, 100, 100, 255]);
    let right = solid(1, 1, [101, 100, 100, 255]);
    let options = CompareOptions {
        tolerance: 1,
        ..CompareOptions::default()
    };
    let result = compare(&left, &right, &options, &NeverCancel).unwrap();
    assert_eq!(result.mask.get(0, 0), Some(PixelClass::Similar));
    assert_eq!(result.totals.similar, 1);
    assert!(!result.totals.is_identical(false));
    assert!(result.totals.is_identical(true));
    // The unimportant difference tints blue while it counts as a difference.
    assert_eq!(result.image.pixel(0, 0), Some([0, 0, 100, 255]));
}

#[test]
fn a_tolerance_of_two_hundred_fifty_five_accepts_every_difference() {
    let left = solid(1, 1, BLACK);
    let right = solid(1, 1, WHITE);
    let options = CompareOptions {
        tolerance: 255,
        ..CompareOptions::default()
    };
    let result = compare(&left, &right, &options, &NeverCancel).unwrap();
    assert_eq!(result.mask.get(0, 0), Some(PixelClass::Similar));
    assert_eq!(result.totals.different, 0);
}

#[test]
fn equal_pixels_stay_matches_at_the_widest_tolerance() {
    let left = solid(2, 2, [7, 8, 9, 10]);
    let options = CompareOptions {
        tolerance: 255,
        ..CompareOptions::default()
    };
    let result = compare(&left, &left, &options, &NeverCancel).unwrap();
    assert_eq!(result.totals.same, 4);
    assert_eq!(result.totals.similar, 0);
}

#[test]
fn ignoring_unimportant_differences_renders_them_as_matches() {
    let left = solid(1, 1, [100, 100, 100, 255]);
    let right = solid(1, 1, [102, 100, 100, 255]);
    let options = CompareOptions {
        tolerance: 4,
        ignore_unimportant: true,
        ..CompareOptions::default()
    };
    let result = compare(&left, &right, &options, &NeverCancel).unwrap();
    assert_eq!(result.mask.get(0, 0), Some(PixelClass::Similar));
    // The tint is the match tint, so the pixel renders gray, not blue.
    assert_eq!(result.image.pixel(0, 0), Some([100, 100, 100, 255]));
    assert!(result.totals.percent_different(true).abs() < f64::EPSILON);
}

#[test]
fn mismatch_range_mode_encodes_the_magnitude() {
    let left = image(3, 1, &[BLACK, [10, 0, 0, 255], BLACK]);
    let right = image(3, 1, &[BLACK, [40, 0, 0, 255], WHITE]);
    let options = CompareOptions {
        mode: DisplayMode::MismatchRange,
        ..CompareOptions::default()
    };
    let result = compare(&left, &right, &options, &NeverCancel).unwrap();
    assert_eq!(result.image.pixel(0, 0), Some([0, 0, 0, 255]));
    assert_eq!(result.image.pixel(1, 0), Some([30, 30, 0, 255]));
    assert_eq!(result.image.pixel(2, 0), Some([255, 255, 0, 255]));
}

#[test]
fn blend_mode_weights_the_left_image() {
    let left = solid(1, 1, [100, 0, 0, 255]);
    let right = solid(1, 1, [0, 200, 0, 255]);
    let weights = [
        (100u8, [100, 0, 0, 255]),
        (0, [0, 200, 0, 255]),
        (50, [50, 100, 0, 255]),
    ];
    for (percent, expected) in weights {
        let options = CompareOptions {
            mode: DisplayMode::Blend,
            blend_percent: percent,
            ..CompareOptions::default()
        };
        let result = compare(&left, &right, &options, &NeverCancel).unwrap();
        assert_eq!(result.image.pixel(0, 0), Some(expected), "at {percent}");
    }
}

#[test]
fn single_side_mode_shows_the_chosen_picture() {
    let left = solid(1, 1, [1, 2, 3, 255]);
    let right = solid(1, 1, [4, 5, 6, 255]);
    for (side, expected) in [(Side::Left, [1, 2, 3, 255]), (Side::Right, [4, 5, 6, 255])] {
        let options = CompareOptions {
            mode: DisplayMode::SingleSide,
            side,
            ..CompareOptions::default()
        };
        let result = compare(&left, &right, &options, &NeverCancel).unwrap();
        assert_eq!(result.image.pixel(0, 0), Some(expected));
    }
    assert_eq!(Side::Left.other(), Side::Right);
}

#[test]
fn channel_modes_combine_the_two_sides() {
    let left = solid(1, 1, [0b1010_1010, 10, 200, 255]);
    let right = solid(1, 1, [0b0000_1111, 40, 100, 255]);

    let difference = CompareOptions {
        mode: DisplayMode::ChannelDifference,
        ..CompareOptions::default()
    };
    let result = compare(&left, &right, &difference, &NeverCancel).unwrap();
    assert_eq!(result.image.pixel(0, 0), Some([0b1001_1011, 30, 100, 0]));

    let xor = CompareOptions {
        mode: DisplayMode::ChannelXor,
        ..CompareOptions::default()
    };
    let result = compare(&left, &right, &xor, &NeverCancel).unwrap();
    assert_eq!(result.image.pixel(0, 0), Some([0b1010_0101, 34, 172, 0]));
}

#[test]
fn a_positive_offset_leaves_one_column_on_each_side() {
    let left = solid(2, 2, [1, 1, 1, 255]);
    let right = solid(2, 2, [1, 1, 1, 255]);
    let options = CompareOptions {
        offset: Offset::new(1, 0),
        ..CompareOptions::default()
    };
    let result = compare(&left, &right, &options, &NeverCancel).unwrap();
    assert_eq!((result.image.width(), result.image.height()), (3, 2));
    assert_eq!(result.origin, Offset::zero());
    assert_eq!(result.mask.get(0, 0), Some(PixelClass::LeftOnly));
    assert_eq!(result.mask.get(1, 0), Some(PixelClass::Same));
    assert_eq!(result.mask.get(2, 0), Some(PixelClass::RightOnly));
    assert_eq!(result.totals.left_only, 2);
    assert_eq!(result.totals.right_only, 2);
    assert_eq!(result.totals.same, 2);
}

#[test]
fn a_negative_offset_moves_the_result_origin() {
    let left = solid(2, 2, [1, 1, 1, 255]);
    let right = solid(2, 2, [1, 1, 1, 255]);
    let options = CompareOptions {
        offset: Offset::new(-1, -1),
        ..CompareOptions::default()
    };
    let result = compare(&left, &right, &options, &NeverCancel).unwrap();
    assert_eq!((result.image.width(), result.image.height()), (3, 3));
    assert_eq!(result.origin, Offset::new(-1, -1));
    assert_eq!(result.mask.get(0, 0), Some(PixelClass::RightOnly));
    assert_eq!(result.mask.get(1, 1), Some(PixelClass::Same));
    assert_eq!(result.mask.get(2, 2), Some(PixelClass::LeftOnly));
}

#[test]
fn an_offset_aligns_a_cropped_image() {
    let left = image(
        3,
        1,
        &[[9, 9, 9, 255], [10, 20, 30, 255], [40, 50, 60, 255]],
    );
    let right = image(2, 1, &[[10, 20, 30, 255], [40, 50, 60, 255]]);
    let options = CompareOptions {
        offset: Offset::new(1, 0),
        ..CompareOptions::default()
    };
    let result = compare(&left, &right, &options, &NeverCancel).unwrap();
    assert_eq!(result.totals.different, 0);
    assert_eq!(result.totals.same, 2);
    assert_eq!(result.totals.left_only, 1);
    assert_eq!(result.totals.right_only, 0);
}

#[test]
fn different_sizes_without_an_offset_cover_the_union() {
    let left = solid(4, 1, [1, 1, 1, 255]);
    let right = solid(1, 3, [1, 1, 1, 255]);
    let result = compare(&left, &right, &CompareOptions::default(), &NeverCancel).unwrap();
    assert_eq!((result.image.width(), result.image.height()), (4, 3));
    assert_eq!(result.totals.total(), 12);
    assert_eq!(result.totals.same, 1);
    assert_eq!(result.totals.left_only, 3);
    assert_eq!(result.totals.right_only, 2);
    // The bounding box is larger than the union of the two placed images, so
    // the corner covered by neither is counted apart and left transparent.
    assert_eq!(result.totals.uncovered, 6);
    assert_eq!(result.totals.compared_total(), 6);
    assert_eq!(result.image.pixel(3, 2), Some([0, 0, 0, 0]));
    assert_eq!(result.mask.get(3, 2), Some(PixelClass::Uncovered));
}

#[test]
fn an_offset_step_saturates_rather_than_wrapping() {
    assert_eq!(Offset::new(5, 5).nudged(-1, 2), Offset::new(4, 7));
    assert_eq!(Offset::new(i32::MAX, 0).nudged(1, 0).x, i32::MAX);
}

#[test]
fn ignoring_alpha_hides_a_transparency_difference() {
    let left = solid(1, 1, [10, 20, 30, 255]);
    let right = solid(1, 1, [10, 20, 30, 0]);

    let strict = compare(&left, &right, &CompareOptions::default(), &NeverCancel).unwrap();
    assert_eq!(strict.mask.get(0, 0), Some(PixelClass::Different));

    let options = CompareOptions {
        ignore_alpha: true,
        ..CompareOptions::default()
    };
    let relaxed = compare(&left, &right, &options, &NeverCancel).unwrap();
    assert_eq!(relaxed.mask.get(0, 0), Some(PixelClass::Same));
}

#[test]
fn two_invisible_pixels_with_different_hidden_colors_match() {
    let left = solid(1, 1, [255, 0, 0, 0]);
    let right = solid(1, 1, [0, 255, 0, 0]);

    let result = compare(&left, &right, &CompareOptions::default(), &NeverCancel).unwrap();
    assert_eq!(result.mask.get(0, 0), Some(PixelClass::Same));
    assert_eq!(result.totals.different, 0);

    for mode in [DisplayMode::Tolerance, DisplayMode::MismatchRange] {
        let options = CompareOptions {
            mode,
            ..CompareOptions::default()
        };
        let result = compare(&left, &right, &options, &NeverCancel).unwrap();
        assert_eq!(result.mask.get(0, 0), Some(PixelClass::Same), "{mode:?}");
    }

    // Mismatch range paints a match black, so nothing marks the pixel.
    let options = CompareOptions {
        mode: DisplayMode::MismatchRange,
        ..CompareOptions::default()
    };
    let result = compare(&left, &right, &options, &NeverCancel).unwrap();
    assert_eq!(result.image.pixel(0, 0), Some([0, 0, 0, 255]));

    let strict = CompareOptions {
        transparent_pixels_equal: false,
        ..CompareOptions::default()
    };
    let result = compare(&left, &right, &strict, &NeverCancel).unwrap();
    assert_eq!(result.mask.get(0, 0), Some(PixelClass::Different));
}

#[test]
fn an_invisible_pixel_still_differs_from_a_visible_one() {
    let left = solid(1, 1, [255, 0, 0, 0]);
    let right = solid(1, 1, [255, 0, 0, 255]);
    let result = compare(&left, &right, &CompareOptions::default(), &NeverCancel).unwrap();
    assert_eq!(result.mask.get(0, 0), Some(PixelClass::Different));
}

#[test]
fn a_replacement_makes_one_color_change_unimportant() {
    let left = image(2, 1, &[[255, 0, 0, 255], [0, 255, 0, 255]]);
    let right = image(2, 1, &[[0, 0, 255, 255], [0, 0, 255, 255]]);
    let options = CompareOptions {
        replacements: vec![Replacement {
            matched: [255, 0, 0, 255],
            replacement: [0, 0, 255, 255],
            ..Replacement::default()
        }],
        ..CompareOptions::default()
    };
    let result = compare(&left, &right, &options, &NeverCancel).unwrap();
    assert_eq!(result.mask.get(0, 0), Some(PixelClass::Similar));
    assert_eq!(result.mask.get(1, 0), Some(PixelClass::Different));
}

#[test]
fn the_mask_packs_two_pixels_to_the_byte() {
    let left = image(3, 2, &[BLACK, WHITE, BLACK, WHITE, BLACK, WHITE]);
    let right = image(3, 2, &[BLACK, BLACK, BLACK, WHITE, WHITE, WHITE]);
    let result = compare(&left, &right, &CompareOptions::default(), &NeverCancel).unwrap();
    assert_eq!(result.mask.stride(), 2);
    assert_eq!(result.mask.packed().len(), 4);
    assert_eq!(result.mask.get(0, 0), Some(PixelClass::Same));
    assert_eq!(result.mask.get(1, 0), Some(PixelClass::Different));
    assert_eq!(result.mask.get(2, 0), Some(PixelClass::Same));
    assert_eq!(result.mask.get(1, 1), Some(PixelClass::Different));
    assert_eq!(result.mask.get(3, 0), None);
    assert_eq!(result.mask.get(0, 2), None);
    assert_eq!((result.mask.width(), result.mask.height()), (3, 2));
}

#[test]
fn a_fired_cancel_stops_the_comparison() {
    let left = solid(64, 64, BLACK);
    let right = solid(64, 64, WHITE);
    let flag = CancelFlag::new();
    flag.cancel();
    let outcome = compare(&left, &right, &CompareOptions::default(), &flag);
    assert!(matches!(outcome, Err(Error::Cancelled)));

    flag.reset();
    assert!(!flag.is_cancelled());
    assert!(compare(&left, &right, &CompareOptions::default(), &flag).is_ok());
}

#[test]
fn a_blend_percentage_above_one_hundred_is_rejected() {
    let left = solid(1, 1, BLACK);
    let options = CompareOptions {
        blend_percent: 200,
        ..CompareOptions::default()
    };
    assert!(matches!(
        compare(&left, &left, &options, &NeverCancel),
        Err(Error::OutOfRange(_))
    ));
}

#[test]
fn a_result_above_the_limits_is_rejected() {
    let left = solid(4, 4, BLACK);
    let options = CompareOptions {
        result_limits: ca_image::Limits {
            max_width: 4,
            max_height: 4,
            ..ca_image::Limits::default()
        },
        offset: Offset::new(4, 0),
        ..CompareOptions::default()
    };
    assert!(matches!(
        compare(&left, &left, &options, &NeverCancel),
        Err(Error::ResultTooLarge { width: 8, .. })
    ));
}

#[test]
fn two_largest_images_still_compare_after_a_one_pixel_nudge() {
    // The images are not allocated: the size check runs on the result size,
    // before any buffer exists.
    let limits = ca_image::Limits::for_result();
    let edge = ca_image::Limits::default().max_width;
    assert!(limits.check_result(edge + 1, 2).is_ok());
    assert!(limits.check_result(edge * 2, 2).is_ok());
}

#[test]
fn totals_report_shares_that_sum_to_one_hundred() {
    let left = image(4, 1, &[BLACK, BLACK, WHITE, WHITE]);
    let right = image(4, 1, &[BLACK, WHITE, WHITE, BLACK]);
    let result = compare(&left, &right, &CompareOptions::default(), &NeverCancel).unwrap();
    assert_eq!(result.totals.same, 2);
    assert_eq!(result.totals.different, 2);
    assert!(
        (result.totals.percent_same(false) + result.totals.percent_different(false) - 100.0).abs()
            < 1e-9
    );
    assert_eq!(result.totals.difference_count(false), 2);
}
