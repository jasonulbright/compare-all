//! Cell edits, undo, saving, and find and replace for the table view.
//!
//! An edit replaces a range of one side's source text and parses that text
//! again on a worker. The grid maps a cell to a byte range through the table it
//! paints, so an edit is refused until the table on screen was parsed from the
//! text as it now stands.

use crate::edit::SideText;
use crate::jobs::{self, Stage};
use crate::model::{Cursor, Side};
use crate::search::{self, CellMatcher, ReplaceMessage, Scope, SearchMessage};
use crate::{Status, TableView};
use ca_table::write::{cell_write, is_writable};
use ca_text::TextBuffer;
use ca_ui::save::text::SaveConsent;
use ca_ui::save::{Baseline, CloseChoice, Reread, SaveMessage, SaveOutcome};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A question the view is waiting on an answer to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Prompt {
    /// A concurrent version was retained beside the saved file.
    KeptCopy(String),
    /// The file changed on disk since it was read.
    DiskChanged(Side),
    /// The encoding cannot write the text back unchanged. The target of a
    /// Save As is kept so the retry writes to the same name.
    WouldLose(Side, String, Option<PathBuf>),
    /// The tab is closing with changes that are not written.
    Closing,
    /// A command that reads the files again waits on changes that are not
    /// written.
    Reread(Reread),
}

/// What the running comparison was started for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pending {
    /// The files are read.
    Load,
    /// A settings change compares the tables already held.
    Recompare,
    /// Edited text is parsed again.
    Edit,
}

/// The cell being typed into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CellEditor {
    pub(crate) side: Side,
    pub(crate) row: usize,
    pub(crate) column: usize,
    pub(crate) text: String,
    pub(crate) focused: bool,
}

impl TableView {
    pub(crate) const fn side_text(&self, side: Side) -> &SideText {
        match side {
            Side::Left => &self.left_text,
            Side::Right => &self.right_text,
        }
    }

    const fn side_text_mut(&mut self, side: Side) -> &mut SideText {
        match side {
            Side::Left => &mut self.left_text,
            Side::Right => &mut self.right_text,
        }
    }

    fn side_data(&self, side: Side) -> Option<&jobs::Side> {
        let sides = self.sides.as_ref()?;
        Some(match side {
            Side::Left => &sides.0,
            Side::Right => &sides.1,
        })
    }

    const fn parsed_revision(&self, side: Side) -> u64 {
        match side {
            Side::Left => self.parsed.0,
            Side::Right => self.parsed.1,
        }
    }

    /// True when the table on screen was parsed from the side's current text.
    fn in_step(&self, side: Side) -> bool {
        self.parsed_revision(side) == self.side_text(side).revision()
    }

    pub(crate) fn has_unparsed_edits(&self) -> bool {
        !self.in_step(Side::Left) || !self.in_step(Side::Right)
    }

    /// True when one side holds edits that are not written.
    #[must_use]
    pub const fn is_modified(&self, side: Side) -> bool {
        self.side_text(side).is_modified()
    }

    /// The side that edits, saves and searches act on.
    #[must_use]
    pub const fn active_side(&self) -> Side {
        self.active
    }

    /// Make one side the one that edits, saves and searches act on.
    pub fn set_active_side(&mut self, side: Side) {
        if self.active != side {
            self.clear_selection();
        }
        self.active = side;
    }

    /// The source text of one side as it now stands.
    #[must_use]
    pub fn side_source(&self, side: Side) -> &str {
        self.side_text(side).text()
    }

