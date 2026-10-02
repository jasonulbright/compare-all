//! One side of a comparison held in something other than a local folder.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use ca_fs::{
    compare_source_contents, quick_compare_with, scan_source, scan_with, Cancel, ContentError,
    ContentOutcome, ContentSide, ContentTests, Entry, EntryFacts, FileOps, Mount, QuickTests,
    Source, SourceKind, SourceOps, TimeFidelity,
};
use ca_vfs::{
    ArchiveOptions, Cancel as VfsCancel, Limits, LocalFs, Snapshot, SnapshotOptions, VfsPath,
};

/// The 22 bytes of an end of central directory record, which is a zip holding
/// nothing.
const EMPTY_ZIP: &[u8] = &[
    0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];

fn options() -> ArchiveOptions {
    ArchiveOptions::default()
}

/// A zip at `path` holding each named entry.
fn zip_with(path: &Path, entries: &[(&str, &[u8])]) -> Source {
    std::fs::write(path, EMPTY_ZIP).unwrap();
    let source = Source::archive(path, options()).unwrap();
    let cancel = VfsCancel::new();
    for (name, bytes) in entries {
        let mut reader = *bytes;
        source
            .file_system()
            .write_file(&VfsPath::parse(name).unwrap(), &mut reader, &cancel)
            .unwrap();
    }
    source
}

/// The bytes of a zip built in a temporary place.
fn zip_bytes(dir: &Path, name: &str, entries: &[(&str, &[u8])]) -> Vec<u8> {
    let path = dir.join(name);
    drop(zip_with(&path, entries));
    std::fs::read(&path).unwrap()
}

fn tree(root: &Path, files: &[(&str, &[u8])]) {
    for (name, bytes) in files {
        let path = root.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, bytes).unwrap();
    }
}

fn names(result: &ca_fs::ScanResult) -> Vec<String> {
    result
        .entries
        .keys()
        .map(|rel| rel.to_string_lossy().replace('\\', "/"))
        .collect()
}

// -- the local path ----------------------------------------------------------

#[test]
fn a_local_folder_scanned_as_a_source_matches_the_path_scanner() {
    let dir = tempfile::tempdir().unwrap();
    tree(
        dir.path(),
        &[("a.txt", b"one"), ("sub/b.txt", b"two"), ("sub/c.bin", b"")],
    );
    let options = ca_fs::ScanOptions::default();
    let cancel = Cancel::new();
    let direct = scan_with(dir.path(), &options, &cancel, &|_| {}).unwrap();
    let through = scan_source(&Source::local(dir.path()), &options, &cancel, &|_| {}).unwrap();

    assert_eq!(names(&direct), names(&through));
    for (rel, entry) in &direct.entries {
        let other = &through.entries[rel];
        assert_eq!(entry.size, other.size, "{}", rel.display());
        assert_eq!(entry.is_dir, other.is_dir);
        if !entry.is_dir {
            // A directory's stamp is flushed lazily on some platforms, so two
            // reads of the same directory may disagree by a fraction of a
            // second. A file's stamp is settled once it is closed.
            assert_eq!(entry.modified, other.modified, "{}", rel.display());
        }
        assert_eq!(entry.name, other.name);
    }
    assert!(through.facts.is_empty(), "a folder states nothing coarser");
}

/// The local scan keeps its own path, so the two entry points cost the same.
///
/// The budget is a ratio between two measurements taken in the same run, not a
/// wall clock figure, so it holds on any machine.
#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "wall-clock budget holds for release builds"
)]
fn the_local_scan_does_not_slow_down_when_it_goes_through_a_source() {
    let dir = tempfile::tempdir().unwrap();
    for branch in 0..40 {
        let sub = dir.path().join(format!("d{branch}"));
        std::fs::create_dir(&sub).unwrap();
        for leaf in 0..100 {
            std::fs::write(sub.join(format!("f{leaf}.txt")), b"x").unwrap();
        }
    }
    let options = ca_fs::ScanOptions::default();
    let cancel = Cancel::new();
    let source = Source::local(dir.path());

    // One untimed pass each, so neither figure carries the first read of the
    // directory metadata.
    drop(scan_with(dir.path(), &options, &cancel, &|_| {}).unwrap());
    drop(scan_source(&source, &options, &cancel, &|_| {}).unwrap());

    let start = std::time::Instant::now();
    let direct = scan_with(dir.path(), &options, &cancel, &|_| {}).unwrap();
    let direct_time = start.elapsed();

    let start = std::time::Instant::now();
    let through = scan_source(&source, &options, &cancel, &|_| {}).unwrap();
    let through_time = start.elapsed();

    println!("path entry point: {direct_time:?}");
    println!("source entry point: {through_time:?}");
    assert_eq!(direct.entries.len(), through.entries.len());
    assert_eq!(direct.entries.len(), 4_040);
    assert!(
        through_time < direct_time * 2 + std::time::Duration::from_millis(5),
        "path {direct_time:?}, source {through_time:?}"
    );
}

// -- containers as folders ---------------------------------------------------

