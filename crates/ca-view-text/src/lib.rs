//! Two pane editable text comparison.

pub use ca_ui::save::text as save;

pub use ca_ui::find;
pub mod edit;
mod file_save;
pub mod jobs;
pub mod lines;
pub mod model;
pub mod patch_view;
pub mod prettify;
pub mod settings;
pub mod sidecopy;
pub mod structure;
pub mod syntax;

use ca_diff::Importance;
use ca_text::{EolStyle, LoadedText, TextBuffer};
pub use ca_ui::editor;
pub use ca_ui::rediff;

use ca_ui::command::Command;
use ca_ui::dialog::{self, DialogMessage, Pick, Target};
use ca_ui::report::{ReportKind, ViewReport};
use ca_ui::save::{Baseline, CloseChoice, Reread, SaveMessage, SaveOutcome, Stamp};
use ca_ui::scroll::RowScroll;
use ca_ui::theme::{Palette, TextClass};
use ca_ui::thumbnail::{Strip, Thumbnail};
use ca_ui::toolbar;
use ca_ui::view::{SessionView, ViewAction, ViewContext};
use ca_ui::widgets;
use ca_ui::worker::{Job, Terminal};
pub use edit::TextEditView;
use editor::{Motion, Pane};
use find::{Bookmarks, FindOperation, FindPanel, FindSettings, FindTask, PanelRequest};
use jobs::{ColumnSpan, TextData, TextMessage};
use model::{DisplayFilter, RowClass, Visible};
pub use patch_view::TextPatchView;
use rediff::Rediff;
use save::SaveConsent;
use sidecopy::Side;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

/// Width of the overview strip.
const THUMBNAIL_WIDTH: f32 = 26.0;
/// Width of the splitter between the two panes.
///
/// The panes meet at a splitter one line of each separator color wide, so the
/// room between them is a rule rather than a column.
const CENTER_WIDTH: f32 = 2.0;
/// Thickness of a scrollbar.
const SCROLLBAR: f32 = 12.0;
/// Display font size a new view starts at.
///
/// The size is chosen for the row height it produces: at this size and at a
/// display scale of one, [`TextView::row_height`] is fifteen points, which is
/// the measured row height.
const DEFAULT_FONT_SIZE: f32 = 10.5;
/// Row height as a multiple of the display font size.
const ROW_HEIGHT_RATIO: f32 = 1.45;
/// What the message panel says when navigation reaches the end.
const LAST_DIFFERENCE: &str = "No further difference in that direction.";
/// Width of the copy arrow column at the left edge of each pane's gutter.
const ARROW_WIDTH: f32 = 13.0;
/// Columns kept for line numbers, in characters.
const LINE_NUMBER_COLUMNS: f32 = 6.0;
/// Points between the diagonal lines of a gap row's hatch.
const HATCH_STEP: f32 = 7.0;
/// Columns a wheel notch moves the panes sideways.
const HORIZONTAL_STEP: f32 = 3.0;
/// Reason shown on a control this build does not answer for.
const PENDING: &str = "Not available in this build";
/// Reason shown on a control that needs a finished comparison.
const NOT_COMPARED: &str = "Available once the comparison finishes";
/// Reason shown on a control a read-only comparison does not offer.
const LOCKED: &str = "This view is read-only";

const fn command_is_save_or_overwrite(command: Command) -> bool {
    matches!(
        command,
        Command::SaveFile | Command::SaveFileAs | Command::SaveBoth | Command::ToggleOverwrite
    )
}

const fn command_is_disabled_while_locked(command: Command) -> bool {
    matches!(
        command,
        Command::SwapSides
            | Command::Reload
            | Command::SaveFile
            | Command::SaveFileAs
            | Command::SaveBoth
            | Command::CopyToOtherSide
            | Command::ToggleOverwrite
            | Command::OpenFile
    )
}

/// Every command this view answers for.
///
/// The shell reads the list to build its menus, so the set is stated once here
/// and nowhere else.
const HANDLED: &[Command] = &[
    Command::CompareReport,
    Command::NextDifference,
    Command::PreviousDifference,
    Command::NextSection,
    Command::PreviousSection,
    Command::ShowAll,
    Command::ShowDifferences,
    Command::ShowSame,
    Command::ShowContext,
    Command::ToggleIgnoreUnimportant,
    Command::ToggleLineDetails,
    Command::ToggleLineNumbers,
    Command::ToggleSyntaxHighlighting,
    Command::PrettifyForComparison,
    Command::CompareStructure,
    Command::CopyToOtherSide,
    Command::SwapSides,
    Command::Reload,
    Command::OpenFile,
    Command::Find,
    Command::FindNext,
    Command::FindPrevious,
    Command::Replace,
    Command::GoTo,
    Command::ClearBookmarks,
    Command::NextEdit,
    Command::PreviousEdit,
    Command::Undo,
    Command::Redo,
    Command::Cut,
    Command::Copy,
    Command::Paste,
    Command::SelectAll,
    Command::SelectSection,
    Command::IncreaseIndent,
    Command::DecreaseIndent,
    Command::CopyToRight,
    Command::CopyToLeft,
    Command::CopyLineToRight,
    Command::CopyLineToLeft,
    Command::ToggleOverwrite,
    Command::SaveFile,
    Command::SaveFileAs,
    Command::SaveBoth,
    Command::IncreaseFontSize,
    Command::DecreaseFontSize,
    Command::ResetFontSize,
    Command::Cancel,
];

/// Where a comparison currently stands.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Status {
    /// Work is in progress, with the step it has reached.
    Running(&'static str),
    /// The comparison is on screen.
    Ready,
    /// The comparison could not be produced.
    Failed(String),
    /// The run stopped before it produced a comparison.
    Cancelled,
}

impl Status {
    const fn is_running(&self) -> bool {
        matches!(self, Status::Running(_))
    }
}

/// A formatted-comparison job result.
enum FormatMessage {
    /// Formatting finished without changing either source.
    Ready {
        original: Box<PreparedBuffers>,
        formatted: Box<PreparedBuffers>,
        comparison: Option<Box<TextData>>,
        summary: Option<structure::Summary>,
    },
    /// Formatting could not safely produce a comparison projection.
    Failed(String),
    Refused(String),
    /// The user stopped the formatting job.
    Cancelled,
}

impl Terminal for FormatMessage {
    fn is_terminal(&self) -> bool {
        true
    }

    fn cancelled() -> Self {
        Self::Cancelled
    }

    fn panicked(detail: String) -> Self {
        Self::Failed(format!("formatter stopped unexpectedly: {detail}"))
    }
}

/// Original pane contents held while the temporary formatted view is active.
struct FormattedCompare {
    original: PreparedBuffers,
    summary: Option<structure::Summary>,
}

struct PreparedBuffers {
    left: ca_text::TextBuffer,
    right: ca_text::TextBuffer,
    left_text: String,
    right_text: String,
    left_lines: Arc<Vec<String>>,
    right_lines: Arc<Vec<String>>,
}

fn prepared_lines(text: &str, cancel: &dyn ca_diff::Cancel) -> Result<Arc<Vec<String>>, String> {
    let mut lines = Vec::new();
    for (index, line) in ca_diff::split_lines(text).into_iter().enumerate() {
        if index % 1024 == 0 && cancel.is_cancelled() {
            return Err("formatting cancelled".to_owned());
        }
        lines.push(line.to_owned());
    }
    Ok(Arc::new(lines))
}

/// A text comparison tab.
#[allow(clippy::struct_excessive_bools)]
pub struct TextView {
    id: egui::Id,
    left_path: PathBuf,
    right_path: PathBuf,
    left_field: String,
    right_field: String,
    /// Caller supplied names that replace temporary paths in the interface.
    left_title: Option<String>,
    right_title: Option<String>,
    /// Kept so every job this view ever spawns asks for the repaint that shows
    /// its result. A job spawned without it posts into a frame loop that is
    /// asleep.
    notify: Arc<dyn Fn() + Send + Sync>,
    job: Option<Job<TextMessage>>,
    format_job: Option<Job<FormatMessage>>,
    picker: Option<Job<DialogMessage>>,
    picker_target: Target,
    /// True when the open panel names a file to write rather than to read.
    picker_saves: bool,
    status: Status,
    data: TextData,
    filter: DisplayFilter,
    context_lines: u32,
    visible: Visible,
    caret: usize,
    font: ca_ui::font::FontSize,
    /// Padding the options add between rows, read once per frame.
    line_spacing: u32,
    /// What the Next Difference page states, read once per frame.
    navigation: ca_session::options::NextDifferenceOptions,
    /// True when opening the find strip takes the word under the caret.
    find_from_word: bool,
    /// True when a difference line keeps its syntax colors.
    syntax_on_differences: bool,
    /// True when grammar-colored text is painted.
    syntax_highlighting: bool,
    /// The text column a ruler is drawn at, where one is drawn.
    column_line: Option<u32>,
    /// How far the pane without focus is darkened, from zero to one.
    dim_fraction: f32,
    /// Where the last frame drew the ruler in each pane, where it drew one.
    painted_rulers: Vec<f32>,
    /// The pane the last frame darkened, where it darkened one.
    painted_dim: Option<Side>,
    scroll: RowScroll,
    /// How far the panes are scrolled sideways, in columns.
    horizontal: f32,
    strip: Strip<TextClass>,
    strip_stale: bool,
    viewport_height: f32,
    viewport_rows: usize,
    left_pane: Pane,
    right_pane: Pane,
    left_source: Option<LoadedText>,
    right_source: Option<LoadedText>,
    left_stamp: Option<Stamp>,
    right_stamp: Option<Stamp>,
    active: Side,
    rediff: Rediff<Job<TextMessage>>,
    /// The generation of the comparison that was running when the last load
    /// landed. A comparison of this generation or an older one compares pane
    /// text the load replaced, so its result is refused.
    loaded_generation: u64,
    find: FindPanel,
    find_task: FindTask,
    find_side: Side,
    bookmarks: Bookmarks,
    message: Option<String>,
    save_job: Option<Job<SaveMessage>>,
    save_rules: ca_ui::save::SaveRules,
    saving: Option<Side>,
    saving_revision: Option<u64>,
    saving_as: Option<PathBuf>,
    /// What the user agreed to for the save in progress. An answer adds its
    /// own consent to these and never grants another.
    saving_consent: SaveConsent,
    save_both_pending: bool,
    prompt: Option<Prompt>,
    close_requested: bool,
    /// The command that reads the files again once the save in progress has
    /// written every edit.
    reread_after_save: Option<Reread>,
    /// Settings whose format change waits on the question about the edits.
    pending_settings: Option<Box<ca_session::settings::TextCompareSettings>>,
    /// The text and width of the lines the viewport covers.
    ///
    /// Reading a line out of a rope costs the length of that line, so a pane
    /// showing one very long line must not read it again every frame. The cache
    /// holds only the rows in view and is emptied whenever an edit lands.
    line_cache: std::collections::HashMap<(u8, u32), CachedLine>,
    /// Counts the edits this view has seen, which is what empties the cache.
    revision: u64,
    /// The edit and coloring revisions the cache was filled at.
    cached_revision: (u64, u64, u64),
    /// The rectangle the rows were painted in on the last frame.
    body: egui::Rect,
    /// The x coordinate that separates the two panes, from the last frame.
    center_x: f32,
    left_syntax: syntax::Highlighter,
    right_syntax: syntax::Highlighter,
    left_edits: EditMarks,
    right_edits: EditMarks,
    /// The session's settings, which are the one source the comparison options
    /// are derived from.
    session_settings: ca_session::settings::TextCompareSettings,
    /// Unimportant differences read as matching text.
    ignore_unimportant: bool,
    /// The line details area under the panes is shown.
    show_details: bool,
    /// The line number gutter is shown.
    show_line_numbers: bool,
    /// How far the two detail lines are scrolled sideways, in columns.
    details_horizontal: f32,
    /// When the comparison on screen started reading.
    started: Option<Instant>,
    /// How long the comparison on screen took, measured.
    load_time: Option<std::time::Duration>,
    /// What the last frame laid the toolbar out in, and the room it had.
    toolbar_extent: (f32, f32),
    /// Where the last frame put each section's copy arrow.
    arrows: Vec<Arrow>,
    /// Left edge of each pane's gutter on the last frame, and its width.
    gutter_x: (f32, f32, f32),
    /// The pass the keyboard events were last consumed in.
    ///
    /// Reading the same pass twice inserts every typed character twice, so the
    /// pass number gates the handler rather than the call site.
    input_pass: Option<u64>,
    pending_clipboard: Option<String>,
    formatted_compare: Option<FormattedCompare>,
    paste_requested: bool,
    /// A load can finish in `tick` before the first `ui` pass has read the
    /// options, so the jump to the first difference waits for `ui`.
    first_difference_pending: bool,
    /// The report command of this view.
    report: ViewReport,
    /// The two texts came from the caller rather than from files, so nothing
    /// is read, swapped, edited or written.
    locked: bool,
    /// The files are copies the tab owns, so neither pane takes an edit and
    /// nothing is saved.
    read_only: bool,
}

/// Where one difference section's copy arrow was drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Arrow {
    /// The pane the arrow sits in.
    pub side: Side,
    /// The difference section it copies.
    pub section: u32,
    /// The rectangle it was drawn in.
    pub rect: egui::Rect,
}

/// The lines one pane has been edited on during this session.
#[derive(Debug, Default)]
struct EditMarks {
    lines: Vec<u32>,
}

impl EditMarks {
    /// Record an edit and move the marks under it.
    fn note(&mut self, span: editor::EditSpan) {
        let at = span.start_line;
        let removed = at.saturating_add(span.removed_lines);
        self.lines.retain(|line| *line <= at || *line >= removed);
        let shift = i64::from(span.inserted_lines) - i64::from(span.removed_lines);
        for line in &mut self.lines {
            if *line > at {
                let moved = i64::from(*line).saturating_add(shift).max(0);
                *line = u32::try_from(moved).unwrap_or(u32::MAX);
            }
        }
        for line in at..=at.saturating_add(span.inserted_lines) {
            self.lines.push(line);
        }
        self.lines.sort_unstable();
        self.lines.dedup();
    }

    fn next(&self, after: u32) -> Option<u32> {
        self.lines.iter().copied().find(|line| *line > after)
    }

    fn previous(&self, before: u32) -> Option<u32> {
        self.lines.iter().rev().copied().find(|line| *line < before)
    }

    fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

/// One viewport line, held between frames.
struct CachedLine {
    text: String,
    metrics: lines::LineMetrics,
    /// The colored runs, or `None` when the format has no grammar.
    spans: Option<Arc<[syntax::Span]>>,
}

/// A question the view is waiting on an answer to.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Prompt {
    /// A concurrent version was retained beside the saved file.
    KeptCopy(String),
    /// The file changed on disk since it was read.
    DiskChanged(Side),
    /// Writing the file cannot reproduce every character.
    WouldLose(Side, String),
    /// The tab is closing with edits that are not written.
    Closing,
    /// A command that reads the files again waits on edits that are not
    /// written.
    Reread(Reread),
}

impl TextView {
    /// A comparison of two files, with the work already started.
    #[must_use]
    pub fn new(left: PathBuf, right: PathBuf, context: &ViewContext, salt: u64) -> Self {
        let mut view = Self::blank(left, right, context, salt);
        view.restart();
        view
    }

    /// A read-only comparison of two texts the caller holds.
    ///
    /// The paths only name the sides and choose the syntax format; nothing is
    /// read from them. The comparison runs on a worker.
    #[must_use]
    pub fn fixed(
        left: PathBuf,
        right: PathBuf,
        context: &ViewContext,
        salt: u64,
        texts: (String, String),
    ) -> Self {
        let mut view = Self::blank(left, right, context, salt);
        view.locked = true;
        view.status = Status::Running("Comparing");
        view.left_syntax.set_path(&view.left_path);
        view.right_syntax.set_path(&view.right_path);
        view.left_pane = Pane::from_text(&texts.0);
        view.right_pane = Pane::from_text(&texts.1);
        view.left_pane.set_read_only(true);
        view.right_pane.set_read_only(true);
        view.job = Some(jobs::spawn_texts(
            texts.0,
            texts.1,
            jobs::SidePayload::default(),
            jobs::SidePayload::default(),
            view.compare_settings(),
            view.notify.clone(),
        ));
        view.started = Some(Instant::now());
        view
    }

    /// True when the view shows texts the caller supplied, read-only.
    #[must_use]
    pub const fn is_locked(&self) -> bool {
        self.locked
    }

    fn blank(left: PathBuf, right: PathBuf, context: &ViewContext, salt: u64) -> Self {
        Self {
            id: egui::Id::new(("text-compare", salt)),
            left_field: left.display().to_string(),
            right_field: right.display().to_string(),
            left_title: None,
            right_title: None,
            left_path: left,
            right_path: right,
            notify: context.notify.clone(),
            job: None,
            format_job: None,
            picker: None,
            picker_target: Target::Left,
            picker_saves: false,
            status: Status::Running("Starting"),
            data: TextData::default(),
            filter: DisplayFilter::All,
            context_lines: 2,
            visible: Visible::All(0),
            caret: 0,
            font: ca_ui::font::FontSize::new(DEFAULT_FONT_SIZE),
            line_spacing: 0,
            navigation: ca_session::options::NextDifferenceOptions::default(),
            find_from_word: ca_session::options::TextEditingOptions::default()
                .find_uses_current_word,
            syntax_on_differences: false,
            syntax_highlighting: true,
            column_line: None,
            dim_fraction: 0.0,
            painted_rulers: Vec::new(),
            painted_dim: None,
            scroll: RowScroll::top(),
            horizontal: 0.0,
            strip: Strip::default(),
            strip_stale: true,
            viewport_height: 0.0,
            viewport_rows: 0,
            left_pane: Pane::default(),
            right_pane: Pane::default(),
            left_source: None,
            right_source: None,
            left_stamp: None,
            right_stamp: None,
            active: Side::Left,
            rediff: Rediff::default(),
            loaded_generation: 0,
            find_task: FindTask::default(),
            find_side: Side::Left,
            find: FindPanel::with_settings(FindSettings {
                wrap: true,
                ..FindSettings::default()
            }),
            bookmarks: Bookmarks::new(),
            message: None,
            save_job: None,
            save_rules: ca_ui::save::SaveRules::default(),
            saving: None,
            saving_revision: None,
            saving_as: None,
            saving_consent: SaveConsent::default(),
            save_both_pending: false,
            prompt: None,
            close_requested: false,
            reread_after_save: None,
            pending_settings: None,
            line_cache: std::collections::HashMap::new(),
            revision: 0,
            cached_revision: (u64::MAX, u64::MAX, u64::MAX),
            left_syntax: syntax::Highlighter::default(),
            right_syntax: syntax::Highlighter::default(),
            left_edits: EditMarks::default(),
            right_edits: EditMarks::default(),
            session_settings: ca_session::settings::TextCompareSettings::default(),
            ignore_unimportant: false,
            show_details: true,
            show_line_numbers: true,
            details_horizontal: 0.0,
            started: None,
            load_time: None,
            toolbar_extent: (0.0, 0.0),
            arrows: Vec::new(),
            gutter_x: (0.0, 0.0, 0.0),
            body: egui::Rect::NOTHING,
            center_x: 0.0,
            input_pass: None,
            pending_clipboard: None,
            formatted_compare: None,
            paste_requested: false,
            first_difference_pending: false,
            report: ViewReport::new(
                ReportKind::Text,
                egui::Id::new(("text-compare", salt)),
                context.notify.clone(),
            ),
            locked: false,
            read_only: false,
        }
    }

    /// A view over a comparison the caller already holds, with nothing to read.
    #[must_use]
    pub fn from_data(
        left: PathBuf,
        right: PathBuf,
        context: &ViewContext,
        salt: u64,
        data: TextData,
    ) -> Self {
        let mut view = Self::blank(left, right, context, salt);
        view.left_syntax.set_path(&view.left_path);
        view.right_syntax.set_path(&view.right_path);
        let left = data.left.lines.join("\n");
        let right = data.right.lines.join("\n");
        view.data = data;
        view.left_pane = Pane::from_text(&left);
        view.right_pane = Pane::from_text(&right);
        view.status = Status::Ready;
        view.refilter();
        view.share_lines();
        view
    }

    /// The row layout currently on screen.
    #[must_use]
    pub fn model(&self) -> &model::RowModel {
        &self.data.model
    }

    /// The display filter in force.
    #[must_use]
    pub fn filter(&self) -> DisplayFilter {
        self.filter
    }

