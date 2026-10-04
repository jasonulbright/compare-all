//! Editing a registry comparison.
//!
//! Every command adds a step of plan operations to [`History`]. The panes show
//! the models as read with the plan applied, rebuilt on a worker. Nothing
//! reaches a source until Save: an export file is written through the checked
//! save of `ca_ui::save`, and a live key receives the writes that turn what it
//! last received into what the panes show, after a confirmation that states
//! the counts and after a restore file of the touched keys is written to the
//! journals folder.

use super::RecordsView;
use crate::editor::{from_display, ValueForm};
use crate::history::{plan_side, History};
use crate::jobs::{self, Side as SideData};
use crate::model::{Node, Side};
use ca_records::registry::live::{self, WriteConsent};
use ca_records::registry::plan::{
    is_within, live_steps, preview_value, subtree, EditOp, LiveStep, StepCounts,
};
use ca_records::registry::{LiveOptions, RegFile, ValueKind, ValueName};
use ca_ui::command::Command;
use ca_ui::save::{self, Baseline, RealFileSystem, SaveOutcome, Stamp};
use ca_ui::worker::{Job, Terminal};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Type name of the marker row an export file's key deletion shows as.
const KEY_DELETE_MARKER: &str = "REG_KEY_DELETE";

/// Why a rename that changes only the case of a name is refused.
const CASE_ONLY: &str = "The new name differs only in case, which the registry ignores.";

/// Why a live side cannot be written on this platform.
pub const LIVE_UNAVAILABLE: &str = "This platform has no Windows registry to write to";

/// Every command the registry view adds for editing.
pub const EDIT_COMMANDS: &[Command] = &[
    Command::SetAsBaseKeys,
    Command::SetBothAsBaseKeys,
    Command::SetAsBaseKeyOnOtherSide,
    Command::UpOneLevelLeft,
    Command::UpOneLevelRight,
    Command::UpOneLevelBoth,
    Command::Undo,
    Command::Redo,
    Command::CopyToRight,
    Command::CopyToLeft,
    Command::CopyToOtherSide,
    Command::Delete,
    Command::Rename,
    Command::NewKey,
    Command::NewValue,
    Command::Modify,
    Command::CopyKeyName,
    Command::Export,
    Command::ExportAll,
    Command::SaveFile,
    Command::SaveBoth,
];

/// What the view says when an edit or a save is asked for while the editing
/// switch of the session is on.
const EDITING_OFF: &str = "Editing is turned off for this session";

/// Why a copy waits for the comparison.
const NOT_COMPARED: &str = "Available once the comparison finishes";
/// Why a copy is refused after the comparison failed.
const FAILED: &str = "The comparison failed. Use Reload to compare again";
/// Why a copy is refused after the comparison was stopped.
const STOPPED: &str = "The comparison was stopped. Use Reload to compare again";
/// Why a copy is refused while a side read from the clipboard is shown.
const CLIPBOARD_SIDE: &str = "A side comes from the clipboard, so no side takes an edit";
/// Why a copy waits for a save, an export or a write to a live key.
const WRITING: &str = "Available once the current write finishes";
/// Why a copy waits for an answer.
const QUESTION_OPEN: &str = "Answer the open question first";
/// Why a copy has nothing to act on.
const NOTHING_CHOSEN: &str = "Select the items to copy first";

/// Commands that change a side or write one.
const CHANGES_A_SIDE: &[Command] = &[
    Command::Undo,
    Command::Redo,
    Command::CopyToRight,
    Command::CopyToLeft,
    Command::CopyToOtherSide,
    Command::Delete,
    Command::Rename,
    Command::NewKey,
    Command::NewValue,
    Command::Modify,
    Command::SaveFile,
    Command::SaveBoth,
];

/// An editor open inside the view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Panel {
    /// A new value, or the type and data of an existing one.
    Value {
        /// Side the value is on.
        side: Side,
        /// Key that holds the value.
        key_path: String,
        /// What the editor holds.
        form: ValueForm,
        /// True for New Value, false for Modify.
        is_new: bool,
    },
    /// The name of a new key.
    NewKey {
        /// Side the key is created on.
        side: Side,
        /// Key the new key goes under.
        parent: String,
        /// The name typed so far.
        name: String,
    },
    /// A new name for a key or a value.
    Rename {
        /// Side the item is on.
        side: Side,
        /// The key, or the key that holds the value.
        key_path: String,
        /// The value, when a value is renamed.
        value: Option<ValueName>,
        /// The name typed so far.
        name: String,
    },
}

/// A question the view waits on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prompt {
    /// Writes to a live key wait for a confirmation.
    ConfirmLive {
        /// Side the writes go to.
        side: Side,
        /// Base key of that side.
        base: String,
        /// Hive of the base key.
        hive: &'static str,
        /// How many keys and values change.
        counts: StepCounts,
        /// The writes.
        steps: Arc<Vec<LiveStep>>,
        /// The model the side holds once every write is done.
        target: Arc<RegFile>,
        /// True when the hive needs a second confirmation.
        protected: bool,
        /// True once the first confirmation is given.
        first_given: bool,
    },
    /// The export file changed on disk since it was read.
    DiskChanged(Side),
    /// Another version of the file was kept beside it during the save.
    KeptCopy(String),
    /// The tab is closing with edits that are not written.
    Closing,
}

/// What a write job posts back.
#[derive(Debug)]
pub enum EditMessage {
    /// An export file side was saved, or not.
    Saved {
        /// Side written.
        side: Side,
        /// How the save ended.
        outcome: SaveOutcome,
        /// The model that was written.
        model: Arc<RegFile>,
    },
    /// The writes a live side needs are known.
    Steps {
        /// Side the writes go to.
        side: Side,
        /// The writes.
        steps: Arc<Vec<LiveStep>>,
        /// The model the side holds once every write is done.
        target: Arc<RegFile>,
    },
    /// Every write reached the live key.
    Applied {
        /// Side written.
        side: Side,
        /// Restore file written before the first write.
        backup: PathBuf,
        /// The model the side now holds.
        target: Arc<RegFile>,
        /// Number of writes.
        count: usize,
    },
    /// The live writes stopped, or never started.
    ApplyFailed {
        /// Side written.
        side: Side,
        /// Why.
        reason: String,
        /// Restore file, when one was written.
        backup: Option<PathBuf>,
        /// The key as read after the failure, when any write ran.
        now: Option<Arc<RegFile>>,
    },
    /// An export was written, or not.
    Exported {
        /// Where.
        path: PathBuf,
        /// How the write ended.
        outcome: SaveOutcome,
    },
    /// The job ended with no outcome.
    Failed(String),
}

impl Terminal for EditMessage {
    fn is_terminal(&self) -> bool {
        true
    }

    fn cancelled() -> Self {
        Self::Failed("The write stopped before it finished.".to_owned())
    }

    fn panicked(detail: String) -> Self {
        Self::Failed(detail)
    }
}

/// What an export writes.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ExportTarget {
    /// One key and everything under it.
    Key(Side, String),
    /// Every key of one side.
    All(Side),
}

/// The editing state of a registry view.
pub struct EditState {
    pub(super) active: Side,
    pub(super) selection: Vec<usize>,
    pub(super) origin: Option<Box<(SideData, SideData)>>,
    pub(super) history: History,
    pub(super) written: [Option<Arc<RegFile>>; 2],
    pub(super) stamps: [Option<Stamp>; 2],
    pub(super) panel: Option<Panel>,
    pub(super) panel_error: Option<String>,
    pub(super) prompt: Option<Prompt>,
    pub(super) message: Option<String>,
    pub(super) work: Option<Job<EditMessage>>,
    pub(super) pending_saves: Vec<Side>,
    pub(super) journal_directory: PathBuf,
    pub(super) close_requested: bool,
    pub(super) discard: bool,
    export_picker: Option<Job<ca_ui::dialog::DialogMessage>>,
    export_target: Option<ExportTarget>,
}

