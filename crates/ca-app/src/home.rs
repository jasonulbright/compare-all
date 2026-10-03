//! The launcher: session type buttons, the paths a new session starts from,
//! the saved sessions tree and the sessions kept automatically.

use crate::registry;
use crate::tree::{Branch, HomeModel, Row};
use ca_session::{SessionId, SessionKind, SessionStore};
use ca_ui::dialog::{self, DialogMessage, Pick, Target};
use ca_ui::sessions::StoreHandle;
use ca_ui::view::{OpenRequest, SessionView, ViewAction, ViewContext};
use ca_ui::widgets;
use ca_ui::worker::Job;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

/// The store the whole window shares.
pub type SharedStore = Rc<RefCell<StoreHandle>>;

/// The list of asks every launcher of one window shares with the window.
pub type HomeOutbox = Rc<RefCell<Vec<HomeAction>>>;

/// A notice the shell writes and whichever launcher is open reads.
pub type SharedNotice = Arc<std::sync::Mutex<Option<String>>>;

/// Indent one level of the tree takes.
const INDENT: f32 = 14.0;

/// Width the name of a row is drawn in.
const ROW_NAME_WIDTH: f32 = 260.0;

/// A banner is a bounded summary, not a history of failed background jobs.
const MAX_NOTICE_BYTES: usize = 32 * 1024;

/// What the launcher asks the shell to do beyond opening a tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HomeAction {
    /// Open the saved session with this identifier.
    OpenSaved(SessionId),
    /// Open a session of this kind with no sides yet.
    NewSession(SessionKind),
    /// Write the store, because the tree was edited.
    Save,
}

/// One line of the saved sessions tree, as the shell's tests read it.
pub type SessionRow = Row;

/// The launcher tab.
pub struct HomeView {
    id: egui::Id,
    left: String,
    right: String,
    store: SharedStore,
    model: HomeModel,
    /// The row being renamed, with the text typed so far.
    renaming: Option<(SessionId, String)>,
    /// The row a delete is waiting for an answer about.
    confirming: Option<SessionId>,
    /// The name a new folder is being given.
    new_folder: Option<String>,
    notify: Arc<dyn Fn() + Send + Sync>,
    picker: Option<Job<DialogMessage>>,
    picker_target: Target,
    /// Something the launcher has to say that has nowhere else to appear.
    notice: Option<String>,
    /// A notice the shell may raise after the launcher was built.
    shared: Option<SharedNotice>,
    /// Last broadcast already shown here, including one the user dismissed.
    shown_shared: Option<String>,
    /// What the launcher wants the shell to do, shared with the window.
    actions: HomeOutbox,
}

impl HomeView {
    /// A launcher over a store the window already holds.
    #[must_use]
    pub fn new(context: &ViewContext, instance: u64, store: SharedStore) -> Self {
        Self {
            id: egui::Id::new(("home", instance)),
            left: String::new(),
            right: String::new(),
            store,
            model: HomeModel::new(),
            renaming: None,
            confirming: None,
            new_folder: None,
            notify: context.notify.clone(),
            picker: None,
            picker_target: Target::Left,
            notice: None,
            shared: None,
            shown_shared: None,
            actions: HomeOutbox::default(),
        }
    }

    /// A launcher over a store of its own, reading a stated directory.
    #[must_use]
    pub fn in_settings_directory(
        context: &ViewContext,
        instance: u64,
        settings_directory: PathBuf,
    ) -> Self {
        let store = Rc::new(RefCell::new(StoreHandle::open_in(
            settings_directory,
            context.notify.clone(),
        )));
        Self::new(context, instance, store)
    }

    /// A launcher that opens showing `notice`.
    #[must_use]
    pub fn with_notice(
        context: &ViewContext,
        instance: u64,
        store: SharedStore,
        notice: String,
    ) -> Self {
        let mut view = Self::new(context, instance, store);
        view.notice = Some(notice);
        view
    }

    /// Read later notices from `shared` as well as its own.
    pub fn watch(&mut self, shared: SharedNotice) {
        self.shared = Some(shared);
        self.shown_shared = None;
    }

    /// The notice on show, if any.
    #[must_use]
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// The store this launcher reads.
    #[must_use]
    pub fn store(&self) -> &SharedStore {
        &self.store
    }

    /// The tree model, for a caller driving the launcher without a frame.
    #[must_use]
    pub fn model(&self) -> &HomeModel {
        &self.model
    }

