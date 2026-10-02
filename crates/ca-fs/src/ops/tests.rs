//! Behaviour tests for planning, execution, journalling and recovery.

#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]
#![allow(clippy::panic)]

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use crate::cancel::Cancel;
use crate::compare::{align_trees, compare_quick, AlignmentOptions, Node, NodeStatus};
use crate::criteria::{CompareOptions, Side};
use crate::journal::{recover, Journal, JournalRecord, JournalWriter};
use crate::ops::exec::{
    execute, ConflictDecision, Decision, Drift, ErrorPolicy, ExecutionContext, Journaling,
    Progress, StepOutcome,
};
use crate::ops::fsops::{
    same_item, write_new_local, AttributeChange, FileOps, Reach, RealFs, SyncWrite, TargetState,
};
use crate::ops::plan::{
    exclude_masks, plan_attributes, plan_copy, plan_delete, plan_exchange, plan_move,
    plan_new_folder, plan_rename, plan_to_folder, plan_touch, resolve_selection, BackupOptions,
    Bases, Conflict, OperationOptions, OperationPlan, PathOption, PlanStep, RenameAction,
    RenameError, Sides, StepAction, TouchSpec, Verify,
};
use crate::scan::{scan_with, ScanOptions, ScanResult};
use crate::sync::{plan_sync, preview, SyncAction, SyncPreset};

// ---------------------------------------------------------------- fixtures

fn options() -> OperationOptions {
    OperationOptions {
        // The trash is a machine-wide side effect, so tests remove outright.
        use_recycle_bin: false,
        buffer_size: 4096,
        ..OperationOptions::default()
    }
}

fn scan_side(root: &Path) -> ScanResult {
    scan_with(root, &ScanOptions::default(), &Cancel::new(), &|_| {}).unwrap()
}

fn compared(left: &Path, right: &Path) -> Node {
    let mut root = align_trees(
        &scan_side(left),
        &scan_side(right),
        &AlignmentOptions::default(),
        &Cancel::new(),
    );
    compare_quick(&mut root, &CompareOptions::default());
    root
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path.clone());
                out.insert(path.strip_prefix(root).unwrap().to_path_buf(), Vec::new());
            } else if let Ok(bytes) = std::fs::read(&path) {
                out.insert(path.strip_prefix(root).unwrap().to_path_buf(), bytes);
            }
        }
    }
    out
}

fn run(plan: &OperationPlan) -> crate::ops::exec::ExecutionReport {
    let cancel = Cancel::new();
    execute(
        plan,
        &ExecutionContext::new(&RealFs, &cancel, Journaling::Disabled),
    )
}

fn selection<'a>(root: &'a Node, rels: &[&str]) -> Vec<&'a Node> {
    let set: BTreeSet<PathBuf> = rels.iter().map(PathBuf::from).collect();
    resolve_selection(root, &set)
}

// ------------------------------------------------------- injectable shim

/// A call this shim fails once it sees a path containing the given text.
#[derive(Debug, Clone)]
struct Fault {
    call: &'static str,
    path_contains: String,
    kind: io::ErrorKind,
}

/// [`FileOps`] that forwards to the real file system and fails the calls the
/// test names, so every step of a batch can be made to fail in isolation.
struct FaultFs {
    inner: RealFs,
    faults: Mutex<Vec<Fault>>,
    calls: Mutex<Vec<String>>,
    copies_started: AtomicUsize,
    /// Position of the rename to fail, counting from zero across the batch.
    failing_rename: Option<usize>,
    renames: AtomicUsize,
    /// Answer given to the volume question, which is what decides whether a
    /// move renames or copies.
    same_volume: bool,
    /// A file that another writer puts at a path when a rename onto that path
    /// starts, after every check the executor made.
    arrival: Mutex<Option<(PathBuf, Vec<u8>)>>,
    /// The same, for the first rename onto a name that ends with the text.
    arrival_by_suffix: Mutex<Option<(String, Vec<u8>)>>,
}

impl FaultFs {
    fn new(faults: Vec<Fault>) -> Self {
        Self {
            inner: RealFs,
            faults: Mutex::new(faults),
            calls: Mutex::new(Vec::new()),
            copies_started: AtomicUsize::new(0),
            failing_rename: None,
            renames: AtomicUsize::new(0),
            same_volume: true,
            arrival: Mutex::new(None),
            arrival_by_suffix: Mutex::new(None),
        }
    }

    fn arriving_at_suffix(suffix: &str, bytes: &[u8]) -> Self {
        Self {
            arrival_by_suffix: Mutex::new(Some((suffix.to_owned(), bytes.to_vec()))),
            ..Self::new(Vec::new())
        }
    }

    fn failing_rename(position: usize) -> Self {
        Self {
            failing_rename: Some(position),
            ..Self::new(Vec::new())
        }
    }

    fn across_volumes(faults: Vec<Fault>) -> Self {
        Self {
            same_volume: false,
            ..Self::new(faults)
        }
    }

    fn arriving(path: &Path, bytes: &[u8]) -> Self {
        Self {
            arrival: Mutex::new(Some((path.to_path_buf(), bytes.to_vec()))),
            ..Self::new(Vec::new())
        }
    }

    fn before_rename(&self, to: &Path) -> io::Result<()> {
        self.check("rename", to)?;
        let position = self.renames.fetch_add(1, Ordering::SeqCst);
        if self.failing_rename == Some(position) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "injected rename failure",
            ));
        }
        let mut arrival = self.arrival.lock().unwrap();
        if arrival.as_ref().is_some_and(|(path, _)| path == to) {
            if let Some((path, bytes)) = arrival.take() {
                std::fs::write(path, bytes)?;
            }
        }
        let mut by_suffix = self.arrival_by_suffix.lock().unwrap();
        if by_suffix
            .as_ref()
            .is_some_and(|(suffix, _)| to.to_string_lossy().ends_with(suffix.as_str()))
        {
            if let Some((_, bytes)) = by_suffix.take() {
                std::fs::write(to, bytes)?;
            }
        }
        Ok(())
    }

    fn check(&self, call: &'static str, path: &Path) -> io::Result<()> {
        let text = path.to_string_lossy().replace('\\', "/");
        self.calls.lock().unwrap().push(format!("{call}:{text}"));
        let faults = self.faults.lock().unwrap();
        for fault in faults.iter() {
            if fault.call == call && text.contains(&fault.path_contains) {
                return Err(io::Error::new(fault.kind, "injected failure"));
            }
        }
        Ok(())
    }

    fn saw(&self, needle: &str) -> bool {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.contains(needle))
    }
}

impl FileOps for FaultFs {
    fn probe(&self, path: &Path) -> io::Result<TargetState> {
        self.check("probe", path)?;
        self.inner.probe(path)
    }
    fn create_dir(&self, path: &Path) -> io::Result<()> {
        self.check("create_dir", path)?;
        self.inner.create_dir(path)
    }
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn Read + Send>> {
        self.check("open_read", path)?;
        self.copies_started.fetch_add(1, Ordering::SeqCst);
        self.inner.open_read(path)
    }
    fn create_new(&self, path: &Path) -> io::Result<Box<dyn SyncWrite>> {
        self.check("create_new", path)?;
        self.inner.create_new(path)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.before_rename(to)?;
        self.inner.rename(from, to)
    }
    fn rename_no_replace(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.before_rename(to)?;
        self.inner.rename_no_replace(from, to)
    }
    fn write_new(
        &self,
        target: &Path,
        reader: &mut dyn Read,
        buffer_size: usize,
    ) -> io::Result<()> {
        self.check("create_new", target)?;
        write_new_local(target, reader, buffer_size, || self.before_rename(target))
    }
    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.check("remove_file", path)?;
        self.inner.remove_file(path)
    }
    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        self.check("remove_dir", path)?;
        self.inner.remove_dir(path)
    }
    fn move_to_trash(&self, path: &Path) -> io::Result<()> {
        self.check("move_to_trash", path)?;
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
    fn set_modified(&self, path: &Path, time: SystemTime) -> io::Result<()> {
        self.check("set_modified", path)?;
        self.inner.set_modified(path, time)
    }
    fn set_created(&self, path: &Path, time: SystemTime) -> io::Result<()> {
        self.inner.set_created(path, time)
    }
    fn set_attributes(&self, path: &Path, change: &AttributeChange) -> io::Result<()> {
        self.check("set_attributes", path)?;
        self.inner.set_attributes(path, change)
    }
    fn same_volume(&self, from: &Path, to: &Path) -> bool {
        self.same_volume && self.inner.same_volume(from, to)
    }
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        self.check("read_dir", path)?;
        self.inner.read_dir(path)
    }
}

/// [`FileOps`] whose copies land short, so a finished copy does not match the
/// source it was made from.
struct ShortWriteFs {
    inner: RealFs,
}

struct HalfWriter {
    inner: Box<dyn SyncWrite>,
}

impl io::Write for HalfWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let keep = buffer.len() / 2;
        self.inner.write_all(&buffer[..keep])?;
        Ok(buffer.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl SyncWrite for HalfWriter {
    fn sync_data(&mut self) -> io::Result<()> {
        self.inner.sync_data()
    }
}

impl FileOps for ShortWriteFs {
    fn probe(&self, path: &Path) -> io::Result<TargetState> {
        self.inner.probe(path)
    }
    fn create_dir(&self, path: &Path) -> io::Result<()> {
        self.inner.create_dir(path)
    }
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn Read + Send>> {
        self.inner.open_read(path)
    }
    fn create_new(&self, path: &Path) -> io::Result<Box<dyn SyncWrite>> {
        Ok(Box::new(HalfWriter {
            inner: self.inner.create_new(path)?,
        }))
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.inner.rename(from, to)
    }
    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.inner.remove_file(path)
    }
    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        self.inner.remove_dir(path)
    }
    fn move_to_trash(&self, path: &Path) -> io::Result<()> {
        self.inner.move_to_trash(path)
    }
    fn set_modified(&self, path: &Path, time: SystemTime) -> io::Result<()> {
        self.inner.set_modified(path, time)
    }
    fn set_created(&self, path: &Path, time: SystemTime) -> io::Result<()> {
        self.inner.set_created(path, time)
    }
    fn set_attributes(&self, path: &Path, change: &AttributeChange) -> io::Result<()> {
        self.inner.set_attributes(path, change)
    }
    fn same_volume(&self, from: &Path, to: &Path) -> bool {
        self.inner.same_volume(from, to)
    }
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        self.inner.read_dir(path)
    }
}

/// A reader that trips the cancel flag once one block has been handed over, so
/// a copy is always interrupted with the destination half written.
struct TripReader {
    inner: Box<dyn Read + Send>,
    cancel: Cancel,
    blocks: usize,
}

impl Read for TripReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.blocks += 1;
        if self.blocks >= 2 {
            self.cancel.cancel();
        }
        Ok(read)
    }
}

struct CancellingFs {
    inner: RealFs,
    cancel: Cancel,
}

impl FileOps for CancellingFs {
    fn probe(&self, path: &Path) -> io::Result<TargetState> {
        self.inner.probe(path)
    }
    fn create_dir(&self, path: &Path) -> io::Result<()> {
        self.inner.create_dir(path)
    }
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn Read + Send>> {
        Ok(Box::new(TripReader {
            inner: self.inner.open_read(path)?,
            cancel: self.cancel.clone(),
            blocks: 0,
        }))
    }
    fn create_new(&self, path: &Path) -> io::Result<Box<dyn SyncWrite>> {
        self.inner.create_new(path)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.inner.rename(from, to)
    }
    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.inner.remove_file(path)
    }
    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        self.inner.remove_dir(path)
    }
    fn move_to_trash(&self, path: &Path) -> io::Result<()> {
        self.inner.move_to_trash(path)
    }
    fn set_modified(&self, path: &Path, time: SystemTime) -> io::Result<()> {
        self.inner.set_modified(path, time)
    }
    fn set_created(&self, path: &Path, time: SystemTime) -> io::Result<()> {
        self.inner.set_created(path, time)
    }
    fn set_attributes(&self, path: &Path, change: &AttributeChange) -> io::Result<()> {
        self.inner.set_attributes(path, change)
    }
    fn same_volume(&self, from: &Path, to: &Path) -> bool {
        self.inner.same_volume(from, to)
    }
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        self.inner.read_dir(path)
    }
}

struct RetryOnce(AtomicUsize);

impl ErrorPolicy for RetryOnce {
    fn on_error(&self, _step: &PlanStep, attempt: u32, _message: &str) -> Decision {
        self.0.fetch_add(1, Ordering::SeqCst);
        if attempt < 2 {
            Decision::Retry
        } else {
            Decision::Skip
        }
    }
}

struct DeclineConflicts;

impl ErrorPolicy for DeclineConflicts {
    fn on_error(&self, _step: &PlanStep, _attempt: u32, _message: &str) -> Decision {
        Decision::Skip
    }
    fn on_conflict(&self, _step: &PlanStep) -> ConflictDecision {
        ConflictDecision::Skip
    }
}

// -------------------------------------------------------------- planning

#[test]
fn planning_changes_nothing_on_disk() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::create_dir(left.path().join("sub")).unwrap();
    std::fs::write(left.path().join("sub/a.txt"), b"left").unwrap();
    std::fs::write(right.path().join("b.txt"), b"right").unwrap();

    let before_left = snapshot(left.path());
    let before_right = snapshot(right.path());
    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };

    let copy = plan_copy(&selection(&root, &["sub"]), Side::Left, bases, &options());
    let delete = plan_delete(
        &selection(&root, &["b.txt"]),
        Sides::Right,
        bases,
        &options(),
    );
    let moved = plan_move(&selection(&root, &["sub"]), Side::Left, bases, &options());

    assert!(!copy.steps.is_empty());
    assert!(!delete.steps.is_empty());
    assert!(!moved.steps.is_empty());
    assert_eq!(snapshot(left.path()), before_left);
    assert_eq!(snapshot(right.path()), before_right);
}

#[test]
fn a_plan_stays_inside_its_base_folders() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"x").unwrap();
    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let plan = plan_copy(&selection(&root, &["a.txt"]), Side::Left, bases, &options());
    assert!(plan.is_contained());
    assert_eq!(plan.total_bytes(), 1);
}

#[test]
fn excluding_steps_renumbers_what_is_left() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    for name in ["a.txt", "b.txt", "c.txt"] {
        std::fs::write(left.path().join(name), b"x").unwrap();
    }
    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let mut plan = plan_copy(
        &selection(&root, &["a.txt", "b.txt", "c.txt"]),
        Side::Left,
        bases,
        &options(),
    );
    assert_eq!(plan.steps.len(), 3);
    plan.exclude(&[1].into_iter().collect());
    assert_eq!(plan.steps.len(), 2);
    assert_eq!(plan.steps[1].index, 1);
}

// ------------------------------------------------------------- copy, move

#[test]
fn copy_preserves_contents_and_modification_time() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    let source = left.path().join("a.txt");
    std::fs::write(&source, b"payload").unwrap();
    let when = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
    RealFs.set_modified(&source, when).unwrap();

    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let plan = plan_copy(&selection(&root, &["a.txt"]), Side::Left, bases, &options());
    assert!(run(&plan).is_clean());

    let target = right.path().join("a.txt");
    assert_eq!(std::fs::read(&target).unwrap(), b"payload");
    let landed = RealFs.probe(&target).unwrap().modified.unwrap();
    let delta = landed
        .duration_since(when)
        .or_else(|_| when.duration_since(landed))
        .unwrap();
    assert!(
        delta < Duration::from_secs(2),
        "timestamp drifted by {delta:?}"
    );
}

#[test]
fn copy_carries_a_whole_subtree() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(left.path().join("sub/deep")).unwrap();
    std::fs::write(left.path().join("sub/one.txt"), b"1").unwrap();
    std::fs::write(left.path().join("sub/deep/two.txt"), b"2").unwrap();

    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let plan = plan_copy(&selection(&root, &["sub"]), Side::Left, bases, &options());
    assert!(run(&plan).is_clean());
    assert_eq!(
        std::fs::read(right.path().join("sub/deep/two.txt")).unwrap(),
        b"2"
    );
}

#[test]
fn a_verified_copy_reports_the_bytes_it_moved() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.bin"), vec![7u8; 20_000]).unwrap();
    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let mut opts = options();
    opts.verify = Verify::Hash;
    let plan = plan_copy(&selection(&root, &["a.bin"]), Side::Left, bases, &opts);
    let report = run(&plan);
    assert!(report.is_clean());
    assert_eq!(report.bytes_copied, 20_000);
}

#[test]
fn a_read_only_target_is_left_alone_unless_the_option_says_otherwise() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"new").unwrap();
    let target = right.path().join("a.txt");
    std::fs::write(&target, b"old").unwrap();
    RealFs
        .set_attributes(
            &target,
            &AttributeChange {
                read_only: Some(true),
                ..AttributeChange::default()
            },
        )
        .unwrap();

    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let sel = selection(&root, &["a.txt"]);
    let plan = plan_copy(&sel, Side::Left, bases, &options());
    assert!(plan.conflicts().contains(&Conflict::TargetReadOnly));
    let report = run(&plan);
    assert!(!report.is_clean());
    assert_eq!(std::fs::read(&target).unwrap(), b"old");

    let mut opts = options();
    opts.clear_read_only_targets = true;
    let plan = plan_copy(&sel, Side::Left, bases, &opts);
    assert!(run(&plan).is_clean());
    assert_eq!(std::fs::read(&target).unwrap(), b"new");
}

#[test]
fn an_older_source_over_a_newer_target_is_flagged_before_it_runs() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"old").unwrap();
    std::fs::write(right.path().join("a.txt"), b"new").unwrap();
    RealFs
        .set_modified(
            &left.path().join("a.txt"),
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_000),
        )
        .unwrap();
    RealFs
        .set_modified(
            &right.path().join("a.txt"),
            SystemTime::UNIX_EPOCH + Duration::from_secs(2_000),
        )
        .unwrap();

    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let plan = plan_copy(&selection(&root, &["a.txt"]), Side::Left, bases, &options());
    assert!(plan.conflicts().contains(&Conflict::OverwriteNewer));

    let cancel = Cancel::new();
    let policy = DeclineConflicts;
    let report = execute(
        &plan,
        &ExecutionContext::new(&RealFs, &cancel, Journaling::Disabled).with_policy(&policy),
    );
    assert!(!report.is_clean());
    assert_eq!(std::fs::read(right.path().join("a.txt")).unwrap(), b"new");
}

