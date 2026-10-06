//! Behaviour tests for the three way folder comparison and its planner.

#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]
#![allow(clippy::panic)]
#![allow(
    clippy::disallowed_methods,
    reason = "test setup runs a platform tool directly"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::{
    compare3, left_for_person, output_written_after_inputs, plan_merge, Change, FolderMergeOptions,
    MergeBases, MergeFilters, MergeInputs, MergeRefused, MergeRequest, MergeRow, MergeStatus,
    MergeTree, Pane, Resolution,
};
use crate::cancel::Cancel;
use crate::criteria::{ContentMethod, ContentTests};
use crate::filter::{CaseSensitivity, FilterContext, NameFilters, OtherFilters};
use crate::ops::exec::{execute, ExecutionContext, ExecutionReport, Journaling};
use crate::ops::fsops::RealFs;
use crate::ops::plan::{Conflict, OperationOptions, OperationPlan, StepAction};
use crate::scan::{scan_with, EntryError, ScanOptions, ScanResult};

/// Four sibling folders in one temporary directory.
struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        for side in ["left", "center", "right", "output"] {
            std::fs::create_dir_all(dir.path().join(side)).unwrap();
        }
        Self { dir }
    }

    fn path(&self, side: &str) -> PathBuf {
        self.dir.path().join(side)
    }

    fn write(&self, side: &str, rel: &str, body: &[u8]) {
        let path = self.path(side).join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    /// The same file in every one of the named folders.
    fn everywhere(&self, sides: &[&str], rel: &str, body: &[u8]) {
        for side in sides {
            self.write(side, rel, body);
        }
    }

    fn bases(&self) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        (
            self.path("left"),
            self.path("center"),
            self.path("right"),
            self.path("output"),
        )
    }
}

fn scan(root: &Path) -> ScanResult {
    scan_with(root, &ScanOptions::default(), &Cancel::new(), &|_| {}).unwrap()
}

fn content_options() -> FolderMergeOptions {
    let mut options = FolderMergeOptions::default();
    options.compare.content = ContentTests {
        enabled: true,
        method: ContentMethod::Binary,
        skip_if_quick_same: false,
        ..ContentTests::default()
    };
    options
}

fn operation_options() -> OperationOptions {
    OperationOptions {
        // The trash is a machine-wide side effect, so tests remove outright.
        use_recycle_bin: false,
        buffer_size: 4096,
        ..OperationOptions::default()
    }
}

struct Scans {
    left: ScanResult,
    center: ScanResult,
    right: ScanResult,
    output: Option<ScanResult>,
}

fn scans(fixture: &Fixture) -> Scans {
    let output = fixture.path("output");
    Scans {
        left: scan(&fixture.path("left")),
        center: scan(&fixture.path("center")),
        right: scan(&fixture.path("right")),
        output: output.is_dir().then(|| scan(&output)),
    }
}

fn compare_with(
    fixture: &Fixture,
    scans: &Scans,
    options: &FolderMergeOptions,
    names: &NameFilters,
) -> MergeTree {
    let (left, center, right, output) = fixture.bases();
    compare3(
        MergeInputs {
            left: &scans.left,
            center: Some(&scans.center),
            right: &scans.right,
            output: scans.output.as_ref(),
        },
        MergeBases {
            left: &left,
            center: Some(&center),
            right: &right,
            output: &output,
        },
        options,
        MergeFilters {
            names,
            others: &OtherFilters::default(),
            context: &FilterContext::default(),
        },
        None,
        &Cancel::new(),
    )
}

fn compare(fixture: &Fixture) -> MergeTree {
    compare_with(
        fixture,
        &scans(fixture),
        &content_options(),
        &NameFilters::default(),
    )
}

fn plan(fixture: &Fixture, tree: &MergeTree, request: &MergeRequest<'_>) -> OperationPlan {
    let (left, center, right, output) = fixture.bases();
    plan_merge(
        tree,
        MergeBases {
            left: &left,
            center: Some(&center),
            right: &right,
            output: &output,
        },
        request,
        &operation_options(),
    )
    .unwrap()
}

