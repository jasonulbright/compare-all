//! Line oriented, restartable tokenizer.
//!
//! The tokenizer sees one line at a time plus the state carried out of the
//! previous line, and returns tokens that tile that line completely: every byte
//! of the line belongs to exactly one token, and text no item claims becomes an
//! unclaimed token rather than a gap. Tiling is what lets a caller walk tokens
//! instead of the raw line.
//!
//! Carried state is small and comparable, which is what makes an edit cheap: a
//! [`StateCache`] keeps the state at the start of every line, and re-lexing
//! after an edit runs forward only until the freshly computed state matches the
//! cached one, at which point every later line is already correct.
//!
//! # Cost
//!
//! Work is linear in the length of a line. Literal items are rejected at a
//! position by a first-byte bitmap; pattern items keep a cursor holding the
//! leftmost match found so far, so a pattern is re-scanned only after a token
//! is consumed past its cached match, bounding scans by the token count rather
//! than by the position count.

use crate::grammar::{ColumnEnd, Grammar, ItemKind, MatchOptions};
use crate::pattern::CompiledPattern;
use crate::GrammarError;
use serde::{Deserialize, Serialize};
use std::ops::Range;

/// State carried from the end of one line into the start of the next.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LineState {
    /// A delimited item whose closing delimiter has not been reached.
    pub open_item: Option<u32>,
    /// A multi-line block item still covering following lines.
    pub block_item: Option<u32>,
    /// How many further lines the block item covers.
    pub block_remaining: u32,
}

impl LineState {
    /// The state at the start of a file: nothing open, no block running.
    pub fn start() -> Self {
        Self::default()
    }
}

/// One token covering a byte range of a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexToken {
    /// Byte range within the line. Both ends fall on character boundaries.
    pub range: Range<usize>,
    /// The grammar item that claimed the range, or `None` for unclaimed text.
    pub item: Option<u32>,
}

/// Tokens for one line together with the state to carry into the next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexOutput {
    /// Tokens in increasing order, tiling the line.
    pub tokens: Vec<LexToken>,
    /// State to pass into the following line.
    pub state: LineState,
}

/// Reusable per-line working storage.
///
/// Holding it outside the tokenizer keeps the tokenizer shareable between
/// threads while still avoiding an allocation for every line.
#[derive(Debug, Clone, Default)]
pub struct LexScratch {
    cursors: Vec<Cursor>,
    /// Bytes the pattern searches have covered, each from its origin to the
    /// end of its match, or to the end of the line for a miss.
    #[cfg(test)]
    scanned: usize,
}

impl LexScratch {
    /// Empty scratch, sized on first use.
    pub fn new() -> Self {
        Self::default()
    }
}

/// The leftmost match of one pattern at or after a search origin.
///
/// `origin` is `usize::MAX` while nothing has been searched yet.
#[derive(Debug, Clone, Copy)]
struct Cursor {
    origin: usize,
    found: Option<(usize, usize)>,
}

impl Default for Cursor {
    fn default() -> Self {
        Self {
            origin: usize::MAX,
            found: None,
        }
    }
}

/// A literal string or a compiled pattern.
#[derive(Debug, Clone)]
enum Matcher {
    Literal { text: String, case_sensitive: bool },
    Pattern { slot: usize },
}

#[derive(Debug, Clone)]
enum CompiledKind {
    Basic {
        matcher: Matcher,
        whole_word: bool,
        first_bytes: Option<Box<ByteSet>>,
    },
    List {
        matchers: Vec<Matcher>,
        whole_word: bool,
        first_bytes: Option<Box<ByteSet>>,
    },
    Delimited {
        start: Matcher,
        stop: Option<Matcher>,
        stop_at_end_of_line: bool,
        escape: Option<char>,
        line_spanning: bool,
        continue_after_escaped_newline: bool,
        first_bytes: Option<Box<ByteSet>>,
    },
    Columns {
        start_column: u32,
        end: ColumnEnd,
    },
    Block {
        matcher: Matcher,
        or_line_1: bool,
        line_count: u32,
    },
    /// An item this build cannot run, which therefore claims nothing.
    Inert,
}

/// Which bytes a literal alternative can start with.
type ByteSet = [bool; 256];

#[derive(Debug, Clone)]
struct CompiledItem {
    element: String,
    kind: CompiledKind,
}

/// A grammar compiled for tokenizing.
#[derive(Debug, Clone)]
pub struct Lexer {
    items: Vec<CompiledItem>,
    patterns: Vec<CompiledPattern>,
    /// Indices of the block items, in grammar order, so a line start does not
    /// have to walk the whole item list looking for them.
    block_items: Vec<u32>,
    grammar: Grammar,
    /// Interval between tab stops, used to turn characters into the display
    /// columns a column range item is written against.
    tab_stop: usize,
}

