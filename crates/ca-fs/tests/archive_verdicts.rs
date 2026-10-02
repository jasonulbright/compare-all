//! Folder comparisons of containers that hold names no Windows path can
//! carry, or a file and a directory under one name.

#![allow(clippy::unwrap_used, clippy::panic, clippy::cast_possible_truncation)]

use std::path::{Path, PathBuf};

use ca_fs::{
    align_trees, compare_quick, compare_source_contents_parallel, scan_source, AlignmentOptions,
    ArchiveTypes, Cancel, CompareOptions, ContentMethod, Limits, NodeStatus, ScanOptions, Source,
};

/// A zip holding `entries` stored as they are, so no writer can change a
/// name on the way in. A name that ends with a slash is a directory record.
/// Every record carries the DOS stamp of 1980-01-01.
fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in entries {
        let crc = crc32fast::hash(data);
        let offset = out.len() as u32;
        let size = data.len() as u32;
        let name_len = name.len() as u16;
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        for field in [20u16, 0, 0, 0, 0x21] {
            out.extend_from_slice(&field.to_le_bytes());
        }
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&name_len.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);

        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        for field in [20u16, 20, 0, 0, 0, 0x21] {
            central.extend_from_slice(&field.to_le_bytes());
        }
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&name_len.to_le_bytes());
        for field in [0u16, 0, 0, 0] {
            central.extend_from_slice(&field.to_le_bytes());
        }
        let external: u32 = if name.ends_with('/') { 0x10 } else { 0 };
        central.extend_from_slice(&external.to_le_bytes());
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let central_offset = out.len() as u32;
    let central_size = central.len() as u32;
    out.extend_from_slice(&central);
    let count = entries.len() as u16;
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    for field in [0u16, 0, count, count] {
        out.extend_from_slice(&field.to_le_bytes());
    }
    out.extend_from_slice(&central_size.to_le_bytes());
    out.extend_from_slice(&central_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

/// A tar holding `entries` under their names exactly as given.
fn tar_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, data) in entries {
        let mut header = [0u8; 512];
        header[..name.len()].copy_from_slice(name.as_bytes());
        header[100..108].copy_from_slice(b"0000644\0");
        header[108..116].copy_from_slice(b"0000000\0");
        header[116..124].copy_from_slice(b"0000000\0");
        header[124..136].copy_from_slice(format!("{:011o}\0", data.len()).as_bytes());
        header[136..148].copy_from_slice(b"13000000000\0");
        header[148..156].copy_from_slice(b"        ");
        header[156] = b'0';
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        let sum: u32 = header.iter().map(|byte| u32::from(*byte)).sum();
        header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        out.extend_from_slice(&header);
        out.extend_from_slice(data);
        out.resize(out.len().div_ceil(512) * 512, 0);
    }
    out.extend(std::iter::repeat_n(0u8, 1024));
    out
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

fn open(path: &Path) -> Source {
    Source::open(path, &ArchiveTypes::default(), &Limits::default()).unwrap()
}

/// The status of the root after the quick tests, and after the binary content
/// test of every pair when `content` is set.
fn verdict(left: &Path, right: &Path, content: bool) -> NodeStatus {
    let left = open(left);
    let right = open(right);
    let cancel = Cancel::new();
    let scanned_left = scan_source(&left, &ScanOptions::default(), &cancel, &|_| {}).unwrap();
    let scanned_right = scan_source(&right, &ScanOptions::default(), &cancel, &|_| {}).unwrap();
    let mut tree = align_trees(
        &scanned_left,
        &scanned_right,
        &AlignmentOptions::default(),
        &cancel,
    );
    let mut options = CompareOptions::default();
    options.content.enabled = content;
    options.content.method = ContentMethod::Binary;
    options.content.skip_if_quick_same = false;
    compare_quick(&mut tree, &options);
    assert!(compare_source_contents_parallel(
        &mut tree,
        &left,
        &right,
        &options,
        None,
        &cancel,
        &|_| {}
    ));
    tree.status
}

#[test]
fn two_tars_that_differ_only_in_an_entry_with_a_refused_name_cannot_be_compared() {
    let dir = tempfile::tempdir().unwrap();
    for (left_body, right_body) in [(b"AAAA" as &[u8], b"BBBBBBBB" as &[u8]), (b"AAAA", b"BBBB")] {
        let left = write(
            dir.path(),
            "a.tar",
            &tar_bytes(&[("ok.txt", b"x"), ("log-12:30.txt", left_body)]),
        );
        let right = write(
            dir.path(),
            "b.tar",
            &tar_bytes(&[("ok.txt", b"x"), ("log-12:30.txt", right_body)]),
        );
        for content in [false, true] {
            assert_eq!(
                verdict(&left, &right, content),
                NodeStatus::Error,
                "bodies {left_body:?} and {right_body:?}, content test {content}"
            );
        }
    }
}

