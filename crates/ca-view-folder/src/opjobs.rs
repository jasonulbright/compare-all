//! Background work for the file operations: planning, execution, recovery and
//! the targeted rescan that follows a batch.
//!
//! Planning over a large selection, execution, journal recovery and rescanning
//! all run here. The frame thread only ever drains messages and reads a shared
//! progress record, so none of these ever hold a frame.

use crate::operations::{Operation, Request};
use crate::selection::Scope;
use crate::tree::Arena;
use ca_fs::{
    execute, plan_attributes, plan_copy, plan_delete, plan_exchange, plan_move, plan_new_folder,
    plan_rename, plan_sync, plan_to_folder, plan_touch, recover_all, retire, sweep, Bases,
    Conflict, ConflictDecision, Decision, Drift, ErrorPolicy, ExcludeMasks, ExecutionContext,
    ExecutionReport, Journal, Journaling, Node, OperationOptions, OperationPlan, PlanStep,
    Progress, RealFs, Side, Sides, SyncPreset, SyncPreview,
};
use ca_fs::{ArchiveTypes, FileOps, Mount, SourceOps};
use ca_ui::worker::{Cancel, Job, Terminal};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

/// What a plan run produced.
#[derive(Debug)]
pub enum PlanOutcome {
    /// Steps ready to be confirmed.
    Plan(Box<OperationPlan>),
    /// Filter masks the Exclude command would add.
    Masks(ExcludeMasks),
    /// The operation cannot be planned, with the reason to show.
    Refused(String),
    /// Stop was given before the plan was handed over. Nothing runs.
    Stopped,
}

/// What the planning worker posts back.
#[derive(Debug)]
pub enum PlanMessage {
    /// The run finished, handing the tree back so the view keeps it.
    Done {
        /// The tree the run borrowed, where the worker still held it.
        tree: Option<Box<Node>>,
        /// What the run produced.
        outcome: PlanOutcome,
    },
    /// The run could not proceed.
    Failed(String),
}

impl Terminal for PlanMessage {
    fn is_terminal(&self) -> bool {
        true
    }

    fn cancelled() -> Self {
        Self::Done {
            tree: None,
            outcome: PlanOutcome::Stopped,
        }
    }

    fn panicked(detail: String) -> Self {
        Self::Failed(detail)
    }
}

