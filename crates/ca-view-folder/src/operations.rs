//! The file operations the folder view offers, and the settings each one
//! collects before anything is planned.
//!
//! Nothing here touches the disk. An [`Operation`] plus a [`Request`] is all the
//! planning worker needs, and the plan it returns is what the confirmation
//! dialog shows and what execution later runs.

use crate::selection::{Scope, Selection};
use ca_fs::{
    AttributeChange, Conflict, OperationOptions, OperationPlan, PathOption, RenameAction, Side,
    TouchSpec, Verify,
};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::SystemTime;

/// One command that changes files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    /// Copy the selection to the opposite side.
    CopyToOtherSide,
    /// Move the selection to the opposite side.
    MoveToOtherSide,
    /// Copy the selection into a chosen folder.
    CopyToFolder,
    /// Move the selection into a chosen folder.
    MoveToFolder,
    /// Remove the selection.
    Delete,
    /// Give the selection new names in place.
    Rename,
    /// Write timestamps to the selection.
    Touch,
    /// Write attributes to the selection.
    Attributes,
    /// Create one folder.
    NewFolder,
    /// Move each side's selection to the other side.
    Exchange,
    /// Add the selection to the session's name filters.
    Exclude,
}

/// Every operation, in the order the menus list them.
pub const OPERATIONS: [Operation; 11] = [
    Operation::CopyToOtherSide,
    Operation::MoveToOtherSide,
    Operation::CopyToFolder,
    Operation::MoveToFolder,
    Operation::Delete,
    Operation::Rename,
    Operation::Touch,
    Operation::Attributes,
    Operation::NewFolder,
    Operation::Exchange,
    Operation::Exclude,
];

impl Operation {
    /// The label a button or menu entry shows.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::CopyToOtherSide => "Copy to Other Side",
            Self::MoveToOtherSide => "Move to Other Side",
            Self::CopyToFolder => "Copy to Folder",
            Self::MoveToFolder => "Move to Folder",
            Self::Delete => "Delete",
            Self::Rename => "Rename",
            Self::Touch => "Touch",
            Self::Attributes => "Attributes",
            Self::NewFolder => "New Folder",
            Self::Exchange => "Exchange",
            Self::Exclude => "Exclude",
        }
    }

    /// The keystroke shown beside the label, where there is one.
    #[must_use]
    pub const fn shortcut_text(self) -> Option<&'static str> {
        match self {
            Self::CopyToOtherSide => Some("Ctrl+R"),
            Self::Delete => Some("Delete"),
            Self::Rename => Some("F2"),
            Self::NewFolder => Some("F7"),
            Self::Exclude => Some("Ctrl+K"),
            _ => None,
        }
    }

    /// The title the confirmation dialog carries.
    #[must_use]
    pub const fn dialog_title(self) -> &'static str {
        match self {
            Self::CopyToOtherSide => "Confirm Copy",
            Self::MoveToOtherSide => "Confirm Move",
            Self::CopyToFolder => "Copy to Folder",
            Self::MoveToFolder => "Move to Folder",
            Self::Delete => "Confirm Delete",
            Self::Rename => "Rename",
            Self::Touch => "Touch",
            Self::Attributes => "Attributes",
            Self::NewFolder => "New Folder",
            Self::Exchange => "Confirm Exchange",
            Self::Exclude => "Exclude",
        }
    }

    /// True when the operation needs a selection to act on.
    #[must_use]
    pub const fn needs_selection(self) -> bool {
        !matches!(self, Self::NewFolder)
    }

    /// True when the operation asks for a folder outside the comparison.
    #[must_use]
    pub const fn needs_target_folder(self) -> bool {
        matches!(self, Self::CopyToFolder | Self::MoveToFolder)
    }

    /// True when the operation reads one side's selection only.
    #[must_use]
    pub const fn is_one_sided(self) -> bool {
        matches!(
            self,
            Self::CopyToOtherSide | Self::MoveToOtherSide | Self::CopyToFolder | Self::MoveToFolder
        )
    }

    /// True when the operation removes or replaces items.
    #[must_use]
    pub const fn is_destructive(self) -> bool {
        matches!(
            self,
            Self::CopyToOtherSide
                | Self::MoveToOtherSide
                | Self::MoveToFolder
                | Self::Delete
                | Self::Exchange
        )
    }

    /// True when the operation only edits the session and writes nothing.
    #[must_use]
    pub const fn writes_files(self) -> bool {
        !matches!(self, Self::Exclude)
    }
}

/// Which timestamp a touch writes, before a time has been resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchChoice {
    /// Take each item's timestamp from its counterpart.
    FromOtherSide,
    /// Write the time the dialog holds.
    Explicit,
}

