//! `cargo xtask pack-icons <source> <destination> [alpha-floor]` turns a folder
//! of large PNG icons into the four sizes a theme pack carries, and
//! `cargo xtask pack-index` writes the Rust file that embeds every built-in
//! pack.
//!
//! The output depends only on the input bytes and the constants below: the
//! resampling runs in single-threaded floating point and the encoder tries a
//! fixed list of settings.

use anyhow::{bail, Context};
use image::imageops::{resize, FilterType};
use image::{ImageBuffer, Rgba};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Pixel sizes a pack carries.
const SIZES: [u32; 4] = [16, 24, 32, 48];

/// Alpha below this value is removed before measuring: it is the faint glow
/// the generator leaves around a shape.
const DEFAULT_ALPHA_FLOOR: u8 = 40;

/// The transparent border on each side is one part in this many of the
/// finished icon.
const PADDING_DIVISOR: u32 = 32;

/// A shape smaller than this percentage of the source canvas is not enlarged
/// beyond it, so a small glyph stays small beside a large one.
const MINIMUM_FILL_PERCENT: u32 = 60;

/// A source image with a side above this many pixels is refused: the working
/// canvas is a square of four 32-bit floats per pixel.
const MAXIMUM_SOURCE_SIDE: u32 = 8192;

type Linear = ImageBuffer<Rgba<f32>, Vec<f32>>;

/// Writes `<destination>/<size>/<name>.png` for every PNG in `source`.
pub fn icons(arguments: &[String]) -> anyhow::Result<()> {
    let (Some(source), Some(destination)) = (arguments.first(), arguments.get(1)) else {
        bail!("usage: cargo xtask pack-icons <source> <destination> [alpha-floor]");
    };
    let floor = match arguments.get(2) {
        Some(text) => text.parse().context("the alpha floor is not a number")?,
        None => DEFAULT_ALPHA_FLOOR,
    };
    let destination = Path::new(destination);
    let mut names = Vec::new();
    for entry in std::fs::read_dir(source).with_context(|| format!("cannot read {source}"))? {
        let path = entry?.path();
        if is_png(&path) {
            names.push(path);
        }
    }
    names.sort();
    if names.is_empty() {
        bail!("{source} holds no PNG file");
    }
    for size in SIZES {
        std::fs::create_dir_all(destination.join(size.to_string()))?;
    }
    for path in &names {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .context("a file name is not text")?;
        let picture = image::open(path)
            .with_context(|| format!("cannot read {}", path.display()))?
            .to_rgba8();
        let square = prepare(&picture, floor).with_context(|| stem.to_owned())?;
        for size in SIZES {
            let bytes = encode(&shrink(&square, size), size)?;
            let target = destination
                .join(size.to_string())
                .join(format!("{stem}.png"));
            std::fs::write(&target, bytes)
                .with_context(|| format!("cannot write {}", target.display()))?;
        }
    }
    println!("{} icons written to {}", names.len(), destination.display());
    Ok(())
}

fn is_png(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("png"))
}

fn check_size(width: u32, height: u32) -> anyhow::Result<()> {
    if width.max(height) > MAXIMUM_SOURCE_SIDE {
        bail!("the picture is {width} x {height} pixels; the limit is {MAXIMUM_SOURCE_SIDE} on each side");
    }
    Ok(())
}

