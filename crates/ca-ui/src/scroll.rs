//! Scroll arithmetic shared by the virtualized views.
//!
//! Only the rows a viewport covers are ever laid out, so every view needs the
//! same three answers: which rows a scroll offset exposes, what offset brings a
//! row into view, and what offset is legal for a given content height. Keeping
//! them here makes them testable without a frame.

use std::ops::Range;

/// The rows a viewport covers, with one row of overscan on each side so a
/// partially visible row is still laid out.
///
/// A non-positive row height would divide by zero, so it yields an empty range.
#[must_use]
pub fn visible_rows(offset: f32, viewport: f32, row_height: f32, total: usize) -> Range<usize> {
    if row_height <= 0.0 || total == 0 || viewport <= 0.0 {
        return 0..0;
    }
    let first = (offset / row_height).floor().max(0.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let first = (first as usize).min(total);
    let span = (viewport / row_height).ceil().max(0.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let span = span as usize;
    let last = first.saturating_add(span).saturating_add(1).min(total);
    first..last
}

/// The largest legal scroll offset for a content height inside a viewport.
#[must_use]
pub fn max_offset(content: f32, viewport: f32) -> f32 {
    (content - viewport).max(0.0)
}

/// `offset` clamped to the legal range.
#[must_use]
pub fn clamp_offset(offset: f32, content: f32, viewport: f32) -> f32 {
    offset.clamp(0.0, max_offset(content, viewport))
}

/// The smallest change to `offset` that brings `row` fully into the viewport.
///
/// A row already in view leaves the offset alone, so navigation does not drag
/// the display around when it does not have to.
#[must_use]
pub fn reveal(offset: f32, viewport: f32, row_height: f32, row: usize, total: usize) -> f32 {
    if row_height <= 0.0 {
        return offset;
    }
    #[allow(clippy::cast_precision_loss)]
    let top = row as f32 * row_height;
    let bottom = top + row_height;
    #[allow(clippy::cast_precision_loss)]
    let content = total as f32 * row_height;
    let wanted = if top < offset {
        top
    } else if bottom > offset + viewport {
        bottom - viewport
    } else {
        offset
    };
    clamp_offset(wanted, content, viewport)
}

/// The offset that centers `row` in the viewport.
#[must_use]
pub fn center(viewport: f32, row_height: f32, row: usize, total: usize) -> f32 {
    #[allow(clippy::cast_precision_loss)]
    let top = row as f32 * row_height;
    #[allow(clippy::cast_precision_loss)]
    let content = total as f32 * row_height;
    clamp_offset(top - (viewport - row_height) / 2.0, content, viewport)
}

/// The two panes of a comparison scroll as one surface, so the follower always
/// takes the leader's offset outright.
///
/// Horizontal offsets are shared the same way, which is why this is one
/// function rather than two.
#[must_use]
pub fn synchronize(leader: f32, follower_content: f32, viewport: f32) -> f32 {
    clamp_offset(leader, follower_content, viewport)
}

/// A scroll position held as a whole row plus the fraction of a row scrolled
/// past it.
///
/// A pixel offset accumulated in `f32` stops being able to name every row once
/// the content is taller than the mantissa can count: past roughly 880,000 rows
/// at a normal row height, neighbouring rows share one representable offset and
/// paint one pixel out. Counting rows as integers and keeping only the sub-row
/// remainder as a float removes that limit, and painting each row relative to
/// the first painted row keeps the arithmetic small however far down the model
/// the viewport sits.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RowScroll {
    row: usize,
    fraction: f32,
}

// Row counts reach a few million at most, far inside what a 64 bit float counts
// exactly, and counting rows as floats is what this type exists to confine.
#[allow(clippy::cast_precision_loss)]
impl RowScroll {
    /// A position at the top of the model.
    #[must_use]
    pub const fn top() -> Self {
        Self {
            row: 0,
            fraction: 0.0,
        }
    }

    /// The first row the viewport shows, whole or partly.
    #[must_use]
    pub const fn first_row(&self) -> usize {
        self.row
    }

    /// How far into the first row the viewport's top edge sits, as a fraction
    /// of one row.
    #[must_use]
    pub const fn fraction(&self) -> f32 {
        self.fraction
    }

    /// How many pixels the first painted row's top sits above the viewport's
    /// top edge.
    #[must_use]
    pub fn pixel_shift(&self, row_height: f32) -> f32 {
        self.fraction * row_height
    }

    /// The y of a row's top, in pixels from the viewport's top edge.
    ///
    /// Taken relative to the first painted row, so the distance measured is one
    /// screenful whatever the row's index.
    #[must_use]
    pub fn row_y(&self, position: usize, row_height: f32) -> f32 {
        #[allow(clippy::cast_precision_loss)]
        let steps = position as f32 - self.row as f32;
        steps.mul_add(row_height, -self.pixel_shift(row_height))
    }

    /// The row position under a point `y` pixels below the viewport's top
    /// edge, the inverse of [`Self::row_y`].
    ///
    /// The first painted row starts above the top edge by the pixel shift, so
    /// a count from the top edge alone lands one row early near a row's lower
    /// edge.
    #[must_use]
    pub fn row_under(&self, y: f32, row_height: f32) -> usize {
        if row_height <= 0.0 {
            return self.row;
        }
        let steps = ((y.max(0.0) + self.pixel_shift(row_height)) / row_height).floor();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let steps = steps as usize;
        self.row.saturating_add(steps)
    }

    /// The rows a viewport covers, with one row of overscan at the bottom.
    #[must_use]
    pub fn visible(&self, viewport: f32, row_height: f32, total: usize) -> Range<usize> {
        if row_height <= 0.0 || total == 0 || viewport <= 0.0 {
            return 0..0;
        }
        let first = self.row.min(total);
        let span = ((viewport + self.pixel_shift(row_height)) / row_height).ceil();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let span = span.max(0.0) as usize;
        first..first.saturating_add(span).saturating_add(1).min(total)
    }

    /// The furthest down the model a viewport may sit, as a row and a fraction.
    #[must_use]
    pub fn limit(viewport: f32, row_height: f32, total: usize) -> Self {
        if row_height <= 0.0 || total == 0 {
            return Self::top();
        }
        let rows_in_view = f64::from(viewport.max(0.0)) / f64::from(row_height);
        let furthest = (total as f64 - rows_in_view).max(0.0);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let row = furthest.floor() as usize;
        #[allow(clippy::cast_possible_truncation)]
        let fraction = (furthest - furthest.floor()) as f32;
        Self { row, fraction }
    }

    /// Hold this position inside the legal range for a model and viewport.
    pub fn clamp(&mut self, viewport: f32, row_height: f32, total: usize) {
        let limit = Self::limit(viewport, row_height, total);
        if (self.row, self.fraction) > (limit.row, limit.fraction) {
            *self = limit;
        }
    }

    /// Move by a pixel distance, which is how a wheel or a trackpad addresses
    /// the model.
    pub fn scroll_by(&mut self, pixels: f32, viewport: f32, row_height: f32, total: usize) {
        if row_height <= 0.0 {
            return;
        }
        let steps = f64::from(pixels) / f64::from(row_height);
        let here = self.row as f64 + f64::from(self.fraction) + steps;
        self.set_exact(here, viewport, row_height, total);
    }

    /// Put the top of the viewport at a position given in rows.
    fn set_exact(&mut self, rows: f64, viewport: f32, row_height: f32, total: usize) {
        let rows = rows.max(0.0);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let row = rows.floor() as usize;
        #[allow(clippy::cast_possible_truncation)]
        let fraction = (rows - rows.floor()) as f32;
        self.row = row;
        self.fraction = fraction;
        self.clamp(viewport, row_height, total);
    }

    /// Put `position` at the top of the viewport.
    pub fn go_to(&mut self, position: usize, viewport: f32, row_height: f32, total: usize) {
        self.row = position;
        self.fraction = 0.0;
        self.clamp(viewport, row_height, total);
    }

    /// The smallest move that brings `position` fully into view.
    pub fn reveal(&mut self, position: usize, viewport: f32, row_height: f32, total: usize) {
        if row_height <= 0.0 {
            return;
        }
        if position < self.row || (position == self.row && self.fraction > 0.0) {
            self.go_to(position, viewport, row_height, total);
            return;
        }
        let rows_in_view = f64::from(viewport.max(0.0)) / f64::from(row_height);
        let bottom = self.row as f64 + f64::from(self.fraction) + rows_in_view;
        if (position + 1) as f64 > bottom {
            let wanted = (position + 1) as f64 - rows_in_view;
            self.set_exact(wanted, viewport, row_height, total);
        }
    }

    /// The position that centres `position` in the viewport.
    pub fn center_on(&mut self, position: usize, viewport: f32, row_height: f32, total: usize) {
        if row_height <= 0.0 {
            return;
        }
        let rows_in_view = f64::from(viewport.max(0.0)) / f64::from(row_height);
        self.set_exact(
            position as f64 - (rows_in_view - 1.0) / 2.0,
            viewport,
            row_height,
            total,
        );
    }

    /// The scrollbar thumb for this position, in pixels down a track.
    ///
    /// Computed in `f64` so the thumb still tracks single rows on a model the
    /// track cannot resolve.
    #[must_use]
    pub fn thumb(&self, track: f32, viewport: f32, row_height: f32, total: usize) -> Range<f32> {
        if track <= 0.0 || total == 0 || row_height <= 0.0 {
            return 0.0..track.max(0.0);
        }
        let rows_in_view = (f64::from(viewport.max(0.0)) / f64::from(row_height)).min(total as f64);
        let span = (rows_in_view / total as f64 * f64::from(track)).max(f64::from(MINIMUM_THUMB));
        let limit = Self::limit(viewport, row_height, total);
        let furthest = limit.row as f64 + f64::from(limit.fraction);
        let here = self.row as f64 + f64::from(self.fraction);
        let progress = if furthest <= 0.0 {
            0.0
        } else {
            here / furthest
        };
        let travel = (f64::from(track) - span).max(0.0);
        #[allow(clippy::cast_possible_truncation)]
        let top = (progress.clamp(0.0, 1.0) * travel) as f32;
        #[allow(clippy::cast_possible_truncation)]
        let height = span as f32;
        top..(top + height).min(track)
    }

    /// The position a thumb dragged so its top sits at `y` stands for.
    #[must_use]
    pub fn from_thumb(y: f32, track: f32, viewport: f32, row_height: f32, total: usize) -> Self {
        let limit = Self::limit(viewport, row_height, total);
        if track <= 0.0 || total == 0 || row_height <= 0.0 {
            return Self::top();
        }
        let rows_in_view = (f64::from(viewport.max(0.0)) / f64::from(row_height)).min(total as f64);
        let span = (rows_in_view / total as f64 * f64::from(track)).max(f64::from(MINIMUM_THUMB));
        let travel = (f64::from(track) - span).max(0.0);
        if travel <= 0.0 {
            return limit;
        }
        let progress = (f64::from(y.max(0.0)) / travel).clamp(0.0, 1.0);
        let furthest = limit.row as f64 + f64::from(limit.fraction);
        let mut here = Self::top();
        here.set_exact(progress * furthest, viewport, row_height, total);
        here
    }
}

/// The smallest scrollbar thumb that stays grabbable on a long model.
const MINIMUM_THUMB: f32 = 12.0;

/// A horizontal offset in pixels, clamped to the widest line a pane holds.
#[must_use]
pub fn clamp_horizontal(offset: f32, content_width: f32, viewport: f32) -> f32 {
    clamp_offset(offset, content_width, viewport)
}

#[cfg(test)]
mod tests {
    use super::{
        center, clamp_horizontal, clamp_offset, max_offset, reveal, synchronize, visible_rows,
        RowScroll,
    };

    #[test]
    fn a_viewport_exposes_only_the_rows_it_covers() {
        assert_eq!(visible_rows(0.0, 100.0, 10.0, 1_000), 0..11);
        assert_eq!(visible_rows(95.0, 100.0, 10.0, 1_000), 9..20);
    }

    #[test]
    fn the_range_never_runs_past_the_model() {
        assert_eq!(visible_rows(0.0, 100.0, 10.0, 4), 0..4);
        assert_eq!(visible_rows(1e9, 100.0, 10.0, 4), 4..4);
        assert_eq!(visible_rows(0.0, 100.0, 10.0, 0), 0..0);
    }

    #[test]
    fn degenerate_metrics_expose_nothing() {
        assert_eq!(visible_rows(0.0, 100.0, 0.0, 10), 0..0);
        assert_eq!(visible_rows(0.0, 0.0, 10.0, 10), 0..0);
    }

    #[test]
    fn a_million_rows_resolve_to_a_short_range() {
        let range = visible_rows(5_000_000.0, 800.0, 16.0, 1_000_000);
        assert_eq!(range.start, 312_500);
        assert!(range.len() <= 52, "laid out {} rows", range.len());
    }

    #[test]
    fn offsets_clamp_to_the_content() {
        assert!((max_offset(500.0, 100.0) - 400.0).abs() < f32::EPSILON);
        assert!((max_offset(50.0, 100.0)).abs() < f32::EPSILON);
        assert!((clamp_offset(-5.0, 500.0, 100.0)).abs() < f32::EPSILON);
        assert!((clamp_offset(9_999.0, 500.0, 100.0) - 400.0).abs() < f32::EPSILON);
    }

    #[test]
    fn revealing_a_visible_row_changes_nothing() {
        let offset = reveal(100.0, 100.0, 10.0, 12, 1_000);
        assert!((offset - 100.0).abs() < f32::EPSILON);
    }

    #[test]
    fn revealing_scrolls_the_shortest_distance() {
        assert!((reveal(100.0, 100.0, 10.0, 5, 1_000) - 50.0).abs() < f32::EPSILON);
        assert!((reveal(100.0, 100.0, 10.0, 25, 1_000) - 160.0).abs() < f32::EPSILON);
    }

    #[test]
    fn centering_puts_the_row_in_the_middle() {
        let offset = center(100.0, 10.0, 50, 1_000);
        assert!((offset - 455.0).abs() < f32::EPSILON);
        assert!((center(100.0, 10.0, 0, 1_000)).abs() < f32::EPSILON);
    }

    #[test]
    fn the_follower_takes_the_leaders_offset_within_its_own_limits() {
        assert!((synchronize(300.0, 1_000.0, 100.0) - 300.0).abs() < f32::EPSILON);
        assert!((synchronize(300.0, 150.0, 100.0) - 50.0).abs() < f32::EPSILON);
    }

    #[test]
    fn a_horizontal_offset_clamps_to_the_widest_line() {
        assert!((clamp_horizontal(500.0, 300.0, 100.0) - 200.0).abs() < f32::EPSILON);
        assert!((clamp_horizontal(-10.0, 300.0, 100.0)).abs() < f32::EPSILON);
    }

    const HEIGHT: f32 = 19.0;
    const VIEWPORT: f32 = 800.0;

    #[test]
    fn a_fresh_position_sits_at_the_top() {
        let position = RowScroll::top();
        assert_eq!(position.first_row(), 0);
        assert!(position.fraction().abs() < f32::EPSILON);
        assert!(position.row_y(0, HEIGHT).abs() < f32::EPSILON);
    }

    #[test]
    fn scrolling_by_pixels_splits_into_rows_and_a_remainder() {
        let mut position = RowScroll::top();
        position.scroll_by(HEIGHT * 3.5, VIEWPORT, HEIGHT, 1_000);
        assert_eq!(position.first_row(), 3);
        assert!((position.fraction() - 0.5).abs() < 1e-4);
        assert!((position.pixel_shift(HEIGHT) - HEIGHT / 2.0).abs() < 1e-3);
    }

    #[test]
    fn a_position_never_runs_past_the_last_page() {
        let mut position = RowScroll::top();
        position.scroll_by(1e9, VIEWPORT, HEIGHT, 1_000);
        let limit = RowScroll::limit(VIEWPORT, HEIGHT, 1_000);
        assert_eq!(position, limit);
        let range = position.visible(VIEWPORT, HEIGHT, 1_000);
        assert_eq!(range.end, 1_000);
    }

    #[test]
    fn a_viewport_lays_out_only_what_it_covers() {
        let mut position = RowScroll::top();
        position.go_to(312_500, VIEWPORT, HEIGHT, 1_000_000);
        let range = position.visible(VIEWPORT, HEIGHT, 1_000_000);
        assert_eq!(range.start, 312_500);
        assert!(range.len() <= 45, "laid out {} rows", range.len());
    }

    #[test]
    fn degenerate_metrics_expose_no_rows() {
        let position = RowScroll::top();
        assert_eq!(position.visible(VIEWPORT, 0.0, 10), 0..0);
        assert_eq!(position.visible(0.0, HEIGHT, 10), 0..0);
        assert_eq!(position.visible(VIEWPORT, HEIGHT, 0), 0..0);
    }

    #[test]
    fn revealing_a_visible_row_leaves_the_position_alone() {
        let mut position = RowScroll::top();
        position.go_to(100, VIEWPORT, HEIGHT, 10_000);
        let before = position;
        position.reveal(105, VIEWPORT, HEIGHT, 10_000);
        assert_eq!(position, before);
    }

    #[test]
    fn revealing_scrolls_the_shortest_distance_in_either_direction() {
        let mut position = RowScroll::top();
        position.go_to(100, VIEWPORT, HEIGHT, 10_000);
        position.reveal(20, VIEWPORT, HEIGHT, 10_000);
        assert_eq!(position.first_row(), 20);
        position.reveal(500, VIEWPORT, HEIGHT, 10_000);
        let range = position.visible(VIEWPORT, HEIGHT, 10_000);
        assert!(range.contains(&500));
        assert!(position.first_row() < 500);
    }

    #[test]
    fn centering_puts_the_row_in_the_middle_of_the_viewport() {
        let mut position = RowScroll::top();
        position.center_on(5_000, VIEWPORT, HEIGHT, 10_000);
        let middle = position.row_y(5_000, HEIGHT) + HEIGHT / 2.0;
        assert!((middle - VIEWPORT / 2.0).abs() <= HEIGHT);
    }

    /// Five million rows is past the point where a pixel offset accumulated in
    /// `f32` can name a single row, so every check here is about rows staying
    /// distinguishable that far down.
    #[test]
    fn five_million_rows_stay_addressable_one_row_at_a_time() {
        const TOTAL: usize = 5_000_000;
        for start in [0usize, 880_000, 2_500_000, TOTAL - 60] {
            let mut here = RowScroll::top();
            here.go_to(start, VIEWPORT, HEIGHT, TOTAL);
            let mut next = RowScroll::top();
            next.go_to(start + 1, VIEWPORT, HEIGHT, TOTAL);
            assert_ne!(
                here,
                next,
                "rows {start} and {} share a scroll position",
                start + 1
            );
        }
    }

    #[test]
    fn painted_rows_stay_one_row_height_apart_five_million_rows_down() {
        const TOTAL: usize = 5_000_000;
        let mut position = RowScroll::top();
        position.go_to(4_000_000, VIEWPORT, HEIGHT, TOTAL);
        position.scroll_by(HEIGHT / 2.0, VIEWPORT, HEIGHT, TOTAL);
        assert_eq!(position.first_row(), 4_000_000);
        let first = position.first_row();
        let mut previous = position.row_y(first, HEIGHT);
        for position_index in first + 1..first + 40 {
            let y = position.row_y(position_index, HEIGHT);
            assert!(
                (y - previous - HEIGHT).abs() < 0.01,
                "row {position_index} sits {} from its neighbour",
                y - previous
            );
            previous = y;
        }
        // The first painted row straddles the top edge by half a row, and that
        // remainder survives at this depth.
        assert!((position.pixel_shift(HEIGHT) - HEIGHT / 2.0).abs() < 0.05);
    }

    #[test]
    fn the_thumb_stays_inside_its_track_and_grabbable() {
        const TOTAL: usize = 5_000_000;
        let track = 700.0_f32;
        for row in [0usize, 1, 2_500_000, TOTAL - 1] {
            let mut position = RowScroll::top();
            position.go_to(row, VIEWPORT, HEIGHT, TOTAL);
            let thumb = position.thumb(track, VIEWPORT, HEIGHT, TOTAL);
            assert!(thumb.start >= 0.0, "thumb starts at {}", thumb.start);
            assert!(thumb.end <= track, "thumb ends at {}", thumb.end);
            assert!(thumb.end - thumb.start >= 11.0);
        }
    }

    #[test]
    fn dragging_the_thumb_round_trips_through_the_position() {
        const TOTAL: usize = 5_000_000;
        let track = 700.0_f32;
        let mut position = RowScroll::top();
        position.go_to(2_500_000, VIEWPORT, HEIGHT, TOTAL);
        let thumb = position.thumb(track, VIEWPORT, HEIGHT, TOTAL);
        let back = RowScroll::from_thumb(thumb.start, track, VIEWPORT, HEIGHT, TOTAL);
        let gap = back.first_row().abs_diff(position.first_row());
        assert!(gap <= TOTAL / 50, "came back {gap} rows away");
    }

    #[test]
    fn dragging_past_the_ends_clamps() {
        const TOTAL: usize = 1_000;
        let top = RowScroll::from_thumb(-50.0, 400.0, VIEWPORT, HEIGHT, TOTAL);
        assert_eq!(top, RowScroll::top());
        let bottom = RowScroll::from_thumb(9_999.0, 400.0, VIEWPORT, HEIGHT, TOTAL);
        assert_eq!(bottom, RowScroll::limit(VIEWPORT, HEIGHT, TOTAL));
    }

    #[test]
    fn a_model_shorter_than_the_viewport_never_scrolls() {
        let mut position = RowScroll::top();
        position.scroll_by(5_000.0, VIEWPORT, HEIGHT, 3);
        assert_eq!(position, RowScroll::top());
        assert_eq!(position.visible(VIEWPORT, HEIGHT, 3), 0..3);
    }

    #[test]
    fn a_point_maps_to_the_row_painted_under_it_at_a_fractional_offset() {
        const TOTAL: usize = 1_000;
        let mut position = RowScroll::top();
        position.scroll_by(HEIGHT * 2.5, VIEWPORT, HEIGHT, TOTAL);
        let y = HEIGHT * 0.75;
        assert_eq!(position.row_under(y, HEIGHT), 3);
        for row in 2..10 {
            let top = position.row_y(row, HEIGHT);
            assert_eq!(position.row_under(top + HEIGHT * 0.5, HEIGHT), row);
        }
    }
}
