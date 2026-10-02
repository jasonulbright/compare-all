//! The editing model of one pane: a buffer, a caret, a selection and the
//! operations that move or change them.
//!
//! Nothing here paints or reads input. A view converts a keystroke into a
//! [`Motion`] or an edit call and reads the result back, so every rule about
//! where the caret lands is testable without a frame.
//!
//! Two column systems meet here. A caret holds a character offset into its
//! line, because that is what an edit needs. Painting and mouse placement need
//! a display column instead, which counts a tab as the distance to the next tab
//! stop and every other character as [`ca_text::display_width`] reports. The
//! conversion between the two lives in [`display_column`] and
//! [`index_at_column`] and costs the length of the prefix, never the buffer.

use ca_text::{LineRange, TabSettings, TextBuffer};
use unicode_segmentation::{GraphemeCursor, GraphemeIncomplete};

/// A caret position inside a buffer.
///
/// `index` counts characters from the start of the line, not bytes and not
/// display columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Caret {
    /// Zero based line number.
    pub line: u32,
    /// Characters from the start of the line.
    pub index: u32,
}

impl Caret {
    /// A caret at `line` and `index`.
    #[must_use]
    pub const fn new(line: u32, index: u32) -> Self {
        Self { line, index }
    }
}

/// A caret movement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    /// One extended grapheme towards the start of the buffer.
    Left,
    /// One extended grapheme towards the end of the buffer.
    Right,
    /// To the start of the word at or before the caret.
    WordLeft,
    /// To the start of the word after the caret.
    WordRight,
    /// One line up, keeping the display column where it can.
    Up,
    /// One line down, keeping the display column where it can.
    Down,
    /// To the first character of the line.
    LineStart,
    /// Past the last character of the line.
    LineEnd,
    /// The given number of lines up.
    PageUp(u32),
    /// The given number of lines down.
    PageDown(u32),
    /// To the first character of the buffer.
    DocumentStart,
    /// Past the last character of the buffer.
    DocumentEnd,
}

impl Motion {
    /// True when the motion keeps the display column of the previous motion.
    const fn keeps_goal_column(self) -> bool {
        matches!(
            self,
            Motion::Up | Motion::Down | Motion::PageUp(_) | Motion::PageDown(_)
        )
    }
}

/// The three character classes word movement distinguishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Space,
    Word,
    Other,
}

fn class_of(character: char) -> CharClass {
    if character.is_whitespace() {
        CharClass::Space
    } else if character.is_alphanumeric() || character == '_' {
        CharClass::Word
    } else {
        CharClass::Other
    }
}

/// The display column a character offset sits at.
#[must_use]
pub fn display_column(line: &str, index: u32, tabs: TabSettings) -> u32 {
    let stop = tabs.stop();
    let mut column = 0usize;
    for (count, character) in line.chars().enumerate() {
        if count as u64 >= u64::from(index) {
            break;
        }
        column = advance(column, character, stop);
    }
    u32::try_from(column).unwrap_or(u32::MAX)
}

/// The character offset a display column falls on.
///
/// A column inside a tab's run resolves to the offset of that tab, so a click
/// anywhere in the run places the caret before the tab.
#[must_use]
pub fn index_at_column(line: &str, column: u32, tabs: TabSettings) -> u32 {
    let stop = tabs.stop();
    let target = column as usize;
    let mut at = 0usize;
    for (count, character) in line.chars().enumerate() {
        let next = advance(at, character, stop);
        if next > target {
            return u32::try_from(count).unwrap_or(u32::MAX);
        }
        at = next;
    }
    u32::try_from(line.chars().count()).unwrap_or(u32::MAX)
}

fn advance(column: usize, character: char, stop: usize) -> usize {
    if character == '\t' {
        column / stop * stop + stop
    } else {
        column.saturating_add(ca_text::display_width(character))
    }
}

/// How far an edit reached, so a view can keep its own mirrors in step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditSpan {
    /// First line the edit touched.
    pub start_line: u32,
    /// Lines the edit removed, counted before the edit.
    pub removed_lines: u32,
    /// Lines the edit inserted, counted after the edit.
    pub inserted_lines: u32,
}

/// One pane's editable state.
#[derive(Debug)]
pub struct Pane {
    buffer: TextBuffer,
    caret: Caret,
    anchor: Caret,
    goal_column: Option<u32>,
    overwrite: bool,
    read_only: bool,
    tabs: TabSettings,
}

impl Default for Pane {
    fn default() -> Self {
        Self::new(TextBuffer::new())
    }
}

impl Pane {
    /// A pane over `buffer`, with the caret at the start.
    #[must_use]
    pub fn new(buffer: TextBuffer) -> Self {
        Self {
            buffer,
            caret: Caret::default(),
            anchor: Caret::default(),
            goal_column: None,
            overwrite: false,
            read_only: false,
            tabs: TabSettings::default(),
        }
    }

    /// A pane over the given text.
    #[must_use]
    pub fn from_text(text: &str) -> Self {
        Self::new(TextBuffer::from_text(text))
    }

    /// The buffer behind the pane.
    #[must_use]
    pub const fn buffer(&self) -> &TextBuffer {
        &self.buffer
    }

    /// The buffer behind the pane, for a caller that edits it directly.
    pub fn buffer_mut(&mut self) -> &mut TextBuffer {
        &mut self.buffer
    }

    /// Replace the whole buffer, resetting the caret and the history.
    pub fn reset(&mut self, buffer: TextBuffer) {
        self.buffer = buffer;
        self.caret = Caret::default();
        self.anchor = Caret::default();
        self.goal_column = None;
    }

    /// The tab settings the column arithmetic uses.
    #[must_use]
    pub const fn tabs(&self) -> TabSettings {
        self.tabs
    }

    /// Set the tab settings.
    pub fn set_tabs(&mut self, tabs: TabSettings) {
        self.tabs = tabs;
    }

    /// True when the pane refuses every edit.
    #[must_use]
    pub const fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Set whether the pane refuses every edit.
    pub fn set_read_only(&mut self, read_only: bool) {
        self.read_only = read_only;
    }

