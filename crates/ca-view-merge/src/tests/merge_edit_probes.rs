use super::*;
use ca_ui::editor::Caret;

const F1_LEFT: &str = "a\nL\nc\nd\ng\nh\ni\nX\nf\n";
const F1_CENTER: &str = "a\nb\nc\nd\ng\nh\ni\ne\nf\n";
const F1_RIGHT: &str = "a\nR\nc\nd\ng\nh\ni\ne\nf\n";
const F1_OUTPUT: &str = "a\nb\nc\nd\ng\nh\ni\nX\nf\n";

fn f1() -> (MergeView, tempfile::TempDir) {
    let (mut view, dir) = open(F1_LEFT, Some(F1_CENTER), F1_RIGHT);
    run_until_ready(&mut view);
    assert_eq!(view.model().sections().len(), 5);
    (view, dir)
}

fn backspace_at(view: &mut MergeView, line: u32, index: u32) {
    view.output_pane.place(Caret::new(line, index), false);
    view.output_pane.backspace();
    view.absorb_output_edits();
    assert_output_lines_match_pane(view);
}

fn delete_at(view: &mut MergeView, line: u32, index: u32) {
    view.output_pane.place(Caret::new(line, index), false);
    view.output_pane.delete();
    view.absorb_output_edits();
    assert_output_lines_match_pane(view);
}

fn select(view: &mut MergeView, from: (u32, u32), to: (u32, u32)) {
    view.focus = crate::Pane::Output;
    view.output_pane.place(Caret::new(from.0, from.1), false);
    view.output_pane.place(Caret::new(to.0, to.1), true);
}

fn delete_selection(view: &mut MergeView, from: (u32, u32), to: (u32, u32)) {
    select(view, from, to);
    view.output_pane.backspace();
    view.absorb_output_edits();
    assert_output_lines_match_pane(view);
}

fn join_b_c(view: &mut MergeView) {
    backspace_at(view, 2, 0);
    assert_eq!(view.output_text(), "a\nbc\nd\ng\nh\ni\nX\nf\n");
}

fn take_at(view: &mut MergeView, section: usize, command: Command) {
    view.output_pane.clear_selection();
    view.current = section;
    view.run(command);
    assert_output_lines_match_pane(view);
}

fn undo_to_bottom(view: &mut MergeView) {
    for _ in 0..20 {
        view.run(Command::Undo);
        assert_output_lines_match_pane(view);
    }
}

fn conflicts(view: &MergeView) -> u32 {
    view.model().totals().conflicts_remaining
}

#[test]
fn a_take_after_deleting_the_joined_line_does_not_restore_its_text() {
    let (mut view, _dir) = f1();
    join_b_c(&mut view);
    delete_selection(&mut view, (1, 0), (2, 0));
    assert_eq!(view.output_text(), "a\nd\ng\nh\ni\nX\nf\n");
    take_at(&mut view, 1, Command::TakeLeft);
    assert_eq!(view.output_text(), "a\nL\nd\ng\nh\ni\nX\nf\n");
}

#[test]
fn a_take_after_removing_the_joined_text_does_not_restore_it() {
    let (mut view, _dir) = f1();
    join_b_c(&mut view);
    backspace_at(&mut view, 1, 2);
    assert_eq!(view.output_text(), "a\nb\nd\ng\nh\ni\nX\nf\n");
    take_at(&mut view, 1, Command::TakeLeft);
    assert_eq!(view.output_text(), "a\nL\nd\ng\nh\ni\nX\nf\n");
}

