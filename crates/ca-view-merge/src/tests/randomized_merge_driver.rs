use super::*;
use ca_ui::editor::Caret;
use std::collections::{BTreeMap, HashMap, HashSet};

const SEEDS: [u64; 20] = [
    0x1001, 0x1002, 0x1003, 0x1004, 0x1005, 0x1006, 0x1007, 0x1008, 0x1009, 0x100a, 0x3001, 0x3002,
    0x3003, 0x3004, 0x3005, 0x3006, 0x3007, 0x3008, 0x3009, 0x300a,
];
const CASES: usize = 600;
const CLAMP_COUNTERS: [&std::thread::LocalKey<std::cell::Cell<usize>>; 3] = [
    &crate::model::SPLICE_CLAMPS,
    &crate::model::REPLAY_START_CLAMPS,
    &crate::model::REPLAY_END_CLAMPS,
];
const MAX_OPS: usize = 24;

const ASSERTED: &[&str] = &[
    "panic",
    "save",
    "model-pane",
    "redo-to-top-model-pane",
    "undo-model-pane",
    "redo-model-pane",
    "reload-changed-text",
    "rules-changed-text",
    "toggle-changed-text",
    "save-changed-text",
    "take-lost-other-section-token",
    "take-resurrected-other-token",
    "take-duplicated-other-token",
    "line-take-lost-token",
    "take-all-lost-conflict-token",
    "take-all-duplicated-token",
    "undo-all-not-baseline",
    "redo-all-not-end",
    "redo-path-differs-from-undo-path",
    "undo-all-state-differs-from-fresh",
    "take-after-undo-all-differs-from-fresh",
    "take-changed-other-section-text",
    "take-all-changed-conflict-text",
    "ownership-changed-without-edit",
    "lent-record-off-text",
    "lent-record-invariant",
    "take-reordered-other-section-text",
    "take-restore-differs-from-selected-input",
    "take-order-differs",
    "take-restore-duplicated-token",
    "take-order-replay-differs",
    "edit-kept-take-restore",
];

/// Every token with its byte range in `text`.
fn token_spans(text: &str) -> Vec<(String, usize, usize)> {
    let mut out = Vec::new();
    let mut base = 0usize;
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let inner_start = base + open + 1;
        let after = &rest[open + 1..];
        let Some(close) = after.find(']') else { break };
        let inner = &after[..close];
        if let Some(nested) = inner.rfind('[') {
            base = inner_start + nested;
            rest = &after[nested..];
            continue;
        }
        if !inner.is_empty() && !inner.contains(['\r', '\n']) {
            out.push((
                format!("[{inner}]"),
                inner_start - 1,
                inner_start + close + 1,
            ));
        }
        base = inner_start + close;
        rest = &after[close..];
    }
    out
}

fn tokens(text: &str) -> Vec<String> {
    token_spans(text)
        .into_iter()
        .map(|(token, _, _)| token)
        .collect()
}

const NO_OWNER: usize = usize::MAX;

/// The model's output text with the section that owns each byte: the
/// section of the line, or the lender of a lent record on that line.
struct Owned {
    text: String,
    owners: Vec<usize>,
    problems: Vec<String>,
}

fn owned_text(view: &MergeView) -> Owned {
    let model = view.model();
    let lines: Vec<String> = model.output_lines().iter().cloned().collect();
    let mut per_line: Vec<Vec<usize>> = lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let owner = u32::try_from(index)
                .ok()
                .and_then(|index| model.section_of_output_line(index))
                .unwrap_or(NO_OWNER);
            vec![owner; line.chars().count()]
        })
        .collect();
    let mut problems = Vec::new();
    for (lender, section) in model.sections().iter().enumerate() {
        for entry in section.lent_records() {
            let Some(range) = model.output_range(entry.holder) else {
                problems.push(format!(
                    "s{lender} record on missing holder s{}",
                    entry.holder
                ));
                continue;
            };
            let line = (range.start + entry.offset) as usize;
            let column = entry.column as usize;
            let count = entry.text.chars().count();
            let here: String = lines
                .get(line)
                .map(|text| text.chars().skip(column).take(count).collect())
                .unwrap_or_default();
            if line >= range.end as usize || here != entry.text {
                problems.push(format!(
                    "s{lender} record {:?} at s{} line {line} column {column} finds {here:?}",
                    entry.text, entry.holder
                ));
                continue;
            }
            for owner in per_line[line].iter_mut().skip(column).take(count) {
                *owner = lender;
            }
        }
    }
    let mut text = String::new();
    let mut owners = Vec::new();
    for (line, own) in lines.iter().zip(&per_line) {
        for (ch, owner) in line.chars().zip(own) {
            owners.extend(std::iter::repeat_n(*owner, ch.len_utf8()));
        }
        text.push_str(line);
    }
    Owned {
        text,
        owners,
        problems,
    }
}

/// The one section that owns every byte of a span, if one does.
fn single_owner(owners: &[usize]) -> Option<usize> {
    let first = *owners.first()?;
    (first != NO_OWNER && owners.iter().all(|owner| *owner == first)).then_some(first)
}

/// Each section's characters in output order, without line terminators.
fn section_chars(owned: &Owned, sections: usize) -> Vec<String> {
    let mut out = vec![String::new(); sections];
    for (index, ch) in owned.text.char_indices() {
        if ch == '\r' || ch == '\n' {
            continue;
        }
        if let Some(text) = out.get_mut(owned.owners[index]) {
            text.push(ch);
        }
    }
    out
}

fn sorted_chars(text: &str) -> Vec<char> {
    let mut chars: Vec<char> = text.chars().collect();
    chars.sort_unstable();
    chars
}

/// Tokens whose every character belongs to one section in `keep`.
fn kept_tokens(owned: &Owned, keep: &dyn Fn(usize) -> bool) -> HashMap<String, usize> {
    let mut map = HashMap::new();
    for (token, start, end) in token_spans(&owned.text) {
        if single_owner(&owned.owners[start..end]).is_some_and(keep) {
            *map.entry(token).or_insert(0) += 1;
        }
    }
    map
}

fn counts(text: &str) -> HashMap<String, usize> {
    let mut map = HashMap::new();
    for token in tokens(text) {
        *map.entry(token).or_insert(0) += 1;
    }
    map
}

fn token_owner(view: &MergeView) -> HashMap<String, (usize, Option<usize>)> {
    let model = view.model();
    let inputs = model.inputs();
    let mut map = HashMap::new();
    let mut rank = 0usize;
    for (index, section) in model.sections().iter().enumerate() {
        let unchanged = matches!(section.kind, ca_diff::merge3::MergeKind::Unchanged);
        for (lines, range) in [
            (&inputs.center, &section.center),
            (&inputs.left, &section.left),
            (&inputs.right, &section.right),
        ] {
            for line in &lines[range.start as usize..range.end as usize] {
                for token in tokens(line) {
                    map.entry(token).or_insert_with(|| {
                        let r = unchanged.then_some(rank);
                        rank += 1;
                        (index, r)
                    });
                }
            }
        }
    }
    map
}