    /// True when typing replaces the character under the caret.
    #[must_use]
    pub const fn is_overwrite(&self) -> bool {
        self.overwrite
    }

    /// Swap between inserting and overwriting.
    pub fn toggle_overwrite(&mut self) {
        self.overwrite = !self.overwrite;
    }

    /// Where the caret sits.
    #[must_use]
    pub const fn caret(&self) -> Caret {
        self.caret
    }

    /// Where the selection was started.
    #[must_use]
    pub const fn anchor(&self) -> Caret {
        self.anchor
    }

    /// The selection in buffer order, or `None` when nothing is selected.
    #[must_use]
    pub fn selection(&self) -> Option<(Caret, Caret)> {
        if self.anchor == self.caret {
            return None;
        }
        if self.anchor < self.caret {
            Some((self.anchor, self.caret))
        } else {
            Some((self.caret, self.anchor))
        }
    }

    /// The selected text, or `None` when nothing is selected.
    #[must_use]
    pub fn selected_text(&self) -> Option<String> {
        let (start, end) = self.selection()?;
        Some(
            self.buffer
                .slice_text(self.char_of(start)..self.char_of(end)),
        )
    }

    /// The word the caret is in or directly after, or `None` when the caret
    /// touches no word character.
    #[must_use]
    pub fn word_at_caret(&self) -> Option<String> {
        let characters: Vec<char> = self.line_text(self.caret.line).chars().collect();
        let is_word = |at: usize| {
            characters
                .get(at)
                .is_some_and(|character| class_of(*character) == CharClass::Word)
        };
        let caret = (self.caret.index as usize).min(characters.len());
        let inside = if is_word(caret) {
            caret
        } else if caret > 0 && is_word(caret - 1) {
            caret - 1
        } else {
            return None;
        };
        let mut start = inside;
        while start > 0 && is_word(start - 1) {
            start -= 1;
        }
        let mut end = inside;
        while is_word(end) {
            end += 1;
        }
        characters.get(start..end).map(|run| run.iter().collect())
    }

    /// The lines the selection touches, or the caret's line when there is none.
    ///
    /// A selection that ends at the first character of a line does not claim
    /// that line, so selecting three whole lines yields three lines rather than
    /// four.
    #[must_use]
    pub fn selected_lines(&self) -> LineRange {
        match self.selection() {
            None => LineRange::single(self.caret.line),
            Some((start, end)) => {
                let last = if end.index == 0 && end.line > start.line {
                    end.line - 1
                } else {
                    end.line
                };
                LineRange::new(start.line, last.saturating_add(1))
            }
        }
    }

    /// Number of lines the buffer holds.
    #[must_use]
    pub fn line_count(&self) -> u32 {
        self.buffer.len_lines()
    }

    /// The text of one line, without its terminator.
    #[must_use]
    pub fn line_text(&self, line: u32) -> String {
        self.buffer.line_text(line).unwrap_or_default()
    }

    fn line_len(&self, line: u32) -> u32 {
        let Some(text) = self.buffer.line(line) else {
            return 0;
        };
        let mut len = text.len_chars();
        if len > 0 && text.char(len - 1) == '\n' {
            len -= 1;
        }
        if len > 0 && text.char(len - 1) == '\r' {
            len -= 1;
        }
        u32::try_from(len).unwrap_or(u32::MAX)
    }

    fn last_line(&self) -> u32 {
        self.buffer.len_lines().saturating_sub(1)
    }

    fn clamp(&self, caret: Caret) -> Caret {
        let line = caret.line.min(self.last_line());
        Caret {
            line,
            index: caret.index.min(self.line_len(line)),
        }
    }

    fn grapheme_edge(&self, caret: Caret, forward: bool, advance: bool) -> Caret {
        let caret = self.clamp(caret);
        let Some(line) = self.buffer.line(caret.line) else {
            return caret;
        };
        let line = line.slice(..self.line_len(caret.line) as usize);
        if line.len_bytes() == 0 {
            return caret;
        }
        let offset = line.char_to_byte(caret.index as usize);
        let mut cursor = GraphemeCursor::new(offset, line.len_bytes(), true);
        let (mut chunk, mut start, _, _) = line.chunk_at_byte(offset);
        let mut check = !advance;
        let boundary = loop {
            let result = if check {
                cursor
                    .is_boundary(chunk, start)
                    .map(|boundary| boundary.then_some(offset))
            } else if forward {
                cursor.next_boundary(chunk, start)
            } else {
                cursor.prev_boundary(chunk, start)
            };
            match result {
                Ok(Some(boundary)) => break boundary,
                Ok(None) if check => check = false,
                Ok(None) => break if forward { line.len_bytes() } else { 0 },
                Err(GraphemeIncomplete::PreContext(end)) => {
                    let (context, context_start, _, _) = line.chunk_at_byte(end.saturating_sub(1));
                    cursor.provide_context(&context[..end - context_start], context_start);
                }
                Err(GraphemeIncomplete::NextChunk) => {
                    let held = line.chunk_at_byte(start + chunk.len());
                    chunk = held.0;
                    start = held.1;
                }
                Err(GraphemeIncomplete::PrevChunk) => {
                    let held = line.chunk_at_byte(start.saturating_sub(1));
                    chunk = held.0;
                    start = held.1;
                }
                Err(GraphemeIncomplete::InvalidOffset) => {
                    break if forward { line.len_bytes() } else { 0 }
                }
            }
        };
        Caret::new(
            caret.line,
            u32::try_from(line.byte_to_char(boundary)).unwrap_or(u32::MAX),
        )
    }

    /// The character index of a caret in the whole buffer.
    #[must_use]
    pub fn char_of(&self, caret: Caret) -> usize {
        let caret = self.clamp(caret);
        self.buffer
            .line_to_char(caret.line)
            .saturating_add(caret.index as usize)
    }

    /// The caret a whole-buffer character index names.
    #[must_use]
    pub fn caret_of(&self, char_index: usize) -> Caret {
        let line = self.buffer.char_to_line(char_index);
        let start = self.buffer.line_to_char(line);
        let index = u32::try_from(char_index.saturating_sub(start)).unwrap_or(u32::MAX);
        self.clamp(Caret { line, index })
    }

