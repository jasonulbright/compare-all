//! Execution of a plan, one step at a time.
//!
//! Execution never abandons a batch because one step failed: every step yields
//! a result and the caller's error policy decides whether the batch carries
//! on. A copy is written to a uniquely named temporary file in the
//! destination's own directory and renamed over the target only once it is
//! complete and on stable storage, so an interrupted copy leaves the previous
//! target untouched.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::cancel::Cancel;
use crate::journal::{now_unix_seconds, JournalRecord, JournalWriter};
use crate::ops::fsops::{same_item, AttributeChange, FileOps, Reach, TargetState};
use crate::ops::plan::{
    onto_itself, path_is_within, Conflict, OperationOptions, OperationPlan, PathState, PlanStep,
    StepAction, Verify,
};

/// Suffix that marks a partially written destination file.
const TEMP_SUFFIX: &str = ".ca-part";

/// Distinguishes temporary names created within one process.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// How one step ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "outcome",
    rename_all = "snake_case",
    rename_all_fields = "snake_case"
)]
pub enum StepOutcome {
    /// The step did what it said it would.
    Done,
    /// The step was not attempted, or was abandoned before it changed
    /// anything.
    Skipped {
        /// Why the step was passed over.
        reason: String,
    },
    /// The step failed and left its target as it found it.
    Failed {
        /// Text of the underlying error.
        message: String,
    },
    /// A move wrote its destination and could not then remove its source, so
    /// the item now exists in both places.
    CopiedSourceRemains {
        /// Source that could not be removed.
        source: PathBuf,
        /// Destination that was written.
        target: PathBuf,
        /// Text of the error that stopped the removal.
        message: String,
    },
    /// The cancel flag was set before or during the step.
    Cancelled,
}

impl StepOutcome {
    /// True only for a step that completed.
    #[must_use]
    pub fn is_done(&self) -> bool {
        matches!(self, Self::Done)
    }

    fn skipped(reason: impl Into<String>) -> Self {
        Self::Skipped {
            reason: reason.into(),
        }
    }

    fn failed(error: &io::Error) -> Self {
        Self::Failed {
            message: error.to_string(),
        }
    }
}

/// What the caller wants done about a failed step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Leave the step undone and carry on with the next one.
    Skip,
    /// Attempt the same step again.
    Retry,
    /// Stop the batch; the steps already done stay done.
    Abort,
}

/// What the caller wants done about a step whose conflicts the plan flagged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictDecision {
    /// Run the step.
    Proceed,
    /// Leave the step undone.
    Skip,
    /// Stop the batch.
    Abort,
}

/// Where the answers to "this step failed" and "this step conflicts" come
/// from.
///
/// A user interface implements this by asking; an unattended run uses
/// [`ContinueOnError`].
pub trait ErrorPolicy: Send + Sync {
    /// Decide what to do about a step that failed.
    ///
    /// `attempt` counts from one and lets a policy stop retrying.
    fn on_error(&self, step: &PlanStep, attempt: u32, message: &str) -> Decision;

    /// Decide what to do about a step the plan flagged as conflicting.
    ///
    /// The conflicts were shown before execution began, so the default is to
    /// proceed.
    fn on_conflict(&self, _step: &PlanStep) -> ConflictDecision {
        ConflictDecision::Proceed
    }

    /// Decide what to do about a step whose paths no longer hold what the plan
    /// was built from.
    ///
    /// Nobody has seen the state the disk is actually in, so an unattended run
    /// leaves the step undone.
    fn on_drift(&self, _step: &PlanStep, _drift: &Drift) -> ConflictDecision {
        ConflictDecision::Skip
    }

    /// Decide what to do about a deletion the trash refuses to take.
    ///
    /// The only way to carry the step out is an outright removal, which is not
    /// what the plan promised and cannot be undone, so it happens only when
    /// this returns [`ConflictDecision::Proceed`]. An unattended run leaves the
    /// item where it is.
    fn on_recycle_bin_unavailable(&self, _step: &PlanStep) -> ConflictDecision {
        ConflictDecision::Skip
    }
}

/// How the disk departs from what the plan recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drift {
    /// Path whose state moved on.
    pub path: PathBuf,
    /// What the plan expected to find.
    pub expected: String,
    /// What was found instead.
    pub found: String,
}

impl std::fmt::Display for Drift {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} was {} when the plan was built and is {} now",
            self.path.display(),
            self.expected,
            self.found
        )
    }
}

/// The non-interactive policy: skip whatever fails, run everything else.
#[derive(Debug, Clone, Copy, Default)]
pub struct ContinueOnError;

impl ErrorPolicy for ContinueOnError {
    fn on_error(&self, _step: &PlanStep, _attempt: u32, _message: &str) -> Decision {
        Decision::Skip
    }
}

/// The policy that stops at the first failure.
#[derive(Debug, Clone, Copy, Default)]
pub struct AbortOnError;

impl ErrorPolicy for AbortOnError {
    fn on_error(&self, _step: &PlanStep, _attempt: u32, _message: &str) -> Decision {
        Decision::Abort
    }
}

/// An event reported while a batch runs.
#[derive(Debug)]
pub enum Progress<'a> {
    /// A step is about to run.
    StepStarted {
        /// The step.
        step: &'a PlanStep,
    },
    /// Bytes have been written for the step at `index`.
    Bytes {
        /// Position of the step in the plan.
        index: usize,
        /// Bytes written so far for this step.
        done: u64,
        /// Bytes the step expects to write.
        total: u64,
    },
    /// A step has finished.
    StepFinished {
        /// Position of the step in the plan.
        index: usize,
        /// How it ended.
        outcome: &'a StepOutcome,
    },
}

/// Result of one step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepResult {
    /// Position of the step in the plan.
    pub index: usize,
    /// How the step ended.
    pub outcome: StepOutcome,
    /// Where the replaced file was saved, when one was replaced and a backup
    /// was asked for. The name is settled here, not while planning.
    pub backup: Option<PathBuf>,
}

/// What a whole batch did.
#[derive(Debug, Clone, Default)]
pub struct ExecutionReport {
    /// One entry per step the batch reached.
    pub results: Vec<StepResult>,
    /// Bytes written by completed copies and moves.
    pub bytes_copied: u64,
    /// True when the cancel flag stopped the batch.
    pub cancelled: bool,
    /// True when the error policy stopped the batch.
    pub aborted: bool,
}

