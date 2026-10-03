//! Random sequences of edits, copies, moves and filter changes, checked
//! against a model of the two texts kept as plain strings.
//!
//! The seeds are fixed, so every run takes the same steps. Between steps the
//! comparison is either held, so commands meet the row map of older text, or
//! settled. A copy the view accepts must replace exactly the target lines of
//! the section it acts on with that section's source lines; a copy or a move
//! issued over a row map of older text must change nothing.

use super::row_commands::Fixture;
use super::Side;
use ca_ui::command::Command;
use ca_ui::editor::Caret;
use ca_ui::view::SessionView;
use std::ops::Range;

/// A small deterministic generator, so the crate needs no random source.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            return 0;
        }
        #[allow(clippy::cast_possible_truncation)]
        let value = (self.next() % bound as u64) as usize;
        value
    }

    fn side(&mut self) -> Side {
        if self.below(2) == 0 {
            Side::Left
        } else {
            Side::Right
        }
    }
}

/// The lines of a text with their terminators; no empty segment follows the
/// last terminator.
fn segments(text: &str) -> Vec<String> {
    text.split_inclusive('\n').map(str::to_owned).collect()
}

/// The lines of a text as the comparison numbers them, without terminators.
fn compared_lines(text: &str) -> Vec<String> {
    ca_diff::split_lines(text)
        .into_iter()
        .map(|line| line.trim_end_matches(['\n', '\r']).to_owned())
        .collect()
}

/// The char offset of a caret in a text.
fn offset(text: &str, line: usize, column: usize) -> usize {
    let mut at = 0;
    for (index, piece) in text.split('\n').enumerate() {
        if index == line {
            return at + column.min(piece.chars().count());
        }
        at += piece.chars().count() + 1;
    }
    at
}

fn insert_at(text: &str, at: usize, inserted: &str) -> String {
    let mut out: String = text.chars().take(at).collect();
    out.push_str(inserted);
    out.extend(text.chars().skip(at));
    out
}

fn remove_at(text: &str, at: usize) -> String {
    text.chars()
        .enumerate()
        .filter(|(index, _)| *index != at)
        .map(|(_, character)| character)
        .collect()
}

/// A pair of files whose comparison has several sections, some of them with
/// lines on one side only.
fn pair(rng: &mut Rng) -> (String, String) {
    let count = 4 + rng.below(9);
    let left: Vec<String> = (0..count).map(|index| format!("p{index}")).collect();
    let mut right = Vec::new();
    for line in &left {
        match rng.below(6) {
            0 => {}
            1 => right.push(format!("{line}x")),
            2 => {
                right.push(line.clone());
                right.push(format!("n{}", rng.below(100)));
            }
            _ => right.push(line.clone()),
        }
    }
    let join = |lines: &[String]| -> String {
        lines.iter().fold(String::new(), |mut out, line| {
            out.push_str(line);
            out.push('\n');
            out
        })
    };
    (join(&left), join(&right))
}

#[derive(Debug, Clone, Copy)]
enum Step {
    Type(Side, char),
    Enter(Side),
    Backspace(Side),
    Copy(Side),
    CopyOther,
    CopyLine(Side),
    Move(Command),
    Filter(Command),
    Undo(Side),
    Redo(Side),
    Settle,
}

fn step(rng: &mut Rng) -> Step {
    const MOVES: [Command; 4] = [
        Command::NextSection,
        Command::PreviousSection,
        Command::NextDifference,
        Command::PreviousDifference,
    ];
    const FILTERS: [Command; 4] = [
        Command::ShowAll,
        Command::ShowDifferences,
        Command::ShowSame,
        Command::ShowContext,
    ];
    match rng.below(26) {
        0..=2 => Step::Type(
            rng.side(),
            char::from(b'a' + u8::try_from(rng.below(3)).unwrap_or(0)),
        ),
        3 => Step::Enter(rng.side()),
        4 => Step::Backspace(rng.side()),
        5..=7 => Step::Copy(rng.side()),
        8 => Step::CopyOther,
        9 => Step::CopyLine(rng.side()),
        10..=12 => Step::Move(MOVES[rng.below(MOVES.len())]),
        13 => Step::Filter(FILTERS[rng.below(FILTERS.len())]),
        14 | 15 => Step::Undo(rng.side()),
        16 => Step::Redo(rng.side()),
        _ => Step::Settle,
    }
}