#[test]
fn an_archive_lists_as_a_folder() {
    let dir = tempfile::tempdir().unwrap();
    let source = zip_with(
        &dir.path().join("a.zip"),
        &[("one.txt", b"alpha"), ("inner/two.txt", b"beta")],
    );
    let result = scan_source(
        &source,
        &ca_fs::ScanOptions::default(),
        &Cancel::new(),
        &|_| {},
    )
    .unwrap();
    let listed = names(&result);
    assert!(listed.contains(&"one.txt".to_owned()), "{listed:?}");
    assert!(listed.contains(&"inner/two.txt".to_owned()), "{listed:?}");
    assert_eq!(source.kind(), SourceKind::Archive);
}

#[test]
fn an_archive_compares_against_a_plain_folder() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("folder");
    std::fs::create_dir(&folder).unwrap();
    tree(&folder, &[("same.txt", b"payload"), ("other.txt", b"left")]);
    let archive = zip_with(
        &dir.path().join("a.zip"),
        &[("same.txt", b"payload"), ("other.txt", b"right")],
    );
    let local = Source::local(&folder);
    let tests = ContentTests::default();
    let cancel = Cancel::new();

    let same = VfsPath::parse("same.txt").unwrap();
    assert_eq!(
        compare_source_contents(side(&local, &same), side(&archive, &same), &tests, &cancel)
            .unwrap(),
        ContentOutcome::BinarySame
    );

    let other = VfsPath::parse("other.txt").unwrap();
    assert_eq!(
        compare_source_contents(
            side(&local, &other),
            side(&archive, &other),
            &tests,
            &cancel
        )
        .unwrap(),
        ContentOutcome::BinaryDifferences
    );
}

#[test]
fn two_archives_compare_against_each_other() {
    let dir = tempfile::tempdir().unwrap();
    let left = zip_with(&dir.path().join("l.zip"), &[("f.txt", b"payload")]);
    let right = zip_with(&dir.path().join("r.zip"), &[("f.txt", b"payload")]);
    let path = VfsPath::parse("f.txt").unwrap();
    assert_eq!(
        compare_source_contents(
            side(&left, &path),
            side(&right, &path),
            &ContentTests::default(),
            &Cancel::new()
        )
        .unwrap(),
        ContentOutcome::BinarySame
    );
}

#[test]
fn a_stored_checksum_that_disagrees_settles_the_pair_without_a_read() {
    let dir = tempfile::tempdir().unwrap();
    let left = zip_with(&dir.path().join("l.zip"), &[("f.txt", b"one")]);
    let right = zip_with(&dir.path().join("r.zip"), &[("f.txt", b"two")]);
    let path = VfsPath::parse("f.txt").unwrap();
    let left_facts = EntryFacts {
        crc32: Some(1),
        ..EntryFacts::default()
    };
    let right_facts = EntryFacts {
        crc32: Some(2),
        ..EntryFacts::default()
    };
    let outcome = compare_source_contents(
        ContentSide {
            source: &left,
            path: &path,
            facts: left_facts,
        },
        ContentSide {
            source: &right,
            path: &path,
            facts: right_facts,
        },
        &ContentTests::default(),
        &Cancel::new(),
    )
    .unwrap();
    assert_eq!(outcome, ContentOutcome::BinaryDifferences);

    // Two checksums that agree prove nothing, so the bytes are still read.
    let agreeing = EntryFacts {
        crc32: Some(7),
        ..EntryFacts::default()
    };
    let outcome = compare_source_contents(
        ContentSide {
            source: &left,
            path: &path,
            facts: agreeing,
        },
        ContentSide {
            source: &right,
            path: &path,
            facts: agreeing,
        },
        &ContentTests::default(),
        &Cancel::new(),
    )
    .unwrap();
    assert_eq!(outcome, ContentOutcome::BinaryDifferences);
}

#[test]
fn an_archive_inside_an_archive_opens_as_its_own_source() {
    let dir = tempfile::tempdir().unwrap();
    let inner = zip_bytes(dir.path(), "inner.zip", &[("deep.txt", b"nested")]);
    let outer = zip_with(&dir.path().join("outer.zip"), &[("inner.zip", &inner)]);

    let nested = outer
        .nested_archive(&VfsPath::parse("inner.zip").unwrap(), &VfsCancel::new())
        .unwrap();
    let result = scan_source(
        &nested,
        &ca_fs::ScanOptions::default(),
        &Cancel::new(),
        &|_| {},
    )
    .unwrap();
    assert_eq!(names(&result), vec!["deep.txt".to_owned()]);
}

#[test]
fn a_nesting_ceiling_of_zero_refuses_the_nested_container() {
    let dir = tempfile::tempdir().unwrap();
    let inner = zip_bytes(dir.path(), "inner.zip", &[("deep.txt", b"nested")]);
    let path = dir.path().join("outer.zip");
    drop(zip_with(&path, &[("inner.zip", &inner)]));

    let limited = Source::archive(
        &path,
        ArchiveOptions {
            limits: Limits {
                max_nesting_depth: 0,
                ..Limits::default()
            },
            ..ArchiveOptions::default()
        },
    )
    .unwrap();
    let error = limited
        .nested_archive(&VfsPath::parse("inner.zip").unwrap(), &VfsCancel::new())
        .unwrap_err();
    assert!(error.to_string().contains("nesting"), "{error}");
}

