//! What the shell requires of anything that can occupy a tab.

use crate::command::{Command, MenuView};
use crate::theme::Palette;
use ca_session::SessionKind;
use std::path::PathBuf;
use std::sync::Arc;

/// The caller supplied name of one side, where there is one.
///
/// A tool that hands over a temporary file supplies the name the user knows,
/// so the pane shows that instead of the path on disk.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Titles {
    /// Name shown for the left side.
    pub left: Option<String>,
    /// Name shown for the right side.
    pub right: Option<String>,
    /// Name shown for the ancestor.
    pub center: Option<String>,
    /// Name shown for the output.
    pub output: Option<String>,
}

/// A comparison a view asks the shell to open.
///
/// A view never constructs another view. It names the kind and the sides, and
/// the shell's registry decides which crate answers for that kind. This is what
/// keeps the view crates independent of one another.
///
/// Two paths describe every comparison of two sides. A merge adds an ancestor
/// and an output, both optional, so a two path caller is unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenRequest {
    /// Which comparison to open.
    pub kind: SessionKind,
    /// Left side path.
    pub left: PathBuf,
    /// Right side path.
    pub right: PathBuf,
    /// Common ancestor path, where the comparison has one.
    pub center: Option<PathBuf>,
    /// Path the result is written to, where the comparison writes one.
    pub output: Option<PathBuf>,
    /// Caller supplied names of the sides.
    pub titles: Titles,
    /// The view refuses every edit and every save.
    pub read_only: bool,
    /// Copies made for this tab alone. The tab deletes them when it closes.
    pub temporaries: Vec<PathBuf>,
}

impl OpenRequest {
    /// A request for `kind` over the two paths.
    #[must_use]
    pub fn new(kind: SessionKind, left: PathBuf, right: PathBuf) -> Self {
        Self {
            kind,
            left,
            right,
            center: None,
            output: None,
            titles: Titles::default(),
            read_only: false,
            temporaries: Vec::new(),
        }
    }

    /// The same request with a common ancestor.
    #[must_use]
    pub fn with_center(mut self, center: Option<PathBuf>) -> Self {
        self.center = center;
        self
    }

    /// The same request with an output path.
    #[must_use]
    pub fn with_output(mut self, output: Option<PathBuf>) -> Self {
        self.output = output;
        self
    }

    /// The same request over copies the tab owns: read-only, and deleted when
    /// the tab closes.
    #[must_use]
    pub fn over_temporaries(mut self, temporaries: Vec<PathBuf>) -> Self {
        self.read_only = true;
        self.temporaries = temporaries;
        self
    }

    /// The same request with caller supplied names.
    #[must_use]
    pub fn with_titles(mut self, titles: Titles) -> Self {
        self.titles = titles;
        self
    }
}

/// What a view asks the shell to do once its frame is over.
///
/// The open request is the large variant. One action is built per frame at
/// most, so boxing it would cost an allocation to save nothing.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewAction {
    /// Open a comparison in a new tab.
    Open(OpenRequest),
    /// Open a tab showing the launcher.
    OpenHome,
    /// Close the tab this view occupies.
    Close,
}

/// One command a view handles, with whether it can run now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandState {
    /// The command.
    pub command: Command,
    /// True when the view can run it at this moment.
    pub enabled: bool,
}

/// What a view is given each frame.
#[derive(Clone)]
pub struct ViewContext {
    /// The options in force when the view is built or painted.
    pub options: Arc<crate::options::AppOptions>,
    /// The color table in force.
    pub palette: Palette,
    /// Ask for another frame once background work posts a result.
    pub notify: Arc<dyn Fn() + Send + Sync>,
}

/// A tab's content.
pub trait SessionView {
    /// Comparison kind shown by this tab, where one exists.
    fn kind(&self) -> Option<ca_session::SessionKind> {
        None
    }
    /// Text shown on the tab.
    fn title(&self) -> String;

    /// Which menu bar and which keyboard table this view carries.
    ///
    /// The same key names a different command in each compare type, so the bar
    /// and the routing follow the view rather than the window.
    fn menu_view(&self) -> MenuView {
        MenuView::Other
    }