impl ExecutionReport {
    /// Steps that completed.
    #[must_use]
    pub fn completed(&self) -> usize {
        self.results
            .iter()
            .filter(|result| result.outcome.is_done())
            .count()
    }

    /// Steps that failed, with their messages.
    #[must_use]
    pub fn failures(&self) -> Vec<(usize, String)> {
        self.results
            .iter()
            .filter_map(|result| match &result.outcome {
                StepOutcome::Failed { message } => Some((result.index, message.clone())),
                _ => None,
            })
            .collect()
    }

    /// True when every step the batch reached completed.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        !self.cancelled
            && !self.aborted
            && self.results.iter().all(|result| result.outcome.is_done())
    }
}

static SKIP_ON_ERROR: ContinueOnError = ContinueOnError;
static SILENT: fn(Progress<'_>) = |_| {};

/// Whether a batch keeps a write-ahead record, stated rather than defaulted.
///
/// Running without one leaves an interrupted batch with nothing that names the
/// step that was in flight, so the choice is made where the batch is set up.
#[derive(Debug, Clone, Copy)]
pub enum Journaling<'a> {
    /// Record every step to this journal before and after it runs.
    To(&'a dyn JournalWriter),
    /// Run with no record at all.
    Disabled,
}

impl<'a> Journaling<'a> {
    fn journal(self) -> Option<&'a dyn JournalWriter> {
        match self {
            Self::To(journal) => Some(journal),
            Self::Disabled => None,
        }
    }
}

/// Everything execution needs besides the plan itself.
pub struct ExecutionContext<'a> {
    /// The file system calls every step goes through.
    pub fs: &'a dyn FileOps,
    /// What to do about failures and conflicts.
    pub policy: &'a dyn ErrorPolicy,
    /// Checked between steps and between blocks of a copy.
    pub cancel: &'a Cancel,
    /// Where progress events go.
    pub progress: &'a (dyn Fn(Progress<'_>) + Sync),
    /// Where the write-ahead record goes.
    pub journal: Journaling<'a>,
}

impl std::fmt::Debug for ExecutionContext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecutionContext")
            .field("journal", &self.journal.journal().map(JournalWriter::path))
            .finish_non_exhaustive()
    }
}

impl<'a> ExecutionContext<'a> {
    /// A context that skips whatever fails and reports nothing, journalling as
    /// `journal` says.
    #[must_use]
    pub fn new(fs: &'a dyn FileOps, cancel: &'a Cancel, journal: Journaling<'a>) -> Self {
        Self {
            fs,
            policy: &SKIP_ON_ERROR,
            cancel,
            progress: &SILENT,
            journal,
        }
    }

    /// Use a different error policy.
    #[must_use]
    pub fn with_policy(mut self, policy: &'a dyn ErrorPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Report progress to `progress`.
    #[must_use]
    pub fn with_progress(mut self, progress: &'a (dyn Fn(Progress<'_>) + Sync)) -> Self {
        self.progress = progress;
        self
    }

    fn record(&self, record: &JournalRecord) {
        if let Some(journal) = self.journal.journal() {
            let _ = journal.append(record);
        }
    }

    /// Write the record that must exist before anything is destroyed.
    ///
    /// A batch that asked for a journal and cannot write one destroys nothing:
    /// without the record there is no way to tell afterwards what was in
    /// flight.
    fn record_before_destruction(&self, record: &JournalRecord) -> Result<(), StepOutcome> {
        match self.journal.journal() {
            None => Ok(()),
            Some(journal) => journal.append(record).map_err(|error| StepOutcome::Failed {
                message: format!("journal write failed, step not attempted: {error}"),
            }),
        }
    }
}

/// Run every step of `plan`.
///
/// Steps run in the order the plan holds them. Each step's outcome is
/// recorded; a failure consults the error policy, and only an
/// [`Decision::Abort`] or a set cancel flag stops the batch.
#[must_use]
pub fn execute(plan: &OperationPlan, ctx: &ExecutionContext<'_>) -> ExecutionReport {
    let mut report = ExecutionReport::default();
    ctx.record(&JournalRecord::BatchStart {
        kind: plan.kind,
        steps: plan.steps.len(),
        unix_seconds: now_unix_seconds(),
    });
    // The session's base folders are resolved once; every step's real
    // destination is then measured against the same fixed ground.
    let roots: Vec<PathBuf> = plan
        .roots
        .iter()
        .map(|root| resolve_root(ctx.fs, root))
        .collect();

    for step in &plan.steps {
        if ctx.cancel.is_cancelled() {
            report.cancelled = true;
            break;
        }
        (ctx.progress)(Progress::StepStarted { step });

        let mut backup = None;
        let outcome = run_step_with_policy(plan, step, ctx, &roots, &mut report, &mut backup);
        if matches!(outcome, StepOutcome::Cancelled) {
            report.cancelled = true;
        }
        (ctx.progress)(Progress::StepFinished {
            index: step.index,
            outcome: &outcome,
        });
        report.results.push(StepResult {
            index: step.index,
            outcome,
            backup,
        });
        if report.cancelled || report.aborted {
            break;
        }
    }

    ctx.record(&JournalRecord::BatchEnd {
        completed: report.completed(),
        cancelled: report.cancelled,
        aborted: report.aborted,
    });
    report
}

/// The real location of one base folder.
///
/// A base folder the batch itself creates does not exist yet, so its parent is
/// resolved instead and the final name appended. Measuring later steps against
/// the unresolved text would refuse every step once the parent's resolved form
/// differs from its spelling, as a verbatim prefix does.
fn resolve_root(fs: &dyn FileOps, root: &Path) -> PathBuf {
    if let Ok(real) = fs.canonicalize(root) {
        return real;
    }
    match (root.parent(), root.file_name()) {
        (Some(parent), Some(name)) if !parent.as_os_str().is_empty() => fs
            .canonicalize(parent)
            .map_or_else(|_| root.to_path_buf(), |real| real.join(name)),
        _ => root.to_path_buf(),
    }
}

