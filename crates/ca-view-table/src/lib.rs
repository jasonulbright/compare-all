//! Table comparison view.
//!
//! Two grids sit side by side over one aligned row model. The rows and the
//! columns are both virtualized, so a frame lays out only what the viewport
//! covers and its cost follows the viewport rather than the comparison.
//!
//! Reading, parsing, aligning and comparing all run on a worker. The frame
//! thread reads the result the worker posted and paints it.

mod auto_fit;
pub mod edit;
mod editing;
pub mod jobs;
pub mod model;
pub mod search;
pub mod session_options;
pub mod settings;
pub mod source;
pub mod thumbnail;

#[cfg(test)]
pub(crate) mod testing;

use ca_session::SessionKind;
use ca_table::compare::{CellStatus, RowStatus};
use ca_ui::command::{Command, MenuView};
use ca_ui::dialog::{DialogMessage, Target};
use ca_ui::report::{ReportKind, ViewReport};
use ca_ui::scroll::RowScroll;
use ca_ui::theme::table::{CellClass, Palette};
use ca_ui::theme::Variant;
use ca_ui::toolbar;
use ca_ui::view::{SessionView, ViewAction, ViewContext};
use ca_ui::widgets;
use ca_ui::worker::{Job, Terminal};
use jobs::{Pipeline, Side as SideData, Stage, TableMessage, TableSettings};
use model::{Cursor, DisplayFilter, Grid, Side};
use settings::SettingsPanel;
use source::ComparisonSource;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thumbnail::Strip;

/// Height of one grid row at the default font size, in points.
///
/// The tests measure against this; a frame asks the view, because the size
/// follows the options document.
#[cfg(test)]
const ROW_HEIGHT: f32 = 19.0;
/// Row height as a multiple of the display font size.
///
/// The default editor point size times this ratio is [`ROW_HEIGHT`], so a build
/// that states no font size keeps the measured row height.
const ROW_HEIGHT_RATIO: f32 = 1.81;
/// Height of the column header row.
const HEADER_HEIGHT: f32 = 22.0;
/// Width of the row number and status gutter.
const GUTTER_WIDTH: f32 = 54.0;
/// Width of the gap between the two grids.
const SEPARATOR: f32 = 6.0;
/// Thickness of a scrollbar.
const SCROLLBAR: f32 = 12.0;
/// Width of the difference strip.
const STRIP_WIDTH: f32 = 14.0;
/// How wide the grab area of a column edge is.
const GRIP: f32 = 4.0;
/// Room kept for text inside a cell.
const CELL_PAD: f32 = 4.0;
/// Width the display filter drop down is laid out in.
const FILTER_COMBO_WIDTH: f32 = 160.0;
/// Size of the mark drawn on a key column's heading.
const MARKER: f32 = 5.0;

/// Every command this view answers for.
const HANDLED: &[Command] = &[
    Command::OpenFile,
    Command::OpenClipboard,
    Command::CompareReport,
    Command::CompareInfo,
    Command::ShowAll,
    Command::ShowDifferences,
    Command::ShowSame,
    Command::ShowNone,
    Command::ToggleIgnoreUnimportant,
    Command::HideSameColumns,
    Command::ToggleLineNumbers,
    Command::Thumbnail,
    Command::ToggleLineDetails,
    Command::NextDifference,
    Command::PreviousDifference,
    Command::NextSection,
    Command::PreviousSection,
    Command::Reload,
    Command::Recompare,
    Command::SwapSides,
    Command::Cancel,
    Command::Undo,
    Command::Redo,
    Command::Cut,
    Command::Copy,
    Command::CopyCellToRight,
    Command::CopyCellToLeft,
    Command::CopyCellToOtherSide,
    Command::SelectAll,
    Command::Paste,
    Command::Delete,
    Command::SaveFile,
    Command::SaveFileAs,
    Command::SaveBoth,
    Command::Find,
    Command::Replace,
    Command::FindNext,
    Command::FindPrevious,
    Command::GoTo,
    Command::ResizeColumnsToFit,
];

/// Where a comparison currently stands.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Status {
    /// A step of the pipeline is running.
    Running(Stage),
    /// A comparison is on screen.
    Ready,
    /// The comparison could not be produced.
    Failed(String),
    /// The run stopped before producing a comparison.
    Cancelled,
}

impl Status {
    pub(crate) const fn is_running(&self) -> bool {
        matches!(self, Self::Running(_))
    }
}

enum SelectionCopyMessage {
    Ready(String),
    Cancelled,
    Failed(String),
}

impl Terminal for SelectionCopyMessage {
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

/// The Table comparison tab.
#[allow(clippy::struct_excessive_bools)]
pub struct TableView {
    id: egui::Id,
    /// The sides and the description the session last gave, reported with the
    /// sides replaced by the files the view has open.
    specs: ca_session::settings::SpecsSettings,
    /// The files are copies the tab owns, so no cell takes an edit and nothing
    /// is saved.
    pub(crate) read_only: bool,
    left_path: PathBuf,
    right_path: PathBuf,
    left_field: String,
    right_field: String,
    notify: Arc<dyn Fn() + Send + Sync>,
    pipeline: Pipeline,
    settings: TableSettings,
    grid: Grid,
    /// The parsed tables, kept so a settings change re-runs only the stages
    /// that depend on it.
    sides: Option<Box<(SideData, SideData)>>,
    status: Status,
    strip: Strip,
    /// What the strip was built for, so it is built again only when it must be.
    strip_key: (usize, u64, usize),
    revision: u64,
    scroll: RowScroll,
    horizontal: f32,
    viewport_height: f32,
    rows_in_view: usize,
    show_row_numbers: bool,
    show_strip: bool,
    show_details: bool,
    show_notices: bool,
    /// Source rows selected by Select All in the active pane.
    selected_rows: Vec<usize>,
    selection_side: Option<Side>,
    panel: SettingsPanel,
    picker: Option<Job<DialogMessage>>,
    picker_target: Target,
    font: ca_ui::font::FontSize,
    /// Padding the options add between rows, read once per frame.
    line_spacing: u32,
    /// The report command of this view.
    report: ViewReport,
    info_open: bool,
    active: Side,
    left_text: edit::SideText,
    right_text: edit::SideText,
    /// Revisions of the texts the tables on screen were parsed from.
    parsed: (u64, u64),
    /// Revisions of the texts the running reparse reads.
    reparse_revisions: (u64, u64),
    pending: editing::Pending,
    /// The column pairing and the row pairing of the comparison on screen,
    /// which map a grid cell to a file cell.
    mapping: Option<(
        Arc<ca_table::schema::Schema>,
        Arc<ca_table::compare::TableComparison>,
    )>,
    /// What each file looked like when it was read or last written.
    stamps: (Option<ca_ui::save::Stamp>, Option<ca_ui::save::Stamp>),
    cell_editor: Option<editing::CellEditor>,
    message: Option<String>,
    save_job: Option<Job<ca_ui::save::SaveMessage>>,
    save_rules: ca_ui::save::SaveRules,
    saving: Option<Side>,
    saving_revision: Option<u64>,
    saving_as: Option<PathBuf>,
    /// What the user agreed to for the save in progress. An answer adds its
    /// own consent to these and never grants another.
    saving_consent: ca_ui::save::text::SaveConsent,
    save_both_pending: bool,
    prompt: Option<editing::Prompt>,
    close_requested: bool,
    /// The command that reads the files again once the save in progress has
    /// written every edit.
    reread_after_save: Option<ca_ui::save::Reread>,
    /// Settings that read the files again, waiting on the question about the
    /// edits.
    pending_settings: Option<Box<TableSettings>>,
    picker_saves: bool,
    find: ca_ui::find::FindPanel,
    search_job: Option<Job<search::SearchMessage>>,
    fit_job: Option<Job<auto_fit::Message>>,
    fit_snapshot: Option<(u64, f32, f32)>,
    fit_requested: bool,
    copy_job: Option<Job<SelectionCopyMessage>>,
    copy_generation: Option<u64>,
    comparison_generation: u64,
    pixels_per_point: f32,
    replace_job: Option<Job<search::ReplaceMessage>>,
    /// The side and the text revision the running replace all read.
    replace_target: (Side, u64),
    find_after_reparse: bool,
    /// Whether the table cell address strip is open.
    goto_open: bool,
    /// The one-based visual row and optional column in the address strip.
    goto_text: String,
    /// Whether the address field asks for the keyboard on its next frame.
    goto_focus: bool,
    /// True while a system clipboard paste has been requested.
    paste_requested: bool,
    /// Whether the current paste request has already been sent to egui.
    paste_asked: bool,
    /// Clipboard text waiting to be sent to the platform.
    pending_clipboard: Option<String>,
    /// A tab-local clipboard snapshot, indexed by the side it replaced.
    clipboard_text: [Option<String>; 2],
    /// The pane whose clipboard text the platform was asked to provide.
    clipboard_requested: Option<Side>,
    /// The pane awaiting the paste event requested by Open Clipboard.
    clipboard_asked: Option<Side>,
}

impl TableView {
    /// A tab over the two sides, with the comparison already started.
    #[must_use]
    pub fn new(left: PathBuf, right: PathBuf, context: &ViewContext, instance: u64) -> Self {
        let mut view = Self {
            id: egui::Id::new(("table-compare", instance)),
            specs: ca_session::settings::SpecsSettings::default(),
            read_only: false,
            left_field: left.display().to_string(),
            right_field: right.display().to_string(),
            left_path: left,
            right_path: right,
            notify: context.notify.clone(),
            pipeline: Pipeline::new(),
            settings: TableSettings::default(),
            grid: Grid::default(),
            sides: None,
            status: Status::Running(Stage::Reading),
            strip: Strip::default(),
            strip_key: (0, 0, 0),
            revision: 0,
            scroll: RowScroll::top(),
            horizontal: 0.0,
            viewport_height: 0.0,
            rows_in_view: 0,
            show_row_numbers: true,
            show_strip: true,
            show_details: true,
            show_notices: true,
            selected_rows: Vec::new(),
            selection_side: None,
            panel: SettingsPanel::default(),
            picker: None,
            picker_target: Target::Left,
            font: ca_ui::font::FontSize::new(ca_session::options::provisional::EDITOR_POINT_SIZE),
            line_spacing: 0,
            report: ViewReport::new(
                ReportKind::Table,
                egui::Id::new(("table-compare", instance)),
                context.notify.clone(),
            ),
            info_open: false,
            active: Side::Left,
            left_text: edit::SideText::default(),
            right_text: edit::SideText::default(),
            parsed: (0, 0),
            reparse_revisions: (0, 0),
            pending: editing::Pending::Load,
            mapping: None,
            stamps: (None, None),
            cell_editor: None,
            message: None,
            save_job: None,
            save_rules: ca_ui::save::SaveRules::default(),
            saving: None,
            saving_revision: None,
            saving_as: None,
            saving_consent: ca_ui::save::text::SaveConsent::default(),
            save_both_pending: false,
            prompt: None,
            close_requested: false,
            reread_after_save: None,
            pending_settings: None,
            picker_saves: false,
            find: ca_ui::find::FindPanel::with_settings(ca_ui::find::FindSettings {
                wrap: true,
                ..ca_ui::find::FindSettings::default()
            }),
            search_job: None,
            fit_job: None,
            fit_snapshot: None,
            fit_requested: false,
            copy_job: None,
            copy_generation: None,
            comparison_generation: 0,
            pixels_per_point: 1.0,
            replace_job: None,
            replace_target: (Side::Left, 0),
            find_after_reparse: false,
            goto_open: false,
            goto_text: String::new(),
            goto_focus: false,
            paste_requested: false,
            paste_asked: false,
            pending_clipboard: None,
            clipboard_text: [None, None],
            clipboard_requested: None,
            clipboard_asked: None,
        };
        view.restart();
        view
    }

    /// Which kind of session this view answers for.
    #[must_use]
    pub fn kind() -> SessionKind {
        SessionKind::TableCompare
    }

    /// Height of one grid row at the size the options state.
    #[must_use]
    pub fn row_height(&self) -> f32 {
        self.font.row_height(ROW_HEIGHT_RATIO, self.line_spacing)
    }

    /// Take the sizes the options document now states.
    fn follow_options(&mut self, ctx: &egui::Context) {
        let options = ca_ui::options::current(ctx);
        self.font.follow(options.editor_point_size());
        self.line_spacing = options.extra_line_spacing();
        self.save_rules = ca_ui::save::SaveRules::from_options(&options.stored);
    }

    /// The left side path.
    #[must_use]
    pub fn left(&self) -> &Path {
        &self.left_path
    }

    /// The right side path.
    #[must_use]
    pub fn right(&self) -> &Path {
        &self.right_path
    }

    /// The display model behind the grids.
    #[must_use]
    pub const fn grid(&self) -> &Grid {
        &self.grid
    }

    /// The display model behind the grids.
    pub const fn grid_mut(&mut self) -> &mut Grid {
        &mut self.grid
    }

    /// The settings the next comparison runs under.
    #[must_use]
    pub const fn settings(&self) -> &TableSettings {
        &self.settings
    }

    /// True when the row number column is shown.
    #[must_use]
    pub const fn shows_row_numbers(&self) -> bool {
        self.show_row_numbers
    }

    /// True when the thumbnail strip is shown.
    #[must_use]
    pub const fn shows_strip(&self) -> bool {
        self.show_strip
    }

    /// True when the cell details area is shown.
    #[must_use]
    pub const fn shows_details(&self) -> bool {
        self.show_details
    }

    /// True once a comparison is on screen.
    #[must_use]
    pub fn has_comparison(&self) -> bool {
        self.status == Status::Ready
    }

    /// Why the comparison failed, where it did.
    #[must_use]
    pub fn failure(&self) -> Option<&str> {
        match &self.status {
            Status::Failed(reason) => Some(reason),
            _ => None,
        }
    }

    /// How many requests were replaced before they finished.
    #[must_use]
    pub const fn superseded_requests(&self) -> u64 {
        self.pipeline.superseded_count()
    }

    /// Show a different set of rows.
    pub fn set_filter(&mut self, filter: DisplayFilter) {
        self.clear_selection();
        self.grid.set_filter(filter);
        self.revision += 1;
        self.clamp_scroll();
    }