    /// True once both files are loaded and compared.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.status == Status::Ready
    }

    /// The scroll position, in rows.
    #[must_use]
    pub fn scroll_position(&self) -> RowScroll {
        self.scroll
    }

    /// The rectangle the rows were painted in on the last frame.
    #[must_use]
    pub const fn body_rect(&self) -> egui::Rect {
        self.body
    }

    /// The x coordinate that separates the two panes, from the last frame.
    #[must_use]
    pub const fn pane_split_x(&self) -> f32 {
        self.center_x
    }

    /// The row the row caret sits on.
    #[must_use]
    pub const fn current_row(&self) -> usize {
        self.caret
    }

    /// What the message panel is showing, where it shows anything.
    #[must_use]
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    /// The height of one row, in points.
    #[must_use]
    pub fn row_pixels(&self) -> f32 {
        self.row_height()
    }

    /// How many rows the overview strip reduces to pixels.
    #[must_use]
    pub fn strip_pixels(&self) -> usize {
        self.strip.buckets().len()
    }

    fn restart(&mut self) {
        if self.locked {
            return;
        }
        if let Some(job) = self.format_job.take() {
            job.cancel();
        }
        self.formatted_compare = None;
        if let Some(job) = self.job.take() {
            job.cancel();
        }
        // The load compares the files under the settings in force, which
        // covers every change a pending comparison was due to follow.
        self.rediff.clear();
        self.status = Status::Running("Starting");
        self.started = Some(Instant::now());
        self.load_time = None;
        self.data = TextData::default();
        self.visible = Visible::All(0);
        self.caret = 0;
        self.scroll = RowScroll::top();
        self.horizontal = 0.0;
        self.strip_stale = true;
        let format = &self.session_settings.format;
        self.left_syntax.set_format_override(
            settings::pinned_format(&format.left_format),
            &self.left_path,
        );
        self.right_syntax.set_format_override(
            settings::pinned_format(&format.right_format),
            &self.right_path,
        );
        self.left_syntax.set_path(&self.left_path);
        self.right_syntax.set_path(&self.right_path);
        self.job = Some(jobs::spawn(
            self.left_path.clone(),
            self.right_path.clone(),
            self.compare_settings(),
            self.notify.clone(),
        ));
    }

    /// The grammar and the rules a comparison runs under.
    ///
    /// The left file's format decides, so a pair of files with different names
    /// still compares under one rule set.
    fn compare_settings(&self) -> jobs::CompareSettings {
        let options = settings::options_from(&self.session_settings);
        let format = &self.session_settings.format;
        jobs::CompareSettings {
            grammar: self.left_syntax.format().grammar.clone(),
            styles: self.left_syntax.format().styles.clone(),
            rules: options.rules,
            compare: options.compare,
            replacements: options.replacements,
            anchors: options.anchors,
            left_decode: settings::decode_from(&format.left_encoding),
            right_decode: settings::decode_from(&format.right_encoding),
        }
    }

    /// The importance rules in force.
    #[must_use]
    pub fn rules(&self) -> jobs::RuleToggles {
        settings::rules_from(&self.session_settings.importance)
    }

    /// Set the importance rules and compare again under them.
    ///
    /// The rules are one part of the session's settings, so this writes them
    /// there rather than holding a second copy beside them.
    pub fn set_rules(&mut self, rules: jobs::RuleToggles) {
        if self.rules() == rules {
            return;
        }
        settings::write_rules(&mut self.session_settings.importance, rules);
        if self.structural_summary().is_none() {
            self.rediff.mark_stale(Instant::now());
        }
    }

    /// The settings of the session this view shows.
    #[must_use]
    pub fn session_settings(&self) -> &ca_session::settings::TextCompareSettings {
        &self.session_settings
    }

    /// Replace the settings and compare again under them.
    ///
    /// A change to the format group decides which bytes are read and which
    /// grammar reads them, so it reads both files again, which drops every
    /// edit. With an edit that is not written it asks first, as Reload does,
    /// and the settings apply after the answer. Every other change compares
    /// the text already in the panes again.
    pub fn apply_session_settings(&mut self, settings: ca_session::settings::TextCompareSettings) {
        if self.session_settings == settings {
            return;
        }
        if self.session_settings.format == settings.format {
            let mut compared = settings.clone();
            compared.specs.clone_from(&self.session_settings.specs);
            let compare_again = compared != self.session_settings;
            let switched =
                self.session_settings.specs.disable_editing != settings.specs.disable_editing;
            self.session_settings = settings;
            if switched {
                self.set_panes_editable();
            }
            if compare_again && self.structural_summary().is_none() {
                self.rediff.mark_stale(Instant::now());
            }
            return;
        }
        self.pending_settings = Some(Box::new(settings));
        self.request_reread(Reread::Settings);
    }

    /// True when no pane may take an edit and no save may run: the files are
    /// copies the tab owns, or the editing switch of the session is on.
    fn editing_off(&self) -> bool {
        self.read_only
            || self.session_settings.specs.disable_editing
            || self.formatted_compare.is_some()
            || self.format_job.is_some()
    }

    fn start_prettify(&mut self) {
        self.start_projection(false);
    }

    #[allow(clippy::too_many_lines)]
    fn start_projection(&mut self, structural: bool) {
        let Some(format) = prettify::pair_format(&self.left_path, &self.right_path) else {
            return;
        };
        if self.left_pane.is_modified() || self.right_pane.is_modified() {
            self.message =
                Some("Save or discard pane edits before formatting for comparison.".into());
            return;
        }
        let structural_format = structural
            .then(|| structure::pair_format(&self.left_path, &self.right_path))
            .flatten();
        if structural
            && (structural_format.is_none()
                || self.data.left.had_errors
                || self.data.right.had_errors)
        {
            self.message = Some(
                "Structure comparison requires two JSON or two XML files without decoding errors."
                    .into(),
            );
            return;
        }
        let left = TextBuffer::from_rope(self.left_pane.buffer().rope().clone());
        let right = TextBuffer::from_rope(self.right_pane.buffer().rope().clone());
        let left_lines = self.left_syntax.lines();
        let right_lines = self.right_syntax.lines();
        let notify = self.notify.clone();
        self.format_job = Some(Job::spawn_notifying(
            move |emitter, cancel| {
                let original_left = left;
                let original_right = right;
                let original_left_text = original_left.text();
                let original_right_text = original_right.text();
                let comparison = if let Some(format) = structural_format {
                    match structure::compare(
                        &original_left_text,
                        &original_right_text,
                        format,
                        &cancel,
                    ) {
                        Ok(comparison) => Some(comparison),
                        Err(error) => {
                            if !cancel.is_cancelled() {
                                emitter.send(FormatMessage::Refused(error));
                            }
                            return;
                        }
                    }
                } else {
                    None
                };
                let formatted_left_text = match comparison.as_ref().map_or_else(
                    || prettify::format_cancellable(&original_left_text, format, &cancel),
                    |result| Ok(result.left_text.clone()),
                ) {
                    Ok(text) => text,
                    Err(error) => {
                        if cancel.is_cancelled() {
                            return;
                        }
                        emitter.send(FormatMessage::Failed(format!("left side: {error}")));
                        return;
                    }
                };
                if cancel.is_cancelled() {
                    return;
                }
                let formatted_left = TextBuffer::from_text(&formatted_left_text);
                let formatted_left_lines = match prepared_lines(&formatted_left_text, &cancel) {
                    Ok(lines) => lines,
                    Err(_error) if cancel.is_cancelled() => return,
                    Err(error) => {
                        emitter.send(FormatMessage::Failed(format!("left side: {error}")));
                        return;
                    }
                };
                let formatted_right_text = match comparison.as_ref().map_or_else(
                    || prettify::format_cancellable(&original_right_text, format, &cancel),
                    |result| Ok(result.right_text.clone()),
                ) {
                    Ok(text) => text,
                    Err(error) => {
                        if cancel.is_cancelled() {
                            return;
                        }
                        emitter.send(FormatMessage::Failed(format!("right side: {error}")));
                        return;
                    }
                };
                if cancel.is_cancelled() {
                    return;
                }
                let formatted_right = TextBuffer::from_text(&formatted_right_text);
                let formatted_right_lines = match prepared_lines(&formatted_right_text, &cancel) {
                    Ok(lines) => lines,
                    Err(_error) if cancel.is_cancelled() => return,
                    Err(error) => {
                        emitter.send(FormatMessage::Failed(format!("right side: {error}")));
                        return;
                    }
                };
                if cancel.is_cancelled() {
                    return;
                }
                let (comparison, summary) = comparison.map_or((None, None), |result| {
                    (Some(Box::new(result.data)), Some(result.summary))
                });
                emitter.send(FormatMessage::Ready {
                    comparison,
                    summary,
                    original: Box::new(PreparedBuffers {
                        left: original_left,
                        right: original_right,
                        left_text: original_left_text,
                        right_text: original_right_text,
                        left_lines,
                        right_lines,
                    }),
                    formatted: Box::new(PreparedBuffers {
                        left: formatted_left,
                        right: formatted_right,
                        left_text: formatted_left_text,
                        right_text: formatted_right_text,
                        left_lines: formatted_left_lines,
                        right_lines: formatted_right_lines,
                    }),
                });
            },
            notify,
        ));
        self.set_panes_editable();
        self.status = Status::Running(if structural {
            "Comparing structure"
        } else {
            "Formatting structured text"
        });
        self.message = None;
    }

    fn poll_format(&mut self) {
        let Some(job) = self.format_job.as_mut() else {
            return;
        };
        let messages = job.drain();
        let finished =
            job.is_finished() || messages.iter().any(ca_ui::worker::Terminal::is_terminal);
        if finished {
            self.format_job = None;
        }
        for message in messages {
            match message {
                FormatMessage::Ready {
                    mut original,
                    formatted,
                    comparison,
                    summary,
                } => {
                    // Keep the actual live histories for restoration. The job
                    // shared only the ropes, without copying deleted text.
                    std::mem::swap(&mut original.left, self.left_pane.buffer_mut());
                    std::mem::swap(&mut original.right, self.right_pane.buffer_mut());
                    self.formatted_compare = Some(FormattedCompare {
                        original: *original,
                        summary,
                    });
                    self.install_projection_buffers(*formatted, comparison);
                    self.message = Some(if summary.is_some() {
                        "Source files are unchanged. Use Compare Structure again to restore the original text.".into()
                    } else {
                        "Formatted for comparison; source files are unchanged. Use Prettify for Comparison again to restore the original text.".into()
                    });
                }
                FormatMessage::Failed(reason) => {
                    self.status = Status::Ready;
                    self.set_panes_editable();
                    self.message = Some(format!("Could not format for comparison: {reason}"));
                }
                FormatMessage::Refused(reason) => {
                    self.status = Status::Ready;
                    self.set_panes_editable();
                    self.message = Some(format!(
                        "Structure comparison refused; showing the line comparison: {reason}"
                    ));
                }
                FormatMessage::Cancelled => {
                    self.status = Status::Ready;
                    self.set_panes_editable();
                    self.message = Some("Formatting stopped; the source text is unchanged.".into());
                }
            }
        }
    }

    fn toggle_prettified_compare(&mut self) {
        if let Some(original) = self.formatted_compare.take() {
            self.install_comparison_buffers(original.original);
            self.message = Some("Showing the original text; source files are unchanged.".into());
        } else {
            self.start_prettify();
        }
    }

    /// Counts describe the entire structural comparison before display filters.
    #[must_use]
    pub fn structural_summary(&self) -> Option<structure::Summary> {
        self.formatted_compare
            .as_ref()
            .and_then(|projection| projection.summary)
    }

    fn toggle_structure(&mut self) {
        if self.formatted_compare.is_some() {
            self.toggle_prettified_compare();
        } else {
            self.start_projection(true);
        }
    }

    fn install_comparison_buffers(&mut self, buffers: PreparedBuffers) {
        self.install_projection_buffers(buffers, None);
    }

    fn install_projection_buffers(
        &mut self,
        buffers: PreparedBuffers,
        comparison: Option<Box<TextData>>,
    ) {
        self.rediff.clear();
        self.left_pane.reset(buffers.left);
        self.right_pane.reset(buffers.right);
        self.set_panes_editable();
        self.left_syntax.set_lines(buffers.left_lines);
        self.right_syntax.set_lines(buffers.right_lines);
        self.line_cache.clear();
        self.left_edits = EditMarks::default();
        self.right_edits = EditMarks::default();
        self.bookmarks.clear();
        self.caret = 0;
        self.scroll = RowScroll::top();
        self.horizontal = 0.0;
        self.strip_stale = true;
        self.revision = self.revision.saturating_add(1);
        self.cached_revision = (u64::MAX, u64::MAX, u64::MAX);
        let left_payload = Self::side_facts(&self.data.left);
        let right_payload = Self::side_facts(&self.data.right);
        self.status = Status::Ready;
        if let Some(mut comparison) = comparison {
            comparison.left = jobs::SidePayload {
                lines: std::mem::take(&mut comparison.left.lines),
                metrics: std::mem::take(&mut comparison.left.metrics),
                ..left_payload
            };
            comparison.right = jobs::SidePayload {
                lines: std::mem::take(&mut comparison.right.lines),
                metrics: std::mem::take(&mut comparison.right.metrics),
                ..right_payload
            };
            self.install(*comparison, false);
            return;
        }
        self.rediff.start(jobs::spawn_texts(
            buffers.left_text,
            buffers.right_text,
            left_payload,
            right_payload,
            self.compare_settings(),
            self.notify.clone(),
        ));
    }

    /// Give both panes the editing state the view is under. A load gives it
    /// again to the panes it fills. A pane a failed save made read-only takes
    /// edits again only when the editing switch changes.
    fn set_panes_editable(&mut self) {
        if self.locked {
            return;
        }
        let off = self.editing_off();
        self.left_pane.set_read_only(off);
        self.right_pane.set_read_only(off);
    }

    /// True when the view shows a finished comparison and none is pending.
    ///
    /// A settings change schedules the next comparison rather than running it,
    /// so a ready status alone still describes the previous run.
    #[must_use]
    pub fn is_settled(&self) -> bool {
        self.is_ready()
            && self.job.is_none()
            && !self.rediff.is_running()
            && !self.rediff.is_stale()
    }

    /// The colored runs of one line, for a test that checks what a frame paints.
    pub fn syntax_spans(&mut self, side: Side, line: u32) -> Option<Arc<[syntax::Span]>> {
        let text = self.pane(side).line_text(line);
        match side {
            Side::Left => self.left_syntax.spans(line, &text),
            Side::Right => self.right_syntax.spans(line, &text),
        }
    }

    /// True once both panes' carried lexer states describe their whole text.
    #[must_use]
    pub const fn syntax_converged(&self) -> bool {
        self.left_syntax.is_converged() && self.right_syntax.is_converged()
    }

    /// The name of the format the left file's name selected.
    #[must_use]
    pub fn format_name(&self) -> &str {
        &self.left_syntax.format().name
    }

    /// Take everything the background work has posted.
    ///
    /// Runs every frame for every tab, active or not, so a comparison finishing
    /// behind another tab still reaches a terminal state and stops holding its
    /// queue.
    fn poll(&mut self) {
        self.poll_format();
        self.poll_comparison();
        self.poll_picker();
    }

    fn poll_comparison(&mut self) {
        let Some(job) = self.job.as_mut() else {
            return;
        };
        let messages = job.drain();
        let mut finished = job.is_finished();
        for message in messages {
            finished |= ca_ui::worker::Terminal::is_terminal(&message);
            match message {
                TextMessage::Progress(step) => self.status = Status::Running(step),
                TextMessage::Failed(reason) => self.status = Status::Failed(reason),
                TextMessage::Cancelled => self.status = Status::Cancelled,
                TextMessage::Ready(data) => {
                    self.install(*data, true);
                }
            }
        }
        if finished {
            self.job = None;
            // The worker layer posts a terminal message on every exit path, so
            // this only covers a stream that ended without one reaching here.
            if self.status.is_running() {
                self.status = Status::Cancelled;
            }
        }
    }

    fn poll_picker(&mut self) {
        let Some(job) = self.picker.as_mut() else {
            return;
        };
        let messages = job.drain();
        let finished = job.is_finished();
        let mut reload = false;
        let mut chosen_name: Option<PathBuf> = None;
        for message in messages {
            match message {
                DialogMessage::Chosen(path) => {
                    let text = path.display().to_string();
                    if self.picker_saves {
                        chosen_name = Some(path);
                    } else {
                        match self.picker_target {
                            Target::Left => {
                                self.left_field = text;
                                self.left_title = None;
                            }
                            Target::Right => {
                                self.right_field = text;
                                self.right_title = None;
                            }
                        }
                        reload = true;
                    }
                }
                DialogMessage::Dismissed => {}
                DialogMessage::Failed(reason) => self.status = Status::Failed(reason),
            }
        }
        if finished {
            self.picker = None;
        }
        if reload {
            self.request_reread(Reread::Open);
        }
        if let Some(path) = chosen_name {
            self.save_active_as(&path);
        }
    }

    /// Write the active pane under a new name.
    ///
    /// The pane takes the new name and the format that name selects, and the
    /// write goes through the same worker and the same checks a save does. The
    /// recorded state of the old file is dropped, because the new name was
    /// never read.
    pub fn save_active_as(&mut self, path: &Path) {
        if self.save_job.is_some() {
            self.message = Some("A save is still running. Try Save As again.".to_owned());
            return;
        }
        self.save_side_to(
            self.active,
            SaveConsent::default(),
            Some(path.to_path_buf()),
        );
    }

    fn apply_fields(&mut self) {
        self.left_path = PathBuf::from(self.left_field.clone());
        self.right_path = PathBuf::from(self.right_field.clone());
        self.restart();
    }

    fn open_picker(&mut self, target: Target) {
        if self.locked || self.picker.is_some() {
            return;
        }
        self.picker_target = target;
        self.picker_saves = false;
        self.picker = Some(dialog::spawn(Pick::File, self.notify.clone()));
    }

    /// Raise the panel that names the file the active pane is written to.
    fn open_save_picker(&mut self) {
        if self.picker.is_some() {
            return;
        }
        self.picker_saves = true;
        self.picker = Some(dialog::spawn(Pick::SaveFile, self.notify.clone()));
    }

    fn first_difference(&self) -> Option<usize> {
        if self
            .data
            .model
            .row(0)
            .is_some_and(|row| row.class.is_difference())
        {
            return Some(0);
        }
        self.data.model.next_difference(0)
    }

    fn refilter(&mut self) {
        let filter = match self.filter {
            DisplayFilter::Context(_) => DisplayFilter::Context(self.context_lines),
            other => other,
        };
        self.visible = self.data.model.visible(filter);
        self.strip_stale = true;
        self.scroll
            .clamp(self.viewport_height, self.row_height(), self.visible.len());
    }

    fn set_filter(&mut self, filter: DisplayFilter) {
        self.filter = filter;
        self.refilter();
        self.go_to_row(self.caret);
    }

    fn row_height(&self) -> f32 {
        self.font.row_height(ROW_HEIGHT_RATIO, self.line_spacing)
    }

    /// The point size the panes draw at.
    fn font_size(&self) -> f32 {
        self.font.points()
    }

    /// Take what the options document now states.
    fn follow_options(&mut self, ctx: &egui::Context) {
        let options = ca_ui::options::current(ctx);
        let before = (self.font.points(), self.line_spacing);
        self.font.follow(options.editor_point_size());
        self.line_spacing = options.extra_line_spacing();
        self.navigation = options.stored.next_difference.clone();
        self.find_from_word = options.stored.text_editing.find_uses_current_word;
        self.syntax_on_differences = options
            .stored
            .tweaks
            .syntax_highlighting_on_difference_lines;
        self.column_line = options.column_line_at();
        self.dim_fraction = options.dim_inactive_pane();
        self.save_rules = ca_ui::save::SaveRules::from_options(&options.stored);
        if before != (self.font.points(), self.line_spacing) {
            self.strip_stale = true;
        }
    }

    fn go_to_row(&mut self, row: usize) {
        self.caret = row.min(self.data.model.row_count().saturating_sub(1));
        let Some(position) = self.visible.position_of(self.caret) else {
            return;
        };
        self.scroll.reveal(
            position,
            self.viewport_height,
            self.row_height(),
            self.visible.len(),
        );
    }

    fn scroll_to_position(&mut self, position: usize) {
        self.scroll.go_to(
            position,
            self.viewport_height,
            self.row_height(),
            self.visible.len(),
        );
        if let Some(row) = self.visible.row_at(position) {
            self.caret = row;
        }
    }

    /// Move to the next or the previous difference.
    ///
    /// Where nothing lies in that direction the Next Difference page decides
    /// what happens: the move continues from the other end, or it stops and the
    /// message panel says why.
    fn navigate(&mut self, command: Command) {
        let model = &self.data.model;
        let target = match command {
            Command::NextDifference => model.next_difference(self.caret),
            Command::PreviousDifference => model.previous_difference(self.caret),
            Command::NextSection => model.next_section(self.caret),
            Command::PreviousSection => model.previous_section(self.caret),
            _ => None,
        };
        if let Some(row) = target {
            self.go_to_row(row);
            return;
        }
        if self.navigation.wrap_around {
            let wrapped = match command {
                Command::NextDifference => model.first_difference(),
                Command::PreviousDifference => model.last_difference(),
                Command::NextSection => model.first_section(),
                Command::PreviousSection => model.last_section(),
                _ => None,
            };
            if let Some(row) = wrapped {
                self.go_to_row(row);
                return;
            }
        }
        if self.navigation.show_message_panel {
            self.message = Some(LAST_DIFFERENCE.to_owned());
        }
    }

    fn jump_to_first_difference_once(&mut self) {
        if !std::mem::take(&mut self.first_difference_pending) {
            return;
        }
        if self.navigation.go_to_first_difference_on_load {
            if let Some(first) = self.first_difference() {
                self.go_to_row(first);
                self.place_carets_on_row(first);
            }
        }
    }

    /// Install a finished comparison.
    ///
    /// A comparison that came from disk also replaces the buffers; one that
    /// followed an edit replaces only the alignment, so the text the user is
    /// typing into is never rolled back.
    fn install(&mut self, data: TextData, keep_carets: bool) {
        let mut data = data;
        data.model.set_ignore_unimportant(self.ignore_unimportant);
        if let Some(sources) = data.sources.take() {
            if let Some(generation) = self.rediff.generation() {
                self.loaded_generation = generation;
            }
            if let Some(started) = self.started.take() {
                self.load_time = Some(started.elapsed());
            }
            self.left_pane.reset(sources.left.buffer.clone());
            self.right_pane.reset(sources.right.buffer.clone());
            self.left_stamp = sources.left_stamp;
            self.right_stamp = sources.right_stamp;
            self.left_source = Some(sources.left);
            self.right_source = Some(sources.right);
            let off = self.editing_off();
            self.left_pane.set_read_only(off);
            self.right_pane.set_read_only(off);
            self.data = data;
            self.status = Status::Ready;
            self.refilter();
            self.share_lines();
            self.first_difference_pending = true;
            return;
        }
        if self.locked {
            if let Some(started) = self.started.take() {
                self.load_time = Some(started.elapsed());
                self.first_difference_pending = true;
            }
        }
        self.data = data;
        self.status = Status::Ready;
        self.refilter();
        self.share_lines();
        if keep_carets {
            self.follow_caret();
        }
    }

    /// Hand both highlighters the text the carried lexer states describe.
    fn share_lines(&mut self) {
        self.left_syntax
            .set_lines(Arc::clone(&self.data.left.lines));
        self.right_syntax
            .set_lines(Arc::clone(&self.data.right.lines));
    }

    /// One pane.
    #[must_use]
    pub const fn pane(&self, side: Side) -> &Pane {
        match side {
            Side::Left => &self.left_pane,
            Side::Right => &self.right_pane,
        }
    }

    fn pane_mut(&mut self, side: Side) -> &mut Pane {
        match side {
            Side::Left => &mut self.left_pane,
            Side::Right => &mut self.right_pane,
        }
    }

    /// The pane the caret is in.
    #[must_use]
    pub const fn active_side(&self) -> Side {
        self.active
    }

    /// Put the selection of the active pane, or the word under its caret, in
    /// the find strip when the options ask for it. A selection over several
    /// lines is left out, because the strip holds one line.
    fn seed_find(&mut self) {
        if !self.find_from_word {
            return;
        }
        let pane = self.active_pane();
        let seed = pane
            .selected_text()
            .filter(|text| !text.contains(['\n', '\r']))
            .or_else(|| pane.word_at_caret());
        if let Some(seed) = seed {
            self.find.settings.pattern = seed;
        }
    }

    /// The horizontal positions the last frame drew the column ruler at, one
    /// for each pane that shows the ruler's column.
    #[must_use]
    pub fn painted_rulers(&self) -> &[f32] {
        &self.painted_rulers
    }

    /// The pane the last frame darkened because it had no focus.
    #[must_use]
    pub const fn dimmed_pane(&self) -> Option<Side> {
        self.painted_dim
    }

    /// The text the find strip searches for.
    #[must_use]
    pub fn find_pattern(&self) -> &str {
        &self.find.settings.pattern
    }

    /// Set the text the find panel searches for.
    pub fn set_find_pattern(&mut self, pattern: &str) {
        pattern.clone_into(&mut self.find.settings.pattern);
    }

    /// Record the text of both panes as written.
    pub fn mark_saved(&mut self) {
        self.left_pane.mark_saved();
        self.right_pane.mark_saved();
    }

    fn active_pane(&self) -> &Pane {
        self.pane(self.active)
    }

    fn active_pane_mut(&mut self) -> &mut Pane {
        let side = self.active;
        self.pane_mut(side)
    }

    /// The line terminator a side writes.
    fn ending_of(&self, side: Side) -> &'static str {
        let label = match side {
            Side::Left => self.data.left.eol.as_str(),
            Side::Right => self.data.right.eol.as_str(),
        };
        match label {
            "Windows" => EolStyle::CrLf.as_str(),
            "Mac" => EolStyle::Cr.as_str(),
            _ => EolStyle::Lf.as_str(),
        }
    }

    /// The row showing the active pane's caret line.
    fn caret_row(&self) -> Option<usize> {
        let line = self.active_pane().caret().line;
        match self.active {
            Side::Left => self.data.model.row_of_left_line(line),
            Side::Right => self.data.model.row_of_right_line(line),
        }
    }

    /// Put the row caret back on the line the text caret is on.
    fn follow_caret(&mut self) {
        if let Some(row) = self.caret_row() {
            self.caret = row;
        }
        self.scroll
            .clamp(self.viewport_height, self.row_height(), self.visible.len());
    }

    /// Put both text carets on the lines a row shows.
    fn place_carets_on_row(&mut self, row: usize) {
        let Some(entry) = self.data.model.row(row).copied() else {
            return;
        };
        if let Some(line) = entry.left {
            self.left_pane.place(editor::Caret::new(line, 0), false);
        }
        if let Some(line) = entry.right {
            self.right_pane.place(editor::Caret::new(line, 0), false);
        }
    }

    /// The difference section the caret is in, or the next one.
    fn current_section_rows(&self) -> Option<std::ops::Range<usize>> {
        sidecopy::section_rows(&self.data.model, self.caret).or_else(|| {
            let next = self.data.model.next_difference(self.caret)?;
            sidecopy::section_rows(&self.data.model, next)
        })
    }

    /// Bring the cache of visible lines up to date.
    ///
    /// Every entry it holds is a row the viewport covers, so its size follows
    /// the window and its cost is the lines that just came into view.
    fn refresh_line_cache(&mut self, range: &std::ops::Range<usize>) {
        let stamp = (
            self.revision,
            self.left_syntax.revision(),
            self.right_syntax.revision(),
        );
        if self.cached_revision != stamp {
            self.line_cache.clear();
            self.cached_revision = stamp;
        }
        let mut wanted: Vec<(u8, u32)> = Vec::with_capacity(range.len() * 2);
        for position in range.clone() {
            let Some(index) = self.visible.row_at(position) else {
                continue;
            };
            let Some(row) = self.data.model.row(index) else {
                continue;
            };
            if let Some(line) = row.left {
                wanted.push((0, line));
            }
            if let Some(line) = row.right {
                wanted.push((1, line));
            }
        }
        self.line_cache.retain(|key, _| wanted.contains(key));
        for key in &wanted {
            let key = *key;
            if self.line_cache.contains_key(&key) {
                continue;
            }
            let left = key.0 == 0;
            let text = if left {
                self.left_pane.line_text(key.1)
            } else {
                self.right_pane.line_text(key.1)
            };
            let metrics = lines::measure(&text);
            // A file no named format claims carries no syntax runs, so its text
            // takes the pane's own color.
            let spans = if !self.syntax_highlighting {
                None
            } else if left {
                self.left_syntax
                    .is_claimed()
                    .then(|| self.left_syntax.spans(key.1, &text))
                    .flatten()
            } else {
                self.right_syntax
                    .is_claimed()
                    .then(|| self.right_syntax.spans(key.1, &text))
                    .flatten()
            };
            self.line_cache.insert(
                key,
                CachedLine {
                    text,
                    metrics,
                    spans,
                },
            );
        }
        let left_lines: Vec<u32> = wanted
            .iter()
            .filter(|key| key.0 == 0)
            .map(|key| key.1)
            .collect();
        let right_lines: Vec<u32> = wanted
            .iter()
            .filter(|key| key.0 == 1)
            .map(|key| key.1)
            .collect();
        self.left_syntax.retain_lines(&left_lines);
        self.right_syntax.retain_lines(&right_lines);
    }

    fn cached_line(&self, side: Side, line: Option<u32>) -> Option<&CachedLine> {
        let key = (u8::from(side == Side::Right), line?);
        self.line_cache.get(&key)
    }

    /// Record that an edit made the comparison stale.
    fn note_edit(&mut self) {
        self.revision = self.revision.saturating_add(1);
        self.rediff.mark_stale(Instant::now());
        for span in self.left_pane.take_changes() {
            self.bookmarks.shift(
                span.start_line as usize,
                span.removed_lines as usize,
                span.inserted_lines as usize,
            );
            self.left_syntax.note_edit(span);
            self.left_edits.note(span);
        }
        for span in self.right_pane.take_changes() {
            self.bookmarks.shift(
                span.start_line as usize,
                span.removed_lines as usize,
                span.inserted_lines as usize,
            );
            self.right_syntax.note_edit(span);
            self.right_edits.note(span);
        }
    }

    fn side_facts(payload: &jobs::SidePayload) -> jobs::SidePayload {
        jobs::SidePayload {
            lines: Arc::new(Vec::new()),
            metrics: Vec::new(),
            encoding: payload.encoding.clone(),
            eol: payload.eol.clone(),
            mixed_eol: payload.mixed_eol,
            had_errors: payload.had_errors,
        }
    }

    /// Start the comparison an edit made due.
    fn start_rediff(&mut self) {
        if self.structural_summary().is_some() {
            self.rediff.clear();
            return;
        }
        let job = jobs::spawn_texts(
            self.left_pane.buffer().text(),
            self.right_pane.buffer().text(),
            Self::side_facts(&self.data.left),
            Self::side_facts(&self.data.right),
            self.compare_settings(),
            self.notify.clone(),
        );
        self.rediff.start(job);
    }

    fn poll_rediff(&mut self) {
        if self
            .rediff
            .generation()
            .is_some_and(|generation| generation <= self.loaded_generation)
        {
            self.rediff.finish();
            self.rediff.mark_stale(Instant::now());
            return;
        }
        let Some(job) = self.rediff.job_mut() else {
            return;
        };
        let messages = job.drain();
        let mut finished = job.is_finished();
        for message in messages {
            finished |= ca_ui::worker::Terminal::is_terminal(&message);
            match message {
                TextMessage::Ready(data) => self.install(*data, true),
                TextMessage::Failed(reason) => self.status = Status::Failed(reason),
                TextMessage::Progress(_) | TextMessage::Cancelled => {}
            }
        }
        if finished {
            self.rediff.finish();
        }
    }

    fn poll_save(&mut self) {
        let Some(job) = self.save_job.as_mut() else {
            return;
        };
        let messages = job.drain();
        let finished = job.is_finished();
        if finished || !messages.is_empty() {
            self.save_job = None;
        }
        for message in messages {
            if let SaveMessage::Done(outcome) = message {
                self.apply_save_outcome(*outcome);
            } else {
                self.saving = None;
                self.saving_as = None;
                self.save_both_pending = false;
                self.forget_reread();
            }
        }
    }

    fn apply_save_outcome(&mut self, outcome: SaveOutcome) {
        let Some(side) = self.saving else {
            return;
        };
        let saving_as = self.saving_as.take();
        let conflict_backup = match &outcome {
            SaveOutcome::SavedWithConflict { backup, .. } => Some(backup.display().to_string()),
            _ => None,
        };
        match outcome {
            SaveOutcome::Saved(stamp) | SaveOutcome::SavedWithConflict { stamp, .. } => {
                if let Some(path) = saving_as {
                    let text = path.display().to_string();
                    match side {
                        Side::Left => {
                            self.left_path.clone_from(&path);
                            self.left_field = text;
                            self.left_title = None;
                            self.left_syntax.set_path(&path);
                            self.rediff.mark_stale(Instant::now());
                        }
                        Side::Right => {
                            self.right_path.clone_from(&path);
                            self.right_field = text;
                            self.right_title = None;
                            self.right_syntax.set_path(&path);
                        }
                    }
                    let read_only = self.editing_off();
                    self.pane_mut(side).set_read_only(read_only);
                }
                match side {
                    Side::Left => self.left_stamp = Some(stamp),
                    Side::Right => self.right_stamp = Some(stamp),
                }
                if let Some(revision) = self.saving_revision.take() {
                    self.pane_mut(side)
                        .buffer_mut()
                        .mark_saved_revision(revision);
                }
                self.message = Some(conflict_backup.as_ref().map_or_else(
                    || "Saved.".to_owned(),
                    |backup| format!("Saved, but another version was kept at {backup}. Review it."),
                ));
                self.saving = None;
                if let Some(backup) = conflict_backup {
                    self.prompt = Some(Prompt::KeptCopy(backup));
                    self.save_both_pending = false;
                    self.close_requested = false;
                    self.forget_reread();
                } else {
                    if self.save_both_pending {
                        self.save_both();
                    }
                    self.reread_when_saved();
                }
            }
            SaveOutcome::ChangedOnDisk => {
                self.saving_as = saving_as;
                self.prompt = Some(Prompt::DiskChanged(side));
            }
            SaveOutcome::WouldLose(reason) => {
                self.saving_as = saving_as;
                self.prompt = Some(Prompt::WouldLose(side, reason));
            }
            SaveOutcome::NotWritable => {
                self.pane_mut(side).set_read_only(true);
                self.message = Some("The file cannot be written, so the pane is read only.".into());
                self.saving = None;
                self.save_both_pending = false;
                self.forget_reread();
            }
            SaveOutcome::Failed(reason) => {
                self.message = Some(format!("The file was not written: {reason}"));
                self.saving = None;
                self.save_both_pending = false;
                self.forget_reread();
            }
        }
    }

    /// Start writing one side.
    fn save_side(&mut self, side: Side, consent: SaveConsent) {
        self.save_side_to(side, consent, None);
    }

    fn save_side_to(&mut self, side: Side, consent: SaveConsent, save_as: Option<PathBuf>) {
        if self.editing_off() {
            self.message = Some("Editing is turned off for this session".to_owned());
            self.save_both_pending = false;
            return;
        }
        if matches!(self.prompt, Some(Prompt::KeptCopy(_))) {
            return;
        }
        if self.save_job.is_some() {
            return;
        }
        let path = save_as.clone().unwrap_or_else(|| match side {
            Side::Left => self.left_path.clone(),
            Side::Right => self.right_path.clone(),
        });
        let Some(source) = (match side {
            Side::Left => self.left_source.clone(),
            Side::Right => self.right_source.clone(),
        }) else {
            return;
        };
        let mut source = source;
        source.buffer = self.pane(side).buffer().clone();
        let stamp = match side {
            Side::Left => self.left_stamp,
            Side::Right => self.right_stamp,
        };
        let expected = if save_as.is_some() {
            Baseline::Unchecked
        } else {
            stamp.into()
        };
        self.saving = Some(side);
        self.saving_as = save_as;
        self.saving_revision = Some(source.buffer.revision());
        self.saving_consent = consent;
        self.prompt = None;
        self.save_job = Some(save::spawn(
            path,
            source,
            expected,
            consent,
            self.save_rules.clone(),
            self.notify.clone(),
        ));
    }

    /// Copy the selection, or the current difference section, to `to`.
    fn copy_to(&mut self, to: Side, line_only: bool) {
        if self.pane(to).is_read_only() {
            return;
        }
        let from = to.other();
        let chosen = if line_only {
            self.line_rows(from)
        } else {
            self.selected_rows(from)
                .or_else(|| self.current_section_rows())
        };
        let Some(rows) = chosen else {
            return;
        };
        let Some(plan) = sidecopy::plan(&self.data.model, rows, from) else {
            return;
        };
        let ending = self.ending_of(to).to_owned();
        let (left, right) = (&mut self.left_pane, &mut self.right_pane);
        match to {
            Side::Left => sidecopy::apply(left, right, &plan, &ending),
            Side::Right => sidecopy::apply(right, left, &plan, &ending),
        }
        // The pane that changed becomes the active one, so an undo straight
        // after a copy reverses that copy.
        self.active = to;
        self.note_edit();
        if self.navigation.go_to_next_after_copy {
            self.navigate(Command::NextDifference);
        }
    }

    /// The rows the caret's line occupies on one side.
    fn line_rows(&self, side: Side) -> Option<std::ops::Range<usize>> {
        let line = self.pane(side).caret().line;
        let row = match side {
            Side::Left => self.data.model.row_of_left_line(line),
            Side::Right => self.data.model.row_of_right_line(line),
        }?;
        Some(row..row + 1)
    }

    /// The rows one side's selection covers, when it has one.
    fn selected_rows(&self, side: Side) -> Option<std::ops::Range<usize>> {
        let pane = self.pane(side);
        pane.selection()?;
        let lines = pane.selected_lines();
        let model = &self.data.model;
        let first = (lines.start..lines.end).find_map(|line| match side {
            Side::Left => model.row_of_left_line(line),
            Side::Right => model.row_of_right_line(line),
        })?;
        let last = (lines.start..lines.end).rev().find_map(|line| match side {
            Side::Left => model.row_of_left_line(line),
            Side::Right => model.row_of_right_line(line),
        })?;
        Some(first..last + 1)
    }

    /// Select every line of the difference section the caret is in.
    fn select_section(&mut self) {
        let Some(rows) = self.current_section_rows() else {
            return;
        };
        let side = self.active;
        let model = &self.data.model;
        let lines: Vec<u32> = rows
            .clone()
            .filter_map(|index| model.row(index).and_then(|row| side.line_of(row)))
            .collect();
        let (Some(first), Some(last)) = (lines.first().copied(), lines.last().copied()) else {
            return;
        };
        self.pane_mut(side)
            .select_lines(ca_text::LineRange::new(first, last + 1));
    }

    /// True when both panes refuse edits.
    fn editing_blocked(&self) -> bool {
        self.status != Status::Ready
    }

    fn run_find(&mut self, backwards: bool) {
        self.start_find(FindOperation::Next(backwards));
    }

    fn run_replace_all(&mut self) {
        self.start_find(FindOperation::ReplaceAll);
    }

    fn start_find(&mut self, operation: FindOperation) {
        if self.status != Status::Ready {
            return;
        }
        self.find_side = self.active;
        let pane = match self.active {
            Side::Left => &self.left_pane,
            Side::Right => &self.right_pane,
        };
        self.find_task.start(
            pane,
            &self.find.settings,
            operation,
            Arc::clone(&self.notify),
        );
        self.message = Some("Searching…".to_owned());
    }

    fn poll_find(&mut self) {
        if self.active != self.find_side || self.status != Status::Ready {
            if self.find_task.is_running() {
                self.find_task.cancel();
                self.message =
                    Some("Search cancelled because the active comparison changed.".to_owned());
            }
            return;
        }
        let pane = match self.active {
            Side::Left => &mut self.left_pane,
            Side::Right => &mut self.right_pane,
        };
        if let Some(done) = self.find_task.poll(pane, &self.find.settings) {
            self.message = done.message;
            if done.edited {
                self.note_edit();
            }
            if done.selected {
                self.follow_caret();
                self.go_to_row(self.caret);
            }
        }
    }

    fn path_bar(&mut self, ui: &mut egui::Ui) {
        if self.locked {
            ui.horizontal(|ui| {
                ui.monospace(
                    self.left_title
                        .as_deref()
                        .filter(|title| !title.is_empty())
                        .unwrap_or(&self.left_field),
                );
                ui.separator();
                ui.monospace(
                    self.right_title
                        .as_deref()
                        .filter(|title| !title.is_empty())
                        .unwrap_or(&self.right_field),
                );
            });
            return;
        }
        let action = widgets::path_bar_with_titles(
            ui,
            &mut self.left_field,
            &mut self.right_field,
            self.left_title.as_deref(),
            self.right_title.as_deref(),
            "Reload",
        );
        match action {
            Some(widgets::PathBarAction::Browse(target)) => self.open_picker(target),
            Some(widgets::PathBarAction::Reload) => self.request_reread(Reread::Open),
            None => {}
        }
    }

    /// The toolbar items this view declares, in the state it is in now.
    #[must_use]
    pub fn toolbar_items(&self) -> Vec<toolbar::Item> {
        let ready = self.status == Status::Ready;
        vec![
            toolbar::Item::widget("home", 70.0),
            toolbar::Item::widget("sessions", 90.0),
            toolbar::Item::separator("separator-1"),
            toolbar::Item::widget("all", 46.0),
            toolbar::Item::widget("diffs", 56.0),
            toolbar::Item::widget("same", 56.0),
            toolbar::Item::widget("context", 130.0),
            toolbar::Item::widget("minor", 60.0),
            toolbar::Item::widget("rules", 70.0),
            toolbar::Item::widget("format", 74.0),
            toolbar::Item::separator("separator-2"),
            toolbar::Item::command(
                "copy",
                Command::CopyToOtherSide,
                "Copy",
                ready && !self.locked,
                if self.locked { LOCKED } else { NOT_COMPARED },
            ),
            toolbar::Item::command(
                "next-section",
                Command::NextSection,
                "Next Section",
                ready,
                NOT_COMPARED,
            ),
            toolbar::Item::command(
                "previous-section",
                Command::PreviousSection,
                "Prev Section",
                ready,
                NOT_COMPARED,
            ),
            toolbar::Item::command("swap", Command::SwapSides, "Swap", !self.locked, LOCKED),
            toolbar::Item::command("reload", Command::Reload, "Reload", !self.locked, LOCKED),
            toolbar::Item::separator("separator-3"),
            toolbar::Item::command(
                "report",
                Command::CompareReport,
                "Report",
                ready,
                NOT_COMPARED,
            ),
        ]
    }

    /// The session toolbar, in the recorded order.
    ///
    /// A control this build does not answer for stays on the bar, disabled,
    /// with the reason attached.
    #[allow(clippy::too_many_lines)]
    fn toolbar(&mut self, ui: &mut egui::Ui, palette: &Palette) -> Option<ViewAction> {
        let running = self.status.is_running();
        let mut filter = self.filter;
        let mut lines = self.context_lines;
        let mut cancel = false;
        let mut home = false;
        let mut minor = self.ignore_unimportant;
        let mut rules = self.rules();
        let id = self.id;
        let items = self.toolbar_items();
        let layout = toolbar::Layout::from_options(
            &ca_ui::options::runtime::current(ui.ctx()).stored.commands,
            toolbar::ToolbarView::Text,
        );
        // The background is reserved before the controls and filled after, so
        // it covers the bar the controls actually took and nothing else.
        let background = ui.painter().add(egui::Shape::Noop);
        let outcome = toolbar::show_for(
            toolbar::ToolbarView::Text,
            ui,
            id.with("toolbar"),
            &items,
            &layout,
            |ui, name| match name {
                "home" => {
                    if ui
                        .add(widgets::IconButton::new(
                            "Home",
                            Some(ca_ui::icons::Icon::Home),
                        ))
                        .clicked()
                    {
                        home = true;
                    }
                }
                "sessions" => {
                    widgets::toolbar_button(ui, "Sessions", false, PENDING);
                }
                "all" | "diffs" | "same" => {
                    let (option, label) = match name {
                        "all" => (DisplayFilter::All, "All"),
                        "diffs" => (DisplayFilter::Differences, "Diffs"),
                        _ => (DisplayFilter::Same, "Same"),
                    };
                    let selected =
                        std::mem::discriminant(&option) == std::mem::discriminant(&filter);
                    if ui
                        .add(
                            widgets::IconButton::new(
                                label,
                                ca_ui::icons::toolbar_icon(toolbar::ToolbarView::Text, name),
                            )
                            .selected(selected),
                        )
                        .on_hover_text(filter_label(option))
                        .clicked()
                    {
                        filter = option;
                    }
                }
                "context" => {
                    let option = DisplayFilter::Context(0);
                    let selected =
                        std::mem::discriminant(&option) == std::mem::discriminant(&filter);
                    if ui
                        .add(
                            widgets::IconButton::new(
                                "Context",
                                Some(ca_ui::icons::Icon::ShowContext),
                            )
                            .selected(selected),
                        )
                        .on_hover_text(filter_label(option))
                        .clicked()
                    {
                        filter = option;
                    }
                    if matches!(filter, DisplayFilter::Context(_)) {
                        ui.add(egui::DragValue::new(&mut lines).range(0..=50));
                    }
                }
                "minor" => {
                    if ui
                        .add(
                            widgets::IconButton::new(
                                "Minor",
                                Some(ca_ui::icons::Icon::IgnoreUnimportant),
                            )
                            .selected(minor),
                        )
                        .on_hover_text(Command::ToggleIgnoreUnimportant.label())
                        .clicked()
                    {
                        minor = !minor;
                    }
                }
                "rules" => {
                    widgets::icon_menu(ui, "Rules", ca_ui::icons::Icon::Rules, |ui| {
                        for (label, important) in [
                            ("Comments", &mut rules.elements.comments),
                            ("Strings", &mut rules.elements.strings),
                            ("Numbers", &mut rules.elements.numbers),
                            ("Keywords", &mut rules.elements.keywords),
                            ("Identifiers", &mut rules.elements.identifiers),
                        ] {
                            ui.checkbox(important, label);
                        }
                        ui.separator();
                        let mut whitespace = rules.whitespace_unimportant();
                        if ui
                            .checkbox(&mut whitespace, "Whitespace unimportant")
                            .changed()
                        {
                            rules.set_whitespace_unimportant(whitespace);
                        }
                        ui.checkbox(&mut rules.case_unimportant, "Case unimportant");
                    });
                }
                "format" => {
                    widgets::toolbar_button(ui, "Format", false, PENDING);
                    if running {
                        ui.spinner();
                        if ca_ui::widgets::inline_button(ui, "Cancel", ca_ui::icons::Icon::Stop)
                            .clicked()
                        {
                            cancel = true;
                        }
                    }
                }
                _ => {}
            },
        );
        let bar = outcome.rect;
        ui.painter().set(
            background,
            egui::Shape::rect_filled(bar, 0.0, palette.toolbar),
        );
        self.toolbar_extent = (bar.width(), outcome.room);
        match outcome.command {
            Some(command @ (Command::NextSection | Command::PreviousSection)) => {
                self.navigate(command);
            }
            Some(Command::CopyToOtherSide) => self.copy_to(self.active.other(), false),
            Some(Command::SwapSides) => self.request_reread(Reread::SwapSides),
            Some(Command::Reload) => self.request_reread(Reread::Reload),
            Some(Command::CompareReport) => self.report.request(),
            _ => {}
        }
        if std::mem::discriminant(&filter) != std::mem::discriminant(&self.filter) {
            self.set_filter(filter);
        } else if lines != self.context_lines {
            self.context_lines = lines;
            self.refilter();
        }
        self.set_ignore_unimportant(minor);
        self.set_rules(rules);
        if cancel {
            if let Some(job) = self.job.as_ref() {
                job.cancel();
            }
        }
        home.then_some(ViewAction::OpenHome)
    }

    /// Count unimportant differences as matching text, or stop doing so.
    pub fn set_ignore_unimportant(&mut self, ignore: bool) {
        if self.ignore_unimportant == ignore {
            return;
        }
        self.ignore_unimportant = ignore;
        self.data.model.set_ignore_unimportant(ignore);
        self.refilter();
        self.follow_caret();
    }

    /// True when unimportant differences read as matching text.
    #[must_use]
    pub const fn ignores_unimportant(&self) -> bool {
        self.ignore_unimportant
    }

    /// The painting class of a row under the rules in force.
    fn row_class(&self, row: &model::Row) -> TextClass {
        row_class(row, self.ignore_unimportant)
    }

    /// True when the line details area is shown.
    #[must_use]
    pub const fn shows_details(&self) -> bool {
        self.show_details
    }

    /// True when the line number gutter is shown.
    #[must_use]
    pub const fn shows_line_numbers(&self) -> bool {
        self.show_line_numbers
    }

    /// Where the last frame drew each difference section's copy arrow.
    #[must_use]
    pub fn section_arrows(&self) -> &[Arrow] {
        &self.arrows
    }

    /// The left edge of one pane's gutter on the last frame.
    #[must_use]
    pub const fn gutter_x(&self, side: Side) -> f32 {
        match side {
            Side::Left => self.gutter_x.0,
            Side::Right => self.gutter_x.1,
        }
    }

    /// The width of a pane's gutter on the last frame.
    #[must_use]
    pub const fn gutter_width(&self) -> f32 {
        self.gutter_x.2
    }

    /// The background one pane paints a row in.
    #[must_use]
    pub fn pane_background(
        &self,
        side: Side,
        row: usize,
        palette: &Palette,
    ) -> Option<egui::Color32> {
        let entry = self.data.model.row(row)?;
        let class = if side.line_of(entry).is_none() {
            TextClass::Gap
        } else {
            self.row_class(entry)
        };
        Some(palette.text_row_in(class, side == self.active).0)
    }

    /// The left edge of the splitter between the panes and its width.
    #[must_use]
    pub const fn splitter(&self) -> (f32, f32) {
        (self.center_x, CENTER_WIDTH)
    }

    /// The two colors the splitter paints, left line first.
    #[must_use]
    pub const fn splitter_colors(palette: &Palette) -> (egui::Color32, egui::Color32) {
        (palette.separator_strong, palette.separator)
    }

    /// The color the hatch over a gap row draws its lines in.
    #[must_use]
    pub const fn gap_hatch_color(palette: &Palette) -> egui::Color32 {
        palette.gap_pattern
    }

    /// What the last frame laid the toolbar out in, and the room it had.
    #[must_use]
    pub const fn toolbar_extent(&self) -> (f32, f32) {
        self.toolbar_extent
    }

    /// The colored runs one row of one pane paints, for a test.
    ///
    /// The same splitting the frame uses, over the whole line rather than over
    /// the viewport.
    #[must_use]
    pub fn painted_runs(&self, side: Side, row: usize, palette: &Palette) -> Vec<Run> {
        let Some(entry) = self.data.model.row(row).copied() else {
            return Vec::new();
        };
        let Some(line) = side.line_of(&entry) else {
            return Vec::new();
        };
        let class = self.row_class(&entry);
        // Characters outside a difference take the pane's matching-text color.
        let base = palette.text_row_in(TextClass::Same, side == self.active).1;
        let text = self.pane(side).line_text(line);
        let metrics = lines::measure(&text);
        let inline = self
            .data
            .inline
            .get(&u32::try_from(row).unwrap_or(u32::MAX));
        let spans = inline.map(|diff| match side {
            Side::Left => diff.left.as_slice(),
            Side::Right => diff.right.as_slice(),
        });
        let differs = class != TextClass::Same && class != TextClass::Gap;
        if class == TextClass::Orphan || (differs && spans.is_none()) {
            return vec![Run {
                start: 0,
                end: metrics.columns,
                color: palette.text_row_in(class, side == self.active).1,
            }];
        }
        let mut runs = Vec::new();
        let syntax = self
            .line_cache
            .get(&(u8::from(side == Side::Right), line))
            .filter(|_| self.syntax_on_differences || !differs);
        let empty: [ColumnSpan; 0] = [];
        build_runs(
            &mut runs,
            0..metrics.columns,
            base,
            syntax
                .and_then(|entry| entry.spans.as_deref())
                .unwrap_or(&[]),
            palette,
            if differs {
                spans.unwrap_or(&empty)
            } else {
                &empty
            },
            palette.text_row_in(class, side == self.active).1,
        );
        runs
    }

    /// The two lines the details area shows, when it is shown.
    #[must_use]
    pub fn details_lines(&self) -> Option<(String, String)> {
        if !self.show_details {
            return None;
        }
        let row = self.data.model.row(self.caret)?;
        Some((
            row.left
                .map(|line| self.left_pane.line_text(line))
                .unwrap_or_default(),
            row.right
                .map(|line| self.right_pane.line_text(line))
                .unwrap_or_default(),
        ))
    }

    /// The copy arrows of the visible difference sections.
    ///
    /// One arrow per section per pane, at the left edge of that pane's gutter.
    /// The arrow in the left pane copies its section to the right.
    fn paint_arrows(
        &self,
        ui: &mut egui::Ui,
        painter: &egui::Painter,
        _font: &egui::FontId,
        palette: &Palette,
    ) -> Option<(usize, Side)> {
        if self.structural_summary().is_some() {
            return None;
        }
        let mut copy = None;
        for arrow in &self.arrows {
            let response = ui.interact(
                arrow.rect,
                self.id
                    .with(("arrow", arrow.section, arrow.side == Side::Right)),
                egui::Sense::click(),
            );
            let icon = if arrow.side == Side::Left {
                ca_ui::icons::Icon::CopyToRight
            } else {
                ca_ui::icons::Icon::CopyToLeft
            };
            let label = if arrow.side == Side::Left {
                "Copy to Right"
            } else {
                "Copy to Left"
            };
            response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
            });
            icon.paint_in_row(
                painter,
                arrow.rect.center(),
                arrow.rect.height(),
                if response.hovered() {
                    palette.important_text
                } else {
                    palette.gutter_arrow
                },
            );
            if response.clicked() {
                let start = self
                    .data
                    .model
                    .sections()
                    .get(arrow.section as usize)
                    .map_or(self.caret, |range| range.start as usize);
                copy = Some((start, arrow.side.other()));
            }
        }
        copy
    }

    /// The line details area: the caret row's line from each side.
    ///
    /// Nothing is laid out or read while the area is hidden.
    #[allow(clippy::too_many_lines)]
    fn details(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        if !self.show_details {
            return;
        }
        let row_height = self.row_height();
        let font = egui::FontId::monospace(self.font_size());
        let char_width = column_pitch(ui, &font);
        let marker = char_width * 2.0;
        let height = row_height * 2.0 + SCROLLBAR;
        let full = ui.available_rect_before_wrap();
        let area = egui::Rect::from_min_size(full.left_top(), egui::vec2(full.width(), height));
        ui.allocate_rect(area, egui::Sense::hover());
        let painter = ui.painter_at(area);
        painter.rect_filled(area, 0.0, palette.chrome);
        let Some(entry) = self.data.model.row(self.caret).copied() else {
            return;
        };
        let class = self.row_class(&entry);
        let inline = self
            .data
            .inline
            .get(&u32::try_from(self.caret).unwrap_or(u32::MAX));
        let text_x = area.left() + marker;
        let width = (area.width() - marker).max(0.0);
        let first_column = self.details_horizontal.floor().max(0.0);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let first_column_index = first_column as usize;
        let columns = (width / char_width).ceil().max(0.0);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let columns = columns as usize + 1;
        let mut widest = 1.0_f32;
        let mut runs: Vec<Run> = Vec::new();
        for (index, side) in [Side::Left, Side::Right].into_iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let y = index as f32;
            let y = y.mul_add(row_height, area.top());
            painter.text(
                egui::pos2(area.left() + 2.0, y),
                egui::Align2::LEFT_TOP,
                if side == Side::Left { "L" } else { "R" },
                font.clone(),
                palette.gutter_text,
            );
            let line = side.line_of(&entry);
            let text = line.map(|line| self.pane(side).line_text(line));
            let metrics = text.as_deref().map(lines::measure);
            #[allow(clippy::cast_precision_loss)]
            if let Some(metrics) = metrics {
                widest = widest.max(metrics.columns as f32);
            }
            let focused = side == self.active;
            paint_side(
                &painter,
                &PaneGeometry {
                    gutter_x: text_x,
                    text_x,
                    width,
                    gutter_width: 0.0,
                    y,
                    row_height,
                    char_width,
                    first_column: first_column_index,
                    columns,
                    column_shift: (self.details_horizontal - first_column) * char_width,
                },
                &PaneContent {
                    line: None,
                    text: text.as_deref(),
                    metrics,
                    syntax: None,
                    spans: inline.map(|diff| match side {
                        Side::Left => diff.left.as_slice(),
                        Side::Right => diff.right.as_slice(),
                    }),
                    background: palette.text_row_in(class, focused).0,
                    base_color: palette.text_row_in(TextClass::Same, focused).1,
                    difference_color: palette.text_row_in(class, focused).1,
                    whole_line_differs: class == TextClass::Orphan
                        || (class != TextClass::Same
                            && class != TextClass::Gap
                            && inline.is_none()),
                    is_difference: class != TextClass::Same && class != TextClass::Gap,
                    gap: line.is_none(),
                },
                palette,
                &font,
                &mut runs,
            );
        }
        let track = egui::Rect::from_min_max(
            egui::pos2(text_x, area.bottom() - SCROLLBAR),
            egui::pos2(area.right(), area.bottom()),
        );
        let visible_columns = (width / char_width).max(1.0);
        // One bar moves both lines, so the two always show the same columns.
        let moved = widgets::horizontal_scrollbar(
            ui,
            &painter,
            self.id.with("details-bar"),
            track,
            (palette.gutter_background, palette.thumbnail_marker),
            (
                widest.max(visible_columns),
                visible_columns,
                self.details_horizontal,
            ),
            SCROLLBAR * 2.0,
        );
        if let Some(offset) = moved {
            self.details_horizontal = offset.max(0.0);
        }
    }

    /// The find and replace strip.
    /// The name the report display filter carries for what the view shows.
    #[must_use]
    pub const fn report_filter(&self) -> &'static str {
        match self.filter {
            DisplayFilter::Differences => "mismatches",
            DisplayFilter::Context(_) => "context",
            DisplayFilter::Same => "matches",
            DisplayFilter::All | DisplayFilter::None => "all",
        }
    }

    /// The comparison the report is written from.
    #[must_use]
    ///
    /// The rows name line numbers in the two line vectors the view already
    /// holds, and those vectors are shared, so no file is copied.
    pub fn report_payload(&self) -> (ca_ui::report::ReportMeta, ca_ui::report::Payload) {
        let meta = ca_ui::report::ReportMeta::new(
            self.left_path.display().to_string(),
            self.right_path.display().to_string(),
        )
        .with_title(self.structural_summary().map_or_else(
            || ReportKind::Text.title().to_owned(),
            structure::Summary::label,
        ));
        let mut rows = Vec::with_capacity(self.visible.len());
        for position in 0..self.visible.len() {
            let Some(index) = self.visible.row_at(position) else {
                continue;
            };
            let Some(row) = self.data.model.row(index) else {
                continue;
            };
            rows.push(ca_ui::report::TextRowRef {
                kind: match row.class {
                    model::RowClass::Same => ca_ui::report::RowKind::Same,
                    model::RowClass::Changed => ca_ui::report::RowKind::Changed,
                    model::RowClass::LeftOnly => ca_ui::report::RowKind::LeftOnly,
                    model::RowClass::RightOnly => ca_ui::report::RowKind::RightOnly,
                },
                importance: row.importance.map(Into::into),
                left: row.left,
                right: row.right,
            });
        }
        (
            meta,
            ca_ui::report::Payload::Text(ca_ui::report::TextPayload {
                left: Arc::clone(&self.data.left.lines),
                right: Arc::clone(&self.data.right.lines),
                rows,
            }),
        )
    }

    fn report_panel(&mut self, ui: &mut egui::Ui) {
        if !self.report.is_open() {
            return;
        }
        if self.report.draw(ui) == ca_ui::report::ReportAction::Write {
            let (meta, payload) = self.report_payload();
            self.report.start(meta, payload, 0);
        }
        if let Some(text) = self.report.take_clipboard() {
            ui.ctx().copy_text(text);
        }
    }

    fn find_panel(&mut self, ui: &mut egui::Ui) {
        let request = self.find.show_find(ui);
        self.answer_panel(request);
    }

    /// The go to line strip.
    fn goto_panel(&mut self, ui: &mut egui::Ui) {
        let request = self.find.show_go_to(ui);
        self.answer_panel(request);
    }

    /// Carry out what the find or go to strip asked for.
    fn answer_panel(&mut self, request: Option<PanelRequest>) {
        match request {
            None => {}
            Some(PanelRequest::Next) => self.run_find(false),
            Some(PanelRequest::Previous) => self.run_find(true),
            Some(PanelRequest::Replace) => {
                self.start_find(FindOperation::Replace);
            }
            Some(PanelRequest::ReplaceAll) => self.run_replace_all(),
            Some(PanelRequest::GoTo(line)) => {
                find::go_to(self.active_pane_mut(), line, 1);
                self.follow_caret();
                let row = self.caret;
                self.go_to_row(row);
            }
            Some(PanelRequest::NoLine) => self.message = Some(find::NO_LINE.to_owned()),
        }
    }

    /// True while a save runs.
    #[must_use]
    pub const fn is_saving(&self) -> bool {
        self.save_job.is_some()
    }

    /// The strip that asks a question a save or a close raised.
    fn prompt_panel(&mut self, ui: &mut egui::Ui) {
        let Some(prompt) = self.prompt.clone() else {
            return;
        };
        if let Prompt::KeptCopy(path) = &prompt {
            egui::Window::new("Another version was kept")
                .id(self.id.with("kept-copy"))
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ui.ctx(), |ui| {
                    ca_ui::widgets::notice_current(ui,ca_ui::icons::Icon::Warning,24.0,"Another program changed the file during your save. The saved file contains your edit. Review the other version at:");
                    ui.monospace(path);
                    if ui.button("I have the path").clicked() {
                        self.prompt = None;
                    }
                });
            return;
        }
        let Some(text) = self.prompt_text() else {
            return;
        };
        let mut answer: Option<bool> = None;
        let mut unsaved: Option<CloseChoice> = None;
        ui.horizontal_wrapped(|ui| {
            ui.label(text);
            match &prompt {
                Prompt::KeptCopy(_) => {}
                Prompt::DiskChanged(_) | Prompt::WouldLose(..) => {
                    let yes = if matches!(prompt, Prompt::DiskChanged(_)) {
                        "Overwrite"
                    } else {
                        "Save anyway"
                    };
                    if ui.button(yes).clicked() {
                        answer = Some(true);
                    }
                    if ca_ui::widgets::inline_button(ui, "Cancel", ca_ui::icons::Icon::Stop)
                        .clicked()
                    {
                        answer = Some(false);
                    }
                }
                Prompt::Closing | Prompt::Reread(_) => {
                    let (save, discard) = match &prompt {
                        Prompt::Reread(action) => (action.save_label(), action.discard_label()),
                        _ => ("Save and close", "Discard and close"),
                    };
                    for (label, choice) in [
                        (save, CloseChoice::Save),
                        (discard, CloseChoice::Discard),
                        ("Cancel", CloseChoice::Cancel),
                    ] {
                        if ui.button(label).clicked() {
                            unsaved = Some(choice);
                        }
                    }
                }
            }
        });
        if let Some(choice) = unsaved {
            self.answer_unsaved(choice);
        } else if let Some(answer) = answer {
            self.answer_prompt(answer);
        }
    }

    /// The question the view is waiting on, as the sentence it shows.
    #[must_use]
    pub fn prompt_text(&self) -> Option<String> {
        Some(match self.prompt.as_ref()? {
            Prompt::KeptCopy(path) => format!(
                "Another program changed the file during your save. The saved file contains your edit. Review the other version at {path}."
            ),
            Prompt::DiskChanged(_) => {
                "The file changed on disk since it was read. Overwrite it?".to_owned()
            }
            Prompt::WouldLose(_, reason) => reason.clone(),
            Prompt::Closing | Prompt::Reread(_) => {
                "This tab has edits that are not written.".to_owned()
            }
        })
    }

    /// Answer the waiting question: `true` goes ahead, `false` stays put.
    ///
    /// Going ahead on the question about unwritten edits saves every modified
    /// side, then closes the tab or runs the command that asked.
    pub fn answer_prompt(&mut self, proceed: bool) {
        let Some(prompt) = self.prompt.clone() else {
            return;
        };
        match prompt {
            Prompt::KeptCopy(_) => self.prompt = None,
            Prompt::Closing | Prompt::Reread(_) => self.answer_unsaved(if proceed {
                CloseChoice::Save
            } else {
                CloseChoice::Cancel
            }),
            _ if !proceed => {
                self.prompt = None;
                self.saving = None;
                self.saving_as = None;
                self.save_both_pending = false;
                self.close_requested = false;
                self.forget_reread();
            }
            Prompt::DiskChanged(side) => {
                let save_as = self.saving_as.take();
                let consent = SaveConsent {
                    accept_disk_change: true,
                    ..self.saving_consent
                };
                self.save_side_to(side, consent, save_as);
            }
            Prompt::WouldLose(side, _) => {
                let save_as = self.saving_as.take();
                let consent = SaveConsent {
                    accept_loss: true,
                    ..self.saving_consent
                };
                self.save_side_to(side, consent, save_as);
            }
        }
    }

    /// Answer the question about edits that are not written, which a close or
    /// a command that reads the files again raised.
    pub fn answer_unsaved(&mut self, choice: CloseChoice) {
        let action = match self.prompt {
            Some(Prompt::Closing) => None,
            Some(Prompt::Reread(action)) => Some(action),
            _ => return,
        };
        self.prompt = None;
        match (choice, action) {
            (CloseChoice::Cancel, _) => {
                self.close_requested = false;
                self.forget_reread();
            }
            (CloseChoice::Discard, None) => {
                self.left_pane.mark_saved();
                self.right_pane.mark_saved();
                self.close_requested = true;
            }
            (CloseChoice::Discard, Some(action)) => self.reread(action),
            (CloseChoice::Save, None) => {
                self.close_requested = true;
                self.save_both();
            }
            (CloseChoice::Save, Some(action)) => {
                self.reread_after_save = Some(action);
                self.save_both();
                self.reread_when_saved();
            }
        }
    }

    /// Run a command that reads the files again, asking first while a pane
    /// holds an edit that is not written.
    fn request_reread(&mut self, action: Reread) {
        if self.locked && action != Reread::Settings {
            return;
        }
        if self.save_job.is_some() || self.prompt.is_some() {
            self.message = Some("Finish the save and its questions first.".to_owned());
            self.pending_settings = None;
            return;
        }
        if self.left_pane.is_modified() || self.right_pane.is_modified() {
            self.prompt = Some(Prompt::Reread(action));
            return;
        }
        self.reread(action);
    }

    /// Carry out a command that reads the files again, dropping every edit.
    fn reread(&mut self, action: Reread) {
        self.reread_after_save = None;
        match action {
            Reread::Reload => self.restart(),
            Reread::Open => self.apply_fields(),
            Reread::SwapSides => {
                std::mem::swap(&mut self.left_path, &mut self.right_path);
                std::mem::swap(&mut self.left_field, &mut self.right_field);
                std::mem::swap(&mut self.left_title, &mut self.right_title);
                self.restart();
            }
            Reread::Settings => {
                if let Some(settings) = self.pending_settings.take() {
                    self.session_settings = *settings;
                }
                self.restart();
            }
        }
    }

    /// Run the command that waited on a save once every edit is written.
    fn reread_when_saved(&mut self) {
        let written = self.save_job.is_none()
            && self.prompt.is_none()
            && !self.left_pane.is_modified()
            && !self.right_pane.is_modified();
        if !written {
            return;
        }
        if let Some(action) = self.reread_after_save.take() {
            self.reread(action);
        }
    }

    /// Drop the command that waited on a save that did not complete.
    fn forget_reread(&mut self) {
        self.reread_after_save = None;
        self.pending_settings = None;
    }

    /// Write whichever sides are modified, one after the other.
    fn save_both(&mut self) {
        self.save_both_pending = true;
        if self.left_pane.is_modified() {
            self.save_side(Side::Left, SaveConsent::default());
        } else if self.right_pane.is_modified() {
            self.save_side(Side::Right, SaveConsent::default());
        } else {
            self.save_both_pending = false;
        }
    }

    /// The line under each path field, naming what the side holds.
    fn file_info(&self, ui: &mut egui::Ui) {
        let sides: Vec<widgets::FileInfo> = [Side::Left, Side::Right]
            .into_iter()
            .map(|side| {
                let (label, payload, stamp, modified, format) = match side {
                    Side::Left => (
                        self.left_title
                            .as_deref()
                            .filter(|title| !title.is_empty())
                            .unwrap_or("Left"),
                        &self.data.left,
                        self.left_stamp,
                        self.left_pane.is_modified(),
                        self.left_syntax.format().name.clone(),
                    ),
                    Side::Right => (
                        self.right_title
                            .as_deref()
                            .filter(|title| !title.is_empty())
                            .unwrap_or("Right"),
                        &self.data.right,
                        self.right_stamp,
                        self.right_pane.is_modified(),
                        self.right_syntax.format().name.clone(),
                    ),
                };
                if payload.encoding.is_empty() {
                    return widgets::FileInfo::new(label);
                }
                widgets::FileInfo {
                    size: stamp.map(|stamp| stamp.size),
                    modified: stamp.and_then(|stamp| stamp.modified),
                    format: Some(format),
                    encoding: Some(payload.encoding.clone()),
                    line_ending: Some(if payload.mixed_eol {
                        format!("{}, mixed", payload.eol)
                    } else {
                        payload.eol.clone()
                    }),
                    is_modified: modified,
                    ..widgets::FileInfo::new(label)
                }
            })
            .collect();
        widgets::file_info_bar(ui, ca_ui::format::probed_offset().unwrap_or(0), &sides);
    }

    /// How the caret's row reads.
    #[must_use]
    pub fn current_status(&self) -> model::LineStatus {
        self.data
            .model
            .row(self.caret)
            .map_or(model::LineStatus::Same, |row| self.data.model.status(row))
    }

    /// The four fields the status bar carries, in order.
    ///
    /// Encoding and line ending are left out: the information line under each
    /// path already carries them.
    #[must_use]
    pub fn status_fields(&self) -> Vec<String> {
        let counts = self.data.model.counts();
        let mut fields = vec![
            format!("{} difference section(s)", counts.sections),
            self.current_status().label().to_owned(),
            if self.structural_summary().is_some() {
                "Read-only".to_owned()
            } else if self.active_pane().is_overwrite() {
                "Overwrite".to_owned()
            } else {
                "Insert".to_owned()
            },
        ];
        if let Some(summary) = self.structural_summary() {
            fields.push(summary.label());
        }
        if counts.ignored_unimportant > 0 {
            fields.push(format!(
                "Ignoring {} unimportant difference(s)",
                counts.ignored_unimportant
            ));
        }
        if self.data.left.had_errors || self.data.right.had_errors {
            fields.push(
                "Invalid byte sequences were replaced; text comparison may be incomplete".into(),
            );
        }
        if let Some(taken) = self.load_time {
            fields.push(format!("Load time {:.3} s", taken.as_secs_f64()));
        }
        fields
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            match &self.status {
                Status::Running(step) => {
                    ui.label(*step);
                }
                Status::Failed(reason) => {
                    ca_ui::widgets::notice_current(
                        ui,
                        ca_ui::icons::Icon::Error,
                        16.0,
                        &format!("Failed: {reason}"),
                    );
                }
                Status::Cancelled => {
                    ca_ui::widgets::notice_current(
                        ui,
                        ca_ui::icons::Icon::Warning,
                        16.0,
                        "Stopped before finishing",
                    );
                }
                Status::Ready => {
                    for (index, field) in self.status_fields().into_iter().enumerate() {
                        if index > 0 {
                            ui.separator();
                        }
                        ui.label(field);
                    }
                }
            }
            if self.rediff.is_running() || self.rediff.is_stale() {
                ui.separator();
                ui.label("Comparing the edit");
            }
            if let Some(message) = &self.message {
                ui.separator();
                ca_ui::widgets::notice_current(
                    ui,
                    ca_ui::icons::Icon::Info,
                    16.0,
                    message.as_str(),
                );
            }
            if self.data.inline_omitted {
                ui.separator();
                ui.label("Inline highlighting omitted: comparison too large");
            }
        });
    }

    /// Reduce the comparison to one entry per strip pixel.
    ///
    /// Runs when the model, the filter or the strip's height changes, never per
    /// frame.
    fn rebuild_strip(&mut self, height: f32) {
        if !self.strip_stale && self.strip.matches(height, self.visible.len()) {
            return;
        }
        let visible = &self.visible;
        let model = &self.data.model;
        let ignore = self.ignore_unimportant;
        self.strip = Strip::from_rows(height, visible.len(), |position| {
            let index = visible.row_at(position)?;
            let row = model.row(index)?;
            model.is_difference(row).then(|| row_class(row, ignore))
        });
        self.strip_stale = false;
    }

    fn thumbnail_strip(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        let height = ui.available_height();
        let (response, painter) = ui.allocate_painter(
            egui::vec2(THUMBNAIL_WIDTH, height),
            egui::Sense::click_and_drag(),
        );
        let rect = response.rect;
        painter.rect_filled(rect, 0.0, palette.thumbnail_background);
        self.rebuild_strip(rect.height());
        for (pixel, bucket) in self.strip.buckets().iter().enumerate() {
            let Some(class) = bucket.class() else {
                continue;
            };
            let (background, _) = palette.text_row(class);
            #[allow(clippy::cast_precision_loss)]
            let top = rect.top() + pixel as f32;
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(rect.left() + 2.0, top),
                    egui::pos2(rect.right() - 2.0, top + 1.0),
                ),
                0.0,
                background,
            );
        }
        let strip = Thumbnail::new(rect.height(), self.visible.len());
        if strip.is_empty() {
            return;
        }
        let marker = strip.viewport_marker(self.scroll.first_row(), self.viewport_rows);
        painter.rect_stroke(
            egui::Rect::from_min_max(
                egui::pos2(rect.left(), rect.top() + marker.start),
                egui::pos2(rect.right(), rect.top() + marker.end),
            ),
            0.0,
            egui::Stroke::new(1.0, palette.thumbnail_marker),
            egui::StrokeKind::Inside,
        );
        if let Some(caret) = self.visible.position_of(self.caret) {
            let y = rect.top() + strip.row_to_y(caret);
            painter.line_segment(
                [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
                egui::Stroke::new(1.0, palette.thumbnail_caret),
            );
        }
        if let Some(pointer) = response.interact_pointer_pos() {
            let position = strip.drag_to_first_row(pointer.y - rect.top(), self.viewport_rows);
            self.scroll_to_position(position);
        }
    }

    /// The panes, the vertical scrollbar and the shared horizontal scrollbar.
    #[allow(clippy::too_many_lines)]
    fn rows(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        let row_height = self.row_height();
        let font = egui::FontId::monospace(self.font_size());
        let char_width = column_pitch(ui, &font);
        let numbers = if self.show_line_numbers {
            char_width * LINE_NUMBER_COLUMNS
        } else {
            0.0
        };
        let gutter = ARROW_WIDTH + numbers;
        let total = self.visible.len();

        let full = ui.available_rect_before_wrap();
        let body = egui::Rect::from_min_max(
            full.left_top(),
            egui::pos2(full.right() - SCROLLBAR, full.bottom() - SCROLLBAR),
        );
        let response = ui.allocate_rect(full, egui::Sense::hover());
        let painter = ui.painter_at(full);

        self.body = body;
        self.viewport_height = body.height();
        self.scroll.clamp(body.height(), row_height, total);
        let range = self.scroll.visible(body.height(), row_height, total);
        self.viewport_rows = range.len();

        let pane_width = ((body.width() - 2.0 * gutter - CENTER_WIDTH) / 2.0).max(0.0);
        let left_text_x = body.left() + gutter;
        let center_x = left_text_x + pane_width;
        let right_gutter_x = center_x + CENTER_WIDTH;
        let right_text_x = right_gutter_x + gutter;
        self.center_x = center_x;
        self.gutter_x = (body.left(), right_gutter_x, gutter);

        let pane_columns = (pane_width / char_width).ceil().max(0.0);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let pane_columns = pane_columns as usize + 1;
        let first_column = self.horizontal.floor().max(0.0);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let first_column_index = first_column as usize;
        // The sub-column remainder is a pixel shift, so scrolling sideways is
        // smooth without the slice changing.
        let column_shift = (self.horizontal - first_column) * char_width;

        let clip = painter.with_clip_rect(body);
        for (x, color) in [
            (center_x, palette.separator_strong),
            (right_gutter_x - 1.0, palette.separator),
        ] {
            clip.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(x, body.top()),
                    egui::pos2(x + 1.0, body.bottom()),
                ),
                0.0,
                color,
            );
        }

        self.refresh_line_cache(&range);
        self.arrows.clear();
        let mut runs: Vec<Run> = Vec::new();
        let mut gaps = GapRuns::default();
        let mut arrow_sections: (Option<u32>, Option<u32>) = (None, None);
        let syntax_on_differences = self.syntax_on_differences;
        let caret_row = self.caret;
        for position in range.clone() {
            let Some(index) = self.visible.row_at(position) else {
                continue;
            };
            let Some(row) = self.data.model.row(index).copied() else {
                continue;
            };
            let y = body.top() + self.scroll.row_y(position, row_height);
            let class = self.row_class(&row);
            let row_differs = class != TextClass::Same && class != TextClass::Gap;
            let inline = self
                .data
                .inline
                .get(&u32::try_from(index).unwrap_or(u32::MAX));
            let geometry = |gutter_x: f32, text_x: f32| PaneGeometry {
                gutter_x,
                text_x,
                width: pane_width,
                gutter_width: gutter,
                y,
                row_height,
                char_width,
                first_column: first_column_index,
                columns: pane_columns,
                column_shift,
            };
            // The text comes from the buffers rather than from the comparison,
            // so a keystroke shows at once while the comparison behind it is
            // still catching up. Only the rows in view are read.
            let left_text = self.cached_line(Side::Left, row.left);
            let right_text = self.cached_line(Side::Right, row.right);
            let left_geometry = geometry(body.left(), left_text_x);
            let right_geometry = geometry(right_gutter_x, right_text_x);
            for (side, entry, geometry) in [
                (Side::Left, left_text, &left_geometry),
                (Side::Right, right_text, &right_geometry),
            ] {
                let line = side.line_of(&row);
                let focused = side == self.active;
                // A side with no line on this row carries the pane's normal
                // background under the hatch. A difference color there would
                // claim the side holds text that differs.
                let painted_class = if line.is_none() {
                    TextClass::Gap
                } else {
                    class
                };
                let (mut background, difference_color) =
                    palette.text_row_in(painted_class, focused);
                // Characters outside a difference take the matching-text color.
                let base = palette.text_row_in(TextClass::Same, focused).1;
                if index == caret_row
                    && painted_class != TextClass::Same
                    && painted_class != TextClass::Gap
                {
                    background = palette.caret_line;
                }
                paint_side(
                    &clip,
                    geometry,
                    &PaneContent {
                        line: if self.show_line_numbers { line } else { None },
                        text: entry.map(|entry| entry.text.as_str()),
                        metrics: entry.map(|entry| entry.metrics),
                        syntax: entry
                            .and_then(|entry| entry.spans.as_deref())
                            .filter(|_| syntax_on_differences || !row_differs),
                        spans: inline.map(|diff| match side {
                            Side::Left => diff.left.as_slice(),
                            Side::Right => diff.right.as_slice(),
                        }),
                        background,
                        base_color: base,
                        difference_color,
                        whole_line_differs: class == TextClass::Orphan
                            || (class != TextClass::Same
                                && class != TextClass::Gap
                                && inline.is_none()),
                        is_difference: class != TextClass::Same && class != TextClass::Gap,
                        gap: line.is_none(),
                    },
                    palette,
                    &font,
                    &mut runs,
                );
                gaps.mark(side, line.is_none(), y, y + row_height);
            }
            if index == caret_row && class != TextClass::Same && class != TextClass::Gap {
                for edge in [y, y + row_height - 1.0] {
                    clip.rect_filled(
                        egui::Rect::from_min_max(
                            egui::pos2(left_text_x, edge),
                            egui::pos2(body.right(), edge + 1.0),
                        ),
                        0.0,
                        palette.separator,
                    );
                }
            }
            paint_caret(
                &clip,
                &left_geometry,
                &self.left_pane,
                row.left,
                left_text.map_or("", |entry| entry.text.as_str()),
                self.active == Side::Left,
                (
                    palette,
                    &font,
                    left_text.map_or_else(lines::LineMetrics::default, |entry| entry.metrics),
                ),
            );
            paint_caret(
                &clip,
                &right_geometry,
                &self.right_pane,
                row.right,
                right_text.map_or("", |entry| entry.text.as_str()),
                self.active == Side::Right,
                (
                    palette,
                    &font,
                    right_text.map_or_else(lines::LineMetrics::default, |entry| entry.metrics),
                ),
            );
            if let Some(section) = row.section {
                for (side, geometry, seen) in [
                    (Side::Left, &left_geometry, &mut arrow_sections.0),
                    (Side::Right, &right_geometry, &mut arrow_sections.1),
                ] {
                    if *seen == Some(section) {
                        continue;
                    }
                    *seen = Some(section);
                    self.arrows.push(Arrow {
                        side,
                        section,
                        rect: egui::Rect::from_min_size(
                            egui::pos2(geometry.gutter_x, y),
                            egui::vec2(ARROW_WIDTH, row_height),
                        ),
                    });
                }
            }
            if self.bookmarks.marks(index) {
                ca_ui::icons::Icon::Bookmark.paint_in_row(
                    &clip,
                    egui::pos2(body.left() + 8.0, y + row_height / 2.0),
                    row_height,
                    palette.thumbnail_caret,
                );
            }
        }
        gaps.paint(
            &clip,
            (
                egui::Rangef::new(left_text_x, center_x),
                egui::Rangef::new(right_text_x, body.right()),
            ),
            palette.gap_pattern,
        );
        let panes = [
            (Side::Left, egui::Rangef::new(left_text_x, center_x)),
            (Side::Right, egui::Rangef::new(right_text_x, body.right())),
        ];
        self.painted_rulers.clear();
        if let Some(column) = self.column_line {
            #[allow(clippy::cast_precision_loss)]
            let offset = (column as f32 - self.horizontal) * char_width;
            for (_, span) in panes {
                let x = span.min + offset;
                if span.contains(x) {
                    clip.rect_filled(
                        egui::Rect::from_min_max(
                            egui::pos2(x, body.top()),
                            egui::pos2(x + 1.0, body.bottom()),
                        ),
                        0.0,
                        palette.separator,
                    );
                    self.painted_rulers.push(x);
                }
            }
        }
        self.painted_dim = None;
        if self.dim_fraction > 0.0 {
            let inactive = self.active.other();
            for (side, span) in panes {
                if side == inactive {
                    clip.rect_filled(
                        egui::Rect::from_x_y_ranges(span, body.y_range()),
                        0.0,
                        ca_ui::theme::dim_overlay(self.dim_fraction),
                    );
                    self.painted_dim = Some(side);
                }
            }
        }
        let arrow_copy = self.paint_arrows(ui, &clip, &font, palette);
        if let Some((row, to)) = arrow_copy {
            self.caret = row;
            self.copy_to(to, false);
        }
        self.pane_pointer(
            ui,
            body,
            &PointerGeometry {
                left_text_x,
                right_text_x,
                center_x,
                char_width,
                row_height,
                first_column: first_column_index,
            },
        );

        self.vertical_scrollbar(ui, &painter, palette, body, (row_height, total));
        self.horizontal_scrollbar(ui, &painter, palette, body, char_width);

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
        let _ = response;
    }

    /// The closest row at or above `from` that one side has a line on, falling
    /// back to the closest row below it.
    fn nearest_line(&self, from: usize, side: Side) -> Option<(usize, u32)> {
        let model = &self.data.model;
        let line_of = |index: usize| model.row(index).and_then(|row| side.line_of(row));
        for index in (0..=from).rev() {
            if let Some(line) = line_of(index) {
                return Some((index, line));
            }
        }
        for index in from..model.row_count() {
            if let Some(line) = line_of(index) {
                return Some((index, line));
            }
        }
        None
    }

    /// Place or extend the caret from the pointer.
    fn pane_pointer(&mut self, ui: &egui::Ui, body: egui::Rect, geometry: &PointerGeometry) {
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
        let side = if pointer.x < geometry.center_x {
            Side::Left
        } else {
            Side::Right
        };
        let text_x = match side {
            Side::Left => geometry.left_text_x,
            Side::Right => geometry.right_text_x,
        };
        let position = self
            .scroll
            .row_under(pointer.y - body.top(), geometry.row_height);
        let Some(index) = self.visible.row_at(position) else {
            return;
        };
        // A row the side has no line on is a gap. The caret goes to the nearest
        // real line of the same pane, which keeps the click inside the pane the
        // user aimed at.
        let direct = self.data.model.row(index).and_then(|row| side.line_of(row));
        let (index, line, column) = if let Some(line) = direct {
            let column = ((pointer.x - text_x) / geometry.char_width)
                .floor()
                .max(0.0);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let mut column = geometry.first_column + column as usize;
            // The buffer text painted on this row, not the comparison's copy:
            // after an edit the two differ until the comparison catches up.
            if let Some(entry) = self.cached_line(side, Some(line)) {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let columns = (body.width() / geometry.char_width).ceil() as usize + 2;
                #[allow(clippy::cast_precision_loss)]
                let shift = (self.horizontal - geometry.first_column as f32) * geometry.char_width;
                if let Some(hit) = glyph_column_at_x(
                    ui.painter(),
                    &entry.text,
                    entry.metrics,
                    geometry.first_column..geometry.first_column.saturating_add(columns),
                    &egui::FontId::monospace(self.font_size()),
                    pointer.x - text_x + shift,
                ) {
                    column = hit;
                }
            }
            (index, line, u32::try_from(column).unwrap_or(u32::MAX))
        } else {
            let Some((index, line)) = self.nearest_line(index, side) else {
                return;
            };
            (index, line, 0)
        };
        if pressed {
            self.active = side;
            self.pane_mut(side).place_at_column(line, column, shift);
        } else {
            self.pane_mut(side).place_at_column(line, column, true);
        }
        self.caret = index;
    }

    /// Turn the frame's keyboard events into caret moves and edits.
    fn handle_keys(&mut self, ui: &egui::Ui) {
        if !ui.is_enabled() || self.editing_blocked() {
            return;
        }
        let pass = ui.ctx().cumulative_pass_nr();
        if self.input_pass == Some(pass) {
            return;
        }
        self.input_pass = Some(pass);
        // A focused widget owns the keyboard. Without this the path fields, the
        // find field and the go to line field would type into a pane as well as
        // into themselves.
        if ui.ctx().memory(egui::Memory::focused).is_some() {
            return;
        }
        let (events, held) = ui.input(|input| (input.events.clone(), input.modifiers));
        // A shortcut is a command, never text. Some backends deliver the letter
        // of a shortcut as text beside the key press, so the modifiers of the
        // last key press in the frame decide, not the frame's own snapshot.
        let mut shortcut = held.command || held.ctrl || held.alt;
        let page_rows = u32::try_from(self.viewport_rows).unwrap_or(1).max(1);
        let mut edited = false;
        let mut copied: Option<String> = None;
        for event in events {
            match event {
                egui::Event::Text(text) => {
                    if shortcut {
                        continue;
                    }
                    let writable = !self.active_pane().is_read_only();
                    for character in text.chars() {
                        if !character.is_control() {
                            self.active_pane_mut().type_character(character);
                            edited |= writable;
                        }
                    }
                }
                egui::Event::Copy => copied = self.active_pane().copy(),
                egui::Event::Cut => {
                    let writable = !self.active_pane().is_read_only();
                    copied = self.active_pane_mut().cut();
                    edited |= writable;
                }
                egui::Event::Paste(text) => {
                    let writable = !self.active_pane().is_read_only();
                    self.active_pane_mut().paste(&text);
                    edited |= writable;
                }
                egui::Event::Key {
                    key,
                    pressed,
                    modifiers,
                    ..
                } => {
                    shortcut = modifiers.command || modifiers.ctrl || modifiers.alt;
                    if pressed {
                        let writable = !self.active_pane().is_read_only();
                        let changed = self.handle_key(key, modifiers, page_rows);
                        edited |= writable && changed;
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
        }
        self.follow_caret();
    }

    /// Move the caret to the next or previous line edited in this session.
    fn go_to_edit(&mut self, forward: bool) {
        let side = self.active;
        let here = self.pane(side).caret().line;
        let marks = match side {
            Side::Left => &self.left_edits,
            Side::Right => &self.right_edits,
        };
        let Some(line) = (if forward {
            marks.next(here)
        } else {
            marks.previous(here)
        }) else {
            return;
        };
        self.pane_mut(side)
            .place(editor::Caret::new(line, 0), false);
        self.follow_caret();
        let row = self.caret;
        self.go_to_row(row);
    }

    /// True when the active pane has any line edited in this session.
    fn has_edits(&self) -> bool {
        match self.active {
            Side::Left => !self.left_edits.is_empty(),
            Side::Right => !self.right_edits.is_empty(),
        }
    }

    /// Delete the word on one side of the caret.
    ///
    /// A file a named format claims uses that format's token boundaries, so a
    /// delete stops where the language does. Every other file uses the caret's
    /// own word movement, which follows character classes.
    fn delete_word(&mut self, forward: bool) -> bool {
        let side = self.active;
        let caret = self.pane(side).caret();
        let text = self.pane(side).line_text(caret.line);
        if let Some(target) = self.token_boundary(side, caret, &text, forward) {
            return self.pane_mut(side).delete_to(target);
        }
        self.pane_mut(side).delete_word(forward)
    }

    /// The token edge the caret's word ends at, in the caret's own line.
    fn token_boundary(
        &mut self,
        side: Side,
        caret: editor::Caret,
        text: &str,
        forward: bool,
    ) -> Option<editor::Caret> {
        let highlighter = match side {
            Side::Left => &mut self.left_syntax,
            Side::Right => &mut self.right_syntax,
        };
        if !highlighter.is_claimed() {
            return None;
        }
        let spans = highlighter.spans(caret.line, text)?;
        let tabs = self.pane(side).tabs();
        let column = editor::display_column(text, caret.index, tabs);
        let edge = if forward {
            spans
                .iter()
                .map(|span| span.end)
                .find(|end| *end > column)?
        } else {
            spans
                .iter()
                .map(|span| span.start)
                .rev()
                .find(|start| *start < column)?
        };
        Some(editor::Caret::new(
            caret.line,
            editor::index_at_column(text, edge, tabs),
        ))
    }

    /// One key press. Returns true when it changed the text.
    fn handle_key(&mut self, key: egui::Key, modifiers: egui::Modifiers, page_rows: u32) -> bool {
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
            self.active_pane_mut().move_caret(motion, shift);
            return false;
        }
        if modifiers.command {
            return match key {
                Key::Backspace => self.delete_word(false),
                Key::Delete => self.delete_word(true),
                _ => false,
            };
        }
        let ending = self.ending_of(self.active).to_owned();
        let pane = self.active_pane_mut();
        match key {
            Key::Backspace => pane.backspace(),
            Key::Delete => pane.delete(),
            Key::Enter => pane.enter(&ending),
            Key::Tab if shift => pane.decrease_indent(),
            Key::Tab => pane.tab(),
            _ => return false,
        }
        true
    }

    fn set_horizontal(&mut self, columns: f32, viewport: f32, char_width: f32) {
        let visible_columns = viewport / char_width;
        #[allow(clippy::cast_precision_loss)]
        let widest = self.data.widest as f32;
        self.horizontal = columns.clamp(0.0, (widest - visible_columns).max(0.0));
    }

    fn vertical_scrollbar(
        &mut self,
        ui: &mut egui::Ui,
        painter: &egui::Painter,
        palette: &Palette,
        body: egui::Rect,
        metrics: (f32, usize),
    ) {
        let (row_height, total) = metrics;
        let track = egui::Rect::from_min_max(
            egui::pos2(body.right(), body.top()),
            egui::pos2(body.right() + SCROLLBAR, body.bottom()),
        );
        painter.rect_filled(track, 0.0, palette.gutter_background);
        let thumb = self
            .scroll
            .thumb(track.height(), body.height(), row_height, total);
        let thumb_rect = egui::Rect::from_min_max(
            egui::pos2(track.left() + 2.0, track.top() + thumb.start),
            egui::pos2(track.right() - 2.0, track.top() + thumb.end),
        );
        painter.rect_filled(thumb_rect, 2.0, palette.thumbnail_marker);
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
    }

    fn horizontal_scrollbar(
        &mut self,
        ui: &mut egui::Ui,
        painter: &egui::Painter,
        palette: &Palette,
        body: egui::Rect,
        char_width: f32,
    ) {
        let track = egui::Rect::from_min_max(
            egui::pos2(body.left(), body.bottom()),
            egui::pos2(body.right(), body.bottom() + SCROLLBAR),
        );
        painter.rect_filled(track, 0.0, palette.gutter_background);
        let pane = ((body.width() - CENTER_WIDTH) / 2.0).max(1.0);
        let visible_columns = (pane / char_width).max(1.0);
        #[allow(clippy::cast_precision_loss)]
        let widest = (self.data.widest as f32).max(visible_columns);
        // The bar works in character cells here, which is the unit the pane
        // offset is held in.
        let moved = widgets::horizontal_scrollbar(
            ui,
            painter,
            self.id.with("horizontal-bar"),
            track,
            (palette.gutter_background, palette.thumbnail_marker),
            (widest, visible_columns, self.horizontal),
            SCROLLBAR * 2.0,
        );
        if let Some(offset) = moved {
            self.set_horizontal(offset, pane, char_width);
        }
    }
}

/// The painting class of a row, given whether unimportant differences count.
#[must_use]
pub const fn row_class(row: &model::Row, ignore_unimportant: bool) -> TextClass {
    match (row.class, row.importance) {
        (RowClass::Same, _) => TextClass::Same,
        (RowClass::LeftOnly | RowClass::RightOnly, _) => TextClass::Orphan,
        (RowClass::Changed, Some(Importance::Unimportant)) => {
            if ignore_unimportant {
                TextClass::Same
            } else {
                TextClass::Unimportant
            }
        }
        (RowClass::Changed, _) => TextClass::Important,
    }
}

/// A run of columns painted in one color.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Run {
    /// First column of the run.
    pub start: u32,
    /// One past the last column of the run.
    pub end: u32,
    /// The color the run is painted in.
    pub color: egui::Color32,
}

