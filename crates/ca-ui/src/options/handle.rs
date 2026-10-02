//! The options document, read and written on a worker.
//!
//! Loading and saving both touch the disk, so neither runs on the frame thread.
//! The handle owns the document between jobs and hands it to the job that is
//! writing it, so a save and an edit can never run over each other. A save that
//! meets a document another instance replaced combines the two rather than
//! dropping either.

use crate::worker::{Job, Terminal};
use ca_session::options::ProgramOptions;
use ca_session::SettingsPaths;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// What the options worker posts back.
pub enum OptionsMessage {
    /// The document was read.
    Loaded {
        /// The options, the built-in set when no document existed.
        options: Box<ProgramOptions>,
        /// Something about the load a person has to be told.
        notice: Option<String>,
    },
    /// The document was written.
    Saved {
        /// The options, with the document they now match.
        options: Box<ProgramOptions>,
        /// True when the save had to combine two instances' changes.
        merged: bool,
    },
    /// The work failed. The options come back so the handle is never left
    /// without them.
    Failed {
        /// The options the job was given, where it had them.
        options: Option<Box<ProgramOptions>>,
        /// What went wrong.
        reason: String,
    },
}

impl Terminal for OptionsMessage {
    fn is_terminal(&self) -> bool {
        true
    }

    fn cancelled() -> Self {
        OptionsMessage::Failed {
            options: None,
            reason: "reading the options document stopped".to_owned(),
        }
    }

    fn panicked(detail: String) -> Self {
        OptionsMessage::Failed {
            options: None,
            reason: detail,
        }
    }
}

/// What a handle is doing right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionsState {
    /// The first read has not finished.
    Loading,
    /// The document is in hand and can be edited.
    Ready,
    /// A save is in flight, so the document is with the job.
    Saving,
}

/// The options document and the jobs that read and write it.
pub struct OptionsHandle {
    path: PathBuf,
    options: Option<ProgramOptions>,
    /// A copy of what a running save holds.
    ///
    /// A save takes the document, so without this copy a reader between the
    /// start of a save and its end sees nothing and falls back to the built-in
    /// set. Every frame painted in that window would then drop the colors and
    /// the shortcuts a person had just chosen.
    in_flight: Option<ProgramOptions>,
    job: Option<Job<OptionsMessage>>,
    state: OptionsState,
    notice: Option<String>,
    notify: Arc<dyn Fn() + Send + Sync>,
    save_again: bool,
    /// The instant a close stops waiting for a save in flight, where the
    /// exit set one.
    close_deadline: Option<Instant>,
}

impl OptionsHandle {
    /// Starts reading the document of the settings directory the environment
    /// names.
    #[must_use]
    pub fn open(notify: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self::open_in(crate::paths::settings_directory(), notify)
    }

    /// Starts reading the document of a stated directory.
    #[must_use]
    pub fn open_in(directory: PathBuf, notify: Arc<dyn Fn() + Send + Sync>) -> Self {
        let paths = SettingsPaths::at(ca_session::SettingsDirectory::PerUser(directory));
        let path = paths.options_file();
        let job = spawn_load(path.clone(), Arc::clone(&notify));
        Self {
            path,
            options: None,
            in_flight: None,
            job: Some(job),
            state: OptionsState::Loading,
            notice: None,
            notify,
            save_again: false,
            close_deadline: None,
        }
    }

    /// Where the document is read from and written to.
    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// What the handle is doing.
    #[must_use]
    pub fn state(&self) -> OptionsState {
        self.state
    }