fn automatic(overrides: &BTreeMap<PathBuf, Resolution>) -> MergeRequest<'_> {
    MergeRequest {
        overrides,
        selection: None,
        automatic: true,
    }
}

fn run(plan: &OperationPlan) -> ExecutionReport {
    let cancel = Cancel::new();
    execute(
        plan,
        &ExecutionContext::new(&RealFs, &cancel, Journaling::Disabled),
    )
}

fn status(tree: &MergeTree, rel: &str) -> MergeStatus {
    tree.rows
        .iter()
        .find(|row| row.rel == Path::new(rel))
        .unwrap_or_else(|| panic!("no row for {rel}"))
        .status
}

/// Every file under a folder with its bytes.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let rel = path.strip_prefix(root).unwrap().to_path_buf();
            if path.is_dir() {
                stack.push(path);
                out.insert(rel, Vec::new());
            } else if let Ok(bytes) = std::fs::read(&path) {
                out.insert(rel, bytes);
            }
        }
    }
    out
}

fn targets(plan: &OperationPlan) -> Vec<PathBuf> {
    plan.steps
        .iter()
        .map(|step| step.action.target().to_path_buf())
        .collect()
}

// ------------------------------------------------------------ classification

/// Listings that prove no size and hold no time settle nothing, so the quick
/// tests alone never call such an item unchanged, and a content test that
/// skips the pairs the quick tests call the same still reads it.
#[test]
fn an_item_the_quick_tests_cannot_settle_is_never_unchanged() {
    let fixture = Fixture::new();
    fixture.write("left", "a.txt", b"changed");
    fixture.write("center", "a.txt", b"base");
    fixture.write("right", "a.txt", b"base");
    let mut scans = scans(&fixture);
    for scan in [&mut scans.left, &mut scans.center, &mut scans.right] {
        let rel = PathBuf::from("a.txt");
        if let Some(entry) = scan.entries.get_mut(&rel) {
            entry.modified = None;
        }
        scan.facts.insert(
            rel,
            crate::source::EntryFacts {
                size_is_exact: false,
                ..crate::source::EntryFacts::default()
            },
        );
    }

    let quick_only = FolderMergeOptions::default();
    assert!(!quick_only.compare.content.enabled);
    let tree = compare_with(&fixture, &scans, &quick_only, &NameFilters::default());
    let row = tree
        .rows
        .iter()
        .find(|row| row.rel == Path::new("a.txt"))
        .unwrap();
    assert!(row.error.is_some(), "{row:?}");

    let mut content = FolderMergeOptions::default();
    content.compare.content = ContentTests {
        enabled: true,
        method: ContentMethod::Binary,
        skip_if_quick_same: true,
        ..ContentTests::default()
    };
    let tree = compare_with(&fixture, &scans, &content, &NameFilters::default());
    assert_eq!(
        status(&tree, "a.txt"),
        MergeStatus::LeftChange(Change::Modified)
    );
}