impl EditState {
    pub(super) fn new() -> Self {
        Self {
            active: Side::Left,
            selection: Vec::new(),
            origin: None,
            history: History::new(),
            written: [None, None],
            stamps: [None, None],
            panel: None,
            panel_error: None,
            prompt: None,
            message: None,
            work: None,
            pending_saves: Vec::new(),
            journal_directory: ca_ui::paths::journal_directory(),
            close_requested: false,
            discard: false,
            export_picker: None,
            export_target: None,
        }
    }

    /// Forget every edit, for a comparison read again from its sources.
    pub(super) fn reset(&mut self, left: &SideData, right: &SideData) {
        self.origin = Some(Box::new((left.clone(), right.clone())));
        self.history = History::new();
        self.written = [left.model.clone(), right.model.clone()];
        self.stamps = [left.facts.stamp, right.facts.stamp];
        self.selection.clear();
        self.panel = None;
        self.panel_error = None;
    }
}

const fn index(side: Side) -> usize {
    match side {
        Side::Left => 0,
        Side::Right => 1,
    }
}

const fn other(side: Side) -> Side {
    match side {
        Side::Left => Side::Right,
        Side::Right => Side::Left,
    }
}

/// The sides an Up One Level command moves.
const fn up_sides(command: Command) -> &'static [Side] {
    match command {
        Command::UpOneLevelLeft => &[Side::Left],
        Command::UpOneLevelRight => &[Side::Right],
        Command::UpOneLevelBoth => &[Side::Left, Side::Right],
        _ => &[],
    }
}

const fn label(side: Side) -> &'static str {
    match side {
        Side::Left => "Left",
        Side::Right => "Right",
    }
}

/// The part of `path` a side whose base key is `base` may change: the path
/// itself when it lies inside the base key, the base key when the path is a
/// key above it, nothing otherwise.
fn clip_to_base(path: &str, base: Option<&str>) -> Option<String> {
    match base {
        None => Some(path.to_owned()),
        Some(base) if is_within(path, base) => Some(path.to_owned()),
        Some(base) if is_within(base, path) => Some(base.to_owned()),
        Some(_) => None,
    }
}

fn is_marker(node: &Node, side: Side) -> bool {
    node.record(side)
        .is_some_and(|record| record.type_name == KEY_DELETE_MARKER)
}

/// A name `base` followed by the lowest number no sibling holds.
fn fresh_name(taken: &[String], base: &str) -> String {
    (1..=taken.len().saturating_add(1))
        .map(|number| format!("{base} #{number}"))
        .find(|name| !taken.iter().any(|held| held.eq_ignore_ascii_case(name)))
        .unwrap_or_else(|| base.to_owned())
}

impl RecordsView {
    /// True when this view edits: a registry comparison.
    #[must_use]
    pub fn is_editable_kind(&self) -> bool {
        self.flavor == crate::flavor::Flavor::Registry
    }

    /// The pane edits and copies act on.
    #[must_use]
    pub const fn active_side(&self) -> Side {
        self.edit.active
    }

    /// Make `side` the pane edits and copies act on.
    pub fn set_active_side(&mut self, side: Side) {
        self.edit.active = side;
    }

    /// The undo history of the edits.
    #[must_use]
    pub const fn history(&self) -> &History {
        &self.edit.history
    }

    /// True when `side` holds edits its source has not received.
    #[must_use]
    pub fn is_modified(&self, side: Side) -> bool {
        self.is_editable_kind() && self.edit.history.is_modified(side)
    }

    /// The editor open in the view.
    #[must_use]
    pub const fn panel(&self) -> Option<&Panel> {
        self.edit.panel.as_ref()
    }

    /// The editor open in the view, to change what it holds.
    pub const fn panel_mut(&mut self) -> Option<&mut Panel> {
        self.edit.panel.as_mut()
    }

    /// Why the last commit of the editor was refused.
    #[must_use]
    pub fn panel_error(&self) -> Option<&str> {
        self.edit.panel_error.as_deref()
    }

    /// The question the view waits on.
    #[must_use]
    pub const fn prompt(&self) -> Option<&Prompt> {
        self.edit.prompt.as_ref()
    }

    /// The last thing an edit or a write reported.
    #[must_use]
    pub fn message(&self) -> Option<&str> {
        self.edit.message.as_deref()
    }

    /// Text waiting to reach the clipboard on the next frame.
    #[must_use]
    pub fn pending_clipboard(&self) -> Option<&str> {
        self.clipboard.as_deref()
    }

    /// Arena positions of the rows Select All chose.
    #[must_use]
    pub fn selection(&self) -> &[usize] {
        &self.edit.selection
    }

    /// True while a save, a live write or an export runs.
    #[must_use]
    pub fn is_writing(&self) -> bool {
        self.edit.work.is_some()
    }

    /// Where the restore files of live writes go.
    pub fn set_journal_directory(&mut self, directory: PathBuf) {
        self.edit.journal_directory = directory;
    }

    fn side_data(&self, side: Side) -> Option<&SideData> {
        self.sides.as_ref().map(|sides| match side {
            Side::Left => &sides.0,
            Side::Right => &sides.1,
        })
    }

    /// The base key of a live side.
    fn live_base(&self, side: Side) -> Option<String> {
        self.side_data(side)
            .and_then(|data| data.live.as_ref())
            .map(ca_records::registry::RegistrySpec::key_path)
    }

    fn is_live(&self, side: Side) -> bool {
        self.side_data(side).is_some_and(|data| data.live.is_some())
    }

    fn model(&self, side: Side) -> Option<&Arc<RegFile>> {
        self.side_data(side).and_then(|data| data.model.as_ref())
    }

    /// True while the editing switch of the session is on: no edit starts
    /// and nothing is written.
    fn editing_off(&self) -> bool {
        self.clipboard_input.iter().any(Option::is_some)
            || self
                .settings
                .specs()
                .is_some_and(|specs| specs.disable_editing)
    }

    /// True when a new edit can start now.
    fn can_edit(&self) -> bool {
        self.is_editable_kind()
            && !self.editing_off()
            && self.has_comparison()
            && self.edit.work.is_none()
            && self.edit.prompt.is_none()
            && self.edit.origin.is_some()
    }

    fn cursor_node(&self) -> Option<&Node> {
        self.listing.node_at(self.listing.cursor())
    }

    /// Arena positions an edit acts on: the selection, or the cursor row.
    fn chosen(&self) -> Vec<usize> {
        if self.edit.selection.is_empty() {
            self.listing
                .index_at(self.listing.cursor())
                .into_iter()
                .collect()
        } else {
            self.edit.selection.clone()
        }
    }

    /// The chosen rows present on `side`, without a row whose key is chosen
    /// too.
    fn topmost_on(&self, side: Side) -> Vec<&Node> {
        let chosen = self.chosen();
        let set: std::collections::HashSet<usize> = chosen.iter().copied().collect();
        let nodes = self.listing.nodes();
        chosen
            .iter()
            .filter_map(|index| nodes.get(*index))
            .filter(|node| {
                let mut parent = node.parent;
                while let Some(held) = parent {
                    if set.contains(&held) {
                        return false;
                    }
                    parent = nodes.get(held).and_then(|key| key.parent);
                }
                true
            })
            .filter(|node| node.is_on(side) && !is_marker(node, side))
            .collect()
    }