/// The two texts the steps should have produced, every state each side has
/// held, and the text the newest edit of each side left, which Redo returns to.
struct Model {
    left: String,
    right: String,
    seen_left: Vec<String>,
    seen_right: Vec<String>,
    top: (String, String),
}

impl Model {
    fn text(&self, side: Side) -> &str {
        match side {
            Side::Left => &self.left,
            Side::Right => &self.right,
        }
    }

    fn set(&mut self, side: Side, text: String) {
        let (slot, seen) = match side {
            Side::Left => (&mut self.left, &mut self.seen_left),
            Side::Right => (&mut self.right, &mut self.seen_right),
        };
        if !seen.contains(&text) {
            seen.push(text.clone());
        }
        *slot = text;
    }

    /// Record an edit, which empties the side's redo history.
    fn edit(&mut self, side: Side, text: String) {
        match side {
            Side::Left => self.top.0.clone_from(&text),
            Side::Right => self.top.1.clone_from(&text),
        }
        self.set(side, text);
    }

    fn top(&self, side: Side) -> &str {
        match side {
            Side::Left => &self.top.0,
            Side::Right => &self.top.1,
        }
    }

    fn seen(&self, side: Side) -> &[String] {
        match side {
            Side::Left => &self.seen_left,
            Side::Right => &self.seen_right,
        }
    }
}

/// The text a copy of `rows` from `from` leaves in the other pane.
///
/// Works on the row map as the command found it and on plain line lists. An
/// insertion goes after the nearest target line above the rows, else before
/// the nearest one below, else at the start.
fn copied(
    map: &crate::model::RowModel,
    rows: Range<usize>,
    from: Side,
    model: &Model,
) -> Option<String> {
    let to = from.other();
    let mut source: Vec<u32> = Vec::new();
    let mut target: Vec<u32> = Vec::new();
    for index in rows.clone() {
        let row = map.row(index)?;
        source.extend(from.line_of(row));
        target.extend(to.line_of(row));
    }
    if source.is_empty() && target.is_empty() {
        return None;
    }
    let source_lines = compared_lines(model.text(from));
    let mut inserted = String::new();
    if let (Some(first), Some(last)) = (source.first(), source.last()) {
        for line in *first..=*last {
            inserted.push_str(&source_lines[line as usize]);
            inserted.push('\n');
        }
    }
    let (start, end) = if let (Some(first), Some(last)) = (target.first(), target.last()) {
        (*first as usize, *last as usize + 1)
    } else {
        let above = (0..rows.start)
            .rev()
            .find_map(|index| map.row(index).and_then(|row| to.line_of(row)))
            .map(|line| line as usize + 1);
        let below = (rows.end..map.row_count())
            .find_map(|index| map.row(index).and_then(|row| to.line_of(row)))
            .map(|line| line as usize);
        let at = above.or(below).unwrap_or(0);
        (at, at)
    };
    let mut pieces = segments(model.text(to));
    if start >= pieces.len() {
        if let Some(last) = pieces.last_mut() {
            if !last.ends_with('\n') {
                last.push('\n');
            }
        }
    }
    let end = end.min(pieces.len());
    let start = start.min(end);
    pieces.splice(start..end, std::iter::once(inserted));
    Some(pieces.concat())
}

/// A row map that describes the panes: every line of each side on exactly
/// one row, in order, and a matching row carries equal text.
fn check_map(fixture: &Fixture, model: &Model, context: &str) {
    let map = &fixture.view.data.model;
    let left = compared_lines(&model.left);
    let right = compared_lines(&model.right);
    let mut next = (0u32, 0u32);
    for (index, row) in map.rows().iter().enumerate() {
        if let Some(line) = row.left {
            assert_eq!(
                line, next.0,
                "{context}: row {index} holds left line {line}"
            );
            next.0 += 1;
        }
        if let Some(line) = row.right {
            assert_eq!(
                line, next.1,
                "{context}: row {index} holds right line {line}"
            );
            next.1 += 1;
        }
        if let (crate::model::RowClass::Same, Some(l), Some(r)) = (row.class, row.left, row.right) {
            assert_eq!(
                left[l as usize], right[r as usize],
                "{context}: matching row {index} holds different text"
            );
        }
    }
    assert_eq!(next.0 as usize, left.len(), "{context}: left lines missing");
    assert_eq!(
        next.1 as usize,
        right.len(),
        "{context}: right lines missing"
    );
}