#[test]
fn the_ancestor_decides_between_an_addition_and_a_deletion() {
    let fixture = Fixture::new();
    fixture.everywhere(&["left", "center", "right"], "same.txt", b"same");
    fixture.write("left", "added-left.txt", b"new");
    fixture.everywhere(&["center", "left"], "deleted-right.txt", b"old");
    fixture.everywhere(&["center", "right"], "changed-left.txt", b"old");
    fixture.write("left", "changed-left.txt", b"new text");
    fixture.everywhere(&["center"], "both-deleted.txt", b"gone");
    fixture.everywhere(&["center"], "conflict.bin", b"\x00base\x00");
    fixture.write("left", "conflict.bin", b"\x00left\x00");
    fixture.write("right", "conflict.bin", b"\x00right\x00");
    fixture.everywhere(&["left", "right"], "both-added.txt", b"twin");

    let tree = compare(&fixture);
    assert_eq!(status(&tree, "same.txt"), MergeStatus::Unchanged);
    assert_eq!(
        status(&tree, "added-left.txt"),
        MergeStatus::LeftChange(Change::Added)
    );
    assert_eq!(
        status(&tree, "deleted-right.txt"),
        MergeStatus::RightChange(Change::Deleted)
    );
    assert_eq!(
        status(&tree, "changed-left.txt"),
        MergeStatus::LeftChange(Change::Modified)
    );
    assert_eq!(
        status(&tree, "both-deleted.txt"),
        MergeStatus::SameChange(Change::Deleted)
    );
    assert_eq!(status(&tree, "conflict.bin"), MergeStatus::Conflict);
    assert_eq!(
        status(&tree, "both-added.txt"),
        MergeStatus::SameChange(Change::Added)
    );
    let counts = tree.counts();
    assert_eq!(counts.conflicts, 1);
    assert_eq!(counts.unchanged, 1);
}

#[test]
fn text_changes_on_separate_lines_are_mergeable_and_overlapping_ones_conflict() {
    let fixture = Fixture::new();
    let base = b"one\ntwo\nthree\nfour\nfive\nsix\nseven\n";
    fixture.write("center", "apart.txt", base);
    fixture.write(
        "left",
        "apart.txt",
        b"ONE\ntwo\nthree\nfour\nfive\nsix\nseven\n",
    );
    fixture.write(
        "right",
        "apart.txt",
        b"one\ntwo\nthree\nfour\nfive\nsix\nSEVEN\n",
    );
    fixture.write("center", "clash.txt", base);
    fixture.write(
        "left",
        "clash.txt",
        b"one\nLEFT\nthree\nfour\nfive\nsix\nseven\n",
    );
    fixture.write(
        "right",
        "clash.txt",
        b"one\nRIGHT\nthree\nfour\nfive\nsix\nseven\n",
    );

    let tree = compare(&fixture);
    assert_eq!(status(&tree, "apart.txt"), MergeStatus::Mergeable);
    assert_eq!(status(&tree, "clash.txt"), MergeStatus::Conflict);
    let clash = &tree.rows[tree.find(Path::new("clash.txt")).unwrap()];
    assert!(clash.text, "a text conflict can open in the text merge");
}

#[test]
fn with_no_ancestor_every_one_sided_item_is_an_addition() {
    let fixture = Fixture::new();
    fixture.write("left", "only-left.txt", b"l");
    fixture.write("right", "only-right.txt", b"r");
    fixture.write("left", "both.txt", b"left");
    fixture.write("right", "both.txt", b"right");
    let scans = scans(&fixture);
    let (left, _, right, output) = fixture.bases();
    let tree = compare3(
        MergeInputs {
            left: &scans.left,
            center: None,
            right: &scans.right,
            output: scans.output.as_ref(),
        },
        MergeBases {
            left: &left,
            center: None,
            right: &right,
            output: &output,
        },
        &content_options(),
        MergeFilters {
            names: &NameFilters::default(),
            others: &OtherFilters::default(),
            context: &FilterContext::default(),
        },
        None,
        &Cancel::new(),
    );
    assert!(!tree.has_center);
    assert_eq!(
        status(&tree, "only-left.txt"),
        MergeStatus::LeftChange(Change::Added)
    );
    assert_eq!(
        status(&tree, "only-right.txt"),
        MergeStatus::RightChange(Change::Added)
    );
    assert_eq!(status(&tree, "both.txt"), MergeStatus::Conflict);
}

// ------------------------------------------------------------------ writing