/// Plan one operation on a worker.
///
/// The compared tree moves onto the worker and comes back with the answer, so
/// a selection of any size costs no copy and no frame. A run the cancel flag
/// reached answers [`PlanOutcome::Stopped`] and still hands the tree back.
#[must_use]
pub fn spawn_plan(
    tree: Box<Node>,
    request: Request,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<PlanMessage> {
    Job::spawn_notifying(
        move |emitter, cancel| {
            let tree = tree;
            let outcome = unless_stopped(cancel, || build(&tree, &request));
            emitter.send(PlanMessage::Done {
                tree: Some(tree),
                outcome,
            });
        },
        notify,
    )
}

/// What `plan` produces, or [`PlanOutcome::Stopped`] when the flag is raised
/// before it starts or before its answer is handed over.
fn unless_stopped(cancel: &Cancel, plan: impl FnOnce() -> PlanOutcome) -> PlanOutcome {
    if cancel.is_cancelled() {
        return PlanOutcome::Stopped;
    }
    let outcome = plan();
    if cancel.is_cancelled() {
        return PlanOutcome::Stopped;
    }
    outcome
}

/// Turn one request into steps.
#[allow(clippy::too_many_lines)]
fn build(tree: &Node, request: &Request) -> PlanOutcome {
    let bases = Bases {
        left: &request.left_base,
        right: &request.right_base,
    };
    let include_hidden = request.options.include_hidden;
    let left = ca_fs::resolve_selection_with(tree, &request.left, include_hidden);
    let right = ca_fs::resolve_selection_with(tree, &request.right, include_hidden);
    let chosen = |side: Side| if side == Side::Left { &left } else { &right };

    match request.operation {
        Operation::CopyToOtherSide => {
            let selection = chosen(request.from);
            if selection.is_empty() {
                return refuse_empty();
            }
            PlanOutcome::Plan(Box::new(plan_copy(
                selection,
                request.from,
                bases,
                &request.options,
            )))
        }
        Operation::MoveToOtherSide => {
            let selection = chosen(request.from);
            if selection.is_empty() {
                return refuse_empty();
            }
            PlanOutcome::Plan(Box::new(plan_move(
                selection,
                request.from,
                bases,
                &request.options,
            )))
        }
        Operation::CopyToFolder | Operation::MoveToFolder => {
            let Some(target) = request.target_folder.as_ref() else {
                return PlanOutcome::Refused("Name a folder to write into".to_string());
            };
            if !request.left.is_empty() && !request.right.is_empty() {
                return PlanOutcome::Refused(
                    "This command reads one side's selection, not both".to_string(),
                );
            }
            let selection = chosen(request.from);
            if selection.is_empty() {
                return refuse_empty();
            }
            PlanOutcome::Plan(Box::new(plan_to_folder(
                selection,
                request.from,
                bases,
                target,
                request.path_option,
                &request.options,
                request.operation == Operation::MoveToFolder,
                &RealFs,
            )))
        }
        Operation::Delete => per_side(request, &left, &right, |selection, sides| {
            plan_delete(selection, sides, bases, &request.options)
        }),
        Operation::Touch => per_side(request, &left, &right, |selection, sides| {
            plan_touch(selection, sides, bases, request.touch, &request.options)
        }),
        Operation::Attributes => {
            if request.attributes.is_empty() {
                return PlanOutcome::Refused("No attribute is being changed".to_string());
            }
            per_side(request, &left, &right, |selection, sides| {
                plan_attributes(
                    selection,
                    sides,
                    bases,
                    request.attributes,
                    &request.options,
                )
            })
        }
        Operation::Rename => {
            let mut merged = OperationPlan::new(
                ca_fs::OperationKind::Rename,
                vec![request.left_base.clone(), request.right_base.clone()],
                request.options.clone(),
            );
            for (selection, sides) in [(&left, Sides::Left), (&right, Sides::Right)] {
                if selection.is_empty() {
                    continue;
                }
                match plan_rename(
                    tree,
                    selection,
                    sides,
                    bases,
                    &request.rename,
                    &request.options,
                ) {
                    Ok(plan) => merged = merge(merged, plan),
                    Err(error) => return PlanOutcome::Refused(error.to_string()),
                }
            }
            if merged.steps.is_empty() {
                return PlanOutcome::Refused(left_alone(
                    &merged.skipped,
                    "No selected name changes under this mask",
                ));
            }
            PlanOutcome::Plan(Box::new(merged))
        }
        Operation::NewFolder => {
            if request.new_folder_name.trim().is_empty() {
                return PlanOutcome::Refused("Name the folder".to_string());
            }
            let plan = plan_new_folder(
                tree,
                &request.new_folder_parent,
                request.new_folder_name.trim(),
                sides_of(request.scope),
                bases,
                &request.options,
            );
            if plan.steps.is_empty() {
                return PlanOutcome::Refused(left_alone(&plan.skipped, "No folder is created"));
            }
            PlanOutcome::Plan(Box::new(plan))
        }
        Operation::Exchange => {
            if left.is_empty() && right.is_empty() {
                return refuse_empty();
            }
            PlanOutcome::Plan(Box::new(plan_exchange(
                &left,
                &right,
                bases,
                &request.options,
            )))
        }
        Operation::Exclude => {
            let mut both: Vec<&Node> = left;
            for node in right {
                if !both.iter().any(|other| other.rel == node.rel) {
                    both.push(node);
                }
            }
            if both.is_empty() {
                return refuse_empty();
            }
            PlanOutcome::Masks(ca_fs::exclude_masks(&both, true))
        }
    }
}

fn refuse_empty() -> PlanOutcome {
    PlanOutcome::Refused("Nothing is selected".to_string())
}

fn left_alone(skipped: &[ca_fs::PlanSkip], nothing: &str) -> String {
    match skipped {
        [] => nothing.to_string(),
        [only] => format!("{}: {}", only.path.display(), only.reason),
        [first, rest @ ..] => format!(
            "{}: {}\n{} more items are left alone",
            first.path.display(),
            first.reason,
            rest.len()
        ),
    }
}

/// Run one planner once per side that holds a selection and join the results.
fn per_side(
    request: &Request,
    left: &[&Node],
    right: &[&Node],
    mut planner: impl FnMut(&[&Node], Sides) -> OperationPlan,
) -> PlanOutcome {
    let mut merged = OperationPlan::new(
        ca_fs::OperationKind::Delete,
        vec![request.left_base.clone(), request.right_base.clone()],
        request.options.clone(),
    );
    let mut kind = None;
    for (selection, sides) in [(left, Sides::Left), (right, Sides::Right)] {
        if selection.is_empty() {
            continue;
        }
        let plan = planner(selection, sides);
        kind = Some(plan.kind);
        merged = merge(merged, plan);
    }
    match kind {
        Some(kind) => {
            merged.kind = kind;
            PlanOutcome::Plan(Box::new(merged))
        }
        None => refuse_empty(),
    }
}

/// Append one plan's steps to another, renumbering as they land.
fn merge(mut into: OperationPlan, other: OperationPlan) -> OperationPlan {
    for mut step in other.steps {
        step.index = into.steps.len();
        into.steps.push(step);
    }
    into.skipped.extend(other.skipped);
    into
}

/// The engine's form of a scope.
#[must_use]
pub const fn sides_of(scope: Scope) -> Sides {
    match scope {
        Scope::Left => Sides::Left,
        Scope::Right => Sides::Right,
        Scope::Both => Sides::Both,
    }
}

/// Plan a synchronisation on a worker.
#[must_use]
pub fn spawn_sync_plan(
    tree: Box<Node>,
    preset: SyncPreset,
    preview: SyncPreview,
    left_base: PathBuf,
    right_base: PathBuf,
    options: ca_fs::OperationOptions,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<PlanMessage> {
    Job::spawn_notifying(
        move |emitter, cancel| {
            let tree = tree;
            let bases = Bases {
                left: &left_base,
                right: &right_base,
            };
            let outcome = unless_stopped(cancel, || {
                match plan_sync(&tree, &preset, bases, &options, &preview) {
                    Ok(plan) => PlanOutcome::Plan(Box::new(plan)),
                    Err(refused) => PlanOutcome::Refused(refused.to_string()),
                }
            });
            emitter.send(PlanMessage::Done {
                tree: Some(tree),
                outcome,
            });
        },
        notify,
    )
}

/// What the preview worker posts back.
#[derive(Debug)]
pub enum PreviewMessage {
    /// The preview, with the tree it was built from.
    Done {
        /// The tree the run borrowed.
        tree: Option<Box<Node>>,
        /// One row per pair the method acts on.
        preview: Box<SyncPreview>,
    },
    /// The preview could not be built.
    Failed(String),
}

impl Terminal for PreviewMessage {
    fn is_terminal(&self) -> bool {
        true
    }

    fn cancelled() -> Self {
        Self::Failed("the preview stopped before it finished".to_string())
    }

    fn panicked(detail: String) -> Self {
        Self::Failed(detail)
    }
}

/// Build a sync preview on a worker.
///
/// The preview is a pass over the whole comparison, so it never runs on a
/// frame.
#[must_use]
pub fn spawn_preview(
    tree: Box<Node>,
    preset: SyncPreset,
    overrides: std::collections::BTreeMap<PathBuf, ca_fs::SyncAction>,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<PreviewMessage> {
    Job::spawn_notifying(
        move |emitter, _cancel| {
            let tree = tree;
            let mut preview = ca_fs::preview(&tree, &preset);
            preview.overrides = overrides;
            emitter.send(PreviewMessage::Done {
                tree: Some(tree),
                preview: Box::new(preview),
            });
        },
        notify,
    )
}

/// One question execution puts to the user, and waits on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Question {
    /// Recycling was unavailable; removal requires separate consent.
    RecycleUnavailable {
        /// Position of the step in the plan.
        index: usize,
        /// Item that would be removed permanently.
        path: PathBuf,
    },
    /// The plan flagged a step and the answer decides whether it runs.
    Conflict {
        /// Position of the step in the plan.
        index: usize,
        /// Path the step writes.
        path: PathBuf,
        /// What the plan flagged.
        conflicts: Vec<Conflict>,
    },
    /// The disk moved on between the confirmation and the step.
    Drifted {
        /// Position of the step in the plan.
        index: usize,
        /// What moved on, in words.
        detail: String,
    },
    /// The step failed and the answer decides what happens next.
    Failure {
        /// Position of the step in the plan.
        index: usize,
        /// Path the step writes.
        path: PathBuf,
        /// What the file system said.
        message: String,
        /// How many times the step has been attempted.
        attempt: u32,
    },
}

/// What the user answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// Run this step.
    Proceed,
    /// Run this step and every later one like it without asking again.
    ProceedAll,
    /// Leave this step undone.
    Skip,
    /// Leave this step and every later one like it undone without asking.
    SkipAll,
    /// Attempt the step again.
    Retry,
    /// Stop the batch.
    Abort,
}

/// One publication of a question. Even a repeated question needs a new reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingQuestion {
    /// Identity of this publication within the batch.
    pub sequence: u64,
    /// The item and decision shown to the user.
    pub question: Question,
}

