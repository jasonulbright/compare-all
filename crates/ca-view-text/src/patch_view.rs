//! A patch file shown as the comparison it describes.
//!
//! The patch is read, parsed and applied on a worker. With a file to apply
//! it to, the left side is that file and the right side is the patched
//! result; without one, the two sides are the lines the hunks carry. The
//! comparison itself is a read-only [`TextView`].

use crate::file_save::{FileSave, SaveEvent};
use crate::TextView;
use ca_diff::{apply_patch, parse_patch_with, PatchLimits};
use ca_text::{DecodeOptions, LoadedText, TextBuffer};
use ca_ui::command::Command;
use ca_ui::dialog::{self, DialogMessage, Pick};
use ca_ui::save::{Baseline, FileSystem, RealFileSystem, Stamp};
use ca_ui::view::{SessionView, ViewAction, ViewContext};
use ca_ui::worker::{Cancel, Emitter, Job, Terminal};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// What the view says when a write is asked for while the editing switch of
/// the session is on.
const EDITING_OFF: &str = "Editing is turned off for this session, so nothing is written.";

/// Commands the patch view answers itself.
const OWN: &[Command] = &[
    Command::ApplyPatch,
    Command::NextDifferenceFiles,
    Command::PreviousDifferenceFiles,
    Command::SaveFileAs,
    Command::OpenFile,
    Command::Reload,
    Command::Cancel,
];

/// Commands the comparison inside answers.
const SHOWN: &[Command] = &[
    Command::NextDifference,
    Command::PreviousDifference,
    Command::NextSection,
    Command::PreviousSection,
    Command::ShowAll,
    Command::ShowDifferences,
    Command::ShowSame,
    Command::ShowContext,
    Command::ToggleIgnoreUnimportant,
    Command::Find,
    Command::FindNext,
    Command::FindPrevious,
    Command::GoTo,
    Command::ClearBookmarks,
    Command::Copy,
    Command::SelectAll,
    Command::SelectSection,
    Command::ToggleLineNumbers,
    Command::ToggleLineDetails,
    Command::IncreaseFontSize,
    Command::DecreaseFontSize,
    Command::ResetFontSize,
];

/// What the patch worker posts.
#[derive(Debug)]
pub enum PatchMessage {
    /// The patch is read and applied.
    Ready(Box<PatchData>),
    /// The patch or the target could not be read, or the patch is not one.
    Failed(String),
    /// The work stopped.
    Cancelled,
}

impl Terminal for PatchMessage {
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

/// One file section of a patch, applied.
#[derive(Debug)]
pub struct PatchData {
    names: Vec<String>,
    index: usize,
    original: String,
    patched: String,
    patch_lines: Arc<Vec<String>>,
    target: Option<(LoadedText, Option<Stamp>)>,
    rejected: Vec<usize>,
    moved: Vec<usize>,
}

fn read_text(path: &Path, limit: usize) -> Result<(LoadedText, Option<Stamp>), String> {
    let size = std::fs::metadata(path)
        .map_err(|error| format!("{}: {error}", path.display()))?
        .len();
    if usize::try_from(size).map_or(true, |size| size > limit) {
        return Err(format!(
            "{} is larger than the {limit} byte limit.",
            path.display()
        ));
    }
    let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let stamp = RealFileSystem.stamp(path);
    Ok((LoadedText::load(&bytes, &DecodeOptions::default()), stamp))
}

fn run(
    patch: &Path,
    target: &Path,
    index: usize,
    emitter: &Emitter<PatchMessage>,
    cancel: &Cancel,
) {
    let limits = PatchLimits::default();
    let outcome = (|| {
        let (loaded, _) = read_text(patch, limits.max_input_bytes)?;
        let text = loaded.buffer.text();
        let parsed = parse_patch_with(&text, &limits).map_err(|error| error.to_string())?;
        let index = index.min(parsed.files.len().saturating_sub(1));
        let file = parsed
            .files
            .get(index)
            .ok_or_else(|| "The patch holds no file section.".to_owned())?;
        let names = parsed
            .files
            .iter()
            .map(ca_diff::FilePatch::display_name)
            .collect();
        let patch_lines = crate::edit::split_lines(&text);
        if target.as_os_str().is_empty() {
            let (original, patched) = file.reconstruct();
            return Ok(PatchData {
                names,
                index,
                original,
                patched,
                patch_lines,
                target: None,
                rejected: Vec::new(),
                moved: Vec::new(),
            });
        }
        let (source, stamp) = read_text(target, usize::MAX)?;
        let original = source.buffer.text();
        let applied = apply_patch(&original, file);
        Ok(PatchData {
            names,
            index,
            original,
            patched: applied.text,
            patch_lines,
            target: Some((source, stamp)),
            rejected: applied.rejected,
            moved: applied.moved,
        })
    })();
    if cancel.is_cancelled() {
        return;
    }
    emitter.send(match outcome {
        Ok(data) => PatchMessage::Ready(Box::new(data)),
        Err(reason) => PatchMessage::Failed(reason),
    });
}

/// Which field a file picker fills.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Picking {
    Patch,
    Target,
    SaveAs,
}

