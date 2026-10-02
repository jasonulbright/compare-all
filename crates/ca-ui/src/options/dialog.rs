//! The application options dialog.
//!
//! The dialog edits a copy of the stored document. Apply hands that copy to the
//! caller without closing; OK does the same and closes; Cancel drops it. A
//! field no part of the program reads is drawn with its stored value and takes
//! no edit, so nothing on a page looks as though it changed something.

use super::schema::{self, OptionField, OptionPage, PageKind, ARCHIVE_PAGE_NOTE, POLICY_REASON};
use crate::command::{bindings, Command, Keystroke, MenuView, ShortcutTable};
use crate::settings::field::{FieldShape, FieldValue};
use crate::theme::slots::{self, to_color, to_stored};
use crate::theme::{Palette, Variant};
use crate::toolbar::{self, ToolbarView};
use ca_session::options::{ColorGroup, OpenWithEntry, ProgramOptions, WorkingFolder};
use ca_session::AdminPolicies;

/// Width the label column takes before the controls start.
const LABEL_WIDTH: f32 = 230.0;

/// Width a control takes when the window is at its narrowest.
const CONTROL_WIDTH: f32 = 170.0;

/// Room the dialog keeps for its own frame inside the window.
const DIALOG_MARGIN: f32 = 24.0;

/// What the toolbar page reaches.
const TOOLBAR_NOTE: &str =
    "Each comparison type draws its toolbar in the order below. A change takes effect on the next frame.";

/// What the Open With page reaches.
///
/// The entries are stored, exported and imported, and the File menu starts one
/// over what the active view holds.
const OPEN_WITH_REASON: &str =
    "The File menu lists these entries for the active comparison and starts the one chosen.";

/// What the dialog produced.
#[derive(Debug, Clone, PartialEq)]
pub struct OptionsOutcome {
    /// The options as edited.
    pub options: Box<ProgramOptions>,
    /// True when the dialog closed as well as committing.
    pub closed: bool,
}

/// The application options dialog.
pub struct OptionsDialog {
    edited: ProgramOptions,
    committed: ProgramOptions,
    policies: AdminPolicies,
    active: usize,
    open: bool,
    id: egui::Id,
    color_group: ColorGroup,
    color_variant: Variant,
    command_view: MenuView,
    toolbar_view: ToolbarView,
    search: String,
    selected_command: Option<Command>,
    capturing: bool,
    conflict: Option<String>,
    selected_entry: Option<usize>,
}

impl OptionsDialog {
    /// A dialog over a copy of `options`.
    #[must_use]
    pub fn new(options: &ProgramOptions, policies: AdminPolicies, salt: u64) -> Self {
        Self {
            edited: options.clone(),
            committed: options.clone(),
            policies,
            active: 0,
            open: true,
            id: egui::Id::new(("program-options", salt)),
            color_group: ColorGroup::Text,
            color_variant: Variant::Dark,
            command_view: MenuView::Text,
            toolbar_view: ToolbarView::Text,
            search: String::new(),
            selected_command: None,
            capturing: false,
            conflict: None,
            selected_entry: None,
        }
    }

