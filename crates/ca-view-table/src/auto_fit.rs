//! Background measurement for Resize Columns to Fit.
//!
//! Font shaping and the full comparison scan happen on the job thread. Every
//! batch owns a short-lived font context, so the cache is bounded and the
//! measurement never holds the live window's font lock.

use crate::model::{self, Side, Source};
use ca_table::compare::RowStatus;
use ca_ui::worker::{Cancel, Job, Terminal};
use std::borrow::Cow;
use std::sync::Arc;

/// Bound the font cache to one modest batch instead of every distinct cell.
const LAYOUTS_PER_PASS: usize = 10_000;

struct Layout<'a> {
    column: usize,
    text: Cow<'a, str>,
    inset: f32,
}

/// What the sizing worker posted.
pub enum Message {
    /// One width per source column, in points.
    Ready(Vec<f32>),
    /// The worker stopped before finishing.
    Cancelled,
    /// The worker panicked while measuring text.
    Failed(String),
}

impl Terminal for Message {
    fn is_terminal(&self) -> bool {
        matches!(self, Self::Ready(_) | Self::Cancelled | Self::Failed(_))
    }

    fn cancelled() -> Self {
        Self::Cancelled
    }

    fn panicked(detail: String) -> Self {
        Self::Failed(detail)
    }
}

