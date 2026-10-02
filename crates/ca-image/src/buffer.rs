//! The 8-bit RGBA pixel buffer every other module operates on.
//!
//! Every allocation here reserves fallibly and reports [`Error::TooLarge`]
//! instead of aborting the process, and every constructor that takes
//! caller-supplied dimensions checks them against a [`Limits`] value before it
//! reserves.

use crate::error::{Error, Result};
use crate::limits::Limits;
use std::fmt;

/// Bytes in one pixel: red, green, blue, alpha.
pub const BYTES_PER_PIXEL: usize = 4;

/// One image in 8-bit RGBA, row major, four bytes per pixel and no row
/// padding.
///
/// The constructors are the only place that checks the relation between the
/// dimensions and the buffer length. Every accessor below relies on the
/// invariant they establish: `pixels.len() == width * height * 4`, and the
/// product fits in `usize`.
#[derive(Clone, PartialEq, Eq)]
pub struct RgbaImage {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl fmt::Debug for RgbaImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RgbaImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("bytes", &self.pixels.len())
            .finish()
    }
}

impl RgbaImage {
    /// A fully transparent image of the given size, under the default decode
    /// limits.
    ///
    /// # Errors
    /// Returns [`Error::EmptyImage`] when either dimension is zero, and
    /// [`Error::TooLarge`] when the size crosses the limits or the allocation
    /// fails.
    pub fn new(width: u32, height: u32) -> Result<Self> {
        Self::new_within(width, height, &Limits::default())
    }

    /// A fully transparent image of the given size, under the given limits.
    ///
    /// # Errors
    /// As [`RgbaImage::new`].
    pub fn new_within(width: u32, height: u32, limits: &Limits) -> Result<Self> {
        limits.check(width, height)?;
        let len = byte_len(width, height)?;
        Ok(Self {
            width,
            height,
            pixels: try_zeroed(len)?,
        })
    }

    /// An image whose every pixel is `color`, under the default decode limits.
    ///
    /// # Errors
    /// As [`RgbaImage::new`].
    pub fn filled(width: u32, height: u32, color: [u8; 4]) -> Result<Self> {
        Self::filled_within(width, height, color, &Limits::default())
    }