fn two_holders() -> (MergeView, tempfile::TempDir) {
    let (mut view, dir) = open(
        "a\nP1\nm\nm2\nm3\nQ1\nc\nd\nz\n",
        Some("a\np\nm\nm2\nm3\nq\nc\nd\nz\n"),
        "a\nP2\nm\nm2\nm3\nQ2\nc\nd\nz\n",
    );
    run_until_ready(&mut view);
    assert_eq!(view.model().sections().len(), 5);
    backspace_at(&mut view, 6, 0);
    assert_eq!(view.output_text(), "a\np\nm\nm2\nm3\nqc\nd\nz\n");
    select(&mut view, (1, 1), (6, 1));
    view.output_pane.type_character('Z');
    view.absorb_output_edits();
    assert_output_lines_match_pane(&view);
    assert_eq!(view.output_text(), "a\npZ\nz\n");
    (view, dir)
}

#[test]
fn a_section_lending_to_two_holders_keeps_its_line_order_whichever_holder_is_taken_first() {
    let expected = "a\nP1\nQ1\nz\n";
    let (mut view, _dir) = two_holders();
    take_at(&mut view, 1, Command::TakeLeft);
    take_at(&mut view, 3, Command::TakeLeft);
    assert_eq!(view.output_text(), expected, "outer holder first");
    let (mut view, _dir) = two_holders();
    take_at(&mut view, 3, Command::TakeLeft);
    take_at(&mut view, 1, Command::TakeLeft);
    assert_eq!(view.output_text(), expected, "inner holder first");
}

#[test]
fn a_holder_taken_undone_and_taken_from_the_other_side_keeps_every_line() {
    let (mut view, _dir) = f1();
    join_b_c(&mut view);
    take_at(&mut view, 1, Command::TakeLeft);
    assert_eq!(view.output_text(), "a\nL\nc\nd\ng\nh\ni\nX\nf\n");
    view.run(Command::Undo);
    assert_output_lines_match_pane(&view);
    assert_eq!(view.output_text(), "a\nbc\nd\ng\nh\ni\nX\nf\n");
    take_at(&mut view, 1, Command::TakeRight);
    assert_eq!(view.output_text(), "a\nR\nc\nd\ng\nh\ni\nX\nf\n");
    view.run(Command::Undo);
    view.run(Command::Undo);
    assert_output_lines_match_pane(&view);
    assert_eq!(view.output_text(), F1_OUTPUT);
    assert_eq!(conflicts(&view), 1);
}

#[test]
fn a_marked_or_ignored_lender_keeps_its_mark_when_its_line_comes_back() {
    let (mut view, dir) = f1();
    backspace_at(&mut view, 7, 0);
    assert_eq!(view.output_text(), "a\nb\nc\nd\ng\nh\niX\nf\n");
    view.output_pane.clear_selection();
    view.current = 3;
    view.run(Command::ToggleConflict);
    assert_eq!(conflicts(&view), 2);
    take_at(&mut view, 2, Command::TakeCenter);
    assert_eq!(view.output_text(), "a\nb\nc\nd\ng\nh\ni\nX\nf\n");
    assert_eq!(conflicts(&view), 2);
    let expected = format!(
        "a\n{}c\nd\ng\nh\ni\n{}f\n",
        marker_block(&view, "\n", ["L\n", "b\n", "R\n"]),
        marker_block(&view, "\n", ["X\n", "e\n", "e\n"])
    );
    assert_eq!(save_with_markers(&mut view, &dir), expected);
    assert_eq!(view.exit_code(), Some(14));

    let (mut view, _dir) = f1();
    backspace_at(&mut view, 7, 0);
    view.output_pane.clear_selection();
    view.current = 3;
    view.run(Command::ToggleSectionIgnored);
    take_at(&mut view, 2, Command::TakeCenter);
    assert_eq!(view.output_text(), "a\nb\nc\nd\ng\nh\ni\nX\nf\n");
    assert_eq!(conflicts(&view), 1);
    assert!(view.model().sections()[3].ignored);
}

