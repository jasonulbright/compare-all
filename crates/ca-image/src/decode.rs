//! Decoding encoded image bytes into an RGBA8 buffer plus metadata.
//!
//! Only 8-bit RGBA data is produced. Deeper samples are reduced to eight bits
//! per channel, and formats without a pure Rust decoder in this build report
//! [`Error::Unsupported`] rather than reaching for a system codec.
//!
//! The limits in [`Limits`] are checked against the header values the decoder
//! reports, before any pixel buffer is allocated. A file that declares a huge
//! image therefore costs a header parse, not a large allocation.
//!
//! What the byte limit bounds: a decode that produces a native buffer and then
//! converts it to RGBA8 holds both buffers at the same time, so the check adds
//! the two sizes together. A format that decodes straight to RGBA8 holds one
//! buffer, except GIF and animated WebP, whose frame and canvas buffers can
//! coexist with the full-screen output. WebP metadata allocations are included
//! too.

use crate::buffer::RgbaImage;
use crate::cancel::{Cancel, NeverCancel};
use crate::error::{Error, Result};
use crate::scan::{self, Container};
use crate::settings::provisional;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader};
use std::io::{BufRead, Cursor, Seek, SeekFrom};

pub use crate::limits::Limits;

/// Greatest number of RIFF chunks the WebP metadata preflight will inspect.
const MAX_WEBP_CHUNKS: u64 = 100_000;

/// How a decode is carried out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeOptions {
    /// Bounds applied before allocation.
    pub limits: Limits,
    /// Whether the frames of an animated image are counted. Counting walks the
    /// container structure, so it is off by default and the count reports as
    /// `None`.
    pub count_frames: bool,
    /// Greatest number of container bytes a frame count reads or skips. The
    /// count stops there and reports the frames found so far.
    pub max_frame_scan_bytes: u64,
}

impl Default for DecodeOptions {
    fn default() -> Self {
        Self {
            limits: Limits::default(),
            count_frames: false,
            max_frame_scan_bytes: provisional::MAX_FRAME_SCAN_BYTES,
        }
    }
}

/// The encoded format an image was read from.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SourceFormat {
    /// Portable Network Graphics.
    Png,
    /// JPEG.
    Jpeg,
    /// Graphics Interchange Format.
    Gif,
    /// Windows bitmap.
    Bmp,
    /// Tagged Image File Format.
    Tiff,
    /// Windows icon.
    Ico,
    /// WebP.
    WebP,
    /// Truvision Targa.
    Tga,
    /// Netpbm.
    Pnm,
    /// A format this build has no decoder for, named as the decoder names it.
    Other(String),
}

