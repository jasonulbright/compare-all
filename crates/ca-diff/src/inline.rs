//! Character and token level differences inside one pair of changed lines.
//!
//! Spans are byte offsets into the line they belong to, always on character
//! boundaries. A pure insertion on one side is reported on the other side as a
//! zero width span marking where the text would go.

use crate::cancel::{check, Cancel, NeverCancel};
use crate::DiffError;
use imara_diff::intern::InternedInput;
use imara_diff::{diff, Algorithm, Sink};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

/// A byte range within a single line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    /// First byte of the span.
    pub start: u32,
    /// One past the last byte of the span.
    pub end: u32,
}

impl Span {
    /// Whether the span covers no bytes.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.start >= self.end
    }

    /// Length of the span in bytes.
    #[must_use]
    pub const fn len(&self) -> u32 {
        self.end.saturating_sub(self.start)
    }
}

/// Differing spans of one line pair.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InlineDiff {
    /// Spans of the left line that are not present on the right.
    pub left: Vec<Span>,
    /// Spans of the right line that are not present on the left.
    pub right: Vec<Span>,
}

impl InlineDiff {
    /// Whether the two lines are identical under the options used.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.left.iter().all(Span::is_empty) && self.right.iter().all(Span::is_empty)
    }
}

/// Comparison options for inline highlighting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InlineOptions {
    /// Compare letters without regard to capitalization.
    pub ignore_case: bool,
}

/// Split a line into word, whitespace, and punctuation runs.
///
/// A word run is a maximal sequence of alphanumeric characters and
/// underscores; a whitespace run is a maximal sequence of whitespace; every
/// other character is its own token.
#[must_use]
pub fn tokenize(line: &str) -> Vec<Span> {
    let mut out = Vec::new();
    let mut iter = line.char_indices().peekable();
    while let Some((start, c)) = iter.next() {
        let word = c.is_alphanumeric() || c == '_';
        let space = c.is_whitespace();
        let mut end = start + c.len_utf8();
        if word || space {
            while let Some(&(idx, next)) = iter.peek() {
                let same = if word {
                    next.is_alphanumeric() || next == '_'
                } else {
                    next.is_whitespace()
                };
                if !same {
                    break;
                }
                end = idx + next.len_utf8();
                iter.next();
            }
        }
        out.push(Span {
            start: u32::try_from(start).unwrap_or(u32::MAX),
            end: u32::try_from(end).unwrap_or(u32::MAX),
        });
    }
    out
}

/// Character level units, one per extended grapheme cluster.
///
/// A base character and its combining marks form one unit, so a reported span
/// never starts or ends in the middle of a cluster and never leaves a
/// combining mark stranded without the character it modifies.
fn char_spans(line: &str) -> Vec<Span> {
    line.grapheme_indices(true)
        .map(|(idx, g)| Span {
            start: u32::try_from(idx).unwrap_or(u32::MAX),
            end: u32::try_from(idx + g.len()).unwrap_or(u32::MAX),
        })
        .collect()
}

/// Comparison keys for each unit, borrowing the line wherever the options do
/// not change the text. A line of several megabytes is one unit per character,
/// so owning every key costs an allocation per character.
fn unit_keys<'a>(line: &'a str, spans: &[Span], options: InlineOptions) -> Vec<Cow<'a, str>> {
    spans
        .iter()
        .map(|s| {
            let text = line
                .get(s.start as usize..s.end as usize)
                .unwrap_or_default();
            if options.ignore_case {
                let folded = text.to_lowercase();
                if folded == text {
                    Cow::Borrowed(text)
                } else {
                    Cow::Owned(folded)
                }
            } else {
                Cow::Borrowed(text)
            }
        })
        .collect()
}

struct SpanCollector {
    left: Vec<Range<u32>>,
    right: Vec<Range<u32>>,
}

impl Sink for SpanCollector {
    type Out = (Vec<Range<u32>>, Vec<Range<u32>>);

    fn process_change(&mut self, before: Range<u32>, after: Range<u32>) {
        self.left.push(before);
        self.right.push(after);
    }

    fn finish(self) -> Self::Out {
        (self.left, self.right)
    }
}

