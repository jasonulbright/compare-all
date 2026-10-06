//! Three way folder merge: a left, an optional center and a right folder, and
//! the output folder the merge result is written to.
//!
//! The comparison and the plan both run on workers. The plan goes through the
//! same confirmation, journal and execution as every other file operation, so
//! nothing reaches the disk that was not shown first when the options ask for
//! a confirmation, and nothing ever reaches it outside the journal.

use crate::dialogs::{self, modal};
use crate::opjobs::{self, Answer, Ask, ExecMessage, ProgressState};
use crate::settings::{merge_options_of, EngineOptions};
use ca_fs::merge::automatic_resolution;
use ca_fs::{
    compare3_sources, leaves_for_person, output_copies_an_input, output_holds_conflict_markers,
    output_written_after_inputs, plan_merge, scan_source, scan_with, ExecutionReport,
    FilterContext, FolderMergeOptions, MergeBases, MergeFilters, MergeInputs, MergeRequest,
    MergeRow, MergeSources, MergeStatus, MergeTree, OperationOptions, OperationPlan, Pane,
    Resolution, RulesEngine, ScanResult, Source,
};
use ca_session::settings::folder::MergeTarget;
use ca_session::settings::{FolderMergeSettings, SessionSettings};
use ca_session::{SessionKind, SideLocation};
use ca_ui::command::{Command, MenuView};
use ca_ui::report::{ReportKind, ViewReport};
use ca_ui::theme::folder_merge::{self, FolderMergeClass};
use ca_ui::theme::Variant;
use ca_ui::toolbar;
use ca_ui::view::{
    self, CommandState, OpenRequest, SavedFile, SessionView, ViewAction, ViewContext,
};
use ca_ui::worker::{Cancel, Emitter, Job, Terminal};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Height of one row.
const ROW_HEIGHT: f32 = 18.0;
/// Indent added per tree level.
const INDENT: f32 = 14.0;
/// Width the display filter drop down is laid out in.
const FILTER_COMBO_WIDTH: f32 = 170.0;
/// Width of the column that states the planned action.
const ACTION_WIDTH: f32 = 110.0;
/// Why a merge cannot run when no output folder is named.
const NO_OUTPUT: &str = "Name an output folder in the session settings first";
/// Why a command waits for the comparison.
/// Why a merge does not write into a container.
const ARCHIVE_OUTPUT: &str =
    "The output is an archive. A merge writes into a local folder only. Name a folder as the output.";
const NOT_READY: &str = "Available once the comparison finishes";
/// What the view says when a merge has nothing to write and the output holds
/// the merge result of every row.
const FINISHED: &str = "The output already holds the merge result.";
/// The Action column of a row whose output holds a merge by hand.
const MERGED_BY_HAND: &str = "Merged by hand";
/// The Action column of a row whose output Text Merge saved with conflicts
/// still marked.
const SAVED_WITH_CONFLICTS: &str = "Saved with conflicts";
/// The status text after a merge of a selection when other items still
/// differ from the merge result.
const REST_OF_THE_TREE: &str = "Only the selected items take part in the merge. \
     To write the other items to the output, select them and merge again.";
/// How many items a dialog names before it counts the rest.
const DIALOG_NAMES: usize = 20;
/// How many items the status text names before it counts the rest.
const MESSAGE_NAMES: usize = 5;
/// Exit code when no item waits for a merge by hand.
const EXIT_SUCCESS: i32 = 0;
/// Exit code when items wait for a merge by hand and a merge wrote to the
/// output; the text merge returns the same code for conflicts it leaves.
const EXIT_LEFT_FOR_HAND: i32 = 14;
/// Exit code when items wait for a merge by hand and no merge wrote to the
/// output; the text merge returns the same code when it writes no file.
const EXIT_LEFT_FOR_HAND_NOTHING_WRITTEN: i32 = 101;
/// Exit code when a step of a merge did not complete and no later merge
/// settled its item; the text merge returns the same code when its write
/// fails.
const EXIT_STEP_UNDONE: i32 = 100;

/// Every command from the shared vocabulary this view answers for.
const HANDLED: &[Command] = &[
    Command::SwapSides,
    Command::Reload,
    Command::CompareReport,
    Command::SelectAll,
    Command::NextDifference,
    Command::PreviousDifference,
    Command::ShowAll,
    Command::ShowDifferences,
    Command::ShowConflicts,
    Command::ShowLeftChanges,
    Command::ShowRightChanges,
    Command::ShowMergeable,
    Command::ShowSame,
    Command::ShowNone,
    Command::ToggleIgnoreSameChanges,
    Command::TakeLeft,
    Command::TakeCenter,
    Command::TakeRight,
    Command::NextConflict,
    Command::PreviousConflict,
    Command::ToggleCenterPane,
    Command::MergeFolders,
    Command::CopyToOutput,
    Command::OpenTextMerge,
    Command::CompareToOutput,
];

/// Which rows the view shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeFilter {
    /// Every row.
    All,
    /// Every change on either side, conflicts included.
    Changes,
    /// Conflicts only.
    Conflicts,
    /// Changes only the left side made.
    LeftChanges,
    /// Changes only the right side made.
    RightChanges,
    /// Same changes and changes a text merge resolves.
    Mergeable,
    /// Unchanged rows only.
    Unchanged,
    /// No files.
    None,
}

impl MergeFilter {
    /// Every filter, in the order the drop down lists them.
    pub const ALL: [Self; 8] = [
        Self::All,
        Self::Changes,
        Self::Conflicts,
        Self::LeftChanges,
        Self::RightChanges,
        Self::Mergeable,
        Self::Unchanged,
        Self::None,
    ];

    /// The name the drop down shows.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "Show All",
            Self::Changes => "Show Changes",
            Self::Conflicts => "Show Conflicts",
            Self::LeftChanges => "Show Left Changes",
            Self::RightChanges => "Show Right Changes",
            Self::Mergeable => "Show Mergeable",
            Self::Unchanged => "Show Unchanged",
            Self::None => "Show None",
        }
    }

    /// The command that selects this filter.
    #[must_use]
    pub const fn command(self) -> Command {
        match self {
            Self::All => Command::ShowAll,
            Self::Changes => Command::ShowDifferences,
            Self::Conflicts => Command::ShowConflicts,
            Self::LeftChanges => Command::ShowLeftChanges,
            Self::RightChanges => Command::ShowRightChanges,
            Self::Mergeable => Command::ShowMergeable,
            Self::Unchanged => Command::ShowSame,
            Self::None => Command::ShowNone,
        }
    }

    /// The filter a command selects.
    #[must_use]
    pub fn of(command: Command) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|filter| filter.command() == command)
    }

    /// True when a row of `status` passes, with same changes counted as
    /// unchanged when `ignore_same` is set.
    #[must_use]
    pub const fn admits(self, status: MergeStatus, ignore_same: bool) -> bool {
        let same_as_unchanged = ignore_same && matches!(status, MergeStatus::SameChange(_));
        match self {
            Self::All => true,
            Self::Changes => !same_as_unchanged && !matches!(status, MergeStatus::Unchanged),
            Self::Conflicts => matches!(status, MergeStatus::Conflict),
            Self::LeftChanges => matches!(status, MergeStatus::LeftChange(_)),
            Self::RightChanges => matches!(status, MergeStatus::RightChange(_)),
            Self::Mergeable => {
                !same_as_unchanged
                    && matches!(status, MergeStatus::SameChange(_) | MergeStatus::Mergeable)
            }
            Self::Unchanged => same_as_unchanged || matches!(status, MergeStatus::Unchanged),
            Self::None => false,
        }
    }
}

/// The color class of a status.
#[must_use]
pub const fn class_of(status: MergeStatus, ignore_same: bool) -> FolderMergeClass {
    match status {
        MergeStatus::Unchanged => FolderMergeClass::Same,
        MergeStatus::SameChange(_) if ignore_same => FolderMergeClass::Same,
        MergeStatus::SameChange(_) | MergeStatus::Mergeable => FolderMergeClass::Mergeable,
        MergeStatus::LeftChange(_) => FolderMergeClass::LeftChange,
        MergeStatus::RightChange(_) => FolderMergeClass::RightChange,
        MergeStatus::Conflict => FolderMergeClass::Conflict,
        MergeStatus::Unknown => FolderMergeClass::Unknown,
    }
}

/// The action a row's resolution stands for, in the words of the output it is
/// written to.
#[must_use]
pub fn action_label(row: &MergeRow, resolution: Resolution, target: &MergeTarget) -> &'static str {
    let wanted = match resolution {
        Resolution::Leave if row.status.needs_person() => return "Merge by hand",
        Resolution::Leave => return "",
        Resolution::Delete => None,
        Resolution::Take(pane) => Some((pane, row.entry(pane).is_some())),
    };
    match (target, wanted) {
        (MergeTarget::Left, Some((Pane::Left, _)))
        | (MergeTarget::Right, Some((Pane::Right, _))) => "",
        (MergeTarget::Left, Some((_, true))) => "Copy to left",
        (MergeTarget::Left, _) => "Delete left",
        (MergeTarget::Right, Some((_, true))) => "Copy to right",
        (MergeTarget::Right, _) => "Delete right",
        (_, Some((Pane::Left, true))) => "Take left",
        (_, Some((Pane::Center, true))) => "Take center",
        (_, Some((Pane::Right, true))) => "Take right",
        (_, _) => "Delete",
    }
}

/// A relative path as the report writes it, with `/` between the names.
fn slash_path(rel: &Path) -> String {
    rel.to_string_lossy().replace('\\', "/")
}

/// The path in the output folder a row writes to: the output's own spelling
/// where the output holds an item, else the row's.
fn target_rel(row: &MergeRow) -> &Path {
    row.output.as_ref().map_or(&row.rel, |entry| &entry.rel)
}

/// True when Text Merge opens `row`: both changed versions are text files,
/// every input is a local folder, and writing the output item passes through
/// no link or file of the output.
///
/// The Text Merge toolbar button and the notice that sends a row to Text
/// Merge both read this rule, so the notice never names a step the toolbar
/// refuses.
fn opens_in_text_merge(tree: &MergeTree, row: &MergeRow) -> bool {
    row.text
        && row.left.is_some()
        && row.right.is_some()
        && tree.text_merge_inputs_are_local()
        && tree.output_target_is_safe(target_rel(row))
}

/// True when two path components name the same item under the platform's
/// rule: letter case does not count on Windows.
fn same_component(first: Component<'_>, second: Component<'_>) -> bool {
    if cfg!(windows) {
        first.as_os_str().to_string_lossy().to_lowercase()
            == second.as_os_str().to_string_lossy().to_lowercase()
    } else {
        first == second
    }
}

/// `path` below `root`, compared name by name under the platform's rule.
/// Trailing separators do not count.
fn relative_to(path: &Path, root: &Path) -> Option<PathBuf> {
    let mut rest = path.components();
    for part in root.components() {
        if !same_component(part, rest.next()?) {
            return None;
        }
    }
    Some(rest.collect())
}

/// The key a relative output path is recorded under: `/` between the names,
/// and lower case on Windows, where letter case does not count.
fn path_key(rel: &Path) -> String {
    let key = rel
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    if cfg!(windows) {
        key.to_lowercase()
    } else {
        key
    }
}

/// The size and time of one input item, as a comparison listed it.
type ItemFacts = Option<(u64, Option<std::time::SystemTime>)>;

/// The size and time of the input items of `row`, leaving out the input that
/// is the output folder itself.
fn input_facts(row: &MergeRow, output_pane: Option<Pane>) -> [ItemFacts; 3] {
    [Pane::Left, Pane::Center, Pane::Right].map(|pane| {
        if output_pane == Some(pane) {
            return None;
        }
        row.entry(pane).map(|entry| (entry.size, entry.modified))
    })
}

