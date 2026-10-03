//! Picture comparison: two images, the difference between them, and the
//! controls that decide how that difference is worked out and drawn.
//!
//! The division of labour is the rule the whole crate is built around. Reading,
//! decoding, aligning, comparing and resampling all happen in [`jobs`] on a
//! worker thread. The frame thread lays out panes, hands finished pixels to the
//! graphics device a few tiles at a time through [`tiles`], and paints. The
//! arithmetic that connects a pointer to a pixel lives in [`model`], the
//! decision of when a change becomes a run lives in [`schedule`], and both are
//! testable with no frame at all.

pub mod details;
pub mod jobs;
pub mod model;
pub mod schedule;
pub mod settings;
pub mod status;
pub mod tiles;

use ca_image::compare::{DisplayMode, Offset, Side};
use ca_image::settings::provisional;
use ca_session::SessionKind;
use ca_ui::command::Command;
use ca_ui::dialog::{self, DialogMessage, Pick, Target};
use ca_ui::report::{ReportKind, ViewReport};
use ca_ui::theme::picture::Palette;
use ca_ui::toolbar;
use ca_ui::view::{CommandState, OpenRequest, SessionView, ViewAction, ViewContext};
use ca_ui::widgets;
use ca_ui::worker::Job;
use details::{Layout, PixelDetails};
use jobs::{Message, Outcome, SideImage, Sources};
use model::{Camera, Pane, PaneToggles, Placement};
use schedule::{Scheduler, Stage, DEBOUNCE};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use tiles::Uploader;

/// Width of the control column beside the panes.
///
/// The column carries the offset fields side by side, so it is never narrower
/// than two fields plus the padding around them.
const CONTROLS_WIDTH: f32 = 210.0;

/// Smallest side a pane is given before the layout stops dividing.
const MIN_PANE_SIDE: f32 = 40.0;

/// Width a toolbar drop down is laid out in.
const COMBO_WIDTH: f32 = 148.0;

/// Width a toolbar slider is laid out in.
const SLIDER_WIDTH: f32 = 160.0;

/// Width a numeric field is laid out in.
const FIELD_WIDTH: f32 = 84.0;

/// Image pixels one wheel notch moves the panes.
const WHEEL_STEP: f32 = 48.0;

/// Every command this view answers for.
const HANDLED: &[Command] = &[
    Command::CompareReport,
    Command::Reload,
    Command::Recompare,
    Command::SwapSides,
    Command::OpenFile,
    Command::Cancel,
    Command::NextDisplayMode,
    Command::ZoomIn,
    Command::ZoomOut,
    Command::ZoomToFit,
    Command::ActualSize,
    Command::RotateClockwise,
    Command::RotateCounterclockwise,
    Command::FlipHorizontal,
    Command::FlipVertical,
    Command::NudgeOffsetLeft,
    Command::NudgeOffsetRight,
    Command::NudgeOffsetUp,
    Command::NudgeOffsetDown,
    Command::ResetOffset,
    Command::ToggleAutoScale,
];

/// Where a comparison currently stands.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Status {
    /// Nothing has been asked for yet.
    Idle,
    /// Work is in progress, with the step it has reached.
    Running(&'static str),
    /// The comparison is on screen.
    Ready,
    /// The comparison could not be produced.
    Failed(String),
}

/// The textures of one pane's buffer, and what they were built from.
struct PaneCache {
    uploader: Uploader,
    /// The outcome the textures belong to.
    generation: u64,
    /// True when the textures hold the reduced copy rather than the full
    /// buffer.
    reduced: bool,
    /// The device bound the tiles were cut for.
    max_texture_side: u32,
}

/// What one pane draws this frame.
struct PaneContent {
    image: Arc<ca_image::RgbaImage>,
    /// Shared-space placement of the content, whatever buffer carries it.
    placement: Placement,
    reduced: bool,
}

/// One rectangle the view reports so a layout test can check it.
///
/// Only rectangles a layout can place wrongly are reported: the controls beside
/// the panes and the title of each pane.
#[derive(Debug, Clone)]
pub struct WidgetRect {
    /// What the rectangle holds.
    pub name: String,
    /// Where it was drawn.
    pub rect: egui::Rect,
    /// The pane it belongs to, when it belongs to one.
    pub pane: Option<Pane>,
}

/// How a pane's own message is colored, chosen by the message rather than by
/// the caller so a failure never reads as progress.
type MessageColor = fn(&Palette) -> egui::Color32;

/// A picture comparison tab.
// The toggles are independent view controls with independent stored names, so
// grouping them into enums would change what the settings document holds.
#[allow(clippy::struct_excessive_bools)]
pub struct PictureView {
    /// The sides and the description the session last gave, reported with the
    /// sides replaced by the files the view has open.
    specs: ca_session::settings::SpecsSettings,
    stored: ca_session::settings::binary::PictureCompareSettings,
    id: egui::Id,
    left_path: PathBuf,
    right_path: PathBuf,
    left_field: String,
    right_field: String,
    /// Kept so every job this view spawns asks for the repaint that shows its
    /// result. A job spawned without it posts into a frame loop that is asleep.
    notify: Arc<dyn Fn() + Send + Sync>,
    job: Option<Job<Message>>,
    job_id: u64,
    picker: Option<Job<DialogMessage>>,
    picker_target: Target,
    scheduler: Scheduler,
    settings: jobs::Settings,
    left_image: Option<SideImage>,
    right_image: Option<SideImage>,
    /// The report command of this view.
    report: ViewReport,
    outcome: Option<Box<Outcome>>,
    generation: u64,
    status: Status,
    side_error: [Option<String>; 2],
    camera: Camera,
    panes: PaneToggles,
    focused: Pane,
    caches: HashMap<Pane, PaneCache>,
    checker: Option<egui::TextureHandle>,
    /// The shared-space point under the pointer, from this frame.
    pointer: Option<[f32; 2]>,
    /// Whole pixels of an offset drag that have not been applied yet.
    offset_drag: [f32; 2],
    offset_x_text: String,
    offset_y_text: String,
    /// The side the rotate and flip controls act on.
    active_side: Side,
    show_metadata: bool,
    checkerboard: bool,
    pane_rects: HashMap<Pane, egui::Rect>,
    /// The rectangle each pane's content was painted in, from the last frame.
    image_rects: HashMap<Pane, egui::Rect>,
    /// Rectangles reported for a layout check, rebuilt every frame.
    reported: Vec<WidgetRect>,
    toolbar_rect: egui::Rect,
    /// Lines the toolbar wrapped onto on the last frame.
    toolbar_rows: usize,
    toolbar_collapsed: bool,
    status_rect: egui::Rect,
    /// True while the magnification follows the pane size. A manual zoom
    /// clears it, so a later resize does not undo what was asked for.
    fit_wanted: bool,
    /// The pane size the current fit was computed from. A different size means
    /// the fit is stale and is worked out again.
    fitted_for: Option<[f32; 2]>,
    close_requested: bool,
}

impl PictureView {
    /// A tab over the two sides.
    #[must_use]
    pub fn new(left: PathBuf, right: PathBuf, context: &ViewContext, instance: u64) -> Self {
        let mut view = Self {
            specs: ca_session::settings::SpecsSettings::default(),
            stored: ca_session::settings::binary::PictureCompareSettings::default(),
            id: egui::Id::new(("picture", instance)),
            left_field: left.display().to_string(),
            right_field: right.display().to_string(),
            left_path: left,
            right_path: right,
            notify: Arc::clone(&context.notify),
            job: None,
            job_id: 0,
            picker: None,
            picker_target: Target::Left,
            scheduler: Scheduler::new(),
            settings: jobs::Settings::default(),
            left_image: None,
            right_image: None,
            report: ViewReport::new(
                ReportKind::Picture,
                egui::Id::new(("picture-compare", instance)),
                context.notify.clone(),
            ),
            outcome: None,
            generation: 0,
            status: Status::Idle,
            side_error: [None, None],
            camera: Camera::new(),
            panes: PaneToggles::default(),
            focused: Pane::Difference,
            caches: HashMap::new(),
            checker: None,
            pointer: None,
            offset_drag: [0.0, 0.0],
            offset_x_text: "0".to_owned(),
            offset_y_text: "0".to_owned(),
            active_side: Side::Left,
            show_metadata: ca_image::settings::PictureViewSettings::default().compare_metadata,
            checkerboard: provisional::SHOW_TRANSPARENCY_AS_CHECKERBOARD,
            pane_rects: HashMap::new(),
            image_rects: HashMap::new(),
            reported: Vec::new(),
            toolbar_rect: egui::Rect::NOTHING,
            toolbar_rows: 0,
            toolbar_collapsed: false,
            status_rect: egui::Rect::NOTHING,
            fit_wanted: true,
            fitted_for: None,
            close_requested: false,
        };
        view.scheduler.request_now(Stage::Load, Instant::now());
        view
    }

