//! Find and replace over a [`TextBuffer`].
//!
//! Matching runs line by line. A pattern never sees a line terminator, so `^`
//! and `$` anchor to the line and `.` can never cross a line, which is what the
//! documented dialect specifies.
//!
//! The documented dialect is a subset of PCRE and is translated onto
//! `fancy-regex` before compiling. See [`translate_pattern`] for the constructs
//! that need rewriting and the dialect tests for accepted pattern spellings.

use std::ops::Range;

use fancy_regex::Regex;
use thiserror::Error;

use crate::buffer::TextBuffer;

/// Word characters for the whole-word option and for `\w`, `\d` and their
/// negations: the documented dialect is ASCII only, unlike the engine's default.
const WORD_CLASS: &str = "0-9A-Za-z_";

/// Why a search could not be prepared.
#[derive(Debug, Error)]
pub enum SearchError {
    /// The pattern did not compile after translation.
    #[error("invalid regular expression: {0}")]
    InvalidPattern(String),
    /// A predefined class was negated inside a bracket class, which a bracket
    /// class cannot express.
    #[error("\\{0} cannot be used inside a bracket class")]
    UnsupportedInClass(char),
    /// A hexadecimal escape was malformed.
    #[error("malformed hexadecimal escape in pattern")]
    MalformedHexEscape,
    /// The pattern ended with a dangling backslash.
    #[error("pattern ends with an unfinished escape")]
    DanglingEscape,
    /// The engine gave up on a line before exhausting its matches.
    #[error("pattern exceeded the backtracking limit")]
    BacktrackLimit,
    /// The caller stopped the search before it finished.
    #[error("search cancelled")]
    Cancelled,
}

/// Which parts of the buffer a search covers and how it moves.
///
/// The flags mirror the documented Find panel one for one.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug)]
pub struct SearchOptions {
    /// Interpret the search text as a regular expression rather than literal text.
    pub regex: bool,
    /// Match only text with identical capitalization.
    pub match_case: bool,
    /// Reject matches that fall inside a longer word.
    pub whole_words: bool,
    /// Continue from the opposite end of the range when one end is reached.
    pub wrap: bool,
    /// Search towards the start of the buffer instead of the end.
    pub backwards: bool,
    /// Restrict the search to a character range, as a selection does.
    pub scope: Option<Range<usize>>,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            regex: false,
            match_case: false,
            whole_words: false,
            wrap: true,
            backwards: false,
            scope: None,
        }
    }
}

/// One match, in buffer character indices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    /// The matched character range.
    pub range: Range<usize>,
    /// The line the match sits on.
    pub line: u32,
    /// The matched text.
    pub text: String,
    /// Capture group texts, group one first.
    pub groups: Vec<Option<String>>,
}

/// Every match of a whole-buffer search.
#[derive(Clone, Debug, Default)]
pub struct SearchResults {
    /// The matches found, in buffer order.
    pub matches: Vec<Match>,
    /// True when the engine gave up on at least one line, so matches are missing.
    pub limit_exceeded: bool,
}

impl std::ops::Deref for SearchResults {
    type Target = [Match];

    fn deref(&self) -> &[Match] {
        &self.matches
    }
}

/// A compiled search.
#[derive(Debug)]
pub struct Searcher {
    regex: Regex,
    options: SearchOptions,
}

impl Searcher {
    /// Compiles `pattern` under `options`.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError`] when the pattern uses a construct the dialect
    /// cannot express, or when the translated pattern does not compile.
    pub fn new(pattern: &str, options: SearchOptions) -> Result<Self, SearchError> {
        let body = if options.regex {
            translate_pattern(pattern)?
        } else {
            fancy_regex::escape(pattern).into_owned()
        };
        let mut full = String::with_capacity(body.len() + 48);
        if !options.match_case {
            full.push_str("(?i)");
        }
        if options.whole_words {
            full.push_str("(?<![");
            full.push_str(WORD_CLASS);
            full.push_str("])(?:");
            full.push_str(&body);
            full.push_str(")(?![");
            full.push_str(WORD_CLASS);
            full.push_str("])");
        } else {
            full.push_str(&body);
        }
        let regex = Regex::new(&full).map_err(|e| SearchError::InvalidPattern(e.to_string()))?;
        Ok(Self { regex, options })
    }

