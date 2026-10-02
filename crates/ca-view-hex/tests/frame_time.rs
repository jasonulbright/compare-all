//! What a frame costs on a comparison far larger than a window can show, and
//! what the toolbar does when the window is as narrow as the shell allows.
//!
//! The view lays out only the rows its viewport covers, so a frame has to cost
//! the same on a layout of tens of millions of rows as on one of ten. Each case
//! paints real frames against a synthetic comparison and reports the time one
//! frame took; the budget is one frame of a sixty hertz display.
//!
//! A frame budget describes an optimized build, so every case that asserts one
//! runs there alone. The correctness those cases also carry is kept in a small
//! always-on case beside them.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ca_diff::{ByteAlignment, ByteHunk, HunkKind};
use ca_ui::command::Command;
use ca_ui::testing::{context, raw_input, sized_input};
use ca_ui::view::SessionView;
use ca_view_hex::jobs::{HexData, SidePayload};
use ca_view_hex::model::{RowModel, Side};
use ca_view_hex::HexView;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// One frame of a sixty hertz display.
const BUDGET: Duration = Duration::from_micros(16_700);

/// Frames painted before the clock starts, so font rasterization and the first
/// layout of each panel are not counted.
const WARMUP: usize = 6;

/// Frames measured.
const MEASURED: usize = 30;

/// One side of the synthetic pair.
const SIDE_BYTES: u64 = 500 * 1024 * 1024;

/// How much of that side the test holds in memory.
///
/// A row whose bytes are not resident still paints, so the deep rows exercise
/// the layout without the test allocating a gigabyte.
const RESIDENT_BYTES: usize = 4 * 1024 * 1024;

/// Paint frames and report the slowest one, after a warm-up.
fn worst_frame(view: &mut HexView, label: &str, mut between: impl FnMut(&mut HexView)) -> Duration {
    let ctx = egui::Context::default();
    let context = context();
    let frame = |view: &mut HexView| {
        let _ = ctx.run(raw_input(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                view.ui(ui, &context);
            });
        });
    };
    for _ in 0..WARMUP {
        view.tick();
        frame(view);
    }
    let mut worst = Duration::ZERO;
    let mut total = Duration::ZERO;
    for _ in 0..MEASURED {
        between(view);
        view.tick();
        let started = Instant::now();
        frame(view);
        let taken = started.elapsed();
        worst = worst.max(taken);
        total += taken;
    }
    let mean = total / u32::try_from(MEASURED).unwrap_or(1);
    println!(
        "{label}: worst {:.2} ms, mean {:.2} ms over {MEASURED} frames",
        worst.as_secs_f64() * 1_000.0,
        mean.as_secs_f64() * 1_000.0
    );
    worst
}

/// A pair of 500 MB sides in which a short run differs every `period` bytes.
fn huge_pair(period: u64) -> HexData {
    let mut hunks: Vec<ByteHunk> = Vec::new();
    let run = 8u64;
    let mut at = 0u64;
    while at + period < SIDE_BYTES {
        hunks.push(ByteHunk {
            kind: HunkKind::Same,
            left: at..at + period - run,
            right: at..at + period - run,
        });
        at += period - run;
        hunks.push(ByteHunk {
            kind: HunkKind::Changed,
            left: at..at + run,
            right: at..at + run,
        });
        at += run;
    }
    hunks.push(ByteHunk {
        kind: HunkKind::Same,
        left: at..SIDE_BYTES,
        right: at..SIDE_BYTES,
    });
    let resident: Vec<u8> = (0..RESIDENT_BYTES)
        .map(|index| u8::try_from(index % 251).unwrap_or(0))
        .collect();
    let side = |offset: u8| SidePayload {
        bytes: Arc::new(
            resident
                .iter()
                .map(|byte| byte.wrapping_add(offset))
                .collect(),
        ),
        len: SIDE_BYTES,
        stamp: None,
    };
    HexData {
        model: RowModel::build(hunks, 16),
        left: side(0),
        right: side(1),
        alignment: ByteAlignment::Complete,
        fresh: true,
    }
}