    /// The tree model for editing.
    pub fn model_mut(&mut self) -> &mut HomeModel {
        &mut self.model
    }

    /// The saved sessions currently listed.
    #[must_use]
    pub fn sessions(&self) -> Vec<SessionRow> {
        match self.store.try_borrow() {
            Ok(handle) => handle
                .store()
                .map(|store| self.model.rows(store))
                .unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }

    /// Take whatever the launcher wants the shell to do.
    pub fn take_actions(&mut self) -> Vec<HomeAction> {
        self.actions
            .try_borrow_mut()
            .map(|mut held| held.drain(..).collect())
            .unwrap_or_default()
    }

    /// Read the same list of asks the window reads.
    ///
    /// Every launcher of one window shares it, so the window takes what any of
    /// them asked for in one place.
    pub fn share_actions(&mut self, outbox: HomeOutbox) {
        outbox
            .borrow_mut()
            .extend(self.actions.borrow_mut().drain(..));
        self.actions = outbox;
    }

    /// Ask the window for something.
    fn push_action(&self, action: HomeAction) {
        if let Ok(mut held) = self.actions.try_borrow_mut() {
            held.push(action);
        }
    }

    fn poll(&mut self) {
        let mut raised_notices = Vec::new();
        if let Ok(mut handle) = self.store.try_borrow_mut() {
            handle.poll();
            if let Some(raised) = handle.notice().map(str::to_owned) {
                raised_notices.push(raised);
                handle.clear_notice();
            }
        }
        if let Some(raised) = self
            .shared
            .as_ref()
            .and_then(|shared| shared.lock().ok()?.clone())
        {
            if self.shown_shared.as_ref() != Some(&raised) {
                self.shown_shared = Some(raised.clone());
                raised_notices.push(raised);
            }
        }
        for raised in raised_notices {
            self.raise_notice(raised);
        }
        if let Some(job) = self.picker.as_mut() {
            let messages = job.drain();
            let finished = job.is_finished();
            for message in messages {
                match message {
                    DialogMessage::Chosen(path) => {
                        let text = path.display().to_string();
                        match self.picker_target {
                            Target::Left => self.left = text,
                            Target::Right => self.right = text,
                        }
                    }
                    DialogMessage::Dismissed => {}
                    DialogMessage::Failed(reason) => self.notice = Some(reason),
                }
            }
            if finished {
                self.picker = None;
            }
        }
    }

    /// Keep complete messages when they fit, suppress repeats, and explicitly
    /// report any omitted history or shortened individual message.
    fn raise_notice(&mut self, mut raised: String) {
        const OMITTED: &str = "Earlier notices were omitted.\n";
        const SHORTENED: &str = "\n[message shortened]";
        let message_budget = MAX_NOTICE_BYTES - OMITTED.len();
        if raised.len() > message_budget {
            let mut end = message_budget - SHORTENED.len();
            while !raised.is_char_boundary(end) {
                end -= 1;
            }
            raised.truncate(end);
            raised.push_str(SHORTENED);
        }
        if let Some(notice) = self.notice.as_mut() {
            if notice.match_indices(&raised).any(|(start, text)| {
                let end = start + text.len();
                (start == 0 || notice.as_bytes()[start - 1] == b'\n')
                    && (end == notice.len() || notice.as_bytes()[end] == b'\n')
            }) {
                return;
            }
            if notice.len() + 1 + raised.len() > MAX_NOTICE_BYTES {
                *notice = format!("{OMITTED}{raised}");
            } else {
                notice.push('\n');
                notice.push_str(&raised);
            }
        } else {
            self.notice = Some(raised);
        }
    }

    fn open_picker(&mut self, target: Target, pick: Pick) {
        if self.picker.is_some() {
            return;
        }
        self.picker_target = target;
        self.picker = Some(dialog::spawn(pick, self.notify.clone()));
    }

    fn paths_are_set(&self) -> bool {
        !self.left.trim().is_empty() && !self.right.trim().is_empty()
    }

    fn sides(&self) -> (PathBuf, PathBuf) {
        (
            PathBuf::from(self.left.trim()),
            PathBuf::from(self.right.trim()),
        )
    }

    /// Apply one tree operation, keeping whatever it refused as a notice.
    fn edit(
        &mut self,
        operation: impl FnOnce(&mut HomeModel, &mut SessionStore) -> Result<(), String>,
    ) {
        let mut outcome = Ok(());
        if let Ok(mut handle) = self.store.try_borrow_mut() {
            if let Some(store) = handle.store_mut() {
                outcome = operation(&mut self.model, store);
            }
        }
        match outcome {
            Ok(()) => self.push_action(HomeAction::Save),
            Err(reason) => self.notice = Some(reason),
        }
    }
}

impl SessionView for HomeView {
    fn title(&self) -> String {
        "Home".to_string()
    }

