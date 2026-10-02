//! Searching one side for a run of bytes.
//!
//! A search reads the whole side, so it runs on a worker and reports one
//! outcome. The scan itself is a plain function over a slice, which is what
//! lets every case be tested without a thread.

use crate::chars::CharEncoding;
use crate::edit::from_hex_text;
use crate::model::Side;
use ca_ui::worker::{Cancel, Job, Terminal};
use std::sync::Arc;

/// Bytes scanned between two cancellation checks.
const SCAN_STEP: u64 = 1 << 20;

/// How the search phrase is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PatternKind {
    /// Pairs of hexadecimal digits.
    #[default]
    Bytes,
    /// Characters, turned into bytes by the character area's encoding.
    Text,
}

impl PatternKind {
    /// The label a menu shows.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Bytes => "Hex bytes",
            Self::Text => "Text",
        }
    }
}

/// What a search looks for and how.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FindSettings {
    /// The phrase as it was typed.
    pub pattern: String,
    /// How the phrase is read.
    pub kind: PatternKind,
    /// True when an upper case letter only matches an upper case letter.
    pub match_case: bool,
    /// True when a search that reaches the end starts again at the other one.
    pub wrap: bool,
}

/// Why a phrase cannot be searched for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatternError {
    /// The phrase is empty.
    Empty,
    /// The phrase is not a whole run of pairs of hexadecimal digits.
    NotHex,
    /// The character encoding cannot represent one of the characters.
    Unrepresentable,
}

impl PatternError {
    /// A sentence naming what is wrong.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Empty => "Enter something to search for.",
            Self::NotHex => "Enter pairs of hexadecimal digits.",
            Self::Unrepresentable => "The character encoding cannot represent that text.",
        }
    }
}

/// The bytes a search phrase stands for.
///
/// # Errors
///
/// Returns why the phrase cannot be read.
pub fn pattern_bytes(
    settings: &FindSettings,
    encoding: CharEncoding,
) -> Result<Vec<u8>, PatternError> {
    if settings.pattern.is_empty() {
        return Err(PatternError::Empty);
    }
    let bytes = match settings.kind {
        PatternKind::Bytes => from_hex_text(&settings.pattern).ok_or(PatternError::NotHex)?,
        PatternKind::Text => encoding
            .encode(&settings.pattern)
            .ok_or(PatternError::Unrepresentable)?,
    };
    if bytes.is_empty() {
        return Err(PatternError::Empty);
    }
    Ok(bytes)
}

/// True when two bytes match under the case rule.
fn same_byte(left: u8, right: u8, match_case: bool) -> bool {
    if match_case {
        return left == right;
    }
    left.eq_ignore_ascii_case(&right)
}

/// True when `needle` sits at `at` in `haystack`.
fn matches_at(haystack: &[u8], at: usize, needle: &[u8], match_case: bool) -> bool {
    let Some(window) = haystack.get(at..at.saturating_add(needle.len())) else {
        return false;
    };
    window
        .iter()
        .zip(needle.iter())
        .all(|(found, wanted)| same_byte(*found, *wanted, match_case))
}

/// The first offset at or after `from` where `needle` occurs.
#[must_use]
pub fn find_forward(haystack: &[u8], needle: &[u8], from: u64, match_case: bool) -> Option<u64> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    let last = haystack.len() - needle.len();
    let start = usize::try_from(from).ok()?;
    (start..=last)
        .find(|at| matches_at(haystack, *at, needle, match_case))
        .map(|at| at as u64)
}

/// The last offset strictly before `before` where `needle` occurs.
#[must_use]
pub fn find_backward(haystack: &[u8], needle: &[u8], before: u64, match_case: bool) -> Option<u64> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    let last = haystack.len() - needle.len();
    let before = usize::try_from(before).unwrap_or(usize::MAX);
    let highest = before.min(last.saturating_add(1)).checked_sub(1)?;
    (0..=highest)
        .rev()
        .find(|at| matches_at(haystack, *at, needle, match_case))
        .map(|at| at as u64)
}

