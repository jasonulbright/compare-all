//! Planning. A selection, an operation kind and a set of options become a list
//! of concrete steps.
//!
//! Nothing in this module writes to the file system. Every planner except
//! [`plan_to_folder`] reads only the compared tree the scan already produced.
//! A chosen folder has no listing in that tree, so [`plan_to_folder`] reads
//! the state of each destination through [`FileOps::probe`]. A plan can
//! therefore be shown, edited and discarded with no effect on the disk.

use std::collections::{BTreeSet, HashMap};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::compare::Node;
use crate::criteria::Side;
use crate::ops::fsops::{same_item, AttributeChange, FileOps, Reach, TargetState};

/// How much of a copied file is re-read to prove the copy landed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verify {
    /// Trust the write.
    #[default]
    None,
    /// Compare the destination's size with the source's.
    Size,
    /// Compare a hash of the destination with a hash of the source.
    Hash,
}

/// Where a backup of an overwritten file is written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupOptions {
    /// Folder the backup is written to. `None` keeps it beside the original.
    pub folder: Option<PathBuf>,
    /// Text appended to the original file name.
    pub suffix: String,
}

impl Default for BackupOptions {
    fn default() -> Self {
        Self {
            folder: None,
            suffix: ".bak".to_string(),
        }
    }
}

impl BackupOptions {
    /// The first candidate backup path for one target.
    ///
    /// The name execution settles on is [`BackupOptions::path_for_index`] of
    /// the first free index, because an existing backup is never replaced.
    #[must_use]
    pub fn path_for(&self, target: &Path) -> PathBuf {
        self.path_for_index(target, 0)
    }

    /// The candidate backup path at one position in the numbered sequence.
    ///
    /// Index zero is the plain suffixed name; every later index appends its own
    /// number, so a batch that overwrites one target repeatedly keeps every
    /// generation it displaced.
    #[must_use]
    pub fn path_for_index(&self, target: &Path, index: usize) -> PathBuf {
        let name = target.file_name().map_or_else(
            || String::from("backup"),
            |n| n.to_string_lossy().to_string(),
        );
        let name = if index == 0 {
            format!("{name}{}", self.suffix)
        } else {
            format!("{name}{}{index}", self.suffix)
        };
        match (&self.folder, target.parent()) {
            (Some(folder), _) => folder.join(name),
            (None, Some(parent)) => parent.join(name),
            (None, None) => PathBuf::from(name),
        }
    }
}

/// Settings every destructive operation reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationOptions {
    /// Replace an existing item at the destination. Copy and move do this
    /// without a separate opt-in; clearing it turns every occupied target into
    /// a skipped step.
    pub overwrite: bool,
    /// Give the destination the source's last modification time. A record
    /// copied into a zip takes it with the write the open batch queued for
    /// the record, at a whole second.
    pub preserve_modified: bool,
    /// Give the destination the source's creation time where the platform
    /// stores one.
    pub preserve_created: bool,
    /// Give the destination the source's attributes.
    pub preserve_attributes: bool,
    /// After a copy, give the source the time the destination actually
    /// received, for a destination that records a time of its own.
    pub touch_source_after_copy: bool,
    /// How a finished copy is checked.
    pub verify: Verify,
    /// Send deletions to the platform's trash instead of removing them.
    pub use_recycle_bin: bool,
    /// Clear a target's read-only flag rather than failing the step.
    pub clear_read_only_targets: bool,
    /// Take a copy of a file that is about to be overwritten.
    pub backup: Option<BackupOptions>,
    /// Act on hidden items even where the session's filters would drop them.
    /// Clearing it leaves a hidden item out unless the selection names it.
    pub include_hidden: bool,
    /// Create a destination folder whose contents are empty or entirely
    /// filtered out.
    pub create_empty_folders: bool,
    /// Bytes moved per read and write during a copy, and per cancellation
    /// check.
    pub buffer_size: usize,
}

impl Default for OperationOptions {
    fn default() -> Self {
        Self {
            overwrite: true,
            preserve_modified: true,
            preserve_created: false,
            preserve_attributes: false,
            touch_source_after_copy: false,
            verify: Verify::None,
            use_recycle_bin: true,
            clear_read_only_targets: false,
            backup: None,
            include_hidden: true,
            create_empty_folders: false,
            buffer_size: 256 * 1024,
        }
    }
}

/// Which sides an operation reads or writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Sides {
    /// The left side only.
    Left,
    /// The right side only.
    Right,
    /// Both sides at once.
    Both,
}

impl Sides {
    /// The individual sides this selection covers.
    #[must_use]
    pub fn each(self) -> &'static [Side] {
        match self {
            Self::Left => &[Side::Left],
            Self::Right => &[Side::Right],
            Self::Both => &[Side::Left, Side::Right],
        }
    }
}

/// Where a copy or move puts the path information of its sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PathOption {
    /// Recreate the shortest path that still tells the selected items apart.
    KeepRelative,
    /// Recreate the whole path back to the base folder.
    KeepBase,
    /// Discard path information; every item lands directly in the target.
    Flatten,
}

/// Which timestamp a touch writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchSpec {
    /// Take each item's timestamp from its counterpart on the other side.
    FromOtherSide,
    /// Write one chosen timestamp to every selected item.
    Explicit(SystemTime),
}

/// How a rename computes new names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameAction {
    /// A mask in which `?` copies one character from the old name and `*`
    /// copies the rest of the segment.
    Mask(String),
    /// A regular expression matched against the old name and a template that
    /// builds the new one, with `$1` style capture references.
    Regex {
        /// Expression matched against the old name.
        find: String,
        /// Template the new name is built from.
        replace: String,
    },
}

/// The kind of operation a plan carries out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    /// Copy to the opposite side.
    Copy,
    /// Move to the opposite side.
    Move,
    /// Copy into a chosen folder.
    CopyToFolder,
    /// Move into a chosen folder.
    MoveToFolder,
    /// Remove selected items.
    Delete,
    /// Give selected items new names in place.
    Rename,
    /// Change selected items' timestamps.
    Touch,
    /// Change selected items' attributes.
    Attributes,
    /// Create one folder.
    NewFolder,
    /// Move each side's selection to the other side.
    Exchange,
    /// Reconcile two trees under one rule set.
    Sync,
    /// Write the result of a three way folder merge into its output folder.
    Merge,
}

/// One concrete thing a step does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", rename_all_fields = "snake_case")]
pub enum StepAction {
    /// Create one directory, parents first.
    CreateDir {
        /// Directory to create.
        path: PathBuf,
    },
    /// Copy one file's contents over a destination path.
    CopyFile {
        /// File read from.
        source: PathBuf,
        /// File written, through a temporary name in its own directory.
        target: PathBuf,
    },
    /// Move one file, by rename where the volumes allow it and by a verified
    /// copy followed by a delete where they do not.
    MoveFile {
        /// File read from.
        source: PathBuf,
        /// File written.
        target: PathBuf,
    },
    /// Remove one file.
    DeleteFile {
        /// File to remove.
        path: PathBuf,
    },
    /// Remove one directory, which the preceding steps have emptied.
    DeleteDir {
        /// Directory to remove.
        path: PathBuf,
    },
    /// Remove a link without touching what it points at.
    DeleteLink {
        /// Link to remove.
        path: PathBuf,
    },
    /// Hand one item to the platform's trash, contents and all.
    Trash {
        /// Item to send to the trash.
        path: PathBuf,
    },
    /// Give one item a new name in its own directory.
    Rename {
        /// Current path.
        from: PathBuf,
        /// New path.
        to: PathBuf,
    },
    /// Write timestamps to one item.
    SetTimes {
        /// Item to touch.
        path: PathBuf,
        /// New last modification time.
        modified: Option<SystemTime>,
        /// New creation time.
        created: Option<SystemTime>,
    },
    /// Write attributes to one item.
    SetAttributes {
        /// Item to change.
        path: PathBuf,
        /// Attributes to write.
        change: AttributeChange,
    },
    /// Swap two files that occupy the same relative path on the two sides.
    ///
    /// The two paths are read and written as one unit: no ordering of plain
    /// moves can swap them without one of the two passing through the other's
    /// name, which destroys it.
    ExchangeFiles {
        /// Left side path.
        left: PathBuf,
        /// Right side path.
        right: PathBuf,
    },
}

