//! The one record comparison display the three views share.
//!
//! Two panes sit side by side over one row model. A key and its values sit on
//! the same row in both panes, aligned by name. A row the other side does not
//! hold is filler on that side. Rows are virtualized: a frame lays out only the
//! rows the viewport covers.

use crate::flavor::Flavor;
use crate::jobs::{self, Pipeline, RecordMessage, Side as SideData, Stage};
use crate::model::{DisplayFilter, Listing, Node, Side};
use crate::search::{self, FindMessage};
use crate::session_options::{belongs_to, rules_from, Rules};
use crate::text::{hex_lines, inline_difference};
use ca_records::RecordValue;
use ca_session::settings::SessionSettings;
use ca_ui::command::{Command, MenuView};
use ca_ui::dialog::{DialogMessage, Target};
use ca_ui::report::{Payload, ReportMeta, ViewReport};
use ca_ui::scroll::RowScroll;
use ca_ui::theme::records::{Palette, RecordClass};
use ca_ui::theme::Variant;
use ca_ui::toolbar;
use ca_ui::view::{CommandState, SessionView, ViewAction, ViewContext};
use ca_ui::widgets;
use ca_ui::worker::{Job, Terminal};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub mod editing;

/// Row height as a multiple of the display font size.
const ROW_HEIGHT_RATIO: f32 = 1.81;
/// Height of the column header row.
const HEADER_HEIGHT: f32 = 22.0;
/// Width of the gap between the two panes.
const SEPARATOR: f32 = 6.0;
/// Thickness of the scrollbar.
const SCROLLBAR: f32 = 12.0;
/// Width of the thumbnail strip.
const STRIP_WIDTH: f32 = 14.0;
/// Room kept for text inside a column.
const CELL_PAD: f32 = 4.0;
/// Indent of one tree level.
const INDENT: f32 = 14.0;
/// Width of the open and close mark in front of a key.
const EXPANDER: f32 = 14.0;
/// Share of a pane the name column takes.
const NAME_SHARE: f32 = 0.42;
/// Share of a pane the type column takes.
const TYPE_SHARE: f32 = 0.16;
/// Spacing of the hatch lines drawn on filler.
const HATCH_STEP: f32 = 10.0;
/// Characters of a value the pane lays out at most; the details area shows
/// the whole value.
const PANE_TEXT_LIMIT: usize = 400;
/// Characters of a value the details area shows at most.
const DETAILS_TEXT_LIMIT: usize = 4_000;
/// Width the display filter drop down is laid out in.
const FILTER_COMBO_WIDTH: f32 = 150.0;
/// Room the details area takes under the panes.
const DETAILS_HEIGHT: f32 = 58.0;
/// Room the hex details area takes under the panes.
const HEX_HEIGHT: f32 = 150.0;
/// Room the status bar takes.
const STATUS_HEIGHT: f32 = 26.0;

/// Every command a record view answers for.
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
    Command::NextDifference,
    Command::PreviousDifference,
    Command::ExpandAll,
    Command::CollapseAll,
    Command::Copy,
    Command::SelectAll,
    Command::Find,
    Command::FindNext,
    Command::FindPrevious,
    Command::ToggleLineDetails,
    Command::HexDetails,
    Command::Thumbnail,
    Command::IncreaseFontSize,
    Command::DecreaseFontSize,
    Command::ResetFontSize,
    Command::Reload,
    Command::Recompare,
    Command::SwapSides,
    Command::Cancel,
];

/// Why the importance switch is disabled on a registry comparison.
const NO_IMPORTANCE: &str = "A registry comparison has no importance setting";

/// Why reading the sources again is refused while edits wait.
const UNWRITTEN: &str = "Save or undo the edits first. Reading the sources again would lose them.";

/// Where a comparison currently stands.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Progress {
    /// A step of the pipeline is running.
    Running(Stage),
    /// A comparison is on screen.
    Ready,
    /// The comparison could not be produced.
    Failed(String),
    /// The run stopped before producing a comparison.
    Cancelled,
}

impl Progress {
    const fn is_running(&self) -> bool {
        matches!(self, Self::Running(_))
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
}

/// A registry, version or media comparison tab.
#[allow(clippy::struct_excessive_bools)]
pub struct RecordsView {
    flavor: Flavor,
    id: egui::Id,
    left_path: PathBuf,
    right_path: PathBuf,
    left_field: String,
    right_field: String,
    notify: Arc<dyn Fn() + Send + Sync>,
    pipeline: Pipeline,
    settings: SessionSettings,
    rules: Rules,
    listing: Listing,
    sides: Option<Box<(SideData, SideData)>>,
    progress: Progress,
    strip: ca_ui::thumbnail::Strip<RecordClass>,
    strip_key: (usize, u64, usize),
    revision: u64,
    scroll: RowScroll,
    viewport_height: f32,
    rows_in_view: usize,
    rows_area: egui::Rect,
    show_strip: bool,
    show_details: bool,
    show_hex: bool,
    picker: Option<Job<DialogMessage>>,
    picker_target: Target,
    font: ca_ui::font::FontSize,
    line_spacing: u32,
    report: ViewReport,
    info_open: bool,
    is_active: bool,
    clipboard: Option<String>,
    /// Tab-local registry export text, indexed by the side it replaced.
    clipboard_input: [Option<String>; 2],
    /// The pane whose clipboard text the platform was asked to provide.
    clipboard_requested: Option<Side>,
    /// The pane awaiting the paste event requested by Open Clipboard.
    clipboard_asked: Option<Side>,
    copy_job: Option<SelectionCopyRun>,
    copy_status: Option<String>,
    #[cfg(test)]
    selection_copy_gate: Option<Arc<std::sync::Barrier>>,
    #[cfg(test)]
    selection_copy_finished: Arc<std::sync::atomic::AtomicBool>,
    edit: editing::EditState,
    find: ca_ui::find::FindPanel,
    find_job: Option<FindRun>,
    find_status: Option<String>,
}

/// A find result is only applied while its source rows and cursor remain current.
struct FindRun {
    job: Job<FindMessage>,
    revision: u64,
    cursor: usize,
}

/// A clipboard result belongs to the displayed rows from which it was built.
struct SelectionCopyRun {
    job: Job<SelectionCopyMessage>,
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

impl RecordsView {
    /// A tab over the two sides, with the comparison already started.
    #[must_use]
    pub fn new(
        flavor: Flavor,
        left: PathBuf,
        right: PathBuf,
        context: &ViewContext,
        instance: u64,
    ) -> Self {
        let id = egui::Id::new((flavor.id(), instance));
        let settings = SessionSettings::defaults_for(&flavor.kind());
        let mut view = Self {
            flavor,
            id,
            left_field: left.display().to_string(),
            right_field: right.display().to_string(),
            left_path: left,
            right_path: right,
            notify: Arc::clone(&context.notify),
            pipeline: Pipeline::new(),
            rules: rules_from(flavor, &settings),
            settings,
            listing: Listing::default(),
            sides: None,
            progress: Progress::Running(Stage::Reading),
            strip: ca_ui::thumbnail::Strip::default(),
            strip_key: (0, 0, 0),
            revision: 0,
            scroll: RowScroll::top(),
            viewport_height: 0.0,
            rows_in_view: 0,
            rows_area: egui::Rect::NOTHING,
            show_strip: true,
            show_details: true,
            show_hex: false,
            picker: None,
            picker_target: Target::Left,
            font: ca_ui::font::FontSize::new(ca_session::options::provisional::EDITOR_POINT_SIZE),
            line_spacing: 0,
            report: ViewReport::new(flavor.report_kind(), id, Arc::clone(&context.notify)),
            info_open: false,
            is_active: true,
            clipboard: None,
            clipboard_input: [None, None],
            clipboard_requested: None,
            clipboard_asked: None,
            copy_job: None,
            copy_status: None,
            #[cfg(test)]
            selection_copy_gate: None,
            #[cfg(test)]
            selection_copy_finished: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            edit: editing::EditState::new(),
            find: ca_ui::find::FindPanel::default(),
            find_job: None,
            find_status: None,
        };
        view.restart();
        view
    }

