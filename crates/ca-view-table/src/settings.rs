//! The panel that edits how the columns and the rows are compared.
//!
//! Every control writes into the settings the next comparison runs under. The
//! panel itself starts no work: it reports that something changed and the view
//! sends the affected stages to a worker.

use crate::jobs::TableSettings;
use crate::model::ColumnInfo;
use ca_table::align::RowAlignmentMode;
use ca_table::schema::{
    column_letter, ColumnAlignment, ColumnHandling, ColumnType, ManualColumnPair,
};

/// Width the type drop down is laid out in.
const TYPE_COMBO_WIDTH: f32 = 150.0;
/// Width the alignment drop downs are laid out in.
const ALIGN_COMBO_WIDTH: f32 = 200.0;

/// The column and row settings panel.
#[derive(Debug, Clone, Default)]
pub struct SettingsPanel {
    open: bool,
    selected: usize,
}

impl SettingsPanel {
    /// True when the panel is showing.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.open
    }

    /// Show or hide the panel.
    pub fn set_open(&mut self, open: bool) {
        self.open = open;
    }

    /// Show the panel, or hide it if it is already showing.
    pub fn toggle(&mut self) {
        self.open = !self.open;
    }

    /// The comparison column the panel is editing.
    #[must_use]
    pub const fn selected(&self) -> usize {
        self.selected
    }

    /// Edit a different comparison column.
    pub fn select(&mut self, column: usize) {
        self.selected = column;
    }

    /// Lay the panel out. Returns true when a setting changed.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        id: egui::Id,
        settings: &mut TableSettings,
        columns: &[ColumnInfo],
    ) -> bool {
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.heading("Comparison settings");
            if ui.button("Close").clicked() {
                self.open = false;
            }
        });
        ui.separator();
        egui::ScrollArea::vertical()
            .id_salt(id.with("settings-scroll"))
            .show(ui, |ui| {
                changed |= self.column_section(ui, id, settings, columns);
                ui.separator();
                changed |= default_section(ui, id, settings);
                ui.separator();
                changed |= row_section(ui, id, settings);
            });
        changed
    }

    fn column_section(
        &mut self,
        ui: &mut egui::Ui,
        id: egui::Id,
        settings: &mut TableSettings,
        columns: &[ColumnInfo],
    ) -> bool {
        let mut changed = false;
        ui.label("Columns");
        changed |= column_alignment(ui, id, settings, columns);
        if columns.is_empty() {
            ui.label("No columns yet.");
            return changed;
        }
        self.selected = self.selected.min(columns.len() - 1);
        egui::ComboBox::from_id_salt(id.with("column-pick"))
            .width(ALIGN_COMBO_WIDTH)
            .selected_text(column_summary(columns, self.selected))
            .show_ui(ui, |ui| {
                for index in 0..columns.len() {
                    ui.selectable_value(&mut self.selected, index, column_summary(columns, index));
                }
            });
        if let Some(info) = columns.get(self.selected) {
            ui.label(format!(
                "Left {} against right {}, read as {}",
                letter_or_none(&info.left_letter),
                letter_or_none(&info.right_letter),
                info.type_label
            ));
        }
        let index = u32::try_from(self.selected).unwrap_or(0);
        let handling = settings.schema.handling.entry(index).or_default();
        changed |= handling_controls(ui, id.with("column"), handling, false);
        if matches!(settings.schema.alignment, ColumnAlignment::Custom) {
            ui.separator();
            changed |= custom_pairs(ui, id, settings);
        }
        changed
    }
}

fn column_alignment(
    ui: &mut egui::Ui,
    id: egui::Id,
    settings: &mut TableSettings,
    columns: &[ColumnInfo],
) -> bool {
    let mut choice = settings.schema.alignment.clone();
    let before = choice.clone();
    egui::ComboBox::from_id_salt(id.with("column-alignment"))
        .width(ALIGN_COMBO_WIDTH)
        .selected_text(alignment_label(&choice))
        .show_ui(ui, |ui| {
            for option in [
                ColumnAlignment::Unaligned,
                ColumnAlignment::ByLeftName,
                ColumnAlignment::ByRightName,
                ColumnAlignment::Custom,
            ] {
                let label = alignment_label(&option);
                ui.selectable_value(&mut choice, option, label);
            }
        });
    if choice == before {
        return false;
    }
    if choice == ColumnAlignment::Custom {
        settings.schema.custom = seed_custom_pairs(columns);
    }
    settings.schema.alignment = choice;
    true
}

fn seed_custom_pairs(columns: &[ColumnInfo]) -> Vec<ManualColumnPair> {
    columns
        .iter()
        .map(|column| ManualColumnPair {
            left: column.left_index,
            right: column.right_index,
            ..ManualColumnPair::default()
        })
        .collect()
}

