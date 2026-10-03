use super::*;
use ca_ui::editor::Caret;

const F1: [&str; 3] = [
    "a\nL\nc\nd\ng\nh\ni\nX\nf\n",
    "a\nb\nc\nd\ng\nh\ni\ne\nf\n",
    "a\nR\nc\nd\ng\nh\ni\ne\nf\n",
];

fn key(key: egui::Key, shift: bool) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: if shift {
            egui::Modifiers::SHIFT
        } else {
            egui::Modifiers::NONE
        },
    }
}

fn random_caret(r: &mut Seeded, view: &MergeView) -> Caret {
    let lines = view.output_pane.line_count().max(1);
    let line = u32::try_from(r.pick(lines as usize)).unwrap();
    let len = view.output_pane.line_text(line).chars().count();
    Caret::new(line, u32::try_from(r.pick(len + 1)).unwrap())
}

fn random_event(r: &mut Seeded) -> egui::Event {
    match r.pick(11) {
        0 | 1 => egui::Event::Text("W".to_owned()),
        2 => egui::Event::Text("UV".to_owned()),
        3 => key(egui::Key::Enter, false),
        4 => key(egui::Key::Backspace, false),
        5 => key(egui::Key::Delete, false),
        6 => egui::Event::Paste("P1\nP2".to_owned()),
        7 => egui::Event::Cut,
        8 => key(egui::Key::ArrowRight, true),
        9 => key(egui::Key::ArrowDown, true),
        _ => key(egui::Key::ArrowLeft, false),
    }
}

const FRAME_FIXTURES: [[&str; 3]; 4] = [
    F1,
    [
        "a\nL\nc\nc2\nc3\nX\nf\nM\ng\n",
        "a\nb\nc\nc2\nc3\ne\nf\nm\ng\n",
        "a\nR\nc\nc2\nc3\ne\nf\nm\ng\n",
    ],
    [
        "a\nL\nc\nc2\nc3\nX1\nX2\nf\n",
        "a\nb\nc\nc2\nc3\ne\nf\n",
        "a\nR\nc\nc2\nc3\ne\nf\n",
    ],
    [
        "a\r\nL\r\nc\r\nd\r\ng\r\nX\r\nf",
        "a\r\nb\r\nc\r\nd\r\ng\r\ne\r\nf",
        "a\r\nR\r\nc\r\nd\r\ng\r\ne\r\nf",
    ],
];

fn model_pane(view: &MergeView) -> Result<(), String> {
    let pane = view.output_pane.buffer().text();
    let lines = ca_diff::split_lines(&pane);
    let model: Vec<&str> = view
        .model()
        .output_lines()
        .iter()
        .map(String::as_str)
        .collect();
    if view.output_text() != pane || model != lines {
        return Err(format!("model {:?} pane {pane:?}", view.output_text()));
    }
    Ok(())
}

