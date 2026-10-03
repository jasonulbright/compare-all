use super::*;
use ca_ui::editor::Caret;
use std::collections::{BTreeMap, HashMap, HashSet};

const SEEDS: [u64; 10] = [
    0x1001, 0x1002, 0x1003, 0x1004, 0x1005, 0x1006, 0x1007, 0x1008, 0x1009, 0x100a,
];
const CASES: usize = 600;
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
];

fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        let Some(close) = after.find(']') else { break };
        let inner = &after[..close];
        if let Some(nested) = inner.rfind('[') {
            rest = &after[nested..];
            continue;
        }
        if !inner.is_empty() && !inner.contains(['\r', '\n']) {
            out.push(format!("[{inner}]"));
        }
        rest = &after[close..];
    }
    out
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
    panics: usize,
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
    crate::model::ownership::RESYNCS.with(|c| c.set(0));
    crate::model::ownership::REABSORBS.with(|c| c.set(0));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut local: Vec<(String, String)> = Vec::new();
        for _ in 0..len {
            let op = pick_op(&mut gen.0, &view, frames);
            ops.push(op.clone());
            let before = pane_text(&view);
            let before_counts = counts(&before);
            let owners = token_owner(&view);
            let sections_before = view.model().sections().len();
            let taken: Option<HashSet<usize>> = match &op {
                Op::Take(section, _, selection) => {
                    let section = (*section).min(sections_before - 1);
                    let mut set: HashSet<usize> = HashSet::new();
                    if let Some((a, b)) = selection {
                        let (start, end) = if a <= b { (a, b) } else { (b, a) };
                        if start != end {
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
            if let Some(taken) = &taken {
                for (token, count) in &before_counts {
                    let Some(&(owner, _)) = owners.get(token) else {
                        continue;
                    };
                    if taken.contains(&owner) || *count == 0 {
                        continue;
                    }
                    if after_counts.get(token).copied().unwrap_or(0) == 0 {
                        local.push((
                            "take-lost-other-section-token".to_owned(),
                            format!("{token} of s{owner}; before {before:?} after {after:?}"),
                        ));
                    }
                }
                for (token, count) in &after_counts {
                    let Some(&(owner, _)) = owners.get(token) else {
                        continue;
                    };
                    if taken.contains(&owner) {
                        continue;
                    }
                    let had = before_counts.get(token).copied().unwrap_or(0);
                    if had < *count {
                        let class = if had == 0 {
                            "take-resurrected-other-token"
                        } else {
                            "take-duplicated-other-token"
                        };
                        local.push((
                            class.to_owned(),
                            format!("{token} of s{owner}; before {before:?} after {after:?}"),
                        ));
                    }
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
                for token in before_counts.keys() {
                    let Some(&(owner, _)) = owners.get(token) else {
                        continue;
                    };
                    if waiting.contains(&owner)
                        && after_counts.get(token).copied().unwrap_or(0) == 0
                    {
                        local.push((
                            "take-all-lost-conflict-token".to_owned(),
                            format!("{token} of s{owner}; before {before:?} after {after:?}"),
                        ));
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
        for _ in 0..(4 * len + 8) {
            view.run(Command::Redo);
            if let Err(problem) = model_matches_pane(&view) {
                local.push(("redo-to-top-model-pane".to_owned(), problem));
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
            println!("REPLAY CLASS {class}");
        }
    }
}

#[test]
fn replay_one_randomized_merge_case() {
    replay(0x1007, 31, false);
}
