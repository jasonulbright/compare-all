//! Two pane folder comparison, with the file operations that act on a
//! selection.
//!
//! Every operation follows one order: plan on a worker, show the plan and wait
//! for a confirmation, execute on a worker with a journal, report what did not
//! complete, then re-read only the folders the batch touched. Nothing reaches
//! the disk that was not shown first.

pub mod dialogs;

pub mod jobs;
pub mod merge_view;
pub mod operations;
pub mod opjobs;

pub mod selection;
pub mod settings;
pub mod sync_mode;
pub mod tree;

pub use merge_view::FolderMergeView;
pub use opjobs::MISSING_FILES;

use ca_fs::{
    ContentMethod, ContentOutcome, DisplayFilter, ExecutionReport, FolderDisplayFilter, Node,
    NodeStatus, OperationPlan, Side, SyncAction,
};
use ca_session::SessionKind;
use ca_ui::command::Command;
use ca_ui::dialog::{self, DialogMessage, Pick, Target};
use ca_ui::report::{ReportKind, ViewReport};
use ca_ui::scroll::RowScroll;
use ca_ui::theme::{self, Palette};
use ca_ui::toolbar;
use ca_ui::view::{self, OpenRequest, SessionView, ViewAction, ViewContext};
use ca_ui::widgets;
use ca_ui::worker::{Cancel, Job};
use dialogs::{ConfirmChoice, RecoveryChoice};
use jobs::{ContentMessage, ScanFailure, ScanMessage, SortMessage};
use operations::{Form, Operation, Request};
use opjobs::{
    Answer, Ask, CleanupMessage, ExecMessage, PlanMessage, PlanOutcome, PreviewMessage,
    ProgressState, RecoverMessage, RecoveryNotice, RescanMessage,
};
use selection::{ClickKind, Scope, SelectRule, Selection};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use sync_mode::SyncMode;
use tree::{Arena, ArenaNode, Expanded, FlatRow, Sort, SortColumn};

/// Row height as a multiple of the folder listing font size.
///
/// The default folder point size times this ratio is sixteen points, which is
/// the measured row height, so a build that states no font size keeps it.
const ROW_HEIGHT_RATIO: f32 = 4.0 / 3.0;
/// Why a comparison with an archive side takes no file operation.
const ARCHIVE_SIDE_REFUSAL: &str =
    "A side is an archive read as a folder. File operations need two local folders.";
/// Height of the column header row.
const HEADER_HEIGHT: f32 = 22.0;
/// Width of the splitter between the two panes.
const SPLITTER_WIDTH: f32 = 6.0;
/// Narrowest share of the width either pane may be dragged to.
const MINIMUM_SPLIT: f32 = 0.1;
/// Starting width of a size column.
const SIZE_WIDTH: f32 = 90.0;
/// Starting width of a timestamp column.
const TIME_WIDTH: f32 = 160.0;
/// No column may be dragged narrower than this.
const MINIMUM_COLUMN: f32 = 36.0;
/// How wide the grab area of a column edge is.
const GRIP: f32 = 5.0;
/// Indent added per tree level.
const INDENT: f32 = 14.0;
/// Thickness of the vertical scrollbar.
const SCROLLBAR: f32 = 12.0;
/// Width the display filter drop down is laid out in.
const FILTER_COMBO_WIDTH: f32 = 230.0;
/// Width the folder handling drop down is laid out in.
const FOLDER_COMBO_WIDTH: f32 = 250.0;
/// Width the content method drop down is laid out in.
const METHOD_COMBO_WIDTH: f32 = 120.0;
/// Width the name filter label and field are laid out in.
const NAME_FILTER_WIDTH: f32 = 190.0;
/// Width the synchronisation method drop down is laid out in.
const SYNC_COMBO_WIDTH: f32 = 170.0;
/// Height the synchronisation preview is given.
const PREVIEW_HEIGHT: f32 = 220.0;
/// What a control this build does not carry says when hovered.
const NOT_BUILT: &str = "Not available in this build";
/// How often streamed content results are folded into the rows on screen.
const REBUILD_INTERVAL: Duration = Duration::from_millis(100);

/// Every command from the shared vocabulary this view answers for.
const HANDLED: &[Command] = &[
    Command::CompareReport,
    Command::ExpandAll,
    Command::CollapseAll,
    Command::CompareContents,
    Command::SwapSides,
    Command::Reload,
    Command::Cancel,
    Command::SelectAll,
    Command::SelectAllFiles,
    Command::SelectDifferences,
    Command::SelectNewer,
    Command::SelectOrphans,
    Command::ClearSelection,
    Command::CopyToRight,
    Command::CopyToLeft,
    Command::CopyToOtherSide,
    Command::MoveToOtherSide,
    Command::CopyToFolder,
    Command::MoveToFolder,
    Command::Delete,
    Command::Rename,
    Command::Touch,
    Command::Attributes,
    Command::NewFolder,
    Command::Exchange,
    Command::Exclude,
];

/// Every command from the shared vocabulary the synchronisation view adds.
const SYNC_HANDLED: &[Command] = &[Command::Synchronize];

/// The operation a file operation command stands for.
const fn operation_of(command: Command) -> Option<Operation> {
    Some(match command {
        Command::CopyToOtherSide => Operation::CopyToOtherSide,
        Command::MoveToOtherSide => Operation::MoveToOtherSide,
        Command::CopyToFolder => Operation::CopyToFolder,
        Command::MoveToFolder => Operation::MoveToFolder,
        Command::Delete => Operation::Delete,
        Command::Rename => Operation::Rename,
        Command::Touch => Operation::Touch,
        Command::Attributes => Operation::Attributes,
        Command::NewFolder => Operation::NewFolder,
        Command::Exchange => Operation::Exchange,
        Command::Exclude => Operation::Exclude,
        _ => return None,
    })
}

/// The selection rule a select command stands for.
const fn select_rule_of(command: Command) -> Option<SelectRule> {
    Some(match command {
        Command::SelectAll => SelectRule::All,
        Command::SelectAllFiles => SelectRule::AllFiles,
        Command::SelectDifferences => SelectRule::Differences,
        Command::SelectNewer => SelectRule::Newer,
        Command::SelectOrphans => SelectRule::Orphans,
        _ => return None,
    })
}

/// Which comparison the view is carrying out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Two folders side by side.
    Compare,
    /// Two folders reconciled by copies and deletions.
    Sync,
}

/// Where a comparison currently stands.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Status {
    /// Work is in progress, with the step it has reached.
    Running(&'static str),
    /// The comparison is on screen.
    Ready,
    /// The comparison could not be produced.
    Failed(String),
    /// The run stopped before it finished.
    Cancelled,
}

impl Status {
    const fn is_running(&self) -> bool {
        matches!(self, Status::Running(_))
    }
}

/// Where one file operation stands.
enum Stage {
    /// No operation is in flight.
    Idle,
    /// A plan is being built.
    Planning(Operation, Basis),
    /// A plan is on screen waiting for a confirmation.
    Confirming(Box<Shown>),
    /// The dialog's settings changed after the plan was shown, and the plan is
    /// being built again from them. The plan held is the one that was shown,
    /// and the basis is the one being planned.
    Replanning(Box<Shown>),
    /// The confirmed plan is running.
    Running(Box<OperationPlan>),
    /// The batch has finished and its result is on screen.
    Summary(Box<OperationPlan>, Box<ExecutionReport>),
}

impl Stage {
    const fn is_busy(&self) -> bool {
        matches!(
            self,
            Stage::Planning(..) | Stage::Replanning(_) | Stage::Running(_)
        )
    }

    const fn blocks_input(&self) -> bool {
        !matches!(self, Stage::Idle)
    }
}

/// A plan on screen, with what it was built from.
struct Shown {
    operation: Operation,
    plan: Box<OperationPlan>,
    basis: Basis,
    /// Set when the plan replaced one whose steps the dialog's settings
    /// changed, so the dialog says why it is asking again.
    revised: bool,
}

/// What a plan was built from, so a confirmation can tell whether the settings
/// the dialog now holds still describe it.
enum Basis {
    /// An operation over the selection.
    Request(Box<Request>),
    /// A synchronisation over the preview.
    Sync(Box<SyncBasis>),
}

/// What a synchronisation plan was built from.
struct SyncBasis {
    preset: ca_fs::SyncPreset,
    preview: ca_fs::SyncPreview,
    options: ca_fs::OperationOptions,
}

impl Basis {
    /// The basis the dialog's settings stand for now, or `None` when they are
    /// the ones this basis holds.
    fn revised(&self, form: &Form) -> Option<Self> {
        match self {
            Self::Request(request) => {
                let current = request.with_form(form);
                (current != **request).then(|| Self::Request(Box::new(current)))
            }
            Self::Sync(sync) => (sync.options != form.options).then(|| {
                Self::Sync(Box::new(SyncBasis {
                    preset: sync.preset.clone(),
                    preview: sync.preview.clone(),
                    options: form.options.clone(),
                }))
            }),
        }
    }
}

/// True when two plans list the same steps with the same conflicts and leave
/// the same items alone, which is everything the dialog shows of a plan.
fn shows_the_same_steps(shown: &OperationPlan, built: &OperationPlan) -> bool {
    shown.steps.len() == built.steps.len()
        && shown
            .steps
            .iter()
            .zip(&built.steps)
            .all(|(a, b)| a.action == b.action && a.conflicts == b.conflicts)
        && shown.skipped == built.skipped
}

/// A folder comparison tab.
#[allow(clippy::struct_excessive_bools)]
pub struct FolderView {
    id: egui::Id,
    mode: Mode,
    left_path: PathBuf,
    right_path: PathBuf,
    left_field: String,
    right_field: String,
    /// Kept so every job this view spawns asks for the repaint that shows its
    /// result, whichever code path spawned it.
    notify: Arc<dyn Fn() + Send + Sync>,
    scan_job: Option<Job<ScanMessage>>,
    content_job: Option<Job<ContentMessage>>,
    sort_job: Option<Job<SortMessage>>,
    picker: Option<Job<DialogMessage>>,
    picker_target: Target,
    plan_job: Option<Job<PlanMessage>>,
    exec_job: Option<Job<ExecMessage>>,
    rescan_job: Option<Job<RescanMessage>>,
    recover_job: Option<Job<RecoverMessage>>,
    cleanup_job: Option<Job<CleanupMessage>>,
    preview_job: Option<Job<PreviewMessage>>,
    /// The comparison the rows and the compared tree belong to. Every restart
    /// starts a new one.
    comparisons: ca_ui::schedule::Generations,
    /// The comparison the plan, rescan and preview jobs were started over.
    /// Each of them holds the compared tree, and a tree an earlier comparison
    /// produced must never replace the current one.
    plan_comparison: u64,
    rescan_comparison: u64,
    preview_comparison: u64,
    extract_job: Option<Job<jobs::ExtractMessage>>,
    /// Tabs to open on the next frame, from work that finished off the frame.
    pending_opens: Vec<OpenRequest>,
    /// The report command of this view.
    report: ViewReport,
    arena: Arena,
    compared_tree: Option<Box<Node>>,
    expanded: Expanded,
    visible: Vec<bool>,
    rows: Vec<FlatRow>,
    filter: DisplayFilter,
    folder_filter: FolderDisplayFilter,
    name_filter: String,
    /// The session's settings, which the scan and the comparison options are
    /// derived from.
    session_settings: ca_session::settings::FolderCompareSettings,
    /// Which operations ask before they run. Read from the application options
    /// each frame, so a change reaches the next operation without a restart.
    confirmations: ca_session::options::FileOperationOptions,
    /// The backup page last taken into the form, so an edit made in a dialog
    /// stays until the page itself changes.
    backups: Option<ca_session::options::BackupOptions>,
    /// The Archive Types page last read from the options, for the next scan.
    archive_masks: Option<ca_session::options::ArchiveOptions>,
    /// What the last scan opened each side as. Empty until a scan lands.
    sides: Option<jobs::Sides>,
    sort: Sort,
    status: Status,
    left_count: usize,
    right_count: usize,
    scan_errors: Vec<ScanFailure>,
    root_incomplete: bool,
    show_errors: bool,
    content_method: ContentMethod,
    content_done: usize,
    selection: Selection,
    form: Form,
    stage: Stage,
    ask: Option<Arc<Ask>>,
    exec_cancel: Option<Cancel>,
    progress: Arc<Mutex<ProgressState>>,
    notices: Vec<RecoveryNotice>,
    show_recovery: bool,
    message: Option<(String, String)>,
    sync: SyncMode,
    /// Where this view's batches write their write-ahead records.
    journal_directory: PathBuf,
    offset_seconds: i32,
    font: ca_ui::font::FontSize,
    /// Padding the options add between rows, read once per frame.
    line_spacing: u32,
    scroll: RowScroll,
    viewport_height: f32,
    size_width: f32,
    time_width: f32,
    split: f32,
    /// Set when streamed results have landed but the rows have not been rebuilt
    /// for them yet.
    rebuild_pending: bool,
    last_rebuild: Instant,
    rebuilds: u64,
    /// When the comparison last finished, for the automatic refresh interval.
    last_refresh: Instant,
}

impl FolderView {
    /// A comparison of two folders, with the scan already started.
    #[must_use]
    pub fn new(left: PathBuf, right: PathBuf, context: &ViewContext, instance: u64) -> Self {
        Self::in_mode(left, right, context, instance, Mode::Compare)
    }

    /// A synchronisation of two folders, with the scan already started.
    #[must_use]
    pub fn new_sync(left: PathBuf, right: PathBuf, context: &ViewContext, instance: u64) -> Self {
        Self::in_mode(left, right, context, instance, Mode::Sync)
    }

    /// A comparison whose batches journal somewhere other than the settings
    /// directory.
    #[must_use]
    pub fn with_journal_directory(
        left: PathBuf,
        right: PathBuf,
        context: &ViewContext,
        instance: u64,
        journal_directory: PathBuf,
    ) -> Self {
        Self::in_mode_with_journal_directory(
            left,
            right,
            context,
            instance,
            Mode::Compare,
            journal_directory,
        )
    }

    /// A synchronisation whose batches journal somewhere other than the
    /// settings directory.
    #[must_use]
    pub fn sync_with_journal_directory(
        left: PathBuf,
        right: PathBuf,
        context: &ViewContext,
        instance: u64,
        journal_directory: PathBuf,
    ) -> Self {
        Self::in_mode_with_journal_directory(
            left,
            right,
            context,
            instance,
            Mode::Sync,
            journal_directory,
        )
    }

    fn in_mode_with_journal_directory(
        left: PathBuf,
        right: PathBuf,
        context: &ViewContext,
        instance: u64,
        mode: Mode,
        journal_directory: PathBuf,
    ) -> Self {
        let mut view = Self::bare(left, right, context, instance, mode);
        view.journal_directory = journal_directory;
        view.start_jobs();
        view
    }

    fn in_mode(
        left: PathBuf,
        right: PathBuf,
        context: &ViewContext,
        instance: u64,
        mode: Mode,
    ) -> Self {
        let mut view = Self::bare(left, right, context, instance, mode);
        view.start_jobs();
        view
    }

    /// Start the work a freshly built view owes: recovery, then the scan.
    fn start_jobs(&mut self) {
        self.recover_job = Some(opjobs::spawn_recovery(
            self.journal_directory.clone(),
            self.notify.clone(),
        ));
        self.restart();
    }

