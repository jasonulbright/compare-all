//! Writing sessions and settings to a package, and reading one back.
//!
//! Both touch the disk, so both run on a worker. What an import did is shown
//! as a report rather than left to be guessed at.

use crate::worker::{Job, Terminal};
use ca_session::{ExportSelection, ImportOptions, ImportReport, SessionStore, SettingsPackage};
use std::path::PathBuf;
use std::sync::Arc;

/// What the package worker posts back.
pub enum ShareMessage {
    /// A package was written.
    Exported {
        /// Where it was written.
        path: PathBuf,
        /// How many sessions it carries.
        sessions: usize,
    },
    /// A package was read and applied.
    Imported {
        /// The store with the package applied.
        store: Box<SessionStore>,
        /// The application options the package carried, where it carried any.
        options: Option<Box<ca_session::ProgramOptions>>,
        /// What the import did.
        report: Box<ImportReport>,
    },
    /// The work failed. A store handed to the job comes back with it.
    Failed {
        /// The store the job was given, where it had one.
        store: Option<Box<SessionStore>>,
        /// What went wrong.
        reason: String,
    },
}

impl Terminal for ShareMessage {
    fn is_terminal(&self) -> bool {
        true
    }

    fn cancelled() -> Self {
        ShareMessage::Failed {
            store: None,
            reason: "the package was not read".to_owned(),
        }
    }

    fn panicked(detail: String) -> Self {
        ShareMessage::Failed {
            store: None,
            reason: detail,
        }
    }
}