/// The gap rows on each side that are still waiting for their hatch.
///
/// Adjacent gap rows merge into one block, so the hatch costs the visible gap
/// area once rather than once per row.
#[derive(Debug, Default)]
struct GapRuns {
    left: Vec<(f32, f32)>,
    right: Vec<(f32, f32)>,
    open: [Option<(f32, f32)>; 2],
}

impl GapRuns {
    fn mark(&mut self, side: Side, gap: bool, top: f32, bottom: f32) {
        let index = usize::from(side == Side::Right);
        if gap {
            match &mut self.open[index] {
                Some(block) if (block.1 - top).abs() < 0.5 => block.1 = bottom,
                slot => {
                    if let Some(block) = slot.take() {
                        self.finished(side).push(block);
                    }
                    self.open[index] = Some((top, bottom));
                }
            }
            return;
        }
        if let Some(block) = self.open[index].take() {
            self.finished(side).push(block);
        }
    }

    fn finished(&mut self, side: Side) -> &mut Vec<(f32, f32)> {
        match side {
            Side::Left => &mut self.left,
            Side::Right => &mut self.right,
        }
    }

    fn paint(
        &mut self,
        painter: &egui::Painter,
        columns: (egui::Rangef, egui::Rangef),
        color: egui::Color32,
    ) {
        for side in [Side::Left, Side::Right] {
            let index = usize::from(side == Side::Right);
            if let Some(block) = self.open[index].take() {
                self.finished(side).push(block);
            }
            let span = if side == Side::Left {
                columns.0
            } else {
                columns.1
            };
            let blocks = std::mem::take(self.finished(side));
            for (top, bottom) in blocks {
                paint_hatch(
                    painter,
                    egui::Rect::from_x_y_ranges(span, egui::Rangef::new(top, bottom)),
                    color,
                );
            }
        }
    }
}

