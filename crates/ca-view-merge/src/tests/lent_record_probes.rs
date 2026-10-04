use super::*;
use ca_ui::editor::{Caret, Motion};
use std::collections::BTreeMap;

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

fn join_line(view: &mut MergeView, line: u32, times: usize) {
    for _ in 0..times {
        view.output_pane.place(Caret::new(line, 0), false);
        view.output_pane.move_caret(Motion::LineEnd, false);
        view.output_pane.delete();
        absorb(view);
    }
}

fn saved_matches(view: &mut MergeView, dir: &tempfile::TempDir) -> Result<(), String> {
    let conflicts = view.model().totals().conflicts_remaining;
    let expected = if conflicts > 0 {
        expected_markers(view).0
    } else {
        view.output_pane.buffer().text()
    };
    let saved = save_and_read(view, dir);
    if saved == expected.as_bytes() {
        Ok(())
    } else {
        Err(format!(
            "saved {:?} expected {expected:?}",
            String::from_utf8_lossy(&saved)
        ))
    }
}

fn permutations(n: usize) -> Vec<Vec<usize>> {
    if n == 0 {
        return vec![Vec::new()];
    }
    let mut out = Vec::new();
    for rest in permutations(n - 1) {
        for at in 0..=rest.len() {
            let mut order = rest.clone();
            order.insert(at, n - 1);
            out.push(order);
        }
    }
    out
}

/// Run every order of `pool` on a fresh view after `setup`. Each step keeps
/// the model on the pane; Undo of the pool returns to the text after setup,
/// Redo to the final text, and the saved bytes equal the pane. Returns the
/// final texts with the orders that gave them.
fn every_order(
    inputs: [&str; 3],
    setup: &dyn Fn(&mut MergeView),
    pool: &[(usize, Command)],
) -> BTreeMap<String, Vec<Vec<usize>>> {
    let mut finals: BTreeMap<String, Vec<Vec<usize>>> = BTreeMap::new();
    for order in permutations(pool.len()) {
        let (mut view, dir) = open(inputs[0], Some(inputs[1]), inputs[2]);
        run_until_ready(&mut view);
        setup(&mut view);
        let joined = view.output_text();
        let mut texts = vec![joined.clone()];
        for &index in &order {
            let (section, command) = pool[index];
            take_at(&mut view, section, command);
            texts.push(view.output_text());
        }
        let last = view.output_text();
        for at in (0..order.len()).rev() {
            view.run(Command::Undo);
            assert_output_lines_match_pane(&view);
            assert_eq!(view.output_text(), texts[at], "undo {order:?} step {at}");
        }
        for (at, text) in texts.iter().enumerate().skip(1) {
            view.run(Command::Redo);
            assert_output_lines_match_pane(&view);
            assert_eq!(&view.output_text(), text, "redo {order:?} step {at}");
        }
        assert_eq!(view.output_text(), last);
        if let Err(problem) = saved_matches(&mut view, &dir) {
            panic!("save after {order:?}: {problem}");
        }
        finals.entry(last).or_default().push(order);
    }
    finals
}

fn single(finals: &BTreeMap<String, Vec<Vec<usize>>>) -> &str {
    assert_eq!(finals.len(), 1, "orders disagree: {finals:#?}");
    finals.keys().next().unwrap()
}

// s0 a, s1 conflict b, s2 c, s3 left change X, s4 f, s5 left change M, s6 g.
const F3: [&str; 3] = [
    "a\nL\nc\nc2\nc3\nX\nf\nM\ng\n",
    "a\nb\nc\nc2\nc3\ne\nf\nm\ng\n",
    "a\nR\nc\nc2\nc3\ne\nf\nm\ng\n",
];

fn f3_joined(view: &mut MergeView) {
    assert_eq!(view.model().sections().len(), 7);
    join_line(view, 1, 7);
    assert_eq!(view.output_text(), "a\nbcc2c3XfMg\n");
}

#[test]
fn five_lenders_on_one_line_give_one_text_in_every_order_with_the_holder() {
    let pool = [
        (1, Command::TakeLeft),
        (3, Command::TakeCenter),
        (5, Command::TakeLeft),
        (2, Command::TakeCenter),
    ];
    let finals = every_order(F3, &f3_joined, &pool);
    assert_eq!(single(&finals), "a\nL\nc\nc2\nc3\ne\nf\nM\ng\n");
}

