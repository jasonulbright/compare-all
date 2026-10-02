//! Per column settings and the pairing of left columns with right columns.
//!
//! A comparison column pairs one column of the left file with one column of
//! the right file. The pairing need not be positional: comparison column one
//! may hold left column C against right column E. Either side may be absent,
//! which makes the comparison column unmapped on that side.
//!
//! Each comparison column carries its own handling: the type its cells are
//! read as, whether case and whitespace matter, how far two values may differ
//! before the difference is important, whether the column is a key, and
//! whether the column matters at all.

use crate::detect::{column_type_of_sample, SampledColumnType, TYPE_SAMPLE_BUDGET};
use crate::parse::{FirstLineContains, Table};
use crate::regional::Regional;
use crate::Unknown;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// How a column's cells are read.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ColumnType {
    /// Decide the type from the data in the two files.
    #[default]
    General,
    /// Compare the cells as characters.
    Text,
    /// Compare the cells as numbers, so `1.0` equals `1`.
    Number,
    /// Compare the cells as instants, so the same moment written two ways is
    /// equal.
    DateTime,
    /// A type written by another build, carried through unchanged. It is
    /// handled as [`ColumnType::General`].
    #[serde(untagged)]
    Unknown(serde_json::Value),
}

/// How one comparison column is compared.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ColumnHandling {
    /// The column controls sorting and alignment. With several keys, the
    /// earlier comparison column has the higher precedence.
    pub key: bool,
    /// Take every setting below from the session default instead of from this
    /// column. The key flag is not covered: it stays with the column.
    pub use_default: bool,
    /// How the cells are read.
    pub column_type: ColumnType,
    /// The column does not matter: its differences are unimportant.
    pub unimportant: bool,
    /// Capitalization differences are unimportant in this column.
    pub ignore_case: bool,
    /// Differences in the number of blanks before, after or between words are
    /// unimportant in this column.
    pub ignore_whitespace: bool,
    /// Numbers may differ by this much before the difference is important.
    pub numeric_tolerance: f64,
    /// Instants may differ by this many seconds before the difference is
    /// important.
    pub date_tolerance_seconds: f64,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, skip_serializing_if = "Unknown::is_empty")]
    pub unknown: Unknown,
}

impl Default for ColumnHandling {
    fn default() -> Self {
        Self {
            key: false,
            use_default: true,
            column_type: ColumnType::General,
            unimportant: false,
            ignore_case: false,
            ignore_whitespace: false,
            numeric_tolerance: 0.0,
            date_tolerance_seconds: 0.0,
            unknown: Unknown::new(),
        }
    }
}

impl ColumnHandling {
    /// This column's settings with the session default applied where the
    /// column defers to it. The key flag always comes from the column.
    #[must_use]
    pub fn resolved(&self, default: &Self) -> Self {
        if !self.use_default {
            return self.clone();
        }
        Self {
            key: self.key,
            use_default: true,
            unknown: self.unknown.clone(),
            ..default.clone()
        }
    }
}

/// How columns of the two files are paired.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ColumnAlignment {
    /// Use the file order on both sides.
    #[default]
    Unaligned,
    /// Keep the left file's order and reorder the right side to match names.
    ByLeftName,
    /// Keep the right file's order and reorder the left side to match names.
    ByRightName,
    /// Use the pairs listed in [`SchemaSettings::custom`].
    Custom,
    /// A choice written by another build. It is handled as
    /// [`ColumnAlignment::Unaligned`].
    #[serde(untagged)]
    Unknown(serde_json::Value),
}

/// One hand written pairing of a left column with a right column.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ManualColumnPair {
    /// Zero based left file column, or `None` for a right only column.
    pub left: Option<u32>,
    /// Zero based right file column, or `None` for a left only column.
    pub right: Option<u32>,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, skip_serializing_if = "Unknown::is_empty")]
    pub unknown: Unknown,
}

/// Everything that decides how the columns of two files are compared.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SchemaSettings {
    /// How columns of the two files are paired.
    pub alignment: ColumnAlignment,
    /// Settings every column that defers inherits.
    pub default_handling: ColumnHandling,
    /// Settings of named comparison columns, keyed by comparison column index.
    pub handling: BTreeMap<u32, ColumnHandling>,
    /// Hand written pairs, used when `alignment` is
    /// [`ColumnAlignment::Custom`].
    pub custom: Vec<ManualColumnPair>,
    /// Number and date conventions of the left file.
    pub left_regional: Regional,
    /// Number and date conventions of the right file.
    pub right_regional: Regional,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, skip_serializing_if = "Unknown::is_empty")]
    pub unknown: Unknown,
}