    /// Which comparison this view shows.
    #[must_use]
    pub const fn flavor(&self) -> Flavor {
        self.flavor
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

    /// The row model behind the panes.
    #[must_use]
    pub const fn listing(&self) -> &Listing {
        &self.listing
    }

    /// The rules the next comparison runs under.
    #[must_use]
    pub const fn rules(&self) -> &Rules {
        &self.rules
    }

    /// True once a comparison is on screen.
    #[must_use]
    pub fn has_comparison(&self) -> bool {
        self.progress == Progress::Ready
    }

    /// Why the comparison failed, where it did.
    #[must_use]
    pub fn failure(&self) -> Option<&str> {
        match &self.progress {
            Progress::Failed(reason) => Some(reason),
            _ => None,
        }
    }

    /// How many requests were replaced before they finished.
    #[must_use]
    pub const fn superseded_requests(&self) -> u64 {
        self.pipeline.superseded_count()
    }

    /// True while the details area is shown.
    #[must_use]
    pub const fn shows_details(&self) -> bool {
        self.show_details
    }

    /// True while the hex details area is shown.
    #[must_use]
    pub const fn shows_hex(&self) -> bool {
        self.show_hex
    }

    /// Where the rows were painted in the last frame.
    #[must_use]
    pub const fn rows_area(&self) -> egui::Rect {
        self.rows_area
    }

    /// True while the thumbnail strip is shown.
    #[must_use]
    pub const fn shows_strip(&self) -> bool {
        self.show_strip
    }

    /// Height of one row at the size the options state.
    #[must_use]
    pub fn row_height(&self) -> f32 {
        self.font.row_height(ROW_HEIGHT_RATIO, self.line_spacing)
    }

    fn follow_options(&mut self, ctx: &egui::Context) {
        let options = ca_ui::options::current(ctx);
        self.font.follow(options.editor_point_size());
        self.line_spacing = options.extra_line_spacing();
    }

    /// Show a different set of rows.
    pub fn set_filter(&mut self, filter: DisplayFilter) {
        self.listing.set_filter(filter);
        self.changed();
    }

    /// Show unimportant differences as matches, or stop doing so.
    pub fn set_ignore_unimportant(&mut self, ignore: bool) {
        self.listing.set_ignore_unimportant(ignore);
        self.changed();
    }

    /// Put the cursor on a row on screen.
    pub fn set_cursor(&mut self, row: usize) {
        self.clear_selection();
        self.listing.set_cursor(row);
        self.reveal_cursor();
    }

    fn cancel_find(&mut self) {
        if let Some(run) = self.find_job.take() {
            run.job.cancel();
        }
        if self.find_status.as_deref() == Some("Searching…") {
            self.find_status = None;
        }
    }

    fn start_selection_copy(&mut self) {
        if !self.is_active {
            return;
        }
        self.cancel_selection_copy();
        self.copy_status = None;
        #[cfg(test)]
        self.selection_copy_finished
            .store(false, std::sync::atomic::Ordering::SeqCst);
        #[cfg(test)]
        let selection_copy_gate = self.selection_copy_gate.take();
        let selection = self.edit.selection.clone();
        let selected = !selection.is_empty();
        let rows = if selected {
            selection
        } else {
            let Some(index) = self.listing.index_at(self.listing.cursor()) else {
                return;
            };
            vec![index]
        };
        let nodes = self.listing.shared_nodes();
        let notify = Arc::clone(&self.notify);
        #[cfg(test)]
        let selection_copy_finished = Arc::clone(&self.selection_copy_finished);
        let job = Job::spawn_notifying(
            move |emitter, cancel| {
                #[cfg(test)]
                if let Some(gate) = selection_copy_gate {
                    gate.wait();
                }
                let mut text = String::new();
                let mut wrote_line = false;
                for (row_index, index) in rows.into_iter().enumerate() {
                    if row_index % 128 == 0 && cancel.is_cancelled() {
                        #[cfg(test)]
                        selection_copy_finished.store(true, std::sync::atomic::Ordering::SeqCst);
                        return;
                    }
                    let Some(node) = nodes.get(index) else {
                        continue;
                    };
                    if wrote_line {
                        text.push('\n');
                    }
                    wrote_line = true;
                    let name = node.qualified_name();
                    let value = |side| {
                        node.record(side)
                            .map(|record| record.display.as_str())
                            .unwrap_or_default()
                    };
                    text.push_str(&ca_ui::clipboard::table_field(&name));
                    text.push('\t');
                    text.push_str(&ca_ui::clipboard::table_field(value(Side::Left)));
                    text.push('\t');
                    text.push_str(&ca_ui::clipboard::table_field(value(Side::Right)));
                }
                #[cfg(test)]
                let _ = emitter.send(SelectionCopyMessage::Ready(text));
                #[cfg(not(test))]
                let _ = emitter.send(SelectionCopyMessage::Ready(text));
                #[cfg(test)]
                selection_copy_finished.store(true, std::sync::atomic::Ordering::SeqCst);
            },
            notify,
        );
        self.copy_job = Some(SelectionCopyRun { job });
    }

    fn poll_selection_copy(&mut self) {
        if !self.is_active {
            self.cancel_selection_copy();
            return;
        }
        let Some(run) = self.copy_job.as_mut() else {
            return;
        };
        let messages = run.job.drain();
        let finished = run.job.is_finished();
        for message in messages {
            match message {
                SelectionCopyMessage::Ready(text) => {
                    self.clipboard = Some(text);
                }
                SelectionCopyMessage::Cancelled => {}
                SelectionCopyMessage::Failed(detail) => {
                    self.copy_status = Some(format!("Could not copy the selection: {detail}"));
                }
            }
        }
        if finished {
            self.copy_job = None;
        }
    }

    fn cancel_selection_copy(&mut self) {
        self.copy_job = None;
    }

    #[cfg(test)]
    pub(crate) fn gate_next_selection_copy(&mut self) -> Arc<std::sync::Barrier> {
        let gate = Arc::new(std::sync::Barrier::new(2));
        self.selection_copy_gate = Some(Arc::clone(&gate));
        gate
    }

    #[cfg(test)]
    pub(crate) fn selection_copy_finished(&self) -> bool {
        self.selection_copy_finished
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// True when a side holds edits that its source has not received, or a
    /// write is running, so reading the sources again would lose work.
    fn holds_edits(&self) -> bool {
        self.is_editable_kind() && (self.edit.history.is_any_modified() || self.is_writing())
    }

    /// Open or close the key on screen at `row`.
    pub fn toggle(&mut self, row: usize) {
        self.listing.toggle(row);
        self.changed();
    }

    /// Read both sides again and compare them.
    pub fn restart(&mut self) {
        self.clipboard_requested = None;
        self.clipboard_asked = None;
        self.cancel_find();
        self.cancel_selection_copy();
        self.sides = None;
        self.progress = Progress::Running(Stage::Reading);
        let job = jobs::spawn_load_with_clipboard(
            self.flavor,
            self.left_path.clone(),
            self.right_path.clone(),
            self.rules.clone(),
            self.clipboard_input.clone(),
            Arc::clone(&self.notify),
        );
        self.pipeline.start(job);
    }

    /// Compare again under the current rules, without reading the sides.
    pub fn recompare(&mut self) {
        self.cancel_find();
        if self.is_editable_kind() && self.edit.origin.is_some() {
            self.rebuild();
            return;
        }
        let Some(sides) = self.sides.as_ref() else {
            self.restart();
            return;
        };
        self.progress = Progress::Running(Stage::Comparing);
        let job = jobs::spawn_recompare(
            self.flavor,
            sides.0.clone(),
            sides.1.clone(),
            self.rules.clone(),
            Arc::clone(&self.notify),
        );
        self.pipeline.start(job);
    }

    /// Take whatever the worker has posted.
    pub fn poll(&mut self) {
        for message in self.pipeline.poll() {
            match message {
                RecordMessage::Progress(stage) => self.progress = Progress::Running(stage),
                RecordMessage::Failed(reason) => self.progress = Progress::Failed(reason),
                RecordMessage::Cancelled => {
                    if self.progress.is_running() {
                        self.progress = Progress::Cancelled;
                    }
                }
                RecordMessage::Ready(data) => self.apply(*data),
                RecordMessage::Refused(reason) => self.refused(&reason),
            }
        }
        self.poll_picker();
        self.poll_edit();
        self.poll_find();
        self.poll_selection_copy();
    }

    fn poll_find(&mut self) {
        let Some(run) = self.find_job.as_mut() else {
            return;
        };
        let messages = run.job.drain();
        let finished = run.job.is_finished();
        let revision = run.revision;
        let cursor = run.cursor;
        let stale = revision != self.revision || cursor != self.listing.cursor();
        for message in messages {
            if stale {
                continue;
            }
            match message {
                FindMessage::Found(row) => {
                    self.find_status = None;
                    self.set_cursor(row);
                }
                FindMessage::NotFound => {
                    self.find_status = Some("No match found.".to_owned());
                }
                FindMessage::NoPattern => {
                    self.find_status = Some("Enter something to search for.".to_owned());
                }
                FindMessage::BadPattern(reason) => {
                    self.find_status = Some(format!("The pattern is not usable: {reason}"));
                }
                FindMessage::LimitExceeded => {
                    self.find_status = Some(
                        "The pattern is too costly to match; some matches may be missing."
                            .to_owned(),
                    );
                }
                FindMessage::Cancelled => {}
            }
        }
        if finished {
            if stale && self.find_status.as_deref() == Some("Searching…") {
                self.find_status = None;
            }
            self.find_job = None;
        }
    }

    fn start_find(&mut self, backwards: bool) {
        if let Some(run) = self.find_job.take() {
            run.job.cancel();
        }
        let listing = self.listing.clone();
        let settings = self.find.settings.clone();
        let cursor = listing.cursor();
        let revision = self.revision;
        self.find_status = Some("Searching…".to_owned());
        let job = search::spawn_find(
            listing,
            settings,
            cursor,
            backwards,
            Arc::clone(&self.notify),
        );
        self.find_job = Some(FindRun {
            job,
            revision,
            cursor,
        });
    }

    fn find_panel(&mut self, ui: &mut egui::Ui) {
        let controls = ca_ui::find::FindControls {
            whole_words: true,
            regex: true,
            selection_only: false,
            enter_searches: true,
        };
        match self.find.show_find_with(ui, controls, |_| {}) {
            Some(ca_ui::find::PanelRequest::Next) => self.start_find(false),
            Some(ca_ui::find::PanelRequest::Previous) => self.start_find(true),
            _ => {}
        }
    }

    fn apply(&mut self, data: jobs::RecordData) {
        self.cancel_selection_copy();
        let jobs::RecordData {
            left,
            right,
            nodes,
            fresh,
        } = data;
        if fresh {
            self.edit.reset(&left, &right);
        }
        self.clear_selection();
        self.sides = Some(Box::new((left, right)));
        self.progress = Progress::Ready;
        if fresh {
            self.listing.set_nodes(nodes);
            if let Some(row) = self.listing.next_difference(0) {
                self.listing.set_cursor(row);
            }
        } else {
            self.listing.set_nodes_keeping_cursor(nodes);
        }
        self.changed();
    }

    /// Note a change of the rows on screen, so the strip is built again and
    /// the cursor stays in view.
    fn changed(&mut self) {
        self.cancel_find();
        self.revision += 1;
        self.reveal_cursor();
    }

    fn reveal_cursor(&mut self) {
        let total = self.listing.rows();
        let height = self.row_height();
        self.scroll
            .reveal(self.listing.cursor(), self.viewport_height, height, total);
    }

    fn poll_picker(&mut self) {
        let Some(job) = self.picker.as_mut() else {
            return;
        };
        let messages = job.drain();
        let finished = job.is_finished();
        for message in messages {
            if let DialogMessage::Chosen(path) = message {
                let text = path.display().to_string();
                match self.picker_target {
                    Target::Left => {
                        self.left_field = text;
                        self.clipboard_input[0] = None;
                    }
                    Target::Right => {
                        self.right_field = text;
                        self.clipboard_input[1] = None;
                    }
                }
                self.apply_fields();
            }
        }
        if finished {
            self.picker = None;
        }
    }

    fn open_picker(&mut self, target: Target) {
        self.picker_target = target;
        self.picker = Some(ca_ui::dialog::spawn(
            ca_ui::dialog::Pick::File,
            Arc::clone(&self.notify),
        ));
    }

    fn apply_fields(&mut self) {
        if self.holds_edits() {
            self.edit.message = Some(UNWRITTEN.to_owned());
            return;
        }
        self.left_path = PathBuf::from(self.left_field.trim());
        self.right_path = PathBuf::from(self.right_field.trim());
        self.restart();
    }

    /// Move to the next row that differs.
    pub fn go_to_next_difference(&mut self) {
        let from = self.listing.cursor().saturating_add(1);
        if let Some(row) = self.listing.next_difference(from) {
            self.set_cursor(row);
        }
    }

    /// Move to the previous row that differs.
    pub fn go_to_previous_difference(&mut self) {
        let Some(from) = self.listing.cursor().checked_sub(1) else {
            return;
        };
        if let Some(row) = self.listing.previous_difference(from) {
            self.set_cursor(row);
        }
    }

    /// Move the cursor with the keyboard.
    pub fn navigate(&mut self, key: egui::Key, page: usize) {
        if !matches!(
            key,
            egui::Key::ArrowDown
                | egui::Key::ArrowUp
                | egui::Key::PageDown
                | egui::Key::PageUp
                | egui::Key::Home
                | egui::Key::End
                | egui::Key::ArrowRight
                | egui::Key::ArrowLeft
        ) {
            return;
        }
        self.clear_selection();
        let step = i64::try_from(page.max(1)).unwrap_or(1);
        let row = self.listing.cursor();
        let node = self.listing.node_at(row);
        let is_open_key = node.is_some_and(|node| node.is_group)
            && self.listing.has_children(row)
            && self
                .listing
                .index_at(row)
                .is_some_and(|index| !self.listing.is_collapsed(index));
        match key {
            egui::Key::ArrowDown => self.listing.move_cursor(1),
            egui::Key::ArrowUp => self.listing.move_cursor(-1),
            egui::Key::PageDown => self.listing.move_cursor(step),
            egui::Key::PageUp => self.listing.move_cursor(-step),
            egui::Key::Home => self.listing.set_cursor(0),
            egui::Key::End => self.listing.set_cursor(usize::MAX),
            egui::Key::ArrowRight => {
                if node.is_some_and(|node| node.is_group) && !is_open_key {
                    self.listing.expand(row);
                    self.revision += 1;
                }
            }
            egui::Key::ArrowLeft => {
                if is_open_key {
                    self.listing.collapse(row);
                    self.revision += 1;
                } else if let Some(parent) = self.listing.parent_row(row) {
                    self.listing.set_cursor(parent);
                }
            }
            _ => return,
        }
        self.reveal_cursor();
    }

    /// The text a copy of the cursor row puts on the clipboard.
    #[must_use]
    pub fn copy_text(&self) -> Option<String> {
        let node = self.listing.node_at(self.listing.cursor())?;
        let side = |side: Side| {
            node.record(side)
                .map(|record| record.display.clone())
                .unwrap_or_default()
        };
        Some(format!(
            "{}\t{}\t{}",
            ca_ui::clipboard::table_field(&node.qualified_name()),
            ca_ui::clipboard::table_field(&side(Side::Left)),
            ca_ui::clipboard::table_field(&side(Side::Right))
        ))
    }

    /// The comparison the report is written from.
    #[must_use]
    pub fn report_payload(&self) -> (ReportMeta, Payload) {
        let meta = ReportMeta::new(
            self.left_path.display().to_string(),
            self.right_path.display().to_string(),
        )
        .with_title(self.flavor.report_kind().title());
        (
            meta,
            Payload::Record(self.flavor.record_kind(), self.listing.report_rows()),
        )
    }

    /// The name the report display filter carries for what the panes show.
    #[must_use]
    pub const fn report_filter(&self) -> &'static str {
        match self.listing.filter() {
            DisplayFilter::Differences => "mismatches",
            DisplayFilter::Same => "matches",
            _ => "all",
        }
    }

    fn palette(ui: &egui::Ui) -> &'static Palette {
        ca_ui::theme::records::palette(Variant::from_dark_mode(ui.visuals().dark_mode))
    }