fn run_step_with_policy(
    plan: &OperationPlan,
    step: &PlanStep,
    ctx: &ExecutionContext<'_>,
    roots: &[PathBuf],
    report: &mut ExecutionReport,
    backup: &mut Option<PathBuf>,
) -> StepOutcome {
    if !step.conflicts.is_empty() {
        match ctx.policy.on_conflict(step) {
            ConflictDecision::Proceed => {}
            ConflictDecision::Skip => return StepOutcome::skipped("conflict declined"),
            ConflictDecision::Abort => {
                report.aborted = true;
                return StepOutcome::skipped("conflict declined, batch stopped");
            }
        }
    }

    if let Some(drift) = drift(ctx.fs, step) {
        match ctx.policy.on_drift(step, &drift) {
            ConflictDecision::Proceed => {}
            ConflictDecision::Skip => return StepOutcome::skipped(drift.to_string()),
            ConflictDecision::Abort => {
                report.aborted = true;
                return StepOutcome::skipped(format!("{drift}, batch stopped"));
            }
        }
    }

    let mut attempt = 1u32;
    loop {
        let outcome = run_step(plan, step, ctx, roots, report, backup);
        let StepOutcome::Failed { message } = &outcome else {
            return outcome;
        };
        match ctx.policy.on_error(step, attempt, message) {
            Decision::Skip => return outcome,
            Decision::Abort => {
                report.aborted = true;
                return outcome;
            }
            Decision::Retry => {
                attempt = attempt.saturating_add(1);
                if ctx.cancel.is_cancelled() {
                    return StepOutcome::Cancelled;
                }
            }
        }
    }
}

fn run_step(
    plan: &OperationPlan,
    step: &PlanStep,
    ctx: &ExecutionContext<'_>,
    roots: &[PathBuf],
    report: &mut ExecutionReport,
    backup_taken: &mut Option<PathBuf>,
) -> StepOutcome {
    if let Some(reason) = escape(plan, step) {
        return StepOutcome::skipped(reason);
    }
    if let Some(reason) = resolved_escape(ctx.fs, roots, step) {
        return StepOutcome::skipped(reason);
    }
    let options = &plan.options;
    let fs = ctx.fs;

    macro_rules! destructive {
        ($body:expr) => {{
            if let Err(outcome) = ctx.record_before_destruction(&begin(step, None, None)) {
                return outcome;
            }
            let outcome = $body;
            ctx.record(&end(step, &outcome));
            outcome
        }};
    }

    match &step.action {
        StepAction::CreateDir { path } => {
            ctx.record(&begin(step, None, None));
            let outcome = match fs.create_dir(path) {
                Ok(()) => StepOutcome::Done,
                Err(error) => StepOutcome::failed(&error),
            };
            ctx.record(&end(step, &outcome));
            outcome
        }
        StepAction::CopyFile { source, target } => {
            transfer(ctx, plan, step, source, target, false, report, backup_taken)
        }
        StepAction::MoveFile { source, target } => {
            transfer(ctx, plan, step, source, target, true, report, backup_taken)
        }
        StepAction::DeleteFile { path } | StepAction::DeleteLink { path } => {
            destructive!(remove_one(fs, path, options))
        }
        StepAction::DeleteDir { path } => destructive!(match fs.remove_dir(path) {
            Ok(()) => StepOutcome::Done,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                StepOutcome::skipped("already gone")
            }
            Err(error) => StepOutcome::failed(&error),
        }),
        StepAction::Trash { path } => destructive!(match fs.move_to_trash(path) {
            Ok(()) => StepOutcome::Done,
            // A volume with no recycle bin, such as a network share, refuses
            // the call. An outright removal is the only way left to carry the
            // step out, and it cannot be undone, so the policy decides.
            Err(error) if error.kind() == io::ErrorKind::Unsupported => {
                match ctx.policy.on_recycle_bin_unavailable(step) {
                    // The answer can arrive long after the earlier check; the
                    // consent covers only the item the plan described.
                    ConflictDecision::Proceed => match drift(fs, step) {
                        Some(drift) => StepOutcome::skipped(drift.to_string()),
                        None => remove_tree(fs, path, options, ctx.cancel),
                    },
                    ConflictDecision::Skip => {
                        StepOutcome::skipped("no recycle bin here, removal declined")
                    }
                    ConflictDecision::Abort => {
                        report.aborted = true;
                        StepOutcome::skipped("no recycle bin here, removal declined, batch stopped")
                    }
                }
            }
            Err(error) => StepOutcome::failed(&error),
        }),
        StepAction::Rename { from, to } => rename_step(ctx, plan, step, from, to, backup_taken),
        StepAction::SetTimes {
            path,
            modified,
            created,
        } => {
            ctx.record(&begin(step, None, None));
            let outcome = set_times(fs, path, *modified, *created);
            ctx.record(&end(step, &outcome));
            outcome
        }
        StepAction::SetAttributes { path, change } => {
            ctx.record(&begin(step, None, None));
            let outcome = match fs.set_attributes(path, change) {
                Ok(()) => StepOutcome::Done,
                Err(error) => StepOutcome::failed(&error),
            };
            ctx.record(&end(step, &outcome));
            outcome
        }
        StepAction::ExchangeFiles { left, right } => {
            let staging = temporary_path(right);
            // The staged name holds the right file while the swap runs, so it
            // is recorded as a kept file: a clean-up of part files would
            // remove it.
            if let Err(outcome) =
                ctx.record_before_destruction(&begin(step, None, Some(staging.clone())))
            {
                return outcome;
            }
            let outcome = exchange(fs, left, right, &staging, options);
            ctx.record(&end(step, &outcome));
            outcome
        }
    }
}

/// A step whose paths leave the session's base folders is never run, whatever
/// produced the plan.
fn escape(plan: &OperationPlan, step: &PlanStep) -> Option<String> {
    let contained = |path: &Path| plan.roots.iter().any(|root| path_is_within(root, path));
    for written in step.action.written_paths() {
        if !contained(written) {
            return Some(format!(
                "target {} is outside the base folders",
                written.display()
            ));
        }
    }
    match step.action.source() {
        Some(source) if !contained(source) => Some(format!(
            "source {} is outside the base folders",
            source.display()
        )),
        _ => None,
    }
}

