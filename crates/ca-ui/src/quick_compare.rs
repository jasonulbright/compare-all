//! The summary window for a quick comparison.

use ca_session::SessionKind;

const VIEWS: &[SessionKind] = &[
    SessionKind::TextCompare,
    SessionKind::HexCompare,
    SessionKind::MediaCompare,
    SessionKind::PictureCompare,
    SessionKind::RegistryCompare,
    SessionKind::TableCompare,
    SessionKind::VersionCompare,
];

/// True when the summary offers `kind` in its view selector.
#[must_use]
pub fn supports_view(kind: &SessionKind) -> bool {
    VIEWS.contains(kind)
}

/// What the quick comparison window asks the caller to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuickCompareAction {
    /// Open the two paths in the selected comparison view.
    OpenView(SessionKind),
    /// Close the summary window.
    Close,
}

/// The quick comparison result and the view that can open next.
#[derive(Debug, Clone)]
pub struct QuickCompareDialog {
    left: String,
    right: String,
    result: Option<String>,
    selected: SessionKind,
}

impl QuickCompareDialog {
    /// A summary for the two displayed paths. Text Compare is the default view.
    #[must_use]
    pub fn new(left: impl Into<String>, right: impl Into<String>) -> Self {
        Self {
            left: left.into(),
            right: right.into(),
            result: None,
            selected: SessionKind::TextCompare,
        }
    }

    /// Set the initially selected view when the kind is offered.
    #[must_use]
    pub fn with_selected_view(mut self, kind: SessionKind) -> Self {
        if supports_view(&kind) {
            self.selected = kind;
        }
        self
    }

    /// The result text to show after the comparison finishes.
    pub fn set_result(&mut self, result: impl Into<String>) {
        self.result = Some(result.into());
    }

    /// The result text, once the comparison has finished.
    #[must_use]
    pub fn result(&self) -> Option<&str> {
        self.result.as_deref()
    }

    /// Draw the summary window and return one action when it closes or opens a view.
    pub fn show(&mut self, ctx: &egui::Context, id: egui::Id) -> Option<QuickCompareAction> {
        use crate::widgets::DialogKeyAction;

        match crate::widgets::dialog_key_action(ctx) {
            Some(DialogKeyAction::Cancel) => return Some(QuickCompareAction::Close),
            Some(DialogKeyAction::Accept) if self.result.is_some() => {
                return Some(QuickCompareAction::OpenView(self.selected.clone()));
            }
            Some(DialogKeyAction::Accept) | None => {}
        }

        let mut open = true;
        let mut action = None;
        egui::Window::new("Quick Compare")
            .id(id)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(&self.left);
                ui.label(&self.right);
                ui.add_space(crate::spacing::TOKENS.egui.item_spacing.y * 2.0);
                ui.label("Quick Compare:");
                if let Some(result) = &self.result {
                    ui.label(egui::RichText::new(result).strong());
                } else {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Comparing files…");
                    });
                }
                ui.add_space(crate::spacing::TOKENS.egui.item_spacing.y * 2.0);
                egui::ComboBox::new(id.with("view"), "View")
                    .selected_text(self.selected.title())
                    .show_ui(ui, |ui| {
                        for kind in VIEWS {
                            ui.selectable_value(&mut self.selected, kind.clone(), kind.title());
                        }
                    });
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(self.result.is_some(), egui::Button::new("Open View"))
                        .clicked()
                    {
                        action = Some(QuickCompareAction::OpenView(self.selected.clone()));
                    }
                    if ui.button("Close").clicked() {
                        action = Some(QuickCompareAction::Close);
                    }
                });
            });
        if !open {
            return Some(QuickCompareAction::Close);
        }
        action
    }
}
