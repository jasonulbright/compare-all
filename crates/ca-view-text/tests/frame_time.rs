//! What a frame costs on a comparison far larger than a window can show.
//!
//! Every view lays out only the rows and columns its viewport covers, so a
//! frame has to cost the same on a model of a million rows as on one of ten.
//! Each case here paints real frames against a synthetic comparison and reports
//! the time one frame took; the budget is one frame of a sixty hertz display.
//!
//! A frame budget describes an optimized build, so every case that asserts one
//! runs there alone. The correctness those cases also carry is kept in small
//! always-on cases beside them.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ca_diff::{ClassifiedHunk, Hunk, HunkKind, Importance};
use ca_ui::theme::{palette, Variant};
use ca_ui::view::{SessionView, ViewContext};
use ca_view_text::jobs::{SidePayload, TextData};
use ca_view_text::TextView;
use ca_view_text::{lines, model};
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

fn context() -> ViewContext {
    ViewContext {
        options: Arc::default(),
        palette: palette(Variant::Light),
        notify: Arc::new(|| {}),
    }
}

fn raw_input() -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1_280.0, 800.0),
        )),
        ..egui::RawInput::default()
    }
}

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

/// A comparison of `rows` rows in which every row differs on both sides.
fn all_differing(rows: u32, line: impl Fn(u32, bool) -> String) -> TextData {
    let left: Vec<String> = (0..rows).map(|index| line(index, false)).collect();
    let right: Vec<String> = (0..rows).map(|index| line(index, true)).collect();
    let hunk = ClassifiedHunk {
        hunk: Hunk {
            kind: HunkKind::Changed,
            left: 0..rows,
            right: 0..rows,
        },
        importance: Some(Importance::Important),
        left_lines: vec![Importance::Important; rows as usize],
        right_lines: vec![Importance::Important; rows as usize],
    };
    let left_metrics: Vec<lines::LineMetrics> =
        left.iter().map(|text| lines::measure(text)).collect();
    let right_metrics: Vec<lines::LineMetrics> =
        right.iter().map(|text| lines::measure(text)).collect();
    let widest = left_metrics
        .iter()
        .chain(right_metrics.iter())
        .map(|metrics| metrics.columns)
        .max()
        .unwrap_or(0);
    TextData {
        left: SidePayload {
            lines: std::sync::Arc::new(left),
            metrics: left_metrics,
            encoding: "UTF-8".to_string(),
            eol: "Unix".to_string(),
            mixed_eol: false,
            had_errors: false,
        },
        right: SidePayload {
            lines: std::sync::Arc::new(right),
            metrics: right_metrics,
            encoding: "UTF-8".to_string(),
            eol: "Unix".to_string(),
            mixed_eol: false,
            had_errors: false,
        },
        model: model::build(&[hunk]),
        inline: std::collections::HashMap::new(),
        inline_omitted: true,
        widest,
        sources: None,
    }
}

fn text_view(data: TextData) -> TextView {
    named_text_view(data, "txt")
}

/// A view whose file names carry `suffix`, which is what picks the format.
fn named_text_view(data: TextData, suffix: &str) -> TextView {
    TextView::from_data(
        PathBuf::from(format!("left.{suffix}")),
        PathBuf::from(format!("right.{suffix}")),
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
fn a_million_differing_rows_paint_inside_the_budget() {
    let data = all_differing(1_000_000, |index, right| {
        if right {
            format!("right line {index} of a comparison in which every row differs")
        } else {
            format!("left line {index} of a comparison in which every row differs")
        }
    });
    let mut view = text_view(data);
    assert_eq!(view.model().row_count(), 1_000_000);
    let worst = worst_frame(&mut view, "text view, 1,000,000 rows all differing", |_| {});
    // The overview strip is reduced to one entry per pixel once, not per frame.
    assert!(view.strip_pixels() > 0 && view.strip_pixels() < 1_000);
    assert!(worst <= BUDGET, "worst frame was {worst:?}");
}

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "wall-clock budget holds for release builds"
)]
fn a_million_colored_rows_paint_inside_the_budget_while_the_cache_builds() {
    let data = all_differing(1_000_000, |index, right| {
        let tail = if right { "right" } else { "left" };
        match index % 4 {
            0 => format!("// comment {index} on the {tail}"),
            1 => format!("fn item_{index}() -> u32 {{ {index} }} // {tail}"),
            2 => format!("    let text_{index} = \"a {tail} literal\";"),
            _ => format!("    /* block {index} */ let value = {index};"),
        }
    });
    let mut view = named_text_view(data, "rs");
    assert_eq!(view.format_name(), "Rust");
    let worst = worst_frame(&mut view, "text view, 1,000,000 colored rows", |_| {});
    assert!(worst <= BUDGET, "worst frame was {worst:?}");
}

