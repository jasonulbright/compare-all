//! Mapping between comparison rows and the overview strip.
//!
//! The strip is a fixed height whatever the comparison's size, so several rows
//! usually share one pixel and the mapping is lossy in that direction only. The
//! reduction runs once per change of the model, the display filter or the
//! strip's height, never per frame.
//!
//! The class painted for a row belongs to the view, so the strip is generic
//! over it and asks only how loudly a class speaks for a pixel it shares.

use std::ops::Range;

/// The smallest viewport marker that stays grabbable on a long comparison.
pub const MINIMUM_MARKER: f32 = 6.0;

/// How loudly a class speaks for the pixel it shares.
///
/// A rank of zero paints nothing, so a matching row never claims a pixel.
pub trait Severity: Copy {
    /// The rank, with the larger value winning the pixel.
    fn rank(self) -> u8;
}

/// Whole pixels a float height covers.
fn pixels_of(height: f32) -> u32 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let pixels = height.max(0.0).floor() as u32;
    pixels
}

/// What one pixel row of the strip stands for.
///
/// Several comparison rows usually share one pixel, and the strongest of them
/// decides the pixel's color, so a single important difference in a million
/// matching rows is still visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bucket<C> {
    class: Option<C>,
}

impl<C> Default for Bucket<C> {
    fn default() -> Self {
        Self { class: None }
    }
}

impl<C: Severity> Bucket<C> {
    /// The class this pixel paints, or nothing when no differing row fell in
    /// it.
    #[must_use]
    pub const fn class(self) -> Option<C>
    where
        C: Copy,
    {
        self.class
    }

    fn admit(&mut self, class: C) {
        if class.rank() > self.class.map_or(0, Severity::rank) {
            self.class = Some(class);
        }
    }
}

/// The strip's differing rows reduced to one entry per pixel.
///
/// Built once whenever the model, the display filter or the strip's height
/// changes, so painting a frame costs the strip's height rather than the
/// comparison's length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Strip<C> {
    buckets: Vec<Bucket<C>>,
    rows: usize,
    height: u32,
}

impl<C> Default for Strip<C> {
    fn default() -> Self {
        Self {
            buckets: Vec::new(),
            rows: 0,
            height: 0,
        }
    }
}

impl<C: Severity> Strip<C> {
    /// Reduce `rows` positions into one bucket per pixel of `height`.
    ///
    /// `class_of` is asked for each position once and returns nothing for a row
    /// that leaves the strip blank.
    #[must_use]
    pub fn from_rows(
        height: f32,
        rows: usize,
        mut class_of: impl FnMut(usize) -> Option<C>,
    ) -> Self {
        let pixels = pixels_of(height);
        if pixels == 0 || rows == 0 {
            return Self {
                buckets: Vec::new(),
                rows,
                height: pixels,
            };
        }
        let span = pixels as usize;
        let mut buckets = vec![Bucket::default(); span];
        for position in 0..rows {
            let Some(class) = class_of(position) else {
                continue;
            };
            let bucket = position.saturating_mul(span) / rows;
            if let Some(slot) = buckets.get_mut(bucket.min(span - 1)) {
                slot.admit(class);
            }
        }
        Self {
            buckets,
            rows,
            height: pixels,
        }
    }

    /// Reduce difference sections into one bucket per pixel of `height`.
    ///
    /// Sections are handed over as ranges rather than row by row, so the cost
    /// is the number of sections and a comparison of tens of millions of rows
    /// is reduced without being walked.
    #[must_use]
    pub fn from_sections(
        height: f32,
        rows: usize,
        sections: impl IntoIterator<Item = (Range<usize>, C)>,
    ) -> Self {
        let pixels = pixels_of(height);
        if pixels == 0 || rows == 0 {
            return Self {
                buckets: Vec::new(),
                rows,
                height: pixels,
            };
        }
        let span = pixels as usize;
        let mut buckets = vec![Bucket::default(); span];
        for (range, class) in sections {
            let first = range.start.saturating_mul(span) / rows;
            let last = range.end.saturating_sub(1).saturating_mul(span) / rows;
            for index in first..=last.min(span - 1) {
                if let Some(slot) = buckets.get_mut(index) {
                    slot.admit(class);
                }
            }
        }
        Self {
            buckets,
            rows,
            height: pixels,
        }
    }

    /// True when the strip was built for the same model size and height.
    #[must_use]
    pub fn matches(&self, height: f32, rows: usize) -> bool {
        self.height == pixels_of(height) && self.rows == rows
    }