    /// True once the document can be read and edited.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.state == OptionsState::Ready && self.options.is_some()
    }

    /// The options, once they have been read.
    #[must_use]
    pub fn options(&self) -> Option<&ProgramOptions> {
        self.options.as_ref().or(self.in_flight.as_ref())
    }

    /// The options, or the built-in set while the read is still running.
    #[must_use]
    pub fn options_or_default(&self) -> ProgramOptions {
        self.options().cloned().unwrap_or_default()
    }

    /// The options for editing.
    pub fn options_mut(&mut self) -> Option<&mut ProgramOptions> {
        self.options.as_mut()
    }

    /// Replace the options and write them.
    pub fn replace(&mut self, options: ProgramOptions) {
        match self.options.as_mut() {
            Some(held) => held.adopt(options),
            None => self.options = Some(options),
        }
        self.save();
    }

    /// Whatever the handle has to report that has nowhere else to appear.
    #[must_use]
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// Drop the notice once it has been shown.
    pub fn clear_notice(&mut self) {
        self.notice = None;
    }

    /// Start writing the document.
    ///
    /// A save asked for while one is running is remembered and started when
    /// that one finishes, so an edit is never left unwritten.
    pub fn save(&mut self) {
        if self.state == OptionsState::Saving || self.state == OptionsState::Loading {
            self.save_again = true;
            return;
        }
        let Some(options) = self.options.take() else {
            self.save_again = true;
            return;
        };
        self.in_flight = Some(options.clone());
        self.state = OptionsState::Saving;
        self.job = Some(spawn_save(
            self.path.clone(),
            Box::new(options),
            Arc::clone(&self.notify),
        ));
    }

    /// Take whatever the worker has posted. Never blocks.
    pub fn poll(&mut self) {
        let Some(job) = self.job.as_mut() else {
            return;
        };
        let messages = job.drain();
        let finished = job.is_finished();
        for message in messages {
            self.apply(message);
        }
        if finished {
            self.job = None;
        }
        if self.save_again && self.state == OptionsState::Ready {
            self.save_again = false;
            self.save();
        }
    }

    /// Take the document a finished job returns, keeping an edit made while it
    /// ran.
    ///
    /// A save takes the document with it, so an edit that arrives in the
    /// meantime is held here. Assigning the returned copy whole would discard
    /// that edit; the returned copy is kept only for the record of which file
    /// it matches, which the next save needs.
    fn take_back(&mut self, returned: ProgramOptions) {
        let mut returned = returned;
        if let Some(held) = self.options.take() {
            returned.adopt(held);
        }
        self.options = Some(returned);
    }

    fn apply(&mut self, message: OptionsMessage) {
        self.in_flight = None;
        match message {
            OptionsMessage::Loaded { options, notice } => {
                self.options = Some(*options);
                self.state = OptionsState::Ready;
                if notice.is_some() {
                    self.notice = notice;
                }
            }
            OptionsMessage::Saved { options, merged } => {
                self.take_back(*options);
                self.state = OptionsState::Ready;
                if merged {
                    self.notice = Some(
                        "Another instance had written the options document. The two sets of changes were combined."
                            .to_owned(),
                    );
                }
            }
            OptionsMessage::Failed { options, reason } => {
                if let Some(options) = options {
                    self.take_back(*options);
                }
                if self.options.is_none() {
                    self.options = Some(ProgramOptions::default());
                }
                self.state = OptionsState::Ready;
                self.notice = Some(reason);
            }
        }
    }

    /// Complete every write asked for, on the calling thread.
    ///
    /// For the exit path: a save in flight lands first, bounded by `limit`,
    /// and a save asked for while it ran is written here, because no later
    /// frame starts it and a worker still running when the process ends dies
    /// with it.
    pub fn finish(&mut self, limit: Duration) {
        self.wait(limit);
        if !self.save_again || self.state != OptionsState::Ready {
            return;
        }
        let Some(options) = self.options.as_mut() else {
            return;
        };
        self.save_again = false;
        match options.save_merging(&self.path) {
            Ok(merged) => {
                if merged {
                    self.notice = Some(
                        "Another instance had written the options document. The two sets of changes were combined."
                            .to_owned(),
                    );
                }
            }
            Err(error) => {
                self.notice = Some(format!(
                    "The options document could not be written: {error}"
                ));
            }
        }
    }

    /// Block until the running job has landed, or until `limit` passes.
    fn wait(&mut self, limit: Duration) {
        let Some(job) = self.job.as_mut() else {
            return;
        };
        let messages = job.wait(limit);
        let finished = job.is_finished();
        for message in messages {
            self.apply(message);
        }
        if finished {
            self.job = None;
        }
    }

    /// Put a save that never ends in flight, for a test of how long a close
    /// waits.
    pub(crate) fn stall_save(&mut self) {
        self.state = OptionsState::Saving;
        self.job = Some(Job::spawn(|_, cancel| {
            while !cancel.is_cancelled() {
                std::thread::sleep(Duration::from_millis(5));
            }
        }));
    }

    /// End the wait of a later close for a save in flight at `deadline`.
    ///
    /// The exit spends one bound on every wait for a save in flight, and the
    /// handle closes after the exit has spent part of it.
    pub fn close_by(&mut self, deadline: Instant) {
        self.close_deadline = Some(deadline);
    }

    /// Stop the worker.
    ///
    /// A save in flight lands first, bounded by
    /// [`crate::sessions::CLOSE_WAIT`] or by the deadline
    /// [`OptionsHandle::close_by`] set.
    pub fn close(&mut self) {
        if self.state == OptionsState::Saving {
            self.wait(crate::sessions::close_wait(self.close_deadline));
        }
        if let Some(job) = self.job.take() {
            job.cancel();
        }
    }
}

impl Drop for OptionsHandle {
    fn drop(&mut self) {
        self.close();
    }
}

fn spawn_load(path: PathBuf, notify: Arc<dyn Fn() + Send + Sync>) -> Job<OptionsMessage> {
    Job::spawn_notifying(
        move |emitter, _| {
            let message = match ProgramOptions::load(&path) {
                Ok(outcome) => {
                    let mut parts = Vec::new();
                    if let Some(backup) = &outcome.recovered_backup {
                        parts.push(format!(
                            "The options document could not be read and was moved to {}. The built-in options are in use.",
                            backup.display()
                        ));
                    }
                    if let Some(version) = outcome.newer_schema {
                        parts.push(format!(
                            "The options document was written by a newer build, schema {version}. Everything in it is kept."
                        ));
                    }
                    OptionsMessage::Loaded {
                        options: outcome.options,
                        notice: (!parts.is_empty()).then(|| parts.join(" ")),
                    }
                }
                Err(error) => OptionsMessage::Failed {
                    options: Some(Box::new(ProgramOptions::default())),
                    reason: format!("The options document could not be read: {error}"),
                },
            };
            emitter.send(message);
        },
        notify,
    )
}

