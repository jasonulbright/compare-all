use super::*;
use ca_ui::editor::{Caret, Motion};

// s0 a, s1 conflict b, s2 c d g h i, s3 left change X, s4 f.
const F1: [&str; 3] = [
    "a\nL\nc\nd\ng\nh\ni\nX\nf\n",
    "a\nb\nc\nd\ng\nh\ni\ne\nf\n",
    "a\nR\nc\nd\ng\nh\ni\ne\nf\n",
];
const F1_CRLF: [&str; 3] = [
    "a\r\nL\r\nc\r\nd\r\ng\r\nh\r\ni\r\nX\r\nf\r\n",
    "a\r\nb\r\nc\r\nd\r\ng\r\nh\r\ni\r\ne\r\nf\r\n",
    "a\r\nR\r\nc\r\nd\r\ng\r\nh\r\ni\r\ne\r\nf\r\n",
];
// The center ends its lines with CRLF, the other inputs with LF.
const CRLF_CENTER: [&str; 3] = [
    "a\nL\nc\nd\ng\nh\ni\nX\nf\n",
    "a\r\nb\r\nc\r\nd\r\ng\r\nh\r\ni\r\ne\r\nf\r\n",
    "a\nR\nc\nd\ng\nh\ni\ne\nf\n",
];

fn absorb(view: &mut MergeView) {
    view.absorb_output_edits();
    assert_output_lines_match_pane(view);
}

fn take_at(view: &mut MergeView, section: usize, command: Command) {
    view.output_pane.clear_selection();
    view.focus = crate::Pane::Output;
    view.current = section;
    view.run(command);
    assert_output_lines_match_pane(view);
}

/// Delete at the end of line 1 `times` times; twice gives `a\nbcd\ng...`,
/// where s1 holds the `cd` of s2.
fn joined(inputs: [&str; 3], times: usize) -> (MergeView, tempfile::TempDir) {
    let (mut view, dir) = open(inputs[0], Some(inputs[1]), inputs[2]);
    run_until_ready(&mut view);
    for _ in 0..times {
        view.output_pane.place(Caret::new(1, 0), false);
        view.output_pane.move_caret(Motion::LineEnd, false);
        view.output_pane.delete();
        absorb(&mut view);
    }
    (view, dir)
}

