//! Line-oriented alignment of two texts.
//!
//! Comparison options never change which text a hunk points at: they are
//! applied by deriving a normalization key per line, running the alignment on
//! the keys, and reporting hunks as ranges over the original lines.

use crate::cancel::{check, Cancel, NeverCancel};
use crate::DiffError;
use imara_diff::intern::InternedInput;
use imara_diff::{diff, Algorithm, Sink};
use serde::{Deserialize, Serialize};
use std::ops::Range;

/// Classification of an aligned hunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HunkKind {
    /// Lines identical on both sides.
    Same,
    /// Lines present only on the left.
    LeftOnly,
    /// Lines present only on the right.
    RightOnly,
    /// Lines present on both sides but different.
    Changed,
}

/// One aligned region of the two inputs, expressed as line ranges.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hunk {
    /// Region classification.
    pub kind: HunkKind,
    /// Line range on the left side (zero-based, end exclusive).
    pub left: Range<u32>,
    /// Line range on the right side (zero-based, end exclusive).
    pub right: Range<u32>,
}

/// Algorithm used to pair lines between the two sides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum AlignmentMode {
    /// Histogram alignment: the default, fast and stable on source text.
    #[default]
    Standard,
    /// Patience-style alignment on rare anchor lines. It shares the histogram
    /// engine, and therefore the size limit above which Myers runs instead.
    Patience,
    /// Longest-common-subsequence alignment with the usual cut-off heuristics.
    Myers,
    /// Longest-common-subsequence alignment with the heuristics disabled.
    MyersMinimal,
    /// No content alignment at all: line N on the left pairs with line N on the
    /// right and the longer side's tail is reported as an orphan run.
    Unaligned,
}

/// Controls over how lines are paired once their keys are known.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlignmentOptions {
    /// Algorithm used for the main pass.
    pub mode: AlignmentMode,
    /// Largest line displacement a match may carry. A matched run whose left
    /// and right start indices differ by more than this is rejected and folded
    /// into the surrounding difference, which is what bounding the search
    /// distance would have produced. `None` leaves the match distance
    /// unbounded.
    pub skew_tolerance: Option<u32>,
    /// Report changed regions as separate deleted and inserted blocks instead
    /// of paired changed lines.
    pub never_align_differences: bool,
    /// Pair lines still unmatched after the main pass by similarity.
    pub use_closeness_matching: bool,
}

impl Default for AlignmentOptions {
    fn default() -> Self {
        Self {
            mode: AlignmentMode::Standard,
            skew_tolerance: None,
            never_align_differences: false,
            use_closeness_matching: true,
        }
    }
}

/// Text differences that comparison should disregard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct LineCompareOptions {
    /// Compare letters without regard to capitalization.
    pub ignore_case: bool,
    /// Disregard whitespace before the first non-whitespace character.
    pub ignore_leading_whitespace: bool,
    /// Disregard how much whitespace separates non-whitespace runs. Runs are
    /// still required to be separated.
    pub ignore_embedded_whitespace: bool,
    /// Disregard whitespace after the last non-whitespace character.
    pub ignore_trailing_whitespace: bool,
    /// Disregard whitespace anywhere, including the separation between runs.
    /// Overrides the three positional whitespace options.
    pub ignore_all_whitespace: bool,
    /// Disregard which terminator style ends a line. Default on; when off, a
    /// line ending in CR+LF differs from the same text ending in LF.
    pub ignore_line_endings: bool,
    /// Line pairing controls.
    pub alignment: AlignmentOptions,
}

impl Default for LineCompareOptions {
    fn default() -> Self {
        Self {
            ignore_case: false,
            ignore_leading_whitespace: false,
            ignore_embedded_whitespace: false,
            ignore_trailing_whitespace: false,
            ignore_all_whitespace: false,
            ignore_line_endings: true,
            alignment: AlignmentOptions::default(),
        }
    }
}

/// A user-forced pairing of a run of left lines with a run of right lines.
///
/// Either range may be empty, which forces the opposite side's run to stand
/// alone instead of being matched against anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlignAnchor {
    /// Forced left line range (zero-based, end exclusive).
    pub left: Range<u32>,
    /// Forced right line range (zero-based, end exclusive).
    pub right: Range<u32>,
}

/// Split `text` into lines, keeping each line's terminator attached.
///
/// Text not ending in a terminator yields a final line without one; empty text
/// yields no lines.
#[must_use]
pub fn split_lines(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let bytes = text.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            b'\r' if bytes.get(index + 1) == Some(&b'\n') => {
                let end = index + 2;
                out.push(text.get(start..end).unwrap_or_default());
                start = end;
                index = end;
            }
            b'\r' | b'\n' => {
                let end = index + 1;
                out.push(text.get(start..end).unwrap_or_default());
                start = end;
                index = end;
            }
            _ => index += 1,
        }
    }
    if start < text.len() {
        out.push(text.get(start..).unwrap_or_default());
    }
    out
}

/// Split a line into its body and its terminator.
fn body_and_ending(line: &str) -> (&str, &str) {
    if let Some(rest) = line.strip_suffix("\r\n") {
        (rest, "\r\n")
    } else if let Some(rest) = line.strip_suffix('\n') {
        (rest, "\n")
    } else if let Some(rest) = line.strip_suffix('\r') {
        (rest, "\r")
    } else {
        (line, "")
    }
}

fn is_rule_whitespace(character: char) -> bool {
    matches!(character, ' ' | '\t')
}

/// Split a body into its leading whitespace, its core, and its trailing
/// whitespace. An all-whitespace body is entirely leading.
fn whitespace_parts(body: &str) -> (&str, &str, &str) {
    let core_start = body
        .char_indices()
        .find(|(_, character)| !is_rule_whitespace(*character))
        .map_or(body.len(), |(index, _)| index);
    if core_start == body.len() {
        return (body, "", "");
    }
    let core_end = body
        .char_indices()
        .rev()
        .find(|(_, character)| !is_rule_whitespace(*character))
        .map_or(0, |(index, character)| index + character.len_utf8());
    let (lead, rest) = body.split_at(core_start);
    let (core, trail) = rest.split_at(core_end - core_start);
    (lead, core, trail)
}