/// Map a unit index range onto a byte span. An empty index range becomes a
/// zero width span at the byte offset where the units would start.
fn to_span(spans: &[Span], range: &Range<u32>, line_len: u32) -> Span {
    if range.is_empty() {
        let at = spans
            .get(range.start as usize)
            .map_or(line_len, |s| s.start);
        return Span { start: at, end: at };
    }
    let start = spans
        .get(range.start as usize)
        .map_or(line_len, |s| s.start);
    let end = spans
        .get(range.end as usize - 1)
        .map_or(line_len, |s| s.end);
    Span { start, end }
}

/// Product of the two sides' differing unit counts above which the pass stops
/// subdividing and reports the whole differing middle as one span per side.
///
/// The alignment underneath is quadratic in that product, so an uncapped pass
/// over two long dissimilar lines costs more than the rest of a comparison put
/// together. Above the budget the spans are coarser; they still cover exactly
/// the bytes that differ, because the shared prefix and suffix are removed
/// before the budget is tested.
const INLINE_AREA_BUDGET: u64 = 4_000_000;

/// Number of leading units the two key sequences share.
fn common_prefix_units(lk: &[Cow<'_, str>], rk: &[Cow<'_, str>]) -> usize {
    let limit = lk.len().min(rk.len());
    let mut n = 0;
    while n < limit && lk.get(n) == rk.get(n) {
        n += 1;
    }
    n
}

/// Number of trailing units the two key sequences share, ignoring the first
/// `pre` units which are already accounted for.
fn common_suffix_units(lk: &[Cow<'_, str>], rk: &[Cow<'_, str>], pre: usize) -> usize {
    let limit = lk.len().min(rk.len()).saturating_sub(pre);
    let mut n = 0;
    while n < limit && lk.get(lk.len() - 1 - n) == rk.get(rk.len() - 1 - n) {
        n += 1;
    }
    n
}

fn diff_units(
    left: &str,
    right: &str,
    left_spans: &[Span],
    right_spans: &[Span],
    options: InlineOptions,
    cancel: &dyn Cancel,
) -> Result<InlineDiff, DiffError> {
    check(cancel)?;
    let lk = unit_keys(left, left_spans, options);
    let rk = unit_keys(right, right_spans, options);
    check(cancel)?;
    let ll = u32::try_from(left.len()).unwrap_or(u32::MAX);
    let rl = u32::try_from(right.len()).unwrap_or(u32::MAX);

    let pre = common_prefix_units(&lk, &rk);
    check(cancel)?;
    let suf = common_suffix_units(&lk, &rk, pre);
    let lmid = pre..lk.len() - suf;
    let rmid = pre..rk.len() - suf;
    if lmid.is_empty() && rmid.is_empty() {
        return Ok(InlineDiff::default());
    }
    let offset = u32::try_from(pre).unwrap_or(u32::MAX);
    let area = (lmid.len() as u64).saturating_mul(rmid.len() as u64);
    if area > INLINE_AREA_BUDGET {
        let lr = offset..u32::try_from(lmid.end).unwrap_or(u32::MAX);
        let rr = offset..u32::try_from(rmid.end).unwrap_or(u32::MAX);
        return Ok(InlineDiff {
            left: vec![to_span(left_spans, &lr, ll)],
            right: vec![to_span(right_spans, &rr, rl)],
        });
    }

    let mut input: InternedInput<&str> = InternedInput::default();
    input.update_before(
        lk.get(lmid.clone())
            .unwrap_or_default()
            .iter()
            .map(Cow::as_ref),
    );
    input.update_after(
        rk.get(rmid.clone())
            .unwrap_or_default()
            .iter()
            .map(Cow::as_ref),
    );
    check(cancel)?;
    // Histogram's rare-token heuristic degrades badly on alphabets this small.
    let (lr, rr) = diff(
        Algorithm::Myers,
        &input,
        SpanCollector {
            left: Vec::new(),
            right: Vec::new(),
        },
    );
    check(cancel)?;
    let shift = |r: &Range<u32>| -> Range<u32> {
        r.start.saturating_add(offset)..r.end.saturating_add(offset)
    };
    Ok(InlineDiff {
        left: lr
            .iter()
            .map(|r| to_span(left_spans, &shift(r), ll))
            .collect(),
        right: rr
            .iter()
            .map(|r| to_span(right_spans, &shift(r), rl))
            .collect(),
    })
}