#[test]
fn giving_back_a_conflicts_whole_text_restores_its_wait_for_review() {
    let (mut view, dir) = open("a\nk\nL\nz\n", Some("a\nk\nb\nz\n"), "a\nk\nR\nz\n");
    run_until_ready(&mut view);
    assert_eq!(conflicts(&view), 1);
    delete_at(&mut view, 1, 1);
    assert_eq!(view.output_text(), "a\nkb\nz\n");
    take_at(&mut view, 0, Command::TakeCenter);
    assert_eq!(view.output_text(), "a\nk\nb\nz\n");
    assert_eq!(conflicts(&view), 1);
    let expected = format!(
        "a\nk\n{}z\n",
        marker_block(&view, "\n", ["L\n", "b\n", "R\n"])
    );
    assert_eq!(save_with_markers(&mut view, &dir), expected);
    assert_eq!(view.exit_code(), Some(14));
}

#[test]
fn taking_one_line_over_a_joined_line_keeps_the_next_sections_line() {
    let (mut view, _dir) = f1();
    join_b_c(&mut view);
    view.output_pane.clear_selection();
    view.row = view.model().row_of_section(1).unwrap();
    view.run(Command::TakeLeftLine);
    assert_output_lines_match_pane(&view);
    assert_eq!(view.output_text(), "a\nL\nc\nd\ng\nh\ni\nX\nf\n");
    take_at(&mut view, 1, Command::TakeRight);
    assert_eq!(view.output_text(), "a\nR\nc\nd\ng\nh\ni\nX\nf\n");
}

#[test]
fn taking_all_non_conflicting_after_joining_an_unchanged_line_with_a_left_change_restores_both() {
    let (mut view, _dir) = f1();
    backspace_at(&mut view, 7, 0);
    view.run(Command::TakeAllNonConflicting);
    assert_output_lines_match_pane(&view);
    assert_eq!(view.output_text(), "a\nb\nc\nd\ng\nh\ni\nX\nf\n");
    assert_eq!(conflicts(&view), 1);
    view.run(Command::Undo);
    assert_output_lines_match_pane(&view);
    assert_eq!(view.output_text(), "a\nb\nc\nd\ng\nh\niX\nf\n");
}

#[test]
fn taking_all_non_conflicting_after_a_conflict_absorbed_the_next_line_keeps_one_copy() {
    let (mut view, dir) = open(
        "a\nL\nc\nd\ng\nh\ni\nX\nf\nf2\nf3\nM1\nz\n",
        Some("a\nb\nc\nd\ng\nh\ni\ne\nf\nf2\nf3\nm\nz\n"),
        "a\nR\nc\nd\ng\nh\ni\ne\nf\nf2\nf3\nM2\nz\n",
    );
    run_until_ready(&mut view);
    assert_eq!(view.model().sections().len(), 7);
    assert_eq!(conflicts(&view), 2);
    backspace_at(&mut view, 2, 0);
    assert_eq!(
        view.output_text(),
        "a\nbc\nd\ng\nh\ni\nX\nf\nf2\nf3\nm\nz\n"
    );
    assert_eq!(conflicts(&view), 1);
    view.run(Command::TakeAllNonConflicting);
    assert_output_lines_match_pane(&view);
    assert_eq!(
        view.output_text(),
        "a\nbc\nd\ng\nh\ni\nX\nf\nf2\nf3\nm\nz\n"
    );
    assert_eq!(conflicts(&view), 1);
    let expected = format!(
        "a\nbc\nd\ng\nh\ni\nX\nf\nf2\nf3\n{}z\n",
        marker_block(&view, "\n", ["M1\n", "m\n", "M2\n"])
    );
    assert_eq!(save_with_markers(&mut view, &dir), expected);
    assert_eq!(view.exit_code(), Some(14));
}