impl StepAction {
    /// The path the step writes to, which is the path a failure leaves in an
    /// unknown state.
    #[must_use]
    pub fn target(&self) -> &Path {
        match self {
            Self::CreateDir { path }
            | Self::DeleteFile { path }
            | Self::DeleteDir { path }
            | Self::DeleteLink { path }
            | Self::Trash { path }
            | Self::SetTimes { path, .. }
            | Self::SetAttributes { path, .. } => path,
            Self::CopyFile { target, .. } | Self::MoveFile { target, .. } => target,
            Self::Rename { to, .. } => to,
            Self::ExchangeFiles { right, .. } => right,
        }
    }

    /// The path the step reads from, when it reads one.
    #[must_use]
    pub fn source(&self) -> Option<&Path> {
        match self {
            Self::CopyFile { source, .. } | Self::MoveFile { source, .. } => Some(source),
            Self::Rename { from, .. } => Some(from),
            Self::ExchangeFiles { left, .. } => Some(left),
            _ => None,
        }
    }

    /// Every path the step writes to, which is what containment is judged on.
    #[must_use]
    pub fn written_paths(&self) -> Vec<&Path> {
        match self {
            Self::ExchangeFiles { left, right } => vec![left.as_path(), right.as_path()],
            other => vec![other.target()],
        }
    }

    /// True when the step removes something.
    #[must_use]
    pub fn is_destructive(&self) -> bool {
        matches!(
            self,
            Self::DeleteFile { .. }
                | Self::DeleteDir { .. }
                | Self::DeleteLink { .. }
                | Self::Trash { .. }
        )
    }
}

/// Something about a step the caller may want to confirm before it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Conflict {
    /// An item already exists at the destination and will be replaced.
    TargetExists,
    /// The item being replaced is newer than the one replacing it.
    OverwriteNewer,
    /// The destination is marked read-only.
    TargetReadOnly,
    /// The destination is hidden or marked as a system item.
    TargetHiddenOrSystem,
    /// The item being removed is marked read-only.
    DeleteReadOnly,
    /// The name is a file on one side and a folder on the other.
    KindMismatch,
    /// The old and new names differ only in case, which needs two renames on a
    /// file system that ignores case.
    CaseOnlyRename,
    /// The item being removed is a link; the link goes, its target stays.
    RemovesLinkOnly,
    /// The other side could not be read in full, so the item's absence there is
    /// not evidence and no removal or replacement rests on it.
    CounterpartUnreadable,
    /// The disk no longer matches what the plan was built from.
    Drift,
    /// The destination of one step is the source of another in the same batch.
    DestinationIsAnotherSource,
}

/// Why a selected item produced no step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanSkip {
    /// Path the selection named.
    pub path: PathBuf,
    /// Reason no step was produced.
    pub reason: String,
    /// The conflict the refusal stands for, when it has a typed form.
    pub conflict: Option<Conflict>,
}

impl PlanSkip {
    /// A refusal that carries a typed conflict.
    #[must_use]
    pub fn refused(path: PathBuf, conflict: Conflict, reason: impl Into<String>) -> Self {
        Self {
            path,
            reason: reason.into(),
            conflict: Some(conflict),
        }
    }

    /// A plain note that a selected item produced nothing.
    #[must_use]
    pub fn noted(path: PathBuf, reason: impl Into<String>) -> Self {
        Self {
            path,
            reason: reason.into(),
            conflict: None,
        }
    }
}

/// What the plan believed one path held when it was built.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathState {
    /// True when something was there.
    pub exists: bool,
    /// True when that something was a directory.
    pub is_dir: bool,
    /// True when that something was a link.
    pub is_link: bool,
    /// Size in bytes; zero for directories.
    pub size: u64,
    /// Last modification time, where one was reported.
    pub modified: Option<SystemTime>,
    /// Items counted at every depth under a directory the plan removes with
    /// its contents. A link is counted and not entered.
    pub children: Option<usize>,
}

impl PathState {
    /// The absent state.
    #[must_use]
    pub fn absent() -> Self {
        Self::default()
    }

    fn of(entry: &crate::scan::Entry) -> Self {
        Self {
            exists: true,
            is_dir: entry.is_dir,
            is_link: entry.is_link(),
            size: entry.size,
            modified: entry.modified,
            children: None,
        }
    }

    fn found(state: &TargetState) -> Self {
        Self {
            exists: true,
            is_dir: state.is_dir,
            is_link: state.is_link,
            size: state.size,
            modified: state.modified,
            children: None,
        }
    }
}

/// The state the plan expects to find immediately before a step acts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepExpectation {
    /// State of the step's source, where it reads one.
    pub source: Option<PathState>,
    /// State of the step's target.
    pub target: Option<PathState>,
}

/// One step of a plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanStep {
    /// Position in the plan, and the key the journal records the step under.
    pub index: usize,
    /// What the step does.
    pub action: StepAction,
    /// Bytes the step moves; zero for everything but a copy or move.
    pub bytes: u64,
    /// What the caller may want to confirm before the step runs.
    pub conflicts: Vec<Conflict>,
    /// Path of the file replaced by this step, saved before it is replaced.
    pub backup: Option<PathBuf>,
    /// Side the step writes to, where the operation has one.
    pub side: Option<PlanSide>,
    /// Path relative to the base folder the step's target sits under.
    pub rel: PathBuf,
    /// What the plan believed the step's paths held, re-checked before the step
    /// acts so a disk that moved on is not acted on blind.
    pub expected: StepExpectation,
}

/// A serialisable stand-in for [`Side`], which the comparison crate keeps free
/// of serialisation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanSide {
    /// The left base folder.
    Left,
    /// The right base folder.
    Right,
}

impl From<Side> for PlanSide {
    fn from(side: Side) -> Self {
        match side {
            Side::Left => Self::Left,
            Side::Right => Self::Right,
        }
    }
}

/// Every step one operation would carry out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationPlan {
    /// Operation the plan was built for.
    pub kind: OperationKind,
    /// Steps, in the order they must run.
    pub steps: Vec<PlanStep>,
    /// Selected items that produced no step.
    pub skipped: Vec<PlanSkip>,
    /// Folders every path in the plan must stay inside.
    pub roots: Vec<PathBuf>,
    /// Options the plan was built with, which execution reads again.
    pub options: OperationOptions,
}

impl OperationPlan {
    /// An empty plan for one kind of operation.
    #[must_use]
    pub fn new(kind: OperationKind, roots: Vec<PathBuf>, options: OperationOptions) -> Self {
        Self {
            kind,
            steps: Vec::new(),
            skipped: Vec::new(),
            roots,
            options,
        }
    }

    /// Total bytes the plan moves.
    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        self.steps.iter().map(|step| step.bytes).sum()
    }

    /// Every conflict the plan raises, without duplicates.
    #[must_use]
    pub fn conflicts(&self) -> BTreeSet<Conflict> {
        self.steps
            .iter()
            .flat_map(|step| step.conflicts.iter().copied())
            .collect()
    }

    /// True when no step writes outside [`OperationPlan::roots`].
    #[must_use]
    pub fn is_contained(&self) -> bool {
        self.steps.iter().all(|step| {
            step.action
                .written_paths()
                .iter()
                .all(|written| self.roots.iter().any(|root| path_is_within(root, written)))
        })
    }

    /// Refusals that carry a typed conflict, which is what a caller shows
    /// instead of the destructive step the plan declined to build.
    #[must_use]
    pub fn refusals(&self) -> Vec<&PlanSkip> {
        self.skipped
            .iter()
            .filter(|skip| skip.conflict.is_some())
            .collect()
    }

    /// Drop the steps whose index is in `indexes`, renumbering what is left.
    ///
    /// This is how a preview removes an operation from a pending batch.
    pub fn exclude(&mut self, indexes: &BTreeSet<usize>) {
        self.steps.retain(|step| !indexes.contains(&step.index));
        for (position, step) in self.steps.iter_mut().enumerate() {
            step.index = position;
        }
    }

    fn push(&mut self, mut step: PlanStep) {
        step.index = self.steps.len();
        self.steps.push(step);
    }
}

