//! Runs the commands of a script against a session.
//!
//! Every command that writes to disk builds a `ca_fs` plan and runs it through
//! the journalling executor. In dry run mode the plan is built and reported and
//! nothing is written.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use ca_fs::{
    AbortOnError, Bases, Cancel, ConflictDecision, ContinueOnError, Decision, ErrorPolicy,
    ExecutionContext, Journal, Journaling, Node, OperationOptions, OperationPlan, PlanStep, RealFs,
    Side, Sides, SyncPreset,
};

use crate::ast::{
    AttrSet, AttribChange, Command, CompareType, ConfirmMode, ContentCriterion, Direction,
    LoadSpec, PathOption, RenameSpec, ReportKind, Script, SideArg, SnapshotSource, SnapshotSpec,
    Span, SyncDirection, SyncMode, SyncSpec, TouchSpec, TouchValue,
};
use crate::clock;
use crate::error::{ExecError, RunError};
use crate::report;
use crate::rules::TextRules;
use crate::state::Session;
use crate::subst::Substitution;
use crate::text;

/// What one command did.
#[derive(Debug, Clone)]
pub struct StepReport {
    /// Where the command sat in the source.
    pub span: Span,
    /// The command as it was written back out, or the command word.
    pub command: String,
    /// The failure, when the command failed.
    pub error: Option<String>,
}

/// What a whole run did.
#[derive(Debug, Clone, Default)]
pub struct Outcome {
    /// One entry per command that ran.
    pub steps: Vec<StepReport>,
    /// Commands that failed.
    pub failures: usize,
    /// True when the run stopped before the last command.
    pub stopped: bool,
    /// Commands whose engine is not built yet, by command word.
    pub not_supported: Vec<String>,
}

impl Outcome {
    /// True when every command ran without a failure.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.failures == 0 && !self.stopped
    }
}

/// Parse and run script text.
///
/// # Errors
/// Returns [`RunError::Syntax`] when the text does not parse and
/// [`RunError::LoadFailed`] when a `load` command cannot open what it names.
pub fn run_script(
    source: &str,
    subst: &Substitution,
    session: &mut Session,
) -> Result<Outcome, RunError> {
    let script = crate::parse::parse_with(source, subst)?;
    run(&script, session)
}

/// Run a parsed script.
///
/// A failing command is recorded and the run carries on, unless
/// `option stop-on-error` has been given. A `load` that cannot open what it
/// names always ends the run.
///
/// # Errors
/// Returns [`RunError::LoadFailed`] when a `load` command fails and
/// [`RunError::Stopped`] when a failure ends the run.
pub fn run(script: &Script, session: &mut Session) -> Result<Outcome, RunError> {
    let mut outcome = Outcome::default();
    for statement in &script.statements {
        let written = text::encode(&statement.command)
            .unwrap_or_else(|_| statement.command.word().to_string());
        session.log.command(&written);
        let result = step(session, &statement.command);
        let mut report = StepReport {
            span: statement.span,
            command: written.clone(),
            error: None,
        };
        match result {
            Ok(()) => outcome.steps.push(report),
            Err(error) => {
                let message = error.to_string();
                session.log.error(&message);
                if error.is_not_supported() {
                    outcome
                        .not_supported
                        .push(statement.command.word().to_string());
                }
                report.error = Some(message.clone());
                outcome.steps.push(report);
                outcome.failures += 1;
                if matches!(statement.command, Command::Load(_)) {
                    outcome.stopped = true;
                    return Err(RunError::LoadFailed(message));
                }
                if session.options.stop_on_error {
                    outcome.stopped = true;
                    return Err(RunError::Stopped(message));
                }
            }
        }
    }
    Ok(outcome)
}

