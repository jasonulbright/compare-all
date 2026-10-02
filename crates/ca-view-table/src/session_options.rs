//! The one conversion from stored settings to the options the table engine
//! takes.

use crate::jobs::{FormatSettings as SideFormat, TableSettings};
use ca_session::settings::common::{
    AlignmentAlgorithm, EncodingChoice, FileFormatChoice, FormatSettings,
};
use ca_session::settings::table::{
    ColumnHandling as StoredHandling, ColumnSettings, ColumnType as StoredType, RowSettings,
    TableCompareSettings, TablePairing,
};
use ca_table::align::{RowAlignOptions, RowAlignmentMode};
use ca_table::parse::ParseOptions;
use ca_table::schema::{ColumnAlignment, ColumnHandling, ColumnType, ManualColumnPair};

/// The field syntaxes a stored format name can pin, by the name it is stored
/// under.
///
/// A name outside this list leaves the syntax to detection, so a document
/// naming a format from a format registry this build does not carry still
/// opens.
const NAMED_FORMATS: &[(&str, char)] = &[
    ("comma-separated", ','),
    ("tab-separated", '\t'),
    ("semicolon-separated", ';'),
    ("bar-separated", '|'),
    ("space-separated", ' '),
];

/// The row pairing algorithm an alignment setting names.
#[must_use]
pub fn row_mode(algorithm: &AlignmentAlgorithm) -> RowAlignmentMode {
    match algorithm {
        AlignmentAlgorithm::Unaligned => RowAlignmentMode::Unaligned,
        AlignmentAlgorithm::MyersOnd => RowAlignmentMode::Myers,
        AlignmentAlgorithm::Patience => RowAlignmentMode::Patience,
        _ => RowAlignmentMode::Standard,
    }
}

/// The column pairing rule a stored pairing names.
#[must_use]
pub fn column_alignment(pairing: &TablePairing) -> ColumnAlignment {
    match pairing {
        TablePairing::ByLeftName { .. } => ColumnAlignment::ByLeftName,
        TablePairing::ByRightName { .. } => ColumnAlignment::ByRightName,
        TablePairing::Custom { .. } => ColumnAlignment::Custom,
        _ => ColumnAlignment::Unaligned,
    }
}

/// How a column's cells are read.
///
/// The stored numeric and date treatments both reach the engine's own; a
/// treatment this build does not understand is read from the data.
#[must_use]
pub fn column_type(stored: &StoredType) -> ColumnType {
    match stored {
        StoredType::Text => ColumnType::Text,
        StoredType::Numeric => ColumnType::Number,
        StoredType::Date => ColumnType::DateTime,
        _ => ColumnType::General,
    }
}

/// The engine treatment a stored column handling states.
#[must_use]
pub fn handling_from(stored: &StoredHandling) -> ColumnHandling {
    ColumnHandling {
        key: stored.key,
        use_default: stored.use_default,
        column_type: column_type(&stored.column_type),
        unimportant: stored.unimportant,
        ignore_case: stored.ignore_character_case,
        ignore_whitespace: stored.ignore_whitespace,
        numeric_tolerance: stored.numeric_tolerance,
        date_tolerance_seconds: f64::from(stored.date_tolerance_seconds),
        ..ColumnHandling::default()
    }
}

/// The row pairing a stored row group states.
#[must_use]
pub fn row_options(rows: &RowSettings) -> RowAlignOptions {
    RowAlignOptions {
        mode: row_mode(&rows.algorithm),
        never_align_differences: rows.never_align_differences,
        // Zero states no bound rather than a bound of nothing.
        skew_tolerance: (rows.skew_tolerance > 0).then_some(rows.skew_tolerance),
        use_closeness_matching: rows.use_closeness_matching,
        sort_rows_before_alignment: rows.sort_before_alignment,
        ..RowAlignOptions::default()
    }
}

/// The hand written column pairs a stored pairing carries.
///
/// A pairing rule other than the explicit one carries no pairs, so the list is
/// empty and the engine falls back to the named rule.
#[must_use]
pub fn manual_pairs(pairing: &TablePairing) -> Vec<ManualColumnPair> {
    let TablePairing::Custom { pairs, .. } = pairing else {
        return Vec::new();
    };
    pairs
        .iter()
        .map(|(left, right)| ManualColumnPair {
            left: *left,
            right: *right,
            ..ManualColumnPair::default()
        })
        .collect()
}