/// The tab stop assumed when a caller supplies none.
const DEFAULT_TAB_STOP: usize = 8;

impl Lexer {
    /// Compile `grammar`.
    ///
    /// An item whose kind this build does not understand compiles to an item
    /// that never matches, rather than failing the whole grammar: a settings
    /// file from a newer build still colors everything it can.
    ///
    /// # Errors
    ///
    /// Returns [`GrammarError`] when a pattern does not compile or an item's
    /// fields contradict its kind.
    pub fn new(grammar: &Grammar) -> Result<Self, GrammarError> {
        Self::with_tab_stop(grammar, DEFAULT_TAB_STOP)
    }

    /// Compile `grammar` against a format's tab stop.
    ///
    /// The tab stop only affects column range items, which are written against
    /// the columns a reader sees rather than against character counts. A tab
    /// stop of zero is read as one, so a tab always advances at least one
    /// column and a scan cannot stall.
    ///
    /// # Errors
    ///
    /// Returns [`GrammarError`] when a pattern does not compile or an item's
    /// fields contradict its kind.
    pub fn with_tab_stop(grammar: &Grammar, tab_stop: usize) -> Result<Self, GrammarError> {
        let mut patterns = Vec::new();
        let mut items = Vec::with_capacity(grammar.items.len());
        let mut block_items = Vec::new();
        for (index, item) in grammar.items.iter().enumerate() {
            let kind = match item.kind.known() {
                None => CompiledKind::Inert,
                Some(kind) => compile_kind(&item.element, kind, &mut patterns)?,
            };
            if matches!(kind, CompiledKind::Block { .. }) {
                block_items.push(u32::try_from(index).unwrap_or(u32::MAX));
            }
            items.push(CompiledItem {
                element: item.element.clone(),
                kind,
            });
        }
        Ok(Self {
            items,
            patterns,
            block_items,
            grammar: grammar.clone(),
            tab_stop: tab_stop.max(1),
        })
    }

    /// The grammar this tokenizer was built from.
    pub fn grammar(&self) -> &Grammar {
        &self.grammar
    }

    /// The tab stop column range items are measured against.
    pub fn tab_stop(&self) -> usize {
        self.tab_stop
    }

    /// The element name of the item at `index`.
    pub fn element_of(&self, index: u32) -> Option<&str> {
        self.items
            .get(usize::try_from(index).ok()?)
            .map(|i| i.element.as_str())
    }

    /// Element names in precedence order, each named once.
    pub fn element_names(&self) -> Vec<String> {
        self.grammar.element_names()
    }

    /// Tokenize one line, allocating a fresh token vector and scratch.
    ///
    /// # Errors
    ///
    /// Returns [`GrammarError`] when a pattern fails at match time.
    pub fn lex_line(
        &self,
        line: &str,
        line_index: usize,
        state_in: LineState,
    ) -> Result<LexOutput, GrammarError> {
        let mut scratch = LexScratch::new();
        let mut tokens = Vec::new();
        let state = self.lex_line_into(&mut scratch, line, line_index, state_in, &mut tokens)?;
        Ok(LexOutput { tokens, state })
    }

    /// Tokenize one line into a caller-owned vector, which is cleared first.
    ///
    /// # Errors
    ///
    /// Returns [`GrammarError`] when a pattern fails at match time.
    pub fn lex_line_into(
        &self,
        scratch: &mut LexScratch,
        line: &str,
        line_index: usize,
        state_in: LineState,
        out: &mut Vec<LexToken>,
    ) -> Result<LineState, GrammarError> {
        out.clear();
        scratch.cursors.clear();
        scratch
            .cursors
            .resize(self.patterns.len(), Cursor::default());
        let mut state = state_in;

        if state.block_remaining > 0 {
            let item = state.block_item;
            state.block_remaining -= 1;
            if state.block_remaining == 0 {
                state.block_item = None;
            }
            push_whole_line(out, line, item);
            return Ok(state);
        }

        let mut pos = 0;
        if let Some(open) = state.open_item {
            let (end, closed) = self.scan_body(scratch, line, 0, open)?;
            if end > 0 {
                out.push(LexToken {
                    range: 0..end,
                    item: Some(open),
                });
            }
            pos = end;
            if closed {
                state.open_item = None;
            } else {
                if !self.line_continues(open, line) {
                    state.open_item = None;
                }
                return Ok(state);
            }
        }

        if pos == 0 {
            if let Some((item, count)) = self.match_block(scratch, line, line_index)? {
                push_whole_line(out, line, Some(item));
                state.block_item = Some(item);
                state.block_remaining = count.saturating_sub(1);
                if state.block_remaining == 0 {
                    state.block_item = None;
                }
                return Ok(state);
            }
        }

        let mut column = advance_display(&line[..pos], 0, self.tab_stop);
        let mut unclaimed: Option<usize> = None;
        while pos < line.len() {
            match self.claim_at(scratch, line, pos, column)? {
                Some(claim) if claim.end > pos => {
                    if let Some(start) = unclaimed.take() {
                        out.push(LexToken {
                            range: start..pos,
                            item: None,
                        });
                    }
                    out.push(LexToken {
                        range: pos..claim.end,
                        item: Some(claim.item),
                    });
                    column = advance_display(&line[pos..claim.end], column, self.tab_stop);
                    pos = claim.end;
                    if claim.leaves_open {
                        state.open_item = Some(claim.item);
                    }
                }
                _ => {
                    if unclaimed.is_none() {
                        unclaimed = Some(pos);
                    }
                    let next = line[pos..].chars().next();
                    pos += next.map_or(1, char::len_utf8);
                    column += next.map_or(1, |c| display_width(c, column, self.tab_stop));
                }
            }
        }
        if let Some(start) = unclaimed {
            out.push(LexToken {
                range: start..line.len(),
                item: None,
            });
        }
        Ok(state)
    }