    /// Which kind of session this view answers for.
    #[must_use]
    pub fn kind() -> SessionKind {
        SessionKind::PictureCompare
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

    /// The settings the next run uses.
    #[must_use]
    pub fn settings(&self) -> &jobs::Settings {
        &self.settings
    }

    /// The camera the three panes share.
    #[must_use]
    pub fn camera(&self) -> Camera {
        self.camera
    }

    /// Which panes are shown.
    #[must_use]
    pub fn panes(&self) -> PaneToggles {
        self.panes
    }

    /// The finished comparison, once there is one.
    #[must_use]
    pub fn outcome(&self) -> Option<&Outcome> {
        self.outcome.as_deref()
    }

    /// The message shown in one pane, when that side could not be read.
    #[must_use]
    pub fn side_error(&self, side: Side) -> Option<&str> {
        self.side_error[side_index(side)].as_deref()
    }

    /// The message shown when the comparison itself could not be produced.
    #[must_use]
    pub fn failure(&self) -> Option<&str> {
        match &self.status {
            Status::Failed(text) => Some(text),
            _ => None,
        }
    }

    /// The rectangle one pane occupied on the last frame, when it was shown.
    #[must_use]
    pub fn pane_rect(&self, pane: Pane) -> Option<egui::Rect> {
        self.pane_rects.get(&pane).copied()
    }

    /// The rectangle one pane's content was painted in, when it was painted.
    #[must_use]
    pub fn image_rect(&self, pane: Pane) -> Option<egui::Rect> {
        self.image_rects.get(&pane).copied()
    }

    /// The rectangles the view reports for a layout check.
    #[must_use]
    pub fn widget_rects(&self) -> &[WidgetRect] {
        &self.reported
    }

    /// The rectangle the toolbar occupied on the last frame.
    #[must_use]
    pub fn toolbar_rect(&self) -> egui::Rect {
        self.toolbar_rect
    }

    /// How many lines the toolbar wrapped onto on the last frame.
    #[must_use]
    pub fn toolbar_rows(&self) -> usize {
        self.toolbar_rows
    }

    /// The rectangle the status bar occupied on the last frame.
    #[must_use]
    pub fn status_rect(&self) -> egui::Rect {
        self.status_rect
    }

    /// True while the magnification follows the pane size.
    #[must_use]
    pub fn fits_to_panes(&self) -> bool {
        self.fit_wanted
    }

    /// True when a transparent pixel is drawn over a checkerboard.
    #[must_use]
    pub const fn shows_checkerboard(&self) -> bool {
        self.checkerboard
    }

    /// True when the facts of each picture are shown.
    #[must_use]
    pub const fn shows_metadata(&self) -> bool {
        self.show_metadata
    }

    /// True when the toolbar moved its controls behind one button because the
    /// window was too narrow to carry them.
    #[must_use]
    pub fn toolbar_collapsed(&self) -> bool {
        self.toolbar_collapsed
    }

    /// The readout for the point under the pointer, when the pointer is over a
    /// pane and a comparison is on screen.
    #[must_use]
    pub fn pixel_details(&self) -> Option<PixelDetails> {
        let point = self.pointer?;
        let outcome = self.outcome.as_ref()?;
        let layout = layout_of(outcome);
        #[allow(clippy::cast_possible_truncation)]
        let whole = [point[0].floor() as i32, point[1].floor() as i32];
        Some(details::sample(
            whole,
            layout,
            &outcome.left,
            &outcome.right,
            &outcome.mask,
        ))
    }

    /// Choose how the difference pane renders.
    pub fn set_mode(&mut self, mode: DisplayMode) {
        if self.settings.mode != mode {
            self.settings.mode = mode;
            self.ask(Stage::Compare);
        }
    }

    /// Set the greatest per-channel difference still treated as unimportant.
    pub fn set_tolerance(&mut self, tolerance: u8) {
        if self.settings.tolerance != tolerance {
            self.settings.tolerance = tolerance;
            self.ask(Stage::Compare);
        }
    }

    /// Decide whether differences at or below the tolerance count as matches.
    pub fn set_ignore_unimportant(&mut self, ignore: bool) {
        if self.settings.ignore_unimportant != ignore {
            self.settings.ignore_unimportant = ignore;
            self.ask(Stage::Compare);
        }
    }

    /// Set the weight of the left image in blend mode.
    pub fn set_blend_percent(&mut self, percent: u8) {
        let percent = percent.min(100);
        if self.settings.blend_percent != percent {
            self.settings.blend_percent = percent;
            self.ask(Stage::Compare);
        }
    }

    /// Choose the side single side mode renders, which the blend toggle also
    /// switches.
    pub fn set_side(&mut self, side: Side) {
        if self.settings.side != side {
            self.settings.side = side;
            self.ask(Stage::Compare);
        }
    }

    /// Decide whether the smaller image is enlarged to the larger one's scale.
    pub fn set_auto_scale(&mut self, auto_scale: bool) {
        if self.settings.auto_scale != auto_scale {
            self.settings.auto_scale = auto_scale;
            self.ask(Stage::Compare);
        }
    }

    /// Set the displacement applied to the right image.
    pub fn set_offset(&mut self, offset: Offset) {
        if self.settings.offset != offset {
            self.settings.offset = offset;
            self.sync_offset_fields();
            self.ask(Stage::Compare);
        }
    }

    /// Move the displacement by whole pixels.
    pub fn nudge(&mut self, dx: i32, dy: i32) {
        self.nudge_offset(dx, dy);
    }

    /// Return the displacement to zero, realigning both top-left corners.
    pub fn reset_difference_offset(&mut self) {
        self.reset_offset();
    }

    /// Turn one side a quarter turn.
    pub fn rotate(&mut self, side: Side, clockwise: bool) {
        if clockwise {
            self.settings.transform_mut(side).rotate_clockwise();
        } else {
            self.settings.transform_mut(side).rotate_counterclockwise();
        }
        self.ask(Stage::Compare);
    }

    /// Reflect one side across an axis.
    pub fn flip(&mut self, side: Side, across_vertical_axis: bool) {
        if across_vertical_axis {
            self.settings.transform_mut(side).toggle_flip_horizontal();
        } else {
            self.settings.transform_mut(side).toggle_flip_vertical();
        }
        self.ask(Stage::Compare);
    }

    /// Show or hide one pane. The last pane shown cannot be hidden.
    pub fn show_pane(&mut self, pane: Pane, shown: bool) {
        self.panes.set(pane, shown);
    }

    /// One step of magnification in.
    pub fn zoom_in(&mut self) {
        let center = self.pane_center();
        self.end_fit();
        self.camera.zoom_in_about([0.0, 0.0], center);
    }

    /// One step of magnification out.
    pub fn zoom_out(&mut self) {
        let center = self.pane_center();
        self.end_fit();
        self.camera.zoom_out_about([0.0, 0.0], center);
    }

    /// One screen point per image pixel.
    pub fn actual_size(&mut self) {
        let room = self.pane_size();
        self.end_fit();
        self.camera.actual_size(room);
    }

    /// Fit the result to the panes on the next frame, once their size is
    /// known, and keep it fitted while they are resized.
    pub fn zoom_to_fit(&mut self) {
        self.fit_wanted = true;
        self.fitted_for = None;
    }

    /// Stop following the pane size, because a magnification was asked for.
    fn end_fit(&mut self) {
        self.fit_wanted = false;
        self.fitted_for = None;
    }

    /// Ask for a run, letting the delay collapse a burst of changes into one.
    fn ask(&mut self, stage: Stage) {
        self.scheduler.request(stage, Instant::now(), DEBOUNCE);
    }

    /// Ask for a run that starts at the next poll.
    fn ask_now(&mut self, stage: Stage) {
        self.scheduler.request_now(stage, Instant::now());
    }

    /// Start whatever run has come due, cancelling the one it supersedes.
    fn start_due(&mut self) {
        let Some((id, stage)) = self.scheduler.take_due(Instant::now()) else {
            return;
        };
        if let Some(job) = self.job.take() {
            job.cancel();
        }
        self.job_id = id;
        self.status = Status::Running("Starting");
        let settings = self.settings.clone();
        let notify = Arc::clone(&self.notify);
        let decoded = if stage == Stage::Compare {
            self.sources()
        } else {
            None
        };
        self.job = Some(if let Some(sources) = decoded {
            jobs::spawn_compare(sources, settings, notify)
        } else {
            self.side_error = [None, None];
            self.left_image = None;
            self.right_image = None;
            jobs::spawn_load(
                self.left_path.clone(),
                self.right_path.clone(),
                settings,
                notify,
            )
        });
    }

    fn drain_job(&mut self) {
        let Some(job) = self.job.as_mut() else {
            return;
        };
        let messages = job.drain();
        let mut finished = job.is_finished();
        let current = self.scheduler.is_current(self.job_id);
        for message in messages {
            finished |= ca_ui::worker::Terminal::is_terminal(&message);
            if !current {
                continue;
            }
            self.apply(message);
        }
        if finished {
            self.job = None;
        }
    }

    fn apply(&mut self, message: Message) {
        match message {
            Message::Progress(step) => self.status = Status::Running(step),
            Message::SideReady(side, image) => {
                self.side_error[side_index(side)] = None;
                self.remember_side(side, *image);
            }
            Message::SideFailed(side, text) => {
                self.side_error[side_index(side)] = Some(text);
            }
            Message::Ready(outcome) => {
                self.outcome = Some(outcome);
                self.generation = self.generation.wrapping_add(1);
                self.caches.clear();
                self.status = Status::Ready;
            }
            Message::Failed(text) => {
                self.outcome = None;
                self.caches.clear();
                self.status = Status::Failed(text);
            }
            Message::Cancelled => {
                if matches!(self.status, Status::Running(_)) {
                    self.status = Status::Idle;
                }
            }
        }
    }

    /// Keep a decoded side so a later change can be compared without reading
    /// the file again.
    fn remember_side(&mut self, side: Side, image: SideImage) {
        match side {
            Side::Left => self.left_image = Some(image),
            Side::Right => self.right_image = Some(image),
        }
    }

    /// Both decoded sides, once both of them have arrived.
    fn sources(&self) -> Option<Sources> {
        Some(Sources {
            left: self.left_image.clone()?,
            right: self.right_image.clone()?,
        })
    }

    fn drain_picker(&mut self) {
        let Some(picker) = self.picker.as_mut() else {
            return;
        };
        let messages = picker.drain();
        let finished = picker.is_finished();
        for message in messages {
            if let DialogMessage::Chosen(path) = message {
                match self.picker_target {
                    Target::Left => self.left_field = path.display().to_string(),
                    Target::Right => self.right_field = path.display().to_string(),
                }
                self.adopt_fields();
            }
        }
        if finished {
            self.picker = None;
        }
    }

    fn open_picker(&mut self, target: Target) {
        if self.picker.is_some() {
            return;
        }
        self.picker_target = target;
        self.picker = Some(dialog::spawn(Pick::File, Arc::clone(&self.notify)));
    }

    /// Take the two path fields as the sides and read them again.
    fn adopt_fields(&mut self) {
        self.left_path = PathBuf::from(self.left_field.clone());
        self.right_path = PathBuf::from(self.right_field.clone());
        self.left_image = None;
        self.right_image = None;
        self.outcome = None;
        self.caches.clear();
        self.zoom_to_fit();
        self.ask_now(Stage::Load);
    }

    fn swap_sides(&mut self) {
        std::mem::swap(&mut self.left_path, &mut self.right_path);
        std::mem::swap(&mut self.left_field, &mut self.right_field);
        std::mem::swap(
            &mut self.settings.left_transform,
            &mut self.settings.right_transform,
        );
        std::mem::swap(
            &mut self.settings.left_format,
            &mut self.settings.right_format,
        );
        self.side_error.swap(0, 1);
        std::mem::swap(&mut self.left_image, &mut self.right_image);
        // The offset displaces the right image, so exchanging the sides
        // reverses what it means.
        self.settings.offset = Offset::new(-self.settings.offset.x, -self.settings.offset.y);
        self.sync_offset_fields();
        self.ask_now(Stage::Compare);
    }

    fn sync_offset_fields(&mut self) {
        self.offset_x_text = self.settings.offset.x.to_string();
        self.offset_y_text = self.settings.offset.y.to_string();
    }

    fn nudge_offset(&mut self, dx: i32, dy: i32) {
        self.settings.offset = self.settings.offset.nudged(dx, dy);
        self.sync_offset_fields();
        self.ask(Stage::Compare);
    }

    fn reset_offset(&mut self) {
        self.settings.offset = Offset::zero();
        self.offset_drag = [0.0, 0.0];
        self.sync_offset_fields();
        self.ask(Stage::Compare);
    }

    /// The content one pane draws, choosing the reduced copy when the
    /// magnification makes the full buffer unnecessary.
    fn content(&self, pane: Pane) -> Option<PaneContent> {
        let outcome = self.outcome.as_ref()?;
        let (image, preview, origin) = match pane {
            Pane::Left => (
                Arc::clone(&outcome.left),
                outcome.left_preview.as_ref(),
                [0, 0],
            ),
            Pane::Right => (
                Arc::clone(&outcome.right),
                outcome.right_preview.as_ref(),
                [outcome.offset.x, outcome.offset.y],
            ),
            Pane::Difference => (
                Arc::clone(&outcome.result),
                outcome.preview.as_ref(),
                [outcome.origin.x, outcome.origin.y],
            ),
        };
        let placement = Placement::new(origin, [image.width(), image.height()]);
        if let Some(preview) = preview {
            // The reduced copy carries the same content, so it is used
            // whenever one of its pixels covers at least one screen point.
            let factor = f64::from(preview.width()) / f64::from(image.width().max(1));
            #[allow(clippy::cast_possible_truncation)]
            if f64::from(self.camera.zoom) <= factor {
                return Some(PaneContent {
                    image: Arc::clone(preview),
                    placement,
                    reduced: true,
                });
            }
        }
        Some(PaneContent {
            image,
            placement,
            reduced: false,
        })
    }

    fn checker_texture(&mut self, ctx: &egui::Context) -> egui::TextureHandle {
        if let Some(handle) = self.checker.as_ref() {
            return handle.clone();
        }
        let light = color_of(provisional::CHECKER_LIGHT);
        let dark = color_of(provisional::CHECKER_DARK);
        let image = egui::ColorImage {
            size: [2, 2],
            pixels: vec![light, dark, dark, light],
        };
        let handle = ctx.load_texture(
            "picture-checker",
            image,
            egui::TextureOptions {
                magnification: egui::TextureFilter::Nearest,
                minification: egui::TextureFilter::Nearest,
                wrap_mode: egui::TextureWrapMode::Repeat,
                mipmap_mode: None,
            },
        );
        self.checker = Some(handle.clone());
        handle
    }
}

/// The index one side occupies in the per-side arrays.
const fn side_index(side: Side) -> usize {
    match side {
        Side::Left => 0,
        Side::Right => 1,
    }
}

fn color_of(pixel: [u8; 4]) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(pixel[0], pixel[1], pixel[2], pixel[3])
}