    fn copy_ops(&self, from: Side) -> Vec<EditOp> {
        #[cfg(test)]
        COPY_OPS_CALLS.with(|calls| calls.set(calls.get() + 1));
        let target_base = self.live_base(other(from));
        let source_base = self.live_base(from);
        let mut ops = Vec::new();
        for node in self.topmost_on(from) {
            if node.is_group {
                let Some(path) = clip_to_base(&node.path, source_base.as_deref())
                    .and_then(|path| clip_to_base(&path, target_base.as_deref()))
                else {
                    continue;
                };
                ops.push(EditOp::copy_key(plan_side(from), path));
            } else if target_base
                .as_deref()
                .is_none_or(|base| is_within(&node.path, base))
            {
                ops.push(EditOp::copy_value(
                    plan_side(from),
                    node.path.clone(),
                    from_display(&node.name),
                ));
            }
        }
        ops
    }

    fn delete_ops(&self, side: Side) -> Vec<EditOp> {
        let base = self.live_base(side);
        let mut ops = Vec::new();
        for node in self.topmost_on(side) {
            if node.is_group {
                let Some(path) = clip_to_base(&node.path, base.as_deref()) else {
                    continue;
                };
                ops.push(EditOp::delete_key(plan_side(side), path));
            } else if base
                .as_deref()
                .is_none_or(|base| is_within(&node.path, base))
            {
                ops.push(EditOp::delete_value(
                    plan_side(side),
                    node.path.clone(),
                    from_display(&node.name),
                ));
            }
        }
        ops
    }

    fn has_edit_targets(&self) -> bool {
        !self.edit.selection.is_empty() || self.cursor_node().is_some()
    }

    /// The key the cursor names on the active side, with the value when the
    /// cursor sits on one, where the side may change it.
    fn cursor_target(&self) -> Option<(String, Option<ValueName>)> {
        let side = self.edit.active;
        let node = self.cursor_node()?;
        if !node.is_on(side) || is_marker(node, side) {
            return None;
        }
        let allowed = self
            .live_base(side)
            .is_none_or(|base| is_within(&node.path, &base));
        if !allowed {
            return None;
        }
        let value = (!node.is_group).then(|| from_display(&node.name));
        Some((node.path.clone(), value))
    }

    fn can_rename(&self) -> bool {
        if self.edit.selection.len() > 1 {
            return false;
        }
        let Some((path, value)) = self.cursor_target() else {
            return false;
        };
        match value {
            Some(name) => name != ValueName::Default,
            None => {
                path.contains('\\')
                    && self
                        .live_base(self.edit.active)
                        .is_none_or(|base| !path.eq_ignore_ascii_case(&base))
            }
        }
    }

    fn base_key_row(&self) -> Option<&Node> {
        self.cursor_node().filter(|node| node.is_group)
    }

    fn can_set_base(&self, command: Command) -> bool {
        if !self.can_edit() || self.edit.history.is_any_modified() {
            return false;
        }
        let Some(node) = self.base_key_row() else {
            return false;
        };
        let live_on = |side: Side| self.is_live(side) && node.is_on(side);
        match command {
            Command::SetAsBaseKeys => live_on(Side::Left) || live_on(Side::Right),
            Command::SetBothAsBaseKeys => live_on(Side::Left) && live_on(Side::Right),
            Command::SetAsBaseKeyOnOtherSide => {
                node.is_on(self.edit.active) && self.is_live(other(self.edit.active))
            }
            _ => false,
        }
    }

    /// True when the view can run an editing command now.
    pub(super) fn accepts_edit(&self, command: Command) -> bool {
        if !self.is_editable_kind() {
            return false;
        }
        let rows = self.listing.rows() > 0;
        let active = self.edit.active;
        match command {
            Command::Undo => self.can_edit() && self.edit.history.can_undo(),
            Command::Redo => self.can_edit() && self.edit.history.can_redo(),
            Command::CopyToRight
            | Command::CopyToLeft
            | Command::CopyToOtherSide
            | Command::Delete => self.can_edit() && self.has_edit_targets(),
            Command::Rename => self.can_edit() && self.can_rename(),
            Command::NewKey | Command::NewValue => {
                self.can_edit() && self.edit.selection.len() <= 1 && self.cursor_target().is_some()
            }
            Command::Modify => {
                self.can_edit()
                    && self.edit.selection.len() <= 1
                    && self
                        .cursor_target()
                        .is_some_and(|(_, value)| value.is_some())
            }
            Command::CopyKeyName => self.has_comparison() && rows,
            Command::Export => {
                self.has_comparison()
                    && self.edit.work.is_none()
                    && self.cursor_node().is_some_and(|node| node.is_on(active))
            }
            Command::ExportAll => {
                self.has_comparison() && self.edit.work.is_none() && self.model(active).is_some()
            }
            Command::SetAsBaseKeys
            | Command::SetBothAsBaseKeys
            | Command::SetAsBaseKeyOnOtherSide => self.can_set_base(command),
            Command::UpOneLevelLeft | Command::UpOneLevelRight | Command::UpOneLevelBoth => {
                self.can_edit()
                    && !self.edit.history.is_any_modified()
                    && up_sides(command)
                        .iter()
                        .all(|side| self.parent_base(*side).is_some())
            }
            Command::SaveFile => self.can_save() && self.edit.history.is_modified(active),
            Command::SaveBoth => self.can_save() && self.edit.history.is_any_modified(),
            _ => false,
        }
    }