    fn path_bar(&mut self, ui: &mut egui::Ui) {
        let left_title = self.clipboard_input[0].as_ref().map(|_| "Clipboard");
        let right_title = self.clipboard_input[1].as_ref().map(|_| "Clipboard");
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
            Some(widgets::PathBarAction::Reload) => self.apply_fields(),
            None => {}
        }
    }

    fn clipboard_events(&mut self, ctx: &egui::Context) {
        if let Some(text) = self.clipboard.take() {
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
        if let Some(side) = self.clipboard_asked.take() {
            match pasted.filter(|text| !text.is_empty()) {
                Some(text) => {
                    self.clipboard_input[match side {
                        Side::Left => 0,
                        Side::Right => 1,
                    }] = Some(text);
                    self.copy_status = None;
                    self.restart();
                }
                None => self.copy_status = Some("The clipboard holds no text".to_owned()),
            }
        } else if open_clipboard_shortcut && self.accepts(Command::OpenClipboard) {
            match pasted.filter(|text| !text.is_empty()) {
                Some(text) => {
                    self.clipboard_input[match self.active_side() {
                        Side::Left => 0,
                        Side::Right => 1,
                    }] = Some(text);
                    self.copy_status = None;
                    self.restart();
                }
                None => self.copy_status = Some("The clipboard holds no text".to_owned()),
            }
        }
        if let Some(side) = self.clipboard_requested.take() {
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
            self.clipboard_asked = Some(side);
        }
    }

    fn file_info(&self, ui: &mut egui::Ui) {
        let sides: Vec<widgets::FileInfo> = [
            ("Left", true, self.clipboard_input[0].is_some()),
            ("Right", false, self.clipboard_input[1].is_some()),
        ]
        .into_iter()
        .map(|(label, is_left, is_clipboard)| {
            let facts = self
                .sides
                .as_ref()
                .map(|sides| if is_left { &sides.0 } else { &sides.1 })
                .map(|side| &side.facts);
            widgets::FileInfo {
                format: facts.map(|facts| facts.format.clone()),
                size: facts.and_then(|facts| facts.size),
                modified: facts.and_then(|facts| facts.modified),
                ..widgets::FileInfo::new(if is_clipboard { "Clipboard" } else { label })
            }
        })
        .collect();
        widgets::file_info_bar(ui, ca_ui::format::probed_offset().unwrap_or(0), &sides);
    }

    /// The toolbar items this view declares, in the state it is in now.
    #[must_use]
    pub fn toolbar_items(&self) -> Vec<toolbar::Item> {
        let running = self.progress.is_running();
        let ready = self.has_comparison();
        vec![
            toolbar::Item::widget("filter", FILTER_COMBO_WIDTH),
            toolbar::Item::widget("minor", 70.0),
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
            toolbar::Item::command(
                "expand",
                Command::ExpandAll,
                "Expand",
                ready,
                "No comparison yet",
            ),
            toolbar::Item::command(
                "collapse",
                Command::CollapseAll,
                "Collapse",
                ready,
                "No comparison yet",
            ),
            toolbar::Item::separator("separator-3"),
            toolbar::Item::command(
                "reload",
                Command::Reload,
                "Reload",
                !running,
                "Work is running",
            ),
            toolbar::Item::command(
                "recompare",
                Command::Recompare,
                "Recompare",
                !running && self.sides.is_some(),
                "Nothing is loaded to compare again",
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
            toolbar::Item::separator("separator-4"),
            toolbar::Item::widget("strip", 110.0),
            toolbar::Item::widget("details", 90.0),
            toolbar::Item::widget("hex", 110.0),
        ]
    }

    #[allow(clippy::too_many_lines)]
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let has_importance = self.flavor.has_importance();
        let mut filter = self.listing.filter();
        let mut ignore = self.listing.ignores_unimportant();
        let mut strip = self.show_strip;
        let mut details = self.show_details;
        let mut hex = self.show_hex;
        let id = self.id;
        let items = self.toolbar_items();
        let layout = toolbar::Layout::from_options(
            &ca_ui::options::runtime::current(ui.ctx()).stored.commands,
            toolbar::ToolbarView::Records,
        );
        let outcome = toolbar::show_for(
            toolbar::ToolbarView::Records,
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
                            for option in DisplayFilter::ALL {
                                ui.selectable_value(&mut filter, option, option.label());
                            }
                        });
                }
                "minor" => {
                    ui.add_enabled_ui(has_importance, |ui| {
                        ca_ui::widgets::icon_toggle(
                            ui,
                            &mut ignore,
                            "Minor",
                            ca_ui::icons::Icon::IgnoreUnimportant,
                        )
                    })
                    .inner
                    .on_disabled_hover_text(NO_IMPORTANCE);
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
                "hex" => {
                    ca_ui::widgets::icon_toggle(
                        ui,
                        &mut hex,
                        "Hex details",
                        ca_ui::icons::Icon::HexDetails,
                    );
                }
                _ => {}
            },
        );
        if filter != self.listing.filter() {
            self.set_filter(filter);
        }
        if ignore != self.listing.ignores_unimportant() {
            self.set_ignore_unimportant(ignore);
        }
        self.show_strip = strip;
        self.show_details = details;
        self.show_hex = hex;
        if let Some(command) = outcome.command {
            if self.accepts(command) {
                self.run(command);
            }
        }
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
        let counts = self.listing.counts();
        let keys = self.listing.key_counts();
        let total = counts
            .same
            .saturating_add(counts.different)
            .saturating_add(counts.unimportant)
            .saturating_add(counts.left_only)
            .saturating_add(counts.right_only);
        let id = self.id;
        egui::Window::new(Command::CompareInfo.label_in(self.menu_view()))
            .id(id.with("compare-info"))
            .open(&mut self.info_open)
            .show(ctx, |ui| {
                ui.label(format!("Compared records: {total}"));
                egui::Grid::new(id.with("compare-info-values")).show(ui, |ui| {
                    for (label, count) in [
                        ("Same", counts.same),
                        ("Different", counts.different),
                        ("Unimportant", counts.unimportant),
                        ("Left only", counts.left_only),
                        ("Right only", counts.right_only),
                    ] {
                        ui.label(label);
                        ui.label(count.to_string());
                        ui.end_row();
                    }
                });
                ui.separator();
                ui.label("Keys");
                egui::Grid::new(id.with("compare-info-keys")).show(ui, |ui| {
                    for (label, count) in [
                        ("Same", keys.same),
                        ("Different", keys.different),
                        ("Unimportant", keys.unimportant),
                        ("Left only", keys.left_only),
                        ("Right only", keys.right_only),
                    ] {
                        ui.label(label);
                        ui.label(count.to_string());
                        ui.end_row();
                    }
                });
            });
    }

    fn layout(&self, full: egui::Rect) -> Areas {
        let strip_width = if self.show_strip { STRIP_WIDTH } else { 0.0 };
        let body_right = full.right() - SCROLLBAR - strip_width;
        let header = egui::Rect::from_min_max(
            full.left_top(),
            egui::pos2(body_right, full.top() + HEADER_HEIGHT),
        );
        let rows = egui::Rect::from_min_max(
            egui::pos2(full.left(), header.bottom()),
            egui::pos2(body_right, full.bottom().max(header.bottom())),
        );
        let middle = f32::midpoint(full.left(), body_right);
        Areas {
            header,
            rows,
            left: egui::Rect::from_min_max(
                rows.left_top(),
                egui::pos2((middle - SEPARATOR / 2.0).max(full.left()), rows.bottom()),
            ),
            right: egui::Rect::from_min_max(
                egui::pos2((middle + SEPARATOR / 2.0).min(body_right), rows.top()),
                rows.right_bottom(),
            ),
            strip: egui::Rect::from_min_max(
                egui::pos2(body_right, rows.top()),
                egui::pos2(body_right + strip_width, rows.bottom()),
            ),
            vertical_bar: egui::Rect::from_min_max(
                egui::pos2(full.right() - SCROLLBAR, rows.top()),
                full.right_bottom(),
            ),
        }
    }

    fn panes(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        let full = ui.available_rect_before_wrap();
        if full.width() <= 0.0 || full.height() <= 0.0 {
            return;
        }
        let areas = self.layout(full);
        let response = ui.allocate_rect(full, egui::Sense::click_and_drag());
        let painter = ui.painter_at(full);
        painter.rect_filled(full, 0.0, palette.background);

        let total = self.listing.rows();
        let height = self.row_height();
        self.viewport_height = areas.rows.height();
        self.rows_area = areas.rows;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let rows_in_view = (areas.rows.height() / height).max(0.0) as usize;
        self.rows_in_view = rows_in_view;
        self.scroll.clamp(areas.rows.height(), height, total);
        let range = self.scroll.visible(areas.rows.height(), height, total);

        self.header(&painter, palette, &areas);
        for (side, pane) in [(Side::Left, areas.left), (Side::Right, areas.right)] {
            self.paint_pane(&painter, palette, side, pane, &range);
        }
        let middle = areas.left.right() + SEPARATOR / 2.0;
        painter.line_segment(
            [
                egui::pos2(middle, areas.header.top()),
                egui::pos2(middle, areas.rows.bottom()),
            ],
            egui::Stroke::new(1.0, palette.separator),
        );
        self.strip_ui(ui, &painter, palette, areas.strip, total);
        self.vertical_bar(ui, &painter, palette, areas.vertical_bar, total);
        self.pointer(ui, &response, &areas);

        if ui.rect_contains_pointer(areas.rows) {
            let delta = ui.input(|input| input.smooth_scroll_delta);
            if delta.y.abs() > f32::EPSILON {
                self.scroll
                    .scroll_by(-delta.y, areas.rows.height(), height, total);
            }
        }
        if response.has_focus() {
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
                self.navigate(key, rows_in_view.saturating_sub(1));
            }
        }
    }

    fn header(&self, painter: &egui::Painter, palette: &Palette, areas: &Areas) {
        painter.rect_filled(areas.header, 0.0, palette.header_background);
        let font = egui::FontId::proportional(self.font.points());
        for pane in [areas.left, areas.right] {
            let clipped = painter.with_clip_rect(egui::Rect::from_min_max(
                egui::pos2(pane.left(), areas.header.top()),
                egui::pos2(pane.right(), areas.header.bottom()),
            ));
            let columns = Columns::of(pane);
            let y = areas.header.center().y;
            for (x, caption) in [
                (columns.name, self.flavor.group_heading()),
                (columns.kind, "Type"),
                (columns.data, "Data"),
            ] {
                clipped.text(
                    egui::pos2(x, y),
                    egui::Align2::LEFT_CENTER,
                    caption,
                    font.clone(),
                    palette.header_text,
                );
            }
        }
    }

    fn paint_pane(
        &self,
        painter: &egui::Painter,
        palette: &Palette,
        side: Side,
        pane: egui::Rect,
        rows: &std::ops::Range<usize>,
    ) {
        if pane.width() <= 0.0 {
            return;
        }
        let font = egui::FontId::proportional(self.font.points());
        let height = self.row_height();
        let clipped = painter.with_clip_rect(pane);
        let columns = Columns::of(pane);
        let ignore = self.listing.ignores_unimportant();
        let cursor = self.listing.cursor();
        for row in rows.clone() {
            let Some(index) = self.listing.index_at(row) else {
                continue;
            };
            let Some(node) = self.listing.nodes().get(index) else {
                continue;
            };
            let y = pane.top() + self.scroll.row_y(row, height);
            let rect = egui::Rect::from_min_size(
                egui::pos2(pane.left(), y),
                egui::vec2(pane.width(), height),
            );
            if !node.is_on(side) {
                paint_gap(&clipped, palette, rect, row == cursor);
                continue;
            }
            let class = node.class(ignore);
            let (background, text) = palette.row(class);
            if row == cursor || self.is_selected(index) {
                clipped.rect_filled(rect, 0.0, palette.selection);
            } else if class != RecordClass::Same {
                clipped.rect_filled(rect, 0.0, background);
            }
            #[allow(clippy::cast_precision_loss)]
            let indent = columns.name + INDENT * node.depth as f32;
            let middle = rect.center().y;
            if node.is_group {
                if self.listing.has_children(row) {
                    let icon = if self.listing.is_collapsed(index) {
                        ca_ui::icons::Icon::ChevronRight
                    } else {
                        ca_ui::icons::Icon::ChevronDown
                    };
                    icon.paint_in_row(
                        &clipped,
                        egui::pos2(indent + EXPANDER / 2.0, middle),
                        height,
                        text,
                    );
                }
                clipped.text(
                    egui::pos2(indent + EXPANDER, middle),
                    egui::Align2::LEFT_CENTER,
                    &node.name,
                    font.clone(),
                    text,
                );
                continue;
            }
            let Some(record) = node.record(side) else {
                continue;
            };
            let name_clip = painter.with_clip_rect(
                egui::Rect::from_x_y_ranges(pane.left()..=columns.kind - CELL_PAD, rect.y_range())
                    .intersect(pane),
            );
            name_clip.text(
                egui::pos2(indent + EXPANDER, middle),
                egui::Align2::LEFT_CENTER,
                &node.name,
                font.clone(),
                text,
            );
            let type_clip = painter.with_clip_rect(
                egui::Rect::from_x_y_ranges(columns.kind..=columns.data - CELL_PAD, rect.y_range())
                    .intersect(pane),
            );
            type_clip.text(
                egui::pos2(columns.kind, middle),
                egui::Align2::LEFT_CENTER,
                &record.type_name,
                font.clone(),
                palette.type_text,
            );
            let format = egui::TextFormat {
                font_id: font.clone(),
                color: text,
                ..egui::TextFormat::default()
            };
            let galley = painter.layout_job(data_job(node, side, class, format, palette));
            let data_clip = painter.with_clip_rect(
                egui::Rect::from_x_y_ranges(columns.data..=pane.right(), rect.y_range())
                    .intersect(pane),
            );
            data_clip.galley(
                egui::pos2(columns.data, middle - galley.size().y / 2.0),
                galley,
                text,
            );
        }
    }

    fn strip_ui(
        &mut self,
        ui: &egui::Ui,
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
            self.strip = crate::thumbnail::build(&self.listing, rect.height());
            self.strip_key = key;
        }
        for pixel in 0..self.strip.pixels() {
            let Some(class) = self.strip.class_at(pixel) else {
                continue;
            };
            #[allow(clippy::cast_precision_loss)]
            let y = rect.top() + pixel as f32;
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(rect.left(), y),
                    egui::vec2(rect.width(), 1.0),
                ),
                0.0,
                palette.strip(class),
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
            let height = self.row_height();
            self.scroll
                .center_on(row, self.viewport_height, height, total);
        }
    }

    fn vertical_bar(
        &mut self,
        ui: &egui::Ui,
        painter: &egui::Painter,
        palette: &Palette,
        track: egui::Rect,
        total: usize,
    ) {
        if track.height() <= 0.0 {
            return;
        }
        painter.rect_filled(track, 0.0, palette.track);
        let height = self.row_height();
        let thumb = self
            .scroll
            .thumb(track.height(), self.viewport_height, height, total);
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
                RowScroll::from_thumb(top, track.height(), self.viewport_height, height, total);
        }
    }

    /// Put the cursor where the pointer clicked, and open or close a key.
    fn pointer(&mut self, ui: &egui::Ui, response: &egui::Response, areas: &Areas) {
        if !response.clicked() && !response.double_clicked() {
            return;
        }
        response.request_focus();
        let Some(pointer) = ui.ctx().pointer_interact_pos() else {
            return;
        };
        if !areas.rows.contains(pointer) {
            return;
        }
        let height = self.row_height();
        let offset = pointer.y - areas.rows.top() + self.scroll.pixel_shift(height);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let step = (offset / height).max(0.0) as usize;
        let row = self.scroll.first_row().saturating_add(step);
        if row >= self.listing.rows() {
            return;
        }
        self.clear_selection();
        self.listing.set_cursor(row);
        let (pane, side) = if pointer.x <= areas.left.right() {
            (areas.left, Side::Left)
        } else {
            (areas.right, Side::Right)
        };
        self.edit.active = side;
        let Some((is_group, depth)) = self
            .listing
            .node_at(row)
            .map(|node| (node.is_group, node.depth))
        else {
            return;
        };
        if !is_group {
            if response.double_clicked() && self.accepts(Command::Modify) {
                self.open_modify();
            }
            return;
        }
        if !self.listing.has_children(row) {
            return;
        }
        #[allow(clippy::cast_precision_loss)]
        let mark = Columns::of(pane).name + INDENT * depth as f32;
        let on_mark = (mark..=mark + EXPANDER).contains(&pointer.x);
        if response.double_clicked() {
            if !on_mark {
                self.toggle(row);
            }
        } else if on_mark {
            self.toggle(row);
        }
    }

    fn details_area(&self, ui: &mut egui::Ui, palette: &Palette) {
        if !self.show_details && !self.show_hex {
            return;
        }
        let node = self.listing.node_at(self.listing.cursor());
        egui::Frame::new()
            .fill(palette.details_background)
            .inner_margin(egui::Margin::same(4))
            .show(ui, |ui| {
                if self.show_details {
                    for (label, side) in [("Left", Side::Left), ("Right", Side::Right)] {
                        ui.horizontal_wrapped(|ui| {
                            ui.colored_label(palette.details_text, label);
                            ui.colored_label(palette.details_text, details_line(node, side));
                        });
                    }
                }
                if self.show_hex {
                    egui::ScrollArea::vertical()
                        .id_salt(self.id.with("hex"))
                        .max_height(HEX_HEIGHT - 12.0)
                        .show(ui, |ui| {
                            for (label, side) in [("Left", Side::Left), ("Right", Side::Right)] {
                                ui.colored_label(palette.details_text, label);
                                for line in hex_detail(node, side) {
                                    ui.label(
                                        egui::RichText::new(line)
                                            .monospace()
                                            .color(palette.details_text),
                                    );
                                }
                            }
                        });
                }
            });
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            match &self.progress {
                Progress::Running(stage) => {
                    ui.label(stage.label());
                }
                Progress::Failed(reason) => {
                    ca_ui::widgets::notice_current(ui,ca_ui::icons::Icon::Error,16.0,&format!("Failed: {reason}"));
                }
                Progress::Cancelled => {
                    ca_ui::widgets::notice_current(ui,ca_ui::icons::Icon::Warning,16.0,"Stopped before finishing");
                }
                Progress::Ready => {
                    let counts = self.listing.counts();
                    let keys = self.listing.key_counts();
                    ui.label(format!(
                        "{} same, {} differ, {} minor, {} left only, {} right only; keys: {} left only, {} right only",
                        counts.same,
                        counts.different,
                        counts.unimportant,
                        counts.left_only,
                        counts.right_only,
                        keys.left_only,
                        keys.right_only,
                    ));
                }
            }
            ui.separator();
            ui.label(format!("{} rows shown", self.listing.rows()));
            if let Some(node) = self.listing.node_at(self.listing.cursor()) {
                ui.separator();
                ui.label(format!(
                    "{}: {}",
                    node.qualified_name(),
                    class_label(node.class(self.listing.ignores_unimportant()))
                ));
            }
            if !self.selection().is_empty() {
                ui.separator();
                ui.label(format!("{} selected", self.selection().len()));
            }
            if self.is_editable_kind() {
                ui.separator();
                let mark = |side: Side| {
                    if self.is_modified(side) {
                        " (edited)"
                    } else {
                        ""
                    }
                };
                ui.label(format!(
                    "Active: {}. Left{}, right{}",
                    match self.active_side() {
                        Side::Left => "left",
                        Side::Right => "right",
                    },
                    mark(Side::Left),
                    mark(Side::Right)
                ));
                if let Some(message) = self.message() {
                    ui.separator();
                    ca_ui::widgets::notice_current(ui,ca_ui::icons::Icon::Info,16.0,message);
                }
            }
            if let Some(message) = &self.find_status {
                ui.separator();
                ca_ui::widgets::notice_current(ui,ca_ui::icons::Icon::Info,16.0,message);
            }
            if let Some(message) = &self.copy_status {
                ui.separator();
                ca_ui::widgets::notice_current(ui,ca_ui::icons::Icon::Info,16.0,message);
            }
        });
    }
}