/// Measure headings and every present cell on both sides of a comparison.
pub fn spawn(
    source: Arc<dyn Source>,
    point_size: f32,
    pixels_per_point: f32,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<Message> {
    Job::spawn_notifying(
        move |emitter, cancel| {
            let font = egui::FontId::proportional(point_size);
            let widths = measure(source.as_ref(), pixels_per_point, &font, cancel, |_, _| {});
            let message = widths.map_or(Message::Cancelled, Message::Ready);
            let _ = emitter.send(message);
        },
        notify,
    )
}

fn measure(
    source: &dyn Source,
    pixels_per_point: f32,
    font: &egui::FontId,
    cancel: &Cancel,
    mut on_font_pass: impl FnMut(usize, &Arc<egui::Context>),
) -> Option<Vec<f32>> {
    let columns = source.columns();
    let mut widths = vec![model::MINIMUM_COLUMN; columns.len()];
    let mut batch = Vec::with_capacity(LAYOUTS_PER_PASS);

    for (index, column) in columns.iter().enumerate() {
        if cancel.is_cancelled() {
            return None;
        }
        batch.push(Layout {
            column: index,
            text: Cow::Borrowed(&column.name),
            inset: column_inset(column),
        });
        if batch.len() == LAYOUTS_PER_PASS {
            if !measure_batch(
                pixels_per_point,
                font,
                cancel,
                &batch,
                &mut widths,
                &mut on_font_pass,
            ) {
                return None;
            }
            batch.clear();
        }
    }

    for row in 0..source.rows() {
        if cancel.is_cancelled() {
            return None;
        }
        let row_status = source.row_status(row);
        for (column, info) in columns.iter().enumerate() {
            if cancel.is_cancelled() {
                return None;
            }
            let sides: &[Side] = match row_status {
                RowStatus::LeftOnly => &[Side::Left],
                RowStatus::RightOnly => &[Side::Right],
                _ => &[Side::Left, Side::Right],
            };
            for side in sides {
                if cancel.is_cancelled() {
                    return None;
                }
                let text = source.cell_text(row, column, *side);
                if text.is_empty() {
                    continue;
                }
                batch.push(Layout {
                    column,
                    text,
                    inset: column_inset(info),
                });
                if batch.len() == LAYOUTS_PER_PASS {
                    if !measure_batch(
                        pixels_per_point,
                        font,
                        cancel,
                        &batch,
                        &mut widths,
                        &mut on_font_pass,
                    ) {
                        return None;
                    }
                    batch.clear();
                }
            }
        }
    }

    if !batch.is_empty()
        && !measure_batch(
            pixels_per_point,
            font,
            cancel,
            &batch,
            &mut widths,
            &mut on_font_pass,
        )
    {
        return None;
    }
    Some(widths)
}

/// Shape one bounded batch in a fresh context so its galley cache dies with it.
fn measure_batch(
    pixels_per_point: f32,
    font: &egui::FontId,
    cancel: &Cancel,
    batch: &[Layout<'_>],
    widths: &mut [f32],
    on_font_pass: &mut impl FnMut(usize, &Arc<egui::Context>),
) -> bool {
    // epaint retains galleys from the current and immediately preceding font
    // pass. A fresh context per batch keeps even that overlap within this
    // batch's bound, then drops the cache when this function returns.
    let fonts = Arc::new(egui::Context::default());
    fonts.set_pixels_per_point(pixels_per_point);
    let mut cancelled = false;
    let _ = fonts.run(egui::RawInput::default(), |ctx| {
        on_font_pass(batch.len(), &fonts);
        ctx.fonts(|font_cache| {
            for layout in batch {
                if cancel.is_cancelled() {
                    cancelled = true;
                    break;
                }
                let galley = font_cache.layout_delayed_color(
                    layout.text.as_ref().to_owned(),
                    font.clone(),
                    f32::INFINITY,
                );
                widths[layout.column] = widths[layout.column].max(galley.size().x + layout.inset);
            }
        });
    });
    !cancelled
}

/// Cell padding plus room for the optional header markers.
fn column_inset(column: &model::ColumnInfo) -> f32 {
    use super::{CELL_PAD, MARKER};
    let marker_room = if column.key || column.unimportant {
        // The outer marker ends 24 points before the column edge; four more
        // points leave a clear gap between it and the text.
        MARKER * 4.8 + CELL_PAD
    } else {
        0.0
    };
    CELL_PAD * 2.0 + marker_room
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
mod tests {
    use super::{measure, spawn, Message, LAYOUTS_PER_PASS};
    use crate::model::Side;
    use crate::testing::FakeSource;
    use ca_table::compare::RowStatus;
    use ca_ui::worker::Job;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn finish(mut job: Job<Message>) -> Vec<Message> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut messages = Vec::new();
        while Instant::now() < deadline {
            messages.extend(job.drain());
            if job.is_finished() {
                return messages;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("column sizing job did not finish in time");
    }

    #[test]
    fn sizes_to_the_widest_heading_or_cell_on_either_side_without_a_cap() {
        let long = "wide-value".repeat(160);
        let source = FakeSource::new(&[RowStatus::Same], 2)
            .with_column_name(0, "ID")
            .with_text(0, 0, Side::Left, "")
            .with_text(0, 0, Side::Right, &long)
            .with_column_name(1, "A much longer heading")
            .with_text(0, 1, Side::Left, "")
            .with_text(0, 1, Side::Right, "");
        let job = spawn(Arc::new(source), 14.0, 1.0, Arc::new(|| {}));
        let messages = finish(job);
        let [Message::Ready(widths)] = messages.as_slice() else {
            panic!(
                "expected one completed sizing result, got {} messages",
                messages.len()
            );
        };
        assert!(widths[0] > 1_000.0, "width was capped at {}", widths[0]);
        assert!(widths[1] > 28.0, "heading was not included: {}", widths[1]);
        assert!(
            widths[0] > widths[1],
            "right side's cell text was not included"
        );
    }

    #[test]
    fn a_cancelled_full_scan_posts_no_partial_widths() {
        let source = FakeSource::repeating(1_000_000, 2, &[RowStatus::Same]);
        let job = spawn(Arc::new(source), 14.0, 1.0, Arc::new(|| {}));
        job.cancel();
        let messages = finish(job);
        assert!(matches!(messages.as_slice(), [Message::Cancelled]));
    }

    #[test]
    fn each_batch_uses_a_separate_bounded_font_cache() {
        let rows = LAYOUTS_PER_PASS / 2 + 1;
        let source = FakeSource::repeating(rows, 1, &[RowStatus::Same]);
        let font = egui::FontId::proportional(14.0);
        let mut pass_sizes = Vec::new();
        let mut contexts: Vec<std::sync::Weak<egui::Context>> = Vec::new();

        let widths = measure(
            &source,
            1.0,
            &font,
            &ca_ui::worker::Cancel::new(),
            |layouts, fonts| {
                if let Some(previous) = contexts.last() {
                    assert!(
                        previous.upgrade().is_none(),
                        "the previous font context survived into another batch"
                    );
                }
                pass_sizes.push(layouts);
                contexts.push(Arc::downgrade(fonts));
            },
        );

        assert!(widths.is_some());
        assert_eq!(pass_sizes.len(), 2, "the source should require two batches");
        assert_eq!(pass_sizes.iter().sum::<usize>(), rows * 2 + 1);
        assert!(
            pass_sizes.iter().all(|&size| size <= LAYOUTS_PER_PASS),
            "one batch laid out more than its cache bound: {pass_sizes:?}"
        );
        assert_eq!(
            contexts
                .iter()
                .filter(|context| context.upgrade().is_some())
                .count(),
            0,
            "font contexts and their galley caches must be dropped after each batch"
        );
    }
}
