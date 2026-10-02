//! Classification of differences as important or unimportant.
//!
//! The engine does not know any file syntax. It asks a [`LineClassifier`],
//! supplied by the grammar layer, which category each run of characters in a
//! line belongs to, then consults a [`RuleSet`] for whether that category
//! counts. The result lets a view color the two classes separately and lets a
//! filter treat unimportant differences as identical.

use crate::inline::{diff_tokens_with, InlineOptions, Span};
use crate::lines::{Hunk, HunkKind};
use crate::DiffError;
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

#[cfg(test)]
std::thread_local! {
    static SPAN_TOKEN_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn record_span_token_visit() {
    SPAN_TOKEN_VISITS.with(|visits| visits.set(visits.get().saturating_add(1)));
}

/// Category a classifier assigns to a run of characters within a line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TokenCategory {
    /// Whitespace before the first non-whitespace character.
    LeadingWhitespace,
    /// Whitespace between non-whitespace runs.
    EmbeddedWhitespace,
    /// Whitespace after the last non-whitespace character.
    TrailingWhitespace,
    /// Text claimed by the named grammar element.
    Element(String),
    /// Non-whitespace text that no grammar element claims.
    EverythingElse,
}

/// One categorized run of characters within a line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassifiedToken {
    /// Byte range within the line.
    pub range: Range<u32>,
    /// What the run is.
    pub category: TokenCategory,
}

/// Supplies the category of every run of characters in a line.
pub trait LineClassifier {
    /// Split `line` into non-overlapping runs in increasing order. The runs
    /// are expected to cover the line; text no run covers is treated as
    /// [`TokenCategory::EverythingElse`].
    fn classify_line(&self, line: &str) -> Vec<ClassifiedToken>;
}

/// Which of the two compared texts a line belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClassifierSide {
    /// The left text.
    Left,
    /// The right text.
    Right,
}

/// Supplies the category of every run of characters in a line, knowing where
/// that line sits.
///
/// A syntax element can begin on one line and end on a later one, so the
/// category of a run can depend on lines the engine never passes in. A
/// classifier told the side and the line number can look the carried-in state
/// up for itself; one that cannot is adapted by the blanket implementation
/// below and behaves exactly as it does through [`LineClassifier`].
pub trait IndexedLineClassifier {
    /// Split `line`, which is line `line_index` of `side`, into
    /// non-overlapping runs in increasing order.
    fn classify_line_at(
        &self,
        side: ClassifierSide,
        line_index: usize,
        line: &str,
    ) -> Vec<ClassifiedToken>;
}

impl<T: LineClassifier + ?Sized> IndexedLineClassifier for T {
    fn classify_line_at(
        &self,
        _side: ClassifierSide,
        _line_index: usize,
        line: &str,
    ) -> Vec<ClassifiedToken> {
        self.classify_line(line)
    }
}

/// Classifier that recognises only whitespace position, used until a grammar
/// is available.
#[derive(Debug, Clone, Copy, Default)]
pub struct WhitespaceClassifier;

impl LineClassifier for WhitespaceClassifier {
    fn classify_line(&self, line: &str) -> Vec<ClassifiedToken> {
        let body_len = line_body_len(line);
        let first = line[..body_len].find(|c: char| !is_rule_whitespace(c));
        let last = line[..body_len].rfind(|c: char| !is_rule_whitespace(c));
        let mut out = Vec::new();
        let mut run_start: Option<usize> = None;
        let mut run_ws = false;
        let push = |start: usize, end: usize, ws: bool, out: &mut Vec<ClassifiedToken>| {
            if start >= end {
                return;
            }
            let category = if ws {
                match (first, last) {
                    (Some(f), _) if end <= f => TokenCategory::LeadingWhitespace,
                    (_, Some(l)) if start > l => TokenCategory::TrailingWhitespace,
                    (Some(_), Some(_)) => TokenCategory::EmbeddedWhitespace,
                    _ => TokenCategory::TrailingWhitespace,
                }
            } else {
                TokenCategory::EverythingElse
            };
            out.push(ClassifiedToken {
                range: u32::try_from(start).unwrap_or(u32::MAX)
                    ..u32::try_from(end).unwrap_or(u32::MAX),
                category,
            });
        };
        for (idx, c) in line.char_indices() {
            if idx >= body_len {
                break;
            }
            let ws = is_rule_whitespace(c);
            match run_start {
                Some(s) if ws != run_ws => {
                    push(s, idx, run_ws, &mut out);
                    run_start = Some(idx);
                    run_ws = ws;
                }
                Some(_) => {}
                None => {
                    run_start = Some(idx);
                    run_ws = ws;
                }
            }
        }
        if let Some(s) = run_start {
            push(s, body_len, run_ws, &mut out);
        }
        out
    }
}

fn is_rule_whitespace(character: char) -> bool {
    matches!(character, ' ' | '\t')
}

/// Which pane a replacement's find text is searched in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ReplacementSide {
    /// Search the left pane and compare the result against the right.
    #[default]
    Left,
    /// Search the right pane and compare the result against the left.
    Right,
}

/// Switches on one replacement rule.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplacementOptions {
    /// Only match text with identical capitalization.
    pub match_character_case: bool,
    /// Refuse matches that fall inside a longer word.
    pub whole_words_only: bool,
    /// Treat both fields as regular expressions rather than literal text.
    pub regular_expression: bool,
    /// Pane the find text applies to.
    pub side: ReplacementSide,
}

/// A substitution declaring that two spellings mean the same thing.
#[derive(Debug, Clone)]
pub struct ReplacementRule {
    find: Regex,
    replace: String,
    side: ReplacementSide,
}

impl ReplacementRule {
    /// Compile a replacement rule.
    ///
    /// # Errors
    ///
    /// Returns [`DiffError::Regex`] when `find` is marked as a regular
    /// expression and does not compile.
    pub fn new(find: &str, replace: &str, options: ReplacementOptions) -> Result<Self, DiffError> {
        let mut pattern = if options.regular_expression {
            find.to_owned()
        } else {
            regex::escape(find)
        };
        if options.whole_words_only {
            pattern = format!(r"\b(?:{pattern})\b");
        }
        let compiled = RegexBuilder::new(&pattern)
            .case_insensitive(!options.match_character_case)
            .build()
            .map_err(|e| DiffError::Regex {
                pattern: pattern.clone(),
                message: e.to_string(),
            })?;
        let replace = if options.regular_expression {
            replace.to_owned()
        } else {
            replace.replace('$', "$$")
        };
        Ok(Self {
            find: compiled,
            replace,
            side: options.side,
        })
    }