/// Where the three columns of a pane start.
struct Columns {
    name: f32,
    kind: f32,
    data: f32,
}

impl Columns {
    fn of(pane: egui::Rect) -> Self {
        let kind = pane.left() + pane.width() * NAME_SHARE;
        Self {
            name: pane.left() + CELL_PAD,
            kind,
            data: kind + pane.width() * TYPE_SHARE,
        }
    }
}

/// Paint the filler of a side that does not hold the row.
///
/// The hatch lines follow the window coordinates, not the row, so they join
/// from one filler row to the next.
fn paint_gap(painter: &egui::Painter, palette: &Palette, rect: egui::Rect, current: bool) {
    painter.rect_filled(rect, 0.0, palette.gap_background);
    let clipped = painter.with_clip_rect(rect.intersect(painter.clip_rect()));
    let stroke = egui::Stroke::new(1.0, palette.gap_hatch);
    let height = rect.height();
    let phase = (rect.left() + rect.top()).rem_euclid(HATCH_STEP);
    let mut x = rect.left() - phase;
    while x <= rect.right() + height {
        clipped.line_segment(
            [
                egui::pos2(x, rect.top()),
                egui::pos2(x - height, rect.bottom()),
            ],
            stroke,
        );
        x += HATCH_STEP;
    }
    if current {
        painter.rect_stroke(
            rect,
            0.0,
            egui::Stroke::new(1.0, palette.selection),
            egui::StrokeKind::Inside,
        );
    }
}