/// The same question asked of the disk rather than of the text.
///
/// The parent directory is resolved through every link it holds and the final
/// component is appended, so a link swapped in after the plan was built moves
/// the real destination out of the base folders and the step is not run. The
/// final component itself is deliberately not followed: removing a link is a
/// legitimate step whose target lies elsewhere.
fn resolved_escape(fs: &dyn FileOps, roots: &[PathBuf], step: &PlanStep) -> Option<String> {
    let mut paths: Vec<&Path> = step.action.written_paths();
    if let Some(source) = step.action.source() {
        paths.push(source);
    }
    for path in paths {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let real_parent = match fs.canonicalize(parent) {
            Ok(real) => real,
            Err(error) => return Some(format!("cannot resolve {}: {error}", parent.display())),
        };
        let real = path
            .file_name()
            .map_or(real_parent.clone(), |name| real_parent.join(name));
        if !roots.iter().any(|root| path_is_within(root, &real)) {
            return Some(format!(
                "{} really resolves to {}, outside the base folders",
                path.display(),
                real.display()
            ));
        }
    }
    None
}

/// Compare what the plan recorded against what the disk holds now.
fn drift(fs: &dyn FileOps, step: &PlanStep) -> Option<Drift> {
    let checks = [
        (step.action.source(), step.expected.source.as_ref()),
        (Some(step.action.target()), step.expected.target.as_ref()),
    ];
    for (path, expected) in checks {
        let (Some(path), Some(expected)) = (path, expected) else {
            continue;
        };
        let found = fs.probe(path).ok();
        let Some(found) = found else {
            if expected.exists {
                return Some(Drift {
                    path: path.to_path_buf(),
                    expected: describe_expected(expected),
                    found: "absent".to_string(),
                });
            }
            continue;
        };
        if !expected.exists {
            return Some(Drift {
                path: path.to_path_buf(),
                expected: "absent".to_string(),
                found: describe_found(&found),
            });
        }
        // A folder's own timestamp moves whenever anything under it is written,
        // including by the earlier steps of this very batch, so it says nothing
        // about whether the plan still holds.
        let moved_on = expected.is_dir != found.is_dir
            || expected.is_link != found.is_link
            || (!found.is_dir
                && (expected.size != found.size
                    || !times_agree(expected.modified, found.modified)));
        if moved_on {
            return Some(Drift {
                path: path.to_path_buf(),
                expected: describe_expected(expected),
                found: describe_found(&found),
            });
        }
        // A folder that gained content since it was listed is never carried
        // away recursively on the strength of the old listing.
        if let Some(planned) = expected.children {
            let found = match subtree_items(fs, path, planned) {
                Ok(now) if now == planned => None,
                Ok(now) => Some(format!("a folder holding {now} items")),
                Err(error) => Some(format!("a folder whose contents cannot be read: {error}")),
            };
            if let Some(found) = found {
                return Some(Drift {
                    path: path.to_path_buf(),
                    expected: format!("a folder holding {planned} items"),
                    found,
                });
            }
        }
    }
    None
}

/// Items at every depth under `path`, without entering a link. The walk stops
/// once the count passes `planned`, so a folder that grew is not walked whole.
fn subtree_items(fs: &dyn FileOps, path: &Path, planned: usize) -> io::Result<usize> {
    let mut count = 0usize;
    let mut stack = vec![path.to_path_buf()];
    while let Some(folder) = stack.pop() {
        for child in fs.read_dir(&folder)? {
            count = count.saturating_add(1);
            if count > planned {
                return Ok(count);
            }
            let state = fs.probe(&child)?;
            if state.is_dir && !state.is_link {
                stack.push(child);
            }
        }
    }
    Ok(count)
}

/// File systems store timestamps at their own granularity, so equality is
/// judged with the slack a round trip through one can introduce.
fn times_agree(expected: Option<SystemTime>, found: Option<SystemTime>) -> bool {
    match (expected, found) {
        (Some(left), Some(right)) => left
            .duration_since(right)
            .or_else(|_| right.duration_since(left))
            .is_ok_and(|gap| gap <= std::time::Duration::from_secs(2)),
        (None, _) | (_, None) => true,
    }
}

fn describe_expected(state: &PathState) -> String {
    if state.is_dir {
        "a folder".to_string()
    } else {
        format!("a {} byte file", state.size)
    }
}

fn describe_found(state: &TargetState) -> String {
    if state.is_dir {
        "a folder".to_string()
    } else {
        format!("a {} byte file", state.size)
    }
}

/// Swap two files through a staged name in one of their own directories.
///
/// Every rename refuses a taken name, so an item that another process puts at
/// the staged name, or at a name the swap has vacated, keeps its content. Each
/// rename is undone in turn when a later one fails. An undo that meets a taken
/// name leaves the file under the name it holds, and the failure names it.
fn exchange(
    fs: &dyn FileOps,
    left: &Path,
    right: &Path,
    staging: &Path,
    options: &OperationOptions,
) -> StepOutcome {
    if let Some(outcome) =
        exchange_blocked(fs, left, options).or(exchange_blocked(fs, right, options))
    {
        return outcome;
    }
    if let Err(error) = fs.rename_no_replace(right, staging) {
        return StepOutcome::failed(&error);
    }
    if let Err(error) = fs.rename_no_replace(left, right) {
        if fs.rename_no_replace(staging, right).is_err() {
            return exchange_failed(&error, &[(right, staging)]);
        }
        return StepOutcome::failed(&error);
    }
    if let Err(error) = fs.rename_no_replace(staging, left) {
        if fs.rename_no_replace(right, left).is_err() {
            return exchange_failed(&error, &[(left, right), (right, staging)]);
        }
        if fs.rename_no_replace(staging, right).is_err() {
            return exchange_failed(&error, &[(right, staging)]);
        }
        return StepOutcome::failed(&error);
    }
    StepOutcome::Done
}