/// Derive the comparison key for one line.
///
/// Two lines are considered equal exactly when their keys are equal. The key
/// is never shown to the user and never used to index into the original text.
#[must_use]
pub fn normalize_line(line: &str, options: &LineCompareOptions) -> String {
    let (body, ending) = body_and_ending(line);
    let mut key = String::with_capacity(body.len() + 4);
    if options.ignore_all_whitespace {
        key.extend(body.chars().filter(|c| !is_rule_whitespace(*c)));
    } else {
        let (lead, core, trail) = whitespace_parts(body);
        if !options.ignore_leading_whitespace {
            key.push_str(lead);
        }
        if options.ignore_embedded_whitespace {
            let mut in_whitespace = false;
            for c in core.chars() {
                if is_rule_whitespace(c) {
                    if !in_whitespace {
                        key.push(' ');
                        in_whitespace = true;
                    }
                } else {
                    key.push(c);
                    in_whitespace = false;
                }
            }
        } else {
            key.push_str(core);
        }
        if !options.ignore_trailing_whitespace {
            key.push_str(trail);
        }
    }
    if options.ignore_case {
        key = key.to_lowercase();
    }
    if !options.ignore_line_endings {
        // Separator keeps a body ending in "\r" from colliding with a CR
        // terminated empty body.
        key.push('\u{1}');
        key.push_str(ending);
    }
    key
}

struct Collector {
    hunks: Vec<Hunk>,
    left_len: u32,
    right_len: u32,
    cursor_left: u32,
    cursor_right: u32,
}

impl Collector {
    fn push_same(&mut self, upto_left: u32, upto_right: u32) {
        if upto_left > self.cursor_left {
            self.hunks.push(Hunk {
                kind: HunkKind::Same,
                left: self.cursor_left..upto_left,
                right: self.cursor_right..upto_right,
            });
        }
    }
}

impl Sink for Collector {
    type Out = Vec<Hunk>;

    fn process_change(&mut self, before: Range<u32>, after: Range<u32>) {
        self.push_same(before.start, after.start);
        let kind = match (before.is_empty(), after.is_empty()) {
            (true, false) => HunkKind::RightOnly,
            (false, true) => HunkKind::LeftOnly,
            _ => HunkKind::Changed,
        };
        self.hunks.push(Hunk {
            kind,
            left: before.clone(),
            right: after.clone(),
        });
        self.cursor_left = before.end;
        self.cursor_right = after.end;
    }

    fn finish(mut self) -> Self::Out {
        self.push_same(self.left_len, self.right_len);
        self.hunks
    }
}

fn len_u32<T>(slice: &[T]) -> u32 {
    u32::try_from(slice.len()).unwrap_or(u32::MAX)
}

/// Input size, in lines on the larger side, above which alignment runs Myers
/// instead of histogram.
///
/// Histogram's cost is driven by how often it has to rescan a region for a
/// rarer element, which grows far faster than linearly once the number of
/// distinct lines is large; Myers' cost is proportional to the number of
/// differing lines. Above this size the two agree on the hunks for realistic
/// change densities while histogram costs more than an order of magnitude
/// more time, so the quality argument for histogram no longer buys anything.
const HISTOGRAM_LINE_LIMIT: u32 = 50_000;

/// Engine histogram-seeking alignment uses for inputs of the given size.
fn standard_algorithm(left_len: u32, right_len: u32) -> Algorithm {
    if left_len.max(right_len) > HISTOGRAM_LINE_LIMIT {
        Algorithm::Myers
    } else {
        Algorithm::Histogram
    }
}

fn algorithm_for(mode: AlignmentMode, left_len: u32, right_len: u32) -> Algorithm {
    match mode {
        // Histogram is a patience variant, so both modes map onto the same
        // engine and both take the same size limit: above it histogram's rescan
        // cost grows far faster than Myers' while the hunks agree.
        AlignmentMode::Standard | AlignmentMode::Unaligned | AlignmentMode::Patience => {
            standard_algorithm(left_len, right_len)
        }
        AlignmentMode::Myers => Algorithm::Myers,
        AlignmentMode::MyersMinimal => Algorithm::MyersMinimal,
    }
}

fn hunks_from_keys(
    before: &[String],
    after: &[String],
    mode: AlignmentMode,
    cancel: &dyn Cancel,
) -> Result<Vec<Hunk>, DiffError> {
    check(cancel)?;
    if mode == AlignmentMode::Unaligned {
        return unaligned_hunks(before, after, cancel);
    }
    let (ll, rl) = (len_u32(before), len_u32(after));
    let mut input: InternedInput<&str> = InternedInput::default();
    input.update_before(before.iter().map(String::as_str));
    input.update_after(after.iter().map(String::as_str));
    check(cancel)?;
    let collector = Collector {
        hunks: Vec::new(),
        left_len: ll,
        right_len: rl,
        cursor_left: 0,
        cursor_right: 0,
    };
    Ok(diff(algorithm_for(mode, ll, rl), &input, collector))
}

fn unaligned_hunks(
    before: &[String],
    after: &[String],
    cancel: &dyn Cancel,
) -> Result<Vec<Hunk>, DiffError> {
    let (ll, rl) = (len_u32(before), len_u32(after));
    let common = ll.min(rl);
    let mut out: Vec<Hunk> = Vec::new();
    let mut run_start = 0u32;
    let mut run_same = true;
    for i in 0..common {
        let idx = i as usize;
        if idx.is_multiple_of(CANCEL_POLL_LINES) {
            check(cancel)?;
        }
        let same = before.get(idx) == after.get(idx);
        if i == 0 {
            run_same = same;
        } else if same != run_same {
            out.push(Hunk {
                kind: if run_same {
                    HunkKind::Same
                } else {
                    HunkKind::Changed
                },
                left: run_start..i,
                right: run_start..i,
            });
            run_start = i;
            run_same = same;
        }
    }
    if common > run_start {
        out.push(Hunk {
            kind: if run_same {
                HunkKind::Same
            } else {
                HunkKind::Changed
            },
            left: run_start..common,
            right: run_start..common,
        });
    }
    if ll > common {
        out.push(Hunk {
            kind: HunkKind::LeftOnly,
            left: common..ll,
            right: common..common,
        });
    } else if rl > common {
        out.push(Hunk {
            kind: HunkKind::RightOnly,
            left: common..common,
            right: common..rl,
        });
    }
    Ok(out)
}

