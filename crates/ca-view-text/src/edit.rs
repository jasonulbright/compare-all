//! One editable text file with no comparison.
//!
//! The pane, the find and go to strips, the syntax coloring and the save are
//! the ones the comparison view uses; this view only lays one pane out where
//! the comparison lays out two.

use crate::file_save::{FileSave, SaveEvent};
use crate::{
    lines, paint_caret, paint_side, syntax, CachedLine, EditMarks, PaneContent, PaneGeometry, Run,
    DEFAULT_FONT_SIZE, HORIZONTAL_STEP, LINE_NUMBER_COLUMNS, ROW_HEIGHT_RATIO, SCROLLBAR,
};
use ca_text::{DecodeOptions, EolStyle, LoadedText};
use ca_ui::command::Command;
use ca_ui::dialog::{self, DialogMessage, Pick};
use ca_ui::editor::{self, Motion, Pane};
use ca_ui::find::{
    self, Bookmarks, FindOperation, FindPanel, FindSettings, FindTask, PanelRequest,
};
use ca_ui::save::{Baseline, FileSystem, RealFileSystem, Stamp};
use ca_ui::scroll::RowScroll;
use ca_ui::theme::{Palette, TextClass};
use ca_ui::view::{SessionView, ViewAction, ViewContext};
use ca_ui::widgets;
use ca_ui::worker::{Cancel, Emitter, Job, Terminal};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Every command this view answers for.
const HANDLED: &[Command] = &[
    Command::Undo,
    Command::Redo,
    Command::Cut,
    Command::Copy,
    Command::Paste,
    Command::SelectAll,
    Command::IncreaseIndent,
    Command::DecreaseIndent,
    Command::ToggleOverwrite,
    Command::Find,
    Command::Replace,
    Command::FindNext,
    Command::FindPrevious,
    Command::GoTo,
    Command::ClearBookmarks,
    Command::NextEdit,
    Command::PreviousEdit,
    Command::ToggleLineNumbers,
    Command::SaveFile,
    Command::SaveFileAs,
    Command::Reload,
    Command::OpenFile,
    Command::IncreaseFontSize,
    Command::DecreaseFontSize,
    Command::ResetFontSize,
    Command::Cancel,
];

/// What the loading worker posts.
#[derive(Debug)]
pub enum EditMessage {
    /// The file is read.
    Loaded(Box<Loaded>),
    /// The file could not be read.
    Failed(String),
    /// The read stopped.
    Cancelled,
}

/// A file as read.
#[derive(Debug)]
pub struct Loaded {
    source: LoadedText,
    stamp: Option<Stamp>,
    lines: Arc<Vec<String>>,
}

impl Terminal for EditMessage {
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

fn read(path: &Path, emitter: &Emitter<EditMessage>, cancel: &Cancel) {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            emitter.send(EditMessage::Failed(format!("{}: {error}", path.display())));
            return;
        }
    };
    let stamp = RealFileSystem.stamp(path);
    let source = LoadedText::load(&bytes, &DecodeOptions::default());
    if cancel.is_cancelled() {
        return;
    }
    let lines = split_lines(&source.buffer.text());
    emitter.send(EditMessage::Loaded(Box::new(Loaded {
        source,
        stamp,
        lines,
    })));
}

/// The lines of a text without their endings, for the coloring worker.
pub(crate) fn split_lines(text: &str) -> Arc<Vec<String>> {
    Arc::new(
        ca_diff::split_lines(text)
            .into_iter()
            .map(|line| line.trim_end_matches(['\n', '\r']).to_owned())
            .collect(),
    )
}

/// Where the view stands.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Status {
    Loading,
    Ready,
    Failed(String),
}

/// A single file text editor tab.
#[allow(clippy::struct_excessive_bools)]
pub struct TextEditView {
    id: egui::Id,
    path: PathBuf,
    /// The settings the session last gave, reported with the side replaced by
    /// the file the editor has open.
    session_settings: ca_session::settings::TextEditSettings,
    field: String,
    notify: Arc<dyn Fn() + Send + Sync>,
    job: Option<Job<EditMessage>>,
    picker: Option<Job<DialogMessage>>,
    picker_saves: bool,
    status: Status,
    pane: Pane,
    source: LoadedText,
    stamp: Option<Stamp>,
    syntax: syntax::Highlighter,
    font: ca_ui::font::FontSize,
    line_spacing: u32,
    find_from_word: bool,
    save: FileSave,
    find: FindPanel,
    find_task: FindTask,
    bookmarks: Bookmarks,
    edits: EditMarks,
    message: Option<String>,
    closing: bool,
    close_requested: bool,
    show_line_numbers: bool,
    scroll: RowScroll,
    horizontal: f32,
    widest: u32,
    viewport_height: f32,
    viewport_rows: usize,
    input_pass: Option<u64>,
    pending_clipboard: Option<String>,
    paste_requested: bool,
    line_cache: HashMap<u32, CachedLine>,
    revision: u64,
    cached_revision: (u64, u64),
}