    /// The first item, in precedence order, that claims text starting at `pos`.
    fn claim_at(
        &self,
        scratch: &mut LexScratch,
        line: &str,
        pos: usize,
        column: usize,
    ) -> Result<Option<Claim>, GrammarError> {
        for (index, item) in self.items.iter().enumerate() {
            let index = u32::try_from(index).unwrap_or(u32::MAX);
            match &item.kind {
                CompiledKind::Inert | CompiledKind::Block { .. } => {}
                CompiledKind::Basic {
                    matcher,
                    whole_word,
                    first_bytes,
                } => {
                    if rejects(first_bytes.as_deref(), line, pos)
                        || (*whole_word && inside_word(line, pos))
                    {
                        continue;
                    }
                    if let Some(end) = self.match_at(scratch, matcher, line, pos)? {
                        if end > pos && (!*whole_word || word_bounded(line, pos, end)) {
                            return Ok(Some(Claim::plain(index, end)));
                        }
                    }
                }
                CompiledKind::List {
                    matchers,
                    whole_word,
                    first_bytes,
                } => {
                    if rejects(first_bytes.as_deref(), line, pos)
                        || (*whole_word && inside_word(line, pos))
                    {
                        continue;
                    }
                    let mut best = None;
                    for matcher in matchers {
                        let Some(end) = self.match_at(scratch, matcher, line, pos)? else {
                            continue;
                        };
                        if end <= pos || (*whole_word && !word_bounded(line, pos, end)) {
                            continue;
                        }
                        // Longest alternative wins, so a list holding both a
                        // token and a longer token starting with it cannot
                        // truncate the longer one depending on list order. The
                        // rule covers pattern alternatives as well, so the two
                        // kinds of alternative behave the same way.
                        if best.is_none_or(|b| end > b) {
                            best = Some(end);
                        }
                    }
                    if let Some(end) = best {
                        return Ok(Some(Claim::plain(index, end)));
                    }
                }
                CompiledKind::Delimited {
                    start, first_bytes, ..
                } => {
                    if rejects(first_bytes.as_deref(), line, pos) {
                        continue;
                    }
                    let Some(body) = self.match_at(scratch, start, line, pos)? else {
                        continue;
                    };
                    if body <= pos {
                        continue;
                    }
                    let (end, closed) = self.scan_body(scratch, line, body, index)?;
                    return Ok(Some(Claim {
                        item: index,
                        end,
                        leaves_open: !closed && self.line_continues(index, line),
                    }));
                }
                CompiledKind::Columns { start_column, end } => {
                    if usize::try_from(*start_column).unwrap_or(usize::MAX) != column + 1 {
                        continue;
                    }
                    let stop = match end {
                        ColumnEnd::EndOfLine => line.len(),
                        ColumnEnd::Column(last) => {
                            let last = usize::try_from(*last).unwrap_or(usize::MAX);
                            advance_to_column(line, pos, column, last, self.tab_stop)
                        }
                    };
                    if stop > pos {
                        return Ok(Some(Claim::plain(index, stop)));
                    }
                }
            }
        }
        Ok(None)
    }

