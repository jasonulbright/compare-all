//! Guessing the shape of a file from a sample of it.
//!
//! Detection answers four questions: are the fields separated by a delimiter
//! or defined by position, which delimiter and text qualifier are in use,
//! does the first line name the columns, and what type does each column hold.
//!
//! The sample is bounded, so detection costs the same on a small file and on
//! a large one.

use crate::decimal::Decimal;
use crate::parse::{
    looks_like_header, parse, without_bom, FieldSyntax, FirstLineContains, ParseOptions, Table,
};
use crate::regional::Regional;
use crate::schema::ColumnType;
use crate::value::{has_leading_zero_integer, parse_date, parse_number};
use crate::Unknown;
use serde::{Deserialize, Serialize};

/// Delimiters tried in order. The first that explains the sample as well as
/// any other wins, so a file that works as both stays with the commoner one.
const CANDIDATE_DELIMITERS: [char; 5] = ['\t', ',', ';', '|', ':'];

/// How much of the file detection reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DetectOptions {
    /// Largest number of lines read from the start of the file.
    pub sample_lines: usize,
    /// Largest number of bytes read from the start of the file.
    pub sample_bytes: usize,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, skip_serializing_if = "Unknown::is_empty")]
    pub unknown: Unknown,
}

impl Default for DetectOptions {
    fn default() -> Self {
        Self {
            sample_lines: 100,
            sample_bytes: 256 * 1024,
            unknown: Unknown::new(),
        }
    }
}

/// What detection concluded about a file.
#[derive(Debug, Clone, PartialEq)]
pub struct DetectedFormat {
    /// How the fields are separated.
    pub syntax: FieldSyntax,
    /// Whether the first line names the columns.
    pub first_line_contains: FirstLineContains,
    /// Type of each column, empty when only the syntax was asked for.
    pub column_types: Vec<ColumnType>,
    /// Number of columns the sample settled on.
    pub column_count: usize,
}

/// Guess the field syntax and header presence of a file.
///
/// Column types are not filled in; use [`detect`] for those.
#[must_use]
pub fn detect_format(text: &str, options: &DetectOptions) -> DetectedFormat {
    let sample = take_sample(text, options);
    let (syntax, column_count) = detect_syntax(sample);
    let first_line_contains = detect_header(sample, &syntax);
    DetectedFormat {
        syntax,
        first_line_contains,
        column_types: Vec::new(),
        column_count,
    }
}

/// Guess the field syntax, header presence and column types of a file.
#[must_use]
pub fn detect(text: &str, options: &DetectOptions, regional: &Regional) -> DetectedFormat {
    let mut format = detect_format(text, options);
    let parse_options = ParseOptions {
        syntax: format.syntax.clone(),
        first_line_contains: format.first_line_contains.clone(),
        unknown: Unknown::new(),
    };
    let sample = take_sample(text, options);
    if let Ok(table) = parse(sample, &parse_options) {
        format.column_count = table.column_count();
        format.column_types = (0..table.column_count())
            .map(|column| {
                column_type_of_sample(&table, index_u32(column), options.sample_lines, regional)
                    .column_type
            })
            .collect();
    }
    format
}

/// What a sample of one column says about its type.
#[derive(Debug, Clone, PartialEq)]
pub struct SampledColumnType {
    /// The type the sample settled on.
    pub column_type: ColumnType,
    /// Non-empty cells inspected.
    pub sampled: u64,
    /// Inspected cells that did not read as the leading candidate type. Any
    /// such cell demotes the column to [`ColumnType::Text`].
    pub failed: u64,
}

impl Default for SampledColumnType {
    fn default() -> Self {
        Self {
            column_type: ColumnType::Text,
            sampled: 0,
            failed: 0,
        }
    }
}

/// Largest number of cells one column's type detection reads.
pub const TYPE_SAMPLE_BUDGET: usize = 2_000;

