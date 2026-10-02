//! Properties the engine holds for any input.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ca_table::align::{align, RowAlignOptions, RowAlignmentMode, RowPair};
use ca_table::compare::compare;
use ca_table::parse::{parse, FieldSyntax, FirstLineContains, ParseOptions};
use ca_table::schema::{ColumnHandling, ColumnType, Schema, SchemaSettings};
use ca_table::{CellStatus, Unknown};
use proptest::prelude::*;
use std::collections::BTreeMap;

/// Fractional digits the reference arithmetic works in. It holds both the six
/// digits a generated value may carry and the ten a tolerance of `k / 1024`
/// needs.
const REFERENCE_SCALE: u32 = 10;

/// A written decimal, and the same value as an integer count of
/// `10^-REFERENCE_SCALE`.
#[derive(Debug, Clone)]
struct Sample {
    text: String,
    units: i128,
    leading_zero_identifier: bool,
}

fn sample_strategy() -> impl Strategy<Value = Sample> {
    (
        proptest::bool::ANY,
        prop_oneof![
            proptest::string::string_regex("[1-9][0-9]{0,19}")
                .expect("a nonzero integer pattern")
                .prop_map(|integer| (integer, false)),
            Just(("0".to_owned(), false)),
            proptest::string::string_regex("0[0-9]{1,19}")
                .expect("a leading-zero integer pattern")
                .prop_map(|integer| (integer, true)),
        ],
        proptest::option::of(
            proptest::string::string_regex("[0-9]{1,6}").expect("a valid fraction pattern"),
        ),
    )
        .prop_map(|(negative, (integer, leading_zero_identifier), fraction)| {
            let mut text = String::new();
            if negative {
                text.push('-');
            }
            text.push_str(&integer);
            if let Some(fraction) = &fraction {
                text.push('.');
                text.push_str(fraction);
            }
            let fraction = fraction.unwrap_or_default();
            let mut digits = integer;
            digits.push_str(&fraction);
            for _ in 0..(REFERENCE_SCALE - u32::try_from(fraction.len()).unwrap_or(0)) {
                digits.push('0');
            }
            let magnitude: i128 = digits.parse().expect("a bounded digit string");
            Sample {
                text,
                units: if negative { -magnitude } else { magnitude },
                leading_zero_identifier,
            }
        })
}

#[test]
fn leading_zero_integer_spellings_keep_their_documented_verdicts() {
    assert_eq!(number_verdict("007", "7", 1.0), CellStatus::Different);
    assert_eq!(number_verdict("007", "008", 1.0), CellStatus::Unimportant);
}

/// Compares one cell of a number column with the given tolerance.
fn number_verdict(left: &str, right: &str, tolerance: f64) -> CellStatus {
    let options = {
        let mut options = ParseOptions::comma_separated();
        options.first_line_contains = FirstLineContains::ColumnNames;
        options
    };
    let left = parse(&format!("v\n{left}\n"), &options).expect("parsing the left cell");
    let right = parse(&format!("v\n{right}\n"), &options).expect("parsing the right cell");
    let mut handling = BTreeMap::new();
    handling.insert(
        0,
        ColumnHandling {
            use_default: false,
            column_type: ColumnType::Number,
            numeric_tolerance: tolerance,
            ..ColumnHandling::default()
        },
    );
    let settings = SchemaSettings {
        handling,
        ..SchemaSettings::default()
    };
    let schema = Schema::build(&left, &right, &settings);
    assert_eq!(schema.columns[0].effective_type, ColumnType::Number);
    let alignment = ca_table::Alignment {
        pairs: vec![RowPair::both(0, 0)],
        keyed: false,
        duplicate_keys: Vec::new(),
        duplicate_keys_dropped: 0,
    };
    let result = compare(&left, &right, &schema, &alignment).expect("comparing one cell");
    result.cell(0, 0).expect("one classified cell")
}

/// Options that keep every character of a cell, so a round trip is exact.
fn exact_options() -> ParseOptions {
    ParseOptions {
        syntax: FieldSyntax::Delimited {
            delimiters: vec![','],
            text_qualifier: Some('"'),
            consecutive_delimiters_as_one: false,
            surrounding_whitespace_is_delimiter: false,
            unknown: Unknown::new(),
        },
        first_line_contains: FirstLineContains::CellData,
        unknown: Unknown::new(),
    }
}

