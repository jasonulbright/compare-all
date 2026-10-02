//! Pairing rows of the left file with rows of the right file.
//!
//! Two strategies exist. When the schema marks key columns, rows pair only
//! when every key column holds the same value, and the two files may be sorted
//! by key first. When no key column is marked, a per row normalization key is
//! built from the mapped columns and the two sequences of keys are aligned by
//! the sequence diff engine.
//!
//! Both strategies cover every row exactly once: each row appears in exactly
//! one [`RowPair`], either paired with a row on the other side or alone.
//!
//! Duplicate keys are legal. The duplicates on one side pair with the
//! duplicates on the other in file order, first with first; a surplus on
//! either side is left unpaired and reported in
//! [`Alignment::duplicate_keys`].

use crate::parse::Table;
use crate::schema::{ColumnType, Schema};
use crate::value::{has_leading_zero_integer, normalize_text, parse_date, parse_number};
use crate::{Result, TableError, Unknown};
use ca_diff::{
    diff_line_slices_cancellable, AlignmentOptions, Cancel, HunkKind, LineCompareOptions,
    NeverCancel,
};
use serde::{Deserialize, Serialize};

/// Separator between key column values. Cell text can hold this character, so
/// every part is escaped before it is joined: a part carries neither the
/// separator nor an unescaped backslash, and two different column splits
/// cannot build the same key.
const KEY_SEPARATOR: char = '\u{1f}';

/// Escape character introduced by [`push_escaped`].
const KEY_ESCAPE: char = '\\';

/// Largest number of duplicate keys reported. Beyond this the report is
/// truncated so a file of one repeated key cannot exhaust memory.
pub const MAX_DUPLICATE_REPORTS: usize = 256;

/// Rows checked between two cancellation polls.
const CANCEL_STRIDE: usize = 4_096;

/// Algorithm used to pair rows when no key column is marked.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum RowAlignmentMode {
    /// Pair row one with row one and so on, whatever the rows hold.
    Unaligned,
    /// Align by comparing successively smaller sections of each file.
    #[default]
    Standard,
    /// Align by longest common subsequence.
    Myers,
    /// Align on rows that occur once on each side.
    Patience,
    /// A mode written by another build. It is handled as
    /// [`RowAlignmentMode::Standard`].
    #[serde(untagged)]
    Unknown(serde_json::Value),
}

/// Controls over row pairing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RowAlignOptions {
    /// Algorithm used when no key column is marked.
    pub mode: RowAlignmentMode,
    /// Show rows with important differences as blocks of added and deleted
    /// rows instead of paired changed rows.
    pub never_align_differences: bool,
    /// Largest number of rows scanned when looking for a match, or `None` for
    /// an unbounded search.
    pub skew_tolerance: Option<u32>,
    /// Pair rows still unmatched after the main pass by similarity.
    pub use_closeness_matching: bool,
    /// Reorder the rows of each file before aligning them.
    pub sort_rows_before_alignment: bool,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, skip_serializing_if = "Unknown::is_empty")]
    pub unknown: Unknown,
}

impl Default for RowAlignOptions {
    fn default() -> Self {
        Self {
            mode: RowAlignmentMode::Standard,
            never_align_differences: false,
            skew_tolerance: None,
            use_closeness_matching: true,
            sort_rows_before_alignment: false,
            unknown: Unknown::new(),
        }
    }
}

/// One line of the aligned comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowPair {
    /// Zero based row of the left file, or `None` when the line is right only.
    pub left: Option<u32>,
    /// Zero based row of the right file, or `None` when the line is left only.
    pub right: Option<u32>,
}

impl RowPair {
    /// A line holding a row from each side.
    #[must_use]
    pub fn both(left: u32, right: u32) -> Self {
        Self {
            left: Some(left),
            right: Some(right),
        }
    }

    /// A line holding only a left row.
    #[must_use]
    pub fn left_only(left: u32) -> Self {
        Self {
            left: Some(left),
            right: None,
        }
    }

    /// A line holding only a right row.
    #[must_use]
    pub fn right_only(right: u32) -> Self {
        Self {
            left: None,
            right: Some(right),
        }
    }
}