/// Drop every step whose target sits in a source that cannot carry it out, and
/// record a typed refusal in its place.
///
/// This runs while the plan is still a plan. A container format this build
/// only reads, a recorded listing and a remote location that refuses writes
/// all lose their steps here, so no batch starts and stops half way through
/// against a source that was never going to accept it.
///
/// Every copy writes under a temporary name and then renames it onto the
/// target, so a source that accepts writes and refuses the rename fails the
/// step at the rename and the target keeps its content.
pub fn refuse_unsupported_targets(plan: &mut OperationPlan, mounts: &[crate::ops::vfsops::Mount]) {
    let mut kept: Vec<PlanStep> = Vec::new();
    for step in std::mem::take(&mut plan.steps) {
        let refusal = step.action.written_paths().into_iter().find_map(|written| {
            mounts
                .iter()
                .find(|mount| written.starts_with(&mount.prefix))
                .and_then(|mount| {
                    crate::ops::vfsops::write_refusal(&mount.source)
                        .map(|reason| (written.to_path_buf(), reason))
                })
        });
        match refusal {
            Some((path, reason)) => {
                plan.skipped
                    .push(PlanSkip::refused(path, Conflict::TargetReadOnly, reason));
            }
            None => kept.push(step),
        }
    }
    plan.steps = kept;
    for (position, step) in plan.steps.iter_mut().enumerate() {
        step.index = position;
    }
}

/// Where the plan's paths come from.
#[derive(Debug, Clone, Copy)]
pub struct Bases<'a> {
    /// Left base folder.
    pub left: &'a Path,
    /// Right base folder.
    pub right: &'a Path,
}

impl<'a> Bases<'a> {
    /// The base folder of one side.
    #[must_use]
    pub fn of(&self, side: Side) -> &'a Path {
        match side {
            Side::Left => self.left,
            Side::Right => self.right,
        }
    }
}

/// True when `candidate` is `root` or sits under it, judged on the path text
/// alone so the answer never depends on the disk.
///
/// A `..` component that would climb out of `root` makes the answer false even
/// when the components after it climb back in, because the intermediate path
/// may be a link.
#[must_use]
pub fn path_is_within(root: &Path, candidate: &Path) -> bool {
    let Some(root) = lexical_normalize(root) else {
        return false;
    };
    let Some(candidate) = lexical_normalize(candidate) else {
        return false;
    };
    candidate.starts_with(&root)
}

/// Resolve `.` components and reject any path that climbs above its own root.
fn lexical_normalize(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    let mut depth = 0usize;
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if depth == 0 {
                    return None;
                }
                depth -= 1;
                out.pop();
            }
            Component::Normal(part) => {
                depth += 1;
                out.push(part);
            }
            Component::Prefix(_) | Component::RootDir => out.push(component.as_os_str()),
        }
    }
    Some(out)
}

/// Reduce a selection to the items the operations actually act on, carrying
/// hidden items with it.
///
/// A selected folder stands for its contents, so it contributes its own
/// descendants; and when a folder and any of its descendants are both selected
/// the folder's own selection is dropped, leaving only the descendants.
#[must_use]
pub fn resolve_selection<'a>(root: &'a Node, selected: &BTreeSet<PathBuf>) -> Vec<&'a Node> {
    resolve_selection_with(root, selected, true)
}

/// The same reduction, with [`OperationOptions::include_hidden`] applied.
///
/// A hidden item the selection reaches only through a selected folder is left
/// out when `include_hidden` is clear. An item named in `selected` is always
/// acted on, because leaving out what the user picked by hand would make the
/// plan disagree with the selection it was built from.
#[must_use]
pub fn resolve_selection_with<'a>(
    root: &'a Node,
    selected: &BTreeSet<PathBuf>,
    include_hidden: bool,
) -> Vec<&'a Node> {
    let mut out = Vec::new();
    collect_selected(root, selected, false, include_hidden, &mut out);
    out
}

/// True when a descendant reached through a selected folder is left out because
/// it is hidden.
fn skips_hidden(options: &OperationOptions, node: &Node) -> bool {
    !options.include_hidden && is_hidden(node)
}

/// True when the pair exists and every side of it carries the hidden flag.
fn is_hidden(node: &Node) -> bool {
    let sides = [node.left.as_ref(), node.right.as_ref()];
    let mut present = sides.iter().flatten().peekable();
    present.peek().is_some() && present.all(|entry| entry.attributes.hidden)
}

fn collect_selected<'a>(
    node: &'a Node,
    selected: &BTreeSet<PathBuf>,
    ancestor_selected: bool,
    include_hidden: bool,
    out: &mut Vec<&'a Node>,
) {
    let here = selected.contains(&node.rel);
    if !here && ancestor_selected && !include_hidden && is_hidden(node) {
        return;
    }
    let covered = here || ancestor_selected;
    if covered && !has_selected_descendant(node, selected) {
        out.push(node);
        return;
    }
    // Reaching here means a descendant is selected, which cancels this node's
    // own selection and any it inherited.
    for child in &node.children {
        collect_selected(child, selected, false, include_hidden, out);
    }
}

fn has_selected_descendant(node: &Node, selected: &BTreeSet<PathBuf>) -> bool {
    node.children
        .iter()
        .any(|child| selected.contains(&child.rel) || has_selected_descendant(child, selected))
}

pub(crate) fn entry_of(node: &Node, side: Side) -> Option<&crate::scan::Entry> {
    match side {
        Side::Left => node.left.as_ref(),
        Side::Right => node.right.as_ref(),
    }
}

/// Record `node` as left alone when its entry on any of `sides` is refused,
/// and say whether it was.
///
/// A refused row lists an item its source does not open. For a local folder
/// the row's path names nothing, and the stored name reaches another item or
/// a device, so no step is given either path.
pub(crate) fn skips_refused(plan: &mut OperationPlan, node: &Node, sides: &[Side]) -> bool {
    let refused = sides
        .iter()
        .filter_map(|side| entry_of(node, *side))
        .find(|entry| entry.refused);
    let Some(entry) = refused else {
        return false;
    };
    plan.skipped.push(PlanSkip::noted(
        node.rel.clone(),
        entry
            .error
            .clone()
            .unwrap_or_else(|| "the source refuses to open this entry".to_owned()),
    ));
    true
}

/// Record `rel` as left alone when a component of `dest_rel` is a name that a
/// path of this platform does not reach as itself, and say whether it was.
///
/// On Windows a path drops a dot or a space at the end of a component and
/// reads a DOS device name as the device. A container or a remote location
/// stores such a name as it stands, so a step that wrote it into a folder
/// would write another item or the device. The reason is the one the folder
/// scan gives for such a name.
pub(crate) fn skips_unreachable_name(
    plan: &mut OperationPlan,
    rel: &Path,
    dest_rel: &Path,
) -> bool {
    let reason = dest_rel.components().find_map(|component| match component {
        Component::Normal(name) => ca_vfs::platform_refusal(name).map(|(_, reason)| reason),
        _ => None,
    });
    let Some(reason) = reason else {
        return false;
    };
    plan.skipped
        .push(PlanSkip::noted(rel.to_path_buf(), reason));
    true
}

pub(crate) fn copy_conflicts(node: &Node, from: Side, to: Side) -> Vec<Conflict> {
    match (entry_of(node, from), entry_of(node, to)) {
        (Some(source), Some(target)) => replacement_conflicts(source, target),
        _ => Vec::new(),
    }
}

