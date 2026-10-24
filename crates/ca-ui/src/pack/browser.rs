//! The pack browser: a dialog that lists the download packs of the release
//! this build belongs to and installs or removes them.
//!
//! The frame thread only shows state. The list, the preview images, every
//! install and every removal run on worker threads that report through a
//! channel.

use super::index::{IndexEntry, PackIndex};
use super::remote::{self, IndexFailure, InstallStage, PackServices};
use std::collections::BTreeMap;
use std::io::Cursor;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

/// Text of the button that opens the browser.
pub const OPEN_LABEL: &str = "Get more packs";
/// Text of the button that closes the browser.
pub const CLOSE_LABEL: &str = "Close";
/// Text of the button that opens the release page.
pub const PAGE_LABEL: &str = "Open release page";
/// Shown when the release has no pack list.
pub const NO_LIST_TEXT: &str = "No pack list exists for this build.";
/// Shown when the pack list cannot be loaded.
pub const LIST_FAILED_TEXT: &str = "The pack list could not be loaded.";
/// Shown on a row when the zip cannot be downloaded.
pub const DOWNLOAD_FAILED_TEXT: &str = "The pack could not be downloaded.";
/// Shown on the button that opens the browser when a policy turns it off.
pub const POLICY_TEXT: &str = "Turned off by your administrator.";

/// Longest side of a preview image accepted, in pixels.
const MAX_PREVIEW_SIDE: u32 = 1024;
/// Allocation cap for decoding a preview image, in bytes.
const MAX_PREVIEW_ALLOC: u64 = 8 << 20;
/// Width the preview image is drawn at.
const PREVIEW_WIDTH: f32 = 132.0;
/// Height the preview image is drawn at.
const PREVIEW_HEIGHT: f32 = 68.0;
/// Height of one row with its separator: the preview, the item spacing above
/// and below it, and the separator line.
const ROW_HEIGHT: f32 = PREVIEW_HEIGHT + 12.0;
/// Rows the window shows without a scroll bar at its default size.
const DEFAULT_ROWS: f32 = 10.0;
/// Height of the title bar, the frame margins and the Close button.
const CHROME_HEIGHT: f32 = 100.0;
/// Space kept free below the default window on a short screen.
const SCREEN_MARGIN: f32 = 40.0;
/// Width of the window at its default size.
const DEFAULT_WIDTH: f32 = 560.0;

/// Height the window opens at: room for the default row count, or the screen
/// less a margin when the screen is shorter. A smaller window scrolls the list.
fn default_height(screen_height: f32) -> f32 {
    (ROW_HEIGHT * DEFAULT_ROWS + CHROME_HEIGHT).min((screen_height - SCREEN_MARGIN).max(120.0))
}

/// The accessible name of the button of the pack `name`.
#[must_use]
pub fn button_label(action: &str, name: &str) -> String {
    format!("{action} {name}")
}

enum Message {
    Listed(Result<(PackIndex, BTreeMap<String, String>), IndexFailure>),
    Preview {
        id: String,
        image: Result<egui::ColorImage, String>,
    },
    Finished {
        id: String,
        outcome: Outcome,
        installed: BTreeMap<String, String>,
    },
}

enum Outcome {
    Installed,
    Removed,
    Failed { text: String, release_page: bool },
}

enum Preview {
    Waiting,
    Ready(egui::TextureHandle),
    Missing,
}

struct Row {
    entry: IndexEntry,
    preview: Preview,
    working: bool,
    note: Option<(String, bool)>,
}

enum State {
    Closed,
    Loading,
    NoList,
    Failed(String),
    Ready(Vec<Row>),
}

impl Drop for PackBrowser {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// The pack browser of one window.
pub struct PackBrowser {
    id: egui::Id,
    services: Option<Arc<PackServices>>,
    state: State,
    installed: BTreeMap<String, String>,
    sender: Option<Sender<Message>>,
    receiver: Option<Receiver<Message>>,
    cancel: Arc<AtomicBool>,
    page_request: Option<String>,
}

impl PackBrowser {
    /// A closed browser.
    #[must_use]
    pub fn new(instance: u64) -> Self {
        Self {
            id: egui::Id::new(("pack-browser", instance)),
            services: None,
            state: State::Closed,
            installed: BTreeMap::new(),
            sender: None,
            receiver: None,
            cancel: Arc::new(AtomicBool::new(false)),
            page_request: None,
        }
    }