/// A key that occurs more than once on at least one side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateKey {
    /// The key as it was built from the key columns, with the part boundaries
    /// written as vertical bars for display.
    pub key: String,
    /// Number of left rows carrying it.
    pub left_rows: u32,
    /// Number of right rows carrying it.
    pub right_rows: u32,
}

/// The result of pairing rows.
#[derive(Debug, Clone, PartialEq)]
pub struct Alignment {
    /// The comparison lines, in display order.
    pub pairs: Vec<RowPair>,
    /// Whether key columns drove the pairing.
    pub keyed: bool,
    /// Keys that occur more than once, truncated at
    /// [`MAX_DUPLICATE_REPORTS`].
    pub duplicate_keys: Vec<DuplicateKey>,
    /// Number of duplicate keys that were not reported.
    pub duplicate_keys_dropped: u64,
}

impl Alignment {
    /// Number of comparison lines.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pairs.len()
    }

    /// Whether there is nothing to show.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }
}

/// Pair the rows of two files.
///
/// # Errors
///
/// Returns [`TableError::InvalidSettings`] when the sequence engine rejects
/// the request.
pub fn align(
    left: &Table,
    right: &Table,
    schema: &Schema,
    options: &RowAlignOptions,
) -> Result<Alignment> {
    align_cancellable(left, right, schema, options, &NeverCancel)
}

/// Pair the rows of two files, abandoning the work when `cancel` is raised.
///
/// # Errors
///
/// Returns [`TableError::Cancelled`] when the flag is raised before the work
/// finishes, and [`TableError::InvalidSettings`] when the sequence engine
/// rejects the request.
pub fn align_cancellable(
    left: &Table,
    right: &Table,
    schema: &Schema,
    options: &RowAlignOptions,
    cancel: &dyn Cancel,
) -> Result<Alignment> {
    if schema.has_keys() {
        align_by_keys(left, right, schema, options, cancel)
    } else {
        align_without_keys(left, right, schema, options, cancel)
    }
}

fn check(cancel: &dyn Cancel) -> Result<()> {
    if cancel.is_cancelled() {
        Err(TableError::Cancelled)
    } else {
        Ok(())
    }
}

/// Builds the comparison key of one row from the given comparison columns.
fn row_key(
    table: &Table,
    row: usize,
    schema: &Schema,
    on_right: bool,
    columns: &[usize],
) -> String {
    let regional = if on_right {
        &schema.right_regional
    } else {
        &schema.left_regional
    };
    let mut key = String::new();
    for (position, index) in columns.iter().enumerate() {
        if position > 0 {
            key.push(KEY_SEPARATOR);
        }
        let Some(column) = schema.columns.get(*index) else {
            continue;
        };
        let source = if on_right { column.right } else { column.left };
        let Some(source) = source else {
            continue;
        };
        let text = table.cell_text(row, source as usize);
        let as_text = || {
            normalize_text(
                &text,
                column.handling.ignore_case,
                column.handling.ignore_whitespace,
            )
        };
        let part = match column.effective_type {
            // The exact decimal form makes `1` and `1.0` one key, and keeps
            // two identifiers that differ in a late digit apart.
            ColumnType::Number => {
                if has_leading_zero_integer(&text) {
                    as_text()
                } else {
                    parse_number(text.trim(), regional).map_or_else(as_text, |value| value.key())
                }
            }
            ColumnType::DateTime => parse_date(text.trim(), regional)
                .map_or_else(as_text, |value| value.seconds().to_string()),
            _ => as_text(),
        };
        push_escaped(&mut key, &part);
    }
    key
}

/// Appends one key part, escaping the separator so no part can be mistaken for
/// a part boundary.
fn push_escaped(key: &mut String, part: &str) {
    if !part.contains([KEY_ESCAPE, KEY_SEPARATOR]) {
        key.push_str(part);
        return;
    }
    for ch in part.chars() {
        match ch {
            KEY_ESCAPE => key.push_str("\\\\"),
            KEY_SEPARATOR => key.push_str("\\u"),
            _ => key.push(ch),
        }
    }
}

/// The parts of a key, written for a person to read.
fn display_key(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    let mut chars = key.chars();
    while let Some(ch) = chars.next() {
        match ch {
            KEY_SEPARATOR => out.push_str(" | "),
            KEY_ESCAPE => match chars.next() {
                Some('u') => out.push(KEY_SEPARATOR),
                Some(other) => out.push(other),
                None => {}
            },
            _ => out.push(ch),
        }
    }
    out
}