#[derive(Default, Debug)]
struct Stats {
    cases: usize,
    steps: usize,
    edits: usize,
    frames: usize,
    edge_edits: usize,
    replace_alls: usize,
    takes: usize,
    line_takes: usize,
    take_alls: usize,
    toggles: usize,
    undos: usize,
    redos: usize,
    reloads: usize,
    changed_reloads: usize,
    swaps: usize,
    rules: usize,
    saves: usize,
    marker_saves: usize,
    empty_inputs: usize,
    resyncs: usize,
    reabsorbs: usize,
    clamps: usize,
    panics: usize,
    take_order_checks: usize,
    pending_takes_restored: usize,
}

struct Gen(Seeded, usize);

impl Gen {
    fn inputs(&mut self) -> (String, String, String) {
        let r = &mut self.0;
        let mut counter = 0usize;
        let mut next = |tag: &str| {
            counter += 1;
            format!("[{tag}{counter}]")
        };
        let mut left = Vec::new();
        let mut center = Vec::new();
        let mut right = Vec::new();
        let blocks = 1 + r.pick(5);
        let filler = |r: &mut Seeded| -> Option<String> {
            match r.pick(10) {
                0 => Some("~".to_owned()),
                1 => Some("~e\u{301}\t\u{1F600}".to_owned()),
                2 => Some(String::new()),
                _ => None,
            }
        };
        for block in 0..=blocks {
            let run = if block == 0 { r.pick(3) } else { 1 + r.pick(3) };
            for _ in 0..run {
                let line = filler(r).unwrap_or_else(|| next("u"));
                left.push(line.clone());
                center.push(line.clone());
                right.push(line);
            }
            if block == blocks {
                break;
            }
            let base: Vec<String> = (0..r.pick(3)).map(|_| next("c")).collect();
            let kind = r.pick(5);
            let changed = |r: &mut Seeded, tag: &str, next: &mut dyn FnMut(&str) -> String| {
                let mut lines: Vec<String> = (0..r.pick(3)).map(|_| next(tag)).collect();
                if lines.is_empty() && r.one_in(2) {
                    lines.push(next(tag));
                }
                lines
            };
            center.extend(base.iter().cloned());
            match kind {
                0 => {
                    left.extend(changed(r, "l", &mut next));
                    right.extend(base.iter().cloned());
                }
                1 => {
                    left.extend(base.iter().cloned());
                    right.extend(changed(r, "r", &mut next));
                }
                2 => {
                    let same = changed(r, "s", &mut next);
                    left.extend(same.iter().cloned());
                    right.extend(same);
                }
                _ => {
                    left.extend(changed(r, "l", &mut next));
                    right.extend(changed(r, "r", &mut next));
                }
            }
        }
        let style = r.pick(6);
        let join = |r: &mut Seeded, lines: &[String]| {
            let mut text = String::new();
            for line in lines {
                text.push_str(line);
                text.push_str(r.ending(match style {
                    0 | 1 => 0,
                    2 => 1,
                    3 => 2,
                    _ => 3,
                }));
            }
            if r.one_in(3) {
                let kept = text.trim_end_matches(['\r', '\n']).len();
                text.truncate(kept);
            }
            text
        };
        let mut l = join(r, &left);
        let mut c = join(r, &center);
        let mut rr = join(r, &right);
        match r.pick(12) {
            0 => {
                l.clear();
                c.clear();
                rr.clear();
                self.1 += 1;
            }
            1 => {
                c.clear();
                self.1 += 1;
            }
            2 => {
                l.clear();
                self.1 += 1;
            }
            _ => {}
        }
        (l, c, rr)
    }
}

#[derive(Clone, Debug)]
enum Small {
    Type(Caret),
    Enter(Caret),
    Backspace(Caret),
    Delete(Caret),
}

#[derive(Clone, Debug)]
enum Op {
    Join(u32),
    DeleteAtEnd(u32),
    SelectEdit(Caret, Caret, u8),
    Split(Caret),
    Type(Caret),
    Frame(Vec<Small>),
    Edge(u8),
    ReplaceAll(u8),
    Take(usize, Command, Option<(Caret, Caret)>),
    TakeLine(usize, Command),
    TakeAll,
    ToggleConflict(usize),
    ToggleIgnored(usize),
    Undo,
    Redo,
    Reload,
    ReloadChanged(u8, usize),
    Swap,
    Rules(Command),
    Save,
}

fn pick_op(r: &mut Seeded, view: &MergeView, frames: bool) -> Op {
    let sections = view.model().sections().len();
    let lines = view.output_pane.line_count();
    let take_cmd = |r: &mut Seeded| {
        [
            Command::TakeLeft,
            Command::TakeCenter,
            Command::TakeRight,
            Command::TakeLeftThenRight,
            Command::TakeRightThenLeft,
        ][r.pick(5)]
    };
    loop {
        let choice = r.pick(36);
        return match choice {
            0..=3 => Op::Join(1 + u32::try_from(r.pick(lines.max(2) as usize - 1)).unwrap()),
            4 | 5 => Op::DeleteAtEnd(u32::try_from(r.pick(lines as usize)).unwrap()),
            6..=8 => {
                let a = r.caret(view);
                let b = r.caret(view);
                Op::SelectEdit(a, b, u8::try_from(r.pick(4)).unwrap())
            }
            9 => Op::Split(r.caret(view)),
            10 => Op::Type(r.caret(view)),
            11..=14 => {
                let selection = r.one_in(5).then(|| (r.caret(view), r.caret(view)));
                Op::Take(r.pick(sections), take_cmd(r), selection)
            }
            15 => Op::TakeLine(
                r.pick(view.model().rows().len()),
                [
                    Command::TakeLeftLine,
                    Command::TakeCenterLine,
                    Command::TakeRightLine,
                ][r.pick(3)],
            ),
            16 => Op::TakeAll,
            17 => Op::ToggleConflict(r.pick(sections)),
            18 => Op::ToggleIgnored(r.pick(sections)),
            19..=21 => Op::Undo,
            22 | 23 => Op::Redo,
            24 => Op::Reload,
            25 if r.one_in(2) => Op::Swap,
            26 => Op::Rules(
                [
                    Command::ToggleIgnoreSameChanges,
                    Command::ToggleIgnoreUnimportant,
                ][r.pick(2)],
            ),
            27 => Op::Save,
            28 | 29 => Op::Edge(u8::try_from(r.pick(6)).unwrap()),
            30 | 31 => Op::ReplaceAll(u8::try_from(r.pick(5)).unwrap()),
            32 => Op::ReloadChanged(u8::try_from(r.pick(3)).unwrap(), r.pick(64)),
            33 | 34 if frames => {
                let count = 2 + r.pick(3);
                let mut smalls = Vec::new();
                for _ in 0..count {
                    let caret = r.caret(view);
                    smalls.push(match r.pick(4) {
                        0 => Small::Type(caret),
                        1 => Small::Enter(caret),
                        2 => Small::Backspace(caret),
                        _ => Small::Delete(caret),
                    });
                }
                Op::Frame(smalls)
            }
            _ => continue,
        };
    }
}