#[test]
fn an_automatic_merge_writes_the_output_and_touches_no_input() {
    let fixture = Fixture::new();
    fixture.everywhere(&["left", "center", "right"], "same.txt", b"same");
    fixture.write("left", "sub/added.txt", b"from left");
    fixture.everywhere(&["center", "left"], "gone.txt", b"old");
    fixture.everywhere(&["left", "center", "right"], "edited.txt", b"old");
    fixture.write("right", "edited.txt", b"right edit");
    fixture.everywhere(&["center"], "conflict.bin", b"\x00base");
    fixture.write("left", "conflict.bin", b"\x00left");
    fixture.write("right", "conflict.bin", b"\x00right");
    std::fs::remove_dir_all(fixture.path("output")).unwrap();

    let before: Vec<_> = ["left", "center", "right"]
        .iter()
        .map(|side| snapshot(&fixture.path(side)))
        .collect();
    let tree = compare(&fixture);
    assert!(!tree.output_listed);
    let overrides = BTreeMap::new();
    let plan = plan(&fixture, &tree, &automatic(&overrides));
    let output = fixture.path("output");
    for target in targets(&plan) {
        assert!(
            target.starts_with(&output),
            "{target:?} is not in the output"
        );
    }
    let report = run(&plan);
    assert!(report.is_clean(), "{:?}", report.results);

    let after: Vec<_> = ["left", "center", "right"]
        .iter()
        .map(|side| snapshot(&fixture.path(side)))
        .collect();
    assert_eq!(before, after, "an input folder changed");
    let written = snapshot(&output);
    assert_eq!(written.get(Path::new("same.txt")).unwrap(), b"same");
    assert_eq!(
        written.get(&Path::new("sub").join("added.txt")).unwrap(),
        b"from left"
    );
    assert_eq!(written.get(Path::new("edited.txt")).unwrap(), b"right edit");
    assert!(!written.contains_key(Path::new("gone.txt")));
    assert!(
        !written.contains_key(Path::new("conflict.bin")),
        "a conflict was resolved with nobody deciding"
    );
}

#[test]
fn merging_into_the_left_folder_deletes_what_the_right_side_deleted() {
    let fixture = Fixture::new();
    fixture.everywhere(&["left", "center"], "gone.txt", b"old");
    fixture.everywhere(&["left", "center", "right"], "kept.txt", b"kept");
    let (left, center, right, _) = fixture.bases();
    let scanned = scans(&fixture);
    let scans = Scans {
        output: Some(scan(&left)),
        ..scanned
    };
    let tree = compare3(
        MergeInputs {
            left: &scans.left,
            center: Some(&scans.center),
            right: &scans.right,
            output: scans.output.as_ref(),
        },
        MergeBases {
            left: &left,
            center: Some(&center),
            right: &right,
            output: &left,
        },
        &content_options(),
        MergeFilters {
            names: &NameFilters::default(),
            others: &OtherFilters::default(),
            context: &FilterContext::default(),
        },
        None,
        &Cancel::new(),
    );
    let overrides = BTreeMap::new();
    let plan = plan_merge(
        &tree,
        MergeBases {
            left: &left,
            center: Some(&center),
            right: &right,
            output: &left,
        },
        &automatic(&overrides),
        &operation_options(),
    )
    .unwrap();
    assert_eq!(plan.steps.len(), 1, "{:?}", plan.steps);
    assert!(run(&plan).is_clean());
    assert!(!left.join("gone.txt").exists());
    assert_eq!(std::fs::read(left.join("kept.txt")).unwrap(), b"kept");
}

#[test]
fn a_take_resolves_a_conflict_the_automatic_merge_leaves_alone() {
    let fixture = Fixture::new();
    fixture.write("center", "conflict.bin", b"\x00base");
    fixture.write("left", "conflict.bin", b"\x00left");
    fixture.write("right", "conflict.bin", b"\x00right");
    let tree = compare(&fixture);
    let mut overrides = BTreeMap::new();
    overrides.insert(PathBuf::from("conflict.bin"), Resolution::Take(Pane::Right));
    let plan = plan(&fixture, &tree, &automatic(&overrides));
    assert!(run(&plan).is_clean());
    assert_eq!(
        std::fs::read(fixture.path("output").join("conflict.bin")).unwrap(),
        b"\x00right"
    );
}