/// A value as one pane line: cut to a bounded length, with line breaks and
/// tabs shown as spaces.
///
/// Every replaced character is one byte and so is its replacement, so a byte
/// range worked out on this text stays on character boundaries.
fn pane_text(value: &str) -> String {
    value
        .chars()
        .take(PANE_TEXT_LIMIT)
        .map(|character| {
            if matches!(character, '\n' | '\r' | '\t') {
                ' '
            } else {
                character
            }
        })
        .collect()
}

/// The value column of one side of a value row, with the characters that
/// differ from the other side on a tinted background.
fn data_job(
    node: &Node,
    side: Side,
    class: RecordClass,
    format: egui::TextFormat,
    palette: &Palette,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let Some(record) = node.record(side) else {
        return job;
    };
    let shown = pane_text(&record.display);
    let highlight = match (class, node.left.as_ref(), node.right.as_ref()) {
        (RecordClass::Different | RecordClass::Unimportant, Some(left), Some(right)) => {
            let (left_range, right_range) =
                inline_difference(&pane_text(&left.display), &pane_text(&right.display));
            Some(match side {
                Side::Left => left_range,
                Side::Right => right_range,
            })
        }
        _ => None,
    };
    match highlight {
        Some(range) if !range.is_empty() => {
            job.append(
                shown.get(..range.start).unwrap_or_default(),
                0.0,
                format.clone(),
            );
            job.append(
                shown.get(range.clone()).unwrap_or_default(),
                0.0,
                egui::TextFormat {
                    background: palette.inline_difference,
                    ..format.clone()
                },
            );
            job.append(shown.get(range.end..).unwrap_or_default(), 0.0, format);
        }
        _ => job.append(&shown, 0.0, format),
    }
    job
}