#[test]
fn an_entry_with_a_refused_name_on_one_side_only_is_a_difference() {
    let dir = tempfile::tempdir().unwrap();
    let left = write(
        dir.path(),
        "a.zip",
        &zip_bytes(&[
            ("ok.txt", b"x"),
            ("notes/2024-01-01T12:30.log", b"AAAA"),
            ("trailing.", b"AAAA"),
        ]),
    );
    let right = write(dir.path(), "b.zip", &zip_bytes(&[("ok.txt", b"x")]));
    for content in [false, true] {
        assert_eq!(
            verdict(&left, &right, content),
            NodeStatus::Different,
            "content test {content}"
        );
    }
}

#[test]
fn two_zips_whose_file_differs_beside_its_namesake_directory_never_compare_as_same() {
    let dir = tempfile::tempdir().unwrap();
    for (left_body, right_body) in [(b"AAAA" as &[u8], b"BBBBBBBB" as &[u8]), (b"AAAA", b"BBBB")] {
        let left = write(
            dir.path(),
            "a.zip",
            &zip_bytes(&[
                ("src", left_body),
                ("src-x", b"x"),
                ("src/main.rs", b"fn main() {}"),
            ]),
        );
        let right = write(
            dir.path(),
            "b.zip",
            &zip_bytes(&[
                ("src", right_body),
                ("src-x", b"x"),
                ("src/main.rs", b"fn main() {}"),
            ]),
        );
        assert_eq!(
            verdict(&left, &right, true),
            NodeStatus::Different,
            "bodies {left_body:?} and {right_body:?}"
        );
    }
}

#[test]
fn a_file_moved_aside_by_its_namesake_directory_is_scanned_and_read() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(
        dir.path(),
        "a.zip",
        &zip_bytes(&[
            ("src", b"AAAA"),
            ("src-x", b"x"),
            ("src/main.rs", b"fn main() {}"),
        ]),
    );
    let source = open(&path);
    let scanned = scan_source(&source, &ScanOptions::default(), &Cancel::new(), &|_| {}).unwrap();
    let mut listed: Vec<String> = scanned
        .entries
        .keys()
        .map(|rel| {
            rel.to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/")
        })
        .collect();
    listed.sort_unstable();
    assert_eq!(listed, ["src", "src-x", "src/main.rs", "src~1"]);
    let moved = &scanned.entries[Path::new("src~1")];
    assert!(!moved.is_dir);
    assert_eq!(moved.size, 4);
    let mut open = source
        .file_system()
        .open(
            &ca_vfs::VfsPath::parse("src~1").unwrap(),
            &ca_vfs::Cancel::new(),
        )
        .unwrap();
    let mut content = Vec::new();
    std::io::Read::read_to_end(&mut open, &mut content).unwrap();
    assert_eq!(content, b"AAAA");
}

#[test]
fn the_stamp_of_a_directory_record_reaches_the_scan_when_a_sibling_sorts_between() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(
        dir.path(),
        "a.zip",
        &zip_bytes(&[
            ("src/", b""),
            ("src-old/x.txt", b"old"),
            ("src/a.rs", b"a"),
            ("src/main.rs", b"fn main() {}"),
        ]),
    );
    let scanned = scan_source(
        &open(&path),
        &ScanOptions::default(),
        &Cancel::new(),
        &|_| {},
    )
    .unwrap();
    assert_eq!(scanned.entries.len(), 5, "{:?}", scanned.entries.keys());
    let src = &scanned.entries[Path::new("src")];
    assert!(src.is_dir);
    assert!(
        src.modified.is_some(),
        "the record of the directory is kept"
    );
}