    /// Pane this rule's find text is searched in.
    #[must_use]
    pub const fn side(&self) -> ReplacementSide {
        self.side
    }

    /// Apply the rule to one line of the side it targets.
    #[must_use]
    pub fn apply(&self, text: &str) -> String {
        self.find
            .replace_all(text, self.replace.as_str())
            .into_owned()
    }
}

/// Everything that decides whether a difference counts.
#[derive(Debug, Clone, Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct RuleSet {
    /// Names of the grammar elements whose differences are important.
    pub important_elements: BTreeSet<String>,
    /// Names of elements whose text remains case sensitive when the global
    /// character case rule is off.
    pub case_sensitive_elements: BTreeSet<String>,
    /// Whitespace at the start of a line is an important difference.
    pub leading_whitespace_important: bool,
    /// Whitespace in the middle of a line is an important difference.
    pub embedded_whitespace_important: bool,
    /// Whitespace at the end of a line is an important difference.
    pub trailing_whitespace_important: bool,
    /// Text no grammar element claims is an important difference.
    pub everything_else_important: bool,
    /// Unclaimed text compares case-sensitively.
    pub match_character_case: bool,
    /// An orphan line is important even when it holds only unimportant text.
    pub orphan_lines_always_important: bool,
    /// A difference in the line terminator is important.
    pub compare_line_endings: bool,
    /// Text fully matched by one of these patterns is never important. Every
    /// pattern is anchored to both ends of the text when it is compiled, so a
    /// pattern's alternation order cannot change the verdict.
    unimportance_patterns: Vec<Regex>,
    /// Substitutions that make two spellings equivalent.
    pub replacements: Vec<ReplacementRule>,
}

impl RuleSet {
    /// A rule set in which every difference is important.
    #[must_use]
    pub fn all_important() -> Self {
        Self {
            important_elements: BTreeSet::new(),
            case_sensitive_elements: BTreeSet::new(),
            leading_whitespace_important: true,
            embedded_whitespace_important: true,
            trailing_whitespace_important: true,
            everything_else_important: true,
            match_character_case: true,
            orphan_lines_always_important: true,
            compare_line_endings: false,
            unimportance_patterns: Vec::new(),
            replacements: Vec::new(),
        }
    }

    /// Add an unimportance pattern.
    ///
    /// # Errors
    ///
    /// Returns [`DiffError::Regex`] when the pattern does not compile.
    pub fn push_unimportance_pattern(&mut self, pattern: &str) -> Result<(), DiffError> {
        // Anchoring at compile time is what makes the test a whole-text match:
        // searching for the pattern and then checking the match bounds accepts
        // or rejects depending on which alternative the engine finds first.
        let anchored = format!("^(?:{pattern})$");
        let compiled = Regex::new(&anchored).map_err(|e| DiffError::Regex {
            pattern: pattern.to_owned(),
            message: e.to_string(),
        })?;
        self.unimportance_patterns.push(compiled);
        Ok(())
    }

    /// The compiled unimportance patterns, each anchored to both ends.
    #[must_use]
    pub fn unimportance_patterns(&self) -> &[Regex] {
        &self.unimportance_patterns
    }

    fn category_important(&self, category: &TokenCategory) -> bool {
        match category {
            TokenCategory::LeadingWhitespace => self.leading_whitespace_important,
            TokenCategory::EmbeddedWhitespace => self.embedded_whitespace_important,
            TokenCategory::TrailingWhitespace => self.trailing_whitespace_important,
            TokenCategory::Element(name) => self.important_elements.contains(name),
            TokenCategory::EverythingElse => self.everything_else_important,
        }
    }

    fn matched_unimportant(&self, text: &str) -> bool {
        if text.is_empty() {
            return true;
        }
        self.unimportance_patterns
            .iter()
            .any(|re| re.is_match(text))
    }
}

/// Whether a difference is worth the user's attention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Importance {
    /// Colored as a real difference.
    Important,
    /// Colored separately and suppressed by the ignore-unimportant filter.
    Unimportant,
}

/// One hunk together with its classification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassifiedHunk {
    /// The hunk being classified.
    pub hunk: Hunk,
    /// Importance of the whole hunk. `None` for [`HunkKind::Same`], which is
    /// not a difference at all.
    pub importance: Option<Importance>,
    /// Importance of each left line in the hunk, in order.
    pub left_lines: Vec<Importance>,
    /// Importance of each right line in the hunk, in order.
    pub right_lines: Vec<Importance>,
}

fn line_text<'a>(lines: &[&'a str], index: u32) -> &'a str {
    lines.get(index as usize).copied().unwrap_or_default()
}

fn slice(line: &str, span: Span) -> &str {
    line.get(span.start as usize..span.end as usize)
        .unwrap_or_default()
}

/// Return the byte length of a line without its one actual terminator.
///
/// A CR immediately before LF belongs to the CRLF terminator. Any earlier CR
/// remains part of the line body and is classified as content.
fn line_body_len(line: &str) -> usize {
    if line.ends_with("\r\n") {
        line.len().saturating_sub(2)
    } else if line.ends_with(['\r', '\n']) {
        line.len().saturating_sub(1)
    } else {
        line.len()
    }
}

fn line_ending(line: &str) -> &str {
    if line.ends_with("\r\n") {
        "\r\n"
    } else if line.ends_with('\r') {
        "\r"
    } else if line.ends_with('\n') {
        "\n"
    } else {
        ""
    }
}

/// Trim a span down to the line body. Which terminator ends a line is decided
/// by the line ending comparison option, never by importance rules.
fn clamp_to_body(line: &str, span: Span) -> Span {
    let body = u32::try_from(line_body_len(line)).unwrap_or(u32::MAX);
    Span {
        start: span.start.min(body),
        end: span.end.min(body),
    }
}

