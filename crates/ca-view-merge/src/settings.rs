//! The one conversion from stored settings to the options the merge engine
//! takes.

use ca_diff::lines::{AlignmentMode, AlignmentOptions, LineCompareOptions};
use ca_diff::merge3::{ConflictScope, MergeOptions};
use ca_diff::RuleSet;
use ca_session::settings::common::AlignmentAlgorithm;
use ca_session::settings::text::{TextConflictSettings, TextImportanceSettings, TextMergeSettings};
use ca_text::{DecodeOptions, TextEncoding};

/// When two opposing changes are one conflict.
///
/// Asking for changed lines only states a separation of nothing, so the two
/// stored fields never disagree about the same run of lines.
#[must_use]
pub fn conflict_scope(conflicts: &TextConflictSettings) -> ConflictScope {
    if conflicts.same_lines_only {
        return ConflictScope::ChangedLinesOnly;
    }
    ConflictScope::Separation {
        lines: conflicts.separation_lines,
    }
}

/// How one side's bytes are decoded under a stored encoding choice.
///
/// A name no encoding this build has carries leaves detection in place, so a
/// session written by another build still opens.
#[must_use]
pub fn decode_from(choice: &ca_session::settings::common::EncodingChoice) -> DecodeOptions {
    let forced = match choice {
        ca_session::settings::common::EncodingChoice::Named { name, .. } => {
            TextEncoding::from_label(name)
        }
        _ => None,
    };
    DecodeOptions {
        forced,
        ..DecodeOptions::default()
    }
}

/// The pairing algorithm an alignment setting names.
///
/// An algorithm this build does not understand falls back to the standard
/// pass rather than refusing the session.
#[must_use]
pub fn alignment_mode(algorithm: &AlignmentAlgorithm) -> AlignmentMode {
    match algorithm {
        AlignmentAlgorithm::Unaligned => AlignmentMode::Unaligned,
        AlignmentAlgorithm::MyersOnd => AlignmentMode::Myers,
        AlignmentAlgorithm::Patience => AlignmentMode::Patience,
        _ => AlignmentMode::Standard,
    }
}

/// Everything a merge runs under, derived from stored settings.
#[derive(Debug, Clone)]
pub struct EngineOptions {
    /// What the line pass disregards, how lines are paired, and when two
    /// opposing changes are one conflict.
    pub merge: MergeOptions,
    /// Which differences count, for Ignore Unimportant Differences.
    pub rules: RuleSet,
}

/// The importance rules a stored importance group states.
///
/// A merge reads no grammar, so every non-whitespace run is text no element
/// claims and the grammar element entries have nothing to act on.
#[must_use]
pub fn rules_from(importance: &TextImportanceSettings) -> RuleSet {
    let mut rules = RuleSet::all_important();
    rules.leading_whitespace_important = importance.leading_whitespace_important;
    rules.embedded_whitespace_important = importance.embedded_whitespace_important;
    rules.trailing_whitespace_important = importance.trailing_whitespace_important;
    rules.everything_else_important = importance.everything_else_important;
    rules.match_character_case = importance.character_case_important;
    rules.orphan_lines_always_important = importance.orphan_lines_always_important;
    rules
}

/// The options a merge session's settings state.
///
/// The importance group decides which differences count, not which lines
/// differ: only the line ending test reaches the line pass, and the rest reach
/// the classification that Ignore Unimportant Differences reads.
#[must_use]
pub fn options_from(settings: &TextMergeSettings) -> EngineOptions {
    EngineOptions {
        merge: merge_options(settings),
        rules: rules_from(&settings.importance),
    }
}

