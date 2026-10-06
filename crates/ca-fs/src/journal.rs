//! A write-ahead record of a batch of destructive steps.
//!
//! One JSON object per line is appended and forced to stable storage before
//! the step it describes runs, and again once the step has finished. A batch
//! that is interrupted therefore leaves a file that names the step that was in
//! flight and the temporary file it was writing, which is what
//! [`recover`] reads.

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::ops::exec::StepOutcome;
use crate::ops::fsops::FileOps;
use crate::ops::plan::{OperationKind, StepAction};

/// Extension every journal file carries.
const JOURNAL_SUFFIX: &str = ".jsonl";

/// Days a journal stays on disk after a batch that ended with failed or
/// skipped steps.
///
/// The record is the only account of which steps did not run, so it outlives
/// the batch long enough to be read.
pub const KEEP_TROUBLED_DAYS: u64 = 14;

/// Seconds in one day.
const DAY_SECONDS: u64 = 24 * 60 * 60;

/// One line of a journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "record",
    rename_all = "snake_case",
    rename_all_fields = "snake_case"
)]
pub enum JournalRecord {
    /// Written before the first step runs.
    BatchStart {
        /// Operation the batch carries out.
        kind: OperationKind,
        /// Number of steps the plan holds.
        steps: usize,
        /// Wall clock time the batch started.
        unix_seconds: i64,
    },
    /// Written before one step runs.
    StepBegin {
        /// Position of the step in the plan.
        index: usize,
        /// What the step is about to do.
        action: StepAction,
        /// File the step writes before renaming it over its target.
        temporary: Option<PathBuf>,
        /// Copy of the target taken before it is replaced, or the staged
        /// name an exchange keeps one of its two files under while it runs.
        /// Recovery lists it and never removes it.
        backup: Option<PathBuf>,
    },
    /// Written once one step has finished, whatever the result.
    StepEnd {
        /// Position of the step in the plan.
        index: usize,
        /// How the step ended.
        outcome: StepOutcome,
    },
    /// Written once the batch has finished.
    BatchEnd {
        /// Steps that ran to completion.
        completed: usize,
        /// True when the cancel flag stopped the batch.
        cancelled: bool,
        /// True when the error policy stopped the batch.
        aborted: bool,
    },
}

/// An open journal file.
///
/// Appending takes a lock and forces the line to stable storage, so a record
/// that is visible in the file is a record whose write completed.
#[derive(Debug)]
pub struct Journal {
    path: PathBuf,
    file: Mutex<File>,
}

impl Journal {
    /// Create a journal at `path`, which must not already exist.
    ///
    /// An existing journal is never truncated: the file it holds may be the
    /// only record of an interrupted batch's open steps.
    ///
    /// # Errors
    /// Returns [`std::io::ErrorKind::AlreadyExists`] when the path is taken,
    /// and otherwise propagates the underlying I/O error.
    pub fn create(path: &Path) -> std::io::Result<Self> {
        let mut options = File::options();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let file = options.open(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            file: Mutex::new(file),
        })
    }

    /// Create a journal under `directory` with a name no other batch shares.
    ///
    /// The directory is created when it is missing, so a caller only has to
    /// name a place to keep journals. On Unix it is open only to the running
    /// user, as is each journal.
    ///
    /// # Errors
    /// Propagates the underlying I/O error, and refuses a directory that is a
    /// link or belongs to another user.
    pub fn create_in(directory: &Path) -> std::io::Result<Self> {
        ca_io::private::create_owned_folder(directory)?;
        let stamp = now_unix_seconds();
        let process = std::process::id();
        for attempt in 0..1_000u32 {
            let name = format!("batch-{stamp}-{process}-{attempt}{JOURNAL_SUFFIX}");
            match Self::create(&directory.join(name)) {
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                other => return other,
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "no free journal name",
        ))
    }

    /// The file this journal writes to.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one record and force it to stable storage.
    ///
    /// # Errors
    /// Propagates the underlying I/O error, or reports a poisoned lock as
    /// [`std::io::ErrorKind::Other`].
    pub fn append(&self, record: &JournalRecord) -> std::io::Result<()> {
        JournalWriter::append(self, record)
    }
}

/// Where execution writes its records.
///
/// Execution holds the writer behind this trait, so a test can supply one whose
/// writes fail and drive the path a full disk takes.
pub trait JournalWriter: std::fmt::Debug + Send + Sync {
    /// Append one record and force it to stable storage.
    ///
    /// # Errors
    /// Propagates the underlying I/O error.
    fn append(&self, record: &JournalRecord) -> std::io::Result<()>;

    /// Path the records land in.
    fn path(&self) -> &Path;
}

impl JournalWriter for Journal {
    fn path(&self) -> &Path {
        &self.path
    }

