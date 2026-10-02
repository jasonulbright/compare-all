//! Rotation, reflection and resampling of RGBA8 buffers.
//!
//! These back the view commands that align image content before a comparison:
//! quarter turns, reflections, and the scaling that makes the smaller image
//! share the scale of the larger one.

use crate::buffer::{RgbaImage, BYTES_PER_PIXEL};
use crate::error::{Error, Result};
use crate::limits::Limits;
use crate::settings::provisional;

/// A quarter turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rotation {
    /// A quarter turn clockwise.
    Clockwise90,
    /// A half turn.
    Half180,
    /// A quarter turn counterclockwise.
    Counterclockwise90,
}

/// How a resample picks its samples.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Filter {
    /// Takes the nearest source pixel. Keeps hard edges and exact colors.
    #[default]
    Nearest,
    /// Averages the four surrounding source pixels.
    Bilinear,
}

/// Rotates an image by a quarter or half turn.
///
/// # Errors
/// Returns an allocation error from [`RgbaImage::new`] for the rotated size.
pub fn rotate(source: &RgbaImage, rotation: Rotation) -> Result<RgbaImage> {
    let (width, height) = match rotation {
        Rotation::Half180 => (source.width(), source.height()),
        Rotation::Clockwise90 | Rotation::Counterclockwise90 => (source.height(), source.width()),
    };
    let mut target = RgbaImage::new(width, height)?;
    for y in 0..source.height() {
        for x in 0..source.width() {
            let Some(pixel) = source.pixel(x, y) else {
                continue;
            };
            let (tx, ty) = match rotation {
                // The target width is the source height, so `height - 1 - y`
                // stays inside the target on a quarter turn.
                Rotation::Clockwise90 => (source.height() - 1 - y, x),
                Rotation::Counterclockwise90 => (y, source.width() - 1 - x),
                Rotation::Half180 => (source.width() - 1 - x, source.height() - 1 - y),
            };
            target.set_pixel(tx, ty, pixel);
        }
    }
    Ok(target)
}

/// Reflects an image across its vertical axis.
///
/// # Errors
/// Returns an allocation error from [`RgbaImage::new`].
pub fn flip_horizontal(source: &RgbaImage) -> Result<RgbaImage> {
    let mut target = RgbaImage::new(source.width(), source.height())?;
    for y in 0..source.height() {
        for x in 0..source.width() {
            if let Some(pixel) = source.pixel(x, y) {
                target.set_pixel(source.width() - 1 - x, y, pixel);
            }
        }
    }
    Ok(target)
}

/// Reflects an image across its horizontal axis.
///
/// # Errors
/// Returns an allocation error from [`RgbaImage::new`].
pub fn flip_vertical(source: &RgbaImage) -> Result<RgbaImage> {
    let mut target = RgbaImage::new(source.width(), source.height())?;
    let stride = source.row_stride();
    for y in 0..source.height() {
        let Some(row) = source.row(y) else { continue };
        let flipped = source.height() - 1 - y;
        let start = flipped as usize * stride;
        if let Some(slot) = target.pixels_mut().get_mut(start..start + stride) {
            slot.copy_from_slice(row);
        }
    }
    Ok(target)
}

/// Resamples an image to a new size, under the default limits.
///
/// # Errors
/// Returns [`Error::EmptyImage`] for a zero target size and
/// [`Error::TooLarge`] when the target size crosses the limits.
pub fn resize(source: &RgbaImage, width: u32, height: u32, filter: Filter) -> Result<RgbaImage> {
    resize_within(source, width, height, filter, &Limits::default())
}

/// Resamples an image to a new size, under the given limits.
///
/// The target size comes from the caller, so it is checked before the target
/// buffer is allocated.
///
/// # Errors
/// As [`resize`].
pub fn resize_within(
    source: &RgbaImage,
    width: u32,
    height: u32,
    filter: Filter,
    limits: &Limits,
) -> Result<RgbaImage> {
    limits.check(width, height)?;
    match filter {
        Filter::Nearest => resize_nearest(source, width, height, limits),
        Filter::Bilinear => resize_bilinear(source, width, height, limits),
    }
}

