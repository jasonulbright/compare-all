use super::*;
use ca_ui::editor::{Caret, Motion};

const F1_LEFT: &str = "a\nL\nc\nd\ng\nh\ni\nX\nf\n";
const F1_CENTER: &str = "a\nb\nc\nd\ng\nh\ni\ne\nf\n";
const F1_RIGHT: &str = "a\nR\nc\nd\ng\nh\ni\ne\nf\n";

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

fn join_line_one(view: &mut MergeView, times: usize) {
    for _ in 0..times {
        view.output_pane.place(Caret::new(1, 0), false);
        view.output_pane.move_caret(Motion::LineEnd, false);
        view.output_pane.delete();
        absorb(view);
    }
}

fn undo_all(view: &mut MergeView) {
    while view.output_pane.buffer().can_undo() {
        view.run(Command::Undo);
        assert_output_lines_match_pane(view);
    }
}

fn joined_then_middle_removed() -> (MergeView, tempfile::TempDir) {
    let (mut view, dir) = f1();
    join_line_one(&mut view, 5);
    assert_eq!(view.output_text(), "a\nbcdghi\nX\nf\n");
    view.output_pane.place(Caret::new(1, 2), false);
    view.output_pane.place(Caret::new(1, 4), true);
    view.output_pane.backspace();
    absorb(&mut view);
    assert_eq!(view.output_text(), "a\nbchi\nX\nf\n");
    (view, dir)
}

#[test]
fn removing_the_middle_of_joined_text_keeps_the_rest_with_its_section() {
    let (mut view, _dir) = joined_then_middle_removed();
    take_at(&mut view, 1, Command::TakeLeft);
    assert_eq!(view.output_text(), "a\nL\nchi\nX\nf\n");

    let (mut view, _dir) = joined_then_middle_removed();
    take_at(&mut view, 2, Command::TakeCenter);
    assert_eq!(view.output_text(), "a\nbchi\nX\nf\n");
    view.run(Command::TakeAllNonConflicting);
    assert_output_lines_match_pane(&view);
    assert_eq!(view.output_text(), "a\nbchi\nX\nf\n");
    take_at(&mut view, 1, Command::TakeLeft);
    assert_eq!(view.output_text(), "a\nL\nc\nd\ng\nh\ni\nX\nf\n");
    undo_all(&mut view);
    assert_eq!(view.output_text(), "a\nb\nc\nd\ng\nh\ni\nX\nf\n");
}

#[test]
fn deleting_a_line_that_holds_text_of_two_sections_takes_that_text_out_of_both() {
    let (mut view, _dir) = f1();
    join_line_one(&mut view, 6);
    assert_eq!(view.output_text(), "a\nbcdghiX\nf\n");
    view.output_pane.place(Caret::new(1, 0), false);
    view.output_pane.place(Caret::new(2, 0), true);
    view.output_pane.backspace();
    absorb(&mut view);
    assert_eq!(view.output_text(), "a\nf\n");
    take_at(&mut view, 1, Command::TakeLeft);
    assert_eq!(view.output_text(), "a\nL\nf\n");
    take_at(&mut view, 3, Command::TakeLeft);
    assert_eq!(view.output_text(), "a\nL\nX\nf\n");
    take_at(&mut view, 2, Command::TakeCenter);
    assert_eq!(view.output_text(), "a\nL\nc\nd\ng\nh\ni\nX\nf\n");
    for _ in 0..3 {
        view.run(Command::Undo);
        assert_output_lines_match_pane(&view);
    }
    assert_eq!(view.output_text(), "a\nf\n");
    view.run(Command::Undo);
    assert_output_lines_match_pane(&view);
    assert_eq!(view.output_text(), "a\nbcdghiX\nf\n");
    take_at(&mut view, 1, Command::TakeLeft);
    assert_eq!(view.output_text(), "a\nL\ncdghi\nX\nf\n");
}

