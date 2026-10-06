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