/// Frames of 2 to 8 mixed events, with Replace All, Save, Reload, takes and
/// Undo between them, then Undo to the bottom and Redo to the top.
#[test]
#[allow(clippy::too_many_lines)]
fn frames_of_mixed_events_undo_and_redo_with_the_model_on_the_pane() {
    let mut r = Seeded(0x5eed_0003);
    let mut frames = 0;
    let mut undos_total = 0;
    let mut redos_total = 0;
    let mut problems = Vec::new();
    for case in 0..600 {
        let inputs = FRAME_FIXTURES[case % FRAME_FIXTURES.len()];
        let (mut view, dir) = open(inputs[0], Some(inputs[1]), inputs[2]);
        run_until_ready(&mut view);
        view.focus = crate::Pane::Output;
        let loaded = view.output_text();
        let mut log = Vec::new();
        let steps = 2 + r.pick(7);
        let mut reloaded = false;
        let mut failed = false;
        for _ in 0..steps {
            match r.pick(10) {
                0 => {
                    view.run(Command::Replace);
                    let settings = &mut view.find_panel().settings;
                    settings.pattern = ["c", "W", "\\n"][r.pick(3)].to_owned();
                    settings.replacement = ["CC", "", "\\n\\n"][r.pick(3)].to_owned();
                    settings.regex = true;
                    view.answer_panel(Some(crate::PanelRequest::ReplaceAll));
                    finish_search(&mut view);
                    view.focus = crate::Pane::Output;
                    log.push("ReplaceAll".to_owned());
                }
                1 => {
                    let _ = save_and_read(&mut view, &dir);
                    log.push("Save".to_owned());
                }
                2 => {
                    let section = r.pick(view.model().sections().len());
                    let command = [Command::TakeLeft, Command::TakeRight][r.pick(2)];
                    view.output_pane.clear_selection();
                    view.current = section;
                    view.run(command);
                    log.push(format!("Take {section} {command:?}"));
                }
                3 => {
                    view.run(Command::Undo);
                    log.push("Undo".to_owned());
                }
                4 if !reloaded => {
                    view.run(Command::Reload);
                    run_until_ready(&mut view);
                    view.focus = crate::Pane::Output;
                    reloaded = true;
                    log.push("Reload".to_owned());
                }
                _ => {
                    let caret = random_caret(&mut r, &view);
                    view.output_pane.place(caret, false);
                    let count = 2 + r.pick(7);
                    let events: Vec<egui::Event> =
                        (0..count).map(|_| random_event(&mut r)).collect();
                    if r.one_in(4) {
                        // The shell runs a shortcut before the view reads the
                        // other events of the frame.
                        view.run(Command::Undo);
                        log.push("Undo in frame".to_owned());
                    }
                    log.push(format!("Frame {caret:?} {events:?}"));
                    let _ = frame_output(&mut view, events);
                    frames += 1;
                }
            }
            if let Err(problem) = model_pane(&view) {
                problems.push(format!("case {case} after {log:?}: {problem}"));
                failed = true;
                break;
            }
        }
        if failed {
            continue;
        }
        let mut ups = 0;
        while view.output_pane.buffer().can_redo() && ups < 200 {
            view.run(Command::Redo);
            ups += 1;
            if let Err(problem) = model_pane(&view) {
                problems.push(format!(
                    "case {case} first redo {ups} after {log:?}: {problem}"
                ));
                failed = true;
                break;
            }
        }
        if failed {
            continue;
        }
        let top = view.output_text();
        let mut undos = 0;
        while view.output_pane.buffer().can_undo() && undos < 200 {
            view.run(Command::Undo);
            undos += 1;
            if let Err(problem) = model_pane(&view) {
                problems.push(format!("case {case} undo {undos} after {log:?}: {problem}"));
                failed = true;
                break;
            }
        }
        undos_total += undos;
        if failed {
            continue;
        }
        if !reloaded && view.output_text() != loaded {
            problems.push(format!(
                "case {case} undo-all {:?} loaded {loaded:?} after {log:?}",
                view.output_text()
            ));
            continue;
        }
        let mut redos = 0;
        while view.output_pane.buffer().can_redo() && redos < 200 {
            view.run(Command::Redo);
            redos += 1;
            if let Err(problem) = model_pane(&view) {
                problems.push(format!("case {case} redo {redos} after {log:?}: {problem}"));
                failed = true;
                break;
            }
        }
        redos_total += redos;
        if !failed && view.output_text() != top {
            problems.push(format!(
                "case {case} redo-all {:?} top {top:?} after {log:?}",
                view.output_text()
            ));
        }
    }
    println!(
        "FRAMES frames {frames} undos {undos_total} redos {redos_total} problems {}",
        problems.len()
    );
    for problem in problems.iter().take(5) {
        println!("PROBLEM {problem}");
    }
    assert!(problems.is_empty());
}

/// Typed characters that join one undo group across frames undo in one step.
#[test]
fn typing_across_frames_joins_one_undo_group_and_undoes_in_one_step() {
    let (mut view, _dir) = open(F1[0], Some(F1[1]), F1[2]);
    run_until_ready(&mut view);
    view.focus = crate::Pane::Output;
    view.output_pane.place(Caret::new(3, 1), false);
    for character in ["x", "y", "z"] {
        let _ = frame_output(&mut view, vec![egui::Event::Text(character.to_owned())]);
        assert_output_lines_match_pane(&view);
    }
    assert_eq!(view.output_text(), "a\nb\nc\ndxyz\ng\nh\ni\nX\nf\n");
    view.run(Command::Undo);
    assert_output_lines_match_pane(&view);
    assert_eq!(view.output_text(), "a\nb\nc\nd\ng\nh\ni\nX\nf\n");
    view.run(Command::Redo);
    assert_output_lines_match_pane(&view);
    assert_eq!(view.output_text(), "a\nb\nc\ndxyz\ng\nh\ni\nX\nf\n");
    view.output_pane.place(Caret::new(1, 1), false);
    let _ = frame_output(
        &mut view,
        vec![
            egui::Event::Text("Q".to_owned()),
            key(egui::Key::Enter, false),
            egui::Event::Text("R".to_owned()),
            key(egui::Key::Backspace, false),
            key(egui::Key::Backspace, false),
            key(egui::Key::Backspace, false),
        ],
    );
    assert_output_lines_match_pane(&view);
    while view.output_pane.buffer().can_undo() {
        view.run(Command::Undo);
        assert_output_lines_match_pane(&view);
    }
    assert_eq!(view.output_text(), "a\nb\nc\nd\ng\nh\ni\nX\nf\n");
    assert_eq!(view.model().totals().conflicts_remaining, 1);
}