/// A patch viewing tab.
pub struct TextPatchView {
    id: egui::Id,
    context: ViewContext,
    instance: u64,
    patch_path: PathBuf,
    target_path: PathBuf,
    /// The settings the session last gave, reported with the sides replaced by
    /// the patch and the target the view has open.
    session_settings: ca_session::settings::TextPatchSettings,
    patch_field: String,
    target_field: String,
    job: Option<Job<PatchMessage>>,
    picker: Option<(Job<DialogMessage>, Picking)>,
    failure: Option<String>,
    inner: Option<TextView>,
    data: Option<PatchData>,
    index: usize,
    show_patch: bool,
    save: FileSave,
    message: Option<String>,
}

impl TextPatchView {
    /// A view of the patch at `patch`, applied to `target` when one is named.
    #[must_use]
    pub fn new(patch: PathBuf, target: PathBuf, context: &ViewContext, instance: u64) -> Self {
        let mut view = Self {
            id: egui::Id::new(("text-patch", instance)),
            context: context.clone(),
            instance,
            patch_field: patch.display().to_string(),
            target_field: target.display().to_string(),
            patch_path: patch,
            target_path: target,
            session_settings: ca_session::settings::TextPatchSettings::default(),
            job: None,
            picker: None,
            failure: None,
            inner: None,
            data: None,
            index: 0,
            show_patch: false,
            save: FileSave::default(),
            message: None,
        };
        view.load();
        view
    }

    fn load(&mut self) {
        if let Some(job) = self.job.take() {
            job.cancel();
        }
        self.failure = None;
        if self.patch_path.as_os_str().is_empty() {
            self.inner = None;
            self.data = None;
            return;
        }
        let (patch, target, index) = (
            self.patch_path.clone(),
            self.target_path.clone(),
            self.index,
        );
        self.job = Some(Job::spawn_notifying(
            move |emitter, cancel| run(&patch, &target, index, emitter, cancel),
            self.context.notify.clone(),
        ));
    }

    /// The comparison on screen, once the patch is applied.
    #[must_use]
    pub const fn comparison(&self) -> Option<&TextView> {
        self.inner.as_ref()
    }

    /// True once the patch is applied and its comparison has finished.
    #[must_use]
    pub fn is_shown(&self) -> bool {
        self.job.is_none() && self.inner.as_ref().is_some_and(TextView::is_ready)
    }

    /// Why the patch could not be shown, where it could not.
    #[must_use]
    pub fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    /// The patched text of the file section on screen.
    #[must_use]
    pub fn patched_text(&self) -> Option<&str> {
        self.data.as_ref().map(|data| data.patched.as_str())
    }

    /// The original text of the file section on screen.
    #[must_use]
    pub fn original_text(&self) -> Option<&str> {
        self.data.as_ref().map(|data| data.original.as_str())
    }

    /// The hunks of the section on screen that matched nowhere.
    #[must_use]
    pub fn rejected_hunks(&self) -> &[usize] {
        self.data
            .as_ref()
            .map_or(&[], |data| data.rejected.as_slice())
    }

    /// The names of the file sections the patch holds.
    #[must_use]
    pub fn file_names(&self) -> &[String] {
        self.data.as_ref().map_or(&[], |data| data.names.as_slice())
    }

    /// Which file section is on screen.
    #[must_use]
    pub const fn file_index(&self) -> usize {
        self.index
    }