/// Run one command.
///
/// # Errors
/// Returns the failure the command raised.
#[allow(clippy::too_many_lines)]
pub fn step(session: &mut Session, command: &Command) -> Result<(), ExecError> {
    match command {
        Command::Beep => {
            beep();
            Ok(())
        }
        Command::Option(spec) => {
            match spec {
                crate::ast::OptionSpec::StopOnError => session.options.stop_on_error = true,
                crate::ast::OptionSpec::Confirm(mode) => session.options.confirm = *mode,
            }
            Ok(())
        }
        Command::Log(spec) => {
            if session.options.dry_run {
                println!("would configure log: {spec:?}");
                return Ok(());
            }
            let working = session.options.working_directory.clone();
            let would_write =
                spec.level.unwrap_or_else(|| session.log.level()) != crate::ast::LogLevel::None;
            let already_open = spec.target.is_none() && session.log.path().is_some();
            if would_write && !already_open {
                let name = spec.target.as_ref().map_or_else(
                    || PathBuf::from(crate::log::DEFAULT_LOG_NAME),
                    |target| PathBuf::from(&target.file),
                );
                let target = if name.is_absolute() {
                    name
                } else {
                    working.join(name)
                };
                session.check_path_write(&target, "log")?;
            }
            let offset = session.options.offset_seconds;
            session.log.set_offset_seconds(offset);
            session
                .log
                .configure(spec, &working)
                .map_err(|source| ExecError::Io {
                    context: "cannot open the log file".to_string(),
                    source,
                })
        }
        Command::Load(spec) => load(session, spec),
        Command::Expand(paths) => {
            session.set_expanded(paths, true);
            Ok(())
        }
        Command::Collapse(paths) => {
            session.set_expanded(paths, false);
            Ok(())
        }
        Command::Select(masks) => {
            session.select(masks);
            let count = session.selection.len();
            session.log.note(&format!("{count} items selected"));
            Ok(())
        }
        Command::Filter(clauses) => {
            session.set_filters(clauses)?;
            if session.left.is_some() && session.right.is_some() {
                session.rescan()?;
            }
            Ok(())
        }
        Command::Criteria(criteria) => {
            session.criteria = *criteria;
            session.recompare();
            Ok(())
        }
        Command::Compare(kind) => compare(session, *kind),
        Command::Copy(direction) => {
            session.require_writable_sides("copy")?;
            transfer(session, *direction, false)
        }
        Command::Move(direction) => {
            session.require_writable_sides("move")?;
            transfer(session, *direction, true)
        }
        Command::CopyTo {
            side,
            path_option,
            path,
        } => {
            session.require_writable_sides("copyto")?;
            to_folder(session, *side, *path_option, path, false)
        }
        Command::MoveTo {
            side,
            path_option,
            path,
        } => {
            session.require_writable_sides("moveto")?;
            to_folder(session, *side, *path_option, path, true)
        }
        Command::Delete { recycle_bin, side } => {
            session.require_writable_sides("delete")?;
            delete(session, *recycle_bin, *side)
        }
        Command::Rename(spec) => {
            session.require_writable_sides("rename")?;
            rename(session, spec)
        }
        Command::Touch(spec) => {
            session.require_writable_sides("touch")?;
            touch(session, spec)
        }
        Command::Attrib(groups) => {
            session.require_writable_sides("attrib")?;
            attrib(session, groups)
        }
        Command::Sync(spec) => {
            session.require_writable_sides("sync")?;
            sync(session, *spec)
        }
        Command::Snapshot(spec) => snapshot(session, spec),
        Command::Report { kind, spec } => {
            let target = report::write(session, *kind, spec)?;
            if session.options.dry_run {
                println!("would write a report to {}", target.display());
            } else {
                session
                    .log
                    .note(&format!("report written to {}", target.display()));
            }
            Ok(())
        }
    }
}

fn beep() {
    use std::io::Write as _;
    let mut out = std::io::stdout();
    let _ = out.write_all(b"\x07");
    let _ = out.flush();
}

// -- load --------------------------------------------------------------------

fn load(session: &mut Session, spec: &LoadSpec) -> Result<(), ExecError> {
    match spec {
        LoadSpec::Default => {
            session.left = None;
            session.right = None;
            session.left_source = None;
            session.right_source = None;
            session.tree = None;
            session.selection.clear();
            session.expanded.clear();
            session.criteria = crate::state::default_comparison_criteria();
            session.name_masks.clear();
            session.other_filters = ca_fs::OtherFilters::default();
            Ok(())
        }
        LoadSpec::Paths {
            create,
            left,
            right,
        } => {
            let Some(right) = right else {
                if is_remote(left) {
                    return Err(ExecError::Refused(
                        "load needs two locations to compare".to_string(),
                    ));
                }
                let path = session.resolve(left);
                if path.is_dir() {
                    return Err(ExecError::Refused(
                        "load needs two folders to compare".to_string(),
                    ));
                }
                return Err(ExecError::not_supported(
                    "load",
                    "saved sessions are not built yet",
                ));
            };
            let lookup = std::sync::Arc::clone(&session.options.profiles);
            let left_source =
                open_location(session, left, *create, SideArg::Left, lookup.as_ref())?;
            let right_source =
                open_location(session, right, *create, SideArg::Right, lookup.as_ref())?;
            let left_path = location_path(session, left);
            let right_path = location_path(session, right);
            session.left = Some(left_path);
            session.right = Some(right_path);
            session.left_source = Some(left_source);
            session.right_source = Some(right_source);
            session.expanded.clear();
            session.rescan()
        }
    }
}

fn is_remote(path: &str) -> bool {
    let lowered = path.to_ascii_lowercase();
    [
        "ftp://", "ftps://", "sftp://", "http://", "https://", "s3://", "dav://",
    ]
    .iter()
    .any(|scheme| lowered.starts_with(scheme))
}