#[test]
fn a_backup_is_taken_before_the_target_is_replaced() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"new").unwrap();
    std::fs::write(right.path().join("a.txt"), b"old").unwrap();
    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let mut opts = options();
    opts.backup = Some(crate::ops::plan::BackupOptions::default());
    let plan = plan_copy(&selection(&root, &["a.txt"]), Side::Left, bases, &opts);
    assert!(run(&plan).is_clean());
    assert_eq!(std::fs::read(right.path().join("a.txt")).unwrap(), b"new");
    assert_eq!(
        std::fs::read(right.path().join("a.txt.bak")).unwrap(),
        b"old"
    );
}

#[test]
fn move_across_directories_leaves_nothing_behind() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::create_dir(left.path().join("sub")).unwrap();
    std::fs::write(left.path().join("sub/a.txt"), b"payload").unwrap();

    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let plan = plan_move(&selection(&root, &["sub"]), Side::Left, bases, &options());
    assert!(run(&plan).is_clean());
    assert_eq!(
        std::fs::read(right.path().join("sub/a.txt")).unwrap(),
        b"payload"
    );
    assert!(!left.path().join("sub/a.txt").exists());
    assert!(!left.path().join("sub").exists());
}

#[test]
fn a_move_that_cannot_rename_falls_back_to_a_verified_copy() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"payload").unwrap();
    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let mut opts = options();
    opts.verify = Verify::Size;
    let plan = plan_move(&selection(&root, &["a.txt"]), Side::Left, bases, &opts);

    // A rename is refused, which is what a cross-volume move looks like.
    let fs = FaultFs::new(vec![Fault {
        call: "rename",
        path_contains: "/a.txt".to_string(),
        kind: io::ErrorKind::PermissionDenied,
    }]);
    let cancel = Cancel::new();
    let report = execute(
        &plan,
        &ExecutionContext::new(&fs, &cancel, Journaling::Disabled),
    );
    // The rename of the temporary onto the target is refused too, so the copy
    // cannot land and the source must survive untouched.
    assert!(!report.is_clean());
    assert_eq!(
        std::fs::read(left.path().join("a.txt")).unwrap(),
        b"payload"
    );
    assert!(fs.saw("open_read"));
}

// ------------------------------------------------- copy and move to folder

#[test]
fn keep_relative_strips_the_shared_folder() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(left.path().join("a/b")).unwrap();
    std::fs::write(left.path().join("a/b/one.txt"), b"1").unwrap();
    std::fs::write(left.path().join("a/two.txt"), b"2").unwrap();

    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let sel = selection(&root, &["a/b/one.txt", "a/two.txt"]);
    let plan = plan_to_folder(
        &sel,
        Side::Left,
        bases,
        target.path(),
        PathOption::KeepRelative,
        &options(),
        false,
        &RealFs,
    );
    assert!(run(&plan).is_clean());
    assert_eq!(
        std::fs::read(target.path().join("b/one.txt")).unwrap(),
        b"1"
    );
    assert_eq!(std::fs::read(target.path().join("two.txt")).unwrap(), b"2");
}

#[test]
fn keeping_the_base_structure_recreates_the_whole_path() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(left.path().join("a/b")).unwrap();
    std::fs::write(left.path().join("a/b/one.txt"), b"1").unwrap();

    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let plan = plan_to_folder(
        &selection(&root, &["a/b/one.txt"]),
        Side::Left,
        bases,
        target.path(),
        PathOption::KeepBase,
        &options(),
        false,
        &RealFs,
    );
    assert!(run(&plan).is_clean());
    assert!(target.path().join("a/b/one.txt").exists());
}

#[test]
fn flattening_drops_every_path_component() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(left.path().join("a/b")).unwrap();
    std::fs::write(left.path().join("a/b/one.txt"), b"1").unwrap();

    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let plan = plan_to_folder(
        &selection(&root, &["a/b/one.txt"]),
        Side::Left,
        bases,
        target.path(),
        PathOption::Flatten,
        &options(),
        true,
        &RealFs,
    );
    assert!(run(&plan).is_clean());
    assert!(target.path().join("one.txt").exists());
    assert!(!left.path().join("a/b/one.txt").exists());
}

// ------------------------------------------------------------------ delete

#[test]
fn delete_removes_contents_before_their_folder() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(left.path().join("sub/deep")).unwrap();
    std::fs::write(left.path().join("sub/deep/a.txt"), b"x").unwrap();

    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let plan = plan_delete(&selection(&root, &["sub"]), Sides::Left, bases, &options());
    assert!(run(&plan).is_clean());
    assert!(!left.path().join("sub").exists());
}

/// Creates a file the platform reports as hidden, and returns its name. Off
/// Windows the leading period is what the scan reads as hidden.
fn hidden_file(dir: &Path) -> String {
    let name = if cfg!(windows) {
        "secret.txt"
    } else {
        ".secret"
    };
    let path = dir.join(name);
    std::fs::write(&path, b"h").unwrap();
    if cfg!(windows) {
        RealFs
            .set_attributes(
                &path,
                &AttributeChange {
                    hidden: Some(true),
                    ..AttributeChange::default()
                },
            )
            .unwrap();
    }
    name.to_owned()
}

#[test]
fn a_copy_of_a_selected_folder_carries_its_hidden_items_only_when_the_option_is_set() {
    for include_hidden in [true, false] {
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        let sub = left.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("plain.txt"), b"p").unwrap();
        let hidden = hidden_file(&sub);

        let root = compared(left.path(), right.path());
        let bases = Bases {
            left: left.path(),
            right: right.path(),
        };
        let opts = OperationOptions {
            include_hidden,
            ..options()
        };
        let plan = plan_copy(&selection(&root, &["sub"]), Side::Left, bases, &opts);
        assert!(run(&plan).is_clean());

        assert!(right.path().join("sub/plain.txt").exists());
        assert_eq!(
            right.path().join("sub").join(&hidden).exists(),
            include_hidden,
            "include_hidden = {include_hidden}"
        );
    }
}

#[test]
fn a_hidden_item_the_selection_names_is_acted_on_whatever_the_option_says() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    let hidden = hidden_file(left.path());

    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let opts = OperationOptions {
        include_hidden: false,
        ..options()
    };
    let plan = plan_copy(&selection(&root, &[&hidden]), Side::Left, bases, &opts);
    assert!(run(&plan).is_clean());
    assert!(right.path().join(&hidden).exists());
}

#[test]
fn delete_reaches_both_sides_at_once() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"l").unwrap();
    std::fs::write(right.path().join("a.txt"), b"r").unwrap();

    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let plan = plan_delete(
        &selection(&root, &["a.txt"]),
        Sides::Both,
        bases,
        &options(),
    );
    assert!(run(&plan).is_clean());
    assert!(!left.path().join("a.txt").exists());
    assert!(!right.path().join("a.txt").exists());
}

#[test]
fn a_read_only_item_is_removed_only_when_the_option_allows_it() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    let path = left.path().join("a.txt");
    std::fs::write(&path, b"x").unwrap();
    RealFs
        .set_attributes(
            &path,
            &AttributeChange {
                read_only: Some(true),
                ..AttributeChange::default()
            },
        )
        .unwrap();

    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let sel = selection(&root, &["a.txt"]);
    let plan = plan_delete(&sel, Sides::Left, bases, &options());
    assert!(plan.conflicts().contains(&Conflict::DeleteReadOnly));
    assert!(!run(&plan).is_clean());
    assert!(path.exists());

    let mut opts = options();
    opts.clear_read_only_targets = true;
    let plan = plan_delete(&sel, Sides::Left, bases, &opts);
    assert!(run(&plan).is_clean());
    assert!(!path.exists());
}

/// Create a directory link, returning false when the platform or the policy
/// refuses one.
fn make_junction(link: &Path, target: &Path) -> bool {
    #[cfg(windows)]
    {
        std::process::Command::new("cmd")
            .args([
                "/C",
                "mklink",
                "/J",
                &link.to_string_lossy(),
                &target.to_string_lossy(),
            ])
            .output()
            .is_ok_and(|output| output.status.success())
    }
    #[cfg(not(windows))]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
}

#[test]
fn deleting_a_directory_link_never_reaches_its_target() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("precious.txt"), b"keep").unwrap();

    let link = left.path().join("link");
    if !make_junction(&link, outside.path()) {
        return;
    }

    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let plan = plan_delete(&selection(&root, &["link"]), Sides::Left, bases, &options());
    assert!(plan
        .steps
        .iter()
        .all(|step| matches!(step.action, StepAction::DeleteLink { .. })));
    assert!(plan.conflicts().contains(&Conflict::RemovesLinkOnly));
    assert!(run(&plan).is_clean());
    assert!(!link.exists());
    assert_eq!(
        std::fs::read(outside.path().join("precious.txt")).unwrap(),
        b"keep"
    );
}

// ------------------------------------------------------- rename and touch

#[test]
fn a_mask_rename_renumbers_a_whole_selection() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    for name in ["abc1.txt", "abc2.txt", "abc3.txt"] {
        std::fs::write(left.path().join(name), b"x").unwrap();
    }
    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let plan = plan_rename(
        &root,
        &selection(&root, &["abc1.txt", "abc2.txt", "abc3.txt"]),
        Sides::Left,
        bases,
        &RenameAction::Mask("abc?.bak".to_string()),
        &options(),
    )
    .unwrap();
    assert!(run(&plan).is_clean());
    for name in ["abc1.bak", "abc2.bak", "abc3.bak"] {
        assert!(left.path().join(name).exists(), "{name} missing");
    }
}

#[test]
fn a_capture_rename_rebuilds_the_stem() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("abc1.txt"), b"x").unwrap();
    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let plan = plan_rename(
        &root,
        &selection(&root, &["abc1.txt"]),
        Sides::Left,
        bases,
        &RenameAction::Regex {
            find: r".*(\d\.txt)".to_string(),
            replace: "xyz$1".to_string(),
        },
        &options(),
    )
    .unwrap();
    assert!(run(&plan).is_clean());
    assert!(left.path().join("xyz1.txt").exists());
}

#[test]
fn a_case_only_rename_lands_even_where_case_is_ignored() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("readme.txt"), b"x").unwrap();
    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let plan = plan_rename(
        &root,
        &selection(&root, &["readme.txt"]),
        Sides::Left,
        bases,
        &RenameAction::Mask("README.txt".to_string()),
        &options(),
    )
    .unwrap();
    assert!(plan.conflicts().contains(&Conflict::CaseOnlyRename));
    assert!(run(&plan).is_clean());

    let listed: Vec<String> = std::fs::read_dir(left.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(listed, vec!["README.txt".to_string()]);
}

#[test]
fn a_duplicate_new_name_is_refused_while_planning() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    for name in ["a1.txt", "a2.txt"] {
        std::fs::write(left.path().join(name), b"x").unwrap();
    }
    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let result = plan_rename(
        &root,
        &selection(&root, &["a1.txt", "a2.txt"]),
        Sides::Left,
        bases,
        &RenameAction::Mask("fixed.txt".to_string()),
        &options(),
    );
    assert!(result.is_err());
}

// ------------------------------------------ rename onto a name that is taken

/// Answers Proceed to the drift question, as a user who reads it and agrees
/// does.
struct ProceedOnDrift;

impl ErrorPolicy for ProceedOnDrift {
    fn on_error(&self, _step: &PlanStep, _attempt: u32, _message: &str) -> Decision {
        Decision::Skip
    }
    fn on_drift(&self, _step: &PlanStep, _drift: &Drift) -> ConflictDecision {
        ConflictDecision::Proceed
    }
}

fn run_with(plan: &OperationPlan, policy: &dyn ErrorPolicy) -> crate::ops::exec::ExecutionReport {
    let cancel = Cancel::new();
    execute(
        plan,
        &ExecutionContext::new(&RealFs, &cancel, Journaling::Disabled).with_policy(policy),
    )
}

fn rename_plan(
    root: &Node,
    rels: &[&str],
    sides: Sides,
    action: &RenameAction,
    bases: Bases<'_>,
    options: &OperationOptions,
) -> Result<OperationPlan, RenameError> {
    plan_rename(root, &selection(root, rels), sides, bases, action, options)
}

fn mask(name: &str) -> RenameAction {
    RenameAction::Mask(name.to_string())
}

/// `a.txt` and `b.txt` in the left folder, and nothing on the right.
fn two_left_files() -> (tempfile::TempDir, tempfile::TempDir) {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"a").unwrap();
    std::fs::write(left.path().join("b.txt"), b"b").unwrap();
    (left, right)
}

#[test]
fn a_rename_onto_an_unselected_file_asks_first_and_a_refusal_keeps_both_files() {
    let (left, right) = two_left_files();
    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let plan = rename_plan(
        &root,
        &["a.txt"],
        Sides::Left,
        &mask("b.txt"),
        bases,
        &options(),
    )
    .unwrap();
    assert_eq!(plan.steps.len(), 1, "{:?}", plan.steps);
    assert!(
        plan.steps[0].conflicts.contains(&Conflict::TargetExists),
        "{:?}",
        plan.steps[0].conflicts
    );

    let report = run_with(&plan, &DeclineConflicts);
    assert!(!report.is_clean());
    assert_eq!(std::fs::read(left.path().join("a.txt")).unwrap(), b"a");
    assert_eq!(std::fs::read(left.path().join("b.txt")).unwrap(), b"b");
}

#[test]
fn a_rename_onto_an_unselected_file_is_skipped_when_overwrite_is_off() {
    let (left, right) = two_left_files();
    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let options = OperationOptions {
        overwrite: false,
        ..options()
    };
    let plan = rename_plan(
        &root,
        &["a.txt"],
        Sides::Left,
        &mask("b.txt"),
        bases,
        &options,
    )
    .unwrap();

    let report = run(&plan);
    assert!(
        matches!(report.results[0].outcome, StepOutcome::Skipped { .. }),
        "{:?}",
        report.results
    );
    assert_eq!(std::fs::read(left.path().join("a.txt")).unwrap(), b"a");
    assert_eq!(std::fs::read(left.path().join("b.txt")).unwrap(), b"b");
}

/// A name that was free when the plan was built and is taken when the step
/// runs is reported as drift, and a Proceed to that question still does not
/// replace the item.
#[test]
fn a_rename_never_replaces_an_item_that_took_the_name_after_planning() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"a").unwrap();
    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let plan = rename_plan(
        &root,
        &["a.txt"],
        Sides::Left,
        &mask("b.txt"),
        bases,
        &options(),
    )
    .unwrap();
    assert!(plan.conflicts().is_empty());
    std::fs::write(left.path().join("b.txt"), b"late").unwrap();

    let unattended = run(&plan);
    match &unattended.results[0].outcome {
        StepOutcome::Skipped { reason } => assert!(reason.contains("absent"), "{reason}"),
        other => panic!("the step ran: {other:?}"),
    }
    let answered = run_with(&plan, &ProceedOnDrift);
    assert!(!answered.is_clean(), "{:?}", answered.results);
    assert_eq!(std::fs::read(left.path().join("a.txt")).unwrap(), b"a");
    assert_eq!(std::fs::read(left.path().join("b.txt")).unwrap(), b"late");
}

#[test]
fn a_confirmed_rename_onto_a_taken_name_keeps_a_numbered_backup() {
    let (left, right) = two_left_files();
    std::fs::write(left.path().join("b.txt.bak"), b"older backup").unwrap();
    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let options = OperationOptions {
        backup: Some(BackupOptions::default()),
        ..options()
    };
    let plan = rename_plan(
        &root,
        &["a.txt"],
        Sides::Left,
        &mask("b.txt"),
        bases,
        &options,
    )
    .unwrap();

    let journals = tempfile::tempdir().unwrap();
    let journal = Journal::create_in(journals.path()).unwrap();
    let cancel = Cancel::new();
    let report = execute(
        &plan,
        &ExecutionContext::new(&RealFs, &cancel, Journaling::To(&journal)),
    );
    let journal_path = journal.path().to_path_buf();
    drop(journal);
    assert!(report.is_clean(), "{:?}", report.results);
    assert!(!left.path().join("a.txt").exists());
    assert_eq!(std::fs::read(left.path().join("b.txt")).unwrap(), b"a");
    assert_eq!(
        std::fs::read(left.path().join("b.txt.bak")).unwrap(),
        b"older backup"
    );
    assert_eq!(std::fs::read(left.path().join("b.txt.bak1")).unwrap(), b"b");
    let settled = left.path().join("b.txt.bak1");
    assert_eq!(report.results[0].backup.as_deref(), Some(settled.as_path()));
    assert_eq!(journalled_backups(&journal_path), vec![Some(settled)]);
}

/// A Windows path drops a dot or a space at the end of a name, so `b.` and
/// `b ` reach `b`, and it reads a DOS device name as the device.
#[cfg(windows)]
#[test]
fn a_new_name_a_windows_path_does_not_reach_is_refused_while_planning() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"a").unwrap();
    std::fs::write(left.path().join("b"), b"b").unwrap();
    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    for (name, why) in [
        ("b.", "ends with a dot or a space"),
        ("b ", "ends with a dot or a space"),
        ("nul", "is a device name on Windows"),
        ("con.txt", "is a device name on Windows"),
    ] {
        let action = RenameAction::Regex {
            find: r"^a\.txt$".to_string(),
            replace: name.to_string(),
        };
        let plan = rename_plan(&root, &["a.txt"], Sides::Left, &action, bases, &options()).unwrap();
        assert!(plan.steps.is_empty(), "{name:?}: {:?}", plan.steps);
        let skip = plan
            .skipped
            .iter()
            .find(|skip| skip.path == Path::new("a.txt"))
            .expect("the refusal is recorded");
        assert!(
            skip.reason.contains(&format!("{name:?}")),
            "{}",
            skip.reason
        );
        assert!(skip.reason.contains(why), "{}", skip.reason);
        run(&plan);
    }
    assert_eq!(std::fs::read(left.path().join("a.txt")).unwrap(), b"a");
    assert_eq!(std::fs::read(left.path().join("b")).unwrap(), b"b");
}