/// What the caller may want to confirm before `source` replaces `target`.
fn replacement_conflicts(
    source: &crate::scan::Entry,
    target: &crate::scan::Entry,
) -> Vec<Conflict> {
    let mut conflicts = vec![Conflict::TargetExists];
    if source.is_dir != target.is_dir {
        conflicts.push(Conflict::KindMismatch);
    }
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

/// What the caller may want to confirm before `source` replaces an item that
/// a probe of the destination found, as [`replacement_conflicts`] says for an
/// item the scan listed.
fn probed_conflicts(source: &crate::scan::Entry, target: &TargetState) -> Vec<Conflict> {
    let mut conflicts = vec![Conflict::TargetExists];
    if source.is_dir != target.is_dir {
        conflicts.push(Conflict::KindMismatch);
    }
    if let (Some(source_time), Some(target_time)) = (source.modified, target.modified) {
        if target_time > source_time {
            conflicts.push(Conflict::OverwriteNewer);
        }
    }
    if target.read_only {
        conflicts.push(Conflict::TargetReadOnly);
    }
    if target.hidden {
        conflicts.push(Conflict::TargetHiddenOrSystem);
    }
    conflicts
}

fn delete_conflicts(entry: &crate::scan::Entry) -> Vec<Conflict> {
    let mut conflicts = Vec::new();
    if entry.attributes.read_only {
        conflicts.push(Conflict::DeleteReadOnly);
    }
    if entry.is_link() {
        conflicts.push(Conflict::RemovesLinkOnly);
    }
    conflicts
}

pub(crate) fn step(action: StepAction, rel: PathBuf, side: Option<Side>) -> PlanStep {
    PlanStep {
        index: 0,
        action,
        bytes: 0,
        conflicts: Vec::new(),
        backup: None,
        side: side.map(PlanSide::from),
        rel,
        expected: StepExpectation::default(),
    }
}

/// The expectation for a step that reads one side of a pair and writes the
/// other.
pub(crate) fn expectation(node: &Node, from: Option<Side>, to: Option<Side>) -> StepExpectation {
    StepExpectation {
        source: from.map(|side| entry_of(node, side).map_or_else(PathState::absent, PathState::of)),
        target: to.map(|side| entry_of(node, side).map_or_else(PathState::absent, PathState::of)),
    }
}

/// The expectation for a removal.
///
/// A removal that takes a folder's contents with it records how many children
/// the folder held, so one that gained content between planning and acting is
/// not carried away on the strength of the old listing. A removal that only
/// unlinks an already emptied folder records no count, because the preceding
/// steps are what emptied it.
/// Items on `side` at every depth under `node`. A link is counted and not
/// entered, as the removal does not enter it.
fn subtree_items(node: &Node, side: Side) -> usize {
    let mut count = 0usize;
    let mut stack: Vec<&Node> = vec![node];
    while let Some(folder) = stack.pop() {
        for child in &folder.children {
            let Some(entry) = entry_of(child, side) else {
                continue;
            };
            count = count.saturating_add(1);
            if entry.is_dir && !entry.is_link() {
                stack.push(child);
            }
        }
    }
    count
}

pub(crate) fn removal_expectation(node: &Node, side: Side, recursive: bool) -> StepExpectation {
    let mut target = entry_of(node, side).map_or_else(PathState::absent, PathState::of);
    if recursive && target.is_dir && !target.is_link {
        target.children = Some(subtree_items(node, side));
    }
    StepExpectation {
        source: None,
        target: Some(target),
    }
}

/// Append the steps that put `node`'s subtree from `from` onto `to` under
/// `target_root`, with the destination path built from `rel_for`.
pub(crate) struct Transfer<'a> {
    pub from: Side,
    pub to: Side,
    pub source_root: &'a Path,
    pub target_root: &'a Path,
    pub moving: bool,
}

pub(crate) fn plan_copy_subtree(
    plan: &mut OperationPlan,
    node: &Node,
    transfer: &Transfer<'_>,
    dest_rel: &Path,
) {
    let Transfer {
        from,
        to,
        source_root,
        target_root,
        moving,
    } = *transfer;
    if skips_refused(plan, node, &[from, to]) || skips_unreachable_name(plan, &node.rel, dest_rel) {
        return;
    }
    let Some(source) = entry_of(node, from) else {
        plan.skipped.push(PlanSkip::noted(
            node.rel.clone(),
            format!("no item on the {from:?} side"),
        ));
        return;
    };
    let source_path = source_root.join(&node.rel);
    let target_path = target_root.join(dest_rel);

    if source.is_dir {
        if source.is_link() {
            // A link is carried or removed as a link; its target lies outside
            // the tree the operation was approved over.
            plan.skipped.push(PlanSkip::refused(
                node.rel.clone(),
                Conflict::RemovesLinkOnly,
                "a directory link is not descended into",
            ));
            return;
        }
        if node.children.is_empty() && !plan.options.create_empty_folders {
            return;
        }
        let mut dir_step = step(
            StepAction::CreateDir {
                path: target_path.clone(),
            },
            dest_rel.to_path_buf(),
            Some(to),
        );
        dir_step.conflicts = copy_conflicts(node, from, to)
            .into_iter()
            .filter(|conflict| *conflict == Conflict::KindMismatch)
            .collect();
        dir_step.expected = expectation(node, Some(from), Some(to));
        plan.push(dir_step);
        for child in &node.children {
            if skips_hidden(&plan.options, child) {
                continue;
            }
            plan_copy_subtree(plan, child, transfer, &dest_rel.join(&child.name));
        }
        if moving {
            if node.incomplete {
                plan.skipped.push(PlanSkip::refused(
                    node.rel.clone(),
                    Conflict::CounterpartUnreadable,
                    "the source folder's listing is partial, so it is not removed",
                ));
                return;
            }
            let mut remove = step(
                StepAction::DeleteDir {
                    path: source_path.clone(),
                },
                node.rel.clone(),
                Some(from),
            );
            remove.conflicts = delete_conflicts(source);
            remove.expected = removal_expectation(node, from, false);
            plan.push(remove);
        }
        return;
    }

    let action = if moving {
        StepAction::MoveFile {
            source: source_path,
            target: target_path.clone(),
        }
    } else {
        StepAction::CopyFile {
            source: source_path,
            target: target_path.clone(),
        }
    };
    let mut file_step = step(action, dest_rel.to_path_buf(), Some(to));
    file_step.bytes = source.size;
    file_step.conflicts = copy_conflicts(node, from, to);
    file_step.expected = expectation(node, Some(from), Some(to));
    file_step.backup = plan
        .options
        .backup
        .as_ref()
        .filter(|_| entry_of(node, to).is_some())
        .map(|backup| backup.path_for(&target_path));
    plan.push(file_step);
}

/// Append the steps that remove `node`'s subtree from one side.
///
/// Children are removed before their parent, and a link is removed as a link
/// so nothing outside the tree is reached through it.
pub(crate) fn plan_delete_subtree(plan: &mut OperationPlan, node: &Node, side: Side, root: &Path) {
    if skips_refused(plan, node, &[side]) {
        return;
    }
    let Some(entry) = entry_of(node, side) else {
        return;
    };
    let path = root.join(&node.rel);

    if entry.is_link() {
        let mut link_step = step(
            StepAction::DeleteLink { path },
            node.rel.clone(),
            Some(side),
        );
        link_step.conflicts = delete_conflicts(entry);
        link_step.expected = removal_expectation(node, side, false);
        plan.push(link_step);
        return;
    }

    // Removing a folder removes everything under it, so a folder whose listing
    // is partial would carry away items no one was shown.
    if entry.is_dir && node.incomplete {
        plan.skipped.push(PlanSkip::refused(
            node.rel.clone(),
            Conflict::CounterpartUnreadable,
            "the folder's listing is partial, so it is not removed",
        ));
        return;
    }

    if plan.options.use_recycle_bin {
        let mut trash_step = step(StepAction::Trash { path }, node.rel.clone(), Some(side));
        trash_step.conflicts = delete_conflicts(entry);
        trash_step.bytes = entry.size;
        trash_step.expected = removal_expectation(node, side, true);
        plan.push(trash_step);
        return;
    }

    if entry.is_dir {
        for child in &node.children {
            if skips_hidden(&plan.options, child) {
                continue;
            }
            plan_delete_subtree(plan, child, side, root);
        }
        let mut dir_step = step(StepAction::DeleteDir { path }, node.rel.clone(), Some(side));
        dir_step.conflicts = delete_conflicts(entry);
        dir_step.expected = removal_expectation(node, side, false);
        plan.push(dir_step);
        return;
    }

    let mut file_step = step(
        StepAction::DeleteFile { path },
        node.rel.clone(),
        Some(side),
    );
    file_step.bytes = entry.size;
    file_step.conflicts = delete_conflicts(entry);
    file_step.expected = removal_expectation(node, side, false);
    plan.push(file_step);
}

