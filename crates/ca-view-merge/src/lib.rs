//! Three way text merge view.
//!
//! Three non-editable input panes sit above one editable output pane. The
//! panes share one scroll position, so a row of the output always lines up
//! with the rows it was merged from.
//!
//! The merge itself is [`model`], which knows nothing about painting. Reading
//! the inputs and merging them is [`jobs`], which runs on a worker. Writing the
//! result is [`output`].

pub mod automerge;
pub mod filter;
pub mod jobs;
pub mod model;
pub mod output;
pub mod settings;

use ca_session::SessionKind;
use ca_text::{LineRange, TextBuffer};
use ca_ui::command::{Command, MenuView};
use ca_ui::editor::{Caret, Motion};
use ca_ui::filter::Visible;
use ca_ui::find::{self, FindOperation, FindPanel, FindSettings, FindTask, PanelRequest};
use ca_ui::report::{ReportKind, ViewReport};
use ca_ui::save::text::SaveConsent;
use ca_ui::save::{Baseline, SaveMessage, SaveOutcome};
use ca_ui::scroll::RowScroll;
use ca_ui::theme::merge::{palette as merge_palette, MergeClass, Palette};
use ca_ui::theme::Variant;
use ca_ui::thumbnail::Strip;
use ca_ui::toolbar;
use ca_ui::view::{OpenRequest, SessionView, Titles, ViewAction, ViewContext};
use ca_ui::widgets;
use ca_ui::worker::Job;
use filter::MergeFilter;
use jobs::{MergeData, MergeMessage, MergePaths};
use model::{DisplayRules, MergeModel, Pane, Resolution};
use output::{MarkerLabels, Outcome};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Width of the overview strip.
const THUMBNAIL_WIDTH: f32 = 26.0;
/// Display font size a new view starts at.
const DEFAULT_FONT_SIZE: f32 = 10.5;
/// Row height as a multiple of the display font size.
const ROW_HEIGHT_RATIO: f32 = 1.45;
/// Columns kept for line numbers, in characters.
const LINE_NUMBER_COLUMNS: f32 = 5.0;
/// Width of the take button column at the left edge of the output gutter.
const ARROW_WIDTH: f32 = 32.0;
/// Fraction of the comparison area the three input panes take.
const INPUT_SHARE: f32 = 0.55;
/// Points between the diagonal lines of a gap row's hatch.
const HATCH_STEP: f32 = 7.0;
/// Width of the rule between two panes.
const SEPARATOR: f32 = 2.0;
/// Reason shown on a control that needs a finished merge.
const NOT_MERGED: &str = "Available once the merge finishes";
/// Reason shown on a control that needs an output file.
const NO_OUTPUT: &str = "This session names no output file";
/// What the view says when an edit of the output is asked for while the
/// editing switch of the session is on.
const EDITING_OFF: &str = "Editing is turned off for this session";

/// Commands that change the output or write it.
const CHANGES_OUTPUT: &[Command] = &[
    Command::TakeLeft,
    Command::TakeCenter,
    Command::TakeRight,
    Command::TakeLeftThenRight,
    Command::TakeRightThenLeft,
    Command::TakeAllNonConflicting,
    Command::TakeLeftLine,
    Command::TakeCenterLine,
    Command::TakeRightLine,
    Command::ClearConflictNext,
    Command::ToggleConflict,
    Command::ToggleSectionIgnored,
    Command::Cut,
    Command::Paste,
    Command::Replace,
    Command::Undo,
    Command::Redo,
    Command::SaveFile,
];
/// Width of the display filter control on the toolbar.
const FILTER_COMBO_WIDTH: f32 = 150.0;
/// The filters the toolbar control lists. Show None stays off the control and
/// off the menus, and is reached through a shortcut the commands page binds.
const LISTED_FILTERS: [MergeFilter; 8] = [
    MergeFilter::All,
    MergeFilter::Changes,
    MergeFilter::Conflicts,
    MergeFilter::LeftChanges,
    MergeFilter::RightChanges,
    MergeFilter::Mergeable,
    MergeFilter::Unchanged,
    MergeFilter::Context(0),
];

/// Every command this view answers for, in the order the menus show them.
const HANDLED: &[Command] = &[
    Command::CompareReport,
    Command::TakeLeft,
    Command::TakeCenter,
    Command::TakeRight,
    Command::TakeLeftThenRight,
    Command::TakeRightThenLeft,
    Command::TakeLeftLine,
    Command::TakeCenterLine,
    Command::TakeRightLine,
    Command::TakeAllNonConflicting,
    Command::FavorLeft,
    Command::FavorRight,
    Command::NextConflict,
    Command::PreviousConflict,
    Command::ClearConflictNext,
    Command::NextLeftTaken,
    Command::PreviousLeftTaken,
    Command::NextRightTaken,
    Command::PreviousRightTaken,
    Command::ToggleCenterPane,
    Command::MergeInfo,
    Command::CompareToOutput,
    Command::NextSection,
    Command::PreviousSection,
    Command::NextDifference,
    Command::PreviousDifference,
    Command::Undo,
    Command::Redo,
    Command::Cut,
    Command::Copy,
    Command::Paste,
    Command::SelectAll,
    Command::SelectSection,
    Command::ToggleConflict,
    Command::Find,
    Command::Replace,
    Command::FindNext,
    Command::FindPrevious,
    Command::GoTo,
    Command::NextEdit,
    Command::PreviousEdit,
    Command::ShowAll,
    Command::ShowDifferences,
    Command::ShowConflicts,
    Command::ShowLeftChanges,
    Command::ShowRightChanges,
    Command::ShowMergeable,
    Command::ShowSame,
    Command::ShowNone,
    Command::ShowContext,
    Command::ToggleIgnoreUnimportant,
    Command::ToggleIgnoreSameChanges,
    Command::ToggleSectionIgnored,
    Command::SaveFile,
    Command::SwapSides,
    Command::Reload,
    Command::ToggleLineNumbers,
    Command::IncreaseFontSize,
    Command::DecreaseFontSize,
    Command::ResetFontSize,
    Command::Cancel,
];

/// Where the merge currently stands.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Status {
    /// Work is in progress, with the step it has reached.
    Running(&'static str),
    /// The merge is on screen.
    Ready,
    /// The merge could not be produced.
    Failed(String),
    /// The run stopped before it produced a merge.
    Cancelled,
}

impl Status {
    const fn is_running(&self) -> bool {
        matches!(self, Status::Running(_))
    }
}

/// A question the view is waiting on an answer to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Question {
    /// A concurrent output version was retained beside the saved file.
    KeptCopy(String),
    /// The output still holds conflicts and the user has not agreed to markers.
    SaveWithConflicts,
    /// The output changed on disk since the session read it.
    OverwriteChanged,
    /// The output cannot be written in its encoding without losing content.
    AcceptLoss(String),
    /// The tab is closing with the output unwritten.
    CloseModified,
}

impl Question {
    /// The sentence the question panel shows.
    #[must_use]
    pub fn prompt(&self) -> String {
        match self {
            Self::KeptCopy(path) => {
                format!("Another version was kept at {path}. Review it before deleting it.")
            }
            Self::SaveWithConflicts => {
                "The output still holds conflicts. Write it with conflict markers?".to_owned()
            }
            Self::OverwriteChanged => "The output file changed on disk. Overwrite it?".to_owned(),
            Self::AcceptLoss(reason) => format!("{reason} Write it anyway?"),
            Self::CloseModified => {
                "The output holds changes that are not written. Close anyway?".to_owned()
            }
        }
    }
}

/// A rectangle one frame drew, named, for a layout check.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WidgetRect {
    /// What the rectangle holds.
    pub name: &'static str,
    /// Where it was drawn.
    pub rect: egui::Rect,
}

/// The three way merge tab.
#[allow(clippy::struct_excessive_bools)]
pub struct MergeView {
    id: egui::Id,
    paths: MergePaths,
    /// The session's settings, which the merge options are derived from.
    session_settings: ca_session::settings::TextMergeSettings,
    titles: Titles,
    data: MergeData,
    status: Status,
    job: Option<Job<MergeMessage>>,
    save_job: Option<Job<SaveMessage>>,
    save_rules: ca_ui::save::SaveRules,
    saving_output: Option<String>,
    saving_revision: Option<u64>,
    saving_conflicts: Option<u32>,
    output_pane: ca_ui::editor::Pane,
    scroll: RowScroll,
    horizontal: f32,
    current: usize,
    /// True after the user has visited a section; otherwise first navigation
    /// includes a matching section at the top of the file.
    navigation_started: bool,
    /// The model row the caret or the last click is on.
    row: usize,
    filter: MergeFilter,
    /// Unchanged lines kept around each change under Show Context, read from
    /// the options each frame.
    context_lines: u32,
    /// The rows the filter shows, rebuilt whenever the model or the filter
    /// changes.
    visible: Visible,
    rules: DisplayRules,
    find: FindPanel,
    find_task: FindTask,
    /// Background of selected output text, read from the theme each frame.
    selection_color: egui::Color32,
    focus: Pane,
    show_center: bool,
    show_thumbnail: bool,
    show_line_numbers: bool,
    font: ca_ui::font::FontSize,
    /// Padding the options add between rows, read once per frame.
    line_spacing: u32,
    strip: Strip<MergeClass>,
    strip_stale: bool,
    question: Option<Question>,
    consent: SaveConsent,
    accept_markers: bool,
    output_baseline: Baseline,
    saved_output: Option<String>,
    saved_conflicts: Option<u32>,
    /// Whether the adopted result already differs from the last written or
    /// freshly generated output. This is computed once when a merge completes.
    output_changed_on_load: bool,
    message: Option<String>,
    failed: bool,
    notify: Arc<dyn Fn() + Send + Sync>,
    widgets: Vec<WidgetRect>,
    window: egui::Rect,
    panes: Vec<(Pane, egui::Rect)>,
    output_glyphs: Vec<(u32, egui::Pos2, Arc<egui::Galley>)>,
    toolbar_rect: egui::Rect,
    status_rect: egui::Rect,
    /// The report command of this view.
    report: ViewReport,
    toolbar_rows: usize,
    closing: bool,
    pending: Vec<ViewAction>,
    /// Text a menu command put on the clipboard. A command runs without a
    /// frame, and only a frame reaches the clipboard.
    pending_clipboard: Option<String>,
    /// A menu Paste waits for the clipboard text, which arrives as a paste
    /// event in a later frame.
    paste_requested: bool,
    /// The running merge reads the inputs the other way round from the model
    /// it replaces, so its decisions carry over mirrored.
    swapped: bool,
}

/// A row count as a coordinate. A pane never holds enough rows for the loss to
/// reach one row.
#[allow(clippy::cast_precision_loss)]
fn as_f32(value: usize) -> f32 {
    value as f32
}

/// A coordinate as a row count, with anything above the pane clamped away.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn as_usize(value: f32) -> usize {
    if value.is_finite() {
        value.max(0.0) as usize
    } else {
        0
    }
}

/// One pane's lines, without their terminators.
fn pane_lines(buffer: &TextBuffer) -> Vec<String> {
    (0..buffer.len_lines())
        .filter_map(|line| buffer.line_text(line))
        .collect()
}

fn output_lines_equal_text(lines: &crate::model::ChunkedVec<String>, text: &str) -> bool {
    let mut remaining = text;
    for line in lines.iter() {
        let Some(rest) = remaining.strip_prefix(line) else {
            return false;
        };
        remaining = rest;
    }
    remaining.is_empty()
}

/// The pane lines a batch of edits replaced, as `(start, old_end, new_end)`:
/// lines `start..old_end` before the batch are lines `start..new_end` after it.
///
/// Each span counts lines of the text as the edits before it left it, and a
/// span can start above an earlier one, for example when a pasted LF joins
/// the CR that ends the line above a replaced selection. Each earlier window
/// is therefore carried through every later span.
fn edited_window(spans: &[ca_ui::editor::EditSpan]) -> Option<(u32, u32, u32)> {
    let mut window: Option<(i64, i64)> = None;
    let mut shifted = 0i64;
    for span in spans {
        let start = i64::from(span.start_line);
        let removed_end = start + i64::from(span.removed_lines) + 1;
        let inserted_end = start + i64::from(span.inserted_lines) + 1;
        let shift = inserted_end - removed_end;
        let carried = window.map(|(low, high)| {
            let low = if low < start {
                low
            } else if low >= removed_end {
                low + shift
            } else {
                start
            };
            let high = if high <= start {
                high
            } else if high >= removed_end {
                high + shift
            } else {
                inserted_end
            };
            (low.min(start), high.max(inserted_end))
        });
        window = Some(carried.unwrap_or((start, inserted_end)));
        shifted += shift;
    }
    let (low, high) = window?;
    let line = |value: i64| u32::try_from(value.max(0)).unwrap_or(u32::MAX);
    Some((line(low), line(high - shifted), line(high)))
}

fn empty_text() -> ca_text::LoadedText {
    ca_text::LoadedText::load(b"", &ca_text::DecodeOptions::default())
}

impl MergeView {
    /// A tab over the paths a request names.
    #[must_use]
    pub fn from_request(request: &OpenRequest, context: &ViewContext, salt: u64) -> Self {
        let paths = MergePaths {
            left: request.left.clone(),
            center: request.center.clone(),
            right: request.right.clone(),
            output: request.output.clone(),
        };
        Self::over(paths, request.titles.clone(), context, salt)
    }

    /// A tab over two versions with no ancestor.
    #[must_use]
    pub fn new(left: PathBuf, right: PathBuf, context: &ViewContext, salt: u64) -> Self {
        let paths = MergePaths {
            left,
            center: None,
            right,
            output: None,
        };
        Self::over(paths, Titles::default(), context, salt)
    }

    /// A tab over a whole set of merge paths.
    #[must_use]
    pub fn over(paths: MergePaths, titles: Titles, context: &ViewContext, salt: u64) -> Self {
        let mut view = Self {
            id: egui::Id::new(("merge", salt)),
            paths,
            titles,
            session_settings: ca_session::settings::TextMergeSettings::default(),
            data: MergeData {
                model: MergeModel::default(),
                sources: Box::new(jobs::Sources {
                    left: empty_text(),
                    center: None,
                    right: empty_text(),
                    left_facts: jobs::SideFacts::default(),
                    center_facts: jobs::SideFacts::default(),
                    right_facts: jobs::SideFacts::default(),
                }),
                widest: 0,
                output_baseline: Baseline::Unchecked,
            },
            status: Status::Running("Reading files"),
            job: None,
            save_job: None,
            save_rules: ca_ui::save::SaveRules::default(),
            saving_output: None,
            saving_revision: None,
            saving_conflicts: None,
            output_pane: ca_ui::editor::Pane::default(),
            scroll: RowScroll::top(),
            horizontal: 0.0,
            current: 0,
            navigation_started: false,
            row: 0,
            filter: MergeFilter::All,
            context_lines: ca_session::options::ProgramOptions::default()
                .text_editing
                .context_lines,
            visible: Visible::default(),
            rules: DisplayRules::default(),
            selection_color: ca_ui::theme::palette(Variant::Light).selection,
            find_task: FindTask::default(),
            find: FindPanel::with_settings(FindSettings {
                wrap: true,
                ..FindSettings::default()
            }),
            focus: Pane::Output,
            show_center: true,
            show_thumbnail: true,
            show_line_numbers: true,
            font: ca_ui::font::FontSize::new(DEFAULT_FONT_SIZE),
            line_spacing: 0,
            strip: Strip::default(),
            strip_stale: true,
            question: None,
            consent: SaveConsent::default(),
            accept_markers: false,
            output_baseline: Baseline::Unchecked,
            saved_output: None,
            saved_conflicts: None,
            output_changed_on_load: false,
            message: None,
            failed: false,
            notify: Arc::clone(&context.notify),
            widgets: Vec::new(),
            window: egui::Rect::NOTHING,
            panes: Vec::new(),
            output_glyphs: Vec::new(),
            report: ViewReport::new(
                ReportKind::Merge,
                egui::Id::new(("text-merge", salt)),
                context.notify.clone(),
            ),
            toolbar_rect: egui::Rect::NOTHING,
            status_rect: egui::Rect::NOTHING,
            toolbar_rows: 1,
            closing: false,
            pending: Vec::new(),
            pending_clipboard: None,
            paste_requested: false,
            swapped: false,
        };
        view.start_merge();
        view
    }

    /// Which kind of session this view answers for.
    #[must_use]
    pub fn kind() -> SessionKind {
        SessionKind::TextMerge
    }

    /// The paths this session reads and writes.
    #[must_use]
    pub const fn paths(&self) -> &MergePaths {
        &self.paths
    }

    /// The left side path.
    #[must_use]
    pub fn left(&self) -> &Path {
        &self.paths.left
    }

    /// The right side path.
    #[must_use]
    pub fn right(&self) -> &Path {
        &self.paths.right
    }

    /// The merge and its section states.
    #[must_use]
    pub const fn model(&self) -> &MergeModel {
        &self.data.model
    }

    /// The section the caret is in.
    #[must_use]
    pub const fn current_section(&self) -> usize {
        self.current
    }

    /// The pane the caret is in.
    #[must_use]
    pub const fn focused_pane(&self) -> Pane {
        self.focus
    }

    /// The model row the caret or the last click is on.
    #[must_use]
    pub const fn current_row(&self) -> usize {
        self.row
    }

    /// Put the current row on a model row of `pane`, as a click on it does.
    pub fn place_on_row(&mut self, row: usize, pane: Pane) {
        let Some(section) = self.data.model.section_of_row(row) else {
            return;
        };
        self.row = row;
        self.current = section;
        self.navigation_started = true;
        self.focus = pane;
        if let Some(line) = self.data.model.output_line_for_row(row) {
            self.output_pane.place(Caret::new(line, 0), false);
            let _ = self.output_pane.take_changes();
        }
    }

    /// The display filter in force.
    #[must_use]
    pub const fn filter(&self) -> MergeFilter {
        self.filter
    }

    /// The model rows the filter shows.
    #[must_use]
    pub const fn visible_rows(&self) -> &Visible {
        &self.visible
    }

    /// Which differences count as none.
    #[must_use]
    pub const fn rules(&self) -> DisplayRules {
        self.rules
    }

    /// The find, replace and go to strips.
    pub fn find_panel(&mut self) -> &mut FindPanel {
        &mut self.find
    }

    /// The output pane's editing model.
    #[must_use]
    pub const fn output_pane(&self) -> &ca_ui::editor::Pane {
        &self.output_pane
    }

    /// The output text the view would write.
    #[must_use]
    pub fn output_text(&self) -> String {
        self.data.model.output_text()
    }

    /// The question the view is waiting on, if any.
    #[must_use]
    pub const fn question(&self) -> Option<&Question> {
        self.question.as_ref()
    }

    /// True while a write of the output is still running.
    ///
    /// Nothing the view reports about a save is settled until this is false:
    /// the file on disk can already hold the bytes while the outcome of the
    /// write has not reached the view.
    #[must_use]
    pub const fn is_saving(&self) -> bool {
        self.save_job.is_some()
    }

    /// True when the output differs from the text this session last wrote,
    /// or, before the first write, from the output the merge run produced.
    #[must_use]
    pub fn is_modified(&self) -> bool {
        self.output_changed_on_load || self.output_pane.is_modified()
    }

    /// What a caller that started this merge learns from it.
    #[must_use]
    pub fn outcome(&self) -> Outcome {
        Outcome {
            conflicts_remaining: self
                .saved_conflicts
                .unwrap_or_else(|| self.data.model.totals().conflicts_remaining),
            written: self.saved_output.is_some(),
            failed: self.failed,
        }
    }