    fn bare(
        left: PathBuf,
        right: PathBuf,
        context: &ViewContext,
        instance: u64,
        mode: Mode,
    ) -> Self {
        let mut view = Self {
            id: egui::Id::new(("folder-compare", instance)),
            mode,
            left_field: left.display().to_string(),
            right_field: right.display().to_string(),
            left_path: left,
            right_path: right,
            notify: context.notify.clone(),
            scan_job: None,
            content_job: None,
            sort_job: None,
            picker: None,
            picker_target: Target::Left,
            plan_job: None,
            exec_job: None,
            rescan_job: None,
            recover_job: None,
            cleanup_job: None,
            preview_job: None,
            comparisons: ca_ui::schedule::Generations::new(),
            plan_comparison: 0,
            rescan_comparison: 0,
            preview_comparison: 0,
            extract_job: None,
            pending_opens: Vec::new(),
            report: ViewReport::new(
                ReportKind::Folder,
                egui::Id::new(("folder-compare", instance)),
                context.notify.clone(),
            ),
            arena: Arena::default(),
            compared_tree: None,
            expanded: Expanded::new(),
            visible: Vec::new(),
            rows: Vec::new(),
            filter: DisplayFilter::ShowAll,
            folder_filter: FolderDisplayFilter::default(),
            name_filter: String::new(),
            session_settings: ca_session::settings::FolderCompareSettings::default(),
            confirmations: ca_session::options::FileOperationOptions::default(),
            backups: None,
            archive_masks: Some(context.options.stored.archives.clone()),
            sides: None,
            sort: Sort::default(),
            status: Status::Running("Starting"),
            left_count: 0,
            right_count: 0,
            scan_errors: Vec::new(),
            root_incomplete: false,
            show_errors: false,
            content_method: ContentMethod::Binary,
            content_done: 0,
            selection: Selection::new(),
            form: Form::default(),
            stage: Stage::Idle,
            ask: None,
            exec_cancel: None,
            progress: Arc::new(Mutex::new(ProgressState::default())),
            notices: Vec::new(),
            show_recovery: false,
            message: None,
            sync: SyncMode::new(),
            journal_directory: ca_ui::paths::journal_directory(),
            offset_seconds: ca_ui::format::probed_offset().unwrap_or(0),
            scroll: RowScroll::top(),
            viewport_height: 0.0,
            font: ca_ui::font::FontSize::new(ca_session::options::provisional::FOLDER_POINT_SIZE),
            line_spacing: 0,
            size_width: SIZE_WIDTH,
            time_width: TIME_WIDTH,
            split: 0.5,
            rebuild_pending: false,
            last_rebuild: Instant::now(),
            rebuilds: 0,
            last_refresh: Instant::now(),
        };
        view.comparisons.start();
        view.rebuild();
        view
    }

    /// A view over a comparison the caller already holds, with nothing to scan.
    #[must_use]
    pub fn from_arena(
        left: PathBuf,
        right: PathBuf,
        context: &ViewContext,
        instance: u64,
        arena: Arena,
    ) -> Self {
        let mut view = Self::new(left, right, context, instance);
        view.drop_jobs();
        view.arena = arena;
        view.expanded.expand_all(&view.arena);
        view.status = Status::Ready;
        view.rebuild();
        view
    }

    /// A view over a compared tree the caller already holds.
    ///
    /// The tree is what the operations plan against, so a view built this way
    /// can plan without a scan having run.
    #[must_use]
    pub fn from_tree(
        left: PathBuf,
        right: PathBuf,
        context: &ViewContext,
        instance: u64,
        tree: Box<Node>,
    ) -> Self {
        let mut view = Self::bare(left, right, context, instance, Mode::Compare);
        view.arena = Arena::from_root(&tree);
        view.compared_tree = Some(tree);
        view.expanded.expand_all(&view.arena);
        view.status = Status::Ready;
        view.rebuild();
        view
    }

    /// Write this view's journals somewhere other than the settings directory.
    ///
    /// Batches already planned are unaffected; the directory is read when a
    /// batch starts.
    pub fn set_journal_directory(&mut self, directory: PathBuf) {
        self.journal_directory = directory;
    }

    /// Where this view's batches write their journals.
    #[must_use]
    pub fn journal_directory(&self) -> &Path {
        &self.journal_directory
    }

    /// Carry out the plan that is waiting for a confirmation.
    ///
    /// A plan whose settings the dialog still holds runs as it was shown. A
    /// setting changed in the dialog builds the plan again from the dialog:
    /// the new plan runs at once when it lists the steps shown, and goes back
    /// on screen for a second confirmation when it does not.
    pub fn confirm_pending(&mut self) {
        if let Stage::Confirming(shown) = std::mem::replace(&mut self.stage, Stage::Idle) {
            self.confirm(*shown);
        }
    }

    fn confirm(&mut self, shown: Shown) {
        let Some(basis) = shown.basis.revised(&self.form) else {
            self.execute_plan(shown.plan);
            return;
        };
        let Some(tree) = self.compared_tree.take() else {
            self.message = Some((
                shown.operation.dialog_title().to_string(),
                "The comparison is not loaded yet.".to_string(),
            ));
            return;
        };
        self.plan_comparison = self.comparisons.current();
        self.plan_job = Some(match &basis {
            Basis::Request(request) => {
                opjobs::spawn_plan(tree, (**request).clone(), self.notify.clone())
            }
            Basis::Sync(sync) => opjobs::spawn_sync_plan(
                tree,
                sync.preset.clone(),
                sync.preview.clone(),
                self.left_path.clone(),
                self.right_path.clone(),
                sync.options.clone(),
                self.notify.clone(),
            ),
        });
        self.stage = Stage::Replanning(Box::new(Shown { basis, ..shown }));
    }

    /// Drop the plan that is waiting for a confirmation, touching nothing.
    pub fn cancel_pending(&mut self) {
        if matches!(self.stage, Stage::Confirming(_)) {
            self.stage = Stage::Idle;
        }
    }

    /// Close the result of the batch that has just finished.
    pub fn dismiss_summary(&mut self) {
        if matches!(self.stage, Stage::Summary(_, _)) {
            self.stage = Stage::Idle;
        }
    }

    /// True once a batch has finished and its result is on screen.
    #[must_use]
    pub const fn has_summary(&self) -> bool {
        matches!(self.stage, Stage::Summary(_, _))
    }

    /// True while a batch is writing files.
    #[must_use]
    pub const fn is_running_batch(&self) -> bool {
        matches!(self.stage, Stage::Running(_))
    }

    /// The comparison currently on screen.
    #[must_use]
    pub fn arena(&self) -> &Arena {
        &self.arena
    }

    /// Which comparison the view is carrying out.
    #[must_use]
    pub const fn mode(&self) -> Mode {
        self.mode
    }

    /// The rows selected, on each side.
    #[must_use]
    pub const fn selection(&self) -> &Selection {
        &self.selection
    }

    /// The rows selected, for a caller that wants to change them.
    pub fn selection_mut(&mut self) -> &mut Selection {
        &mut self.selection
    }

    /// Select every visible row a rule takes, on the sides a scope covers.
    pub fn select(&mut self, rule: SelectRule, scope: Scope) {
        let rows = std::mem::take(&mut self.rows);
        self.selection.select_rule(rule, scope, &rows, &self.arena);
        self.rows = rows;
    }

    /// The settings the operation dialogs collect.
    pub fn form_mut(&mut self) -> &mut Form {
        &mut self.form
    }

    /// The title and body of the notice on screen, when one is up.
    #[must_use]
    pub fn message(&self) -> Option<(&str, &str)> {
        self.message
            .as_ref()
            .map(|(title, body)| (title.as_str(), body.as_str()))
    }

    /// The session's name filter, as the filter box holds it.
    #[must_use]
    pub fn name_filter(&self) -> &str {
        &self.name_filter
    }

    /// The widget of the name filter box, for a test that gives it the
    /// keyboard.
    #[must_use]
    pub fn name_filter_widget(&self) -> egui::Id {
        name_filter_id(self.id)
    }

    /// The widget that stands for the tree when the keyboard is handed out.
    #[must_use]
    pub fn tree_widget(&self) -> egui::Id {
        self.id.with("tree-keys")
    }

    /// The synchronisation half of the view.
    #[must_use]
    pub const fn sync(&self) -> &SyncMode {
        &self.sync
    }

    /// Choose the synchronisation method and rebuild the preview.
    pub fn set_sync_method(&mut self, method: sync_mode::Method) {
        self.sync.choose(method);
        self.refresh_preview();
    }

    /// Force one preview row to an action the method did not choose.
    pub fn override_sync_row(&mut self, rel: PathBuf, action: SyncAction) {
        self.sync.set_override(rel, action);
    }

