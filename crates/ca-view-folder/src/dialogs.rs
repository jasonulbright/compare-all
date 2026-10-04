//! The windows that stand between a command and the disk.
//!
//! Every operation shows its settings and then the plan itself: how many files
//! and folders, how many bytes, and every conflict the plan reports grouped by
//! kind. Nothing runs until the confirm button is pressed.

use crate::operations::{
    conflict_heading, conflicts_by_kind, path_option_label, totals, verify_label, Form, Operation,
    RenameMode, TouchChoice,
};
use crate::opjobs::{Answer, ProgressState, Question, RecoveryNotice};
use crate::selection::Scope;
use ca_fs::{ExecutionReport, OperationPlan, PathOption, StepOutcome, Verify};
use ca_ui::format::{format_bytes, format_stamp};
use ca_ui::widgets::{path_line, path_text, wrapped_text};

/// Room left between a dialog and the edge of the window.
const MARGIN: f32 = 12.0;

/// Width a dialog takes when the window has room for it.
const PREFERRED_WIDTH: f32 = 560.0;

/// Height the scrolling part of a dialog is allowed.
const BODY_HEIGHT: f32 = 260.0;

/// What the user pressed on a confirmation dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmChoice {
    /// Neither button was pressed this frame.
    Pending,
    /// Carry the plan out.
    Confirm,
    /// Leave everything as it is.
    Cancel,
}

/// Lay one dialog out so it always fits the window it is shown in.
///
/// The width follows the window, so a narrow window narrows the dialog instead
/// of pushing its controls past the edge.
pub(crate) fn modal<R>(
    ui: &egui::Ui,
    id: egui::Id,
    title: &str,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> Option<R> {
    let screen = ui.ctx().screen_rect();
    let width = (screen.width() - MARGIN * 2.0).clamp(80.0, PREFERRED_WIDTH);
    egui::Window::new(title)
        .id(id)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .max_width(width)
        .default_width(width)
        .show(ui.ctx(), |ui| {
            ui.set_max_width(width);
            add(ui)
        })
        .and_then(|response| response.inner)
}

/// The settings and the plan for one operation, with its two buttons.
///
/// `revised` is set when the plan was built again because a setting changed
/// the steps of the plan confirmed before it.
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
pub fn confirm(
    ui: &egui::Ui,
    id: egui::Id,
    operation: Operation,
    plan: &OperationPlan,
    form: &mut Form,
    offset_seconds: i32,
    revised: bool,
) -> ConfirmChoice {
    modal(ui, id, operation.dialog_title(), |ui| {
        if revised {
            wrapped_text(
                ui,
                "The settings changed the steps. Examine the steps and confirm again.",
            );
            ui.separator();
        }
        let counted = totals(plan);
        ui.label(format!(
            "{} files, {} folders, {} bytes",
            counted.files,
            counted.folders,
            format_bytes(counted.bytes)
        ));
        if plan.steps.is_empty() {
            ui.label("This operation would do nothing.");
        }
        ui.separator();
        egui::ScrollArea::vertical()
            .id_salt(id.with("body"))
            .max_height(BODY_HEIGHT)
            .show(ui, |ui| {
                options_for(ui, operation, form, offset_seconds);
                conflicts(ui, plan);
                steps(ui, plan);
            });
        ui.separator();
        let mut choice = ConfirmChoice::Pending;
        ui.horizontal_wrapped(|ui| {
            let allowed = !plan.steps.is_empty();
            let confirm = ui.add_enabled(allowed, egui::Button::new(confirm_label(operation)));
            if !allowed {
                let _ = ca_ui::widgets::disabled_reason(confirm, "There is nothing to carry out");
            } else if confirm.clicked() {
                choice = ConfirmChoice::Confirm;
            }
            if ui.button("Cancel").clicked() {
                choice = ConfirmChoice::Cancel;
            }
        });
        choice
    })
    .unwrap_or(ConfirmChoice::Cancel)
}

/// The label the button that starts the operation carries.
const fn confirm_label(operation: Operation) -> &'static str {
    match operation {
        Operation::Delete => "Delete",
        Operation::CopyToOtherSide | Operation::CopyToFolder => "Copy",
        Operation::MoveToOtherSide | Operation::MoveToFolder => "Move",
        Operation::Exchange => "Exchange",
        Operation::Rename => "Rename",
        Operation::Touch => "Touch",
        Operation::Attributes => "Apply",
        Operation::NewFolder => "Create",
        Operation::Exclude => "Exclude",
    }
}