    /// Every rectangle the last frame drew, for a layout check.
    #[must_use]
    pub fn widgets(&self) -> &[WidgetRect] {
        &self.widgets
    }

    /// The window the last frame ran in.
    #[must_use]
    pub const fn window(&self) -> egui::Rect {
        self.window
    }

    /// Where one pane was drawn in the last frame.
    #[must_use]
    pub fn pane_rect(&self, pane: Pane) -> Option<egui::Rect> {
        self.panes
            .iter()
            .find(|(named, _)| *named == pane)
            .map(|(_, rect)| *rect)
    }

    /// Where the toolbar was drawn.
    #[must_use]
    pub const fn toolbar_rect(&self) -> egui::Rect {
        self.toolbar_rect
    }

    /// Where the status bar was drawn.
    #[must_use]
    pub const fn status_rect(&self) -> egui::Rect {
        self.status_rect
    }

    /// How many lines the toolbar wrapped onto.
    #[must_use]
    pub const fn toolbar_rows(&self) -> usize {
        self.toolbar_rows
    }

    /// Height of one row at the current display font size.
    #[must_use]
    pub fn row_height(&self) -> f32 {
        self.font.row_height(ROW_HEIGHT_RATIO, self.line_spacing)
    }

    /// The point size every pane draws at.
    fn font_size(&self) -> f32 {
        self.font.points()
    }

    /// Take the sizes the options document now states.
    ///
    /// The four panes share one row grid, so one size applies to all of them.
    fn follow_options(&mut self, ctx: &egui::Context) {
        let options = ca_ui::options::current(ctx);
        let before = (self.font.points(), self.line_spacing);
        self.font.follow(options.editor_point_size());
        self.line_spacing = options.extra_line_spacing();
        self.save_rules = ca_ui::save::SaveRules::from_options(&options.stored);
        if before != (self.font.points(), self.line_spacing) {
            self.strip_stale = true;
        }
        let context = options.stored.text_editing.context_lines;
        if context != self.context_lines {
            self.context_lines = context;
            if let MergeFilter::Context(_) = self.filter {
                self.set_filter(MergeFilter::Context(context));
            }
        }
    }

    /// The panes that are on screen, left to right.
    #[must_use]
    pub fn visible_inputs(&self) -> Vec<Pane> {
        Pane::INPUTS
            .into_iter()
            .filter(|pane| *pane != Pane::Center || self.shows_center())
            .collect()
    }

    /// True when the ancestor pane is on screen.
    #[must_use]
    pub const fn shows_center(&self) -> bool {
        self.show_center && !self.paths.is_two_way()
    }

    /// The title one pane's header shows.
    #[must_use]
    pub fn pane_title(&self, pane: Pane) -> String {
        let supplied = match pane {
            Pane::Left => &self.titles.left,
            Pane::Center => &self.titles.center,
            Pane::Right => &self.titles.right,
            Pane::Output => &self.titles.output,
        };
        if let Some(name) = supplied {
            return name.clone();
        }
        self.paths.input(pane).map_or_else(
            || pane.label().to_owned(),
            |path| path.display().to_string(),
        )
    }

    /// The fields the status bar shows.
    #[must_use]
    pub fn status_fields(&self) -> Vec<String> {
        let totals = self.data.model.totals();
        let mut fields = vec![
            format!("{} difference section(s)", totals.differences),
            format!("{} conflict(s) remaining", totals.conflicts_remaining),
            format!(
                "Taken: {} left, {} right, {} center",
                totals.taken_left, totals.taken_right, totals.taken_center
            ),
        ];
        if let Some(section) = self.data.model.sections().get(self.current) {
            fields.push(format!("Current section: {}", section.resolution.label()));
        }
        if self.paths.is_two_way() {
            fields.push("No common ancestor: every difference needs review".to_owned());
        }
        fields
    }

    /// The information line of every pane that reads a file.
    #[must_use]
    pub fn file_info_lines(&self) -> Vec<widgets::FileInfo> {
        self.visible_inputs()
            .into_iter()
            .map(|pane| {
                let facts = self.data.sources.facts(pane);
                let mut info = widgets::FileInfo::new(pane.label());
                info.size = Some(facts.size);
                info.modified = facts.stamp.and_then(|stamp| stamp.modified);
                info.encoding = Some(facts.encoding.clone());
                info.line_ending = Some(facts.eol.clone());
                info
            })
            .collect()
    }

    fn start_merge(&mut self) {
        if let Some(job) = self.job.take() {
            job.cancel();
        }
        self.status = Status::Running("Reading files");
        self.job = Some(jobs::spawn(
            self.paths.clone(),
            crate::settings::options_from(&self.session_settings),
            jobs::MergeDecoding {
                left: crate::settings::decode_from(&self.session_settings.format.left_encoding),
                right: crate::settings::decode_from(&self.session_settings.format.right_encoding),
            },
            Arc::clone(&self.notify),
        ));
    }

    fn drain_jobs(&mut self) {
        let Some(job) = self.job.as_mut() else {
            self.drain_save();
            return;
        };
        let messages = job.drain();
        let mut finished = job.is_finished();
        for message in messages {
            match message {
                MergeMessage::Progress(step) => self.status = Status::Running(step),
                MergeMessage::Failed(reason) => {
                    self.status = Status::Failed(reason);
                    self.failed = true;
                    finished = true;
                }
                MergeMessage::Cancelled => {
                    self.status = Status::Cancelled;
                    finished = true;
                }
                MergeMessage::Ready(data) => {
                    self.adopt(*data);
                    finished = true;
                }
            }
        }
        if finished {
            self.job = None;
        }
        self.drain_save();
    }

    fn drain_save(&mut self) {
        let Some(job) = self.save_job.as_mut() else {
            return;
        };
        let messages = job.drain();
        let mut finished = job.is_finished();
        for message in messages {
            match message {
                SaveMessage::Cancelled => finished = true,
                SaveMessage::Done(outcome) => {
                    self.finish_save(*outcome);
                    finished = true;
                }
            }
        }
        if finished {
            self.save_job = None;
        }
    }

    fn finish_save(&mut self, outcome: SaveOutcome) {
        let saved_output = self.saving_output.take();
        let saving_revision = self.saving_revision.take();
        let saving_conflicts = self.saving_conflicts.take();
        self.consent = SaveConsent::default();
        let conflict_backup = match &outcome {
            SaveOutcome::SavedWithConflict { backup, .. } => Some(backup.display().to_string()),
            _ => None,
        };
        match outcome {
            SaveOutcome::Saved(stamp) | SaveOutcome::SavedWithConflict { stamp, .. } => {
                self.output_baseline = Baseline::Present(stamp);
                self.saved_output = saved_output;
                self.saved_conflicts = saving_conflicts;
                self.output_changed_on_load = false;
                if let Some(revision) = saving_revision {
                    self.output_pane.buffer_mut().mark_saved_revision(revision);
                }
                if let Some(backup) = &conflict_backup {
                    self.question = Some(Question::KeptCopy(backup.clone()));
                }
                self.message = Some(conflict_backup.map_or_else(
                    || "Output written".to_owned(),
                    |backup| {
                        format!(
                            "Output written, but another version was kept at {backup}. Review it."
                        )
                    },
                ));
            }
            SaveOutcome::ChangedOnDisk => self.question = Some(Question::OverwriteChanged),
            SaveOutcome::WouldLose(reason) => self.question = Some(Question::AcceptLoss(reason)),
            SaveOutcome::NotWritable => {
                self.failed = true;
                self.message = Some("The output file cannot be replaced".to_owned());
            }
            SaveOutcome::Failed(reason) => {
                self.failed = true;
                self.message = Some(format!("The output was not written: {reason}"));
            }
        }
    }

    /// Take a finished merge, keeping decisions whose region did not change.
    fn adopt(&mut self, mut data: MergeData) {
        let generated_output = data.model.output_text();
        if std::mem::take(&mut self.swapped) {
            data.model.carry_over_swapped(&self.data.model);
        } else {
            data.model.carry_over(&self.data.model);
        }
        let baseline = self.saved_output.as_deref().unwrap_or(&generated_output);
        self.output_changed_on_load = !output_lines_equal_text(data.model.output_lines(), baseline);
        data.model.set_rules(self.rules);
        // The baseline of the output is taken once. A later run must not adopt
        // the state of a file that was written after the session started.
        if self.output_baseline == Baseline::Unchecked {
            self.output_baseline = data.output_baseline;
        }
        self.data = data;
        self.status = Status::Ready;
        self.touched();
        self.current = self
            .current
            .min(self.data.model.sections().len().saturating_sub(1));
        self.navigation_started = false;
        self.reset_output_buffer();
    }

    fn reset_output_buffer(&mut self) {
        let column = self.output_pane.caret_column();
        let line = self.output_pane.caret().line;
        self.output_pane
            .reset(TextBuffer::from_text(&self.data.model.output_text()));
        self.output_pane.set_read_only(self.editing_off());
        let last = self.output_pane.line_count().saturating_sub(1);
        self.output_pane
            .place(Caret::new(line.min(last), column), false);
        let _ = self.output_pane.take_changes();
    }

    /// Synchronize only the sections a take changed, keeping the output pane's
    /// saved point and undo history intact.
    fn replace_output_sections(&mut self, changed: &[(usize, Range<u32>)]) {
        if changed.is_empty() {
            return;
        }
        let ranges: Vec<_> = changed
            .iter()
            .filter_map(|(section, old)| {
                self.data
                    .model
                    .output_range(*section)
                    .map(|new| (old.clone(), new))
            })
            .collect();
        self.replace_output_ranges(&ranges);
    }

    /// Synchronize changed output spans while keeping the pane's undo history.
    fn replace_output_ranges(&mut self, changed: &[(Range<u32>, Range<u32>)]) {
        if changed.is_empty() {
            return;
        }
        let caret = self.output_pane.caret();
        let mut replacements = changed.to_vec();
        replacements.sort_by_key(|(old, _)| old.start);
        let mut caret_line = caret.line;
        for (old, new) in &replacements {
            let start = i64::from(old.start);
            let end = i64::from(old.end);
            let new_len = i64::from(new.end.saturating_sub(new.start));
            let old_len = end.saturating_sub(start);
            if i64::from(caret_line) >= end {
                caret_line = u32::try_from(
                    i64::from(caret_line).saturating_add(new_len.saturating_sub(old_len)),
                )
                .unwrap_or(u32::MAX);
            } else if i64::from(caret_line) >= start {
                caret_line = old.start;
            }
        }
        // A take can repair the preceding terminator. Include both adjacent
        // lines so replacing bytes cannot coalesce a CR and LF at either seam.
        let old_end = self.output_pane.line_count();
        let new_end = u32::try_from(self.data.model.output_lines().len()).unwrap_or(u32::MAX);
        let mut expanded: Vec<(Range<u32>, Range<u32>)> = Vec::new();
        for (old, new) in replacements {
            let old = old.start.saturating_sub(1)..old.end.saturating_add(1).min(old_end);
            let new = new.start.saturating_sub(1)..new.end.saturating_add(1).min(new_end);
            if let Some((previous_old, previous_new)) = expanded.last_mut() {
                if old.start <= previous_old.end {
                    previous_old.end = previous_old.end.max(old.end);
                    previous_new.end = previous_new.end.max(new.end);
                    continue;
                }
            }
            expanded.push((old, new));
        }
        self.output_pane.buffer_mut().begin_group();
        for (old, new) in expanded.iter().rev() {
            let text = self.data.model.output_text_range(new.clone());
            self.output_pane
                .replace_lines(LineRange::new(old.start, old.end), &text);
        }
        self.output_pane.buffer_mut().end_group();
        self.output_pane
            .place(Caret::new(caret_line, caret.index), false);
        let _ = self.output_pane.take_changes();
    }

    /// True while the editing switch of the session is on: the output takes
    /// no edit and is not written.
    fn editing_off(&self) -> bool {
        self.session_settings.specs.disable_editing
    }

    /// Fold the buffer's edits back into the sections they landed in.
    ///
    /// The pane supplies the saved text. Keep the model's sections in step
    /// with it so later takes and aligned rows address the same actual lines.
    fn absorb_output_edits(&mut self) {
        let spans = self.output_pane.take_changes();
        let Some((first, old_end, new_end)) = edited_window(&spans) else {
            return;
        };
        // The pane counts an empty line after a final terminator; the model
        // holds no line there.
        let output_len = u32::try_from(self.data.model.output_lines().len()).unwrap_or(u32::MAX);
        let last = old_end.min(output_len);
        if let Some(section) = self.data.model.section_of_output_line(first) {
            if let Some(section_range) = self.data.model.output_range(section) {
                if section_range.start <= first && last <= section_range.end {
                    let replacement = self.buffer_lines(first, new_end);
                    let local_start = first - section_range.start;
                    let local_end = last - section_range.start;
                    let status = self.data.model.sections()[section].status();
                    let row_count = self
                        .data
                        .model
                        .section_row_range(section)
                        .map_or(0, |range| range.len());
                    if self.data.model.edit_output_range(
                        section,
                        local_start..local_end,
                        replacement,
                    ) {
                        self.current = section;
                        let entry = &self.data.model.sections()[section];
                        let rows_changed = self
                            .data
                            .model
                            .section_row_range(section)
                            .is_some_and(|range| range.len() != row_count);
                        if entry.status() != status || rows_changed {
                            self.touched();
                        } else {
                            self.strip_stale = true;
                        }
                        return;
                    }
                }
            }
        }
        let Some(from) = self.data.model.section_of_output_line(first) else {
            return;
        };
        let to = self
            .data
            .model
            .section_of_output_line(last.saturating_sub(1).max(first))
            .unwrap_or(from)
            .max(from);
        let Some(start) = self.data.model.output_range(from).map(|range| range.start) else {
            return;
        };
        let Some(end_before) = self.data.model.output_range(to).map(|range| range.end) else {
            return;
        };
        let end = if end_before >= output_len {
            self.output_pane.line_count()
        } else {
            u32::try_from(i64::from(end_before) + i64::from(new_end) - i64::from(old_end))
                .unwrap_or(start)
        };
        let lines = self.buffer_lines(start, end.max(start));
        for section in (from + 1)..=to {
            self.data.model.set_edited(section, Vec::new());
        }
        self.data.model.set_edited(from, lines);
        self.current = from;
        self.touched();
    }

    /// The buffer's lines in a range, each with the terminator the buffer
    /// holds for it.
    ///
    /// The empty line after a final terminator is not a line of the output.
    fn buffer_lines(&self, start: u32, end: u32) -> Vec<String> {
        let total = self.output_pane.line_count();
        let mut lines = Vec::new();
        for line in start..end.min(total) {
            if let Some(text) = self.output_pane.buffer().line(line) {
                if text.len_chars() > 0 {
                    lines.push(text.to_string());
                }
            }
        }
        lines
    }