/// The type a column's cells read as.
///
/// The sample is spread across the column with a stride and includes the last
/// row. Unsampled rows may still be missed. Every non-empty sampled cell must
/// read as the candidate type; one cell that does not demotes the column to
/// characters, which never calls two differently written values equal.
///
/// A column whose cells carry a leading zero before another digit, or more
/// significant digits than a double holds, is not a number column: such cells
/// are identifiers, and reading them as numbers would merge distinct values.
#[must_use]
pub fn column_type_of_sample(
    table: &Table,
    column: u32,
    budget: usize,
    regional: &Regional,
) -> SampledColumnType {
    let rows = table.row_count();
    let budget = budget.max(1);
    let stride = rows.div_ceil(budget).max(1);
    let mut seen = 0u64;
    let mut numbers = 0u64;
    let mut dates = 0u64;
    let mut inspect = |row: usize| {
        let Some(cell) = table.cell(row, column as usize) else {
            return;
        };
        let value = cell.text();
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return;
        }
        seen += 1;
        if detectable_number(trimmed, regional).is_some() {
            numbers += 1;
        } else if parse_date(trimmed, regional).is_some() {
            dates += 1;
        }
    };
    let mut row = 0usize;
    while row < rows {
        inspect(row);
        row += stride;
    }
    // The stride can step over the last row, where a column often changes.
    if let Some(last) = rows.checked_sub(1) {
        if last % stride != 0 {
            inspect(last);
        }
    }
    if seen == 0 {
        return SampledColumnType {
            column_type: ColumnType::Text,
            sampled: 0,
            failed: 0,
        };
    }
    let (candidate, matched) = if numbers >= dates && numbers > 0 {
        (ColumnType::Number, numbers)
    } else if dates > 0 {
        (ColumnType::DateTime, dates)
    } else {
        (ColumnType::Text, seen)
    };
    let failed = seen.saturating_sub(matched);
    SampledColumnType {
        column_type: if failed == 0 {
            candidate
        } else {
            ColumnType::Text
        },
        sampled: seen,
        failed,
    }
}

/// Largest number of significant digits a detected number column may carry.
/// Past this a double cannot hold the value, so the cells are identifiers.
const MAX_DETECTED_DIGITS: usize = 15;

/// Reads a cell as a number only when its written form is one a number column
/// may hold.
fn detectable_number(text: &str, regional: &Regional) -> Option<Decimal> {
    let value = parse_number(text, regional)?;
    if value.significant_digits() > MAX_DETECTED_DIGITS {
        return None;
    }
    if has_leading_zero_integer(text) {
        return None;
    }
    Some(value)
}

fn take_sample<'a>(text: &'a str, options: &DetectOptions) -> &'a str {
    let text = without_bom(text);
    let mut end = text.len().min(options.sample_bytes);
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let head = text.get(..end).unwrap_or(text);
    let mut lines = 0usize;
    for (offset, byte) in head.as_bytes().iter().enumerate() {
        if *byte == b'\n' {
            lines += 1;
            if lines >= options.sample_lines {
                return head.get(..=offset).unwrap_or(head);
            }
        }
    }
    head
}

fn detect_syntax(sample: &str) -> (FieldSyntax, usize) {
    let mut best: Option<(f64, usize, FieldSyntax)> = None;
    for delimiter in CANDIDATE_DELIMITERS {
        let qualifier = detect_qualifier(sample, delimiter);
        let syntax = FieldSyntax::Delimited {
            delimiters: vec![delimiter],
            text_qualifier: Some(qualifier),
            consecutive_delimiters_as_one: false,
            surrounding_whitespace_is_delimiter: true,
            unknown: Unknown::new(),
        };
        let Some((score, columns)) = score_syntax(sample, &syntax) else {
            continue;
        };
        if best.as_ref().is_none_or(|(top, _, _)| score > *top) {
            best = Some((score, columns, syntax));
        }
    }
    if let Some((score, columns, syntax)) = best {
        if score > 0.0 {
            return (syntax, columns);
        }
    }
    if let Some((widths, columns)) = detect_fixed_widths(sample) {
        return (
            FieldSyntax::Fixed {
                column_widths: widths,
                trim: true,
                unknown: Unknown::new(),
            },
            columns,
        );
    }
    let whitespace = FieldSyntax::Delimited {
        delimiters: vec![' '],
        text_qualifier: Some('"'),
        consecutive_delimiters_as_one: true,
        surrounding_whitespace_is_delimiter: true,
        unknown: Unknown::new(),
    };
    if let Some((score, columns)) = score_syntax(sample, &whitespace) {
        if score > 0.0 {
            return (whitespace, columns);
        }
    }
    (FieldSyntax::delimited([',']), 1)
}

