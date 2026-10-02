//! `cargo xtask icon`: rasterises `assets/icon/compare-all.svg` into one PNG
//! for each size and one ICO that holds every size up to 256 pixels.

use anyhow::{bail, Context};
use std::path::Path;

/// Pixel sizes written as PNG files.
pub const SIZES: [u32; 8] = [16, 24, 32, 48, 64, 128, 256, 512];

/// Largest size packed into the ICO. The directory entry stores a width in one
/// byte where 0 means 256, so a larger image is read as 256 by Windows.
const ICO_LIMIT: u32 = 256;

/// Below this size a text line is thinner than one pixel and turns to noise.
const SMALLEST_WITH_LINES: u32 = 32;

const LINES_BEGIN: &str = "<!--lines:begin-->";
const LINES_END: &str = "<!--lines:end-->";

/// Writes every PNG and the ICO next to the SVG.
pub fn run(root: &Path) -> anyhow::Result<()> {
    let folder = root.join("assets").join("icon");
    let svg = std::fs::read_to_string(folder.join("compare-all.svg"))
        .context("cannot read assets/icon/compare-all.svg")?;
    let without_lines = strip_lines(&svg)?;
    let mut directory = ico::IconDir::new(ico::ResourceType::Icon);
    for size in SIZES {
        let source = if size < SMALLEST_WITH_LINES {
            &without_lines
        } else {
            &svg
        };
        let pixmap = render(source, size)?;
        let png = pixmap.encode_png().context("cannot encode the PNG")?;
        let name = format!("compare-all-{size}.png");
        std::fs::write(folder.join(&name), &png).with_context(|| format!("cannot write {name}"))?;
        if size <= ICO_LIMIT {
            let image = ico::IconImage::from_rgba_data(size, size, pixmap.take());
            directory.add_entry(ico::IconDirEntry::encode_as_png(&image)?);
        }
        println!("{name}");
    }
    let file = std::fs::File::create(folder.join("compare-all.ico"))
        .context("cannot create compare-all.ico")?;
    directory
        .write(file)
        .context("cannot write compare-all.ico")?;
    println!("compare-all.ico");
    Ok(())
}

/// The SVG with every marked block of text lines removed.
fn strip_lines(svg: &str) -> anyhow::Result<String> {
    let mut out = String::with_capacity(svg.len());
    let mut rest = svg;
    while let Some(start) = rest.find(LINES_BEGIN) {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start..].find(LINES_END) else {
            bail!("a lines block has no end marker");
        };
        rest = &rest[start + end + LINES_END.len()..];
    }
    out.push_str(rest);
    Ok(out)
}

/// The SVG drawn into a square of `size` pixels.
fn render(svg: &str, size: u32) -> anyhow::Result<resvg::tiny_skia::Pixmap> {
    let tree = resvg::usvg::Tree::from_str(svg, &resvg::usvg::Options::default())
        .context("cannot parse the SVG")?;
    let mut pixmap =
        resvg::tiny_skia::Pixmap::new(size, size).context("cannot allocate the pixmap")?;
    let view = tree.size();
    #[allow(clippy::cast_precision_loss)]
    let scale = size as f32 / view.width().max(view.height());
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    Ok(pixmap)
}