/// The hand written column pairs, with the controls that reorder them.
fn custom_pairs(ui: &mut egui::Ui, id: egui::Id, settings: &mut TableSettings) -> bool {
    let mut changed = false;
    ui.label("Custom column pairs");
    let mut move_up: Option<usize> = None;
    let mut move_down: Option<usize> = None;
    let mut remove: Option<usize> = None;
    for (index, pair) in settings.schema.custom.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            changed |= file_column(ui, id.with(("pair-left", index)), "Left", &mut pair.left);
            changed |= file_column(ui, id.with(("pair-right", index)), "Right", &mut pair.right);
            if ui.button("Up").clicked() {
                move_up = Some(index);
            }
            if ui.button("Down").clicked() {
                move_down = Some(index);
            }
            if ui.button("Remove").clicked() {
                remove = Some(index);
            }
        });
    }
    ui.horizontal(|ui| {
        if ui.button("Add pair").clicked() {
            settings.schema.custom.push(ManualColumnPair::default());
            changed = true;
        }
        if ui.button("Tidy").clicked() {
            settings
                .schema
                .custom
                .retain(|pair| pair.left.is_some() || pair.right.is_some());
            changed = true;
        }
    });
    if let Some(index) = move_up {
        if index > 0 {
            settings.schema.custom.swap(index, index - 1);
            changed = true;
        }
    }
    if let Some(index) = move_down {
        if index + 1 < settings.schema.custom.len() {
            settings.schema.custom.swap(index, index + 1);
            changed = true;
        }
    }
    if let Some(index) = remove {
        if index < settings.schema.custom.len() {
            settings.schema.custom.remove(index);
            changed = true;
        }
    }
    changed
}

/// One file column number, where zero stands for an unmapped side.
fn file_column(ui: &mut egui::Ui, id: egui::Id, label: &str, value: &mut Option<u32>) -> bool {
    let mut number = i64::from(value.map_or(0, |column| column.saturating_add(1)));
    let before = number;
    ui.label(label);
    ui.push_id(id, |ui| {
        ui.add(
            egui::DragValue::new(&mut number)
                .speed(0.2)
                .range(0..=4_096)
                .custom_formatter(|number, _| {
                    #[allow(clippy::cast_possible_truncation)]
                    let number = number as i64;
                    if number <= 0 {
                        "none".to_string()
                    } else {
                        column_letter(u32::try_from(number - 1).unwrap_or(0))
                    }
                }),
        )
        .on_hover_text("The file column this side takes, or none");
    });
    if number == before {
        return false;
    }
    *value = if number <= 0 {
        None
    } else {
        u32::try_from(number - 1).ok()
    };
    true
}

fn default_section(ui: &mut egui::Ui, id: egui::Id, settings: &mut TableSettings) -> bool {
    ui.label("Default column handling");
    handling_controls(
        ui,
        id.with("default"),
        &mut settings.schema.default_handling,
        true,
    )
}

/// The per-column controls. `is_default` drops the two controls that make no
/// sense for the set every column inherits from.
fn handling_controls(
    ui: &mut egui::Ui,
    id: egui::Id,
    handling: &mut ColumnHandling,
    is_default: bool,
) -> bool {
    let before = handling.clone();
    ui.horizontal_wrapped(|ui| {
        if !is_default {
            ui.checkbox(&mut handling.key, "Key")
                .on_hover_text("Rows align only where every key column matches");
            ui.checkbox(&mut handling.use_default, "Use default");
        }
        ui.checkbox(&mut handling.unimportant, "Unimportant");
    });
    ui.horizontal_wrapped(|ui| {
        ui.label("Type");
        let mut column_type = handling.column_type.clone();
        egui::ComboBox::from_id_salt(id.with("type"))
            .width(TYPE_COMBO_WIDTH)
            .selected_text(crate::source::type_label(&column_type))
            .show_ui(ui, |ui| {
                for option in [
                    ColumnType::General,
                    ColumnType::Text,
                    ColumnType::Number,
                    ColumnType::DateTime,
                ] {
                    let label = crate::source::type_label(&option);
                    ui.selectable_value(&mut column_type, option, label);
                }
            });
        handling.column_type = column_type;
    });
    ui.horizontal_wrapped(|ui| {
        ui.label("Text important except for");
        ui.checkbox(&mut handling.ignore_case, "Character case");
        ui.checkbox(&mut handling.ignore_whitespace, "Whitespace");
    });
    ui.horizontal_wrapped(|ui| {
        ui.label("Numeric tolerance");
        ui.add(
            egui::DragValue::new(&mut handling.numeric_tolerance)
                .speed(0.01)
                .range(0.0..=f64::MAX),
        );
        ui.label("Date tolerance, seconds");
        ui.add(
            egui::DragValue::new(&mut handling.date_tolerance_seconds)
                .speed(1.0)
                .range(0.0..=f64::MAX),
        );
    });
    *handling != before
}