#[test]
fn a_rename_on_both_sides_of_a_pair_is_not_refused_as_a_duplicate() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"left").unwrap();
    std::fs::write(right.path().join("a.txt"), b"right").unwrap();
    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let plan = rename_plan(
        &root,
        &["a.txt"],
        Sides::Both,
        &mask("c.txt"),
        bases,
        &options(),
    )
    .unwrap();
    assert_eq!(plan.steps.len(), 2, "{:?}", plan.steps);
    assert!(run(&plan).is_clean());
    assert_eq!(std::fs::read(left.path().join("c.txt")).unwrap(), b"left");
    assert_eq!(std::fs::read(right.path().join("c.txt")).unwrap(), b"right");
}

/// A volume that folds case reaches `B.txt` through the name `b.txt`.
#[test]
fn a_new_name_that_differs_from_a_selected_name_only_in_case_is_refused() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"a").unwrap();
    std::fs::write(left.path().join("B.txt"), b"b").unwrap();
    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let action = RenameAction::Regex {
        find: "^a".to_string(),
        replace: "b".to_string(),
    };
    let result = rename_plan(
        &root,
        &["a.txt", "B.txt"],
        Sides::Left,
        &action,
        bases,
        &options(),
    );
    assert!(matches!(result, Err(RenameError::Chain(_))), "{result:?}");
    assert_eq!(std::fs::read(left.path().join("B.txt")).unwrap(), b"b");
}

/// Where case is kept, `a.txt` and `A.txt` are two items. A case-only rename
/// of one onto the other, with both selected, is a chain. The tree is built
/// from listings, so the test does not need a volume that keeps case.
#[test]
fn a_case_only_rename_onto_another_selected_spelling_is_refused() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"a").unwrap();
    let mut listed = scan_side(left.path());
    let mut upper = listed.entries[Path::new("a.txt")].clone();
    upper.rel = PathBuf::from("A.txt");
    upper.name = "A.txt".to_owned();
    listed.entries.insert(PathBuf::from("A.txt"), upper);
    let root = align_trees(
        &listed,
        &ScanResult::default(),
        &AlignmentOptions::default(),
        &Cancel::new(),
    );
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let result = rename_plan(
        &root,
        &["a.txt", "A.txt"],
        Sides::Left,
        &mask("A.txt"),
        bases,
        &options(),
    );
    assert!(matches!(result, Err(RenameError::Chain(_))), "{result:?}");
}

#[test]
fn a_new_name_that_differs_from_an_unselected_name_only_in_case_is_flagged() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("x.txt"), b"x").unwrap();
    std::fs::write(left.path().join("b.txt"), b"b").unwrap();
    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let plan = rename_plan(
        &root,
        &["x.txt"],
        Sides::Left,
        &mask("B.txt"),
        bases,
        &options(),
    )
    .unwrap();
    assert!(
        plan.conflicts().contains(&Conflict::TargetExists),
        "{:?}",
        plan.steps
    );
    run_with(&plan, &DeclineConflicts);
    assert_eq!(std::fs::read(left.path().join("x.txt")).unwrap(), b"x");
    assert_eq!(std::fs::read(left.path().join("b.txt")).unwrap(), b"b");
}

#[test]
fn a_rename_never_puts_a_folder_over_another_item_or_a_file_over_a_folder() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"a").unwrap();
    std::fs::create_dir(left.path().join("sub")).unwrap();
    std::fs::write(left.path().join("sub/inner.txt"), b"inner").unwrap();
    std::fs::create_dir(left.path().join("other")).unwrap();
    std::fs::write(left.path().join("other/kept.txt"), b"kept").unwrap();
    let before = snapshot(left.path());
    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    for (selected, find, replace) in [
        ("a.txt", r"^a\.txt$", "sub"),
        ("other", "^other$", "a.txt"),
        ("other", "^other$", "sub"),
    ] {
        let action = RenameAction::Regex {
            find: find.to_string(),
            replace: replace.to_string(),
        };
        let plan =
            rename_plan(&root, &[selected], Sides::Left, &action, bases, &options()).unwrap();
        assert!(
            plan.steps.is_empty(),
            "{selected} to {replace}: {:?}",
            plan.steps
        );
        assert!(
            plan.skipped
                .iter()
                .any(|skip| skip.path == Path::new(selected)
                    && skip.reason.contains("replaces only a file with a file")),
            "{selected} to {replace}: {:?}",
            plan.skipped
        );
        run(&plan);
    }
    assert_eq!(snapshot(left.path()), before);
}

#[test]
fn touch_takes_the_other_side_timestamp_and_leaves_contents_alone() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"left").unwrap();
    std::fs::write(right.path().join("a.txt"), b"right").unwrap();
    let when = SystemTime::UNIX_EPOCH + Duration::from_secs(1_234_567_890);
    RealFs
        .set_modified(&right.path().join("a.txt"), when)
        .unwrap();

    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let plan = plan_touch(
        &selection(&root, &["a.txt"]),
        Sides::Left,
        bases,
        TouchSpec::FromOtherSide,
        &options(),
    );
    assert!(run(&plan).is_clean());
    assert_eq!(std::fs::read(left.path().join("a.txt")).unwrap(), b"left");
    let landed = RealFs
        .probe(&left.path().join("a.txt"))
        .unwrap()
        .modified
        .unwrap();
    assert!(landed.duration_since(when).unwrap() < Duration::from_secs(2));
}

#[test]
fn touching_a_folder_does_not_reach_its_contents() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::create_dir(left.path().join("sub")).unwrap();
    std::fs::write(left.path().join("sub/a.txt"), b"x").unwrap();
    let inner_before = RealFs
        .probe(&left.path().join("sub/a.txt"))
        .unwrap()
        .modified
        .unwrap();

    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let plan = plan_touch(
        &selection(&root, &["sub"]),
        Sides::Left,
        bases,
        TouchSpec::Explicit(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000)),
        &options(),
    );
    assert_eq!(plan.steps.len(), 1);
    run(&plan);
    let inner_after = RealFs
        .probe(&left.path().join("sub/a.txt"))
        .unwrap()
        .modified
        .unwrap();
    assert_eq!(inner_before, inner_after);
}

// ---------------------------------------- attributes, new folder, exchange

#[test]
fn attributes_are_written_to_every_selected_item() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    for name in ["a.txt", "b.txt"] {
        std::fs::write(left.path().join(name), b"x").unwrap();
    }
    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let plan = plan_attributes(
        &selection(&root, &["a.txt", "b.txt"]),
        Sides::Left,
        bases,
        AttributeChange {
            read_only: Some(true),
            ..AttributeChange::default()
        },
        &options(),
    );
    assert!(run(&plan).is_clean());
    for name in ["a.txt", "b.txt"] {
        assert!(RealFs.probe(&left.path().join(name)).unwrap().read_only);
        RealFs
            .set_attributes(
                &left.path().join(name),
                &AttributeChange {
                    read_only: Some(false),
                    ..AttributeChange::default()
                },
            )
            .unwrap();
    }
}

#[test]
fn a_new_folder_appears_on_both_sides() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let plan = plan_new_folder(
        &compared(left.path(), right.path()),
        Path::new(""),
        "fresh",
        Sides::Both,
        bases,
        &options(),
    );
    assert!(run(&plan).is_clean());
    assert!(left.path().join("fresh").is_dir());
    assert!(right.path().join("fresh").is_dir());
}

/// Following a link for comparison does not authorize separate mutations of
/// its target's children. The preview must describe the link-only plan.
#[test]
fn sync_preview_does_not_offer_separate_actions_below_a_linked_folder() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(left.path().join("linked/sub")).unwrap();
    std::fs::write(left.path().join("linked/sub/keep.txt"), b"target content").unwrap();
    let mut root = compared(left.path(), right.path());
    root.children[0].left.as_mut().unwrap().link = Some(crate::scan::LinkKind::DirectoryLink);
    let mut shown = preview(&root, &SyncPreset::MirrorToLeft);
    assert_eq!(shown.rows.len(), 1);
    assert_eq!(shown.pending().len(), 1);
    assert_eq!(shown.pending()[0].rel, Path::new("linked"));
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let plan = plan_sync(&root, &SyncPreset::MirrorToLeft, bases, &options(), &shown).unwrap();
    assert_eq!(plan.steps.len(), 1);
    assert_eq!(plan.steps[0].rel, Path::new("linked"));
    assert!(matches!(
        plan.steps[0].action,
        StepAction::DeleteLink { .. }
    ));
    shown.set_override(PathBuf::from("linked"), SyncAction::LeaveAlone);
    assert!(shown.pending().is_empty());
    assert!(
        plan_sync(&root, &SyncPreset::MirrorToLeft, bases, &options(), &shown)
            .unwrap()
            .steps
            .is_empty()
    );
}

#[test]
fn exchange_swaps_each_side_selection() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("l.txt"), b"left").unwrap();
    std::fs::write(right.path().join("r.txt"), b"right").unwrap();

    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let left_sel = selection(&root, &["l.txt"]);
    let right_sel = selection(&root, &["r.txt"]);
    let plan = plan_exchange(&left_sel, &right_sel, bases, &options());
    assert!(run(&plan).is_clean());
    assert_eq!(std::fs::read(right.path().join("l.txt")).unwrap(), b"left");
    assert_eq!(std::fs::read(left.path().join("r.txt")).unwrap(), b"right");
    assert!(!left.path().join("l.txt").exists());
    assert!(!right.path().join("r.txt").exists());
}

#[test]
fn exclude_offers_one_mask_per_type() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.log"), b"x").unwrap();
    std::fs::write(left.path().join("b.log"), b"x").unwrap();
    let root = compared(left.path(), right.path());
    let sel = selection(&root, &["a.log", "b.log"]);

    let by_type = exclude_masks(&sel, true);
    assert_eq!(by_type.files, vec!["*.log".to_string()]);
    let by_name = exclude_masks(&sel, false);
    assert_eq!(by_name.files.len(), 2);
}

// ------------------------------------------ cancellation, faults, recovery

#[test]
fn a_cancelled_copy_leaves_the_old_target_and_no_temporary_behind() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.bin"), vec![9u8; 200_000]).unwrap();
    let target = right.path().join("a.bin");
    std::fs::write(&target, b"previous").unwrap();

    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let plan = plan_copy(&selection(&root, &["a.bin"]), Side::Left, bases, &options());

    let journal_dir = tempfile::tempdir().unwrap();
    let journal_path = journal_dir.path().join("batch.jsonl");
    let journal = Journal::create(&journal_path).unwrap();
    let cancel = Cancel::new();
    let fs = CancellingFs {
        inner: RealFs,
        cancel: cancel.clone(),
    };
    let report = execute(
        &plan,
        &ExecutionContext::new(&fs, &cancel, Journaling::To(&journal)),
    );
    drop(journal);

    assert!(report.cancelled);
    assert_eq!(std::fs::read(&target).unwrap(), b"previous");

    let recovery = recover(&journal_path).unwrap();
    assert!(!recovery.leftover_temporaries.is_empty());
    assert!(recovery.clean_up(&RealFs).is_empty());

    let strays: Vec<_> = std::fs::read_dir(right.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|name| crate::ops::exec::is_temporary_name(name))
        .collect();
    assert!(strays.is_empty(), "left temporary files: {strays:?}");
}

#[test]
fn one_failed_step_does_not_stop_the_batch() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    for name in ["a.txt", "b.txt", "c.txt"] {
        std::fs::write(left.path().join(name), b"x").unwrap();
    }
    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let plan = plan_copy(
        &selection(&root, &["a.txt", "b.txt", "c.txt"]),
        Side::Left,
        bases,
        &options(),
    );

    let fs = FaultFs::new(vec![Fault {
        call: "create_new",
        path_contains: "b.txt".to_string(),
        kind: io::ErrorKind::PermissionDenied,
    }]);
    let cancel = Cancel::new();
    let report = execute(
        &plan,
        &ExecutionContext::new(&fs, &cancel, Journaling::Disabled),
    );

    assert_eq!(report.results.len(), 3);
    assert_eq!(report.completed(), 2);
    assert_eq!(report.failures().len(), 1);
    assert!(right.path().join("a.txt").exists());
    assert!(!right.path().join("b.txt").exists());
    assert!(right.path().join("c.txt").exists());
}

#[test]
fn every_primitive_can_be_failed_in_turn_without_losing_the_target() {
    let calls = [
        ("probe", "a.txt"),
        ("open_read", "a.txt"),
        ("create_new", "a.txt"),
        ("rename", "a.txt"),
    ];
    for (call, needle) in calls {
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        std::fs::write(left.path().join("a.txt"), b"new").unwrap();
        std::fs::write(right.path().join("a.txt"), b"old").unwrap();
        let root = compared(left.path(), right.path());
        let bases = Bases {
            left: left.path(),
            right: right.path(),
        };
        let plan = plan_copy(&selection(&root, &["a.txt"]), Side::Left, bases, &options());

        let fs = FaultFs::new(vec![Fault {
            call,
            path_contains: needle.to_string(),
            kind: io::ErrorKind::PermissionDenied,
        }]);
        let cancel = Cancel::new();
        let report = execute(
            &plan,
            &ExecutionContext::new(&fs, &cancel, Journaling::Disabled),
        );
        assert!(!report.is_clean(), "{call} should have failed the step");
        assert_eq!(
            std::fs::read(right.path().join("a.txt")).unwrap(),
            b"old",
            "{call} destroyed the previous target"
        );
        assert_eq!(std::fs::read(left.path().join("a.txt")).unwrap(), b"new");
    }
}

#[test]
fn the_journal_brackets_every_step_it_records() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"x").unwrap();
    std::fs::write(left.path().join("b.txt"), b"y").unwrap();
    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let plan = plan_copy(
        &selection(&root, &["a.txt", "b.txt"]),
        Side::Left,
        bases,
        &options(),
    );

    let journal_dir = tempfile::tempdir().unwrap();
    let journal_path = journal_dir.path().join("batch.jsonl");
    let journal = Journal::create(&journal_path).unwrap();
    let cancel = Cancel::new();
    let report = execute(
        &plan,
        &ExecutionContext::new(&RealFs, &cancel, Journaling::To(&journal)),
    );
    drop(journal);
    assert!(report.is_clean());

    let recovery = recover(&journal_path).unwrap();
    assert!(recovery.completed);
    assert!(recovery.unfinished.is_empty());
    assert!(!recovery.was_interrupted());
    let begins = recovery
        .records
        .iter()
        .filter(|r| matches!(r, JournalRecord::StepBegin { .. }))
        .count();
    let ends = recovery
        .records
        .iter()
        .filter(|r| matches!(r, JournalRecord::StepEnd { .. }))
        .count();
    assert_eq!(begins, 2);
    assert_eq!(ends, 2);
    assert!(matches!(
        recovery.records.first(),
        Some(JournalRecord::BatchStart { steps: 2, .. })
    ));
}

#[test]
fn a_retrying_policy_attempts_the_same_step_again() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"x").unwrap();
    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let plan = plan_copy(&selection(&root, &["a.txt"]), Side::Left, bases, &options());

    let fs = FaultFs::new(vec![Fault {
        call: "open_read",
        path_contains: "a.txt".to_string(),
        kind: io::ErrorKind::PermissionDenied,
    }]);
    let policy = RetryOnce(AtomicUsize::new(0));
    let cancel = Cancel::new();
    let report = execute(
        &plan,
        &ExecutionContext::new(&fs, &cancel, Journaling::Disabled).with_policy(&policy),
    );
    assert!(!report.is_clean());
    assert_eq!(policy.0.load(Ordering::SeqCst), 2);
}

#[test]
fn progress_reports_bytes_and_step_boundaries() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.bin"), vec![3u8; 40_000]).unwrap();
    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let plan = plan_copy(&selection(&root, &["a.bin"]), Side::Left, bases, &options());

    let started = AtomicUsize::new(0);
    let finished = AtomicUsize::new(0);
    let blocks = AtomicUsize::new(0);
    let sink = |event: Progress<'_>| match event {
        Progress::StepStarted { .. } => {
            started.fetch_add(1, Ordering::SeqCst);
        }
        Progress::StepFinished { .. } => {
            finished.fetch_add(1, Ordering::SeqCst);
        }
        Progress::Bytes { .. } => {
            blocks.fetch_add(1, Ordering::SeqCst);
        }
    };
    let cancel = Cancel::new();
    let _ = execute(
        &plan,
        &ExecutionContext::new(&RealFs, &cancel, Journaling::Disabled).with_progress(&sink),
    );
    assert_eq!(started.load(Ordering::SeqCst), 1);
    assert_eq!(finished.load(Ordering::SeqCst), 1);
    assert!(blocks.load(Ordering::SeqCst) > 1);
}

#[test]
fn a_step_pointing_outside_the_base_folders_never_runs() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let victim = outside.path().join("precious.txt");
    std::fs::write(&victim, b"keep").unwrap();

    let mut plan = OperationPlan::new(
        crate::ops::plan::OperationKind::Delete,
        vec![left.path().to_path_buf(), right.path().to_path_buf()],
        options(),
    );
    plan.steps.push(PlanStep {
        index: 0,
        action: StepAction::DeleteFile {
            path: victim.clone(),
        },
        bytes: 0,
        conflicts: Vec::new(),
        backup: None,
        side: None,
        rel: PathBuf::from("precious.txt"),
        expected: crate::ops::plan::StepExpectation::default(),
    });
    assert!(!plan.is_contained());

    let report = run(&plan);
    assert!(matches!(
        report.results[0].outcome,
        StepOutcome::Skipped { .. }
    ));
    assert_eq!(std::fs::read(&victim).unwrap(), b"keep");
}

// -------------------------------------------------------------------- sync

fn mirror_right(left: &Path, right: &Path, opts: &OperationOptions) -> OperationPlan {
    let root = compared(left, right);
    let bases = Bases { left, right };
    let view = preview(&root, &SyncPreset::MirrorToRight);
    plan_sync(&root, &SyncPreset::MirrorToRight, bases, opts, &view).unwrap()
}

#[test]
fn a_mirror_copies_differences_and_removes_the_targets_orphans() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("same.txt"), b"same").unwrap();
    std::fs::write(right.path().join("same.txt"), b"same").unwrap();
    std::fs::write(left.path().join("only-left.txt"), b"l").unwrap();
    std::fs::write(right.path().join("only-right.txt"), b"r").unwrap();

    let plan = mirror_right(left.path(), right.path(), &options());
    assert!(plan.is_contained());
    assert!(run(&plan).is_clean());

    assert_eq!(
        std::fs::read(right.path().join("only-left.txt")).unwrap(),
        b"l"
    );
    assert!(!right.path().join("only-right.txt").exists());
    assert!(!left.path().join("only-right.txt").exists());
    assert_eq!(
        std::fs::read(left.path().join("only-left.txt")).unwrap(),
        b"l"
    );
}