fn spawn_save(
    path: PathBuf,
    mut options: Box<ProgramOptions>,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<OptionsMessage> {
    Job::spawn_notifying(
        move |emitter, _| {
            let message = match options.save_merging(&path) {
                Ok(merged) => OptionsMessage::Saved { options, merged },
                Err(error) => OptionsMessage::Failed {
                    options: Some(options),
                    reason: format!("The options document could not be written: {error}"),
                },
            };
            emitter.send(message);
        },
        notify,
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::{OptionsHandle, OptionsState};
    use std::sync::Arc;
    use std::time::Duration;

    fn settle(handle: &mut OptionsHandle) {
        assert!(
            crate::testing::wait_until(Duration::from_secs(10), || {
                handle.poll();
                handle.state() == OptionsState::Ready
            }),
            "the options worker never answered"
        );
    }

    fn handle(directory: &std::path::Path) -> OptionsHandle {
        OptionsHandle::open_in(directory.to_path_buf(), Arc::new(|| {}))
    }

    #[test]
    fn an_empty_directory_yields_the_built_in_options() {
        let dir = tempfile::tempdir().unwrap();
        let mut handle = handle(dir.path());
        assert_eq!(handle.state(), OptionsState::Loading);
        settle(&mut handle);
        assert!(handle.is_ready());
        assert_eq!(handle.options().unwrap().text_editing.tab_stop, 8);
    }

    #[test]
    fn an_edit_reads_back_from_a_second_handle() {
        let dir = tempfile::tempdir().unwrap();
        let mut first = handle(dir.path());
        settle(&mut first);
        first.options_mut().unwrap().text_editing.tab_stop = 4;
        first.save();
        settle(&mut first);
        first.close();

        let mut second = handle(dir.path());
        settle(&mut second);
        assert_eq!(second.options().unwrap().text_editing.tab_stop, 4);
    }

    /// A save holds the document, so a reader during one has to be answered
    /// with what is being written. A frame painted here otherwise drops back to
    /// the built-in colors and shortcuts.
    #[test]
    fn a_read_during_a_save_still_reports_the_edit() {
        let dir = tempfile::tempdir().unwrap();
        let mut handle = handle(dir.path());
        settle(&mut handle);
        let mut edited = handle.options_or_default();
        edited.text_editing.tab_stop = 3;
        handle.replace(edited);
        assert_eq!(handle.state(), OptionsState::Saving);
        assert_eq!(handle.options().unwrap().text_editing.tab_stop, 3);
        assert_eq!(handle.options_or_default().text_editing.tab_stop, 3);
        settle(&mut handle);
        assert_eq!(handle.options().unwrap().text_editing.tab_stop, 3);
    }

    /// A save takes the document with it. An edit made before that save reports
    /// has to survive the report, or the next frame paints what was replaced.
    #[test]
    fn an_edit_made_during_a_save_survives_the_save_finishing() {
        let dir = tempfile::tempdir().unwrap();
        let mut handle = handle(dir.path());
        settle(&mut handle);

        let mut first = handle.options_or_default();
        first.text_editing.tab_stop = 2;
        handle.replace(first);
        assert_eq!(handle.state(), OptionsState::Saving);

        let mut second = handle.options_or_default();
        second.text_editing.tab_stop = 6;
        handle.replace(second);

        settle(&mut handle);
        assert_eq!(handle.options().unwrap().text_editing.tab_stop, 6);
        handle.close();

        let mut again = super::tests::handle(dir.path());
        settle(&mut again);
        assert_eq!(again.options().unwrap().text_editing.tab_stop, 6);
    }

    #[test]
    fn two_instances_saving_in_turn_keep_the_later_edit() {
        let dir = tempfile::tempdir().unwrap();
        let mut first = handle(dir.path());
        let mut second = handle(dir.path());
        settle(&mut first);
        settle(&mut second);

        first.options_mut().unwrap().tweaks.use_ipv6 = false;
        first.save();
        settle(&mut first);

        second.options_mut().unwrap().text_editing.tab_stop = 2;
        second.save();
        settle(&mut second);
        assert!(second.notice().is_some(), "the merge was not reported");

        let mut third = handle(dir.path());
        settle(&mut third);
        assert_eq!(third.options().unwrap().text_editing.tab_stop, 2);
    }

    #[test]
    fn a_save_asked_for_during_a_save_still_happens() {
        let dir = tempfile::tempdir().unwrap();
        let mut handle = handle(dir.path());
        settle(&mut handle);
        handle.options_mut().unwrap().text_editing.tab_stop = 5;
        handle.save();
        handle.save();
        assert!(crate::testing::wait_until(Duration::from_secs(10), || {
            handle.poll();
            handle.state() == OptionsState::Ready && handle.job.is_none()
        }));
        handle.close();

        let mut second = super::tests::handle(dir.path());
        settle(&mut second);
        assert_eq!(second.options().unwrap().text_editing.tab_stop, 5);
    }
}
