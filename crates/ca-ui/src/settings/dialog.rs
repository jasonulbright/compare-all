//! The session settings dialog, drawn from the field description of any kind.

use super::field::{Field, FieldShape, FieldValue};
use super::schema::{self, Tab};
use crate::theme::Palette;
use ca_session::settings::table::ColumnHandling;
use ca_session::settings::{ReplacementItem, SessionSettings, SessionSettingsOverride};
use ca_session::{SessionKind, SettingsLayers};

/// Width the label column takes before the controls start.
const LABEL_WIDTH: f32 = 220.0;

/// Width a control takes when the window is at its narrowest.
const CONTROL_WIDTH: f32 = 180.0;

/// Room the dialog keeps for its own frame inside the window.
const DIALOG_MARGIN: f32 = 24.0;

/// Which layers an edit is written to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Scope {
    /// Write the edit to this session alone.
    #[default]
    ThisViewOnly,
    /// Write the edit to this session and to the defaults new sessions of the
    /// kind start from.
    UpdateSessionDefaults,
}

impl Scope {
    /// Both scopes, in the order the drop down lists them.
    pub const ALL: &'static [Scope] = &[Scope::ThisViewOnly, Scope::UpdateSessionDefaults];

    /// Label shown in the drop down.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Scope::ThisViewOnly => "Use for this view only",
            Scope::UpdateSessionDefaults => "Update session defaults",
        }
    }
}

/// What the dialog produced when it was accepted.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsOutcome {
    /// The settings as edited, every field populated.
    pub settings: SessionSettings,
    /// The same settings as an override that states only what differs from the
    /// layer below, so a later change to the defaults still reaches this
    /// session.
    pub overrides: SessionSettingsOverride,
    /// Which layers the edit was written to.
    pub scope: Scope,
}

/// The session settings dialog.
///
/// The dialog holds the settings being edited and the values the layer below
/// supplies, so every field can report whether it is inherited and can be put
/// back without reading the store again.
pub struct SettingsDialog {
    kind: SessionKind,
    current: SessionSettings,
    defaults: SessionSettings,
    tabs: &'static [Tab],
    active: usize,
    scope: Scope,
    open: bool,
    id: egui::Id,
}

impl SettingsDialog {
    /// A dialog over one session's settings.
    ///
    /// `layers` supplies the values the session inherits when it states none of
    /// its own.
    #[must_use]
    pub fn new(
        kind: SessionKind,
        overrides: &SessionSettingsOverride,
        layers: &SettingsLayers,
        instance: u64,
    ) -> Self {
        let defaults = layers.resolve_defaults(&kind);
        let current = layers
            .resolve(&kind, overrides)
            .unwrap_or_else(|_| defaults.clone());
        Self {
            tabs: schema::tabs_for(&kind),
            kind,
            current,
            defaults,
            active: 0,
            scope: Scope::ThisViewOnly,
            open: true,
            id: egui::Id::new(("session-settings", instance)),
        }
    }

    /// True while the dialog is on screen.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// The settings as they stand.
    #[must_use]
    pub fn settings(&self) -> &SessionSettings {
        &self.current
    }

    /// The values the layer below supplies.
    #[must_use]
    pub fn defaults(&self) -> &SessionSettings {
        &self.defaults
    }

    /// Which layers an accepted edit is written to.
    #[must_use]
    pub fn scope(&self) -> Scope {
        self.scope
    }

    /// Choose which layers an accepted edit is written to.
    pub fn set_scope(&mut self, scope: Scope) {
        self.scope = scope;
    }