    /// Why a copy line of a registry comparison is refused now.
    ///
    /// A view of another kind takes no edit, which the line of the bar says.
    pub(super) fn copy_refusal(&self, command: Command) -> Option<&'static str> {
        let copy = matches!(
            command,
            Command::CopyToRight | Command::CopyToLeft | Command::CopyToOtherSide
        );
        if !copy || !self.is_editable_kind() || self.accepts_edit(command) {
            return None;
        }
        Some(match &self.progress {
            super::Progress::Running(_) => NOT_COMPARED,
            super::Progress::Failed(_) => FAILED,
            super::Progress::Cancelled => STOPPED,
            super::Progress::Ready if self.clipboard_input.iter().any(Option::is_some) => {
                CLIPBOARD_SIDE
            }
            super::Progress::Ready if self.editing_off() => EDITING_OFF,
            super::Progress::Ready if self.edit.work.is_some() => WRITING,
            super::Progress::Ready if self.edit.prompt.is_some() => QUESTION_OPEN,
            super::Progress::Ready if self.edit.origin.is_none() => NOT_COMPARED,
            super::Progress::Ready => NOTHING_CHOSEN,
        })
    }

    fn can_save(&self) -> bool {
        self.is_editable_kind()
            && !self.editing_off()
            && self.has_comparison()
            && self.edit.work.is_none()
            && self.edit.prompt.is_none()
    }

    /// Run an editing command. Returns false for a command this part does not
    /// run.
    pub(super) fn run_edit(&mut self, command: Command) -> bool {
        if self.editing_off() && CHANGES_A_SIDE.contains(&command) {
            self.edit.message = Some(EDITING_OFF.to_owned());
            return true;
        }
        match command {
            Command::Undo => {
                if self.edit.history.undo() {
                    self.rebuild();
                }
            }
            Command::Redo => {
                if self.edit.history.redo() {
                    self.rebuild();
                }
            }
            Command::CopyToRight => self.push_step(self.copy_ops(Side::Left), "Nothing to copy"),
            Command::CopyToLeft => self.push_step(self.copy_ops(Side::Right), "Nothing to copy"),
            Command::CopyToOtherSide => {
                self.push_step(self.copy_ops(self.edit.active), "Nothing to copy");
            }
            Command::Delete => {
                self.push_step(self.delete_ops(self.edit.active), "Nothing to delete");
            }
            Command::Rename => self.open_rename(),
            Command::NewKey => self.open_new_key(),
            Command::NewValue => self.open_new_value(),
            Command::Modify => self.open_modify(),
            Command::CopyKeyName => {
                self.cancel_selection_copy();
                self.clipboard = self.cursor_node().map(|node| node.path.clone());
            }
            Command::Export => self.request_export(false),
            Command::ExportAll => self.request_export(true),
            Command::SetAsBaseKeys
            | Command::SetBothAsBaseKeys
            | Command::SetAsBaseKeyOnOtherSide => self.set_base_keys(command),
            Command::UpOneLevelLeft | Command::UpOneLevelRight | Command::UpOneLevelBoth => {
                self.up_one_level(command);
            }
            Command::SaveFile => {
                self.edit.pending_saves = vec![self.edit.active];
                self.save_next();
            }
            Command::SaveBoth => {
                self.edit.pending_saves = [Side::Left, Side::Right]
                    .into_iter()
                    .filter(|side| self.edit.history.is_modified(*side))
                    .collect();
                self.save_next();
            }
            _ => return false,
        }
        true
    }

    fn push_step(&mut self, ops: Vec<EditOp>, empty: &str) {
        if self.editing_off() {
            self.edit.message = Some(EDITING_OFF.to_owned());
            return;
        }
        if ops.is_empty() {
            self.edit.message = Some(empty.to_owned());
            return;
        }
        self.edit.message = None;
        self.edit.history.push(ops);
        self.edit.selection.clear();
        self.rebuild();
    }

    /// Show the models as read with the plan in force applied.
    pub(super) fn rebuild(&mut self) {
        let Some(origin) = self.edit.origin.as_ref() else {
            return;
        };
        self.progress = super::Progress::Running(jobs::Stage::Comparing);
        let job = jobs::spawn_rebuild(
            origin.0.clone(),
            origin.1.clone(),
            self.edit.history.ops(),
            self.rules.clone(),
            Arc::clone(&self.notify),
        );
        self.pipeline.start(job);
    }

    /// A rebuild could not apply the last step: drop it and say why.
    pub(super) fn refused(&mut self, reason: &str) {
        self.edit.history.discard_last();
        self.edit.message = Some(format!("The edit was not made: {reason}"));
        self.progress = super::Progress::Ready;
    }

    pub(super) fn select_all(&mut self) {
        let side = self.edit.active;
        let mut chosen: Vec<usize> = (0..self.listing.rows())
            .filter_map(|row| {
                let index = self.listing.index_at(row)?;
                self.listing
                    .nodes()
                    .get(index)
                    .filter(|node| node.is_on(side))
                    .map(|_| index)
            })
            .collect();
        chosen.sort_unstable();
        self.edit.selection = chosen;
    }

    /// Drop the selection, as any move of the cursor does.
    pub(super) fn clear_selection(&mut self) {
        self.edit.selection.clear();
    }

    /// True when the arena row `index` is selected.
    pub(super) fn is_selected(&self, index: usize) -> bool {
        self.edit.selection.binary_search(&index).is_ok()
    }

    fn children_names(&self, key_path: &str, groups: bool) -> Vec<String> {
        let side = self.edit.active;
        self.listing
            .nodes()
            .iter()
            .filter(|node| node.is_on(side) && node.is_group == groups)
            .filter(|node| {
                if groups {
                    node.path
                        .rsplit_once('\\')
                        .is_some_and(|(parent, _)| parent.eq_ignore_ascii_case(key_path))
                } else {
                    node.path.eq_ignore_ascii_case(key_path)
                }
            })
            .map(|node| node.name.clone())
            .collect()
    }

    fn open_new_key(&mut self) {
        let Some((path, _)) = self.cursor_target() else {
            return;
        };
        let name = fresh_name(&self.children_names(&path, true), "New Key");
        self.edit.panel_error = None;
        self.edit.panel = Some(Panel::NewKey {
            side: self.edit.active,
            parent: path,
            name,
        });
    }

    fn open_new_value(&mut self) {
        let Some((path, _)) = self.cursor_target() else {
            return;
        };
        let name = fresh_name(&self.children_names(&path, false), "New Value");
        self.edit.panel_error = None;
        self.edit.panel = Some(Panel::Value {
            side: self.edit.active,
            key_path: path,
            form: ValueForm::new_value(name),
            is_new: true,
        });
    }

    /// Open the value editor on the cursor value of the active side.
    pub fn open_modify(&mut self) {
        let side = self.edit.active;
        let Some((path, Some(name))) = self.cursor_target() else {
            return;
        };
        let data = self
            .model(side)
            .and_then(|model| preview_value(model, &path, &name))
            .unwrap_or_else(|| ca_records::registry::ValueData::Sz(String::new()));
        self.edit.panel_error = None;
        self.edit.panel = Some(Panel::Value {
            side,
            key_path: path,
            form: ValueForm::existing(&name, &data),
            is_new: false,
        });
    }

    fn open_rename(&mut self) {
        let Some((path, value)) = self.cursor_target() else {
            return;
        };
        let name = match &value {
            Some(value) => value.display().to_owned(),
            None => path.rsplit('\\').next().unwrap_or_default().to_owned(),
        };
        self.edit.panel_error = None;
        self.edit.panel = Some(Panel::Rename {
            side: self.edit.active,
            key_path: path,
            value,
            name,
        });
    }

    /// Close the editor without an edit.
    pub fn cancel_panel(&mut self) {
        self.edit.panel = None;
        self.edit.panel_error = None;
    }

    /// Turn what the editor holds into an edit. Returns false, with the
    /// reason in [`RecordsView::panel_error`], when the editor holds
    /// something that cannot be written.
    pub fn commit_panel(&mut self) -> bool {
        let Some(panel) = self.edit.panel.clone() else {
            return false;
        };
        match self.panel_ops(&panel) {
            Ok(ops) => {
                self.edit.panel = None;
                self.edit.panel_error = None;
                self.push_step(ops, "Nothing changed");
                true
            }
            Err(reason) => {
                self.edit.panel_error = Some(reason);
                false
            }
        }
    }

    fn panel_ops(&self, panel: &Panel) -> Result<Vec<EditOp>, String> {
        if !self.can_edit() {
            return Err("Wait for the comparison and the running write to finish.".to_owned());
        }
        match panel {
            Panel::Value {
                side,
                key_path,
                form,
                is_new,
            } => {
                let name = form.value_name()?;
                let data = form.data()?;
                let taken = self
                    .model(*side)
                    .and_then(|model| preview_value(model, key_path, &name))
                    .is_some();
                if *is_new && taken {
                    return Err(format!(
                        "A value named {} is already there.",
                        name.display()
                    ));
                }
                Ok(vec![EditOp::set_value(
                    plan_side(*side),
                    key_path.clone(),
                    name,
                    &data,
                )])
            }
            Panel::NewKey { side, parent, name } => {
                check_key_name(name)?;
                if self
                    .children_names(parent, true)
                    .iter()
                    .any(|held| held.eq_ignore_ascii_case(name))
                {
                    return Err(format!("A key named {name} is already there."));
                }
                Ok(vec![EditOp::create_key(
                    plan_side(*side),
                    format!("{parent}\\{name}"),
                )])
            }
            Panel::Rename {
                side,
                key_path,
                value,
                name,
            } => {
                let old = value.as_ref().map_or_else(
                    || key_path.rsplit('\\').next().unwrap_or_default(),
                    ValueName::raw,
                );
                if name.is_empty() {
                    return Err("Type a new name.".to_owned());
                }
                if old.eq_ignore_ascii_case(name) {
                    return Err(CASE_ONLY.to_owned());
                }
                let op = if let Some(value) = value {
                    EditOp::rename_value(
                        plan_side(*side),
                        key_path.clone(),
                        value.clone(),
                        ValueName::from_raw(name),
                    )
                } else {
                    check_key_name(name)?;
                    EditOp::rename_key(plan_side(*side), key_path.clone(), name.clone())
                };
                Ok(vec![op])
            }
        }
    }

    /// The parent of a live side's base key. A hive root has none.
    fn parent_base(&self, side: Side) -> Option<String> {
        if !self.is_live(side) {
            return None;
        }
        let base = self.live_base(side)?;
        let (parent, _) = base.trim_end_matches('\\').rsplit_once('\\')?;
        (!parent.is_empty()).then(|| parent.to_owned())
    }

    fn up_one_level(&mut self, command: Command) {
        let sides = up_sides(command);
        let parents: Vec<(Side, String)> = sides
            .iter()
            .filter_map(|side| Some((*side, self.parent_base(*side)?)))
            .collect();
        if parents.len() != sides.len() || self.edit.history.is_any_modified() {
            return;
        }
        for (side, parent) in parents {
            let spec = format!("reg:\\\\{parent}");
            match side {
                Side::Left => {
                    self.left_field.clone_from(&spec);
                    self.left_path = PathBuf::from(spec);
                }
                Side::Right => {
                    self.right_field.clone_from(&spec);
                    self.right_path = PathBuf::from(spec);
                }
            }
        }
        self.restart();
    }

    fn set_base_keys(&mut self, command: Command) {
        if !self.can_set_base(command) {
            return;
        }
        let Some(node) = self.base_key_row() else {
            return;
        };
        let path = node.path.clone();
        let on = |side: Side| node.is_on(side);
        let sides: Vec<Side> = match command {
            Command::SetAsBaseKeys => [Side::Left, Side::Right]
                .into_iter()
                .filter(|side| self.is_live(*side) && on(*side))
                .collect(),
            Command::SetBothAsBaseKeys => vec![Side::Left, Side::Right],
            _ => vec![other(self.edit.active)],
        };
        for side in sides {
            let spec = format!("reg:\\\\{path}");
            match side {
                Side::Left => {
                    self.left_field.clone_from(&spec);
                    self.left_path = PathBuf::from(spec);
                }
                Side::Right => {
                    self.right_field.clone_from(&spec);
                    self.right_path = PathBuf::from(spec);
                }
            }
        }
        self.restart();
    }

    fn save_next(&mut self) {
        if self.edit.work.is_some() || self.edit.prompt.is_some() {
            return;
        }
        if self.editing_off() {
            self.edit.pending_saves.clear();
            self.edit.message = Some(EDITING_OFF.to_owned());
            return;
        }
        let Some(side) = self.edit.pending_saves.first().copied() else {
            return;
        };
        self.edit.pending_saves.remove(0);
        self.save_side(side, false);
    }

    /// Start writing `side` to its source.
    fn save_side(&mut self, side: Side, accept_disk_change: bool) {
        let Some(model) = self.model(side).cloned() else {
            return;
        };
        let notify = Arc::clone(&self.notify);
        if self.is_live(side) {
            if !live::is_available() {
                self.edit.message = Some(LIVE_UNAVAILABLE.to_owned());
                self.edit.pending_saves.clear();
                return;
            }
            let before = self.edit.written[index(side)]
                .clone()
                .unwrap_or_else(|| Arc::new(RegFile::empty(model.version)));
            self.edit.work = Some(Job::spawn_notifying(
                move |emitter, _| {
                    let steps = live_steps(&before, &model);
                    emitter.send(EditMessage::Steps {
                        side,
                        steps: Arc::new(steps),
                        target: model,
                    });
                },
                notify,
            ));
            return;
        }
        let path = match side {
            Side::Left => self.left_path.clone(),
            Side::Right => self.right_path.clone(),
        };
        let expected = Baseline::from(self.edit.stamps[index(side)]);
        self.edit.work = Some(Job::spawn_notifying(
            move |emitter, _| {
                let outcome = write_file(&path, &model, expected, accept_disk_change);
                emitter.send(EditMessage::Saved {
                    side,
                    outcome,
                    model,
                });
            },
            notify,
        ));
    }

    /// Give the confirmation the live prompt asks for.
    ///
    /// A hive the system depends on asks twice; the writes start after the
    /// second answer.
    pub fn confirm_live(&mut self) {
        let Some(Prompt::ConfirmLive {
            side,
            steps,
            target,
            protected,
            first_given,
            ..
        }) = self.edit.prompt.clone()
        else {
            return;
        };
        if protected && !first_given {
            if let Some(Prompt::ConfirmLive { first_given, .. }) = self.edit.prompt.as_mut() {
                *first_given = true;
            }
            return;
        }
        self.edit.prompt = None;
        let Some(spec) = self.side_data(side).and_then(|data| data.live.clone()) else {
            return;
        };
        let consent = WriteConsent {
            confirmed: true,
            protected_hive_confirmed: protected,
        };
        let journal = self.edit.journal_directory.clone();
        self.edit.work = Some(Job::spawn_notifying(
            move |emitter, _| {
                emitter.send(apply_live(side, &spec, &steps, consent, &journal, target));
            },
            Arc::clone(&self.notify),
        ));
    }

    /// Answer no to the question the view waits on.
    pub fn cancel_prompt(&mut self) {
        if matches!(self.edit.prompt, Some(Prompt::Closing)) {
            self.edit.close_requested = false;
        }
        self.edit.prompt = None;
        self.edit.pending_saves.clear();
    }

    /// Overwrite a file that changed on disk since it was read.
    pub fn overwrite_changed(&mut self) {
        if let Some(Prompt::DiskChanged(side)) = self.edit.prompt.clone() {
            self.edit.prompt = None;
            self.save_side(side, true);
        }
    }

    /// Take whatever the write job has posted.
    pub(super) fn poll_edit(&mut self) {
        self.poll_export_picker();
        let Some(job) = self.edit.work.as_mut() else {
            return;
        };
        let messages = job.drain();
        if job.is_finished() || !messages.is_empty() {
            self.edit.work = None;
        }
        for message in messages {
            self.receive(message);
        }
    }

    fn receive(&mut self, message: EditMessage) {
        match message {
            EditMessage::Saved {
                side,
                outcome,
                model,
            } => self.saved(side, outcome, model),
            EditMessage::Steps {
                side,
                steps,
                target,
            } => {
                if steps.is_empty() {
                    self.edit.written[index(side)] = Some(target);
                    self.edit.history.mark_saved(side);
                    self.edit.message = Some(format!("{}: nothing to write.", label(side)));
                    self.save_next();
                    return;
                }
                let Some(spec) = self.side_data(side).and_then(|data| data.live.clone()) else {
                    return;
                };
                self.edit.prompt = Some(Prompt::ConfirmLive {
                    side,
                    base: spec.key_path(),
                    hive: spec.hive.full_name(),
                    counts: StepCounts::of(&steps),
                    steps,
                    target,
                    protected: live::is_protected_hive(spec.hive),
                    first_given: false,
                });
            }
            EditMessage::Applied {
                side,
                backup,
                target,
                count,
            } => {
                self.edit.written[index(side)] = Some(target);
                self.edit.history.mark_saved(side);
                self.edit.message = Some(format!(
                    "{}: {count} writes done. Restore file: {}",
                    label(side),
                    backup.display()
                ));
                self.save_next();
            }
            EditMessage::ApplyFailed {
                side,
                reason,
                backup,
                now,
            } => {
                if let Some(now) = now {
                    self.edit.written[index(side)] = Some(now);
                }
                self.edit.pending_saves.clear();
                self.edit.close_requested = false;
                self.edit.message = Some(match backup {
                    Some(path) => format!(
                        "{}: the live write stopped: {reason}. Restore file: {}",
                        label(side),
                        path.display()
                    ),
                    None => format!("{}: nothing was written: {reason}", label(side)),
                });
            }
            EditMessage::Exported { path, outcome } => {
                self.edit.message = Some(match outcome {
                    SaveOutcome::Saved(_) | SaveOutcome::SavedWithConflict { .. } => {
                        format!("Exported to {}", path.display())
                    }
                    other => format!("The export was not written: {}", outcome_text(&other)),
                });
            }
            EditMessage::Failed(reason) => {
                self.edit.pending_saves.clear();
                self.edit.message = Some(reason);
            }
        }
    }

    fn saved(&mut self, side: Side, outcome: SaveOutcome, model: Arc<RegFile>) {
        let kept = match &outcome {
            SaveOutcome::SavedWithConflict { backup, .. } => Some(backup.display().to_string()),
            _ => None,
        };
        match outcome {
            SaveOutcome::Saved(stamp) | SaveOutcome::SavedWithConflict { stamp, .. } => {
                self.edit.stamps[index(side)] = Some(stamp);
                self.edit.written[index(side)] = Some(model);
                self.edit.history.mark_saved(side);
                if let Some(backup) = kept {
                    self.edit.message = Some(format!(
                        "Saved, but another version was kept at {backup}. Review it."
                    ));
                    self.edit.prompt = Some(Prompt::KeptCopy(backup));
                    self.edit.pending_saves.clear();
                    self.edit.close_requested = false;
                } else {
                    self.edit.message = Some(format!("{} written.", label(side)));
                    self.save_next();
                }
            }
            SaveOutcome::ChangedOnDisk => self.edit.prompt = Some(Prompt::DiskChanged(side)),
            other => {
                self.edit.pending_saves.clear();
                self.edit.close_requested = false;
                self.edit.message = Some(format!(
                    "{}: save failed: {}",
                    label(side),
                    outcome_text(&other)
                ));
            }
        }
    }

    fn request_export(&mut self, all: bool) {
        let side = self.edit.active;
        let target = if all {
            ExportTarget::All(side)
        } else {
            let Some(node) = self.cursor_node() else {
                return;
            };
            ExportTarget::Key(side, node.path.clone())
        };
        self.edit.export_target = Some(target);
        self.edit.export_picker = Some(ca_ui::dialog::spawn(
            ca_ui::dialog::Pick::SaveFile,
            Arc::clone(&self.notify),
        ));
    }

    fn poll_export_picker(&mut self) {
        let Some(job) = self.edit.export_picker.as_mut() else {
            return;
        };
        let messages = job.drain();
        let finished = job.is_finished();
        for message in messages {
            if let ca_ui::dialog::DialogMessage::Chosen(path) = message {
                if let Some(target) = self.edit.export_target.clone() {
                    self.start_export(&path, target);
                }
            }
        }
        if finished {
            self.edit.export_picker = None;
        }
    }

    /// Write the cursor key of the active side, or the whole side, to `path`.
    pub fn export_to(&mut self, path: &Path, all: bool) {
        let side = self.edit.active;
        let target = if all {
            ExportTarget::All(side)
        } else {
            let Some(node) = self.cursor_node() else {
                return;
            };
            ExportTarget::Key(side, node.path.clone())
        };
        self.start_export(path, target);
    }

    fn start_export(&mut self, path: &Path, target: ExportTarget) {
        if self.edit.work.is_some() {
            self.edit.message = Some("A write is still running. Export again after it.".to_owned());
            return;
        }
        let (side, key) = match target {
            ExportTarget::All(side) => (side, None),
            ExportTarget::Key(side, key) => (side, Some(key)),
        };
        let Some(model) = self.model(side).cloned() else {
            return;
        };
        let path = path.to_path_buf();
        self.edit.work = Some(Job::spawn_notifying(
            move |emitter, _| {
                let file = match &key {
                    Some(key) => subtree(&model, key),
                    None => RegFile::clone(&model),
                };
                // The save picker asked about an existing name already.
                let outcome = write_file(&path, &file, Baseline::Unchecked, true);
                emitter.send(EditMessage::Exported { path, outcome });
            },
            Arc::clone(&self.notify),
        ));
    }

    /// True when the tab may close now.
    pub(super) fn may_close_edit(&mut self) -> bool {
        if !self.is_editable_kind() {
            return true;
        }
        if self.edit.work.is_some() || matches!(self.edit.prompt, Some(Prompt::KeptCopy(_))) {
            return false;
        }
        if self.edit.discard || !self.edit.history.is_any_modified() {
            return true;
        }
        self.edit.prompt = Some(Prompt::Closing);
        false
    }

    /// True once a close was asked for and nothing is left to write.
    pub(super) fn wants_close_edit(&self) -> bool {
        self.edit.close_requested
            && self.edit.work.is_none()
            && (self.edit.discard || !self.edit.history.is_any_modified())
    }

    /// Close without writing the edits.
    pub fn discard_and_close(&mut self) {
        self.edit.prompt = None;
        self.edit.discard = true;
        self.edit.close_requested = true;
    }

    /// Write every modified export file, then close.
    pub fn save_and_close(&mut self) {
        self.edit.prompt = None;
        self.edit.close_requested = true;
        self.run_edit(Command::SaveBoth);
    }

    /// True when a side with unwritten edits is a live key, which a close
    /// does not write without its own confirmation.
    fn live_modified(&self) -> bool {
        [Side::Left, Side::Right]
            .into_iter()
            .any(|side| self.is_live(side) && self.edit.history.is_modified(side))
    }

    /// The editor strip under the toolbar.
    pub(super) fn panel_ui(&mut self, ui: &mut egui::Ui) {
        let Some(mut panel) = self.edit.panel.clone() else {
            return;
        };
        let mut commit = false;
        let mut cancel = false;
        ui.group(|ui| {
            match &mut panel {
                Panel::Value {
                    side,
                    key_path,
                    form,
                    is_new,
                } => {
                    ui.label(format!(
                        "{} value in {key_path} ({})",
                        if *is_new { "New" } else { "Modify" },
                        label(*side)
                    ));
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Name");
                        ui.add_enabled(
                            !form.name_fixed,
                            egui::TextEdit::singleline(&mut form.name).desired_width(200.0),
                        );
                        ui.label("Type");
                        let kinds = form.kinds();
                        egui::ComboBox::from_id_salt(self.id.with("value-type"))
                            .selected_text(form.kind.name())
                            .show_ui(ui, |ui| {
                                for kind in kinds {
                                    ui.selectable_value(&mut form.kind, kind, kind.name());
                                }
                            });
                    });
                    ui.label(data_hint(form.kind));
                    ui.add(
                        egui::TextEdit::multiline(&mut form.text)
                            .desired_rows(3)
                            .desired_width(f32::INFINITY),
                    );
                }
                Panel::NewKey { side, parent, name } => {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(format!("New key under {parent} ({})", label(*side)));
                        ui.add(egui::TextEdit::singleline(name).desired_width(200.0));
                    });
                }
                Panel::Rename {
                    side,
                    key_path,
                    value,
                    name,
                } => {
                    ui.horizontal_wrapped(|ui| {
                        let what = value.as_ref().map_or_else(
                            || key_path.clone(),
                            |value| format!("{key_path}\\{}", value.display()),
                        );
                        ui.label(format!("Rename {what} ({})", label(*side)));
                        ui.add(egui::TextEdit::singleline(name).desired_width(200.0));
                    });
                }
            }
            ui.horizontal_wrapped(|ui| {
                commit = ui.button("OK").clicked();
                cancel = ui.button("Cancel").clicked();
                if let Some(error) = &self.edit.panel_error {
                    ui.label(error);
                }
            });
        });
        self.edit.panel = Some(panel);
        if cancel {
            self.cancel_panel();
        } else if commit {
            self.commit_panel();
        }
    }

    /// The question strip, and the window for a kept copy.
    pub(super) fn prompt_ui(&mut self, ui: &mut egui::Ui) {
        let Some(prompt) = self.edit.prompt.clone() else {
            return;
        };
        if let Prompt::KeptCopy(path) = &prompt {
            egui::Window::new("Another version was kept")
                .id(self.id.with("kept-copy"))
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ui.ctx(), |ui| {
                    ca_ui::widgets::notice_current(ui,ca_ui::icons::Icon::Warning,24.0,"Another program changed the file during your save. The saved file contains your edit. Review the other version at:");
                    ui.monospace(path);
                    if ui.button("I have the path").clicked() {
                        self.edit.prompt = None;
                    }
                });
            return;
        }
        let live_modified = self.live_modified();
        ui.group(|ui| match &prompt {
            Prompt::ConfirmLive {
                base,
                hive,
                counts,
                protected,
                first_given,
                ..
            } => {
                if *protected && *first_given {
                    ui.label(format!(
                        "{base} is in {hive}, which the system depends on. Confirm again to write to it."
                    ));
                } else {
                    ui.label(format!(
                        "Write to the live registry under {base}: {} keys created, {} keys deleted with everything under them, {} values written, {} values deleted. A restore file of the touched keys is written to {} first.",
                        counts.keys_created,
                        counts.keys_deleted,
                        counts.values_set,
                        counts.values_deleted,
                        self.edit.journal_directory.display()
                    ));
                }
                ui.horizontal_wrapped(|ui| {
                    let caption = if *protected && *first_given {
                        format!("Write to {hive}")
                    } else {
                        "Write".to_owned()
                    };
                    if ui.button(caption).clicked() {
                        self.confirm_live();
                    }
                    if ui.button("Cancel").clicked() {
                        self.cancel_prompt();
                    }
                });
            }
            Prompt::DiskChanged(side) => {
                ui.horizontal_wrapped(|ui| {
                    ui.label(format!(
                        "{} changed on disk since it was read. Overwrite it?",
                        label(*side)
                    ));
                    if ui.button("Overwrite").clicked() {
                        self.overwrite_changed();
                    }
                    if ui.button("Cancel").clicked() {
                        self.cancel_prompt();
                    }
                });
            }
            Prompt::Closing => {
                ui.horizontal_wrapped(|ui| {
                    ui.label("This tab has edits that are not written.");
                    if live_modified {
                        ui.label("Write the live keys with Save first, or discard the edits.");
                    } else if ui.button("Save and close").clicked() {
                        self.save_and_close();
                    }
                    if ui.button("Discard and close").clicked() {
                        self.discard_and_close();
                    }
                    if ui.button("Cancel").clicked() {
                        self.cancel_prompt();
                    }
                });
            }
            Prompt::KeptCopy(_) => {}
        });
    }
}

