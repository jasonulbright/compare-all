//! Adapter that lets grammar elements drive the important/unimportant split.
//!
//! The diff engine knows nothing about syntax. It asks a classifier which
//! category each run of characters belongs to and then consults its own rule
//! set, whose element checklist is keyed by the names this crate hands back.
//! The names therefore have to be the grammar's element names verbatim.
//!
//! Text no item claims is split further before it reaches the engine: a run of
//! whitespace becomes leading, embedded or trailing whitespace according to
//! where it sits on the line, and any other unclaimed run becomes the
//! catch-all category.

use crate::lexer::{LexScratch, LexToken, Lexer, LineState, StateCache};
use crate::{Grammar, GrammarError};
use ca_diff::importance::{
    ClassifiedToken, ClassifierSide, IndexedLineClassifier, LineClassifier, TokenCategory,
};

/// A [`LineClassifier`] backed by a compiled grammar.
///
/// The classifier is stateless across lines, so it classifies a line in
/// isolation. A view that needs multi-line elements colored correctly drives
/// the [`Lexer`] itself with a [`crate::StateCache`]; the importance rules the
/// engine applies are per line, which is what this adapter serves.
#[derive(Debug, Clone)]
pub struct GrammarClassifier {
    lexer: Lexer,
    element_names: Vec<String>,
}

impl GrammarClassifier {
    /// Compile `grammar` for classification.
    ///
    /// # Errors
    ///
    /// Returns [`GrammarError`] when the grammar does not compile.
    pub fn new(grammar: &Grammar) -> Result<Self, GrammarError> {
        let lexer = Lexer::new(grammar)?;
        let element_names = lexer.element_names();
        Ok(Self {
            lexer,
            element_names,
        })
    }

    /// The tokenizer underneath.
    pub fn lexer(&self) -> &Lexer {
        &self.lexer
    }

    /// The element names to present as the importance checklist, in the order
    /// the grammar defines them.
    pub fn element_names(&self) -> &[String] {
        &self.element_names
    }

    /// The unique grammar elements with case-sensitive text, in grammar order.
    pub fn case_sensitive_elements(&self) -> Vec<String> {
        let mut names = Vec::new();
        for item in &self.lexer.grammar().items {
            if item.case_sensitive && !names.contains(&item.element) {
                names.push(item.element.clone());
            }
        }
        names
    }

    /// Classify one line, starting from `state`, and report the state to carry
    /// into the next line.
    ///
    /// # Errors
    ///
    /// Returns [`GrammarError`] when a pattern fails at match time.
    pub fn classify_line_from(
        &self,
        line: &str,
        line_index: usize,
        state: LineState,
    ) -> Result<(Vec<ClassifiedToken>, LineState), GrammarError> {
        let mut scratch = LexScratch::new();
        let mut tokens = Vec::new();
        let out = self
            .lexer
            .lex_line_into(&mut scratch, line, line_index, state, &mut tokens)?;
        Ok((self.to_categories(line, &tokens), out))
    }

    /// Turn lexer tokens into engine categories, splitting unclaimed runs.
    ///
    /// A line terminator is not part of the line's body and is left
    /// unclassified, which is the same treatment the engine's own whitespace
    /// classifier gives it. Reporting it would make a carriage return look like
    /// a trailing whitespace difference under one classifier and nothing at all
    /// under the other.
    fn to_categories(&self, line: &str, tokens: &[LexToken]) -> Vec<ClassifiedToken> {
        let body = line_body_len(line);
        let first_text = line[..body].find(|c: char| !is_rule_whitespace(c));
        let last_text = line[..body].rfind(|c: char| !is_rule_whitespace(c));
        let mut out = Vec::with_capacity(tokens.len());
        for token in tokens {
            let start = token.range.start.min(body);
            let end = token.range.end.min(body);
            match token.item {
                Some(index) => {
                    let name = self.lexer.element_of(index).unwrap_or_default();
                    push(
                        &mut out,
                        start,
                        end,
                        TokenCategory::Element(name.to_owned()),
                    );
                }
                None => {
                    split_unclaimed(&mut out, line, start, end, first_text, last_text);
                }
            }
        }
        out
    }
}

/// A classifier that keeps the state carried between lines, so a line inside a
/// multi-line element is classified as part of that element.
///
/// The two sides each get their own cache, because a line number means nothing
/// without knowing which text it indexes into. A line index past the end of the
/// side it names falls back to the state a file starts in, which is what a
/// stale index from a caller would otherwise silently misreport.
#[derive(Debug, Clone)]
pub struct IndexedGrammarClassifier {
    inner: GrammarClassifier,
    left: StateCache,
    right: StateCache,
}

impl IndexedGrammarClassifier {
    /// Compile `grammar` and lex both sides once to build the carried states.
    ///
    /// # Errors
    ///
    /// Returns [`GrammarError`] when the grammar does not compile or a pattern
    /// fails while the caches are built.
    pub fn new<S: AsRef<str>>(
        grammar: &Grammar,
        left: &[S],
        right: &[S],
    ) -> Result<Self, GrammarError> {
        let inner = GrammarClassifier::new(grammar)?;
        let left = StateCache::build(inner.lexer(), left)?;
        let right = StateCache::build(inner.lexer(), right)?;
        Ok(Self { inner, left, right })
    }