/// The name a display mode carries in the controls.
const fn mode_label(mode: DisplayMode) -> &'static str {
    match mode {
        DisplayMode::Tolerance => "Tolerance",
        DisplayMode::MismatchRange => "Mismatch range",
        DisplayMode::Blend => "Blend",
        DisplayMode::SingleSide => "Single side",
        DisplayMode::ChannelDifference => "Channel difference",
        DisplayMode::ChannelXor => "Channel exclusive or",
    }
}

/// Every mode the controls offer, in the order they are listed.
const MODES: [DisplayMode; 6] = [
    DisplayMode::Tolerance,
    DisplayMode::MismatchRange,
    DisplayMode::Blend,
    DisplayMode::SingleSide,
    DisplayMode::ChannelDifference,
    DisplayMode::ChannelXor,
];

/// The mode after `mode`, wrapping at the end of the list.
fn next_mode(mode: DisplayMode) -> DisplayMode {
    let at = MODES.iter().position(|known| *known == mode).unwrap_or(0);
    MODES[(at + 1) % MODES.len()]
}

const fn side_label(side: Side) -> &'static str {
    match side {
        Side::Left => "Left",
        Side::Right => "Right",
    }
}

impl SessionView for PictureView {
    fn kind(&self) -> Option<ca_session::SessionKind> {
        Some(ca_session::SessionKind::PictureCompare)
    }

    fn title(&self) -> String {
        let name = |path: &Path| {
            path.file_name().map_or_else(
                || path.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            )
        };
        format!("{} - {}", name(&self.left_path), name(&self.right_path))
    }

    fn tick(&mut self) {
        self.start_due();
        self.drain_job();
        self.drain_picker();
    }

    fn ui(&mut self, ui: &mut egui::Ui, context: &ViewContext) -> Vec<ViewAction> {
        let palette = *ca_ui::theme::picture::palette(theme_variant(ui));
        let mut actions = Vec::new();
        ui.painter()
            .rect_filled(ui.max_rect(), 0.0, palette.background);
        self.report.poll(ui.ctx());
        self.reported.clear();
        self.image_rects.clear();
        // The status bar claims its height before the toolbar does, so a
        // toolbar that wraps onto more lines never pushes it off the window.
        let status = egui::TopBottomPanel::bottom(self.id.with("status")).show_inside(ui, |ui| {
            self.status_bar(ui, &palette);
        });
        self.status_rect = status.response.rect;
        egui::TopBottomPanel::top(self.id.with("head")).show_inside(ui, |ui| {
            self.path_bar(ui);
            self.toolbar(ui, &mut actions);
            self.report_panel(ui);
        });
        self.keyboard(ui);
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show_inside(ui, |ui| {
                self.body(ui, &palette);
            });
        if self.scheduler.has_pending() || self.job.is_some() {
            ui.ctx().request_repaint_after(DEBOUNCE);
        }
        let _ = context;
        actions
    }

    fn commands(&self) -> Vec<CommandState> {
        ca_ui::view::declare(HANDLED, |command| match command {
            Command::Cancel => self.job.is_some(),
            Command::OpenFile => self.picker.is_none(),
            Command::Recompare => self.left_image.is_some() && self.right_image.is_some(),
            Command::CompareReport => self.is_ready(),
            _ => true,
        })
    }