#[test]
fn replace_all_in_two_conflicts_keeps_the_untouched_conflict_between_them() {
    let (mut view, dir) = open(
        "a\nL1q\nm\nm2\nm3\nLc\nn\nn2\nn3\nL3q\nf\n",
        Some("a\nbq\nm\nm2\nm3\nc\nn\nn2\nn3\neq\nf\n"),
        "a\nR1q\nm\nm2\nm3\nRc\nn\nn2\nn3\nR3q\nf\n",
    );
    run_until_ready(&mut view);
    assert_eq!(view.model().sections().len(), 7);
    assert_eq!(conflicts(&view), 3);
    view.run(Command::Replace);
    view.find_panel().settings.pattern = "q".to_owned();
    view.find_panel().settings.replacement = "Q".to_owned();
    view.answer_panel(Some(crate::PanelRequest::ReplaceAll));
    finish_search(&mut view);
    assert_output_lines_match_pane(&view);
    assert_eq!(
        view.output_text(),
        "a\nbQ\nm\nm2\nm3\nc\nn\nn2\nn3\neQ\nf\n"
    );
    assert_eq!(conflicts(&view), 1);
    take_at(&mut view, 1, Command::TakeLeft);
    assert_eq!(
        view.output_text(),
        "a\nL1q\nm\nm2\nm3\nc\nn\nn2\nn3\neQ\nf\n"
    );
    assert_eq!(conflicts(&view), 1);
    let expected = format!(
        "a\nL1q\nm\nm2\nm3\n{}n\nn2\nn3\neQ\nf\n",
        marker_block(&view, "\n", ["Lc\n", "c\n", "Rc\n"])
    );
    assert_eq!(save_with_markers(&mut view, &dir), expected);
    assert_eq!(view.exit_code(), Some(14));
}

#[test]
fn deleting_a_conflict_line_equal_to_the_next_sections_first_line_keeps_that_line() {
    let (mut view, _dir) = open("a\nL\nx\nz\n", Some("a\nx\nx\nz\n"), "a\nR\nx\nz\n");
    run_until_ready(&mut view);
    assert_eq!(view.model().output_range(1), Some(1..2));
    assert!(view.model().sections()[1].is_unresolved_conflict());
    delete_selection(&mut view, (1, 0), (2, 0));
    assert_eq!(view.output_text(), "a\nx\nz\n");
    assert_eq!(conflicts(&view), 0);
    take_at(&mut view, 1, Command::TakeLeft);
    assert_eq!(view.output_text(), "a\nL\nx\nz\n");
}

#[test]
fn taking_the_lender_after_an_undone_typing_keeps_one_copy_of_the_joined_line() {
    let (mut view, _dir) = f1();
    join_b_c(&mut view);
    view.output_pane.type_character('Z');
    view.absorb_output_edits();
    assert_eq!(view.output_text(), "a\nbZc\nd\ng\nh\ni\nX\nf\n");
    view.run(Command::Undo);
    assert_output_lines_match_pane(&view);
    assert_eq!(view.output_text(), "a\nbc\nd\ng\nh\ni\nX\nf\n");
    take_at(&mut view, 2, Command::TakeCenter);
    assert_eq!(view.output_text(), "a\nbc\nd\ng\nh\ni\nX\nf\n");
    view.run(Command::Undo);
    take_at(&mut view, 1, Command::TakeLeft);
    assert_eq!(view.output_text(), "a\nL\nc\nd\ng\nh\ni\nX\nf\n");
}

#[test]
fn undoing_a_join_restores_the_conflict_and_a_later_take_of_the_next_section_changes_nothing() {
    let (mut view, _dir) = f1();
    join_b_c(&mut view);
    view.run(Command::Undo);
    assert_output_lines_match_pane(&view);
    assert_eq!(view.output_text(), F1_OUTPUT);
    assert_eq!(conflicts(&view), 1);
    take_at(&mut view, 2, Command::TakeCenter);
    assert_eq!(view.output_text(), F1_OUTPUT);
    assert_eq!(conflicts(&view), 1);
}