impl SourceFormat {
    /// The short name of the format.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            SourceFormat::Png => "PNG",
            SourceFormat::Jpeg => "JPEG",
            SourceFormat::Gif => "GIF",
            SourceFormat::Bmp => "BMP",
            SourceFormat::Tiff => "TIFF",
            SourceFormat::Ico => "ICO",
            SourceFormat::WebP => "WebP",
            SourceFormat::Tga => "TGA",
            SourceFormat::Pnm => "PNM",
            SourceFormat::Other(name) => name,
        }
    }

    /// True when the format can carry more than one frame.
    #[must_use]
    pub fn may_animate(&self) -> bool {
        self.container().is_some()
    }

    /// The container structure a frame count walks, when this build can walk
    /// one for the format.
    fn container(&self) -> Option<Container> {
        match self {
            SourceFormat::Gif => Some(Container::Gif),
            SourceFormat::WebP => Some(Container::WebP),
            SourceFormat::Png => Some(Container::Png),
            _ => None,
        }
    }

    /// Static collections whose directory count explains which images the
    /// comparison selected.
    fn collection_container(&self) -> Option<Container> {
        match self {
            SourceFormat::Tiff => Some(Container::Tiff),
            SourceFormat::Ico => Some(Container::Ico),
            _ => None,
        }
    }

    /// The format a file extension names, without its leading dot.
    ///
    /// A picture format's masks decide which decoder reads a file, so a file
    /// whose bytes carry no signature still resolves through its name.
    #[must_use]
    pub fn from_extension(extension: &str) -> Option<Self> {
        if ["icb", "vda", "vst", "win"]
            .iter()
            .any(|extension_name| extension.eq_ignore_ascii_case(extension_name))
        {
            return Some(Self::Tga);
        }
        ImageFormat::from_extension(extension).map(SourceFormat::from_image_format)
    }

    fn to_image_format(&self) -> Option<ImageFormat> {
        match self {
            SourceFormat::Png => Some(ImageFormat::Png),
            SourceFormat::Jpeg => Some(ImageFormat::Jpeg),
            SourceFormat::Gif => Some(ImageFormat::Gif),
            SourceFormat::Bmp => Some(ImageFormat::Bmp),
            SourceFormat::Tiff => Some(ImageFormat::Tiff),
            SourceFormat::Ico => Some(ImageFormat::Ico),
            SourceFormat::WebP => Some(ImageFormat::WebP),
            SourceFormat::Tga => Some(ImageFormat::Tga),
            SourceFormat::Pnm => Some(ImageFormat::Pnm),
            SourceFormat::Other(_) => None,
        }
    }

    fn from_image_format(format: ImageFormat) -> Self {
        match format {
            ImageFormat::Png => SourceFormat::Png,
            ImageFormat::Jpeg => SourceFormat::Jpeg,
            ImageFormat::Gif => SourceFormat::Gif,
            ImageFormat::Bmp => SourceFormat::Bmp,
            ImageFormat::Tiff => SourceFormat::Tiff,
            ImageFormat::Ico => SourceFormat::Ico,
            ImageFormat::WebP => SourceFormat::WebP,
            ImageFormat::Tga => SourceFormat::Tga,
            ImageFormat::Pnm => SourceFormat::Pnm,
            other => SourceFormat::Other(format!("{other:?}")),
        }
    }
}

/// The channel layout the file stores, before conversion to RGBA8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ColorKind {
    /// One gray channel.
    Luma,
    /// One gray channel and alpha.
    LumaAlpha,
    /// Red, green and blue.
    Rgb,
    /// Red, green, blue and alpha.
    Rgba,
    /// A layout this build does not name.
    Other,
}

impl ColorKind {
    /// True when the stored layout carries an alpha channel.
    #[must_use]
    pub fn has_alpha(self) -> bool {
        matches!(self, ColorKind::LumaAlpha | ColorKind::Rgba)
    }
}

/// How a file asks its pixels to be oriented for display.
///
/// The decoder reports it; this crate never applies it. The view commands
/// rotate and flip explicitly through [`crate::transform`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Orientation {
    /// Store and display agree.
    #[default]
    Identity,
    /// Turn a quarter turn clockwise.
    Rotate90,
    /// Turn a half turn.
    Rotate180,
    /// Turn a quarter turn counterclockwise.
    Rotate270,
    /// Reflect across the vertical axis.
    FlipHorizontal,
    /// Reflect across the horizontal axis.
    FlipVertical,
    /// Turn a quarter turn clockwise, then reflect across the vertical axis.
    Rotate90FlipHorizontal,
    /// Turn a quarter turn counterclockwise, then reflect across the vertical
    /// axis.
    Rotate270FlipHorizontal,
}

impl Orientation {
    fn from_image(value: image::metadata::Orientation) -> Self {
        use image::metadata::Orientation as Source;
        match value {
            Source::NoTransforms => Orientation::Identity,
            Source::Rotate90 => Orientation::Rotate90,
            Source::Rotate180 => Orientation::Rotate180,
            Source::Rotate270 => Orientation::Rotate270,
            Source::FlipHorizontal => Orientation::FlipHorizontal,
            Source::FlipVertical => Orientation::FlipVertical,
            Source::Rotate90FlipH => Orientation::Rotate90FlipHorizontal,
            Source::Rotate270FlipH => Orientation::Rotate270FlipHorizontal,
        }
    }
}

