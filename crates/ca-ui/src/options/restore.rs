//! The restore factory defaults wizard.
//!
//! The first page selects categories. The second page appears only when a
//! category that holds named items was selected, and states what a delete would
//! remove. A category this build has no store for is offered and marked, so the
//! wizard never claims to have reset something it did not.

use ca_session::options::{RestoreCategory, RestoreSelection};

/// Why a category is offered but does nothing.
const NOT_STORED: &str = "This build stores nothing for this category.";

/// What the wizard produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreOutcome {
    /// What to restore.
    pub selection: RestoreSelection,
}

/// The restore factory defaults wizard.
pub struct RestoreDialog {
    selection: RestoreSelection,
    second_page: bool,
    open: bool,
    id: egui::Id,
}

impl RestoreDialog {
    /// A wizard with nothing selected.
    #[must_use]
    pub fn new(instance: u64) -> Self {
        Self {
            selection: RestoreSelection::default(),
            second_page: false,
            open: true,
            id: egui::Id::new(("restore-defaults", instance)),
        }
    }

    /// True while the wizard is on screen.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.open
    }

    /// What is selected so far.
    #[must_use]
    pub const fn selection(&self) -> &RestoreSelection {
        &self.selection
    }

    /// Select or clear one category.
    pub fn set(&mut self, category: RestoreCategory, chosen: bool) {
        self.selection.categories.retain(|held| *held != category);
        if chosen {
            self.selection.categories.push(category);
        }
        if !self.selection.needs_second_page() {
            self.second_page = false;
        }
    }

    /// True when the second page is on screen.
    #[must_use]
    pub const fn on_second_page(&self) -> bool {
        self.second_page
    }

    /// Move to the second page, where the selection has one.
    pub fn next(&mut self) {
        if self.selection.needs_second_page() {
            self.second_page = true;
        }
    }

    /// Close without restoring anything.
    pub fn cancel(&mut self) {
        self.open = false;
    }

    /// Close and report what to restore.
    #[must_use]
    pub fn finish(&mut self) -> RestoreOutcome {
        self.open = false;
        RestoreOutcome {
            selection: self.selection.clone(),
        }
    }

    /// Draw the wizard and report what it produced.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<RestoreOutcome> {
        if !self.open {
            return None;
        }
        let mut outcome = None;
        let mut open = true;
        egui::Window::new("Restore Factory Defaults")
            .id(self.id)
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .show(ctx, |ui| {
                crate::widgets::notice_current(
                    ui,
                    crate::icons::Icon::Warning,
                    24.0,
                    "Restore Factory Defaults",
                );
                outcome = self.body(ui);
            });
        if !open {
            self.open = false;
        }
        outcome
    }

    /// Draw the wizard into a panel rather than a window.
    pub fn body(&mut self, ui: &mut egui::Ui) -> Option<RestoreOutcome> {
        if self.second_page {
            self.second_page_body(ui);
        } else {
            self.first_page_body(ui);
        }
        let mut outcome = None;
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            let needs_next = self.selection.needs_second_page() && !self.second_page;
            if ui
                .add_enabled(needs_next, egui::Button::new("Next"))
                .clicked()
            {
                self.next();
            }
            if ui
                .add_enabled(!needs_next, egui::Button::new("Finish"))
                .clicked()
            {
                outcome = Some(self.finish());
            }
            if ui.button("Cancel").clicked() {
                self.cancel();
            }
        });
        outcome
    }

    fn first_page_body(&mut self, ui: &mut egui::Ui) {
        crate::widgets::wrapped_text(ui, "Select what to put back to its built-in value.");
        for category in RestoreCategory::ALL {
            let mut chosen = self.selection.holds(*category);
            ui.horizontal_wrapped(|ui| {
                let response = ui.checkbox(&mut chosen, category.label());
                if !category.is_carried_out() {
                    response.on_hover_text(NOT_STORED);
                    ui.label("(not stored)");
                }
            });
            if chosen != self.selection.holds(*category) {
                self.set(*category, chosen);
            }
        }
    }

    fn second_page_body(&mut self, ui: &mut egui::Ui) {
        crate::widgets::wrapped_text(
            ui,
            "Select the named items to remove rather than reset. Removing cannot be undone.",
        );
        if self.selection.holds(RestoreCategory::Sessions) {
            ui.checkbox(
                &mut self.selection.delete_all_sessions,
                "Delete every stored session",
            );
        }
        if self.selection.holds(RestoreCategory::FileFormats) {
            let mut value = self.selection.delete_all_file_formats;
            let _ = crate::widgets::disabled_reason(
                ui.add_enabled(
                    false,
                    egui::Checkbox::new(&mut value, "Delete every customized file format"),
                ),
                NOT_STORED,
            );
        }
        if self.selection.holds(RestoreCategory::Profiles) {
            let mut value = self.selection.delete_all_profiles;
            let _ = crate::widgets::disabled_reason(
                ui.add_enabled(
                    false,
                    egui::Checkbox::new(&mut value, "Delete every named profile"),
                ),
                NOT_STORED,
            );
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::RestoreDialog;
    use ca_session::options::RestoreCategory;

    #[test]
    fn a_selection_without_named_items_needs_no_second_page() {
        let mut dialog = RestoreDialog::new(1);
        dialog.set(RestoreCategory::ProgramOptions, true);
        assert!(!dialog.selection().needs_second_page());
        dialog.next();
        assert!(!dialog.on_second_page());
    }

    #[test]
    fn selecting_sessions_opens_the_second_page_and_clearing_it_closes_again() {
        let mut dialog = RestoreDialog::new(1);
        dialog.set(RestoreCategory::Sessions, true);
        dialog.next();
        assert!(dialog.on_second_page());
        dialog.set(RestoreCategory::Sessions, false);
        assert!(!dialog.on_second_page());
    }

    #[test]
    fn finishing_reports_what_was_selected_and_closes() {
        let mut dialog = RestoreDialog::new(1);
        dialog.set(RestoreCategory::ThemeColorsFonts, true);
        let outcome = dialog.finish();
        assert!(outcome.selection.holds(RestoreCategory::ThemeColorsFonts));
        assert!(!dialog.is_open());
    }

    #[test]
    fn a_category_selected_twice_is_held_once() {
        let mut dialog = RestoreDialog::new(1);
        dialog.set(RestoreCategory::Sessions, true);
        dialog.set(RestoreCategory::Sessions, true);
        assert_eq!(dialog.selection().categories.len(), 1);
    }
}
