//! The plan that writes a merge result into the output folder.
//!
//! Every row resolves to what the output should hold at its path: one input's
//! item, nothing, or no change at all. The planner turns the difference
//! between that and what the output holds now into ordinary plan steps, so
//! the result runs through the same confirmation, journal and execution as
//! every other file operation.
//!
//! The planner writes into the output folder only. A step whose written path
//! would leave it is dropped and reported, whatever produced it.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Component, Path, PathBuf};

use crate::criteria::quick_compare;
use crate::merge::{MergeBases, MergeRow, MergeStatus, MergeTree, Pane};
use crate::ops::plan::{
    path_is_within, step, Conflict, OperationKind, OperationOptions, OperationPlan, PathState,
    PlanSkip, PlanStep, StepAction, StepExpectation,
};
use crate::scan::Entry;

/// What the output should hold at one path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// Whatever the named input holds, including nothing.
    Take(Pane),
    /// Nothing.
    Delete,
    /// Leave the output as it is.
    Leave,
}

/// Why a merge produced no plan at all.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MergeRefused {
    /// A scan was cancelled, so every absence in the tree may be an artefact
    /// of the interruption.
    #[error("a cancelled scan cannot be merged")]
    ScanCancelled,
}

/// Which rows take part in one merge, and how the user resolved them.
#[derive(Debug, Clone, Copy)]
pub struct MergeRequest<'a> {
    /// Resolutions the user chose, by relative path.
    pub overrides: &'a BTreeMap<PathBuf, Resolution>,
    /// The rows that take part, with everything below each. `None` takes
    /// every row.
    pub selection: Option<&'a BTreeSet<PathBuf>>,
    /// Resolve the rows the user did not resolve from their status.
    pub automatic: bool,
}

/// The resolution a row's status implies when nobody chose one.
///
/// A row a person has to resolve, and a row with no provable status, is left
/// alone.
#[must_use]
pub fn automatic_resolution(row: &MergeRow) -> Resolution {
    match row.status {
        MergeStatus::Unchanged if row.left.is_some() => Resolution::Take(Pane::Left),
        MergeStatus::Unchanged if row.right.is_some() => Resolution::Take(Pane::Right),
        MergeStatus::LeftChange(_) | MergeStatus::SameChange(_) => Resolution::Take(Pane::Left),
        MergeStatus::RightChange(_) => Resolution::Take(Pane::Right),
        MergeStatus::Unchanged
        | MergeStatus::Mergeable
        | MergeStatus::Conflict
        | MergeStatus::Unknown => Resolution::Leave,
    }
}

/// The resolution one row receives in `request`.
#[must_use]
pub fn resolution_of(row: &MergeRow, request: &MergeRequest<'_>) -> Resolution {
    if let Some(chosen) = request.overrides.get(&row.rel) {
        return *chosen;
    }
    if request.automatic {
        automatic_resolution(row)
    } else {
        Resolution::Leave
    }
}

/// True when `row` is in the part of the tree `request` merges.
fn takes_part(row: &MergeRow, request: &MergeRequest<'_>) -> bool {
    request
        .selection
        .is_none_or(|selected| selected.iter().any(|chosen| row.rel.starts_with(chosen)))
}

/// True when a merge of `request` leaves `row` for a person: the row takes
/// part, needs a person, and nobody chose a resolution that writes it.
///
/// The plan has no step for such a row, so the output holds its merge result
/// only after a person writes it.
#[must_use]
pub fn leaves_for_person(row: &MergeRow, request: &MergeRequest<'_>) -> bool {
    row.status.needs_person()
        && takes_part(row, request)
        && resolution_of(row, request) == Resolution::Leave
}

/// Every row a merge of `request` leaves for a person, in tree order.
#[must_use]
pub fn left_for_person<'t>(tree: &'t MergeTree, request: &MergeRequest<'_>) -> Vec<&'t MergeRow> {
    tree.rows
        .iter()
        .filter(|row| leaves_for_person(row, request))
        .collect()
}