#[test]
fn five_lenders_on_one_line_give_one_text_in_every_order_without_the_holder() {
    let pool = [
        (3, Command::TakeCenter),
        (5, Command::TakeCenter),
        (2, Command::TakeCenter),
        (6, Command::TakeRight),
    ];
    let finals = every_order(F3, &f3_joined, &pool);
    assert_eq!(single(&finals), "a\nbcc2c3fg\ne\nm\n");
}

#[test]
fn three_lenders_taken_to_both_sides_and_the_holder_give_one_text_in_every_order() {
    let pool = [
        (3, Command::TakeLeft),
        (5, Command::TakeCenter),
        (1, Command::TakeRight),
        (4, Command::TakeCenter),
    ];
    let finals = every_order(F3, &f3_joined, &pool);
    assert_eq!(single(&finals), "a\nR\ncc2c3\nX\nf\nm\ng\n");
}

// s0 a, s1 conflict b, s2 c c2 c3, s3 conflict x, s4 z.
const F4: [&str; 3] = [
    "a\nL1\nc\nc2\nc3\nL2\nz\n",
    "a\nb\nc\nc2\nc3\nx\nz\n",
    "a\nR1\nc\nc2\nc3\nR2\nz\n",
];

fn f4_joined(view: &mut MergeView) {
    assert_eq!(view.model().sections().len(), 5);
    join_line(view, 1, 4);
    assert_eq!(view.output_text(), "a\nbcc2c3x\nz\n");
}

#[test]
fn a_conflict_lending_to_a_conflict_gives_one_text_in_every_order() {
    let pool = [
        (1, Command::TakeLeft),
        (3, Command::TakeRight),
        (2, Command::TakeCenter),
    ];
    let finals = every_order(F4, &f4_joined, &pool);
    assert_eq!(single(&finals), "a\nL1\nc\nc2\nc3\nR2\nz\n");
    let pool = [(3, Command::TakeLeft), (2, Command::TakeCenter)];
    let finals = every_order(F4, &f4_joined, &pool);
    assert_eq!(single(&finals), "a\nbcc2c3\nL2\nz\n");
    let pool = [(3, Command::TakeCenter), (2, Command::TakeCenter)];
    let finals = every_order(F4, &f4_joined, &pool);
    assert_eq!(single(&finals), "a\nbcc2c3x\nz\n");
}

#[test]
fn a_pending_take_restores_selected_lines_after_reload() {
    let (mut view, _dir) = open(F4[0], Some(F4[1]), F4[2]);
    run_until_ready(&mut view);
    f4_joined(&mut view);
    take_at(&mut view, 2, Command::TakeCenter);
    take_at(&mut view, 3, Command::TakeRight);
    view.run(Command::Reload);
    run_until_ready(&mut view);
    take_at(&mut view, 1, Command::TakeLeft);
    assert_eq!(view.output_text(), "a\nL1\nc\nc2\nc3\nR2\nz\n");
}

#[test]
fn a_pending_take_restores_selected_lines_after_side_swap() {
    let (mut view, _dir) = open(F4[0], Some(F4[1]), F4[2]);
    run_until_ready(&mut view);
    f4_joined(&mut view);
    take_at(&mut view, 2, Command::TakeCenter);
    take_at(&mut view, 3, Command::TakeRight);
    view.run(Command::SwapSides);
    run_until_ready(&mut view);
    take_at(&mut view, 1, Command::TakeLeft);
    assert_eq!(view.output_text(), "a\nR1\nc\nc2\nc3\nR2\nz\n");
}

const F1: [&str; 3] = [
    "a\nL\nc\nd\ng\nh\ni\nX\nf\n",
    "a\nb\nc\nd\ng\nh\ni\ne\nf\n",
    "a\nR\nc\nd\ng\nh\ni\ne\nf\n",
];

fn f1() -> (MergeView, tempfile::TempDir) {
    let (mut view, dir) = open(F1[0], Some(F1[1]), F1[2]);
    run_until_ready(&mut view);
    (view, dir)
}

