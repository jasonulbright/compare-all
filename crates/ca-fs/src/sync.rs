//! Sync methods as plan generators.
//!
//! A sync method is a table that maps each pair's comparison status to one
//! action. Turning the table over a compared tree produces an ordinary
//! [`OperationPlan`], which is also exactly what a preview grid shows: one row
//! per pair, with the action that pair will receive.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::compare::{Node, NodeStatus};
use crate::criteria::Side;
use crate::ops::plan::{
    entry_of, path_is_within, step, Bases, Conflict, OperationKind, OperationOptions,
    OperationPlan, PlanStep, StepAction,
};

/// What a sync does to one pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SyncAction {
    /// Touch neither side.
    #[default]
    LeaveAlone,
    /// Replace the right side with the left.
    CopyLeftToRight,
    /// Replace the left side with the right.
    CopyRightToLeft,
    /// Remove the left side.
    DeleteLeft,
    /// Remove the right side.
    DeleteRight,
}

impl SyncAction {
    /// True when the action removes something.
    #[must_use]
    pub fn is_delete(self) -> bool {
        matches!(self, Self::DeleteLeft | Self::DeleteRight)
    }

    /// True when the action does nothing.
    #[must_use]
    pub fn is_idle(self) -> bool {
        self == Self::LeaveAlone
    }
}

/// The action chosen for each comparison status.
///
/// A status with no entry is left alone, which keeps a rule table that predates
/// a new status safe rather than surprising.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncRules {
    /// Action per status.
    pub actions: BTreeMap<StatusKey, SyncAction>,
}

/// A comparison status in a form a rule table can key on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum StatusKey {
    /// The two sides match.
    Same,
    /// The two sides differ with no newer side.
    Different,
    /// The left side is newer.
    LeftNewer,
    /// The right side is newer.
    RightNewer,
    /// The item exists only on the left.
    LeftOrphan,
    /// The item exists only on the right.
    RightOrphan,
    /// The name is a file on one side and a folder on the other.
    KindMismatch,
    /// The pair could not be compared.
    Error,
    /// The pair has not been compared.
    NotCompared,
}

impl From<NodeStatus> for StatusKey {
    fn from(status: NodeStatus) -> Self {
        match status {
            NodeStatus::Same => Self::Same,
            NodeStatus::Different => Self::Different,
            NodeStatus::LeftNewer => Self::LeftNewer,
            NodeStatus::RightNewer => Self::RightNewer,
            NodeStatus::LeftOrphan => Self::LeftOrphan,
            NodeStatus::RightOrphan => Self::RightOrphan,
            NodeStatus::KindMismatch => Self::KindMismatch,
            NodeStatus::Error => Self::Error,
            NodeStatus::NotCompared => Self::NotCompared,
        }
    }
}

impl SyncRules {
    /// The action for one status.
    #[must_use]
    pub fn action_for(&self, status: NodeStatus) -> SyncAction {
        self.actions
            .get(&StatusKey::from(status))
            .copied()
            .unwrap_or_default()
    }

    fn from_pairs(pairs: &[(StatusKey, SyncAction)]) -> Self {
        Self {
            actions: pairs.iter().copied().collect(),
        }
    }
}

/// The documented sync methods, plus a table the caller supplies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncPreset {
    /// Copy newer and orphan items from right to left.
    UpdateLeft,
    /// Copy newer and orphan items from left to right.
    UpdateRight,
    /// Copy newer and orphan items in both directions.
    UpdateBoth,
    /// Replace every differing item on the left, and every pair the
    /// comparison could not settle, delete left orphans and copy right
    /// orphans left.
    MirrorToLeft,
    /// Replace every differing item on the right, and every pair the
    /// comparison could not settle, delete right orphans and copy left
    /// orphans right.
    MirrorToRight,
    /// A table saved as a preset of its own.
    Custom(SyncRules),
}