fn resize_nearest(
    source: &RgbaImage,
    width: u32,
    height: u32,
    limits: &Limits,
) -> Result<RgbaImage> {
    let mut target = RgbaImage::new_within(width, height, limits)?;
    for y in 0..height {
        let sy = map_index(y, height, source.height());
        for x in 0..width {
            let sx = map_index(x, width, source.width());
            if let Some(pixel) = source.pixel(sx, sy) {
                target.set_pixel(x, y, pixel);
            }
        }
    }
    Ok(target)
}

fn resize_bilinear(
    source: &RgbaImage,
    width: u32,
    height: u32,
    limits: &Limits,
) -> Result<RgbaImage> {
    let mut target = RgbaImage::new_within(width, height, limits)?;
    for y in 0..height {
        let (sy, fy) = map_fraction(y, height, source.height());
        let sy1 = (sy + 1).min(source.height() - 1);
        for x in 0..width {
            let (sx, fx) = map_fraction(x, width, source.width());
            let sx1 = (sx + 1).min(source.width() - 1);
            let corners = [
                source.pixel(sx, sy),
                source.pixel(sx1, sy),
                source.pixel(sx, sy1),
                source.pixel(sx1, sy1),
            ];
            let mut pixel = [0u8; BYTES_PER_PIXEL];
            for (channel, slot) in pixel.iter_mut().enumerate() {
                let top = lerp(
                    corners[0].map_or(0, |p| p[channel]),
                    corners[1].map_or(0, |p| p[channel]),
                    fx,
                );
                let bottom = lerp(
                    corners[2].map_or(0, |p| p[channel]),
                    corners[3].map_or(0, |p| p[channel]),
                    fx,
                );
                *slot = lerp(top, bottom, fy);
            }
            target.set_pixel(x, y, pixel);
        }
    }
    Ok(target)
}

/// Enlarges whichever image is smaller so that both share one scale, under the
/// default limits.
///
/// # Errors
/// As [`auto_scale_within`].
pub fn auto_scale(
    left: &RgbaImage,
    right: &RgbaImage,
    filter: Filter,
) -> Result<(RgbaImage, RgbaImage)> {
    auto_scale_within(left, right, filter, &Limits::default())
}

/// Enlarges whichever image is smaller so that both share one scale, under the
/// given limits.
///
/// The smaller image grows by one uniform factor, so its aspect ratio does not
/// change. The factor is the one that makes the smaller image reach the larger
/// one on the axis that needs the most growth, so the smaller image matches the
/// larger one on at least one axis and is never below it on the other. The
/// operation only enlarges: a factor at or below one leaves both images
/// unchanged, and no dimension falls below one pixel.
///
/// # Errors
/// Returns [`Error::TooLarge`] when the enlarged size crosses `limits`.
pub fn auto_scale_within(
    left: &RgbaImage,
    right: &RgbaImage,
    filter: Filter,
    limits: &Limits,
) -> Result<(RgbaImage, RgbaImage)> {
    let left_pixels = u64::from(left.width()) * u64::from(left.height());
    let right_pixels = u64::from(right.width()) * u64::from(right.height());
    if left_pixels == right_pixels {
        return Ok((left.clone(), right.clone()));
    }
    let (small, large, small_is_left) = if left_pixels < right_pixels {
        (left, right, true)
    } else {
        (right, left, false)
    };
    let (width, height) = fitted_size(small, large);
    if (width, height) == (small.width(), small.height()) {
        return Ok((left.clone(), right.clone()));
    }
    let scaled = resize_within(small, width, height, filter, limits)?;
    if small_is_left {
        Ok((scaled, right.clone()))
    } else {
        Ok((left.clone(), scaled))
    }
}

/// The size the smaller image takes when one uniform factor enlarges it to the
/// larger one's scale.
///
/// The factor is the greater of the two axis ratios, so neither axis shrinks.
/// A factor at or below one returns the original size.
fn fitted_size(small: &RgbaImage, large: &RgbaImage) -> (u32, u32) {
    let small_width = u64::from(small.width());
    let small_height = u64::from(small.height());
    let by_width = (u64::from(large.width()), small_width);
    let by_height = (u64::from(large.height()), small_height);
    // Compare the two ratios without division: a/b >= c/d is a*d >= c*b.
    let (numerator, denominator) = if by_width.0 * by_height.1 >= by_height.0 * by_width.1 {
        by_width
    } else {
        by_height
    };
    if denominator == 0 || numerator <= denominator {
        return (small.width(), small.height());
    }
    let scaled = |value: u64| -> u32 {
        let product = value.saturating_mul(numerator);
        let rounded = product.saturating_add(denominator / 2) / denominator;
        clamp_dimension(rounded)
    };
    (scaled(small_width), scaled(small_height))
}