    fn terminator(&self) -> &'static str {
        match self.data.sources.left_facts.eol.as_str() {
            "Windows" => "\r\n",
            "Mac" => "\r",
            _ => "\n",
        }
    }

    fn take(&mut self, resolution: Resolution) {
        if self.status != Status::Ready {
            return;
        }
        if self.editing_off() {
            self.message = Some(EDITING_OFF.to_owned());
            return;
        }
        let sections = self.selected_sections();
        if sections.is_empty() {
            return;
        }
        let changed: Vec<_> = sections
            .iter()
            .filter_map(|&index| {
                self.data
                    .model
                    .output_range(index)
                    .map(|range| (index, range))
            })
            .collect();
        self.data.model.set_resolutions(&sections, resolution);
        self.current = sections[0];
        self.touched();
        self.replace_output_sections(&changed);
    }

    /// Take one input's version of the current line into the output.
    fn take_line(&mut self, pane: Pane) {
        if self.status != Status::Ready {
            return;
        }
        let row = self.row;
        if let Some((section, old, new)) = self.data.model.take_line_with_range(row, pane) {
            self.current = section;
            self.touched();
            self.replace_output_ranges(&[(old, new)]);
        } else {
            self.message = Some("The output already holds that line".to_owned());
        }
    }

    /// Record that the model changed: the strip is redrawn and the filter
    /// applied again.
    fn touched(&mut self) {
        self.strip_stale = true;
        self.visible = filter::visible(&self.data.model, self.filter);
    }

    /// Exchange the left and right inputs and merge again.
    fn swap_sides(&mut self) {
        std::mem::swap(&mut self.paths.left, &mut self.paths.right);
        std::mem::swap(&mut self.titles.left, &mut self.titles.right);
        let format = &mut self.session_settings.format;
        std::mem::swap(&mut format.left_encoding, &mut format.right_encoding);
        std::mem::swap(&mut format.left_format, &mut format.right_format);
        let specs = &mut self.session_settings.specs;
        std::mem::swap(&mut specs.left, &mut specs.right);
        self.swapped = true;
        self.start_merge();
    }

    /// Show only the lines `filter` keeps.
    fn set_filter(&mut self, filter: MergeFilter) {
        let row = self.row;
        self.filter = filter;
        self.touched();
        let viewport = self.viewport_height();
        let row_height = self.row_height();
        if let Some(position) = self.visible.position_of(row) {
            self.scroll
                .center_on(position, viewport, row_height, self.visible.len());
        } else {
            self.scroll = RowScroll::top();
        }
    }

    /// Change which differences count as none.
    fn set_rules(&mut self, rules: DisplayRules) {
        self.rules = rules;
        self.data.model.set_rules(rules);
        self.touched();
    }

    /// The sections a section command acts on: every section the output
    /// selection touches, or the current section.
    fn selected_sections(&self) -> Vec<usize> {
        if self.focus == Pane::Output && self.output_pane.selection().is_some() {
            let lines = self.output_pane.selected_lines();
            let model = &self.data.model;
            let first = model.section_of_output_line(lines.start);
            let last = model.section_of_output_line(lines.end.saturating_sub(1).max(lines.start));
            if let (Some(first), Some(last)) = (first, last) {
                return (first..=last.max(first)).collect();
            }
        }
        if self.current < self.data.model.sections().len() {
            vec![self.current]
        } else {
            Vec::new()
        }
    }

    /// The sections the Conflict command acts on: the selected ones that hold
    /// a change, because lines neither side changed have nothing to review.
    fn conflict_sections(&self) -> Vec<usize> {
        let model = &self.data.model;
        self.selected_sections()
            .into_iter()
            .filter(|index| {
                model.sections().get(*index).is_some_and(|section| {
                    !matches!(section.kind, ca_diff::merge3::MergeKind::Unchanged)
                        || section.conflict
                })
            })
            .collect()
    }

    fn conflict_applies(&self) -> bool {
        !self.conflict_sections().is_empty()
    }

    fn toggle_conflict(&mut self) {
        let sections = self.conflict_sections();
        if sections.is_empty() {
            return;
        }
        let set = self.data.model.toggle_conflict(&sections);
        self.message = Some(
            if set {
                "Marked as a conflict"
            } else {
                "Conflict cleared"
            }
            .to_owned(),
        );
        self.touched();
    }

    fn toggle_ignored(&mut self) {
        let sections = self.selected_sections();
        if sections.is_empty() {
            return;
        }
        let set = self.data.model.toggle_ignored(&sections);
        self.message = Some(
            if set {
                "Differences ignored"
            } else {
                "Differences no longer ignored"
            }
            .to_owned(),
        );
        self.touched();
    }

    fn go_to_section(&mut self, section: Option<usize>) {
        let Some(section) = section else {
            self.message = Some("No further section".to_owned());
            return;
        };
        self.current = section;
        self.navigation_started = true;
        if let Some(row) = self.data.model.row_of_section(section) {
            self.row = row;
            self.show_row(row);
            let line = self.data.model.output_line_for_row(row).unwrap_or(0);
            self.output_pane.place(Caret::new(line, 0), false);
            let _ = self.output_pane.take_changes();
        }
    }

    /// Scroll so a model row is in the middle of the panes, or the row after
    /// it when the filter hides it.
    fn show_row(&mut self, row: usize) {
        let viewport = self.viewport_height();
        let row_height = self.row_height();
        if let Some(position) = self.visible.position_of(row) {
            self.scroll
                .center_on(position, viewport, row_height, self.visible.len());
        }
    }

    /// Follow the output caret: the current row, the current section and the
    /// scroll position all move to its line.
    fn follow_caret(&mut self) {
        let line = self.output_pane.caret().line;
        if let Some(row) = self.data.model.row_of_output_line(line) {
            self.row = row;
            if let Some(section) = self.data.model.section_of_row(row) {
                self.current = section;
                self.navigation_started = true;
            }
            self.show_row(row);
        }
    }

    fn run_find(&mut self, backwards: bool) {
        self.start_find(FindOperation::Next(backwards));
    }

    fn start_find(&mut self, operation: FindOperation) {
        if self.status != Status::Ready {
            return;
        }
        self.find_task.start(
            &self.output_pane,
            &self.find.settings,
            operation,
            Arc::clone(&self.notify),
        );
        self.message = Some("Searching…".to_owned());
    }

    fn poll_find(&mut self) {
        if self.status != Status::Ready {
            self.find_task.cancel();
            return;
        }
        if let Some(done) = self
            .find_task
            .poll(&mut self.output_pane, &self.find.settings)
        {
            self.message = done.message;
            if done.edited {
                self.absorb_output_edits();
                self.follow_caret();
            }
            if done.selected {
                self.focus = Pane::Output;
                self.follow_caret();
            }
        }
    }

    fn initial_navigation_target(
        &mut self,
        next: Option<usize>,
        current_matches: bool,
    ) -> Option<usize> {
        let target = if !self.navigation_started && current_matches {
            Some(self.current)
        } else {
            next
        };
        self.navigation_started = true;
        target
    }

    /// Carry out what the find or go to strip asked for, in the output pane.
    fn answer_panel(&mut self, request: Option<PanelRequest>) {
        if self.status != Status::Ready {
            return;
        }
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
                find::go_to(&mut self.output_pane, line, 1);
                self.focus = Pane::Output;
                self.follow_caret();
            }
            Some(PanelRequest::NoLine) => self.message = Some(find::NO_LINE.to_owned()),
        }
    }

    fn viewport_height(&self) -> f32 {
        self.pane_rect(Pane::Output)
            .map_or(200.0, |rect| rect.height())
    }

    fn save_output(&mut self) {
        if matches!(self.question, Some(Question::KeptCopy(_))) {
            return;
        }
        if self.editing_off() {
            self.message = Some(EDITING_OFF.to_owned());
            return;
        }
        if !self.consent.accept_loss
            && output::contains_lossy_input(&self.data.model, &self.data.sources)
        {
            self.question = Some(Question::AcceptLoss(
                "The output includes replacement characters from an input that did not decode cleanly."
                    .to_owned(),
            ));
            return;
        }
        let Some(path) = self.paths.output.clone() else {
            self.message = Some(NO_OUTPUT.to_owned());
            return;
        };
        if self.save_job.is_some() {
            return;
        }
        let conflicts = self.data.model.totals().conflicts_remaining;
        if conflicts > 0 && !self.accept_markers {
            self.question = Some(Question::SaveWithConflicts);
            return;
        }
        let markers = (conflicts > 0).then(|| self.marker_labels());
        let model = markers.as_ref().map(|_| self.data.model.clone());
        let snapshot = self.output_pane.buffer().edit_snapshot();
        self.saving_output = Some(self.output_pane.buffer().text());
        self.saving_revision = Some(self.output_pane.buffer().revision());
        self.saving_conflicts = Some(conflicts);
        let template = self.data.sources.left.clone();
        let expected = self.output_baseline;
        let consent = self.consent;
        let rules = self.save_rules.clone();
        self.save_job = Some(ca_ui::save::spawn_with(
            Arc::clone(&self.notify),
            move || {
                if let Some(model) = model {
                    output::save_with_endings(
                        &rules.files(),
                        &path,
                        &model,
                        &template,
                        markers.as_ref(),
                        expected,
                        consent,
                        rules.line_endings,
                    )
                } else {
                    let mut loaded = template;
                    loaded.buffer = TextBuffer::from_rope(snapshot.buffer().rope().clone());
                    loaded.had_errors = false;
                    ca_ui::save::text::save_with_endings(
                        &rules.files(),
                        &path,
                        &loaded,
                        expected,
                        consent,
                        rules.line_endings,
                    )
                }
            },
        ));
    }

    fn marker_labels(&self) -> MarkerLabels {
        MarkerLabels {
            left: self.pane_title(Pane::Left),
            center: self.pane_title(Pane::Center),
            right: self.pane_title(Pane::Right),
        }
    }

    /// Answer the question the view is waiting on.
    pub fn answer(&mut self, accept: bool) {
        let Some(question) = self.question.take() else {
            return;
        };
        if !accept {
            return;
        }
        match question {
            Question::KeptCopy(_) => {}
            Question::SaveWithConflicts => {
                self.accept_markers = true;
                self.save_output();
            }
            Question::OverwriteChanged => {
                self.consent.accept_disk_change = true;
                self.save_output();
            }
            Question::AcceptLoss(_) => {
                self.consent.accept_loss = true;
                self.save_output();
            }
            Question::CloseModified => self.closing = true,
        }
    }

    fn palette(context: &ViewContext) -> Palette {
        let variant = if context.palette.same_line.r() < 0x80 {
            Variant::Dark
        } else {
            Variant::Light
        };
        merge_palette(variant)
    }

    fn character_width(&self, ui: &egui::Ui) -> f32 {
        let font = egui::FontId::monospace(self.font_size());
        ui.fonts(|fonts| {
            let scale = fonts.pixels_per_point();
            (fonts.glyph_width(&font, '0') * scale).round() / scale
        })
        .max(1.0)
    }

    fn gutter_width(&self, ui: &egui::Ui, pane: Pane) -> f32 {
        let numbers = if self.show_line_numbers {
            self.character_width(ui) * LINE_NUMBER_COLUMNS
        } else {
            0.0
        };
        if pane == Pane::Output {
            numbers + ARROW_WIDTH
        } else {
            numbers
        }
    }
}

// Painting.
impl MergeView {
    /// The toolbar items this view declares, in the state it is in now.
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn toolbar_items(&self) -> Vec<toolbar::Item> {
        let ready = self.status == Status::Ready;
        vec![
            toolbar::Item::command(
                "previous-conflict",
                Command::PreviousConflict,
                "Prev Conflict",
                ready,
                NOT_MERGED,
            ),
            toolbar::Item::command(
                "next-conflict",
                Command::NextConflict,
                "Next Conflict",
                ready,
                NOT_MERGED,
            ),
            toolbar::Item::separator("separator-1"),
            toolbar::Item::command(
                "take-left",
                Command::TakeLeft,
                "Take Left",
                ready,
                NOT_MERGED,
            ),
            toolbar::Item::command(
                "take-center",
                Command::TakeCenter,
                "Take Center",
                ready && self.shows_center(),
                NOT_MERGED,
            ),
            toolbar::Item::command(
                "take-right",
                Command::TakeRight,
                "Take Right",
                ready,
                NOT_MERGED,
            ),
            toolbar::Item::command(
                "take-both",
                Command::TakeLeftThenRight,
                "Take Both",
                ready,
                NOT_MERGED,
            ),
            toolbar::Item::command(
                "take-all",
                Command::TakeAllNonConflicting,
                "Take All",
                ready,
                NOT_MERGED,
            ),
            toolbar::Item::toggle(
                "favor-left",
                Command::FavorLeft,
                "Favor Left",
                true,
                self.rules.favor_left,
            ),
            toolbar::Item::toggle(
                "favor-right",
                Command::FavorRight,
                "Favor Right",
                true,
                self.rules.favor_right,
            ),
            toolbar::Item::toggle(
                "ignore-same",
                Command::ToggleIgnoreSameChanges,
                "Ignore Same",
                true,
                self.rules.ignore_same_changes,
            ),
            toolbar::Item::separator("separator-2"),
            toolbar::Item::toggle(
                "center-pane",
                Command::ToggleCenterPane,
                "Center Pane",
                true,
                self.shows_center(),
            ),
            toolbar::Item::command(
                "save",
                Command::SaveFile,
                "Save",
                ready && self.paths.output.is_some() && !self.editing_off(),
                NOT_MERGED,
            ),
            toolbar::Item::command("reload", Command::Reload, "Reload", true, NOT_MERGED),
            toolbar::Item::command(
                "report",
                Command::CompareReport,
                "Report",
                ready,
                NOT_MERGED,
            ),
            toolbar::Item::separator("separator-3"),
            toolbar::Item::widget("filter", FILTER_COMBO_WIDTH),
            toolbar::Item::toggle(
                "minor",
                Command::ToggleIgnoreUnimportant,
                "Minor",
                true,
                self.rules.ignore_unimportant,
            ),
        ]
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        let background = ui.painter().add(egui::Shape::Noop);
        let items = self.toolbar_items();
        let layout = toolbar::Layout::from_options(
            &ca_ui::options::runtime::current(ui.ctx()).stored.commands,
            toolbar::ToolbarView::Merge,
        );
        let id = self.id;
        let current = self.filter;
        let mut chosen = None;
        let outcome = toolbar::show_for(
            toolbar::ToolbarView::Merge,
            ui,
            id.with("toolbar"),
            &items,
            &layout,
            |ui, name| {
                if name == "filter" {
                    egui::ComboBox::from_id_salt(id.with("filter"))
                        .width(FILTER_COMBO_WIDTH - 10.0)
                        .selected_text(current.label())
                        .show_ui(ui, |ui| {
                            for option in LISTED_FILTERS {
                                if ui
                                    .selectable_label(current.same_choice(option), option.label())
                                    .clicked()
                                {
                                    chosen = Some(option.command());
                                }
                            }
                        });
                }
            },
        );
        if let Some(command) = chosen {
            self.run(command);
        }
        let command = outcome.command;
        let bar = outcome.rect;
        self.toolbar_rect = bar;
        self.toolbar_rows = 1;
        ui.painter().set(
            background,
            egui::Shape::rect_filled(bar, 0.0, palette.unchanged_line),
        );
        self.widgets.push(WidgetRect {
            name: "toolbar",
            rect: bar,
        });
        if let Some(command) = command {
            self.run(command);
        }
    }

    /// The comparison the report is written from.
    #[must_use]
    ///
    /// The merge is reported as the two changed versions beside each other,
    /// row for row, which is what the four panes show.
    pub fn report_payload(&self) -> (ca_ui::report::ReportMeta, ca_ui::report::Payload) {
        let meta = ca_ui::report::ReportMeta::new(
            self.paths.left.display().to_string(),
            self.paths.right.display().to_string(),
        )
        .with_title(ReportKind::Merge.title());
        let rows = self
            .data
            .model
            .rows()
            .iter()
            .map(|row| ca_ui::report::TextRowRef {
                kind: match (row.left, row.right) {
                    (Some(_), Some(_)) => ca_ui::report::RowKind::Changed,
                    (Some(_), None) => ca_ui::report::RowKind::LeftOnly,
                    (None, Some(_)) => ca_ui::report::RowKind::RightOnly,
                    (None, None) => ca_ui::report::RowKind::Same,
                },
                importance: None,
                left: row.left,
                right: row.right,
            })
            .collect();
        (
            meta,
            ca_ui::report::Payload::Text(ca_ui::report::TextPayload {
                left: Arc::new(pane_lines(&self.data.sources.left.buffer)),
                right: Arc::new(pane_lines(&self.data.sources.right.buffer)),
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

    fn question_panel(&mut self, ui: &mut egui::Ui) {
        let Some(question) = self.question.clone() else {
            return;
        };
        if let Question::KeptCopy(path) = &question {
            egui::Window::new("Another version was kept")
                .id(self.id.with("kept-copy"))
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ui.ctx(), |ui| {
                    ca_ui::widgets::notice_current(ui,ca_ui::icons::Icon::Warning,24.0,"Another program changed the output during your save. The saved file contains your edit. Review the other version at:");
                    ui.monospace(path);
                    if ui.button("I have the path").clicked() {
                        self.question = None;
                    }
                });
            return;
        }
        let mut answer: Option<bool> = None;
        ui.horizontal_wrapped(|ui| {
            ca_ui::widgets::notice_current(
                ui,
                ca_ui::icons::Icon::Question,
                24.0,
                &question.prompt(),
            );
            if ui.button("Yes").clicked() {
                answer = Some(true);
            }
            if ui.button("No").clicked() {
                answer = Some(false);
            }
        });
        if let Some(accept) = answer {
            self.answer(accept);
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let rect = ui
            .horizontal_wrapped(|ui| match &self.status {
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
                    if let Some(message) = &self.message {
                        ui.separator();
                        ca_ui::widgets::notice_current(
                            ui,
                            ca_ui::icons::Icon::Info,
                            16.0,
                            message.as_str(),
                        );
                    }
                }
            })
            .response
            .rect;
        self.status_rect = rect;
        self.widgets.push(WidgetRect {
            name: "status",
            rect,
        });
    }

    fn thumbnail_strip(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        let rect = ui.available_rect_before_wrap();
        self.widgets.push(WidgetRect {
            name: "thumbnail",
            rect,
        });
        self.rebuild_strip(rect.height());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, palette.gap_line);
        for (index, bucket) in self.strip.buckets().iter().enumerate() {
            let Some(class) = bucket.class() else {
                continue;
            };
            let y = rect.top() + as_f32(index);
            let (background, _) = palette.row(class);
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(rect.left() + 2.0, y),
                    egui::pos2(rect.right() - 2.0, y + 1.0),
                ),
                0.0,
                background,
            );
        }
        let rows = self.visible.len();
        let thumbnail = ca_ui::thumbnail::Thumbnail::new(rect.height(), rows);
        let visible = as_usize((rect.height() / self.row_height()).ceil());
        let marker = thumbnail.viewport_marker(self.scroll.first_row(), visible);
        painter.rect_stroke(
            egui::Rect::from_min_max(
                egui::pos2(rect.left() + 1.0, rect.top() + marker.start),
                egui::pos2(rect.right() - 1.0, rect.top() + marker.end),
            ),
            0.0,
            egui::Stroke::new(1.0, palette.gap_pattern),
            egui::StrokeKind::Inside,
        );
    }

    fn rebuild_strip(&mut self, height: f32) {
        let rows = self.visible.len();
        if !self.strip_stale && self.strip.matches(height, rows) {
            return;
        }
        let model = &self.data.model;
        let visible = &self.visible;
        self.strip = Strip::from_rows(height, rows, |position| {
            let row = model.rows().get(visible.row_at(position)?)?;
            let section = model.sections().get(row.section as usize)?;
            let class = section.output_class();
            class.is_change().then_some(class)
        });
        self.strip_stale = false;
    }

    /// Lay the four panes out and paint them.
    fn comparison(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        let area = ui.available_rect_before_wrap();
        self.panes.clear();
        self.output_glyphs.clear();
        let inputs = self.visible_inputs();
        let input_height = (area.height() * INPUT_SHARE).max(self.row_height());
        let top = egui::Rect::from_min_max(
            area.min,
            egui::pos2(area.right(), area.top() + input_height),
        );
        let bottom =
            egui::Rect::from_min_max(egui::pos2(area.left(), top.bottom() + SEPARATOR), area.max);
        let count = as_f32(inputs.len().max(1));
        let width = ((top.width() - SEPARATOR * (count - 1.0)) / count).max(1.0);
        let clicked = self.handle_scroll(ui, area);
        for (index, pane) in inputs.iter().enumerate() {
            let left = top.left() + (width + SEPARATOR) * as_f32(index);
            let rect = egui::Rect::from_min_max(
                egui::pos2(left, top.top()),
                egui::pos2((left + width).min(top.right()), top.bottom()),
            );
            self.paint_pane(ui, rect, *pane, palette);
        }
        self.paint_pane(ui, bottom, Pane::Output, palette);
        if let Some(position) = clicked {
            self.click(position);
        }
    }