/// A save of a row's output from Text Merge in this session.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SaveRecord {
    /// Conflicts the saved text still marks.
    conflicts: u32,
    /// The input items of the row when the save arrived, as the comparison
    /// listed them; `None` until a comparison lists the row. A comparison
    /// that lists any of them with another size or time drops the record.
    inputs: Option<[ItemFacts; 3]>,
}

/// The rows of a comparison a save or the output time settles.
#[derive(Debug, Clone, Default)]
struct SaveStates {
    /// Rows merged by hand.
    merged: BTreeSet<PathBuf>,
    /// Rows Text Merge saved with conflicts still marked.
    conflicted: BTreeSet<PathBuf>,
}

/// What a worker that resolves the path of a save posts back.
#[derive(Debug)]
pub enum ResolveMessage {
    /// The save, and its path below the output folder where it lies there
    /// once links and junctions are resolved.
    Done(SavedFile, Option<PathBuf>),
    /// The worker stopped.
    Lost,
}

impl Terminal for ResolveMessage {
    fn is_terminal(&self) -> bool {
        true
    }

    fn cancelled() -> Self {
        Self::Lost
    }

    fn panicked(_detail: String) -> Self {
        Self::Lost
    }
}

/// The items of a comparison that wait for a merge by hand: no merge writes
/// them, and the output does not hold a merge by hand of them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ByHand {
    mergeable: usize,
    conflicts: usize,
    /// How many of them Text Merge opens.
    text_merge: usize,
    /// The first items in tree order, at most [`DIALOG_NAMES`] of them.
    first: Vec<ByHandItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ByHandItem {
    rel: PathBuf,
    /// The words in brackets after the path, in order.
    facts: Vec<&'static str>,
}

impl ByHand {
    /// The rows that wait, each with whether Text Merge opens it and whether
    /// Text Merge saved it with conflicts.
    fn of<'t>(rows: impl IntoIterator<Item = (&'t MergeRow, bool, bool)>) -> Self {
        let mut by_hand = Self::default();
        for (row, text_merge, saved_with_conflicts) in rows {
            let conflict = row.status == MergeStatus::Conflict;
            if conflict {
                by_hand.conflicts += 1;
            } else {
                by_hand.mergeable += 1;
            }
            if text_merge {
                by_hand.text_merge += 1;
            }
            if by_hand.first.len() < DIALOG_NAMES {
                let mut facts = vec![if conflict { "conflict" } else { "mergeable" }];
                if !text_merge {
                    facts.push("Take only");
                }
                if saved_with_conflicts {
                    facts.push("saved with conflicts");
                }
                if row.output.is_none() {
                    facts.push("not in the output");
                }
                by_hand.first.push(ByHandItem {
                    rel: row.rel.clone(),
                    facts,
                });
            }
        }
        by_hand
    }

    /// The rows of `tree` that wait under `request` with every row taking
    /// part, leaving out the rows merged by hand.
    fn waiting_in(tree: &MergeTree, request: &MergeRequest<'_>, saves: &SaveStates) -> Self {
        let whole = MergeRequest {
            selection: None,
            ..*request
        };
        Self::of(
            tree.rows
                .iter()
                .filter(|row| leaves_for_person(row, &whole) && !saves.merged.contains(&row.rel))
                .map(|row| {
                    (
                        row,
                        opens_in_text_merge(tree, row),
                        saves.conflicted.contains(&row.rel),
                    )
                }),
        )
    }

    /// How many items wait for a merge by hand.
    #[must_use]
    pub const fn total(&self) -> usize {
        self.mergeable + self.conflicts
    }

    /// True when no item waits for a merge by hand.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.total() == 0
    }

    /// The sentences that count the items and say what the user can do.
    fn sentences(&self, has_center: bool) -> Vec<String> {
        let one = self.total() == 1;
        let (items, need) = if one {
            ("item", "needs")
        } else {
            ("items", "need")
        };
        let conflicts = if self.conflicts == 1 {
            "conflict"
        } else {
            "conflicts"
        };
        let takes = if has_center {
            "Take Left, Take Center or Take Right"
        } else {
            "Take Left or Take Right"
        };
        let mut sentences = vec![
            format!(
                "{} {items} {need} a merge by hand ({} mergeable, {} {conflicts}).",
                self.total(),
                self.mergeable,
                self.conflicts
            ),
            if one {
                "The merge does not write it to the output."
            } else {
                "The merge does not write them to the output."
            }
            .to_owned(),
        ];
        let others = self.total() - self.text_merge;
        match (self.text_merge, others) {
            (_, 0) if one => sentences.push(
                "To put its merge result in the output, merge it in Text Merge and save it."
                    .to_owned(),
            ),
            (_, 0) => sentences.push(
                "To put their merge result in the output, merge each one in Text Merge and save it."
                    .to_owned(),
            ),
            (0, _) if one => sentences.push("Text Merge cannot open it.".to_owned()),
            (0, _) => sentences.push("Text Merge cannot open them.".to_owned()),
            (opened, _) => {
                sentences.push(if opened == 1 {
                    "1 of them opens in Text Merge. To put its merge result in the output, \
                     merge it there and save it."
                        .to_owned()
                } else {
                    format!(
                        "{opened} of them open in Text Merge. To put their merge result in the \
                         output, merge each one there and save it."
                    )
                });
                sentences.push(if others == 1 {
                    "Text Merge cannot open the other one.".to_owned()
                } else {
                    format!("Text Merge cannot open the other {others}.")
                });
            }
        }
        sentences.push(if self.text_merge == 0 {
            format!("To put one input's copy in the output, choose {takes} and merge again.")
        } else {
            format!("To keep one input's copy instead, choose {takes} and merge again.")
        });
        sentences
    }

    /// One line per named item, at most `limit` of them.
    fn names(&self, limit: usize) -> Vec<String> {
        self.first
            .iter()
            .take(limit)
            .map(|item| format!("{} ({})", slash_path(&item.rel), item.facts.join(", ")))
            .collect()
    }

    /// How many items a list of at most `limit` names leaves out.
    fn unnamed(&self, limit: usize) -> usize {
        self.total() - self.first.len().min(limit)
    }

    /// The whole notice as one paragraph, naming at most `limit` items.
    fn notice(&self, has_center: bool, limit: usize) -> String {
        let label = if self.total() == 1 { "Item" } else { "Items" };
        let names = self.names(limit).join(", ");
        let tail = match self.unnamed(limit) {
            0 => String::new(),
            more => format!(" and {more} more"),
        };
        format!(
            "{} {label}: {names}{tail}.",
            self.sentences(has_center).join(" ")
        )
    }

    /// Draw the count and what the user can do.
    fn show_sentences(&self, ui: &mut egui::Ui, has_center: bool) {
        for sentence in self.sentences(has_center) {
            ca_ui::widgets::wrapped_text(ui, &sentence);
        }
    }

    /// One line per item a dialog names, then a count of the rest.
    fn dialog_lines(&self) -> Vec<String> {
        let mut lines = self.names(DIALOG_NAMES);
        match self.unnamed(DIALOG_NAMES) {
            0 => {}
            more => lines.push(format!("and {more} more")),
        }
        lines
    }
}

/// The output items merged by hand that a plan replaces or removes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Overwrites {
    total: usize,
    /// The first items in plan order, each with true where the step removes
    /// the item, at most [`DIALOG_NAMES`] of them.
    first: Vec<(PathBuf, bool)>,
}

impl Overwrites {
    /// The steps of `plan` that write over a row in `merged`.
    fn of(plan: &OperationPlan, merged: &BTreeSet<PathBuf>) -> Self {
        let mut overwrites = Self::default();
        for step in &plan.steps {
            if !merged.contains(&step.rel) {
                continue;
            }
            let removed = match step.action {
                ca_fs::StepAction::CopyFile { .. } => false,
                ca_fs::StepAction::DeleteFile { .. }
                | ca_fs::StepAction::Trash { .. }
                | ca_fs::StepAction::DeleteLink { .. } => true,
                _ => continue,
            };
            overwrites.total += 1;
            if overwrites.first.len() < DIALOG_NAMES {
                overwrites.first.push((step.rel.clone(), removed));
            }
        }
        overwrites
    }

    const fn is_empty(&self) -> bool {
        self.total == 0
    }

    fn sentence(&self) -> String {
        if self.total == 1 {
            "The merge replaces 1 item merged by hand in the output.".to_owned()
        } else {
            format!(
                "The merge replaces {} items merged by hand in the output.",
                self.total
            )
        }
    }

    fn lines(&self) -> Vec<String> {
        let mut lines: Vec<String> = self
            .first
            .iter()
            .map(|(rel, removed)| {
                let effect = if *removed { "removed" } else { "replaced" };
                format!("{} (merged by hand, {effect})", slash_path(rel))
            })
            .collect();
        if self.total > self.first.len() {
            lines.push(format!("and {} more", self.total - self.first.len()));
        }
        lines
    }
}

/// What a plan with no step leaves undone over the whole comparison, read on
/// the planning worker.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Finish {
    /// Rows with no resolution because automatic merge is off, that an
    /// automatic merge would write.
    unresolved: usize,
    /// True when a merge of every row would write something a merge of the
    /// selection does not.
    rest: bool,
    /// The items a merge of every row leaves alone, each with the reason.
    skipped: Vec<String>,
}

impl Finish {
    fn of(
        tree: &MergeTree,
        bases: MergeBases<'_>,
        request: &MergeRequest<'_>,
        options: &OperationOptions,
        plan: &OperationPlan,
    ) -> Self {
        let whole = MergeRequest {
            selection: None,
            ..*request
        };
        let whole_plan = request
            .selection
            .is_some()
            .then(|| plan_merge(tree, bases, &whole, options).ok())
            .flatten();
        let whole_plan = whole_plan.as_ref().unwrap_or(plan);
        let unresolved = if request.automatic {
            0
        } else {
            let automatic = MergeRequest {
                automatic: true,
                ..whole
            };
            plan_merge(tree, bases, &automatic, options).map_or(0, |written| {
                let stepped: BTreeSet<&Path> = written
                    .steps
                    .iter()
                    .map(|step| step.rel.as_path())
                    .collect();
                tree.rows
                    .iter()
                    .filter(|row| {
                        !request.overrides.contains_key(&row.rel)
                            && automatic_resolution(row) != Resolution::Leave
                            && stepped.contains(row.rel.as_path())
                    })
                    .count()
            })
        };
        Self {
            unresolved,
            rest: request.selection.is_some() && !whole_plan.steps.is_empty(),
            skipped: whole_plan
                .skipped
                .iter()
                .map(|skip| format!("{} ({})", slash_path(&skip.path), skip.reason))
                .collect(),
        }
    }

    const fn is_empty(&self) -> bool {
        self.unresolved == 0 && !self.rest && self.skipped.is_empty()
    }

    fn sentences(&self, has_center: bool) -> Vec<String> {
        let mut sentences = Vec::new();
        if self.unresolved > 0 {
            let takes = if has_center {
                "Take Left, Take Center or Take Right"
            } else {
                "Take Left or Take Right"
            };
            sentences.push(if self.unresolved == 1 {
                format!(
                    "Merge automatically is off in the session settings, so 1 item has no \
                     action and the merge leaves it out. To write it, choose {takes} and merge \
                     again."
                )
            } else {
                format!(
                    "Merge automatically is off in the session settings, so {} items have no \
                     action and the merge leaves them out. To write them, choose {takes} and \
                     merge again.",
                    self.unresolved
                )
            });
        }
        if self.rest {
            sentences.push(REST_OF_THE_TREE.to_owned());
        }
        if !self.skipped.is_empty() {
            sentences.push(format!(
                "{} items are left alone: {}",
                self.skipped.len(),
                self.skipped.join("; ")
            ));
        }
        sentences
    }
}

