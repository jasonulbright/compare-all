//! Property tests: mutated encoded input never aborts the process, and the
//! comparison invariants hold for arbitrary small images.

#![allow(clippy::unwrap_used)]

mod fixtures;

use ca_image::{
    compare, decode_bytes_as, CompareOptions, DecodeOptions, DisplayMode, NeverCancel, Offset,
    RgbaImage, SourceFormat,
};
use proptest::prelude::*;

fn encoded_cases() -> Vec<(&'static str, Vec<u8>)> {
    fixtures::all_encoded(12, 9)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Flipping bytes of a valid file must end in a value or an error, never
    /// in a panic or an abort.
    #[test]
    fn a_mutated_file_never_panics(
        case in 0usize..8,
        position in 0usize..4096,
        mask in 1u8..=255,
    ) {
        let cases = encoded_cases();
        let (name, bytes) = &cases[case % cases.len()];
        let mut damaged = bytes.clone();
        let index = position % damaged.len();
        damaged[index] ^= mask;
        let named = SourceFormat::from_extension(name);
        let outcome = decode_bytes_as(&damaged, named.as_ref(), &DecodeOptions::default());
        prop_assert!(outcome.is_ok() || outcome.is_err());
    }

    /// Truncating a valid file at any point must end in a value or an error.
    #[test]
    fn a_truncated_file_never_panics(case in 0usize..8, keep in 0usize..4096) {
        let cases = encoded_cases();
        let (name, bytes) = &cases[case % cases.len()];
        let cut = keep % (bytes.len() + 1);
        let named = SourceFormat::from_extension(name);
        let outcome = decode_bytes_as(&bytes[..cut], named.as_ref(), &DecodeOptions::default());
        prop_assert!(outcome.is_ok() || outcome.is_err());
    }

    /// Random bytes must never decode into an image larger than the limits.
    #[test]
    fn arbitrary_bytes_stay_within_the_limits(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
        if let Ok(decoded) = decode_bytes_as(&bytes, None, &DecodeOptions::default()) {
            let limits = ca_image::Limits::default();
            prop_assert!(decoded.image.width() <= limits.max_width);
            prop_assert!(decoded.image.height() <= limits.max_height);
        }
    }

    /// The counters always cover every result pixel exactly once.
    #[test]
    fn the_counters_cover_the_whole_result(
        left_pixels in proptest::collection::vec(any::<[u8; 4]>(), 1..17),
        right_pixels in proptest::collection::vec(any::<[u8; 4]>(), 1..17),
        tolerance in any::<u8>(),
        offset_x in -8i32..8,
        offset_y in -8i32..8,
    ) {
        let left = flat(&left_pixels);
        let right = flat(&right_pixels);
        let options = CompareOptions {
            tolerance,
            offset: Offset::new(offset_x, offset_y),
            ..CompareOptions::default()
        };
        let result = compare(&left, &right, &options, &NeverCancel).unwrap();
        let pixels = u64::from(result.image.width()) * u64::from(result.image.height());
        prop_assert_eq!(result.totals.total(), pixels);
        prop_assert!(result.totals.percent_different(false) <= 100.0);
        prop_assert!(result.totals.percent_same(false) >= 0.0);
    }

    /// Comparing an image with itself finds no difference in any mode.
    #[test]
    fn an_image_never_differs_from_itself(
        pixels in proptest::collection::vec(any::<[u8; 4]>(), 1..17),
        mode in 0usize..6,
    ) {
        let image = flat(&pixels);
        let modes = [
            DisplayMode::Tolerance,
            DisplayMode::MismatchRange,
            DisplayMode::Blend,
            DisplayMode::SingleSide,
            DisplayMode::ChannelDifference,
            DisplayMode::ChannelXor,
        ];
        let options = CompareOptions {
            mode: modes[mode],
            ..CompareOptions::default()
        };
        let result = compare(&image, &image, &options, &NeverCancel).unwrap();
        prop_assert!(result.totals.is_identical(false));
        prop_assert_eq!(result.totals.same, u64::from(image.width()) * u64::from(image.height()));
    }
}

fn flat(pixels: &[[u8; 4]]) -> RgbaImage {
    let mut bytes = Vec::with_capacity(pixels.len() * 4);
    for pixel in pixels {
        bytes.extend_from_slice(pixel);
    }
    let width = u32::try_from(pixels.len()).unwrap_or(1);
    RgbaImage::from_pixels(width, 1, bytes).unwrap()
}