/// Character level differences between two lines, with default options.
#[must_use]
pub fn diff_chars(left: &str, right: &str) -> InlineDiff {
    diff_chars_with(left, right, InlineOptions::default())
}

/// Character level differences between two lines.
#[must_use]
pub fn diff_chars_with(left: &str, right: &str, options: InlineOptions) -> InlineDiff {
    diff_chars_cancellable(left, right, options, &NeverCancel).unwrap_or_default()
}

/// Character level differences between two lines, abandoning the work when
/// `cancel` is raised.
///
/// # Errors
///
/// Returns [`DiffError::Cancelled`] when the flag is raised before the
/// comparison finishes.
pub fn diff_chars_cancellable(
    left: &str,
    right: &str,
    options: InlineOptions,
    cancel: &dyn Cancel,
) -> Result<InlineDiff, DiffError> {
    let ls = char_spans(left);
    let rs = char_spans(right);
    diff_units(left, right, &ls, &rs, options, cancel)
}

/// Token level differences between two lines, with default options.
#[must_use]
pub fn diff_tokens(left: &str, right: &str) -> InlineDiff {
    diff_tokens_with(left, right, InlineOptions::default())
}

/// Token level differences between two lines, using [`tokenize`].
#[must_use]
pub fn diff_tokens_with(left: &str, right: &str, options: InlineOptions) -> InlineDiff {
    diff_tokens_cancellable(left, right, options, &NeverCancel).unwrap_or_default()
}