/// Importance of one span of a line, given the classifier's runs.
///
/// The runs are walked in order behind a cursor so that every stretch no run
/// claims is seen, including one before the first overlapping run and one
/// after the last. A classifier that starts partway into the line leaves the
/// text ahead of it unclaimed, which is a difference under the unclaimed-text
/// rule just as a gap in the middle is.
fn span_importance(
    rules: &RuleSet,
    tokens: &[ClassifiedToken],
    token_cursor: &mut usize,
    span: Span,
) -> Importance {
    if span.is_empty() {
        return Importance::Unimportant;
    }
    while let Some(token) = tokens.get(*token_cursor) {
        #[cfg(test)]
        record_span_token_visit();
        if token.range.end > span.start {
            break;
        }
        *token_cursor = (*token_cursor).saturating_add(1);
    }
    let mut cursor = span.start;
    while let Some(token) = tokens.get(*token_cursor) {
        #[cfg(test)]
        record_span_token_visit();
        if token.range.start >= span.end {
            break;
        }
        if token.range.start > cursor && rules.everything_else_important {
            return Importance::Important;
        }
        if rules.category_important(&token.category) {
            return Importance::Important;
        }
        cursor = cursor.max(token.range.end);
        if token.range.end <= span.end {
            *token_cursor = (*token_cursor).saturating_add(1);
        } else {
            // One token can cover several disjoint inline spans. Keep its
            // index for the next span unless this span consumed it entirely.
            break;
        }
    }
    if cursor < span.end && rules.everything_else_important {
        return Importance::Important;
    }
    Importance::Unimportant
}

fn token_category_at<'a>(
    tokens: &'a [ClassifiedToken],
    cursor: &mut usize,
    position: u32,
) -> Option<&'a TokenCategory> {
    while tokens
        .get(*cursor)
        .is_some_and(|token| token.range.end <= position)
    {
        *cursor = cursor.saturating_add(1);
    }
    tokens
        .get(*cursor)
        .filter(|token| token.range.start <= position && position < token.range.end)
        .map(|token| &token.category)
}

fn is_case_sensitive_category(rules: &RuleSet, category: Option<&TokenCategory>) -> bool {
    matches!(category, Some(TokenCategory::Element(name)) if rules.case_sensitive_elements.contains(name))
}

struct ClassifiedSide<'a> {
    text: &'a str,
    map: &'a OffsetMap,
    tokens: &'a [ClassifiedToken],
    cursor: usize,
}

/// Importance carried by text that matches on both sides but belongs to
/// different grammar elements. A token changing from code to comment (or back)
/// is itself meaningful even when comments are unchecked.
fn matched_text_category_importance(
    rules: &RuleSet,
    left: &mut ClassifiedSide<'_>,
    right: &mut ClassifiedSide<'_>,
    left_span: Span,
    right_span: Span,
) -> (bool, bool) {
    let left_text = slice(left.text, left_span);
    let right_text = slice(right.text, right_span);
    let mut left_importance = false;
    let mut right_importance = false;
    let mut right_units = right_text.grapheme_indices(true);
    for (left_offset, left_unit) in left_text.grapheme_indices(true) {
        let Some((right_offset, right_unit)) = right_units.next() else {
            break;
        };
        if !equal_under_case(rules, left_unit, right_unit) {
            break;
        }
        let left_position = left_span
            .start
            .saturating_add(u32::try_from(left_offset).unwrap_or(u32::MAX));
        let right_position = right_span
            .start
            .saturating_add(u32::try_from(right_offset).unwrap_or(u32::MAX));
        let left_category = token_category_at(left.tokens, &mut left.cursor, left_position);
        let right_category = token_category_at(right.tokens, &mut right.cursor, right_position);
        let left_important = left_category.map_or(rules.everything_else_important, |category| {
            rules.category_important(category)
        });
        let right_important = right_category.map_or(rules.everything_else_important, |category| {
            rules.category_important(category)
        });
        if left_important != right_important {
            left_importance |= left_important;
            right_importance |= right_important;
        }
        if left_unit != right_unit
            && (is_case_sensitive_category(rules, left_category)
                || is_case_sensitive_category(rules, right_category))
        {
            left_importance |= left_important;
            right_importance |= right_important;
        }
    }
    (left_importance, right_importance)
}

/// Find equal text between changed spans whose grammar elements have different
/// importance. The inline diff supplies matching regions as the gaps between
/// its changed spans, so repeated words are matched at their actual positions.
fn matched_category_changes(
    rules: &RuleSet,
    left: &mut ClassifiedSide<'_>,
    right: &mut ClassifiedSide<'_>,
    inline: &crate::inline::InlineDiff,
) -> (bool, bool) {
    let left_body =
        u32::try_from(left.text.trim_end_matches(['\r', '\n']).len()).unwrap_or(u32::MAX);
    let right_body =
        u32::try_from(right.text.trim_end_matches(['\r', '\n']).len()).unwrap_or(u32::MAX);
    let mut left_cursor = 0;
    let mut right_cursor = 0;
    let mut importance = (false, false);

    for (left_change, right_change) in inline.left.iter().zip(&inline.right) {
        let left_start = left_change.start.min(left_body).max(left_cursor);
        let right_start = right_change.start.min(right_body).max(right_cursor);
        let left_common = left.map.map(Span {
            start: left_cursor,
            end: left_start,
        });
        let right_common = right.map.map(Span {
            start: right_cursor,
            end: right_start,
        });
        let crossed =
            matched_text_category_importance(rules, left, right, left_common, right_common);
        importance.0 |= crossed.0;
        importance.1 |= crossed.1;

        left_cursor = left_change.end.min(left_body).max(left_cursor);
        right_cursor = right_change.end.min(right_body).max(right_cursor);
    }

    let left_common = left.map.map(Span {
        start: left_cursor,
        end: left_body.max(left_cursor),
    });
    let right_common = right.map.map(Span {
        start: right_cursor,
        end: right_body.max(right_cursor),
    });
    let crossed = matched_text_category_importance(rules, left, right, left_common, right_common);
    importance.0 |= crossed.0;
    importance.1 |= crossed.1;
    importance
}

/// Maps byte offsets in a rewritten line back onto the line as the user wrote
/// it, so importance can be decided on the rewritten text while the spans that
/// decide it still name real positions in the original.
/// Ascending, gap free pairs of (rewritten range, original range).
type Segments = Vec<(Range<u32>, Range<u32>)>;

#[derive(Debug, Clone)]
struct OffsetMap {
    segments: Segments,
}

impl OffsetMap {
    fn identity(len: usize) -> Self {
        let n = u32::try_from(len).unwrap_or(u32::MAX);
        Self {
            segments: vec![(0..n, 0..n)],
        }
    }

    fn segment_at(&self, pos: u32) -> Option<&(Range<u32>, Range<u32>)> {
        self.segments
            .iter()
            .find(|(new, _)| new.start <= pos && pos < new.end)
            .or_else(|| self.segments.last())
    }