/// The parse options a stored format choice pins, or `None` when the syntax is
/// read from the data.
#[must_use]
pub fn parse_options(choice: &FileFormatChoice) -> Option<ParseOptions> {
    let FileFormatChoice::Named { name, .. } = choice else {
        return None;
    };
    let wanted = name.trim().to_ascii_lowercase();
    NAMED_FORMATS
        .iter()
        .find(|(known, _)| *known == wanted)
        .map(|(_, delimiter)| ParseOptions {
            syntax: ca_table::parse::FieldSyntax::delimited([*delimiter]),
            ..ParseOptions::default()
        })
}

/// The encoding a stored choice pins, or `None` when the encoding is detected.
///
/// A label this build has no encoding for leaves detection in place rather
/// than refusing the session.
#[must_use]
pub fn forced_encoding(choice: &EncodingChoice) -> Option<ca_text::TextEncoding> {
    let EncodingChoice::Named { name, .. } = choice else {
        return None;
    };
    ca_text::TextEncoding::from_label(name.trim())
}

/// How one side is read, from the stored format group.
#[must_use]
pub fn side_format(
    format: &FileFormatChoice,
    encoding: &EncodingChoice,
    base: &SideFormat,
) -> SideFormat {
    SideFormat {
        parse: parse_options(format).or_else(|| base.parse.clone()),
        encoding: forced_encoding(encoding),
    }
}

/// The options a stored table session states, over the view's current ones.
///
/// The view holds the per-side parse choices the settings do not pin, so a
/// stored format naming no syntax leaves what the view has in place.
#[must_use]
pub fn options_over(settings: &TableCompareSettings, base: &TableSettings) -> TableSettings {
    let mut built = base.clone();
    built.schema.alignment = column_alignment(&settings.columns.pairing);
    built.schema.default_handling = handling_from(&settings.columns.default_handling);
    built.schema.handling = per_column(&settings.columns);
    built.schema.custom = manual_pairs(&settings.columns.pairing);
    built.align = row_options(&settings.rows);
    built.left_format = side_format(
        &settings.format.left_format,
        &settings.format.left_encoding,
        &base.left_format,
    );
    built.right_format = side_format(
        &settings.format.right_format,
        &settings.format.right_encoding,
        &base.right_format,
    );
    built
}

/// The stored format group a pair of side formats states.
///
/// Only a syntax this build names can be written back; a side the view pinned
/// by hand is left as detection, which is what the stored group means.
#[must_use]
pub fn format_settings(left: &SideFormat, right: &SideFormat) -> FormatSettings {
    FormatSettings {
        left_format: format_choice(left),
        right_format: format_choice(right),
        left_encoding: encoding_choice(left),
        right_encoding: encoding_choice(right),
        ..FormatSettings::default()
    }
}

fn format_choice(side: &SideFormat) -> FileFormatChoice {
    let Some(options) = &side.parse else {
        return FileFormatChoice::detected();
    };
    let ca_table::parse::FieldSyntax::Delimited { delimiters, .. } = &options.syntax else {
        return FileFormatChoice::detected();
    };
    let [only] = delimiters[..] else {
        return FileFormatChoice::detected();
    };
    NAMED_FORMATS
        .iter()
        .find(|(_, delimiter)| *delimiter == only)
        .map_or_else(FileFormatChoice::detected, |(name, _)| {
            FileFormatChoice::Named {
                name: (*name).to_owned(),
                unknown: std::collections::BTreeMap::new(),
            }
        })
}

fn encoding_choice(side: &SideFormat) -> EncodingChoice {
    side.encoding
        .map_or_else(EncodingChoice::from_format, |encoding| {
            EncodingChoice::Named {
                name: encoding.label().to_owned(),
                unknown: std::collections::BTreeMap::new(),
            }
        })
}

/// The per-column treatments a stored column group states, keyed by the
/// comparison column each one names.
///
/// A treatment naming no column states the inherited one instead and is left
/// out of the map.
#[must_use]
pub fn per_column(columns: &ColumnSettings) -> std::collections::BTreeMap<u32, ColumnHandling> {
    columns
        .per_column
        .iter()
        .filter_map(|stored| Some((stored.column?, handling_from(stored))))
        .collect()
}