/// The controls one operation offers.
#[allow(clippy::too_many_lines)]
fn options_for(ui: &mut egui::Ui, operation: Operation, form: &mut Form, offset_seconds: i32) {
    match operation {
        Operation::Delete => {
            ui.checkbox(
                &mut form.options.use_recycle_bin,
                "Use recycle bin if possible",
            );
            ui.label("Network locations, remote services and archives ignore the recycle bin.");
            ui.checkbox(
                &mut form.options.clear_read_only_targets,
                "Clear the read-only flag rather than failing",
            );
        }
        Operation::CopyToOtherSide
        | Operation::MoveToOtherSide
        | Operation::CopyToFolder
        | Operation::MoveToFolder
        | Operation::Exchange => {
            ui.checkbox(&mut form.options.overwrite, "Replace items at the target");
            ui.checkbox(
                &mut form.options.preserve_modified,
                "Preserve the last modified time",
            );
            ui.checkbox(
                &mut form.options.preserve_attributes,
                "Preserve the attributes",
            );
            let mut backup = form.options.backup.is_some();
            if ui
                .checkbox(&mut backup, "Back a replaced file up")
                .changed()
            {
                form.options.backup = backup.then(|| form.backup_names.clone());
            }
            ui.horizontal_wrapped(|ui| {
                ui.label("Verify");
                for option in [Verify::None, Verify::Size, Verify::Hash] {
                    ui.selectable_value(&mut form.options.verify, option, verify_label(option));
                }
            });
            if operation.needs_target_folder() {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Target folder");
                    ui.add(
                        egui::TextEdit::singleline(&mut form.target_folder).desired_width(260.0),
                    );
                });
                for option in [
                    PathOption::KeepRelative,
                    PathOption::KeepBase,
                    PathOption::Flatten,
                ] {
                    ui.radio_value(&mut form.path_option, option, path_option_label(option));
                }
            }
        }
        Operation::Rename => {
            ui.horizontal_wrapped(|ui| {
                ui.radio_value(&mut form.rename_mode, RenameMode::Mask, "Name mask");
                ui.radio_value(
                    &mut form.rename_mode,
                    RenameMode::Regex,
                    "Regular expression",
                );
            });
            match form.rename_mode {
                RenameMode::Mask => {
                    ui.horizontal_wrapped(|ui| {
                        ui.label("New mask");
                        ui.add(
                            egui::TextEdit::singleline(&mut form.rename_mask).desired_width(240.0),
                        );
                    });
                    ui.label("A question mark copies one character; a star copies the rest.");
                }
                RenameMode::Regex => {
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Old mask");
                        ui.add(
                            egui::TextEdit::singleline(&mut form.rename_find).desired_width(240.0),
                        );
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.label("New mask");
                        ui.add(
                            egui::TextEdit::singleline(&mut form.rename_replace)
                                .desired_width(240.0),
                        );
                    });
                    ui.label("A capture is written back as a dollar sign and its number.");
                }
            }
        }
        Operation::Touch => {
            ui.radio_value(
                &mut form.touch_choice,
                TouchChoice::FromOtherSide,
                "Copy timestamps from the other side",
            );
            ui.radio_value(
                &mut form.touch_choice,
                TouchChoice::Explicit,
                "Set timestamps to",
            );
            ui.horizontal_wrapped(|ui| {
                ui.add_enabled(
                    form.touch_choice == TouchChoice::Explicit,
                    egui::TextEdit::singleline(&mut form.touch_text).desired_width(200.0),
                );
                ui.label("YYYY-MM-DD hh:mm:ss");
            });
            ui.label(format!(
                "Current time: {}",
                format_stamp(Some(std::time::SystemTime::now()), offset_seconds)
            ));
            ui.label("A folder's own timestamp is written; its contents are left alone.");
        }
        Operation::Attributes => {
            for (label, state) in [
                ("Read-only", &mut form.read_only),
                ("Hidden", &mut form.hidden),
                ("Archive", &mut form.archive),
            ] {
                let mut checked = state.target() == Some(true);
                if ui
                    .add(
                        egui::Checkbox::new(&mut checked, label)
                            .indeterminate(state.target().is_none()),
                    )
                    .clicked()
                {
                    *state = state.cycled();
                }
            }
            ui.label("A dash leaves the attribute as each item already has it.");
            ui.label("The system attribute is not edited here.");
        }
        Operation::NewFolder => {
            ui.horizontal_wrapped(|ui| {
                ui.label("Name");
                ui.add(egui::TextEdit::singleline(&mut form.new_folder_name).desired_width(240.0));
            });
            ui.horizontal_wrapped(|ui| {
                for scope in [Scope::Left, Scope::Right, Scope::Both] {
                    ui.selectable_value(&mut form.scope, scope, scope.label());
                }
            });
        }
        Operation::Exclude => {
            ui.checkbox(
                &mut form.exclude_by_type,
                "Exclude every file of this type rather than these names",
            );
        }
    }
}