#[test]
fn a_mirror_never_removes_anything_outside_the_base_folders() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("precious.txt"), b"keep").unwrap();
    std::fs::write(right.path().join("orphan.txt"), b"r").unwrap();

    let plan = mirror_right(left.path(), right.path(), &options());
    for step in &plan.steps {
        assert!(
            crate::ops::plan::path_is_within(right.path(), step.action.target())
                || crate::ops::plan::path_is_within(left.path(), step.action.target())
        );
    }
    assert!(run(&plan).is_clean());
    assert_eq!(
        std::fs::read(outside.path().join("precious.txt")).unwrap(),
        b"keep"
    );
}

#[test]
fn a_mirror_removes_a_whole_orphan_subtree() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(right.path().join("gone/deep")).unwrap();
    std::fs::write(right.path().join("gone/deep/a.txt"), b"x").unwrap();
    std::fs::write(left.path().join("keep.txt"), b"k").unwrap();

    let plan = mirror_right(left.path(), right.path(), &options());
    assert!(run(&plan).is_clean());
    assert!(!right.path().join("gone").exists());
    assert_eq!(std::fs::read(right.path().join("keep.txt")).unwrap(), b"k");
}

#[test]
fn an_update_method_leaves_orphans_on_the_target_alone() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("new.txt"), b"n").unwrap();
    std::fs::write(right.path().join("keep.txt"), b"k").unwrap();

    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let view = preview(&root, &SyncPreset::UpdateRight);
    let plan = plan_sync(&root, &SyncPreset::UpdateRight, bases, &options(), &view).unwrap();
    assert!(plan.steps.iter().all(|s| !s.action.is_destructive()));
    assert!(run(&plan).is_clean());
    assert!(right.path().join("new.txt").exists());
    assert!(right.path().join("keep.txt").exists());
}

#[test]
fn an_overridden_row_replaces_the_action_the_rules_chose() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"l").unwrap();
    std::fs::write(left.path().join("b.txt"), b"l").unwrap();

    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let mut view = preview(&root, &SyncPreset::MirrorToRight);
    assert_eq!(view.pending().len(), 2);
    view.set_override(PathBuf::from("b.txt"), SyncAction::LeaveAlone);
    assert_eq!(view.pending().len(), 1);

    let plan = plan_sync(&root, &SyncPreset::MirrorToRight, bases, &options(), &view).unwrap();
    assert!(run(&plan).is_clean());
    assert!(right.path().join("a.txt").exists());
    assert!(!right.path().join("b.txt").exists());
}

#[test]
fn sync_preserves_an_excluded_child_of_a_deleted_folder() {
    for recycle in [false, true] {
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        std::fs::create_dir(right.path().join("orphan")).unwrap();
        std::fs::write(right.path().join("orphan/keep.txt"), b"keep").unwrap();
        std::fs::write(right.path().join("orphan/drop.txt"), b"drop").unwrap();
        let root = compared(left.path(), right.path());
        let bases = Bases {
            left: left.path(),
            right: right.path(),
        };
        let mut view = preview(&root, &SyncPreset::MirrorToRight);
        view.set_override(PathBuf::from("orphan/keep.txt"), SyncAction::LeaveAlone);
        let mut opts = options();
        opts.use_recycle_bin = recycle;
        let plan = plan_sync(&root, &SyncPreset::MirrorToRight, bases, &opts, &view).unwrap();
        assert!(plan
            .steps
            .iter()
            .all(|step| step.rel != Path::new("orphan/keep.txt")));
        assert!(
            plan.steps
                .iter()
                .all(|step| step.rel != Path::new("orphan")),
            "the parent must not delete a protected child"
        );
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].rel, Path::new("orphan/drop.txt"));
        if !recycle {
            assert!(run(&plan).is_clean());
            assert_eq!(
                std::fs::read(right.path().join("orphan/keep.txt")).unwrap(),
                b"keep"
            );
            assert!(!right.path().join("orphan/drop.txt").exists());
        }
    }
}

#[test]
fn a_preview_row_exists_for_every_pair_the_filters_left() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::create_dir(left.path().join("sub")).unwrap();
    std::fs::write(left.path().join("sub/a.txt"), b"x").unwrap();
    let root = compared(left.path(), right.path());
    let view = preview(&root, &SyncPreset::MirrorToRight);
    let names: Vec<&str> = view.rows.iter().map(|row| row.name.as_str()).collect();
    assert_eq!(names, vec!["sub", "a.txt"]);
    assert!(view
        .rows
        .iter()
        .all(|row| row.status == NodeStatus::LeftOrphan));
}

#[test]
fn an_empty_folder_is_created_only_when_the_option_asks_for_it() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::create_dir(left.path().join("empty")).unwrap();

    let plan = mirror_right(left.path(), right.path(), &options());
    run(&plan);
    assert!(!right.path().join("empty").exists());

    let mut opts = options();
    opts.create_empty_folders = true;
    let plan = mirror_right(left.path(), right.path(), &opts);
    assert!(run(&plan).is_clean());
    assert!(right.path().join("empty").is_dir());
}

#[test]
fn mirroring_twice_finds_nothing_left_to_do() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(left.path().join("sub")).unwrap();
    std::fs::write(left.path().join("sub/a.txt"), b"one").unwrap();
    std::fs::write(left.path().join("b.txt"), b"two").unwrap();
    std::fs::write(right.path().join("stale.txt"), b"gone").unwrap();

    let plan = mirror_right(left.path(), right.path(), &options());
    assert!(run(&plan).is_clean());
    let second = mirror_right(left.path(), right.path(), &options());
    assert!(second.steps.is_empty(), "left over: {:?}", second.steps);
}

#[test]
fn long_paths_survive_a_copy() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    let deep: PathBuf = (0..12).fold(PathBuf::new(), |acc, index| {
        acc.join(format!("segment-{index}-padded-out-to-a-useful-length"))
    });
    std::fs::create_dir_all(left.path().join(&deep)).unwrap();
    std::fs::write(left.path().join(&deep).join("a.txt"), b"deep").unwrap();
    assert!(
        left.path()
            .join(&deep)
            .join("a.txt")
            .to_string_lossy()
            .len()
            > 260
    );

    let plan = mirror_right(left.path(), right.path(), &options());
    assert!(run(&plan).is_clean());
    assert_eq!(
        std::fs::read(right.path().join(&deep).join("a.txt")).unwrap(),
        b"deep"
    );
}

// ---------------------------------------------------------------- property

mod properties {
    use super::{compared, mirror_right, options, run, snapshot};
    use crate::compare::NodeStatus;
    use proptest::prelude::*;

    fn tree_strategy() -> impl Strategy<Value = Vec<(String, u8, bool)>> {
        proptest::collection::vec(
            (
                proptest::sample::select(vec![
                    "a.txt".to_string(),
                    "b.txt".to_string(),
                    "c.txt".to_string(),
                    "sub/d.txt".to_string(),
                    "sub/e.txt".to_string(),
                    "sub/deep/f.txt".to_string(),
                ]),
                any::<u8>(),
                any::<bool>(),
            ),
            0..8,
        )
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(24))]

        /// After a mirror, a fresh comparison reports no differences at all,
        /// and the side the mirror read from is byte for byte what it was.
        #[test]
        fn mirror_makes_the_two_sides_agree(items in tree_strategy(), extras in tree_strategy()) {
            let left = tempfile::tempdir().unwrap();
            let right = tempfile::tempdir().unwrap();

            for (name, byte, on_left) in &items {
                let root = if *on_left { left.path() } else { right.path() };
                let path = root.join(name);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).unwrap();
                }
                std::fs::write(&path, vec![*byte; usize::from(*byte) + 1]).unwrap();
            }
            for (name, byte, _) in &extras {
                let path = right.path().join(name);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).unwrap();
                }
                std::fs::write(&path, vec![byte.wrapping_add(1); 3]).unwrap();
            }

            let before = snapshot(left.path());
            let mut opts = options();
            opts.create_empty_folders = true;
            let plan = mirror_right(left.path(), right.path(), &opts);
            let report = run(&plan);
            prop_assert!(report.is_clean(), "{:?}", report.failures());

            prop_assert_eq!(snapshot(left.path()), before);

            let after = compared(left.path(), right.path());
            let mut offenders = Vec::new();
            after.walk(&mut |node| {
                if node.status != NodeStatus::Same && !node.rel.as_os_str().is_empty() {
                    offenders.push((node.rel.clone(), node.status));
                }
            });
            prop_assert!(offenders.is_empty(), "not reconciled: {:?}", offenders);
        }
    }
}

// ------------------------------------------------ data loss regression tests

/// Compare two roots with scan options of the caller's choosing, so a
/// deliberately partial listing can be put in front of the planners.
fn compared_with(left: &Path, right: &Path, left_options: &ScanOptions) -> Node {
    let left_scan = scan_with(left, left_options, &Cancel::new(), &|_| {}).unwrap();
    let mut root = align_trees(
        &left_scan,
        &scan_side(right),
        &AlignmentOptions::default(),
        &Cancel::new(),
    );
    compare_quick(&mut root, &CompareOptions::default());
    root
}

#[test]
fn exchanging_one_path_swaps_both_files() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"left").unwrap();
    std::fs::write(right.path().join("a.txt"), b"right").unwrap();

    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let sel = selection(&root, &["a.txt"]);
    let plan = plan_exchange(&sel, &sel, bases, &options());
    assert_eq!(plan.steps.len(), 1);
    assert!(matches!(
        plan.steps[0].action,
        StepAction::ExchangeFiles { .. }
    ));
    assert!(run(&plan).is_clean());

    assert_eq!(std::fs::read(left.path().join("a.txt")).unwrap(), b"right");
    assert_eq!(std::fs::read(right.path().join("a.txt")).unwrap(), b"left");
}

#[test]
fn an_exchange_that_fails_at_any_rename_leaves_both_files_untouched() {
    for position in 0..3usize {
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        std::fs::write(left.path().join("a.txt"), b"left").unwrap();
        std::fs::write(right.path().join("a.txt"), b"right").unwrap();

        let root = compared(left.path(), right.path());
        let bases = Bases {
            left: left.path(),
            right: right.path(),
        };
        let sel = selection(&root, &["a.txt"]);
        let plan = plan_exchange(&sel, &sel, bases, &options());

        let fs = FaultFs::failing_rename(position);
        let cancel = Cancel::new();
        let report = execute(
            &plan,
            &ExecutionContext::new(&fs, &cancel, Journaling::Disabled),
        );
        assert!(!report.is_clean(), "rename {position} should have failed");
        assert_eq!(
            std::fs::read(left.path().join("a.txt")).unwrap(),
            b"left",
            "rename {position} lost the left file"
        );
        assert_eq!(
            std::fs::read(right.path().join("a.txt")).unwrap(),
            b"right",
            "rename {position} lost the right file"
        );
        let strays: Vec<String> = std::fs::read_dir(right.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|name| crate::ops::exec::is_temporary_name(name))
            .collect();
        assert!(strays.is_empty(), "rename {position} left {strays:?}");
    }
}

#[test]
fn a_mirror_leaves_alone_what_sits_beside_a_listing_it_could_not_read() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::create_dir(left.path().join("sub")).unwrap();
    std::fs::write(left.path().join("sub/shared.txt"), b"x").unwrap();
    std::fs::create_dir(right.path().join("sub")).unwrap();
    std::fs::write(right.path().join("sub/shared.txt"), b"x").unwrap();
    std::fs::write(right.path().join("sub/precious.txt"), b"keep").unwrap();

    // A depth limit is one of the ways a listing ends up partial: the left
    // "sub" is known to exist and its contents were never read.
    let truncated = ScanOptions {
        max_depth: Some(1),
        ..ScanOptions::default()
    };
    let root = compared_with(left.path(), right.path(), &truncated);
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let view = preview(&root, &SyncPreset::MirrorToRight);
    let plan = plan_sync(&root, &SyncPreset::MirrorToRight, bases, &options(), &view).unwrap();

    assert!(
        plan.steps.iter().all(|step| !step.action.is_destructive()),
        "planned a removal under an unread listing: {:?}",
        plan.steps
    );
    assert!(plan
        .refusals()
        .iter()
        .any(|skip| skip.conflict == Some(Conflict::CounterpartUnreadable)));
    run(&plan);
    assert_eq!(
        std::fs::read(right.path().join("sub/precious.txt")).unwrap(),
        b"keep"
    );
}

#[test]
fn a_cancelled_scan_produces_no_sync_plan_at_all() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(right.path().join("orphan.txt"), b"keep").unwrap();

    let cancel = Cancel::new();
    cancel.cancel();
    let left_scan = scan_with(left.path(), &ScanOptions::default(), &cancel, &|_| {}).unwrap();
    let root = align_trees(
        &left_scan,
        &scan_side(right.path()),
        &AlignmentOptions::default(),
        &Cancel::new(),
    );
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let view = preview(&root, &SyncPreset::MirrorToRight);
    let refused = plan_sync(&root, &SyncPreset::MirrorToRight, bases, &options(), &view);
    assert_eq!(refused.err(), Some(crate::sync::SyncRefused::ScanCancelled));
    assert_eq!(
        std::fs::read(right.path().join("orphan.txt")).unwrap(),
        b"keep"
    );
}

#[test]
fn a_partial_listing_stops_a_plain_recursive_delete() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::create_dir(left.path().join("sub")).unwrap();
    std::fs::write(left.path().join("sub/inside.txt"), b"keep").unwrap();

    let truncated = ScanOptions {
        max_depth: Some(1),
        ..ScanOptions::default()
    };
    let root = compared_with(left.path(), right.path(), &truncated);
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let plan = plan_delete(&selection(&root, &["sub"]), Sides::Left, bases, &options());
    assert!(plan.steps.is_empty());
    assert!(plan
        .refusals()
        .iter()
        .any(|skip| skip.conflict == Some(Conflict::CounterpartUnreadable)));
    assert_eq!(
        std::fs::read(left.path().join("sub/inside.txt")).unwrap(),
        b"keep"
    );
}

#[test]
fn a_filtered_out_pair_is_never_read_as_absent_on_one_side() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("keep.txt"), b"k").unwrap();
    std::fs::write(left.path().join("notes.log"), b"l").unwrap();
    std::fs::write(right.path().join("keep.txt"), b"k").unwrap();
    std::fs::write(right.path().join("notes.log"), b"l").unwrap();

    let mut root = compared(left.path(), right.path());
    let names = crate::filter::NameFilters::from_lists("*.txt", "", "", "");
    crate::compare::apply_filters(
        &mut root,
        &names,
        &crate::filter::OtherFilters::default(),
        &crate::filter::FilterContext::default(),
    );

    // The filter drops the pair, not one side of it, so no rule can read the
    // excluded name as an orphan.
    let mut seen = Vec::new();
    root.walk(&mut |node| {
        if node.rel.as_os_str().is_empty() {
            return;
        }
        seen.push((node.rel.clone(), node.status));
    });
    assert!(
        seen.iter().all(|(_, status)| !status.is_orphan()),
        "{seen:?}"
    );

    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let view = preview(&root, &SyncPreset::MirrorToRight);
    let plan = plan_sync(&root, &SyncPreset::MirrorToRight, bases, &options(), &view).unwrap();
    assert!(plan.steps.iter().all(|step| !step.action.is_destructive()));
    assert!(run(&plan).is_clean());
    assert_eq!(std::fs::read(right.path().join("notes.log")).unwrap(), b"l");
}

#[test]
fn a_sync_never_reaches_through_a_followed_junction() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("precious.txt"), b"keep").unwrap();
    if !make_junction(&right.path().join("link"), outside.path()) {
        return;
    }

    let followed = ScanOptions {
        follow_links: true,
        ..ScanOptions::default()
    };
    let left_scan = scan_with(left.path(), &followed, &Cancel::new(), &|_| {}).unwrap();
    let right_scan = scan_with(right.path(), &followed, &Cancel::new(), &|_| {}).unwrap();
    let mut root = align_trees(
        &left_scan,
        &right_scan,
        &AlignmentOptions::default(),
        &Cancel::new(),
    );
    compare_quick(&mut root, &CompareOptions::default());

    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let view = preview(&root, &SyncPreset::MirrorToRight);
    let plan = plan_sync(&root, &SyncPreset::MirrorToRight, bases, &options(), &view).unwrap();
    let outside_text = outside.path().to_string_lossy().to_string();
    for step in &plan.steps {
        assert!(
            !step
                .action
                .target()
                .to_string_lossy()
                .contains(&outside_text),
            "step reaches outside: {:?}",
            step.action
        );
    }
    run(&plan);
    assert_eq!(
        std::fs::read(outside.path().join("precious.txt")).unwrap(),
        b"keep"
    );
}

#[test]
fn a_junction_swapped_in_after_planning_stops_the_step() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("a.txt"), b"keep").unwrap();

    std::fs::create_dir(left.path().join("sub")).unwrap();
    std::fs::write(left.path().join("sub/a.txt"), b"doomed").unwrap();
    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let plan = plan_delete(
        &selection(&root, &["sub/a.txt"]),
        Sides::Left,
        bases,
        &options(),
    );
    assert_eq!(plan.steps.len(), 1);

    // Between planning and acting, the step's parent directory becomes a link
    // to somewhere else entirely.
    std::fs::remove_dir_all(left.path().join("sub")).unwrap();
    if !make_junction(&left.path().join("sub"), outside.path()) {
        return;
    }

    let report = run(&plan);
    assert!(
        matches!(report.results[0].outcome, StepOutcome::Skipped { .. }),
        "{:?}",
        report.results[0].outcome
    );
    assert_eq!(
        std::fs::read(outside.path().join("a.txt")).unwrap(),
        b"keep"
    );
}

#[test]
fn a_renaming_move_still_takes_a_backup() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"new").unwrap();
    std::fs::write(right.path().join("a.txt"), b"old").unwrap();

    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let mut opts = options();
    opts.backup = Some(crate::ops::plan::BackupOptions::default());
    let plan = plan_move(&selection(&root, &["a.txt"]), Side::Left, bases, &opts);
    let report = run(&plan);
    assert!(report.is_clean());
    assert_eq!(std::fs::read(right.path().join("a.txt")).unwrap(), b"new");
    assert_eq!(
        std::fs::read(right.path().join("a.txt.bak")).unwrap(),
        b"old",
        "the rename path skipped the backup"
    );
    assert_eq!(
        report.results[0].backup,
        Some(right.path().join("a.txt.bak"))
    );
}