/// Writes cells back out in the syntax [`exact_options`] reads.
fn write_csv(rows: &[Vec<String>]) -> String {
    let mut out = String::new();
    for (index, row) in rows.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        for (column, cell) in row.iter().enumerate() {
            if column > 0 {
                out.push(',');
            }
            if cell.contains([',', '"', '\n', '\r']) {
                out.push('"');
                for ch in cell.chars() {
                    if ch == '"' {
                        out.push('"');
                    }
                    out.push(ch);
                }
                out.push('"');
            } else {
                out.push_str(cell);
            }
        }
    }
    out.push('\n');
    out
}

fn read_csv(text: &str) -> Vec<Vec<String>> {
    let table = parse(text, &exact_options()).expect("parsing a bounded sample");
    (0..table.row_count())
        .map(|row| {
            table
                .row(row)
                .map(|cell| cell.text().into_owned())
                .collect()
        })
        .collect()
}

fn cell_strategy() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[a-c0-9 ,\"\n\t;]{0,6}").expect("a valid sample pattern")
}

fn table_strategy(max_rows: usize, max_columns: usize) -> impl Strategy<Value = Vec<Vec<String>>> {
    proptest::collection::vec(
        proptest::collection::vec(cell_strategy(), 1..=max_columns),
        1..=max_rows,
    )
}