fn huge_view(data: HexData) -> HexView {
    HexView::from_data(
        PathBuf::from("left.bin"),
        PathBuf::from("right.bin"),
        &context(),
        1,
        data,
    )
}

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "wall-clock budget holds for release builds"
)]
fn a_five_hundred_megabyte_pair_paints_inside_the_budget() {
    let data = huge_pair(64 * 1024);
    let mut view = huge_view(data);
    assert!(
        view.model().row_count() > 30_000_000,
        "{}",
        view.model().row_count()
    );
    let worst = worst_frame(&mut view, "hex view, 500 MB a side, at the top", |_| {});
    // The overview strip is reduced to one entry per pixel once, not per frame.
    assert!(view.strip_pixels() > 0 && view.strip_pixels() < 2_000);
    assert!(worst <= BUDGET, "worst frame was {worst:?}");
}

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "wall-clock budget holds for release builds"
)]
fn the_same_pair_paints_as_fast_deep_inside_the_file() {
    let data = huge_pair(64 * 1024);
    let mut view = huge_view(data);
    view.place_caret(Side::Left, SIDE_BYTES / 2);
    let worst = worst_frame(&mut view, "hex view, 500 MB a side, at the middle", |_| {});
    assert!(worst <= BUDGET, "worst frame was {worst:?}");
}

/// Stepping from one difference to the next has to cost a lookup, not a walk
/// over the rows between them.
#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "wall-clock budget holds for release builds"
)]
fn stepping_through_the_differences_of_a_huge_pair_stays_inside_the_budget() {
    let data = huge_pair(64 * 1024);
    let mut view = huge_view(data);
    let worst = worst_frame(&mut view, "hex view, 500 MB a side, stepping", |view| {
        view.run(Command::NextSection);
    });
    assert!(view.caret_row() > 0);
    assert!(worst <= BUDGET, "worst frame was {worst:?}");
}

/// A difference every few hundred bytes gives the layout hundreds of thousands
/// of regions, which is the shape a per-region cost would show up in.
#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "wall-clock budget holds for release builds"
)]
fn a_pair_with_a_difference_every_kilobyte_paints_inside_the_budget() {
    let data = huge_pair(1_024);
    let mut view = huge_view(data);
    assert!(view.model().counts().sections > 100_000);
    let worst = worst_frame(
        &mut view,
        "hex view, 500 MB a side, dense differences",
        |_| {},
    );
    assert!(worst <= BUDGET, "worst frame was {worst:?}");
}

/// The shell allows a window this narrow, so every control the toolbar carries
/// has to stay inside it.
#[test]
fn no_toolbar_control_runs_past_the_edge_of_a_narrow_window() {
    let data = huge_pair(64 * 1024);
    let mut view = huge_view(data);
    let ctx = egui::Context::default();
    let context = context();
    for width in [640.0_f32, 800.0, 1_280.0] {
        for _ in 0..4 {
            view.tick();
            let _ = ctx.run(sized_input(width, 400.0), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    view.ui(ui, &context);
                });
            });
        }
        let (room, used) = view.toolbar_extent();
        assert!(room > 0.0, "the toolbar was given no room at {width}");
        assert!(
            used <= room + 0.5,
            "at {width} px the toolbar took {used} of {room}"
        );
    }
}

/// Correctness half of the frame cases, on a model a debug build paints
/// quickly: the row model, the section count, the bounded overview strip and
/// the caret move are checked without timing anything.
#[test]
fn a_small_pair_paints_and_keeps_the_strip_bounded() {
    let data = huge_pair(1_024 * 1_024);
    let mut view = huge_view(data);
    assert!(view.model().row_count() > 30_000_000);
    assert!(view.model().counts().sections > 100);
    let ctx = egui::Context::default();
    let context = context();
    for _ in 0..WARMUP {
        view.tick();
        let _ = ctx.run(raw_input(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                view.ui(ui, &context);
            });
        });
    }
    assert!(view.strip_pixels() > 0 && view.strip_pixels() < 2_000);
    view.run(Command::NextSection);
    view.tick();
    assert!(view.caret_row() > 0);
}