#[test]
fn a_copy_that_does_not_match_its_source_never_replaces_the_target() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.bin"), vec![4u8; 8_000]).unwrap();
    let target = right.path().join("a.bin");
    std::fs::write(&target, b"previous").unwrap();

    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let mut opts = options();
    opts.verify = Verify::Size;
    let plan = plan_copy(&selection(&root, &["a.bin"]), Side::Left, bases, &opts);

    let fs = ShortWriteFs { inner: RealFs };
    let cancel = Cancel::new();
    let report = execute(
        &plan,
        &ExecutionContext::new(&fs, &cancel, Journaling::Disabled),
    );
    assert!(!report.is_clean());
    assert_eq!(
        std::fs::read(&target).unwrap(),
        b"previous",
        "a failed check destroyed the old target"
    );
    let strays: Vec<String> = std::fs::read_dir(right.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|name| crate::ops::exec::is_temporary_name(name))
        .collect();
    assert!(strays.is_empty(), "left temporary files: {strays:?}");
}

#[test]
fn a_move_whose_source_is_read_only_writes_no_destination() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    let source = left.path().join("a.txt");
    std::fs::write(&source, b"payload").unwrap();
    let target = right.path().join("a.txt");
    std::fs::write(&target, b"old").unwrap();
    RealFs
        .set_attributes(
            &source,
            &AttributeChange {
                read_only: Some(true),
                ..AttributeChange::default()
            },
        )
        .unwrap();

    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let plan = plan_move(&selection(&root, &["a.txt"]), Side::Left, bases, &options());
    let report = run(&plan);

    assert!(!report.is_clean());
    assert_eq!(std::fs::read(&target).unwrap(), b"old");
    assert_eq!(std::fs::read(&source).unwrap(), b"payload");
    RealFs
        .set_attributes(
            &source,
            &AttributeChange {
                read_only: Some(false),
                ..AttributeChange::default()
            },
        )
        .unwrap();
}

#[test]
fn a_move_that_wrote_its_destination_never_reports_a_bare_skip() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"payload").unwrap();

    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let plan = plan_move(&selection(&root, &["a.txt"]), Side::Left, bases, &options());

    // Copying is forced, and the removal of the source is then refused.
    let fs = FaultFs::across_volumes(vec![Fault {
        call: "remove_file",
        path_contains: "a.txt".to_string(),
        kind: io::ErrorKind::PermissionDenied,
    }]);
    let cancel = Cancel::new();
    let report = execute(
        &plan,
        &ExecutionContext::new(&fs, &cancel, Journaling::Disabled),
    );
    assert!(!report.is_clean());
    let StepOutcome::CopiedSourceRemains { source, target, .. } = &report.results[0].outcome else {
        panic!("expected a distinct outcome, got {:?}", report.results[0]);
    };
    assert_eq!(source, &left.path().join("a.txt"));
    assert_eq!(target, &right.path().join("a.txt"));
    assert_eq!(
        std::fs::read(right.path().join("a.txt")).unwrap(),
        b"payload"
    );
}

#[test]
fn each_backup_keeps_the_one_before_it() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    let mut opts = options();
    opts.backup = Some(crate::ops::plan::BackupOptions::default());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };

    for (source, previous) in [(&b"one"[..], &b"first"[..]), (&b"two"[..], &b"second"[..])] {
        std::fs::write(left.path().join("a.txt"), source).unwrap();
        std::fs::write(right.path().join("a.txt"), previous).unwrap();
        let root = compared(left.path(), right.path());
        let plan = plan_copy(&selection(&root, &["a.txt"]), Side::Left, bases, &opts);
        assert!(run(&plan).is_clean());
    }

    assert_eq!(
        std::fs::read(right.path().join("a.txt.bak")).unwrap(),
        b"first"
    );
    assert_eq!(
        std::fs::read(right.path().join("a.txt.bak1")).unwrap(),
        b"second",
        "the second backup replaced the first"
    );
}

fn a_copy_with_a_backup() -> (tempfile::TempDir, tempfile::TempDir, OperationPlan) {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"new").unwrap();
    std::fs::write(right.path().join("a.txt"), b"old").unwrap();
    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let options = OperationOptions {
        backup: Some(BackupOptions::default()),
        ..options()
    };
    let plan = plan_copy(&selection(&root, &["a.txt"]), Side::Left, bases, &options);
    (left, right, plan)
}

/// A backup is written under a temporary name and then renamed to its own
/// name. A failed rename removes the temporary, and the step replaces nothing.
#[test]
fn a_backup_whose_rename_fails_leaves_no_temporary_and_the_original_untouched() {
    let (left, right, plan) = a_copy_with_a_backup();
    let fs = FaultFs::new(vec![Fault {
        call: "rename",
        path_contains: "a.txt.bak".to_string(),
        kind: io::ErrorKind::PermissionDenied,
    }]);
    let cancel = Cancel::new();
    let report = execute(
        &plan,
        &ExecutionContext::new(&fs, &cancel, Journaling::Disabled),
    );
    assert!(
        matches!(report.results[0].outcome, StepOutcome::Failed { .. }),
        "{:?}",
        report.results
    );
    assert_eq!(
        snapshot(right.path()),
        BTreeMap::from([(PathBuf::from("a.txt"), b"old".to_vec())])
    );
    assert_eq!(std::fs::read(left.path().join("a.txt")).unwrap(), b"new");
}

/// Another writer takes the backup name after the check for a free name. The
/// backup does not replace that file, and the step replaces nothing.
#[test]
fn a_backup_never_replaces_a_file_that_took_its_name_after_the_check() {
    let (left, right, plan) = a_copy_with_a_backup();
    let fs = FaultFs::arriving(&right.path().join("a.txt.bak"), b"another writer");
    let cancel = Cancel::new();
    let report = execute(
        &plan,
        &ExecutionContext::new(&fs, &cancel, Journaling::Disabled),
    );
    assert!(
        matches!(report.results[0].outcome, StepOutcome::Failed { .. }),
        "{:?}",
        report.results
    );
    assert_eq!(
        snapshot(right.path()),
        BTreeMap::from([
            (PathBuf::from("a.txt"), b"old".to_vec()),
            (PathBuf::from("a.txt.bak"), b"another writer".to_vec()),
        ])
    );
    assert_eq!(std::fs::read(left.path().join("a.txt")).unwrap(), b"new");
}

#[test]
fn a_target_that_changed_since_planning_is_not_overwritten() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"source").unwrap();
    let target = right.path().join("a.txt");
    std::fs::write(&target, b"old").unwrap();

    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let plan = plan_copy(&selection(&root, &["a.txt"]), Side::Left, bases, &options());

    // Someone else writes the target between the plan and the batch.
    std::fs::write(&target, b"work nobody has seen yet").unwrap();

    let report = run(&plan);
    assert!(
        matches!(report.results[0].outcome, StepOutcome::Skipped { .. }),
        "{:?}",
        report.results[0].outcome
    );
    assert_eq!(std::fs::read(&target).unwrap(), b"work nobody has seen yet");
}

fn a_left_file_to_carry(moving: bool) -> (tempfile::TempDir, tempfile::TempDir, OperationPlan) {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"source").unwrap();
    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let selected = selection(&root, &["a.txt"]);
    let plan = if moving {
        plan_move(&selected, Side::Left, bases, &options())
    } else {
        plan_copy(&selected, Side::Left, bases, &options())
    };
    (left, right, plan)
}

/// A target name that was free when the plan was built and is taken when the
/// step runs is drift, and a Proceed to that question does not make the copy
/// or the move a replacement.
#[test]
fn a_copy_or_move_never_replaces_an_item_that_took_a_free_target_name_after_planning() {
    for moving in [false, true] {
        let (left, right, plan) = a_left_file_to_carry(moving);
        assert!(plan.conflicts().is_empty());
        let target = right.path().join("a.txt");
        std::fs::write(&target, b"late").unwrap();

        let report = run_with(&plan, &ProceedOnDrift);
        match &report.results[0].outcome {
            StepOutcome::Skipped { reason } => {
                assert!(reason.contains("does not replace"), "{reason}");
            }
            other => panic!("moving {moving}: the step ran: {other:?}"),
        }
        assert_eq!(std::fs::read(&target).unwrap(), b"late");
        assert_eq!(std::fs::read(left.path().join("a.txt")).unwrap(), b"source");
    }
}

/// The target appears after the executor's check and before the commit. On
/// Windows the commit refuses a taken name in the call that renames.
#[cfg(windows)]
#[test]
fn a_copy_or_move_onto_a_target_that_appears_before_the_commit_leaves_it_alone() {
    for moving in [false, true] {
        let (left, right, plan) = a_left_file_to_carry(moving);
        let fs = FaultFs::arriving(&right.path().join("a.txt"), b"arrived");
        let cancel = Cancel::new();
        let report = execute(
            &plan,
            &ExecutionContext::new(&fs, &cancel, Journaling::Disabled),
        );

        let StepOutcome::Failed { message } = &report.results[0].outcome else {
            panic!("moving {moving}: {:?}", report.results[0].outcome);
        };
        assert!(message.contains("after the check"), "{message}");
        assert_eq!(
            snapshot(right.path()),
            BTreeMap::from([(PathBuf::from("a.txt"), b"arrived".to_vec())]),
            "moving {moving}"
        );
        assert_eq!(std::fs::read(left.path().join("a.txt")).unwrap(), b"source");
    }
}

#[test]
fn a_folder_that_gained_content_is_never_removed_wholesale() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::create_dir(left.path().join("sub")).unwrap();
    std::fs::write(left.path().join("sub/known.txt"), b"x").unwrap();

    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let mut opts = options();
    opts.use_recycle_bin = true;
    let plan = plan_delete(&selection(&root, &["sub"]), Sides::Left, bases, &opts);
    assert_eq!(plan.steps.len(), 1);

    std::fs::write(left.path().join("sub/arrived.txt"), b"keep").unwrap();

    // The trash is refused by the shim and the policy approves the fallback,
    // so the step reaches the recursive removal this test is about.
    let fs = FaultFs::new(Vec::new());
    let cancel = Cancel::new();
    let policy = ApproveOutrightRemoval;
    let report = execute(
        &plan,
        &ExecutionContext::new(&fs, &cancel, Journaling::Disabled).with_policy(&policy),
    );
    assert!(
        matches!(report.results[0].outcome, StepOutcome::Skipped { .. }),
        "{:?}",
        report.results[0].outcome
    );
    assert_eq!(
        std::fs::read(left.path().join("sub/arrived.txt")).unwrap(),
        b"keep"
    );
}

#[cfg(windows)]
#[test]
fn a_copy_keeps_the_creation_time_when_asked() {
    for preserve_created in [true, false] {
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        let source = left.path().join("a.txt");
        std::fs::write(&source, b"x").unwrap();
        // Far enough back that a copy made now cannot match it by accident.
        let created = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        RealFs.set_created(&source, created).unwrap();

        let root = compared(left.path(), right.path());
        let bases = Bases {
            left: left.path(),
            right: right.path(),
        };
        let opts = OperationOptions {
            preserve_created,
            ..options()
        };
        let plan = plan_copy(&selection(&root, &["a.txt"]), Side::Left, bases, &opts);
        assert!(run(&plan).is_clean());

        let landed = RealFs.probe(&right.path().join("a.txt")).unwrap();
        let landed = landed.created.unwrap();
        if preserve_created {
            assert_eq!(landed, created);
        } else {
            assert_ne!(landed, created);
        }
    }
}

/// A journal whose writes fail once the batch reaches its first step, which is
/// what a full disk looks like to execution.
#[derive(Debug)]
struct FailingJournal {
    path: PathBuf,
    fail_from: usize,
    writes: AtomicUsize,
}

impl JournalWriter for FailingJournal {
    fn path(&self) -> &Path {
        &self.path
    }

    fn append(&self, _record: &JournalRecord) -> io::Result<()> {
        let seen = self.writes.fetch_add(1, Ordering::SeqCst);
        if seen >= self.fail_from {
            return Err(io::Error::new(io::ErrorKind::StorageFull, "disk full"));
        }
        Ok(())
    }
}

#[test]
fn a_step_whose_journal_record_cannot_be_written_is_not_attempted() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"keep").unwrap();

    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let plan = plan_delete(
        &selection(&root, &["a.txt"]),
        Sides::Left,
        bases,
        &options(),
    );
    assert!(plan.steps[0].action.is_destructive());

    // The batch start record is written; the record that must precede the
    // destruction is not.
    let journal = FailingJournal {
        path: left.path().join("batch.jsonl"),
        fail_from: 1,
        writes: AtomicUsize::new(0),
    };
    let cancel = Cancel::new();
    let report = execute(
        &plan,
        &ExecutionContext::new(&RealFs, &cancel, Journaling::To(&journal)),
    );

    let StepOutcome::Failed { message } = &report.results[0].outcome else {
        panic!("{:?}", report.results[0].outcome);
    };
    assert!(message.contains("journal write failed"), "{message}");
    assert_eq!(std::fs::read(left.path().join("a.txt")).unwrap(), b"keep");
}

/// A policy that answers the recycle bin question the way a user who confirmed
/// the outright removal would.
struct ApproveOutrightRemoval;

impl ErrorPolicy for ApproveOutrightRemoval {
    fn on_error(&self, _step: &PlanStep, _attempt: u32, _message: &str) -> Decision {
        Decision::Skip
    }
    fn on_recycle_bin_unavailable(&self, _step: &PlanStep) -> ConflictDecision {
        ConflictDecision::Proceed
    }
}

#[test]
fn a_deletion_the_recycle_bin_refuses_is_left_in_place_unless_the_policy_approves() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"keep").unwrap();

    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let mut opts = options();
    opts.use_recycle_bin = true;
    let plan = plan_delete(&selection(&root, &["a.txt"]), Sides::Left, bases, &opts);
    assert!(matches!(plan.steps[0].action, StepAction::Trash { .. }));

    let fs = FaultFs::new(Vec::new());
    let cancel = Cancel::new();
    let report = execute(
        &plan,
        &ExecutionContext::new(&fs, &cancel, Journaling::Disabled),
    );
    assert!(
        matches!(report.results[0].outcome, StepOutcome::Skipped { .. }),
        "{:?}",
        report.results[0].outcome
    );
    assert_eq!(std::fs::read(left.path().join("a.txt")).unwrap(), b"keep");
}

#[test]
fn a_deletion_the_recycle_bin_refuses_is_removed_outright_once_the_policy_approves() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"gone").unwrap();

    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let mut opts = options();
    opts.use_recycle_bin = true;
    let plan = plan_delete(&selection(&root, &["a.txt"]), Sides::Left, bases, &opts);

    let fs = FaultFs::new(Vec::new());
    let cancel = Cancel::new();
    let policy = ApproveOutrightRemoval;
    let report = execute(
        &plan,
        &ExecutionContext::new(&fs, &cancel, Journaling::Disabled).with_policy(&policy),
    );
    assert!(report.is_clean(), "{report:?}");
    assert!(!left.path().join("a.txt").exists());
}

/// A policy that sees the item replaced while its permanent-removal question
/// is open, then approves the removal.
struct ReplacedWhileAsking(PathBuf);

impl ErrorPolicy for ReplacedWhileAsking {
    fn on_error(&self, _step: &PlanStep, _attempt: u32, _message: &str) -> Decision {
        Decision::Skip
    }
    fn on_recycle_bin_unavailable(&self, _step: &PlanStep) -> ConflictDecision {
        std::fs::write(&self.0, b"written while the question was open").unwrap();
        ConflictDecision::Proceed
    }
}

#[test]
fn permanent_removal_consent_does_not_cover_content_that_changed_while_asking() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    let path = left.path().join("a.txt");
    std::fs::write(&path, b"planned").unwrap();

    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let mut opts = options();
    opts.use_recycle_bin = true;
    let plan = plan_delete(&selection(&root, &["a.txt"]), Sides::Left, bases, &opts);

    let fs = FaultFs::new(Vec::new());
    let cancel = Cancel::new();
    let policy = ReplacedWhileAsking(path.clone());
    let report = execute(
        &plan,
        &ExecutionContext::new(&fs, &cancel, Journaling::Disabled).with_policy(&policy),
    );
    assert!(
        matches!(report.results[0].outcome, StepOutcome::Skipped { .. }),
        "{:?}",
        report.results[0].outcome
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"written while the question was open"
    );
}

/// A policy whose user stops at the permanent-removal question after an
/// earlier standing answer skipped every failure.
struct StopAtUnavailableRecycling(AtomicUsize);

impl ErrorPolicy for StopAtUnavailableRecycling {
    fn on_error(&self, _step: &PlanStep, _attempt: u32, _message: &str) -> Decision {
        Decision::Skip
    }
    fn on_recycle_bin_unavailable(&self, _step: &PlanStep) -> ConflictDecision {
        self.0.fetch_add(1, Ordering::SeqCst);
        ConflictDecision::Abort
    }
}

#[test]
fn stopping_at_unavailable_recycling_stops_the_batch() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"first").unwrap();
    std::fs::write(left.path().join("b.txt"), b"second").unwrap();

    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let mut opts = options();
    opts.use_recycle_bin = true;
    let plan = plan_delete(
        &selection(&root, &["a.txt", "b.txt"]),
        Sides::Left,
        bases,
        &opts,
    );
    assert_eq!(plan.steps.len(), 2);

    let fs = FaultFs::new(Vec::new());
    let cancel = Cancel::new();
    let policy = StopAtUnavailableRecycling(AtomicUsize::new(0));
    let report = execute(
        &plan,
        &ExecutionContext::new(&fs, &cancel, Journaling::Disabled).with_policy(&policy),
    );
    assert!(report.aborted, "{report:?}");
    assert_eq!(policy.0.load(Ordering::SeqCst), 1);
    assert_eq!(report.results.len(), 1);
    assert!(left.path().join("a.txt").exists());
    assert!(left.path().join("b.txt").exists());
}

