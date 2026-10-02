//! A format aware comparison for text files.
//!
//! The comparison reads both files as lines, aligns them, and asks the rule set
//! whether each difference matters. Two files whose only differences are
//! unimportant compare as equivalent. A file that does not decode as UTF-8 is
//! read with the invalid sequences replaced, so a byte difference inside them
//! still shows up as a difference in the text.

use std::path::Path;

use ca_diff::{RuleSet, WhitespaceClassifier};
use ca_fs::{ContentError, ContentOutcome, RulesComparer};

/// The rule set a run uses when a command asks for a rules based comparison.
#[derive(Debug, Default)]
pub struct TextRules {
    rules: RuleSet,
}

impl TextRules {
    /// A comparer that treats a difference in white space alone as
    /// unimportant.
    #[must_use]
    pub fn new() -> Self {
        let mut rules = RuleSet::all_important();
        rules.leading_whitespace_important = false;
        rules.embedded_whitespace_important = false;
        rules.trailing_whitespace_important = false;
        Self { rules }
    }
}

impl RulesComparer for TextRules {
    fn compare(
        &self,
        left: &Path,
        right: &Path,
        cancel: &ca_fs::Cancel,
    ) -> Result<ContentOutcome, ContentError> {
        let left_bytes = read(left)?;
        let right_bytes = read(right)?;
        self.compare_bytes(left, &left_bytes, &right_bytes, cancel)
    }

    fn compare_bytes(
        &self,
        left: &Path,
        left_bytes: &[u8],
        right_bytes: &[u8],
        _cancel: &ca_fs::Cancel,
    ) -> Result<ContentOutcome, ContentError> {
        if left_bytes == right_bytes {
            return Ok(ContentOutcome::BinarySame);
        }
        let left_text = String::from_utf8_lossy(left_bytes);
        let right_text = String::from_utf8_lossy(right_bytes);
        let left_lines = ca_diff::split_lines(&left_text);
        let right_lines = ca_diff::split_lines(&right_text);
        let hunks = ca_diff::diff_line_slices(
            &left_lines,
            &right_lines,
            &ca_diff::LineCompareOptions::default(),
        );
        let classified = ca_diff::classify_hunks(
            &left_lines,
            &right_lines,
            &hunks,
            &self.rules,
            &WhitespaceClassifier,
        )
        .map_err(|error| ContentError::Io {
            path: left.display().to_string(),
            source: std::io::Error::other(error.to_string()),
        })?;
        let important = classified
            .iter()
            .any(|hunk| hunk.importance == Some(ca_diff::Importance::Important));
        Ok(if important {
            ContentOutcome::ImportantDifferences
        } else {
            ContentOutcome::UnimportantDifferences
        })
    }
}

fn read(path: &Path) -> Result<Vec<u8>, ContentError> {
    std::fs::read(path).map_err(|source| ContentError::Io {
        path: path.display().to_string(),
        source,
    })
}