    /// The display column the caret sits at.
    #[must_use]
    pub fn caret_column(&self) -> u32 {
        display_column(
            &self.line_text(self.caret.line),
            self.caret.index,
            self.tabs,
        )
    }

    /// Place the caret, extending the selection or dropping it.
    pub fn place(&mut self, caret: Caret, extend: bool) {
        self.caret = self.grapheme_edge(caret, extend && caret > self.anchor, false);
        if !extend {
            self.anchor = self.caret;
        }
        self.goal_column = None;
    }

    /// Place the caret at a line and a display column, as a click does.
    pub fn place_at_column(&mut self, line: u32, column: u32, extend: bool) {
        let line = line.min(self.last_line());
        let index = index_at_column(&self.line_text(line), column, self.tabs);
        self.place(Caret { line, index }, extend);
    }

    /// Select everything.
    pub fn select_all(&mut self) {
        self.anchor = Caret::default();
        let line = self.last_line();
        self.caret = Caret {
            line,
            index: self.line_len(line),
        };
        self.goal_column = None;
    }

    /// Select a line range in full.
    pub fn select_lines(&mut self, lines: LineRange) {
        let last = lines.end.saturating_sub(1).min(self.last_line());
        self.anchor = Caret::new(lines.start.min(self.last_line()), 0);
        self.caret = Caret::new(last, self.line_len(last));
        self.goal_column = None;
    }

    /// Drop the selection, leaving the caret where it is.
    pub fn clear_selection(&mut self) {
        self.anchor = self.caret;
    }

    /// Move the caret.
    pub fn move_caret(&mut self, motion: Motion, extend: bool) {
        let goal = if motion.keeps_goal_column() {
            Some(self.goal_column.unwrap_or_else(|| self.caret_column()))
        } else {
            None
        };
        let target = self.target_of(motion, goal);
        self.caret = self.grapheme_edge(target, motion == Motion::WordRight, false);
        if !extend {
            self.anchor = self.caret;
        }
        self.goal_column = goal;
    }

    fn target_of(&self, motion: Motion, goal: Option<u32>) -> Caret {
        let caret = self.caret;
        match motion {
            Motion::Left => {
                if caret.index > 0 {
                    self.grapheme_edge(caret, false, true)
                } else if caret.line > 0 {
                    Caret::new(caret.line - 1, self.line_len(caret.line - 1))
                } else {
                    caret
                }
            }
            Motion::Right => {
                if caret.index < self.line_len(caret.line) {
                    self.grapheme_edge(caret, true, true)
                } else if caret.line < self.last_line() {
                    Caret::new(caret.line + 1, 0)
                } else {
                    caret
                }
            }
            Motion::WordLeft => self.word_left(caret),
            Motion::WordRight => self.word_right(caret),
            Motion::Up => self.vertical(caret, caret.line.saturating_sub(1), goal),
            Motion::Down => self.vertical(caret, caret.line.saturating_add(1), goal),
            Motion::PageUp(rows) => {
                self.vertical(caret, caret.line.saturating_sub(rows.max(1)), goal)
            }
            Motion::PageDown(rows) => {
                self.vertical(caret, caret.line.saturating_add(rows.max(1)), goal)
            }
            Motion::LineStart => Caret::new(caret.line, 0),
            Motion::LineEnd => Caret::new(caret.line, self.line_len(caret.line)),
            Motion::DocumentStart => Caret::default(),
            Motion::DocumentEnd => {
                let line = self.last_line();
                Caret::new(line, self.line_len(line))
            }
        }
    }

    fn vertical(&self, caret: Caret, line: u32, goal: Option<u32>) -> Caret {
        let line = line.min(self.last_line());
        let column = goal
            .unwrap_or_else(|| display_column(&self.line_text(caret.line), caret.index, self.tabs));
        Caret::new(
            line,
            index_at_column(&self.line_text(line), column, self.tabs),
        )
    }

    fn word_left(&self, caret: Caret) -> Caret {
        if caret.index == 0 {
            return if caret.line == 0 {
                caret
            } else {
                Caret::new(caret.line - 1, self.line_len(caret.line - 1))
            };
        }
        let characters: Vec<char> = self.line_text(caret.line).chars().collect();
        let mut at = caret.index as usize;
        while at > 0 && characters.get(at - 1).copied().map(class_of) == Some(CharClass::Space) {
            at -= 1;
        }
        let Some(class) = characters.get(at.saturating_sub(1)).copied().map(class_of) else {
            return Caret::new(caret.line, 0);
        };
        while at > 0 && characters.get(at - 1).copied().map(class_of) == Some(class) {
            at -= 1;
        }
        Caret::new(caret.line, u32::try_from(at).unwrap_or(0))
    }

    fn word_right(&self, caret: Caret) -> Caret {
        let characters: Vec<char> = self.line_text(caret.line).chars().collect();
        if caret.index as usize >= characters.len() {
            return if caret.line < self.last_line() {
                Caret::new(caret.line + 1, 0)
            } else {
                caret
            };
        }
        let mut at = caret.index as usize;
        if let Some(class) = characters.get(at).copied().map(class_of) {
            if class != CharClass::Space {
                while characters.get(at).copied().map(class_of) == Some(class) {
                    at += 1;
                }
            }
        }
        while characters.get(at).copied().map(class_of) == Some(CharClass::Space) {
            at += 1;
        }
        Caret::new(caret.line, u32::try_from(at).unwrap_or(0))
    }

    // --- editing -----------------------------------------------------------

    fn delete_selection_chars(&mut self) -> bool {
        let Some((start, end)) = self.selection() else {
            return false;
        };
        let range = self.char_of(start)..self.char_of(end);
        self.buffer.delete(range);
        self.caret = self.grapheme_edge(start, false, false);
        self.anchor = self.caret;
        true
    }