    /// Say where the packs come from. Without services the browser cannot open.
    pub fn set_services(&mut self, services: Option<Arc<PackServices>>) {
        self.services = services;
    }

    /// Whether the browser can open.
    #[must_use]
    pub fn available(&self) -> bool {
        self.services.is_some()
    }

    /// True while the dialog is on screen.
    #[must_use]
    pub fn is_open(&self) -> bool {
        !matches!(self.state, State::Closed)
    }

    /// The release page the person asked to open, once.
    pub fn take_page_request(&mut self) -> Option<String> {
        self.page_request.take()
    }

    /// Open the dialog and fetch the pack list.
    pub fn open(&mut self, ctx: &egui::Context) {
        let Some(services) = self.services.clone() else {
            return;
        };
        self.close();
        self.cancel = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = channel();
        self.sender = Some(sender.clone());
        self.receiver = Some(receiver);
        self.state = State::Loading;
        let cancel = Arc::clone(&self.cancel);
        let repaint = ctx.clone();
        let started = std::thread::Builder::new()
            .name("theme-pack-list".to_owned())
            .spawn(move || list_and_preview(&services, &sender, &cancel, &repaint));
        if started.is_err() {
            self.state = State::Failed("The worker could not start.".to_owned());
        }
    }

    /// Close the dialog and stop the transfers in flight.
    pub fn close(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.state = State::Closed;
        self.sender = None;
        self.receiver = None;
    }

    /// Take in what the workers reported.
    pub fn poll(&mut self, ctx: &egui::Context) {
        let mut messages = Vec::new();
        if let Some(receiver) = &self.receiver {
            while let Ok(message) = receiver.try_recv() {
                messages.push(message);
            }
        }
        for message in messages {
            self.apply(ctx, message);
        }
    }

    fn apply(&mut self, ctx: &egui::Context, message: Message) {
        match message {
            Message::Listed(Ok((index, installed))) => {
                self.installed = installed;
                self.state = State::Ready(
                    index
                        .packs
                        .into_iter()
                        .map(|entry| Row {
                            entry,
                            preview: Preview::Waiting,
                            working: false,
                            note: None,
                        })
                        .collect(),
                );
            }
            Message::Listed(Err(IndexFailure::NoList)) => self.state = State::NoList,
            Message::Listed(Err(IndexFailure::Failed(reason))) => {
                self.state = State::Failed(reason);
            }
            Message::Preview { id, image } => {
                if let State::Ready(rows) = &mut self.state {
                    if let Some(row) = rows.iter_mut().find(|row| row.entry.id == id) {
                        row.preview = match image {
                            Ok(image) => Preview::Ready(ctx.load_texture(
                                format!("pack-preview/{id}"),
                                image,
                                egui::TextureOptions::LINEAR,
                            )),
                            Err(_) => Preview::Missing,
                        };
                    }
                }
            }
            Message::Finished {
                id,
                outcome,
                installed,
            } => {
                self.installed = installed;
                if let State::Ready(rows) = &mut self.state {
                    if let Some(row) = rows.iter_mut().find(|row| row.entry.id == id) {
                        row.working = false;
                        row.note = match outcome {
                            Outcome::Installed | Outcome::Removed => None,
                            Outcome::Failed { text, release_page } => Some((text, release_page)),
                        };
                    }
                }
                super::request_rescan(ctx);
            }
        }
    }

    /// Draw the dialog as a window. Does nothing while closed.
    pub fn show(&mut self, ctx: &egui::Context) {
        self.poll(ctx);
        if !self.is_open() {
            return;
        }
        let mut open = true;
        let mut close = false;
        egui::Window::new("Theme packs")
            .id(self.id)
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(DEFAULT_WIDTH)
            .default_height(default_height(ctx.screen_rect().height()))
            .show(ctx, |ui| close = self.body(ui));
        if !open || close {
            self.close();
        }
    }