/// The status text a merge leaves, kept in parts so the text follows the
/// rows when a resolution, a save or the comparison changes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Notice {
    /// True when the merge had no step to run.
    nothing_copied: bool,
    /// What the plan left undone over the whole comparison. Dropped once the
    /// text is rebuilt, since it describes the plan, not the rows.
    finish: Finish,
}

impl Notice {
    fn text(&self, by_hand: &ByHand, has_center: bool) -> Option<String> {
        let mut parts = Vec::new();
        if !by_hand.is_empty() {
            parts.push(by_hand.notice(has_center, MESSAGE_NAMES));
        }
        parts.extend(self.finish.sentences(has_center));
        if parts.is_empty() {
            return None;
        }
        if self.nothing_copied {
            parts.insert(0, "Nothing is copied.".to_owned());
        }
        Some(parts.join(" "))
    }
}

/// What the planning worker hands back with a plan.
#[derive(Debug)]
pub struct Planned {
    plan: Box<OperationPlan>,
    by_hand: ByHand,
    overwrites: Overwrites,
    finish: Finish,
    /// Items a step left undone before that take part in this merge and that
    /// it has no step for, so the output already holds what the merge wants.
    settled: Vec<PathBuf>,
}

/// What the exit code of the view is read from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Outcome {
    /// True once a batch of this view has completed a step.
    wrote: bool,
    /// Whether the last comparison left a row for a person, kept while the
    /// next comparison runs.
    waiting: Option<bool>,
}

/// The folders one merge reads and writes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Folders {
    left: PathBuf,
    center: Option<PathBuf>,
    right: PathBuf,
    /// The separate output folder, where the session names one.
    output: Option<PathBuf>,
}

/// Where the comparison stands.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Status {
    Running(&'static str),
    Ready,
    Failed(String),
}

/// Where the merge operation stands.
#[derive(Debug)]
enum Stage {
    Idle,
    Planning,
    Confirming(Box<OperationPlan>),
    Running(Box<OperationPlan>),
    Summary(Box<OperationPlan>, Box<ExecutionReport>),
}

/// Work requested while a merge batch is still running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeferredComparison {
    /// Rebuild the comparison after the batch finishes.
    Rescan,
}

/// What the comparison worker posts back.
#[derive(Debug)]
pub enum CompareMessage {
    /// A step of the comparison started.
    Progress(&'static str),
    /// The comparison finished, with the rows whose output item was written
    /// after every input item.
    Ready(Box<MergeTree>, BTreeSet<PathBuf>),
    /// The comparison could not run.
    Failed(String),
}

impl Terminal for CompareMessage {
    fn is_terminal(&self) -> bool {
        !matches!(self, Self::Progress(_))
    }

    fn cancelled() -> Self {
        Self::Failed("The comparison stopped before it finished".to_owned())
    }

    fn panicked(detail: String) -> Self {
        Self::Failed(detail)
    }
}

/// What the planning worker posts back.
#[derive(Debug)]
pub enum MergePlanMessage {
    /// The plan with what it leaves undone, or why no plan was built.
    Done(Result<Box<Planned>, String>),
}

impl Terminal for MergePlanMessage {
    fn is_terminal(&self) -> bool {
        true
    }

    fn cancelled() -> Self {
        Self::Done(Err("Planning stopped before it finished".to_owned()))
    }

    fn panicked(detail: String) -> Self {
        Self::Done(Err(detail))
    }
}

/// A three way folder merge in one tab.
pub struct FolderMergeView {
    id: egui::Id,
    notify: Arc<dyn Fn() + Send + Sync>,
    folders: Folders,
    settings: FolderMergeSettings,
    engine: EngineOptions,
    archive_masks: ca_session::options::ArchiveOptions,
    operations: OperationOptions,
    confirm: bool,
    journal_directory: PathBuf,
    status: Status,
    compare_job: Option<Job<CompareMessage>>,
    plan_job: Option<Job<MergePlanMessage>>,
    exec_job: Option<Job<ExecMessage>>,
    deferred_comparison: Option<DeferredComparison>,
    ask: Option<Arc<Ask>>,
    progress: Arc<Mutex<ProgressState>>,
    tree: Option<Arc<MergeTree>>,
    filter: MergeFilter,
    ignore_same: bool,
    show_center: bool,
    visible: Vec<usize>,
    overrides: BTreeMap<PathBuf, Resolution>,
    selection: BTreeSet<PathBuf>,
    cursor: Option<usize>,
    active: Pane,
    stage: Stage,
    /// The items that wait for a merge by hand, as the last plan or the last
    /// change of the rows left them.
    by_hand: ByHand,
    /// The items merged by hand the plan on screen replaces.
    overwrites: Overwrites,
    /// Rows whose output item was written after every input item, as the
    /// last comparison read them.
    written_after: BTreeSet<PathBuf>,
    /// Saves from Text Merge in this session, by the key of the output path.
    saved: BTreeMap<String, SaveRecord>,
    /// Workers that resolve the path of a save no output spelling matched.
    resolve_jobs: Vec<Job<ResolveMessage>>,
    /// Items a step left undone that no later merge settled.
    undone: BTreeSet<PathBuf>,
    outcome: Outcome,
    report: ViewReport,
    actions: Vec<ViewAction>,
    message: Option<String>,
    /// The parts of `message` a merge wrote, where it wrote it.
    notice: Option<Notice>,
}

impl FolderMergeView {
    /// A merge over the folders a request names, journalling into the
    /// application's journal directory.
    #[must_use]
    pub fn from_request(request: &OpenRequest, context: &ViewContext, instance: u64) -> Self {
        Self::with_journal_directory(
            request,
            context,
            instance,
            ca_ui::paths::journal_directory(),
        )
    }

    /// A merge over the folders a request names, journalling into `journals`.
    #[must_use]
    pub fn with_journal_directory(
        request: &OpenRequest,
        context: &ViewContext,
        instance: u64,
        journals: PathBuf,
    ) -> Self {
        let settings = FolderMergeSettings::default();
        let archive_masks = context.options.stored.archives.clone();
        let engine = engine_of(&settings, &archive_masks);
        let mut view = Self {
            id: egui::Id::new(("folder-merge", instance)),
            notify: context.notify.clone(),
            folders: Folders {
                left: request.left.clone(),
                center: request.center.clone(),
                right: request.right.clone(),
                output: request.output.clone(),
            },
            operations: engine.operations.clone(),
            engine,
            archive_masks,
            settings,
            confirm: true,
            journal_directory: journals,
            status: Status::Running("Starting"),
            compare_job: None,
            plan_job: None,
            exec_job: None,
            deferred_comparison: None,
            ask: None,
            progress: Arc::new(Mutex::new(ProgressState::default())),
            tree: None,
            filter: MergeFilter::All,
            ignore_same: false,
            show_center: true,
            visible: Vec::new(),
            overrides: BTreeMap::new(),
            selection: BTreeSet::new(),
            cursor: None,
            active: Pane::Left,
            stage: Stage::Idle,
            by_hand: ByHand::default(),
            overwrites: Overwrites::default(),
            written_after: BTreeSet::new(),
            saved: BTreeMap::new(),
            resolve_jobs: Vec::new(),
            undone: BTreeSet::new(),
            outcome: Outcome::default(),
            report: ViewReport::new(
                ReportKind::Folder,
                egui::Id::new(("folder-merge-report", instance)),
                context.notify.clone(),
            ),
            actions: Vec::new(),
            message: None,
            notice: None,
        };
        view.restart();
        view
    }

    /// The folder the merge result is written to, where one is named.
    #[must_use]
    pub fn output_folder(&self) -> Option<&Path> {
        match &self.settings.merge.target {
            MergeTarget::Left => Some(&self.folders.left),
            MergeTarget::Right => Some(&self.folders.right),
            MergeTarget::OutputFolder => self.folders.output.as_deref(),
            _ => None,
        }
    }

    /// The compared tree, once the comparison has finished.
    #[must_use]
    pub fn tree(&self) -> Option<&MergeTree> {
        self.tree.as_deref()
    }

    /// The rows the display filter shows, in tree order.
    #[must_use]
    pub fn visible_rows(&self) -> Vec<&MergeRow> {
        let Some(tree) = self.tree.as_deref() else {
            return Vec::new();
        };
        self.visible
            .iter()
            .filter_map(|index| tree.rows.get(*index))
            .collect()
    }

    /// The row the cursor is on.
    #[must_use]
    pub fn cursor_row(&self) -> Option<&MergeRow> {
        let index = *self.visible.get(self.cursor?)?;
        self.tree.as_deref()?.rows.get(index)
    }

    /// Put the cursor on the visible row at `rel`, and make it the selection.
    pub fn select(&mut self, rel: &Path) -> bool {
        let Some(tree) = self.tree.as_deref() else {
            return false;
        };
        let Some(position) = self
            .visible
            .iter()
            .position(|index| tree.rows.get(*index).is_some_and(|row| row.rel == rel))
        else {
            return false;
        };
        self.cursor = Some(position);
        self.selection.clear();
        self.selection.insert(rel.to_path_buf());
        true
    }

    /// The display filter in force.
    #[must_use]
    pub const fn filter(&self) -> MergeFilter {
        self.filter
    }

    /// The resolutions the user chose, by relative path.
    #[must_use]
    pub const fn overrides(&self) -> &BTreeMap<PathBuf, Resolution> {
        &self.overrides
    }

    /// The input the next Copy to Output and Compare to Output read.
    pub fn set_active(&mut self, pane: Pane) {
        self.active = pane;
    }

    /// True when the ancestor column is drawn.
    #[must_use]
    pub const fn shows_center(&self) -> bool {
        self.show_center
    }

    /// True when a change both sides made alike counts as unchanged.
    #[must_use]
    pub const fn ignores_same_changes(&self) -> bool {
        self.ignore_same
    }

    /// What a file operation of this view carries across.
    pub fn operation_options_mut(&mut self) -> &mut OperationOptions {
        &mut self.operations
    }

    /// Take the confirmation the application options state.
    pub fn set_confirm_merge(&mut self, confirm: bool) {
        self.confirm = confirm;
    }

    /// True while a plan is on screen waiting for an answer.
    #[must_use]
    pub const fn is_confirming(&self) -> bool {
        matches!(self.stage, Stage::Confirming(_))
    }

    /// True once a batch has finished and its result is on screen.
    #[must_use]
    pub const fn has_summary(&self) -> bool {
        matches!(self.stage, Stage::Summary(_, _))
    }

    /// The plan on screen, running or finished.
    #[must_use]
    pub fn pending_plan(&self) -> Option<&OperationPlan> {
        match &self.stage {
            Stage::Confirming(plan) | Stage::Running(plan) | Stage::Summary(plan, _) => Some(plan),
            Stage::Idle | Stage::Planning => None,
        }
    }

    /// The report of the batch that has just finished.
    #[must_use]
    pub fn last_report(&self) -> Option<&ExecutionReport> {
        match &self.stage {
            Stage::Summary(_, report) => Some(report),
            _ => None,
        }
    }

    /// The notice on screen, where there is one.
    #[must_use]
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    /// Carry out the plan on screen.
    pub fn confirm_pending(&mut self) {
        if let Stage::Confirming(plan) = std::mem::replace(&mut self.stage, Stage::Idle) {
            self.execute(plan);
        }
    }

