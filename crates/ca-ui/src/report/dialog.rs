//! The report dialog, shared by every view.
//!
//! The controls are laid out in a column of a fixed width, so the dialog reads
//! the same in a narrow window as in a wide one and no control leaves the
//! window. Nothing here touches the disk: the picker and the write both run as
//! jobs and the dialog reads what they post.

use super::payload::Payload;
use super::plan::{ReportPlan, ReportSettings, Target, DIALOG_WIDTH, PRINTER_UNAVAILABLE};
use super::{spawn, ReportMessage};
use crate::dialog::{DialogMessage, Pick};
use crate::launch::{LaunchMessage, Spawner, SystemSpawner};
use crate::widgets;
use crate::worker::Job;
use ca_report::options::ReportMeta;
use std::path::PathBuf;
use std::sync::Arc;

/// What the dialog asks the view for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportAction {
    /// Nothing this frame.
    None,
    /// Build the payload and start the write.
    Write,
    /// The dialog is finished.
    Close,
    /// Put this text on the clipboard.
    Copy(String),
}

/// The report dialog and the jobs it owns.
pub struct ReportDialog {
    id: egui::Id,
    settings: ReportSettings,
    notify: Arc<dyn Fn() + Send + Sync>,
    spawner: Arc<dyn Spawner>,
    picker: Option<Job<DialogMessage>>,
    job: Option<Job<ReportMessage>>,
    launch: Option<Job<LaunchMessage>>,
    progress: u64,
    status: Option<String>,
    written: Option<PathBuf>,
}

impl ReportDialog {
    /// A dialog over `settings`.
    #[must_use]
    pub fn new(
        id: egui::Id,
        settings: ReportSettings,
        notify: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self {
            id,
            settings,
            notify,
            spawner: Arc::new(SystemSpawner),
            picker: None,
            job: None,
            launch: None,
            progress: 0,
            status: None,
            written: None,
        }
    }

    /// The same dialog with a stated spawner, so a test starts no program.
    #[must_use]
    pub fn with_spawner(mut self, spawner: Arc<dyn Spawner>) -> Self {
        self.spawner = spawner;
        self
    }

    /// What the dialog collected.
    #[must_use]
    pub const fn settings(&self) -> &ReportSettings {
        &self.settings
    }

    /// What the dialog collected, for a caller that seeds it.
    pub fn settings_mut(&mut self) -> &mut ReportSettings {
        &mut self.settings
    }

    /// True while a report is being written.
    #[must_use]
    pub const fn is_running(&self) -> bool {
        self.job.is_some()
    }

    /// What the last run reported, where it reported anything.
    #[must_use]
    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    /// Where the last report was written.
    #[must_use]
    pub fn written(&self) -> Option<&PathBuf> {
        self.written.as_ref()
    }

    /// Start the write of `payload`.
    ///
    /// Replacing the handle raises the older run's flag, so a second request
    /// supersedes the first rather than racing it.
    pub fn start(&mut self, meta: ReportMeta, payload: Payload, bytes_per_row: u32) {
        self.progress = 0;
        self.written = None;
        self.status = Some("Writing the report".to_owned());
        self.job = Some(spawn(
            ReportPlan {
                settings: self.settings.clone(),
                meta,
                payload,
                bytes_per_row,
            },
            Arc::clone(&self.notify),
        ));
    }

    /// Track `job` as the running write, for a caller that spawned it itself.
    pub fn adopt(&mut self, job: Job<ReportMessage>) {
        self.progress = 0;
        self.written = None;
        self.status = Some("Writing the report".to_owned());
        self.job = Some(job);
    }

    /// Raise the flag of whatever is running.
    pub fn cancel(&mut self) {
        if self.job.take().is_some() {
            self.status = Some("The report was stopped.".to_owned());
        }
    }