#[test]
fn undoing_typing_in_a_conflict_restores_the_conflict() {
    let (mut view, dir) = f1();
    view.output_pane.place(Caret::new(1, 0), false);
    view.output_pane.type_character('Z');
    view.absorb_output_edits();
    assert_eq!(conflicts(&view), 0);
    view.run(Command::Undo);
    assert_output_lines_match_pane(&view);
    assert_eq!(view.output_text(), F1_OUTPUT);
    assert_eq!(conflicts(&view), 1);
    let expected = format!(
        "a\n{}c\nd\ng\nh\ni\nX\nf\n",
        marker_block(&view, "\n", ["L\n", "b\n", "R\n"])
    );
    assert_eq!(save_with_markers(&mut view, &dir), expected);
    assert_eq!(view.exit_code(), Some(14));
}

#[test]
fn a_reload_after_the_lenders_region_changed_keeps_one_copy_of_the_joined_line() {
    let (mut view, dir) = f1();
    join_b_c(&mut view);
    std::fs::write(dir.path().join("left.txt"), "a\nL\nc\nD\ng\nh\ni\nX\nf\n").unwrap();
    std::fs::write(dir.path().join("center.txt"), "a\nb\nc\nD\ng\nh\ni\ne\nf\n").unwrap();
    std::fs::write(dir.path().join("right.txt"), "a\nR\nc\nD\ng\nh\ni\ne\nf\n").unwrap();
    view.run(Command::Reload);
    run_until_ready(&mut view);
    assert_output_lines_match_pane(&view);
    assert_eq!(view.output_text(), "a\nbc\nD\ng\nh\ni\nX\nf\n");
    take_at(&mut view, 1, Command::TakeLeft);
    assert_eq!(view.output_text(), "a\nL\nc\nD\ng\nh\ni\nX\nf\n");
}

#[test]
fn a_reload_after_the_holders_region_changed_gives_the_joined_line_back() {
    let (mut view, dir) = f1();
    join_b_c(&mut view);
    std::fs::write(dir.path().join("left.txt"), "a\nL2\nc\nd\ng\nh\ni\nX\nf\n").unwrap();
    view.run(Command::Reload);
    run_until_ready(&mut view);
    assert_output_lines_match_pane(&view);
    assert_eq!(view.output_text(), F1_OUTPUT);
    assert_eq!(conflicts(&view), 1);
}

#[test]
fn swap_sides_and_rule_changes_after_a_join_keep_every_line() {
    let (mut view, _dir) = f1();
    join_b_c(&mut view);
    view.run(Command::SwapSides);
    run_until_ready(&mut view);
    assert_output_lines_match_pane(&view);
    assert_eq!(view.output_text(), "a\nbc\nd\ng\nh\ni\nX\nf\n");
    take_at(&mut view, 1, Command::TakeRight);
    assert_eq!(view.output_text(), "a\nL\nc\nd\ng\nh\ni\nX\nf\n");
    view.run(Command::ToggleIgnoreSameChanges);
    view.run(Command::ToggleIgnoreUnimportant);
    assert_eq!(view.output_text(), "a\nL\nc\nd\ng\nh\ni\nX\nf\n");
}

#[test]
fn a_join_and_a_take_after_it_read_only_the_lent_lines_of_a_large_section() {
    use crate::model::MergeModel;
    use std::fmt::Write as _;
    let mut shared = String::new();
    for index in 0..20_000 {
        let _ = writeln!(shared, "shared {index}");
    }
    let (mut view, _dir) = open(
        &format!("a\nL\n{shared}"),
        Some(&format!("a\nb\n{shared}")),
        &format!("a\nR\n{shared}"),
    );
    run_until_ready(&mut view);
    MergeModel::reset_rebuild_visit_counts();
    backspace_at(&mut view, 2, 0);
    let join = (
        MergeModel::edited_output_read_count(),
        MergeModel::sequence_items_touched(),
    );
    MergeModel::reset_rebuild_visit_counts();
    take_at(&mut view, 1, Command::TakeLeft);
    let take = (
        MergeModel::edited_output_read_count(),
        MergeModel::sequence_items_touched(),
    );
    assert_eq!(view.model().output_lines()[1].as_str(), "L\n");
    assert_eq!(view.model().output_lines()[2].as_str(), "shared 0\n");
    assert!(join.0 < 10_000 && join.1 < 10_000, "join {join:?}");
    assert!(take.0 < 10_000 && take.1 < 10_000, "take {take:?}");
}