    fn append(&self, record: &JournalRecord) -> std::io::Result<()> {
        let mut line = serde_json::to_string(record).map_err(std::io::Error::other)?;
        line.push('\n');
        let mut file = self
            .file
            .lock()
            .map_err(|_| std::io::Error::other("journal lock poisoned"))?;
        file.write_all(line.as_bytes())?;
        file.flush()?;
        file.sync_all()
    }
}

/// Seconds since the Unix epoch, saturating before it.
#[must_use]
pub fn now_unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX)
        })
}

/// Read a journal, ignoring a trailing line that was cut short by the
/// interruption the journal exists to record.
///
/// # Errors
/// Propagates the underlying I/O error.
pub fn read_records(path: &Path) -> std::io::Result<Vec<JournalRecord>> {
    let reader = BufReader::new(File::open(path)?);
    let mut records = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str(&line) {
            Ok(record) => records.push(record),
            Err(_) => break,
        }
    }
    Ok(records)
}

/// What a journal says about a batch that may not have finished.
#[derive(Debug, Clone, Default)]
pub struct Recovery {
    /// Every record the journal holds.
    pub records: Vec<JournalRecord>,
    /// True when the journal ends with a batch end record.
    pub completed: bool,
    /// Steps that began and never reported an end.
    pub unfinished: Vec<usize>,
    /// True when the batch recorded that the cancel flag stopped it.
    pub cancelled: bool,
    /// True when the batch recorded that the error policy stopped it.
    pub aborted: bool,
    /// Steps the batch set out to run, as the start record names them.
    pub planned: usize,
    /// Steps the end record counts as finished.
    pub finished: usize,
    /// True when at least one step ended as anything other than done.
    pub troubled: bool,
    /// Temporary files the batch may have left behind.
    pub leftover_temporaries: Vec<PathBuf>,
    /// Backups the batch took, which the caller may want to keep or restore.
    pub backups: Vec<PathBuf>,
}

/// Read a journal and work out what an interrupted batch left behind.
///
/// # Errors
/// Propagates the underlying I/O error.
pub fn recover(journal_path: &Path) -> std::io::Result<Recovery> {
    let records = read_records(journal_path)?;
    let mut recovery = Recovery {
        completed: records
            .last()
            .is_some_and(|record| matches!(record, JournalRecord::BatchEnd { .. })),
        ..Recovery::default()
    };

    let mut open: Vec<(usize, Option<PathBuf>)> = Vec::new();
    for record in &records {
        match record {
            JournalRecord::StepBegin {
                index,
                temporary,
                backup,
                ..
            } => {
                open.push((*index, temporary.clone()));
                if let Some(temporary) = temporary {
                    recovery.leftover_temporaries.push(temporary.clone());
                }
                if let Some(backup) = backup {
                    recovery.backups.push(backup.clone());
                }
            }
            JournalRecord::StepEnd { index, outcome } => {
                open.retain(|(open_index, _)| open_index != index);
                if !matches!(outcome, StepOutcome::Done) {
                    recovery.troubled = true;
                }
            }
            JournalRecord::BatchEnd {
                completed,
                cancelled,
                aborted,
            } => {
                recovery.finished = *completed;
                recovery.cancelled = *cancelled;
                recovery.aborted = *aborted;
            }
            JournalRecord::BatchStart { steps, .. } => recovery.planned = *steps,
        }
    }
    recovery.unfinished = open.iter().map(|(index, _)| *index).collect();
    recovery.records = records;
    Ok(recovery)
}

/// Read every journal in `directory` and report the batches that did not
/// finish.
///
/// Only journals this module wrote are read, and only the temporaries those
/// journals name are ever offered for removal; nothing is judged by file name
/// alone.
///
/// # Errors
/// Propagates the underlying I/O error from listing the directory.
pub fn recover_all(directory: &Path) -> std::io::Result<Vec<(PathBuf, Recovery)>> {
    let mut out = Vec::new();
    if !directory.is_dir() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        if path.extension().is_none_or(|ext| ext != "jsonl") {
            continue;
        }
        let Ok(recovery) = recover(&path) else {
            continue;
        };
        if recovery.was_interrupted() {
            out.push((path, recovery));
        }
    }
    out.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(out)
}

impl Recovery {
    /// Remove every temporary file the batch may have left behind.
    ///
    /// A temporary that is already gone is not an error: the executor removes
    /// its own temporary when a step fails, so the usual outcome is that
    /// nothing is left to remove. Returns the temporaries that could not be
    /// removed, with the reason.
    #[must_use]
    pub fn clean_up(&self, fs: &dyn FileOps) -> Vec<(PathBuf, String)> {
        let mut failures = Vec::new();
        for temporary in &self.leftover_temporaries {
            if fs.probe(temporary).is_err() {
                continue;
            }
            if let Err(error) = fs.remove_file(temporary) {
                failures.push((temporary.clone(), error.to_string()));
            }
        }
        failures
    }

