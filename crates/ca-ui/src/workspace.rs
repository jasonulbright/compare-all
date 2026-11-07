//! Named sets of open tabs, and the dialog that manages them.
//!
//! A workspace holds the tabs of every window and which tab each window was
//! showing. Saving one takes a snapshot of the open tabs; loading one closes
//! what is open and reopens the snapshot.

use crate::spacing::TOKENS;
use ca_session::{SessionStore, Workspace, WorkspaceTab, WorkspaceWindow};

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
    focus_requested: Option<WorkspaceFocus>,
    accept_focused_text: Option<(WorkspaceFocus, egui::Id)>,
    id: egui::Id,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkspaceFocus {
    Name,
    Rename,
    Keep,
}

enum KeyHandling {
    Continue,
    Return(Option<WorkspaceAction>),
}

impl WorkspaceManager {
    /// A manager with nothing selected and an empty name field.
    #[must_use]
    pub fn new(instance: u64) -> Self {
        Self {
            name: String::new(),
            selected: None,
            renaming: None,
            confirming: None,
            open: true,
            focus_requested: Some(WorkspaceFocus::Name),
            accept_focused_text: None,
            id: egui::Id::new(("workspaces", instance)),
        }
    }

    /// A manager opened with the name field filled, for Save Workspace As.
    #[must_use]
    pub fn with_name(instance: u64, name: impl Into<String>) -> Self {
        let mut manager = Self::new(instance);
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

    fn clear_focus_request(&mut self, target: WorkspaceFocus) {
        if self.focus_requested == Some(target) {
            self.focus_requested = None;
        }
    }

    fn handle_key(
        &mut self,
        action: crate::widgets::DialogKeyAction,
        focused: Option<egui::Id>,
    ) -> KeyHandling {
        match action {
            crate::widgets::DialogKeyAction::Cancel => {
                if self.confirming.take().is_some() {
                    self.clear_focus_request(WorkspaceFocus::Keep);
                    self.accept_focused_text = None;
                } else if self.renaming.take().is_some() {
                    self.clear_focus_request(WorkspaceFocus::Rename);
                    self.accept_focused_text = None;
                } else {
                    self.close();
                }
                KeyHandling::Return(None)
            }
            crate::widgets::DialogKeyAction::Accept => {
                if self.confirming.take().is_some() {
                    // Keep is the focused safe default when the delete
                    // question opens, so Enter dismisses the question.
                    self.clear_focus_request(WorkspaceFocus::Keep);
                    self.accept_focused_text = None;
                    return KeyHandling::Return(None);
                }
                if self.accept_focused_text.is_some_and(|(target, id)| {
                    target == WorkspaceFocus::Rename && focused == Some(id)
                }) {
                    if let Some((from, to)) = self.renaming.take() {
                        self.clear_focus_request(WorkspaceFocus::Rename);
                        self.accept_focused_text = None;
                        return KeyHandling::Return(Some(WorkspaceAction::Rename { from, to }));
                    }
                }
                let name = self.name.trim();
                if name.is_empty() {
                    KeyHandling::Continue
                } else {
                    KeyHandling::Return(Some(WorkspaceAction::Save(name.to_owned())))
                }
            }
        }
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
            .max_width((ctx.screen_rect().width() - TOKENS.forms.dialog_screen_margin).max(200.0))
            .show(ctx, |ui| {
                action = self.body(ui, store);
            });
        if !open {
            self.open = false;
        }
        action
    }

    /// Draw the dialog into a panel, for a headless frame.
    #[allow(clippy::too_many_lines)]
    pub fn body(&mut self, ui: &mut egui::Ui, store: &SessionStore) -> Option<WorkspaceAction> {
        let focused_text = self.accept_focused_text.map(|(_, id)| id);
        let focused = ui.ctx().memory(egui::Memory::focused);
        if let Some(action) =
            crate::widgets::dialog_key_action_with_text_focus(ui.ctx(), focused_text)
        {
            match self.handle_key(action, focused) {
                KeyHandling::Continue => {}
                KeyHandling::Return(action) => return action,
            }
        }
        let mut action = None;
        if let Some(name) = self.confirming.clone() {
            let text = format!("Delete the workspace {name}? The open tabs are not affected.");
            crate::widgets::wrapped_text(ui, &text);
            ui.horizontal_wrapped(|ui| {
                let (delete, keep) = crate::widgets::dialog_button_pair(ui, "Delete", "Keep");
                if self.focus_requested == Some(WorkspaceFocus::Keep) {
                    keep.request_focus();
                    self.clear_focus_request(WorkspaceFocus::Keep);
                }
                if delete.clicked() {
                    action = Some(WorkspaceAction::Delete(name.clone()));
                    self.confirming = None;
                    self.clear_focus_request(WorkspaceFocus::Keep);
                }
                if keep.clicked() {
                    self.confirming = None;
                    self.clear_focus_request(WorkspaceFocus::Keep);
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
                        crate::widgets::sized(ui, TOKENS.forms.workspace_name_width, |ui| {
                            let response = ui.selectable_label(
                                chosen,
                                crate::widgets::elide_to_width(
                                    ui,
                                    &workspace.name,
                                    TOKENS.forms.workspace_name_width
                                        - TOKENS.forms.workspace_name_elision_padding,
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
                            self.focus_requested = Some(WorkspaceFocus::Rename);
                        }
                        if ui.small_button("Delete").clicked() {
                            self.confirming = Some(workspace.name.clone());
                            self.focus_requested = Some(WorkspaceFocus::Keep);
                            self.accept_focused_text = None;
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
            let mut clear_rename_focus_request = false;
            ui.horizontal_wrapped(|ui| {
                crate::widgets::sized(ui, TOKENS.forms.dialog_label_width, |ui| {
                    ui.label("New name");
                });
                crate::widgets::sized(ui, TOKENS.forms.workspace_name_width, |ui| {
                    let response = ui.add(
                        egui::TextEdit::singleline(to)
                            .desired_width(TOKENS.forms.workspace_name_width),
                    );
                    if self.focus_requested == Some(WorkspaceFocus::Rename) {
                        response.request_focus();
                        self.accept_focused_text = Some((WorkspaceFocus::Rename, response.id));
                        clear_rename_focus_request = true;
                    } else if response.has_focus() || response.clicked() {
                        self.accept_focused_text = Some((WorkspaceFocus::Rename, response.id));
                    }
                });
                if ui.button("Rename").clicked() {
                    action = Some(WorkspaceAction::Rename {
                        from,
                        to: to.clone(),
                    });
                }
            });
            if clear_rename_focus_request {
                self.clear_focus_request(WorkspaceFocus::Rename);
            }
            if action.is_some() || ui.button("Cancel rename").clicked() {
                self.renaming = None;
                self.clear_focus_request(WorkspaceFocus::Rename);
                self.accept_focused_text = None;
            }
        }
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            crate::widgets::sized(ui, TOKENS.forms.dialog_label_width, |ui| {
                ui.label("Name");
            });
            crate::widgets::sized(ui, TOKENS.forms.workspace_name_width, |ui| {
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.name)
                        .desired_width(TOKENS.forms.workspace_name_width),
                );
                if self.focus_requested == Some(WorkspaceFocus::Name) {
                    response.request_focus();
                    self.accept_focused_text = Some((WorkspaceFocus::Name, response.id));
                    self.clear_focus_request(WorkspaceFocus::Name);
                } else if response.has_focus() || response.clicked() {
                    self.accept_focused_text = Some((WorkspaceFocus::Name, response.id));
                }
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
    use super::{one_window, tab_count, WorkspaceFocus, WorkspaceManager};
    use ca_session::{SavedSession, SessionId, SessionKind, SessionStore, WorkspaceTab};

    fn key_input(key: egui::Key) -> egui::RawInput {
        crate::testing::event_input(
            800.0,
            600.0,
            vec![egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        )
    }

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

    #[test]
    fn enter_keeps_the_safe_default_on_a_workspace_delete_question() {
        let mut manager = WorkspaceManager::new(1);
        manager.confirming = Some("Daily".to_owned());
        let store = SessionStore::default();
        let ctx = egui::Context::default();
        let mut action = None;
        let _ = ctx.run(key_input(egui::Key::Enter), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                action = manager.body(ui, &store);
            });
        });
        assert_eq!(action, None);
        assert!(manager.confirming.is_none());
    }

    #[test]
    fn canceling_a_delete_question_clears_the_previous_text_target() {
        let mut manager = WorkspaceManager::with_name(1, "Daily");
        manager.renaming = Some(("Old".to_owned(), "New".to_owned()));
        manager.confirming = Some("Old".to_owned());
        manager.focus_requested = Some(WorkspaceFocus::Keep);
        manager.accept_focused_text = Some((WorkspaceFocus::Rename, egui::Id::new("rename")));
        let store = SessionStore::default();
        let ctx = egui::Context::default();
        let mut action = None;

        let _ = ctx.run(key_input(egui::Key::Escape), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                action = manager.body(ui, &store);
            });
        });
        assert_eq!(action, None);
        assert!(manager.renaming.is_some());
        assert!(manager.accept_focused_text.is_none());

        let _ = ctx.run(key_input(egui::Key::Enter), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                action = manager.body(ui, &store);
            });
        });
        assert_eq!(
            action,
            Some(super::WorkspaceAction::Save("Daily".to_owned()))
        );
    }

    #[test]
    fn enter_saves_a_named_workspace_when_no_question_is_open() {
        let mut manager = WorkspaceManager::with_name(1, "Daily");
        let store = SessionStore::default();
        let ctx = egui::Context::default();
        let mut action = None;
        let _ = ctx.run(key_input(egui::Key::Enter), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                action = manager.body(ui, &store);
            });
        });
        assert_eq!(
            action,
            Some(super::WorkspaceAction::Save("Daily".to_owned()))
        );
    }

    #[test]
    fn enter_saves_from_the_initially_focused_workspace_name_field() {
        let mut manager = WorkspaceManager::with_name(1, "Daily");
        let store = SessionStore::default();
        let ctx = egui::Context::default();
        let _ = ctx.run(crate::testing::sized_input(800.0, 600.0), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = manager.body(ui, &store);
            });
        });
        let mut action = None;
        let _ = ctx.run(key_input(egui::Key::Enter), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                action = manager.body(ui, &store);
            });
        });
        assert_eq!(
            action,
            Some(super::WorkspaceAction::Save("Daily".to_owned()))
        );
    }

    #[test]
    fn enter_saves_the_name_field_while_a_rename_row_is_open() {
        let mut manager = WorkspaceManager::with_name(1, "Daily");
        manager.renaming = Some(("Old".to_owned(), "New".to_owned()));
        manager.focus_requested = Some(WorkspaceFocus::Name);
        let store = SessionStore::default();
        let ctx = egui::Context::default();
        let _ = ctx.run(crate::testing::sized_input(800.0, 600.0), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = manager.body(ui, &store);
            });
        });
        assert!(matches!(
            manager.accept_focused_text,
            Some((WorkspaceFocus::Name, _))
        ));
        let mut action = None;

        let _ = ctx.run(key_input(egui::Key::Enter), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                action = manager.body(ui, &store);
            });
        });

        assert_eq!(
            action,
            Some(super::WorkspaceAction::Save("Daily".to_owned()))
        );
        assert!(manager.renaming.is_some());
    }

    #[test]
    fn enter_renames_when_the_rename_field_owns_focus() {
        let mut manager = WorkspaceManager::with_name(1, "Daily");
        manager.renaming = Some(("Old".to_owned(), "New".to_owned()));
        manager.focus_requested = Some(WorkspaceFocus::Rename);
        let store = SessionStore::default();
        let ctx = egui::Context::default();
        let _ = ctx.run(crate::testing::sized_input(800.0, 600.0), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = manager.body(ui, &store);
            });
        });
        assert!(matches!(
            manager.accept_focused_text,
            Some((WorkspaceFocus::Rename, _))
        ));
        let mut action = None;

        let _ = ctx.run(key_input(egui::Key::Enter), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                action = manager.body(ui, &store);
            });
        });

        assert_eq!(
            action,
            Some(super::WorkspaceAction::Rename {
                from: "Old".to_owned(),
                to: "New".to_owned(),
            })
        );
        assert!(manager.renaming.is_none());
    }

    #[test]
    fn enter_without_text_focus_does_not_rename_from_a_stale_target() {
        let mut manager = WorkspaceManager::with_name(1, "Daily");
        manager.renaming = Some(("Old".to_owned(), "New".to_owned()));
        manager.accept_focused_text =
            Some((WorkspaceFocus::Rename, egui::Id::new("stale rename target")));
        let store = SessionStore::default();
        let ctx = egui::Context::default();
        let mut action = None;

        let _ = ctx.run(key_input(egui::Key::Enter), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                action = manager.body(ui, &store);
            });
        });

        assert_eq!(
            action,
            Some(super::WorkspaceAction::Save("Daily".to_owned()))
        );
        assert!(manager.renaming.is_some());
    }
}