#[test]
fn an_entry_ceiling_stops_the_content_test_with_a_typed_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.zip");
    drop(zip_with(&path, &[("big.bin", &vec![7_u8; 64 * 1024])]));
    let limited = Source::archive(
        &path,
        ArchiveOptions {
            limits: Limits {
                max_entry_bytes: 16,
                ..Limits::default()
            },
            ..ArchiveOptions::default()
        },
    )
    .unwrap();
    let plain = zip_with(&dir.path().join("b.zip"), &[("big.bin", b"short")]);
    let entry = VfsPath::parse("big.bin").unwrap();
    let error = compare_source_contents(
        side(&limited, &entry),
        side(&plain, &entry),
        &ContentTests::default(),
        &Cancel::new(),
    )
    .unwrap_err();
    assert!(
        matches!(error, ContentError::Source { .. }),
        "expected a typed source failure, got {error}"
    );
}

// -- writing into a container ------------------------------------------------

#[test]
fn a_batch_of_copies_into_a_zip_rewrites_it_once() {
    let dir = tempfile::tempdir().unwrap();
    let container = dir.path().join("target.zip");
    let archive = zip_with(&container, &[("kept.txt", b"kept")]);
    let before = archive.as_archive().unwrap().rewrite_count();

    let ops = SourceOps::new(vec![Mount::of_archive(archive.clone())]);
    ops.open_batch();
    for name in ["one.txt", "two.txt", "three.txt"] {
        let temp = container.join(format!("{name}.part"));
        let mut writer = ops.create_new(&temp).unwrap();
        std::io::Write::write_all(&mut writer, name.as_bytes()).unwrap();
        writer.sync_data().unwrap();
        drop(writer);
        ops.rename(&temp, &container.join(name)).unwrap();
    }
    ops.commit_batch().unwrap();

    assert_eq!(
        archive.as_archive().unwrap().rewrite_count() - before,
        1,
        "one batch is one rewrite"
    );
    let listed = listing(&archive);
    for name in ["kept.txt", "one.txt", "two.txt", "three.txt"] {
        assert!(listed.contains(&name.to_owned()), "{listed:?}");
    }
    assert!(
        !listed.iter().any(|name| name.contains(".part")),
        "the temporary name is collapsed into the stored name: {listed:?}"
    );
}

#[test]
fn a_delete_inside_a_zip_goes_through_the_same_batch() {
    let dir = tempfile::tempdir().unwrap();
    let container = dir.path().join("target.zip");
    let archive = zip_with(&container, &[("gone.txt", b"x"), ("kept.txt", b"y")]);
    let before = archive.as_archive().unwrap().rewrite_count();

    let ops = SourceOps::new(vec![Mount::of_archive(archive.clone())]);
    ops.open_batch();
    ops.remove_file(&container.join("gone.txt")).unwrap();
    let temp = container.join("added.txt.part");
    let mut writer = ops.create_new(&temp).unwrap();
    std::io::Write::write_all(&mut writer, b"added").unwrap();
    writer.sync_data().unwrap();
    drop(writer);
    ops.rename(&temp, &container.join("added.txt")).unwrap();
    ops.commit_batch().unwrap();

    assert_eq!(archive.as_archive().unwrap().rewrite_count() - before, 1);
    let listed = listing(&archive);
    assert!(!listed.contains(&"gone.txt".to_owned()), "{listed:?}");
    assert!(listed.contains(&"kept.txt".to_owned()), "{listed:?}");
    assert!(listed.contains(&"added.txt".to_owned()), "{listed:?}");
}

#[test]
fn a_batch_that_is_never_committed_leaves_the_container_as_it_was() {
    let dir = tempfile::tempdir().unwrap();
    let container = dir.path().join("target.zip");
    let archive = zip_with(&container, &[("kept.txt", b"kept")]);
    let original = std::fs::read(&container).unwrap();

    let ops = SourceOps::new(vec![Mount::of_archive(archive.clone())]);
    ops.open_batch();
    let temp = container.join("new.txt.part");
    let mut writer = ops.create_new(&temp).unwrap();
    std::io::Write::write_all(&mut writer, b"never lands").unwrap();
    writer.sync_data().unwrap();
    drop(writer);
    assert_eq!(ops.pending(0), 1);
    drop(ops);

    assert_eq!(std::fs::read(&container).unwrap(), original);
    assert_eq!(listing(&archive), vec!["kept.txt".to_owned()]);
}

#[test]
fn a_cancelled_write_into_a_container_leaves_the_old_container_intact() {
    let dir = tempfile::tempdir().unwrap();
    let container = dir.path().join("target.zip");
    let archive = zip_with(&container, &[("kept.txt", b"kept")]);
    let original = std::fs::read(&container).unwrap();

    let ops = SourceOps::new(vec![Mount::of_archive(archive.clone())]);
    ops.open_batch();
    let temp = container.join("new.txt.part");
    let mut writer = ops.create_new(&temp).unwrap();
    std::io::Write::write_all(&mut writer, b"interrupted").unwrap();
    writer.sync_data().unwrap();
    drop(writer);
    ops.cancel();
    let error = ops.commit_batch().unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);

    assert_eq!(
        std::fs::read(&container).unwrap(),
        original,
        "an interrupted rewrite never replaces the container"
    );
}

