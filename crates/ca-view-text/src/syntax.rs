//! Syntax coloring for one pane.
//!
//! Three pieces cooperate. The format registry picks a grammar from the file
//! name. A [`StateCache`] holds the lexer state carried into every line, built
//! and repaired on a worker because a single unterminated block can make that
//! work reach the end of the file. The frame thread lexes only the lines the
//! viewport covers, and at most [`FRAME_LEX_BYTES`] of each, starting from the
//! carried state, and keeps their tokens until an edit or a scroll drops them.

use crate::editor::EditSpan;
use crate::lines;
use ca_grammar::{FileFormat, FormatRegistry, Lexer, LineState, StateCache, StyleSlot};
use ca_ui::worker::{Cancel, Emitter, Job, Terminal};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, OnceLock};

/// The most bytes of one line the frame thread lexes.
///
/// Lexing costs the length of the line, and a visible line is lexed on the
/// frame thread. Past this bound the rest of the line takes the pane's own
/// text color, so one visible line costs a bounded amount of lexing.
const FRAME_LEX_BYTES: usize = 16 * 1024;

/// Lines one slice of the worker's build covers.
///
/// The slice bound is what makes the build cancellable: the worker checks the
/// cancel flag between slices, so closing a tab does not wait for a whole file.
const BUILD_SLICE: usize = 8_192;

/// The stock formats, compiled once for the process.
fn registry() -> &'static FormatRegistry {
    static REGISTRY: OnceLock<FormatRegistry> = OnceLock::new();
    REGISTRY.get_or_init(ca_grammar::builtin::registry)
}

/// The stock format that claims `path`.
#[must_use]
pub fn format_for(path: &Path) -> &'static FileFormat {
    registry().lookup(&path.display().to_string())
}

/// The stock format named `name`.
///
/// A name no stock format carries yields nothing, so a session naming a format
/// this build does not ship falls back to the file name rather than refusing.
#[must_use]
pub fn format_named(name: &str) -> Option<&'static FileFormat> {
    registry().by_name(name)
}

/// One colored run of a line, in display columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// First column of the run.
    pub start: u32,
    /// One past the last column of the run.
    pub end: u32,
    /// The color role the run plays.
    pub slot: StyleSlot,
}

/// What the cache worker posts back.
#[derive(Debug)]
pub enum CacheMessage {
    /// The carried states are rebuilt up to the end of the text.
    Ready(Box<StateCache>),
    /// The build stopped before it finished.
    Cancelled,
}

impl Terminal for CacheMessage {
    fn is_terminal(&self) -> bool {
        true
    }

    fn cancelled() -> Self {
        CacheMessage::Cancelled
    }

    fn panicked(_detail: String) -> Self {
        CacheMessage::Cancelled
    }
}

/// The coloring state of one pane.
pub struct Highlighter {
    format: &'static FileFormat,
    /// Format the session pinned, which outranks the file name.
    pinned: Option<&'static FileFormat>,
    /// True when a named format claimed the file name, rather than the
    /// catch-all entry.
    claimed: bool,
    lexer: Option<Arc<Lexer>>,
    /// The lines the carried states describe. They come from the last finished
    /// comparison, so they lag the buffer by at most one comparison.
    lines: Arc<Vec<String>>,
    cache: Option<StateCache>,
    job: Option<Job<CacheMessage>>,
    /// First line whose carried state is in doubt, or `None` when none is.
    dirty_from: Option<usize>,
    /// First line an edit touched since the text was last handed over.
    ///
    /// The text a build runs over comes from the comparison, which lands after
    /// the edit. Re-lexing before it would describe the text the edit replaced.
    edited_from: Option<usize>,
    tokens: HashMap<u32, Arc<[Span]>>,
    revision: u64,
    cached_revision: u64,
}

impl std::fmt::Debug for Highlighter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Highlighter")
            .field("format", &self.format.name)
            .field("lines", &self.lines.len())
            .field("dirty_from", &self.dirty_from)
            .finish_non_exhaustive()
    }
}

