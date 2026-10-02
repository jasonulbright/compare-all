//! Two pane editable byte comparison.
//!
//! The two files are read and aligned on a worker. The layout the alignment
//! produces is never materialized row by row, and a frame lays out only the
//! rows its viewport covers, so the cost of a frame follows the window rather
//! than the size of the files.

pub mod chars;
pub mod edit;
pub mod find;
pub mod jobs;
pub mod model;
pub mod settings;

use ca_diff::ByteAlignment;
use ca_session::SessionKind;
use ca_ui::command::Command;
use ca_ui::dialog::{self, DialogMessage, Pick, Target};
use ca_ui::report::{ReportKind, ViewReport};
use ca_ui::save::{self as save, Baseline, CloseChoice, Reread, SaveMessage, SaveOutcome, Stamp};
use ca_ui::scroll::RowScroll;
use ca_ui::theme::hex::{ByteClass, Palette};
use ca_ui::thumbnail::{Strip, Thumbnail};
use ca_ui::toolbar;
use ca_ui::view::{OpenRequest, SessionView, ViewAction, ViewContext};
use ca_ui::widgets;
use ca_ui::worker::Job;
use chars::CharEncoding;
use edit::{ByteBuffer, EditMode};
use find::{Direction, FindAllMessage, FindMessage, FindSettings, PatternKind};
use jobs::{HexData, HexMessage, SidePayload};
use model::{DisplayFilter, HexRow, RowClass, RowSpan, Side, Visible};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Width of the overview strip.
const THUMBNAIL_WIDTH: f32 = 26.0;
/// Width of the column between the two panes.
const CENTER_WIDTH: f32 = 14.0;
/// Thickness of a scrollbar.
const SCROLLBAR: f32 = 12.0;
/// Row height as a multiple of the display font size.
const ROW_HEIGHT_RATIO: f32 = 1.45;
/// Display font size a new view starts at.
const DEFAULT_FONT_SIZE: f32 = 13.0;
/// How long the last change is waited out before the comparison runs again.
const REDIFF_DEBOUNCE: Duration = Duration::from_millis(300);
/// Width the drop downs on the toolbar are laid out in.
const COMBO_WIDTH: f32 = 116.0;
/// Why a toolbar button refuses while the comparison is still running.
const NOT_READY: &str = "Available once the comparison finishes";
/// Character columns the address area takes, including its trailing space.
const ADDRESS_COLUMNS_HEX: f32 = 9.0;
/// Character columns a decimal address area takes.
const ADDRESS_COLUMNS_DECIMAL: f32 = 12.0;

/// Every command this view answers for.
///
/// The shell reads the list to build its menus, so the set is stated once here
/// and nowhere else.
const HANDLED: &[Command] = &[
    Command::NextDifference,
    Command::PreviousDifference,
    Command::NextSection,
    Command::PreviousSection,
    Command::ShowAll,
    Command::ShowDifferences,
    Command::ShowSame,
    Command::ShowContext,
    Command::SwapSides,
    Command::Reload,
    Command::OpenFile,
    Command::Recompare,
    Command::Find,
    Command::FindNext,
    Command::FindPrevious,
    Command::Replace,
    Command::GoTo,
    Command::SelectAll,
    Command::Copy,
    Command::Cut,
    Command::Paste,
    Command::Undo,
    Command::Redo,
    Command::ToggleOverwrite,
    Command::CopyToLeft,
    Command::CopyToRight,
    Command::SaveFile,
    Command::SaveFileAs,
    Command::SaveBoth,
    Command::IncreaseFontSize,
    Command::DecreaseFontSize,
    Command::ResetFontSize,
    Command::Cancel,
    Command::CompareReport,
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

/// Which part of a pane the caret is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Area {
    /// The two hexadecimal digits of each byte.
    #[default]
    Hex,
    /// The one character of each byte.
    Chars,
}

/// Where the caret sits and what it has selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Caret {
    /// The byte the caret is on.
    pub offset: u64,
    /// The part of the pane the caret is in.
    pub area: Area,
    /// The other end of the selection, when there is one.
    pub anchor: Option<u64>,
}

impl Caret {
    /// The selected bytes, or `None` when nothing is selected.
    #[must_use]
    pub fn selection(&self) -> Option<std::ops::Range<u64>> {
        let anchor = self.anchor?;
        let (start, end) = if anchor <= self.offset {
            (anchor, self.offset)
        } else {
            (self.offset, anchor)
        };
        (end > start).then_some(start..end)
    }

    /// How many bytes are selected.
    #[must_use]
    pub fn selection_len(&self) -> u64 {
        self.selection().map_or(0, |range| range.end - range.start)
    }
}

/// A question the view is waiting on an answer to.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Prompt {
    /// A concurrent version was retained beside the saved file.
    KeptCopy(String),
    /// The file changed on disk since it was read.
    DiskChanged(Side),
    /// The tab is closing with changes that are not written.
    Closing,
    /// A command that reads the files again waits on changes that are not
    /// written.
    Reread(Reread),
}

/// A byte comparison tab.
#[allow(clippy::struct_excessive_bools)]
pub struct HexView {
    id: egui::Id,
    /// The sides and the description the session last gave, reported with the
    /// sides replaced by the files the view has open.
    specs: ca_session::settings::SpecsSettings,
    /// The files are copies the tab owns, so neither side takes an edit and
    /// nothing is saved.
    read_only: bool,
    left_path: PathBuf,
    right_path: PathBuf,
    left_field: String,
    right_field: String,
    /// Kept so every job this view spawns asks for the repaint that shows its
    /// result. A job spawned without it posts into a frame loop that is asleep.
    notify: Arc<dyn Fn() + Send + Sync>,
    job: Option<Job<HexMessage>>,
    picker: Option<Job<DialogMessage>>,
    picker_target: Target,
    picker_saves: bool,
    status: Status,
    data: HexData,
    /// True once a read of the files has landed. Before that the buffers are
    /// empty, and a comparison of them would replace the read with two empty
    /// sides.
    loaded: bool,
    left_bytes: ByteBuffer,
    right_bytes: ByteBuffer,
    left_stamp: Option<Stamp>,
    right_stamp: Option<Stamp>,
    alignment: ByteAlignment,
    filter: DisplayFilter,
    context_rows: u32,
    visible: Visible,
    bytes_per_row: u32,
    auto_width: bool,
    encoding: CharEncoding,
    hex_addresses: bool,
    show_addresses: bool,
    show_thumbnail: bool,
    show_file_info: bool,
    edit_mode: EditMode,
    /// The high half of a byte a hexadecimal digit started.
    pending_nibble: Option<u8>,
    active: Side,
    left_caret: Caret,
    right_caret: Caret,
    row: usize,
    font: ca_ui::font::FontSize,
    /// Padding the options add between rows, read once per frame.
    line_spacing: u32,
    scroll: RowScroll,
    horizontal: f32,
    strip: Strip<RowClass>,
    strip_stale: bool,
    viewport_height: f32,
    viewport_rows: usize,
    body: egui::Rect,
    center_x: f32,
    /// When the comparison behind a change is due to run again.
    stale_at: Option<Instant>,
    find: ca_ui::find::FindPanel,
    find_kind: PatternKind,
    find_job: Option<Job<FindMessage>>,
    /// Length of the phrase the running search looks for, so a match can be
    /// selected when it arrives.
    find_len: u64,
    replace_job: Option<Job<FindAllMessage>>,
    /// Content revision the running replace all scanned. A scan over content
    /// that changed since holds offsets into bytes that are gone.
    replace_revision: u64,
    goto_open: bool,
    /// The go to field takes the keyboard on the next frame.
    goto_focus: bool,
    goto_text: String,
    message: Option<String>,
    save_job: Option<Job<SaveMessage>>,
    save_rules: ca_ui::save::SaveRules,
    saving: Option<Side>,
    saving_revision: Option<u64>,
    saving_as: Option<PathBuf>,
    save_both_pending: bool,
    prompt: Option<Prompt>,
    close_requested: bool,
    /// The command that reads the files again once the save in progress has
    /// written every change.
    reread_after_save: Option<Reread>,
    /// Text a command asked to be put on the clipboard.
    ///
    /// A command runs without a frame in hand, and the clipboard is reached
    /// through the frame context, so the text waits here until the next frame.
    pending_clipboard: Option<String>,
    /// A Paste command waits for the clipboard event in the next frame.
    paste_requested: bool,
    /// Room the toolbar was given and room its controls took, from the last
    /// frame. A test reads both to check that nothing ran past the edge.
    toolbar_room: f32,
    toolbar_used: f32,
    /// The pass the keyboard events were last consumed in.
    ///
    /// Reading the same pass twice applies every typed digit twice, so the pass
    /// number gates the handler rather than the call site.
    input_pass: Option<u64>,
    /// The report command of this view.
    report: ViewReport,
}

impl HexView {
    /// A comparison of two files, with the work already started.
    #[must_use]
    pub fn new(left: PathBuf, right: PathBuf, context: &ViewContext, salt: u64) -> Self {
        let mut view = Self::empty(left, right, context, salt);
        view.restart();
        view
    }

    fn empty(left: PathBuf, right: PathBuf, context: &ViewContext, salt: u64) -> Self {
        Self {
            id: egui::Id::new(("hex-compare", salt)),
            specs: ca_session::settings::SpecsSettings::default(),
            read_only: false,
            left_field: left.display().to_string(),
            right_field: right.display().to_string(),
            left_path: left,
            right_path: right,
            notify: context.notify.clone(),
            job: None,
            picker: None,
            picker_target: Target::Left,
            picker_saves: false,
            status: Status::Running("Starting"),
            data: HexData::default(),
            loaded: false,
            left_bytes: ByteBuffer::default(),
            right_bytes: ByteBuffer::default(),
            left_stamp: None,
            right_stamp: None,
            alignment: ByteAlignment::default(),
            filter: DisplayFilter::All,
            context_rows: 2,
            visible: Visible::All(0),
            bytes_per_row: model::DEFAULT_BYTES_PER_ROW,
            auto_width: true,
            encoding: CharEncoding::default(),
            hex_addresses: true,
            show_addresses: true,
            show_thumbnail: true,
            show_file_info: true,
            edit_mode: EditMode::default(),
            pending_nibble: None,
            active: Side::Left,
            left_caret: Caret::default(),
            right_caret: Caret::default(),
            row: 0,
            font: ca_ui::font::FontSize::new(DEFAULT_FONT_SIZE),
            line_spacing: 0,
            scroll: RowScroll::top(),
            horizontal: 0.0,
            strip: Strip::default(),
            strip_stale: true,
            viewport_height: 0.0,
            viewport_rows: 0,
            body: egui::Rect::NOTHING,
            center_x: 0.0,
            stale_at: None,
            find: ca_ui::find::FindPanel::with_settings(ca_ui::find::FindSettings {
                wrap: true,
                ..ca_ui::find::FindSettings::default()
            }),
            find_kind: PatternKind::Bytes,
            find_job: None,
            find_len: 0,
            replace_job: None,
            replace_revision: 0,
            goto_open: false,
            goto_focus: false,
            goto_text: String::new(),
            message: None,
            save_job: None,
            save_rules: ca_ui::save::SaveRules::default(),
            saving: None,
            saving_revision: None,
            saving_as: None,
            save_both_pending: false,
            prompt: None,
            close_requested: false,
            reread_after_save: None,
            pending_clipboard: None,
            paste_requested: false,
            toolbar_room: 0.0,
            toolbar_used: 0.0,
            input_pass: None,
            report: ViewReport::new(
                ReportKind::Hex,
                egui::Id::new(("hex-compare", salt)),
                context.notify.clone(),
            ),
        }
    }