    /// Why no file operation runs on this comparison, when a side is a
    /// container read as a folder. The operation pipeline writes and reads
    /// local paths only.
    fn archive_refusal(&self) -> Option<&'static str> {
        self.sides
            .as_ref()
            .filter(|sides| !sides.are_local())
            .map(|_| ARCHIVE_SIDE_REFUSAL)
    }

    /// Plan the synchronisation the preview shows.
    pub fn start_synchronisation(&mut self) {
        self.start_sync();
    }

    /// Why the last synchronisation attempt produced nothing.
    #[must_use]
    pub fn sync_refusal(&self) -> Option<&str> {
        self.sync.refusal.as_deref()
    }

    /// How many rows the synchronisation would act on.
    #[must_use]
    pub fn sync_pending(&self) -> usize {
        self.sync.pending()
    }

    /// The unfinished batches the journal directory reported.
    #[must_use]
    pub fn recovery_notices(&self) -> &[RecoveryNotice] {
        &self.notices
    }

    /// The plan waiting for a confirmation, if one is on screen.
    #[must_use]
    pub fn pending_plan(&self) -> Option<&OperationPlan> {
        match &self.stage {
            Stage::Confirming(shown) => Some(&shown.plan),
            Stage::Running(plan) | Stage::Summary(plan, _) => Some(plan),
            _ => None,
        }
    }

    /// The report of the batch that has just finished, if one is on screen.
    #[must_use]
    pub fn last_report(&self) -> Option<&ExecutionReport> {
        match &self.stage {
            Stage::Summary(_, report) => Some(report),
            _ => None,
        }
    }

    /// True while a plan is on screen waiting for an answer.
    #[must_use]
    pub const fn is_confirming(&self) -> bool {
        matches!(self.stage, Stage::Confirming(_))
    }

    /// The question a running batch is waiting on, if there is one.
    #[must_use]
    pub fn pending_question(&self) -> Option<opjobs::PendingQuestion> {
        self.ask.as_ref().and_then(|ask| ask.pending())
    }

    /// Answer the question a running batch is waiting on.
    pub fn answer(&self, shown: &opjobs::PendingQuestion, answer: Answer) {
        if let Some(ask) = self.ask.as_ref() {
            ask.answer(shown, answer);
        }
    }

    /// Fold one content result into the comparison.
    pub fn apply_content(&mut self, update: &ca_fs::ContentUpdate) -> bool {
        if !self.arena.apply_content(update, false) {
            return false;
        }
        self.content_done += 1;
        self.rebuild_pending = true;
        true
    }

    /// How many times the rows have been laid out again since the view opened.
    #[must_use]
    pub const fn rebuild_count(&self) -> u64 {
        self.rebuilds
    }

    /// The rows currently laid out.
    #[must_use]
    pub fn rows(&self) -> &[FlatRow] {
        &self.rows
    }

    /// The settings of the session this view shows, as they now stand.
    ///
    /// The controls the toolbar carries are written back into the stored
    /// groups, so the two never hold different values. The sides are the
    /// folders the view has open.
    #[must_use]
    pub fn session_settings(&self) -> ca_session::settings::FolderCompareSettings {
        let mut settings = self.session_settings.clone();
        settings.specs =
            ca_ui::view::with_sides(&settings.specs, &self.left_path, &self.right_path);
        settings.comparison.content_comparison = match self.content_method {
            ContentMethod::Crc32 => ca_session::settings::folder::ContentComparison::Crc,
            ContentMethod::Binary => ca_session::settings::folder::ContentComparison::Binary,
            ContentMethod::Rules => ca_session::settings::folder::ContentComparison::RulesBased,
        };
        settings
    }

    /// Take the settings of the session and read both sides again under them.
    ///
    /// The sides and the description change no scan, so a change of them
    /// alone is kept and scans nothing again.
    pub fn apply_session_settings(
        &mut self,
        settings: &ca_session::settings::FolderCompareSettings,
    ) {
        let mut scanned = settings.clone();
        scanned.specs.clone_from(&self.session_settings.specs);
        if scanned == self.session_settings {
            self.session_settings.specs.clone_from(&settings.specs);
            return;
        }
        self.session_settings = settings.clone();
        self.name_filter = crate::settings::name_filter_text(&settings.name_filters);
        self.apply_copy_settings();
        if let Some(method) =
            crate::settings::content_method(&settings.comparison.content_comparison)
        {
            self.content_method = method;
        }
        self.restart();
    }

    /// Take the masks of the Archive Types options page.
    ///
    /// A change reopens both sides and scans again, as a change of the session
    /// settings does: a side opened under the old masks may be a folder, a
    /// container or neither under the new ones.
    pub fn apply_archive_masks(&mut self, masks: &ca_session::options::ArchiveOptions) {
        if self.archive_masks.as_ref() == Some(masks) {
            return;
        }
        self.archive_masks = Some(masks.clone());
        self.restart();
    }

    /// Why the scan failed, once it has.
    #[must_use]
    pub fn failure(&self) -> Option<&str> {
        match &self.status {
            Status::Failed(reason) => Some(reason),
            _ => None,
        }
    }

    /// True once both sides are scanned and compared.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.status == Status::Ready
    }

    /// The order in force.
    #[must_use]
    pub fn sort_order(&self) -> Sort {
        self.sort
    }

    /// The entries the scan could not read.
    #[must_use]
    pub fn scan_errors(&self) -> &[ScanFailure] {
        &self.scan_errors
    }

    /// Start one operation: build its plan on a worker.
    ///
    /// Nothing is written here and nothing is decided here; the plan comes back
    /// to the confirmation dialog.
    pub fn start_operation(&mut self, operation: Operation) {
        if self.stage.blocks_input() {
            return;
        }
        if let Some(reason) = self.archive_refusal() {
            self.message = Some((operation.dialog_title().to_string(), reason.to_string()));
            return;
        }
        if operation.needs_selection() && self.selection.is_empty() {
            self.message = Some((
                operation.dialog_title().to_string(),
                "Select the items to act on first.".to_string(),
            ));
            return;
        }
        let Some(tree) = self.compared_tree.take() else {
            self.message = Some((
                operation.dialog_title().to_string(),
                "The comparison is not loaded yet.".to_string(),
            ));
            return;
        };
        let parent = self.new_folder_parent();
        let request = Request::build(
            operation,
            &self.selection,
            &self.form,
            self.left_path.clone(),
            self.right_path.clone(),
            parent,
        );
        self.plan_comparison = self.comparisons.current();
        self.plan_job = Some(opjobs::spawn_plan(
            tree,
            request.clone(),
            self.notify.clone(),
        ));
        self.stage = Stage::Planning(operation, Basis::Request(Box::new(request)));
    }

    /// Copy the selection to one named side.
    fn copy_to(&mut self, side: Side) {
        self.selection.restrict_to(opposite(side));
        self.start_operation(Operation::CopyToOtherSide);
    }

    /// Where a new folder is created, taken from the row the keyboard sits on.
    fn new_folder_parent(&self) -> PathBuf {
        let Some((_, rel)) = self.selection.cursor() else {
            return PathBuf::new();
        };
        let Some(index) = self.arena.index_of(rel) else {
            return PathBuf::new();
        };
        let Some(node) = self.arena.node(index) else {
            return PathBuf::new();
        };
        if node.is_dir && self.expanded.is_expanded(index) {
            return node.rel.clone();
        }
        node.rel
            .parent()
            .map_or_else(PathBuf::new, Path::to_path_buf)
    }

    /// Run the confirmed plan.
    fn execute_plan(&mut self, plan: Box<OperationPlan>) {
        let ask = Ask::new(self.notify.clone(), Cancel::new());
        if let Ok(mut state) = self.progress.lock() {
            *state = ProgressState::default();
        }
        let job = opjobs::spawn_execute(
            plan.clone(),
            self.journal_directory.clone(),
            Arc::clone(&ask),
            Arc::clone(&self.progress),
            self.notify.clone(),
        );
        self.exec_cancel = Some(job.cancel_handle());
        self.ask = Some(ask);
        self.exec_job = Some(job);
        self.stage = Stage::Running(plan);
    }

    /// Re-read the folders the finished batch touched.
    fn rescan_after(&mut self, plan: &OperationPlan) {
        let Some(tree) = self.compared_tree.take() else {
            return;
        };
        let roots = opjobs::affected_roots(plan);
        self.rescan_comparison = self.comparisons.current();
        self.rescan_job = Some(opjobs::spawn_rescan(
            tree,
            self.left_path.clone(),
            self.right_path.clone(),
            roots,
            self.name_filter.clone(),
            Box::new(self.engine_options()),
            self.notify.clone(),
        ));
    }

    /// Rebuild the synchronisation preview from the comparison.
    fn refresh_preview(&mut self) {
        if self.mode != Mode::Sync || self.stage.blocks_input() {
            return;
        }
        let Some(tree) = self.compared_tree.take() else {
            return;
        };
        self.sync.refusal = None;
        self.preview_comparison = self.comparisons.current();
        self.preview_job = Some(opjobs::spawn_preview(
            tree,
            self.sync.method.preset(),
            self.sync.overrides(),
            self.notify.clone(),
        ));
    }

    /// Plan the synchronisation the preview shows.
    fn start_sync(&mut self) {
        if self.stage.blocks_input() {
            return;
        }
        if let Some(reason) = self.archive_refusal() {
            self.sync.refusal = Some(reason.to_string());
            return;
        }
        let Some(preview) = self.sync.preview.clone() else {
            self.sync.refusal = Some("Build the preview first.".to_string());
            return;
        };
        let Some(tree) = self.compared_tree.take() else {
            return;
        };
        let basis = SyncBasis {
            preset: self.sync.method.preset(),
            preview,
            options: self.form.options.clone(),
        };
        self.plan_comparison = self.comparisons.current();
        self.plan_job = Some(opjobs::spawn_sync_plan(
            tree,
            basis.preset.clone(),
            basis.preview.clone(),
            self.left_path.clone(),
            self.right_path.clone(),
            basis.options.clone(),
            self.notify.clone(),
        ));
        self.stage = Stage::Planning(Operation::CopyToOtherSide, Basis::Sync(Box::new(basis)));
    }

    /// Give the operation form the three copy settings the handling group
    /// states, leaving the values the dialogs collect where they are.
    fn apply_copy_settings(&mut self) {
        let stated = crate::settings::operation_options(&self.session_settings.handling);
        self.form.options.preserve_created = stated.preserve_created;
        self.form.options.preserve_attributes = stated.preserve_attributes;
        self.form.options.touch_source_after_copy = stated.touch_source_after_copy;
    }

    /// The options every job of this view runs under.
    #[must_use]
    pub fn engine_options(&self) -> crate::settings::EngineOptions {
        let mut options = crate::settings::options_of(&self.session_settings());
        options.archives = crate::settings::archive_types(
            &self.session_settings.handling.archive_handling,
            self.archive_masks.as_ref(),
        );
        options
    }

    /// Open the folders the handling settings say to open when a comparison
    /// lands.
    fn apply_load_expansion(&mut self) {
        let handling = crate::settings::handling_options(&self.session_settings.handling);
        if !handling.expand_on_load {
            self.expanded.collapse_all();
            return;
        }
        if handling.expand_only_with_differences {
            self.expanded.expand_differences(&self.arena);
        } else {
            self.expanded.expand_all(&self.arena);
        }
    }

    /// Start the scan again when the refresh interval has run out.
    fn poll_automatic_refresh(&mut self) {
        // Read the one group rather than the whole option set: this runs on
        // every frame.
        let Some(minutes) =
            crate::settings::handling_options(&self.session_settings.handling).refresh_minutes
        else {
            return;
        };
        if !self.is_ready() || self.stage.blocks_input() || self.scan_job.is_some() {
            return;
        }
        let interval = Duration::from_secs(u64::from(minutes) * 60);
        if self.last_refresh.elapsed() >= interval {
            self.last_refresh = Instant::now();
            self.restart();
        }
    }

    fn restart(&mut self) {
        self.comparisons.start();
        self.drop_jobs();
        self.drop_operation_in_wait();
        self.arena = Arena::default();
        self.sides = None;
        self.compared_tree = None;
        self.expanded = Expanded::new();
        self.rows.clear();
        self.visible.clear();
        self.left_count = 0;
        self.right_count = 0;
        self.scan_errors.clear();
        self.root_incomplete = false;
        self.content_done = 0;
        self.rebuild_pending = false;
        self.sync.stale = true;
        self.status = Status::Running("Starting");
        self.scan_job = Some(jobs::spawn_scan(
            self.left_path.clone(),
            self.right_path.clone(),
            self.name_filter.clone(),
            Box::new(self.engine_options()),
            self.notify.clone(),
        ));
    }

    /// Stop the jobs and let go of them, for a view that is being replaced.
    ///
    /// The plan, rescan and preview jobs hold the compared tree. Letting go of
    /// them drops that tree with them, so it can never land over the tree of
    /// the comparison that replaces it.
    fn drop_jobs(&mut self) {
        for job in [
            self.scan_job.take().map(|job| job.cancel_handle()),
            self.content_job.take().map(|job| job.cancel_handle()),
            self.sort_job.take().map(|job| job.cancel_handle()),
            self.plan_job.take().map(|job| job.cancel_handle()),
            self.rescan_job.take().map(|job| job.cancel_handle()),
            self.preview_job.take().map(|job| job.cancel_handle()),
        ]
        .into_iter()
        .flatten()
        {
            job.cancel();
        }
    }

    /// Drop the operation that waits on a plan or on a confirmation, because
    /// its plan describes a comparison that is being replaced.
    fn drop_operation_in_wait(&mut self) {
        let operation = match std::mem::replace(&mut self.stage, Stage::Idle) {
            Stage::Planning(operation, _) => operation,
            Stage::Replanning(held) | Stage::Confirming(held) => held.operation,
            other => {
                self.stage = other;
                return;
            }
        };
        self.message = Some((
            operation.dialog_title().to_string(),
            "The folders were read again before the operation ran, so it did not run. \
             Nothing was changed."
                .to_string(),
        ));
    }

    /// Stop the jobs but keep their handles.
    fn request_stop(&mut self) {
        for job in [
            self.scan_job.as_ref().map(Job::cancel_handle),
            self.content_job.as_ref().map(Job::cancel_handle),
            self.sort_job.as_ref().map(Job::cancel_handle),
            self.rescan_job.as_ref().map(Job::cancel_handle),
            self.plan_job.as_ref().map(Job::cancel_handle),
            self.exec_cancel.clone(),
        ]
        .into_iter()
        .flatten()
        {
            job.cancel();
        }
    }

    /// Take everything the background work has posted.
    fn poll(&mut self) {
        self.poll_scan();
        self.poll_content();
        self.poll_sort();
        self.poll_picker();
        self.poll_plan();
        self.poll_exec();
        self.poll_rescan();
        self.poll_recovery();
        self.poll_cleanup();
        self.poll_preview();
        self.poll_extract();
        self.poll_automatic_refresh();
        self.flush_rebuild();
    }

    fn poll_scan(&mut self) {
        let Some(job) = self.scan_job.as_mut() else {
            return;
        };
        let messages = job.drain();
        let finished = job.is_finished();
        for message in messages {
            match message {
                ScanMessage::Counted { side, entries } => match side {
                    jobs::Side::Left => self.left_count = entries,
                    jobs::Side::Right => self.right_count = entries,
                },
                ScanMessage::Progress(step) => self.status = Status::Running(step),
                ScanMessage::Partial(arena) => {
                    // The top level only. Nothing plans against it, so the
                    // compared tree stays empty until the full pass lands.
                    self.arena = *arena;
                    self.apply_load_expansion();
                    self.selection.retain_known(&self.arena);
                    self.rebuild();
                }
                ScanMessage::Failed(reason) => self.status = Status::Failed(reason),
                ScanMessage::Cancelled => self.status = Status::Cancelled,
                ScanMessage::Ready {
                    arena,
                    tree,
                    errors,
                    root_incomplete,
                    sources,
                } => {
                    self.arena = *arena;
                    self.sides = Some(*sources);
                    self.compared_tree = Some(tree);
                    self.scan_errors = errors;
                    self.root_incomplete = root_incomplete;
                    self.apply_load_expansion();
                    self.status = Status::Ready;
                    self.last_refresh = Instant::now();
                    self.selection.retain_known(&self.arena);
                    if self.sort != Sort::default() {
                        self.start_sort();
                    }
                    self.rebuild();
                    self.refresh_preview();
                }
            }
        }
        if finished {
            self.scan_job = None;
            if self.status.is_running() {
                self.status = Status::Cancelled;
            }
        }
    }

    fn poll_content(&mut self) {
        let Some(job) = self.content_job.as_mut() else {
            return;
        };
        let messages = job.drain();
        let finished = job.is_finished();
        for message in messages {
            match message {
                ContentMessage::Update(update) => {
                    self.apply_content(&update);
                }
                ContentMessage::Done { tree, complete } => {
                    if let Some(tree) = tree {
                        self.compared_tree = Some(tree);
                    }
                    self.status = if complete {
                        Status::Ready
                    } else {
                        Status::Cancelled
                    };
                    self.rebuild_pending = true;
                }
                ContentMessage::Failed(reason) => self.status = Status::Failed(reason),
            }
        }
        if finished {
            self.content_job = None;
            if self.status.is_running() {
                self.status = Status::Cancelled;
            }
        }
    }

    fn poll_sort(&mut self) {
        let Some(job) = self.sort_job.as_mut() else {
            return;
        };
        let messages = job.drain();
        let finished = job.is_finished();
        for message in messages {
            match message {
                SortMessage::Done(arena) => {
                    self.arena = *arena;
                    self.rebuild();
                }
                SortMessage::Cancelled => {}
                SortMessage::Failed(reason) => self.status = Status::Failed(reason),
            }
        }
        if finished {
            self.sort_job = None;
        }
    }

    fn poll_picker(&mut self) {
        let Some(job) = self.picker.as_mut() else {
            return;
        };
        let messages = job.drain();
        let finished = job.is_finished();
        let mut reload = false;
        for message in messages {
            match message {
                DialogMessage::Chosen(path) => {
                    let text = path.display().to_string();
                    match self.picker_target {
                        Target::Left => self.left_field = text,
                        Target::Right => self.right_field = text,
                    }
                    reload = true;
                }
                DialogMessage::Dismissed => {}
                DialogMessage::Failed(reason) => self.status = Status::Failed(reason),
            }
        }
        if finished {
            self.picker = None;
        }
        if reload {
            self.apply_fields();
        }
    }

    fn poll_plan(&mut self) {
        let Some(job) = self.plan_job.as_mut() else {
            return;
        };
        let messages = job.drain();
        let finished = job.is_finished();
        // Stop can land after the worker has answered; the answer then must
        // not run either.
        let stopped = job.is_cancelled();
        let current = self.comparisons.is_current(self.plan_comparison);
        for message in messages {
            if !current {
                continue;
            }
            match message {
                PlanMessage::Done { tree, outcome } => {
                    if let Some(tree) = tree {
                        self.compared_tree = Some(tree);
                    }
                    self.accept_plan(if stopped {
                        PlanOutcome::Stopped
                    } else {
                        outcome
                    });
                }
                PlanMessage::Failed(reason) => {
                    self.stage = Stage::Idle;
                    self.message = Some(("Planning failed".to_string(), reason));
                }
            }
        }
        if finished {
            self.plan_job = None;
            if matches!(self.stage, Stage::Planning(..) | Stage::Replanning(_)) {
                self.stage = Stage::Idle;
            }
        }
    }

    /// True when `operation` asks before it runs.
    ///
    /// An operation whose option is cleared runs at once. Every operation with
    /// no option of its own keeps asking, because turning a confirmation off is
    /// a choice and not a default.
    #[must_use]
    pub fn asks_before(&self, operation: Operation) -> bool {
        let options = &self.confirmations;
        match operation {
            Operation::CopyToOtherSide | Operation::CopyToFolder => options.confirm_copy,
            Operation::MoveToOtherSide | Operation::MoveToFolder | Operation::Exchange => {
                options.confirm_move
            }
            Operation::Delete => options.confirm_delete,
            _ => true,
        }
    }

    /// Take the confirmations the application options state.
    pub fn set_confirmations(&mut self, options: ca_session::options::FileOperationOptions) {
        self.confirmations = options;
    }

    fn accept_plan(&mut self, outcome: PlanOutcome) {
        let (operation, basis, shown) = match std::mem::replace(&mut self.stage, Stage::Idle) {
            Stage::Planning(operation, basis) => (operation, basis, None),
            Stage::Replanning(held) => {
                let Shown {
                    operation,
                    plan,
                    basis,
                    ..
                } = *held;
                (operation, basis, Some(plan))
            }
            other => {
                self.stage = other;
                return;
            }
        };
        match outcome {
            PlanOutcome::Plan(plan) => match shown {
                // The dialog showed these steps and was confirmed; only the
                // settings they run under changed.
                Some(previous) if shows_the_same_steps(&previous, &plan) => {
                    self.execute_plan(plan);
                }
                Some(_) => {
                    self.stage = Stage::Confirming(Box::new(Shown {
                        operation,
                        plan,
                        basis,
                        revised: true,
                    }));
                }
                None if !self.asks_before(operation) => self.execute_plan(plan),
                None => {
                    self.stage = Stage::Confirming(Box::new(Shown {
                        operation,
                        plan,
                        basis,
                        revised: false,
                    }));
                }
            },
            PlanOutcome::Masks(masks) => self.apply_exclusions(&masks),
            PlanOutcome::Refused(reason) => {
                self.message = Some((operation.dialog_title().to_string(), reason));
            }
            PlanOutcome::Stopped => {
                self.message = Some((
                    operation.dialog_title().to_string(),
                    "Stopped before the operation ran. Nothing was changed.".to_string(),
                ));
            }
        }
    }

    /// Add the Exclude command's masks to the session's name filters.
    fn apply_exclusions(&mut self, masks: &ca_fs::ExcludeMasks) {
        for mask in masks.files.iter().chain(masks.folders.iter()) {
            let entry = format!("-{mask}");
            if self.name_filter.split(';').any(|part| part.trim() == entry) {
                continue;
            }
            if !self.name_filter.trim().is_empty() {
                self.name_filter.push(';');
            }
            self.name_filter.push_str(&entry);
        }
        self.restart();
    }

    fn poll_exec(&mut self) {
        let Some(job) = self.exec_job.as_mut() else {
            return;
        };
        let messages = job.drain();
        let finished = job.is_finished();
        for message in messages {
            match message {
                ExecMessage::Done(report) => {
                    let plan = match std::mem::replace(&mut self.stage, Stage::Idle) {
                        Stage::Running(plan) => plan,
                        other => {
                            self.stage = other;
                            continue;
                        }
                    };
                    self.rescan_after(&plan);
                    self.stage = Stage::Summary(plan, report);
                }
                ExecMessage::Failed(reason) => {
                    self.stage = Stage::Idle;
                    self.message = Some(("The batch did not run".to_string(), reason));
                }
            }
        }
        if finished {
            self.exec_job = None;
            self.ask = None;
            self.exec_cancel = None;
        }
    }

    fn poll_rescan(&mut self) {
        let Some(job) = self.rescan_job.as_mut() else {
            return;
        };
        let messages = job.drain();
        let finished = job.is_finished();
        let current = self.comparisons.is_current(self.rescan_comparison);
        for message in messages {
            if !current {
                continue;
            }
            match message {
                RescanMessage::Done { arena, tree } => {
                    // The scroll position and the selection are kept: the rows
                    // are named by path, so only what disappeared is dropped.
                    self.arena = *arena;
                    self.compared_tree = Some(tree);
                    self.expanded.expand_all(&self.arena);
                    self.selection.retain_known(&self.arena);
                    self.rebuild();
                    self.sync.stale = true;
                }
                RescanMessage::Failed { reason, tree } => {
                    if let Some(tree) = tree {
                        self.compared_tree = Some(tree);
                    }
                    self.status = Status::Failed(reason);
                }
            }
        }
        if finished {
            self.rescan_job = None;
        }
    }

    fn poll_recovery(&mut self) {
        let Some(job) = self.recover_job.as_mut() else {
            return;
        };
        let messages = job.drain();
        let finished = job.is_finished();
        for message in messages {
            match message {
                RecoverMessage::Done {
                    notices,
                    offset_seconds,
                } => {
                    self.offset_seconds = offset_seconds;
                    self.show_recovery = !notices.is_empty();
                    self.notices = notices;
                }
                RecoverMessage::Failed(reason) => {
                    self.message = Some(("Recovery could not run".to_string(), reason));
                }
            }
        }
        if finished {
            self.recover_job = None;
        }
    }

    fn poll_cleanup(&mut self) {
        let Some(job) = self.cleanup_job.as_mut() else {
            return;
        };
        let messages = job.drain();
        let finished = job.is_finished();
        for message in messages {
            match message {
                CleanupMessage::Done { journal, failures } => {
                    if failures.is_empty() {
                        self.notices.retain(|notice| notice.journal != journal);
                        self.show_recovery = !self.notices.is_empty();
                    } else {
                        let body = failures
                            .iter()
                            .map(|(path, why)| format!("{}: {why}", path.display()))
                            .collect::<Vec<String>>()
                            .join("\n");
                        self.message = Some(("Some files remain".to_string(), body));
                    }
                }
                CleanupMessage::Failed(reason) => {
                    self.message = Some(("Recovery could not run".to_string(), reason));
                }
            }
        }
        if finished {
            self.cleanup_job = None;
        }
    }

    fn poll_extract(&mut self) {
        let Some(job) = self.extract_job.as_mut() else {
            return;
        };
        let messages = job.drain();
        let finished = job.is_finished();
        for message in messages {
            match message {
                jobs::ExtractMessage::Ready(request) => self.pending_opens.push(*request),
                jobs::ExtractMessage::Failed(reason) => {
                    self.message = Some(("Open".to_string(), reason));
                }
                jobs::ExtractMessage::Cancelled => {}
            }
        }
        if finished {
            self.extract_job = None;
        }
    }

    /// The tabs the view asks for next frame, taken so each opens once.
    pub fn take_pending_opens(&mut self) -> Vec<OpenRequest> {
        std::mem::take(&mut self.pending_opens)
    }

    fn poll_preview(&mut self) {
        let Some(job) = self.preview_job.as_mut() else {
            return;
        };
        let messages = job.drain();
        let finished = job.is_finished();
        let current = self.comparisons.is_current(self.preview_comparison);
        for message in messages {
            if !current {
                continue;
            }
            match message {
                PreviewMessage::Done { tree, preview } => {
                    if let Some(tree) = tree {
                        self.compared_tree = Some(tree);
                    }
                    self.sync.preview = Some(*preview);
                    self.sync.stale = false;
                }
                PreviewMessage::Failed(reason) => self.sync.refusal = Some(reason),
            }
        }
        if finished {
            self.preview_job = None;
        }
    }

    /// Fold whatever has arrived into the rows, at most once per interval.
    fn flush_rebuild(&mut self) {
        if !self.rebuild_pending || self.last_rebuild.elapsed() < REBUILD_INTERVAL {
            return;
        }
        self.arena.roll_up();
        self.rebuild();
    }

    /// Height of one tree row at the size the options state.
    #[must_use]
    pub fn row_height(&self) -> f32 {
        self.font.row_height(ROW_HEIGHT_RATIO, self.line_spacing)
    }

    fn rebuild(&mut self) {
        self.visible = self.arena.visibility(self.filter, self.folder_filter);
        self.rows = self.arena.flatten(&self.expanded, &self.visible);
        self.scroll
            .clamp(self.viewport_height, self.row_height(), self.rows.len());
        self.rebuild_pending = false;
        self.last_rebuild = Instant::now();
        self.rebuilds = self.rebuilds.saturating_add(1);
    }

    fn apply_fields(&mut self) {
        self.left_path = PathBuf::from(self.left_field.clone());
        self.right_path = PathBuf::from(self.right_field.clone());
        self.selection.clear();
        self.restart();
    }

    fn open_picker(&mut self, target: Target) {
        if self.picker.is_some() {
            return;
        }
        self.picker_target = target;
        self.picker = Some(dialog::spawn(Pick::Folder, self.notify.clone()));
    }

    fn start_sort(&mut self) {
        if let Some(job) = self.sort_job.take() {
            job.cancel();
        }
        if self.arena.is_empty() {
            return;
        }
        self.sort_job = Some(jobs::spawn_sort(
            Box::new(self.arena.clone()),
            self.sort,
            self.notify.clone(),
        ));
    }

    fn set_sort(&mut self, column: SortColumn) {
        self.sort = self.sort.clicked(column);
        self.start_sort();
    }

    fn start_content_comparison(&mut self) {
        let Some(tree) = self.compared_tree.take() else {
            return;
        };
        self.content_done = 0;
        self.status = Status::Running("Comparing contents");
        let sides = self.sides.clone().unwrap_or_else(|| jobs::Sides {
            left: ca_fs::Source::local(&self.left_path),
            right: ca_fs::Source::local(&self.right_path),
        });
        self.content_job = Some(jobs::spawn_contents(
            tree,
            sides,
            self.content_method,
            Box::new(self.engine_options()),
            self.notify.clone(),
        ));
    }

    fn path_bar(&mut self, ui: &mut egui::Ui) {
        let action = widgets::path_bar(ui, &mut self.left_field, &mut self.right_field, "Refresh");
        match action {
            Some(widgets::PathBarAction::Browse(target)) => self.open_picker(target),
            Some(widgets::PathBarAction::Reload) => self.apply_fields(),
            None => {}
        }
    }

    /// The selection scopes the shared vocabulary does not name, and the count.
    ///
    /// The file operations and the both-sides select rules reach the menu bar
    /// through [`SessionView::commands`]. What is left here is the per-side
    /// scope of a rule, which one command cannot express.
    fn selection_bar(&mut self, ui: &mut egui::Ui) {
        let mut rule: Option<(SelectRule, Scope)> = None;
        ui.horizontal_wrapped(|ui| {
            for select in [SelectRule::Newer, SelectRule::Orphans] {
                ui.menu_button(select.label(), |ui| {
                    for scope in [Scope::Left, Scope::Right, Scope::Both] {
                        if ui.button(scope.label()).clicked() {
                            rule = Some((select, scope));
                            ui.close_menu();
                        }
                    }
                });
            }
            ui.separator();
            ui.label(format!("{} selected", self.selection.len()));
        });
        if let Some((select, scope)) = rule {
            let rows = std::mem::take(&mut self.rows);
            self.selection
                .select_rule(select, scope, &rows, &self.arena);
            self.rows = rows;
        }
    }

    /// The toolbar items this view declares, in the state it is in now.
    #[must_use]
    pub fn toolbar_items(&self) -> Vec<toolbar::Item> {
        let running = self.status.is_running() || self.stage.is_busy();
        let has_tree = self.compared_tree.is_some();
        let has_selection = !self.selection.is_empty();
        let idle = matches!(self.stage, Stage::Idle);
        vec![
            toolbar::Item::widget("home", 70.0),
            toolbar::Item::widget("sessions", 90.0),
            toolbar::Item::separator("separator-1"),
            toolbar::Item::widget("all", 46.0),
            toolbar::Item::widget("diffs", 56.0),
            toolbar::Item::widget("same", 56.0),
            toolbar::Item::widget("filter", FILTER_COMBO_WIDTH),
            toolbar::Item::widget("structure", FOLDER_COMBO_WIDTH),
            toolbar::Item::widget("minor", 60.0),
            toolbar::Item::widget("rules", 60.0),
            toolbar::Item::separator("separator-2"),
            toolbar::Item::command(
                "copy",
                Command::CopyToOtherSide,
                "Copy",
                idle && has_selection,
                "Select the items to copy first",
            ),
            toolbar::Item::widget("expand", 80.0),
            toolbar::Item::widget("collapse", 90.0),
            toolbar::Item::widget("select", 80.0),
            toolbar::Item::widget("files", 60.0),
            toolbar::Item::widget("method", METHOD_COMBO_WIDTH),
            toolbar::Item::command(
                "compare-contents",
                Command::CompareContents,
                "Compare Contents",
                has_tree && !running,
                "Available once the scan finishes",
            ),
            toolbar::Item::command(
                "refresh",
                Command::Reload,
                "Refresh",
                !running,
                "Work is in progress",
            ),
            toolbar::Item::command(
                "swap",
                Command::SwapSides,
                "Swap",
                !running,
                "Work is in progress",
            ),
            toolbar::Item::command(
                "stop",
                Command::Cancel,
                "Stop",
                running,
                "Nothing is running",
            ),
            toolbar::Item::separator("separator-3"),
            toolbar::Item::widget("name-filter", NAME_FILTER_WIDTH),
            toolbar::Item::widget("peek", 60.0),
            toolbar::Item::command(
                "report",
                Command::CompareReport,
                "Report",
                has_tree,
                "Available once the scan finishes",
            ),
        ]
    }

    #[allow(clippy::too_many_lines)]
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let running = self.status.is_running() || self.stage.is_busy();
        let mut filter = self.filter;
        let mut folder_filter = self.folder_filter;
        let mut method = self.content_method;
        let mut expand: Option<bool> = None;
        let mut rescan = false;
        let mut rule: Option<(SelectRule, Scope)> = None;
        let empty = self.arena.is_empty();
        let id = self.id;
        let items = self.toolbar_items();
        let layout = toolbar::Layout::from_options(
            &ca_ui::options::runtime::current(ui.ctx()).stored.commands,
            toolbar::ToolbarView::Folder,
        );
        let name_filter = &mut self.name_filter;
        let outcome = toolbar::show_for(
            toolbar::ToolbarView::Folder,
            ui,
            id.with("toolbar"),
            &items,
            &layout,
            |ui, name| match name {
                "home" => {
                    widgets::toolbar_button(ui, "Home", false, NOT_BUILT);
                }
                "sessions" => {
                    widgets::toolbar_button(ui, "Sessions", false, NOT_BUILT);
                }
                "all" | "diffs" | "same" => {
                    let (label, option) = match name {
                        "all" => ("All", DisplayFilter::ShowAll),
                        "diffs" => ("Diffs", DisplayFilter::ShowDifferences),
                        _ => ("Same", DisplayFilter::ShowSame),
                    };
                    if ui
                        .add(
                            widgets::IconButton::new(
                                label,
                                ca_ui::icons::toolbar_icon(toolbar::ToolbarView::Folder, name),
                            )
                            .selected(filter == option),
                        )
                        .clicked()
                    {
                        filter = option;
                    }
                }
                "filter" => {
                    egui::ComboBox::from_id_salt(id.with("filter"))
                        .width(FILTER_COMBO_WIDTH)
                        .selected_text(filter_label(filter))
                        .show_ui(ui, |ui| {
                            for option in DISPLAY_FILTERS {
                                ui.selectable_value(&mut filter, option, filter_label(option));
                            }
                        });
                }
                "structure" => {
                    egui::ComboBox::from_id_salt(id.with("folders"))
                        .width(FOLDER_COMBO_WIDTH)
                        .selected_text(folder_filter_label(folder_filter))
                        .show_ui(ui, |ui| {
                            for option in [
                                FolderDisplayFilter::AlwaysShowFolders,
                                FolderDisplayFilter::CompareFilesAndFolderStructure,
                                FolderDisplayFilter::OnlyCompareFiles,
                            ] {
                                ui.selectable_value(
                                    &mut folder_filter,
                                    option,
                                    folder_filter_label(option),
                                );
                            }
                        });
                }
                "minor" => {
                    widgets::toolbar_button(ui, "Minor", false, NOT_BUILT);
                }
                "rules" => {
                    widgets::toolbar_button(ui, "Rules", false, NOT_BUILT);
                }
                "expand" => {
                    if widgets::toolbar_button(ui, "Expand", !empty, "Nothing loaded") {
                        expand = Some(true);
                    }
                }
                "collapse" => {
                    if widgets::toolbar_button(ui, "Collapse", !empty, "Nothing loaded") {
                        expand = Some(false);
                    }
                }
                "select" => {
                    widgets::icon_menu(ui, "Select", ca_ui::icons::Icon::Select, |ui| {
                        ui.set_min_width(200.0);
                        for select in [
                            SelectRule::All,
                            SelectRule::AllFiles,
                            SelectRule::Differences,
                            SelectRule::Newer,
                            SelectRule::Orphans,
                        ] {
                            if ui.button(select.label()).clicked() {
                                rule = Some((select, Scope::Both));
                                ui.close_menu();
                            }
                        }
                    });
                }
                "files" => {
                    widgets::toolbar_button(ui, "Files", false, NOT_BUILT);
                }
                "method" => {
                    egui::ComboBox::from_id_salt(id.with("method"))
                        .width(METHOD_COMBO_WIDTH)
                        .selected_text(method_label(method))
                        .show_ui(ui, |ui| {
                            for option in [ContentMethod::Binary, ContentMethod::Crc32] {
                                ui.selectable_value(&mut method, option, method_label(option));
                            }
                        });
                }
                "name-filter" => {
                    ui.label("Filters");
                    let response = ui.add(
                        egui::TextEdit::singleline(name_filter)
                            .id(name_filter_id(id))
                            .hint_text("*.rs;-target")
                            .desired_width(140.0),
                    );
                    if response.lost_focus()
                        && ui.input(|input| input.key_pressed(egui::Key::Enter))
                    {
                        rescan = true;
                    }
                }
                "peek" => {
                    widgets::toolbar_button(ui, "Peek", false, NOT_BUILT);
                    if running {
                        ui.spinner();
                    }
                }
                _ => {}
            },
        );
        let _ = outcome.rect;
        self.content_method = method;
        let mut contents = false;
        let mut cancel = false;
        let mut swap = false;
        let mut copy = false;
        match outcome.command {
            Some(Command::CopyToOtherSide) => copy = true,
            Some(Command::CompareContents) => contents = true,
            Some(Command::Reload) => rescan = true,
            Some(Command::SwapSides) => swap = true,
            Some(Command::Cancel) => cancel = true,
            Some(Command::CompareReport) => self.report.request(),
            _ => {}
        }
        if filter != self.filter {
            self.filter = filter;
            self.rebuild();
        }
        if folder_filter != self.folder_filter {
            self.folder_filter = folder_filter;
            self.rebuild();
        }
        match expand {
            Some(true) => {
                self.expanded.expand_all(&self.arena);
                self.rebuild();
            }
            Some(false) => {
                self.expanded.collapse_all();
                self.rebuild();
            }
            None => {}
        }
        if let Some((select, scope)) = rule {
            self.selection
                .select_rule(select, scope, &self.rows, &self.arena);
        }
        if rescan {
            self.restart();
        }
        if contents {
            self.start_content_comparison();
        }
        if cancel {
            self.request_stop();
        }
        if swap {
            self.run(Command::SwapSides);
        }
        if copy {
            self.start_operation(Operation::CopyToOtherSide);
        }
    }

    /// The comparison the report is written from.
    #[must_use]
    ///
    /// The rows are the ones the tree is showing, so the display filter and the
    /// open folders both reach the document.
    pub fn report_payload(&self) -> (ca_ui::report::ReportMeta, ca_ui::report::Payload) {
        let meta = ca_ui::report::ReportMeta::new(
            self.left_path.display().to_string(),
            self.right_path.display().to_string(),
        )
        .with_title(ReportKind::Folder.title());
        let mut rows = Vec::with_capacity(self.rows.len());
        for flat in &self.rows {
            let Some(node) = self.arena.node(flat.node) else {
                continue;
            };
            rows.push(ca_ui::report::FolderRow {
                depth: u32::from(flat.depth),
                name: node.name.clone(),
                relative_path: node.rel.to_string_lossy().replace('\\', "/"),
                is_dir: node.is_dir,
                status: ca_ui::report::EntryStatus::from(node.status),
                left: side_facts(node.left.as_ref(), node.is_dir),
                right: side_facts(node.right.as_ref(), node.is_dir),
                link: None,
            });
        }
        (meta, ca_ui::report::Payload::Folder(rows))
    }

    fn report_panel(&mut self, ui: &mut egui::Ui) {
        if !self.report.is_open() {
            return;
        }
        if self.report.draw(ui) == ca_ui::report::ReportAction::Write {
            let (meta, payload) = self.report_payload();
            self.report.start(meta, payload, 0);
        }
        if let Some(text) = self.report.take_clipboard() {
            ui.ctx().copy_text(text);
        }
    }

    /// The name the report display filter carries for what the tree shows.
    #[must_use]
    pub const fn report_filter(&self) -> &'static str {
        match self.filter {
            DisplayFilter::ShowDifferences => "mismatches",
            DisplayFilter::ShowSame => "matches",
            DisplayFilter::ShowNoOrphans => "no-orphans",
            DisplayFilter::ShowDifferencesNoOrphans => "mismatches-no-orphans",
            DisplayFilter::ShowOrphans => "orphans",
            DisplayFilter::ShowLeftNewer => "left-newer",
            DisplayFilter::ShowRightNewer => "right-newer",
            DisplayFilter::ShowLeftNewerAndLeftOrphans => "left-newer-orphans",
            DisplayFilter::ShowRightNewerAndRightOrphans => "right-newer-orphans",
            DisplayFilter::ShowLeftOrphans => "left-orphans",
            DisplayFilter::ShowRightOrphans => "right-orphans",
            DisplayFilter::ShowAll | DisplayFilter::ShowNone => "all",
        }
    }

    /// The synchronisation method, its options and the button that starts it.
    fn sync_bar(&mut self, ui: &mut egui::Ui) {
        let mut method = self.sync.method;
        let mut accept = false;
        let mut start = false;
        ui.horizontal_wrapped(|ui| {
            widgets::sized(ui, SYNC_COMBO_WIDTH, |ui| {
                egui::ComboBox::from_id_salt(self.id.with("sync-method"))
                    .width(SYNC_COMBO_WIDTH)
                    .selected_text(method.label())
                    .show_ui(ui, |ui| {
                        for option in sync_mode::PRESETS {
                            ui.selectable_value(&mut method, option, option.label());
                        }
                    });
            });
            ui.checkbox(
                &mut self.form.options.use_recycle_bin,
                "Use recycle bin if possible",
            );
            ui.checkbox(
                &mut self.form.options.preserve_modified,
                "Preserve timestamps",
            );
            if ui.button("Accept").clicked() {
                accept = true;
            }
            let pending = self.sync.pending();
            if widgets::with_toolbar_icon(ui, ca_ui::icons::Icon::Synchronize, true, |ui| {
                widgets::toolbar_button(
                    ui,
                    "Sync Now",
                    pending > 0 && matches!(self.stage, Stage::Idle),
                    "Build a preview with something in it first",
                )
            }) {
                start = true;
            }
            ui.label(format!("{pending} operations pending"));
            if self.sync.stale {
                ui.label("The preview is out of date.");
            }
        });
        if let Some(reason) = self.sync.refusal.clone() {
            ui.label(format!("This synchronisation cannot run: {reason}"));
        }
        if method != self.sync.method {
            self.sync.choose(method);
            self.refresh_preview();
        }
        if accept {
            self.refresh_preview();
        }
        if start {
            self.start_sync();
        }
    }

    /// One row per pair the method acts on, with the action it will receive.
    fn preview_grid(&mut self, ui: &mut egui::Ui) {
        let Some(preview) = self.sync.preview.as_ref() else {
            ui.label("No preview has been built yet.");
            return;
        };
        let rows: Vec<(PathBuf, String, NodeStatus, SyncAction)> = preview
            .rows
            .iter()
            .map(|row| {
                (
                    row.rel.clone(),
                    row.name.clone(),
                    row.status,
                    preview.effective(&row.rel, row.action),
                )
            })
            .collect();
        let mut change: Option<(PathBuf, SyncAction)> = None;
        let height = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
        egui::ScrollArea::vertical()
            .id_salt(self.id.with("preview"))
            .max_height(PREVIEW_HEIGHT)
            .show_rows(ui, height, rows.len(), |ui, range| {
                for index in range {
                    let Some((rel, name, status, action)) = rows.get(index) else {
                        continue;
                    };
                    ui.horizontal(|ui| {
                        sync_mode::action_icon(*action).show(ui, 16.0, ui.visuals().text_color());
                        ui.label(name.clone());
                        ca_ui::icons::node_status_icon(*status, None, false).show(
                            ui,
                            16.0,
                            ui.visuals().text_color(),
                        );
                        egui::ComboBox::from_id_salt(self.id.with(("preview-row", index)))
                            .selected_text(sync_mode::action_label(*action))
                            .show_ui(ui, |ui| {
                                for option in sync_mode::ACTIONS {
                                    if ui
                                        .selectable_label(
                                            option == *action,
                                            sync_mode::action_label(option),
                                        )
                                        .clicked()
                                    {
                                        change = Some((rel.clone(), option));
                                    }
                                }
                            });
                    });
                }
            });
        if let Some((rel, action)) = change {
            self.sync.set_override(rel, action);
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let mut toggle_errors = false;
        ui.horizontal_wrapped(|ui| {
            match &self.status {
                Status::Running(step) => {
                    ui.label(format!(
                        "{step}: {} left, {} right",
                        self.left_count, self.right_count
                    ));
                }
                Status::Failed(reason) => {
                    ca_ui::widgets::notice_current(
                        ui,
                        ca_ui::icons::Icon::Error,
                        16.0,
                        &format!("Failed: {reason}"),
                    );
                }
                Status::Cancelled => {
                    ca_ui::widgets::notice_current(
                        ui,
                        ca_ui::icons::Icon::Warning,
                        16.0,
                        "Stopped before finishing",
                    );
                }
                Status::Ready => {
                    let totals = self.arena.totals();
                    ui.label(format!(
                        "{} folders, {} files: {} same, {} differ, {} orphans, {} not compared",
                        totals.folders,
                        totals.files,
                        totals.same,
                        totals.differences,
                        totals.orphans,
                        totals.not_compared
                    ));
                }
            }
            ui.separator();
            ui.label(format!("{} rows shown", self.rows.len()));
            ui.separator();
            ui.label(format!("{} selected", self.selection.len()));
            if self.content_done > 0 {
                ui.separator();
                ui.label(format!("{} contents compared", self.content_done));
            }
            let unreadable = self.unreadable_folders();
            if unreadable > 0 || !self.scan_errors.is_empty() {
                ui.separator();
                let label = format!("{unreadable} folders unreadable");
                if ui.link(label).clicked() {
                    toggle_errors = true;
                }
            }
            let mismatches = self.arena.totals().kind_mismatches;
            if mismatches > 0 {
                ui.separator();
                ui.label(format!("{mismatches} file and folder clashes"));
            }
            if !self.notices.is_empty() {
                ui.separator();
                if ui
                    .link(format!("{} unfinished batches", self.notices.len()))
                    .clicked()
                {
                    self.show_recovery = true;
                }
            }
        });
        if toggle_errors {
            self.show_errors = !self.show_errors;
        }
    }

    /// Folders whose listing did not come back whole, counting the two roots.
    fn unreadable_folders(&self) -> usize {
        self.arena.totals().incomplete + usize::from(self.root_incomplete)
    }

    fn errors_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Unreadable entries");
            if ui.button("Close").clicked() {
                self.show_errors = false;
            }
        });
        if self.root_incomplete {
            ui.label("A base folder's own listing could not be read in full");
        }
        if self.scan_errors.is_empty() {
            ui.label("The scan recorded no per entry errors");
            return;
        }
        egui::ScrollArea::vertical()
            .id_salt(self.id.with("errors"))
            .max_height(180.0)
            .show_rows(
                ui,
                ui.text_style_height(&egui::TextStyle::Body),
                self.scan_errors.len(),
                |ui, range| {
                    for error in &self.scan_errors[range] {
                        let side = if error.right { "Right" } else { "Left" };
                        ca_ui::widgets::path_text(
                            ui,
                            &format!("{side}  {}  {}", error.path.display(), error.message),
                        );
                    }
                },
            );
    }

    /// The clickable column headings, mirrored on both sides.
    fn header(&mut self, ui: &mut egui::Ui, layout: &Layout, palette: &Palette) {
        let rect = egui::Rect::from_min_size(
            ui.available_rect_before_wrap().left_top(),
            egui::vec2(ui.available_width(), HEADER_HEIGHT),
        );
        ui.allocate_rect(rect, egui::Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, palette.folder_header_background);
        let mut clicked: Option<SortColumn> = None;
        let font = egui::FontId::proportional(12.0);
        for (index, pane) in [layout.left, layout.right].into_iter().enumerate() {
            for (column, span) in [
                (SortColumn::Name, pane.name),
                (SortColumn::Size, pane.size),
                (SortColumn::Modified, pane.modified),
            ] {
                let cell = egui::Rect::from_min_max(
                    egui::pos2(span.start, rect.top()),
                    egui::pos2(span.end, rect.bottom()),
                );
                let label = column_label(column);
                if self.sort.column == column {
                    let icon = match self.sort.direction {
                        tree::SortDirection::Ascending => ca_ui::icons::Icon::SortAscending,
                        tree::SortDirection::Descending => ca_ui::icons::Icon::SortDescending,
                    };
                    icon.paint_in_row(
                        &painter,
                        cell.right_center() - egui::vec2(10.0, 0.0),
                        HEADER_HEIGHT,
                        palette.folder_header_text,
                    );
                }
                painter.with_clip_rect(cell).text(
                    cell.left_center() + egui::vec2(4.0, 0.0),
                    egui::Align2::LEFT_CENTER,
                    label,
                    font.clone(),
                    palette.folder_header_text,
                );
                let response = ui.interact(
                    cell,
                    self.id.with(("head", index, column_label(column))),
                    egui::Sense::click(),
                );
                if response.clicked() {
                    clicked = Some(column);
                }
            }
        }
        self.column_grips(ui, rect, layout);
        if let Some(column) = clicked {
            self.set_sort(column);
        }
    }

    /// The draggable edges between the data columns.
    fn column_grips(&mut self, ui: &mut egui::Ui, rect: egui::Rect, layout: &Layout) {
        let pane = layout.left;
        for (name, edge, width) in [
            ("size", pane.size.start, self.size_width),
            ("modified", pane.modified.start, self.time_width),
        ] {
            let grip = egui::Rect::from_min_max(
                egui::pos2(edge - GRIP, rect.top()),
                egui::pos2(edge + GRIP, rect.bottom()),
            );
            let response = ui.interact(
                grip,
                self.id.with(("grip", name)),
                egui::Sense::click_and_drag(),
            );
            if response.hovered() || response.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
            let moved = -response.drag_delta().x;
            if moved.abs() > f32::EPSILON {
                let next = (width + moved).max(MINIMUM_COLUMN);
                if name == "size" {
                    self.size_width = next;
                } else {
                    self.time_width = next;
                }
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn tree_ui(&mut self, ui: &mut egui::Ui, palette: &Palette) -> Vec<ViewAction> {
        let full = ui.available_rect_before_wrap();
        let body = egui::Rect::from_min_max(
            full.left_top(),
            egui::pos2(full.right() - SCROLLBAR, full.bottom()),
        );
        let layout = Layout::new(body, self.size_width, self.time_width, self.split);
        ui.allocate_rect(full, egui::Sense::hover());
        self.claim_keyboard(ui, body);
        let painter = ui.painter_at(full);
        painter.rect_filled(body, 0.0, palette.folder_background);

        let total = self.rows.len();
        self.viewport_height = body.height();
        let row_height = self.row_height();
        self.scroll.clamp(body.height(), row_height, total);
        let range = self.scroll.visible(body.height(), row_height, total);
        let font = egui::FontId::proportional(self.font.points());

        let mut actions: Vec<ViewAction> = Vec::new();
        let mut toggle: Option<u32> = None;
        let mut click: Option<(Side, usize, ClickKind)> = None;
        let cursor = ui.ctx().pointer_latest_pos();
        let (clicked, double_clicked, modifiers) = ui.input(|input| {
            (
                input.pointer.primary_clicked(),
                input
                    .pointer
                    .button_double_clicked(egui::PointerButton::Primary),
                input.modifiers,
            )
        });
        let clip = painter.with_clip_rect(body);
        for position in range {
            let Some(row) = self.rows.get(position) else {
                continue;
            };
            let Some(node) = self.arena.node(row.node) else {
                continue;
            };
            let y = body.top() + self.scroll.row_y(position, self.row_height());
            let row_rect = egui::Rect::from_min_size(
                egui::pos2(body.left(), y),
                egui::vec2(body.width(), self.row_height()),
            );
            if position % 2 == 1 {
                clip.rect_filled(row_rect, 0.0, palette.stripe);
            }
            let color = palette.folder_row(theme::folder_class(node.status));
            let expanded = node.is_dir && self.expanded.is_expanded(row.node);
            for (side, pane, info) in [
                (Side::Left, layout.left, node.left.as_ref()),
                (Side::Right, layout.right, node.right.as_ref()),
            ] {
                if self.selection.contains(side, &node.rel) {
                    let pane_rect = egui::Rect::from_min_max(
                        egui::pos2(pane.name.start, y),
                        egui::pos2(pane.modified.end, y + self.row_height()),
                    );
                    clip.rect_filled(pane_rect, 0.0, palette.folder_selection);
                    clip.line_segment(
                        [pane_rect.left_bottom(), pane_rect.right_bottom()],
                        egui::Stroke::new(1.0, palette.folder_selection_edge),
                    );
                }
                paint_pane(
                    &clip,
                    &font,
                    row_height,
                    color,
                    y,
                    pane,
                    node,
                    row.depth,
                    expanded,
                    info,
                    palette,
                    self.offset_seconds,
                );
            }
            let Some(cursor) = cursor else {
                continue;
            };
            if !row_rect.contains(cursor) {
                continue;
            }
            if let Some(tip) = node_tooltip(node) {
                egui::show_tooltip_at(
                    ui.ctx(),
                    ui.layer_id(),
                    self.id.with("row-tip"),
                    row_rect.left_bottom(),
                    |ui| {
                        ui.label(tip);
                    },
                );
            }
            let side = if cursor.x >= layout.right.name.start {
                Side::Right
            } else {
                Side::Left
            };
            if node.is_dir {
                let pane = if side == Side::Right {
                    layout.right
                } else {
                    layout.left
                };
                let marker_end = pane.name.start + f32::from(row.depth) * INDENT + 14.0;
                if clicked && cursor.x <= marker_end {
                    toggle = Some(row.node);
                    continue;
                }
                if double_clicked {
                    toggle = Some(row.node);
                    continue;
                }
            } else if double_clicked && node.left.is_some() && node.right.is_some() {
                let request = pair_request(
                    &self.left_path,
                    &self.right_path,
                    &node.rel,
                    &self.engine_options().archives,
                );
                match self.sides.as_ref().filter(|sides| !sides.are_local()) {
                    Some(sides) => {
                        self.extract_job = Some(jobs::spawn_extract(
                            sides.clone(),
                            node.rel.clone(),
                            request,
                            self.notify.clone(),
                        ));
                    }
                    None => actions.push(ViewAction::Open(request)),
                }
            }
            if clicked {
                let kind = if modifiers.command {
                    ClickKind::Toggle
                } else if modifiers.shift {
                    ClickKind::Range
                } else {
                    ClickKind::Replace
                };
                click = Some((side, position, kind));
            }
        }

        self.splitter(ui, &painter, palette, body, &layout);
        self.vertical_scrollbar(ui, &painter, palette, full, body, total);

        if let Some((side, position, kind)) = click {
            let rows = std::mem::take(&mut self.rows);
            self.selection
                .click(side, position, &rows, &self.arena, kind);
            self.rows = rows;
        }
        if let Some(node) = toggle {
            self.expanded.toggle(node);
            self.rebuild();
        }
        if ui
            .ctx()
            .pointer_latest_pos()
            .is_some_and(|pointer| full.contains(pointer))
        {
            let delta = ui.input(|input| input.smooth_scroll_delta.y);
            if delta.abs() > f32::EPSILON {
                self.scroll
                    .scroll_by(-delta, body.height(), self.row_height(), total);
            }
        }
        actions
    }

    /// The draggable divider between the two panes.
    fn splitter(
        &mut self,
        ui: &mut egui::Ui,
        painter: &egui::Painter,
        palette: &Palette,
        body: egui::Rect,
        layout: &Layout,
    ) {
        let rect = egui::Rect::from_min_max(
            egui::pos2(layout.splitter.start, body.top()),
            egui::pos2(layout.splitter.end, body.bottom()),
        );
        painter.rect_filled(rect, 0.0, palette.folder_header_background);
        let response = ui.interact(
            rect,
            self.id.with("splitter"),
            egui::Sense::click_and_drag(),
        );
        if response.hovered() || response.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        let moved = response.drag_delta().x;
        if moved.abs() > f32::EPSILON && body.width() > 0.0 {
            self.split =
                (self.split + moved / body.width()).clamp(MINIMUM_SPLIT, 1.0 - MINIMUM_SPLIT);
        }
    }

    fn vertical_scrollbar(
        &mut self,
        ui: &mut egui::Ui,
        painter: &egui::Painter,
        palette: &Palette,
        full: egui::Rect,
        body: egui::Rect,
        total: usize,
    ) {
        let track = egui::Rect::from_min_max(
            egui::pos2(full.right() - SCROLLBAR, body.top()),
            egui::pos2(full.right(), body.bottom()),
        );
        painter.rect_filled(track, 0.0, palette.gutter_background);
        let thumb = self
            .scroll
            .thumb(track.height(), body.height(), self.row_height(), total);
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(track.left() + 2.0, track.top() + thumb.start),
                egui::pos2(track.right() - 2.0, track.top() + thumb.end),
            ),
            2.0,
            palette.thumbnail_marker,
        );
        let response = ui.interact(
            track,
            self.id.with("tree-bar"),
            egui::Sense::click_and_drag(),
        );
        if let Some(pointer) = response.interact_pointer_pos() {
            let top = pointer.y - track.top() - (thumb.end - thumb.start) / 2.0;
            self.scroll =
                RowScroll::from_thumb(top, track.height(), body.height(), self.row_height(), total);
        }
    }

    /// True while no dialog of the view is on screen, so the tree may hold
    /// the keyboard.
    fn tree_takes_keys(&self) -> bool {
        let recovery_shown = self.show_recovery && !self.notices.is_empty();
        !self.stage.blocks_input() && self.message.is_none() && !recovery_shown
    }

    /// Register the tree as a widget that holds the keyboard.
    ///
    /// The tree takes the keyboard on a press inside it, and whenever no
    /// other widget holds it and no dialog is open. While it holds it, Tab and
    /// the arrow keys stay with it: egui would otherwise read Tab, pressed with
    /// no widget holding the keyboard, as a move to the first path field.
    fn claim_keyboard(&self, ui: &egui::Ui, body: egui::Rect) {
        let id = self.tree_widget();
        let response = ui.interact(body, id, egui::Sense::click());
        if !self.tree_takes_keys() {
            ui.memory_mut(|memory| memory.surrender_focus(id));
            return;
        }
        let pressed = response.hovered() && ui.input(|input| input.pointer.primary_pressed());
        if pressed || ui.memory(|memory| memory.focused().is_none()) {
            response.request_focus();
        }
        if response.has_focus() {
            ui.memory_mut(|memory| {
                memory.set_focus_lock_filter(
                    id,
                    egui::EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: false,
                    },
                );
            });
        }
    }

    /// The keystrokes this view routes itself.
    fn keyboard(&mut self, ui: &egui::Ui) {
        // A text field that holds the keyboard reads the same key events
        // without consuming them, so an arrow typed into the name filter
        // would also move the tree cursor.
        let focused = ui.ctx().memory(egui::Memory::focused);
        if self.stage.blocks_input() || focused.is_some_and(|id| id != self.tree_widget()) {
            return;
        }
        let (pressed, modifiers) = ui.input(|input| {
            let mut pressed = Vec::new();
            // The file operation keys are routed by the shell, which reaches
            // this view through its command declaration. Handling them again
            // here would start each operation twice.
            for key in [
                egui::Key::ArrowUp,
                egui::Key::ArrowDown,
                egui::Key::ArrowLeft,
                egui::Key::ArrowRight,
                egui::Key::Tab,
            ] {
                if input.key_pressed(key) {
                    pressed.push(key);
                }
            }
            (pressed, input.modifiers)
        });
        for key in pressed {
            match key {
                egui::Key::ArrowUp => {
                    let rows = std::mem::take(&mut self.rows);
                    self.selection
                        .move_cursor(-1, modifiers.shift, &rows, &self.arena);
                    self.rows = rows;
                    self.reveal_cursor();
                }
                egui::Key::ArrowDown => {
                    let rows = std::mem::take(&mut self.rows);
                    self.selection
                        .move_cursor(1, modifiers.shift, &rows, &self.arena);
                    self.rows = rows;
                    self.reveal_cursor();
                }
                egui::Key::ArrowLeft if modifiers.shift => {
                    self.selection.restrict_to(Side::Left);
                }
                egui::Key::ArrowRight if modifiers.shift => {
                    self.selection.restrict_to(Side::Right);
                }
                egui::Key::Tab => {
                    if let Some((side, rel)) = self.selection.cursor() {
                        let rel = rel.to_path_buf();
                        self.selection.set_cursor(opposite(side), rel);
                    }
                }
                _ => {}
            }
        }
    }

    /// Bring the keyboard row into view.
    fn reveal_cursor(&mut self) {
        let Some(position) = self.selection.cursor_position(&self.rows, &self.arena) else {
            return;
        };
        self.scroll.reveal(
            position,
            self.viewport_height,
            self.row_height(),
            self.rows.len(),
        );
    }

    /// The windows that stand between a command and the disk.
    #[allow(clippy::too_many_lines)]
    fn overlays(&mut self, ui: &mut egui::Ui) {
        if let Some((title, body)) = self.message.clone() {
            if dialogs::notice(ui, self.id.with("notice"), &title, &body) {
                self.message = None;
            }
        }
        if self.show_recovery && !self.notices.is_empty() {
            match dialogs::recovery(ui, self.id.with("recovery"), &self.notices) {
                Some(RecoveryChoice::CleanUp(journal)) => {
                    self.cleanup_job = Some(opjobs::spawn_cleanup(journal, self.notify.clone()));
                }
                Some(RecoveryChoice::Dismiss) => self.show_recovery = false,
                None => {}
            }
        }
        if let Some(question) = self.pending_question() {
            if let Some(answer) = dialogs::question(
                ui,
                self.id.with(("question", question.sequence)),
                &question.question,
            ) {
                self.answer(&question, answer);
            }
        }
        let stage = std::mem::replace(&mut self.stage, Stage::Idle);
        self.stage = match stage {
            Stage::Confirming(shown) => {
                let offset = self.offset_seconds;
                let choice = dialogs::confirm(
                    ui,
                    self.id.with("confirm"),
                    shown.operation,
                    &shown.plan,
                    &mut self.form,
                    offset,
                    shown.revised,
                );
                match choice {
                    ConfirmChoice::Pending => Stage::Confirming(shown),
                    ConfirmChoice::Confirm => {
                        self.confirm(*shown);
                        return;
                    }
                    ConfirmChoice::Cancel => Stage::Idle,
                }
            }
            Stage::Running(plan) => {
                let snapshot = self
                    .progress
                    .lock()
                    .map(|held| held.clone())
                    .unwrap_or_default();
                if dialogs::progress(ui, self.id.with("progress"), &snapshot) {
                    if let Some(cancel) = self.exec_cancel.as_ref() {
                        cancel.cancel();
                    }
                }
                Stage::Running(plan)
            }
            Stage::Summary(plan, report) => {
                if dialogs::summary(ui, self.id.with("summary"), &plan, &report) {
                    Stage::Idle
                } else {
                    Stage::Summary(plan, report)
                }
            }
            other => other,
        };
    }
}