    fn is_launcher(&self) -> bool {
        true
    }

    fn tick(&mut self) {
        self.poll();
    }

    #[allow(clippy::too_many_lines)]
    fn ui(&mut self, ui: &mut egui::Ui, context: &ViewContext) -> Vec<ViewAction> {
        let mut actions = Vec::new();
        let mut dismiss = false;
        if let Some(notice) = &self.notice {
            let notice = notice.clone();
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.vertical(|ui| {
                    widgets::notice(
                        ui,
                        &context.palette,
                        ca_ui::icons::Icon::Info,
                        16.0,
                        &notice,
                    );
                    if ui.button("Dismiss").clicked() {
                        dismiss = true;
                    }
                });
            });
        }
        if dismiss {
            self.notice = None;
        }
        ui.heading("New session");
        ui.horizontal_wrapped(|ui| {
            // The buttons come from the registry, so a new view crate reaches
            // the launcher without this file naming it.
            for (kind, available) in launcher_entries() {
                if !available {
                    ui.add_enabled(
                        false,
                        widgets::IconButton::new(
                            kind.title(),
                            Some(ca_ui::icons::session_icon(&kind)),
                        ),
                    )
                    .on_hover_text("Not available in this build");
                    continue;
                }
                let response = ui.add(widgets::IconButton::new(
                    kind.title(),
                    Some(ca_ui::icons::session_icon(&kind)),
                ));
                if response.clicked() {
                    if self.paths_are_set() {
                        let (left, right) = self.sides();
                        actions.push(ViewAction::Open(OpenRequest::new(kind, left, right)));
                    } else {
                        self.push_action(HomeAction::NewSession(kind));
                    }
                }
            }
        });
        ui.separator();
        let mut picker: Option<(Target, Pick)> = None;
        for (label, target) in [("Left", Target::Left), ("Right", Target::Right)] {
            let field = match target {
                Target::Left => &mut self.left,
                Target::Right => &mut self.right,
            };
            ui.horizontal_wrapped(|ui| {
                ui.label(label);
                let width = (ui.available_width() * 0.5).clamp(80.0, 480.0);
                ui.add(egui::TextEdit::singleline(field).desired_width(width));
                if ca_ui::widgets::inline_button(ui, "File", ca_ui::icons::Icon::File).clicked() {
                    picker = Some((target, Pick::File));
                }
                if ca_ui::widgets::inline_button(ui, "Folder", ca_ui::icons::Icon::Folder).clicked()
                {
                    picker = Some((target, Pick::Folder));
                }
            });
        }
        if let Some((target, pick)) = picker {
            self.open_picker(target, pick);
        }
        ui.separator();
        self.sessions_pane(ui);
        actions
    }

    fn notice(&self) -> Option<String> {
        self.notice.clone()
    }

    fn on_close(&mut self) {
        if let Some(job) = self.picker.take() {
            job.cancel();
        }
    }
}