    /// True while the dialog is on screen.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.open
    }

    /// The options as they stand, which a live preview paints from.
    #[must_use]
    pub const fn options(&self) -> &ProgramOptions {
        &self.edited
    }

    /// True when the user asked for pending changes to reach the open views.
    #[must_use]
    pub fn previews(&self) -> bool {
        self.edited.appearance.enable_preview
    }

    /// The pages the dialog carries.
    #[must_use]
    pub const fn pages(&self) -> &'static [OptionPage] {
        schema::PAGES
    }

    /// Index of the page on screen.
    #[must_use]
    pub const fn active_page(&self) -> usize {
        self.active
    }

    /// Show the page at `index`.
    pub fn select_page(&mut self, index: usize) {
        if index < schema::PAGES.len() {
            self.active = index;
        }
    }

    /// Show the page named `name`.
    pub fn select_page_named(&mut self, name: &str) {
        if let Some(index) = schema::PAGES.iter().position(|page| page.name == name) {
            self.active = index;
        }
    }

    /// Which color group the color pages are showing.
    pub fn set_color_group(&mut self, group: ColorGroup) {
        self.color_group = group;
    }

    /// Which variant the color pages are editing.
    pub fn set_color_variant(&mut self, variant: Variant) {
        self.color_variant = variant;
    }

    /// Which bar the commands page is editing.
    pub fn set_command_view(&mut self, view: MenuView) {
        self.command_view = view;
    }

    /// Which toolbar the toolbars page is editing.
    pub fn set_toolbar_view(&mut self, view: ToolbarView) {
        self.toolbar_view = view;
    }

    /// Read one field of the options being edited.
    #[must_use]
    pub fn value(&self, key: &str) -> Option<FieldValue> {
        schema::field(key).map(|field| field.read(&self.edited))
    }

    /// Write one field of the options being edited.
    ///
    /// A field nothing reads, or one a policy disables, takes no edit.
    pub fn set_value(&mut self, key: &str, value: &FieldValue) {
        let Some(field) = schema::field(key) else {
            return;
        };
        if field.blocked_by(self.policies).is_some() {
            return;
        }
        field.write(&mut self.edited, value);
    }

    /// The color one slot carries under the edits made so far.
    #[must_use]
    pub fn color(&self, group: ColorGroup, variant: Variant, slot: &str) -> egui::Color32 {
        let dark = matches!(variant, Variant::Dark);
        slots::color_of(
            group,
            variant,
            self.edited.appearance.palettes.group(group).table(dark),
            slot,
        )
    }

    /// States a color for one slot.
    pub fn set_color(
        &mut self,
        group: ColorGroup,
        variant: Variant,
        slot: &str,
        color: egui::Color32,
    ) {
        let dark = matches!(variant, Variant::Dark);
        self.edited
            .appearance
            .palettes
            .group_mut(group)
            .table_mut(dark)
            .set(slot, to_stored(color));
    }

    /// Puts one color group back to the built-in table, in both variants.
    pub fn restore_colors(&mut self, group: ColorGroup) {
        *self.edited.appearance.palettes.group_mut(group) =
            ca_session::options::ColorPair::default();
    }

    /// Puts one named group of one color page back to the built-in table.
    pub fn restore_color_slots(&mut self, group: ColorGroup, variant: Variant, name: &str) {
        let dark = matches!(variant, Variant::Dark);
        let table = self
            .edited
            .appearance
            .palettes
            .group_mut(group)
            .table_mut(dark);
        for slot in slots::slots_of(group)
            .iter()
            .filter(|slot| slot.group == name)
        {
            table.clear(slot.name);
        }
    }

    /// The routing table the edits so far produce.
    #[must_use]
    pub fn shortcut_table(&self) -> ShortcutTable {
        ShortcutTable::from_options(&self.edited.commands)
    }

    /// Adds a keystroke to a command of one bar.
    ///
    /// A keystroke another command of the same bar already claims is refused
    /// and the reason is reported, so a binding never silently replaces one the
    /// user cannot see.
    ///
    /// # Errors
    /// Returns the reason when the keystroke is already claimed.
    pub fn add_shortcut(
        &mut self,
        view: MenuView,
        command: Command,
        stroke: Keystroke,
    ) -> Result<(), String> {
        let table = self.shortcut_table();
        if let Some(held) = table.claimed_by(view, stroke) {
            if held != command {
                return Err(format!(
                    "{} already runs {} in {}.",
                    stroke.to_text(),
                    held.label(),
                    view.label()
                ));
            }
            return Ok(());
        }
        let mut keys: Vec<String> = table
            .shortcuts_of(view, command)
            .into_iter()
            .map(Keystroke::to_text)
            .collect();
        keys.push(stroke.to_text());
        self.edited
            .commands
            .set_shortcuts(view.id(), command.id(), keys);
        Ok(())
    }

    /// Removes one keystroke from a command of one bar.
    pub fn remove_shortcut(&mut self, view: MenuView, command: Command, stroke: Keystroke) {
        let keys: Vec<String> = self
            .shortcut_table()
            .shortcuts_of(view, command)
            .into_iter()
            .filter(|held| *held != stroke)
            .map(Keystroke::to_text)
            .collect();
        self.edited
            .commands
            .set_shortcuts(view.id(), command.id(), keys);
    }

    /// Puts one command's bindings back to the built-in ones.
    pub fn reset_shortcut(&mut self, view: MenuView, command: Command) {
        self.edited.commands.reset_command(view.id(), command.id());
    }

    /// Puts every command of every bar back to the built-in bindings.
    pub fn reset_all_shortcuts(&mut self) {
        self.edited.commands.reset_all_shortcuts();
    }

    /// The commands of one bar whose name or description holds `search`.
    #[must_use]
    pub fn commands_matching(&self, view: MenuView, search: &str) -> Vec<Command> {
        let needle = search.trim().to_lowercase();
        let mut listed: Vec<Command> = bindings(view)
            .into_iter()
            .map(|(_, command)| command)
            .collect();
        for command in Command::ALL {
            if !listed.contains(command) {
                listed.push(*command);
            }
        }
        listed.retain(|command| {
            needle.is_empty()
                || command.label_in(view).to_lowercase().contains(&needle)
                || command.description().to_lowercase().contains(&needle)
        });
        listed
    }

    /// The entry list of the Open With page.
    #[must_use]
    pub fn open_with_entries(&self) -> &[OpenWithEntry] {
        &self.edited.open_with.entries
    }

    /// Adds an empty entry to the Open With page and selects it.
    pub fn add_open_with_entry(&mut self) {
        self.edited.open_with.entries.push(OpenWithEntry::default());
        self.selected_entry = Some(self.edited.open_with.entries.len().saturating_sub(1));
    }

    /// Removes one entry of the Open With page.
    pub fn remove_open_with_entry(&mut self, index: usize) {
        if index < self.edited.open_with.entries.len() {
            self.edited.open_with.entries.remove(index);
            self.selected_entry = None;
        }
    }

    /// Commits the edits without closing.
    #[must_use]
    pub fn apply(&mut self) -> OptionsOutcome {
        self.committed = self.edited.clone();
        OptionsOutcome {
            options: Box::new(self.edited.clone()),
            closed: false,
        }
    }

    /// Commits the edits and closes.
    #[must_use]
    pub fn accept(&mut self) -> OptionsOutcome {
        self.open = false;
        self.committed = self.edited.clone();
        OptionsOutcome {
            options: Box::new(self.edited.clone()),
            closed: true,
        }
    }

    /// Closes the dialog, dropping whatever was not applied.
    ///
    /// The last applied copy comes back so a preview that painted from the
    /// edits is put right on the next frame.
    #[must_use]
    pub fn cancel(&mut self) -> OptionsOutcome {
        self.open = false;
        self.edited = self.committed.clone();
        OptionsOutcome {
            options: Box::new(self.committed.clone()),
            closed: true,
        }
    }

    /// Draw the dialog and report whatever it produced.
    pub fn show(&mut self, ctx: &egui::Context, palette: &Palette) -> Option<OptionsOutcome> {
        if !self.open {
            return None;
        }
        let mut outcome = None;
        let mut open = true;
        let width = (ctx.screen_rect().width() - DIALOG_MARGIN).max(CONTROL_WIDTH);
        egui::Window::new("Options")
            .id(self.id)
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .max_width(width)
            .show(ctx, |ui| {
                ui.set_max_width(width);
                outcome = self.body(ui, palette);
            });
        if !open && outcome.is_none() {
            outcome = Some(self.cancel());
        }
        outcome
    }

    /// Draw the dialog into a panel rather than a window, for a headless frame.
    pub fn body(&mut self, ui: &mut egui::Ui, palette: &Palette) -> Option<OptionsOutcome> {
        self.page_strip(ui);
        ui.separator();
        let page = schema::PAGES.get(self.active).copied();
        let height = (ui.available_height() - 40.0).max(60.0);
        egui::ScrollArea::vertical()
            .id_salt(self.id.with(self.active))
            .max_height(height)
            .show(ui, |ui| {
                if let Some(page) = page {
                    self.page_body(ui, palette, &page);
                }
            });
        ui.separator();
        self.footer(ui)
    }

    fn page_strip(&mut self, ui: &mut egui::Ui) {
        let mut chosen = self.active;
        ui.horizontal_wrapped(|ui| {
            for (index, page) in schema::PAGES.iter().enumerate() {
                if ui
                    .selectable_label(index == self.active, page.name)
                    .clicked()
                {
                    chosen = index;
                }
            }
        });
        self.active = chosen;
    }

    fn page_body(&mut self, ui: &mut egui::Ui, palette: &Palette, page: &OptionPage) {
        let mut edits: Vec<(&'static OptionField, FieldValue)> = Vec::new();
        for field in page.fields {
            self.row(ui, palette, field, &mut edits);
        }
        for (field, value) in edits {
            field.write(&mut self.edited, &value);
        }
        match page.kind {
            PageKind::Fields => {
                if page.name == "Archive Types" {
                    crate::widgets::wrapped_text(ui, ARCHIVE_PAGE_NOTE);
                }
            }
            PageKind::Colors => {
                ui.separator();
                self.colors_body(ui, page);
            }
            PageKind::Commands => self.commands_body(ui),
            PageKind::Toolbars => self.toolbars_body(ui),
            PageKind::OpenWith => self.open_with_body(ui),
        }
    }

    /// One labeled control, with the reason when it takes no edit.
    fn row(
        &self,
        ui: &mut egui::Ui,
        palette: &Palette,
        field: &'static OptionField,
        edits: &mut Vec<(&'static OptionField, FieldValue)>,
    ) {
        let value = field.read(&self.edited);
        let blocked = field.blocked_by(self.policies);
        let stated = field.read(&ProgramOptions::default()) != value;
        ui.horizontal_wrapped(|ui| {
            let label_width = LABEL_WIDTH.min((ui.available_width() - CONTROL_WIDTH).max(80.0));
            crate::widgets::sized(ui, label_width, |ui| {
                let text = egui::RichText::new(field.label).color(if stated {
                    palette.settings_overridden
                } else {
                    palette.settings_inherited
                });
                ui.add(egui::Label::new(text).truncate())
                    .on_hover_text(match blocked {
                        Some(reason) => reason,
                        None => field.key,
                    });
            });
            if let Some(reason) = blocked {
                ui.add_enabled_ui(false, |ui| {
                    control(ui, field, &value);
                })
                .response
                .on_hover_text(reason);
                return;
            }
            if let Some(edited) = control(ui, field, &value) {
                edits.push((field, edited));
            }
        });
    }

    fn colors_body(&mut self, ui: &mut egui::Ui, page: &OptionPage) {
        let mut group = self.color_group;
        if !page.colors.contains(&group) {
            group = page.colors.first().copied().unwrap_or(ColorGroup::Text);
        }
        ui.horizontal_wrapped(|ui| {
            for offered in page.colors {
                if ui
                    .selectable_label(*offered == group, group_label(*offered))
                    .clicked()
                {
                    group = *offered;
                }
            }
            ui.separator();
            for variant in [Variant::Light, Variant::Dark] {
                if ui
                    .selectable_label(self.color_variant == variant, variant_label(variant))
                    .clicked()
                {
                    self.color_variant = variant;
                }
            }
        });
        self.color_group = group;
        let variant = self.color_variant;

        let mut changes: Vec<(&'static str, egui::Color32)> = Vec::new();
        let mut restore: Option<&'static str> = None;
        let mut current_group = "";
        for slot in slots::slots_of(group) {
            if slot.group != current_group {
                current_group = slot.group;
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new(current_group).strong());
                    if ui
                        .small_button("Restore defaults")
                        .on_hover_text("Put this group back to the built-in colors")
                        .clicked()
                    {
                        restore = Some(current_group);
                    }
                });
            }
            let mut color = self.color(group, variant, slot.name);
            ui.horizontal_wrapped(|ui| {
                let label_width = LABEL_WIDTH.min((ui.available_width() - CONTROL_WIDTH).max(80.0));
                crate::widgets::sized(ui, label_width, |ui| {
                    ui.add(egui::Label::new(slot.label).truncate())
                        .on_hover_text(slot.name);
                });
                if ui.color_edit_button_srgba(&mut color).changed() {
                    changes.push((slot.name, color));
                }
            });
        }
        for (slot, color) in changes {
            self.set_color(group, variant, slot, color);
        }
        if let Some(name) = restore {
            self.restore_color_slots(group, variant, name);
        }
        ui.separator();
        if ui
            .button("Restore every color of this view")
            .on_hover_text("Put both variants back to the built-in colors")
            .clicked()
        {
            self.restore_colors(group);
        }
    }

    fn commands_body(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            for view in MenuView::ALL {
                if ui
                    .selectable_label(self.command_view == *view, view.label())
                    .clicked()
                {
                    self.command_view = *view;
                    self.selected_command = None;
                    self.capturing = false;
                }
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("Search");
            crate::widgets::sized(ui, CONTROL_WIDTH, |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .hint_text("Name or description")
                        .desired_width(CONTROL_WIDTH),
                );
            });
            if ui.button("Reset all").clicked() {
                self.reset_all_shortcuts();
            }
        });
        if let Some(reason) = &self.conflict {
            crate::widgets::wrapped_text(ui, reason);
        }

        let view = self.command_view;
        let table = self.shortcut_table();
        let listed = self.commands_matching(view, &self.search.clone());
        let mut select: Option<Command> = None;
        let mut remove: Option<(Command, Keystroke)> = None;
        let mut reset: Option<Command> = None;
        for command in listed {
            ui.horizontal_wrapped(|ui| {
                let label_width = LABEL_WIDTH.min((ui.available_width() - CONTROL_WIDTH).max(80.0));
                crate::widgets::sized(ui, label_width, |ui| {
                    if ui
                        .selectable_label(
                            self.selected_command == Some(command),
                            command.label_in(view),
                        )
                        .on_hover_text(command.description())
                        .clicked()
                    {
                        select = Some(command);
                    }
                });
                for stroke in table.shortcuts_of(view, command) {
                    if ui
                        .small_button(stroke.to_text())
                        .on_hover_text("Remove this keystroke")
                        .clicked()
                    {
                        remove = Some((command, stroke));
                    }
                }
                if ui.small_button("Default").clicked() {
                    reset = Some(command);
                }
            });
        }
        if let Some(command) = select {
            self.selected_command = Some(command);
            self.capturing = true;
            self.conflict = None;
        }
        if let Some((command, stroke)) = remove {
            self.remove_shortcut(view, command, stroke);
        }
        if let Some(command) = reset {
            self.reset_shortcut(view, command);
        }

        if let Some(command) = self.selected_command {
            ui.separator();
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("Press a keystroke for {}", command.label_in(view)));
                if ui.small_button("Stop").clicked() {
                    self.capturing = false;
                }
            });
            if self.capturing {
                if let Some(stroke) = pressed(ui.ctx()) {
                    match self.add_shortcut(view, command, stroke) {
                        Ok(()) => {
                            self.conflict = None;
                            self.capturing = false;
                        }
                        Err(reason) => self.conflict = Some(reason),
                    }
                }
            }
        }
    }

    fn toolbars_body(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            for view in ToolbarView::ALL {
                if ui
                    .selectable_label(self.toolbar_view == *view, view.label())
                    .clicked()
                {
                    self.toolbar_view = *view;
                }
            }
        });
        crate::widgets::wrapped_text(ui, TOOLBAR_NOTE);
        let view = self.toolbar_view;
        let layout = toolbar::Layout::from_options(&self.edited.commands, view);
        let arranged = layout.arrange_names(toolbar::defaults(view));
        let mut moved: Option<(usize, usize)> = None;
        let mut toggled: Option<(&'static str, bool)> = None;
        let last = arranged.len().saturating_sub(1);
        for (index, held) in arranged.iter().enumerate() {
            ui.horizontal_wrapped(|ui| {
                let mut shown = layout.shows(held.name);
                if let Some(icon) = crate::icons::toolbar_icon(view, held.name) {
                    icon.show(ui, 16.0, ui.visuals().text_color());
                }
                if ui.checkbox(&mut shown, held.label).changed() {
                    toggled = Some((held.name, !shown));
                }
                if ui
                    .add_enabled(index > 0, egui::Button::new("Up").small())
                    .clicked()
                {
                    moved = Some((index, index - 1));
                }
                if ui
                    .add_enabled(index < last, egui::Button::new("Down").small())
                    .clicked()
                {
                    moved = Some((index, index + 1));
                }
            });
        }
        ui.separator();
        if ui.button("Reset this toolbar").clicked() {
            self.edited.commands.reset_toolbar(view.id());
            return;
        }
        if let Some((name, hidden)) = toggled {
            self.edited
                .commands
                .set_hidden_from_toolbar(view.id(), name, hidden);
        }
        if let Some((from, to)) = moved {
            let mut order: Vec<String> = arranged.iter().map(|held| held.name.to_owned()).collect();
            if from < order.len() && to < order.len() {
                order.swap(from, to);
                self.edited.commands.set_toolbar(view.id(), order);
            }
        }
    }

    fn open_with_body(&mut self, ui: &mut egui::Ui) {
        ui.add_enabled_ui(false, |ui| {
            crate::widgets::wrapped_text(ui, OPEN_WITH_REASON);
        });
        let mut remove: Option<usize> = None;
        let mut select: Option<usize> = None;
        for (index, entry) in self.edited.open_with.entries.iter().enumerate() {
            ui.horizontal_wrapped(|ui| {
                let label = if entry.description.is_empty() {
                    "Untitled entry"
                } else {
                    entry.description.as_str()
                };
                if ui
                    .selectable_label(self.selected_entry == Some(index), label)
                    .clicked()
                {
                    select = Some(index);
                }
                if ui.small_button("Remove").clicked() {
                    remove = Some(index);
                }
            });
        }
        if let Some(index) = select {
            self.selected_entry = Some(index);
        }
        if let Some(index) = remove {
            self.remove_open_with_entry(index);
        }
        if ui.button("New").clicked() {
            self.add_open_with_entry();
        }
        if let Some(index) = self.selected_entry {
            self.open_with_entry_body(ui, index);
        }
    }

    /// The editor of one Open With entry.
    fn open_with_entry_body(&mut self, ui: &mut egui::Ui, index: usize) {
        let Some(entry) = self.edited.open_with.entries.get_mut(index) else {
            return;
        };
        ui.separator();
        text_row(ui, "Description", &mut entry.description);
        let mut program = entry.program.as_path().display().to_string();
        if text_row(ui, "Program", &mut program) {
            entry.program = std::path::PathBuf::from(&program).into();
        }
        let mut arguments = entry.arguments.join("\n");
        ui.horizontal_wrapped(|ui| {
            crate::widgets::sized(ui, LABEL_WIDTH, |ui| {
                ui.add(egui::Label::new("Arguments, one per line").truncate())
                    .on_hover_text("Variables: %f %l %n %p %x %b %F %P, with 1 or 2 for the side");
            });
            crate::widgets::sized(ui, CONTROL_WIDTH, |ui| {
                if ui
                    .add(
                        egui::TextEdit::multiline(&mut arguments)
                            .desired_width(CONTROL_WIDTH)
                            .desired_rows(3),
                    )
                    .changed()
                {
                    entry.arguments = arguments
                        .lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty())
                        .map(str::to_owned)
                        .collect();
                }
            });
        });
        text_row(ui, "Shortcut", &mut entry.shortcut);
        ui.horizontal_wrapped(|ui| {
            crate::widgets::sized(ui, LABEL_WIDTH, |ui| {
                ui.add(egui::Label::new("Working folder").truncate());
            });
            let mut chosen = entry.working_folder.id().to_owned();
            crate::widgets::sized(ui, CONTROL_WIDTH, |ui| {
                egui::ComboBox::from_id_salt(ui.id().with("working-folder"))
                    .selected_text(working_folder_label(&chosen))
                    .show_ui(ui, |ui| {
                        for id in ["inherit", "parentFolder", "baseFolder", "named"] {
                            ui.selectable_value(
                                &mut chosen,
                                id.to_owned(),
                                working_folder_label(id),
                            );
                        }
                    });
            });
            if chosen != entry.working_folder.id() {
                entry.working_folder = entry.working_folder.with_id(&chosen);
            }
        });
        if let WorkingFolder::Named { path, .. } = &mut entry.working_folder {
            let mut text = path.as_path().display().to_string();
            if text_row(ui, "Folder", &mut text) {
                *path = std::path::PathBuf::from(&text).into();
            }
        }
        text_row(ui, "Path delimiter", &mut entry.path_delimiter);
        flag_row(ui, "Accepts files", &mut entry.accepts_files);
        flag_row(ui, "Accepts folders", &mut entry.accepts_folders);
        flag_row(
            ui,
            "Refresh when finished",
            &mut entry.refresh_when_finished,
        );
        flag_row(
            ui,
            "Allow multiple instances",
            &mut entry.multiple_instances,
        );
        flag_row(
            ui,
            "Wait for the previous instance",
            &mut entry.wait_for_previous,
        );
    }

    fn footer(&mut self, ui: &mut egui::Ui) -> Option<OptionsOutcome> {
        let mut outcome = None;
        ui.horizontal_wrapped(|ui| {
            if ui.button("OK").clicked() {
                outcome = Some(self.accept());
            }
            if ui.button("Cancel").clicked() {
                outcome = Some(self.cancel());
            }
            if ui.button("Apply").clicked() {
                outcome = Some(self.apply());
            }
        });
        outcome
    }
}