    /// True when the batch did not run every step it set out to run.
    ///
    /// A batch that reached its end record having been cancelled or stopped
    /// counts, because the temporaries and backups it names are still the only
    /// account of what it left behind.
    #[must_use]
    pub fn was_interrupted(&self) -> bool {
        !self.completed || !self.unfinished.is_empty() || self.cancelled || self.aborted
    }

    /// True when the batch reached its end record with every step done.
    ///
    /// Such a journal describes nothing the user has to decide about, so it is
    /// the one case housekeeping removes without asking.
    #[must_use]
    pub fn ended_cleanly(&self) -> bool {
        !self.was_interrupted() && !self.troubled && self.finished == self.planned
    }
}

/// Remove the journal at `path` when its batch ended with every step done.
///
/// Returns true when the file was removed. A batch that had failed or skipped
/// steps keeps its journal, which [`sweep`] removes later.
///
/// # Errors
/// Propagates the underlying I/O error from reading or removing the file.
pub fn retire(path: &Path) -> std::io::Result<bool> {
    if !recover(path)?.ended_cleanly() {
        return Ok(false);
    }
    std::fs::remove_file(path)?;
    Ok(true)
}

/// Remove the journals in `directory` that nothing is waiting on.
///
/// A journal whose batch ended cleanly goes at once. A journal whose batch
/// ended with failed or skipped steps goes once it is older than `keep_days`.
/// A journal whose batch never reported an end stays, whatever its age: what
/// to do about those steps is the user's choice.
///
/// Returns the journals that were removed.
///
/// # Errors
/// Propagates the underlying I/O error from listing the directory.
pub fn sweep(directory: &Path, keep_days: u64) -> std::io::Result<Vec<PathBuf>> {
    let mut removed = Vec::new();
    if !directory.is_dir() {
        return Ok(removed);
    }
    let cutoff = keep_days.saturating_mul(DAY_SECONDS);
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        if path.extension().is_none_or(|ext| ext != "jsonl") {
            continue;
        }
        let Ok(recovery) = recover(&path) else {
            continue;
        };
        if recovery.was_interrupted() {
            continue;
        }
        if !recovery.ended_cleanly() && age_seconds(&path).is_none_or(|age| age < cutoff) {
            continue;
        }
        if std::fs::remove_file(&path).is_ok() {
            removed.push(path);
        }
    }
    removed.sort();
    Ok(removed)
}