    /// Insert text at the caret, replacing the selection.
    ///
    /// The whole call is one undo group, so a paste undoes in one step.
    pub fn insert(&mut self, text: &str) {
        if self.read_only || text.is_empty() {
            return;
        }
        let at = self.char_of(self.selection().map_or(self.caret, |(start, _)| start));
        self.buffer.begin_group();
        self.delete_selection_chars();
        self.buffer.insert(at, text);
        self.buffer.end_group();
        let moved = at.saturating_add(text.chars().count());
        self.caret = self.grapheme_edge(self.caret_of(moved), true, false);
        self.anchor = self.caret;
        self.goal_column = None;
    }

    /// Type one character at the caret.
    ///
    /// Consecutive characters join one undo group. In overwrite mode the
    /// grapheme under the caret is replaced instead, except at a line end.
    pub fn type_character(&mut self, character: char) {
        if self.read_only {
            return;
        }
        let mut text = [0u8; 4];
        let text = character.encode_utf8(&mut text);
        if self.selection().is_some() {
            self.insert(text);
            return;
        }
        let at = self.char_of(self.caret);
        if self.overwrite && self.caret.index < self.line_len(self.caret.line) {
            let after = self.char_of(self.target_of(Motion::Right, None));
            self.buffer.replace(at..after, text);
        } else {
            self.buffer.insert_typed(at, text);
        }
        self.caret = self.grapheme_edge(self.caret_of(at.saturating_add(1)), true, false);
        self.anchor = self.caret;
        self.goal_column = None;
    }

    /// Break the line at the caret, replacing the selection.
    pub fn enter(&mut self, ending: &str) {
        let (start, end) = self.selection().unwrap_or((self.caret, self.caret));
        let at = self.char_of(start);
        let after = self.char_of(end);
        let joins_previous = ending == "\n" && at > 0 && self.buffer.slice_text(at - 1..at) == "\r";
        let joins_next = ending == "\r"
            && after < self.buffer.len_chars()
            && self.buffer.slice_text(after..after + 1) == "\n";
        self.insert(if joins_previous || joins_next {
            "\r\n"
        } else {
            ending
        });
    }

    /// Delete the selection, or the grapheme before the caret.
    pub fn backspace(&mut self) {
        if self.read_only {
            return;
        }
        if self.delete_selection_chars() {
            self.goal_column = None;
            return;
        }
        let at = self.char_of(self.caret);
        if at == 0 {
            return;
        }
        let before = self.char_of(self.target_of(Motion::Left, None));
        self.buffer.delete(before..at);
        self.caret = self.grapheme_edge(self.caret_of(before), false, false);
        self.anchor = self.caret;
        self.goal_column = None;
    }

    /// Delete the selection, or the grapheme after the caret.
    pub fn delete(&mut self) {
        if self.read_only {
            return;
        }
        if self.delete_selection_chars() {
            self.goal_column = None;
            return;
        }
        let at = self.char_of(self.caret);
        if at >= self.buffer.len_chars() {
            return;
        }
        let after = self.char_of(self.target_of(Motion::Right, None));
        self.buffer.delete(at..after);
        self.caret = self.grapheme_edge(self.caret, false, false);
        self.anchor = self.caret;
        self.goal_column = None;
    }

    /// Delete the selection, or the word on one side of the caret.
    ///
    /// Returns true when the text changed.
    pub fn delete_word(&mut self, forward: bool) -> bool {
        if self.read_only {
            return false;
        }
        if self.delete_selection_chars() {
            self.goal_column = None;
            return true;
        }
        let from = self.caret;
        self.move_caret(
            if forward {
                Motion::WordRight
            } else {
                Motion::WordLeft
            },
            false,
        );
        let to = self.caret;
        self.caret = from;
        self.anchor = from;
        self.delete_to(to)
    }

    /// Delete everything between the caret and `to`, leaving the caret at the
    /// lower of the two positions.
    ///
    /// Returns true when the text changed.
    pub fn delete_to(&mut self, to: Caret) -> bool {
        if self.read_only {
            return false;
        }
        let here = self.char_of(self.caret);
        let there = self.char_of(self.grapheme_edge(to, to > self.caret, false));
        let (start, end) = if here <= there {
            (here, there)
        } else {
            (there, here)
        };
        if end <= start {
            return false;
        }
        self.buffer.delete(start..end);
        self.caret = self.grapheme_edge(self.caret_of(start), false, false);
        self.anchor = self.caret;
        self.goal_column = None;
        true
    }

    /// Delete the caret's line whole.
    pub fn delete_line(&mut self) {
        if self.read_only {
            return;
        }
        self.buffer.delete_lines(LineRange::single(self.caret.line));
        self.caret = self.clamp(Caret::new(self.caret.line, 0));
        self.anchor = self.caret;
    }

    /// Delete from the caret back to the first column.
    pub fn delete_to_line_start(&mut self) {
        if self.read_only || self.caret.index == 0 {
            return;
        }
        let end = self.char_of(self.caret);
        let start = self.buffer.line_to_char(self.caret.line);
        self.buffer.delete(start..end);
        self.caret = Caret::new(self.caret.line, 0);
        self.anchor = self.caret;
    }

    /// Delete from the caret to the end of the line, terminator excluded.
    pub fn delete_to_line_end(&mut self) {
        if self.read_only {
            return;
        }
        let start = self.char_of(self.caret);
        let end = self.char_of(Caret::new(self.caret.line, self.line_len(self.caret.line)));
        if end > start {
            self.buffer.delete(start..end);
        }
        self.anchor = self.caret;
    }

    /// Indent every touched line by one level, as one undo group.
    pub fn increase_indent(&mut self) {
        if self.read_only {
            return;
        }
        let lines = self.selected_lines();
        ca_text::increase_indent(&mut self.buffer, lines, self.tabs);
        self.reselect(lines);
    }

    /// Remove one indent level from every touched line, as one undo group.
    pub fn decrease_indent(&mut self) {
        if self.read_only {
            return;
        }
        let lines = self.selected_lines();
        ca_text::decrease_indent(&mut self.buffer, lines, self.tabs);
        self.reselect(lines);
    }

    fn reselect(&mut self, lines: LineRange) {
        if self.selection().is_some() {
            self.select_lines(lines);
        } else {
            self.caret = self.grapheme_edge(self.caret, false, false);
            self.anchor = self.caret;
        }
    }