/// Scan the journal directory for unfinished batches.
///
/// Exposed so a launcher can raise the same notice without opening a
/// comparison first.
#[must_use]
pub fn recovery_scan(notify: Arc<dyn Fn() + Send + Sync>) -> Job<RecoverMessage> {
    recovery_scan_in(ca_ui::paths::journal_directory(), notify)
}

/// Scan a stated journal directory for unfinished batches.
#[must_use]
pub fn recovery_scan_in(
    journal_directory: PathBuf,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<RecoverMessage> {
    opjobs::spawn_recovery(journal_directory, notify)
}

/// The other side.
const fn opposite(side: Side) -> Side {
    match side {
        Side::Left => Side::Right,
        Side::Right => Side::Left,
    }
}

/// A column's left and right edge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Span {
    /// Left edge.
    pub start: f32,
    /// Right edge.
    pub end: f32,
}

impl Span {
    const fn new(start: f32, end: f32) -> Self {
        Self { start, end }
    }

    /// How wide the column is.
    #[must_use]
    pub fn width(self) -> f32 {
        self.end - self.start
    }
}

/// Where one pane's three columns sit.
#[derive(Debug, Clone, Copy)]
pub struct Pane {
    /// The name column, which carries the tree indentation and the status
    /// glyph.
    pub name: Span,
    /// The size column.
    pub size: Span,
    /// The modified column.
    pub modified: Span,
}