/// The first keystroke this frame carried, ignoring a bare modifier.
fn pressed(ctx: &egui::Context) -> Option<Keystroke> {
    ctx.input(|input| {
        input.events.iter().find_map(|event| match event {
            egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => Some(Keystroke {
                key: *key,
                command: modifiers.command,
                shift: modifiers.shift,
                alt: modifiers.alt,
            }),
            _ => None,
        })
    })
}

/// One labeled single line edit. Reports whether the text changed.
fn text_row(ui: &mut egui::Ui, label: &str, value: &mut String) -> bool {
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        crate::widgets::sized(ui, LABEL_WIDTH, |ui| {
            ui.add(egui::Label::new(label).truncate());
        });
        crate::widgets::sized(ui, CONTROL_WIDTH, |ui| {
            changed = ui
                .add(egui::TextEdit::singleline(value).desired_width(CONTROL_WIDTH))
                .changed();
        });
    });
    changed
}

/// One labeled check box.
fn flag_row(ui: &mut egui::Ui, label: &str, value: &mut bool) {
    ui.horizontal_wrapped(|ui| {
        crate::widgets::sized(ui, LABEL_WIDTH, |ui| {
            ui.add(egui::Label::new(label).truncate());
        });
        ui.checkbox(value, "");
    });
}