    /// The Tab key: indent a multi-line selection, otherwise insert one level.
    pub fn tab(&mut self) {
        if self.read_only {
            return;
        }
        let spans_lines = self
            .selection()
            .is_some_and(|(start, end)| start.line != end.line);
        if spans_lines {
            self.increase_indent();
        } else {
            let indent = self.tabs.indent_text();
            self.insert(&indent);
        }
    }

    /// The text a copy would place on the clipboard.
    #[must_use]
    pub fn copy(&self) -> Option<String> {
        self.selected_text()
    }

    /// Remove the selection and report what it held.
    pub fn cut(&mut self) -> Option<String> {
        let text = self.selected_text()?;
        if self.read_only {
            return Some(text);
        }
        self.delete_selection_chars();
        Some(text)
    }

    /// Insert clipboard text at the caret as one undo group.
    pub fn paste(&mut self, text: &str) {
        self.insert(text);
    }

    /// Replace a line range as one undo group and leave the caret on the first
    /// line of the replacement.
    pub fn replace_lines(&mut self, lines: LineRange, text: &str) {
        if self.read_only {
            return;
        }
        self.buffer.replace_lines(lines, text);
        self.caret = self.clamp(Caret::new(lines.start, 0));
        self.anchor = self.caret;
    }

    /// Undo one group and place the caret where it reached.
    pub fn undo(&mut self) -> bool {
        if self.read_only {
            return false;
        }
        let caret = self.buffer.undo_caret().unwrap_or(None);
        self.after_history(caret);
        caret.is_some()
    }

    /// Redo one group and place the caret where it reached.
    pub fn redo(&mut self) -> bool {
        if self.read_only {
            return false;
        }
        let caret = self.buffer.redo_caret().unwrap_or(None);
        self.after_history(caret);
        caret.is_some()
    }

    fn after_history(&mut self, caret: Option<usize>) {
        let Some(caret) = caret else {
            return;
        };
        self.caret = self.grapheme_edge(self.caret_of(caret), true, false);
        self.anchor = self.caret;
        self.goal_column = None;
    }

    /// True when the buffer differs from the last saved state.
    #[must_use]
    pub fn is_modified(&self) -> bool {
        self.buffer.is_modified()
    }

    /// Record the current text as the saved state.
    pub fn mark_saved(&mut self) {
        self.buffer.mark_saved();
    }

    /// Take the queued change notifications as edit spans.
    pub fn take_changes(&mut self) -> Vec<EditSpan> {
        self.buffer
            .take_changes()
            .into_iter()
            .map(|change| EditSpan {
                start_line: change.start_line,
                removed_lines: change.removed_lines,
                inserted_lines: change.inserted_lines,
            })
            .collect()
    }

