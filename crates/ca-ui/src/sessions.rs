//! The one stored session document, read and written on a worker.
//!
//! Loading and saving both touch the disk, so neither runs on the frame thread.
//! The handle here owns the store between jobs and hands it to the job that is
//! writing it, so a save and an edit can never run over each other.
//!
//! A second instance of the program finds the settings directory already
//! claimed. It opens the document read-mostly: it still edits and still saves,
//! but a save that finds the document replaced reads it again and combines the
//! two sets of changes rather than dropping either.

use crate::worker::{Job, Terminal};
use ca_session::{
    LockOutcome, SaveOutcome, SessionStore, SettingsLock, SettingsPaths,
    SETTINGS_DIRECTORY_VARIABLE,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Longest time a closing handle waits for a save in flight before it lets
/// the settings directory go.
pub const CLOSE_WAIT: Duration = Duration::from_secs(5);

/// How long a close waits for a save in flight: [`CLOSE_WAIT`], or less when
/// `deadline` comes first.
pub(crate) fn close_wait(deadline: Option<Instant>) -> Duration {
    deadline.map_or(CLOSE_WAIT, |deadline| {
        deadline
            .saturating_duration_since(Instant::now())
            .min(CLOSE_WAIT)
    })
}

/// What the store worker posts back.
pub enum StoreMessage {
    /// The document was read.
    Loaded {
        /// The store, empty when no document existed.
        store: Box<SessionStore>,
        /// The claim on the settings directory, absent when another instance
        /// holds it or the directory cannot be written.
        lock: Option<SettingsLock>,
        /// True when another instance holds the settings directory.
        read_mostly: bool,
        /// Something about the load a person has to be told.
        notice: Option<String>,
    },
    /// The document was written.
    Saved {
        /// The store, with the document it now matches.
        store: Box<SessionStore>,
        /// What the save had to do to keep another writer's changes.
        outcome: SaveOutcome,
    },
    /// The work failed. The store comes back so the handle is never left
    /// without one.
    Failed {
        /// The store the job was given, where it had one.
        store: Option<Box<SessionStore>>,
        /// What went wrong.
        reason: String,
    },
}

impl Terminal for StoreMessage {
    fn is_terminal(&self) -> bool {
        true
    }

    fn cancelled() -> Self {
        StoreMessage::Failed {
            store: None,
            reason: "reading the sessions document stopped".to_owned(),
        }
    }

    fn panicked(detail: String) -> Self {
        StoreMessage::Failed {
            store: None,
            reason: detail,
        }
    }
}

/// What a handle is doing right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreState {
    /// The first read has not finished.
    Loading,
    /// The store is in hand and can be edited.
    Ready,
    /// A save is in flight, so the store is with the job.
    Saving,
}

/// The stored session document and the jobs that read and write it.
pub struct StoreHandle {
    paths: SettingsPaths,
    store: Option<SessionStore>,
    job: Option<Job<StoreMessage>>,
    state: StoreState,
    lock: Option<SettingsLock>,
    read_mostly: bool,
    notice: Option<String>,
    last_save: Option<SaveOutcome>,
    notify: Arc<dyn Fn() + Send + Sync>,
    /// A save asked for while one was already running.
    save_again: bool,
    /// The instant a close stops waiting for a save in flight, where the
    /// exit set one.
    close_deadline: Option<Instant>,
}