    fn line_continues(&self, item: u32, line: &str) -> bool {
        let Some(CompiledItem {
            kind:
                CompiledKind::Delimited {
                    stop_at_end_of_line,
                    escape,
                    line_spanning,
                    continue_after_escaped_newline,
                    ..
                },
            ..
        }) = self.items.get(usize::try_from(item).unwrap_or(usize::MAX))
        else {
            return false;
        };
        if *stop_at_end_of_line {
            return false;
        }
        *line_spanning
            || (*continue_after_escaped_newline
                && escape.is_some_and(|character| has_escaped_line_ending(line, character)))
    }

    /// Consume from `from` to the closing delimiter of the delimited `item`.
    ///
    /// Returns the end offset and whether the delimiter was reached. An escape
    /// character consumes itself and the character after it, so a closing
    /// delimiter written after an escape does not close the element.
    fn scan_body(
        &self,
        scratch: &mut LexScratch,
        line: &str,
        from: usize,
        item: u32,
    ) -> Result<(usize, bool), GrammarError> {
        let Some(CompiledItem {
            kind:
                CompiledKind::Delimited {
                    stop,
                    stop_at_end_of_line,
                    escape,
                    ..
                },
            ..
        }) = self.items.get(usize::try_from(item).unwrap_or(usize::MAX))
        else {
            return Ok((line.len(), true));
        };
        if *stop_at_end_of_line {
            return Ok((line.len(), true));
        }
        let Some(stop) = stop else {
            return Ok((line.len(), true));
        };
        let mut pos = from;
        while pos < line.len() {
            let Some(next) = line[pos..].chars().next() else {
                break;
            };
            if Some(next) == *escape {
                pos += next.len_utf8();
                let Some(escaped) = line[pos..].chars().next() else {
                    return Ok((line.len(), false));
                };
                pos += escaped.len_utf8();
                continue;
            }
            if let Some(end) = self.match_at(scratch, stop, line, pos)? {
                if end > pos {
                    return Ok((end, true));
                }
            }
            pos += next.len_utf8();
        }
        Ok((line.len(), false))
    }

    /// The first block item whose marker this line carries.
    fn match_block(
        &self,
        scratch: &mut LexScratch,
        line: &str,
        line_index: usize,
    ) -> Result<Option<(u32, u32)>, GrammarError> {
        for index in &self.block_items {
            let Some(item) = self
                .items
                .get(usize::try_from(*index).unwrap_or(usize::MAX))
            else {
                continue;
            };
            let CompiledKind::Block {
                matcher,
                or_line_1,
                line_count,
            } = &item.kind
            else {
                continue;
            };
            if *line_count == 0 {
                continue;
            }
            if *or_line_1 && line_index == 0 {
                return Ok(Some((*index, *line_count)));
            }
            let _ = &scratch;
            if self.find_anywhere(matcher, line)? {
                return Ok(Some((*index, *line_count)));
            }
        }
        Ok(None)
    }

    /// Whether `matcher` matches anywhere in `line`.
    fn find_anywhere(&self, matcher: &Matcher, line: &str) -> Result<bool, GrammarError> {
        match matcher {
            Matcher::Literal {
                text,
                case_sensitive,
            } => {
                if text.is_empty() {
                    return Ok(false);
                }
                let mut pos = 0;
                while pos <= line.len() {
                    if line.is_char_boundary(pos)
                        && literal_match(line, pos, text, *case_sensitive).is_some()
                    {
                        return Ok(true);
                    }
                    pos += 1;
                }
                Ok(false)
            }
            Matcher::Pattern { slot } => {
                let Some(pattern) = self.patterns.get(*slot) else {
                    return Ok(false);
                };
                Ok(pattern.find_at(line, 0)?.is_some())
            }
        }
    }

    /// The end offset of `matcher` when it matches exactly at `pos`.
    fn match_at(
        &self,
        scratch: &mut LexScratch,
        matcher: &Matcher,
        line: &str,
        pos: usize,
    ) -> Result<Option<usize>, GrammarError> {
        match matcher {
            Matcher::Literal {
                text,
                case_sensitive,
            } => Ok(literal_match(line, pos, text, *case_sensitive)),
            Matcher::Pattern { slot } => {
                let Some(pattern) = self.patterns.get(*slot) else {
                    return Ok(None);
                };
                let Some(cursor) = scratch.cursors.get_mut(*slot) else {
                    return Ok(None);
                };
                // A cached match starting before `pos` is stale, and a cached
                // miss from an origin at or before `pos` is conclusive: no
                // match begins anywhere in the searched span.
                let stale = cursor.origin == usize::MAX
                    || cursor.origin > pos
                    || cursor.found.is_some_and(|(s, _)| s < pos);
                if stale {
                    cursor.origin = pos;
                    cursor.found = pattern.find_at(line, pos)?;
                }
                let found = cursor.found;
                #[cfg(test)]
                if stale {
                    scratch.scanned += found.map_or(line.len(), |(_, end)| end).saturating_sub(pos);
                }
                Ok(match found {
                    Some((s, e)) if s == pos => Some(e),
                    _ => None,
                })
            }
        }
    }
}

