//! Whole-file and line-range editing transforms.
//!
//! Every buffer transform replaces the covered line range in one undo group, so
//! a single undo reverses the whole conversion. The string level functions are
//! pure and reusable on their own.

use crate::buffer::{LineRange, TextBuffer};
use crate::eol::{self, EolStyle, LineEnding};

/// Tab handling settings a file format supplies.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TabSettings {
    /// Columns between tab stops. Zero is treated as one to keep conversions total.
    pub tab_stop: u32,
    /// True when the Tab key inserts spaces rather than a tab character.
    pub insert_spaces: bool,
}

impl Default for TabSettings {
    fn default() -> Self {
        Self {
            tab_stop: 8,
            insert_spaces: false,
        }
    }
}

impl TabSettings {
    /// The effective tab stop, never zero.
    #[must_use]
    pub fn stop(self) -> usize {
        self.tab_stop.max(1) as usize
    }

    /// The text one indent level inserts.
    #[must_use]
    pub fn indent_text(self) -> String {
        if self.insert_spaces {
            " ".repeat(self.stop())
        } else {
            "\t".to_owned()
        }
    }
}

/// The number of columns a character occupies when measuring tab stops.
///
/// Every character other than a tab counts as one column, so a combining mark, a
/// wide East Asian character and an emoji each advance the column by one.
///
/// Invariant: the renderer must measure columns with this same function, or a
/// converted tab lands on a different column than the one the user saw.
#[must_use]
pub fn display_width(_c: char) -> usize {
    1
}

/// Expands every tab in a single line to spaces, honoring tab stops.
#[must_use]
pub fn tabs_to_spaces_line(line: &str, settings: TabSettings) -> String {
    let stop = settings.stop();
    let mut out = String::with_capacity(line.len());
    let mut column = 0usize;
    for c in line.chars() {
        if c == '\t' {
            let width = stop - (column % stop);
            for _ in 0..width {
                out.push(' ');
            }
            column += width;
        } else {
            out.push(c);
            column += display_width(c);
        }
    }
    out
}

/// Replaces the leading whitespace of a line with tabs, then spaces for the remainder.
#[must_use]
pub fn leading_spaces_to_tabs_line(line: &str, settings: TabSettings) -> String {
    let stop = settings.stop();
    let mut column = 0usize;
    for (offset, c) in line.char_indices() {
        match c {
            ' ' => column += display_width(' '),
            '\t' => column += stop - (column % stop),
            _ => return build_indent(column, stop, &line[offset..]),
        }
    }
    // The line is whitespace only; its indentation is still normalized.
    build_indent(column, stop, "")
}

fn build_indent(column: usize, stop: usize, rest: &str) -> String {
    let mut out = String::with_capacity(rest.len() + column);
    for _ in 0..column / stop {
        out.push('\t');
    }
    for _ in 0..column % stop {
        out.push(' ');
    }
    out.push_str(rest);
    out
}

/// Replaces every run of spaces that reaches a tab stop with a tab character.
#[must_use]
pub fn spaces_to_tabs_line(line: &str, settings: TabSettings) -> String {
    let stop = settings.stop();
    let mut out = String::with_capacity(line.len());
    let mut column = 0usize;
    let mut pending = 0usize;
    let mut pending_start = 0usize;
    for c in line.chars() {
        if c == ' ' {
            if pending == 0 {
                pending_start = column;
            }
            pending += 1;
            column += display_width(' ');
            if column.is_multiple_of(stop) {
                // A run that reaches a stop collapses, but a single space that
                // merely sits on a stop must stay a space or the column shifts.
                if pending > 1 || pending_start.is_multiple_of(stop) {
                    out.push('\t');
                } else {
                    out.push(' ');
                }
                pending = 0;
            }
        } else {
            for _ in 0..pending {
                out.push(' ');
            }
            pending = 0;
            if c == '\t' {
                column += stop - (column % stop);
            } else {
                column += display_width(c);
            }
            out.push(c);
        }
    }
    for _ in 0..pending {
        out.push(' ');
    }
    out
}

/// Removes spaces and tabs from the end of a single line.
#[must_use]
pub fn trim_trailing_whitespace_line(line: &str) -> &str {
    line.trim_end_matches([' ', '\t'])
}

/// Adds one indent level to a line, leaving empty lines alone.
#[must_use]
pub fn increase_indent_line(line: &str, settings: TabSettings) -> String {
    if line.is_empty() {
        return String::new();
    }
    let mut out = settings.indent_text();
    out.push_str(line);
    out
}

