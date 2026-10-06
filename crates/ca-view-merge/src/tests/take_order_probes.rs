use super::*;
use ca_ui::editor::{Caret, Motion};

// s0 a, s1 conflict b, s2 c d g h i, s3 left change X, s4 f.
const F1: [&str; 3] = [
    "a\nL\nc\nd\ng\nh\ni\nX\nf\n",
    "a\nb\nc\nd\ng\nh\ni\ne\nf\n",
    "a\nR\nc\nd\ng\nh\ni\ne\nf\n",
];
const F1_TAKEN: &str = "a\nL\nc\nd\ng\nh\ni\nX\nf\n";

// s0 a, s1 conflict b, s2 c d g; the center ends its lines with CRLF.
const CRLF_CENTER: [&str; 3] = [
    "a\nL\nc\nd\ng\n",
    "a\r\nb\r\nc\r\nd\r\ng\r\n",
    "a\nR\nc\nd\ng\n",
];
const CRLF_CENTER_TAKEN: &str = "a\r\nL\nc\r\nd\r\ng\r\n";

fn same(view: &MergeView) -> bool {
    let pane = view.output_pane.buffer().text();
    let lines = ca_diff::split_lines(&pane);
    view.output_text() == pane
        && view
            .model()
            .output_lines()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            == lines
}

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

fn resolution(view: &MergeView, section: usize) -> String {
    format!("{:?}", view.model().sections()[section].resolution)
}

/// Both orders of an explicit Take Center of the lender s2 and Take Left of
/// the holder s1, after `edit`: (lender first, holder first), each as the
/// output text and the lender's resolution.
fn both_orders(inputs: [&str; 3], edit: &dyn Fn(&mut MergeView)) -> [(String, String); 2] {
    let mut out = Vec::new();
    for lender_first in [true, false] {
        let (mut view, _dir) = joined(inputs, 2);
        edit(&mut view);
        if lender_first {
            take_at(&mut view, 2, Command::TakeCenter);
            take_at(&mut view, 1, Command::TakeLeft);
        } else {
            take_at(&mut view, 1, Command::TakeLeft);
            take_at(&mut view, 2, Command::TakeCenter);
        }
        out.push((view.output_text(), resolution(&view, 2)));
    }
    [out[0].clone(), out[1].clone()]
}

/// An explicit Take of a section restores it from the selected input, in
/// either order with the holder's take, also after the user typed into the
/// text that section lent to the holder.
#[test]
fn an_explicit_lender_take_after_typing_in_its_lent_text_gives_one_text_in_both_orders() {
    let [lender_first, holder_first] = both_orders(F1, &|view| {
        view.output_pane.place(Caret::new(1, 2), false);
        view.output_pane.type_character('Q');
        absorb(view);
        assert_eq!(view.output_text(), "a\nbcQd\ng\nh\ni\nX\nf\n");
    });
    assert_eq!(holder_first.0, F1_TAKEN, "holder, then lender");
    assert_eq!(lender_first.0, F1_TAKEN, "lender, then holder");
}

#[test]
fn an_explicit_lender_take_after_deleting_in_its_lent_text_gives_one_text_in_both_orders() {
    let [lender_first, holder_first] = both_orders(F1, &|view| {
        view.output_pane.place(Caret::new(1, 3), false);
        view.output_pane.backspace();
        absorb(view);
        assert_eq!(view.output_text(), "a\nbc\ng\nh\ni\nX\nf\n");
    });
    assert_eq!(holder_first.0, F1_TAKEN, "holder, then lender");
    assert_eq!(lender_first.0, F1_TAKEN, "lender, then holder");
}

/// A line break typed inside lent text is a separator change only, so the
/// explicit lender take still restores the selected input's bytes.
#[test]
fn an_explicit_lender_take_after_enter_inside_its_lent_text_restores_the_selected_input() {
    let [lender_first, holder_first] = both_orders(CRLF_CENTER, &|view| {
        view.output_pane.place(Caret::new(1, 2), false);
        let ending = view.terminator();
        view.output_pane.enter(ending);
        absorb(view);
    });
    assert_eq!(holder_first.0, CRLF_CENTER_TAKEN, "holder, then lender");
    assert_eq!(holder_first.1, "Center");
    assert_eq!(lender_first, holder_first, "lender, then holder");
}