/// A tri-state attribute checkbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TriState {
    /// Clear the attribute everywhere.
    Off,
    /// Set the attribute everywhere.
    On,
    /// Leave the attribute as each item has it.
    #[default]
    Mixed,
}

impl TriState {
    /// The value this state writes, or nothing when it leaves items alone.
    #[must_use]
    pub const fn target(self) -> Option<bool> {
        match self {
            Self::Off => Some(false),
            Self::On => Some(true),
            Self::Mixed => None,
        }
    }

    /// The next state a click moves to.
    #[must_use]
    pub const fn cycled(self) -> Self {
        match self {
            Self::Mixed => Self::On,
            Self::On => Self::Off,
            Self::Off => Self::Mixed,
        }
    }

    /// The mark shown on the checkbox.
    #[must_use]
    pub const fn mark(self) -> &'static str {
        match self {
            Self::Off => " ",
            Self::On => "x",
            Self::Mixed => "-",
        }
    }
}

/// How the rename dialog computes new names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenameMode {
    /// A name mask in which `?` and `*` copy from the old name.
    Mask,
    /// A regular expression and a template.
    Regex,
}

/// Everything the dialogs collect, kept between openings so a repeated
/// operation starts where the last one left off.
#[derive(Debug, Clone)]
pub struct Form {
    /// Options every destructive operation reads.
    pub options: OperationOptions,
    /// Folder a copy or move to folder writes into.
    pub target_folder: String,
    /// Where a copy or move to folder puts path information.
    pub path_option: PathOption,
    /// Which rename mode the dialog is in.
    pub rename_mode: RenameMode,
    /// The mask the simple rename mode uses.
    pub rename_mask: String,
    /// The expression the regular expression mode matches with.
    pub rename_find: String,
    /// The template the regular expression mode builds names from.
    pub rename_replace: String,
    /// Which timestamp a touch writes.
    pub touch_choice: TouchChoice,
    /// The time the touch dialog holds, as `YYYY-MM-DD hh:mm:ss`.
    pub touch_text: String,
    /// Target state of the read-only attribute.
    pub read_only: TriState,
    /// Target state of the hidden attribute.
    pub hidden: TriState,
    /// Target state of the archive attribute.
    pub archive: TriState,
    /// The name a new folder is given.
    pub new_folder_name: String,
    /// True when Exclude offers the selection's file type rather than its
    /// names.
    pub exclude_by_type: bool,
    /// Which sides an operation that can read either one acts on.
    pub scope: Scope,
    /// The folder and suffix a copy of a replaced file is written with.
    pub backup_names: ca_fs::BackupOptions,
}

impl Default for Form {
    fn default() -> Self {
        Self {
            options: OperationOptions::default(),
            target_folder: String::new(),
            path_option: PathOption::KeepRelative,
            rename_mode: RenameMode::Mask,
            rename_mask: String::new(),
            rename_find: String::new(),
            rename_replace: String::new(),
            touch_choice: TouchChoice::Explicit,
            touch_text: String::new(),
            read_only: TriState::Mixed,
            hidden: TriState::Mixed,
            archive: TriState::Mixed,
            new_folder_name: "New Folder".to_string(),
            exclude_by_type: false,
            scope: Scope::Both,
            backup_names: ca_fs::BackupOptions::default(),
        }
    }
}

impl Form {
    /// Take the backup page of the options: the names every copy uses, and
    /// whether a replaced file is copied unless the dialog says otherwise.
    pub fn follow_backups(&mut self, options: &ca_session::options::BackupOptions) {
        self.backup_names = ca_ui::save::backup::names(options);
        self.options.backup = options.before_overwrite.then(|| self.backup_names.clone());
    }

    /// The attribute edit the dialog's three checkboxes stand for.
    #[must_use]
    pub const fn attribute_change(&self) -> AttributeChange {
        AttributeChange {
            read_only: self.read_only.target(),
            hidden: self.hidden.target(),
            archive: self.archive.target(),
            system: None,
            unix_mode: None,
        }
    }

    /// The rename the dialog's fields stand for.
    #[must_use]
    pub fn rename_action(&self) -> RenameAction {
        match self.rename_mode {
            RenameMode::Mask => RenameAction::Mask(self.rename_mask.clone()),
            RenameMode::Regex => RenameAction::Regex {
                find: self.rename_find.clone(),
                replace: self.rename_replace.clone(),
            },
        }
    }

    /// The timestamp a touch writes, read from the dialog's own field.
    #[must_use]
    pub fn touch_spec(&self) -> Option<TouchSpec> {
        match self.touch_choice {
            TouchChoice::FromOtherSide => Some(TouchSpec::FromOtherSide),
            TouchChoice::Explicit => parse_stamp(&self.touch_text).map(TouchSpec::Explicit),
        }
    }
}