fn pane_text(view: &MergeView) -> String {
    view.output_pane.buffer().text()
}

fn model_matches_pane(view: &MergeView) -> Result<(), String> {
    let pane = pane_text(view);
    let lines = ca_diff::split_lines(&pane);
    let model: Vec<&str> = view
        .model()
        .output_lines()
        .iter()
        .map(String::as_str)
        .collect();
    if view.output_text() != pane || model != lines {
        return Err(format!("model {:?} pane {:?}", view.output_text(), pane));
    }
    Ok(())
}

thread_local! {
    static PASTE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static DUMP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn clamp_caret(view: &MergeView, caret: Caret) -> Caret {
    let line = caret
        .line
        .min(view.output_pane.line_count().saturating_sub(1));
    let len = u32::try_from(view.output_pane.line_text(line).chars().count()).unwrap();
    Caret::new(line, caret.index.min(len))
}

#[allow(clippy::too_many_lines)]
fn apply(
    view: &mut MergeView,
    dir: &tempfile::TempDir,
    op: &Op,
    stats: &mut Stats,
) -> Option<String> {
    view.output_pane.clear_selection();
    view.focus = crate::Pane::Output;
    match op {
        Op::Join(line) => {
            let line = (*line).min(view.output_pane.line_count().saturating_sub(1));
            view.output_pane.place(Caret::new(line, 0), false);
            view.output_pane.backspace();
            view.absorb_output_edits();
            stats.edits += 1;
        }
        Op::DeleteAtEnd(line) => {
            let line = (*line).min(view.output_pane.line_count().saturating_sub(1));
            let len = u32::try_from(view.output_pane.line_text(line).chars().count()).unwrap();
            view.output_pane.place(Caret::new(line, len), false);
            view.output_pane.delete();
            view.absorb_output_edits();
            stats.edits += 1;
        }
        Op::SelectEdit(a, b, kind) => {
            view.output_pane.place(*a, false);
            view.output_pane.place(*b, true);
            match kind {
                0 => view.output_pane.type_character('Z'),
                1 => view.output_pane.backspace(),
                2 => {
                    let ending = view.terminator();
                    view.output_pane.enter(ending);
                }
                _ => {
                    let ending = view.terminator();
                    let n = PASTE.with(|p| {
                        let v = p.get() + 1;
                        p.set(v);
                        v
                    });
                    let text = format!("[p{n}a]{ending}~{ending}[p{n}b]");
                    view.output_pane.paste(&text);
                }
            }
            view.absorb_output_edits();
            stats.edits += 1;
        }
        Op::Split(caret) => {
            view.output_pane.place(*caret, false);
            let ending = view.terminator();
            view.output_pane.enter(ending);
            view.absorb_output_edits();
            stats.edits += 1;
        }
        Op::Type(caret) => {
            view.output_pane.place(*caret, false);
            view.output_pane.type_character('Z');
            view.absorb_output_edits();
            stats.edits += 1;
        }
        Op::Frame(smalls) => {
            for small in smalls {
                let ending = view.terminator();
                match small {
                    Small::Type(caret) => {
                        let caret = clamp_caret(view, *caret);
                        view.output_pane.place(caret, false);
                        view.output_pane.type_character('W');
                    }
                    Small::Enter(caret) => {
                        let caret = clamp_caret(view, *caret);
                        view.output_pane.place(caret, false);
                        view.output_pane.enter(ending);
                    }
                    Small::Backspace(caret) => {
                        let caret = clamp_caret(view, *caret);
                        view.output_pane.place(caret, false);
                        view.output_pane.backspace();
                    }
                    Small::Delete(caret) => {
                        let caret = clamp_caret(view, *caret);
                        view.output_pane.place(caret, false);
                        view.output_pane.delete();
                    }
                }
            }
            view.absorb_output_edits();
            stats.frames += 1;
        }
        Op::Edge(kind) => {
            let last = view.output_pane.line_count().saturating_sub(1);
            let end = u32::try_from(view.output_pane.line_text(last).chars().count()).unwrap();
            let ending = view.terminator();
            match kind {
                0 => {
                    view.output_pane.place(Caret::new(0, 0), false);
                    view.output_pane.type_character('F');
                }
                1 => {
                    view.output_pane.place(Caret::new(0, 0), false);
                    view.output_pane.delete();
                }
                2 => {
                    view.output_pane.place(Caret::new(last, end), false);
                    view.output_pane.backspace();
                }
                3 => {
                    view.output_pane.place(Caret::new(last, end), false);
                    view.output_pane.type_character('E');
                }
                4 => {
                    view.output_pane.place(Caret::new(last, end), false);
                    view.output_pane.enter(ending);
                }
                _ => {
                    view.output_pane.place(Caret::new(0, 0), false);
                    view.output_pane.place(Caret::new(last, end), true);
                    view.output_pane.paste("[q1]");
                }
            }
            view.absorb_output_edits();
            stats.edge_edits += 1;
        }
        Op::ReplaceAll(kind) => {
            let (pattern, replacement, regex) = [
                ("~", "~~", false),
                ("~", "~\\n~", true),
                ("\t", " ", false),
                ("e\u{301}", "E", false),
                ("\\]$", "]x", true),
            ][*kind as usize];
            view.run(Command::Replace);
            let settings = &mut view.find_panel().settings;
            settings.pattern = pattern.to_owned();
            settings.replacement = replacement.to_owned();
            settings.regex = regex;
            view.answer_panel(Some(crate::PanelRequest::ReplaceAll));
            finish_search(view);
            stats.replace_alls += 1;
        }
        Op::Take(section, command, selection) => {
            view.current = (*section).min(view.model().sections().len() - 1);
            if let Some((a, b)) = selection {
                view.output_pane.place(*a, false);
                view.output_pane.place(*b, true);
            }
            view.run(*command);
            stats.takes += 1;
        }
        Op::TakeLine(row, command) => {
            view.row = (*row).min(view.model().rows().len().saturating_sub(1));
            view.run(*command);
            stats.line_takes += 1;
        }
        Op::TakeAll => {
            view.run(Command::TakeAllNonConflicting);
            stats.take_alls += 1;
        }
        Op::ToggleConflict(section) => {
            view.current = (*section).min(view.model().sections().len() - 1);
            if view.accepts(Command::ToggleConflict) {
                view.run(Command::ToggleConflict);
            }
            stats.toggles += 1;
        }
        Op::ToggleIgnored(section) => {
            view.current = (*section).min(view.model().sections().len() - 1);
            view.run(Command::ToggleSectionIgnored);
            stats.toggles += 1;
        }
        Op::Undo => {
            view.run(Command::Undo);
            stats.undos += 1;
        }
        Op::Redo => {
            view.run(Command::Redo);
            stats.redos += 1;
        }
        Op::Reload => {
            view.run(Command::Reload);
            run_until_ready(view);
            stats.reloads += 1;
        }
        Op::ReloadChanged(file, line) => {
            let name = ["left.txt", "center.txt", "right.txt"][*file as usize];
            let path = dir.path().join(name);
            if let Ok(text) = std::fs::read_to_string(&path) {
                let mut lines: Vec<String> = split(&text);
                if !lines.is_empty() {
                    let at = line % lines.len();
                    let ending = if lines[at].ends_with("\r\n") {
                        "\r\n"
                    } else if lines[at].ends_with('\n') {
                        "\n"
                    } else if lines[at].ends_with('\r') {
                        "\r"
                    } else {
                        ""
                    };
                    lines[at] = format!("[d{line}]{ending}");
                    let _ = std::fs::write(&path, lines.concat());
                }
            }
            view.run(Command::Reload);
            run_until_ready(view);
            stats.changed_reloads += 1;
        }
        Op::Swap => {
            view.run(Command::SwapSides);
            run_until_ready(view);
            stats.swaps += 1;
        }
        Op::Rules(command) => {
            view.run(*command);
            stats.rules += 1;
        }
        Op::Save => {
            let conflicts = view.model().totals().conflicts_remaining;
            let expected = if conflicts > 0 {
                expected_markers(view).0
            } else {
                pane_text(view)
            };
            let saved = save_and_read(view, dir);
            stats.saves += 1;
            stats.marker_saves += usize::from(conflicts > 0);
            let code = view.exit_code();
            if saved != expected.as_bytes() {
                return Some(format!(
                    "save mismatch: saved {:?} expected {:?}",
                    String::from_utf8_lossy(&saved),
                    expected
                ));
            }
            if (conflicts > 0) != (code == Some(14)) {
                return Some(format!("exit code {code:?} with {conflicts} conflicts"));
            }
            if view.is_modified() {
                return Some("modified right after a save".to_owned());
            }
        }
    }
    None
}

fn split(text: &str) -> Vec<String> {
    ca_diff::split_lines(text)
        .into_iter()
        .map(str::to_owned)
        .collect()
}

fn summary(view: &MergeView) -> Vec<String> {
    let model = view.model();
    model
        .sections()
        .iter()
        .enumerate()
        .map(|(index, section)| {
            format!(
                "{index}:{:?}:{}:{:?}",
                section.resolution,
                section.is_unresolved_conflict(),
                model.output_range(index)
            )
        })
        .collect()
}

/// A section whose take waits for its lent text, with the resolution and
/// lines that take recorded.
type PendingTake = (usize, Resolution, Vec<String>);

/// Token counts of the input lines of every section but `skip`, on all
/// three sides.
fn input_tokens_outside(model: &crate::model::MergeModel, skip: usize) -> HashMap<String, usize> {
    let inputs = model.inputs();
    let mut map = HashMap::new();
    for (index, section) in model.sections().iter().enumerate() {
        if index == skip {
            continue;
        }
        for (lines, range) in [
            (&inputs.center, &section.center),
            (&inputs.left, &section.left),
            (&inputs.right, &section.right),
        ] {
            for line in lines
                .get(range.start as usize..range.end as usize)
                .unwrap_or(&[])
            {
                for token in tokens(line) {
                    *map.entry(token).or_insert(0) += 1;
                }
            }
        }
    }
    map
}

/// The sections whose pending take a take restored, and the problems of
/// those restores. `pending` holds, from before the take, each section that
/// waited for its lent text with the resolution and lines its take
/// recorded. A restored section holds exactly those lines. Every token of
/// them appears outside the section no more often than sections other than
/// it showed that token before the take, plus the copies the inputs of
/// other sections hold: a holder that keeps the lent text next to the
/// restored lines is a duplicate.
fn restore_problems(
    view: &MergeView,
    op: &str,
    pending: &[PendingTake],
    owned_before: &Owned,
) -> (HashSet<usize>, Vec<(String, String)>) {
    let model = view.model();
    let after = owned_text(view).text;
    let after_counts = counts(&after);
    let before_counts = counts(&owned_before.text);
    let mut restored = HashSet::new();
    let mut problems = Vec::new();
    for (index, resolution, lines) in pending {
        let Some(section) = model.sections().get(*index) else {
            continue;
        };
        if model.pending_take(*index).is_some() || section.resolution != *resolution {
            continue;
        }
        let now = model.section_lines(*index);
        if now != *lines {
            problems.push((
                "take-restore-differs-from-selected-input".to_owned(),
                format!("after {op}: s{index} {now:?} expected {lines:?}; text {after:?}"),
            ));
            continue;
        }
        restored.insert(*index);
        let own_before = kept_tokens(owned_before, &|owner| owner == *index);
        let elsewhere = input_tokens_outside(model, *index);
        for (token, count) in counts(&now.concat()) {
            let outside = after_counts
                .get(&token)
                .copied()
                .unwrap_or(0)
                .saturating_sub(count);
            let allowed = before_counts
                .get(&token)
                .copied()
                .unwrap_or(0)
                .saturating_sub(own_before.get(&token).copied().unwrap_or(0))
                + elsewhere.get(&token).copied().unwrap_or(0);
            if outside > allowed {
                problems.push((
                    "take-restore-duplicated-token".to_owned(),
                    format!(
                        "after {op}: s{index} restored {token} with {outside} other \
                         cop(ies), {allowed} allowed; text {after:?}"
                    ),
                ));
                break;
            }
        }
    }
    (restored, problems)
}

/// The first lender whose lent text one holder alone holds, with that
/// holder.
fn single_holder_lender(view: &MergeView) -> Option<(usize, usize)> {
    view.model()
        .sections()
        .iter()
        .enumerate()
        .find_map(|(lender, section)| {
            let holder = section.lent_records().first()?.holder;
            section
                .lent_records()
                .iter()
                .all(|entry| entry.holder == holder)
                .then_some((lender, holder))
        })
}

/// The output text, the lender's resolution and lines, and the saved bytes
/// after two takes.
#[derive(PartialEq)]
struct TakeOrderAfter {
    text: String,
    lender: Resolution,
    lines: Vec<String>,
    saved: Vec<u8>,
}

impl std::fmt::Debug for TakeOrderAfter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "text {:?} lender {:?} lines {:?} saved {:?}",
            self.text,
            self.lender,
            self.lines,
            String::from_utf8_lossy(&self.saved)
        )
    }
}