fn build_keys(
    table: &Table,
    schema: &Schema,
    on_right: bool,
    columns: &[usize],
    cancel: &dyn Cancel,
) -> Result<Vec<String>> {
    let mut keys = Vec::with_capacity(table.row_count());
    for row in 0..table.row_count() {
        if row % CANCEL_STRIDE == 0 {
            check(cancel)?;
        }
        keys.push(row_key(table, row, schema, on_right, columns));
    }
    Ok(keys)
}

fn align_by_keys(
    left: &Table,
    right: &Table,
    schema: &Schema,
    options: &RowAlignOptions,
    cancel: &dyn Cancel,
) -> Result<Alignment> {
    let columns = schema.key_columns();
    let left_keys = build_keys(left, schema, false, &columns, cancel)?;
    let right_keys = build_keys(right, schema, true, &columns, cancel)?;
    let (duplicate_keys, dropped) = duplicate_report(&left_keys, &right_keys);
    check(cancel)?;

    let pairs = if options.sort_rows_before_alignment {
        merge_sorted(&left_keys, &right_keys)
    } else {
        pair_in_file_order(&left_keys, &right_keys, cancel)?
    };
    Ok(Alignment {
        pairs,
        keyed: true,
        duplicate_keys,
        duplicate_keys_dropped: dropped,
    })
}

/// Sorts both sides by key and walks the two sorted runs together. Equal keys
/// pair in file order, first with first.
fn merge_sorted(left_keys: &[String], right_keys: &[String]) -> Vec<RowPair> {
    let mut left_order: Vec<u32> = (0..index_u32(left_keys.len())).collect();
    let mut right_order: Vec<u32> = (0..index_u32(right_keys.len())).collect();
    left_order.sort_by(|a, b| {
        key_at(left_keys, *a)
            .cmp(key_at(left_keys, *b))
            .then(a.cmp(b))
    });
    right_order.sort_by(|a, b| {
        key_at(right_keys, *a)
            .cmp(key_at(right_keys, *b))
            .then(a.cmp(b))
    });
    let mut pairs = Vec::with_capacity(left_order.len().max(right_order.len()));
    let (mut i, mut j) = (0usize, 0usize);
    while i < left_order.len() && j < right_order.len() {
        let left_row = left_order.get(i).copied().unwrap_or(0);
        let right_row = right_order.get(j).copied().unwrap_or(0);
        match key_at(left_keys, left_row).cmp(key_at(right_keys, right_row)) {
            std::cmp::Ordering::Equal => {
                pairs.push(RowPair::both(left_row, right_row));
                i += 1;
                j += 1;
            }
            std::cmp::Ordering::Less => {
                pairs.push(RowPair::left_only(left_row));
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                pairs.push(RowPair::right_only(right_row));
                j += 1;
            }
        }
    }
    for row in left_order.get(i..).unwrap_or_default() {
        pairs.push(RowPair::left_only(*row));
    }
    for row in right_order.get(j..).unwrap_or_default() {
        pairs.push(RowPair::right_only(*row));
    }
    pairs
}

/// Pairs equal keys without reordering either file. Pairs that would cross
/// each other are demoted to unpaired rows, so the comparison lines stay in
/// file order on both sides.
fn pair_in_file_order(
    left_keys: &[String],
    right_keys: &[String],
    cancel: &dyn Cancel,
) -> Result<Vec<RowPair>> {
    let mut queues: std::collections::HashMap<&str, std::collections::VecDeque<u32>> =
        std::collections::HashMap::with_capacity(right_keys.len());
    for (row, key) in right_keys.iter().enumerate() {
        queues
            .entry(key.as_str())
            .or_default()
            .push_back(index_u32(row));
    }
    let mut candidates: Vec<(u32, u32)> = Vec::new();
    for (row, key) in left_keys.iter().enumerate() {
        if row % CANCEL_STRIDE == 0 {
            check(cancel)?;
        }
        if let Some(queue) = queues.get_mut(key.as_str()) {
            if let Some(matched) = queue.pop_front() {
                candidates.push((index_u32(row), matched));
            }
        }
    }
    check(cancel)?;
    let anchors = longest_increasing(&candidates);
    let mut pairs = Vec::with_capacity(left_keys.len().max(right_keys.len()));
    let mut next_left = 0u32;
    let mut next_right = 0u32;
    for (left_row, right_row) in anchors {
        while next_left < left_row {
            pairs.push(RowPair::left_only(next_left));
            next_left += 1;
        }
        while next_right < right_row {
            pairs.push(RowPair::right_only(next_right));
            next_right += 1;
        }
        pairs.push(RowPair::both(left_row, right_row));
        next_left = left_row + 1;
        next_right = right_row + 1;
    }
    while (next_left as usize) < left_keys.len() {
        pairs.push(RowPair::left_only(next_left));
        next_left += 1;
    }
    while (next_right as usize) < right_keys.len() {
        pairs.push(RowPair::right_only(next_right));
        next_right += 1;
    }
    Ok(pairs)
}