    /// What the status line says last.
    #[must_use]
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    /// Why one side cannot be edited or saved, or `None` when it can.
    #[must_use]
    pub fn write_block(&self, side: Side) -> Option<&'static str> {
        if self.clipboard_text.iter().any(Option::is_some) {
            return Some("Clipboard text is a temporary input and cannot be edited or saved.");
        }
        if self.read_only {
            return Some(
                "The file is a copy taken out of an archive, so it cannot be edited or saved.",
            );
        }
        if self.specs.disable_editing {
            return Some("Editing is turned off for this session, so no cell takes an edit.");
        }
        let Some(data) = self.side_data(side) else {
            return Some("No comparison is loaded.");
        };
        if data.facts.template.is_none() {
            return Some("The file was not read from disk.");
        }
        if !is_writable(&data.facts.options) {
            return Some(
                "This field syntax cannot be written back, so the file cannot be edited or saved.",
            );
        }
        None
    }

    /// The file row and column of the current cell on one side.
    fn file_cell(&self, side: Side) -> Option<(usize, usize)> {
        let cursor = self.grid.cursor();
        let row = self.grid.source_row(cursor.row)?;
        let column = self.grid.column_at(cursor.column)?;
        let (schema, comparison) = self.mapping.as_ref()?;
        let pair = comparison.rows.get(row)?.pair;
        let mapped = schema.columns.get(column)?;
        let (file_row, file_column) = match side {
            Side::Left => (pair.left?, mapped.left?),
            Side::Right => (pair.right?, mapped.right?),
        };
        Some((file_row as usize, file_column as usize))
    }

    /// True when the current cell of the active side can take a new value now.
    pub(crate) fn can_edit_cell(&self) -> bool {
        self.has_comparison()
            && self.write_block(self.active).is_none()
            && self.in_step(self.active)
            && self.file_cell(self.active).is_some()
    }

    pub(crate) fn can_copy_cell(&self, source: Side, target: Side) -> bool {
        if source == target
            || self.cell_editor.is_some()
            || !self.has_comparison()
            || !self.in_step(source)
            || !self.in_step(target)
            || self.write_block(target).is_some()
        {
            return false;
        }
        let Some((source_row, source_column)) = self.file_cell(source) else {
            return false;
        };
        let Some((target_row, target_column)) = self.file_cell(target) else {
            return false;
        };
        let (Some(source_data), Some(target_data)) =
            (self.side_data(source), self.side_data(target))
        else {
            return false;
        };
        let value = source_data
            .table
            .cell_text(source_row, source_column)
            .into_owned();
        cell_write(
            &target_data.table,
            &target_data.facts.options,
            target_row,
            target_column,
            &value,
        )
        .is_ok()
    }

    pub(crate) fn copy_cell_from_to(&mut self, source: Side, target: Side) {
        let result = (|| {
            if !self.can_copy_cell(source, target) {
                return Err("The current cell cannot be copied to that side.".to_owned());
            }
            let (source_row, source_column) = self
                .file_cell(source)
                .ok_or_else(|| "The source side has no cell there.".to_owned())?;
            let (target_row, target_column) = self
                .file_cell(target)
                .ok_or_else(|| "The target side has no matching cell.".to_owned())?;
            let source_data = self
                .side_data(source)
                .ok_or_else(|| "No comparison is loaded.".to_owned())?;
            let target_data = self
                .side_data(target)
                .ok_or_else(|| "No comparison is loaded.".to_owned())?;
            let value = source_data
                .table
                .cell_text(source_row, source_column)
                .into_owned();
            let write = cell_write(
                &target_data.table,
                &target_data.facts.options,
                target_row,
                target_column,
                &value,
            )
            .map_err(|error| format!("The cell cannot be copied: {error}."))?;
            if target_data.table.source().get(write.range.clone()) == Some(write.text.as_str()) {
                return Ok(());
            }
            self.side_text_mut(target)
                .replace(vec![(write.range, write.text)]);
            self.start_reparse();
            Ok(())
        })();
        if let Err(reason) = result {
            self.message = Some(reason);
        }
    }

    /// Put `value` into the current cell of the active side.
    ///
    /// # Errors
    ///
    /// Returns a sentence when the cell cannot take the value now.
    pub fn edit_current_cell(&mut self, value: &str) -> Result<(), String> {
        let side = self.active;
        if !self.has_comparison() || !self.in_step(side) {
            return Err("Wait for the comparison to finish.".to_owned());
        }
        if let Some(reason) = self.write_block(side) {
            return Err(reason.to_owned());
        }
        let (row, column) = self
            .file_cell(side)
            .ok_or_else(|| "This side has no cell there.".to_owned())?;
        let data = self
            .side_data(side)
            .ok_or_else(|| "No comparison is loaded.".to_owned())?;
        let write = cell_write(&data.table, &data.facts.options, row, column, value)
            .map_err(|error| format!("The value cannot be written: {error}."))?;
        if data.table.source().get(write.range.clone()) == Some(write.text.as_str()) {
            return Ok(());
        }
        self.side_text_mut(side)
            .replace(vec![(write.range, write.text)]);
        self.start_reparse();
        Ok(())
    }

    /// Open the cell editor over the current cell of the active side.
    pub fn begin_cell_edit(&mut self) {
        if !self.can_edit_cell() {
            self.message = Some(
                self.write_block(self.active)
                    .unwrap_or("This cell cannot be edited now.")
                    .to_owned(),
            );
            return;
        }
        let cursor = self.grid.cursor();
        let text = self
            .grid
            .cell_text(cursor.row, cursor.column, self.active)
            .into_owned();
        self.cell_editor = Some(CellEditor {
            side: self.active,
            row: cursor.row,
            column: cursor.column,
            text,
            focused: false,
        });
    }

    /// True while the cell editor is open.
    #[must_use]
    pub const fn is_editing_cell(&self) -> bool {
        self.cell_editor.is_some()
    }

    /// Set the text of the open cell editor, as typing into it does.
    pub fn set_cell_editor_text(&mut self, text: &str) {
        if let Some(editor) = self.cell_editor.as_mut() {
            text.clone_into(&mut editor.text);
        }
    }

    /// Write the open cell editor's text into its cell and close it.
    pub fn commit_cell_edit(&mut self) {
        let Some(editor) = self.cell_editor.take() else {
            return;
        };
        self.active = editor.side;
        self.set_grid_cursor(Cursor {
            row: editor.row,
            column: editor.column,
        });
        if let Err(reason) = self.edit_current_cell(&editor.text) {
            self.message = Some(reason);
        }
    }

    /// Close the cell editor without writing.
    pub fn cancel_cell_edit(&mut self) {
        self.cell_editor = None;
    }

    /// Parse both sides' current text again and compare.
    pub(crate) fn start_reparse(&mut self) {
        let Some(sides) = self.sides.as_ref() else {
            return;
        };
        let left = (sides.0.clone(), Some(self.left_text.snapshot()));
        let right = (sides.1.clone(), Some(self.right_text.snapshot()));
        self.reparse_revisions = (self.left_text.revision(), self.right_text.revision());
        self.pending = Pending::Edit;
        self.status = Status::Running(Stage::Parsing);
        let job = jobs::spawn_reparse(left, right, self.settings.clone(), Arc::clone(&self.notify));
        self.pipeline.start(job);
    }

    /// Reverse the last edit of the active side.
    pub fn undo(&mut self) {
        if self.status.is_running() || self.write_block(self.active).is_some() {
            return;
        }
        let side = self.active;
        if self.side_text_mut(side).undo().is_some() {
            self.start_reparse();
        }
    }

    /// Reapply the last reversed edit of the active side.
    pub fn redo(&mut self) {
        if self.status.is_running() || self.write_block(self.active).is_some() {
            return;
        }
        let side = self.active;
        if self.side_text_mut(side).redo().is_some() {
            self.start_reparse();
        }
    }

    // --- saving ----------------------------------------------------------

    fn side_path(&self, side: Side) -> PathBuf {
        match side {
            Side::Left => self.left_path.clone(),
            Side::Right => self.right_path.clone(),
        }
    }

    pub(crate) fn save_side(&mut self, side: Side, consent: SaveConsent) {
        self.save_side_to(side, consent, None);
    }

    fn save_side_to(&mut self, side: Side, consent: SaveConsent, save_as: Option<PathBuf>) {
        if matches!(self.prompt, Some(Prompt::KeptCopy(_))) || self.save_job.is_some() {
            return;
        }
        if let Some(reason) = self.write_block(side) {
            self.message = Some(reason.to_owned());
            self.save_both_pending = false;
            return;
        }
        let Some(template) = self
            .side_data(side)
            .and_then(|data| data.facts.template.clone())
        else {
            return;
        };
        let path = save_as.clone().unwrap_or_else(|| self.side_path(side));
        let expected = if save_as.is_some() {
            Baseline::Unchecked
        } else {
            match side {
                Side::Left => self.stamps.0,
                Side::Right => self.stamps.1,
            }
            .into()
        };
        let text = self.side_text(side).snapshot();
        let modified = self.side_text(side).is_modified();
        self.saving = Some(side);
        self.saving_as = save_as;
        self.saving_revision = Some(self.side_text(side).revision());
        self.saving_consent = consent;
        self.prompt = None;
        let rules = self.save_rules.clone();
        self.save_job = Some(ca_ui::save::spawn_with(
            Arc::clone(&self.notify),
            move || {
                let mut loaded = (*template).clone();
                // An unedited buffer replays the bytes of a file that did not
                // decode cleanly; an edited one must not, so the edit marks it.
                if modified {
                    loaded.buffer = TextBuffer::from_text("");
                    loaded.buffer.insert(0, &text);
                } else {
                    loaded.buffer = TextBuffer::from_text(&text);
                }
                ca_ui::save::text::save(&rules.files(), &path, &loaded, expected, consent)
            },
        ));
    }

    pub(crate) fn save_next_modified(&mut self) {
        self.save_both_pending = true;
        if self.left_text.is_modified() {
            self.save_side(Side::Left, SaveConsent::default());
        } else if self.right_text.is_modified() {
            self.save_side(Side::Right, SaveConsent::default());
        } else {
            self.save_both_pending = false;
        }
    }

    /// Write the active side under a new name.
    pub fn save_active_as(&mut self, path: &Path) {
        if self.save_job.is_some() {
            self.message = Some("A save is still running. Try Save As again.".to_owned());
            return;
        }
        let consent = SaveConsent {
            accept_disk_change: true,
            ..SaveConsent::default()
        };
        self.save_side_to(self.active, consent, Some(path.to_path_buf()));
    }

    /// True while a save runs.
    #[must_use]
    pub const fn is_saving(&self) -> bool {
        self.save_job.is_some()
    }

    pub(crate) fn poll_save(&mut self) {
        let Some(job) = self.save_job.as_mut() else {
            return;
        };
        let messages = job.drain();
        let finished = job.is_finished();
        if finished || !messages.is_empty() {
            self.save_job = None;
        }
        for message in messages {
            match message {
                SaveMessage::Done(outcome) => self.apply_save_outcome(*outcome),
                SaveMessage::Cancelled => {
                    self.message = Some("The save stopped.".to_owned());
                    self.saving = None;
                    self.saving_as = None;
                    self.save_both_pending = false;
                    self.forget_reread();
                }
            }
        }
    }

    fn apply_save_outcome(&mut self, outcome: SaveOutcome) {
        let Some(side) = self.saving.take() else {
            return;
        };
        let saving_as = self.saving_as.take();
        let backup = match &outcome {
            SaveOutcome::SavedWithConflict { backup, .. } => Some(backup.display().to_string()),
            _ => None,
        };
        match outcome {
            SaveOutcome::Saved(stamp) | SaveOutcome::SavedWithConflict { stamp, .. } => {
                if let Some(path) = saving_as {
                    match side {
                        Side::Left => {
                            self.left_field = path.display().to_string();
                            self.left_path = path;
                        }
                        Side::Right => {
                            self.right_field = path.display().to_string();
                            self.right_path = path;
                        }
                    }
                }
                match side {
                    Side::Left => self.stamps.0 = Some(stamp),
                    Side::Right => self.stamps.1 = Some(stamp),
                }
                if let Some(revision) = self.saving_revision.take() {
                    self.side_text_mut(side).mark_saved_revision(revision);
                }
                if let Some(backup) = backup {
                    self.message = Some(format!(
                        "Saved, but another version was kept at {backup}. Review it."
                    ));
                    self.prompt = Some(Prompt::KeptCopy(backup));
                    self.save_both_pending = false;
                    self.close_requested = false;
                    self.forget_reread();
                } else {
                    self.message = Some(format!("{} written.", side.label()));
                    if self.save_both_pending {
                        self.save_next_modified();
                    }
                    self.reread_when_saved();
                }
            }
            SaveOutcome::ChangedOnDisk => self.prompt = Some(Prompt::DiskChanged(side)),
            SaveOutcome::WouldLose(reason) => {
                self.prompt = Some(Prompt::WouldLose(side, reason, saving_as));
            }
            SaveOutcome::NotWritable => {
                self.message = Some(format!("{} cannot be written.", side.label()));
                self.save_both_pending = false;
                self.forget_reread();
            }
            SaveOutcome::Failed(reason) => {
                self.message = Some(format!("Save failed: {reason}"));
                self.save_both_pending = false;
                self.forget_reread();
            }
        }
    }

    /// The question the view is waiting on, as a sentence.
    #[must_use]
    pub fn prompt_text(&self) -> Option<String> {
        Some(match self.prompt.as_ref()? {
            Prompt::KeptCopy(path) => format!(
                "Another program changed the file during your save. The saved file contains your edit. Review the other version at {path}."
            ),
            Prompt::DiskChanged(_) => {
                "The file changed on disk since it was read. Overwrite it?".to_owned()
            }
            Prompt::WouldLose(_, reason, _) => {
                format!("{reason} Save anyway and lose those characters?")
            }
            Prompt::Closing | Prompt::Reread(_) => {
                "This tab has changes that are not written.".to_owned()
            }
        })
    }

    /// Answer the waiting question: `true` goes ahead, `false` stays put.
    ///
    /// Going ahead on the question about unwritten changes saves every
    /// modified side, then closes the tab or runs the command that asked.
    pub fn answer_prompt(&mut self, proceed: bool) {
        let Some(prompt) = self.prompt.clone() else {
            return;
        };
        match prompt {
            Prompt::KeptCopy(_) => self.prompt = None,
            Prompt::Closing | Prompt::Reread(_) => self.answer_unsaved(if proceed {
                CloseChoice::Save
            } else {
                CloseChoice::Cancel
            }),
            _ if !proceed => {
                self.prompt = None;
                self.saving = None;
                self.save_both_pending = false;
                self.close_requested = false;
                self.forget_reread();
            }
            Prompt::DiskChanged(side) => {
                let consent = SaveConsent {
                    accept_disk_change: true,
                    ..self.saving_consent
                };
                self.save_side(side, consent);
            }
            Prompt::WouldLose(side, _, target) => {
                let consent = SaveConsent {
                    accept_loss: true,
                    ..self.saving_consent
                };
                self.save_side_to(side, consent, target);
            }
        }
    }

    /// Answer the question about changes that are not written, which a close
    /// or a command that reads the files again raised.
    pub fn answer_unsaved(&mut self, choice: CloseChoice) {
        let action = match self.prompt {
            Some(Prompt::Closing) => None,
            Some(Prompt::Reread(action)) => Some(action),
            _ => return,
        };
        self.prompt = None;
        match (choice, action) {
            (CloseChoice::Cancel, _) => {
                self.close_requested = false;
                self.forget_reread();
            }
            (CloseChoice::Discard, None) => self.discard_and_close(),
            (CloseChoice::Discard, Some(action)) => self.reread(action),
            (CloseChoice::Save, None) => {
                self.close_requested = true;
                self.save_next_modified();
            }
            (CloseChoice::Save, Some(action)) => {
                self.reread_after_save = Some(action);
                self.save_next_modified();
                self.reread_when_saved();
            }
        }
    }

    /// Close the tab and drop every unwritten edit.
    pub fn discard_and_close(&mut self) {
        self.prompt = None;
        self.left_text.mark_saved();
        self.right_text.mark_saved();
        self.close_requested = true;
    }

    /// Run a command that reads the files again, asking first while a side
    /// holds an edit that is not written.
    pub(crate) fn request_reread(&mut self, action: Reread) {
        if self.save_job.is_some() || self.prompt.is_some() {
            self.message = Some("Finish the save and its questions first.".to_owned());
            self.forget_reread();
            return;
        }
        if self.is_modified(Side::Left) || self.is_modified(Side::Right) {
            self.prompt = Some(Prompt::Reread(action));
            return;
        }
        self.reread(action);
    }

    /// Carry out a command that reads the files again, dropping every edit.
    fn reread(&mut self, action: Reread) {
        self.reread_after_save = None;
        match action {
            Reread::Reload => self.restart(),
            Reread::Open => self.apply_fields(),
            Reread::SwapSides => {
                std::mem::swap(&mut self.left_path, &mut self.right_path);
                std::mem::swap(&mut self.left_field, &mut self.right_field);
                self.clipboard_text.swap(0, 1);
                self.restart();
            }
            Reread::Settings => {
                if let Some(settings) = self.pending_settings.take() {
                    self.settings = *settings;
                }
                self.restart();
            }
        }
    }

    /// Run the command that waited on a save once every edit is written.
    fn reread_when_saved(&mut self) {
        let written = self.save_job.is_none()
            && self.prompt.is_none()
            && !self.is_modified(Side::Left)
            && !self.is_modified(Side::Right);
        if !written {
            return;
        }
        if let Some(action) = self.reread_after_save.take() {
            self.reread(action);
        }
    }

    /// Drop the command that waited on a save that did not complete.
    fn forget_reread(&mut self) {
        self.reread_after_save = None;
        self.pending_settings = None;
    }

    pub(crate) fn prompt_panel(&mut self, ui: &mut egui::Ui) {
        let Some(prompt) = self.prompt.clone() else {
            return;
        };
        let Some(text) = self.prompt_text() else {
            return;
        };
        ui.horizontal_wrapped(|ui| {
            ui.label(text);
            match prompt {
                Prompt::KeptCopy(_) => {
                    if ui.button("I have the path").clicked() {
                        self.answer_prompt(true);
                    }
                }
                Prompt::DiskChanged(_) | Prompt::WouldLose(..) => {
                    if ui.button("Save anyway").clicked() {
                        self.answer_prompt(true);
                    }
                    if ui.button("Cancel").clicked() {
                        self.answer_prompt(false);
                    }
                }
                Prompt::Closing | Prompt::Reread(_) => {
                    let (save, discard) = match prompt {
                        Prompt::Reread(action) => (action.save_label(), action.discard_label()),
                        _ => ("Save and close", "Discard and close"),
                    };
                    for (label, choice) in [
                        (save, CloseChoice::Save),
                        (discard, CloseChoice::Discard),
                        ("Cancel", CloseChoice::Cancel),
                    ] {
                        if ui.button(label).clicked() {
                            self.answer_unsaved(choice);
                        }
                    }
                }
            }
        });
    }

    // --- find and replace -------------------------------------------------

    fn scope(&self) -> Scope {
        Scope {
            source: Arc::clone(self.grid.source()),
            filter: self.grid.filter(),
            ignore_unimportant: self.grid.ignores_unimportant(),
            columns: self.grid.shown_columns().to_vec(),
            side: self.active,
        }
    }

    /// Set what a search looks for.
    pub fn set_find_pattern(&mut self, pattern: &str) {
        pattern.clone_into(&mut self.find.settings.pattern);
    }

    /// Set what a replace puts in place of a match.
    pub fn set_replacement(&mut self, replacement: &str) {
        replacement.clone_into(&mut self.find.settings.replacement);
    }

    /// Search the active side's shown cells for the next or the previous
    /// match after the current cell.
    pub fn find_next(&mut self, backwards: bool) {
        if !self.has_comparison() {
            return;
        }
        if let Some(job) = self.search_job.take() {
            job.cancel();
        }
        let cursor = self.grid.cursor();
        let row = self.grid.source_row(cursor.row).unwrap_or(0);
        self.message = Some("Searching".to_owned());
        self.search_job = Some(search::spawn_find(
            self.scope(),
            self.find.settings.clone(),
            (row, cursor.column),
            backwards,
            Arc::clone(&self.notify),
        ));
    }

    /// True while a search or a replace all runs.
    #[must_use]
    pub const fn is_searching(&self) -> bool {
        self.search_job.is_some() || self.replace_job.is_some()
    }

    pub(crate) fn poll_search(&mut self) {
        let Some(job) = self.search_job.as_mut() else {
            return;
        };
        let messages = job.drain();
        let finished = job.is_finished();
        if finished || !messages.is_empty() {
            self.search_job = None;
        }
        for message in messages {
            match message {
                SearchMessage::Found { row, column } => {
                    if let Some(visual) = self.grid.visual_of_source(row) {
                        self.set_grid_cursor(Cursor {
                            row: visual,
                            column,
                        });
                        self.reveal_cursor();
                        self.message = None;
                    } else {
                        self.message = Some("The display changed during the search.".to_owned());
                    }
                }
                SearchMessage::NotFound => self.message = Some("No match found.".to_owned()),
                SearchMessage::BadPattern(reason) => self.message = Some(reason),
                SearchMessage::Cancelled => {}
            }
        }
    }

    /// Replace the first match in the current cell of the active side, then
    /// search on once the edit is compared.
    pub fn replace_current(&mut self) {
        if let Some(reason) = self.write_block(self.active) {
            self.message = Some(reason.to_owned());
            return;
        }
        let matcher = match CellMatcher::new(&self.find.settings) {
            Ok(matcher) => matcher,
            Err(reason) => {
                self.message = Some(reason);
                return;
            }
        };
        let cursor = self.grid.cursor();
        let cell = self
            .grid
            .cell_text(cursor.row, cursor.column, self.active)
            .into_owned();
        let Some(value) = matcher.replace_first(&cell, &self.find.settings.replacement) else {
            self.find_next(false);
            return;
        };
        match self.edit_current_cell(&value) {
            Ok(()) => self.find_after_reparse = self.status.is_running(),
            Err(reason) => self.message = Some(reason),
        }
    }

    /// Replace every match in the active side's shown cells as one undo step.
    pub fn replace_all_cells(&mut self) {
        let side = self.active;
        if let Some(reason) = self.write_block(side) {
            self.message = Some(reason.to_owned());
            return;
        }
        if !self.has_comparison() || !self.in_step(side) {
            self.message = Some("Wait for the comparison to finish.".to_owned());
            return;
        }
        let Some((schema, _)) = self.mapping.as_ref() else {
            return;
        };
        let file_columns = schema
            .columns
            .iter()
            .map(|column| match side {
                Side::Left => column.left,
                Side::Right => column.right,
            })
            .collect();
        if let Some(job) = self.replace_job.take() {
            job.cancel();
        }
        self.replace_target = (side, self.side_text(side).revision());
        self.message = Some("Searching".to_owned());
        self.replace_job = Some(search::spawn_replace_all(
            self.scope(),
            file_columns,
            self.find.settings.clone(),
            Arc::clone(&self.notify),
        ));
    }

    pub(crate) fn poll_replace(&mut self) {
        let Some(job) = self.replace_job.as_mut() else {
            return;
        };
        let messages = job.drain();
        let finished = job.is_finished();
        if finished || !messages.is_empty() {
            self.replace_job = None;
        }
        for message in messages {
            match message {
                ReplaceMessage::Changes(changes) => self.apply_replacements(&changes),
                ReplaceMessage::Failed(reason) => self.message = Some(reason),
                ReplaceMessage::Cancelled => {}
            }
        }
    }

    fn apply_replacements(&mut self, changes: &[search::CellChange]) {
        let (side, revision) = self.replace_target;
        if self.side_text(side).revision() != revision || !self.in_step(side) {
            self.message =
                Some("The table changed during the search. Replace All again.".to_owned());
            return;
        }
        let Some(data) = self.side_data(side) else {
            return;
        };
        let mut writes = Vec::with_capacity(changes.len());
        let mut refused = 0usize;
        for change in changes {
            match cell_write(
                &data.table,
                &data.facts.options,
                change.row,
                change.column,
                &change.value,
            ) {
                Ok(write) => writes.push((write.range, write.text)),
                Err(_) => refused += 1,
            }
        }
        let count = self.side_text_mut(side).replace(writes);
        self.message = Some(if refused == 0 {
            format!("{count} cells replaced.")
        } else {
            format!("{count} cells replaced. {refused} cells could not take the new value.")
        });
        if count > 0 {
            self.start_reparse();
        }
    }

    pub(crate) fn find_panel(&mut self, ui: &mut egui::Ui) {
        let controls = ca_ui::find::FindControls {
            whole_words: true,
            regex: true,
            selection_only: false,
            enter_searches: true,
        };
        match self.find.show_find_with(ui, controls, |_| {}) {
            Some(ca_ui::find::PanelRequest::Next) => self.find_next(false),
            Some(ca_ui::find::PanelRequest::Previous) => self.find_next(true),
            Some(ca_ui::find::PanelRequest::Replace) => self.replace_current(),
            Some(ca_ui::find::PanelRequest::ReplaceAll) => self.replace_all_cells(),
            _ => {}
        }
    }

    /// Draw the cell editor over its cell.
    pub(crate) fn cell_editor_ui(&mut self, ui: &mut egui::Ui, pane: egui::Rect, gutter: f32) {
        let Some(editor) = self.cell_editor.as_ref() else {
            return;
        };
        let Some(column) = self.grid.column_at(editor.column) else {
            self.cell_editor = None;
            return;
        };
        let height = self.row_height();
        let x = pane.left() + gutter - self.horizontal + self.grid.column_x(editor.column);
        let y = pane.top() + self.scroll.row_y(editor.row, height);
        let rect = egui::Rect::from_min_size(
            egui::pos2(x, y),
            egui::vec2(self.grid.width(column), height),
        );
        let id = self.id.with("cell-editor");
        let Some(editor) = self.cell_editor.as_mut() else {
            return;
        };
        let response = ui.put(
            rect,
            egui::TextEdit::singleline(&mut editor.text)
                .id(id)
                .margin(egui::Margin::same(1)),
        );
        if !editor.focused {
            response.request_focus();
            editor.focused = true;
            return;
        }
        let escape = ui.input(|input| input.key_pressed(egui::Key::Escape));
        if escape {
            self.cancel_cell_edit();
        } else if response.lost_focus() {
            self.commit_cell_edit();
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use crate::model::{Cursor, Side};
    use crate::TableView;
    use ca_table::parse::ParseOptions;
    use ca_ui::command::Command;
    use ca_ui::testing::{context, wait_until};
    use ca_ui::view::SessionView;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    fn files(left: &[u8], right: &[u8]) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let left_path = dir.path().join("left.csv");
        let right_path = dir.path().join("right.csv");
        std::fs::write(&left_path, left).unwrap();
        std::fs::write(&right_path, right).unwrap();
        (dir, left_path, right_path)
    }

    /// Wait until no comparison, save or search is running.
    fn settle(view: &mut TableView) {
        let idle = wait_until(Duration::from_secs(30), || {
            view.tick();
            !view.status.is_running()
                && !view.pipeline.is_running()
                && !view.is_saving()
                && !view.is_searching()
        });
        assert!(idle, "the view never became idle");
    }

    fn open(left: &Path, right: &Path) -> TableView {
        let mut view = TableView::new(left.to_path_buf(), right.to_path_buf(), &context(), 11);
        settle(&mut view);
        assert!(view.has_comparison(), "{:?}", view.failure());
        view
    }

    fn at(view: &mut TableView, row: usize, column: usize) {
        view.grid.set_cursor(Cursor { row, column });
    }

    const CSV: &[u8] = b"id,name\r\n1,Ann\r\n2,Bob\r\n";

    #[test]
    fn an_edited_cell_reaches_the_grid_and_undo_and_redo_walk_it_back() {
        let (_dir, left, right) = files(CSV, CSV);
        let mut view = open(&left, &right);
        at(&mut view, 0, 1);
        assert!(view.can_edit_cell());
        view.edit_current_cell("Anna").unwrap();
        settle(&mut view);
        assert_eq!(view.grid.cell_text(0, 1, Side::Left), "Anna");
        assert_eq!(
            view.side_source(Side::Left),
            "id,name\r\n1,Anna\r\n2,Bob\r\n"
        );
        assert!(view.is_modified(Side::Left));
        assert!(!view.is_modified(Side::Right));
        assert_eq!(view.grid.cursor(), Cursor { row: 0, column: 1 });
        assert!(view.accepts(Command::Undo));
        view.run(Command::Undo);
        settle(&mut view);
        assert_eq!(view.grid.cell_text(0, 1, Side::Left), "Ann");
        assert!(!view.is_modified(Side::Left));
        view.run(Command::Redo);
        settle(&mut view);
        assert_eq!(view.grid.cell_text(0, 1, Side::Left), "Anna");
    }

    #[test]
    fn a_settings_change_during_edit_reparse_does_not_hide_the_edit() {
        let (_dir, left, right) = files(CSV, CSV);
        let mut view = open(&left, &right);
        at(&mut view, 0, 1);
        view.edit_current_cell("Anna").unwrap();
        view.settings.schema.handling.entry(0).or_default().key = true;
        view.recompare();
        settle(&mut view);
        assert_eq!(view.grid.cell_text(0, 1, Side::Left), "Anna");
        assert!(view.can_edit_cell());
    }

    #[test]
    fn an_edit_keeps_the_header_choice_from_a_named_format() {
        let source = b"name,qty\nA,5\n";
        let (_dir, left, right) = files(source, source);
        let mut view = open(&left, &right);
        let mut options = ca_table::parse::ParseOptions::comma_separated();
        options.first_line_contains = ca_table::parse::FirstLineContains::Detect;
        view.settings.left_format.parse = Some(options.clone());
        view.settings.right_format.parse = Some(options);
        view.restart();
        settle(&mut view);
        assert_eq!(view.grid.visual_rows(), 1);
        at(&mut view, 0, 1);
        view.edit_current_cell("five").unwrap();
        settle(&mut view);
        assert_eq!(view.grid.visual_rows(), 1);
        assert_eq!(view.grid.cell_text(0, 1, Side::Left), "five");
        assert!(view.grid.cell_differs(0, 1));
    }

    #[test]
    fn a_save_keeps_the_delimiter_the_quoting_and_the_line_endings() {
        let source = b"id,name,note\r\n1,\"Ann\",x\r\n2,Bob,y\r\n";
        let (_dir, left, right) = files(source, source);
        let mut view = open(&left, &right);
        at(&mut view, 1, 2);
        view.edit_current_cell("a, \"b\"").unwrap();
        settle(&mut view);
        at(&mut view, 0, 1);
        view.edit_current_cell("Anne").unwrap();
        settle(&mut view);
        assert!(view.accepts(Command::SaveFile));
        view.run(Command::SaveFile);
        settle(&mut view);
        assert_eq!(view.message(), Some("Left written."));
        assert_eq!(
            std::fs::read(&left).unwrap(),
            b"id,name,note\r\n1,\"Anne\",x\r\n2,Bob,\"a, \"\"b\"\"\"\r\n"
        );
        assert!(!view.is_modified(Side::Left));
        assert_eq!(std::fs::read(&right).unwrap(), source);
    }

    #[test]
    fn a_byte_order_mark_and_a_tab_delimiter_survive_a_save() {
        let source = "\u{feff}id\tname\n1\tAnn\n".as_bytes();
        let (_dir, left, right) = files(source, source);
        let mut view = open(&left, &right);
        at(&mut view, 0, 1);
        view.edit_current_cell("Zed").unwrap();
        settle(&mut view);
        view.run(Command::SaveFile);
        settle(&mut view);
        assert_eq!(
            std::fs::read(&left).unwrap(),
            "\u{feff}id\tname\n1\tZed\n".as_bytes()
        );
    }

    #[test]
    fn a_save_keeps_a_copy_when_the_backup_page_asks_for_one() {
        let (dir, left, right) = files(CSV, CSV);
        let mut view = open(&left, &right);
        let mut options = ca_session::options::ProgramOptions::default();
        options.backups.before_save = true;
        let resolved = std::sync::Arc::new(ca_ui::options::AppOptions::resolve(
            options,
            ca_ui::theme::Variant::Light,
            ca_session::AdminPolicies::default(),
        ));
        let ctx = egui::Context::default();
        let held = context();
        let _ = ctx.run(ca_ui::testing::raw_input(), |ctx| {
            ca_ui::options::install(ctx, std::sync::Arc::clone(&resolved));
            egui::CentralPanel::default().show(ctx, |ui| {
                view.ui(ui, &held);
            });
        });
        at(&mut view, 0, 1);
        view.edit_current_cell("Anna").unwrap();
        settle(&mut view);
        view.run(Command::SaveFile);
        settle(&mut view);
        assert_eq!(std::fs::read(dir.path().join("left.csv.bak")).unwrap(), CSV);
        assert_eq!(
            std::fs::read(&left).unwrap(),
            b"id,name\r\n1,Anna\r\n2,Bob\r\n"
        );
    }

    #[test]
    fn a_file_changed_on_disk_asks_before_it_is_overwritten() {
        let (_dir, left, right) = files(CSV, CSV);
        let mut view = open(&left, &right);
        at(&mut view, 0, 1);
        view.edit_current_cell("Anna").unwrap();
        settle(&mut view);
        std::fs::write(&left, b"changed elsewhere, longer than before\n").unwrap();
        view.run(Command::SaveFile);
        settle(&mut view);
        assert!(matches!(
            view.prompt,
            Some(super::Prompt::DiskChanged(Side::Left))
        ));
        assert_eq!(
            std::fs::read(&left).unwrap(),
            b"changed elsewhere, longer than before\n"
        );
        view.answer_prompt(true);
        settle(&mut view);
        assert_eq!(
            std::fs::read(&left).unwrap(),
            b"id,name\r\n1,Anna\r\n2,Bob\r\n"
        );
        assert!(!view.is_modified(Side::Left));
    }

    #[test]
    fn a_save_in_flight_keeps_the_tab_busy() {
        let (_dir, left, right) = files(CSV, CSV);
        let mut view = open(&left, &right);
        at(&mut view, 0, 1);
        view.edit_current_cell("Anna").unwrap();
        settle(&mut view);
        assert!(!SessionView::is_busy(&view));
        view.run(Command::SaveFile);
        assert!(view.is_saving());
        assert!(SessionView::is_busy(&view));
        assert!(!view.may_close());
        settle(&mut view);
        assert!(!SessionView::is_busy(&view));
    }

    #[test]
    fn closing_with_unwritten_edits_asks_first() {
        let (_dir, left, right) = files(CSV, CSV);
        let mut view = open(&left, &right);
        assert!(view.may_close());
        at(&mut view, 0, 1);
        view.edit_current_cell("Anna").unwrap();
        settle(&mut view);
        assert!(!view.may_close());
        assert_eq!(
            view.prompt_text().as_deref(),
            Some("This tab has changes that are not written.")
        );
        assert!(!view.wants_close());
        view.discard_and_close();
        assert!(view.wants_close());
        assert_eq!(std::fs::read(&left).unwrap(), CSV);
    }

    /// Reload and Swap Sides read both files again, so an edit that is not
    /// written raises the question a close raises instead of a refusal, and
    /// the edit stays until it is answered.
    #[test]
    fn reload_with_an_edit_asks_before_it_reads_the_files_again() {
        let (_dir, left, right) = files(CSV, CSV);
        let mut view = open(&left, &right);
        at(&mut view, 0, 1);
        view.edit_current_cell("Anna").unwrap();
        settle(&mut view);
        for command in [Command::Reload, Command::SwapSides] {
            assert!(view.accepts(command), "{command:?} is refused");
            view.run(command);
            settle(&mut view);
            assert_eq!(
                view.prompt_text().as_deref(),
                Some("This tab has changes that are not written.")
            );
            assert!(view.is_modified(Side::Left));
            view.answer_unsaved(ca_ui::save::CloseChoice::Cancel);
            assert_eq!(view.prompt_text(), None);
            assert_eq!(view.grid.cell_text(0, 1, Side::Left), "Anna");
        }

        view.run(Command::Reload);
        view.answer_unsaved(ca_ui::save::CloseChoice::Discard);
        settle(&mut view);
        assert!(!view.is_modified(Side::Left));
        assert_eq!(view.grid.cell_text(0, 1, Side::Left), "Ann");
        assert_eq!(std::fs::read(&left).unwrap(), CSV);
    }

    /// A settings change that reads the files again asks first while a side
    /// holds an edit, and the settings apply after the answer.
    #[test]
    fn a_settings_change_that_reads_the_files_again_asks_first() {
        use ca_session::settings::common::EncodingChoice;
        use ca_session::settings::{SessionSettings, TableCompareSettings};

        let (_dir, left, right) = files(CSV, CSV);
        let mut view = open(&left, &right);
        at(&mut view, 0, 1);
        view.edit_current_cell("Anna").unwrap();
        settle(&mut view);
        let before = view.settings.left_format.encoding;
        let mut changed = TableCompareSettings::default();
        changed.format.left_encoding = EncodingChoice::Named {
            name: "windows-1252".to_owned(),
            unknown: std::collections::BTreeMap::default(),
        };
        let changed = SessionSettings::TableCompare(changed);

        view.apply_settings(&changed);
        settle(&mut view);
        assert_eq!(
            view.prompt_text().as_deref(),
            Some("This tab has changes that are not written.")
        );
        assert!(view.is_modified(Side::Left));
        assert!(view.accepts(Command::Undo));
        assert_eq!(view.settings.left_format.encoding, before);

        view.answer_unsaved(ca_ui::save::CloseChoice::Cancel);
        settle(&mut view);
        assert!(view.is_modified(Side::Left));
        assert_eq!(view.grid.cell_text(0, 1, Side::Left), "Anna");
        assert_eq!(view.settings.left_format.encoding, before);

        view.apply_settings(&changed);
        view.answer_unsaved(ca_ui::save::CloseChoice::Discard);
        settle(&mut view);
        assert!(!view.is_modified(Side::Left));
        assert_ne!(view.settings.left_format.encoding, before);
        assert_eq!(std::fs::read(&left).unwrap(), CSV);
    }

    #[test]
    fn save_and_close_writes_every_edited_side_before_closing() {
        let (_dir, left, right) = files(CSV, CSV);
        let mut view = open(&left, &right);
        at(&mut view, 0, 1);
        view.edit_current_cell("L").unwrap();
        settle(&mut view);
        view.set_active_side(Side::Right);
        view.edit_current_cell("R").unwrap();
        settle(&mut view);
        assert!(view.accepts(Command::SaveBoth));
        assert!(!view.may_close());
        view.answer_prompt(true);
        let closed = wait_until(Duration::from_secs(30), || {
            view.tick();
            view.wants_close()
        });
        assert!(closed);
        assert_eq!(
            std::fs::read(&left).unwrap(),
            b"id,name\r\n1,L\r\n2,Bob\r\n"
        );
        assert_eq!(
            std::fs::read(&right).unwrap(),
            b"id,name\r\n1,R\r\n2,Bob\r\n"
        );
    }

    #[test]
    fn save_as_writes_the_new_name_and_leaves_the_old_file() {
        let (dir, left, right) = files(CSV, CSV);
        let mut view = open(&left, &right);
        at(&mut view, 1, 1);
        view.edit_current_cell("Bo").unwrap();
        settle(&mut view);
        let target = dir.path().join("copy.csv");
        view.save_active_as(&target);
        settle(&mut view);
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"id,name\r\n1,Ann\r\n2,Bo\r\n"
        );
        assert_eq!(std::fs::read(&left).unwrap(), CSV);
        assert_eq!(view.left(), target.as_path());
        assert!(!view.is_modified(Side::Left));
    }

    #[test]
    fn a_fixed_width_field_refuses_a_value_that_would_move_the_next_column() {
        let source = b"abcdef\nghijkl\n";
        let (_dir, left, right) = files(source, source);
        let mut view = TableView::new(left, right, &context(), 12);
        let fixed = ParseOptions {
            first_line_contains: ca_table::parse::FirstLineContains::CellData,
            ..ParseOptions::fixed_width([3, 3])
        };
        view.settings.left_format.parse = Some(fixed.clone());
        view.settings.right_format.parse = Some(fixed);
        view.restart();
        settle(&mut view);
        at(&mut view, 0, 0);
        let refused = view.edit_current_cell("wxyz").unwrap_err();
        assert!(refused.contains("3 characters"), "{refused}");
        assert!(!view.is_modified(Side::Left));
        view.edit_current_cell("x").unwrap();
        settle(&mut view);
        assert_eq!(view.side_source(Side::Left), "x  def\nghijkl\n");
    }

    #[test]
    fn a_syntax_that_cannot_be_written_disables_editing_and_saving_with_a_reason() {
        let (_dir, left, right) = files(CSV, CSV);
        let mut view = TableView::new(left, right, &context(), 13);
        view.settings.left_format.parse = Some(ParseOptions::default());
        view.restart();
        settle(&mut view);
        assert!(view.write_block(Side::Left).is_some());
        assert!(!view.can_edit_cell());
        assert!(!view.accepts(Command::SaveFileAs));
        assert!(!view.accepts(Command::Replace));
        assert!(view.edit_current_cell("x").is_err());
        view.set_active_side(Side::Right);
        assert!(view.write_block(Side::Right).is_none());
    }

    #[test]
    fn find_next_moves_to_the_matching_cell_of_the_active_side() {
        let (_dir, left, right) = files(
            b"id,name\n1,Ann\n2,Bob\n3,Cy\n",
            b"id,name\n1,Ann\n2,Rob\n3,Cy\n",
        );
        let mut view = open(&left, &right);
        at(&mut view, 0, 0);
        view.set_find_pattern("cy");
        view.run(Command::FindNext);
        settle(&mut view);
        assert_eq!(view.grid.cursor(), Cursor { row: 2, column: 1 });
        view.set_find_pattern("rob");
        view.run(Command::FindNext);
        settle(&mut view);
        assert_eq!(view.message(), Some("No match found."));
        view.set_active_side(Side::Right);
        view.run(Command::FindPrevious);
        settle(&mut view);
        assert_eq!(view.grid.cursor(), Cursor { row: 1, column: 1 });
    }

    #[test]
    fn replace_all_changes_every_matching_cell_in_one_undo_step() {
        let source = b"id,name\n1,Ann\n2,Anna\n3,Bob\n";
        let (_dir, left, right) = files(source, source);
        let mut view = open(&left, &right);
        view.set_find_pattern("Ann");
        view.set_replacement("Jo");
        view.replace_all_cells();
        settle(&mut view);
        assert_eq!(view.message(), Some("2 cells replaced."));
        assert_eq!(
            view.side_source(Side::Left),
            "id,name\n1,Jo\n2,Joa\n3,Bob\n"
        );
        assert_eq!(
            view.side_source(Side::Right),
            "id,name\n1,Ann\n2,Anna\n3,Bob\n"
        );
        view.run(Command::Undo);
        settle(&mut view);
        assert_eq!(
            view.side_source(Side::Left),
            "id,name\n1,Ann\n2,Anna\n3,Bob\n"
        );
        assert!(!view.is_modified(Side::Left));
    }

    #[test]
    fn replace_changes_the_current_cell_and_moves_to_the_next_match() {
        let source = b"id,name\n1,Ann\n2,Bob\n3,Ann\n";
        let (_dir, left, right) = files(source, source);
        let mut view = open(&left, &right);
        at(&mut view, 0, 1);
        view.set_find_pattern("Ann");
        view.set_replacement("Al");
        view.replace_current();
        settle(&mut view);
        settle(&mut view);
        assert_eq!(
            view.side_source(Side::Left),
            "id,name\n1,Al\n2,Bob\n3,Ann\n"
        );
        assert_eq!(view.grid.cursor(), Cursor { row: 2, column: 1 });
    }

    #[test]
    fn the_cell_editor_writes_on_commit_and_drops_on_cancel() {
        let (_dir, left, right) = files(CSV, CSV);
        let mut view = open(&left, &right);
        at(&mut view, 1, 1);
        view.begin_cell_edit();
        assert!(view.is_editing_cell());
        view.set_cell_editor_text("Zed");
        view.cancel_cell_edit();
        assert!(!view.is_modified(Side::Left));
        view.begin_cell_edit();
        view.set_cell_editor_text("Zed");
        view.commit_cell_edit();
        settle(&mut view);
        assert_eq!(view.grid.cell_text(1, 1, Side::Left), "Zed");
    }

    #[test]
    fn the_cell_editor_takes_typing_in_a_frame_and_enter_writes_it() {
        let (_dir, left, right) = files(CSV, CSV);
        let mut view = open(&left, &right);
        at(&mut view, 1, 1);
        let ctx = egui::Context::default();
        let frame = |view: &mut TableView, events: Vec<egui::Event>| {
            let input = ca_ui::testing::event_input(1_280.0, 800.0, events);
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    view.ui(ui, &context());
                });
            });
        };
        view.begin_cell_edit();
        frame(&mut view, Vec::new());
        frame(&mut view, Vec::new());
        frame(&mut view, vec![egui::Event::Text("!".to_owned())]);
        assert!(view.is_editing_cell());
        frame(
            &mut view,
            vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
            }],
        );
        assert!(!view.is_editing_cell());
        settle(&mut view);
        assert_eq!(view.grid.cell_text(1, 1, Side::Left), "Bob!");
    }

    #[test]
    fn a_lossy_load_asks_before_an_edited_save() {
        // 0xFF is not valid UTF-8, so the load needs a replacement character.
        let source = b"id,name\n1,A\xFFn\n";
        let (_dir, left, right) = files(source, source);
        let mut view = TableView::new(left.clone(), right, &context(), 14);
        view.settings.left_format.encoding = Some(ca_text::TextEncoding::Utf8);
        view.restart();
        settle(&mut view);
        at(&mut view, 0, 0);
        view.edit_current_cell("9").unwrap();
        settle(&mut view);
        view.run(Command::SaveFile);
        settle(&mut view);
        assert!(matches!(
            view.prompt,
            Some(super::Prompt::WouldLose(Side::Left, _, None))
        ));
        assert_eq!(std::fs::read(&left).unwrap(), source);
    }

    /// Each answer adds its own consent to the save in progress: the loss
    /// agreed to first still holds after the change on disk is agreed to,
    /// so the second answer ends the save rather than asking again.
    #[test]
    fn a_lossy_save_over_a_file_changed_on_disk_asks_each_question_once() {
        let source = b"id,name\n1,A\xFFn\n";
        let (_dir, left, right) = files(source, source);
        let mut view = TableView::new(left.clone(), right, &context(), 15);
        view.settings.left_format.encoding = Some(ca_text::TextEncoding::Utf8);
        view.restart();
        settle(&mut view);
        at(&mut view, 0, 0);
        view.edit_current_cell("9").unwrap();
        settle(&mut view);
        view.run(Command::SaveFile);
        settle(&mut view);
        assert!(matches!(view.prompt, Some(super::Prompt::WouldLose(..))));
        std::fs::write(&left, b"changed elsewhere, longer than before\n").unwrap();

        view.answer_prompt(true);
        settle(&mut view);
        assert!(matches!(
            view.prompt,
            Some(super::Prompt::DiskChanged(Side::Left))
        ));
        assert_eq!(
            std::fs::read(&left).unwrap(),
            b"changed elsewhere, longer than before\n"
        );

        view.answer_prompt(true);
        settle(&mut view);
        assert_eq!(view.prompt, None, "a question was asked a second time");
        assert!(!view.is_modified(Side::Left));
        assert_eq!(
            std::fs::read_to_string(&left).unwrap(),
            "id,name\n9,A\u{FFFD}n\n"
        );
    }
}
