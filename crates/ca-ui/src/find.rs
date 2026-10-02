//! Find, replace, go to line and bookmarks for the active pane.
//!
//! The search itself belongs to the text crate. What lives here is the panel's
//! state, the translation of its options into a search, and what the caret and
//! the selection do with a result. A pattern that the engine gives up on is
//! reported as its own outcome rather than as "not found", because matches past
//! that point are unknown rather than absent.

use crate::editor::{Caret, Pane};
use crate::worker::{Job, Terminal};
use ca_text::{EditSnapshot, SearchError, SearchOptions, SearchResults, Searcher, TextBuffer};
use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::Arc;

/// How many characters a buffer may hold before a whole-buffer search is moved
/// off the frame thread.
pub const WORKER_THRESHOLD: usize = 256 * 1024;

/// How many bookmarks a comparison carries.
pub const BOOKMARK_SLOTS: u8 = 10;

/// The Find panel's settings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct FindSettings {
    /// The text or pattern to look for.
    pub pattern: String,
    /// The text to put in its place.
    pub replacement: String,
    /// Read the pattern as a regular expression.
    pub regex: bool,
    /// Match only text with the same capitalization.
    pub match_case: bool,
    /// Reject a match inside a longer word.
    pub whole_words: bool,
    /// Continue from the other end of the file.
    pub wrap: bool,
    /// Search only inside the selection.
    pub selection_only: bool,
}

impl FindSettings {
    /// The search options these settings describe.
    #[must_use]
    pub fn to_search(&self, backwards: bool, scope: Option<Range<usize>>) -> SearchOptions {
        SearchOptions {
            regex: self.regex,
            match_case: self.match_case,
            whole_words: self.whole_words,
            wrap: self.wrap,
            backwards,
            scope: if self.selection_only { scope } else { None },
        }
    }

    /// A compiled searcher, or why the pattern cannot be used.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError`] when the pattern does not compile.
    pub fn searcher(
        &self,
        backwards: bool,
        scope: Option<Range<usize>>,
    ) -> Result<Searcher, SearchError> {
        Searcher::new(&self.pattern, self.to_search(backwards, scope))
    }
}

/// What a find attempt produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FindOutcome {
    /// The caret moved to a match.
    Found,
    /// The search covered its whole range without a match.
    NotFound,
    /// The pattern cannot be used.
    BadPattern(String),
    /// The engine gave up, so some matches are unknown.
    LimitExceeded,
}

impl FindOutcome {
    /// The sentence the status line shows, or `None` when there is nothing to
    /// report.
    #[must_use]
    pub fn message(&self) -> Option<String> {
        match self {
            FindOutcome::Found => None,
            FindOutcome::NotFound => Some("No match found.".to_owned()),
            FindOutcome::BadPattern(reason) => Some(format!("The pattern is not usable: {reason}")),
            FindOutcome::LimitExceeded => Some(
                "The pattern is too costly to match on this text, so some matches are missing."
                    .to_owned(),
            ),
        }
    }
}

/// The selection a find should search inside, when it is told to.
#[must_use]
pub fn selection_scope(pane: &Pane) -> Option<Range<usize>> {
    let (start, end) = pane.selection()?;
    Some(pane.char_of(start)..pane.char_of(end))
}

/// Move the caret to the next match and select it.
pub fn find_next(pane: &mut Pane, settings: &FindSettings, backwards: bool) -> FindOutcome {
    if settings.pattern.is_empty() {
        return FindOutcome::NotFound;
    }
    let scope = selection_scope(pane);
    let searcher = match settings.searcher(backwards, scope.clone()) {
        Ok(searcher) => searcher,
        Err(reason) => return FindOutcome::BadPattern(reason.to_string()),
    };
    let from = match (settings.selection_only, &scope) {
        // The selection is both the range and the caret, so a search inside it
        // starts at its edge rather than where the caret happens to sit.
        (true, Some(range)) => {
            if backwards {
                range.end
            } else {
                range.start
            }
        }
        _ => start_of_search(pane, backwards),
    };
    match searcher.find_from(pane.buffer(), from) {
        Err(SearchError::BacktrackLimit) => FindOutcome::LimitExceeded,
        Err(reason) => FindOutcome::BadPattern(reason.to_string()),
        Ok(None) => FindOutcome::NotFound,
        Ok(Some(found)) => {
            let start = pane.caret_of(found.range.start);
            let end = pane.caret_of(found.range.end);
            pane.place(start, false);
            pane.place(end, true);
            FindOutcome::Found
        }
    }
}

/// Where the next search starts, so repeating it walks forward.
fn start_of_search(pane: &Pane, backwards: bool) -> usize {
    match pane.selection() {
        Some((start, end)) => {
            if backwards {
                pane.char_of(start)
            } else {
                pane.char_of(end)
            }
        }
        None => pane.char_of(pane.caret()),
    }
}

/// Replace the selection when it is already a match, then find the next one.
pub fn replace_next(pane: &mut Pane, settings: &FindSettings) -> FindOutcome {
    if pane.is_read_only() {
        return FindOutcome::NotFound;
    }
    let searcher = match settings.searcher(false, selection_scope(pane)) {
        Ok(searcher) => searcher,
        Err(reason) => return FindOutcome::BadPattern(reason.to_string()),
    };
    let from = pane.char_of(pane.caret());
    match searcher.replace_next(pane.buffer_mut(), from, &settings.replacement) {
        Err(SearchError::BacktrackLimit) => FindOutcome::LimitExceeded,
        Err(reason) => FindOutcome::BadPattern(reason.to_string()),
        Ok(None) => FindOutcome::NotFound,
        Ok(Some(_)) => FindOutcome::Found,
    }
}