// -- refusal while the plan is still a plan ----------------------------------

#[test]
fn a_read_only_container_refuses_a_write_before_anything_runs() {
    let dir = tempfile::tempdir().unwrap();
    let tar = dir.path().join("a.tar");
    std::fs::write(&tar, tar_with("f.txt", b"payload")).unwrap();
    let source = Source::archive(&tar, options()).unwrap();
    assert!(!source.is_writable());
    assert!(ca_fs::write_refusal(&source).is_some());

    let ops = SourceOps::new(vec![Mount::of_archive(source)]);
    let Err(error) = ops.create_new(&tar.join("new.txt")) else {
        panic!("a read-only container took a write");
    };
    assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
}

/// A Debian package whose data member is a plain tar holding one file.
fn deb_with(name: &str, content: &[u8]) -> Vec<u8> {
    let mut out = b"!<arch>\n".to_vec();
    for (member, bytes) in [
        ("debian-binary", b"2.0\n".to_vec()),
        ("data.tar", tar_with(name, content)),
    ] {
        let header = format!(
            "{member:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
            0,
            0,
            0,
            100_644,
            bytes.len()
        );
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(&bytes);
        if out.len() % 2 == 1 {
            out.push(b'\n');
        }
    }
    out
}

#[test]
fn a_package_refuses_a_write_before_anything_runs() {
    let dir = tempfile::tempdir().unwrap();
    let deb = dir.path().join("a.deb");
    std::fs::write(&deb, deb_with("f.txt", b"payload")).unwrap();
    let source = Source::archive(&deb, options()).unwrap();
    assert!(!source.is_writable());
    let reason = ca_fs::write_refusal(&source).unwrap();
    assert!(reason.contains("read-only"), "{reason}");

    let ops = SourceOps::new(vec![Mount::of_archive(source)]);
    let Err(error) = ops.create_new(&deb.join("data.tar").join("new.txt")) else {
        panic!("a read-only container took a write");
    };
    assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
}

#[test]
fn a_plan_loses_the_steps_that_name_a_source_which_cannot_run_them() {
    let dir = tempfile::tempdir().unwrap();
    let capture = capture_snapshot(dir.path(), &[("f.txt", b"payload")]);
    let snapshot = Source::snapshot(&capture).unwrap();
    let mount = Mount::of_archive(snapshot);

    let mut plan = ca_fs::OperationPlan::new(
        ca_fs::OperationKind::Copy,
        vec![capture.clone()],
        ca_fs::OperationOptions::default(),
    );
    plan.steps.push(ca_fs::PlanStep {
        index: 0,
        action: ca_fs::StepAction::CopyFile {
            source: dir.path().join("elsewhere/f.txt"),
            target: capture.join("f.txt"),
        },
        bytes: 7,
        conflicts: Vec::new(),
        backup: None,
        side: None,
        rel: PathBuf::from("f.txt"),
        expected: ca_fs::StepExpectation::default(),
    });

    ca_fs::refuse_unsupported_targets(&mut plan, &[mount]);
    assert!(plan.steps.is_empty(), "the step never reaches execution");
    let refusals = plan.refusals();
    assert_eq!(refusals.len(), 1);
    assert_eq!(refusals[0].conflict, Some(ca_fs::Conflict::TargetReadOnly));
}

// -- recorded listings -------------------------------------------------------

#[test]
fn a_snapshot_lists_but_holds_no_content() {
    let dir = tempfile::tempdir().unwrap();
    let capture = capture_snapshot(dir.path(), &[("a.txt", b"one"), ("sub/b.txt", b"two")]);
    let snapshot = Source::snapshot(&capture).unwrap();
    assert_eq!(snapshot.kind(), SourceKind::Snapshot);
    assert!(!snapshot.has_content());

    let result = scan_source(
        &snapshot,
        &ca_fs::ScanOptions::default(),
        &Cancel::new(),
        &|_| {},
    )
    .unwrap();
    let listed = names(&result);
    assert!(listed.contains(&"a.txt".to_owned()), "{listed:?}");
    assert!(listed.contains(&"sub/b.txt".to_owned()), "{listed:?}");

    let other = zip_with(&dir.path().join("b.zip"), &[("a.txt", b"one")]);
    let path = VfsPath::parse("a.txt").unwrap();
    let error = compare_source_contents(
        side(&snapshot, &path),
        side(&other, &path),
        &ContentTests::default(),
        &Cancel::new(),
    )
    .unwrap_err();
    assert!(
        matches!(error, ContentError::ContentNotStored { .. }),
        "expected the typed no-content failure, got {error}"
    );
}