/// Fill a rectangle with evenly spaced diagonal lines.
///
/// The count of lines follows the rectangle, so the cost of a frame follows the
/// gap rows the viewport shows and nothing else.
fn paint_hatch(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let clip = painter.with_clip_rect(rect);
    let stroke = egui::Stroke::new(1.0, color);
    let height = rect.height();
    let mut x = rect.left() - height;
    while x < rect.right() {
        clip.line_segment(
            [
                egui::pos2(x, rect.bottom()),
                egui::pos2(x + height, rect.top()),
            ],
            stroke,
        );
        x += HATCH_STEP;
    }
}

/// Split a line into colored runs.
///
/// Difference coloring wins over syntax coloring where both claim a column, and
/// every other column takes the pane's own text color. The walk visits each
/// syntax run and each difference run once, so a row costs the runs it shows.
fn build_runs(
    out: &mut Vec<Run>,
    window: std::ops::Range<u32>,
    base: egui::Color32,
    syntax: &[syntax::Span],
    palette: &Palette,
    difference: &[ColumnSpan],
    difference_color: egui::Color32,
) {
    out.clear();
    let columns = window.end;
    let mut position = window.start;
    let mut run = 0usize;
    let mut span = 0usize;
    while position < columns {
        while run < syntax.len() && syntax[run].end <= position {
            run += 1;
        }
        while span < difference.len() && difference[span].end <= position {
            span += 1;
        }
        let in_difference = difference
            .get(span)
            .is_some_and(|entry| entry.start <= position);
        let in_syntax = syntax.get(run).is_some_and(|entry| entry.start <= position);
        // Text no grammar element claims carries the pane's own text color.
        let claimed = in_syntax && syntax[run].slot != ca_grammar::StyleSlot::Plain;
        let (color, mut next) = if in_difference {
            (difference_color, difference[span].end)
        } else if claimed {
            (palette.syntax_slot(syntax[run].slot), syntax[run].end)
        } else {
            (base, columns)
        };
        if !in_difference {
            if let Some(entry) = difference.get(span) {
                next = next.min(entry.start.max(position + 1));
            }
            if !claimed {
                if let Some(entry) = syntax.get(run) {
                    let edge = if in_syntax { entry.end } else { entry.start };
                    next = next.min(edge.max(position + 1));
                }
            }
        }
        let next = next.min(columns).max(position + 1);
        match out.last_mut() {
            Some(last) if last.color == color && last.end == position => last.end = next,
            _ => out.push(Run {
                start: position,
                end: next,
                color,
            }),
        }
        position = next;
    }
}