fn has_escaped_line_ending(line: &str, escape: char) -> bool {
    let body = line
        .strip_suffix("\r\n")
        .or_else(|| line.strip_suffix('\n'))
        .or_else(|| line.strip_suffix('\r'))
        .unwrap_or(line);
    body.chars()
        .rev()
        .take_while(|character| *character == escape)
        .count()
        % 2
        == 1
}

/// What an item claims at a position.
#[derive(Debug, Clone, Copy)]
struct Claim {
    item: u32,
    end: usize,
    leaves_open: bool,
}

impl Claim {
    const fn plain(item: u32, end: usize) -> Self {
        Self {
            item,
            end,
            leaves_open: false,
        }
    }
}

fn push_whole_line(out: &mut Vec<LexToken>, line: &str, item: Option<u32>) {
    if !line.is_empty() {
        out.push(LexToken {
            range: 0..line.len(),
            item,
        });
    }
}

/// Whether a first-byte set rules out any match at `pos`.
fn rejects(set: Option<&ByteSet>, line: &str, pos: usize) -> bool {
    match (set, line.as_bytes().get(pos)) {
        (Some(set), Some(byte)) => !set[*byte as usize],
        _ => false,
    }
}

/// How many display columns `c` occupies when it starts at `column`.
///
/// A tab runs to the next multiple of the tab stop; everything else is one
/// column wide. The one-column assumption has to match the column model the
/// text layer measures lines with, so that a column range item lands where the
/// same number reaches in a view.
fn display_width(c: char, column: usize, tab_stop: usize) -> usize {
    if c == '\t' {
        let stop = tab_stop.max(1);
        return stop - (column % stop);
    }
    1
}

/// The display column reached after `text`, starting from `column`.
fn advance_display(text: &str, column: usize, tab_stop: usize) -> usize {
    let mut column = column;
    for c in text.chars() {
        column = column.saturating_add(display_width(c, column, tab_stop));
    }
    column
}

/// The offset at which a column range ending at display column `last` stops.
///
/// `column` is the display column the range starts at, counted from zero. A tab
/// is one character and cannot be split, so a range whose last column falls
/// inside a tab takes the whole tab.
fn advance_to_column(line: &str, pos: usize, column: usize, last: usize, tab_stop: usize) -> usize {
    let mut end = pos;
    let mut column = column;
    while column < last {
        let Some(c) = line[end..].chars().next() else {
            break;
        };
        end += c.len_utf8();
        column = column.saturating_add(display_width(c, column, tab_stop));
    }
    end
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Whether `pos` lies between two word characters.
///
/// A match that starts there starts with the word character at `pos`, so
/// [`word_bounded`] refuses it whatever it covers. Skipping the search there
/// keeps a whole-word pattern from being searched again at every position of
/// a long word, each search running to the end of the word, which costs the
/// square of the word's length.
fn inside_word(line: &str, pos: usize) -> bool {
    let before = line.get(..pos).and_then(|text| text.chars().next_back());
    let at = line.get(pos..).and_then(|text| text.chars().next());
    before.is_some_and(is_word_char) && at.is_some_and(is_word_char)
}

/// Whether the span `start..end` is not glued to a word character on a side
/// whose own edge character is a word character.
fn word_bounded(line: &str, start: usize, end: usize) -> bool {
    let matched = &line[start..end];
    if matched.chars().next().is_some_and(is_word_char) {
        if let Some(before) = line[..start].chars().next_back() {
            if is_word_char(before) {
                return false;
            }
        }
    }
    if matched.chars().next_back().is_some_and(is_word_char) {
        if let Some(after) = line[end..].chars().next() {
            if is_word_char(after) {
                return false;
            }
        }
    }
    true
}

/// Characters outside ASCII whose simple lowercase is exactly one ASCII
/// character, each paired with the first byte of its UTF-8 encoding.
///
/// The first-byte filter and the character comparison have to agree on which
/// characters can start a case-insensitive literal, or a literal is rejected at
/// a position the comparison would have accepted. The table is derived from the
/// same lowercase mapping the comparison uses, so the two cannot drift.
fn ascii_folding_lead_bytes() -> &'static [(u8, u8)] {
    static TABLE: std::sync::OnceLock<Vec<(u8, u8)>> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        let mut out: Vec<(u8, u8)> = Vec::new();
        let mut buffer = [0_u8; 4];
        for code in 0x80_u32..=0x0010_FFFF {
            let Some(c) = char::from_u32(code) else {
                continue;
            };
            let mut lower = c.to_lowercase();
            let (Some(first), None) = (lower.next(), lower.next()) else {
                continue;
            };
            if !first.is_ascii() {
                continue;
            }
            let lead = c.encode_utf8(&mut buffer).as_bytes()[0];
            let entry = (first as u8, lead);
            if !out.contains(&entry) {
                out.push(entry);
            }
        }
        out
    })
}

