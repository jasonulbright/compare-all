//! A rope backed editable text buffer with grouped undo and change notifications.
//!
//! Every index taken by this module is clamped to the buffer, so no public call
//! can panic on an out of range index.
//!
//! Cost invariant: every edit, undo and redo does work proportional to the size
//! of the edited text plus `log n` in the size of the buffer. Nothing walks the
//! whole buffer, so a single keystroke costs the same in a buffer of a million
//! lines as in a buffer of ten.

use std::ops::{Deref, DerefMut, Range};
use std::sync::atomic::{AtomicU64, Ordering};

use ropey::{Rope, RopeSlice};
use thiserror::Error;

use crate::eol::{self, LineEnding};

static NEXT_REVISION: AtomicU64 = AtomicU64::new(1);

fn next_revision() -> u64 {
    NEXT_REVISION.fetch_add(1, Ordering::Relaxed)
}

/// Why an edit could not be applied to the rope.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Error)]
pub enum EditError {
    /// A recorded range no longer lies inside the buffer.
    #[error("edit range {at}..{end} lies outside a buffer of {len} characters")]
    OutOfBounds {
        /// Start of the range, in characters.
        at: usize,
        /// End of the range, in characters.
        end: usize,
        /// Length of the buffer, in characters.
        len: usize,
    },
}

/// A half open range of line indices.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LineRange {
    /// First line in the range.
    pub start: u32,
    /// One past the last line in the range.
    pub end: u32,
}

impl LineRange {
    /// A range covering `start` up to but excluding `end`.
    #[must_use]
    pub fn new(start: u32, end: u32) -> Self {
        Self {
            start,
            end: end.max(start),
        }
    }

    /// A range covering the single line `line`.
    #[must_use]
    pub fn single(line: u32) -> Self {
        Self {
            start: line,
            end: line.saturating_add(1),
        }
    }

    /// Number of lines covered.
    #[must_use]
    pub fn len(&self) -> u32 {
        self.end.saturating_sub(self.start)
    }

    /// True when the range covers no lines.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// What kind of user action produced an edit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EditKind {
    /// Character by character typing, which coalesces into one undo group.
    Typing,
    /// A deletion, which never coalesces with typing.
    Deletion,
    /// A whole command, which is always its own undo group.
    Command,
}

/// The lines an edit touched, for an incremental re-diff.
///
/// Lines `start_line ..= start_line + removed_lines` were replaced by lines
/// `start_line ..= start_line + inserted_lines`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Change {
    /// First line the edit touched, the same line before and after the edit.
    pub start_line: u32,
    /// Lines after `start_line` that the edit replaced.
    pub removed_lines: u32,
    /// Lines after `start_line` that replaced them.
    pub inserted_lines: u32,
}

/// One recorded edit.
///
/// Memory cost: an edit holds the removed and the inserted text in full, so a
/// transform that rewrites the whole buffer in one step costs roughly twice the
/// buffer size in history for that step alone.
#[derive(Clone, Debug)]
struct Edit {
    at: usize,
    removed: String,
    inserted: String,
}

#[derive(Clone, Debug)]
struct EditGroup {
    kind: EditKind,
    edits: Vec<Edit>,
}

/// An editable text buffer.
#[derive(Clone, Debug)]
pub struct TextBuffer {
    rope: Rope,
    revision: u64,
    undo: Vec<EditGroup>,
    redo: Vec<EditGroup>,
    saved_at: Option<usize>,
    open_depth: u32,
    open_group: Option<EditGroup>,
    sealed: bool,
    changes: Vec<Change>,
}

/// A content snapshot with its own edit history, prepared without copying the
/// live buffer's history. Its base revision identifies where the edits belong.
#[derive(Debug)]
pub struct EditSnapshot {
    base_revision: u64,
    buffer: Box<TextBuffer>,
}

impl EditSnapshot {
    /// The isolated buffer on which a worker prepares edits.
    pub fn buffer_mut(&mut self) -> &mut TextBuffer {
        &mut self.buffer
    }

