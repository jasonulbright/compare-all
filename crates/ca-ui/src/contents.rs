//! The choices shown before a folder content comparison starts.

use ca_fs::ContentMethod;
use std::path::PathBuf;

/// What the content comparison dialog asks its caller to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentsAction {
    /// Start the comparison with the selected options.
    Start {
        /// How to compare the file contents.
        method: ContentMethod,
        /// Open a results window when the comparison completes.
        show_results: bool,
        /// Save the choice to skip this dialog on later runs.
        dont_show_again: bool,
    },
    /// Close the dialog without starting a comparison.
    Cancel,
}

/// The method and choices for one content comparison.
#[derive(Debug, Clone)]
pub struct ContentsDialog {
    method: ContentMethod,
    show_results: bool,
    dont_show_again: bool,
}

impl Default for ContentsDialog {
    fn default() -> Self {
        Self {
            method: ContentMethod::Binary,
            show_results: false,
            dont_show_again: false,
        }
    }
}

impl ContentsDialog {
    /// Build the dialog with saved choices.
    #[must_use]
    pub const fn new(method: ContentMethod, show_results: bool) -> Self {
        Self {
            method,
            show_results,
            dont_show_again: false,
        }
    }

    /// The currently selected method.
    #[must_use]
    pub const fn method(&self) -> ContentMethod {
        self.method
    }

    /// Draw the dialog in its own window and return its choice.
    pub fn show(&mut self, ctx: &egui::Context, id: egui::Id) -> Option<ContentsAction> {
        use crate::widgets::DialogKeyAction;

        match crate::widgets::dialog_key_action(ctx) {
            Some(DialogKeyAction::Cancel) => return Some(ContentsAction::Cancel),
            Some(DialogKeyAction::Accept) => {
                return Some(self.start_action());
            }
            None => {}
        }

        let mut open = true;
        let mut action = None;
        egui::Window::new("Compare Contents")
            .id(id)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .open(&mut open)
            .show(ctx, |ui| action = self.body(ui));
        if !open {
            return Some(ContentsAction::Cancel);
        }
        action
    }

    /// Draw the dialog body in a native window.
    pub fn body(&mut self, ui: &mut egui::Ui) -> Option<ContentsAction> {
        let mut action = None;
        egui::ComboBox::from_label("Method")
            .selected_text(method_label(self.method))
            .show_ui(ui, |ui| {
                for method in [
                    ContentMethod::Crc32,
                    ContentMethod::Binary,
                    ContentMethod::Rules,
                ] {
                    ui.selectable_value(&mut self.method, method, method_label(method));
                }
            });
        ui.checkbox(&mut self.show_results, "Show results dialog");
        ui.checkbox(&mut self.dont_show_again, "Don't show again");
        ui.horizontal(|ui| {
            if ui.button("Start").clicked() {
                action = Some(self.start_action());
            }
            if ui.button("Cancel").clicked() {
                action = Some(ContentsAction::Cancel);
            }
        });
        action
    }

    fn start_action(&self) -> ContentsAction {
        ContentsAction::Start {
            method: self.method,
            show_results: self.show_results,
            dont_show_again: self.dont_show_again,
        }
    }
}

/// The group that holds one result row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ResultGroup {
    /// A file exists on the left side only.
    LeftOrphan,
    /// A file exists on the right side only.
    RightOrphan,
    /// The content comparison found differences.
    Different,
    /// The content comparison found only ignored differences.
    UnimportantDifferences,
    /// The content comparison found identical files.
    Same,
    /// A file could not be compared.
    NotCompared,
    /// A file comparison failed.
    Error,
}

impl ResultGroup {
    const fn index(self) -> usize {
        match self {
            Self::LeftOrphan => 0,
            Self::RightOrphan => 1,
            Self::Different => 2,
            Self::UnimportantDifferences => 3,
            Self::Same => 4,
            Self::NotCompared => 5,
            Self::Error => 6,
        }
    }

    fn label(self, count: usize) -> String {
        let name = match self {
            Self::LeftOrphan => "left orphan file(s)",
            Self::RightOrphan => "right orphan file(s)",
            Self::Different => "different file(s)",
            Self::UnimportantDifferences => "file(s) with unimportant differences",
            Self::Same => "identical file(s)",
            Self::NotCompared => "file(s) not compared",
            Self::Error => "file comparison error(s)",
        };
        format!("{count} {name}")
    }
}

/// One file listed in the content results window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultRow {
    /// File path relative to the compared roots.
    pub rel: PathBuf,
    /// File name shown in the results window.
    pub label: String,
    /// The results group that holds this file.
    pub group: ResultGroup,
    /// The file or pair the row opens.
    pub target: ResultTarget,
}