/// The node of `name` after the quick tests of a comparison of `left` and
/// `right`.
fn quick_status(left: &Path, right: &Path, quick: ca_fs::QuickTests, name: &str) -> NodeStatus {
    let left = open(left);
    let right = open(right);
    let cancel = Cancel::new();
    let scanned_left = scan_source(&left, &ScanOptions::default(), &cancel, &|_| {}).unwrap();
    let scanned_right = scan_source(&right, &ScanOptions::default(), &cancel, &|_| {}).unwrap();
    let mut tree = align_trees(
        &scanned_left,
        &scanned_right,
        &AlignmentOptions::default(),
        &cancel,
    );
    let options = CompareOptions {
        quick,
        ..CompareOptions::default()
    };
    compare_quick(&mut tree, &options);
    tree.children
        .iter()
        .find(|node| node.name == name)
        .map(|node| node.status)
        .unwrap()
}

/// One gzip member that holds `data` in a stored block.
fn gzip_member(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 0xff];
    out.push(1);
    let len = data.len() as u16;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&(!len).to_le_bytes());
    out.extend_from_slice(data);
    out.extend_from_slice(&crc32fast::hash(data).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out
}

/// The instant the DOS wall clock 1980-01-01 00:00:00 names in a zone
/// `zone_offset_seconds` ahead of UTC, plus `extra` seconds.
fn dos_epoch_in_zone(zone_offset_seconds: i32, extra: i64) -> std::time::SystemTime {
    let seconds = 315_532_800 - i64::from(zone_offset_seconds) + extra;
    std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds.unsigned_abs())
}

fn stamp(path: &Path, time: std::time::SystemTime) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(time)
        .unwrap();
}

/// A zip written by the Windows file manager stores a DOS stamp only: the
/// wall clock of the file in the zone of the machine that wrote it. A
/// container side reads the stamp in the zone of this machine, and a DOS stamp
/// has a two second tick, so a zip entry one second off the file it was made
/// from is the same time.
#[test]
fn a_zip_time_one_second_off_a_local_time_compares_as_the_same_time() {
    let dir = tempfile::tempdir().unwrap();
    let zip = write(dir.path(), "a.zip", &zip_bytes(&[("f.txt", b"same")]));
    let folder = dir.path().join("folder");
    std::fs::create_dir(&folder).unwrap();
    let local = write(&folder, "f.txt", b"same");
    stamp(&local, dos_epoch_in_zone(ca_fs::local_offset_seconds(), 1));

    let status = quick_status(&zip, &folder, ca_fs::QuickTests::default(), "f.txt");
    assert_eq!(status, NodeStatus::Same);
}

/// The same reading in two fixed zones on each side of UTC: the file made
/// from the entry is the same time, and a file one hour later is newer.
#[test]
fn a_zip_stamp_is_read_in_the_zone_the_container_is_opened_with() {
    for zone in [-14_400, 19_800] {
        let dir = tempfile::tempdir().unwrap();
        let zip = write(dir.path(), "a.zip", &zip_bytes(&[("f.txt", b"same")]));
        let folder = dir.path().join("folder");
        std::fs::create_dir(&folder).unwrap();
        let local = write(&folder, "f.txt", b"same");
        let container = Source::archive(
            &zip,
            ca_vfs::ArchiveOptions {
                zone_offset_seconds: zone,
                ..ca_vfs::ArchiveOptions::default()
            },
        )
        .unwrap();
        let cancel = Cancel::new();
        let status = |local_time: std::time::SystemTime| {
            stamp(&local, local_time);
            let left = scan_source(&container, &ScanOptions::default(), &cancel, &|_| {}).unwrap();
            let right =
                scan_source(&open(&folder), &ScanOptions::default(), &cancel, &|_| {}).unwrap();
            let mut tree = align_trees(&left, &right, &AlignmentOptions::default(), &cancel);
            compare_quick(&mut tree, &CompareOptions::default());
            tree.children[0].status
        };
        assert_eq!(
            status(dos_epoch_in_zone(zone, 0)),
            NodeStatus::Same,
            "zone {zone}"
        );
        assert_eq!(
            status(dos_epoch_in_zone(zone, 3_600)),
            NodeStatus::RightNewer,
            "zone {zone}"
        );
    }
}

/// A container places a DOS stamp in the zone it is opened with, and a scan
/// of the side takes the stamp as placed, so the stamp moves once.
#[test]
fn a_scan_of_a_container_side_moves_a_dos_stamp_once() {
    for zone in [-14_400, 19_800] {
        let dir = tempfile::tempdir().unwrap();
        let zip = write(dir.path(), "a.zip", &zip_bytes(&[("f.txt", b"same")]));
        let container = Source::archive(
            &zip,
            ca_vfs::ArchiveOptions {
                zone_offset_seconds: zone,
                ..ca_vfs::ArchiveOptions::default()
            },
        )
        .unwrap();
        let scanned =
            scan_source(&container, &ScanOptions::default(), &Cancel::new(), &|_| {}).unwrap();
        assert_eq!(
            scanned.entries[Path::new("f.txt")].modified,
            Some(dos_epoch_in_zone(zone, 0)),
            "zone {zone}"
        );
    }
}

