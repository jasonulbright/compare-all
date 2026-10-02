//! Writing a changed cell back into the text a table was parsed from.
//!
//! A cell edit is a replacement of one byte range of the source text. Every
//! byte outside that range is kept, so the delimiter, the quoting of the other
//! cells, the line endings and any text the parser skipped all survive an edit
//! unchanged. What is decided here is the text that goes into the range: a
//! value the syntax cannot carry as it stands is quoted, and a value no quoting
//! can carry is refused rather than written in a form that parses differently.

use crate::parse::{FieldSyntax, ParseOptions, Table};
use std::ops::Range;

/// One replacement of source text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellWrite {
    /// Byte range of the source text the new text replaces.
    pub range: Range<usize>,
    /// The text that goes into the range.
    pub text: String,
}

/// Why a value cannot be written into a cell.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum WriteError {
    /// The row is short or does not exist on this side.
    #[error("this side has no cell there")]
    NoCell,
    /// The value holds a delimiter, a line break or edge whitespace, and the
    /// syntax has no text qualifier to protect it.
    #[error("the value needs quoting, and this file has no text qualifier")]
    NeedsQualifier,
    /// A fixed width field cannot hold the value without moving every column
    /// after it.
    #[error("the value is longer than the {width} characters of this field")]
    TooWide {
        /// Width of the field in characters.
        width: usize,
    },
    /// A fixed width line cannot hold a line break.
    #[error("a fixed width field cannot hold a line break")]
    LineBreak,
    /// The syntax is not one this build writes.
    #[error("this field syntax cannot be written back")]
    Unsupported,
}

/// True when the options name a syntax [`cell_write`] can produce text for.
#[must_use]
pub const fn is_writable(options: &ParseOptions) -> bool {
    matches!(
        options.syntax,
        FieldSyntax::Delimited { .. } | FieldSyntax::Fixed { .. }
    )
}

