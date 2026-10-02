//! Cutting a pixel buffer into texture tiles, and feeding those tiles to the
//! graphics device a few at a time.
//!
//! Two bounds drive this module. A texture cannot be wider or taller than the
//! device allows, which is why a buffer is cut up at all; the bound is read
//! from the running context and never assumed. And a frame must not be spent
//! handing over a whole large result, which is why an upload budget limits how
//! much one frame converts and submits.
//!
//! The scheduler is therefore the one place per-image work is allowed on the
//! frame thread. It only ever touches tiles the viewport covers, and it stops
//! as soon as the frame's budget is used up, leaving the rest for the frames
//! that follow.

use std::collections::HashMap;

/// Pixels one frame may convert and hand to the device.
///
/// It is a budget, not a measurement: it bounds the work a frame does so that
/// a result of any size is handed over across several frames rather than one.
pub const UPLOAD_BUDGET_PIXELS: u64 = 1 << 21;

/// Largest tile side the view asks for, before the device bound is applied.
///
/// Smaller tiles waste less work when a viewport covers part of one, and a
/// bound the view sets itself keeps the count of textures predictable on a
/// device that allows very large ones.
pub const PREFERRED_TILE_SIDE: u32 = 1024;

/// One rectangle of a buffer that becomes one texture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Tile {
    /// Left edge in buffer pixels.
    pub x: u32,
    /// Top edge in buffer pixels.
    pub y: u32,
    /// Width in pixels. Never zero.
    pub width: u32,
    /// Height in pixels. Never zero.
    pub height: u32,
}

impl Tile {
    /// Pixels the tile covers.
    #[must_use]
    pub fn pixels(self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }

    /// True when the tile crosses the half-open rectangle
    /// `[x0, y0, x1, y1]`.
    ///
    /// An empty rectangle crosses nothing, whatever its corner sits on.
    #[must_use]
    pub fn intersects(self, region: [u32; 4]) -> bool {
        let [x0, y0, x1, y1] = region;
        if x0 >= x1 || y0 >= y1 {
            return false;
        }
        self.x < x1
            && x0 < self.x.saturating_add(self.width)
            && self.y < y1
            && y0 < self.y.saturating_add(self.height)
    }
}

/// The tile side used for a buffer on a device with the given bound.
#[must_use]
pub fn tile_side(max_texture_side: u32) -> u32 {
    PREFERRED_TILE_SIDE.min(max_texture_side.max(1))
}

/// Cut a buffer of `width` by `height` into tiles no larger than the device
/// allows.
///
/// The tiles cover the buffer exactly: they do not overlap, none is empty, and
/// the ones on the right and bottom edges are as wide and tall as what is left.
#[must_use]
pub fn plan(width: u32, height: u32, max_texture_side: u32) -> Vec<Tile> {
    let side = tile_side(max_texture_side);
    let mut tiles = Vec::new();
    if width == 0 || height == 0 {
        return tiles;
    }
    let across = width.div_ceil(side);
    let down = height.div_ceil(side);
    tiles.reserve(across as usize * down as usize);
    let mut y = 0;
    while y < height {
        let tile_height = side.min(height - y);
        let mut x = 0;
        while x < width {
            let tile_width = side.min(width - x);
            tiles.push(Tile {
                x,
                y,
                width: tile_width,
                height: tile_height,
            });
            x += tile_width;
        }
        y += tile_height;
    }
    tiles
}

/// A buffer the scheduler reads tiles out of.
pub trait TileSource {
    /// Width of the whole buffer in pixels.
    fn width(&self) -> u32;

    /// Height of the whole buffer in pixels.
    fn height(&self) -> u32;

    /// Append the tile's pixels to `out` in row-major RGBA8 order, so that
    /// `out` grows by exactly `tile.width * tile.height * 4` bytes.
    fn read_tile(&self, tile: Tile, out: &mut Vec<u8>);
}

impl TileSource for ca_image::RgbaImage {
    fn width(&self) -> u32 {
        ca_image::RgbaImage::width(self)
    }

    fn height(&self) -> u32 {
        ca_image::RgbaImage::height(self)
    }