/// What one take order leaves: the output text, the lender's resolution
/// and the saved bytes.
#[derive(Debug, PartialEq)]
struct Outcome {
    text: String,
    lender: Resolution,
    saved: Vec<u8>,
}

/// One step of a probe after the join: the edit, the explicit Take Center
/// of the lender s2, or the Take Left of the holder s1.
#[derive(Clone, Copy, Debug)]
enum Step {
    Edit,
    Lender,
    Holder,
}

/// Join line 1 `joins` times, then run `steps`. Every step keeps the model
/// on the pane and the lent records on their text; Undo to the bottom gives
/// the loaded text and Redo to the top the last text.
fn in_order(
    inputs: [&str; 3],
    joins: usize,
    edit: &dyn Fn(&mut MergeView),
    steps: [Step; 3],
) -> Outcome {
    let (mut view, dir) = open(inputs[0], Some(inputs[1]), inputs[2]);
    run_until_ready(&mut view);
    let loaded = view.output_text();
    for _ in 0..joins {
        view.output_pane.place(Caret::new(1, 0), false);
        view.output_pane.move_caret(Motion::LineEnd, false);
        view.output_pane.delete();
        absorb(&mut view);
    }
    for step in steps {
        match step {
            Step::Edit => edit(&mut view),
            Step::Lender => take_at(&mut view, 2, Command::TakeCenter),
            Step::Holder => take_at(&mut view, 1, Command::TakeLeft),
        }
        assert_eq!(
            view.model().lent_record_problems(),
            Vec::<String>::new(),
            "{step:?}"
        );
    }
    let text = view.output_text();
    let lender = view.model().sections()[2].resolution;
    assert_eq!(view.model().totals().conflicts_remaining, 0);
    let saved = save_and_read(&mut view, &dir);
    assert_eq!(saved, view.output_pane.buffer().text().as_bytes());
    let mut count = 0;
    while view.output_pane.buffer().can_undo() && count < 100 {
        view.run(Command::Undo);
        assert!(same(&view), "undo step {count}");
        count += 1;
    }
    assert_eq!(view.output_text(), loaded, "undo to the bottom");
    while view.output_pane.buffer().redo_group_id().is_some() && count < 200 {
        view.run(Command::Redo);
        assert!(same(&view), "redo step {count}");
        count += 1;
    }
    assert_eq!(view.output_text(), text, "redo to the top");
    Outcome {
        text,
        lender,
        saved,
    }
}

/// [`in_order`] with the edit first and the two takes in the order
/// `lender_first` names.
fn one_order(
    inputs: [&str; 3],
    joins: usize,
    edit: &dyn Fn(&mut MergeView),
    lender_first: bool,
) -> Outcome {
    let steps = if lender_first {
        [Step::Edit, Step::Lender, Step::Holder]
    } else {
        [Step::Edit, Step::Holder, Step::Lender]
    };
    in_order(inputs, joins, edit, steps)
}

fn assert_restored(outcome: &Outcome, expected: &str) {
    assert_eq!(outcome.text, expected);
    assert_eq!(outcome.lender, Resolution::Center);
    assert_eq!(String::from_utf8_lossy(&outcome.saved), expected);
}

fn type_inside_lent_text(view: &mut MergeView) {
    view.output_pane.place(Caret::new(1, 2), false);
    view.output_pane.type_character('Q');
    absorb(view);
    assert_eq!(view.output_text(), "a\nbcQd\ng\nh\ni\nX\nf\n");
}

#[test]
fn typing_inside_lent_text_then_taking_the_lender_before_the_holder_restores_the_selected_input() {
    let outcome = one_order(F1, 2, &type_inside_lent_text, true);
    assert_restored(&outcome, F1_TAKEN);
}