/// A failed swap that could not put every file back, with the name each such
/// file is kept under: `(original, kept)`.
fn exchange_failed(error: &io::Error, kept: &[(&Path, &Path)]) -> StepOutcome {
    let mut parts = vec![error.to_string()];
    parts.extend(kept.iter().map(|(original, holder)| {
        format!(
            "the file of {} is kept as {}",
            original.display(),
            holder.display()
        )
    }));
    StepOutcome::Failed {
        message: parts.join("; "),
    }
}

/// A read-only file cannot take part in a swap unless the options allow the
/// flag to be cleared, and finding that out before the first rename is what
/// keeps the swap all-or-nothing.
fn exchange_blocked(
    fs: &dyn FileOps,
    path: &Path,
    options: &OperationOptions,
) -> Option<StepOutcome> {
    match fs.probe(path) {
        Ok(state) if state.read_only && !options.clear_read_only_targets => {
            Some(StepOutcome::skipped("one side is read-only"))
        }
        Ok(_) => None,
        Err(error) => Some(StepOutcome::failed(&error)),
    }
}

fn begin(step: &PlanStep, temporary: Option<PathBuf>, backup: Option<PathBuf>) -> JournalRecord {
    JournalRecord::StepBegin {
        index: step.index,
        action: step.action.clone(),
        temporary,
        backup,
    }
}

fn end(step: &PlanStep, outcome: &StepOutcome) -> JournalRecord {
    JournalRecord::StepEnd {
        index: step.index,
        outcome: outcome.clone(),
    }
}

fn remove_one(fs: &dyn FileOps, path: &Path, options: &OperationOptions) -> StepOutcome {
    let state = match fs.probe(path) {
        Ok(state) => state,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return StepOutcome::skipped("already gone")
        }
        Err(error) => return StepOutcome::failed(&error),
    };
    if state.read_only {
        if !options.clear_read_only_targets {
            return StepOutcome::skipped("read-only");
        }
        if let Err(error) = clear_read_only(fs, path) {
            return StepOutcome::failed(&error);
        }
    }
    // A directory link is removed as a link; nothing under its target is
    // reached through it.
    let result = if state.is_link && state.is_dir {
        fs.remove_dir(path).or_else(|_| fs.remove_file(path))
    } else if state.is_dir {
        fs.remove_dir(path)
    } else {
        fs.remove_file(path)
    };
    match result {
        Ok(()) => StepOutcome::Done,
        Err(error) => StepOutcome::failed(&error),
    }
}

/// Remove a whole subtree without ever descending through a link.
fn remove_tree(
    fs: &dyn FileOps,
    path: &Path,
    options: &OperationOptions,
    cancel: &Cancel,
) -> StepOutcome {
    if cancel.is_cancelled() {
        return StepOutcome::Cancelled;
    }
    let state = match fs.probe(path) {
        Ok(state) => state,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return StepOutcome::skipped("already gone")
        }
        Err(error) => return StepOutcome::failed(&error),
    };
    if state.is_dir && !state.is_link {
        let children = match fs.read_dir(path) {
            Ok(children) => children,
            Err(error) => return StepOutcome::failed(&error),
        };
        for child in children {
            let outcome = remove_tree(fs, &child, options, cancel);
            if !outcome.is_done() {
                return outcome;
            }
        }
    }
    remove_one(fs, path, options)
}

fn clear_read_only(fs: &dyn FileOps, path: &Path) -> io::Result<()> {
    fs.set_attributes(
        path,
        &AttributeChange {
            read_only: Some(false),
            ..AttributeChange::default()
        },
    )
}

fn set_times(
    fs: &dyn FileOps,
    path: &Path,
    modified: Option<SystemTime>,
    created: Option<SystemTime>,
) -> StepOutcome {
    if let Some(modified) = modified {
        if let Err(error) = fs.set_modified(path, modified) {
            return StepOutcome::failed(&error);
        }
    }
    if let Some(created) = created {
        // Creation time is not writable everywhere, and a copy whose contents
        // are correct is not failed for it.
        let _ = fs.set_created(path, created);
    }
    StepOutcome::Done
}

/// Run a rename step: settle the backup name, record it, and rename.
fn rename_step(
    ctx: &ExecutionContext<'_>,
    plan: &OperationPlan,
    step: &PlanStep,
    from: &Path,
    to: &Path,
    backup_taken: &mut Option<PathBuf>,
) -> StepOutcome {
    let options = &plan.options;
    let backup = match rename_backup(ctx.fs, step, from, to, options) {
        Ok(backup) => backup,
        Err(error) => {
            let outcome = StepOutcome::failed(&error);
            ctx.record(&begin(step, None, None));
            ctx.record(&end(step, &outcome));
            return outcome;
        }
    };
    // A case-only rename on a volume that folds case parks the item under an
    // intermediate name. The record names it, so an interruption between the
    // two renames leaves an account of where the item is.
    let intermediate =
        (is_case_only(from, to) && !ctx.fs.keeps_case(from)).then(|| temporary_path(to));
    let kept = backup.clone().or_else(|| intermediate.clone());
    if let Err(outcome) = ctx.record_before_destruction(&begin(step, None, kept)) {
        return outcome;
    }
    let outcome = rename_item(
        ctx.fs,
        step,
        from,
        to,
        options,
        (backup, intermediate),
        backup_taken,
    );
    ctx.record(&end(step, &outcome));
    outcome
}

/// Rename in place, taking a case-only change through an intermediate name
/// unless the file system keeps case, so a volume that folds case does not
/// take the rename for a rename onto itself. A container batch reads each
/// queued rename against the listing the batch started from, where an
/// intermediate name does not exist, so a file system that keeps case
/// renames directly.
///
/// An item that holds the new name is replaced only when the plan recorded it
/// as a conflict of the step and the options allow a replacement. Every other
/// rename refuses a taken name.
fn rename_item(
    fs: &dyn FileOps,
    step: &PlanStep,
    from: &Path,
    to: &Path,
    options: &OperationOptions,
    (settled, intermediate): (Option<PathBuf>, Option<PathBuf>),
    backup_taken: &mut Option<PathBuf>,
) -> StepOutcome {
    if from == to {
        return StepOutcome::skipped("name unchanged");
    }
    let planned = step.conflicts.contains(&Conflict::TargetExists);

    if let Some(intermediate) = intermediate {
        if let Err(error) = fs.rename_no_replace(from, &intermediate) {
            return StepOutcome::failed(&error);
        }
        let outcome = rename_onto(
            fs,
            &intermediate,
            to,
            planned,
            options,
            settled,
            backup_taken,
        );
        if !outcome.is_done() {
            // Put the item back under its original name rather than leaving
            // it under a temporary one.
            if let Err(error) = fs.rename_no_replace(&intermediate, from) {
                let message = format!(
                    "{}; the item could not take its old name back ({error}) and is kept at {}",
                    outcome_text(&outcome),
                    intermediate.display()
                );
                *backup_taken = Some(intermediate);
                return StepOutcome::Failed { message };
            }
        }
        return outcome;
    }
    rename_onto(fs, from, to, planned, options, settled, backup_taken)
}