    /// Drop the plan on screen with nothing written.
    pub fn cancel_pending(&mut self) {
        if matches!(self.stage, Stage::Confirming(_)) {
            self.stage = Stage::Idle;
        }
    }

    /// Close the result of a finished batch and compare again.
    pub fn close_summary(&mut self) {
        if matches!(self.stage, Stage::Summary(_, _)) {
            self.stage = Stage::Idle;
            self.restart();
        }
    }

    /// Answer the question a running batch is waiting on.
    pub fn answer(&self, shown: &opjobs::PendingQuestion, answer: Answer) {
        if let Some(ask) = &self.ask {
            ask.answer(shown, answer);
        }
    }

    /// The question a running batch is waiting on.
    #[must_use]
    pub fn pending_question(&self) -> Option<opjobs::PendingQuestion> {
        self.ask.as_ref().and_then(|ask| ask.pending())
    }

    /// Take the requests the view made of the shell since the last frame.
    pub fn take_actions(&mut self) -> Vec<ViewAction> {
        std::mem::take(&mut self.actions)
    }

    /// The comparison the report is written from: the rows the filter shows,
    /// with the left and right items of each.
    #[must_use]
    pub fn report_payload(&self) -> (ca_ui::report::ReportMeta, ca_ui::report::Payload) {
        let meta = ca_ui::report::ReportMeta::new(
            self.folders.left.display().to_string(),
            self.folders.right.display().to_string(),
        )
        .with_title("Folder Merge Report");
        let request = self.whole_request();
        let rows = self
            .visible_rows()
            .into_iter()
            .map(|row| ca_ui::report::FolderRow {
                depth: u32::try_from(row.depth).unwrap_or(u32::MAX),
                name: row.name.clone(),
                relative_path: slash_path(&row.rel),
                is_dir: row.is_dir,
                status: match (leaves_for_person(row, &request), self.merged_by_hand(row)) {
                    (true, true) => ca_ui::report::EntryStatus::MergedByHand,
                    (true, false) => ca_ui::report::EntryStatus::MergeByHand,
                    (false, _) => report_status(row),
                },
                left: side_facts(row.left.as_ref()),
                right: side_facts(row.right.as_ref()),
                link: None,
            })
            .collect();
        (meta, ca_ui::report::Payload::Folder(rows))
    }

    /// Take the stored Archive Types masks and compare again when they change.
    pub fn follow_archive_masks(&mut self, masks: &ca_session::options::ArchiveOptions) {
        if &self.archive_masks == masks {
            return;
        }
        self.archive_masks = masks.clone();
        self.engine.archives =
            crate::settings::archive_types(&self.settings.handling.archive_handling, Some(masks));
        self.restart();
    }

    /// The archive types the jobs of this view open sides under.
    #[must_use]
    pub fn archive_types(&self) -> &ca_fs::ArchiveTypes {
        &self.engine.archives
    }

    fn restart(&mut self) {
        if let Some(tree) = self.tree.clone() {
            self.outcome.waiting = Some(self.leaves_any_for_person(&tree));
        }
        if let Some(job) = &self.plan_job {
            job.cancel();
        }
        self.plan_job = None;
        if matches!(self.stage, Stage::Planning | Stage::Confirming(_)) {
            self.stage = Stage::Idle;
            self.say(Some(
                "The merge plan was dropped because the comparison changed. Build a new plan before running it."
                    .to_owned(),
            ));
        }
        if self.exec_job.is_some() {
            if let Some(job) = self.compare_job.take() {
                job.cancel();
            }
            self.deferred_comparison = Some(DeferredComparison::Rescan);
            self.tree = None;
            self.visible.clear();
            self.cursor = None;
            self.status = Status::Running("Waiting for the current merge");
            return;
        }
        self.compare_job = None;
        self.tree = None;
        self.visible.clear();
        self.cursor = None;
        self.status = Status::Running("Scanning");
        let folders = self.folders.clone();
        let output = self.output_folder().map(Path::to_path_buf);
        let engine = Box::new(self.engine.clone());
        self.compare_job = Some(Job::spawn_notifying(
            move |emitter, cancel| {
                run_compare(&folders, output.as_deref(), &engine, emitter, cancel);
            },
            self.notify.clone(),
        ));
    }

    /// Put `text` in the status text, as no merge notice.
    fn say(&mut self, text: Option<String>) {
        self.message = text;
        self.notice = None;
    }

    /// Put the merge notice in the status text, from the items that wait now.
    fn show_notice(&mut self, notice: Notice) {
        self.message = notice.text(&self.by_hand, self.has_center());
        self.notice = self.message.is_some().then_some(notice);
    }

    /// Count the items that wait again and rebuild the merge notice from
    /// them, so the notice and the exit code read the same rows.
    fn refresh_notice(&mut self) {
        let Some(notice) = &self.notice else {
            return;
        };
        let Some(tree) = self.tree.clone() else {
            return;
        };
        let rebuilt = Notice {
            nothing_copied: notice.nothing_copied,
            finish: Finish::default(),
        };
        let saves = self.save_states(&tree);
        self.by_hand = ByHand::waiting_in(&tree, &self.whole_request(), &saves);
        self.show_notice(rebuilt);
    }

    /// Keep the items whose step did not complete, and forget the saves a
    /// completed step wrote over.
    fn record_steps(&mut self, plan: &OperationPlan, report: Option<&ExecutionReport>) {
        let done: BTreeSet<usize> = report
            .map(|report| {
                report
                    .results
                    .iter()
                    .filter(|result| result.outcome.is_done())
                    .map(|result| result.index)
                    .collect()
            })
            .unwrap_or_default();
        for step in &plan.steps {
            if done.contains(&step.index) {
                self.undone.remove(&step.rel);
                self.saved.remove(&path_key(&step.rel));
            } else {
                self.undone.insert(step.rel.clone());
            }
        }
    }

    /// Show a finished comparison, with the saves and the notice checked
    /// against it.
    fn take_comparison(&mut self, tree: Arc<MergeTree>, written_after: BTreeSet<PathBuf>) {
        self.check_saves(&tree);
        self.tree = Some(tree);
        self.written_after = written_after;
        self.status = Status::Ready;
        self.rebuild();
        self.refresh_notice();
    }

    /// Record the saves whose path a worker resolved below the output.
    fn poll_resolves(&mut self) {
        let mut resolved = Vec::new();
        self.resolve_jobs.retain_mut(|job| {
            resolved.extend(job.drain());
            !job.is_finished()
        });
        for message in resolved {
            if let ResolveMessage::Done(saved, Some(rel)) = message {
                self.record_save(&rel, saved.conflicts);
            }
        }
    }

    fn poll(&mut self) {
        self.report.tick();
        if let Some(job) = &mut self.compare_job {
            let messages = job.drain();
            let finished = job.is_finished();
            for message in messages {
                match message {
                    CompareMessage::Progress(step) => self.status = Status::Running(step),
                    CompareMessage::Ready(tree, written_after) => {
                        self.take_comparison(Arc::from(tree), written_after);
                    }
                    CompareMessage::Failed(reason) => self.status = Status::Failed(reason),
                }
            }
            if finished {
                self.compare_job = None;
            }
        }
        if let Some(job) = &mut self.plan_job {
            let messages = job.drain();
            if let Some(MergePlanMessage::Done(outcome)) = messages.into_iter().next() {
                self.plan_job = None;
                match outcome {
                    Ok(planned) => {
                        let Planned {
                            plan,
                            by_hand,
                            overwrites,
                            finish,
                            settled,
                        } = *planned;
                        for rel in &settled {
                            self.undone.remove(rel);
                        }
                        self.by_hand = by_hand;
                        self.overwrites = overwrites;
                        if plan.steps.is_empty() {
                            self.stage = Stage::Idle;
                            if self.by_hand.is_empty() && finish.is_empty() {
                                self.say(Some(FINISHED.to_owned()));
                            } else {
                                self.show_notice(Notice {
                                    nothing_copied: true,
                                    finish,
                                });
                            }
                        } else if self.confirm {
                            self.stage = Stage::Confirming(plan);
                        } else {
                            self.execute(plan);
                        }
                    }
                    Err(reason) => {
                        self.stage = Stage::Idle;
                        self.by_hand = ByHand::default();
                        self.overwrites = Overwrites::default();
                        self.say(Some(reason));
                    }
                }
            }
        }
        if let Some(job) = &mut self.exec_job {
            let messages = job.drain();
            if let Some(message) = messages.into_iter().next() {
                self.exec_job = None;
                self.ask = None;
                let plan = match std::mem::replace(&mut self.stage, Stage::Idle) {
                    Stage::Running(plan) => plan,
                    _ => Box::new(OperationPlan::new(
                        ca_fs::OperationKind::Merge,
                        Vec::new(),
                        self.operations.clone(),
                    )),
                };
                match message {
                    ExecMessage::Done(report) => {
                        if report.completed() > 0 {
                            self.outcome.wrote = true;
                        }
                        self.record_steps(&plan, Some(&report));
                        self.show_notice(Notice::default());
                        self.stage = Stage::Summary(plan, report);
                    }
                    ExecMessage::Failed(reason) => {
                        self.record_steps(&plan, None);
                        self.say(Some(reason));
                    }
                }
            }
        }
        self.poll_resolves();
        if self.deferred_comparison.is_some() && self.exec_job.is_none() {
            self.deferred_comparison = None;
            self.restart();
        }
    }

    /// Lay out the rows the filter shows.
    fn rebuild(&mut self) {
        let cursor_rel = self.cursor_row().map(|row| row.rel.clone());
        self.visible.clear();
        let Some(tree) = self.tree.clone() else {
            return;
        };
        let by_rel: HashMap<&Path, usize> = tree
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| (row.rel.as_path(), index))
            .collect();
        let mut shown = vec![false; tree.rows.len()];
        for (index, row) in tree.rows.iter().enumerate() {
            let own = self.filter.admits(row.status, self.ignore_same);
            if own {
                shown[index] = true;
                let mut parent = row.rel.parent();
                while let Some(folder) = parent {
                    if let Some(position) = by_rel.get(folder) {
                        shown[*position] = true;
                    }
                    parent = folder.parent();
                }
            }
        }
        self.visible = (0..tree.rows.len()).filter(|index| shown[*index]).collect();
        self.cursor = cursor_rel.and_then(|rel| {
            self.visible
                .iter()
                .position(|index| tree.rows[*index].rel == rel)
        });
    }

    fn set_filter(&mut self, filter: MergeFilter) {
        self.filter = filter;
        self.rebuild();
    }

    /// The rows a Take or a Copy to Output acts on: the selection, or the row
    /// under the cursor, each with everything below it.
    fn targets(&self) -> Vec<PathBuf> {
        if !self.selection.is_empty() {
            return self.selection.iter().cloned().collect();
        }
        self.cursor_row()
            .map(|row| vec![row.rel.clone()])
            .unwrap_or_default()
    }

    fn take(&mut self, pane: Pane) {
        let Some(tree) = self.tree.clone() else {
            return;
        };
        for target in self.targets() {
            for row in tree.rows.iter().filter(|row| row.rel.starts_with(&target)) {
                self.overrides
                    .insert(row.rel.clone(), Resolution::Take(pane));
            }
        }
        if self.message.as_deref() == Some(FINISHED) {
            self.say(None);
        }
        self.refresh_notice();
    }