#[test]
fn a_journal_never_replaces_one_that_is_already_there() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("batch.jsonl");
    let first = Journal::create(&path).unwrap();
    first
        .append(&JournalRecord::BatchStart {
            kind: crate::ops::plan::OperationKind::Delete,
            steps: 1,
            unix_seconds: 0,
        })
        .unwrap();
    drop(first);

    assert!(Journal::create(&path).is_err());
    assert!(!std::fs::read_to_string(&path).unwrap().is_empty());
}

#[test]
fn every_batch_gets_a_journal_of_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("journals");
    let one = Journal::create_in(&home).unwrap();
    let two = Journal::create_in(&home).unwrap();
    assert_ne!(one.path(), two.path());
    assert!(one.path().starts_with(&home));
}

#[test]
fn recovery_reports_every_unfinished_batch_in_a_directory() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.bin"), vec![1u8; 200_000]).unwrap();

    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let plan = plan_copy(&selection(&root, &["a.bin"]), Side::Left, bases, &options());

    let journal = Journal::create_in(home.path()).unwrap();
    let cancel = Cancel::new();
    let fs = CancellingFs {
        inner: RealFs,
        cancel: cancel.clone(),
    };
    let report = execute(
        &plan,
        &ExecutionContext::new(&fs, &cancel, Journaling::To(&journal)),
    );
    drop(journal);
    assert!(report.cancelled);

    let open = crate::journal::recover_all(home.path()).unwrap();
    assert_eq!(open.len(), 1);
    let (_, recovery) = &open[0];
    assert!(recovery.was_interrupted());
    assert!(!recovery.leftover_temporaries.is_empty());
    assert!(recovery.clean_up(&RealFs).is_empty());
}

#[test]
fn sync_step_indices_stay_dense_after_filtering() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    for name in ["a.txt", "b.txt", "c.txt"] {
        std::fs::write(left.path().join(name), b"x").unwrap();
    }
    let plan = mirror_right(left.path(), right.path(), &options());
    let indices: Vec<usize> = plan.steps.iter().map(|step| step.index).collect();
    assert_eq!(indices, (0..plan.steps.len()).collect::<Vec<usize>>());
}

#[test]
fn a_rename_onto_another_selected_name_is_refused() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"a").unwrap();
    std::fs::write(left.path().join("b.txt"), b"b").unwrap();
    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    // One selected name becomes another selected name, so carrying the renames
    // out in plan order would write one file over the other.
    let result = plan_rename(
        &root,
        &selection(&root, &["a.txt", "b.txt"]),
        Sides::Left,
        bases,
        &RenameAction::Regex {
            find: "^a".to_string(),
            replace: "b".to_string(),
        },
        &options(),
    );
    assert!(result.is_err());
    assert_eq!(std::fs::read(left.path().join("b.txt")).unwrap(), b"b");
}

#[test]
fn flattening_two_files_onto_one_name_carries_out_neither() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(left.path().join("one")).unwrap();
    std::fs::create_dir_all(left.path().join("two")).unwrap();
    std::fs::write(left.path().join("one/same.txt"), b"1").unwrap();
    std::fs::write(left.path().join("two/same.txt"), b"2").unwrap();

    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let plan = plan_to_folder(
        &selection(&root, &["one/same.txt", "two/same.txt"]),
        Side::Left,
        bases,
        target.path(),
        PathOption::Flatten,
        &options(),
        true,
        &RealFs,
    );
    assert!(plan.steps.is_empty());
    assert_eq!(plan.refusals().len(), 2);
    run(&plan);
    assert_eq!(
        std::fs::read(left.path().join("one/same.txt")).unwrap(),
        b"1"
    );
    assert_eq!(
        std::fs::read(left.path().join("two/same.txt")).unwrap(),
        b"2"
    );
}

#[test]
fn a_new_folder_never_replaces_a_file_of_the_same_name() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    let occupied = left.path().join("fresh");
    std::fs::write(&occupied, b"a file, not a folder").unwrap();

    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let plan = plan_new_folder(
        &compared(left.path(), right.path()),
        Path::new(""),
        "fresh",
        Sides::Left,
        bases,
        &options(),
    );
    let report = run(&plan);
    assert!(!report.is_clean());
    assert_eq!(std::fs::read(&occupied).unwrap(), b"a file, not a folder");
}

/// A Windows path drops a dot or a space at the end of a name, so a folder
/// named `a.` is created as `a`, or found as `a` when `a` exists. A DOS device
/// name reaches the device.
#[cfg(windows)]
#[test]
fn a_new_folder_name_a_windows_path_does_not_reach_is_refused_while_planning() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::create_dir(right.path().join("a")).unwrap();
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    for (name, why) in [
        ("a.", "ends with a dot or a space"),
        ("a ", "ends with a dot or a space"),
        ("nul", "is a device name on Windows"),
        ("con.txt", "is a device name on Windows"),
    ] {
        let plan = plan_new_folder(
            &compared(left.path(), right.path()),
            Path::new(""),
            name,
            Sides::Both,
            bases,
            &options(),
        );
        assert!(plan.steps.is_empty(), "{name:?}: {:?}", plan.steps);
        let [skip] = plan.skipped.as_slice() else {
            panic!("{name:?}: {:?}", plan.skipped);
        };
        assert_eq!(skip.path, Path::new(name));
        assert!(
            skip.reason.contains(&format!("{name:?}")),
            "{}",
            skip.reason
        );
        assert!(skip.reason.contains(why), "{}", skip.reason);
        assert!(run(&plan).results.is_empty());
    }
    assert_eq!(std::fs::read_dir(left.path()).unwrap().count(), 0);
    assert_eq!(std::fs::read_dir(right.path()).unwrap().count(), 1);
}

#[test]
fn touch_and_attributes_never_reach_through_a_link() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("inside.txt"), b"x").unwrap();
    if !make_junction(&left.path().join("link"), outside.path()) {
        return;
    }
    let before = RealFs.probe(outside.path()).unwrap().modified;

    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let touch = plan_touch(
        &selection(&root, &["link"]),
        Sides::Left,
        bases,
        TouchSpec::Explicit(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000)),
        &options(),
    );
    assert!(touch.steps.is_empty());
    assert!(!touch.refusals().is_empty());

    let attributes = plan_attributes(
        &selection(&root, &["link"]),
        Sides::Left,
        bases,
        AttributeChange {
            read_only: Some(true),
            ..AttributeChange::default()
        },
        &options(),
    );
    assert!(attributes.steps.is_empty());
    assert_eq!(RealFs.probe(outside.path()).unwrap().modified, before);
    assert!(!RealFs.probe(outside.path()).unwrap().read_only);
}

// ------------------------------------ copy and move onto the target folder

/// The left folder holds `top.txt` and `sub/inner.txt`; the right is empty.
fn a_left_tree() -> (tempfile::TempDir, tempfile::TempDir) {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::create_dir(left.path().join("sub")).unwrap();
    std::fs::write(left.path().join("top.txt"), b"TOP-CONTENT").unwrap();
    std::fs::write(left.path().join("sub/inner.txt"), b"INNER-CONTENT").unwrap();
    (left, right)
}

/// Move to Folder with the target folder set to the source's own base folder
/// gives the item a destination equal to its source. With verification on,
/// the copy lands on the file itself and the removal of the source then
/// removes the only copy.
#[test]
fn a_move_or_copy_to_a_folder_onto_the_item_itself_is_refused_while_planning() {
    for (moving, verify, path_option, selected) in [
        (true, Verify::Hash, PathOption::KeepBase, "top.txt"),
        (true, Verify::Size, PathOption::KeepBase, "sub/inner.txt"),
        (true, Verify::Hash, PathOption::Flatten, "top.txt"),
        (true, Verify::None, PathOption::KeepBase, "top.txt"),
        (true, Verify::Hash, PathOption::KeepBase, "sub"),
        (false, Verify::Hash, PathOption::KeepBase, "top.txt"),
    ] {
        let label = format!("moving {moving}, {verify:?}, {path_option:?}, {selected}");
        let (left, right) = a_left_tree();
        let before = snapshot(left.path());
        let root = compared(left.path(), right.path());
        let bases = Bases {
            left: left.path(),
            right: right.path(),
        };
        let opts = OperationOptions {
            verify,
            ..options()
        };
        let plan = plan_to_folder(
            &selection(&root, &[selected]),
            Side::Left,
            bases,
            left.path(),
            path_option,
            &opts,
            moving,
            &RealFs,
        );
        assert!(plan.steps.is_empty(), "{label}: {:?}", plan.steps);
        let skip = plan
            .skipped
            .iter()
            .find(|skip| skip.path == Path::new(selected))
            .unwrap_or_else(|| panic!("{label}: {:?}", plan.skipped));
        assert!(skip.reason.contains("itself"), "{label}: {}", skip.reason);
        run(&plan);
        assert_eq!(snapshot(left.path()), before, "{label}");
    }
}

/// Both sides of the comparison are one folder, so a move to the other side
/// has every file as its own target. The executor refuses the step at its
/// check, whatever the plan says. On Windows the right base folder is spelled
/// in another case, which names the same folder.
#[test]
fn the_executor_refuses_a_move_whose_source_and_target_are_one_item() {
    for verify in [Verify::Hash, Verify::Size, Verify::None] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"ONLY-COPY").unwrap();
        let other = if cfg!(windows) {
            PathBuf::from(dir.path().to_string_lossy().to_uppercase())
        } else {
            dir.path().to_path_buf()
        };
        let root = compared(dir.path(), &other);
        let bases = Bases {
            left: dir.path(),
            right: &other,
        };
        let opts = OperationOptions {
            verify,
            ..options()
        };
        let plan = plan_move(&selection(&root, &["a.txt"]), Side::Left, bases, &opts);
        assert_eq!(plan.steps.len(), 1, "{verify:?}: {:?}", plan.steps);
        let report = run(&plan);
        match &report.results[0].outcome {
            StepOutcome::Skipped { reason } => {
                assert!(reason.contains("itself"), "{verify:?}: {reason}");
            }
            other => panic!("{verify:?}: the step ran: {other:?}"),
        }
        assert_eq!(
            std::fs::read(dir.path().join("a.txt")).unwrap(),
            b"ONLY-COPY",
            "{verify:?}"
        );
    }
}

/// A second route to the folder at `path` whose text differs from the path:
/// the loopback share of its drive on Windows, a link in `spare` elsewhere.
/// `None` when this machine does not reach the folder through the route; an
/// administrative share needs the Server service and an account that may
/// read it.
fn second_route(path: &Path, spare: &Path) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let _ = spare;
        let text = path.to_str()?;
        let (drive, rest) = text.split_once(":\\")?;
        let loopback = PathBuf::from(format!(r"\\localhost\{drive}$\{rest}"));
        loopback.is_dir().then_some(loopback)
    }
    #[cfg(not(windows))]
    {
        let alias = spare.join("alias");
        std::os::unix::fs::symlink(path, &alias).ok()?;
        Some(alias)
    }
}

/// Every second route to the folder at `path`: a [`Route`] that states the
/// identity of each item, so the test asserts on every machine, and the
/// route [`second_route`] finds, read through [`RealFs`], where this machine
/// has one.
fn second_routes(path: &Path, spare: &Path) -> Vec<(PathBuf, Box<dyn FileOps>)> {
    let alias = spare.join("route");
    let double = Route {
        alias: alias.clone(),
        real: path.to_path_buf(),
        states_identity: true,
    };
    let mut routes: Vec<(PathBuf, Box<dyn FileOps>)> = vec![(alias, Box::new(double))];
    if let Some(route) = second_route(path, spare) {
        routes.push((route, Box::new(RealFs)));
    }
    routes
}

/// Move to Folder and Copy to Folder onto the folder the item lies in, named
/// through a second route, have the item itself as the destination. A share
/// of a local folder is not a link, so the resolved text of the two paths
/// differs; the planner gives no step all the same.
#[test]
fn a_move_to_a_folder_named_through_a_second_route_to_its_own_folder_is_refused() {
    for moving in [true, false] {
        for index in 0.. {
            let (left, right) = a_left_tree();
            let spare = tempfile::tempdir().unwrap();
            let mut routes = second_routes(left.path(), spare.path());
            if index >= routes.len() {
                break;
            }
            let (route, fs) = routes.swap_remove(index);
            let label = format!("moving {moving}, {}", route.display());
            let before = snapshot(left.path());
            let root = compared(left.path(), right.path());
            let bases = Bases {
                left: left.path(),
                right: right.path(),
            };
            let plan = plan_to_folder(
                &selection(&root, &["top.txt"]),
                Side::Left,
                bases,
                &route,
                PathOption::KeepBase,
                &options(),
                moving,
                fs.as_ref(),
            );
            assert!(plan.steps.is_empty(), "{label}: {:?}", plan.steps);
            let skip = plan
                .skipped
                .iter()
                .find(|skip| skip.path == Path::new("top.txt"))
                .unwrap_or_else(|| panic!("{label}: {:?}", plan.skipped));
            assert!(skip.reason.contains("itself"), "{label}: {}", skip.reason);
            let cancel = Cancel::new();
            let report = execute(
                &plan,
                &ExecutionContext::new(fs.as_ref(), &cancel, Journaling::Disabled),
            );
            assert!(report.results.is_empty(), "{label}");
            assert_eq!(snapshot(left.path()), before, "{label}");
        }
    }
}

/// Planning a copy to a folder reads the state of each destination once, the
/// destination of a selected item included.
#[test]
fn planning_a_copy_to_a_folder_reads_each_destination_once() {
    let (left, right) = a_left_tree();
    let out = tempfile::tempdir().unwrap();
    std::fs::write(out.path().join("top.txt"), b"TAKEN").unwrap();
    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let fs = FaultFs::new(Vec::new());
    let plan = plan_to_folder(
        &selection(&root, &["top.txt", "sub"]),
        Side::Left,
        bases,
        out.path(),
        PathOption::KeepBase,
        &options(),
        false,
        &fs,
    );
    assert_eq!(plan.steps.len(), 3, "{:?}", plan.steps);
    let calls = fs.calls.lock().unwrap().clone();
    for destination in ["top.txt", "sub", "sub/inner.txt"] {
        let probed = format!(
            "probe:{}",
            out.path()
                .join(destination)
                .to_string_lossy()
                .replace('\\', "/")
        );
        let count = calls.iter().filter(|call| **call == probed).count();
        assert_eq!(count, 1, "{destination}: {calls:?}");
    }
}

/// Both sides of the comparison are one folder, the right one named through a
/// second route, so the move of a file to the other side has the file itself
/// as its target. The executor refuses the step at its check.
#[test]
fn the_executor_refuses_a_move_onto_the_item_named_through_a_second_route() {
    for verify in [Verify::None, Verify::Hash] {
        for index in 0.. {
            let dir = tempfile::tempdir().unwrap();
            let spare = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("a.txt"), b"ONLY-COPY").unwrap();
            let mut routes = second_routes(dir.path(), spare.path());
            if index >= routes.len() {
                break;
            }
            let (route, fs) = routes.swap_remove(index);
            let label = format!("{verify:?}, {}", route.display());
            let root = compared(dir.path(), dir.path());
            let bases = Bases {
                left: dir.path(),
                right: &route,
            };
            let opts = OperationOptions {
                verify,
                ..options()
            };
            let plan = plan_move(&selection(&root, &["a.txt"]), Side::Left, bases, &opts);
            assert_eq!(plan.steps.len(), 1, "{label}: {:?}", plan.steps);
            let cancel = Cancel::new();
            let report = execute(
                &plan,
                &ExecutionContext::new(fs.as_ref(), &cancel, Journaling::Disabled),
            );
            match &report.results[0].outcome {
                StepOutcome::Skipped { reason } => {
                    assert!(reason.contains("itself"), "{label}: {reason}");
                }
                other => panic!("{label}: the step ran: {other:?}"),
            }
            assert_eq!(
                std::fs::read(dir.path().join("a.txt")).unwrap(),
                b"ONLY-COPY",
                "{label}"
            );
        }
    }
}

// ------------------------------------ routes that state no identity or one

/// [`RealFs`] with a second spelling of one folder, as a share of a local
/// folder has: every path under `alias` reaches the same place under `real`,
/// and its locations keep the two spellings apart. It states the identity of
/// each item, as a volume with file numbers does through a share, or none,
/// as a file system without file numbers does.
struct Route {
    alias: PathBuf,
    real: PathBuf,
    states_identity: bool,
}

impl Route {
    fn resolve(&self, path: &Path) -> PathBuf {
        path.strip_prefix(&self.alias)
            .map_or_else(|_| path.to_path_buf(), |rest| self.real.join(rest))
    }
}

impl FileOps for Route {
    fn probe(&self, path: &Path) -> io::Result<TargetState> {
        RealFs.probe(&self.resolve(path))
    }
    fn create_dir(&self, path: &Path) -> io::Result<()> {
        RealFs.create_dir(&self.resolve(path))
    }
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn Read + Send>> {
        RealFs.open_read(&self.resolve(path))
    }
    fn create_new(&self, path: &Path) -> io::Result<Box<dyn SyncWrite>> {
        RealFs.create_new(&self.resolve(path))
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        RealFs.rename(&self.resolve(from), &self.resolve(to))
    }
    fn rename_no_replace(&self, from: &Path, to: &Path) -> io::Result<()> {
        RealFs.rename_no_replace(&self.resolve(from), &self.resolve(to))
    }
    fn remove_file(&self, path: &Path) -> io::Result<()> {
        RealFs.remove_file(&self.resolve(path))
    }
    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        RealFs.remove_dir(&self.resolve(path))
    }
    fn move_to_trash(&self, _path: &Path) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
    fn set_modified(&self, path: &Path, time: SystemTime) -> io::Result<()> {
        RealFs.set_modified(&self.resolve(path), time)
    }
    fn set_created(&self, path: &Path, time: SystemTime) -> io::Result<()> {
        RealFs.set_created(&self.resolve(path), time)
    }
    fn set_attributes(&self, path: &Path, change: &AttributeChange) -> io::Result<()> {
        RealFs.set_attributes(&self.resolve(path), change)
    }
    fn same_volume(&self, from: &Path, to: &Path) -> bool {
        from.starts_with(&self.alias) == to.starts_with(&self.alias)
    }
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        let listed = RealFs.read_dir(&self.resolve(path))?;
        if !path.starts_with(&self.alias) {
            return Ok(listed);
        }
        Ok(listed
            .iter()
            .filter_map(|child| child.file_name().map(|name| path.join(name)))
            .collect())
    }
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        if path.starts_with(&self.alias) {
            return Ok(path.to_path_buf());
        }
        std::fs::canonicalize(path)
    }
    fn identity(&self, path: &Path) -> io::Result<Option<crate::ops::fsops::ItemIdentity>> {
        if self.states_identity {
            return RealFs.identity(&self.resolve(path));
        }
        Ok(None)
    }
}