/// Lines between two polls of the cancellation flag. Large enough that the
/// poll is lost in the surrounding work, small enough to stay responsive.
const CANCEL_POLL_LINES: usize = 4096;

/// Drop empty hunks and fuse neighbours that carry the same classification.
fn coalesce(hunks: Vec<Hunk>) -> Vec<Hunk> {
    let mut out: Vec<Hunk> = Vec::with_capacity(hunks.len());
    for h in hunks {
        if h.left.is_empty() && h.right.is_empty() {
            continue;
        }
        match out.last_mut() {
            Some(prev) if prev.kind == h.kind => {
                prev.left.end = h.left.end;
                prev.right.end = h.right.end;
            }
            _ => out.push(h),
        }
    }
    out
}

/// Fuse every run of adjacent non-`Same` hunks into one region and re-derive
/// its classification from which sides are non-empty.
fn fuse_differences(hunks: Vec<Hunk>) -> Vec<Hunk> {
    let mut out: Vec<Hunk> = Vec::with_capacity(hunks.len());
    for h in hunks {
        if h.left.is_empty() && h.right.is_empty() {
            continue;
        }
        let fuse = matches!(out.last(), Some(prev) if prev.kind != HunkKind::Same)
            && h.kind != HunkKind::Same;
        if fuse {
            if let Some(prev) = out.last_mut() {
                prev.left.end = h.left.end;
                prev.right.end = h.right.end;
                prev.kind = match (prev.left.is_empty(), prev.right.is_empty()) {
                    (true, false) => HunkKind::RightOnly,
                    (false, true) => HunkKind::LeftOnly,
                    _ => HunkKind::Changed,
                };
            }
        } else {
            out.push(h);
        }
    }
    coalesce(out)
}

fn apply_skew_tolerance(hunks: Vec<Hunk>, tolerance: u32) -> Vec<Hunk> {
    let demoted: Vec<Hunk> = hunks
        .into_iter()
        .map(|mut h| {
            if h.kind == HunkKind::Same {
                let skew = h.left.start.abs_diff(h.right.start);
                if skew > tolerance {
                    h.kind = HunkKind::Changed;
                }
            }
            h
        })
        .collect();
    fuse_differences(demoted)
}

/// Largest changed region, in lines per side, that closeness matching will
/// refine. The pairing pass is quadratic, so wider regions stay whole.
const CLOSENESS_LIMIT: u32 = 256;

/// Similarity below which two lines are never paired, in thousandths.
const CLOSENESS_THRESHOLD: u32 = 500;

/// Bytes at the start of a line that contribute to its bigram profile. Lines
/// longer than this compare on their first `CLOSENESS_PROFILE_BYTES` bytes, so
/// profile construction stays proportional to the region's line count rather
/// than to its total size.
const CLOSENESS_PROFILE_BYTES: usize = 1024;

/// Profile merge steps one changed region may spend on pairwise similarity.
/// The pass only subdivides a region that the main alignment already reported
/// as a single difference, so exceeding the budget costs presentation detail
/// and never correctness; without the bound a region of long lines costs the
/// square of its line count times its line length.
const CLOSENESS_WORK_BUDGET: u64 = 16_000_000;

/// Bigram histogram of one line, as ascending (bigram, count) pairs.
struct Profile {
    pairs: Vec<(u16, u32)>,
    total: u32,
    len: usize,
}

fn build_profile(text: &str) -> Profile {
    let capped = text.len().min(CLOSENESS_PROFILE_BYTES);
    let bytes = text.as_bytes().get(..capped).unwrap_or_default();
    let mut keys: Vec<u16> = bytes
        .windows(2)
        .filter_map(|w| {
            let (&x, &y) = w.first().zip(w.get(1))?;
            Some((u16::from(x) << 8) | u16::from(y))
        })
        .collect();
    keys.sort_unstable();
    let total = u32::try_from(keys.len()).unwrap_or(u32::MAX);
    let mut pairs: Vec<(u16, u32)> = Vec::with_capacity(keys.len());
    for key in keys {
        match pairs.last_mut() {
            Some(slot) if slot.0 == key => slot.1 += 1,
            _ => pairs.push((key, 1)),
        }
    }
    Profile {
        pairs,
        total,
        len: capped,
    }
}

/// Whether a Dice coefficient of [`CLOSENESS_THRESHOLD`] is reachable at all
/// for two lines of these lengths. Shared bigrams cannot exceed the shorter
/// line's bigram count, so `3 * shorter - 2 < longer` rules the pair out
/// without touching either line's bytes.
fn length_compatible(a: usize, b: usize) -> bool {
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    lo >= 2 && hi + 2 <= 3 * lo
}

/// Dice coefficient over byte bigrams, scaled to thousandths.
fn similarity(a: &Profile, b: &Profile) -> u32 {
    if a.total == 0 || b.total == 0 || !length_compatible(a.len, b.len) {
        return 0;
    }
    let (mut i, mut j) = (0usize, 0usize);
    let mut common = 0u32;
    while let (Some(&(ka, ca)), Some(&(kb, cb))) = (a.pairs.get(i), b.pairs.get(j)) {
        match ka.cmp(&kb) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                common += ca.min(cb);
                i += 1;
                j += 1;
            }
        }
    }
    let total = a.total.saturating_add(b.total);
    (2 * common.min(total / 2) * 1000)
        .checked_div(total)
        .unwrap_or(0)
}