    /// Put the grid over a comparison built by the caller.
    ///
    /// The tests reach the painting path through this without writing files.
    pub fn set_source(&mut self, source: Arc<dyn model::Source>) {
        self.pipeline.stop();
        self.cancel_selection_copy();
        self.cancel_auto_fit();
        self.clear_selection();
        self.grid.set_source(source);
        self.sides = None;
        self.mapping = None;
        self.status = Status::Ready;
        self.comparison_generation = self.comparison_generation.wrapping_add(1);
        self.revision += 1;
    }

    /// Mark a comparison column as a key and compare again.
    pub fn set_key_column(&mut self, column: usize, key: bool) {
        let index = u32::try_from(column).unwrap_or(0);
        let handling = self.settings.schema.handling.entry(index).or_default();
        handling.key = key;
        self.recompare();
    }

    /// Sort both sides before pairing their rows, or stop doing so.
    pub fn set_sort_rows_before_alignment(&mut self, sort: bool) {
        self.settings.align.sort_rows_before_alignment = sort;
        self.recompare();
    }

    /// Choose how rows pair when no column is a key.
    pub fn set_row_alignment_mode(&mut self, mode: ca_table::align::RowAlignmentMode) {
        self.settings.align.mode = mode;
        self.recompare();
    }

    /// Read both files again and compare them.
    pub fn restart(&mut self) {
        self.clipboard_requested = None;
        self.clipboard_asked = None;
        self.cancel_selection_copy();
        self.sides = None;
        self.cell_editor = None;
        self.pending = editing::Pending::Load;
        self.status = Status::Running(Stage::Reading);
        let job = jobs::spawn_load_with_clipboard(
            self.left_path.clone(),
            self.right_path.clone(),
            self.settings.clone(),
            self.clipboard_text.clone(),
            Arc::clone(&self.notify),
        );
        self.pipeline.start(job);
    }

    /// Compare again under the current settings, without reading the files.
    pub fn recompare(&mut self) {
        self.cancel_selection_copy();
        if self.has_unparsed_edits() {
            self.start_reparse();
            return;
        }
        let Some(sides) = self.sides.as_ref() else {
            self.restart();
            return;
        };
        self.status = Status::Running(Stage::Schema);
        self.pending = editing::Pending::Recompare;
        let job = jobs::spawn_recompare(
            sides.0.clone(),
            sides.1.clone(),
            self.settings.clone(),
            Arc::clone(&self.notify),
        );
        self.pipeline.start(job);
    }

    /// Take whatever the worker has posted.
    pub fn poll(&mut self) {
        for message in self.pipeline.poll() {
            match message {
                TableMessage::Progress(stage) => self.status = Status::Running(stage),
                TableMessage::Failed(reason) => self.status = Status::Failed(reason),
                TableMessage::Cancelled => {
                    if self.status.is_running() {
                        self.status = Status::Cancelled;
                    }
                }
                TableMessage::Ready(data) => self.apply(data),
            }
        }
        self.poll_picker();
        self.poll_save();
        self.poll_search();
        self.poll_replace();
        self.poll_auto_fit();
        self.poll_selection_copy();
    }

    fn start_auto_fit(&mut self, ctx: &egui::Context) {
        if !self.fit_requested || self.fit_job.is_some() {
            return;
        }
        if !self.has_comparison() {
            self.fit_requested = false;
            return;
        }
        self.fit_requested = false;
        self.fit_snapshot = Some((
            self.comparison_generation,
            self.font.points(),
            ctx.pixels_per_point(),
        ));
        self.fit_job = Some(auto_fit::spawn(
            Arc::clone(self.grid.source()),
            self.font.points(),
            ctx.pixels_per_point(),
            Arc::clone(&self.notify),
        ));
    }

    fn poll_auto_fit(&mut self) {
        let Some(job) = self.fit_job.as_mut() else {
            return;
        };
        let messages = job.drain();
        let finished = job.is_finished();
        for message in messages {
            match message {
                auto_fit::Message::Ready(widths) => {
                    let matches = self.fit_snapshot.is_some_and(
                        |(generation, point_size, pixels_per_point)| {
                            generation == self.comparison_generation
                                && point_size.to_bits() == self.font.points().to_bits()
                                && pixels_per_point.to_bits() == self.pixels_per_point.to_bits()
                        },
                    );
                    if matches && widths.len() == self.grid.source().columns().len() {
                        self.grid.set_widths(&widths);
                    } else if self.has_comparison() {
                        // The comparison or display font changed during the
                        // scan. Repeat against the current state.
                        self.fit_requested = true;
                    }
                }
                auto_fit::Message::Cancelled => {}
                auto_fit::Message::Failed(reason) => {
                    self.message = Some(format!("Column sizing failed: {reason}"));
                }
            }
        }
        if finished {
            self.fit_job = None;
            self.fit_snapshot = None;
        }
    }

    fn cancel_auto_fit(&mut self) {
        self.fit_job = None;
        self.fit_snapshot = None;
        self.fit_requested = false;
    }

    fn start_selection_copy(&mut self, side: Side) {
        let source = Arc::clone(self.grid.source());
        let rows = self.selected_rows.clone();
        let columns = self.grid.shown_columns().to_vec();
        let generation = self.comparison_generation;
        self.copy_generation = Some(generation);
        self.copy_job = Some(Job::spawn_notifying(
            move |emitter, cancel| {
                let mut text = String::new();
                for (row_index, row) in rows.iter().copied().enumerate() {
                    if row_index % 128 == 0 && cancel.is_cancelled() {
                        return;
                    }
                    if row_index > 0 {
                        text.push('\n');
                    }
                    for (column_index, column) in columns.iter().copied().enumerate() {
                        if column_index > 0 {
                            text.push('\t');
                        }
                        text.push_str(&ca_ui::clipboard::table_field(
                            &source.cell_text(row, column, side),
                        ));
                    }
                }
                emitter.send(SelectionCopyMessage::Ready(text));
            },
            Arc::clone(&self.notify),
        ));
    }

    fn poll_selection_copy(&mut self) {
        let Some(job) = self.copy_job.as_mut() else {
            return;
        };
        let messages = job.drain();
        let finished = job.is_finished();
        for message in messages {
            match message {
                SelectionCopyMessage::Ready(text)
                    if self.copy_generation == Some(self.comparison_generation) =>
                {
                    self.pending_clipboard = Some(text);
                }
                SelectionCopyMessage::Ready(_) | SelectionCopyMessage::Cancelled => {}
                SelectionCopyMessage::Failed(detail) => {
                    self.message = Some(format!("Could not copy the selection: {detail}"));
                }
            }
        }
        if finished {
            self.copy_job = None;
            self.copy_generation = None;
        }
    }

    fn cancel_selection_copy(&mut self) {
        self.copy_job = None;
        self.copy_generation = None;
    }

    fn apply(&mut self, data: Box<jobs::TableData>) {
        self.cancel_selection_copy();
        self.cancel_auto_fit();
        self.clear_selection();
        let pending = self.pending;
        self.sides = Some(Box::new((data.left.clone(), data.right.clone())));
        self.mapping = Some((Arc::clone(&data.schema), Arc::clone(&data.comparison)));
        if let Some((left, right)) = data.texts.clone() {
            self.left_text = edit::SideText::from_shared(left);
            self.right_text = edit::SideText::from_shared(right);
            self.parsed = (self.left_text.revision(), self.right_text.revision());
            self.stamps = (data.left.facts.stamp, data.right.facts.stamp);
        } else if pending == editing::Pending::Edit {
            self.parsed = self.reparse_revisions;
        }
        let cursor = self.grid.cursor();
        self.grid.set_source(Arc::new(ComparisonSource::new(data)));
        self.status = Status::Ready;
        self.comparison_generation = self.comparison_generation.wrapping_add(1);
        self.revision += 1;
        if pending == editing::Pending::Edit {
            self.grid.set_cursor(cursor);
            self.reveal_cursor();
            self.clamp_scroll();
            if std::mem::take(&mut self.find_after_reparse) {
                self.find_next(false);
            }
            return;
        }
        if let Some(row) = self.grid.next_difference(0) {
            self.grid.set_cursor(Cursor {
                row,
                column: self.grid.cursor().column,
            });
            self.reveal_cursor();
        }
        self.clamp_scroll();
    }

    fn poll_picker(&mut self) {
        let Some(job) = self.picker.as_mut() else {
            return;
        };
        let messages = job.drain();
        let finished = job.is_finished();
        for message in messages {
            if let DialogMessage::Chosen(path) = message {
                if self.picker_saves {
                    self.save_active_as(&path);
                    continue;
                }
                let text = path.display().to_string();
                match self.picker_target {
                    Target::Left => {
                        self.left_field = text;
                        self.clipboard_text[0] = None;
                    }
                    Target::Right => {
                        self.right_field = text;
                        self.clipboard_text[1] = None;
                    }
                }
                self.request_reread(ca_ui::save::Reread::Open);
            }
        }
        if finished {
            self.picker = None;
        }
    }

    fn open_save_picker(&mut self) {
        if self.picker.is_some() {
            return;
        }
        self.picker_saves = true;
        self.picker = Some(ca_ui::dialog::spawn(
            ca_ui::dialog::Pick::SaveFile,
            Arc::clone(&self.notify),
        ));
    }

    fn open_picker(&mut self, target: Target) {
        self.picker_saves = false;
        self.picker_target = target;
        self.picker = Some(ca_ui::dialog::spawn(
            ca_ui::dialog::Pick::File,
            Arc::clone(&self.notify),
        ));
    }

    pub(crate) fn apply_fields(&mut self) {
        self.left_path = PathBuf::from(self.left_field.clone());
        self.right_path = PathBuf::from(self.right_field.clone());
        self.restart();
    }

    fn clamp_scroll(&mut self) {
        let total = self.grid.visual_rows();
        self.scroll
            .clamp(self.viewport_height, self.row_height(), total);
    }

    fn reveal_cursor(&mut self) {
        let total = self.grid.visual_rows();
        self.scroll.reveal(
            self.grid.cursor().row,
            self.viewport_height,
            self.row_height(),
            total,
        );
    }

    fn clear_selection(&mut self) {
        self.selected_rows.clear();
        self.selection_side = None;
    }

    fn select_all_visible_rows(&mut self) {
        let side = self.active;
        let source = Arc::clone(self.grid.source());
        self.selected_rows = (0..self.grid.visual_rows())
            .filter_map(|visual| {
                let row = self.grid.source_row(visual)?;
                let present = !matches!(
                    (source.row_status(row), side),
                    (RowStatus::LeftOnly, Side::Right) | (RowStatus::RightOnly, Side::Left)
                );
                present.then_some(row)
            })
            .collect();
        self.selected_rows.sort_unstable();
        self.selection_side = (!self.selected_rows.is_empty()).then_some(side);
    }

    fn selected_row(&self, row: usize, side: Side) -> bool {
        self.selection_side == Some(side) && self.selected_rows.binary_search(&row).is_ok()
    }

    fn copy_current_to_clipboard(&mut self) {
        if !self.selected_rows.is_empty() {
            if let Some(side) = self.selection_side {
                self.start_selection_copy(side);
                return;
            }
        }
        if self.grid.visual_rows() > 0 && !self.grid.shown_columns().is_empty() {
            let cursor = self.grid.cursor();
            self.pending_clipboard = Some(
                self.grid
                    .cell_text(cursor.row, cursor.column, self.active)
                    .into_owned(),
            );
        }
    }

    fn set_grid_cursor(&mut self, cursor: Cursor) {
        if self.grid.cursor() != cursor {
            self.clear_selection();
        }
        self.grid.set_cursor(cursor);
    }

    fn move_grid_cursor(&mut self, rows: i64, columns: i64) {
        let before = self.grid.cursor();
        self.grid.move_cursor(rows, columns);
        if self.grid.cursor() != before {
            self.clear_selection();
        }
    }

    /// Move the current cell with the keyboard.
    pub fn navigate(&mut self, key: egui::Key, page: usize) {
        let step = i64::try_from(page.max(1)).unwrap_or(1);
        match key {
            egui::Key::ArrowDown => self.move_grid_cursor(1, 0),
            egui::Key::ArrowUp => self.move_grid_cursor(-1, 0),
            egui::Key::ArrowRight | egui::Key::Tab => self.move_grid_cursor(0, 1),
            egui::Key::ArrowLeft => self.move_grid_cursor(0, -1),
            egui::Key::PageDown => self.move_grid_cursor(step, 0),
            egui::Key::PageUp => self.move_grid_cursor(-step, 0),
            egui::Key::Home => self.set_grid_cursor(Cursor {
                row: 0,
                column: self.grid.cursor().column,
            }),
            egui::Key::End => self.set_grid_cursor(Cursor {
                row: self.grid.visual_rows().saturating_sub(1),
                column: self.grid.cursor().column,
            }),
            _ => return,
        }
        self.reveal_cursor();
    }

    /// Move to the next differing row.
    pub fn go_to_next_difference(&mut self) {
        let from = self.grid.cursor().row.saturating_add(1);
        if let Some(row) = self.grid.next_difference(from) {
            self.set_grid_cursor(Cursor {
                row,
                column: self.grid.cursor().column,
            });
            self.reveal_cursor();
        }
    }

    /// Move to the previous differing row.
    pub fn go_to_previous_difference(&mut self) {
        let Some(from) = self.grid.cursor().row.checked_sub(1) else {
            return;
        };
        if let Some(row) = self.grid.previous_difference(from) {
            self.set_grid_cursor(Cursor {
                row,
                column: self.grid.cursor().column,
            });
            self.reveal_cursor();
        }
    }

    /// The status of the current cell, for the status bar.
    #[must_use]
    pub fn current_cell_status(&self) -> Option<CellStatus> {
        let cursor = self.grid.cursor();
        let row = self.grid.source_row(cursor.row)?;
        let column = self.grid.column_at(cursor.column)?;
        self.grid.source().cell_status(row, column).into()
    }