/// The options of a whole table comparison session.
#[must_use]
pub fn options_of(settings: &TableCompareSettings) -> TableSettings {
    options_over(settings, &TableSettings::default())
}

/// The stored pairing rule an engine alignment and its pair list state.
#[must_use]
pub fn stored_pairing(alignment: &ColumnAlignment, custom: &[ManualColumnPair]) -> TablePairing {
    match alignment {
        ColumnAlignment::ByLeftName => TablePairing::by_left_name(),
        ColumnAlignment::ByRightName => TablePairing::by_right_name(),
        ColumnAlignment::Custom => TablePairing::Custom {
            pairs: custom.iter().map(|pair| (pair.left, pair.right)).collect(),
            unknown: std::collections::BTreeMap::new(),
        },
        _ => TablePairing::unaligned(),
    }
}

/// The stored treatment an engine column treatment states.
#[must_use]
pub fn stored_handling(column: Option<u32>, handling: &ColumnHandling) -> StoredHandling {
    StoredHandling {
        column,
        key: handling.key,
        use_default: handling.use_default,
        column_type: stored_type(&handling.column_type),
        unimportant: handling.unimportant,
        ignore_character_case: handling.ignore_case,
        ignore_whitespace: handling.ignore_whitespace,
        numeric_tolerance: handling.numeric_tolerance,
        // The stored tolerance counts whole seconds, so a fraction the engine
        // holds is not representable and rounds to the nearest second.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        date_tolerance_seconds: handling.date_tolerance_seconds.max(0.0).round() as u32,
        unknown: std::collections::BTreeMap::new(),
    }
}

/// The stored name of a column treatment.
#[must_use]
pub fn stored_type(column_type: &ColumnType) -> StoredType {
    match column_type {
        ColumnType::Text => StoredType::Text,
        ColumnType::Number => StoredType::Numeric,
        ColumnType::DateTime => StoredType::Date,
        _ => StoredType::General,
    }
}

/// The stored algorithm name of a row pairing pass.
#[must_use]
pub fn stored_algorithm(mode: &RowAlignmentMode) -> AlignmentAlgorithm {
    match mode {
        RowAlignmentMode::Unaligned => AlignmentAlgorithm::Unaligned,
        RowAlignmentMode::Myers => AlignmentAlgorithm::MyersOnd,
        RowAlignmentMode::Patience => AlignmentAlgorithm::Patience,
        _ => AlignmentAlgorithm::Standard,
    }
}