/// The longest run of candidate pairs whose right rows increase. Candidates
/// arrive with increasing left rows, so the result is monotone on both sides.
fn longest_increasing(candidates: &[(u32, u32)]) -> Vec<(u32, u32)> {
    if candidates.is_empty() {
        return Vec::new();
    }
    // `tails[k]` holds the index of the candidate ending the best run of
    // length k+1 found so far; `previous` links each candidate to its
    // predecessor in that run.
    let mut tails: Vec<usize> = Vec::new();
    let mut previous: Vec<Option<usize>> = vec![None; candidates.len()];
    for (index, (_, right)) in candidates.iter().enumerate() {
        let position = tails.partition_point(|tail| {
            candidates
                .get(*tail)
                .is_some_and(|(_, other)| *other < *right)
        });
        if position > 0 {
            previous.get_mut(index).map(|slot| {
                *slot = tails.get(position - 1).copied();
                slot
            });
        }
        if position == tails.len() {
            tails.push(index);
        } else if let Some(slot) = tails.get_mut(position) {
            *slot = index;
        }
    }
    let mut run = Vec::with_capacity(tails.len());
    let mut cursor = tails.last().copied();
    while let Some(index) = cursor {
        if let Some(pair) = candidates.get(index) {
            run.push(*pair);
        }
        cursor = previous.get(index).copied().flatten();
    }
    run.reverse();
    run
}

fn key_at(keys: &[String], row: u32) -> &str {
    keys.get(row as usize).map_or("", String::as_str)
}

fn duplicate_report(left_keys: &[String], right_keys: &[String]) -> (Vec<DuplicateKey>, u64) {
    let mut counts: std::collections::HashMap<&str, (u32, u32)> = std::collections::HashMap::new();
    for key in left_keys {
        counts.entry(key.as_str()).or_default().0 += 1;
    }
    for key in right_keys {
        counts.entry(key.as_str()).or_default().1 += 1;
    }
    let mut duplicates: Vec<DuplicateKey> = counts
        .into_iter()
        .filter(|(_, (left, right))| *left > 1 || *right > 1)
        .map(|(key, (left_rows, right_rows))| DuplicateKey {
            key: display_key(key),
            left_rows,
            right_rows,
        })
        .collect();
    duplicates.sort_by(|a, b| a.key.cmp(&b.key));
    let dropped =
        u64::try_from(duplicates.len().saturating_sub(MAX_DUPLICATE_REPORTS)).unwrap_or(0);
    duplicates.truncate(MAX_DUPLICATE_REPORTS);
    (duplicates, dropped)
}