/// Build the pairwise similarity matrix and the alignment score matrix that
/// maximizes the total similarity of the chosen pairs.
///
/// `skew` bounds how far apart in line index a pair may sit: a pairing the
/// skew tolerance already rejected must not be reintroduced here.
fn closeness_matrices(
    left: &[String],
    right: &[String],
    left_start: u32,
    right_start: u32,
    skew: Option<u32>,
    cancel: &dyn Cancel,
) -> Result<(Vec<u32>, Vec<u32>), DiffError> {
    let nu = left.len();
    let mu = right.len();
    let width = mu + 1;
    let left_profiles: Vec<Profile> = left.iter().map(|s| build_profile(s)).collect();
    let right_profiles: Vec<Profile> = right.iter().map(|s| build_profile(s)).collect();
    let mut sim = vec![0u32; nu * mu];
    for (i, a) in left_profiles.iter().enumerate() {
        check(cancel)?;
        let li = left_start.saturating_add(u32::try_from(i).unwrap_or(u32::MAX));
        for (j, b) in right_profiles.iter().enumerate() {
            let ri = right_start.saturating_add(u32::try_from(j).unwrap_or(u32::MAX));
            if skew.is_some_and(|tolerance| li.abs_diff(ri) > tolerance) {
                continue;
            }
            let s = if left.get(i) == right.get(j) {
                1000
            } else {
                similarity(a, b)
            };
            if let Some(slot) = sim.get_mut(i * mu + j) {
                *slot = if s >= CLOSENESS_THRESHOLD { s } else { 0 };
            }
        }
    }
    let mut score = vec![0u32; (nu + 1) * width];
    for i in 1..=nu {
        for j in 1..=mu {
            let diag = score.get((i - 1) * width + j - 1).copied().unwrap_or(0)
                + sim.get((i - 1) * mu + j - 1).copied().unwrap_or(0);
            let up = score.get((i - 1) * width + j).copied().unwrap_or(0);
            let prev = score.get(i * width + j - 1).copied().unwrap_or(0);
            if let Some(slot) = score.get_mut(i * width + j) {
                *slot = diag.max(up).max(prev);
            }
        }
    }
    Ok((sim, score))
}

/// Upper bound on the profile merge steps a region of these lines costs.
fn closeness_work(left: &[String], right: &[String]) -> u64 {
    let bigrams = |lines: &[String]| -> u64 {
        lines
            .iter()
            .map(|s| u64::try_from(s.len().min(CLOSENESS_PROFILE_BYTES)).unwrap_or(u64::MAX))
            .sum()
    };
    let (ln, rn) = (left.len() as u64, right.len() as u64);
    bigrams(left)
        .saturating_mul(rn)
        .saturating_add(bigrams(right).saturating_mul(ln))
}

/// Pair the lines of one changed region by similarity, emitting finer hunks.
fn closeness_match(
    left_keys: &[String],
    right_keys: &[String],
    region: &Hunk,
    skew: Option<u32>,
    cancel: &dyn Cancel,
    out: &mut Vec<Hunk>,
) -> Result<(), DiffError> {
    let (ls, le) = (region.left.start, region.left.end);
    let (rs, re) = (region.right.start, region.right.end);
    let left_count = le.saturating_sub(ls);
    let right_count = re.saturating_sub(rs);
    if left_count == 0
        || right_count == 0
        || left_count > CLOSENESS_LIMIT
        || right_count > CLOSENESS_LIMIT
    {
        out.push(region.clone());
        return Ok(());
    }
    let nu = left_count as usize;
    let mu = right_count as usize;
    let width = mu + 1;
    let left_lines = left_keys.get(ls as usize..le as usize).unwrap_or_default();
    let right_lines = right_keys.get(rs as usize..re as usize).unwrap_or_default();
    if closeness_work(left_lines, right_lines) > CLOSENESS_WORK_BUDGET {
        out.push(region.clone());
        return Ok(());
    }
    let (sim, score) = closeness_matrices(left_lines, right_lines, ls, rs, skew, cancel)?;
    let mut steps: Vec<Hunk> = Vec::new();
    let (mut i, mut j) = (nu, mu);
    while i > 0 || j > 0 {
        let here = score.get(i * width + j).copied().unwrap_or(0);
        let pair = if i > 0 && j > 0 {
            sim.get((i - 1) * mu + j - 1).copied().unwrap_or(0)
        } else {
            0
        };
        let diag = if i > 0 && j > 0 {
            score.get((i - 1) * width + j - 1).copied().unwrap_or(0)
        } else {
            0
        };
        if i > 0 && j > 0 && pair > 0 && here == diag + pair {
            let li = ls + u32::try_from(i - 1).unwrap_or(0);
            let ri = rs + u32::try_from(j - 1).unwrap_or(0);
            let same = left_keys.get(li as usize) == right_keys.get(ri as usize);
            steps.push(Hunk {
                kind: if same {
                    HunkKind::Same
                } else {
                    HunkKind::Changed
                },
                left: li..li + 1,
                right: ri..ri + 1,
            });
            i -= 1;
            j -= 1;
        } else if i > 0 && (j == 0 || here == score.get((i - 1) * width + j).copied().unwrap_or(0))
        {
            let li = ls + u32::try_from(i - 1).unwrap_or(0);
            let rj = rs + u32::try_from(j).unwrap_or(0);
            steps.push(Hunk {
                kind: HunkKind::LeftOnly,
                left: li..li + 1,
                right: rj..rj,
            });
            i -= 1;
        } else if j > 0 {
            let li = ls + u32::try_from(i).unwrap_or(0);
            let rj = rs + u32::try_from(j - 1).unwrap_or(0);
            steps.push(Hunk {
                kind: HunkKind::RightOnly,
                left: li..li,
                right: rj..rj + 1,
            });
            j -= 1;
        } else {
            break;
        }
    }
    // Closeness matching only adds pairings. A region in which nothing pairs
    // keeps the classification the main pass gave it.
    if !steps
        .iter()
        .any(|h| matches!(h.kind, HunkKind::Changed | HunkKind::Same))
    {
        out.push(region.clone());
        return Ok(());
    }
    steps.reverse();
    out.extend(steps);
    Ok(())
}

fn refine_closeness(
    hunks: Vec<Hunk>,
    left_keys: &[String],
    right_keys: &[String],
    skew: Option<u32>,
    cancel: &dyn Cancel,
) -> Result<Vec<Hunk>, DiffError> {
    let mut out = Vec::with_capacity(hunks.len());
    for h in hunks {
        if h.kind == HunkKind::Changed {
            closeness_match(left_keys, right_keys, &h, skew, cancel, &mut out)?;
        } else {
            out.push(h);
        }
    }
    Ok(coalesce(out))
}