/// Scores a candidate by how consistently it splits the sample: the share of
/// rows with the commonest width, scaled by that width. A candidate that
/// yields one column everywhere explains nothing and scores zero.
fn score_syntax(sample: &str, syntax: &FieldSyntax) -> Option<(f64, usize)> {
    let options = ParseOptions {
        syntax: syntax.clone(),
        first_line_contains: FirstLineContains::CellData,
        unknown: Unknown::new(),
    };
    let table = parse(sample, &options).ok()?;
    if table.row_count() == 0 {
        return None;
    }
    let mut widths: Vec<usize> = (0..table.row_count())
        .map(|row| table.row_len(row))
        .filter(|width| *width > 0)
        .collect();
    if widths.is_empty() {
        return None;
    }
    let total = widths.len();
    widths.sort_unstable();
    let mut modal = widths.first().copied().unwrap_or(0);
    let mut modal_count = 0usize;
    let mut run_value = modal;
    let mut run = 0usize;
    for width in &widths {
        if *width == run_value {
            run += 1;
        } else {
            run_value = *width;
            run = 1;
        }
        if run > modal_count {
            modal_count = run;
            modal = run_value;
        }
    }
    if modal < 2 {
        return Some((0.0, modal));
    }
    let consistency = ratio(modal_count, total);
    let width_weight = ratio(modal.min(32), 32);
    Some((consistency * consistency * (0.25 + width_weight), modal))
}

#[allow(clippy::cast_precision_loss)]
fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        return 0.0;
    }
    (numerator.min(denominator) as f64) / (denominator as f64)
}

/// Picks a text qualifier by parsing the sample with each candidate and
/// keeping the one the data actually uses.
fn detect_qualifier(sample: &str, delimiter: char) -> char {
    for candidate in ['"', '\''] {
        let options = ParseOptions {
            syntax: FieldSyntax::Delimited {
                delimiters: vec![delimiter],
                text_qualifier: Some(candidate),
                consecutive_delimiters_as_one: false,
                surrounding_whitespace_is_delimiter: true,
                unknown: Unknown::new(),
            },
            first_line_contains: FirstLineContains::CellData,
            unknown: Unknown::new(),
        };
        let Ok(table) = parse(sample, &options) else {
            continue;
        };
        if table.warnings().is_empty()
            && (0..table.row_count()).any(|row| table.row(row).any(|cell| cell.is_qualified()))
        {
            return candidate;
        }
    }
    '"'
}

/// Finds fixed column boundaries: character positions where every sampled line
/// holds a blank, followed by a position where at least one line does not.
fn detect_fixed_widths(sample: &str) -> Option<(Vec<u32>, usize)> {
    let lines: Vec<&str> = sample
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(64)
        .collect();
    if lines.len() < 3 {
        return None;
    }
    let chars: Vec<Vec<char>> = lines.iter().map(|line| line.chars().collect()).collect();
    let width = chars.iter().map(Vec::len).max().unwrap_or(0);
    if width < 4 {
        return None;
    }
    let blank_at: Vec<bool> = (0..width)
        .map(|position| {
            chars
                .iter()
                .all(|line| line.get(position).is_none_or(|ch| *ch == ' '))
        })
        .collect();
    if blank_at.first().copied().unwrap_or(false) {
        return None;
    }
    let mut boundaries: Vec<usize> = Vec::new();
    for position in 1..width {
        let previous = blank_at.get(position - 1).copied().unwrap_or(false);
        let current = blank_at.get(position).copied().unwrap_or(false);
        if previous && !current {
            boundaries.push(position);
        }
    }
    if boundaries.is_empty() {
        return None;
    }
    let mut widths = Vec::with_capacity(boundaries.len());
    let mut previous = 0usize;
    for boundary in &boundaries {
        widths.push(index_u32(boundary - previous));
        previous = *boundary;
    }
    let columns = widths.len() + 1;
    Some((widths, columns))
}