/// What a decode discarded on the way to RGBA8.
///
/// Every comparison runs on eight bits per channel, so a source that stores
/// more precision, another color model or a color profile loses that
/// information before any pixel is compared. The flags let a caller state what
/// the comparison did not see.
///
/// Palette membership is not reported: the decoder this build uses expands a
/// palette before it names the color type, so the information is already gone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Fidelity {
    /// Bits one channel of the stored pixel occupies. Zero when the decoder
    /// does not report a channel width.
    pub bits_per_channel: u8,
    /// True when the stored samples are wider than eight bits, so the RGBA8
    /// buffer holds less precision than the file.
    pub precision_reduced: bool,
    /// True when the file stores cyan, magenta, yellow and black samples,
    /// which the RGBA8 buffer holds as converted values.
    pub cmyk: bool,
    /// True when the file carries an embedded color profile. The profile is
    /// read but never applied.
    pub icc_profile: bool,
}

impl Fidelity {
    /// True when anything the file carried was dropped before comparison.
    #[must_use]
    pub fn reduced(self) -> bool {
        self.precision_reduced || self.cmyk || self.icc_profile
    }
}

/// What the decoder reports about a file, beside its pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metadata {
    /// The encoded format.
    pub format: SourceFormat,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Channel layout stored in the file.
    pub color: ColorKind,
    /// Bits one stored pixel occupies, across all its channels.
    pub bits_per_pixel: u16,
    /// Number of frames, when they were counted. `None` when counting was off
    /// or the format carries exactly one frame.
    pub frame_count: Option<u32>,
    /// Number of pages or icon entries, when the caller asked for a count.
    pub image_count: Option<u32>,
    /// True when a scan budget, the count ceiling or cancellation stopped the
    /// count, so the frame or image count is a lower bound.
    pub count_is_partial: bool,
    /// Orientation the file asks for. It is reported, never applied.
    pub orientation: Orientation,
    /// What the conversion to RGBA8 dropped.
    pub fidelity: Fidelity,
}

/// One decoded image and what the decoder reported about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedImage {
    /// The first frame, converted to RGBA8.
    pub image: RgbaImage,
    /// What the decoder reported.
    pub metadata: Metadata,
}

/// Decodes an image held in memory.
///
/// For an animated format the first frame is returned. The frame count is
/// filled in only when [`DecodeOptions::count_frames`] is set.
///
/// # Errors
/// Returns [`Error::Unsupported`] for a format this build cannot read,
/// [`Error::Truncated`] or [`Error::InvalidData`] for damaged input, and one of
/// the size errors when the header crosses a limit.
pub fn decode_bytes(bytes: &[u8], options: &DecodeOptions) -> Result<DecodedImage> {
    decode_reader(Cursor::new(bytes), options)
}

/// Decodes an image held in memory, with a cancellation signal.
///
/// The signal stops the frame count between container blocks. The still image
/// itself decodes inside the codec, which does not poll.
///
/// # Errors
/// As [`decode_bytes`].
pub fn decode_bytes_with_cancel<C: Cancel>(
    bytes: &[u8],
    options: &DecodeOptions,
    cancel: &C,
) -> Result<DecodedImage> {
    decode_reader_as_with_cancel(Cursor::new(bytes), None, options, cancel)
}

/// Decodes an image held in memory, using a named format instead of the
/// signature in the bytes.
///
/// Some formats carry no signature, so a file only resolves through the mask
/// its picture format declares. Pass `None` to fall back to the signature.
///
/// # Errors
/// As [`decode_bytes`].
pub fn decode_bytes_as(
    bytes: &[u8],
    format: Option<&SourceFormat>,
    options: &DecodeOptions,
) -> Result<DecodedImage> {
    decode_reader_as(Cursor::new(bytes), format, options)
}

