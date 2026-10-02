//! Reading delimited and fixed width text into a table of rows of cells.
//!
//! Parsing is one forward pass over the decoded text. Nothing is copied: a
//! cell is two byte ranges into the source, one for the characters the file
//! actually holds and one for the characters that make up the cell's value.
//! A view maps a cell back to a file position through the first range; a
//! comparison reads the cell's text through the second.
//!
//! Rows may be ragged. A short row has fewer cells than its neighbours, and a
//! cell missing from a row is distinct from a cell that is present and empty.
//!
//! # Hostile input
//!
//! The parser has no recursion and no backtracking, so cost is linear in the
//! input length whatever the input contains. An unterminated text qualifier
//! ends the field at the end of the file and records a warning. Warnings are
//! capped so a file that is wrong on every line cannot exhaust memory.

use crate::detect::{detect_format, DetectOptions};
use crate::{Result, TableError, Unknown};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::fmt;
use std::ops::Range;

/// Largest decoded input accepted by the table engine.
pub const MAX_INPUT_BYTES: usize = 64 * 1024 * 1024;

/// Largest number of cells retained from one input.
///
/// This accommodates a million rows of five columns, plus the header, while
/// keeping sparse, delimiter-heavy input within a fixed allocation bound.
pub const MAX_TABLE_CELLS: usize = 6_000_000;

/// Largest number of physical rows retained from one input, including an
/// optional header row.
pub const MAX_TABLE_ROWS: usize = 1_000_001;

/// Largest number of warnings a single parse records.
pub const MAX_WARNINGS: usize = 64;

/// Whether the first line of the file holds data or column names.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum FirstLineContains {
    /// Decide from the data.
    #[default]
    Detect,
    /// The first line holds column names.
    ColumnNames,
    /// The first line holds cell data like any other line.
    CellData,
    /// A choice written by another build, carried through unchanged.
    #[serde(untagged)]
    Unknown(serde_json::Value),
}

/// How fields are separated on a line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[non_exhaustive]
pub enum FieldSyntax {
    /// Decide between delimited and fixed width from the data.
    #[serde(rename_all = "camelCase")]
    Detect {
        /// Fields written by another build, preserved verbatim.
        #[serde(flatten, default, skip_serializing_if = "Unknown::is_empty")]
        unknown: Unknown,
    },
    /// Fields separated by a delimiter character.
    #[serde(rename_all = "camelCase")]
    Delimited {
        /// Characters that separate fields. An empty set parses each line as
        /// one cell.
        #[serde(default)]
        delimiters: Vec<char>,
        /// Character that optionally surrounds a field, doubled to include
        /// itself in the data.
        #[serde(default)]
        text_qualifier: Option<char>,
        /// A run of delimiter characters forms one delimiter. Set this for
        /// whitespace separated columns. A run at the start of a line is then
        /// also one delimiter, so such a line does not open with an empty
        /// cell.
        #[serde(default)]
        consecutive_delimiters_as_one: bool,
        /// Whitespace next to a delimiter belongs to the delimiter, not to the
        /// data. Unset it to keep that whitespace in the cell.
        #[serde(default = "crate::parse::yes")]
        surrounding_whitespace_is_delimiter: bool,
        /// Fields written by another build, preserved verbatim.
        #[serde(flatten, default, skip_serializing_if = "Unknown::is_empty")]
        unknown: Unknown,
    },
    /// Fields defined by their position on the line.
    #[serde(rename_all = "camelCase")]
    Fixed {
        /// Width of each column in characters. The last width does not bound
        /// the line: characters past the sum of the widths form one further
        /// cell when any are present.
        #[serde(default)]
        column_widths: Vec<u32>,
        /// Strip whitespace from both ends of every field.
        #[serde(default = "crate::parse::yes")]
        trim: bool,
        /// Fields written by another build, preserved verbatim.
        #[serde(flatten, default, skip_serializing_if = "Unknown::is_empty")]
        unknown: Unknown,
    },
    /// A syntax written by another build, carried through unchanged. Parsing
    /// falls back to detection.
    #[serde(untagged)]
    Unknown(serde_json::Value),
}

pub(crate) fn yes() -> bool {
    true
}

impl Default for FieldSyntax {
    fn default() -> Self {
        Self::Detect {
            unknown: Unknown::new(),
        }
    }
}

impl FieldSyntax {
    /// Delimited fields with the given separators, a double quote qualifier
    /// and whitespace treated as part of the delimiter.
    #[must_use]
    pub fn delimited(delimiters: impl IntoIterator<Item = char>) -> Self {
        Self::Delimited {
            delimiters: delimiters.into_iter().collect(),
            text_qualifier: Some('"'),
            consecutive_delimiters_as_one: false,
            surrounding_whitespace_is_delimiter: true,
            unknown: Unknown::new(),
        }
    }

    /// Fixed width fields with the given character widths.
    #[must_use]
    pub fn fixed(widths: impl IntoIterator<Item = u32>) -> Self {
        Self::Fixed {
            column_widths: widths.into_iter().collect(),
            trim: true,
            unknown: Unknown::new(),
        }
    }
}

/// Everything needed to turn text into a table.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ParseOptions {
    /// How fields are separated.
    pub syntax: FieldSyntax,
    /// Whether the first line names the columns.
    pub first_line_contains: FirstLineContains,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, skip_serializing_if = "Unknown::is_empty")]
    pub unknown: Unknown,
}

impl ParseOptions {
    /// Comma separated values with a double quote text qualifier.
    #[must_use]
    pub fn comma_separated() -> Self {
        Self {
            syntax: FieldSyntax::delimited([',']),
            ..Self::default()
        }
    }