impl SyncPreset {
    /// The rule table this method stands for.
    #[must_use]
    pub fn rules(&self) -> SyncRules {
        use StatusKey as K;
        use SyncAction as A;
        match self {
            Self::UpdateLeft => SyncRules::from_pairs(&[
                (K::RightNewer, A::CopyRightToLeft),
                (K::RightOrphan, A::CopyRightToLeft),
            ]),
            Self::UpdateRight => SyncRules::from_pairs(&[
                (K::LeftNewer, A::CopyLeftToRight),
                (K::LeftOrphan, A::CopyLeftToRight),
            ]),
            Self::UpdateBoth => SyncRules::from_pairs(&[
                (K::LeftNewer, A::CopyLeftToRight),
                (K::LeftOrphan, A::CopyLeftToRight),
                (K::RightNewer, A::CopyRightToLeft),
                (K::RightOrphan, A::CopyRightToLeft),
            ]),
            Self::MirrorToLeft => SyncRules::from_pairs(&[
                (K::Different, A::CopyRightToLeft),
                (K::LeftNewer, A::CopyRightToLeft),
                (K::RightNewer, A::CopyRightToLeft),
                (K::KindMismatch, A::CopyRightToLeft),
                (K::NotCompared, A::CopyRightToLeft),
                (K::RightOrphan, A::CopyRightToLeft),
                (K::LeftOrphan, A::DeleteLeft),
            ]),
            Self::MirrorToRight => SyncRules::from_pairs(&[
                (K::Different, A::CopyLeftToRight),
                (K::LeftNewer, A::CopyLeftToRight),
                (K::RightNewer, A::CopyLeftToRight),
                (K::KindMismatch, A::CopyLeftToRight),
                (K::NotCompared, A::CopyLeftToRight),
                (K::LeftOrphan, A::CopyLeftToRight),
                (K::RightOrphan, A::DeleteRight),
            ]),
            Self::Custom(rules) => rules.clone(),
        }
    }
}

/// One row of a sync preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewRow {
    /// Path relative to the base folders.
    pub rel: PathBuf,
    /// Display name.
    pub name: String,
    /// True when the row stands for a folder.
    pub is_dir: bool,
    /// Comparison status the action was chosen from.
    pub status: NodeStatus,
    /// Action the row will receive.
    pub action: SyncAction,
}

/// Every row a sync would act on, in tree order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncPreview {
    /// Rows, parents before children.
    pub rows: Vec<PreviewRow>,
    /// Rows the caller has overridden, keyed by relative path.
    pub overrides: BTreeMap<PathBuf, SyncAction>,
}

impl SyncPreview {
    /// The action one row will actually receive, after overrides.
    #[must_use]
    pub fn effective(&self, rel: &std::path::Path, planned: SyncAction) -> SyncAction {
        self.overrides.get(rel).copied().unwrap_or(planned)
    }

    /// Override one row's action, which is how a preview excludes an operation
    /// or forces one the rules did not choose.
    pub fn set_override(&mut self, rel: PathBuf, action: SyncAction) {
        self.overrides.insert(rel, action);
    }

    /// Rows that will do something.
    #[must_use]
    pub fn pending(&self) -> Vec<&PreviewRow> {
        self.rows
            .iter()
            .filter(|row| !self.effective(&row.rel, row.action).is_idle())
            .collect()
    }
}

/// Build the preview a sync method produces over a compared tree.
///
/// The tree is taken as the session's filters left it, so a filtered item has
/// no row and is never acted on.
#[must_use]
pub fn preview(root: &Node, preset: &SyncPreset) -> SyncPreview {
    let rules = preset.rules();
    let mut preview = SyncPreview::default();
    collect_rows(root, &rules, true, &mut preview.rows);
    preview
}