    /// The element names to present as the importance checklist.
    pub fn element_names(&self) -> &[String] {
        self.inner.element_names()
    }

    /// The unique grammar elements with case-sensitive text, in grammar order.
    pub fn case_sensitive_elements(&self) -> Vec<String> {
        self.inner.case_sensitive_elements()
    }

    /// The per-line classifier underneath.
    pub fn classifier(&self) -> &GrammarClassifier {
        &self.inner
    }
}

impl IndexedLineClassifier for IndexedGrammarClassifier {
    fn classify_line_at(
        &self,
        side: ClassifierSide,
        line_index: usize,
        line: &str,
    ) -> Vec<ClassifiedToken> {
        let cache = match side {
            ClassifierSide::Left => &self.left,
            ClassifierSide::Right => &self.right,
        };
        let state = cache.state_at(line_index);
        // A failing pattern cannot be reported through this trait, so the line
        // falls back to a complete unclaimed tiling rather than a partial one.
        if let Ok((tokens, _)) = self.inner.classify_line_from(line, line_index, state) {
            return tokens;
        }
        self.inner.classify_line(line)
    }
}

impl LineClassifier for GrammarClassifier {
    fn classify_line(&self, line: &str) -> Vec<ClassifiedToken> {
        // A failing pattern cannot be reported through this trait. Falling back
        // to one unclaimed line keeps every byte accounted for, so the engine
        // still sees a complete tiling rather than a partial one.
        if let Ok((tokens, _)) = self.classify_line_from(line, 0, LineState::start()) {
            return tokens;
        }
        let mut out = Vec::new();
        let body = line_body_len(line);
        let first_text = line[..body].find(|c: char| !is_rule_whitespace(c));
        let last_text = line[..body].rfind(|c: char| !is_rule_whitespace(c));
        split_unclaimed(&mut out, line, 0, body, first_text, last_text);
        out
    }
}

/// Split an unclaimed run into whitespace runs and everything-else runs.
fn split_unclaimed(
    out: &mut Vec<ClassifiedToken>,
    line: &str,
    start: usize,
    end: usize,
    first_text: Option<usize>,
    last_text: Option<usize>,
) {
    let mut run_start = start;
    let mut run_space: Option<bool> = None;
    let mut pos = start;
    while pos < end {
        let Some(c) = line[pos..].chars().next() else {
            break;
        };
        let is_space = is_rule_whitespace(c);
        if run_space != Some(is_space) {
            if let Some(space) = run_space {
                push(
                    out,
                    run_start,
                    pos,
                    whitespace_category(space, run_start, first_text, last_text),
                );
            }
            run_start = pos;
            run_space = Some(is_space);
        }
        pos += c.len_utf8();
    }
    if let Some(space) = run_space {
        push(
            out,
            run_start,
            end,
            whitespace_category(space, run_start, first_text, last_text),
        );
    }
}

fn whitespace_category(
    is_space: bool,
    start: usize,
    first_text: Option<usize>,
    last_text: Option<usize>,
) -> TokenCategory {
    if !is_space {
        return TokenCategory::EverythingElse;
    }
    match (first_text, last_text) {
        // A line with no text at all is trailing whitespace: there is nothing
        // it could be leading or embedded relative to.
        (None, _) | (_, None) => TokenCategory::TrailingWhitespace,
        (Some(first), Some(last)) => {
            if start < first {
                TokenCategory::LeadingWhitespace
            } else if start > last {
                TokenCategory::TrailingWhitespace
            } else {
                TokenCategory::EmbeddedWhitespace
            }
        }
    }
}

fn push(out: &mut Vec<ClassifiedToken>, start: usize, end: usize, category: TokenCategory) {
    if end <= start {
        return;
    }
    let start = u32::try_from(start).unwrap_or(u32::MAX);
    let end = u32::try_from(end).unwrap_or(u32::MAX);
    if end <= start {
        return;
    }
    out.push(ClassifiedToken {
        range: start..end,
        category,
    });
}

fn is_rule_whitespace(character: char) -> bool {
    matches!(character, ' ' | '\t')
}

fn line_body_len(line: &str) -> usize {
    if line.ends_with("\r\n") {
        line.len().saturating_sub(2)
    } else if line.ends_with(['\r', '\n']) {
        line.len().saturating_sub(1)
    } else {
        line.len()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{GrammarClassifier, LineClassifier};
    use crate::Grammar;
    use ca_diff::importance::{TokenCategory, WhitespaceClassifier};

    #[test]
    fn blank_lines_have_the_same_whitespace_category_with_or_without_a_grammar() {
        let plain = WhitespaceClassifier.classify_line(" \t\n");
        let grammar = GrammarClassifier::new(&Grammar::default())
            .unwrap()
            .classify_line(" \t\n");

        assert_eq!(plain.len(), 1);
        assert_eq!(grammar.len(), 1);
        assert_eq!(plain[0].category, TokenCategory::TrailingWhitespace);
        assert_eq!(grammar[0].category, TokenCategory::TrailingWhitespace);
    }

    #[test]
    fn non_ascii_space_is_not_a_whitespace_rule_category() {
        let tokens = WhitespaceClassifier.classify_line("\u{00A0}\n");

        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].category, TokenCategory::EverythingElse);
    }
}