    /// The pages this kind carries.
    #[must_use]
    pub fn tabs(&self) -> &'static [Tab] {
        self.tabs
    }

    /// Index of the page on screen.
    #[must_use]
    pub fn active_tab(&self) -> usize {
        self.active
    }

    /// Show the page at `index`.
    pub fn select_tab(&mut self, index: usize) {
        if index < self.tabs.len() {
            self.active = index;
        }
    }

    /// True when the field named by `key` states a value of its own rather than
    /// taking the one the layer below supplies.
    #[must_use]
    pub fn is_overridden(&self, key: &str) -> bool {
        self.field(key)
            .is_some_and(|field| field.is_overridden(&self.current, &self.defaults))
    }

    /// The field named by `key`, on any page.
    #[must_use]
    pub fn field(&self, key: &str) -> Option<&'static Field> {
        self.tabs
            .iter()
            .flat_map(|tab| tab.fields.iter())
            .find(|field| field.key == key)
    }

    /// Read one field of the settings being edited.
    #[must_use]
    pub fn value(&self, key: &str) -> Option<FieldValue> {
        self.field(key)?.read(&self.current)
    }

    /// Write one field of the settings being edited.
    ///
    /// A field no engine reads takes no edit, so a toolbar cannot write a
    /// value the comparison would ignore.
    pub fn set_value(&mut self, key: &str, value: &FieldValue) {
        if let Some(field) = self.field(key).filter(|field| field.is_bound()) {
            field.write(&mut self.current, value);
        }
    }

    /// Put one field back to the value the layer below supplies.
    pub fn reset_field(&mut self, key: &str) {
        let Some(field) = self.field(key) else {
            return;
        };
        if let Some(value) = field.read(&self.defaults) {
            field.write(&mut self.current, &value);
        }
    }

    /// Put every field of the page on screen back to the layer below.
    pub fn reset_page(&mut self) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        for field in tab.fields {
            if let Some(value) = field.read(&self.defaults) {
                field.write(&mut self.current, &value);
            }
        }
    }

    /// Put every field back to the layer below.
    pub fn reset_all(&mut self) {
        schema::reset_to(&mut self.current, &self.defaults);
    }

    /// The settings as an override stating only what differs from the layer
    /// below.
    #[must_use]
    pub fn to_override(&self) -> SessionSettingsOverride {
        schema::override_against(&self.current, &self.defaults)
    }

    /// The outcome an accepted dialog produces.
    #[must_use]
    pub fn accept(&mut self) -> SettingsOutcome {
        self.open = false;
        SettingsOutcome {
            settings: self.current.clone(),
            overrides: self.to_override(),
            scope: self.scope,
        }
    }

    /// Close the dialog without producing anything.
    pub fn cancel(&mut self) {
        self.open = false;
    }

    /// Draw the dialog and report the outcome once it is accepted.
    ///
    /// The whole dialog is laid out inside the width it is given, so a window
    /// at its minimum size still shows every control.
    pub fn show(&mut self, ctx: &egui::Context, palette: &Palette) -> Option<SettingsOutcome> {
        if !self.open {
            return None;
        }
        let mut outcome = None;
        let mut open = true;
        let width = (ctx.screen_rect().width() - DIALOG_MARGIN).max(CONTROL_WIDTH);
        egui::Window::new(format!("{} Session Settings", self.kind.title()))
            .id(self.id)
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .max_width(width)
            .show(ctx, |ui| {
                ui.set_max_width(width);
                outcome = self.body(ui, palette);
            });
        if !open {
            self.open = false;
        }
        outcome
    }

    /// Draw the dialog into a panel rather than a window, for a headless frame.
    pub fn body(&mut self, ui: &mut egui::Ui, palette: &Palette) -> Option<SettingsOutcome> {
        if self.tabs.is_empty() {
            crate::widgets::wrapped_text(ui, "This build has no settings for this session type.");
            return None;
        }
        self.tab_strip(ui);
        ui.separator();
        let fields = self.tabs.get(self.active).map_or(&[][..], |tab| tab.fields);
        let mut edits: Vec<(&'static Field, FieldValue)> = Vec::new();
        let mut resets: Vec<&'static str> = Vec::new();
        egui::ScrollArea::vertical()
            .id_salt(self.id.with(self.active))
            .max_height(ui.available_height() - 48.0)
            .show(ui, |ui| {
                for field in fields {
                    self.row(ui, palette, field, &mut edits, &mut resets);
                }
            });
        for (field, value) in edits {
            field.write(&mut self.current, &value);
        }
        for key in resets {
            self.reset_field(key);
        }
        ui.separator();
        self.footer(ui)
    }

    fn tab_strip(&mut self, ui: &mut egui::Ui) {
        let mut chosen = self.active;
        ui.horizontal_wrapped(|ui| {
            for (index, tab) in self.tabs.iter().enumerate() {
                if ui
                    .selectable_label(index == self.active, tab.name)
                    .clicked()
                {
                    chosen = index;
                }
            }
        });
        self.active = chosen;
    }

    /// One labeled control, with the marker saying where its value comes from.
    fn row(
        &self,
        ui: &mut egui::Ui,
        palette: &Palette,
        field: &'static Field,
        edits: &mut Vec<(&'static Field, FieldValue)>,
        resets: &mut Vec<&'static str>,
    ) {
        let Some(value) = field.read(&self.current) else {
            return;
        };
        let overridden = field.is_overridden(&self.current, &self.defaults);
        ui.horizontal_wrapped(|ui| {
            let label_width = LABEL_WIDTH.min((ui.available_width() - CONTROL_WIDTH).max(80.0));
            crate::widgets::sized(ui, label_width, |ui| {
                let text = egui::RichText::new(field.label).color(if overridden {
                    palette.settings_overridden
                } else {
                    palette.settings_inherited
                });
                ui.add(egui::Label::new(text).truncate())
                    .on_hover_text(match field.unavailable {
                        Some(reason) => reason,
                        None if overridden => "Stated by this session",
                        None => "Inherited from the session defaults",
                    });
            });
            if let Some(reason) = field.unavailable {
                // The control still shows the stored value, so the page reads
                // the same either way, but it takes no edit: a field no engine
                // reads must never look as though it changed something.
                let _ = ui.add_enabled_ui(false, |ui| {
                    control(ui, field, &value, Some(reason));
                });
                return;
            }
            if let Some(edited) = control(ui, field, &value, None) {
                edits.push((field, edited));
            }
            if overridden && ui.small_button("Reset").clicked() {
                resets.push(field.key);
            }
        });
    }

    fn footer(&mut self, ui: &mut egui::Ui) -> Option<SettingsOutcome> {
        let mut outcome = None;
        ui.horizontal_wrapped(|ui| {
            crate::widgets::sized(ui, 240.0, |ui| {
                egui::ComboBox::from_id_salt(self.id.with("scope"))
                    .selected_text(self.scope.label())
                    .show_ui(ui, |ui| {
                        for scope in Scope::ALL {
                            ui.selectable_value(&mut self.scope, *scope, scope.label());
                        }
                    });
            });
            if ui.button("Reset Page").clicked() {
                self.reset_page();
            }
            if ui.button("Reset All").clicked() {
                self.reset_all();
            }
            if ui.button("OK").clicked() {
                outcome = Some(self.accept());
            }
            if ui.button("Cancel").clicked() {
                self.cancel();
            }
        });
        outcome
    }
}

/// Draw the control for one field and report a new value when it changed.
fn control(
    ui: &mut egui::Ui,
    field: &Field,
    value: &FieldValue,
    reason: Option<&str>,
) -> Option<FieldValue> {
    match (field.shape, value) {
        (FieldShape::Flag, FieldValue::Flag(on)) => control_flag(ui, field, *on, reason),
        (FieldShape::Count, FieldValue::Count(number)) => control_count(ui, field, *number, reason),
        (FieldShape::Signed, FieldValue::Signed(number)) => {
            control_signed(ui, field, *number, reason)
        }
        (FieldShape::Decimal, FieldValue::Decimal(number)) => {
            control_decimal(ui, field, *number, reason)
        }
        (FieldShape::Text | FieldShape::Path, FieldValue::Text(text)) => {
            control_text(ui, field, text, reason)
        }
        (FieldShape::Choice, FieldValue::Choice(id)) => control_choice(ui, field, id, reason),
        (FieldShape::Lines, FieldValue::Lines(lines)) => control_lines(ui, field, lines, reason),
        (FieldShape::Replacements, FieldValue::Replacements(items)) => {
            replacement_table(ui, field, items, reason)
        }
        (FieldShape::Columns, FieldValue::Columns(items)) => column_table(ui, items, reason),
        _ => {
            let response = ui.label(value.display());
            describe_field_control(response, field.label, egui::WidgetType::Label, reason);
            None
        }
    }
}

fn field_control_width(ui: &egui::Ui) -> f32 {
    CONTROL_WIDTH.min(ui.available_width().max(60.0))
}

fn control_flag(
    ui: &mut egui::Ui,
    field: &Field,
    selected: bool,
    reason: Option<&str>,
) -> Option<FieldValue> {
    let mut selected = selected;
    let response = ui.checkbox(&mut selected, "");
    let changed = response.changed();
    describe_field_checkbox(response, field.label, selected, reason);
    changed.then_some(FieldValue::Flag(selected))
}

fn control_count(
    ui: &mut egui::Ui,
    field: &Field,
    number: u64,
    reason: Option<&str>,
) -> Option<FieldValue> {
    let mut number = number;
    let response = crate::widgets::sized(ui, field_control_width(ui), |ui| {
        ui.add(egui::DragValue::new(&mut number).range(0..=field.limit))
    });
    let changed = response.changed();
    describe_field_control(response, field.label, egui::WidgetType::DragValue, reason);
    changed.then_some(FieldValue::Count(number))
}

fn control_signed(
    ui: &mut egui::Ui,
    field: &Field,
    number: i64,
    reason: Option<&str>,
) -> Option<FieldValue> {
    let mut number = number;
    let response = crate::widgets::sized(ui, field_control_width(ui), |ui| {
        ui.add(egui::DragValue::new(&mut number))
    });
    let changed = response.changed();
    describe_field_control(response, field.label, egui::WidgetType::DragValue, reason);
    changed.then_some(FieldValue::Signed(number))
}

fn control_decimal(
    ui: &mut egui::Ui,
    field: &Field,
    number: f64,
    reason: Option<&str>,
) -> Option<FieldValue> {
    let mut number = number;
    let response = crate::widgets::sized(ui, field_control_width(ui), |ui| {
        ui.add(egui::DragValue::new(&mut number).speed(0.01))
    });
    let changed = response.changed();
    describe_field_control(response, field.label, egui::WidgetType::DragValue, reason);
    changed.then_some(FieldValue::Decimal(number))
}

fn control_text(
    ui: &mut egui::Ui,
    field: &Field,
    value: &str,
    reason: Option<&str>,
) -> Option<FieldValue> {
    let width = field_control_width(ui);
    let mut text = value.to_owned();
    let response = crate::widgets::sized(ui, width, |ui| {
        ui.add(egui::TextEdit::singleline(&mut text).desired_width(width))
    });
    let changed = response.changed();
    describe_field_control(response, field.label, egui::WidgetType::TextEdit, reason);
    changed.then_some(FieldValue::Text(text))
}

fn control_choice(
    ui: &mut egui::Ui,
    field: &Field,
    id: &str,
    reason: Option<&str>,
) -> Option<FieldValue> {
    let mut chosen = id.to_owned();
    let label = field
        .choices
        .iter()
        .find(|choice| choice.id == id)
        .map_or(id, |choice| choice.label);
    let response = crate::widgets::sized(ui, field_control_width(ui), |ui| {
        egui::ComboBox::from_id_salt(ui.id().with(field.key))
            .selected_text(label)
            .show_ui(ui, |ui| {
                for choice in field.choices {
                    ui.selectable_value(&mut chosen, choice.id.to_owned(), choice.label);
                }
            })
            .response
    });
    describe_field_control(response, field.label, egui::WidgetType::ComboBox, reason);
    (chosen != id).then_some(FieldValue::Choice(chosen))
}

fn control_lines(
    ui: &mut egui::Ui,
    field: &Field,
    lines: &[String],
    reason: Option<&str>,
) -> Option<FieldValue> {
    let width = field_control_width(ui);
    let mut text = lines.join("\n");
    let response = crate::widgets::sized(ui, width, |ui| {
        ui.add(
            egui::TextEdit::multiline(&mut text)
                .desired_width(width)
                .desired_rows(2),
        )
    });
    let changed = response.changed();
    describe_field_control(response, field.label, egui::WidgetType::TextEdit, reason);
    changed.then(|| {
        FieldValue::Lines(
            text.lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect(),
        )
    })
}

fn describe_field_control(
    response: egui::Response,
    label: &str,
    widget_type: egui::WidgetType,
    reason: Option<&str>,
) {
    let enabled = response.enabled();
    response.widget_info(|| egui::WidgetInfo::labeled(widget_type, enabled, label));
    if let Some(reason) = reason {
        let _ = crate::widgets::disabled_reason(response, reason);
    }
}

fn describe_field_checkbox(
    response: egui::Response,
    label: &str,
    selected: bool,
    reason: Option<&str>,
) {
    let enabled = response.enabled();
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, enabled, selected, label)
    });
    if let Some(reason) = reason {
        let _ = crate::widgets::disabled_reason(response, reason);
    }
}