fn row_section(ui: &mut egui::Ui, id: egui::Id, settings: &mut TableSettings) -> bool {
    let before = settings.align.clone();
    ui.label("Rows");
    let mut mode = settings.align.mode.clone();
    egui::ComboBox::from_id_salt(id.with("row-mode"))
        .width(ALIGN_COMBO_WIDTH)
        .selected_text(mode_label(&mode))
        .show_ui(ui, |ui| {
            for option in [
                RowAlignmentMode::Unaligned,
                RowAlignmentMode::Standard,
                RowAlignmentMode::Myers,
                RowAlignmentMode::Patience,
            ] {
                let label = mode_label(&option);
                ui.selectable_value(&mut mode, option, label);
            }
        });
    settings.align.mode = mode;
    ui.horizontal_wrapped(|ui| {
        ui.checkbox(
            &mut settings.align.never_align_differences,
            "Never align differences",
        );
        ui.checkbox(
            &mut settings.align.use_closeness_matching,
            "Use closeness matching",
        );
        ui.checkbox(
            &mut settings.align.sort_rows_before_alignment,
            "Sort rows before alignment",
        );
    });
    ui.horizontal_wrapped(|ui| {
        let mut bounded = settings.align.skew_tolerance.is_some();
        ui.checkbox(&mut bounded, "Limit skew tolerance");
        let mut skew = settings.align.skew_tolerance.unwrap_or(200);
        if bounded {
            ui.add(
                egui::DragValue::new(&mut skew)
                    .speed(1.0)
                    .range(1..=100_000),
            );
            settings.align.skew_tolerance = Some(skew);
        } else {
            settings.align.skew_tolerance = None;
        }
    });
    settings.align != before
}

/// A column's line in the picker: its name and the two file columns it pairs.
#[must_use]
pub fn column_summary(columns: &[ColumnInfo], index: usize) -> String {
    let Some(info) = columns.get(index) else {
        return String::new();
    };
    let mut summary = format!(
        "{}. {} [{} / {}]",
        index + 1,
        info.name,
        letter_or_none(&info.left_letter),
        letter_or_none(&info.right_letter)
    );
    if info.key {
        summary.push_str(" key");
    }
    if info.unimportant {
        summary.push_str(" unimportant");
    }
    summary
}

fn letter_or_none(letter: &str) -> &str {
    if letter.is_empty() {
        "none"
    } else {
        letter
    }
}

/// What a column alignment choice is called on screen.
#[must_use]
pub fn alignment_label(alignment: &ColumnAlignment) -> &'static str {
    match alignment {
        ColumnAlignment::Unaligned => "Unaligned",
        ColumnAlignment::ByLeftName => "Align by left name",
        ColumnAlignment::ByRightName => "Align by right name",
        ColumnAlignment::Custom => "Custom",
        _ => "Unrecognized",
    }
}

/// What a row alignment mode is called on screen.
#[must_use]
pub fn mode_label(mode: &RowAlignmentMode) -> &'static str {
    match mode {
        RowAlignmentMode::Unaligned => "Unaligned",
        RowAlignmentMode::Standard => "Standard",
        RowAlignmentMode::Myers => "Myers",
        RowAlignmentMode::Patience => "Patience",
        _ => "Unrecognized",
    }
}

#[cfg(test)]
mod tests {
    use super::{alignment_label, column_summary, mode_label};
    use crate::model::ColumnInfo;
    use ca_table::align::RowAlignmentMode;
    use ca_table::schema::{ColumnAlignment, ManualColumnPair};

    fn info(key: bool) -> ColumnInfo {
        ColumnInfo {
            name: "id".to_string(),
            left_name: String::new(),
            right_name: String::new(),
            left_letter: "A".to_string(),
            right_letter: String::new(),
            left_index: Some(0),
            right_index: None,
            key,
            unimportant: false,
            type_label: "Number",
        }
    }

    #[test]
    fn a_column_line_names_both_file_columns() {
        let columns = vec![info(true)];
        assert_eq!(column_summary(&columns, 0), "1. id [A / none] key");
        assert_eq!(column_summary(&columns, 4), "");
    }

    #[test]
    fn every_alignment_choice_has_a_label() {
        for choice in [
            ColumnAlignment::Unaligned,
            ColumnAlignment::ByLeftName,
            ColumnAlignment::ByRightName,
            ColumnAlignment::Custom,
        ] {
            assert!(!alignment_label(&choice).is_empty());
        }
        for mode in [
            RowAlignmentMode::Unaligned,
            RowAlignmentMode::Standard,
            RowAlignmentMode::Myers,
            RowAlignmentMode::Patience,
        ] {
            assert!(!mode_label(&mode).is_empty());
        }
    }

    #[test]
    fn choosing_custom_starts_with_the_current_column_pairs() {
        let mut first = info(false);
        first.left_index = Some(0);
        first.right_index = Some(1);
        let mut second = info(false);
        second.left_index = Some(1);
        second.right_index = Some(0);
        assert_eq!(
            super::seed_custom_pairs(&[first, second]),
            vec![
                ManualColumnPair {
                    left: Some(0),
                    right: Some(1),
                    ..ManualColumnPair::default()
                },
                ManualColumnPair {
                    left: Some(1),
                    right: Some(0),
                    ..ManualColumnPair::default()
                },
            ]
        );
    }
}
