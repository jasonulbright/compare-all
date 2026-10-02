//! The editable source text of one side, with the history that reverses
//! changes to it.
//!
//! A table edit is a replacement of byte ranges of the text the table was
//! parsed from, so every other byte of the file is kept as it was loaded. One
//! undo step is one list of replacements, which is what lets a replace all be
//! reversed in one step.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

static NEXT_REVISION: AtomicU64 = AtomicU64::new(1);

/// One reversible replacement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Splice {
    /// Byte offset the replacement starts at.
    pub at: usize,
    /// The text the replacement took out.
    pub removed: String,
    /// The text the replacement put in.
    pub inserted: String,
}

/// The source text of one side and its history.
#[derive(Debug, Clone)]
pub struct SideText {
    text: Arc<String>,
    /// Each entry is one undo step: splices in ascending order, each at an
    /// offset of the text as it was before the step.
    undo: Vec<Vec<Splice>>,
    redo: Vec<Vec<Splice>>,
    /// The depth of the undo stack when the text was last written.
    saved_depth: usize,
    revision: u64,
}

impl Default for SideText {
    fn default() -> Self {
        Self::new(String::new())
    }
}

impl SideText {
    /// Text as it was read.
    #[must_use]
    pub fn new(text: String) -> Self {
        Self {
            text: Arc::new(text),
            undo: Vec::new(),
            redo: Vec::new(),
            saved_depth: 0,
            revision: NEXT_REVISION.fetch_add(1, Ordering::Relaxed),
        }
    }

    /// Text as it was read, shared with the worker that read it.
    #[must_use]
    pub fn from_shared(text: Arc<String>) -> Self {
        Self {
            text,
            ..Self::new(String::new())
        }
    }

    /// The text, shared without a copy.
    #[must_use]
    pub fn snapshot(&self) -> Arc<String> {
        Arc::clone(&self.text)
    }

    /// The text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// True when the text differs from what was last written.
    #[must_use]
    pub const fn is_modified(&self) -> bool {
        self.undo.len() != self.saved_depth
    }

    /// Record the text as written.
    pub const fn mark_saved(&mut self) {
        self.saved_depth = self.undo.len();
    }

    /// Identity of the text sent to an asynchronous save.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Mark only the submitted revision saved; later edits stay unwritten,
    /// including an undo back to the text the save replaced.
    pub const fn mark_saved_revision(&mut self, revision: u64) {
        self.saved_depth = if self.revision == revision {
            self.undo.len()
        } else {
            usize::MAX
        };
    }

    /// True when there is a change to reverse.
    #[must_use]
    pub const fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// True when there is a reversed change to reapply.
    #[must_use]
    pub const fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Apply replacements as one undo step.
    ///
    /// Each entry is a byte range of the current text and the text that goes
    /// into it. Ranges that overlap an earlier one, run past the end or split a
    /// character are skipped. Returns how many were applied.
    pub fn replace(&mut self, mut changes: Vec<(std::ops::Range<usize>, String)>) -> usize {
        changes.sort_by_key(|(range, _)| range.start);
        let mut step = Vec::with_capacity(changes.len());
        let mut free_from = 0usize;
        for (range, inserted) in changes {
            if range.start < free_from || range.end < range.start {
                continue;
            }
            let Some(removed) = self.text.get(range.clone()) else {
                continue;
            };
            free_from = range.end.max(range.start.saturating_add(1));
            step.push(Splice {
                at: range.start,
                removed: removed.to_owned(),
                inserted,
            });
        }
        let count = step.len();
        if count == 0 {
            return 0;
        }
        if self.saved_depth > self.undo.len() {
            self.saved_depth = usize::MAX;
        }
        self.apply(&step);
        self.undo.push(step);
        self.redo.clear();
        count
    }

    /// Reverse the last step and report the offset it started at.
    pub fn undo(&mut self) -> Option<usize> {
        let step = self.undo.pop()?;
        self.apply(&reversed(&step));
        let at = step.first().map_or(0, |splice| splice.at);
        self.redo.push(step);
        Some(at)
    }