impl Pane {
    fn width(self) -> f32 {
        self.modified.end - self.name.start
    }
}

/// The two panes and the splitter between them.
#[derive(Debug, Clone, Copy)]
pub struct Layout {
    /// The left pane.
    pub left: Pane,
    /// The splitter.
    pub splitter: Span,
    /// The right pane.
    pub right: Pane,
}

impl Layout {
    /// The name column never shrinks below this, whatever the data columns ask
    /// for.
    const NAME_MINIMUM: f32 = 60.0;

    /// Split `rect` into two panes around a draggable splitter.
    ///
    /// `split` is the share of the room the left pane takes.
    #[must_use]
    pub fn new(rect: egui::Rect, size_width: f32, time_width: f32, split: f32) -> Self {
        let room = (rect.width() - SPLITTER_WIDTH).max(0.0);
        let split = split.clamp(MINIMUM_SPLIT, 1.0 - MINIMUM_SPLIT);
        let left_width = room * split;
        let right_width = room - left_width;
        let pane_at = |start: f32, width: f32| {
            let data = (size_width.max(MINIMUM_COLUMN) + time_width.max(MINIMUM_COLUMN))
                .min((width - Self::NAME_MINIMUM).max(0.0));
            let size_share = if data <= 0.0 {
                0.0
            } else {
                data * size_width.max(MINIMUM_COLUMN)
                    / (size_width.max(MINIMUM_COLUMN) + time_width.max(MINIMUM_COLUMN))
            };
            let name_width = (width - data).max(0.0);
            Pane {
                name: Span::new(start, start + name_width),
                size: Span::new(start + name_width, start + name_width + size_share),
                modified: Span::new(start + name_width + size_share, start + width),
            }
        };
        let left_start = rect.left();
        let splitter_start = left_start + left_width;
        let right_start = splitter_start + SPLITTER_WIDTH;
        Self {
            left: pane_at(left_start, left_width),
            splitter: Span::new(splitter_start, right_start),
            right: pane_at(right_start, right_width),
        }
    }
}