#[test]
fn typing_inside_lent_text_then_taking_the_holder_before_the_lender_restores_the_selected_input() {
    let outcome = one_order(F1, 2, &type_inside_lent_text, false);
    assert_restored(&outcome, F1_TAKEN);
}

fn delete_inside_lent_text(view: &mut MergeView) {
    view.output_pane.place(Caret::new(1, 3), false);
    view.output_pane.backspace();
    absorb(view);
    assert_eq!(view.output_text(), "a\nbc\ng\nh\ni\nX\nf\n");
}

#[test]
fn deleting_inside_lent_text_then_taking_the_lender_before_the_holder_restores_the_selected_input()
{
    let outcome = one_order(F1, 2, &delete_inside_lent_text, true);
    assert_restored(&outcome, F1_TAKEN);
}

#[test]
fn deleting_inside_lent_text_then_taking_the_holder_before_the_lender_restores_the_selected_input()
{
    let outcome = one_order(F1, 2, &delete_inside_lent_text, false);
    assert_restored(&outcome, F1_TAKEN);
}

fn enter_inside_lent_text(view: &mut MergeView) {
    view.output_pane.place(Caret::new(1, 2), false);
    let ending = view.terminator();
    view.output_pane.enter(ending);
    absorb(view);
    assert_eq!(
        view.output_text().replace('\r', ""),
        "a\nbc\nd\ng\n",
        "{:?}",
        view.output_text()
    );
}

#[test]
fn enter_inside_lent_text_then_taking_the_lender_before_the_holder_restores_the_selected_bytes() {
    let outcome = one_order(CRLF_CENTER, 2, &enter_inside_lent_text, true);
    assert_restored(&outcome, CRLF_CENTER_TAKEN);
}

#[test]
fn enter_inside_lent_text_then_taking_the_holder_before_the_lender_restores_the_selected_bytes() {
    let outcome = one_order(CRLF_CENTER, 2, &enter_inside_lent_text, false);
    assert_restored(&outcome, CRLF_CENTER_TAKEN);
}

#[test]
fn enter_inside_lf_lent_text_then_taking_the_lender_first_resolves_it_to_the_selected_input() {
    let enter = |view: &mut MergeView| {
        view.output_pane.place(Caret::new(1, 2), false);
        view.output_pane.enter("\n");
        absorb(view);
        assert_eq!(view.output_text(), "a\nbc\nd\ng\nh\ni\nX\nf\n");
    };
    assert_restored(&one_order(F1, 2, &enter, true), F1_TAKEN);
    assert_restored(&one_order(F1, 2, &enter, false), F1_TAKEN);
}

fn enter_and_type_inside_lent_text(view: &mut MergeView) {
    view.output_pane.place(Caret::new(1, 2), false);
    let ending = view.terminator();
    view.output_pane.enter(ending);
    absorb(view);
    view.output_pane.place(Caret::new(2, 0), false);
    view.output_pane.type_character('Q');
    absorb(view);
    assert_eq!(
        view.output_text().replace('\r', ""),
        "a\nbc\nQd\ng\n",
        "{:?}",
        view.output_text()
    );
}

#[test]
fn enter_and_typing_inside_lent_text_then_taking_the_lender_first_restores_the_selected_bytes() {
    let outcome = one_order(CRLF_CENTER, 2, &enter_and_type_inside_lent_text, true);
    assert_restored(&outcome, CRLF_CENTER_TAKEN);
}

#[test]
fn enter_and_typing_inside_lent_text_then_taking_the_holder_first_restores_the_selected_bytes() {
    let outcome = one_order(CRLF_CENTER, 2, &enter_and_type_inside_lent_text, false);
    assert_restored(&outcome, CRLF_CENTER_TAKEN);
}

