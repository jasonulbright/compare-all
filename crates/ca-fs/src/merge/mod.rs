//! Three way folder comparison, and the plan that writes its result.
//!
//! Each path is looked up in the left, center and right trees. The center is
//! the common ancestor: a path one side holds and the ancestor lacks is an
//! addition, and a path the ancestor holds and one side lacks is a deletion.
//! A side counts as changed when its item is not the same as the ancestor's
//! under the quick tests and the content test of a two way comparison.
//!
//! An absence proves a deletion only where the listing was read in full. A
//! path under a partial listing, a cancelled scan or an unreadable entry is
//! classified [`MergeStatus::Unknown`], and the planner never removes anything
//! on the strength of such a row.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use rayon::prelude::*;

use crate::cancel::Cancel;
use crate::criteria::{
    compare_contents, compare_source_contents_with, quick_compare_with, CompareOptions,
    ContentError, ContentSide, RulesComparer,
};
use crate::filter::{CaseSensitivity, FilterContext, NameFilters, OtherFilters};
use crate::scan::{Entry, ScanResult};
use crate::source::{EntryFacts, Source};

pub mod plan;

#[cfg(test)]
mod tests;

pub use plan::{
    automatic_resolution, leaves_for_person, left_for_person, plan_merge, MergeRefused,
    MergeRequest, Resolution,
};

/// One of the three inputs of a merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Pane {
    /// The left version.
    Left,
    /// The common ancestor.
    Center,
    /// The right version.
    Right,
}

/// What one side did to the ancestor's item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// The ancestor lacks the item and the side holds it.
    Added,
    /// The ancestor holds the item and the side lacks it.
    Deleted,
    /// Both hold the item and the two differ.
    Modified,
}

/// Merge status of one path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MergeStatus {
    /// Neither side changed the ancestor's item.
    #[default]
    Unchanged,
    /// Only the left side changed the item.
    LeftChange(Change),
    /// Only the right side changed the item.
    RightChange(Change),
    /// Both sides made the same change.
    SameChange(Change),
    /// Both sides changed a text file differently, and a three way text merge
    /// of the two finds no conflicting region.
    Mergeable,
    /// Both sides changed the item differently.
    Conflict,
    /// An input could not be read in full at this path, so no status can be
    /// proved.
    Unknown,
}

impl MergeStatus {
    /// True for the two statuses a person has to resolve.
    #[must_use]
    pub const fn needs_person(self) -> bool {
        matches!(self, Self::Mergeable | Self::Conflict)
    }

    /// True when at least one side changed the item.
    #[must_use]
    pub const fn is_change(self) -> bool {
        !matches!(self, Self::Unchanged | Self::Unknown)
    }

    /// True when the left side changed the item, alone or with the right.
    #[must_use]
    pub const fn left_changed(self) -> bool {
        matches!(
            self,
            Self::LeftChange(_) | Self::SameChange(_) | Self::Mergeable | Self::Conflict
        )
    }

    /// True when the right side changed the item, alone or with the left.
    #[must_use]
    pub const fn right_changed(self) -> bool {
        matches!(
            self,
            Self::RightChange(_) | Self::SameChange(_) | Self::Mergeable | Self::Conflict
        )
    }

    /// The name a legend or a report shows.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Unchanged => "Unchanged",
            Self::LeftChange(Change::Added) => "Left add",
            Self::LeftChange(Change::Deleted) => "Left delete",
            Self::LeftChange(Change::Modified) => "Left change",
            Self::RightChange(Change::Added) => "Right add",
            Self::RightChange(Change::Deleted) => "Right delete",
            Self::RightChange(Change::Modified) => "Right change",
            Self::SameChange(_) => "Same change",
            Self::Mergeable => "Mergeable",
            Self::Conflict => "Conflict",
            Self::Unknown => "Unknown",
        }
    }
}