    fn read_tile(&self, tile: Tile, out: &mut Vec<u8>) {
        let start = tile.x as usize * 4;
        let end = start + tile.width as usize * 4;
        for row in tile.y..tile.y.saturating_add(tile.height) {
            match self.row(row).and_then(|row| row.get(start..end)) {
                Some(slice) => out.extend_from_slice(slice),
                // A row the buffer does not hold still has to contribute its
                // bytes, otherwise the texture would be built from a buffer
                // shorter than the size it declares.
                None => out.extend(std::iter::repeat_n(0u8, end - start)),
            }
        }
    }
}

/// What a scheduler run did, for a caller that has to decide whether to ask
/// for another frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UploadReport {
    /// Tiles handed over this frame.
    pub uploaded: usize,
    /// Visible tiles still without a texture.
    pub remaining: usize,
}

impl UploadReport {
    /// True when another frame is needed before the content is complete.
    #[must_use]
    pub fn is_incomplete(self) -> bool {
        self.remaining > 0
    }
}

/// The textures of one buffer, filled in a few tiles per frame.
pub struct Uploader {
    name: String,
    plan: Vec<Tile>,
    textures: HashMap<Tile, egui::TextureHandle>,
    /// Frame counter used to drop textures the viewport has left behind, so a
    /// buffer larger than the device's memory does not accumulate one texture
    /// per tile.
    age: HashMap<Tile, u64>,
    clock: u64,
    size: [u32; 2],
    budget: u64,
    /// Frames a texture survives after the viewport stops covering it.
    keep_frames: u64,
}

/// Frames an off-screen texture is kept before it is dropped.
const KEEP_FRAMES: u64 = 120;

impl Uploader {
    /// An uploader for a buffer of `size`, cut for a device bound of
    /// `max_texture_side`.
    #[must_use]
    pub fn new(name: impl Into<String>, size: [u32; 2], max_texture_side: u32) -> Self {
        Self {
            name: name.into(),
            plan: plan(size[0], size[1], max_texture_side),
            textures: HashMap::new(),
            age: HashMap::new(),
            clock: 0,
            size,
            budget: UPLOAD_BUDGET_PIXELS,
            keep_frames: KEEP_FRAMES,
        }
    }

    /// The buffer size this uploader was built for.
    #[must_use]
    pub fn size(&self) -> [u32; 2] {
        self.size
    }

    /// Every tile of the plan.
    #[must_use]
    pub fn tiles(&self) -> &[Tile] {
        &self.plan
    }

    /// Tiles that already hold a texture.
    #[must_use]
    pub fn ready_count(&self) -> usize {
        self.textures.len()
    }

    /// The texture of one tile, once it has been handed over.
    #[must_use]
    pub fn texture(&self, tile: Tile) -> Option<&egui::TextureHandle> {
        self.textures.get(&tile)
    }

    /// Hand over the tiles that `region` covers, up to this frame's budget.
    ///
    /// `region` is a half-open rectangle in buffer pixels.
    pub fn upload_region<S: TileSource + ?Sized>(
        &mut self,
        ctx: &egui::Context,
        source: &S,
        region: [u32; 4],
    ) -> UploadReport {
        self.clock = self.clock.saturating_add(1);
        let mut spent = 0u64;
        let mut report = UploadReport::default();
        let mut scratch = Vec::new();
        let visible: Vec<Tile> = self
            .plan
            .iter()
            .copied()
            .filter(|tile| tile.intersects(region))
            .collect();
        for tile in visible {
            self.age.insert(tile, self.clock);
            if self.textures.contains_key(&tile) {
                continue;
            }
            if spent >= self.budget {
                report.remaining += 1;
                continue;
            }
            scratch.clear();
            source.read_tile(tile, &mut scratch);
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [tile.width as usize, tile.height as usize],
                &scratch,
            );
            let handle = ctx.load_texture(
                format!("{}-{}-{}", self.name, tile.x, tile.y),
                image,
                egui::TextureOptions::NEAREST,
            );
            self.textures.insert(tile, handle);
            spent = spent.saturating_add(tile.pixels());
            report.uploaded += 1;
        }
        self.evict();
        report
    }

    /// Drop the textures of tiles the viewport has not covered for a while.
    fn evict(&mut self) {
        let clock = self.clock;
        let keep = self.keep_frames;
        let age = &self.age;
        self.textures
            .retain(|tile, _| age.get(tile).is_some_and(|seen| clock - *seen <= keep));
        self.age.retain(|_, seen| clock - *seen <= keep);
    }
}