/// The end offset of `needle` when it sits at `pos`, comparing character by
/// character so a case-insensitive comparison cannot land off a boundary.
fn literal_match(line: &str, pos: usize, needle: &str, case_sensitive: bool) -> Option<usize> {
    if needle.is_empty() || pos >= line.len() {
        return None;
    }
    if case_sensitive {
        return line[pos..].starts_with(needle).then(|| pos + needle.len());
    }
    let mut end = pos;
    let mut hay = line[pos..].chars();
    for want in needle.chars() {
        let got = hay.next()?;
        if got != want && !got.to_lowercase().eq(want.to_lowercase()) {
            return None;
        }
        end += got.len_utf8();
    }
    Some(end)
}

fn compile_matcher(
    text: &str,
    options: &MatchOptions,
    patterns: &mut Vec<CompiledPattern>,
) -> Result<Matcher, GrammarError> {
    if options.regular_expression {
        let compiled = CompiledPattern::compile(text, options.match_character_case)?;
        patterns.push(compiled);
        Ok(Matcher::Pattern {
            slot: patterns.len() - 1,
        })
    } else {
        Ok(Matcher::Literal {
            text: text.to_owned(),
            case_sensitive: options.match_character_case,
        })
    }
}

/// The set of bytes the literal alternatives can start with, or `None` when an
/// alternative is a pattern and so cannot be summarized this way.
fn first_byte_set(matchers: &[Matcher]) -> Option<Box<ByteSet>> {
    let mut set: ByteSet = [false; 256];
    for matcher in matchers {
        let Matcher::Literal {
            text,
            case_sensitive,
        } = matcher
        else {
            return None;
        };
        let first = text.as_bytes().first()?;
        set[*first as usize] = true;
        if !case_sensitive {
            set[first.to_ascii_lowercase() as usize] = true;
            set[first.to_ascii_uppercase() as usize] = true;
            // A non-ASCII lead byte can change under case folding in ways a
            // byte-level set cannot express, so the filter is abandoned.
            if !first.is_ascii() {
                return None;
            }
            // A character outside ASCII can still fold onto this one, and its
            // lead byte is nothing like it. Admitting those lead bytes is what
            // keeps the filter from rejecting a position the character
            // comparison would accept.
            let lower = first.to_ascii_lowercase();
            for (folded, lead) in ascii_folding_lead_bytes() {
                if *folded == lower {
                    set[*lead as usize] = true;
                }
            }
        }
    }
    Some(Box::new(set))
}

