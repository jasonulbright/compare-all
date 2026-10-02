//! Turning a line of text into display columns.
//!
//! The panes are monospaced, so a line's width is a count of columns rather
//! than a measured pixel width, and a viewport covers a window of columns. The
//! whole point of this module is that nothing here may cost the length of the
//! line: a pane showing eighty columns of a twenty megabyte line does eighty
//! columns of work.
//!
//! Two facts about a line make that possible and are measured once, when the
//! comparison lands:
//!
//! - a line of single byte characters with no tab has column numbers equal to
//!   byte offsets, so any window of it is a byte slice;
//! - every other line needs counting, which is bounded by
//!   [`EXACT_COLUMN_LIMIT`]. Past that bound a line is windowed by bytes
//!   instead, which shifts the text of that one line sideways by the number of
//!   multi-byte characters and tabs to its left.

use std::borrow::Cow;

/// Columns one tab advances to the next multiple of.
pub const TAB_WIDTH: usize = 8;

/// The longest line whose columns are counted exactly.
const EXACT_COLUMN_LIMIT: usize = 64 * 1024;

/// What painting needs to know about one line, measured once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LineMetrics {
    /// The line's width in display columns.
    pub columns: u32,
    /// True when column numbers and byte offsets are the same value, which is
    /// the case for a line of single byte characters holding no tab.
    pub direct: bool,
}

/// Measure one line.
#[must_use]
pub fn measure(line: &str) -> LineMetrics {
    if line.is_ascii() && !line.as_bytes().contains(&b'\t') {
        return LineMetrics {
            columns: u32::try_from(line.len()).unwrap_or(u32::MAX),
            direct: true,
        };
    }
    if line.len() > EXACT_COLUMN_LIMIT {
        return LineMetrics {
            columns: u32::try_from(line.len()).unwrap_or(u32::MAX),
            direct: true,
        };
    }
    let mut columns = 0usize;
    for character in line.chars() {
        columns = advance(columns, character);
    }
    LineMetrics {
        columns: u32::try_from(columns).unwrap_or(u32::MAX),
        direct: false,
    }
}

/// The column a character lands on, given the column before it.
fn advance(column: usize, character: char) -> usize {
    if character == '\t' {
        column / TAB_WIDTH * TAB_WIDTH + TAB_WIDTH
    } else {
        column + 1
    }
}

/// The columns of byte offsets of one line, asked for in increasing order.
///
/// Each column is counted on from the one before it, so the edges of every
/// token of a line cost one pass over the line. The columns are the ones
/// [`measure`] and [`window`] place the text at.
pub struct Columns<'a> {
    line: &'a str,
    direct: bool,
    byte: usize,
    column: usize,
}

impl<'a> Columns<'a> {
    /// Columns of `line`, counted from its start.
    #[must_use]
    pub fn new(line: &'a str) -> Self {
        Self {
            line,
            direct: measure(line).direct,
            byte: 0,
            column: 0,
        }
    }

    /// The column `byte` sits at. An offset smaller than the one asked for
    /// before is counted again from the start of the line.
    pub fn at(&mut self, byte: usize) -> u32 {
        let byte = byte.min(self.line.len());
        if self.direct {
            return u32::try_from(byte).unwrap_or(u32::MAX);
        }
        if byte < self.byte {
            self.byte = 0;
            self.column = 0;
        }
        if let Some(part) = self.line.get(self.byte..byte) {
            for character in part.chars() {
                self.column = advance(self.column, character);
            }
            self.byte = byte;
        }
        u32::try_from(self.column).unwrap_or(u32::MAX)
    }
}

/// The column a byte offset sits at.
///
/// Used once per inline span when a comparison lands, never while painting.
#[must_use]
pub fn column_of_byte(line: &str, byte: usize) -> u32 {
    let byte = byte.min(line.len());
    let Some(prefix) = line.get(..byte) else {
        return 0;
    };
    if prefix.is_ascii() && !prefix.as_bytes().contains(&b'\t') {
        return u32::try_from(prefix.len()).unwrap_or(u32::MAX);
    }
    let mut column = 0usize;
    for character in prefix.chars() {
        column = advance(column, character);
    }
    u32::try_from(column).unwrap_or(u32::MAX)
}