/// Paint one node in one pane.
///
/// A side the node is missing from paints nothing, which is what makes an
/// orphan read as a blank row opposite its own entry. The status is carried by
/// the glyph in front of the name and by the text color, so no column between
/// the panes is needed.
#[allow(clippy::too_many_arguments)]
fn paint_pane(
    painter: &egui::Painter,
    font: &egui::FontId,
    row_height: f32,
    color: egui::Color32,
    y: f32,
    pane: Pane,
    node: &ArenaNode,
    depth: u16,
    expanded: bool,
    side: Option<&tree::SideInfo>,
    palette: &Palette,
    offset_seconds: i32,
) {
    let Some(side) = side else {
        return;
    };
    let indent = f32::from(depth) * INDENT;
    let clip = painter.with_clip_rect(egui::Rect::from_min_max(
        egui::pos2(pane.name.start, y),
        egui::pos2(pane.name.end, y + row_height),
    ));
    let middle = y + row_height / 2.0;
    if node.is_dir {
        let icon = if expanded {
            ca_ui::icons::Icon::ChevronDown
        } else {
            ca_ui::icons::Icon::ChevronRight
        };
        icon.paint_in_row(
            &clip,
            egui::pos2(pane.name.start + indent + 8.0, middle),
            row_height,
            color,
        );
    }
    ca_ui::icons::node_status_icon(node.status, node.content, node.incomplete).paint_in_row(
        &clip,
        egui::pos2(pane.name.start + indent + 26.0, middle),
        row_height,
        color,
    );
    let item_icon = if node.is_dir {
        if expanded {
            ca_ui::icons::Icon::FolderOpen
        } else {
            ca_ui::icons::Icon::Folder
        }
    } else {
        ca_ui::icons::Icon::File
    };
    item_icon.paint_in_row(
        &clip,
        egui::pos2(pane.name.start + indent + 44.0, middle),
        row_height,
        color,
    );
    clip.text(
        egui::pos2(pane.name.start + indent + 56.0, middle),
        egui::Align2::LEFT_CENTER,
        &node.name,
        font.clone(),
        color,
    );
    if node.incomplete {
        ca_ui::icons::Icon::StateIncomplete.paint_in_row(
            &clip,
            egui::pos2(pane.name.end - 10.0, middle),
            row_height,
            palette.folder_row(theme::FolderClass::Unknown),
        );
    }
    if pane.width() <= 0.0 {
        return;
    }
    let data = painter.with_clip_rect(egui::Rect::from_min_max(
        egui::pos2(pane.size.start, y),
        egui::pos2(pane.modified.end, y + row_height),
    ));
    if !node.is_dir {
        data.text(
            egui::pos2(pane.size.end - 4.0, y + row_height / 2.0),
            egui::Align2::RIGHT_CENTER,
            ca_ui::format::format_bytes(side.size),
            font.clone(),
            color,
        );
    }
    data.text(
        egui::pos2(pane.modified.start + 4.0, y + row_height / 2.0),
        egui::Align2::LEFT_CENTER,
        ca_ui::format::format_stamp(side.modified, offset_seconds),
        font.clone(),
        color,
    );
}

/// Marks an entry whose listing did not come back whole.
pub const INCOMPLETE_GLYPH: &str = "\u{2026}";

/// What a row says when the pointer rests on it, where it has something to say.
#[must_use]
pub fn node_tooltip(node: &ArenaNode) -> Option<&'static str> {
    if node.status == NodeStatus::KindMismatch {
        return Some("A file on one side and a folder on the other");
    }
    if node.incomplete {
        return Some(
            "This listing was not read in full: unreadable, stopped early, or not stored locally",
        );
    }
    if node.content == Some(ContentOutcome::NotComparedCloudPlaceholder) {
        return Some("Contents were not read: the file is not stored locally");
    }
    None
}