/// Replace every match as one undo group, reporting how many.
///
/// # Errors
///
/// Returns the outcome to report when the pattern cannot be used or the engine
/// gave up, in which case the buffer is untouched.
pub fn replace_all(pane: &mut Pane, settings: &FindSettings) -> Result<usize, FindOutcome> {
    if pane.is_read_only() || settings.pattern.is_empty() {
        return Ok(0);
    }
    let searcher = match settings.searcher(false, selection_scope(pane)) {
        Ok(searcher) => searcher,
        Err(reason) => return Err(FindOutcome::BadPattern(reason.to_string())),
    };
    match searcher.replace_all(pane.buffer_mut(), &settings.replacement) {
        Ok(count) => Ok(count),
        Err(SearchError::BacktrackLimit) => Err(FindOutcome::LimitExceeded),
        Err(reason) => Err(FindOutcome::BadPattern(reason.to_string())),
    }
}

/// True when a whole-buffer search over this buffer belongs on a worker.
#[must_use]
pub fn wants_worker(buffer: &TextBuffer) -> bool {
    buffer.len_chars() > WORKER_THRESHOLD
}

/// What a whole-buffer search posts back.
#[derive(Debug)]
pub enum SearchMessage {
    /// The search finished.
    Done(Box<SearchResults>),
    /// The pattern could not be used.
    Failed(String),
    /// The search stopped before finishing.
    Cancelled,
}

impl Terminal for SearchMessage {
    fn is_terminal(&self) -> bool {
        true
    }

    fn cancelled() -> Self {
        SearchMessage::Cancelled
    }

    fn panicked(detail: String) -> Self {
        SearchMessage::Failed(detail)
    }
}

/// Search a whole buffer on a worker thread.
#[must_use]
pub fn spawn_find_all(
    buffer: TextBuffer,
    settings: FindSettings,
    scope: Option<Range<usize>>,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<SearchMessage> {
    Job::spawn_notifying(
        move |emitter, cancel| {
            let message = match settings.searcher(false, scope) {
                Err(reason) => SearchMessage::Failed(reason.to_string()),
                Ok(searcher) => {
                    match searcher.find_all_with_cancel(&buffer, &|| cancel.is_cancelled()) {
                        Ok(results) => SearchMessage::Done(Box::new(results)),
                        Err(SearchError::Cancelled) => SearchMessage::Cancelled,
                        Err(reason) => SearchMessage::Failed(reason.to_string()),
                    }
                }
            };
            emitter.send(message);
        },
        notify,
    )
}

/// An operation prepared against the current pane on a worker.
#[derive(Clone, Copy)]
pub enum FindOperation {
    /// Select the next match in the requested direction.
    Next(bool),
    /// Replace one match.
    Replace,
    /// Replace all matches as one undo step.
    ReplaceAll,
}

#[derive(PartialEq, Eq)]
struct SearchStamp {
    revision: u64,
    caret: Caret,
    anchor: Caret,
    read_only: bool,
}

impl SearchStamp {
    fn of(pane: &Pane) -> Self {
        Self {
            revision: pane.buffer().revision(),
            caret: pane.caret(),
            anchor: pane.anchor(),
            read_only: pane.is_read_only(),
        }
    }
}

struct PreparedSearch {
    selection: Option<Range<usize>>,
    edit: Option<EditSnapshot>,
    count: usize,
    outcome: FindOutcome,
}

enum PreparedMessage {
    Done(PreparedSearch),
    Failed(String),
    Cancelled,
}

impl Terminal for PreparedMessage {
    fn is_terminal(&self) -> bool {
        true
    }
    fn cancelled() -> Self {
        Self::Cancelled
    }
    fn panicked(detail: String) -> Self {
        Self::Failed(detail)
    }
}

struct PendingSearch {
    stamp: SearchStamp,
    settings: FindSettings,
    operation: FindOperation,
    job: Job<PreparedMessage>,
}

/// The changes a completed search made to the pane.
pub struct FindCompletion {
    /// Text for the view's status line.
    pub message: Option<String>,
    /// A find selected a match.
    pub selected: bool,
    /// A replacement changed the buffer and needs the view's edit notification.
    pub edited: bool,
}

/// One cancellable search for a view. Starting another request cancels the old
/// one. Results belong to the captured content, caret, selection and settings;
/// changing any of them discards the result instead of applying stale edits.
#[derive(Default)]
pub struct FindTask {
    pending: Option<PendingSearch>,
}

fn retire<T: Send + 'static>(value: T) {
    std::thread::spawn(move || drop(value));
}

impl Drop for FindTask {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl FindTask {
    /// True while a request awaits completion.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.pending.is_some()
    }

    /// Cancel the request and dispose of queued text on a worker.
    pub fn cancel(&mut self) {
        if let Some(pending) = self.pending.take() {
            pending.job.cancel();
            retire(pending);
        }
    }