fn enter_at(view: &mut MergeView, line: u32, column: u32) {
    view.output_pane.clear_selection();
    view.output_pane.place(Caret::new(line, column), false);
    let ending = view.terminator();
    view.output_pane.enter(ending);
    absorb(view);
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

/// The merge text with line 1 taken from `from`: every line from the
/// center but line 7, the left change.
fn center_with_line_one_of(inputs: [&str; 3], from: &str) -> String {
    let center = ca_diff::split_lines(inputs[1]);
    let left = ca_diff::split_lines(inputs[0]);
    let other = ca_diff::split_lines(from);
    let mut text = String::new();
    for (index, line) in center.iter().enumerate() {
        text.push_str(match index {
            1 => other[1],
            7 => left[7],
            _ => line,
        });
    }
    text
}

/// Join line 1 `joins` times, take the lender s2 from the center, and type
/// Enter inside its lent text once per piece after the first, before or
/// after that take. Two joins and one Enter leave `bc` / `d`; three joins
/// and two Enters leave `bc` / `d` / `g`.
fn lent_text_in_pieces(
    inputs: [&str; 3],
    pieces: usize,
    take_first: bool,
) -> (MergeView, tempfile::TempDir) {
    let (mut view, dir) = joined(inputs, pieces);
    if take_first {
        take_at(&mut view, 2, Command::TakeCenter);
    }
    enter_at(&mut view, 1, 2);
    if pieces == 3 {
        enter_at(&mut view, 2, 1);
    }
    let expected = if pieces == 3 {
        "a\nbc\nd\ng\nh\n"
    } else {
        "a\nbc\nd\ng\n"
    };
    assert!(
        view.output_text().replace('\r', "").starts_with(expected),
        "{:?}",
        view.output_text()
    );
    if !take_first {
        take_at(&mut view, 2, Command::TakeCenter);
    }
    assert!(view.model().pending_take(2).is_some());
    (view, dir)
}

/// The holder's input changes on disk, so a reload gives every piece of
/// the lender's text back. The lender takes the selected input once, after
/// its last piece is back: no line shows twice, before or after a take of
/// the holder, and Save writes the pane.
#[test]
fn a_reload_that_returns_every_piece_of_lent_text_restores_the_taken_lender_once() {
    for inputs in [F1, F1_CRLF, CRLF_CENTER] {
        for pieces in [2, 3] {
            for take_first in [true, false] {
                let case = format!("{inputs:?} pieces {pieces} take first {take_first}");
                let (mut view, dir) = lent_text_in_pieces(inputs, pieces, take_first);
                rewrite(&dir, &["center.txt"], "\nb", "\nB");
                reload(&mut view);
                let changed = inputs[1].replace("\nb", "\nB");
                let reloaded = center_with_line_one_of(inputs, &changed);
                assert_eq!(view.output_text(), reloaded, "{case}");
                assert_eq!(
                    view.model().sections()[2].resolution,
                    Resolution::Center,
                    "{case}"
                );
                take_at(&mut view, 1, Command::TakeLeft);
                let taken = center_with_line_one_of(inputs, inputs[0]);
                assert_eq!(view.output_text(), taken, "{case}");
                assert_eq!(save_and_read(&mut view, &dir), taken.as_bytes(), "{case}");
            }
        }
    }
}

/// A plain reload and Swap Sides carry the holder and every piece; the
/// later take of the holder restores the lender once.
#[test]
fn a_plain_reload_or_swap_then_a_holder_take_restores_the_taken_lender_once() {
    for inputs in [F1, F1_CRLF, CRLF_CENTER] {
        for pieces in [2, 3] {
            for swap in [false, true] {
                let case = format!("{inputs:?} pieces {pieces} swap {swap}");
                let (mut view, dir) = lent_text_in_pieces(inputs, pieces, true);
                let before = view.output_text();
                if swap {
                    view.run(Command::SwapSides);
                    run_until_ready(&mut view);
                    assert_output_lines_match_pane(&view);
                } else {
                    reload(&mut view);
                }
                assert_eq!(view.output_text(), before, "{case}");
                let command = if swap {
                    Command::TakeRight
                } else {
                    Command::TakeLeft
                };
                take_at(&mut view, 1, command);
                let taken = center_with_line_one_of(inputs, inputs[0]);
                assert_eq!(view.output_text(), taken, "{case}");
                assert_eq!(
                    view.model().sections()[2].resolution,
                    Resolution::Center,
                    "{case}"
                );
                assert_eq!(save_and_read(&mut view, &dir), taken.as_bytes(), "{case}");
            }
        }
    }
}

const C_TO_X: &[(&str, &str)] = &[("\nc", "\nx")];
const C_D_TO_X_Y: &[(&str, &str)] = &[("\nc", "\nx"), ("\nd", "\ny")];

/// Join line 1 twice, so s1 holds the `cd` of s2, then make each change of
/// `changes` in all three inputs and reload.
fn joined_then_reloaded(
    inputs: [&str; 3],
    changes: &[(&str, &str)],
) -> (MergeView, tempfile::TempDir) {
    let (mut view, dir) = joined(inputs, 2);
    for (from, to) in changes {
        rewrite(&dir, &["left.txt", "center.txt", "right.txt"], from, to);
    }
    reload(&mut view);
    (view, dir)
}

/// `c` of s2 changes to `x` on disk. The new text of s2 shows `d`, a line
/// of the text s1 holds, so that lent copy leaves s1: each line shows once.
#[test]
fn a_reload_with_the_lender_changed_on_disk_shows_each_line_once() {
    for inputs in [F1, F1_CRLF, CRLF_CENTER] {
        let (mut view, dir) = joined_then_reloaded(inputs, C_TO_X);
        let mut reloaded = center_with_line_one_of(inputs, inputs[1]).replace("\nc", "\nx");
        if inputs == CRLF_CENTER {
            // The join removed the line break of `b`; the separator the
            // composition adds follows the output's line ending.
            reloaded = reloaded.replacen("b\r\n", "b\n", 1);
        }
        assert_eq!(view.output_text(), reloaded, "{inputs:?}");
        assert_eq!(view.model().sections()[2].resolution, Resolution::Center);
        assert!(view.model().sections()[2].lent_records().is_empty());
        assert_eq!(save_and_read(&mut view, &dir), reloaded.as_bytes());
    }
}

/// One take sequence after a reload, as the output text and the lender's
/// resolution; `None` stands for Take All Non-Conflicting. Save writes the
/// pane.
fn after_takes(
    inputs: [&str; 3],
    changes: &[(&str, &str)],
    takes: &[Option<(usize, Command)>],
) -> (String, Resolution) {
    let (mut view, dir) = joined_then_reloaded(inputs, changes);
    for take in takes {
        if let Some((section, command)) = take {
            take_at(&mut view, *section, *command);
        } else {
            view.run(Command::TakeAllNonConflicting);
            assert_output_lines_match_pane(&view);
        }
        assert_eq!(view.model().lent_record_problems(), Vec::<String>::new());
    }
    let text = view.output_text();
    assert_eq!(save_and_read(&mut view, &dir), text.as_bytes());
    (text, view.model().sections()[2].resolution)
}

/// After the lender changed on disk, the lender take, Take All
/// Non-Conflicting and the holder take give one result in every order.
#[test]
fn takes_after_a_reload_with_the_lender_changed_give_one_result_in_every_order() {
    let lender = Some((2, Command::TakeCenter));
    let holder = Some((1, Command::TakeLeft));
    let orders: [&[Option<(usize, Command)>]; 4] = [
        &[lender, holder],
        &[holder, lender],
        &[None, holder],
        &[holder, None],
    ];
    for inputs in [F1, F1_CRLF, CRLF_CENTER] {
        let results: Vec<(String, Resolution)> = orders
            .iter()
            .map(|takes| after_takes(inputs, C_TO_X, takes))
            .collect();
        let taken = center_with_line_one_of(inputs, inputs[0]).replace("\nc", "\nx");
        for (order, result) in orders.iter().zip(&results) {
            assert_eq!(result.0, taken, "{order:?} {inputs:?}");
            assert_eq!(result.1, Resolution::Center, "{order:?} {inputs:?}");
        }
    }
}

/// `c` and `d` of s2 change to `x` and `y` on disk. The new text of s2
/// shows no line of the joined `cd`, so s1 keeps it and s2 still lends it on
/// its automatic side. Take All Non-Conflicting takes s2 like an explicit
/// take: one result in both orders with the holder take.
#[test]
fn take_all_takes_a_lender_left_on_its_side_by_a_reload_like_an_explicit_take() {
    let lender = Some((2, Command::TakeCenter));
    let holder = Some((1, Command::TakeLeft));
    let orders: [&[Option<(usize, Command)>]; 4] = [
        &[None, holder],
        &[holder, None],
        &[lender, holder],
        &[holder, lender],
    ];
    for inputs in [F1, F1_CRLF, CRLF_CENTER] {
        let (view, _dir) = joined_then_reloaded(inputs, C_D_TO_X_Y);
        assert!(!view.model().sections()[2].lent_records().is_empty());
        assert_eq!(view.model().sections()[2].resolution, Resolution::Center);
        let taken = center_with_line_one_of(inputs, inputs[0])
            .replace("\nc", "\nx")
            .replace("\nd", "\ny");
        for takes in orders {
            let result = after_takes(inputs, C_D_TO_X_Y, takes);
            assert_eq!(result.0, taken, "{takes:?} {inputs:?}");
            assert_eq!(result.1, Resolution::Center, "{takes:?} {inputs:?}");
        }
    }
}