/// Turn a compared tree and the user's resolutions into the steps that write
/// the output folder.
///
/// # Errors
/// Returns [`MergeRefused::ScanCancelled`] when any scan was cut short.
pub fn plan_merge(
    tree: &MergeTree,
    bases: MergeBases<'_>,
    request: &MergeRequest<'_>,
    options: &OperationOptions,
) -> Result<OperationPlan, MergeRefused> {
    if tree.cancelled {
        return Err(MergeRefused::ScanCancelled);
    }
    let mut roots = vec![bases.output.to_path_buf(), bases.left.to_path_buf()];
    roots.extend(bases.center.map(Path::to_path_buf));
    roots.push(bases.right.to_path_buf());
    let mut builder = Builder {
        plan: OperationPlan::new(OperationKind::Merge, roots, options.clone()),
        bases,
        output_by_rel: tree
            .rows
            .iter()
            .filter_map(|row| row.output.as_ref().map(|entry| (row.rel.clone(), entry)))
            .collect(),
        created: BTreeSet::new(),
        occupied: BTreeSet::new(),
        folder_removals: Vec::new(),
    };
    if !output_exists(tree) {
        builder.create_output_root();
    }

    for row in &tree.rows {
        let resolution = if takes_part(row, request) {
            resolution_of(row, request)
        } else {
            Resolution::Leave
        };
        builder.row(row, resolution);
    }
    builder.finish_folder_removals();
    let mut plan = builder.plan;
    refuse_case_collisions(&mut plan);
    refuse_escapes(&mut plan, bases.output);
    for (position, step) in plan.steps.iter_mut().enumerate() {
        step.index = position;
    }
    Ok(plan)
}

/// True when the output folder was listed, which is how a tree records that
/// it exists.
fn output_exists(tree: &MergeTree) -> bool {
    tree.output_listed
}

struct Builder<'a> {
    plan: OperationPlan,
    bases: MergeBases<'a>,
    output_by_rel: HashMap<PathBuf, &'a Entry>,
    created: BTreeSet<PathBuf>,
    /// Folders of the output that still hold something once the plan has run.
    occupied: BTreeSet<PathBuf>,
    /// Folders of the output the plan would empty and remove, parents first.
    folder_removals: Vec<(&'a MergeRow, &'a Entry)>,
}

impl<'a> Builder<'a> {
    fn create_output_root(&mut self) {
        let target = self.bases.output.to_path_buf();
        let mut create = step(StepAction::CreateDir { path: target }, PathBuf::new(), None);
        create.expected = StepExpectation {
            source: None,
            target: Some(PathState::absent()),
        };
        self.plan.steps.push(create);
        self.created.insert(PathBuf::new());
    }

    fn row(&mut self, row: &'a MergeRow, resolution: Resolution) {
        if !is_plain(&row.rel) {
            self.plan.skipped.push(PlanSkip::noted(
                row.rel.clone(),
                "the path names a folder outside the base folders",
            ));
            self.keep(row);
            return;
        }
        let wanted = match resolution {
            Resolution::Leave => {
                self.keep(row);
                return;
            }
            Resolution::Delete => None,
            Resolution::Take(pane) => {
                if pane == Pane::Center && self.bases.center.is_none() {
                    self.keep(row);
                    return;
                }
                row.entry(pane).map(|entry| (pane, entry))
            }
        };
        match wanted {
            Some((pane, source)) => self.write(row, pane, source),
            None => self.remove(row),
        }
    }

    /// Record that the output keeps whatever it holds at this row.
    fn keep(&mut self, row: &MergeRow) {
        if row.output.is_some() {
            self.occupy(&row.rel);
        }
    }

    fn occupy(&mut self, rel: &Path) {
        let mut current = rel.parent();
        while let Some(parent) = current {
            if parent.as_os_str().is_empty() {
                break;
            }
            self.occupied.insert(parent.to_path_buf());
            current = parent.parent();
        }
    }