struct TakeOrderReplay {
    before: String,
    after: TakeOrderAfter,
}

/// `saved` with the folder of `dir` taken out: conflict markers name the
/// input files by their full paths.
fn without_folder(saved: &[u8], dir: &tempfile::TempDir) -> Vec<u8> {
    let folder = dir.path().display().to_string().into_bytes();
    let mut out = Vec::with_capacity(saved.len());
    let mut at = 0;
    while at < saved.len() {
        if !folder.is_empty() && saved[at..].starts_with(&folder) {
            at += folder.len();
        } else {
            out.push(saved[at]);
            at += 1;
        }
    }
    out
}

/// Run `ops` again on a fresh view of `inputs`, then `takes` in order. The
/// later of the two sections is the lender.
fn replay_with_takes(
    inputs: [&str; 3],
    ops: &[Op],
    paste: usize,
    takes: [(usize, Command); 2],
) -> TakeOrderReplay {
    PASTE.with(|p| p.set(paste));
    let (mut view, dir) = open(inputs[0], Some(inputs[1]), inputs[2]);
    run_until_ready(&mut view);
    let mut stats = Stats::default();
    for op in ops {
        let _ = apply(&mut view, &dir, op, &mut stats);
    }
    let before = pane_text(&view);
    let lender = takes.iter().map(|(section, _)| *section).max().unwrap_or(0);
    for (section, command) in takes {
        view.output_pane.clear_selection();
        view.focus = crate::Pane::Output;
        view.current = section;
        view.run(command);
    }
    let text = pane_text(&view);
    let model = view.model();
    let resolution = model.sections()[lender].resolution;
    let lines = model
        .output_range(lender)
        .map_or_else(Vec::new, |range| split(&model.output_text_range(range)));
    let saved = without_folder(&save_and_read(&mut view, &dir), &dir);
    TakeOrderReplay {
        before,
        after: TakeOrderAfter {
            text,
            lender: resolution,
            lines,
            saved,
        },
    }
}