    /// Original offset a rewritten start offset came from.
    fn start_of(&self, pos: u32) -> u32 {
        match self.segment_at(pos) {
            Some((new, orig)) if new.end - new.start == orig.end - orig.start => {
                orig.start + pos.saturating_sub(new.start)
            }
            Some((_, orig)) => orig.start,
            None => 0,
        }
    }

    /// Original offset a rewritten end offset came from.
    fn end_of(&self, pos: u32) -> u32 {
        if pos == 0 {
            return 0;
        }
        match self.segment_at(pos - 1) {
            Some((new, orig)) if new.end - new.start == orig.end - orig.start => {
                orig.start + pos.saturating_sub(new.start)
            }
            Some((_, orig)) => orig.end,
            None => 0,
        }
    }

    fn map(&self, span: Span) -> Span {
        let start = self.start_of(span.start);
        let end = self.end_of(span.end).max(start);
        Span { start, end }
    }

    fn map_range(&self, range: &Range<u32>) -> Range<u32> {
        let start = self.start_of(range.start);
        start..self.end_of(range.end).max(start)
    }

    /// Compose a map onto a further rewrite: `later` runs from the newest text
    /// to this map's rewritten text, and the result runs to the original.
    fn compose(&self, later: Segments) -> Self {
        Self {
            segments: later
                .into_iter()
                .map(|(newest, middle)| (newest, self.map_range(&middle)))
                .collect(),
        }
    }
}

/// Apply one rule, recording where each stretch of the result came from.
fn rewrite(rule: &ReplacementRule, text: &str) -> (String, Segments) {
    let mut out = String::with_capacity(text.len());
    let mut segments: Segments = Vec::new();
    let mut last = 0usize;
    let at = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
    for caps in rule.find.captures_iter(text) {
        let Some(m) = caps.get(0) else { continue };
        if m.start() > last {
            let from = at(out.len());
            out.push_str(text.get(last..m.start()).unwrap_or_default());
            segments.push((from..at(out.len()), at(last)..at(m.start())));
        }
        let from = at(out.len());
        caps.expand(&rule.replace, &mut out);
        segments.push((from..at(out.len()), at(m.start())..at(m.end())));
        last = m.end();
    }
    if last < text.len() {
        let from = at(out.len());
        out.push_str(text.get(last..).unwrap_or_default());
        segments.push((from..at(out.len()), at(last)..at(text.len())));
    }
    if segments.is_empty() {
        segments.push((0..0, 0..at(text.len())));
    }
    (out, segments)
}

/// Rewrite one side under every rule that targets it.
fn apply_side(rules: &RuleSet, text: &str, side: ReplacementSide) -> (String, OffsetMap) {
    let mut current = text.to_owned();
    let mut map = OffsetMap::identity(text.len());
    for rule in rules.replacements.iter().filter(|r| r.side() == side) {
        let (next, segments) = rewrite(rule, &current);
        map = map.compose(segments);
        current = next;
    }
    (current, map)
}

fn equal_under_case(rules: &RuleSet, a: &str, b: &str) -> bool {
    if rules.match_character_case {
        a == b
    } else {
        a.eq_ignore_ascii_case(b) || a.to_lowercase() == b.to_lowercase()
    }
}

/// Classify one changed line pair.
fn changed_pair<C: IndexedLineClassifier + ?Sized>(
    rules: &RuleSet,
    classifier: &C,
    left: &str,
    right: &str,
    left_index: usize,
    right_index: usize,
) -> (Importance, Importance) {
    // Importance is decided on the rewritten text, so a rule that leaves the
    // two sides differing only in text some other rule calls unimportant still
    // suppresses the difference. Spans are mapped back before they are used,
    // so they name positions in the line the user sees.
    let (l, lmap) = apply_side(rules, left, ReplacementSide::Left);
    let (r, rmap) = apply_side(rules, right, ReplacementSide::Right);
    let left_ending = line_ending(left);
    let right_ending = line_ending(right);
    let line_ending_changed = left_ending.is_empty() != right_ending.is_empty()
        || (rules.compare_line_endings && left_ending != right_ending);
    let inline = diff_tokens_with(
        &l,
        &r,
        InlineOptions {
            ignore_case: !rules.match_character_case,
        },
    );
    let mut left_tokens = classifier.classify_line_at(ClassifierSide::Left, left_index, left);
    let mut right_tokens = classifier.classify_line_at(ClassifierSide::Right, right_index, right);
    left_tokens.sort_unstable_by_key(|token| (token.range.start, token.range.end));
    right_tokens.sort_unstable_by_key(|token| (token.range.start, token.range.end));
    let mut left_classified = ClassifiedSide {
        text: left,
        map: &lmap,
        tokens: &left_tokens,
        cursor: 0,
    };
    let mut right_classified = ClassifiedSide {
        text: right,
        map: &rmap,
        tokens: &right_tokens,
        cursor: 0,
    };
    let (left_category_change, right_category_change) =
        matched_category_changes(rules, &mut left_classified, &mut right_classified, &inline);
    let mut left_importance = if left_category_change || line_ending_changed {
        Importance::Important
    } else {
        Importance::Unimportant
    };
    let mut right_importance = if right_category_change || line_ending_changed {
        Importance::Important
    } else {
        Importance::Unimportant
    };
    let mut left_token_cursor = 0;
    let mut right_token_cursor = 0;
    for span in &inline.left {
        let span = clamp_to_body(left, lmap.map(*span));
        let text = slice(left, span);
        if rules.matched_unimportant(text) {
            continue;
        }
        if span_importance(rules, &left_tokens, &mut left_token_cursor, span)
            == Importance::Important
        {
            left_importance = Importance::Important;
        }
    }
    for span in &inline.right {
        let span = clamp_to_body(right, rmap.map(*span));
        let text = slice(right, span);
        if rules.matched_unimportant(text) {
            continue;
        }
        if span_importance(rules, &right_tokens, &mut right_token_cursor, span)
            == Importance::Important
        {
            right_importance = Importance::Important;
        }
    }
    (left_importance, right_importance)
}