    fn goto_panel(&mut self, ui: &mut egui::Ui) {
        if !self.goto_open {
            return;
        }
        let mut go = false;
        let mut close = false;
        ui.horizontal_wrapped(|ui| {
            ui.label("Go to row[,column]");
            let field = ui.add(
                egui::TextEdit::singleline(&mut self.goto_text)
                    .desired_width(120.0)
                    .id(self.id.with("table-goto")),
            );
            if std::mem::take(&mut self.goto_focus) {
                field.request_focus();
            }
            if field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                go = true;
            }
            if ca_ui::widgets::inline_button(ui, "Go", ca_ui::icons::Icon::GoTo).clicked() {
                go = true;
            }
            if ui.button("Close").clicked() {
                close = true;
            }
        });
        if go {
            self.go_to_address();
        }
        if close || (go && self.message.is_none()) {
            self.goto_open = false;
        }
    }

    fn go_to_address(&mut self) {
        match parse_table_address(
            &self.goto_text,
            self.grid.visual_rows(),
            self.grid.shown_columns().len(),
            self.grid.cursor().column,
        ) {
            Ok(cursor) => {
                self.set_grid_cursor(cursor);
                self.reveal_cursor();
                self.message = None;
            }
            Err(reason) => self.message = Some(reason.to_owned()),
        }
    }

    fn cut_current_cell(&mut self) {
        if !self.can_edit_cell() {
            return;
        }
        let cursor = self.grid.cursor();
        let text = self
            .grid
            .cell_text(cursor.row, cursor.column, self.active)
            .into_owned();
        match self.edit_current_cell("") {
            Ok(()) => self.pending_clipboard = Some(text),
            Err(reason) => self.message = Some(reason),
        }
    }

    fn clear_current_cell(&mut self) {
        if let Err(reason) = self.edit_current_cell("") {
            self.message = Some(reason);
        }
    }

    fn clipboard_events(&mut self, ctx: &egui::Context) {
        if let Some(text) = self.pending_clipboard.take() {
            ctx.copy_text(text);
        }
        let (pasted, open_clipboard_shortcut) = ctx.input(|input| {
            (
                input.events.iter().find_map(|event| match event {
                    egui::Event::Paste(text) => Some(text.clone()),
                    _ => None,
                }),
                input.modifiers.command && input.modifiers.shift,
            )
        });
        let mut paste_consumed = false;
        if let Some(side) = self.clipboard_asked.take() {
            match pasted.as_ref().filter(|text| !text.is_empty()) {
                Some(text) => {
                    self.clipboard_text[match side {
                        Side::Left => 0,
                        Side::Right => 1,
                    }] = Some(text.clone());
                    self.clear_selection();
                    self.restart();
                    self.message = None;
                }
                None => self.message = Some("The clipboard holds no text".to_owned()),
            }
            paste_consumed = true;
        } else if open_clipboard_shortcut && self.can_open_clipboard() {
            match pasted.as_ref().filter(|text| !text.is_empty()) {
                Some(text) => {
                    self.clipboard_text[match self.active {
                        Side::Left => 0,
                        Side::Right => 1,
                    }] = Some(text.clone());
                    self.clear_selection();
                    self.restart();
                    self.message = None;
                }
                None => self.message = Some("The clipboard holds no text".to_owned()),
            }
            paste_consumed = true;
        }
        let waiting_for_paste = std::mem::take(&mut self.paste_asked);
        if waiting_for_paste && !paste_consumed {
            if let Some(text) = pasted {
                self.paste_requested = false;
                if let Err(reason) = self.edit_current_cell(&text) {
                    self.message = Some(reason);
                }
            }
        }
        if let Some(side) = self.clipboard_requested.take() {
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
            self.clipboard_asked = Some(side);
        }
        if self.paste_requested {
            self.paste_requested = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
            self.paste_asked = true;
        }
    }

    fn clipboard_request_pending(&self) -> bool {
        self.clipboard_requested.is_some() || self.clipboard_asked.is_some()
    }

    fn can_open_clipboard(&self) -> bool {
        self.has_comparison()
            && !self.clipboard_request_pending()
            && !self.paste_requested
            && !self.paste_asked
            && self.cell_editor.is_none()
            && self.picker.is_none()
            && self.save_job.is_none()
            && !self.is_modified(Side::Left)
            && !self.is_modified(Side::Right)
    }