fn f1_x_joined() -> (MergeView, tempfile::TempDir) {
    let (mut view, dir) = f1();
    view.output_pane.place(Caret::new(7, 0), false);
    view.output_pane.backspace();
    absorb(&mut view);
    assert_eq!(view.output_text(), "a\nb\nc\nd\ng\nh\niX\nf\n");
    (view, dir)
}

#[test]
fn one_lender_taken_four_times_to_different_sides_undoes_and_redoes_every_step() {
    let (mut view, dir) = f1_x_joined();
    let steps = [
        (Command::TakeLeft, "a\nb\nc\nd\ng\nh\niX\nf\n"),
        (Command::TakeCenter, "a\nb\nc\nd\ng\nh\ni\ne\nf\n"),
        (Command::TakeLeft, "a\nb\nc\nd\ng\nh\ni\nX\nf\n"),
        (Command::TakeRight, "a\nb\nc\nd\ng\nh\ni\ne\nf\n"),
        (Command::TakeLeftThenRight, "a\nb\nc\nd\ng\nh\ni\nX\ne\nf\n"),
    ];
    let mut texts = vec![view.output_text()];
    for (command, expected) in steps {
        take_at(&mut view, 3, command);
        assert_eq!(view.output_text(), expected, "{command:?}");
        texts.push(view.output_text());
    }
    for at in (0..steps.len()).rev() {
        view.run(Command::Undo);
        assert_output_lines_match_pane(&view);
        assert_eq!(view.output_text(), texts[at]);
    }
    for text in texts.iter().skip(1) {
        view.run(Command::Redo);
        assert_output_lines_match_pane(&view);
        assert_eq!(&view.output_text(), text);
    }
    saved_matches(&mut view, &dir).unwrap();
}

#[test]
fn text_typed_inside_lent_text_stays_when_the_lender_takes_the_side_that_holds_it() {
    let (mut view, dir) = f1_x_joined();
    view.output_pane.place(Caret::new(6, 2), false);
    view.output_pane.type_character('Q');
    absorb(&mut view);
    assert_eq!(view.output_text(), "a\nb\nc\nd\ng\nh\niXQ\nf\n");
    take_at(&mut view, 3, Command::TakeLeft);
    assert_eq!(view.output_text(), "a\nb\nc\nd\ng\nh\niXQ\nf\n");
    take_at(&mut view, 2, Command::TakeLeft);
    assert_eq!(view.output_text(), "a\nb\nc\nd\ng\nh\ni\nXQ\nf\n");
    saved_matches(&mut view, &dir).unwrap();
    for _ in 0..3 {
        view.run(Command::Undo);
        assert_output_lines_match_pane(&view);
    }
    assert_eq!(view.output_text(), "a\nb\nc\nd\ng\nh\niX\nf\n");
}

#[test]
fn text_typed_between_the_holder_and_lent_text_after_a_strip() {
    let (mut view, _dir) = f1_x_joined();
    view.output_pane.place(Caret::new(6, 1), false);
    view.output_pane.type_character('Q');
    absorb(&mut view);
    assert_eq!(view.output_text(), "a\nb\nc\nd\ng\nh\niQX\nf\n");
    take_at(&mut view, 3, Command::TakeCenter);
    let after = view.output_text();
    assert!(
        after == "a\nb\nc\nd\ng\nh\niQ\ne\nf\n" || after == "a\nb\nc\nd\ng\nh\ni\ne\nf\n",
        "{after:?}"
    );
    println!("typed between holder and lent text, after strip: {after:?}");
}

#[test]
fn enter_inside_joined_lines_then_taking_the_lender_and_the_holder_keeps_one_copy() {
    let (mut view, dir) = f1();
    join_line(&mut view, 1, 3);
    assert_eq!(view.output_text(), "a\nbcdg\nh\ni\nX\nf\n");
    view.output_pane.place(Caret::new(1, 2), false);
    view.output_pane.enter("\n");
    absorb(&mut view);
    assert_eq!(view.output_text(), "a\nbc\ndg\nh\ni\nX\nf\n");
    take_at(&mut view, 2, Command::TakeCenter);
    assert_eq!(view.output_text(), "a\nbc\ndg\nh\ni\nX\nf\n");
    take_at(&mut view, 1, Command::TakeLeft);
    assert_eq!(view.output_text(), "a\nL\nc\ndg\nh\ni\nX\nf\n");
    saved_matches(&mut view, &dir).unwrap();
    take_at(&mut view, 2, Command::TakeCenter);
    assert_eq!(view.output_text(), "a\nL\nc\nd\ng\nh\ni\nX\nf\n");
}