/// The standing answers a "do this every time" reply leaves behind.
#[derive(Debug, Clone, Copy, Default)]
struct Standing {
    conflicts: Option<bool>,
    drift: Option<bool>,
    failures: Option<bool>,
}

/// The pending question and its answer, shared by the worker and the frame.
#[derive(Debug, Default)]
struct AskState {
    pending: Option<PendingQuestion>,
    sequence: u64,
    answer: Option<Answer>,
    standing: Standing,
}

/// The channel a worker asks a question down and waits on.
///
/// The worker parks on the condition variable; the frame thread only ever
/// reads the pending question and writes an answer, so it never waits.
pub struct Ask {
    state: Mutex<AskState>,
    changed: Condvar,
    notify: Arc<dyn Fn() + Send + Sync>,
    cancel: Cancel,
}

impl std::fmt::Debug for Ask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ask").finish_non_exhaustive()
    }
}

impl Ask {
    /// A channel that asks for a repaint whenever it raises a question.
    #[must_use]
    pub fn new(notify: Arc<dyn Fn() + Send + Sync>, cancel: Cancel) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(AskState::default()),
            changed: Condvar::new(),
            notify,
            cancel,
        })
    }

    /// The question waiting for an answer, if there is one.
    #[must_use]
    pub fn pending(&self) -> Option<PendingQuestion> {
        let state = self.state.lock().ok()?;
        if state.answer.is_some() {
            return None;
        }
        state.pending.clone()
    }

    /// Answer only the publication shown by the frame, once.
    pub fn answer(&self, shown: &PendingQuestion, answer: Answer) {
        if let Ok(mut state) = self.state.lock() {
            if state.pending.as_ref() != Some(shown) || state.answer.is_some() {
                return;
            }
            if matches!(answer, Answer::ProceedAll | Answer::SkipAll) {
                let allow = answer == Answer::ProceedAll;
                match &shown.question {
                    Question::Conflict { .. } => state.standing.conflicts = Some(allow),
                    Question::Drifted { .. } => state.standing.drift = Some(allow),
                    Question::Failure { .. } => state.standing.failures = Some(allow),
                    Question::RecycleUnavailable { .. } => {}
                }
            }
            state.answer = Some(answer);
        }
        self.changed.notify_all();
    }

    /// Put a question and wait for its answer.
    ///
    /// A raised cancel flag answers for the user, because a cancelled batch has
    /// nobody left to ask.
    fn put(&self, question: Question) -> Answer {
        let standing = {
            let Ok(mut state) = self.state.lock() else {
                return Answer::Abort;
            };
            let standing = match &question {
                Question::Conflict { .. } => state.standing.conflicts,
                Question::Drifted { .. } => state.standing.drift,
                Question::Failure { .. } => state.standing.failures,
                Question::RecycleUnavailable { .. } => None,
            };
            if standing.is_none() {
                state.sequence = state.sequence.wrapping_add(1);
                state.pending = Some(PendingQuestion {
                    sequence: state.sequence,
                    question,
                });
                state.answer = None;
            }
            standing
        };
        if let Some(allow) = standing {
            return if allow { Answer::Proceed } else { Answer::Skip };
        }
        (self.notify)();

        let Ok(mut state) = self.state.lock() else {
            return Answer::Abort;
        };
        while state.answer.is_none() {
            if self.cancel.is_cancelled() {
                state.pending = None;
                return Answer::Abort;
            }
            let waited = self
                .changed
                .wait_timeout(state, std::time::Duration::from_millis(100));
            match waited {
                Ok((next, _)) => state = next,
                Err(_) => return Answer::Abort,
            }
        }
        let mut answer = state.answer.take().unwrap_or(Answer::Abort);
        if matches!(
            state.pending.as_ref().map(|shown| &shown.question),
            Some(Question::RecycleUnavailable { .. })
        ) {
            // A broad or unrelated response must never authorize permanent
            // removal. The dialog only offers explicit per-item consent.
            answer = match answer {
                Answer::Proceed | Answer::Skip | Answer::Abort => answer,
                _ => Answer::Skip,
            };
        }
        state.pending = None;
        answer
    }
}