/// Removes up to one indent level of leading whitespace from a line.
#[must_use]
pub fn decrease_indent_line(line: &str, settings: TabSettings) -> String {
    let stop = settings.stop();
    let mut removed = 0usize;
    let mut chars = line.char_indices();
    let mut cut = 0usize;
    while removed < stop {
        let Some((offset, c)) = chars.next() else {
            break;
        };
        match c {
            ' ' => {
                removed += 1;
                cut = offset + 1;
            }
            '\t' => {
                // A tab collapses a whole level whatever is left of the budget.
                cut = offset + 1;
                removed = stop;
            }
            _ => break,
        }
    }
    line[cut..].to_owned()
}

/// Applies `map` to every line of `text`, preserving each line's terminator.
fn map_lines(text: &str, mut map: impl FnMut(&str) -> String) -> String {
    let mut out = String::with_capacity(text.len());
    for (line, ending) in eol::lines(text) {
        out.push_str(&map(line));
        out.push_str(ending.as_str());
    }
    out
}

/// The whole-buffer line range.
#[must_use]
pub fn whole_buffer(buffer: &TextBuffer) -> LineRange {
    LineRange::new(0, buffer.len_lines())
}

/// Replaces a line range with the result of `map` applied line by line.
fn transform(buffer: &mut TextBuffer, lines: LineRange, map: impl FnMut(&str) -> String) {
    let original = buffer.line_range_text(lines);
    let replacement = map_lines(&original, map);
    if replacement != original {
        buffer.replace_lines(lines, &replacement);
    }
    buffer.seal_group();
}

/// Replaces every tab in the range with spaces, as one undo group.
pub fn tabs_to_spaces(buffer: &mut TextBuffer, lines: LineRange, settings: TabSettings) {
    transform(buffer, lines, |line| tabs_to_spaces_line(line, settings));
}

/// Replaces leading spaces in the range with tabs, as one undo group.
pub fn leading_spaces_to_tabs(buffer: &mut TextBuffer, lines: LineRange, settings: TabSettings) {
    transform(buffer, lines, |line| {
        leading_spaces_to_tabs_line(line, settings)
    });
}

/// Replaces every space run in the range that reaches a tab stop with a tab.
pub fn spaces_to_tabs(buffer: &mut TextBuffer, lines: LineRange, settings: TabSettings) {
    transform(buffer, lines, |line| spaces_to_tabs_line(line, settings));
}

/// Removes end of line whitespace across the range, as one undo group.
pub fn trim_trailing_whitespace(buffer: &mut TextBuffer, lines: LineRange) {
    transform(buffer, lines, |line| {
        trim_trailing_whitespace_line(line).to_owned()
    });
}

/// Upper cases the range, as one undo group.
pub fn to_upper_case(buffer: &mut TextBuffer, lines: LineRange) {
    transform(buffer, lines, str::to_uppercase);
}

/// Lower cases the range, as one undo group.
pub fn to_lower_case(buffer: &mut TextBuffer, lines: LineRange) {
    transform(buffer, lines, str::to_lowercase);
}

/// Adds one indent level to every line in the range, as one undo group.
pub fn increase_indent(buffer: &mut TextBuffer, lines: LineRange, settings: TabSettings) {
    transform(buffer, lines, |line| increase_indent_line(line, settings));
}

/// Removes one indent level from every line in the range, as one undo group.
pub fn decrease_indent(buffer: &mut TextBuffer, lines: LineRange, settings: TabSettings) {
    transform(buffer, lines, |line| decrease_indent_line(line, settings));
}

/// Rewrites the line endings of the range to `style`, as one undo group.
///
/// A line with no terminator, which only the last line of the buffer can have,
/// stays unterminated.
pub fn convert_line_endings(buffer: &mut TextBuffer, lines: LineRange, style: EolStyle) {
    let original = buffer.line_range_text(lines);
    let replacement = eol::convert(&original, style);
    if replacement != original {
        buffer.replace_lines(lines, &replacement);
    }
    buffer.seal_group();
}

/// Inserts a blank line above `line`, as one undo group.
pub fn insert_line_before(buffer: &mut TextBuffer, line: u32, style: EolStyle) {
    let at = buffer.line_to_char(line);
    buffer.insert(at, style.as_str());
    buffer.seal_group();
}