struct PaneGeometry {
    gutter_x: f32,
    text_x: f32,
    width: f32,
    gutter_width: f32,
    y: f32,
    row_height: f32,
    char_width: f32,
    first_column: usize,
    columns: usize,
    column_shift: f32,
}

/// What turning a pointer position into a caret needs.
struct PointerGeometry {
    left_text_x: f32,
    right_text_x: f32,
    /// The boundary between the two panes. A pointer left of it belongs to the
    /// left pane whatever the gutters and the centre column are wide.
    center_x: f32,
    char_width: f32,
    row_height: f32,
    first_column: usize,
}

/// The distance one column of monospaced text advances when painted.
///
/// Layout rounds every glyph advance to whole pixels, so painted text steps by
/// the rounded advance. A grid built on the unrounded advance drifts away from
/// the painted glyphs by a fraction of a pixel per column, and a caret or a
/// click far along a line lands on a different character than the one shown.
fn column_pitch(ui: &egui::Ui, font: &egui::FontId) -> f32 {
    ui.fonts(|fonts| {
        let scale = fonts.pixels_per_point();
        (fonts.glyph_width(font, '0') * scale).round() / scale
    })
    .max(1.0)
}

fn glyph_column_at_x(
    painter: &egui::Painter,
    text: &str,
    metrics: lines::LineMetrics,
    window: std::ops::Range<usize>,
    font: &egui::FontId,
    x: f32,
) -> Option<usize> {
    if metrics.direct || text.is_ascii() {
        return None;
    }
    let visible = lines::window(
        text,
        metrics,
        window.start,
        window.end.saturating_sub(window.start),
    );
    let galley = painter.layout_no_wrap(
        visible.into_owned(),
        font.clone(),
        painter.ctx().style().visuals.text_color(),
    );
    Some(window.start + galley.cursor_from_pos(egui::vec2(x, 0.0)).ccursor.index)
}

