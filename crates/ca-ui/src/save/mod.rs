//! Writing bytes back to a file without losing what was there.
//!
//! Two rules shape everything here, because each of them is a way a user loses
//! work:
//!
//! - a detected size/time change on disk requires consent before writing;
//! - the bytes go to a temporary file in the same folder before replacement,
//!   so a write or sync failure leaves the original file intact.
//!
//! The preflight check is not atomic with replacement; a concurrent writer can
//! change the file after the check. Replacement uses `ca-io`.
//!
//! The file system is reached through [`FileSystem`], so both paths are
//! testable without touching a disk.
//!
//! Producing the bytes belongs to the caller. A view whose encode can fail
//! reports [`SaveOutcome::WouldLose`] itself and never reaches this module.

pub mod backup;
pub mod text;

use crate::worker::{Job, Terminal};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

/// What a file looked like the last time it was read or written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    /// Size in bytes.
    pub size: u64,
    /// Last modification time, where the platform reports one.
    pub modified: Option<SystemTime>,
    /// Volume/device and file identity, where the platform reports both.
    pub identity: Option<(u64, u64)>,
}

/// What the target looked like when the caller last knew its state.
///
/// A target the caller never read is [`Baseline::Unchecked`], which skips the
/// changed-on-disk check. A target that did not exist is [`Baseline::Absent`]:
/// a file that appears under that name after the baseline was taken is a
/// change, so it is reported rather than overwritten.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Baseline {
    /// The caller does not know what was there, so nothing is checked.
    #[default]
    Unchecked,
    /// Nothing existed under that name.
    Absent,
    /// The target looked like this.
    Present(Stamp),
}

impl Baseline {
    /// The baseline of `path` as it is now.
    pub fn of(files: &dyn FileSystem, path: &Path) -> Self {
        files.stamp(path).map_or(Self::Absent, Self::Present)
    }
}

impl From<Option<Stamp>> for Baseline {
    /// A stamp that was taken becomes [`Baseline::Present`]. The absence of a
    /// stamp says only that the caller has none, so it becomes
    /// [`Baseline::Unchecked`].
    fn from(stamp: Option<Stamp>) -> Self {
        stamp.map_or(Self::Unchecked, Self::Present)
    }
}

/// The file system operations a save needs.
pub trait FileSystem: Send + Sync {
    /// The size, modification time and available identity of a file, or
    /// `None` when it is absent.
    fn stamp(&self, path: &Path) -> Option<Stamp>;

    /// True when the target can be replaced.
    fn is_writable(&self, path: &Path) -> bool;

    /// Replace `target` with `bytes`, preserving its contents on failure.
    ///
    /// # Errors
    ///
    /// Returns the underlying error when preparation or replacement fails.
    fn replace(&self, target: &Path, bytes: &[u8]) -> io::Result<()>;

    /// Recheck the expected disk state immediately before replacement.
    /// Real file systems perform the check after writing the temporary file.
    ///
    /// # Errors
    /// Returns [`SaveOutcome::ChangedOnDisk`] if the target differs, or
    /// [`SaveOutcome::Failed`] if preparation or replacement fails.
    fn replace_checked(
        &self,
        target: &Path,
        bytes: &[u8],
        expected: Baseline,
        accept_disk_change: bool,
    ) -> Result<(), SaveOutcome> {
        if disk_changed(self.stamp(target), expected, accept_disk_change) {
            return Err(SaveOutcome::ChangedOnDisk);
        }
        self.replace(target, bytes).map_err(SaveOutcome::from)
    }

    /// Replace and return a retained displaced file if a concurrent change
    /// was detected after the final check.
    ///
    /// # Errors
    /// Returns a changed-on-disk or write failure outcome before commit.
    fn replace_checked_with_backup(
        &self,
        target: &Path,
        bytes: &[u8],
        expected: Baseline,
        accept_disk_change: bool,
    ) -> Result<Option<PathBuf>, SaveOutcome> {
        self.replace_checked(target, bytes, expected, accept_disk_change)
            .map(|()| None)
    }
}

/// The real file system.
#[derive(Debug, Default, Clone, Copy)]
pub struct RealFileSystem;

