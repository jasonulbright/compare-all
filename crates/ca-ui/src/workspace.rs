//! Named sets of open tabs, and the dialog that manages them.
//!
//! A workspace holds the tabs of every window and which tab each window was
//! showing. Saving one takes a snapshot of the open tabs; loading one closes
//! what is open and reopens the snapshot.

use ca_session::{SessionStore, Workspace, WorkspaceTab, WorkspaceWindow};

/// Width the name column of the manager takes.
const NAME_WIDTH: f32 = 220.0;

/// What the workspace manager was asked to do this frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceAction {
    /// Close every open tab and reopen the named workspace.
    Load(String),
    /// Save the open tabs under the name in the field.
    Save(String),
    /// Rename the workspace.
    Rename {
        /// Name it carries now.
        from: String,
        /// Name it is to carry.
        to: String,
    },
    /// Delete the workspace.
    Delete(String),
    /// Reopen the named workspace when the program starts, or stop doing so
    /// when the name is empty.
    RestoreAtStart(Option<String>),
}

/// The workspace manager dialog.
pub struct WorkspaceManager {
    name: String,
    selected: Option<String>,
    renaming: Option<(String, String)>,
    confirming: Option<String>,
    open: bool,
    id: egui::Id,
}

impl WorkspaceManager {
    /// A manager with nothing selected and an empty name field.
    #[must_use]
    pub fn new(salt: u64) -> Self {
        Self {
            name: String::new(),
            selected: None,
            renaming: None,
            confirming: None,
            open: true,
            id: egui::Id::new(("workspaces", salt)),
        }
    }

    /// A manager opened with the name field filled, for Save Workspace As.
    #[must_use]
    pub fn with_name(salt: u64, name: impl Into<String>) -> Self {
        let mut manager = Self::new(salt);
        manager.name = name.into();
        manager
    }

    /// True while the dialog is on screen.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Close the dialog.
    pub fn close(&mut self) {
        self.open = false;
    }

    /// The workspace the list has selected.
    #[must_use]
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// Select a workspace by name.
    pub fn select(&mut self, name: impl Into<String>) {
        let name = name.into();
        self.name.clone_from(&name);
        self.selected = Some(name);
    }

    /// The text in the name field.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Draw the dialog and report what it was asked to do.
    pub fn show(&mut self, ctx: &egui::Context, store: &SessionStore) -> Option<WorkspaceAction> {
        if !self.open {
            return None;
        }
        let mut action = None;
        let mut open = true;
        egui::Window::new("Manage Workspaces")
            .id(self.id)
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .max_width((ctx.screen_rect().width() - 24.0).max(200.0))
            .show(ctx, |ui| {
                action = self.body(ui, store);
            });
        if !open {
            self.open = false;
        }
        action
    }

    /// Draw the dialog into a panel, for a headless frame.
    pub fn body(&mut self, ui: &mut egui::Ui, store: &SessionStore) -> Option<WorkspaceAction> {
        let mut action = None;
        if let Some(name) = self.confirming.clone() {
            let text = format!("Delete the workspace {name}? The open tabs are not affected.");
            crate::widgets::wrapped_text(ui, &text);
            ui.horizontal_wrapped(|ui| {
                if ui.button("Delete").clicked() {
                    action = Some(WorkspaceAction::Delete(name.clone()));
                    self.confirming = None;
                }
                if ui.button("Keep").clicked() {
                    self.confirming = None;
                }
            });
            ui.separator();
        }
        egui::ScrollArea::vertical()
            .id_salt(self.id.with("list"))
            .max_height(220.0)
            .show(ui, |ui| {
                for workspace in &store.workspaces {
                    let chosen = self.selected.as_deref() == Some(workspace.name.as_str());
                    ui.horizontal_wrapped(|ui| {
                        crate::widgets::sized(ui, NAME_WIDTH, |ui| {
                            let response = ui.selectable_label(
                                chosen,
                                crate::widgets::elide_to_width(
                                    ui,
                                    &workspace.name,
                                    NAME_WIDTH - 8.0,
                                ),
                            );
                            if response.clicked() {
                                self.name.clone_from(&workspace.name);
                                self.selected = Some(workspace.name.clone());
                            }
                            if response.double_clicked() {
                                action = Some(WorkspaceAction::Load(workspace.name.clone()));
                            }
                        });
                        ui.label(format!("{} tabs", tab_count(workspace)));
                        if ui.small_button("Load").clicked() {
                            action = Some(WorkspaceAction::Load(workspace.name.clone()));
                        }
                        if ui.small_button("Rename").clicked() {
                            self.renaming = Some((workspace.name.clone(), workspace.name.clone()));
                        }
                        if ui.small_button("Delete").clicked() {
                            self.confirming = Some(workspace.name.clone());
                        }
                    });
                }
                if store.workspaces.is_empty() {
                    ui.label("No workspace is stored");
                }
            });
        if let Some((from, to)) = self.renaming.as_mut() {
            ui.separator();
            let from = from.clone();
            ui.horizontal_wrapped(|ui| {
                ui.label("New name");
                crate::widgets::sized(ui, NAME_WIDTH, |ui| {
                    ui.add(egui::TextEdit::singleline(to).desired_width(NAME_WIDTH));
                });
                if ui.button("Rename").clicked() {
                    action = Some(WorkspaceAction::Rename {
                        from,
                        to: to.clone(),
                    });
                }
            });
            if action.is_some() || ui.button("Cancel rename").clicked() {
                self.renaming = None;
            }
        }
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            ui.label("Name");
            crate::widgets::sized(ui, NAME_WIDTH, |ui| {
                ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(NAME_WIDTH));
            });
            let named = !self.name.trim().is_empty();
            if crate::widgets::toolbar_button(ui, "Save Workspace", named, "Enter a name first") {
                action = Some(WorkspaceAction::Save(self.name.trim().to_owned()));
            }
        });
        ui.horizontal_wrapped(|ui| {
            let mut restore = store.restore_last_workspace;
            if ui
                .checkbox(&mut restore, "Reopen this workspace at start")
                .changed()
            {
                action = Some(WorkspaceAction::RestoreAtStart(
                    restore.then(|| self.name.trim().to_owned()),
                ));
            }
            if let Some(name) = &store.last_workspace {
                crate::widgets::path_text(ui, &format!("Reopens {name}"));
            }
        });
        action
    }
}