    /// Tab separated values with a double quote text qualifier.
    #[must_use]
    pub fn tab_separated() -> Self {
        Self {
            syntax: FieldSyntax::delimited(['\t']),
            ..Self::default()
        }
    }

    /// Fixed width fields with the given character widths.
    #[must_use]
    pub fn fixed_width(widths: impl IntoIterator<Item = u32>) -> Self {
        Self {
            syntax: FieldSyntax::fixed(widths),
            ..Self::default()
        }
    }
}

/// Something wrong with the input that parsing recovered from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum ParseWarning {
    /// A qualified field ran to the end of the file with no closing
    /// qualifier. The field holds the rest of the file.
    UnterminatedQualifier {
        /// Byte offset where the field opened.
        offset: u32,
    },
    /// Characters followed a closing qualifier before the next delimiter.
    /// They are part of the cell's source range but not of its value.
    TextAfterQualifier {
        /// Byte offset of the first such character.
        offset: u32,
    },
    /// Further warnings were dropped once the cap was reached.
    Truncated {
        /// Number of warnings that were not recorded.
        dropped: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CellSpan {
    source_start: u32,
    source_end: u32,
    value_start: u32,
    value_end: u32,
}

/// A parsed table: rows of cells over an owned copy of the source text.
#[derive(Clone, PartialEq)]
pub struct Table {
    text: String,
    cells: Vec<CellSpan>,
    row_starts: Vec<u32>,
    header: Option<Vec<String>>,
    column_count: usize,
    qualifier: Option<char>,
    warnings: Vec<ParseWarning>,
    first_line: FirstLineContains,
}

/// Byte length of a leading byte order mark, which belongs to the encoding and
/// not to the first cell.
pub(crate) fn bom_len(text: &str) -> usize {
    if text.starts_with('\u{feff}') {
        '\u{feff}'.len_utf8()
    } else {
        0
    }
}

/// The text with any leading byte order mark removed.
pub(crate) fn without_bom(text: &str) -> &str {
    text.get(bom_len(text)..).unwrap_or(text)
}

impl fmt::Debug for Table {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Table")
            .field("rows", &self.row_count())
            .field("columns", &self.column_count)
            .field("bytes", &self.text.len())
            .field("header", &self.header.is_some())
            .field("warnings", &self.warnings.len())
            .finish_non_exhaustive()
    }
}

/// One cell, borrowed from its table.
#[derive(Debug, Clone, Copy)]
pub struct CellRef<'a> {
    text: &'a str,
    span: CellSpan,
    qualifier: Option<char>,
}

impl<'a> CellRef<'a> {
    /// The cell's value: the characters inside any text qualifier, with a
    /// doubled qualifier collapsed to one character.
    ///
    /// The result borrows the source unless a doubled qualifier had to be
    /// collapsed.
    #[must_use]
    pub fn text(&self) -> Cow<'a, str> {
        let start = self.span.value_start as usize;
        let end = self.span.value_end as usize;
        let raw = self.text.get(start..end).unwrap_or_default();
        let Some(quote) = self.qualifier else {
            return Cow::Borrowed(raw);
        };
        if !self.is_qualified() || !raw.contains(quote) {
            return Cow::Borrowed(raw);
        }
        let mut out = String::with_capacity(raw.len());
        let mut chars = raw.chars();
        while let Some(ch) = chars.next() {
            out.push(ch);
            if ch == quote {
                // A qualified value holds the qualifier only in doubled form,
                // so the following character is always the second half.
                let _ = chars.next();
            }
        }
        Cow::Owned(out)
    }

    /// The cell exactly as the file holds it, including any qualifier.
    #[must_use]
    pub fn raw(&self) -> &'a str {
        let start = self.span.source_start as usize;
        let end = self.span.source_end as usize;
        self.text.get(start..end).unwrap_or_default()
    }

    /// Byte range of [`CellRef::raw`] in the source text.
    #[must_use]
    pub fn source_range(&self) -> Range<u32> {
        self.span.source_start..self.span.source_end
    }

    /// Byte range of the cell's value in the source text. The range is exact
    /// unless a doubled qualifier made the value shorter than the range.
    #[must_use]
    pub fn value_range(&self) -> Range<u32> {
        self.span.value_start..self.span.value_end
    }

    /// Whether a text qualifier surrounds the cell in the file.
    #[must_use]
    pub fn is_qualified(&self) -> bool {
        self.span.value_start > self.span.source_start
            && self.qualifier.is_some_and(|quote| {
                let at = self.span.value_start as usize - quote.len_utf8();
                self.text
                    .get(at..)
                    .is_some_and(|rest| rest.starts_with(quote))
            })
    }
}

impl Table {
    /// Number of data rows, not counting a header row.
    #[must_use]
    pub fn row_count(&self) -> usize {
        self.row_starts.len().saturating_sub(1)
    }

    /// Number of cells in the widest row.
    #[must_use]
    pub fn column_count(&self) -> usize {
        self.column_count
    }