fn op_name(op: &Op) -> &'static str {
    match op {
        Op::Join(_) => "Join",
        Op::DeleteAtEnd(_) => "DeleteAtEnd",
        Op::SelectEdit(..) => "SelectEdit",
        Op::Split(_) => "Split",
        Op::Type(_) => "Type",
        Op::Frame(_) => "Frame",
        Op::Edge(_) => "Edge",
        Op::ReplaceAll(_) => "ReplaceAll",
        Op::Take(..) => "Take",
        Op::TakeLine(..) => "TakeLine",
        Op::TakeAll => "TakeAll",
        Op::ToggleConflict(_) | Op::ToggleIgnored(_) => "Toggle",
        Op::Undo => "Undo",
        Op::Redo => "Redo",
        Op::Reload => "Reload",
        Op::ReloadChanged(..) => "ReloadChanged",
        Op::Swap => "Swap",
        Op::Rules(_) => "Rules",
        Op::Save => "Save",
    }
}

#[allow(clippy::too_many_lines)]
fn run_case(
    seed: u64,
    case: usize,
    frames: bool,
    gen: &mut Gen,
    stats: &mut Stats,
    findings: &mut BTreeMap<String, Vec<(usize, String)>>,
) {
    let (left, center, right) = gen.inputs();
    let (mut view, dir) = open(&left, Some(&center), &right);
    run_until_ready(&mut view);
    let len = 2 + gen.0.pick(MAX_OPS - 1);
    let mut ops: Vec<Op> = Vec::new();
    let mut baseline = pane_text(&view);
    let initial = baseline.clone();
    let mut reset = false;
    let mut flags_changed = false;
    let paste_start = PASTE.with(std::cell::Cell::get);
    crate::model::ownership::RESYNCS.with(|c| c.set(0));
    crate::model::ownership::REABSORBS.with(|c| c.set(0));
    for counter in CLAMP_COUNTERS {
        counter.with(|c| c.set(0));
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut local: Vec<(String, String)> = Vec::new();
        for _ in 0..len {
            let op = pick_op(&mut gen.0, &view, frames);
            ops.push(op.clone());
            let before = pane_text(&view);
            let before_counts = counts(&before);
            let owned_before = owned_text(&view);
            let sections_before = view.model().sections().len();
            let pending: Vec<PendingTake> = (0..sections_before)
                .filter_map(|index| {
                    let (resolution, lines) = view.model().pending_take(index)?;
                    Some((index, resolution, lines))
                })
                .collect();
            let taken: Option<HashSet<usize>> = match &op {
                Op::Take(section, _, selection) => {
                    let section = (*section).min(sections_before - 1);
                    let mut set: HashSet<usize> = HashSet::new();
                    // The pane moves a caret inside a grapheme to its start,
                    // so the selection the take sees can be empty.
                    let snapped = selection.as_ref().and_then(|(a, b)| {
                        view.output_pane.clear_selection();
                        view.output_pane.place(*a, false);
                        view.output_pane.place(*b, true);
                        let snapped = view.output_pane.selection();
                        view.output_pane.clear_selection();
                        snapped
                    });
                    if let Some((start, end)) = snapped {
                        let model = view.model();
                        let last_line = if end.index == 0 && end.line > start.line {
                            end.line - 1
                        } else {
                            end.line
                        };
                        let first = model.section_of_output_line(start.line).unwrap_or(0);
                        let last = model.section_of_output_line(last_line).unwrap_or(first);
                        set.extend(first..=last.max(first));
                    }
                    if set.is_empty() {
                        set.insert(section);
                    }
                    Some(set)
                }
                _ => None,
            };
            let lines_before: Vec<String> = view.model().output_lines().iter().cloned().collect();
            let line_take_line = match &op {
                Op::TakeLine(row, _) => view
                    .model()
                    .output_line_for_row((*row).min(view.model().rows().len().saturating_sub(1))),
                _ => None,
            };
            let waiting: HashSet<usize> = view
                .model()
                .sections()
                .iter()
                .enumerate()
                .filter(|(_, section)| section.is_unresolved_conflict())
                .map(|(index, _)| index)
                .collect();
            if let Some(problem) = apply(&mut view, &dir, &op, stats) {
                local.push(("save".to_owned(), problem));
            }
            if DUMP.with(std::cell::Cell::get) {
                println!(
                    "OP {op:?}\n   text {:?}\n   sections {:?}",
                    pane_text(&view),
                    summary(&view)
                );
            }
            stats.steps += 1;
            if let Err(problem) = model_matches_pane(&view) {
                local.push((
                    "model-pane".to_owned(),
                    format!("after {}: {problem}", op_name(&op)),
                ));
            }
            let after = pane_text(&view);
            let after_counts = counts(&after);
            let unchanged_text_class = match &op {
                Op::Reload => Some("reload-changed-text"),
                Op::Rules(_) => Some("rules-changed-text"),
                Op::ToggleConflict(_) | Op::ToggleIgnored(_) => Some("toggle-changed-text"),
                Op::Save => Some("save-changed-text"),
                Op::Swap => Some("swap-changed-text"),
                _ => None,
            };
            if let Some(class) = unchanged_text_class {
                if after != before {
                    local.push((
                        class.to_owned(),
                        format!("before {before:?} after {after:?}"),
                    ));
                }
            }
            if matches!(op, Op::Reload | Op::Swap | Op::ReloadChanged(..)) {
                reset = true;
                baseline = after.clone();
            }
            if matches!(
                op,
                Op::ToggleConflict(_) | Op::ToggleIgnored(_) | Op::Rules(_)
            ) {
                flags_changed = true;
            }
            let owned_after = owned_text(&view);
            if let Some(problem) = owned_after.problems.first() {
                local.push((
                    "lent-record-off-text".to_owned(),
                    format!("after {}: {problem}", op_name(&op)),
                ));
            }
            if let Some(problem) = view.model().lent_record_problems().first() {
                local.push((
                    "lent-record-invariant".to_owned(),
                    format!("after {}: {problem}", op_name(&op)),
                ));
            }
            if matches!(
                op,
                Op::Reload | Op::Rules(_) | Op::ToggleConflict(_) | Op::ToggleIgnored(_) | Op::Save
            ) && view.model().sections().len() == sections_before
            {
                let old = section_chars(&owned_before, sections_before);
                let new = section_chars(&owned_after, sections_before);
                if let Some(index) = (0..sections_before).find(|index| old[*index] != new[*index]) {
                    local.push((
                        "ownership-changed-without-edit".to_owned(),
                        format!(
                            "after {}: s{index} {:?} -> {:?}; text {after:?}",
                            op_name(&op),
                            old[index],
                            new[index]
                        ),
                    ));
                }
            }
            // An edit that changes a character of a section drops the
            // restore its take recorded.
            if matches!(
                op,
                Op::Join(_)
                    | Op::DeleteAtEnd(_)
                    | Op::SelectEdit(..)
                    | Op::Split(_)
                    | Op::Type(_)
                    | Op::Frame(_)
                    | Op::Edge(_)
                    | Op::ReplaceAll(_)
            ) && view.model().sections().len() == sections_before
            {
                let old = section_chars(&owned_before, sections_before);
                let new = section_chars(&owned_after, sections_before);
                if let Some((index, _, _)) = pending.iter().find(|(index, _, _)| {
                    old[*index] != new[*index] && view.model().pending_take(*index).is_some()
                }) {
                    local.push((
                        "edit-kept-take-restore".to_owned(),
                        format!(
                            "after {}: s{index} {:?} -> {:?}; text {after:?}",
                            op_name(&op),
                            old[*index],
                            new[*index]
                        ),
                    ));
                }
            }
            let mut restored: HashSet<usize> = HashSet::new();
            if matches!(op, Op::Take(..) | Op::TakeLine(..) | Op::TakeAll)
                && view.model().sections().len() == sections_before
            {
                let problems;
                (restored, problems) =
                    restore_problems(&view, op_name(&op), &pending, &owned_before);
                stats.pending_takes_restored += restored.len();
                local.extend(problems);
            }
            if let Some(taken) = &taken {
                // A restored section changed to exactly its selected input's
                // lines, with no copy of them left elsewhere.
                let untaken = |owner: usize| !taken.contains(&owner) && !restored.contains(&owner);
                for (token, count) in kept_tokens(&owned_before, &untaken) {
                    if after_counts.get(&token).copied().unwrap_or(0) < count {
                        local.push((
                            "take-lost-other-section-token".to_owned(),
                            format!("{token} x{count}; before {before:?} after {after:?}"),
                        ));
                    }
                }
                let chars_before = section_chars(&owned_before, sections_before);
                let chars_after = section_chars(&owned_after, sections_before);
                // A token whose section kept its characters was spelled by
                // removing other text between them; no character came back.
                let mut grown: HashMap<String, usize> = HashMap::new();
                for (token, start, end) in token_spans(&owned_after.text) {
                    if let Some(owner) = single_owner(&owned_after.owners[start..end]) {
                        if untaken(owner) && chars_before.get(owner) != chars_after.get(owner) {
                            *grown.entry(token).or_insert(0) += 1;
                        }
                    }
                }
                for (token, count) in grown {
                    let had = before_counts.get(&token).copied().unwrap_or(0);
                    if had < count {
                        let class = if had == 0 {
                            "take-resurrected-other-token"
                        } else {
                            "take-duplicated-other-token"
                        };
                        local.push((
                            class.to_owned(),
                            format!("{token}; before {before:?} after {after:?}"),
                        ));
                    }
                }
                for (index, (old, new)) in chars_before.iter().zip(&chars_after).enumerate() {
                    if !untaken(index) || old == new {
                        continue;
                    }
                    let class = if sorted_chars(old) == sorted_chars(new) {
                        "take-reordered-other-section-text"
                    } else {
                        "take-changed-other-section-text"
                    };
                    local.push((
                        class.to_owned(),
                        format!("s{index} {old:?} -> {new:?}; before {before:?} after {after:?}"),
                    ));
                    break;
                }
            }
            if let Some(line) = line_take_line {
                let replaced: HashSet<String> = lines_before
                    .get(line as usize)
                    .map(|text| tokens(text).into_iter().collect())
                    .unwrap_or_default();
                for token in before_counts.keys() {
                    if replaced.contains(token) {
                        continue;
                    }
                    if after_counts.get(token).copied().unwrap_or(0) == 0 {
                        local.push((
                            "line-take-lost-token".to_owned(),
                            format!("{token}; before {before:?} after {after:?}"),
                        ));
                    }
                }
            }
            if matches!(op, Op::TakeAll) {
                let waits = |owner: usize| waiting.contains(&owner);
                for (token, count) in kept_tokens(&owned_before, &waits) {
                    if after_counts.get(&token).copied().unwrap_or(0) < count {
                        local.push((
                            "take-all-lost-conflict-token".to_owned(),
                            format!("{token} x{count}; before {before:?} after {after:?}"),
                        ));
                    }
                }
                let chars_before = section_chars(&owned_before, sections_before);
                let chars_after = section_chars(&owned_after, sections_before);
                for index in &waiting {
                    if chars_before.get(*index) != chars_after.get(*index) {
                        local.push((
                            "take-all-changed-conflict-text".to_owned(),
                            format!("s{index}; before {before:?} after {after:?}"),
                        ));
                        break;
                    }
                }
                for (token, count) in &after_counts {
                    let had = before_counts.get(token).copied().unwrap_or(0);
                    if had >= 1 && *count > had {
                        local.push((
                            "take-all-duplicated-token".to_owned(),
                            format!("{token}; before {before:?} after {after:?}"),
                        ));
                    }
                }
            }
            let owners_now = token_owner(&view);
            let mut visited = HashSet::new();
            let mut last_rank = None;
            for token in tokens(&after) {
                if !visited.insert(token.clone()) {
                    continue;
                }
                if let Some(&(_, Some(rank))) = owners_now.get(&token) {
                    if last_rank.is_some_and(|last| rank < last) {
                        local.push((
                            "unchanged-order".to_owned(),
                            format!("{token} after {op:?}: {after:?}"),
                        ));
                        break;
                    }
                    last_rank = Some(rank);
                }
            }
        }
        if let Some((lender, holder)) = single_holder_lender(&view) {
            let takes = [
                (
                    lender,
                    [
                        Command::TakeLeft,
                        Command::TakeCenter,
                        Command::TakeRight,
                        Command::TakeLeftThenRight,
                    ][case % 4],
                ),
                (
                    holder,
                    [Command::TakeCenter, Command::TakeRight, Command::TakeLeft][case % 3],
                ),
            ];
            let inputs = [left.as_str(), center.as_str(), right.as_str()];
            let paste_end = PASTE.with(std::cell::Cell::get);
            let lender_first = replay_with_takes(inputs, &ops, paste_start, takes);
            let holder_first = replay_with_takes(inputs, &ops, paste_start, [takes[1], takes[0]]);
            PASTE.with(|p| p.set(paste_end));
            let end = pane_text(&view);
            if lender_first.before != end || holder_first.before != end {
                local.push((
                    "take-order-replay-differs".to_owned(),
                    format!("end {end:?} replays {:?}", lender_first.before),
                ));
            } else {
                stats.take_order_checks += 1;
                if lender_first.after != holder_first.after {
                    local.push((
                        "take-order-differs".to_owned(),
                        format!(
                            "s{lender} held by s{holder}, takes {takes:?}: lender first {:?} \
                             holder first {:?}",
                            lender_first.after, holder_first.after
                        ),
                    ));
                }
            }
        }
        for _ in 0..(4 * len + 8) {
            view.run(Command::Redo);
            if let Err(problem) = model_matches_pane(&view) {
                local.push(("redo-to-top-model-pane".to_owned(), problem));
                break;
            }
            if let Some(problem) = view.model().lent_record_problems().first() {
                local.push((
                    "lent-record-invariant".to_owned(),
                    format!("during the redo-to-top-model-pane pass: {problem}"),
                ));
                break;
            }
        }
        let end_text = pane_text(&view);
        let mut undo_texts = vec![end_text.clone()];
        for _ in 0..(4 * len + 8) {
            view.run(Command::Undo);
            if let Err(problem) = model_matches_pane(&view) {
                local.push(("undo-model-pane".to_owned(), problem));
                break;
            }
            if let Some(problem) = view.model().lent_record_problems().first() {
                local.push((
                    "lent-record-invariant".to_owned(),
                    format!("during the undo-model-pane pass: {problem}"),
                ));
                break;
            }
            let text = pane_text(&view);
            if &text != undo_texts.last().unwrap() {
                undo_texts.push(text);
            }
        }
        let bottom = pane_text(&view);
        if bottom != baseline {
            local.push((
                "undo-all-not-baseline".to_owned(),
                format!("bottom {bottom:?} baseline {baseline:?}"),
            ));
        }
        let mut redo_texts = vec![pane_text(&view)];
        for _ in 0..(4 * len + 8) {
            view.run(Command::Redo);
            if let Err(problem) = model_matches_pane(&view) {
                local.push(("redo-model-pane".to_owned(), problem));
                break;
            }
            if let Some(problem) = view.model().lent_record_problems().first() {
                local.push((
                    "lent-record-invariant".to_owned(),
                    format!("during the redo-model-pane pass: {problem}"),
                ));
                break;
            }
            let text = pane_text(&view);
            if &text != redo_texts.last().unwrap() {
                redo_texts.push(text);
            }
        }
        let top = pane_text(&view);
        if top != end_text {
            local.push((
                "redo-all-not-end".to_owned(),
                format!("top {top:?} end {end_text:?}"),
            ));
        }
        let mut reversed = undo_texts.clone();
        reversed.reverse();
        if reversed != redo_texts {
            local.push((
                "redo-path-differs-from-undo-path".to_owned(),
                format!("undo {undo_texts:?} redo {redo_texts:?}"),
            ));
        }
        for _ in 0..(4 * len + 8) {
            view.run(Command::Undo);
        }
        let bottom = pane_text(&view);
        if !reset && !flags_changed && bottom == initial {
            let (mut fresh, _fresh_dir) = open(&left, Some(&center), &right);
            run_until_ready(&mut fresh);
            if summary(&fresh) != summary(&view) {
                local.push((
                    "undo-all-state-differs-from-fresh".to_owned(),
                    format!("fresh {:?} undone {:?}", summary(&fresh), summary(&view)),
                ));
            }
            let count = view
                .model()
                .sections()
                .len()
                .min(fresh.model().sections().len());
            for section in (0..count).rev() {
                for target in [&mut view, &mut fresh] {
                    target.output_pane.clear_selection();
                    target.current = section;
                    target.run(Command::TakeLeft);
                }
                if pane_text(&view) != pane_text(&fresh) {
                    local.push((
                        "take-after-undo-all-differs-from-fresh".to_owned(),
                        format!(
                            "section {section}: undone {:?} fresh {:?}",
                            pane_text(&view),
                            pane_text(&fresh)
                        ),
                    ));
                    break;
                }
            }
        }
        local
    }));
    stats.cases += 1;
    stats.resyncs += crate::model::ownership::RESYNCS.with(std::cell::Cell::get);
    stats.reabsorbs += crate::model::ownership::REABSORBS.with(std::cell::Cell::get);
    for counter in CLAMP_COUNTERS {
        stats.clamps += counter.with(std::cell::Cell::get);
    }
    let context = format!(
        "seed {seed:#x} case {case} inputs L={left:?} C={center:?} R={right:?} ops={ops:?}"
    );
    let weight = ops.len();
    match result {
        Ok(local) => {
            let mut reported = HashSet::new();
            for (class, detail) in local {
                if reported.insert(class.clone()) {
                    findings
                        .entry(class)
                        .or_default()
                        .push((weight, format!("{context}\n      {detail}")));
                }
            }
        }
        Err(failure) => {
            stats.panics += 1;
            let message = failure
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| failure.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                .unwrap_or_default();
            findings
                .entry("panic".to_owned())
                .or_default()
                .push((weight, format!("{context}\n      {message}")));
        }
    }
}