/// How many tabs a workspace holds across every window.
#[must_use]
pub fn tab_count(workspace: &Workspace) -> usize {
    workspace
        .windows
        .iter()
        .map(|window| window.tabs.len())
        .sum()
}

/// Builds a workspace from the tabs of one window.
#[must_use]
pub fn one_window(name: impl Into<String>, tabs: Vec<WorkspaceTab>, active: usize) -> Workspace {
    Workspace {
        name: name.into(),
        windows: vec![WorkspaceWindow {
            bounds: None,
            tabs,
            active_tab: active,
            unknown: std::collections::BTreeMap::new(),
        }],
        shortcut: None,
        unknown: std::collections::BTreeMap::new(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::{one_window, tab_count, WorkspaceManager};
    use ca_session::{SavedSession, SessionId, SessionKind, SessionStore, WorkspaceTab};

    #[test]
    fn a_workspace_of_one_window_counts_its_tabs() {
        let workspace = one_window(
            "Daily",
            vec![
                WorkspaceTab::home(),
                WorkspaceTab::saved(SessionId::from_raw("n1")),
            ],
            1,
        );
        assert_eq!(tab_count(&workspace), 2);
        assert_eq!(workspace.windows[0].active_tab, 1);
    }

    #[test]
    fn saving_replaces_a_workspace_of_the_same_name() {
        let mut store = SessionStore::default();
        store.save_workspace(one_window("Daily", vec![WorkspaceTab::home()], 0));
        store.save_workspace(one_window(
            "Daily",
            vec![WorkspaceTab::home(), WorkspaceTab::home()],
            0,
        ));
        assert_eq!(store.workspaces.len(), 1);
        assert_eq!(tab_count(store.workspace("Daily").unwrap()), 2);
    }

    #[test]
    fn an_unsaved_tab_carries_its_whole_session() {
        let session = SavedSession::new(
            SessionId::from_raw("u1"),
            "Scratch",
            SessionKind::TextCompare,
        );
        let workspace = one_window("Work", vec![WorkspaceTab::unsaved(session)], 0);
        let WorkspaceTab::Unsaved { session, .. } = &workspace.windows[0].tabs[0] else {
            panic!("the tab lost its session");
        };
        assert_eq!(session.name, "Scratch");
    }

    #[test]
    fn a_manager_opened_for_a_save_carries_the_name() {
        let manager = WorkspaceManager::with_name(1, "Daily");
        assert_eq!(manager.name(), "Daily");
        assert!(manager.is_open());
        assert!(manager.selected().is_none());
    }

    #[test]
    fn selecting_a_workspace_fills_the_name_field() {
        let mut manager = WorkspaceManager::new(1);
        manager.select("Nightly");
        assert_eq!(manager.selected(), Some("Nightly"));
        assert_eq!(manager.name(), "Nightly");
    }
}