    /// Queue an operation without copying the document or its undo history.
    pub fn start(
        &mut self,
        pane: &Pane,
        settings: &FindSettings,
        operation: FindOperation,
        notify: Arc<dyn Fn() + Send + Sync>,
    ) {
        self.cancel();
        let stamp = SearchStamp::of(pane);
        let worker_stamp = SearchStamp::of(pane);
        let snapshot = pane.buffer().edit_snapshot();
        let worker_settings = settings.clone();
        let job = Job::spawn_notifying(
            move |emitter, cancel| {
                let result = prepare_search(
                    snapshot,
                    &worker_stamp,
                    &worker_settings,
                    operation,
                    &|| cancel.is_cancelled(),
                );
                let message = match result {
                    Ok(result) if !cancel.is_cancelled() => PreparedMessage::Done(result),
                    Ok(_) | Err(SearchError::Cancelled) => PreparedMessage::Cancelled,
                    Err(SearchError::BacktrackLimit) => PreparedMessage::Done(PreparedSearch {
                        selection: None,
                        edit: None,
                        count: 0,
                        outcome: FindOutcome::LimitExceeded,
                    }),
                    Err(reason) => PreparedMessage::Failed(reason.to_string()),
                };
                emitter.send(message);
            },
            notify,
        );
        self.pending = Some(PendingSearch {
            stamp,
            settings: settings.clone(),
            operation,
            job,
        });
    }

    /// Poll without waiting and publish only a result still belonging to this
    /// pane. The caller cancels when the view's target pane or load state changes.
    pub fn poll(&mut self, pane: &mut Pane, settings: &FindSettings) -> Option<FindCompletion> {
        let pending = self.pending.as_mut()?;
        if pending.stamp != SearchStamp::of(pane) || pending.settings != *settings {
            self.cancel();
            return Some(FindCompletion {
                message: Some(
                    "Search cancelled because the text, selection or settings changed.".to_owned(),
                ),
                selected: false,
                edited: false,
            });
        }
        let message = pending.job.drain().pop()?;
        let operation = pending.operation;
        self.cancel();
        match message {
            PreparedMessage::Cancelled => Some(FindCompletion {
                message: Some("Search cancelled.".to_owned()),
                selected: false,
                edited: false,
            }),
            PreparedMessage::Failed(reason) => Some(FindCompletion {
                message: FindOutcome::BadPattern(reason).message(),
                selected: false,
                edited: false,
            }),
            PreparedMessage::Done(mut result) => {
                let edited = if let Some(edit) = result.edit.take() {
                    match pane.buffer_mut().apply_snapshot(edit) {
                        Ok(old) => {
                            retire(old);
                            let anchor = pane.anchor();
                            let caret = pane.caret();
                            pane.place(anchor, false);
                            pane.place(caret, true);
                            true
                        }
                        Err(stale) => {
                            retire(stale);
                            false
                        }
                    }
                } else {
                    false
                };
                let selected = if let Some(range) = result.selection {
                    let start = pane.caret_of(range.start);
                    let end = pane.caret_of(range.end);
                    pane.place(start, false);
                    pane.place(end, true);
                    true
                } else {
                    false
                };
                Some(FindCompletion {
                    message: if matches!(operation, FindOperation::ReplaceAll)
                        && matches!(result.outcome, FindOutcome::Found | FindOutcome::NotFound)
                    {
                        Some(format!("{} replaced.", result.count))
                    } else {
                        result.outcome.message()
                    },
                    selected,
                    edited,
                })
            }
        }
    }
}

fn prepare_search(
    mut snapshot: EditSnapshot,
    stamp: &SearchStamp,
    settings: &FindSettings,
    operation: FindOperation,
    cancelled: &dyn Fn() -> bool,
) -> Result<PreparedSearch, SearchError> {
    let mut result = PreparedSearch {
        selection: None,
        edit: None,
        count: 0,
        outcome: FindOutcome::NotFound,
    };
    if settings.pattern.is_empty()
        || (stamp.read_only && !matches!(operation, FindOperation::Next(_)))
    {
        return Ok(result);
    }
    let position = Pane::new(TextBuffer::from_rope(snapshot.buffer().rope().clone()));
    let caret = position.char_of(stamp.caret);
    let anchor = position.char_of(stamp.anchor);
    let scope = (stamp.caret != stamp.anchor).then(|| caret.min(anchor)..caret.max(anchor));
    drop(position);
    let backwards = matches!(operation, FindOperation::Next(true));
    let searcher = settings.searcher(backwards, scope.clone())?;
    if matches!(operation, FindOperation::ReplaceAll) {
        result.count = searcher.replace_all_with_cancel(
            snapshot.buffer_mut(),
            &settings.replacement,
            cancelled,
        )?;
    } else {
        let from = if matches!(operation, FindOperation::Replace) {
            caret
        } else if settings.selection_only && scope.is_some() {
            if backwards {
                caret.max(anchor)
            } else {
                caret.min(anchor)
            }
        } else if backwards {
            caret.min(anchor)
        } else {
            caret.max(anchor)
        };
        if let Some(found) = searcher.find_from_with_cancel(snapshot.buffer(), from, cancelled)? {
            if matches!(operation, FindOperation::Replace) {
                let replacement = searcher.expand(&found, &settings.replacement);
                snapshot.buffer_mut().replace(found.range, &replacement);
                result.count = 1;
            } else {
                result.selection = Some(found.range);
                result.outcome = FindOutcome::Found;
            }
        }
    }
    if result.count > 0 {
        result.edit = Some(snapshot);
        result.outcome = FindOutcome::Found;
    }
    Ok(result)
}

/// Place the caret at a one based line and column.
pub fn go_to(pane: &mut Pane, line: u32, column: u32) {
    let line = line.saturating_sub(1);
    pane.place_at_column(line, column.saturating_sub(1), false);
}

