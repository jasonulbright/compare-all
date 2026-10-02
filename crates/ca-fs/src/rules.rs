//! A content test that reads two files through the file format that claims
//! them, so a difference the format calls unimportant does not mark the pair as
//! changed.
//!
//! The engine runs on whatever thread the content pass gives it and polls the
//! cancellation flag between its own steps, so a long comparison stops without
//! finishing the file.

use std::path::Path;

use ca_diff::{
    classify_hunks_indexed_cancellable, diff_line_slices_cancellable, split_lines, ClassifiedHunk,
    ClassifiedToken, ClassifierSide, HunkKind, Importance, IndexedLineClassifier,
    LineCompareOptions, RuleSet, WhitespaceClassifier,
};
use ca_grammar::{FormatRegistry, IndexedGrammarClassifier};
use ca_text::{looks_binary, DecodeOptions};

use crate::cancel::Cancel;
use crate::criteria::{ContentError, ContentOutcome, RulesComparer};

/// The greatest number of bytes read from one file as text.
const READ_LIMIT: u64 = 64 * 1024 * 1024;

/// A format aware content test.
///
/// The registry decides which grammar reads a name; the rule set decides which
/// of the resulting elements count. Both are fixed for the life of the engine,
/// so the engine is shared between workers without a lock.
pub struct RulesEngine {
    registry: FormatRegistry,
    rules: RuleSet,
    line_options: LineCompareOptions,
    unimportant_elements: Vec<String>,
    enabled_formats: Vec<String>,
    disabled_formats: Vec<String>,
}

impl std::fmt::Debug for RulesEngine {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RulesEngine")
            .field("enabled_formats", &self.enabled_formats)
            .field("disabled_formats", &self.disabled_formats)
            .finish_non_exhaustive()
    }
}

impl RulesEngine {
    /// An engine over the stock formats, in which whitespace differences are
    /// unimportant and everything else counts.
    #[must_use]
    pub fn new(registry: FormatRegistry) -> Self {
        let mut rules = RuleSet::all_important();
        rules.leading_whitespace_important = false;
        rules.embedded_whitespace_important = false;
        rules.trailing_whitespace_important = false;
        Self {
            registry,
            rules,
            line_options: LineCompareOptions::default(),
            unimportant_elements: Vec::new(),
            enabled_formats: Vec::new(),
            disabled_formats: Vec::new(),
        }
    }

    /// The same engine in which differences inside the named grammar elements
    /// do not count. Every other element the grammar declares counts.
    #[must_use]
    pub fn with_unimportant_elements(mut self, names: Vec<String>) -> Self {
        self.unimportant_elements = names;
        self
    }

    /// The rule set for one grammar: every element it declares is important
    /// unless the engine names it as unimportant.
    fn rules_for(&self, element_names: &[String]) -> RuleSet {
        let mut rules = self.rules.clone();
        for name in element_names {
            if self
                .unimportant_elements
                .iter()
                .any(|other| other.eq_ignore_ascii_case(name))
            {
                rules.important_elements.remove(name);
            } else {
                rules.important_elements.insert(name.clone());
            }
        }
        rules
    }

    /// An engine over the formats this build ships.
    #[must_use]
    pub fn with_builtin_formats() -> Self {
        Self::new(ca_grammar::builtin::registry())
    }

    /// The same engine with a different set of importance rules.
    #[must_use]
    pub fn with_rules(mut self, rules: RuleSet) -> Self {
        self.rules = rules;
        self
    }

    /// The same engine with different line pairing options.
    #[must_use]
    pub fn with_line_options(mut self, options: LineCompareOptions) -> Self {
        self.line_options = options;
        self
    }

    /// The same engine with the per-session format lists applied.
    ///
    /// A name in `disabled` never claims a file even when its masks match; a
    /// name in `enabled` claims a file the global state would have skipped.
    #[must_use]
    pub fn with_format_lists(mut self, enabled: Vec<String>, disabled: Vec<String>) -> Self {
        for name in &enabled {
            if let Some(index) = self.index_of_format(name) {
                if let Some(format) = self.registry.formats.get_mut(index) {
                    format.enabled = true;
                }
            }
        }
        for name in &disabled {
            if let Some(index) = self.index_of_format(name) {
                if let Some(format) = self.registry.formats.get_mut(index) {
                    format.enabled = false;
                }
            }
        }
        self.enabled_formats = enabled;
        self.disabled_formats = disabled;
        self
    }