/// One path of a three way comparison.
#[derive(Debug, Clone)]
pub struct MergeRow {
    /// Path relative to the base folders, spelled as the first input that
    /// holds it spells it.
    pub rel: PathBuf,
    /// Final path component.
    pub name: String,
    /// Depth below the base folders; a top level item is zero.
    pub depth: usize,
    /// True when any input holds a folder here.
    pub is_dir: bool,
    /// The left version's item.
    pub left: Option<Entry>,
    /// The ancestor's item.
    pub center: Option<Entry>,
    /// The right version's item.
    pub right: Option<Entry>,
    /// What the output folder holds here now.
    pub output: Option<Entry>,
    /// Merge status.
    pub status: MergeStatus,
    /// True when both changed versions are files that read as text, so the
    /// three way text merge can open them.
    pub text: bool,
    /// Why the status is [`MergeStatus::Unknown`].
    pub error: Option<String>,
    /// True when the output listing is partial at this path, so its absence
    /// there proves nothing.
    pub output_uncertain: bool,
    /// True when the output holds something here that the session's filters
    /// left out, so the folder is not empty after its listed items go.
    pub output_holds_excluded: bool,
}

impl MergeRow {
    /// The item one input holds here.
    #[must_use]
    pub const fn entry(&self, pane: Pane) -> Option<&Entry> {
        match pane {
            Pane::Left => self.left.as_ref(),
            Pane::Center => self.center.as_ref(),
            Pane::Right => self.right.as_ref(),
        }
    }
}

/// Every path of a three way comparison, parents before children.
#[derive(Debug, Clone, Default)]
pub struct MergeTree {
    /// Rows in tree order.
    pub rows: Vec<MergeRow>,
    /// True when any scan was cancelled, which makes every absence suspect.
    pub cancelled: bool,
    /// True when an ancestor folder took part.
    pub has_center: bool,
    /// True when the output folder exists and was listed.
    pub output_listed: bool,
    output_unsafe_paths: BTreeSet<Vec<String>>,
    output_link_paths: BTreeSet<Vec<String>>,
    text_merge_inputs_are_local: bool,
}

impl MergeTree {
    /// Record whether every input to this tree can be opened as an ordinary
    /// local file by the text merge worker.
    pub fn set_text_merge_inputs_are_local(&mut self, local: bool) {
        self.text_merge_inputs_are_local = local;
    }

    /// Whether text merge can read all input paths directly from disk.
    #[must_use]
    pub const fn text_merge_inputs_are_local(&self) -> bool {
        self.text_merge_inputs_are_local
    }

    /// True when writing `rel` cannot pass through an output link or file.
    #[must_use]
    pub fn output_target_is_safe(&self, rel: &Path) -> bool {
        let Some(key) = key_of(rel, true) else {
            return false;
        };
        for length in (1..=key.len()).rev() {
            let current = &key[..length];
            let is_target = length == key.len();
            if self.output_unsafe_paths.contains(current)
                && (!is_target || self.output_link_paths.contains(current))
            {
                return false;
            }
        }
        true
    }
}

/// How many rows carry each status.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MergeCounts {
    /// Rows neither side changed.
    pub unchanged: usize,
    /// Rows only the left side changed.
    pub left: usize,
    /// Rows only the right side changed.
    pub right: usize,
    /// Rows both sides changed the same way.
    pub same: usize,
    /// Rows a text merge resolves.
    pub mergeable: usize,
    /// Rows a person has to resolve.
    pub conflicts: usize,
    /// Rows with no provable status.
    pub unknown: usize,
}

impl MergeTree {
    /// The position of the row at `rel`.
    #[must_use]
    pub fn find(&self, rel: &Path) -> Option<usize> {
        self.rows.iter().position(|row| row.rel == rel)
    }

    /// How many rows carry each status.
    #[must_use]
    pub fn counts(&self) -> MergeCounts {
        let mut counts = MergeCounts::default();
        for row in &self.rows {
            match row.status {
                MergeStatus::Unchanged => counts.unchanged += 1,
                MergeStatus::LeftChange(_) => counts.left += 1,
                MergeStatus::RightChange(_) => counts.right += 1,
                MergeStatus::SameChange(_) => counts.same += 1,
                MergeStatus::Mergeable => counts.mergeable += 1,
                MergeStatus::Conflict => counts.conflicts += 1,
                MergeStatus::Unknown => counts.unknown += 1,
            }
        }
        counts
    }
}

/// The scanned inputs of one merge.
#[derive(Debug, Clone, Copy)]
pub struct MergeInputs<'a> {
    /// The left version.
    pub left: &'a ScanResult,
    /// The ancestor, where the merge has one.
    pub center: Option<&'a ScanResult>,
    /// The right version.
    pub right: &'a ScanResult,
    /// The output folder, or `None` when it does not exist yet.
    pub output: Option<&'a ScanResult>,
}