fn collect_rows(node: &Node, rules: &SyncRules, is_root: bool, rows: &mut Vec<PreviewRow>) {
    if !is_root {
        rows.push(PreviewRow {
            rel: node.rel.clone(),
            name: node.name.clone(),
            is_dir: node.is_dir,
            status: node.status,
            action: rules.action_for(node.status),
        });
    }
    // Execution treats a linked folder as one item and never walks its target.
    // Offering child overrides here would promise steps that cannot run.
    if is_link_node(node) {
        return;
    }
    for child in &node.children {
        collect_rows(child, rules, false, rows);
    }
}

/// Why a sync produced no plan at all.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SyncRefused {
    /// A scan of one side was cancelled, so the tree is not a picture of either
    /// side and every orphan in it may be an artefact of the interruption.
    #[error("a cancelled scan cannot be synchronised")]
    ScanCancelled,
}

/// Turn a preview into the steps that carry it out.
///
/// Folders are created before the files that go in them and removed after the
/// files they held, and no step is emitted for a path that would leave either
/// base folder.
///
/// # Errors
/// Returns [`SyncRefused::ScanCancelled`] when either side's scan was cut
/// short.
pub fn plan_sync(
    root: &Node,
    preset: &SyncPreset,
    bases: Bases<'_>,
    options: &OperationOptions,
    preview: &SyncPreview,
) -> Result<OperationPlan, SyncRefused> {
    if root.scan_cancelled {
        return Err(SyncRefused::ScanCancelled);
    }
    let rules = preset.rules();
    let mut plan = OperationPlan::new(
        OperationKind::Sync,
        vec![bases.left.to_path_buf(), bases.right.to_path_buf()],
        options.clone(),
    );
    let mut deletions: Vec<PlanStep> = Vec::new();
    let _ = walk(
        root,
        &rules,
        preview,
        bases,
        &mut plan,
        &mut deletions,
        true,
    );
    // The walk queues children before parents for directory removal.
    for removal in deletions {
        plan.steps.push(removal);
    }
    // Filtering before numbering keeps the index dense, which is what the
    // journal and the progress key are read back by.
    plan.steps
        .retain(|candidate| plan_roots_contain(bases, candidate));
    for (position, step) in plan.steps.iter_mut().enumerate() {
        step.index = position;
    }
    Ok(plan)
}

fn plan_roots_contain(bases: Bases<'_>, step: &PlanStep) -> bool {
    step.action
        .written_paths()
        .iter()
        .all(|written| path_is_within(bases.left, written) || path_is_within(bases.right, written))
}

#[allow(clippy::too_many_arguments)]
fn walk(
    node: &Node,
    rules: &SyncRules,
    preview: &SyncPreview,
    bases: Bases<'_>,
    plan: &mut OperationPlan,
    deletions: &mut Vec<PlanStep>,
    is_root: bool,
) -> Option<SyncAction> {
    let action = if is_root {
        SyncAction::LeaveAlone
    } else {
        preview.effective(&node.rel, rules.action_for(node.status))
    };

    if !is_root
        && !action.is_idle()
        && crate::ops::plan::skips_refused(plan, node, &[Side::Left, Side::Right])
    {
        return None;
    }
    if !is_root && !action.is_delete() {
        emit(node, action, bases, plan, deletions);
    }
    let mut all_deleted = !node.incomplete;
    let mark = plan.steps.len();
    // Each child's preview controls its action. A parent's removal must never
    // turn a preserved child into an implicit deletion, including via trash.
    if !is_link_node(node) {
        for child in &node.children {
            let removed = walk(child, rules, preview, bases, plan, deletions, false);
            all_deleted &= removed == Some(action);
        }
    }
    if !is_root && action.is_idle() {
        create_missing_folder(node, bases, plan, mark);
    }
    if action.is_delete() && node.incomplete {
        // Keep the existing refusal diagnostic for unreadable counterparts.
        emit(node, action, bases, plan, deletions);
        None
    } else if action.is_delete() && all_deleted {
        emit(node, action, bases, plan, deletions);
        Some(action)
    } else {
        None
    }
}