fn clamp_dimension(value: u64) -> u32 {
    u32::try_from(value.max(1)).unwrap_or(u32::MAX)
}

/// Composites an image over a checkerboard so that transparent regions become
/// visible.
///
/// # Errors
/// Returns [`Error::OutOfRange`] for a zero square size and an allocation
/// error from [`RgbaImage::new`].
pub fn composite_over_checkerboard(source: &RgbaImage, square: u32) -> Result<RgbaImage> {
    if square == 0 {
        return Err(Error::OutOfRange(
            "the checkerboard square size is zero".to_owned(),
        ));
    }
    let mut target = RgbaImage::new(source.width(), source.height())?;
    for y in 0..source.height() {
        for x in 0..source.width() {
            let light = ((x / square) + (y / square)).is_multiple_of(2);
            let background = if light {
                provisional::CHECKER_LIGHT
            } else {
                provisional::CHECKER_DARK
            };
            let pixel = source.pixel(x, y).unwrap_or([0, 0, 0, 0]);
            target.set_pixel(x, y, over(pixel, background));
        }
    }
    Ok(target)
}

/// Source over destination, with both sides not premultiplied.
fn over(source: [u8; 4], background: [u8; 4]) -> [u8; 4] {
    let alpha = u32::from(source[3]);
    let inverse = 255 - alpha;
    let mix = |s: u8, b: u8| -> u8 {
        let value = (u32::from(s) * alpha + u32::from(b) * inverse) / 255;
        u8::try_from(value).unwrap_or(u8::MAX)
    };
    [
        mix(source[0], background[0]),
        mix(source[1], background[1]),
        mix(source[2], background[2]),
        u8::MAX,
    ]
}

/// The source index a target index samples, for nearest neighbour.
fn map_index(target: u32, target_span: u32, source_span: u32) -> u32 {
    let numerator = u64::from(target) * u64::from(source_span);
    let index = numerator / u64::from(target_span.max(1));
    u32::try_from(index)
        .unwrap_or(u32::MAX)
        .min(source_span.saturating_sub(1))
}

/// The source index and the fraction past it a target index samples.
///
/// The fraction is expressed in 1/256 steps so that the interpolation stays in
/// integer arithmetic.
fn map_fraction(target: u32, target_span: u32, source_span: u32) -> (u32, u32) {
    let scaled = u64::from(target) * u64::from(source_span) * 256;
    let position = scaled / u64::from(target_span.max(1));
    let index = position / 256;
    let fraction = position % 256;
    let index = u32::try_from(index)
        .unwrap_or(u32::MAX)
        .min(source_span.saturating_sub(1));
    (index, u32::try_from(fraction).unwrap_or(0))
}