#[test]
fn a_reload_that_changed_the_lender_and_a_later_section_keeps_one_copy_of_the_joined_text() {
    let (mut view, dir) = f1();
    view.output_pane.place(Caret::new(2, 0), false);
    view.output_pane.backspace();
    absorb(&mut view);
    assert_eq!(view.output_text(), "a\nbc\nd\ng\nh\ni\nX\nf\n");
    for (name, text) in [
        ("left.txt", "a\nL\nc\nD\ng\nh\ni\nX\nF\n"),
        ("center.txt", "a\nb\nc\nD\ng\nh\ni\ne\nF\n"),
        ("right.txt", "a\nR\nc\nD\ng\nh\ni\ne\nF\n"),
    ] {
        std::fs::write(dir.path().join(name), text).unwrap();
    }
    view.run(Command::Reload);
    run_until_ready(&mut view);
    assert_output_lines_match_pane(&view);
    assert_eq!(view.output_text(), "a\nbc\nD\ng\nh\ni\nX\nF\n");
    take_at(&mut view, 1, Command::TakeLeft);
    assert_eq!(view.output_text(), "a\nL\nc\nD\ng\nh\ni\nX\nF\n");
}

#[test]
fn a_reload_that_split_the_lender_keeps_one_copy_of_the_joined_text() {
    let (mut view, dir) = f1();
    join_line_one(&mut view, 2);
    assert_eq!(view.output_text(), "a\nbcd\ng\nh\ni\nX\nf\n");
    std::fs::write(dir.path().join("left.txt"), "a\nL\nc\nd\ng\nH\ni\nX\nf\n").unwrap();
    view.run(Command::Reload);
    run_until_ready(&mut view);
    assert_output_lines_match_pane(&view);
    assert!(view.model().sections().len() > 5);
    assert_eq!(view.output_text(), "a\nbcd\ng\nH\ni\nX\nf\n");
    take_at(&mut view, 1, Command::TakeLeft);
    assert_eq!(view.output_text(), "a\nL\ncd\ng\nH\ni\nX\nf\n");
}

#[test]
fn a_deletion_that_ends_inside_a_line_starting_like_the_kept_text_keeps_that_line_whole() {
    let (mut view, _dir) = open(
        "a\n[u1]\nL\n[u2]\nz\n",
        Some("a\n[u1]\nb\n[u2]\nz\n"),
        "a\n[u1]\nR\n[u2]\nz\n",
    );
    run_until_ready(&mut view);
    view.output_pane.place(Caret::new(1, 2), false);
    view.output_pane.place(Caret::new(3, 2), true);
    view.output_pane.backspace();
    absorb(&mut view);
    assert_eq!(view.output_text(), "a\n[u2]\nz\n");
    take_at(&mut view, 0, Command::TakeLeft);
    assert_eq!(view.output_text(), "a\n[u1]\n[u2]\nz\n");
}

#[test]
fn a_deletion_that_starts_inside_a_line_ending_like_the_removed_text_keeps_that_line_whole() {
    let (mut view, _dir) = open(
        "[u1]\n~\nL\nz\n",
        Some("[u1]\n~\n[u6]\nz\n"),
        "[u1]\n~\nR\nz\n",
    );
    run_until_ready(&mut view);
    view.output_pane.place(Caret::new(2, 0), false);
    view.output_pane.backspace();
    absorb(&mut view);
    assert_eq!(view.output_text(), "[u1]\n~[u6]\nz\n");
    view.output_pane.place(Caret::new(0, 3), false);
    view.output_pane.place(Caret::new(1, 4), true);
    view.output_pane.backspace();
    absorb(&mut view);
    assert_eq!(view.output_text(), "[u1]\nz\n");
    take_at(&mut view, 1, Command::TakeRight);
    assert_eq!(view.output_text(), "[u1]\nR\nz\n");
}

#[test]
fn a_reload_that_changed_a_waiting_lender_keeps_its_joined_text_through_take_all() {
    let (mut view, dir) = open("a\nb\nL\nz\n", Some("a\nb\nc\nz\n"), "a\nb\nR\nz\n");
    run_until_ready(&mut view);
    take_at(&mut view, 1, Command::TakeRight);
    view.output_pane.place(Caret::new(2, 0), false);
    view.output_pane.backspace();
    absorb(&mut view);
    assert_eq!(view.output_text(), "a\nbR\nz\n");
    std::fs::write(dir.path().join("center.txt"), "a\nb\nC\nz\n").unwrap();
    view.run(Command::Reload);
    run_until_ready(&mut view);
    assert_output_lines_match_pane(&view);
    assert_eq!(view.output_text(), "a\nbR\nC\nz\n");
    assert_eq!(view.model().totals().conflicts_remaining, 1);
    view.run(Command::TakeAllNonConflicting);
    assert_output_lines_match_pane(&view);
    assert_eq!(view.output_text(), "a\nb\nR\nC\nz\n");
    assert_eq!(view.model().totals().conflicts_remaining, 1);
}