/// The picture with faint pixels cleared, trimmed, and centered on a
/// transparent square with premultiplied color.
fn prepare(picture: &image::RgbaImage, floor: u8) -> anyhow::Result<Linear> {
    let (width, height) = picture.dimensions();
    check_size(width, height)?;
    let (mut left, mut top, mut right, mut bottom) = (width, height, 0, 0);
    for (x, y, pixel) in picture.enumerate_pixels() {
        if pixel.0[3] >= floor {
            left = left.min(x);
            top = top.min(y);
            right = right.max(x + 1);
            bottom = bottom.max(y + 1);
        }
    }
    if right <= left || bottom <= top {
        bail!("no pixel reaches the alpha floor");
    }
    let (inner_width, inner_height) = (right - left, bottom - top);
    let minimum = width.max(height) * MINIMUM_FILL_PERCENT / 100;
    let side = inner_width.max(inner_height).max(minimum);
    let canvas = (side * PADDING_DIVISOR).div_ceil(PADDING_DIVISOR - 2);
    let mut out: Linear = ImageBuffer::new(canvas, canvas);
    let (shift_x, shift_y) = ((canvas - inner_width) / 2, (canvas - inner_height) / 2);
    for y in 0..inner_height {
        for x in 0..inner_width {
            let pixel = picture.get_pixel(left + x, top + y).0;
            if pixel[3] < floor {
                continue;
            }
            let alpha = f32::from(pixel[3]) / 255.0;
            out.put_pixel(
                shift_x + x,
                shift_y + y,
                Rgba([
                    f32::from(pixel[0]) / 255.0 * alpha,
                    f32::from(pixel[1]) / 255.0 * alpha,
                    f32::from(pixel[2]) / 255.0 * alpha,
                    alpha,
                ]),
            );
        }
    }
    Ok(out)
}

fn to_byte(value: f32) -> u8 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let rounded = (value * 255.0).round().clamp(0.0, 255.0) as u8;
    rounded
}

/// The square resampled to `size` pixels, as straight 8-bit RGBA bytes.
fn shrink(square: &Linear, size: u32) -> Vec<u8> {
    let small = resize(square, size, size, FilterType::Lanczos3);
    let mut bytes = Vec::with_capacity(small.as_raw().len());
    for pixel in small.pixels() {
        let alpha = pixel.0[3].clamp(0.0, 1.0);
        let byte_alpha = to_byte(alpha);
        if byte_alpha == 0 {
            bytes.extend_from_slice(&[0, 0, 0, 0]);
            continue;
        }
        bytes.extend_from_slice(&[
            to_byte(pixel.0[0] / alpha),
            to_byte(pixel.0[1] / alpha),
            to_byte(pixel.0[2] / alpha),
            byte_alpha,
        ]);
    }
    bytes
}

/// The smallest lossless PNG found over the tried color types and filters.
fn encode(rgba: &[u8], size: u32) -> anyhow::Result<Vec<u8>> {
    let mut best: Option<Vec<u8>> = None;
    let filters = [
        png::Filter::NoFilter,
        png::Filter::Sub,
        png::Filter::Up,
        png::Filter::Avg,
        png::Filter::Paeth,
        png::Filter::Adaptive,
        png::Filter::MinEntropy,
    ];
    let indexed = index_colors(rgba, size);
    for filter in filters {
        let mut candidates = vec![write_png(size, filter, &Layout::Rgba(rgba))?];
        if let Some(indexed) = &indexed {
            candidates.push(write_png(size, filter, &Layout::Indexed(indexed))?);
        }
        for candidate in candidates {
            if best.as_ref().is_none_or(|b| candidate.len() < b.len()) {
                best = Some(candidate);
            }
        }
    }
    best.context("no encoding was tried")
}

struct Indexed {
    palette: Vec<u8>,
    transparency: Vec<u8>,
    depth: png::BitDepth,
    rows: Vec<u8>,
}

enum Layout<'a> {
    Rgba(&'a [u8]),
    Indexed(&'a Indexed),
}

fn write_png(size: u32, filter: png::Filter, layout: &Layout<'_>) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, size, size);
        encoder.set_deflate_compression(png::DeflateCompression::Level(9));
        encoder.set_filter(filter);
        let data = match layout {
            Layout::Rgba(bytes) => {
                encoder.set_color(png::ColorType::Rgba);
                encoder.set_depth(png::BitDepth::Eight);
                *bytes
            }
            Layout::Indexed(indexed) => {
                encoder.set_color(png::ColorType::Indexed);
                encoder.set_depth(indexed.depth);
                encoder.set_palette(indexed.palette.as_slice());
                if !indexed.transparency.is_empty() {
                    encoder.set_trns(indexed.transparency.as_slice());
                }
                indexed.rows.as_slice()
            }
        };
        let mut writer = encoder.write_header()?;
        writer.write_image_data(data)?;
        writer.finish()?;
    }
    Ok(out)
}