/// The error policy that asks the user and waits for the answer.
struct AskingPolicy {
    ask: Arc<Ask>,
    /// What the batch confirmation already settled, so it is not asked again.
    approved: Vec<Conflict>,
}

/// The conflicts the batch confirmation dialog answers for the whole batch.
///
/// The per-item prompt covers only what that dialog left open. A target that
/// merely exists is one the user agreed to replace by confirming with the
/// replace option set, so replacing it raises no second question. Every other
/// conflict, including an older item replacing a newer one, a read-only target
/// and a counterpart that could not be read, is still put to the user.
fn approved_conflicts(options: &OperationOptions) -> Vec<Conflict> {
    if options.overwrite {
        vec![Conflict::TargetExists]
    } else {
        Vec::new()
    }
}

fn decide(answer: Answer) -> ConflictDecision {
    match answer {
        Answer::Proceed | Answer::ProceedAll | Answer::Retry => ConflictDecision::Proceed,
        Answer::Skip | Answer::SkipAll => ConflictDecision::Skip,
        Answer::Abort => ConflictDecision::Abort,
    }
}

impl ErrorPolicy for AskingPolicy {
    fn on_recycle_bin_unavailable(&self, step: &PlanStep) -> ConflictDecision {
        decide(self.ask.put(Question::RecycleUnavailable {
            index: step.index,
            path: step.action.target().to_path_buf(),
        }))
    }

    fn on_error(&self, step: &PlanStep, attempt: u32, message: &str) -> Decision {
        let answer = self.ask.put(Question::Failure {
            index: step.index,
            path: step.action.target().to_path_buf(),
            message: message.to_string(),
            attempt,
        });
        match answer {
            Answer::Retry => Decision::Retry,
            Answer::Abort => Decision::Abort,
            _ => Decision::Skip,
        }
    }

    fn on_conflict(&self, step: &PlanStep) -> ConflictDecision {
        if step
            .conflicts
            .iter()
            .all(|conflict| self.approved.contains(conflict))
        {
            return ConflictDecision::Proceed;
        }
        decide(self.ask.put(Question::Conflict {
            index: step.index,
            path: step.action.target().to_path_buf(),
            conflicts: step.conflicts.clone(),
        }))
    }

    fn on_drift(&self, step: &PlanStep, drift: &Drift) -> ConflictDecision {
        // Nobody approved what the disk now holds, so an unanswered drift is
        // left alone rather than carried out.
        decide(self.ask.put(Question::Drifted {
            index: step.index,
            detail: drift.to_string(),
        }))
    }
}

/// What a running batch has done so far.
#[derive(Debug, Clone, Default)]
pub struct ProgressState {
    /// Steps started.
    pub steps_started: usize,
    /// Steps finished.
    pub steps_done: usize,
    /// Steps the plan holds.
    pub steps_total: usize,
    /// Bytes written by finished steps, plus the current step's progress.
    pub bytes_done: u64,
    /// Bytes the plan moves.
    pub bytes_total: u64,
    /// Path the current step writes.
    pub current: PathBuf,
}

/// What the execution worker posts back.
#[derive(Debug)]
pub enum ExecMessage {
    /// The batch finished, however it ended.
    Done(Box<ExecutionReport>),
    /// The batch could not run.
    Failed(String),
}

impl Terminal for ExecMessage {
    fn is_terminal(&self) -> bool {
        true
    }

    fn cancelled() -> Self {
        Self::Done(Box::new(ExecutionReport {
            cancelled: true,
            ..ExecutionReport::default()
        }))
    }

    fn panicked(detail: String) -> Self {
        Self::Failed(detail)
    }
}