fn detect_header(sample: &str, syntax: &FieldSyntax) -> FirstLineContains {
    let options = ParseOptions {
        syntax: syntax.clone(),
        first_line_contains: FirstLineContains::CellData,
        unknown: Unknown::new(),
    };
    match parse(sample, &options) {
        Ok(table) if looks_like_header(&table) => FirstLineContains::ColumnNames,
        _ => FirstLineContains::CellData,
    }
}

#[inline]
fn index_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use std::fmt::Write as _;

    fn delimiter_of(syntax: &FieldSyntax) -> Option<char> {
        match syntax {
            FieldSyntax::Delimited { delimiters, .. } => delimiters.first().copied(),
            _ => None,
        }
    }

    #[test]
    fn a_comma_file_is_detected() {
        let format = detect_format("a,b,c\n1,2,3\n4,5,6\n", &DetectOptions::default());
        assert_eq!(delimiter_of(&format.syntax), Some(','));
        assert_eq!(format.column_count, 3);
    }

    #[test]
    fn a_tab_file_is_detected() {
        let format = detect_format("a\tb\tc\n1\t2\t3\n4\t5\t6\n", &DetectOptions::default());
        assert_eq!(delimiter_of(&format.syntax), Some('\t'));
    }

    #[test]
    fn a_semicolon_file_is_detected() {
        let format = detect_format("a;b;c\n1;2;3\n4;5;6\n", &DetectOptions::default());
        assert_eq!(delimiter_of(&format.syntax), Some(';'));
    }

    #[test]
    fn a_pipe_file_is_detected() {
        let format = detect_format("a|b|c\n1|2|3\n4|5|6\n", &DetectOptions::default());
        assert_eq!(delimiter_of(&format.syntax), Some('|'));
    }

    #[test]
    fn a_single_quote_qualifier_is_detected() {
        let text = "'a,x',b\n'c,y',d\n'e,z',f\n";
        let format = detect_format(text, &DetectOptions::default());
        match &format.syntax {
            FieldSyntax::Delimited { text_qualifier, .. } => {
                assert_eq!(*text_qualifier, Some('\''));
            }
            other => panic!("expected a delimited syntax, got {other:?}"),
        }
    }

    #[test]
    fn a_header_is_detected() {
        let format = detect_format("id,name\n1,Ann\n2,Bob\n", &DetectOptions::default());
        assert_eq!(format.first_line_contains, FirstLineContains::ColumnNames);
    }

    #[test]
    fn a_data_first_line_is_detected() {
        let format = detect_format("1,2\n3,4\n5,6\n", &DetectOptions::default());
        assert_eq!(format.first_line_contains, FirstLineContains::CellData);
    }

    #[test]
    fn fixed_width_columns_are_detected() {
        let text = "Ann   Rome    30\nBob   Oslo    41\nCyd   Lima    22\nDee   Bonn    35\n";
        let format = detect_format(text, &DetectOptions::default());
        match &format.syntax {
            FieldSyntax::Fixed { column_widths, .. } => {
                assert_eq!(column_widths, &vec![6, 8]);
            }
            other => panic!("expected a fixed syntax, got {other:?}"),
        }
    }

    #[test]
    fn column_types_come_back_with_the_format() {
        let text = "id,name,when\n1,Ann,01/02/2020\n2,Bob,03/04/2020\n";
        let format = detect(text, &DetectOptions::default(), &Regional::dot_decimal());
        assert_eq!(
            format.column_types,
            vec![ColumnType::Number, ColumnType::Text, ColumnType::DateTime]
        );
    }

    #[test]
    fn an_empty_file_gets_a_usable_answer() {
        let format = detect_format("", &DetectOptions::default());
        assert_eq!(delimiter_of(&format.syntax), Some(','));
        assert_eq!(format.first_line_contains, FirstLineContains::CellData);
    }

    #[test]
    fn a_single_column_file_stays_one_column() {
        let format = detect_format("alpha\nbeta\ngamma\n", &DetectOptions::default());
        assert_eq!(format.column_count, 1);
    }

    #[test]
    #[cfg_attr(
        debug_assertions,
        ignore = "wall-clock budget holds for release builds"
    )]
    fn detection_reads_only_the_sample() {
        let mut text = String::from("a,b,c\n");
        for index in 0..200_000 {
            let _ = writeln!(text, "{index},{index},{index}");
        }
        let start = std::time::Instant::now();
        let format = detect_format(&text, &DetectOptions::default());
        assert_eq!(delimiter_of(&format.syntax), Some(','));
        assert!(start.elapsed().as_millis() < 2_000);
    }

    /// Correctness half of the sampling case, on a table a debug build reads
    /// quickly: the delimiter is still the one every row uses.
    #[test]
    fn the_delimiter_of_a_smaller_table_is_detected() {
        let mut text = String::from("a,b,c\n");
        for index in 0..2_000 {
            let _ = writeln!(text, "{index},{index},{index}");
        }
        let format = detect_format(&text, &DetectOptions::default());
        assert_eq!(delimiter_of(&format.syntax), Some(','));
    }

    #[test]
    fn identifier_columns_are_not_numbers() {
        let text = "id\n007\n013\n042\n";
        let format = detect(text, &DetectOptions::default(), &Regional::dot_decimal());
        assert_eq!(format.column_types, vec![ColumnType::Text]);

        let long = "id\n12345678901234567890\n12345678901234567891\n12345678901234567892\n";
        let format = detect(long, &DetectOptions::default(), &Regional::dot_decimal());
        assert_eq!(format.column_types, vec![ColumnType::Text]);
    }

    #[test]
    fn a_column_that_changes_late_is_still_seen() {
        let mut text = String::from("v\n");
        for index in 0..=5_000 {
            let value = if index == 3_999 {
                "n/a".to_owned()
            } else {
                (index + 1).to_string()
            };
            let _ = writeln!(text, "{value}");
        }
        let table = {
            let mut options = ParseOptions::comma_separated();
            options.first_line_contains = FirstLineContains::ColumnNames;
            parse(&text, &options).unwrap()
        };
        let sample = column_type_of_sample(&table, 0, TYPE_SAMPLE_BUDGET, &Regional::dot_decimal());
        assert_eq!(sample.column_type, ColumnType::Text);
        assert_eq!(sample.failed, 1);
        assert!(sample.sampled <= TYPE_SAMPLE_BUDGET as u64);
    }

    #[test]
    fn a_byte_order_mark_does_not_reach_the_first_cell() {
        let format = detect_format("\u{feff}id,name\n1,Ann\n2,Bob\n", &DetectOptions::default());
        assert_eq!(delimiter_of(&format.syntax), Some(','));
        assert_eq!(format.first_line_contains, FirstLineContains::ColumnNames);
    }

    #[test]
    fn options_round_trip_with_unknown_fields() {
        let text = r#"{"sampleLines":10,"futureFlag":"x"}"#;
        let options: DetectOptions = serde_json::from_str(text).unwrap();
        assert_eq!(options.sample_lines, 10);
        let back = serde_json::to_string(&options).unwrap();
        assert!(back.contains("futureFlag"));
    }
}