    /// The source text the cells index into.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.text
    }

    /// Column names taken from the header row, when the file has one.
    #[must_use]
    pub fn header(&self) -> Option<&[String]> {
        self.header.as_deref()
    }

    /// What the parser had to recover from.
    #[must_use]
    pub fn warnings(&self) -> &[ParseWarning] {
        &self.warnings
    }

    /// What the first line of the file was read as: either
    /// [`FirstLineContains::ColumnNames`] or [`FirstLineContains::CellData`].
    /// A caller compares the two sides of a comparison to see whether they
    /// agree.
    #[must_use]
    pub fn first_line_contains(&self) -> &FirstLineContains {
        &self.first_line
    }

    /// Number of cells in a row, or zero when the row does not exist.
    #[must_use]
    pub fn row_len(&self, row: usize) -> usize {
        let Some((start, end)) = self.row_bounds(row) else {
            return 0;
        };
        end - start
    }

    /// One cell, or `None` when the row is short or does not exist.
    #[must_use]
    pub fn cell(&self, row: usize, column: usize) -> Option<CellRef<'_>> {
        let (start, end) = self.row_bounds(row)?;
        let index = start.checked_add(column)?;
        if index >= end {
            return None;
        }
        Some(CellRef {
            text: &self.text,
            span: *self.cells.get(index)?,
            qualifier: self.qualifier,
        })
    }

    /// A cell's value, empty when the cell is absent.
    #[must_use]
    pub fn cell_text(&self, row: usize, column: usize) -> Cow<'_, str> {
        self.cell(row, column)
            .map_or(Cow::Borrowed(""), |cell| cell.text())
    }

    /// Every cell of a row, in order.
    pub fn row(&self, row: usize) -> impl Iterator<Item = CellRef<'_>> + '_ {
        let (start, end) = self.row_bounds(row).unwrap_or((0, 0));
        self.cells
            .get(start..end)
            .unwrap_or_default()
            .iter()
            .map(|span| CellRef {
                text: &self.text,
                span: *span,
                qualifier: self.qualifier,
            })
    }

    /// Byte range the whole row occupies, from its first cell's source start
    /// to its last cell's source end. The line terminator is not included.
    #[must_use]
    pub fn row_source_range(&self, row: usize) -> Option<Range<u32>> {
        let (start, end) = self.row_bounds(row)?;
        let first = self.cells.get(start)?;
        let last = self.cells.get(end.checked_sub(1)?)?;
        Some(first.source_start..last.source_end)
    }

    fn row_bounds(&self, row: usize) -> Option<(usize, usize)> {
        let start = *self.row_starts.get(row)? as usize;
        let end = *self.row_starts.get(row.checked_add(1)?)? as usize;
        Some((start, end))
    }
}

/// Parse text into a table.
///
/// A [`FieldSyntax::Detect`] or unrecognised syntax runs detection over a
/// sample of the text first.
///
/// # Errors
///
/// Returns [`TableError::TooLarge`] when the text is past [`MAX_INPUT_BYTES`],
/// [`TableError::TooManyCells`] or [`TableError::TooManyRows`] when a table
/// exceeds its memory bounds, and [`TableError::InvalidSettings`] when a fixed
/// width column list holds a zero width.
pub fn parse(text: &str, options: &ParseOptions) -> Result<Table> {
    check_input_size(text.len())?;
    let resolved;
    let syntax = match &options.syntax {
        FieldSyntax::Detect { .. } | FieldSyntax::Unknown(_) => {
            resolved = detect_format(text, &DetectOptions::default()).syntax;
            &resolved
        }
        concrete => concrete,
    };
    let header_wanted = match options.first_line_contains {
        FirstLineContains::ColumnNames => Some(true),
        FirstLineContains::CellData => Some(false),
        FirstLineContains::Detect | FirstLineContains::Unknown(_) => None,
    };
    match syntax {
        FieldSyntax::Delimited {
            delimiters,
            text_qualifier,
            consecutive_delimiters_as_one,
            surrounding_whitespace_is_delimiter,
            ..
        } => {
            let mut parser = DelimitedParser::new(
                text,
                delimiters,
                *text_qualifier,
                *consecutive_delimiters_as_one,
                *surrounding_whitespace_is_delimiter,
                bom_len(text),
            );
            parser.run()?;
            Ok(finish(
                text,
                parser.cells,
                parser.row_starts,
                parser.warnings,
                parser.dropped,
                *text_qualifier,
                header_wanted,
            ))
        }
        FieldSyntax::Fixed {
            column_widths,
            trim,
            ..
        } => {
            if column_widths.contains(&0) {
                return Err(TableError::invalid(
                    "column widths",
                    "a fixed width column cannot be zero characters wide",
                ));
            }
            let (cells, row_starts) = parse_fixed(text, column_widths, *trim, bom_len(text))?;
            Ok(finish(
                text,
                cells,
                row_starts,
                Vec::new(),
                0,
                None,
                header_wanted,
            ))
        }
        // Detection always yields a concrete syntax.
        FieldSyntax::Detect { .. } | FieldSyntax::Unknown(_) => Err(TableError::invalid(
            "field syntax",
            "detection did not yield a concrete syntax",
        )),
    }
}

