use super::*;
use ca_ui::editor::{Caret, Motion};

// s0 a, s1 conflict b, s2 ab cd g h i, s3 left change X, s4 f.
const G: [&str; 3] = [
    "a\nL\nab\ncd\ng\nh\ni\nX\nf\n",
    "a\nb\nab\ncd\ng\nh\ni\ne\nf\n",
    "a\nR\nab\ncd\ng\nh\ni\ne\nf\n",
];
const G_TAKEN: &str = "a\nL\nab\ncd\nG\nh\ni\nX\nf\n";
const ALL: [&str; 3] = ["left.txt", "center.txt", "right.txt"];

fn absorb(view: &mut MergeView) {
    view.absorb_output_edits();
    assert_output_lines_match_pane(view);
}

fn at(view: &mut MergeView, line: u32, column: u32) {
    view.output_pane.clear_selection();
    view.output_pane.place(Caret::new(line, column), false);
}

fn join(view: &mut MergeView, line: u32) {
    at(view, line, 0);
    view.output_pane.move_caret(Motion::LineEnd, false);
    view.output_pane.delete();
    absorb(view);
}

fn take(view: &mut MergeView, section: usize, command: Command) {
    view.output_pane.clear_selection();
    view.focus = crate::Pane::Output;
    view.current = section;
    view.run(command);
    assert_output_lines_match_pane(view);
    assert_eq!(view.model().lent_record_problems(), Vec::<String>::new());
}

fn rewrite(dir: &tempfile::TempDir, names: &[&str], from: &str, to: &str) {
    for name in names {
        let path = dir.path().join(name);
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, text.replace(from, to)).unwrap();
    }
}

fn reload(view: &mut MergeView) {
    view.run(Command::Reload);
    run_until_ready(view);
    assert_output_lines_match_pane(view);
    assert_eq!(view.model().lent_record_problems(), Vec::<String>::new());
}

/// Join `ab` of s2 to the conflict line and change that lent text: type `Q`
/// inside it, or remove its `b`. Then change an own line of s2 in every
/// input and reload: s2 leaves out the line `ab` its holder still shows.
fn edited_lent_text_then_a_changed_reload(typed: bool) -> (MergeView, tempfile::TempDir) {
    let (mut view, dir) = open(G[0], Some(G[1]), G[2]);
    run_until_ready(&mut view);
    join(&mut view, 1);
    assert_eq!(view.output_text(), "a\nbab\ncd\ng\nh\ni\nX\nf\n");
    if typed {
        at(&mut view, 1, 2);
        view.output_pane.type_character('Q');
        absorb(&mut view);
        assert_eq!(view.output_text(), "a\nbaQb\ncd\ng\nh\ni\nX\nf\n");
    } else {
        at(&mut view, 1, 3);
        view.output_pane.backspace();
        absorb(&mut view);
        assert_eq!(view.output_text(), "a\nba\ncd\ng\nh\ni\nX\nf\n");
    }
    rewrite(&dir, &ALL, "\ng\n", "\nG\n");
    reload(&mut view);
    (view, dir)
}

#[test]
fn a_changed_reload_after_an_edit_in_lent_text_gives_one_result_in_both_take_orders() {
    for typed in [false, true] {
        for lender_first in [true, false] {
            let (mut view, dir) = edited_lent_text_then_a_changed_reload(typed);
            let order = if lender_first {
                [(2, Command::TakeCenter), (1, Command::TakeLeft)]
            } else {
                [(1, Command::TakeLeft), (2, Command::TakeCenter)]
            };
            for (section, command) in order {
                take(&mut view, section, command);
            }
            let case = format!("typed {typed} lender first {lender_first}");
            assert_eq!(view.output_text(), G_TAKEN, "{case}");
            assert_eq!(
                view.model().sections()[2].resolution,
                Resolution::Center,
                "{case}"
            );
            assert_eq!(save_and_read(&mut view, &dir), G_TAKEN.as_bytes(), "{case}");
        }
    }
}

/// The holder take alone gives the changed text back to s2. s2 then holds
/// other text than its center lines, so it is an edited section, and a Take
/// Center of it is a change.
#[test]
fn a_holder_take_after_a_changed_reload_leaves_the_lender_with_changed_text_edited() {
    for (typed, returned) in [(false, "a"), (true, "aQb")] {
        let (mut view, dir) = edited_lent_text_then_a_changed_reload(typed);
        take(&mut view, 1, Command::TakeLeft);
        let text = format!("a\nL\n{returned}\ncd\nG\nh\ni\nX\nf\n");
        assert_eq!(view.output_text(), text, "typed {typed}");
        assert_eq!(
            view.model().sections()[2].resolution,
            Resolution::Edited,
            "typed {typed}"
        );
        assert_eq!(save_and_read(&mut view, &dir), text.as_bytes());
        take(&mut view, 2, Command::TakeCenter);
        assert_eq!(view.output_text(), G_TAKEN, "typed {typed}");
        assert_eq!(view.model().sections()[2].resolution, Resolution::Center);
    }
}

/// Without an edit in the lent text the holder take gives the line `ab`
/// back unchanged, and s2 shows its center lines again as a center section.
#[test]
fn a_holder_take_after_a_changed_reload_restores_a_lender_whose_text_is_unchanged() {
    let (mut view, dir) = open(G[0], Some(G[1]), G[2]);
    run_until_ready(&mut view);
    join(&mut view, 1);
    rewrite(&dir, &ALL, "\ng\n", "\nG\n");
    reload(&mut view);
    assert_eq!(view.output_text(), "a\nbab\ncd\nG\nh\ni\nX\nf\n");
    take(&mut view, 1, Command::TakeLeft);
    assert_eq!(view.output_text(), G_TAKEN);
    assert_eq!(view.model().sections()[2].resolution, Resolution::Center);
}

/// A line break typed back at a place other than the removed one returns
/// every character of s2 on other lines than its input has. s2 stays an
/// edited section through a reload.
#[test]
fn text_back_on_other_lines_than_the_input_has_stays_edited_through_a_reload() {
    let (mut view, _dir) = open(G[0], Some(G[1]), G[2]);
    run_until_ready(&mut view);
    join(&mut view, 1);
    join(&mut view, 1);
    assert_eq!(view.output_text(), "a\nbabcd\ng\nh\ni\nX\nf\n");
    at(&mut view, 1, 1);
    let ending = view.terminator();
    view.output_pane.enter(ending);
    absorb(&mut view);
    let text = "a\nb\nabcd\ng\nh\ni\nX\nf\n";
    assert_eq!(view.output_text(), text);
    assert_eq!(view.model().sections()[2].resolution, Resolution::Edited);
    reload(&mut view);
    assert_eq!(view.output_text(), text);
    assert_eq!(view.model().sections()[2].resolution, Resolution::Edited);
}