    /// Reapply the last reversed step and report the offset it started at.
    pub fn redo(&mut self) -> Option<usize> {
        let step = self.redo.pop()?;
        self.apply(&step);
        let at = step.first().map_or(0, |splice| splice.at);
        self.undo.push(step);
        Some(at)
    }

    /// Apply one step in a single pass over the text.
    fn apply(&mut self, step: &[Splice]) {
        let old = self.text.as_str();
        let mut out = String::with_capacity(old.len());
        let mut from = 0usize;
        for splice in step {
            let at = splice.at.clamp(from, old.len());
            out.push_str(old.get(from..at).unwrap_or_default());
            out.push_str(&splice.inserted);
            from = at.saturating_add(splice.removed.len()).min(old.len());
        }
        out.push_str(old.get(from..).unwrap_or_default());
        self.text = Arc::new(out);
        self.revision = NEXT_REVISION.fetch_add(1, Ordering::Relaxed);
    }
}

/// The step that reverses `step`, in the offsets of the text after it.
fn reversed(step: &[Splice]) -> Vec<Splice> {
    let mut grown = 0usize;
    let mut shrunk = 0usize;
    step.iter()
        .map(|splice| {
            let at = (splice.at + grown).saturating_sub(shrunk);
            grown += splice.inserted.len();
            shrunk += splice.removed.len();
            Splice {
                at,
                removed: splice.inserted.clone(),
                inserted: splice.removed.clone(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::SideText;

    #[test]
    fn a_step_of_several_replacements_reverses_in_one_undo() {
        let mut side = SideText::new("a,b,c\n".to_owned());
        assert_eq!(
            side.replace(vec![(4..5, "CCC".to_owned()), (0..1, String::new())]),
            2
        );
        assert_eq!(side.text(), ",b,CCC\n");
        assert!(side.is_modified());
        assert_eq!(side.undo(), Some(0));
        assert_eq!(side.text(), "a,b,c\n");
        assert!(!side.is_modified());
        assert_eq!(side.redo(), Some(0));
        assert_eq!(side.text(), ",b,CCC\n");
    }

    #[test]
    fn overlapping_and_out_of_range_replacements_are_skipped() {
        let mut side = SideText::new("abcdef".to_owned());
        assert_eq!(
            side.replace(vec![
                (0..3, "X".to_owned()),
                (2..4, "Y".to_owned()),
                (5..99, "Z".to_owned()),
            ]),
            1
        );
        assert_eq!(side.text(), "Xdef");
        assert_eq!(side.replace(Vec::new()), 0);
        assert_eq!(side.undo(), Some(0));
        assert!(!side.can_undo());
    }

    #[test]
    fn editing_after_undo_does_not_reuse_a_discarded_save_point() {
        let mut side = SideText::new("a".to_owned());
        side.replace(vec![(0..1, "b".to_owned())]);
        side.mark_saved();
        side.undo();
        side.replace(vec![(0..1, "c".to_owned())]);
        assert!(side.is_modified(), "the disk holds b, not c");
        side.undo();
        assert!(side.is_modified());
    }

    #[test]
    fn a_save_of_an_older_revision_leaves_later_edits_unwritten() {
        let mut side = SideText::new("a".to_owned());
        side.replace(vec![(0..1, "b".to_owned())]);
        let sent = side.revision();
        side.replace(vec![(0..1, "c".to_owned())]);
        side.mark_saved_revision(sent);
        assert!(side.is_modified());
        let current = side.revision();
        side.mark_saved_revision(current);
        assert!(!side.is_modified());
    }

    #[test]
    fn a_range_that_splits_a_character_is_skipped() {
        let mut side = SideText::new("é".to_owned());
        assert_eq!(side.replace(vec![(1..2, "x".to_owned())]), 0);
        assert_eq!(side.text(), "é");
    }
}