/// The whole stored session the engine options state.
///
/// This is the inverse of [`options_over`] over every value the stored
/// settings name, so a control that edits the engine options and the settings
/// dialog read one state.
#[must_use]
pub fn stored_from(settings: &TableSettings) -> TableCompareSettings {
    TableCompareSettings {
        format: format_settings(&settings.left_format, &settings.right_format),
        columns: ca_session::settings::table::ColumnSettings {
            pairing: stored_pairing(&settings.schema.alignment, &settings.schema.custom),
            default_handling: stored_handling(None, &settings.schema.default_handling),
            per_column: settings
                .schema
                .handling
                .iter()
                .map(|(column, handling)| stored_handling(Some(*column), handling))
                .collect(),
            ..ColumnSettings::default()
        },
        rows: RowSettings {
            algorithm: stored_algorithm(&settings.align.mode),
            never_align_differences: settings.align.never_align_differences,
            // Zero states no bound, which is what no skew tolerance means.
            skew_tolerance: settings.align.skew_tolerance.unwrap_or(0),
            use_closeness_matching: settings.align.use_closeness_matching,
            sort_before_alignment: settings.align.sort_rows_before_alignment,
            ..RowSettings::default()
        },
        ..TableCompareSettings::default()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
mod tests {
    use super::{column_alignment, column_type, options_of, row_mode};
    use ca_session::settings::common::AlignmentAlgorithm;
    use ca_session::settings::table::{
        ColumnHandling as StoredHandling, ColumnType as StoredType, TableCompareSettings,
        TablePairing,
    };
    use ca_table::align::RowAlignmentMode;
    use ca_table::schema::{ColumnAlignment, ColumnType};

    #[test]
    fn every_row_algorithm_reaches_its_own_pass() {
        for (algorithm, mode) in [
            (AlignmentAlgorithm::Unaligned, RowAlignmentMode::Unaligned),
            (AlignmentAlgorithm::Standard, RowAlignmentMode::Standard),
            (AlignmentAlgorithm::MyersOnd, RowAlignmentMode::Myers),
            (AlignmentAlgorithm::Patience, RowAlignmentMode::Patience),
        ] {
            assert_eq!(row_mode(&algorithm), mode);
        }
        assert_eq!(
            row_mode(&AlignmentAlgorithm::Unknown(serde_json::json!("future"))),
            RowAlignmentMode::Standard
        );
    }

    #[test]
    fn every_pairing_rule_reaches_its_own_alignment() {
        assert_eq!(
            column_alignment(&TablePairing::unaligned()),
            ColumnAlignment::Unaligned
        );
        assert_eq!(
            column_alignment(&TablePairing::by_left_name()),
            ColumnAlignment::ByLeftName
        );
        assert_eq!(
            column_alignment(&TablePairing::by_right_name()),
            ColumnAlignment::ByRightName
        );
    }

    #[test]
    fn every_column_treatment_reaches_its_own_type() {
        assert_eq!(column_type(&StoredType::General), ColumnType::General);
        assert_eq!(column_type(&StoredType::Text), ColumnType::Text);
        assert_eq!(column_type(&StoredType::Numeric), ColumnType::Number);
        assert_eq!(column_type(&StoredType::Date), ColumnType::DateTime);
    }

    #[test]
    fn the_row_group_reaches_the_row_options() {
        let mut settings = TableCompareSettings::default();
        settings.rows.skew_tolerance = 21;
        settings.rows.never_align_differences = true;
        settings.rows.use_closeness_matching = false;
        settings.rows.sort_before_alignment = true;
        let align = options_of(&settings).align;
        assert_eq!(align.skew_tolerance, Some(21));
        assert!(align.never_align_differences);
        assert!(!align.use_closeness_matching);
        assert!(align.sort_rows_before_alignment);
    }

    #[test]
    fn the_inherited_column_treatment_reaches_the_schema() {
        let mut settings = TableCompareSettings::default();
        settings.columns.default_handling.ignore_character_case = true;
        settings.columns.default_handling.ignore_whitespace = true;
        settings.columns.default_handling.unimportant = true;
        settings.columns.default_handling.numeric_tolerance = 0.5;
        settings.columns.default_handling.date_tolerance_seconds = 60;
        let handling = options_of(&settings).schema.default_handling;
        assert!(handling.ignore_case);
        assert!(handling.ignore_whitespace);
        assert!(handling.unimportant);
        assert!((handling.numeric_tolerance - 0.5).abs() < f64::EPSILON);
        assert!((handling.date_tolerance_seconds - 60.0).abs() < f64::EPSILON);
    }

    #[test]
    fn a_column_treatment_reaches_the_column_it_names() {
        let mut settings = TableCompareSettings::default();
        settings.columns.per_column = vec![
            StoredHandling {
                column: Some(2),
                key: true,
                use_default: false,
                ..StoredHandling::default()
            },
            StoredHandling {
                column: None,
                ..StoredHandling::default()
            },
        ];
        let handling = options_of(&settings).schema.handling;
        assert_eq!(
            handling.len(),
            1,
            "a treatment naming no column is left out"
        );
        assert!(handling.get(&2).unwrap().key);
    }

    #[test]
    fn an_explicit_pair_list_reaches_the_engine() {
        let mut settings = TableCompareSettings::default();
        settings.columns.pairing = TablePairing::Custom {
            pairs: vec![(Some(2), Some(0)), (Some(1), None), (None, Some(3))],
            unknown: std::collections::BTreeMap::new(),
        };
        let schema = options_of(&settings).schema;
        assert_eq!(schema.alignment, ColumnAlignment::Custom);
        assert_eq!(schema.custom.len(), 3);
        assert_eq!(schema.custom[0].left, Some(2));
        assert_eq!(schema.custom[0].right, Some(0));
        assert_eq!(schema.custom[2].left, None);
        assert_eq!(schema.custom[2].right, Some(3));
    }

    #[test]
    fn a_named_rule_carries_no_pair_list() {
        let mut settings = TableCompareSettings::default();
        settings.columns.pairing = TablePairing::by_left_name();
        assert!(options_of(&settings).schema.custom.is_empty());
    }

    #[test]
    fn a_named_format_pins_the_field_syntax() {
        use ca_session::settings::common::FileFormatChoice;
        let mut settings = TableCompareSettings::default();
        settings.format.left_format = FileFormatChoice::Named {
            name: "tab-separated".to_owned(),
            unknown: std::collections::BTreeMap::new(),
        };
        let built = options_of(&settings);
        let Some(options) = &built.left_format.parse else {
            panic!("a named format pins the syntax");
        };
        assert_eq!(
            options.syntax,
            ca_table::parse::FieldSyntax::delimited(['\t'])
        );
        assert!(
            built.right_format.parse.is_none(),
            "the side left at detection is not pinned"
        );
    }

    #[test]
    fn a_format_name_this_build_does_not_know_leaves_detection_in_place() {
        use ca_session::settings::common::FileFormatChoice;
        let mut settings = TableCompareSettings::default();
        settings.format.left_format = FileFormatChoice::Named {
            name: "a format from another build".to_owned(),
            unknown: std::collections::BTreeMap::new(),
        };
        assert!(options_of(&settings).left_format.parse.is_none());
    }

    #[test]
    fn a_named_encoding_reaches_the_side_it_names() {
        use ca_session::settings::common::EncodingChoice;
        let mut settings = TableCompareSettings::default();
        settings.format.right_encoding = EncodingChoice::Named {
            name: "windows-1252".to_owned(),
            unknown: std::collections::BTreeMap::new(),
        };
        let built = options_of(&settings);
        assert!(built.right_format.encoding.is_some());
        assert!(built.left_format.encoding.is_none());
        settings.format.right_encoding = EncodingChoice::Named {
            name: "an encoding from another build".to_owned(),
            unknown: std::collections::BTreeMap::new(),
        };
        assert!(options_of(&settings).right_format.encoding.is_none());
    }

    #[test]
    fn every_engine_value_returns_to_the_settings_it_came_from() {
        use ca_session::settings::common::FileFormatChoice;
        let mut settings = TableCompareSettings::default();
        settings.columns.pairing = TablePairing::Custom {
            pairs: vec![(Some(1), None)],
            unknown: std::collections::BTreeMap::new(),
        };
        settings.columns.default_handling.ignore_whitespace = true;
        settings.columns.default_handling.numeric_tolerance = 0.25;
        settings.columns.default_handling.date_tolerance_seconds = 45;
        settings.columns.per_column = vec![StoredHandling {
            column: Some(3),
            key: true,
            use_default: false,
            column_type: StoredType::Numeric,
            ..StoredHandling::default()
        }];
        settings.rows.algorithm = AlignmentAlgorithm::Patience;
        settings.rows.skew_tolerance = 13;
        settings.rows.sort_before_alignment = true;
        settings.format.left_format = FileFormatChoice::Named {
            name: "bar-separated".to_owned(),
            unknown: std::collections::BTreeMap::new(),
        };

        let back = super::stored_from(&options_of(&settings));
        assert_eq!(back.columns.pairing, settings.columns.pairing);
        assert!(back.columns.default_handling.ignore_whitespace);
        assert_eq!(back.columns.default_handling.date_tolerance_seconds, 45);
        assert_eq!(back.columns.per_column.len(), 1);
        assert_eq!(back.columns.per_column[0].column, Some(3));
        assert!(back.columns.per_column[0].key);
        assert_eq!(back.columns.per_column[0].column_type, StoredType::Numeric);
        assert_eq!(back.rows.algorithm, AlignmentAlgorithm::Patience);
        assert_eq!(back.rows.skew_tolerance, 13);
        assert!(back.rows.sort_before_alignment);
        assert_eq!(back.format.left_format, settings.format.left_format);
    }

    #[test]
    fn a_skew_of_zero_states_no_bound() {
        let mut settings = TableCompareSettings::default();
        settings.rows.skew_tolerance = 0;
        assert_eq!(options_of(&settings).align.skew_tolerance, None);
    }
}