#[test]
fn the_quick_tests_still_settle_a_snapshot_against_a_folder() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("folder");
    std::fs::create_dir(&folder).unwrap();
    tree(&folder, &[("a.txt", b"one")]);
    let capture = Snapshot::capture(
        &LocalFs::new(&folder),
        &VfsPath::root(),
        SnapshotOptions::default(),
        &VfsCancel::new(),
    )
    .unwrap();
    let file = dir.path().join("capture.cass");
    capture.save(&file).unwrap();

    let snapshot = Source::snapshot(&file).unwrap();
    let listed = scan_source(
        &snapshot,
        &ca_fs::ScanOptions::default(),
        &Cancel::new(),
        &|_| {},
    )
    .unwrap();
    let live = scan_source(
        &Source::local(&folder),
        &ca_fs::ScanOptions::default(),
        &Cancel::new(),
        &|_| {},
    )
    .unwrap();

    let rel = Path::new("a.txt");
    let result = quick_compare_with(
        &live.entries[rel],
        live.facts_of(rel),
        &listed.entries[rel],
        listed.facts_of(rel),
        &QuickTests::default(),
    );
    assert!(result.is_same(), "{result:?}");
}

// -- a source that states less than a local listing does ---------------------

#[test]
fn an_inexact_size_never_settles_a_pair_on_its_own() {
    let left = entry("f.txt", 100, 0);
    let right = entry("f.txt", 200, 0);
    let exact = EntryFacts::default();
    let inexact = EntryFacts {
        size_is_exact: false,
        ..EntryFacts::default()
    };
    let tests = QuickTests {
        timestamp: false,
        ..QuickTests::default()
    };
    assert!(!quick_compare_with(&left, exact, &right, exact, &tests).is_same());
    let unproven = quick_compare_with(&left, inexact, &right, exact, &tests);
    assert!(
        unproven.differences.is_empty(),
        "a size the source could not prove is not evidence of a difference"
    );
    assert!(
        unproven.is_unsettled() && !unproven.is_same(),
        "nor is it evidence of a match"
    );
}

#[test]
fn a_coarse_time_stamp_is_compared_at_its_own_precision() {
    let left = entry("f.txt", 1, 1_000);
    let right = entry("f.txt", 1, 1_030);
    let exact = EntryFacts::default();
    let minute = EntryFacts {
        time_fidelity: TimeFidelity::MinutePrecision,
        ..EntryFacts::default()
    };
    let tests = QuickTests {
        size: false,
        ..QuickTests::default()
    };
    assert!(!quick_compare_with(&left, exact, &right, exact, &tests).is_same());
    assert!(
        quick_compare_with(&left, exact, &right, minute, &tests).is_same(),
        "the coarser side sets the tolerance"
    );

    let far = entry("f.txt", 1, 1_000 + 120);
    assert!(!quick_compare_with(&left, exact, &far, minute, &tests).is_same());
}

// -- helpers -----------------------------------------------------------------

fn side<'a>(source: &'a Source, path: &'a VfsPath) -> ContentSide<'a> {
    ContentSide {
        source,
        path,
        facts: EntryFacts::default(),
    }
}

fn listing(source: &Source) -> Vec<String> {
    let result = scan_source(
        source,
        &ca_fs::ScanOptions::default(),
        &Cancel::new(),
        &|_| {},
    )
    .unwrap();
    names(&result)
}

fn entry(name: &str, size: u64, modified: i64) -> Entry {
    Entry {
        rel: PathBuf::from(name),
        name: name.to_owned(),
        is_dir: false,
        size,
        modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(modified.unsigned_abs())),
        created: None,
        attributes: ca_fs::Attributes::default(),
        link: None,
        listing_incomplete: false,
        error: None,
        refused: false,
    }
}

fn capture_snapshot(dir: &Path, files: &[(&str, &[u8])]) -> PathBuf {
    let folder = dir.join("captured");
    std::fs::create_dir_all(&folder).unwrap();
    tree(&folder, files);
    let capture = Snapshot::capture(
        &LocalFs::new(&folder),
        &VfsPath::root(),
        SnapshotOptions::default(),
        &VfsCancel::new(),
    )
    .unwrap();
    let path = dir.join("capture.cass");
    capture.save(&path).unwrap();
    path
}

/// A tar holding one file, built by hand so no writer is needed.
fn tar_with(name: &str, content: &[u8]) -> Vec<u8> {
    let mut header = [0_u8; 512];
    let name_bytes = name.as_bytes();
    header[..name_bytes.len()].copy_from_slice(name_bytes);
    write_octal(&mut header[100..108], 0o644);
    write_octal(&mut header[108..116], 0);
    write_octal(&mut header[116..124], 0);
    write_octal(&mut header[124..136], content.len() as u64);
    write_octal(&mut header[136..148], 0);
    header[148..156].fill(b' ');
    header[156] = b'0';
    header[257..262].copy_from_slice(b"ustar");
    header[263..265].copy_from_slice(b"00");
    let checksum: u32 = header.iter().map(|byte| u32::from(*byte)).sum();
    write_octal(&mut header[148..154], u64::from(checksum));
    header[154] = 0;
    header[155] = b' ';

    let mut out = header.to_vec();
    out.extend_from_slice(content);
    out.resize(out.len().div_ceil(512) * 512, 0);
    out.extend(std::iter::repeat_n(0_u8, 1024));
    out
}

fn write_octal(field: &mut [u8], value: u64) {
    let text = format!("{value:o}");
    let width = field.len() - 1;
    let padded = format!("{text:0>width$}");
    field[..width].copy_from_slice(&padded.as_bytes()[padded.len() - width..]);
    field[width] = 0;
}