/// The label of one color group.
const fn group_label(group: ColorGroup) -> &'static str {
    match group {
        ColorGroup::Text => "Text",
        ColorGroup::Folder => "Folder",
        ColorGroup::Hex => "Bytes",
        ColorGroup::Table => "Table",
        ColorGroup::Picture => "Picture",
        ColorGroup::Merge => "Merge",
    }
}

/// The label of one theme variant.
const fn variant_label(variant: Variant) -> &'static str {
    match variant {
        Variant::Light => "Light",
        Variant::Dark => "Dark",
    }
}

/// The label of one working folder choice.
fn working_folder_label(id: &str) -> &'static str {
    match id {
        "parentFolder" => "The folder holding the item",
        "baseFolder" => "The base folder of the side",
        "named" => "A named folder",
        _ => "The program's own folder",
    }
}

/// Draw the control for one field and report a new value when it changed.
fn control(ui: &mut egui::Ui, field: &OptionField, value: &FieldValue) -> Option<FieldValue> {
    let width = CONTROL_WIDTH.min(ui.available_width().max(60.0));
    match (field.shape, value) {
        (FieldShape::Flag, FieldValue::Flag(on)) => {
            let mut on = *on;
            if ui.checkbox(&mut on, "").changed() {
                return Some(FieldValue::Flag(on));
            }
        }
        (FieldShape::Count, FieldValue::Count(number)) => {
            let mut number = *number;
            let changed = crate::widgets::sized(ui, width, |ui| {
                ui.add(egui::DragValue::new(&mut number).range(0..=field.limit))
                    .changed()
            });
            if changed {
                return Some(FieldValue::Count(number));
            }
        }
        (FieldShape::Decimal, FieldValue::Decimal(number)) => {
            let mut number = *number;
            let changed = crate::widgets::sized(ui, width, |ui| {
                ui.add(egui::DragValue::new(&mut number).range(
                    ca_session::options::provisional::MINIMUM_POINT_SIZE
                        ..=ca_session::options::provisional::MAXIMUM_POINT_SIZE,
                ))
                .changed()
            });
            if changed {
                return Some(FieldValue::Decimal(number));
            }
        }
        (FieldShape::Text | FieldShape::Path, FieldValue::Text(text)) => {
            let mut text = text.clone();
            let changed = crate::widgets::sized(ui, width, |ui| {
                ui.add(egui::TextEdit::singleline(&mut text).desired_width(width))
                    .changed()
            });
            if changed {
                return Some(FieldValue::Text(text));
            }
        }
        (FieldShape::Choice, FieldValue::Choice(id)) => {
            let mut chosen = id.clone();
            let label = field
                .choices
                .iter()
                .find(|choice| choice.id == id)
                .map_or(id.as_str(), |choice| choice.label);
            crate::widgets::sized(ui, width, |ui| {
                egui::ComboBox::from_id_salt(ui.id().with(field.key))
                    .selected_text(label)
                    .show_ui(ui, |ui| {
                        for choice in field.choices {
                            ui.selectable_value(&mut chosen, choice.id.to_owned(), choice.label);
                        }
                    });
            });
            if &chosen != id {
                return Some(FieldValue::Choice(chosen));
            }
        }
        (FieldShape::Lines, FieldValue::Lines(lines)) => {
            let mut text = lines.join("\n");
            let changed = crate::widgets::sized(ui, width, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut text)
                        .desired_width(width)
                        .desired_rows(2),
                )
                .changed()
            });
            if changed {
                return Some(FieldValue::Lines(
                    text.lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty())
                        .map(str::to_owned)
                        .collect(),
                ));
            }
        }
        _ => {
            ui.label(value.display());
        }
    }
    None
}

