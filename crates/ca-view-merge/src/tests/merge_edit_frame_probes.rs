use super::*;
use ca_ui::editor::Caret;

const F1_LEFT: &str = "a\nL\nc\nd\ng\nh\ni\nX\nf\n";
const F1_CENTER: &str = "a\nb\nc\nd\ng\nh\ni\ne\nf\n";
const F1_RIGHT: &str = "a\nR\nc\nd\ng\nh\ni\ne\nf\n";
const F1_OUTPUT: &str = "a\nb\nc\nd\ng\nh\ni\nX\nf\n";

fn f1() -> (MergeView, tempfile::TempDir) {
    let (mut view, dir) = open(F1_LEFT, Some(F1_CENTER), F1_RIGHT);
    run_until_ready(&mut view);
    (view, dir)
}

fn absorb(view: &mut MergeView) {
    view.absorb_output_edits();
    assert_output_lines_match_pane(view);
}

fn take_at(view: &mut MergeView, section: usize, command: Command) {
    view.output_pane.clear_selection();
    view.current = section;
    view.run(command);
    assert_output_lines_match_pane(view);
}

fn key_event(key: egui::Key) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    }
}

#[test]
fn undoing_three_undo_groups_typed_in_one_frame_keeps_the_model_on_the_pane() {
    let (mut view, _dir) = f1();
    view.output_pane.place(Caret::new(3, 1), false);
    view.output_pane.type_character('Z');
    view.output_pane.enter("\n");
    view.output_pane.type_character('Y');
    absorb(&mut view);
    let typed = "a\nb\nc\ndZ\nY\ng\nh\ni\nX\nf\n";
    assert_eq!(view.output_text(), typed);
    for _ in 0..3 {
        view.run(Command::Undo);
        assert_output_lines_match_pane(&view);
    }
    assert_eq!(view.output_text(), F1_OUTPUT);
    for _ in 0..3 {
        view.run(Command::Redo);
        assert_output_lines_match_pane(&view);
    }
    assert_eq!(view.output_text(), typed);
}

#[test]
fn undoing_keys_that_one_frame_delivered_keeps_the_model_on_the_pane() {
    let (mut view, _dir) = f1();
    run_until_ready(&mut view);
    view.focus = crate::Pane::Output;
    view.output_pane.place(Caret::new(3, 1), false);
    let _ = frame_output(
        &mut view,
        vec![
            egui::Event::Text("Z".to_owned()),
            key_event(egui::Key::Enter),
            egui::Event::Text("Y".to_owned()),
        ],
    );
    assert_output_lines_match_pane(&view);
    let typed = view.output_text();
    assert_ne!(typed, F1_OUTPUT);
    for _ in 0..3 {
        view.run(Command::Undo);
        assert_output_lines_match_pane(&view);
    }
    assert_eq!(view.output_text(), F1_OUTPUT);
    for _ in 0..3 {
        view.run(Command::Redo);
        assert_output_lines_match_pane(&view);
    }
    assert_eq!(view.output_text(), typed);
}

#[test]
fn a_join_and_typing_in_one_frame_undo_in_two_steps_with_the_model_on_the_pane() {
    let (mut view, _dir) = f1();
    view.output_pane.place(Caret::new(2, 0), false);
    view.output_pane.backspace();
    view.output_pane.type_character('Q');
    absorb(&mut view);
    assert_eq!(view.output_text(), "a\nbQc\nd\ng\nh\ni\nX\nf\n");
    view.run(Command::Undo);
    assert_output_lines_match_pane(&view);
    assert_eq!(view.output_text(), "a\nbc\nd\ng\nh\ni\nX\nf\n");
    view.run(Command::Undo);
    assert_output_lines_match_pane(&view);
    assert_eq!(view.output_text(), F1_OUTPUT);
    assert_eq!(view.model().totals().conflicts_remaining, 1);
}

#[test]
fn redo_after_undoing_typing_that_was_not_yet_folded_keeps_the_model_on_the_pane() {
    let (mut view, _dir) = f1();
    view.output_pane.place(Caret::new(3, 1), false);
    view.output_pane.type_character('a');
    absorb(&mut view);
    view.output_pane.type_character('b');
    view.run(Command::Undo);
    assert_output_lines_match_pane(&view);
    view.run(Command::Redo);
    assert_output_lines_match_pane(&view);
    assert_eq!(view.output_text(), "a\nb\nc\ndab\ng\nh\ni\nX\nf\n");
}