/// Where a search should start looking next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Forward from the caret.
    Forward,
    /// Backward from the caret.
    Backward,
}

/// What a search job posts back.
#[derive(Debug, PartialEq, Eq)]
pub enum FindMessage {
    /// The phrase was found at this offset of the searched side.
    Found(Side, u64),
    /// The phrase does not occur.
    NotFound,
    /// The run stopped before reaching an answer.
    Cancelled,
}

impl Terminal for FindMessage {
    fn is_terminal(&self) -> bool {
        true
    }

    fn cancelled() -> Self {
        FindMessage::Cancelled
    }

    fn panicked(_detail: String) -> Self {
        FindMessage::NotFound
    }
}

/// Scan one side on a worker thread.
///
/// The scan runs in steps so a raised flag is observed part way through a long
/// side rather than only at the end.
#[must_use]
pub fn spawn(
    side: Side,
    bytes: Arc<Vec<u8>>,
    needle: Vec<u8>,
    from: u64,
    direction: Direction,
    settings: FindSettings,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<FindMessage> {
    Job::spawn_notifying(
        move |emitter, cancel| {
            let found = scan(&bytes, &needle, from, direction, &settings, cancel);
            match found {
                Scan::Found(at) => emitter.send(FindMessage::Found(side, at)),
                Scan::Missing => emitter.send(FindMessage::NotFound),
                Scan::Stopped => return,
            };
        },
        notify,
    )
}

/// What a replace all scan posts back.
#[derive(Debug, PartialEq, Eq)]
pub enum FindAllMessage {
    /// Every place the phrase starts, in ascending order, without overlaps.
    Found(Side, Vec<u64>),
    /// The run stopped before reaching an answer.
    Cancelled,
}

impl Terminal for FindAllMessage {
    fn is_terminal(&self) -> bool {
        true
    }

    fn cancelled() -> Self {
        FindAllMessage::Cancelled
    }

    fn panicked(_detail: String) -> Self {
        FindAllMessage::Found(Side::Left, Vec::new())
    }
}

/// Every non-overlapping place `needle` starts in `bytes`, scanning forward.
///
/// Returns `None` when `cancel` is raised part way.
#[must_use]
pub fn find_every(
    bytes: &[u8],
    needle: &[u8],
    match_case: bool,
    cancel: &Cancel,
) -> Option<Vec<u64>> {
    let mut found = Vec::new();
    if needle.is_empty() || needle.len() > bytes.len() {
        return Some(found);
    }
    let last = bytes.len() - needle.len();
    let mut at = 0usize;
    let mut checked = 0usize;
    while at <= last {
        if matches_at(bytes, at, needle, match_case) {
            found.push(at as u64);
            at += needle.len();
        } else {
            at += 1;
        }
        checked += 1;
        if checked.is_multiple_of(1 << 20) && cancel.is_cancelled() {
            return None;
        }
    }
    Some(found)
}

/// Find every occurrence on one side on a worker thread, for a replace all.
#[must_use]
pub fn spawn_all(
    side: Side,
    bytes: Arc<Vec<u8>>,
    needle: Vec<u8>,
    match_case: bool,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<FindAllMessage> {
    Job::spawn_notifying(
        move |emitter, cancel| {
            if let Some(found) = find_every(&bytes, &needle, match_case, cancel) {
                emitter.send(FindAllMessage::Found(side, found));
            }
        },
        notify,
    )
}

enum Scan {
    Found(u64),
    Missing,
    Stopped,
}

fn scan(
    bytes: &[u8],
    needle: &[u8],
    from: u64,
    direction: Direction,
    settings: &FindSettings,
    cancel: &Cancel,
) -> Scan {
    let total = bytes.len() as u64;
    let ranges: [(u64, u64); 2] = match direction {
        Direction::Forward => [(from, total), (0, from.min(total))],
        Direction::Backward => [(0, from), (from, total)],
    };
    let passes = if settings.wrap { 2 } else { 1 };
    for (start, end) in ranges.iter().take(passes) {
        match step_scan(bytes, needle, *start, *end, direction, settings, cancel) {
            Scan::Missing => {}
            other => return other,
        }
    }
    Scan::Missing
}

/// Scan one range in bounded steps.
fn step_scan(
    bytes: &[u8],
    needle: &[u8],
    start: u64,
    end: u64,
    direction: Direction,
    settings: &FindSettings,
    cancel: &Cancel,
) -> Scan {
    if end <= start {
        return Scan::Missing;
    }
    // A phrase may straddle a step boundary, so each step reaches one phrase
    // length past its own end.
    let reach = needle.len() as u64;
    let mut at = match direction {
        Direction::Forward => start,
        Direction::Backward => end,
    };
    loop {
        if cancel.is_cancelled() {
            return Scan::Stopped;
        }
        match direction {
            Direction::Forward => {
                if at >= end {
                    return Scan::Missing;
                }
                let stop = at.saturating_add(SCAN_STEP).min(end);
                let window = window(bytes, at, stop.saturating_add(reach));
                if let Some(found) = find_forward(window, needle, 0, settings.match_case) {
                    let found = at.saturating_add(found);
                    if found < end {
                        return Scan::Found(found);
                    }
                }
                at = stop;
            }
            Direction::Backward => {
                if at <= start {
                    return Scan::Missing;
                }
                let from = at.saturating_sub(SCAN_STEP).max(start);
                let window = window(bytes, from, at.saturating_add(reach));
                if let Some(found) =
                    find_backward(window, needle, at.saturating_sub(from), settings.match_case)
                {
                    return Scan::Found(from.saturating_add(found));
                }
                at = from;
            }
        }
    }
}

fn window(bytes: &[u8], start: u64, end: u64) -> &[u8] {
    let Ok(start) = usize::try_from(start) else {
        return &[];
    };
    let end = usize::try_from(end).unwrap_or(usize::MAX).min(bytes.len());
    bytes.get(start..end.max(start)).unwrap_or_default()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{
        find_backward, find_forward, pattern_bytes, spawn, Direction, FindMessage, FindSettings,
        PatternError, PatternKind,
    };
    use crate::chars::CharEncoding;
    use crate::model::Side;
    use ca_ui::testing::wait_until;
    use std::sync::Arc;
    use std::time::Duration;

    fn settings(pattern: &str, kind: PatternKind) -> FindSettings {
        FindSettings {
            pattern: pattern.to_owned(),
            kind,
            match_case: true,
            wrap: false,
        }
    }

    #[test]
    fn a_phrase_reads_as_bytes_or_as_text() {
        assert_eq!(
            pattern_bytes(&settings("de ad", PatternKind::Bytes), CharEncoding::Ansi),
            Ok(vec![0xDE, 0xAD])
        );
        assert_eq!(
            pattern_bytes(&settings("Hi", PatternKind::Text), CharEncoding::Ansi),
            Ok(vec![b'H', b'i'])
        );
        assert_eq!(
            pattern_bytes(&settings("", PatternKind::Bytes), CharEncoding::Ansi),
            Err(PatternError::Empty)
        );
        assert_eq!(
            pattern_bytes(&settings("xyz", PatternKind::Bytes), CharEncoding::Ansi),
            Err(PatternError::NotHex)
        );
        assert_eq!(
            pattern_bytes(&settings("é", PatternKind::Text), CharEncoding::Ascii),
            Err(PatternError::Unrepresentable)
        );
        assert!(!PatternError::NotHex.message().is_empty());
    }

    #[test]
    fn a_scan_finds_the_nearest_occurrence_in_each_direction() {
        let bytes = b"one two one two one";
        assert_eq!(find_forward(bytes, b"one", 0, true), Some(0));
        assert_eq!(find_forward(bytes, b"one", 1, true), Some(8));
        assert_eq!(find_backward(bytes, b"one", 16, true), Some(8));
        assert_eq!(find_backward(bytes, b"one", 0, true), None);
        assert_eq!(find_forward(bytes, b"three", 0, true), None);
        assert_eq!(find_forward(bytes, b"", 0, true), None);
    }

    #[test]
    fn every_occurrence_is_found_without_overlaps() {
        let cancel = ca_ui::worker::Cancel::default();
        assert_eq!(
            super::find_every(b"aaaXaa", b"aa", true, &cancel),
            Some(vec![0, 4])
        );
        assert_eq!(
            super::find_every(b"AbAB", b"ab", false, &cancel),
            Some(vec![0, 2])
        );
        assert_eq!(
            super::find_every(b"a", b"ab", true, &cancel),
            Some(Vec::new())
        );
    }

    #[test]
    fn a_case_insensitive_scan_ignores_letter_case() {
        let bytes = b"Alpha BETA";
        assert_eq!(find_forward(bytes, b"beta", 0, true), None);
        assert_eq!(find_forward(bytes, b"beta", 0, false), Some(6));
    }

    fn run(from: u64, direction: Direction, wrap: bool) -> Vec<FindMessage> {
        let bytes: Arc<Vec<u8>> = Arc::new(b"aXbXc".to_vec());
        let mut job = spawn(
            Side::Left,
            bytes,
            b"X".to_vec(),
            from,
            direction,
            FindSettings {
                wrap,
                ..settings("58", PatternKind::Bytes)
            },
            Arc::new(|| {}),
        );
        let mut seen = Vec::new();
        assert!(wait_until(Duration::from_secs(10), || {
            seen.extend(job.drain());
            job.is_finished()
        }));
        seen
    }

    #[test]
    fn a_search_reports_where_it_landed() {
        assert_eq!(
            run(0, Direction::Forward, false),
            vec![FindMessage::Found(Side::Left, 1)]
        );
        assert_eq!(
            run(2, Direction::Forward, false),
            vec![FindMessage::Found(Side::Left, 3)]
        );
        assert_eq!(
            run(4, Direction::Backward, false),
            vec![FindMessage::Found(Side::Left, 3)]
        );
    }

    #[test]
    fn a_search_past_the_last_match_reports_nothing_unless_it_wraps() {
        assert_eq!(
            run(4, Direction::Forward, false),
            vec![FindMessage::NotFound]
        );
        assert_eq!(
            run(4, Direction::Forward, true),
            vec![FindMessage::Found(Side::Left, 1)]
        );
        assert_eq!(
            run(0, Direction::Backward, false),
            vec![FindMessage::NotFound]
        );
        assert_eq!(
            run(0, Direction::Backward, true),
            vec![FindMessage::Found(Side::Left, 3)]
        );
    }

    /// The scan runs in steps, so a phrase lying across a step boundary is the
    /// case a stepped scan loses.
    #[test]
    fn a_phrase_across_a_step_boundary_is_still_found() {
        let step = usize::try_from(super::SCAN_STEP).unwrap_or(0);
        let mut bytes = vec![0u8; step * 2];
        let at = step - 2;
        for (index, value) in [0xDEu8, 0xAD, 0xBE, 0xEF].iter().enumerate() {
            if let Some(slot) = bytes.get_mut(at + index) {
                *slot = *value;
            }
        }
        let needle = [0xDEu8, 0xAD, 0xBE, 0xEF];
        let cancel = ca_ui::worker::Cancel::new();
        let found = super::scan(
            &bytes,
            &needle,
            0,
            Direction::Forward,
            &FindSettings::default(),
            &cancel,
        );
        assert!(matches!(found, super::Scan::Found(offset) if offset == at as u64));
        let back = super::scan(
            &bytes,
            &needle,
            bytes.len() as u64,
            Direction::Backward,
            &FindSettings::default(),
            &cancel,
        );
        assert!(matches!(back, super::Scan::Found(offset) if offset == at as u64));
    }

    #[test]
    fn a_raised_flag_stops_a_scan() {
        let bytes = vec![0u8; usize::try_from(super::SCAN_STEP).unwrap_or(0) * 4];
        let cancel = ca_ui::worker::Cancel::new();
        cancel.cancel();
        let found = super::scan(
            &bytes,
            &[1u8],
            0,
            Direction::Forward,
            &FindSettings::default(),
            &cancel,
        );
        assert!(matches!(found, super::Scan::Stopped));
    }
}