/// A folder row the preview leaves alone still has to exist on a side where a
/// child the preview keeps is copied, so it is created ahead of the steps that
/// write into it.
fn create_missing_folder(node: &Node, bases: Bases<'_>, plan: &mut OperationPlan, mark: usize) {
    for (from, to) in [(Side::Left, Side::Right), (Side::Right, Side::Left)] {
        let Some(source) = entry_of(node, from) else {
            continue;
        };
        if !source.is_dir || source.is_link() || entry_of(node, to).is_some() {
            continue;
        }
        let folder = bases.of(to).join(&node.rel);
        let writes_inside = plan.steps.get(mark..).is_some_and(|added| {
            added.iter().any(|queued| {
                queued
                    .action
                    .written_paths()
                    .iter()
                    .any(|written| written.starts_with(&folder) && *written != folder)
            })
        });
        if !writes_inside {
            continue;
        }
        let mut create = step(
            StepAction::CreateDir { path: folder },
            node.rel.clone(),
            Some(to),
        );
        create.expected = crate::ops::plan::expectation(node, Some(from), Some(to));
        plan.steps.insert(mark, create);
    }
}

fn is_link_node(node: &Node) -> bool {
    node.left.as_ref().is_some_and(crate::scan::Entry::is_link)
        || node.right.as_ref().is_some_and(crate::scan::Entry::is_link)
}

fn emit(
    node: &Node,
    action: SyncAction,
    bases: Bases<'_>,
    plan: &mut OperationPlan,
    deletions: &mut Vec<PlanStep>,
) {
    let (from, to) = match action {
        SyncAction::LeaveAlone => return,
        SyncAction::CopyLeftToRight => (Side::Left, Side::Right),
        SyncAction::CopyRightToLeft => (Side::Right, Side::Left),
        SyncAction::DeleteLeft => {
            queue_delete(node, Side::Left, bases, plan, deletions);
            return;
        }
        SyncAction::DeleteRight => {
            queue_delete(node, Side::Right, bases, plan, deletions);
            return;
        }
    };
    if crate::ops::plan::skips_unreachable_name(plan, &node.rel, &node.rel) {
        return;
    }

    let Some(source) = entry_of(node, from) else {
        return;
    };
    let source_path = bases.of(from).join(&node.rel);
    let target_path = bases.of(to).join(&node.rel);

    if source.is_dir {
        if source.is_link() {
            return;
        }
        if node.children.is_empty() && !plan.options.create_empty_folders {
            return;
        }
        let mut create = step(
            StepAction::CreateDir { path: target_path },
            node.rel.clone(),
            Some(to),
        );
        create.expected = crate::ops::plan::expectation(node, Some(from), Some(to));
        plan.steps.push(create);
        return;
    }

    let mut copy = step(
        StepAction::CopyFile {
            source: source_path,
            target: target_path.clone(),
        },
        node.rel.clone(),
        Some(to),
    );
    copy.bytes = source.size;
    copy.conflicts = crate::ops::plan::copy_conflicts(node, from, to);
    copy.expected = crate::ops::plan::expectation(node, Some(from), Some(to));
    copy.backup = plan
        .options
        .backup
        .as_ref()
        .filter(|_| entry_of(node, to).is_some())
        .map(|backup| backup.path_for(&target_path));
    plan.steps.push(copy);
}