    /// Take whatever background work has posted, whether or not this tab is the
    /// one on screen.
    ///
    /// The shell calls this for every tab each frame. A view that only drained
    /// its queues while painting would leave a background tab's results piling
    /// up behind it and its jobs never observed as finished.
    fn tick(&mut self) {}

    /// Let a view cancel work that should not finish after it leaves the
    /// active tab. Called before [`SessionView::tick`] for every tab.
    fn set_active(&mut self, _active: bool) {}

    /// Paint one frame and collect whatever the view wants the shell to do.
    fn ui(&mut self, ui: &mut egui::Ui, context: &ViewContext) -> Vec<ViewAction>;

    /// Every command this view handles, with its enabled state this frame.
    ///
    /// The shell builds its menus from this, so a new view type reaches the
    /// menus without any other view being edited.
    fn commands(&self) -> Vec<CommandState> {
        Vec::new()
    }

    /// True when the view can run `command` right now.
    ///
    /// The default answer is read from [`SessionView::commands`]. A view whose
    /// command set is large overrides this with a direct test, so a menu that
    /// asks about one command does not build the whole list.
    fn accepts(&self, command: Command) -> bool {
        self.commands()
            .iter()
            .any(|state| state.command == command && state.enabled)
    }

    /// Why the view refuses `command` at this moment, where the view knows a
    /// more exact reason than the one the menu line carries.
    fn refusal(&self, _command: Command) -> Option<&'static str> {
        None
    }

    /// Run a command the view reported it accepts.
    fn run(&mut self, _command: Command) {}

    /// Take the settings of the session this view shows and compare again
    /// under them.
    ///
    /// The view converts them to its engine's options through the one
    /// conversion its crate owns. Settings of another kind are ignored, so a
    /// caller never has to match the kind first.
    fn apply_settings(&mut self, _settings: &ca_session::settings::SessionSettings) {}

    /// The settings of the session this view shows, where the view holds them.
    ///
    /// The shell opens the settings dialog over this, so the dialog shows what
    /// the view is actually running under.
    fn settings(&self) -> Option<ca_session::settings::SessionSettings> {
        None
    }

    /// True once whatever the view loads in the background has arrived.
    fn is_ready(&self) -> bool {
        true
    }

    /// Something the view is showing that the shell may also want to report.
    fn notice(&self) -> Option<String> {
        None
    }

    /// True once the view has asked to be closed from inside.
    fn wants_close(&self) -> bool {
        false
    }

    /// Called before the tab is dropped, so background work can be stopped.
    fn on_close(&mut self) {}

    /// True while the tab reads copies made for it alone.
    ///
    /// A session over such copies names paths that are gone once the tab
    /// closes, so it is never kept: the shell keeps no automatic session of
    /// the tab, refuses Save Session and Save Session As on it, and records
    /// it in a workspace as the launcher.
    fn holds_temporaries(&self) -> bool {
        false
    }

    /// True when the tab may close now.
    ///
    /// A view that holds unwritten edits answers false and raises its own
    /// question, so a close never discards work without asking.
    fn may_close(&mut self) -> bool {
        true
    }

    /// True while work the view runs stops it from closing, such as a file
    /// operation that writes files or a save in flight.
    ///
    /// [`SessionView::may_close`] refuses without a question while this is
    /// true, so a close that waits for the tab tries again once it is false.
    fn is_busy(&self) -> bool {
        false
    }

    /// True while the view holds edits that are not written to disk.
    ///
    /// Unlike [`SessionView::may_close`] this raises no question. The shell
    /// reads it before it opens the tab again over other sides, so a change
    /// of the sides never drops an edit.
    fn holds_unwritten_edits(&self) -> bool {
        false
    }

    /// The process exit code this view asks for, where it has one.
    ///
    /// A comparison started by another program reports its result this way, so
    /// the caller reads an outcome rather than parsing the window.
    fn exit_code(&self) -> Option<i32> {
        None
    }

    /// True when the tab shows the launcher rather than a comparison.
    ///
    /// The window reads this to place a session in the tab the launcher was
    /// started from rather than in a new one.
    fn is_launcher(&self) -> bool {
        false
    }

    /// What an Open With entry would be run over, where the view has one.
    ///
    /// A view that names nothing offers no Open With line, so the menu never
    /// shows an entry that would start a program over no file.
    fn launch_target(&self) -> Option<crate::launch::LaunchTarget> {
        None
    }

    /// The active item to show in the platform file manager.
    ///
    /// Two-file views may override this to follow their active side. Folder
    /// views can use the first item in the target because it follows the
    /// selected row and side.
    fn explorer_target(&self) -> Option<(std::path::PathBuf, crate::launch::Selection)> {
        self.launch_target()
            .map(|target| (target.context.first.path, target.selection))
    }
}