/// The per-column table, with one row per treatment and a button that adds one.
///
/// A treatment naming no column states the inherited one, which the engine
/// leaves out, so a new row starts on the first column rather than on none.
fn column_table(
    ui: &mut egui::Ui,
    items: &[ColumnHandling],
    reason: Option<&str>,
) -> Option<FieldValue> {
    let mut edited = items.to_vec();
    let mut changed = false;
    let mut remove: Option<usize> = None;
    ui.vertical(|ui| {
        for (index, item) in edited.iter_mut().enumerate() {
            ui.horizontal_wrapped(|ui| {
                let mut column = item.column.unwrap_or_default();
                let response = crate::widgets::sized(ui, 80.0, |ui| {
                    ui.add(egui::DragValue::new(&mut column).prefix("Column "))
                });
                changed |= response.changed();
                describe_unavailable(response, reason);
                item.column = Some(column);
                let response = ui.checkbox(&mut item.key, "Key");
                changed |= response.changed();
                describe_unavailable(response, reason);
                let response = ui.checkbox(&mut item.unimportant, "Unimportant");
                changed |= response.changed();
                describe_unavailable(response, reason);
                let response = ui.checkbox(&mut item.ignore_character_case, "Case");
                changed |= response.changed();
                describe_unavailable(response, reason);
                let response = ui.checkbox(&mut item.ignore_whitespace, "Whitespace");
                changed |= response.changed();
                describe_unavailable(response, reason);
                let response = ui
                    .small_button("Remove")
                    .on_hover_text("Drop this treatment");
                if response.clicked() {
                    remove = Some(index);
                }
                describe_unavailable(response, reason);
            });
        }
        let response = ui.button("Add column");
        if response.clicked() {
            edited.push(ColumnHandling {
                column: Some(0),
                use_default: false,
                ..ColumnHandling::default()
            });
            changed = true;
        }
        describe_unavailable(response, reason);
    });
    if let Some(index) = remove {
        edited.remove(index);
        changed = true;
    }
    changed.then_some(FieldValue::Columns(edited))
}