    fn run(&mut self, command: Command) {
        match command {
            Command::CompareReport if self.is_ready() => self.report.request(),
            Command::OpenFile => self.open_picker(match self.active_side {
                Side::Left => Target::Left,
                Side::Right => Target::Right,
            }),
            Command::Reload => self.adopt_fields(),
            Command::Recompare => self.ask_now(Stage::Compare),
            Command::SwapSides => self.swap_sides(),
            Command::NextDisplayMode => self.set_mode(next_mode(self.settings.mode)),
            Command::ZoomIn => self.zoom_in(),
            Command::ZoomOut => self.zoom_out(),
            Command::ZoomToFit => self.zoom_to_fit(),
            Command::ActualSize => self.actual_size(),
            // The transforms act on the side the controls name; the offset
            // displaces the right image, which is what the offset means.
            Command::RotateClockwise => self.rotate(self.active_side, true),
            Command::RotateCounterclockwise => self.rotate(self.active_side, false),
            Command::FlipHorizontal => self.flip(self.active_side, true),
            Command::FlipVertical => self.flip(self.active_side, false),
            Command::NudgeOffsetLeft => self.nudge_offset(-provisional::OFFSET_NUDGE, 0),
            Command::NudgeOffsetRight => self.nudge_offset(provisional::OFFSET_NUDGE, 0),
            Command::NudgeOffsetUp => self.nudge_offset(0, -provisional::OFFSET_NUDGE),
            Command::NudgeOffsetDown => self.nudge_offset(0, provisional::OFFSET_NUDGE),
            Command::ResetOffset => self.reset_offset(),
            Command::ToggleAutoScale => self.set_auto_scale(!self.settings.auto_scale),
            Command::Cancel => {
                if let Some(job) = self.job.take() {
                    job.cancel();
                }
                self.scheduler.clear();
                self.status = Status::Idle;
            }
            _ => {}
        }
    }

    fn apply_settings(&mut self, settings: &ca_session::settings::SessionSettings) {
        let ca_session::settings::SessionSettings::PictureCompare(picture) = settings else {
            return;
        };
        self.specs.clone_from(&picture.specs);
        let built = crate::settings::options_over(picture, &self.settings);
        self.stored.clone_from(picture);
        // The whole stored set is compared, so a change to any one field
        // reaches a run and an unchanged set starts none.
        if crate::settings::stored_from(&built) == crate::settings::stored_from(&self.settings) {
            return;
        }
        self.settings = built;
        self.ask_now(schedule::Stage::Compare);
    }

    fn settings(&self) -> Option<ca_session::settings::SessionSettings> {
        let mut settings = crate::settings::stored_over(&self.settings, &self.stored);
        settings.specs = ca_ui::view::with_sides(&self.specs, &self.left_path, &self.right_path);
        Some(ca_session::settings::SessionSettings::PictureCompare(
            settings,
        ))
    }

    fn is_ready(&self) -> bool {
        matches!(self.status, Status::Ready)
            && self.outcome.is_some()
            && !self.scheduler.has_pending()
            && self.job.is_none()
    }

    fn notice(&self) -> Option<String> {
        let outcome = self.outcome.as_ref()?;
        let sources = self.sources()?;
        status::fidelity_note(
            outcome.totals,
            sources.left.metadata.fidelity,
            sources.right.metadata.fidelity,
        )
    }

    fn wants_close(&self) -> bool {
        self.close_requested
    }

    fn on_close(&mut self) {
        if let Some(job) = self.job.take() {
            job.cancel();
        }
        self.scheduler.clear();
    }
}

fn theme_variant(ui: &egui::Ui) -> ca_ui::theme::Variant {
    ca_ui::theme::Variant::from_dark_mode(ui.visuals().dark_mode)
}

impl PictureView {
    fn path_bar(&mut self, ui: &mut egui::Ui) {
        let mut left = std::mem::take(&mut self.left_field);
        let mut right = std::mem::take(&mut self.right_field);
        let action = widgets::path_bar(ui, &mut left, &mut right, "Reload");
        self.left_field = left;
        self.right_field = right;
        match action {
            Some(widgets::PathBarAction::Browse(target)) => self.open_picker(target),
            Some(widgets::PathBarAction::Reload) => self.adopt_fields(),
            None => {}
        }
    }

    /// The toolbar items this view declares, in the state it is in now.
    #[must_use]
    pub fn toolbar_items(&self) -> Vec<toolbar::Item> {
        vec![
            toolbar::Item::widget("home", 70.0),
            toolbar::Item::widget("mode", COMBO_WIDTH),
            toolbar::Item::widget("tolerance", SLIDER_WIDTH),
            toolbar::Item::widget("zoom-in", 90.0),
            toolbar::Item::widget("zoom-out", 100.0),
            toolbar::Item::widget("actual-size", 60.0),
            toolbar::Item::widget("fit", 60.0),
            toolbar::Item::widget("panes", 320.0),
            toolbar::Item::separator("separator-1"),
            toolbar::Item::command(
                "report",
                Command::CompareReport,
                "Report",
                self.is_ready(),
                "Available once the comparison finishes",
            ),
            toolbar::Item::widget("more", 80.0),
        ]
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, actions: &mut Vec<ViewAction>) {
        let row = widgets::toolbar_height(ui) + ui.spacing().item_spacing.y;
        let mut changed = None;
        let items = self.toolbar_items();
        let layout = toolbar::Layout::from_options(
            &ca_ui::options::runtime::current(ui.ctx()).stored.commands,
            toolbar::ToolbarView::Picture,
        );
        let mut extras: Vec<ViewAction> = Vec::new();
        let outcome = toolbar::show_for(
            toolbar::ToolbarView::Picture,
            ui,
            self.id.with("toolbar"),
            &items,
            &layout,
            |ui, name| {
                if name == "more" {
                    // The rarely reached controls stay behind one button, so the
                    // bar holds two lines at a common window width.
                    widgets::icon_menu(ui, "More", ca_ui::icons::Icon::More, |ui| {
                        ui.set_min_width(200.0);
                        self.extra_controls(ui, &mut extras, &mut changed);
                    });
                    return;
                }
                self.common_control(ui, name, &mut extras, &mut changed);
            },
        );
        actions.extend(extras);
        if outcome.command == Some(Command::CompareReport) {
            self.report.request();
        }
        self.toolbar_collapsed = outcome.collapsed;
        self.toolbar_rect = outcome.rect;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let rows = (self.toolbar_rect.height() / row.max(1.0)).round().max(1.0) as usize;
        self.toolbar_rows = rows;
        if let Some(stage) = changed {
            self.ask(stage);
        }
    }

    /// The comparison the report is written from.
    #[must_use]
    ///
    /// The report carries the counts and the two pictures' facts, never the
    /// pixels, so nothing large is copied.
    pub fn report_payload(&self) -> (ca_ui::report::ReportMeta, ca_ui::report::Payload) {
        let meta = ca_ui::report::ReportMeta::new(
            self.left_path.display().to_string(),
            self.right_path.display().to_string(),
        )
        .with_title(ReportKind::Picture.title());
        let facts = ca_ui::report::PictureFacts {
            left: picture_side(self.left_image.as_ref()),
            right: picture_side(self.right_image.as_ref()),
            totals: self
                .outcome
                .as_ref()
                .map(|outcome| ca_ui::report::PixelTotals::from(outcome.totals))
                .unwrap_or_default(),
            tolerance: self
                .outcome
                .as_ref()
                .map_or(self.settings.tolerance, |outcome| outcome.tolerance),
            difference_png: None,
        };
        (meta, ca_ui::report::Payload::Picture(Box::new(facts)))
    }

    fn report_panel(&mut self, ui: &mut egui::Ui) {
        if !self.report.is_open() {
            return;
        }
        if self.report.draw(ui) == ca_ui::report::ReportAction::Write && self.is_ready() {
            let (meta, payload) = self.report_payload();
            self.report.start(meta, payload, 0);
        }
        if let Some(text) = self.report.take_clipboard() {
            ui.ctx().copy_text(text);
        }
    }

    /// One of the controls the toolbar always carries.
    fn common_control(
        &mut self,
        ui: &mut egui::Ui,
        name: &str,
        actions: &mut Vec<ViewAction>,
        changed: &mut Option<Stage>,
    ) {
        match name {
            "home" => {
                if widgets::toolbar_button(ui, "Home", true, "") {
                    actions.push(ViewAction::OpenHome);
                }
            }
            "mode" => {
                egui::ComboBox::from_id_salt(self.id.with("mode"))
                    .selected_text(mode_label(self.settings.mode))
                    .width(COMBO_WIDTH - 8.0)
                    .show_ui(ui, |ui| {
                        for mode in MODES {
                            if ui
                                .selectable_label(self.settings.mode == mode, mode_label(mode))
                                .clicked()
                                && self.settings.mode != mode
                            {
                                self.settings.mode = mode;
                                *changed = Some(Stage::Compare);
                            }
                        }
                    });
            }
            "tolerance" => {
                if ui
                    .add(egui::Slider::new(&mut self.settings.tolerance, 0..=255).text("Tolerance"))
                    .changed()
                {
                    *changed = Some(Stage::Compare);
                }
            }
            "zoom-in" => {
                if widgets::toolbar_button(ui, "Zoom in", true, "") {
                    self.zoom_in();
                }
            }
            "zoom-out" => {
                if widgets::toolbar_button(ui, "Zoom out", true, "") {
                    self.zoom_out();
                }
            }
            "actual-size" => {
                if widgets::toolbar_button(ui, "1:1", true, "") {
                    self.actual_size();
                }
            }
            "fit" => {
                if widgets::toolbar_button(ui, "Fit", true, "") {
                    self.zoom_to_fit();
                }
            }
            "panes" => {
                for pane in Pane::ALL {
                    let mut shown = self.panes.shows(pane);
                    if ui.checkbox(&mut shown, pane.label()).changed() {
                        self.show_pane(pane, shown);
                    }
                }
            }
            _ => {}
        }
    }

