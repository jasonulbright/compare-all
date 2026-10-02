//! Encoded fixtures built at run time. No binary fixture is stored in the
//! repository.

#![allow(clippy::unwrap_used, dead_code)]

use image::{ImageFormat, RgbaImage};
use std::io::Cursor;

/// The encodable formats this build reads, with the format used to encode a
/// fixture for each one.
pub const ENCODABLE: &[(&str, ImageFormat)] = &[
    ("png", ImageFormat::Png),
    ("jpeg", ImageFormat::Jpeg),
    ("gif", ImageFormat::Gif),
    ("bmp", ImageFormat::Bmp),
    ("tiff", ImageFormat::Tiff),
    ("ico", ImageFormat::Ico),
    ("tga", ImageFormat::Tga),
    ("pnm", ImageFormat::Pnm),
];

/// A small gradient with a varying alpha channel.
pub fn gradient(width: u32, height: u32) -> RgbaImage {
    RgbaImage::from_fn(width, height, |x, y| {
        let r = u8::try_from((x * 37) % 256).unwrap_or(0);
        let g = u8::try_from((y * 53) % 256).unwrap_or(0);
        let b = u8::try_from((x * y) % 256).unwrap_or(0);
        image::Rgba([r, g, b, 255])
    })
}

/// Encodes an image in one format.
pub fn encode(image: &RgbaImage, format: ImageFormat) -> Vec<u8> {
    let mut bytes = Vec::new();
    let dynamic = image::DynamicImage::ImageRgba8(image.clone());
    // GIF and PNM cannot store an alpha channel, and JPEG stores neither alpha
    // nor exact samples, so each fixture is encoded from the layout its format
    // accepts.
    let dynamic = match format {
        ImageFormat::Jpeg | ImageFormat::Pnm => image::DynamicImage::ImageRgb8(dynamic.to_rgb8()),
        _ => dynamic,
    };
    dynamic
        .write_to(&mut Cursor::new(&mut bytes), format)
        .unwrap();
    bytes
}

/// A fixture in each encodable format, keyed by its short name.
pub fn all_encoded(width: u32, height: u32) -> Vec<(&'static str, Vec<u8>)> {
    let image = gradient(width, height);
    ENCODABLE
        .iter()
        .map(|(name, format)| (*name, encode(&image, *format)))
        .collect()
}

/// Bytes that carry a WebP signature but no usable payload. The WebP encoder
/// is not part of this build, so the hostile-input cases are built by hand.
pub fn broken_webp() -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&40u32.to_le_bytes());
    bytes.extend_from_slice(b"WEBPVP8L");
    bytes.extend_from_slice(&28u32.to_le_bytes());
    bytes.extend_from_slice(&[0x2f, 0x00, 0x00, 0x00, 0x00]);
    bytes.resize(48, 0);
    bytes
}

/// A PNG that declares a size in its header and carries no image data.
///
/// The header alone decides whether a decode is allowed to start, so this is
/// the input that proves the size check runs before any pixel buffer exists.
pub fn png_header_only(width: u32, height: u32) -> Vec<u8> {
    png_bytes(width, height, false)
}

/// A PNG that declares a size in its header and carries an image data chunk
/// that holds no pixels. The header parse succeeds, so the size check is the
/// next thing that runs.
pub fn png_declaring(width: u32, height: u32) -> Vec<u8> {
    png_bytes(width, height, true)
}

/// A PNG that declares a size and eight bit truecolor without alpha.
///
/// Such a file decodes into a three channel native buffer and then into an
/// RGBA8 buffer, so the two buffers are alive together.
pub fn png_declaring_rgb(width: u32, height: u32) -> Vec<u8> {
    png_bytes_of(width, height, true, 2)
}

/// A sixteen bit per channel PNG holding one pixel of the given channels.
pub fn png_sixteen_bit(red: u16, green: u16, blue: u16) -> Vec<u8> {
    let image = image::ImageBuffer::from_pixel(1, 1, image::Rgb([red, green, blue]));
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgb16(image)
        .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
        .unwrap();
    bytes
}

/// A GIF with a large logical screen and many one-pixel frames.
///
/// The file is small, and every frame is a valid one-pixel image, so a counter
/// that composites each frame onto the logical screen does far more work than
/// the file size suggests.
pub fn gif_many_small_frames(frames: u32, screen: u16) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"GIF89a");
    bytes.extend_from_slice(&screen.to_le_bytes());
    bytes.extend_from_slice(&screen.to_le_bytes());
    // A global color table of two entries follows the screen descriptor.
    bytes.extend_from_slice(&[0x80, 0, 0]);
    bytes.extend_from_slice(&[0, 0, 0, 255, 255, 255]);
    for _ in 0..frames {
        bytes.push(0x2C);
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.push(0x00);
        // One pixel of index zero: a clear code, the index and an end code.
        bytes.extend_from_slice(&[0x02, 0x02, 0x44, 0x01, 0x00]);
    }
    bytes.push(0x3B);
    bytes
}

fn png_bytes(width: u32, height: u32, with_data: bool) -> Vec<u8> {
    png_bytes_of(width, height, with_data, 6)
}

fn png_bytes_of(width: u32, height: u32, with_data: bool, color_type: u8) -> Vec<u8> {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut header = Vec::new();
    header.extend_from_slice(b"IHDR");
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    // Eight bits per sample, the given color type, deflate, adaptive
    // filtering, no interlacing.
    header.extend_from_slice(&[8, color_type, 0, 0, 0]);
    push_chunk(&mut bytes, &header);
    if with_data {
        let mut data = Vec::new();
        data.extend_from_slice(b"IDAT");
        // A zlib stream holding one final stored block of length zero.
        data.extend_from_slice(&[
            0x78, 0x01, 0x01, 0x00, 0x00, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x01,
        ]);
        push_chunk(&mut bytes, &data);
    }
    let mut end = Vec::new();
    end.extend_from_slice(b"IEND");
    push_chunk(&mut bytes, &end);
    bytes
}

fn push_chunk(bytes: &mut Vec<u8>, body: &[u8]) {
    let length = u32::try_from(body.len().saturating_sub(4)).unwrap_or(0);
    bytes.extend_from_slice(&length.to_be_bytes());
    bytes.extend_from_slice(body);
    bytes.extend_from_slice(&crc32(body).to_be_bytes());
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            if crc & 1 == 1 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}
