//! One file written through the shared text save, with the questions a save
//! can raise.
//!
//! The single pane views write one file at a time. The write itself, the
//! changed-on-disk check, the backup and the line ending rule are
//! `ca_ui::save`; this module holds what is being written while a question
//! is open, so an answer retries the same bytes to the same name.

use crate::save::{self, SaveConsent};
use ca_text::LoadedText;
use ca_ui::save::{Baseline, SaveMessage, SaveOutcome, SaveRules, Stamp};
use ca_ui::worker::Job;
use std::path::PathBuf;
use std::sync::Arc;

/// What is being written.
#[derive(Clone)]
struct Pending {
    path: PathBuf,
    source: LoadedText,
    expected: Baseline,
    revision: u64,
    /// What the user agreed to so far. An answer adds its own consent to
    /// these and never grants another.
    consent: SaveConsent,
}

/// A question a save raised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SavePrompt {
    /// The file changed on disk since it was read.
    DiskChanged,
    /// Writing cannot reproduce every character.
    WouldLose(String),
    /// A concurrent version was kept beside the saved file.
    KeptCopy(String),
}

/// What a finished save reports to its view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveEvent {
    /// The file was written.
    Saved {
        /// Where it was written.
        path: PathBuf,
        /// What the file looks like now.
        stamp: Stamp,
        /// The buffer revision the bytes came from.
        revision: u64,
    },
    /// The target cannot be written.
    NotWritable,
    /// The write failed or stopped; the text says why.
    Failed(String),
}

/// The save of one file, with its open question.
#[derive(Default)]
pub struct FileSave {
    job: Option<Job<SaveMessage>>,
    pending: Option<Pending>,
    prompt: Option<SavePrompt>,
    /// What the options add to a save.
    pub rules: SaveRules,
}

impl FileSave {
    /// True while a write runs.
    #[must_use]
    pub const fn is_busy(&self) -> bool {
        self.job.is_some()
    }

    /// The question waiting on an answer, where one is.
    #[must_use]
    pub const fn prompt(&self) -> Option<&SavePrompt> {
        self.prompt.as_ref()
    }

    /// True when a close has to wait: a write runs or a kept copy is unread.
    #[must_use]
    pub const fn blocks_close(&self) -> bool {
        self.job.is_some() || matches!(self.prompt, Some(SavePrompt::KeptCopy(_)))
    }

    /// Start writing `source` to `path`.
    ///
    /// Returns false when a write already runs or a kept copy is unread.
    pub fn start(
        &mut self,
        path: PathBuf,
        source: LoadedText,
        expected: Baseline,
        revision: u64,
        notify: &Arc<dyn Fn() + Send + Sync>,
    ) -> bool {
        if self.blocks_close() {
            return false;
        }
        self.pending = Some(Pending {
            path,
            source,
            expected,
            revision,
            consent: SaveConsent::default(),
        });
        self.prompt = None;
        self.spawn(notify);
        true
    }

    fn spawn(&mut self, notify: &Arc<dyn Fn() + Send + Sync>) {
        let Some(pending) = self.pending.clone() else {
            return;
        };
        self.job = Some(save::spawn(
            pending.path,
            pending.source,
            pending.expected,
            pending.consent,
            self.rules.clone(),
            notify.clone(),
        ));
    }

    /// Answer the open question. A yes retries with the consent it gives,
    /// added to the consent the earlier answers of this save gave.
    pub fn answer(&mut self, accept: bool, notify: &Arc<dyn Fn() + Send + Sync>) {
        let Some(prompt) = self.prompt.take() else {
            return;
        };
        if !accept {
            self.pending = None;
            return;
        }
        let Some(pending) = self.pending.as_mut() else {
            return;
        };
        match prompt {
            SavePrompt::DiskChanged => pending.consent.accept_disk_change = true,
            SavePrompt::WouldLose(_) => pending.consent.accept_loss = true,
            SavePrompt::KeptCopy(_) => {
                self.pending = None;
                return;
            }
        }
        self.spawn(notify);
    }

    /// Take what the write posted.
    pub fn poll(&mut self) -> Option<SaveEvent> {
        let job = self.job.as_mut()?;
        let messages = job.drain();
        let finished = job.is_finished();
        if !finished && messages.is_empty() {
            return None;
        }
        self.job = None;
        let outcome = messages.into_iter().find_map(|message| match message {
            SaveMessage::Done(outcome) => Some(*outcome),
            SaveMessage::Cancelled => None,
        });
        let Some(outcome) = outcome else {
            self.pending = None;
            return Some(SaveEvent::Failed("The save stopped.".to_owned()));
        };
        match outcome {
            SaveOutcome::Saved(stamp) | SaveOutcome::SavedWithConflict { stamp, .. } => {
                if let SaveOutcome::SavedWithConflict { backup, .. } = &outcome {
                    self.prompt = Some(SavePrompt::KeptCopy(backup.display().to_string()));
                }
                let pending = self.pending.take()?;
                if self.prompt.is_some() {
                    self.pending = Some(pending.clone());
                }
                Some(SaveEvent::Saved {
                    path: pending.path,
                    stamp,
                    revision: pending.revision,
                })
            }
            SaveOutcome::ChangedOnDisk => {
                self.prompt = Some(SavePrompt::DiskChanged);
                None
            }
            SaveOutcome::WouldLose(reason) => {
                self.prompt = Some(SavePrompt::WouldLose(reason));
                None
            }
            SaveOutcome::NotWritable => {
                self.pending = None;
                Some(SaveEvent::NotWritable)
            }
            SaveOutcome::Failed(reason) => {
                self.pending = None;
                Some(SaveEvent::Failed(format!(
                    "The file was not written: {reason}"
                )))
            }
        }
    }

    /// The strip or window that asks the open question.
    pub fn prompt_ui(
        &mut self,
        ui: &mut egui::Ui,
        id: egui::Id,
        notify: &Arc<dyn Fn() + Send + Sync>,
    ) {
        let Some(prompt) = self.prompt.clone() else {
            return;
        };
        let mut answer: Option<bool> = None;
        match &prompt {
            SavePrompt::KeptCopy(path) => {
                egui::Window::new("Another version was kept")
                    .id(id.with("kept-copy"))
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ui.ctx(), |ui| {
                        ca_ui::widgets::notice_current(ui,ca_ui::icons::Icon::Warning,24.0,"Another program changed the file during your save. The saved file contains your edit. Review the other version at:");
                        ui.monospace(path);
                        if ui.button("I have the path").clicked() {
                            answer = Some(true);
                        }
                    });
            }
            SavePrompt::DiskChanged | SavePrompt::WouldLose(_) => {
                ui.horizontal_wrapped(|ui| {
                    let (label, yes) = match &prompt {
                        SavePrompt::WouldLose(reason) => (reason.as_str(), "Save anyway"),
                        _ => (
                            "The file changed on disk since it was read. Overwrite it?",
                            "Overwrite",
                        ),
                    };
                    ui.label(label);
                    if ui.button(yes).clicked() {
                        answer = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        answer = Some(false);
                    }
                });
            }
        }
        if let Some(answer) = answer {
            self.answer(answer, notify);
        }
    }
}