/// Four joins give `a\nbcdgh\ni...`; removing `dg` leaves `bch`.
fn remove_middle_of_lent_text(view: &mut MergeView) {
    assert_eq!(view.output_text(), "a\nbcdgh\ni\nX\nf\n");
    view.output_pane.place(Caret::new(1, 2), false);
    view.output_pane.place(Caret::new(1, 4), true);
    view.output_pane.backspace();
    absorb(view);
    assert_eq!(view.output_text(), "a\nbch\ni\nX\nf\n");
}

#[test]
fn removing_the_middle_of_lent_text_then_taking_the_lender_first_restores_the_selected_input() {
    let outcome = one_order(F1, 4, &remove_middle_of_lent_text, true);
    assert_restored(&outcome, F1_TAKEN);
}

#[test]
fn removing_the_middle_of_lent_text_then_taking_the_holder_first_restores_the_selected_input() {
    let outcome = one_order(F1, 4, &remove_middle_of_lent_text, false);
    assert_restored(&outcome, F1_TAKEN);
}

fn type_in_the_holder_text_of_the_joined_line(view: &mut MergeView) {
    view.output_pane.place(Caret::new(1, 1), false);
    view.output_pane.type_character('W');
    absorb(view);
    assert_eq!(view.output_text(), "a\nbWcd\ng\nh\ni\nX\nf\n");
}

#[test]
fn typing_in_the_holder_part_of_a_joined_line_then_taking_the_lender_first_restores_both() {
    let outcome = one_order(F1, 2, &type_in_the_holder_text_of_the_joined_line, true);
    assert_restored(&outcome, F1_TAKEN);
}