    fn paint_pane(&mut self, ui: &mut egui::Ui, rect: egui::Rect, pane: Pane, palette: &Palette) {
        self.panes.push((pane, rect));
        self.widgets.push(WidgetRect {
            name: pane.label(),
            rect,
        });
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, palette.unchanged_line);
        let row_height = self.row_height();
        let character = self.character_width(ui);
        let gutter = self.gutter_width(ui, pane);
        let header = row_height;
        let header_rect =
            egui::Rect::from_min_max(rect.min, egui::pos2(rect.right(), rect.top() + header));
        painter.rect_filled(header_rect, 0.0, palette.gap_line);
        painter.text(
            header_rect.left_center() + egui::vec2(4.0, 0.0),
            egui::Align2::LEFT_CENTER,
            widgets::elide_to_width(ui, &self.pane_title(pane), rect.width() - 8.0),
            egui::FontId::proportional(self.font_size()),
            palette.unchanged_text,
        );
        let body =
            egui::Rect::from_min_max(egui::pos2(rect.left(), header_rect.bottom()), rect.max);
        self.widgets.push(WidgetRect {
            name: "gutter",
            rect: egui::Rect::from_min_max(
                body.min,
                egui::pos2(body.left() + gutter, body.bottom()),
            ),
        });
        let rows = self.visible.len();
        let visible = self.scroll.visible(body.height(), row_height, rows);
        let shift = self.scroll.pixel_shift(row_height);
        let font = egui::FontId::monospace(self.font_size());
        let text_left = body.left() + gutter + 2.0 - self.horizontal * character;
        for index in visible.clone() {
            let Some(row_index) = self.visible.row_at(index) else {
                break;
            };
            let Some(row) = self.data.model.rows().get(row_index) else {
                break;
            };
            let Some(section) = self.data.model.sections().get(row.section as usize) else {
                break;
            };
            let y = body.top() + as_f32(index - visible.start) * row_height - shift;
            let line_rect = egui::Rect::from_min_size(
                egui::pos2(body.left(), y),
                egui::vec2(body.width(), row_height),
            );
            if line_rect.top() > body.bottom() {
                break;
            }
            let line = row.line(pane, self.data.model.output_line_for_row(row_index));
            let class = match line {
                None => MergeClass::Gap,
                Some(_) if pane == Pane::Output => section.output_class_under(self.rules),
                Some(_) => section.input_class(),
            };
            let (background, text_color) = palette.row(class);
            painter.rect_filled(line_rect, 0.0, background);
            if class == MergeClass::Gap {
                hatch(&painter, line_rect, palette.gap_pattern);
                continue;
            }
            let Some(line) = line else {
                continue;
            };
            if self.show_line_numbers {
                painter.text(
                    egui::pos2(body.left() + gutter - 2.0, line_rect.center().y),
                    egui::Align2::RIGHT_CENTER,
                    format!("{}", line + 1),
                    font.clone(),
                    palette.gap_pattern,
                );
            }
            let content = self
                .data
                .model
                .line(pane, line as usize)
                .map(|text| text.trim_end_matches(['\n', '\r']).to_owned())
                .unwrap_or_default();
            let galley = painter.layout_no_wrap(content, font.clone(), text_color);
            let origin = egui::pos2(text_left, line_rect.center().y - galley.size().y / 2.0);
            if pane == Pane::Output {
                self.paint_selection(&painter, line_rect, text_left, &galley, line, text_color);
                self.output_glyphs.push((line, origin, Arc::clone(&galley)));
            }
            painter.galley(origin, galley, text_color);
        }
        if pane == Pane::Output {
            self.paint_take_buttons(ui, body, gutter, palette, visible);
        }
    }

    /// The selection and the caret on one output line.
    ///
    /// Columns are counted in characters, which is how the caret and the find
    /// panel address a line.
    fn paint_selection(
        &self,
        painter: &egui::Painter,
        line_rect: egui::Rect,
        text_left: f32,
        galley: &egui::Galley,
        line: u32,
        caret_color: egui::Color32,
    ) {
        let column_x = |column: u32| {
            text_left
                + galley
                    .pos_from_ccursor(egui::text::CCursor::new(column as usize))
                    .left()
        };
        if let Some((start, end)) = self.output_pane.selection() {
            if start.line <= line && line <= end.line {
                let from = if line == start.line { start.index } else { 0 };
                let to = if line == end.line {
                    end.index
                } else {
                    u32::try_from(self.output_pane.line_text(line).chars().count())
                        .unwrap_or(u32::MAX)
                        .saturating_add(1)
                };
                painter.rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(column_x(from), line_rect.top()),
                        egui::pos2(column_x(to.max(from)), line_rect.bottom()),
                    ),
                    0.0,
                    self.selection_color,
                );
            }
        }
        let caret = self.output_pane.caret();
        if self.focus == Pane::Output && caret.line == line {
            let x = column_x(caret.index);
            painter.line_segment(
                [
                    egui::pos2(x, line_rect.top()),
                    egui::pos2(x, line_rect.bottom()),
                ],
                egui::Stroke::new(1.0, caret_color),
            );
        }
    }

    /// One pair of take buttons beside each section of the output pane.
    fn paint_take_buttons(
        &mut self,
        ui: &mut egui::Ui,
        body: egui::Rect,
        gutter: f32,
        palette: &Palette,
        visible: std::ops::Range<usize>,
    ) {
        let row_height = self.row_height();
        let shift = self.scroll.pixel_shift(row_height);
        let painter = ui.painter_at(body);
        let mut taken: Option<(usize, Resolution)> = None;
        let mut seen: Option<u32> = None;
        for index in visible.clone() {
            let Some(row) = self
                .visible
                .row_at(index)
                .and_then(|row| self.data.model.rows().get(row))
            else {
                break;
            };
            if seen == Some(row.section) {
                continue;
            }
            seen = Some(row.section);
            let Some(section) = self.data.model.sections().get(row.section as usize) else {
                break;
            };
            if !section.is_difference() {
                continue;
            }
            let y = body.top() + as_f32(index - visible.start) * row_height - shift;
            let half = ARROW_WIDTH / 2.0;
            let left =
                egui::Rect::from_min_size(egui::pos2(body.left(), y), egui::vec2(half, row_height));
            let right = egui::Rect::from_min_size(
                egui::pos2(body.left() + half, y),
                egui::vec2(half, row_height),
            );

            let section_index = row.section as usize;
            for (rect, resolution) in [(left, Resolution::Left), (right, Resolution::Right)] {
                let response = ui.interact(
                    rect,
                    self.id.with(("take", index, resolution as u8)),
                    egui::Sense::click(),
                );
                let (icon, label, tint) = if resolution == Resolution::Left {
                    (
                        ca_ui::icons::Icon::TakeLeft,
                        "Take Left into Output",
                        palette.left_text,
                    )
                } else {
                    (
                        ca_ui::icons::Icon::TakeRight,
                        "Take Right into Output",
                        palette.right_text,
                    )
                };
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
                });
                icon.paint_in_row(&painter, rect.center(), row_height, tint);
                if response.clicked() {
                    taken = Some((section_index, resolution));
                }
            }
        }
        let _ = gutter;
        if let Some((section, resolution)) = taken {
            self.current = section;
            self.take(resolution);
        }
    }

    fn handle_scroll(&mut self, ui: &mut egui::Ui, area: egui::Rect) -> Option<egui::Pos2> {
        let response = ui.interact(area, self.id.with("rows"), egui::Sense::click_and_drag());
        let rows = self.visible.len();
        let row_height = self.row_height();
        let viewport = self.viewport_height();
        if response.hovered() {
            let wheel = ui.input(|input| input.smooth_scroll_delta.y);
            if wheel.abs() > f32::EPSILON {
                self.scroll.scroll_by(-wheel, viewport, row_height, rows);
            }
        }
        if response.clicked() {
            response.interact_pointer_pos()
        } else {
            None
        }
    }

    fn click(&mut self, position: egui::Pos2) {
        let Some((pane, rect)) = self
            .panes
            .iter()
            .find(|(_, rect)| rect.contains(position))
            .copied()
        else {
            return;
        };
        self.focus = pane;
        let row_height = self.row_height();
        let body_top = rect.top() + row_height;
        let position_row = self.scroll.row_under(position.y - body_top, row_height);
        let Some(row) = self.visible.row_at(position_row) else {
            return;
        };
        self.row = row;
        if let Some(section) = self.data.model.section_of_row(row) {
            self.current = section;
            self.navigation_started = true;
        }
        if pane == Pane::Output {
            if let Some(line) = self.data.model.output_line_for_row(row) {
                let index = self
                    .output_glyphs
                    .iter()
                    .find(|held| held.0 == line)
                    .map_or(0, |(_, origin, galley)| {
                        let x = egui::vec2(position.x - origin.x, 0.0);
                        u32::try_from(galley.cursor_from_pos(x).ccursor.index).unwrap_or(u32::MAX)
                    });
                self.output_pane.place(Caret::new(line, index), false);
                let _ = self.output_pane.take_changes();
            }
        }
    }

    /// Turn the keys the output pane answers into edits.
    fn handle_keys(&mut self, ui: &egui::Ui) {
        if !ui.is_enabled() || self.status != Status::Ready || self.focus != Pane::Output {
            return;
        }
        // A text field of the find or go to strip holds the keyboard, so its
        // keystrokes must not also reach the output pane.
        if ui.ctx().wants_keyboard_input() {
            return;
        }
        let events = ui.input(|input| input.events.clone());
        let touched_caret = !events.is_empty();
        let mut edited = false;
        for event in events {
            match event {
                egui::Event::Text(text) => {
                    for character in text.chars() {
                        self.output_pane.type_character(character);
                    }
                    edited = true;
                }
                egui::Event::Copy => self.pending_clipboard = self.output_pane.copy(),
                egui::Event::Cut => {
                    self.pending_clipboard = self.output_pane.cut();
                    edited = true;
                }
                egui::Event::Paste(text) => {
                    self.output_pane.paste(&text);
                    edited = true;
                }
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => edited |= self.key(key, modifiers),
                _ => {}
            }
        }
        if edited {
            self.absorb_output_edits();
        }
        if touched_caret {
            self.sync_row();
        }
    }

    /// Move the current row and section to the output caret's line.
    fn sync_row(&mut self) {
        let line = self.output_pane.caret().line;
        if let Some(row) = self.data.model.row_of_output_line(line) {
            self.row = row;
            if let Some(section) = self.data.model.section_of_row(row) {
                self.current = section;
            }
        }
    }

    fn key(&mut self, key: egui::Key, modifiers: egui::Modifiers) -> bool {
        let extend = modifiers.shift;
        match key {
            egui::Key::ArrowLeft => self.output_pane.move_caret(Motion::Left, extend),
            egui::Key::ArrowRight => self.output_pane.move_caret(Motion::Right, extend),
            egui::Key::ArrowUp => self.output_pane.move_caret(Motion::Up, extend),
            egui::Key::ArrowDown => self.output_pane.move_caret(Motion::Down, extend),
            egui::Key::Home => self.output_pane.move_caret(Motion::LineStart, extend),
            egui::Key::End => self.output_pane.move_caret(Motion::LineEnd, extend),
            egui::Key::Enter => {
                let end = self.terminator().to_owned();
                self.output_pane.enter(&end);
                return true;
            }
            egui::Key::Backspace => {
                self.output_pane.backspace();
                return true;
            }
            egui::Key::Delete => {
                self.output_pane.delete();
                return true;
            }
            _ => {}
        }
        false
    }
}

fn hatch(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    let stroke = egui::Stroke::new(1.0, color);
    let mut x = rect.left();
    while x < rect.right() {
        painter.line_segment(
            [
                egui::pos2(x, rect.bottom()),
                egui::pos2(x + rect.height(), rect.top()),
            ],
            stroke,
        );
        x += HATCH_STEP;
    }
}

impl SessionView for MergeView {
    fn kind(&self) -> Option<ca_session::SessionKind> {
        Some(ca_session::SessionKind::TextMerge)
    }

    fn menu_view(&self) -> MenuView {
        MenuView::Merge
    }

    fn title(&self) -> String {
        let name = |path: &Path| {
            path.file_name().map_or_else(
                || path.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            )
        };
        match &self.paths.output {
            Some(output) => format!(
                "{} - {} - {}",
                name(&self.paths.left),
                name(&self.paths.right),
                name(output)
            ),
            None => format!("{} - {}", name(&self.paths.left), name(&self.paths.right)),
        }
    }

    fn tick(&mut self) {
        self.drain_jobs();
        self.poll_find();
    }

    fn ui(&mut self, ui: &mut egui::Ui, context: &ViewContext) -> Vec<ViewAction> {
        self.notify = Arc::clone(&context.notify);
        self.widgets.clear();
        self.window = ui.clip_rect();
        let palette = Self::palette(context);
        self.selection_color = context.palette.selection;
        self.follow_options(ui.ctx());
        self.report.poll(ui.ctx());
        self.handle_keys(ui);
        if let Some(text) = self.pending_clipboard.take() {
            ui.ctx().copy_text(text);
        }
        if std::mem::take(&mut self.paste_requested) {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::RequestPaste);
        }
        egui::TopBottomPanel::top(self.id.with("head")).show_inside(ui, |ui| {
            self.toolbar(ui, &palette);
            widgets::file_info_bar(
                ui,
                ca_ui::format::local_offset_seconds(),
                &self.file_info_lines(),
            );
            self.question_panel(ui);
            self.report_panel(ui);
            let request = self.find.show_find(ui);
            self.answer_panel(request);
            let request = self.find.show_go_to(ui);
            self.answer_panel(request);
        });
        egui::TopBottomPanel::bottom(self.id.with("status")).show_inside(ui, |ui| {
            self.status_bar(ui);
        });
        if self.show_thumbnail {
            egui::SidePanel::left(self.id.with("thumb"))
                .exact_width(THUMBNAIL_WIDTH)
                .resizable(false)
                .show_inside(ui, |ui| {
                    self.thumbnail_strip(ui, &palette);
                });
        }
        egui::CentralPanel::default().show_inside(ui, |ui| {
            self.comparison(ui, &palette);
        });
        std::mem::take(&mut self.pending)
    }

    fn commands(&self) -> Vec<ca_ui::view::CommandState> {
        ca_ui::view::declare(HANDLED, |command| SessionView::accepts(self, command))
    }

    fn accepts(&self, command: Command) -> bool {
        if self.editing_off() && CHANGES_OUTPUT.contains(&command) {
            return false;
        }
        let ready = self.status == Status::Ready;
        match command {
            Command::TakeLeft
            | Command::TakeRight
            | Command::TakeLeftThenRight
            | Command::TakeRightThenLeft
            | Command::TakeAllNonConflicting
            | Command::NextSection
            | Command::PreviousSection
            | Command::NextDifference
            | Command::PreviousDifference
            | Command::SelectAll
            | Command::SelectSection
            | Command::Copy
            | Command::Cut
            | Command::Paste
            | Command::MergeInfo
            | Command::CompareToOutput
            | Command::CompareReport
            | Command::TakeLeftLine
            | Command::TakeRightLine
            | Command::NextLeftTaken
            | Command::PreviousLeftTaken
            | Command::NextRightTaken
            | Command::PreviousRightTaken
            | Command::ToggleSectionIgnored
            | Command::Find
            | Command::Replace
            | Command::GoTo
            | Command::NextEdit
            | Command::PreviousEdit
            | Command::ShowAll
            | Command::ShowDifferences
            | Command::ShowConflicts
            | Command::ShowLeftChanges
            | Command::ShowRightChanges
            | Command::ShowMergeable
            | Command::ShowSame
            | Command::ShowNone
            | Command::ShowContext => ready,
            Command::ToggleConflict => ready && self.conflict_applies(),
            Command::FindNext | Command::FindPrevious => {
                ready && !self.find.settings.pattern.is_empty()
            }
            Command::TakeCenter | Command::TakeCenterLine | Command::ToggleCenterPane => {
                ready && !self.paths.is_two_way()
            }
            Command::ClearConflictNext => ready && self.data.model.totals().conflicts_remaining > 0,
            Command::NextConflict | Command::PreviousConflict => {
                ready && self.data.model.totals().conflicts > 0
            }
            Command::Undo => self.output_pane.buffer().can_undo(),
            Command::Redo => self.output_pane.buffer().can_redo(),
            Command::SaveFile => ready && self.paths.output.is_some() && self.save_job.is_none(),
            Command::Reload => !self.status.is_running(),
            Command::SwapSides => ready && self.save_job.is_none(),

            Command::Thumbnail
            | Command::ToggleLineNumbers
            | Command::IncreaseFontSize
            | Command::DecreaseFontSize
            | Command::ResetFontSize
            | Command::ToggleIgnoreUnimportant
            | Command::ToggleIgnoreSameChanges
            | Command::FavorLeft
            | Command::FavorRight => true,
            Command::Cancel => {
                self.status.is_running()
                    || self.find_task.is_running()
                    || self.find.is_find_open()
                    || self.find.is_go_to_open()
            }
            _ => false,
        }
    }

    fn run(&mut self, command: Command) {
        self.message = None;
        if self.editing_off() && CHANGES_OUTPUT.contains(&command) {
            self.message = Some(EDITING_OFF.to_owned());
            return;
        }
        match command {
            Command::CompareReport => self.report.request(),
            Command::TakeLeft => self.take(Resolution::Left),
            Command::TakeCenter => self.take(Resolution::Center),
            Command::TakeRight => self.take(Resolution::Right),
            Command::TakeLeftThenRight => self.take(Resolution::LeftThenRight),
            Command::TakeRightThenLeft => self.take(Resolution::RightThenLeft),
            Command::TakeAllNonConflicting => {
                let changed = self.data.model.take_all_non_conflicting();
                self.touched();
                self.replace_output_sections(&changed);
            }
            Command::FavorLeft => self.rules.favor_left = !self.rules.favor_left,
            Command::FavorRight => self.rules.favor_right = !self.rules.favor_right,
            Command::NextConflict => self.navigate_conflict(true),
            Command::PreviousConflict => self.navigate_conflict(false),
            Command::ClearConflictNext => {
                if self.data.model.clear_conflict(self.current) {
                    self.touched();
                }
                self.go_to_section(self.data.model.next_conflict(self.current));
            }
            Command::NextSection | Command::NextDifference => self.navigate_difference(true),
            Command::PreviousSection | Command::PreviousDifference => {
                self.navigate_difference(false);
            }
            Command::ToggleCenterPane => self.show_center = !self.show_center,
            Command::Thumbnail => self.show_thumbnail = !self.show_thumbnail,
            Command::ToggleLineNumbers => self.show_line_numbers = !self.show_line_numbers,
            Command::IncreaseFontSize | Command::DecreaseFontSize | Command::ResetFontSize => {
                self.font.run(command);
                self.strip_stale = true;
            }
            Command::SelectAll => self.output_pane.select_all(),
            Command::Copy => self.pending_clipboard = self.output_pane.copy(),
            Command::Cut => {
                self.pending_clipboard = self.output_pane.cut();
                self.absorb_output_edits();
            }
            Command::Paste => {
                self.focus = Pane::Output;
                self.paste_requested = true;
            }
            Command::Undo => {
                if self.output_pane.undo() {
                    self.absorb_output_edits();
                }
            }
            Command::Redo => {
                if self.output_pane.redo() {
                    self.absorb_output_edits();
                }
            }
            Command::SaveFile => self.save_output(),
            Command::Reload => self.start_merge(),
            Command::SwapSides => self.swap_sides(),
            Command::Cancel => {
                self.find_task.cancel();
                if let Some(job) = self.job.as_ref() {
                    job.cancel();
                }
                self.find.close();
            }
            Command::MergeInfo => {
                self.message = Some(self.status_fields().join("; "));
            }
            Command::CompareToOutput => {
                if let Some(output) = self.paths.output.clone() {
                    self.pending.push(ViewAction::Open(OpenRequest::new(
                        SessionKind::TextCompare,
                        self.paths.left.clone(),
                        output,
                    )));
                } else {
                    self.message = Some(NO_OUTPUT.to_owned());
                }
            }
            _ => self.run_review(command),
        }
    }

    fn is_ready(&self) -> bool {
        !self.status.is_running()
    }

    fn apply_settings(&mut self, settings: &ca_session::settings::SessionSettings) {
        let ca_session::settings::SessionSettings::TextMerge(merge) = settings else {
            return;
        };
        let mut merged = merge.clone();
        merged.specs.clone_from(&self.session_settings.specs);
        let merge_again = merged != self.session_settings;
        self.session_settings = merge.clone();
        self.output_pane.set_read_only(self.editing_off());
        if merge_again {
            self.start_merge();
        }
    }

    fn holds_unwritten_edits(&self) -> bool {
        self.is_modified()
    }

    fn settings(&self) -> Option<ca_session::settings::SessionSettings> {
        let mut settings = self.session_settings.clone();
        settings.specs =
            ca_ui::view::with_sides(&settings.specs, &self.paths.left, &self.paths.right);
        settings.specs.ancestor = self
            .paths
            .center
            .as_deref()
            .and_then(ca_ui::view::side_location);
        settings.specs.output = self
            .paths
            .output
            .as_deref()
            .and_then(ca_ui::view::side_location);
        Some(ca_session::settings::SessionSettings::TextMerge(settings))
    }

    fn notice(&self) -> Option<String> {
        match &self.status {
            Status::Failed(reason) => Some(reason.clone()),
            _ => self.message.clone(),
        }
    }

    fn wants_close(&self) -> bool {
        self.closing
    }

    fn is_busy(&self) -> bool {
        self.save_job.is_some()
    }

    fn on_close(&mut self) {
        if let Some(job) = self.job.take() {
            job.cancel();
        }
        if let Some(job) = self.save_job.take() {
            job.cancel();
        }
    }

    fn may_close(&mut self) -> bool {
        if matches!(self.question, Some(Question::KeptCopy(_))) {
            return false;
        }
        if self.save_job.is_some() {
            return false;
        }
        if !self.is_modified() || self.closing {
            return true;
        }
        self.question = Some(Question::CloseModified);
        false
    }

    fn exit_code(&self) -> Option<i32> {
        Some(output::exit_code(self.outcome()))
    }
}

impl MergeView {
    fn navigate_conflict(&mut self, forward: bool) {
        let current_matches = self
            .data
            .model
            .sections()
            .get(self.current)
            .is_some_and(crate::model::Section::is_unresolved_conflict);
        let target = if forward {
            self.data.model.next_conflict(self.current)
        } else {
            self.data.model.previous_conflict(self.current)
        };
        let target = self.initial_navigation_target(target, current_matches);
        self.go_to_section(target);
    }