/// A gzip footer records the size of the last member only, so the listed
/// size proves nothing about the content: the pair is neither different nor
/// the same on the size test alone.
#[test]
fn a_gzip_member_with_an_unproven_size_is_not_different_on_size_alone() {
    let dir = tempfile::tempdir().unwrap();
    let mut stream = gzip_member(b"hel");
    stream.extend_from_slice(&gzip_member(b"lo"));
    let gz = write(dir.path(), "data.txt.gz", &stream);
    let folder = dir.path().join("folder");
    std::fs::create_dir(&folder).unwrap();
    write(&folder, "data.txt", b"hello");

    let quick = ca_fs::QuickTests {
        timestamp: false,
        ..ca_fs::QuickTests::default()
    };
    let status = quick_status(&gz, &folder, quick, "data.txt");
    assert_eq!(status, NodeStatus::NotCompared);
}

/// Two gzip files hold different content. Neither listing proves a size and
/// neither holds a time, so the quick tests settle nothing: the pair is not
/// the same, a mirror replaces it, and a content test runs for it even when
/// content tests skip the pairs the quick tests call the same.
#[test]
fn two_gzip_files_of_different_content_are_not_the_same_on_the_quick_tests() {
    let dir = tempfile::tempdir().unwrap();
    let left_dir = dir.path().join("l");
    let right_dir = dir.path().join("r");
    std::fs::create_dir(&left_dir).unwrap();
    std::fs::create_dir(&right_dir).unwrap();
    let long = "a much longer text with different content\n".repeat(20);
    let left = open(&write(&left_dir, "notes.txt.gz", &gzip_member(b"short")));
    let right = open(&write(
        &right_dir,
        "notes.txt.gz",
        &gzip_member(long.as_bytes()),
    ));
    let cancel = Cancel::new();
    let scanned_left = scan_source(&left, &ScanOptions::default(), &cancel, &|_| {}).unwrap();
    let scanned_right = scan_source(&right, &ScanOptions::default(), &cancel, &|_| {}).unwrap();
    let mut tree = align_trees(
        &scanned_left,
        &scanned_right,
        &AlignmentOptions::default(),
        &cancel,
    );
    let mut options = CompareOptions::default();
    compare_quick(&mut tree, &options);
    let pair = &tree.children[0];
    assert_eq!(pair.name, "notes.txt");
    assert_eq!(pair.status, NodeStatus::NotCompared);
    assert_ne!(tree.status, NodeStatus::Same);

    for (preset, action) in [
        (
            ca_fs::SyncPreset::MirrorToRight,
            ca_fs::SyncAction::CopyLeftToRight,
        ),
        (
            ca_fs::SyncPreset::MirrorToLeft,
            ca_fs::SyncAction::CopyRightToLeft,
        ),
    ] {
        let preview = ca_fs::preview(&tree, &preset);
        let row = preview
            .rows
            .iter()
            .find(|row| row.name == "notes.txt")
            .unwrap();
        assert_eq!(row.action, action, "{preset:?}");
    }

    options.content.enabled = true;
    options.content.method = ContentMethod::Binary;
    assert!(options.content.skip_if_quick_same);
    assert!(compare_source_contents_parallel(
        &mut tree,
        &left,
        &right,
        &options,
        None,
        &cancel,
        &|_| {}
    ));
    assert_eq!(tree.children[0].status, NodeStatus::Different);
}