/// What a content result row opens when the user double-clicks it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultTarget {
    /// Open both files in their associated comparison view.
    Pair,
    /// Open the left-only file in Text Edit.
    Left,
    /// Open the right-only file in Text Edit.
    Right,
}

/// What a content results window asks its caller to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResultsAction {
    /// Open the file or pair at the stated path.
    Open {
        /// Path relative to the compared roots.
        rel: PathBuf,
        /// Which side or sides to open.
        target: ResultTarget,
    },
    /// Close the results window.
    Close,
}

/// The grouped results of one folder content comparison.
#[derive(Debug, Clone)]
pub struct ContentsResultsDialog {
    method: ContentMethod,
    rows: Vec<ResultRow>,
}

impl ContentsResultsDialog {
    /// Build a result window from rows collected by the content worker.
    #[must_use]
    pub fn new(method: ContentMethod, rows: Vec<ResultRow>) -> Self {
        Self { method, rows }
    }

    /// The native window title for the selected method.
    #[must_use]
    pub fn title(&self) -> &'static str {
        results_title(self.method)
    }

    /// The rows shown in the results window.
    #[must_use]
    pub fn rows(&self) -> &[ResultRow] {
        &self.rows
    }

    /// Draw the results window and return an action when a file is opened or
    /// the user closes the window.
    pub fn show(&mut self, ctx: &egui::Context, id: egui::Id) -> Option<ResultsAction> {
        if crate::widgets::dialog_key_action(ctx) == Some(crate::widgets::DialogKeyAction::Cancel) {
            return Some(ResultsAction::Close);
        }
        let mut open = true;
        let mut action = None;
        egui::Window::new(results_title(self.method))
            .id(id)
            .collapsible(true)
            .resizable(true)
            .default_size(egui::vec2(560.0, 420.0))
            .open(&mut open)
            .show(ctx, |ui| action = self.body(ui));
        if !open {
            return Some(ResultsAction::Close);
        }
        action
    }

    /// Draw the result list inside a native window.
    pub fn body(&mut self, ui: &mut egui::Ui) -> Option<ResultsAction> {
        const GROUPS: [ResultGroup; 7] = [
            ResultGroup::LeftOrphan,
            ResultGroup::RightOrphan,
            ResultGroup::Different,
            ResultGroup::UnimportantDifferences,
            ResultGroup::Same,
            ResultGroup::NotCompared,
            ResultGroup::Error,
        ];
        let mut grouped: [Vec<&ResultRow>; 7] = std::array::from_fn(|_| Vec::new());
        for row in &self.rows {
            grouped[row.group.index()].push(row);
        }

        let mut action = None;
        let row_height = ui.spacing().interact_size.y;
        let footer_height = ui.text_style_height(&egui::TextStyle::Body)
            + ui.spacing().interact_size.y
            + ui.spacing().item_spacing.y * 2.0;
        let results_height = (ui.available_height() - footer_height).max(0.0);
        egui::ScrollArea::vertical()
            .id_salt("contents-results-groups")
            .auto_shrink([false, false])
            .max_height(results_height)
            .show(ui, |ui| {
                for group in GROUPS {
                    let rows = &grouped[group.index()];
                    if rows.is_empty() {
                        continue;
                    }
                    egui::CollapsingHeader::new(group.label(rows.len()))
                        .id_salt(("contents-results-group", group.index()))
                        .default_open(true)
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical()
                                .id_salt(("contents-results-rows", group.index()))
                                .auto_shrink([false, false])
                                .max_height(240.0)
                                .show_rows(ui, row_height, rows.len(), |ui, range| {
                                    for index in range {
                                        let Some(row) = rows.get(index) else {
                                            continue;
                                        };
                                        let response = ui
                                            .push_id(&row.rel, |ui| {
                                                ui.selectable_label(false, &row.label)
                                            })
                                            .inner;
                                        if response.clicked() {
                                            response.request_focus();
                                        }
                                        let keyboard_open = response.has_focus()
                                            && ui.input_mut(|input| {
                                                input.consume_key(
                                                    egui::Modifiers::NONE,
                                                    egui::Key::Enter,
                                                ) || input.consume_key(
                                                    egui::Modifiers::NONE,
                                                    egui::Key::Space,
                                                )
                                            });
                                        if response.double_clicked() || keyboard_open {
                                            action = Some(ResultsAction::Open {
                                                rel: row.rel.clone(),
                                                target: row.target,
                                            });
                                        }
                                    }
                                });
                        });
                }
            });
        ui.label("Double-click a file or press Enter or Space to open it.");
        if ui.button("Close").clicked() {
            action = Some(ResultsAction::Close);
        }
        action
    }
}

fn results_title(method: ContentMethod) -> &'static str {
    match method {
        ContentMethod::Crc32 => "CRC Comparison Results",
        ContentMethod::Binary => "Binary Comparison Results",
        ContentMethod::Rules => "Rules-based Comparison Results",
    }
}

