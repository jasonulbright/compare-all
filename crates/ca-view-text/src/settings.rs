//! The one conversion from stored settings to the options the engines take.
//!
//! Every value the comparison runs under comes through here, so a toolbar
//! toggle and a settings page edit the same thing and the two can never
//! disagree.

use crate::jobs::{ElementRules, RuleToggles};
use ca_diff::importance::{ReplacementOptions, ReplacementRule, ReplacementSide};
use ca_diff::lines::{AlignAnchor, AlignmentMode, AlignmentOptions, LineCompareOptions};
use ca_session::settings::common::{AlignmentAlgorithm, ReplacementItem, RuleSide};
use ca_session::settings::text::{element, TextCompareSettings, TextImportanceSettings};
use ca_text::{DecodeOptions, TextEncoding};

/// Everything a text comparison runs under, derived from stored settings.
#[derive(Debug, Clone)]
pub struct EngineOptions {
    /// What the line pass disregards, and how lines are paired.
    pub compare: LineCompareOptions,
    /// Which differences count toward the result.
    pub rules: RuleToggles,
    /// Substitutions that make two spellings equivalent.
    pub replacements: Vec<ReplacementRule>,
    /// Pairings the user forced, in increasing line order.
    pub anchors: Vec<AlignAnchor>,
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

/// The importance rules a stored importance group states.
#[must_use]
pub fn rules_from(importance: &TextImportanceSettings) -> RuleToggles {
    RuleToggles {
        elements: ElementRules {
            comments: importance.element_important(element::COMMENT),
            strings: importance.element_important(element::STRING),
            numbers: importance.element_important(element::NUMBER),
            keywords: importance.element_important(element::KEYWORD),
            identifiers: importance.element_important(element::IDENTIFIER),
        },
        leading_whitespace_important: importance.leading_whitespace_important,
        embedded_whitespace_important: importance.embedded_whitespace_important,
        trailing_whitespace_important: importance.trailing_whitespace_important,
        everything_else_important: importance.everything_else_important,
        case_unimportant: !importance.character_case_important,
        orphan_lines_always_important: importance.orphan_lines_always_important,
    }
}

/// The sides a stored rule side names.
///
/// A rule applying to both sides becomes one rule per side, because the engine
/// rewrites one side at a time.
fn sides_of(side: &RuleSide) -> &'static [ReplacementSide] {
    match side {
        RuleSide::Left => &[ReplacementSide::Left],
        RuleSide::Right => &[ReplacementSide::Right],
        // A side this build does not understand applies to both, which is what
        // the stored default states.
        _ => &[ReplacementSide::Left, ReplacementSide::Right],
    }
}

/// The compiled form of one stored substitution, once per side it applies to.
///
/// A rule whose pattern does not compile yields nothing. Refusing the session
/// over one malformed pattern would hide every other rule as well.
fn compile(item: &ReplacementItem) -> Vec<ReplacementRule> {
    if item.find.is_empty() {
        return Vec::new();
    }
    sides_of(&item.side)
        .iter()
        .filter_map(|side| {
            ReplacementRule::new(
                &item.find,
                &item.replace_with,
                ReplacementOptions {
                    match_character_case: item.match_case,
                    whole_words_only: item.whole_words_only,
                    regular_expression: item.regular_expression,
                    side: *side,
                },
            )
            .ok()
        })
        .collect()
}

/// The substitutions a stored replacement list states, in list order.
#[must_use]
pub fn replacements_from(settings: &TextCompareSettings) -> Vec<ReplacementRule> {
    settings
        .replacements
        .items
        .iter()
        .flat_map(compile)
        .collect()
}

/// The forced pairings a stored alignment group states.
///
/// Anchors reach the engine sorted and non-overlapping; one that runs backwards
/// or overlaps its predecessor is dropped, because the engine refuses the whole
/// comparison over a malformed list.
#[must_use]
pub fn anchors_from(
    alignment: &ca_session::settings::text::TextAlignmentSettings,
) -> Vec<AlignAnchor> {
    let mut wanted: Vec<AlignAnchor> = alignment
        .manual_alignments
        .iter()
        .filter(|entry| entry.left_start <= entry.left_end && entry.right_start <= entry.right_end)
        .map(|entry| AlignAnchor {
            left: entry.left_start..entry.left_end,
            right: entry.right_start..entry.right_end,
        })
        .collect();
    wanted.sort_by_key(|anchor| (anchor.left.start, anchor.right.start));
    let mut out: Vec<AlignAnchor> = Vec::with_capacity(wanted.len());
    let (mut left, mut right) = (0u32, 0u32);
    for anchor in wanted {
        if anchor.left.start < left || anchor.right.start < right {
            continue;
        }
        left = anchor.left.end;
        right = anchor.right.end;
        out.push(anchor);
    }
    out
}