fn large_view(lines: usize) -> (MergeView, tempfile::TempDir) {
    use std::fmt::Write as _;
    let mut shared = String::new();
    for index in 0..lines {
        let _ = writeln!(shared, "line {index} q");
    }
    let (mut view, dir) = open(
        &format!("a\nL\n{shared}"),
        Some(&format!("a\nb\n{shared}")),
        &format!("a\nR\n{shared}"),
    );
    run_until_ready(&mut view);
    view.focus = crate::Pane::Output;
    (view, dir)
}

fn counters() -> (usize, usize, usize) {
    (
        crate::model::MergeModel::edited_output_read_count(),
        crate::model::MergeModel::sequence_items_touched(),
        crate::model::ownership::FILTER_PASSES.with(std::cell::Cell::get),
    )
}

fn reset() {
    crate::model::MergeModel::reset_rebuild_visit_counts();
    crate::model::ownership::FILTER_PASSES.with(|c| c.set(0));
    crate::model::ownership::PANE_EDITS.with(|c| c.set(0));
}

#[test]
fn cost_of_one_character_and_one_frame_does_not_grow_with_the_document() {
    let mut results = Vec::new();
    for lines in [20_000, 200_000] {
        let (mut view, _dir) = large_view(lines);
        let middle = u32::try_from(lines / 2).unwrap();
        reset();
        view.output_pane.place(Caret::new(middle, 0), false);
        let _ = frame_output(&mut view, vec![egui::Event::Text("Z".to_owned())]);
        let one = counters();
        reset();
        view.output_pane.place(Caret::new(middle, 3), false);
        let _ = frame_output(
            &mut view,
            vec![
                egui::Event::Text("A".to_owned()),
                key(egui::Key::Enter, false),
                egui::Event::Text("B".to_owned()),
                key(egui::Key::Backspace, false),
                key(egui::Key::Backspace, false),
                egui::Event::Paste("P\nQ".to_owned()),
                key(egui::Key::Delete, false),
                egui::Event::Text("C".to_owned()),
            ],
        );
        let eight = counters();
        let groups = crate::model::ownership::PANE_EDITS.with(std::cell::Cell::get);
        assert_output_lines_match_pane(&view);
        reset();
        view.run(Command::Undo);
        let undo = counters();
        println!("COST lines {lines}: one char (reads, items, filter passes) {one:?}; frame of 8 events {eight:?} pane edits {groups}; undo {undo:?}");
        results.push((one, eight));
    }
    assert_eq!(results[0].0 .0, results[1].0 .0);
    assert_eq!(results[0].1 .0, results[1].1 .0);
}

#[test]
fn cost_of_a_replace_all_with_5000_replacements() {
    let (mut view, _dir) = large_view(5_000);
    reset();
    view.run(Command::Replace);
    view.find_panel().settings.pattern = "q".to_owned();
    view.find_panel().settings.replacement = "Q".to_owned();
    view.answer_panel(Some(crate::PanelRequest::ReplaceAll));
    finish_search(&mut view);
    assert_output_lines_match_pane(&view);
    let pane_edits = crate::model::ownership::PANE_EDITS.with(std::cell::Cell::get);
    println!(
        "COST replace all 5000: (reads, items, filter passes) {:?} pane edits {pane_edits}",
        counters()
    );
    let (mut view, _dir) = large_view(5_000);
    reset();
    view.run(Command::Replace);
    view.find_panel().settings.pattern = "q$".to_owned();
    view.find_panel().settings.replacement = "q\nq".to_owned();
    view.find_panel().settings.regex = true;
    view.answer_panel(Some(crate::PanelRequest::ReplaceAll));
    finish_search(&mut view);
    assert_output_lines_match_pane(&view);
    let pane_edits = crate::model::ownership::PANE_EDITS.with(std::cell::Cell::get);
    println!("COST replace all 5000 line splits: (reads, items, filter passes) {:?} pane edits {pane_edits}", counters());
}

#[test]
fn cost_of_line_splitting_replacements_by_document_size() {
    for lines in [5_000, 50_000] {
        let (mut view, _dir) = large_view(lines);
        reset();
        view.run(Command::Replace);
        view.find_panel().settings.pattern = "^line [0-9]*[05]00 q$".to_owned();
        view.find_panel().settings.replacement = "x\ny".to_owned();
        view.find_panel().settings.regex = true;
        view.answer_panel(Some(crate::PanelRequest::ReplaceAll));
        finish_search(&mut view);
        assert_output_lines_match_pane(&view);
        let pane_edits = crate::model::ownership::PANE_EDITS.with(std::cell::Cell::get);
        println!(
            "COST split lines {lines}: (reads, items, filter passes) {:?} pane edits {pane_edits}",
            counters()
        );
    }
}