#[cfg(test)]
mod tests {
    use super::{plan, tile_side, Tile, TileSource, PREFERRED_TILE_SIDE};

    fn covered(width: u32, height: u32, max_side: u32) -> u64 {
        plan(width, height, max_side)
            .iter()
            .map(|tile| tile.pixels())
            .sum()
    }

    #[test]
    fn the_tiles_cover_the_buffer_exactly() {
        for (width, height, side) in [
            (1_u32, 1_u32, 256_u32),
            (300, 200, 128),
            (1_024, 1_024, 1_024),
            (16_000, 16_000, 4_096),
            (65_535, 3, 2_048),
        ] {
            let total = u64::from(width) * u64::from(height);
            assert_eq!(covered(width, height, side), total, "{width}x{height}");
        }
    }

    #[test]
    fn no_tile_is_larger_than_the_device_allows() {
        for max_side in [16_u32, 64, 512, 1_024, 4_096, 16_384] {
            let side = tile_side(max_side);
            assert!(side <= max_side);
            assert!(side <= PREFERRED_TILE_SIDE);
            for tile in plan(5_000, 3_000, max_side) {
                assert!(tile.width <= side && tile.height <= side);
                assert!(tile.width > 0 && tile.height > 0);
            }
        }
    }

    #[test]
    fn the_tiles_do_not_overlap() {
        let tiles = plan(700, 500, 256);
        for (index, tile) in tiles.iter().enumerate() {
            for other in tiles.iter().skip(index + 1) {
                assert!(
                    !tile.intersects([
                        other.x,
                        other.y,
                        other.x + other.width,
                        other.y + other.height
                    ]),
                    "{tile:?} overlaps {other:?}"
                );
            }
        }
    }

    #[test]
    fn a_sixteen_thousand_square_buffer_plans_a_known_tile_count() {
        // The plan is built from the device bound, never from a fixed number,
        // so a smaller bound produces more and smaller tiles.
        let wide = plan(16_000, 16_000, 16_384);
        assert_eq!(wide.len(), 16 * 16);
        let narrow = plan(16_000, 16_000, 512);
        assert_eq!(narrow.len(), 32 * 32);
    }

    #[test]
    fn an_empty_buffer_plans_no_tiles() {
        assert!(plan(0, 10, 256).is_empty());
        assert!(plan(10, 0, 256).is_empty());
    }

    #[test]
    fn a_region_selects_only_the_tiles_it_crosses() {
        let tiles = plan(1_024, 1_024, 256);
        let crossing: Vec<Tile> = tiles
            .iter()
            .copied()
            .filter(|tile| tile.intersects([0, 0, 1, 1]))
            .collect();
        assert_eq!(crossing.len(), 1);
        assert_eq!(crossing[0].x, 0);
        let none: Vec<Tile> = tiles
            .iter()
            .copied()
            .filter(|tile| tile.intersects([10, 10, 10, 10]))
            .collect();
        assert!(none.is_empty());
    }

    #[test]
    fn a_tile_read_yields_exactly_its_own_bytes() {
        let Ok(mut image) = ca_image::RgbaImage::new(5, 4) else {
            return;
        };
        image.set_pixel(3, 2, [1, 2, 3, 4]);
        let tile = Tile {
            x: 2,
            y: 1,
            width: 3,
            height: 3,
        };
        let mut out = Vec::new();
        image.read_tile(tile, &mut out);
        assert_eq!(out.len(), 3 * 3 * 4);
        // The marked pixel sits one column across and one row down in a tile
        // three pixels wide.
        let at = (3 + 1) * 4;
        assert_eq!(&out[at..at + 4], &[1, 2, 3, 4]);
    }
}