    /// Take whatever the jobs posted.
    pub fn tick(&mut self) -> ReportAction {
        let mut action = ReportAction::None;
        if let Some(picker) = &mut self.picker {
            for message in picker.drain() {
                match message {
                    DialogMessage::Chosen(path) => {
                        self.settings.path = path.display().to_string();
                    }
                    DialogMessage::Failed(reason) => self.status = Some(reason),
                    DialogMessage::Dismissed => {}
                }
            }
            if picker.is_finished() {
                self.picker = None;
            }
        }
        let mut finished = false;
        if let Some(job) = &mut self.job {
            for message in job.drain() {
                match message {
                    ReportMessage::Progress { bytes } => self.progress = bytes,
                    ReportMessage::Written { path, bytes } => {
                        self.status = Some(format!("{bytes} bytes written to {}", path.display()));
                        self.written = Some(path);
                        finished = true;
                    }
                    ReportMessage::Copied { text } => {
                        self.status = Some(format!("{} characters copied.", text.len()));
                        action = ReportAction::Copy(text);
                        finished = true;
                    }
                    ReportMessage::Cancelled => {
                        self.status = Some("The report was stopped.".to_owned());
                        finished = true;
                    }
                    ReportMessage::Failed { reason } => {
                        self.status = Some(reason);
                        finished = true;
                    }
                }
            }
            if job.is_finished() {
                finished = true;
            }
        }
        if finished {
            self.job = None;
            if self.settings.open_after_saving {
                if let Some(path) = self.written.clone() {
                    self.launch = Some(crate::launch::open_with_system(
                        &path,
                        Arc::clone(&self.spawner),
                        Arc::clone(&self.notify),
                    ));
                }
            }
        }
        if let Some(launch) = &mut self.launch {
            for message in launch.drain() {
                if let LaunchMessage::Failed { reason } = message {
                    self.status = Some(reason);
                }
            }
            if launch.is_finished() {
                self.launch = None;
            }
        }
        action
    }

    /// Draw one frame of the dialog.
    #[allow(clippy::too_many_lines)]
    pub fn ui(&mut self, ui: &mut egui::Ui) -> ReportAction {
        let mut action = self.tick();
        let width = ui.available_width().min(DIALOG_WIDTH);
        let mut write = false;
        let mut close = false;
        ui.allocate_ui(egui::vec2(width, ui.available_height()), |ui| {
            ui.set_width(width);
            ui.heading(self.settings.kind.title());
            ui.separator();

            ui.label("Layout");
            for (index, choice) in self.settings.kind.layouts().iter().enumerate() {
                if ui
                    .radio(self.settings.layout == index, choice.label)
                    .clicked()
                {
                    self.settings.layout = index;
                }
            }
            ui.separator();

            let filters = self.settings.kind.filters();
            if !filters.is_empty() {
                ui.label("Include");
                let selected = filters
                    .get(self.settings.filter)
                    .map_or("Every row", |held| held.label);
                egui::ComboBox::from_id_salt(self.id.with("filter"))
                    .width(width * 0.6)
                    .selected_text(selected)
                    .show_ui(ui, |ui| {
                        for (index, choice) in filters.iter().enumerate() {
                            ui.selectable_value(&mut self.settings.filter, index, choice.label);
                        }
                    });
                if self.settings.shows_context_lines() {
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Lines of context");
                        ui.add(
                            egui::DragValue::new(&mut self.settings.context_lines).range(0..=50),
                        );
                    });
                }
            }
            if self.settings.kind.has_importance() {
                ui.checkbox(
                    &mut self.settings.ignore_unimportant,
                    "Treat an unimportant difference as a match",
                );
            }
            if self.settings.kind.has_line_numbers() {
                ui.checkbox(&mut self.settings.line_numbers, "Write line numbers");
            }
            if self.settings.shows_patch_format() {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Patch format");
                    for (index, label) in ["Normal", "Context", "Unified"].iter().enumerate() {
                        if ui
                            .selectable_label(self.settings.patch == index, *label)
                            .clicked()
                        {
                            self.settings.patch = index;
                        }
                    }
                });
            }
            ui.separator();

            ui.label("Write to");
            for target in [
                Target::File,
                Target::Clipboard,
                Target::Printer,
                Target::PrintPreview,
            ] {
                let response = ui.add_enabled(
                    target.is_available(),
                    egui::RadioButton::new(self.settings.target == target, target.label()),
                );
                if target.is_available() {
                    if response.clicked() {
                        self.settings.target = target;
                    }
                } else {
                    response.on_hover_text(PRINTER_UNAVAILABLE);
                }
            }
            if !self.settings.layout_choice().fixed_document {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Document");
                    if ui.selectable_label(self.settings.html, "HTML").clicked() {
                        self.settings.html = true;
                    }
                    if ui
                        .selectable_label(!self.settings.html, "Plain text")
                        .clicked()
                    {
                        self.settings.html = false;
                    }
                });
            }
            if self.settings.target == Target::File {
                let hint = format!("report.{}", self.settings.extension());
                ui.horizontal_wrapped(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.settings.path)
                            .desired_width(width * 0.6)
                            .hint_text(hint),
                    );
                    if crate::widgets::inline_button(ui, "Browse", crate::icons::Icon::Browse)
                        .clicked()
                    {
                        self.picker = Some(crate::dialog::spawn(
                            Pick::SaveFile,
                            Arc::clone(&self.notify),
                        ));
                    }
                });
                widgets::wrapped_text(ui, &elided(&self.settings.path, width));
                ui.checkbox(
                    &mut self.settings.open_after_saving,
                    "Open the file once it is written",
                );
            }
            ui.separator();

            widgets::wrapped_text(ui, &self.summary());
            if let Some(reason) = self.settings.refusal() {
                widgets::wrapped_text(ui, reason);
            }
            if let Some(status) = &self.status {
                widgets::wrapped_text(ui, status);
            }
            ui.horizontal_wrapped(|ui| {
                let ready = self.settings.refusal().is_none() && self.job.is_none();
                if widgets::toolbar_button(
                    ui,
                    "Write",
                    ready,
                    self.settings
                        .refusal()
                        .unwrap_or("A report is already being written"),
                ) {
                    write = true;
                }
                if widgets::toolbar_button(ui, "Stop", self.job.is_some(), "Nothing is running") {
                    self.cancel();
                }
                if ui.button("Close").clicked() {
                    close = true;
                }
                if self.job.is_some() {
                    ui.spinner();
                    ui.label(format!("{} bytes", self.progress));
                }
            });
        });
        if close {
            action = ReportAction::Close;
        } else if write {
            action = ReportAction::Write;
        }
        action
    }

    /// One line saying what the next run writes.
    #[must_use]
    pub fn summary(&self) -> String {
        let document = if self.settings.writes_html() {
            "an HTML document"
        } else {
            match self.settings.extension() {
                "xml" => "an XML document",
                "csv" => "a comma separated table",
                "patch" => "a patch",
                _ => "a plain text document",
            }
        };
        match self.settings.target {
            Target::Clipboard => format!(
                "The {} layout is put on the clipboard as {document}.",
                self.settings.layout_choice().label.to_lowercase()
            ),
            _ => format!(
                "The {} layout is written to the named file as {document}.",
                self.settings.layout_choice().label.to_lowercase()
            ),
        }
    }
}