impl Default for Highlighter {
    fn default() -> Self {
        Self::new(Path::new(""))
    }
}

impl Highlighter {
    /// A highlighter for the format `path` belongs to.
    #[must_use]
    pub fn new(path: &Path) -> Self {
        let format = format_for(path);
        Self {
            format,
            pinned: None,
            claimed: registry().index_of(&path.display().to_string()).is_some(),
            lexer: Lexer::new(&format.grammar).ok().map(Arc::new),
            lines: Arc::new(Vec::new()),
            cache: None,
            job: None,
            dirty_from: None,
            edited_from: None,
            tokens: HashMap::new(),
            revision: 0,
            cached_revision: 0,
        }
    }

    /// The format the file name selected.
    #[must_use]
    pub const fn format(&self) -> &'static FileFormat {
        self.format
    }

    /// Follow a new file name, dropping everything the old grammar produced.
    pub fn set_path(&mut self, path: &Path) {
        let pinned = self.pinned;
        let format = pinned.unwrap_or_else(|| format_for(path));
        if std::ptr::eq(format, self.format) {
            return;
        }
        let lines = Arc::clone(&self.lines);
        *self = Self::new(path);
        self.pinned = pinned;
        if let Some(pinned) = pinned {
            self.adopt(pinned);
        }
        self.set_lines(lines);
    }

    /// Pin the format the session named, or return to the file name when it
    /// names none.
    ///
    /// The pinned format decides the grammar and the importance classes, so a
    /// session comparing two files whose names claim different formats still
    /// reads both under one syntax.
    pub fn set_format_override(&mut self, name: Option<&str>, path: &Path) {
        let pinned = name.and_then(format_named);
        if pinned.map(std::ptr::from_ref) == self.pinned.map(std::ptr::from_ref) {
            return;
        }
        self.pinned = pinned;
        self.adopt(pinned.unwrap_or_else(|| format_for(path)));
    }

    /// Take a format and drop everything the previous grammar produced.
    fn adopt(&mut self, format: &'static FileFormat) {
        if std::ptr::eq(format, self.format) {
            return;
        }
        self.format = format;
        self.lexer = Lexer::new(&format.grammar).ok().map(Arc::new);
        self.clear();
        self.cache = None;
        self.dirty_from = Some(0);
        self.invalidate_tokens();
    }

    /// Take the text the carried states are built over.
    ///
    /// The states already follow the edits that produced this text, so only a
    /// first text asks for a build from the start. Rebuilding every time would
    /// undo the incremental repair an edit set up.
    pub fn set_lines(&mut self, lines: Arc<Vec<String>>) {
        let first = self.lines.is_empty();
        self.lines = lines;
        if first {
            self.dirty_from = Some(0);
        } else if let Some(at) = self.edited_from.take() {
            self.dirty_from = Some(self.dirty_from.map_or(at, |from| from.min(at)));
        }
        self.edited_from = None;
        self.invalidate_tokens();
    }

    /// A shared snapshot of the lines the syntax state describes.
    #[must_use]
    pub(crate) fn lines(&self) -> Arc<Vec<String>> {
        Arc::clone(&self.lines)
    }

    /// Account for an edit, moving the cached states under it.
    pub fn note_edit(&mut self, span: EditSpan) {
        let at = span.start_line as usize;
        if let Some(cache) = self.cache.as_mut() {
            cache.splice_lines(
                at,
                span.removed_lines as usize,
                span.inserted_lines as usize,
            );
        }
        self.dirty_from = Some(self.dirty_from.map_or(at, |from| from.min(at)));
        self.edited_from = Some(self.edited_from.map_or(at, |from| from.min(at)));
        self.invalidate_tokens();
    }

    /// Drop every cached token, which the next frame recomputes for the lines
    /// it shows.
    fn invalidate_tokens(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// True when the file name selected a named format.
    #[must_use]
    pub const fn is_claimed(&self) -> bool {
        self.claimed
    }

    /// Counts everything that makes previously produced spans stale.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// True while the carried states are being built or repaired.
    #[must_use]
    pub const fn is_building(&self) -> bool {
        self.job.is_some()
    }

    /// True once the carried states describe the whole text.
    #[must_use]
    pub const fn is_converged(&self) -> bool {
        self.cache.is_some()
            && self.dirty_from.is_none()
            && self.edited_from.is_none()
            && self.job.is_none()
    }

    /// Collect a finished build and start the next one.
    ///
    /// Only the hand-off happens here; the lexing itself is on the worker.
    pub fn poll(&mut self, notify: &Arc<dyn Fn() + Send + Sync>) {
        if let Some(job) = self.job.as_mut() {
            let messages = job.drain();
            let finished = job.is_finished();
            for message in messages {
                if let CacheMessage::Ready(cache) = message {
                    self.cache = Some(*cache);
                    self.invalidate_tokens();
                }
            }
            if finished {
                self.job = None;
            } else {
                return;
            }
        }
        if self.edited_from.is_some() {
            return;
        }
        let (Some(from), Some(lexer)) = (self.dirty_from, self.lexer.clone()) else {
            return;
        };
        self.dirty_from = None;
        let lines = Arc::clone(&self.lines);
        let cache = self.cache.take();
        let notify = Arc::clone(notify);
        self.job = Some(Job::spawn_notifying(
            move |emitter: &Emitter<CacheMessage>, cancel: &Cancel| {
                if let Some(cache) = build(&lexer, &lines, cache, from, cancel) {
                    emitter.send(CacheMessage::Ready(Box::new(cache)));
                } else {
                    emitter.send(CacheMessage::Cancelled);
                }
            },
            notify,
        ));
    }

    /// Stop any work in progress.
    pub fn clear(&mut self) {
        if let Some(job) = self.job.take() {
            job.cancel();
        }
    }

    /// The colored runs of one line, lexed from the carried state.
    ///
    /// `text` is the live buffer line, so a keystroke colors at once while the
    /// carried state behind it is still catching up.
    pub fn spans(&mut self, line: u32, text: &str) -> Option<Arc<[Span]>> {
        let lexer = self.lexer.clone()?;
        if self.cached_revision != self.revision {
            self.tokens.clear();
            self.cached_revision = self.revision;
        }
        if let Some(found) = self.tokens.get(&line) {
            return Some(Arc::clone(found));
        }
        let state = self
            .cache
            .as_ref()
            .map_or_else(LineState::start, |cache| cache.state_at(line as usize));
        let head = text.get(..text.floor_char_boundary(FRAME_LEX_BYTES))?;
        let output = lexer.lex_line(head, line as usize, state).ok()?;
        let mut spans: Vec<Span> = Vec::with_capacity(output.tokens.len());
        let mut columns = lines::Columns::new(text);
        for token in &output.tokens {
            let slot = token.item.map_or(StyleSlot::Plain, |index| {
                lexer
                    .element_of(index)
                    .map_or(StyleSlot::Plain, |name| self.format.styles.slot_for(name))
            });
            let start = columns.at(token.range.start);
            let end = columns.at(token.range.end);
            if end > start {
                spans.push(Span { start, end, slot });
            }
        }
        let spans: Arc<[Span]> = spans.into();
        self.tokens.insert(line, Arc::clone(&spans));
        Some(spans)
    }

    /// Drop the tokens of every line outside `wanted`, so the store follows the
    /// viewport rather than the file.
    pub fn retain_lines(&mut self, wanted: &[u32]) {
        self.tokens.retain(|line, _| wanted.contains(line));
    }
}