fn create_side(create: Option<SideArg>, side: SideArg, path: &Path) -> Result<(), ExecError> {
    let wanted = matches!(
        (create, side),
        (Some(SideArg::All), _)
            | (Some(SideArg::Left), SideArg::Left)
            | (Some(SideArg::Right), SideArg::Right)
    );
    if !wanted || path.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(path).map_err(|source| ExecError::Io {
        context: format!("cannot create {}", path.display()),
        source,
    })
}

// -- compare -----------------------------------------------------------------

fn compare(session: &mut Session, kind: Option<CompareType>) -> Result<(), ExecError> {
    let kind = kind
        .or(session.last_compare)
        .unwrap_or(CompareType::RulesBased);
    session.last_compare = Some(kind);
    let (left_base, right_base) = {
        let (left, right) = session.bases("compare")?;
        (left.to_path_buf(), right.to_path_buf())
    };
    if session.selection.is_empty() {
        return Err(ExecError::NoSelection {
            command: "compare".to_string(),
        });
    }
    let mut tests = session.compare_options().content;
    tests.enabled = true;
    tests.method = match kind {
        CompareType::Crc => ca_fs::ContentMethod::Crc32,
        CompareType::Binary => ca_fs::ContentMethod::Binary,
        CompareType::RulesBased => ca_fs::ContentMethod::Rules,
    };
    let rules = TextRules::new();
    let cancel = Cancel::default();
    let selection = session.selection.clone();
    let (Some(left_source), Some(right_source)) = (
        session.source(ca_fs::Side::Left),
        session.source(ca_fs::Side::Right),
    ) else {
        return Err(ExecError::NoComparison {
            command: "compare".to_string(),
        });
    };
    let on_disk = left_source.is_local_folder() && right_source.is_local_folder();
    let mut failures = Vec::new();
    let mut compared = 0usize;
    if let Some(tree) = session.tree.as_mut() {
        walk_mut(tree, &mut |node| {
            if node.is_dir || !selection.contains(&node.rel) {
                return;
            }
            let (Some(left), Some(right)) = (node.left.as_ref(), node.right.as_ref()) else {
                return;
            };
            let result = if on_disk {
                ca_fs::compare_contents(
                    &left_base.join(&left.rel),
                    &right_base.join(&right.rel),
                    &tests,
                    Some(&rules),
                    &cancel,
                )
            } else {
                compare_inside(
                    [(&left_source, left), (&right_source, right)],
                    node.facts,
                    &tests,
                    &rules,
                    &cancel,
                )
            };
            match result {
                Ok(outcome) => {
                    compared += 1;
                    node.content = Some(outcome);
                    node.status = if outcome.is_same(tests.ignore_unimportant) {
                        ca_fs::NodeStatus::Same
                    } else {
                        ca_fs::NodeStatus::Different
                    };
                }
                Err(error) => {
                    node.status = ca_fs::NodeStatus::Error;
                    node.error = Some(error.to_string());
                    failures.push(format!("{}: {error}", node.rel.display()));
                }
            }
        });
        ca_fs::rollup(tree);
    }
    session.log.note(&format!("{compared} files compared"));
    for failure in &failures {
        session.log.item(failure);
    }
    match failures.first() {
        None => Ok(()),
        Some(first) => Err(ExecError::Refused(first.clone())),
    }
}

/// Compare the content of one pair read through the sources that hold it.
fn compare_inside(
    sides: [(&ca_fs::Source, &ca_fs::Entry); 2],
    facts: ca_fs::PairFacts,
    tests: &ca_fs::ContentTests,
    rules: &TextRules,
    cancel: &Cancel,
) -> Result<ca_fs::ContentOutcome, ca_fs::ContentError> {
    let [(left_source, left), (right_source, right)] = sides;
    let path_of = |source: &ca_fs::Source, entry: &ca_fs::Entry| {
        source
            .entry_path(&entry.rel)
            .map_err(|error| ca_fs::ContentError::Source {
                path: entry.rel.display().to_string(),
                detail: error.to_string(),
            })
    };
    let left_path = path_of(left_source, left)?;
    let right_path = path_of(right_source, right)?;
    ca_fs::compare_source_contents_with(
        ca_fs::ContentSide {
            source: left_source,
            path: &left_path,
            facts: facts.left,
        },
        ca_fs::ContentSide {
            source: right_source,
            path: &right_path,
            facts: facts.right,
        },
        tests,
        Some(rules),
        cancel,
    )
}

fn walk_mut(node: &mut Node, visit: &mut dyn FnMut(&mut Node)) {
    visit(node);
    for child in &mut node.children {
        walk_mut(child, visit);
    }
}

// -- file operations ---------------------------------------------------------

/// The answers an unattended run gives to the questions a batch raises.
struct ScriptPolicy {
    confirm: ConfirmMode,
    stop_on_error: bool,
}