/// Every conflict the plan reports, grouped by kind.
fn conflicts(ui: &mut egui::Ui, plan: &OperationPlan) {
    let grouped = conflicts_by_kind(plan);
    ui.separator();
    if grouped.is_empty() {
        ui.label("No conflicts reported.");
        return;
    }
    ui.label("Conflicts");
    for (conflict, paths) in grouped {
        ui.label(format!("{} ({})", conflict_heading(conflict), paths.len()));
        for path in paths.iter().take(8) {
            path_line(ui, "    ", path);
        }
        if paths.len() > 8 {
            ui.label(format!("    and {} more", paths.len() - 8));
        }
    }
}

/// The steps themselves, so the plan shown is the plan that runs.
fn steps(ui: &mut egui::Ui, plan: &OperationPlan) {
    ui.separator();
    ui.label(format!("Steps ({})", plan.steps.len()));
    for step in plan.steps.iter().take(200) {
        path_text(ui, &format!("    {}", describe(&step.action)));
    }
    if plan.steps.len() > 200 {
        ui.label(format!("    and {} more", plan.steps.len() - 200));
    }
    let noted: Vec<&ca_fs::PlanSkip> = plan
        .skipped
        .iter()
        .filter(|skip| skip.conflict.is_none())
        .collect();
    if !noted.is_empty() {
        ui.label(format!("Not acted on ({})", noted.len()));
        for skip in noted.iter().take(20) {
            path_text(ui, &format!("    {}: {}", skip.path.display(), skip.reason));
        }
    }
}

/// One step in words.
#[must_use]
pub fn describe(action: &ca_fs::StepAction) -> String {
    use ca_fs::StepAction as A;
    match action {
        A::CreateDir { path } => format!("Create folder {}", path.display()),
        A::CopyFile { source, target } => {
            format!("Copy {} to {}", source.display(), target.display())
        }
        A::MoveFile { source, target } => {
            format!("Move {} to {}", source.display(), target.display())
        }
        A::DeleteFile { path } => format!("Delete file {}", path.display()),
        A::DeleteDir { path } => format!("Delete folder {}", path.display()),
        A::DeleteLink { path } => format!("Delete link {}", path.display()),
        A::Trash { path } => format!("Send to the recycle bin: {}", path.display()),
        A::Rename { from, to } => format!("Rename {} to {}", from.display(), to.display()),
        A::SetTimes { path, .. } => format!("Set the timestamp of {}", path.display()),
        A::SetAttributes { path, .. } => format!("Set the attributes of {}", path.display()),
        A::ExchangeFiles { left, right } => {
            format!("Exchange {} and {}", left.display(), right.display())
        }
    }
}

/// The running batch, with the button that stops it.
#[must_use]
pub fn progress(ui: &egui::Ui, id: egui::Id, state: &ProgressState) -> bool {
    modal(ui, id, "Working", |ui| {
        ui.label(format!(
            "Step {} of {}",
            state.steps_done.min(state.steps_total),
            state.steps_total
        ));
        ui.label(format!(
            "{} of {} bytes",
            format_bytes(state.bytes_done),
            format_bytes(state.bytes_total)
        ));
        path_line(ui, "", &state.current);
        ui.add(
            egui::ProgressBar::new(fraction(state.steps_done, state.steps_total))
                .desired_width((ui.available_width() - MARGIN).max(40.0)),
        );
        ui.horizontal_wrapped(|ui| ui.button("Cancel").clicked())
            .inner
    })
    .unwrap_or(false)
}

fn fraction(done: usize, total: usize) -> f32 {
    if total == 0 {
        return 0.0;
    }
    #[allow(clippy::cast_precision_loss)]
    let value = done as f32 / total as f32;
    value.clamp(0.0, 1.0)
}