/// How much of a column the type detection read, and what it could not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TypeSample {
    /// Non-empty cells inspected across both sides.
    pub sampled: u64,
    /// Inspected cells that did not read as the leading candidate type.
    pub failed: u64,
}

/// Something about the two files that a caller should see.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SchemaWarning {
    /// The two files disagree on whether line one names the columns. One side
    /// then compares a header row against data.
    FirstLineDisagrees {
        /// What the left file's first line was read as.
        left: FirstLineContains,
        /// What the right file's first line was read as.
        right: FirstLineContains,
    },
    /// Custom alignment leaves one or more source columns outside every pair.
    UnpairedColumns {
        /// Names (or letters when no header exists) not used on the left.
        left: Vec<String>,
        /// Names (or letters when no header exists) not used on the right.
        right: Vec<String>,
    },
}

/// One column of the comparison.
#[derive(Debug, Clone, PartialEq)]
pub struct ComparisonColumn {
    /// Zero based column of the left file, or `None` when unmapped there.
    pub left: Option<u32>,
    /// Zero based column of the right file, or `None` when unmapped there.
    pub right: Option<u32>,
    /// Name shown for the column: the header name when there is one, the
    /// column letter otherwise.
    pub name: String,
    /// Settings after the session default is applied.
    pub handling: ColumnHandling,
    /// The type the cells are actually read as, with
    /// [`ColumnType::General`] already resolved against the data.
    pub effective_type: ColumnType,
    /// What the type detection read. Both counts are zero when the caller
    /// pinned the type instead of letting the data decide.
    pub type_sample: TypeSample,
}

impl ComparisonColumn {
    /// Whether both sides have a column here, so cells can be compared.
    #[must_use]
    pub fn is_mapped(&self) -> bool {
        self.left.is_some() && self.right.is_some()
    }
}

/// The resolved column layout of one comparison.
#[derive(Debug, Clone, PartialEq)]
pub struct Schema {
    /// Comparison columns in display order.
    pub columns: Vec<ComparisonColumn>,
    /// Number and date conventions of the left file.
    pub left_regional: Regional,
    /// Number and date conventions of the right file.
    pub right_regional: Regional,
    /// What the two files disagree about.
    pub warnings: Vec<SchemaWarning>,
}

impl Schema {
    /// Build the column layout of a comparison from two parsed files.
    #[must_use]
    pub fn build(left: &Table, right: &Table, settings: &SchemaSettings) -> Self {
        let pairs = pair_columns(left, right, settings);
        let mut columns = Vec::with_capacity(pairs.len());
        for (index, (left_column, right_column)) in pairs.into_iter().enumerate() {
            let handling = settings.handling.get(&index_u32(index)).map_or_else(
                || settings.default_handling.clone(),
                |own| own.resolved(&settings.default_handling),
            );
            let name = column_name(left, right, left_column, right_column);
            let (effective_type, type_sample) = match handling.column_type {
                ColumnType::Text => (ColumnType::Text, TypeSample::default()),
                ColumnType::Number => (ColumnType::Number, TypeSample::default()),
                ColumnType::DateTime => (ColumnType::DateTime, TypeSample::default()),
                ColumnType::General | ColumnType::Unknown(_) => resolve_type(
                    left,
                    right,
                    left_column,
                    right_column,
                    &settings.left_regional,
                    &settings.right_regional,
                ),
            };
            columns.push(ComparisonColumn {
                left: left_column,
                right: right_column,
                name,
                handling,
                effective_type,
                type_sample,
            });
        }
        let mut warnings = Vec::new();
        if left.first_line_contains() != right.first_line_contains() {
            warnings.push(SchemaWarning::FirstLineDisagrees {
                left: left.first_line_contains().clone(),
                right: right.first_line_contains().clone(),
            });
        }
        if settings.alignment == ColumnAlignment::Custom {
            let left_used: BTreeSet<u32> = settings
                .custom
                .iter()
                .filter_map(|pair| pair.left)
                .collect();
            let right_used: BTreeSet<u32> = settings
                .custom
                .iter()
                .filter_map(|pair| pair.right)
                .collect();
            let left_unpaired = unpaired_column_names(left, &left_used);
            let right_unpaired = unpaired_column_names(right, &right_used);
            if !left_unpaired.is_empty() || !right_unpaired.is_empty() {
                warnings.push(SchemaWarning::UnpairedColumns {
                    left: left_unpaired,
                    right: right_unpaired,
                });
            }
        }
        Self {
            columns,
            left_regional: settings.left_regional.clone(),
            right_regional: settings.right_regional.clone(),
            warnings,
        }
    }