/// The glyph shown in front of a name for a status.
#[must_use]
pub const fn status_glyph(
    status: NodeStatus,
    content: Option<ContentOutcome>,
    incomplete: bool,
) -> &'static str {
    if matches!(status, NodeStatus::KindMismatch) {
        return "\u{2260}";
    }
    if incomplete {
        return INCOMPLETE_GLYPH;
    }
    if let Some(outcome) = content {
        return match outcome {
            ContentOutcome::BinarySame | ContentOutcome::RulesSame => "=",
            ContentOutcome::UnimportantDifferences => "~",
            ContentOutcome::BinaryDifferences | ContentOutcome::ImportantDifferences => "X",
            ContentOutcome::NotComparedCloudPlaceholder => "?",
        };
    }
    match status {
        NodeStatus::Same => "=",
        NodeStatus::Different => "X",
        NodeStatus::LeftNewer => "<",
        NodeStatus::RightNewer => ">",
        NodeStatus::LeftOrphan => "-",
        NodeStatus::RightOrphan => "+",
        NodeStatus::Error => "!",
        NodeStatus::NotCompared | NodeStatus::KindMismatch => "?",
    }
}

/// The heading of a sortable column.
#[must_use]
pub const fn column_label(column: SortColumn) -> &'static str {
    match column {
        SortColumn::Name => "Name",
        SortColumn::Size => "Size",
        SortColumn::Modified => "Modified",
    }
}

/// The display filters offered, in menu order.
/// What one side of a folder entry holds, as a report reads it.
fn side_facts(info: Option<&crate::tree::SideInfo>, is_dir: bool) -> ca_ui::report::SideFacts {
    match info {
        None => ca_ui::report::SideFacts::absent(),
        Some(side) => ca_ui::report::SideFacts {
            present: true,
            size: (!is_dir).then_some(side.size),
            timestamp: side.modified.map(ca_ui::report::format_timestamp),
            ..ca_ui::report::SideFacts::default()
        },
    }
}

/// Every display filter the toolbar offers, in the order it lists them.
pub const DISPLAY_FILTERS: [DisplayFilter; 13] = [
    DisplayFilter::ShowAll,
    DisplayFilter::ShowDifferences,
    DisplayFilter::ShowSame,
    DisplayFilter::ShowNoOrphans,
    DisplayFilter::ShowDifferencesNoOrphans,
    DisplayFilter::ShowOrphans,
    DisplayFilter::ShowLeftNewer,
    DisplayFilter::ShowRightNewer,
    DisplayFilter::ShowLeftNewerAndLeftOrphans,
    DisplayFilter::ShowRightNewerAndRightOrphans,
    DisplayFilter::ShowLeftOrphans,
    DisplayFilter::ShowRightOrphans,
    DisplayFilter::ShowNone,
];

/// The label for a display filter.
#[must_use]
pub const fn filter_label(filter: DisplayFilter) -> &'static str {
    match filter {
        DisplayFilter::ShowAll => "Show All",
        DisplayFilter::ShowDifferences => "Show Differences",
        DisplayFilter::ShowSame => "Show Same",
        DisplayFilter::ShowNoOrphans => "Show No Orphans",
        DisplayFilter::ShowDifferencesNoOrphans => "Show Differences but No Orphans",
        DisplayFilter::ShowOrphans => "Show Orphans",
        DisplayFilter::ShowLeftNewer => "Show Left Newer",
        DisplayFilter::ShowRightNewer => "Show Right Newer",
        DisplayFilter::ShowLeftNewerAndLeftOrphans => "Show Left Newer and Left Orphans",
        DisplayFilter::ShowRightNewerAndRightOrphans => "Show Right Newer and Right Orphans",
        DisplayFilter::ShowLeftOrphans => "Show Left Orphans",
        DisplayFilter::ShowRightOrphans => "Show Right Orphans",
        DisplayFilter::ShowNone => "Show None",
    }
}

const fn folder_filter_label(filter: FolderDisplayFilter) -> &'static str {
    match filter {
        FolderDisplayFilter::AlwaysShowFolders => "Always Show Folders",
        FolderDisplayFilter::CompareFilesAndFolderStructure => "Compare Files and Folder Structure",
        FolderDisplayFilter::OnlyCompareFiles => "Only Compare Files",
    }
}

const fn method_label(method: ContentMethod) -> &'static str {
    match method {
        ContentMethod::Binary => "Binary",
        ContentMethod::Crc32 => "CRC",
        ContentMethod::Rules => "Rules-based",
    }
}

fn name_filter_id(view: egui::Id) -> egui::Id {
    view.with("name-filter")
}

/// The tab a double click on a pair of files opens.
///
/// A pair of containers the handling opens as folders becomes a folder
/// comparison of the two; any other pair goes to the shell, which decides
/// which view answers for a file comparison. The name decides, so no file is
/// read on the frame thread. A side that is not a local folder has no path
/// for the entry, so the caller copies the entry out before it opens this.
fn pair_request(
    left_base: &Path,
    right_base: &Path,
    rel: &Path,
    archives: &ca_fs::ArchiveTypes,
) -> OpenRequest {
    let name = rel
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    let opens_as_folder = archives.handling() != ca_fs::ArchiveHandling::AsFiles
        && archives
            .format_for_name(&name)
            .is_some_and(ca_fs::ArchiveFormat::is_supported);
    let kind = if opens_as_folder {
        SessionKind::FolderCompare
    } else {
        SessionKind::TextCompare
    };
    OpenRequest::new(kind, left_base.join(rel), right_base.join(rel))
}

fn folder_name(path: &Path) -> String {
    match path.file_name() {
        Some(name) => name.to_string_lossy().into_owned(),
        None => path.display().to_string(),
    }
}

impl ca_ui::view::ViewFactory for FolderView {
    fn create(left: PathBuf, right: PathBuf, context: &ViewContext, instance: u64) -> Self {
        Self::new(left, right, context, instance)
    }
}

/// A folder synchronisation the registry can build like any other view.
pub struct FolderSyncView {
    view: FolderView,
    /// The settings as last applied. The folder groups and the method are
    /// read back from the view, so a toolbar change reaches the dialog.
    applied: ca_session::settings::FolderSyncSettings,
    /// The count of method choices the view had made when `applied` named a
    /// method this build does not know. While the count stands, the stored
    /// method is reported unchanged.
    unknown_method_at: Option<u64>,
}

impl ca_ui::view::ViewFactory for FolderSyncView {
    fn create(left: PathBuf, right: PathBuf, context: &ViewContext, instance: u64) -> Self {
        Self::from(FolderView::new_sync(left, right, context, instance))
    }
}

impl From<FolderView> for FolderSyncView {
    fn from(view: FolderView) -> Self {
        Self {
            view,
            applied: ca_session::settings::FolderSyncSettings::default(),
            unknown_method_at: None,
        }
    }
}

impl FolderSyncView {
    /// The folder view behind this tab.
    pub fn view_mut(&mut self) -> &mut FolderView {
        &mut self.view
    }
}

impl SessionView for FolderSyncView {
    fn kind(&self) -> Option<ca_session::SessionKind> {
        Some(ca_session::SessionKind::FolderSync)
    }

    fn menu_view(&self) -> ca_ui::command::MenuView {
        self.view.menu_view()
    }

    fn title(&self) -> String {
        self.view.title()
    }

    fn tick(&mut self) {
        self.view.tick();
    }

    fn ui(&mut self, ui: &mut egui::Ui, context: &ViewContext) -> Vec<ViewAction> {
        self.view.ui(ui, context)
    }

    fn commands(&self) -> Vec<ca_ui::view::CommandState> {
        let mut declared = self.view.commands();
        declared.extend(view::declare(SYNC_HANDLED, |command| self.accepts(command)));
        declared
    }

    fn accepts(&self, command: Command) -> bool {
        match command {
            Command::Synchronize => {
                self.view.sync_refusal().is_none() && self.view.sync_pending() > 0
            }
            other => self.view.accepts(other),
        }
    }

    fn run(&mut self, command: Command) {
        match command {
            Command::Synchronize => self.view.start_synchronisation(),
            other => self.view.run(other),
        }
    }

    fn apply_settings(&mut self, settings: &ca_session::settings::SessionSettings) {
        let ca_session::settings::SessionSettings::FolderSync(sync) = settings else {
            return;
        };
        self.applied = sync.clone();
        let folder = crate::settings::compare_settings_of(sync, &self.view.session_settings());
        self.view.apply_session_settings(&folder);
        match crate::settings::sync_method(&sync.sync.method) {
            Some(method) => {
                self.unknown_method_at = None;
                if method != self.view.sync().method {
                    self.view.set_sync_method(method);
                }
            }
            None => self.unknown_method_at = Some(self.view.sync().choices),
        }
    }

    fn settings(&self) -> Option<ca_session::settings::SessionSettings> {
        let chosen = (self.unknown_method_at != Some(self.view.sync().choices))
            .then_some(self.view.sync().method);
        Some(ca_session::settings::SessionSettings::FolderSync(
            crate::settings::sync_settings_of(self.view.session_settings(), chosen, &self.applied),
        ))
    }

    fn is_ready(&self) -> bool {
        SessionView::is_ready(&self.view)
    }

    fn notice(&self) -> Option<String> {
        self.view.notice()
    }

    fn wants_close(&self) -> bool {
        self.view.wants_close()
    }

    fn on_close(&mut self) {
        self.view.on_close();
    }

    fn holds_temporaries(&self) -> bool {
        self.view.holds_temporaries()
    }

    fn may_close(&mut self) -> bool {
        self.view.may_close()
    }

    fn is_busy(&self) -> bool {
        self.view.is_busy()
    }

    fn exit_code(&self) -> Option<i32> {
        self.view.exit_code()
    }

    fn is_launcher(&self) -> bool {
        self.view.is_launcher()
    }

    fn launch_target(&self) -> Option<ca_ui::launch::LaunchTarget> {
        self.view.launch_target()
    }

    fn explorer_target(&self) -> Option<(PathBuf, ca_ui::launch::Selection)> {
        self.view.explorer_target()
    }
}

impl SessionView for FolderView {
    fn kind(&self) -> Option<ca_session::SessionKind> {
        Some(ca_session::SessionKind::FolderCompare)
    }

    fn menu_view(&self) -> ca_ui::command::MenuView {
        ca_ui::command::MenuView::Folder
    }

    fn apply_settings(&mut self, settings: &ca_session::settings::SessionSettings) {
        if let ca_session::settings::SessionSettings::FolderCompare(folder) = settings {
            self.apply_session_settings(folder);
        }
    }

    fn settings(&self) -> Option<ca_session::settings::SessionSettings> {
        Some(ca_session::settings::SessionSettings::FolderCompare(
            self.session_settings(),
        ))
    }

    fn title(&self) -> String {
        let left = folder_name(&self.left_path);
        let right = folder_name(&self.right_path);
        match self.mode {
            Mode::Compare => format!("{left} - {right}"),
            Mode::Sync => format!("{left} - {right} (sync)"),
        }
    }

    fn tick(&mut self) {
        self.poll();
    }

    fn ui(&mut self, ui: &mut egui::Ui, context: &ViewContext) -> Vec<ViewAction> {
        let options = ca_ui::options::current(ui.ctx());
        self.confirmations = options.stored.file_operations.clone();
        if self.backups.as_ref() != Some(&options.stored.backups) {
            self.form.follow_backups(&options.stored.backups);
            self.backups = Some(options.stored.backups.clone());
        }
        self.apply_archive_masks(&options.stored.archives);
        self.font.follow(options.folder_row_point_size());
        self.line_spacing = options.extra_line_spacing();
        let filter_name = self.report_filter();
        self.report.poll_with(ui.ctx(), |settings| {
            settings.select_filter(filter_name);
        });
        egui::TopBottomPanel::top(self.id.with("head")).show_inside(ui, |ui| {
            self.path_bar(ui);
            self.selection_bar(ui);
            self.toolbar(ui);
            self.report_panel(ui);
            if self.mode == Mode::Sync {
                self.sync_bar(ui);
            }
        });
        egui::TopBottomPanel::bottom(self.id.with("status")).show_inside(ui, |ui| {
            self.status_bar(ui);
        });
        if self.mode == Mode::Sync {
            egui::TopBottomPanel::bottom(self.id.with("preview-panel")).show_inside(ui, |ui| {
                self.preview_grid(ui);
            });
        }
        if self.show_errors {
            egui::TopBottomPanel::bottom(self.id.with("errors-panel")).show_inside(ui, |ui| {
                self.errors_panel(ui);
            });
        }
        let mut actions = Vec::new();
        egui::CentralPanel::default().show_inside(ui, |ui| {
            let layout = Layout::new(
                ui.available_rect_before_wrap(),
                self.size_width,
                self.time_width,
                self.split,
            );
            self.header(ui, &layout, &context.palette);
            actions = self.tree_ui(ui, &context.palette);
        });
        actions.extend(self.take_pending_opens().into_iter().map(ViewAction::Open));
        self.keyboard(ui);
        self.overlays(ui);
        actions
    }

    fn commands(&self) -> Vec<ca_ui::view::CommandState> {
        view::declare(HANDLED, |command| self.accepts(command))
    }

    fn accepts(&self, command: Command) -> bool {
        let idle = matches!(self.stage, Stage::Idle);
        match command {
            Command::ExpandAll | Command::CollapseAll => !self.arena.is_empty(),
            Command::CompareContents | Command::CompareReport => self.compared_tree.is_some(),
            Command::SwapSides | Command::Reload => self.status == Status::Ready && idle,
            Command::Cancel => self.status.is_running() || self.stage.is_busy(),
            Command::ClearSelection => !self.selection.is_empty(),
            Command::CopyToRight | Command::CopyToLeft => idle && !self.selection.is_empty(),
            other => {
                if select_rule_of(other).is_some() {
                    return !self.rows.is_empty();
                }
                match operation_of(other) {
                    Some(operation) => {
                        idle && self.status == Status::Ready
                            && (!operation.needs_selection() || !self.selection.is_empty())
                    }
                    None => false,
                }
            }
        }
    }

    fn run(&mut self, command: Command) {
        match command {
            Command::ExpandAll => {
                self.expanded.expand_all(&self.arena);
                self.rebuild();
            }
            Command::CollapseAll => {
                self.expanded.collapse_all();
                self.rebuild();
            }
            Command::CompareContents => self.start_content_comparison(),
            Command::CompareReport => self.report.request(),
            Command::Cancel => self.request_stop(),
            Command::Reload => self.restart(),
            Command::ClearSelection => self.selection.clear(),
            Command::CopyToRight => self.copy_to(Side::Right),
            Command::CopyToLeft => self.copy_to(Side::Left),
            Command::SwapSides => {
                std::mem::swap(&mut self.left_path, &mut self.right_path);
                std::mem::swap(&mut self.left_field, &mut self.right_field);
                self.selection.clear();
                self.restart();
            }
            other => {
                if let Some(rule) = select_rule_of(other) {
                    let rows = std::mem::take(&mut self.rows);
                    self.selection
                        .select_rule(rule, Scope::Both, &rows, &self.arena);
                    self.rows = rows;
                } else if let Some(operation) = operation_of(other) {
                    self.start_operation(operation);
                }
            }
        }
    }

    fn is_ready(&self) -> bool {
        !self.status.is_running()
    }