#[test]
fn a_tree_of_a_folder_and_an_archive_takes_its_content_results_from_both() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("folder");
    std::fs::create_dir(&folder).unwrap();
    tree(
        &folder,
        &[("same.txt", b"payload"), ("sub/other.txt", b"left!")],
    );
    let archive = zip_with(
        &dir.path().join("a.zip"),
        &[("same.txt", b"payload"), ("sub/other.txt", b"right")],
    );
    let local = Source::local(&folder);
    let cancel = Cancel::new();
    let scan_options = ca_fs::ScanOptions::default();
    let left = scan_source(&local, &scan_options, &cancel, &|_| {}).unwrap();
    let right = scan_source(&archive, &scan_options, &cancel, &|_| {}).unwrap();
    let mut root = ca_fs::align_trees(&left, &right, &ca_fs::AlignmentOptions::default(), &cancel);
    let mut options = ca_fs::CompareOptions::default();
    options.content.enabled = true;
    options.content.method = ca_fs::ContentMethod::Binary;
    options.content.skip_if_quick_same = false;
    assert!(ca_fs::compare_source_contents_parallel(
        &mut root,
        &local,
        &archive,
        &options,
        None,
        &cancel,
        &|_| {}
    ));
    let mut outcomes = Vec::new();
    root.walk(&mut |node| {
        if !node.is_dir {
            outcomes.push((
                node.rel.to_string_lossy().replace('\\', "/"),
                node.content,
                node.error.clone(),
            ));
        }
    });
    outcomes.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        outcomes,
        vec![
            (
                "same.txt".to_owned(),
                Some(ContentOutcome::BinarySame),
                None
            ),
            (
                "sub/other.txt".to_owned(),
                Some(ContentOutcome::BinaryDifferences),
                None
            ),
        ]
    );
}

#[test]
fn the_rules_method_reaches_the_rules_engine_when_a_side_is_an_archive() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("folder");
    std::fs::create_dir(&folder).unwrap();
    tree(&folder, &[("notes.txt", b"one  two\n")]);
    let archive = zip_with(&dir.path().join("a.zip"), &[("notes.txt", b"one two\n")]);
    let local = Source::local(&folder);
    let cancel = Cancel::new();
    let scan_options = ca_fs::ScanOptions::default();
    let left = scan_source(&local, &scan_options, &cancel, &|_| {}).unwrap();
    let right = scan_source(&archive, &scan_options, &cancel, &|_| {}).unwrap();
    let mut root = ca_fs::align_trees(&left, &right, &ca_fs::AlignmentOptions::default(), &cancel);
    let mut options = ca_fs::CompareOptions::default();
    options.content.enabled = true;
    options.content.method = ca_fs::ContentMethod::Rules;
    options.content.skip_if_quick_same = false;
    let engine = ca_fs::RulesEngine::with_builtin_formats();
    assert!(ca_fs::compare_source_contents_parallel(
        &mut root,
        &local,
        &archive,
        &options,
        Some(&engine),
        &cancel,
        &|_| {}
    ));
    let mut outcome = None;
    root.walk(&mut |node| {
        if !node.is_dir {
            outcome = node.content;
        }
    });
    assert_eq!(outcome, Some(ContentOutcome::UnimportantDifferences));
}

// -- stored names a Windows path does not reach ------------------------------

/// Every row of a tree with a container on the left and a folder on the right,
/// with the options the planners get.
struct ContainerAndFolder {
    _dir: tempfile::TempDir,
    container: PathBuf,
    folder: PathBuf,
    target: PathBuf,
    tree: ca_fs::Node,
}

impl ContainerAndFolder {
    fn new(file_name: &str, bytes: &[u8]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let container = dir.path().join(file_name);
        std::fs::write(&container, bytes).unwrap();
        Self::open(dir, container)
    }

    #[cfg(windows)]
    fn zip(entries: &[(&str, &[u8])]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let container = dir.path().join("a.zip");
        drop(zip_with(&container, entries));
        Self::open(dir, container)
    }

    fn open(dir: tempfile::TempDir, container: PathBuf) -> Self {
        let folder = dir.path().join("folder");
        let target = dir.path().join("target");
        std::fs::create_dir(&folder).unwrap();
        std::fs::create_dir(&target).unwrap();
        let archive = Source::archive(&container, options()).unwrap();
        let cancel = Cancel::new();
        let scan_options = ca_fs::ScanOptions::default();
        let left = scan_source(&archive, &scan_options, &cancel, &|_| {}).unwrap();
        let right = scan_with(&folder, &scan_options, &cancel, &|_| {}).unwrap();
        let tree = ca_fs::align_trees(&left, &right, &ca_fs::AlignmentOptions::default(), &cancel);
        Self {
            _dir: dir,
            container,
            folder,
            target,
            tree,
        }
    }

    fn row(&self, name: &str) -> &ca_fs::Node {
        self.tree
            .children
            .iter()
            .find(|node| node.rel == Path::new(name))
            .unwrap_or_else(|| panic!("no row {name}"))
    }

