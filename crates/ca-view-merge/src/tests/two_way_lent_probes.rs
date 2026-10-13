use super::*;
use ca_ui::editor::{Caret, Motion};

// A two way merge: s0 a, s1 conflict L or R, s2 ab cd g h. s2 shows the
// left input, which stands in for the ancestor.
const TWO: [&str; 2] = ["a\nL\nab\ncd\ng\nh\n", "a\nR\nab\ncd\ng\nh\n"];
const BOTH: [&str; 2] = ["left.txt", "right.txt"];

fn absorb(view: &mut MergeView) {
    view.absorb_output_edits();
    assert_output_lines_match_pane(view);
}

fn at(view: &mut MergeView, line: u32, column: u32) {
    view.output_pane.clear_selection();
    view.output_pane.place(Caret::new(line, column), false);
}

/// Open the two way merge and join line 1 `times` times: s1 then holds the
/// first lines of s2.
fn joined(times: usize) -> (MergeView, tempfile::TempDir) {
    let (mut view, dir) = open(TWO[0], None, TWO[1]);
    run_until_ready(&mut view);
    assert!(view.paths().is_two_way());
    assert_eq!(view.model().sections().len(), 3);
    assert_eq!(view.output_text(), TWO[0]);
    for _ in 0..times {
        at(&mut view, 1, 0);
        view.output_pane.move_caret(Motion::LineEnd, false);
        view.output_pane.delete();
        absorb(&mut view);
    }
    (view, dir)
}

fn take(view: &mut MergeView, section: usize, command: Command) {
    view.output_pane.clear_selection();
    view.focus = crate::Pane::Output;
    view.current = section;
    view.run(command);
    assert_output_lines_match_pane(view);
    assert_eq!(view.model().lent_record_problems(), Vec::<String>::new());
}

fn rewrite(dir: &tempfile::TempDir, from: &str, to: &str) {
    for name in BOTH {
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

/// Every edit before the takes, then the lender take and the holder take
/// in both orders: one text, the lender a left section with its left lines.
#[test]
fn a_two_way_lender_take_and_holder_take_give_one_result_in_both_orders() {
    let edits: [(&str, &dyn Fn(&mut MergeView)); 4] = [
        ("none", &|_| {}),
        ("type", &|view| {
            at(view, 1, 2);
            view.output_pane.type_character('Q');
            absorb(view);
        }),
        ("backspace", &|view| {
            at(view, 1, 3);
            view.output_pane.backspace();
            absorb(view);
        }),
        ("enter", &|view| {
            at(view, 1, 2);
            let ending = view.terminator();
            view.output_pane.enter(ending);
            absorb(view);
        }),
    ];
    for (name, edit) in edits {
        for joins in [1, 2] {
            for lender_first in [true, false] {
                let (mut view, dir) = joined(joins);
                edit(&mut view);
                let order = if lender_first {
                    [(2, Command::TakeLeft), (1, Command::TakeRight)]
                } else {
                    [(1, Command::TakeRight), (2, Command::TakeLeft)]
                };
                for (section, command) in order {
                    take(&mut view, section, command);
                }
                let case = format!("{name} joins {joins} lender first {lender_first}");
                assert_eq!(view.output_text(), TWO[1], "{case}");
                assert_eq!(
                    view.model().sections()[2].resolution,
                    Resolution::Left,
                    "{case}"
                );
                assert_eq!(save_and_read(&mut view, &dir), TWO[1].as_bytes(), "{case}");
            }
        }
    }
}

/// A character of the lent text changes, an own line of the lender changes
/// in both inputs and a reload follows. The holder take alone leaves the
/// lender an edited section; both take orders give the left lines.
#[test]
fn a_two_way_changed_reload_after_an_edit_in_lent_text_gives_one_result() {
    let taken = "a\nR\nab\ncd\nG\nh\n";
    for lender_first in [true, false] {
        let (mut view, dir) = joined(1);
        at(&mut view, 1, 3);
        view.output_pane.backspace();
        absorb(&mut view);
        assert_eq!(view.output_text(), "a\nLa\ncd\ng\nh\n");
        rewrite(&dir, "\ng\n", "\nG\n");
        reload(&mut view);
        assert_eq!(view.output_text(), "a\nLa\ncd\nG\nh\n");
        if lender_first {
            take(&mut view, 2, Command::TakeLeft);
            take(&mut view, 1, Command::TakeRight);
        } else {
            take(&mut view, 1, Command::TakeRight);
            assert_eq!(view.output_text(), "a\nR\na\ncd\nG\nh\n");
            assert_eq!(view.model().sections()[2].resolution, Resolution::Edited);
            take(&mut view, 2, Command::TakeLeft);
        }
        assert_eq!(view.output_text(), taken, "lender first {lender_first}");
        assert_eq!(view.model().sections()[2].resolution, Resolution::Left);
        assert_eq!(save_and_read(&mut view, &dir), taken.as_bytes());
    }
}

/// Both inputs replace the lines the holder shows. The joined copy leaves
/// the holder, and the holder take gives the lender nothing back.
#[test]
fn a_two_way_reload_that_replaced_the_lent_lines_takes_the_joined_copy_out() {
    let (mut view, dir) = joined(2);
    assert_eq!(view.output_text(), "a\nLabcd\ng\nh\n");
    rewrite(&dir, "\nab\ncd\n", "\nx\ny\n");
    reload(&mut view);
    assert_eq!(view.output_text(), "a\nL\nx\ny\ng\nh\n");
    take(&mut view, 1, Command::TakeRight);
    let taken = "a\nR\nx\ny\ng\nh\n";
    assert_eq!(view.output_text(), taken);
    assert_eq!(view.model().sections()[2].resolution, Resolution::Left);
    assert_eq!(save_and_read(&mut view, &dir), taken.as_bytes());
}

/// A line take on a row whose line the holder shows gives the joined lines
/// back first: each line shows once.
#[test]
fn a_two_way_line_take_on_a_lent_row_gives_the_joined_lines_back_first() {
    for joins in [1, 2] {
        for left_line in [2, 3] {
            let (mut view, _dir) = joined(joins);
            view.output_pane.clear_selection();
            view.focus = crate::Pane::Output;
            let row = view
                .model()
                .rows()
                .iter()
                .position(|row| row.line(crate::model::Pane::Left, None) == Some(left_line))
                .unwrap();
            view.row = row;
            view.run(Command::TakeRightLine);
            assert_output_lines_match_pane(&view);
            assert_eq!(view.model().lent_record_problems(), Vec::<String>::new());
            assert_eq!(
                view.output_text(),
                TWO[0],
                "joins {joins} left line {left_line}"
            );
        }
    }
}