/// The numbered markers of one comparison.
#[derive(Debug, Clone, Default)]
pub struct Bookmarks {
    slots: BTreeMap<u8, usize>,
}

impl Bookmarks {
    /// No bookmarks.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Place or remove the numbered marker on `row`.
    ///
    /// Setting a slot that already points at `row` clears it, so the same
    /// keystroke turns a marker off again.
    pub fn toggle(&mut self, slot: u8, row: usize) {
        if slot >= BOOKMARK_SLOTS {
            return;
        }
        if self.slots.get(&slot) == Some(&row) {
            self.slots.remove(&slot);
        } else {
            self.slots.insert(slot, row);
        }
    }

    /// The row a numbered marker sits on.
    #[must_use]
    pub fn row_of(&self, slot: u8) -> Option<usize> {
        self.slots.get(&slot).copied()
    }

    /// True when any row carries a marker.
    #[must_use]
    pub fn marks(&self, row: usize) -> bool {
        self.slots.values().any(|marked| *marked == row)
    }

    /// The first marked row after `row`.
    #[must_use]
    pub fn next(&self, row: usize) -> Option<usize> {
        self.slots.values().filter(|at| **at > row).min().copied()
    }

    /// The last marked row before `row`.
    #[must_use]
    pub fn previous(&self, row: usize) -> Option<usize> {
        self.slots.values().filter(|at| **at < row).max().copied()
    }

    /// Remove every marker.
    pub fn clear(&mut self) {
        self.slots.clear();
    }

    /// How many markers are set.
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// True when no marker is set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Move every marker to follow an edit that changed the row count.
    pub fn shift(&mut self, at: usize, removed: usize, inserted: usize) {
        let removed_end = at.saturating_add(removed);
        self.slots.retain(|_, row| {
            if *row < at {
                return true;
            }
            if *row < removed_end {
                return false;
            }
            *row = row.saturating_sub(removed).saturating_add(inserted);
            true
        });
    }
}

/// What the user asked the find or go to strip for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelRequest {
    /// Find the next match.
    Next,
    /// Find the previous match.
    Previous,
    /// Replace the current match and find the next one.
    Replace,
    /// Replace every match.
    ReplaceAll,
    /// Move the caret to this one based line.
    GoTo(u32),
    /// The go to field holds no line number.
    NoLine,
}

/// Which optional controls the find strip draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct FindControls {
    /// Draw the whole words switch.
    pub whole_words: bool,
    /// Draw the regular expression switch.
    pub regex: bool,
    /// Draw the selection only switch.
    pub selection_only: bool,
    /// Enter in the search field asks for the next match and keeps the focus.
    pub enter_searches: bool,
}

impl FindControls {
    /// Every control of a text search.
    pub const ALL: Self = Self {
        whole_words: true,
        regex: true,
        selection_only: true,
        enter_searches: false,
    };
}

/// The sentence the status line shows when the go to field holds no number.
pub const NO_LINE: &str = "Enter a line number.";

/// The find, replace and go to strips of one editable view.
///
/// The strips own the settings and their open state. The view that draws them
/// owns the pane a request acts on, so a strip returns what was asked for
/// rather than acting on a pane itself.
#[derive(Debug, Clone, Default)]
pub struct FindPanel {
    /// What the find strip searches for.
    pub settings: FindSettings,
    find_open: bool,
    replace_open: bool,
    goto_open: bool,
    goto_text: String,
    /// The strip whose field asks for the keyboard when it is next drawn,
    /// set when a strip opens. A view whose panes take every key while no
    /// widget holds the keyboard would otherwise take what is typed into a
    /// strip just opened.
    focus: Option<Strip>,
}

/// Select the whole text of the field `id`, so what is typed next replaces it.
fn select_all(ctx: &egui::Context, id: egui::Id, text: &str) {
    let mut state = egui::widgets::text_edit::TextEditState::load(ctx, id).unwrap_or_default();
    let end = egui::text::CCursor::new(text.chars().count());
    state
        .cursor
        .set_char_range(Some(egui::text::CCursorRange::two(
            egui::text::CCursor::new(0),
            end,
        )));
    state.store(ctx, id);
}

/// One of the strips a [`FindPanel`] draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Strip {
    Find,
    GoTo,
}

impl FindPanel {
    /// Closed strips that search under `settings`.
    #[must_use]
    pub fn with_settings(settings: FindSettings) -> Self {
        Self {
            settings,
            ..Self::default()
        }
    }

    /// Open the find strip without the replace row, with the search field
    /// holding the keyboard.
    pub fn open_find(&mut self) {
        self.find_open = true;
        self.replace_open = false;
        self.focus = Some(Strip::Find);
    }

    /// Open the find strip with the replace row, with the search field holding
    /// the keyboard.
    pub fn open_replace(&mut self) {
        self.find_open = true;
        self.replace_open = true;
        self.focus = Some(Strip::Find);
    }

    /// Open the go to strip, with its field holding the keyboard.
    pub fn open_go_to(&mut self) {
        self.goto_open = true;
        self.focus = Some(Strip::GoTo);
    }

    /// Close the find strip and the go to strip.
    pub fn close(&mut self) {
        self.find_open = false;
        self.replace_open = false;
        self.goto_open = false;
    }

    /// True while the find strip is on screen.
    #[must_use]
    pub const fn is_find_open(&self) -> bool {
        self.find_open
    }

    /// True while the replace row is on screen.
    #[must_use]
    pub const fn is_replace_open(&self) -> bool {
        self.find_open && self.replace_open
    }