fn run(frames: bool) {
    let mut stats = Stats::default();
    let mut findings: BTreeMap<String, Vec<(usize, String)>> = BTreeMap::new();
    let mut empty = 0;
    for seed in SEEDS {
        let mut gen = Gen(Seeded(seed), 0);
        for case in 0..CASES {
            run_case(seed, case, frames, &mut gen, &mut stats, &mut findings);
        }
        empty += gen.1;
    }
    stats.empty_inputs = empty;
    println!("STATS frames={frames} {stats:?}");
    for (class, list) in &findings {
        let asserted = if ASSERTED.contains(&class.as_str()) {
            "asserted"
        } else {
            "reported"
        };
        println!("CLASS {class} ({asserted}): {} case(s)", list.len());
        for set in ["seed 0x1", "seed 0x3"] {
            let in_set = list
                .iter()
                .filter(|(_, text)| text.starts_with(set))
                .count();
            println!("  SET {set}: {in_set} case(s)");
        }
        let mut shortest: Vec<&(usize, String)> = list.iter().collect();
        shortest.sort_by_key(|(ops, text)| (*ops, text.len()));
        for (_, example) in shortest.iter().take(3) {
            println!("  EXAMPLE {example}");
        }
    }
    let failing: Vec<(&String, usize)> = findings
        .iter()
        .filter(|(class, _)| ASSERTED.contains(&class.as_str()))
        .map(|(class, list)| (class, list.len()))
        .collect();
    assert_eq!(stats.cases, SEEDS.len() * CASES);
    assert_eq!(
        (stats.panics, stats.resyncs, stats.reabsorbs, stats.clamps),
        (0, 0, 0, 0)
    );
    assert!(
        failing.is_empty(),
        "invariant classes with cases: {failing:?}"
    );
}