    /// The last message the view showed.
    #[must_use]
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    /// True while a write runs.
    #[must_use]
    pub const fn is_saving(&self) -> bool {
        self.save.is_busy()
    }

    /// Answer the question a save raised.
    pub fn answer(&mut self, accept: bool) {
        let notify = self.context.notify.clone();
        self.save.answer(accept, &notify);
    }

    /// Write the patched result under `path`.
    pub fn save_result_as(&mut self, path: &Path) {
        self.write(path.to_path_buf(), Baseline::Unchecked);
    }

    fn apply_to_target(&mut self) {
        let Some(stamp) = self
            .data
            .as_ref()
            .and_then(|data| data.target.as_ref())
            .map(|(_, stamp)| *stamp)
        else {
            return;
        };
        self.write(self.target_path.clone(), stamp.into());
    }

    fn write(&mut self, path: PathBuf, expected: Baseline) {
        if self.session_settings.specs.disable_editing {
            self.message = Some(EDITING_OFF.to_owned());
            return;
        }
        let Some(data) = self.data.as_ref() else {
            return;
        };
        let mut source = data.target.as_ref().map_or_else(
            || LoadedText::load(b"", &DecodeOptions::default()),
            |(source, _)| source.clone(),
        );
        source.buffer = TextBuffer::from_text(&data.patched);
        let revision = source.buffer.revision();
        let notify = self.context.notify.clone();
        if !self.save.start(path, source, expected, revision, &notify) {
            self.message = Some("A save is still running. Try again.".to_owned());
        }
    }

    fn show_file(&mut self, index: usize) {
        let count = self.file_names().len();
        if count == 0 || index >= count || index == self.index {
            return;
        }
        self.index = index;
        self.load();
    }

    fn poll(&mut self) {
        if let Some(job) = self.job.as_mut() {
            let messages = job.drain();
            let finished = job.is_finished();
            for message in messages {
                match message {
                    PatchMessage::Ready(data) => {
                        let data = *data;
                        self.index = data.index;
                        let name = if self.target_path.as_os_str().is_empty() {
                            PathBuf::from(data.names.get(data.index).cloned().unwrap_or_default())
                        } else {
                            self.target_path.clone()
                        };
                        self.inner = Some(TextView::fixed(
                            name.clone(),
                            name,
                            &self.context,
                            self.instance,
                            (data.original.clone(), data.patched.clone()),
                        ));
                        self.message = match (data.rejected.len(), data.moved.len()) {
                            (0, 0) => None,
                            (0, moved) => Some(format!("{moved} hunks applied at a moved line.")),
                            (rejected, _) => Some(format!(
                                "{rejected} hunks did not match the file and are left out."
                            )),
                        };
                        self.data = Some(data);
                    }
                    PatchMessage::Failed(reason) => {
                        self.failure = Some(reason);
                        self.inner = None;
                        self.data = None;
                    }
                    PatchMessage::Cancelled => {}
                }
            }
            if finished {
                self.job = None;
            }
        }
        if let Some((job, picking)) = self.picker.as_mut() {
            let picking = *picking;
            let messages = job.drain();
            if job.is_finished() || !messages.is_empty() {
                self.picker = None;
            }
            for message in messages {
                if let DialogMessage::Chosen(path) = message {
                    match picking {
                        Picking::Patch => self.patch_field = path.display().to_string(),
                        Picking::Target => self.target_field = path.display().to_string(),
                        Picking::SaveAs => self.save_result_as(&path),
                    }
                    if picking != Picking::SaveAs {
                        self.take_fields();
                    }
                }
            }
        }
        match self.save.poll() {
            Some(SaveEvent::Saved { path, .. }) => {
                self.message = Some(format!("Written to {}.", path.display()));
            }
            Some(SaveEvent::NotWritable) => {
                self.message = Some("The file cannot be written.".to_owned());
            }
            Some(SaveEvent::Failed(reason)) => self.message = Some(reason),
            None => {}
        }
    }

    fn take_fields(&mut self) {
        self.patch_path = PathBuf::from(self.patch_field.trim());
        self.target_path = PathBuf::from(self.target_field.trim());
        self.index = 0;
        self.load();
    }