fn two_lenders_on_one_line() -> (MergeView, tempfile::TempDir) {
    let (mut view, dir) = f1();
    assert_eq!(view.model().sections().len(), 5);
    for _ in 0..6 {
        view.output_pane.place(Caret::new(1, 0), false);
        view.output_pane
            .move_caret(ca_ui::editor::Motion::LineEnd, false);
        view.output_pane.delete();
        absorb(&mut view);
    }
    assert_eq!(view.output_text(), "a\nbcdghiX\nf\n");
    (view, dir)
}

#[test]
fn two_lenders_and_the_holder_keep_one_copy_and_honor_selected_inputs() {
    let holder_only = "a\nL\ncdghi\nX\nf\n";
    let lenders_then_holder = "a\nL\nc\nd\ng\nh\ni\nX\nf\n";
    let (mut view, _dir) = two_lenders_on_one_line();
    take_at(&mut view, 1, Command::TakeLeft);
    assert_eq!(view.output_text(), holder_only, "holder");
    let (mut view, _dir) = two_lenders_on_one_line();
    take_at(&mut view, 3, Command::TakeLeft);
    assert_eq!(view.output_text(), "a\nbcdghiX\nf\n", "second lender first");
    take_at(&mut view, 2, Command::TakeCenter);
    assert_eq!(view.output_text(), "a\nbcdghiX\nf\n", "first lender second");
    take_at(&mut view, 1, Command::TakeLeft);
    assert_eq!(view.output_text(), lenders_then_holder, "holder last");
    for _ in 0..3 {
        view.run(Command::Undo);
        assert_output_lines_match_pane(&view);
    }
    assert_eq!(view.output_text(), "a\nbcdghiX\nf\n");
}
#[test]
fn a_line_break_typed_inside_lent_text_keeps_both_parts_with_the_lender() {
    let (mut view, _dir) = open("a\nL\ncd\nz\n", Some("a\nb\ncd\nz\n"), "a\nR\ncd\nz\n");
    run_until_ready(&mut view);
    view.output_pane.place(Caret::new(2, 0), false);
    view.output_pane.backspace();
    absorb(&mut view);
    assert_eq!(view.output_text(), "a\nbcd\nz\n");
    view.output_pane.place(Caret::new(1, 2), false);
    view.output_pane.enter("\n");
    absorb(&mut view);
    assert_eq!(view.output_text(), "a\nbc\nd\nz\n");
    take_at(&mut view, 1, Command::TakeLeft);
    assert_eq!(view.output_text(), "a\nL\nc\nd\nz\n");
}

#[test]
fn deleting_a_character_inside_lent_text_keeps_the_rest_with_the_lender() {
    let (mut view, _dir) = open("a\nL\ncde\nz\n", Some("a\nb\ncde\nz\n"), "a\nR\ncde\nz\n");
    run_until_ready(&mut view);
    view.output_pane.place(Caret::new(2, 0), false);
    view.output_pane.backspace();
    absorb(&mut view);
    view.output_pane.place(Caret::new(1, 3), false);
    view.output_pane.backspace();
    absorb(&mut view);
    assert_eq!(view.output_text(), "a\nbce\nz\n");
    take_at(&mut view, 1, Command::TakeLeft);
    assert_eq!(view.output_text(), "a\nL\nce\nz\n");
}

fn wide_text_round_trip(left: &str, center: &str, right: &str, joined: &str, back: &str) {
    let (mut view, _dir) = open(left, Some(center), right);
    run_until_ready(&mut view);
    view.output_pane.place(Caret::new(2, 0), false);
    view.output_pane.backspace();
    absorb(&mut view);
    assert_eq!(view.output_text(), joined);
    take_at(&mut view, 1, Command::TakeLeft);
    assert_eq!(view.output_text(), back);
    view.run(Command::Undo);
    assert_output_lines_match_pane(&view);
    assert_eq!(view.output_text(), joined);
}

#[test]
fn lent_text_with_combining_marks_emoji_and_tabs_comes_back_whole() {
    wide_text_round_trip(
        "a\nL\ne\u{301}\t\u{1F600}x\nz\n",
        "a\nb\u{301}\ne\u{301}\t\u{1F600}x\nz\n",
        "a\nR\ne\u{301}\t\u{1F600}x\nz\n",
        "a\nb\u{301}e\u{301}\t\u{1F600}x\nz\n",
        "a\nL\ne\u{301}\t\u{1F600}x\nz\n",
    );
}