const fn method_label(method: ContentMethod) -> &'static str {
    match method {
        ContentMethod::Crc32 => "CRC comparison",
        ContentMethod::Binary => "Binary comparison",
        ContentMethod::Rules => "Rules-based comparison",
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{ContentsAction, ContentsDialog};
    use crate::testing::probe::Probe;
    use ca_fs::ContentMethod;

    fn draw(dialog: &mut ContentsDialog) -> impl FnMut(&egui::Context) + '_ {
        move |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                dialog.body(ui);
            });
        }
    }

    #[test]
    fn defaults_match_the_reference_choices() {
        let dialog = ContentsDialog::default();
        assert_eq!(dialog.method(), ContentMethod::Binary);
        let mut probe = Probe::new(600.0, 400.0);
        let mut dialog = dialog;
        let mut run = draw(&mut dialog);
        probe.idle(&mut run);
        assert!(probe.find("Show results dialog").unwrap().toggled == Some(false));
        assert!(probe.find("Don't show again").unwrap().toggled == Some(false));
    }

    #[test]
    fn every_method_and_checkbox_is_reachable_by_accessible_label() {
        let mut probe = Probe::new(600.0, 400.0);
        let mut dialog = ContentsDialog::default();
        {
            let mut run = draw(&mut dialog);
            probe.idle(&mut run);
            probe.click("Show results dialog", &mut run).unwrap();
            assert!(probe.find("Show results dialog").unwrap().toggled == Some(true));
            probe.click("Don't show again", &mut run).unwrap();
            assert!(probe.find("Don't show again").unwrap().toggled == Some(true));
            probe.click("Method", &mut run).unwrap();
            probe.click("Rules-based comparison", &mut run).unwrap();
        }
        assert_eq!(dialog.method(), ContentMethod::Rules);
    }

    #[test]
    fn start_returns_all_selected_choices() {
        let mut probe = Probe::new(600.0, 400.0);
        let mut dialog = ContentsDialog::new(ContentMethod::Crc32, true);
        let mut action = None;
        let mut run = |ctx: &egui::Context| {
            egui::CentralPanel::default().show(ctx, |ui| {
                if let Some(produced) = dialog.body(ui) {
                    action = Some(produced);
                }
            });
        };
        probe.idle(&mut run);
        probe.click("Don't show again", &mut run).unwrap();
        probe.click("Start", &mut run).unwrap();
        assert_eq!(
            action,
            Some(ContentsAction::Start {
                method: ContentMethod::Crc32,
                show_results: true,
                dont_show_again: true,
            })
        );
    }

    #[test]
    fn cancel_returns_without_starting() {
        let mut probe = Probe::new(600.0, 400.0);
        let mut dialog = ContentsDialog::default();
        let mut action = None;
        let mut run = |ctx: &egui::Context| {
            egui::CentralPanel::default().show(ctx, |ui| {
                if let Some(produced) = dialog.body(ui) {
                    action = Some(produced);
                }
            });
        };
        probe.idle(&mut run);
        probe.click("Cancel", &mut run).unwrap();
        assert_eq!(action, Some(ContentsAction::Cancel));
    }

    #[test]
    fn results_group_rows_open_on_double_click_and_close_by_label() {
        use super::{ContentsResultsDialog, ResultGroup, ResultRow, ResultTarget, ResultsAction};
        use std::path::PathBuf;

        let mut probe = Probe::new(640.0, 480.0);
        let mut dialog = ContentsResultsDialog::new(
            ContentMethod::Binary,
            vec![ResultRow {
                rel: PathBuf::from("nested/item.txt"),
                label: "nested\\item.txt".to_owned(),
                group: ResultGroup::Different,
                target: ResultTarget::Pair,
            }],
        );
        let mut action = None;
        {
            let mut run = |ctx: &egui::Context| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    if let Some(produced) = dialog.body(ui) {
                        action = Some(produced);
                    }
                });
            };
            probe.idle(&mut run);
            probe.click("1 different file(s)", &mut run).unwrap();
            let _header = probe.find("1 different file(s)").unwrap();
            probe.click("1 different file(s)", &mut run).unwrap();
            for _ in 0..45 {
                probe.idle(&mut run);
            }
            let item = probe.find("nested\\item.txt").unwrap();
            let point = item.rect.center();
            let button = |pressed| egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::default(),
            };
            probe.frame(
                vec![egui::Event::PointerMoved(point), button(true)],
                &mut run,
            );
            probe.frame(vec![button(false)], &mut run);
            probe.frame(
                vec![egui::Event::PointerMoved(point), button(true)],
                &mut run,
            );
            probe.frame(vec![button(false)], &mut run);
        }
        assert_eq!(
            action,
            Some(ResultsAction::Open {
                rel: PathBuf::from("nested/item.txt"),
                target: ResultTarget::Pair,
            })
        );
        {
            let mut run = |ctx: &egui::Context| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    if let Some(produced) = dialog.body(ui) {
                        action = Some(produced);
                    }
                });
            };
            probe.click("Close", &mut run).unwrap();
        }
        assert_eq!(action, Some(ResultsAction::Close));
    }

    #[test]
    fn results_rows_open_from_the_keyboard_only_when_focused() {
        use super::{ContentsResultsDialog, ResultGroup, ResultRow, ResultTarget, ResultsAction};
        use std::path::PathBuf;

        let mut probe = Probe::new(640.0, 480.0);
        let mut dialog = ContentsResultsDialog::new(
            ContentMethod::Binary,
            vec![
                ResultRow {
                    rel: PathBuf::from("first.txt"),
                    label: "first.txt".to_owned(),
                    group: ResultGroup::Different,
                    target: ResultTarget::Pair,
                },
                ResultRow {
                    rel: PathBuf::from("second.txt"),
                    label: "second.txt".to_owned(),
                    group: ResultGroup::Different,
                    target: ResultTarget::Left,
                },
            ],
        );
        let mut action = None;
        let mut run = |ctx: &egui::Context| {
            egui::CentralPanel::default().show(ctx, |ui| {
                action = dialog.body(ui);
            });
        };
        probe.idle(&mut run);
        let first = probe.find("first.txt").unwrap();
        probe.press_at(first.rect.center(), &mut run);
        let enter = egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        probe.frame(vec![enter], &mut run);
        assert_eq!(
            action,
            Some(ResultsAction::Open {
                rel: PathBuf::from("first.txt"),
                target: ResultTarget::Pair,
            })
        );
    }

    #[test]
    fn large_result_groups_scroll_and_keep_close_reachable() {
        use super::{ContentsResultsDialog, ResultGroup, ResultRow, ResultTarget};
        use std::path::PathBuf;

        let rows = (0..1_000)
            .map(|index| ResultRow {
                rel: PathBuf::from(format!("item-{index:04}.txt")),
                label: format!("item-{index:04}.txt"),
                group: ResultGroup::Different,
                target: ResultTarget::Pair,
            })
            .collect();
        let mut probe = Probe::new(640.0, 320.0);
        let mut dialog = ContentsResultsDialog::new(ContentMethod::Binary, rows);
        let mut run = |ctx: &egui::Context| {
            egui::CentralPanel::default().show(ctx, |ui| {
                dialog.body(ui);
            });
        };
        probe.idle(&mut run);

        let close = probe.find("Close").unwrap();
        assert!(close.rect.bottom() <= 320.0);
        assert!(probe.find("item-0000.txt").is_ok());
        assert!(probe.find("item-0999.txt").is_err());
    }

    #[test]
    fn orphan_rows_open_the_side_that_contains_the_file() {
        use super::{ContentsResultsDialog, ResultGroup, ResultRow, ResultTarget, ResultsAction};
        use std::path::PathBuf;

        let mut probe = Probe::new(640.0, 480.0);
        let mut dialog = ContentsResultsDialog::new(
            ContentMethod::Binary,
            vec![
                ResultRow {
                    rel: PathBuf::from("left-only.txt"),
                    label: "left-only.txt".to_owned(),
                    group: ResultGroup::LeftOrphan,
                    target: ResultTarget::Left,
                },
                ResultRow {
                    rel: PathBuf::from("right-only.txt"),
                    label: "right-only.txt".to_owned(),
                    group: ResultGroup::RightOrphan,
                    target: ResultTarget::Right,
                },
            ],
        );
        let action = std::cell::RefCell::new(None);
        let mut run = |ctx: &egui::Context| {
            egui::CentralPanel::default().show(ctx, |ui| {
                if let Some(produced) = dialog.body(ui) {
                    *action.borrow_mut() = Some(produced);
                }
            });
        };
        probe.idle(&mut run);

        for (label, target) in [
            ("left-only.txt", ResultTarget::Left),
            ("right-only.txt", ResultTarget::Right),
        ] {
            for _ in 0..45 {
                probe.idle(&mut run);
            }
            let item = probe.find(label).unwrap();
            let point = item.rect.center();
            let button = |pressed| egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::default(),
            };
            probe.frame(
                vec![egui::Event::PointerMoved(point), button(true)],
                &mut run,
            );
            probe.frame(vec![button(false)], &mut run);
            probe.frame(
                vec![egui::Event::PointerMoved(point), button(true)],
                &mut run,
            );
            probe.frame(vec![button(false)], &mut run);
            assert_eq!(
                action.borrow().as_ref(),
                Some(&ResultsAction::Open {
                    rel: PathBuf::from(label),
                    target,
                })
            );
            *action.borrow_mut() = None;
        }
    }
}
