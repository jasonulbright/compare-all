//! What a frame costs on a comparison far larger than a window can show.
//!
//! The view lays out only the rows its viewport covers, so a frame has to cost
//! the same on a comparison of half a million nodes as on one of ten. The case
//! here paints real frames against a synthetic comparison while streamed
//! content results arrive, and reports the time one frame took; the budget is
//! one frame of a sixty hertz display.
//!
//! A frame budget describes an optimized build, so every case that asserts one
//! runs there alone. The correctness those cases also carry is kept in a small
//! always-on case beside them.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ca_fs::{Attributes, ContentOutcome, ContentUpdate, Entry, Node, NodeStatus, StatusFlags};
use ca_ui::testing::{context, raw_input};
use ca_ui::view::SessionView;
use ca_view_folder::tree::Arena;
use ca_view_folder::FolderView;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// One frame of a sixty hertz display.
const BUDGET: Duration = Duration::from_micros(16_700);

/// Frames painted before the clock starts, so font rasterization and the first
/// layout of each panel are not counted.
const WARMUP: usize = 6;

/// Frames measured.
const MEASURED: usize = 30;

/// Paint frames and report the slowest one, after a warm-up.
///
/// `between` runs before each frame, which is where a case feeds the view the
/// results a worker would have posted.
fn worst_frame<V: SessionView>(
    view: &mut V,
    label: &str,
    mut between: impl FnMut(&mut V),
) -> Duration {
    let ctx = egui::Context::default();
    let context = context();
    let frame = |view: &mut V| {
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

fn entry(rel: &str, is_dir: bool) -> Entry {
    Entry {
        rel: PathBuf::from(rel),
        name: rel.rsplit('/').next().unwrap_or(rel).to_string(),
        is_dir,
        size: 4_096,
        modified: Some(std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
        created: None,
        attributes: Attributes::default(),
        link: None,
        error: None,
        listing_incomplete: false,
        refused: false,
    }
}

fn node(rel: &str, is_dir: bool, status: NodeStatus, children: Vec<Node>) -> Node {
    Node {
        name: rel.rsplit('/').next().unwrap_or(rel).to_string(),
        rel: PathBuf::from(rel),
        is_dir,
        left: Some(entry(rel, is_dir)),
        right: Some(entry(rel, is_dir)),
        facts: ca_fs::PairFacts::default(),
        status,
        flags: StatusFlags::default(),
        quick: None,
        content: None,
        error: None,
        incomplete: false,
        scan_cancelled: false,
        children,
    }
}

/// A comparison of `folders` times `each` files, plus the folders themselves.
fn large_tree(folders: u32, each: u32) -> Node {
    let children = (0..folders)
        .map(|outer| {
            let files = (0..each)
                .map(|inner| {
                    node(
                        &format!("d{outer}/f{inner}.bin"),
                        false,
                        NodeStatus::NotCompared,
                        Vec::new(),
                    )
                })
                .collect();
            node(&format!("d{outer}"), true, NodeStatus::NotCompared, files)
        })
        .collect();
    node("", true, NodeStatus::NotCompared, children)
}

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "wall-clock budget holds for release builds"
)]
fn half_a_million_nodes_paint_inside_the_budget_while_results_arrive() {
    let arena = Arena::from_root(&large_tree(500, 1_000));
    assert_eq!(arena.len(), 500_500);
    let mut view = FolderView::from_arena(
        PathBuf::from("left"),
        PathBuf::from("right"),
        &context(),
        1,
        arena,
    );
    assert_eq!(view.rows().len(), 500_500);
    // Results arrive one file at a time while the window paints, which is the
    // case that costs a rebuild of the rows if they are not coalesced.
    let mut next = 0u32;
    let worst = worst_frame(
        &mut view,
        "folder view, 500,000 nodes with content results arriving",
        |view: &mut FolderView| {
            for _ in 0..200 {
                let outer = next / 1_000;
                let inner = next % 1_000;
                view.apply_content(&ContentUpdate {
                    rel: PathBuf::from(format!("d{outer}/f{inner}.bin")),
                    outcome: Ok(ContentOutcome::BinarySame),
                });
                next += 1;
            }
        },
    );
    assert!(worst <= BUDGET, "worst frame was {worst:?}");
}

/// Correctness half of the frame case, on a tree a debug build paints quickly:
/// the arena, the row list and the path a content result takes are checked
/// without timing anything.
#[test]
fn a_small_tree_paints_and_takes_content_results() {
    let arena = Arena::from_root(&large_tree(10, 100));
    assert_eq!(arena.len(), 1_010);
    let mut view = FolderView::from_arena(
        PathBuf::from("left"),
        PathBuf::from("right"),
        &context(),
        1,
        arena,
    );
    assert_eq!(view.rows().len(), 1_010);
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
    view.apply_content(&ContentUpdate {
        rel: PathBuf::from("d0/f0.bin"),
        outcome: Ok(ContentOutcome::BinarySame),
    });
    view.tick();
    assert_eq!(view.rows().len(), 1_010);
}