    fn palette(ui: &egui::Ui) -> &'static Palette {
        ca_ui::theme::table::palette(Variant::from_dark_mode(ui.visuals().dark_mode))
    }

    fn path_bar(&mut self, ui: &mut egui::Ui) {
        let left_title = self.clipboard_text[0].as_ref().map(|_| "Clipboard");
        let right_title = self.clipboard_text[1].as_ref().map(|_| "Clipboard");
        let action = widgets::path_bar_with_titles(
            ui,
            &mut self.left_field,
            &mut self.right_field,
            left_title,
            right_title,
            "Reload",
        );
        match action {
            Some(widgets::PathBarAction::Browse(target)) => self.open_picker(target),
            Some(widgets::PathBarAction::Reload) => {
                self.request_reread(ca_ui::save::Reread::Open);
            }
            None => {}
        }
    }

    /// The toolbar items this view declares, in the state it is in now.
    #[must_use]
    pub fn toolbar_items(&self) -> Vec<toolbar::Item> {
        let running = self.status.is_running();
        let ready = self.has_comparison();
        vec![
            toolbar::Item::widget("filter", FILTER_COMBO_WIDTH),
            toolbar::Item::widget("minor", 70.0),
            toolbar::Item::widget("hide-same", 160.0),
            toolbar::Item::widget("unhide-column", 150.0),
            toolbar::Item::separator("separator-1"),
            toolbar::Item::command(
                "previous-difference",
                Command::PreviousDifference,
                "Previous",
                ready,
                "No comparison yet",
            ),
            toolbar::Item::command(
                "next-difference",
                Command::NextDifference,
                "Next",
                ready,
                "No comparison yet",
            ),
            toolbar::Item::separator("separator-2"),
            toolbar::Item::command("settings", Command::SessionSettings, "Settings", true, ""),
            toolbar::Item::command(
                "recompare",
                Command::Recompare,
                "Recompare",
                !running,
                "Work is running",
            ),
            toolbar::Item::command(
                "swap",
                Command::SwapSides,
                "Swap",
                !running,
                "Work is running",
            ),
            toolbar::Item::command(
                "stop",
                Command::Cancel,
                "Stop",
                running,
                "Nothing is running",
            ),
            toolbar::Item::command(
                "report",
                Command::CompareReport,
                "Report",
                ready,
                "No comparison yet",
            ),
            toolbar::Item::separator("separator-3"),
            toolbar::Item::widget("row-numbers", 120.0),
            toolbar::Item::widget("strip", 110.0),
            toolbar::Item::widget("details", 100.0),
        ]
    }

    #[allow(clippy::too_many_lines)]
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let mut filter = self.grid.filter();
        let mut ignore = self.grid.ignores_unimportant();
        let mut hide_same = self.grid.hides_same_columns();
        let hidden_columns: Vec<_> = self
            .grid
            .hidden_columns()
            .map(|(index, column)| (index, column.name.clone()))
            .collect();
        let mut unhide_column = None;
        let mut row_numbers = self.show_row_numbers;
        let mut strip = self.show_strip;
        let mut details = self.show_details;
        let id = self.id;
        let items = self.toolbar_items();
        let layout = toolbar::Layout::from_options(
            &ca_ui::options::runtime::current(ui.ctx()).stored.commands,
            toolbar::ToolbarView::Table,
        );
        let outcome = toolbar::show_for(
            toolbar::ToolbarView::Table,
            ui,
            id.with("toolbar"),
            &items,
            &layout,
            |ui, name| match name {
                "filter" => {
                    egui::ComboBox::from_id_salt(id.with("filter"))
                        .width(FILTER_COMBO_WIDTH)
                        .selected_text(filter.label())
                        .show_ui(ui, |ui| {
                            for option in [
                                DisplayFilter::All,
                                DisplayFilter::Differences,
                                DisplayFilter::Same,
                                DisplayFilter::None,
                            ] {
                                ui.selectable_value(&mut filter, option, option.label());
                            }
                        });
                }
                "minor" => {
                    ca_ui::widgets::icon_toggle(
                        ui,
                        &mut ignore,
                        "Minor",
                        ca_ui::icons::Icon::IgnoreUnimportant,
                    );
                }
                "hide-same" => {
                    ca_ui::widgets::icon_toggle(
                        ui,
                        &mut hide_same,
                        "Hide same columns",
                        ca_ui::icons::Icon::HideSameColumns,
                    );
                }
                "unhide-column" => {
                    ui.add_enabled_ui(!hidden_columns.is_empty(), |ui| {
                        ca_ui::widgets::icon_menu(
                            ui,
                            "Unhide Column",
                            ca_ui::icons::Icon::UnhideColumn,
                            |ui| {
                                for (column, name) in &hidden_columns {
                                    if ui.button(name).clicked() {
                                        unhide_column = Some(*column);
                                        ui.close_menu();
                                    }
                                }
                            },
                        );
                    });
                }
                "row-numbers" => {
                    ca_ui::widgets::icon_toggle(
                        ui,
                        &mut row_numbers,
                        "Row numbers",
                        ca_ui::icons::Icon::LineNumbers,
                    );
                }
                "strip" => {
                    ca_ui::widgets::icon_toggle(
                        ui,
                        &mut strip,
                        "Thumbnail",
                        ca_ui::icons::Icon::Thumbnail,
                    );
                }
                "details" => {
                    ca_ui::widgets::icon_toggle(
                        ui,
                        &mut details,
                        "Details",
                        ca_ui::icons::Icon::LineDetails,
                    );
                }
                _ => {}
            },
        );
        if filter != self.grid.filter() {
            self.set_filter(filter);
        }
        if ignore != self.grid.ignores_unimportant() {
            self.grid.set_ignore_unimportant(ignore);
            self.revision += 1;
        }
        if hide_same != self.grid.hides_same_columns() {
            self.grid.set_hide_same_columns(hide_same);
        }
        if let Some(column) = unhide_column {
            self.grid.set_column_hidden(column, false);
        }
        self.show_row_numbers = row_numbers;
        self.show_strip = strip;
        self.show_details = details;
        match outcome.command {
            Some(Command::PreviousDifference) => self.go_to_previous_difference(),
            Some(Command::NextDifference) => self.go_to_next_difference(),
            Some(Command::SessionSettings) => self.panel.toggle(),
            Some(Command::Recompare) => self.recompare(),
            Some(Command::SwapSides) => self.run(Command::SwapSides),
            Some(Command::Cancel) => self.pipeline.cancel(),
            Some(Command::CompareReport) => self.report.request(),
            _ => {}
        }
    }

    /// The comparison the report is written from.
    #[must_use]
    ///
    /// The rows and the columns are the ones the grid is showing, so the
    /// display filter and the hidden columns both reach the document.
    pub fn report_payload(&self) -> (ca_ui::report::ReportMeta, ca_ui::report::Payload) {
        let meta = ca_ui::report::ReportMeta::new(
            self.left_path.display().to_string(),
            self.right_path.display().to_string(),
        )
        .with_title(ReportKind::Table.title());
        let source = self.grid.source();
        let shown = self.grid.shown_columns().to_vec();
        let header = ca_ui::report::TableHeader {
            sheet: None,
            columns: shown
                .iter()
                .filter_map(|column| source.columns().get(*column))
                .map(|column| column.name.clone())
                .collect(),
        };
        let mut rows = Vec::with_capacity(self.grid.visual_rows());
        for visual in 0..self.grid.visual_rows() {
            let Some(row) = self.grid.source_row(visual) else {
                continue;
            };
            let (left_number, right_number) = source.row_numbers(row);
            let cells = shown
                .iter()
                .map(|column| ca_ui::report::TableCell {
                    status: ca_ui::report::CellStatus::from(source.cell_status(row, *column)),
                    left: source.cell_text(row, *column, Side::Left).into_owned(),
                    right: source.cell_text(row, *column, Side::Right).into_owned(),
                })
                .collect();
            rows.push(ca_ui::report::TableRow {
                left_number: left_number.map(u64::from),
                right_number: right_number.map(u64::from),
                kind: ca_ui::report::RowKind::from(source.row_status(row)),
                importance: None,
                cells,
            });
        }
        (meta, ca_ui::report::Payload::Table(header, rows))
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

    fn compare_info_window(&mut self, ctx: &egui::Context) {
        if !self.info_open {
            return;
        }
        let source = self.grid.source();
        let rows = source.rows();
        let columns = source.columns().len();
        let totals = source.totals();
        let id = self.id;
        egui::Window::new(Command::CompareInfo.label_in(MenuView::Table))
            .id(id.with("compare-info"))
            .open(&mut self.info_open)
            .show(ctx, |ui| {
                ui.label(format!("Rows: {rows}"));
                ui.label(format!("Columns: {columns}"));
                ui.separator();
                ui.label("Rows");
                egui::Grid::new(id.with("compare-info-rows")).show(ui, |ui| {
                    for (label, count) in [
                        ("Same", totals.same),
                        ("Different", totals.different),
                        ("Unimportant", totals.unimportant),
                        ("Left only", totals.left_only),
                        ("Right only", totals.right_only),
                    ] {
                        ui.label(label);
                        ui.label(count.to_string());
                        ui.end_row();
                    }
                });
                ui.separator();
                ui.label("Cells");
                egui::Grid::new(id.with("compare-info-cells")).show(ui, |ui| {
                    for (label, count) in [
                        ("Same", totals.cells_same),
                        ("Different", totals.cells_different),
                        ("Unimportant", totals.cells_unimportant),
                        ("Left only", totals.cells_left_only),
                        ("Right only", totals.cells_right_only),
                    ] {
                        ui.label(label);
                        ui.label(count.to_string());
                        ui.end_row();
                    }
                });
            });
    }

    /// The name the report display filter carries for what the grid shows.
    #[must_use]
    pub fn report_filter(&self) -> &'static str {
        match self.grid.filter() {
            DisplayFilter::Differences => "mismatches",
            DisplayFilter::Same => "matches",
            _ => "all",
        }
    }

    /// The line under each path field, naming what the side was read as.
    fn file_info(&self, ui: &mut egui::Ui) {
        let sides: Vec<widgets::FileInfo> = [
            ("Left", true, self.clipboard_text[0].is_some()),
            ("Right", false, self.clipboard_text[1].is_some()),
        ]
        .into_iter()
        .map(|(label, is_left, is_clipboard)| {
            let facts = self
                .sides
                .as_ref()
                .map(|sides| if is_left { &sides.0 } else { &sides.1 })
                .map(|side| &side.facts);
            widgets::FileInfo {
                format: facts.map(|facts| facts.syntax.clone()),
                encoding: facts.map(|facts| facts.encoding.clone()),
                ..widgets::FileInfo::new(if is_clipboard { "Clipboard" } else { label })
            }
        })
        .collect();
        widgets::file_info_bar(ui, ca_ui::format::probed_offset().unwrap_or(0), &sides);
    }

    fn notice_bar(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        let notices = self.grid.source().notices();
        if notices.is_empty() || !self.show_notices {
            return;
        }
        egui::Frame::new()
            .fill(palette.notice_background)
            .inner_margin(egui::Margin::same(4))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(palette.notice_text, format!("{} notices", notices.len()));
                    if ui.button("Dismiss").clicked() {
                        self.show_notices = false;
                    }
                });
                for notice in notices {
                    ui.colored_label(palette.notice_text, notice);
                }
            });
    }

    fn details_area(&self, ui: &mut egui::Ui, palette: &Palette) {
        if !self.show_details {
            return;
        }
        let cursor = self.grid.cursor();
        let left = self.grid.cell_text(cursor.row, cursor.column, Side::Left);
        let right = self.grid.cell_text(cursor.row, cursor.column, Side::Right);
        egui::Frame::new()
            .fill(palette.details_background)
            .inner_margin(egui::Margin::same(4))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(palette.details_text, "Left");
                    ui.colored_label(palette.details_text, left.as_ref());
                });
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(palette.details_text, "Right");
                    ui.colored_label(palette.details_text, right.as_ref());
                });
            });
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            match &self.status {
                Status::Running(stage) => {
                    ui.label(stage.label());
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
                    let totals = self.grid.source().totals();
                    ui.label(format!(
                        "{} rows: {} same, {} differ, {} minor, {} left only, {} right only",
                        self.grid.source().rows(),
                        totals.same,
                        totals.different,
                        totals.unimportant,
                        totals.left_only,
                        totals.right_only
                    ));
                    ui.separator();
                    ui.label(format!("{} cells differ", totals.cells_with_differences()));
                }
            }
            ui.separator();
            ui.label(format!("{} rows shown", self.grid.visual_rows()));
            ui.separator();
            ui.label(format!(
                "{} side{}",
                self.active.label(),
                if self.is_modified(self.active) {
                    ", modified"
                } else {
                    ""
                }
            ));
            if let Some(message) = &self.message {
                ui.separator();
                ca_ui::widgets::notice_current(ui, ca_ui::icons::Icon::Info, 16.0, message);
            }
            if self.fit_requested || self.fit_job.is_some() {
                ui.separator();
                ui.label("Sizing columns to fit…");
            }
            if self.copy_job.is_some() {
                ui.separator();
                ui.label("Copying selected rows…");
            }
            ui.separator();
            let cursor = self.grid.cursor();
            let column_name = self
                .grid
                .column_at(cursor.column)
                .and_then(|column| self.grid.source().columns().get(column))
                .map_or_else(String::new, |info| info.name.clone());
            ui.label(format!(
                "Row {}, column {column_name}: {}",
                cursor.row.saturating_add(1),
                cell_status_label(self.current_cell_status())
            ));
        });
    }

    /// Where the parts of the comparison area sit this frame.
    fn layout(&self, full: egui::Rect) -> Areas {
        let strip_width = if self.show_strip { STRIP_WIDTH } else { 0.0 };
        let body_right = full.right() - SCROLLBAR - strip_width;
        let grids_bottom = full.bottom() - SCROLLBAR;
        let header = egui::Rect::from_min_max(
            egui::pos2(full.left(), full.top()),
            egui::pos2(body_right, full.top() + HEADER_HEIGHT),
        );
        let rows = egui::Rect::from_min_max(
            egui::pos2(full.left(), header.bottom()),
            egui::pos2(body_right, grids_bottom.max(header.bottom())),
        );
        let middle = f32::midpoint(full.left(), body_right);
        let left = egui::Rect::from_min_max(
            egui::pos2(full.left(), rows.top()),
            egui::pos2((middle - SEPARATOR / 2.0).max(full.left()), rows.bottom()),
        );
        let right = egui::Rect::from_min_max(
            egui::pos2((middle + SEPARATOR / 2.0).min(body_right), rows.top()),
            egui::pos2(body_right, rows.bottom()),
        );
        Areas {
            header,
            rows,
            left,
            right,
            strip: egui::Rect::from_min_max(
                egui::pos2(body_right, rows.top()),
                egui::pos2(body_right + strip_width, rows.bottom()),
            ),
            vertical_bar: egui::Rect::from_min_max(
                egui::pos2(full.right() - SCROLLBAR, rows.top()),
                egui::pos2(full.right(), rows.bottom()),
            ),
            horizontal_bar: egui::Rect::from_min_max(
                egui::pos2(full.left(), grids_bottom),
                egui::pos2(body_right, full.bottom()),
            ),
            gutter: if self.show_row_numbers {
                GUTTER_WIDTH
            } else {
                0.0
            },
        }
    }

    #[allow(clippy::too_many_lines)]
    fn grids(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        let full = ui.available_rect_before_wrap();
        if full.width() <= 0.0 || full.height() <= 0.0 {
            return;
        }
        let areas = self.layout(full);
        let response = ui.allocate_rect(full, egui::Sense::click_and_drag());
        let painter = ui.painter_at(full);
        painter.rect_filled(full, 0.0, palette.background);

        let total = self.grid.visual_rows();
        self.viewport_height = areas.rows.height();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let rows_in_view = (areas.rows.height() / self.row_height()).max(0.0) as usize;
        self.rows_in_view = rows_in_view;
        self.scroll
            .clamp(areas.rows.height(), self.row_height(), total);
        let range = self
            .scroll
            .visible(areas.rows.height(), self.row_height(), total);

        let cells_width = (areas.left.width() - areas.gutter).max(0.0);
        let content = self.grid.total_width();
        self.horizontal = ca_ui::scroll::clamp_horizontal(self.horizontal, content, cells_width);
        let columns = self.grid.visible_columns(self.horizontal, cells_width);

        self.header(ui, &painter, palette, &areas, &columns);
        for (side, pane) in [(Side::Left, areas.left), (Side::Right, areas.right)] {
            self.paint_pane(
                &painter,
                palette,
                side,
                pane,
                areas.gutter,
                &range,
                &columns,
            );
        }
        painter.line_segment(
            [
                egui::pos2(areas.left.right() + SEPARATOR / 2.0, areas.rows.top()),
                egui::pos2(areas.left.right() + SEPARATOR / 2.0, areas.rows.bottom()),
            ],
            egui::Stroke::new(1.0, palette.separator),
        );

        self.strip_ui(ui, &painter, palette, areas.strip, total);
        self.vertical_bar(ui, &painter, palette, areas.vertical_bar, total);
        self.horizontal_bar(ui, &painter, palette, areas.horizontal_bar, cells_width);
        self.column_grips(ui, areas.header, areas.gutter, &columns);
        self.pointer(ui, &response, &areas);
        if let Some(side) = self.cell_editor.as_ref().map(|editor| editor.side) {
            let pane = match side {
                Side::Left => areas.left,
                Side::Right => areas.right,
            };
            self.cell_editor_ui(ui, pane, areas.gutter);
        }

        if ui.rect_contains_pointer(areas.rows) {
            let delta = ui.input(|input| input.smooth_scroll_delta);
            if delta.y.abs() > f32::EPSILON {
                self.scroll
                    .scroll_by(-delta.y, areas.rows.height(), self.row_height(), total);
            }
            if delta.x.abs() > f32::EPSILON {
                self.horizontal = ca_ui::scroll::clamp_horizontal(
                    self.horizontal - delta.x,
                    content,
                    cells_width,
                );
            }
        }
        if response.has_focus() && !self.goto_open {
            let keys = ui.input(|input| {
                input
                    .events
                    .iter()
                    .filter_map(|event| match event {
                        egui::Event::Key {
                            key, pressed: true, ..
                        } => Some(*key),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            });
            for key in keys {
                if matches!(key, egui::Key::F2 | egui::Key::Enter) {
                    self.begin_cell_edit();
                } else {
                    self.navigate(key, rows_in_view.saturating_sub(1));
                }
            }
        }
    }

    fn header(
        &mut self,
        ui: &mut egui::Ui,
        painter: &egui::Painter,
        palette: &Palette,
        areas: &Areas,
        columns: &std::ops::Range<usize>,
    ) {
        painter.rect_filled(areas.header, 0.0, palette.header_background);
        let font = egui::FontId::proportional(self.font.points());
        let mut hide_column = None;
        for (pane_index, pane) in [areas.left, areas.right].into_iter().enumerate() {
            let origin = pane.left() + areas.gutter - self.horizontal;
            let clip = egui::Rect::from_min_max(
                egui::pos2(pane.left() + areas.gutter, areas.header.top()),
                egui::pos2(pane.right(), areas.header.bottom()),
            );
            let clipped = painter.with_clip_rect(clip);
            for display in columns.clone() {
                let Some(column) = self.grid.column_at(display) else {
                    continue;
                };
                let Some(info) = self.grid.source().columns().get(column) else {
                    continue;
                };
                let x = origin + self.grid.column_x(display);
                let width = self.grid.width(column);
                let cell = egui::Rect::from_min_size(
                    egui::pos2(x, areas.header.top()),
                    egui::vec2(width, areas.header.height()),
                );
                clipped.text(
                    cell.left_center() + egui::vec2(CELL_PAD, 0.0),
                    egui::Align2::LEFT_CENTER,
                    &info.name,
                    font.clone(),
                    palette.header_text,
                );
                let response = ui.interact(
                    cell.intersect(clip),
                    ui.id().with(("table-column-header", pane_index, column)),
                    egui::Sense::click(),
                );
                response.context_menu(|ui| {
                    if ui.button("Hide Column").clicked() {
                        hide_column = Some(column);
                        ui.close_menu();
                    }
                });
                if info.key {
                    ca_ui::icons::Icon::Key.paint_in_row(
                        &clipped,
                        cell.right_center() - egui::vec2(CELL_PAD + 8.0, 0.0),
                        cell.height(),
                        palette.key_marker,
                    );
                }
                if info.unimportant {
                    ca_ui::icons::Icon::StateUnimportant.paint_in_row(
                        &clipped,
                        cell.right_center() - egui::vec2(CELL_PAD + 26.0, 0.0),
                        cell.height(),
                        palette.unimportant_marker,
                    );
                }
                clipped.line_segment(
                    [cell.right_top(), cell.right_bottom()],
                    egui::Stroke::new(1.0, palette.grid_line),
                );
            }
        }
        if let Some(column) = hide_column {
            self.grid.set_column_hidden(column, true);
        }
        let _ = ui;
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_pane(
        &self,
        painter: &egui::Painter,
        palette: &Palette,
        side: Side,
        pane: egui::Rect,
        gutter: f32,
        rows: &std::ops::Range<usize>,
        columns: &std::ops::Range<usize>,
    ) {
        if pane.width() <= 0.0 {
            return;
        }
        let font = egui::FontId::proportional(self.font.points());
        let cells = egui::Rect::from_min_max(
            egui::pos2(pane.left() + gutter, pane.top()),
            egui::pos2(pane.right(), pane.bottom()),
        );
        let clipped = painter.with_clip_rect(cells);
        let origin = cells.left() - self.horizontal;
        let cursor = self.grid.cursor();
        if gutter > 0.0 {
            painter.rect_filled(
                egui::Rect::from_min_max(
                    pane.left_top(),
                    egui::pos2(pane.left() + gutter, pane.bottom()),
                ),
                0.0,
                palette.gutter_background,
            );
        }
        for visual in rows.clone() {
            let y = pane.top() + self.scroll.row_y(visual, self.row_height());
            let row_rect = egui::Rect::from_min_size(
                egui::pos2(pane.left(), y),
                egui::vec2(pane.width(), self.row_height()),
            );
            if visual % 2 == 1 {
                painter.rect_filled(row_rect, 0.0, palette.stripe);
            }
            let selected = self
                .grid
                .source_row(visual)
                .is_some_and(|row| self.selected_row(row, side));
            if visual == cursor.row || selected {
                painter.rect_filled(row_rect, 0.0, palette.selection);
            }
            if gutter > 0.0 {
                self.paint_gutter(painter, palette, side, pane, gutter, visual, y, &font);
            }
            for display in columns.clone() {
                let Some(column) = self.grid.column_at(display) else {
                    continue;
                };
                let x = origin + self.grid.column_x(display);
                let cell = egui::Rect::from_min_size(
                    egui::pos2(x, y),
                    egui::vec2(self.grid.width(column), self.row_height()),
                );
                let class = self.grid.cell_class(visual, display, side);
                let (background, text) = palette.cell(class);
                if class != CellClass::Same || visual == cursor.row {
                    clipped.rect_filled(cell, 0.0, background);
                }
                let value = self.grid.cell_text(visual, display, side);
                if !value.is_empty() {
                    let inner = clipped.with_clip_rect(cell.intersect(cells));
                    inner.text(
                        cell.left_center() + egui::vec2(CELL_PAD, 0.0),
                        egui::Align2::LEFT_CENTER,
                        value.as_ref(),
                        font.clone(),
                        text,
                    );
                }
                clipped.line_segment(
                    [cell.right_top(), cell.right_bottom()],
                    egui::Stroke::new(1.0, palette.grid_line),
                );
                if visual == cursor.row && display == cursor.column {
                    clipped.rect_stroke(
                        cell,
                        0.0,
                        egui::Stroke::new(2.0, palette.current_cell_border),
                        egui::StrokeKind::Inside,
                    );
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_gutter(
        &self,
        painter: &egui::Painter,
        palette: &Palette,
        side: Side,
        pane: egui::Rect,
        gutter: f32,
        visual: usize,
        y: f32,
        font: &egui::FontId,
    ) {
        let Some(row) = self.grid.source_row(visual) else {
            return;
        };
        let (left, right) = self.grid.source().row_numbers(row);
        let number = match side {
            Side::Left => left,
            Side::Right => right,
        };
        if let Some(number) = number {
            painter.text(
                egui::pos2(
                    pane.left() + gutter - MARKER * 3.0,
                    y + self.row_height() / 2.0,
                ),
                egui::Align2::RIGHT_CENTER,
                number.to_string(),
                font.clone(),
                palette.gutter_text,
            );
        }
        let class = model::row_class(self.grid.source().row_status(row));
        let class = if self.grid.ignores_unimportant() && class == CellClass::Unimportant {
            CellClass::Same
        } else {
            class
        };
        if let Some(spot) = palette.spot(class) {
            painter.circle_filled(
                egui::pos2(pane.left() + gutter - MARKER, y + self.row_height() / 2.0),
                MARKER / 2.0,
                spot,
            );
        }
    }

    fn strip_ui(
        &mut self,
        ui: &mut egui::Ui,
        painter: &egui::Painter,
        palette: &Palette,
        rect: egui::Rect,
        total: usize,
    ) {
        if !self.show_strip || rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        painter.rect_filled(rect, 0.0, palette.thumbnail_background);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let pixels = rect.height().max(0.0) as usize;
        let key = (pixels, self.revision, total);
        if self.strip_key != key {
            self.strip = thumbnail::build(&self.grid, pixels);
            self.strip_key = key;
        }
        for pixel in 0..self.strip.pixels() {
            let Some(class) = self.strip.class_at(pixel) else {
                continue;
            };
            let (color, _) = palette.cell(class);
            #[allow(clippy::cast_precision_loss)]
            let y = rect.top() + pixel as f32;
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(rect.left(), y),
                    egui::vec2(rect.width(), 1.0),
                ),
                0.0,
                color,
            );
        }
        let marker = self
            .strip
            .viewport_marker(self.scroll.first_row(), self.rows_in_view);
        painter.rect_stroke(
            egui::Rect::from_min_max(
                egui::pos2(rect.left(), rect.top() + marker.start),
                egui::pos2(rect.right(), rect.top() + marker.end),
            ),
            0.0,
            egui::Stroke::new(1.0, palette.thumbnail_marker),
            egui::StrokeKind::Inside,
        );
        let response = ui.interact(rect, self.id.with("strip"), egui::Sense::click_and_drag());
        if let Some(pointer) = response.interact_pointer_pos() {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let pixel = (pointer.y - rect.top()).max(0.0) as usize;
            let row = self.strip.row_at(pixel);
            self.scroll
                .center_on(row, self.viewport_height, self.row_height(), total);
        }
    }

    fn vertical_bar(
        &mut self,
        ui: &mut egui::Ui,
        painter: &egui::Painter,
        palette: &Palette,
        track: egui::Rect,
        total: usize,
    ) {
        if track.height() <= 0.0 {
            return;
        }
        painter.rect_filled(track, 0.0, palette.gutter_background);
        let thumb = self.scroll.thumb(
            track.height(),
            self.viewport_height,
            self.row_height(),
            total,
        );
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
            self.scroll = RowScroll::from_thumb(
                top,
                track.height(),
                self.viewport_height,
                self.row_height(),
                total,
            );
        }
    }

    fn horizontal_bar(
        &mut self,
        ui: &mut egui::Ui,
        painter: &egui::Painter,
        palette: &Palette,
        track: egui::Rect,
        viewport: f32,
    ) {
        let moved = widgets::horizontal_scrollbar(
            ui,
            painter,
            self.id.with("horizontal-bar"),
            track,
            (palette.gutter_background, palette.thumbnail_marker),
            (self.grid.total_width(), viewport, self.horizontal),
            SCROLLBAR,
        );
        if let Some(offset) = moved {
            self.horizontal = offset;
        }
    }

    /// The draggable edges between the columns, taken from the left grid.
    fn column_grips(
        &mut self,
        ui: &mut egui::Ui,
        header: egui::Rect,
        gutter: f32,
        columns: &std::ops::Range<usize>,
    ) {
        let origin = header.left() + gutter - self.horizontal;
        for display in columns.clone() {
            let Some(column) = self.grid.column_at(display) else {
                continue;
            };
            let edge = origin + self.grid.column_x(display) + self.grid.width(column);
            if edge < header.left() || edge > header.right() {
                continue;
            }
            let grip = egui::Rect::from_min_max(
                egui::pos2(edge - GRIP, header.top()),
                egui::pos2(edge + GRIP, header.bottom()),
            );
            let response = ui.interact(
                grip,
                self.id.with(("grip", column)),
                egui::Sense::click_and_drag(),
            );
            if response.hovered() || response.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
            let moved = response.drag_delta().x;
            if moved.abs() > f32::EPSILON {
                self.grid.set_width(column, self.grid.width(column) + moved);
            }
        }
    }

    /// Put the current cell where the pointer clicked.
    fn pointer(&mut self, ui: &egui::Ui, response: &egui::Response, areas: &Areas) {
        if !response.clicked() {
            return;
        }
        response.request_focus();
        let Some(pointer) = ui.ctx().pointer_interact_pos() else {
            return;
        };
        if !areas.rows.contains(pointer) {
            return;
        }
        let (pane, side) = if pointer.x <= areas.left.right() {
            (areas.left, Side::Left)
        } else {
            (areas.right, Side::Right)
        };
        self.set_active_side(side);
        let x = pointer.x - pane.left() - areas.gutter + self.horizontal;
        let Some(column) = self.grid.column_at_x(x) else {
            return;
        };
        let offset = pointer.y - areas.rows.top() + self.scroll.pixel_shift(self.row_height());
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let step = (offset / self.row_height()).max(0.0) as usize;
        let row = self.scroll.first_row().saturating_add(step);
        self.set_grid_cursor(Cursor { row, column });
    }
}

fn parse_table_address(
    text: &str,
    rows: usize,
    columns: usize,
    current_column: usize,
) -> Result<Cursor, &'static str> {
    let mut parts = text.trim().split(',');
    let row_text = parts.next().unwrap_or_default().trim();
    let Some(row) = row_text.parse::<usize>().ok().filter(|row| *row > 0) else {
        return Err("Enter a row number greater than zero.");
    };
    if row > rows {
        return Err("That row is outside the rows currently shown.");
    }
    let column = match parts.next() {
        Some(text) => {
            if parts.next().is_some() {
                return Err("Use row or row,column.");
            }
            let Some(column) = text
                .trim()
                .parse::<usize>()
                .ok()
                .filter(|column| *column > 0)
            else {
                return Err("Enter a column number greater than zero.");
            };
            column
        }
        None => current_column.saturating_add(1),
    };
    if column > columns {
        return Err("That column is outside the columns currently shown.");
    }
    Ok(Cursor {
        row: row - 1,
        column: column - 1,
    })
}

fn copy_cell_sides(command: Command, active: Side) -> Option<(Side, Side)> {
    match command {
        Command::CopyCellToRight => Some((Side::Left, Side::Right)),
        Command::CopyCellToLeft => Some((Side::Right, Side::Left)),
        Command::CopyCellToOtherSide => Some((active, active.other())),
        _ => None,
    }
}

/// Where the parts of the comparison area sit.
struct Areas {
    header: egui::Rect,
    rows: egui::Rect,
    left: egui::Rect,
    right: egui::Rect,
    strip: egui::Rect,
    vertical_bar: egui::Rect,
    horizontal_bar: egui::Rect,
    gutter: f32,
}

/// What the status bar calls a cell status.
#[must_use]
pub const fn cell_status_label(status: Option<CellStatus>) -> &'static str {
    match status {
        Some(CellStatus::Same) => "same",
        Some(CellStatus::Unimportant) => "differs, minor",
        Some(CellStatus::LeftOnly) => "left only",
        Some(CellStatus::RightOnly) => "right only",
        Some(_) => "differs",
        None => "no cell",
    }
}

/// What the status bar calls a row status.
#[must_use]
pub const fn row_status_label(status: RowStatus) -> &'static str {
    match status {
        RowStatus::Same => "same",
        RowStatus::Unimportant => "differs, minor",
        RowStatus::LeftOnly => "left only",
        RowStatus::RightOnly => "right only",
        _ => "differs",
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

impl ca_ui::view::ViewFactory for TableView {
    fn create(left: PathBuf, right: PathBuf, context: &ViewContext, instance: u64) -> Self {
        Self::new(left, right, context, instance)
    }

    fn create_from(
        request: &ca_ui::view::OpenRequest,
        context: &ViewContext,
        instance: u64,
    ) -> Self {
        let mut view = Self::new(
            request.left.clone(),
            request.right.clone(),
            context,
            instance,
        );
        view.read_only = request.read_only;
        view
    }
}

impl SessionView for TableView {
    fn kind(&self) -> Option<ca_session::SessionKind> {
        Some(ca_session::SessionKind::TableCompare)
    }

    fn title(&self) -> String {
        let left = if self.clipboard_text[0].is_some() {
            "Clipboard".to_owned()
        } else {
            file_name(&self.left_path)
        };
        let right = if self.clipboard_text[1].is_some() {
            "Clipboard".to_owned()
        } else {
            file_name(&self.right_path)
        };
        format!("{left} - {right}")
    }

    fn menu_view(&self) -> MenuView {
        MenuView::Table
    }

    fn tick(&mut self) {
        self.poll();
    }

    fn set_active(&mut self, active: bool) {
        if !active {
            self.clipboard_requested = None;
            self.clipboard_asked = None;
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _context: &ViewContext) -> Vec<ViewAction> {
        let palette = Self::palette(ui);
        self.pixels_per_point = ui.ctx().pixels_per_point();
        self.follow_options(ui.ctx());
        let filter_name = self.report_filter();
        self.report.poll_with(ui.ctx(), |settings| {
            settings.select_filter(filter_name);
        });
        self.poll();
        self.start_auto_fit(ui.ctx());
        self.clipboard_events(ui.ctx());
        self.path_bar(ui);
        self.file_info(ui);
        self.toolbar(ui);
        self.find_panel(ui);
        self.goto_panel(ui);
        self.prompt_panel(ui);
        self.report_panel(ui);
        self.compare_info_window(ui.ctx());
        self.notice_bar(ui, palette);
        ui.separator();
        if self.panel.is_open() {
            let columns = self.grid.source().columns().to_vec();
            let mut settings = self.settings.clone();
            let changed = self.panel.show(ui, self.id, &mut settings, &columns);
            self.settings = settings;
            if changed {
                self.recompare();
            }
            return Vec::new();
        }
        let reserved = if self.show_details { 54.0 } else { 0.0 };
        let status_height = 26.0;
        let room = (ui.available_height() - reserved - status_height).max(self.row_height() * 2.0);
        ui.allocate_ui(egui::vec2(ui.available_width(), room), |ui| {
            self.grids(ui, palette);
        });
        self.details_area(ui, palette);
        ui.separator();
        self.status_bar(ui);
        Vec::new()
    }

    fn commands(&self) -> Vec<ca_ui::view::CommandState> {
        ca_ui::view::declare(HANDLED, |command| self.accepts(command))
    }

    fn accepts(&self, command: Command) -> bool {
        let ready = self.has_comparison();
        match command {
            Command::OpenFile => {
                !self.status.is_running()
                    && self.picker.is_none()
                    && self.cell_editor.is_none()
                    && !self.clipboard_request_pending()
            }
            Command::OpenClipboard => self.can_open_clipboard(),
            Command::ShowAll
            | Command::ShowDifferences
            | Command::ShowSame
            | Command::ShowNone
            | Command::ToggleIgnoreUnimportant
            | Command::HideSameColumns
            | Command::ToggleLineNumbers
            | Command::Thumbnail
            | Command::ToggleLineDetails
            | Command::CompareReport
            | Command::CompareInfo
            | Command::Find
            | Command::FindNext
            | Command::FindPrevious => ready,
            Command::ResizeColumnsToFit => ready && self.fit_job.is_none() && !self.fit_requested,
            Command::Copy => {
                ready
                    && self.cell_editor.is_none()
                    && self.copy_job.is_none()
                    && self.grid.visual_rows() > 0
                    && !self.grid.shown_columns().is_empty()
            }
            Command::CopyCellToRight => self.can_copy_cell(Side::Left, Side::Right),
            Command::CopyCellToLeft => self.can_copy_cell(Side::Right, Side::Left),
            Command::CopyCellToOtherSide => self.can_copy_cell(self.active, self.active.other()),
            Command::Cut | Command::Paste | Command::Delete => {
                self.cell_editor.is_none() && self.can_edit_cell()
            }
            Command::GoTo => {
                ready && self.grid.visual_rows() > 0 && !self.grid.shown_columns().is_empty()
            }
            Command::NextDifference
            | Command::PreviousDifference
            | Command::NextSection
            | Command::PreviousSection
            | Command::SelectAll => ready && self.grid.visual_rows() > 0,
            Command::Reload | Command::SwapSides => {
                !self.status.is_running() && !self.clipboard_request_pending()
            }
            Command::Undo => {
                !self.status.is_running()
                    && self.write_block(self.active).is_none()
                    && self.side_text(self.active).can_undo()
            }
            Command::Redo => {
                !self.status.is_running()
                    && self.write_block(self.active).is_none()
                    && self.side_text(self.active).can_redo()
            }
            Command::SaveFile => {
                self.is_modified(self.active)
                    && self.save_job.is_none()
                    && self.write_block(self.active).is_none()
            }
            Command::SaveFileAs => {
                ready
                    && self.save_job.is_none()
                    && self.picker.is_none()
                    && self.write_block(self.active).is_none()
            }
            Command::SaveBoth => {
                (self.is_modified(Side::Left) || self.is_modified(Side::Right))
                    && self.save_job.is_none()
                    && !self.specs.disable_editing
            }
            Command::Replace => ready && self.write_block(self.active).is_none(),
            Command::Recompare => {
                !self.status.is_running()
                    && self.sides.is_some()
                    && !self.clipboard_request_pending()
            }
            Command::Cancel => {
                self.status.is_running()
                    || self.fit_job.is_some()
                    || self.fit_requested
                    || self.copy_job.is_some()
                    || self.clipboard_requested.is_some()
                    || self.clipboard_asked.is_some()
            }
            _ => false,
        }
    }

    fn run(&mut self, command: Command) {
        if let Some((source, target)) = copy_cell_sides(command, self.active) {
            self.copy_cell_from_to(source, target);
            return;
        }
        match command {
            Command::OpenFile => self.open_picker(match self.active {
                Side::Left => Target::Left,
                Side::Right => Target::Right,
            }),
            Command::OpenClipboard => self.clipboard_requested = Some(self.active),
            Command::CompareReport => self.report.request(),
            Command::CompareInfo => self.info_open = true,
            Command::ShowAll => self.set_filter(DisplayFilter::All),
            Command::ShowDifferences => self.set_filter(DisplayFilter::Differences),
            Command::ShowSame => self.set_filter(DisplayFilter::Same),
            Command::ShowNone => self.set_filter(DisplayFilter::None),
            Command::ToggleIgnoreUnimportant => {
                self.clear_selection();
                self.grid
                    .set_ignore_unimportant(!self.grid.ignores_unimportant());
                self.revision += 1;
                self.clamp_scroll();
            }
            Command::HideSameColumns => {
                self.grid
                    .set_hide_same_columns(!self.grid.hides_same_columns());
            }
            Command::ToggleLineNumbers => self.show_row_numbers = !self.show_row_numbers,
            Command::Thumbnail => self.show_strip = !self.show_strip,
            Command::ToggleLineDetails => self.show_details = !self.show_details,
            Command::ResizeColumnsToFit => self.fit_requested = true,
            Command::NextDifference => self.go_to_next_difference(),
            Command::PreviousDifference => self.go_to_previous_difference(),
            Command::NextSection => {
                if let Some(row) = self.grid.next_section(self.grid.cursor().row) {
                    self.set_grid_cursor(Cursor {
                        row,
                        column: self.grid.cursor().column,
                    });
                    self.reveal_cursor();
                }
            }
            Command::PreviousSection => {
                if let Some(row) = self.grid.previous_section(self.grid.cursor().row) {
                    self.set_grid_cursor(Cursor {
                        row,
                        column: self.grid.cursor().column,
                    });
                    self.reveal_cursor();
                }
            }
            Command::Reload => self.request_reread(ca_ui::save::Reread::Reload),
            Command::Recompare => self.recompare(),
            Command::SwapSides => self.request_reread(ca_ui::save::Reread::SwapSides),
            Command::Cancel => {
                self.clipboard_requested = None;
                self.clipboard_asked = None;
                self.pipeline.cancel();
                self.cancel_auto_fit();
                if let Some(job) = self.search_job.take() {
                    job.cancel();
                }
                if let Some(job) = self.replace_job.take() {
                    job.cancel();
                }
                self.cancel_selection_copy();
                self.cell_editor = None;
                self.find.close();
            }
            Command::Undo => self.undo(),
            Command::Redo => self.redo(),
            Command::Copy => self.copy_current_to_clipboard(),
            Command::SelectAll => self.select_all_visible_rows(),
            Command::Cut => self.cut_current_cell(),
            Command::Paste => {
                self.paste_requested = true;
                self.paste_asked = false;
            }
            Command::Delete => self.clear_current_cell(),
            Command::SaveFile => {
                let side = self.active;
                self.save_side(side, ca_ui::save::text::SaveConsent::default());
            }
            Command::SaveFileAs => self.open_save_picker(),
            Command::SaveBoth => self.save_next_modified(),
            Command::Find => self.find.open_find(),
            Command::Replace => self.find.open_replace(),
            Command::FindNext => self.find_next(false),
            Command::FindPrevious => self.find_next(true),
            Command::GoTo => {
                let cursor = self.grid.cursor();
                self.goto_text = format!("{},{}", cursor.row + 1, cursor.column + 1);
                self.goto_open = true;
                self.goto_focus = true;
            }
            _ => {}
        }
    }

    fn wants_close(&self) -> bool {
        self.close_requested
            && !self.is_modified(Side::Left)
            && !self.is_modified(Side::Right)
            && self.save_job.is_none()
    }

    fn is_busy(&self) -> bool {
        self.save_job.is_some()
    }

    fn may_close(&mut self) -> bool {
        if matches!(self.prompt, Some(editing::Prompt::KeptCopy(_))) || self.save_job.is_some() {
            return false;
        }
        if !ca_ui::save::needs_close_prompt(
            self.is_modified(Side::Left),
            self.is_modified(Side::Right),
        ) {
            return true;
        }
        self.prompt = Some(editing::Prompt::Closing);
        false
    }

    fn apply_settings(&mut self, settings: &ca_session::settings::SessionSettings) {
        let ca_session::settings::SessionSettings::TableCompare(table) = settings else {
            return;
        };
        self.specs.clone_from(&table.specs);
        let built = crate::session_options::options_over(table, &self.settings);
        // A format change decides how the bytes are decoded and split, which
        // happens before the tables exist, so it needs the files read again.
        let reread = built.left_format.parse != self.settings.left_format.parse
            || built.right_format.parse != self.settings.right_format.parse
            || built.left_format.encoding != self.settings.left_format.encoding
            || built.right_format.encoding != self.settings.right_format.encoding;
        if !reread && built.schema == self.settings.schema && built.align == self.settings.align {
            return;
        }
        if reread {
            // Reading the files again drops every edit, so it waits on the
            // question a close asks, and the settings apply after the answer.
            self.pending_settings = Some(Box::new(built));
            self.request_reread(ca_ui::save::Reread::Settings);
        } else {
            self.settings = built;
            self.recompare();
        }
    }

    fn settings(&self) -> Option<ca_session::settings::SessionSettings> {
        let mut settings = crate::session_options::stored_from(&self.settings);
        settings.specs = ca_ui::view::with_sides(&self.specs, &self.left_path, &self.right_path);
        Some(ca_session::settings::SessionSettings::TableCompare(
            settings,
        ))
    }

    fn launch_target(&self) -> Option<ca_ui::launch::LaunchTarget> {
        use ca_session::options::{LaunchContext, LaunchSide};
        if self.left_path.as_os_str().is_empty() || self.clipboard_text.iter().any(Option::is_some)
        {
            return None;
        }
        let side = |path: &Path| LaunchSide {
            path: path.to_path_buf(),
            base: path.parent().map(Path::to_path_buf),
            line: None,
        };
        Some(ca_ui::launch::LaunchTarget::files(LaunchContext {
            first: side(&self.left_path),
            second: (!self.right_path.as_os_str().is_empty()).then(|| side(&self.right_path)),
        }))
    }

    fn explorer_target(&self) -> Option<(PathBuf, ca_ui::launch::Selection)> {
        if self.clipboard_text[match self.active {
            Side::Left => 0,
            Side::Right => 1,
        }]
        .is_some()
        {
            return None;
        }
        let path = match self.active {
            Side::Left => &self.left_path,
            Side::Right => &self.right_path,
        };
        (!path.as_os_str().is_empty()).then(|| (path.clone(), ca_ui::launch::Selection::Files))
    }

    fn holds_unwritten_edits(&self) -> bool {
        self.is_modified(Side::Left) || self.is_modified(Side::Right)
    }

    fn is_ready(&self) -> bool {
        !self.status.is_running()
    }

    fn notice(&self) -> Option<String> {
        match &self.status {
            Status::Failed(reason) => Some(reason.clone()),
            _ => None,
        }
    }

    fn holds_temporaries(&self) -> bool {
        self.clipboard_text.iter().any(Option::is_some)
    }

    fn on_close(&mut self) {
        self.pipeline.stop();
        self.clipboard_requested = None;
        self.clipboard_asked = None;
        self.cancel_auto_fit();
        self.cancel_selection_copy();
        self.picker = None;
        if let Some(job) = self.search_job.take() {
            job.cancel();
        }
        if let Some(job) = self.replace_job.take() {
            job.cancel();
        }
        if let Some(job) = self.save_job.take() {
            job.cancel();
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{cell_status_label, parse_table_address, MenuView, TableView, ROW_HEIGHT};
    use crate::model::{Cursor, DisplayFilter, Side, Source};
    use crate::testing::FakeSource;
    use ca_table::compare::{CellStatus, RowStatus};
    use ca_ui::command::Command;
    use ca_ui::testing::{context, event_input, sized_input, wait_until};
    use ca_ui::view::SessionView;
    use std::fmt::Write as _;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// Run one frame of a view over a window of the stated width.
    fn frame(view: &mut TableView, ctx: &egui::Context, width: f32, height: f32) {
        let _ = ctx.run(sized_input(width, height), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                view.ui(ui, &context());
            });
        });
    }

    fn clipboard_frame(
        view: &mut TableView,
        ctx: &egui::Context,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        ctx.run(event_input(1_280.0, 800.0, events), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                view.ui(ui, &context());
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

    fn drawn_text(output: &egui::FullOutput) -> Vec<String> {
        output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some(text.galley.text().to_owned()),
                _ => None,
            })
            .collect()
    }

    fn view_over(source: Arc<dyn crate::model::Source>) -> TableView {
        let mut view = TableView::new(
            PathBuf::from("left.csv"),
            PathBuf::from("right.csv"),
            &context(),
            7,
        );
        view.set_source(source);
        view
    }

    #[test]
    fn compare_info_command_opens_a_window_for_a_finished_comparison() {
        let mut view = TableView::new(
            PathBuf::from("left.csv"),
            PathBuf::from("right.csv"),
            &context(),
            18,
        );
        assert!(!view.accepts(Command::CompareInfo));
        let source = FakeSource::new(
            &[RowStatus::Same, RowStatus::Different, RowStatus::LeftOnly],
            2,
        );
        let rows = source.rows();
        let columns = source.columns().len();
        let totals = source.totals();
        view.set_source(Arc::new(source));
        assert!(view.accepts(Command::CompareInfo));
        view.run(Command::CompareInfo);

        let ctx = egui::Context::default();
        let _ = ctx.run(sized_input(640.0, 420.0), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                view.ui(ui, &context());
            });
        });
        let output = ctx.run(sized_input(640.0, 420.0), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                view.ui(ui, &context());
            });
        });
        let text = drawn_text(&output);
        assert!(
            text.contains(&format!("Rows: {rows}")),
            "Compare Info omitted row count {rows}; painted text: {text:?}"
        );
        assert!(
            text.contains(&format!("Columns: {columns}")),
            "Compare Info omitted column count {columns}; painted text: {text:?}"
        );
        for (label, count) in [
            ("Same", totals.same),
            ("Different", totals.different),
            ("Unimportant", totals.unimportant),
            ("Left only", totals.left_only),
            ("Right only", totals.right_only),
            ("Same", totals.cells_same),
            ("Different", totals.cells_different),
            ("Unimportant", totals.cells_unimportant),
            ("Left only", totals.cells_left_only),
            ("Right only", totals.cells_right_only),
        ] {
            let count = count.to_string();
            assert!(
                text.windows(2)
                    .any(|pair| pair[0] == label && pair[1] == count),
                "Compare Info did not show {label}: {count}; text was {text:?}"
            );
        }
        let area = ctx
            .memory(|memory| memory.area_rect(view.id.with("compare-info")))
            .expect("Compare Info should open its window");
        assert!(ctx.screen_rect().contains_rect(area));
    }

    #[test]
    fn resize_columns_to_fit_runs_in_the_background_and_updates_the_grid() {
        let long = "a value much wider than the default column ".repeat(20);
        let source = FakeSource::new(&[RowStatus::Same], 1)
            .with_text(0, 0, Side::Left, &long)
            .with_text(0, 0, Side::Right, "short");
        let mut view = view_over(Arc::new(source));
        let ctx = egui::Context::default();
        frame(&mut view, &ctx, 1_280.0, 800.0);
        let initial = view.grid().width(0);

        assert!(view.accepts(Command::ResizeColumnsToFit));
        assert!(view
            .commands()
            .iter()
            .any(|state| state.command == Command::ResizeColumnsToFit));
        view.run(Command::ResizeColumnsToFit);
        frame(&mut view, &ctx, 1_280.0, 800.0);
        assert!(view.fit_job.is_some());
        assert!(view.accepts(Command::Cancel));
        assert!(wait_until(Duration::from_secs(5), || {
            view.poll();
            view.fit_job.is_none()
        }));
        assert!(view.grid().width(0) > initial);
        assert!(!view.accepts(Command::Cancel));
    }

    #[test]
    fn table_view_commands_toggle_its_existing_display_controls() {
        let source = FakeSource::new(&[RowStatus::Same], 2);
        let mut view = view_over(Arc::new(source));
        assert_eq!(view.menu_view(), MenuView::Table);

        view.run(Command::ToggleIgnoreUnimportant);
        assert!(view.grid().ignores_unimportant());
        view.run(Command::HideSameColumns);
        assert!(view.grid().hides_same_columns());
        view.run(Command::ToggleLineNumbers);
        assert!(!view.show_row_numbers);
        view.run(Command::Thumbnail);
        assert!(!view.show_strip);
        view.run(Command::ToggleLineDetails);
        assert!(!view.show_details);
    }

    #[test]
    fn cancelling_column_sizing_discards_its_result() {
        let source = FakeSource::repeating(100_000, 2, &[RowStatus::Same]);
        let mut view = view_over(Arc::new(source));
        let ctx = egui::Context::default();
        frame(&mut view, &ctx, 1_280.0, 800.0);
        let initial = view.grid().width(0);
        view.run(Command::ResizeColumnsToFit);
        frame(&mut view, &ctx, 1_280.0, 800.0);
        assert!(view.fit_job.is_some());

        view.run(Command::Cancel);
        assert!(view.fit_job.is_none());
        assert!((view.grid().width(0) - initial).abs() < f32::EPSILON);
    }

    #[test]
    fn changing_comparisons_cancels_column_sizing() {
        let old = FakeSource::repeating(100_000, 1, &[RowStatus::Same]).with_text(
            99_999,
            0,
            Side::Left,
            &"a late wide cell ".repeat(100),
        );
        let mut view = view_over(Arc::new(old));
        let ctx = egui::Context::default();
        frame(&mut view, &ctx, 1_280.0, 800.0);
        view.run(Command::ResizeColumnsToFit);
        frame(&mut view, &ctx, 1_280.0, 800.0);
        assert!(view.fit_job.is_some());

        view.set_source(Arc::new(FakeSource::new(&[RowStatus::Same], 1)));
        assert!(view.fit_job.is_none());
        assert!((view.grid().width(0) - crate::model::DEFAULT_COLUMN).abs() < f32::EPSILON);
    }

    fn files(left: &str, right: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let left_path = dir.path().join("left.csv");
        let right_path = dir.path().join("right.csv");
        std::fs::write(&left_path, left).unwrap();
        std::fs::write(&right_path, right).unwrap();
        (dir, left_path, right_path)
    }

    #[test]
    fn open_with_target_names_both_table_files() {
        let (_dir, left, right) = files("Name,Value\nA,1\n", "Name,Value\nA,2\n");
        let mut view = TableView::new(left.clone(), right.clone(), &context(), 911);

        let target = view.launch_target().unwrap();

        assert_eq!(target.selection, ca_ui::launch::Selection::Files);
        assert_eq!(target.context.first.path, left);
        assert_eq!(
            target.context.first.base,
            left.parent().map(std::path::Path::to_path_buf)
        );
        assert_eq!(target.context.second.unwrap().path, right);

        view.set_active_side(Side::Right);
        assert_eq!(
            ca_ui::view::SessionView::explorer_target(&view),
            Some((right, ca_ui::launch::Selection::Files))
        );
    }

    /// Run frames until the view has a comparison or the budget runs out.
    fn settle(view: &mut TableView, ctx: &egui::Context) -> bool {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            frame(view, ctx, 1_280.0, 800.0);
            if view.has_comparison() || view.failure().is_some() {
                return view.has_comparison();
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        false
    }

    #[test]
    fn the_title_uses_both_file_names() {
        let view = view_over(Arc::new(FakeSource::new(&[RowStatus::Same], 1)));
        assert_eq!(view.title(), "left.csv - right.csv");
    }

    #[test]
    fn go_to_accepts_one_based_row_and_column_or_keeps_the_column() {
        assert_eq!(
            parse_table_address("4,2", 10, 5, 0),
            Ok(Cursor { row: 3, column: 1 })
        );
        assert_eq!(
            parse_table_address("4", 10, 5, 2),
            Ok(Cursor { row: 3, column: 2 })
        );
        assert_eq!(
            parse_table_address("0,1", 10, 5, 0),
            Err("Enter a row number greater than zero.")
        );
        assert_eq!(
            parse_table_address("11,1", 10, 5, 0),
            Err("That row is outside the rows currently shown.")
        );
        assert_eq!(
            parse_table_address("4,6", 10, 5, 0),
            Err("That column is outside the columns currently shown.")
        );
        assert_eq!(
            parse_table_address("4,2,1", 10, 5, 0),
            Err("Use row or row,column.")
        );
    }

    #[test]
    fn go_to_command_moves_to_a_cell_and_keeps_invalid_addresses_open() {
        let mut view = view_over(Arc::new(FakeSource::new(&[RowStatus::Same; 5], 3)));
        assert!(view.accepts(Command::GoTo));
        view.run(Command::GoTo);
        assert!(view.goto_open);
        view.goto_text = "4,2".to_owned();
        view.go_to_address();
        assert_eq!(view.grid().cursor(), Cursor { row: 3, column: 1 });
        assert_eq!(view.message, None);

        view.goto_text = "8,1".to_owned();
        view.go_to_address();
        assert_eq!(
            view.message.as_deref(),
            Some("That row is outside the rows currently shown.")
        );
        assert!(view.goto_open);
    }

    #[test]
    fn cell_clipboard_commands_copy_cut_paste_and_delete_the_active_cell() {
        let (_dir, left, right) = files("id,name\n1,Ann\n2,Bob\n", "id,name\n1,Ann\n2,Bob\n");
        let ctx = egui::Context::default();
        let mut view = TableView::new(left, right, &context(), 31);
        assert!(settle(&mut view, &ctx));
        view.set_active_side(Side::Left);
        view.grid.set_cursor(Cursor { row: 0, column: 1 });
        assert!(view.accepts(Command::Copy));
        assert!(view.accepts(Command::Cut));
        assert!(view.accepts(Command::Paste));
        assert!(view.accepts(Command::Delete));

        view.run(Command::Copy);
        assert_eq!(
            copied(&clipboard_frame(&mut view, &ctx, Vec::new())),
            ["Ann"]
        );

        view.run(Command::Cut);
        assert_eq!(
            copied(&clipboard_frame(&mut view, &ctx, Vec::new())),
            ["Ann"]
        );
        assert!(view.is_modified(Side::Left));
        assert!(settle(&mut view, &ctx));
        assert_eq!(view.grid.cell_text(0, 1, Side::Left), "");

        view.run(Command::Paste);
        let output = clipboard_frame(&mut view, &ctx, Vec::new());
        assert!(output.viewport_output.values().any(|viewport| {
            viewport
                .commands
                .contains(&egui::ViewportCommand::RequestPaste)
        }));
        clipboard_frame(&mut view, &ctx, vec![egui::Event::Paste("Zoe".to_owned())]);
        assert!(settle(&mut view, &ctx));
        assert_eq!(view.grid.cell_text(0, 1, Side::Left), "Zoe");

        view.run(Command::Delete);
        assert!(settle(&mut view, &ctx));
        assert_eq!(view.grid.cell_text(0, 1, Side::Left), "");
    }

    #[test]
    fn open_clipboard_loads_a_read_only_transient_table_on_the_active_side() {
        let (_dir, left, right) = files("id,name\n1,Ann\n", "id,name\n1,Bob\n");
        let ctx = egui::Context::default();
        let mut view = TableView::new(left.clone(), right, &context(), 35);
        assert!(settle(&mut view, &ctx));
        view.set_active_side(Side::Left);

        assert!(view.accepts(Command::OpenClipboard));
        view.run(Command::OpenClipboard);
        let requested = clipboard_frame(&mut view, &ctx, Vec::new());
        assert!(requested.viewport_output.values().any(|viewport| {
            viewport
                .commands
                .contains(&egui::ViewportCommand::RequestPaste)
        }));
        clipboard_frame(
            &mut view,
            &ctx,
            vec![egui::Event::Paste("id,name\n1,Zoe\n".to_owned())],
        );
        assert!(settle(&mut view, &ctx));

        assert_eq!(view.grid.cell_text(0, 1, Side::Left), "Zoe");
        assert_eq!(view.grid.cell_text(0, 1, Side::Right), "Bob");
        assert_eq!(view.title(), "Clipboard - right.csv");
        assert_eq!(view.left_path, left);
        assert!(view.clipboard_text.iter().any(Option::is_some));
        assert!(view.holds_temporaries());
        assert!(!view.accepts(Command::SaveFile));
        assert!(!view.accepts(Command::SaveFileAs));
        assert!(!view.accepts(Command::Undo));
        assert_eq!(
            view.write_block(Side::Right),
            Some("Clipboard text is a temporary input and cannot be edited or saved.")
        );
        assert_eq!(
            SessionView::settings(&view).unwrap().kind(),
            ca_session::SessionKind::TableCompare
        );
    }

    #[test]
    fn open_clipboard_without_text_stops_waiting_and_ignores_a_later_paste() {
        let (_dir, left, right) = files("id,name\n1,Ann\n", "id,name\n1,Bob\n");
        let ctx = egui::Context::default();
        let mut view = TableView::new(left.clone(), right, &context(), 36);
        assert!(settle(&mut view, &ctx));
        view.set_active_side(Side::Left);
        view.run(Command::OpenClipboard);

        clipboard_frame(&mut view, &ctx, Vec::new());
        clipboard_frame(&mut view, &ctx, Vec::new());

        assert!(!view.clipboard_request_pending());
        assert!(view.accepts(Command::Reload));
        assert_eq!(view.message.as_deref(), Some("The clipboard holds no text"));
        clipboard_frame(
            &mut view,
            &ctx,
            vec![egui::Event::Paste("late clipboard text".to_owned())],
        );

        assert_eq!(view.grid.cell_text(0, 1, Side::Left), "Ann");
        assert_eq!(view.left_path, left);
        assert!(!view.holds_temporaries());
    }

    #[test]
    fn paste_event_with_command_and_shift_opens_clipboard_input() {
        let (_dir, left, right) = files("id,name\n1,Ann\n", "id,name\n1,Bob\n");
        let ctx = egui::Context::default();
        let mut view = TableView::new(left, right, &context(), 37);
        assert!(settle(&mut view, &ctx));
        view.set_active_side(Side::Left);

        let mut input = event_input(
            1_280.0,
            800.0,
            vec![egui::Event::Paste("id,name\n1,Zoe\n".to_owned())],
        );
        input.modifiers.command = true;
        input.modifiers.ctrl = true;
        input.modifiers.shift = true;
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                view.ui(ui, &context());
            });
        });
        assert!(settle(&mut view, &ctx));

        assert_eq!(view.grid.cell_text(0, 1, Side::Left), "Zoe");
        assert!(view.holds_temporaries());
        assert!(!view.clipboard_request_pending());
    }

    #[test]
    fn a_late_clipboard_paste_does_not_edit_the_active_cell() {
        let (_dir, left, right) = files("id,name\n1,Ann\n", "id,name\n1,Ann\n");
        let ctx = egui::Context::default();
        let mut view = TableView::new(left, right, &context(), 34);
        assert!(settle(&mut view, &ctx));
        view.set_active_side(Side::Left);
        view.grid.set_cursor(Cursor { row: 0, column: 1 });

        view.run(Command::Paste);
        let requested = clipboard_frame(&mut view, &ctx, Vec::new());
        assert!(requested.viewport_output.values().any(|viewport| {
            viewport
                .commands
                .contains(&egui::ViewportCommand::RequestPaste)
        }));
        clipboard_frame(&mut view, &ctx, Vec::new());
        clipboard_frame(
            &mut view,
            &ctx,
            vec![egui::Event::Paste("late clipboard text".to_owned())],
        );

        assert_eq!(view.grid.cell_text(0, 1, Side::Left), "Ann");
        assert!(!view.is_modified(Side::Left));
    }

    #[test]
    fn copy_cell_commands_write_the_named_side_and_undo_as_one_edit() {
        let (_dir, left, right) = files("id,name\n1,Ann\n", "id,name\n1,Bob\n");
        let ctx = egui::Context::default();
        let mut view = TableView::new(left, right, &context(), 32);
        assert!(settle(&mut view, &ctx));
        view.grid.set_cursor(Cursor { row: 0, column: 1 });
        view.set_active_side(Side::Left);

        assert!(view.accepts(Command::CopyCellToRight));
        view.run(Command::CopyCellToRight);
        assert!(settle(&mut view, &ctx));
        assert_eq!(view.grid.cell_text(0, 1, Side::Right), "Ann");
        assert!(view.is_modified(Side::Right));

        view.set_active_side(Side::Right);
        view.run(Command::Undo);
        assert!(settle(&mut view, &ctx));
        assert_eq!(view.grid.cell_text(0, 1, Side::Right), "Bob");

        view.set_active_side(Side::Left);
        assert!(view.accepts(Command::CopyCellToLeft));
        view.run(Command::CopyCellToLeft);
        assert!(settle(&mut view, &ctx));
        assert_eq!(view.grid.cell_text(0, 1, Side::Left), "Bob");

        assert!(view.edit_current_cell("Eve").is_ok());
        assert!(settle(&mut view, &ctx));
        view.set_active_side(Side::Left);
        assert!(view.accepts(Command::CopyCellToOtherSide));
        view.run(Command::CopyCellToOtherSide);
        assert!(settle(&mut view, &ctx));
        assert_eq!(view.grid.cell_text(0, 1, Side::Right), "Eve");
    }

    #[test]
    fn copy_cell_to_other_side_is_disabled_when_the_row_has_no_counterpart() {
        let (_dir, left, right) = files("id,name\n1,Ann\n2,Bob\n", "id,name\n1,Ann\n");
        let ctx = egui::Context::default();
        let mut view = TableView::new(left, right, &context(), 33);
        assert!(settle(&mut view, &ctx));
        view.grid.set_cursor(Cursor { row: 1, column: 1 });
        view.set_active_side(Side::Left);

        assert!(!view.accepts(Command::CopyCellToOtherSide));
        assert!(!view.accepts(Command::CopyCellToRight));
    }

    #[test]
    fn copy_stays_available_for_a_read_only_table() {
        let mut view = view_over(Arc::new(FakeSource::new(&[RowStatus::Same; 1], 2)));
        view.read_only = true;
        assert!(view.accepts(Command::Copy));
        assert!(!view.accepts(Command::Cut));
        assert!(!view.accepts(Command::Paste));
        assert!(!view.accepts(Command::Delete));
    }

    #[test]
    fn select_all_copies_visible_rows_present_on_the_active_side_as_tsv() {
        let source = FakeSource::new(
            &[
                RowStatus::Same,
                RowStatus::LeftOnly,
                RowStatus::RightOnly,
                RowStatus::Different,
                RowStatus::Same,
            ],
            2,
        )
        .with_text(2, 0, Side::Right, "right\t2")
        .with_text(2, 1, Side::Right, "value 2")
        .with_text(3, 0, Side::Right, "line\nbreak")
        .with_text(3, 1, Side::Right, "he said \"yes\"");
        let mut view = view_over(Arc::new(source));
        let ctx = egui::Context::default();
        view.set_active_side(Side::Right);
        view.set_filter(DisplayFilter::Differences);

        assert!(view.accepts(Command::SelectAll));
        view.run(Command::SelectAll);
        assert_eq!(view.selected_rows, [2, 3]);
        view.run(Command::Copy);
        assert!(view.copy_job.is_some());
        assert!(view.pending_clipboard.is_none());
        assert!(wait_until(Duration::from_secs(5), || {
            view.poll();
            view.pending_clipboard.is_some()
        }));
        assert_eq!(
            copied(&clipboard_frame(&mut view, &ctx, Vec::new())),
            ["\"right\t2\"\tvalue 2\n\"line\nbreak\"\t\"he said \"\"yes\"\"\""]
        );

        view.set_filter(DisplayFilter::All);
        view.run(Command::SelectAll);
        assert_eq!(view.selected_rows, [0, 2, 3, 4]);
        view.navigate(egui::Key::ArrowDown, 10);
        assert!(view.selected_rows.is_empty());
        assert_eq!(view.selection_side, None);
    }

    #[test]
    fn selected_rows_are_copied_by_a_job_and_landed_on_a_later_frame() {
        let source = FakeSource::repeating(50, 4, &[RowStatus::Same]);
        let mut view = view_over(Arc::new(source));
        let ctx = egui::Context::default();
        view.set_active_side(Side::Left);
        view.run(Command::SelectAll);
        view.run(Command::Copy);

        assert!(view.copy_job.is_some());
        assert!(view.pending_clipboard.is_none());
        assert!(wait_until(Duration::from_secs(5), || {
            view.poll();
            view.pending_clipboard.is_some()
        }));

        let copied = copied(&clipboard_frame(&mut view, &ctx, Vec::new()));
        assert_eq!(copied.len(), 1);
        let lines: Vec<_> = copied[0].lines().collect();
        assert_eq!(lines.len(), 50);
        assert_eq!(
            lines.first().copied(),
            Some("Left0-0\tLeft0-1\tLeft0-2\tLeft0-3")
        );
        assert_eq!(
            lines.last().copied(),
            Some("Left49-0\tLeft49-1\tLeft49-2\tLeft49-3")
        );
    }

    #[test]
    fn two_small_files_compare_and_navigate_to_a_difference() {
        let (_dir, left, right) = files(
            "id,name,score\n1,Ann,10\n2,Bob,20\n3,Cy,30\n",
            "id,name,score\n1,Ann,10\n2,Bob,21\n3,Cy,30\n",
        );
        let ctx = egui::Context::default();
        let mut view = TableView::new(left, right, &context(), 1);
        assert!(settle(&mut view, &ctx), "the comparison finished");
        assert_eq!(view.grid().visual_rows(), 3);
        assert_eq!(view.grid().shown_columns().len(), 3);
        // The view lands on the first difference without being asked.
        assert_eq!(view.grid().cursor().row, 1);
        view.run(Command::PreviousDifference);
        assert_eq!(view.grid().cursor().row, 1);
        view.run(Command::ShowDifferences);
        assert_eq!(view.grid().visual_rows(), 1);
        view.run(Command::ShowNone);
        assert_eq!(view.grid().visual_rows(), 0);
        assert!(!view.accepts(Command::GoTo));
        view.run(Command::ShowAll);
        assert_eq!(view.grid().visual_rows(), 3);
        frame(&mut view, &ctx, 1_280.0, 800.0);
    }

    #[test]
    fn marking_a_key_column_realigns_the_rows() {
        let (_dir, left, right) = files("id,name\n2,Bob\n1,Ann\n", "id,name\n1,Ann\n2,Bob\n");
        let ctx = egui::Context::default();
        let mut view = TableView::new(left, right, &context(), 2);
        assert!(settle(&mut view, &ctx));
        let before = view.grid().difference_rows();
        assert!(before > 0, "the unkeyed comparison finds differences");
        // The two files hold the same rows in opposite order, so the pairs
        // cross. Sorted key alignment is what uncrosses them.
        view.set_sort_rows_before_alignment(true);
        view.set_key_column(0, true);
        assert!(settle(&mut view, &ctx), "the keyed comparison finished");
        assert_eq!(view.grid().difference_rows(), 0);
        assert!(view.grid().source().columns()[0].key);
    }

    #[test]
    fn a_read_only_request_refuses_every_save_and_edit() {
        use ca_ui::view::ViewFactory;
        let (_dir, left, right) = files(
            "id,name
1,Ann
",
            "id,name
1,Bob
",
        );
        let ctx = egui::Context::default();
        let request =
            ca_ui::view::OpenRequest::new(ca_session::SessionKind::TableCompare, left, right)
                .over_temporaries(Vec::new());
        let mut view = TableView::create_from(&request, &context(), 8);
        assert!(settle(&mut view, &ctx));
        assert!(view.write_block(Side::Left).is_some());
        assert!(view.write_block(Side::Right).is_some());
        assert!(!view.accepts(Command::SaveFileAs));
        assert!(!view.can_edit_cell());
    }

    /// The editing switch of the Specs page refuses every cell edit and every
    /// save, and a cleared switch gives them back.
    #[test]
    fn the_editing_switch_refuses_every_save_and_edit() {
        use ca_ui::view::SessionView;
        let (_dir, left, right) = files(
            "id,name
1,Ann
",
            "id,name
1,Bob
",
        );
        let ctx = egui::Context::default();
        let mut view = TableView::new(left, right, &context(), 9);
        assert!(settle(&mut view, &ctx));
        assert!(view.can_edit_cell());
        let mut settings = SessionView::settings(&view).unwrap();
        settings.specs_mut().unwrap().disable_editing = true;
        view.apply_settings(&settings);
        assert!(view.write_block(Side::Left).is_some());
        assert!(view.write_block(Side::Right).is_some());
        assert!(!view.accepts(Command::SaveFileAs));
        assert!(!view.can_edit_cell());
        assert!(!view.holds_unwritten_edits());
        settings.specs_mut().unwrap().disable_editing = false;
        view.apply_settings(&settings);
        assert!(view.can_edit_cell());
        assert!(view.accepts(Command::SaveFileAs));
    }

    #[test]
    fn a_missing_file_is_reported_rather_than_painted_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("present.csv");
        std::fs::write(&left, "a\n1\n").unwrap();
        let ctx = egui::Context::default();
        let mut view = TableView::new(left, dir.path().join("absent.csv"), &context(), 3);
        assert!(!settle(&mut view, &ctx));
        assert!(view.failure().is_some());
        assert!(view.notice().is_some());
    }

    #[test]
    fn a_frame_over_a_million_rows_by_fifty_columns_stays_bounded() {
        let source = FakeSource::repeating(
            1_000_000,
            50,
            &[
                RowStatus::Same,
                RowStatus::Same,
                RowStatus::Different,
                RowStatus::Unimportant,
                RowStatus::LeftOnly,
            ],
        );
        let ctx = egui::Context::default();
        let mut view = view_over(Arc::new(source));
        // The first frame builds the strip and warms the font atlas.
        frame(&mut view, &ctx, 1_280.0, 800.0);
        let mut worst = Duration::ZERO;
        for step in 0..10 {
            view.grid_mut().set_cursor(Cursor {
                row: step * 90_000,
                column: 0,
            });
            let started = Instant::now();
            frame(&mut view, &ctx, 1_280.0, 800.0);
            worst = worst.max(started.elapsed());
        }
        assert_eq!(view.grid().visual_rows(), 1_000_000);
        println!("worst frame over 1,000,000 by 50: {worst:?}");
        assert!(
            worst < Duration::from_millis(400),
            "the worst frame took {worst:?}"
        );
    }

    #[test]
    fn only_the_rows_and_columns_in_view_are_laid_out() {
        let source = FakeSource::repeating(1_000_000, 50, &[RowStatus::Different]);
        let grid = crate::model::Grid::new(Arc::new(source));
        let rows = ca_ui::scroll::RowScroll::top().visible(800.0, ROW_HEIGHT, grid.visual_rows());
        assert!(rows.len() <= 45, "laid out {} rows", rows.len());
        let columns = grid.visible_columns(0.0, 600.0);
        assert!(columns.len() <= 8, "laid out {} columns", columns.len());
    }

    #[test]
    fn a_narrow_window_keeps_every_toolbar_control_inside_it() {
        let source = FakeSource::new(&[RowStatus::Different, RowStatus::Same], 4);
        let ctx = egui::Context::default();
        let mut view = view_over(Arc::new(source));
        // The overflow menu decides from the width the controls needed last
        // frame, so the first frame is what teaches it.
        frame(&mut view, &ctx, 640.0, 600.0);
        let output = ctx.run(sized_input(640.0, 600.0), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                view.ui(ui, &context());
            });
        });
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(640.0, 600.0));
        for shape in output.shapes {
            // What reaches the window is the shape inside its clip rectangle,
            // so a cell whose text runs past its column is not an overflow.
            let rect = shape
                .shape
                .visual_bounding_rect()
                .intersect(shape.clip_rect);
            if !rect.is_finite() || rect.is_negative() {
                continue;
            }
            assert!(
                rect.right() <= screen.right() + 1.0,
                "a control reaches {} on a {} point window",
                rect.right(),
                screen.right()
            );
        }
    }

    #[test]
    fn keyboard_navigation_walks_the_grid() {
        let source = FakeSource::new(&[RowStatus::Same; 10], 4);
        let mut view = view_over(Arc::new(source));
        view.navigate(egui::Key::ArrowDown, 5);
        view.navigate(egui::Key::ArrowRight, 5);
        assert_eq!(view.grid().cursor(), Cursor { row: 1, column: 1 });
        view.navigate(egui::Key::End, 5);
        assert_eq!(view.grid().cursor().row, 9);
        view.navigate(egui::Key::Home, 5);
        assert_eq!(view.grid().cursor().row, 0);
        view.navigate(egui::Key::ArrowUp, 5);
        assert_eq!(view.grid().cursor().row, 0);
    }

    #[test]
    fn a_key_press_reaches_the_grid_through_a_frame() {
        let source = FakeSource::new(&[RowStatus::Same; 10], 4);
        let ctx = egui::Context::default();
        let mut view = view_over(Arc::new(source));
        frame(&mut view, &ctx, 1_280.0, 800.0);
        let click = vec![
            egui::Event::PointerMoved(egui::pos2(200.0, 300.0)),
            egui::Event::PointerButton {
                pos: egui::pos2(200.0, 300.0),
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::default(),
            },
            egui::Event::PointerButton {
                pos: egui::pos2(200.0, 300.0),
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::default(),
            },
        ];
        let _ = ctx.run(event_input(1_280.0, 800.0, click), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                view.ui(ui, &context());
            });
        });
        assert!(view.grid().cursor().row > 0, "the click moved the cursor");
    }

    #[test]
    fn the_details_area_shows_both_sides_of_the_current_cell() {
        let source = FakeSource::new(&[RowStatus::Different], 2);
        let view = view_over(Arc::new(source));
        assert_eq!(view.grid().cell_text(0, 1, Side::Left), "Left0-1");
        assert_eq!(view.grid().cell_text(0, 1, Side::Right), "Right0-1");
    }

    #[test]
    fn the_status_bar_names_every_cell_status() {
        for status in [
            CellStatus::Same,
            CellStatus::Different,
            CellStatus::Unimportant,
            CellStatus::LeftOnly,
            CellStatus::RightOnly,
        ] {
            assert!(!cell_status_label(Some(status)).is_empty());
        }
        assert_eq!(cell_status_label(None), "no cell");
    }

    #[test]
    fn notices_reach_the_frame() {
        let source = FakeSource::new(&[RowStatus::Same], 1)
            .with_notice("The two files disagree about line one.");
        let ctx = egui::Context::default();
        let mut view = view_over(Arc::new(source));
        assert_eq!(view.grid().source().notices().len(), 1);
        frame(&mut view, &ctx, 1_280.0, 800.0);
    }

    #[test]
    fn a_settings_change_supersedes_the_running_comparison() {
        let (_dir, left, right) = files("id,v\n1,2\n2,3\n", "id,v\n1,2\n2,4\n");
        let ctx = egui::Context::default();
        let mut view = TableView::new(left, right, &context(), 4);
        assert!(settle(&mut view, &ctx));
        let before = view.superseded_requests();
        view.set_key_column(0, true);
        view.set_key_column(1, true);
        assert!(
            view.superseded_requests() > before,
            "the second change replaced the first"
        );
        assert!(settle(&mut view, &ctx));
    }

    #[test]
    fn cancelling_stops_the_work_and_says_so() {
        let mut rows = String::new();
        for row in 0..20_000 {
            let _ = writeln!(rows, "{row},{}", row * 2);
        }
        let (_dir, left, right) = files(&format!("id,v\n{rows}"), &format!("id,v\n{rows}"));
        let ctx = egui::Context::default();
        let mut view = TableView::new(left, right, &context(), 5);
        view.run(Command::Cancel);
        assert!(wait_until(Duration::from_secs(30), || {
            frame(&mut view, &ctx, 1_280.0, 800.0);
            view.is_ready()
        }));
    }

    #[test]
    fn the_command_declaration_covers_what_the_view_runs() {
        let source = FakeSource::new(&[RowStatus::Different], 2);
        let view = view_over(Arc::new(source));
        let declared = view.commands();
        assert!(!declared.is_empty());
        for state in declared {
            assert_eq!(state.enabled, view.accepts(state.command));
        }
        assert!(view.accepts(Command::ShowDifferences));
        assert!(!view.accepts(Command::SaveFile));
    }

    #[test]
    fn a_filter_change_renumbers_the_rows_the_view_shows() {
        let source = FakeSource::new(&[RowStatus::Same, RowStatus::Different, RowStatus::Same], 2);
        let mut view = view_over(Arc::new(source));
        view.set_filter(DisplayFilter::Differences);
        assert_eq!(view.grid().visual_rows(), 1);
        view.set_filter(DisplayFilter::All);
        assert_eq!(view.grid().visual_rows(), 3);
    }

    #[test]
    fn closing_the_view_leaves_nothing_running() {
        let (_dir, left, right) = files("a\n1\n", "a\n2\n");
        let mut view = TableView::new(left, right, &context(), 6);
        view.on_close();
        // The handle is gone, so nothing the worker still posts can be taken.
        view.poll();
        assert!(view.grid().visual_rows() == 0);
    }

    #[test]
    fn a_key_column_is_marked_on_its_heading() {
        let source = FakeSource::new(&[RowStatus::Different], 3).with_key(1);
        let ctx = egui::Context::default();
        let mut view = view_over(Arc::new(source));
        assert!(view.grid().source().columns()[1].key);
        frame(&mut view, &ctx, 1_280.0, 800.0);
    }
}