/// Where the inputs and the output live.
#[derive(Debug, Clone, Copy)]
pub struct MergeBases<'a> {
    /// Left base folder.
    pub left: &'a Path,
    /// Ancestor base folder.
    pub center: Option<&'a Path>,
    /// Right base folder.
    pub right: &'a Path,
    /// Output base folder.
    pub output: &'a Path,
}

impl<'a> MergeBases<'a> {
    /// The base folder of one input.
    #[must_use]
    pub fn of(&self, pane: Pane) -> Option<&'a Path> {
        match pane {
            Pane::Left => Some(self.left),
            Pane::Center => self.center,
            Pane::Right => Some(self.right),
        }
    }
}

/// Settings of a three way comparison.
#[derive(Debug, Clone)]
pub struct FolderMergeOptions {
    /// The quick and content tests, as a two way comparison takes them.
    pub compare: CompareOptions,
    /// Case rule for matching names across the inputs.
    pub case: CaseSensitivity,
    /// Settings of the text merge that decides whether a row is mergeable.
    pub text: ca_diff::merge3::MergeOptions,
    /// Largest file the text merge reads. A larger pair is a conflict.
    pub text_limit: u64,
}

impl Default for FolderMergeOptions {
    fn default() -> Self {
        Self {
            compare: CompareOptions::default(),
            case: CaseSensitivity::default(),
            text: ca_diff::merge3::MergeOptions::default(),
            text_limit: 4 * 1024 * 1024,
        }
    }
}

/// The session's filters, as the comparison applies them.
#[derive(Debug, Clone, Copy)]
pub struct MergeFilters<'a> {
    /// Name masks.
    pub names: &'a NameFilters,
    /// Non-name criteria.
    pub others: &'a OtherFilters,
    /// The instant and time zone the date criteria read.
    pub context: &'a FilterContext,
}

/// Slot positions in the per-path array.
const LEFT: usize = 0;
const CENTER: usize = 1;
const RIGHT: usize = 2;
const OUTPUT: usize = 3;

/// One path as the four listings hold it.
#[derive(Debug, Default)]
struct Slot {
    rel: Option<PathBuf>,
    entries: [Option<Entry>; 4],
    facts: [EntryFacts; 4],
}

/// Whether two items count as the same.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Sameness {
    Same,
    Different,
    Unknown(String),
}

/// Compare three folder trees and classify every path.
///
/// Filters remove whole paths, never one side of a path: a row the session
/// leaves out takes no part at all, so a filter can never make an item look
/// deleted on one side.
#[must_use]
pub fn compare3(
    inputs: MergeInputs<'_>,
    bases: MergeBases<'_>,
    options: &FolderMergeOptions,
    filters: MergeFilters<'_>,
    rules: Option<&dyn RulesComparer>,
    cancel: &Cancel,
) -> MergeTree {
    compare3_in(inputs, bases, None, options, filters, rules, cancel)
}

/// The sources the three inputs were scanned from, where any of them is not
/// a local folder.
#[derive(Debug, Clone, Copy)]
pub struct MergeSources<'a> {
    /// The left version.
    pub left: &'a Source,
    /// The ancestor, where the merge has one.
    pub center: Option<&'a Source>,
    /// The right version.
    pub right: &'a Source,
}

impl<'a> MergeSources<'a> {
    fn of(&self, position: usize) -> Option<&'a Source> {
        match position {
            LEFT => Some(self.left),
            CENTER => self.center,
            RIGHT => Some(self.right),
            _ => None,
        }
    }
}

/// [`compare3`] over inputs that may be containers or other sources.
///
/// Every content read of an input goes through its source. The output is
/// still the local folder `bases.output` names.
#[must_use]
pub fn compare3_sources(
    inputs: MergeInputs<'_>,
    bases: MergeBases<'_>,
    sources: MergeSources<'_>,
    options: &FolderMergeOptions,
    filters: MergeFilters<'_>,
    rules: Option<&dyn RulesComparer>,
    cancel: &Cancel,
) -> MergeTree {
    compare3_in(
        inputs,
        bases,
        Some(sources),
        options,
        filters,
        rules,
        cancel,
    )
}