/// Classify one orphan line: a line present on only one side.
fn orphan_line<C: IndexedLineClassifier + ?Sized>(
    rules: &RuleSet,
    classifier: &C,
    line: &str,
    side: ClassifierSide,
    line_index: usize,
) -> Importance {
    if rules.orphan_lines_always_important {
        return Importance::Important;
    }
    let body = line.get(..line_body_len(line)).unwrap_or_default();
    if rules.matched_unimportant(body) {
        return Importance::Unimportant;
    }
    let tokens = classifier.classify_line_at(side, line_index, line);
    if tokens.is_empty() {
        // A line the classifier claims nothing in still holds unclaimed text
        // unless it is empty.
        return if body.is_empty() || !rules.everything_else_important {
            Importance::Unimportant
        } else {
            Importance::Important
        };
    }
    if tokens.iter().any(|t| rules.category_important(&t.category)) {
        Importance::Important
    } else {
        Importance::Unimportant
    }
}

/// Reject a hunk whose ranges do not describe a position in the two inputs.
///
/// The fields are public, so a hunk can reach this crate without having been
/// produced by it. A reversed range would otherwise be turned into a line
/// count by wrapping subtraction.
fn validate_hunk(left_len: usize, right_len: usize, hunk: &Hunk) -> Result<(), DiffError> {
    if hunk.left.start > hunk.left.end || hunk.right.start > hunk.right.end {
        return Err(DiffError::malformed(
            "hunk",
            format!(
                "range runs backwards: left {:?} right {:?}",
                hunk.left, hunk.right
            ),
        ));
    }
    if hunk.left.end as usize > left_len || hunk.right.end as usize > right_len {
        return Err(DiffError::malformed(
            "hunk",
            format!(
                "range reaches past the input ({left_len} left lines, {right_len} right lines): left {:?} right {:?}",
                hunk.left, hunk.right
            ),
        ));
    }
    Ok(())
}

/// Classify every hunk of a comparison.
///
/// `left` and `right` are the original line sequences the hunks index into.
///
/// # Errors
///
/// Returns [`DiffError::Malformed`] when a hunk's range runs backwards or
/// reaches past the lines it indexes into.
pub fn classify_hunks(
    left: &[&str],
    right: &[&str],
    hunks: &[Hunk],
    rules: &RuleSet,
    classifier: &dyn LineClassifier,
) -> Result<Vec<ClassifiedHunk>, DiffError> {
    classify_hunks_cancellable(left, right, hunks, rules, classifier, &crate::NeverCancel)
}

/// Classify every hunk, checking for cancellation between hunks and lines.
///
/// # Errors
///
/// Returns [`DiffError::Cancelled`] when the flag is raised, or
/// [`DiffError::Malformed`] when a hunk's range is invalid.
pub fn classify_hunks_cancellable(
    left: &[&str],
    right: &[&str],
    hunks: &[Hunk],
    rules: &RuleSet,
    classifier: &dyn LineClassifier,
    cancel: &dyn crate::Cancel,
) -> Result<Vec<ClassifiedHunk>, DiffError> {
    classify_hunks_with(left, right, hunks, rules, classifier, cancel)
}

/// Classify every hunk of a comparison with a classifier that is told where
/// each line sits.
///
/// Identical to [`classify_hunks`] apart from the classifier it takes, so a
/// classifier needing the state carried in from earlier lines can be used
/// without changing anything else about a comparison.
///
/// # Errors
///
/// Returns [`DiffError::Malformed`] when a hunk's range runs backwards or
/// reaches past the lines it indexes into.
pub fn classify_hunks_indexed(
    left: &[&str],
    right: &[&str],
    hunks: &[Hunk],
    rules: &RuleSet,
    classifier: &dyn IndexedLineClassifier,
) -> Result<Vec<ClassifiedHunk>, DiffError> {
    classify_hunks_indexed_cancellable(left, right, hunks, rules, classifier, &crate::NeverCancel)
}

/// Classify every indexed hunk, checking for cancellation between hunks and
/// lines.
///
/// # Errors
///
/// Returns [`DiffError::Cancelled`] when the flag is raised, or
/// [`DiffError::Malformed`] when a hunk's range is invalid.
pub fn classify_hunks_indexed_cancellable(
    left: &[&str],
    right: &[&str],
    hunks: &[Hunk],
    rules: &RuleSet,
    classifier: &dyn IndexedLineClassifier,
    cancel: &dyn crate::Cancel,
) -> Result<Vec<ClassifiedHunk>, DiffError> {
    classify_hunks_with(left, right, hunks, rules, classifier, cancel)
}

fn classify_hunks_with<C: IndexedLineClassifier + ?Sized>(
    left: &[&str],
    right: &[&str],
    hunks: &[Hunk],
    rules: &RuleSet,
    classifier: &C,
    cancel: &dyn crate::Cancel,
) -> Result<Vec<ClassifiedHunk>, DiffError> {
    let mut output = Vec::with_capacity(hunks.len());
    for hunk in hunks {
        crate::cancel::check(cancel)?;
        output.push(classify_hunk_with(
            left, right, hunk, rules, classifier, cancel,
        )?);
    }
    Ok(output)
}

/// Classify a single hunk.
///
/// # Errors
///
/// Returns [`DiffError::Malformed`] when the hunk's range runs backwards or
/// reaches past the lines it indexes into.
pub fn classify_hunk(
    left: &[&str],
    right: &[&str],
    hunk: &Hunk,
    rules: &RuleSet,
    classifier: &dyn LineClassifier,
) -> Result<ClassifiedHunk, DiffError> {
    classify_hunk_with(left, right, hunk, rules, classifier, &crate::NeverCancel)
}