    fn index_of_format(&self, name: &str) -> Option<usize> {
        self.registry
            .formats
            .iter()
            .position(|format| format.name.eq_ignore_ascii_case(name.trim()))
    }

    /// The name of the format that claims `path`.
    #[must_use]
    pub fn format_name(&self, path: &Path) -> String {
        self.registry.lookup(&path.to_string_lossy()).name.clone()
    }
}

impl RulesComparer for RulesEngine {
    fn compare(
        &self,
        left: &Path,
        right: &Path,
        cancel: &Cancel,
    ) -> Result<ContentOutcome, ContentError> {
        let left_bytes = read_limited(left)?;
        if cancel.is_cancelled() {
            return Err(ContentError::Cancelled);
        }
        let right_bytes = read_limited(right)?;
        if cancel.is_cancelled() {
            return Err(ContentError::Cancelled);
        }
        self.compare_bytes(left, &left_bytes, &right_bytes, cancel)
    }

    fn compare_bytes(
        &self,
        left: &Path,
        left_bytes: &[u8],
        right_bytes: &[u8],
        cancel: &Cancel,
    ) -> Result<ContentOutcome, ContentError> {
        if looks_binary(left_bytes) || looks_binary(right_bytes) {
            return Ok(ContentOutcome::BinaryDifferences);
        }

        let options = DecodeOptions::default();
        if left_bytes == right_bytes {
            return Ok(ContentOutcome::BinarySame);
        }
        let left_decoded = ca_text::decode(left_bytes, &options);
        let right_decoded = ca_text::decode(right_bytes, &options);
        if left_decoded.had_errors || right_decoded.had_errors {
            return Ok(ContentOutcome::BinaryDifferences);
        }
        let left_text = left_decoded.text;
        let right_text = right_decoded.text;

        let left_lines = split_lines(&left_text);
        let right_lines = split_lines(&right_text);
        let flag = CancelFlag(cancel.clone());
        let hunks =
            diff_line_slices_cancellable(&left_lines, &right_lines, &self.line_options, &flag)
                .map_err(|_| ContentError::Cancelled)?;
        if cancel.is_cancelled() {
            return Err(ContentError::Cancelled);
        }

        let grammar = &self.registry.lookup(&left.to_string_lossy()).grammar;
        let indexed = IndexedGrammarClassifier::new(grammar, &left_lines, &right_lines).ok();
        let classified = if let Some(classifier) = indexed.as_ref() {
            let mut rules = self.rules_for(classifier.element_names());
            rules
                .case_sensitive_elements
                .extend(classifier.case_sensitive_elements());
            rules.compare_line_endings = !self.line_options.ignore_line_endings;
            classify_hunks_indexed_cancellable(
                &left_lines,
                &right_lines,
                &hunks,
                &rules,
                classifier,
                &flag,
            )
        } else {
            let mut rules = self.rules_for(&[]);
            rules.compare_line_endings = !self.line_options.ignore_line_endings;
            classify_hunks_indexed_cancellable(
                &left_lines,
                &right_lines,
                &hunks,
                &rules,
                &PlainClassifier,
                &flag,
            )
        }
        .map_err(|_| ContentError::Cancelled)?;

        Ok(verdict(&classified))
    }
}

/// The outcome a classified comparison states.
fn verdict(classified: &[ClassifiedHunk]) -> ContentOutcome {
    let mut any_difference = false;
    for hunk in classified {
        if hunk.hunk.kind == HunkKind::Same {
            continue;
        }
        any_difference = true;
        if hunk.importance == Some(Importance::Important) {
            return ContentOutcome::ImportantDifferences;
        }
    }
    if any_difference {
        ContentOutcome::UnimportantDifferences
    } else {
        // The bytes differ, because the caller ran the byte test first, so the
        // difference is one the line comparison folds away: an encoding, a byte
        // order mark or a line terminator.
        ContentOutcome::RulesSame
    }
}

