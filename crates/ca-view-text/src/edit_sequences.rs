//! Random sequences of edits, copies, moves and filter changes, checked
//! against a model of the two texts kept as plain strings.
//!
//! The seeds are fixed, so every run takes the same steps. Between steps the
//! comparison is either held, so commands meet the row map of older text, or
//! settled. A copy the view accepts must replace exactly the target lines of
//! the section it acts on with that section's source lines; a copy or a move
//! issued over a row map of older text must change nothing.
//!
//! The section a move reaches is also recorded as lines of the texts. While
//! the text and the carets stay as the move left them, a section copy must
//! change only those lines, through any comparison an importance rule change
//! starts, as long as those lines still form one section.

use super::row_commands::Fixture;
use super::Side;
use crate::model::RowModel;
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
/// lines on one side only and some that differ in letter case only, which an
/// importance rule turns from an important difference into an unimportant one.
fn pair(rng: &mut Rng) -> (String, String) {
    let count = 4 + rng.below(9);
    let left: Vec<String> = (0..count).map(|index| format!("p{index}")).collect();
    let mut right = Vec::new();
    for line in &left {
        match rng.below(7) {
            0 => {}
            1 => right.push(format!("{line}x")),
            2 => {
                right.push(line.clone());
                right.push(format!("n{}", rng.below(100)));
            }
            3 => right.push(line.to_uppercase()),
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
    /// Flip the letter case importance rule, which compares again with no
    /// edit once the comparison settles.
    Rules,
    /// Toggle Ignore Unimportant.
    Minor,
}

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

fn step(rng: &mut Rng) -> Step {
    match rng.below(28) {
        26 => return Step::Rules,
        27 => return Step::Minor,
        _ => {}
    }
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
/// held, the text the newest edit of each side left, which Redo returns to,
/// and the section the last move reached.
struct Model {
    left: String,
    right: String,
    seen_left: Vec<String>,
    seen_right: Vec<String>,
    top: (String, String),
    reached: Option<Reached>,
    /// Copies checked against the section the last move reached, and those of
    /// them that came after a rule change and the comparison it started.
    reached_copies: (usize, usize),
}

/// The section a move reached, as the lines of each side it held in the
/// texts the move ran over, and the carets the move left.
#[derive(Debug, Clone)]
struct Reached {
    active: Side,
    carets: (Caret, Caret),
    left: Range<usize>,
    right: Range<usize>,
    /// A rule change followed the move.
    ruled: bool,
}

impl Reached {
    fn lines(&self, side: Side) -> Range<usize> {
        match side {
            Side::Left => self.left.clone(),
            Side::Right => self.right.clone(),
        }
    }

    /// True while the active pane and both carets are where the move left
    /// them.
    fn holds(&self, fixture: &Fixture) -> bool {
        fixture.view.active == self.active && carets(fixture) == self.carets
    }

    /// True when the lines still form one whole section of `map`.
    fn is_a_section_of(&self, map: &RowModel) -> bool {
        let row = if self.left.is_empty() {
            map.row_of_right_line(u32::try_from(self.right.start).unwrap_or(u32::MAX))
        } else {
            map.row_of_left_line(u32::try_from(self.left.start).unwrap_or(u32::MAX))
        };
        row.and_then(|row| crate::sidecopy::section_rows(map, row))
            .is_some_and(|rows| {
                section_lines(map, &rows, Side::Left) == self.left
                    && section_lines(map, &rows, Side::Right) == self.right
            })
    }
}

fn carets(fixture: &Fixture) -> (Caret, Caret) {
    (
        fixture.view.pane(Side::Left).caret(),
        fixture.view.pane(Side::Right).caret(),
    )
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

/// The lines of `side` that `rows` hold, from the first to the last.
///
/// A side with no line on the rows gets an empty range where a copy inserts:
/// after the nearest line above the rows, else before the nearest one below,
/// else at the start.
fn section_lines(map: &RowModel, rows: &Range<usize>, side: Side) -> Range<usize> {
    let mut lines = rows
        .clone()
        .filter_map(|index| map.row(index).and_then(|row| side.line_of(row)));
    if let Some(first) = lines.next() {
        let last = lines.last().unwrap_or(first);
        return first as usize..last as usize + 1;
    }
    let above = (0..rows.start)
        .rev()
        .find_map(|index| map.row(index).and_then(|row| side.line_of(row)))
        .map(|line| line as usize + 1);
    let below = (rows.end..map.row_count())
        .find_map(|index| map.row(index).and_then(|row| side.line_of(row)))
        .map(|line| line as usize);
    let at = above.or(below).unwrap_or(0);
    at..at
}

/// The text the other pane holds once lines `source` of `from` replace its
/// lines `target`, worked on plain line lists.
fn spliced(model: &Model, from: Side, source: Range<usize>, target: Range<usize>) -> String {
    let source_lines = compared_lines(model.text(from));
    let mut inserted = String::new();
    for line in source {
        inserted.push_str(&source_lines[line]);
        inserted.push('\n');
    }
    let mut pieces = segments(model.text(from.other()));
    if target.start >= pieces.len() {
        if let Some(last) = pieces.last_mut() {
            if !last.ends_with('\n') {
                last.push('\n');
            }
        }
    }
    let end = target.end.min(pieces.len());
    let start = target.start.min(end);
    pieces.splice(start..end, std::iter::once(inserted));
    pieces.concat()
}

/// The text a copy of `rows` from `from` leaves in the other pane, from the
/// row map as the command found it.
fn copied(map: &RowModel, rows: Range<usize>, from: Side, model: &Model) -> Option<String> {
    let to = from.other();
    let mut holds_a_line = false;
    for index in rows.clone() {
        let row = map.row(index)?;
        holds_a_line |= from.line_of(row).is_some() || to.line_of(row).is_some();
    }
    if !holds_a_line {
        return None;
    }
    Some(spliced(
        model,
        from,
        section_lines(map, &rows, from),
        section_lines(map, &rows, to),
    ))
}

/// The section a move made current, where the view holds one the move
/// reached and the carets are still where the move left them.
fn reached(fixture: &Fixture) -> Option<Reached> {
    let anchor = fixture.view.anchor?;
    let carets = carets(fixture);
    if anchor.active != fixture.view.active || (anchor.left, anchor.right) != carets {
        return None;
    }
    let rows = fixture.view.current_section_rows()?;
    let map = &fixture.view.data.model;
    Some(Reached {
        active: anchor.active,
        carets,
        left: section_lines(map, &rows, Side::Left),
        right: section_lines(map, &rows, Side::Right),
        ruled: false,
    })
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
    if matches!(step, Step::Type(..) | Step::Enter(_) | Step::Backspace(_)) {
        model.reached = None;
    }
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
            let reached_text = model
                .reached
                .as_ref()
                .filter(|reached| {
                    current
                        && !line_only
                        && reached.holds(fixture)
                        && fixture.view.selected_rows(from).is_none()
                        && reached.is_a_section_of(&fixture.view.data.model)
                })
                .map(|reached| spliced(model, from, reached.lines(from), reached.lines(to)));
            fixture.view.run(command);
            if let Some(text) = reached_text {
                assert_eq!(
                    fixture.text(to),
                    text,
                    "{context}: the copy changed other lines than those of the section the last \
                     move reached, {:?}; it acted on rows {section:?} of {:?}",
                    model.reached,
                    fixture.view.data.model.rows(),
                );
                model.reached_copies.0 += 1;
                if model.reached.as_ref().is_some_and(|reached| reached.ruled) {
                    model.reached_copies.1 += 1;
                }
            }
            if current {
                model.reached = None;
            }
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
            if current {
                model.reached = reached(fixture);
            }
        }
        Step::Filter(command) => fixture.view.run(command),
        Step::Undo(side) | Step::Redo(side) => {
            model.reached = None;
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
        Step::Rules => {
            if let Some(reached) = model.reached.as_mut() {
                reached.ruled = true;
            }
            let mut rules = fixture.view.rules();
            rules.case_unimportant = !rules.case_unimportant;
            fixture.view.set_rules(rules);
        }
        Step::Minor => fixture.view.run(Command::ToggleIgnoreUnimportant),
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

/// What the steps of a run of seeds did.
#[derive(Debug, Default)]
struct Totals {
    copies: usize,
    refused: usize,
    moves: (usize, usize),
    held_edits: usize,
    reached_copies: (usize, usize),
}

/// Run `steps` steps drawn by `next` from a fresh pair for each seed.
fn drive(seeds: Range<u64>, steps: usize, next: fn(&mut Rng) -> Step) -> Totals {
    let mut totals = Totals::default();
    for seed in seeds {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let original = pair(&mut rng);
        let mut fixture = Fixture::new(&original.0, &original.1);
        let mut model = Model {
            left: original.0.clone(),
            right: original.1.clone(),
            seen_left: vec![original.0.clone()],
            seen_right: vec![original.1.clone()],
            top: original.clone(),
            reached: None,
            reached_copies: (0, 0),
        };
        let mut taken = Vec::new();
        for index in 0..steps {
            let step = next(&mut rng);
            taken.push(step);
            let context = format!("seed {seed} step {index} {step:?} after {taken:?}");
            let before = (fixture.text(Side::Left), fixture.text(Side::Right));
            let current = fixture.view.rows_current();
            run(&mut fixture, &mut model, &mut rng, step, &context);
            let changed = before != (fixture.text(Side::Left), fixture.text(Side::Right));
            if !current && changed {
                totals.held_edits += 1;
            }
            if let Step::Move(_) = step {
                if current {
                    totals.moves.0 += 1;
                } else {
                    totals.moves.1 += 1;
                }
            }
            if matches!(step, Step::Copy(_) | Step::CopyLine(_) | Step::CopyOther) {
                if current {
                    totals.copies += usize::from(changed);
                } else {
                    totals.refused += 1;
                }
            }
        }
        fixture.settle();
        check_map(&fixture, &model, &format!("seed {seed} settled"));
        unwind(
            &mut fixture,
            &model,
            &original,
            &format!("seed {seed} after {taken:?}"),
        );
        totals.reached_copies.0 += model.reached_copies.0;
        totals.reached_copies.1 += model.reached_copies.1;
    }
    totals
}

const SEEDS: u64 = 64;
const STEPS: usize = 80;

#[test]
fn random_edit_copy_and_move_sequences_keep_both_panes_on_the_model() {
    let totals = drive(1..SEEDS + 1, STEPS, step);
    println!("{SEEDS} seeds of {STEPS} steps: {totals:?}");
    assert!(
        totals.copies > usize::try_from(SEEDS).unwrap_or(usize::MAX),
        "only {} copies changed text",
        totals.copies
    );
    assert!(totals.refused > 0, "no copy met a row map of older text");
    assert!(totals.moves.1 > 0, "no move met a row map of older text");
    assert!(
        totals.held_edits > 0,
        "no edit landed while a comparison was held"
    );
    assert!(
        totals.reached_copies.0 > 0,
        "no copy acted on the section a move reached"
    );
}

/// Mostly moves, rule changes, settles and section copies, with few edits, so
/// a copy often follows a move and the comparison a rule change started.
fn rule_step(rng: &mut Rng) -> Step {
    match rng.below(12) {
        0..=2 => Step::Move(MOVES[rng.below(MOVES.len())]),
        3 | 4 => Step::Rules,
        5 | 6 => Step::Settle,
        7 => Step::Minor,
        8 => Step::Filter(FILTERS[rng.below(FILTERS.len())]),
        9 | 10 => Step::Copy(rng.side()),
        _ => Step::CopyOther,
    }
}

#[test]
fn random_move_rule_change_and_copy_sequences_copy_the_section_a_move_reached() {
    let totals = drive(1_001..1_033, 60, rule_step);
    println!("32 seeds of 60 steps: {totals:?}");
    assert!(
        totals.reached_copies.1 > 0,
        "no copy acted on the section a move reached after a rule change"
    );
}