fn compare3_in(
    inputs: MergeInputs<'_>,
    bases: MergeBases<'_>,
    sources: Option<MergeSources<'_>>,
    options: &FolderMergeOptions,
    filters: MergeFilters<'_>,
    rules: Option<&dyn RulesComparer>,
    cancel: &Cancel,
) -> MergeTree {
    let ignore_case = options.case.ignores_case();
    let output_unsafe_paths = inputs.output.map_or_else(BTreeSet::new, |listing| {
        listing
            .entries
            .values()
            .filter(|entry| entry.is_link() || !entry.is_dir)
            .filter_map(|entry| key_of(&entry.rel, true))
            .collect()
    });
    let output_link_paths = inputs.output.map_or_else(BTreeSet::new, |listing| {
        listing
            .entries
            .values()
            .filter(|entry| entry.is_link())
            .filter_map(|entry| key_of(&entry.rel, true))
            .collect()
    });
    let mut slots: BTreeMap<Vec<String>, Slot> = BTreeMap::new();
    let listings = [
        Some(inputs.left),
        inputs.center,
        Some(inputs.right),
        inputs.output,
    ];
    for (position, listing) in listings.iter().enumerate() {
        let Some(listing) = listing else { continue };
        for entry in listing.entries.values() {
            let Some(key) = key_of(&entry.rel, ignore_case) else {
                continue;
            };
            let slot = slots.entry(key).or_default();
            if slot.rel.is_none() {
                slot.rel = Some(entry.rel.clone());
            }
            slot.entries[position] = Some(entry.clone());
            slot.facts[position] = listing.facts_of(&entry.rel);
        }
    }

    let incomplete: Vec<BTreeSet<Vec<String>>> = listings
        .iter()
        .map(|listing| {
            listing.map_or_else(BTreeSet::new, |listing| {
                incomplete_keys(listing, ignore_case)
            })
        })
        .collect();
    let cancelled =
        listings.iter().flatten().any(|listing| listing.cancelled) || cancel.is_cancelled();

    // A path the filters leave out is dropped with every path below it.
    let mut excluded: BTreeSet<Vec<String>> = BTreeSet::new();
    let mut output_excluded: BTreeSet<Vec<String>> = BTreeSet::new();
    let mut kept: Vec<(Vec<String>, Slot)> = Vec::new();
    for (key, slot) in slots {
        let under_excluded = (1..key.len()).any(|end| excluded.contains(&key[..end]));
        if under_excluded || !allowed(&slot, filters) {
            if slot.entries[OUTPUT].is_some() {
                output_excluded.insert(key.clone());
            }
            excluded.insert(key);
            continue;
        }
        kept.push((key, slot));
    }

    let has_center = inputs.center.is_some();
    let mut rows: Vec<MergeRow> = kept
        .into_par_iter()
        .map(|(key, slot)| {
            classify(
                &key,
                slot,
                &incomplete,
                cancelled,
                Where { bases, sources },
                options,
                rules,
                cancel,
            )
        })
        .collect();
    for row in &mut rows {
        if row.is_dir {
            let key = key_of(&row.rel, ignore_case).unwrap_or_default();
            row.output_holds_excluded = output_excluded
                .iter()
                .any(|excluded| excluded.len() > key.len() && excluded.starts_with(&key));
        }
    }
    MergeTree {
        rows,
        cancelled,
        has_center,
        output_listed: inputs.output.is_some(),
        output_unsafe_paths,
        output_link_paths,
        text_merge_inputs_are_local: false,
    }
}

/// The comparison key of a relative path, or `None` for a path that could
/// climb out of its base folder.
fn key_of(rel: &Path, ignore_case: bool) -> Option<Vec<String>> {
    let mut key = Vec::new();
    for component in rel.components() {
        match component {
            Component::Normal(part) => {
                let text = part.to_string_lossy();
                key.push(if ignore_case {
                    text.to_lowercase()
                } else {
                    text.into_owned()
                });
            }
            _ => return None,
        }
    }
    Some(key)
}