impl TextEditView {
    /// An editor over `path`, or over a new empty text when `path` is empty.
    #[must_use]
    pub fn new(path: PathBuf, context: &ViewContext, instance: u64) -> Self {
        let mut view = Self {
            id: egui::Id::new(("text-edit", instance)),
            session_settings: ca_session::settings::TextEditSettings::default(),
            field: path.display().to_string(),
            path,
            notify: context.notify.clone(),
            job: None,
            picker: None,
            picker_saves: false,
            status: Status::Ready,
            pane: Pane::default(),
            source: LoadedText::load(b"", &DecodeOptions::default()),
            stamp: None,
            syntax: syntax::Highlighter::default(),
            font: ca_ui::font::FontSize::new(DEFAULT_FONT_SIZE),
            line_spacing: 0,
            find_from_word: ca_session::options::TextEditingOptions::default()
                .find_uses_current_word,
            save: FileSave::default(),
            find_task: FindTask::default(),
            find: FindPanel::with_settings(FindSettings {
                wrap: true,
                ..FindSettings::default()
            }),
            bookmarks: Bookmarks::new(),
            edits: EditMarks::default(),
            message: None,
            closing: false,
            close_requested: false,
            show_line_numbers: true,
            scroll: RowScroll::top(),
            horizontal: 0.0,
            widest: 0,
            viewport_height: 0.0,
            viewport_rows: 0,
            input_pass: None,
            pending_clipboard: None,
            paste_requested: false,
            line_cache: HashMap::new(),
            revision: 0,
            cached_revision: (u64::MAX, u64::MAX),
        };
        view.reload();
        view
    }

    /// Read the file again, or start empty when no file is named.
    fn reload(&mut self) {
        if let Some(job) = self.job.take() {
            job.cancel();
        }
        self.syntax.set_path(&self.path);
        self.edits = EditMarks::default();
        self.bookmarks.clear();
        self.scroll = RowScroll::top();
        self.horizontal = 0.0;
        self.widest = 0;
        self.revision = self.revision.saturating_add(1);
        if self.path.as_os_str().is_empty() {
            self.source = LoadedText::load(b"", &DecodeOptions::default());
            self.stamp = None;
            self.pane = Pane::default();
            self.status = Status::Ready;
            return;
        }
        self.status = Status::Loading;
        let path = self.path.clone();
        self.job = Some(Job::spawn_notifying(
            move |emitter, cancel| read(&path, emitter, cancel),
            self.notify.clone(),
        ));
    }

    /// True once the file is on screen.
    #[must_use]
    pub fn is_loaded(&self) -> bool {
        self.status == Status::Ready
    }

    /// The editing pane.
    #[must_use]
    pub const fn pane(&self) -> &Pane {
        &self.pane
    }

    /// The editing pane, for a caller that drives it directly.
    pub fn pane_mut(&mut self) -> &mut Pane {
        &mut self.pane
    }

    /// The file the pane is written to; empty for a new text.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The last message the view showed.
    #[must_use]
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    /// True while a save runs.
    #[must_use]
    pub const fn is_saving(&self) -> bool {
        self.save.is_busy()
    }

    /// The question a save or a close is waiting on, as the text it shows.
    #[must_use]
    pub fn open_question(&self) -> Option<String> {
        if self.closing {
            return Some("This tab has edits that are not written.".to_owned());
        }
        self.save.prompt().map(|prompt| format!("{prompt:?}"))
    }

    /// Answer the open question: yes saves or retries, no cancels.
    pub fn answer(&mut self, accept: bool) {
        if self.closing {
            self.closing = false;
            if accept {
                self.close_requested = true;
                self.save_to(None);
            }
            return;
        }
        let notify = self.notify.clone();
        self.save.answer(accept, &notify);
    }

    /// Drop the edits and close, which is the close question's third answer.
    pub fn discard_and_close(&mut self) {
        self.closing = false;
        self.pane.mark_saved();
        self.close_requested = true;
    }

    /// The text the find strip searches for.
    pub fn set_find_pattern(&mut self, pattern: &str) {
        pattern.clone_into(&mut self.find.settings.pattern);
    }

    /// The text the replace strip writes.
    pub fn set_replacement(&mut self, replacement: &str) {
        replacement.clone_into(&mut self.find.settings.replacement);
    }

    /// Replace every match of the find strip in the pane.
    pub fn replace_all(&mut self) {
        self.answer_panel(Some(PanelRequest::ReplaceAll));
    }

    /// Write the pane under a new name.
    pub fn save_as(&mut self, path: &Path) {
        self.save_to(Some(path.to_path_buf()));
    }

    /// The colored runs of one line, for a test that checks the coloring.
    pub fn syntax_spans(&mut self, line: u32) -> Option<Arc<[syntax::Span]>> {
        let text = self.pane.line_text(line);
        self.syntax.spans(line, &text)
    }

    /// The name of the format the file name selected.
    #[must_use]
    pub fn format_name(&self) -> &str {
        &self.syntax.format().name
    }

    fn row_height(&self) -> f32 {
        self.font.row_height(ROW_HEIGHT_RATIO, self.line_spacing)
    }