    /// A view over a comparison the caller already holds, with nothing to read.
    #[must_use]
    pub fn from_data(
        left: PathBuf,
        right: PathBuf,
        context: &ViewContext,
        salt: u64,
        data: HexData,
    ) -> Self {
        let mut view = Self::empty(left, right, context, salt);
        view.auto_width = false;
        view.bytes_per_row = data.model.bytes_per_row();
        view.install(HexData {
            fresh: true,
            ..data
        });
        view
    }

    /// Which kind of session this view answers for.
    #[must_use]
    pub fn kind() -> SessionKind {
        SessionKind::HexCompare
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

    /// The row layout currently on screen.
    #[must_use]
    pub const fn model(&self) -> &model::RowModel {
        &self.data.model
    }

    /// The display filter in force.
    #[must_use]
    pub const fn filter(&self) -> DisplayFilter {
        self.filter
    }

    /// How many bytes a row shows.
    #[must_use]
    pub const fn bytes_per_row(&self) -> u32 {
        self.bytes_per_row
    }

    /// The caret of one pane.
    #[must_use]
    pub const fn caret(&self, side: Side) -> Caret {
        match side {
            Side::Left => self.left_caret,
            Side::Right => self.right_caret,
        }
    }

    /// The pane the caret is in.
    #[must_use]
    pub const fn active_side(&self) -> Side {
        self.active
    }

    /// The row the caret is on.
    #[must_use]
    pub const fn caret_row(&self) -> usize {
        self.row
    }

    /// The bytes of one pane.
    #[must_use]
    pub const fn buffer(&self, side: Side) -> &ByteBuffer {
        match side {
            Side::Left => &self.left_bytes,
            Side::Right => &self.right_bytes,
        }
    }

    /// How a typed byte enters the content.
    #[must_use]
    pub const fn edit_mode(&self) -> EditMode {
        self.edit_mode
    }

    /// The scroll position, in rows.
    #[must_use]
    pub const fn scroll_position(&self) -> RowScroll {
        self.scroll
    }

    /// How many rows the overview strip reduces to pixels.
    #[must_use]
    pub fn strip_pixels(&self) -> usize {
        self.strip.buckets().len()
    }

    /// Room the toolbar was given and room its controls took, last frame.
    ///
    /// A control that took more room than the bar had would be drawn past the
    /// edge, so a test reads both figures rather than a screen shot.
    #[must_use]
    pub const fn toolbar_extent(&self) -> (f32, f32) {
        (self.toolbar_room, self.toolbar_used)
    }

    /// What a search looks for, as the find field holds it.
    #[must_use]
    pub fn find_pattern(&self) -> &str {
        &self.find.settings.pattern
    }

    /// Set what a search looks for.
    pub fn set_find_pattern(&mut self, pattern: &str, kind: PatternKind) {
        pattern.clone_into(&mut self.find.settings.pattern);
        self.find_kind = kind;
    }

    /// Set what a replace puts in place of a match.
    pub fn set_replacement(&mut self, replacement: &str) {
        replacement.clone_into(&mut self.find.settings.replacement);
    }

    /// The byte search settings the find strip describes.
    fn byte_find(&self) -> FindSettings {
        FindSettings {
            pattern: self.find.settings.pattern.clone(),
            kind: self.find_kind,
            match_case: self.find.settings.match_case,
            wrap: self.find.settings.wrap,
        }
    }

    /// Put the caret on one byte of one pane, and make that pane the active one.
    pub fn place_caret(&mut self, side: Side, offset: u64) {
        self.go_to_offset(side, offset, false);
    }

    /// Pick which part of the active pane the caret is in.
    pub fn set_area(&mut self, area: Area) {
        let side = self.active;
        self.caret_mut(side).area = area;
        self.pending_nibble = None;
    }

    /// Show or hide the byte address column, as the Addresses toggle does.
    pub fn set_show_addresses(&mut self, show: bool) {
        self.show_addresses = show;
    }

    /// True when the byte address column is shown.
    #[must_use]
    pub const fn shows_addresses(&self) -> bool {
        self.show_addresses
    }

    /// True when addresses are written in hexadecimal.
    #[must_use]
    pub const fn hex_addresses(&self) -> bool {
        self.hex_addresses
    }

    /// True when the thumbnail strip is shown.
    #[must_use]
    pub const fn shows_thumbnail(&self) -> bool {
        self.show_thumbnail
    }

    /// True when the line naming what each side holds is shown.
    #[must_use]
    pub const fn shows_file_info(&self) -> bool {
        self.show_file_info
    }

    /// True when the row width follows the width of the panes.
    #[must_use]
    pub const fn fits_row(&self) -> bool {
        self.auto_width
    }

    /// Whatever the view is reporting.
    #[must_use]
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    fn buffer_mut(&mut self, side: Side) -> &mut ByteBuffer {
        match side {
            Side::Left => &mut self.left_bytes,
            Side::Right => &mut self.right_bytes,
        }
    }

    fn caret_mut(&mut self, side: Side) -> &mut Caret {
        match side {
            Side::Left => &mut self.left_caret,
            Side::Right => &mut self.right_caret,
        }
    }

    fn settings(&self) -> jobs::CompareSettings {
        jobs::CompareSettings {
            alignment: self.alignment,
            bytes_per_row: self.bytes_per_row,
        }
    }

    /// The settings of the session this view shows, as they now stand.
    #[must_use]
    pub fn session_settings(&self) -> ca_session::settings::HexCompareSettings {
        let mut settings = ca_session::settings::HexCompareSettings {
            specs: ca_ui::view::with_sides(&self.specs, &self.left_path, &self.right_path),
            ..ca_session::settings::HexCompareSettings::default()
        };
        crate::settings::write_alignment(&mut settings.comparison, self.alignment);
        crate::settings::write_bytes_per_row(&mut settings.comparison, self.bytes_per_row);
        crate::settings::write_char_encoding(&mut settings.comparison, self.encoding);
        settings
    }

    /// True when no side may take an edit and no save may run: the files are
    /// copies the tab owns, or the editing switch of the session is on.
    fn editing_off(&self) -> bool {
        self.read_only || self.specs.disable_editing
    }

    /// Take the settings of the session and compare again under them.
    pub fn apply_session_settings(&mut self, settings: &ca_session::settings::HexCompareSettings) {
        let switched = self.specs.disable_editing != settings.specs.disable_editing;
        self.specs.clone_from(&settings.specs);
        if switched {
            let off = self.editing_off();
            self.left_bytes.set_read_only(off);
            self.right_bytes.set_read_only(off);
        }
        self.encoding = crate::settings::char_encoding(&settings.comparison.char_encoding);
        let options = crate::settings::options_of(settings);
        if options.alignment == self.alignment && options.bytes_per_row == self.bytes_per_row {
            return;
        }
        self.alignment = options.alignment;
        self.bytes_per_row = options.bytes_per_row;
        self.auto_width = false;
        self.recompare();
    }
    /// Read both files again and compare them.
    fn restart(&mut self) {
        if let Some(job) = self.job.take() {
            job.cancel();
        }
        self.status = Status::Running("Starting");
        self.data = HexData::default();
        self.loaded = false;
        self.left_bytes = ByteBuffer::default();
        self.right_bytes = ByteBuffer::default();
        self.visible = Visible::All(0);
        self.row = 0;
        self.left_caret = Caret::default();
        self.right_caret = Caret::default();
        self.scroll = RowScroll::top();
        self.horizontal = 0.0;
        self.strip_stale = true;
        self.stale_at = None;
        self.job = Some(jobs::spawn(
            self.left_path.clone(),
            self.right_path.clone(),
            self.settings(),
            self.notify.clone(),
        ));
    }

    /// Compare the content in memory again, keeping every change.
    ///
    /// Before a read of the files lands there is no content in memory, so the
    /// files are read again under the settings now in force.
    fn recompare(&mut self) {
        if !self.loaded {
            self.restart();
            return;
        }
        if let Some(job) = self.job.take() {
            job.cancel();
        }
        self.stale_at = None;
        self.status = Status::Running("Comparing");
        self.job = Some(jobs::spawn_bytes(
            SidePayload {
                bytes: Arc::new(self.left_bytes.bytes().to_vec()),
                len: self.left_bytes.len(),
                stamp: self.left_stamp,
            },
            SidePayload {
                bytes: Arc::new(self.right_bytes.bytes().to_vec()),
                len: self.right_bytes.len(),
                stamp: self.right_stamp,
            },
            self.settings(),
            self.notify.clone(),
        ));
    }

    /// Record that a change made the comparison stale.
    fn note_change(&mut self) {
        self.stale_at = Some(Instant::now() + REDIFF_DEBOUNCE);
    }

    fn install(&mut self, data: HexData) {
        let fresh = data.fresh;
        if fresh {
            self.loaded = true;
            self.left_bytes = ByteBuffer::new(data.left.bytes.as_ref().clone());
            self.right_bytes = ByteBuffer::new(data.right.bytes.as_ref().clone());
            let off = self.editing_off();
            self.left_bytes.set_read_only(off);
            self.right_bytes.set_read_only(off);
            self.left_stamp = data.left.stamp;
            self.right_stamp = data.right.stamp;
        }
        self.alignment = data.alignment;
        self.bytes_per_row = data.model.bytes_per_row();
        self.data = data;
        self.status = Status::Ready;
        self.refilter();
        if fresh {
            if let Some(first) = self.data.model.next_section(0).or_else(|| {
                self.data
                    .model
                    .sections()
                    .first()
                    .map(|section| section.start)
            }) {
                self.go_to_row(first);
                self.place_caret_on_row(first);
            }
        } else {
            self.follow_caret();
        }
    }

    fn refilter(&mut self) {
        let filter = match self.filter {
            DisplayFilter::Context(_) => DisplayFilter::Context(self.context_rows),
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
        let row = self.row;
        self.go_to_row(row);
    }

    fn set_bytes_per_row(&mut self, bytes: u32) {
        let bytes = bytes.clamp(model::MIN_BYTES_PER_ROW, model::MAX_BYTES_PER_ROW);
        if bytes == self.bytes_per_row {
            return;
        }
        self.bytes_per_row = bytes;
        self.data.model.set_bytes_per_row(bytes);
        self.refilter();
        self.follow_caret();
    }

    fn row_height(&self) -> f32 {
        self.font.row_height(ROW_HEIGHT_RATIO, self.line_spacing)
    }

    /// The point size the byte panes draw at.
    #[must_use]
    pub fn font_size(&self) -> f32 {
        self.font.points()
    }

    /// Take the sizes the options document now states.
    fn follow_options(&mut self, ctx: &egui::Context) {
        let options = ca_ui::options::current(ctx);
        let before = (self.font.points(), self.line_spacing);
        self.font.follow(options.hex_point_size());
        self.line_spacing = options.extra_line_spacing();
        self.save_rules = ca_ui::save::SaveRules::from_options(&options.stored);
        if before != (self.font.points(), self.line_spacing) {
            self.strip_stale = true;
        }
    }

    fn set_font_size(&mut self, size: f32) {
        self.font.set(size);
        self.strip_stale = true;
    }

    fn go_to_row(&mut self, row: usize) {
        self.row = row.min(self.data.model.row_count().saturating_sub(1));
        let Some(position) = self.visible.position_of(self.row) else {
            return;
        };
        self.scroll.reveal(
            position,
            self.viewport_height,
            self.row_height(),
            self.visible.len(),
        );
    }

    /// Put the caret of both panes on the first byte a row shows.
    fn place_caret_on_row(&mut self, row: usize) {
        let Some(entry) = self.data.model.row(row) else {
            return;
        };
        if let Some(span) = entry.left {
            self.left_caret.offset = span.start;
            self.left_caret.anchor = None;
        }
        if let Some(span) = entry.right {
            self.right_caret.offset = span.start;
            self.right_caret.anchor = None;
        }
    }

    /// Put the row caret back on the row the active pane's caret is on.
    fn follow_caret(&mut self) {
        let offset = self.caret(self.active).offset;
        if let Some(row) = self.data.model.row_of_offset(self.active, offset) {
            self.row = row;
        }
        self.scroll
            .clamp(self.viewport_height, self.row_height(), self.visible.len());
    }

    /// Move the caret to a byte and bring its row into view.
    fn go_to_offset(&mut self, side: Side, offset: u64, extend: bool) {
        self.active = side;
        let limit = self.buffer(side).len();
        let offset = offset.min(limit);
        {
            let caret = self.caret_mut(side);
            if extend {
                caret.anchor.get_or_insert(caret.offset);
            } else {
                caret.anchor = None;
            }
            caret.offset = offset;
        }
        self.pending_nibble = None;
        if let Some(row) = self.data.model.row_of_offset(side, offset) {
            self.go_to_row(row);
        }
    }

    fn navigate(&mut self, command: Command) {
        let model = &self.data.model;
        let target = match command {
            Command::NextDifference => model.next_difference(self.row),
            Command::PreviousDifference => model.previous_difference(self.row),
            Command::NextSection => model.next_section(self.row),
            Command::PreviousSection => model.previous_section(self.row),
            _ => None,
        };
        if let Some(row) = target {
            self.go_to_row(row);
            self.place_caret_on_row(row);
        }
    }

    // --- background work -------------------------------------------------

    fn poll(&mut self) {
        self.poll_comparison();
        self.poll_picker();
        self.poll_find();
        self.poll_replace();
        self.poll_save();
    }

    fn poll_comparison(&mut self) {
        let Some(job) = self.job.as_mut() else {
            return;
        };
        let messages = job.drain();
        let finished = job.is_finished();
        for message in messages {
            match message {
                HexMessage::Progress(step) => self.status = Status::Running(step),
                HexMessage::Failed(reason) => self.status = Status::Failed(reason),
                HexMessage::Cancelled => self.status = Status::Cancelled,
                HexMessage::Ready(data) => self.install(*data),
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
        let mut chosen: Option<PathBuf> = None;
        for message in messages {
            match message {
                DialogMessage::Chosen(path) => {
                    if self.picker_saves {
                        chosen = Some(path);
                    } else {
                        let text = path.display().to_string();
                        match self.picker_target {
                            Target::Left => self.left_field = text,
                            Target::Right => self.right_field = text,
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
        if let Some(path) = chosen {
            self.save_active_as(&path);
        }
    }

    fn poll_find(&mut self) {
        let Some(job) = self.find_job.as_mut() else {
            return;
        };
        let messages = job.drain();
        let finished = job.is_finished();
        for message in messages {
            match message {
                FindMessage::Found(side, offset) => {
                    self.message = None;
                    self.go_to_offset(side, offset, false);
                    let end = offset.saturating_add(self.find_len);
                    // The caret stays on the first byte so the next search starts
                    // one byte on and still finds a match that follows at once.
                    self.caret_mut(side).anchor = Some(end);
                }
                FindMessage::NotFound => self.message = Some("Not found.".to_owned()),
                FindMessage::Cancelled => {}
            }
        }
        if finished {
            self.find_job = None;
        }
    }

    fn poll_replace(&mut self) {
        let Some(job) = self.replace_job.as_mut() else {
            return;
        };
        let messages = job.drain();
        let finished = job.is_finished();
        if finished || !messages.is_empty() {
            self.replace_job = None;
        }
        for message in messages {
            let FindAllMessage::Found(side, offsets) = message else {
                continue;
            };
            if self.buffer(side).revision() != self.replace_revision {
                self.message =
                    Some("The content changed during the search. Replace All again.".to_owned());
                continue;
            }
            let Ok(needle) = find::pattern_bytes(&self.byte_find(), self.encoding) else {
                continue;
            };
            let Some(replacement) = self.replacement_bytes() else {
                continue;
            };
            let count =
                self.buffer_mut(side)
                    .replace_all(&offsets, needle.len() as u64, &replacement);
            self.message = Some(format!("{count} replaced."));
            if count > 0 {
                self.note_change();
            }
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
            match message {
                SaveMessage::Done(outcome) => self.apply_save_outcome(*outcome),
                SaveMessage::Cancelled => {
                    self.message = Some("The save stopped.".to_owned());
                    self.saving = None;
                    self.saving_as = None;
                    self.save_both_pending = false;
                    self.reread_after_save = None;
                }
            }
        }
    }

    fn apply_save_outcome(&mut self, outcome: SaveOutcome) {
        let Some(side) = self.saving.take() else {
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
                    match side {
                        Side::Left => {
                            self.left_field = path.display().to_string();
                            self.left_path = path;
                        }
                        Side::Right => {
                            self.right_field = path.display().to_string();
                            self.right_path = path;
                        }
                    }
                }
                match side {
                    Side::Left => self.left_stamp = Some(stamp),
                    Side::Right => self.right_stamp = Some(stamp),
                }
                if let Some(revision) = self.saving_revision.take() {
                    self.buffer_mut(side).mark_saved_revision(revision);
                }
                self.message = Some(conflict_backup.as_ref().map_or_else(
                    || format!("{} written.", side.label()),
                    |backup| format!("Saved, but another version was kept at {backup}. Review it."),
                ));
                if let Some(backup) = conflict_backup {
                    self.prompt = Some(Prompt::KeptCopy(backup));
                    self.save_both_pending = false;
                    self.close_requested = false;
                    self.reread_after_save = None;
                } else {
                    if self.save_both_pending {
                        self.save_next_modified();
                    }
                    self.reread_when_saved();
                }
            }
            SaveOutcome::ChangedOnDisk => self.prompt = Some(Prompt::DiskChanged(side)),
            SaveOutcome::NotWritable => {
                self.message = Some(format!("{} cannot be written.", side.label()));
                self.save_both_pending = false;
                self.reread_after_save = None;
            }
            // A byte comparison encodes nothing, so this outcome never arises
            // here; it is reported by a view whose encode can fail.
            SaveOutcome::WouldLose(reason) | SaveOutcome::Failed(reason) => {
                self.message = Some(format!("Save failed: {reason}"));
                self.save_both_pending = false;
                self.reread_after_save = None;
            }
        }
    }

    fn save_side(&mut self, side: Side, accept_disk_change: bool) {
        self.save_side_to(side, accept_disk_change, None);
    }

    fn save_side_to(&mut self, side: Side, accept_disk_change: bool, save_as: Option<PathBuf>) {
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
        self.prompt = None;
        self.saving = Some(side);
        self.saving_revision = Some(self.buffer(side).revision());
        let path = save_as.as_ref().map_or_else(
            || match side {
                Side::Left => self.left_path.clone(),
                Side::Right => self.right_path.clone(),
            },
            Clone::clone,
        );
        let expected = if save_as.is_some() {
            Baseline::Unchecked
        } else {
            match side {
                Side::Left => self.left_stamp,
                Side::Right => self.right_stamp,
            }
            .into()
        };
        self.saving_as = save_as;
        self.save_job = Some(save::spawn(
            path,
            self.buffer(side).bytes().to_vec(),
            expected,
            accept_disk_change,
            self.save_rules.clone(),
            self.notify.clone(),
        ));
    }

    fn save_next_modified(&mut self) {
        self.save_both_pending = true;
        if self.left_bytes.is_modified() {
            self.save_side(Side::Left, false);
        } else if self.right_bytes.is_modified() {
            self.save_side(Side::Right, false);
        } else {
            self.save_both_pending = false;
        }
    }

    /// Write the active pane under a new name.
    pub fn save_active_as(&mut self, path: &Path) {
        if self.save_job.is_some() {
            self.message = Some("A save is still running. Try Save As again.".to_owned());
            return;
        }
        self.save_side_to(self.active, true, Some(path.to_path_buf()));
    }

    fn apply_fields(&mut self) {
        self.left_path = PathBuf::from(self.left_field.clone());
        self.right_path = PathBuf::from(self.right_field.clone());
        self.restart();
    }

    fn open_picker(&mut self, target: Target) {
        if self.picker.is_some() {
            return;
        }
        self.picker_target = target;
        self.picker_saves = false;
        self.picker = Some(dialog::spawn(Pick::File, self.notify.clone()));
    }

    fn open_save_picker(&mut self) {
        if self.picker.is_some() {
            return;
        }
        self.picker_saves = true;
        self.picker = Some(dialog::spawn(Pick::SaveFile, self.notify.clone()));
    }

    // --- editing ---------------------------------------------------------

    fn editable(&self) -> bool {
        self.status == Status::Ready && !self.buffer(self.active).is_read_only()
    }

    /// Put one byte in at the caret and step past it.
    fn type_byte(&mut self, byte: u8) {
        if !self.editable() {
            return;
        }
        let side = self.active;
        let caret = self.caret(side);
        let at = caret.offset;
        let mode = self.edit_mode;
        if self.buffer_mut(side).write(at, &[byte], mode) {
            self.go_to_offset(side, at + 1, false);
            self.note_change();
        }
    }

    /// Take one hexadecimal digit, which fills half a byte.
    fn type_digit(&mut self, digit: u8) {
        if !self.editable() {
            return;
        }
        let side = self.active;
        let at = self.caret(side).offset;
        if self.pending_nibble.take().is_some() {
            // The second digit always lands on the byte the first one made, so
            // it replaces that byte whatever mode the pane is in.
            let high = self.buffer(side).byte(at).unwrap_or(0) & 0xF0;
            let byte = high | digit;
            if self
                .buffer_mut(side)
                .write(at, &[byte], EditMode::Overwrite)
            {
                self.note_change();
                self.go_to_offset(side, at + 1, false);
            }
            return;
        }
        let low = self.buffer(side).byte(at).unwrap_or(0) & 0x0F;
        let byte = (digit << 4) | low;
        let mode = if self.buffer(side).byte(at).is_some() {
            EditMode::Overwrite
        } else {
            self.edit_mode
        };
        if self.buffer_mut(side).write(at, &[byte], mode) {
            self.pending_nibble = Some(digit);
            self.note_change();
        }
    }

    /// Take out the selection, or one byte in the given direction.
    fn delete(&mut self, forward: bool) {
        if !self.editable() {
            return;
        }
        let side = self.active;
        let caret = self.caret(side);
        let (at, len) = match caret.selection() {
            Some(range) => (range.start, range.end - range.start),
            None if forward => (caret.offset, 1),
            None => (caret.offset.saturating_sub(1), 1),
        };
        if !forward && caret.selection().is_none() && caret.offset == 0 {
            return;
        }
        if self.buffer_mut(side).delete(at, len) {
            self.go_to_offset(side, at, false);
            self.note_change();
        }
    }

    /// The bytes the active pane has selected, or the byte under the caret.
    fn selected_bytes(&self, side: Side) -> Vec<u8> {
        let caret = self.caret(side);
        let range = caret.selection().unwrap_or(caret.offset..caret.offset + 1);
        let len = u32::try_from(range.end - range.start).unwrap_or(u32::MAX);
        self.buffer(side).slice(range.start, len).to_vec()
    }

    /// The clipboard form of a run of bytes.
    ///
    /// The hexadecimal area puts digits on the clipboard and the character area
    /// puts the characters, so a copy and a paste inside one area round trip.
    fn clipboard_text(&self, bytes: &[u8]) -> String {
        match self.caret(self.active).area {
            Area::Hex => edit::to_hex_text(bytes),
            Area::Chars => bytes
                .iter()
                .map(|byte| self.encoding.character(*byte))
                .collect(),
        }
    }

    fn paste(&mut self, text: &str) {
        if !self.editable() {
            return;
        }
        let bytes = match self.caret(self.active).area {
            Area::Hex => edit::from_hex_text(text),
            Area::Chars => self.encoding.encode(text),
        };
        let Some(bytes) = bytes else {
            self.message = Some("The clipboard does not hold bytes this area can take.".to_owned());
            return;
        };
        let side = self.active;
        let caret = self.caret(side);
        let at = caret.selection().map_or(caret.offset, |range| range.start);
        if let Some(range) = caret.selection() {
            self.buffer_mut(side)
                .delete(range.start, range.end - range.start);
        }
        let mode = self.edit_mode;
        if self.buffer_mut(side).write(at, &bytes, mode) {
            self.go_to_offset(side, at + bytes.len() as u64, false);
            self.note_change();
        }
    }

    /// Put the selected bytes of one side into the other.
    fn copy_to(&mut self, to: Side) {
        let from = to.other();
        if self.buffer(to).is_read_only() {
            return;
        }
        let bytes = self.selected_bytes(from);
        if bytes.is_empty() {
            return;
        }
        let caret = self.caret(to);
        let at = caret.selection().map_or(caret.offset, |range| range.start);
        let taken = caret.selection_len();
        if self.buffer_mut(to).replace(at, taken, &bytes) {
            self.note_change();
        }
    }

    fn select_all(&mut self) {
        let side = self.active;
        let len = self.buffer(side).len();
        let caret = self.caret_mut(side);
        caret.anchor = Some(0);
        caret.offset = len;
    }

    // --- searching -------------------------------------------------------

    fn run_find(&mut self, direction: Direction) {
        if let Some(job) = self.find_job.take() {
            job.cancel();
        }
        let settings = self.byte_find();
        let needle = match find::pattern_bytes(&settings, self.encoding) {
            Ok(needle) => needle,
            Err(reason) => {
                self.message = Some(reason.message().to_owned());
                return;
            }
        };
        let side = self.active;
        let caret = self.caret(side);
        let from = match direction {
            Direction::Forward => caret.offset.saturating_add(1),
            Direction::Backward => caret.offset,
        };
        self.message = Some("Searching".to_owned());
        self.find_len = needle.len() as u64;
        self.find_job = Some(find::spawn(
            side,
            Arc::new(self.buffer(side).bytes().to_vec()),
            needle,
            from,
            direction,
            settings,
            self.notify.clone(),
        ));
    }

    /// The bytes a replace puts in, read the way the search phrase is read.
    ///
    /// An empty replacement is allowed and deletes the match.
    fn replacement_bytes(&mut self) -> Option<Vec<u8>> {
        let text = &self.find.settings.replacement;
        let bytes = match self.find_kind {
            PatternKind::Bytes => edit::from_hex_text(text),
            PatternKind::Text => self.encoding.encode(text),
        };
        if bytes.is_none() {
            self.message = Some(
                match self.find_kind {
                    PatternKind::Bytes => "Enter the replacement as pairs of hexadecimal digits.",
                    PatternKind::Text => "The character encoding cannot represent the replacement.",
                }
                .to_owned(),
            );
        }
        bytes
    }

    /// Replace the selected match, then find the next one.
    pub fn replace_match(&mut self) {
        if !self.editable() {
            self.message = Some("This side cannot be edited.".to_owned());
            return;
        }
        let settings = self.byte_find();
        let needle = match find::pattern_bytes(&settings, self.encoding) {
            Ok(needle) => needle,
            Err(reason) => {
                self.message = Some(reason.message().to_owned());
                return;
            }
        };
        let Some(replacement) = self.replacement_bytes() else {
            return;
        };
        let side = self.active;
        let selected = self.caret(side).selection();
        let matches = selected.as_ref().is_some_and(|range| {
            let len = range.end - range.start;
            len == needle.len() as u64
                && find::find_forward(
                    self.buffer(side)
                        .slice(range.start, u32::try_from(len).unwrap_or(0)),
                    &needle,
                    0,
                    settings.match_case,
                ) == Some(0)
        });
        if let (true, Some(range)) = (matches, selected) {
            if self
                .buffer_mut(side)
                .replace(range.start, range.end - range.start, &replacement)
            {
                let after = range.start + replacement.len() as u64;
                self.go_to_offset(side, after.saturating_sub(1), false);
                self.note_change();
            }
        }
        self.run_find(Direction::Forward);
    }

    /// Replace every match on the active side as one undo step.
    pub fn replace_all_matches(&mut self) {
        if !self.editable() {
            self.message = Some("This side cannot be edited.".to_owned());
            return;
        }
        let settings = self.byte_find();
        let needle = match find::pattern_bytes(&settings, self.encoding) {
            Ok(needle) => needle,
            Err(reason) => {
                self.message = Some(reason.message().to_owned());
                return;
            }
        };
        if self.replacement_bytes().is_none() {
            return;
        }
        let side = self.active;
        self.replace_revision = self.buffer(side).revision();
        self.message = Some("Searching".to_owned());
        self.replace_job = Some(find::spawn_all(
            side,
            self.buffer(side).snapshot(),
            needle,
            settings.match_case,
            self.notify.clone(),
        ));
    }

    // --- panels ----------------------------------------------------------

    fn path_bar(&mut self, ui: &mut egui::Ui) {
        let action = widgets::path_bar(ui, &mut self.left_field, &mut self.right_field, "Reload");
        match action {
            Some(widgets::PathBarAction::Browse(target)) => self.open_picker(target),
            Some(widgets::PathBarAction::Reload) => self.request_reread(Reread::Open),
            None => {}
        }
    }

    /// The line under each path field, naming what the side holds.
    fn file_info(&mut self, ui: &mut egui::Ui) {
        if !self.show_file_info {
            return;
        }
        let sides: Vec<widgets::FileInfo> = [Side::Left, Side::Right]
            .into_iter()
            .map(|side| {
                let bytes = self.buffer(side);
                let stamp = match side {
                    Side::Left => self.left_stamp,
                    Side::Right => self.right_stamp,
                };
                widgets::FileInfo {
                    size: Some(bytes.len()),
                    modified: stamp.and_then(|stamp| stamp.modified),
                    is_modified: bytes.is_modified(),
                    ..widgets::FileInfo::new(side.label())
                }
            })
            .collect();
        widgets::file_info_bar(ui, ca_ui::format::probed_offset().unwrap_or(0), &sides);
    }

    /// The toolbar items this view declares, in the state it is in now.
    #[must_use]
    pub fn toolbar_items(&self) -> Vec<toolbar::Item> {
        let ready = self.status == Status::Ready;
        vec![
            toolbar::Item::command(
                "previous-section",
                Command::PreviousSection,
                Command::PreviousSection.label(),
                ready,
                NOT_READY,
            ),
            toolbar::Item::command(
                "previous-difference",
                Command::PreviousDifference,
                Command::PreviousDifference.label(),
                ready,
                NOT_READY,
            ),
            toolbar::Item::command(
                "next-difference",
                Command::NextDifference,
                Command::NextDifference.label(),
                ready,
                NOT_READY,
            ),
            toolbar::Item::command(
                "next-section",
                Command::NextSection,
                Command::NextSection.label(),
                ready,
                NOT_READY,
            ),
            toolbar::Item::separator("separator-1"),
            toolbar::Item::widget("filter", COMBO_WIDTH + 60.0),
            toolbar::Item::separator("separator-2"),
            toolbar::Item::widget("alignment", COMBO_WIDTH),
            toolbar::Item::widget("encoding", COMBO_WIDTH),
            toolbar::Item::separator("separator-3"),
            toolbar::Item::widget("width", 140.0),
            toolbar::Item::widget("hex-addresses", 120.0),
            toolbar::Item::widget("addresses", 100.0),
            toolbar::Item::widget("thumbnail", 100.0),
            toolbar::Item::widget("file-info", 90.0),
            toolbar::Item::separator("separator-4"),
            toolbar::Item::command(
                "copy-left",
                Command::CopyToLeft,
                Command::CopyToLeft.label(),
                ready,
                "Nothing to copy yet",
            ),
            toolbar::Item::command(
                "copy-right",
                Command::CopyToRight,
                Command::CopyToRight.label(),
                ready,
                "Nothing to copy yet",
            ),
            toolbar::Item::separator("separator-5"),
            toolbar::Item::command(
                "swap",
                Command::SwapSides,
                Command::SwapSides.label(),
                true,
                "",
            ),
            toolbar::Item::command("reload", Command::Reload, Command::Reload.label(), true, ""),
            toolbar::Item::widget("text-compare", 120.0),
            toolbar::Item::widget("parent-folders", 130.0),
            toolbar::Item::command("report", Command::CompareReport, "Report", ready, NOT_READY),
            toolbar::Item::separator("separator-6"),
            toolbar::Item::widget("font", 140.0),
        ]
    }

    #[allow(clippy::too_many_lines)]
    fn toolbar(&mut self, ui: &mut egui::Ui) -> Vec<ViewAction> {
        let mut actions = Vec::new();
        let running = self.status.is_running();
        let mut filter = self.filter;
        let mut context_rows = self.context_rows;
        let mut alignment = self.alignment;
        let mut encoding = self.encoding;
        let mut auto_width = self.auto_width;
        let mut width = self.bytes_per_row;
        let mut hex_addresses = self.hex_addresses;
        let mut show_addresses = self.show_addresses;
        let mut show_thumbnail = self.show_thumbnail;
        let mut show_file_info = self.show_file_info;
        let mut font_step = 0.0_f32;
        let mut cancel = false;
        let mut open: Option<SessionKind> = None;
        let id = self.id;
        let items = self.toolbar_items();
        let layout = toolbar::Layout::from_options(
            &ca_ui::options::runtime::current(ui.ctx()).stored.commands,
            toolbar::ToolbarView::Hex,
        );
        let outcome = toolbar::show_for(
            toolbar::ToolbarView::Hex,
            ui,
            id.with("toolbar"),
            &items,
            &layout,
            |ui, name| match name {
                "filter" => {
                    egui::ComboBox::from_id_salt(id.with("filter"))
                        .width(COMBO_WIDTH)
                        .selected_text(filter.label())
                        .show_ui(ui, |ui| {
                            for option in [
                                DisplayFilter::All,
                                DisplayFilter::Differences,
                                DisplayFilter::Same,
                                DisplayFilter::Context(0),
                            ] {
                                ui.selectable_value(&mut filter, option, option.label());
                            }
                        });
                    if matches!(filter, DisplayFilter::Context(_)) {
                        ui.add(egui::DragValue::new(&mut context_rows).range(0..=50));
                    }
                }
                "alignment" => {
                    egui::ComboBox::from_id_salt(id.with("alignment"))
                        .width(COMBO_WIDTH)
                        .selected_text(alignment_label(alignment))
                        .show_ui(ui, |ui| {
                            for option in [
                                ByteAlignment::Complete,
                                ByteAlignment::Fast,
                                ByteAlignment::None,
                            ] {
                                ui.selectable_value(
                                    &mut alignment,
                                    option,
                                    alignment_label(option),
                                );
                            }
                        });
                }
                "encoding" => {
                    egui::ComboBox::from_id_salt(id.with("encoding"))
                        .width(COMBO_WIDTH)
                        .selected_text(encoding.label())
                        .show_ui(ui, |ui| {
                            for option in chars::ENCODINGS {
                                ui.selectable_value(&mut encoding, option, option.label());
                            }
                        });
                }
                "width" => {
                    ui.checkbox(&mut auto_width, "Fit row");
                    if !auto_width {
                        ui.add(
                            egui::DragValue::new(&mut width)
                                .range(model::MIN_BYTES_PER_ROW..=model::MAX_BYTES_PER_ROW),
                        );
                    }
                }
                "hex-addresses" => {
                    ui.checkbox(&mut hex_addresses, "Hex addresses");
                }
                "addresses" => {
                    ca_ui::widgets::icon_toggle(
                        ui,
                        &mut show_addresses,
                        "Addresses",
                        ca_ui::icons::Icon::LineNumbers,
                    );
                }
                "thumbnail" => {
                    ca_ui::widgets::icon_toggle(
                        ui,
                        &mut show_thumbnail,
                        "Thumbnail",
                        ca_ui::icons::Icon::Thumbnail,
                    );
                }
                "file-info" => {
                    ca_ui::widgets::icon_toggle(
                        ui,
                        &mut show_file_info,
                        "File info",
                        ca_ui::icons::Icon::FileInfo,
                    );
                }
                "text-compare" => {
                    if ui
                        .add(ca_ui::widgets::IconButton::new(
                            "Text Compare",
                            Some(ca_ui::icons::Icon::SessionTextCompare),
                        ))
                        .clicked()
                    {
                        open = Some(SessionKind::TextCompare);
                    }
                }
                "parent-folders" => {
                    if ui
                        .add(ca_ui::widgets::IconButton::new(
                            "Parent Folders",
                            Some(ca_ui::icons::Icon::CompareParentFolders),
                        ))
                        .clicked()
                    {
                        open = Some(SessionKind::FolderCompare);
                    }
                }
                "font" => {
                    if ca_ui::widgets::icon_only_with_hover(
                        ui,
                        "A-",
                        Command::DecreaseFontSize.label(),
                        ca_ui::icons::Icon::FontDecrease,
                        None,
                    )
                    .clicked()
                    {
                        font_step = -1.0;
                    }
                    if ca_ui::widgets::icon_only_with_hover(
                        ui,
                        "A+",
                        Command::IncreaseFontSize.label(),
                        ca_ui::icons::Icon::FontIncrease,
                        None,
                    )
                    .clicked()
                    {
                        font_step = 1.0;
                    }
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
        self.hex_addresses = hex_addresses;
        self.show_addresses = show_addresses;
        self.show_thumbnail = show_thumbnail;
        self.show_file_info = show_file_info;
        match open {
            Some(SessionKind::TextCompare) => {
                actions.push(ViewAction::Open(OpenRequest::new(
                    SessionKind::TextCompare,
                    self.left_path.clone(),
                    self.right_path.clone(),
                )));
            }
            Some(SessionKind::FolderCompare) => {
                if let (Some(left), Some(right)) =
                    (self.left_path.parent(), self.right_path.parent())
                {
                    actions.push(ViewAction::Open(OpenRequest::new(
                        SessionKind::FolderCompare,
                        left.to_path_buf(),
                        right.to_path_buf(),
                    )));
                }
            }
            _ => {}
        }
        self.toolbar_room = outcome.room;
        self.toolbar_used = outcome.used;
        let mut copy: Option<Command> = None;
        let mut swap = false;
        let mut reload = false;
        match outcome.command {
            Some(
                command @ (Command::NextDifference
                | Command::PreviousDifference
                | Command::NextSection
                | Command::PreviousSection),
            ) => self.navigate(command),
            Some(command @ (Command::CopyToLeft | Command::CopyToRight)) => {
                copy = Some(command);
            }
            Some(Command::SwapSides) => swap = true,
            Some(Command::Reload) => reload = true,
            Some(Command::CompareReport) => self.report.request(),
            _ => {}
        }
        match copy {
            Some(Command::CopyToLeft) => self.copy_to(Side::Left),
            Some(Command::CopyToRight) => self.copy_to(Side::Right),
            _ => {}
        }
        if std::mem::discriminant(&filter) != std::mem::discriminant(&self.filter) {
            self.set_filter(filter);
        } else if context_rows != self.context_rows {
            self.context_rows = context_rows;
            self.refilter();
        }
        if encoding != self.encoding {
            self.encoding = encoding;
        }
        if alignment != self.alignment {
            self.alignment = alignment;
            self.recompare();
        }
        self.auto_width = auto_width;
        if !auto_width {
            self.set_bytes_per_row(width);
        }
        if font_step != 0.0 {
            self.set_font_size(self.font_size() + font_step);
        }
        if swap {
            self.request_reread(Reread::SwapSides);
        }
        if reload {
            self.request_reread(Reread::Reload);
        }
        if cancel {
            if let Some(job) = self.job.as_ref() {
                job.cancel();
            }
        }
        actions
    }

    /// The name the report display filter carries for what the view shows.
    #[must_use]
    pub const fn report_filter(&self) -> &'static str {
        match self.filter {
            DisplayFilter::Differences | DisplayFilter::Context(_) => "mismatches",
            DisplayFilter::Same => "matches",
            DisplayFilter::All => "all",
        }
    }

    /// The comparison the report is written from.
    #[must_use]
    ///
    /// The two byte buffers are shared rather than copied, and the hunks are
    /// the ones the view is drawing, so the document follows the display.
    pub fn report_payload(&self) -> (ca_ui::report::ReportMeta, ca_ui::report::Payload, u32) {
        let meta = ca_ui::report::ReportMeta::new(
            self.left_path.display().to_string(),
            self.right_path.display().to_string(),
        )
        .with_title(ReportKind::Hex.title());
        let payload = ca_ui::report::Payload::Hex(ca_ui::report::HexPayload {
            left: self.left_bytes.snapshot(),
            right: self.right_bytes.snapshot(),
            hunks: Arc::new(self.data.model.hunks().to_vec()),
        });
        (meta, payload, self.bytes_per_row)
    }

    fn report_panel(&mut self, ui: &mut egui::Ui) {
        if !self.report.is_open() {
            return;
        }
        if self.report.draw(ui) == ca_ui::report::ReportAction::Write {
            let (meta, payload, width) = self.report_payload();
            self.report.start(meta, payload, width);
        }
        if let Some(text) = self.report.take_clipboard() {
            self.pending_clipboard = Some(text);
        }
    }

    fn find_panel(&mut self, ui: &mut egui::Ui) {
        let controls = ca_ui::find::FindControls {
            whole_words: false,
            regex: false,
            selection_only: false,
            enter_searches: true,
        };
        let id = self.id;
        let kind = &mut self.find_kind;
        let request = self.find.show_find_with(ui, controls, |ui| {
            widgets::sized(ui, COMBO_WIDTH, |ui| {
                egui::ComboBox::from_id_salt(id.with("find-kind"))
                    .width(COMBO_WIDTH)
                    .selected_text(kind.label())
                    .show_ui(ui, |ui| {
                        for option in [PatternKind::Bytes, PatternKind::Text] {
                            ui.selectable_value(kind, option, option.label());
                        }
                    });
            });
        });
        match request {
            Some(ca_ui::find::PanelRequest::Next) => self.run_find(Direction::Forward),
            Some(ca_ui::find::PanelRequest::Previous) => self.run_find(Direction::Backward),
            Some(ca_ui::find::PanelRequest::Replace) => self.replace_match(),
            Some(ca_ui::find::PanelRequest::ReplaceAll) => self.replace_all_matches(),
            _ => {}
        }
    }

    fn goto_panel(&mut self, ui: &mut egui::Ui) {
        if !self.goto_open {
            return;
        }
        let mut go = false;
        let mut close = false;
        ui.horizontal_wrapped(|ui| {
            ui.label("Go to byte address");
            let field =
                ui.add(egui::TextEdit::singleline(&mut self.goto_text).desired_width(140.0));
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
            match parse_address(&self.goto_text) {
                Some(offset) => {
                    let side = self.active;
                    self.go_to_offset(side, offset, false);
                    self.message = None;
                    close = true;
                }
                None => {
                    self.message = Some(
                        "Enter a byte address, in decimal or as 0x and hex digits.".to_owned(),
                    );
                }
            }
        }
        if close {
            self.goto_open = false;
        }
    }

    /// True while a save runs.
    #[must_use]
    pub const fn is_saving(&self) -> bool {
        self.save_job.is_some()
    }

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
                Prompt::DiskChanged(_) => {
                    if ui.button("Overwrite").clicked() {
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
            Prompt::Closing | Prompt::Reread(_) => {
                "This tab has changes that are not written.".to_owned()
            }
        })
    }

    /// Answer the waiting question: `true` goes ahead, `false` stays put.
    ///
    /// Going ahead on the question about unwritten changes saves every
    /// modified side, then closes the tab or runs the command that asked.
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
            Prompt::DiskChanged(_) if !proceed => {
                self.prompt = None;
                self.saving = None;
                self.save_both_pending = false;
                self.close_requested = false;
                self.reread_after_save = None;
            }
            Prompt::DiskChanged(side) => self.save_side(side, true),
        }
    }

    /// Answer the question about changes that are not written, which a close
    /// or a command that reads the files again raised.
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
                self.reread_after_save = None;
            }
            (CloseChoice::Discard, None) => {
                self.left_bytes.mark_saved();
                self.right_bytes.mark_saved();
                self.close_requested = true;
            }
            (CloseChoice::Discard, Some(action)) => self.reread(action),
            (CloseChoice::Save, None) => {
                self.close_requested = true;
                self.save_next_modified();
            }
            (CloseChoice::Save, Some(action)) => {
                self.reread_after_save = Some(action);
                self.save_next_modified();
                self.reread_when_saved();
            }
        }
    }

    /// Run a command that reads the files again, asking first while a side
    /// holds a change that is not written.
    fn request_reread(&mut self, action: Reread) {
        if self.save_job.is_some() || self.prompt.is_some() {
            self.message = Some("Finish the save and its questions first.".to_owned());
            return;
        }
        if self.left_bytes.is_modified() || self.right_bytes.is_modified() {
            self.prompt = Some(Prompt::Reread(action));
            return;
        }
        self.reread(action);
    }

    /// Carry out a command that reads the files again, dropping every change.
    fn reread(&mut self, action: Reread) {
        self.reread_after_save = None;
        match action {
            Reread::Reload | Reread::Settings => self.restart(),
            Reread::Open => self.apply_fields(),
            Reread::SwapSides => {
                std::mem::swap(&mut self.left_path, &mut self.right_path);
                std::mem::swap(&mut self.left_field, &mut self.right_field);
                self.restart();
            }
        }
    }

    /// Run the command that waited on a save once every change is written.
    fn reread_when_saved(&mut self) {
        let written = self.save_job.is_none()
            && self.prompt.is_none()
            && !self.left_bytes.is_modified()
            && !self.right_bytes.is_modified();
        if !written {
            return;
        }
        if let Some(action) = self.reread_after_save.take() {
            self.reread(action);
        }
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            match &self.status {
                Status::Running(step) => {
                    ui.label(*step);
                }
                Status::Failed(reason) => {
                    ca_ui::widgets::notice_current(ui,ca_ui::icons::Icon::Error,16.0,&format!("Failed: {reason}"));
                }
                Status::Cancelled => {
                    ca_ui::widgets::notice_current(ui,ca_ui::icons::Icon::Warning,16.0,"Stopped before finishing");
                }
                Status::Ready => {
                    let counts = self.data.model.counts();
                    ui.label(format!(
                        "{} difference sections, {} differing bytes ({} changed, {} left only, {} right only)",
                        counts.sections,
                        counts.difference_bytes(),
                        counts.changed_bytes,
                        counts.left_only_bytes,
                        counts.right_only_bytes
                    ));
                }
            }
            ui.separator();
            let caret = self.caret(self.active);
            ui.label(format!(
                "{} byte address {}",
                self.active.label(),
                self.address_text(caret.offset)
            ));
            let selected = caret.selection_len();
            if selected > 0 {
                ui.separator();
                ui.label(format!("{selected} bytes selected"));
            }
            ui.separator();
            ui.label(self.edit_mode.label());
            if self.stale_at.is_some() {
                ui.separator();
                ui.label("Comparing the change");
            }
            if let Some(message) = &self.message {
                ui.separator();
                ca_ui::widgets::notice_current(ui,ca_ui::icons::Icon::Info,16.0,message.as_str());
            }
        });
    }

    /// One byte address as the address column writes it.
    fn address_text(&self, offset: u64) -> String {
        if self.hex_addresses {
            format!("{offset:08X}")
        } else {
            format!("{offset}")
        }
    }

    // --- painting --------------------------------------------------------

    fn rebuild_strip(&mut self, height: f32) {
        if !self.strip_stale && self.strip.matches(height, self.visible.len()) {
            return;
        }
        let visible = &self.visible;
        let model = &self.data.model;
        let rows = visible.len();
        let sections: Vec<(std::ops::Range<usize>, RowClass)> = model
            .sections()
            .iter()
            .filter_map(|section| {
                let start = visible.position_of(section.start)?;
                let end = visible
                    .position_of(section.end.saturating_sub(1))
                    .map_or(start + 1, |last| last + 1);
                let class = model
                    .row(section.start)
                    .map_or(RowClass::Changed, |row| row.class);
                (end > start).then_some((start..end, class))
            })
            .collect();
        self.strip = Strip::from_sections(height, rows, sections);
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
            let (background, _) = palette.byte_colors(byte_class_of_row(class));
            let Some(background) = background else {
                continue;
            };
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
        if let Some(position) = self.visible.position_of(self.row) {
            let y = rect.top() + strip.row_to_y(position);
            painter.line_segment(
                [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
                egui::Stroke::new(1.0, palette.thumbnail_caret),
            );
        }
        if let Some(pointer) = response.interact_pointer_pos() {
            let position = strip.drag_to_first_row(pointer.y - rect.top(), self.viewport_rows);
            self.scroll.go_to(
                position,
                self.viewport_height,
                self.row_height(),
                self.visible.len(),
            );
        }
    }

    /// The bytes a row shows fit in the pane, so the row width follows the pane.
    fn fit_bytes_per_row(&self, pane_width: f32, char_width: f32) -> u32 {
        let address = if self.show_addresses {
            self.address_columns()
        } else {
            0.0
        };
        let columns = (pane_width / char_width.max(1.0)) - address - 1.0;
        // Three columns for the two digits and their space, one for the
        // character, and one column of gap between the two areas.
        let bytes = (columns / 4.0).floor();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let bytes = bytes.max(1.0) as u32;
        bytes.clamp(model::MIN_BYTES_PER_ROW, model::MAX_BYTES_PER_ROW)
    }

    const fn address_columns(&self) -> f32 {
        if self.hex_addresses {
            ADDRESS_COLUMNS_HEX
        } else {
            ADDRESS_COLUMNS_DECIMAL
        }
    }

    /// The panes, the scrollbars and everything painted inside them.
    #[allow(clippy::too_many_lines)]
    fn rows(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        let row_height = self.row_height();
        let font = egui::FontId::monospace(self.font_size());
        let char_width = ui.fonts(|fonts| fonts.glyph_width(&font, '0')).max(1.0);

        let full = ui.available_rect_before_wrap();
        let body = egui::Rect::from_min_max(
            full.left_top(),
            egui::pos2(full.right() - SCROLLBAR, full.bottom()),
        );
        let _ = ui.allocate_rect(full, egui::Sense::hover());
        self.claim_keyboard(ui, body);
        let painter = ui.painter_at(full);
        let pane_width = ((body.width() - CENTER_WIDTH) / 2.0).max(1.0);
        if self.auto_width {
            let wanted = self.fit_bytes_per_row(pane_width, char_width);
            self.set_bytes_per_row(wanted);
        }

        let total = self.visible.len();
        self.body = body;
        self.viewport_height = body.height();
        self.scroll.clamp(body.height(), row_height, total);
        let range = self.scroll.visible(body.height(), row_height, total);
        self.viewport_rows = range.len();
        self.center_x = body.left() + pane_width;

        let clip = painter.with_clip_rect(body);
        clip.rect_filled(body, 0.0, palette.background);
        for side in [Side::Left, Side::Right] {
            let left = match side {
                Side::Left => body.left(),
                Side::Right => self.center_x + CENTER_WIDTH,
            };
            let rect = egui::Rect::from_min_max(
                egui::pos2(left, body.top()),
                egui::pos2(left + pane_width, body.bottom()),
            );
            let background = if side == self.active {
                palette.pane_focused
            } else {
                palette.pane_other
            };
            clip.rect_filled(rect, 0.0, background);
        }

        let geometry = |side: Side, y: f32| PaneGeometry {
            left: match side {
                Side::Left => body.left(),
                Side::Right => self.center_x + CENTER_WIDTH,
            } - self.horizontal,
            width: pane_width,
            char_width,
            row_height,
            y,
            address_columns: if self.show_addresses {
                self.address_columns()
            } else {
                0.0
            },
        };

        for position in range.clone() {
            let Some(index) = self.visible.row_at(position) else {
                continue;
            };
            let Some(row) = self.data.model.row(index) else {
                continue;
            };
            let y = body.top() + self.scroll.row_y(position, row_height);
            for side in [Side::Left, Side::Right] {
                self.paint_row(&clip, &geometry(side, y), palette, &font, side, &row);
            }
        }
        self.vertical_scrollbar(ui, &painter, palette, body, (row_height, total));
    }

    /// Paint one side of one row.
    fn paint_row(
        &self,
        painter: &egui::Painter,
        geometry: &PaneGeometry,
        palette: &Palette,
        font: &egui::FontId,
        side: Side,
        row: &HexRow,
    ) {
        let y = geometry.y;
        let row_rect = egui::Rect::from_min_size(
            egui::pos2(geometry.left, y),
            egui::vec2(geometry.width, geometry.row_height),
        );
        let Some(span) = row.side(side) else {
            paint_gap(painter, row_rect, palette);
            return;
        };
        let text_top = y + (geometry.row_height - self.font_size()) / 2.0 - 1.0;
        let mut x = geometry.left;
        if geometry.address_columns > 0.0 {
            let width = geometry.address_columns * geometry.char_width;
            painter.rect_filled(
                egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(width, geometry.row_height)),
                0.0,
                palette.address_background,
            );
            painter.text(
                egui::pos2(x, text_top),
                egui::Align2::LEFT_TOP,
                self.address_text(span.start),
                font.clone(),
                palette.address_text,
            );
            x += width;
        }
        let hex_x = x;
        let chars_x = hex_x
            + f32::from(u16::try_from(self.bytes_per_row).unwrap_or(u16::MAX))
                * 3.0
                * geometry.char_width
            + geometry.char_width;
        let selection = self.caret(side).selection();
        let caret = self.caret(side);
        for column in 0..span.len {
            let offset = span.start + u64::from(column);
            let class = self.byte_class(row, side, column);
            let (background, text_color) = palette.byte_colors(class);
            let byte = self.buffer(side).byte(offset);
            let selected = selection
                .as_ref()
                .is_some_and(|range| range.contains(&offset));
            #[allow(clippy::cast_precision_loss)]
            let step = column as f32;
            let hex_rect = egui::Rect::from_min_size(
                egui::pos2(hex_x + step * 3.0 * geometry.char_width, y),
                egui::vec2(2.0 * geometry.char_width, geometry.row_height),
            );
            let char_rect = egui::Rect::from_min_size(
                egui::pos2(chars_x + step * geometry.char_width, y),
                egui::vec2(geometry.char_width, geometry.row_height),
            );
            if selected {
                painter.rect_filled(hex_rect, 0.0, palette.selection);
                painter.rect_filled(char_rect, 0.0, palette.selection);
            } else if let Some(background) = background {
                painter.rect_filled(hex_rect, 0.0, background);
                painter.rect_filled(char_rect, 0.0, background);
            }
            let (digits, character, color) = match byte {
                Some(byte) => {
                    let digits = edit::hex_digits(byte);
                    (
                        [digits[0], digits[1]].iter().collect::<String>(),
                        self.encoding.character(byte),
                        text_color,
                    )
                }
                // A row whose bytes are not resident still paints, so a load
                // that stopped part way shows what it does not have.
                None => ("--".to_owned(), '?', palette.unavailable_text),
            };
            painter.text(
                hex_rect.left_top() + egui::vec2(0.0, text_top - y),
                egui::Align2::LEFT_TOP,
                digits,
                font.clone(),
                color,
            );
            painter.text(
                char_rect.left_top() + egui::vec2(0.0, text_top - y),
                egui::Align2::LEFT_TOP,
                character.to_string(),
                font.clone(),
                color,
            );
            if side == self.active && caret.offset == offset {
                let bar = match caret.area {
                    Area::Hex => hex_rect,
                    Area::Chars => char_rect,
                };
                painter.rect_stroke(
                    bar,
                    0.0,
                    egui::Stroke::new(1.0, palette.caret),
                    egui::StrokeKind::Inside,
                );
            }
        }
    }

    /// How one byte of a row compares with its counterpart.
    fn byte_class(&self, row: &HexRow, side: Side, column: u32) -> ByteClass {
        if row.class == RowClass::Same {
            return ByteClass::Same;
        }
        let here = row
            .side(side)
            .and_then(|span| self.byte_at(side, span, column));
        let there = row
            .side(side.other())
            .and_then(|span| self.byte_at(side.other(), span, column));
        match (here, there) {
            (Some(a), Some(b)) if a == b => ByteClass::Same,
            (Some(_), Some(_)) => ByteClass::Different,
            (Some(_), None) => match side {
                Side::Left => ByteClass::LeftOnly,
                Side::Right => ByteClass::RightOnly,
            },
            _ => ByteClass::Gap,
        }
    }

    fn byte_at(&self, side: Side, span: RowSpan, column: u32) -> Option<u8> {
        (column < span.len).then(|| self.buffer(side).byte(span.start + u64::from(column)))?
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
        painter.rect_filled(track, 0.0, palette.address_background);
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
    }

    // --- input -----------------------------------------------------------

    /// The widget that stands for the byte panes when the keyboard is handed
    /// out.
    fn body_id(&self) -> egui::Id {
        self.id.with("body")
    }

    /// Register the byte panes as a widget that holds the keyboard.
    ///
    /// The panes take the keyboard on a press inside them, and whenever no
    /// other widget holds it. While they hold it, Tab and the arrow keys stay
    /// with them: Tab moves between the two areas, which egui would otherwise
    /// read as a move to the next widget.
    fn claim_keyboard(&self, ui: &egui::Ui, body: egui::Rect) {
        if !ui.is_enabled() {
            return;
        }
        let id = self.body_id();
        let response = ui.interact(body, id, egui::Sense::click());
        let pressed = response.hovered() && ui.input(|input| input.pointer.primary_pressed());
        if pressed || ui.memory(|memory| memory.focused().is_none()) {
            response.request_focus();
        }
        if response.has_focus() {
            ui.memory_mut(|memory| {
                memory.set_focus_lock_filter(
                    id,
                    egui::EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: false,
                    },
                );
            });
        }
    }

    fn handle_keys(&mut self, ui: &egui::Ui) {
        if !ui.is_enabled() {
            return;
        }
        let pass = ui.ctx().cumulative_pass_nr();
        if self.input_pass == Some(pass) {
            return;
        }
        self.input_pass = Some(pass);
        if !self.body.is_positive() {
            return;
        }
        // A text field that holds the keyboard reads the same events without
        // consuming them, so the bytes take keys only while the panes hold it.
        let owns_keys = ui.ctx().memory(egui::Memory::focused) == Some(self.body_id());
        let events = ui.input(|input| input.events.clone());
        let page = u32::try_from(self.viewport_rows.max(1)).unwrap_or(1);
        for event in events {
            match event {
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } if owns_keys => {
                    self.handle_key(key, modifiers, page);
                }
                egui::Event::Text(text) if owns_keys => self.handle_text(&text),
                egui::Event::Paste(text) if owns_keys => self.paste(&text),
                egui::Event::MouseWheel { delta, .. } => {
                    self.scroll.scroll_by(
                        -delta.y,
                        self.viewport_height,
                        self.row_height(),
                        self.visible.len(),
                    );
                    // The wheel scrolls the display sideways as well, which is
                    // what a row wider than the pane needs.
                    self.horizontal = (self.horizontal - delta.x).max(0.0);
                }
                _ => {}
            }
        }
    }