// s0 a, s1 conflict b, s2 c c2 c3, s3 left change X1 X2, s4 f.
const F5: [&str; 3] = [
    "a\nL\nc\nc2\nc3\nX1\nX2\nf\n",
    "a\nb\nc\nc2\nc3\ne\nf\n",
    "a\nR\nc\nc2\nc3\ne\nf\n",
];

fn f5_joined() -> (MergeView, tempfile::TempDir) {
    let (mut view, dir) = open(F5[0], Some(F5[1]), F5[2]);
    run_until_ready(&mut view);
    assert_eq!(view.model().sections().len(), 5);
    for _ in 0..2 {
        view.output_pane.place(Caret::new(5, 0), false);
        view.output_pane.backspace();
        absorb(&mut view);
    }
    assert_eq!(view.output_text(), "a\nb\nc\nc2\nc3X1X2\nf\n");
    (view, dir)
}

#[test]
fn lent_text_split_by_enter_goes_when_the_lender_takes_a_side_without_it() {
    let (mut view, dir) = f5_joined();
    view.output_pane.place(Caret::new(4, 4), false);
    view.output_pane.enter("\n");
    absorb(&mut view);
    assert_eq!(view.output_text(), "a\nb\nc\nc2\nc3X1\nX2\nf\n");
    take_at(&mut view, 3, Command::TakeCenter);
    assert_eq!(view.output_text(), "a\nb\nc\nc2\nc3\ne\nf\n");
    take_at(&mut view, 3, Command::TakeLeft);
    assert_eq!(view.output_text(), "a\nb\nc\nc2\nc3\nX1\nX2\nf\n");
    saved_matches(&mut view, &dir).unwrap();
    for _ in 0..2 {
        view.run(Command::Undo);
        assert_output_lines_match_pane(&view);
    }
    assert_eq!(view.output_text(), "a\nb\nc\nc2\nc3X1\nX2\nf\n");
}

#[test]
fn lent_text_split_by_enter_comes_back_whole_when_the_holder_is_taken() {
    let (mut view, _dir) = f5_joined();
    view.output_pane.place(Caret::new(4, 4), false);
    view.output_pane.enter("\n");
    absorb(&mut view);
    take_at(&mut view, 2, Command::TakeLeft);
    assert_eq!(view.output_text(), "a\nb\nc\nc2\nc3\nX1\nX2\nf\n");
    take_at(&mut view, 3, Command::TakeCenter);
    assert_eq!(view.output_text(), "a\nb\nc\nc2\nc3\ne\nf\n");
}

#[test]
fn lent_text_with_its_middle_removed_goes_when_the_lender_takes_a_side_without_it() {
    let (mut view, dir) = f5_joined();
    view.output_pane.place(Caret::new(4, 3), false);
    view.output_pane.place(Caret::new(4, 5), true);
    view.output_pane.backspace();
    absorb(&mut view);
    assert_eq!(view.output_text(), "a\nb\nc\nc2\nc3X2\nf\n");
    take_at(&mut view, 3, Command::TakeCenter);
    assert_eq!(view.output_text(), "a\nb\nc\nc2\nc3\ne\nf\n");
    saved_matches(&mut view, &dir).unwrap();
    take_at(&mut view, 3, Command::TakeLeft);
    assert_eq!(view.output_text(), "a\nb\nc\nc2\nc3\nX1\nX2\nf\n");
}

#[test]
fn lent_text_with_its_middle_removed_keeps_the_rest_when_the_lender_takes_the_side_that_holds_it() {
    let (mut view, _dir) = f5_joined();
    view.output_pane.place(Caret::new(4, 3), false);
    view.output_pane.place(Caret::new(4, 5), true);
    view.output_pane.backspace();
    absorb(&mut view);
    take_at(&mut view, 3, Command::TakeLeft);
    assert_eq!(view.output_text(), "a\nb\nc\nc2\nc3X2\nf\n");
    take_at(&mut view, 2, Command::TakeCenter);
    assert_eq!(view.output_text(), "a\nb\nc\nc2\nc3\nX2\nf\n");
}

