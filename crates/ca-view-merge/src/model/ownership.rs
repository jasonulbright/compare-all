//! Which section owns each output character after a pane edit, and the
//! text one section lends to a line of another.
//!
//! Sections own whole lines. An edit that removes the break between the last
//! line of one section and the first line of a later one leaves one line
//! with characters of both. The line belongs to the earlier section, the
//! holder; the later section, the lender, keeps a record of its characters
//! on that line. A take that replaces the holder's line gives those
//! characters back to the lender. A take of the lender leaves them out while
//! the holder still shows them, when its new text holds them; otherwise the
//! take removes them from the holder's line. No take loses or repeats a line
//! of another section. Characters an edit removes are gone for every section.

use super::{automatic_len, MergeModel, Resolution, Section};
use ca_text::AppliedEdit;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

/// Characters of one owner, in output order. `place` is the section whose
/// lines hold them: the owner, or the holder of lent text.
#[derive(Debug, Clone)]
struct Run {
    owner: usize,
    place: usize,
    text: String,
}

/// Characters of an inserted text on one line, as `(local line, column,
/// text)`.
type Piece = (u32, u32, String);

/// Text a pane edit leaves on a line of an earlier section. `fresh` holds
/// the characters that sat on the lender's own lines before the edit.
struct NewRecord {
    lender: usize,
    holder: usize,
    position: usize,
    column: u32,
    text: String,
    fresh: String,
}

fn push_run(runs: &mut Vec<Run>, owner: usize, place: usize, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = runs.last_mut() {
        if last.owner == owner && last.place == place {
            last.text.push_str(text);
            return;
        }
    }
    runs.push(Run {
        owner,
        place,
        text: text.to_owned(),
    });
}

/// The byte index of character `chars` of `text`, or its length.
fn byte_at(text: &str, chars: usize) -> usize {
    text.char_indices()
        .nth(chars)
        .map_or(text.len(), |(index, _)| index)
}

/// Where a deletion of `removed` characters at `offset` of `text` starts
/// when it is moved to a line start. A deletion leaves the same text at
/// every offset it can slide to over equal characters; one that starts at a
/// line start removes whole lines and keeps the characters of each kept
/// line with that line's section, rather than joining the head of one line
/// to the tail of another. Without such an offset, `offset` itself.
fn slide_to_line_start(text: &str, line_starts: &[usize], offset: usize, removed: usize) -> usize {
    if line_starts.contains(&offset) {
        return offset;
    }
    let chars: Vec<char> = text.chars().collect();
    let mut left = offset;
    while left > 0
        && chars.get(left - 1).is_some()
        && chars.get(left - 1) == chars.get(left - 1 + removed)
    {
        left -= 1;
        if line_starts.contains(&left) {
            break;
        }
    }
    let mut right = offset;
    while right + removed < chars.len() && chars.get(right) == chars.get(right + removed) {
        right += 1;
        if line_starts.contains(&right) {
            break;
        }
    }
    let left_fits = line_starts.contains(&left);
    let right_fits = line_starts.contains(&right);
    match (left_fits, right_fits) {
        (true, true) if offset - left <= right - offset => left,
        (_, true) => right,
        (true, false) => left,
        (false, false) => offset,
    }
}

fn is_terminator_only(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c == '\r' || c == '\n')
}

/// Remove `prefix` from the start of the text `lines` hold. Returns false,
/// leaving `lines` alone, when the text does not start with it.
pub(super) fn remove_prefix(lines: &mut Vec<String>, prefix: &str) -> bool {
    let mut rest = prefix;
    let mut whole = 0;
    while !rest.is_empty() {
        let Some(line) = lines.get(whole) else {
            return false;
        };
        if let Some(after) = rest.strip_prefix(line.as_str()) {
            rest = after;
            whole += 1;
        } else if line.starts_with(rest) {
            let cut = rest.len();
            lines.drain(..whole);
            if let Some(first) = lines.first_mut() {
                first.replace_range(..cut, "");
            }
            if lines.first().is_some_and(String::is_empty) {
                lines.remove(0);
            }
            return true;
        } else {
            return false;
        }
    }
    lines.drain(..whole);
    true
}