    /// Indices of the key columns, in precedence order.
    #[must_use]
    pub fn key_columns(&self) -> Vec<usize> {
        self.columns
            .iter()
            .enumerate()
            .filter(|(_, column)| column.handling.key && column.is_mapped())
            .map(|(index, _)| index)
            .collect()
    }

    /// Whether any mapped column is marked as a key.
    #[must_use]
    pub fn has_keys(&self) -> bool {
        self.columns
            .iter()
            .any(|column| column.handling.key && column.is_mapped())
    }
}

fn unpaired_column_names(table: &Table, used: &BTreeSet<u32>) -> Vec<String> {
    (0..table.column_count())
        .filter_map(|index| {
            let index = index_u32(index);
            (!used.contains(&index)).then(|| {
                header_name(table, Some(index))
                    .filter(|name| !name.is_empty())
                    .map_or_else(|| column_letter(index), str::to_owned)
            })
        })
        .collect()
}

/// The spreadsheet style letter of a zero based column: `A`, `B`, ... `Z`,
/// `AA`, `AB`, and so on.
#[must_use]
pub fn column_letter(index: u32) -> String {
    let mut letters = Vec::new();
    let mut value = u64::from(index);
    loop {
        let digit = u8::try_from(value % 26).unwrap_or(0);
        letters.push(b'A' + digit);
        if value < 26 {
            break;
        }
        value = value / 26 - 1;
    }
    letters.reverse();
    String::from_utf8(letters).unwrap_or_default()
}