    fn write(&mut self, row: &'a MergeRow, pane: Pane, source: &'a Entry) {
        self.occupy(&row.rel);
        if source.is_dir {
            self.occupied.insert(row.rel.clone());
        }
        if source.is_link() {
            self.plan.skipped.push(PlanSkip::noted(
                row.rel.clone(),
                "a link is not merged; its target may lie outside the base folders",
            ));
            return;
        }
        let target_rel = row.output.as_ref().map_or(&row.rel, |entry| &entry.rel);
        if crate::ops::plan::skips_unreachable_name(&mut self.plan, &row.rel, target_rel) {
            return;
        }
        if !self.ancestors_ready(row) {
            return;
        }
        let Some(source_base) = self.bases.of(pane) else {
            return;
        };
        let target = self.bases.output.join(target_rel);
        match row.output.as_ref() {
            Some(existing) if existing.is_link() => {
                self.plan.skipped.push(PlanSkip::refused(
                    row.rel.clone(),
                    Conflict::RemovesLinkOnly,
                    "the output item is a link, so nothing is written through it",
                ));
            }
            Some(existing) if existing.is_dir != source.is_dir => {
                self.plan.skipped.push(PlanSkip::refused(
                    row.rel.clone(),
                    Conflict::KindMismatch,
                    "the output holds a file where a folder goes, or a folder where a file goes",
                ));
            }
            Some(_) if source.is_dir => {}
            None if source.is_dir => {
                if self.created.insert(row.rel.clone()) {
                    let mut create = step(
                        StepAction::CreateDir { path: target },
                        row.rel.clone(),
                        None,
                    );
                    create.expected = StepExpectation {
                        source: None,
                        target: Some(PathState::absent()),
                    };
                    self.plan.steps.push(create);
                }
            }
            existing => {
                let source_path = source_base.join(&source.rel);
                if same_path(&source_path, &target) {
                    return;
                }
                if existing.is_some_and(|existing| {
                    quick_compare(source, existing, &crate::criteria::QuickTests::default())
                        .is_same()
                }) {
                    return;
                }
                let mut copy = step(
                    StepAction::CopyFile {
                        source: source_path,
                        target: target.clone(),
                    },
                    row.rel.clone(),
                    None,
                );
                copy.bytes = source.size;
                copy.conflicts = copy_conflicts(source, existing);
                copy.expected = StepExpectation {
                    source: Some(state_of(Some(source))),
                    target: Some(state_of(existing)),
                };
                copy.backup = self
                    .plan
                    .options
                    .backup
                    .as_ref()
                    .filter(|_| existing.is_some())
                    .map(|backup| backup.path_for(&target));
                self.plan.steps.push(copy);
            }
        }
    }

    /// Make sure every folder above the row exists in the output, creating
    /// the missing ones. Returns false, with the row refused, when a folder
    /// above it is a link or a file.
    fn ancestors_ready(&mut self, row: &MergeRow) -> bool {
        let mut ancestors: Vec<PathBuf> = Vec::new();
        let mut current = row.rel.parent();
        while let Some(parent) = current {
            if parent.as_os_str().is_empty() {
                break;
            }
            ancestors.push(parent.to_path_buf());
            current = parent.parent();
        }
        ancestors.reverse();
        for ancestor in ancestors {
            match self.output_by_rel.get(&ancestor) {
                Some(existing) if existing.is_link() || !existing.is_dir => {
                    self.plan.skipped.push(PlanSkip::refused(
                        row.rel.clone(),
                        Conflict::RemovesLinkOnly,
                        "a folder above this item in the output is a link or a file",
                    ));
                    return false;
                }
                Some(_) => {}
                None => {
                    if self.created.insert(ancestor.clone()) {
                        let mut create = step(
                            StepAction::CreateDir {
                                path: self.bases.output.join(&ancestor),
                            },
                            ancestor,
                            None,
                        );
                        create.expected = StepExpectation {
                            source: None,
                            target: Some(PathState::absent()),
                        };
                        self.plan.steps.push(create);
                    }
                }
            }
        }
        true
    }

    fn remove(&mut self, row: &'a MergeRow) {
        let Some(existing) = row.output.as_ref() else {
            return;
        };
        // An absence proves nothing where a listing was partial, so neither an
        // unreadable input nor an unreadable output leads to a removal.
        if row.status == MergeStatus::Unknown || row.output_uncertain {
            self.plan.skipped.push(PlanSkip::refused(
                row.rel.clone(),
                Conflict::CounterpartUnreadable,
                "a listing here could not be read in full, so nothing here is removed",
            ));
            self.keep(row);
            return;
        }
        if row.left.is_none() && row.center.is_none() && row.right.is_none() {
            self.plan.skipped.push(PlanSkip::noted(
                row.rel.clone(),
                "no input holds this item, so the merge leaves it alone",
            ));
            self.keep(row);
            return;
        }
        let path = self.bases.output.join(&existing.rel);
        if existing.is_link() {
            let mut removal = step(StepAction::DeleteLink { path }, row.rel.clone(), None);
            removal.conflicts.push(Conflict::RemovesLinkOnly);
            removal.expected = removal_expected(existing);
            self.plan.steps.push(removal);
            return;
        }
        if existing.is_dir {
            self.folder_removals.push((row, existing));
            return;
        }
        let action = if self.plan.options.use_recycle_bin {
            StepAction::Trash { path }
        } else {
            StepAction::DeleteFile { path }
        };
        let mut removal = step(action, row.rel.clone(), None);
        if existing.attributes.read_only {
            removal.conflicts.push(Conflict::DeleteReadOnly);
        }
        removal.bytes = existing.size;
        removal.expected = removal_expected(existing);
        self.plan.steps.push(removal);
    }

