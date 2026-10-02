//! The error type every fallible entry point of this crate returns.

use std::io;

/// Shorthand for a result carrying this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// A failure reading, comparing or transforming an image.
///
/// The variants separate hostile or damaged input from configuration mistakes,
/// so a caller can report a bad file without treating it as a program defect.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The image declares a width or height of zero.
    #[error("the image has a zero width or height")]
    EmptyImage,

    /// The declared width or height is above the configured limit. The check
    /// runs on the header values, before a pixel buffer is allocated.
    #[error(
        "image size {width}x{height} is above the configured limit {limit_width}x{limit_height}"
    )]
    DimensionsTooLarge {
        /// Declared width in pixels.
        width: u32,
        /// Declared height in pixels.
        height: u32,
        /// Configured maximum width in pixels.
        limit_width: u32,
        /// Configured maximum height in pixels.
        limit_height: u32,
    },

    /// The pixel count or the decoded byte count is above the configured
    /// limit. The check runs on the header values, before allocation.
    #[error("decoding needs {needed} {unit} which is above the configured limit {limit}")]
    TooLarge {
        /// Amount the input asks for.
        needed: u64,
        /// Configured maximum.
        limit: u64,
        /// Name of the counted unit, either `pixels` or `bytes`.
        unit: &'static str,
    },

    /// The comparison result does not fit in the configured result bounds. The
    /// result covers the bounding box of the two placed images, so an offset
    /// can push it above the bounds even when both images decode.
    #[error(
        "the comparison result {width}x{height} is above the configured result limit \
         of {limit_pixels} pixels and {limit_bytes} bytes"
    )]
    ResultTooLarge {
        /// Width of the result in pixels.
        width: u32,
        /// Height of the result in pixels.
        height: u32,
        /// Configured maximum pixel count of a result.
        limit_pixels: u64,
        /// Configured maximum byte count of a result.
        limit_bytes: u64,
    },

    /// The decoder's own guard stopped the decode. It triggers on input whose
    /// true cost only becomes visible part way through decoding, which the
    /// header checks cannot see.
    #[error("the image exceeds the configured decode limits: {0}")]
    LimitExceeded(String),

    /// The input uses a format, or a feature of a format, that this build has
    /// no pure Rust decoder for.
    #[error("the image format is not supported: {0}")]
    Unsupported(String),

    /// The input is not a valid image of its declared format.
    #[error("the image data is not valid: {0}")]
    InvalidData(String),

    /// The input ended before the image was complete.
    #[error("the input ended before the image was complete")]
    Truncated,

    /// Reading the input failed.
    #[error("reading the image failed: {0}")]
    Io(#[from] io::Error),

    /// A pixel buffer length does not match the width and height it is built
    /// with.
    #[error("a buffer of {len} bytes does not hold {width}x{height} RGBA pixels")]
    BufferLength {
        /// Length of the supplied buffer in bytes.
        len: usize,
        /// Width the buffer was built with.
        width: u32,
        /// Height the buffer was built with.
        height: u32,
    },

    /// An argument is outside the range the operation accepts.
    #[error("{0}")]
    OutOfRange(String),

    /// The caller cancelled the operation through its [`crate::Cancel`].
    #[error("the operation was cancelled")]
    Cancelled,
}

impl Error {
    /// True when the error describes input that this build cannot read at all,
    /// as opposed to input that is damaged or too large.
    #[must_use]
    pub fn is_unsupported(&self) -> bool {
        matches!(self, Error::Unsupported(_))
    }

    /// True when the error describes damaged, truncated or hostile input
    /// rather than a configuration or programming mistake.
    #[must_use]
    pub fn is_bad_input(&self) -> bool {
        matches!(
            self,
            Error::EmptyImage
                | Error::DimensionsTooLarge { .. }
                | Error::TooLarge { .. }
                | Error::LimitExceeded(_)
                | Error::Unsupported(_)
                | Error::InvalidData(_)
                | Error::Truncated
        )
    }
}

impl From<image::ImageError> for Error {
    fn from(source: image::ImageError) -> Self {
        use image::error::ImageError;
        match source {
            ImageError::Unsupported(inner) => Error::Unsupported(inner.to_string()),
            ImageError::Limits(inner) => Error::LimitExceeded(inner.to_string()),
            ImageError::IoError(inner) => {
                if inner.kind() == io::ErrorKind::UnexpectedEof {
                    Error::Truncated
                } else {
                    Error::Io(inner)
                }
            }
            ImageError::Decoding(inner) => Error::InvalidData(inner.to_string()),
            ImageError::Encoding(inner) => Error::InvalidData(inner.to_string()),
            ImageError::Parameter(inner) => Error::OutOfRange(inner.to_string()),
        }
    }
}
