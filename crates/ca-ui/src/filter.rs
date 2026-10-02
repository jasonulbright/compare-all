//! The result of a display filter over a list of rows.
//!
//! A view decides which of its rows a filter keeps; what the kept rows are
//! afterwards is shared. A position on screen maps to a row of the model and
//! back through [`Visible`], so scrolling, clicking and navigation work the
//! same under every filter.

use std::ops::Range;

/// The rows a display filter shows.
///
/// The unfiltered case carries only a length, so showing every row of a very
/// large comparison costs nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Visible {
    /// Every row, in order.
    All(usize),
    /// The listed rows, in order.
    Subset(Vec<u32>),
}

impl Default for Visible {
    fn default() -> Self {
        Self::All(0)
    }
}

impl Visible {
    /// How many rows are shown.
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Self::All(count) => *count,
            Self::Subset(rows) => rows.len(),
        }
    }

    /// True when nothing is shown.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The model row shown at a display position.
    #[must_use]
    pub fn row_at(&self, position: usize) -> Option<usize> {
        match self {
            Self::All(count) => (position < *count).then_some(position),
            Self::Subset(rows) => rows.get(position).map(|row| *row as usize),
        }
    }

    /// The display position of a model row, or the position of the first row
    /// after it when the row itself is filtered out.
    #[must_use]
    pub fn position_of(&self, row: usize) -> Option<usize> {
        match self {
            Self::All(count) => (row < *count).then_some(row),
            Self::Subset(rows) => {
                let row = u32::try_from(row).unwrap_or(u32::MAX);
                match rows.binary_search(&row) {
                    Ok(position) => Some(position),
                    Err(position) => (position < rows.len()).then_some(position),
                }
            }
        }
    }

    /// True when the model row is on screen under this filter.
    #[must_use]
    pub fn shows(&self, row: usize) -> bool {
        match self {
            Self::All(count) => row < *count,
            Self::Subset(rows) => {
                u32::try_from(row).is_ok_and(|row| rows.binary_search(&row).is_ok())
            }
        }
    }
}

/// The rows of `count` that `keep` accepts, in order.
#[must_use]
pub fn select(count: usize, keep: impl Fn(usize) -> bool) -> Visible {
    Visible::Subset(
        (0..count)
            .filter(|index| keep(*index))
            .filter_map(|index| u32::try_from(index).ok())
            .collect(),
    )
}

/// The rows of every section plus `reach` rows before and after each one.
///
/// `sections` are ordered, disjoint row ranges. A row two sections both reach
/// is listed once.
#[must_use]
pub fn context(sections: &[Range<usize>], reach: usize, count: usize) -> Visible {
    let mut out: Vec<u32> = Vec::new();
    let mut cursor = 0usize;
    for section in sections {
        let start = section.start.saturating_sub(reach);
        let end = section.end.saturating_add(reach).min(count);
        for index in start.max(cursor)..end {
            if let Ok(index) = u32::try_from(index) {
                out.push(index);
            }
        }
        cursor = cursor.max(end);
    }
    Visible::Subset(out)
}

#[cfg(test)]
mod tests {
    use super::{context, select, Visible};

    #[test]
    fn the_unfiltered_result_maps_every_position_to_itself() {
        let visible = Visible::All(4);
        assert_eq!(visible.row_at(3), Some(3));
        assert_eq!(visible.row_at(4), None);
        assert_eq!(visible.position_of(2), Some(2));
        assert!(visible.shows(0));
    }

    #[test]
    fn a_hidden_row_maps_to_the_next_shown_one() {
        let visible = select(6, |row| row % 2 == 1);
        assert_eq!(visible, Visible::Subset(vec![1, 3, 5]));
        assert_eq!(visible.position_of(2), Some(1));
        assert_eq!(visible.position_of(6), None);
        assert!(!visible.shows(2));
        assert!(visible.shows(3));
    }

    #[test]
    fn context_rows_are_listed_once_where_two_sections_reach_them() {
        let visible = context(&[2..3, 5..6], 2, 10);
        assert_eq!(visible, Visible::Subset(vec![0, 1, 2, 3, 4, 5, 6, 7]));
        assert!(context(&[], 3, 10).is_empty());
    }
}
