//! Bounded time checks on a large table.
//!
//! The cases work on one million rows by five columns. A budget describes an
//! optimized build, so they run there alone; a small always-on case beside
//! them keeps the counts they check.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ca_table::align::{align, RowAlignOptions};
use ca_table::compare::compare;
use ca_table::parse::{parse, FirstLineContains, ParseOptions, Table};
use ca_table::schema::{ColumnHandling, Schema, SchemaSettings};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::{Duration, Instant};

/// Rows generated.
const ROWS: usize = 1_000_000;

/// Time a single stage may take.
const BUDGET: Duration = Duration::from_secs(60);

fn build_text(rows: usize, shift: usize) -> String {
    let mut text = String::with_capacity(rows * 40);
    text.push_str("id,name,city,amount,when\n");
    for row in 0..rows {
        let key = row + shift;
        let _ = writeln!(
            text,
            "{key},name{},city{},{}.50,01/02/2020",
            row % 997,
            row % 31,
            row % 5_000
        );
    }
    text
}

fn options() -> ParseOptions {
    let mut options = ParseOptions::comma_separated();
    options.first_line_contains = FirstLineContains::ColumnNames;
    options
}

fn keyed_schema(left: &Table, right: &Table) -> Schema {
    let mut handling = BTreeMap::new();
    handling.insert(
        0,
        ColumnHandling {
            key: true,
            ..ColumnHandling::default()
        },
    );
    Schema::build(
        left,
        right,
        &SchemaSettings {
            handling,
            ..SchemaSettings::default()
        },
    )
}

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "wall-clock budget holds for release builds"
)]
fn a_million_rows_parse_align_and_compare_within_the_budget() {
    let rows = ROWS;
    let left_text = build_text(rows, 0);
    let right_text = build_text(rows, 1);

    let start = Instant::now();
    let left = parse(&left_text, &options()).expect("parsing the left file");
    let right = parse(&right_text, &options()).expect("parsing the right file");
    let parse_time = start.elapsed();
    assert_eq!(left.row_count(), rows);
    assert_eq!(left.column_count(), 5);
    assert_eq!(right.row_count(), rows);

    let schema = keyed_schema(&left, &right);
    assert!(schema.has_keys());

    let start = Instant::now();
    let alignment = align(&left, &right, &schema, &RowAlignOptions::default())
        .expect("aligning by the key column");
    let align_time = start.elapsed();
    // One key is missing from each side, so every other row pairs.
    assert_eq!(alignment.pairs.len(), rows + 1);

    let start = Instant::now();
    let result = compare(&left, &right, &schema, &alignment).expect("comparing the rows");
    let compare_time = start.elapsed();
    assert_eq!(result.rows.len(), rows + 1);
    assert_eq!(result.totals.left_only, 1);
    assert_eq!(result.totals.right_only, 1);

    println!(
        "rows {rows}: parse {parse_time:?} (two files), keyed align {align_time:?}, compare {compare_time:?}"
    );
    assert!(parse_time < BUDGET, "parse took {parse_time:?}");
    assert!(align_time < BUDGET, "align took {align_time:?}");
    assert!(compare_time < BUDGET, "compare took {compare_time:?}");
}

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "wall-clock budget holds for release builds"
)]
fn a_million_rows_align_without_keys_within_the_budget() {
    let rows = ROWS;
    let left = parse(&build_text(rows, 0), &options()).expect("parsing the left file");
    let right = parse(&build_text(rows, 1), &options()).expect("parsing the right file");
    let schema = Schema::build(&left, &right, &SchemaSettings::default());
    assert!(!schema.has_keys());

    let start = Instant::now();
    let alignment =
        align(&left, &right, &schema, &RowAlignOptions::default()).expect("aligning without keys");
    let align_time = start.elapsed();
    let covered_left = alignment.pairs.iter().filter(|p| p.left.is_some()).count();
    let covered_right = alignment.pairs.iter().filter(|p| p.right.is_some()).count();
    assert_eq!(covered_left, rows);
    assert_eq!(covered_right, rows);

    println!("rows {rows}: keyless align {align_time:?}");
    assert!(align_time < BUDGET, "align took {align_time:?}");
}

/// Correctness half of the scale cases, on a table a debug build handles: the
/// parse, the keyed alignment and the comparison report the same counts they
/// report on a million rows.
#[test]
fn a_small_table_parses_aligns_and_compares_to_the_same_counts() {
    let rows = 2_000;
    let left = parse(&build_text(rows, 0), &options()).expect("parsing the left file");
    let right = parse(&build_text(rows, 1), &options()).expect("parsing the right file");
    assert_eq!(left.row_count(), rows);
    assert_eq!(left.column_count(), 5);

    let schema = keyed_schema(&left, &right);
    assert!(schema.has_keys());
    let alignment =
        align(&left, &right, &schema, &RowAlignOptions::default()).expect("aligning by the key");
    assert_eq!(alignment.pairs.len(), rows + 1);

    let result = compare(&left, &right, &schema, &alignment).expect("comparing the rows");
    assert_eq!(result.rows.len(), rows + 1);
    assert_eq!(result.totals.left_only, 1);
    assert_eq!(result.totals.right_only, 1);

    let keyless = Schema::build(&left, &right, &SchemaSettings::default());
    assert!(!keyless.has_keys());
    let alignment =
        align(&left, &right, &keyless, &RowAlignOptions::default()).expect("aligning without keys");
    assert_eq!(
        alignment.pairs.iter().filter(|p| p.left.is_some()).count(),
        rows
    );
}