/// Normalized form used when matching column names: character case and
/// whitespace are ignored.
fn match_key(name: &str) -> String {
    name.chars()
        .filter(|ch| !ch.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

fn header_name(table: &Table, column: Option<u32>) -> Option<&str> {
    let column = column?;
    let names = table.header()?;
    names.get(column as usize).map(String::as_str)
}

fn column_name(
    left: &Table,
    right: &Table,
    left_column: Option<u32>,
    right_column: Option<u32>,
) -> String {
    if let Some(name) = header_name(left, left_column).filter(|name| !name.is_empty()) {
        return name.to_owned();
    }
    if let Some(name) = header_name(right, right_column).filter(|name| !name.is_empty()) {
        return name.to_owned();
    }
    match (left_column, right_column) {
        (Some(index), _) | (None, Some(index)) => column_letter(index),
        (None, None) => String::new(),
    }
}

type Pair = (Option<u32>, Option<u32>);

fn pair_columns(left: &Table, right: &Table, settings: &SchemaSettings) -> Vec<Pair> {
    match settings.alignment {
        ColumnAlignment::Custom => settings
            .custom
            .iter()
            .map(|pair| (pair.left, pair.right))
            .collect(),
        ColumnAlignment::ByLeftName => pair_by_name(left, right, false),
        ColumnAlignment::ByRightName => pair_by_name(left, right, true),
        ColumnAlignment::Unaligned | ColumnAlignment::Unknown(_) => {
            pair_positionally(left.column_count(), right.column_count())
        }
    }
}

fn pair_positionally(left_count: usize, right_count: usize) -> Vec<Pair> {
    (0..left_count.max(right_count))
        .map(|index| {
            let column = index_u32(index);
            (
                (index < left_count).then_some(column),
                (index < right_count).then_some(column),
            )
        })
        .collect()
}

/// Pairs by header name, keeping one side's file order and reordering the
/// other. Columns without a name on the leading side, and names the other side
/// does not carry, stay unmapped rather than being paired by position.
fn pair_by_name(left: &Table, right: &Table, right_leads: bool) -> Vec<Pair> {
    let (lead, follow) = if right_leads {
        (right, left)
    } else {
        (left, right)
    };
    let lead_count = lead.column_count();
    let follow_count = follow.column_count();
    let mut follow_by_name: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    for index in 0..follow_count {
        let column = index_u32(index);
        if let Some(name) = header_name(follow, Some(column)) {
            let key = match_key(name);
            if !key.is_empty() {
                follow_by_name.entry(key).or_default().push(column);
            }
        }
    }
    let mut used = vec![false; follow_count];
    let mut pairs: Vec<Pair> = Vec::with_capacity(lead_count.max(follow_count));
    for index in 0..lead_count {
        let column = index_u32(index);
        let matched = header_name(lead, Some(column))
            .map(match_key)
            .filter(|key| !key.is_empty())
            .and_then(|key| {
                follow_by_name.get(&key).and_then(|candidates| {
                    candidates
                        .iter()
                        .copied()
                        .find(|candidate| !used.get(*candidate as usize).copied().unwrap_or(true))
                })
            });
        if let Some(found) = matched {
            if let Some(slot) = used.get_mut(found as usize) {
                *slot = true;
            }
        }
        pairs.push(if right_leads {
            (matched, Some(column))
        } else {
            (Some(column), matched)
        });
    }
    for (index, taken) in used.iter().enumerate() {
        if !*taken {
            let column = index_u32(index);
            pairs.push(if right_leads {
                (Some(column), None)
            } else {
                (None, Some(column))
            });
        }
    }
    pairs
}

fn resolve_type(
    left: &Table,
    right: &Table,
    left_column: Option<u32>,
    right_column: Option<u32>,
    left_regional: &Regional,
    right_regional: &Regional,
) -> (ColumnType, TypeSample) {
    let left_sample = left_column
        .map(|column| column_type_of_sample(left, column, TYPE_SAMPLE_BUDGET, left_regional));
    let right_sample = right_column
        .map(|column| column_type_of_sample(right, column, TYPE_SAMPLE_BUDGET, right_regional));
    let sample = TypeSample {
        sampled: count(left_sample.as_ref(), |s| s.sampled)
            .saturating_add(count(right_sample.as_ref(), |s| s.sampled)),
        failed: count(left_sample.as_ref(), |s| s.failed)
            .saturating_add(count(right_sample.as_ref(), |s| s.failed)),
    };
    let column_type = match (left_sample, right_sample) {
        (Some(a), Some(b)) if a.column_type == b.column_type => a.column_type,
        (Some(a), None) => a.column_type,
        (None, Some(b)) => b.column_type,
        // Sides that disagree fall back to characters, which never claims two
        // differently written values are equal.
        _ => ColumnType::Text,
    };
    (column_type, sample)
}

fn count(sample: Option<&SampledColumnType>, field: fn(&SampledColumnType) -> u64) -> u64 {
    sample.map_or(0, field)
}

#[inline]
fn index_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::parse::{parse, FirstLineContains, ParseOptions};

    fn headed(text: &str) -> Table {
        let mut options = ParseOptions::comma_separated();
        options.first_line_contains = FirstLineContains::ColumnNames;
        parse(text, &options).unwrap()
    }

    fn headless(text: &str) -> Table {
        let mut options = ParseOptions::comma_separated();
        options.first_line_contains = FirstLineContains::CellData;
        parse(text, &options).unwrap()
    }

    #[test]
    fn column_letters_follow_the_spreadsheet_sequence() {
        assert_eq!(column_letter(0), "A");
        assert_eq!(column_letter(25), "Z");
        assert_eq!(column_letter(26), "AA");
        assert_eq!(column_letter(27), "AB");
        assert_eq!(column_letter(701), "ZZ");
        assert_eq!(column_letter(702), "AAA");
    }

    #[test]
    fn unaligned_pairs_by_position_and_keeps_extra_columns() {
        let left = headless("a,b,c");
        let right = headless("d,e");
        let schema = Schema::build(&left, &right, &SchemaSettings::default());
        assert_eq!(schema.columns.len(), 3);
        assert_eq!(schema.columns[2].right, None);
        assert!(!schema.columns[2].is_mapped());
        assert_eq!(schema.columns[2].name, "C");
    }

    #[test]
    fn name_alignment_ignores_case_and_whitespace() {
        let left = headed("Id, Full Name ,City\n1,Ann,Rome\n2,Bob,Oslo\n");
        let right = headed("city,ID,fullname\nRome,1,Ann\nOslo,2,Bob\n");
        let settings = SchemaSettings {
            alignment: ColumnAlignment::ByLeftName,
            ..SchemaSettings::default()
        };
        let schema = Schema::build(&left, &right, &settings);
        assert_eq!(schema.columns.len(), 3);
        assert_eq!(schema.columns[0].right, Some(1));
        assert_eq!(schema.columns[1].right, Some(2));
        assert_eq!(schema.columns[2].right, Some(0));
    }

    #[test]
    fn right_name_alignment_keeps_the_right_order() {
        let left = headed("b,a\n1,2\n3,4\n");
        let right = headed("a,b\n2,1\n4,3\n");
        let settings = SchemaSettings {
            alignment: ColumnAlignment::ByRightName,
            ..SchemaSettings::default()
        };
        let schema = Schema::build(&left, &right, &settings);
        assert_eq!(schema.columns[0].right, Some(0));
        assert_eq!(schema.columns[0].left, Some(1));
        assert_eq!(schema.columns[1].left, Some(0));
    }

    #[test]
    fn unmatched_names_stay_unmapped_and_are_listed() {
        let left = headed("id,only_left\n1,x\n2,y\n");
        let right = headed("id,only_right\n1,p\n2,q\n");
        let settings = SchemaSettings {
            alignment: ColumnAlignment::ByLeftName,
            ..SchemaSettings::default()
        };
        let schema = Schema::build(&left, &right, &settings);
        assert_eq!(schema.columns.len(), 3);
        assert_eq!(schema.columns[1].right, None);
        assert_eq!(schema.columns[2].left, None);
        assert_eq!(schema.columns[2].name, "only_right");
    }

    #[test]
    fn custom_alignment_reports_every_source_column_not_in_a_pair() {
        let left = headed("id,name\n1,Ann\n");
        let right = headed("id,name,city\n1,Ann,Rome\n");
        let settings = SchemaSettings {
            alignment: ColumnAlignment::Custom,
            custom: vec![ManualColumnPair {
                left: Some(0),
                right: Some(0),
                ..ManualColumnPair::default()
            }],
            ..SchemaSettings::default()
        };
        let schema = Schema::build(&left, &right, &settings);
        assert!(schema.warnings.contains(&SchemaWarning::UnpairedColumns {
            left: vec!["name".to_owned()],
            right: vec!["name".to_owned(), "city".to_owned()],
        }));
    }

    #[test]
    fn a_manual_remap_is_used_verbatim() {
        let left = headless("a,b,c");
        let right = headless("d,e,f");
        let settings = SchemaSettings {
            alignment: ColumnAlignment::Custom,
            custom: vec![
                ManualColumnPair {
                    left: Some(2),
                    right: Some(0),
                    unknown: Unknown::new(),
                },
                ManualColumnPair {
                    left: Some(0),
                    right: None,
                    unknown: Unknown::new(),
                },
            ],
            ..SchemaSettings::default()
        };
        let schema = Schema::build(&left, &right, &settings);
        assert_eq!(schema.columns.len(), 2);
        assert_eq!(schema.columns[0].left, Some(2));
        assert_eq!(schema.columns[0].right, Some(0));
        assert!(!schema.columns[1].is_mapped());
    }

    #[test]
    fn a_column_inherits_the_default_but_keeps_its_key_flag() {
        let default = ColumnHandling {
            ignore_case: true,
            numeric_tolerance: 0.5,
            ..ColumnHandling::default()
        };
        let own = ColumnHandling {
            key: true,
            use_default: true,
            ..ColumnHandling::default()
        };
        let resolved = own.resolved(&default);
        assert!(resolved.key);
        assert!(resolved.ignore_case);
        assert!((resolved.numeric_tolerance - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn a_column_that_does_not_defer_keeps_its_own_settings() {
        let default = ColumnHandling {
            ignore_case: true,
            ..ColumnHandling::default()
        };
        let own = ColumnHandling {
            use_default: false,
            ignore_case: false,
            ..ColumnHandling::default()
        };
        assert!(!own.resolved(&default).ignore_case);
    }

    #[test]
    fn general_columns_resolve_against_the_data() {
        let left = headed("id,name,when\n1,Ann,01/02/2020\n2,Bob,03/04/2020\n");
        let right = headed("id,name,when\n1,Ann,01/02/2020\n2,Bob,03/04/2020\n");
        let schema = Schema::build(&left, &right, &SchemaSettings::default());
        assert_eq!(schema.columns[0].effective_type, ColumnType::Number);
        assert_eq!(schema.columns[1].effective_type, ColumnType::Text);
        assert_eq!(schema.columns[2].effective_type, ColumnType::DateTime);
    }

    #[test]
    fn key_columns_report_in_precedence_order() {
        let left = headless("a,b,c");
        let right = headless("a,b,c");
        let mut handling = BTreeMap::new();
        handling.insert(
            2,
            ColumnHandling {
                key: true,
                ..ColumnHandling::default()
            },
        );
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
        assert!(schema.has_keys());
        assert_eq!(schema.key_columns(), vec![0, 2]);
    }

    #[test]
    fn sides_that_disagree_about_line_one_are_reported() {
        let left = headed("id,name\n1,Ann\n2,Bob\n");
        let right = headless("id,name\n1,Ann\n2,Bob\n");
        let schema = Schema::build(&left, &right, &SchemaSettings::default());
        assert_eq!(
            schema.warnings,
            vec![SchemaWarning::FirstLineDisagrees {
                left: FirstLineContains::ColumnNames,
                right: FirstLineContains::CellData,
            }]
        );

        let agreeing = Schema::build(&left, &left, &SchemaSettings::default());
        assert!(agreeing.warnings.is_empty());
    }

    #[test]
    fn a_resolved_column_records_what_the_sample_read() {
        let left = headed("id,name\n1,Ann\n2,Bob\n");
        let right = headed("id,name\n1,Ann\n2,Bob\n");
        let schema = Schema::build(&left, &right, &SchemaSettings::default());
        assert_eq!(schema.columns[0].type_sample.sampled, 4);
        assert_eq!(schema.columns[0].type_sample.failed, 0);

        let mixed_left = headed("v\n1\n2\nx\n");
        let mixed_right = headed("v\n1\n2\n3\n");
        let schema = Schema::build(&mixed_left, &mixed_right, &SchemaSettings::default());
        assert_eq!(schema.columns[0].effective_type, ColumnType::Text);
        assert_eq!(schema.columns[0].type_sample.sampled, 6);
        assert_eq!(schema.columns[0].type_sample.failed, 1);
    }

    #[test]
    fn a_pinned_type_records_no_sample() {
        let left = headed("v\n1\n2\n");
        let right = headed("v\n1\n2\n");
        let mut handling = BTreeMap::new();
        handling.insert(
            0,
            ColumnHandling {
                use_default: false,
                column_type: ColumnType::Text,
                ..ColumnHandling::default()
            },
        );
        let settings = SchemaSettings {
            handling,
            ..SchemaSettings::default()
        };
        let schema = Schema::build(&left, &right, &settings);
        assert_eq!(schema.columns[0].effective_type, ColumnType::Text);
        assert_eq!(schema.columns[0].type_sample, TypeSample::default());
    }

    #[test]
    fn settings_round_trip_with_unknown_fields() {
        let text = r#"{"alignment":"diagonal","defaultHandling":{"columnType":"currency","futureFlag":true},"leftRegional":{"decimalSeparator":","},"futureTop":[1]}"#;
        let settings: SchemaSettings = serde_json::from_str(text).unwrap();
        assert!(matches!(settings.alignment, ColumnAlignment::Unknown(_)));
        assert!(matches!(
            settings.default_handling.column_type,
            ColumnType::Unknown(_)
        ));
        let back = serde_json::to_string(&settings).unwrap();
        let again: SchemaSettings = serde_json::from_str(&back).unwrap();
        assert_eq!(again, settings);
        assert!(back.contains("\"diagonal\""));
        assert!(back.contains("\"currency\""));
        assert!(back.contains("futureTop"));
    }

    #[test]
    fn an_unknown_alignment_pairs_by_position() {
        let left = headless("a,b");
        let right = headless("c,d");
        let settings = SchemaSettings {
            alignment: ColumnAlignment::Unknown(serde_json::Value::from("diagonal")),
            ..SchemaSettings::default()
        };
        let schema = Schema::build(&left, &right, &settings);
        assert_eq!(schema.columns.len(), 2);
        assert!(schema.columns.iter().all(ComparisonColumn::is_mapped));
    }
}