/// Inserts a blank line below `line`, as one undo group.
///
/// When the line has no terminator, one is added so the new line exists.
pub fn insert_line_after(buffer: &mut TextBuffer, line: u32, style: EolStyle) {
    let unterminated = buffer.line_ending(line) == Some(LineEnding::None);
    let at = buffer.line_char_range(LineRange::single(line)).end;
    let text = if unterminated {
        let mut t = style.as_str().to_owned();
        t.push_str(style.as_str());
        t
    } else {
        style.as_str().to_owned()
    };
    buffer.insert(at, &text);
    buffer.seal_group();
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const FOUR: TabSettings = TabSettings {
        tab_stop: 4,
        insert_spaces: false,
    };

    #[test]
    fn tabs_expand_to_the_next_stop() {
        assert_eq!(tabs_to_spaces_line("\tab", FOUR), "    ab");
        assert_eq!(tabs_to_spaces_line("a\tb", FOUR), "a   b");
        assert_eq!(tabs_to_spaces_line("abcd\te", FOUR), "abcd    e");
    }

    #[test]
    fn every_character_is_one_column_wide() {
        for c in ['a', 'あ', '\u{0301}', '🎉'] {
            assert_eq!(display_width(c), 1);
        }
        assert_eq!(tabs_to_spaces_line("あ\tx", FOUR), "あ   x");
    }

    #[test]
    fn zero_tab_stop_does_not_divide_by_zero() {
        let settings = TabSettings {
            tab_stop: 0,
            insert_spaces: false,
        };
        assert_eq!(tabs_to_spaces_line("\ta", settings), " a");
    }

    #[test]
    fn leading_spaces_become_tabs() {
        assert_eq!(leading_spaces_to_tabs_line("      x", FOUR), "\t  x");
        assert_eq!(leading_spaces_to_tabs_line("  x  y", FOUR), "  x  y");
        assert_eq!(leading_spaces_to_tabs_line("no indent", FOUR), "no indent");
    }

    #[test]
    fn space_runs_become_tabs() {
        assert_eq!(spaces_to_tabs_line("a   b", FOUR), "a\tb");
        assert_eq!(spaces_to_tabs_line("abc d", FOUR), "abc d");
    }

    #[test]
    fn trailing_whitespace_is_trimmed() {
        assert_eq!(trim_trailing_whitespace_line("a \t "), "a");
        assert_eq!(trim_trailing_whitespace_line(""), "");
    }

    #[test]
    fn indent_and_outdent_are_inverse_for_simple_indents() {
        let indented = increase_indent_line("x", FOUR);
        assert_eq!(indented, "\tx");
        assert_eq!(decrease_indent_line(&indented, FOUR), "x");
        assert_eq!(decrease_indent_line("      x", FOUR), "  x");
        assert_eq!(decrease_indent_line("x", FOUR), "x");
    }

    #[test]
    fn indent_skips_empty_lines() {
        assert_eq!(increase_indent_line("", FOUR), "");
    }

    #[test]
    fn transform_is_one_undo_group() {
        let mut buf = TextBuffer::from_text("\ta\n\tb\n");
        let all = whole_buffer(&buf);
        tabs_to_spaces(&mut buf, all, FOUR);
        assert_eq!(buf.text(), "    a\n    b\n");
        assert!(buf.undo().unwrap());
        assert_eq!(buf.text(), "\ta\n\tb\n");
    }

    #[test]
    fn transform_limited_to_a_line_range() {
        let mut buf = TextBuffer::from_text("a  \nb  \nc  \n");
        trim_trailing_whitespace(&mut buf, LineRange::new(1, 2));
        assert_eq!(buf.text(), "a  \nb\nc  \n");
    }

    #[test]
    fn no_change_records_no_undo_step() {
        let mut buf = TextBuffer::from_text("clean\n");
        let all = whole_buffer(&buf);
        trim_trailing_whitespace(&mut buf, all);
        assert!(!buf.can_undo());
        assert!(!buf.is_modified());
    }

    #[test]
    fn case_conversion_covers_the_range() {
        let mut buf = TextBuffer::from_text("Mixed Case\n");
        let all = whole_buffer(&buf);
        to_upper_case(&mut buf, all);
        assert_eq!(buf.text(), "MIXED CASE\n");
        to_lower_case(&mut buf, all);
        assert_eq!(buf.text(), "mixed case\n");
    }

    #[test]
    fn line_ending_conversion_undoes_as_one_group() {
        let mut buf = TextBuffer::from_text("a\r\nb\rc\n");
        let all = whole_buffer(&buf);
        convert_line_endings(&mut buf, all, EolStyle::Lf);
        assert_eq!(buf.text(), "a\nb\nc\n");
        assert!(buf.undo().unwrap());
        assert_eq!(buf.text(), "a\r\nb\rc\n");
    }

    #[test]
    fn blank_lines_inserted_around_a_line() {
        let mut buf = TextBuffer::from_text("a\nb\n");
        insert_line_before(&mut buf, 1, EolStyle::Lf);
        assert_eq!(buf.text(), "a\n\nb\n");
        let mut buf = TextBuffer::from_text("a");
        insert_line_after(&mut buf, 0, EolStyle::Lf);
        assert_eq!(buf.text(), "a\n\n");
    }
}