/// Run a plan on a worker, journalling every step.
///
/// The journal directory is required rather than optional: a batch with no
/// record leaves nothing that names the step that was in flight.
#[must_use]
pub fn spawn_execute(
    plan: Box<OperationPlan>,
    journal_directory: PathBuf,
    ask: Arc<Ask>,
    progress: Arc<Mutex<ProgressState>>,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<ExecMessage> {
    spawn_execute_reading(
        plan,
        journal_directory,
        ask,
        progress,
        notify,
        Containers::default(),
    )
}

/// Container files a plan reads from, and the masks that open them.
#[derive(Debug, Clone, Default)]
pub struct Containers {
    /// Each container file. A path that is not a file is left to the local
    /// disk.
    pub paths: Vec<PathBuf>,
    /// The masks and handling the containers open under.
    pub types: ArchiveTypes,
}

/// [`spawn_execute`] for a plan whose sources may sit inside `containers`.
///
/// A path under a container file is read from inside the container. A
/// container that does not open fails the whole batch before any step runs.
#[must_use]
pub fn spawn_execute_reading(
    plan: Box<OperationPlan>,
    journal_directory: PathBuf,
    ask: Arc<Ask>,
    progress: Arc<Mutex<ProgressState>>,
    notify: Arc<dyn Fn() + Send + Sync>,
    containers: Containers,
) -> Job<ExecMessage> {
    let repaint = Arc::clone(&notify);
    Job::spawn_notifying(
        move |emitter, cancel| {
            let mut mounts = Vec::new();
            for path in containers.paths.iter().filter(|path| path.is_file()) {
                match crate::jobs::open_side(path, &containers.types) {
                    Ok(source) => mounts.push(Mount::of_archive(source)),
                    Err(reason) => {
                        emitter.send(ExecMessage::Failed(format!(
                            "{} could not be opened, so nothing ran: {reason}",
                            path.display()
                        )));
                        return;
                    }
                }
            }
            let routed = SourceOps::new(mounts);
            let file_ops: &dyn FileOps = if containers.paths.is_empty() {
                &RealFs
            } else {
                &routed
            };
            let journal = match Journal::create_in(&journal_directory) {
                Ok(journal) => journal,
                Err(error) => {
                    emitter.send(ExecMessage::Failed(format!(
                        "no journal could be opened, so nothing ran: {error}"
                    )));
                    return;
                }
            };
            if let Ok(mut state) = progress.lock() {
                state.steps_total = plan.steps.len();
                state.bytes_total = plan.total_bytes();
            }
            let policy = AskingPolicy {
                ask: Arc::clone(&ask),
                approved: approved_conflicts(&plan.options),
            };
            let report = {
                let observer = |event: Progress<'_>| {
                    let Ok(mut state) = progress.lock() else {
                        return;
                    };
                    match event {
                        Progress::StepStarted { step } => {
                            state.steps_started += 1;
                            state.current = step.action.target().to_path_buf();
                        }
                        Progress::Bytes { .. } => {}
                        Progress::StepFinished { .. } => state.steps_done += 1,
                    }
                    drop(state);
                    repaint();
                };
                let context =
                    ExecutionContext::new(file_ops, cancel.as_fs(), Journaling::To(&journal))
                        .with_policy(&policy)
                        .with_progress(&observer);
                execute(&plan, &context)
            };
            if let Ok(mut state) = progress.lock() {
                state.bytes_done = report.bytes_copied;
            }
            // The journal is closed before it is read back, so the record the
            // check reads is the one the batch finished writing.
            let journal_path = journal.path().to_path_buf();
            drop(journal);
            let _ = retire(&journal_path);
            emitter.send(ExecMessage::Done(Box::new(report)));
        },
        notify,
    )
}

/// One unfinished batch a journal recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryNotice {
    /// The journal file.
    pub journal: PathBuf,
    /// Steps that began and never reported an end.
    pub unfinished: usize,
    /// Temporary files the batch may have left behind.
    pub temporaries: Vec<PathBuf>,
    /// Backups the batch took.
    pub backups: Vec<PathBuf>,
    /// True when the batch named at least one path and none of them are on
    /// disk any more.
    pub files_missing: bool,
    /// The first path the batch names, which says what it was working on.
    pub first_path: Option<PathBuf>,
    /// Why the batch is reported, in words.
    pub summary: String,
}

/// What the recovery worker posts back.
#[derive(Debug)]
pub enum RecoverMessage {
    /// Every unfinished batch found, with the machine's zone offset.
    Done {
        /// Unfinished batches, oldest first.
        notices: Vec<RecoveryNotice>,
        /// Seconds the machine's clock stands ahead of universal time.
        offset_seconds: i32,
    },
    /// The journals could not be read.
    Failed(String),
}

impl Terminal for RecoverMessage {
    fn is_terminal(&self) -> bool {
        true
    }

    fn cancelled() -> Self {
        Self::Done {
            notices: Vec::new(),
            offset_seconds: 0,
        }
    }

    fn panicked(detail: String) -> Self {
        Self::Failed(detail)
    }
}

/// Read the journal directory on a worker and report unfinished batches.
#[must_use]
pub fn spawn_recovery(
    journal_directory: PathBuf,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<RecoverMessage> {
    Job::spawn_notifying(
        move |emitter, _| {
            let offset_seconds = ca_ui::format::local_offset_seconds();
            // Records nothing is waiting on go first, so the check reports only
            // batches the user still has a choice about.
            let _ = sweep(&journal_directory, ca_fs::KEEP_TROUBLED_DAYS);
            match recover_all(&journal_directory) {
                Ok(found) => {
                    let notices = found
                        .into_iter()
                        .map(|(journal, recovery)| {
                            let files_missing = batch_files_missing(&recovery);
                            RecoveryNotice {
                                journal,
                                unfinished: recovery.unfinished.len(),
                                temporaries: recovery.leftover_temporaries.clone(),
                                backups: recovery.backups.clone(),
                                files_missing,
                                first_path: batch_paths(&recovery).into_iter().next(),
                                summary: summarize(&recovery, files_missing),
                            }
                        })
                        .collect();
                    emitter.send(RecoverMessage::Done {
                        notices,
                        offset_seconds,
                    });
                }
                Err(error) => {
                    emitter.send(RecoverMessage::Failed(error.to_string()));
                }
            }
        },
        notify,
    )
}

/// True when the batch named paths and none of them are on disk.
///
/// A batch whose folders were removed after the interruption still has to be
/// listed, because the record is the only account of what it was doing, but the
/// choice about it is only informed once this is known.
#[must_use]
pub fn batch_files_missing(recovery: &ca_fs::Recovery) -> bool {
    let mut named = false;
    for path in batch_paths(recovery) {
        named = true;
        if path.exists() {
            return false;
        }
    }
    named
}

/// Every path a journal names, in the order the records hold them.
fn batch_paths(recovery: &ca_fs::Recovery) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for record in &recovery.records {
        if let ca_fs::JournalRecord::StepBegin { action, .. } = record {
            paths.push(action.target().to_path_buf());
            if let Some(source) = action.source() {
                paths.push(source.to_path_buf());
            }
        }
    }
    paths.extend(recovery.leftover_temporaries.iter().cloned());
    paths.extend(recovery.backups.iter().cloned());
    paths
}