/// A comparison of three rows whose middle row carries a line of `bytes`.
fn one_long_line(bytes: usize) -> TextData {
    all_differing(3, move |index, right| {
        if index == 1 {
            let mut text = "x".repeat(bytes);
            if right {
                text.push('y');
            }
            text
        } else if right {
            format!("right {index}")
        } else {
            format!("left {index}")
        }
    })
}

/// A frame on a very long line costs the length of the line today. The case
/// holds that cost to the length: four times the line must not cost far more
/// than four times the frame, which is what a quadratic path would show. The
/// ratio is what the case asserts, because an absolute figure would describe
/// the machine that ran it.
#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "wall-clock budget holds for release builds"
)]
fn a_longer_line_costs_a_frame_no_more_than_a_shorter_one() {
    let mut short = text_view(one_long_line(5 * 1024 * 1024));
    let short_worst = worst_frame(&mut short, "text view, one 5 MB line", |_| {});
    let mut long = text_view(one_long_line(20 * 1024 * 1024));
    let long_worst = worst_frame(&mut long, "text view, one 20 MB line", |_| {});
    let ratio = long_worst.as_secs_f64() / short_worst.as_secs_f64().max(f64::MIN_POSITIVE);
    println!("four times the line length cost {ratio:.2} times the frame");
    assert!(
        ratio < 12.0,
        "four times the line length cost {ratio:.2} times the frame \
         ({short_worst:?} against {long_worst:?})"
    );
}
/// Typing into a comparison of a million lines must cost the viewport, not the
/// file. Each frame carries one character and the comparison that follows the
/// edit runs on a worker, so the frame itself only re-lays the rows in view.
#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "wall-clock budget holds for release builds"
)]
fn typing_into_a_million_line_pair_stays_inside_the_budget() {
    let data = all_differing(1_000_000, |index, right| {
        if right {
            format!("right line {index}")
        } else {
            format!("left line {index}")
        }
    });
    let mut view = text_view(data);
    let ctx = egui::Context::default();
    let context = context();
    let typed = |character: char| egui::RawInput {
        events: vec![egui::Event::Text(character.to_string())],
        ..raw_input()
    };
    let frame = |view: &mut TextView, input: egui::RawInput| {
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                view.ui(ui, &context);
            });
        });
    };
    for _ in 0..WARMUP {
        view.tick();
        frame(&mut view, raw_input());
    }
    let mut worst = Duration::ZERO;
    let mut total = Duration::ZERO;
    let strokes = 1_000;
    for index in 0..strokes {
        view.tick();
        let started = Instant::now();
        #[allow(clippy::cast_possible_truncation)]
        let character = char::from(b'a' + (index % 26) as u8);
        frame(&mut view, typed(character));
        let taken = started.elapsed();
        worst = worst.max(taken);
        total += taken;
    }
    let mean = total / strokes;
    println!(
        "typing into a million line pair: worst {:.2} ms, mean {:.2} ms over {strokes} frames",
        worst.as_secs_f64() * 1_000.0,
        mean.as_secs_f64() * 1_000.0
    );
    assert!(
        view.pane(ca_view_text::sidecopy::Side::Left)
            .buffer()
            .is_modified(),
        "the keystrokes did not reach the buffer"
    );
    assert!(
        worst <= BUDGET,
        "the slowest frame took {worst:?}, over the budget of {BUDGET:?}"
    );
}

/// Correctness half of the frame cases, on a model small enough for a debug
/// build: the row model, the format, the bounded overview strip and the path a
/// keystroke takes are all checked without timing anything.
#[test]
fn a_small_comparison_paints_and_keeps_the_strip_bounded() {
    let data = all_differing(1_000, |index, right| {
        let tail = if right { "right" } else { "left" };
        format!("fn item_{index}() -> u32 {{ {index} }} // {tail}")
    });
    let mut view = named_text_view(data, "rs");
    assert_eq!(view.model().row_count(), 1_000);
    assert_eq!(view.format_name(), "Rust");

    let ctx = egui::Context::default();
    let context = context();
    let frame = |view: &mut TextView, input: egui::RawInput| {
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                view.ui(ui, &context);
            });
        });
    };
    for _ in 0..WARMUP {
        view.tick();
        frame(&mut view, raw_input());
    }
    assert!(view.strip_pixels() > 0 && view.strip_pixels() < 1_000);

    let typed = egui::RawInput {
        events: vec![egui::Event::Text("a".to_owned())],
        ..raw_input()
    };
    view.tick();
    frame(&mut view, typed);
    assert!(
        view.pane(ca_view_text::sidecopy::Side::Left)
            .buffer()
            .is_modified(),
        "the keystroke did not reach the buffer"
    );
}