/// Plan a copy of the selected items onto the opposite side.
#[must_use]
pub fn plan_copy(
    selection: &[&Node],
    from: Side,
    bases: Bases<'_>,
    options: &OperationOptions,
) -> OperationPlan {
    plan_transfer(selection, from, bases, options, OperationKind::Copy, false)
}

/// Plan a move of the selected items onto the opposite side.
#[must_use]
pub fn plan_move(
    selection: &[&Node],
    from: Side,
    bases: Bases<'_>,
    options: &OperationOptions,
) -> OperationPlan {
    plan_transfer(selection, from, bases, options, OperationKind::Move, true)
}

fn plan_transfer(
    selection: &[&Node],
    from: Side,
    bases: Bases<'_>,
    options: &OperationOptions,
    kind: OperationKind,
    moving: bool,
) -> OperationPlan {
    let to = opposite(from);
    let mut plan = OperationPlan::new(
        kind,
        vec![bases.left.to_path_buf(), bases.right.to_path_buf()],
        options.clone(),
    );
    let transfer = Transfer {
        from,
        to,
        source_root: bases.of(from),
        target_root: bases.of(to),
        moving,
    };
    for node in selection {
        let rel = node.rel.clone();
        plan_copy_subtree(&mut plan, node, &transfer, &rel);
    }
    plan
}

/// Plan an exchange: each side's selection moves to the other side.
#[must_use]
pub fn plan_exchange(
    left_selection: &[&Node],
    right_selection: &[&Node],
    bases: Bases<'_>,
    options: &OperationOptions,
) -> OperationPlan {
    let mut plan = OperationPlan::new(
        OperationKind::Exchange,
        vec![bases.left.to_path_buf(), bases.right.to_path_buf()],
        options.clone(),
    );
    // A path selected on both sides cannot be swapped by two independent moves:
    // whichever runs first destroys the other's source.
    let both: BTreeSet<PathBuf> = left_selection
        .iter()
        .map(|node| node.rel.clone())
        .filter(|rel| right_selection.iter().any(|node| node.rel == *rel))
        .collect();

    for node in left_selection
        .iter()
        .filter(|node| both.contains(&node.rel))
    {
        if skips_refused(&mut plan, node, &[Side::Left, Side::Right]) {
            continue;
        }
        let (Some(left), Some(right)) = (entry_of(node, Side::Left), entry_of(node, Side::Right))
        else {
            continue;
        };
        if left.is_dir || right.is_dir || left.is_link() || right.is_link() {
            plan.skipped.push(PlanSkip::refused(
                node.rel.clone(),
                Conflict::KindMismatch,
                "only two plain files at one path can be exchanged in place",
            ));
            continue;
        }
        let mut swap = step(
            StepAction::ExchangeFiles {
                left: bases.left.join(&node.rel),
                right: bases.right.join(&node.rel),
            },
            node.rel.clone(),
            None,
        );
        swap.bytes = left.size.saturating_add(right.size);
        swap.conflicts.push(Conflict::TargetExists);
        swap.expected = expectation(node, Some(Side::Left), Some(Side::Right));
        plan.push(swap);
    }

    for (selection, from) in [(left_selection, Side::Left), (right_selection, Side::Right)] {
        let transfer = Transfer {
            from,
            to: opposite(from),
            source_root: bases.of(from),
            target_root: bases.of(opposite(from)),
            moving: true,
        };
        for node in selection.iter().filter(|node| !both.contains(&node.rel)) {
            let rel = node.rel.clone();
            plan_copy_subtree(&mut plan, node, &transfer, &rel);
        }
    }
    plan
}

/// The shortest prefix that every selected path shares, which
/// [`PathOption::KeepRelative`] strips.
#[must_use]
pub fn common_parent(paths: &[PathBuf]) -> PathBuf {
    let mut iter = paths.iter();
    let Some(first) = iter.next() else {
        return PathBuf::new();
    };
    let mut prefix: Vec<_> = first
        .parent()
        .unwrap_or(Path::new(""))
        .components()
        .collect();
    for path in iter {
        let other: Vec<_> = path
            .parent()
            .unwrap_or(Path::new(""))
            .components()
            .collect();
        let keep = prefix
            .iter()
            .zip(other.iter())
            .take_while(|(a, b)| a == b)
            .count();
        prefix.truncate(keep);
    }
    prefix.iter().collect()
}

/// Why an item whose destination is the item itself produces no step.
pub const ONTO_ITSELF: &str = "the destination is the item itself";

/// Why an item whose destination may be the item itself, reached under
/// another path, produces no step.
pub const MAYBE_ITSELF: &str = "the destination may be the item itself through another path: \
                                identity and file-state checks are inconclusive";

/// Why a copy or a move whose source and destination reach as `reach` gives
/// no step, or `None` when the two are two items.
pub(crate) fn onto_itself(reach: Reach) -> Option<&'static str> {
    match reach {
        Reach::TwoItems => None,
        Reach::OneItem => Some(ONTO_ITSELF),
        Reach::Unproven => Some(MAYBE_ITSELF),
    }
}

/// Plan a copy or move of one side's selection into a chosen folder.
///
/// The compared tree holds no listing of the chosen folder, so each
/// destination is read through `fs` while the plan is built. An item found
/// at a destination is a conflict of the step, with the conflicts, backup and
/// recorded state a copy onto an existing file has; a destination found free
/// is recorded as absent, so an item that takes the name before the step
/// runs is drift. A destination that resolves to the item itself gives no
/// step, because a move would copy the item onto itself and then remove it.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn plan_to_folder(
    selection: &[&Node],
    from: Side,
    bases: Bases<'_>,
    target_root: &Path,
    path_option: PathOption,
    options: &OperationOptions,
    moving: bool,
    fs: &dyn FileOps,
) -> OperationPlan {
    let kind = if moving {
        OperationKind::MoveToFolder
    } else {
        OperationKind::CopyToFolder
    };
    let mut plan = OperationPlan::new(
        kind,
        vec![bases.of(from).to_path_buf(), target_root.to_path_buf()],
        options.clone(),
    );
    let rels: Vec<PathBuf> = selection.iter().map(|node| node.rel.clone()).collect();
    let trimmed = common_parent(&rels);

    let destinations: Vec<PathBuf> = selection
        .iter()
        .map(|node| match path_option {
            PathOption::KeepBase => node.rel.clone(),
            PathOption::KeepRelative => node
                .rel
                .strip_prefix(&trimmed)
                .unwrap_or(&node.rel)
                .to_path_buf(),
            PathOption::Flatten => PathBuf::from(&node.name),
        })
        .collect();

    let mut created: BTreeSet<PathBuf> = BTreeSet::new();
    for (node, dest_rel) in selection.iter().zip(destinations.iter()) {
        // Two selected items that land on one destination path would leave only
        // the last one written, so neither is carried out.
        if destinations
            .iter()
            .filter(|other| *other == dest_rel)
            .count()
            > 1
        {
            plan.skipped.push(PlanSkip::refused(
                node.rel.clone(),
                Conflict::DestinationIsAnotherSource,
                "another selected item claims the same destination",
            ));
            continue;
        }
        if skips_unreachable_name(&mut plan, &node.rel, dest_rel)
            || skips_refused(&mut plan, node, &[from])
            || entry_of(node, from).is_none()
        {
            continue;
        }
        let source_path = bases.of(from).join(&node.rel);
        let target_path = target_root.join(dest_rel);
        let destination = probed_destination(&mut plan, node, fs, &source_path, &target_path);
        if matches!(destination, Destination::Refused) {
            continue;
        }
        // Intermediate folders that were not themselves selected still have to
        // exist before the first file lands in them.
        let mut ancestor = PathBuf::new();
        for component in dest_rel.parent().unwrap_or(Path::new("")).components() {
            ancestor.push(component);
            if created.insert(ancestor.clone()) {
                plan.push(step(
                    StepAction::CreateDir {
                        path: target_root.join(&ancestor),
                    },
                    ancestor.clone(),
                    None,
                ));
            }
        }
        let to_folder = ToFolder {
            from,
            source_root: bases.of(from),
            target_root,
            moving,
            fs,
        };
        plan_copy_subtree_to(&mut plan, node, &to_folder, dest_rel, Some(destination));
    }
    plan
}