    fn handle_text(&mut self, text: &str) {
        let Some(key) = text.chars().next() else {
            return;
        };
        match self.caret(self.active).area {
            Area::Hex => {
                if let Some(value) = edit::hex_value(key) {
                    self.type_digit(value);
                }
            }
            Area::Chars => {
                if let Some(bytes) = self.encoding.encode(&key.to_string()) {
                    if let Some(byte) = bytes.first() {
                        self.type_byte(*byte);
                    }
                }
            }
        }
    }

    fn handle_key(&mut self, key: egui::Key, modifiers: egui::Modifiers, page: u32) {
        use egui::Key;
        let side = self.active;
        let step = u64::from(self.bytes_per_row);
        let offset = self.caret(side).offset;
        let extend = modifiers.shift;
        let limit = self.buffer(side).len();
        let target = match key {
            Key::ArrowLeft => Some(offset.saturating_sub(1)),
            Key::ArrowRight => Some(offset.saturating_add(1).min(limit)),
            Key::ArrowUp => Some(offset.saturating_sub(step)),
            Key::ArrowDown => Some(offset.saturating_add(step).min(limit)),
            Key::PageUp => Some(offset.saturating_sub(step * u64::from(page))),
            Key::PageDown => Some(offset.saturating_add(step * u64::from(page)).min(limit)),
            Key::Home if modifiers.command => Some(0),
            Key::End if modifiers.command => Some(limit),
            Key::Home => Some(offset - offset % step.max(1)),
            Key::End => Some((offset - offset % step.max(1) + step.saturating_sub(1)).min(limit)),
            _ => None,
        };
        if let Some(target) = target {
            self.go_to_offset(side, target, extend);
            return;
        }
        match key {
            // Moving between the two areas keeps the selection, so a run of
            // bytes picked in one area can be read in the other.
            Key::Tab => {
                let area = self.caret(side).area;
                self.caret_mut(side).area = match area {
                    Area::Hex => Area::Chars,
                    Area::Chars => Area::Hex,
                };
                self.pending_nibble = None;
            }
            Key::Delete if !modifiers.command => self.delete(true),
            Key::Backspace => self.delete(false),
            _ => {}
        }
    }