/// The rows the plan writes nothing for are the rows a person still has to
/// merge: a mergeable and a conflicting row nobody resolved, inside the part
/// of the tree the merge takes in.
#[test]
fn the_rows_left_for_a_person_are_the_unresolved_ones_the_plan_skips() {
    let fixture = Fixture::new();
    fixture.write("left", "added.txt", b"from the left");
    let base = b"one\ntwo\nthree\nfour\nfive\nsix\nseven\n";
    fixture.write("center", "apart.txt", base);
    fixture.write(
        "left",
        "apart.txt",
        b"ONE\ntwo\nthree\nfour\nfive\nsix\nseven\n",
    );
    fixture.write(
        "right",
        "apart.txt",
        b"one\ntwo\nthree\nfour\nfive\nsix\nSEVEN\n",
    );
    fixture.write("center", "conflict.bin", b"\x00base");
    fixture.write("left", "conflict.bin", b"\x00left");
    fixture.write("right", "conflict.bin", b"\x00right");
    let tree = compare(&fixture);
    let names = |rows: Vec<&MergeRow>| -> Vec<PathBuf> {
        rows.into_iter().map(|row| row.rel.clone()).collect()
    };

    let none = BTreeMap::new();
    let request = automatic(&none);
    let left = names(left_for_person(&tree, &request));
    assert_eq!(
        left,
        vec![PathBuf::from("apart.txt"), PathBuf::from("conflict.bin")]
    );
    let planned = plan(&fixture, &tree, &request);
    assert!(planned
        .steps
        .iter()
        .any(|step| step.rel == Path::new("added.txt")));
    assert!(planned.steps.iter().all(|step| !left.contains(&step.rel)));

    let manual = MergeRequest {
        overrides: &none,
        selection: None,
        automatic: false,
    };
    assert_eq!(names(left_for_person(&tree, &manual)), left);

    let mut taken = BTreeMap::new();
    taken.insert(PathBuf::from("conflict.bin"), Resolution::Take(Pane::Right));
    assert_eq!(
        names(left_for_person(&tree, &automatic(&taken))),
        vec![PathBuf::from("apart.txt")]
    );

    let selection: BTreeSet<PathBuf> = [PathBuf::from("conflict.bin")].into_iter().collect();
    let selected = MergeRequest {
        overrides: &none,
        selection: Some(&selection),
        automatic: true,
    };
    assert_eq!(
        names(left_for_person(&tree, &selected)),
        vec![PathBuf::from("conflict.bin")]
    );
}

/// The output holds a merge by hand only where its file is not older than
/// any input item and is not a copy of one under the size and time test.
#[test]
fn an_output_file_counts_as_written_after_the_inputs_only_when_no_input_is_newer_or_the_same() {
    const T: u64 = 1_700_000_000;
    let fixture = Fixture::new();
    let stamp = |side: &str, rel: &str, body: &[u8], secs: u64| {
        fixture.write(side, rel, body);
        std::fs::File::options()
            .write(true)
            .open(fixture.path(side).join(rel))
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs))
            .unwrap();
    };
    for rel in [
        "later.txt",
        "older.txt",
        "copy.txt",
        "same_time.txt",
        "none.txt",
    ] {
        stamp("center", rel, b"one\ntwo\nthree\n", T);
        stamp("left", rel, b"ONE\ntwo\nthree\n", T + 4);
        stamp("right", rel, b"one\ntwo\nTHREE!\n", T + 8);
    }
    stamp("output", "later.txt", b"ONE\ntwo\nTHREE!\n", T + 30);
    stamp("output", "older.txt", b"ONE\ntwo\nTHREE!\n", T + 6);
    stamp("output", "copy.txt", b"one\ntwo\nTHREE!\n", T + 8);
    stamp("output", "same_time.txt", b"ONE\ntwo\nTHREE\n", T + 8);
    let tree = compare(&fixture);
    let written = |rel: &str| {
        let row = tree
            .rows
            .iter()
            .find(|row| row.rel == Path::new(rel))
            .unwrap();
        output_written_after_inputs(row, &crate::criteria::QuickTests::default())
    };
    assert!(written("later.txt"));
    assert!(!written("older.txt"), "the right item is newer");
    assert!(
        !written("copy.txt"),
        "the output holds the right item's copy"
    );
    assert!(written("same_time.txt"), "a time that matches is not older");
    assert!(!written("none.txt"), "the output holds nothing");
}