    /// Move the cursor to the next or previous visible row that passes `test`.
    fn jump(&mut self, forward: bool, test: impl Fn(&MergeRow) -> bool) {
        let Some(tree) = self.tree.clone() else {
            return;
        };
        let count = self.visible.len();
        let candidates: Box<dyn Iterator<Item = usize>> = match (forward, self.cursor) {
            (true, Some(at)) => Box::new(at + 1..count),
            (true, None) => Box::new(0..count),
            (false, Some(at)) => Box::new((0..at).rev()),
            (false, None) => Box::new((0..count).rev()),
        };
        for position in candidates {
            if tree.rows.get(self.visible[position]).is_some_and(&test) {
                self.cursor = Some(position);
                self.selection.clear();
                if let Some(row) = tree.rows.get(self.visible[position]) {
                    self.selection.insert(row.rel.clone());
                }
                return;
            }
        }
    }

    fn start_merge(
        &mut self,
        selection: Option<BTreeSet<PathBuf>>,
        overrides: BTreeMap<PathBuf, Resolution>,
    ) {
        let Some(tree) = self.tree.clone() else {
            return;
        };
        let Some(output) = self.output_folder().map(Path::to_path_buf) else {
            self.say(Some(NO_OUTPUT.to_owned()));
            return;
        };
        let folders = self.folders.clone();
        let options = self.operations.clone();
        let automatic = self.settings.merge.automatic_merge;
        let saves = self.save_states(&tree);
        let undone = self.undone.clone();
        self.stage = Stage::Planning;
        self.say(None);
        self.plan_job = Some(Job::spawn_notifying(
            move |emitter, _cancel| {
                if output.is_file() {
                    emitter.send(MergePlanMessage::Done(Err(ARCHIVE_OUTPUT.to_owned())));
                    return;
                }
                let bases = MergeBases {
                    left: &folders.left,
                    center: folders.center.as_deref(),
                    right: &folders.right,
                    output: &output,
                };
                let request = MergeRequest {
                    overrides: &overrides,
                    selection: selection.as_ref(),
                    automatic,
                };
                let outcome = plan_merge(&tree, bases, &request, &options)
                    .map(|plan| {
                        let finish = if plan.steps.is_empty() {
                            Finish::of(&tree, bases, &request, &options, &plan)
                        } else {
                            Finish::default()
                        };
                        let stepped: BTreeSet<&Path> =
                            plan.steps.iter().map(|step| step.rel.as_path()).collect();
                        let settled = undone
                            .into_iter()
                            .filter(|rel| {
                                !stepped.contains(rel.as_path())
                                    && request.selection.is_none_or(|selected| {
                                        selected.iter().any(|chosen| rel.starts_with(chosen))
                                    })
                            })
                            .collect();
                        Box::new(Planned {
                            by_hand: ByHand::waiting_in(&tree, &request, &saves),
                            overwrites: Overwrites::of(&plan, &saves.merged),
                            finish,
                            settled,
                            plan: Box::new(plan),
                        })
                    })
                    .map_err(|refused| refused.to_string());
                emitter.send(MergePlanMessage::Done(outcome));
            },
            self.notify.clone(),
        ));
    }

    fn merge_selection(&mut self) {
        let selection = (!self.selection.is_empty()).then(|| self.selection.clone());
        self.start_merge(selection, self.overrides.clone());
    }

    fn copy_to_output(&mut self) {
        let targets = self.targets();
        if targets.is_empty() {
            return;
        }
        let Some(tree) = self.tree.clone() else {
            return;
        };
        let mut overrides = self.overrides.clone();
        for target in &targets {
            for row in tree.rows.iter().filter(|row| row.rel.starts_with(target)) {
                overrides.insert(row.rel.clone(), Resolution::Take(self.active));
            }
        }
        self.start_merge(Some(targets.into_iter().collect()), overrides);
    }

    fn execute(&mut self, plan: Box<OperationPlan>) {
        let cancel = Cancel::new();
        let ask = Ask::new(self.notify.clone(), cancel);
        self.progress = Arc::new(Mutex::new(ProgressState::default()));
        let containers = opjobs::Containers {
            paths: [
                Some(self.folders.left.clone()),
                self.folders.center.clone(),
                Some(self.folders.right.clone()),
            ]
            .into_iter()
            .flatten()
            .collect(),
            types: self.engine.archives.clone(),
        };
        self.exec_job = Some(opjobs::spawn_execute_reading(
            plan.clone(),
            self.journal_directory.clone(),
            Arc::clone(&ask),
            Arc::clone(&self.progress),
            self.notify.clone(),
            containers,
        ));
        self.ask = Some(ask);
        self.stage = Stage::Running(plan);
    }

    /// The request that opens the cursor row in the three way text merge.
    #[must_use]
    pub fn text_merge_request(&self) -> Option<OpenRequest> {
        let row = self.cursor_row()?;
        if !opens_in_text_merge(self.tree.as_deref()?, row) {
            return None;
        }
        let output = self.output_folder()?;
        let left = self.folders.left.join(&row.left.as_ref()?.rel);
        let right = self.folders.right.join(&row.right.as_ref()?.rel);
        let center = match (&self.folders.center, row.center.as_ref()) {
            (Some(base), Some(entry)) if !entry.is_dir => Some(base.join(&entry.rel)),
            _ => None,
        };
        Some(
            OpenRequest::new(SessionKind::TextMerge, left, right)
                .with_center(center)
                .with_output(Some(output.join(target_rel(row)))),
        )
    }