/// Where [`plan_copy_subtree_to`] reads from and writes to.
struct ToFolder<'a> {
    from: Side,
    source_root: &'a Path,
    target_root: &'a Path,
    moving: bool,
    fs: &'a dyn FileOps,
}

/// What a probe of one destination found.
enum Destination {
    /// No item holds the name.
    Free,
    /// An item holds the name.
    Occupied(TargetState),
    /// The item produces no step, and the plan records why: the destination
    /// could not be read, or it is the item itself.
    Refused,
}

fn probed_destination(
    plan: &mut OperationPlan,
    node: &Node,
    fs: &dyn FileOps,
    source_path: &Path,
    target_path: &Path,
) -> Destination {
    let found = match fs.probe(target_path) {
        Ok(state) => state,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Destination::Free,
        Err(error) => {
            plan.skipped.push(PlanSkip::noted(
                node.rel.clone(),
                format!("{} cannot be read: {error}", target_path.display()),
            ));
            return Destination::Refused;
        }
    };
    if let Some(reason) = onto_itself(same_item(fs, source_path, target_path)) {
        plan.skipped.push(PlanSkip::noted(node.rel.clone(), reason));
        return Destination::Refused;
    }
    Destination::Occupied(found)
}

/// The target-folder form of [`plan_copy_subtree`]. The destination has no
/// counterpart in the compared tree, so its state is read through the probe,
/// unless the caller already read it and passes it as `probed`.
fn plan_copy_subtree_to(
    plan: &mut OperationPlan,
    node: &Node,
    to: &ToFolder<'_>,
    dest_rel: &Path,
    probed: Option<Destination>,
) {
    let ToFolder {
        from,
        source_root,
        target_root,
        moving,
        fs,
    } = *to;
    if skips_refused(plan, node, &[from]) || skips_unreachable_name(plan, &node.rel, dest_rel) {
        return;
    }
    let Some(source) = entry_of(node, from) else {
        return;
    };
    let source_path = source_root.join(&node.rel);
    let target_path = target_root.join(dest_rel);
    let destination = match probed {
        Some(destination) => destination,
        None => probed_destination(plan, node, fs, &source_path, &target_path),
    };
    let found = match destination {
        Destination::Free => None,
        Destination::Occupied(state) => Some(state),
        Destination::Refused => return,
    };
    let target_state = found
        .as_ref()
        .map_or_else(PathState::absent, PathState::found);

    if source.is_dir {
        if source.is_link() {
            plan.skipped.push(PlanSkip::refused(
                node.rel.clone(),
                Conflict::RemovesLinkOnly,
                "a directory link is not descended into",
            ));
            return;
        }
        let mut dir_step = step(
            StepAction::CreateDir { path: target_path },
            dest_rel.to_path_buf(),
            None,
        );
        if found.as_ref().is_some_and(|found| !found.is_dir) {
            dir_step.conflicts.push(Conflict::KindMismatch);
        }
        dir_step.expected = expectation(node, Some(from), None);
        dir_step.expected.target = Some(target_state);
        plan.push(dir_step);
        for child in &node.children {
            plan_copy_subtree_to(plan, child, to, &dest_rel.join(&child.name), None);
        }
        if moving {
            if node.incomplete {
                plan.skipped.push(PlanSkip::refused(
                    node.rel.clone(),
                    Conflict::CounterpartUnreadable,
                    "the source folder's listing is partial, so it is not removed",
                ));
                return;
            }
            let mut remove = step(
                StepAction::DeleteDir { path: source_path },
                node.rel.clone(),
                Some(from),
            );
            remove.expected = removal_expectation(node, from, false);
            plan.push(remove);
        }
        return;
    }

    let action = if moving {
        StepAction::MoveFile {
            source: source_path,
            target: target_path.clone(),
        }
    } else {
        StepAction::CopyFile {
            source: source_path,
            target: target_path.clone(),
        }
    };
    let mut file_step = step(action, dest_rel.to_path_buf(), None);
    file_step.bytes = source.size;
    file_step.conflicts = found
        .as_ref()
        .map(|found| probed_conflicts(source, found))
        .unwrap_or_default();
    file_step.expected = expectation(node, Some(from), None);
    file_step.expected.target = Some(target_state);
    file_step.backup = plan
        .options
        .backup
        .as_ref()
        .filter(|_| found.is_some())
        .map(|backup| backup.path_for(&target_path));
    plan.push(file_step);
}

/// Plan a delete of the selected items from one or both sides.
#[must_use]
pub fn plan_delete(
    selection: &[&Node],
    sides: Sides,
    bases: Bases<'_>,
    options: &OperationOptions,
) -> OperationPlan {
    let mut plan = OperationPlan::new(
        OperationKind::Delete,
        vec![bases.left.to_path_buf(), bases.right.to_path_buf()],
        options.clone(),
    );
    for side in sides.each() {
        for node in selection {
            plan_delete_subtree(&mut plan, node, *side, bases.of(*side));
        }
    }
    plan
}

/// Plan a touch of the selected items.
///
/// A selected folder has its own timestamp written; its contents are left
/// alone unless they were selected too.
#[must_use]
pub fn plan_touch(
    selection: &[&Node],
    sides: Sides,
    bases: Bases<'_>,
    spec: TouchSpec,
    options: &OperationOptions,
) -> OperationPlan {
    let mut plan = OperationPlan::new(
        OperationKind::Touch,
        vec![bases.left.to_path_buf(), bases.right.to_path_buf()],
        options.clone(),
    );
    for side in sides.each() {
        for node in selection {
            if skips_refused(&mut plan, node, &[*side]) {
                continue;
            }
            let Some(entry) = entry_of(node, *side) else {
                continue;
            };
            // Writing through a link would change an item outside the compared
            // tree, which no selection in the tree stands for.
            if entry.is_link() {
                plan.skipped.push(PlanSkip::refused(
                    node.rel.clone(),
                    Conflict::RemovesLinkOnly,
                    "a link's target is not touched",
                ));
                continue;
            }
            let modified = match spec {
                TouchSpec::Explicit(time) => Some(time),
                TouchSpec::FromOtherSide => {
                    entry_of(node, opposite(*side)).and_then(|entry| entry.modified)
                }
            };
            let Some(modified) = modified else {
                plan.skipped.push(PlanSkip::noted(
                    node.rel.clone(),
                    "no timestamp available on the other side",
                ));
                continue;
            };
            plan.push(step(
                StepAction::SetTimes {
                    path: bases.of(*side).join(&node.rel),
                    modified: Some(modified),
                    created: None,
                },
                node.rel.clone(),
                Some(*side),
            ));
        }
    }
    plan
}

/// Plan an attribute change over the selected items.
///
/// A selected folder has its own attributes written; its contents are left
/// alone unless they were selected too.
#[must_use]
pub fn plan_attributes(
    selection: &[&Node],
    sides: Sides,
    bases: Bases<'_>,
    change: AttributeChange,
    options: &OperationOptions,
) -> OperationPlan {
    let mut plan = OperationPlan::new(
        OperationKind::Attributes,
        vec![bases.left.to_path_buf(), bases.right.to_path_buf()],
        options.clone(),
    );
    if change.is_empty() {
        return plan;
    }
    for side in sides.each() {
        for node in selection {
            if skips_refused(&mut plan, node, &[*side]) {
                continue;
            }
            let Some(entry) = entry_of(node, *side) else {
                continue;
            };
            if entry.is_link() {
                plan.skipped.push(PlanSkip::refused(
                    node.rel.clone(),
                    Conflict::RemovesLinkOnly,
                    "a link's target keeps its attributes",
                ));
                continue;
            }
            plan.push(step(
                StepAction::SetAttributes {
                    path: bases.of(*side).join(&node.rel),
                    change,
                },
                node.rel.clone(),
                Some(*side),
            ));
        }
    }
    plan
}