#[allow(clippy::too_many_lines)]
fn compile_kind(
    element: &str,
    kind: &ItemKind,
    patterns: &mut Vec<CompiledPattern>,
) -> Result<CompiledKind, GrammarError> {
    match kind {
        ItemKind::Basic {
            text,
            options,
            whole_word,
        } => {
            if text.is_empty() {
                return Err(GrammarError::Item {
                    element: element.to_owned(),
                    message: "a basic item needs matching text".to_owned(),
                });
            }
            let matcher = compile_matcher(text, options, patterns)?;
            let first_bytes = first_byte_set(std::slice::from_ref(&matcher));
            Ok(CompiledKind::Basic {
                matcher,
                whole_word: *whole_word,
                first_bytes,
            })
        }
        ItemKind::List {
            tokens,
            options,
            whole_word,
        } => {
            if tokens.iter().all(String::is_empty) {
                return Err(GrammarError::Item {
                    element: element.to_owned(),
                    message: "a list item needs at least one token".to_owned(),
                });
            }
            let mut matchers = Vec::with_capacity(tokens.len());
            for token in tokens.iter().filter(|t| !t.is_empty()) {
                matchers.push(compile_matcher(token, options, patterns)?);
            }
            let first_bytes = first_byte_set(&matchers);
            Ok(CompiledKind::List {
                matchers,
                whole_word: *whole_word,
                first_bytes,
            })
        }
        ItemKind::Delimited {
            start,
            stop,
            stop_at_end_of_line,
            escape,
            line_spanning,
            continue_after_escaped_newline,
            options,
        } => {
            if start.is_empty() {
                return Err(GrammarError::Item {
                    element: element.to_owned(),
                    message: "a delimited item needs a starting delimiter".to_owned(),
                });
            }
            if !*stop_at_end_of_line && stop.is_empty() {
                return Err(GrammarError::Item {
                    element: element.to_owned(),
                    message: "a delimited item needs an ending delimiter or the end-of-line option"
                        .to_owned(),
                });
            }
            let start = compile_matcher(start, options, patterns)?;
            let first_bytes = first_byte_set(std::slice::from_ref(&start));
            let stop = if *stop_at_end_of_line {
                None
            } else {
                Some(compile_matcher(stop, options, patterns)?)
            };
            Ok(CompiledKind::Delimited {
                start,
                stop,
                stop_at_end_of_line: *stop_at_end_of_line,
                escape: *escape,
                line_spanning: *line_spanning,
                continue_after_escaped_newline: *continue_after_escaped_newline,
                first_bytes,
            })
        }
        ItemKind::Columns { start_column, end } => {
            if *start_column == 0 {
                return Err(GrammarError::Item {
                    element: element.to_owned(),
                    message: "columns are counted from one".to_owned(),
                });
            }
            if let ColumnEnd::Column(last) = end {
                if last < start_column {
                    return Err(GrammarError::Item {
                        element: element.to_owned(),
                        message: "the ending column precedes the starting column".to_owned(),
                    });
                }
            }
            Ok(CompiledKind::Columns {
                start_column: *start_column,
                end: end.clone(),
            })
        }
        ItemKind::Lines {
            text,
            or_line_1,
            line_count,
            options,
        } => {
            if *line_count == 0 {
                return Err(GrammarError::Item {
                    element: element.to_owned(),
                    message: "a block item covers at least one line".to_owned(),
                });
            }
            Ok(CompiledKind::Block {
                matcher: compile_matcher(text, options, patterns)?,
                or_line_1: *or_line_1,
                line_count: *line_count,
            })
        }
    }
}

/// The state at the start of every line of a file.
///
/// The cache is what makes an edit cheap. After a line changes, re-lexing runs
/// forward from it and stops as soon as the state carried out of a line equals
/// the state the cache already holds for the next line, because from that point
/// on nothing downstream can differ.
#[derive(Debug, Clone, Default)]
pub struct StateCache {
    states: Vec<LineState>,
    /// How many leading entries of `states` describe the current text. Entries
    /// past it are padding and must never take part in a convergence test,
    /// because the padding value is also a perfectly ordinary state.
    valid: usize,
    /// First line start at which a convergence test is allowed to fire. Lines
    /// spliced in by an edit have no cached state yet, so an entry inside that
    /// window describes nothing and must not end a re-lex.
    converge_from: usize,
}

impl StateCache {
    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Build the cache for `lines` from the start.
    ///
    /// # Errors
    ///
    /// Returns [`GrammarError`] when a pattern fails at match time.
    pub fn build<S: AsRef<str>>(lexer: &Lexer, lines: &[S]) -> Result<Self, GrammarError> {
        let mut cache = Self {
            states: vec![LineState::start(); lines.len() + 1],
            valid: lines.len() + 1,
            converge_from: 0,
        };
        let mut scratch = LexScratch::new();
        let mut tokens = Vec::new();
        let mut state = LineState::start();
        for (index, line) in lines.iter().enumerate() {
            cache.states[index] = state;
            state = lexer.lex_line_into(&mut scratch, line.as_ref(), index, state, &mut tokens)?;
        }
        if let Some(last) = cache.states.last_mut() {
            *last = state;
        }
        Ok(cache)
    }

    /// The state at the start of line `index`.
    pub fn state_at(&self, index: usize) -> LineState {
        self.states
            .get(index)
            .copied()
            .unwrap_or_else(LineState::start)
    }

    /// How many line starts the cache holds, which is one more than the number
    /// of lines.
    pub fn len(&self) -> usize {
        self.states.len()
    }