impl FileSystem for RealFileSystem {
    fn stamp(&self, path: &Path) -> Option<Stamp> {
        #[cfg(windows)]
        let (data, identity) = match winapi_util::Handle::from_path(path) {
            Ok(handle) => {
                let data = handle.as_file().metadata().ok()?;
                let identity = winapi_util::file::information(&handle)
                    .ok()
                    .map(|info| (info.volume_serial_number(), info.file_index()));
                (data, identity)
            }
            Err(_) => (std::fs::metadata(path).ok()?, None),
        };
        #[cfg(not(windows))]
        let data = std::fs::metadata(path).ok()?;
        #[cfg(unix)]
        let identity = {
            use std::os::unix::fs::MetadataExt;
            Some((data.dev(), data.ino()))
        };
        #[cfg(not(any(windows, unix)))]
        let identity = None;
        Some(Stamp {
            size: data.len(),
            modified: data.modified().ok(),
            identity,
        })
    }

    fn is_writable(&self, path: &Path) -> bool {
        match std::fs::metadata(path) {
            Ok(data) => !data.permissions().readonly(),
            // A target that does not exist yet is writable when its folder is.
            Err(_) => path.parent().is_none_or(Path::is_dir),
        }
    }

    fn replace(&self, target: &Path, bytes: &[u8]) -> io::Result<()> {
        ca_io::write_atomic(target, bytes)
    }

    fn replace_checked(
        &self,
        target: &Path,
        bytes: &[u8],
        expected: Baseline,
        accept_disk_change: bool,
    ) -> Result<(), SaveOutcome> {
        let produce = |writer: &mut dyn io::Write| -> Result<(), SaveOutcome> {
            writer.write_all(bytes)?;
            Ok(())
        };
        let check = || {
            if disk_changed(self.stamp(target), expected, accept_disk_change) {
                Err(SaveOutcome::ChangedOnDisk)
            } else {
                Ok(())
            }
        };
        if expected == Baseline::Absent && !accept_disk_change {
            ca_io::replace_new_checked(target, produce, check)
        } else {
            ca_io::replace_checked(target, produce, check)
        }
    }

    fn replace_checked_with_backup(
        &self,
        target: &Path,
        bytes: &[u8],
        expected: Baseline,
        accept_disk_change: bool,
    ) -> Result<Option<PathBuf>, SaveOutcome> {
        #[cfg(any(windows, unix))]
        if let Baseline::Present(previous) = expected {
            if !accept_disk_change {
                return ca_io::replace_checked_preserving_conflict(
                    target,
                    |writer| -> Result<(), SaveOutcome> {
                        writer.write_all(bytes)?;
                        Ok(())
                    },
                    || {
                        if disk_changed(self.stamp(target), expected, false) {
                            Err(SaveOutcome::ChangedOnDisk)
                        } else {
                            Ok(())
                        }
                    },
                    |backup| self.stamp(backup) != Some(previous),
                )
                .map(|((), backup)| backup);
            }
        }
        self.replace_checked(target, bytes, expected, accept_disk_change)
            .map(|()| None)
    }
}

/// How a save ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveOutcome {
    /// The file was written and now looks like this.
    Saved(Stamp),
    /// The requested bytes were written, and a concurrent version was kept
    /// at this backup path for recovery.
    SavedWithConflict {
        /// The saved file's current stamp.
        stamp: Stamp,
        /// The displaced version retained beside the target.
        backup: PathBuf,
    },
    /// The file changed on disk since it was read, so nothing was written.
    ChangedOnDisk,
    /// The target cannot be replaced.
    NotWritable,
    /// Producing the bytes would lose content, and the caller has not agreed
    /// to that. Only a caller whose encode can fail reports this.
    WouldLose(String),
    /// The write failed; the detail names a retained recovery backup if
    /// Windows moved the original during a failed replacement.
    Failed(String),
}

impl From<io::Error> for SaveOutcome {
    fn from(error: io::Error) -> Self {
        if error.kind() == io::ErrorKind::AlreadyExists {
            Self::ChangedOnDisk
        } else {
            Self::Failed(error.to_string())
        }
    }
}

/// Write `bytes` to `path`, honoring both rules above.
///
/// `expected` is the baseline taken when the file was read or first named;
/// [`Baseline::Unchecked`] skips the changed-on-disk check, which is what a
/// write to a new name wants. `accept_disk_change` records that the user
/// already agreed to overwrite a file that changed.
pub fn save(
    files: &dyn FileSystem,
    path: &Path,
    bytes: &[u8],
    expected: Baseline,
    accept_disk_change: bool,
) -> SaveOutcome {
    if let Some(outcome) = check_disk_change(files, path, expected, accept_disk_change) {
        return outcome;
    }
    if !files.is_writable(path) {
        return SaveOutcome::NotWritable;
    }
    let backup = match files.replace_checked_with_backup(path, bytes, expected, accept_disk_change)
    {
        Ok(backup) => backup,
        Err(outcome) => return outcome,
    };
    let stamp = files.stamp(path).unwrap_or(Stamp {
        size: bytes.len() as u64,
        modified: None,
        identity: None,
    });
    backup.map_or(SaveOutcome::Saved(stamp), |backup| {
        SaveOutcome::SavedWithConflict { stamp, backup }
    })
}