    /// The controls that sit behind the overflow button.
    ///
    /// These act on one side or are reached rarely, so they leave the bar to
    /// keep it inside two lines at a common window width.
    #[allow(clippy::too_many_lines)]
    fn extra_controls(
        &mut self,
        ui: &mut egui::Ui,
        actions: &mut Vec<ViewAction>,
        changed: &mut Option<Stage>,
    ) {
        widgets::sized(ui, SLIDER_WIDTH, |ui| {
            let enabled = self.settings.mode == DisplayMode::Blend;
            if ui
                .add_enabled(
                    enabled,
                    egui::Slider::new(&mut self.settings.blend_percent, 0..=100).text("Blend %"),
                )
                .changed()
            {
                *changed = Some(Stage::Compare);
            }
        });
        if ui
            .selectable_label(self.settings.side == Side::Right, "Blend toggle")
            .clicked()
        {
            self.settings.side = self.settings.side.other();
            *changed = Some(Stage::Compare);
        }
        if ui
            .checkbox(&mut self.settings.ignore_unimportant, "Minor")
            .changed()
        {
            *changed = Some(Stage::Compare);
        }
        if ui
            .checkbox(&mut self.settings.ignore_alpha, "Ignore alpha")
            .changed()
        {
            *changed = Some(Stage::Compare);
        }
        if ui
            .checkbox(
                &mut self.settings.transparent_pixels_equal,
                "Transparent equal",
            )
            .changed()
        {
            *changed = Some(Stage::Compare);
        }
        if ui
            .checkbox(&mut self.settings.auto_scale, "Auto scale")
            .changed()
        {
            *changed = Some(Stage::Compare);
        }
        widgets::sized(ui, COMBO_WIDTH, |ui| {
            egui::ComboBox::from_id_salt(self.id.with("side"))
                .selected_text(format!("Acts on: {}", side_label(self.active_side)))
                .width(COMBO_WIDTH - 8.0)
                .show_ui(ui, |ui| {
                    for side in [Side::Left, Side::Right] {
                        if ui
                            .selectable_label(self.active_side == side, side_label(side))
                            .clicked()
                        {
                            self.active_side = side;
                        }
                    }
                });
        });
        let side = self.active_side;
        if widgets::with_toolbar_icon(ui, ca_ui::icons::Icon::RotateClockwise, true, |ui| {
            widgets::toolbar_button(ui, "Rotate right", true, "")
        }) {
            self.rotate(side, true);
        }
        if widgets::with_toolbar_icon(ui, ca_ui::icons::Icon::RotateCounterclockwise, true, |ui| {
            widgets::toolbar_button(ui, "Rotate left", true, "")
        }) {
            self.rotate(side, false);
        }
        if widgets::with_toolbar_icon(ui, ca_ui::icons::Icon::FlipHorizontal, true, |ui| {
            widgets::toolbar_button(ui, "Flip across y", true, "")
        }) {
            self.flip(side, true);
        }
        if widgets::with_toolbar_icon(ui, ca_ui::icons::Icon::FlipVertical, true, |ui| {
            widgets::toolbar_button(ui, "Flip across x", true, "")
        }) {
            self.flip(side, false);
        }
        ui.checkbox(&mut self.checkerboard, "Checkerboard");
        ui.checkbox(&mut self.show_metadata, "Metadata");
        if widgets::with_toolbar_icon(ui, ca_ui::icons::Icon::SwapSides, true, |ui| {
            widgets::toolbar_button(ui, "Swap", true, "")
        }) {
            self.swap_sides();
        }
        if widgets::with_toolbar_icon(ui, ca_ui::icons::Icon::Reload, true, |ui| {
            widgets::toolbar_button(ui, "Reload", true, "")
        }) {
            self.adopt_fields();
        }
        if widgets::with_toolbar_icon(ui, ca_ui::icons::Icon::Stop, true, |ui| {
            widgets::toolbar_button(ui, "Stop", self.job.is_some(), "Nothing is running")
        }) {
            self.run(Command::Cancel);
        }
        if widgets::with_toolbar_icon(ui, ca_ui::icons::Icon::SessionHexCompare, true, |ui| {
            widgets::toolbar_button(ui, "Compare as hex", true, "")
        }) {
            actions.push(ViewAction::Open(OpenRequest::new(
                SessionKind::HexCompare,
                self.left_path.clone(),
                self.right_path.clone(),
            )));
        }
    }