    /// The isolated content, without the live undo history.
    #[must_use]
    pub fn buffer(&self) -> &TextBuffer {
        &self.buffer
    }
}

impl Default for TextBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl TextBuffer {
    /// An empty buffer.
    #[must_use]
    pub fn new() -> Self {
        Self::from_rope(Rope::new())
    }

    /// A buffer holding `text`, with no undo history and no unsaved changes.
    #[must_use]
    pub fn from_text(text: &str) -> Self {
        Self::from_rope(Rope::from_str(text))
    }

    /// A buffer taking ownership of an existing rope.
    #[must_use]
    pub fn from_rope(rope: Rope) -> Self {
        Self {
            rope,
            revision: next_revision(),
            undo: Vec::new(),
            redo: Vec::new(),
            saved_at: Some(0),
            open_depth: 0,
            open_group: None,
            sealed: true,
            changes: Vec::new(),
        }
    }

    /// The underlying rope.
    #[must_use]
    pub fn rope(&self) -> &Rope {
        &self.rope
    }

    /// Share the rope with a worker, starting a separate, empty edit history.
    /// Taking this snapshot does not walk the document or its undo records.
    #[must_use]
    pub fn edit_snapshot(&self) -> EditSnapshot {
        EditSnapshot {
            base_revision: self.revision,
            buffer: Box::new(Self::from_rope(self.rope.clone())),
        }
    }

    /// Adopt prepared edits if the live content has not changed. Existing undo
    /// records and the save point survive; the prepared history is appended.
    /// The returned old content and discarded redo records can be dropped on a
    /// worker instead of freeing a large document on the frame thread.
    ///
    /// # Errors
    /// Returns the unapplied snapshot when its base revision is stale.
    pub fn apply_snapshot(&mut self, mut snapshot: EditSnapshot) -> Result<Self, EditSnapshot> {
        if self.revision != snapshot.base_revision {
            return Err(snapshot);
        }
        let prepared = &mut snapshot.buffer;
        prepared.close_open_group();
        if prepared.undo.is_empty() {
            return Ok(*snapshot.buffer);
        }
        self.close_open_group();
        if self.saved_at.is_some_and(|at| at > self.undo.len()) {
            self.saved_at = None;
        }
        self.undo.append(&mut prepared.undo);
        std::mem::swap(&mut self.rope, &mut prepared.rope);
        std::mem::swap(&mut self.revision, &mut prepared.revision);
        if self.changes.is_empty() {
            std::mem::swap(&mut self.changes, &mut prepared.changes);
        } else {
            self.changes.append(&mut prepared.changes);
        }
        prepared.redo = std::mem::take(&mut self.redo);
        Ok(*snapshot.buffer)
    }

    /// The whole buffer as a string.
    #[must_use]
    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    /// Number of characters in the buffer.
    #[must_use]
    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    /// Number of bytes the buffer occupies as UTF-8.
    #[must_use]
    pub fn len_bytes(&self) -> usize {
        self.rope.len_bytes()
    }

    /// Number of lines, counting the empty line after a trailing line break.
    #[must_use]
    pub fn len_lines(&self) -> u32 {
        clamp_u32(self.rope.len_lines())
    }