fn outcome_text(outcome: &StepOutcome) -> String {
    match outcome {
        StepOutcome::Failed { message } => message.clone(),
        StepOutcome::Skipped { reason } => reason.clone(),
        other => format!("{other:?}"),
    }
}

/// True when `to` names `from` again with only the case of its name changed.
fn is_case_only(from: &Path, to: &Path) -> bool {
    from.parent() == to.parent()
        && from
            .file_name()
            .zip(to.file_name())
            .is_some_and(|(left, right)| {
                left != right
                    && left.to_string_lossy().to_lowercase()
                        == right.to_string_lossy().to_lowercase()
            })
}

/// The backup name a rename settles on before its record is written.
///
/// Only a rename the plan made a replacement, onto a file other than the
/// item itself, takes a backup. A case-only rename on a volume that folds
/// case finds the item itself at the new name and settles on none; on a
/// volume that keeps case it finds the sibling it replaces.
fn rename_backup(
    fs: &dyn FileOps,
    step: &PlanStep,
    from: &Path,
    to: &Path,
    options: &OperationOptions,
) -> io::Result<Option<PathBuf>> {
    if from == to
        || options.backup.is_none()
        || !options.overwrite
        || !step.conflicts.contains(&Conflict::TargetExists)
    {
        return Ok(None);
    }
    match fs.probe(to) {
        Ok(existing) if !existing.is_dir && same_item(fs, from, to) == Reach::TwoItems => {
            backup_name(fs, to, Some(&existing), options)
        }
        _ => Ok(None),
    }
}

/// Give `from` the path `to`. An item at `to` is replaced only when `planned`
/// says the plan recorded it and the options allow a replacement; it is then
/// backed up under `settled`, or under the first free name when none was
/// settled, and prepared as a copy's target is.
fn rename_onto(
    fs: &dyn FileOps,
    from: &Path,
    to: &Path,
    planned: bool,
    options: &OperationOptions,
    settled: Option<PathBuf>,
    backup_taken: &mut Option<PathBuf>,
) -> StepOutcome {
    let existing = match fs.probe(to) {
        Ok(state) => state,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return match fs.rename_no_replace(from, to) {
                Ok(()) => StepOutcome::Done,
                Err(error) => StepOutcome::failed(&error),
            };
        }
        Err(error) => return StepOutcome::failed(&error),
    };
    if !options.overwrite {
        return StepOutcome::skipped("target exists and overwrite is off");
    }
    if !planned {
        return StepOutcome::skipped("target exists and the plan does not replace it");
    }
    match fs.probe(from) {
        Ok(source) if source.is_dir || existing.is_dir => {
            return StepOutcome::skipped("a rename replaces only a file with a file");
        }
        Ok(_) => {}
        Err(error) => return StepOutcome::failed(&error),
    }
    let backup = match settled {
        Some(name) => Some(name),
        None => match backup_name(fs, to, Some(&existing), options) {
            Ok(name) => name,
            Err(error) => return StepOutcome::failed(&error),
        },
    };
    if let Some(name) = backup {
        if let Err(error) = write_backup(fs, to, &name, options) {
            return StepOutcome::failed(&error);
        }
        *backup_taken = Some(name);
    }
    if let Some(outcome) = prepare_target(fs, to, Some(&existing), options) {
        return outcome;
    }
    match fs.rename(from, to) {
        Ok(()) => StepOutcome::Done,
        Err(error) => StepOutcome::failed(&error),
    }
}

/// A name that does not exist yet, in the destination's own directory so the
/// final rename stays on one volume.
pub(crate) fn temporary_path(target: &Path) -> PathBuf {
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
    let stamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.subsec_nanos());
    let name = target
        .file_name()
        .map_or_else(|| String::from("item"), |n| n.to_string_lossy().to_string());
    let unique = format!(
        "{name}.{}-{counter}-{stamp}{TEMP_SUFFIX}",
        std::process::id()
    );
    target
        .parent()
        .map_or_else(|| PathBuf::from(&unique), |parent| parent.join(&unique))
}