/// Why the whole dialog is unavailable, where a policy says so.
#[must_use]
pub fn policy_reason() -> &'static str {
    POLICY_REASON
}

/// The colors one group carries once the edits are applied, for a preview.
#[must_use]
pub fn preview_tables(options: &ProgramOptions, variant: Variant) -> crate::theme::slots::Tables {
    crate::theme::slots::Tables::resolve(variant, &options.appearance.palettes)
}

/// A color as the theme states it.
#[must_use]
pub const fn color_from(stored: ca_session::options::Rgb) -> egui::Color32 {
    to_color(stored)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::OptionsDialog;
    use crate::command::{Command, Keystroke, MenuView};
    use crate::settings::field::FieldValue;
    use crate::theme::slots::to_stored;
    use crate::theme::Variant;
    use ca_session::options::{ColorGroup, ProgramOptions, Rgb};
    use ca_session::{AdminPolicies, InMemoryPolicySource, PolicyKey, PolicyLoader};

    fn dialog() -> OptionsDialog {
        OptionsDialog::new(&ProgramOptions::default(), AdminPolicies::default(), 1)
    }

    #[test]
    fn an_edit_reaches_the_outcome_only_once_it_is_applied() {
        let mut dialog = dialog();
        dialog.set_value("text_editing.tab_stop", &FieldValue::Count(4));
        assert_eq!(
            dialog.value("text_editing.tab_stop"),
            Some(FieldValue::Count(4))
        );
        let outcome = dialog.apply();
        assert_eq!(outcome.options.text_editing.tab_stop, 4);
        assert!(!outcome.closed);
        assert!(dialog.is_open());
    }

    #[test]
    fn cancel_gives_back_the_last_applied_copy() {
        let mut dialog = dialog();
        dialog.set_value("text_editing.tab_stop", &FieldValue::Count(4));
        let _ = dialog.apply();
        dialog.set_value("text_editing.tab_stop", &FieldValue::Count(9));
        let outcome = dialog.cancel();
        assert_eq!(outcome.options.text_editing.tab_stop, 4);
        assert!(!dialog.is_open());
    }

    #[test]
    fn a_field_a_policy_disables_takes_no_edit() {
        let policies: AdminPolicies = InMemoryPolicySource::new()
            .with(PolicyKey::DisableCheckForUpdates, true)
            .load()
            .unwrap();
        let mut dialog = OptionsDialog::new(&ProgramOptions::default(), policies, 1);
        dialog.set_value("tweaks.check_for_updates_days", &FieldValue::Count(30));
        assert_eq!(
            dialog.value("tweaks.check_for_updates_days"),
            Some(FieldValue::Count(u64::from(
                ProgramOptions::default().tweaks.check_for_updates_days
            )))
        );
    }

    #[test]
    fn a_color_edit_reaches_the_resolved_table_of_its_own_variant() {
        let mut dialog = dialog();
        dialog.set_color(
            ColorGroup::Text,
            Variant::Dark,
            "same_line",
            crate::theme::slots::to_color(Rgb::new(1, 2, 3)),
        );
        let tables = super::preview_tables(dialog.options(), Variant::Dark);
        assert_eq!(to_stored(tables.main.same_line), Rgb::new(1, 2, 3));
        let light = super::preview_tables(dialog.options(), Variant::Light);
        assert_eq!(light.main.same_line, crate::theme::LIGHT.same_line);
    }

    #[test]
    fn restoring_a_group_puts_both_variants_back() {
        let mut dialog = dialog();
        for variant in [Variant::Light, Variant::Dark] {
            dialog.set_color(
                ColorGroup::Folder,
                variant,
                "folder_different",
                crate::theme::slots::to_color(Rgb::new(9, 9, 9)),
            );
        }
        dialog.restore_colors(ColorGroup::Folder);
        assert!(dialog
            .options()
            .appearance
            .palettes
            .group(ColorGroup::Folder)
            .is_empty());
    }

    #[test]
    fn restoring_one_named_group_leaves_the_other_slots_alone() {
        let mut dialog = dialog();
        dialog.set_color(
            ColorGroup::Text,
            Variant::Dark,
            "syntax_comment",
            crate::theme::slots::to_color(Rgb::new(9, 9, 9)),
        );
        dialog.set_color(
            ColorGroup::Text,
            Variant::Dark,
            "same_line",
            crate::theme::slots::to_color(Rgb::new(8, 8, 8)),
        );
        dialog.restore_color_slots(ColorGroup::Text, Variant::Dark, "Syntax");
        let table = dialog
            .options()
            .appearance
            .palettes
            .group(ColorGroup::Text)
            .table(true);
        assert_eq!(table.get("syntax_comment"), None);
        assert_eq!(table.get("same_line"), Some(Rgb::new(8, 8, 8)));
    }

    #[test]
    fn a_rebound_key_routes_and_the_old_one_stops() {
        let mut dialog = dialog();
        dialog
            .add_shortcut(
                MenuView::Text,
                Command::FindNext,
                Keystroke::with_command_alt(egui::Key::J),
            )
            .unwrap();
        let table = dialog.shortcut_table();
        assert_eq!(
            table.claimed_by(MenuView::Text, Keystroke::with_command_alt(egui::Key::J)),
            Some(Command::FindNext)
        );
        assert_eq!(
            table.claimed_by(MenuView::Text, Keystroke::plain(egui::Key::F3)),
            Some(Command::FindNext),
            "the built-in key is kept when a second one is added"
        );
    }

    #[test]
    fn a_keystroke_another_command_claims_is_refused() {
        let mut dialog = dialog();
        let error = dialog
            .add_shortcut(
                MenuView::Text,
                Command::FindNext,
                Keystroke::with_command(egui::Key::F),
            )
            .unwrap_err();
        assert!(error.contains("Find"), "{error}");
        assert_eq!(
            dialog
                .shortcut_table()
                .claimed_by(MenuView::Text, Keystroke::with_command(egui::Key::F)),
            Some(Command::Find)
        );
    }

    #[test]
    fn removing_and_resetting_a_binding_both_work() {
        let mut dialog = dialog();
        dialog.remove_shortcut(
            MenuView::Text,
            Command::FindNext,
            Keystroke::plain(egui::Key::F3),
        );
        assert!(dialog
            .shortcut_table()
            .shortcuts_of(MenuView::Text, Command::FindNext)
            .is_empty());
        dialog.reset_shortcut(MenuView::Text, Command::FindNext);
        assert_eq!(
            dialog
                .shortcut_table()
                .claimed_by(MenuView::Text, Keystroke::plain(egui::Key::F3)),
            Some(Command::FindNext)
        );
    }

    #[test]
    fn reset_all_puts_every_bar_back() {
        let mut dialog = dialog();
        dialog
            .add_shortcut(
                MenuView::Text,
                Command::FindNext,
                Keystroke::with_command_alt(egui::Key::J),
            )
            .unwrap();
        dialog
            .add_shortcut(
                MenuView::Folder,
                Command::Reload,
                Keystroke::with_command_alt(egui::Key::K),
            )
            .unwrap();
        dialog.reset_all_shortcuts();
        assert!(dialog.options().commands.views.is_empty());
    }

    #[test]
    fn the_search_field_matches_a_name_and_a_description() {
        let dialog = dialog();
        let by_name = dialog.commands_matching(MenuView::Text, "find next");
        assert!(by_name.contains(&Command::FindNext), "{by_name:?}");
        let by_description = dialog.commands_matching(MenuView::Text, "clipboard");
        assert!(
            by_description.contains(&Command::Copy),
            "{by_description:?}"
        );
        assert!(dialog.commands_matching(MenuView::Text, "").len() > 50);
    }

    #[test]
    fn an_open_with_entry_is_added_and_removed() {
        let mut dialog = dialog();
        dialog.add_open_with_entry();
        assert_eq!(dialog.open_with_entries().len(), 1);
        dialog.remove_open_with_entry(0);
        assert!(dialog.open_with_entries().is_empty());
    }

    #[test]
    fn every_page_is_reachable_by_name() {
        let mut dialog = dialog();
        for page in dialog.pages() {
            dialog.select_page_named(page.name);
            assert_eq!(dialog.pages()[dialog.active_page()].name, page.name);
        }
    }
}