/// The substitutions table, with one row per rule and a button that adds one.
fn replacement_table(
    ui: &mut egui::Ui,
    field: &Field,
    items: &[ReplacementItem],
    reason: Option<&str>,
) -> Option<FieldValue> {
    let mut edited = items.to_vec();
    let mut changed = false;
    let mut remove: Option<usize> = None;
    ui.vertical(|ui| {
        for (index, item) in edited.iter_mut().enumerate() {
            ui.horizontal_wrapped(|ui| {
                let width = (ui.available_width() * 0.3).clamp(60.0, 200.0);
                let response = crate::widgets::sized(ui, width, |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut item.find)
                            .hint_text("Find")
                            .desired_width(width),
                    )
                });
                changed |= response.changed();
                describe_unavailable(response, reason);
                let response = crate::widgets::sized(ui, width, |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut item.replace_with)
                            .hint_text("Replace with")
                            .desired_width(width),
                    )
                });
                changed |= response.changed();
                describe_unavailable(response, reason);
                let response = ui.checkbox(&mut item.match_case, "Case");
                changed |= response.changed();
                describe_unavailable(response, reason);
                let response = ui.checkbox(&mut item.whole_words_only, "Words");
                changed |= response.changed();
                describe_unavailable(response, reason);
                let response = ui.checkbox(&mut item.regular_expression, "Pattern");
                changed |= response.changed();
                describe_unavailable(response, reason);
                let response = ui.small_button("Remove").on_hover_text("Drop this rule");
                if response.clicked() {
                    remove = Some(index);
                }
                describe_unavailable(response, reason);
            });
        }
        let response = ui.button("Add rule");
        if response.clicked() {
            edited.push(ReplacementItem::default());
            changed = true;
        }
        describe_unavailable(response, reason);
    });
    if let Some(index) = remove {
        edited.remove(index);
        changed = true;
    }
    let _ = field;
    changed.then_some(FieldValue::Replacements(edited))
}