/// The picture as a palette image, when it has at most 256 distinct colors.
fn index_colors(rgba: &[u8], size: u32) -> Option<Indexed> {
    let mut colors: Vec<[u8; 4]> = Vec::new();
    let mut indices = Vec::with_capacity(rgba.len() / 4);
    for chunk in rgba.chunks_exact(4) {
        let color = [chunk[0], chunk[1], chunk[2], chunk[3]];
        let position = if let Some(position) = colors.iter().position(|c| *c == color) {
            position
        } else {
            colors.push(color);
            colors.len() - 1
        };
        indices.push(u8::try_from(position).ok()?);
    }
    let (depth, bits) = match colors.len() {
        0..=2 => (png::BitDepth::One, 1usize),
        3..=4 => (png::BitDepth::Two, 2),
        5..=16 => (png::BitDepth::Four, 4),
        _ => (png::BitDepth::Eight, 8),
    };
    let width = usize::try_from(size).ok()?;
    let mut rows = Vec::new();
    for row in indices.chunks(width) {
        let mut byte = 0u8;
        let mut used = 0usize;
        for &index in row {
            byte = byte.checked_shl(u32::try_from(bits).ok()?).unwrap_or(0) | index;
            used += bits;
            if used == 8 {
                rows.push(byte);
                byte = 0;
                used = 0;
            }
        }
        if used > 0 {
            rows.push(byte << (8 - used));
        }
    }
    let palette = colors.iter().flat_map(|c| [c[0], c[1], c[2]]).collect();
    let mut transparency: Vec<u8> = colors.iter().map(|c| c[3]).collect();
    while transparency.last() == Some(&255) {
        transparency.pop();
    }
    Some(Indexed {
        palette,
        transparency,
        depth,
        rows,
    })
}