/// The replacement that puts `value` into one cell of `table`.
///
/// `options` must be the options `table` was parsed with, so a quoted value
/// reads back as the same value.
///
/// # Errors
///
/// Returns [`WriteError`] when the cell is absent or the syntax cannot carry
/// the value.
pub fn cell_write(
    table: &Table,
    options: &ParseOptions,
    row: usize,
    column: usize,
    value: &str,
) -> Result<CellWrite, WriteError> {
    let cell = table.cell(row, column).ok_or(WriteError::NoCell)?;
    let source = cell.source_range();
    let source = source.start as usize..source.end as usize;
    match &options.syntax {
        FieldSyntax::Delimited {
            delimiters,
            text_qualifier,
            consecutive_delimiters_as_one,
            surrounding_whitespace_is_delimiter,
            ..
        } => {
            let unsafe_bare = value.contains(['\n', '\r'])
                || value.chars().any(|ch| delimiters.contains(&ch))
                || text_qualifier.is_some_and(|quote| value.contains(quote))
                || (*surrounding_whitespace_is_delimiter && value.trim() != value)
                || (*consecutive_delimiters_as_one && value.is_empty())
                || (value.is_empty()
                    && table.column_count() == 1
                    && row + 1 == table.row_count()
                    && table
                        .row_source_range(row)
                        .is_some_and(|line| line.end as usize == table.source().len()));
            if !unsafe_bare && !cell.is_qualified() {
                let range = cell.value_range();
                return Ok(CellWrite {
                    range: range.start as usize..range.end as usize,
                    text: value.to_owned(),
                });
            }
            let quote = text_qualifier.ok_or(WriteError::NeedsQualifier)?;
            let mut text = String::with_capacity(value.len() + 2);
            text.push(quote);
            for ch in value.chars() {
                text.push(ch);
                if ch == quote {
                    text.push(quote);
                }
            }
            text.push(quote);
            Ok(CellWrite {
                range: source,
                text,
            })
        }
        FieldSyntax::Fixed { column_widths, .. } => {
            if value.contains(['\n', '\r']) {
                return Err(WriteError::LineBreak);
            }
            let field = table.source().get(source.clone()).unwrap_or_default();
            let span = field.chars().count();
            let length = value.chars().count();
            let ends_line = table
                .row_source_range(row)
                .is_some_and(|line| line.end as usize == source.end);
            let limit = match column_widths.get(column) {
                // The field past the last width takes the rest of the line.
                None => usize::MAX,
                Some(width) if ends_line => *width as usize,
                Some(_) => span,
            };
            if length > limit {
                return Err(WriteError::TooWide { width: limit });
            }
            let mut text = value.to_owned();
            text.extend(std::iter::repeat_n(' ', span.saturating_sub(length)));
            Ok(CellWrite {
                range: source,
                text,
            })
        }
        _ => Err(WriteError::Unsupported),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{cell_write, is_writable, WriteError};
    use crate::parse::{parse, FieldSyntax, ParseOptions};

    fn written(
        source: &str,
        options: &ParseOptions,
        row: usize,
        column: usize,
        value: &str,
    ) -> String {
        let table = parse(source, options).unwrap();
        let edit = cell_write(&table, options, row, column, value).unwrap();
        let mut out = source.to_owned();
        out.replace_range(edit.range, &edit.text);
        let reread = parse(&out, options).unwrap();
        assert_eq!(reread.cell_text(row, column), value, "the value reads back");
        out
    }

    fn csv() -> ParseOptions {
        ParseOptions {
            first_line_contains: crate::parse::FirstLineContains::CellData,
            ..ParseOptions::comma_separated()
        }
    }

    #[test]
    fn a_plain_value_replaces_only_the_cell() {
        assert_eq!(
            written("a,b,c\r\nd,e,f\r\n", &csv(), 1, 1, "XY"),
            "a,b,c\r\nd,XY,f\r\n"
        );
    }

    #[test]
    fn a_value_with_a_delimiter_or_a_quote_is_quoted() {
        assert_eq!(written("a,b\n", &csv(), 0, 0, "x,y"), "\"x,y\",b\n");
        assert_eq!(
            written("a,b\n", &csv(), 0, 1, "say \"hi\""),
            "a,\"say \"\"hi\"\"\"\n"
        );
        assert_eq!(
            written("a,b\n", &csv(), 0, 1, "two\nlines"),
            "a,\"two\nlines\"\n"
        );
    }

    #[test]
    fn a_quoted_cell_stays_quoted() {
        assert_eq!(written("\"a\",b\n", &csv(), 0, 0, "z"), "\"z\",b\n");
    }

    #[test]
    fn clearing_a_single_cell_at_end_of_file_keeps_its_row() {
        assert_eq!(written("v\na\nb", &csv(), 2, 0, ""), "v\na\n\"\"");
    }

    #[test]
    fn a_single_cell_at_end_of_file_needs_a_qualifier_to_be_cleared() {
        let options = ParseOptions {
            syntax: FieldSyntax::Delimited {
                delimiters: vec![','],
                text_qualifier: None,
                consecutive_delimiters_as_one: false,
                surrounding_whitespace_is_delimiter: true,
                unknown: crate::Unknown::new(),
            },
            first_line_contains: crate::parse::FirstLineContains::CellData,
            ..ParseOptions::default()
        };
        let table = parse("v\na\nb", &options).unwrap();
        assert_eq!(
            cell_write(&table, &options, 2, 0, ""),
            Err(WriteError::NeedsQualifier)
        );
    }

    #[test]
    fn a_tab_file_keeps_its_tabs() {
        let options = ParseOptions {
            first_line_contains: crate::parse::FirstLineContains::CellData,
            ..ParseOptions::tab_separated()
        };
        assert_eq!(written("a\tb\n", &options, 0, 1, "c"), "a\tc\n");
    }

    #[test]
    fn no_qualifier_refuses_a_value_that_needs_one() {
        let options = ParseOptions {
            syntax: FieldSyntax::Delimited {
                delimiters: vec![','],
                text_qualifier: None,
                consecutive_delimiters_as_one: false,
                surrounding_whitespace_is_delimiter: true,
                unknown: crate::Unknown::new(),
            },
            first_line_contains: crate::parse::FirstLineContains::CellData,
            ..ParseOptions::default()
        };
        let table = parse("a,b\n", &options).unwrap();
        assert_eq!(
            cell_write(&table, &options, 0, 0, "x,y"),
            Err(WriteError::NeedsQualifier)
        );
    }

    #[test]
    fn a_fixed_width_field_is_padded_and_never_widened() {
        let options = ParseOptions {
            first_line_contains: crate::parse::FirstLineContains::CellData,
            ..ParseOptions::fixed_width([3, 3])
        };
        assert_eq!(written("abcdef\n", &options, 0, 0, "x"), "x  def\n");
        let table = parse("abcdef\n", &options).unwrap();
        assert_eq!(
            cell_write(&table, &options, 0, 0, "wxyz"),
            Err(WriteError::TooWide { width: 3 })
        );
        assert_eq!(
            cell_write(&table, &options, 0, 0, "a\nb"),
            Err(WriteError::LineBreak)
        );
    }

    #[test]
    fn an_absent_cell_is_refused() {
        let table = parse("a,b\nc\n", &csv()).unwrap();
        assert_eq!(
            cell_write(&table, &csv(), 1, 1, "x"),
            Err(WriteError::NoCell)
        );
        assert_eq!(
            cell_write(&table, &csv(), 9, 0, "x"),
            Err(WriteError::NoCell)
        );
    }

    #[test]
    fn only_a_concrete_syntax_is_writable() {
        assert!(is_writable(&ParseOptions::comma_separated()));
        assert!(is_writable(&ParseOptions::fixed_width([2])));
        assert!(!is_writable(&ParseOptions::default()));
    }
}