/// Read `YYYY-MM-DD hh:mm:ss` into a point in time, in coordinated universal
/// time.
#[must_use]
pub fn parse_stamp(text: &str) -> Option<SystemTime> {
    let text = text.trim();
    let (date, clock) = text.split_once(' ')?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: i64 = date_parts.next()?.parse().ok()?;
    let day: i64 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let mut clock_parts = clock.split(':');
    let hour: i64 = clock_parts.next()?.parse().ok()?;
    let minute: i64 = clock_parts.next()?.parse().ok()?;
    let second: i64 = clock_parts.next().unwrap_or("0").parse().ok()?;
    if clock_parts.next().is_some() || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let seconds = days * 86_400 + hour * 3_600 + minute * 60 + second;
    let magnitude = std::time::Duration::from_secs(seconds.unsigned_abs());
    if seconds >= 0 {
        std::time::UNIX_EPOCH.checked_add(magnitude)
    } else {
        std::time::UNIX_EPOCH.checked_sub(magnitude)
    }
}

/// A civil date to days since the epoch, by the usual era based algorithm.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let shifted = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * shifted + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Everything the planning worker needs, taken from the view in one frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    /// The operation to plan.
    pub operation: Operation,
    /// Selected paths on the left side.
    pub left: BTreeSet<PathBuf>,
    /// Selected paths on the right side.
    pub right: BTreeSet<PathBuf>,
    /// The side a one-sided operation reads from.
    pub from: Side,
    /// The sides a two-sided operation acts on.
    pub scope: Scope,
    /// Left base folder.
    pub left_base: PathBuf,
    /// Right base folder.
    pub right_base: PathBuf,
    /// Options the plan is built with.
    pub options: OperationOptions,
    /// Folder a copy or move to folder writes into.
    pub target_folder: Option<PathBuf>,
    /// Where a copy or move to folder puts path information.
    pub path_option: PathOption,
    /// How a rename computes new names.
    pub rename: RenameAction,
    /// Which timestamp a touch writes.
    pub touch: TouchSpec,
    /// The attribute edit to apply.
    pub attributes: AttributeChange,
    /// Folder the new folder is created under.
    pub new_folder_parent: PathBuf,
    /// Name the new folder is given.
    pub new_folder_name: String,
}

impl Request {
    /// Build the request one operation over one selection stands for.
    #[must_use]
    pub fn build(
        operation: Operation,
        selection: &Selection,
        form: &Form,
        left_base: PathBuf,
        right_base: PathBuf,
        new_folder_parent: PathBuf,
    ) -> Self {
        let from = match selection.occupied_scope() {
            Some(Scope::Right) => Side::Right,
            _ => Side::Left,
        };
        let mut request = Self {
            operation,
            left: selection.side(Side::Left).clone(),
            right: selection.side(Side::Right).clone(),
            from,
            scope: form.scope,
            left_base,
            right_base,
            options: OperationOptions::default(),
            target_folder: None,
            path_option: form.path_option,
            rename: RenameAction::Mask(String::new()),
            touch: TouchSpec::FromOtherSide,
            attributes: AttributeChange::default(),
            new_folder_parent,
            new_folder_name: String::new(),
        };
        request.take_form(form);
        request
    }

    /// The same request with every setting the dialog holds read again from
    /// `form`.
    ///
    /// The selection, the bases and the new folder's parent stay as this
    /// request took them, so a difference from this request is a difference
    /// the dialog made.
    #[must_use]
    pub fn with_form(&self, form: &Form) -> Self {
        let mut request = self.clone();
        request.take_form(form);
        request
    }

    fn take_form(&mut self, form: &Form) {
        let occupied = match (self.left.is_empty(), self.right.is_empty()) {
            (true, true) => None,
            (false, true) => Some(Scope::Left),
            (true, false) => Some(Scope::Right),
            (false, false) => Some(Scope::Both),
        };
        self.scope = occupied.unwrap_or(form.scope);
        self.options = form.options.clone();
        self.target_folder = (!form.target_folder.trim().is_empty())
            .then(|| PathBuf::from(form.target_folder.trim()));
        self.path_option = form.path_option;
        self.rename = form.rename_action();
        self.touch = form.touch_spec().unwrap_or(TouchSpec::FromOtherSide);
        self.attributes = form.attribute_change();
        self.new_folder_name.clone_from(&form.new_folder_name);
    }
}

/// How many files and folders a plan touches, and how large they are.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlanTotals {
    /// Steps that act on a file.
    pub files: usize,
    /// Steps that act on a folder.
    pub folders: usize,
    /// Bytes the plan moves.
    pub bytes: u64,
}