    /// Draw the content of the dialog into `ui`. Returns true when the person
    /// pressed Close.
    pub fn body(&mut self, ui: &mut egui::Ui) -> bool {
        self.poll(ui.ctx());
        let mut start: Option<(IndexEntry, bool)> = None;
        let mut want_page = false;
        match &mut self.state {
            State::Closed => {}
            State::Loading => {
                ui.label("Loading the pack list.");
            }
            State::NoList => {
                crate::widgets::wrapped_text(ui, NO_LIST_TEXT);
            }
            State::Failed(reason) => {
                crate::widgets::wrapped_text(ui, LIST_FAILED_TEXT);
                crate::widgets::wrapped_text(ui, reason);
                want_page = ui.button(PAGE_LABEL).clicked();
            }
            State::Ready(rows) => {
                if rows.is_empty() {
                    crate::widgets::wrapped_text(ui, "This release lists no packs.");
                }
                let installed = &self.installed;
                egui::ScrollArea::vertical()
                    .max_height((ui.available_height() - 40.0).max(120.0))
                    .show(ui, |ui| {
                        for row in rows.iter_mut() {
                            draw_row(
                                ui,
                                row,
                                installed.contains_key(&row.entry.id),
                                &mut start,
                                &mut want_page,
                            );
                            ui.separator();
                        }
                    });
            }
        }
        let close = ui.button(CLOSE_LABEL).clicked();
        if want_page {
            self.page_request = self.services.as_ref().map(|s| s.locations.page.clone());
        }
        if let Some((entry, remove)) = start {
            self.start_work(ui.ctx(), &entry, remove);
        }
        close
    }

    fn start_work(&mut self, ctx: &egui::Context, entry: &IndexEntry, remove: bool) {
        let (Some(services), Some(sender)) = (self.services.clone(), self.sender.clone()) else {
            return;
        };
        if let State::Ready(rows) = &mut self.state {
            if let Some(row) = rows.iter_mut().find(|row| row.entry.id == entry.id) {
                row.working = true;
                row.note = None;
            }
        }
        let entry = entry.clone();
        let cancel = Arc::clone(&self.cancel);
        let repaint = ctx.clone();
        let id = entry.id.clone();
        let failed_sender = sender.clone();
        let started = std::thread::Builder::new()
            .name("theme-pack-work".to_owned())
            .spawn(move || {
                let outcome = if remove {
                    match remote::remove(&services.directory, &entry.id) {
                        Ok(()) => Outcome::Removed,
                        Err(text) => Outcome::Failed {
                            text,
                            release_page: false,
                        },
                    }
                } else {
                    match remote::install(&services, &entry, &cancel) {
                        Ok(()) => Outcome::Installed,
                        Err(error) if error.stage == InstallStage::Download => Outcome::Failed {
                            text: format!("{DOWNLOAD_FAILED_TEXT} {}", capitalized(&error.message)),
                            release_page: true,
                        },
                        Err(error) => Outcome::Failed {
                            text: error.message,
                            release_page: false,
                        },
                    }
                };
                let installed = remote::installed_by_browser(&services.directory);
                let _ = sender.send(Message::Finished {
                    id: entry.id,
                    outcome,
                    installed,
                });
                repaint.request_repaint();
            });
        if started.is_err() {
            let _ = failed_sender.send(Message::Finished {
                id,
                outcome: Outcome::Failed {
                    text: "The worker could not start.".to_owned(),
                    release_page: false,
                },
                installed: self.installed.clone(),
            });
        }
    }
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    let mut out: String = chars
        .next()
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default();
    out.push_str(chars.as_str());
    if !out.ends_with('.') && !out.is_empty() {
        out.push('.');
    }
    out
}

fn mode_text(modes: &[String]) -> &'static str {
    let light = modes.iter().any(|mode| mode == "light");
    let dark = modes.iter().any(|mode| mode == "dark");
    match (light, dark) {
        (true, true) => "Light and dark",
        (true, false) => "Light",
        _ => "Dark",
    }
}