    fn ending(&self) -> &'static str {
        match self.source.eol.dominant {
            EolStyle::CrLf => EolStyle::CrLf.as_str(),
            EolStyle::Cr => EolStyle::Cr.as_str(),
            EolStyle::Lf => EolStyle::Lf.as_str(),
        }
    }

    fn follow_options(&mut self, ctx: &egui::Context) {
        let options = ca_ui::options::current(ctx);
        self.font.follow(options.editor_point_size());
        self.line_spacing = options.extra_line_spacing();
        self.find_from_word = options.stored.text_editing.find_uses_current_word;
        self.save.rules = ca_ui::save::SaveRules::from_options(&options.stored);
    }

    fn poll_load(&mut self) {
        let Some(job) = self.job.as_mut() else {
            return;
        };
        let messages = job.drain();
        let finished = job.is_finished();
        for message in messages {
            match message {
                EditMessage::Loaded(loaded) => {
                    let loaded = *loaded;
                    self.pane.reset(loaded.source.buffer.clone());
                    self.pane
                        .set_read_only(self.session_settings.specs.disable_editing);
                    self.source = loaded.source;
                    self.stamp = loaded.stamp;
                    self.syntax.set_lines(loaded.lines);
                    self.status = Status::Ready;
                }
                EditMessage::Failed(reason) => self.status = Status::Failed(reason),
                EditMessage::Cancelled => {}
            }
        }
        if finished {
            self.job = None;
            if self.status == Status::Loading {
                self.status = Status::Failed("The read stopped.".to_owned());
            }
        }
    }

    fn poll_picker(&mut self) {
        let Some(job) = self.picker.as_mut() else {
            return;
        };
        let messages = job.drain();
        if job.is_finished() || !messages.is_empty() {
            self.picker = None;
        }
        for message in messages {
            match message {
                DialogMessage::Chosen(path) => {
                    if self.picker_saves {
                        self.save_to(Some(path));
                    } else {
                        self.open_path(path);
                    }
                }
                DialogMessage::Dismissed => {}
                DialogMessage::Failed(reason) => self.message = Some(reason),
            }
        }
    }

    fn open_path(&mut self, path: PathBuf) {
        if self.pane.is_modified() {
            self.message = Some("Save or undo the edits before another file is opened.".to_owned());
            return;
        }
        self.field = path.display().to_string();
        self.path = path;
        self.reload();
    }

    fn poll_save(&mut self) {
        let Some(event) = self.save.poll() else {
            return;
        };
        match event {
            SaveEvent::Saved {
                path,
                stamp,
                revision,
            } => {
                if path != self.path {
                    self.field = path.display().to_string();
                    self.syntax.set_path(&path);
                    self.path = path;
                    self.pane
                        .set_read_only(self.session_settings.specs.disable_editing);
                }
                self.stamp = Some(stamp);
                self.pane.buffer_mut().mark_saved_revision(revision);
                self.message = Some("Saved.".to_owned());
            }
            SaveEvent::NotWritable => {
                self.pane.set_read_only(true);
                self.message = Some("The file cannot be written, so the pane is read only.".into());
                self.close_requested = false;
            }
            SaveEvent::Failed(reason) => {
                self.message = Some(reason);
                self.close_requested = false;
            }
        }
    }

    /// Start a save to the file's own name, or to `save_as`.
    fn save_to(&mut self, save_as: Option<PathBuf>) {
        if self.session_settings.specs.disable_editing {
            self.message = Some("Editing is turned off for this session".to_owned());
            return;
        }
        let (path, expected) = match save_as {
            Some(path) => (path, Baseline::Unchecked),
            None if self.path.as_os_str().is_empty() => {
                self.open_picker(true);
                return;
            }
            None => (self.path.clone(), self.stamp.into()),
        };
        let mut source = self.source.clone();
        source.buffer = self.pane.buffer().clone();
        let revision = source.buffer.revision();
        let notify = self.notify.clone();
        if !self.save.start(path, source, expected, revision, &notify) {
            self.message = Some("A save is still running. Try again.".to_owned());
        }
    }

    fn open_picker(&mut self, saves: bool) {
        if self.picker.is_some() {
            return;
        }
        self.picker_saves = saves;
        let pick = if saves { Pick::SaveFile } else { Pick::File };
        self.picker = Some(dialog::spawn(pick, self.notify.clone()));
    }

    fn note_edit(&mut self) {
        self.revision = self.revision.saturating_add(1);
        for span in self.pane.take_changes() {
            self.bookmarks.shift(
                span.start_line as usize,
                span.removed_lines as usize,
                span.inserted_lines as usize,
            );
            self.syntax.note_edit(span);
            self.edits.note(span);
        }
        self.follow_caret();
    }

    fn follow_caret(&mut self) {
        let total = self.pane.line_count() as usize;
        let line = self.pane.caret().line as usize;
        self.scroll
            .reveal(line, self.viewport_height, self.row_height(), total);
    }

    fn seed_find(&mut self) {
        if !self.find_from_word {
            return;
        }
        let seed = self
            .pane
            .selected_text()
            .filter(|text| !text.contains(['\n', '\r']))
            .or_else(|| self.pane.word_at_caret());
        if let Some(seed) = seed {
            self.find.settings.pattern = seed;
        }
    }

    fn run_find(&mut self, backwards: bool) {
        self.start_find(FindOperation::Next(backwards));
    }

    fn start_find(&mut self, operation: FindOperation) {
        if self.job.is_some() {
            return;
        }
        self.find_task.start(
            &self.pane,
            &self.find.settings,
            operation,
            Arc::clone(&self.notify),
        );
        self.message = Some("Searching…".to_owned());
    }

    fn poll_find(&mut self) {
        if self.job.is_some() {
            self.find_task.cancel();
            return;
        }
        if let Some(done) = self.find_task.poll(&mut self.pane, &self.find.settings) {
            self.message = done.message;
            if done.edited {
                self.note_edit();
            }
            if done.selected {
                self.follow_caret();
            }
        }
    }

    fn answer_panel(&mut self, request: Option<PanelRequest>) {
        match request {
            None => {}
            Some(PanelRequest::Next) => self.run_find(false),
            Some(PanelRequest::Previous) => self.run_find(true),
            Some(PanelRequest::Replace) => {
                self.start_find(FindOperation::Replace);
            }
            Some(PanelRequest::ReplaceAll) => {
                self.start_find(FindOperation::ReplaceAll);
            }
            Some(PanelRequest::GoTo(line)) => {
                find::go_to(&mut self.pane, line, 1);
                self.follow_caret();
            }
            Some(PanelRequest::NoLine) => self.message = Some(find::NO_LINE.to_owned()),
        }
    }

    fn go_to_edit(&mut self, forward: bool) {
        let here = self.pane.caret().line;
        let target = if forward {
            self.edits.next(here)
        } else {
            self.edits.previous(here)
        };
        if let Some(line) = target {
            self.pane.place(editor::Caret::new(line, 0), false);
            self.follow_caret();
        }
    }

    fn head(&mut self, ui: &mut egui::Ui) -> Option<ViewAction> {
        let mut action = None;
        ui.horizontal(|ui| {
            if ca_ui::widgets::inline_button(ui, "Home", ca_ui::icons::Icon::Home).clicked() {
                action = Some(ViewAction::OpenHome);
            }
            ui.label("File");
            ui.add(egui::TextEdit::singleline(&mut self.field).desired_width(420.0));
            if ca_ui::widgets::inline_button(ui, "Browse", ca_ui::icons::Icon::Browse).clicked() {
                self.open_picker(false);
            }
            if ca_ui::widgets::inline_button(ui, "Open", ca_ui::icons::Icon::OpenFile).clicked() {
                let path = PathBuf::from(self.field.clone());
                self.open_path(path);
            }
            let can_save = self.status == Status::Ready && !self.save.is_busy();
            if ui
                .add_enabled(
                    can_save && self.pane.is_modified(),
                    egui::Button::new("Save"),
                )
                .clicked()
            {
                self.save_to(None);
            }
        });
        let facts = format!(
            "{}  {}{}",
            self.source.spec.encoding.label(),
            match self.source.eol.dominant {
                EolStyle::CrLf => "Windows",
                EolStyle::Cr => "Mac",
                EolStyle::Lf => "Unix",
            },
            if self.source.eol.mixed {
                " (mixed)"
            } else {
                ""
            }
        );
        ui.label(facts);
        let request = self.find.show_find(ui);
        self.answer_panel(request);
        let request = self.find.show_go_to(ui);
        self.answer_panel(request);
        let notify = self.notify.clone();
        self.save.prompt_ui(ui, self.id, &notify);
        if self.closing {
            ui.horizontal_wrapped(|ui| {
                ui.label("This tab has edits that are not written.");
                if ui.button("Save and close").clicked() {
                    self.answer(true);
                }
                if ui.button("Discard and close").clicked() {
                    self.discard_and_close();
                }
                if ui.button("Cancel").clicked() {
                    self.answer(false);
                }
            });
        }
        if let Status::Failed(reason) = &self.status {
            ui.colored_label(ui.visuals().error_fg_color, reason.as_str());
        }
        if let Some(message) = &self.message {
            ca_ui::widgets::notice_current(ui, ca_ui::icons::Icon::Info, 16.0, message.as_str());
        }
        action
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        let caret = self.pane.caret();
        ui.horizontal(|ui| {
            ui.label(format!(
                "Line {}  Column {}",
                caret.line + 1,
                self.pane.caret_column() + 1
            ));
            ui.separator();
            ui.label(if self.pane.is_overwrite() {
                "Overwrite"
            } else {
                "Insert"
            });
            ui.separator();
            ui.label(if self.pane.is_modified() {
                "Modified"
            } else {
                ""
            });
        });
    }

    fn refresh_line_cache(&mut self, range: &std::ops::Range<usize>) {
        let stamp = (self.revision, self.syntax.revision());
        if self.cached_revision != stamp {
            self.line_cache.clear();
            self.cached_revision = stamp;
        }
        let wanted: Vec<u32> = range
            .clone()
            .filter_map(|line| u32::try_from(line).ok())
            .collect();
        self.line_cache.retain(|line, _| wanted.contains(line));
        for line in &wanted {
            if self.line_cache.contains_key(line) {
                continue;
            }
            let text = self.pane.line_text(*line);
            let metrics = lines::measure(&text);
            self.widest = self.widest.max(metrics.columns);
            let spans = self
                .syntax
                .is_claimed()
                .then(|| self.syntax.spans(*line, &text))
                .flatten();
            self.line_cache.insert(
                *line,
                CachedLine {
                    text,
                    metrics,
                    spans,
                },
            );
        }
        self.syntax.retain_lines(&wanted);
    }

    #[allow(clippy::too_many_lines)]
    fn rows(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        let row_height = self.row_height();
        let font = egui::FontId::monospace(self.font.points());
        let char_width = crate::column_pitch(ui, &font);
        let gutter = if self.show_line_numbers {
            char_width * LINE_NUMBER_COLUMNS
        } else {
            0.0
        };
        let total = self.pane.line_count() as usize;
        let full = ui.available_rect_before_wrap();
        let body = egui::Rect::from_min_max(
            full.left_top(),
            egui::pos2(full.right() - SCROLLBAR, full.bottom() - SCROLLBAR),
        );
        let _ = ui.allocate_rect(full, egui::Sense::hover());
        let painter = ui.painter_at(full);
        self.viewport_height = body.height();
        self.scroll.clamp(body.height(), row_height, total);
        let range = self.scroll.visible(body.height(), row_height, total);
        self.viewport_rows = range.len();
        let text_x = body.left() + gutter;
        let width = (body.right() - text_x).max(0.0);
        let columns = (width / char_width).ceil().max(0.0);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let columns = columns as usize + 1;
        let first_column = self.horizontal.floor().max(0.0);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let first_column_index = first_column as usize;
        let column_shift = (self.horizontal - first_column) * char_width;
        self.refresh_line_cache(&range);
        let clip = painter.with_clip_rect(body);
        let (background, base) = palette.text_row_in(TextClass::Same, true);
        let mut runs: Vec<Run> = Vec::new();
        for position in range {
            let Ok(line) = u32::try_from(position) else {
                continue;
            };
            let y = body.top() + self.scroll.row_y(position, row_height);
            let geometry = PaneGeometry {
                gutter_x: body.left(),
                text_x,
                width,
                gutter_width: gutter,
                y,
                row_height,
                char_width,
                first_column: first_column_index,
                columns,
                column_shift,
            };
            let entry = self.line_cache.get(&line);
            paint_side(
                &clip,
                &geometry,
                &PaneContent {
                    line: self.show_line_numbers.then_some(line),
                    text: entry.map(|entry| entry.text.as_str()),
                    metrics: entry.map(|entry| entry.metrics),
                    syntax: entry.and_then(|entry| entry.spans.as_deref()),
                    spans: None,
                    background,
                    base_color: base,
                    difference_color: base,
                    whole_line_differs: false,
                    is_difference: false,
                    gap: false,
                },
                palette,
                &font,
                &mut runs,
            );
            paint_caret(
                &clip,
                &geometry,
                &self.pane,
                Some(line),
                entry.map_or("", |entry| entry.text.as_str()),
                true,
                (
                    palette,
                    &font,
                    entry.map_or_else(lines::LineMetrics::default, |entry| entry.metrics),
                ),
            );
            if self.bookmarks.marks(position) {
                ca_ui::icons::Icon::Bookmark.paint_in_row(
                    &clip,
                    egui::pos2(body.left() + 8.0, y + row_height / 2.0),
                    row_height,
                    palette.thumbnail_caret,
                );
            }
        }
        self.pointer(ui, body, text_x, char_width, row_height, first_column_index);
        self.scrollbars(ui, &painter, palette, body, (row_height, total, char_width));
        let hovered = ui
            .ctx()
            .pointer_latest_pos()
            .is_some_and(|pointer| full.contains(pointer));
        if hovered {
            let delta = ui.input(|input| input.smooth_scroll_delta);
            if delta.y.abs() > f32::EPSILON {
                self.scroll
                    .scroll_by(-delta.y, body.height(), row_height, total);
            }
            if delta.x.abs() > f32::EPSILON {
                self.set_horizontal(
                    self.horizontal - delta.x / char_width * HORIZONTAL_STEP,
                    body.width(),
                    char_width,
                );
            }
        }
    }

    fn set_horizontal(&mut self, columns: f32, viewport: f32, char_width: f32) {
        let visible = viewport / char_width;
        #[allow(clippy::cast_precision_loss)]
        let widest = self.widest as f32;
        self.horizontal = columns.clamp(0.0, (widest - visible).max(0.0));
    }

    fn scrollbars(
        &mut self,
        ui: &mut egui::Ui,
        painter: &egui::Painter,
        palette: &Palette,
        body: egui::Rect,
        metrics: (f32, usize, f32),
    ) {
        let (row_height, total, char_width) = metrics;
        let track = egui::Rect::from_min_max(
            egui::pos2(body.right(), body.top()),
            egui::pos2(body.right() + SCROLLBAR, body.bottom()),
        );
        painter.rect_filled(track, 0.0, palette.gutter_background);
        let thumb = self
            .scroll
            .thumb(track.height(), body.height(), row_height, total);
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(track.left() + 2.0, track.top() + thumb.start),
                egui::pos2(track.right() - 2.0, track.top() + thumb.end),
            ),
            2.0,
            palette.thumbnail_marker,
        );
        let response = ui.interact(
            track,
            self.id.with("vertical-bar"),
            egui::Sense::click_and_drag(),
        );
        if let Some(pointer) = response.interact_pointer_pos() {
            let top = pointer.y - track.top() - (thumb.end - thumb.start) / 2.0;
            self.scroll =
                RowScroll::from_thumb(top, track.height(), body.height(), row_height, total);
        }
        let bottom = egui::Rect::from_min_max(
            egui::pos2(body.left(), body.bottom()),
            egui::pos2(body.right(), body.bottom() + SCROLLBAR),
        );
        painter.rect_filled(bottom, 0.0, palette.gutter_background);
        let visible = (body.width() / char_width).max(1.0);
        #[allow(clippy::cast_precision_loss)]
        let widest = (self.widest as f32).max(visible);
        let moved = widgets::horizontal_scrollbar(
            ui,
            painter,
            self.id.with("horizontal-bar"),
            bottom,
            (palette.gutter_background, palette.thumbnail_marker),
            (widest, visible, self.horizontal),
            SCROLLBAR * 2.0,
        );
        if let Some(offset) = moved {
            self.set_horizontal(offset, body.width(), char_width);
        }
    }

    fn pointer(
        &mut self,
        ui: &egui::Ui,
        body: egui::Rect,
        text_x: f32,
        char_width: f32,
        row_height: f32,
        first_column: usize,
    ) {
        let (pointer, pressed, down, shift) = ui.input(|input| {
            (
                input.pointer.latest_pos(),
                input.pointer.primary_pressed(),
                input.pointer.primary_down(),
                input.modifiers.shift,
            )
        });
        let Some(pointer) = pointer else {
            return;
        };
        if !body.contains(pointer) || !(pressed || down) {
            return;
        }
        let position = self.scroll.row_under(pointer.y - body.top(), row_height);
        let last = self.pane.line_count().saturating_sub(1);
        let line = u32::try_from(position).unwrap_or(u32::MAX).min(last);
        let column = ((pointer.x - text_x) / char_width).floor().max(0.0);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let mut column = first_column + column as usize;
        if let Some(entry) = self.line_cache.get(&line) {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let columns = (body.width() / char_width).ceil() as usize + 2;
            #[allow(clippy::cast_precision_loss)]
            let offset = (self.horizontal - first_column as f32) * char_width;
            if let Some(hit) = crate::glyph_column_at_x(
                ui.painter(),
                &entry.text,
                entry.metrics,
                first_column..first_column.saturating_add(columns),
                &egui::FontId::monospace(self.font.points()),
                pointer.x - text_x + offset,
            ) {
                column = hit;
            }
        }
        let column = u32::try_from(column).unwrap_or(u32::MAX);
        self.pane
            .place_at_column(line, column, if pressed { shift } else { true });
    }

    fn handle_keys(&mut self, ui: &egui::Ui) {
        if self.status != Status::Ready {
            return;
        }
        let pass = ui.ctx().cumulative_pass_nr();
        if self.input_pass == Some(pass) {
            return;
        }
        self.input_pass = Some(pass);
        if ui.ctx().memory(egui::Memory::focused).is_some() {
            return;
        }
        let (events, held) = ui.input(|input| (input.events.clone(), input.modifiers));
        let mut shortcut = held.command || held.ctrl || held.alt;
        let page_rows = u32::try_from(self.viewport_rows).unwrap_or(1).max(1);
        let mut edited = false;
        let mut moved = false;
        let mut copied: Option<String> = None;
        for event in events {
            match event {
                egui::Event::Text(text) => {
                    if shortcut {
                        continue;
                    }
                    for character in text.chars() {
                        if !character.is_control() {
                            self.pane.type_character(character);
                            edited = true;
                        }
                    }
                }
                egui::Event::Copy => copied = self.pane.copy(),
                egui::Event::Cut => {
                    copied = self.pane.cut();
                    edited = true;
                }
                egui::Event::Paste(text) => {
                    self.pane.paste(&text);
                    edited = true;
                }
                egui::Event::Key {
                    key,
                    pressed,
                    modifiers,
                    ..
                } => {
                    shortcut = modifiers.command || modifiers.ctrl || modifiers.alt;
                    if pressed {
                        match self.handle_key(key, modifiers, page_rows) {
                            Some(true) => edited = true,
                            Some(false) => moved = true,
                            None => {}
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some(text) = copied {
            ui.ctx().copy_text(text);
        }
        if edited {
            self.note_edit();
        } else if moved {
            self.follow_caret();
        }
    }

    /// One key press: `Some(true)` for an edit, `Some(false)` for a move.
    fn handle_key(
        &mut self,
        key: egui::Key,
        modifiers: egui::Modifiers,
        page_rows: u32,
    ) -> Option<bool> {
        use egui::Key;
        let shift = modifiers.shift;
        let word = modifiers.command;
        let motion = match key {
            Key::ArrowLeft => Some(if word { Motion::WordLeft } else { Motion::Left }),
            Key::ArrowRight => Some(if word {
                Motion::WordRight
            } else {
                Motion::Right
            }),
            Key::ArrowUp => Some(Motion::Up),
            Key::ArrowDown => Some(Motion::Down),
            Key::PageUp => Some(Motion::PageUp(page_rows)),
            Key::PageDown => Some(Motion::PageDown(page_rows)),
            Key::Home => Some(if word {
                Motion::DocumentStart
            } else {
                Motion::LineStart
            }),
            Key::End => Some(if word {
                Motion::DocumentEnd
            } else {
                Motion::LineEnd
            }),
            _ => None,
        };
        if let Some(motion) = motion {
            self.pane.move_caret(motion, shift);
            return Some(false);
        }
        if modifiers.command {
            return match key {
                Key::Backspace => Some(self.pane.delete_word(false)),
                Key::Delete => Some(self.pane.delete_word(true)),
                _ => None,
            };
        }
        let ending = self.ending();
        match key {
            Key::Backspace => self.pane.backspace(),
            Key::Delete => self.pane.delete(),
            Key::Enter => self.pane.enter(ending),
            Key::Tab if shift => self.pane.decrease_indent(),
            Key::Tab => self.pane.tab(),
            _ => return None,
        }
        Some(true)
    }
}

impl ca_ui::view::ViewFactory for TextEditView {
    fn create(left: PathBuf, right: PathBuf, context: &ViewContext, instance: u64) -> Self {
        // A single file view takes whichever side names a file.
        let path = if left.as_os_str().is_empty() {
            right
        } else {
            left
        };
        Self::new(path, context, instance)
    }
}

impl SessionView for TextEditView {
    fn kind(&self) -> Option<ca_session::SessionKind> {
        Some(ca_session::SessionKind::TextEdit)
    }

    fn menu_view(&self) -> ca_ui::command::MenuView {
        ca_ui::command::MenuView::Other
    }

    fn apply_settings(&mut self, settings: &ca_session::settings::SessionSettings) {
        if let ca_session::settings::SessionSettings::TextEdit(edit) = settings {
            let switched =
                self.session_settings.specs.disable_editing != edit.specs.disable_editing;
            self.session_settings.clone_from(edit);
            if switched {
                self.pane
                    .set_read_only(self.session_settings.specs.disable_editing);
            }
        }
    }

    fn settings(&self) -> Option<ca_session::settings::SessionSettings> {
        let mut settings = self.session_settings.clone();
        settings.specs = ca_ui::view::with_sides(&settings.specs, &self.path, Path::new(""));
        Some(ca_session::settings::SessionSettings::TextEdit(settings))
    }

    fn holds_unwritten_edits(&self) -> bool {
        self.pane.is_modified()
    }

    fn title(&self) -> String {
        let modified = if self.pane.is_modified() { "* " } else { "" };
        let name = if self.path.as_os_str().is_empty() {
            "Untitled".to_owned()
        } else {
            crate::file_name(&self.path)
        };
        format!("{modified}{name}")
    }

    fn tick(&mut self) {
        self.poll_load();
        self.poll_picker();
        self.poll_save();
        self.poll_find();
        self.syntax.poll(&self.notify);
    }

    fn ui(&mut self, ui: &mut egui::Ui, context: &ViewContext) -> Vec<ViewAction> {
        self.follow_options(ui.ctx());
        self.handle_keys(ui);
        if let Some(text) = self.pending_clipboard.take() {
            ui.ctx().copy_text(text);
        }
        if std::mem::take(&mut self.paste_requested) {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::RequestPaste);
        }
        let mut actions = Vec::new();
        egui::TopBottomPanel::top(self.id.with("head")).show_inside(ui, |ui| {
            if let Some(action) = self.head(ui) {
                actions.push(action);
            }
        });
        egui::TopBottomPanel::bottom(self.id.with("status")).show_inside(ui, |ui| {
            self.status_bar(ui);
        });
        egui::CentralPanel::default().show_inside(ui, |ui| {
            if self.status == Status::Loading {
                ui.spinner();
                return;
            }
            self.rows(ui, &context.palette);
        });
        actions
    }

    fn commands(&self) -> Vec<ca_ui::view::CommandState> {
        ca_ui::view::declare(HANDLED, |command| self.accepts(command))
    }

    fn accepts(&self, command: Command) -> bool {
        let ready = self.status == Status::Ready;
        let editable = ready && !self.pane.is_read_only();
        let writes = !self.session_settings.specs.disable_editing;
        match command {
            Command::Undo => editable && self.pane.buffer().can_undo(),
            Command::Redo => editable && self.pane.buffer().can_redo(),
            Command::Cut
            | Command::Paste
            | Command::Replace
            | Command::IncreaseIndent
            | Command::DecreaseIndent => editable,
            Command::Copy
            | Command::SelectAll
            | Command::ToggleOverwrite
            | Command::Find
            | Command::FindNext
            | Command::FindPrevious
            | Command::GoTo
            | Command::ClearBookmarks => ready,
            Command::NextEdit | Command::PreviousEdit => ready && !self.edits.is_empty(),
            Command::SaveFile => writes && ready && self.pane.is_modified() && !self.save.is_busy(),
            Command::SaveFileAs => writes && ready && !self.save.is_busy() && self.picker.is_none(),
            Command::Reload => !self.path.as_os_str().is_empty() && !self.pane.is_modified(),
            Command::OpenFile => self.picker.is_none() && !self.save.is_busy(),
            Command::ToggleLineNumbers
            | Command::IncreaseFontSize
            | Command::DecreaseFontSize
            | Command::ResetFontSize => true,
            Command::Cancel => {
                self.job.is_some() || self.find_task.is_running() || self.find.is_find_open()
            }
            _ => false,
        }
    }

    fn run(&mut self, command: Command) {
        match command {
            Command::Undo => {
                if self.pane.undo() {
                    self.note_edit();
                }
            }
            Command::Redo => {
                if self.pane.redo() {
                    self.note_edit();
                }
            }
            Command::Cut => {
                self.pending_clipboard = self.pane.cut();
                self.note_edit();
            }
            Command::Copy => self.pending_clipboard = self.pane.copy(),
            Command::Paste => self.paste_requested = true,
            Command::SelectAll => self.pane.select_all(),
            Command::IncreaseIndent => {
                self.pane.increase_indent();
                self.note_edit();
            }
            Command::DecreaseIndent => {
                self.pane.decrease_indent();
                self.note_edit();
            }
            Command::ToggleOverwrite => self.pane.toggle_overwrite(),
            Command::Find => {
                self.seed_find();
                self.find.open_find();
            }
            Command::Replace => {
                self.seed_find();
                self.find.open_replace();
            }
            Command::FindNext => self.run_find(false),
            Command::FindPrevious => self.run_find(true),
            Command::GoTo => self.find.open_go_to(),
            Command::ClearBookmarks => self.bookmarks.clear(),
            Command::NextEdit => self.go_to_edit(true),
            Command::PreviousEdit => self.go_to_edit(false),
            Command::ToggleLineNumbers => self.show_line_numbers = !self.show_line_numbers,
            Command::SaveFile => self.save_to(None),
            Command::SaveFileAs => self.open_picker(true),
            Command::Reload => self.reload(),
            Command::OpenFile => self.open_picker(false),
            Command::IncreaseFontSize | Command::DecreaseFontSize | Command::ResetFontSize => {
                self.font.run(command);
            }
            Command::Cancel => {
                self.find_task.cancel();
                if let Some(job) = self.job.as_ref() {
                    job.cancel();
                }
                self.find.close();
                self.message = None;
            }
            _ => {}
        }
    }

    fn wants_close(&self) -> bool {
        self.close_requested && !self.pane.is_modified() && !self.save.is_busy()
    }

    fn is_busy(&self) -> bool {
        self.save.is_busy()
    }

    fn may_close(&mut self) -> bool {
        if self.save.blocks_close() {
            return false;
        }
        if !self.pane.is_modified() {
            return true;
        }
        self.closing = true;
        false
    }

    fn is_ready(&self) -> bool {
        self.status != Status::Loading
    }

    fn launch_target(&self) -> Option<ca_ui::launch::LaunchTarget> {
        use ca_session::options::{LaunchContext, LaunchSide};
        if self.path.as_os_str().is_empty() {
            return None;
        }
        Some(ca_ui::launch::LaunchTarget::files(LaunchContext {
            first: LaunchSide {
                path: self.path.clone(),
                base: self.path.parent().map(Path::to_path_buf),
                line: Some(self.pane.caret().line.saturating_add(1)),
            },
            second: None,
        }))
    }

    fn on_close(&mut self) {
        if let Some(job) = self.job.take() {
            job.cancel();
        }
        if let Some(job) = self.picker.take() {
            job.cancel();
        }
        self.syntax.clear();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::TextEditView;
    use ca_ui::command::Command;
    use ca_ui::view::SessionView;
    use std::path::PathBuf;

    #[test]
    fn open_file_is_available_in_the_text_editor() {
        let view = TextEditView::new(PathBuf::new(), &ca_ui::testing::context(), 1);
        assert!(view
            .commands()
            .iter()
            .any(|state| state.command == Command::OpenFile && state.enabled));
    }

    #[test]
    fn a_text_editor_names_its_file_for_the_file_manager() {
        let path = PathBuf::from("draft.txt");
        let view = TextEditView::new(path.clone(), &ca_ui::testing::context(), 2);

        assert_eq!(
            view.explorer_target(),
            Some((path, ca_ui::launch::Selection::Files))
        );
    }
}