fn table_of(rows: &[Vec<String>]) -> ca_table::Table {
    parse(&write_csv(rows), &exact_options()).expect("parsing written cells")
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Writing a parsed table out and reading it back yields the same cells,
    /// and doing it once more changes nothing further.
    #[test]
    fn a_write_and_read_round_trip_is_stable(rows in table_strategy(6, 4)) {
        let text = write_csv(&rows);
        let once = read_csv(&text);
        prop_assert_eq!(&once, &rows);
        let twice = read_csv(&write_csv(&once));
        prop_assert_eq!(&twice, &once);
    }

    /// Every cell's source range names the characters the file holds for it.
    #[test]
    fn source_ranges_name_the_source(rows in table_strategy(6, 4)) {
        let text = write_csv(&rows);
        let table = parse(&text, &exact_options()).expect("parsing written cells");
        for row in 0..table.row_count() {
            for cell in table.row(row) {
                let range = cell.source_range();
                let slice = text
                    .get(range.start as usize..range.end as usize)
                    .expect("a range on a character boundary");
                prop_assert_eq!(slice, cell.raw());
                let value = cell.value_range();
                prop_assert!(value.start >= range.start);
                prop_assert!(value.end <= range.end);
            }
        }
    }

    /// Keyless alignment puts every row of both files on exactly one line.
    #[test]
    fn keyless_alignment_covers_every_row(
        left_rows in table_strategy(8, 3),
        right_rows in table_strategy(8, 3),
        mode_choice in 0usize..4,
        sort in proptest::bool::ANY,
    ) {
        let left = table_of(&left_rows);
        let right = table_of(&right_rows);
        let schema = Schema::build(&left, &right, &SchemaSettings::default());
        let mode = [
            RowAlignmentMode::Standard,
            RowAlignmentMode::Myers,
            RowAlignmentMode::Patience,
            RowAlignmentMode::Unaligned,
        ]
        .get(mode_choice)
        .cloned()
        .unwrap_or(RowAlignmentMode::Standard);
        let options = RowAlignOptions {
            mode,
            sort_rows_before_alignment: sort,
            ..RowAlignOptions::default()
        };
        let alignment = align(&left, &right, &schema, &options).expect("aligning rows");
        assert_covers(&alignment, left.row_count(), right.row_count())?;
    }

    /// Keyed alignment puts every row of both files on exactly one line, and
    /// never lets a line run backwards on either side.
    #[test]
    fn keyed_alignment_covers_every_row(
        left_rows in table_strategy(8, 3),
        right_rows in table_strategy(8, 3),
        sort in proptest::bool::ANY,
    ) {
        let left = table_of(&left_rows);
        let right = table_of(&right_rows);
        let mut handling = BTreeMap::new();
        handling.insert(
            0,
            ColumnHandling {
                key: true,
                ..ColumnHandling::default()
            },
        );
        let settings = SchemaSettings {
            handling,
            ..SchemaSettings::default()
        };
        let schema = Schema::build(&left, &right, &settings);
        prop_assume!(schema.has_keys());
        let options = RowAlignOptions {
            sort_rows_before_alignment: sort,
            ..RowAlignOptions::default()
        };
        let alignment = align(&left, &right, &schema, &options).expect("aligning rows");
        assert_covers(&alignment, left.row_count(), right.row_count())?;
        if !sort {
            let mut last_left: Option<u32> = None;
            let mut last_right: Option<u32> = None;
            for pair in &alignment.pairs {
                if let Some(row) = pair.left {
                    prop_assert!(last_left.is_none_or(|previous| previous < row));
                    last_left = Some(row);
                }
                if let Some(row) = pair.right {
                    prop_assert!(last_right.is_none_or(|previous| previous < row));
                    last_right = Some(row);
                }
            }
        }
    }

    /// Comparison produces one status per comparison column per line, and the
    /// row totals add up to the number of lines.
    #[test]
    fn comparison_covers_the_alignment(
        left_rows in table_strategy(6, 3),
        right_rows in table_strategy(6, 3),
    ) {
        let left = table_of(&left_rows);
        let right = table_of(&right_rows);
        let schema = Schema::build(&left, &right, &SchemaSettings::default());
        let alignment =
            align(&left, &right, &schema, &RowAlignOptions::default()).expect("aligning rows");
        let result = compare(&left, &right, &schema, &alignment).expect("comparing rows");
        prop_assert_eq!(result.rows.len(), alignment.pairs.len());
        for row in 0..result.rows.len() {
            prop_assert_eq!(result.cells(row).len(), schema.columns.len());
        }
        let counted = result.totals.same
            + result.totals.different
            + result.totals.unimportant
            + result.totals.left_only
            + result.totals.right_only;
        prop_assert_eq!(usize::try_from(counted).ok(), Some(alignment.pairs.len()));
    }

    /// Number spellings compare by exact decimal equality, preserve leading
    /// zero identifiers, and use exact distance when tolerance applies.
    #[test]
    fn number_verdicts_follow_exact_decimal_arithmetic(
        left in sample_strategy(),
        right in sample_strategy(),
        tolerance_1024ths in 0i128..=1_024,
    ) {
        // A tolerance of k / 1024 is held exactly by a double, so the written
        // tolerance and the reference tolerance are the same number.
        let tolerance = f64::from(i32::try_from(tolerance_1024ths).unwrap_or(0)) / 1_024.0;
        let tolerance_units = tolerance_1024ths * 9_765_625;

        let verdict = number_verdict(&left.text, &right.text, tolerance);
        let distance = (left.units - right.units).abs();
        let expected = if distance == 0 {
            if left.text == right.text
                || !left.leading_zero_identifier && !right.leading_zero_identifier
            {
                CellStatus::Same
            } else {
                CellStatus::Different
            }
        } else if distance <= tolerance_units {
            CellStatus::Unimportant
        } else {
            CellStatus::Different
        };
        prop_assert_eq!(verdict, expected, "{} against {} at {}", left.text, right.text, tolerance);
    }
}

fn assert_covers(
    alignment: &ca_table::Alignment,
    left_rows: usize,
    right_rows: usize,
) -> Result<(), TestCaseError> {
    let mut left_seen = vec![0u32; left_rows];
    let mut right_seen = vec![0u32; right_rows];
    for pair in &alignment.pairs {
        prop_assert!(pair.left.is_some() || pair.right.is_some());
        if let Some(row) = pair.left {
            let slot = left_seen
                .get_mut(row as usize)
                .ok_or_else(|| TestCaseError::fail("a line names a left row that is not there"))?;
            *slot += 1;
        }
        if let Some(row) = pair.right {
            let slot = right_seen
                .get_mut(row as usize)
                .ok_or_else(|| TestCaseError::fail("a line names a right row that is not there"))?;
            *slot += 1;
        }
    }
    prop_assert!(left_seen.iter().all(|count| *count == 1));
    prop_assert!(right_seen.iter().all(|count| *count == 1));
    Ok(())
}