/// Both sides of the comparison are one folder, the right one reached
/// through a route whose file system states no identity and whose text no
/// comparison folds. Nothing read proves two items, so the executor refuses
/// the move, and the only copy stays.
#[test]
fn a_move_onto_the_item_through_a_route_with_no_identity_is_refused() {
    for verify in [Verify::None, Verify::Hash] {
        let dir = tempfile::tempdir().unwrap();
        let spare = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"ONLY-COPY").unwrap();
        let alias = spare.path().join("route");
        let fs = Route {
            alias: alias.clone(),
            real: dir.path().to_path_buf(),
            states_identity: false,
        };
        let root = compared(dir.path(), dir.path());
        let bases = Bases {
            left: dir.path(),
            right: &alias,
        };
        let opts = OperationOptions {
            verify,
            ..options()
        };
        let plan = plan_move(&selection(&root, &["a.txt"]), Side::Left, bases, &opts);
        assert_eq!(plan.steps.len(), 1, "{verify:?}: {:?}", plan.steps);
        let cancel = Cancel::new();
        let report = execute(
            &plan,
            &ExecutionContext::new(&fs, &cancel, Journaling::Disabled),
        );
        match &report.results[0].outcome {
            StepOutcome::Skipped { reason } => {
                assert!(reason.contains("itself"), "{verify:?}: {reason}");
            }
            other => panic!("{verify:?}: the step ran: {other:?}"),
        }
        assert_eq!(
            std::fs::read(dir.path().join("a.txt")).unwrap(),
            b"ONLY-COPY",
            "{verify:?}"
        );
    }
}

/// Move to Folder and Copy to Folder into the folder the item lies in,
/// named through a route that states no identity: nothing read proves the
/// destination is another item, so the planner gives no step.
#[test]
fn a_move_to_its_own_folder_through_a_route_with_no_identity_is_refused_while_planning() {
    for moving in [true, false] {
        let (left, right) = a_left_tree();
        let spare = tempfile::tempdir().unwrap();
        let alias = spare.path().join("route");
        let fs = Route {
            alias: alias.clone(),
            real: left.path().to_path_buf(),
            states_identity: false,
        };
        let before = snapshot(left.path());
        let root = compared(left.path(), right.path());
        let bases = Bases {
            left: left.path(),
            right: right.path(),
        };
        let plan = plan_to_folder(
            &selection(&root, &["top.txt"]),
            Side::Left,
            bases,
            &alias,
            PathOption::KeepBase,
            &options(),
            moving,
            &fs,
        );
        assert!(plan.steps.is_empty(), "moving {moving}: {:?}", plan.steps);
        let skip = plan
            .skipped
            .iter()
            .find(|skip| skip.path == Path::new("top.txt"))
            .unwrap_or_else(|| panic!("moving {moving}: {:?}", plan.skipped));
        assert!(
            skip.reason.contains("itself"),
            "moving {moving}: {}",
            skip.reason
        );
        let cancel = Cancel::new();
        let report = execute(
            &plan,
            &ExecutionContext::new(&fs, &cancel, Journaling::Disabled),
        );
        assert!(report.results.is_empty(), "moving {moving}");
        assert_eq!(snapshot(left.path()), before, "moving {moving}");
    }
}

/// Over a file system that states no identity, a destination whose size or
/// time differs from its source is another item, so a copy onto it runs.
#[test]
fn a_copy_through_a_route_with_no_identity_runs_onto_an_item_that_differs() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    let spare = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"NEW-CONTENT").unwrap();
    std::fs::write(right.path().join("a.txt"), b"OLD").unwrap();
    let alias = spare.path().join("route");
    let fs = Route {
        alias: alias.clone(),
        real: right.path().to_path_buf(),
        states_identity: false,
    };
    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: &alias,
    };
    let plan = plan_copy(&selection(&root, &["a.txt"]), Side::Left, bases, &options());
    let cancel = Cancel::new();
    let report = execute(
        &plan,
        &ExecutionContext::new(&fs, &cancel, Journaling::Disabled),
    );
    assert!(report.is_clean(), "{:?}", report.results);
    assert_eq!(
        std::fs::read(right.path().join("a.txt")).unwrap(),
        b"NEW-CONTENT"
    );
}

/// [`RealFs`] that states one identity for every item, as two volumes with
/// one serial number do for two files at one position of their file tables.
struct OneIdentityFs {
    same_volume: Option<bool>,
    distinct_volumes: Option<(PathBuf, PathBuf)>,
}

impl FileOps for OneIdentityFs {
    fn probe(&self, path: &Path) -> io::Result<TargetState> {
        RealFs.probe(path)
    }
    fn create_dir(&self, path: &Path) -> io::Result<()> {
        RealFs.create_dir(path)
    }
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn Read + Send>> {
        RealFs.open_read(path)
    }
    fn create_new(&self, path: &Path) -> io::Result<Box<dyn SyncWrite>> {
        RealFs.create_new(path)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        RealFs.rename(from, to)
    }
    fn rename_no_replace(&self, from: &Path, to: &Path) -> io::Result<()> {
        RealFs.rename_no_replace(from, to)
    }
    fn remove_file(&self, path: &Path) -> io::Result<()> {
        RealFs.remove_file(path)
    }
    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        RealFs.remove_dir(path)
    }
    fn move_to_trash(&self, _path: &Path) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
    fn set_modified(&self, path: &Path, time: SystemTime) -> io::Result<()> {
        RealFs.set_modified(path, time)
    }
    fn set_created(&self, path: &Path, time: SystemTime) -> io::Result<()> {
        RealFs.set_created(path, time)
    }
    fn set_attributes(&self, path: &Path, change: &AttributeChange) -> io::Result<()> {
        RealFs.set_attributes(path, change)
    }
    fn same_volume(&self, from: &Path, to: &Path) -> bool {
        self.same_volume
            .unwrap_or_else(|| RealFs.same_volume(from, to))
    }
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        RealFs.read_dir(path)
    }
    fn identity(&self, _path: &Path) -> io::Result<Option<crate::ops::fsops::ItemIdentity>> {
        Ok(Some(crate::ops::fsops::ItemIdentity {
            volume: 7,
            number: 42,
        }))
    }
    fn volume_identity(&self, path: &Path) -> io::Result<Option<Vec<u8>>> {
        Ok(self.distinct_volumes.as_ref().and_then(|(first, second)| {
            if path.starts_with(first) {
                Some(vec![1])
            } else if path.starts_with(second) {
                Some(vec![2])
            } else {
                None
            }
        }))
    }
}

/// Two files the file system gives one identity are two items when their
/// sizes or times differ: the copy between them runs, and Copy to Folder
/// onto the other one plans a replacement. Two that agree in kind, size and
/// time are refused as one item, which loses nothing.
#[test]
fn an_equal_identity_with_another_size_or_time_is_two_items() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("changed.txt"), b"NEW-CONTENT").unwrap();
    std::fs::write(right.path().join("changed.txt"), b"OLD").unwrap();
    let time = SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
    for side in [left.path(), right.path()] {
        std::fs::write(side.join("same.txt"), b"SAME").unwrap();
        RealFs.set_modified(&side.join("same.txt"), time).unwrap();
    }
    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };

    let folder = plan_to_folder(
        &selection(&root, &["changed.txt", "same.txt"]),
        Side::Left,
        bases,
        right.path(),
        PathOption::KeepBase,
        &options(),
        false,
        &OneIdentityFs {
            same_volume: None,
            distinct_volumes: None,
        },
    );
    assert_eq!(folder.steps.len(), 1, "{:?}", folder.steps);
    assert!(folder.steps[0].conflicts.contains(&Conflict::TargetExists));
    assert!(
        folder
            .skipped
            .iter()
            .any(|skip| skip.path == Path::new("same.txt") && skip.reason.contains("itself")),
        "{:?}",
        folder.skipped
    );

    let plan = plan_copy(
        &selection(&root, &["changed.txt", "same.txt"]),
        Side::Left,
        bases,
        &options(),
    );
    let cancel = Cancel::new();
    let report = execute(
        &plan,
        &ExecutionContext::new(
            &OneIdentityFs {
                same_volume: None,
                distinct_volumes: None,
            },
            &cancel,
            Journaling::Disabled,
        ),
    );
    let outcome = |name: &str| {
        plan.steps
            .iter()
            .zip(&report.results)
            .find(|(step, _)| step.rel == Path::new(name))
            .map(|(_, result)| result.outcome.clone())
            .unwrap()
    };
    assert_eq!(outcome("changed.txt"), StepOutcome::Done);
    assert!(
        matches!(outcome("same.txt"), StepOutcome::Skipped { ref reason } if reason.contains("itself")),
        "{:?}",
        outcome("same.txt")
    );
    assert_eq!(
        std::fs::read(right.path().join("changed.txt")).unwrap(),
        b"NEW-CONTENT"
    );
    assert_eq!(
        std::fs::read(right.path().join("same.txt")).unwrap(),
        b"SAME"
    );
}

/// Equal volume serial numbers and file indexes do not prove a shared item
/// when the paths do not resolve together or have known common volume.
#[test]
fn files_on_distinct_volumes_with_colliding_file_ids_are_two_items() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    let first = left.path().join("same.txt");
    let second = right.path().join("same.txt");
    let time = SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
    for path in [&first, &second] {
        std::fs::write(path, "same content").unwrap();
        RealFs.set_modified(path, time).unwrap();
    }
    let fs = OneIdentityFs {
        same_volume: Some(false),
        distinct_volumes: Some((left.path().to_path_buf(), right.path().to_path_buf())),
    };
    assert!(fs.distinct_volumes.is_some());
    assert_eq!(same_item(&fs, &first, &second), Reach::TwoItems);
    let root = compared(left.path(), right.path());
    let plan = plan_to_folder(
        &selection(&root, &["same.txt"]),
        Side::Left,
        Bases {
            left: left.path(),
            right: right.path(),
        },
        right.path(),
        PathOption::KeepBase,
        &options(),
        true,
        &fs,
    );
    assert_eq!(plan.steps.len(), 1);
    assert!(plan.skipped.is_empty());
}

#[test]
fn colliding_file_identities_on_different_volumes_remain_unproven() {
    let first_dir = tempfile::tempdir().unwrap();
    let second_dir = tempfile::tempdir().unwrap();
    let first = first_dir.path().join("same.txt");
    let second = second_dir.path().join("same.txt");
    let time = SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
    std::fs::write(&first, b"SAME").unwrap();
    std::fs::write(&second, b"SAME").unwrap();
    RealFs.set_modified(&first, time).unwrap();
    RealFs.set_modified(&second, time).unwrap();

    let fs = OneIdentityFs {
        same_volume: Some(false),
        distinct_volumes: None,
    };
    assert_eq!(same_item(&fs, &first, &second), Reach::Unproven);

    let root = compared(first_dir.path(), second_dir.path());
    let plan = plan_to_folder(
        &selection(&root, &["same.txt"]),
        Side::Left,
        Bases {
            left: first_dir.path(),
            right: second_dir.path(),
        },
        second_dir.path(),
        PathOption::KeepBase,
        &options(),
        true,
        &fs,
    );
    assert!(
        plan.steps.is_empty(),
        "a move onto an unproven item must stop"
    );
    assert_eq!(plan.skipped.len(), 1, "the reason is shown on the row");
    assert_eq!(
        plan.skipped[0].reason,
        crate::ops::plan::MAYBE_ITSELF,
        "the message does not claim that the file system has no identity"
    );
}

/// Copy to Folder and Move to Folder onto a folder that already holds the
/// name list the item as a conflict of the copy, with the state it was found
/// in, and a declined conflict leaves the item alone.
#[test]
fn a_copy_or_move_to_a_folder_lists_an_occupied_name_as_a_conflict() {
    for moving in [false, true] {
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        std::fs::write(left.path().join("a.txt"), b"SOURCE").unwrap();
        std::fs::write(out.path().join("a.txt"), b"PRECIOUS-TARGET").unwrap();
        let root = compared(left.path(), right.path());
        let bases = Bases {
            left: left.path(),
            right: right.path(),
        };
        let opts = OperationOptions {
            backup: Some(BackupOptions::default()),
            ..options()
        };
        let plan = plan_to_folder(
            &selection(&root, &["a.txt"]),
            Side::Left,
            bases,
            out.path(),
            PathOption::KeepBase,
            &opts,
            moving,
            &RealFs,
        );
        assert!(
            plan.conflicts().contains(&Conflict::TargetExists),
            "moving {moving}: {:?}",
            plan.steps
        );
        let step = &plan.steps[0];
        let target = step.expected.target.as_ref().expect("the target state");
        assert!(target.exists && !target.is_dir, "moving {moving}");
        assert_eq!(target.size, 15, "moving {moving}");
        assert_eq!(
            step.backup.as_deref(),
            Some(out.path().join("a.txt.bak").as_path()),
            "moving {moving}"
        );

        let report = run_with(&plan, &DeclineConflicts);
        assert!(
            matches!(report.results[0].outcome, StepOutcome::Skipped { .. }),
            "moving {moving}: {:?}",
            report.results
        );
        assert_eq!(
            snapshot(out.path()),
            BTreeMap::from([(PathBuf::from("a.txt"), b"PRECIOUS-TARGET".to_vec())]),
            "moving {moving}"
        );
        assert_eq!(std::fs::read(left.path().join("a.txt")).unwrap(), b"SOURCE");
    }
}

/// A name that was free when the plan was built and is taken when the step
/// runs is drift, and a Proceed to that question does not make the copy or the
/// move a replacement.
#[test]
fn a_copy_or_move_to_a_folder_never_replaces_an_item_that_appeared_after_planning() {
    for moving in [false, true] {
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        std::fs::write(left.path().join("a.txt"), b"SOURCE").unwrap();
        let root = compared(left.path(), right.path());
        let bases = Bases {
            left: left.path(),
            right: right.path(),
        };
        let plan = plan_to_folder(
            &selection(&root, &["a.txt"]),
            Side::Left,
            bases,
            out.path(),
            PathOption::KeepBase,
            &options(),
            moving,
            &RealFs,
        );
        assert!(plan.conflicts().is_empty(), "moving {moving}");
        std::fs::write(out.path().join("a.txt"), b"LATE").unwrap();

        let report = run_with(&plan, &ProceedOnDrift);
        match &report.results[0].outcome {
            StepOutcome::Skipped { reason } => {
                assert!(reason.contains("does not replace"), "{reason}");
            }
            other => panic!("moving {moving}: the step ran: {other:?}"),
        }
        assert_eq!(std::fs::read(out.path().join("a.txt")).unwrap(), b"LATE");
        assert_eq!(std::fs::read(left.path().join("a.txt")).unwrap(), b"SOURCE");
    }
}

// ------------------------------------------ one backup, and its settled name

/// The backup names every `StepBegin` record of a journal holds.
fn journalled_backups(path: &Path) -> Vec<Option<PathBuf>> {
    crate::journal::read_records(path)
        .unwrap()
        .into_iter()
        .filter_map(|record| match record {
            JournalRecord::StepBegin { backup, .. } => Some(backup),
            _ => None,
        })
        .collect()
}

/// A move onto an existing file whose replacing rename fails for a reason
/// other than a taken name falls back to a copy. The replaced file is saved
/// once, the journal names the backup the step settled on, and the batch is
/// not left troubled by the fallback.
#[test]
fn a_move_whose_replacing_rename_fails_once_takes_one_backup_and_journals_it() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"NEW").unwrap();
    std::fs::write(right.path().join("a.txt"), b"OLD").unwrap();
    std::fs::write(right.path().join("a.txt.bak"), b"OLDER").unwrap();
    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let opts = OperationOptions {
        backup: Some(BackupOptions::default()),
        ..options()
    };
    let plan = plan_move(&selection(&root, &["a.txt"]), Side::Left, bases, &opts);
    assert!(plan.steps[0].conflicts.contains(&Conflict::TargetExists));

    // Rename 0 commits the backup; rename 1 is the move's own rename.
    let fs = FaultFs::failing_rename(1);
    let journals = tempfile::tempdir().unwrap();
    let journal = Journal::create_in(journals.path()).unwrap();
    let cancel = Cancel::new();
    let report = execute(
        &plan,
        &ExecutionContext::new(&fs, &cancel, Journaling::To(&journal)),
    );
    let journal_path = journal.path().to_path_buf();
    drop(journal);

    assert!(report.is_clean(), "{:?}", report.results);
    let settled = right.path().join("a.txt.bak1");
    assert_eq!(report.results[0].backup.as_deref(), Some(settled.as_path()));
    assert_eq!(
        snapshot(right.path()),
        BTreeMap::from([
            (PathBuf::from("a.txt"), b"NEW".to_vec()),
            (PathBuf::from("a.txt.bak"), b"OLDER".to_vec()),
            (PathBuf::from("a.txt.bak1"), b"OLD".to_vec()),
        ])
    );
    assert!(!left.path().join("a.txt").exists());
    assert_eq!(journalled_backups(&journal_path), vec![Some(settled)]);
    let recovery = recover(&journal_path).unwrap();
    assert!(!recovery.troubled, "{:?}", recovery.records);
    assert!(recovery.ended_cleanly());
    assert!(crate::journal::retire(&journal_path).unwrap());
}

/// A copy over a target whose first backup name is taken records the name
/// the step settled on, not the name the plan proposed.
#[test]
fn a_copy_journals_the_backup_name_it_settled_on() {
    let (left, right, plan) = a_copy_with_a_backup();
    std::fs::write(right.path().join("a.txt.bak"), b"older backup").unwrap();
    let journals = tempfile::tempdir().unwrap();
    let journal = Journal::create_in(journals.path()).unwrap();
    let cancel = Cancel::new();
    let report = execute(
        &plan,
        &ExecutionContext::new(&RealFs, &cancel, Journaling::To(&journal)),
    );
    let journal_path = journal.path().to_path_buf();
    drop(journal);
    assert!(report.is_clean(), "{:?}", report.results);
    let settled = right.path().join("a.txt.bak1");
    assert_eq!(report.results[0].backup.as_deref(), Some(settled.as_path()));
    assert_eq!(std::fs::read(&settled).unwrap(), b"old");
    assert_eq!(journalled_backups(&journal_path), vec![Some(settled)]);
    assert_eq!(std::fs::read(left.path().join("a.txt")).unwrap(), b"new");
}