impl ErrorPolicy for ScriptPolicy {
    fn on_error(&self, step: &PlanStep, attempt: u32, message: &str) -> Decision {
        if self.stop_on_error {
            AbortOnError.on_error(step, attempt, message)
        } else {
            ContinueOnError.on_error(step, attempt, message)
        }
    }

    fn on_conflict(&self, _step: &PlanStep) -> ConflictDecision {
        match self.confirm {
            ConfirmMode::YesToAll => ConflictDecision::Proceed,
            // A run with nobody to ask treats an unanswered question as a
            // refusal, so a confirmation never turns into a silent overwrite.
            ConfirmMode::Prompt | ConfirmMode::NoToAll => ConflictDecision::Skip,
        }
    }

    fn on_drift(&self, _step: &PlanStep, _drift: &ca_fs::Drift) -> ConflictDecision {
        ConflictDecision::Skip
    }

    fn on_recycle_bin_unavailable(&self, _step: &PlanStep) -> ConflictDecision {
        ConflictDecision::Skip
    }
}

fn operation_options(_session: &Session, use_recycle_bin: bool) -> OperationOptions {
    OperationOptions {
        use_recycle_bin,
        preserve_attributes: true,
        ..OperationOptions::default()
    }
}

fn selected_nodes<'a>(session: &'a Session, command: &str) -> Result<Vec<&'a Node>, ExecError> {
    let Some(tree) = &session.tree else {
        return Err(ExecError::NoComparison {
            command: command.to_string(),
        });
    };
    if session.selection.is_empty() {
        return Err(ExecError::NoSelection {
            command: command.to_string(),
        });
    }
    let selection: BTreeSet<PathBuf> = session.selection.clone();
    Ok(ca_fs::resolve_selection(tree, &selection))
}

/// Refuse a plan that writes below a base folder the run locked.
///
/// The check reads the paths each step writes rather than the side the plan
/// recorded, so a step that writes outside both base folders is unaffected and
/// a step that reaches a locked folder by any route is caught.
fn check_read_only(session: &Session, plan: &OperationPlan) -> Result<(), ExecError> {
    for step in &plan.steps {
        for written in step.action.written_paths() {
            session.check_path_write(written, "operation")?;
        }
    }
    Ok(())
}

/// Build the plan, then either report it or run it.
fn carry_out(session: &mut Session, plan: &OperationPlan, label: &str) -> Result<(), ExecError> {
    let result = carry_out_inner(session, plan, label);
    if result.is_err() {
        session.selection.clear();
        session.tree = None;
        let _ = session.rescan();
    }
    result
}

#[allow(
    clippy::too_many_lines,
    reason = "one planned batch is validated, executed and reported as a single result"
)]
fn carry_out_inner(
    session: &mut Session,
    plan: &OperationPlan,
    label: &str,
) -> Result<(), ExecError> {
    check_read_only(session, plan)?;
    for skip in &plan.skipped {
        if skip.conflict.is_some() {
            session.log.error(&format!(
                "{} left undone: {}",
                skip.path.display(),
                skip.reason
            ));
        } else {
            session
                .log
                .item(&format!("skipped {}: {}", skip.path.display(), skip.reason));
        }
    }
    if session.options.dry_run {
        for step in &plan.steps {
            let line = format!("would {label} {}", step.action.target().display());
            session.log.item(&line);
            println!("{line}");
        }
        session
            .log
            .note(&format!("{} steps planned, none run", plan.steps.len()));
        if let Some(skip) = plan.skipped.iter().find(|skip| skip.conflict.is_some()) {
            return Err(ExecError::Refused(format!(
                "{} left undone: {}",
                skip.path.display(),
                skip.reason
            )));
        }
        return Ok(());
    }
    std::fs::create_dir_all(&session.options.journal_directory).map_err(|source| {
        ExecError::Io {
            context: format!(
                "cannot create {}",
                session.options.journal_directory.display()
            ),
            source,
        }
    })?;
    let journal =
        Journal::create_in(&session.options.journal_directory).map_err(|source| ExecError::Io {
            context: "cannot open a journal, so nothing ran".to_string(),
            source,
        })?;
    let policy = ScriptPolicy {
        confirm: session.options.confirm,
        stop_on_error: session.options.stop_on_error,
    };
    let cancel = Cancel::default();
    let mounts = session.mounts();
    let mut commit: Option<std::io::Error> = None;
    let report = if mounts.is_empty() {
        let context =
            ExecutionContext::new(&RealFs, &cancel, Journaling::To(&journal)).with_policy(&policy);
        ca_fs::execute(plan, &context)
    } else {
        // A container is rewritten once for the whole batch, so every step of
        // one plan is queued and applied together.
        let ops = ca_fs::SourceOps::new(mounts);
        ops.open_batch();
        let report = {
            let context =
                ExecutionContext::new(&ops, &cancel, Journaling::To(&journal)).with_policy(&policy);
            ca_fs::execute(plan, &context)
        };
        commit = ops.commit_batch().err();
        report
    };
    let journal_path = journal.path().to_path_buf();
    drop(journal);
    let _ = ca_fs::retire(&journal_path);

    for step in &plan.steps {
        session
            .log
            .item(&format!("{label} {}", step.action.target().display()));
    }
    for result in &report.results {
        if let ca_fs::StepOutcome::Skipped { reason } = &result.outcome {
            session
                .log
                .error(&format!("step {} left undone: {reason}", result.index));
        }
    }
    let failures = report.failures();
    session.log.note(&format!(
        "{} of {} steps done",
        report.completed(),
        plan.steps.len()
    ));
    for (index, message) in &failures {
        session.log.error(&format!("step {index}: {message}"));
    }
    if let Some(error) = commit {
        let message = format!("the queued changes were not applied: {error}");
        session.log.error(&message);
        return Err(ExecError::Refused(message));
    }
    if let Some((_, message)) = failures.first() {
        return Err(ExecError::Refused(message.clone()));
    }
    if let Some(skip) = plan.skipped.iter().find(|skip| skip.conflict.is_some()) {
        return Err(ExecError::Refused(format!(
            "{} left undone: {}",
            skip.path.display(),
            skip.reason
        )));
    }
    if let Some(result) = report
        .results
        .iter()
        .find(|result| matches!(&result.outcome, ca_fs::StepOutcome::Skipped { .. }))
    {
        if let ca_fs::StepOutcome::Skipped { reason } = &result.outcome {
            return Err(ExecError::Refused(format!(
                "step {} left undone: {reason}",
                result.index
            )));
        }
    }
    if report.cancelled {
        return Err(ExecError::Refused(format!(
            "{label} was cancelled before it finished"
        )));
    }
    Ok(())
}