/// The text covering `span` columns starting at `from`, with tabs already
/// expanded so the caller can place it at a fixed column pitch.
///
/// Borrows the line where it can and allocates only the window itself, so the
/// cost is the window's width rather than the line's.
#[must_use]
pub fn window(line: &str, metrics: LineMetrics, from: usize, span: usize) -> Cow<'_, str> {
    if span == 0 || from >= metrics.columns as usize {
        return Cow::Borrowed("");
    }
    let end = from.saturating_add(span);
    if metrics.direct {
        let start = floor_boundary(line, from);
        let stop = floor_boundary(line, end.min(line.len()));
        return Cow::Borrowed(line.get(start..stop).unwrap_or(""));
    }
    let mut out = String::with_capacity(span);
    let mut column = 0usize;
    for character in line.chars() {
        if column >= end {
            break;
        }
        let next = advance(column, character);
        if next > from {
            if character == '\t' {
                // A tab straddling the left edge contributes only the part of
                // its run that falls inside the window.
                for filled in column.max(from)..next.min(end) {
                    let _ = filled;
                    out.push(' ');
                }
            } else if column >= from {
                out.push(character);
            }
        }
        column = next;
    }
    Cow::Owned(out)
}

/// The largest character boundary at or below `byte`.
fn floor_boundary(line: &str, byte: usize) -> usize {
    let mut byte = byte.min(line.len());
    while byte > 0 && !line.is_char_boundary(byte) {
        byte -= 1;
    }
    byte
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation)]
mod tests {
    use super::{column_of_byte, measure, window, Columns, LineMetrics, TAB_WIDTH};

    #[test]
    fn a_plain_line_measures_as_its_bytes() {
        let metrics = measure("abcdef");
        assert_eq!(metrics.columns, 6);
        assert!(metrics.direct);
    }

    #[test]
    fn a_tab_advances_to_the_next_stop() {
        assert_eq!(measure("\tx").columns, TAB_WIDTH as u32 + 1);
        assert_eq!(measure("ab\tx").columns, TAB_WIDTH as u32 + 1);
        assert!(!measure("ab\tx").direct);
    }

    #[test]
    fn multi_byte_characters_count_as_one_column_each() {
        let metrics = measure("äöü");
        assert_eq!(metrics.columns, 3);
        assert!(!metrics.direct);
    }

    #[test]
    fn columns_counted_on_agree_with_a_count_from_the_start() {
        let line = "a\t\u{e4}b\tc\u{1f600}d";
        let mut columns = Columns::new(line);
        let edges = line
            .char_indices()
            .map(|(byte, _)| byte)
            .chain(std::iter::once(line.len()));
        for byte in edges {
            assert_eq!(columns.at(byte), column_of_byte(line, byte), "{byte}");
        }
        assert_eq!(columns.at(0), 0);
        assert_eq!(Columns::new("plain").at(3), 3);
    }

    #[test]
    fn a_byte_offset_becomes_a_column() {
        assert_eq!(column_of_byte("abc", 2), 2);
        assert_eq!(column_of_byte("äöü", 4), 2);
        assert_eq!(column_of_byte("abc", 99), 3);
        assert_eq!(column_of_byte("\tx", 1), TAB_WIDTH as u32);
    }

    #[test]
    fn a_window_takes_only_the_columns_asked_for() {
        let line = "abcdefghij";
        let metrics = measure(line);
        assert_eq!(window(line, metrics, 0, 4).as_ref(), "abcd");
        assert_eq!(window(line, metrics, 4, 3).as_ref(), "efg");
        assert_eq!(window(line, metrics, 8, 99).as_ref(), "ij");
        assert_eq!(window(line, metrics, 40, 4).as_ref(), "");
        assert_eq!(window(line, metrics, 0, 0).as_ref(), "");
    }

    #[test]
    fn a_window_of_a_multi_byte_line_cuts_on_characters() {
        let line = "äöüxyz";
        let metrics = measure(line);
        assert_eq!(window(line, metrics, 0, 3).as_ref(), "äöü");
        assert_eq!(window(line, metrics, 3, 3).as_ref(), "xyz");
        assert_eq!(window(line, metrics, 1, 2).as_ref(), "öü");
    }

    #[test]
    fn a_window_expands_tabs_to_their_columns() {
        let line = "a\tb";
        let metrics = measure(line);
        assert_eq!(window(line, metrics, 0, 10).as_ref(), "a       b");
        // The window opens inside the tab's run, so only its tail arrives.
        assert_eq!(window(line, metrics, 4, 4).as_ref(), "    ");
        assert_eq!(window(line, metrics, 8, 1).as_ref(), "b");
    }

    #[test]
    fn a_very_long_line_is_measured_without_counting_it() {
        let line = "x".repeat(20 * 1024 * 1024);
        let started = std::time::Instant::now();
        let metrics = measure(&line);
        assert!(metrics.direct);
        assert_eq!(metrics.columns, 20 * 1024 * 1024);
        let sliced = window(&line, metrics, 1_000_000, 80);
        assert_eq!(sliced.len(), 80);
        assert!(
            started.elapsed().as_millis() < 500,
            "measuring took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_window_of_an_empty_line_is_empty() {
        assert_eq!(window("", LineMetrics::default(), 0, 10).as_ref(), "");
    }
}