    /// The buckets, one per pixel from the strip's top.
    #[must_use]
    pub fn buckets(&self) -> &[Bucket<C>] {
        &self.buckets
    }

    /// The class one pixel of the strip is painted as, where it has one.
    #[must_use]
    pub fn class_at(&self, pixel: usize) -> Option<C> {
        self.buckets.get(pixel).and_then(|bucket| bucket.class)
    }

    /// How many pixels the strip covers.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// How many pixels the strip was built for.
    #[must_use]
    pub fn pixels(&self) -> usize {
        self.buckets.len()
    }

    /// How many display rows the strip covers.
    #[must_use]
    pub const fn rows(&self) -> usize {
        self.rows
    }

    /// True when nothing has been worked out yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buckets.is_empty()
    }

    /// The geometry of the strip as it was built.
    fn geometry(&self) -> Thumbnail {
        #[allow(clippy::cast_precision_loss)]
        let height = self.buckets.len() as f32;
        Thumbnail::new(height, self.rows)
    }

    /// The display row a pixel of the strip stands for.
    #[must_use]
    pub fn row_at(&self, pixel: usize) -> usize {
        if self.buckets.is_empty() || self.rows == 0 {
            return 0;
        }
        (pixel.min(self.buckets.len() - 1) * self.rows / self.buckets.len()).min(self.rows - 1)
    }

    /// The pixels the viewport covers, for the marker drawn over the strip.
    #[must_use]
    pub fn viewport_marker(&self, first_row: usize, rows_in_view: usize) -> Range<f32> {
        self.geometry()
            .with_minimum_marker(2.0)
            .viewport_marker(first_row, rows_in_view)
    }
}

/// A strip of `height` pixels standing for `rows` comparison rows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thumbnail {
    height: f32,
    rows: usize,
    minimum_marker: f32,
}

impl Thumbnail {
    /// A strip for a comparison of `rows` rows drawn `height` pixels tall.
    #[must_use]
    pub fn new(height: f32, rows: usize) -> Self {
        Self {
            height: height.max(0.0),
            rows,
            minimum_marker: MINIMUM_MARKER,
        }
    }

    /// The same strip with a different smallest marker height.
    #[must_use]
    pub const fn with_minimum_marker(mut self, pixels: f32) -> Self {
        self.minimum_marker = pixels;
        self
    }

    /// True when there is nothing to map.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows == 0 || self.height <= 0.0
    }

    /// The top of the band standing for `row`, in pixels from the strip's top.
    ///
    /// The division runs in double precision, because a comparison of tens of
    /// millions of rows reduced in single precision puts neighboring bands on
    /// the same pixel.
    #[must_use]
    pub fn row_to_y(&self, row: usize) -> f32 {
        if self.is_empty() {
            return 0.0;
        }
        #[allow(clippy::cast_precision_loss)]
        let fraction = row.min(self.rows) as f64 / self.rows as f64;
        #[allow(clippy::cast_possible_truncation)]
        let y = (fraction * f64::from(self.height)) as f32;
        y
    }

    /// The band standing for `row`, never thinner than one pixel so a single
    /// differing row in a large comparison still marks the strip.
    #[must_use]
    pub fn row_band(&self, row: usize) -> Range<f32> {
        let top = self.row_to_y(row);
        let bottom = self.row_to_y(row + 1).max(top + 1.0);
        top..bottom
    }

    /// The row a click at `y` pixels from the strip's top addresses.
    #[must_use]
    pub fn y_to_row(&self, y: f32) -> usize {
        if self.is_empty() {
            return 0;
        }
        let fraction = f64::from((y / self.height).clamp(0.0, 1.0));
        #[allow(clippy::cast_precision_loss)]
        let scaled = fraction * self.rows as f64;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let row = scaled as usize;
        row.min(self.rows.saturating_sub(1))
    }

    /// The marker showing which rows the main panes currently display.
    #[must_use]
    pub fn viewport_marker(&self, first_row: usize, visible_rows: usize) -> Range<f32> {
        if self.is_empty() {
            return 0.0..0.0;
        }
        let top = self.row_to_y(first_row);
        let bottom = self.row_to_y(first_row.saturating_add(visible_rows));
        let bottom = bottom.max(top + self.minimum_marker).min(self.height);
        let top = top.min(bottom - self.minimum_marker).max(0.0);
        top..bottom
    }

    /// The first row to display when the marker is dragged so its top sits at
    /// `y`, given how many rows fit in the panes.
    #[must_use]
    pub fn drag_to_first_row(&self, y: f32, visible_rows: usize) -> usize {
        let row = self.y_to_row(y);
        row.min(self.rows.saturating_sub(visible_rows.max(1)))
    }
}