    fn on_close(&mut self) {
        self.drop_jobs();
        self.request_stop();
        if let Some(job) = self.picker.take() {
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

    /// The item under the cursor, with the base folder it sits under.
    ///
    /// A folder comparison names an item on one side, so the second side is
    /// filled only where the other side holds the same relative path.
    fn launch_target(&self) -> Option<ca_ui::launch::LaunchTarget> {
        use ca_session::options::{LaunchContext, LaunchSide};
        let (side, rel) = self.selection.cursor()?;
        let node = self.arena.node(self.arena.index_of(rel)?)?;
        let (near, far) = match side {
            Side::Left => (&self.left_path, &self.right_path),
            Side::Right => (&self.right_path, &self.left_path),
        };
        let held = |base: &std::path::Path| LaunchSide {
            path: base.join(rel),
            base: Some(base.to_path_buf()),
            line: None,
        };
        let both = node.left.is_some() && node.right.is_some();
        let context = LaunchContext {
            first: held(near),
            second: both.then(|| held(far)),
        };
        Some(if node.is_dir {
            ca_ui::launch::LaunchTarget::folders(context)
        } else {
            ca_ui::launch::LaunchTarget::files(context)
        })
    }

    fn explorer_target(&self) -> Option<(PathBuf, ca_ui::launch::Selection)> {
        let (side, rel) = self.selection.cursor()?;
        let node = self.arena.node(self.arena.index_of(rel)?)?;
        let side_has_item = match side {
            Side::Left => node.left.is_some(),
            Side::Right => node.right.is_some(),
        };
        if !side_has_item {
            return None;
        }
        let sources = self.sides.as_ref()?;
        let source = match side {
            Side::Left => &sources.left,
            Side::Right => &sources.right,
        };
        if !source.is_local_folder() {
            return None;
        }
        let target = self.launch_target()?;
        Some((target.context.first.path, target.selection))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{
        column_label, filter_label, folder_name, node_tooltip, pair_request, status_glyph, Layout,
        DISPLAY_FILTERS, INCOMPLETE_GLYPH, MINIMUM_COLUMN,
    };
    use crate::tree::{ArenaNode, SortColumn};
    use ca_fs::{ContentOutcome, NodeStatus};
    use std::path::{Path, PathBuf};

    fn rect(width: f32) -> egui::Rect {
        egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 400.0))
    }

    #[test]
    fn every_display_filter_has_a_label() {
        for filter in DISPLAY_FILTERS {
            assert!(!filter_label(filter).is_empty());
        }
        assert_eq!(DISPLAY_FILTERS.len(), 13);
    }

    #[test]
    fn every_sortable_column_has_a_heading() {
        for column in [SortColumn::Name, SortColumn::Size, SortColumn::Modified] {
            assert!(!column_label(column).is_empty());
        }
    }

    #[test]
    fn a_content_result_outranks_the_listing_status() {
        assert_eq!(
            status_glyph(
                NodeStatus::Different,
                Some(ContentOutcome::BinarySame),
                false
            ),
            "="
        );
        assert_eq!(status_glyph(NodeStatus::Different, None, false), "X");
        assert_eq!(status_glyph(NodeStatus::LeftOrphan, None, false), "-");
        assert_eq!(status_glyph(NodeStatus::RightOrphan, None, false), "+");
    }

    #[test]
    fn an_incomplete_listing_has_its_own_glyph() {
        assert_eq!(status_glyph(NodeStatus::Same, None, true), INCOMPLETE_GLYPH);
        assert_eq!(
            status_glyph(NodeStatus::KindMismatch, None, true),
            "\u{2260}"
        );
    }

    fn node(status: NodeStatus, incomplete: bool) -> ArenaNode {
        ArenaNode {
            name: "thing".to_string(),
            rel: PathBuf::from("thing"),
            is_dir: true,
            status,
            left: None,
            right: None,
            content: None,
            children: Vec::new(),
            depth: 0,
            incomplete,
            own_error: false,
        }
    }

    #[test]
    fn a_kind_clash_and_an_incomplete_listing_each_explain_themselves() {
        assert!(node_tooltip(&node(NodeStatus::KindMismatch, false)).is_some());
        assert!(node_tooltip(&node(NodeStatus::Same, true)).is_some());
        assert_ne!(
            node_tooltip(&node(NodeStatus::KindMismatch, false)),
            node_tooltip(&node(NodeStatus::Same, true))
        );
        assert!(node_tooltip(&node(NodeStatus::Same, false)).is_none());
    }

    #[test]
    fn a_cloud_placeholder_says_why_its_contents_were_not_read() {
        let mut placeholder = node(NodeStatus::NotCompared, false);
        placeholder.content = Some(ContentOutcome::NotComparedCloudPlaceholder);
        assert!(node_tooltip(&placeholder).is_some());
    }

    #[test]
    fn the_two_panes_sit_either_side_of_the_splitter() {
        for width in [200.0_f32, 640.0, 1_280.0, 3_000.0] {
            let layout = Layout::new(rect(width), 90.0, 160.0, 0.5);
            assert!(layout.left.name.start <= layout.left.size.start);
            assert!(layout.left.size.start <= layout.left.modified.start);
            assert!(layout.left.modified.end <= layout.splitter.start);
            assert!(layout.splitter.end <= layout.right.name.start);
            let left_width = layout.left.modified.end - layout.left.name.start;
            let right_width = layout.right.modified.end - layout.right.name.start;
            assert!(
                (left_width - right_width).abs() < 0.01,
                "panes differ at width {width}"
            );
        }
    }

    #[test]
    fn dragging_the_splitter_moves_the_share_each_pane_takes() {
        let layout = Layout::new(rect(1_000.0), 90.0, 160.0, 0.25);
        let left_width = layout.left.modified.end - layout.left.name.start;
        let right_width = layout.right.modified.end - layout.right.name.start;
        assert!(left_width < right_width);
        // A share outside the limits is clamped rather than collapsing a pane.
        let extreme = Layout::new(rect(1_000.0), 90.0, 160.0, 0.0);
        assert!(extreme.left.modified.end - extreme.left.name.start > 0.0);
    }

    #[test]
    fn a_pane_keeps_room_for_the_name_however_wide_the_data_columns_ask_to_be() {
        let layout = Layout::new(rect(400.0), 5_000.0, 5_000.0, 0.5);
        assert!(layout.left.name.end - layout.left.name.start >= 59.0);
    }

    #[test]
    fn a_column_dragged_narrow_stops_at_its_minimum() {
        let layout = Layout::new(rect(1_280.0), 1.0, 1.0, 0.5);
        let size = layout.left.size.end - layout.left.size.start;
        assert!(size >= MINIMUM_COLUMN - 1.0, "size column came out {size}");
    }

    #[test]
    fn a_title_uses_the_folder_names() {
        assert_eq!(folder_name(&PathBuf::from("/a/b/project")), "project");
    }

    fn archives(handling: &ca_session::settings::folder::ArchiveHandling) -> ca_fs::ArchiveTypes {
        crate::settings::archive_types(handling, None)
    }

    #[test]
    fn a_double_click_on_a_pair_of_archives_opens_a_folder_comparison() {
        let request = pair_request(
            Path::new("/l"),
            Path::new("/r"),
            Path::new("sub/pack.zip"),
            &archives(&ca_session::settings::folder::ArchiveHandling::AsFolders),
        );
        assert_eq!(request.kind, ca_session::SessionKind::FolderCompare);
        assert_eq!(request.left, Path::new("/l").join("sub/pack.zip"));
        assert_eq!(request.right, Path::new("/r").join("sub/pack.zip"));
    }

    #[test]
    fn a_double_click_on_an_archive_under_the_as_files_handling_opens_a_file_comparison() {
        let request = pair_request(
            Path::new("/l"),
            Path::new("/r"),
            Path::new("pack.zip"),
            &archives(&ca_session::settings::folder::ArchiveHandling::AsFiles),
        );
        assert_eq!(request.kind, ca_session::SessionKind::TextCompare);
    }

    #[test]
    fn a_double_click_on_a_plain_file_opens_a_file_comparison() {
        let request = pair_request(
            Path::new("/l"),
            Path::new("/r"),
            Path::new("notes.txt"),
            &archives(&ca_session::settings::folder::ArchiveHandling::AsFolders),
        );
        assert_eq!(request.kind, ca_session::SessionKind::TextCompare);
    }

    #[test]
    fn a_file_inside_an_archive_side_opens_read_only_over_a_copy() {
        const EMPTY_ZIP: &[u8] = &[
            0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left");
        std::fs::create_dir_all(left.join("sub")).unwrap();
        std::fs::write(left.join("sub/notes.txt"), b"left").unwrap();
        let zip = dir.path().join("right.zip");
        std::fs::write(&zip, EMPTY_ZIP).unwrap();
        let right = ca_fs::Source::archive(&zip, ca_vfs::ArchiveOptions::default()).unwrap();
        let mut reader: &[u8] = b"right";
        right
            .file_system()
            .write_file(
                &ca_vfs::VfsPath::parse("sub/notes.txt").unwrap(),
                &mut reader,
                &ca_vfs::Cancel::new(),
            )
            .unwrap();
        let sides = crate::jobs::Sides {
            left: ca_fs::Source::local(&left),
            right: ca_fs::Source::archive(&zip, ca_vfs::ArchiveOptions::default()).unwrap(),
        };
        let rel = Path::new("sub").join("notes.txt");
        let request = pair_request(
            &left,
            &zip,
            &rel,
            &archives(&ca_session::settings::folder::ArchiveHandling::AsFolders),
        );
        let copies = dir.path().join("copies");
        let opened = crate::jobs::extract_pair(
            &sides,
            &rel,
            &copies,
            request,
            &ca_ui::worker::Cancel::new(),
        )
        .unwrap();
        assert_eq!(opened.kind, ca_session::SessionKind::TextCompare);
        assert!(opened.read_only);
        assert_eq!(opened.left, left.join(&rel));
        assert_eq!(std::fs::read(&opened.right).unwrap(), b"right");
        assert_eq!(opened.temporaries.len(), 1);
        assert!(opened.right.starts_with(&opened.temporaries[0]));
        assert!(opened.temporaries[0].starts_with(&copies));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod stale_results {
    use super::FolderView;
    use crate::opjobs::{PreviewMessage, RescanMessage};
    use crate::tree::Arena;
    use ca_fs::Node;
    use ca_ui::view::SessionView;
    use ca_ui::worker::{Job, Terminal};
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc};
    use std::time::{Duration, Instant};

    const PATIENCE: Duration = Duration::from_secs(30);

    fn poll_until(view: &mut FolderView, ready: impl Fn(&FolderView) -> bool) -> bool {
        let deadline = Instant::now() + PATIENCE;
        loop {
            view.tick();
            if ready(view) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Two folders holding `same.txt`, compared and on screen.
    fn ready_view(dir: &Path) -> FolderView {
        for side in ["left", "right", "journals"] {
            std::fs::create_dir_all(dir.join(side)).unwrap();
        }
        for side in ["left", "right"] {
            std::fs::write(dir.join(side).join("same.txt"), b"same").unwrap();
        }
        let mut view = FolderView::with_journal_directory(
            dir.join("left"),
            dir.join("right"),
            &ca_ui::testing::context(),
            1,
            dir.join("journals"),
        );
        assert!(poll_until(&mut view, FolderView::is_ready));
        view
    }

    /// A compared tree that holds `stale.txt`, which no scan of the view's
    /// folders produces.
    fn stale_tree(dir: &Path) -> Box<Node> {
        let root = dir.join("stale");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("stale.txt"), b"stale").unwrap();
        let cancel = ca_fs::Cancel::new();
        let scanned =
            ca_fs::scan_with(&root, &ca_fs::ScanOptions::default(), &cancel, &|_| {}).unwrap();
        Box::new(ca_fs::align_trees(
            &scanned,
            &scanned,
            &ca_fs::AlignmentOptions::default(),
            &cancel,
        ))
    }

    /// A job that answers `answer` only once `gate` opens, and records that
    /// its answer went out, whether or not anything still reads it.
    fn gated<M: Terminal>(answer: M, gate: mpsc::Receiver<()>, sent: Arc<AtomicBool>) -> Job<M> {
        Job::spawn(move |emitter, _cancel| {
            let _ = gate.recv();
            emitter.send(answer);
            sent.store(true, Ordering::SeqCst);
        })
    }

    /// Open the gate, wait for the answer to go out, and give the view the
    /// frames it takes to read it.
    fn release(view: &mut FolderView, open: &mpsc::Sender<()>, sent: &AtomicBool) {
        let _ = open.send(());
        assert!(poll_until(view, |_| sent.load(Ordering::SeqCst)));
        for _ in 0..20 {
            view.tick();
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn shows_stale(view: &FolderView) -> bool {
        view.arena().index_of(Path::new("stale.txt")).is_some()
    }

    #[test]
    fn a_rescan_that_lands_after_a_restart_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let mut view = ready_view(dir.path());
        let tree = stale_tree(dir.path());
        let (open, gate) = mpsc::channel();
        let sent = Arc::new(AtomicBool::new(false));
        view.rescan_comparison = view.comparisons.current();
        view.rescan_job = Some(gated(
            RescanMessage::Done {
                arena: Box::new(Arena::from_root(&tree)),
                tree,
            },
            gate,
            Arc::clone(&sent),
        ));

        view.run(ca_ui::command::Command::Reload);
        assert!(poll_until(&mut view, FolderView::is_ready));
        release(&mut view, &open, &sent);

        assert!(
            !shows_stale(&view),
            "the rows show a tree the restart replaced"
        );
        assert!(view.arena().index_of(Path::new("same.txt")).is_some());
    }

    #[test]
    fn a_preview_that_lands_after_a_restart_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let mut view = ready_view(dir.path());
        let tree = stale_tree(dir.path());
        let preview = ca_fs::preview(&tree, &ca_fs::SyncPreset::MirrorToRight);
        let (open, gate) = mpsc::channel();
        let sent = Arc::new(AtomicBool::new(false));
        view.preview_comparison = view.comparisons.current();
        view.preview_job = Some(gated(
            PreviewMessage::Done {
                tree: Some(tree),
                preview: Box::new(preview),
            },
            gate,
            Arc::clone(&sent),
        ));

        view.run(ca_ui::command::Command::Reload);
        assert!(poll_until(&mut view, FolderView::is_ready));
        release(&mut view, &open, &sent);

        assert!(
            view.sync().preview.is_none(),
            "a preview of the replaced comparison landed"
        );
        let held = view.compared_tree.as_ref().unwrap();
        assert!(held
            .children
            .iter()
            .all(|child| child.rel != Path::new("stale.txt")));
    }

    #[test]
    fn scan_sort_and_picker_slots_are_let_go_in_the_poll_that_delivers_their_answer() {
        let dir = tempfile::tempdir().unwrap();
        let mut view = ready_view(dir.path());
        assert!(poll_until(&mut view, |view| view.scan_job.is_none()));
        let (scan, _scan_held) =
            ca_ui::testing::job_held_after(vec![crate::jobs::ScanMessage::Cancelled]);
        view.scan_job = Some(scan);
        let (sort, _sort_held) =
            ca_ui::testing::job_held_after(vec![crate::jobs::SortMessage::Cancelled]);
        view.sort_job = Some(sort);
        let (picker, _picker_held) =
            ca_ui::testing::job_held_after(vec![ca_ui::dialog::DialogMessage::Dismissed]);
        view.picker = Some(picker);

        view.poll();

        assert!(
            view.picker.is_none(),
            "Browse stays refused for a picker that has answered"
        );
        assert!(
            view.scan_job.is_none(),
            "the automatic refresh stays held back by a scan that has answered"
        );
        assert!(view.sort_job.is_none());
        assert_eq!(view.status, super::Status::Cancelled);
    }

    #[test]
    fn a_result_tagged_with_an_earlier_comparison_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let mut view = ready_view(dir.path());
        let earlier = view.comparisons.current();
        view.run(ca_ui::command::Command::Reload);
        assert!(poll_until(&mut view, FolderView::is_ready));

        let tree = stale_tree(dir.path());
        let (open, gate) = mpsc::channel();
        let sent = Arc::new(AtomicBool::new(false));
        view.rescan_comparison = earlier;
        view.rescan_job = Some(gated(
            RescanMessage::Done {
                arena: Box::new(Arena::from_root(&tree)),
                tree,
            },
            gate,
            Arc::clone(&sent),
        ));
        release(&mut view, &open, &sent);

        assert!(!shows_stale(&view));
        assert!(view.rescan_job.is_none(), "the finished job is let go");
    }
}