fn describe_unavailable(response: egui::Response, reason: Option<&str>) {
    if let Some(reason) = reason {
        let _ = crate::widgets::disabled_reason(response, reason);
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
mod tests {
    use super::{Scope, SettingsDialog};
    use crate::settings::field::FieldValue;
    use ca_session::settings::{SessionSettings, SessionSettingsOverride};
    use ca_session::{SessionKind, SettingsLayers};

    fn dialog(kind: SessionKind) -> SettingsDialog {
        let layers = SettingsLayers::new();
        let empty = SessionSettingsOverride::empty_for(&kind);
        SettingsDialog::new(kind, &empty, &layers, 1)
    }

    #[test]
    fn a_new_dialog_inherits_every_field() {
        let dialog = dialog(SessionKind::TextCompare);
        for field in dialog.tabs().iter().flat_map(|tab| tab.fields.iter()) {
            assert!(
                !dialog.is_overridden(field.key),
                "{} is marked as stated by the session",
                field.key
            );
        }
        assert!(dialog.to_override().is_empty());
    }

    #[test]
    fn an_edited_field_is_marked_and_reaches_the_override() {
        let mut dialog = dialog(SessionKind::TextCompare);
        dialog.set_value("alignment.skew_tolerance", &FieldValue::Count(12));
        assert!(dialog.is_overridden("alignment.skew_tolerance"));
        assert!(!dialog.is_overridden("alignment.use_closeness_matching"));
        let outcome = dialog.accept();
        let SessionSettingsOverride::TextCompare(text) = &outcome.overrides else {
            panic!("kind changed");
        };
        assert_eq!(text.alignment.skew_tolerance, Some(12));
        assert_eq!(text.alignment.never_align_differences, None);
        assert!(!dialog.is_open());
    }

    #[test]
    fn a_malformed_remote_side_keeps_the_last_safe_value() {
        let mut dialog = dialog(SessionKind::TextCompare);
        dialog.set_value(
            "specs.left",
            &FieldValue::Text("ftp://alice:correct@example.test/pub".to_owned()),
        );
        let before = dialog.value("specs.left");

        dialog.set_value(
            "specs.left",
            &FieldValue::Text("ftp://alice:pa/ss@files.example.test/pub".to_owned()),
        );

        assert_eq!(dialog.value("specs.left"), before);
        let stored = serde_json::to_string(dialog.settings())
            .unwrap_or_else(|_| panic!("settings serialize"));
        assert!(!stored.contains("pa/ss"));
        assert!(!stored.contains("correct"));
        assert!(stored.contains("example.test/pub"));
        assert!(!stored.contains("files.example.test"));
    }

    #[test]
    fn a_reset_field_inherits_again() {
        let mut dialog = dialog(SessionKind::HexCompare);
        dialog.set_value("comparison.bytes_per_row", &FieldValue::Count(32));
        assert!(dialog.is_overridden("comparison.bytes_per_row"));
        dialog.reset_field("comparison.bytes_per_row");
        assert!(!dialog.is_overridden("comparison.bytes_per_row"));
        assert!(dialog.to_override().is_empty());
    }

    #[test]
    fn reset_all_puts_every_page_back() {
        let mut dialog = dialog(SessionKind::FolderCompare);
        dialog.set_value("comparison.compare_size", &FieldValue::Flag(false));
        dialog.set_value(
            "name_filters.exclude_files",
            &FieldValue::Lines(vec!["*.tmp".to_owned()]),
        );
        assert!(!dialog.to_override().is_empty());
        dialog.reset_all();
        assert!(dialog.to_override().is_empty());
    }

    #[test]
    fn reset_page_leaves_the_other_pages_alone() {
        let mut dialog = dialog(SessionKind::FolderCompare);
        dialog.set_value("comparison.compare_size", &FieldValue::Flag(false));
        dialog.set_value("handling.follow_symbolic_links", &FieldValue::Flag(true));
        let comparison = dialog
            .tabs()
            .iter()
            .position(|tab| tab.name == "Comparison")
            .unwrap();
        dialog.select_tab(comparison);
        dialog.reset_page();
        assert!(!dialog.is_overridden("comparison.compare_size"));
        assert!(dialog.is_overridden("handling.follow_symbolic_links"));
    }

    #[test]
    fn a_session_states_only_what_differs_from_edited_defaults() {
        let mut layers = SettingsLayers::new();
        let mut defaults = SessionSettings::defaults_for(&SessionKind::TextCompare);
        let field = crate::settings::fields_for(&SessionKind::TextCompare)
            .into_iter()
            .find(|field| field.key == "alignment.skew_tolerance")
            .unwrap();
        field.write(&mut defaults, &FieldValue::Count(7));
        layers
            .update_session_defaults_from(&SessionKind::TextCompare, &defaults)
            .unwrap();

        let empty = SessionSettingsOverride::empty_for(&SessionKind::TextCompare);
        let dialog = SettingsDialog::new(SessionKind::TextCompare, &empty, &layers, 1);
        assert_eq!(
            dialog.value("alignment.skew_tolerance"),
            Some(FieldValue::Count(7)),
            "the session starts from the edited defaults"
        );
        assert!(!dialog.is_overridden("alignment.skew_tolerance"));
        assert!(dialog.to_override().is_empty());
    }

    #[test]
    fn the_scope_defaults_to_this_session_alone() {
        let mut dialog = dialog(SessionKind::TextCompare);
        assert_eq!(dialog.scope(), Scope::ThisViewOnly);
        dialog.set_scope(Scope::UpdateSessionDefaults);
        assert_eq!(dialog.accept().scope, Scope::UpdateSessionDefaults);
    }

    #[test]
    fn a_kind_with_no_settings_opens_without_a_page() {
        let dialog = dialog(SessionKind::Unknown("chart-compare".into()));
        assert!(dialog.tabs().is_empty());
    }

    #[test]
    fn every_kind_carries_the_pages_its_type_is_documented_with() {
        let expected: &[(SessionKind, &[&str])] = &[
            (
                SessionKind::FolderCompare,
                &[
                    "Specs",
                    "Comparison",
                    "Handling",
                    "Name Filters",
                    "Other Filters",
                    "Misc",
                ],
            ),
            (
                SessionKind::TextCompare,
                &["Specs", "Format", "Importance", "Alignment", "Replacements"],
            ),
            (
                SessionKind::TextMerge,
                &["Specs", "Format", "Importance", "Alignment"],
            ),
            (
                SessionKind::TableCompare,
                &["Specs", "Format", "Sheets", "Columns", "Rows"],
            ),
            (SessionKind::HexCompare, &["Specs", "Format", "Comparison"]),
            (
                SessionKind::PictureCompare,
                &["Specs", "Format", "Comparison", "Replacements"],
            ),
        ];
        for (kind, names) in expected {
            let drawn: Vec<&str> = crate::settings::tabs_for(kind)
                .iter()
                .map(|tab| tab.name)
                .collect();
            assert_eq!(&drawn, names, "{kind} pages");
        }
    }

    #[test]
    fn an_unavailable_reason_is_attached_to_the_disabled_checkbox() {
        use crate::testing::probe::controls;

        let mut dialog = dialog(SessionKind::FolderSync);
        dialog.active = dialog
            .tabs()
            .iter()
            .position(|tab| tab.name == "Sync")
            .unwrap();
        let field = dialog.tabs()[dialog.active]
            .fields
            .iter()
            .find(|field| field.key == "sync.copy_orphans")
            .unwrap();
        let reason = field.unavailable.unwrap();
        let mut probe = crate::testing::probe::Probe::new(1_280.0, 900.0);
        let mut draw = |ctx: &egui::Context| {
            let _ = dialog.show(ctx, &crate::theme::LIGHT);
        };
        let output = probe.frame(Vec::new(), &mut draw);
        let checkbox = controls(&output)
            .into_iter()
            .find(|control| control.label == "Copy orphans")
            .unwrap();
        assert_eq!(checkbox.role, egui::accesskit::Role::CheckBox);
        assert!(!checkbox.enabled);
        assert_eq!(checkbox.toggled, Some(true));
        assert_eq!(checkbox.description.as_deref(), Some(reason));
    }
}
