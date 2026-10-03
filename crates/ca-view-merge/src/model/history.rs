//! Exact undo records of the model.
//!
//! A step holds every output line it replaced, with the separator count a
//! composition added to each line, and the state of every section it
//! changed. Undo and Redo replay a step without reading the pane or any
//! input, so the cost of a replay is the lines and sections the step changed.

use super::{MergeModel, Section};
use std::collections::BTreeMap;
use std::ops::Range;

/// Output lines one change replaced, in the line numbers of the output at
/// the time of that change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Replacement {
    pub(crate) at: usize,
    pub(crate) old: Vec<(String, usize)>,
    pub(crate) new: Vec<(String, usize)>,
}

#[derive(Debug, Clone)]
struct StepSection {
    index: usize,
    before: Section,
    after: Section,
    /// Output lines before this section that sections outside the step
    /// hold; the step leaves them alone, so the count is the same before
    /// and after it.
    outside: u32,
}

/// One undoable change of the model.
#[derive(Debug, Clone, Default)]
pub(crate) struct Step {
    replacements: Vec<Replacement>,
    sections: Vec<StepSection>,
}

/// The record of a step while it is made.
#[derive(Debug, Clone, Default)]
pub(crate) struct Journal {
    replacements: Vec<Replacement>,
    before: BTreeMap<usize, Section>,
}

impl Step {
    /// The output lines this step replaced, as `(old, new)` line ranges of
    /// the output before and after the whole step. Overlapping and touching
    /// changes become one range.
    pub(crate) fn windows(&self) -> Vec<(Range<u32>, Range<u32>)> {
        let mut windows: Vec<(Range<usize>, Range<usize>)> = Vec::new();
        for replacement in &self.replacements {
            let start = replacement.at;
            let removed_end = start + replacement.old.len();
            let shift = signed(replacement.new.len()) - signed(replacement.old.len());
            let mut merged_old_start = None;
            let mut merged_new_start = start;
            let mut merged_end = removed_end;
            let mut inner_shift = 0i64;
            let mut before_shift = 0i64;
            let mut kept = Vec::new();
            let mut after = Vec::new();
            for (old, new) in windows.drain(..) {
                if new.end < start {
                    before_shift += signed(new.len()) - signed(old.len());
                    kept.push((old, new));
                } else if new.start > removed_end {
                    let moved = shift_range(&new, shift);
                    after.push((old, moved));
                } else {
                    if new.start < merged_new_start {
                        merged_new_start = new.start;
                        merged_old_start = Some(old.start);
                    }
                    merged_end = merged_end.max(new.end);
                    inner_shift += signed(new.len()) - signed(old.len());
                }
            }
            let old_start =
                merged_old_start.unwrap_or_else(|| shift_value(merged_new_start, -before_shift));
            let old_end = shift_value(merged_end, -before_shift - inner_shift);
            let new_end = shift_value(merged_end, shift);
            kept.push((
                old_start..old_end.max(old_start),
                merged_new_start..new_end.max(merged_new_start),
            ));
            kept.extend(after);
            windows = kept;
        }
        let line = |value: usize| u32::try_from(value).unwrap_or(u32::MAX);
        windows
            .into_iter()
            .map(|(old, new)| {
                (
                    line(old.start)..line(old.end),
                    line(new.start)..line(new.end),
                )
            })
            .collect()
    }

    /// True when the step changes a section's line status or its row count,
    /// which the display filter reads.
    pub(crate) fn changes_layout(&self) -> bool {
        let rows = |section: &Section| {
            let len = |range: &Range<u32>| range.end.saturating_sub(range.start);
            len(&section.left)
                .max(len(&section.center))
                .max(len(&section.right))
                .max(section.output_len)
        };
        self.sections.iter().any(|entry| {
            entry.before.status() != entry.after.status()
                || rows(&entry.before) != rows(&entry.after)
        })
    }

    /// True when the step changed more than one section.
    pub(crate) fn touches_several_sections(&self) -> bool {
        self.sections.len() > 1
    }

    /// Add a later step of the same undo group. `model` holds the state
    /// after both.
    pub(crate) fn extend(&mut self, later: Self, model: &MergeModel) {
        self.replacements.extend(later.replacements);
        for section in later.sections {
            if let Some(known) = self
                .sections
                .iter_mut()
                .find(|known| known.index == section.index)
            {
                known.after = section.after;
            } else {
                self.sections.push(section);
            }
        }
        self.sections.sort_by_key(|section| section.index);
        model.count_outside(&mut self.sections);
    }
}