/// Writes `crates/ca-ui/src/pack/builtin_files.rs`: one manifest constant and
/// one file-list constant for each folder under `assets/packs`.
pub fn index(root: &Path) -> anyhow::Result<()> {
    let packs = root.join("assets").join("packs");
    let mut folders: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(&packs).context("cannot read assets/packs")? {
        let path = entry?.path();
        if path.is_dir() {
            folders.push(path);
        }
    }
    folders.sort();
    let mut out = String::from(
        "//! Embedded files of the built-in packs. Written by `cargo xtask pack-index`.\n\n",
    );
    for folder in &folders {
        let name = folder
            .file_name()
            .and_then(|n| n.to_str())
            .context("a pack folder name is not text")?;
        let constant = name.to_uppercase().replace('-', "_");
        write!(
            out,
            "/// The manifest of the pack in `assets/packs/{name}`.\npub const {constant}_MANIFEST: &[u8] = include_bytes!(concat!(\n    env!(\"CARGO_MANIFEST_DIR\"),\n    \"/../../assets/packs/{name}/pack.json\"\n));\n\n"
        )?;
        let mut files = Vec::new();
        for size in SIZES {
            let mut names: Vec<String> =
                std::fs::read_dir(folder.join("icons").join(size.to_string()))?
                    .filter_map(Result::ok)
                    .filter_map(|e| e.file_name().into_string().ok())
                    .filter(|n| is_png(Path::new(n)))
                    .collect();
            names.sort();
            files.extend(names.into_iter().map(|n| format!("icons/{size}/{n}")));
        }
        write!(
            out,
            "/// The icon files of the pack in `assets/packs/{name}`.\npub const {constant}_FILES: &[(&str, &[u8])] = &[\n"
        )?;
        for file in files {
            write!(
                out,
                "    (\n        \"{file}\",\n        include_bytes!(concat!(\n            env!(\"CARGO_MANIFEST_DIR\"),\n            \"/../../assets/packs/{name}/{file}\"\n        )),\n    ),\n"
            )?;
        }
        out.push_str("];\n\n");
    }
    let target = root.join("crates/ca-ui/src/pack/builtin_files.rs");
    std::fs::write(&target, out.trim_end().to_owned() + "\n")?;
    println!("{}", target.display());
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn picture() -> image::RgbaImage {
        let mut picture = image::RgbaImage::new(128, 128);
        for y in 20..60 {
            for x in 30..100 {
                picture.put_pixel(x, y, Rgba([200, 40, 40, 255]));
            }
        }
        picture.put_pixel(5, 5, Rgba([255, 255, 255, 10]));
        picture
    }

    #[test]
    fn faint_pixels_do_not_count_toward_the_trim() {
        let square = prepare(&picture(), DEFAULT_ALPHA_FLOOR).unwrap();
        // The 70 x 40 shape rises to the 76 pixel minimum fill; a trim that
        // kept the faint corner pixel would give a 102 pixel canvas.
        assert_eq!((square.width(), square.height()), (82, 82));
        assert_eq!(shrink(&square, 16)[..4], [0, 0, 0, 0]);
    }

    #[test]
    fn the_shape_is_centered() {
        let square = prepare(&picture(), DEFAULT_ALPHA_FLOOR).unwrap();
        let opaque = |x: u32, y: u32| square.get_pixel(x, y).0[3] > 0.0;
        assert!(opaque(6, 21) && opaque(75, 60));
        assert!(!opaque(5, 21) && !opaque(76, 60));
        assert!(!opaque(6, 20) && !opaque(75, 61));
        let bytes = shrink(&square, 32);
        let alpha = |x: usize, y: usize| bytes[(y * 32 + x) * 4 + 3];
        let left = (0..32).find(|x| alpha(*x, 16) > 0).unwrap_or(0);
        let right = (0..32).rev().find(|x| alpha(*x, 16) > 0).unwrap_or(0);
        assert!(left.abs_diff(31 - right) <= 1, "{left} {right}");
        let top = (0..32).find(|y| alpha(16, *y) > 0).unwrap_or(0);
        let bottom = (0..32).rev().find(|y| alpha(16, *y) > 0).unwrap_or(0);
        assert!(top > 0 && top.abs_diff(31 - bottom) <= 1, "{top} {bottom}");
    }

    #[test]
    fn a_huge_source_is_refused_before_any_arithmetic() {
        assert!(check_size(72_000_000, 1).is_err());
        assert!(check_size(1, u32::MAX).is_err());
        assert!(check_size(MAXIMUM_SOURCE_SIDE, MAXIMUM_SOURCE_SIDE).is_ok());
    }

    #[test]
    fn the_png_extension_matches_in_any_case() {
        assert!(is_png(Path::new("a.png")));
        assert!(is_png(Path::new("a.PNG")));
        assert!(is_png(Path::new("a.Png")));
        assert!(!is_png(Path::new("a.jpg")));
        assert!(!is_png(Path::new("png")));
    }

    #[test]
    fn encoding_is_lossless_and_repeatable() {
        let square = prepare(&picture(), DEFAULT_ALPHA_FLOOR).unwrap();
        let bytes = shrink(&square, 24);
        let first = encode(&bytes, 24).unwrap();
        assert_eq!(first, encode(&bytes, 24).unwrap());
        let decoded = image::load_from_memory(&first)
            .map(|d| d.to_rgba8().into_raw())
            .unwrap_or_default();
        assert_eq!(decoded, bytes);
    }
}