/// A classifier for a format with no grammar, under which whitespace is the
/// only structure a line carries.
struct PlainClassifier;

impl IndexedLineClassifier for PlainClassifier {
    fn classify_line_at(
        &self,
        _side: ClassifierSide,
        _line_index: usize,
        line: &str,
    ) -> Vec<ClassifiedToken> {
        use ca_diff::LineClassifier;
        WhitespaceClassifier.classify_line(line)
    }
}

/// The scan's flag as the diff engine's flag.
struct CancelFlag(Cancel);

impl ca_diff::Cancel for CancelFlag {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

/// Read a file as text, refusing it when it is larger than the limit.
fn read_limited(path: &Path) -> Result<Vec<u8>, ContentError> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|source| ContentError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let mut bytes = Vec::new();
    file.take(READ_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| ContentError::Io {
            path: path.display().to_string(),
            source,
        })?;
    if u64::try_from(bytes.len()).is_ok_and(|size| size > READ_LIMIT) {
        return Err(ContentError::Source {
            path: path.display().to_string(),
            detail: format!("rules comparison is limited to {READ_LIMIT} bytes per file"),
        });
    }
    Ok(bytes)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::{RulesEngine, READ_LIMIT};
    use crate::cancel::Cancel;
    use crate::criteria::{ContentError, ContentOutcome, RulesComparer};

    fn engine() -> RulesEngine {
        RulesEngine::new(ca_grammar::builtin::registry())
    }