/// Build or repair the carried states, in slices, checking the cancel flag
/// between them.
fn build(
    lexer: &Lexer,
    lines: &[String],
    cache: Option<StateCache>,
    from: usize,
    cancel: &Cancel,
) -> Option<StateCache> {
    let mut cache = cache.unwrap_or_default();
    let mut done = from.min(lines.len());
    loop {
        if cancel.is_cancelled() {
            return None;
        }
        let bound = done.saturating_add(BUILD_SLICE).min(lines.len());
        let reached = cache.relex_from(lexer, &lines[..bound], done).ok()?;
        if bound >= lines.len() {
            return Some(cache);
        }
        // A slice that converged still leaves the lines after it undescribed,
        // so the next slice continues from its bound rather than from the
        // convergence point.
        done = bound.max(reached);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{format_for, Highlighter};
    use ca_grammar::StyleSlot;
    use std::path::Path;
    use std::sync::Arc;

    #[test]
    fn a_file_name_selects_its_format() {
        assert_eq!(format_for(Path::new("src/main.rs")).name, "Rust");
        assert_eq!(format_for(Path::new("a/b/tool.py")).name, "Python");
        for (path, expected) in [
            ("config.json", "JSON"),
            ("config.jsonc", "JSON"),
            ("events.jsonl", "JSON"),
            ("pom.xml", "XML"),
            ("schema.xsd", "XML"),
            ("settings.yaml", "YAML"),
            ("settings.yml", "YAML"),
            ("Cargo.toml", "TOML"),
            ("desktop.ini", "INI"),
        ] {
            assert_eq!(format_for(Path::new(path)).name, expected, "for {path}");
        }
        let fallback = format_for(Path::new("notes.unknown-suffix"));
        assert_ne!(fallback.name, "Rust");
        assert_eq!(fallback.name, format_for(Path::new("other.unknown")).name);
    }

    #[test]
    fn a_comment_run_takes_the_comment_slot() {
        let mut highlighter = Highlighter::new(Path::new("main.rs"));
        let line = "let x = 1; // note";
        let spans = highlighter.spans(0, line).unwrap();
        let comment = spans
            .iter()
            .find(|span| span.slot == StyleSlot::Comment)
            .expect("the line has a comment");
        assert_eq!(comment.start, 11);
        assert_eq!(comment.end, 18);
    }

    /// The frame thread lexes the head of a long line only, so the cost of one
    /// visible line has a ceiling. The rest of the line carries no syntax run.
    #[test]
    fn a_long_line_is_colored_in_its_head_only() {
        let mut highlighter = Highlighter::new(Path::new("main.c"));
        let line = "int x = 1; ".repeat(200_000 / 11);
        let spans = highlighter.spans(0, &line).unwrap();
        assert!(spans.iter().any(|span| span.slot == StyleSlot::Keyword));
        let reach = spans.last().map_or(0, |span| span.end) as usize;
        assert!(
            reach < line.len() / 4,
            "runs reach column {reach} of a line of {}",
            line.len()
        );
    }

    /// Tabs and characters of more than one byte move the columns of the runs
    /// after them as they move the text.
    #[test]
    fn runs_after_tabs_and_wide_characters_start_at_their_columns() {
        let mut highlighter = Highlighter::new(Path::new("main.c"));
        let line = "\t\u{e9}\u{e9} int x; // n\u{f6}te";
        let spans = highlighter.spans(0, line).unwrap();
        let keyword = spans
            .iter()
            .find(|span| span.slot == StyleSlot::Keyword)
            .unwrap();
        assert_eq!((keyword.start, keyword.end), (11, 14));
        let comment = spans
            .iter()
            .find(|span| span.slot == StyleSlot::Comment)
            .unwrap();
        assert_eq!((comment.start, comment.end), (18, 25));
    }

    #[test]
    fn a_line_inside_a_block_comment_needs_the_carried_state() {
        let lines = Arc::new(vec![
            "/* open".to_owned(),
            "still inside".to_owned(),
            "*/".to_owned(),
        ]);
        let mut highlighter = Highlighter::new(Path::new("main.rs"));
        highlighter.set_lines(Arc::clone(&lines));
        // Without the carried state the second line reads as plain text.
        let spans = highlighter.spans(1, &lines[1]).unwrap();
        assert!(spans.iter().all(|span| span.slot != StyleSlot::Comment));
    }
}