/// Writes a package on a worker.
pub fn spawn_export(
    path: PathBuf,
    store: &SessionStore,
    options: &ca_session::ProgramOptions,
    selection: &ExportSelection,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<ShareMessage> {
    let mut package = SettingsPackage::export(store, selection);
    if selection.program_options {
        package = package.with_program_options(options.clone());
    }
    Job::spawn_notifying(
        move |emitter, _| {
            let message = match package.write(&path) {
                Ok(()) => ShareMessage::Exported {
                    sessions: package.sessions.len(),
                    path,
                },
                Err(error) => ShareMessage::Failed {
                    store: None,
                    reason: format!("The package could not be written: {error}"),
                },
            };
            emitter.send(message);
        },
        notify,
    )
}

/// Reads a package and applies it to a copy of the store, on a worker.
///
/// The store is handed to the job and comes back with the package applied, so
/// a half-applied package is never left in the window.
pub fn spawn_import(
    path: PathBuf,
    store: Box<SessionStore>,
    options: ImportOptions,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<ShareMessage> {
    Job::spawn_notifying(
        move |emitter, _| {
            let mut store = store;
            let message = match SettingsPackage::read(&path) {
                Ok(package) => match package.import(&mut store, &options) {
                    Ok(report) => ShareMessage::Imported {
                        store,
                        options: package.program_options().cloned().map(Box::new),
                        report: Box::new(report),
                    },
                    Err(error) => ShareMessage::Failed {
                        store: Some(store),
                        reason: format!("The package could not be applied: {error}"),
                    },
                },
                Err(error) => ShareMessage::Failed {
                    store: Some(store),
                    reason: format!("The package could not be read: {error}"),
                },
            };
            emitter.send(message);
        },
        notify,
    )
}

/// The lines of an import report, one statement each.
#[must_use]
pub fn report_lines(report: &ImportReport) -> Vec<String> {
    let mut lines = vec![
        format!("{} sessions added", report.sessions_added),
        format!("{} sessions replaced", report.sessions_replaced),
        format!(
            "{} sessions left alone because they are locked",
            report.sessions_skipped_locked
        ),
        format!("{} workspaces imported", report.workspaces_imported),
        format!(
            "{} session defaults imported",
            report.session_defaults_imported
        ),
    ];
    if report.tabs_dropped > 0 {
        lines.push(format!(
            "{} workspace tabs named a session the package does not carry",
            report.tabs_dropped
        ));
    }
    if let Some(version) = report.newer_schema {
        lines.push(format!(
            "The package was written by a newer build, schema {version}"
        ));
    }
    if report.unplaced_fields > 0 {
        lines.push(format!(
            "{} fields this build does not understand were kept",
            report.unplaced_fields
        ));
    }
    if report.unknown_tree_nodes_not_imported > 0 {
        lines.push(format!(
            "{} session-tree nodes this build does not understand were kept in the package and not imported",
            report.unknown_tree_nodes_not_imported
        ));
    }
    for failure in &report.failures {
        lines.push(format!("{}: {}", failure.item, failure.reason));
    }
    lines
}

/// The report of an import, shown until it is dismissed.
pub struct ImportReportDialog {
    lines: Vec<String>,
    open: bool,
    id: egui::Id,
}

impl ImportReportDialog {
    /// A dialog over one report.
    #[must_use]
    pub fn new(report: &ImportReport, instance: u64) -> Self {
        Self {
            lines: report_lines(report),
            open: true,
            id: egui::Id::new(("import-report", instance)),
        }
    }

    /// True while the report is on screen.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// The lines the report shows.
    #[must_use]
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    /// Draw the report.
    pub fn show(&mut self, ctx: &egui::Context) {
        if !self.open {
            return;
        }
        let mut open = true;
        egui::Window::new("Import Report")
            .id(self.id)
            .open(&mut open)
            .collapsible(false)
            .max_width((ctx.screen_rect().width() - 24.0).max(200.0))
            .show(ctx, |ui| self.body(ui));
        if !open {
            self.open = false;
        }
    }

    /// Draw the report into a panel, for a headless frame.
    pub fn body(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .id_salt(self.id.with("lines"))
            .max_height(240.0)
            .show(ui, |ui| {
                for line in &self.lines {
                    crate::widgets::wrapped_text(ui, line);
                }
            });
        if ui.button("Close").clicked() {
            self.open = false;
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::{report_lines, spawn_export, spawn_import, ShareMessage};
    use ca_session::{ExportSelection, ImportOptions, SavedSession, SessionKind, SessionStore};
    use std::sync::Arc;
    use std::time::Duration;

    fn drain(job: &mut crate::worker::Job<ShareMessage>) -> ShareMessage {
        let mut held = None;
        assert!(crate::testing::wait_until(Duration::from_secs(10), || {
            for message in job.drain() {
                held = Some(message);
            }
            held.is_some()
        }));
        held.unwrap()
    }

    fn store_with_one_session() -> SessionStore {
        let mut store = SessionStore::default();
        let id = store.next_id();
        store
            .add_session(
                None,
                SavedSession::new(id, "nightly", SessionKind::TextCompare),
            )
            .unwrap();
        store
    }

    #[test]
    fn a_package_written_here_reads_back_into_another_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("share.capkg");
        let store = store_with_one_session();
        let mut options = ca_session::ProgramOptions::default();
        options.text_editing.tab_stop = 3;
        let mut job = spawn_export(
            path.clone(),
            &store,
            &options,
            &ExportSelection::everything(),
            Arc::new(|| {}),
        );
        let ShareMessage::Exported { sessions, .. } = drain(&mut job) else {
            panic!("the package was not written");
        };
        assert_eq!(sessions, 1);

        let mut job = spawn_import(
            path,
            Box::default(),
            ImportOptions::default(),
            Arc::new(|| {}),
        );
        let ShareMessage::Imported {
            store,
            options,
            report,
        } = drain(&mut job)
        else {
            panic!("the package was not read");
        };
        assert_eq!(report.sessions_added, 1);
        assert_eq!(store.sessions().len(), 1);
        assert_eq!(
            options.map(|options| options.text_editing.tab_stop),
            Some(3),
            "the package did not carry the application options"
        );
    }

    #[test]
    fn a_missing_package_hands_the_store_back() {
        let dir = tempfile::tempdir().unwrap();
        let mut job = spawn_import(
            dir.path().join("absent.capkg"),
            Box::new(store_with_one_session()),
            ImportOptions::default(),
            Arc::new(|| {}),
        );
        let ShareMessage::Failed { store, reason } = drain(&mut job) else {
            panic!("a missing package was read");
        };
        assert!(!reason.is_empty());
        assert_eq!(store.unwrap().sessions().len(), 1, "the store came back");
    }

    #[test]
    fn a_report_states_every_count() {
        let report = ca_session::ImportReport {
            sessions_added: 2,
            tabs_dropped: 1,
            unknown_tree_nodes_not_imported: 3,
            ..ca_session::ImportReport::default()
        };
        let lines = report_lines(&report);
        assert!(lines.iter().any(|line| line.contains("2 sessions added")));
        assert!(lines.iter().any(|line| line.contains("workspace tabs")));
        assert!(lines
            .iter()
            .any(|line| line.contains("3 session-tree nodes")));
    }
}