/// Every folder of one listing whose contents were not read in full.
fn incomplete_keys(listing: &ScanResult, ignore_case: bool) -> BTreeSet<Vec<String>> {
    let mut keys = BTreeSet::new();
    if listing.root_incomplete || listing.cancelled {
        keys.insert(Vec::new());
    }
    for entry in listing.entries.values() {
        if entry.listing_incomplete {
            if let Some(key) = key_of(&entry.rel, ignore_case) {
                keys.insert(key);
            }
        }
    }
    for error in &listing.errors {
        match key_of(&error.rel, ignore_case) {
            Some(key) => {
                keys.insert(key);
            }
            None => {
                keys.insert(Vec::new());
            }
        }
    }
    keys
}

/// True when `key` or any folder above it is partial in `set`.
fn is_uncertain(set: &BTreeSet<Vec<String>>, key: &[String]) -> bool {
    (0..=key.len()).any(|end| set.contains(&key[..end]))
}

/// True when the session's filters keep the path.
///
/// A non-name criterion leaves a file out only when it rejects every item the
/// inputs hold, so no one side is ever filtered away from the others.
fn allowed(slot: &Slot, filters: MergeFilters<'_>) -> bool {
    let Some(rel) = slot.rel.as_deref() else {
        return false;
    };
    let is_dir = slot.entries.iter().flatten().any(|entry| entry.is_dir);
    if is_dir {
        return filters.names.allows_folder_traversal(rel);
    }
    if !filters.names.allows(rel, false) {
        return false;
    }
    let present: Vec<&Entry> = slot.entries.iter().flatten().collect();
    present.is_empty()
        || present
            .iter()
            .any(|entry| filters.others.allows(entry, filters.context))
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn classify(
    key: &[String],
    slot: Slot,
    incomplete: &[BTreeSet<Vec<String>>],
    cancelled: bool,
    places: Where<'_>,
    options: &FolderMergeOptions,
    rules: Option<&dyn RulesComparer>,
    cancel: &Cancel,
) -> MergeRow {
    let rel = slot.rel.clone().unwrap_or_default();
    let [left, center, right, output] = slot.entries;
    let [left_facts, center_facts, right_facts, _] = slot.facts;
    let is_dir = [&left, &center, &right, &output]
        .iter()
        .any(|entry| entry.as_ref().is_some_and(|entry| entry.is_dir));
    let name = rel
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    let mut row = MergeRow {
        depth: key.len().saturating_sub(1),
        rel,
        name,
        is_dir,
        left,
        center,
        right,
        output,
        status: MergeStatus::Unknown,
        text: false,
        error: None,
        output_uncertain: cancelled || is_uncertain(&incomplete[OUTPUT], key),
        output_holds_excluded: false,
    };

    let has_center = places.bases.center.is_some();
    let panes: [(usize, Option<&Entry>); 3] = [
        (LEFT, row.left.as_ref()),
        (CENTER, row.center.as_ref()),
        (RIGHT, row.right.as_ref()),
    ];
    let problem = panes.iter().find_map(|(position, entry)| {
        if *position == CENTER && !has_center {
            return None;
        }
        if let Some(message) = entry.and_then(|entry| entry.error.clone()) {
            return Some(message);
        }
        (entry.is_none() && (cancelled || is_uncertain(&incomplete[*position], key)))
            .then(|| "a listing here could not be read in full".to_owned())
    });
    if problem.is_some() {
        row.error = problem;
        return row;
    }
    if row.left.is_none() && row.center.is_none() && row.right.is_none() {
        row.status = MergeStatus::Unchanged;
        return row;
    }

    let left_path = places.of(LEFT, row.left.as_ref(), left_facts);
    let center_path = places.of(CENTER, row.center.as_ref(), center_facts);
    let right_path = places.of(RIGHT, row.right.as_ref(), right_facts);

    let left_vs_center = same_item(
        row.left.as_ref(),
        &left_path,
        row.center.as_ref(),
        &center_path,
        options,
        rules,
        cancel,
    );
    let right_vs_center = same_item(
        row.right.as_ref(),
        &right_path,
        row.center.as_ref(),
        &center_path,
        options,
        rules,
        cancel,
    );
    let (left_changed, right_changed) = match (&left_vs_center, &right_vs_center) {
        (Sameness::Unknown(message), _) | (_, Sameness::Unknown(message)) => {
            row.error = Some(message.clone());
            return row;
        }
        (left, right) => (*left == Sameness::Different, *right == Sameness::Different),
    };
    let change_of = |side: Option<&Entry>, ancestor: Option<&Entry>| match (side, ancestor) {
        (Some(_), None) => Change::Added,
        (None, Some(_)) => Change::Deleted,
        _ => Change::Modified,
    };
    row.status = match (left_changed, right_changed) {
        (false, false) => MergeStatus::Unchanged,
        (true, false) => MergeStatus::LeftChange(change_of(row.left.as_ref(), row.center.as_ref())),
        (false, true) => {
            MergeStatus::RightChange(change_of(row.right.as_ref(), row.center.as_ref()))
        }
        (true, true) => {
            match same_item(
                row.left.as_ref(),
                &left_path,
                row.right.as_ref(),
                &right_path,
                options,
                rules,
                cancel,
            ) {
                Sameness::Same => {
                    MergeStatus::SameChange(change_of(row.left.as_ref(), row.center.as_ref()))
                }
                Sameness::Unknown(message) => {
                    row.error = Some(message);
                    return row;
                }
                Sameness::Different => {
                    let texts = text_pair(&row, &left_path, &center_path, &right_path, options);
                    row.text = texts.is_some();
                    match texts {
                        Some((left, Some(base), right))
                            if ca_diff::merge3::merge3_text(
                                &left,
                                &base,
                                &right,
                                &options.text,
                            )
                            .is_clean() =>
                        {
                            MergeStatus::Mergeable
                        }
                        _ => MergeStatus::Conflict,
                    }
                }
            }
        }
    };
    row
}

/// Whether two items at the same path count as the same.
fn same_item(
    first: Option<&Entry>,
    first_path: &Place<'_>,
    second: Option<&Entry>,
    second_path: &Place<'_>,
    options: &FolderMergeOptions,
    rules: Option<&dyn RulesComparer>,
    cancel: &Cancel,
) -> Sameness {
    let (first, second) = match (first, second) {
        (None, None) => return Sameness::Same,
        (Some(first), Some(second)) => (first, second),
        _ => return Sameness::Different,
    };
    if first.is_dir != second.is_dir || first.is_link() != second.is_link() {
        return Sameness::Different;
    }
    if first.is_dir {
        return Sameness::Same;
    }
    let quick = quick_compare_with(
        first,
        first_path.facts(),
        second,
        second_path.facts(),
        &options.compare.quick,
    );
    let quick_same = quick.is_same();
    let content = &options.compare.content;
    if quick.is_unsettled() && (!content.enabled || first.is_link()) {
        return Sameness::Unknown(
            "neither listing states a size or a time the comparison can use, and no content \
             test runs"
                .to_owned(),
        );
    }
    if !content.enabled || (content.skip_if_quick_same && quick_same) || first.is_link() {
        return if quick_same {
            Sameness::Same
        } else {
            Sameness::Different
        };
    }
    match compare_places(first_path, second_path, content, rules, cancel) {
        Ok(outcome) if outcome.is_not_compared() => {
            Sameness::Unknown("the content is not stored on this computer".to_owned())
        }
        Ok(outcome) => {
            let quick_allows = content.override_quick || quick_same || quick.is_unsettled();
            if outcome.is_same(content.ignore_unimportant) && quick_allows {
                Sameness::Same
            } else {
                Sameness::Different
            }
        }
        Err(ContentError::Cancelled) => Sameness::Unknown("the comparison was stopped".to_owned()),
        Err(error) => Sameness::Unknown(error.to_string()),
    }
}

/// The three texts of a row whose two versions are text files, or `None`
/// when either version is not a readable text file.
fn text_pair(
    row: &MergeRow,
    left_path: &Place<'_>,
    center_path: &Place<'_>,
    right_path: &Place<'_>,
    options: &FolderMergeOptions,
) -> Option<(String, Option<String>, String)> {
    let is_file =
        |entry: Option<&Entry>| entry.is_some_and(|entry| !entry.is_dir && !entry.is_link());
    if !is_file(row.left.as_ref()) || !is_file(row.right.as_ref()) {
        return None;
    }
    let left = read_text(left_path, options.text_limit)?;
    let right = read_text(right_path, options.text_limit)?;
    let center = if is_file(row.center.as_ref()) {
        read_text(center_path, options.text_limit)
    } else {
        None
    };
    Some((left, center, right))
}

/// The file as text, or `None` for a file that is too large, binary or not
/// valid UTF-8.
fn read_text(path: &Place<'_>, limit: u64) -> Option<String> {
    let file = path.open()?;
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limit
        || ca_text::binary::looks_binary(&bytes)
    {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// Where the inputs of one comparison are read from.
#[derive(Clone, Copy)]
struct Where<'a> {
    bases: MergeBases<'a>,
    sources: Option<MergeSources<'a>>,
}

impl<'a> Where<'a> {
    /// Where the item `entry` of the input at `position` is read from.
    fn of(&self, position: usize, entry: Option<&Entry>, facts: EntryFacts) -> Place<'a> {
        let base = match position {
            LEFT => Some(self.bases.left),
            CENTER => self.bases.center,
            _ => Some(self.bases.right),
        };
        let (Some(base), Some(entry)) = (base, entry) else {
            return Place::Local {
                base: PathBuf::new(),
                rel: PathBuf::new(),
                facts,
            };
        };
        match self.sources.and_then(|sources| sources.of(position)) {
            Some(source) if !source.is_local_folder() => Place::Inside {
                source,
                rel: entry.rel.clone(),
                facts,
            },
            Some(source) => Place::Local {
                base: source.origin().to_path_buf(),
                rel: entry.rel.clone(),
                facts,
            },
            None => Place::Local {
                base: base.to_path_buf(),
                rel: entry.rel.clone(),
                facts,
            },
        }
    }
}

/// Where one version of a row is read from.
enum Place<'a> {
    /// A file under a local folder.
    Local {
        base: PathBuf,
        rel: PathBuf,
        facts: EntryFacts,
    },
    /// An entry inside a container or another source.
    Inside {
        source: &'a Source,
        rel: PathBuf,
        facts: EntryFacts,
    },
}