#[cfg(test)]
mod tests {
    use super::{Severity, Strip, Thumbnail};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Class {
        Same,
        Minor,
        Orphan,
        Major,
    }

    impl Severity for Class {
        fn rank(self) -> u8 {
            match self {
                Class::Same => 0,
                Class::Minor => 1,
                Class::Orphan => 2,
                Class::Major => 3,
            }
        }
    }

    #[test]
    fn a_strip_holds_one_bucket_per_pixel() {
        let strip = Strip::from_rows(100.0, 1_000, |_| Some(Class::Major));
        assert_eq!(strip.height(), 100);
        assert_eq!(strip.buckets().len(), 100);
        assert!(strip
            .buckets()
            .iter()
            .all(|bucket| bucket.class() == Some(Class::Major)));
    }

    #[test]
    fn the_loudest_row_sharing_a_pixel_decides_its_color() {
        let strip = Strip::from_rows(1.0, 100, |position| {
            if position == 7 {
                Some(Class::Major)
            } else {
                Some(Class::Minor)
            }
        });
        assert_eq!(strip.class_at(0), Some(Class::Major));
    }

    #[test]
    fn a_matching_row_never_claims_a_pixel() {
        let strip = Strip::from_rows(4.0, 8, |_| Some(Class::Same));
        assert!(strip
            .buckets()
            .iter()
            .all(|bucket| bucket.class().is_none()));
    }

    #[test]
    fn a_single_difference_in_a_million_rows_still_marks_the_strip() {
        let strip = Strip::from_rows(400.0, 1_000_000, |position| {
            (position == 500_000).then_some(Class::Major)
        });
        let marked = strip
            .buckets()
            .iter()
            .filter(|bucket| bucket.class().is_some())
            .count();
        assert_eq!(marked, 1);
    }

    #[test]
    fn the_loudest_section_sharing_a_pixel_decides_its_color() {
        let strip =
            Strip::from_sections(1.0, 100, [(0..10, Class::Orphan), (20..30, Class::Major)]);
        assert_eq!(strip.class_at(0), Some(Class::Major));
    }

    #[test]
    fn a_single_differing_row_in_millions_still_marks_the_strip() {
        let strip: Strip<Class> =
            Strip::from_sections(400.0, 30_000_000, [(15_000_000..15_000_001, Class::Major)]);
        let marked = strip
            .buckets()
            .iter()
            .filter(|bucket| bucket.class().is_some())
            .count();
        assert_eq!(marked, 1);
    }

    #[test]
    fn a_strip_knows_when_it_is_stale() {
        let strip: Strip<Class> = Strip::from_rows(120.0, 500, |_| None);
        assert!(strip.matches(120.0, 500));
        assert!(strip.matches(120.4, 500));
        assert!(!strip.matches(121.0, 500));
        assert!(!strip.matches(120.0, 501));
    }

    #[test]
    fn a_strip_with_no_room_or_no_rows_holds_nothing() {
        let no_room: Strip<Class> = Strip::from_rows(0.0, 500, |_| None);
        let no_rows: Strip<Class> = Strip::from_sections(100.0, 0, []);
        assert!(no_room.is_empty());
        assert!(no_rows.is_empty());
        assert_eq!(no_rows.row_at(4), 0);
        assert_eq!(no_rows.viewport_marker(0, 10), 0.0..0.0);
    }

    /// Building the strip is the whole per-frame cost of the overview, so it
    /// has to stay a single pass over the model rather than a pass per frame.
    #[test]
    fn building_a_million_row_strip_asks_for_each_row_once() {
        let mut asked = 0usize;
        let strip = Strip::from_rows(600.0, 1_000_000, |position| {
            asked += 1;
            (position % 3 == 0).then_some(Class::Major)
        });
        assert_eq!(asked, 1_000_000);
        assert_eq!(strip.buckets().len(), 600);
    }

    #[test]
    fn a_pixel_maps_back_to_a_row() {
        let strip: Strip<Class> = Strip::from_rows(100.0, 1_000, |_| None);
        assert_eq!(strip.row_at(0), 0);
        assert_eq!(strip.row_at(50), 500);
        assert_eq!(strip.row_at(999), 990);
        assert_eq!(strip.rows(), 1_000);
        assert_eq!(strip.pixels(), 100);
    }