    /// True while the go to strip is on screen.
    #[must_use]
    pub const fn is_go_to_open(&self) -> bool {
        self.goto_open
    }

    /// Draw the find strip, and the replace row when it is open.
    pub fn show_find(&mut self, ui: &mut egui::Ui) -> Option<PanelRequest> {
        self.show_find_with(ui, FindControls::ALL, |_| {})
    }

    /// Draw the find strip with only the controls a view can honor.
    ///
    /// `extra` draws the view's own controls after the search field, such as a
    /// choice of how the phrase is read. A control a view cannot honor is not
    /// drawn, so a setting never appears to take effect when it does not.
    pub fn show_find_with(
        &mut self,
        ui: &mut egui::Ui,
        controls: FindControls,
        extra: impl FnOnce(&mut egui::Ui),
    ) -> Option<PanelRequest> {
        if !self.find_open {
            return None;
        }
        let mut request = None;
        let mut close = false;
        ui.horizontal_wrapped(|ui| {
            ui.label("Find");
            let field =
                ui.add(egui::TextEdit::singleline(&mut self.settings.pattern).desired_width(180.0));
            if self.focus == Some(Strip::Find) {
                self.focus = None;
                field.request_focus();
                select_all(ui.ctx(), field.id, &self.settings.pattern);
            }
            if controls.enter_searches
                && field.lost_focus()
                && ui.input(|input| input.key_pressed(egui::Key::Enter))
            {
                request = Some(PanelRequest::Next);
                field.request_focus();
            }
            extra(ui);
            if crate::widgets::inline_button(ui, "Next", crate::icons::Icon::FindNext).clicked() {
                request = Some(PanelRequest::Next);
            }
            if crate::widgets::inline_button(ui, "Previous", crate::icons::Icon::FindPrevious)
                .clicked()
            {
                request = Some(PanelRequest::Previous);
            }
            ui.checkbox(&mut self.settings.match_case, "Match case");
            if controls.whole_words {
                ui.checkbox(&mut self.settings.whole_words, "Whole words");
            }
            if controls.regex {
                ui.checkbox(&mut self.settings.regex, "Regular expression");
            }
            ui.checkbox(&mut self.settings.wrap, "Wrap");
            if controls.selection_only {
                ui.checkbox(&mut self.settings.selection_only, "Selection only");
            }
            if crate::widgets::inline_button(ui, "Close", crate::icons::Icon::Close).clicked() {
                close = true;
            }
        });
        if self.replace_open {
            ui.horizontal_wrapped(|ui| {
                ui.label("Replace with");
                ui.add(
                    egui::TextEdit::singleline(&mut self.settings.replacement).desired_width(180.0),
                );
                if crate::widgets::inline_button(ui, "Replace", crate::icons::Icon::Replace)
                    .clicked()
                {
                    request = Some(PanelRequest::Replace);
                }
                if ui.button("Replace All").clicked() {
                    request = Some(PanelRequest::ReplaceAll);
                }
            });
        }
        if close {
            self.find_open = false;
            self.replace_open = false;
        }
        request
    }