/// What one step's outcome says, where it did not simply complete.
#[must_use]
pub fn outcome_text(outcome: &StepOutcome) -> Option<String> {
    match outcome {
        StepOutcome::Done => None,
        StepOutcome::Skipped { reason } => Some(format!("skipped: {reason}")),
        StepOutcome::Failed { message } => Some(format!("failed: {message}")),
        StepOutcome::CopiedSourceRemains {
            source,
            target,
            message,
        } => Some(format!(
            "copied to {} but {} could not be removed: {message}",
            target.display(),
            source.display()
        )),
        StepOutcome::Cancelled => Some("stopped before it ran".to_string()),
    }
}

/// Everything the batch did not complete.
#[must_use]
pub fn summary(
    ui: &egui::Ui,
    id: egui::Id,
    plan: &OperationPlan,
    report: &ExecutionReport,
) -> bool {
    modal(ui, id, "Result", |ui| {
        ui.label(format!(
            "{} of {} steps completed",
            report.completed(),
            plan.steps.len()
        ));
        if report.cancelled {
            ui.label("The batch was stopped before it finished.");
        }
        if report.aborted {
            ui.label("The batch was stopped by an error.");
        }
        let unfinished: Vec<&ca_fs::StepResult> = report
            .results
            .iter()
            .filter(|result| !result.outcome.is_done())
            .collect();
        ui.separator();
        if unfinished.is_empty() {
            ui.label("Every step completed.");
        } else {
            ui.label(format!("{} steps did not complete", unfinished.len()));
        }
        egui::ScrollArea::vertical()
            .id_salt(id.with("body"))
            .max_height(BODY_HEIGHT)
            .show(ui, |ui| {
                for result in unfinished {
                    let action = plan
                        .steps
                        .iter()
                        .find(|step| step.index == result.index)
                        .map_or_else(String::new, |step| describe(&step.action));
                    let text = outcome_text(&result.outcome).unwrap_or_default();
                    wrapped_text(ui, &format!("{action} — {text}"));
                }
            });
        ui.separator();
        ui.horizontal_wrapped(|ui| ui.button("Close").clicked())
            .inner
    })
    .unwrap_or(false)
}

/// A later click of a fast series lands on the question published after the
/// one the series started on, at the same button position, so only the first
/// click of a series answers.
fn single_click(response: &egui::Response) -> bool {
    response.clicked() && !response.double_clicked() && !response.triple_clicked()
}

/// One question the running batch is waiting on.
#[must_use]
pub fn question(ui: &egui::Ui, id: egui::Id, question: &Question) -> Option<Answer> {
    let title = match question {
        Question::Conflict { .. } => "Confirm this item",
        Question::Drifted { .. } => "The disk has changed",
        Question::Failure { .. } => "A step failed",
        Question::RecycleUnavailable { .. } => "Recycle bin unavailable",
    };
    modal(ui, id, title, |ui| {
        let icon = match question {
            Question::Conflict { .. } => ca_ui::icons::Icon::Question,
            Question::Drifted { .. } | Question::RecycleUnavailable { .. } => ca_ui::icons::Icon::Warning,
            Question::Failure { .. } => ca_ui::icons::Icon::Error,
        };
        ca_ui::widgets::notice_current(ui,icon,24.0,title);
        let mut answer = None;
        match question {
            Question::RecycleUnavailable { path, .. } => {
                path_line(ui, "", path);
                wrapped_text(ui, "This location cannot send the item to the recycle bin. Permanently deleting it cannot be undone through the recycle bin.");
            }
            Question::Conflict {
                path, conflicts, ..
            } => {
                path_line(ui, "", path);
                for conflict in conflicts {
                    ui.label(conflict_heading(*conflict));
                }
            }
            Question::Drifted { detail, .. } => {
                wrapped_text(ui, detail);
                ui.label("Nobody has approved what is there now.");
            }
            Question::Failure {
                path,
                message,
                attempt,
                ..
            } => {
                path_line(ui, "", path);
                wrapped_text(ui, message);
                ui.label(format!("Attempt {attempt}"));
            }
        }
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            if matches!(question, Question::Failure { .. }) && single_click(&ui.button("Retry")) {
                answer = Some(Answer::Retry);
            }
            if matches!(question, Question::RecycleUnavailable { .. }) {
                if single_click(&ui.button("Delete permanently")) {
                    answer = Some(Answer::Proceed);
                }
            } else if !matches!(question, Question::Failure { .. }) {
                if single_click(&ui.button("Proceed")) {
                    answer = Some(Answer::Proceed);
                }
                if single_click(&ui.button("Proceed for all")) {
                    answer = Some(Answer::ProceedAll);
                }
            }
            if single_click(&ui.button("Skip")) {
                answer = Some(Answer::Skip);
            }
            if !matches!(question, Question::RecycleUnavailable { .. })
                && single_click(&ui.button("Skip all"))
            {
                answer = Some(Answer::SkipAll);
            }
            if single_click(&ui.button("Stop")) {
                answer = Some(Answer::Abort);
            }
        });
        answer
    })
    .flatten()
}