fn align_without_keys(
    left: &Table,
    right: &Table,
    schema: &Schema,
    options: &RowAlignOptions,
    cancel: &dyn Cancel,
) -> Result<Alignment> {
    let columns: Vec<usize> = schema
        .columns
        .iter()
        .enumerate()
        .filter(|(_, column)| column.is_mapped() && !column.handling.unimportant)
        .map(|(index, _)| index)
        .collect();
    let columns = if columns.is_empty() {
        (0..schema.columns.len()).collect()
    } else {
        columns
    };
    let left_keys = build_keys(left, schema, false, &columns, cancel)?;
    let right_keys = build_keys(right, schema, true, &columns, cancel)?;

    let pairs = if options.sort_rows_before_alignment {
        merge_sorted(&left_keys, &right_keys)
    } else if matches!(options.mode, RowAlignmentMode::Unaligned) {
        pair_positionally(left_keys.len(), right_keys.len())
    } else {
        let left_view: Vec<&str> = left_keys.iter().map(String::as_str).collect();
        let right_view: Vec<&str> = right_keys.iter().map(String::as_str).collect();
        let compare = LineCompareOptions {
            ignore_case: false,
            ignore_leading_whitespace: false,
            ignore_embedded_whitespace: false,
            ignore_trailing_whitespace: false,
            ignore_all_whitespace: false,
            ignore_line_endings: false,
            alignment: AlignmentOptions {
                mode: sequence_mode(&options.mode),
                skew_tolerance: options.skew_tolerance,
                never_align_differences: options.never_align_differences,
                use_closeness_matching: options.use_closeness_matching,
            },
        };
        let hunks = diff_line_slices_cancellable(&left_view, &right_view, &compare, cancel)?;
        pairs_from_hunks(&hunks)
    };
    Ok(Alignment {
        pairs,
        keyed: false,
        duplicate_keys: Vec::new(),
        duplicate_keys_dropped: 0,
    })
}

fn sequence_mode(mode: &RowAlignmentMode) -> ca_diff::AlignmentMode {
    match mode {
        RowAlignmentMode::Myers => ca_diff::AlignmentMode::Myers,
        RowAlignmentMode::Patience => ca_diff::AlignmentMode::Patience,
        RowAlignmentMode::Unaligned => ca_diff::AlignmentMode::Unaligned,
        RowAlignmentMode::Standard | RowAlignmentMode::Unknown(_) => {
            ca_diff::AlignmentMode::Standard
        }
    }
}

fn pair_positionally(left_count: usize, right_count: usize) -> Vec<RowPair> {
    (0..left_count.max(right_count))
        .map(|index| {
            let row = index_u32(index);
            RowPair {
                left: (index < left_count).then_some(row),
                right: (index < right_count).then_some(row),
            }
        })
        .collect()
}

fn pairs_from_hunks(hunks: &[ca_diff::Hunk]) -> Vec<RowPair> {
    let mut pairs = Vec::new();
    for hunk in hunks {
        match hunk.kind {
            HunkKind::Same | HunkKind::Changed => {
                let left_len = hunk.left.end.saturating_sub(hunk.left.start);
                let right_len = hunk.right.end.saturating_sub(hunk.right.start);
                let shared = left_len.min(right_len);
                for offset in 0..shared {
                    pairs.push(RowPair::both(
                        hunk.left.start + offset,
                        hunk.right.start + offset,
                    ));
                }
                for offset in shared..left_len {
                    pairs.push(RowPair::left_only(hunk.left.start + offset));
                }
                for offset in shared..right_len {
                    pairs.push(RowPair::right_only(hunk.right.start + offset));
                }
            }
            HunkKind::LeftOnly => {
                for row in hunk.left.clone() {
                    pairs.push(RowPair::left_only(row));
                }
                for row in hunk.right.clone() {
                    pairs.push(RowPair::right_only(row));
                }
            }
            HunkKind::RightOnly => {
                for row in hunk.right.clone() {
                    pairs.push(RowPair::right_only(row));
                }
                for row in hunk.left.clone() {
                    pairs.push(RowPair::left_only(row));
                }
            }
        }
    }
    pairs
}