/// Paint the selection band and the caret of one pane on one row.
///
/// Only the columns the pane covers are drawn, so the cost follows the pane
/// rather than the length of the line.
fn paint_caret(
    painter: &egui::Painter,
    geometry: &PaneGeometry,
    pane: &Pane,
    line: Option<u32>,
    text: &str,
    active: bool,
    style: (&Palette, &egui::FontId, lines::LineMetrics),
) {
    let (palette, font, metrics) = style;
    let Some(line) = line else {
        return;
    };
    let row_rect = egui::Rect::from_min_size(
        egui::pos2(geometry.text_x, geometry.y),
        egui::vec2(geometry.width, geometry.row_height),
    );
    let clip = painter.with_clip_rect(row_rect);
    let origin = geometry.text_x - geometry.column_shift;
    let galley = (!metrics.direct && !text.is_ascii()).then(|| {
        let window = lines::window(text, metrics, geometry.first_column, geometry.columns);
        painter.layout_no_wrap(window.into_owned(), font.clone(), palette.same_text)
    });
    let column_x = |column: u32| {
        if let Some(galley) = &galley {
            if let Some(index) = usize::try_from(column)
                .ok()
                .and_then(|column| column.checked_sub(geometry.first_column))
            {
                if index <= galley.job.text.chars().count() {
                    return origin
                        + galley
                            .pos_from_ccursor(egui::text::CCursor::new(index))
                            .left();
                }
            }
        }
        #[allow(clippy::cast_precision_loss)]
        let steps = column as f32 - geometry.first_column as f32;
        steps.mul_add(geometry.char_width, origin)
    };
    if let Some((start, end)) = pane.selection() {
        if line >= start.line && line <= end.line {
            let from = if line == start.line {
                editor::display_column(text, start.index, pane.tabs())
            } else {
                0
            };
            let to = if line == end.line {
                editor::display_column(text, end.index, pane.tabs())
            } else {
                editor::display_column(text, u32::MAX, pane.tabs()).saturating_add(1)
            };
            let x0 = column_x(from);
            let x1 = column_x(to).max(x0 + 2.0);
            clip.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(x0, geometry.y),
                    egui::pos2(x1, geometry.y + geometry.row_height),
                ),
                0.0,
                palette.selection,
            );
        }
    }
    if !active || pane.caret().line != line {
        return;
    }
    let x = column_x(editor::display_column(
        text,
        pane.caret().index,
        pane.tabs(),
    ));
    clip.line_segment(
        [
            egui::pos2(x, geometry.y),
            egui::pos2(x, geometry.y + geometry.row_height),
        ],
        egui::Stroke::new(
            if pane.is_overwrite() { 3.0 } else { 1.5 },
            palette.thumbnail_caret,
        ),
    );
}

struct PaneContent<'a> {
    line: Option<u32>,
    text: Option<&'a str>,
    metrics: Option<lines::LineMetrics>,
    /// The syntax runs of the line, in columns.
    syntax: Option<&'a [syntax::Span]>,
    spans: Option<&'a [ColumnSpan]>,
    background: egui::Color32,
    /// Color of the characters that are not part of a difference.
    base_color: egui::Color32,
    /// Color of the characters that are part of a difference.
    difference_color: egui::Color32,
    /// Every character of the line is part of the difference.
    whole_line_differs: bool,
    /// The row differs, so syntax coloring gives way where the two meet.
    is_difference: bool,
    gap: bool,
}

/// Paint one row of one pane.
///
/// Only the columns the pane covers are laid out, so a row costs the width of
/// the pane whatever the length of the line behind it.
#[allow(clippy::too_many_lines)]
fn paint_side(
    painter: &egui::Painter,
    geometry: &PaneGeometry,
    content: &PaneContent<'_>,
    palette: &Palette,
    font: &egui::FontId,
    runs: &mut Vec<Run>,
) {
    let row_rect = egui::Rect::from_min_size(
        egui::pos2(geometry.text_x, geometry.y),
        egui::vec2(geometry.width, geometry.row_height),
    );
    painter.rect_filled(row_rect, 0.0, content.background);
    let gutter_rect = egui::Rect::from_min_size(
        egui::pos2(geometry.gutter_x, geometry.y),
        egui::vec2(geometry.gutter_width, geometry.row_height),
    );
    painter.rect_filled(gutter_rect, 0.0, palette.gutter_background);
    if let Some(line) = content.line {
        painter.text(
            gutter_rect.right_center() - egui::vec2(3.0, 0.0),
            egui::Align2::RIGHT_CENTER,
            (line + 1).to_string(),
            font.clone(),
            palette.gutter_text,
        );
    }
    if content.gap {
        return;
    }
    let (Some(text), Some(metrics)) = (content.text, content.metrics) else {
        return;
    };
    let origin = geometry.text_x - geometry.column_shift;
    let clip = painter.with_clip_rect(row_rect);
    if metrics.columns == 0 {
        return;
    }
    // A plain row with no grammar and no inline runs behind it paints in one
    // call, which is what a plain text comparison costs.
    let plain = content.syntax.is_none() && (content.whole_line_differs || content.spans.is_none());
    if plain {
        let window = lines::window(text, metrics, geometry.first_column, geometry.columns);
        if window.is_empty() {
            return;
        }
        clip.text(
            egui::pos2(origin, geometry.y),
            egui::Align2::LEFT_TOP,
            window.as_ref(),
            font.clone(),
            if content.whole_line_differs {
                content.difference_color
            } else {
                content.base_color
            },
        );
        return;
    }
    #[allow(clippy::cast_possible_truncation)]
    let window = u32::try_from(geometry.first_column).unwrap_or(u32::MAX)
        ..metrics.columns.min(
            u32::try_from(geometry.first_column.saturating_add(geometry.columns))
                .unwrap_or(u32::MAX),
        );
    if content.whole_line_differs {
        runs.clear();
        runs.push(Run {
            start: window.start,
            end: window.end,
            color: content.difference_color,
        });
    } else {
        // Difference coloring wins where it meets syntax coloring, so a row that
        // differs hands its runs in as the top layer.
        let empty: [ColumnSpan; 0] = [];
        build_runs(
            runs,
            window,
            content.base_color,
            content.syntax.unwrap_or(&[]),
            palette,
            if content.is_difference {
                content.spans.unwrap_or(&empty)
            } else {
                &empty
            },
            content.difference_color,
        );
    }
    let last = geometry.first_column.saturating_add(geometry.columns);
    let mut job = egui::text::LayoutJob::default();
    for run in runs.iter() {
        if run.end as usize <= geometry.first_column || run.start as usize >= last {
            continue;
        }
        let piece = lines::window(
            text,
            metrics,
            run.start as usize,
            (run.end - run.start) as usize,
        );
        if piece.is_empty() {
            continue;
        }
        job.append(
            piece.as_ref(),
            0.0,
            egui::TextFormat {
                font_id: font.clone(),
                color: run.color,
                ..Default::default()
            },
        );
    }
    // Separate layouts round each span's glyph advances independently and shift
    // the text at color boundaries. One layout retains a continuous glyph grid.
    let galley = clip.layout_job(job);
    clip.galley(egui::pos2(origin, geometry.y), galley, content.base_color);
}

fn filter_label(filter: DisplayFilter) -> &'static str {
    match filter {
        DisplayFilter::All => "Show All",
        DisplayFilter::Differences => "Show Differences",
        DisplayFilter::Same => "Show Same",
        DisplayFilter::Context(_) => "Show Context",
        DisplayFilter::None => "Show None",
    }
}

fn file_name(path: &Path) -> String {
    match path.file_name() {
        Some(name) => name.to_string_lossy().into_owned(),
        None => path.display().to_string(),
    }
}

impl ca_ui::view::ViewFactory for TextView {
    fn create(left: PathBuf, right: PathBuf, context: &ViewContext, salt: u64) -> Self {
        Self::new(left, right, context, salt)
    }

    fn create_from(request: &ca_ui::view::OpenRequest, context: &ViewContext, salt: u64) -> Self {
        let mut view = Self::blank(request.left.clone(), request.right.clone(), context, salt);
        view.left_title.clone_from(&request.titles.left);
        view.right_title.clone_from(&request.titles.right);
        view.read_only = request.read_only;
        view.restart();
        view
    }
}

impl TextView {
    fn run_buffer_edit(&mut self, command: Command) {
        match command {
            Command::Undo => {
                if self.active_pane_mut().undo() {
                    self.note_edit();
                    if let Some(row) = self.caret_row() {
                        self.go_to_row(row);
                    }
                }
            }
            Command::Redo => {
                if self.active_pane_mut().redo() {
                    self.note_edit();
                    if let Some(row) = self.caret_row() {
                        self.go_to_row(row);
                    }
                }
            }
            Command::SelectAll => self.active_pane_mut().select_all(),
            Command::Copy => self.pending_clipboard = self.active_pane().copy(),
            Command::Cut => {
                self.pending_clipboard = self.active_pane_mut().cut();
                self.note_edit();
            }
            Command::Paste => self.paste_requested = true,
            Command::SelectSection => self.select_section(),
            Command::IncreaseIndent => {
                self.active_pane_mut().increase_indent();
                self.note_edit();
            }
            Command::DecreaseIndent => {
                self.active_pane_mut().decrease_indent();
                self.note_edit();
            }
            _ => {}
        }
    }
}

impl SessionView for TextView {
    fn kind(&self) -> Option<ca_session::SessionKind> {
        Some(ca_session::SessionKind::TextCompare)
    }

    fn menu_view(&self) -> ca_ui::command::MenuView {
        ca_ui::command::MenuView::Text
    }

    fn title(&self) -> String {
        let modified = if self.left_pane.is_modified() || self.right_pane.is_modified() {
            "* "
        } else {
            ""
        };
        let left = self
            .left_title
            .as_deref()
            .filter(|title| !title.is_empty())
            .map_or_else(|| file_name(&self.left_path), str::to_owned);
        let right = self
            .right_title
            .as_deref()
            .filter(|title| !title.is_empty())
            .map_or_else(|| file_name(&self.right_path), str::to_owned);
        format!("{modified}{left} - {right}")
    }

    fn tick(&mut self) {
        self.poll();
        self.poll_rediff();
        self.poll_save();
        self.poll_find();
        self.left_syntax.poll(&self.notify);
        self.right_syntax.poll(&self.notify);
    }