#[test]
fn typing_in_the_holder_part_of_a_joined_line_then_taking_the_holder_first_restores_both() {
    let outcome = one_order(F1, 2, &type_in_the_holder_text_of_the_joined_line, false);
    assert_restored(&outcome, F1_TAKEN);
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Op {
    TakeLender,
    TakeLenderLeft,
    TakeHolder,
    TypeInLent,
    TypeInHolder,
    Reload,
    ReloadLenderChanged,
    ReloadHolderChanged,
    Swap,
    UndoRedo,
}

fn find(view: &MergeView, needle: &str) -> Option<(u32, u32)> {
    let text = view.output_pane.buffer().text();
    for (n, line) in ca_diff::split_lines(&text).iter().enumerate() {
        if let Some(at) = line.find(needle) {
            return Some((
                u32::try_from(n).unwrap(),
                u32::try_from(line[..at].chars().count()).unwrap(),
            ));
        }
    }
    None
}

/// Run `ops` after the join. After every step the model equals the pane;
/// Save writes the pane or its markers; Undo to the bottom gives the loaded
/// text when no reload or swap ran; Redo to the top gives the last text.
/// Returns the last text and whether `Q` was typed.
fn run_ops(ops: &[Op]) -> (String, bool) {
    let (mut view, dir) = open(F1[0], Some(F1[1]), F1[2]);
    run_until_ready(&mut view);
    let loaded = view.output_text();
    for _ in 0..2 {
        view.output_pane.place(Caret::new(1, 0), false);
        view.output_pane.move_caret(Motion::LineEnd, false);
        view.output_pane.delete();
        absorb(&mut view);
    }
    let mut history = true;
    let mut typed = false;
    for &op in ops {
        match op {
            Op::TakeLender => take_at(&mut view, 2, Command::TakeCenter),
            Op::TakeLenderLeft => take_at(&mut view, 2, Command::TakeLeft),
            Op::TakeHolder => take_at(&mut view, 1, Command::TakeLeft),
            Op::TypeInLent => {
                if let Some((line, column)) = find(&view, "cd") {
                    view.output_pane.place(Caret::new(line, column + 1), false);
                    view.output_pane.type_character('Q');
                    absorb(&mut view);
                    typed = true;
                }
            }
            Op::TypeInHolder => {
                view.output_pane.place(Caret::new(1, 0), false);
                view.output_pane.type_character('W');
                absorb(&mut view);
            }
            Op::Reload | Op::ReloadLenderChanged | Op::ReloadHolderChanged => {
                if op == Op::ReloadLenderChanged {
                    for (name, text) in [
                        ("left.txt", F1[0]),
                        ("center.txt", F1[1]),
                        ("right.txt", F1[2]),
                    ] {
                        std::fs::write(dir.path().join(name), text.replace("\nh\n", "\nH\n"))
                            .unwrap();
                    }
                }
                if op == Op::ReloadHolderChanged {
                    std::fs::write(
                        dir.path().join("center.txt"),
                        F1[1].replace("\nb\n", "\nB\n"),
                    )
                    .unwrap();
                }
                view.run(Command::Reload);
                run_until_ready(&mut view);
                history = false;
            }
            Op::Swap => {
                view.run(Command::SwapSides);
                run_until_ready(&mut view);
                history = false;
            }
            Op::UndoRedo => {
                view.run(Command::Undo);
                assert!(same(&view), "{ops:?}: undo");
                view.run(Command::Redo);
            }
        }
        assert!(same(&view), "{ops:?}: after {op:?}");
    }
    let last = view.output_text();
    let conflicts = view.model().totals().conflicts_remaining;
    let expected = if conflicts > 0 {
        expected_markers(&view).0
    } else {
        view.output_pane.buffer().text()
    };
    assert_eq!(
        String::from_utf8(save_and_read(&mut view, &dir)).unwrap(),
        expected,
        "{ops:?}: save"
    );
    let mut steps = 0;
    while view.output_pane.buffer().undo_group_id().is_some() && steps < 100 {
        view.run(Command::Undo);
        assert!(same(&view), "{ops:?}: undo step {steps}");
        steps += 1;
    }
    if history {
        assert_eq!(view.output_text(), loaded, "{ops:?}: undo to the bottom");
    }
    while view.output_pane.buffer().redo_group_id().is_some() && steps < 200 {
        view.run(Command::Redo);
        assert!(same(&view), "{ops:?}: redo step {steps}");
        steps += 1;
    }
    assert_eq!(view.output_text(), last, "{ops:?}: redo to the top");
    (last, typed)
}

/// Explicit lender takes, holder takes, typing, reloads (plain, lender
/// changed on disk, holder changed on disk), Swap Sides and Undo/Redo in
/// every position: model on the pane, save, history, no line twice, and a
/// typed character stays unless an explicit lender take, or a reload of
/// the lender changed on disk, follows it.
#[test]
fn lender_and_holder_takes_with_reloads_swaps_and_history_keep_every_line_once() {
    use Op::*;
    let base: Vec<Vec<Op>> = vec![
        vec![TakeLender, TakeHolder],
        vec![TakeHolder, TakeLender],
        vec![TakeHolder],
        vec![TakeLender],
        vec![TypeInLent, TakeHolder],
        vec![TakeLender, TypeInLent, TakeHolder],
        vec![TypeInLent, TakeHolder, TakeLender],
        vec![TakeLender, TypeInHolder, TakeHolder],
        vec![TakeLenderLeft, TakeHolder],
        vec![TakeLender, TakeLenderLeft, TakeHolder],
        vec![TakeLender, UndoRedo, TakeHolder, UndoRedo],
    ];
    let inserts = [
        None,
        Some(Reload),
        Some(ReloadLenderChanged),
        Some(ReloadHolderChanged),
        Some(Swap),
    ];
    let mut runs = 0;
    for ops in &base {
        for insert in inserts {
            for at in 0..=ops.len() {
                let mut seq = ops.clone();
                match insert {
                    Some(op) => seq.insert(at, op),
                    None if at > 0 => continue,
                    None => {}
                }
                let (last, typed) = run_ops(&seq);
                runs += 1;
                for line in ["c", "d", "g", "i", "X", "f"] {
                    assert_eq!(last.matches(line).count(), 1, "{seq:?}: {line} in {last:?}");
                }
                assert_eq!(
                    last.matches('h').count() + last.matches('H').count(),
                    1,
                    "{seq:?}: {last:?}"
                );
                let typed_at = seq.iter().position(|op| *op == TypeInLent);
                let retaken = typed_at.is_some_and(|at| {
                    seq[at..]
                        .iter()
                        .any(|op| matches!(op, TakeLender | TakeLenderLeft | ReloadLenderChanged))
                });
                if typed && !retaken {
                    assert!(last.contains('Q'), "{seq:?}: typed Q lost: {last:?}");
                }
            }
        }
    }
    assert!(runs > 100, "{runs}");
}

/// Every line ending, an unterminated last line, both take orders.
#[test]
fn lender_and_holder_takes_in_every_line_ending_restore_the_selected_input() {
    let cases: [[&str; 3]; 5] = [
        ["a\nL\nc\nd", "a\nb\nc\nd", "a\nR\nc\nd"],
        [
            "a\r\nL\r\nc\r\nd\r\ng\r\n",
            "a\r\nb\r\nc\r\nd\r\ng\r\n",
            "a\r\nR\r\nc\r\nd\r\ng\r\n",
        ],
        ["a\rL\rc\rd\rg\r", "a\rb\rc\rd\rg\r", "a\rR\rc\rd\rg\r"],
        ["a\nL\r\nc\rd\ng", "a\nb\r\nc\rd\ng", "a\nR\r\nc\rd\ng"],
        CRLF_CENTER,
    ];
    for inputs in cases {
        let [lender_first, holder_first] = both_orders(inputs, &|_| {});
        assert_eq!(lender_first, holder_first, "{inputs:?}");
        assert_eq!(holder_first.1, "Center", "{inputs:?}");
        let left = ca_diff::split_lines(inputs[0]);
        let center = ca_diff::split_lines(inputs[1]);
        let mut expected = center[0].to_owned();
        expected.push_str(left[1]);
        for line in &center[2..] {
            expected.push_str(line);
        }
        assert_eq!(holder_first.0, expected, "{inputs:?}");
    }
}

/// Join, then change line `c` of s2 to `x` in all three inputs and reload:
/// s2 keeps its automatic resolution, its text no longer starts with the
/// characters s1 holds, and it keeps the record of them.
fn joined_then_lender_changed_on_disk() -> (MergeView, tempfile::TempDir) {
    let (mut view, dir) = joined(F1, 2);
    for (name, text) in [
        ("left.txt", F1[0]),
        ("center.txt", F1[1]),
        ("right.txt", F1[2]),
    ] {
        std::fs::write(dir.path().join(name), text.replace("\nc\n", "\nx\n")).unwrap();
    }
    view.run(Command::Reload);
    run_until_ready(&mut view);
    assert_output_lines_match_pane(&view);
    (view, dir)
}

#[test]
fn a_take_of_a_lender_that_a_reload_left_on_its_side_restores_it_in_both_orders() {
    let mut texts = Vec::new();
    for lender_first in [true, false] {
        let (mut view, dir) = joined_then_lender_changed_on_disk();
        assert!(!view.model().sections()[2].lent_records().is_empty());
        assert_eq!(resolution(&view, 2), "Center");
        let order = if lender_first {
            [(2, Command::TakeCenter), (1, Command::TakeLeft)]
        } else {
            [(1, Command::TakeLeft), (2, Command::TakeCenter)]
        };
        for (section, command) in order {
            take_at(&mut view, section, command);
            assert_eq!(view.model().lent_record_problems(), Vec::<String>::new());
        }
        let text = view.output_text();
        assert_eq!(
            resolution(&view, 2),
            "Center",
            "lender first {lender_first}"
        );
        assert_eq!(
            save_and_read(&mut view, &dir),
            text.as_bytes(),
            "lender first {lender_first}"
        );
        texts.push(text);
    }
    assert_eq!(texts[0], texts[1], "lender first, holder first");
    assert_eq!(texts[1], "a\nL\nx\nd\ng\nh\ni\nX\nf\n");
}