/// Count the files, folders and bytes a plan covers.
#[must_use]
pub fn totals(plan: &OperationPlan) -> PlanTotals {
    use ca_fs::StepAction;
    let mut totals = PlanTotals {
        bytes: plan.total_bytes(),
        ..PlanTotals::default()
    };
    for step in &plan.steps {
        match step.action {
            StepAction::CreateDir { .. } | StepAction::DeleteDir { .. } => totals.folders += 1,
            _ => totals.files += 1,
        }
    }
    totals
}

/// The heading one group of conflicts is listed under.
#[must_use]
pub const fn conflict_heading(conflict: Conflict) -> &'static str {
    match conflict {
        Conflict::TargetExists => "Target exists and will be replaced",
        Conflict::OverwriteNewer => "A newer item will be replaced by an older one",
        Conflict::TargetReadOnly => "The target is read-only",
        Conflict::TargetHiddenOrSystem => "The target is hidden or a system item",
        Conflict::DeleteReadOnly => "A read-only item will be removed",
        Conflict::KindMismatch => "A file on one side and a folder on the other",
        Conflict::CaseOnlyRename => "The new name differs only in letter case",
        Conflict::RemovesLinkOnly => "A link is acted on, not what it points at",
        Conflict::CounterpartUnreadable => "The counterpart could not be read",
        Conflict::Drift => "The disk no longer matches the plan",
        Conflict::DestinationIsAnotherSource => "Two items claim one destination",
    }
}

/// Every conflict a plan reports, grouped by kind, with the steps in each
/// group.
#[must_use]
pub fn conflicts_by_kind(plan: &OperationPlan) -> Vec<(Conflict, Vec<PathBuf>)> {
    let mut grouped: std::collections::BTreeMap<Conflict, Vec<PathBuf>> =
        std::collections::BTreeMap::new();
    for step in &plan.steps {
        for conflict in &step.conflicts {
            grouped
                .entry(*conflict)
                .or_default()
                .push(step.action.target().to_path_buf());
        }
    }
    for skip in plan.refusals() {
        if let Some(conflict) = skip.conflict {
            grouped.entry(conflict).or_default().push(skip.path.clone());
        }
    }
    grouped.into_iter().collect()
}

/// The label one verification choice shows.
#[must_use]
pub const fn verify_label(verify: Verify) -> &'static str {
    match verify {
        Verify::None => "Do not verify",
        Verify::Size => "Verify size",
        Verify::Hash => "Verify contents",
    }
}

/// The label one path option shows.
#[must_use]
pub const fn path_option_label(option: PathOption) -> &'static str {
    match option {
        PathOption::KeepRelative => "Keep relative folder structure",
        PathOption::KeepBase => "Keep base folder structure",
        PathOption::Flatten => "Do not keep folder structure",
    }
}

#[cfg(test)]
mod tests {
    use super::{days_from_civil, parse_stamp, Form, Operation, TriState, OPERATIONS};
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn every_operation_has_a_label_and_a_dialog_title() {
        for operation in OPERATIONS {
            assert!(!operation.label().is_empty());
            assert!(!operation.dialog_title().is_empty());
        }
        assert_eq!(OPERATIONS.len(), 11);
    }

    #[test]
    fn only_exclude_leaves_the_files_alone() {
        for operation in OPERATIONS {
            assert_eq!(
                operation.writes_files(),
                operation != Operation::Exclude,
                "{operation:?}"
            );
        }
    }

    #[test]
    fn a_tri_state_cycles_through_its_three_answers() {
        let mut state = TriState::Mixed;
        state = state.cycled();
        assert_eq!(state, TriState::On);
        state = state.cycled();
        assert_eq!(state, TriState::Off);
        state = state.cycled();
        assert_eq!(state, TriState::Mixed);
        assert_eq!(TriState::Mixed.target(), None);
    }

    #[test]
    fn an_untouched_attribute_form_changes_nothing() {
        let form = Form::default();
        assert!(form.attribute_change().is_empty());
    }

    #[test]
    fn a_timestamp_field_reads_back_as_the_time_it_names() {
        assert_eq!(parse_stamp("1970-01-01 00:00:00"), Some(UNIX_EPOCH));
        assert_eq!(
            parse_stamp("1970-01-02 00:00:01"),
            Some(UNIX_EPOCH + Duration::from_secs(86_401))
        );
        assert_eq!(parse_stamp("not a time"), None);
        assert_eq!(parse_stamp("1970-13-01 00:00:00"), None);
        assert_eq!(parse_stamp("1970-01-01 25:00:00"), None);
    }

    #[test]
    fn the_civil_date_arithmetic_round_trips() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2024, 2, 29), 19_782);
    }
}