/// Seconds since the file was last written, or none when the clock or the
/// file system does not answer.
fn age_seconds(path: &Path) -> Option<u64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    SystemTime::now()
        .duration_since(modified)
        .ok()
        .map(|since| since.as_secs())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::path::PathBuf;

    use super::{recover, recover_all, retire, sweep, Journal, JournalRecord, KEEP_TROUBLED_DAYS};
    use crate::ops::exec::StepOutcome;
    use crate::ops::fsops::RealFs;
    use crate::ops::plan::{OperationKind, StepAction};

    /// Write a one step batch whose step ends with `outcome` and whose batch
    /// end record counts `finished` steps.
    fn write_batch(path: &std::path::Path, outcome: StepOutcome, finished: usize) {
        let journal = Journal::create(path).unwrap();
        journal
            .append(&JournalRecord::BatchStart {
                kind: OperationKind::Copy,
                steps: 1,
                unix_seconds: 0,
            })
            .unwrap();
        journal
            .append(&JournalRecord::StepBegin {
                index: 0,
                action: StepAction::CopyFile {
                    source: PathBuf::from("a"),
                    target: PathBuf::from("b"),
                },
                temporary: None,
                backup: None,
            })
            .unwrap();
        journal
            .append(&JournalRecord::StepEnd { index: 0, outcome })
            .unwrap();
        journal
            .append(&JournalRecord::BatchEnd {
                completed: finished,
                cancelled: false,
                aborted: false,
            })
            .unwrap();
    }

    /// A journal lists the path of every item in a batch.
    #[cfg(unix)]
    #[test]
    fn a_journal_and_its_folder_are_open_only_to_their_user() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("settings").join("journals");
        let journal = Journal::create_in(&folder).unwrap();
        let mode =
            |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&folder), 0o700);
        assert_eq!(mode(journal.path()), 0o600);
    }

    #[test]
    fn a_batch_with_every_step_done_ends_cleanly_and_is_retired() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clean.jsonl");
        write_batch(&path, StepOutcome::Done, 1);
        assert!(recover(&path).unwrap().ended_cleanly());
        assert!(retire(&path).unwrap());
        assert!(!path.exists());
    }

    #[test]
    fn a_batch_with_a_skipped_step_is_kept_for_the_documented_days() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("skipped.jsonl");
        write_batch(
            &path,
            StepOutcome::Skipped {
                reason: "the target is newer".to_string(),
            },
            0,
        );
        let recovery = recover(&path).unwrap();
        assert!(recovery.troubled);
        assert!(!recovery.ended_cleanly());
        assert!(!retire(&path).unwrap());
        assert!(path.exists(), "a troubled batch keeps its record");
        assert!(sweep(dir.path(), KEEP_TROUBLED_DAYS).unwrap().is_empty());
        assert!(path.exists());
        // Past the keeping time the same record goes.
        assert_eq!(sweep(dir.path(), 0).unwrap(), vec![path.clone()]);
        assert!(!path.exists());
    }

    #[test]
    fn a_sweep_removes_a_clean_record_and_leaves_an_unfinished_one() {
        let dir = tempfile::tempdir().unwrap();
        let clean = dir.path().join("clean.jsonl");
        write_batch(&clean, StepOutcome::Done, 1);
        let open = dir.path().join("open.jsonl");
        let journal = Journal::create(&open).unwrap();
        journal
            .append(&JournalRecord::BatchStart {
                kind: OperationKind::Delete,
                steps: 3,
                unix_seconds: 0,
            })
            .unwrap();
        journal
            .append(&JournalRecord::StepBegin {
                index: 0,
                action: StepAction::CopyFile {
                    source: PathBuf::from("a"),
                    target: PathBuf::from("b"),
                },
                temporary: None,
                backup: None,
            })
            .unwrap();
        drop(journal);

        assert_eq!(sweep(dir.path(), 0).unwrap(), vec![clean.clone()]);
        assert!(!clean.exists());
        assert!(open.exists(), "an unfinished batch is never swept");
    }

    #[test]
    fn a_cleanly_ended_batch_is_never_reported_for_recovery() {
        let dir = tempfile::tempdir().unwrap();
        write_batch(&dir.path().join("clean.jsonl"), StepOutcome::Done, 1);
        assert!(recover_all(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn a_torn_last_line_leaves_the_batch_unfinished_and_unswept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("torn.jsonl");
        write_batch(&path, StepOutcome::Done, 1);
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("{\"record\":\"batch_e");
        std::fs::write(&path, text).unwrap();

        let recovery = recover(&path).unwrap();
        assert!(recovery.ended_cleanly(), "the torn line carries no record");
        // A torn line after an unfinished step keeps the batch open.
        let other = dir.path().join("open.jsonl");
        let journal = Journal::create(&other).unwrap();
        journal
            .append(&JournalRecord::BatchStart {
                kind: OperationKind::Copy,
                steps: 2,
                unix_seconds: 0,
            })
            .unwrap();
        drop(journal);
        let mut text = std::fs::read_to_string(&other).unwrap();
        text.push_str("{\"record\":\"step_beg");
        std::fs::write(&other, text).unwrap();
        assert!(recover(&other).unwrap().was_interrupted());
        assert!(!retire(&other).unwrap());
        assert!(sweep(dir.path(), 0).unwrap().contains(&path));
        assert!(other.exists());
    }

    #[test]
    fn an_interrupted_batch_reports_its_open_step_and_temporary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("batch.jsonl");
        let temporary = dir.path().join("target.tmp");
        std::fs::write(&temporary, b"partial").unwrap();

        let journal = Journal::create(&path).unwrap();
        journal
            .append(&JournalRecord::BatchStart {
                kind: OperationKind::Copy,
                steps: 1,
                unix_seconds: 0,
            })
            .unwrap();
        journal
            .append(&JournalRecord::StepBegin {
                index: 0,
                action: StepAction::CopyFile {
                    source: PathBuf::from("a"),
                    target: PathBuf::from("b"),
                },
                temporary: Some(temporary.clone()),
                backup: None,
            })
            .unwrap();
        drop(journal);

        let recovery = recover(&path).unwrap();
        assert!(recovery.was_interrupted());
        assert_eq!(recovery.unfinished, vec![0]);
        assert_eq!(recovery.leftover_temporaries, vec![temporary.clone()]);
        assert!(recovery.clean_up(&RealFs).is_empty());
        assert!(!temporary.exists());
    }

    #[test]
    fn a_truncated_final_line_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("batch.jsonl");
        let journal = Journal::create(&path).unwrap();
        journal
            .append(&JournalRecord::BatchStart {
                kind: OperationKind::Delete,
                steps: 2,
                unix_seconds: 7,
            })
            .unwrap();
        drop(journal);
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("{\"record\":\"step_beg");
        std::fs::write(&path, text).unwrap();

        let recovery = recover(&path).unwrap();
        assert_eq!(recovery.records.len(), 1);
        assert!(recovery.was_interrupted());
    }
}