    /// The size of the largest pane laid out on the last frame, which is what
    /// a zoom command without a pointer works against.
    fn pane_size(&self) -> [f32; 2] {
        self.pane_rects
            .values()
            .map(|rect| [rect.width(), rect.height()])
            .max_by(|a, b| {
                (a[0] * a[1])
                    .partial_cmp(&(b[0] * b[1]))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap_or([MIN_PANE_SIDE, MIN_PANE_SIDE])
    }

    fn pane_center(&self) -> [f32; 2] {
        let size = self.pane_size();
        [size[0] / 2.0, size[1] / 2.0]
    }

    /// Read the keys this view handles itself.
    ///
    /// The shared routing table has no picture commands, so the offset nudge
    /// and the zoom keys are read here. A field with the keyboard focus takes
    /// precedence, otherwise typing a path would move the offset.
    fn keyboard(&mut self, ui: &mut egui::Ui) {
        if ui.memory(egui::Memory::focused).is_some() {
            return;
        }
        let events: Vec<(egui::Key, egui::Modifiers)> = ui.input(|input| {
            input
                .events
                .iter()
                .filter_map(|event| match event {
                    egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => Some((*key, *modifiers)),
                    _ => None,
                })
                .collect()
        });
        for (key, modifiers) in events {
            let step = if modifiers.command || modifiers.shift {
                provisional::OFFSET_NUDGE_LARGE
            } else {
                provisional::OFFSET_NUDGE
            };
            match key {
                egui::Key::ArrowLeft => self.nudge_offset(-step, 0),
                egui::Key::ArrowRight => self.nudge_offset(step, 0),
                egui::Key::ArrowUp => self.nudge_offset(0, -step),
                egui::Key::ArrowDown => self.nudge_offset(0, step),
                egui::Key::Plus | egui::Key::Equals if modifiers.command => self.zoom_in(),
                egui::Key::Minus if modifiers.command => self.zoom_out(),
                egui::Key::Num0 if modifiers.command => self.actual_size(),
                _ => {}
            }
        }
    }

    /// The control column and the panes beside it.
    ///
    /// The panes are equal columns of the full body height. Equal panes are
    /// what lets one camera place the same image pixel at the same position in
    /// every pane, so the three views stay comparable by eye.
    fn body(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        let room = ui.available_rect_before_wrap();
        let shown: Vec<Pane> = Pane::ALL
            .into_iter()
            .filter(|pane| self.panes.shows(*pane))
            .collect();
        let controls_width = CONTROLS_WIDTH.min((room.width() - MIN_PANE_SIDE).max(0.0));
        let controls = egui::Rect::from_min_size(
            room.min,
            egui::vec2(controls_width, room.height().max(MIN_PANE_SIDE)),
        );

        self.pointer = None;
        let rects = pane_columns(room, controls_width, &shown);
        self.refit(&rects);

        for pane in Pane::ALL {
            let Some(rect) = rects.get(&pane).copied() else {
                continue;
            };
            self.paint_pane(ui, pane, rect, palette);
        }
        self.pane_rects = rects;

        self.control_column(ui, controls, palette);
        ui.allocate_rect(room, egui::Sense::hover());
    }

    /// Work the magnification out again when the fit is missing or stale.
    ///
    /// The smallest pane decides, so the content fits in every pane and not
    /// only in the widest one.
    fn refit(&mut self, rects: &HashMap<Pane, egui::Rect>) {
        if !self.fit_wanted {
            return;
        }
        let Some(rect) = smallest(rects) else {
            return;
        };
        let size = [rect.width(), rect.height()];
        if self.fitted_for == Some(size) {
            return;
        }
        let Some(bounds) = self.content_bounds() else {
            return;
        };
        self.camera.fit_bounds(size, bounds);
        self.fitted_for = Some(size);
    }

    /// The shared-space rectangle that covers every pane's content.
    ///
    /// A result can start before the left image's corner, so the fit works
    /// against the union rather than against one buffer.
    fn content_bounds(&self) -> Option<[f32; 4]> {
        let outcome = self.outcome.as_ref()?;
        let mut bounds: Option<[f32; 4]> = None;
        for pane in Pane::ALL {
            let placement = match pane {
                Pane::Left => Placement::new([0, 0], [outcome.left.width(), outcome.left.height()]),
                Pane::Right => Placement::new(
                    [outcome.offset.x, outcome.offset.y],
                    [outcome.right.width(), outcome.right.height()],
                ),
                Pane::Difference => Placement::new(
                    [outcome.origin.x, outcome.origin.y],
                    [outcome.result.width(), outcome.result.height()],
                ),
            };
            let next = placement.bounds();
            bounds = Some(match bounds {
                None => next,
                Some(held) => [
                    held[0].min(next[0]),
                    held[1].min(next[1]),
                    held[2].max(next[2]),
                    held[3].max(next[3]),
                ],
            });
        }
        bounds
    }

    #[allow(clippy::too_many_lines)]
    fn paint_pane(&mut self, ui: &mut egui::Ui, pane: Pane, rect: egui::Rect, palette: &Palette) {
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, palette.pane_background);
        let response = ui.interact(
            rect,
            self.id.with(("pane", pane.label())),
            egui::Sense::click_and_drag(),
        );
        if response.clicked() {
            self.focused = pane;
        }

        // Every per-image byte below is either already prepared by a worker or
        // bounded by the upload budget; nothing here reads a file, compares or
        // resamples.
        if let Some(message) = self.pane_message(pane) {
            painter.text(
                rect.left_top() + egui::vec2(8.0, 8.0),
                egui::Align2::LEFT_TOP,
                message.0,
                egui::FontId::proportional(13.0),
                message.1(palette),
            );
        } else if let Some(content) = self.content(pane) {
            self.draw_content(ui, pane, rect, &content);
        }

        self.handle_pane_input(ui, pane, rect, &response);

        let outline = if self.focused == pane {
            palette.pane_border_focused
        } else {
            palette.pane_border
        };
        painter.rect_stroke(
            rect,
            0.0,
            egui::Stroke::new(1.0, outline),
            egui::StrokeKind::Inside,
        );
        let title = painter.text(
            rect.left_top() + egui::vec2(6.0, 4.0),
            egui::Align2::LEFT_TOP,
            pane.label(),
            egui::FontId::proportional(11.0),
            palette.label_text,
        );
        self.reported.push(WidgetRect {
            name: format!("{} title", pane.label()),
            rect: title,
            pane: Some(pane),
        });
        self.draw_crosshair(ui, pane, rect, palette);
    }

    /// What a pane says instead of an image, when it has no image to show.
    fn pane_message(&self, pane: Pane) -> Option<(String, MessageColor)> {
        let side = match pane {
            Pane::Left => Some(Side::Left),
            Pane::Right => Some(Side::Right),
            Pane::Difference => None,
        };
        if let Some(side) = side {
            if let Some(text) = self.side_error(side) {
                return Some((text.to_owned(), |palette| palette.error_text));
            }
        }
        if self.outcome.is_some() {
            return None;
        }
        match &self.status {
            Status::Failed(text) => Some((text.clone(), |palette| palette.error_text)),
            Status::Running(step) => {
                Some((format!("{step}\u{2026}"), |palette| palette.progress_text))
            }
            Status::Idle | Status::Ready => Some(("Nothing is loaded.".to_owned(), |palette| {
                palette.progress_text
            })),
        }
    }

    fn draw_content(
        &mut self,
        ui: &mut egui::Ui,
        pane: Pane,
        rect: egui::Rect,
        content: &PaneContent,
    ) {
        let bounds = content.placement.bounds();
        let top_left = self
            .camera
            .screen_of([rect.min.x, rect.min.y], [bounds[0], bounds[1]]);
        let bottom_right = self
            .camera
            .screen_of([rect.min.x, rect.min.y], [bounds[2], bounds[3]]);
        let content_rect = egui::Rect::from_min_max(
            egui::pos2(top_left[0], top_left[1]),
            egui::pos2(bottom_right[0], bottom_right[1]),
        );
        self.image_rects.insert(pane, content_rect);
        let painter = ui.painter_at(rect);
        if self.checkerboard {
            let handle = self.checker_texture(ui.ctx());
            let square =
                (f32::from(u16::try_from(provisional::CHECKER_SIZE).unwrap_or(8))).max(1.0) * 2.0;
            let uv = egui::Rect::from_min_max(
                egui::pos2(0.0, 0.0),
                egui::pos2(
                    (content_rect.width() / square).max(0.0),
                    (content_rect.height() / square).max(0.0),
                ),
            );
            painter.image(handle.id(), content_rect, uv, egui::Color32::WHITE);
        }

        let buffer = [content.image.width(), content.image.height()];
        let max_side = ui.ctx().input(|input| input.max_texture_side);
        #[allow(clippy::cast_possible_truncation)]
        let max_side = u32::try_from(max_side).unwrap_or(2_048).max(1);
        let cache = self.caches.entry(pane).or_insert_with(|| PaneCache {
            uploader: Uploader::new(pane.label(), buffer, max_side),
            generation: self.generation,
            reduced: content.reduced,
            max_texture_side: max_side,
        });
        if cache.generation != self.generation
            || cache.reduced != content.reduced
            || cache.max_texture_side != max_side
            || cache.uploader.size() != buffer
        {
            *cache = PaneCache {
                uploader: Uploader::new(pane.label(), buffer, max_side),
                generation: self.generation,
                reduced: content.reduced,
                max_texture_side: max_side,
            };
        }

        let region = buffer_region(&self.camera, rect, content, buffer);
        let report = cache
            .uploader
            .upload_region(ui.ctx(), content.image.as_ref(), region);
        // A result larger than one frame's budget finishes over the frames that
        // follow, so the frame loop is asked to keep going until it is whole.
        if report.is_incomplete() {
            ui.ctx().request_repaint();
        }

        let scale = [
            (bounds[2] - bounds[0]) / f32_of(buffer[0]),
            (bounds[3] - bounds[1]) / f32_of(buffer[1]),
        ];
        let tiles: Vec<tiles::Tile> = cache
            .uploader
            .tiles()
            .iter()
            .copied()
            .filter(|tile| tile.intersects(region))
            .collect();
        for tile in tiles {
            let Some(handle) = cache.uploader.texture(tile) else {
                continue;
            };
            let min = self.camera.screen_of(
                [rect.min.x, rect.min.y],
                [
                    bounds[0] + f32_of(tile.x) * scale[0],
                    bounds[1] + f32_of(tile.y) * scale[1],
                ],
            );
            let max = self.camera.screen_of(
                [rect.min.x, rect.min.y],
                [
                    bounds[0] + f32_of(tile.x + tile.width) * scale[0],
                    bounds[1] + f32_of(tile.y + tile.height) * scale[1],
                ],
            );
            painter.image(
                handle.id(),
                egui::Rect::from_min_max(egui::pos2(min[0], min[1]), egui::pos2(max[0], max[1])),
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
    }

    fn handle_pane_input(
        &mut self,
        ui: &mut egui::Ui,
        pane: Pane,
        rect: egui::Rect,
        response: &egui::Response,
    ) {
        let pane_min = [rect.min.x, rect.min.y];
        if let Some(pointer) = response.hover_pos() {
            self.pointer = Some(self.camera.point_at(pane_min, [pointer.x, pointer.y]));
            let (scroll, zoom_modifier) = ui.input(|input| {
                (
                    [input.raw_scroll_delta.x, input.raw_scroll_delta.y],
                    input.modifiers.command,
                )
            });
            if scroll[1].abs() > f32::EPSILON || scroll[0].abs() > f32::EPSILON {
                if zoom_modifier {
                    let factor = (scroll[1] / 120.0).mul_add(0.25, 1.0).max(0.1);
                    let zoom = self.camera.zoom * factor;
                    self.end_fit();
                    self.camera
                        .zoom_about(pane_min, [pointer.x, pointer.y], zoom);
                } else {
                    let step = WHEEL_STEP / 120.0;
                    self.camera
                        .pan_by_screen([scroll[0] * step, scroll[1] * step]);
                }
            }
        }

        let offset_drag =
            pane == Pane::Difference && response.dragged_by(egui::PointerButton::Primary);
        let pan_drag = response.dragged_by(egui::PointerButton::Middle)
            || (response.dragged_by(egui::PointerButton::Primary) && !offset_drag);
        let delta = response.drag_delta();
        if pan_drag && delta != egui::Vec2::ZERO {
            self.camera.pan_by_screen([delta.x, delta.y]);
        }
        if offset_drag && delta != egui::Vec2::ZERO {
            let zoom = model::clamp_zoom(self.camera.zoom);
            self.offset_drag[0] += delta.x / zoom;
            self.offset_drag[1] += delta.y / zoom;
            #[allow(clippy::cast_possible_truncation)]
            let steps = [
                self.offset_drag[0].trunc() as i32,
                self.offset_drag[1].trunc() as i32,
            ];
            if steps != [0, 0] {
                self.offset_drag[0] -= f32_of_signed(steps[0]);
                self.offset_drag[1] -= f32_of_signed(steps[1]);
                self.nudge_offset(steps[0], steps[1]);
            }
        }
        if response.drag_stopped() {
            self.offset_drag = [0.0, 0.0];
        }
    }

    /// The crosshair, drawn in every pane at the same shared-space point.
    fn draw_crosshair(&self, ui: &egui::Ui, pane: Pane, rect: egui::Rect, palette: &Palette) {
        let Some(point) = self.pointer else {
            return;
        };
        let screen = self
            .camera
            .screen_of([rect.min.x, rect.min.y], [point[0], point[1]]);
        let at = egui::pos2(screen[0], screen[1]);
        if !rect.contains(at) {
            return;
        }
        let under_pointer = ui
            .ctx()
            .pointer_latest_pos()
            .is_some_and(|pos| rect.contains(pos));
        let (color, arm) = if under_pointer {
            (palette.crosshair, 9.0)
        } else {
            (palette.crosshair_echo, 5.0)
        };
        let painter = ui.painter_at(rect);
        let stroke = egui::Stroke::new(1.0, color);
        painter.line_segment(
            [at - egui::vec2(arm, 0.0), at + egui::vec2(arm, 0.0)],
            stroke,
        );
        painter.line_segment(
            [at - egui::vec2(0.0, arm), at + egui::vec2(0.0, arm)],
            stroke,
        );
        let _ = pane;
    }

    fn control_column(&mut self, ui: &mut egui::Ui, rect: egui::Rect, palette: &Palette) {
        ui.painter_at(rect)
            .rect_filled(rect, 0.0, palette.panel_background);
        let builder = egui::UiBuilder::new()
            .max_rect(rect.shrink(6.0))
            .layout(egui::Layout::top_down(egui::Align::LEFT));
        ui.scope_builder(builder, |ui| {
            self.control_lines(ui, palette);
        });
    }

    /// The controls beside the panes.
    ///
    /// The offset row is stacked under its own label and above its own button,
    /// because the label, two fields and a button on one line need more width
    /// than the column has.
    fn control_lines(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        ui.colored_label(palette.label_text, "Offset");
        let mut reported = Vec::new();
        ui.horizontal(|ui| {
            let mut apply = false;
            widgets::sized(ui, FIELD_WIDTH, |ui| {
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.offset_x_text)
                        .desired_width(FIELD_WIDTH - 10.0),
                );
                reported.push(("Offset x", response.rect));
                apply |= response.lost_focus();
            });
            widgets::sized(ui, FIELD_WIDTH, |ui| {
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.offset_y_text)
                        .desired_width(FIELD_WIDTH - 10.0),
                );
                reported.push(("Offset y", response.rect));
                apply |= response.lost_focus();
            });
            if apply {
                let x = self.offset_x_text.trim().parse::<i32>();
                let y = self.offset_y_text.trim().parse::<i32>();
                if let (Ok(x), Ok(y)) = (x, y) {
                    if self.settings.offset != Offset::new(x, y) {
                        self.settings.offset = Offset::new(x, y);
                        self.ask(Stage::Compare);
                    }
                } else {
                    self.sync_offset_fields();
                }
            }
        });
        let reset = ui.button("Reset");
        reported.push(("Offset reset", reset.rect));
        if reset.clicked() {
            self.reset_offset();
        }
        for (name, rect) in reported {
            self.reported.push(WidgetRect {
                name: name.to_owned(),
                rect,
                pane: None,
            });
        }
        ui.horizontal(|ui| {
            ui.colored_label(palette.label_text, "Zoom");
            ui.colored_label(
                palette.value_text,
                format!("{:.0}%", f64::from(self.camera.zoom) * 100.0),
            );
        });
        ui.colored_label(palette.label_text, "Pixel details");
        match self.pixel_details() {
            Some(pixel) => {
                ui.colored_label(
                    palette.value_text,
                    format!("At {}, {}", pixel.point[0], pixel.point[1]),
                );
                ui.colored_label(
                    palette.value_text,
                    format!("Left  {}", details::format_rgba(pixel.left)),
                );
                ui.colored_label(
                    palette.value_text,
                    format!("Right {}", details::format_rgba(pixel.right)),
                );
                ui.colored_label(
                    palette.value_text,
                    format!("Verdict {}", details::verdict_label(pixel.verdict)),
                );
            }
            None => {
                ui.colored_label(palette.progress_text, "Point at a pane");
            }
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        let line = |ui: &mut egui::Ui, side: Side, image: Option<&SideImage>| match image {
            Some(image) => {
                ui.colored_label(
                    palette.value_text,
                    format!(
                        "{}: {} {}",
                        side_label(side),
                        image.metadata.format.name(),
                        image.dimensions_label()
                    ),
                );
            }
            None => {
                ui.colored_label(palette.progress_text, format!("{}: -", side_label(side)));
            }
        };
        ui.horizontal_wrapped(|ui| {
            let left = self.left_image.clone();
            let right = self.right_image.clone();
            line(ui, Side::Left, left.as_ref());
            ui.separator();
            line(ui, Side::Right, right.as_ref());
        });
        if let Some(outcome) = self.outcome.as_ref() {
            let totals = outcome.totals;
            let ignore = self.settings.ignore_unimportant;
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(palette.value_text, status::verdict(totals, ignore));
                ui.separator();
                ui.colored_label(palette.value_text, status::counts(totals));
            });
            if let Some(note) = self.notice() {
                ui.colored_label(palette.notice_text, note);
            }
        }
        if let Status::Failed(text) = &self.status {
            ui.colored_label(palette.error_text, text.clone());
        }
        if self.show_metadata {
            if let Some(sources) = self.sources() {
                Self::metadata_lines(ui, &sources, palette);
            }
        }
    }

    fn metadata_lines(ui: &mut egui::Ui, sources: &Sources, palette: &Palette) {
        for (side, image) in [(Side::Left, &sources.left), (Side::Right, &sources.right)] {
            if let Some(note) = image.fidelity_note() {
                ui.colored_label(palette.notice_text, format!("{}: {note}", side_label(side)));
            }
            let bound = if image.metadata.count_is_partial {
                "at least "
            } else {
                ""
            };
            if let Some(frames) = image.metadata.frame_count {
                if frames > 1 || image.metadata.count_is_partial {
                    ui.colored_label(
                        palette.label_text,
                        format!(
                            "{}: {bound}{frames} frames, the first one is compared",
                            side_label(side)
                        ),
                    );
                }
            }
            if let Some(images) = image
                .metadata
                .image_count
                .filter(|count| *count > 1 || image.metadata.count_is_partial)
            {
                let note = match &image.metadata.format {
                    ca_image::decode::SourceFormat::Tiff => {
                        format!(
                            "{}: {bound}{images} pages, the first one is compared",
                            side_label(side)
                        )
                    }
                    ca_image::decode::SourceFormat::Ico => format!(
                        "{}: {bound}{images} icon entries, the largest one is compared",
                        side_label(side)
                    ),
                    _ => format!(
                        "{}: {bound}{images} images, the first one is compared",
                        side_label(side)
                    ),
                };
                ui.colored_label(palette.label_text, note);
            }
        }
    }
}