fn transfer(session: &mut Session, direction: Direction, moving: bool) -> Result<(), ExecError> {
    let word = if moving { "move" } else { "copy" };
    let from = match direction {
        Direction::LeftToRight => Side::Left,
        Direction::RightToLeft => Side::Right,
    };
    let options = operation_options(session, false);
    let plan = {
        let selection = selected_nodes(session, word)?;
        let (left, right) = session.bases(word)?;
        let bases = Bases { left, right };
        if moving {
            ca_fs::plan_move(&selection, from, bases, &options)
        } else {
            ca_fs::plan_copy(&selection, from, bases, &options)
        }
    };
    carry_out(session, &plan, word)?;
    session.rescan()
}

fn to_folder(
    session: &mut Session,
    side: SideArg,
    path_option: PathOption,
    path: &str,
    moving: bool,
) -> Result<(), ExecError> {
    let word = if moving { "moveto" } else { "copyto" };
    let target = session.resolve(path);
    session.check_path_write(&target, word)?;
    let sides: &[Side] = match side {
        SideArg::Left => &[Side::Left],
        SideArg::Right => &[Side::Right],
        SideArg::All => &[Side::Left, Side::Right],
    };
    let has_present = {
        let selection = selected_nodes(session, word)?;
        sides.iter().any(|from| {
            selection.iter().any(|node| match from {
                Side::Left => node.left.is_some(),
                Side::Right => node.right.is_some(),
            })
        })
    };
    if !has_present {
        return Ok(());
    }
    // The plan resolves the destination while it is built, so the folder has to
    // be there before planning even when nothing is copied into it yet.
    if !session.options.dry_run && !target.exists() {
        std::fs::create_dir_all(&target).map_err(|source| ExecError::Io {
            context: format!("cannot create {}", target.display()),
            source,
        })?;
    }
    let option = match path_option {
        PathOption::Relative => ca_fs::PathOption::KeepRelative,
        PathOption::Base => ca_fs::PathOption::KeepBase,
        PathOption::None => ca_fs::PathOption::Flatten,
    };
    let options = operation_options(session, false);
    // The plan reads each destination through the same routes the batch then
    // writes through, so a side inside a container resolves as the step does.
    let routed = ca_fs::SourceOps::new(session.mounts());
    for from in sides {
        let plan = {
            let selection = selected_nodes(session, word)?;
            let present: Vec<&Node> = selection
                .into_iter()
                .filter(|node| match from {
                    Side::Left => node.left.is_some(),
                    Side::Right => node.right.is_some(),
                })
                .collect();
            if present.is_empty() {
                continue;
            }
            let (left, right) = session.bases(word)?;
            let bases = Bases { left, right };
            ca_fs::plan_to_folder(
                &present, *from, bases, &target, option, &options, moving, &routed,
            )
        };
        carry_out(session, &plan, word)?;
    }
    if moving {
        session.rescan()?;
    }
    Ok(())
}

fn sides_of(side: SideArg) -> Sides {
    match side {
        SideArg::Left => Sides::Left,
        SideArg::Right => Sides::Right,
        SideArg::All => Sides::Both,
    }
}