/// [`SaveOutcome::ChangedOnDisk`] when the target no longer matches `expected`.
///
/// A caller that produces its bytes fallibly runs this before the encode, so a
/// file that changed underneath is reported rather than encoded for nothing.
#[must_use]
pub fn check_disk_change(
    files: &dyn FileSystem,
    path: &Path,
    expected: Baseline,
    accept_disk_change: bool,
) -> Option<SaveOutcome> {
    disk_changed(files.stamp(path), expected, accept_disk_change)
        .then_some(SaveOutcome::ChangedOnDisk)
}

fn disk_changed(actual: Option<Stamp>, expected: Baseline, accept_disk_change: bool) -> bool {
    if accept_disk_change {
        return false;
    }
    match expected {
        Baseline::Unchecked => false,
        Baseline::Absent => actual.is_some(),
        Baseline::Present(expected) => actual != Some(expected),
    }
}

/// What a user may answer when a modified tab is about to close, or about to
/// read its files again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseChoice {
    /// Write the files, then continue.
    Save,
    /// Lose the edits, then continue.
    Discard,
    /// Leave the tab as it is.
    Cancel,
}

/// A command that reads both files again, which replaces every edit the panes
/// hold.
///
/// With an edit that is not written, the command waits on the question a
/// close asks, and runs after a completed save or after the edits are
/// dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reread {
    /// Read the two files again.
    Reload,
    /// Read the files the path fields name.
    Open,
    /// Exchange the two files and read them again.
    SwapSides,
    /// Read the two files again under changed session settings.
    Settings,
}

impl Reread {
    /// The label of the button that writes the edits first.
    #[must_use]
    pub const fn save_label(self) -> &'static str {
        match self {
            Self::Reload | Self::Open => "Save and reload",
            Self::SwapSides => "Save and swap",
            Self::Settings => "Save and apply",
        }
    }

    /// The label of the button that drops the edits.
    #[must_use]
    pub const fn discard_label(self) -> &'static str {
        match self {
            Self::Reload | Self::Open => "Discard and reload",
            Self::SwapSides => "Discard and swap",
            Self::Settings => "Discard and apply",
        }
    }
}

/// True when closing must ask first.
#[must_use]
pub const fn needs_close_prompt(left_modified: bool, right_modified: bool) -> bool {
    left_modified || right_modified
}

/// What a save job posts back.
#[derive(Debug)]
pub enum SaveMessage {
    /// The save reached an outcome.
    Done(Box<SaveOutcome>),
    /// The job stopped without an outcome.
    Cancelled,
}

impl Terminal for SaveMessage {
    fn is_terminal(&self) -> bool {
        true
    }

    fn cancelled() -> Self {
        SaveMessage::Cancelled
    }

    fn panicked(detail: String) -> Self {
        SaveMessage::Done(Box::new(SaveOutcome::Failed(detail)))
    }
}

/// What the options add to a save of an edited file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SaveRules {
    /// The copy taken of the file before it is replaced, where one is.
    pub backups: Option<backup::SaveBackups>,
    /// The line ending every line of a text is written with, where the
    /// options name one. A byte save ignores it.
    pub line_endings: Option<ca_text::EolStyle>,
}

impl SaveRules {
    /// The rules the options document states for a save.
    #[must_use]
    pub fn from_options(options: &ca_session::options::ProgramOptions) -> Self {
        use ca_session::options::LineEndingsOnSave;
        let line_endings = match options.text_editing.line_endings_on_save {
            LineEndingsOnSave::CrLf { .. } => Some(ca_text::EolStyle::CrLf),
            LineEndingsOnSave::Lf { .. } => Some(ca_text::EolStyle::Lf),
            LineEndingsOnSave::Cr { .. } => Some(ca_text::EolStyle::Cr),
            _ => None,
        };
        Self {
            backups: backup::SaveBackups::for_save(&options.backups),
            line_endings,
        }
    }

    /// The real file system, taking the copy these rules ask for.
    #[must_use]
    pub fn files(&self) -> backup::WithBackups<RealFileSystem> {
        backup::WithBackups {
            files: RealFileSystem,
            rule: self.backups.clone(),
        }
    }
}