    fn text_merge_disabled_reason(&self) -> &'static str {
        if self.cursor_row().is_some_and(|row| row.text)
            && self
                .tree
                .as_deref()
                .is_some_and(|tree| !tree.text_merge_inputs_are_local())
        {
            "Text Merge does not open entries inside archives"
        } else {
            "Put the cursor on a text file both sides changed"
        }
    }

    /// The request that compares the active input with the output.
    #[must_use]
    pub fn compare_to_output_request(&self) -> Option<OpenRequest> {
        let output = self.output_folder()?.to_path_buf();
        let side = match self.active {
            Pane::Left => self.folders.left.clone(),
            Pane::Center => self.folders.center.clone()?,
            Pane::Right => self.folders.right.clone(),
        };
        Some(OpenRequest::new(SessionKind::FolderCompare, side, output))
    }

    fn idle(&self) -> bool {
        matches!(self.stage, Stage::Idle) && self.plan_job.is_none() && self.exec_job.is_none()
    }

    fn ready(&self) -> bool {
        self.status == Status::Ready && self.tree.is_some()
    }

    fn has_center(&self) -> bool {
        self.folders.center.is_some()
    }

    /// True when a merge of every row of `tree` leaves one for a person.
    fn leaves_any_for_person(&self, tree: &MergeTree) -> bool {
        let request = self.whole_request();
        tree.rows
            .iter()
            .any(|row| leaves_for_person(row, &request) && !self.merged_by_hand(row))
    }

    /// The save from Text Merge recorded for the output path of `row`.
    fn save_of(&self, row: &MergeRow) -> Option<&SaveRecord> {
        if self.saved.is_empty() {
            return None;
        }
        self.saved.get(&path_key(target_rel(row)))
    }

    /// True when a person has to merge `row` and the output holds their
    /// merge: a save from Text Merge in this session that left no conflict,
    /// or, without such a save, an output file written after every input
    /// item that copies none of them and marks no conflict.
    fn merged_by_hand(&self, row: &MergeRow) -> bool {
        row.status.needs_person()
            && self.save_of(row).map_or_else(
                || self.written_after.contains(&row.rel),
                |save| save.conflicts == 0,
            )
    }

    /// True when a person has to merge `row` and Text Merge saved its output
    /// with conflicts still marked.
    fn saved_with_conflicts(&self, row: &MergeRow) -> bool {
        row.status.needs_person() && self.save_of(row).is_some_and(|save| save.conflicts > 0)
    }

    /// The rows of `tree` merged by hand and saved with conflicts.
    fn save_states(&self, tree: &MergeTree) -> SaveStates {
        if self.saved.is_empty() {
            return SaveStates {
                merged: self.written_after.clone(),
                conflicted: BTreeSet::new(),
            };
        }
        let mut states = SaveStates::default();
        for row in &tree.rows {
            if self.merged_by_hand(row) {
                states.merged.insert(row.rel.clone());
            } else if self.saved_with_conflicts(row) {
                states.conflicted.insert(row.rel.clone());
            }
        }
        states
    }

    /// The input of the merge that is the output folder itself, where one is.
    const fn output_pane(&self) -> Option<Pane> {
        match self.settings.merge.target {
            MergeTarget::Left => Some(Pane::Left),
            MergeTarget::Right => Some(Pane::Right),
            _ => None,
        }
    }

    /// Record a save of the output item at `rel`, with the input items of its
    /// row as the comparison on screen lists them.
    fn record_save(&mut self, rel: &Path, conflicts: u32) {
        let key = path_key(rel);
        let output_pane = self.output_pane();
        let inputs = self.tree.as_deref().and_then(|tree| {
            tree.rows
                .iter()
                .find(|row| path_key(target_rel(row)) == key)
                .map(|row| input_facts(row, output_pane))
        });
        self.saved.insert(key, SaveRecord { conflicts, inputs });
        self.refresh_notice();
    }

    /// Drop every save whose row a new comparison lists with an input item
    /// of another size or time, or does not list; take the input items of a
    /// save that arrived with no comparison on screen.
    fn check_saves(&mut self, tree: &MergeTree) {
        if self.saved.is_empty() {
            return;
        }
        let output_pane = self.output_pane();
        let mut kept = BTreeMap::new();
        for row in &tree.rows {
            let key = path_key(target_rel(row));
            let Some(mut record) = self.saved.remove(&key) else {
                continue;
            };
            let now = input_facts(row, output_pane);
            match record.inputs {
                Some(then) if then != now => continue,
                Some(_) => {}
                None => record.inputs = Some(now),
            }
            kept.insert(key, record);
        }
        self.saved = kept;
    }

    /// A merge of every row with the resolutions the view holds now.
    fn whole_request(&self) -> MergeRequest<'_> {
        MergeRequest {
            overrides: &self.overrides,
            selection: None,
            automatic: self.settings.merge.automatic_merge,
        }
    }

    fn resolution(&self, row: &MergeRow) -> Resolution {
        match self.overrides.get(&row.rel) {
            Some(chosen) => *chosen,
            None if self.settings.merge.automatic_merge => ca_fs::merge::automatic_resolution(row),
            None => Resolution::Leave,
        }
    }

    /// The toolbar items this view declares, in the state it is in now.
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn toolbar_items(&self) -> Vec<toolbar::Item> {
        let ready = self.ready();
        let idle = ready && self.idle();
        vec![
            toolbar::Item::widget("filter", FILTER_COMBO_WIDTH),
            toolbar::Item::separator("separator-1"),
            toolbar::Item::command(
                "previous-conflict",
                Command::PreviousConflict,
                "Prev Conflict",
                self.accepts(Command::PreviousConflict),
                NOT_READY,
            ),
            toolbar::Item::command(
                "next-conflict",
                Command::NextConflict,
                "Next Conflict",
                self.accepts(Command::NextConflict),
                NOT_READY,
            ),
            toolbar::Item::separator("separator-2"),
            toolbar::Item::command(
                "take-left",
                Command::TakeLeft,
                "Take Left",
                self.accepts(Command::TakeLeft),
                NOT_READY,
            ),
            toolbar::Item::command(
                "take-center",
                Command::TakeCenter,
                "Take Center",
                self.accepts(Command::TakeCenter),
                NOT_READY,
            ),
            toolbar::Item::command(
                "take-right",
                Command::TakeRight,
                "Take Right",
                self.accepts(Command::TakeRight),
                NOT_READY,
            ),
            toolbar::Item::separator("separator-3"),
            toolbar::Item::command(
                "merge",
                Command::MergeFolders,
                "Merge",
                self.accepts(Command::MergeFolders),
                NO_OUTPUT,
            ),
            toolbar::Item::command(
                "copy-to-output",
                Command::CopyToOutput,
                "Copy to Output",
                self.accepts(Command::CopyToOutput),
                NO_OUTPUT,
            ),
            toolbar::Item::command(
                "text-merge",
                Command::OpenTextMerge,
                "Text Merge",
                self.accepts(Command::OpenTextMerge),
                self.text_merge_disabled_reason(),
            ),
            toolbar::Item::separator("separator-4"),
            toolbar::Item::toggle(
                "center-pane",
                Command::ToggleCenterPane,
                "Center Pane",
                self.has_center(),
                self.show_center,
            ),
            toolbar::Item::toggle(
                "ignore-same",
                Command::ToggleIgnoreSameChanges,
                "Ignore Same",
                ready,
                self.ignore_same,
            ),
            toolbar::Item::command("swap", Command::SwapSides, "Swap", idle, NOT_READY),
            toolbar::Item::command("reload", Command::Reload, "Reload", idle, NOT_READY),
            toolbar::Item::command("report", Command::CompareReport, "Report", ready, NOT_READY),
        ]
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let items = self.toolbar_items();
        let layout = toolbar::Layout::from_options(
            &ca_ui::options::runtime::current(ui.ctx()).stored.commands,
            toolbar::ToolbarView::FolderMerge,
        );
        let id = self.id;
        let current = self.filter;
        let mut chosen = None;
        let outcome = toolbar::show_for(
            toolbar::ToolbarView::FolderMerge,
            ui,
            id.with("toolbar"),
            &items,
            &layout,
            |ui, name| {
                if name == "filter" {
                    egui::ComboBox::from_id_salt(id.with("filter"))
                        .width(FILTER_COMBO_WIDTH - 10.0)
                        .selected_text(current.label())
                        .show_ui(ui, |ui| {
                            for option in MergeFilter::ALL {
                                if ui
                                    .selectable_label(current == option, option.label())
                                    .clicked()
                                {
                                    chosen = Some(option);
                                }
                            }
                        });
                }
            },
        );
        if let Some(filter) = chosen {
            self.set_filter(filter);
        }
        if let Some(command) = outcome.command {
            if self.accepts(command) {
                self.run(command);
            }
        }
    }

    fn header(&self, ui: &mut egui::Ui) {
        let names = |path: &Path| path.display().to_string();
        ui.horizontal_wrapped(|ui| {
            ca_ui::widgets::path_line(ui, "Left: ", &self.folders.left);
            if let Some(center) = &self.folders.center {
                ca_ui::widgets::path_line(ui, "Center: ", center);
            }
            ca_ui::widgets::path_line(ui, "Right: ", &self.folders.right);
            match self.output_folder() {
                Some(output) => {
                    ca_ui::widgets::path_text(ui, &format!("Output: {}", names(output)));
                }
                None => {
                    ui.label(NO_OUTPUT);
                }
            }
        });
    }

    fn status_line(&self, ui: &mut egui::Ui) {
        let text = match &self.status {
            Status::Running(step) => format!("{step}..."),
            Status::Failed(reason) => reason.clone(),
            Status::Ready => self.tree.as_deref().map_or_else(String::new, |tree| {
                let counts = tree.counts();
                format!(
                    "{} unchanged, {} left, {} right, {} same, {} mergeable, {} conflicts, {} unknown",
                    counts.unchanged,
                    counts.left,
                    counts.right,
                    counts.same,
                    counts.mergeable,
                    counts.conflicts,
                    counts.unknown
                )
            }),
        };
        ui.label(text);
        if let Some(message) = &self.message {
            ca_ui::widgets::wrapped_text(ui, message);
        }
    }

    /// The columns the rows are drawn in: the inputs shown, then the output.
    fn columns(&self) -> Vec<Option<Pane>> {
        let mut columns = vec![Some(Pane::Left)];
        if self.has_center() && self.show_center {
            columns.push(Some(Pane::Center));
        }
        columns.push(Some(Pane::Right));
        columns.push(None);
        columns
    }

    #[allow(
        clippy::too_many_lines,
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    fn rows_ui(&mut self, ui: &mut egui::Ui, context: &ViewContext) {
        let Some(tree) = self.tree.clone() else {
            return;
        };
        let palette = folder_merge::palette(Variant::from_dark_mode(ui.visuals().dark_mode));
        let columns = self.columns();
        let width = ui.available_width();
        let column_width = ((width - ACTION_WIDTH) / columns.len().max(1) as f32).max(40.0);
        let font = egui::FontId::proportional(13.0);
        let target = self.settings.merge.target.clone();
        let mut clicked: Option<(usize, Option<Pane>, bool, bool)> = None;
        ui.horizontal(|ui| {
            for column in &columns {
                let label = match column {
                    Some(Pane::Left) => "Left",
                    Some(Pane::Center) => "Center",
                    Some(Pane::Right) => "Right",
                    None => "Output",
                };
                ui.add_sized([column_width - 4.0, ROW_HEIGHT], egui::Label::new(label));
            }
            ui.add_sized([ACTION_WIDTH - 4.0, ROW_HEIGHT], egui::Label::new("Action"));
        });
        egui::ScrollArea::vertical()
            .id_salt(self.id.with("rows"))
            .auto_shrink([false, false])
            .show_rows(ui, ROW_HEIGHT, self.visible.len(), |ui, range| {
                for position in range {
                    let Some(row) = self
                        .visible
                        .get(position)
                        .and_then(|index| tree.rows.get(*index))
                    else {
                        continue;
                    };
                    let (rect, response) =
                        ui.allocate_exact_size(egui::vec2(width, ROW_HEIGHT), egui::Sense::click());
                    let painter = ui.painter_at(rect);
                    if self.selection.contains(&row.rel) || self.cursor == Some(position) {
                        painter.rect_filled(rect, 0.0, context.palette.folder_selection);
                    }
                    let text_color = palette.text(class_of(row.status, self.ignore_same));
                    let indent = INDENT * row.depth as f32;
                    for (column_index, column) in columns.iter().enumerate() {
                        let left = rect.left() + column_width * column_index as f32;
                        let cell = egui::Rect::from_min_size(
                            egui::pos2(left, rect.top()),
                            egui::vec2(column_width, ROW_HEIGHT),
                        );
                        if column.is_none() {
                            painter.rect_filled(cell, 0.0, palette.output_column);
                        }
                        let entry = match column {
                            Some(pane) => row.entry(*pane),
                            None => row.output.as_ref(),
                        };
                        if entry.is_some() {
                            let marker = egui::pos2(cell.left() + indent + 12.0, cell.center().y);
                            ca_ui::icons::merge_status_icon(row.status)
                                .paint_in_row(&painter, marker, ROW_HEIGHT, text_color);
                            let item = if row.is_dir {
                                ca_ui::icons::Icon::Folder
                            } else {
                                ca_ui::icons::Icon::File
                            };
                            item.paint_in_row(
                                &painter,
                                marker + egui::vec2(18.0, 0.0),
                                ROW_HEIGHT,
                                text_color,
                            );
                            let name = if row.is_dir {
                                format!("{}{}", row.name, std::path::MAIN_SEPARATOR)
                            } else {
                                row.name.clone()
                            };
                            painter.text(
                                egui::pos2(cell.left() + 42.0 + indent, cell.center().y),
                                egui::Align2::LEFT_CENTER,
                                name,
                                font.clone(),
                                text_color,
                            );
                        }
                    }
                    let resolution = self.resolution(row);
                    let action = if resolution == Resolution::Leave && self.merged_by_hand(row) {
                        MERGED_BY_HAND
                    } else if resolution == Resolution::Leave && self.saved_with_conflicts(row) {
                        SAVED_WITH_CONFLICTS
                    } else {
                        action_label(row, resolution, &target)
                    };
                    let action_icon = match action {
                        "Take left" => Some(ca_ui::icons::Icon::TakeLeft),
                        "Take center" => Some(ca_ui::icons::Icon::TakeCenter),
                        "Take right" => Some(ca_ui::icons::Icon::TakeRight),
                        "Delete" => Some(ca_ui::icons::Icon::Delete),
                        "Merge by hand" => Some(ca_ui::icons::Icon::Conflict),
                        "Copy to left" => Some(ca_ui::icons::Icon::CopyToLeft),
                        "Copy to right" => Some(ca_ui::icons::Icon::CopyToRight),
                        "Delete left" => Some(ca_ui::icons::Icon::DeleteLeft),
                        "Delete right" => Some(ca_ui::icons::Icon::DeleteRight),
                        _ => None,
                    };
                    if let Some(icon) = action_icon {
                        icon.paint_in_row(
                            &painter,
                            egui::pos2(
                                rect.left() + column_width * columns.len() as f32 + 12.0,
                                rect.center().y,
                            ),
                            ROW_HEIGHT,
                            text_color,
                        );
                    }
                    painter.text(
                        egui::pos2(
                            rect.left() + column_width * columns.len() as f32 + 24.0,
                            rect.center().y,
                        ),
                        egui::Align2::LEFT_CENTER,
                        action,
                        font.clone(),
                        text_color,
                    );
                    let response = match &row.error {
                        Some(reason) => response.on_hover_text(reason),
                        None => response.on_hover_text(row.status.label()),
                    };
                    if response.clicked() || response.double_clicked() {
                        let column = response.interact_pointer_pos().map_or(0, |pointer| {
                            ((pointer.x - rect.left()) / column_width).max(0.0) as usize
                        });
                        let additive = ui.input(|input| input.modifiers.command);
                        clicked = Some((
                            position,
                            columns.get(column).copied().flatten(),
                            additive,
                            response.double_clicked(),
                        ));
                    }
                }
            });
        if let Some((position, pane, additive, double)) = clicked {
            let Some(rel) = self
                .visible
                .get(position)
                .and_then(|index| tree.rows.get(*index))
                .map(|row| row.rel.clone())
            else {
                return;
            };
            if let Some(pane) = pane {
                self.active = pane;
            }
            if !additive {
                self.selection.clear();
            }
            if !self.selection.remove(&rel) {
                self.selection.insert(rel);
            }
            self.cursor = Some(position);
            if double {
                if let Some(request) = self.text_merge_request() {
                    self.actions.push(ViewAction::Open(request));
                }
            }
        }
    }

    fn overlays(&mut self, ui: &mut egui::Ui) {
        let id = self.id;
        let has_center = self.has_center();
        match std::mem::replace(&mut self.stage, Stage::Idle) {
            Stage::Confirming(plan) => {
                match confirm(
                    ui,
                    id.with("confirm"),
                    &plan,
                    (&self.by_hand, &self.overwrites),
                    has_center,
                ) {
                    Some(true) => self.execute(plan),
                    Some(false) => {}
                    None => self.stage = Stage::Confirming(plan),
                }
            }
            Stage::Running(plan) => {
                self.stage = Stage::Running(plan);
                if let Some(question) = self.pending_question() {
                    if let Some(answer) = dialogs::question(
                        ui,
                        id.with(("question", question.sequence)),
                        &question.question,
                    ) {
                        self.answer(&question, answer);
                    }
                } else {
                    let state = self
                        .progress
                        .lock()
                        .map(|state| state.clone())
                        .unwrap_or_default();
                    if dialogs::progress(ui, id.with("progress"), &state) {
                        if let Some(job) = &self.exec_job {
                            job.cancel();
                        }
                    }
                }
            }
            Stage::Summary(plan, report) => {
                let (notes, listed) = if self.by_hand.is_empty() {
                    (Vec::new(), Vec::new())
                } else {
                    (
                        self.by_hand.sentences(has_center),
                        self.by_hand.dialog_lines(),
                    )
                };
                if dialogs::summary(ui, id.with("summary"), &plan, &report, &notes, &listed) {
                    self.restart();
                } else {
                    self.stage = Stage::Summary(plan, report);
                }
            }
            other => self.stage = other,
        }
        if self.report.is_open() {
            if self.report.draw(ui) == ca_ui::report::ReportAction::Write {
                let (meta, payload) = self.report_payload();
                self.report.start(meta, payload, 0);
            }
            if let Some(text) = self.report.take_clipboard() {
                ui.ctx().copy_text(text);
            }
        }
    }
}