/// What the data field takes for `kind`.
const fn data_hint(kind: ValueKind) -> &'static str {
    match kind {
        ValueKind::Sz | ValueKind::ExpandSz => "Data: text",
        ValueKind::MultiSz => "Data: one item on each line",
        ValueKind::Dword | ValueKind::DwordBigEndian | ValueKind::Qword => {
            "Data: a decimal number, or 0x and hexadecimal digits"
        }
        _ => "Data: hexadecimal bytes, such as 01 a0 ff",
    }
}

fn check_key_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("A key needs a name.".to_owned());
    }
    if name.contains('\\') {
        return Err("A key name cannot hold a backslash.".to_owned());
    }
    Ok(())
}

/// Write `model` to `path` through the checked save.
fn write_file(path: &Path, model: &RegFile, expected: Baseline, accept: bool) -> SaveOutcome {
    if let Some(outcome) = save::check_disk_change(&RealFileSystem, path, expected, accept) {
        return outcome;
    }
    match model.to_bytes() {
        Ok(bytes) => save::save(&RealFileSystem, path, &bytes, expected, accept),
        Err(error) => SaveOutcome::WouldLose(error.to_string()),
    }
}

fn outcome_text(outcome: &SaveOutcome) -> String {
    match outcome {
        SaveOutcome::Saved(_) | SaveOutcome::SavedWithConflict { .. } => "written".to_owned(),
        SaveOutcome::ChangedOnDisk => "the file changed on disk".to_owned(),
        SaveOutcome::NotWritable => "the file cannot be written".to_owned(),
        SaveOutcome::WouldLose(reason) | SaveOutcome::Failed(reason) => reason.clone(),
    }
}