impl StoreHandle {
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
        let job = spawn_load(paths.clone(), Arc::clone(&notify));
        Self {
            paths,
            store: None,
            job: Some(job),
            state: StoreState::Loading,
            lock: None,
            read_mostly: false,
            notice: None,
            last_save: None,
            notify,
            save_again: false,
            close_deadline: None,
        }
    }

    /// The name of the variable that moves the settings directory, for a
    /// caller reporting where it read from.
    #[must_use]
    pub fn directory_variable() -> &'static str {
        SETTINGS_DIRECTORY_VARIABLE
    }

    /// Where the document is read from and written to.
    #[must_use]
    pub fn paths(&self) -> &SettingsPaths {
        &self.paths
    }

    /// What the handle is doing.
    #[must_use]
    pub fn state(&self) -> StoreState {
        self.state
    }

    /// True once the store can be read and edited.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.state == StoreState::Ready && self.store.is_some()
    }

    /// True when another instance holds the settings directory, so this one
    /// combines its changes with that instance's on every save.
    #[must_use]
    pub fn is_read_mostly(&self) -> bool {
        self.read_mostly
    }

    /// The store, once it has been read.
    #[must_use]
    pub fn store(&self) -> Option<&SessionStore> {
        self.store.as_ref()
    }

    /// The store for editing, once it has been read and while no save holds it.
    pub fn store_mut(&mut self) -> Option<&mut SessionStore> {
        self.store.as_mut()
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

    /// What the last save had to do.
    #[must_use]
    pub fn last_save(&self) -> Option<SaveOutcome> {
        self.last_save
    }

    /// Start writing the document.
    ///
    /// A save asked for while one is running is remembered and started when
    /// that one finishes, so an edit is never left unwritten.
    pub fn save(&mut self) {
        if self.state == StoreState::Saving || self.state == StoreState::Loading {
            self.save_again = true;
            return;
        }
        let Some(store) = self.store.take() else {
            self.save_again = true;
            return;
        };
        self.state = StoreState::Saving;
        self.job = Some(spawn_save(
            self.paths.sessions_file(),
            Box::new(store),
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
        if self.save_again && self.state == StoreState::Ready {
            self.save_again = false;
            self.save();
        }
    }

    fn apply(&mut self, message: StoreMessage) {
        match message {
            StoreMessage::Loaded {
                store,
                lock,
                read_mostly,
                notice,
            } => {
                self.read_mostly = read_mostly;
                self.lock = lock;
                self.store = Some(*store);
                self.state = StoreState::Ready;
                if notice.is_some() {
                    self.notice = notice;
                }
            }
            StoreMessage::Saved { store, outcome } => {
                self.store = Some(*store);
                self.state = StoreState::Ready;
                self.last_save = Some(outcome);
                if outcome.merged {
                    self.notice = Some(merge_notice(outcome));
                }
            }
            StoreMessage::Failed { store, reason } => {
                if let Some(store) = store {
                    self.store = Some(*store);
                }
                if self.store.is_none() {
                    self.store = Some(SessionStore::default());
                }
                self.state = StoreState::Ready;
                self.notice = Some(reason);
            }
        }
    }

    /// Block until the running job has landed, or until `limit` passes.
    ///
    /// For the exit path: a save in flight holds the store, so an edit made
    /// before it lands has nowhere to go.
    pub fn wait(&mut self, limit: Duration) {
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

    /// Write the document on the calling thread, combining it with another
    /// writer's document as a worker save does.
    ///
    /// For the exit path: no later frame starts the write a worker would
    /// leave behind, and a worker still running when the process ends dies
    /// with it. Returns true when the document was written.
    pub fn save_now(&mut self) -> bool {
        if self.state != StoreState::Ready {
            return false;
        }
        let path = self.paths.sessions_file();
        let Some(store) = self.store.as_mut() else {
            return false;
        };
        self.save_again = false;
        match store.save_merging(&path) {
            Ok(outcome) => {
                self.last_save = Some(outcome);
                if outcome.merged {
                    self.notice = Some(merge_notice(outcome));
                }
                true
            }
            Err(error) => {
                self.notice = Some(format!(
                    "The sessions document could not be written: {error}"
                ));
                false
            }
        }
    }

    /// Put a save that never ends in flight, for a test of how long a close
    /// waits.
    pub(crate) fn stall_save(&mut self) {
        self.state = StoreState::Saving;
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

    /// Release the claim on the settings directory and stop the worker.
    ///
    /// A save in flight lands first, bounded by [`CLOSE_WAIT`] or by the
    /// deadline [`StoreHandle::close_by`] set, so an instance that takes the
    /// claim next never reads over a write of this one.
    pub fn close(&mut self) {
        if self.state == StoreState::Saving {
            self.wait(close_wait(self.close_deadline));
        }
        if let Some(job) = self.job.take() {
            job.cancel();
        }
        if let Some(lock) = self.lock.take() {
            lock.release();
        }
    }
}

impl Drop for StoreHandle {
    fn drop(&mut self) {
        self.close();
    }
}

/// The line shown when a save had to combine two instances' changes.
fn merge_notice(outcome: SaveOutcome) -> String {
    format!(
        "Another instance had written the sessions document. The two sets of changes were combined: {} sessions and {} workspaces were kept.",
        outcome.nodes_kept, outcome.workspaces_kept
    )
}

/// Reads the document and claims the settings directory, on a worker.
fn spawn_load(paths: SettingsPaths, notify: Arc<dyn Fn() + Send + Sync>) -> Job<StoreMessage> {
    Job::spawn_notifying(
        move |emitter, _| {
            let directory = paths.directory().path();
            let (lock, claim) = match SettingsLock::acquire(directory) {
                Ok(LockOutcome::Acquired(lock)) => (Some(lock), Claim::Taken),
                Ok(LockOutcome::Held { .. }) => (None, Claim::HeldElsewhere),
                Err(error) => (
                    None,
                    Claim::Unwritable(unwritable_notice(directory, &error)),
                ),
            };
            let announced = crate::paths::settings_notice_for(directory);
            let message = match SessionStore::load(&paths.sessions_file()) {
                Ok(outcome) => {
                    let read_mostly = claim == Claim::HeldElsewhere;
                    let notice = load_notice(announced, &outcome, claim);
                    StoreMessage::Loaded {
                        store: Box::new(outcome.store),
                        lock,
                        read_mostly,
                        notice,
                    }
                }
                Err(error) => {
                    let mut parts: Vec<String> = announced.into_iter().map(str::to_owned).collect();
                    if let Claim::Unwritable(text) = claim {
                        parts.push(text);
                    }
                    parts.push(format!("The sessions document could not be read: {error}"));
                    StoreMessage::Failed {
                        store: Some(Box::new(SessionStore::default())),
                        reason: parts.join(" "),
                    }
                }
            };
            emitter.send(message);
        },
        notify,
    )
}

/// How the claim on the settings directory went.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Claim {
    /// This instance holds it.
    Taken,
    /// Another instance holds it.
    HeldElsewhere,
    /// The folder or its lock file cannot be created or written; the text
    /// says which and why.
    Unwritable(String),
}

/// The line shown when the settings directory cannot be claimed because it
/// cannot be created or written.
fn unwritable_notice(directory: &std::path::Path, error: &ca_session::Error) -> String {
    let cause = match error {
        ca_session::Error::Io { path, source } if path == directory => source.to_string(),
        ca_session::Error::Io { path, source } => format!("{}: {source}", path.display()),
        other => other.to_string(),
    };
    format!(
        "The settings folder {} cannot be created or written ({cause}). Settings and sessions are not saved.",
        directory.display()
    )
}

/// What a load has to report, where it has anything. `announced` is what the
/// settings directory itself has to say, shown first.
fn load_notice(
    announced: Option<&str>,
    outcome: &ca_session::LoadOutcome,
    claim: Claim,
) -> Option<String> {
    let mut parts: Vec<String> = announced.into_iter().map(str::to_owned).collect();
    if let Some(backup) = &outcome.recovered_backup {
        parts.push(format!(
            "The sessions document could not be read and was moved to {}. An empty one is in use.",
            backup.display()
        ));
    }
    if let Some(version) = outcome.newer_schema {
        parts.push(format!(
            "The sessions document was written by a newer build, schema {version}. Everything in it is kept."
        ));
    }
    if !outcome.repairs.is_clean() {
        parts.push(format!(
            "The sessions document was repaired: {} duplicate identifiers and {} settings of the wrong type.",
            outcome.repairs.duplicate_ids, outcome.repairs.kinds_corrected
        ));
    }
    match claim {
        Claim::Taken => {}
        Claim::HeldElsewhere => parts.push(
            "Another instance holds the settings directory. Changes made here are combined with that instance's when they are saved."
                .to_owned(),
        ),
        Claim::Unwritable(text) => parts.push(text),
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// Writes the document on a worker, combining rather than overwriting.
fn spawn_save(
    path: PathBuf,
    mut store: Box<SessionStore>,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<StoreMessage> {
    Job::spawn_notifying(
        move |emitter, _| {
            let message = match store.save_merging(&path) {
                Ok(outcome) => StoreMessage::Saved { store, outcome },
                Err(error) => StoreMessage::Failed {
                    store: Some(store),
                    reason: format!("The sessions document could not be written: {error}"),
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
    use super::{StoreHandle, StoreState};
    use ca_session::{SavedSession, SessionId, SessionKind, SessionStore, TreeNode};
    use std::sync::Arc;
    use std::time::Duration;

    fn settle(handle: &mut StoreHandle) {
        assert!(
            crate::testing::wait_until(Duration::from_secs(10), || {
                handle.poll();
                handle.state() == StoreState::Ready
            }),
            "the store worker never answered"
        );
    }

    fn handle(directory: &std::path::Path) -> StoreHandle {
        StoreHandle::open_in(directory.to_path_buf(), Arc::new(|| {}))
    }

    #[test]
    fn an_empty_directory_yields_an_empty_store() {
        let dir = tempfile::tempdir().unwrap();
        let mut handle = handle(dir.path());
        assert_eq!(handle.state(), StoreState::Loading);
        settle(&mut handle);
        assert!(handle.is_ready());
        assert!(handle.store().unwrap().root.is_empty());
        assert!(!handle.is_read_mostly());
    }

    #[test]
    fn a_saved_session_reads_back_from_a_second_handle() {
        let dir = tempfile::tempdir().unwrap();
        let mut first = handle(dir.path());
        settle(&mut first);
        let store = first.store_mut().unwrap();
        let id = store.next_id();
        store
            .add_session(
                None,
                SavedSession::new(id, "nightly", SessionKind::TextCompare),
            )
            .unwrap();
        first.save();
        settle(&mut first);
        first.close();

        let mut second = handle(dir.path());
        settle(&mut second);
        assert_eq!(second.store().unwrap().sessions().len(), 1);
    }

    #[test]
    fn a_second_instance_opens_read_mostly_while_the_first_holds_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let mut first = handle(dir.path());
        settle(&mut first);
        assert!(!first.is_read_mostly());

        let mut second = handle(dir.path());
        settle(&mut second);
        assert!(second.is_read_mostly());
        assert!(
            second.notice().is_some(),
            "the second instance says nothing"
        );
    }

    /// A folder under a regular file cannot be created on any platform, and
    /// even a privileged user meets the same refusal there.
    #[test]
    fn a_settings_folder_that_cannot_be_created_is_named_as_the_cause() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not-a-folder");
        std::fs::write(&file, b"x").unwrap();
        let settings = file.join("compare-all");
        let mut handle = handle(&settings);
        settle(&mut handle);
        let notice = handle.notice().unwrap_or_default().to_owned();
        assert!(!notice.contains("Another instance"), "{notice}");
        assert!(notice.contains("cannot be created or written"), "{notice}");
        assert!(notice.contains(&settings.display().to_string()), "{notice}");
        assert!(!handle.is_read_mostly());
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_home_is_named_as_the_cause() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
        let settings = home.path().join(".config").join("compare-all");
        let writable = std::fs::create_dir(home.path().join("probe")).is_ok();
        if !writable {
            let mut handle = handle(&settings);
            settle(&mut handle);
            let notice = handle.notice().unwrap_or_default().to_owned();
            assert!(!notice.contains("Another instance"), "{notice}");
            assert!(notice.contains("cannot be created or written"), "{notice}");
        }
        std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Neither instance's work is dropped: a save that meets a replaced
    /// document reads it again and keeps both sets of changes.
    #[test]
    fn two_instances_saving_in_turn_keep_both_sets_of_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let mut first = handle(dir.path());
        let mut second = handle(dir.path());
        settle(&mut first);
        settle(&mut second);

        let add = |handle: &mut StoreHandle, name: &str| {
            let store = handle.store_mut().unwrap();
            let id = store.next_id();
            store
                .add_session(None, SavedSession::new(id, name, SessionKind::TextCompare))
                .unwrap();
        };
        add(&mut first, "from the first");
        add(&mut second, "from the second");

        first.save();
        settle(&mut first);
        second.save();
        settle(&mut second);
        assert!(
            second.last_save().unwrap().merged,
            "the second save did not notice the replaced document"
        );

        let names: Vec<String> = second
            .store()
            .unwrap()
            .sessions()
            .iter()
            .map(|session| session.name.clone())
            .collect();
        assert!(names.contains(&"from the first".to_owned()), "{names:?}");
        assert!(names.contains(&"from the second".to_owned()), "{names:?}");
    }

    /// A deletion made here is not undone by the merge, and an addition made
    /// elsewhere is still kept.
    #[test]
    fn a_merge_keeps_a_deletion_and_an_addition_apart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(ca_session::store::SESSIONS_FILE);
        let mut seed = SessionStore::default();
        let kept = seed.next_id();
        seed.add_session(
            None,
            SavedSession::new(kept.clone(), "shared", SessionKind::TextCompare),
        )
        .unwrap();
        seed.save_replacing(&path).unwrap();

        let mut first = handle(dir.path());
        let mut second = handle(dir.path());
        settle(&mut first);
        settle(&mut second);

        // The first instance adds one; the second deletes the shared one.
        let store = first.store_mut().unwrap();
        let id = store.next_id();
        store
            .add_session(
                None,
                SavedSession::new(id, "added", SessionKind::HexCompare),
            )
            .unwrap();
        second.store_mut().unwrap().delete(&kept).unwrap();

        first.save();
        settle(&mut first);
        second.save();
        settle(&mut second);

        let names: Vec<String> = second
            .store()
            .unwrap()
            .sessions()
            .iter()
            .map(|session| session.name.clone())
            .collect();
        assert_eq!(names, vec!["added".to_owned()], "{names:?}");
    }

    #[test]
    fn automatic_saving_keeps_the_documented_number_of_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let mut handle = handle(dir.path());
        settle(&mut handle);
        let store = handle.store_mut().unwrap();
        store.set_max_auto_saved(3);
        for index in 0..5 {
            store.record_auto_saved(SavedSession::new(
                SessionId::from_raw(format!("auto{index}")),
                format!("session {index}"),
                SessionKind::TextCompare,
            ));
        }
        assert_eq!(store.auto_saved.len(), 3);
        assert_eq!(store.auto_saved[0].name, "session 4", "newest is first");
        store.set_max_auto_saved(0);
        assert!(store.auto_saved.is_empty(), "a cap of zero holds nothing");
    }

    #[test]
    fn a_save_asked_for_during_a_save_still_happens() {
        let dir = tempfile::tempdir().unwrap();
        let mut handle = handle(dir.path());
        settle(&mut handle);
        handle.store_mut().unwrap().max_auto_saved = 4;
        handle.save();
        // The store is with the job, so this one is remembered instead.
        handle.save();
        settle(&mut handle);
        handle.poll();
        assert!(crate::testing::wait_until(Duration::from_secs(10), || {
            handle.poll();
            handle.state() == StoreState::Ready && handle.job.is_none()
        }));
        handle.close();

        let mut second = handle_of(dir.path());
        settle(&mut second);
        assert_eq!(second.store().unwrap().max_auto_saved, 4);
    }

    fn handle_of(directory: &std::path::Path) -> StoreHandle {
        handle(directory)
    }

    #[test]
    fn a_node_added_under_a_folder_by_another_instance_lands_in_that_folder() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(ca_session::store::SESSIONS_FILE);
        let mut seed = SessionStore::default();
        let folder = seed.create_folder(None, "Group").unwrap();
        seed.save_replacing(&path).unwrap();

        let mut first = handle(dir.path());
        let mut second = handle(dir.path());
        settle(&mut first);
        settle(&mut second);

        let store = first.store_mut().unwrap();
        let id = store.next_id();
        store
            .add_session(
                Some(&folder),
                SavedSession::new(id, "inner", SessionKind::TextCompare),
            )
            .unwrap();
        first.save();
        settle(&mut first);

        second.store_mut().unwrap().max_auto_saved = 9;
        second.save();
        settle(&mut second);

        let Some(TreeNode::Folder { children, .. }) = second.store().unwrap().find(&folder) else {
            panic!("the folder is gone");
        };
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].name(), "inner");
    }
}