/// Plan the creation of one folder on the given sides.
///
/// A name that is not one folder name gives no step: an empty name and `.`
/// name the parent itself, `..` names a folder above it, and a separator
/// makes a path of several folders. On Windows a name that ends with a dot or
/// a space, or that is a DOS device name, gives no step either: the path would
/// create or find the folder without the dot or the space, or reach the
/// device. The plan records the reason the folder scan gives for such a name.
/// The parent and its ancestors are checked on each requested side; a refused
/// mapped row can never become a parent through its display spelling.
#[must_use]
pub fn plan_new_folder(
    tree: &Node,
    parent_rel: &Path,
    name: &str,
    sides: Sides,
    bases: Bases<'_>,
    options: &OperationOptions,
) -> OperationPlan {
    let mut plan = OperationPlan::new(
        OperationKind::NewFolder,
        vec![bases.left.to_path_buf(), bases.right.to_path_buf()],
        options.clone(),
    );
    let rel = parent_rel.join(name);
    if !is_one_name(name) {
        plan.skipped.push(PlanSkip::noted(
            rel,
            format!(
                "{name:?} is not one folder name: a folder name is not empty, is not . or .., \
                 and holds no path separator"
            ),
        ));
        return plan;
    }
    if skips_unreachable_name(&mut plan, &rel, Path::new(name)) {
        return plan;
    }
    for side in sides.each() {
        let mut refused = false;
        for ancestor in parent_rel.ancestors() {
            let Some(node) = node_at(tree, ancestor) else {
                plan.skipped.push(PlanSkip::noted(
                    ancestor.to_path_buf(),
                    "the parent was not found in the compared tree",
                ));
                refused = true;
                break;
            };
            if skips_refused(&mut plan, node, &[*side]) {
                refused = true;
                break;
            }
        }
        if refused {
            continue;
        }
        plan.push(step(
            StepAction::CreateDir {
                path: bases.of(*side).join(&rel),
            },
            rel.clone(),
            Some(*side),
        ));
    }
    plan
}

/// True when `name` is one normal path component spelled exactly as given.
fn is_one_name(name: &str) -> bool {
    let mut components = Path::new(name).components();
    let single = matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(part)), None) if part == std::ffi::OsStr::new(name)
    );
    single && !name.chars().any(std::path::is_separator)
}

/// Errors raised while a rename is planned.
#[derive(Debug, thiserror::Error)]
pub enum RenameError {
    /// The regular expression could not be parsed.
    #[error("invalid expression: {0}")]
    BadExpression(String),
    /// Two selected items would end up with the same new name.
    #[error("duplicate new name: {0}")]
    Duplicate(String),
    /// One item's new name is another selected item's current name, so the
    /// renames would have to run in an order that overwrites one of them.
    #[error("new name {0} is another selected item")]
    Chain(String),
}

/// Apply a mask to one name, one segment at a time.
///
/// `?` copies one character from the old name, `*` copies the rest of the
/// segment, and any other character is written literally while consuming one
/// character of the old name.
#[must_use]
pub fn apply_mask(name: &str, mask: &str) -> String {
    let (name_stem, name_ext) = split_extension(name);
    let (mask_stem, mask_ext) = split_extension(mask);
    let stem = apply_segment(name_stem, mask_stem);
    match mask_ext {
        Some(ext_mask) => format!("{stem}.{}", apply_segment(name_ext.unwrap_or(""), ext_mask)),
        None => stem,
    }
}

fn split_extension(name: &str) -> (&str, Option<&str>) {
    match name.rfind('.') {
        Some(0) | None => (name, None),
        Some(position) => (&name[..position], Some(&name[position + 1..])),
    }
}

fn apply_segment(source: &str, mask: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut out = String::new();
    let mut index = 0usize;
    for marker in mask.chars() {
        match marker {
            '?' => {
                if let Some(character) = chars.get(index) {
                    out.push(*character);
                }
                index += 1;
            }
            '*' => {
                for character in chars.iter().skip(index) {
                    out.push(*character);
                }
                index = chars.len();
            }
            literal => {
                out.push(literal);
                index += 1;
            }
        }
    }
    out
}

/// Build the mask that stands for every selected name at once.
///
/// Characters every name shares stay literal; a single differing character
/// becomes `?` and a longer difference becomes `*`.
#[must_use]
pub fn common_mask(names: &[String]) -> String {
    let Some(first) = names.first() else {
        return String::new();
    };
    if names.len() == 1 {
        return first.clone();
    }
    let lists: Vec<Vec<char>> = names.iter().map(|name| name.chars().collect()).collect();
    let shortest = lists.iter().map(Vec::len).min().unwrap_or(0);

    let prefix = (0..shortest)
        .take_while(|index| {
            let candidate = lists[0][*index];
            lists.iter().all(|list| list[*index] == candidate)
        })
        .count();
    let suffix = (0..shortest - prefix)
        .take_while(|back| {
            let candidate = lists[0][lists[0].len() - 1 - back];
            lists
                .iter()
                .all(|list| list[list.len() - 1 - back] == candidate)
        })
        .count();

    let middles: Vec<usize> = lists
        .iter()
        .map(|list| list.len() - prefix - suffix)
        .collect();
    let wildcard = if middles.iter().all(|length| *length == 1) {
        "?"
    } else {
        "*"
    };
    let head: String = lists[0][..prefix].iter().collect();
    let tail: String = lists[0][lists[0].len() - suffix..].iter().collect();
    format!("{head}{wildcard}{tail}")
}

/// Plan a rename of the selected items in place.
///
/// `root` is the tree the selection was taken from, and the other items of
/// each folder are read from it. An item outside the selection that holds a
/// new name, with names compared without case, makes the step a replacement
/// of that item: the step carries the conflicts of a copy onto it, and the
/// overwrite and backup options apply as they do to a copy. A rename never
/// puts a folder over another item or a file over a folder.
///
/// # Errors
/// Returns [`RenameError::BadExpression`] for an unparsable expression,
/// [`RenameError::Duplicate`] when two items would take the same name, and
/// [`RenameError::Chain`] when a new name is another selected item's name.
pub fn plan_rename(
    root: &Node,
    selection: &[&Node],
    sides: Sides,
    bases: Bases<'_>,
    action: &RenameAction,
    options: &OperationOptions,
) -> Result<OperationPlan, RenameError> {
    let mut plan = OperationPlan::new(
        OperationKind::Rename,
        vec![bases.left.to_path_buf(), bases.right.to_path_buf()],
        options.clone(),
    );
    let expression = match action {
        RenameAction::Mask(_) => None,
        RenameAction::Regex { find, .. } => Some(
            regex::Regex::new(find)
                .map_err(|error| RenameError::BadExpression(error.to_string()))?,
        ),
    };

    for side in sides.each() {
        // A volume that folds case reaches an item through every spelling
        // that differs from its name only by case, so names are compared
        // without case.
        let mut taken: BTreeSet<String> = BTreeSet::new();
        let mut occupied: HashMap<String, usize> = HashMap::new();
        for node in selection
            .iter()
            .filter(|node| entry_of(node, *side).is_some())
        {
            *occupied.entry(folded(&node.rel)).or_default() += 1;
        }
        let mut folders = FolderNames::new(root, *side);
        for node in selection {
            let Some(source) = entry_of(node, *side) else {
                continue;
            };
            if skips_refused(&mut plan, node, &[*side]) {
                continue;
            }
            let new_name = match action {
                RenameAction::Mask(mask) => apply_mask(&node.name, mask),
                RenameAction::Regex { replace, .. } => expression.as_ref().map_or_else(
                    || node.name.clone(),
                    |pattern| {
                        pattern
                            .replace_all(&node.name, replace.as_str())
                            .into_owned()
                    },
                ),
            };
            if new_name == node.name || new_name.is_empty() {
                continue;
            }
            if skips_unreachable_name(&mut plan, &node.rel, Path::new(&new_name)) {
                continue;
            }
            let parent = node.rel.parent().unwrap_or(Path::new("")).to_path_buf();
            let new_rel = parent.join(&new_name);
            let key = folded(&new_rel);
            if !taken.insert(key.clone()) {
                return Err(RenameError::Duplicate(new_name));
            }
            // A chain or a swap needs a staging name that this operation does
            // not have; carrying it out in plan order would destroy an item.
            let own = usize::from(key == folded(&node.rel));
            if occupied.get(&key).is_some_and(|count| *count > own) {
                return Err(RenameError::Chain(new_name));
            }
            let target = bases.of(*side).join(&new_rel);
            let mut rename_step = step(
                StepAction::Rename {
                    from: bases.of(*side).join(&node.rel),
                    to: target.clone(),
                },
                new_rel,
                Some(*side),
            );
            let case_only = new_name.to_lowercase() == node.name.to_lowercase();
            if case_only {
                rename_step.conflicts.push(Conflict::CaseOnlyRename);
            }
            match folders.occupant(node, &new_name) {
                Some(held) if source.is_dir || held.is_dir => {
                    plan.skipped.push(PlanSkip::noted(
                        node.rel.clone(),
                        "another item holds the new name, and a rename replaces only a file with a file",
                    ));
                    continue;
                }
                Some(held) => {
                    rename_step
                        .conflicts
                        .extend(replacement_conflicts(source, held));
                    // A spelling that differs only by case names another item
                    // only where case is kept, so its state is no expectation.
                    if held.name == new_name {
                        rename_step.expected.target = Some(PathState::of(held));
                    }
                    rename_step.backup = options
                        .backup
                        .as_ref()
                        .map(|backup| backup.path_for(&target));
                }
                // Where case is folded the new name reaches the item itself.
                None if case_only => {}
                None => rename_step.expected.target = Some(PathState::absent()),
            }
            plan.push(rename_step);
        }
    }
    Ok(plan)
}