impl Place<'_> {
    /// What the listing stated about the entry's own precision.
    const fn facts(&self) -> EntryFacts {
        match self {
            Place::Local { facts, .. } | Place::Inside { facts, .. } => *facts,
        }
    }

    /// The entry opened for reading.
    fn open(&self) -> Option<Box<dyn Read + '_>> {
        match self {
            Place::Local { base, rel, .. } => std::fs::File::open(base.join(rel))
                .ok()
                .map(|file| Box::new(file) as Box<dyn Read>),
            Place::Inside { source, rel, .. } => {
                let path = crate::compare::source_path(source, rel).ok()?;
                source
                    .file_system()
                    .open(&path, &ca_vfs::Cancel::new())
                    .ok()
                    .map(|file| Box::new(file) as Box<dyn Read>)
            }
        }
    }
}

/// The content test over two versions, read through their sources where
/// either is not a local file.
fn compare_places(
    first: &Place<'_>,
    second: &Place<'_>,
    content: &crate::criteria::ContentTests,
    rules: Option<&dyn RulesComparer>,
    cancel: &Cancel,
) -> Result<crate::criteria::ContentOutcome, ContentError> {
    if let (
        Place::Local {
            base: first_base,
            rel: first_rel,
            ..
        },
        Place::Local {
            base: second_base,
            rel: second_rel,
            ..
        },
    ) = (first, second)
    {
        return compare_contents(
            &first_base.join(first_rel),
            &second_base.join(second_rel),
            content,
            rules,
            cancel,
        );
    }
    let resolve = |place: &Place<'_>| -> Result<(Source, ca_vfs::VfsPath), ContentError> {
        let (source, rel) = match place {
            Place::Local { base, rel, .. } => (Source::local(base), rel),
            Place::Inside { source, rel, .. } => ((*source).clone(), rel),
        };
        let path = crate::compare::source_path(&source, rel)?;
        Ok((source, path))
    };
    let (first_source, first_path) = resolve(first)?;
    let (second_source, second_path) = resolve(second)?;
    compare_source_contents_with(
        ContentSide {
            source: &first_source,
            path: &first_path,
            facts: EntryFacts::default(),
        },
        ContentSide {
            source: &second_source,
            path: &second_path,
            facts: EntryFacts::default(),
        },
        content,
        rules,
        cancel,
    )
}