const UNDO_ALL_INPUTS: [&str; 3] = [
    "[l1]\r\n[u3]\r\n~\r\n[l5]\r\n[u7]\r\n[u8]\r\n[l9]\r\n[u12]\r\n[s14]\r\n[u15]",
    "[u3]\r\n~\r\n[c4]\r\n[u7]\r\n[u8]\r\n[u12]\r\n[c13]\r\n[u15]\r\n",
    "[r2]\r\n[u3]\r\n~\r\n[r6]\r\n[u7]\r\n[u8]\r\n[r10]\r\n[r11]\r\n[u12]\r\n[s14]\r\n[u15]",
];

#[test]
fn undoing_every_edit_then_taking_matches_a_fresh_merge() {
    let (mut view, _dir) = open(
        UNDO_ALL_INPUTS[0],
        Some(UNDO_ALL_INPUTS[1]),
        UNDO_ALL_INPUTS[2],
    );
    run_until_ready(&mut view);
    let initial = view.output_text();
    let first = u32::try_from(view.output_pane.line_text(0).chars().count()).unwrap();
    delete_at(&mut view, 0, first);
    view.output_pane.place(Caret::new(4, 5), false);
    let ending = view.terminator();
    view.output_pane.enter(ending);
    view.absorb_output_edits();
    assert_output_lines_match_pane(&view);
    backspace_at(&mut view, 7, 0);
    delete_selection(&mut view, (2, 0), (7, 0));
    assert_eq!(view.output_text(), "[u3]~\r\n[c4]\r\n");
    undo_to_bottom(&mut view);
    assert_eq!(view.output_text(), initial);
    let waiting = conflicts(&view);
    take_at(&mut view, 0, Command::TakeLeft);
    assert_eq!(
        view.output_text(),
        "[l1]\r\n[u3]\r\n~\r\n[l5]\r\n[u7]\r\n[u8]\r\n[l9]\r\n[u12]\r\n[s14]\r\n[u15]\r\n"
    );
    assert_eq!(waiting, 1);
}

#[test]
fn undoing_an_edit_at_the_output_end_returns_the_lines_to_their_own_sections() {
    let (mut view, _dir) = open(
        "~\r[l2]\n[u3]\r[u4]\n[l5]\n[u8]\r\n[l10]\n~",
        Some("~\r\n[c1]\n[u3]\r\n[u4]\r\n[u8]\r[c9]\r\n~\r"),
        "~\r\n[c1]\r[u3]\r\n[u4]\n[r6]\r[r7]\n[u8]\r\n~\r",
    );
    run_until_ready(&mut view);
    let initial = view.output_text();
    select(&mut view, (7, 0), (2, 0));
    let ending = view.terminator();
    view.output_pane.enter(ending);
    view.absorb_output_edits();
    assert_output_lines_match_pane(&view);
    view.output_pane.clear_selection();
    view.current = 4;
    select(&mut view, (1, 2), (0, 0));
    view.run(Command::TakeCenter);
    assert_output_lines_match_pane(&view);
    view.output_pane.clear_selection();
    let end = u32::try_from(view.output_pane.line_text(2).chars().count()).unwrap();
    delete_at(&mut view, 2, end);
    undo_to_bottom(&mut view);
    assert_eq!(view.output_text(), initial);
    let waiting = conflicts(&view);
    take_at(&mut view, 4, Command::TakeLeft);
    assert_eq!(
        view.output_text(),
        "~\r\n[l2]\n[u3]\r\n[u4]\r\n[u8]\r[c9]\r\n~"
    );
    assert_eq!(waiting, 1);
}