fn place(fixture: &mut Fixture, rng: &mut Rng, side: Side) {
    let text = fixture.text(side);
    let lines: Vec<&str> = text.split('\n').collect();
    let line = rng.below(lines.len());
    let column = rng.below(lines[line].chars().count() + 1);
    fixture.view.active = side;
    fixture.view.pane_mut(side).place(
        Caret::new(
            u32::try_from(line).unwrap_or(0),
            u32::try_from(column).unwrap_or(0),
        ),
        false,
    );
}

fn key(key: egui::Key) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::default(),
    }
}

/// Run one step and check what it did.
#[allow(clippy::too_many_lines)]
fn run(fixture: &mut Fixture, model: &mut Model, rng: &mut Rng, step: Step, context: &str) {
    match step {
        Step::Type(side, character) => {
            place(fixture, rng, side);
            let caret = fixture.view.pane(side).caret();
            let at = offset(model.text(side), caret.line as usize, caret.index as usize);
            let expected = insert_at(model.text(side), at, &character.to_string());
            fixture.frame(vec![egui::Event::Text(character.to_string())]);
            model.edit(side, expected);
        }
        Step::Enter(side) => {
            place(fixture, rng, side);
            let caret = fixture.view.pane(side).caret();
            let at = offset(model.text(side), caret.line as usize, caret.index as usize);
            let expected = insert_at(model.text(side), at, "\n");
            fixture.frame(vec![key(egui::Key::Enter)]);
            model.edit(side, expected);
        }
        Step::Backspace(side) => {
            place(fixture, rng, side);
            let caret = fixture.view.pane(side).caret();
            let at = offset(model.text(side), caret.line as usize, caret.index as usize);
            let expected = (at > 0).then(|| remove_at(model.text(side), at - 1));
            fixture.frame(vec![key(egui::Key::Backspace)]);
            if let Some(expected) = expected {
                model.edit(side, expected);
            }
        }
        Step::Copy(_) | Step::CopyLine(_) | Step::CopyOther => {
            let (to, command, line_only) = match step {
                Step::Copy(Side::Right) => (Side::Right, Command::CopyToRight, false),
                Step::Copy(Side::Left) => (Side::Left, Command::CopyToLeft, false),
                Step::CopyLine(Side::Right) => (Side::Right, Command::CopyLineToRight, true),
                Step::CopyLine(Side::Left) => (Side::Left, Command::CopyLineToLeft, true),
                _ => (fixture.view.active.other(), Command::CopyToOtherSide, false),
            };
            let from = to.other();
            let current = fixture.view.rows_current();
            assert_eq!(
                fixture.view.accepts(command),
                current,
                "{context}: {command:?} is offered over a row map of older text"
            );
            let expected = if current {
                let map = &fixture.view.data.model;
                let rows = if line_only {
                    let line = fixture.view.pane(from).caret().line;
                    map.rows()
                        .iter()
                        .position(|row| from.line_of(row) == Some(line))
                        .map(|row| row..row + 1)
                } else {
                    fixture.view.current_section_rows()
                };
                rows.and_then(|rows| copied(map, rows, from, model))
            } else {
                None
            };
            let section = fixture.view.current_section_rows();
            fixture.view.run(command);
            if let Some(text) = expected {
                assert_eq!(
                    fixture.text(to),
                    text,
                    "{context}: section {section:?} of {:?} from {:?}",
                    fixture.view.data.model.rows(),
                    model.text(from)
                );
                model.edit(to, text);
            }
        }
        Step::Move(command) => {
            let current = fixture.view.rows_current();
            assert_eq!(
                fixture.view.accepts(command),
                current,
                "{context}: {command:?}"
            );
            let carets = (
                fixture.view.pane(Side::Left).caret(),
                fixture.view.pane(Side::Right).caret(),
            );
            fixture.view.run(command);
            let after = (
                fixture.view.pane(Side::Left).caret(),
                fixture.view.pane(Side::Right).caret(),
            );
            let active = fixture.view.active;
            let moved = match active {
                Side::Left => after.0 != carets.0,
                Side::Right => after.1 != carets.1,
            };
            if !current {
                assert_eq!(
                    after, carets,
                    "{context}: a move over older rows moved a caret"
                );
            } else if moved {
                let line = fixture.view.pane(active).caret().line;
                let shown = fixture
                    .view
                    .data
                    .model
                    .rows()
                    .iter()
                    .position(|row| active.line_of(row) == Some(line))
                    .is_some_and(|row| fixture.view.visible.shows(row));
                assert!(shown, "{context}: the move left the caret on a hidden line");
                let row = fixture.view.caret;
                fixture.frame(Vec::new());
                assert_eq!(
                    fixture.view.caret, row,
                    "{context}: a frame moved the row the move made current"
                );
            }
        }
        Step::Filter(command) => fixture.view.run(command),
        Step::Undo(side) | Step::Redo(side) => {
            fixture.view.active = side;
            let other = fixture.text(side.other());
            fixture.view.run(if matches!(step, Step::Undo(_)) {
                Command::Undo
            } else {
                Command::Redo
            });
            let text = fixture.text(side);
            assert!(
                model.seen(side).contains(&text),
                "{context}: {step:?} produced a text the side never held: {text:?}"
            );
            assert_eq!(
                fixture.text(side.other()),
                other,
                "{context}: {step:?} changed the other pane"
            );
            model.set(side, text);
        }
        Step::Settle => fixture.settle(),
    }
    for side in [Side::Left, Side::Right] {
        assert_eq!(
            fixture.text(side),
            model.text(side),
            "{context}: the {side:?} pane differs from the model"
        );
    }
    if fixture.view.rows_current() {
        check_map(fixture, model, context);
    }
}