fn delete(
    session: &mut Session,
    recycle_bin: Option<bool>,
    side: SideArg,
) -> Result<(), ExecError> {
    let use_recycle_bin = recycle_bin.unwrap_or(session.options.recycle_bin);
    let options = operation_options(session, use_recycle_bin);
    let plan = {
        let selection = selected_nodes(session, "delete")?;
        let (left, right) = session.bases("delete")?;
        let bases = Bases { left, right };
        ca_fs::plan_delete(&selection, sides_of(side), bases, &options)
    };
    carry_out(session, &plan, "delete")?;
    session.rescan()
}

fn rename(session: &mut Session, spec: &RenameSpec) -> Result<(), ExecError> {
    let action = match spec {
        RenameSpec::Mask(mask) => ca_fs::RenameAction::Mask(mask.clone()),
        RenameSpec::Regex { find, replace } => ca_fs::RenameAction::Regex {
            find: find.clone(),
            replace: replace.clone(),
        },
    };
    let options = operation_options(session, false);
    let plan = {
        let selection = selected_nodes(session, "rename")?;
        let Some(tree) = session.tree.as_ref() else {
            return Err(ExecError::NoComparison {
                command: "rename".to_string(),
            });
        };
        let (left, right) = session.bases("rename")?;
        let bases = Bases { left, right };
        ca_fs::plan_rename(tree, &selection, Sides::Both, bases, &action, &options)
            .map_err(|error| ExecError::Refused(error.to_string()))?
    };
    carry_out(session, &plan, "rename")?;
    session.rescan()
}

fn touch(session: &mut Session, spec: &TouchSpec) -> Result<(), ExecError> {
    let (sides, touch_spec) = match spec {
        TouchSpec::Copy(Direction::LeftToRight) => (Sides::Right, ca_fs::TouchSpec::FromOtherSide),
        TouchSpec::Copy(Direction::RightToLeft) => (Sides::Left, ca_fs::TouchSpec::FromOtherSide),
        TouchSpec::Set { side, value } => {
            let when = match value {
                TouchValue::Now => std::time::SystemTime::now(),
                TouchValue::Timestamp(text) => {
                    let seconds = clock::parse_timestamp(text, session.options.offset_seconds)
                        .ok_or_else(|| {
                            ExecError::Refused(format!(
                                "the timestamp {text} is not written as yyyy-mm-dd with an optional hh:mm:ss"
                            ))
                        })?;
                    clock::system_time(seconds).ok_or_else(|| {
                        ExecError::Refused(format!(
                            "the timestamp {text} is outside the range this system can represent"
                        ))
                    })?
                }
            };
            (sides_of(*side), ca_fs::TouchSpec::Explicit(when))
        }
    };
    let options = operation_options(session, false);
    let plan = {
        let selection = selected_nodes(session, "touch")?;
        let (left, right) = session.bases("touch")?;
        let bases = Bases { left, right };
        ca_fs::plan_touch(&selection, sides, bases, touch_spec, &options)
    };
    carry_out(session, &plan, "touch")?;
    session.rescan()
}

fn attrib(session: &mut Session, groups: &[AttribChange]) -> Result<(), ExecError> {
    if !cfg!(windows) {
        return Err(ExecError::not_supported(
            "attrib",
            "this platform has no DOS file attributes",
        ));
    }
    let change = attribute_change(groups);
    if change.is_empty() {
        return Err(ExecError::Refused(
            "the attrib command names no attribute".to_string(),
        ));
    }
    let options = operation_options(session, false);
    let plan = {
        let selection = selected_nodes(session, "attrib")?;
        let (left, right) = session.bases("attrib")?;
        let bases = Bases { left, right };
        ca_fs::plan_attributes(&selection, Sides::Both, bases, change, &options)
    };
    carry_out(session, &plan, "attrib")?;
    session.rescan()
}

fn attribute_change(groups: &[AttribChange]) -> ca_fs::AttributeChange {
    let mut change = ca_fs::AttributeChange::default();
    for group in groups {
        let AttrSet {
            archive,
            system,
            hidden,
            read_only,
        } = group.attrs;
        if archive {
            change.archive = Some(group.set);
        }
        if hidden {
            change.hidden = Some(group.set);
        }
        if read_only {
            change.read_only = Some(group.set);
        }
        if system {
            change.system = Some(group.set);
        }
    }
    change
}