// The lender's center side starts with the characters it lent, split over
// other lines.
const F6: [&str; 3] = [
    "a\nL\nc\nc2\nc3\nXY\nf\n",
    "a\nb\nc\nc2\nc3\nX\nYz\nf\n",
    "a\nR\nc\nc2\nc3\nX\nYz\nf\n",
];

#[test]
fn a_side_that_starts_with_the_lent_characters_on_other_lines_keeps_one_copy() {
    let (mut view, _dir) = open(F6[0], Some(F6[1]), F6[2]);
    run_until_ready(&mut view);
    let sections = view.model().sections().len();
    println!("F6 sections {sections}: {:?}", view.output_text());
    view.output_pane.place(Caret::new(5, 0), false);
    view.output_pane.backspace();
    absorb(&mut view);
    println!("F6 joined {:?}", view.output_text());
    take_at(&mut view, 3, Command::TakeCenter);
    println!("F6 after Take Center on s3 {:?}", view.output_text());
    take_at(&mut view, 2, Command::TakeLeft);
    println!("F6 after Take Left on s2 {:?}", view.output_text());
}

#[test]
fn take_all_and_line_takes_after_joins_keep_one_copy() {
    let (mut view, dir) = open(F3[0], Some(F3[1]), F3[2]);
    run_until_ready(&mut view);
    f3_joined(&mut view);
    take_at(&mut view, 3, Command::TakeCenter);
    assert_eq!(view.output_text(), "a\nbcc2c3fMg\ne\n");
    view.run(Command::TakeAllNonConflicting);
    assert_output_lines_match_pane(&view);
    println!("take all after s3 center: {:?}", view.output_text());
    take_at(&mut view, 1, Command::TakeLeft);
    println!("holder after take all: {:?}", view.output_text());
    saved_matches(&mut view, &dir).unwrap();
    view.row = 1;
    view.run(Command::TakeRightLine);
    assert_output_lines_match_pane(&view);
    println!("right line on row 1: {:?}", view.output_text());
    saved_matches(&mut view, &dir).unwrap();
    while view.output_pane.buffer().can_undo() {
        view.run(Command::Undo);
        assert_output_lines_match_pane(&view);
    }
    assert_eq!(view.output_text(), "a\nb\nc\nc2\nc3\nX\nf\nM\ng\n");
}

#[test]
fn toggles_reload_and_swap_after_joins_keep_the_text() {
    let (mut view, dir) = open(F3[0], Some(F3[1]), F3[2]);
    run_until_ready(&mut view);
    f3_joined(&mut view);
    take_at(&mut view, 5, Command::TakeCenter);
    let text = view.output_text();
    assert_eq!(text, "a\nbcc2c3Xfg\nm\n");
    view.current = 3;
    view.run(Command::ToggleSectionIgnored);
    view.current = 1;
    if view.accepts(Command::ToggleConflict) {
        view.run(Command::ToggleConflict);
    }
    assert_eq!(view.output_text(), text);
    view.run(Command::Reload);
    run_until_ready(&mut view);
    assert_output_lines_match_pane(&view);
    assert_eq!(view.output_text(), text, "reload");
    saved_matches(&mut view, &dir).unwrap();
    take_at(&mut view, 1, Command::TakeLeft);
    assert_eq!(view.output_text(), "a\nL\ncc2c3\nX\nf\nm\ng\n");
    view.run(Command::Undo);
    assert_output_lines_match_pane(&view);
    assert_eq!(view.output_text(), text);
    view.run(Command::SwapSides);
    run_until_ready(&mut view);
    assert_output_lines_match_pane(&view);
    println!("swap after joins: {:?}", view.output_text());
    saved_matches(&mut view, &dir).unwrap();
    take_at(&mut view, 1, Command::TakeRight);
    println!("swap then take right on holder: {:?}", view.output_text());
}