    /// Whether the cache holds nothing.
    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }

    /// Drop every cached state from `index` onward.
    ///
    /// Convergence compares a freshly computed state against the cached state
    /// of the following line. After lines are inserted or removed the cached
    /// states no longer line up with the text, so a caller that changed the
    /// line count calls this before re-lexing; otherwise a stale entry can
    /// match by accident and stop the re-lex too early.
    pub fn invalidate_from(&mut self, index: usize) {
        self.valid = self.valid.min(index);
    }

    /// Account for `removed` lines at `at` being replaced by `inserted` lines.
    ///
    /// The cached states below the edit still describe the same lines, only at
    /// different indices, so they are moved rather than dropped. Keeping them
    /// is what lets a re-lex converge a few lines under the edit instead of
    /// running to the end of the file. The entries covering newly inserted
    /// lines describe nothing until they are recomputed, so convergence is
    /// barred until the re-lex has passed them.
    pub fn splice_lines(&mut self, at: usize, removed: usize, inserted: usize) {
        let at = at.min(self.states.len().saturating_sub(1));
        let removed = removed.min(self.states.len().saturating_sub(1).saturating_sub(at));
        if removed > 0 {
            self.states.drain(at..at + removed);
            if self.valid > at {
                self.valid = self.valid.saturating_sub(removed).max(at);
            }
        }
        if inserted > 0 {
            let filler = self.state_at(at);
            self.states
                .splice(at..at, std::iter::repeat_n(filler, inserted));
            if self.valid > at {
                self.valid = self.valid.saturating_add(inserted);
            }
        }
        self.converge_from = self.converge_from.max(at + inserted);
    }

    /// Re-lex from `first_changed` until the carried state converges.
    ///
    /// `lines` is the whole current file. Returns the index one past the last
    /// line that had to be re-lexed, so a caller can repaint exactly that span.
    ///
    /// # Errors
    ///
    /// Returns [`GrammarError`] when a pattern fails at match time.
    pub fn relex_from<S: AsRef<str>>(
        &mut self,
        lexer: &Lexer,
        lines: &[S],
        first_changed: usize,
    ) -> Result<usize, GrammarError> {
        self.states.resize(lines.len() + 1, LineState::start());
        let mut start = first_changed.min(lines.len());
        if start >= self.valid {
            start = self.valid.saturating_sub(1).min(lines.len());
            if self.valid == 0 {
                start = 0;
            }
        }
        let mut scratch = LexScratch::new();
        let mut tokens = Vec::new();
        let mut state = if start == 0 {
            LineState::start()
        } else {
            self.state_at(start)
        };
        let mut index = start;
        while index < lines.len() {
            self.states[index] = state;
            state = lexer.lex_line_into(
                &mut scratch,
                lines[index].as_ref(),
                index,
                state,
                &mut tokens,
            )?;
            index += 1;
            if index > start
                && index >= self.converge_from
                && index < self.valid
                && self.states.get(index) == Some(&state)
            {
                self.valid = self.states.len();
                self.converge_from = 0;
                return Ok(index);
            }
        }
        if let Some(slot) = self.states.get_mut(index) {
            *slot = state;
        }
        self.valid = self.states.len();
        self.converge_from = 0;
        Ok(index)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{LexScratch, Lexer, LineState};
    use crate::builtin;

    /// Bytes the pattern searches covered while `line` was lexed as the first
    /// line of a file.
    fn scanned(lexer: &Lexer, line: &str) -> usize {
        let mut scratch = LexScratch::new();
        let mut tokens = Vec::new();
        lexer
            .lex_line_into(&mut scratch, line, 0, LineState::start(), &mut tokens)
            .unwrap();
        assert_eq!(tokens.last().map(|token| token.range.end), Some(line.len()));
        scratch.scanned
    }

    /// A deterministic hexadecimal run with no separator, as a hex encoded
    /// blob holds.
    fn hex_like(length: usize) -> String {
        let alphabet = b"0123456789abcdef";
        let mut state: u32 = 0x1234_5678;
        (0..length)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                char::from(alphabet[(state >> 28) as usize])
            })
            .collect()
    }

    fn digit_letter_lines(length: usize) -> [String; 3] {
        [
            "1a".repeat(length / 2),
            "1e".repeat(length / 2),
            hex_like(length),
        ]
    }

    /// Inside a run of word characters a whole-word item can match nowhere,
    /// so the searches of a line cover each byte a bounded number of times
    /// however long the run is.
    #[test]
    fn a_long_digit_letter_run_lexes_in_a_linear_number_of_steps() {
        for length in [2_000, 20_000] {
            for format in builtin::formats() {
                let Ok(lexer) = Lexer::new(&format.grammar) else {
                    continue;
                };
                for line in digit_letter_lines(length) {
                    let steps = scanned(&lexer, &line);
                    assert!(
                        steps <= 8 * line.len(),
                        "{}: {steps} bytes searched for a line of {}",
                        format.name,
                        line.len()
                    );
                }
            }
        }
        for format in [builtin::c_cpp(), builtin::yaml()] {
            let lexer = Lexer::new(&format.grammar).unwrap();
            for line in digit_letter_lines(200_000) {
                let steps = scanned(&lexer, &line);
                assert!(
                    steps <= 8 * line.len(),
                    "{}: {steps} bytes searched for a line of {}",
                    format.name,
                    line.len()
                );
            }
        }
    }
}