impl HomeView {
    /// The saved sessions tree, the search field and the automatic list.
    #[allow(clippy::too_many_lines)]
    fn sessions_pane(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.heading("Saved sessions");
            let mut search = self.model.search().to_owned();
            let width = (ui.available_width() * 0.4).clamp(80.0, 280.0);
            if ui
                .add(
                    egui::TextEdit::singleline(&mut search)
                        .hint_text("Search")
                        .desired_width(width),
                )
                .changed()
            {
                self.model.set_search(search);
            }
            if ca_ui::widgets::inline_button(ui, "New Folder", ca_ui::icons::Icon::NewFolder)
                .clicked()
            {
                self.new_folder = Some(String::new());
            }
        });
        if let Some(name) = self.new_folder.as_mut() {
            let mut create = false;
            let mut cancel = false;
            ui.horizontal_wrapped(|ui| {
                ui.label("Folder name");
                ui.add(egui::TextEdit::singleline(name).desired_width(200.0));
                create = ui.button("Create").clicked();
                cancel = ui.button("Cancel").clicked();
            });
            if create {
                let name = name.clone();
                let parent = self.model.selected().cloned();
                let parent = parent.filter(|id| self.is_folder(id));
                self.edit(|model, store| model.create_folder(store, parent.as_ref(), &name));
                self.new_folder = None;
            } else if cancel {
                self.new_folder = None;
            }
        }
        if let Some(id) = self.confirming.clone() {
            let name = self.name_of(&id);
            let text = format!("Delete {name}? Everything inside it goes too.");
            widgets::wrapped_text(ui, &text);
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add(
                        widgets::IconButton::new("Delete", Some(ca_ui::icons::Icon::Delete)).menu(),
                    )
                    .clicked()
                {
                    self.edit(|model, store| model.delete(store, &id));
                    self.confirming = None;
                } else if ui.button("Keep").clicked() {
                    self.confirming = None;
                }
            });
        }
        let loading = self
            .store
            .try_borrow()
            .is_ok_and(|handle| !handle.is_ready());
        if loading {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Reading saved sessions");
            });
            return;
        }
        let rows = self.sessions();
        let saved: Vec<Row> = rows
            .iter()
            .filter(|row| row.branch == Branch::Saved)
            .cloned()
            .collect();
        let automatic: Vec<Row> = rows
            .iter()
            .filter(|row| row.branch == Branch::AutoSaved)
            .cloned()
            .collect();
        if saved.is_empty() {
            ui.label("No saved session");
        }
        egui::ScrollArea::vertical()
            .id_salt(self.id.with("sessions"))
            .max_height(260.0)
            .show(ui, |ui| {
                for row in &saved {
                    self.row(ui, row);
                }
            });
        if !automatic.is_empty() {
            ui.separator();
            ui.heading("Recent sessions");
            egui::ScrollArea::vertical()
                .id_salt(self.id.with("automatic"))
                .max_height(140.0)
                .show(ui, |ui| {
                    for row in &automatic {
                        self.row(ui, row);
                    }
                });
        }
    }

    /// One line of the tree, with its menu.
    fn row(&mut self, ui: &mut egui::Ui, row: &Row) {
        ui.horizontal(|ui| {
            #[allow(clippy::cast_precision_loss)]
            ui.add_space(row.depth as f32 * INDENT);
            if row.is_folder {
                let (icon, label) = if row.is_open {
                    (ca_ui::icons::Icon::ChevronDown, "Collapse Folder")
                } else {
                    (ca_ui::icons::Icon::ChevronRight, "Expand Folder")
                };
                if widgets::icon_only(ui, label, icon, None).clicked() {
                    self.model.toggle(&row.id);
                }
            }
            if let Some((id, name)) = self.renaming.as_mut() {
                if id == &row.id {
                    let id = id.clone();
                    let mut accept = false;
                    ui.add(egui::TextEdit::singleline(name).desired_width(180.0));
                    accept = accept || ui.small_button("Rename").clicked();
                    let cancel = ui.small_button("Cancel").clicked();
                    if accept {
                        let name = name.clone();
                        self.edit(|model, store| model.rename(store, &id, &name));
                        self.renaming = None;
                    } else if cancel {
                        self.renaming = None;
                    }
                    return;
                }
            }
            let picked = self.model.selected() == Some(&row.id);
            let label = label_of(row);
            let icon = row.kind.as_ref().map_or_else(
                || {
                    if row.is_open {
                        ca_ui::icons::Icon::FolderOpen
                    } else {
                        ca_ui::icons::Icon::Folder
                    }
                },
                ca_ui::icons::session_icon,
            );
            let response = widgets::sized(ui, ROW_NAME_WIDTH.min(ui.available_width()), |ui| {
                ui.add(
                    widgets::IconButton::new(
                        &widgets::elide_to_width(ui, &label, ROW_NAME_WIDTH - 24.0),
                        Some(icon),
                    )
                    .inline()
                    .selected(picked),
                )
                .on_hover_text(label.clone())
            });
            response.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::WidgetType::SelectableLabel,
                    ui.is_enabled(),
                    picked,
                    &label,
                )
            });
            if row.is_locked {
                ca_ui::icons::Icon::Lock.show(ui, 16.0, ui.visuals().text_color());
            }
            if response.clicked() {
                self.model.select(row.id.clone());
            }
            if response.double_clicked() {
                if row.is_folder {
                    self.model.toggle(&row.id);
                } else {
                    self.push_action(HomeAction::OpenSaved(row.id.clone()));
                }
            }
            if response.drag_started() {
                self.model.begin_drag(row.id.clone());
            }
            if response.hovered()
                && ui.input(|input| input.pointer.any_released())
                && self.model.dragging().is_some()
            {
                let parent = row.is_folder.then(|| row.id.clone());
                self.edit(|model, store| model.drop_onto(store, parent.as_ref()));
            }
            self.row_menu(&response, row);
        });
    }

    /// The menu one line carries.
    fn row_menu(&mut self, response: &egui::Response, row: &Row) {
        let id = row.id.clone();
        let mut open = false;
        let mut rename = false;
        let mut duplicate = false;
        let mut delete = false;
        let mut lock: Option<bool> = None;
        let mut move_out = false;
        response.context_menu(|ui| {
            ui.set_min_width(180.0);
            if !row.is_folder
                && ui
                    .add(
                        widgets::IconButton::new("Open", Some(ca_ui::icons::Icon::OpenSession))
                            .menu(),
                    )
                    .clicked()
            {
                open = true;
                ui.close_menu();
            }
            if ui
                .add(widgets::IconButton::new("Rename", Some(ca_ui::icons::Icon::Rename)).menu())
                .clicked()
            {
                rename = true;
                ui.close_menu();
            }
            if ui
                .add(
                    widgets::IconButton::new("Duplicate", Some(ca_ui::icons::Icon::Duplicate))
                        .menu(),
                )
                .clicked()
            {
                duplicate = true;
                ui.close_menu();
            }
            if ui
                .add(widgets::IconButton::new("Move to top level", None).menu())
                .clicked()
            {
                move_out = true;
                ui.close_menu();
            }
            if !row.is_folder {
                let label = if row.is_locked { "Unlock" } else { "Lock" };
                if ui
                    .add(
                        widgets::IconButton::new(
                            label,
                            Some(if row.is_locked {
                                ca_ui::icons::Icon::Unlock
                            } else {
                                ca_ui::icons::Icon::Lock
                            }),
                        )
                        .menu(),
                    )
                    .clicked()
                {
                    lock = Some(!row.is_locked);
                    ui.close_menu();
                }
            }
            if ui
                .add(widgets::IconButton::new("Delete", Some(ca_ui::icons::Icon::Delete)).menu())
                .clicked()
            {
                delete = true;
                ui.close_menu();
            }
        });
        if open {
            self.push_action(HomeAction::OpenSaved(id.clone()));
        }
        if rename {
            self.renaming = Some((id.clone(), row.name.clone()));
        }
        if duplicate {
            let target = id.clone();
            self.edit(|model, store| model.duplicate(store, &target));
        }
        if move_out {
            let target = id.clone();
            self.edit(|model, store| model.move_node(store, &target, None));
        }
        if let Some(locked) = lock {
            let target = id.clone();
            self.edit(move |model, store| model.set_locked(store, &target, locked));
        }
        if delete {
            self.confirming = Some(id);
        }
    }

    fn is_folder(&self, id: &SessionId) -> bool {
        self.store.try_borrow().is_ok_and(|handle| {
            handle
                .store()
                .and_then(|store| store.find(id))
                .is_some_and(ca_session::TreeNode::is_folder)
        })
    }

    fn name_of(&self, id: &SessionId) -> String {
        self.store
            .try_borrow()
            .ok()
            .and_then(|handle| {
                handle
                    .store()
                    .and_then(|store| store.find(id))
                    .map(|node| node.name().to_owned())
            })
            .unwrap_or_else(|| id.to_string())
    }
}