fn split_changed(hunks: Vec<Hunk>) -> Vec<Hunk> {
    let mut out = Vec::with_capacity(hunks.len() + 4);
    for h in hunks {
        if h.kind == HunkKind::Changed {
            if !h.left.is_empty() {
                out.push(Hunk {
                    kind: HunkKind::LeftOnly,
                    left: h.left.clone(),
                    right: h.right.start..h.right.start,
                });
            }
            if !h.right.is_empty() {
                out.push(Hunk {
                    kind: HunkKind::RightOnly,
                    left: h.left.end..h.left.end,
                    right: h.right.clone(),
                });
            }
        } else {
            out.push(h);
        }
    }
    out
}

fn post_process(
    hunks: Vec<Hunk>,
    l: &[String],
    r: &[String],
    opts: &AlignmentOptions,
    cancel: &dyn Cancel,
) -> Result<Vec<Hunk>, DiffError> {
    let mut out = coalesce(hunks);
    if let Some(tolerance) = opts.skew_tolerance {
        out = apply_skew_tolerance(out, tolerance);
    }
    if opts.use_closeness_matching && opts.mode != AlignmentMode::Unaligned {
        out = refine_closeness(out, l, r, opts.skew_tolerance, cancel)?;
    }
    if opts.never_align_differences {
        out = coalesce(split_changed(out));
    }
    Ok(out)
}

fn keys(
    lines: &[&str],
    options: &LineCompareOptions,
    cancel: &dyn Cancel,
) -> Result<Vec<String>, DiffError> {
    let mut out = Vec::with_capacity(lines.len());
    for (i, line) in lines.iter().enumerate() {
        if i.is_multiple_of(CANCEL_POLL_LINES) {
            check(cancel)?;
        }
        out.push(normalize_line(line, options));
    }
    Ok(out)
}

/// Diff two texts line by line with default options.
///
/// The returned hunks cover both inputs completely and in order, with no gaps
/// and no overlaps.
#[must_use]
pub fn diff_lines(left: &str, right: &str) -> Vec<Hunk> {
    diff_lines_with(left, right, &LineCompareOptions::default())
}

/// Diff two texts line by line under the given options.
#[must_use]
pub fn diff_lines_with(left: &str, right: &str, options: &LineCompareOptions) -> Vec<Hunk> {
    diff_line_slices(&split_lines(left), &split_lines(right), options)
}

/// Diff two already split line sequences.
#[must_use]
pub fn diff_line_slices(left: &[&str], right: &[&str], options: &LineCompareOptions) -> Vec<Hunk> {
    diff_line_slices_cancellable(left, right, options, &NeverCancel).unwrap_or_default()
}

/// Diff two already split line sequences, abandoning the work when `cancel` is
/// raised.
///
/// # Errors
///
/// Returns [`DiffError::Cancelled`] when the flag is raised before the
/// comparison finishes.
pub fn diff_line_slices_cancellable(
    left: &[&str],
    right: &[&str],
    options: &LineCompareOptions,
    cancel: &dyn Cancel,
) -> Result<Vec<Hunk>, DiffError> {
    let lk = keys(left, options, cancel)?;
    let rk = keys(right, options, cancel)?;
    let raw = hunks_from_keys(&lk, &rk, options.alignment.mode, cancel)?;
    post_process(raw, &lk, &rk, &options.alignment, cancel)
}

/// Diff two line sequences while forcing the given pairings.
///
/// Anchors must be sorted, must not overlap, must not run backwards, and must
/// lie within their side's line count. Each anchor splits the inputs, so the
/// text before and after it is aligned independently of the text on the other
/// side of it.
///
/// # Errors
///
/// Returns [`DiffError::Malformed`] when an anchor breaks any of those rules,
/// and [`DiffError::Cancelled`] when `cancel` is raised.
pub fn diff_line_slices_anchored_cancellable(
    left: &[&str],
    right: &[&str],
    options: &LineCompareOptions,
    anchors: &[AlignAnchor],
    cancel: &dyn Cancel,
) -> Result<Vec<Hunk>, DiffError> {
    if anchors.is_empty() {
        return diff_line_slices_cancellable(left, right, options, cancel);
    }
    let (ll, rl) = (len_u32(left), len_u32(right));
    validate_anchors(anchors, ll, rl)?;
    let lk = keys(left, options, cancel)?;
    let rk = keys(right, options, cancel)?;
    let mut out: Vec<Hunk> = Vec::new();
    let mut cl = 0u32;
    let mut cr = 0u32;
    for anchor in anchors {
        segment(
            &lk,
            &rk,
            cl..anchor.left.start,
            cr..anchor.right.start,
            options,
            cancel,
            &mut out,
        )?;
        push_anchor(&lk, &rk, anchor, &mut out);
        cl = anchor.left.end;
        cr = anchor.right.end;
    }
    segment(&lk, &rk, cl..ll, cr..rl, options, cancel, &mut out)?;
    Ok(coalesce(out))
}

/// Diff two line sequences while forcing the given pairings.
///
/// # Errors
///
/// Returns [`DiffError::Malformed`] when an anchor is out of order, overlaps
/// its predecessor, runs backwards, or reaches past its side's line count.
pub fn diff_line_slices_anchored(
    left: &[&str],
    right: &[&str],
    options: &LineCompareOptions,
    anchors: &[AlignAnchor],
) -> Result<Vec<Hunk>, DiffError> {
    diff_line_slices_anchored_cancellable(left, right, options, anchors, &NeverCancel)
}

fn validate_anchors(anchors: &[AlignAnchor], ll: u32, rl: u32) -> Result<(), DiffError> {
    let mut cl = 0u32;
    let mut cr = 0u32;
    for (i, anchor) in anchors.iter().enumerate() {
        if anchor.left.start > anchor.left.end || anchor.right.start > anchor.right.end {
            return Err(DiffError::malformed(
                "anchor",
                format!("anchor {i} has a reversed range"),
            ));
        }
        if anchor.left.end > ll || anchor.right.end > rl {
            return Err(DiffError::malformed(
                "anchor",
                format!("anchor {i} reaches past the input ({ll} left lines, {rl} right lines)"),
            ));
        }
        if anchor.left.start < cl || anchor.right.start < cr {
            return Err(DiffError::malformed(
                "anchor",
                format!("anchor {i} is out of order or overlaps its predecessor"),
            ));
        }
        cl = anchor.left.end;
        cr = anchor.right.end;
    }
    Ok(())
}