    fn navigate_difference(&mut self, forward: bool) {
        let current_matches = self
            .data
            .model
            .sections()
            .get(self.current)
            .is_some_and(crate::model::Section::is_difference);
        let target = if forward {
            self.data.model.next_difference(self.current)
        } else {
            self.data.model.previous_difference(self.current)
        };
        let target = self.initial_navigation_target(target, current_matches);
        self.go_to_section(target);
    }

    /// The commands that review a merge line by line: single line takes,
    /// conflict and ignore marks, filters, taken runs and the find strips.
    fn run_review(&mut self, command: Command) {
        match command {
            Command::NextEdit => {
                self.go_to_section(self.data.model.next_edit(self.current));
            }
            Command::PreviousEdit => {
                self.go_to_section(self.data.model.previous_edit(self.current));
            }
            Command::TakeLeftLine => self.take_line(Pane::Left),
            Command::TakeCenterLine => self.take_line(Pane::Center),
            Command::TakeRightLine => self.take_line(Pane::Right),
            Command::NextLeftTaken => {
                self.go_to_section(self.data.model.next_taken(self.current, Pane::Left));
            }
            Command::PreviousLeftTaken => {
                self.go_to_section(self.data.model.previous_taken(self.current, Pane::Left));
            }
            Command::NextRightTaken => {
                self.go_to_section(self.data.model.next_taken(self.current, Pane::Right));
            }
            Command::PreviousRightTaken => {
                self.go_to_section(self.data.model.previous_taken(self.current, Pane::Right));
            }
            Command::ToggleConflict => self.toggle_conflict(),
            Command::ToggleSectionIgnored => self.toggle_ignored(),
            Command::ToggleIgnoreUnimportant => {
                let mut rules = self.rules;
                rules.ignore_unimportant = !rules.ignore_unimportant;
                self.set_rules(rules);
            }
            Command::ToggleIgnoreSameChanges => {
                let mut rules = self.rules;
                rules.ignore_same_changes = !rules.ignore_same_changes;
                self.set_rules(rules);
            }
            Command::ShowAll
            | Command::ShowDifferences
            | Command::ShowConflicts
            | Command::ShowLeftChanges
            | Command::ShowRightChanges
            | Command::ShowMergeable
            | Command::ShowSame
            | Command::ShowNone
            | Command::ShowContext => {
                if let Some(filter) = MergeFilter::from_command(command, self.context_lines) {
                    self.set_filter(filter);
                }
            }
            Command::Find => self.find.open_find(),
            Command::Replace => self.find.open_replace(),
            Command::GoTo => self.find.open_go_to(),
            Command::FindNext => self.answer_panel(Some(PanelRequest::Next)),
            Command::FindPrevious => self.answer_panel(Some(PanelRequest::Previous)),
            _ => {}
        }
    }
}

impl ca_ui::view::ViewFactory for MergeView {
    fn create(left: PathBuf, right: PathBuf, context: &ViewContext, salt: u64) -> Self {
        Self::new(left, right, context, salt)
    }