/// The stored form of one side a view has open, or `None` for a side that
/// names no path.
#[must_use]
pub fn side_location(path: &std::path::Path) -> Option<ca_session::SideLocation> {
    (!path.as_os_str().is_empty()).then(|| ca_session::SideLocation::local(path.to_path_buf()))
}

/// The sides and the description a view reports: `kept`, the ones it was
/// last given, with the left and the right side replaced by the paths the
/// view has open. Save Session stores what [`SessionView::settings`]
/// reports, and a session reopens over the sides it names.
#[must_use]
pub fn with_sides(
    kept: &ca_session::settings::SpecsSettings,
    left: &std::path::Path,
    right: &std::path::Path,
) -> ca_session::settings::SpecsSettings {
    let mut specs = kept.clone();
    specs.left = side_location(left);
    specs.right = side_location(right);
    specs
}

/// A view the shell can build from a kind and two paths.
///
/// The shell's registry holds one entry per kind; this trait is what lets that
/// entry name the view type alone, with no constructor written out beside it.
pub trait ViewFactory: SessionView + Sized + 'static {
    /// Build the view over the two sides.
    ///
    /// `instance` distinguishes one tab's widget identifiers from another's.
    fn create(left: PathBuf, right: PathBuf, context: &ViewContext, instance: u64) -> Self;

    /// Build the view from a whole request.
    ///
    /// A view that reads more than the two paths overrides this. The default
    /// reads the two paths, which is what every two sided comparison needs.
    #[must_use]
    fn create_from(request: &OpenRequest, context: &ViewContext, instance: u64) -> Self {
        Self::create(
            request.left.clone(),
            request.right.clone(),
            context,
            instance,
        )
    }
}

/// Box the view of type `V` that answers for a request.
///
/// Taken as a function pointer by the registry, so every entry is one line.
#[must_use]
pub fn build<V: ViewFactory>(
    request: &OpenRequest,
    context: &ViewContext,
    instance: u64,
) -> Box<dyn SessionView> {
    let view: Box<dyn SessionView> = Box::new(V::create_from(request, context, instance));
    if request.temporaries.is_empty() {
        view
    } else {
        Box::new(OwnsTemporaries {
            view,
            temporaries: request.temporaries.clone(),
        })
    }
}

/// A view over copies made for its tab alone, which deletes them on close.
pub struct OwnsTemporaries {
    view: Box<dyn SessionView>,
    temporaries: Vec<PathBuf>,
}

/// Delete workers that have not been waited for. A worker still running when
/// the process exits is killed with it, and its copies stay on disk.
static PENDING_DELETES: std::sync::Mutex<Vec<std::thread::JoinHandle<()>>> =
    std::sync::Mutex::new(Vec::new());

fn pending_deletes() -> std::sync::MutexGuard<'static, Vec<std::thread::JoinHandle<()>>> {
    match PENDING_DELETES.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Waits until every delete worker started by a closed tab has finished, or