#[test]
fn randomized_merge_edits_with_one_edit_per_fold_keep_every_section() {
    run(false);
}

#[test]
fn randomized_merge_edits_with_several_edits_per_fold_keep_every_section() {
    run(true);
}

fn replay(seed: u64, target: usize, frames: bool) {
    let mut stats = Stats::default();
    let mut findings: BTreeMap<String, Vec<(usize, String)>> = BTreeMap::new();
    let mut gen = Gen(Seeded(seed), 0);
    for case in 0..=target {
        DUMP.with(|d| d.set(case == target));
        run_case(seed, case, frames, &mut gen, &mut stats, &mut findings);
    }
    DUMP.with(|d| d.set(false));
    for (class, list) in &findings {
        if list
            .iter()
            .any(|(_, text)| text.contains(&format!("case {target} ")))
        {
            for (_, text) in list
                .iter()
                .filter(|(_, text)| text.contains(&format!("case {target} ")))
            {
                println!("REPLAY CLASS {class}: {text}");
            }
        }
    }
}

#[test]
fn replay_one_randomized_merge_case() {
    replay(0x1007, 31, false);
}

const TOKEN_INPUTS: [&str; 3] = [
    "[a]\n[L]\n[c]\n[d]\n[g]\n",
    "[a]\n[b]\n[c]\n[d]\n[g]\n",
    "[a]\n[R]\n[c]\n[d]\n[g]\n",
];