/// Decodes an image from a seekable reader.
///
/// The reader is rewound to its start before the header is read, and again
/// before frames are counted.
///
/// # Errors
/// As [`decode_bytes`], plus [`Error::Io`] when the reader fails.
pub fn decode_reader<R: BufRead + Seek>(
    reader: R,
    options: &DecodeOptions,
) -> Result<DecodedImage> {
    decode_reader_as(reader, None, options)
}

/// Decodes an image from a seekable reader, using a named format instead of
/// the signature in the bytes.
///
/// # Errors
/// As [`decode_reader`].
pub fn decode_reader_as<R: BufRead + Seek>(
    reader: R,
    format: Option<&SourceFormat>,
    options: &DecodeOptions,
) -> Result<DecodedImage> {
    decode_reader_as_with_cancel(reader, format, options, &NeverCancel)
}

/// Decodes an image from a seekable reader, with a named format and a
/// cancellation signal.
///
/// # Errors
/// As [`decode_reader`].
pub fn decode_reader_as_with_cancel<R: BufRead + Seek, C: Cancel>(
    mut reader: R,
    format: Option<&SourceFormat>,
    options: &DecodeOptions,
    cancel: &C,
) -> Result<DecodedImage> {
    reader.seek(SeekFrom::Start(0))?;
    let named = match format {
        Some(named) => Some(
            named
                .to_image_format()
                .ok_or_else(|| Error::Unsupported(format!("no decoder for {}", named.name())))?,
        ),
        None => None,
    };
    let probe = match named {
        Some(named) => ImageReader::with_format(&mut reader, named),
        None => ImageReader::new(&mut reader).with_guessed_format()?,
    };
    let Some(format) = probe.format() else {
        return Err(Error::Unsupported(
            "the format is not recognized".to_owned(),
        ));
    };
    if !format.can_read() {
        return Err(Error::Unsupported(format!(
            "no decoder for {}",
            SourceFormat::from_image_format(format).name()
        )));
    }
    drop(probe);
    let webp_info = if format == ImageFormat::WebP {
        check_webp_metadata(&mut reader, options.limits, cancel)?
    } else {
        WebPInfo::default()
    };
    let mut probe = ImageReader::with_format(&mut reader, format);
    probe.limits(options.limits.as_image_limits());
    let mut decoder = probe.into_decoder()?;

    let (width, height) = decoder.dimensions();
    options.limits.check(width, height)?;

    let color_type = decoder.color_type();
    let color = color_kind(color_type);
    let original = decoder.original_color_type();
    let bits_per_pixel = original.bits_per_pixel();
    let mut decoder_peak = peak_decode_bytes(format, &decoder, width, height, color_type);
    if webp_info.animated {
        let canvas = u64::from(width)
            .saturating_mul(u64::from(height))
            .saturating_mul(4);
        decoder_peak = decoder_peak.max(canvas.saturating_mul(3));
    }
    // WebP keeps its ancillary chunks in the decoder and returns a copy of
    // the ICC profile when asked for it. Budget that possible second copy
    // before either metadata accessor can allocate it.
    let metadata_peak = webp_info.metadata_bytes.saturating_mul(2);
    options
        .limits
        .check_bytes(decoder_peak.saturating_add(metadata_peak))?;

    let orientation = decoder
        .orientation()
        .map_or(Orientation::Identity, Orientation::from_image);
    let fidelity = Fidelity {
        bits_per_channel: bits_per_channel(original),
        precision_reduced: is_wider_than_eight_bits(original),
        cmyk: is_cmyk(original),
        icc_profile: decoder
            .icc_profile()
            .ok()
            .flatten()
            .is_some_and(|profile| !profile.is_empty()),
    };

    let frame = DynamicImage::from_decoder(decoder)?.into_rgba8();
    let image = RgbaImage::from_pixels(width, height, frame.into_raw())?;

    let source = SourceFormat::from_image_format(format);
    let frame_container = source.container();
    let collection_container = source.collection_container();
    let counted = count_if_asked(
        &mut reader,
        frame_container.or(collection_container),
        options,
        cancel,
    )?;
    let count = counted.map(|(count, _)| count);
    let count_is_partial = counted.is_some_and(|(_, partial)| partial);
    let frame_count = frame_container.and(count);
    let image_count = collection_container.and(count);

    Ok(DecodedImage {
        image,
        metadata: Metadata {
            format: source,
            width,
            height,
            color,
            bits_per_pixel,
            frame_count,
            image_count,
            count_is_partial,
            orientation,
            fidelity,
        },
    })
}