    #[test]
    fn a_strip_marker_stays_inside_the_strip() {
        let strip: Strip<Class> = Strip::from_rows(200.0, 1_000, |_| None);
        let marker = strip.viewport_marker(0, 40);
        assert!(marker.start >= 0.0 && marker.end <= 200.0);
        let bottom = strip.viewport_marker(999, 40);
        assert!(bottom.start >= 0.0 && bottom.end <= 200.0);
    }

    #[test]
    fn an_empty_strip_maps_everything_to_zero() {
        let strip = Thumbnail::new(0.0, 100);
        assert!(strip.is_empty());
        assert!((strip.row_to_y(50)).abs() < f32::EPSILON);
        assert_eq!(strip.y_to_row(10.0), 0);
        assert_eq!(strip.viewport_marker(0, 10), 0.0..0.0);
    }

    #[test]
    fn rows_spread_evenly_over_the_strip() {
        let strip = Thumbnail::new(200.0, 100);
        assert!((strip.row_to_y(0)).abs() < f32::EPSILON);
        assert!((strip.row_to_y(50) - 100.0).abs() < f32::EPSILON);
        assert!((strip.row_to_y(100) - 200.0).abs() < f32::EPSILON);
    }

    #[test]
    fn clicking_maps_back_to_the_row_under_the_pointer() {
        let strip = Thumbnail::new(200.0, 100);
        assert_eq!(strip.y_to_row(0.0), 0);
        assert_eq!(strip.y_to_row(100.0), 50);
        assert_eq!(strip.y_to_row(199.9), 99);
    }

    #[test]
    fn clicks_outside_the_strip_clamp_to_its_ends() {
        let strip = Thumbnail::new(200.0, 100);
        assert_eq!(strip.y_to_row(-50.0), 0);
        assert_eq!(strip.y_to_row(5_000.0), 99);
    }

    #[test]
    fn a_thin_band_still_covers_a_pixel() {
        let strip = Thumbnail::new(100.0, 1_000_000);
        let band = strip.row_band(500_000);
        assert!(band.end - band.start >= 1.0);
    }

    #[test]
    fn the_marker_stays_grabbable_on_a_large_comparison() {
        let strip = Thumbnail::new(400.0, 1_000_000);
        let marker = strip.viewport_marker(0, 50);
        assert!(marker.end - marker.start >= 6.0);
        assert!(marker.start >= 0.0);
        let end = strip.viewport_marker(999_950, 50);
        assert!(end.end <= 400.0);
        assert!(end.start >= 0.0);
    }

    #[test]
    fn the_marker_stays_grabbable_and_inside_a_thirty_million_row_strip() {
        let strip = Thumbnail::new(400.0, 30_000_000);
        for first in [0usize, 15_000_000, 29_999_950] {
            let marker = strip.viewport_marker(first, 50);
            assert!(marker.end - marker.start >= 6.0);
            assert!(marker.start >= 0.0 && marker.end <= 400.0);
        }
    }

    #[test]
    fn the_marker_tracks_the_displayed_rows() {
        let strip = Thumbnail::new(200.0, 100);
        let marker = strip.viewport_marker(25, 50);
        assert!((marker.start - 50.0).abs() < f32::EPSILON);
        assert!((marker.end - 150.0).abs() < f32::EPSILON);
    }

    #[test]
    fn dragging_past_the_end_stops_at_the_last_page() {
        let strip = Thumbnail::new(200.0, 100);
        assert_eq!(strip.drag_to_first_row(200.0, 20), 80);
        assert_eq!(strip.drag_to_first_row(0.0, 20), 0);
    }

    #[test]
    fn round_tripping_a_row_through_the_strip_lands_nearby() {
        let strip = Thumbnail::new(500.0, 1_000_000);
        for row in [0usize, 1, 12_345, 999_999] {
            let back = strip.y_to_row(strip.row_to_y(row));
            let gap = row.abs_diff(back);
            assert!(gap <= 2_000, "row {row} came back as {back}");
        }
    }

    /// A row a long way down the model must still resolve to its own pixel
    /// band, which is what a reduction done in single precision loses.
    #[test]
    fn rows_stay_distinguishable_thirty_million_rows_down() {
        let strip = Thumbnail::new(600.0, 30_000_000);
        let here = strip.row_to_y(29_000_000);
        let further = strip.row_to_y(29_100_000);
        assert!(further > here, "{here} and {further} share a pixel");
    }
}