    /// The line at `line`, including its terminator, or `None` when out of range.
    #[must_use]
    pub fn line(&self, line: u32) -> Option<RopeSlice<'_>> {
        if line >= self.len_lines() {
            return None;
        }
        self.rope.get_line(line as usize)
    }

    /// The line at `line` as a string, without its terminator.
    #[must_use]
    pub fn line_text(&self, line: u32) -> Option<String> {
        let slice = self.line(line)?;
        let text = slice.to_string();
        let trimmed = eol::lines(&text)
            .next()
            .map_or(String::new(), |(l, _)| l.to_owned());
        Some(trimmed)
    }

    /// The terminator of `line`, or `None` when the line is out of range.
    #[must_use]
    pub fn line_ending(&self, line: u32) -> Option<LineEnding> {
        let text = self.line(line)?.to_string();
        Some(
            eol::lines(&text)
                .next()
                .map_or(LineEnding::None, |(_, e)| e),
        )
    }

    /// The character index the line begins at, clamped to the buffer.
    #[must_use]
    pub fn line_to_char(&self, line: u32) -> usize {
        // Clamping to `len_lines` keeps the rope's own bounds check satisfied.
        let line = (line as usize).min(self.rope.len_lines());
        self.rope
            .try_line_to_char(line)
            .unwrap_or_else(|_| self.rope.len_chars())
    }

    /// The line a character index falls on, clamped to the buffer.
    #[must_use]
    pub fn char_to_line(&self, char_idx: usize) -> u32 {
        let idx = char_idx.min(self.rope.len_chars());
        clamp_u32(self.rope.try_char_to_line(idx).unwrap_or(0))
    }

    /// The character range a line range covers, including line terminators.
    #[must_use]
    pub fn line_char_range(&self, lines: LineRange) -> Range<usize> {
        let start = self.line_to_char(lines.start);
        let end = if lines.end >= self.len_lines() {
            self.rope.len_chars()
        } else {
            self.line_to_char(lines.end)
        };
        start..end.max(start)
    }

    /// The text of a character range, clamped to the buffer.
    #[must_use]
    pub fn slice_text(&self, range: Range<usize>) -> String {
        let range = self.clamp(range);
        self.rope.slice(range).to_string()
    }

    /// The text of a line range, including line terminators.
    #[must_use]
    pub fn line_range_text(&self, lines: LineRange) -> String {
        self.slice_text(self.line_char_range(lines))
    }

    fn clamp(&self, range: Range<usize>) -> Range<usize> {
        let len = self.rope.len_chars();
        let start = range.start.min(len);
        let end = range.end.min(len).max(start);
        start..end
    }

    // --- editing -----------------------------------------------------------

    /// Inserts `text` at a character index as one undo group.
    pub fn insert(&mut self, char_idx: usize, text: &str) {
        self.edit(char_idx..char_idx, text, EditKind::Command);
    }

    /// Inserts `text` at a character index, coalescing with adjacent typing.
    ///
    /// Consecutive insertions that continue where the previous one ended and
    /// carry no line break join the same undo group, so one undo removes a
    /// typed run rather than one character.
    pub fn insert_typed(&mut self, char_idx: usize, text: &str) {
        self.edit(char_idx..char_idx, text, EditKind::Typing);
    }

    /// Deletes a character range as one undo group.
    pub fn delete(&mut self, range: Range<usize>) {
        self.edit(range, "", EditKind::Deletion);
    }

    /// Replaces a character range with `text` as one undo group.
    pub fn replace(&mut self, range: Range<usize>, text: &str) {
        self.edit(range, text, EditKind::Command);
    }

    /// Replaces a line range with `text` as one undo group.
    ///
    /// `text` supplies its own line terminators.
    pub fn replace_lines(&mut self, lines: LineRange, text: &str) {
        let range = self.line_char_range(lines);
        self.edit(range, text, EditKind::Command);
    }

    /// Deletes a line range, terminators included, as one undo group.
    pub fn delete_lines(&mut self, lines: LineRange) {
        self.replace_lines(lines, "");
    }

    /// Opens an undo group that closes when the returned guard drops.
    ///
    /// This is the preferred grouping API: a guard cannot leave a group open, so
    /// the edits it covers always reach the undo stack.
    pub fn group(&mut self) -> UndoGroup<'_> {
        self.begin_group();
        UndoGroup { buffer: self }
    }

    /// Opens an undo group so every edit until the matching [`TextBuffer::end_group`]
    /// undoes together. Groups nest; only the outermost one closes the group.
    ///
    /// Prefer [`TextBuffer::group`], whose guard closes the group on drop.
    pub fn begin_group(&mut self) {
        if self.open_depth == 0 {
            self.open_group = Some(EditGroup {
                kind: EditKind::Command,
                edits: Vec::new(),
            });
        }
        self.open_depth = self.open_depth.saturating_add(1);
    }

    /// Closes the undo group opened by [`TextBuffer::begin_group`].
    pub fn end_group(&mut self) {
        self.open_depth = self.open_depth.saturating_sub(1);
        if self.open_depth == 0 {
            if let Some(group) = self.open_group.take() {
                if !group.edits.is_empty() {
                    self.push_group(group);
                }
            }
            self.sealed = true;
        }
    }

    /// Prevents the next edit from coalescing with the previous one.
    pub fn seal_group(&mut self) {
        self.sealed = true;
    }

    fn edit(&mut self, range: Range<usize>, insert: &str, kind: EditKind) {
        let range = self.clamp(range);
        if range.is_empty() && insert.is_empty() {
            return;
        }
        let removed = self.rope.slice(range.clone()).to_string();
        // The range is clamped, so the apply cannot fail; a failure would leave
        // the rope untouched and must not record an edit that did not happen.
        let Ok(change) = self.apply_tracked(range.start, removed.chars().count(), insert) else {
            return;
        };
        let edit = Edit {
            at: range.start,
            removed,
            inserted: insert.to_owned(),
        };
        self.record(edit, kind, change);
    }

    /// Applies an edit and reports every line it replaced.
    ///
    /// A CR at either edge of the edit can join with an LF or part from one,
    /// which changes the line count by one more or one less than the edit
    /// text holds. The window therefore comes from the rope's lines before
    /// and after the edit.
    fn apply_tracked(
        &mut self,
        at: usize,
        remove_chars: usize,
        insert: &str,
    ) -> Result<Change, EditError> {
        let before = (
            self.char_to_line(at),
            self.char_to_line(at.saturating_add(remove_chars)),
        );
        self.apply_raw(at, remove_chars, insert)?;
        let after = (
            self.char_to_line(at),
            self.char_to_line(at.saturating_add(insert.chars().count())),
        );
        let start_line = before.0.min(after.0);
        Ok(Change {
            start_line,
            removed_lines: before.1.saturating_sub(start_line),
            inserted_lines: after.1.saturating_sub(start_line),
        })
    }

    /// Applies an edit to the rope, leaving it untouched when the range does not fit.
    fn apply_raw(&mut self, at: usize, remove_chars: usize, insert: &str) -> Result<(), EditError> {
        let len = self.rope.len_chars();
        let end = at.saturating_add(remove_chars);
        if end > len {
            return Err(EditError::OutOfBounds { at, end, len });
        }
        if remove_chars > 0 {
            self.rope
                .try_remove(at..end)
                .map_err(|_| EditError::OutOfBounds { at, end, len })?;
        }
        if !insert.is_empty() {
            self.rope
                .try_insert(at, insert)
                .map_err(|_| EditError::OutOfBounds { at, end: at, len })?;
        }
        self.revision = next_revision();
        Ok(())
    }

    fn record(&mut self, edit: Edit, kind: EditKind, change: Change) {
        self.changes.push(change);
        self.redo.clear();
        if self.open_depth > 0 {
            if let Some(group) = self.open_group.as_mut() {
                group.edits.push(edit);
                return;
            }
        }
        if kind == EditKind::Typing && !self.sealed && self.can_coalesce(&edit) {
            if let Some(last) = self.undo.last_mut().and_then(|g| g.edits.last_mut()) {
                last.inserted.push_str(&edit.inserted);
                return;
            }
        }
        // A line break ends a typing run, so the next character starts its own group.
        self.sealed = kind != EditKind::Typing || edit.inserted.contains(['\r', '\n']);
        self.push_group(EditGroup {
            kind,
            edits: vec![edit],
        });
    }

    fn can_coalesce(&self, edit: &Edit) -> bool {
        if !edit.removed.is_empty() || edit.inserted.contains(['\r', '\n']) {
            return false;
        }
        let Some(group) = self.undo.last() else {
            return false;
        };
        if group.kind != EditKind::Typing {
            return false;
        }
        let Some(last) = group.edits.last() else {
            return false;
        };
        last.removed.is_empty() && last.at + last.inserted.chars().count() == edit.at
    }

    fn push_group(&mut self, group: EditGroup) {
        // A save point above the new top of the stack can never be reached again.
        if self.saved_at.is_some_and(|at| at > self.undo.len()) {
            self.saved_at = None;
        }
        self.undo.push(group);
    }

    // --- undo --------------------------------------------------------------

    /// Edits recorded in a group that is still open.
    fn pending_edits(&self) -> usize {
        self.open_group.as_ref().map_or(0, |g| g.edits.len())
    }

    /// Closes any open group, whatever its nesting depth.
    ///
    /// Undo and redo operate on whole groups, so an open group must reach the
    /// undo stack before either runs; otherwise they replay stack entries whose
    /// recorded ranges no longer describe the current text.
    fn close_open_group(&mut self) {
        self.open_depth = 0;
        if let Some(group) = self.open_group.take() {
            if !group.edits.is_empty() {
                self.push_group(group);
            }
        }
        self.sealed = true;
    }

    /// True when there is something to undo.
    #[must_use]
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty() || self.pending_edits() > 0
    }

    /// True when there is something to redo.
    #[must_use]
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Undoes one group. Returns false when the undo stack is empty.
    ///
    /// An open group is closed first, so its edits undo as one step.
    ///
    /// # Errors
    ///
    /// Returns [`EditError`] when a recorded range no longer fits the buffer.
    /// The buffer then keeps the text it had before the call.
    pub fn undo(&mut self) -> Result<bool, EditError> {
        self.undo_caret().map(|caret| caret.is_some())
    }

    /// Undo one group and return the character index reached by its final edit.
    ///
    /// # Errors
    /// Returns [`EditError`] when a recorded range no longer fits the buffer.
    pub fn undo_caret(&mut self) -> Result<Option<usize>, EditError> {
        self.close_open_group();
        let Some(group) = self.undo.pop() else {
            return Ok(None);
        };
        let mut caret = None;
        for edit in group.edits.iter().rev() {
            let inserted = edit.inserted.chars().count();
            let change = self.apply_tracked(edit.at, inserted, &edit.removed)?;
            caret = Some(edit.at + edit.removed.chars().count());
            self.changes.push(change);
        }
        self.redo.push(group);
        self.sealed = true;
        Ok(caret)
    }

    /// Redoes one group. Returns false when the redo stack is empty.
    ///
    /// An open group is closed first, so its edits are not replayed over.
    ///
    /// # Errors
    ///
    /// Returns [`EditError`] when a recorded range no longer fits the buffer.
    pub fn redo(&mut self) -> Result<bool, EditError> {
        self.redo_caret().map(|caret| caret.is_some())
    }

    /// Redo one group and return the character index reached by its final edit.
    ///
    /// # Errors
    /// Returns [`EditError`] when a recorded range no longer fits the buffer.
    pub fn redo_caret(&mut self) -> Result<Option<usize>, EditError> {
        self.close_open_group();
        let Some(group) = self.redo.pop() else {
            return Ok(None);
        };
        let mut caret = None;
        for edit in &group.edits {
            let removed = edit.removed.chars().count();
            let change = self.apply_tracked(edit.at, removed, &edit.inserted)?;
            caret = Some(edit.at + edit.inserted.chars().count());
            self.changes.push(change);
        }
        self.undo.push(group);
        self.sealed = true;
        Ok(caret)
    }

    /// Discards all undo and redo history, keeping the current text as saved.
    pub fn clear_history(&mut self) {
        self.open_depth = 0;
        self.open_group = None;
        self.undo.clear();
        self.redo.clear();
        self.saved_at = Some(0);
        self.sealed = true;
    }

    // --- modified state ----------------------------------------------------

    /// True when the text differs from the last saved state.
    ///
    /// Edits recorded in a group that is still open count as modifications.
    #[must_use]
    pub fn is_modified(&self) -> bool {
        self.saved_at != Some(self.undo.len()) || self.pending_edits() > 0
    }

    /// Records the current undo position as the saved state.
    pub fn mark_saved(&mut self) {
        self.saved_at = Some(self.undo.len());
        self.sealed = true;
    }

    /// Identity of this content revision, retained by clones and changed by
    /// edits, undo and redo. Capture it when starting an asynchronous save.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Complete a save without marking later edits as written. When the content
    /// moved on, discard the old save point too: it no longer describes disk.
    /// Undo after an intervening edit conservatively stays modified until saved.
    pub fn mark_saved_revision(&mut self, revision: u64) {
        if self.revision == revision {
            self.mark_saved();
        } else {
            self.saved_at = None;
        }
    }

    // --- change notifications ----------------------------------------------

    /// True when changes are waiting to be drained.
    #[must_use]
    pub fn has_changes(&self) -> bool {
        !self.changes.is_empty()
    }

    /// Takes the queued change notifications, leaving the queue empty.
    pub fn take_changes(&mut self) -> Vec<Change> {
        std::mem::take(&mut self.changes)
    }
}