#[test]
fn a_reload_keeps_the_terminator_the_pane_shows_after_the_next_line_was_deleted() {
    let (mut view, _dir) = open("a\nb\nc\n", Some("a\nb"), "a\nb");
    run_until_ready(&mut view);
    assert_eq!(view.output_text(), "a\nb\nc\n");
    view.output_pane.place(Caret::new(2, 0), false);
    view.output_pane.place(Caret::new(3, 0), true);
    view.output_pane.backspace();
    absorb(&mut view);
    assert_eq!(view.output_text(), "a\nb\n");
    view.run(Command::Reload);
    run_until_ready(&mut view);
    assert_output_lines_match_pane(&view);
    assert_eq!(view.output_text(), "a\nb\n");
    view.run(Command::Undo);
    assert_output_lines_match_pane(&view);
}

#[test]
fn a_paste_over_lines_of_several_sections_keeps_the_character_before_it_with_the_pasted_text() {
    let (mut view, _dir) = open(
        "[s3]\n[s4]\n[u5]\n[u6]\n[u7]\n[c8]\n\n[l11]\n[u12]\n[u13]\n",
        Some("[c1]\n[c2]\n[u5]\n[u6]\n[u7]\n[c8]\n\n[c10]\n[u12]\n[u13]\n"),
        "[s3]\n[s4]\n[u5]\n[u6]\n[u7]\n[r9]\n\n[u12]\n[u13]\n",
    );
    run_until_ready(&mut view);
    view.output_pane.place(Caret::new(1, 1), false);
    view.output_pane.place(Caret::new(8, 1), true);
    view.output_pane.paste("[p1a]\n~\n[p1b]");
    absorb(&mut view);
    assert_eq!(view.output_text(), "[s3]\n[[p1a]\n~\n[p1b]u12]\n[u13]\n");
    // The removed text ends with the "[" the selection kept, so the deletion
    // keeps the line "[u12]" whole and the paste follows that line's "[".
    take_at(&mut view, 0, Command::TakeLeftThenRight);
    assert_eq!(
        view.output_text(),
        "[s3]\n[s4]\n[s3]\n[s4]\n[[p1a]\n~\n[p1b]u12]\n[u13]\n"
    );
    undo_all(&mut view);
    assert_eq!(
        view.output_text(),
        "[s3]\n[s4]\n[u5]\n[u6]\n[u7]\n[c8]\n\n[c10]\n[u12]\n[u13]\n"
    );
}

#[test]
fn swap_sides_that_gives_text_back_to_a_holder_keeps_its_records_on_their_text() {
    let (mut view, _dir) = open(
        "[c1]\n[u3]\n[l5]\n[u8]\n[u9]\n[u10]\n[l11]\n[l12]\n[u13]\n[l16]\n[u17]\n[u18]\n",
        Some("[c1]\n[u3]\n[c4]\n[u8]\n[u9]\n[u10]\n[u13]\n[c14]\n[c15]\n[u17]\n[u18]\n"),
        "[r2]\n[u3]\n[r6]\n[r7]\n[u8]\n[u9]\n[u10]\n[u13]\n[c14]\n[c15]\n[u17]\n[u18]",
    );
    run_until_ready(&mut view);
    view.output_pane.place(Caret::new(5, 5), false);
    view.output_pane.place(Caret::new(11, 0), true);
    view.output_pane.paste("[p1a]\n~\n[p1b]");
    absorb(&mut view);
    view.output_pane.place(Caret::new(2, 2), false);
    view.output_pane.place(Caret::new(4, 3), true);
    view.output_pane.backspace();
    absorb(&mut view);
    view.output_pane.place(Caret::new(4, 0), false);
    view.output_pane.backspace();
    absorb(&mut view);
    assert_eq!(
        view.output_text(),
        "[c1]\n[u3]\n[c]\n[u10][p1a]~\n[p1b][u18]\n"
    );
    view.run(Command::SwapSides);
    run_until_ready(&mut view);
    assert_output_lines_match_pane(&view);
    assert_eq!(view.model().lent_record_problems(), Vec::<String>::new());
    println!("after swap: {:?}", view.output_text());
    let holder = view
        .model()
        .sections()
        .iter()
        .position(crate::model::Section::holds_joined_lines)
        .unwrap();
    take_at(&mut view, holder, Command::TakeLeft);
    println!("after holder take: {:?}", view.output_text());
    assert_eq!(view.model().lent_record_problems(), Vec::<String>::new());
    assert_eq!(view.output_text().matches("[u18]").count(), 1);
}