    fn create_from(request: &OpenRequest, context: &ViewContext, salt: u64) -> Self {
        Self::from_request(request, context, salt)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::{MergeView, Question};
    use ca_ui::command::Command;
    use ca_ui::testing::{context, raw_input};
    use ca_ui::view::SessionView;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn open(left: &str, center: Option<&str>, right: &str) -> (MergeView, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, text: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, text.as_bytes()).unwrap();
            path
        };
        let paths = super::MergePaths {
            left: write("left.txt", left),
            center: center.map(|text| write("center.txt", text)),
            right: write("right.txt", right),
            output: Some(dir.path().join("out.txt")),
        };
        let view = MergeView::over(paths, ca_ui::view::Titles::default(), &context(), 7);
        (view, dir)
    }

    fn open_bytes(
        left: &[u8],
        center: Option<&[u8]>,
        right: &[u8],
    ) -> (MergeView, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, bytes: &[u8]| {
            let path = dir.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            path
        };
        let paths = super::MergePaths {
            left: write("left.txt", left),
            center: center.map(|bytes| write("center.txt", bytes)),
            right: write("right.txt", right),
            output: Some(dir.path().join("out.txt")),
        };
        let view = MergeView::over(paths, ca_ui::view::Titles::default(), &context(), 7);
        (view, dir)
    }

    fn run_until_ready(view: &mut MergeView) {
        let ctx = egui::Context::default();
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            view.tick();
            let _ = ctx.run(raw_input(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    view.ui(ui, &context());
                });
            });
            if view.is_ready() {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("the merge never became ready");
    }

    fn finish_search(view: &mut MergeView) {
        assert!(ca_ui::testing::wait_until(Duration::from_secs(20), || {
            view.tick();
            !view.find_task.is_running()
        }));
    }

    fn click_at(
        view: &mut MergeView,
        ctx: &egui::Context,
        held: &ca_ui::view::ViewContext,
        pos: egui::Pos2,
        pressed: bool,
    ) {
        view.tick();
        let mut input = raw_input();
        input.events = vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            },
        ];
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| view.ui(ui, held));
        });
    }

    #[test]
    fn a_merge_of_three_files_paints_a_frame_and_names_its_totals() {
        let (mut view, _dir) = open("a\nL\nc\n", Some("a\nb\nc\n"), "a\nR\nc\n");
        run_until_ready(&mut view);
        assert_eq!(view.model().totals().conflicts, 1);
        assert!(view
            .status_fields()
            .iter()
            .any(|field| field.contains("conflict(s) remaining")));
        assert!(view.pane_rect(super::Pane::Output).is_some());
    }

    #[test]
    fn taking_a_side_changes_the_output_and_clears_the_conflict() {
        let (mut view, _dir) = open("a\nL\nc\n", Some("a\nb\nc\n"), "a\nR\nc\n");
        run_until_ready(&mut view);
        view.run(Command::NextConflict);
        view.run(Command::TakeRight);
        assert_eq!(view.output_text(), "a\nR\nc\n");
        assert_eq!(view.model().totals().conflicts_remaining, 0);
    }

    fn assert_output_lines_match_pane(view: &MergeView) {
        let pane = view.output_pane.buffer().text();
        let lines = ca_diff::split_lines(&pane);
        assert_eq!(view.output_text(), pane);
        assert_eq!(
            view.model()
                .output_lines()
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            lines,
            "model lines must be the pane's actual lines"
        );
    }

    fn assert_saved_pane(view: &mut MergeView, dir: &tempfile::TempDir) {
        // Suppression saves the pane without explicitly requested conflict markers.
        for section in 0..view.model().sections().len() {
            view.data.model.clear_conflict(section);
        }
        let expected = view.output_pane.buffer().text();
        view.save_output();
        assert!(ca_ui::testing::wait_until(Duration::from_secs(20), || {
            view.drain_save();
            !view.is_saving()
        }));
        assert_eq!(
            std::fs::read(dir.path().join("out.txt")).unwrap(),
            expected.as_bytes()
        );
    }

    #[test]
    fn merge_boundary_load_separates_an_unterminated_line_from_an_added_line() {
        for ending in ["\n", "\r\n", "\r"] {
            let base = format!("a{ending}b");
            let right = format!("a{ending}b{ending}c{ending}");
            let (mut view, dir) = open(&base, Some(&base), &right);
            run_until_ready(&mut view);
            assert_eq!(view.output_pane.buffer().text(), right);
            assert_output_lines_match_pane(&view);
            assert_saved_pane(&mut view, &dir);
            view.current = view.model().sections().len() - 1;
            view.run(Command::TakeCenter);
            assert_eq!(view.output_pane.buffer().text(), base);
            assert_output_lines_match_pane(&view);
            assert_saved_pane(&mut view, &dir);
        }
    }

    #[test]
    fn merge_boundary_load_keeps_a_lf_blank_line_after_a_lone_cr() {
        let (mut view, dir) = open("k\rm\r", Some("k\rm\r"), "k\n\nm\n");
        run_until_ready(&mut view);
        assert_eq!(view.output_pane.buffer().text(), "k\r\n\nm\r");
        assert_output_lines_match_pane(&view);
        assert_saved_pane(&mut view, &dir);
    }

    #[test]
    fn merge_boundary_take_both_then_edit_and_history_keep_every_line() {
        let (mut view, dir) = open("k\rx\rm\r", Some("k\rc\rm\r"), "k\n\nm\n");
        run_until_ready(&mut view);
        view.run(Command::NextConflict);
        view.run(Command::TakeRight);
        assert_eq!(view.output_pane.buffer().text(), "k\r\n\nm\r");
        assert_output_lines_match_pane(&view);
        view.run(Command::TakeLeftThenRight);
        let taken = "k\rx\r\n\nm\r";
        assert_eq!(view.output_pane.buffer().text(), taken);
        assert_output_lines_match_pane(&view);
        assert_saved_pane(&mut view, &dir);
        view.output_pane
            .place(ca_ui::editor::Caret::new(3, 0), false);
        view.output_pane.type_character('Z');
        view.absorb_output_edits();
        assert_eq!(view.output_text(), "k\rx\r\n\nZm\r");
        assert_output_lines_match_pane(&view);
        assert_saved_pane(&mut view, &dir);
        view.run(Command::Undo);
        assert_eq!(view.output_text(), taken);
        assert_output_lines_match_pane(&view);
        view.run(Command::Redo);
        assert_output_lines_match_pane(&view);
        assert_saved_pane(&mut view, &dir);
    }

    #[test]
    fn merge_boundary_take_line_repairs_both_section_seams() {
        let (mut view, dir) = open("k\rx\ry\rm\r", Some("k\rc\rd\rm\r"), "k\np\n\nm\n");
        run_until_ready(&mut view);
        view.run(Command::NextConflict);
        view.run(Command::TakeRight);
        view.row = view.model().row_of_section(view.current).unwrap();
        view.run(Command::TakeLeftLine);
        assert_eq!(view.output_pane.buffer().text(), "k\rx\r\n\nm\r");
        assert_output_lines_match_pane(&view);
        assert_saved_pane(&mut view, &dir);
        view.run(Command::Undo);
        assert_output_lines_match_pane(&view);
        assert_saved_pane(&mut view, &dir);
        view.run(Command::Redo);
        assert_output_lines_match_pane(&view);
    }

    #[test]
    fn save_uses_the_pane_snapshot_even_before_the_model_absorbs_an_edit() {
        let (mut view, dir) = open("a\rb\r", Some("a\rb\r"), "a\rb\r");
        run_until_ready(&mut view);
        view.output_pane.type_character('Z');
        assert_saved_pane(&mut view, &dir);
        assert!(
            !view.is_modified(),
            "the saved pane revision is the save point"
        );
    }

    #[test]
    fn randomized_merges_save_the_pane_in_every_line_ending_style() {
        use std::fmt::Write as _;
        struct Random(u64);
        impl Random {
            fn pick(&mut self, limit: usize) -> usize {
                self.0 ^= self.0 << 13;
                self.0 ^= self.0 >> 7;
                self.0 ^= self.0 << 17;
                usize::try_from(self.0 % u64::try_from(limit).unwrap()).unwrap()
            }
            fn text(&mut self, style: usize, side: usize) -> String {
                let mut text = String::new();
                for line in 0..6 {
                    if self.pick(5) == 0 {
                        continue;
                    }
                    if self.pick(4) != 0 {
                        write!(text, "line {line}").unwrap();
                        if self.pick(3) == 0 {
                            write!(text, " side {side}").unwrap();
                        }
                    }
                    let ending =
                        ["\n", "\r\n", "\r"][if style == 3 { self.pick(3) } else { style }];
                    text.push_str(ending);
                }
                if self.pick(2) == 0 {
                    text.push_str("unterminated");
                }
                text
            }
        }
        let mut random = Random(0x6d65_7267_6520_454f);
        for case in 0..64 {
            let style = case % 4;
            let left = random.text(style, 0);
            let center = random.text(style, 1);
            let right = random.text(style, 2);
            let (mut view, dir) = open(&left, Some(&center), &right);
            run_until_ready(&mut view);
            for step in 0..17 {
                assert_output_lines_match_pane(&view);
                assert_saved_pane(&mut view, &dir);
                if step == 16 {
                    break;
                }
                let section = random.pick(view.model().sections().len());
                view.current = section;
                view.row = view.model().row_of_section(section).unwrap_or(0);
                view.output_pane.clear_selection();
                match random.pick(12) {
                    0 => view.run(Command::TakeLeft),
                    1 => view.run(Command::TakeCenter),
                    2 => view.run(Command::TakeRight),
                    3 => view.run(Command::TakeLeftThenRight),
                    4 => view.run(Command::TakeRightThenLeft),
                    5 => view.run(Command::TakeLeftLine),
                    6 => view.run(Command::TakeRightLine),
                    7 => view.run(Command::Undo),
                    8 => view.run(Command::Redo),
                    action => {
                        let line =
                            u32::try_from(random.pick(view.output_pane.line_count() as usize))
                                .unwrap();
                        view.output_pane
                            .place(ca_ui::editor::Caret::new(line, 0), false);
                        match action {
                            9 => view.output_pane.type_character('Z'),
                            10 => {
                                let ending = view.terminator();
                                view.output_pane.enter(ending);
                            }
                            _ => {
                                view.output_pane.backspace();
                            }
                        }
                        view.absorb_output_edits();
                    }
                }
            }
        }
    }

    #[test]
    fn modified_check_does_not_rebuild_the_output_text() {
        let (mut view, _dir) = open("a\nL\nc\n", Some("a\nb\nc\n"), "a\nR\nc\n");
        run_until_ready(&mut view);
        let calls = crate::model::MergeModel::output_text_call_count();

        assert!(!view.is_modified());
        assert_eq!(crate::model::MergeModel::output_text_call_count(), calls);

        view.run(Command::NextConflict);
        view.run(Command::TakeRight);

        assert_eq!(view.output_pane.buffer().text(), "a\nR\nc\n");
        assert!(view.is_modified());
        assert_eq!(crate::model::MergeModel::output_text_call_count(), calls);
    }

    #[test]
    fn typing_in_a_large_unchanged_merge_section_updates_one_output_line() {
        use std::fmt::Write as _;
        let mut shared = String::with_capacity(20_000 * 16);
        for index in 0..20_000 {
            let _ = writeln!(shared, "shared {index}");
        }
        let left = format!("left\n{shared}");
        let center = format!("base\n{shared}");
        let right = format!("right\n{shared}");
        let (mut view, _dir) = open(&left, Some(&center), &right);
        run_until_ready(&mut view);
        let section = view
            .model()
            .sections()
            .iter()
            .position(|section| section.kind == ca_diff::merge3::MergeKind::Unchanged)
            .unwrap();
        let output_start = view.model().output_range(section).unwrap().start;
        let edited_line = output_start + 10_000;
        crate::model::MergeModel::reset_rebuild_visit_counts();

        view.output_pane
            .place(ca_ui::editor::Caret::new(edited_line, 0), false);
        view.output_pane.type_character('X');
        view.absorb_output_edits();

        assert_eq!(
            view.model().output_lines()[edited_line as usize],
            "Xshared 10000\n"
        );
        assert_eq!(crate::model::MergeModel::edited_output_read_count(), 0);
        assert!(view.is_modified());

        view.output_pane
            .place(ca_ui::editor::Caret::new(edited_line, 1), false);
        view.output_pane.enter("\n");
        crate::model::MergeModel::reset_rebuild_visit_counts();
        view.absorb_output_edits();

        assert_eq!(view.model().output_lines()[edited_line as usize], "X\n");
        assert_eq!(
            view.model().output_lines()[(edited_line + 1) as usize],
            "shared 10000\n"
        );
        assert_eq!(crate::model::MergeModel::edited_output_read_count(), 0);
    }

    #[test]
    fn take_left_applies_to_every_section_touched_by_the_output_selection() {
        let shared = (0..12)
            .map(|index| format!("shared {index}\n"))
            .collect::<Vec<_>>()
            .concat();
        let left = format!("a\nL1\n{shared}L2\n");
        let center = format!("a\nb1\n{shared}b2\n");
        let right = format!("a\nR1\n{shared}R2\n");
        let (mut view, _dir) = open(&left, Some(&center), &right);
        run_until_ready(&mut view);
        assert_eq!(view.model().totals().conflicts_remaining, 2);

        view.run(Command::SelectAll);
        view.run(Command::TakeLeft);

        assert_eq!(view.output_text(), left);
        assert_eq!(view.model().totals().conflicts_remaining, 0);
    }

    #[test]
    fn first_navigation_reaches_a_conflict_in_section_zero() {
        let (mut view, _dir) = open("L\nc\n", Some("b\nc\n"), "R\nc\n");
        run_until_ready(&mut view);
        assert!(view
            .model()
            .sections()
            .first()
            .is_some_and(crate::model::Section::is_unresolved_conflict));

        view.run(Command::NextConflict);

        assert_eq!(view.current_section(), 0);
        assert_ne!(view.notice().as_deref(), Some("No further section"));
    }

    #[test]
    fn saving_with_a_conflict_asks_before_it_writes_markers() {
        let (mut view, _dir) = open("a\nL\nc\n", Some("a\nb\nc\n"), "a\nR\nc\n");
        run_until_ready(&mut view);
        view.run(Command::SaveFile);
        assert_eq!(view.question(), Some(&Question::SaveWithConflicts));
    }

    #[test]
    fn typing_after_a_trailing_line_ending_updates_and_saves_the_output() {
        let (mut view, dir) = open("a\nL\nc\n", Some("a\nb\nc\n"), "a\nb\nc\n");
        run_until_ready(&mut view);
        view.output_pane
            .place(ca_ui::editor::Caret::new(3, 0), false);
        view.output_pane.type_character('X');
        view.absorb_output_edits();

        let expected = "a\nL\nc\nX";
        assert_eq!(view.output_text(), expected);
        assert_eq!(view.output_pane.buffer().text(), expected);
        view.save_output();
        let deadline = Instant::now() + Duration::from_secs(20);
        while view.is_saving() && Instant::now() < deadline {
            view.drain_save();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(!view.is_saving());
        assert_eq!(
            std::fs::read(dir.path().join("out.txt")).unwrap(),
            expected.as_bytes()
        );
    }

    #[test]
    fn typing_into_an_empty_merge_updates_and_saves_the_output() {
        let (mut view, dir) = open("", Some(""), "");
        run_until_ready(&mut view);
        view.output_pane
            .place(ca_ui::editor::Caret::new(0, 0), false);
        view.output_pane.type_character('X');
        view.absorb_output_edits();

        assert_eq!(view.output_text(), "X");
        assert_eq!(view.output_pane.buffer().text(), "X");
        view.save_output();
        let deadline = Instant::now() + Duration::from_secs(20);
        while view.is_saving() && Instant::now() < deadline {
            view.drain_save();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(!view.is_saving());
        assert_eq!(std::fs::read(dir.path().join("out.txt")).unwrap(), b"X");
    }

    #[test]
    fn edits_remain_modified_when_a_reload_fails() {
        let (mut view, dir) = open("a\nL\nc\n", Some("a\nb\nc\n"), "a\nR\nc\n");
        run_until_ready(&mut view);
        view.run(Command::NextConflict);
        view.run(Command::TakeLeft);
        assert!(view.is_modified());
        std::fs::remove_file(dir.path().join("right.txt")).unwrap();
        view.run(Command::Reload);

        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline && !matches!(view.status, super::Status::Failed(_)) {
            view.tick();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(matches!(view.status, super::Status::Failed(_)));
        assert!(view.is_modified());
        assert!(!view.may_close());
        assert_eq!(view.question(), Some(&Question::CloseModified));
    }

    #[test]
    fn clear_conflict_next_keeps_two_way_output_and_clears_only_its_status() {
        let (mut view, _dir) = open("a\nL\nc\n", None, "a\nR\nc\n");
        run_until_ready(&mut view);
        view.run(Command::NextConflict);
        let before = view.output_text();
        assert_eq!(view.model().totals().conflicts_remaining, 1);

        view.run(Command::ClearConflictNext);

        assert_eq!(view.output_text(), before);
        assert_eq!(view.model().totals().conflicts_remaining, 0);
    }

    #[test]
    fn clear_conflict_next_does_not_replace_a_taken_nonconflict_section() {
        let (mut view, _dir) = open(
            "A\np\nq\nr\ns\nL1\nL2\n",
            Some("a\np\nq\nr\ns\nb1\nb2\n"),
            "a\np\nq\nr\ns\nR1\nR2\n",
        );
        run_until_ready(&mut view);
        view.run(Command::TakeLeft);
        let before = view.output_text();
        assert_eq!(view.model().totals().conflicts_remaining, 1);

        view.run(Command::ClearConflictNext);

        assert_eq!(view.output_text(), before);
        assert_eq!(view.model().totals().conflicts_remaining, 1);
    }

    #[test]
    fn saving_output_that_contains_a_decode_replacement_asks_first() {
        let utf16 = |text: &str| {
            let mut bytes = vec![0xff, 0xfe];
            for unit in text.encode_utf16() {
                bytes.extend_from_slice(&unit.to_le_bytes());
            }
            bytes
        };
        let normal = "a\nb\nc\nd\ne\nf\ng\n";
        let mut malformed = utf16(normal);
        malformed.push(0x21);
        let (mut view, dir) = open_bytes(
            &malformed,
            Some(&utf16(normal)),
            &utf16("A\nb\nc\nd\ne\nf\ng\n"),
        );
        run_until_ready(&mut view);
        assert!(view.data.sources.left.had_errors);
        assert!(view.output_text().contains('\u{fffd}'));

        view.run(Command::SaveFile);

        assert!(matches!(view.question(), Some(Question::AcceptLoss(_))));
        assert!(!dir.path().join("out.txt").exists());
    }

    #[test]
    fn a_gutter_take_button_receives_its_click() {
        let (mut view, _dir) = open("a\nL\nc\n", Some("a\nb\nc\n"), "a\nR\nc\n");
        run_until_ready(&mut view);
        let ctx = egui::Context::default();
        let held = context();
        let _ = ctx.run(raw_input(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| view.ui(ui, &held));
        });
        let conflict_row = view
            .model()
            .rows()
            .iter()
            .position(|row| view.model().sections()[row.section as usize].is_unresolved_conflict())
            .unwrap();
        let visible = view.visible.position_of(conflict_row).unwrap();
        let output = view.pane_rect(super::Pane::Output).unwrap();
        let row_height = view.row_height();
        let y = output.top() + row_height * (1.5 + super::as_f32(visible));
        let pos = egui::pos2(output.left() + super::ARROW_WIDTH / 4.0, y);

        click_at(&mut view, &ctx, &held, pos, true);
        click_at(&mut view, &ctx, &held, pos, false);

        assert_eq!(view.model().totals().conflicts_remaining, 0);
        assert_eq!(view.output_text(), "a\nL\nc\n");
    }

    #[test]
    fn edits_during_save_remain_unsaved() {
        let (mut view, dir) = open("a\nL\nc\n", Some("a\nb\nc\n"), "a\nR\nc\n");
        run_until_ready(&mut view);
        view.run(Command::NextConflict);
        view.run(Command::TakeLeft);
        view.save_output();
        view.run(Command::TakeRight);
        let deadline = Instant::now() + Duration::from_secs(20);
        while view.is_saving() && Instant::now() < deadline {
            view.drain_save();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(!view.is_saving());
        assert_eq!(
            std::fs::read(dir.path().join("out.txt")).unwrap(),
            b"a\nL\nc\n"
        );
        assert_eq!(view.output_text(), "a\nR\nc\n");
        assert!(view.is_modified());
    }

    #[test]
    fn disk_change_consent_expires_when_the_accepted_save_fails() {
        let (mut view, _dir) = open("a\nL\nc\n", Some("a\nb\nc\n"), "a\nR\nc\n");
        run_until_ready(&mut view);
        view.consent.accept_disk_change = true;
        view.finish_save(ca_ui::save::SaveOutcome::NotWritable);
        assert!(!view.consent.accept_disk_change);

        view.finish_save(ca_ui::save::SaveOutcome::ChangedOnDisk);

        assert_eq!(view.question(), Some(&Question::OverwriteChanged));
    }

    #[test]
    fn exit_code_tracks_conflicts_in_the_last_successful_save() {
        let (mut view, _dir) = open("a\nL\nc\n", Some("a\nb\nc\n"), "a\nR\nc\n");
        run_until_ready(&mut view);
        view.run(Command::SaveFile);
        assert_eq!(view.question(), Some(&Question::SaveWithConflicts));
        view.answer(true);
        let deadline = Instant::now() + Duration::from_secs(20);
        while view.is_saving() && Instant::now() < deadline {
            view.drain_save();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(!view.is_saving());

        view.run(Command::NextConflict);
        view.run(Command::TakeLeft);

        assert_eq!(view.exit_code(), Some(14));
    }

    #[test]
    fn a_save_in_flight_keeps_the_merge_tab_open_until_its_result_arrives() {
        let (mut view, _dir) = open("a\nL\nc\n", Some("a\nb\nc\n"), "a\nR\nc\n");
        run_until_ready(&mut view);
        view.run(Command::NextConflict);
        view.run(Command::TakeLeft);

        let (release, wait) = std::sync::mpsc::channel();
        view.save_job = Some(ca_ui::save::spawn_with(Arc::new(|| {}), move || {
            wait.recv().unwrap();
            ca_ui::save::SaveOutcome::NotWritable
        }));

        assert!(SessionView::is_busy(&view));
        assert!(!view.may_close());
        assert_eq!(view.question(), None);

        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while SessionView::is_busy(&view) && Instant::now() < deadline {
            view.tick();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(!SessionView::is_busy(&view));
        assert!(!view.may_close());
        assert_eq!(view.question(), Some(&Question::CloseModified));
    }

    #[test]
    fn a_view_with_two_paths_merges_without_an_ancestor() {
        let context = context();
        let view = MergeView::new(
            PathBuf::from("left.txt"),
            PathBuf::from("right.txt"),
            &context,
            1,
        );
        assert!(view.paths().is_two_way());
        assert!(!view.shows_center());
        assert_eq!(view.title(), "left.txt - right.txt");
    }

    #[test]
    fn every_declared_command_is_answered_or_refused_without_a_panic() {
        let (mut view, _dir) = open("a\nL\nc\n", Some("a\nb\nc\n"), "a\nR\nc\n");
        run_until_ready(&mut view);
        assert_eq!(view.model().totals().conflicts_remaining, 1);
        let commands = view.commands();
        assert!(commands
            .iter()
            .any(|state| state.command == Command::TakeLeft && state.enabled));
        for state in commands {
            if state.enabled && !matches!(state.command, Command::Reload | Command::SwapSides) {
                view.run(state.command);
                if view.question().is_some() {
                    view.answer(false);
                }
            }
        }
        assert!(view.is_ready());
        assert_eq!(view.model().totals().conflicts_remaining, 0);
    }

    /// A left change far from a two line conflict, with unchanged lines
    /// between them.
    const LEFT: &str = "A\np\nq\nr\ns\nL1\nL2\n";
    const CENTER: &str = "a\np\nq\nr\ns\nb1\nb2\n";
    const RIGHT: &str = "a\np\nq\nr\ns\nR1\nR2\n";

    fn frame(view: &mut MergeView) {
        let ctx = egui::Context::default();
        let _ = ctx.run(raw_input(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                view.ui(ui, &context());
            });
        });
    }

    fn conflict_rows(view: &MergeView) -> std::ops::Range<usize> {
        let section = view
            .model()
            .sections()
            .iter()
            .position(super::model::Section::is_unresolved_conflict)
            .unwrap();
        view.model().section_row_range(section).unwrap()
    }

    #[test]
    fn the_merge_view_carries_the_merge_menu_bar() {
        let (view, _dir) = open("a\n", Some("a\n"), "a\n");
        assert_eq!(view.menu_view(), ca_ui::command::MenuView::Merge);
    }

    #[test]
    fn a_display_filter_command_shows_only_the_lines_it_keeps() {
        let (mut view, _dir) = open(LEFT, Some(CENTER), RIGHT);
        run_until_ready(&mut view);
        let all = view.visible_rows().len();
        view.run(Command::ShowConflicts);
        assert_eq!(view.filter(), super::MergeFilter::Conflicts);
        assert_eq!(view.visible_rows().len(), conflict_rows(&view).len());
        view.run(Command::ShowMergeable);
        assert_eq!(view.visible_rows().len(), 1);
        view.run(Command::ShowSame);
        assert_eq!(view.visible_rows().len(), 4);
        view.run(Command::ShowNone);
        assert!(view.visible_rows().is_empty());
        frame(&mut view);
        view.run(Command::ShowContext);
        assert!(matches!(view.filter(), super::MergeFilter::Context(_)));
        view.run(Command::ShowAll);
        assert_eq!(view.visible_rows().len(), all);
        frame(&mut view);
    }

    #[test]
    fn a_filter_follows_a_take_that_resolves_a_conflict() {
        let (mut view, _dir) = open(LEFT, Some(CENTER), RIGHT);
        run_until_ready(&mut view);
        view.run(Command::ShowConflicts);
        view.run(Command::NextConflict);
        view.run(Command::TakeRight);
        assert!(view.visible_rows().is_empty());
    }

    #[test]
    fn take_line_commands_replace_only_the_current_line() {
        let (mut view, _dir) = open(LEFT, Some(CENTER), RIGHT);
        run_until_ready(&mut view);
        let rows = conflict_rows(&view);
        view.place_on_row(rows.start + 1, super::Pane::Output);
        view.run(Command::TakeLeftLine);
        assert_eq!(view.output_text(), "A\np\nq\nr\ns\nb1\nL2\n");
        view.run(Command::TakeRightLine);
        assert_eq!(view.output_text(), "A\np\nq\nr\ns\nb1\nR2\n");
        view.run(Command::TakeCenterLine);
        assert_eq!(view.output_text(), "A\np\nq\nr\ns\nb1\nb2\n");
        let section = view.current_section();
        assert_eq!(
            view.model().sections()[section].resolution,
            super::Resolution::Edited
        );
    }

    #[test]
    fn taking_a_line_from_a_shorter_side_updates_only_the_output_range() {
        let (mut view, _dir) = open("a\nL1\nL2\nc\n", Some("a\nb1\nb2\nc\n"), "a\nR1\nc\n");
        run_until_ready(&mut view);
        let rows = conflict_rows(&view);
        view.place_on_row(rows.start + 1, super::Pane::Output);

        view.run(Command::TakeRightLine);

        assert_eq!(view.output_text(), "a\nb1\nc\n");
        assert_eq!(view.output_pane.buffer().text(), view.output_text());
        assert!(view.is_modified());
    }

    #[test]
    fn the_conflict_command_marks_and_clears_the_current_section() {
        let (mut view, _dir) = open(LEFT, Some(CENTER), RIGHT);
        run_until_ready(&mut view);
        view.place_on_row(0, super::Pane::Left);
        view.run(Command::ToggleConflict);
        assert_eq!(view.model().totals().conflicts_remaining, 2);
        view.run(Command::ToggleConflict);
        assert_eq!(view.model().totals().conflicts_remaining, 1);
        view.place_on_row(1, super::Pane::Left);
        assert!(
            !view.accepts(Command::ToggleConflict),
            "an unchanged line has nothing to review"
        );
    }

    #[test]
    fn the_conflict_command_acts_on_every_section_the_selection_touches() {
        let (mut view, _dir) = open(LEFT, Some(CENTER), RIGHT);
        run_until_ready(&mut view);
        view.place_on_row(0, super::Pane::Output);
        view.run(Command::SelectAll);
        view.run(Command::ToggleConflict);
        assert_eq!(view.model().totals().conflicts_remaining, 2);
        let unchanged = &view.model().sections()[1];
        assert!(!unchanged.conflict, "an unchanged section was marked");
    }

    #[test]
    fn the_ignore_commands_change_what_counts_as_a_difference() {
        let (mut view, _dir) = open(
            "S\np\nq\nr\ns\nx\n",
            Some("a\np\nq\nr\ns\nx\n"),
            "S\np\nq\nr\ns\nx\n",
        );
        run_until_ready(&mut view);
        assert_eq!(view.model().totals().differences, 1);
        view.run(Command::ToggleIgnoreSameChanges);
        assert!(view.rules().ignore_same_changes);
        assert_eq!(view.model().totals().differences, 0);
        view.run(Command::ToggleIgnoreSameChanges);
        view.place_on_row(0, super::Pane::Left);
        view.run(Command::ToggleSectionIgnored);
        assert_eq!(view.model().totals().differences, 0);
        view.run(Command::ToggleSectionIgnored);
        assert_eq!(view.model().totals().differences, 1);
    }

    #[test]
    fn ignore_unimportant_reads_the_session_importance_rules() {
        let (mut view, _dir) = open(
            "a  \np\nq\nr\ns\nx\n",
            Some("a\np\nq\nr\ns\nx\n"),
            "a\np\nq\nr\ns\nx\n",
        );
        let mut settings = ca_session::settings::TextMergeSettings::default();
        settings.importance.trailing_whitespace_important = false;
        view.apply_settings(&ca_session::settings::SessionSettings::TextMerge(settings));
        run_until_ready(&mut view);
        assert_eq!(view.model().totals().differences, 1);
        view.run(Command::ToggleIgnoreUnimportant);
        assert_eq!(view.model().totals().differences, 0);
        view.run(Command::ToggleIgnoreUnimportant);
        assert_eq!(view.model().totals().differences, 1);
    }

    #[test]
    fn find_next_selects_a_match_in_the_output_and_moves_the_current_row() {
        let (mut view, _dir) = open(LEFT, Some(CENTER), RIGHT);
        run_until_ready(&mut view);
        view.run(Command::Find);
        assert!(view.find_panel().is_find_open());
        view.find_panel().settings.pattern = "b2".to_owned();
        view.run(Command::FindNext);
        assert!(view.output_pane().selection().is_none());
        finish_search(&mut view);
        assert_eq!(view.output_pane().selected_text().as_deref(), Some("b2"));
        assert_eq!(view.current_row(), conflict_rows(&view).start + 1);
        view.run(Command::FindPrevious);
        finish_search(&mut view);
        assert_eq!(view.output_pane().selected_text().as_deref(), Some("b2"));
        view.run(Command::Cancel);
        assert!(!view.find_panel().is_find_open());
    }

    #[test]
    fn replace_all_leaves_output_unchanged_until_the_worker_is_polled() {
        let (mut view, _dir) = open(LEFT, Some(CENTER), RIGHT);
        run_until_ready(&mut view);
        view.find_panel().settings.pattern = "b".to_owned();
        view.find_panel().settings.replacement = "B".to_owned();
        let before = view.output_text();
        view.answer_panel(Some(super::PanelRequest::ReplaceAll));
        assert_eq!(view.output_text(), before);
        finish_search(&mut view);
        assert_eq!(view.output_text(), "A\np\nq\nr\ns\nB1\nB2\n");
    }

    #[test]
    fn replace_all_in_the_output_edits_the_sections_it_reaches() {
        let (mut view, _dir) = open(LEFT, Some(CENTER), RIGHT);
        run_until_ready(&mut view);
        view.run(Command::Replace);
        view.find_panel().settings.pattern = "b".to_owned();
        view.find_panel().settings.replacement = "B".to_owned();
        view.answer_panel(Some(super::PanelRequest::ReplaceAll));
        finish_search(&mut view);
        assert_eq!(view.output_text(), "A\np\nq\nr\ns\nB1\nB2\n");
        assert_eq!(view.model().totals().conflicts_remaining, 0);
        view.run(Command::Undo);
        assert_eq!(view.output_text(), "A\np\nq\nr\ns\nb1\nb2\n");
    }

    #[test]
    fn go_to_places_the_caret_on_the_output_line() {
        let (mut view, _dir) = open(LEFT, Some(CENTER), RIGHT);
        run_until_ready(&mut view);
        view.run(Command::GoTo);
        view.find_panel().set_go_to_text("7");
        let request = view.find_panel().go_to_request();
        view.answer_panel(Some(request));
        assert_eq!(view.output_pane().caret().line, 6);
        assert_eq!(view.current_row(), conflict_rows(&view).start + 1);
        view.find_panel().set_go_to_text("seven");
        let request = view.find_panel().go_to_request();
        view.answer_panel(Some(request));
        assert!(view.notice().is_some());
    }

    #[test]
    fn taken_navigation_steps_between_runs_of_one_side() {
        let (mut view, _dir) = open(LEFT, Some(CENTER), RIGHT);
        run_until_ready(&mut view);
        view.run(Command::NextConflict);
        view.run(Command::TakeLeft);
        view.place_on_row(0, super::Pane::Output);
        view.run(Command::NextLeftTaken);
        let conflict = view.current_section();
        assert!(view.model().sections()[conflict].resolution.holds_left());
        assert!(conflict > 0);
        view.run(Command::PreviousLeftTaken);
        assert_eq!(view.current_section(), 0);
        view.run(Command::NextRightTaken);
        assert_eq!(view.current_section(), 0, "no right run exists");
        assert!(view.notice().is_some());
    }

    #[test]
    fn edit_navigation_steps_between_sections_that_were_typed_into() {
        let (mut view, _dir) = open(LEFT, Some(CENTER), RIGHT);
        run_until_ready(&mut view);
        let rows = conflict_rows(&view);
        view.place_on_row(rows.start, super::Pane::Output);
        view.run(Command::TakeLeftLine);
        let edited = view.current_section();
        view.place_on_row(0, super::Pane::Output);
        view.run(Command::NextEdit);
        assert_eq!(view.current_section(), edited);
        view.run(Command::PreviousEdit);
        assert_eq!(view.current_section(), edited, "no earlier edit exists");
        assert!(view.notice().is_some());
    }

    #[test]
    fn show_context_keeps_the_line_count_the_options_state() {
        let (mut view, _dir) = open(LEFT, Some(CENTER), RIGHT);
        run_until_ready(&mut view);
        view.run(Command::ShowContext);
        let frame_with = |view: &mut MergeView, lines: u32| {
            let mut options = ca_session::options::ProgramOptions::default();
            options.text_editing.context_lines = lines;
            let resolved = Arc::new(ca_ui::options::AppOptions::resolve(
                options,
                ca_ui::theme::Variant::Light,
                ca_session::AdminPolicies::default(),
            ));
            let ctx = egui::Context::default();
            let _ = ctx.run(raw_input(), |ctx| {
                ca_ui::options::install(ctx, Arc::clone(&resolved));
                egui::CentralPanel::default().show(ctx, |ui| {
                    view.ui(ui, &context());
                });
            });
        };
        frame_with(&mut view, 0);
        assert_eq!(view.filter(), super::MergeFilter::Context(0));
        let bare = view.visible_rows().len();
        frame_with(&mut view, 1);
        assert_eq!(view.filter(), super::MergeFilter::Context(1));
        assert_eq!(view.visible_rows().len(), bare + 2);
    }

    #[test]
    fn a_click_under_a_filter_lands_on_the_row_the_filter_shows_there() {
        let (mut view, _dir) = open(LEFT, Some(CENTER), RIGHT);
        run_until_ready(&mut view);
        view.run(Command::ShowConflicts);
        frame(&mut view);
        let pane = view.pane_rect(super::Pane::Left).unwrap();
        let first_body_row = pane.top() + view.row_height() * 1.5;
        view.click(egui::pos2(pane.center().x, first_body_row));
        assert_eq!(view.current_row(), conflict_rows(&view).start);
        assert_eq!(view.focused_pane(), super::Pane::Left);
    }

    fn long_text(changed: &str) -> String {
        let mut text = (0..200)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        text.push('\n');
        text.push_str(changed);
        text
    }

    #[test]
    fn a_click_at_a_fractional_scroll_lands_on_the_row_painted_under_it() {
        let (mut view, _dir) = open(
            &long_text("L\n"),
            Some(&long_text("b\n")),
            &long_text("R\n"),
        );
        run_until_ready(&mut view);
        frame(&mut view);
        let row_height = view.row_height();
        let viewport = view.viewport_height();
        let rows = view.visible_rows().len();
        view.scroll
            .scroll_by(row_height * 2.5, viewport, row_height, rows);
        assert!(view.scroll.fraction() > 0.0);
        let pane = view.pane_rect(super::Pane::Left).unwrap();
        let body_top = pane.top() + row_height;
        view.click(egui::pos2(pane.center().x, body_top + row_height * 0.75));
        assert_eq!(Some(view.current_row()), view.visible_rows().row_at(3));
    }

    fn frame_output(view: &mut MergeView, events: Vec<egui::Event>) -> egui::FullOutput {
        let ctx = egui::Context::default();
        let input = ca_ui::testing::event_input(1_000.0, 700.0, events);
        ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                view.ui(ui, &context());
            });
        })
    }

    #[test]
    fn enter_after_lone_cr_keeps_the_inserted_line_in_merge_output() {
        let original = "alpha\rbeta\nlast\n";
        let (mut view, _dir) = open(original, Some(original), original);
        run_until_ready(&mut view);
        view.output_pane
            .place(ca_ui::editor::Caret::new(1, 0), false);
        assert!(view.key(egui::Key::Enter, egui::Modifiers::NONE));
        view.absorb_output_edits();
        let expected = "alpha\r\r\nbeta\nlast\n";
        assert_eq!(view.output_pane.buffer().text(), expected);
        assert_eq!(view.output_text(), expected);
        view.run(Command::Undo);
        assert_eq!(view.output_text(), original);
        view.run(Command::Redo);
        assert_eq!(view.output_text(), expected);
    }

    #[test]
    fn enter_in_a_cr_only_merge_writes_a_carriage_return() {
        let original = "one\rtwo\rthree\r";
        let (mut view, _dir) = open(original, Some(original), original);
        run_until_ready(&mut view);
        view.output_pane
            .place(ca_ui::editor::Caret::new(1, 1), false);
        assert!(view.key(egui::Key::Enter, egui::Modifiers::NONE));
        view.absorb_output_edits();
        let expected = "one\rt\rwo\rthree\r";
        assert_eq!(view.output_pane.buffer().text(), expected);
        assert_eq!(view.output_text(), expected);
    }

    #[test]
    fn output_caret_matches_the_painted_glyph_at_each_font_and_scale() {
        let text = "0123456789".repeat(9);
        for scale in [1.0, 1.25, 1.5, 2.0] {
            for points in [10.0, 13.0, 17.0] {
                let (mut view, _dir) = open(&text, Some(&text), &text);
                run_until_ready(&mut view);
                view.font.set(points);
                view.focus = super::Pane::Output;
                view.output_pane
                    .place(ca_ui::editor::Caret::new(0, 70), false);
                let ctx = egui::Context::default();
                ctx.set_pixels_per_point(scale);
                let output = ctx.run(ca_ui::testing::sized_input(1400.0, 800.0), |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| view.ui(ui, &context()));
                });
                let body = view.pane_rect(super::Pane::Output).unwrap();
                let painted = output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::Shape::Text(held)
                            if held.galley.text() == text && body.contains(held.pos) =>
                        {
                            Some(held)
                        }
                        _ => None,
                    })
                    .unwrap();
                let expected = painted.pos.x + painted.galley.rows[0].glyphs[70].pos.x;
                let actual = output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::Shape::LineSegment { points, .. }
                            if body.contains(points[0])
                                && (points[1].y - points[0].y - view.row_height()).abs() < 0.01 =>
                        {
                            Some(points[0].x)
                        }
                        _ => None,
                    })
                    .unwrap();
                assert!(
                    (actual - expected).abs() < 0.5,
                    "scale {scale}, font {points}: caret {actual}, glyph {expected}"
                );
            }
        }
    }

    #[test]
    fn clicking_an_output_glyph_types_at_that_character() {
        for text in [
            "0123456789".repeat(9),
            format!("日本語{}", "0123456789".repeat(9)),
        ] {
            let (mut view, _dir) = open(&text, Some(&text), &text);
            run_until_ready(&mut view);
            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(1.25);
            let _ = ctx.run(ca_ui::testing::sized_input(1400.0, 800.0), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| view.ui(ui, &context()));
            });
            let output = ctx.run(ca_ui::testing::sized_input(1400.0, 800.0), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| view.ui(ui, &context()));
            });
            let output_rect = view.pane_rect(super::Pane::Output).unwrap();
            let painted = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(held)
                        if held.galley.text() == text && output_rect.contains(held.pos) =>
                    {
                        Some(held.clone())
                    }
                    _ => None,
                })
                .unwrap();
            let glyph = &painted.galley.rows[0].glyphs[70];
            let at = painted.pos + egui::vec2(glyph.pos.x + glyph.advance_width * 0.25, 2.0);
            for pressed in [true, false] {
                let _ = ctx.run(
                    ca_ui::testing::event_input(
                        1400.0,
                        800.0,
                        vec![
                            egui::Event::PointerMoved(at),
                            egui::Event::PointerButton {
                                pos: at,
                                button: egui::PointerButton::Primary,
                                pressed,
                                modifiers: egui::Modifiers::NONE,
                            },
                        ],
                    ),
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| view.ui(ui, &context()));
                    },
                );
            }
            assert_eq!(view.output_pane.caret().index, 70, "{text}");
            let _ = ctx.run(
                ca_ui::testing::event_input(1400.0, 800.0, vec![egui::Event::Text("X".into())]),
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| view.ui(ui, &context()));
                },
            );
            let mut expected: Vec<char> = text.chars().collect();
            expected.insert(70, 'X');
            assert_eq!(view.output_text(), expected.into_iter().collect::<String>());
        }
    }

    #[test]
    fn clicking_the_row_space_above_or_below_an_output_glyph_places_the_caret_at_it() {
        let text = "0123456789".repeat(9);
        for spacing in [0, 6] {
            let (mut view, _dir) = open(&text, Some(&text), &text);
            run_until_ready(&mut view);
            view.line_spacing = spacing;
            let ctx = egui::Context::default();
            let output = ctx.run(ca_ui::testing::sized_input(1400.0, 800.0), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| view.ui(ui, &context()));
            });
            let output_rect = view.pane_rect(super::Pane::Output).unwrap();
            let painted = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(held)
                        if held.galley.text() == text && output_rect.contains(held.pos) =>
                    {
                        Some(held.clone())
                    }
                    _ => None,
                })
                .unwrap();
            let height = painted.galley.size().y;
            let margin = (view.row_height() - height) / 2.0;
            assert!(margin > 1.0, "spacing {spacing}: margin {margin}");
            let glyph = &painted.galley.rows[0].glyphs[70];
            for dy in [0.5 - margin, height + margin - 0.5] {
                let at = painted.pos + egui::vec2(glyph.pos.x + glyph.advance_width * 0.25, dy);
                view.output_pane
                    .place(ca_ui::editor::Caret::new(0, 5), false);
                for pressed in [true, false] {
                    let _ = ctx.run(
                        ca_ui::testing::event_input(
                            1400.0,
                            800.0,
                            vec![
                                egui::Event::PointerMoved(at),
                                egui::Event::PointerButton {
                                    pos: at,
                                    button: egui::PointerButton::Primary,
                                    pressed,
                                    modifiers: egui::Modifiers::NONE,
                                },
                            ],
                        ),
                        |ctx| {
                            egui::CentralPanel::default().show(ctx, |ui| view.ui(ui, &context()));
                        },
                    );
                }
                assert_eq!(
                    view.output_pane.caret(),
                    ca_ui::editor::Caret::new(0, 70),
                    "spacing {spacing}, offset {dy}"
                );
            }
        }
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
    fn copy_and_cut_put_the_output_selection_on_the_clipboard() {
        let (mut view, _dir) = open("a\nL\nc\n", Some("a\nb\nc\n"), "a\nR\nc\n");
        run_until_ready(&mut view);
        view.run(Command::TakeLeft);
        let before = view.output_text();
        view.run(Command::SelectAll);
        view.run(Command::Copy);
        let output = frame_output(&mut view, Vec::new());
        assert_eq!(copied(&output), vec![before.clone()]);
        assert_eq!(view.output_text(), before);
        view.run(Command::SelectAll);
        view.run(Command::Cut);
        let output = frame_output(&mut view, Vec::new());
        assert_eq!(copied(&output), vec![before]);
        assert!(view.output_text().is_empty());
        assert!(view.is_modified());
    }

    #[test]
    fn paste_asks_for_the_clipboard_and_inserts_what_arrives() {
        let (mut view, _dir) = open("a\nL\nc\n", Some("a\nb\nc\n"), "a\nR\nc\n");
        run_until_ready(&mut view);
        view.run(Command::Paste);
        let output = frame_output(&mut view, Vec::new());
        let asked = output.viewport_output.values().any(|viewport| {
            viewport
                .commands
                .contains(&egui::ViewportCommand::RequestPaste)
        });
        assert!(asked);
        frame_output(&mut view, vec![egui::Event::Paste("P".to_owned())]);
        assert!(view.output_text().starts_with('P'));
    }

    #[test]
    fn favor_commands_leave_the_conflicts_and_the_output_alone() {
        let (mut view, _dir) = open(LEFT, Some(CENTER), RIGHT);
        run_until_ready(&mut view);
        let before = view.output_text();
        let waiting = view.model().totals().conflicts_remaining;
        assert!(waiting > 0);
        view.run(Command::FavorLeft);
        view.run(Command::FavorRight);
        assert_eq!(view.output_text(), before);
        assert_eq!(view.model().totals().conflicts_remaining, waiting);
    }

    #[test]
    fn favor_left_paints_only_the_left_changes_as_unchanged_in_the_output() {
        use ca_diff::merge3::MergeKind;
        use ca_ui::theme::merge::MergeClass;
        let (mut view, _dir) = open(
            "a\nL\nc\nd\ne\nf\ng\nh\n",
            Some("a\nb\nc\nd\ne\nf\ng\nh\n"),
            "a\nb\nc\nd\ne\nf\ng\nR\n",
        );
        run_until_ready(&mut view);
        let class_of = |view: &MergeView, kind: MergeKind| {
            let section = view
                .model()
                .sections()
                .iter()
                .find(|section| section.kind == kind)
                .unwrap();
            section.output_class_under(view.rules())
        };
        assert_eq!(
            class_of(&view, MergeKind::LeftChange),
            MergeClass::LeftChange
        );
        view.run(Command::FavorLeft);
        assert!(view.rules().favor_left);
        assert_eq!(
            class_of(&view, MergeKind::LeftChange),
            MergeClass::Unchanged
        );
        assert_eq!(
            class_of(&view, MergeKind::RightChange),
            MergeClass::RightChange
        );
        assert_eq!(view.model().totals().conflicts, 0);
        view.run(Command::FavorLeft);
        assert_eq!(
            class_of(&view, MergeKind::LeftChange),
            MergeClass::LeftChange
        );
    }

    #[test]
    fn swap_sides_exchanges_the_inputs_and_keeps_each_decision() {
        let (mut view, _dir) = open(LEFT, Some(CENTER), RIGHT);
        run_until_ready(&mut view);
        let left = view.paths().left.clone();
        let right = view.paths().right.clone();
        let rows = conflict_rows(&view);
        view.place_on_row(rows.start, super::Pane::Output);
        view.run(Command::TakeLeft);
        let taken = view.output_text();
        assert!(view.accepts(Command::SwapSides));
        view.run(Command::SwapSides);
        run_until_ready(&mut view);
        assert_eq!(view.paths().left, right);
        assert_eq!(view.paths().right, left);
        assert_eq!(view.output_text(), taken);
        assert_eq!(view.model().totals().conflicts_remaining, 0);
        assert!(view
            .model()
            .sections()
            .iter()
            .any(|section| section.resolution == super::Resolution::Right));
    }

    fn save_with_markers(view: &mut MergeView, dir: &tempfile::TempDir) -> String {
        view.run(Command::SaveFile);
        assert_eq!(view.question(), Some(&Question::SaveWithConflicts));
        view.answer(true);
        assert!(ca_ui::testing::wait_until(Duration::from_secs(20), || {
            view.drain_save();
            !view.is_saving()
        }));
        String::from_utf8(std::fs::read(dir.path().join("out.txt")).unwrap()).unwrap()
    }

    fn marker_block(view: &MergeView, end: &str, blocks: [&str; 3]) -> String {
        let labels = view.marker_labels();
        format!(
            "<<<<<<< {}{end}{}||||||| {}{end}{}======={end}{}>>>>>>> {}{end}",
            labels.left, blocks[0], labels.center, blocks[1], blocks[2], labels.right
        )
    }

    #[test]
    fn a_paste_that_completes_a_crlf_keeps_the_model_on_the_pane_lines() {
        let (mut view, _dir) = open("a\rb\rc\rL\r", Some("a\rb\rc\rB\r"), "a\rb\rc\rR\r");
        run_until_ready(&mut view);
        view.output_pane
            .place(ca_ui::editor::Caret::new(1, 0), false);
        view.output_pane.paste("\nx");
        view.absorb_output_edits();
        assert_eq!(view.output_pane.buffer().text(), "a\r\nxb\rc\rB\r");
        view.run(Command::NextConflict);
        view.run(Command::TakeLeft);
        assert_eq!(view.output_pane.buffer().text(), "a\r\nxb\rc\rL\r");
        assert_output_lines_match_pane(&view);
    }

    #[test]
    fn a_marker_save_after_a_crlf_paste_writes_the_pane_lines() {
        let (mut view, dir) = open("a\rb\rc\nL\n", Some("a\rb\rc\nB\n"), "a\rb\rc\nR\n");
        run_until_ready(&mut view);
        view.output_pane
            .place(ca_ui::editor::Caret::new(1, 0), false);
        view.output_pane.paste("\nx");
        view.absorb_output_edits();
        assert_eq!(view.output_pane.buffer().text(), "a\r\nxb\rc\nB\n");
        let saved = save_with_markers(&mut view, &dir);
        let expected = format!(
            "a\r\nxb\rc\n{}",
            marker_block(&view, "\r\n", ["L\n", "B\n", "R\n"])
        );
        assert_eq!(saved, expected);
        assert_output_lines_match_pane(&view);
    }

    #[test]
    fn deleting_between_a_lone_cr_and_a_lone_lf_keeps_the_model_on_the_pane_lines() {
        let (mut view, _dir) = open("p\rq\nz\nL\n", Some("p\rq\nz\nB\n"), "p\rq\nz\nR\n");
        run_until_ready(&mut view);
        view.output_pane
            .place(ca_ui::editor::Caret::new(1, 1), false);
        view.output_pane.backspace();
        view.absorb_output_edits();
        assert_eq!(view.output_pane.buffer().text(), "p\r\nz\nB\n");
        view.run(Command::NextConflict);
        view.run(Command::TakeLeft);
        assert_eq!(view.output_pane.buffer().text(), "p\r\nz\nL\n");
        assert_output_lines_match_pane(&view);
    }

    #[test]
    fn an_edit_after_a_repaired_separator_keeps_the_model_on_the_pane_lines() {
        let (mut view, dir) = open(
            "k\rx\rm1\rm2\rm3\rL\r",
            Some("k\rc\rm1\rm2\rm3\rB\r"),
            "k\n\nm1\nm2\nm3\nR\n",
        );
        run_until_ready(&mut view);
        view.run(Command::NextConflict);
        view.run(Command::TakeRight);
        assert_eq!(view.output_pane.buffer().text(), "k\r\n\nm1\rm2\rm3\rB\r");
        view.output_pane
            .place(ca_ui::editor::Caret::new(1, 0), false);
        view.output_pane
            .place(ca_ui::editor::Caret::new(2, 0), true);
        view.output_pane.type_character('Q');
        view.absorb_output_edits();
        assert_eq!(view.output_pane.buffer().text(), "k\r\nQm1\rm2\rm3\rB\r");
        let saved = save_with_markers(&mut view, &dir);
        let expected = format!(
            "k\r\nQm1\rm2\rm3\r{}",
            marker_block(&view, "\r\n", ["L\r", "B\r", "R\n"])
        );
        assert_eq!(saved, expected);
        assert_output_lines_match_pane(&view);
    }

    #[test]
    fn a_paste_over_a_selection_that_joins_the_line_above_keeps_the_model_on_the_pane_lines() {
        let (mut view, dir) = open(
            "a\rx\ry\rm1\rm2\rL\r",
            Some("a\rc\rd\rm1\rm2\rB\r"),
            "a\rc\rd\rm1\rm2\rR\r",
        );
        run_until_ready(&mut view);
        assert_eq!(view.output_pane.buffer().text(), "a\rx\ry\rm1\rm2\rB\r");
        view.output_pane
            .place(ca_ui::editor::Caret::new(1, 0), false);
        view.output_pane
            .place(ca_ui::editor::Caret::new(3, 0), true);
        view.output_pane.paste("\nP");
        view.absorb_output_edits();
        assert_eq!(view.output_pane.buffer().text(), "a\r\nPm1\rm2\rB\r");
        let saved = save_with_markers(&mut view, &dir);
        let expected = format!(
            "a\r\nPm1\rm2\r{}",
            marker_block(&view, "\r\n", ["L\r", "B\r", "R\r"])
        );
        assert_eq!(saved, expected);
        assert_output_lines_match_pane(&view);
    }

    #[test]
    fn an_inserted_separator_is_withdrawn_only_while_no_edit_has_claimed_it() {
        let (mut view, dir) = open("a\nb", Some("a\nb"), "a\nb\nc\n");
        run_until_ready(&mut view);
        let last = view.model().sections().len() - 1;
        assert_eq!(view.output_pane.buffer().text(), "a\nb\nc\n");
        view.output_pane
            .place(ca_ui::editor::Caret::new(2, 0), false);
        view.output_pane.type_character('Z');
        view.absorb_output_edits();
        view.current = last;
        view.run(Command::TakeCenter);
        assert_eq!(view.output_pane.buffer().text(), "a\nb");
        assert_output_lines_match_pane(&view);
        view.run(Command::Undo);
        assert_eq!(view.output_pane.buffer().text(), "a\nb\nZc\n");
        assert_output_lines_match_pane(&view);
        view.run(Command::Redo);
        assert_eq!(view.output_pane.buffer().text(), "a\nb");
        assert_output_lines_match_pane(&view);
        assert_eq!(save_and_read(&mut view, &dir), b"a\nb");

        let (mut view, dir) = open("a\nb", Some("a\nb"), "a\nb\nc\n");
        run_until_ready(&mut view);
        view.output_pane
            .place(ca_ui::editor::Caret::new(1, 1), false);
        view.output_pane.delete();
        view.absorb_output_edits();
        assert_eq!(view.output_pane.buffer().text(), "a\nbc\n");
        view.output_pane.enter("\n");
        view.absorb_output_edits();
        view.current = last;
        view.run(Command::TakeCenter);
        assert_eq!(view.output_pane.buffer().text(), "a\nb\nc\n");
        assert_output_lines_match_pane(&view);
        assert_eq!(save_and_read(&mut view, &dir), b"a\nb\nc\n");
    }

    /// Save, accepting markers when conflicts remain, and read the file back.
    fn save_and_read(view: &mut MergeView, dir: &tempfile::TempDir) -> Vec<u8> {
        view.run(Command::SaveFile);
        if view.question() == Some(&Question::SaveWithConflicts) {
            view.answer(true);
        }
        assert!(ca_ui::testing::wait_until(Duration::from_secs(20), || {
            view.drain_save();
            !view.is_saving()
        }));
        assert_eq!(view.question(), None);
        std::fs::read(dir.path().join("out.txt")).unwrap()
    }

    #[test]
    fn a_save_converts_the_pane_lines_when_the_options_name_an_ending() {
        let (mut view, dir) = open("a\rb\nc", Some("a\rb\nc"), "a\rb\nc\nd\r\n");
        run_until_ready(&mut view);
        assert_eq!(view.output_pane.buffer().text(), "a\rb\nc\rd\r\n");
        view.save_rules.line_endings = Some(ca_text::EolStyle::CrLf);
        assert_eq!(save_and_read(&mut view, &dir), b"a\r\nb\r\nc\r\nd\r\n");

        let (mut view, dir) = open("a\nL\nc\n", Some("a\nb\nc\n"), "a\nR\nc\n");
        run_until_ready(&mut view);
        view.save_rules.line_endings = Some(ca_text::EolStyle::Cr);
        let saved = String::from_utf8(save_and_read(&mut view, &dir)).unwrap();
        let expected = format!("a\r{}c\r", marker_block(&view, "\r", ["L\r", "b\r", "R\r"]));
        assert_eq!(saved, expected);
    }

    #[test]
    fn a_failed_save_keeps_the_target_and_the_unsaved_state() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, text: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, text.as_bytes()).unwrap();
            path
        };
        let output = dir.path().join("out.txt");
        std::fs::create_dir(&output).unwrap();
        std::fs::write(output.join("inside.txt"), b"kept").unwrap();
        let paths = super::MergePaths {
            left: write("left.txt", "a\nL\nc\n"),
            center: Some(write("center.txt", "a\nb\nc\n")),
            right: write("right.txt", "a\nb\nc\n"),
            output: Some(output.clone()),
        };
        let mut view = MergeView::over(paths, ca_ui::view::Titles::default(), &context(), 7);
        run_until_ready(&mut view);
        view.output_pane
            .place(ca_ui::editor::Caret::new(0, 0), false);
        view.output_pane.type_character('Z');
        view.absorb_output_edits();
        assert!(view.is_modified());
        view.run(Command::SaveFile);
        assert!(ca_ui::testing::wait_until(Duration::from_secs(20), || {
            view.drain_save();
            !view.is_saving()
        }));
        assert!(output.is_dir());
        assert_eq!(std::fs::read(output.join("inside.txt")).unwrap(), b"kept");
        assert!(view.is_modified());
        assert!(view.outcome().failed);
        assert!(!view.outcome().written);
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Encoding {
        Utf8,
        Utf8Bom,
        Utf16LeBom,
        Utf16BeBom,
    }

    fn encoded(text: &str, encoding: Encoding) -> Vec<u8> {
        match encoding {
            Encoding::Utf8 => text.as_bytes().to_vec(),
            Encoding::Utf8Bom => [&[0xef, 0xbb, 0xbf][..], text.as_bytes()].concat(),
            Encoding::Utf16LeBom => {
                let mut bytes = vec![0xff, 0xfe];
                for unit in text.encode_utf16() {
                    bytes.extend_from_slice(&unit.to_le_bytes());
                }
                bytes
            }
            Encoding::Utf16BeBom => {
                let mut bytes = vec![0xfe, 0xff];
                for unit in text.encode_utf16() {
                    bytes.extend_from_slice(&unit.to_be_bytes());
                }
                bytes
            }
        }
    }

    const ROUND_TRIP_TEXTS: [&str; 14] = [
        "a\nb\nc\n",
        "a\r\nb\r\nc\r\n",
        "a\rb\rc\r",
        "a\nb\r\nc\rd\n",
        "a\nb",
        "a\r\nb",
        "a\rb",
        "",
        "\n",
        "\r\n\r\n",
        "\r\r",
        "\n\r",
        "\r\n\n\r\r\n",
        "x\r\n\r\n\ny\rz",
    ];

    #[test]
    fn identical_inputs_save_their_own_bytes() {
        let mut cases: Vec<Vec<u8>> = Vec::new();
        for text in ROUND_TRIP_TEXTS {
            for encoding in [
                Encoding::Utf8,
                Encoding::Utf8Bom,
                Encoding::Utf16LeBom,
                Encoding::Utf16BeBom,
            ] {
                cases.push(encoded(text, encoding));
            }
        }
        cases.push(b"na\xefve caf\xe9\r\nr\xe9sum\xe9\nd\xe9j\xe0 vu".to_vec());
        for bytes in cases {
            for center in [true, false] {
                let (mut view, dir) = open_bytes(&bytes, center.then_some(&bytes[..]), &bytes);
                run_until_ready(&mut view);
                assert_eq!(view.model().totals().conflicts_remaining, 0);
                assert_output_lines_match_pane(&view);
                assert_eq!(
                    save_and_read(&mut view, &dir),
                    bytes,
                    "{bytes:?} center={center}"
                );
            }
        }
    }

    #[test]
    fn taking_every_section_from_one_side_saves_that_sides_bytes() {
        let cases: [(&str, &str, &str); 7] = [
            ("a\nL\nc", "a\nb\nc\n", "a\nR\nc\nd\n"),
            ("a\r\nL\r\nc\r\n", "a\r\nb\r\nc\r\n", "a\r\nR\r\nc"),
            ("a\rL\rc\r", "a\rb\rc", "a\rR\rc\rd\r"),
            ("a\nL\r\nc\r", "a\nb\nc\n", "a\r\nR\nc\n\n"),
            ("k\rx\r\nm\n", "k\rc\rm\r", "k\n\nm\n"),
            ("\r\r\n", "\n\n", "\r\n\r"),
            ("a\nb", "a\nb", "a\nb\nc\n"),
        ];
        for (left, center, right) in cases {
            for (command, side) in [
                (Command::TakeLeft, left),
                (Command::TakeCenter, center),
                (Command::TakeRight, right),
            ] {
                for encoding in [Encoding::Utf8, Encoding::Utf16LeBom] {
                    let (mut view, dir) = open_bytes(
                        &encoded(left, encoding),
                        Some(&encoded(center, encoding)),
                        &encoded(right, encoding),
                    );
                    run_until_ready(&mut view);
                    view.run(Command::SelectAll);
                    view.run(command);
                    let context = format!("{left:?} {center:?} {right:?} {command:?}");
                    assert_eq!(view.output_pane.buffer().text(), side, "{context}");
                    assert_output_lines_match_pane(&view);
                    assert_eq!(
                        save_and_read(&mut view, &dir),
                        encoded(side, encoding),
                        "{context} {encoding:?}"
                    );
                }
            }
        }
    }

    struct Seeded(u64);

    impl Seeded {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }

        fn pick(&mut self, limit: usize) -> usize {
            let limit = u64::try_from(limit.max(1)).unwrap();
            usize::try_from(self.next() % limit).unwrap()
        }

        fn one_in(&mut self, count: usize) -> bool {
            self.pick(count) == 0
        }

        fn ending(&mut self, style: usize) -> &'static str {
            ["\n", "\r\n", "\r"][if style < 3 { style } else { self.pick(3) }]
        }

        /// A line of content that later sides keep, change or drop.
        fn content(&mut self, tag: &str) -> String {
            match self.pick(6) {
                0 | 1 => String::new(),
                2 => " ".to_owned(),
                _ => format!("{tag}{}", self.pick(4)),
            }
        }

        fn side(&mut self, base: &[String], style: usize, tag: &str) -> String {
            let mut lines = base.to_vec();
            for _ in 0..self.pick(4) {
                let at = self.pick(lines.len() + 1);
                match self.pick(3) {
                    0 if at < lines.len() => lines[at] = self.content(tag),
                    1 if at < lines.len() => {
                        lines.remove(at);
                    }
                    _ => lines.insert(at, self.content(tag)),
                }
            }
            let mut text = String::new();
            for line in &lines {
                text.push_str(line);
                text.push_str(self.ending(style));
            }
            if self.one_in(2) {
                let kept = text.trim_end_matches(['\r', '\n']).len();
                let last = text[..kept].rfind(['\r', '\n']).map_or(0, |at| at + 1);
                if kept > last {
                    text.truncate(kept);
                }
            }
            text
        }

        fn pasted(&mut self) -> String {
            let mut text = String::new();
            if self.one_in(3) {
                text.push('\n');
            }
            for _ in 0..self.pick(3) {
                text.push_str(&self.content("p"));
                text.push_str(self.ending(3));
            }
            text.push('q');
            if self.one_in(3) {
                text.push('\r');
            }
            text
        }

        fn caret(&mut self, view: &MergeView) -> ca_ui::editor::Caret {
            let line = u32::try_from(self.pick(view.output_pane.line_count() as usize)).unwrap();
            let length = view.output_pane.line_text(line).chars().count();
            ca_ui::editor::Caret::new(line, u32::try_from(self.pick(length + 1)).unwrap())
        }
    }

    /// The output the line composition rule builds from the sections'
    /// sources, computed without the model's own repair.
    fn composed(model: &crate::model::MergeModel) -> Option<String> {
        use super::Resolution;
        let inputs = model.inputs();
        let slice = |lines: &[String], range: &std::ops::Range<u32>| -> Vec<String> {
            lines[range.start as usize..range.end as usize].to_vec()
        };
        let mut lines = Vec::new();
        for section in model.sections() {
            let left = slice(&inputs.left, &section.left);
            let right = slice(&inputs.right, &section.right);
            let baseline = if inputs.two_way {
                left.clone()
            } else {
                slice(&inputs.center, &section.center)
            };
            lines.extend(match section.resolution {
                Resolution::Left => left,
                Resolution::Right => right,
                Resolution::Center => slice(&inputs.center, &section.center),
                Resolution::LeftThenRight => [left, right].concat(),
                Resolution::RightThenLeft => [right, left].concat(),
                Resolution::Unresolved => baseline,
                Resolution::Edited => return None,
            });
        }
        let ending = ca_text::scan_eol(&inputs.left.concat()).dominant.as_str();
        for index in 1..lines.len() {
            let (before, after) = lines.split_at_mut(index);
            let line = &mut before[index - 1];
            if !line.ends_with(['\r', '\n']) {
                line.push_str(ending);
            }
            if line.ends_with('\r') && after[0].starts_with('\n') {
                line.push('\n');
            }
        }
        Some(lines.concat())
    }

    /// The marker output under the rule `output.rs` documents, built from the
    /// pane's lines, with the number of lines it must hold.
    fn expected_markers(view: &MergeView) -> (String, usize) {
        let pane = view.output_pane.buffer().text();
        let pane_lines = ca_diff::split_lines(&pane);
        let model = view.model();
        let inputs = model.inputs();
        let terminated = || {
            pane_lines
                .iter()
                .copied()
                .chain(inputs.left.iter().map(String::as_str))
        };
        let end = match terminated().find(|line| line.ends_with('\n')) {
            Some(line) if line.ends_with("\r\n") => "\r\n",
            None if terminated().any(|line| line.ends_with('\r')) => "\r",
            Some(_) | None => "\n",
        };
        let labels = view.marker_labels();
        let mut pieces: Vec<String> = Vec::new();
        for (index, section) in model.sections().iter().enumerate() {
            let range = model.output_range(index).unwrap();
            if !section.is_unresolved_conflict() {
                pieces.extend(
                    pane_lines[range.start as usize..range.end as usize]
                        .iter()
                        .map(|line| (*line).to_owned()),
                );
                continue;
            }
            let block = |lines: &[String], range: &std::ops::Range<u32>| {
                lines[range.start as usize..range.end as usize].to_vec()
            };
            pieces.push(format!("<<<<<<< {}{end}", labels.left));
            pieces.extend(block(&inputs.left, &section.left));
            if !inputs.two_way {
                pieces.push(format!("||||||| {}{end}", labels.center));
                pieces.extend(block(&inputs.center, &section.center));
            }
            pieces.push(format!("======={end}"));
            pieces.extend(block(&inputs.right, &section.right));
            pieces.push(format!(">>>>>>> {}{end}", labels.right));
        }
        let mut text = String::new();
        let mut count = 0;
        for piece in pieces.iter().filter(|piece| !piece.is_empty()) {
            if !text.is_empty() && !text.ends_with(['\r', '\n']) {
                text.push_str(end);
            }
            if text.ends_with('\r') && piece.starts_with('\n') {
                text.push('\n');
            }
            text.push_str(piece);
            count += 1;
        }
        (text, count)
    }

    #[derive(Debug, Default)]
    struct Tally {
        cases: usize,
        actions: usize,
        checkpoints: usize,
        composed: usize,
        saves: usize,
        marker_saves: usize,
        replace_alls: usize,
        reloads: usize,
    }

    fn checkpoint(
        view: &mut MergeView,
        dir: &tempfile::TempDir,
        encoding: Encoding,
        save: bool,
        tally: &mut Tally,
    ) {
        assert_output_lines_match_pane(view);
        if let Some(expected) = composed(view.model()) {
            assert_eq!(view.output_pane.buffer().text(), expected);
            tally.composed += 1;
        }
        if save {
            let conflicts = view.model().totals().conflicts_remaining;
            let (expected, lines) = if conflicts > 0 {
                expected_markers(view)
            } else {
                let pane = view.output_pane.buffer().text();
                let lines = ca_diff::split_lines(&pane).len();
                (pane, lines)
            };
            let saved = save_and_read(view, dir);
            assert_eq!(saved, encoded(&expected, encoding));
            assert_eq!(ca_diff::split_lines(&expected).len(), lines);
            assert!(!view.is_modified());
            tally.saves += 1;
            tally.marker_saves += usize::from(conflicts > 0);
        }
        tally.checkpoints += 1;
    }

    #[allow(clippy::too_many_lines)]
    fn act(view: &mut MergeView, random: &mut Seeded, tally: &mut Tally) {
        let sections = view.model().sections().len();
        let section = random.pick(sections);
        view.output_pane.clear_selection();
        view.current = section;
        view.row = view.model().row_of_section(section).unwrap_or(0);
        if random.one_in(5) {
            let from = random.caret(view);
            let to = random.caret(view);
            view.output_pane.place(from, false);
            view.output_pane.place(to, true);
        }
        if random.one_in(96) {
            view.output_pane.clear_selection();
            let settings = &mut view.find_panel().settings;
            settings.pattern = ["0", "1", "q", " "][random.pick(4)].to_owned();
            settings.replacement = ["", "Q", "\\n", "x\\r", "\\r\\ny"][random.pick(5)].to_owned();
            settings.regex = true;
            view.answer_panel(Some(super::PanelRequest::ReplaceAll));
            finish_search(view);
            tally.replace_alls += 1;
            return;
        }
        if random.one_in(192) {
            view.run(Command::Reload);
            run_until_ready(view);
            tally.reloads += 1;
            return;
        }
        match random.pick(20) {
            0 => view.run(Command::TakeLeft),
            1 => view.run(Command::TakeCenter),
            2 => view.run(Command::TakeRight),
            3 => view.run(Command::TakeLeftThenRight),
            4 => view.run(Command::TakeRightThenLeft),
            5..=7 => {
                view.row = random.pick(view.model().rows().len());
                view.run(
                    [
                        Command::TakeLeftLine,
                        Command::TakeCenterLine,
                        Command::TakeRightLine,
                    ][random.pick(3)],
                );
            }
            8 => view.run(Command::TakeAllNonConflicting),
            9 => {
                if view.accepts(Command::ToggleConflict) {
                    view.run(Command::ToggleConflict);
                }
            }
            10 | 11 => view.run(Command::Undo),
            12 => view.run(Command::Redo),
            action => {
                if view.output_pane.selection().is_none() {
                    let caret = random.caret(view);
                    view.output_pane.place(caret, false);
                }
                match action {
                    13 => view.output_pane.type_character('Z'),
                    14 => {
                        let ending = view.terminator();
                        view.output_pane.enter(ending);
                    }
                    15 => view.output_pane.backspace(),
                    16 => view.output_pane.delete(),
                    _ => {
                        let text = random.pasted();
                        view.output_pane.paste(&text);
                    }
                }
                view.absorb_output_edits();
            }
        }
    }

    #[test]
    fn randomized_merges_keep_composition_markers_and_saved_bytes() {
        const SEEDS: [u64; 4] = [
            0x6d65_7267_6520_454f,
            0x0123_4567_89ab_cdef,
            0x5eed_0000_0000_0035,
            0x00c0_ffee_d00d_f00d,
        ];
        const CASES: usize = 32;
        const ACTIONS: usize = 48;
        const SAVE_EVERY: usize = 12;
        let mut tally = Tally::default();
        for seed in SEEDS {
            let mut random = Seeded(seed);
            for case in 0..CASES {
                let base: Vec<String> = (0..random.pick(7)).map(|_| random.content("b")).collect();
                let styles = [random.pick(4), random.pick(4), random.pick(4)];
                let left = random.side(&base, styles[0], "L");
                let center = random.side(&base, styles[1], "C");
                let right = random.side(&base, styles[2], "R");
                let encoding = [
                    Encoding::Utf8,
                    Encoding::Utf8,
                    Encoding::Utf8,
                    Encoding::Utf8Bom,
                    Encoding::Utf16LeBom,
                ][random.pick(5)];
                let other = if random.one_in(2) {
                    encoding
                } else {
                    Encoding::Utf8
                };
                let two_way = random.one_in(6);
                let center_bytes = encoded(&center, other);
                let (mut view, dir) = open_bytes(
                    &encoded(&left, encoding),
                    (!two_way).then_some(&center_bytes[..]),
                    &encoded(&right, other),
                );
                run_until_ready(&mut view);
                let context = format!(
                    "seed {seed:#x} case {case}: {left:?} {center:?} {right:?} two_way={two_way}"
                );
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut local = Tally::default();
                    checkpoint(&mut view, &dir, encoding, true, &mut local);
                    for step in 1..=ACTIONS {
                        act(&mut view, &mut random, &mut local);
                        local.actions += 1;
                        let save = step % SAVE_EVERY == 0 || step == ACTIONS;
                        checkpoint(&mut view, &dir, encoding, save, &mut local);
                    }
                    local
                }));
                match result {
                    Ok(local) => {
                        tally.cases += 1;
                        tally.actions += local.actions;
                        tally.checkpoints += local.checkpoints;
                        tally.composed += local.composed;
                        tally.saves += local.saves;
                        tally.marker_saves += local.marker_saves;
                        tally.replace_alls += local.replace_alls;
                        tally.reloads += local.reloads;
                    }
                    Err(failure) => {
                        eprintln!("{context}");
                        std::panic::resume_unwind(failure);
                    }
                }
            }
        }
        println!("{tally:?}");
        assert_eq!(tally.cases, SEEDS.len() * CASES);
        assert!(tally.marker_saves > 0 && tally.saves > tally.marker_saves);
    }
}