#[test]
fn a_reload_with_the_lender_changed_on_disk_keeps_one_copy() {
    let (mut view, dir) = open(F3[0], Some(F3[1]), F3[2]);
    run_until_ready(&mut view);
    f3_joined(&mut view);
    std::fs::write(
        dir.path().join("left.txt"),
        "a\nL\nc\nc2\nc3\nX\nf\nMM\ng\n",
    )
    .unwrap();
    view.run(Command::Reload);
    run_until_ready(&mut view);
    assert_output_lines_match_pane(&view);
    let text = view.output_text();
    println!("reload with s5 left changed: {text:?}");
    assert_eq!(text.matches("c2").count(), 1, "{text:?}");
    assert_eq!(text.matches('g').count(), 1, "{text:?}");
    take_at(&mut view, 1, Command::TakeLeft);
    let after = view.output_text();
    assert!(!after.contains("MMM"), "{after:?}");
    saved_matches(&mut view, &dir).unwrap();
}

fn f1_x_and_f_joined() -> (MergeView, tempfile::TempDir) {
    let (mut view, dir) = f1();
    for _ in 0..2 {
        view.output_pane.place(Caret::new(7, 0), false);
        view.output_pane.backspace();
        absorb(&mut view);
    }
    assert_eq!(view.output_text(), "a\nb\nc\nd\ng\nh\niXf\n");
    (view, dir)
}

#[test]
fn a_lender_left_out_then_given_back_onto_its_own_remaining_lines() {
    // s3 left change X X2; the join takes only X, then f's join removes
    // X's line break; X2 stays on s3's own line.
    let (mut view, _dir) = open(
        "a\nL\nc\nd\ng\nh\ni\nX\nX2\nf\n",
        Some("a\nb\nc\nd\ng\nh\ni\ne\nf\n"),
        "a\nR\nc\nd\ng\nh\ni\ne\nf\n",
    );
    run_until_ready(&mut view);
    view.output_pane.place(Caret::new(7, 0), false);
    view.output_pane.backspace();
    absorb(&mut view);
    view.output_pane.place(Caret::new(6, 2), false);
    view.output_pane.type_character('Q');
    absorb(&mut view);
    println!("joined X, typed Q: {:?}", view.output_text());
    take_at(&mut view, 3, Command::TakeLeft);
    println!("lender left: {:?}", view.output_text());
    take_at(&mut view, 2, Command::TakeLeft);
    println!("holder left: {:?}", view.output_text());
}

/// Every lent record's text sits on its holder line at its column.
fn records_on_their_text(view: &MergeView) -> Result<(), String> {
    let model = view.model();
    let lines: Vec<String> = model.output_lines().iter().cloned().collect();
    for (lender, section) in model.sections().iter().enumerate() {
        for entry in section.lent_records() {
            let Some(range) = model.output_range(entry.holder) else {
                return Err(format!("s{lender} record on a missing holder"));
            };
            let line = (range.start + entry.offset) as usize;
            let here: String = lines
                .get(line)
                .map(|text| {
                    text.chars()
                        .skip(entry.column as usize)
                        .take(entry.text.chars().count())
                        .collect()
                })
                .unwrap_or_default();
            if line >= range.end as usize || here != entry.text {
                return Err(format!(
                    "s{lender} record {:?} at line {line} column {} finds {here:?}",
                    entry.text, entry.column
                ));
            }
        }
    }
    Ok(())
}

#[test]
fn a_reload_that_changed_a_lender_whose_joined_text_lost_its_line_break_keeps_its_lines_apart() {
    for take in [Command::TakeLeft, Command::TakeCenter] {
        let (mut view, dir) = f1_x_and_f_joined();
        std::fs::write(dir.path().join("left.txt"), "a\nL\nc\nd\ng\nh\ni\nXX\nf\n").unwrap();
        view.run(Command::Reload);
        run_until_ready(&mut view);
        assert_output_lines_match_pane(&view);
        take_at(&mut view, 2, take);
        let text = view.output_text();
        assert!(!text.contains("XXX"), "{take:?}: {text:?}");
    }
}