#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
fn transfer(
    ctx: &ExecutionContext<'_>,
    plan: &OperationPlan,
    step: &PlanStep,
    source: &Path,
    target: &Path,
    moving: bool,
    report: &mut ExecutionReport,
    backup_taken: &mut Option<PathBuf>,
) -> StepOutcome {
    let fs = ctx.fs;
    let options = &plan.options;

    let bracket = |outcome: StepOutcome| -> StepOutcome {
        ctx.record(&begin(step, None, None));
        ctx.record(&end(step, &outcome));
        outcome
    };

    let source_state = match fs.probe(source) {
        Ok(state) => state,
        Err(error) => return bracket(StepOutcome::failed(&error)),
    };

    let existing = match fs.probe(target) {
        Ok(state) => Some(state),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return bracket(StepOutcome::failed(&error)),
    };
    // A copy onto the source itself replaces the source with its own bytes,
    // and a move then removes the only copy.
    if existing.is_some() {
        if let Some(reason) = onto_itself(same_item(fs, source, target)) {
            return bracket(StepOutcome::skipped(reason));
        }
    }
    if existing.is_some() && !options.overwrite {
        return bracket(StepOutcome::skipped("target exists and overwrite is off"));
    }
    if existing.is_some() && !replacement_planned(step) {
        return bracket(StepOutcome::skipped(
            "target exists and the plan does not replace it",
        ));
    }
    let replacing = existing.is_some();

    // A move whose source cannot be removed would otherwise replace the
    // destination and then find that out, leaving two copies where the caller
    // asked for one.
    if moving && source_state.read_only && !options.clear_read_only_targets {
        return bracket(StepOutcome::skipped(
            "source is read-only, so it cannot move",
        ));
    }

    // What the destination must match is read from the source before anything
    // is replaced, because a move leaves no source to compare against
    // afterwards.
    let expectation = match expected_content(fs, source, options) {
        Ok(expectation) => expectation,
        Err(error) => return bracket(StepOutcome::failed(&error)),
    };

    // One record covers the rename and the copy it can fall back to, so it
    // names the temporary the copy writes and the backup the step settles on.
    let backup = match backup_name(fs, target, existing.as_ref(), options) {
        Ok(backup) => backup,
        Err(error) => return bracket(StepOutcome::failed(&error)),
    };
    let temporary = temporary_path(target);
    if let Err(outcome) =
        ctx.record_before_destruction(&begin(step, Some(temporary.clone()), backup.clone()))
    {
        return outcome;
    }
    let finish = |outcome: StepOutcome| -> StepOutcome {
        ctx.record(&end(step, &outcome));
        outcome
    };
    if let Some(name) = &backup {
        if let Err(error) = write_backup(fs, target, name, options) {
            return finish(StepOutcome::failed(&error));
        }
        *backup_taken = Some(name.clone());
    }

    // A rename keeps the destination's data intact until the instant it is
    // replaced, so it is preferred whenever the volumes allow it. It is given
    // up when a check is asked for and there is something to lose, because
    // only the copy path can verify before it replaces.
    let verifying_over_something = options.verify != Verify::None && existing.is_some();
    if moving && !verifying_over_something && fs.same_volume(source, target) {
        if let Some(outcome) = prepare_target(fs, target, existing.as_ref(), options) {
            return finish(outcome);
        }
        match commit(fs, source, target, replacing) {
            Ok(()) => {
                let outcome = match check_content(fs, target, &expectation, options) {
                    Ok(()) => {
                        report.bytes_copied = report.bytes_copied.saturating_add(source_state.size);
                        StepOutcome::Done
                    }
                    Err(error) => StepOutcome::failed(&error),
                };
                return finish(outcome);
            }
            // A copy would meet the same taken name at its own commit.
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                return finish(StepOutcome::failed(&error));
            }
            Err(_) => {}
        }
    }

    // A replacement keeps the access of the file it replaces; a new file takes
    // the access of its source.
    let access_of = if replacing { target } else { source };
    let copied = copy_into_temporary(
        ctx,
        step,
        source,
        &temporary,
        access_of,
        options,
        source_state.size,
    );
    let copied = match copied {
        Ok(bytes) => bytes,
        Err(outcome) => {
            let _ = fs.remove_file(&temporary);
            return finish(outcome);
        }
    };

    if options.preserve_modified {
        if let Some(modified) = source_state.modified {
            let _ = fs.set_modified(&temporary, modified);
        }
    }
    if options.preserve_created {
        if let Some(created) = source_state.created {
            let _ = fs.set_created(&temporary, created);
        }
    }
    if options.preserve_attributes {
        let _ = fs.set_attributes(
            &temporary,
            &AttributeChange {
                hidden: Some(source_state.hidden),
                ..AttributeChange::default()
            },
        );
    }

    // The check runs against the temporary, while the previous target is still
    // whole; a copy that does not match is thrown away and nothing is
    // replaced.
    if let Err(error) = check_content(fs, &temporary, &expectation, options) {
        let _ = fs.remove_file(&temporary);
        return finish(StepOutcome::failed(&error));
    }

    if let Some(outcome) = prepare_target(fs, target, existing.as_ref(), options) {
        let _ = fs.remove_file(&temporary);
        return finish(outcome);
    }

    let committed = if replacing {
        fs.replace(&temporary, target)
    } else {
        commit(fs, &temporary, target, false)
    };
    if let Err(error) = committed {
        let _ = fs.remove_file(&temporary);
        return finish(StepOutcome::failed(&error));
    }
    // A replacement can keep the creation time and the Hidden flag of the
    // file it replaced, so the source's values are applied again.
    if replacing && options.preserve_created {
        if let Some(created) = source_state.created {
            let _ = fs.set_created(target, created);
        }
    }
    if replacing && options.preserve_attributes {
        let _ = fs.set_attributes(
            target,
            &AttributeChange {
                hidden: Some(source_state.hidden),
                ..AttributeChange::default()
            },
        );
    }

    // A destination that refuses a client-set time keeps a time of its own, so
    // the source is given that time; otherwise the next comparison reports the
    // pair as different again.
    if options.touch_source_after_copy && !moving {
        if let Some(written) = fs.probe(target).ok().and_then(|state| state.modified) {
            let _ = fs.set_modified(source, written);
        }
    }

    // The source is removed only once the destination is in place and has
    // passed whatever check the options asked for.
    if moving {
        let removal = remove_one(fs, source, options);
        if !removal.is_done() {
            return finish(StepOutcome::CopiedSourceRemains {
                source: source.to_path_buf(),
                target: target.to_path_buf(),
                message: outcome_text(&removal),
            });
        }
    }

    report.bytes_copied = report.bytes_copied.saturating_add(copied);
    finish(StepOutcome::Done)
}

/// The first numbered backup name of `target` that no item holds, when the
/// options ask for a backup and `existing` is an item to save.
///
/// An existing backup is never overwritten, so a target replaced repeatedly
/// keeps one saved copy per replacement.
fn backup_name(
    fs: &dyn FileOps,
    target: &Path,
    existing: Option<&TargetState>,
    options: &OperationOptions,
) -> io::Result<Option<PathBuf>> {
    let (Some(backup), Some(_)) = (options.backup.as_ref(), existing) else {
        return Ok(None);
    };
    for index in 0..10_000usize {
        let candidate = backup.path_for_index(target, index);
        if fs.probe(&candidate).is_err() {
            return Ok(Some(candidate));
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free backup name",
    ))
}