fn check_input_size(size: usize) -> Result<()> {
    if size > MAX_INPUT_BYTES {
        return Err(TableError::TooLarge {
            size,
            limit: MAX_INPUT_BYTES,
        });
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn finish(
    text: &str,
    cells: Vec<CellSpan>,
    row_starts: Vec<u32>,
    mut warnings: Vec<ParseWarning>,
    dropped: u64,
    qualifier: Option<char>,
    header_wanted: Option<bool>,
) -> Table {
    if dropped > 0 {
        warnings.push(ParseWarning::Truncated { dropped });
    }
    let mut table = Table {
        text: text.to_owned(),
        cells,
        row_starts,
        header: None,
        column_count: 0,
        qualifier,
        warnings,
        first_line: FirstLineContains::CellData,
    };
    table.column_count = (0..table.row_count())
        .map(|row| table.row_len(row))
        .max()
        .unwrap_or(0);
    let take_header = match header_wanted {
        Some(choice) => choice,
        None => looks_like_header(&table),
    };
    table.first_line = if take_header {
        FirstLineContains::ColumnNames
    } else {
        FirstLineContains::CellData
    };
    if take_header && table.row_count() > 0 {
        let names: Vec<String> = table.row(0).map(|cell| cell.text().into_owned()).collect();
        table.header = Some(names);
        table.drop_first_row();
    }
    table
}

impl Table {
    /// Removes row zero without moving any cell: the row index table simply
    /// starts one entry later, so the cell indices it holds stay valid.
    fn drop_first_row(&mut self) {
        if self.row_starts.len() > 1 {
            self.row_starts.remove(0);
        }
        self.column_count = (0..self.row_count())
            .map(|row| self.row_len(row))
            .max()
            .unwrap_or(0);
    }
}

/// Whether row zero reads as column names: every cell is non-empty, no cell
/// parses as a number, the names are distinct, and at least one later row has
/// a cell that does parse as a number.
pub(crate) fn looks_like_header(table: &Table) -> bool {
    if table.row_count() < 2 {
        return false;
    }
    let width = table.row_len(0);
    if width == 0 {
        return false;
    }
    let mut names: Vec<String> = Vec::with_capacity(width);
    for cell in table.row(0) {
        let value = cell.text();
        let trimmed = value.trim();
        if trimmed.is_empty() || looks_numeric(trimmed) {
            return false;
        }
        names.push(trimmed.to_lowercase());
    }
    names.sort_unstable();
    let distinct = names.windows(2).all(|pair| pair[0] != pair[1]);
    if !distinct {
        return false;
    }
    let sample = table.row_count().min(20);
    (1..sample).any(|row| {
        (0..table.row_len(row)).any(|column| looks_numeric(table.cell_text(row, column).trim()))
    })
}

fn looks_numeric(text: &str) -> bool {
    !text.is_empty()
        && text.chars().any(|ch| ch.is_ascii_digit())
        && text
            .chars()
            .all(|ch| ch.is_ascii_digit() || matches!(ch, '+' | '-' | '.' | ',' | ' ' | 'e' | 'E'))
}

struct DelimitedParser<'a> {
    bytes: &'a [u8],
    ascii_delimiters: [bool; 128],
    other_delimiters: Vec<char>,
    quote: Option<char>,
    fold_runs: bool,
    trim_space: bool,
    trim_tab: bool,
    cells: Vec<CellSpan>,
    row_starts: Vec<u32>,
    warnings: Vec<ParseWarning>,
    dropped: u64,
    at: usize,
    start: usize,
}

impl<'a> DelimitedParser<'a> {
    fn new(
        text: &'a str,
        delimiters: &[char],
        quote: Option<char>,
        fold_runs: bool,
        trim: bool,
        start: usize,
    ) -> Self {
        let mut ascii = [false; 128];
        let mut other = Vec::new();
        for ch in delimiters {
            if let Some(index) = ascii_index(*ch) {
                if let Some(slot) = ascii.get_mut(index) {
                    *slot = true;
                }
            } else {
                other.push(*ch);
            }
        }
        // A character that separates fields can never also be stripped as
        // padding, or a whitespace delimited file would lose every column.
        let trim_space = trim && !ascii.get(usize::from(b' ')).copied().unwrap_or(false);
        let trim_tab = trim && !ascii.get(usize::from(b'\t')).copied().unwrap_or(false);
        Self {
            bytes: text.as_bytes(),
            ascii_delimiters: ascii,
            other_delimiters: other,
            quote,
            fold_runs,
            trim_space,
            trim_tab,
            cells: Vec::new(),
            row_starts: vec![0],
            warnings: Vec::new(),
            dropped: 0,
            at: start,
            start,
        }
    }

    fn warn(&mut self, warning: ParseWarning) {
        if self.warnings.len() < MAX_WARNINGS {
            self.warnings.push(warning);
        } else {
            self.dropped = self.dropped.saturating_add(1);
        }
    }

    fn is_delimiter(&self, at: usize) -> Option<usize> {
        let (ch, width) = char_at(self.bytes, at)?;
        if let Some(index) = ascii_index(ch) {
            if self.ascii_delimiters.get(index).copied().unwrap_or(false) {
                return Some(width);
            }
            return None;
        }
        if self.other_delimiters.contains(&ch) {
            return Some(width);
        }
        None
    }

    fn is_padding(&self, at: usize) -> bool {
        match self.bytes.get(at) {
            Some(b' ') => self.trim_space,
            Some(b'\t') => self.trim_tab,
            _ => false,
        }
    }

    fn skip_padding(&mut self) {
        while self.is_padding(self.at) {
            self.at += 1;
        }
    }

    fn at_line_end(&self, at: usize) -> bool {
        matches!(self.bytes.get(at), Some(b'\n' | b'\r') | None)
    }

    fn run(&mut self) -> Result<()> {
        let len = self.bytes.len();
        while self.at < len {
            if self.fold_runs {
                // A run at the start of a line is one delimiter too, so a
                // whitespace separated line does not open with an empty cell.
                while let Some(width) = self.is_delimiter(self.at) {
                    self.at += width;
                }
            }
            loop {
                self.read_field()?;
                match self.is_delimiter(self.at) {
                    Some(width) => {
                        self.at += width;
                        if self.fold_runs {
                            loop {
                                self.skip_padding();
                                match self.is_delimiter(self.at) {
                                    Some(more) => self.at += more,
                                    None => break,
                                }
                            }
                            // A folded run that reaches the line end closes the
                            // row rather than opening a trailing empty cell.
                            if self.at_line_end(self.at) {
                                break;
                            }
                        }
                    }
                    None => break,
                }
            }
            self.close_row()?;
            self.skip_line_break();
        }
        if self.row_starts.len() == 1 && self.bytes.len() > self.start {
            // Reached only when the input is a bare line terminator.
            self.close_row()?;
        }
        Ok(())
    }

    fn close_row(&mut self) -> Result<()> {
        if self.row_starts.len().saturating_sub(1) >= MAX_TABLE_ROWS {
            return Err(TableError::TooManyRows {
                limit: MAX_TABLE_ROWS,
            });
        }
        self.row_starts.push(index_u32(self.cells.len()));
        Ok(())
    }

    fn skip_line_break(&mut self) {
        match self.bytes.get(self.at) {
            Some(b'\r') => {
                self.at += 1;
                if self.bytes.get(self.at) == Some(&b'\n') {
                    self.at += 1;
                }
            }
            Some(b'\n') => self.at += 1,
            _ => {}
        }
    }

    fn read_field(&mut self) -> Result<()> {
        let source_start = self.at;
        self.skip_padding();
        let opened_quoted = self
            .quote
            .and_then(|quote| char_at(self.bytes, self.at).map(|(ch, w)| (ch == quote, w, quote)))
            .filter(|(matched, _, _)| *matched);

        let (value_start, value_end) = if let Some((_, width, quote)) = opened_quoted {
            self.at += width;
            let start = self.at;
            let end = self.scan_qualified(quote, source_start);
            (start, end)
        } else {
            let start = self.at;
            self.scan_plain();
            let mut end = self.at;
            while end > start && self.is_padding(end - 1) {
                end -= 1;
            }
            (start, end)
        };
        let source_end = self.at;
        let count = self.cells.len().saturating_add(1);
        if count > MAX_TABLE_CELLS {
            return Err(TableError::TooManyCells {
                count,
                limit: MAX_TABLE_CELLS,
            });
        }
        self.cells.push(CellSpan {
            source_start: index_u32(source_start),
            source_end: index_u32(source_end),
            value_start: index_u32(value_start),
            value_end: index_u32(value_end),
        });
        Ok(())
    }

    /// Advances to the delimiter or line break that ends an unqualified field.
    fn scan_plain(&mut self) {
        while self.at < self.bytes.len() {
            let byte = self.bytes.get(self.at).copied().unwrap_or(0);
            if byte == b'\n' || byte == b'\r' {
                return;
            }
            if byte < 0x80 {
                if self
                    .ascii_delimiters
                    .get(usize::from(byte))
                    .copied()
                    .unwrap_or(false)
                {
                    return;
                }
                self.at += 1;
                continue;
            }
            let Some((ch, width)) = char_at(self.bytes, self.at) else {
                self.at += 1;
                continue;
            };
            if self.other_delimiters.contains(&ch) {
                return;
            }
            self.at += width;
        }
    }

    /// Advances past a qualified field. Returns the end of its value; the
    /// cursor lands on the delimiter or line break that ends the field.
    fn scan_qualified(&mut self, quote: char, source_start: usize) -> usize {
        let quote_width = quote.len_utf8();
        let value_end;
        loop {
            let Some(position) = self.find_quote(quote) else {
                self.warn(ParseWarning::UnterminatedQualifier {
                    offset: index_u32(source_start),
                });
                self.at = self.bytes.len();
                return self.bytes.len();
            };
            let after = position + quote_width;
            if starts_with_char(self.bytes, after, quote) {
                self.at = after + quote_width;
                continue;
            }
            value_end = position;
            self.at = after;
            break;
        }
        // Padding after the closing qualifier belongs to the delimiter.
        self.skip_padding();
        if !self.at_line_end(self.at) && self.is_delimiter(self.at).is_none() {
            self.warn(ParseWarning::TextAfterQualifier {
                offset: index_u32(self.at),
            });
            self.scan_plain();
        }
        value_end
    }

    fn find_quote(&self, quote: char) -> Option<usize> {
        if let Some(byte) = ascii_byte(quote) {
            let rest = self.bytes.get(self.at..)?;
            return rest.iter().position(|b| *b == byte).map(|at| self.at + at);
        }
        let mut at = self.at;
        while at < self.bytes.len() {
            let (ch, width) = char_at(self.bytes, at)?;
            if ch == quote {
                return Some(at);
            }
            at += width;
        }
        None
    }
}

fn parse_fixed(
    text: &str,
    widths: &[u32],
    trim: bool,
    start: usize,
) -> Result<(Vec<CellSpan>, Vec<u32>)> {
    let bytes = text.as_bytes();
    let mut cells = Vec::new();
    let mut row_starts = vec![0u32];
    let mut at = start;
    while at < bytes.len() {
        let line_start = at;
        while at < bytes.len() && !matches!(bytes.get(at), Some(b'\n' | b'\r')) {
            at += 1;
        }
        let line_end = at;
        push_fixed_row(text, line_start, line_end, widths, trim, &mut cells)?;
        if row_starts.len().saturating_sub(1) >= MAX_TABLE_ROWS {
            return Err(TableError::TooManyRows {
                limit: MAX_TABLE_ROWS,
            });
        }
        row_starts.push(index_u32(cells.len()));
        match bytes.get(at) {
            Some(b'\r') => {
                at += 1;
                if bytes.get(at) == Some(&b'\n') {
                    at += 1;
                }
            }
            Some(b'\n') => at += 1,
            _ => {}
        }
    }
    Ok((cells, row_starts))
}

fn push_fixed_row(
    text: &str,
    line_start: usize,
    line_end: usize,
    widths: &[u32],
    trim: bool,
    cells: &mut Vec<CellSpan>,
) -> Result<()> {
    let bytes = text.as_bytes();
    let mut at = line_start;
    for width in widths {
        let field_start = at;
        let mut taken = 0u32;
        while taken < *width && at < line_end {
            let step = char_at(bytes, at).map_or(1, |(_, w)| w);
            at += step;
            taken += 1;
        }
        push_cell(cells, span(text, field_start, at, trim))?;
    }
    if at < line_end {
        push_cell(cells, span(text, at, line_end, trim))?;
    }
    Ok(())
}

fn push_cell(cells: &mut Vec<CellSpan>, cell: CellSpan) -> Result<()> {
    let count = cells.len().saturating_add(1);
    if count > MAX_TABLE_CELLS {
        return Err(TableError::TooManyCells {
            count,
            limit: MAX_TABLE_CELLS,
        });
    }
    cells.push(cell);
    Ok(())
}

fn span(text: &str, start: usize, end: usize, trim: bool) -> CellSpan {
    let (mut value_start, mut value_end) = (start, end);
    if trim {
        let bytes = text.as_bytes();
        while value_start < value_end && matches!(bytes.get(value_start), Some(b' ' | b'\t')) {
            value_start += 1;
        }
        while value_end > value_start && matches!(bytes.get(value_end - 1), Some(b' ' | b'\t')) {
            value_end -= 1;
        }
    }
    CellSpan {
        source_start: index_u32(start),
        source_end: index_u32(end),
        value_start: index_u32(value_start),
        value_end: index_u32(value_end),
    }
}

#[inline]
fn char_at(bytes: &[u8], at: usize) -> Option<(char, usize)> {
    let first = *bytes.get(at)?;
    if first < 0x80 {
        return Some((first as char, 1));
    }
    let width = utf8_width(first);
    let slice = bytes.get(at..at + width)?;
    let text = std::str::from_utf8(slice).ok()?;
    let ch = text.chars().next()?;
    Some((ch, ch.len_utf8()))
}

#[inline]
fn utf8_width(first: u8) -> usize {
    match first {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

#[inline]
fn starts_with_char(bytes: &[u8], at: usize, ch: char) -> bool {
    char_at(bytes, at).is_some_and(|(found, _)| found == ch)
}

#[inline]
fn ascii_index(ch: char) -> Option<usize> {
    ch.is_ascii().then_some(ch as usize)
}

#[inline]
fn ascii_byte(ch: char) -> Option<u8> {
    ch.is_ascii()
        .then_some(ch as u32)
        .and_then(|v| u8::try_from(v).ok())
}

/// Offsets are validated against [`MAX_INPUT_BYTES`] before parsing starts, so
/// the saturating arm is unreachable for any input the engine accepts.
#[inline]
fn index_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn rows(table: &Table) -> Vec<Vec<String>> {
        (0..table.row_count())
            .map(|row| {
                table
                    .row(row)
                    .map(|cell| cell.text().into_owned())
                    .collect()
            })
            .collect()
    }

    fn csv(text: &str) -> Table {
        let mut options = ParseOptions::comma_separated();
        options.first_line_contains = FirstLineContains::CellData;
        parse(text, &options).unwrap()
    }

    #[test]
    fn parser_edge_cases() {
        let cases: &[(&str, &[&[&str]])] = &[
            ("a,b,c", &[&["a", "b", "c"]]),
            ("a,b\nc,d\n", &[&["a", "b"], &["c", "d"]]),
            ("a,b\r\nc,d\r\n", &[&["a", "b"], &["c", "d"]]),
            ("a,b\rc,d", &[&["a", "b"], &["c", "d"]]),
            ("a,,c", &[&["a", "", "c"]]),
            (",", &[&["", ""]]),
            ("a\n\nb", &[&["a"], &[""], &["b"]]),
            ("\"a,b\",c", &[&["a,b", "c"]]),
            ("\"say \"\"hi\"\"\",c", &[&["say \"hi\"", "c"]]),
            ("\"line\none\",c", &[&["line\none", "c"]]),
            ("  a  ,  b  ", &[&["a", "b"]]),
            ("\"  a  \",b", &[&["  a  ", "b"]]),
            ("a,b,c\nd,e", &[&["a", "b", "c"], &["d", "e"]]),
            ("\"unterminated,b", &[&["unterminated,b"]]),
            ("\"a\"x,b", &[&["a", "b"]]),
            ("a,b\n", &[&["a", "b"]]),
        ];
        for (input, expected) in cases {
            let table = csv(input);
            let got = rows(&table);
            let want: Vec<Vec<String>> = expected
                .iter()
                .map(|row| row.iter().map(|cell| (*cell).to_owned()).collect())
                .collect();
            assert_eq!(got, want, "input {input:?}");
        }
    }

    #[test]
    fn input_and_table_memory_limits_are_enforced() {
        const INPUT_LIMIT: usize = 64 * 1024 * 1024;
        const CELL_LIMIT: usize = MAX_TABLE_CELLS;
        const ROW_LIMIT: usize = MAX_TABLE_ROWS;
        let too_long = "x".repeat(INPUT_LIMIT + 1);
        let mut one_column = ParseOptions::comma_separated();
        one_column.first_line_contains = FirstLineContains::CellData;
        assert!(parse(&too_long, &one_column).is_err());

        let dense = ",".repeat(CELL_LIMIT);
        let mut delimited = ParseOptions::comma_separated();
        delimited.first_line_contains = FirstLineContains::CellData;
        assert!(parse(&dense, &delimited).is_err());

        let fixed = ParseOptions {
            syntax: FieldSyntax::Fixed {
                column_widths: vec![1; CELL_LIMIT + 1],
                trim: true,
                unknown: Unknown::new(),
            },
            first_line_contains: FirstLineContains::CellData,
            unknown: Unknown::new(),
        };
        assert!(parse("x", &fixed).is_err());

        let blank_rows = "\n".repeat(ROW_LIMIT + 1);
        let empty_widths = ParseOptions {
            syntax: FieldSyntax::Fixed {
                column_widths: Vec::new(),
                trim: true,
                unknown: Unknown::new(),
            },
            first_line_contains: FirstLineContains::CellData,
            unknown: Unknown::new(),
        };
        assert!(parse(&blank_rows, &empty_widths).is_err());
    }

    #[test]
    fn a_byte_order_mark_stays_out_of_the_first_cell() {
        let text = "\u{feff}id,name\n1,Ann\n";
        let table = csv(text);
        assert_eq!(table.cell_text(0, 0), "id");
        let cell = table.cell(0, 0).unwrap();
        let range = cell.source_range();
        assert_eq!(
            text.get(range.start as usize..range.end as usize),
            Some("id")
        );
        assert_eq!(range.start, 3);

        let mut options = ParseOptions::comma_separated();
        options.first_line_contains = FirstLineContains::ColumnNames;
        let headed = parse(text, &options).unwrap();
        assert_eq!(
            headed.header(),
            Some(["id".to_owned(), "name".to_owned()].as_slice())
        );

        let fixed = parse("\u{feff}ab  cd\n", &ParseOptions::fixed_width([4])).unwrap();
        assert_eq!(fixed.cell_text(0, 0), "ab");

        let empty = csv("\u{feff}");
        assert_eq!(empty.row_count(), 0);
    }

    #[test]
    fn the_first_line_kind_is_reported() {
        let mut options = ParseOptions::comma_separated();
        options.first_line_contains = FirstLineContains::Detect;
        let headed = parse("id,name\n1,Ann\n2,Bob\n", &options).unwrap();
        assert_eq!(
            headed.first_line_contains(),
            &FirstLineContains::ColumnNames
        );
        let plain = parse("1,2\n3,4\n5,6\n", &options).unwrap();
        assert_eq!(plain.first_line_contains(), &FirstLineContains::CellData);
    }

    #[test]
    fn ragged_rows_keep_their_own_width() {
        let table = csv("a,b,c\nd\ne,f");
        assert_eq!(table.column_count(), 3);
        assert_eq!(table.row_len(1), 1);
        assert!(table.cell(1, 1).is_none());
        assert_eq!(table.cell_text(1, 1), "");
    }

    #[test]
    fn byte_ranges_round_trip_to_the_source() {
        let text = "id,name\n1,\"Ann, A\"\n2,Bob\n";
        let table = csv(text);
        for row in 0..table.row_count() {
            for cell in table.row(row) {
                let range = cell.source_range();
                let slice = &text[range.start as usize..range.end as usize];
                assert_eq!(slice, cell.raw());
                let value = cell.value_range();
                assert!(value.start >= range.start && value.end <= range.end);
            }
        }
        let quoted = table.cell(1, 1).unwrap();
        assert_eq!(quoted.raw(), "\"Ann, A\"");
        assert_eq!(quoted.text(), "Ann, A");
        assert!(quoted.is_qualified());
    }

    #[test]
    fn a_doubled_qualifier_shortens_the_value_but_not_the_range() {
        let table = csv("\"a\"\"b\"");
        let cell = table.cell(0, 0).unwrap();
        assert_eq!(cell.text(), "a\"b");
        assert_eq!(cell.raw(), "\"a\"\"b\"");
        assert_eq!(cell.value_range(), 1..5);
    }

    #[test]
    fn whitespace_can_stay_in_the_field() {
        let options = ParseOptions {
            syntax: FieldSyntax::Delimited {
                delimiters: vec![','],
                text_qualifier: Some('"'),
                consecutive_delimiters_as_one: false,
                surrounding_whitespace_is_delimiter: false,
                unknown: Unknown::new(),
            },
            first_line_contains: FirstLineContains::CellData,
            unknown: Unknown::new(),
        };
        let table = parse("  a  ,  b  ", &options).unwrap();
        assert_eq!(
            rows(&table),
            vec![vec!["  a  ".to_owned(), "  b  ".to_owned()]]
        );
    }

    #[test]
    fn consecutive_delimiters_fold_into_one() {
        let options = ParseOptions {
            syntax: FieldSyntax::Delimited {
                delimiters: vec![' '],
                text_qualifier: None,
                consecutive_delimiters_as_one: true,
                surrounding_whitespace_is_delimiter: true,
                unknown: Unknown::new(),
            },
            first_line_contains: FirstLineContains::CellData,
            unknown: Unknown::new(),
        };
        let table = parse("  alpha   beta  gamma  \nx y", &options).unwrap();
        assert_eq!(
            rows(&table),
            vec![
                vec!["alpha".to_owned(), "beta".to_owned(), "gamma".to_owned()],
                vec!["x".to_owned(), "y".to_owned()],
            ]
        );
    }

    #[test]
    fn a_semicolon_delimiter_and_a_single_quote_qualifier_work() {
        let options = ParseOptions {
            syntax: FieldSyntax::Delimited {
                delimiters: vec![';'],
                text_qualifier: Some('\''),
                consecutive_delimiters_as_one: false,
                surrounding_whitespace_is_delimiter: true,
                unknown: Unknown::new(),
            },
            first_line_contains: FirstLineContains::CellData,
            unknown: Unknown::new(),
        };
        let table = parse("'a;b';c", &options).unwrap();
        assert_eq!(rows(&table), vec![vec!["a;b".to_owned(), "c".to_owned()]]);
    }

    #[test]
    fn a_non_ascii_delimiter_works() {
        let options = ParseOptions {
            syntax: FieldSyntax::Delimited {
                delimiters: vec!['\u{2502}'],
                text_qualifier: None,
                consecutive_delimiters_as_one: false,
                surrounding_whitespace_is_delimiter: true,
                unknown: Unknown::new(),
            },
            first_line_contains: FirstLineContains::CellData,
            unknown: Unknown::new(),
        };
        let table = parse("a\u{2502}\u{e9}b\u{2502}c", &options).unwrap();
        assert_eq!(
            rows(&table),
            vec![vec!["a".to_owned(), "\u{e9}b".to_owned(), "c".to_owned()]]
        );
    }

    #[test]
    fn fixed_width_columns_split_by_character_count() {
        let options = ParseOptions::fixed_width([4, 4]);
        let table = parse("abc defg\nxy  z\n", &options).unwrap();
        assert_eq!(
            rows(&table),
            vec![
                vec!["abc".to_owned(), "defg".to_owned()],
                vec!["xy".to_owned(), "z".to_owned()],
            ]
        );
    }

    #[test]
    fn fixed_width_keeps_the_remainder_as_a_further_cell() {
        let options = ParseOptions::fixed_width([2]);
        let table = parse("abcdef", &options).unwrap();
        assert_eq!(rows(&table), vec![vec!["ab".to_owned(), "cdef".to_owned()]]);
    }

    #[test]
    fn a_zero_fixed_width_is_rejected() {
        let options = ParseOptions::fixed_width([3, 0]);
        assert!(matches!(
            parse("abc", &options),
            Err(TableError::InvalidSettings { .. })
        ));
    }

    #[test]
    fn a_header_row_is_taken_when_asked_for() {
        let mut options = ParseOptions::comma_separated();
        options.first_line_contains = FirstLineContains::ColumnNames;
        let table = parse("id,name\n1,Ann\n", &options).unwrap();
        assert_eq!(
            table.header().map(<[String]>::to_vec),
            Some(vec!["id".to_owned(), "name".to_owned()])
        );
        assert_eq!(table.row_count(), 1);
        assert_eq!(table.cell_text(0, 0), "1");
    }

    #[test]
    fn a_header_row_is_detected_from_the_data() {
        let table = parse("id,name\n1,Ann\n2,Bob\n", &ParseOptions::comma_separated()).unwrap();
        assert!(table.header().is_some());
        assert_eq!(table.row_count(), 2);
    }

    #[test]
    fn numeric_first_rows_are_not_headers() {
        let table = parse("1,2\n3,4\n5,6\n", &ParseOptions::comma_separated()).unwrap();
        assert!(table.header().is_none());
        assert_eq!(table.row_count(), 3);
    }

    #[test]
    fn an_unterminated_qualifier_is_reported() {
        let table = csv("a,\"b\nc");
        assert!(matches!(
            table.warnings().first(),
            Some(ParseWarning::UnterminatedQualifier { .. })
        ));
        assert_eq!(table.row_count(), 1);
    }

    #[test]
    fn warnings_are_capped() {
        let mut text = String::new();
        for _ in 0..(MAX_WARNINGS * 4) {
            text.push_str("\"a\"x,b\n");
        }
        let table = csv(&text);
        assert_eq!(table.warnings().len(), MAX_WARNINGS + 1);
        assert!(matches!(
            table.warnings().last(),
            Some(ParseWarning::Truncated { .. })
        ));
    }

    #[test]
    #[cfg_attr(
        debug_assertions,
        ignore = "wall-clock budget holds for release builds"
    )]
    fn a_huge_single_field_stays_linear() {
        let mut text = String::with_capacity(20_000_100);
        text.push('"');
        for _ in 0..10_000_000 {
            text.push('x');
        }
        text.push_str("\",b");
        let start = std::time::Instant::now();
        let table = csv(&text);
        assert_eq!(table.row_len(0), 2);
        assert_eq!(table.cell_text(0, 0).len(), 10_000_000);
        assert!(start.elapsed().as_secs() < 20);
    }

    /// Correctness half of the huge field case, at a size a debug build parses
    /// quickly: a quoted field of any length is one cell.
    #[test]
    fn a_long_single_field_is_one_cell() {
        let mut text = String::with_capacity(100_100);
        text.push('"');
        for _ in 0..100_000 {
            text.push('x');
        }
        text.push_str("\",b");
        let table = csv(&text);
        assert_eq!(table.row_len(0), 2);
        assert_eq!(table.cell_text(0, 0).len(), 100_000);
    }

    #[test]
    fn a_row_of_a_million_columns_parses() {
        let mut text = String::with_capacity(2_000_000);
        for index in 0..1_000_000 {
            if index > 0 {
                text.push(',');
            }
            text.push('z');
        }
        let table = csv(&text);
        assert_eq!(table.row_count(), 1);
        assert_eq!(table.row_len(0), 1_000_000);
        assert_eq!(table.cell_text(0, 999_999), "z");
    }

    #[test]
    fn unbalanced_quotes_everywhere_terminate() {
        let text = "\"".repeat(200_000);
        let table = csv(&text);
        assert!(table.row_count() >= 1);
    }

    #[test]
    fn options_round_trip_with_unknown_fields() {
        let text = r#"{"syntax":{"kind":"delimited","delimiters":[";"],"futureFlag":7},"firstLineContains":"column-names","futureTop":1}"#;
        let options: ParseOptions = serde_json::from_str(text).unwrap();
        assert!(options.unknown.contains_key("futureTop"));
        let back = serde_json::to_string(&options).unwrap();
        assert!(back.contains("futureFlag"));
        assert!(back.contains("futureTop"));
        let again: ParseOptions = serde_json::from_str(&back).unwrap();
        assert_eq!(again, options);
    }

    #[test]
    fn an_unknown_syntax_falls_back_to_detection() {
        let syntax: FieldSyntax = serde_json::from_str(r#"{"kind":"columnar"}"#).unwrap();
        assert!(matches!(syntax, FieldSyntax::Unknown(_)));
        let options = ParseOptions {
            syntax,
            first_line_contains: FirstLineContains::CellData,
            unknown: Unknown::new(),
        };
        let table = parse("a,b\nc,d\n", &options).unwrap();
        assert_eq!(table.row_len(0), 2);
    }
}