/// Undo every edit, then redo every edit, of both panes.
fn unwind(fixture: &mut Fixture, model: &Model, original: &(String, String), seed: &str) {
    for side in [Side::Left, Side::Right] {
        fixture.undo_all(side);
    }
    assert_eq!(fixture.text(Side::Left), original.0, "{seed}: undo left");
    assert_eq!(fixture.text(Side::Right), original.1, "{seed}: undo right");
    for side in [Side::Left, Side::Right] {
        fixture.view.active = side;
        while fixture.view.pane(side).buffer().can_redo() {
            fixture.view.run(Command::Redo);
            let text = fixture.text(side);
            assert!(
                model.seen(side).contains(&text),
                "{seed}: redo of the {side:?} pane produced {text:?}"
            );
        }
    }
    assert_eq!(
        fixture.text(Side::Left),
        model.top(Side::Left),
        "{seed}: redo left"
    );
    assert_eq!(
        fixture.text(Side::Right),
        model.top(Side::Right),
        "{seed}: redo right"
    );
}

const SEEDS: u64 = 64;
const STEPS: usize = 80;

#[test]
fn random_edit_copy_and_move_sequences_keep_both_panes_on_the_model() {
    let mut copies = 0usize;
    let mut refused = 0usize;
    let mut moves = (0usize, 0usize);
    let mut held_edits = 0usize;
    for seed in 1..=SEEDS {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let original = pair(&mut rng);
        let mut fixture = Fixture::new(&original.0, &original.1);
        let mut model = Model {
            left: original.0.clone(),
            right: original.1.clone(),
            seen_left: vec![original.0.clone()],
            seen_right: vec![original.1.clone()],
            top: original.clone(),
        };
        let mut steps = Vec::new();
        for index in 0..STEPS {
            let next = step(&mut rng);
            steps.push(next);
            let context = format!("seed {seed} step {index} {next:?} after {steps:?}");
            let before = (fixture.text(Side::Left), fixture.text(Side::Right));
            let current = fixture.view.rows_current();
            run(&mut fixture, &mut model, &mut rng, next, &context);
            let changed = before != (fixture.text(Side::Left), fixture.text(Side::Right));
            if !current && changed {
                held_edits += 1;
            }
            if let Step::Move(_) = next {
                if current {
                    moves.0 += 1;
                } else {
                    moves.1 += 1;
                }
            }
            if matches!(next, Step::Copy(_) | Step::CopyLine(_) | Step::CopyOther) {
                if current {
                    copies += usize::from(
                        before != (fixture.text(Side::Left), fixture.text(Side::Right)),
                    );
                } else {
                    refused += 1;
                }
            }
        }
        fixture.settle();
        check_map(&fixture, &model, &format!("seed {seed} settled"));
        unwind(
            &mut fixture,
            &model,
            &original,
            &format!("seed {seed} after {steps:?}"),
        );
    }
    println!(
        "{SEEDS} seeds of {STEPS} steps: {copies} copies changed text, {refused} copies \
         met older rows, {} moves ran, {} moves met older rows, {held_edits} edits landed \
         while a comparison was held",
        moves.0, moves.1
    );
    assert!(
        copies > usize::try_from(SEEDS).unwrap_or(usize::MAX),
        "only {copies} copies changed text"
    );
    assert!(refused > 0, "no copy met a row map of older text");
    assert!(moves.1 > 0, "no move met a row map of older text");
    assert!(held_edits > 0, "no edit landed while a comparison was held");
}