    fn pick(&mut self, picking: Picking) {
        if self.picker.is_some() {
            return;
        }
        let pick = if picking == Picking::SaveAs {
            Pick::SaveFile
        } else {
            Pick::File
        };
        self.picker = Some((dialog::spawn(pick, self.context.notify.clone()), picking));
    }

    fn head(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Patch");
            ui.add(egui::TextEdit::singleline(&mut self.patch_field).desired_width(300.0));
            if ca_ui::widgets::inline_button(ui, "Browse", ca_ui::icons::Icon::Browse).clicked() {
                self.pick(Picking::Patch);
            }
            ui.label("Apply to");
            ui.add(egui::TextEdit::singleline(&mut self.target_field).desired_width(300.0));
            if ca_ui::widgets::inline_button(ui, "Browse", ca_ui::icons::Icon::Browse).clicked() {
                self.pick(Picking::Target);
            }
            if ca_ui::widgets::inline_button(ui, "Load", ca_ui::icons::Icon::OpenFile).clicked() {
                self.take_fields();
            }
        });
        ui.horizontal(|ui| {
            let count = self.file_names().len();
            if ui
                .add_enabled(
                    self.accepts(Command::PreviousDifferenceFiles),
                    egui::Button::new("Prev File"),
                )
                .clicked()
            {
                self.run(Command::PreviousDifferenceFiles);
            }
            if ui
                .add_enabled(
                    self.accepts(Command::NextDifferenceFiles),
                    egui::Button::new("Next File"),
                )
                .clicked()
            {
                self.run(Command::NextDifferenceFiles);
            }
            if count > 0 {
                let name = self
                    .file_names()
                    .get(self.index)
                    .cloned()
                    .unwrap_or_default();
                ui.label(format!("File {} of {count}: {name}", self.index + 1));
            }
            ui.separator();
            if ui
                .add_enabled(
                    self.accepts(Command::ApplyPatch),
                    egui::Button::new("Apply Patch"),
                )
                .on_hover_text(Command::ApplyPatch.description())
                .clicked()
            {
                self.run(Command::ApplyPatch);
            }
            if ui
                .add_enabled(
                    self.accepts(Command::SaveFileAs),
                    egui::Button::new("Save Result As"),
                )
                .clicked()
            {
                self.run(Command::SaveFileAs);
            }
            ui.checkbox(&mut self.show_patch, "Patch text");
        });
        let notify = self.context.notify.clone();
        self.save.prompt_ui(ui, self.id, &notify);
        if let Some(reason) = &self.failure {
            ui.colored_label(ui.visuals().error_fg_color, reason.as_str());
        }
        if let Some(message) = &self.message {
            ca_ui::widgets::notice_current(ui, ca_ui::icons::Icon::Info, 16.0, message.as_str());
        }
    }

    fn patch_text(&self, ui: &mut egui::Ui) {
        let Some(data) = self.data.as_ref() else {
            return;
        };
        let lines = &data.patch_lines;
        let row = ui.text_style_height(&egui::TextStyle::Monospace);
        egui::ScrollArea::both()
            .id_salt(self.id.with("patch-text"))
            .auto_shrink([false, false])
            .show_rows(ui, row, lines.len(), |ui, range| {
                for line in lines.get(range).unwrap_or_default() {
                    ui.monospace(line.as_str());
                }
            });
    }
}

impl ca_ui::view::ViewFactory for TextPatchView {
    fn create(left: PathBuf, right: PathBuf, context: &ViewContext, instance: u64) -> Self {
        Self::new(left, right, context, instance)
    }
}

impl SessionView for TextPatchView {
    fn kind(&self) -> Option<ca_session::SessionKind> {
        Some(ca_session::SessionKind::TextPatch)
    }

    fn menu_view(&self) -> ca_ui::command::MenuView {
        ca_ui::command::MenuView::Other
    }

    fn apply_settings(&mut self, settings: &ca_session::settings::SessionSettings) {
        if let ca_session::settings::SessionSettings::TextPatch(patch) = settings {
            self.session_settings.clone_from(patch);
        }
    }

    fn settings(&self) -> Option<ca_session::settings::SessionSettings> {
        let mut settings = self.session_settings.clone();
        settings.specs =
            ca_ui::view::with_sides(&settings.specs, &self.patch_path, &self.target_path);
        Some(ca_session::settings::SessionSettings::TextPatch(settings))
    }