/// until `limit` passes. Returns true when none is left running.
pub fn wait_for_temporary_deletes(limit: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + limit;
    loop {
        let finished: Vec<_> = {
            let mut pending = pending_deletes();
            let (finished, running) = pending
                .drain(..)
                .partition(std::thread::JoinHandle::is_finished);
            *pending = running;
            finished
        };
        for handle in finished {
            let _ = handle.join();
        }
        if pending_deletes().is_empty() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// Delete each copy on a worker, so a close never waits on the disk.
///
/// A copy that is a folder goes with everything in it. A copy that is already
/// gone is not an error.
fn delete_temporaries(paths: Vec<PathBuf>) {
    let handle = std::thread::spawn(move || {
        for path in paths {
            let _ = if path.is_dir() {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
        }
    });
    pending_deletes().push(handle);
}

impl SessionView for OwnsTemporaries {
    fn kind(&self) -> Option<ca_session::SessionKind> {
        self.view.kind()
    }
    fn title(&self) -> String {
        self.view.title()
    }

    fn menu_view(&self) -> MenuView {
        self.view.menu_view()
    }

    fn tick(&mut self) {
        self.view.tick();
    }

    fn ui(&mut self, ui: &mut egui::Ui, context: &ViewContext) -> Vec<ViewAction> {
        self.view.ui(ui, context)
    }

    fn commands(&self) -> Vec<CommandState> {
        self.view.commands()
    }

    fn accepts(&self, command: Command) -> bool {
        self.view.accepts(command)
    }

    fn refusal(&self, command: Command) -> Option<&'static str> {
        self.view.refusal(command)
    }

    fn run(&mut self, command: Command) {
        self.view.run(command);
    }

    fn apply_settings(&mut self, settings: &ca_session::settings::SessionSettings) {
        self.view.apply_settings(settings);
    }

    fn settings(&self) -> Option<ca_session::settings::SessionSettings> {
        self.view.settings()
    }

    fn is_ready(&self) -> bool {
        self.view.is_ready()
    }

    fn notice(&self) -> Option<String> {
        self.view.notice()
    }

    fn wants_close(&self) -> bool {
        self.view.wants_close()
    }

    fn on_close(&mut self) {
        self.view.on_close();
        delete_temporaries(std::mem::take(&mut self.temporaries));
    }

    fn holds_temporaries(&self) -> bool {
        !self.temporaries.is_empty()
    }

    fn may_close(&mut self) -> bool {
        self.view.may_close()
    }

    fn is_busy(&self) -> bool {
        self.view.is_busy()
    }

    fn holds_unwritten_edits(&self) -> bool {
        self.view.holds_unwritten_edits()
    }

    fn exit_code(&self) -> Option<i32> {
        self.view.exit_code()
    }

    fn is_launcher(&self) -> bool {
        self.view.is_launcher()
    }

    fn launch_target(&self) -> Option<crate::launch::LaunchTarget> {
        self.view.launch_target()
    }

    fn explorer_target(&self) -> Option<(std::path::PathBuf, crate::launch::Selection)> {
        self.view.explorer_target()
    }
}

/// Build a command declaration from a fixed list and a test of each one.
///
/// Every view whose enabled state is a direct match on the command uses this,
/// so the list a view handles is stated once.
#[must_use]
pub fn declare(commands: &[Command], enabled: impl Fn(Command) -> bool) -> Vec<CommandState> {
    commands
        .iter()
        .map(|command| CommandState {
            command: *command,
            enabled: enabled(*command),
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{build, OpenRequest, SessionView, ViewAction, ViewContext, ViewFactory};
    use ca_session::SessionKind;
    use std::path::PathBuf;
    use std::time::Duration;

    struct Blank;

    impl SessionView for Blank {
        fn title(&self) -> String {
            String::new()
        }

        fn ui(&mut self, _ui: &mut egui::Ui, _context: &ViewContext) -> Vec<ViewAction> {
            Vec::new()
        }
    }

    impl ViewFactory for Blank {
        fn create(_left: PathBuf, _right: PathBuf, _context: &ViewContext, _instance: u64) -> Self {
            Self
        }
    }

    #[test]
    fn a_tab_over_temporary_copies_deletes_them_when_it_closes() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("copy.txt");
        let folder = dir.path().join("copies");
        std::fs::write(&file, b"x").unwrap();
        std::fs::create_dir_all(folder.join("inner")).unwrap();
        std::fs::write(folder.join("inner/y.txt"), b"y").unwrap();
        let request = OpenRequest::new(
            SessionKind::TextCompare,
            file.clone(),
            folder.join("inner/y.txt"),
        )
        .over_temporaries(vec![file.clone(), folder.clone()]);
        assert!(request.read_only);
        let mut view = build::<Blank>(&request, &crate::testing::context(), 1);
        assert!(file.exists());
        view.on_close();
        assert!(super::wait_for_temporary_deletes(Duration::from_secs(30)));
        assert!(!file.exists());
        assert!(!folder.exists());
    }
}