/// Save the file about to be replaced under `name`.
///
/// The copy takes the name only while the name is free, so a file that
/// another writer puts there after [`backup_name`] chose it fails the step and
/// keeps its content.
fn write_backup(
    fs: &dyn FileOps,
    target: &Path,
    name: &Path,
    options: &OperationOptions,
) -> io::Result<()> {
    let mut reader = fs.open_read(target)?;
    fs.write_new(name, &mut *reader, options.buffer_size)?;
    fs.match_access(target, name)
}

/// True when the plan recorded an item at the step's target as
/// [`Conflict::TargetExists`], which is the only consent a replacement has.
fn replacement_planned(step: &PlanStep) -> bool {
    step.conflicts.contains(&Conflict::TargetExists)
}

/// Give `from` the path `to`, replacing an item there only when `replacing`
/// says the step found one it may replace.
///
/// Every other commit refuses a taken name. On a local Windows volume the
/// refusal is part of the rename call, so an item that another process
/// creates at `to` after the check keeps its content. On other platforms, and
/// in a container or a remote location, the check and the rename are separate
/// operations.
fn commit(fs: &dyn FileOps, from: &Path, to: &Path, replacing: bool) -> io::Result<()> {
    if replacing {
        return fs.rename(from, to);
    }
    fs.rename_no_replace(from, to).map_err(|error| {
        if error.kind() == io::ErrorKind::AlreadyExists {
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "{} appeared after the check and was not replaced",
                    to.display()
                ),
            )
        } else {
            error
        }
    })
}

/// Make the target replaceable, or say why the step cannot go ahead.
fn prepare_target(
    fs: &dyn FileOps,
    target: &Path,
    existing: Option<&TargetState>,
    options: &OperationOptions,
) -> Option<StepOutcome> {
    let existing = existing?;
    if existing.is_dir {
        return Some(StepOutcome::skipped("target is a folder"));
    }
    if existing.read_only {
        if !options.clear_read_only_targets {
            return Some(StepOutcome::skipped("target is read-only"));
        }
        if let Err(error) = clear_read_only(fs, target) {
            return Some(StepOutcome::failed(&error));
        }
    }
    None
}

/// Copy `source` into the new file `temporary`, which takes the access of
/// `access_of` before any content is written to it.
fn copy_into_temporary(
    ctx: &ExecutionContext<'_>,
    step: &PlanStep,
    source: &Path,
    temporary: &Path,
    access_of: &Path,
    options: &OperationOptions,
    total: u64,
) -> Result<u64, StepOutcome> {
    let fs = ctx.fs;
    let mut reader = fs.open_read(source).map_err(|e| StepOutcome::failed(&e))?;
    let mut writer = fs
        .create_new(temporary)
        .map_err(|e| StepOutcome::failed(&e))?;
    fs.match_access(access_of, temporary)
        .map_err(|e| StepOutcome::failed(&e))?;

    let mut buffer = vec![0u8; options.buffer_size.max(4096)];
    let mut done = 0u64;
    loop {
        if ctx.cancel.is_cancelled() {
            return Err(StepOutcome::Cancelled);
        }
        let read = reader
            .read(&mut buffer)
            .map_err(|e| StepOutcome::failed(&e))?;
        if read == 0 {
            break;
        }
        writer
            .write_all(&buffer[..read])
            .map_err(|e| StepOutcome::failed(&e))?;
        done = done.saturating_add(read as u64);
        (ctx.progress)(Progress::Bytes {
            index: step.index,
            done,
            total,
        });
    }
    writer.flush().map_err(|e| StepOutcome::failed(&e))?;
    writer.sync_data().map_err(|e| StepOutcome::failed(&e))?;
    drop(writer);
    Ok(done)
}

/// What a finished copy must match, read from the source before the
/// destination is touched.
enum Expectation {
    /// Nothing is checked.
    Trusted,
    /// The destination must have this many bytes.
    Size(u64),
    /// The destination must hash to this value.
    Hash(blake3::Hash),
}

fn expected_content(
    fs: &dyn FileOps,
    source: &Path,
    options: &OperationOptions,
) -> io::Result<Expectation> {
    match options.verify {
        Verify::None => Ok(Expectation::Trusted),
        Verify::Size => Ok(Expectation::Size(fs.probe(source)?.size)),
        Verify::Hash => Ok(Expectation::Hash(hash_through(
            fs,
            source,
            options.buffer_size,
        )?)),
    }
}

fn check_content(
    fs: &dyn FileOps,
    written: &Path,
    expected: &Expectation,
    options: &OperationOptions,
) -> io::Result<()> {
    match expected {
        Expectation::Trusted => Ok(()),
        Expectation::Size(expected) => {
            let found = fs.probe(written)?.size;
            if found == *expected {
                Ok(())
            } else {
                Err(io::Error::other(format!(
                    "copy is {found} bytes where the source is {expected}"
                )))
            }
        }
        Expectation::Hash(expected) => {
            if hash_through(fs, written, options.buffer_size)? == *expected {
                Ok(())
            } else {
                Err(io::Error::other("copy does not hash to the source's value"))
            }
        }
    }
}

fn hash_through(fs: &dyn FileOps, path: &Path, buffer_size: usize) -> io::Result<blake3::Hash> {
    let mut reader = fs.open_read(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0u8; buffer_size.max(4096)];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize())
}

/// True when `name` is one of the temporary files a copy writes.
///
/// Recovery uses this to tell a partially written destination apart from a
/// file the user put there.
#[must_use]
pub fn is_temporary_name(name: &str) -> bool {
    name.ends_with(TEMP_SUFFIX)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{is_temporary_name, temporary_path, StepOutcome};

    #[test]
    fn a_temporary_sits_beside_its_target() {
        let target = std::path::Path::new("/base/dir/file.txt");
        let temporary = temporary_path(target);
        assert_eq!(temporary.parent(), target.parent());
        assert!(is_temporary_name(&temporary.to_string_lossy()));
    }

    #[test]
    fn two_temporaries_never_share_a_name() {
        let target = std::path::Path::new("/base/dir/file.txt");
        assert_ne!(temporary_path(target), temporary_path(target));
    }

    #[test]
    fn only_done_counts_as_done() {
        assert!(StepOutcome::Done.is_done());
        assert!(!StepOutcome::Cancelled.is_done());
        assert!(!StepOutcome::skipped("x").is_done());
    }
}