fn queue_delete(
    node: &Node,
    side: Side,
    bases: Bases<'_>,
    plan: &mut OperationPlan,
    deletions: &mut Vec<PlanStep>,
) {
    let Some(entry) = entry_of(node, side) else {
        return;
    };
    // A rule that removes an orphan is reading the other side's silence as
    // proof the item is unwanted. Under a listing that was never read in full
    // that silence proves nothing, so the removal is refused and reported.
    if node.incomplete {
        plan.skipped.push(crate::ops::plan::PlanSkip::refused(
            node.rel.clone(),
            Conflict::CounterpartUnreadable,
            "the counterpart could not be read, so nothing here is removed",
        ));
        return;
    }
    let path = bases.of(side).join(&node.rel);
    let mut removal = if entry.is_link() {
        let mut link = step(
            StepAction::DeleteLink { path },
            node.rel.clone(),
            Some(side),
        );
        link.conflicts.push(Conflict::RemovesLinkOnly);
        link
    } else if plan.options.use_recycle_bin {
        step(StepAction::Trash { path }, node.rel.clone(), Some(side))
    } else if entry.is_dir {
        step(StepAction::DeleteDir { path }, node.rel.clone(), Some(side))
    } else {
        step(
            StepAction::DeleteFile { path },
            node.rel.clone(),
            Some(side),
        )
    };
    if entry.attributes.read_only {
        removal.conflicts.push(Conflict::DeleteReadOnly);
    }
    removal.bytes = entry.size;
    let recursive = plan.options.use_recycle_bin && !entry.is_link();
    removal.expected = crate::ops::plan::removal_expectation(node, side, recursive);

    // A trashed folder goes with its contents, so its descendants must not be
    // queued separately.
    if plan.options.use_recycle_bin && entry.is_dir && !entry.is_link() {
        deletions.retain(|queued| !queued.rel.starts_with(&node.rel));
    }
    deletions.push(removal);
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{StatusKey, SyncAction, SyncPreset};
    use crate::compare::NodeStatus;

    #[test]
    fn mirror_to_right_deletes_only_right_orphans() {
        let rules = SyncPreset::MirrorToRight.rules();
        assert_eq!(
            rules.action_for(NodeStatus::RightOrphan),
            SyncAction::DeleteRight
        );
        assert_eq!(
            rules.action_for(NodeStatus::LeftOrphan),
            SyncAction::CopyLeftToRight
        );
        assert_eq!(rules.action_for(NodeStatus::Same), SyncAction::LeaveAlone);
    }

    #[test]
    fn update_methods_never_delete() {
        for preset in [
            SyncPreset::UpdateLeft,
            SyncPreset::UpdateRight,
            SyncPreset::UpdateBoth,
        ] {
            assert!(preset.rules().actions.values().all(|a| !a.is_delete()));
        }
    }

    #[test]
    fn update_both_moves_each_newer_side_outwards() {
        let rules = SyncPreset::UpdateBoth.rules();
        assert_eq!(
            rules.action_for(NodeStatus::LeftNewer),
            SyncAction::CopyLeftToRight
        );
        assert_eq!(
            rules.action_for(NodeStatus::RightNewer),
            SyncAction::CopyRightToLeft
        );
        assert_eq!(
            rules.action_for(NodeStatus::Different),
            SyncAction::LeaveAlone
        );
    }

    #[test]
    fn a_mirror_replaces_a_pair_no_test_could_settle_and_an_update_leaves_it() {
        assert_eq!(
            SyncPreset::MirrorToRight
                .rules()
                .action_for(NodeStatus::NotCompared),
            SyncAction::CopyLeftToRight
        );
        assert_eq!(
            SyncPreset::MirrorToLeft
                .rules()
                .action_for(NodeStatus::NotCompared),
            SyncAction::CopyRightToLeft
        );
        for preset in [
            SyncPreset::UpdateLeft,
            SyncPreset::UpdateRight,
            SyncPreset::UpdateBoth,
        ] {
            assert!(preset.rules().action_for(NodeStatus::NotCompared).is_idle());
        }
    }

    #[test]
    fn an_unlisted_status_is_left_alone() {
        let rules = SyncPreset::Custom(super::SyncRules::default()).rules();
        assert!(rules.action_for(NodeStatus::LeftOrphan).is_idle());
        assert_eq!(StatusKey::from(NodeStatus::Error), StatusKey::Error);
    }
}