// ------------------------------------------------------------------- safety

/// A step the user did not approve never runs: a conflict with no chosen
/// side, a row outside the selection and, with the automatic merge off, every
/// row the user did not resolve all produce no step.
#[test]
fn only_approved_rows_reach_the_plan() {
    let fixture = Fixture::new();
    fixture.write("left", "chosen.txt", b"chosen");
    fixture.write("left", "unselected.txt", b"not chosen");
    fixture.write("center", "conflict.bin", b"\x00base");
    fixture.write("left", "conflict.bin", b"\x00left");
    fixture.write("right", "conflict.bin", b"\x00right");
    let tree = compare(&fixture);

    let overrides = BTreeMap::new();
    let selection: BTreeSet<PathBuf> = [PathBuf::from("chosen.txt")].into_iter().collect();
    let selected = plan(
        &fixture,
        &tree,
        &MergeRequest {
            overrides: &overrides,
            selection: Some(&selection),
            automatic: true,
        },
    );
    let rels: Vec<&Path> = selected
        .steps
        .iter()
        .map(|step| step.rel.as_path())
        .collect();
    assert_eq!(rels, vec![Path::new("chosen.txt")]);

    let manual = plan(
        &fixture,
        &tree,
        &MergeRequest {
            overrides: &overrides,
            selection: None,
            automatic: false,
        },
    );
    assert!(manual.steps.is_empty(), "{:?}", manual.steps);

    let everything = plan(&fixture, &tree, &automatic(&overrides));
    assert!(everything
        .steps
        .iter()
        .all(|step| step.rel != Path::new("conflict.bin")));
}

/// A listing that could not be read in full proves no deletion, whatever the
/// user asks for.
#[test]
fn a_scan_error_never_turns_into_a_deletion() {
    let fixture = Fixture::new();
    fixture.everywhere(&["center", "right", "output"], "sub/file.txt", b"body");
    std::fs::create_dir_all(fixture.path("left").join("sub")).unwrap();
    let mut scanned = scans(&fixture);
    // The left side could not read its folder, so its silence about the file
    // is not a deletion.
    scanned.left.errors.push(EntryError {
        rel: PathBuf::from("sub"),
        message: "access denied".to_owned(),
    });
    let tree = compare_with(
        &fixture,
        &scanned,
        &content_options(),
        &NameFilters::default(),
    );
    let rel = Path::new("sub").join("file.txt");
    assert_eq!(status(&tree, &rel.to_string_lossy()), MergeStatus::Unknown);

    let mut overrides = BTreeMap::new();
    overrides.insert(rel.clone(), Resolution::Take(Pane::Left));
    let plan = plan(&fixture, &tree, &automatic(&overrides));
    assert!(
        plan.steps.iter().all(|step| !step.action.is_destructive()),
        "{:?}",
        plan.steps
    );
    assert!(plan
        .skipped
        .iter()
        .any(|skip| skip.conflict == Some(Conflict::CounterpartUnreadable)));
    run(&plan);
    assert!(fixture.path("output").join(&rel).exists());
}