/// A copy into a zip, run through the batch a script opens, gives the record
/// the modification time of its source, as the default operation options
/// ask. The source falls on an odd second, which a DOS stamp cannot hold, so
/// the record reads back as the source's instant only through its extended
/// timestamp field. The pair the copy made compares as the same time, and
/// still does after a later edit rewrites the container.
#[test]
fn a_copy_into_a_zip_keeps_the_time_of_its_source() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("folder");
    std::fs::create_dir(&folder).unwrap();
    let file = write(&folder, "a.txt", b"hello");
    // 2024-06-15 12:00:01 UTC.
    let source_time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_718_452_801);
    stamp(&file, source_time);
    let zip = write(dir.path(), "right.zip", &zip_bytes(&[]));
    let quick = ca_fs::QuickTests::default();
    assert_eq!(
        quick_status(&folder, &zip, quick.clone(), "a.txt"),
        NodeStatus::LeftOrphan
    );

    let left = open(&folder);
    let right = open(&zip);
    let cancel = Cancel::new();
    let scanned_left = scan_source(&left, &ScanOptions::default(), &cancel, &|_| {}).unwrap();
    let scanned_right = scan_source(&right, &ScanOptions::default(), &cancel, &|_| {}).unwrap();
    let mut tree = align_trees(
        &scanned_left,
        &scanned_right,
        &AlignmentOptions::default(),
        &cancel,
    );
    compare_quick(&mut tree, &CompareOptions::default());
    let chosen = [PathBuf::from("a.txt")].into_iter().collect();
    let options = ca_fs::OperationOptions::default();
    assert!(options.preserve_modified);
    let plan = ca_fs::plan_copy(
        &ca_fs::resolve_selection(&tree, &chosen),
        ca_fs::Side::Left,
        ca_fs::Bases {
            left: &folder,
            right: &zip,
        },
        &options,
    );
    let ops = ca_fs::SourceOps::new(vec![ca_fs::Mount::new(zip.clone(), right.clone())]);
    ops.open_batch();
    let report = ca_fs::execute(
        &plan,
        &ca_fs::ExecutionContext::new(&ops, &cancel, ca_fs::Journaling::Disabled),
    );
    ops.commit_batch().unwrap();
    assert!(report.is_clean(), "{:?}", report.results);

    let entry = open(&zip)
        .file_system()
        .metadata(&ca_vfs::VfsPath::parse("a.txt").unwrap())
        .unwrap();
    assert_eq!(entry.modified, Some(source_time));
    assert_eq!(
        quick_status(&folder, &zip, quick.clone(), "a.txt"),
        NodeStatus::Same
    );

    let later = ca_vfs::ArchiveFs::open_path(
        &zip,
        ca_vfs::ArchiveOptions {
            zone_offset_seconds: ca_fs::local_offset_seconds(),
            ..ca_vfs::ArchiveOptions::default()
        },
    )
    .unwrap();
    later
        .apply_edits(
            &[ca_vfs::ArchiveEdit::WriteFile {
                path: ca_vfs::VfsPath::parse("later.txt").unwrap(),
                content: b"later".to_vec(),
                modified: None,
            }],
            &ca_vfs::Cancel::new(),
        )
        .unwrap();
    drop(later);
    let kept = open(&zip)
        .file_system()
        .metadata(&ca_vfs::VfsPath::parse("a.txt").unwrap())
        .unwrap();
    assert_eq!(kept.modified, Some(source_time));
    assert_eq!(kept.time_fidelity, ca_vfs::TimeFidelity::Utc);
    assert_eq!(
        quick_status(&folder, &zip, quick, "a.txt"),
        NodeStatus::Same,
        "the record kept the time of its source through the next rewrite"
    );
}

/// A snapshot of a zip records each DOS stamp with its two-second tick, so
/// the folder the zip was made from compares against the snapshot as it does
/// against the zip: a file one second off its stamp is the same time.
#[test]
fn a_snapshot_of_a_zip_keeps_the_two_second_tick_of_its_stamps() {
    let dir = tempfile::tempdir().unwrap();
    let zip = write(
        dir.path(),
        "a.zip",
        &zip_bytes(&[("even.txt", b"same"), ("odd.txt", b"same")]),
    );
    let folder = dir.path().join("folder");
    std::fs::create_dir(&folder).unwrap();
    for (name, extra) in [("even.txt", 0), ("odd.txt", 1)] {
        let local = write(&folder, name, b"same");
        stamp(
            &local,
            dos_epoch_in_zone(ca_fs::local_offset_seconds(), extra),
        );
    }
    let snapshot = ca_vfs::Snapshot::capture(
        open(&zip).file_system().as_ref(),
        &ca_vfs::VfsPath::root(),
        ca_vfs::SnapshotOptions::default(),
        &ca_vfs::Cancel::new(),
    )
    .unwrap();
    let recorded = dir.path().join("a.cass");
    snapshot.save(&recorded).unwrap();

    for name in ["even.txt", "odd.txt"] {
        for (label, side) in [("zip", &zip), ("snapshot", &recorded)] {
            assert_eq!(
                quick_status(side, &folder, ca_fs::QuickTests::default(), name),
                NodeStatus::Same,
                "{label}, {name}"
            );
        }
    }
}