    /// Draw the go to strip.
    ///
    /// A usable line number closes the strip; anything else leaves it open.
    pub fn show_go_to(&mut self, ui: &mut egui::Ui) -> Option<PanelRequest> {
        if !self.goto_open {
            return None;
        }
        let mut go = false;
        let mut close = false;
        ui.horizontal_wrapped(|ui| {
            ui.label("Go to line");
            let field = ui.add(egui::TextEdit::singleline(&mut self.goto_text).desired_width(80.0));
            if self.focus == Some(Strip::GoTo) {
                self.focus = None;
                field.request_focus();
                select_all(ui.ctx(), field.id, &self.goto_text);
            }
            if field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                go = true;
            }
            if crate::widgets::inline_button(ui, "Go", crate::icons::Icon::GoTo).clicked() {
                go = true;
            }
            if crate::widgets::inline_button(ui, "Close", crate::icons::Icon::Close).clicked() {
                close = true;
            }
        });
        let request = go.then(|| self.go_to_request());
        if close || matches!(request, Some(PanelRequest::GoTo(_))) {
            self.goto_open = false;
        }
        request
    }

    /// Set the go to field, as typing into it does.
    pub fn set_go_to_text(&mut self, text: &str) {
        text.clone_into(&mut self.goto_text);
    }

    /// What the go to field asks for.
    #[must_use]
    pub fn go_to_request(&self) -> PanelRequest {
        self.goto_text
            .trim()
            .parse::<u32>()
            .map_or(PanelRequest::NoLine, PanelRequest::GoTo)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{
        find_next, go_to, replace_all, replace_next, wants_worker, Bookmarks, FindOutcome,
        FindPanel, FindSettings, PanelRequest,
    };
    use crate::editor::{Caret, Pane};

    #[test]
    fn the_panel_opens_find_and_replace_and_closes_both() {
        let mut panel = FindPanel::default();
        panel.open_find();
        assert!(panel.is_find_open());
        assert!(!panel.is_replace_open());
        panel.open_replace();
        assert!(panel.is_replace_open());
        panel.open_go_to();
        assert!(panel.is_go_to_open());
        panel.close();
        assert!(!panel.is_find_open() && !panel.is_go_to_open());
    }

    #[test]
    fn the_go_to_field_names_a_line_or_nothing() {
        let mut panel = FindPanel::default();
        panel.set_go_to_text(" 12 ");
        assert_eq!(panel.go_to_request(), PanelRequest::GoTo(12));
        panel.set_go_to_text("twelve");
        assert_eq!(panel.go_to_request(), PanelRequest::NoLine);
    }

    #[test]
    fn a_limited_strip_draws_the_view_controls_only_while_open() {
        let ctx = egui::Context::default();
        let mut panel = FindPanel::default();
        let controls = super::FindControls {
            whole_words: false,
            regex: false,
            selection_only: false,
            enter_searches: true,
        };
        let mut drawn = 0;
        let mut run = |panel: &mut FindPanel| {
            let _ = ctx.run(crate::testing::raw_input(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let _ = panel.show_find_with(ui, controls, |_| drawn += 1);
                });
            });
        };
        run(&mut panel);
        panel.open_find();
        run(&mut panel);
        assert_eq!(drawn, 1);
    }

    #[test]
    fn a_closed_panel_draws_nothing_and_asks_for_nothing() {
        let ctx = egui::Context::default();
        let mut panel = FindPanel::default();
        let mut asked = Some(PanelRequest::Next);
        let _ = ctx.run(crate::testing::raw_input(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                asked = panel.show_find(ui).or(panel.show_go_to(ui));
            });
        });
        assert_eq!(asked, None);
    }

    fn settings(pattern: &str) -> FindSettings {
        FindSettings {
            pattern: pattern.to_owned(),
            wrap: true,
            ..FindSettings::default()
        }
    }

    fn ready_task(
        pane: &Pane,
        settings: &FindSettings,
        operation: super::FindOperation,
    ) -> super::FindTask {
        let (send, receive) = std::sync::mpsc::channel();
        let mut task = super::FindTask::default();
        task.start(
            pane,
            settings,
            operation,
            std::sync::Arc::new(move || {
                let _ = send.send(std::thread::current().id());
            }),
        );
        let thread = receive
            .recv_timeout(std::time::Duration::from_secs(20))
            .unwrap();
        assert_ne!(thread, std::thread::current().id());
        task
    }

    #[test]
    fn worker_find_preserves_the_pane_until_polled_and_honors_direction_and_scope() {
        let mut pane = Pane::from_text("one two one\n");
        let settings = settings("one");
        let mut task = ready_task(&pane, &settings, super::FindOperation::Next(false));
        assert!(pane.selection().is_none());
        assert!(task.poll(&mut pane, &settings).unwrap().selected);
        assert_eq!(pane.caret(), Caret::new(0, 3));
        let mut task = ready_task(&pane, &settings, super::FindOperation::Next(false));
        assert!(task.poll(&mut pane, &settings).unwrap().selected);
        assert_eq!(pane.caret(), Caret::new(0, 11));
        let mut task = ready_task(&pane, &settings, super::FindOperation::Next(true));
        assert!(task.poll(&mut pane, &settings).unwrap().selected);
        assert_eq!(pane.caret(), Caret::new(0, 3));
        pane.place(Caret::new(0, 8), false);
        pane.place(Caret::new(0, 11), true);
        let mut scoped = settings.clone();
        scoped.selection_only = true;
        let mut task = ready_task(&pane, &scoped, super::FindOperation::Next(false));
        assert!(task.poll(&mut pane, &scoped).unwrap().selected);
        assert_eq!(pane.caret(), Caret::new(0, 11));
    }

    #[test]
    fn worker_find_clamps_a_caret_left_past_the_line_by_a_replacement() {
        let mut pane = Pane::from_text("catcat\ncat\n");
        pane.place(Caret::new(0, 6), false);
        let mut settings = settings("catcat");
        settings.wrap = false;
        settings.replacement = "x".to_owned();
        let mut task = ready_task(&pane, &settings, super::FindOperation::ReplaceAll);
        assert!(task.poll(&mut pane, &settings).unwrap().edited);
        settings.pattern = "cat".to_owned();
        let mut task = ready_task(&pane, &settings, super::FindOperation::Next(false));
        assert!(task.poll(&mut pane, &settings).unwrap().selected);
        assert_eq!(pane.selection().unwrap().0, Caret::new(1, 0));
    }

    #[test]
    fn worker_find_clamps_external_edits_without_expanding_an_empty_selection_scope() {
        for selection_only in [false, true] {
            let mut pane = Pane::from_text("one\ntwo\n");
            pane.place(Caret::new(0, 3), selection_only);
            pane.buffer_mut().replace(0..3, "");
            let mut settings = settings("two");
            settings.wrap = false;
            settings.selection_only = selection_only;
            let mut task = ready_task(&pane, &settings, super::FindOperation::Next(false));
            assert_eq!(
                task.poll(&mut pane, &settings).unwrap().selected,
                !selection_only
            );
        }
    }

    #[test]
    fn worker_replacements_preserve_prior_undo_and_one_group_for_all_matches() {
        for operation in [
            super::FindOperation::Replace,
            super::FindOperation::ReplaceAll,
        ] {
            let mut pane = Pane::from_text("cat cat\n");
            pane.buffer_mut().insert(0, "prefix ");
            let _ = pane.take_changes();
            let mut settings = settings("cat");
            settings.replacement = "cow".to_owned();
            let mut task = ready_task(&pane, &settings, operation);
            assert_eq!(pane.buffer().text(), "prefix cat cat\n");
            assert!(task.poll(&mut pane, &settings).unwrap().edited);
            assert_eq!(
                pane.buffer().text(),
                if matches!(operation, super::FindOperation::Replace) {
                    "prefix cow cat\n"
                } else {
                    "prefix cow cow\n"
                }
            );
            assert!(pane.undo());
            assert_eq!(pane.buffer().text(), "prefix cat cat\n");
            assert!(pane.undo());
            assert_eq!(pane.buffer().text(), "cat cat\n");
            assert!(!pane.is_modified());
        }
    }

    #[test]
    fn worker_replacements_reject_changed_content_cursor_settings_and_read_only_state() {
        for change in 0..5 {
            let mut pane = Pane::from_text("cat cat\n");
            let mut settings = settings("cat");
            settings.replacement = "cow".to_owned();
            let mut task = ready_task(&pane, &settings, super::FindOperation::ReplaceAll);
            match change {
                0 => pane.buffer_mut().insert(0, "x"),
                1 => pane.place(Caret::new(0, 2), false),
                2 => settings.replacement = "dog".to_owned(),
                3 => pane.set_read_only(true),
                _ => pane.reset(ca_text::TextBuffer::from_text("cat cat\n")),
            }
            let before = pane.buffer().text();
            let done = task.poll(&mut pane, &settings).unwrap();
            assert!(!done.edited && !done.selected);
            assert!(done.message.unwrap().contains("cancelled"));
            assert_eq!(pane.buffer().text(), before);
        }
    }

    #[test]
    fn cancelled_or_superseded_worker_results_never_apply() {
        let mut pane = Pane::from_text("cat cat\n");
        let mut settings = settings("cat");
        settings.replacement = "cow".to_owned();
        let mut task = ready_task(&pane, &settings, super::FindOperation::ReplaceAll);
        task.cancel();
        assert!(task.poll(&mut pane, &settings).is_none());
        assert_eq!(pane.buffer().text(), "cat cat\n");
        let (send, receive) = std::sync::mpsc::channel();
        task.start(
            &pane,
            &settings,
            super::FindOperation::Replace,
            std::sync::Arc::new(move || {
                let _ = send.send(());
            }),
        );
        receive
            .recv_timeout(std::time::Duration::from_secs(20))
            .unwrap();
        settings.replacement = "dog".to_owned();
        let (send, receive) = std::sync::mpsc::channel();
        task.start(
            &pane,
            &settings,
            super::FindOperation::ReplaceAll,
            std::sync::Arc::new(move || {
                let _ = send.send(());
            }),
        );
        receive
            .recv_timeout(std::time::Duration::from_secs(20))
            .unwrap();
        assert!(task.poll(&mut pane, &settings).unwrap().edited);
        assert_eq!(pane.buffer().text(), "dog dog\n");
    }

    #[test]
    fn worker_regex_replacement_expands_captures_and_reports_invalid_patterns_without_edits() {
        let mut pane = Pane::from_text("cat 12 cat 34\n");
        let mut settings = settings("cat (\\d+)");
        settings.regex = true;
        settings.replacement = "\\1\\n".to_owned();
        let mut task = ready_task(&pane, &settings, super::FindOperation::ReplaceAll);
        let done = task.poll(&mut pane, &settings).unwrap();
        assert!(done.edited);
        assert_eq!(done.message.as_deref(), Some("2 replaced."));
        assert_eq!(pane.buffer().text(), "12\n 34\n\n");
        assert!(pane.undo());
        settings.pattern = "(".to_owned();
        let mut task = ready_task(&pane, &settings, super::FindOperation::ReplaceAll);
        let done = task.poll(&mut pane, &settings).unwrap();
        assert!(!done.edited);
        assert!(done.message.unwrap().contains("pattern"));
        assert_eq!(pane.buffer().text(), "cat 12 cat 34\n");
    }

    #[test]
    fn find_selects_the_match_and_repeats_forward() {
        let mut pane = Pane::from_text("one two one\n");
        let settings = settings("one");
        assert_eq!(find_next(&mut pane, &settings, false), FindOutcome::Found);
        assert_eq!(pane.selected_text().as_deref(), Some("one"));
        assert_eq!(pane.caret(), Caret::new(0, 3));
        assert_eq!(find_next(&mut pane, &settings, false), FindOutcome::Found);
        assert_eq!(pane.caret(), Caret::new(0, 11));
    }

    #[test]
    fn a_backwards_find_walks_the_other_way() {
        let mut pane = Pane::from_text("one two one\n");
        let settings = settings("one");
        pane.place(Caret::new(0, 11), false);
        assert_eq!(find_next(&mut pane, &settings, true), FindOutcome::Found);
        assert_eq!(pane.selection().unwrap().0, Caret::new(0, 8));
    }

    #[test]
    fn a_search_without_wrap_stops_at_the_end() {
        let mut pane = Pane::from_text("alpha\n");
        let mut settings = settings("alpha");
        settings.wrap = false;
        pane.place(Caret::new(0, 5), false);
        assert_eq!(
            find_next(&mut pane, &settings, false),
            FindOutcome::NotFound
        );
    }

    #[test]
    fn a_wrapping_search_comes_back_round() {
        let mut pane = Pane::from_text("alpha\n");
        let settings = settings("alpha");
        pane.place(Caret::new(0, 5), false);
        assert_eq!(find_next(&mut pane, &settings, false), FindOutcome::Found);
    }

    #[test]
    fn matching_case_narrows_the_search() {
        let mut pane = Pane::from_text("Apple apple\n");
        let mut settings = settings("apple");
        settings.match_case = true;
        assert_eq!(find_next(&mut pane, &settings, false), FindOutcome::Found);
        assert_eq!(pane.selection().unwrap().0, Caret::new(0, 6));
    }

    #[test]
    fn whole_words_rejects_a_match_inside_a_word() {
        let mut pane = Pane::from_text("applesauce apple\n");
        let mut settings = settings("apple");
        settings.whole_words = true;
        assert_eq!(find_next(&mut pane, &settings, false), FindOutcome::Found);
        assert_eq!(pane.selection().unwrap().0, Caret::new(0, 11));
    }

    #[test]
    fn a_regular_expression_matches_a_class() {
        let mut pane = Pane::from_text("abc 123\n");
        let mut settings = settings("\\d+");
        settings.regex = true;
        assert_eq!(find_next(&mut pane, &settings, false), FindOutcome::Found);
        assert_eq!(pane.selected_text().as_deref(), Some("123"));
    }

    #[test]
    fn a_broken_pattern_reports_itself() {
        let mut pane = Pane::from_text("abc\n");
        let mut settings = settings("(");
        settings.regex = true;
        let outcome = find_next(&mut pane, &settings, false);
        assert!(matches!(outcome, FindOutcome::BadPattern(_)));
        assert!(outcome.message().is_some());
    }

    #[test]
    fn a_selection_only_search_stays_inside_it() {
        let mut pane = Pane::from_text("one\ntwo\none\n");
        let mut settings = settings("one");
        settings.selection_only = true;
        settings.wrap = false;
        pane.place(Caret::new(0, 0), false);
        pane.place(Caret::new(1, 3), true);
        assert_eq!(find_next(&mut pane, &settings, false), FindOutcome::Found);
        assert_eq!(pane.selection().unwrap().0, Caret::new(0, 0));
    }

    #[test]
    fn replace_next_substitutes_one_match() {
        let mut pane = Pane::from_text("one one\n");
        let mut settings = settings("one");
        settings.replacement = "two".to_owned();
        assert_eq!(replace_next(&mut pane, &settings), FindOutcome::Found);
        assert_eq!(pane.buffer().text(), "two one\n");
    }

    #[test]
    fn replace_all_substitutes_every_match_in_one_step() {
        let mut pane = Pane::from_text("one one one\n");
        let mut settings = settings("one");
        settings.replacement = "x".to_owned();
        assert_eq!(replace_all(&mut pane, &settings), Ok(3));
        assert_eq!(pane.buffer().text(), "x x x\n");
        assert!(pane.undo());
        assert_eq!(pane.buffer().text(), "one one one\n");
    }

    #[test]
    fn a_read_only_pane_replaces_nothing() {
        let mut pane = Pane::from_text("one\n");
        pane.set_read_only(true);
        let mut settings = settings("one");
        settings.replacement = "two".to_owned();
        assert_eq!(replace_all(&mut pane, &settings), Ok(0));
        assert_eq!(pane.buffer().text(), "one\n");
    }

    #[test]
    fn go_to_places_the_caret_by_one_based_line_and_column() {
        let mut pane = Pane::from_text("alpha\nbeta\ngamma\n");
        go_to(&mut pane, 2, 3);
        assert_eq!(pane.caret(), Caret::new(1, 2));
        go_to(&mut pane, 99, 1);
        assert_eq!(pane.caret().line, pane.line_count() - 1);
    }

    #[test]
    fn a_large_buffer_wants_a_worker() {
        let small = Pane::from_text("x\n");
        assert!(!wants_worker(small.buffer()));
        let large = Pane::from_text(&"line\n".repeat(80_000));
        assert!(wants_worker(large.buffer()));
    }

    #[test]
    fn a_bookmark_toggles_on_and_off() {
        let mut marks = Bookmarks::new();
        marks.toggle(3, 10);
        assert_eq!(marks.row_of(3), Some(10));
        assert!(marks.marks(10));
        marks.toggle(3, 10);
        assert!(marks.is_empty());
    }

    #[test]
    fn a_bookmark_slot_moves_rather_than_duplicating() {
        let mut marks = Bookmarks::new();
        marks.toggle(1, 5);
        marks.toggle(1, 9);
        assert_eq!(marks.row_of(1), Some(9));
        assert_eq!(marks.len(), 1);
    }

    #[test]
    fn bookmark_navigation_finds_the_nearest_marker() {
        let mut marks = Bookmarks::new();
        marks.toggle(0, 2);
        marks.toggle(1, 8);
        marks.toggle(2, 20);
        assert_eq!(marks.next(2), Some(8));
        assert_eq!(marks.previous(8), Some(2));
        assert_eq!(marks.next(20), None);
        assert_eq!(marks.previous(2), None);
    }

    #[test]
    fn a_slot_past_the_last_one_is_ignored() {
        let mut marks = Bookmarks::new();
        marks.toggle(10, 4);
        assert!(marks.is_empty());
    }

    #[test]
    fn bookmarks_follow_an_edit_that_changes_the_line_count() {
        let mut marks = Bookmarks::new();
        marks.toggle(0, 1);
        marks.toggle(1, 5);
        marks.toggle(2, 10);
        marks.shift(4, 2, 0);
        assert_eq!(marks.row_of(0), Some(1));
        assert_eq!(marks.row_of(1), None);
        assert_eq!(marks.row_of(2), Some(8));
    }

    #[test]
    fn clearing_removes_every_marker() {
        let mut marks = Bookmarks::new();
        marks.toggle(0, 1);
        marks.clear();
        assert!(marks.is_empty());
    }
}