/// Interpolates two channel values, with `fraction` in 1/256 steps.
fn lerp(start: u8, end: u8, fraction: u32) -> u8 {
    let value = (u32::from(start) * (256 - fraction) + u32::from(end) * fraction) / 256;
    u8::try_from(value).unwrap_or(u8::MAX)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn ramp(width: u32, height: u32) -> RgbaImage {
        let mut image = RgbaImage::new(width, height).unwrap();
        for y in 0..height {
            for x in 0..width {
                let value = u8::try_from((y * width + x) % 256).unwrap_or(0);
                image.set_pixel(x, y, [value, value, value, 255]);
            }
        }
        image
    }

    #[test]
    fn four_quarter_turns_return_the_original() {
        let source = ramp(3, 2);
        let mut turned = source.clone();
        for _ in 0..4 {
            turned = rotate(&turned, Rotation::Clockwise90).unwrap();
        }
        assert_eq!(turned, source);
    }

    #[test]
    fn a_quarter_turn_swaps_the_dimensions() {
        let source = ramp(3, 2);
        let turned = rotate(&source, Rotation::Clockwise90).unwrap();
        assert_eq!((turned.width(), turned.height()), (2, 3));
        assert_eq!(turned.pixel(1, 0), source.pixel(0, 0));
    }

    #[test]
    fn two_flips_return_the_original() {
        let source = ramp(4, 3);
        let flipped = flip_horizontal(&flip_horizontal(&source).unwrap()).unwrap();
        assert_eq!(flipped, source);
        let flipped = flip_vertical(&flip_vertical(&source).unwrap()).unwrap();
        assert_eq!(flipped, source);
    }

    #[test]
    fn a_nearest_resample_doubles_each_pixel() {
        let source = ramp(2, 1);
        let scaled = resize(&source, 4, 1, Filter::Nearest).unwrap();
        assert_eq!(scaled.pixel(0, 0), source.pixel(0, 0));
        assert_eq!(scaled.pixel(1, 0), source.pixel(0, 0));
        assert_eq!(scaled.pixel(2, 0), source.pixel(1, 0));
        assert_eq!(scaled.pixel(3, 0), source.pixel(1, 0));
    }

    #[test]
    fn a_bilinear_resample_keeps_a_flat_image_flat() {
        let source = RgbaImage::filled(3, 3, [40, 50, 60, 255]).unwrap();
        let scaled = resize(&source, 7, 5, Filter::Bilinear).unwrap();
        for y in 0..scaled.height() {
            for x in 0..scaled.width() {
                assert_eq!(scaled.pixel(x, y), Some([40, 50, 60, 255]));
            }
        }
    }

    #[test]
    fn auto_scale_enlarges_only_the_smaller_image() {
        let small = RgbaImage::filled(2, 2, [1, 1, 1, 255]).unwrap();
        let large = RgbaImage::filled(6, 6, [2, 2, 2, 255]).unwrap();
        let (left, right) = auto_scale(&small, &large, Filter::Nearest).unwrap();
        assert_eq!((left.width(), left.height()), (6, 6));
        assert_eq!((right.width(), right.height()), (6, 6));
    }

    #[test]
    fn auto_scale_never_shrinks_an_image_with_the_opposite_aspect_ratio() {
        let tall = RgbaImage::filled(100, 1000, [1, 1, 1, 255]).unwrap();
        let wide = RgbaImage::filled(4000, 300, [2, 2, 2, 255]).unwrap();
        let (left, right) = auto_scale(&tall, &wide, Filter::Nearest).unwrap();
        assert_eq!((right.width(), right.height()), (4000, 300));
        assert!(left.width() >= tall.width(), "the width shrank");
        assert!(left.height() >= tall.height(), "the height shrank");
        // One uniform factor of forty reaches the larger image's width.
        assert_eq!((left.width(), left.height()), (4000, 40_000));
    }

    #[test]
    fn auto_scale_keeps_a_thin_image_at_full_height() {
        let thin = RgbaImage::filled(1, 9, [1, 1, 1, 255]).unwrap();
        let flat = RgbaImage::filled(100, 1, [2, 2, 2, 255]).unwrap();
        let (left, right) = auto_scale(&thin, &flat, Filter::Nearest).unwrap();
        assert_eq!((right.width(), right.height()), (100, 1));
        assert_eq!((left.width(), left.height()), (100, 900));
    }

    #[test]
    fn auto_scale_reports_a_size_above_the_limits() {
        let thin = RgbaImage::filled(1, 9, [1, 1, 1, 255]).unwrap();
        let flat = RgbaImage::filled(100, 1, [2, 2, 2, 255]).unwrap();
        let limits = Limits {
            max_pixels: 1024,
            ..Limits::default()
        };
        let outcome = auto_scale_within(&thin, &flat, Filter::Nearest, &limits);
        assert!(matches!(outcome, Err(Error::TooLarge { .. })));
    }

    #[test]
    fn a_checkerboard_shows_through_a_transparent_pixel() {
        let source = RgbaImage::filled(2, 1, [0, 0, 0, 0]).unwrap();
        let composited = composite_over_checkerboard(&source, 1).unwrap();
        assert_eq!(composited.pixel(0, 0), Some(provisional::CHECKER_LIGHT));
        assert_eq!(composited.pixel(1, 0), Some(provisional::CHECKER_DARK));
    }
}