/// The text one line shows.
fn label_of(row: &Row) -> String {
    let kind = row.kind.as_ref().map_or("Folder", SessionKind::title);
    let locked = if row.is_locked { ", locked" } else { "" };
    format!("{}  ({kind}){locked}", row.name)
}

/// Every session kind the launcher offers, with whether this build has a view
/// for it.
///
/// A kind with no view stays on the list and is shown disabled, so the
/// launcher reports what the build does not do rather than hiding it.
#[must_use]
pub fn launcher_entries() -> Vec<(SessionKind, bool)> {
    SessionKind::ALL
        .iter()
        .map(|kind| (kind.clone(), registry::is_available(kind)))
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    #[test]
    fn folder_chevrons_expand_and_collapse_by_accessible_label() {
        let dir = tempfile::tempdir().unwrap();
        let context = ca_ui::testing::context();
        let mut view =
            super::HomeView::in_settings_directory(&context, 1, dir.path().to_path_buf());
        let id = ca_session::SessionId::from_raw("folder");
        let mut probe = ca_ui::testing::probe::Probe::new(800.0, 500.0);
        for label in ["Expand Folder", "Collapse Folder"] {
            let mut run = |ctx: &egui::Context| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let row = crate::tree::Row {
                        id: id.clone(),
                        depth: 0,
                        name: "Folder".into(),
                        kind: None,
                        is_folder: true,
                        is_open: view.model.is_open(&id),
                        is_locked: false,
                        branch: crate::tree::Branch::Saved,
                    };
                    view.row(ui, &row);
                });
            };
            probe.idle(&mut run);
            probe.idle(&mut run);
            probe.click(label, &mut run).unwrap();
            assert_eq!(view.model.is_open(&id), label == "Expand Folder");
        }
    }

    #[test]
    fn a_launcher_icon_button_starts_its_kind_by_accessible_label() {
        let dir = tempfile::tempdir().unwrap();
        let context = ca_ui::testing::context();
        let mut view =
            super::HomeView::in_settings_directory(&context, 1, dir.path().to_path_buf());
        let mut probe = ca_ui::testing::probe::Probe::new(1280.0, 800.0);
        let mut run = |ctx: &egui::Context| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = ca_ui::view::SessionView::ui(&mut view, ui, &context);
            });
        };
        probe.idle(&mut run);
        probe.idle(&mut run);
        probe
            .click(SessionKind::TextCompare.title(), &mut run)
            .unwrap();
        assert_eq!(
            view.take_actions(),
            vec![super::HomeAction::NewSession(SessionKind::TextCompare)]
        );
    }

    #[test]
    fn a_locked_saved_session_announces_its_state_and_keeps_it_in_hover_text() {
        let dir = tempfile::tempdir().unwrap();
        let context = ca_ui::testing::context();
        let mut view =
            super::HomeView::in_settings_directory(&context, 1, dir.path().to_path_buf());
        let id = ca_session::SessionId::from_raw("locked");
        let row = crate::tree::Row {
            id: id.clone(),
            depth: 0,
            name: "Saved".into(),
            kind: Some(SessionKind::TextCompare),
            is_folder: false,
            is_open: false,
            is_locked: true,
            branch: crate::tree::Branch::Saved,
        };
        let label = "Saved  (Text Compare), locked";
        assert_eq!(super::label_of(&row), label);
        let mut probe = ca_ui::testing::probe::Probe::new(800.0, 500.0);
        let mut run = |ctx: &egui::Context| {
            ctx.style_mut(|style| style.interaction.tooltip_delay = 0.0);
            egui::CentralPanel::default().show(ctx, |ui| view.row(ui, &row));
        };
        probe.idle(&mut run);
        probe.idle(&mut run);
        let control = probe.find(label).unwrap();
        probe.frame(
            vec![egui::Event::PointerMoved(control.rect.center())],
            &mut run,
        );
        let output = probe.frame(Vec::new(), &mut run);
        assert!(
            output
                .shapes
                .iter()
                .filter(|shape| {
                    matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == label)
                })
                .count()
                >= 2
        );
        probe.click(label, &mut run).unwrap();
        assert_eq!(view.model.selected(), Some(&id));
    }

    #[test]
    fn a_saved_session_row_is_selected_by_accessible_label() {
        let dir = tempfile::tempdir().unwrap();
        let context = ca_ui::testing::context();
        let mut view =
            super::HomeView::in_settings_directory(&context, 1, dir.path().to_path_buf());
        let id = ca_session::SessionId::from_raw("saved");
        let row = crate::tree::Row {
            id: id.clone(),
            depth: 0,
            name: "Saved".into(),
            kind: Some(SessionKind::TextCompare),
            is_folder: false,
            is_open: false,
            is_locked: false,
            branch: crate::tree::Branch::Saved,
        };
        let mut probe = ca_ui::testing::probe::Probe::new(800.0, 500.0);
        let mut run = |ctx: &egui::Context| {
            egui::CentralPanel::default().show(ctx, |ui| view.row(ui, &row));
        };
        probe.idle(&mut run);
        probe.idle(&mut run);
        probe.click("Saved  (Text Compare)", &mut run).unwrap();
        assert_eq!(view.model.selected(), Some(&id));
    }
    use super::launcher_entries;
    use crate::registry;
    use ca_session::SessionKind;

    /// Recovery remains visible alongside an earlier settings notice; polling
    /// cannot duplicate it or bring it back after dismissal.
    #[test]
    #[allow(clippy::unwrap_used)]
    fn a_recovery_notice_joins_an_existing_notice_once() {
        let dir = tempfile::tempdir().unwrap();
        let context = ca_ui::testing::context();
        let mut view =
            super::HomeView::in_settings_directory(&context, 1, dir.path().to_path_buf());
        let store = std::rc::Rc::clone(view.store());
        let _borrow = store.borrow_mut();
        view.notice = Some("Another instance holds the settings directory.".to_owned());
        let shared = std::sync::Arc::new(std::sync::Mutex::new(Some(
            "An unfinished operation names a long path.".to_owned(),
        )));
        view.watch(std::sync::Arc::clone(&shared));
        view.poll();
        let notice = view.notice().unwrap().to_owned();
        assert!(notice.contains("Another instance"));
        assert!(notice.contains("unfinished operation"));
        view.poll();
        assert_eq!(view.notice(), Some(notice.as_str()));
        view.notice = None;
        view.poll();
        assert!(view.notice().is_none());
        *shared.lock().unwrap() = Some("Another interrupted batch.".to_owned());
        view.poll();
        assert_eq!(view.notice(), Some("Another interrupted batch."));
    }

    /// Repeated failures do not grow the banner, and distinct failures retain
    /// the latest message within a finite display budget.
    #[test]
    fn repeated_launcher_notices_have_bounded_retention() {
        let dir = tempfile::tempdir().unwrap();
        let context = ca_ui::testing::context();
        let mut view =
            super::HomeView::in_settings_directory(&context, 1, dir.path().to_path_buf());
        let store = std::rc::Rc::clone(view.store());
        let _borrow = store.borrow_mut();
        let shared = std::sync::Arc::new(std::sync::Mutex::new(None));
        view.watch(std::sync::Arc::clone(&shared));
        for _ in 0..100 {
            for text in ["Failed to save.", "Failed to read."] {
                *shared.lock().unwrap() = Some(text.to_owned());
                view.poll();
            }
        }
        assert_eq!(view.notice(), Some("Failed to save.\nFailed to read."));
        for index in 0..100 {
            *shared.lock().unwrap() = Some(format!("Failure {index}: {}", "é".repeat(1_000)));
            view.poll();
        }
        let notice = view.notice().unwrap();
        assert!(notice.len() <= 32 * 1024);
        assert!(notice.contains("Failure 99:"));
        assert!(notice.contains("Earlier notices were omitted."));
        *shared.lock().unwrap() = Some("é".repeat(40_000));
        view.poll();
        assert!(view.notice().unwrap().len() <= 32 * 1024);
        assert!(view.notice().unwrap().ends_with("[message shortened]"));
    }

    #[test]
    fn every_kind_has_a_label() {
        for kind in [
            SessionKind::TextCompare,
            SessionKind::FolderCompare,
            SessionKind::HexCompare,
            SessionKind::Unknown("custom".to_string()),
        ] {
            assert!(!kind.title().is_empty());
        }
    }

    /// The launcher lists every kind, and every registered kind reaches a
    /// working button.
    #[test]
    fn the_launcher_lists_every_kind_and_enables_the_registered_ones() {
        let entries = launcher_entries();
        assert_eq!(entries.len(), SessionKind::ALL.len());
        for kind in registry::available() {
            assert!(
                entries
                    .iter()
                    .any(|(listed, enabled)| *listed == kind && *enabled),
                "{kind} is registered but the launcher does not offer it"
            );
        }
        assert!(
            entries
                .iter()
                .any(|(kind, enabled)| *kind == SessionKind::FolderSync && *enabled),
            "folder sync is registered and has to reach the launcher"
        );
        for (kind, enabled) in &entries {
            assert_eq!(*enabled, registry::is_available(kind), "{kind}");
            assert!(!kind.title().is_empty());
        }
    }
}