/// The frame or image count and whether it is a lower bound, when the caller
/// asked for one and the container carries one.
fn count_if_asked<R: BufRead + Seek, C: Cancel>(
    reader: &mut R,
    container: Option<scan::Container>,
    options: &DecodeOptions,
    cancel: &C,
) -> Result<Option<(u32, bool)>> {
    if !options.count_frames {
        return Ok(None);
    }
    container
        .map(|container| {
            scan::count_frames_in_part(reader, container, options.max_frame_scan_bytes, cancel)
        })
        .transpose()
}

/// Check WebP chunk extents and metadata sizes before its decoder reads those
/// chunks into vectors. The `image` WebP wrapper does not forward its memory
/// limit to the underlying decoder.
#[derive(Debug, Clone, Copy, Default)]
struct WebPInfo {
    metadata_bytes: u64,
    animated: bool,
}

fn check_webp_metadata<R: BufRead + Seek, C: Cancel>(
    reader: &mut R,
    limits: Limits,
    cancel: &C,
) -> Result<WebPInfo> {
    let result = (|| {
        let file_len = reader.seek(SeekFrom::End(0))?;
        reader.seek(SeekFrom::Start(0))?;
        if file_len < 12 {
            return Err(Error::Truncated);
        }
        let mut header = [0; 12];
        reader.read_exact(&mut header)?;
        if &header[..4] != b"RIFF" || &header[8..] != b"WEBP" {
            return Err(Error::InvalidData("invalid WebP RIFF header".to_owned()));
        }
        let riff_end = 8u64
            + u64::from(u32::from_le_bytes([
                header[4], header[5], header[6], header[7],
            ]));
        if riff_end < 12 {
            return Err(Error::InvalidData("invalid WebP RIFF size".to_owned()));
        }
        if riff_end > file_len {
            return Err(Error::Truncated);
        }
        scan_webp_chunks(reader, file_len, riff_end, limits, cancel)
    })();
    let restore = reader.seek(SeekFrom::Start(0));
    match (result, restore) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error.into()),
        (Ok(info), Ok(_)) => Ok(info),
    }
}

fn scan_webp_chunks<R: BufRead + Seek, C: Cancel>(
    reader: &mut R,
    file_len: u64,
    riff_end: u64,
    limits: Limits,
    cancel: &C,
) -> Result<WebPInfo> {
    let mut position = 12u64;
    let mut chunks = 0u64;
    let mut extended = false;
    let mut info = WebPInfo::default();
    loop {
        let decoder_scan_end = if extended {
            riff_end.saturating_add(10)
        } else {
            riff_end
        };
        if position >= decoder_scan_end {
            break;
        }
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if chunks >= MAX_WEBP_CHUNKS {
            return Err(Error::LimitExceeded(format!(
                "WebP has more than {MAX_WEBP_CHUNKS} chunks"
            )));
        }
        let in_riff = position < riff_end;
        if in_riff && riff_end - position < 8 {
            return Err(Error::InvalidData(
                "incomplete WebP chunk header".to_owned(),
            ));
        }
        if file_len.saturating_sub(position) < 8 {
            if in_riff {
                return Err(Error::Truncated);
            }
            break;
        }
        reader.seek(SeekFrom::Start(position))?;
        let mut chunk_header = [0; 8];
        reader.read_exact(&mut chunk_header)?;
        let chunk_size = u64::from(u32::from_le_bytes([
            chunk_header[4],
            chunk_header[5],
            chunk_header[6],
            chunk_header[7],
        ]));
        let data_end = position.saturating_add(8).saturating_add(chunk_size);
        let chunk_end = data_end.saturating_add(chunk_size & 1);
        let is_metadata = &chunk_header[..4] == b"ICCP"
            || &chunk_header[..4] == b"EXIF"
            || &chunk_header[..4] == b"XMP ";
        if chunks == 0 && &chunk_header[..4] == b"VP8X" {
            if chunk_size != 10 {
                return Err(Error::InvalidData(
                    "invalid WebP extended header size".to_owned(),
                ));
            }
            extended = true;
        }
        if is_metadata {
            limits.check_bytes(chunk_size)?;
            info.metadata_bytes = info.metadata_bytes.saturating_add(chunk_size);
            limits.check_bytes(info.metadata_bytes)?;
            if !in_riff {
                return Err(Error::InvalidData(
                    "WebP metadata lies outside the RIFF container".to_owned(),
                ));
            }
        }
        if &chunk_header[..4] == b"ANIM" || &chunk_header[..4] == b"ANMF" {
            info.animated = true;
        } else if chunks == 0 && &chunk_header[..4] == b"VP8X" {
            let mut flags = [0];
            reader.read_exact(&mut flags)?;
            info.animated |= flags[0] & 0x02 != 0;
        }
        if in_riff && chunk_end > riff_end {
            return Err(Error::Truncated);
        }
        position = chunk_end;
        chunks += 1;
    }
    Ok(info)
}

