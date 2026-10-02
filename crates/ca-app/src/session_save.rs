//! Naming a session and choosing its destination before changing the store.

use ca_session::{SessionId, SessionStore, TreeNode};

/// A Save Session As request stays attached to the tab that opened it.
pub(crate) struct SessionSave {
    pub(crate) tab: usize,
    pub(crate) name: String,
    pub(crate) parent: Option<SessionId>,
    pub(crate) error: Option<String>,
    pub(crate) open: bool,
    focus_name: bool,
}

impl SessionSave {
    pub(crate) fn new(tab: usize, name: String) -> Self {
        Self {
            tab,
            name,
            parent: None,
            error: None,
            open: true,
            focus_name: true,
        }
    }

    /// Returns true only after the user requests a save. Cancellation has no
    /// store effects, and a refused save leaves the dialog and name visible.
    pub(crate) fn show(&mut self, ctx: &egui::Context, store: Option<&SessionStore>) -> bool {
        let mut save = false;
        let response = egui::Modal::new(egui::Id::new("save-session-as")).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.heading("Save Session As");
            ui.label("Session name");
            let name = ui.text_edit_singleline(&mut self.name);
            if self.focus_name {
                name.request_focus();
                self.focus_name = false;
            }
            ui.label("Save in");
            egui::ScrollArea::vertical()
                .max_height(240.0)
                .show(ui, |ui| {
                    if ui
                        .selectable_label(self.parent.is_none(), "<Root>")
                        .clicked()
                    {
                        self.parent = None;
                    }
                    if let Some(store) = store {
                        folders(ui, &store.root, &mut self.parent);
                    } else {
                        ui.label("Saved sessions are loading or saving. Please wait.");
                    }
                });
            if let Some(error) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            ui.separator();
            ui.horizontal(|ui| {
                save = ui
                    .add_enabled(
                        store.is_some() && !self.name.trim().is_empty(),
                        egui::Button::new("Save"),
                    )
                    .clicked();
                if ui.button("Cancel").clicked() {
                    self.open = false;
                }
            });
            if store.is_some()
                && !self.name.trim().is_empty()
                && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
            {
                save = true;
            }
        });
        if response.should_close() {
            self.open = false;
        }
        save && self.open
    }
}

fn folders(ui: &mut egui::Ui, nodes: &[TreeNode], picked: &mut Option<SessionId>) {
    for node in nodes {
        if let TreeNode::Folder {
            id, name, children, ..
        } = node
        {
            ui.push_id(id.as_str(), |ui| {
                if ui
                    .selectable_label(picked.as_ref() == Some(id), name)
                    .clicked()
                {
                    *picked = Some(id.clone());
                }
                ui.indent("children", |ui| folders(ui, children, picked));
            });
        }
    }
}