    fn title(&self) -> String {
        if self.patch_path.as_os_str().is_empty() {
            return "Text Patch".to_owned();
        }
        crate::file_name(&self.patch_path)
    }

    fn tick(&mut self) {
        self.poll();
        if let Some(inner) = self.inner.as_mut() {
            inner.tick();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, context: &ViewContext) -> Vec<ViewAction> {
        let mut actions = Vec::new();
        egui::TopBottomPanel::top(self.id.with("patch-head")).show_inside(ui, |ui| {
            self.head(ui);
        });
        if self.show_patch {
            egui::TopBottomPanel::bottom(self.id.with("patch-body"))
                .resizable(true)
                .default_height(160.0)
                .show_inside(ui, |ui| self.patch_text(ui));
        }
        egui::CentralPanel::default().show_inside(ui, |ui| {
            if let Some(inner) = self.inner.as_mut() {
                actions = inner.ui(ui, context);
            } else if self.job.is_some() {
                ui.spinner();
            } else if self.failure.is_none() {
                ui.label("Name a patch file, and the file to apply it to, then choose Load.");
            }
        });
        actions
    }

    fn commands(&self) -> Vec<ca_ui::view::CommandState> {
        let all: Vec<Command> = OWN.iter().chain(SHOWN.iter()).copied().collect();
        ca_ui::view::declare(&all, |command| self.accepts(command))
    }

    fn accepts(&self, command: Command) -> bool {
        let shown = self.data.is_some() && self.job.is_none();
        let writes = !self.session_settings.specs.disable_editing;
        match command {
            Command::ApplyPatch => {
                writes
                    && shown
                    && !self.save.is_busy()
                    && self
                        .data
                        .as_ref()
                        .is_some_and(|data| data.target.is_some() && data.rejected.is_empty())
            }
            Command::NextDifferenceFiles => shown && self.index + 1 < self.file_names().len(),
            Command::PreviousDifferenceFiles => shown && self.index > 0,
            Command::SaveFileAs => writes && shown && !self.save.is_busy() && self.picker.is_none(),
            Command::OpenFile => !self.save.is_busy() && self.picker.is_none(),
            Command::Reload => !self.patch_path.as_os_str().is_empty(),
            Command::Cancel => self.job.is_some(),
            other if SHOWN.contains(&other) => self
                .inner
                .as_ref()
                .is_some_and(|inner| inner.accepts(other)),
            _ => false,
        }
    }

    fn run(&mut self, command: Command) {
        match command {
            Command::ApplyPatch => self.apply_to_target(),
            Command::NextDifferenceFiles => self.show_file(self.index + 1),
            Command::PreviousDifferenceFiles => {
                if let Some(index) = self.index.checked_sub(1) {
                    self.show_file(index);
                }
            }
            Command::SaveFileAs => self.pick(Picking::SaveAs),
            Command::OpenFile => self.pick(Picking::Patch),
            Command::Reload => self.load(),
            Command::Cancel => {
                if let Some(job) = self.job.as_ref() {
                    job.cancel();
                }
            }
            other if SHOWN.contains(&other) => {
                if let Some(inner) = self.inner.as_mut() {
                    inner.run(other);
                }
            }
            _ => {}
        }
    }

    fn may_close(&mut self) -> bool {
        !self.save.blocks_close()
    }

    fn is_busy(&self) -> bool {
        self.save.is_busy()
    }

    fn is_ready(&self) -> bool {
        self.job.is_none()
            && self
                .inner
                .as_ref()
                .is_none_or(<TextView as SessionView>::is_ready)
    }

    fn on_close(&mut self) {
        if let Some(job) = self.job.take() {
            job.cancel();
        }
        if let Some((job, _)) = self.picker.take() {
            job.cancel();
        }
        if let Some(inner) = self.inner.as_mut() {
            inner.on_close();
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::TextPatchView;
    use ca_ui::command::Command;
    use ca_ui::view::SessionView;
    use std::path::PathBuf;

    #[test]
    fn open_file_is_available_for_the_patch_input() {
        let view = TextPatchView::new(
            PathBuf::new(),
            PathBuf::new(),
            &ca_ui::testing::context(),
            1,
        );
        assert!(view
            .commands()
            .iter()
            .any(|state| state.command == Command::OpenFile && state.enabled));
    }
}