    /// The plans of every operation that writes the left rows into a folder.
    fn plans(&self) -> Vec<ca_fs::OperationPlan> {
        let chosen: std::collections::BTreeSet<PathBuf> = self
            .tree
            .children
            .iter()
            .map(|node| node.rel.clone())
            .collect();
        let selection = ca_fs::resolve_selection(&self.tree, &chosen);
        let bases = ca_fs::Bases {
            left: &self.container,
            right: &self.folder,
        };
        let options = ca_fs::OperationOptions {
            use_recycle_bin: false,
            ..ca_fs::OperationOptions::default()
        };
        vec![
            ca_fs::plan_copy(&selection, ca_fs::Side::Left, bases, &options),
            ca_fs::plan_move(&selection, ca_fs::Side::Left, bases, &options),
            ca_fs::plan_to_folder(
                &selection,
                ca_fs::Side::Left,
                bases,
                &self.target,
                ca_fs::PathOption::Flatten,
                &options,
                false,
                &ca_fs::RealFs,
            ),
            ca_fs::plan_sync(
                &self.tree,
                &ca_fs::SyncPreset::MirrorToRight,
                bases,
                &options,
                &ca_fs::SyncPreview::default(),
            )
            .unwrap(),
        ]
    }
}

#[cfg(windows)]
fn writes_name(plan: &ca_fs::OperationPlan, name: &str) -> bool {
    plan.steps
        .iter()
        .any(|step| step.action.target().file_name() == Some(std::ffi::OsStr::new(name)))
}

/// A container stores a DOS device name as it stands, and the scan lists it
/// as a usable row. A Windows path built from such a name reaches the device,
/// so no plan writes it into a folder, and each plan says why.
#[cfg(windows)]
#[test]
fn a_container_entry_named_as_a_device_is_never_written_into_a_folder() {
    let sides = ContainerAndFolder::zip(&[
        ("nul", b"device"),
        ("con.txt", b"device too"),
        ("plain.txt", b"plain"),
    ]);
    for name in ["nul", "con.txt"] {
        let row = sides.row(name);
        assert!(
            row.left.as_ref().is_some_and(|entry| !entry.refused),
            "{name} reaches the planner as a usable row"
        );
    }
    for plan in sides.plans() {
        assert!(writes_name(&plan, "plain.txt"), "{:?}", plan.steps);
        for name in ["nul", "con.txt"] {
            assert!(!writes_name(&plan, name), "{name}: {:?}", plan.steps);
            let skip = plan
                .skipped
                .iter()
                .find(|skip| skip.path == Path::new(name))
                .unwrap_or_else(|| panic!("{name}: no reason among {:?}", plan.skipped));
            assert!(
                skip.reason.contains(&format!("{name:?}")),
                "{}",
                skip.reason
            );
            assert!(skip.reason.contains("device name"), "{}", skip.reason);
        }
    }
}

/// A stored name that ends with a dot or a space lists as a refused row on
/// every platform, so no planner receives it as a usable row.
#[test]
fn a_container_entry_whose_name_ends_with_a_dot_or_a_space_never_reaches_a_step() {
    for (stored, listed) in [("dot.", "dot_"), ("space ", "space_")] {
        let sides = ContainerAndFolder::new("a.tar", &tar_with(stored, b"payload"));
        let row = sides.row(listed);
        assert!(
            row.left.as_ref().is_some_and(|entry| entry.refused),
            "{stored:?} is listed refused"
        );
        for plan in sides.plans() {
            assert!(plan.steps.is_empty(), "{stored:?}: {:?}", plan.steps);
            let skip = plan
                .skipped
                .iter()
                .find(|skip| skip.path == Path::new(listed))
                .unwrap_or_else(|| panic!("{stored:?}: no reason among {:?}", plan.skipped));
            assert!(
                skip.reason.contains(&format!("{stored:?}")),
                "{}",
                skip.reason
            );
        }
    }
}

/// A rename that must not replace refuses a taken name inside a container as
/// it does on a disk, also while a batch is open.
#[test]
fn a_rename_that_must_not_replace_refuses_a_taken_name_inside_a_container() {
    let dir = tempfile::tempdir().unwrap();
    let container = dir.path().join("a.zip");
    let archive = zip_with(&container, &[("a.txt", b"a"), ("b.txt", b"b")]);
    let ops = SourceOps::new(vec![Mount::of_archive(archive.clone())]);
    ops.open_batch();
    let error = ops
        .rename_no_replace(&container.join("a.txt"), &container.join("b.txt"))
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists, "{error}");
    ops.rename_no_replace(&container.join("a.txt"), &container.join("c.txt"))
        .unwrap();
    ops.commit_batch().unwrap();

    assert_eq!(
        listing(&archive),
        vec!["b.txt".to_owned(), "c.txt".to_owned()]
    );
    let mut kept = Vec::new();
    let mut reader = archive
        .file_system()
        .open(&VfsPath::parse("b.txt").unwrap(), &VfsCancel::new())
        .unwrap();
    std::io::Read::read_to_end(&mut reader, &mut kept).unwrap();
    assert_eq!(kept, b"b");
}