    /// Put the caret where a click landed.
    fn pointer(&mut self, ui: &egui::Ui, char_width: f32) {
        let Some(pointer) = ui.ctx().pointer_interact_pos() else {
            return;
        };
        if !self.body.contains(pointer) || !ui.ctx().input(|input| input.pointer.primary_pressed())
        {
            return;
        }
        let side = if pointer.x < self.center_x {
            Side::Left
        } else {
            Side::Right
        };
        let row_height = self.row_height();
        let position = self
            .scroll
            .row_under(pointer.y - self.body.top(), row_height);
        let Some(index) = self.visible.row_at(position) else {
            return;
        };
        let Some(row) = self.data.model.row(index) else {
            return;
        };
        let Some(span) = row.side(side) else {
            return;
        };
        let pane_left = match side {
            Side::Left => self.body.left(),
            Side::Right => self.center_x + CENTER_WIDTH,
        } - self.horizontal;
        let address = if self.show_addresses {
            self.address_columns()
        } else {
            0.0
        };
        let hex_x = pane_left + address * char_width;
        let hex_width =
            f32::from(u16::try_from(self.bytes_per_row).unwrap_or(u16::MAX)) * 3.0 * char_width;
        let chars_x = hex_x + hex_width + char_width;
        let (area, column) = if pointer.x >= chars_x {
            (Area::Chars, ((pointer.x - chars_x) / char_width).max(0.0))
        } else {
            (
                Area::Hex,
                ((pointer.x - hex_x) / (3.0 * char_width)).max(0.0),
            )
        };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let column = (column as u32).min(span.len.saturating_sub(1));
        self.caret_mut(side).area = area;
        self.go_to_offset(side, span.start + u64::from(column), false);
        self.row = index;
    }
}