/// The name a stored format choice pins, or nothing when the file name decides.
#[must_use]
pub fn pinned_format(choice: &ca_session::settings::common::FileFormatChoice) -> Option<&str> {
    match choice {
        ca_session::settings::common::FileFormatChoice::Named { name, .. } => Some(name.as_str()),
        _ => None,
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

/// The options a text comparison session's settings state.
#[must_use]
pub fn options_from(settings: &TextCompareSettings) -> EngineOptions {
    let importance = &settings.importance;
    let alignment = &settings.alignment;
    EngineOptions {
        replacements: replacements_from(settings),
        anchors: anchors_from(alignment),
        compare: LineCompareOptions {
            // The importance group decides which differences count, not which
            // lines differ. A pair differing only in letter case is still a
            // pair of different lines; the rules then mark it unimportant.
            ignore_case: false,
            ignore_leading_whitespace: false,
            ignore_embedded_whitespace: false,
            ignore_trailing_whitespace: false,
            ignore_all_whitespace: false,
            ignore_line_endings: !importance.compare_line_endings,
            alignment: AlignmentOptions {
                mode: alignment_mode(&alignment.algorithm),
                // Zero states no bound rather than a bound of nothing.
                skew_tolerance: (alignment.skew_tolerance > 0).then_some(alignment.skew_tolerance),
                never_align_differences: alignment.never_align_differences,
                use_closeness_matching: alignment.use_closeness_matching,
            },
        },
        rules: rules_from(importance),
    }
}

/// Writes the importance rules back into the settings they came from.
///
/// A toolbar toggle is a shortcut to one of these fields, so it edits the
/// stored settings rather than a copy beside them.
pub fn write_rules(importance: &mut TextImportanceSettings, rules: RuleToggles) {
    importance.leading_whitespace_important = rules.leading_whitespace_important;
    importance.embedded_whitespace_important = rules.embedded_whitespace_important;
    importance.trailing_whitespace_important = rules.trailing_whitespace_important;
    importance.everything_else_important = rules.everything_else_important;
    importance.character_case_important = !rules.case_unimportant;
    importance.orphan_lines_always_important = rules.orphan_lines_always_important;
    for (name, important) in [
        (element::COMMENT, rules.elements.comments),
        (element::STRING, rules.elements.strings),
        (element::NUMBER, rules.elements.numbers),
        (element::KEYWORD, rules.elements.keywords),
        (element::IDENTIFIER, rules.elements.identifiers),
    ] {
        importance.set_element_important(name, important);
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::bool_assert_comparison,
    clippy::field_reassign_with_default
)]
mod tests {
    use super::{alignment_mode, options_from, rules_from, write_rules};
    use ca_diff::lines::AlignmentMode;
    use ca_session::settings::common::AlignmentAlgorithm;
    use ca_session::settings::text::{
        element, GrammarImportance, ManualAlignment, TextCompareSettings, TextImportanceSettings,
    };

    /// Each importance flag reaches exactly the rule it names.
    #[test]
    fn every_importance_flag_reaches_its_own_rule() {
        type Case = (
            fn(&mut TextImportanceSettings),
            fn(&super::EngineOptions) -> bool,
        );
        let cases: &[Case] = &[
            (
                |importance| importance.character_case_important = true,
                |options| !options.rules.case_unimportant,
            ),
            (
                |importance| importance.leading_whitespace_important = false,
                |options| !options.rules.leading_whitespace_important,
            ),
            (
                |importance| importance.embedded_whitespace_important = false,
                |options| !options.rules.embedded_whitespace_important,
            ),
            (
                |importance| importance.trailing_whitespace_important = false,
                |options| !options.rules.trailing_whitespace_important,
            ),
            (
                |importance| importance.everything_else_important = false,
                |options| !options.rules.everything_else_important,
            ),
            (
                |importance| importance.orphan_lines_always_important = false,
                |options| !options.rules.orphan_lines_always_important,
            ),
            (
                |importance| importance.compare_line_endings = true,
                |options| !options.compare.ignore_line_endings,
            ),
        ];
        for (index, (edit, read)) in cases.iter().enumerate() {
            let mut settings = TextCompareSettings::default();
            edit(&mut settings.importance);
            assert!(read(&options_from(&settings)), "case {index}");
        }
    }

    /// The importance group says which differences count, not which lines
    /// differ, so no importance flag reaches the line pass.
    #[test]
    fn no_importance_flag_makes_two_different_lines_equal() {
        let mut settings = TextCompareSettings::default();
        settings.importance.character_case_important = false;
        settings.importance.leading_whitespace_important = false;
        settings.importance.embedded_whitespace_important = false;
        settings.importance.trailing_whitespace_important = false;
        let compare = options_from(&settings).compare;
        assert!(!compare.ignore_case);
        assert!(!compare.ignore_leading_whitespace);
        assert!(!compare.ignore_embedded_whitespace);
        assert!(!compare.ignore_trailing_whitespace);
        assert!(!compare.ignore_all_whitespace);
    }

    #[test]
    fn the_defaults_carry_the_documented_line_ending_behavior() {
        let options = options_from(&TextCompareSettings::default());
        assert!(
            options.compare.ignore_line_endings,
            "line endings are not compared unless the session asks"
        );
        assert_eq!(options.compare.alignment.mode, AlignmentMode::Standard);
        assert!(options.compare.alignment.use_closeness_matching);
    }

    #[test]
    fn every_algorithm_reaches_its_own_pass() {
        assert_eq!(
            alignment_mode(&AlignmentAlgorithm::Unaligned),
            AlignmentMode::Unaligned
        );
        assert_eq!(
            alignment_mode(&AlignmentAlgorithm::Standard),
            AlignmentMode::Standard
        );
        assert_eq!(
            alignment_mode(&AlignmentAlgorithm::MyersOnd),
            AlignmentMode::Myers
        );
        assert_eq!(
            alignment_mode(&AlignmentAlgorithm::Patience),
            AlignmentMode::Patience
        );
        assert_eq!(
            alignment_mode(&AlignmentAlgorithm::Unknown(serde_json::json!("future"))),
            AlignmentMode::Standard,
            "an algorithm this build has no pass for still opens"
        );
    }

    #[test]
    fn the_alignment_group_reaches_the_alignment_options() {
        let mut settings = TextCompareSettings::default();
        settings.alignment.skew_tolerance = 37;
        settings.alignment.never_align_differences = true;
        settings.alignment.use_closeness_matching = false;
        let options = options_from(&settings);
        assert_eq!(options.compare.alignment.skew_tolerance, Some(37));
        assert!(options.compare.alignment.never_align_differences);
        assert!(!options.compare.alignment.use_closeness_matching);
    }

    #[test]
    fn a_skew_of_zero_states_no_bound() {
        let mut settings = TextCompareSettings::default();
        settings.alignment.skew_tolerance = 0;
        assert_eq!(
            options_from(&settings).compare.alignment.skew_tolerance,
            None
        );
    }

    #[test]
    fn an_unimportant_comment_element_reaches_the_rules() {
        let mut importance = TextImportanceSettings::default();
        assert!(rules_from(&importance).elements.comments);
        importance.grammar_elements.push(GrammarImportance {
            element: "Comment".to_owned(),
            important: false,
            unknown: std::collections::BTreeMap::new(),
        });
        assert!(!rules_from(&importance).elements.comments);
    }

    /// Every entry of the checklist reaches its own class, and no entry
    /// disturbs another.
    #[test]
    fn every_element_class_reaches_its_own_rule() {
        type Read = fn(&super::ElementRules) -> bool;
        let cases: &[(&str, Read)] = &[
            (element::COMMENT, |rules| rules.comments),
            (element::STRING, |rules| rules.strings),
            (element::NUMBER, |rules| rules.numbers),
            (element::KEYWORD, |rules| rules.keywords),
            (element::IDENTIFIER, |rules| rules.identifiers),
        ];
        for (name, read) in cases {
            let mut importance = TextImportanceSettings::default();
            importance.set_element_important(name, false);
            let rules = rules_from(&importance).elements;
            assert!(!read(&rules), "{name} did not reach its class");
            let others = cases.iter().filter(|(other, _)| other != name);
            for (other, read_other) in others {
                assert!(read_other(&rules), "{name} also changed {other}");
            }
        }
    }

    /// A slot the checklist has no class for stays important, so an element the
    /// dialog never showed behaves as it did before the checklist existed.
    #[test]
    fn a_class_the_checklist_does_not_carry_stays_important() {
        let rules = super::ElementRules {
            comments: false,
            strings: false,
            numbers: false,
            keywords: false,
            identifiers: false,
        };
        assert!(rules.slot_important(ca_grammar::StyleSlot::Operator));
        assert!(rules.slot_important(ca_grammar::StyleSlot::Tag));
        assert!(!rules.slot_important(ca_grammar::StyleSlot::Comment));
    }

    #[test]
    fn the_rules_write_back_into_the_settings_they_came_from() {
        let mut importance = TextImportanceSettings::default();
        let mut rules = rules_from(&importance);
        rules.elements.comments = false;
        rules.elements.numbers = false;
        rules.case_unimportant = false;
        rules.trailing_whitespace_important = false;
        write_rules(&mut importance, rules);
        assert_eq!(
            rules_from(&importance),
            rules,
            "the write did not round trip"
        );
        assert!(importance.character_case_important);
        assert!(!importance.trailing_whitespace_important);
    }

    fn replacement(find: &str, replace: &str) -> ca_session::settings::common::ReplacementItem {
        ca_session::settings::common::ReplacementItem {
            find: find.to_owned(),
            replace_with: replace.to_owned(),
            ..ca_session::settings::common::ReplacementItem::default()
        }
    }

    /// A stored substitution reaches the engine and makes a matching pair
    /// unimportant, which is the whole point of the list.
    fn worst_importance(
        left: &[&str],
        right: &[&str],
        options: &super::EngineOptions,
    ) -> Option<ca_diff::Importance> {
        let mut rules = ca_diff::RuleSet::all_important();
        rules.replacements.clone_from(&options.replacements);
        let hunks = ca_diff::diff_line_slices(left, right, &options.compare);
        ca_diff::classify_hunks(left, right, &hunks, &rules, &ca_diff::WhitespaceClassifier)
            .unwrap()
            .into_iter()
            .find_map(|hunk| hunk.importance)
    }

    #[test]
    fn a_stored_replacement_makes_a_matching_pair_unimportant() {
        use ca_diff::Importance;
        let left = ["an apple a day\n"];
        let right = ["an orange a day\n"];
        let mut settings = TextCompareSettings::default();
        let before = worst_importance(&left, &right, &options_from(&settings));
        assert_eq!(before, Some(Importance::Important));

        settings
            .replacements
            .items
            .push(replacement("apple", "orange"));
        let after = worst_importance(&left, &right, &options_from(&settings));
        assert_eq!(after, Some(Importance::Unimportant));
    }

    /// A replacement that leaves the two sides still differing elsewhere does
    /// not suppress that other difference.
    #[test]
    fn a_stored_replacement_does_not_hide_an_unrelated_difference() {
        use ca_diff::Importance;
        let mut settings = TextCompareSettings::default();
        settings
            .replacements
            .items
            .push(replacement("apple", "orange"));
        assert_eq!(
            worst_importance(
                &["an apple a day\n"],
                &["an orange a week\n"],
                &options_from(&settings),
            ),
            Some(Importance::Important)
        );
    }

    #[test]
    fn a_replacement_applies_to_the_side_it_names() {
        let mut settings = TextCompareSettings::default();
        settings
            .replacements
            .items
            .push(replacement("apple", "orange"));
        // The stored default side is both, which the engine takes as one rule
        // per side.
        assert_eq!(super::replacements_from(&settings).len(), 2);
        settings.replacements.items[0].side = ca_session::settings::common::RuleSide::Left;
        let only_left = super::replacements_from(&settings);
        assert_eq!(only_left.len(), 1);
        assert_eq!(
            only_left[0].side(),
            ca_diff::importance::ReplacementSide::Left
        );
    }

    #[test]
    fn a_replacement_whose_pattern_does_not_compile_is_dropped() {
        let mut settings = TextCompareSettings::default();
        let mut broken = replacement("([unclosed", "x");
        broken.regular_expression = true;
        settings.replacements.items.push(broken);
        settings
            .replacements
            .items
            .push(replacement("apple", "orange"));
        assert_eq!(
            super::replacements_from(&settings).len(),
            2,
            "the sound rule still reaches both sides"
        );
    }

    #[test]
    fn a_manual_pairing_reaches_the_engine_in_line_order() {
        let mut settings = TextCompareSettings::default();
        assert!(options_from(&settings).anchors.is_empty());
        settings.alignment.manual_alignments = vec![
            ManualAlignment {
                left_start: 4,
                left_end: 5,
                right_start: 6,
                right_end: 7,
                ..ManualAlignment::default()
            },
            ManualAlignment {
                left_start: 1,
                left_end: 2,
                right_start: 1,
                right_end: 2,
                ..ManualAlignment::default()
            },
        ];
        let anchors = options_from(&settings).anchors;
        assert_eq!(anchors.len(), 2);
        assert_eq!(anchors[0].left, 1..2);
        assert_eq!(anchors[1].right, 6..7);
    }

    /// The engine refuses a malformed anchor list outright, so an overlapping
    /// or reversed entry is dropped before it gets there.
    #[test]
    fn an_overlapping_or_reversed_pairing_is_dropped() {
        let mut settings = TextCompareSettings::default();
        settings.alignment.manual_alignments = vec![
            ManualAlignment {
                left_start: 0,
                left_end: 4,
                right_start: 0,
                right_end: 4,
                ..ManualAlignment::default()
            },
            ManualAlignment {
                left_start: 2,
                left_end: 3,
                right_start: 2,
                right_end: 3,
                ..ManualAlignment::default()
            },
            ManualAlignment {
                left_start: 9,
                left_end: 8,
                right_start: 9,
                right_end: 10,
                ..ManualAlignment::default()
            },
        ];
        let anchors = options_from(&settings).anchors;
        assert_eq!(anchors.len(), 1);
        assert_eq!(anchors[0].left, 0..4);
        let left = ["a\n", "b\n", "c\n", "d\n"];
        assert!(
            ca_diff::lines::diff_line_slices_anchored(
                &left,
                &left,
                &ca_diff::LineCompareOptions::default(),
                &anchors,
            )
            .is_ok(),
            "the surviving list is one the engine accepts"
        );
    }

    /// A forced pairing overrides what the algorithm produced, which is the
    /// only reason to store one.
    #[test]
    fn a_forced_pairing_changes_the_row_layout() {
        let left = ["x\n", "a\n", "b\n"];
        let right = ["a\n", "b\n", "x\n"];
        let options = ca_diff::LineCompareOptions::default();
        let plain = ca_diff::diff_line_slices(&left, &right, &options);
        let forced = ca_diff::lines::diff_line_slices_anchored(
            &left,
            &right,
            &options,
            &[ca_diff::lines::AlignAnchor {
                left: 0..1,
                right: 0..1,
            }],
        )
        .unwrap();
        assert_ne!(plain, forced);
    }

    #[test]
    fn a_named_encoding_reaches_the_decoder_and_an_unnamed_one_does_not() {
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
        let unheard = EncodingChoice::Named {
            name: "not-an-encoding".to_owned(),
            unknown: std::collections::BTreeMap::new(),
        };
        assert_eq!(
            super::decode_from(&unheard).forced,
            None,
            "an encoding this build has no decoder for still opens the session"
        );
    }

    #[test]
    fn a_named_format_is_pinned_and_a_detected_one_is_not() {
        use ca_session::settings::common::FileFormatChoice;
        assert_eq!(super::pinned_format(&FileFormatChoice::detected()), None);
        let named = FileFormatChoice::Named {
            name: "Python".to_owned(),
            unknown: std::collections::BTreeMap::new(),
        };
        assert_eq!(super::pinned_format(&named), Some("Python"));
    }

    #[test]
    fn the_starting_rules_match_the_stored_defaults() {
        let rules = rules_from(&TextImportanceSettings::default());
        assert_eq!(rules, crate::jobs::RuleToggles::default());
    }
}