fn draw_row(
    ui: &mut egui::Ui,
    row: &Row,
    installed: bool,
    start: &mut Option<(IndexEntry, bool)>,
    want_page: &mut bool,
) {
    ui.horizontal(|ui| {
        let size = egui::vec2(PREVIEW_WIDTH, PREVIEW_HEIGHT);
        match &row.preview {
            Preview::Ready(texture) => {
                ui.add(
                    egui::Image::from_texture(egui::load::SizedTexture::from_handle(texture))
                        .fit_to_exact_size(size)
                        .alt_text(format!("Preview of {}", row.entry.name)),
                );
            }
            Preview::Waiting => {
                ui.add_sized(size, egui::Spinner::new());
            }
            Preview::Missing => {
                ui.add_sized(size, egui::Label::new("No preview"));
            }
        }
        ui.vertical(|ui| {
            ui.label(egui::RichText::new(row.entry.name.as_str()).strong());
            ui.weak(format!(
                "{}, {} KB",
                mode_text(&row.entry.modes),
                row.entry.size.div_ceil(1024)
            ));
            if let Some((text, release_page)) = &row.note {
                crate::widgets::wrapped_text(ui, text);
                if *release_page && ui.button(PAGE_LABEL).clicked() {
                    *want_page = true;
                }
            }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let action = if installed { "Remove" } else { "Install" };
            let label = button_label(action, &row.entry.name);
            let response = ui.add_enabled(!row.working, egui::Button::new(action));
            response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, !row.working, &label)
            });
            if response.clicked() {
                *start = Some((row.entry.clone(), installed));
            }
        });
    });
}

/// The worker of one open: clean up, fetch the list, then fetch the previews.
fn list_and_preview(
    services: &PackServices,
    sender: &Sender<Message>,
    cancel: &Arc<AtomicBool>,
    repaint: &egui::Context,
) {
    remote::clean_leftovers(&services.directory);
    let result = remote::fetch_index(
        services.transport.as_ref(),
        &services.locations,
        &remote::builtin_ids(),
        cancel,
    );
    let entries = result.as_ref().map(|index| index.packs.clone()).ok();
    let installed = remote::installed_by_browser(&services.directory);
    if sender
        .send(Message::Listed(result.map(|index| (index, installed))))
        .is_err()
    {
        return;
    }
    repaint.request_repaint();
    for entry in entries.unwrap_or_default() {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let image = remote::fetch_preview(
            services.transport.as_ref(),
            &services.locations,
            &entry,
            cancel,
        )
        .and_then(|bytes| decode_preview(&bytes));
        if sender
            .send(Message::Preview {
                id: entry.id,
                image,
            })
            .is_err()
        {
            return;
        }
        repaint.request_repaint();
    }
}

/// Decode a preview PNG under a side limit and an allocation cap.
fn decode_preview(bytes: &[u8]) -> Result<egui::ColorImage, String> {
    let reader = || image::ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::Png);
    let (width, height) = reader()
        .into_dimensions()
        .map_err(|_| "The preview is not a PNG image.".to_owned())?;
    if width == 0 || height == 0 || width > MAX_PREVIEW_SIDE || height > MAX_PREVIEW_SIDE {
        return Err("The preview has a size outside the limit.".to_owned());
    }
    let mut limits = image::Limits::no_limits();
    limits.max_image_width = Some(MAX_PREVIEW_SIDE);
    limits.max_image_height = Some(MAX_PREVIEW_SIDE);
    limits.max_alloc = Some(MAX_PREVIEW_ALLOC);
    let mut decoder = reader();
    decoder.limits(limits);
    let rgba = decoder
        .decode()
        .map_err(|_| "The preview could not be decoded.".to_owned())?
        .to_rgba8();
    Ok(egui::ColorImage::from_rgba_unmultiplied(
        [width as usize, height as usize],
        rgba.as_raw(),
    ))
}