#[inline]
fn index_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::parse::{parse, FirstLineContains, ParseOptions};
    use crate::schema::{ColumnHandling, SchemaSettings};
    use std::collections::BTreeMap;
    use std::fmt::Write as _;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn headed(text: &str) -> Table {
        let mut options = ParseOptions::comma_separated();
        options.first_line_contains = FirstLineContains::ColumnNames;
        parse(text, &options).unwrap()
    }

    fn keyed_settings(columns: &[u32]) -> SchemaSettings {
        let mut handling = BTreeMap::new();
        for column in columns {
            handling.insert(
                *column,
                ColumnHandling {
                    key: true,
                    ..ColumnHandling::default()
                },
            );
        }
        SchemaSettings {
            handling,
            ..SchemaSettings::default()
        }
    }

    fn covers_every_row(alignment: &Alignment, left_rows: usize, right_rows: usize) {
        let mut left_seen = vec![0u32; left_rows];
        let mut right_seen = vec![0u32; right_rows];
        for pair in &alignment.pairs {
            if let Some(row) = pair.left {
                left_seen[row as usize] += 1;
            }
            if let Some(row) = pair.right {
                right_seen[row as usize] += 1;
            }
            assert!(pair.left.is_some() || pair.right.is_some());
        }
        assert!(left_seen.iter().all(|count| *count == 1));
        assert!(right_seen.iter().all(|count| *count == 1));
    }

    #[test]
    fn keys_pair_rows_whatever_their_position() {
        let left = headed("id,v\n1,a\n2,b\n3,c\n");
        let right = headed("id,v\n3,C\n1,A\n");
        let schema = Schema::build(&left, &right, &keyed_settings(&[0]));
        let alignment = align(&left, &right, &schema, &RowAlignOptions::default()).unwrap();
        assert!(alignment.keyed);
        covers_every_row(&alignment, 3, 2);
        let paired: Vec<RowPair> = alignment
            .pairs
            .iter()
            .copied()
            .filter(|pair| pair.left.is_some() && pair.right.is_some())
            .collect();
        assert_eq!(paired.len(), 1);
    }

    #[test]
    fn sorting_first_pairs_every_shared_key() {
        let left = headed("id,v\n1,a\n2,b\n3,c\n");
        let right = headed("id,v\n3,C\n1,A\n");
        let schema = Schema::build(&left, &right, &keyed_settings(&[0]));
        let options = RowAlignOptions {
            sort_rows_before_alignment: true,
            ..RowAlignOptions::default()
        };
        let alignment = align(&left, &right, &schema, &options).unwrap();
        covers_every_row(&alignment, 3, 2);
        let paired = alignment
            .pairs
            .iter()
            .filter(|pair| pair.left.is_some() && pair.right.is_some())
            .count();
        assert_eq!(paired, 2);
    }

    #[test]
    fn a_number_key_ignores_how_the_number_is_written() {
        let left = headed("id,v\n1.0,a\n2.50,b\n");
        let right = headed("id,v\n1,A\n2.5,B\n");
        let schema = Schema::build(&left, &right, &keyed_settings(&[0]));
        let alignment = align(&left, &right, &schema, &RowAlignOptions::default()).unwrap();
        assert_eq!(alignment.pairs.len(), 2);
        assert!(alignment
            .pairs
            .iter()
            .all(|pair| pair.left.is_some() && pair.right.is_some()));
    }

    #[test]
    fn several_key_columns_must_all_match() {
        let left = headed("a,b,v\n1,x,p\n1,y,q\n");
        let right = headed("a,b,v\n1,y,Q\n1,z,r\n");
        let schema = Schema::build(&left, &right, &keyed_settings(&[0, 1]));
        assert_eq!(schema.key_columns(), vec![0, 1]);
        let alignment = align(&left, &right, &schema, &RowAlignOptions::default()).unwrap();
        covers_every_row(&alignment, 2, 2);
        let paired: Vec<RowPair> = alignment
            .pairs
            .iter()
            .copied()
            .filter(|pair| pair.left.is_some() && pair.right.is_some())
            .collect();
        assert_eq!(paired, vec![RowPair::both(1, 0)]);
    }

    #[test]
    fn duplicate_keys_pair_in_file_order_and_are_reported() {
        let left = headed("id,v\nk,a\nk,b\nk,c\n");
        let right = headed("id,v\nk,A\nk,B\n");
        let schema = Schema::build(&left, &right, &keyed_settings(&[0]));
        let alignment = align(&left, &right, &schema, &RowAlignOptions::default()).unwrap();
        covers_every_row(&alignment, 3, 2);
        assert_eq!(
            alignment.duplicate_keys,
            vec![DuplicateKey {
                key: "k".to_owned(),
                left_rows: 3,
                right_rows: 2,
            }]
        );
        let paired: Vec<RowPair> = alignment
            .pairs
            .iter()
            .copied()
            .filter(|pair| pair.left.is_some() && pair.right.is_some())
            .collect();
        assert_eq!(paired, vec![RowPair::both(0, 0), RowPair::both(1, 1)]);
    }

    #[test]
    fn a_key_missing_on_one_side_stands_alone() {
        let left = headed("id,v\n1,a\n2,b\n");
        let right = headed("id,v\n2,B\n3,c\n");
        let schema = Schema::build(&left, &right, &keyed_settings(&[0]));
        let alignment = align(&left, &right, &schema, &RowAlignOptions::default()).unwrap();
        covers_every_row(&alignment, 2, 2);
        assert!(alignment.pairs.contains(&RowPair::left_only(0)));
        assert!(alignment.pairs.contains(&RowPair::right_only(1)));
    }

    #[test]
    fn an_empty_key_column_still_pairs() {
        let left = headed("id,v\n,a\n,b\n");
        let right = headed("id,v\n,A\n");
        let schema = Schema::build(&left, &right, &keyed_settings(&[0]));
        let alignment = align(&left, &right, &schema, &RowAlignOptions::default()).unwrap();
        covers_every_row(&alignment, 2, 1);
    }

    #[test]
    fn keyless_alignment_uses_the_sequence_engine() {
        let left = headed("a,b\n1,x\n2,y\n3,z\n");
        let right = headed("a,b\n1,x\n3,z\n");
        let schema = Schema::build(&left, &right, &SchemaSettings::default());
        let alignment = align(&left, &right, &schema, &RowAlignOptions::default()).unwrap();
        assert!(!alignment.keyed);
        covers_every_row(&alignment, 3, 2);
        assert!(alignment.pairs.contains(&RowPair::both(0, 0)));
        assert!(alignment.pairs.contains(&RowPair::left_only(1)));
        assert!(alignment.pairs.contains(&RowPair::both(2, 1)));
    }

    #[test]
    fn unaligned_mode_pairs_by_position() {
        let left = headed("a,b\n1,x\n2,y\n3,z\n");
        let right = headed("a,b\n9,q\n");
        let schema = Schema::build(&left, &right, &SchemaSettings::default());
        let options = RowAlignOptions {
            mode: RowAlignmentMode::Unaligned,
            ..RowAlignOptions::default()
        };
        let alignment = align(&left, &right, &schema, &options).unwrap();
        covers_every_row(&alignment, 3, 1);
        assert_eq!(alignment.pairs.first(), Some(&RowPair::both(0, 0)));
    }

    #[test]
    fn every_sequence_mode_covers_every_row() {
        let left = headed("a,b\n1,x\n2,y\n3,z\n4,w\n");
        let right = headed("a,b\n1,x\n9,q\n3,z\n");
        let schema = Schema::build(&left, &right, &SchemaSettings::default());
        for mode in [
            RowAlignmentMode::Standard,
            RowAlignmentMode::Myers,
            RowAlignmentMode::Patience,
            RowAlignmentMode::Unknown(serde_json::Value::from("future")),
        ] {
            let options = RowAlignOptions {
                mode,
                ..RowAlignOptions::default()
            };
            let alignment = align(&left, &right, &schema, &options).unwrap();
            covers_every_row(&alignment, 4, 3);
        }
    }

    #[test]
    fn never_align_differences_splits_changed_blocks() {
        let left = headed("a,b\n1,x\n2,y\n");
        let right = headed("a,b\n1,x\n9,q\n");
        let schema = Schema::build(&left, &right, &SchemaSettings::default());
        let options = RowAlignOptions {
            never_align_differences: true,
            use_closeness_matching: false,
            ..RowAlignOptions::default()
        };
        let alignment = align(&left, &right, &schema, &options).unwrap();
        covers_every_row(&alignment, 2, 2);
        assert!(alignment.pairs.contains(&RowPair::left_only(1)));
        assert!(alignment.pairs.contains(&RowPair::right_only(1)));
    }

    #[test]
    fn a_raised_flag_stops_the_work() {
        let mut text = String::from("a,b\n");
        for index in 0..20_000 {
            let _ = writeln!(text, "{index},x");
        }
        let left = headed(&text);
        let right = headed(&text);
        let schema = Schema::build(&left, &right, &keyed_settings(&[0]));
        let flag = AtomicBool::new(true);
        let result = align_cancellable(&left, &right, &schema, &RowAlignOptions::default(), &flag);
        assert!(matches!(result, Err(TableError::Cancelled)));
        flag.store(false, Ordering::Relaxed);
        assert!(
            align_cancellable(&left, &right, &schema, &RowAlignOptions::default(), &flag).is_ok()
        );
    }

    #[test]
    fn crossing_matches_are_demoted_so_file_order_holds() {
        let left = headed("id,v\n1,a\n2,b\n3,c\n");
        let right = headed("id,v\n3,C\n2,B\n1,A\n");
        let schema = Schema::build(&left, &right, &keyed_settings(&[0]));
        let alignment = align(&left, &right, &schema, &RowAlignOptions::default()).unwrap();
        covers_every_row(&alignment, 3, 3);
        let mut last_left = None;
        let mut last_right = None;
        for pair in &alignment.pairs {
            if let Some(row) = pair.left {
                assert!(last_left.is_none_or(|previous| previous < row));
                last_left = Some(row);
            }
            if let Some(row) = pair.right {
                assert!(last_right.is_none_or(|previous| previous < row));
                last_right = Some(row);
            }
        }
    }

    #[test]
    fn a_separator_inside_a_cell_does_not_move_a_key_boundary() {
        let left = headed("a,b,v\n\"ab\u{1f}\",c,p\n");
        let right = headed("a,b,v\nab,\"\u{1f}c\",q\n");
        let schema = Schema::build(&left, &right, &keyed_settings(&[0, 1]));
        let alignment = align(&left, &right, &schema, &RowAlignOptions::default()).unwrap();
        covers_every_row(&alignment, 1, 1);
        assert!(alignment
            .pairs
            .iter()
            .all(|pair| pair.left.is_none() || pair.right.is_none()));
    }

    #[test]
    fn a_backslash_inside_a_cell_does_not_move_a_key_boundary() {
        let left = headed("a,b,v\n\"x\\\",y,p\n");
        let right = headed("a,b,v\nx,\"\\y\",q\n");
        let schema = Schema::build(&left, &right, &keyed_settings(&[0, 1]));
        let alignment = align(&left, &right, &schema, &RowAlignOptions::default()).unwrap();
        assert!(alignment
            .pairs
            .iter()
            .all(|pair| pair.left.is_none() || pair.right.is_none()));
    }

    #[test]
    fn a_numeric_key_drops_the_sign_of_zero() {
        let left = headed("id,v\n-0,a\n");
        let right = headed("id,v\n0.00,A\n");
        let schema = Schema::build(&left, &right, &keyed_settings(&[0]));
        let alignment = align(&left, &right, &schema, &RowAlignOptions::default()).unwrap();
        assert_eq!(alignment.pairs, vec![RowPair::both(0, 0)]);
    }

    #[test]
    fn a_numeric_key_keeps_long_identifiers_apart() {
        let left = headed("id,v\n12345678901234567890,a\n");
        let right = headed("id,v\n12345678901234567891,A\n");
        let mut settings = keyed_settings(&[0]);
        settings.default_handling.column_type = ColumnType::Number;
        let schema = Schema::build(&left, &right, &settings);
        assert_eq!(schema.columns[0].effective_type, ColumnType::Number);
        let alignment = align(&left, &right, &schema, &RowAlignOptions::default()).unwrap();
        assert!(alignment
            .pairs
            .iter()
            .all(|pair| pair.left.is_none() || pair.right.is_none()));
    }

    #[test]
    fn a_numeric_key_keeps_leading_zero_identifiers_apart() {
        let left = headed("id,v\n02134,a\n");
        let right = headed("id,v\n2134,a\n");
        let mut settings = keyed_settings(&[0]);
        settings.default_handling.column_type = ColumnType::Number;
        let schema = Schema::build(&left, &right, &settings);
        let alignment = align(&left, &right, &schema, &RowAlignOptions::default()).unwrap();
        assert!(alignment
            .pairs
            .iter()
            .all(|pair| pair.left.is_none() || pair.right.is_none()));
    }

    #[test]
    fn options_round_trip_with_unknown_fields() {
        let text = r#"{"mode":"histogram","skewTolerance":40,"futureFlag":false}"#;
        let options: RowAlignOptions = serde_json::from_str(text).unwrap();
        assert!(matches!(options.mode, RowAlignmentMode::Unknown(_)));
        assert_eq!(options.skew_tolerance, Some(40));
        let back = serde_json::to_string(&options).unwrap();
        let again: RowAlignOptions = serde_json::from_str(&back).unwrap();
        assert_eq!(again, options);
        assert!(back.contains("histogram"));
        assert!(back.contains("futureFlag"));
    }
}