#[test]
fn backspace_over_a_combining_mark_inside_lent_text_keeps_the_model_on_the_pane() {
    let (mut view, _dir) = open(
        "a\nL\ne\u{301}f\nz\n",
        Some("a\nb\ne\u{301}f\nz\n"),
        "a\nR\ne\u{301}f\nz\n",
    );
    run_until_ready(&mut view);
    view.output_pane.place(Caret::new(2, 0), false);
    view.output_pane.backspace();
    absorb(&mut view);
    view.output_pane.place(Caret::new(1, 3), false);
    view.output_pane.backspace();
    absorb(&mut view);
    take_at(&mut view, 1, Command::TakeLeft);
    let text = view.output_text();
    assert!(
        text.starts_with("a\nL\n") && text.ends_with("f\nz\n"),
        "{text:?}"
    );
}

#[test]
fn lent_text_from_a_utf16_input_with_a_surrogate_pair_comes_back_whole() {
    let utf16 = |text: &str| {
        let mut bytes = vec![0xff, 0xfe];
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes
    };
    let (mut view, _dir) = open_bytes(
        &utf16("a\nL\n\u{1F600}c\nz\n"),
        Some(&utf16("a\nb\n\u{1F600}c\nz\n")),
        &utf16("a\nR\n\u{1F600}c\nz\n"),
    );
    run_until_ready(&mut view);
    view.output_pane.place(Caret::new(2, 0), false);
    view.output_pane.backspace();
    absorb(&mut view);
    assert_eq!(view.output_text(), "a\nb\u{1F600}c\nz\n");
    take_at(&mut view, 1, Command::TakeLeft);
    assert_eq!(view.output_text(), "a\nL\n\u{1F600}c\nz\n");
}

#[test]
fn joins_without_removed_text_keep_line_order_whichever_holder_is_taken_first() {
    let make = || {
        let (mut view, dir) = open(
            "a\nP1\nm\nm2\nm3\nQ1\nc\nd\nz\n",
            Some("a\np\nm\nm2\nm3\nq\nc\nd\nz\n"),
            "a\nP2\nm\nm2\nm3\nQ2\nc\nd\nz\n",
        );
        run_until_ready(&mut view);
        view.output_pane.place(Caret::new(6, 0), false);
        view.output_pane.backspace();
        absorb(&mut view);
        for _ in 0..5 {
            view.output_pane.place(Caret::new(1, 1), false);
            view.output_pane
                .move_caret(ca_ui::editor::Motion::LineEnd, false);
            view.output_pane.delete();
            absorb(&mut view);
        }
        (view, dir)
    };
    let (mut first, _a) = make();
    let joined = first.output_text();
    take_at(&mut first, 1, Command::TakeLeft);
    take_at(&mut first, 3, Command::TakeLeft);
    let (mut second, _b) = make();
    take_at(&mut second, 3, Command::TakeLeft);
    take_at(&mut second, 1, Command::TakeLeft);
    assert_eq!(
        first.output_text(),
        second.output_text(),
        "joined {joined:?}"
    );
    let text = first.output_text();
    let c = text.find('c').unwrap();
    let d = text.find('d').unwrap();
    assert!(c < d, "{text:?}");
    assert!(text.starts_with("a\nP1\n"), "{text:?}");
    assert!(text.contains("Q1"), "{text:?}");
}

#[test]
fn a_plain_load_and_a_save_leave_the_view_unmodified() {
    let (mut view, dir) = f1();
    assert!(!view.is_modified());
    assert!(view.may_close());
    let _ = save_with_markers(&mut view, &dir);
    assert!(!view.is_modified());
    assert_eq!(view.exit_code(), Some(14));
}