/// Write the restore file, then the steps. Runs on a worker.
fn apply_live(
    side: Side,
    spec: &ca_records::registry::RegistrySpec,
    steps: &[LiveStep],
    consent: WriteConsent,
    journal: &Path,
    target: Arc<RegFile>,
) -> EditMessage {
    let refused = |reason: String| EditMessage::ApplyFailed {
        side,
        reason,
        backup: None,
        now: None,
    };
    if let Err(error) = live::check_write(spec, steps, consent) {
        return refused(error.to_string());
    }
    let backup = match live::backup(spec, steps, &LiveOptions::default()) {
        Ok(file) => file,
        Err(error) => return refused(format!("the restore file could not be read: {error}")),
    };
    let bytes = match backup.to_bytes() {
        Ok(bytes) => bytes,
        Err(error) => return refused(format!("the restore file could not be encoded: {error}")),
    };
    if let Err(error) = std::fs::create_dir_all(journal) {
        return refused(format!("{}: {error}", journal.display()));
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let path = journal.join(format!(
        "registry-restore-{}-{nanos}.reg",
        std::process::id()
    ));
    match save::save(&RealFileSystem, &path, &bytes, Baseline::Absent, false) {
        SaveOutcome::Saved(_) => {}
        other => {
            return refused(format!(
                "the restore file could not be written: {}",
                outcome_text(&other)
            ))
        }
    }
    match live::apply_confirmed(spec, steps, consent) {
        Ok(()) => EditMessage::Applied {
            side,
            backup: path,
            target,
            count: steps.len(),
        },
        Err(error) => EditMessage::ApplyFailed {
            side,
            reason: error.to_string(),
            backup: Some(path),
            now: live::export(spec, &LiveOptions::default())
                .ok()
                .map(Arc::new),
        },
    }
}

#[cfg(test)]
thread_local! {
    static COPY_OPS_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::{EditMessage, Prompt, COPY_OPS_CALLS};
    use crate::flavor::Flavor;
    use crate::model::Side;
    use crate::testing;
    use crate::RecordsView;
    use ca_records::registry::plan::LiveStep;
    use ca_records::registry::{RegFile, RegFileVersion, RegistrySpec};
    use ca_ui::testing::{context, wait_until};
    use ca_ui::{Command, SessionView};
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn a_protected_hive_asks_twice_and_the_first_answer_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (left, right) = testing::registry_pair(dir.path());
        let mut view = RecordsView::new(Flavor::Registry, left, right, &context(), 30);
        assert!(wait_until(Duration::from_secs(30), || {
            view.poll();
            view.has_comparison()
        }));
        // The side is marked live without opening any key: the prompt reads
        // only the spec.
        let spec = RegistrySpec::parse(r"HKLM\Software\compare-all-tests-never-opened").unwrap();
        if let Some(sides) = view.sides.as_mut() {
            sides.0.live = Some(spec.clone());
        }
        view.receive(EditMessage::Steps {
            side: Side::Left,
            steps: Arc::new(vec![LiveStep::CreateKey {
                key_path: format!(r"{}\X", spec.key_path()),
            }]),
            target: Arc::new(RegFile::empty(RegFileVersion::V5)),
        });
        let asked = |view: &RecordsView| match view.prompt() {
            Some(Prompt::ConfirmLive {
                protected,
                first_given,
                hive,
                ..
            }) => Some((*protected, *first_given, *hive)),
            _ => None,
        };
        assert_eq!(asked(&view), Some((true, false, "HKEY_LOCAL_MACHINE")));
        view.confirm_live();
        assert_eq!(asked(&view), Some((true, true, "HKEY_LOCAL_MACHINE")));
        assert!(!view.is_writing(), "one answer started a write");
        view.cancel_prompt();
        assert!(view.prompt().is_none());
        assert!(!view.is_writing());
    }

    #[test]
    fn steps_that_change_nothing_mark_the_side_written_without_asking() {
        let dir = tempfile::tempdir().unwrap();
        let (left, right) = testing::registry_pair(dir.path());
        let mut view = RecordsView::new(Flavor::Registry, left, right, &context(), 31);
        assert!(wait_until(Duration::from_secs(30), || {
            view.poll();
            view.has_comparison()
        }));
        view.receive(EditMessage::Steps {
            side: Side::Right,
            steps: Arc::new(Vec::new()),
            target: Arc::new(RegFile::empty(RegFileVersion::V5)),
        });
        assert!(view.prompt().is_none());
        assert!(view.message().unwrap().contains("nothing to write"));
    }

    #[test]
    fn a_refused_registry_copy_says_what_stops_it() {
        const COPIES: [Command; 3] = [
            Command::CopyToRight,
            Command::CopyToLeft,
            Command::CopyToOtherSide,
        ];
        let reasons = |view: &RecordsView| -> Vec<Option<&'static str>> {
            COPIES
                .iter()
                .map(|command| {
                    let reason = view.refusal(*command);
                    assert!(
                        reason.is_none() || !view.accepts(*command),
                        "{command:?} is accepted and refused with {reason:?}"
                    );
                    reason
                })
                .collect()
        };
        let every = |reason: &'static str| vec![Some(reason); COPIES.len()];

        let dir = tempfile::tempdir().unwrap();
        let (left, right) = testing::registry_pair(dir.path());
        let mut view = RecordsView::new(Flavor::Registry, left, right, &context(), 34);
        assert_eq!(reasons(&view), every(super::NOT_COMPARED));
        assert!(wait_until(Duration::from_secs(30), || {
            view.poll();
            view.has_comparison()
        }));
        assert!(COPIES.iter().all(|command| view.accepts(*command)));
        assert_eq!(reasons(&view), vec![None; COPIES.len()]);

        view.edit.prompt = Some(Prompt::Closing);
        assert_eq!(reasons(&view), every(super::QUESTION_OPEN));
        view.edit.prompt = None;

        let mut settings = view.settings().unwrap();
        settings.specs_mut().unwrap().disable_editing = true;
        view.apply_settings(&settings);
        assert_eq!(reasons(&view), every(super::EDITING_OFF));
        settings.specs_mut().unwrap().disable_editing = false;
        view.apply_settings(&settings);
        assert_eq!(reasons(&view), vec![None; COPIES.len()]);

        view.clipboard_input[0] = Some(String::new());
        assert_eq!(reasons(&view), every(super::CLIPBOARD_SIDE));
        view.clipboard_input[0] = None;

        view.edit.work = Some(ca_ui::worker::Job::spawn_notifying(
            |_, _| {},
            Arc::new(|| {}),
        ));
        assert_eq!(reasons(&view), every(super::WRITING));
        view.edit.work = None;

        view.progress = super::super::Progress::Failed("unreadable".to_owned());
        assert_eq!(reasons(&view), every(super::FAILED));
        view.progress = super::super::Progress::Cancelled;
        assert_eq!(reasons(&view), every(super::STOPPED));
    }

    #[test]
    fn a_registry_copy_with_no_row_to_act_on_asks_for_a_selection() {
        let dir = tempfile::tempdir().unwrap();
        let empty = "Windows Registry Editor Version 5.00\r\n\r\n";
        let left = dir.path().join("left.reg");
        let right = dir.path().join("right.reg");
        std::fs::write(&left, empty).unwrap();
        std::fs::write(&right, empty).unwrap();
        let mut view = RecordsView::new(Flavor::Registry, left, right, &context(), 36);
        assert!(wait_until(Duration::from_secs(30), || {
            view.poll();
            view.has_comparison()
        }));
        for command in [Command::CopyToRight, Command::CopyToOtherSide] {
            assert!(!view.accepts(command));
            assert_eq!(
                view.refusal(command),
                Some(super::NOTHING_CHOSEN),
                "{command:?}"
            );
        }
    }

    #[test]
    fn a_version_comparison_leaves_the_copy_reason_to_the_bar() {
        let dir = tempfile::tempdir().unwrap();
        let (left, right) = testing::registry_pair(dir.path());
        let view = RecordsView::new(Flavor::Version, left, right, &context(), 35);
        for command in [Command::CopyToRight, Command::CopyToOtherSide] {
            assert!(!view.accepts(command));
            assert_eq!(view.refusal(command), None, "{command:?}");
        }
    }

    #[test]
    fn enabling_copy_after_select_all_does_not_build_copy_operations() {
        let dir = tempfile::tempdir().unwrap();
        let (left, right) = testing::registry_pair(dir.path());
        let mut view = RecordsView::new(Flavor::Registry, left, right, &context(), 32);
        assert!(wait_until(Duration::from_secs(30), || {
            view.poll();
            view.has_comparison()
        }));
        view.run(Command::SelectAll);
        COPY_OPS_CALLS.with(|calls| calls.set(0));
        assert!(view.accepts(Command::CopyToRight));
        assert_eq!(COPY_OPS_CALLS.with(std::cell::Cell::get), 0);
    }

    #[test]
    fn a_selection_copy_is_cancelled_when_its_tab_becomes_inactive() {
        let dir = tempfile::tempdir().unwrap();
        let (left, right) = testing::registry_pair(dir.path());
        let mut view = RecordsView::new(Flavor::Registry, left, right, &context(), 33);
        assert!(wait_until(Duration::from_secs(30), || {
            view.poll();
            view.has_comparison()
        }));
        view.run(Command::SelectAll);
        let gate = view.gate_next_selection_copy();
        view.run(Command::Copy);
        view.set_active(false);
        view.set_active(true);
        gate.wait();
        assert!(wait_until(Duration::from_secs(10), || {
            view.selection_copy_finished()
        }));
        view.tick();
        assert!(view.clipboard.is_none());
        assert!(view
            .copy_status
            .as_deref()
            .is_some_and(|text| text.contains("tab became inactive")));
    }
}