/// What the layout of one pane needs.
struct PaneGeometry {
    left: f32,
    width: f32,
    char_width: f32,
    row_height: f32,
    /// Top edge of the row this geometry paints.
    y: f32,
    address_columns: f32,
}

/// Fill a row a side has no bytes on.
fn paint_gap(painter: &egui::Painter, rect: egui::Rect, palette: &Palette) {
    painter.rect_filled(rect, 0.0, palette.gap_background);
    let step = rect.height().max(2.0);
    let mut x = rect.left();
    while x < rect.right() {
        painter.line_segment(
            [
                egui::pos2(x, rect.bottom()),
                egui::pos2((x + step).min(rect.right()), rect.top()),
            ],
            egui::Stroke::new(1.0, palette.gap_pattern),
        );
        x += step;
    }
}

/// The byte class a whole row of one class paints as in the overview strip.
const fn byte_class_of_row(class: RowClass) -> ByteClass {
    match class {
        RowClass::Same => ByteClass::Same,
        RowClass::Changed => ByteClass::Different,
        RowClass::LeftOnly => ByteClass::LeftOnly,
        RowClass::RightOnly => ByteClass::RightOnly,
    }
}

const fn alignment_label(alignment: ByteAlignment) -> &'static str {
    match alignment {
        ByteAlignment::Complete => "Complete",
        ByteAlignment::Fast => "Fast",
        ByteAlignment::None => "None",
    }
}