#[cfg(test)]
thread_local! {
    pub(crate) static RESYNCS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(crate) static REABSORBS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(crate) static PANE_EDITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(crate) static FILTER_PASSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl MergeModel {
    /// Every way the lent records break their invariants: each record sits
    /// on a line of an earlier section, holds characters that line shows at
    /// its column and overlaps no other record; order numbers are unique and
    /// issued; the oldest record of a lender carries a source; and each
    /// section names the last lender it holds text of.
    #[cfg(test)]
    pub(crate) fn lent_record_problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        let mut spans: BTreeMap<usize, Vec<(usize, usize, usize)>> = BTreeMap::new();
        let mut last_lender: BTreeMap<usize, usize> = BTreeMap::new();
        for (lender, section) in self.sections.iter().enumerate() {
            let mut seqs = BTreeSet::new();
            for entry in &section.lent {
                if !seqs.insert(entry.seq) || entry.seq > self.lend_seq {
                    problems.push(format!("s{lender} order number {} reused", entry.seq));
                }
                if entry.holder >= lender {
                    problems.push(format!("s{lender} lends to s{}", entry.holder));
                    continue;
                }
                let Some(range) = self.output_range(entry.holder) else {
                    problems.push(format!("s{lender} lends to missing s{}", entry.holder));
                    continue;
                };
                last_lender
                    .entry(entry.holder)
                    .and_modify(|last| *last = (*last).max(lender))
                    .or_insert(lender);
                let line = (range.start + entry.offset) as usize;
                let count = entry.text.chars().count();
                let here: String = self
                    .output
                    .get(line)
                    .map(|text| {
                        text.chars()
                            .skip(entry.column as usize)
                            .take(count)
                            .collect()
                    })
                    .unwrap_or_default();
                if entry.offset >= range.end - range.start || count == 0 || here != entry.text {
                    problems.push(format!(
                        "s{lender} record {:?} on s{} line {line} column {} finds {here:?}",
                        entry.text, entry.holder, entry.column
                    ));
                    continue;
                }
                let start = entry.column as usize;
                spans
                    .entry(line)
                    .or_default()
                    .push((start, start + count, lender));
            }
            let oldest = section.lent.iter().min_by_key(|entry| entry.seq);
            if oldest.is_some_and(|entry| entry.source.is_none()) {
                problems.push(format!("s{lender} oldest record has no source"));
            }
        }
        for (line, mut list) in spans {
            list.sort_unstable();
            for pair in list.windows(2) {
                if pair[1].0 < pair[0].1 {
                    problems.push(format!(
                        "line {line}: records of s{} and s{} overlap",
                        pair[0].2, pair[1].2
                    ));
                }
            }
        }
        for (index, section) in self.sections.iter().enumerate() {
            let expected = last_lender.get(&index).copied();
            if section.joined_through != expected {
                problems.push(format!(
                    "s{index} names s{:?} as its last lender, holds text of s{expected:?}",
                    section.joined_through
                ));
            }
        }
        problems
    }

    /// The lent records `holder`'s lines hold, as `(lender, seq)`.
    fn held_by(&self, holder: usize) -> Vec<(usize, u64)> {
        let Some(through) = self.sections.get(holder).and_then(|s| s.joined_through) else {
            return Vec::new();
        };
        let mut held = Vec::new();
        for lender in holder + 1..=through.min(self.sections.len().saturating_sub(1)) {
            for entry in &self.sections[lender].lent {
                if entry.holder == holder {
                    held.push((lender, entry.seq));
                }
            }
        }
        held
    }

    fn lent_record(&self, lender: usize, seq: u64) -> Option<&super::Lent> {
        self.sections
            .get(lender)?
            .lent
            .iter()
            .find(|entry| entry.seq == seq)
    }

    /// Remove one lent record. With `lost`, its text never comes back, so
    /// the lender can no longer return to its earlier state.
    fn remove_lent(&mut self, lender: usize, seq: u64, lost: bool) -> Option<super::Lent> {
        self.touch(lender);
        let section = self.sections.get_mut(lender)?;
        let position = section.lent.iter().position(|entry| entry.seq == seq)?;
        let entry = section.lent.remove(position);
        if lost {
            section.restore = None;
            section.restore_from_take = false;
        }
        let holder = entry.holder;
        self.refresh_joined_through(holder);
        Some(entry)
    }

    /// After `removed` left `lender`: the records whose characters its
    /// source carried now stand for their own text.
    fn uncover_after(&mut self, lender: usize, removed: &super::Lent) {
        if removed.source.is_none() {
            return;
        }
        let Some(section) = self.sections.get(lender) else {
            return;
        };
        let mut later: Vec<(u64, bool)> = section
            .lent
            .iter()
            .filter(|entry| entry.seq > removed.seq)
            .map(|entry| (entry.seq, entry.source.is_none()))
            .collect();
        later.sort_unstable();
        let orphans: Vec<u64> = later
            .iter()
            .take_while(|(_, covered)| *covered)
            .map(|(seq, _)| *seq)
            .collect();
        if orphans.is_empty() {
            return;
        }
        self.touch(lender);
        for entry in &mut self.sections[lender].lent {
            if orphans.contains(&entry.seq) {
                entry.source = Some(entry.text.clone());
            }
        }
    }

    /// A record no older record covers stands for its own text.
    pub(super) fn settle_sources(&mut self, lender: usize) {
        let Some(section) = self.sections.get(lender) else {
            return;
        };
        let oldest_covered = section
            .lent
            .iter()
            .min_by_key(|entry| entry.seq)
            .is_some_and(|entry| entry.source.is_none());
        if !oldest_covered {
            return;
        }
        let mut order: Vec<(u64, bool)> = section
            .lent
            .iter()
            .map(|entry| (entry.seq, entry.source.is_none()))
            .collect();
        order.sort_unstable();
        let orphans: Vec<u64> = order
            .iter()
            .take_while(|(_, covered)| *covered)
            .map(|(seq, _)| *seq)
            .collect();
        self.touch(lender);
        for entry in &mut self.sections[lender].lent {
            if orphans.contains(&entry.seq) {
                entry.source = Some(entry.text.clone());
            }
        }
    }

    /// Remove a record and its text from the holder's line, for a lender
    /// whose new text does not hold that text. Other records on the line
    /// keep their characters. The holder keeps its wait for review.
    fn strip_lent(&mut self, lender: usize, seq: u64) {
        let Some(record) = self.lent_record(lender, seq).cloned() else {
            return;
        };
        let holder = record.holder;
        let Some(range) = self.output_range(holder) else {
            let _ = self.remove_lent(lender, seq, true);
            return;
        };
        let line = (range.start + record.offset) as usize;
        let text = self.output.get(line).cloned().unwrap_or_default();
        let repair = self.output_repairs.get(line).copied().unwrap_or(0);
        let column = record.column as usize;
        let start = byte_at(&text, column);
        let end = byte_at(&text, column + record.text.chars().count());
        if record.offset >= range.end - range.start || text.get(start..end) != Some(&record.text) {
            if let Some(removed) = self.remove_lent(lender, seq, true) {
                self.uncover_after(lender, &removed);
            }
            return;
        }
        // A separator the composition added stays a separator: the seam
        // repair below adds it again when the line still needs one.
        let own_end = text.len().saturating_sub(repair);
        let stripped = if end <= own_end {
            format!("{}{}", &text[..start], &text[end..own_end])
        } else {
            format!("{}{}", &text[..start], &text[end..])
        };
        let removed_chars = record.text.chars().count();
        let mut others = Vec::new();
        for (other, other_seq, _, _) in self.records_on_line(holder, line) {
            if (other, other_seq) == (lender, seq) {
                continue;
            }
            if let Some(mut entry) = self.remove_lent(other, other_seq, false) {
                if entry.column as usize > column {
                    entry.column -= u32::try_from(removed_chars).unwrap_or(entry.column);
                }
                others.push((other, entry));
            }
        }
        if let Some(removed) = self.remove_lent(lender, seq, false) {
            self.uncover_after(lender, &removed);
        }
        let conflict = self.sections[holder].conflict;
        let replacement = if stripped.is_empty() {
            Vec::new()
        } else {
            vec![stripped]
        };
        let local = record.offset;
        if !self.edit_output_range(holder, local..local + 1, replacement) {
            return;
        }
        self.sections[holder].conflict = conflict;
        self.refresh_section_totals(holder);
        for (other, entry) in others {
            self.add_lent(other, entry);
        }
        self.repair_output_seams(line..line + 1);
    }

    /// Make the separator a composition added to output line `line` part of
    /// that line's section once the line after it no longer calls for that
    /// separator. The pane holds the separator as text, and a later
    /// composition, a reload for one, would otherwise drop it.
    fn claim_stale_repair(&mut self, line: usize) {
        let repair = self.output_repairs.get(line).copied().unwrap_or(0);
        if repair == 0 {
            return;
        }
        let Some(text) = self.output.get(line).cloned() else {
            return;
        };
        let bare = &text[..text.len().saturating_sub(repair)];
        let ending = self
            .output_ending
            .unwrap_or_else(ca_text::EolStyle::platform)
            .as_str();
        let next = self.output.get(line + 1);
        let mut needed = String::new();
        if next.is_some() && !bare.ends_with(['\r', '\n']) {
            needed.push_str(ending);
        }
        if format!("{bare}{needed}").ends_with('\r') && next.is_some_and(|n| n.starts_with('\n')) {
            needed.push('\n');
        }
        if text[bare.len()..] == needed {
            return;
        }
        let Ok(at) = u32::try_from(line) else {
            return;
        };
        let Some(owner) = self.section_of_output_line(at) else {
            return;
        };
        let Some(range) = self.output_range(owner) else {
            return;
        };
        let local = at - range.start;
        let held: Vec<(usize, super::Lent)> = self
            .records_on_line(owner, line)
            .into_iter()
            .filter_map(|(lender, seq, _, _)| {
                self.remove_lent(lender, seq, false)
                    .map(|entry| (lender, entry))
            })
            .collect();
        let conflict = self.sections[owner].conflict;
        self.touch(owner);
        self.sections[owner].restore = None;
        self.sections[owner].restore_from_take = false;
        if self.edit_output_range(owner, local..local + 1, vec![text]) {
            self.sections[owner].conflict = conflict;
            self.refresh_section_totals(owner);
        }
        for (lender, entry) in held {
            self.add_lent(lender, entry);
        }
    }

    fn add_lent(&mut self, lender: usize, entry: super::Lent) {
        self.touch(lender);
        let holder = entry.holder;
        if let Some(section) = self.sections.get_mut(lender) {
            section.lent.push(entry);
        }
        self.touch(holder);
        if let Some(section) = self.sections.get_mut(holder) {
            section.joined_through = Some(section.joined_through.map_or(lender, |t| t.max(lender)));
        }
    }

    pub(super) fn refresh_joined_through(&mut self, holder: usize) {
        let Some(through) = self.sections.get(holder).and_then(|s| s.joined_through) else {
            return;
        };
        let last = (holder + 1..=through.min(self.sections.len().saturating_sub(1)))
            .rev()
            .find(|&lender| {
                self.sections[lender]
                    .lent
                    .iter()
                    .any(|entry| entry.holder == holder)
            });
        if last != Some(through) {
            self.touch(holder);
            if let Some(section) = self.sections.get_mut(holder) {
                section.joined_through = last;
            }
        }
    }

    pub(super) fn refresh_all_joined_through(&mut self) {
        for section in &mut self.sections {
            section.joined_through = None;
        }
        for lender in 0..self.sections.len() {
            let holders: Vec<usize> = self.sections[lender]
                .lent
                .iter()
                .map(|entry| entry.holder)
                .collect();
            for holder in holders {
                if let Some(section) = self.sections.get_mut(holder) {
                    section.joined_through =
                        Some(section.joined_through.map_or(lender, |t| t.max(lender)));
                }
            }
        }
    }

    /// Before lines `range` of section `index` become `inserted` lines: move
    /// the records of later lines of that section, and drop the records of
    /// the replaced lines, whose text is gone.
    pub(super) fn shift_held_lines(&mut self, index: usize, range: Range<u32>, inserted: u32) {
        for (lender, seq) in self.held_by(index) {
            let Some(offset) = self.lent_record(lender, seq).map(|entry| entry.offset) else {
                continue;
            };
            if offset >= range.end {
                self.touch(lender);
                if let Some(entry) = self.sections[lender]
                    .lent
                    .iter_mut()
                    .find(|entry| entry.seq == seq)
                {
                    entry.offset = offset - (range.end - range.start) + inserted;
                }
            } else if offset >= range.start {
                if let Some(removed) = self.remove_lent(lender, seq, true) {
                    self.uncover_after(lender, &removed);
                }
            }
        }
    }

    pub(super) fn drop_held_lines(&mut self, index: usize, range: Range<u32>) {
        let inserted = range.end - range.start;
        self.shift_held_lines(index, range, inserted);
    }

    /// Restore the state `index` kept for when its lent text is back.
    /// Rebuild its output because edits to the holder may have changed line
    /// boundaries while the text was away.
    fn apply_restore(&mut self, index: usize) {
        let Some(live) = self.sections.get(index) else {
            return;
        };
        let Some(restore) = live.restore.as_deref() else {
            return;
        };
        let restore_from_take = live.restore_from_take;
        let mut state = live.clone();
        state.resolution = restore.resolution;
        state.conflict = restore.conflict || live.conflict;
        if !live.lent.is_empty()
            || (!restore_from_take && automatic_len(&self.inputs, &state) != live.output_len)
        {
            return;
        }
        state.restore = None;
        state.restore_from_take = false;
        state.edited.clear();
        state.edited_from_output = false;
        state.refresh(self.rules);
        self.touch(index);
        self.sections[index] = state;
        if restore_from_take {
            self.rebuild_changed_sections(&[index], true);
        } else {
            self.refresh_section_totals(index);
        }
    }

    /// The records `holder` keeps on output line `line`, in column order, as
    /// `(lender, seq, column, text)`.
    fn records_on_line(&self, holder: usize, line: usize) -> Vec<(usize, u64, u32, String)> {
        let Some(start) = self.output_range(holder).map(|range| range.start as usize) else {
            return Vec::new();
        };
        let mut records: Vec<(usize, u64, u32, String)> = self
            .held_by(holder)
            .into_iter()
            .filter_map(|(lender, seq)| {
                let record = self.lent_record(lender, seq)?;
                (start + record.offset as usize == line)
                    .then(|| (lender, seq, record.column, record.text.clone()))
            })
            .collect();
        records.sort_by_key(|(lender, seq, column, _)| (*column, *lender, *seq));
        records
    }

    /// Lend the characters back to `holder` and the lenders of `records`,
    /// or, when a record does not match the line, all to `holder`.
    fn split_line(
        holder: usize,
        text: &str,
        records: &[(usize, u64, u32, String)],
    ) -> (Vec<Run>, bool) {
        let mut runs = Vec::new();
        let mut position = 0usize;
        for (lender, _, column, lent) in records {
            let column = *column as usize;
            let start = byte_at(text, column);
            let end = byte_at(text, column + lent.chars().count());
            if column < position || text.get(start..end) != Some(lent.as_str()) {
                return (
                    vec![Run {
                        owner: holder,
                        place: holder,
                        text: text.to_owned(),
                    }],
                    false,
                );
            }
            push_run(
                &mut runs,
                holder,
                holder,
                &text[byte_at(text, position)..start],
            );
            push_run(&mut runs, *lender, holder, lent);
            position = column + lent.chars().count();
        }
        push_run(&mut runs, holder, holder, &text[byte_at(text, position)..]);
        (runs, true)
    }

    /// The output line and column where a record's text starts.
    fn record_at(&self, record: &super::Lent) -> Option<(usize, u32)> {
        let range = self.output_range(record.holder)?;
        Some(((range.start + record.offset) as usize, record.column))
    }

    /// The record of `lender`, other than `seq`, whose text comes first
    /// after output position `at`.
    fn next_record(&self, lender: usize, seq: u64, at: (usize, u32)) -> Option<super::Lent> {
        self.sections
            .get(lender)?
            .lent
            .iter()
            .filter(|entry| entry.seq != seq)
            .filter_map(|entry| Some((self.record_at(entry)?, entry)))
            .filter(|(position, _)| *position > at)
            .min_by_key(|(position, _)| *position)
            .map(|(_, entry)| entry.clone())
    }

    /// Give `text` the line terminators it needs to stay a line of its own
    /// in front of `following`, a character of the same lender. Text whose
    /// break an edit removed would otherwise join that character's line,
    /// a line no input holds.
    fn end_before(&self, text: &mut String, following: Option<char>) {
        if following.is_none() {
            return;
        }
        if !text.ends_with(['\r', '\n']) {
            text.push_str(
                self.output_ending
                    .unwrap_or_else(ca_text::EolStyle::platform)
                    .as_str(),
            );
        }
        if text.ends_with('\r') && following == Some('\n') {
            text.push('\n');
        }
    }

    /// Insert `text` at character `column` of line `local` of section
    /// `index`. The records on that line stay on their characters, and the
    /// section keeps its wait for review. Returns how many lines replace the
    /// line, and the characters of `text` each of them holds, as
    /// `(local line, column, text)`.
    fn insert_into_line(
        &mut self,
        index: usize,
        local: u32,
        column: usize,
        text: &str,
    ) -> Option<(u32, Vec<Piece>)> {
        let range = self.output_range(index)?;
        let line = (range.start + local) as usize;
        let old = self.output.get(line)?.clone();
        let split = byte_at(&old, column);
        let merged = format!("{}{text}{}", &old[..split], &old[split..]);
        let parts = super::split(&merged);
        let count = u32::try_from(parts.len()).ok()?;
        let added = text.chars().count();
        let lengths: Vec<usize> = parts.iter().map(|part| part.chars().count()).collect();
        let locate = |mut at: usize| -> (usize, usize) {
            for (part, length) in lengths.iter().enumerate() {
                if at < *length || part + 1 == lengths.len() {
                    return (part, at);
                }
                at -= length;
            }
            (0, at)
        };
        let mut moved = Vec::new();
        for (lender, seq, record_column, _) in self.records_on_line(index, line) {
            if let Some(record) = self.remove_lent(lender, seq, false) {
                let mut at = record_column as usize;
                if at >= column {
                    at += added;
                }
                moved.push((lender, record, locate(at)));
            }
        }
        let mut pieces = Vec::new();
        let mut start = 0usize;
        for (part, (part_text, length)) in parts.iter().zip(&lengths).enumerate() {
            let low = column.max(start);
            let high = (column + added).min(start + length);
            if low < high {
                let piece: String = part_text
                    .chars()
                    .skip(low - start)
                    .take(high - low)
                    .collect();
                pieces.push((
                    local + u32::try_from(part).ok()?,
                    u32::try_from(low - start).ok()?,
                    piece,
                ));
            }
            start += length;
        }
        let conflict = self.sections[index].conflict;
        if !self.edit_output_range(index, local..local + 1, parts) {
            return None;
        }
        self.sections[index].conflict = conflict;
        self.refresh_section_totals(index);
        for (lender, mut record, (part, at)) in moved {
            record.offset = local + u32::try_from(part).ok()?;
            record.column = u32::try_from(at).ok()?;
            self.add_lent(lender, record);
        }
        Some((count, pieces))
    }

    /// Mend the seams of lines `local` of section `index` at once, so a
    /// later change that moves those lines does not leave a line without
    /// its separator.
    fn repair_local(&mut self, index: usize, local: &Range<u32>) {
        if let Some(range) = self.output_range(index) {
            let start = (range.start + local.start) as usize;
            let end = (range.start + local.end).min(range.end) as usize;
            self.repair_output_seams(start..end.max(start));
        }
    }

    /// Give lent text back to its lender. The text goes in front of the
    /// lender's next character in output order: on a later holder's line
    /// as lent text again, or in front of the lender's own text. The
    /// lender's characters keep their order. Returns the section and the
    /// lines that received the text.
    fn give_back(&mut self, lender: usize, seq: u64) -> Option<(usize, Range<u32>)> {
        let record = self.lent_record(lender, seq)?.clone();
        let next = self
            .record_at(&record)
            .and_then(|at| self.next_record(lender, seq, at));
        let entry = self.remove_lent(lender, seq, false)?;
        match next {
            Some(next) => self.lend_before(lender, entry, &next),
            None => self.return_to_lender(lender, entry),
        }
    }

    /// Put the text of `entry`, a record of `lender` no line holds now, in
    /// front of `next`, another record of `lender`, as lent text of the
    /// same order number and source.
    pub(super) fn lend_before(
        &mut self,
        lender: usize,
        entry: super::Lent,
        next: &super::Lent,
    ) -> Option<(usize, Range<u32>)> {
        let mut text = entry.text.clone();
        self.end_before(&mut text, next.text.chars().next());
        let (count, pieces) =
            self.insert_into_line(next.holder, next.offset, next.column as usize, &text)?;
        let mut first = Some((entry.seq, entry.source));
        for (offset, column, piece) in pieces {
            let (seq, source) = first.take().unwrap_or_else(|| {
                self.lend_seq += 1;
                (self.lend_seq, None)
            });
            self.add_lent(
                lender,
                super::Lent {
                    holder: next.holder,
                    offset,
                    column,
                    text: piece,
                    seq,
                    source,
                },
            );
        }
        let lines = next.offset..next.offset + count;
        self.repair_local(next.holder, &lines);
        Some((next.holder, lines))
    }

    /// Put the text of `entry`, a record of `lender` no line holds now, in
    /// front of the lender's own text.
    ///
    /// A restore the lender still keeps applies once its last record is
    /// back, whatever characters the records hold: an edit that changes a
    /// character of the lender after the restore was recorded drops the
    /// restore at that edit, and a take's restore stands for the selected
    /// input over edits made before the take. Comparing the returned
    /// characters here would make the result depend on the take order, and
    /// a record split by a line break returns in pieces whose first piece
    /// carries no source.
    pub(super) fn return_to_lender(
        &mut self,
        lender: usize,
        entry: super::Lent,
    ) -> Option<(usize, Range<u32>)> {
        self.uncover_after(lender, &entry);
        let range = self.output_range(lender)?;
        let mut target = None;
        for local in 0..range.end - range.start {
            let line = (range.start + local) as usize;
            let text = self.output.get(line)?;
            let repair = self.output_repairs.get(line).copied().unwrap_or(0);
            let limit = text.chars().count().saturating_sub(repair);
            let records = self.records_on_line(lender, line);
            let (runs, _) = Self::split_line(lender, text, &records);
            let mut column = 0usize;
            for run in &runs {
                if run.owner == lender && column < limit {
                    target = Some((local, column, text[byte_at(text, column)..].chars().next()));
                    break;
                }
                column += run.text.chars().count();
            }
            if target.is_some() {
                break;
            }
        }
        let conflict = self.sections[lender].conflict;
        let lines = if let Some((local, column, following)) = target {
            let mut text = entry.text;
            self.end_before(&mut text, following);
            let (count, _) = self.insert_into_line(lender, local, column, &text)?;
            local..local + count
        } else {
            let parts = super::split(&entry.text);
            let count = u32::try_from(parts.len()).ok()?;
            let len = range.end - range.start;
            if !self.edit_output_range(lender, len..len, parts) {
                return None;
            }
            self.sections[lender].conflict = conflict;
            self.refresh_section_totals(lender);
            len..len + count
        };
        self.repair_local(lender, &lines);
        self.apply_restore(lender);
        Some((lender, lines))
    }

    /// Give back the text lent to line `local` of `holder`, before that
    /// line is replaced.
    pub(super) fn give_back_line(&mut self, holder: usize, local: u32) -> Vec<(usize, Range<u32>)> {
        let mut records: Vec<(u32, usize, u64)> = self
            .held_by(holder)
            .into_iter()
            .filter_map(|(lender, seq)| {
                let record = self.lent_record(lender, seq)?;
                (record.offset == local).then_some((record.column, lender, seq))
            })
            .collect();
        records.sort_unstable();
        records
            .into_iter()
            .rev()
            .filter_map(|(_, lender, seq)| self.give_back(lender, seq))
            .collect()
    }

    /// Mend the seams around text a take gave back.
    pub(super) fn repair_returned(&mut self, returned: &[(usize, Range<u32>)]) {
        for (lender, local) in returned {
            if let Some(range) = self.output_range(*lender) {
                let start = (range.start + local.start) as usize;
                let end = (range.start + local.end.min(range.end - range.start)) as usize;
                self.repair_output_seams(start..end.max(start));
            }
        }
    }

    /// Take the sections in `taken` again from their inputs. Text lent to a
    /// taken section goes back to its lender; a taken lender leaves out the
    /// text a holder still shows. Returns how many other sections received
    /// text.
    pub(super) fn retake(
        &mut self,
        taken: &[usize],
        resolution_of: impl Fn(usize) -> Resolution,
        decides: bool,
    ) -> usize {
        let is_taken = |index: &usize| taken.binary_search(index).is_ok();
        let mut returned = Vec::new();
        let mut giving: Vec<((usize, u32), usize, u64)> = Vec::new();
        for &holder in taken {
            for (lender, seq) in self.held_by(holder) {
                if is_taken(&lender) {
                    if let Some(removed) = self.remove_lent(lender, seq, false) {
                        self.uncover_after(lender, &removed);
                    }
                } else if let Some(at) = self
                    .lent_record(lender, seq)
                    .and_then(|record| self.record_at(record))
                {
                    giving.push((at, lender, seq));
                }
            }
        }
        // The last text in output order goes back first, so each text finds
        // the lender's later characters already in their place.
        giving.sort_unstable();
        for (_, lender, seq) in giving.into_iter().rev() {
            if let Some(given) = self.give_back(lender, seq) {
                returned.push(given);
            }
        }
        for &index in taken {
            self.touch(index);
            let resolution = resolution_of(index);
            let section = &mut self.sections[index];
            section.resolution = resolution;
            if decides {
                section.conflict = false;
            }
            section.edited.clear();
            section.edited_from_output = false;
            section.restore = None;
            section.restore_from_take = false;
            let fresh = section.clone();
            let mut lines = self.contribution(index, &fresh);
            let mut own: Vec<super::Lent> = fresh.lent.clone();
            own.sort_by_key(|entry| entry.seq);
            if !own.is_empty() {
                // The holders keep showing the lent text, so the new text
                // leaves out the lender's characters it stands for, which
                // start the new text when it holds them at all.
                let covered = own.first().is_some_and(|entry| entry.source.is_some());
                let prefix: String = own
                    .iter()
                    .filter_map(|entry| entry.source.as_deref())
                    .collect();
                if covered && remove_prefix(&mut lines, &prefix) {
                    let mut restore = fresh.clone();
                    restore.lent.clear();
                    restore.joined_through = None;
                    let section = &mut self.sections[index];
                    section.restore = Some(Box::new(restore));
                    section.restore_from_take = true;
                    section.resolution = Resolution::Edited;
                    section.conflict = false;
                } else {
                    for entry in own {
                        self.strip_lent(index, entry.seq);
                    }
                }
            }
            self.replace_section_lines(index, lines);
            self.refresh_section_totals(index);
        }
        for &index in taken {
            if let Some(range) = self.output_range(index) {
                self.repair_output_seams(range.start as usize..range.end as usize);
            }
        }
        self.repair_returned(&returned);
        returned
            .iter()
            .map(|(lender, _)| *lender)
            .collect::<BTreeSet<_>>()
            .len()
    }

    /// Fold one pane edit into the sections. Returns the section that holds
    /// the edit, or nothing when the edit does not fit the model's lines.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn apply_pane_edit(&mut self, edit: &AppliedEdit) -> Option<usize> {
        #[cfg(test)]
        PANE_EDITS.with(|count| count.set(count.get() + 1));
        let total = self.output.len();
        let first = (edit.change.start_line as usize).min(total);
        let end = (edit.change.start_line as usize)
            .saturating_add(edit.change.removed_lines as usize)
            .saturating_add(1)
            .min(total);
        let to_line = |value: usize| u32::try_from(value).ok();

        let mut runs: Vec<Run> = Vec::new();
        let mut line_starts = Vec::new();
        let mut old_lines: Vec<(usize, String)> = Vec::new();
        let mut window_records: Vec<(usize, u64, bool)> = Vec::new();
        let mut chars = 0usize;
        for line in first..end {
            let owner = self.section_of_output_line(to_line(line)?)?;
            let text = self.output.get(line)?.clone();
            let records = self.records_on_line(owner, line);
            let (line_runs, honored) = Self::split_line(owner, &text, &records);
            for run in &line_runs {
                push_run(&mut runs, run.owner, run.place, &run.text);
            }
            for (lender, seq, _, _) in records {
                window_records.push((lender, seq, honored));
            }
            line_starts.push(chars);
            chars += text.chars().count();
            old_lines.push((owner, text));
        }
        let old_text: String = runs.iter().map(|run| run.text.as_str()).collect();
        let offset = edit.offset;
        let removed = edit.removed.chars().count();
        if offset + removed > chars {
            return None;
        }
        let from = byte_at(&old_text, offset);
        let to = byte_at(&old_text, offset + removed);
        if old_text[from..to] != edit.removed {
            return None;
        }
        let offset = if edit.inserted.is_empty() && edit.removed.contains(['\n', '\r']) {
            slide_to_line_start(&old_text, &line_starts, offset, removed)
        } else {
            offset
        };

        let owner_at = |position: usize| -> Option<(usize, usize)> {
            let mut seen = 0usize;
            for run in &runs {
                let count = run.text.chars().count();
                if position < seen + count {
                    return Some((run.owner, run.place));
                }
                seen += count;
            }
            None
        };
        let at_line_start = line_starts.contains(&offset)
            || (offset == chars && (chars == 0 || old_text.ends_with(['\n', '\r'])));
        let fallback_owner = if first > 0 {
            self.section_of_output_line(to_line(first - 1)?)?
        } else {
            self.section_of_output_line(0).unwrap_or(0)
        };
        let inserter = if !at_line_start && offset > 0 {
            owner_at(offset - 1)
        } else if offset < chars {
            owner_at(offset)
        } else if chars > 0 {
            owner_at(chars - 1)
        } else {
            Some((fallback_owner, fallback_owner))
        }
        .unwrap_or((fallback_owner, fallback_owner));

        let mut modified: BTreeSet<usize> = BTreeSet::new();
        let mut new_runs: Vec<Run> = Vec::new();
        let mut seen = 0usize;
        let mut inserted_done = false;
        for run in &runs {
            let count = run.text.chars().count();
            let run_start = seen;
            let run_end = seen + count;
            seen = run_end;
            let keep_before = offset.clamp(run_start, run_end) - run_start;
            let keep_after = (offset + removed).clamp(run_start, run_end) - run_start;
            if keep_after > keep_before {
                modified.insert(run.owner);
            }
            let before_end = byte_at(&run.text, keep_before);
            push_run(&mut new_runs, run.owner, run.place, &run.text[..before_end]);
            if !inserted_done && run_end >= offset {
                push_run(&mut new_runs, inserter.0, inserter.1, &edit.inserted);
                inserted_done = true;
            }
            let after_start = byte_at(&run.text, keep_after);
            push_run(
                &mut new_runs,
                run.owner,
                run.place,
                &run.text[after_start..],
            );
        }
        if !inserted_done {
            push_run(&mut new_runs, inserter.0, inserter.1, &edit.inserted);
        }
        if !edit.inserted.is_empty() {
            modified.insert(inserter.0);
        }

        let new_text: String = new_runs.iter().map(|run| run.text.as_str()).collect();
        let mut line_runs: Vec<Vec<Run>> = Vec::new();
        {
            let mut cursor = new_runs.into_iter();
            let mut pending: Option<Run> = None;
            for line in ca_diff::split_lines(&new_text) {
                let mut need = line.len();
                let mut parts: Vec<Run> = Vec::new();
                while need > 0 {
                    let mut run = match pending.take() {
                        Some(run) => run,
                        None => cursor.next()?,
                    };
                    if run.text.len() > need {
                        let rest = run.text.split_off(need);
                        pending = Some(Run {
                            owner: run.owner,
                            place: run.place,
                            text: rest,
                        });
                    }
                    need -= run.text.len();
                    push_run(&mut parts, run.owner, run.place, &run.text);
                }
                let mut merged: Vec<Run> = Vec::new();
                for part in parts {
                    if is_terminator_only(&part.text) && !merged.is_empty() {
                        if let Some(last) = merged.last_mut() {
                            last.text.push_str(&part.text);
                        }
                    } else {
                        push_run(&mut merged, part.owner, part.place, &part.text);
                    }
                }
                line_runs.push(merged);
            }
        }

        let low = fallback_owner.min(self.section_of_output_line(to_line(first)?).unwrap_or(0));
        let high = if end < total {
            self.section_of_output_line(to_line(end)?)?
        } else {
            self.sections.len().saturating_sub(1)
        };
        let mut previous = low;
        let mut assigned: BTreeMap<usize, Vec<String>> = BTreeMap::new();
        let mut new_records: Vec<NewRecord> = Vec::new();
        let mut lost: BTreeSet<usize> = BTreeSet::new();
        for (position, parts) in line_runs.iter().enumerate() {
            let lowest = parts.iter().map(|run| run.place).min().unwrap_or(previous);
            let owner = if lowest > high {
                previous
            } else {
                lowest.max(previous)
            };
            let text: String = parts.iter().map(|run| run.text.as_str()).collect();
            let mut column = 0u32;
            let mut previous_run: Option<usize> = None;
            for run in parts {
                let count = u32::try_from(run.text.chars().count()).ok()?;
                if run.owner > owner {
                    let continues = previous_run == Some(run.owner);
                    let fresh = if run.place == run.owner {
                        run.text.as_str()
                    } else {
                        ""
                    };
                    match new_records.last_mut() {
                        Some(record) if continues => {
                            record.text.push_str(&run.text);
                            record.fresh.push_str(fresh);
                        }
                        _ => {
                            new_records.push(NewRecord {
                                lender: run.owner,
                                holder: owner,
                                position,
                                column,
                                text: run.text.clone(),
                                fresh: fresh.to_owned(),
                            });
                        }
                    }
                } else if run.owner < owner {
                    lost.insert(run.owner);
                }
                previous_run = Some(run.owner);
                column += count;
            }
            assigned.entry(owner).or_default().push(text);
            previous = owner;
        }
        let mut span: BTreeSet<usize> = assigned.keys().copied().collect();
        span.extend(old_lines.iter().map(|(owner, _)| *owner));
        let (Some(&span_low), Some(&span_high)) = (span.iter().next(), span.iter().next_back())
        else {
            return Some(inserter.1);
        };
        let lenders_involved: BTreeSet<usize> = window_records
            .iter()
            .map(|(lender, _, _)| *lender)
            .chain(new_records.iter().map(|record| record.lender))
            .collect();
        let mut before: BTreeMap<usize, Section> = BTreeMap::new();
        for index in (span_low..=span_high).chain(lenders_involved.iter().copied()) {
            if let Some(section) = self.sections.get(index) {
                before.entry(index).or_insert_with(|| section.clone());
            }
        }
        let mut previous_records: BTreeMap<usize, Vec<super::Lent>> = BTreeMap::new();
        for (lender, seq, honored) in &window_records {
            if let Some(record) = self.remove_lent(*lender, *seq, !honored) {
                previous_records.entry(*lender).or_default().push(record);
            }
        }
        for records in previous_records.values_mut() {
            records.sort_by_key(|record| record.seq);
        }

        for index in (span_low..=span_high).rev() {
            let range = self.output_range(index)?;
            let local_start = to_line(first)?.clamp(range.start, range.end) - range.start;
            let local_end = to_line(end)?.clamp(range.start, range.end) - range.start;
            let new_lines = assigned.remove(&index).unwrap_or_default();
            let same = local_end - local_start == u32::try_from(new_lines.len()).ok()?
                && new_lines.iter().enumerate().all(|(offset, line)| {
                    self.output
                        .get((range.start + local_start) as usize + offset)
                        == Some(line)
                });
            if !same && !self.edit_output_range(index, local_start..local_end, new_lines) {
                return None;
            }
        }

        // The first new record of a lender carries the source of every
        // record of that lender the window held, then the characters this
        // edit moved off the lender's own lines, in output order.
        let mut sources: BTreeMap<usize, Option<String>> = BTreeMap::new();
        for record in &new_records {
            sources.entry(record.lender).or_insert_with(|| {
                let olds = previous_records.get(&record.lender);
                let carried = olds.is_some_and(|olds| olds.iter().any(|old| old.source.is_some()));
                let fresh: String = new_records
                    .iter()
                    .filter(|other| other.lender == record.lender)
                    .map(|other| other.fresh.as_str())
                    .collect();
                if olds.is_some_and(|olds| !olds.is_empty()) && !carried && fresh.is_empty() {
                    return None;
                }
                let mut source: String = olds
                    .into_iter()
                    .flatten()
                    .filter_map(|old| old.source.as_deref())
                    .collect();
                source.push_str(&fresh);
                Some(source)
            });
        }
        let mut used: BTreeMap<usize, usize> = BTreeMap::new();
        let mut lending: BTreeSet<usize> = BTreeSet::new();
        for record in new_records {
            let lender = record.lender;
            let holder_start = self.output_range(record.holder)?.start as usize;
            let line = first + record.position;
            let offset = u32::try_from(line.checked_sub(holder_start)?).ok()?;
            let rank = used.entry(lender).or_insert(0);
            let old = previous_records
                .get(&lender)
                .and_then(|olds| olds.get(*rank));
            *rank += 1;
            let seq = if let Some(old) = old {
                if old.text != record.text {
                    modified.insert(lender);
                }
                old.seq
            } else {
                self.lend_seq += 1;
                self.lend_seq
            };
            let source = sources.get_mut(&lender).and_then(Option::take);
            self.add_lent(
                lender,
                super::Lent {
                    holder: record.holder,
                    offset,
                    column: record.column,
                    text: record.text,
                    seq,
                    source,
                },
            );
            lending.insert(lender);
        }
        for lender in &lenders_involved {
            self.settle_sources(*lender);
        }

        for index in before.keys().copied().collect::<Vec<_>>() {
            let Some(previous) = before.get(&index) else {
                continue;
            };
            if modified.contains(&index) || lost.contains(&index) {
                if self.sections[index].restore.is_some() {
                    self.touch(index);
                    self.sections[index].restore = None;
                    self.sections[index].restore_from_take = false;
                }
                continue;
            }
            if lending.contains(&index)
                && previous.restore.is_none()
                && previous.lent.is_empty()
                && !matches!(previous.resolution, Resolution::Edited)
            {
                let mut restore = previous.clone();
                restore.lent.clear();
                restore.joined_through = None;
                self.touch(index);
                self.sections[index].restore = Some(Box::new(restore));
                self.sections[index].restore_from_take = false;
            }
            self.apply_restore(index);
        }
        for line in first.saturating_sub(1)..first + line_runs.len() {
            self.claim_stale_repair(line);
        }
        Some(inserter.1)
    }

    /// Put the pane's `lines` in place of output lines `first..end` when an
    /// edit does not fit the model's lines: every line goes to the section of
    /// the first one, and text lent to the replaced lines is lost.
    pub(crate) fn resync_window(
        &mut self,
        first: u32,
        end: u32,
        lines: &[String],
    ) -> Option<usize> {
        #[cfg(test)]
        RESYNCS.with(|count| count.set(count.get() + 1));
        let total = u32::try_from(self.output.len()).ok()?;
        let end = end.min(total);
        let first = first.min(end);
        let owner = if first < total {
            self.section_of_output_line(first)?
        } else if total > 0 {
            self.section_of_output_line(total - 1)?
        } else {
            0
        };
        let last = if end > first {
            self.section_of_output_line(end - 1)?.max(owner)
        } else {
            owner
        };
        for index in (owner..=last).rev() {
            let range = self.output_range(index)?;
            let local_start = first.clamp(range.start, range.end) - range.start;
            let local_end = end.clamp(range.start, range.end) - range.start;
            let replacement = if index == owner {
                lines.to_vec()
            } else {
                Vec::new()
            };
            self.touch(index);
            self.sections[index].restore = None;
            self.sections[index].restore_from_take = false;
            if !self.edit_output_range(index, local_start..local_end, replacement) {
                return None;
            }
        }
        Some(owner)
    }
}