fn sync(session: &mut Session, spec: SyncSpec) -> Result<(), ExecError> {
    let preset = match (spec.mode, spec.direction) {
        (SyncMode::Update, SyncDirection::LeftToRight) => SyncPreset::UpdateRight,
        (SyncMode::Update, SyncDirection::RightToLeft) => SyncPreset::UpdateLeft,
        (SyncMode::Update, SyncDirection::All) => SyncPreset::UpdateBoth,
        (SyncMode::Mirror, SyncDirection::LeftToRight) => SyncPreset::MirrorToRight,
        (SyncMode::Mirror, SyncDirection::RightToLeft) => SyncPreset::MirrorToLeft,
        (SyncMode::Mirror, SyncDirection::All) => {
            return Err(ExecError::Refused(
                "mirror needs one direction, not all".to_string(),
            ))
        }
    };
    let options = OperationOptions {
        create_empty_folders: spec.create_empty,
        ..operation_options(session, false)
    };
    let plan = {
        let Some(tree) = &session.tree else {
            return Err(ExecError::NoComparison {
                command: "sync".to_string(),
            });
        };
        let (left, right) = session.bases("sync")?;
        let bases = Bases { left, right };
        let mut preview = ca_fs::preview(tree, &preset);
        if spec.visible {
            hide_unexpanded(&mut preview, session);
        }
        ca_fs::plan_sync(tree, &preset, bases, &options, &preview)
            .map_err(|error| ExecError::Refused(error.to_string()))?
    };
    carry_out(session, &plan, "sync")?;
    session.rescan()
}

/// `visible` keeps the run to the folders a script opened.
fn hide_unexpanded(preview: &mut ca_fs::SyncPreview, session: &Session) {
    let rows: Vec<PathBuf> = preview
        .rows
        .iter()
        .filter(|row| {
            row.rel
                .parent()
                .is_some_and(|parent| !session.expanded.holds(parent))
        })
        .map(|row| row.rel.clone())
        .collect();
    for rel in rows {
        preview.set_override(rel, ca_fs::SyncAction::LeaveAlone);
    }
}

/// The path a location occupies on this machine. A remote location has none,
/// so it keeps the text it was written with.
fn location_path(session: &Session, location: &str) -> PathBuf {
    if is_remote(location) {
        PathBuf::from(location)
    } else {
        session.resolve(location)
    }
}

/// Open one side of a comparison: a remote location, a folder, a container
/// read as folders, or a recorded listing.
fn open_location(
    session: &Session,
    location: &str,
    create: Option<SideArg>,
    side: SideArg,
    lookup: &dyn crate::profiles::ProfileLookup,
) -> Result<ca_fs::Source, ExecError> {
    if is_remote(location) {
        let fs = crate::profiles::connect(location, lookup, &ca_vfs::Cancel::default())?;
        return Ok(ca_fs::Source::over(ca_fs::SourceKind::Remote, fs));
    }
    let path = session.resolve(location);
    if session.options.dry_run && !path.exists() && create.is_some() {
        return Err(ExecError::Refused(format!(
            "dry run will not create {}; preview requires existing base folders",
            path.display()
        )));
    }
    create_side(create, side, &path)?;
    open_side(&path, &session.options.archive_types)
}

/// Open one side of a comparison: a folder, a container read as folders, or a
/// recorded listing.
fn open_side(path: &Path, types: &ca_fs::ArchiveTypes) -> Result<ca_fs::Source, ExecError> {
    if path.is_dir() {
        return Ok(ca_fs::Source::local(path));
    }
    if !path.exists() {
        return Err(ExecError::Refused(format!(
            "{} is not a folder",
            path.display()
        )));
    }
    ca_fs::Source::open(path, types, &ca_fs::Limits::default())
        .map_err(|error| ExecError::Refused(error.to_string()))
}

// -- snapshot ----------------------------------------------------------------

fn snapshot(session: &mut Session, spec: &SnapshotSpec) -> Result<(), ExecError> {
    if spec.save_version {
        return Err(ExecError::not_supported(
            "snapshot",
            "the version resource of executables is not read yet",
        ));
    }
    let root = match &spec.source {
        SnapshotSource::Left => session
            .left
            .clone()
            .ok_or_else(|| ExecError::NoComparison {
                command: "snapshot".to_string(),
            })?,
        SnapshotSource::Right => session
            .right
            .clone()
            .ok_or_else(|| ExecError::NoComparison {
                command: "snapshot".to_string(),
            })?,
        SnapshotSource::Path(path) => session.resolve(path),
    };
    let target = snapshot_target(session, spec, &root);
    session.check_path_write(&target, "snapshot")?;
    let options = ca_vfs::SnapshotOptions {
        include_crc: spec.save_crc,
        include_empty_folders: spec.include_empty,
        follow_links: spec.follow_symlinks,
    };
    // A container is recorded as folders when the command asks for it, so the
    // capture holds the entries inside it rather than the one container file.
    let source = if spec.expand_archives {
        open_side(&root, &session.options.archive_types)?
    } else {
        ca_fs::Source::local(&root)
    };
    let cancel = ca_fs::Cancel::default();
    let visible = if spec.no_filters {
        None
    } else {
        Some(snapshot_filter_paths(
            &source,
            &root,
            session,
            spec.follow_symlinks,
            &cancel,
        )?)
    };
    let vfs_cancel = ca_vfs::Cancel::from_flag(cancel.as_flag());
    let capture = ca_vfs::Snapshot::capture(
        source.file_system().as_ref(),
        source.root(),
        options,
        &vfs_cancel,
    )
    .map_err(|error| ExecError::Refused(error.to_string()))?;
    let mut capture = capture;
    if let Some(visible) = visible {
        capture
            .entries
            .retain(|entry| visible.contains(&entry.path.replace('\\', "/")));
    }
    if session.options.dry_run {
        session
            .log
            .note(&format!("would write a snapshot to {}", target.display()));
        println!("would write a snapshot to {}", target.display());
        return Ok(());
    }
    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|source| ExecError::Io {
                context: format!("cannot create {}", parent.display()),
                source,
            })?;
        }
    }
    capture
        .save(&target)
        .map_err(|error| ExecError::Refused(error.to_string()))?;
    session
        .log
        .note(&format!("snapshot written to {}", target.display()));
    Ok(())
}