/// Where the three buffers of one comparison sit in the shared space.
fn layout_of(outcome: &Outcome) -> Layout {
    Layout::new(
        &outcome.left,
        &outcome.right,
        outcome.offset,
        outcome.origin,
    )
    .with_result(&outcome.result, outcome.origin)
}

/// The smallest rectangle of a layout, which is the one a fit works against.
fn smallest(rects: &HashMap<Pane, egui::Rect>) -> Option<egui::Rect> {
    rects.values().copied().min_by(|a, b| {
        (a.width() * a.height())
            .partial_cmp(&(b.width() * b.height()))
            .unwrap_or(std::cmp::Ordering::Equal)
    })
}

/// Equal columns for the shown panes, in the room left of the control column.
///
/// An empty map means the room is too small to divide, in which case no pane is
/// drawn rather than a pane being drawn outside the body.
fn pane_columns(
    room: egui::Rect,
    controls_width: f32,
    shown: &[Pane],
) -> HashMap<Pane, egui::Rect> {
    let mut rects = HashMap::new();
    let height = room.height();
    let width = room.width() - controls_width;
    if shown.is_empty() || height < MIN_PANE_SIDE {
        return rects;
    }
    #[allow(clippy::cast_precision_loss)]
    let column = width / shown.len() as f32;
    if column < MIN_PANE_SIDE {
        return rects;
    }
    for (index, pane) in shown.iter().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let min = egui::pos2(
            room.min.x + controls_width + column * index as f32,
            room.min.y,
        );
        rects.insert(
            *pane,
            egui::Rect::from_min_size(min, egui::vec2(column, height)),
        );
    }
    rects
}

/// The buffer rectangle a pane exposes, in the buffer's own pixels.
fn buffer_region(
    camera: &Camera,
    rect: egui::Rect,
    content: &PaneContent,
    buffer: [u32; 2],
) -> [u32; 4] {
    let shared = model::visible_pixels(camera, [rect.width(), rect.height()], content.placement);
    // The buffer may be a reduced copy of the content, so the visible
    // rectangle is expressed in content pixels and converted here.
    let scale = [
        f32_of(buffer[0]) / f32_of(content.placement.size[0]),
        f32_of(buffer[1]) / f32_of(content.placement.size[1]),
    ];
    let low = |value: u32, factor: f32| -> u32 {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let scaled = (f32_of(value) * factor).floor().max(0.0) as u32;
        scaled
    };
    let high = |value: u32, factor: f32, limit: u32| -> u32 {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let scaled = (f32_of(value) * factor).ceil().max(0.0) as u32;
        scaled.min(limit)
    };
    [
        low(shared[0], scale[0]).min(buffer[0]),
        low(shared[1], scale[1]).min(buffer[1]),
        high(shared[2], scale[0], buffer[0]),
        high(shared[3], scale[1], buffer[1]),
    ]
}