/// A path as text in lower case, which is how two paths are judged equal
/// where case is folded.
fn folded(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

/// The names the folders of one side hold, in lower case, each read from the
/// tree once.
struct FolderNames<'a> {
    root: &'a Node,
    side: Side,
    folders: HashMap<PathBuf, HashMap<String, Vec<&'a Node>>>,
}

impl<'a> FolderNames<'a> {
    fn new(root: &'a Node, side: Side) -> Self {
        Self {
            root,
            side,
            folders: HashMap::new(),
        }
    }

    /// The entry other than the entry of `node` that holds `name` in the
    /// folder of `node`, compared without case.
    ///
    /// A refused row names no item on disk, so it holds no name.
    fn occupant(&mut self, node: &Node, name: &str) -> Option<&'a crate::scan::Entry> {
        let parent = node.rel.parent()?;
        let (root, side) = (self.root, self.side);
        let names = self
            .folders
            .entry(parent.to_path_buf())
            .or_insert_with(|| folder_names(root, parent, side));
        names
            .get(&name.to_lowercase())?
            .iter()
            .filter(|sibling| sibling.rel != node.rel)
            .find_map(|sibling| entry_of(sibling, side))
    }
}

/// The items on `side` of the folder at `parent`, by name in lower case.
fn folder_names<'a>(root: &'a Node, parent: &Path, side: Side) -> HashMap<String, Vec<&'a Node>> {
    let mut names: HashMap<String, Vec<&'a Node>> = HashMap::new();
    let Some(folder) = node_at(root, parent) else {
        return names;
    };
    for child in &folder.children {
        if let Some(entry) = entry_of(child, side).filter(|entry| !entry.refused) {
            names
                .entry(entry.name.to_lowercase())
                .or_default()
                .push(child);
        }
    }
    names
}

/// The node at `rel`, found by walking down from `root`.
///
/// A pair takes its path from its left entry and a right orphan from its right
/// entry, so each step down matches a child on the path of either entry.
fn node_at<'a>(root: &'a Node, rel: &Path) -> Option<&'a Node> {
    let mut current = root;
    let mut walked = PathBuf::new();
    for component in rel.components() {
        walked.push(component);
        current = current.children.iter().find(|child| {
            child.rel == walked
                || [child.left.as_ref(), child.right.as_ref()]
                    .into_iter()
                    .flatten()
                    .any(|entry| entry.rel == walked)
        })?;
    }
    Some(current)
}

/// Masks the Exclude command would add to a session's name filters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExcludeMasks {
    /// Masks to add to the excluded file list.
    pub files: Vec<String>,
    /// Masks to add to the excluded folder list.
    pub folders: Vec<String>,
}

/// Build the filter masks that exclude a selection.
///
/// With `by_type` set and every selected file sharing one extension, the
/// extension itself is excluded instead of the individual names.
#[must_use]
pub fn exclude_masks(selection: &[&Node], by_type: bool) -> ExcludeMasks {
    let mut masks = ExcludeMasks::default();
    let files: Vec<&&Node> = selection.iter().filter(|node| !node.is_dir).collect();
    let extensions: BTreeSet<String> = files
        .iter()
        .filter_map(|node| split_extension(&node.name).1.map(str::to_lowercase))
        .collect();

    if by_type && !files.is_empty() && extensions.len() == 1 {
        if let Some(extension) = extensions.iter().next() {
            masks.files.push(format!("*.{extension}"));
        }
    } else {
        for node in files {
            masks.files.push(node.rel.to_string_lossy().to_string());
        }
    }
    for node in selection.iter().filter(|node| node.is_dir) {
        masks.folders.push(node.rel.to_string_lossy().to_string());
    }
    masks
}

pub(crate) fn opposite(side: Side) -> Side {
    match side {
        Side::Left => Side::Right,
        Side::Right => Side::Left,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{
        apply_mask, common_mask, common_parent, path_is_within, resolve_selection, split_extension,
    };
    use crate::compare::Node;

    fn tree() -> Node {
        let left = {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(dir.path().join("a")).unwrap();
            std::fs::write(dir.path().join("a/one.txt"), b"1").unwrap();
            std::fs::write(dir.path().join("a/two.txt"), b"2").unwrap();
            let result = crate::scan::scan_with(
                dir.path(),
                &crate::scan::ScanOptions::default(),
                &crate::cancel::Cancel::new(),
                &|_| {},
            )
            .unwrap();
            drop(dir);
            result
        };
        crate::compare::align_trees(
            &left,
            &crate::scan::ScanResult::default(),
            &crate::compare::AlignmentOptions::default(),
            &crate::cancel::Cancel::new(),
        )
    }

    #[test]
    fn mask_rename_changes_the_extension() {
        assert_eq!(apply_mask("abc1.txt", "abc?.bak"), "abc1.bak");
        assert_eq!(apply_mask("abc2.txt", "abc?.bak"), "abc2.bak");
    }

    #[test]
    fn mask_rename_changes_the_stem() {
        assert_eq!(apply_mask("abc3.txt", "xyz?.txt"), "xyz3.txt");
    }

    #[test]
    fn common_mask_uses_a_single_character_wildcard() {
        let names = vec![
            "abc1.txt".to_string(),
            "abc2.txt".to_string(),
            "abc3.txt".to_string(),
        ];
        assert_eq!(common_mask(&names), "abc?.txt");
    }

    #[test]
    fn common_mask_widens_for_longer_differences() {
        let names = vec!["abc12.txt".to_string(), "abc3.txt".to_string()];
        assert_eq!(common_mask(&names), "abc*.txt");
    }

    #[test]
    fn extension_split_ignores_a_leading_period() {
        assert_eq!(split_extension(".gitignore"), (".gitignore", None));
    }

    #[test]
    fn containment_rejects_an_escaping_path() {
        let root = Path::new("/base/left");
        assert!(path_is_within(root, Path::new("/base/left/a/b")));
        assert!(!path_is_within(root, Path::new("/base/right/a")));
        assert!(!path_is_within(root, Path::new("/base/left/../right/a")));
    }

    #[test]
    fn common_parent_is_the_shared_folder() {
        let paths = vec![PathBuf::from("a/b/one"), PathBuf::from("a/b/c/two")];
        assert_eq!(common_parent(&paths), PathBuf::from("a/b"));
    }

    #[test]
    fn a_selected_child_overrides_its_selected_parent() {
        let root = tree();
        let selected = [PathBuf::from("a"), PathBuf::from("a").join("one.txt")]
            .into_iter()
            .collect();
        let resolved = resolve_selection(&root, &selected);
        let names: Vec<&str> = resolved.iter().map(|node| node.name.as_str()).collect();
        assert_eq!(names, vec!["one.txt"]);
    }

    #[test]
    fn a_selected_folder_stands_for_its_contents() {
        let root = tree();
        let selected = [PathBuf::from("a")].into_iter().collect();
        let resolved = resolve_selection(&root, &selected);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].name, "a");
    }
}