fn snapshot_filter_paths(
    source: &ca_fs::Source,
    root: &Path,
    session: &Session,
    follow_links: bool,
    cancel: &ca_fs::Cancel,
) -> Result<std::collections::BTreeSet<String>, ExecError> {
    let scan = ca_fs::scan_source(
        source,
        &ca_fs::ScanOptions {
            follow_links,
            ..ca_fs::ScanOptions::default()
        },
        cancel,
        &|_| {},
    )
    .map_err(|error| ExecError::Refused(error.to_string()))?;
    if scan.cancelled {
        return Err(ExecError::Refused(
            "snapshot filter scan was cancelled".to_string(),
        ));
    }
    let mut tree = ca_fs::align_trees(
        &scan,
        &ca_fs::ScanResult::default(),
        &ca_fs::AlignmentOptions::default(),
        cancel,
    );
    let names = ca_fs::NameFilters::parse(&session.name_masks);
    let mut other = session.other_filters.clone();
    other.left_root = root.to_path_buf();
    other.right_root = root.to_path_buf();
    let context = ca_fs::FilterContext {
        now: std::time::SystemTime::now(),
        local_offset_seconds: i32::try_from(session.options.offset_seconds).unwrap_or(0),
    };
    ca_fs::apply_filters(&mut tree, &names, &other, &context);
    let mut visible = std::collections::BTreeSet::new();
    tree.walk(&mut |node| {
        if node.left.is_some() {
            visible.insert(node.rel.to_string_lossy().replace('\\', "/"));
        }
    });
    Ok(visible)
}

/// The file extension a snapshot is written with.
const SNAPSHOT_EXTENSION: &str = "cass";

fn snapshot_target(session: &Session, spec: &SnapshotSpec, root: &Path) -> PathBuf {
    let generated = || {
        let name = root.file_name().map_or_else(
            || "snapshot".to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let date = clock::format_date(
            clock::unix_seconds(std::time::SystemTime::now()) + session.options.offset_seconds,
        );
        format!("{name}_{date}.{SNAPSHOT_EXTENSION}")
    };
    let Some(output) = &spec.output else {
        return session.options.working_directory.join(generated());
    };
    let path = session.resolve(output);
    if path.is_dir() {
        return path.join(generated());
    }
    if path.extension().is_none() {
        return path.with_extension(SNAPSHOT_EXTENSION);
    }
    path
}

/// The report commands whose engine is not built yet.
#[must_use]
pub fn unsupported_reports() -> &'static [ReportKind] {
    &[
        ReportKind::Data,
        ReportKind::Media,
        ReportKind::Picture,
        ReportKind::Registry,
        ReportKind::Version,
    ]
}

/// The content criteria a run can carry out.
#[must_use]
pub fn supported_content_criteria() -> &'static [ContentCriterion] {
    &[
        ContentCriterion::Size,
        ContentCriterion::Crc,
        ContentCriterion::Binary,
        ContentCriterion::RulesBased,
    ]
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::carry_out;
    use crate::state::{Session, SessionOptions};
    use ca_fs::{Conflict, OperationKind, OperationOptions, OperationPlan};
    use std::path::PathBuf;

    #[test]
    fn a_refused_plan_skip_is_an_error_even_in_a_dry_run() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let mut session = Session::new(SessionOptions {
            dry_run: true,
            journal_directory: directory.path().join("journals"),
            ..SessionOptions::default()
        });
        let mut plan =
            OperationPlan::new(OperationKind::Sync, Vec::new(), OperationOptions::default());
        plan.skipped.push(ca_fs::ops::plan::PlanSkip::refused(
            PathBuf::from("protected/subfolder"),
            Conflict::CounterpartUnreadable,
            "the listing was incomplete",
        ));

        let error = carry_out(&mut session, &plan, "sync")
            .expect_err("a refusal must not produce a clean dry run");
        assert!(error.to_string().contains("listing was incomplete"));
    }
}