/// The plan, the items merged by hand it replaces, the items it leaves for a
/// merge by hand and its refusals, with the two buttons. `None` while neither
/// button is pressed, `Some(true)` to carry it out.
fn confirm(
    ui: &egui::Ui,
    id: egui::Id,
    plan: &OperationPlan,
    left: (&ByHand, &Overwrites),
    has_center: bool,
) -> Option<bool> {
    let (by_hand, overwrites) = left;
    let shown = modal(ui, id, "Merge", |ui| {
        ui.label(format!(
            "{} steps, {} bytes",
            plan.steps.len(),
            ca_ui::format::format_bytes(plan.total_bytes())
        ));
        if !overwrites.is_empty() {
            ui.separator();
            ca_ui::widgets::wrapped_text(ui, &overwrites.sentence());
        }
        if !by_hand.is_empty() {
            ui.separator();
            by_hand.show_sentences(ui, has_center);
        }
        egui::ScrollArea::vertical()
            .id_salt(id.with("body"))
            .max_height(260.0)
            .show(ui, |ui| {
                for step in &plan.steps {
                    ca_ui::widgets::wrapped_text(ui, &dialogs::describe(&step.action));
                }
                if !overwrites.is_empty() {
                    ui.separator();
                    for line in overwrites.lines() {
                        ca_ui::widgets::wrapped_text(ui, &line);
                    }
                }
                if !by_hand.is_empty() {
                    ui.separator();
                    for line in by_hand.dialog_lines() {
                        ca_ui::widgets::wrapped_text(ui, &line);
                    }
                }
                if !plan.skipped.is_empty() {
                    ui.separator();
                    ui.label(format!("{} items are left alone", plan.skipped.len()));
                    for skip in &plan.skipped {
                        ca_ui::widgets::wrapped_text(
                            ui,
                            &format!("{}: {}", slash_path(&skip.path), skip.reason),
                        );
                    }
                }
            });
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            if ui.button("Merge").clicked() {
                return Some(true);
            }
            if ui.button("Cancel").clicked() {
                return Some(false);
            }
            None
        })
        .inner
    });
    match shown {
        Some(answer) => answer,
        None => Some(false),
    }
}

fn report_status(row: &MergeRow) -> ca_ui::report::EntryStatus {
    use ca_ui::report::EntryStatus;
    match row.status {
        MergeStatus::Unchanged | MergeStatus::SameChange(_) => EntryStatus::Same,
        MergeStatus::Unknown => EntryStatus::Error,
        _ => match (row.left.is_some(), row.right.is_some()) {
            (true, false) => EntryStatus::LeftOrphan,
            (false, true) => EntryStatus::RightOrphan,
            _ => EntryStatus::Different,
        },
    }
}

fn side_facts(entry: Option<&ca_fs::Entry>) -> ca_ui::report::SideFacts {
    match entry {
        None => ca_ui::report::SideFacts::absent(),
        Some(entry) => ca_ui::report::SideFacts {
            present: true,
            size: (!entry.is_dir).then_some(entry.size),
            timestamp: entry.modified.map(ca_ui::report::format_timestamp),
            ..ca_ui::report::SideFacts::default()
        },
    }
}

fn local_path(location: Option<&SideLocation>) -> Option<PathBuf> {
    match location {
        Some(SideLocation::Local { path, .. }) => Some(path.as_path().to_path_buf()),
        _ => None,
    }
}

/// Scan every folder and compare them, on a worker.
fn run_compare(
    folders: &Folders,
    output: Option<&Path>,
    engine: &EngineOptions,
    emitter: &Emitter<CompareMessage>,
    cancel: &Cancel,
) {
    match compare_folders(folders, output, engine, emitter, cancel) {
        Ok((tree, written_after)) => {
            emitter.send(CompareMessage::Ready(Box::new(tree), written_after));
        }
        Err(reason) => {
            emitter.send(CompareMessage::Failed(reason));
        }
    }
}

/// The rows a person has to merge whose output file was written after every
/// input item, copies none of them and marks no conflict. Reads the output
/// file of each such row, so it runs on the comparison worker.
fn written_after_inputs(
    tree: &MergeTree,
    folders: &Folders,
    output: &Path,
    sources: MergeSources<'_>,
    engine: &EngineOptions,
    cancel: &Cancel,
) -> BTreeSet<PathBuf> {
    let bases = MergeBases {
        left: &folders.left,
        center: folders.center.as_deref(),
        right: &folders.right,
        output,
    };
    tree.rows
        .iter()
        .filter(|row| {
            row.status.needs_person()
                && output_written_after_inputs(row, &engine.compare.quick)
                && !output_copies_an_input(row, bases, Some(sources), cancel.as_fs())
                && !(row.text && output_holds_conflict_markers(row, output))
        })
        .map(|row| row.rel.clone())
        .collect()
}

fn compare_folders(
    folders: &Folders,
    output: Option<&Path>,
    engine: &EngineOptions,
    emitter: &Emitter<CompareMessage>,
    cancel: &Cancel,
) -> Result<(MergeTree, BTreeSet<PathBuf>), String> {
    emitter.send(CompareMessage::Progress("Scanning"));
    let open = |path: &Path| -> Result<Source, String> {
        crate::jobs::open_side(path, &engine.archives)
            .map_err(|error| format!("{}: {error}", path.display()))
    };
    let scan_input = |source: &Source| -> Result<ScanResult, String> {
        scan_source(source, &engine.scan, cancel.as_fs(), &|_| {})
            .map_err(|error| format!("{}: {error}", source.label()))
    };
    let scan = |path: &Path| -> Result<ScanResult, String> {
        scan_with(path, &engine.scan, cancel.as_fs(), &|_| {})
            .map_err(|error| format!("{}: {error}", path.display()))
    };
    let left_source = open(&folders.left)?;
    let center_source = folders.center.as_deref().map(open).transpose()?;
    let right_source = open(&folders.right)?;
    let left = scan_input(&left_source)?;
    let center = center_source.as_ref().map(scan_input).transpose()?;
    let right = scan_input(&right_source)?;
    let output_scan = match output {
        Some(path) if path == folders.left || path == folders.right => None,
        Some(path) if path.is_dir() => Some(scan(path)?),
        _ => None,
    };
    let output_listing = match output {
        Some(path) if path == folders.left => Some(&left),
        Some(path) if path == folders.right => Some(&right),
        _ => output_scan.as_ref(),
    };
    emitter.send(CompareMessage::Progress("Comparing"));
    let names = crate::jobs::name_filters_of(engine, "");
    let mut others = engine.other_filters.clone();
    others.left_root.clone_from(&folders.left);
    others.right_root.clone_from(&folders.right);
    let options = FolderMergeOptions {
        compare: engine.compare.clone(),
        case: engine.alignment.case,
        text_limit: engine.compare.content.binary_threshold_bytes,
        ..FolderMergeOptions::default()
    };
    let rules = (engine.compare.content.method == ca_fs::ContentMethod::Rules).then(|| {
        RulesEngine::with_builtin_formats().with_format_lists(
            engine.enabled_formats.clone(),
            engine.disabled_formats.clone(),
        )
    });
    let unnamed = PathBuf::new();
    let mut tree = compare3_sources(
        MergeInputs {
            left: &left,
            center: center.as_ref(),
            right: &right,
            output: output_listing,
        },
        MergeBases {
            left: &folders.left,
            center: folders.center.as_deref(),
            right: &folders.right,
            output: output.unwrap_or(&unnamed),
        },
        MergeSources {
            left: &left_source,
            center: center_source.as_ref(),
            right: &right_source,
        },
        &options,
        MergeFilters {
            names: &names,
            others: &others,
            context: &FilterContext::default(),
        },
        rules
            .as_ref()
            .map(|engine| engine as &dyn ca_fs::RulesComparer),
        cancel.as_fs(),
    );
    tree.set_text_merge_inputs_are_local(
        left_source.is_local_folder()
            && right_source.is_local_folder()
            && center_source.as_ref().is_none_or(Source::is_local_folder),
    );
    let sources = MergeSources {
        left: &left_source,
        center: center_source.as_ref(),
        right: &right_source,
    };
    let written_after = output.map_or_else(BTreeSet::new, |output| {
        written_after_inputs(&tree, folders, output, sources, engine, cancel)
    });
    Ok((tree, written_after))
}

impl ca_ui::view::ViewFactory for FolderMergeView {
    fn create(left: PathBuf, right: PathBuf, context: &ViewContext, instance: u64) -> Self {
        Self::from_request(
            &OpenRequest::new(SessionKind::FolderMerge, left, right),
            context,
            instance,
        )
    }

    fn create_from(request: &OpenRequest, context: &ViewContext, instance: u64) -> Self {
        Self::from_request(request, context, instance)
    }
}

impl SessionView for FolderMergeView {
    fn kind(&self) -> Option<ca_session::SessionKind> {
        Some(ca_session::SessionKind::FolderMerge)
    }

    fn title(&self) -> String {
        let name = |path: &Path| {
            path.file_name().map_or_else(
                || path.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            )
        };
        format!(
            "{} - {} (merge)",
            name(&self.folders.left),
            name(&self.folders.right)
        )
    }

    fn menu_view(&self) -> MenuView {
        MenuView::Merge
    }

    fn tick(&mut self) {
        self.poll();
    }

    fn ui(&mut self, ui: &mut egui::Ui, context: &ViewContext) -> Vec<ViewAction> {
        self.poll();
        let options = ca_ui::options::current(ui.ctx());
        self.confirm = options.stored.file_operations.confirm_merge;
        self.follow_archive_masks(&options.stored.archives);
        self.report.poll(ui.ctx());
        egui::TopBottomPanel::top(self.id.with("head")).show_inside(ui, |ui| {
            self.header(ui);
            self.toolbar(ui);
        });
        egui::TopBottomPanel::bottom(self.id.with("status")).show_inside(ui, |ui| {
            self.status_line(ui);
        });
        egui::CentralPanel::default().show_inside(ui, |ui| {
            self.rows_ui(ui, context);
        });
        self.overlays(ui);
        self.take_actions()
    }

    fn commands(&self) -> Vec<CommandState> {
        view::declare(HANDLED, |command| self.accepts(command))
    }

    fn accepts(&self, command: Command) -> bool {
        let ready = self.ready();
        let idle = self.idle();
        let has_target = !self.selection.is_empty() || self.cursor.is_some();
        let any = |test: fn(&MergeRow) -> bool| {
            self.tree.as_deref().is_some_and(|tree| {
                self.visible
                    .iter()
                    .any(|index| tree.rows.get(*index).is_some_and(test))
            })
        };
        match command {
            Command::SwapSides | Command::Reload => {
                idle && !matches!(self.status, Status::Running(_))
            }
            Command::CompareReport | Command::ToggleIgnoreSameChanges => ready,
            Command::SelectAll => ready && !self.visible.is_empty(),
            Command::NextDifference | Command::PreviousDifference => {
                ready && any(|row| row.status != MergeStatus::Unchanged)
            }
            Command::NextConflict | Command::PreviousConflict => {
                ready && any(|row| row.status.needs_person())
            }
            Command::TakeLeft | Command::TakeRight => ready && idle && has_target,
            Command::TakeCenter => ready && idle && has_target && self.has_center(),
            Command::ToggleCenterPane => self.has_center(),
            Command::MergeFolders => ready && idle && self.output_folder().is_some(),
            Command::CopyToOutput => ready && idle && has_target && self.output_folder().is_some(),
            Command::OpenTextMerge => self.text_merge_request().is_some(),
            Command::CompareToOutput => self.compare_to_output_request().is_some(),
            other => MergeFilter::of(other).is_some() && ready,
        }
    }