#[test]
fn a_cancelled_scan_refuses_to_merge() {
    let fixture = Fixture::new();
    fixture.write("center", "file.txt", b"body");
    let mut scanned = scans(&fixture);
    scanned.left.cancelled = true;
    let tree = compare_with(
        &fixture,
        &scanned,
        &content_options(),
        &NameFilters::default(),
    );
    let (left, center, right, output) = fixture.bases();
    let overrides = BTreeMap::new();
    let refused = plan_merge(
        &tree,
        MergeBases {
            left: &left,
            center: Some(&center),
            right: &right,
            output: &output,
        },
        &automatic(&overrides),
        &operation_options(),
    );
    assert_eq!(refused, Err(MergeRefused::ScanCancelled));
}

/// A path the session's filters leave out takes no part: neither the file nor
/// the folder that holds it is removed from the output.
#[test]
fn a_filter_never_turns_into_a_deletion() {
    let fixture = Fixture::new();
    fixture.everywhere(&["center", "right", "output"], "d/a.txt", b"a");
    fixture.everywhere(&["center", "right", "output"], "d/keep.log", b"log");
    fixture.everywhere(&["center", "right", "output"], "top.log", b"log");
    // The left side deleted the whole folder and the top level log.
    let names = NameFilters::parse("-*.log");
    let tree = compare_with(&fixture, &scans(&fixture), &content_options(), &names);
    assert!(tree.find(Path::new("top.log")).is_none());
    let overrides = BTreeMap::new();
    let plan = plan(&fixture, &tree, &automatic(&overrides));
    for step in &plan.steps {
        assert!(
            !step.action.target().to_string_lossy().ends_with(".log"),
            "a filtered item reached the plan: {:?}",
            step.action
        );
        assert!(
            !matches!(step.action, StepAction::DeleteDir { .. }),
            "a folder holding a filtered item is removed"
        );
    }
    assert!(run(&plan).is_clean());
    let output = fixture.path("output");
    assert!(!output.join("d").join("a.txt").exists());
    assert!(output.join("d").join("keep.log").exists());
    assert!(output.join("top.log").exists());
}

/// A plan never writes through a link in the output, and never copies a link
/// from an input, so nothing outside the base folders is reached.
#[test]
fn a_merge_never_reaches_through_a_junction() {
    let fixture = Fixture::new();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("precious.txt"), b"keep").unwrap();
    if !make_junction(&fixture.path("output").join("sub"), outside.path()) {
        return;
    }
    if !make_junction(&fixture.path("left").join("linked"), outside.path()) {
        return;
    }
    fixture.write("left", "sub/precious.txt", b"replacement");
    fixture.write("left", "sub/new.txt", b"new");

    let tree = compare(&fixture);
    let overrides = BTreeMap::new();
    let plan = plan(&fixture, &tree, &automatic(&overrides));
    let outside_text = outside.path().to_string_lossy().to_string();
    for step in &plan.steps {
        assert!(
            !step
                .action
                .target()
                .to_string_lossy()
                .contains(&outside_text),
            "step reaches outside: {:?}",
            step.action
        );
        assert!(
            step.rel != Path::new("linked"),
            "a link was copied: {:?}",
            step.action
        );
    }
    run(&plan);
    assert_eq!(
        std::fs::read(outside.path().join("precious.txt")).unwrap(),
        b"keep"
    );
    assert!(!outside.path().join("new.txt").exists());
}

#[test]
fn output_link_identity_ignores_case_when_checking_ancestors() {
    let fixture = Fixture::new();
    let outside = tempfile::tempdir().unwrap();
    let output_link = fixture.path("output").join("SUB");
    if !make_junction(&output_link, outside.path()) {
        return;
    }
    for side in ["left", "center", "right"] {
        fixture.write(side, "sub/clash.txt", side.as_bytes());
    }

    let options = FolderMergeOptions {
        case: CaseSensitivity::Sensitive,
        ..content_options()
    };
    let tree = compare_with(
        &fixture,
        &scans(&fixture),
        &options,
        &NameFilters::default(),
    );

    assert!(
        !tree.output_target_is_safe(Path::new("sub/clash.txt")),
        "the differently cased output junction must protect its descendants"
    );
}