/// A byte address written in decimal, or with a `0x` prefix in hexadecimal.
#[must_use]
pub fn parse_address(text: &str) -> Option<u64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if let Some(digits) = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .or_else(|| text.strip_prefix('$'))
    {
        return u64::from_str_radix(digits, 16).ok();
    }
    text.parse::<u64>().ok()
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

impl ca_ui::view::ViewFactory for HexView {
    fn create(left: PathBuf, right: PathBuf, context: &ViewContext, salt: u64) -> Self {
        Self::new(left, right, context, salt)
    }

    fn create_from(request: &ca_ui::view::OpenRequest, context: &ViewContext, salt: u64) -> Self {
        let mut view = Self::empty(request.left.clone(), request.right.clone(), context, salt);
        view.read_only = request.read_only;
        view.restart();
        view
    }
}

impl SessionView for HexView {
    fn kind(&self) -> Option<ca_session::SessionKind> {
        Some(ca_session::SessionKind::HexCompare)
    }

    fn title(&self) -> String {
        let modified = if self.left_bytes.is_modified() || self.right_bytes.is_modified() {
            "* "
        } else {
            ""
        };
        format!(
            "{modified}{} - {}",
            file_name(&self.left_path),
            file_name(&self.right_path)
        )
    }

    fn tick(&mut self) {
        self.poll();
    }