    fn ui(&mut self, ui: &mut egui::Ui, context: &ViewContext) -> Vec<ViewAction> {
        let now = Instant::now();
        // A load in progress replaces the pane text a comparison would read,
        // so a due comparison waits for it and then reads the loaded text.
        if self.job.is_none() {
            if self.rediff.is_due(now) {
                self.start_rediff();
            } else if let Some(remaining) = self.rediff.remaining(now) {
                ui.ctx().request_repaint_after(remaining);
            }
        }
        self.follow_options(ui.ctx());
        self.jump_to_first_difference_once();
        let filter_name = self.report_filter();
        self.report.poll_with(ui.ctx(), |settings| {
            settings.select_filter(filter_name);
        });
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
            self.path_bar(ui);
            self.file_info(ui);
            if let Some(action) = self.toolbar(ui, &context.palette) {
                actions.push(action);
            }
            self.find_panel(ui);
            self.goto_panel(ui);
            self.prompt_panel(ui);
            self.report_panel(ui);
        });
        egui::TopBottomPanel::bottom(self.id.with("status")).show_inside(ui, |ui| {
            self.status_bar(ui);
        });
        if self.show_details {
            egui::TopBottomPanel::bottom(self.id.with("details"))
                .exact_height(self.row_height().mul_add(2.0, SCROLLBAR))
                .resizable(false)
                .show_inside(ui, |ui| {
                    self.details(ui, &context.palette);
                });
        }
        egui::SidePanel::left(self.id.with("thumb"))
            .exact_width(THUMBNAIL_WIDTH)
            .resizable(false)
            .show_inside(ui, |ui| {
                self.thumbnail_strip(ui, &context.palette);
            });
        egui::CentralPanel::default().show_inside(ui, |ui| {
            self.rows(ui, &context.palette);
        });
        actions
    }

    fn commands(&self) -> Vec<ca_ui::view::CommandState> {
        ca_ui::view::declare(HANDLED, |command| self.accepts(command))
    }

    fn accepts(&self, command: Command) -> bool {
        if self.editing_off() && command_is_save_or_overwrite(command) {
            return false;
        }
        if self.locked && command_is_disabled_while_locked(command) {
            return false;
        }
        match command {
            Command::ToggleSyntaxHighlighting => {
                self.status == Status::Ready
                    && (self.left_syntax.is_claimed() || self.right_syntax.is_claimed())
            }
            Command::PrettifyForComparison | Command::CompareStructure => {
                self.status == Status::Ready
                    && self.format_job.is_none()
                    && self.save_job.is_none()
                    && !self.left_pane.is_modified()
                    && !self.right_pane.is_modified()
                    && self.is_settled()
                    && if command == Command::CompareStructure {
                        self.structural_summary().is_some()
                            || (self.formatted_compare.is_none()
                                && structure::pair_format(&self.left_path, &self.right_path)
                                    .is_some()
                                && !self.data.left.had_errors
                                && !self.data.right.had_errors)
                    } else {
                        self.formatted_compare.is_some()
                            || prettify::pair_format(&self.left_path, &self.right_path).is_some()
                    }
            }
            Command::NextDifference
            | Command::PreviousDifference
            | Command::NextSection
            | Command::PreviousSection
            | Command::ShowAll
            | Command::ShowDifferences
            | Command::ShowSame
            | Command::ShowContext
            | Command::SwapSides
            | Command::Find
            | Command::FindNext
            | Command::FindPrevious
            | Command::GoTo
            | Command::SelectAll
            | Command::SelectSection
            | Command::Copy
            | Command::ToggleOverwrite
            | Command::ClearBookmarks
            | Command::CompareReport => self.status == Status::Ready,
            Command::IncreaseFontSize
            | Command::DecreaseFontSize
            | Command::ResetFontSize
            | Command::ToggleLineDetails
            | Command::ToggleLineNumbers
            | Command::ToggleIgnoreUnimportant
            | Command::Reload => true,
            Command::OpenFile => self.picker.is_none(),
            Command::CopyToOtherSide => {
                self.status == Status::Ready && !self.pane(self.active.other()).is_read_only()
            }
            Command::Undo => {
                !self.active_pane().is_read_only() && self.active_pane().buffer().can_undo()
            }
            Command::Redo => {
                !self.active_pane().is_read_only() && self.active_pane().buffer().can_redo()
            }
            Command::Cut
            | Command::Paste
            | Command::Replace
            | Command::IncreaseIndent
            | Command::DecreaseIndent => {
                self.status == Status::Ready && !self.active_pane().is_read_only()
            }
            Command::CopyToRight | Command::CopyLineToRight => {
                self.status == Status::Ready && !self.right_pane.is_read_only()
            }
            Command::CopyToLeft | Command::CopyLineToLeft => {
                self.status == Status::Ready && !self.left_pane.is_read_only()
            }
            Command::SaveFile => self.active_pane().is_modified() && self.save_job.is_none(),
            Command::SaveFileAs => {
                self.status == Status::Ready && self.save_job.is_none() && self.picker.is_none()
            }
            Command::SaveBoth => {
                (self.left_pane.is_modified() || self.right_pane.is_modified())
                    && self.save_job.is_none()
            }
            Command::NextEdit | Command::PreviousEdit => {
                self.status == Status::Ready && self.has_edits()
            }
            Command::Cancel => {
                self.status.is_running() || self.find_task.is_running() || self.find.is_find_open()
            }
            _ => false,
        }
    }

    fn run(&mut self, command: Command) {
        match command {
            Command::NextDifference
            | Command::PreviousDifference
            | Command::NextSection
            | Command::PreviousSection => self.navigate(command),
            Command::CompareReport => self.report.request(),
            Command::ShowAll => self.set_filter(DisplayFilter::All),
            Command::ShowDifferences => self.set_filter(DisplayFilter::Differences),
            Command::ShowSame => self.set_filter(DisplayFilter::Same),
            Command::ShowContext => self.set_filter(DisplayFilter::Context(self.context_lines)),
            Command::IncreaseFontSize | Command::DecreaseFontSize | Command::ResetFontSize => {
                self.font.run(command);
                self.strip_stale = true;
            }
            Command::Reload => self.request_reread(Reread::Reload),
            Command::OpenFile => {
                if self.formatted_compare.is_some() {
                    self.toggle_prettified_compare();
                }
                self.open_picker(match self.active {
                    Side::Left => Target::Left,
                    Side::Right => Target::Right,
                });
            }
            Command::ToggleSyntaxHighlighting => {
                self.syntax_highlighting = !self.syntax_highlighting;
                self.line_cache.clear();
            }
            Command::PrettifyForComparison => self.toggle_prettified_compare(),
            Command::CompareStructure => self.toggle_structure(),
            Command::SwapSides => self.request_reread(Reread::SwapSides),
            Command::Cancel => {
                self.find_task.cancel();
                if let Some(job) = self.format_job.take() {
                    job.cancel();
                    self.status = Status::Ready;
                    self.set_panes_editable();
                    self.message = Some("Formatting stopped; the source text is unchanged.".into());
                    return;
                }
                if !self.status.is_running() {
                    self.find.close();
                    self.message = None;
                    return;
                }
                if let Some(job) = self.job.as_ref() {
                    job.cancel();
                }
                self.rediff.clear();
                self.find.close();
                self.message = None;
            }
            Command::Undo
            | Command::Redo
            | Command::SelectAll
            | Command::Copy
            | Command::Cut
            | Command::Paste
            | Command::SelectSection
            | Command::IncreaseIndent
            | Command::DecreaseIndent => self.run_buffer_edit(command),
            Command::ToggleLineDetails => self.show_details = !self.show_details,
            Command::ToggleLineNumbers => self.show_line_numbers = !self.show_line_numbers,
            Command::ToggleIgnoreUnimportant => {
                self.set_ignore_unimportant(!self.ignore_unimportant);
            }
            Command::CopyToOtherSide => self.copy_to(self.active.other(), false),
            Command::CopyToRight => self.copy_to(Side::Right, false),
            Command::CopyToLeft => self.copy_to(Side::Left, false),
            Command::CopyLineToRight => self.copy_to(Side::Right, true),
            Command::CopyLineToLeft => self.copy_to(Side::Left, true),
            Command::SaveFile => {
                let side = self.active;
                self.save_side(side, SaveConsent::default());
            }
            Command::SaveFileAs => self.open_save_picker(),
            Command::SaveBoth => self.save_both(),
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
            Command::NextEdit => self.go_to_edit(true),
            Command::PreviousEdit => self.go_to_edit(false),
            Command::GoTo => self.find.open_go_to(),
            Command::ClearBookmarks => self.bookmarks.clear(),
            Command::ToggleOverwrite => self.active_pane_mut().toggle_overwrite(),
            _ => {}
        }
    }

    fn wants_close(&self) -> bool {
        self.close_requested
            && !self.left_pane.is_modified()
            && !self.right_pane.is_modified()
            && self.save_job.is_none()
    }

    fn is_busy(&self) -> bool {
        self.save_job.is_some()
    }

    fn may_close(&mut self) -> bool {
        if matches!(self.prompt, Some(Prompt::KeptCopy(_))) {
            return false;
        }
        if self.save_job.is_some() {
            return false;
        }
        if !ca_ui::save::needs_close_prompt(
            self.left_pane.is_modified(),
            self.right_pane.is_modified(),
        ) {
            return true;
        }
        self.prompt = Some(Prompt::Closing);
        false
    }

    fn apply_settings(&mut self, settings: &ca_session::settings::SessionSettings) {
        if let ca_session::settings::SessionSettings::TextCompare(text) = settings {
            self.apply_session_settings(text.clone());
        }
    }

    fn settings(&self) -> Option<ca_session::settings::SessionSettings> {
        let mut settings = self.session_settings.clone();
        settings.specs =
            ca_ui::view::with_sides(&settings.specs, &self.left_path, &self.right_path);
        Some(ca_session::settings::SessionSettings::TextCompare(settings))
    }

    fn holds_unwritten_edits(&self) -> bool {
        self.left_pane.is_modified() || self.right_pane.is_modified()
    }

    fn is_ready(&self) -> bool {
        !self.status.is_running()
    }

    fn launch_target(&self) -> Option<ca_ui::launch::LaunchTarget> {
        use ca_session::options::{LaunchContext, LaunchSide};
        if self.left_path.as_os_str().is_empty() {
            return None;
        }
        let line = u32::try_from(self.caret.saturating_add(1)).ok();
        let side = |path: &std::path::Path| LaunchSide {
            path: path.to_path_buf(),
            base: path.parent().map(std::path::Path::to_path_buf),
            line,
        };
        Some(ca_ui::launch::LaunchTarget::files(LaunchContext {
            first: side(&self.left_path),
            second: (!self.right_path.as_os_str().is_empty()).then(|| side(&self.right_path)),
        }))
    }

    fn explorer_target(&self) -> Option<(PathBuf, ca_ui::launch::Selection)> {
        let path = match self.active_side() {
            Side::Left => &self.left_path,
            Side::Right => &self.right_path,
        };
        (!path.as_os_str().is_empty()).then(|| (path.clone(), ca_ui::launch::Selection::Files))
    }

    fn on_close(&mut self) {
        if let Some(job) = self.format_job.take() {
            job.cancel();
        }
        if let Some(job) = self.job.take() {
            job.cancel();
        }
        if let Some(job) = self.picker.take() {
            job.cancel();
        }
        self.rediff.clear();
        self.left_syntax.clear();
        self.right_syntax.clear();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    #[test]
    fn unicode_caret_and_selection_edges_follow_the_painted_glyphs() {
        let context = egui::Context::default();
        for text in ["日本語x", "e\u{301}xy", "שלום עולם"] {
            let mut pane = super::Pane::from_text(text);
            let start = usize::from(!text.starts_with("e\u{301}"));
            pane.place(
                super::editor::Caret::new(0, u32::try_from(start).unwrap()),
                false,
            );
            pane.place(super::editor::Caret::new(0, 2), true);
            let output = context.run(ca_ui::testing::sized_input(500.0, 200.0), |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    let font = egui::FontId::monospace(13.0);
                    let palette = ca_ui::testing::context().palette;
                    let geometry = super::PaneGeometry {
                        gutter_x: 0.0,
                        text_x: 30.0,
                        width: 400.0,
                        gutter_width: 20.0,
                        y: 10.0,
                        row_height: 20.0,
                        char_width: ui.fonts(|fonts| fonts.glyph_width(&font, '0')),
                        first_column: 0,
                        columns: 80,
                        column_shift: 0.0,
                    };
                    let row_content = super::PaneContent {
                        line: None,
                        text: Some(text),
                        metrics: Some(super::lines::measure(text)),
                        syntax: None,
                        spans: None,
                        background: palette.same_line,
                        base_color: palette.same_text,
                        difference_color: palette.important_text,
                        whole_line_differs: false,
                        is_difference: false,
                        gap: false,
                    };
                    super::paint_side(
                        ui.painter(),
                        &geometry,
                        &row_content,
                        &palette,
                        &font,
                        &mut Vec::new(),
                    );
                    super::paint_caret(
                        ui.painter(),
                        &geometry,
                        &pane,
                        Some(0),
                        text,
                        true,
                        (&palette, &font, super::lines::measure(text)),
                    );
                });
            });
            let painted = output
                .shapes
                .iter()
                .find_map(|shape| {
                    if let egui::Shape::Text(text) = &shape.shape {
                        Some(text)
                    } else {
                        None
                    }
                })
                .unwrap();
            let glyphs = &painted.galley.rows[0].glyphs;
            let expected = painted.pos.x + glyphs[2].pos.x;
            let caret = output
                .shapes
                .iter()
                .find_map(|shape| {
                    if let egui::Shape::LineSegment { points, .. } = &shape.shape {
                        Some(points[0].x)
                    } else {
                        None
                    }
                })
                .unwrap();
            assert!(
                (caret - expected).abs() < 0.01,
                "{text}: caret {caret}, glyph {expected}"
            );
            let selection = output
                .shapes
                .iter()
                .find_map(|shape| {
                    if let egui::Shape::Rect(rect) = &shape.shape {
                        (rect.fill == ca_ui::testing::context().palette.selection)
                            .then_some(rect.rect)
                    } else {
                        None
                    }
                })
                .unwrap();
            assert!((selection.left() - (painted.pos.x + glyphs[start].pos.x)).abs() < 0.01);
            assert!((selection.right() - expected.max(selection.left() + 2.0)).abs() < 0.01);
        }
    }

    #[test]
    fn inline_colors_preserve_the_plain_line_glyph_positions() {
        let ctx = egui::Context::default();
        for scale in [1.0, 1.25, 1.5, 2.0] {
            ctx.set_pixels_per_point(scale);
            for (text, start, end) in [
                ("u32>", 1, 3),
                ("&mut", 1, 4),
                ("limit, hits", 5, 6),
                ("a\tbc\td", 1, 9),
                ("日本語テキスト", 2, 4),
                ("e\u{301}xy", 1, 3),
                ("שלום עולם", 2, 6),
            ] {
                for first_column in [0, 1] {
                    let mut positions = Vec::new();
                    for highlighted in [false, true] {
                        let output = ctx.run(ca_ui::testing::sized_input(500.0, 200.0), |ctx| {
                            egui::CentralPanel::default().show(ctx, |ui| {
                                let font = egui::FontId::monospace(13.0);
                                let spans = [super::ColumnSpan { start, end }];
                                let content = super::PaneContent {
                                    line: None,
                                    text: Some(text),
                                    metrics: Some(super::lines::measure(text)),
                                    syntax: None,
                                    spans: highlighted.then_some(spans.as_slice()),
                                    background: ca_ui::testing::context().palette.same_line,
                                    base_color: ca_ui::testing::context().palette.same_text,
                                    difference_color: ca_ui::testing::context()
                                        .palette
                                        .important_text,
                                    whole_line_differs: false,
                                    is_difference: highlighted,
                                    gap: false,
                                };
                                let geometry = super::PaneGeometry {
                                    gutter_x: 0.0,
                                    text_x: 30.0,
                                    width: 400.0,
                                    gutter_width: 20.0,
                                    y: 10.0,
                                    row_height: 20.0,
                                    char_width: ui.fonts(|fonts| fonts.glyph_width(&font, '0')),
                                    first_column,
                                    columns: 80,
                                    column_shift: 0.3,
                                };
                                super::paint_side(
                                    ui.painter(),
                                    &geometry,
                                    &content,
                                    &ca_ui::testing::context().palette,
                                    &font,
                                    &mut Vec::new(),
                                );
                            });
                        });
                        let glyphs: Vec<_> = output
                            .shapes
                            .iter()
                            .filter_map(|shape| {
                                if let egui::Shape::Text(text) = &shape.shape {
                                    Some(text)
                                } else {
                                    None
                                }
                            })
                            .flat_map(|text| {
                                text.galley.rows.iter().flat_map(move |row| {
                                    row.glyphs
                                        .iter()
                                        .map(move |glyph| (glyph.chr, text.pos.x + glyph.pos.x))
                                })
                            })
                            .collect();
                        positions.push(glyphs);
                    }
                    assert_eq!(
                        positions[0], positions[1],
                        "{text}, scale {scale}, column {first_column}"
                    );
                }
            }
        }
    }
    use super::{build_runs, file_name, filter_label, row_class, DisplayFilter, Run};
    use crate::jobs::ColumnSpan;
    use ca_ui::theme::{palette, TextClass, Variant};
    use std::path::PathBuf;

    #[test]
    fn caller_titles_name_the_panes_without_replacing_the_file_paths() {
        use super::TextView;
        use ca_session::SessionKind;
        use ca_ui::view::{OpenRequest, SessionView, Titles, ViewFactory};

        let left = PathBuf::from("temporary-left.txt");
        let right = PathBuf::from("temporary-right.txt");
        let request = OpenRequest::new(SessionKind::TextCompare, left.clone(), right.clone())
            .with_titles(Titles {
                left: Some("Checked out source".to_owned()),
                right: Some("Workspace version".to_owned()),
                ..Titles::default()
            });
        let mut view = TextView::create_from(&request, &ca_ui::testing::context(), 1);

        assert_eq!(view.title(), "Checked out source - Workspace version");
        assert_eq!(view.left_title.as_deref(), Some("Checked out source"));
        assert_eq!(view.right_title.as_deref(), Some("Workspace version"));
        assert_eq!(view.left_field, left.display().to_string());
        assert_eq!(view.right_field, right.display().to_string());

        view.reread(super::Reread::SwapSides);

        assert_eq!(view.title(), "Workspace version - Checked out source");
        assert_eq!(view.left_path, right);
        assert_eq!(view.right_path, left);
    }

    #[test]
    fn open_file_is_available_for_text_comparisons() {
        use super::{Command, Side, TextView};
        use ca_ui::view::SessionView;

        let mut view = TextView::from_data(
            PathBuf::from("left.txt"),
            PathBuf::from("right.txt"),
            &ca_ui::testing::context(),
            1,
            crate::jobs::TextData::default(),
        );
        view.active = Side::Right;

        assert!(view
            .commands()
            .iter()
            .any(|state| state.command == Command::OpenFile && state.enabled));
        assert_eq!(view.active_side(), Side::Right);
        view.locked = true;
        assert!(!view.accepts(Command::OpenFile));
    }

    #[test]
    fn a_recognized_language_highlight_can_be_turned_on_and_off() {
        use super::{Command, TextView};
        use ca_ui::view::SessionView;
        use std::time::{Duration, Instant};

        let directory = tempfile::tempdir().unwrap();
        let left = directory.path().join("main.go");
        let right = directory.path().join("copy.go");
        std::fs::write(&left, "func main() {}\n").unwrap();
        std::fs::write(&right, "func main() {}\n").unwrap();
        let mut view = TextView::new(left, right, &ca_ui::testing::context(), 1);
        let deadline = Instant::now() + Duration::from_secs(20);
        while (!view.is_ready() || !view.syntax_converged()) && Instant::now() < deadline {
            view.tick();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(view.is_ready());
        assert!(view.syntax_converged());
        assert!(view.accepts(Command::ToggleSyntaxHighlighting));

        view.refresh_line_cache(&(0..view.visible.len()));
        assert!(view.line_cache[&(0, 0)].spans.is_some());
        view.run(Command::ToggleSyntaxHighlighting);
        view.refresh_line_cache(&(0..view.visible.len()));
        assert!(view.line_cache[&(0, 0)].spans.is_none());
    }

    /// Formatting needs content, not copies of potentially large deleted text
    /// in the live undo and redo histories.
    #[test]
    fn the_formatter_worker_receives_content_without_live_edit_history() {
        use super::{Command, FormatMessage, Side, TextView};
        use ca_ui::view::SessionView;
        use std::time::Duration;

        let directory = tempfile::tempdir().unwrap();
        let left = directory.path().join("left.json");
        let right = directory.path().join("right.json");
        std::fs::write(&left, r#"{"value":1}"#).unwrap();
        std::fs::write(&right, r#"{"value":1}"#).unwrap();
        let mut view = TextView::new(left, right, &ca_ui::testing::context(), 1);
        assert!(ca_ui::testing::wait_until(Duration::from_secs(20), || {
            view.tick();
            view.is_settled()
        }));
        view.left_pane.buffer_mut().insert(0, " ");
        view.left_pane.mark_saved();
        view.right_pane
            .buffer_mut()
            .insert(0, &" ".repeat(1024 * 1024));
        assert!(view.right_pane.undo());
        view.right_pane.mark_saved();
        assert!(view.left_pane.buffer().can_undo());
        assert!(view.right_pane.buffer().can_redo());

        view.run(Command::PrettifyForComparison);
        let mut job = view.format_job.take().unwrap();
        let messages = job.wait(Duration::from_secs(20));
        let original = messages
            .into_iter()
            .find_map(|message| match message {
                FormatMessage::Ready { original, .. } => Some(original),
                _ => None,
            })
            .unwrap();
        assert!(
            !original.left.can_undo(),
            "the worker copied live undo records"
        );
        assert!(
            !original.right.can_redo(),
            "the worker copied deleted text in redo records"
        );
        assert_eq!(original.left.text(), r#" {"value":1}"#);
        assert_eq!(original.right.text(), r#"{"value":1}"#);
        assert!(view.pane(Side::Left).buffer().can_undo());
        assert!(view.pane(Side::Right).buffer().can_redo());
    }

    #[test]
    fn restoring_a_prettified_comparison_keeps_the_panes_undo_history() {
        use super::{Command, Side, TextView};
        use ca_ui::view::SessionView;
        use std::time::{Duration, Instant};

        let directory = tempfile::tempdir().unwrap();
        let left = directory.path().join("left.json");
        let right = directory.path().join("right.json");
        std::fs::write(&left, r#"{"value":1}"#).unwrap();
        std::fs::write(&right, r#"{"value":1}"#).unwrap();
        let mut view = TextView::new(left, right, &ca_ui::testing::context(), 1);
        let deadline = Instant::now() + Duration::from_secs(20);
        while !view.is_settled() && Instant::now() < deadline {
            view.tick();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(view.is_settled());

        let left_pane = view.pane_mut(Side::Left);
        left_pane.buffer_mut().insert(0, " ");
        left_pane.mark_saved();
        assert!(left_pane.buffer().can_undo());
        let right_pane = view.pane_mut(Side::Right);
        right_pane.buffer_mut().insert(0, " ");
        assert!(right_pane.undo());
        right_pane.mark_saved();
        assert!(right_pane.buffer().can_redo());

        view.run(Command::PrettifyForComparison);
        while view.format_job.is_some() && Instant::now() < deadline {
            view.tick();
            std::thread::sleep(Duration::from_millis(2));
        }
        while !view.is_settled() && Instant::now() < deadline {
            view.tick();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(view.formatted_compare.is_some());

        view.run(Command::PrettifyForComparison);
        while !view.is_settled() && Instant::now() < deadline {
            view.tick();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(view.is_settled());
        assert!(view.pane(Side::Left).buffer().can_undo());
        assert!(view.pane_mut(Side::Left).undo());
        assert_eq!(view.pane(Side::Left).buffer().text(), r#"{"value":1}"#);
        assert!(view.pane(Side::Right).buffer().can_redo());
        assert!(view.pane_mut(Side::Right).redo());
        assert_eq!(view.pane(Side::Right).buffer().text(), r#" {"value":1}"#);
    }

    #[test]
    fn structural_projection_keeps_and_restores_the_source_line_ending_facts() {
        use super::{Command, TextView};
        use ca_ui::view::SessionView;
        use std::time::{Duration, Instant};

        let directory = tempfile::tempdir().unwrap();
        let left = directory.path().join("left.json");
        let right = directory.path().join("right.json");
        std::fs::write(&left, "{\r\n\"value\":1\n}").unwrap();
        std::fs::write(&right, r#"{"value":1}"#).unwrap();
        let mut view = TextView::new(left, right, &ca_ui::testing::context(), 1);
        let deadline = Instant::now() + Duration::from_secs(20);
        let settle = |view: &mut TextView| {
            while (view.format_job.is_some() || !view.is_settled()) && Instant::now() < deadline {
                view.tick();
                std::thread::sleep(Duration::from_millis(2));
            }
            assert!(view.is_settled());
        };
        settle(&mut view);
        assert!(view.data.left.mixed_eol);

        view.run(Command::CompareStructure);
        settle(&mut view);
        assert!(view.structural_summary().is_some());
        assert!(view.data.left.mixed_eol);

        view.run(Command::CompareStructure);
        settle(&mut view);
        assert!(view.structural_summary().is_none());
        assert!(view.data.left.mixed_eol);
    }

    #[test]
    fn a_right_pane_edit_that_adds_a_line_moves_the_bookmarks_below_it() {
        use super::{Side, TextView};
        use ca_ui::view::SessionView;
        use std::time::{Duration, Instant};

        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left.txt");
        let right = dir.path().join("right.txt");
        std::fs::write(&left, b"a\nb\nc\nd\ne\n").unwrap();
        std::fs::write(&right, b"a\nb\nc\nd\ne\n").unwrap();
        let mut view = TextView::new(left, right, &ca_ui::testing::context(), 1);
        let deadline = Instant::now() + Duration::from_secs(20);
        while !view.is_ready() && Instant::now() < deadline {
            view.tick();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(view.is_ready());
        view.bookmarks.toggle(0, 3);
        view.pane_mut(Side::Right).buffer_mut().insert(0, "new\n");
        view.note_edit();
        assert_eq!(view.bookmarks.row_of(0), Some(4));
    }

    #[test]
    fn save_as_during_a_save_keeps_the_original_target() {
        use super::{Side, TextView};
        use ca_ui::view::SessionView;
        use std::time::{Duration, Instant};

        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("original.txt");
        let another = dir.path().join("another.txt");
        std::fs::write(&original, b"original").unwrap();
        let mut view = TextView::new(
            original.clone(),
            original.clone(),
            &ca_ui::testing::context(),
            1,
        );
        let deadline = Instant::now() + Duration::from_secs(20);
        while !view.is_ready() && Instant::now() < deadline {
            view.tick();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(view.is_ready());
        view.pane_mut(Side::Left).buffer_mut().insert(0, "edit ");
        view.save_side(Side::Left, super::SaveConsent::default());
        view.save_active_as(&another);
        assert_eq!(view.left_path, original);
        assert!(!another.exists());
        while view.save_job.is_some() && Instant::now() < deadline {
            view.poll_save();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(std::fs::read(&original).unwrap(), b"edit original");
        assert!(!another.exists());
    }

    #[test]
    fn a_successful_save_as_replaces_the_caller_supplied_pane_title() {
        use super::{Side, TextView};
        use ca_ui::view::SessionView;
        use std::time::{Duration, Instant};

        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("original.txt");
        let renamed = dir.path().join("renamed.txt");
        std::fs::write(&original, b"before").unwrap();
        let mut view = TextView::new(
            original.clone(),
            original.clone(),
            &ca_ui::testing::context(),
            1,
        );
        let deadline = Instant::now() + Duration::from_secs(20);
        while !view.is_ready() && Instant::now() < deadline {
            view.tick();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(view.is_ready());
        view.left_title = Some("Server version".to_owned());
        view.pane_mut(Side::Left).buffer_mut().insert(0, "after ");
        view.save_active_as(&renamed);
        while view.save_job.is_some() && Instant::now() < deadline {
            view.poll_save();
            std::thread::sleep(Duration::from_millis(2));
        }

        assert_eq!(std::fs::read(&renamed).unwrap(), b"after before");
        assert_eq!(view.left_path, renamed);
        assert_eq!(view.left_title, None);
        assert_eq!(SessionView::title(&view), "renamed.txt - original.txt");
    }

    #[test]
    fn a_save_as_callback_is_refused_after_editing_is_disabled() {
        use super::TextView;
        use ca_ui::view::SessionView;
        use std::time::{Duration, Instant};

        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("original.txt");
        let another = dir.path().join("another.txt");
        std::fs::write(&original, b"original").unwrap();
        let mut view = TextView::new(original.clone(), original, &ca_ui::testing::context(), 1);
        let deadline = Instant::now() + Duration::from_secs(20);
        while !view.is_ready() && Instant::now() < deadline {
            view.tick();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(view.is_ready());

        let mut settings = view.session_settings.clone();
        settings.specs.disable_editing = true;
        view.apply_session_settings(settings);
        assert!(!view.accepts(super::Command::SaveFileAs));

        // This is the callback a picker opened before the setting changed can
        // deliver after the view has become read-only.
        view.save_active_as(&another);
        assert!(view.save_job.is_none());
        assert!(!another.exists());
        assert_eq!(
            view.message.as_deref(),
            Some("Editing is turned off for this session")
        );
    }

    #[test]
    fn failed_save_as_keeps_the_original_name_and_unsaved_edit() {
        use super::{Side, TextView};
        use ca_ui::view::SessionView;
        use std::time::{Duration, Instant};

        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("original.txt");
        let invalid = dir.path().join("folder");
        std::fs::write(&original, b"original").unwrap();
        std::fs::create_dir(&invalid).unwrap();
        let mut view = TextView::new(
            original.clone(),
            original.clone(),
            &ca_ui::testing::context(),
            1,
        );
        let deadline = Instant::now() + Duration::from_secs(20);
        while !view.is_ready() && Instant::now() < deadline {
            view.tick();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(view.is_ready());
        view.pane_mut(Side::Left).buffer_mut().insert(0, "edit ");
        view.save_active_as(&invalid);
        while view.save_job.is_some() && Instant::now() < deadline {
            view.poll_save();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(view.left_path, original);
        assert!(view.pane(Side::Left).is_modified());
        assert_eq!(std::fs::read(&original).unwrap(), b"original");
    }

    #[test]
    fn save_as_loss_prompt_retains_the_new_target_for_retry() {
        use super::{Prompt, SaveConsent, Side, TextView};
        use ca_session::settings::common::EncodingChoice;
        use ca_ui::view::SessionView;
        use std::time::{Duration, Instant};

        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("original.txt");
        let another = dir.path().join("another.txt");
        std::fs::write(&original, [0xF0, 0x28, b'\n']).unwrap();
        // Pin UTF-8 so this remains a lossy input even when automatic
        // detection recognizes Windows-1252 for the same byte sequence.
        let mut view = TextView::blank(
            original.clone(),
            original.clone(),
            &ca_ui::testing::context(),
            1,
        );
        view.session_settings.format.left_encoding = EncodingChoice::Named {
            name: "UTF-8".to_owned(),
            unknown: std::collections::BTreeMap::new(),
        };
        view.restart();
        let deadline = Instant::now() + Duration::from_secs(20);
        while !view.is_ready() && Instant::now() < deadline {
            view.tick();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(view.is_ready());
        view.pane_mut(Side::Left).buffer_mut().insert(0, "edit ");
        view.save_active_as(&another);
        while view.save_job.is_some() && Instant::now() < deadline {
            view.poll_save();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(matches!(view.prompt, Some(Prompt::WouldLose(..))));
        assert_eq!(view.left_path, original);
        assert_eq!(view.saving_as.as_deref(), Some(another.as_path()));
        view.answer_prompt(true);
        // The answer agrees to the loss and to nothing else, so a retry to a
        // checked name still stops at a change on disk.
        assert_eq!(
            view.saving_consent,
            SaveConsent {
                accept_loss: true,
                accept_disk_change: false,
            }
        );
        while view.save_job.is_some() && Instant::now() < deadline {
            view.poll_save();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(view.left_path, another);
        assert!(another.exists());
    }

    #[test]
    fn a_retained_concurrent_version_stops_save_both_from_closing() {
        use super::{Prompt, Side, TextView};
        use ca_ui::save::{FileSystem, RealFileSystem, SaveOutcome};
        use ca_ui::view::SessionView;
        use std::time::{Duration, Instant};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("text.txt");
        let backup = dir.path().join("other-version.txt");
        std::fs::write(&path, b"original").unwrap();
        let mut view = TextView::new(path.clone(), path.clone(), &ca_ui::testing::context(), 1);
        let deadline = Instant::now() + Duration::from_secs(20);
        while !view.is_ready() && Instant::now() < deadline {
            view.tick();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(view.is_ready());
        view.saving = Some(Side::Left);
        view.save_both_pending = true;
        view.close_requested = true;
        view.apply_save_outcome(SaveOutcome::SavedWithConflict {
            stamp: RealFileSystem.stamp(&path).unwrap(),
            backup: backup.clone(),
        });
        assert!(!view.save_both_pending);
        assert!(!view.close_requested);
        assert_eq!(
            view.prompt,
            Some(Prompt::KeptCopy(backup.display().to_string()))
        );
        assert!(!view.may_close());
        assert!(view
            .message
            .as_deref()
            .unwrap()
            .contains(&backup.display().to_string()));
    }

    #[test]
    fn edits_during_save_remain_unsaved() {
        use super::{Side, TextView};
        use ca_ui::view::SessionView;
        use std::time::{Duration, Instant};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("text.txt");
        std::fs::write(&path, b"original").unwrap();
        let mut view = TextView::new(path.clone(), path.clone(), &ca_ui::testing::context(), 1);
        let deadline = Instant::now() + Duration::from_secs(20);
        while !view.is_ready() && Instant::now() < deadline {
            view.tick();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(view.is_ready());
        view.pane_mut(Side::Left).buffer_mut().insert(0, "saved ");
        view.save_side(Side::Left, super::SaveConsent::default());
        view.pane_mut(Side::Left).buffer_mut().insert(0, "later ");
        while view.save_job.is_some() && Instant::now() < deadline {
            view.poll_save();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(view.save_job.is_none());
        assert_eq!(std::fs::read(&path).unwrap(), b"saved original");
        assert!(view.pane(Side::Left).is_modified());
        view.pane_mut(Side::Left).buffer_mut().undo().unwrap();
        view.pane_mut(Side::Left).buffer_mut().undo().unwrap();
        assert!(
            view.pane(Side::Left).is_modified(),
            "the original revision is no longer on disk"
        );
    }

    #[test]
    fn save_both_finishes_both_files_before_closing() {
        for closing in [false, true] {
            use super::{Side, TextView};
            use ca_ui::view::SessionView;
            use std::time::{Duration, Instant};
            let dir = tempfile::tempdir().unwrap();
            let left = dir.path().join("left.txt");
            let right = dir.path().join("right.txt");
            std::fs::write(&left, b"left").unwrap();
            std::fs::write(&right, b"right").unwrap();
            let mut view =
                TextView::new(left.clone(), right.clone(), &ca_ui::testing::context(), 1);
            let deadline = Instant::now() + Duration::from_secs(20);
            while !view.is_ready() && Instant::now() < deadline {
                view.tick();
                std::thread::sleep(Duration::from_millis(2));
            }
            assert!(view.is_ready());
            view.pane_mut(Side::Left).buffer_mut().insert(0, "new ");
            view.pane_mut(Side::Right).buffer_mut().insert(0, "new ");
            view.close_requested = closing;
            view.save_both();
            assert!(!view.may_close());
            assert!(!view.wants_close());
            while view.save_job.is_some() && Instant::now() < deadline {
                view.poll_save();
                std::thread::sleep(Duration::from_millis(2));
            }
            assert_eq!(std::fs::read(&left).unwrap(), b"new left");
            assert_eq!(std::fs::read(&right).unwrap(), b"new right");
            assert_eq!(view.wants_close(), closing);
            assert!(view.may_close());
        }
    }

    #[test]
    fn every_filter_has_a_label() {
        for filter in [
            DisplayFilter::All,
            DisplayFilter::Differences,
            DisplayFilter::Same,
            DisplayFilter::Context(3),
            DisplayFilter::None,
        ] {
            assert!(!filter_label(filter).is_empty());
        }
    }

    #[test]
    fn lossy_decoding_is_reported_in_the_status_bar() {
        use super::TextView;

        let mut view = TextView::from_data(
            PathBuf::from("left.txt"),
            PathBuf::from("right.txt"),
            &ca_ui::testing::context(),
            1,
            crate::jobs::TextData::default(),
        );
        view.data.left.had_errors = true;

        assert!(view
            .status_fields()
            .iter()
            .any(|field| field.contains("text comparison may be incomplete")));
    }

    #[test]
    fn a_title_uses_the_file_names() {
        assert_eq!(file_name(&PathBuf::from("/tmp/one.txt")), "one.txt");
    }

    #[test]
    fn an_ignored_unimportant_row_paints_as_matching_text() {
        let row = crate::model::Row {
            class: crate::model::RowClass::Changed,
            importance: Some(ca_diff::Importance::Unimportant),
            left: Some(0),
            right: Some(0),
            section: None,
        };
        assert_eq!(row_class(&row, false), TextClass::Unimportant);
        assert_eq!(row_class(&row, true), TextClass::Same);
    }

    #[test]
    fn only_the_differing_columns_take_the_difference_color() {
        let table = palette(Variant::Dark);
        let base = table.same_text;
        let difference = table.important_text;
        let mut runs: Vec<Run> = Vec::new();
        build_runs(
            &mut runs,
            0..10,
            base,
            &[],
            &table,
            &[ColumnSpan { start: 3, end: 5 }],
            difference,
        );
        assert_eq!(
            runs,
            vec![
                Run {
                    start: 0,
                    end: 3,
                    color: base
                },
                Run {
                    start: 3,
                    end: 5,
                    color: difference
                },
                Run {
                    start: 5,
                    end: 10,
                    color: base
                },
            ]
        );
    }

    #[test]
    fn a_difference_outranks_the_syntax_color_under_it() {
        let table = palette(Variant::Dark);
        let keyword = crate::syntax::Span {
            start: 0,
            end: 6,
            slot: ca_grammar::StyleSlot::Keyword,
        };
        let mut runs: Vec<Run> = Vec::new();
        build_runs(
            &mut runs,
            0..6,
            table.same_text,
            &[keyword],
            &table,
            &[ColumnSpan { start: 2, end: 4 }],
            table.important_text,
        );
        let color_at = |column: u32| {
            runs.iter()
                .find(|run| run.start <= column && column < run.end)
                .map(|run| run.color)
        };
        assert_eq!(
            color_at(0),
            Some(table.syntax_slot(ca_grammar::StyleSlot::Keyword))
        );
        assert_eq!(color_at(3), Some(table.important_text));
        assert_eq!(
            color_at(5),
            Some(table.syntax_slot(ca_grammar::StyleSlot::Keyword))
        );
    }

    #[test]
    fn runs_are_built_only_for_the_columns_in_view() {
        let table = palette(Variant::Dark);
        let mut runs: Vec<Run> = Vec::new();
        build_runs(
            &mut runs,
            40..60,
            table.same_text,
            &[],
            &table,
            &[],
            table.important_text,
        );
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].start, 40);
        assert_eq!(runs[0].end, 60);
    }

    #[test]
    fn a_click_at_a_fractional_scroll_lands_on_the_row_painted_under_it() {
        use super::{Side, TextView};
        use ca_ui::view::SessionView;
        use std::time::{Duration, Instant};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("long.txt");
        let mut text = (0..300)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        text.push('\n');
        std::fs::write(&path, text.as_bytes()).unwrap();
        let context = ca_ui::testing::context();
        let mut view = TextView::new(path.clone(), path, &context, 1);
        let ctx = egui::Context::default();
        let frame = |view: &mut TextView, events: Vec<egui::Event>| {
            view.tick();
            let input = ca_ui::testing::event_input(1_000.0, 600.0, events);
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    view.ui(ui, &context);
                });
            });
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        while !view.is_ready() && Instant::now() < deadline {
            frame(&mut view, Vec::new());
            std::thread::sleep(Duration::from_millis(2));
        }
        frame(&mut view, Vec::new());
        let row_height = view.row_pixels();
        let body = view.body_rect();
        let total = view.visible.len();
        view.scroll
            .scroll_by(row_height * 2.5, body.height(), row_height, total);
        assert!(view.scroll.fraction() > 0.0);
        let at = egui::pos2(body.left() + 60.0, body.top() + row_height * 0.75);
        frame(
            &mut view,
            vec![
                egui::Event::PointerMoved(at),
                egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::default(),
                },
            ],
        );
        assert_eq!(view.pane(Side::Left).caret().line, 3);
    }

    #[test]
    fn a_click_before_the_rediff_lands_hits_the_edited_text_painted_under_it() {
        use super::{Side, TextView};
        use ca_ui::view::SessionView;
        use std::time::{Duration, Instant};

        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left.txt");
        let right = dir.path().join("right.txt");
        std::fs::write(&left, "\u{1f600}".repeat(16).as_bytes()).unwrap();
        std::fs::write(&right, b"x\n").unwrap();
        let context = ca_ui::testing::context();
        let mut view = TextView::new(left, right, &context, 1);
        let ctx = egui::Context::default();
        let frame = |view: &mut TextView, events: Vec<egui::Event>| {
            let input = ca_ui::testing::event_input(1_000.0, 600.0, events);
            ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    view.ui(ui, &context);
                });
            })
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        while !view.is_ready() && Instant::now() < deadline {
            view.tick();
            let _ = frame(&mut view, Vec::new());
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(view.is_ready());
        let typed = "abcdefghijklmnopqrstuvwxyz";
        view.left_pane.select_all();
        view.left_pane.insert(typed);
        view.note_edit();
        // No tick follows the edit, so the comparison behind the panes stays
        // the one loaded from disk while the edited text is painted.
        let output = frame(&mut view, Vec::new());
        let body = view.body_rect();
        let painted = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text)
                    if text.galley.job.text.starts_with("abc") && body.contains(text.pos) =>
                {
                    Some(text.clone())
                }
                _ => None,
            })
            .unwrap();
        let glyphs = &painted.galley.rows[0].glyphs;
        let at = egui::pos2(
            painted.pos.x + glyphs[10].pos.x + 1.0,
            painted.pos.y + glyphs[10].pos.y,
        );
        let _ = frame(
            &mut view,
            vec![
                egui::Event::PointerMoved(at),
                egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::default(),
                },
            ],
        );
        assert_eq!(view.pane(Side::Left).line_text(0), typed);
        assert_eq!(view.pane(Side::Left).caret().line, 0);
        assert_eq!(view.pane(Side::Left).caret().index, 10);
    }

    #[test]
    fn plain_text_caret_and_clicks_stay_on_the_painted_glyphs_at_every_scale() {
        use super::{Side, TextView};
        use ca_ui::view::SessionView;
        use std::time::{Duration, Instant};

        let line = "0123456789".repeat(9);
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left.txt");
        let right = dir.path().join("right.txt");
        std::fs::write(&left, format!("{line}\n").as_bytes()).unwrap();
        std::fs::write(&right, b"x\n").unwrap();
        let context = ca_ui::testing::context();
        for scale in [1.0_f32, 1.25, 1.5, 2.0] {
            let mut view = TextView::new(left.clone(), right.clone(), &context, 1);
            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(scale);
            let frame = |view: &mut TextView, events: Vec<egui::Event>| {
                let input = ca_ui::testing::event_input(1_400.0, 600.0, events);
                ctx.run(input, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        view.ui(ui, &context);
                    });
                })
            };
            let deadline = Instant::now() + Duration::from_secs(20);
            while !view.is_ready() && Instant::now() < deadline {
                view.tick();
                let _ = frame(&mut view, Vec::new());
                std::thread::sleep(Duration::from_millis(2));
            }
            assert!(view.is_ready());
            let output = frame(&mut view, Vec::new());
            let body = view.body_rect();
            let painted = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text)
                        if text.galley.job.text.starts_with("0123") && body.contains(text.pos) =>
                    {
                        Some(text.clone())
                    }
                    _ => None,
                })
                .unwrap();
            let glyphs = &painted.galley.rows[0].glyphs;
            let target = 70;
            assert!(glyphs.len() > target, "{scale}: {} glyphs", glyphs.len());
            let glyph_x = painted.pos.x + glyphs[target].pos.x;
            let at = egui::pos2(
                glyph_x + glyphs[target].advance_width * 0.25,
                painted.pos.y + 2.0,
            );
            let _ = frame(
                &mut view,
                vec![
                    egui::Event::PointerMoved(at),
                    egui::Event::PointerButton {
                        pos: at,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::default(),
                    },
                ],
            );
            let _ = frame(
                &mut view,
                vec![egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::default(),
                }],
            );
            assert_eq!(view.pane(Side::Left).caret().line, 0, "{scale}");
            assert_eq!(view.pane(Side::Left).caret().index, 70, "{scale}");
            let output = frame(&mut view, Vec::new());
            let caret = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::LineSegment { points, .. }
                        if body.contains(points[0])
                            && (points[1].y - points[0].y - view.row_pixels()).abs() < 0.01 =>
                    {
                        Some(points[0].x)
                    }
                    _ => None,
                })
                .unwrap();
            assert!(
                (caret - glyph_x).abs() < 0.5,
                "{scale}: caret {caret}, glyph {glyph_x}"
            );
        }
    }

    fn clipboard_view() -> (super::TextView, tempfile::TempDir) {
        use super::TextView;
        use ca_ui::view::SessionView;
        use std::time::{Duration, Instant};

        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left.txt");
        let right = dir.path().join("right.txt");
        std::fs::write(&left, b"alpha\n").unwrap();
        std::fs::write(&right, b"beta\n").unwrap();
        let mut view = TextView::new(left, right, &ca_ui::testing::context(), 1);
        let deadline = Instant::now() + Duration::from_secs(20);
        while !view.is_ready() && Instant::now() < deadline {
            view.tick();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(view.is_ready());
        (view, dir)
    }

    fn clipboard_frame(
        ctx: &egui::Context,
        view: &mut super::TextView,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        use ca_ui::view::SessionView;
        let context = ca_ui::testing::context();
        let input = ca_ui::testing::event_input(1_000.0, 600.0, events);
        ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                view.ui(ui, &context);
            });
        })
    }

    fn copied(output: &egui::FullOutput) -> Vec<String> {
        output
            .platform_output
            .commands
            .iter()
            .filter_map(|command| match command {
                egui::OutputCommand::CopyText(text) => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn edit_menu_copy_and_cut_put_the_selection_on_the_clipboard() {
        use super::Side;
        use ca_ui::command::Command;
        use ca_ui::view::SessionView;

        let (mut view, _dir) = clipboard_view();
        let ctx = egui::Context::default();
        view.run(Command::SelectAll);
        view.run(Command::Copy);
        let output = clipboard_frame(&ctx, &mut view, Vec::new());
        assert_eq!(copied(&output), vec!["alpha\n".to_owned()]);
        assert!(!view.pane(Side::Left).is_modified());
        view.run(Command::SelectAll);
        view.run(Command::Cut);
        let output = clipboard_frame(&ctx, &mut view, Vec::new());
        assert_eq!(copied(&output), vec!["alpha\n".to_owned()]);
        assert!(view.pane(Side::Left).buffer().text().is_empty());
        assert!(view.pane(Side::Left).is_modified());
    }

    #[test]
    fn edit_menu_paste_asks_for_the_clipboard_and_inserts_what_arrives() {
        use super::Side;
        use ca_ui::command::Command;
        use ca_ui::view::SessionView;

        let (mut view, _dir) = clipboard_view();
        let ctx = egui::Context::default();
        view.run(Command::Paste);
        let output = clipboard_frame(&ctx, &mut view, Vec::new());
        let asked = output.viewport_output.values().any(|viewport| {
            viewport
                .commands
                .contains(&egui::ViewportCommand::RequestPaste)
        });
        assert!(asked);
        clipboard_frame(&ctx, &mut view, vec![egui::Event::Paste("P".to_owned())]);
        assert!(view.pane(Side::Left).buffer().text().starts_with('P'));
    }

    /// A view over two files holding `text`, with its first load installed.
    fn settled_view(text: &str) -> (super::TextView, tempfile::TempDir) {
        use ca_ui::view::SessionView;
        use std::time::{Duration, Instant};

        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left.txt");
        let right = dir.path().join("right.txt");
        std::fs::write(&left, text).unwrap();
        std::fs::write(&right, text).unwrap();
        let mut view = super::TextView::new(left, right, &ca_ui::testing::context(), 1);
        let deadline = Instant::now() + Duration::from_secs(20);
        while !view.is_settled() && Instant::now() < deadline {
            view.tick();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(view.is_settled());
        (view, dir)
    }

    #[test]
    fn undo_and_redo_reveal_the_replayed_text() {
        use ca_ui::view::SessionView;
        let (mut view, _dir) = settled_view(&"line\n".repeat(200));
        view.viewport_height = 100.0;
        view.left_pane
            .place(ca_ui::editor::Caret::new(150, 1), false);
        view.left_pane.type_character('X');
        view.note_edit();
        view.left_pane.place(ca_ui::editor::Caret::new(0, 0), false);
        view.scroll_to_position(0);
        view.run(ca_ui::Command::Undo);
        assert!(view.scroll.first_row() > 100);
        view.scroll_to_position(0);
        view.run(ca_ui::Command::Redo);
        assert!(view.scroll.first_row() > 100);
    }

    #[test]
    fn a_reload_drops_the_comparison_an_edit_scheduled() {
        use super::Side;
        use ca_ui::command::Command;
        use ca_ui::save::CloseChoice;
        use ca_ui::view::SessionView;

        let (mut view, _dir) = settled_view("alpha\n");
        view.pane_mut(Side::Left).buffer_mut().insert(0, "x");
        view.note_edit();
        assert!(view.rediff.is_stale());
        view.run(Command::Reload);
        view.answer_unsaved(CloseChoice::Discard);
        assert!(
            !view.rediff.is_stale(),
            "the edit's comparison is still due"
        );
        assert!(!view.rediff.is_running());
    }

    #[test]
    fn a_terminal_comparison_is_settled_before_its_sender_disconnects() {
        use super::jobs::TextMessage;
        use ca_ui::worker::Job;
        use std::sync::mpsc;
        use std::time::Duration;

        for rediff in [false, true] {
            let (mut view, _dir) = settled_view("alpha\n");
            let data = Box::new(view.data.clone());
            let (sent, received) = mpsc::channel();
            let (release, held) = mpsc::channel();
            let job = Job::spawn(move |emitter, _| {
                emitter.send(TextMessage::Ready(data));
                sent.send(()).unwrap();
                // Hold the sender after Ready, making the scheduling race deterministic.
                let _ = held.recv_timeout(Duration::from_secs(20));
            });
            received.recv_timeout(Duration::from_secs(20)).unwrap();
            if rediff {
                view.rediff.start(job);
                view.poll_rediff();
            } else {
                view.job = Some(job);
                view.poll_comparison();
            }
            let settled = view.is_settled();
            release.send(()).unwrap();
            assert!(settled, "Ready must settle the comparison; rediff={rediff}");
        }
    }

    #[test]
    fn a_terminal_projection_finishes_before_its_sender_disconnects() {
        use super::{Command, FormatMessage, TextView};
        use ca_ui::view::SessionView;
        use ca_ui::worker::Job;
        use std::sync::mpsc;
        use std::time::Duration;

        let directory = tempfile::tempdir().unwrap();
        let left = directory.path().join("left.json");
        let right = directory.path().join("right.json");
        std::fs::write(&left, r#"{"value":1}"#).unwrap();
        std::fs::write(&right, r#"{"value":2}"#).unwrap();
        let mut view = TextView::new(left, right, &ca_ui::testing::context(), 1);
        assert!(ca_ui::testing::wait_until(Duration::from_secs(20), || {
            view.tick();
            view.is_settled()
        }));
        view.run(Command::PrettifyForComparison);
        let mut prepared = view.format_job.take().unwrap();
        let ready = prepared
            .wait(Duration::from_secs(20))
            .into_iter()
            .find(|message| matches!(message, FormatMessage::Ready { .. }))
            .unwrap();
        let (sent, received) = mpsc::channel();
        let (release, held) = mpsc::channel::<()>();
        view.format_job = Some(Job::spawn(move |emitter, _| {
            emitter.send(ready);
            sent.send(()).unwrap();
            // Hold the sender after Ready, making the scheduling race deterministic.
            let _ = held.recv_timeout(Duration::from_secs(20));
        }));
        received.recv_timeout(Duration::from_secs(20)).unwrap();
        view.poll_format();
        let finished = view.format_job.is_none();
        view.run(Command::Cancel);
        let message = view.message().map(str::to_owned);
        release.send(()).unwrap();
        assert!(view.formatted_compare.is_some());
        assert_ne!(
            message.as_deref(),
            Some("Formatting stopped; the source text is unchanged.")
        );
        assert!(finished, "Ready must finish the projection run");
    }

    /// Escape while the find strip is open closes the strip; the comparison an
    /// edit scheduled must still run.
    #[test]
    fn cancel_with_the_find_strip_open_keeps_the_scheduled_comparison() {
        use super::Side;
        use ca_ui::command::Command;
        use ca_ui::view::SessionView;

        let (mut view, _dir) = settled_view("alpha\n");
        view.run(Command::Find);
        view.pane_mut(Side::Left).buffer_mut().insert(0, "x");
        view.note_edit();
        assert!(view.rediff.is_stale());
        if view.accepts(Command::Cancel) {
            view.run(Command::Cancel);
        }
        assert!(view.rediff.is_stale(), "the edit's comparison was dropped");
    }

    /// A comparison that started before a load landed compares text the load
    /// replaced, so its result is refused.
    #[test]
    fn a_rediff_started_before_a_load_landed_is_ignored() {
        use super::jobs::{self, SidePayload, TextMessage};
        use std::sync::Arc;
        use std::time::{Duration, Instant};

        let (mut view, dir) = settled_view("alpha\n");
        let ready = |mut job: ca_ui::worker::Job<TextMessage>| {
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                for message in job.drain() {
                    if let TextMessage::Ready(data) = message {
                        return data;
                    }
                }
                assert!(Instant::now() < deadline, "the comparison never finished");
                std::thread::sleep(Duration::from_millis(2));
            }
        };
        let loaded = ready(jobs::spawn(
            dir.path().join("left.txt"),
            dir.path().join("right.txt"),
            view.compare_settings(),
            Arc::new(|| {}),
        ));
        let stale = jobs::spawn_texts(
            "alpha\n".to_owned(),
            "omega\n".to_owned(),
            SidePayload::default(),
            SidePayload::default(),
            view.compare_settings(),
            Arc::new(|| {}),
        );
        view.rediff.start(stale);
        view.install(*loaded, false);

        let deadline = Instant::now() + Duration::from_secs(20);
        while view.rediff.is_running() && Instant::now() < deadline {
            view.poll_rediff();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(!view.rediff.is_running());
        assert_eq!(
            view.model().counts().differences,
            0,
            "the result of a comparison over replaced text was installed"
        );
    }
}