/// What the user chose about the batches a journal says did not finish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryChoice {
    /// Remove the temporary files one batch left and forget the record.
    CleanUp(std::path::PathBuf),
    /// Leave everything alone and close the notice.
    Dismiss,
}

/// The notice shown when unfinished batches are found.
#[must_use]
pub fn recovery(ui: &egui::Ui, id: egui::Id, notices: &[RecoveryNotice]) -> Option<RecoveryChoice> {
    modal(ui, id, "Unfinished file operations", |ui| {
        let mut choice = None;
        ui.label(format!("{} earlier batches did not finish.", notices.len()));
        ui.label("Nothing is removed until you choose it here.");
        egui::ScrollArea::vertical()
            .id_salt(id.with("body"))
            .max_height(BODY_HEIGHT)
            .show(ui, |ui| {
                for notice in notices {
                    ui.separator();
                    path_line(ui, "", &notice.journal);
                    wrapped_text(ui, &notice.summary);
                    ui.label(format!(
                        "{} open steps, {} part files, {} backups",
                        notice.unfinished,
                        notice.temporaries.len(),
                        notice.backups.len()
                    ));
                    for backup in notice.backups.iter().take(8) {
                        path_line(ui, "    backup kept: ", backup);
                    }
                    if ui
                        .button("Remove the part files and forget this record")
                        .clicked()
                    {
                        choice = Some(RecoveryChoice::CleanUp(notice.journal.clone()));
                    }
                }
            });
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            if ui.button("Leave them alone").clicked() {
                choice = Some(RecoveryChoice::Dismiss);
            }
        });
        choice
    })
    .flatten()
}