/// Shorten a path from the left so the end of it stays readable.
fn elided(path: &str, width: f32) -> String {
    /// Room one character of the dialog's font takes.
    const CHARACTER_WIDTH: f32 = 7.0;
    let count = path.chars().count();
    let mut room = 0usize;
    let mut used = 0.0f32;
    while room < count && used + CHARACTER_WIDTH <= width {
        used += CHARACTER_WIDTH;
        room += 1;
    }
    if room >= count {
        return path.to_owned();
    }
    let tail: String = path
        .chars()
        .skip(count.saturating_sub(room.saturating_sub(3)))
        .collect();
    format!("...{tail}")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::{elided, ReportDialog};
    use crate::report::{ReportKind, ReportSettings, Target};
    use std::sync::Arc;

    #[test]
    fn a_long_path_is_shortened_from_the_left() {
        let shortened = elided(&"a".repeat(400), 200.0);
        assert!(shortened.starts_with("..."));
        assert!(shortened.len() < 400);
    }

    #[test]
    fn a_short_path_is_left_alone() {
        assert_eq!(elided("out.html", 560.0), "out.html");
    }

    #[test]
    fn the_summary_names_the_layout_and_the_document() {
        let mut settings = ReportSettings::new(ReportKind::Text);
        settings.target = Target::Clipboard;
        settings.html = false;
        let dialog = ReportDialog::new(egui::Id::new("report"), settings, Arc::new(|| {}));
        let summary = dialog.summary();
        assert!(summary.contains("clipboard"), "{summary}");
        assert!(summary.contains("plain text"), "{summary}");
    }
}
