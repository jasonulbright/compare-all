//! Settings owned by the text session kinds.

use super::common::{
    AlignmentAlgorithm, FormatOverride, FormatSettings, ReplacementOverride, ReplacementSettings,
    SpecsOverride, SpecsSettings,
};
use super::defaults::{documented, measured, provisional};
use super::macros::{settings_composite, settings_group};
use serde::{Deserialize, Serialize};

/// Importance assigned to one named grammar element of the active format.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GrammarImportance {
    /// Name of the grammar element as the file format declares it.
    #[serde(default)]
    pub element: String,
    /// Differences inside the element count toward the comparison result.
    #[serde(default)]
    pub important: bool,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub unknown: std::collections::BTreeMap<String, serde_json::Value>,
}

/// Canonical names of the grammar element classes the importance checklist
/// carries.
///
/// A format names its grammar elements freely, so the checklist stores one
/// entry per class and the view assigns each element of the active format to
/// the class its name resolves to.
pub mod element {
    /// Commentary of any form.
    pub const COMMENT: &str = "Comment";
    /// String and character literals.
    pub const STRING: &str = "String";
    /// Numeric literals.
    pub const NUMBER: &str = "Number";
    /// Reserved words of the language.
    pub const KEYWORD: &str = "Keyword";
    /// User-chosen names.
    pub const IDENTIFIER: &str = "Identifier";
    /// Every class, in checklist order.
    pub const ALL: &[&str] = &[COMMENT, STRING, NUMBER, KEYWORD, IDENTIFIER];
}

/// A pairing of a run of left lines with a run of right lines that the user
/// forced, which the comparison honors whatever the alignment algorithm says.
///
/// Either run may be empty, which stands the opposite side's run alone instead
/// of pairing it with anything. Line numbers are zero based and the end is
/// exclusive.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ManualAlignment {
    /// First forced left line.
    #[serde(default)]
    pub left_start: u32,
    /// One past the last forced left line.
    #[serde(default)]
    pub left_end: u32,
    /// First forced right line.
    #[serde(default)]
    pub right_start: u32,
    /// One past the last forced right line.
    #[serde(default)]
    pub right_end: u32,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub unknown: std::collections::BTreeMap<String, serde_json::Value>,
}

settings_group! {
    /// Which differences count toward the comparison result.
    TextImportanceSettings / TextImportanceOverride {
        /// Per-element importance for the active format's grammar.
        grammar_elements: Vec<GrammarImportance> = Vec::new(),
        /// Whitespace at the start of a line is an important difference.
        leading_whitespace_important: bool = true,
        /// Whitespace inside a line is an important difference.
        embedded_whitespace_important: bool = true,
        /// Whitespace at the end of a line is an important difference.
        trailing_whitespace_important: bool = true,
        /// Text no grammar element claims is an important difference.
        everything_else_important: bool = true,
        /// Capitalization differences in unclaimed text are important.
        character_case_important: bool = measured::CHARACTER_CASE_IMPORTANT,
        /// A line present on one side only is important even when it holds
        /// nothing but unimportant text.
        orphan_lines_always_important: bool = true,
        /// Compare the control characters ending each line.
        compare_line_endings: bool = documented::COMPARE_LINE_ENDINGS,
    }
}

impl TextImportanceSettings {
    /// True when differences inside `element` count.
    ///
    /// An element the list does not name counts, so a format naming an element
    /// this session never heard of is important until it is unchecked.
    #[must_use]
    pub fn element_important(&self, element: &str) -> bool {
        self.grammar_elements
            .iter()
            .find(|entry| entry.element.eq_ignore_ascii_case(element))
            .is_none_or(|entry| entry.important)
    }

    /// States whether differences inside `element` count.
    ///
    /// An absent entry already means important, so marking an element important
    /// drops its entry instead of storing one. Without that the list would keep
    /// an entry stating the value the layer below already supplies, and the
    /// session would never inherit that element again. An entry carrying fields
    /// from another build is kept, because dropping it would lose them.
    pub fn set_element_important(&mut self, element: &str, important: bool) {
        let found = self
            .grammar_elements
            .iter()
            .position(|entry| entry.element.eq_ignore_ascii_case(element));
        if let Some(at) = found {
            let bare = self
                .grammar_elements
                .get(at)
                .is_some_and(|entry| entry.unknown.is_empty());
            if important && bare {
                self.grammar_elements.remove(at);
            } else if let Some(entry) = self.grammar_elements.get_mut(at) {
                entry.important = important;
            }
            return;
        }
        if important {
            return;
        }
        self.grammar_elements.push(GrammarImportance {
            element: element.to_owned(),
            important: false,
            unknown: std::collections::BTreeMap::new(),
        });
    }
}

settings_group! {
    /// When two opposing changes of a merge are a conflict.
    TextConflictSettings / TextConflictOverride {
        /// Only changes landing on the same ancestor lines conflict.
        same_lines_only: bool = false,
        /// Largest gap, in ancestor lines, that still makes a left change and a
        /// right change one conflict.
        separation_lines: u32 = 2,
    }
}

settings_group! {
    /// How lines are paired between the two sides.
    TextAlignmentSettings / TextAlignmentOverride {
        /// Algorithm pairing the lines.
        algorithm: AlignmentAlgorithm = AlignmentAlgorithm::Standard,
        /// Show lines carrying important differences as separate added and
        /// deleted blocks instead of one changed pair.
        never_align_differences: bool = false,
        /// Lines scanned ahead and behind when looking for a match.
        skew_tolerance: u32 = provisional::SKEW_TOLERANCE_LINES,
        /// Pair leftover unmatched lines by similarity.
        use_closeness_matching: bool = true,
        /// Pairings the user forced, in increasing line order. They override
        /// whatever the algorithm produced for the lines they name.
        manual_alignments: Vec<ManualAlignment> = Vec::new(),
    }
}

settings_composite! {
    /// Settings a text comparison session owns.
    TextCompareSettings / TextCompareOverride {
        /// Sides and description.
        specs: SpecsSettings => SpecsOverride,
        /// Format and encoding per side.
        format: FormatSettings => FormatOverride,
        /// Which differences count.
        importance: TextImportanceSettings => TextImportanceOverride,
        /// How lines are paired.
        alignment: TextAlignmentSettings => TextAlignmentOverride,
        /// Substitutions treated as unimportant.
        replacements: ReplacementSettings => ReplacementOverride,
    }
}

settings_composite! {
    /// Settings a text merge session owns. A merge has no replacement rules.
    TextMergeSettings / TextMergeOverride {
        /// Sides, ancestor, output and description.
        specs: SpecsSettings => SpecsOverride,
        /// Format and encoding per side.
        format: FormatSettings => FormatOverride,
        /// Which differences count.
        importance: TextImportanceSettings => TextImportanceOverride,
        /// How lines are paired.
        alignment: TextAlignmentSettings => TextAlignmentOverride,
        /// When two opposing changes are a conflict.
        conflicts: TextConflictSettings => TextConflictOverride,
    }
}

settings_composite! {
    /// Settings a single-file text editing session owns.
    TextEditSettings / TextEditOverride {
        /// The edited file and description.
        specs: SpecsSettings => SpecsOverride,
        /// Format and encoding, shared with text comparison.
        format: FormatSettings => FormatOverride,
    }
}

settings_composite! {
    /// Settings a patch viewing session owns.
    TextPatchSettings / TextPatchOverride {
        /// The patch file and description.
        specs: SpecsSettings => SpecsOverride,
        /// Format and encoding used to render the reconstructed sides.
        format: FormatSettings => FormatOverride,
    }
}