fn merge_options(settings: &TextMergeSettings) -> MergeOptions {
    let alignment = &settings.alignment;
    MergeOptions {
        compare: LineCompareOptions {
            ignore_line_endings: !settings.importance.compare_line_endings,
            alignment: AlignmentOptions {
                mode: alignment_mode(&alignment.algorithm),
                // Zero states no bound rather than a bound of nothing.
                skew_tolerance: (alignment.skew_tolerance > 0).then_some(alignment.skew_tolerance),
                never_align_differences: alignment.never_align_differences,
                use_closeness_matching: alignment.use_closeness_matching,
            },
            ..LineCompareOptions::default()
        },
        conflict_scope: conflict_scope(&settings.conflicts),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
mod tests {
    use super::{alignment_mode, options_from};
    use ca_diff::lines::AlignmentMode;
    use ca_session::settings::common::AlignmentAlgorithm;
    use ca_session::settings::text::TextMergeSettings;

    #[test]
    fn every_algorithm_reaches_its_own_pass() {
        for (algorithm, mode) in [
            (AlignmentAlgorithm::Unaligned, AlignmentMode::Unaligned),
            (AlignmentAlgorithm::Standard, AlignmentMode::Standard),
            (AlignmentAlgorithm::MyersOnd, AlignmentMode::Myers),
            (AlignmentAlgorithm::Patience, AlignmentMode::Patience),
        ] {
            assert_eq!(alignment_mode(&algorithm), mode);
        }
        assert_eq!(
            alignment_mode(&AlignmentAlgorithm::Unknown(serde_json::json!("future"))),
            AlignmentMode::Standard
        );
    }

    #[test]
    fn the_alignment_group_reaches_the_merge_options() {
        let mut settings = TextMergeSettings::default();
        settings.alignment.skew_tolerance = 11;
        settings.alignment.never_align_differences = true;
        settings.alignment.use_closeness_matching = false;
        settings.alignment.algorithm = AlignmentAlgorithm::Patience;
        let alignment = options_from(&settings).merge.compare.alignment;
        assert_eq!(alignment.skew_tolerance, Some(11));
        assert!(alignment.never_align_differences);
        assert!(!alignment.use_closeness_matching);
        assert_eq!(alignment.mode, AlignmentMode::Patience);
    }

    #[test]
    fn comparing_line_endings_reaches_the_line_pass() {
        let mut settings = TextMergeSettings::default();
        assert!(options_from(&settings).merge.compare.ignore_line_endings);
        settings.importance.compare_line_endings = true;
        assert!(!options_from(&settings).merge.compare.ignore_line_endings);
    }

    #[test]
    fn a_skew_of_zero_states_no_bound() {
        let mut settings = TextMergeSettings::default();
        settings.alignment.skew_tolerance = 0;
        assert_eq!(
            options_from(&settings)
                .merge
                .compare
                .alignment
                .skew_tolerance,
            None
        );
    }

    /// Each importance flag reaches exactly the rule it names.
    #[test]
    fn every_importance_flag_reaches_its_own_rule() {
        type Edit = fn(&mut ca_session::settings::text::TextImportanceSettings);
        type Read = fn(&ca_diff::RuleSet) -> bool;
        let table: [(Edit, Read); 6] = [
            (
                |importance| importance.leading_whitespace_important = false,
                |rules| !rules.leading_whitespace_important,
            ),
            (
                |importance| importance.embedded_whitespace_important = false,
                |rules| !rules.embedded_whitespace_important,
            ),
            (
                |importance| importance.trailing_whitespace_important = false,
                |rules| !rules.trailing_whitespace_important,
            ),
            (
                |importance| importance.everything_else_important = false,
                |rules| !rules.everything_else_important,
            ),
            (
                |importance| importance.character_case_important = true,
                |rules| rules.match_character_case,
            ),
            (
                |importance| importance.orphan_lines_always_important = false,
                |rules| !rules.orphan_lines_always_important,
            ),
        ];
        for (edit, read) in table {
            let mut settings = TextMergeSettings::default();
            assert!(!read(&options_from(&settings).rules));
            edit(&mut settings.importance);
            assert!(read(&options_from(&settings).rules));
        }
    }

    #[test]
    fn no_importance_flag_makes_two_different_lines_equal() {
        let mut settings = TextMergeSettings::default();
        settings.importance.character_case_important = false;
        settings.importance.leading_whitespace_important = false;
        let compare = options_from(&settings).merge.compare;
        assert!(!compare.ignore_case);
        assert!(!compare.ignore_leading_whitespace);
    }

    /// Each conflict scope field reaches the merge engine and changes what it
    /// treats as one conflict.
    #[test]
    fn the_conflict_scope_reaches_the_merge_options() {
        use ca_diff::merge3::ConflictScope;
        let mut settings = TextMergeSettings::default();
        assert_eq!(
            options_from(&settings).merge.conflict_scope,
            ConflictScope::Separation { lines: 2 }
        );
        settings.conflicts.separation_lines = 9;
        assert_eq!(
            options_from(&settings).merge.conflict_scope,
            ConflictScope::Separation { lines: 9 }
        );
        settings.conflicts.same_lines_only = true;
        assert_eq!(
            options_from(&settings).merge.conflict_scope,
            ConflictScope::ChangedLinesOnly,
            "changed lines only outranks the separation distance"
        );
    }

    /// Two nearby opposing changes are one conflict under the stored
    /// separation and two under changed lines only.
    #[test]
    fn the_conflict_scope_changes_the_merge_result() {
        use ca_diff::merge3::merge3;
        let base = ["a\n", "b\n", "c\n", "d\n", "e\n"];
        let left = ["A\n", "b\n", "c\n", "d\n", "e\n"];
        let right = ["a\n", "b\n", "C\n", "d\n", "e\n"];
        let mut settings = TextMergeSettings::default();
        settings.conflicts.separation_lines = 3;
        let joined = merge3(&left, &base, &right, &options_from(&settings).merge);
        settings.conflicts.same_lines_only = true;
        let apart = merge3(&left, &base, &right, &options_from(&settings).merge);
        assert_ne!(
            joined.conflicts, apart.conflicts,
            "the separation distance did not change the conflict count"
        );
    }

    #[test]
    fn a_named_encoding_reaches_the_decoder() {
        use ca_session::settings::common::EncodingChoice;
        assert_eq!(
            super::decode_from(&EncodingChoice::from_format()).forced,
            None
        );
        let named = EncodingChoice::Named {
            name: "UTF-16LE".to_owned(),
            unknown: std::collections::BTreeMap::new(),
        };
        assert_eq!(
            super::decode_from(&named).forced,
            Some(ca_text::TextEncoding::Utf16Le)
        );
    }

    /// The stored defaults state a skew bound the engine default leaves
    /// unbounded; everything else matches the engine's own starting point.
    #[test]
    fn the_stored_defaults_differ_from_the_engine_defaults_only_in_the_skew_bound() {
        let options = options_from(&TextMergeSettings::default()).merge;
        assert_eq!(options.compare.alignment.skew_tolerance, Some(500));
        let engine = ca_diff::merge3::MergeOptions::default();
        assert_eq!(options.conflict_scope, engine.conflict_scope);
        assert_eq!(
            options.compare.ignore_line_endings,
            engine.compare.ignore_line_endings
        );
        assert_eq!(
            options.compare.alignment.mode,
            engine.compare.alignment.mode
        );
    }
}