    /// Remove the emptied folders, deepest first.
    ///
    /// A folder is removed on its own, never with its contents, so an item
    /// the plan did not list is never taken with it.
    fn finish_folder_removals(&mut self) {
        let removals = std::mem::take(&mut self.folder_removals);
        for (row, existing) in removals.into_iter().rev() {
            if self.occupied.contains(&row.rel) || row.output_holds_excluded {
                self.plan.skipped.push(PlanSkip::noted(
                    row.rel.clone(),
                    "the folder still holds items the merge keeps",
                ));
                self.keep(row);
                continue;
            }
            let path = self.bases.output.join(&existing.rel);
            let mut removal = step(StepAction::DeleteDir { path }, row.rel.clone(), None);
            if existing.attributes.read_only {
                removal.conflicts.push(Conflict::DeleteReadOnly);
            }
            removal.expected = removal_expected(existing);
            self.plan.steps.push(removal);
        }
    }
}

fn state_of(entry: Option<&Entry>) -> PathState {
    entry.map_or_else(PathState::absent, |entry| PathState {
        exists: true,
        is_dir: entry.is_dir,
        is_link: entry.is_link(),
        size: entry.size,
        modified: entry.modified,
        children: None,
    })
}

fn removal_expected(entry: &Entry) -> StepExpectation {
    StepExpectation {
        source: None,
        target: Some(state_of(Some(entry))),
    }
}

fn copy_conflicts(source: &Entry, existing: Option<&Entry>) -> Vec<Conflict> {
    let mut conflicts = Vec::new();
    let Some(target) = existing else {
        return conflicts;
    };
    conflicts.push(Conflict::TargetExists);
    if let (Some(source_time), Some(target_time)) = (source.modified, target.modified) {
        if target_time > source_time {
            conflicts.push(Conflict::OverwriteNewer);
        }
    }
    if target.attributes.read_only {
        conflicts.push(Conflict::TargetReadOnly);
    }
    if target.attributes.hidden || target.attributes.system {
        conflicts.push(Conflict::TargetHiddenOrSystem);
    }
    conflicts
}

/// True when the relative path holds plain names only.
fn is_plain(rel: &Path) -> bool {
    rel.components()
        .all(|component| matches!(component, Component::Normal(_)))
}

/// True when two paths name the same place, judged on their text.
fn same_path(first: &Path, second: &Path) -> bool {
    let fold = |path: &Path| {
        let text = path.to_string_lossy().replace('\\', "/");
        let text = text.trim_end_matches('/').to_owned();
        if cfg!(windows) {
            text.to_lowercase()
        } else {
            text
        }
    };
    fold(first) == fold(second)
}

/// Drop every pair of steps that would write one output path under two
/// spellings that differ only in case.
///
/// A file system that ignores case stores both under one name, so the second
/// write would silently replace the first.
fn refuse_case_collisions(plan: &mut OperationPlan) {
    let mut seen: HashMap<String, Vec<usize>> = HashMap::new();
    for (position, step) in plan.steps.iter().enumerate() {
        if !matches!(
            step.action,
            StepAction::CopyFile { .. } | StepAction::CreateDir { .. }
        ) {
            continue;
        }
        let folded = step.action.target().to_string_lossy().to_lowercase();
        seen.entry(folded).or_default().push(position);
    }
    let clashing: BTreeSet<usize> = seen
        .into_values()
        .filter(|positions| positions.len() > 1)
        .flatten()
        .collect();
    if clashing.is_empty() {
        return;
    }
    let mut kept = Vec::new();
    for (position, step) in std::mem::take(&mut plan.steps).into_iter().enumerate() {
        if clashing.contains(&position) {
            plan.skipped.push(PlanSkip::refused(
                step.rel.clone(),
                Conflict::DestinationIsAnotherSource,
                "two items differ only in letter case and would share one output name",
            ));
        } else {
            kept.push(step);
        }
    }
    plan.steps = kept;
}

/// Drop every step that writes outside the output folder.
fn refuse_escapes(plan: &mut OperationPlan, output: &Path) {
    let steps: Vec<PlanStep> = std::mem::take(&mut plan.steps);
    for step in steps {
        let inside = step
            .action
            .written_paths()
            .iter()
            .all(|written| path_is_within(output, written));
        if inside {
            plan.steps.push(step);
        } else {
            plan.skipped.push(PlanSkip::refused(
                step.rel.clone(),
                Conflict::DestinationIsAnotherSource,
                "the step would write outside the output folder",
            ));
        }
    }
}