/// A new file inside a container is written under a temporary name that
/// collapses into the stored name. A taken name is refused before anything
/// is queued.
#[test]
fn a_new_file_inside_a_container_refuses_a_taken_name_and_stores_no_temporary() {
    let dir = tempfile::tempdir().unwrap();
    let container = dir.path().join("a.zip");
    let archive = zip_with(&container, &[("a.txt", b"a")]);
    let ops = SourceOps::new(vec![Mount::of_archive(archive.clone())]);
    ops.open_batch();
    let error = ops
        .write_new(&container.join("a.txt"), &mut &b"other"[..], 4096)
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists, "{error}");
    assert_eq!(ops.pending(0), 0);
    ops.write_new(&container.join("a.txt.bak"), &mut &b"a"[..], 4096)
        .unwrap();
    ops.commit_batch().unwrap();

    assert_eq!(
        listing(&archive),
        vec!["a.txt".to_owned(), "a.txt.bak".to_owned()]
    );
    for name in ["a.txt", "a.txt.bak"] {
        let mut stored = Vec::new();
        let mut reader = archive
            .file_system()
            .open(&VfsPath::parse(name).unwrap(), &VfsCancel::new())
            .unwrap();
        std::io::Read::read_to_end(&mut reader, &mut stored).unwrap();
        assert_eq!(stored, b"a", "{name}");
    }
}

/// A delete in an open batch cancels a write the batch queued for the same
/// path, and every write it queued under a deleted folder. A step that fails
/// after it wrote its temporary removes the temporary this way.
#[test]
fn a_delete_in_an_open_batch_cancels_a_write_queued_for_the_same_path() {
    let dir = tempfile::tempdir().unwrap();
    let container = dir.path().join("a.zip");
    let archive = zip_with(&container, &[("kept.txt", b"kept"), ("old/x.txt", b"x")]);
    let ops = SourceOps::new(vec![Mount::of_archive(archive.clone())]);
    ops.open_batch();
    for name in ["new.txt.part", "old/y.txt"] {
        let mut writer = ops.create_new(&container.join(name)).unwrap();
        std::io::Write::write_all(&mut writer, b"queued").unwrap();
        writer.sync_data().unwrap();
        drop(writer);
    }
    ops.remove_file(&container.join("new.txt.part")).unwrap();
    ops.remove_dir(&container.join("old")).unwrap();
    ops.commit_batch().unwrap();

    assert_eq!(listing(&archive), vec!["kept.txt".to_owned()]);
}

/// The stored content of every entry of `source`, by name.
fn stored(source: &Source) -> Vec<(String, Vec<u8>)> {
    listing(source)
        .into_iter()
        .map(|name| {
            let mut bytes = Vec::new();
            let mut reader = source
                .file_system()
                .open(&VfsPath::parse(&name).unwrap(), &VfsCancel::new())
                .unwrap();
            std::io::Read::read_to_end(&mut reader, &mut bytes).unwrap();
            (name, bytes)
        })
        .collect()
}

/// A case-only rename inside a zip, run through the batch a script opens:
/// `b.txt` to `B.txt` alone, and onto a sibling `B.txt` with the backup
/// option. A zip keeps case, so the entry lands under the new name, and the
/// sibling it replaces is saved first.
#[test]
fn a_case_only_rename_inside_a_zip_lands_under_the_new_name() {
    for sibling in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let container = dir.path().join("a.zip");
        let right = dir.path().join("right");
        std::fs::create_dir(&right).unwrap();
        let mut entries: Vec<(&str, &[u8])> = vec![("b.txt", b"lower")];
        if sibling {
            entries.push(("B.txt", b"UPPER"));
        }
        let archive = zip_with(&container, &entries);
        let cancel = Cancel::new();
        let left = scan_source(&archive, &ca_fs::ScanOptions::default(), &cancel, &|_| {}).unwrap();
        let alignment = ca_fs::AlignmentOptions {
            case: ca_fs::CaseSensitivity::Sensitive,
            ..ca_fs::AlignmentOptions::default()
        };
        let mut root =
            ca_fs::align_trees(&left, &ca_fs::ScanResult::default(), &alignment, &cancel);
        ca_fs::compare_quick(&mut root, &ca_fs::CompareOptions::default());
        let chosen = [PathBuf::from("b.txt")].into_iter().collect();
        let options = ca_fs::OperationOptions {
            backup: Some(ca_fs::BackupOptions::default()),
            ..ca_fs::OperationOptions::default()
        };
        let plan = ca_fs::plan_rename(
            &root,
            &ca_fs::resolve_selection(&root, &chosen),
            ca_fs::Sides::Left,
            ca_fs::Bases {
                left: &container,
                right: &right,
            },
            &ca_fs::RenameAction::Mask("B.txt".to_owned()),
            &options,
        )
        .unwrap();
        let ops = SourceOps::new(vec![Mount::of_archive(archive.clone())]);
        ops.open_batch();
        let report = ca_fs::execute(
            &plan,
            &ca_fs::ExecutionContext::new(&ops, &cancel, ca_fs::Journaling::Disabled),
        );
        ops.commit_batch().unwrap();

        assert!(report.is_clean(), "sibling {sibling}: {:?}", report.results);
        let mut expected = vec![("B.txt".to_owned(), b"lower".to_vec())];
        if sibling {
            expected.push(("B.txt.bak".to_owned(), b"UPPER".to_vec()));
        }
        assert_eq!(stored(&archive), expected, "sibling {sibling}");
    }
}