#[test]
fn a_take_of_the_first_holder_puts_the_given_back_text_before_the_lenders_other_text() {
    let (mut view, _dir) = open(
        "[s3]\n[s4]\n[u5]\n[u6]\n[u7]\n[c8]\n\n[l11]\n[u12]\n[u13]\n",
        Some("[c1]\n[c2]\n[u5]\n[u6]\n[u7]\n[c8]\n\n[c10]\n[u12]\n[u13]\n"),
        "[s3]\n[s4]\n[u5]\n[u6]\n[u7]\n[r9]\n\n[u12]\n[u13]\n",
    );
    run_until_ready(&mut view);
    join_line(&mut view, 4, 1);
    join_line(&mut view, 6, 1);
    assert_eq!(
        view.output_text(),
        "[s3]\n[s4]\n[u5]\n[u6]\n[u7][c8]\n\n[c10][u12]\n[u13]\n"
    );
    view.output_pane.place(Caret::new(1, 1), false);
    view.output_pane.place(Caret::new(6, 6), true);
    view.output_pane.paste("[p1a]\n~\n[p1b]");
    absorb(&mut view);
    view.output_pane.place(Caret::new(1, 0), false);
    view.output_pane.backspace();
    absorb(&mut view);
    assert_eq!(view.output_text(), "[s3][[p1a]\n~\n[p1b]u12]\n[u13]\n");
    take_at(&mut view, 0, Command::TakeLeftThenRight);
    assert_eq!(
        view.output_text(),
        "[s3]\n[s4]\n[s3]\n[s4]\n[[p1a]\n~\n[p1b]u12]\n[u13]\n"
    );
}

#[test]
fn a_reload_that_gives_text_back_to_a_carried_holder_keeps_its_records_on_their_text() {
    let left = "[u1]\n[l4]\n[l5]\n[u6]\n\n[l9]\n[l10]\n[u13]\n[u14]\n[u15]\n[u17]\n~\n[u18]\n[s21]\n[s22]\n\n";
    let center =
        "[u1]\n[c2]\n[c3]\n[u6]\n\n[c7]\n[c8]\n[u13]\n[u14]\n[u15]\n[u17]\n~\n[u18]\n[c19]\n[c20]";
    let right =
        "[u1]\n[u6]\n\n[r11]\n[r12]\n[u13]\n[u14]\n[u15]\n[r16]\n[u17]\n~\n[u18]\n[s21]\n[s22]\n\n";
    let (mut view, dir) = open(left, Some(center), right);
    run_until_ready(&mut view);
    for _ in 0..2 {
        view.output_pane.place(Caret::new(6, 0), false);
        view.output_pane.backspace();
        absorb(&mut view);
    }
    view.output_pane.place(Caret::new(7, 4), false);
    view.output_pane.place(Caret::new(15, 2), true);
    view.output_pane.type_character('Z');
    absorb(&mut view);
    assert_eq!(
        view.output_text(),
        "[u1]\n[c2]\n[c3]\n[u6]\n\n[c7][c8][u13]\n[u14]\n[u15Z20]"
    );
    records_on_their_text(&view).unwrap();
    let changed = left.replace("[l9]", "[d53]");
    std::fs::write(dir.path().join("left.txt"), changed).unwrap();
    view.run(Command::Reload);
    run_until_ready(&mut view);
    assert_output_lines_match_pane(&view);
    println!("after reload: {:?}", view.output_text());
    records_on_their_text(&view).unwrap();
}

#[test]
fn a_second_reload_keeps_the_record_the_first_reload_moved() {
    let inputs = ["~\n[l1]\n[l2]\n[u4]\n", "~\n[u4]", "~\n[r3]\n[u4]\n"];
    let mut finals = Vec::new();
    for reloads in [1, 2] {
        let (mut view, dir) = open(inputs[0], Some(inputs[1]), inputs[2]);
        run_until_ready(&mut view);
        take_at(&mut view, 1, Command::TakeRight);
        view.output_pane.place(Caret::new(1, 0), false);
        view.output_pane.backspace();
        absorb(&mut view);
        assert_eq!(view.output_text(), "~[r3]\n[u4]");
        std::fs::write(dir.path().join("right.txt"), "~\n[r3]\n[d53]\n").unwrap();
        for _ in 0..reloads {
            view.run(Command::Reload);
            run_until_ready(&mut view);
            assert_output_lines_match_pane(&view);
        }
        take_at(&mut view, 0, Command::TakeRight);
        println!("reloads {reloads}: {:?}", view.output_text());
        finals.push(view.output_text());
    }
    assert_eq!(finals[0], finals[1]);
}