fn classify_hunk_with<C: IndexedLineClassifier + ?Sized>(
    left: &[&str],
    right: &[&str],
    hunk: &Hunk,
    rules: &RuleSet,
    classifier: &C,
    cancel: &dyn crate::Cancel,
) -> Result<ClassifiedHunk, DiffError> {
    crate::cancel::check(cancel)?;
    validate_hunk(left.len(), right.len(), hunk)?;
    let ln = hunk.left.end.saturating_sub(hunk.left.start) as usize;
    let rn = hunk.right.end.saturating_sub(hunk.right.start) as usize;
    let mut left_lines = vec![Importance::Unimportant; ln];
    let mut right_lines = vec![Importance::Unimportant; rn];
    if hunk.kind == HunkKind::Same {
        return Ok(ClassifiedHunk {
            hunk: hunk.clone(),
            importance: None,
            left_lines,
            right_lines,
        });
    }
    let paired = ln.min(rn);
    for i in 0..paired {
        crate::cancel::check(cancel)?;
        let li = hunk.left.start + u32::try_from(i).unwrap_or(0);
        let ri = hunk.right.start + u32::try_from(i).unwrap_or(0);
        let (l, r) = changed_pair(
            rules,
            classifier,
            line_text(left, li),
            line_text(right, ri),
            li as usize,
            ri as usize,
        );
        if let Some(slot) = left_lines.get_mut(i) {
            *slot = l;
        }
        if let Some(slot) = right_lines.get_mut(i) {
            *slot = r;
        }
    }
    for i in paired..ln {
        crate::cancel::check(cancel)?;
        let li = hunk.left.start + u32::try_from(i).unwrap_or(0);
        if let Some(slot) = left_lines.get_mut(i) {
            *slot = orphan_line(
                rules,
                classifier,
                line_text(left, li),
                ClassifierSide::Left,
                li as usize,
            );
        }
    }
    for i in paired..rn {
        crate::cancel::check(cancel)?;
        let ri = hunk.right.start + u32::try_from(i).unwrap_or(0);
        if let Some(slot) = right_lines.get_mut(i) {
            *slot = orphan_line(
                rules,
                classifier,
                line_text(right, ri),
                ClassifierSide::Right,
                ri as usize,
            );
        }
    }
    let important = left_lines
        .iter()
        .chain(right_lines.iter())
        .any(|i| *i == Importance::Important);
    Ok(ClassifiedHunk {
        hunk: hunk.clone(),
        importance: Some(if important {
            Importance::Important
        } else {
            Importance::Unimportant
        }),
        left_lines,
        right_lines,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::lines::{diff_line_slices, LineCompareOptions};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CancelAfter {
        threshold: usize,
        checks: AtomicUsize,
    }

    impl crate::Cancel for CancelAfter {
        fn is_cancelled(&self) -> bool {
            self.checks.fetch_add(1, Ordering::Relaxed) >= self.threshold
        }
    }

    fn classify(left: &[&str], right: &[&str], rules: &RuleSet) -> Vec<ClassifiedHunk> {
        let hunks = diff_line_slices(left, right, &LineCompareOptions::default());
        classify_hunks(left, right, &hunks, rules, &WhitespaceClassifier).unwrap()
    }

    fn worst(hunks: &[ClassifiedHunk]) -> Option<Importance> {
        hunks
            .iter()
            .filter_map(|h| h.importance)
            .max_by_key(|i| u8::from(*i == Importance::Important))
    }

    #[test]
    fn same_hunks_carry_no_importance() {
        let hunks = classify(&["a\n"], &["a\n"], &RuleSet::all_important());
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].importance, None);
    }

    #[test]
    fn text_change_is_important_by_default() {
        let hunks = classify(&["alpha\n"], &["beta\n"], &RuleSet::all_important());
        assert_eq!(worst(&hunks), Some(Importance::Important));
    }

    #[test]
    fn sorted_changed_spans_do_not_rescan_every_token() {
        const TOKEN_COUNT: usize = 2_048;
        let tokens: Vec<_> = (0..TOKEN_COUNT)
            .map(|index| {
                let start = u32::try_from(index * 2).unwrap();
                ClassifiedToken {
                    range: start..start + 2,
                    category: TokenCategory::EverythingElse,
                }
            })
            .collect();
        let mut rules = RuleSet::all_important();
        rules.everything_else_important = false;
        SPAN_TOKEN_VISITS.with(|visits| visits.set(0));
        let mut token_cursor = 0;
        for index in 0..TOKEN_COUNT {
            let start = u32::try_from(index * 2).unwrap();
            let _ = span_importance(
                &rules,
                &tokens,
                &mut token_cursor,
                Span {
                    start,
                    end: start + 1,
                },
            );
        }
        let visits = SPAN_TOKEN_VISITS.with(std::cell::Cell::get);
        assert!(
            visits <= TOKEN_COUNT * 3,
            "classification inspected {visits} tokens for {TOKEN_COUNT} ordered spans"
        );
    }

    #[test]
    fn cancellable_classification_checks_between_paired_lines() {
        let left: Vec<_> = (0..100).map(|index| format!("left {index}\n")).collect();
        let right: Vec<_> = (0..100).map(|index| format!("right {index}\n")).collect();
        let left_refs: Vec<_> = left.iter().map(String::as_str).collect();
        let right_refs: Vec<_> = right.iter().map(String::as_str).collect();
        let hunks = diff_line_slices(&left_refs, &right_refs, &LineCompareOptions::default());
        let cancel = CancelAfter {
            threshold: 3,
            checks: AtomicUsize::new(0),
        };
        let result = classify_hunks_cancellable(
            &left_refs,
            &right_refs,
            &hunks,
            &RuleSet::all_important(),
            &WhitespaceClassifier,
            &cancel,
        );
        assert_eq!(result.unwrap_err(), DiffError::Cancelled);
        assert!(cancel.checks.load(Ordering::Relaxed) >= 4);
    }

    #[test]
    fn trailing_whitespace_can_be_made_unimportant() {
        let mut rules = RuleSet::all_important();
        rules.trailing_whitespace_important = false;
        let hunks = classify(&["alpha   \n"], &["alpha\n"], &rules);
        assert_eq!(worst(&hunks), Some(Importance::Unimportant));
    }

    #[test]
    fn compared_line_ending_changes_are_important() {
        let left = ["alpha\r\n"];
        let right = ["alpha\n"];
        let options = LineCompareOptions {
            ignore_line_endings: false,
            ..LineCompareOptions::default()
        };
        let hunks = diff_line_slices(&left, &right, &options);
        let mut rules = RuleSet::all_important();
        rules.compare_line_endings = true;
        let outcome = classify_hunks(&left, &right, &hunks, &rules, &WhitespaceClassifier).unwrap();
        assert_eq!(worst(&outcome), Some(Importance::Important));
    }

    #[test]
    fn a_missing_final_terminator_is_no_difference_when_style_is_ignored() {
        let left = ["alpha"];
        let right = ["alpha\n"];
        let hunks = diff_line_slices(&left, &right, &LineCompareOptions::default());
        let outcome = classify_hunks(
            &left,
            &right,
            &hunks,
            &RuleSet::all_important(),
            &WhitespaceClassifier,
        )
        .unwrap();

        assert_eq!(worst(&outcome), None);
    }

    #[test]
    fn non_ascii_space_is_content_not_ignorable_whitespace() {
        let left = ["\u{00A0}\n"];
        let right = [" \n"];
        let hunks = diff_line_slices(&left, &right, &LineCompareOptions::default());
        let mut rules = RuleSet::all_important();
        rules.leading_whitespace_important = false;
        rules.trailing_whitespace_important = false;
        let outcome = classify_hunks(&left, &right, &hunks, &rules, &WhitespaceClassifier).unwrap();

        assert_eq!(worst(&outcome), Some(Importance::Important));
    }

    #[test]
    fn extra_carriage_return_before_a_terminator_is_line_content() {
        let left = ["alpha\r\r\n"];
        let right = ["alpha\r\n"];
        let rules = RuleSet::all_important();
        let outcome = classify(&left, &right, &rules);
        assert_eq!(worst(&outcome), Some(Importance::Important));
    }

    #[test]
    fn leading_whitespace_stays_important_when_asked() {
        let mut rules = RuleSet::all_important();
        rules.trailing_whitespace_important = false;
        let hunks = classify(&["    alpha\n"], &["alpha\n"], &rules);
        assert_eq!(worst(&hunks), Some(Importance::Important));
    }

    #[test]
    fn case_only_change_is_unimportant_when_case_is_not_matched() {
        let mut rules = RuleSet::all_important();
        rules.match_character_case = false;
        let hunks = classify(&["Alpha\n"], &["ALPHA\n"], &rules);
        assert_eq!(worst(&hunks), Some(Importance::Unimportant));
    }

    #[test]
    fn replacement_makes_a_pair_equivalent() {
        let mut rules = RuleSet::all_important();
        rules.replacements.push(
            ReplacementRule::new(
                "apple",
                "orange",
                ReplacementOptions {
                    match_character_case: true,
                    whole_words_only: true,
                    ..ReplacementOptions::default()
                },
            )
            .unwrap(),
        );
        let hunks = classify(&["an apple a day\n"], &["an orange a day\n"], &rules);
        assert_eq!(worst(&hunks), Some(Importance::Unimportant));
    }

    #[test]
    fn replacement_does_not_hide_an_unrelated_change() {
        let mut rules = RuleSet::all_important();
        rules
            .replacements
            .push(ReplacementRule::new("apple", "orange", ReplacementOptions::default()).unwrap());
        let hunks = classify(&["an apple a day\n"], &["an orange a week\n"], &rules);
        assert_eq!(worst(&hunks), Some(Importance::Important));
    }

    #[test]
    fn unimportance_pattern_suppresses_matching_text() {
        let mut rules = RuleSet::all_important();
        rules.orphan_lines_always_important = false;
        rules.push_unimportance_pattern(r"\d+").unwrap();
        let hunks = classify(&["id 123\n"], &["id 4567\n"], &rules);
        assert_eq!(worst(&hunks), Some(Importance::Unimportant));
    }

    #[test]
    fn orphan_line_rule_overrides_content() {
        let mut rules = RuleSet::all_important();
        rules.everything_else_important = false;
        rules.leading_whitespace_important = false;
        rules.embedded_whitespace_important = false;
        rules.trailing_whitespace_important = false;
        rules.orphan_lines_always_important = true;
        let hunks = classify(&["a\n", "b\n"], &["a\n", "\n", "b\n"], &rules);
        assert_eq!(worst(&hunks), Some(Importance::Important));
    }

    #[test]
    fn orphan_line_without_the_rule_follows_its_content() {
        let mut rules = RuleSet::all_important();
        rules.orphan_lines_always_important = false;
        rules.everything_else_important = false;
        let hunks = classify(&["a\n", "b\n"], &["a\n", "zzz\n", "b\n"], &rules);
        assert_eq!(worst(&hunks), Some(Importance::Unimportant));
    }

    #[test]
    fn per_line_importance_is_reported() {
        let rules = RuleSet::all_important();
        let hunks = classify(&["a\n", "x\n"], &["a\n", "y\n"], &rules);
        let changed = hunks.iter().find(|h| h.importance.is_some()).unwrap();
        assert_eq!(changed.left_lines, [Importance::Important]);
        assert_eq!(changed.right_lines, [Importance::Important]);
    }

    #[test]
    fn whitespace_classifier_places_runs() {
        let tokens = WhitespaceClassifier.classify_line("  a b  \n");
        let categories: Vec<_> = tokens.into_iter().map(|t| t.category).collect();
        assert_eq!(
            categories,
            [
                TokenCategory::LeadingWhitespace,
                TokenCategory::EverythingElse,
                TokenCategory::EmbeddedWhitespace,
                TokenCategory::EverythingElse,
                TokenCategory::TrailingWhitespace,
            ]
        );
    }

    #[test]
    fn grammar_element_importance_is_consulted() {
        struct AllComments;
        impl LineClassifier for AllComments {
            fn classify_line(&self, line: &str) -> Vec<ClassifiedToken> {
                let len = u32::try_from(line.trim_end_matches('\n').len()).unwrap_or(0);
                vec![ClassifiedToken {
                    range: 0..len,
                    category: TokenCategory::Element("Comment".to_owned()),
                }]
            }
        }
        let mut rules = RuleSet::all_important();
        rules.orphan_lines_always_important = false;
        rules.everything_else_important = false;
        let hunks = diff_line_slices(&["// a\n"], &["// b\n"], &LineCompareOptions::default());
        let unimportant =
            classify_hunks(&["// a\n"], &["// b\n"], &hunks, &rules, &AllComments).unwrap();
        assert_eq!(unimportant[0].importance, Some(Importance::Unimportant));

        rules.important_elements.insert("Comment".to_owned());
        let important =
            classify_hunks(&["// a\n"], &["// b\n"], &hunks, &rules, &AllComments).unwrap();
        assert_eq!(important[0].importance, Some(Importance::Important));
    }

    /// Claims everything from `from` onward, leaving the head of the line
    /// unclaimed.
    struct TailComment {
        from: u32,
    }

    impl LineClassifier for TailComment {
        fn classify_line(&self, line: &str) -> Vec<ClassifiedToken> {
            let len = u32::try_from(line.trim_end_matches(['\r', '\n']).len()).unwrap_or(0);
            if self.from >= len {
                return Vec::new();
            }
            vec![ClassifiedToken {
                range: self.from..len,
                category: TokenCategory::Element("Comment".to_owned()),
            }]
        }
    }

    #[test]
    fn text_before_the_first_claimed_run_is_still_counted() {
        let mut rules = RuleSet::all_important();
        rules.orphan_lines_always_important = false;
        rules.everything_else_important = true;
        let left = ["XYZcomment\n"];
        let right = ["ABCcomment\n"];
        let hunks = diff_line_slices(&left, &right, &LineCompareOptions::default());
        let classified =
            classify_hunks(&left, &right, &hunks, &rules, &TailComment { from: 3 }).unwrap();
        assert_eq!(classified[0].importance, Some(Importance::Important));
    }

    #[test]
    fn text_before_the_first_claimed_run_follows_the_unclaimed_rule() {
        let mut rules = RuleSet::all_important();
        rules.orphan_lines_always_important = false;
        rules.everything_else_important = false;
        let left = ["XYZcomment\n"];
        let right = ["ABCcomment\n"];
        let hunks = diff_line_slices(&left, &right, &LineCompareOptions::default());
        let classified =
            classify_hunks(&left, &right, &hunks, &rules, &TailComment { from: 3 }).unwrap();
        assert_eq!(classified[0].importance, Some(Importance::Unimportant));
    }

    #[test]
    fn unimportance_patterns_do_not_depend_on_alternation_order() {
        for pattern in [r"a|abc", r"abc|a"] {
            let mut rules = RuleSet::all_important();
            rules.orphan_lines_always_important = false;
            rules.push_unimportance_pattern(pattern).unwrap();
            let hunks = classify(&["k\n"], &["k\n", "abc\n"], &rules);
            assert_eq!(
                worst(&hunks),
                Some(Importance::Unimportant),
                "pattern {pattern} covers the whole text either way"
            );
            let compiled = rules.unimportance_patterns().first().unwrap();
            assert!(compiled.is_match("abc"));
            assert!(compiled.is_match("a"));
            assert!(!compiled.is_match("ab"));
            assert!(!compiled.is_match("abcd"));
        }
    }

    #[test]
    fn a_replacement_composes_with_the_other_rules() {
        let mut rules = RuleSet::all_important();
        rules.trailing_whitespace_important = false;
        rules
            .replacements
            .push(ReplacementRule::new("apple", "orange", ReplacementOptions::default()).unwrap());
        let hunks = classify(&["apple x  \n"], &["orange x\n"], &rules);
        assert_eq!(worst(&hunks), Some(Importance::Unimportant));
    }

    #[test]
    #[allow(clippy::reversed_empty_ranges)]
    fn a_reversed_hunk_is_rejected() {
        let left = ["a\n"];
        let right = ["b\n"];
        let rules = RuleSet::all_important();
        let reversed = Hunk {
            kind: HunkKind::Changed,
            left: 1..0,
            right: 0..1,
        };
        assert!(classify_hunk(&left, &right, &reversed, &rules, &WhitespaceClassifier).is_err());
        let past_end = Hunk {
            kind: HunkKind::Changed,
            left: 0..9,
            right: 0..1,
        };
        assert!(classify_hunk(&left, &right, &past_end, &rules, &WhitespaceClassifier).is_err());
        let same_past_end = Hunk {
            kind: HunkKind::Same,
            left: 0..4_000_000_000,
            right: 0..1,
        };
        assert!(
            classify_hunk(&left, &right, &same_past_end, &rules, &WhitespaceClassifier).is_err()
        );
    }

    /// Records which side and line number each classification was asked about,
    /// and calls the named lines' text an element.
    #[derive(Debug, Default)]
    struct Recording {
        seen: std::cell::RefCell<Vec<(ClassifierSide, usize)>>,
        element_at: Vec<(ClassifierSide, usize)>,
    }

    impl IndexedLineClassifier for Recording {
        fn classify_line_at(
            &self,
            side: ClassifierSide,
            line_index: usize,
            line: &str,
        ) -> Vec<ClassifiedToken> {
            self.seen.borrow_mut().push((side, line_index));
            let end = u32::try_from(line.trim_end_matches(['\r', '\n']).len()).unwrap_or(u32::MAX);
            let category = if self.element_at.contains(&(side, line_index)) {
                TokenCategory::Element("Comment".to_owned())
            } else {
                TokenCategory::EverythingElse
            };
            vec![ClassifiedToken {
                range: 0..end,
                category,
            }]
        }
    }

    #[test]
    fn an_indexed_classifier_is_told_the_side_and_line_of_every_line_it_sees() {
        let left = ["a\n", "b\n"];
        let right = ["a\n", "c\n"];
        let hunks = diff_line_slices(&left, &right, &LineCompareOptions::default());
        let classifier = Recording::default();
        classify_hunks_indexed(
            &left,
            &right,
            &hunks,
            &RuleSet::all_important(),
            &classifier,
        )
        .unwrap();
        let seen = classifier.seen.borrow().clone();
        assert!(seen.contains(&(ClassifierSide::Left, 1)));
        assert!(seen.contains(&(ClassifierSide::Right, 1)));
        assert!(!seen.contains(&(ClassifierSide::Left, 0)));
    }

    #[test]
    fn a_line_index_can_decide_a_categorys_importance() {
        let left = ["a\n", "b\n"];
        let right = ["a\n", "c\n"];
        let hunks = diff_line_slices(&left, &right, &LineCompareOptions::default());
        let mut rules = RuleSet::all_important();
        rules.orphan_lines_always_important = false;
        rules.everything_else_important = true;
        let classifier = Recording {
            element_at: vec![(ClassifierSide::Left, 1), (ClassifierSide::Right, 1)],
            ..Recording::default()
        };
        let outcome = classify_hunks_indexed(&left, &right, &hunks, &rules, &classifier).unwrap();
        // The element is not on the important list, so naming it on line one
        // is what makes the change unimportant.
        assert_eq!(
            outcome
                .iter()
                .filter_map(|h| h.importance)
                .collect::<Vec<_>>(),
            vec![Importance::Unimportant]
        );
    }

    #[test]
    fn a_plain_classifier_reaches_the_indexed_entry_point_unchanged() {
        let left = ["alpha\n"];
        let right = ["beta\n"];
        let hunks = diff_line_slices(&left, &right, &LineCompareOptions::default());
        let rules = RuleSet::all_important();
        let plain = classify_hunks(&left, &right, &hunks, &rules, &WhitespaceClassifier).unwrap();
        let indexed =
            classify_hunks_indexed(&left, &right, &hunks, &rules, &WhitespaceClassifier).unwrap();
        assert_eq!(plain, indexed);
    }
}