    fn pair(
        left_text: &str,
        right_text: &str,
    ) -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("a.rs");
        let right = dir.path().join("b.rs");
        std::fs::write(&left, left_text).unwrap();
        std::fs::write(&right, right_text).unwrap();
        (dir, left, right)
    }

    #[test]
    fn a_whitespace_only_difference_is_unimportant() {
        let (_dir, left, right) = pair("let a = 1;\n", "let    a = 1;\n");
        let outcome = engine().compare(&left, &right, &Cancel::new()).unwrap();
        assert!(
            matches!(
                outcome,
                ContentOutcome::UnimportantDifferences | ContentOutcome::RulesSame
            ),
            "{outcome:?}"
        );
        assert!(outcome.is_same(true));
    }

    #[test]
    fn a_changed_statement_is_important() {
        let (_dir, left, right) = pair("let a = 1;\n", "let a = 2;\n");
        assert_eq!(
            engine().compare(&left, &right, &Cancel::new()).unwrap(),
            ContentOutcome::ImportantDifferences
        );
    }

    #[test]
    fn grammar_case_sensitive_elements_override_the_global_case_rule() {
        use ca_diff::RuleSet;

        let mut registry = ca_grammar::builtin::registry();
        let cpp = registry
            .formats
            .iter_mut()
            .find(|format| format.name == "C/C++")
            .unwrap();
        for item in &mut cpp.grammar.items {
            if item.element == "String" {
                item.case_sensitive = true;
            }
        }

        let mut rules = RuleSet::all_important();
        rules.match_character_case = false;
        let engine = RulesEngine::new(registry).with_rules(rules);

        assert_eq!(
            engine
                .compare_bytes(
                    std::path::Path::new("left.cpp"),
                    b"const char *key = \"Secret\";\n",
                    b"const char *key = \"secret\";\n",
                    &Cancel::new(),
                )
                .unwrap(),
            ContentOutcome::ImportantDifferences
        );
    }

    #[test]
    fn a_line_ending_difference_alone_is_the_same_under_the_rules() {
        let (_dir, left, right) = pair("one\ntwo\n", "one\r\ntwo\r\n");
        assert_eq!(
            engine().compare(&left, &right, &Cancel::new()).unwrap(),
            ContentOutcome::RulesSame
        );
    }

    #[test]
    fn line_ending_comparison_applies_when_a_grammar_cannot_be_compiled() {
        use ca_diff::LineCompareOptions;
        use ca_grammar::{Grammar, GrammarItem, ItemKind, MatchOptions};

        let mut registry = ca_grammar::builtin::registry();
        registry.fallback.grammar = Grammar::from_items(vec![GrammarItem::new(
            "Invalid",
            ItemKind::Basic {
                text: "[".to_owned(),
                options: MatchOptions::regex(),
                whole_word: false,
            },
        )]);
        let engine = RulesEngine::new(registry).with_line_options(LineCompareOptions {
            ignore_line_endings: false,
            ..LineCompareOptions::default()
        });

        assert_eq!(
            engine
                .compare_bytes(
                    std::path::Path::new("unknown-format.file"),
                    b"one\n",
                    b"one\r\n",
                    &Cancel::new(),
                )
                .unwrap(),
            ContentOutcome::ImportantDifferences
        );
    }

    #[test]
    fn a_missing_final_newline_is_same_under_the_rules() {
        assert_eq!(
            engine()
                .compare_bytes(
                    std::path::Path::new("left.rs"),
                    b"a\nb",
                    b"a\nb\n",
                    &Cancel::new(),
                )
                .unwrap(),
            ContentOutcome::RulesSame
        );
    }

    #[test]
    fn binary_content_is_not_read_as_text() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("a.bin");
        let right = dir.path().join("b.bin");
        std::fs::write(&left, [0_u8, 1, 2, 0, 3, 0, 0, 9]).unwrap();
        std::fs::write(&right, [0_u8, 1, 2, 0, 4, 0, 0, 9]).unwrap();
        assert_eq!(
            engine().compare(&left, &right, &Cancel::new()).unwrap(),
            ContentOutcome::BinaryDifferences
        );
    }

    #[test]
    fn distinct_non_utf8_text_is_not_reported_as_same() {
        let outcome = engine()
            .compare_bytes(
                std::path::Path::new("left.rs"),
                b"caf\xe9\n",
                b"caf\xe8\n",
                &Cancel::new(),
            )
            .unwrap();

        assert_ne!(outcome, ContentOutcome::RulesSame);
    }

    #[test]
    fn a_raised_flag_stops_the_comparison() {
        let (_dir, left, right) = pair("let a = 1;\n", "let a = 2;\n");
        let cancel = Cancel::new();
        cancel.cancel();
        assert!(matches!(
            engine().compare(&left, &right, &cancel),
            Err(ContentError::Cancelled)
        ));
    }

    #[test]
    fn a_missing_file_reports_an_io_error() {
        assert!(matches!(
            engine().compare(
                std::path::Path::new("Z:/missing/ca-fs-rules.txt"),
                std::path::Path::new("Z:/missing/ca-fs-rules-2.txt"),
                &Cancel::new()
            ),
            Err(ContentError::Io { .. })
        ));
    }

    #[test]
    fn a_disabled_format_stops_claiming_its_names() {
        let engine = engine().with_format_lists(Vec::new(), vec!["Rust".to_owned()]);
        assert_ne!(engine.format_name(std::path::Path::new("main.rs")), "Rust");
    }

    #[test]
    #[cfg_attr(
        debug_assertions,
        ignore = "large input ceiling is exercised in release builds"
    )]
    fn a_difference_past_the_rules_read_limit_is_not_reported_as_same() {
        use std::io::{Seek, SeekFrom, Write};

        let directory = tempfile::tempdir().unwrap();
        let left = directory.path().join("left.rs");
        let right = directory.path().join("right.rs");
        let chunk = vec![b'a'; 64 * 1024];
        for (path, tail) in [
            (&left, b"KEEP\n".as_slice()),
            (&right, b"GONE\n".as_slice()),
        ] {
            let mut file = std::fs::File::create(path).unwrap();
            for _ in 0..=READ_LIMIT / 64 / 1024 {
                file.write_all(&chunk).unwrap();
            }
            file.seek(SeekFrom::End(-5)).unwrap();
            file.write_all(tail).unwrap();
        }

        assert!(matches!(
            engine().compare(&left, &right, &Cancel::new()),
            Err(ContentError::Source { .. })
        ));
    }
}
