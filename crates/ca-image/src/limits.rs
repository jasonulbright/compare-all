//! Upper bounds checked before any pixel buffer is allocated.
//!
//! Two sets exist. The decode set bounds one decoded image. The result set
//! bounds a comparison result, which covers the bounding box of two placed
//! images and is therefore larger than either input.

use crate::error::{Error, Result};
use crate::settings::provisional;

/// Bytes one RGBA8 pixel occupies.
const RGBA_BYTES_PER_PIXEL: u64 = 4;

/// Upper bounds applied to an image before it is decoded or built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Greatest accepted width in pixels.
    pub max_width: u32,
    /// Greatest accepted height in pixels.
    pub max_height: u32,
    /// Greatest accepted pixel count.
    pub max_pixels: u64,
    /// Greatest accepted peak buffer size in bytes. A decode that holds the
    /// native buffer and the RGBA8 buffer at the same time is measured as the
    /// sum of the two.
    pub max_decoded_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_width: provisional::MAX_WIDTH,
            max_height: provisional::MAX_HEIGHT,
            max_pixels: provisional::MAX_PIXELS,
            max_decoded_bytes: provisional::MAX_DECODED_BYTES,
        }
    }
}

impl Limits {
    /// The bounds a comparison result is checked against.
    ///
    /// They stand above the decode bounds, because the result covers the
    /// bounding box of two placed images: two images of the greatest accepted
    /// size, one nudged by a single pixel, still produce a result that fits.
    #[must_use]
    pub fn for_result() -> Self {
        Self {
            max_width: provisional::MAX_RESULT_WIDTH,
            max_height: provisional::MAX_RESULT_HEIGHT,
            max_pixels: provisional::MAX_RESULT_PIXELS,
            max_decoded_bytes: provisional::MAX_RESULT_BYTES,
        }
    }

    /// Rejects a declared image size that is above any of the limits.
    ///
    /// # Errors
    /// Returns [`Error::EmptyImage`], [`Error::DimensionsTooLarge`] or
    /// [`Error::TooLarge`] according to which bound the size crosses.
    pub fn check(&self, width: u32, height: u32) -> Result<()> {
        if width == 0 || height == 0 {
            return Err(Error::EmptyImage);
        }
        if width > self.max_width || height > self.max_height {
            return Err(Error::DimensionsTooLarge {
                width,
                height,
                limit_width: self.max_width,
                limit_height: self.max_height,
            });
        }
        let pixels = u64::from(width) * u64::from(height);
        if pixels > self.max_pixels {
            return Err(Error::TooLarge {
                needed: pixels,
                limit: self.max_pixels,
                unit: "pixels",
            });
        }
        self.check_bytes(pixels.saturating_mul(RGBA_BYTES_PER_PIXEL))
    }

    /// Rejects a byte count above the byte limit.
    ///
    /// # Errors
    /// Returns [`Error::TooLarge`] when `needed` is above the limit.
    pub fn check_bytes(&self, needed: u64) -> Result<()> {
        if needed > self.max_decoded_bytes {
            return Err(Error::TooLarge {
                needed,
                limit: self.max_decoded_bytes,
                unit: "bytes",
            });
        }
        Ok(())
    }

    /// Rejects a comparison result size that is above any of the limits.
    ///
    /// The variant differs from [`Limits::check`] so that a caller can tell a
    /// result that does not fit from a file that does not decode.
    ///
    /// # Errors
    /// Returns [`Error::EmptyImage`] for a zero size and
    /// [`Error::ResultTooLarge`] for a size above any bound.
    pub fn check_result(&self, width: u32, height: u32) -> Result<()> {
        match self.check(width, height) {
            Err(Error::EmptyImage) => Err(Error::EmptyImage),
            Err(_) => Err(Error::ResultTooLarge {
                width,
                height,
                limit_pixels: self.max_pixels,
                limit_bytes: self.max_decoded_bytes,
            }),
            Ok(()) => Ok(()),
        }
    }

    /// The same bounds expressed for the decoder's own internal guard.
    ///
    /// `max_alloc` covers one buffer inside the decoder, so it is set to the
    /// same budget the sum check uses. The sum check runs first and is the
    /// stricter of the two for a format that decodes into a native buffer.
    pub(crate) fn as_image_limits(self) -> image::Limits {
        let mut limits = image::Limits::no_limits();
        limits.max_image_width = Some(self.max_width);
        limits.max_image_height = Some(self.max_height);
        limits.max_alloc = Some(self.max_decoded_bytes);
        limits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_reject_a_zero_dimension() {
        let limits = Limits::default();
        assert!(matches!(limits.check(0, 8), Err(Error::EmptyImage)));
    }

    #[test]
    fn limits_reject_an_oversized_width() {
        let limits = Limits {
            max_width: 16,
            ..Limits::default()
        };
        assert!(matches!(
            limits.check(17, 4),
            Err(Error::DimensionsTooLarge { width: 17, .. })
        ));
    }

    #[test]
    fn limits_reject_an_oversized_pixel_count() {
        let limits = Limits {
            max_pixels: 10,
            ..Limits::default()
        };
        assert!(matches!(
            limits.check(4, 4),
            Err(Error::TooLarge { unit: "pixels", .. })
        ));
    }

    #[test]
    fn limits_reject_an_oversized_byte_count() {
        let limits = Limits {
            max_decoded_bytes: 32,
            ..Limits::default()
        };
        assert!(matches!(
            limits.check(4, 4),
            Err(Error::TooLarge { unit: "bytes", .. })
        ));
    }

    #[test]
    fn a_result_size_above_the_limits_reports_its_own_variant() {
        let limits = Limits {
            max_width: 4,
            ..Limits::default()
        };
        assert!(matches!(
            limits.check_result(5, 4),
            Err(Error::ResultTooLarge { width: 5, .. })
        ));
        assert!(matches!(limits.check_result(0, 4), Err(Error::EmptyImage)));
    }

    #[test]
    fn the_result_bounds_hold_two_largest_images_nudged_by_one_pixel() {
        let decode = Limits::default();
        let result = Limits::for_result();
        assert!(result.max_width >= decode.max_width.saturating_add(1));
        assert!(result.max_height >= decode.max_height.saturating_add(1));
        assert!(result.max_pixels > decode.max_pixels);
        assert!(result.max_decoded_bytes > decode.max_decoded_bytes);
    }
}
