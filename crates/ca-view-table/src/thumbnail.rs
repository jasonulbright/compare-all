//! The strip beside the grids that shows where the differences are.
//!
//! The reduction itself is [`ca_ui::thumbnail`]. What is decided here is which
//! class a display row speaks for, once the unimportant setting has had its
//! say. A row present on one side only reports the left-only class whichever
//! side it is on, because the strip shows that a row is an orphan and not which
//! side holds it.

use crate::model::{row_class, Grid};
use ca_ui::theme::table::CellClass;

/// The strip's colors, one per pixel of its height.
pub type Strip = ca_ui::thumbnail::Strip<CellClass>;

/// Work the strip out for a grid and a height in whole pixels.
///
/// Every display row reaches a pixel, and the most severe row in a pixel wins,
/// so an isolated difference in a very long comparison is still visible.
#[must_use]
pub fn build(grid: &Grid, pixels: usize) -> Strip {
    let rows = grid.visual_rows();
    let ignore_unimportant = grid.ignores_unimportant();
    #[allow(clippy::cast_precision_loss)]
    let height = pixels as f32;
    Strip::from_rows(height, rows, |row| {
        let status = grid.row_status(row)?;
        match row_class(status) {
            CellClass::Unimportant if ignore_unimportant => None,
            CellClass::LeftOnly | CellClass::RightOnly => Some(CellClass::LeftOnly),
            class => Some(class),
        }
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::build;
    use crate::model::Grid;
    use crate::testing::FakeSource;
    use ca_table::compare::RowStatus;
    use ca_ui::theme::table::CellClass;
    use std::sync::Arc;

    #[test]
    fn a_pixel_takes_the_most_severe_row_it_covers() {
        let mut statuses = vec![RowStatus::Same; 100];
        statuses[50] = RowStatus::Different;
        statuses[51] = RowStatus::Unimportant;
        let grid = Grid::new(Arc::new(FakeSource::new(&statuses, 1)));
        let strip = build(&grid, 10);
        assert_eq!(strip.pixels(), 10);
        assert_eq!(strip.class_at(5), Some(CellClass::Different));
        assert_eq!(strip.class_at(0), None);
    }

    #[test]
    fn one_isolated_difference_in_a_long_comparison_still_shows() {
        let mut statuses = vec![RowStatus::Same; 1_000_000];
        statuses[999_999] = RowStatus::Different;
        let grid = Grid::new(Arc::new(FakeSource::new(&statuses, 1)));
        let strip = build(&grid, 400);
        assert_eq!(strip.class_at(399), Some(CellClass::Different));
    }

    #[test]
    fn a_pixel_maps_back_to_a_row() {
        let grid = Grid::new(Arc::new(FakeSource::repeating(
            1_000,
            1,
            &[RowStatus::Same],
        )));
        let strip = build(&grid, 100);
        assert_eq!(strip.row_at(0), 0);
        assert_eq!(strip.row_at(50), 500);
        assert_eq!(strip.row_at(999), 990);
        assert_eq!(strip.rows(), 1_000);
    }

    #[test]
    fn the_marker_stays_inside_the_strip() {
        let grid = Grid::new(Arc::new(FakeSource::repeating(
            1_000,
            1,
            &[RowStatus::Same],
        )));
        let strip = build(&grid, 200);
        let marker = strip.viewport_marker(0, 40);
        assert!(marker.start >= 0.0);
        assert!(marker.end <= 200.0);
        let bottom = strip.viewport_marker(999, 40);
        assert!(bottom.end <= 200.0);
        assert!(bottom.start >= 0.0);
    }

    #[test]
    fn an_empty_comparison_makes_an_empty_strip() {
        let strip = build(&Grid::default(), 100);
        assert!(strip.is_empty());
        assert_eq!(strip.row_at(4), 0);
        assert_eq!(strip.viewport_marker(0, 10), 0.0..0.0);
    }

    #[test]
    fn ignoring_unimportant_clears_those_marks() {
        let grid_source = FakeSource::new(&[RowStatus::Unimportant], 1);
        let mut grid = Grid::new(Arc::new(grid_source));
        assert_eq!(build(&grid, 4).class_at(0), Some(CellClass::Unimportant));
        grid.set_ignore_unimportant(true);
        assert_eq!(build(&grid, 4).class_at(0), None);
    }
}