#[test]
fn toggling_a_conflict_after_a_save_asks_on_close_and_the_next_save_writes_it() {
    let (mut view, dir) = f1();
    take_at(&mut view, 1, Command::TakeLeft);
    let saved = save_and_read(&mut view, &dir);
    assert_eq!(saved, b"a\nL\nc\nd\ng\nh\ni\nX\nf\n");
    assert_eq!(view.exit_code(), Some(0));
    assert!(!view.is_modified());
    view.current = 1;
    view.run(Command::ToggleConflict);
    assert!(view.is_modified());
    assert!(!view.may_close());
    assert_eq!(view.question(), Some(&Question::CloseModified));
    view.question = None;
    let expected = format!(
        "a\n{}c\nd\ng\nh\ni\nX\nf\n",
        marker_block(&view, "\n", ["L\n", "b\n", "R\n"])
    );
    assert_eq!(save_with_markers(&mut view, &dir), expected);
    assert_eq!(view.exit_code(), Some(14));
    assert!(!view.is_modified());
    view.current = 1;
    view.run(Command::ToggleConflict);
    assert!(view.is_modified());
    view.run(Command::ToggleConflict);
    assert!(!view.is_modified());
}

#[test]
fn toggling_ignored_on_a_waiting_conflict_marks_the_view_modified_until_toggled_back() {
    let (mut view, dir) = f1();
    let _ = save_with_markers(&mut view, &dir);
    assert!(!view.is_modified());
    view.current = 1;
    view.run(Command::ToggleSectionIgnored);
    assert!(view.is_modified());
    let saved = save_and_read(&mut view, &dir);
    assert_eq!(saved, F1_OUTPUT.as_bytes());
    assert_eq!(view.exit_code(), Some(0));
    view.run(Command::ToggleSectionIgnored);
    assert!(view.is_modified());
}

#[test]
fn typing_into_a_merge_of_three_empty_files_undoes_and_redoes() {
    let (mut view, _dir) = open("", Some(""), "");
    run_until_ready(&mut view);
    view.output_pane.place(Caret::new(0, 0), false);
    view.output_pane.paste("x\ny");
    absorb(&mut view);
    view.output_pane.enter("\n");
    absorb(&mut view);
    view.run(Command::Undo);
    assert_output_lines_match_pane(&view);
    view.run(Command::Undo);
    assert_output_lines_match_pane(&view);
    assert_eq!(view.output_text(), "");
    view.run(Command::Redo);
    view.run(Command::Redo);
    assert_output_lines_match_pane(&view);
    assert_eq!(view.output_text(), "x\ny\n");
}

#[test]
fn counters_for_one_edit_in_a_large_document_and_a_large_replace_all() {
    use crate::model::ownership::{PANE_EDITS, RESYNCS};
    use crate::model::MergeModel;
    use std::fmt::Write as _;
    let mut shared = String::new();
    for index in 0..200_000 {
        let _ = writeln!(shared, "line {index} q");
    }
    let (mut view, _dir) = open(
        &format!("a\nL\n{shared}"),
        Some(&format!("a\nb\n{shared}")),
        &format!("a\nR\n{shared}"),
    );
    run_until_ready(&mut view);
    MergeModel::reset_rebuild_visit_counts();
    view.output_pane.place(Caret::new(100_000, 0), false);
    view.output_pane.type_character('Z');
    absorb(&mut view);
    let one_edit = (
        MergeModel::edited_output_read_count(),
        MergeModel::sequence_items_touched(),
    );
    println!("one edit at line 100000 of 200002: {one_edit:?}");
    assert!(one_edit.0 < 1_000 && one_edit.1 < 10_000, "{one_edit:?}");
    MergeModel::reset_rebuild_visit_counts();
    view.output_pane.place(Caret::new(2, 0), false);
    view.output_pane.backspace();
    absorb(&mut view);
    println!(
        "join at the large section: edited_output_reads={} sequence_items={}",
        MergeModel::edited_output_read_count(),
        MergeModel::sequence_items_touched()
    );
    MergeModel::reset_rebuild_visit_counts();
    take_at(&mut view, 1, Command::TakeLeft);
    println!(
        "take on the holder: edited_output_reads={} sequence_items={}",
        MergeModel::edited_output_read_count(),
        MergeModel::sequence_items_touched()
    );
    let (mut view, _dir) = open(
        &format!("a\nL\n{}", &shared[..shared.len() / 40]),
        Some(&format!("a\nb\n{}", &shared[..shared.len() / 40])),
        &format!("a\nR\n{}", &shared[..shared.len() / 40]),
    );
    run_until_ready(&mut view);
    MergeModel::reset_rebuild_visit_counts();
    PANE_EDITS.with(|c| c.set(0));
    RESYNCS.with(|c| c.set(0));
    view.run(Command::Replace);
    view.find_panel().settings.pattern = "q".to_owned();
    view.find_panel().settings.replacement = "Q".to_owned();
    view.answer_panel(Some(crate::PanelRequest::ReplaceAll));
    finish_search(&mut view);
    assert_output_lines_match_pane(&view);
    println!(
        "replace all: lines={} pane_edits={} resyncs={} edited_output_reads={} sequence_items={} message={:?}",
        view.model().output_lines().len(),
        PANE_EDITS.with(std::cell::Cell::get),
        RESYNCS.with(std::cell::Cell::get),
        MergeModel::edited_output_read_count(),
        MergeModel::sequence_items_touched(),
        view.message
    );
    assert_eq!(PANE_EDITS.with(std::cell::Cell::get), 5_694);
    assert_eq!(RESYNCS.with(std::cell::Cell::get), 0);
}