#[allow(clippy::cast_precision_loss)]
fn f32_of(value: u32) -> f32 {
    value as f32
}

#[allow(clippy::cast_precision_loss)]
fn f32_of_signed(value: i32) -> f32 {
    value as f32
}

impl ca_ui::view::ViewFactory for PictureView {
    fn create(left: PathBuf, right: PathBuf, context: &ViewContext, instance: u64) -> Self {
        Self::new(left, right, context, instance)
    }
}

/// What one side carries, as a report reads it.
fn picture_side(image: Option<&SideImage>) -> ca_ui::report::PictureSide {
    let Some(image) = image else {
        return ca_ui::report::PictureSide::default();
    };
    ca_ui::report::PictureSide {
        width: image.metadata.width,
        height: image.metadata.height,
        format: format!("{:?}", image.metadata.format),
        metadata: Vec::new(),
        ..ca_ui::report::PictureSide::default()
    }
    .with_fidelity(image.metadata.fidelity)
}
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::{mode_label, side_index, side_label, PictureView, SideImage, Sources, MODES};
    use ca_image::compare::Side;
    use ca_image::decode::{ColorKind, Fidelity, Metadata, Orientation, SourceFormat};
    use ca_ui::testing::context;
    use ca_ui::view::SessionView;
    use std::path::PathBuf;

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

    fn source_image(format: SourceFormat, image_count: Option<u32>) -> SideImage {
        SideImage {
            image: std::sync::Arc::new(ca_image::RgbaImage::filled(1, 1, [0, 0, 0, 255]).unwrap()),
            metadata: Metadata {
                format,
                width: 1,
                height: 1,
                color: ColorKind::Rgba,
                bits_per_pixel: 32,
                frame_count: None,
                image_count,
                count_is_partial: false,
                orientation: Orientation::Identity,
                fidelity: Fidelity::default(),
            },
        }
    }

    #[test]
    fn metadata_shows_the_icon_entry_and_tiff_page_counts() {
        let sources = Sources {
            left: source_image(SourceFormat::Ico, Some(2)),
            right: source_image(SourceFormat::Tiff, Some(3)),
        };
        let context = egui::Context::default();
        let output = context.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                PictureView::metadata_lines(ui, &sources, &ca_ui::theme::picture::LIGHT);
            });
        });
        let text = drawn_text(&output);

        assert!(
            text.iter()
                .any(|line| line == "Left: 2 icon entries, the largest one is compared"),
            "the ICO entry notice was not drawn: {text:?}"
        );
        assert!(
            text.iter()
                .any(|line| line == "Right: 3 pages, the first one is compared"),
            "the TIFF page notice was not drawn: {text:?}"
        );
    }

    #[test]
    fn a_terminal_comparison_is_ready_before_its_sender_disconnects() {
        use ca_ui::worker::Job;
        use std::sync::mpsc;
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left.png");
        let right = dir.path().join("right.png");
        for path in [&left, &right] {
            image::RgbaImage::from_pixel(2, 2, image::Rgba([1, 2, 3, 255]))
                .save(path)
                .unwrap();
        }
        let mut view = PictureView::new(left, right, &context(), 1);
        assert!(ca_ui::testing::wait_until(Duration::from_secs(20), || {
            view.tick();
            view.is_ready()
        }));
        let outcome = view.outcome.take().unwrap();
        let (sent, received) = mpsc::channel();
        let (release, held) = mpsc::channel::<()>();
        view.job = Some(Job::spawn(move |emitter, _| {
            emitter.send(super::jobs::Message::Ready(outcome));
            sent.send(()).unwrap();
            // Hold the sender after Ready, making the scheduling race deterministic.
            let _ = held.recv_timeout(Duration::from_secs(20));
        }));
        received.recv_timeout(Duration::from_secs(20)).unwrap();
        view.drain_job();
        let ready = view.is_ready();
        release.send(()).unwrap();
        assert!(ready, "Ready must finish the comparison run");
    }

    #[test]
    fn the_title_names_both_files() {
        let view = PictureView::new(
            PathBuf::from("a/left.png"),
            PathBuf::from("b/right.png"),
            &context(),
            1,
        );
        assert_eq!(view.title(), "left.png - right.png");
    }

    #[test]
    fn every_mode_has_a_distinct_name() {
        for (index, mode) in MODES.iter().enumerate() {
            assert!(!mode_label(*mode).is_empty());
            for other in MODES.iter().skip(index + 1) {
                assert_ne!(mode_label(*mode), mode_label(*other));
            }
        }
    }

    #[test]
    fn failed_comparison_drops_the_previous_result_and_texture_caches() {
        let image =
            std::sync::Arc::new(ca_image::RgbaImage::filled(2, 2, [20, 30, 40, 255]).unwrap());
        let compared = ca_image::compare::compare(
            &image,
            &image,
            &ca_image::CompareOptions::default(),
            &ca_image::NeverCancel,
        )
        .unwrap();
        let preview =
            std::sync::Arc::new(ca_image::RgbaImage::filled(1, 1, [20, 30, 40, 255]).unwrap());
        let mut view = PictureView::new(
            PathBuf::from("left.png"),
            PathBuf::from("right.png"),
            &context(),
            9,
        );
        view.outcome = Some(Box::new(crate::jobs::Outcome {
            left: std::sync::Arc::clone(&image),
            right: image,
            left_preview: Some(preview),
            right_preview: None,
            result: std::sync::Arc::new(compared.image),
            preview: None,
            mask: std::sync::Arc::new(compared.mask),
            totals: compared.totals,
            tolerance: 0,
            origin: compared.origin,
            offset: ca_image::compare::Offset::zero(),
        }));
        view.caches.insert(
            super::Pane::Left,
            super::PaneCache {
                uploader: crate::tiles::Uploader::new("left", [2, 2], 16),
                generation: view.generation,
                reduced: false,
                max_texture_side: 16,
            },
        );
        view.camera.zoom = 0.25;
        assert!(view.content(super::Pane::Left).unwrap().reduced);

        view.apply(crate::jobs::Message::Failed("outside bounds".to_owned()));

        assert!(view.outcome.is_none());
        assert!(view.caches.is_empty());
        assert!(!view.is_ready());
        assert_eq!(view.failure(), Some("outside bounds"));
    }

    #[test]
    fn the_two_sides_index_and_name_differently() {
        assert_ne!(side_index(Side::Left), side_index(Side::Right));
        assert_ne!(side_label(Side::Left), side_label(Side::Right));
    }

    #[test]
    fn the_declared_commands_are_the_ones_the_view_runs() {
        let view = PictureView::new(
            PathBuf::from("left.png"),
            PathBuf::from("right.png"),
            &context(),
            2,
        );
        let declared = view.commands();
        assert_eq!(declared.len(), super::HANDLED.len());
        for state in declared {
            assert!(super::HANDLED.contains(&state.command));
        }
        assert!(view.accepts(super::Command::OpenFile));
        assert!(!view.is_ready());
        assert!(!view.wants_close());
    }

    #[test]
    fn a_pending_comparison_disables_the_report_command() {
        let mut view = PictureView::new(
            PathBuf::from("left.png"),
            PathBuf::from("right.png"),
            &context(),
            3,
        );
        view.scheduler.clear();
        view.status = super::Status::Ready;
        view.outcome = Some(Box::new(empty_outcome(12)));
        assert!(view.is_ready());

        view.set_tolerance(13);

        assert!(!view.is_ready());
        assert!(view
            .commands()
            .into_iter()
            .find(|state| state.command == super::Command::CompareReport)
            .is_some_and(|state| !state.enabled));
    }

    #[test]
    fn a_picture_report_uses_the_tolerance_that_produced_its_counts() {
        let mut view = PictureView::new(
            PathBuf::from("left.png"),
            PathBuf::from("right.png"),
            &context(),
            4,
        );
        view.settings.tolerance = 99;
        view.outcome = Some(Box::new(empty_outcome(12)));

        let (_, payload) = view.report_payload();
        let ca_ui::report::Payload::Picture(facts) = payload else {
            panic!("picture report returned another payload kind");
        };
        assert_eq!(facts.tolerance, 12);
    }

    fn empty_outcome(tolerance: u8) -> crate::jobs::Outcome {
        let image = std::sync::Arc::new(ca_image::RgbaImage::filled(1, 1, [0, 0, 0, 255]).unwrap());
        let compared = ca_image::compare::compare(
            &image,
            &image,
            &ca_image::CompareOptions::default(),
            &ca_image::NeverCancel,
        )
        .unwrap();
        crate::jobs::Outcome {
            left: std::sync::Arc::clone(&image),
            right: image,
            left_preview: None,
            right_preview: None,
            result: std::sync::Arc::new(compared.image),
            preview: None,
            mask: std::sync::Arc::new(compared.mask),
            totals: compared.totals,
            tolerance,
            origin: compared.origin,
            offset: ca_image::compare::Offset::zero(),
        }
    }
}