    /// Keep a caret pointing at the same text after an edit elsewhere.
    ///
    /// A caret above the edit does not move; one below it shifts by the change
    /// in line count; one inside it falls to the first line of the edit.
    #[must_use]
    pub fn shift_caret(caret: Caret, span: EditSpan) -> Caret {
        if caret.line < span.start_line {
            return caret;
        }
        let removed_end = span.start_line.saturating_add(span.removed_lines);
        if caret.line <= removed_end {
            return Caret::new(span.start_line, 0);
        }
        let line = caret
            .line
            .saturating_sub(span.removed_lines)
            .saturating_add(span.inserted_lines);
        Caret::new(line, caret.index)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{display_column, index_at_column, Caret, EditSpan, Motion, Pane};
    use ca_text::{LineRange, TabSettings};

    fn pane() -> Pane {
        Pane::from_text("alpha\nbeta\ngamma\n")
    }

    #[test]
    fn enter_after_a_lone_carriage_return_adds_a_line() {
        for ending in ["\n", "\r", "\r\n"] {
            let mut pane = Pane::from_text("a\rb");
            pane.place(Caret::new(1, 0), false);
            pane.enter(ending);
            assert_eq!(pane.buffer().len_lines(), 3, "{ending:?}");
            assert_eq!(pane.caret(), Caret::new(2, 0));
            assert!(pane.undo());
            assert_eq!(pane.buffer().text(), "a\rb");
            assert!(pane.redo());
            assert_eq!(pane.buffer().len_lines(), 3);
        }
    }

    #[test]
    fn enter_keeps_existing_terminators_and_selection_in_one_history_step() {
        let mut pane = Pane::from_text("a\rb");
        pane.place(Caret::new(1, 0), false);
        pane.place(Caret::new(1, 1), true);
        pane.enter("\n");
        assert_eq!(pane.buffer().text(), "a\r\r\n");
        assert_eq!(pane.caret(), Caret::new(2, 0));
        assert!(pane.undo());
        assert_eq!(pane.buffer().text(), "a\rb");
        assert!(!pane.undo());

        let mut pane = Pane::from_text("a\nb");
        pane.place(Caret::new(0, 1), false);
        pane.enter("\r");
        assert_eq!(pane.buffer().text(), "a\r\n\nb");
        assert_eq!(pane.buffer().len_lines(), 3);
    }

    #[test]
    fn history_moves_the_caret_to_the_replayed_edit() {
        let mut pane = pane();
        pane.place(Caret::new(1, 2), false);
        pane.insert("e\u{301}");
        pane.place(Caret::new(0, 0), false);
        assert!(pane.undo());
        assert_eq!(pane.caret(), Caret::new(1, 2));
        pane.place(Caret::new(2, 0), false);
        assert!(pane.redo());
        assert_eq!(pane.caret(), Caret::new(1, 4));
        assert_eq!(pane.selection(), None);
    }

    #[test]
    fn grouped_replacement_history_moves_to_restored_and_inserted_text() {
        let mut pane = pane();
        pane.place(Caret::new(1, 0), false);
        pane.place(Caret::new(1, 4), true);
        pane.insert("x\r\ny");
        pane.place(Caret::new(0, 0), false);
        assert!(pane.undo());
        assert_eq!(pane.caret(), Caret::new(1, 4));
        assert!(pane.redo());
        assert_eq!(pane.caret(), Caret::new(2, 1));
        assert!(!pane.redo());
        assert_eq!(pane.caret(), Caret::new(2, 1));
    }

    #[test]
    fn a_tab_advances_the_display_column_to_the_next_stop() {
        let tabs = TabSettings::default();
        assert_eq!(display_column("a\tb", 2, tabs), 8);
        assert_eq!(display_column("\t", 1, tabs), 8);
        assert_eq!(display_column("ab", 2, tabs), 2);
    }

    #[test]
    fn multi_byte_characters_count_one_column_each() {
        let tabs = TabSettings::default();
        assert_eq!(display_column("äöü", 3, tabs), 3);
        assert_eq!(index_at_column("äöü", 2, tabs), 2);
    }

    #[test]
    fn a_column_inside_a_tab_run_resolves_to_the_tab() {
        let tabs = TabSettings::default();
        assert_eq!(index_at_column("a\tb", 4, tabs), 1);
        assert_eq!(index_at_column("a\tb", 8, tabs), 2);
        assert_eq!(index_at_column("a\tb", 99, tabs), 3);
    }

    #[test]
    fn placement_and_arrow_keys_keep_extended_graphemes_together() {
        for cluster in [
            "e\u{301}",
            "👩\u{200d}👩\u{200d}👦",
            "✈\u{fe0f}",
            "🇺🇳",
            "क्\u{0937}",
        ] {
            let length = u32::try_from(cluster.chars().count()).unwrap();
            let mut pane = Pane::from_text(&format!("{cluster}x"));
            pane.place(Caret::new(0, 1), false);
            assert_eq!(pane.caret(), Caret::new(0, 0), "{cluster}");
            pane.move_caret(Motion::Right, true);
            assert_eq!(pane.caret(), Caret::new(0, length), "{cluster}");
            assert_eq!(pane.selected_text().as_deref(), Some(cluster));
            pane.move_caret(Motion::Left, false);
            assert_eq!(pane.caret(), Caret::new(0, 0));
            pane.type_character('X');
            assert_eq!(pane.buffer().text(), format!("X{cluster}x"));
        }
    }

    #[test]
    fn deletion_and_history_keep_a_combining_cluster_whole() {
        let mut pane = Pane::from_text("e\u{301}x");
        pane.place(Caret::new(0, 2), false);
        pane.backspace();
        assert_eq!(pane.buffer().text(), "x");
        assert!(pane.undo());
        pane.place(Caret::new(0, 0), false);
        pane.delete();
        assert_eq!(pane.buffer().text(), "x");
        assert!(pane.undo());
        pane.place(Caret::new(0, 0), false);
        pane.move_caret(Motion::Right, true);
        pane.type_character('A');
        assert_eq!(pane.buffer().text(), "Ax");
    }

    #[test]
    fn a_grapheme_crossing_rope_chunks_has_only_two_caret_edges() {
        let cluster = format!("e{}", "\u{301}".repeat(2_000));
        let prefix = "x".repeat(1_000);
        let mut pane = Pane::from_text(&format!("{prefix}{cluster}z"));
        pane.place(Caret::new(0, 2_000), false);
        assert_eq!(pane.caret().index, 1_000);
        pane.move_caret(Motion::Right, false);
        assert_eq!(pane.caret().index, 3_001);
        pane.move_caret(Motion::Left, false);
        assert_eq!(pane.caret().index, 1_000);
    }

    #[test]
    fn removing_a_line_break_keeps_the_caret_outside_the_joined_grapheme() {
        for action in 0..4 {
            let mut pane = Pane::from_text("e\n\u{301}x");
            pane.place(Caret::new(0, 1), false);
            if action < 3 {
                pane.place(Caret::new(1, 0), true);
            }
            match action {
                0 => {
                    assert_eq!(pane.cut().as_deref(), Some("\n"));
                }
                1 => pane.delete(),
                2 => pane.backspace(),
                _ => {
                    assert!(pane.delete_to(Caret::new(1, 0)));
                }
            }
            assert_eq!(pane.buffer().text(), "e\u{301}x");
            assert_eq!(pane.caret(), Caret::new(0, 0), "action {action}");
            pane.type_character('X');
            assert_eq!(pane.buffer().text(), "Xe\u{301}x");
        }
        let mut pane = Pane::from_text("e\n\u{301}x");
        pane.place(Caret::new(0, 1), false);
        pane.place(Caret::new(1, 0), true);
        pane.insert("A");
        assert_eq!(pane.buffer().text(), "eA\u{301}x");
        assert_eq!(pane.caret(), Caret::new(0, 3));
    }

    #[test]
    fn overwrite_replaces_the_whole_grapheme_under_the_caret() {
        for cluster in ["e\u{301}", "👩\u{200d}👩\u{200d}👦", "🇺🇳", "क्\u{0937}"] {
            let original = format!("{cluster}x\r\ny");
            let mut pane = Pane::from_text(&original);
            pane.toggle_overwrite();
            pane.type_character('A');
            assert_eq!(pane.buffer().text(), "Ax\r\ny", "{cluster}");
            assert_eq!(pane.caret(), Caret::new(0, 1), "{cluster}");
            pane.type_character('B');
            pane.type_character('C');
            assert_eq!(pane.buffer().text(), "ABC\r\ny", "{cluster}");
            while pane.undo() {}
            assert_eq!(pane.buffer().text(), original, "{cluster}");
        }
    }

    #[test]
    fn short_graphemes_split_by_rope_chunks_keep_their_edges() {
        use unicode_segmentation::UnicodeSegmentation;
        let first_chunk = Pane::from_text(&"x".repeat(4_000))
            .buffer()
            .rope()
            .chunks()
            .next()
            .map_or(0, str::len);
        for cluster in [
            "e\u{301}\u{302}",
            "👨\u{200d}👩\u{200d}👧\u{200d}👦",
            "🇺🇳",
            "क्\u{0937}",
            "1\u{fe0f}\u{20e3}",
        ] {
            let mut split = false;
            for shift in first_chunk.saturating_sub(cluster.len())..=first_chunk {
                let line = format!("{}{cluster}{}", "x".repeat(shift), "z".repeat(3_000));
                let mut pane = Pane::from_text(&format!("{line}\r\nend"));
                let chunk = pane.buffer().rope().chunks().next().map_or(0, str::len);
                split |= shift < chunk && chunk < shift + cluster.len();
                let mut edges = vec![0u32];
                for grapheme in line.graphemes(true).take(shift + 3) {
                    let count = u32::try_from(grapheme.chars().count()).unwrap();
                    edges.push(edges.last().unwrap() + count);
                }
                let first = u32::try_from(shift).unwrap();
                let last = first + u32::try_from(cluster.chars().count()).unwrap();
                for index in first..=last {
                    pane.place(Caret::new(0, index), false);
                    let expected = *edges.iter().rev().find(|edge| **edge <= index).unwrap();
                    assert_eq!(pane.caret().index, expected, "{cluster} {shift} {index}");
                }
                pane.place(Caret::new(0, first), false);
                pane.move_caret(Motion::Right, false);
                assert_eq!(pane.caret().index, last, "{cluster} {shift}");
                pane.move_caret(Motion::Left, false);
                assert_eq!(pane.caret().index, first, "{cluster} {shift}");
                pane.move_caret(Motion::Right, false);
                pane.backspace();
                assert_eq!(pane.line_text(0).chars().count(), shift + 3_000);
            }
            assert!(split, "{cluster} never crossed a chunk edge");
        }
    }

    #[test]
    fn character_movement_crosses_line_ends() {
        let mut pane = pane();
        pane.place(Caret::new(0, 5), false);
        pane.move_caret(Motion::Right, false);
        assert_eq!(pane.caret(), Caret::new(1, 0));
        pane.move_caret(Motion::Left, false);
        assert_eq!(pane.caret(), Caret::new(0, 5));
    }

    #[test]
    fn vertical_movement_keeps_the_goal_column() {
        let mut pane = Pane::from_text("longest line\nab\nanother long line\n");
        pane.place(Caret::new(0, 10), false);
        pane.move_caret(Motion::Down, false);
        assert_eq!(pane.caret(), Caret::new(1, 2));
        pane.move_caret(Motion::Down, false);
        assert_eq!(pane.caret(), Caret::new(2, 10));
    }

    #[test]
    fn word_movement_steps_over_runs_of_one_class() {
        let mut pane = Pane::from_text("one two_three  four\n");
        pane.place(Caret::new(0, 0), false);
        pane.move_caret(Motion::WordRight, false);
        assert_eq!(pane.caret(), Caret::new(0, 4));
        pane.move_caret(Motion::WordRight, false);
        assert_eq!(pane.caret(), Caret::new(0, 15));
        pane.move_caret(Motion::WordLeft, false);
        assert_eq!(pane.caret(), Caret::new(0, 4));
    }

    #[test]
    fn the_word_at_the_caret_includes_a_word_the_caret_just_left() {
        let mut pane = Pane::from_text("one two_three  four\n");
        pane.place(Caret::new(0, 6), false);
        assert_eq!(pane.word_at_caret().as_deref(), Some("two_three"));
        pane.place(Caret::new(0, 13), false);
        assert_eq!(pane.word_at_caret().as_deref(), Some("two_three"));
        pane.place(Caret::new(0, 14), false);
        assert_eq!(pane.word_at_caret(), None);
    }

    #[test]
    fn shift_extends_the_selection_and_a_plain_move_drops_it() {
        let mut pane = pane();
        pane.place(Caret::new(0, 0), false);
        pane.move_caret(Motion::Right, true);
        pane.move_caret(Motion::Right, true);
        assert_eq!(pane.selected_text().as_deref(), Some("al"));
        pane.move_caret(Motion::Right, false);
        assert!(pane.selection().is_none());
    }

    #[test]
    fn a_page_move_is_bounded_by_the_buffer() {
        let mut pane = pane();
        pane.move_caret(Motion::PageDown(1000), false);
        assert_eq!(pane.caret().line, pane.line_count() - 1);
        pane.move_caret(Motion::PageUp(1000), false);
        assert_eq!(pane.caret(), Caret::new(0, 0));
    }

    #[test]
    fn document_movement_reaches_both_ends() {
        let mut pane = Pane::from_text("a\nbb");
        pane.move_caret(Motion::DocumentEnd, false);
        assert_eq!(pane.caret(), Caret::new(1, 2));
        pane.move_caret(Motion::DocumentStart, false);
        assert_eq!(pane.caret(), Caret::new(0, 0));
    }

    #[test]
    fn a_click_places_the_caret_by_display_column() {
        let mut pane = Pane::from_text("a\tbc\n");
        pane.place_at_column(0, 8, false);
        assert_eq!(pane.caret(), Caret::new(0, 2));
        pane.place_at_column(0, 9, false);
        assert_eq!(pane.caret(), Caret::new(0, 3));
    }

    #[test]
    fn typing_inserts_and_one_undo_removes_the_run() {
        let mut pane = Pane::from_text("");
        for character in "hello".chars() {
            pane.type_character(character);
        }
        assert_eq!(pane.buffer().text(), "hello");
        assert!(pane.undo());
        assert_eq!(pane.buffer().text(), "");
    }

    #[test]
    fn overwrite_replaces_the_character_under_the_caret() {
        let mut pane = Pane::from_text("abc\n");
        pane.toggle_overwrite();
        pane.type_character('X');
        assert_eq!(pane.buffer().text(), "Xbc\n");
        assert_eq!(pane.caret(), Caret::new(0, 1));
    }

    #[test]
    fn overwrite_at_a_line_end_still_inserts() {
        let mut pane = Pane::from_text("ab\ncd\n");
        pane.toggle_overwrite();
        pane.place(Caret::new(0, 2), false);
        pane.type_character('X');
        assert_eq!(pane.buffer().text(), "abX\ncd\n");
    }

    #[test]
    fn enter_breaks_the_line_at_the_caret() {
        let mut pane = Pane::from_text("abcd\n");
        pane.place(Caret::new(0, 2), false);
        pane.enter("\n");
        assert_eq!(pane.buffer().text(), "ab\ncd\n");
        assert_eq!(pane.caret(), Caret::new(1, 0));
    }

    #[test]
    fn backspace_joins_two_lines() {
        let mut pane = Pane::from_text("ab\ncd\n");
        pane.place(Caret::new(1, 0), false);
        pane.backspace();
        assert_eq!(pane.buffer().text(), "abcd\n");
        assert_eq!(pane.caret(), Caret::new(0, 2));
    }

    #[test]
    fn delete_removes_the_character_after_the_caret() {
        let mut pane = Pane::from_text("abc");
        pane.place(Caret::new(0, 1), false);
        pane.delete();
        assert_eq!(pane.buffer().text(), "ac");
    }

    #[test]
    fn typing_over_a_selection_replaces_it_in_one_step() {
        let mut pane = Pane::from_text("abcdef\n");
        pane.place(Caret::new(0, 1), false);
        pane.place(Caret::new(0, 4), true);
        pane.type_character('Z');
        assert_eq!(pane.buffer().text(), "aZef\n");
        assert!(pane.undo());
        assert_eq!(pane.buffer().text(), "abcdef\n");
    }

    #[test]
    fn cut_copy_and_paste_move_text() {
        let mut pane = Pane::from_text("abcdef\n");
        pane.place(Caret::new(0, 0), false);
        pane.place(Caret::new(0, 3), true);
        assert_eq!(pane.copy().as_deref(), Some("abc"));
        let taken = pane.cut().unwrap();
        assert_eq!(pane.buffer().text(), "def\n");
        pane.move_caret(Motion::LineEnd, false);
        pane.paste(&taken);
        assert_eq!(pane.buffer().text(), "defabc\n");
    }

    #[test]
    fn select_all_covers_every_line() {
        let mut pane = pane();
        pane.select_all();
        assert_eq!(
            pane.selected_text().as_deref(),
            Some("alpha\nbeta\ngamma\n")
        );
    }

    #[test]
    fn selected_lines_do_not_claim_the_line_after_a_trailing_break() {
        let mut pane = pane();
        pane.place(Caret::new(0, 0), false);
        pane.place(Caret::new(2, 0), true);
        assert_eq!(pane.selected_lines(), LineRange::new(0, 2));
    }

    #[test]
    fn tab_indents_a_multi_line_selection_as_one_step() {
        let mut pane = Pane::from_text("a\nb\nc\n");
        pane.place(Caret::new(0, 0), false);
        pane.place(Caret::new(1, 1), true);
        pane.tab();
        assert_eq!(pane.buffer().text(), "\ta\n\tb\nc\n");
        assert!(pane.undo());
        assert_eq!(pane.buffer().text(), "a\nb\nc\n");
    }

    #[test]
    fn shift_tab_removes_one_indent_level() {
        let mut pane = Pane::from_text("\ta\n\tb\n");
        pane.place(Caret::new(0, 0), false);
        pane.place(Caret::new(1, 1), true);
        pane.decrease_indent();
        assert_eq!(pane.buffer().text(), "a\nb\n");
    }

    #[test]
    fn tab_without_a_multi_line_selection_inserts_one_level() {
        let mut pane = Pane::from_text("ab\n");
        pane.place(Caret::new(0, 1), false);
        pane.tab();
        assert_eq!(pane.buffer().text(), "a\tb\n");
    }

    #[test]
    fn a_read_only_pane_refuses_every_edit() {
        let mut pane = Pane::from_text("abc\n");
        pane.set_read_only(true);
        pane.type_character('X');
        pane.backspace();
        pane.delete();
        pane.tab();
        pane.paste("zz");
        assert_eq!(pane.buffer().text(), "abc\n");
    }

    #[test]
    fn undo_and_redo_walk_the_same_steps() {
        let mut pane = Pane::from_text("a\n");
        pane.move_caret(Motion::DocumentEnd, false);
        pane.insert("b");
        pane.insert("c");
        assert_eq!(pane.buffer().text(), "a\nbc");
        assert!(pane.undo());
        assert!(pane.undo());
        assert_eq!(pane.buffer().text(), "a\n");
        assert!(pane.redo());
        assert_eq!(pane.buffer().text(), "a\nb");
    }

    #[test]
    fn deleting_a_line_leaves_the_caret_on_the_next_one() {
        let mut pane = pane();
        pane.place(Caret::new(1, 2), false);
        pane.delete_line();
        assert_eq!(pane.buffer().text(), "alpha\ngamma\n");
        assert_eq!(pane.caret(), Caret::new(1, 0));
    }

    #[test]
    fn deleting_to_the_line_ends_leaves_the_terminator_alone() {
        let mut pane = Pane::from_text("abcdef\nxyz\n");
        pane.place(Caret::new(0, 3), false);
        pane.delete_to_line_end();
        assert_eq!(pane.buffer().text(), "abc\nxyz\n");
        pane.delete_to_line_start();
        assert_eq!(pane.buffer().text(), "\nxyz\n");
    }

    #[test]
    fn an_edit_reports_the_lines_it_touched() {
        let mut pane = Pane::from_text("a\nb\n");
        let _ = pane.take_changes();
        pane.move_caret(Motion::DocumentStart, false);
        pane.insert("x\ny\n");
        let spans = pane.take_changes();
        assert!(!spans.is_empty());
        assert_eq!(spans[0].start_line, 0);
    }

    #[test]
    fn a_caret_below_an_edit_shifts_by_the_line_count() {
        let span = EditSpan {
            start_line: 2,
            removed_lines: 1,
            inserted_lines: 3,
        };
        assert_eq!(Pane::shift_caret(Caret::new(1, 4), span), Caret::new(1, 4));
        assert_eq!(Pane::shift_caret(Caret::new(9, 4), span), Caret::new(11, 4));
        assert_eq!(Pane::shift_caret(Caret::new(3, 4), span), Caret::new(2, 0));
    }

    #[test]
    fn a_modified_buffer_reports_itself_until_it_is_saved() {
        let mut pane = Pane::from_text("a\n");
        assert!(!pane.is_modified());
        pane.type_character('x');
        assert!(pane.is_modified());
        pane.mark_saved();
        assert!(!pane.is_modified());
    }
}