/// A plain message with one button.
#[must_use]
pub fn notice(ui: &egui::Ui, id: egui::Id, title: &str, body: &str) -> bool {
    modal(ui, id, title, |ui| {
        ca_ui::widgets::notice_current(ui, ca_ui::icons::Icon::Info, 24.0, body);
        ui.separator();
        ui.horizontal_wrapped(|ui| ui.button("Close").clicked())
            .inner
    })
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    #[test]
    #[allow(clippy::unwrap_used)]
    fn attribute_checkboxes_cycle_all_three_states_by_accessible_label() {
        let mut probe = ca_ui::testing::probe::Probe::new(800.0, 500.0);
        let mut form = crate::operations::Form::default();
        for label in ["Read-only", "Hidden", "Archive"] {
            for expected in [Some(true), Some(false), None] {
                let mut run = |ctx: &egui::Context| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        super::options_for(
                            ui,
                            crate::operations::Operation::Attributes,
                            &mut form,
                            0,
                        );
                    });
                };
                probe.idle(&mut run);
                probe.idle(&mut run);
                probe.click(label, &mut run).unwrap();
                let state = match label {
                    "Read-only" => form.read_only,
                    "Hidden" => form.hidden,
                    _ => form.archive,
                };
                assert_eq!(state.target(), expected);
            }
        }
    }
    use super::{describe, fraction, outcome_text};
    use ca_fs::{StepAction, StepOutcome};
    use std::path::PathBuf;

    #[test]
    fn every_step_kind_describes_itself() {
        let actions = [
            StepAction::CreateDir {
                path: PathBuf::from("a"),
            },
            StepAction::CopyFile {
                source: PathBuf::from("a"),
                target: PathBuf::from("b"),
            },
            StepAction::MoveFile {
                source: PathBuf::from("a"),
                target: PathBuf::from("b"),
            },
            StepAction::DeleteFile {
                path: PathBuf::from("a"),
            },
            StepAction::DeleteDir {
                path: PathBuf::from("a"),
            },
            StepAction::DeleteLink {
                path: PathBuf::from("a"),
            },
            StepAction::Trash {
                path: PathBuf::from("a"),
            },
            StepAction::Rename {
                from: PathBuf::from("a"),
                to: PathBuf::from("b"),
            },
            StepAction::SetTimes {
                path: PathBuf::from("a"),
                modified: None,
                created: None,
            },
            StepAction::SetAttributes {
                path: PathBuf::from("a"),
                change: ca_fs::AttributeChange::default(),
            },
            StepAction::ExchangeFiles {
                left: PathBuf::from("a"),
                right: PathBuf::from("b"),
            },
        ];
        for action in &actions {
            assert!(!describe(action).is_empty());
        }
    }

    #[test]
    fn a_completed_step_has_nothing_to_report() {
        assert_eq!(outcome_text(&StepOutcome::Done), None);
    }

    #[test]
    fn a_move_that_left_its_source_says_so() {
        let text = outcome_text(&StepOutcome::CopiedSourceRemains {
            source: PathBuf::from("from.txt"),
            target: PathBuf::from("to.txt"),
            message: "in use".to_string(),
        })
        .unwrap_or_default();
        assert!(text.contains("from.txt"));
        assert!(text.contains("to.txt"));
        assert!(text.contains("in use"));
    }

    #[test]
    fn progress_never_leaves_the_zero_to_one_range() {
        assert!((fraction(0, 0) - 0.0).abs() < f32::EPSILON);
        assert!((fraction(5, 5) - 1.0).abs() < f32::EPSILON);
        assert!((fraction(9, 5) - 1.0).abs() < f32::EPSILON);
    }

    fn question_frame(
        ctx: &egui::Context,
        sequence: u64,
        question: &crate::opjobs::Question,
        events: Vec<egui::Event>,
    ) -> Option<crate::opjobs::Answer> {
        let mut answer = None;
        let _ = ctx.run(ca_ui::testing::event_input(1_280.0, 800.0, events), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                answer = super::question(ui, egui::Id::new(("question", sequence)), question);
            });
        });
        answer
    }

    fn pointer(position: egui::Pos2, pressed: bool) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(position),
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }

    fn recycle_question(index: usize) -> crate::opjobs::Question {
        crate::opjobs::Question::RecycleUnavailable {
            index,
            path: PathBuf::from(format!("item{index}.txt")),
        }
    }

    fn settled(sequence: u64, question: &crate::opjobs::Question) -> egui::Context {
        let ctx = egui::Context::default();
        for _ in 0..4 {
            let _ = question_frame(&ctx, sequence, question, Vec::new());
        }
        ctx
    }

    /// The point where one click on a settled dialog answers Delete permanently.
    fn delete_button() -> Option<egui::Pos2> {
        let first = recycle_question(0);
        let window = settled(1, &first)
            .memory(|memory| memory.area_rect(egui::Id::new(("question", 1_u64))))?;
        for up in 1..12_u8 {
            for across in 1..40_u8 {
                let position = egui::pos2(
                    window.left() + f32::from(across) * 4.0,
                    window.bottom() - f32::from(up) * 4.0,
                );
                let ctx = settled(1, &first);
                let _ = question_frame(&ctx, 1, &first, pointer(position, true));
                if question_frame(&ctx, 1, &first, pointer(position, false))
                    == Some(crate::opjobs::Answer::Proceed)
                {
                    return Some(position);
                }
            }
        }
        None
    }

    /// The next question opens where the previous one was. The rest of a fast
    /// click series must not consent to removing the next item permanently.
    #[test]
    #[allow(clippy::unwrap_used)]
    fn a_double_click_answers_only_the_question_it_started_on() {
        use crate::opjobs::Answer;
        let button = delete_button().unwrap();
        let (first, next) = (recycle_question(0), recycle_question(1));
        let ctx = settled(1, &first);
        let _ = question_frame(&ctx, 1, &first, pointer(button, true));
        assert_eq!(
            question_frame(&ctx, 1, &first, pointer(button, false)),
            Some(Answer::Proceed)
        );
        for _ in 0..4 {
            assert_eq!(question_frame(&ctx, 2, &next, Vec::new()), None);
        }
        for _ in 0..2 {
            assert_eq!(question_frame(&ctx, 2, &next, pointer(button, true)), None);
            assert_eq!(
                question_frame(&ctx, 2, &next, pointer(button, false)),
                None,
                "a later click of the series answered the next question"
            );
        }
        for _ in 0..60 {
            let _ = question_frame(&ctx, 2, &next, Vec::new());
        }
        let _ = question_frame(&ctx, 2, &next, pointer(button, true));
        assert_eq!(
            question_frame(&ctx, 2, &next, pointer(button, false)),
            Some(Answer::Proceed),
            "a separate click no longer answers"
        );
    }
}