fn summarize(recovery: &ca_fs::Recovery, files_missing: bool) -> String {
    let mut parts = Vec::new();
    if !recovery.completed {
        parts.push("the batch never reported an end".to_string());
    }
    if recovery.cancelled {
        parts.push("the batch was cancelled".to_string());
    }
    if recovery.aborted {
        parts.push("the batch was stopped by an error".to_string());
    }
    if !recovery.unfinished.is_empty() {
        parts.push(format!("{} steps stayed open", recovery.unfinished.len()));
    }
    if parts.is_empty() {
        parts.push("the batch left files behind".to_string());
    }
    if files_missing {
        parts.push(MISSING_FILES.to_string());
    }
    parts.join("; ")
}

/// What a batch whose paths are all gone reports.
pub const MISSING_FILES: &str = "the files of this batch no longer exist";

/// Remove the temporary files one recovery names, on a worker.
#[must_use]
pub fn spawn_cleanup(journal: PathBuf, notify: Arc<dyn Fn() + Send + Sync>) -> Job<CleanupMessage> {
    Job::spawn_notifying(
        move |emitter, _| {
            let outcome = match ca_fs::recover(&journal) {
                Ok(recovery) => {
                    let failures = recovery.clean_up(&RealFs);
                    // The record is only removed once the files it names are
                    // gone, so a failed clean-up is still reported next time.
                    if failures.is_empty() {
                        let _ = std::fs::remove_file(&journal);
                    }
                    CleanupMessage::Done { journal, failures }
                }
                Err(error) => CleanupMessage::Failed(error.to_string()),
            };
            emitter.send(outcome);
        },
        notify,
    )
}

/// What the clean-up worker posts back.
#[derive(Debug)]
pub enum CleanupMessage {
    /// The clean-up ran.
    Done {
        /// The journal that was read.
        journal: PathBuf,
        /// Files that could not be removed, with the reason.
        failures: Vec<(PathBuf, String)>,
    },
    /// The journal could not be read.
    Failed(String),
}

impl Terminal for CleanupMessage {
    fn is_terminal(&self) -> bool {
        true
    }

    fn cancelled() -> Self {
        Self::Failed("the clean-up stopped before it finished".to_string())
    }

    fn panicked(detail: String) -> Self {
        Self::Failed(detail)
    }
}

/// What the targeted rescan posts back.
#[derive(Debug)]
pub enum RescanMessage {
    /// The refreshed comparison.
    Done {
        /// Flattened comparison.
        arena: Box<Arena>,
        /// The tree the refreshed subtrees were spliced into.
        tree: Box<Node>,
    },
    /// The rescan could not run, handing the old tree back.
    Failed {
        /// Why the rescan stopped.
        reason: String,
        /// The tree as it stood.
        tree: Option<Box<Node>>,
    },
}

impl Terminal for RescanMessage {
    fn is_terminal(&self) -> bool {
        true
    }

    fn cancelled() -> Self {
        Self::Failed {
            reason: "the rescan stopped before it finished".to_string(),
            tree: None,
        }
    }

    fn panicked(detail: String) -> Self {
        Self::Failed {
            reason: detail,
            tree: None,
        }
    }
}

/// Re-read only the folders a batch touched and splice them into the tree.
///
/// Rescanning the whole comparison after every operation costs a pass over
/// both sides; the affected folders are known from the plan, so only they are
/// read again.
#[must_use]
pub fn spawn_rescan(
    tree: Box<Node>,
    left_base: PathBuf,
    right_base: PathBuf,
    subroots: Vec<PathBuf>,
    name_filter: String,
    options: Box<crate::settings::EngineOptions>,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<RescanMessage> {
    Job::spawn_notifying(
        move |emitter, cancel| {
            let mut tree = tree;
            for rel in minimal_roots(&subroots) {
                if cancel.is_cancelled() {
                    break;
                }
                let refreshed = scan_subtree(
                    &left_base,
                    &right_base,
                    &rel,
                    &name_filter,
                    &options,
                    cancel,
                );
                splice(&mut tree, &rel, refreshed);
            }
            ca_fs::rollup(&mut tree);
            let arena = Arena::from_root(&tree);
            emitter.send(RescanMessage::Done {
                arena: Box::new(arena),
                tree,
            });
        },
        notify,
    )
}

/// Drop any root that already sits under another root in the set.
#[must_use]
pub fn minimal_roots(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut sorted: Vec<PathBuf> = roots.to_vec();
    sorted.sort();
    sorted.dedup();
    if sorted.iter().any(|root| root.as_os_str().is_empty()) {
        return vec![PathBuf::new()];
    }
    let mut kept: Vec<PathBuf> = Vec::new();
    for candidate in sorted {
        if kept.iter().any(|root| candidate.starts_with(root)) {
            continue;
        }
        kept.push(candidate);
    }
    kept
}

/// Scan and compare one subtree, with paths relative to the base folders.
fn scan_subtree(
    left_base: &Path,
    right_base: &Path,
    rel: &Path,
    name_filter: &str,
    options: &crate::settings::EngineOptions,
    cancel: &Cancel,
) -> Vec<Node> {
    let read = |base: &Path| {
        ca_fs::scan_with(&base.join(rel), &options.scan, cancel.as_fs(), &|_| {})
            .unwrap_or_default()
    };
    let left = read(left_base);
    let right = read(right_base);
    let mut tree = ca_fs::align_trees(&left, &right, &options.alignment, cancel.as_fs());
    let mut others = options.other_filters.clone();
    others.left_root = left_base.join(rel);
    others.right_root = right_base.join(rel);
    ca_fs::apply_filters(
        &mut tree,
        &crate::jobs::name_filters_of(options, name_filter),
        &others,
        &ca_fs::FilterContext::default(),
    );
    ca_fs::compare_quick(&mut tree, &options.compare);
    let mut children = std::mem::take(&mut tree.children);
    for child in &mut children {
        reprefix(child, rel);
    }
    children
}

/// Put `prefix` in front of every relative path of a freshly scanned subtree.
fn reprefix(node: &mut Node, prefix: &Path) {
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        node.rel = prefix.join(&node.rel);
        if let Some(entry) = node.left.as_mut() {
            entry.rel.clone_from(&node.rel);
        }
        if let Some(entry) = node.right.as_mut() {
            entry.rel.clone_from(&node.rel);
        }
        for child in &mut node.children {
            stack.push(child);
        }
    }
}