#[test]
fn taking_all_non_conflicting_after_joining_several_lines_of_the_next_section_keeps_one_copy() {
    let (mut view, dir) = f1();
    for _ in 0..5 {
        view.output_pane.place(Caret::new(1, 0), false);
        view.output_pane
            .move_caret(ca_ui::editor::Motion::LineEnd, false);
        view.output_pane.delete();
        absorb(&mut view);
    }
    assert_eq!(view.output_text(), "a\nbcdghi\nX\nf\n");
    view.run(Command::TakeAllNonConflicting);
    assert_output_lines_match_pane(&view);
    assert_eq!(view.output_text(), "a\nbcdghi\nX\nf\n");
    assert_eq!(save_and_read(&mut view, &dir), b"a\nbcdghi\nX\nf\n");
    assert_eq!(view.exit_code(), Some(0));
}

#[test]
fn retaking_a_lender_after_joining_lines_restores_its_selected_input() {
    let (mut view, _dir) = f1();
    for _ in 0..2 {
        view.output_pane.place(Caret::new(1, 0), false);
        view.output_pane
            .move_caret(ca_ui::editor::Motion::LineEnd, false);
        view.output_pane.delete();
        absorb(&mut view);
    }
    assert_eq!(view.output_text(), "a\nbcd\ng\nh\ni\nX\nf\n");
    take_at(&mut view, 2, Command::TakeCenter);
    assert_eq!(view.output_text(), "a\nbcd\ng\nh\ni\nX\nf\n");
    take_at(&mut view, 1, Command::TakeLeft);
    assert_eq!(view.output_text(), "a\nL\nc\nd\ng\nh\ni\nX\nf\n");
}

#[test]
fn taking_another_side_for_a_lender_removes_its_old_text_from_the_holders_line() {
    let (mut view, dir) = f1();
    view.output_pane.place(Caret::new(7, 0), false);
    view.output_pane.backspace();
    absorb(&mut view);
    assert_eq!(view.output_text(), "a\nb\nc\nd\ng\nh\niX\nf\n");
    take_at(&mut view, 3, Command::TakeCenter);
    assert_eq!(view.output_text(), "a\nb\nc\nd\ng\nh\ni\ne\nf\n");
    let expected = format!(
        "a\n{}c\nd\ng\nh\ni\ne\nf\n",
        marker_block(&view, "\n", ["L\n", "b\n", "R\n"])
    );
    assert_eq!(save_with_markers(&mut view, &dir), expected);
}

fn join_reload_take(ending: &str, reload: bool) -> (String, String) {
    let e = ending;
    let text = |side: &str| format!("a{e}{side}{e}c{e}d{e}g{e}");
    let (mut view, _dir) = open(&text("L"), Some(&text("b")), &text("R"));
    run_until_ready(&mut view);
    view.output_pane.place(Caret::new(1, 1), false);
    view.output_pane.place(Caret::new(2, 0), true);
    view.output_pane.paste("[");
    absorb(&mut view);
    if reload {
        view.run(Command::Reload);
        run_until_ready(&mut view);
        assert_output_lines_match_pane(&view);
    }
    take_at(&mut view, 1, Command::TakeLeft);
    (view.output_text(), text("L"))
}

#[test]
fn a_join_kept_through_a_reload_gives_the_text_back_in_every_line_ending() {
    for ending in ["\n", "\r\n", "\r"] {
        for reload in [false, true] {
            let (actual, _) = join_reload_take(ending, reload);
            let e = ending;
            let expected = format!("a{e}L{e}c{e}d{e}g{e}");
            assert_eq!(actual, expected, "ending {ending:?} reload {reload}");
        }
    }
}