/// What one path holds in [`CaseKeepingFs`].
#[derive(Clone)]
enum MemNode {
    File(Vec<u8>),
    Folder,
}

type MemNodes = std::sync::Arc<Mutex<BTreeMap<PathBuf, MemNode>>>;

/// An in-memory volume that keeps case, so `b.txt` and `B.txt` are two
/// items. A path is taken as written, and no link stands anywhere.
struct CaseKeepingFs {
    nodes: MemNodes,
    keeps_case: bool,
}

impl Default for CaseKeepingFs {
    fn default() -> Self {
        Self {
            nodes: MemNodes::default(),
            keeps_case: true,
        }
    }
}

impl CaseKeepingFs {
    fn with_unknown_case_behavior() -> Self {
        Self {
            keeps_case: false,
            ..Self::default()
        }
    }

    fn file(&self, path: &Path, bytes: &[u8]) {
        self.nodes
            .lock()
            .unwrap()
            .insert(path.to_path_buf(), MemNode::File(bytes.to_vec()));
    }

    fn folder(&self, path: &Path) {
        self.nodes
            .lock()
            .unwrap()
            .insert(path.to_path_buf(), MemNode::Folder);
    }

    fn contents(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        self.nodes
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(path, node)| match node {
                MemNode::File(bytes) => Some((path.clone(), bytes.clone())),
                MemNode::Folder => None,
            })
            .collect()
    }
}

/// A file of [`CaseKeepingFs`] that lands when it is flushed.
struct MemWriter {
    nodes: MemNodes,
    path: PathBuf,
    bytes: Vec<u8>,
}

impl std::io::Write for MemWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.nodes
            .lock()
            .unwrap()
            .insert(self.path.clone(), MemNode::File(self.bytes.clone()));
        Ok(())
    }
}

impl SyncWrite for MemWriter {
    fn sync_data(&mut self) -> io::Result<()> {
        std::io::Write::flush(self)
    }
}

fn not_there(path: &Path) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, path.display().to_string())
}

impl FileOps for CaseKeepingFs {
    fn probe(&self, path: &Path) -> io::Result<TargetState> {
        let nodes = self.nodes.lock().unwrap();
        let (is_dir, size) = match nodes.get(path) {
            Some(MemNode::File(bytes)) => (false, bytes.len() as u64),
            Some(MemNode::Folder) => (true, 0),
            None => return Err(not_there(path)),
        };
        Ok(TargetState {
            is_dir,
            is_link: false,
            read_only: false,
            hidden: false,
            size,
            modified: None,
            created: None,
        })
    }
    fn create_dir(&self, path: &Path) -> io::Result<()> {
        let mut nodes = self.nodes.lock().unwrap();
        match nodes.get(path) {
            Some(MemNode::Folder) => Ok(()),
            Some(MemNode::File(_)) => Err(io::Error::from(io::ErrorKind::AlreadyExists)),
            None => {
                nodes.insert(path.to_path_buf(), MemNode::Folder);
                Ok(())
            }
        }
    }
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn Read + Send>> {
        match self.nodes.lock().unwrap().get(path) {
            Some(MemNode::File(bytes)) => Ok(Box::new(io::Cursor::new(bytes.clone()))),
            Some(MemNode::Folder) => Err(io::Error::from(io::ErrorKind::IsADirectory)),
            None => Err(not_there(path)),
        }
    }
    fn create_new(&self, path: &Path) -> io::Result<Box<dyn SyncWrite>> {
        if self.nodes.lock().unwrap().contains_key(path) {
            return Err(io::Error::from(io::ErrorKind::AlreadyExists));
        }
        Ok(Box::new(MemWriter {
            nodes: std::sync::Arc::clone(&self.nodes),
            path: path.to_path_buf(),
            bytes: Vec::new(),
        }))
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let mut nodes = self.nodes.lock().unwrap();
        let node = nodes.remove(from).ok_or_else(|| not_there(from))?;
        nodes.insert(to.to_path_buf(), node);
        Ok(())
    }
    fn rename_no_replace(&self, from: &Path, to: &Path) -> io::Result<()> {
        if self.nodes.lock().unwrap().contains_key(to) {
            return Err(io::Error::from(io::ErrorKind::AlreadyExists));
        }
        self.rename(from, to)
    }
    fn remove_file(&self, path: &Path) -> io::Result<()> {
        let mut nodes = self.nodes.lock().unwrap();
        match nodes.get(path) {
            Some(MemNode::File(_)) => {
                nodes.remove(path);
                Ok(())
            }
            Some(MemNode::Folder) => Err(io::Error::from(io::ErrorKind::IsADirectory)),
            None => Err(not_there(path)),
        }
    }
    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        let mut nodes = self.nodes.lock().unwrap();
        if nodes.keys().any(|other| other.parent() == Some(path)) {
            return Err(io::Error::from(io::ErrorKind::DirectoryNotEmpty));
        }
        nodes
            .remove(path)
            .map(|_| ())
            .ok_or_else(|| not_there(path))
    }
    fn move_to_trash(&self, _path: &Path) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
    fn set_modified(&self, _path: &Path, _time: SystemTime) -> io::Result<()> {
        Ok(())
    }
    fn set_created(&self, _path: &Path, _time: SystemTime) -> io::Result<()> {
        Ok(())
    }
    fn set_attributes(&self, _path: &Path, _change: &AttributeChange) -> io::Result<()> {
        Ok(())
    }
    fn same_volume(&self, _from: &Path, _to: &Path) -> bool {
        true
    }
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        Ok(self
            .nodes
            .lock()
            .unwrap()
            .keys()
            .filter(|other| other.parent() == Some(path))
            .cloned()
            .collect())
    }
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        Ok(path.to_path_buf())
    }
    fn identity(&self, _path: &Path) -> io::Result<Option<crate::ops::fsops::ItemIdentity>> {
        Ok(None)
    }
    fn keeps_case(&self, _path: &Path) -> bool {
        self.keeps_case
    }
}

/// A listed file of `size` bytes with no time, for a tree built by hand.
fn listed_file(rel: &str, size: u64) -> crate::scan::Entry {
    crate::scan::Entry {
        rel: PathBuf::from(rel),
        name: rel.to_owned(),
        is_dir: false,
        size,
        modified: None,
        created: None,
        attributes: crate::scan::Attributes::default(),
        link: None,
        listing_incomplete: false,
        error: None,
        refused: false,
    }
}

/// On a volume that keeps case, `b.txt` renamed to `B.txt` replaces the
/// sibling `B.txt`. The backup of the sibling takes its name before the
/// record of the step is written, so the journal names it and a recovery
/// after a crash lists it.
#[test]
fn a_case_only_rename_onto_a_sibling_journals_its_backup() {
    let base = PathBuf::from("/volume/left");
    let other = PathBuf::from("/volume/right");
    let fs = CaseKeepingFs::default();
    fs.folder(&base);
    fs.folder(&other);
    fs.file(&base.join("b.txt"), b"lower");
    fs.file(&base.join("B.txt"), b"UPPER");

    let mut left = ScanResult::default();
    for name in ["b.txt", "B.txt"] {
        left.entries
            .insert(PathBuf::from(name), listed_file(name, 5));
    }
    let alignment = AlignmentOptions {
        case: crate::filter::CaseSensitivity::Sensitive,
        ..AlignmentOptions::default()
    };
    let mut root = align_trees(&left, &ScanResult::default(), &alignment, &Cancel::new());
    compare_quick(&mut root, &CompareOptions::default());
    let opts = OperationOptions {
        backup: Some(BackupOptions::default()),
        ..options()
    };
    let bases = Bases {
        left: &base,
        right: &other,
    };
    let plan = rename_plan(&root, &["b.txt"], Sides::Left, &mask("B.txt"), bases, &opts).unwrap();
    let [step] = plan.steps.as_slice() else {
        panic!("{:?}", plan.steps);
    };
    assert!(step.conflicts.contains(&Conflict::TargetExists), "{step:?}");

    let journals = tempfile::tempdir().unwrap();
    let journal = Journal::create_in(journals.path()).unwrap();
    let cancel = Cancel::new();
    let report = execute(
        &plan,
        &ExecutionContext::new(&fs, &cancel, Journaling::To(&journal)),
    );
    let journal_path = journal.path().to_path_buf();
    drop(journal);

    assert!(report.is_clean(), "{:?}", report.results);
    let settled = base.join("B.txt.bak");
    assert_eq!(report.results[0].backup.as_deref(), Some(settled.as_path()));
    assert_eq!(
        fs.contents(),
        BTreeMap::from([
            (base.join("B.txt"), b"lower".to_vec()),
            (settled.clone(), b"UPPER".to_vec()),
        ])
    );
    assert_eq!(
        journalled_backups(&journal_path),
        vec![Some(settled.clone())]
    );
    assert_eq!(recover(&journal_path).unwrap().backups, vec![settled]);
}

/// A case-folded path match on a volume with unknown case behavior is not
/// proof that two paths name one item. State differences prove two items;
/// matching states leave the result unproven.
#[test]
fn case_only_paths_with_unknown_case_behavior_use_their_states() {
    let base = PathBuf::from("/volume/left");
    let first = base.join("b.txt");
    let second = base.join("B.txt");
    let fs = CaseKeepingFs::with_unknown_case_behavior();
    fs.folder(&base);
    fs.file(&first, b"lower");
    fs.file(&second, b"UPPER");

    assert_eq!(same_item(&fs, &first, &second), Reach::Unproven);

    fs.file(&second, b"different size");
    assert_eq!(same_item(&fs, &first, &second), Reach::TwoItems);
}

// ----------------------------------------------- an exchange meets a taken name

/// The name of every file in `dir` with its content, sorted by name.
fn named_contents(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|entry| {
            (
                entry.file_name().to_string_lossy().into_owned(),
                std::fs::read(entry.path()).unwrap(),
            )
        })
        .collect();
    out.sort();
    out
}

/// Another writer puts a file at the staged name, or at a name the swap has
/// just vacated. The swap never renames over that file: it stops, puts back
/// what it can, and says under which name each file it could not put back is
/// kept. The journal names the staged file as a kept file, so a clean-up of
/// part files never removes it.
#[test]
fn an_exchange_never_replaces_an_item_that_takes_a_name_it_uses() {
    for arrival in ["staged", "right", "left"] {
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        std::fs::write(left.path().join("a.txt"), b"left").unwrap();
        std::fs::write(right.path().join("a.txt"), b"right").unwrap();
        let root = compared(left.path(), right.path());
        let bases = Bases {
            left: left.path(),
            right: right.path(),
        };
        let sel = selection(&root, &["a.txt"]);
        let plan = plan_exchange(&sel, &sel, bases, &options());
        let fs = match arrival {
            "staged" => FaultFs::arriving_at_suffix(".ca-part", b"INTRUDER"),
            "right" => FaultFs::arriving(&right.path().join("a.txt"), b"INTRUDER"),
            _ => FaultFs::arriving(&left.path().join("a.txt"), b"INTRUDER"),
        };
        let journals = tempfile::tempdir().unwrap();
        let journal = Journal::create_in(journals.path()).unwrap();
        let cancel = Cancel::new();
        let report = execute(
            &plan,
            &ExecutionContext::new(&fs, &cancel, Journaling::To(&journal)),
        );
        let journal_path = journal.path().to_path_buf();
        drop(journal);

        let StepOutcome::Failed { message } = &report.results[0].outcome else {
            panic!("{arrival}: {:?}", report.results[0].outcome);
        };
        let mut contents: Vec<Vec<u8>> = named_contents(left.path())
            .into_iter()
            .chain(named_contents(right.path()))
            .map(|(_, bytes)| bytes)
            .collect();
        contents.sort();
        assert_eq!(
            contents,
            vec![b"INTRUDER".to_vec(), b"left".to_vec(), b"right".to_vec()],
            "{arrival}: a file was lost"
        );
        let kept_as_staged: Vec<String> = named_contents(right.path())
            .into_iter()
            .filter(|(name, bytes)| {
                crate::ops::exec::is_temporary_name(name) && bytes.as_slice() == b"right"
            })
            .map(|(name, _)| name)
            .collect();
        for name in &kept_as_staged {
            assert!(message.contains(name.as_str()), "{arrival}: {message}");
        }

        let recovery = recover(&journal_path).unwrap();
        assert!(recovery.leftover_temporaries.is_empty(), "{arrival}");
        assert_eq!(recovery.backups.len(), 1, "{arrival}");
        assert!(recovery.clean_up(&RealFs).is_empty());
        let mut after: Vec<Vec<u8>> = named_contents(left.path())
            .into_iter()
            .chain(named_contents(right.path()))
            .map(|(_, bytes)| bytes)
            .collect();
        after.sort();
        assert_eq!(after, contents, "{arrival}: the clean-up removed a file");
    }
}

/// A folder name is one name. An empty name, `.`, `..` and a name that holds
/// a separator give no step, and the plan says why.
#[test]
fn a_new_folder_name_that_is_not_one_name_is_refused_while_planning() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::create_dir(left.path().join("a")).unwrap();
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let mut names = vec!["", ".", "..", "a/b", "/a", "a/"];
    if cfg!(windows) {
        names.extend([r"a\b", "C:x"]);
    }
    for name in names {
        let plan = plan_new_folder(
            &compared(left.path(), right.path()),
            Path::new(""),
            name,
            Sides::Both,
            bases,
            &options(),
        );
        assert!(plan.steps.is_empty(), "{name:?}: {:?}", plan.steps);
        let [skip] = plan.skipped.as_slice() else {
            panic!("{name:?}: {:?}", plan.skipped);
        };
        assert!(skip.reason.contains("one folder name"), "{}", skip.reason);
        assert!(run(&plan).results.is_empty());
    }
    assert_eq!(
        snapshot(left.path()),
        BTreeMap::from([(PathBuf::from("a"), Vec::new())])
    );
    assert!(snapshot(right.path()).is_empty());
}

#[test]
fn a_sync_never_trashes_a_folder_that_holds_an_excluded_item_below_its_children() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(right.path().join("orphan/sub")).unwrap();
    std::fs::write(right.path().join("orphan/a.txt"), b"a").unwrap();
    std::fs::write(right.path().join("orphan/sub/b.txt"), b"b").unwrap();
    std::fs::write(right.path().join("orphan/sub/notes.log"), b"excluded").unwrap();

    let mut root = compared(left.path(), right.path());
    crate::compare::apply_filters(
        &mut root,
        &crate::filter::NameFilters::from_lists("*.txt", "", "", ""),
        &crate::filter::OtherFilters::default(),
        &crate::filter::FilterContext::default(),
    );
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let mut opts = options();
    opts.use_recycle_bin = true;
    let view = preview(&root, &SyncPreset::MirrorToRight);
    let plan = plan_sync(&root, &SyncPreset::MirrorToRight, bases, &opts, &view).unwrap();
    assert!(plan.steps.iter().any(
        |step| matches!(&step.action, StepAction::Trash { path } if path.ends_with("orphan"))
    ));

    let fs = FaultFs::new(Vec::new());
    let cancel = Cancel::new();
    let _ = execute(
        &plan,
        &ExecutionContext::new(&fs, &cancel, Journaling::Disabled)
            .with_policy(&ApproveOutrightRemoval),
    );
    assert_eq!(
        std::fs::read(right.path().join("orphan/sub/notes.log")).unwrap(),
        b"excluded"
    );
}

#[test]
fn a_case_only_rename_stranded_under_its_intermediate_name_is_journalled_and_reported() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("readme.txt"), b"body").unwrap();
    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let plan = plan_rename(
        &root,
        &selection(&root, &["readme.txt"]),
        Sides::Left,
        bases,
        &RenameAction::Mask("README.txt".to_string()),
        &options(),
    )
    .unwrap();

    // The rename onto the new spelling fails, and so does the way back.
    let fs = FaultFs {
        failing_rename: Some(1),
        ..FaultFs::new(vec![Fault {
            call: "rename",
            path_contains: "/readme.txt".to_owned(),
            kind: io::ErrorKind::PermissionDenied,
        }])
    };
    let journals = tempfile::tempdir().unwrap();
    let journal = Journal::create_in(journals.path()).unwrap();
    let cancel = Cancel::new();
    let report = execute(
        &plan,
        &ExecutionContext::new(&fs, &cancel, Journaling::To(&journal)),
    );
    let journal_path = journal.path().to_path_buf();
    drop(journal);

    let listed: Vec<PathBuf> = std::fs::read_dir(left.path())
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .collect();
    let [parked] = listed.as_slice() else {
        panic!("{listed:?}");
    };
    assert_eq!(std::fs::read(parked).unwrap(), b"body");
    let StepOutcome::Failed { message } = &report.results[0].outcome else {
        panic!("{:?}", report.results[0].outcome);
    };
    assert!(message.contains(&parked.display().to_string()), "{message}");
    assert_eq!(report.results[0].backup.as_deref(), Some(parked.as_path()));
    assert_eq!(
        journalled_backups(&journal_path),
        vec![Some(parked.clone())]
    );
}

#[test]
fn a_sync_copies_a_kept_child_of_an_excluded_orphan_folder_into_place() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::create_dir(left.path().join("orphan")).unwrap();
    std::fs::write(left.path().join("orphan/a.txt"), b"a").unwrap();
    let root = compared(left.path(), right.path());
    let bases = Bases {
        left: left.path(),
        right: right.path(),
    };
    let mut view = preview(&root, &SyncPreset::MirrorToRight);
    view.set_override(PathBuf::from("orphan"), SyncAction::LeaveAlone);
    let pending: Vec<PathBuf> = view.pending().iter().map(|row| row.rel.clone()).collect();
    assert_eq!(pending, vec![PathBuf::from("orphan/a.txt")]);

    let plan = plan_sync(&root, &SyncPreset::MirrorToRight, bases, &options(), &view).unwrap();
    let report = run(&plan);
    assert!(report.is_clean(), "{:?}", report.results);
    assert_eq!(
        std::fs::read(right.path().join("orphan/a.txt")).unwrap(),
        b"a"
    );
}