    /// An image whose every pixel is `color`, under the given limits.
    ///
    /// # Errors
    /// As [`RgbaImage::new`].
    pub fn filled_within(width: u32, height: u32, color: [u8; 4], limits: &Limits) -> Result<Self> {
        limits.check(width, height)?;
        let len = byte_len(width, height)?;
        let mut pixels = try_reserved(len)?;
        for _ in 0..(len / BYTES_PER_PIXEL) {
            pixels.extend_from_slice(&color);
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    /// An image wrapping an existing RGBA8 buffer.
    ///
    /// # Errors
    /// Returns [`Error::EmptyImage`] when either dimension is zero, and
    /// [`Error::BufferLength`] when the buffer does not hold exactly
    /// `width * height` pixels.
    pub fn from_pixels(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self> {
        let len = byte_len(width, height)?;
        if pixels.len() != len {
            return Err(Error::BufferLength {
                len: pixels.len(),
                width,
                height,
            });
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    /// Width in pixels. Always at least one.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels. Always at least one.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Bytes in one row of pixels.
    #[must_use]
    pub fn row_stride(&self) -> usize {
        self.width as usize * BYTES_PER_PIXEL
    }

    /// The whole buffer, row major.
    #[must_use]
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// The whole buffer for in-place work. The length never changes through
    /// this reference, so the size invariant holds.
    #[must_use]
    pub fn pixels_mut(&mut self) -> &mut [u8] {
        &mut self.pixels
    }

    /// Consumes the image and yields its buffer.
    #[must_use]
    pub fn into_pixels(self) -> Vec<u8> {
        self.pixels
    }

    /// One row of pixels, or `None` when `y` is outside the image.
    #[must_use]
    pub fn row(&self, y: u32) -> Option<&[u8]> {
        if y >= self.height {
            return None;
        }
        let stride = self.row_stride();
        let start = y as usize * stride;
        self.pixels.get(start..start + stride)
    }

    /// One pixel, or `None` when the coordinates are outside the image.
    #[must_use]
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        let start = self.offset_of(x, y)?;
        let slice = self.pixels.get(start..start + BYTES_PER_PIXEL)?;
        Some([slice[0], slice[1], slice[2], slice[3]])
    }

    /// Writes one pixel. Coordinates outside the image are ignored.
    pub fn set_pixel(&mut self, x: u32, y: u32, color: [u8; 4]) {
        let Some(start) = self.offset_of(x, y) else {
            return;
        };
        if let Some(slice) = self.pixels.get_mut(start..start + BYTES_PER_PIXEL) {
            slice.copy_from_slice(&color);
        }
    }

    /// Byte offset of a pixel, or `None` when the coordinates are outside the
    /// image.
    fn offset_of(&self, x: u32, y: u32) -> Option<usize> {
        if x >= self.width || y >= self.height {
            return None;
        }
        // The buffer length invariant makes this product fit in `usize`: it is
        // strictly below `width * height * 4`, which is the buffer length.
        Some((y as usize * self.width as usize + x as usize) * BYTES_PER_PIXEL)
    }
}

/// An empty byte buffer with room for `len` bytes.
///
/// The reservation is fallible, so a length the allocator cannot satisfy
/// returns an error instead of aborting the process.
pub(crate) fn try_reserved(len: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(len).map_err(|_| Error::TooLarge {
        needed: u64::try_from(len).unwrap_or(u64::MAX),
        limit: 0,
        unit: "bytes",
    })?;
    Ok(bytes)
}

/// A byte buffer of `len` zero bytes, allocated fallibly.
///
/// # Errors
/// Returns [`Error::TooLarge`] when the allocation fails.
pub(crate) fn try_zeroed(len: usize) -> Result<Vec<u8>> {
    let mut bytes = try_reserved(len)?;
    // The reservation above already holds the capacity, so this fill does not
    // allocate again.
    bytes.resize(len, 0);
    Ok(bytes)
}

/// Byte count of a `width` by `height` RGBA8 buffer.
fn byte_len(width: u32, height: u32) -> Result<usize> {
    if width == 0 || height == 0 {
        return Err(Error::EmptyImage);
    }
    let bytes_per_pixel = u64::try_from(BYTES_PER_PIXEL).unwrap_or(4);
    let pixels = u64::from(width) * u64::from(height);
    let bytes = pixels.checked_mul(bytes_per_pixel).ok_or(Error::TooLarge {
        needed: pixels,
        limit: u64::MAX / bytes_per_pixel,
        unit: "pixels",
    })?;
    usize::try_from(bytes).map_err(|_| Error::TooLarge {
        needed: bytes,
        limit: u64::try_from(usize::MAX).unwrap_or(u64::MAX),
        unit: "bytes",
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_dimension_is_rejected() {
        assert!(matches!(RgbaImage::new(0, 4), Err(Error::EmptyImage)));
        assert!(matches!(RgbaImage::new(4, 0), Err(Error::EmptyImage)));
    }

    #[test]
    fn a_wrong_buffer_length_is_rejected() {
        let error = RgbaImage::from_pixels(2, 2, vec![0; 15]);
        assert!(matches!(error, Err(Error::BufferLength { len: 15, .. })));
    }

    #[test]
    fn pixels_round_trip_through_coordinates() {
        let mut image = RgbaImage::new(3, 2).unwrap();
        image.set_pixel(2, 1, [1, 2, 3, 4]);
        assert_eq!(image.pixel(2, 1), Some([1, 2, 3, 4]));
        assert_eq!(image.pixel(0, 0), Some([0, 0, 0, 0]));
        assert_eq!(image.pixel(3, 1), None);
        assert_eq!(image.pixel(2, 2), None);
    }

    #[test]
    fn a_filled_image_repeats_the_color() {
        let Ok(image) = RgbaImage::filled(2, 2, [9, 8, 7, 6]) else {
            return;
        };
        assert_eq!(image.pixels(), &[9, 8, 7, 6].repeat(4));
    }

    #[test]
    fn a_row_covers_exactly_one_line() {
        let Ok(image) = RgbaImage::new(5, 3) else {
            return;
        };
        assert_eq!(image.row(0).map(<[u8]>::len), Some(20));
        assert_eq!(image.row(3), None);
    }
}