/// Run a save on a worker thread.
#[must_use]
pub fn spawn(
    path: PathBuf,
    bytes: Vec<u8>,
    expected: Baseline,
    accept_disk_change: bool,
    rules: SaveRules,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<SaveMessage> {
    spawn_with(notify, move || {
        save(&rules.files(), &path, &bytes, expected, accept_disk_change)
    })
}

/// Run `produce` on a worker thread and post its outcome.
///
/// A caller whose bytes come from a fallible encode uses this, so the encode
/// runs on the worker and not on the frame thread.
#[must_use]
pub fn spawn_with(
    notify: Arc<dyn Fn() + Send + Sync>,
    produce: impl FnOnce() -> SaveOutcome + Send + 'static,
) -> Job<SaveMessage> {
    Job::spawn_notifying(
        move |emitter, _| {
            emitter.send(SaveMessage::Done(Box::new(produce())));
        },
        notify,
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{needs_close_prompt, save, Baseline, CloseChoice, FileSystem, SaveOutcome, Stamp};
    use std::collections::HashMap;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    #[derive(Default)]
    pub struct FakeFiles {
        content: Mutex<HashMap<PathBuf, Vec<u8>>>,
        writable: Mutex<bool>,
        rename_fails: Mutex<bool>,
        write_fails: Mutex<bool>,
        change_on_writability_check: Mutex<Option<Vec<u8>>>,
    }

    impl FakeFiles {
        fn with(path: &Path, bytes: &[u8]) -> Self {
            let files = Self {
                writable: Mutex::new(true),
                ..Self::default()
            };
            files
                .content
                .lock()
                .map(|mut map| map.insert(path.to_path_buf(), bytes.to_vec()))
                .ok();
            files
        }

        fn bytes(&self, path: &Path) -> Option<Vec<u8>> {
            self.content.lock().ok()?.get(path).cloned()
        }

        fn put(&self, path: &Path, bytes: &[u8]) {
            if let Ok(mut map) = self.content.lock() {
                map.insert(path.to_path_buf(), bytes.to_vec());
            }
        }

        fn set(field: &Mutex<bool>, value: bool) {
            if let Ok(mut flag) = field.lock() {
                *flag = value;
            }
        }

        fn read(field: &Mutex<bool>) -> bool {
            field.lock().map(|flag| *flag).unwrap_or(false)
        }
    }

    impl FileSystem for FakeFiles {
        fn stamp(&self, path: &Path) -> Option<Stamp> {
            self.bytes(path).map(|bytes| Stamp {
                size: bytes.len() as u64,
                modified: None,
                identity: None,
            })
        }

        fn is_writable(&self, path: &Path) -> bool {
            if let Ok(mut pending) = self.change_on_writability_check.lock() {
                if let Some(bytes) = pending.take() {
                    self.put(path, &bytes);
                }
            }
            Self::read(&self.writable)
        }

        fn replace(&self, target: &Path, bytes: &[u8]) -> io::Result<()> {
            if Self::read(&self.write_fails) {
                return Err(io::Error::other("no space"));
            }
            if Self::read(&self.rename_fails) {
                return Err(io::Error::other("rename refused"));
            }
            let Ok(mut map) = self.content.lock() else {
                return Err(io::Error::other("locked"));
            };
            map.insert(target.to_path_buf(), bytes.to_vec());
            Ok(())
        }
    }

    fn path() -> PathBuf {
        PathBuf::from("/work/file.bin")
    }

    #[test]
    fn edited_bytes_go_through_a_temporary_and_are_renamed_over_the_target() {
        let target = path();
        let files = FakeFiles::with(&target, &[1, 2, 3]);
        let stamp = Baseline::of(&files, &target);
        let outcome = save(&files, &target, &[9, 2, 3], stamp, false);
        assert!(matches!(outcome, SaveOutcome::Saved(_)));
        assert_eq!(files.bytes(&target).as_deref(), Some([9, 2, 3].as_slice()));
        assert!(files.bytes(&target.with_extension("saving")).is_none());
    }

    #[test]
    fn a_file_that_changed_on_disk_is_not_overwritten() {
        let target = path();
        let files = FakeFiles::with(&target, &[1, 2, 3]);
        let stamp = Baseline::of(&files, &target);
        files.put(&target, &[1, 2, 3, 4]);
        assert_eq!(
            save(&files, &target, &[9], stamp, false),
            SaveOutcome::ChangedOnDisk
        );
        assert_eq!(
            files.bytes(&target).as_deref(),
            Some([1, 2, 3, 4].as_slice())
        );
    }

    #[test]
    fn a_change_after_preflight_is_caught_before_replacement() {
        let target = path();
        let files = FakeFiles::with(&target, b"old");
        let baseline = Baseline::of(&files, &target);
        *files.change_on_writability_check.lock().unwrap() = Some(b"another writer".to_vec());
        assert_eq!(
            save(&files, &target, b"our edit", baseline, false),
            SaveOutcome::ChangedOnDisk
        );
        assert_eq!(files.bytes(&target).unwrap(), b"another writer");
    }

    #[test]
    fn a_missing_target_is_not_silently_recreated_from_a_present_baseline() {
        let target = path();
        let files = FakeFiles::with(&target, b"old");
        let baseline = Baseline::of(&files, &target);
        files.content.lock().unwrap().remove(&target);
        assert_eq!(
            save(&files, &target, b"our edit", baseline, false),
            SaveOutcome::ChangedOnDisk
        );
        assert!(files.bytes(&target).is_none());
    }

    #[test]
    fn the_disk_change_check_answers_before_any_bytes_are_produced() {
        let target = path();
        let files = FakeFiles::with(&target, &[1, 2, 3]);
        let stamp = Baseline::of(&files, &target);
        assert!(super::check_disk_change(&files, &target, stamp, false).is_none());
        files.put(&target, &[7]);
        assert_eq!(
            super::check_disk_change(&files, &target, stamp, false),
            Some(SaveOutcome::ChangedOnDisk)
        );
        assert!(super::check_disk_change(&files, &target, stamp, true).is_none());
    }

    #[test]
    fn an_accepted_disk_change_lets_the_save_through() {
        let target = path();
        let files = FakeFiles::with(&target, &[1, 2, 3]);
        let stamp = Baseline::of(&files, &target);
        files.put(&target, &[7]);
        assert!(matches!(
            save(&files, &target, &[9], stamp, true),
            SaveOutcome::Saved(_)
        ));
        assert_eq!(files.bytes(&target).as_deref(), Some([9].as_slice()));
    }

    #[test]
    fn a_target_that_appeared_after_the_baseline_is_not_overwritten() {
        let target = path();
        let files = FakeFiles::default();
        FakeFiles::set(&files.writable, true);
        let baseline = Baseline::of(&files, &target);
        assert_eq!(baseline, Baseline::Absent);
        files.put(&target, b"someone else wrote this");
        assert_eq!(
            save(&files, &target, &[9], baseline, false),
            SaveOutcome::ChangedOnDisk
        );
        assert_eq!(
            files.bytes(&target).as_deref(),
            Some(b"someone else wrote this".as_slice())
        );
        assert!(matches!(
            save(&files, &target, &[9], baseline, true),
            SaveOutcome::Saved(_)
        ));
    }

    #[test]
    fn an_absent_target_that_is_still_absent_is_written() {
        let target = path();
        let files = FakeFiles::default();
        FakeFiles::set(&files.writable, true);
        let baseline = Baseline::of(&files, &target);
        assert!(matches!(
            save(&files, &target, &[9], baseline, false),
            SaveOutcome::Saved(_)
        ));
        assert_eq!(files.bytes(&target).as_deref(), Some([9].as_slice()));
    }

    #[test]
    fn a_read_only_target_is_refused_before_anything_is_written() {
        let target = path();
        let files = FakeFiles::with(&target, &[1]);
        FakeFiles::set(&files.writable, false);
        assert_eq!(
            save(&files, &target, &[9], Baseline::Unchecked, false),
            SaveOutcome::NotWritable
        );
        assert_eq!(files.bytes(&target).as_deref(), Some([1].as_slice()));
    }

    #[test]
    fn a_failed_rename_leaves_the_original_and_removes_the_temporary() {
        let target = path();
        let files = FakeFiles::with(&target, &[1]);
        FakeFiles::set(&files.rename_fails, true);
        assert!(matches!(
            save(&files, &target, &[9], Baseline::Unchecked, false),
            SaveOutcome::Failed(_)
        ));
        assert_eq!(files.bytes(&target).as_deref(), Some([1].as_slice()));
        assert!(files.bytes(&target.with_extension("saving")).is_none());
    }

    #[test]
    fn a_failed_temporary_write_leaves_the_original() {
        let target = path();
        let files = FakeFiles::with(&target, &[1]);
        FakeFiles::set(&files.write_fails, true);
        assert!(matches!(
            save(&files, &target, &[9], Baseline::Unchecked, false),
            SaveOutcome::Failed(_)
        ));
        assert_eq!(files.bytes(&target).as_deref(), Some([1].as_slice()));
    }

    #[test]
    fn a_modified_side_makes_closing_ask() {
        assert!(!needs_close_prompt(false, false));
        assert!(needs_close_prompt(true, false));
        assert!(needs_close_prompt(false, true));
        assert_ne!(CloseChoice::Save, CloseChoice::Cancel);
    }
}