    /// The options this searcher was built with.
    #[must_use]
    pub fn options(&self) -> &SearchOptions {
        &self.options
    }

    fn region(&self, buffer: &TextBuffer) -> Range<usize> {
        let len = buffer.len_chars();
        match &self.options.scope {
            Some(scope) => {
                let start = scope.start.min(len);
                start..scope.end.min(len).max(start)
            }
            None => 0..len,
        }
    }

    /// The last line a scan ending at `end` should visit.
    ///
    /// A buffer whose final line carries a terminator reports one more line than
    /// it has content for. That phantom line holds no text, but `^` and other
    /// patterns that can match empty still match on it, which would make a
    /// replace over the whole buffer append a line that was not there.
    fn last_line(buffer: &TextBuffer, end: usize) -> u32 {
        let line = buffer.char_to_line(end);
        let phantom = line > 0
            && line + 1 == buffer.len_lines()
            && buffer.line_to_char(line) == buffer.len_chars();
        if phantom {
            line - 1
        } else {
            line
        }
    }

    /// All matches in the search region, in buffer order.
    #[must_use]
    pub fn find_all(&self, buffer: &TextBuffer) -> SearchResults {
        self.find_all_with_cancel(buffer, &|| false)
            .unwrap_or_default()
    }

    /// All matches, checking cancellation between lines and matches.
    ///
    /// # Errors
    /// Returns [`SearchError::Cancelled`] when the caller stops the search.
    pub fn find_all_with_cancel(
        &self,
        buffer: &TextBuffer,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<SearchResults, SearchError> {
        let region = self.region(buffer);
        let first = buffer.char_to_line(region.start);
        let last = Self::last_line(buffer, region.end);
        let mut out = SearchResults::default();
        for line in first..=last.max(first) {
            let (found, limited) =
                self.matches_in_line(buffer, line, &region, &region, cancelled)?;
            out.matches.extend(found);
            out.limit_exceeded |= limited;
        }
        Ok(out)
    }

    /// The next match at or after `from`, following the direction and wrap options.
    ///
    /// A match is selected by where it starts, so a match the caret sits inside
    /// is still reachable: it is skipped by the pass that begins at the caret and
    /// found by the wrap pass.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::BacktrackLimit`] when the engine gave up before
    /// exhausting a line, because matches past that point are unknown rather
    /// than absent.
    pub fn find_from(
        &self,
        buffer: &TextBuffer,
        from: usize,
    ) -> Result<Option<Match>, SearchError> {
        self.find_from_with_cancel(buffer, from, &|| false)
    }

    /// Find the next match, checking cancellation between lines and matches.
    ///
    /// # Errors
    /// Returns a search error or [`SearchError::Cancelled`].
    pub fn find_from_with_cancel(
        &self,
        buffer: &TextBuffer,
        from: usize,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Option<Match>, SearchError> {
        let region = self.region(buffer);
        let from = from.clamp(region.start, region.end);
        let (first, second) = if self.options.backwards {
            (region.start..from, from..region.end)
        } else {
            (from..region.end, region.start..from)
        };
        if let Some(m) = self.scan(buffer, &region, first, cancelled)? {
            return Ok(Some(m));
        }
        if !self.options.wrap {
            return Ok(None);
        }
        self.scan(buffer, &region, second, cancelled)
    }

    /// Scans `window` in the configured direction for the first match that starts in it.
    fn scan(
        &self,
        buffer: &TextBuffer,
        region: &Range<usize>,
        window: Range<usize>,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Option<Match>, SearchError> {
        if window.start >= window.end {
            return Ok(None);
        }
        let first = buffer.char_to_line(window.start);
        let last = Self::last_line(buffer, window.end);
        let mut limited = false;
        for offset in 0..=last.max(first) - first {
            let line = if self.options.backwards {
                last.max(first) - offset
            } else {
                first + offset
            };
            let (mut found, line_limited) =
                self.matches_in_line(buffer, line, region, &window, cancelled)?;
            limited |= line_limited;
            let picked = if self.options.backwards {
                found.pop()
            } else {
                found.drain(..).next()
            };
            if let Some(m) = picked {
                return Ok(Some(m));
            }
        }
        if limited {
            return Err(SearchError::BacktrackLimit);
        }
        Ok(None)
    }

    /// Every match on one line that lies inside `region` and starts inside `window`.
    ///
    /// The character offset of each match is accumulated from the previous one,
    /// because recounting from the start of the line would make a search over a
    /// long single line quadratic in its length.
    fn matches_in_line(
        &self,
        buffer: &TextBuffer,
        line: u32,
        region: &Range<usize>,
        window: &Range<usize>,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<(Vec<Match>, bool), SearchError> {
        if cancelled() {
            return Err(SearchError::Cancelled);
        }
        let Some(text) = buffer.line_text(line) else {
            return Ok((Vec::new(), false));
        };
        let base = buffer.line_to_char(line);
        let mut out = Vec::new();
        let mut cursor_byte = 0usize;
        let mut cursor_char = 0usize;
        for caps in self.regex.captures_iter(&text) {
            if cancelled() {
                return Err(SearchError::Cancelled);
            }
            let Ok(caps) = caps else {
                return Ok((out, true));
            };
            let Some(whole) = caps.get(0) else {
                return Ok((out, true));
            };
            if whole.start() >= cursor_byte {
                cursor_char += text[cursor_byte..whole.start()].chars().count();
            } else {
                cursor_char = text[..whole.start()].chars().count();
            }
            cursor_byte = whole.start();
            let start = base + cursor_char;
            let end = start + whole.as_str().chars().count();
            if start < region.start || end > region.end {
                continue;
            }
            if start < window.start || start >= window.end {
                continue;
            }
            let groups = (1..caps.len())
                .map(|i| caps.get(i).map(|m| m.as_str().to_owned()))
                .collect();
            out.push(Match {
                range: start..end,
                line,
                text: whole.as_str().to_owned(),
                groups,
            });
        }
        Ok((out, false))
    }

    /// Replaces the next match at or after `from`, returning the match replaced.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::BacktrackLimit`] when the engine gave up on a line.
    pub fn replace_next(
        &self,
        buffer: &mut TextBuffer,
        from: usize,
        replacement: &str,
    ) -> Result<Option<Match>, SearchError> {
        let Some(found) = self.find_from(buffer, from)? else {
            return Ok(None);
        };
        let text = self.expand(&found, replacement);
        buffer.replace(found.range.clone(), &text);
        Ok(Some(found))
    }

    /// Replaces every match in the search region as one undo group.
    ///
    /// Returns the number of replacements made.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::BacktrackLimit`] without touching the buffer when
    /// the engine gave up on a line, because a partial replace-all would leave
    /// matches behind with no way to tell which.
    pub fn replace_all(
        &self,
        buffer: &mut TextBuffer,
        replacement: &str,
    ) -> Result<usize, SearchError> {
        self.replace_all_with_cancel(buffer, replacement, &|| false)
    }

    /// Prepare all replacements, checking cancellation during scanning and
    /// editing. Call this on an isolated snapshot: cancellation during editing
    /// may leave that snapshot partially changed, which must not be published.
    ///
    /// # Errors
    /// Returns a search error or [`SearchError::Cancelled`].
    pub fn replace_all_with_cancel(
        &self,
        buffer: &mut TextBuffer,
        replacement: &str,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<usize, SearchError> {
        let results = self.find_all_with_cancel(buffer, cancelled)?;
        if results.limit_exceeded {
            return Err(SearchError::BacktrackLimit);
        }
        if results.matches.is_empty() {
            return Ok(0);
        }
        let mut group = buffer.group();
        // Replacing from the end keeps the earlier ranges valid.
        for found in results.matches.iter().rev() {
            if cancelled() {
                return Err(SearchError::Cancelled);
            }
            let text = self.expand(found, replacement);
            group.replace(found.range.clone(), &text);
        }
        Ok(results.matches.len())
    }

    /// Expands a replacement string against a match.
    ///
    /// Escape sequences are honored only in regular expression mode; literal
    /// searches take the replacement exactly as given.
    #[must_use]
    pub fn expand(&self, found: &Match, replacement: &str) -> String {
        if !self.options.regex {
            return replacement.to_owned();
        }
        expand_replacement(found, replacement)
    }
}

/// Expands `\0` through `\9`, `\t`, `\n`, `\r` and `\\` in a replacement string.
#[must_use]
pub fn expand_replacement(found: &Match, replacement: &str) -> String {
    let mut out = String::with_capacity(replacement.len());
    let mut chars = replacement.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            None | Some('\\') => out.push('\\'),
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('0') => out.push_str(&found.text),
            Some(d @ '1'..='9') => {
                let index = d as usize - '1' as usize;
                if let Some(Some(group)) = found.groups.get(index) {
                    out.push_str(group);
                }
            }
            Some(other) => out.push(other),
        }
    }
    out
}

/// Rewrites a pattern from the documented dialect onto `fancy-regex` syntax.
///
/// The rewrites are: `\w`, `\W`, `\d`, `\D`, `\s` and `\S` become ASCII bracket
/// classes because the engine's own versions are Unicode aware; `\A` and `\Z`
/// become `^` and `$` because matching is per line; `\e` becomes an explicit hex
/// escape; `\x{F000}` becomes a null character; and `\xnn` is normalized to
/// braced form. Everything else passes through unchanged.
///
/// # Errors
///
/// Returns [`SearchError`] for a negated predefined class inside a bracket
/// class, a malformed hexadecimal escape, or a dangling backslash.
pub fn translate_pattern(pattern: &str) -> Result<String, SearchError> {
    let mut out = String::with_capacity(pattern.len() + 16);
    let mut chars = pattern.chars().peekable();
    let mut in_class = false;
    while let Some(c) = chars.next() {
        match c {
            '[' if !in_class => {
                in_class = true;
                out.push('[');
            }
            ']' if in_class => {
                in_class = false;
                out.push(']');
            }
            '\\' => {
                let escaped = chars.next().ok_or(SearchError::DanglingEscape)?;
                translate_escape(escaped, &mut chars, in_class, &mut out)?;
            }
            other => out.push(other),
        }
    }
    Ok(out)
}

fn translate_escape(
    escaped: char,
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    in_class: bool,
    out: &mut String,
) -> Result<(), SearchError> {
    let class = |out: &mut String, body: &str, negate: bool| {
        if in_class {
            out.push_str(body);
        } else {
            out.push('[');
            if negate {
                out.push('^');
            }
            out.push_str(body);
            out.push(']');
        }
    };
    match escaped {
        'w' => class(out, WORD_CLASS, false),
        'd' => class(out, "0-9", false),
        's' => class(out, " \\t", false),
        'W' | 'D' | 'S' if in_class => return Err(SearchError::UnsupportedInClass(escaped)),
        'W' => class(out, WORD_CLASS, true),
        'D' => class(out, "0-9", true),
        'S' => class(out, " \\t", true),
        'e' => out.push_str("\\x{1B}"),
        'a' => out.push_str("\\x{07}"),
        'f' => out.push_str("\\x{0C}"),
        't' => out.push_str("\\t"),
        'A' if !in_class => out.push('^'),
        'Z' if !in_class => out.push('$'),
        'x' => translate_hex(chars, out)?,
        other => {
            out.push('\\');
            out.push(other);
        }
    }
    Ok(())
}

fn translate_hex(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    out: &mut String,
) -> Result<(), SearchError> {
    if chars.peek() == Some(&'{') {
        chars.next();
        let mut digits = String::new();
        loop {
            match chars.next() {
                Some('}') => break,
                Some(d) if d.is_ascii_hexdigit() => digits.push(d),
                _ => return Err(SearchError::MalformedHexEscape),
            }
        }
        if digits.is_empty() {
            return Err(SearchError::MalformedHexEscape);
        }
        // The reserved spelling for a character with a null value.
        if digits.eq_ignore_ascii_case("F000") {
            out.push_str("\\x{0}");
        } else {
            out.push_str("\\x{");
            out.push_str(&digits);
            out.push('}');
        }
        return Ok(());
    }
    let mut digits = String::with_capacity(2);
    for _ in 0..2 {
        match chars.next() {
            Some(d) if d.is_ascii_hexdigit() => digits.push(d),
            _ => return Err(SearchError::MalformedHexEscape),
        }
    }
    out.push_str("\\x{");
    out.push_str(&digits);
    out.push('}');
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn worker_scans_and_replacements_observe_cancellation() {
        let searcher = Searcher::new("cat", SearchOptions::default()).unwrap();
        let buffer = TextBuffer::from_text(&"cat cat\n".repeat(100));
        let calls = std::cell::Cell::new(0);
        let cancelled = || {
            calls.set(calls.get() + 1);
            calls.get() > 5
        };
        assert!(matches!(
            searcher.find_all_with_cancel(&buffer, &cancelled),
            Err(SearchError::Cancelled)
        ));
        let absent = Searcher::new("dog", SearchOptions::default()).unwrap();
        calls.set(0);
        assert!(matches!(
            absent.find_from_with_cancel(&buffer, 0, &cancelled),
            Err(SearchError::Cancelled)
        ));
        calls.set(0);
        let mut snapshot = buffer.edit_snapshot();
        assert!(matches!(
            searcher.replace_all_with_cancel(snapshot.buffer_mut(), "cow", &cancelled),
            Err(SearchError::Cancelled)
        ));
        assert_eq!(buffer.text(), "cat cat\n".repeat(100));
    }

    fn buf() -> TextBuffer {
        TextBuffer::from_text("apple pie\nsnapple\nApple apples\n")
    }

    #[test]
    fn literal_search_is_case_insensitive_by_default() {
        let s = Searcher::new("apple", SearchOptions::default()).unwrap();
        assert_eq!(s.find_all(&buf()).len(), 4);
    }

    #[test]
    fn match_case_restricts_matches() {
        let options = SearchOptions {
            match_case: true,
            ..SearchOptions::default()
        };
        let s = Searcher::new("Apple", options).unwrap();
        let found = s.find_all(&buf());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].line, 2);
    }

    #[test]
    fn whole_words_rejects_inner_matches() {
        let options = SearchOptions {
            whole_words: true,
            ..SearchOptions::default()
        };
        let s = Searcher::new("apple", options).unwrap();
        let found = s.find_all(&buf());
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].line, 0);
        assert_eq!(found[1].line, 2);
    }

    #[test]
    fn literal_mode_does_not_treat_metacharacters_specially() {
        let s = Searcher::new("a.c", SearchOptions::default()).unwrap();
        let b = TextBuffer::from_text("abc\na.c\n");
        let found = s.find_all(&b);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].line, 1);
    }

    #[test]
    fn forward_and_backward_from_a_position() {
        let b = buf();
        let s = Searcher::new("apple", SearchOptions::default()).unwrap();
        let first = s.find_from(&b, 0).unwrap().unwrap();
        assert_eq!(first.range.start, 0);
        let second = s.find_from(&b, first.range.end).unwrap().unwrap();
        assert_eq!(second.line, 1);

        let options = SearchOptions {
            backwards: true,
            ..SearchOptions::default()
        };
        let back = Searcher::new("apple", options).unwrap();
        let m = back.find_from(&b, b.len_chars()).unwrap().unwrap();
        assert_eq!(m.line, 2);
    }

    #[test]
    fn wrap_returns_to_the_start() {
        let b = TextBuffer::from_text("needle\nhay\n");
        let s = Searcher::new("needle", SearchOptions::default()).unwrap();
        let m = s.find_from(&b, b.len_chars()).unwrap().unwrap();
        assert_eq!(m.line, 0);

        let options = SearchOptions {
            wrap: false,
            ..SearchOptions::default()
        };
        let s = Searcher::new("needle", options).unwrap();
        assert!(s.find_from(&b, b.len_chars()).unwrap().is_none());
    }

    #[test]
    fn scope_limits_the_search_to_a_selection() {
        let b = buf();
        let options = SearchOptions {
            scope: Some(10..b.len_chars()),
            ..SearchOptions::default()
        };
        let s = Searcher::new("apple", options).unwrap();
        let found = s.find_all(&b);
        assert!(found.iter().all(|m| m.line >= 1));
    }

    #[test]
    fn regex_anchors_are_per_line() {
        let b = TextBuffer::from_text("  end;  \nend of line\n");
        let options = SearchOptions {
            regex: true,
            ..SearchOptions::default()
        };
        let s = Searcher::new(r"^\s*end;?\s*$", options).unwrap();
        let found = s.find_all(&b);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].line, 0);
    }

    #[test]
    fn back_references_work() {
        let b = TextBuffer::from_text("been boon bean\n");
        let options = SearchOptions {
            regex: true,
            ..SearchOptions::default()
        };
        let s = Searcher::new(r"b(.)\1n", options).unwrap();
        let found = s.find_all(&b);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].text, "been");
        assert_eq!(found[1].text, "boon");
    }

    #[test]
    fn leftmost_alternative_wins() {
        let b = TextBuffer::from_text("beyond\n");
        let options = SearchOptions {
            regex: true,
            ..SearchOptions::default()
        };
        let s = Searcher::new("bey|beyond", options).unwrap();
        assert_eq!(s.find_all(&b)[0].text, "bey");
    }

    #[test]
    fn whitespace_class_is_space_or_tab_only() {
        let b = TextBuffer::from_text("a\u{0B}b\ta\n");
        let options = SearchOptions {
            regex: true,
            ..SearchOptions::default()
        };
        let s = Searcher::new(r"\s", options).unwrap();
        let found = s.find_all(&b);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].text, "\t");
    }

    #[test]
    fn word_class_is_ascii_only() {
        let b = TextBuffer::from_text("aé\n");
        let options = SearchOptions {
            regex: true,
            match_case: true,
            ..SearchOptions::default()
        };
        let s = Searcher::new(r"\w+", options).unwrap();
        assert_eq!(s.find_all(&b)[0].text, "a");
    }

    #[test]
    fn predefined_class_inside_brackets() {
        assert_eq!(translate_pattern(r"[\w-]").unwrap(), "[0-9A-Za-z_-]");
        assert!(matches!(
            translate_pattern(r"[\W]"),
            Err(SearchError::UnsupportedInClass('W'))
        ));
    }

    #[test]
    fn hex_escapes_are_normalized() {
        assert_eq!(translate_pattern(r"\x41").unwrap(), r"\x{41}");
        assert_eq!(translate_pattern(r"\x{2603}").unwrap(), r"\x{2603}");
        assert_eq!(translate_pattern(r"\x{F000}").unwrap(), r"\x{0}");
        assert!(translate_pattern(r"\xZZ").is_err());
        assert!(translate_pattern("ab\\").is_err());
    }

    #[test]
    fn text_anchors_become_line_anchors() {
        assert_eq!(translate_pattern(r"\Aab\Z").unwrap(), "^ab$");
    }

    #[test]
    fn inline_case_modifier_survives_translation() {
        let b = TextBuffer::from_text("ABC abc\n");
        let options = SearchOptions {
            regex: true,
            match_case: true,
            ..SearchOptions::default()
        };
        let s = Searcher::new("(?i)abc", options).unwrap();
        assert_eq!(s.find_all(&b).len(), 2);
    }

    #[test]
    fn replace_all_is_one_undo_group() {
        let mut b = TextBuffer::from_text("a a a\n");
        let s = Searcher::new("a", SearchOptions::default()).unwrap();
        assert_eq!(s.replace_all(&mut b, "z").unwrap(), 3);
        assert_eq!(b.text(), "z z z\n");
        assert!(b.undo().unwrap());
        assert_eq!(b.text(), "a a a\n");
    }

    #[test]
    fn regex_replacement_expands_groups_and_escapes() {
        let mut b = TextBuffer::from_text("john smith\n");
        let options = SearchOptions {
            regex: true,
            ..SearchOptions::default()
        };
        let s = Searcher::new(r"(\w+) (\w+)", options).unwrap();
        assert_eq!(s.replace_all(&mut b, r"\2,\t\1").unwrap(), 1);
        assert_eq!(b.text(), "smith,\tjohn\n");
    }

    #[test]
    fn literal_replacement_keeps_backslashes() {
        let mut b = TextBuffer::from_text("x\n");
        let s = Searcher::new("x", SearchOptions::default()).unwrap();
        s.replace_all(&mut b, r"a\tb").unwrap();
        assert_eq!(b.text(), "a\\tb\n");
    }

    #[test]
    fn replace_next_replaces_one() {
        let mut b = TextBuffer::from_text("a a\n");
        let s = Searcher::new("a", SearchOptions::default()).unwrap();
        assert!(s.replace_next(&mut b, 0, "z").unwrap().is_some());
        assert_eq!(b.text(), "z a\n");
    }

    #[test]
    fn find_from_finds_a_match_straddling_the_caret() {
        let b = TextBuffer::from_text("aXbc\n");
        let s = Searcher::new("Xb", SearchOptions::default()).unwrap();
        let m = s.find_from(&b, 2).unwrap().unwrap();
        assert_eq!(m.range, 1..3);
    }

    #[test]
    fn line_start_anchor_skips_the_phantom_final_line() {
        let mut b = TextBuffer::from_text("a\nb\n");
        let options = SearchOptions {
            regex: true,
            ..SearchOptions::default()
        };
        let s = Searcher::new("^", options).unwrap();
        assert_eq!(s.replace_all(&mut b, "> ").unwrap(), 2);
        assert_eq!(b.text(), "> a\n> b\n");
    }

    #[test]
    fn empty_matches_land_once_per_position() {
        let b = TextBuffer::from_text("ab\n");
        let options = SearchOptions {
            regex: true,
            ..SearchOptions::default()
        };
        let s = Searcher::new("x*", options).unwrap();
        let found = s.find_all(&b);
        assert!(!found.limit_exceeded);
        assert_eq!(found.len(), 3);
        assert!(found.iter().all(|m| m.range.is_empty()));
        assert_eq!(found[2].range, 2..2);
    }

    #[test]
    fn a_backtracking_blowup_is_reported() {
        let b = TextBuffer::from_text(&format!("{}!\n", "a".repeat(40)));
        let options = SearchOptions {
            regex: true,
            match_case: true,
            ..SearchOptions::default()
        };
        let s = Searcher::new("((?=a)a+)+b", options).unwrap();
        assert!(s.find_all(&b).limit_exceeded);
        assert!(matches!(
            s.find_from(&b, 0),
            Err(SearchError::BacktrackLimit)
        ));
        assert!(matches!(
            s.replace_all(&mut b.clone(), "z"),
            Err(SearchError::BacktrackLimit)
        ));
    }

    #[test]
    fn a_long_single_line_is_not_quadratic() {
        let mut text = "ab".repeat(200_000);
        text.push('\n');
        let b = TextBuffer::from_text(&text);
        let options = SearchOptions {
            match_case: true,
            ..SearchOptions::default()
        };
        let s = Searcher::new("a", options).unwrap();
        let found = s.find_all(&b);
        assert_eq!(found.len(), 200_000);
        assert_eq!(found[199_999].range, 399_998..399_999);
    }

    #[test]
    fn empty_buffer_yields_no_matches() {
        let b = TextBuffer::new();
        let s = Searcher::new("a", SearchOptions::default()).unwrap();
        assert!(s.find_all(&b).is_empty());
        assert!(s.find_from(&b, 0).unwrap().is_none());
    }
}