fn segment(
    lk: &[String],
    rk: &[String],
    left: Range<u32>,
    right: Range<u32>,
    options: &LineCompareOptions,
    cancel: &dyn Cancel,
    out: &mut Vec<Hunk>,
) -> Result<(), DiffError> {
    if left.is_empty() && right.is_empty() {
        return Ok(());
    }
    let ls = lk
        .get(left.start as usize..left.end as usize)
        .unwrap_or_default();
    let rs = rk
        .get(right.start as usize..right.end as usize)
        .unwrap_or_default();
    let raw = hunks_from_keys(ls, rs, options.alignment.mode, cancel)?;
    for mut h in post_process(raw, ls, rs, &options.alignment, cancel)? {
        h.left.start += left.start;
        h.left.end += left.start;
        h.right.start += right.start;
        h.right.end += right.start;
        out.push(h);
    }
    Ok(())
}

fn push_anchor(lk: &[String], rk: &[String], anchor: &AlignAnchor, out: &mut Vec<Hunk>) {
    let (le, re) = (anchor.left.is_empty(), anchor.right.is_empty());
    let kind = match (le, re) {
        (true, true) => return,
        (true, false) => HunkKind::RightOnly,
        (false, true) => HunkKind::LeftOnly,
        (false, false) => {
            let ls = lk.get(anchor.left.start as usize..anchor.left.end as usize);
            let rs = rk.get(anchor.right.start as usize..anchor.right.end as usize);
            if ls.is_some() && ls == rs {
                HunkKind::Same
            } else {
                HunkKind::Changed
            }
        }
    };
    out.push(Hunk {
        kind,
        left: anchor.left.clone(),
        right: anchor.right.clone(),
    });
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn kinds(hunks: &[Hunk]) -> Vec<HunkKind> {
        hunks.iter().map(|h| h.kind).collect()
    }

    fn covers(hunks: &[Hunk], ll: u32, rl: u32) {
        let mut cl = 0;
        let mut cr = 0;
        for h in hunks {
            assert_eq!(h.left.start, cl);
            assert_eq!(h.right.start, cr);
            cl = h.left.end;
            cr = h.right.end;
        }
        assert_eq!(cl, ll);
        assert_eq!(cr, rl);
    }

    #[test]
    fn identical_inputs_yield_one_same_hunk() {
        let hunks = diff_lines("a\nb\n", "a\nb\n");
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].kind, HunkKind::Same);
        assert_eq!(hunks[0].left, 0..2);
    }

    #[test]
    fn insertion_is_right_only() {
        let hunks = diff_lines("a\nc\n", "a\nb\nc\n");
        assert_eq!(
            kinds(&hunks),
            [HunkKind::Same, HunkKind::RightOnly, HunkKind::Same]
        );
        assert_eq!(hunks[1].right, 1..2);
    }

    #[test]
    fn hunks_cover_both_inputs_without_gaps() {
        let hunks = diff_lines("x\ny\nz\n", "x\nq\nz\nw\n");
        covers(&hunks, 3, 4);
    }

    #[test]
    fn split_lines_keeps_terminators() {
        assert_eq!(split_lines("a\r\nb\nc"), ["a\r\n", "b\n", "c"]);
        assert_eq!(split_lines("a\rb\rc\r"), ["a\r", "b\r", "c\r"]);
        assert_eq!(split_lines("a\rb\nc\r\nd"), ["a\r", "b\n", "c\r\n", "d"]);
        assert_eq!(split_lines(""), Vec::<&str>::new());
        assert_eq!(split_lines("\n"), ["\n"]);
    }

    #[test]
    fn ignore_case_hides_case_only_changes() {
        let opts = LineCompareOptions {
            ignore_case: true,
            ..LineCompareOptions::default()
        };
        let hunks = diff_lines_with("Hello\n", "HELLO\n", &opts);
        assert_eq!(kinds(&hunks), [HunkKind::Same]);
    }

    #[test]
    fn leading_whitespace_option_is_positional() {
        let opts = LineCompareOptions {
            ignore_leading_whitespace: true,
            ..LineCompareOptions::default()
        };
        assert_eq!(
            kinds(&diff_lines_with("    a\n", "\ta\n", &opts)),
            [HunkKind::Same]
        );
        assert_eq!(
            kinds(&diff_lines_with("a  \n", "a\n", &opts)),
            [HunkKind::Changed]
        );
    }

    #[test]
    fn embedded_whitespace_option_collapses_only_the_core() {
        let opts = LineCompareOptions {
            ignore_embedded_whitespace: true,
            ..LineCompareOptions::default()
        };
        assert_eq!(
            kinds(&diff_lines_with("a   b\n", "a b\n", &opts)),
            [HunkKind::Same]
        );
        assert_eq!(
            kinds(&diff_lines_with("  a b\n", "a b\n", &opts)),
            [HunkKind::Changed]
        );
        assert_eq!(
            kinds(&diff_lines_with("a b\n", "ab\n", &opts)),
            [HunkKind::Changed]
        );
    }

    #[test]
    fn ignore_all_whitespace_removes_separation() {
        let opts = LineCompareOptions {
            ignore_all_whitespace: true,
            ..LineCompareOptions::default()
        };
        assert_eq!(
            kinds(&diff_lines_with(" a b \n", "ab\n", &opts)),
            [HunkKind::Same]
        );
    }

    #[test]
    fn line_endings_compared_only_when_asked() {
        let ignoring = LineCompareOptions::default();
        assert_eq!(
            kinds(&diff_lines_with("a\r\n", "a\n", &ignoring)),
            [HunkKind::Same]
        );
        let comparing = LineCompareOptions {
            ignore_line_endings: false,
            ..LineCompareOptions::default()
        };
        assert_eq!(
            kinds(&diff_lines_with("a\r\n", "a\n", &comparing)),
            [HunkKind::Changed]
        );
    }

    #[test]
    fn missing_final_terminator_is_same_when_terminator_style_is_ignored() {
        let options = LineCompareOptions::default();
        assert_eq!(
            kinds(&diff_lines_with("a\nb", "a\nb\n", &options)),
            [HunkKind::Same]
        );
        let comparing = LineCompareOptions {
            ignore_line_endings: false,
            ..LineCompareOptions::default()
        };
        assert_eq!(
            kinds(&diff_lines_with("a\nb", "a\nb\n", &comparing)),
            [HunkKind::Same, HunkKind::Changed]
        );
    }

    #[test]
    fn only_spaces_and_tabs_are_whitespace_for_comparison_rules() {
        let options = LineCompareOptions {
            ignore_all_whitespace: true,
            ..LineCompareOptions::default()
        };
        assert_eq!(
            kinds(&diff_lines_with("\u{00A0}\n", " \n", &options)),
            [HunkKind::Changed]
        );
        assert_eq!(
            kinds(&diff_lines_with("\u{2028}\n", "\n", &options)),
            [HunkKind::Changed]
        );
    }

    #[test]
    fn unaligned_mode_pairs_by_index() {
        let opts = LineCompareOptions {
            alignment: AlignmentOptions {
                mode: AlignmentMode::Unaligned,
                ..AlignmentOptions::default()
            },
            ..LineCompareOptions::default()
        };
        let hunks = diff_line_slices(&["a\n", "b\n"], &["x\n", "b\n", "c\n"], &opts);
        assert_eq!(
            kinds(&hunks),
            [HunkKind::Changed, HunkKind::Same, HunkKind::RightOnly]
        );
        covers(&hunks, 2, 3);
    }

    #[test]
    fn never_align_differences_splits_changed_hunks() {
        let opts = LineCompareOptions {
            alignment: AlignmentOptions {
                never_align_differences: true,
                ..AlignmentOptions::default()
            },
            ..LineCompareOptions::default()
        };
        let hunks = diff_line_slices(&["aaa\n"], &["bbb\n"], &opts);
        assert_eq!(kinds(&hunks), [HunkKind::LeftOnly, HunkKind::RightOnly]);
        covers(&hunks, 1, 1);
    }

    #[test]
    fn skew_tolerance_rejects_distant_matches() {
        let left: Vec<&str> = vec!["match\n"];
        let mut right: Vec<&str> = vec!["x\n"; 40];
        right.push("match\n");
        for closeness in [false, true] {
            let strict = LineCompareOptions {
                alignment: AlignmentOptions {
                    skew_tolerance: Some(2),
                    use_closeness_matching: closeness,
                    ..AlignmentOptions::default()
                },
                ..LineCompareOptions::default()
            };
            let hunks = diff_line_slices(&left, &right, &strict);
            assert!(
                hunks.iter().all(|h| h.kind != HunkKind::Same),
                "closeness={closeness}: {hunks:?}"
            );
            covers(&hunks, 1, 41);
        }
    }

    #[test]
    fn closeness_never_pairs_beyond_the_skew_tolerance() {
        let left: Vec<&str> = vec!["let alpha = 1;\n"];
        let mut right: Vec<&str> = vec!["zzzzzzzzzzzzzz\n"; 12];
        right.push("let alpha = 2;\n");
        let opts = LineCompareOptions {
            alignment: AlignmentOptions {
                skew_tolerance: Some(1),
                ..AlignmentOptions::default()
            },
            ..LineCompareOptions::default()
        };
        let hunks = diff_line_slices(&left, &right, &opts);
        covers(&hunks, 1, 13);
        for h in &hunks {
            assert!(
                h.kind != HunkKind::Changed || h.left.start.abs_diff(h.right.start) <= 1,
                "{h:?}"
            );
        }
    }

    #[test]
    fn standard_switches_to_myers_on_large_inputs() {
        assert_eq!(standard_algorithm(10, 10), Algorithm::Histogram);
        assert_eq!(
            standard_algorithm(HISTOGRAM_LINE_LIMIT, HISTOGRAM_LINE_LIMIT),
            Algorithm::Histogram
        );
        assert_eq!(
            standard_algorithm(HISTOGRAM_LINE_LIMIT + 1, 0),
            Algorithm::Myers
        );
        assert_eq!(
            algorithm_for(AlignmentMode::Standard, HISTOGRAM_LINE_LIMIT + 1, 0),
            Algorithm::Myers
        );
        // Patience is a histogram variant and takes the same limit.
        assert_eq!(
            algorithm_for(AlignmentMode::Patience, HISTOGRAM_LINE_LIMIT, 0),
            Algorithm::Histogram
        );
        assert_eq!(
            algorithm_for(AlignmentMode::Patience, HISTOGRAM_LINE_LIMIT + 1, 0),
            Algorithm::Myers
        );
        // A mode naming one engine keeps it at every size.
        assert_eq!(algorithm_for(AlignmentMode::Myers, 1, 1), Algorithm::Myers);
        assert_eq!(
            algorithm_for(AlignmentMode::MyersMinimal, u32::MAX, u32::MAX),
            Algorithm::MyersMinimal
        );
    }

    #[test]
    #[ignore = "a million lines is minutes in a debug build; run it in release"]
    fn a_million_lines_with_one_percent_changes_is_quick() {
        let (left, right) = million_line_pair();
        let lr: Vec<&str> = left.iter().map(String::as_str).collect();
        let rr: Vec<&str> = right.iter().map(String::as_str).collect();
        let start = std::time::Instant::now();
        let hunks = diff_line_slices(&lr, &rr, &LineCompareOptions::default());
        let elapsed = start.elapsed();
        covers(&hunks, len_u32(&lr), len_u32(&rr));
        assert!(
            elapsed < std::time::Duration::from_secs(90),
            "took {elapsed:?}"
        );
    }

    /// Always-on sibling of the million line case, sized to stay quick in a
    /// debug build while still crossing the histogram line limit.
    #[test]
    fn crossing_the_histogram_limit_stays_quick() {
        let (left, right) = changed_pair(HISTOGRAM_LINE_LIMIT as usize * 2, 100);
        let lr: Vec<&str> = left.iter().map(String::as_str).collect();
        let rr: Vec<&str> = right.iter().map(String::as_str).collect();
        let start = std::time::Instant::now();
        let hunks = diff_line_slices(&lr, &rr, &LineCompareOptions::default());
        let elapsed = start.elapsed();
        covers(&hunks, len_u32(&lr), len_u32(&rr));
        assert!(
            hunks.iter().any(|h| h.kind != HunkKind::Same),
            "the seeded changes must show up"
        );
        println!("crossing the histogram limit took {elapsed:?}");
    }

    /// Correctness half of the wide region case, on a region small enough for
    /// a debug build: the hunks still cover both sides end to end.
    #[test]
    fn a_narrow_region_of_long_lines_is_covered_end_to_end() {
        let long = |tag: char| -> String {
            format!("{}\n", std::iter::repeat_n(tag, 1024).collect::<String>())
        };
        let mut left = Vec::new();
        let mut right = Vec::new();
        for i in 0..20u32 {
            left.push(long(char::from(b'a' + (i % 13) as u8)));
            right.push(long(char::from(b'n' + (i % 13) as u8)));
        }
        let lr: Vec<&str> = left.iter().map(String::as_str).collect();
        let rr: Vec<&str> = right.iter().map(String::as_str).collect();
        let hunks = diff_line_slices(&lr, &rr, &LineCompareOptions::default());
        covers(&hunks, 20, 20);
    }

    fn changed_pair(lines: usize, every: usize) -> (Vec<String>, Vec<String>) {
        let left: Vec<String> = (0..lines)
            .map(|i| format!("line {i} of the file\n"))
            .collect();
        let right: Vec<String> = left
            .iter()
            .enumerate()
            .map(|(i, l)| {
                if i % every == 0 {
                    format!("changed {i}\n")
                } else {
                    l.clone()
                }
            })
            .collect();
        (left, right)
    }

    fn million_line_pair() -> (Vec<String>, Vec<String>) {
        changed_pair(1_000_000, 100)
    }

    #[test]
    #[cfg_attr(
        debug_assertions,
        ignore = "wall-clock budget holds for release builds"
    )]
    fn wide_regions_of_long_lines_skip_closeness_matching() {
        // Every pair in the region is dissimilar, so the pass can only cost
        // time; the budget has to stop it before it spends that time.
        let long = |tag: char| -> String {
            format!("{}\n", std::iter::repeat_n(tag, 1024).collect::<String>())
        };
        let mut left = Vec::new();
        let mut right = Vec::new();
        for i in 0..100u32 {
            left.push(long(char::from(b'a' + (i % 13) as u8)));
            right.push(long(char::from(b'n' + (i % 13) as u8)));
        }
        let lr: Vec<&str> = left.iter().map(String::as_str).collect();
        let rr: Vec<&str> = right.iter().map(String::as_str).collect();
        let start = std::time::Instant::now();
        let hunks = diff_line_slices(&lr, &rr, &LineCompareOptions::default());
        let elapsed = start.elapsed();
        covers(&hunks, 100, 100);
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "took {elapsed:?}"
        );
    }

    #[test]
    fn a_raised_flag_stops_the_comparison() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let flag = AtomicBool::new(true);
        let err =
            diff_line_slices_cancellable(&["a\n"], &["b\n"], &LineCompareOptions::default(), &flag);
        assert_eq!(err, Err(DiffError::Cancelled));
        flag.store(false, Ordering::Relaxed);
        assert!(diff_line_slices_cancellable(
            &["a\n"],
            &["b\n"],
            &LineCompareOptions::default(),
            &flag
        )
        .is_ok());
    }

    #[test]
    #[allow(clippy::reversed_empty_ranges)]
    fn malformed_anchors_are_rejected() {
        let left = ["a\n", "b\n"];
        let right = ["a\n", "b\n"];
        let opts = LineCompareOptions::default();
        let reversed = [AlignAnchor {
            left: 2..1,
            right: 0..1,
        }];
        assert!(diff_line_slices_anchored(&left, &right, &opts, &reversed).is_err());
        let past_end = [AlignAnchor {
            left: 0..9,
            right: 0..1,
        }];
        assert!(diff_line_slices_anchored(&left, &right, &opts, &past_end).is_err());
        let unordered = [
            AlignAnchor {
                left: 1..2,
                right: 1..2,
            },
            AlignAnchor {
                left: 0..1,
                right: 0..1,
            },
        ];
        assert!(diff_line_slices_anchored(&left, &right, &opts, &unordered).is_err());
    }

    #[test]
    fn closeness_matching_pairs_similar_lines() {
        let opts = LineCompareOptions::default();
        let hunks = diff_line_slices(
            &["let alpha = 1;\n", "let beta = 2;\n"],
            &[
                "let alpha = 11;\n",
                "brand new line here\n",
                "let beta = 22;\n",
            ],
            &opts,
        );
        covers(&hunks, 2, 3);
        assert!(hunks.iter().filter(|h| h.kind == HunkKind::Changed).count() >= 2);
    }

    #[test]
    fn anchor_forces_a_pairing() {
        let left = ["a\n", "b\n", "c\n"];
        let right = ["a\n", "c\n"];
        let anchors = [AlignAnchor {
            left: 1..2,
            right: 1..2,
        }];
        let hunks =
            diff_line_slices_anchored(&left, &right, &LineCompareOptions::default(), &anchors)
                .unwrap();
        covers(&hunks, 3, 2);
        let forced = hunks.iter().find(|h| h.left == (1..2)).unwrap();
        assert_eq!(forced.kind, HunkKind::Changed);
        assert_eq!(forced.right, 1..2);
    }

    #[test]
    fn empty_anchor_isolates_lines() {
        let left = ["a\n", "b\n"];
        let right = ["a\n", "b\n"];
        let anchors = [AlignAnchor {
            left: 1..2,
            right: 1..1,
        }];
        let hunks =
            diff_line_slices_anchored(&left, &right, &LineCompareOptions::default(), &anchors)
                .unwrap();
        covers(&hunks, 2, 2);
        assert!(hunks.iter().any(|h| h.kind == HunkKind::LeftOnly));
    }

    #[test]
    fn empty_inputs_produce_no_hunks() {
        assert!(diff_lines("", "").is_empty());
    }
}
