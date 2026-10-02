//! Picture comparison engine for compare-all: decode, tolerance, difference and blend modes.
//!
//! The crate has no user interface dependency. Every entry point takes and
//! returns plain data: RGBA8 pixel buffers, a per-pixel classification mask,
//! counters and serde settings types.
//!
//! The modules split the work:
//!
//! - [`buffer`] holds [`RgbaImage`], the 8-bit RGBA pixel buffer everything
//!   else operates on.
//! - [`decode`] turns encoded bytes into an [`RgbaImage`] plus [`Metadata`],
//!   under limits that are checked before any pixel buffer is allocated.
//! - [`limits`] holds [`Limits`], the bounds checked before an allocation, in
//!   one set for a decoded image and one set for a comparison result.
//! - [`compare`] compares two buffers and produces a rendered result buffer, a
//!   packed classification mask and [`Totals`].
//! - [`transform`] rotates, flips and resamples buffers.
//! - [`settings`] carries the serde types for picture session settings and
//!   picture file format settings.
//! - [`cancel`] defines the cooperative cancellation trait long comparisons
//!   poll.

pub mod buffer;
pub mod cancel;
pub mod compare;
pub mod decode;
pub mod error;
pub mod limits;
mod scan;
pub mod settings;
pub mod transform;

pub use buffer::RgbaImage;
pub use cancel::{Cancel, CancelFlag, NeverCancel};
pub use compare::{
    compare, ClassMask, CompareOptions, CompareResult, DisplayMode, Offset, PixelClass,
    Replacement, Side, ToleranceColors, Totals,
};
pub use decode::{
    decode_bytes, decode_bytes_as, decode_bytes_with_cancel, decode_reader, decode_reader_as,
    decode_reader_as_with_cancel, ColorKind, DecodeOptions, DecodedImage, Fidelity, Metadata,
    Orientation, SourceFormat,
};
pub use error::{Error, Result};
pub use limits::Limits;