fn signed(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn shift_value(value: usize, shift: i64) -> usize {
    usize::try_from(signed(value).saturating_add(shift)).unwrap_or(0)
}

fn shift_range(range: &Range<usize>, shift: i64) -> Range<usize> {
    shift_value(range.start, shift)..shift_value(range.end, shift)
}

/// Sections are compared without the two marks a user toggles outside the
/// undo history.
fn same_state(left: &Section, right: &Section) -> bool {
    let mut left = left.clone();
    left.conflict = right.conflict;
    left.ignored = right.ignored;
    left.suppressed = right.suppressed;
    left == *right
}

impl MergeModel {
    /// Start recording a step.
    pub(crate) fn begin_journal(&mut self) {
        self.journal = Some(Journal::default());
    }

    /// Finish the step being recorded. Returns nothing when it changed nothing.
    pub(crate) fn end_journal(&mut self) -> Option<Step> {
        let journal = self.journal.take()?;
        let replacements: Vec<Replacement> = journal
            .replacements
            .into_iter()
            .filter(|replacement| replacement.old != replacement.new)
            .collect();
        let mut sections: Vec<StepSection> = journal
            .before
            .into_iter()
            .filter_map(|(index, before)| {
                let after = self.sections.get(index)?.clone();
                (before != after).then_some(StepSection {
                    index,
                    before,
                    after,
                    outside: 0,
                })
            })
            .collect();
        self.count_outside(&mut sections);
        (!replacements.is_empty() || !sections.is_empty()).then_some(Step {
            replacements,
            sections,
        })
    }

    /// Keep the state of a section before the step in progress changes it.
    pub(crate) fn touch(&mut self, index: usize) {
        if self.journal.is_none() {
            return;
        }
        let Some(section) = self.sections.get(index).cloned() else {
            return;
        };
        if let Some(journal) = self.journal.as_mut() {
            journal.before.entry(index).or_insert(section);
        }
    }

    /// Set each step section's count of earlier lines outside the step,
    /// from the model's current lines. `sections` are in index order.
    fn count_outside(&self, sections: &mut [StepSection]) {
        let mut inside = 0u32;
        for entry in sections {
            let start = self
                .output_range(entry.index)
                .map_or(0, |range| range.start);
            entry.outside = start.saturating_sub(inside);
            inside = inside.saturating_add(
                self.sections
                    .get(entry.index)
                    .map_or(0, |section| section.output_len),
            );
        }
    }

    /// Replace output lines, recording them for the step in progress.
    pub(crate) fn splice_output(
        &mut self,
        range: Range<usize>,
        lines: Vec<String>,
        repairs: Vec<usize>,
    ) {
        if let Some(journal) = self.journal.as_mut() {
            let old = self
                .output
                .copy_range(range.clone())
                .into_iter()
                .zip(self.output_repairs.copy_range(range.clone()))
                .collect();
            let new = lines.iter().cloned().zip(repairs.iter().copied()).collect();
            journal.replacements.push(Replacement {
                at: range.start,
                old,
                new,
            });
        }
        self.output_repairs.splice(range.clone(), repairs);
        self.output.splice(range, lines);
    }

    /// Replace one output line in place, recording it for the step in progress.
    pub(crate) fn set_output_line(&mut self, index: usize, text: String, repair: usize) {
        if let Some(journal) = self.journal.as_mut() {
            let old = (
                self.output.get(index).cloned().unwrap_or_default(),
                self.output_repairs.get(index).copied().unwrap_or(0),
            );
            journal.replacements.push(Replacement {
                at: index,
                old: vec![old],
                new: vec![(text.clone(), repair)],
            });
        }
        if let Some(line) = self.output.get_mut(index) {
            *line = text;
        }
        if let Some(value) = self.output_repairs.get_mut(index) {
            *value = repair;
        }
    }

    /// True when the model holds the state `step` leaves (`forward` false)
    /// or the state it starts from (`forward` true).
    pub(crate) fn step_applies(&self, step: &Step, forward: bool) -> bool {
        let mut inside = 0u32;
        step.sections.iter().all(|entry| {
            let expected = if forward { &entry.before } else { &entry.after };
            let Some(section) = self.sections.get(entry.index) else {
                return false;
            };
            let start = self
                .output_range(entry.index)
                .map_or(0, |range| range.start);
            let aligned = start.checked_sub(inside) == Some(entry.outside);
            inside = inside.saturating_add(section.output_len);
            aligned && same_state(section, expected)
        })
    }

    /// Undo (`forward` false) or redo (`forward` true) a recorded step.
    ///
    /// Toggle Ignored and Toggle Conflict are not undo steps. A mark that
    /// differs from its value in the state being left was set after that
    /// step and stays.
    pub(crate) fn replay(&mut self, step: &Step, forward: bool) {
        if forward {
            for replacement in &step.replacements {
                self.replay_lines(replacement.at, replacement.old.len(), &replacement.new);
            }
        } else {
            for replacement in step.replacements.iter().rev() {
                self.replay_lines(replacement.at, replacement.new.len(), &replacement.old);
            }
        }
        for entry in &step.sections {
            let (target, leaving) = if forward {
                (&entry.after, &entry.before)
            } else {
                (&entry.before, &entry.after)
            };
            let Some(live) = self.sections.get(entry.index) else {
                continue;
            };
            let mut section = target.clone();
            if live.ignored != leaving.ignored {
                section.ignored = live.ignored;
            }
            if live.conflict != leaving.conflict {
                section.conflict = live.conflict;
            }
            let len = section.output_len;
            section.refresh(self.rules);
            self.sections[entry.index] = section;
            self.set_output_len(entry.index, len);
        }
    }

    fn replay_lines(&mut self, at: usize, removed: usize, lines: &[(String, usize)]) {
        let len = self.output.len();
        #[cfg(test)]
        {
            if at > len {
                super::REPLAY_START_CLAMPS.with(|count| count.set(count.get() + 1));
            }
            if at.saturating_add(removed) > len {
                super::REPLAY_END_CLAMPS.with(|count| count.set(count.get() + 1));
            }
        }
        let range = at.min(len)..at.saturating_add(removed).min(len);
        let (texts, repairs): (Vec<String>, Vec<usize>) = lines.iter().cloned().unzip();
        self.output_repairs.splice(range.clone(), repairs);
        self.output.splice(range, texts);
    }
}