/// Join line 1 twice, so s1 holds the `[c][d]` of s2, then take s2 from
/// the center. Returns the view with s2's take pending and the state the
/// oracle needs from before the holder take.
fn lender_taken_while_lent() -> (MergeView, tempfile::TempDir, Vec<PendingTake>, Owned) {
    let (mut view, dir) = open(TOKEN_INPUTS[0], Some(TOKEN_INPUTS[1]), TOKEN_INPUTS[2]);
    run_until_ready(&mut view);
    for _ in 0..2 {
        view.output_pane.place(Caret::new(1, 0), false);
        view.output_pane
            .move_caret(ca_ui::editor::Motion::LineEnd, false);
        view.output_pane.delete();
        view.absorb_output_edits();
    }
    assert_eq!(pane_text(&view), "[a]\n[b][c][d]\n[g]\n");
    view.current = 2;
    view.run(Command::TakeCenter);
    let pending: Vec<PendingTake> = (0..view.model().sections().len())
        .filter_map(|index| {
            let (resolution, lines) = view.model().pending_take(index)?;
            Some((index, resolution, lines))
        })
        .collect();
    assert_eq!(
        pending,
        vec![(
            2,
            Resolution::Center,
            vec!["[c]\n".to_owned(), "[d]\n".to_owned(), "[g]\n".to_owned()]
        )]
    );
    let owned = owned_text(&view);
    view.current = 1;
    view.run(Command::TakeLeft);
    (view, dir, pending, owned)
}

#[test]
fn a_take_restore_with_the_selected_input_and_no_other_copy_passes_the_oracle() {
    let (view, _dir, pending, owned) = lender_taken_while_lent();
    assert_eq!(pane_text(&view), "[a]\n[L]\n[c]\n[d]\n[g]\n");
    let (restored, problems) = restore_problems(&view, "Take", &pending, &owned);
    assert_eq!(restored, HashSet::from([2]));
    assert_eq!(problems, Vec::<(String, String)>::new());
}

/// A holder that keeps the lent text while its lender takes the selected
/// input back shows that text twice; the oracle reports it although both
/// sections are exempt from the checks for untaken sections.
#[test]
fn a_take_restore_that_leaves_the_lent_text_in_the_holder_is_a_finding() {
    let (mut view, _dir, pending, owned) = lender_taken_while_lent();
    view.data
        .model
        .set_edited(1, vec!["[L][c][d]\n".to_owned()]);
    let (restored, problems) = restore_problems(&view, "Take", &pending, &owned);
    assert_eq!(restored, HashSet::from([2]));
    let classes: Vec<&str> = problems.iter().map(|(class, _)| class.as_str()).collect();
    assert_eq!(classes, ["take-restore-duplicated-token"], "{problems:?}");
}

/// A restore that misses a line of the selected input is not exempt.
#[test]
fn a_take_restore_without_a_line_of_the_selected_input_is_a_finding() {
    let (view, _dir, mut pending, owned) = lender_taken_while_lent();
    pending[0].2.insert(1, "[x]\n".to_owned());
    let (restored, problems) = restore_problems(&view, "Take", &pending, &owned);
    assert!(restored.is_empty());
    let classes: Vec<&str> = problems.iter().map(|(class, _)| class.as_str()).collect();
    assert_eq!(
        classes,
        ["take-restore-differs-from-selected-input"],
        "{problems:?}"
    );
}