/// A borrow of a buffer with an undo group open, closed when the guard drops.
///
/// Every [`TextBuffer`] method is reachable through the guard, and the edits
/// made through it undo as one step.
#[derive(Debug)]
pub struct UndoGroup<'a> {
    buffer: &'a mut TextBuffer,
}

impl Deref for UndoGroup<'_> {
    type Target = TextBuffer;

    fn deref(&self) -> &TextBuffer {
        self.buffer
    }
}

impl DerefMut for UndoGroup<'_> {
    fn deref_mut(&mut self) -> &mut TextBuffer {
        self.buffer
    }
}

impl Drop for UndoGroup<'_> {
    fn drop(&mut self) {
        self.buffer.end_group();
    }
}

fn clamp_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    #[test]
    fn prepared_edits_keep_the_live_save_point_and_redo_semantics() {
        let mut buffer = super::TextBuffer::from_text("cat cat\n");
        buffer.insert(0, "prefix ");
        buffer.mark_saved();
        let mut snapshot = buffer.edit_snapshot();
        assert!(snapshot.buffer.undo.is_empty() && snapshot.buffer.redo.is_empty());
        snapshot.buffer_mut().replace(7..10, "dog");
        assert!(buffer.apply_snapshot(snapshot).is_ok());
        assert!(buffer.is_modified());
        assert_eq!(buffer.undo(), Ok(true));
        assert_eq!(buffer.text(), "prefix cat cat\n");
        assert!(!buffer.is_modified());
        assert_eq!(buffer.redo(), Ok(true));
        assert_eq!(buffer.text(), "prefix dog cat\n");
        assert_eq!(buffer.undo(), Ok(true));
        assert_eq!(buffer.undo(), Ok(true));
        let mut snapshot = buffer.edit_snapshot();
        snapshot.buffer_mut().replace(0..3, "cow");
        assert!(buffer.apply_snapshot(snapshot).is_ok());
        assert!(!buffer.can_redo());
        assert_eq!(buffer.undo(), Ok(true));
        assert_eq!(buffer.text(), "cat cat\n");
        assert!(
            buffer.is_modified(),
            "the abandoned save point is no longer reachable"
        );
    }

    #[test]
    fn prepared_edits_reject_a_different_revision_even_with_identical_text() {
        let mut buffer = super::TextBuffer::from_text("cat\n");
        let mut snapshot = buffer.edit_snapshot();
        snapshot.buffer_mut().replace(0..3, "cow");
        buffer.insert(0, "x");
        assert_eq!(buffer.undo(), Ok(true));
        assert!(buffer.apply_snapshot(snapshot).is_err());
        assert_eq!(buffer.text(), "cat\n");
    }
    use super::*;

    #[test]
    fn line_access_is_indexed_by_u32() {
        let buf = TextBuffer::from_text("one\ntwo\r\nthree");
        assert_eq!(buf.len_lines(), 3);
        assert_eq!(buf.line_text(1).unwrap(), "two");
        assert_eq!(buf.line_ending(1).unwrap(), LineEnding::CrLf);
        assert_eq!(buf.line_ending(2).unwrap(), LineEnding::None);
        assert!(buf.line(9).is_none());
    }

    #[test]
    fn out_of_range_indices_are_clamped() {
        let mut buf = TextBuffer::from_text("abc");
        buf.insert(999, "d");
        assert_eq!(buf.text(), "abcd");
        buf.delete(2..999);
        assert_eq!(buf.text(), "ab");
        assert_eq!(buf.slice_text(50..60), "");
        assert_eq!(buf.char_to_line(999), 0);
    }

    #[test]
    fn replace_by_line_range() {
        let mut buf = TextBuffer::from_text("a\nb\nc\n");
        buf.replace_lines(LineRange::new(1, 2), "B\n");
        assert_eq!(buf.text(), "a\nB\nc\n");
    }

    #[test]
    fn typing_coalesces_into_one_group() {
        let mut buf = TextBuffer::new();
        for (i, c) in "hello".chars().enumerate() {
            buf.insert_typed(i, &c.to_string());
        }
        assert_eq!(buf.text(), "hello");
        assert!(buf.undo().unwrap());
        assert_eq!(buf.text(), "");
        assert!(!buf.can_undo());
    }

    #[test]
    fn typing_breaks_at_a_line_break() {
        let mut buf = TextBuffer::new();
        buf.insert_typed(0, "a");
        buf.insert_typed(1, "\n");
        buf.insert_typed(2, "b");
        assert!(buf.undo().unwrap());
        assert_eq!(buf.text(), "a\n");
        assert!(buf.undo().unwrap());
        assert_eq!(buf.text(), "a");
    }

    #[test]
    fn typing_breaks_when_the_caret_jumps() {
        let mut buf = TextBuffer::from_text("xy");
        buf.insert_typed(0, "a");
        buf.insert_typed(2, "b");
        assert!(buf.undo().unwrap());
        assert_eq!(buf.text(), "axy");
    }

    #[test]
    fn explicit_group_undoes_together() {
        let mut buf = TextBuffer::from_text("a\nb\n");
        buf.begin_group();
        buf.replace_lines(LineRange::single(0), "A\n");
        buf.replace_lines(LineRange::single(1), "B\n");
        buf.end_group();
        assert_eq!(buf.text(), "A\nB\n");
        assert!(buf.undo().unwrap());
        assert_eq!(buf.text(), "a\nb\n");
    }

    #[test]
    fn redo_restores_the_edit() {
        let mut buf = TextBuffer::from_text("a");
        buf.insert(1, "b");
        assert!(buf.undo().unwrap());
        assert_eq!(buf.text(), "a");
        assert!(buf.redo().unwrap());
        assert_eq!(buf.text(), "ab");
    }

    #[test]
    fn a_new_edit_clears_redo() {
        let mut buf = TextBuffer::from_text("a");
        buf.insert(1, "b");
        assert!(buf.undo().unwrap());
        buf.insert(1, "c");
        assert!(!buf.can_redo());
    }

    #[test]
    fn modified_flag_tracks_the_saved_position() {
        let mut buf = TextBuffer::from_text("a");
        assert!(!buf.is_modified());
        buf.insert(1, "b");
        assert!(buf.is_modified());
        assert!(buf.undo().unwrap());
        assert!(!buf.is_modified());
        buf.insert(1, "c");
        buf.mark_saved();
        assert!(!buf.is_modified());
        assert!(buf.undo().unwrap());
        assert!(buf.is_modified());
    }

    #[test]
    fn saving_seals_the_typing_group() {
        let mut buf = TextBuffer::new();
        buf.insert_typed(0, "a");
        buf.mark_saved();
        buf.insert_typed(1, "b");
        assert!(buf.is_modified());
        assert!(buf.undo().unwrap());
        assert!(!buf.is_modified());
        assert_eq!(buf.text(), "a");
    }

    #[test]
    fn changes_name_the_affected_lines() {
        let mut buf = TextBuffer::from_text("a\nb\nc\n");
        let _ = buf.take_changes();
        buf.replace_lines(LineRange::new(1, 2), "B\nB2\n");
        let changes = buf.take_changes();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].start_line, 1);
        assert_eq!(changes[0].removed_lines, 1);
        assert_eq!(changes[0].inserted_lines, 2);
        assert!(!buf.has_changes());
    }

    fn assert_change_covers(before: &str, after: &str, change: Change) {
        let lines = |text: &str| -> Vec<String> {
            let buffer = TextBuffer::from_text(text);
            (0..buffer.len_lines())
                .map(|line| buffer.line(line).unwrap().to_string())
                .collect()
        };
        let old = lines(before);
        let new = lines(after);
        let start = change.start_line as usize;
        let old_end = start + change.removed_lines as usize + 1;
        let new_end = start + change.inserted_lines as usize + 1;
        let context = format!("{before:?} -> {after:?}: {change:?}");
        assert!(old_end <= old.len() && new_end <= new.len(), "{context}");
        assert_eq!(old[..start], new[..start], "{context}");
        assert_eq!(old[old_end..], new[new_end..], "{context}");
    }

    #[test]
    fn changes_name_every_line_an_edit_joins_or_splits_at_a_carriage_return() {
        let cases: [(&str, std::ops::Range<usize>, &str); 7] = [
            ("a\rb\rc\r", 2..2, "\nx"),
            ("x\n\ny\n", 2..2, "q\r"),
            ("p\rq\nz\n", 2..3, ""),
            ("a\r\nb\n", 2..3, ""),
            ("a\r\nb\n", 2..2, "z"),
            ("k\r\n\nm\r", 3..4, "Q"),
            ("a\nb\nc\n", 2..2, "X\nY"),
        ];
        for (before, range, inserted) in cases {
            let mut buf = TextBuffer::from_text(before);
            buf.replace(range, inserted);
            let after = buf.text();
            let changes = buf.take_changes();
            assert_eq!(changes.len(), 1);
            assert_change_covers(before, &after, changes[0]);
            assert!(buf.undo().unwrap());
            assert_eq!(buf.text(), before);
            let changes = buf.take_changes();
            assert_eq!(changes.len(), 1);
            assert_change_covers(&after, before, changes[0]);
            assert!(buf.redo().unwrap());
            let changes = buf.take_changes();
            assert_eq!(changes.len(), 1);
            assert_change_covers(before, &after, changes[0]);
        }
    }

    #[test]
    fn undo_inside_an_open_group_does_not_corrupt_the_rope() {
        let mut buf = TextBuffer::from_text("abc");
        buf.insert(3, "X");
        buf.begin_group();
        buf.insert(4, "Y");
        assert!(buf.can_undo());
        assert!(buf.is_modified());
        assert!(buf.undo().unwrap());
        assert_eq!(buf.text(), "abcX");
        buf.end_group();
        assert!(buf.undo().unwrap());
        assert_eq!(buf.text(), "abc");
    }

    #[test]
    fn a_never_closed_group_keeps_its_history() {
        let mut buf = TextBuffer::from_text("a");
        {
            let mut group = buf.group();
            group.insert(1, "b");
            group.insert(2, "c");
        }
        assert_eq!(buf.text(), "abc");
        assert!(buf.undo().unwrap());
        assert_eq!(buf.text(), "a");
    }

    #[test]
    fn large_buffer_edits_work() {
        let mut text = String::with_capacity(1_200_000);
        for i in 0..100_000 {
            text.push_str("line ");
            text.push_str(&i.to_string());
            text.push('\n');
        }
        let mut buf = TextBuffer::from_text(&text);
        assert_eq!(buf.len_lines(), 100_001);
        let at = buf.line_to_char(50_000);
        buf.insert_typed(at, "x");
        assert_eq!(buf.line_text(50_000).unwrap(), "xline 50000");
        assert!(buf.undo().unwrap());
        assert_eq!(buf.line_text(50_000).unwrap(), "line 50000");
    }
}