/// Replace the children of the node at `rel` with a freshly scanned set.
fn splice(tree: &mut Node, rel: &Path, children: Vec<Node>) {
    if rel.as_os_str().is_empty() {
        tree.children = children;
        return;
    }
    if let Some(node) = find_mut(tree, rel) {
        node.children = children;
        return;
    }
    // The folder is new, so it is attached under whichever ancestor the tree
    // does hold; nothing is invented above it.
    if let Some(parent) = rel.parent() {
        if let Some(node) = find_mut(tree, parent) {
            node.children.extend(children);
        } else if parent.as_os_str().is_empty() {
            tree.children.extend(children);
        }
    }
}

fn find_mut<'a>(tree: &'a mut Node, rel: &Path) -> Option<&'a mut Node> {
    let mut stack: Vec<&'a mut Node> = vec![tree];
    while let Some(node) = stack.pop() {
        if node.rel == rel {
            return Some(node);
        }
        if !rel.starts_with(&node.rel) && !node.rel.as_os_str().is_empty() {
            continue;
        }
        for child in &mut node.children {
            stack.push(child);
        }
    }
    None
}

/// The folders a plan's steps touch, as paths relative to the base folders.
#[must_use]
pub fn affected_roots(plan: &OperationPlan) -> Vec<PathBuf> {
    // Both base folders are read again for each root, so one relative parent
    // covers a move's source folder as well as its destination.
    let roots: Vec<PathBuf> = plan
        .steps
        .iter()
        .map(|step| {
            step.rel
                .parent()
                .map_or_else(PathBuf::new, Path::to_path_buf)
        })
        .collect();
    minimal_roots(&roots)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{minimal_roots, Answer, Ask, Question};
    use ca_ui::worker::Cancel;
    use std::path::PathBuf;
    use std::sync::Arc;

    #[test]
    fn a_root_inside_another_root_is_dropped() {
        let roots = vec![
            PathBuf::from("a"),
            PathBuf::from("a").join("b"),
            PathBuf::from("c"),
        ];
        assert_eq!(
            minimal_roots(&roots),
            vec![PathBuf::from("a"), PathBuf::from("c")]
        );
    }

    #[test]
    fn the_base_folder_swallows_every_other_root() {
        let roots = vec![PathBuf::new(), PathBuf::from("a")];
        assert_eq!(minimal_roots(&roots), vec![PathBuf::new()]);
    }

    #[test]
    fn unavailable_recycling_asks_before_each_permanent_removal() {
        use ca_fs::{ConflictDecision, ErrorPolicy, PlanStep, StepAction, StepExpectation};
        for answer in [
            Answer::Proceed,
            Answer::Skip,
            Answer::Abort,
            Answer::ProceedAll,
        ] {
            let (notify, notifications) = std::sync::mpsc::channel();
            let ask = Ask::new(
                Arc::new(move || {
                    let _ = notify.send(());
                }),
                Cancel::new(),
            );
            let worker = Arc::clone(&ask);
            let step = PlanStep {
                index: 7,
                action: StepAction::Trash {
                    path: PathBuf::from("keep.txt"),
                },
                bytes: 0,
                conflicts: Vec::new(),
                backup: None,
                side: None,
                rel: PathBuf::from("keep.txt"),
                expected: StepExpectation::default(),
            };
            let handle = std::thread::spawn(move || {
                let policy = super::AskingPolicy {
                    ask: worker,
                    approved: Vec::new(),
                };
                let first = policy.on_recycle_bin_unavailable(&step);
                let second = policy.on_recycle_bin_unavailable(&step);
                (first, second)
            });
            let raised = ca_ui::testing::wait_until(std::time::Duration::from_secs(5), || {
                notifications.try_recv().is_ok() || handle.is_finished()
            });
            let first_question = ask.pending();
            ask.answer(first_question.as_ref().unwrap(), answer);
            let second_raised =
                ca_ui::testing::wait_until(std::time::Duration::from_secs(5), || {
                    notifications.try_recv().is_ok() || handle.is_finished()
                });
            // Each notification follows publication of a new question, so
            // the old pending question cannot satisfy the second wait.
            let second_question = if second_raised && !handle.is_finished() {
                ask.pending()
            } else {
                None
            };
            ask.answer(second_question.as_ref().unwrap(), Answer::Skip);
            let (first, second) = handle.join().unwrap();
            assert!(raised);
            assert!(
                first_question.is_some(),
                "the deletion was silently skipped"
            );
            assert!(
                second_question.is_some(),
                "permanent consent carried to another item"
            );
            let expected = match answer {
                Answer::Proceed => ConflictDecision::Proceed,
                Answer::Abort => ConflictDecision::Abort,
                _ => ConflictDecision::Skip,
            };
            assert_eq!(first, expected);
            assert_eq!(second, ConflictDecision::Skip);
        }
    }

    /// A mapped parent must not create a child under a different real item,
    /// whether the refused row is the immediate parent or an ancestor.
    #[test]
    #[allow(clippy::panic)]
    fn new_folder_refuses_mapped_parents_and_ancestors() {
        use crate::operations::{Form, Operation, Request};
        use crate::selection::Selection;
        use ca_fs::{align_trees, scan_with, AlignmentOptions, Cancel, ScanOptions};
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        for base in [left.path(), right.path()] {
            std::fs::create_dir_all(base.join("mapped/child")).unwrap();
        }
        let scan =
            |base| scan_with(base, &ScanOptions::default(), &Cancel::new(), &|_| {}).unwrap();
        let mut tree = align_trees(
            &scan(left.path()),
            &scan(right.path()),
            &AlignmentOptions::default(),
            &Cancel::new(),
        );
        let parent = &mut tree.children[0];
        for entry in [parent.left.as_mut(), parent.right.as_mut()]
            .into_iter()
            .flatten()
        {
            entry.refused = true;
            entry.error = Some("The original name was mapped.".to_owned());
        }
        let form = Form {
            new_folder_name: "fresh".to_owned(),
            ..Form::default()
        };
        for parent in ["mapped", "mapped/child"] {
            let request = Request::build(
                Operation::NewFolder,
                &Selection::default(),
                &form,
                left.path().to_path_buf(),
                right.path().to_path_buf(),
                PathBuf::from(parent),
            );
            let super::PlanOutcome::Refused(reason) = super::build(&tree, &request) else {
                panic!("a refused parent produced a writable plan");
            };
            assert!(reason.contains("original name was mapped"));
            for base in [left.path(), right.path()] {
                assert!(!base.join(parent).join("fresh").exists());
            }
        }
    }

    #[test]
    fn a_question_waits_for_its_answer_and_a_standing_answer_stops_asking() {
        let ask = Ask::new(Arc::new(|| {}), Cancel::new());
        let worker = Arc::clone(&ask);
        let handle = std::thread::spawn(move || {
            let first = worker.put(Question::Drifted {
                index: 0,
                detail: "moved".to_string(),
            });
            let second = worker.put(Question::Drifted {
                index: 1,
                detail: "moved again".to_string(),
            });
            (first, second)
        });
        assert!(ca_ui::testing::wait_until(
            std::time::Duration::from_secs(5),
            || ask.pending().is_some()
        ));
        ask.answer(&ask.pending().unwrap(), Answer::SkipAll);
        let (first, second) = handle.join().unwrap();
        assert_eq!(first, Answer::SkipAll);
        // The standing answer is used without a second question being raised.
        assert_eq!(second, Answer::Skip);
        assert!(ask.pending().is_none());
    }

    /// A late click must neither approve another item nor leave a standing
    /// answer. A duplicate response must not replace the first response.
    #[test]
    fn answers_are_bound_to_the_shown_publication_and_accepted_once() {
        let ask = Ask::new(Arc::new(|| {}), Cancel::new());
        let old = super::PendingQuestion {
            sequence: 1,
            question: Question::RecycleUnavailable {
                index: 0,
                path: PathBuf::from("first.txt"),
            },
        };
        let current = super::PendingQuestion {
            sequence: 2,
            question: Question::RecycleUnavailable {
                index: 1,
                path: PathBuf::from("second.txt"),
            },
        };
        ask.state.lock().unwrap().pending = Some(current.clone());
        ask.answer(&old, Answer::Proceed);
        assert_eq!(ask.state.lock().unwrap().answer, None);
        ask.answer(&current, Answer::Skip);
        assert!(
            ask.pending().is_none(),
            "an answered question is not displayed"
        );
        ask.answer(&current, Answer::Proceed);
        assert_eq!(ask.state.lock().unwrap().answer, Some(Answer::Skip));

        let repeated = super::PendingQuestion {
            sequence: 3,
            question: old.question.clone(),
        };
        {
            let mut state = ask.state.lock().unwrap();
            state.pending = Some(repeated.clone());
            state.answer = None;
        }
        ask.answer(&old, Answer::SkipAll);
        let state = ask.state.lock().unwrap();
        assert_eq!(state.answer, None);
        assert_eq!(state.standing.conflicts, None);
        assert_eq!(state.standing.drift, None);
        assert_eq!(state.standing.failures, None);
    }

    #[test]
    fn a_cancelled_batch_answers_its_own_question() {
        let cancel = Cancel::new();
        let ask = Ask::new(Arc::new(|| {}), cancel.clone());
        let worker = Arc::clone(&ask);
        let handle = std::thread::spawn(move || {
            worker.put(Question::Drifted {
                index: 0,
                detail: "moved".to_string(),
            })
        });
        assert!(ca_ui::testing::wait_until(
            std::time::Duration::from_secs(5),
            || ask.pending().is_some()
        ));
        cancel.cancel();
        assert_eq!(handle.join().unwrap(), Answer::Abort);
    }
}