    fn run(&mut self, command: Command) {
        match command {
            Command::SwapSides => {
                std::mem::swap(&mut self.folders.left, &mut self.folders.right);
                self.overrides.clear();
                self.selection.clear();
                self.restart();
            }
            Command::Reload => self.restart(),
            Command::CompareReport => self.report.request(),
            Command::SelectAll => {
                let rels: Vec<PathBuf> = self
                    .visible_rows()
                    .iter()
                    .map(|row| row.rel.clone())
                    .collect();
                self.selection = rels.into_iter().collect();
            }
            Command::NextDifference => self.jump(true, |row| row.status != MergeStatus::Unchanged),
            Command::PreviousDifference => {
                self.jump(false, |row| row.status != MergeStatus::Unchanged);
            }
            Command::NextConflict => self.jump(true, |row| row.status.needs_person()),
            Command::PreviousConflict => self.jump(false, |row| row.status.needs_person()),
            Command::ToggleIgnoreSameChanges => {
                self.ignore_same = !self.ignore_same;
                self.rebuild();
            }
            Command::TakeLeft => self.take(Pane::Left),
            Command::TakeCenter => self.take(Pane::Center),
            Command::TakeRight => self.take(Pane::Right),
            Command::ToggleCenterPane => self.show_center = !self.show_center,
            Command::MergeFolders => self.merge_selection(),
            Command::CopyToOutput => self.copy_to_output(),
            Command::OpenTextMerge => {
                if let Some(request) = self.text_merge_request() {
                    self.actions.push(ViewAction::Open(request));
                }
            }
            Command::CompareToOutput => {
                if let Some(request) = self.compare_to_output_request() {
                    self.actions.push(ViewAction::Open(request));
                }
            }
            other => {
                if let Some(filter) = MergeFilter::of(other) {
                    self.set_filter(filter);
                }
            }
        }
    }

    fn apply_settings(&mut self, settings: &SessionSettings) {
        let SessionSettings::FolderMerge(merge) = settings else {
            return;
        };
        if let Some(left) = local_path(merge.specs.left.as_ref()) {
            self.folders.left = left;
        }
        if let Some(right) = local_path(merge.specs.right.as_ref()) {
            self.folders.right = right;
        }
        self.folders.center = local_path(merge.specs.ancestor.as_ref());
        self.folders.output = local_path(merge.specs.output.as_ref());
        self.settings = merge.clone();
        self.engine = engine_of(merge, &self.archive_masks);
        let recycle = self.operations.use_recycle_bin;
        self.operations = self.engine.operations.clone();
        self.operations.use_recycle_bin = recycle;
        self.overrides.clear();
        self.selection.clear();
        self.saved.clear();
        self.resolve_jobs.clear();
        self.undone.clear();
        self.restart();
    }

    fn settings(&self) -> Option<SessionSettings> {
        let mut settings = self.settings.clone();
        settings.specs =
            ca_ui::view::with_sides(&settings.specs, &self.folders.left, &self.folders.right);
        settings.specs.ancestor = self
            .folders
            .center
            .as_deref()
            .and_then(ca_ui::view::side_location);
        settings.specs.output = self
            .folders
            .output
            .as_deref()
            .and_then(ca_ui::view::side_location);
        Some(SessionSettings::FolderMerge(settings))
    }

    fn is_ready(&self) -> bool {
        !matches!(self.status, Status::Running(_))
    }

    fn notice(&self) -> Option<String> {
        match &self.status {
            Status::Failed(reason) => Some(reason.clone()),
            _ => self.message.clone(),
        }
    }

    fn on_close(&mut self) {
        if let Some(job) = &self.compare_job {
            job.cancel();
        }
        if let Some(job) = &self.plan_job {
            job.cancel();
        }
    }

    fn may_close(&mut self) -> bool {
        // A batch that is writing files is not abandoned by closing the tab.
        !self.is_busy()
    }

    fn is_busy(&self) -> bool {
        matches!(self.stage, Stage::Running(_))
    }

    fn file_saved(&mut self, saved: &SavedFile) {
        let Some(output) = self.output_folder().map(Path::to_path_buf) else {
            return;
        };
        if let Some(rel) = relative_to(&saved.path, &output) {
            self.record_save(&rel, saved.conflicts);
            return;
        }
        // Resolving a link or junction reads the disk, so a worker does it.
        let saved = saved.clone();
        self.resolve_jobs.push(Job::spawn_notifying(
            move |emitter, _cancel| {
                let rel = std::fs::canonicalize(&saved.path)
                    .ok()
                    .zip(std::fs::canonicalize(&output).ok())
                    .and_then(|(path, root)| relative_to(&path, &root));
                emitter.send(ResolveMessage::Done(saved, rel));
            },
            self.notify.clone(),
        ));
    }

    fn exit_code(&self) -> Option<i32> {
        let waiting = match self.tree.as_deref() {
            Some(tree) => self.leaves_any_for_person(tree),
            None => self.outcome.waiting?,
        };
        if !self.undone.is_empty() {
            return Some(EXIT_STEP_UNDONE);
        }
        Some(match (waiting, self.outcome.wrote) {
            (false, _) => EXIT_SUCCESS,
            (true, true) => EXIT_LEFT_FOR_HAND,
            (true, false) => EXIT_LEFT_FOR_HAND_NOTHING_WRITTEN,
        })
    }
}

/// The options of a merge session under the stored Archive Types masks.
fn engine_of(
    settings: &FolderMergeSettings,
    masks: &ca_session::options::ArchiveOptions,
) -> EngineOptions {
    let mut engine = merge_options_of(settings);
    engine.archives =
        crate::settings::archive_types(&settings.handling.archive_handling, Some(masks));
    engine
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{action_label, class_of, DeferredComparison, MergeFilter, Stage};
    use crate::opjobs::ExecMessage;
    use ca_fs::{
        Change, MergeStatus, OperationKind, OperationOptions, OperationPlan, Pane, Resolution,
    };
    use ca_session::settings::folder::MergeTarget;
    use ca_ui::theme::folder_merge::FolderMergeClass;

    #[test]
    fn the_merge_reads_the_stored_archive_masks() {
        let mut stored = ca_session::options::ProgramOptions::default();
        stored
            .archives
            .masks
            .insert("zip".to_owned(), "*.special".to_owned());
        let mut context = ca_ui::testing::context();
        context.options = std::sync::Arc::new(ca_ui::options::AppOptions {
            stored: stored.clone(),
            ..ca_ui::options::AppOptions::default()
        });
        let dir = tempfile::tempdir().unwrap();
        let request = ca_ui::view::OpenRequest::new(
            ca_session::SessionKind::FolderMerge,
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
        );
        let mut view = super::FolderMergeView::with_journal_directory(
            &request,
            &context,
            1,
            dir.path().join("journals"),
        );
        assert_eq!(
            view.archive_types().format_for_name("a.special"),
            Some(ca_fs::ArchiveFormat::Zip)
        );
        assert_eq!(view.archive_types().format_for_name("a.zip"), None);
        stored
            .archives
            .masks
            .insert("zip".to_owned(), "*.other".to_owned());
        view.follow_archive_masks(&stored.archives);
        assert_eq!(
            view.archive_types().format_for_name("a.other"),
            Some(ca_fs::ArchiveFormat::Zip)
        );
    }

    #[test]
    fn archive_mask_changes_wait_to_rescan_until_a_running_merge_finishes() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left");
        let right = dir.path().join("right");
        std::fs::create_dir_all(&left).unwrap();
        std::fs::create_dir_all(&right).unwrap();
        let request =
            ca_ui::view::OpenRequest::new(ca_session::SessionKind::FolderMerge, left, right);
        let mut view = super::FolderMergeView::with_journal_directory(
            &request,
            &ca_ui::testing::context(),
            1,
            dir.path().join("journals"),
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while view.tree.is_none() && std::time::Instant::now() < deadline {
            view.poll();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(view.tree.is_some());

        let (release, wait) = std::sync::mpsc::channel();
        view.exec_job = Some(ca_ui::worker::Job::spawn(move |emitter, _| {
            wait.recv().unwrap();
            emitter.send(ExecMessage::Done(Box::default()));
        }));
        view.stage = Stage::Running(Box::new(OperationPlan::new(
            OperationKind::Merge,
            Vec::new(),
            OperationOptions::default(),
        )));
        let mut masks = ca_session::options::ArchiveOptions::default();
        masks.masks.insert("zip".to_owned(), "*.special".to_owned());
        view.follow_archive_masks(&masks);
        assert!(view.compare_job.is_none());
        assert_eq!(view.deferred_comparison, Some(DeferredComparison::Rescan));
        assert!(view.tree.is_none());

        release.send(()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while view.tree.is_none() && std::time::Instant::now() < deadline {
            view.poll();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(view.tree.is_some());
        assert_eq!(view.deferred_comparison, None);
    }

    #[test]
    fn every_filter_is_reached_by_its_own_command() {
        for filter in MergeFilter::ALL {
            assert_eq!(MergeFilter::of(filter.command()), Some(filter));
            assert!(!filter.label().is_empty());
        }
    }

    #[test]
    fn ignore_same_changes_counts_a_same_change_as_unchanged() {
        let same = MergeStatus::SameChange(Change::Modified);
        assert!(MergeFilter::Mergeable.admits(same, false));
        assert!(!MergeFilter::Mergeable.admits(same, true));
        assert!(MergeFilter::Unchanged.admits(same, true));
        assert!(!MergeFilter::Changes.admits(same, true));
        assert_eq!(class_of(same, true), FolderMergeClass::Same);
        assert_eq!(class_of(same, false), FolderMergeClass::Mergeable);
    }

    #[test]
    fn the_left_and_right_filters_show_one_sided_changes_only() {
        let left = MergeStatus::LeftChange(Change::Added);
        assert!(MergeFilter::LeftChanges.admits(left, false));
        assert!(!MergeFilter::RightChanges.admits(left, false));
        assert!(!MergeFilter::LeftChanges.admits(MergeStatus::Conflict, false));
        assert!(MergeFilter::Conflicts.admits(MergeStatus::Conflict, false));
        assert!(!MergeFilter::None.admits(MergeStatus::Unchanged, false));
    }

    #[test]
    fn the_action_names_the_output_it_is_written_to() {
        let mut row = ca_fs::MergeRow {
            rel: "a.txt".into(),
            name: "a.txt".into(),
            depth: 0,
            is_dir: false,
            left: None,
            center: None,
            right: None,
            output: None,
            status: MergeStatus::RightChange(Change::Deleted),
            text: false,
            error: None,
            output_uncertain: false,
            output_holds_excluded: false,
        };
        let take_right = Resolution::Take(Pane::Right);
        assert_eq!(
            action_label(&row, take_right, &MergeTarget::Left),
            "Delete left"
        );
        assert_eq!(
            action_label(&row, take_right, &MergeTarget::OutputFolder),
            "Delete"
        );
        assert_eq!(action_label(&row, take_right, &MergeTarget::Right), "");
        row.status = MergeStatus::Conflict;
        assert_eq!(
            action_label(&row, Resolution::Leave, &MergeTarget::OutputFolder),
            "Merge by hand"
        );
    }
}