/// The greatest number of pixel bytes the decode holds at one time.
///
/// A decoder that writes a native buffer and then converts it holds that buffer
/// and the RGBA8 buffer together. A decoder that already produces RGBA8 holds
/// one buffer, so the two sizes are not added.
/// GIF and animated WebP can keep frame buffers beside their full-screen output.
fn peak_decode_bytes<D: ImageDecoder>(
    format: ImageFormat,
    decoder: &D,
    width: u32,
    height: u32,
    color_type: image::ColorType,
) -> u64 {
    let native = decoder.total_bytes();
    let rgba = u64::from(width)
        .saturating_mul(u64::from(height))
        .saturating_mul(4);
    if format == ImageFormat::Gif {
        native.saturating_add(rgba)
    } else if color_type == image::ColorType::Rgba8 {
        native.max(rgba)
    } else {
        native.saturating_add(rgba)
    }
}

/// Bits one stored channel occupies, or zero when the layout names none.
fn bits_per_channel(color: image::ExtendedColorType) -> u8 {
    let channels = u16::from(color.channel_count().max(1));
    let bits = color.bits_per_pixel() / channels;
    u8::try_from(bits).unwrap_or(u8::MAX)
}

/// True when one stored channel is wider than the eight bits a comparison uses.
fn is_wider_than_eight_bits(color: image::ExtendedColorType) -> bool {
    bits_per_channel(color) > 8
}

fn is_cmyk(color: image::ExtendedColorType) -> bool {
    use image::ExtendedColorType;
    matches!(color, ExtendedColorType::Cmyk8 | ExtendedColorType::Cmyk16)
}