/// Token level differences between two lines, abandoning the work when
/// `cancel` is raised.
///
/// # Errors
///
/// Returns [`DiffError::Cancelled`] when the flag is raised before the
/// comparison finishes.
pub fn diff_tokens_cancellable(
    left: &str,
    right: &str,
    options: InlineOptions,
    cancel: &dyn Cancel,
) -> Result<InlineDiff, DiffError> {
    let ls = tokenize(left);
    let rs = tokenize(right);
    diff_units(left, right, &ls, &rs, options, cancel)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn text(line: &str, spans: &[Span]) -> Vec<String> {
        spans
            .iter()
            .map(|s| {
                line.get(s.start as usize..s.end as usize)
                    .unwrap_or("")
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn identical_lines_have_no_spans() {
        assert!(diff_chars("abc", "abc").is_empty());
        assert!(diff_tokens("let x = 1;", "let x = 1;").is_empty());
    }

    #[test]
    fn character_diff_isolates_the_changed_run() {
        let d = diff_chars("abcdef", "abXdef");
        assert_eq!(text("abcdef", &d.left), ["c"]);
        assert_eq!(text("abXdef", &d.right), ["X"]);
    }

    #[test]
    fn token_diff_reports_whole_words() {
        let d = diff_tokens("let alpha = 1;", "let beta = 1;");
        assert_eq!(text("let alpha = 1;", &d.left), ["alpha"]);
        assert_eq!(text("let beta = 1;", &d.right), ["beta"]);
    }

    #[test]
    fn insertion_yields_a_zero_width_span_on_the_other_side() {
        let d = diff_chars("ac", "abc");
        assert_eq!(d.left.len(), 1);
        assert!(d.left[0].is_empty());
        assert_eq!(d.left[0].start, 1);
        assert_eq!(text("abc", &d.right), ["b"]);
    }

    #[test]
    fn ignore_case_suppresses_case_only_spans() {
        let d = diff_tokens_with("Alpha", "alpha", InlineOptions { ignore_case: true });
        assert!(d.is_empty());
    }

    #[test]
    fn tokenizer_groups_words_and_whitespace() {
        let spans = tokenize("ab  c_d+e");
        assert_eq!(text("ab  c_d+e", &spans), ["ab", "  ", "c_d", "+", "e"]);
    }

    #[test]
    fn spans_stay_on_character_boundaries() {
        let d = diff_chars("héllo", "hello");
        for s in d.left.iter().chain(d.right.iter()) {
            assert!("héllo".is_char_boundary(s.start as usize) || s.start as usize > 5);
        }
        assert_eq!(text("héllo", &d.left), ["é"]);
    }

    #[test]
    fn empty_line_against_text() {
        let d = diff_chars("", "abc");
        assert_eq!(text("abc", &d.right), ["abc"]);
        assert_eq!(d.left.len(), 1);
        assert!(d.left[0].is_empty());
    }

    #[test]
    fn spans_never_split_a_grapheme_cluster() {
        let left = "e\u{301}";
        let d = diff_chars(left, "e");
        assert_eq!(text(left, &d.left), [left]);
        assert_eq!(text("e", &d.right), ["e"]);
        let combined = "a\u{301}bc";
        let d = diff_chars(combined, "a\u{301}bX");
        for s in &d.left {
            assert!(
                combined.is_char_boundary(s.start as usize),
                "{s:?} splits a cluster"
            );
            assert_ne!(s.start, 1, "a combining mark must not stand alone");
        }
    }

    #[test]
    #[cfg_attr(
        debug_assertions,
        ignore = "wall-clock budget holds for release builds"
    )]
    fn a_long_dissimilar_pair_is_reported_without_a_quadratic_pass() {
        let left: String = "a".repeat(200_000);
        let right: String = "b".repeat(200_000);
        let start = std::time::Instant::now();
        let d = diff_chars(&left, &right);
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "took {elapsed:?}"
        );
        assert_eq!(d.left.len(), 1);
        assert_eq!(d.right.len(), 1);
        assert_eq!(
            d.left[0],
            Span {
                start: 0,
                end: 200_000
            }
        );
        assert_eq!(
            d.right[0],
            Span {
                start: 0,
                end: 200_000
            }
        );
    }

    #[test]
    #[cfg_attr(
        debug_assertions,
        ignore = "wall-clock budget holds for release builds"
    )]
    fn a_shared_prefix_and_suffix_are_trimmed_before_the_budget_applies() {
        let filler: String = "z".repeat(300_000);
        let left = format!("{filler}ABC{filler}");
        let right = format!("{filler}XYZ{filler}");
        let start = std::time::Instant::now();
        let d = diff_chars(&left, &right);
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "took {elapsed:?}"
        );
        assert_eq!(text(&left, &d.left), ["ABC"]);
        assert_eq!(text(&right, &d.right), ["XYZ"]);
    }

    /// Correctness half of the long dissimilar pair, at a size a debug build
    /// handles: the fallback is one span over the whole of each side.
    #[test]
    fn a_dissimilar_pair_falls_back_to_one_span_a_side() {
        let left: String = "a".repeat(4_000);
        let right: String = "b".repeat(4_000);
        let d = diff_chars(&left, &right);
        assert_eq!(d.left.len(), 1);
        assert_eq!(d.right.len(), 1);
        assert_eq!(
            d.left[0],
            Span {
                start: 0,
                end: 4_000
            }
        );
        assert_eq!(
            d.right[0],
            Span {
                start: 0,
                end: 4_000
            }
        );
    }

    /// Correctness half of the trimming case: a shared prefix and suffix leave
    /// only the middle reported.
    #[test]
    fn a_shared_prefix_and_suffix_leave_only_the_middle() {
        let filler: String = "z".repeat(4_000);
        let left = format!("{filler}ABC{filler}");
        let right = format!("{filler}XYZ{filler}");
        let d = diff_chars(&left, &right);
        assert_eq!(text(&left, &d.left), ["ABC"]);
        assert_eq!(text(&right, &d.right), ["XYZ"]);
    }

    #[test]
    fn a_raised_flag_stops_the_inline_pass() {
        use std::sync::atomic::AtomicBool;
        let flag = AtomicBool::new(true);
        assert_eq!(
            diff_chars_cancellable("abc", "abd", InlineOptions::default(), &flag),
            Err(DiffError::Cancelled)
        );
    }

    #[test]
    fn span_len_and_empty_agree() {
        let s = Span { start: 2, end: 5 };
        assert_eq!(s.len(), 3);
        assert!(!s.is_empty());
        assert!(Span { start: 4, end: 4 }.is_empty());
    }
}