/// What the details area says about one side of a row.
fn details_line(node: Option<&Node>, side: Side) -> String {
    let Some(node) = node else {
        return String::new();
    };
    if !node.is_on(side) {
        return format!("{}: not on this side", node.qualified_name());
    }
    if node.is_group {
        return node.qualified_name();
    }
    let Some(record) = node.record(side) else {
        return String::new();
    };
    let display: String = record.display.chars().take(DETAILS_TEXT_LIMIT).collect();
    format!(
        "{}  [{}]  {}",
        node.qualified_name(),
        record.type_name,
        display
    )
}

/// What the hex details area shows for one side of a row.
fn hex_detail(node: Option<&Node>, side: Side) -> Vec<String> {
    let Some(record) = node.and_then(|node| node.record(side)) else {
        return vec!["No value on this side".to_owned()];
    };
    match &record.value {
        RecordValue::Bytes(bytes) => hex_lines(bytes),
        RecordValue::Text(text) => hex_lines(text.as_bytes()),
        _ => vec![record.display.clone()],
    }
}

/// What the status bar calls a row class.
#[must_use]
pub const fn class_label(class: RecordClass) -> &'static str {
    match class {
        RecordClass::Same => "same",
        RecordClass::Different => "differs",
        RecordClass::Unimportant => "differs, minor",
        RecordClass::Orphan => "on one side only",
        RecordClass::Gap => "not on this side",
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

impl SessionView for RecordsView {
    fn kind(&self) -> Option<ca_session::SessionKind> {
        Some(match self.flavor {
            Flavor::Registry => ca_session::SessionKind::RegistryCompare,
            Flavor::Version => ca_session::SessionKind::VersionCompare,
            Flavor::Media => ca_session::SessionKind::MediaCompare,
        })
    }
    fn title(&self) -> String {
        let left = if self.clipboard_input[0].is_some() {
            "Clipboard".to_owned()
        } else {
            file_name(&self.left_path)
        };
        let right = if self.clipboard_input[1].is_some() {
            "Clipboard".to_owned()
        } else {
            file_name(&self.right_path)
        };
        format!("{left} - {right}")
    }

    fn tick(&mut self) {
        self.poll();
    }

    fn set_active(&mut self, active: bool) {
        if self.is_active && !active {
            if self.copy_job.is_some() {
                self.copy_status = Some("Copy cancelled when this tab became inactive.".to_owned());
            }
            self.cancel_selection_copy();
            self.clipboard = None;
            self.clipboard_requested = None;
            self.clipboard_asked = None;
        }
        self.is_active = active;
    }

    fn ui(&mut self, ui: &mut egui::Ui, _context: &ViewContext) -> Vec<ViewAction> {
        let palette = Self::palette(ui);
        self.follow_options(ui.ctx());
        let filter = self.report_filter();
        let ignore = self.listing.ignores_unimportant();
        self.report.poll_with(ui.ctx(), |settings| {
            settings.select_filter(filter);
            settings.ignore_unimportant = ignore;
        });
        self.poll();
        self.clipboard_events(ui.ctx());
        self.path_bar(ui);
        self.file_info(ui);
        self.toolbar(ui);
        self.find_panel(ui);
        self.report_panel(ui);
        self.compare_info_window(ui.ctx());
        self.prompt_ui(ui);
        self.panel_ui(ui);
        ui.separator();
        let mut reserved = STATUS_HEIGHT;
        if self.show_details {
            reserved += DETAILS_HEIGHT;
        }
        if self.show_hex {
            reserved += HEX_HEIGHT;
        }
        let room = (ui.available_height() - reserved).max(self.row_height() * 2.0);
        ui.allocate_ui(egui::vec2(ui.available_width(), room), |ui| {
            self.panes(ui, palette);
        });
        self.details_area(ui, palette);
        ui.separator();
        self.status_bar(ui);
        Vec::new()
    }

    fn menu_view(&self) -> ca_ui::command::MenuView {
        match self.flavor {
            Flavor::Registry => MenuView::Registry,
            Flavor::Version => MenuView::Version,
            Flavor::Media => MenuView::Media,
        }
    }

    fn commands(&self) -> Vec<CommandState> {
        let mut handled: Vec<Command> = HANDLED
            .iter()
            .copied()
            .filter(|command| match command {
                Command::ToggleIgnoreUnimportant => self.flavor.has_importance(),
                _ => true,
            })
            .collect();
        if self.is_editable_kind() {
            handled.extend_from_slice(editing::EDIT_COMMANDS);
        }
        ca_ui::view::declare(&handled, |command| self.accepts(command))
    }

    fn refusal(&self, command: Command) -> Option<&'static str> {
        self.copy_refusal(command)
    }

    fn accepts(&self, command: Command) -> bool {
        if editing::EDIT_COMMANDS.contains(&command) {
            return self.accepts_edit(command);
        }
        let ready = self.has_comparison();
        let running = self.progress.is_running();
        match command {
            Command::OpenFile => {
                !running
                    && !self.is_writing()
                    && !self.holds_edits()
                    && self.picker.is_none()
                    && self.clipboard_requested.is_none()
                    && self.clipboard_asked.is_none()
            }
            Command::OpenClipboard => {
                self.flavor == Flavor::Registry
                    && ready
                    && !self.is_writing()
                    && !self.holds_edits()
                    && self.picker.is_none()
                    && self.clipboard_requested.is_none()
                    && self.clipboard_asked.is_none()
            }
            Command::ShowAll
            | Command::ShowDifferences
            | Command::ShowSame
            | Command::ShowNone
            | Command::ExpandAll
            | Command::CollapseAll
            | Command::CompareReport
            | Command::Find
            | Command::CompareInfo => ready,
            Command::ToggleIgnoreUnimportant => ready && self.flavor.has_importance(),
            Command::FindNext
            | Command::FindPrevious
            | Command::NextDifference
            | Command::PreviousDifference
            | Command::Copy
            | Command::SelectAll => ready && self.listing.rows() > 0,
            Command::ToggleLineDetails
            | Command::HexDetails
            | Command::Thumbnail
            | Command::IncreaseFontSize
            | Command::DecreaseFontSize
            | Command::ResetFontSize => true,
            Command::Reload | Command::SwapSides => {
                !running
                    && !self.holds_edits()
                    && self.clipboard_requested.is_none()
                    && self.clipboard_asked.is_none()
            }
            Command::Recompare => {
                !running
                    && self.sides.is_some()
                    && !self.is_writing()
                    && self.clipboard_requested.is_none()
                    && self.clipboard_asked.is_none()
            }
            Command::Cancel => {
                running
                    || self.find_job.is_some()
                    || self.copy_job.is_some()
                    || self.clipboard_requested.is_some()
                    || self.clipboard_asked.is_some()
            }
            _ => false,
        }
    }

    fn run(&mut self, command: Command) {
        if editing::EDIT_COMMANDS.contains(&command) {
            if self.accepts_edit(command) {
                self.run_edit(command);
            }
            return;
        }
        match command {
            Command::OpenFile => self.open_picker(match self.active_side() {
                Side::Left => Target::Left,
                Side::Right => Target::Right,
            }),
            Command::OpenClipboard => self.clipboard_requested = Some(self.active_side()),
            Command::CompareReport => self.report.request(),
            Command::CompareInfo => self.info_open = true,
            Command::ShowAll => self.set_filter(DisplayFilter::All),
            Command::ShowDifferences => self.set_filter(DisplayFilter::Differences),
            Command::ShowSame => self.set_filter(DisplayFilter::Same),
            Command::ShowNone => self.set_filter(DisplayFilter::None),
            Command::ToggleIgnoreUnimportant => {
                let ignore = !self.listing.ignores_unimportant();
                self.set_ignore_unimportant(ignore);
            }
            Command::NextDifference => self.go_to_next_difference(),
            Command::PreviousDifference => self.go_to_previous_difference(),
            Command::ExpandAll => {
                self.listing.expand_all();
                self.changed();
            }
            Command::CollapseAll => {
                self.listing.collapse_all();
                self.changed();
            }
            Command::Copy => self.start_selection_copy(),
            Command::SelectAll => self.select_all(),
            Command::Find => {
                self.find.open_find();
                self.find_status = None;
            }
            Command::FindNext => self.start_find(false),
            Command::FindPrevious => self.start_find(true),
            Command::ToggleLineDetails => self.show_details = !self.show_details,
            Command::HexDetails => self.show_hex = !self.show_hex,
            Command::Thumbnail => self.show_strip = !self.show_strip,
            Command::IncreaseFontSize | Command::DecreaseFontSize | Command::ResetFontSize => {
                let _ = self.font.run(command);
            }
            Command::Reload => self.restart(),
            Command::Recompare => self.recompare(),
            Command::SwapSides => {
                std::mem::swap(&mut self.left_path, &mut self.right_path);
                std::mem::swap(&mut self.left_field, &mut self.right_field);
                self.clipboard_input.swap(0, 1);
                self.restart();
            }
            Command::Cancel => {
                self.clipboard_requested = None;
                self.clipboard_asked = None;
                self.pipeline.cancel();
                self.cancel_find();
                self.cancel_selection_copy();
            }
            _ => {}
        }
    }

    fn apply_settings(&mut self, settings: &SessionSettings) {
        if !belongs_to(self.flavor, settings) {
            return;
        }
        self.settings = settings.clone();
        let rules = rules_from(self.flavor, settings);
        if rules != self.rules {
            self.rules = rules;
            self.recompare();
        }
    }

    fn settings(&self) -> Option<SessionSettings> {
        let mut settings = self.settings.clone();
        let specs = match &mut settings {
            SessionSettings::RegistryCompare(value) => &mut value.specs,
            SessionSettings::VersionCompare(value) => &mut value.specs,
            SessionSettings::MediaCompare(value) => &mut value.specs,
            _ => return Some(settings),
        };
        *specs = ca_ui::view::with_sides(specs, &self.left_path, &self.right_path);
        Some(settings)
    }

    fn launch_target(&self) -> Option<ca_ui::launch::LaunchTarget> {
        use ca_session::options::{LaunchContext, LaunchSide};
        if self.clipboard_input.iter().any(Option::is_some)
            || self.left_path.as_os_str().is_empty()
            || (self.flavor == Flavor::Registry
                && (jobs::is_live_spec(&self.left_path) || jobs::is_live_spec(&self.right_path)))
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
        let target = self.launch_target()?;
        let path = match self.active_side() {
            Side::Left => &self.left_path,
            Side::Right => &self.right_path,
        };
        Some((path.clone(), target.selection))
    }

    fn is_ready(&self) -> bool {
        !self.progress.is_running()
    }

    fn notice(&self) -> Option<String> {
        self.failure().map(str::to_owned)
    }

    fn wants_close(&self) -> bool {
        self.wants_close_edit()
    }

    fn may_close(&mut self) -> bool {
        self.may_close_edit()
    }

    fn is_busy(&self) -> bool {
        self.is_writing()
    }

    fn holds_unwritten_edits(&self) -> bool {
        self.is_editable_kind() && self.edit.history.is_any_modified()
    }

    fn holds_temporaries(&self) -> bool {
        self.clipboard_input.iter().any(Option::is_some)
    }

    fn on_close(&mut self) {
        self.pipeline.stop();
        self.picker = None;
        self.clipboard_requested = None;
        self.clipboard_asked = None;
        self.find_job = None;
        self.cancel_selection_copy();
        if self.progress.is_running() {
            self.progress = Progress::Cancelled;
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod find_tests {
    use super::{Flavor, RecordsView};
    use crate::testing;
    use ca_ui::command::Command;
    use ca_ui::testing::{context, event_input, sized_input, wait_until};
    use ca_ui::view::SessionView;
    use std::path::PathBuf;
    use std::time::Duration;

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

    fn assert_count_rows(text: &[String], expected: &[(&str, u64)]) {
        for (label, count) in expected {
            let count = count.to_string();
            assert!(
                text.windows(2)
                    .any(|pair| pair[0] == *label && pair[1] == count),
                "Compare Info did not show {label}: {count}; text was {text:?}"
            );
        }
    }

    fn copy_from_command(view: &mut RecordsView) -> String {
        let ctx = egui::Context::default();
        let mut copied = None;
        assert!(
            wait_until(Duration::from_secs(10), || {
                view.poll();
                let output = ctx.run(sized_input(1_280.0, 800.0), |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        view.ui(ui, &context());
                    });
                });
                copied = output
                    .platform_output
                    .commands
                    .iter()
                    .find_map(|command| match command {
                        egui::OutputCommand::CopyText(text) => Some(text.clone()),
                        _ => None,
                    });
                copied.is_some()
            }),
            "the asynchronous copy did not reach the clipboard"
        );
        copied.unwrap()
    }

    #[test]
    fn open_with_target_names_both_record_files_and_refuses_live_registry_keys() {
        let dir = tempfile::tempdir().unwrap();
        let (left, right) = testing::version_pair(dir.path());
        let view = RecordsView::new(
            Flavor::Version,
            left.clone(),
            right.clone(),
            &context(),
            917,
        );

        let target = view.launch_target().unwrap();

        assert_eq!(target.selection, ca_ui::launch::Selection::Files);
        assert_eq!(target.context.first.path, left);
        assert_eq!(target.context.second.unwrap().path, right);

        let live = RecordsView::new(
            Flavor::Registry,
            PathBuf::from(r"reg:\\HKCU\Software\compare-all-test"),
            PathBuf::from(r"reg:\\HKCU\Software\compare-all-test-right"),
            &context(),
            918,
        );
        assert!(live.launch_target().is_none());
    }

    #[test]
    fn open_clipboard_reads_registry_text_without_making_a_writable_session() {
        let dir = tempfile::tempdir().unwrap();
        let (left, right) = testing::registry_pair(dir.path());
        let clipboard_text = std::fs::read_to_string(&left).unwrap();
        let left_path = left.clone();
        let mut view = RecordsView::new(Flavor::Registry, left, right, &context(), 920);
        assert!(wait_until(Duration::from_secs(10), || {
            view.poll();
            view.has_comparison()
        }));
        view.set_active_side(crate::model::Side::Left);
        assert!(view.accepts(Command::OpenClipboard));
        view.run(Command::OpenClipboard);

        let ctx = egui::Context::default();
        let request = ctx.run(event_input(1_280.0, 800.0, Vec::new()), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                view.ui(ui, &context());
            });
        });
        assert!(request.viewport_output.values().any(|viewport| {
            viewport
                .commands
                .contains(&egui::ViewportCommand::RequestPaste)
        }));
        let _ = ctx.run(
            event_input(
                1_280.0,
                800.0,
                vec![egui::Event::Paste(clipboard_text.clone())],
            ),
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    view.ui(ui, &context());
                });
            },
        );
        assert!(wait_until(Duration::from_secs(10), || {
            view.poll();
            view.has_comparison()
        }));

        assert_eq!(
            view.clipboard_input[0].as_deref(),
            Some(clipboard_text.as_str())
        );
        assert!(SessionView::holds_temporaries(&view));
        assert!(!view.accepts(Command::SaveFile));
        assert!(!view.accepts(Command::SaveFileAs));
        assert!(!view.accepts(Command::Delete));
        assert_eq!(view.left_path, left_path);

        let (version_left, version_right) = testing::version_pair(dir.path());
        let version = RecordsView::new(
            Flavor::Version,
            version_left,
            version_right,
            &context(),
            921,
        );
        assert!(!version.accepts(Command::OpenClipboard));
    }

    #[test]
    fn open_clipboard_without_text_stops_waiting_and_ignores_a_later_paste() {
        let dir = tempfile::tempdir().unwrap();
        let (left, right) = testing::registry_pair(dir.path());
        let left_path = left.clone();
        let mut view = RecordsView::new(Flavor::Registry, left, right, &context(), 922);
        assert!(wait_until(Duration::from_secs(10), || {
            view.poll();
            view.has_comparison()
        }));
        view.set_active_side(crate::model::Side::Left);
        view.run(Command::OpenClipboard);

        let ctx = egui::Context::default();
        for _ in 0..2 {
            let _ = ctx.run(event_input(1_280.0, 800.0, Vec::new()), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    view.ui(ui, &context());
                });
            });
        }
        assert!(view.accepts(Command::Reload));
        assert_eq!(
            view.copy_status.as_deref(),
            Some("The clipboard holds no text")
        );

        let _ = ctx.run(
            event_input(
                1_280.0,
                800.0,
                vec![egui::Event::Paste("late clipboard text".to_owned())],
            ),
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    view.ui(ui, &context());
                });
            },
        );
        assert!(view.clipboard_input[0].is_none());
        assert_eq!(view.left_path, left_path);
        assert!(!SessionView::holds_temporaries(&view));
    }

    #[test]
    fn paste_event_with_command_and_shift_opens_registry_clipboard_input() {
        let dir = tempfile::tempdir().unwrap();
        let (left, right) = testing::registry_pair(dir.path());
        let mut view = RecordsView::new(Flavor::Registry, left, right, &context(), 923);
        assert!(wait_until(Duration::from_secs(10), || {
            view.poll();
            view.has_comparison()
        }));
        view.set_active_side(crate::model::Side::Left);

        let clipboard_text = "Windows Registry Editor Version 5.00\n\n[HKEY_CURRENT_USER\\Software\\Clipboard]\n\"Name\"=\"Zoe\"\n";
        let mut input = event_input(
            1_280.0,
            800.0,
            vec![egui::Event::Paste(clipboard_text.to_owned())],
        );
        input.modifiers.command = true;
        input.modifiers.ctrl = true;
        input.modifiers.shift = true;
        let ctx = egui::Context::default();
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                view.ui(ui, &context());
            });
        });
        assert!(wait_until(Duration::from_secs(10), || {
            view.poll();
            view.has_comparison() && view.clipboard_input[0].is_some()
        }));

        assert_eq!(view.clipboard_input[0].as_deref(), Some(clipboard_text));
        assert!(SessionView::holds_temporaries(&view));
    }

    #[test]
    fn find_command_opens_the_panel_and_next_moves_to_a_value_match() {
        let dir = tempfile::tempdir().unwrap();
        let (left, right) = testing::registry_pair(dir.path());
        let mut view = RecordsView::new(Flavor::Registry, left, right, &context(), 918);
        assert!(wait_until(Duration::from_secs(10), || {
            view.poll();
            view.has_comparison()
        }));

        view.run(Command::Find);
        assert!(view.find.is_find_open());
        view.find.settings.pattern = "two".to_owned();
        view.find.close();
        view.run(Command::FindNext);
        assert!(view.accepts(Command::Cancel));
        assert!(wait_until(Duration::from_secs(10), || {
            view.poll();
            view.find_job.is_none()
        }));

        let node = view.listing.node_at(view.listing.cursor()).unwrap();
        assert_eq!(node.name, "Changed");
        assert_eq!(
            node.record(crate::model::Side::Right).unwrap().display,
            "two"
        );
    }

    #[test]
    fn find_copy_and_picker_jobs_give_back_their_commands_in_the_poll_that_delivers_their_answer() {
        let dir = tempfile::tempdir().unwrap();
        let (left, right) = testing::registry_pair(dir.path());
        let mut view = RecordsView::new(Flavor::Registry, left, right, &context(), 919);
        assert!(wait_until(Duration::from_secs(20), || {
            view.poll();
            view.has_comparison() && !view.pipeline.is_running()
        }));
        let (find, _find_held) =
            ca_ui::testing::job_held_after(vec![crate::search::FindMessage::NotFound]);
        view.find_job = Some(super::FindRun {
            job: find,
            revision: view.revision,
            cursor: view.listing.cursor(),
        });
        let (copy, _copy_held) =
            ca_ui::testing::job_held_after(vec![super::SelectionCopyMessage::Cancelled]);
        view.copy_job = Some(super::SelectionCopyRun { job: copy });
        let (picker, _picker_held) =
            ca_ui::testing::job_held_after(vec![ca_ui::dialog::DialogMessage::Dismissed]);
        view.picker = Some(picker);
        assert!(view.accepts(Command::Cancel));
        assert!(!view.accepts(Command::OpenFile));

        view.poll();

        assert_eq!(view.find_status.as_deref(), Some("No match found."));
        assert!(
            !view.accepts(Command::Cancel),
            "Cancel stays on for jobs that have answered"
        );
        assert!(
            view.accepts(Command::OpenFile),
            "Open File stays off for a picker that has answered"
        );
    }

    #[test]
    fn registry_compare_info_opens_a_window_after_the_comparison_finishes() {
        let dir = tempfile::tempdir().unwrap();
        let (left, right) = testing::registry_pair(dir.path());
        let mut view = RecordsView::new(Flavor::Registry, left, right, &context(), 919);
        assert!(!view.accepts(Command::CompareInfo));
        assert!(wait_until(Duration::from_secs(10), || {
            view.poll();
            view.has_comparison()
        }));
        assert!(view.accepts(Command::OpenFile));
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
        let counts = view.listing.counts();
        let keys = view.listing.key_counts();
        let total = counts.same
            + counts.different
            + counts.unimportant
            + counts.left_only
            + counts.right_only;
        assert!(
            text.contains(&format!("Compared records: {total}")),
            "Compare Info omitted total {total}; painted text: {text:?}"
        );
        assert!(text.contains(&"Keys".to_owned()));
        assert_count_rows(
            &text,
            &[
                ("Same", counts.same),
                ("Different", counts.different),
                ("Unimportant", counts.unimportant),
                ("Left only", counts.left_only),
                ("Right only", counts.right_only),
                ("Same", keys.same),
                ("Different", keys.different),
                ("Unimportant", keys.unimportant),
                ("Left only", keys.left_only),
                ("Right only", keys.right_only),
            ],
        );
        let area = ctx.memory(|memory| memory.area_rect(view.id.with("compare-info")));
        let area = area.expect("Registry Compare Info should open its window");
        assert!(ctx.screen_rect().contains_rect(area));
    }

    #[test]
    fn version_and_media_compare_info_open_after_the_comparison_finishes() {
        let dir = tempfile::tempdir().unwrap();
        for (flavor, pair, expected_label) in [
            (
                Flavor::Version,
                testing::version_pair(dir.path()),
                "Version Compare Info",
            ),
            (
                Flavor::Media,
                testing::media_pair(dir.path()),
                "Media Compare Info",
            ),
        ] {
            let mut view = RecordsView::new(flavor, pair.0, pair.1, &context(), 920);
            assert!(!view.accepts(Command::CompareInfo));
            assert!(wait_until(Duration::from_secs(10), || {
                view.poll();
                view.has_comparison()
            }));
            assert!(view.accepts(Command::OpenFile));
            assert!(view.accepts(Command::CompareInfo));
            assert_eq!(
                Command::CompareInfo.label_in(view.menu_view()),
                expected_label
            );
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
            let counts = view.listing.counts();
            let keys = view.listing.key_counts();
            let total = counts.same
                + counts.different
                + counts.unimportant
                + counts.left_only
                + counts.right_only;
            assert!(
                text.contains(&format!("Compared records: {total}")),
                "Compare Info omitted total {total}; painted text: {text:?}"
            );
            assert!(text.contains(&"Keys".to_owned()));
            assert_count_rows(
                &text,
                &[
                    ("Same", counts.same),
                    ("Different", counts.different),
                    ("Unimportant", counts.unimportant),
                    ("Left only", counts.left_only),
                    ("Right only", counts.right_only),
                    ("Same", keys.same),
                    ("Different", keys.different),
                    ("Unimportant", keys.unimportant),
                    ("Left only", keys.left_only),
                    ("Right only", keys.right_only),
                ],
            );
            let area = ctx.memory(|memory| memory.area_rect(view.id.with("compare-info")));
            let area = area.expect("Compare Info should open its window");
            assert!(ctx.screen_rect().contains_rect(area));
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn version_and_media_select_all_copy_the_rows_on_the_active_side() {
        let dir = tempfile::tempdir().unwrap();
        for (instance, flavor, pair, special_field) in [
            (
                921,
                Flavor::Version,
                testing::version_copy_pair(dir.path()),
                "Acme\tNorth\n\"Tools\"",
            ),
            (
                922,
                Flavor::Media,
                testing::media_copy_pair(dir.path()),
                "North\tDivision\n\"Tools\"",
            ),
        ] {
            let mut view = RecordsView::new(flavor, pair.0, pair.1, &context(), instance);
            assert!(!view.accepts(Command::SelectAll));
            assert!(wait_until(Duration::from_secs(10), || {
                view.poll();
                view.has_comparison()
            }));
            let expected = (0..view.listing.rows())
                .filter_map(|row| view.listing.index_at(row))
                .filter(|index| {
                    view.listing
                        .nodes()
                        .get(*index)
                        .is_some_and(|node| node.is_on(crate::model::Side::Left))
                })
                .count();
            assert!(expected > 0);
            assert!(view.accepts(Command::SelectAll));

            view.run(Command::SelectAll);
            assert_eq!(view.selection().len(), expected);
            assert!(view.accepts(Command::Copy));
            view.run(Command::Copy);
            assert!(view.clipboard.is_none());
            let copied = copy_from_command(&mut view);
            let expected_lines: Vec<String> = view
                .edit
                .selection
                .iter()
                .filter_map(|index| view.listing.nodes().get(*index))
                .map(|node| {
                    let value = |side| {
                        node.record(side)
                            .map(|record| record.display.as_str())
                            .unwrap_or_default()
                    };
                    format!(
                        "{}\t{}\t{}",
                        ca_ui::clipboard::table_field(&node.qualified_name()),
                        ca_ui::clipboard::table_field(value(crate::model::Side::Left)),
                        ca_ui::clipboard::table_field(value(crate::model::Side::Right)),
                    )
                })
                .collect();
            assert_eq!(copied, expected_lines.join("\n"));
            let escaped_field = ca_ui::clipboard::table_field(special_field);
            assert!(
                copied.contains(&escaped_field),
                "Select All Copy omitted or failed to quote {special_field:?}: {copied:?}"
            );

            let special_row = (0..view.listing.rows())
                .find(|row| {
                    let Some(index) = view.listing.index_at(*row) else {
                        return false;
                    };
                    view.listing.nodes().get(index).is_some_and(|node| {
                        [crate::model::Side::Left, crate::model::Side::Right]
                            .into_iter()
                            .any(|side| {
                                node.record(side)
                                    .is_some_and(|record| record.display == special_field)
                            })
                    })
                })
                .expect("the special value row is visible");
            let special_index = view
                .listing
                .index_at(special_row)
                .expect("the special value row has a node");
            let special_node = view
                .listing
                .nodes()
                .get(special_index)
                .expect("the special value row is present");
            let value = |side| {
                special_node
                    .record(side)
                    .map(|record| record.display.as_str())
                    .unwrap_or_default()
            };
            let expected_cursor_row = format!(
                "{}\t{}\t{}",
                ca_ui::clipboard::table_field(&special_node.qualified_name()),
                ca_ui::clipboard::table_field(value(crate::model::Side::Left)),
                ca_ui::clipboard::table_field(value(crate::model::Side::Right)),
            );
            view.edit.selection.clear();
            view.listing.set_cursor(special_row);
            assert_eq!(
                view.copy_text().as_deref(),
                Some(expected_cursor_row.as_str())
            );
            view.run(Command::Copy);
            assert_eq!(copy_from_command(&mut view), expected_cursor_row);
        }
    }
}