fn color_kind(color: image::ColorType) -> ColorKind {
    use image::ColorType;
    match color {
        ColorType::L8 | ColorType::L16 => ColorKind::Luma,
        ColorType::La8 | ColorType::La16 => ColorKind::LumaAlpha,
        ColorType::Rgb8 | ColorType::Rgb16 | ColorType::Rgb32F => ColorKind::Rgb,
        ColorType::Rgba8 | ColorType::Rgba16 | ColorType::Rgba32F => ColorKind::Rgba,
        _ => ColorKind::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn webp_with_empty_chunks(
        count: u64,
    ) -> std::result::Result<Vec<u8>, std::num::TryFromIntError> {
        let chunk_bytes = count.saturating_mul(8);
        let riff_size = u32::try_from(chunk_bytes.saturating_add(4))?;
        let capacity = usize::try_from(chunk_bytes.saturating_add(12))?;
        let mut bytes = Vec::with_capacity(capacity);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&riff_size.to_le_bytes());
        bytes.extend_from_slice(b"WEBP");
        for _ in 0..count {
            bytes.extend_from_slice(b"JUNK");
            bytes.extend_from_slice(&0u32.to_le_bytes());
        }
        Ok(bytes)
    }

    fn webp_with_exif_after_riff() -> std::result::Result<Vec<u8>, Box<dyn std::error::Error>> {
        let mut image = Vec::new();
        image::codecs::webp::WebPEncoder::new_lossless(&mut image).encode(
            &[255, 0, 0],
            1,
            1,
            image::ExtendedColorType::Rgb8,
        )?;

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(b"WEBP");
        bytes.extend_from_slice(b"VP8X");
        bytes.extend_from_slice(&10u32.to_le_bytes());
        bytes.extend_from_slice(&[0x08, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        bytes.extend_from_slice(&image[12..]);
        let riff_size = u32::try_from(bytes.len() - 8)?;
        bytes[4..8].copy_from_slice(&riff_size.to_le_bytes());
        bytes.extend_from_slice(b"EXIF");
        bytes.extend_from_slice(&32u32.to_le_bytes());
        bytes.extend_from_slice(&[0; 32]);
        Ok(bytes)
    }

    struct CancelAfterTwoPolls(AtomicUsize);

    impl Cancel for CancelAfterTwoPolls {
        fn is_cancelled(&self) -> bool {
            self.0.fetch_add(1, Ordering::Relaxed) >= 2
        }
    }

    #[test]
    fn unrecognized_bytes_are_unsupported() {
        let options = DecodeOptions::default();
        let error = decode_bytes(&[0x00, 0x01, 0x02, 0x03, 0x04, 0x05], &options);
        assert!(matches!(error, Err(Error::Unsupported(_))));
    }

    #[test]
    fn webp_metadata_preflight_refuses_too_many_chunks(
    ) -> std::result::Result<(), std::num::TryFromIntError> {
        let bytes = webp_with_empty_chunks(MAX_WEBP_CHUNKS + 1)?;
        let result = decode_reader_as_with_cancel(
            Cursor::new(bytes),
            Some(&SourceFormat::WebP),
            &DecodeOptions::default(),
            &NeverCancel,
        );
        assert!(matches!(result, Err(Error::LimitExceeded(_))));
        Ok(())
    }

    #[test]
    fn webp_metadata_preflight_checks_cancellation_between_chunks(
    ) -> std::result::Result<(), std::num::TryFromIntError> {
        let bytes = webp_with_empty_chunks(8)?;
        let cancel = CancelAfterTwoPolls(AtomicUsize::new(0));
        let result = decode_reader_as_with_cancel(
            Cursor::new(bytes),
            Some(&SourceFormat::WebP),
            &DecodeOptions::default(),
            &cancel,
        );
        assert!(matches!(result, Err(Error::Cancelled)));
        assert_eq!(cancel.0.load(Ordering::Relaxed), 3);
        Ok(())
    }

    #[test]
    fn webp_metadata_after_the_declared_riff_end_is_bounded(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let bytes = webp_with_exif_after_riff()?;
        let mut decoder = ImageReader::with_format(Cursor::new(bytes.clone()), ImageFormat::WebP)
            .into_decoder()?;
        let exif = decoder
            .exif_metadata()?
            .ok_or_else(|| std::io::Error::other("the trailing EXIF chunk was not read"))?;
        assert_eq!(exif.len(), 32);

        let limits = Limits {
            max_decoded_bytes: 8,
            ..Limits::default()
        };
        let options = DecodeOptions {
            limits,
            ..DecodeOptions::default()
        };
        let result = decode_reader_as_with_cancel(
            Cursor::new(bytes),
            Some(&SourceFormat::WebP),
            &options,
            &NeverCancel,
        );
        assert!(
            matches!(result, Err(Error::TooLarge { needed: 32, .. })),
            "trailing WebP metadata bypassed the byte limit: {result:?}"
        );
        Ok(())
    }
}