/// A row whose path climbs out of the base folders produces no step, and no
/// step of any plan writes outside the output folder.
#[test]
fn a_path_that_climbs_out_produces_no_step() {
    let fixture = Fixture::new();
    fixture.write("left", "inside.txt", b"inside");
    let mut tree = compare(&fixture);
    let mut escaping: MergeRow = tree.rows[0].clone();
    escaping.rel = PathBuf::from("..").join("escaped.txt");
    tree.rows.push(escaping);
    let overrides = BTreeMap::new();
    let plan = plan(&fixture, &tree, &automatic(&overrides));
    assert!(plan.is_contained());
    let output = fixture.path("output");
    for target in targets(&plan) {
        assert!(
            crate::ops::plan::path_is_within(&output, &target),
            "{target:?} leaves the output"
        );
    }
    run(&plan);
    assert!(!fixture.dir.path().join("escaped.txt").exists());
}

/// Two names that differ only in case would share one output name on a file
/// system that ignores case, so neither is written.
#[test]
fn names_that_differ_only_in_case_are_refused() {
    let fixture = Fixture::new();
    fixture.write("left", "Readme.txt", b"left");
    fixture.write("right", "README.txt", b"right");
    let options = FolderMergeOptions {
        case: CaseSensitivity::Sensitive,
        ..content_options()
    };
    let tree = compare_with(
        &fixture,
        &scans(&fixture),
        &options,
        &NameFilters::default(),
    );
    let overrides = BTreeMap::new();
    let plan = plan(&fixture, &tree, &automatic(&overrides));
    assert!(plan.steps.is_empty(), "{:?}", plan.steps);
    assert_eq!(
        plan.skipped
            .iter()
            .filter(|skip| skip.conflict == Some(Conflict::DestinationIsAnotherSource))
            .count(),
        2
    );
}

/// A container input stores a DOS device name as it stands and lists it as a
/// usable row. A Windows path built from that name reaches the device, so the
/// merge never writes it into the output folder, and says why.
#[cfg(windows)]
#[test]
fn a_device_name_an_input_stores_is_never_written_into_the_output() {
    let fixture = Fixture::new();
    fixture.write("left", "plain.txt", b"plain");
    let mut scanned = scans(&fixture);
    let mut device = scanned.left.entries[Path::new("plain.txt")].clone();
    device.rel = PathBuf::from("nul");
    device.name = "nul".to_owned();
    scanned.left.entries.insert(PathBuf::from("nul"), device);
    let tree = compare_with(
        &fixture,
        &scanned,
        &content_options(),
        &NameFilters::default(),
    );
    let overrides = BTreeMap::new();
    let plan = plan(&fixture, &tree, &automatic(&overrides));

    let written = targets(&plan);
    assert!(
        written.iter().any(|target| target.ends_with("plain.txt")),
        "{written:?}"
    );
    assert!(
        written.iter().all(|target| !target.ends_with("nul")),
        "{written:?}"
    );
    let skip = plan
        .skipped
        .iter()
        .find(|skip| skip.path == Path::new("nul"))
        .unwrap_or_else(|| panic!("no reason among {:?}", plan.skipped));
    assert!(skip.reason.contains("device name"), "{}", skip.reason);
}

/// Create a directory link, returning false when the platform or the policy
/// refuses one.
fn make_junction(link: &Path, target: &Path) -> bool {
    #[cfg(windows)]
    {
        std::process::Command::new("cmd")
            .args([
                "/C",
                "mklink",
                "/J",
                &link.to_string_lossy(),
                &target.to_string_lossy(),
            ])
            .output()
            .is_ok_and(|output| output.status.success())
    }
    #[cfg(not(windows))]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
}