    fn ui(&mut self, ui: &mut egui::Ui, context: &ViewContext) -> Vec<ViewAction> {
        let palette = ca_ui::theme::hex::palette(theme_variant(ui));
        let filter_name = self.report_filter();
        let _ = context;
        if let Some(due) = self.stale_at {
            let now = Instant::now();
            if now >= due {
                self.recompare();
            } else {
                ui.ctx().request_repaint_after(due - now);
            }
        }
        self.follow_options(ui.ctx());
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
            actions.extend(self.toolbar(ui));
            self.find_panel(ui);
            self.goto_panel(ui);
            self.prompt_panel(ui);
            self.report_panel(ui);
        });
        egui::TopBottomPanel::bottom(self.id.with("status")).show_inside(ui, |ui| {
            self.status_bar(ui);
        });
        if self.show_thumbnail {
            egui::SidePanel::left(self.id.with("thumb"))
                .exact_width(THUMBNAIL_WIDTH)
                .resizable(false)
                .show_inside(ui, |ui| {
                    self.thumbnail_strip(ui, palette);
                });
        }
        egui::CentralPanel::default().show_inside(ui, |ui| {
            let font = egui::FontId::monospace(self.font_size());
            let char_width = ui.fonts(|fonts| fonts.glyph_width(&font, '0')).max(1.0);
            self.rows(ui, palette);
            self.pointer(ui, char_width);
        });
        actions
    }

    fn commands(&self) -> Vec<ca_ui::view::CommandState> {
        ca_ui::view::declare(HANDLED, |command| self.accepts(command))
    }

    fn accepts(&self, command: Command) -> bool {
        let ready = self.status == Status::Ready;
        match command {
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
            | Command::Copy
            | Command::Recompare
            | Command::ToggleOverwrite
            | Command::CompareReport => ready,
            Command::IncreaseFontSize
            | Command::DecreaseFontSize
            | Command::ResetFontSize
            | Command::Reload => true,
            Command::OpenFile => self.picker.is_none(),
            Command::Undo => {
                !self.buffer(self.active).is_read_only() && self.buffer(self.active).can_undo()
            }
            Command::Redo => {
                !self.buffer(self.active).is_read_only() && self.buffer(self.active).can_redo()
            }
            Command::Cut | Command::Paste | Command::Replace => self.editable(),
            Command::CopyToRight => ready && !self.right_bytes.is_read_only(),
            Command::CopyToLeft => ready && !self.left_bytes.is_read_only(),
            Command::SaveFile => {
                !self.editing_off()
                    && self.buffer(self.active).is_modified()
                    && self.save_job.is_none()
            }
            Command::SaveFileAs => {
                ready && !self.editing_off() && self.save_job.is_none() && self.picker.is_none()
            }
            Command::SaveBoth => {
                !self.editing_off()
                    && (self.left_bytes.is_modified() || self.right_bytes.is_modified())
                    && self.save_job.is_none()
            }
            Command::Cancel => {
                self.status.is_running() || self.find_job.is_some() || self.replace_job.is_some()
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
            Command::ShowAll => self.set_filter(DisplayFilter::All),
            Command::ShowDifferences => self.set_filter(DisplayFilter::Differences),
            Command::ShowSame => self.set_filter(DisplayFilter::Same),
            Command::ShowContext => self.set_filter(DisplayFilter::Context(self.context_rows)),
            Command::IncreaseFontSize | Command::DecreaseFontSize | Command::ResetFontSize => {
                self.font.run(command);
                self.strip_stale = true;
            }
            Command::Reload => self.request_reread(Reread::Reload),
            Command::OpenFile => self.open_picker(match self.active {
                Side::Left => Target::Left,
                Side::Right => Target::Right,
            }),
            Command::CompareReport => self.report.request(),
            Command::Recompare => self.recompare(),
            Command::SwapSides => self.request_reread(Reread::SwapSides),
            Command::Cancel => {
                if let Some(job) = self.job.as_ref() {
                    job.cancel();
                }
                if let Some(job) = self.find_job.take() {
                    job.cancel();
                }
                if let Some(job) = self.replace_job.take() {
                    job.cancel();
                }
                self.find.close();
                self.goto_open = false;
                self.message = None;
            }
            Command::Undo => {
                let side = self.active;
                if let Some(at) = self.buffer_mut(side).undo() {
                    self.go_to_offset(side, at, false);
                    self.note_change();
                }
            }
            Command::Redo => {
                let side = self.active;
                if let Some(at) = self.buffer_mut(side).redo() {
                    self.go_to_offset(side, at, false);
                    self.note_change();
                }
            }
            Command::SelectAll => self.select_all(),
            Command::Cut => {
                let bytes = self.selected_bytes(self.active);
                self.pending_clipboard = Some(self.clipboard_text(&bytes));
                self.delete(true);
            }
            Command::Copy => {
                let bytes = self.selected_bytes(self.active);
                self.pending_clipboard = Some(self.clipboard_text(&bytes));
            }
            Command::CopyToRight => self.copy_to(Side::Right),
            Command::CopyToLeft => self.copy_to(Side::Left),
            Command::ToggleOverwrite => self.edit_mode = self.edit_mode.toggled(),
            Command::SaveFile => {
                let side = self.active;
                self.save_side(side, false);
            }
            Command::SaveFileAs => self.open_save_picker(),
            Command::SaveBoth => self.save_next_modified(),
            Command::Find => self.find.open_find(),
            Command::Replace => self.find.open_replace(),
            Command::FindNext => self.run_find(Direction::Forward),
            Command::FindPrevious => self.run_find(Direction::Backward),
            Command::GoTo => {
                self.goto_open = true;
                self.goto_focus = true;
            }
            Command::Paste => self.paste_requested = true,
            _ => {}
        }
    }

    fn wants_close(&self) -> bool {
        self.close_requested
            && !self.left_bytes.is_modified()
            && !self.right_bytes.is_modified()
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
        if !save::needs_close_prompt(
            self.left_bytes.is_modified(),
            self.right_bytes.is_modified(),
        ) {
            return true;
        }
        self.prompt = Some(Prompt::Closing);
        false
    }

    fn apply_settings(&mut self, settings: &ca_session::settings::SessionSettings) {
        if let ca_session::settings::SessionSettings::HexCompare(hex) = settings {
            self.apply_session_settings(hex);
        }
    }

    fn settings(&self) -> Option<ca_session::settings::SessionSettings> {
        Some(ca_session::settings::SessionSettings::HexCompare(
            self.session_settings(),
        ))
    }

    fn holds_unwritten_edits(&self) -> bool {
        self.left_bytes.is_modified() || self.right_bytes.is_modified()
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

    fn on_close(&mut self) {
        if let Some(job) = self.job.take() {
            job.cancel();
        }
        if let Some(job) = self.picker.take() {
            job.cancel();
        }
        if let Some(job) = self.find_job.take() {
            job.cancel();
        }
        if let Some(job) = self.replace_job.take() {
            job.cancel();
        }
        if let Some(job) = self.save_job.take() {
            job.cancel();
        }
        // Nothing is left to post a terminal message, so the view must not be
        // left reporting work that can no longer report back.
        if self.status.is_running() {
            self.status = Status::Cancelled;
        }
    }
}

/// The theme variant the surrounding context is painting in.
fn theme_variant(ui: &egui::Ui) -> ca_ui::theme::Variant {
    ca_ui::theme::Variant::from_dark_mode(ui.visuals().dark_mode)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{parse_address, Area, Caret, Command, HexView, Side, Target};
    use ca_ui::testing::context;
    use ca_ui::view::SessionView;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    #[test]
    fn an_address_reads_in_decimal_and_in_hexadecimal() {
        assert_eq!(parse_address("16"), Some(16));
        assert_eq!(parse_address(" 0x10 "), Some(16));
        assert_eq!(parse_address("$FF"), Some(255));
        // A byte address past the signed thirty two bit range still reads.
        assert_eq!(parse_address("0x1FFFFFFFF"), Some(8_589_934_591));
        assert_eq!(parse_address(""), None);
        assert_eq!(parse_address("zz"), None);
    }

    #[test]
    fn a_selection_reads_the_same_whichever_end_it_was_dragged_from() {
        let forward = Caret {
            offset: 9,
            area: Area::Hex,
            anchor: Some(4),
        };
        let backward = Caret {
            offset: 4,
            area: Area::Hex,
            anchor: Some(9),
        };
        assert_eq!(forward.selection(), Some(4..9));
        assert_eq!(backward.selection(), Some(4..9));
        assert_eq!(forward.selection_len(), 5);
        assert_eq!(
            Caret {
                offset: 4,
                area: Area::Hex,
                anchor: Some(4)
            }
            .selection(),
            None
        );
        assert_eq!(Caret::default().selection_len(), 0);
    }

    #[test]
    fn a_title_uses_the_file_names() {
        let view = HexView::from_data(
            PathBuf::from("/tmp/left.bin"),
            PathBuf::from("/tmp/right.bin"),
            &context(),
            1,
            crate::jobs::HexData::default(),
        );
        assert_eq!(view.title(), "left.bin - right.bin");
        assert_eq!(HexView::kind().title(), "Hex Compare");
    }

    #[test]
    fn edits_during_save_remain_unsaved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bytes.bin");
        let mut view = HexView::from_data(
            path.clone(),
            path.clone(),
            &context(),
            1,
            crate::jobs::HexData::default(),
        );
        view.left_bytes = crate::edit::ByteBuffer::new(vec![1]);
        view.left_bytes.replace(0, 1, &[2]);
        view.save_side(crate::model::Side::Left, false);
        view.left_bytes.replace(0, 1, &[3]);
        let deadline = Instant::now() + Duration::from_secs(20);
        while view.save_job.is_some() && Instant::now() < deadline {
            view.poll_save();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(view.save_job.is_none());
        assert_eq!(std::fs::read(&path).unwrap(), [2]);
        assert!(view.left_bytes.is_modified());
        view.left_bytes.undo();
        view.left_bytes.undo();
        assert!(
            view.left_bytes.is_modified(),
            "the original revision is no longer on disk"
        );
    }

    #[test]
    fn save_as_during_a_save_keeps_the_original_target() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("original.bin");
        let another = dir.path().join("another.bin");
        std::fs::write(&original, [1u8]).unwrap();
        let mut view = HexView::from_data(
            original.clone(),
            original.clone(),
            &context(),
            1,
            crate::jobs::HexData::default(),
        );
        view.left_bytes = crate::edit::ByteBuffer::new(vec![1]);
        view.left_bytes.replace(0, 1, &[2]);
        view.save_side(crate::model::Side::Left, false);
        view.save_active_as(&another);
        assert_eq!(view.left_path, original);
        assert!(!another.exists());
        let deadline = Instant::now() + Duration::from_secs(20);
        while view.save_job.is_some() && Instant::now() < deadline {
            view.poll_save();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(std::fs::read(&original).unwrap(), [2]);
        assert!(!another.exists());
    }

    #[test]
    fn failed_save_as_keeps_the_original_name_and_unsaved_edit() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("original.bin");
        let invalid = dir.path().join("folder");
        std::fs::write(&original, [1u8]).unwrap();
        std::fs::create_dir(&invalid).unwrap();
        let mut view = HexView::from_data(
            original.clone(),
            original.clone(),
            &context(),
            1,
            crate::jobs::HexData::default(),
        );
        view.left_bytes = crate::edit::ByteBuffer::new(vec![1]);
        view.left_bytes.replace(0, 1, &[2]);
        view.save_active_as(&invalid);
        let deadline = Instant::now() + Duration::from_secs(20);
        while view.save_job.is_some() && Instant::now() < deadline {
            view.poll_save();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(view.left_path, original);
        assert!(view.left_bytes.is_modified());
        assert_eq!(std::fs::read(&original).unwrap(), [1]);
    }

    #[test]
    fn successful_save_as_changes_the_name_after_writing() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("original.bin");
        let another = dir.path().join("another.bin");
        std::fs::write(&original, [1u8]).unwrap();
        let mut view = HexView::from_data(
            original.clone(),
            original.clone(),
            &context(),
            1,
            crate::jobs::HexData::default(),
        );
        view.left_bytes = crate::edit::ByteBuffer::new(vec![1]);
        view.left_bytes.replace(0, 1, &[2]);
        view.save_active_as(&another);
        assert_eq!(view.left_path, original);
        let deadline = Instant::now() + Duration::from_secs(20);
        while view.save_job.is_some() && Instant::now() < deadline {
            view.poll_save();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(view.left_path, another);
        assert_eq!(std::fs::read(&original).unwrap(), [1]);
        assert_eq!(std::fs::read(&another).unwrap(), [2]);
    }

    #[test]
    fn a_save_as_callback_is_refused_after_editing_is_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("original.bin");
        let another = dir.path().join("another.bin");
        std::fs::write(&original, [1u8]).unwrap();
        let mut view = HexView::from_data(
            original.clone(),
            original,
            &context(),
            1,
            crate::jobs::HexData::default(),
        );
        view.left_bytes = crate::edit::ByteBuffer::new(vec![1]);
        let mut settings = view.session_settings();
        settings.specs.disable_editing = true;
        view.apply_session_settings(&settings);
        assert!(!view.accepts(Command::SaveFileAs));

        // A picker opened before the setting changed can still report a path.
        view.save_active_as(&another);
        assert!(view.save_job.is_none());
        assert!(!another.exists());
        assert_eq!(
            view.message.as_deref(),
            Some("Editing is turned off for this session")
        );
    }

    #[test]
    fn save_both_finishes_both_files_before_closing() {
        for closing in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let left = dir.path().join("left.bin");
            let right = dir.path().join("right.bin");
            let mut view = HexView::from_data(
                left.clone(),
                right.clone(),
                &context(),
                1,
                crate::jobs::HexData::default(),
            );
            view.left_bytes.replace(0, 0, &[1]);
            view.right_bytes.replace(0, 0, &[2]);
            view.close_requested = closing;
            view.run(Command::SaveBoth);
            assert!(!view.may_close());
            assert!(!view.wants_close());
            let deadline = Instant::now() + Duration::from_secs(20);
            while view.save_job.is_some() && Instant::now() < deadline {
                view.poll_save();
                std::thread::sleep(Duration::from_millis(2));
            }
            assert_eq!(std::fs::read(&left).unwrap(), [1]);
            assert_eq!(std::fs::read(&right).unwrap(), [2]);
            assert_eq!(view.wants_close(), closing);
            assert!(view.may_close());
        }
    }

    #[test]
    fn browsing_for_a_replacement_file_puts_a_picker_in_flight_and_blocks_another_dialog() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left.bin");
        let right = dir.path().join("right.bin");
        std::fs::write(&left, [1u8, 2, 3]).unwrap();
        std::fs::write(&right, [1u8, 2, 4]).unwrap();
        let mut view = HexView::new(left, right, &context(), 9);
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline && view.model().row_count() == 0 {
            view.tick();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(
            view.accepts(Command::SaveFileAs),
            "the dialog starts out available"
        );
        view.active = Side::Right;
        view.run(Command::OpenFile);
        assert!(view.picker.is_some(), "Open File must start a picker job");
        assert_eq!(view.picker_target, Target::Right);
        assert!(
            !view.accepts(Command::SaveFileAs),
            "a picker already in flight blocks another dialog from opening"
        );
    }

    #[test]
    fn a_click_at_a_fractional_scroll_lands_on_the_row_painted_under_it() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left.bin");
        let right = dir.path().join("right.bin");
        let bytes: Vec<u8> = (0..4096u32).map(|value| (value % 251) as u8).collect();
        std::fs::write(&left, &bytes).unwrap();
        std::fs::write(&right, &bytes).unwrap();
        let mut view = HexView::new(left, right, &context(), 1);
        let ctx = egui::Context::default();
        let frame = |view: &mut HexView, events: Vec<egui::Event>| {
            view.tick();
            let input = ca_ui::testing::event_input(1_000.0, 600.0, events);
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    view.ui(ui, &context());
                });
            });
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        while view.model().row_count() == 0 && Instant::now() < deadline {
            frame(&mut view, Vec::new());
            std::thread::sleep(Duration::from_millis(2));
        }
        frame(&mut view, Vec::new());
        let row_height = view.row_height();
        view.scroll.scroll_by(
            row_height * 2.5,
            view.viewport_height,
            row_height,
            view.visible.len(),
        );
        assert!(view.scroll.fraction() > 0.0);
        let at = egui::pos2(view.body.left() + 1.0, view.body.top() + row_height * 0.75);
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
        assert_eq!(view.caret_row(), 3);
    }
}
