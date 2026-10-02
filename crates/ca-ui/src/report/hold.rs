//! What one view holds for its report command.
//!
//! Every view opens the same dialog over its own comparison, so the state
//! behind the command lives here once: the dialog, the settings it starts from
//! and the settings it leaves behind.

use super::dialog::{ReportAction, ReportDialog};
use super::payload::Payload;
use super::plan::{ReportKind, ReportSettings};
use ca_report::options::ReportMeta;
use ca_session::options::ReportPreference;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

/// The settings each view kind left behind, waiting to reach the document.
///
/// A view cannot write the options document: the shell owns it. The view
/// records what it ran under, and the shell takes the records and stores them,
/// so the two never hold the document at the same time.
fn recorded() -> &'static Mutex<BTreeMap<String, ReportPreference>> {
    static HELD: OnceLock<Mutex<BTreeMap<String, ReportPreference>>> = OnceLock::new();
    HELD.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Record what one view kind was last run under.
pub fn record(kind: ReportKind, preference: ReportPreference) {
    if let Ok(mut held) = recorded().lock() {
        held.insert(kind.id().to_owned(), preference);
    }
}

/// Take every record made since the last call.
#[must_use]
pub fn take_records() -> BTreeMap<String, ReportPreference> {
    recorded()
        .lock()
        .map(|mut held| std::mem::take(&mut *held))
        .unwrap_or_default()
}

/// The report command of one view.
pub struct ViewReport {
    kind: ReportKind,
    id: egui::Id,
    notify: Arc<dyn Fn() + Send + Sync>,
    dialog: Option<ReportDialog>,
    clipboard: Option<String>,
    requested: bool,
}

impl ViewReport {
    /// The report command of a view of `kind`.
    #[must_use]
    pub fn new(kind: ReportKind, id: egui::Id, notify: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            kind,
            id,
            notify,
            dialog: None,
            clipboard: None,
            requested: false,
        }
    }

    /// Ask for the dialog.
    ///
    /// A command runs with no frame in hand and the settings are seeded from
    /// the options the frame carries, so the request waits for the next frame.
    pub const fn request(&mut self) {
        self.requested = true;
    }

    /// Raise a requested dialog and drain whatever the jobs posted.
    pub fn poll(&mut self, ctx: &egui::Context) {
        self.poll_with(ctx, |_| {});
    }

    /// The same, with the view stating what it is displaying.
    ///
    /// `seed` runs only on the frame the dialog opens, so a change the user
    /// makes in the dialog is not overwritten on the next frame.
    pub fn poll_with(&mut self, ctx: &egui::Context, seed: impl FnOnce(&mut ReportSettings)) {
        if self.requested {
            self.requested = false;
            self.open(ctx);
            if let Some(dialog) = &mut self.dialog {
                seed(dialog.settings_mut());
            }
        }
        self.tick();
    }

    /// True while the dialog is on screen.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.dialog.is_some()
    }

    /// Raise the dialog, seeded with what this kind was last run under.
    pub fn open(&mut self, ctx: &egui::Context) {
        let stored = crate::options::runtime::current(ctx);
        let held = stored.stored.reports.view(self.kind.id()).cloned();
        self.open_with(ReportSettings::from_preference(self.kind, held.as_ref()));
    }

    /// Raise the dialog over stated settings.
    pub fn open_with(&mut self, settings: ReportSettings) {
        self.dialog = Some(ReportDialog::new(
            self.id.with("report"),
            settings,
            Arc::clone(&self.notify),
        ));
    }

    /// Drop the dialog, which stops whatever it was writing.
    pub fn close(&mut self) {
        if let Some(dialog) = &self.dialog {
            record(self.kind, dialog.settings().to_preference());
        }
        self.dialog = None;
    }

    /// Take whatever the dialog's jobs posted, whether or not it is drawn.
    pub fn tick(&mut self) {
        if let Some(dialog) = &mut self.dialog {
            if let ReportAction::Copy(text) = dialog.tick() {
                self.clipboard = Some(text);
            }
        }
    }

    /// What the settings say right now, where the dialog is open.
    #[must_use]
    pub fn settings(&self) -> Option<&ReportSettings> {
        self.dialog.as_ref().map(ReportDialog::settings)
    }

    /// Take the text a clipboard run produced.
    pub fn take_clipboard(&mut self) -> Option<String> {
        self.clipboard.take()
    }

    /// Draw the dialog and report whether the view should build a payload.
    ///
    /// The view answers a [`ReportAction::Write`] with [`ViewReport::start`],
    /// so the payload is built only when a write begins.
    pub fn draw(&mut self, ui: &mut egui::Ui) -> ReportAction {
        let Some(dialog) = &mut self.dialog else {
            return ReportAction::None;
        };
        match dialog.ui(ui) {
            ReportAction::Write => ReportAction::Write,
            ReportAction::Copy(text) => {
                self.clipboard = Some(text);
                ReportAction::None
            }
            ReportAction::Close => {
                self.close();
                ReportAction::None
            }
            ReportAction::None => ReportAction::None,
        }
    }

    /// Start the write of `payload`.
    pub fn start(&mut self, meta: ReportMeta, payload: Payload, bytes_per_row: u32) {
        if let Some(dialog) = &mut self.dialog {
            dialog.start(meta, payload, bytes_per_row);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::{record, take_records, ReportKind, ViewReport};
    use ca_session::options::ReportPreference;
    use std::sync::Arc;

    #[test]
    fn a_dialog_opens_and_closes() {
        let mut held = ViewReport::new(ReportKind::Hex, egui::Id::new("t"), Arc::new(|| {}));
        assert!(!held.is_open());
        held.open_with(super::ReportSettings::new(ReportKind::Hex));
        assert!(held.is_open());
        held.close();
        assert!(!held.is_open());
    }

    #[test]
    fn a_record_is_taken_once() {
        let _ = take_records();
        record(
            ReportKind::Picture,
            ReportPreference {
                layout: "summary".to_owned(),
                ..ReportPreference::default()
            },
        );
        let taken = take_records();
        assert_eq!(
            taken.get("picture").map(|held| held.layout.as_str()),
            Some("summary")
        );
        assert!(take_records().is_empty());
    }
}
