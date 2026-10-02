//! A result far larger than one frame can hand over.
//!
//! The buffer is synthetic and is read tile by tile, so the test costs the
//! tiles a frame actually touches rather than a full-size allocation. What it
//! checks is the property the view depends on: a frame hands over at most its
//! budget, the rest follows on later frames, and no frame is spent converting
//! the whole buffer.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ca_ui::testing::raw_input;
use ca_view_picture::tiles::{plan, Tile, TileSource, Uploader, UPLOAD_BUDGET_PIXELS};
use std::time::{Duration, Instant};

/// Side of the synthetic result.
const SIDE: u32 = 16_000;

/// Longest texture side the test's device is taken to allow.
const MAX_TEXTURE_SIDE: u32 = 4_096;

/// The ceiling one frame is held to. It is a configured bound for the test,
/// not a measurement.
const FRAME_CEILING: Duration = Duration::from_millis(400);

/// A buffer that is never allocated: a tile's pixels are produced when a tile
/// is asked for.
struct Synthetic {
    width: u32,
    height: u32,
}

impl TileSource for Synthetic {
    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn read_tile(&self, tile: Tile, out: &mut Vec<u8>) {
        let bytes = tile.width as usize * tile.height as usize * 4;
        out.reserve(bytes);
        for row in 0..tile.height {
            for column in 0..tile.width {
                let value = u8::try_from((tile.x + column + tile.y + row) % 256).unwrap_or(0);
                out.extend_from_slice(&[value, value, value, 255]);
            }
        }
    }
}

/// Hand a very large result over tile by tile and report the slowest frame.
///
/// Everything the hand-over has to hold true is asserted here; the caller adds
/// the wall-clock bound.
fn hand_over_a_large_result() -> Duration {
    let source = Synthetic {
        width: SIDE,
        height: SIDE,
    };
    let ctx = egui::Context::default();
    let mut uploader = Uploader::new("large", [SIDE, SIDE], MAX_TEXTURE_SIDE);
    let total = plan(SIDE, SIDE, MAX_TEXTURE_SIDE).len();
    assert_eq!(uploader.tiles().len(), total);

    // A viewport over a corner of the buffer, wide enough to cross many tiles.
    let region = [0, 0, 8_192, 8_192];
    let mut worst = Duration::ZERO;
    let mut frames = 0usize;
    let mut first_frame_uploads = 0usize;
    loop {
        frames += 1;
        let started = Instant::now();
        let mut report = None;
        let _ = ctx.run(raw_input(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let done = uploader.upload_region(ui.ctx(), &source, region);
                for tile in uploader.tiles() {
                    if !tile.intersects(region) {
                        continue;
                    }
                    let Some(handle) = uploader.texture(*tile) else {
                        continue;
                    };
                    ui.painter().image(
                        handle.id(),
                        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(8.0, 8.0)),
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                }
                report = Some(done);
            });
        });
        let elapsed = started.elapsed();
        worst = worst.max(elapsed);
        let report = report.expect("the frame ran");
        if frames == 1 {
            first_frame_uploads = report.uploaded;
        }
        if !report.is_incomplete() {
            break;
        }
        assert!(frames < 400, "the upload never finished");
    }

    let tiles_in_region = uploader
        .tiles()
        .iter()
        .filter(|tile| tile.intersects(region))
        .count();
    assert!(tiles_in_region > 1);
    assert!(
        first_frame_uploads < tiles_in_region,
        "one frame handed over the whole region"
    );
    assert!(frames > 1, "the work was not spread over frames");
    let budget_tiles = (UPLOAD_BUDGET_PIXELS / u64::from(MAX_TEXTURE_SIDE.min(1_024)).pow(2)) + 1;
    assert!(
        first_frame_uploads as u64 <= budget_tiles,
        "a frame handed over {first_frame_uploads} tiles, above the budget"
    );
    println!("worst frame {worst:?} over {frames} frames");
    worst
}

#[test]
fn a_very_large_result_is_handed_over_across_frames() {
    let _ = hand_over_a_large_result();
}

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "wall-clock budget holds for release builds"
)]
fn a_very_large_result_never_stalls_one_frame() {
    let worst = hand_over_a_large_result();
    assert!(
        worst < FRAME_CEILING,
        "the worst frame took {worst:?}, above the ceiling"
    );
}

#[test]
fn a_viewport_that_moves_does_not_keep_every_texture() {
    let source = Synthetic {
        width: SIDE,
        height: SIDE,
    };
    let ctx = egui::Context::default();
    let mut uploader = Uploader::new("moving", [SIDE, SIDE], MAX_TEXTURE_SIDE);
    let settle = |uploader: &mut Uploader, region: [u32; 4]| {
        for _ in 0..600 {
            let mut done = false;
            let _ = ctx.run(raw_input(), |ctx| {
                done = !uploader.upload_region(ctx, &source, region).is_incomplete();
            });
            if done {
                return;
            }
        }
        panic!("the region never finished");
    };
    let corner = *uploader
        .tiles()
        .iter()
        .find(|tile| tile.x == 0 && tile.y == 0)
        .expect("the buffer has a first tile");
    settle(&mut uploader, [0, 0, 2_048, 2_048]);
    assert!(uploader.texture(corner).is_some());
    let near = uploader.ready_count();
    assert!(near > 0);
    // Far away from the first region, and left there long enough for the first
    // region's textures to be dropped.
    for _ in 0..400 {
        settle(&mut uploader, [14_000, 14_000, 16_000, 16_000]);
    }
    assert!(
        uploader.texture(corner).is_none(),
        "a texture the viewport left behind was kept"
    );
    assert!(
        uploader.ready_count() < uploader.tiles().len(),
        "every tile was kept: {} held",
        uploader.ready_count()
    );
}
