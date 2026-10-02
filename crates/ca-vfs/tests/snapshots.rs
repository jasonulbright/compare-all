//! Capturing, storing and re-reading folder listings.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read};

use ca_vfs::snapshot::{
    RecordedFidelity, SnapshotRecord, FORMAT_VERSION, MAGIC, MIN_READER_VERSION,
};
use ca_vfs::{
    Cancel, FileSystem, LocalFs, Snapshot, SnapshotFs, SnapshotOptions, TimeFidelity, VfsError,
    VfsPath,
};
use support::{open_memory, zip_bytes};

fn tree() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("a.txt"), b"alpha").unwrap();
    std::fs::write(dir.path().join("sub/b.txt"), b"bravo").unwrap();
    dir
}

fn capture(options: SnapshotOptions) -> (tempfile::TempDir, Snapshot) {
    let dir = tree();
    let fs = LocalFs::new(dir.path());
    let snapshot = Snapshot::capture(&fs, &VfsPath::root(), options, &Cancel::new()).unwrap();
    (dir, snapshot)
}

#[cfg(windows)]
#[test]
fn a_snapshot_refuses_to_overwrite_a_target_that_cannot_be_replaced() {
    use std::os::windows::fs::OpenOptionsExt;

    let (dir, snapshot) = capture(SnapshotOptions::default());
    let target = dir.path().join("listing.cass");
    std::fs::write(&target, b"previous snapshot").unwrap();
    // Allow reads and in-place writes, but deny deletion/rename. A truncating
    // writer ignores this replacement constraint and destroys the old output.
    let _held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(3)
        .open(&target)
        .unwrap();
    let result = snapshot.save(&target);
    assert_eq!(std::fs::read(&target).unwrap(), b"previous snapshot");
    assert!(result.is_err());
    assert!(!std::fs::read_dir(dir.path()).unwrap().any(|entry| entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".compare-all-")));
}

#[test]
fn a_capture_records_the_whole_tree() {
    let (_dir, snapshot) = capture(SnapshotOptions::default());
    let paths: BTreeSet<&str> = snapshot
        .entries
        .iter()
        .map(|record| record.path.as_str())
        .collect();
    assert_eq!(paths, ["a.txt", "sub", "sub/b.txt"].into());
    let file = snapshot
        .entries
        .iter()
        .find(|record| record.path == "a.txt")
        .unwrap();
    assert_eq!(file.size, 5);
    assert!(file.modified.is_some());
    assert!(file.attributes.is_some());
    assert!(file.crc32.is_none());
}

#[test]
fn a_capture_can_include_crcs() {
    let options = SnapshotOptions {
        include_crc: true,
        ..SnapshotOptions::default()
    };
    let (_dir, snapshot) = capture(options);
    let file = snapshot
        .entries
        .iter()
        .find(|record| record.path == "a.txt")
        .unwrap();
    assert_eq!(file.crc32, Some(crc32fast::hash(b"alpha")));
    assert!(snapshot.has_crc);
}

#[test]
fn a_capture_can_leave_empty_folders_out() {
    let dir = tree();
    std::fs::create_dir_all(dir.path().join("empty")).unwrap();
    let fs = LocalFs::new(dir.path());

    let with_empty = Snapshot::capture(
        &fs,
        &VfsPath::root(),
        SnapshotOptions::default(),
        &Cancel::new(),
    )
    .unwrap();
    let paths_with_empty: BTreeSet<&str> = with_empty
        .entries
        .iter()
        .map(|record| record.path.as_str())
        .collect();
    assert!(paths_with_empty.contains("empty"));

    let options = SnapshotOptions {
        include_empty_folders: false,
        ..SnapshotOptions::default()
    };
    let without_empty = Snapshot::capture(&fs, &VfsPath::root(), options, &Cancel::new()).unwrap();
    let paths_without_empty: BTreeSet<&str> = without_empty
        .entries
        .iter()
        .map(|record| record.path.as_str())
        .collect();
    assert!(!paths_without_empty.contains("empty"));
    assert!(paths_without_empty.contains("sub"));
}

#[test]
fn a_snapshot_round_trips_through_the_container_format() {
    let (_dir, snapshot) = capture(SnapshotOptions::default());
    let mut bytes = Vec::new();
    snapshot.write_to(&mut bytes).unwrap();
    assert_eq!(bytes.get(..8), Some(MAGIC.as_slice()));
    assert!(bytes.len() < 4096, "a listing should stay small");

    let reread = Snapshot::read_from(&mut Cursor::new(bytes)).unwrap();
    assert_eq!(reread, snapshot);
}

#[test]
fn a_saved_snapshot_lists_but_holds_no_content() {
    let (dir, snapshot) = capture(SnapshotOptions::default());
    let file = dir.path().join("listing.cass");
    snapshot.save(&file).unwrap();

    let fs = SnapshotFs::open(&file).unwrap();
    let capabilities = fs.capabilities();
    assert!(!capabilities.writable);
    assert!(!capabilities.content_available);
    assert!(capabilities.supports_timestamps);

    let names: BTreeSet<String> = fs
        .list(&VfsPath::root(), &Cancel::new())
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    assert_eq!(names, ["a.txt", "sub"].map(str::to_owned).into());

    let error = fs
        .open(&VfsPath::parse("a.txt").unwrap(), &Cancel::new())
        .unwrap_err();
    assert!(
        matches!(error, VfsError::ContentNotStored { .. }),
        "unexpected {error}"
    );
    assert!(fs
        .root_label()
        .contains(dir.path().file_name().unwrap().to_string_lossy().as_ref()));
}

#[test]
fn a_snapshot_compares_against_the_live_folder_it_came_from() {
    let (dir, snapshot) = capture(SnapshotOptions {
        include_crc: true,
        ..SnapshotOptions::default()
    });
    let recorded = SnapshotFs::new(snapshot);
    std::fs::write(dir.path().join("a.txt"), b"changed").unwrap();

    let live = LocalFs::new(dir.path());
    let path = VfsPath::parse("a.txt").unwrap();
    let before = recorded.metadata(&path).unwrap();
    let after = live.metadata(&path).unwrap();
    assert_ne!(before.size, after.size);
    assert_eq!(before.crc32, Some(crc32fast::hash(b"alpha")));
}

#[test]
fn a_container_can_be_captured_like_a_folder() {
    let fs = open_memory(
        zip_bytes(&[("a.txt", b"alpha"), ("d/b.txt", b"bravo")]),
        "x.zip",
    )
    .unwrap();
    let snapshot = Snapshot::capture(
        &fs,
        &VfsPath::root(),
        SnapshotOptions {
            include_crc: true,
            ..SnapshotOptions::default()
        },
        &Cancel::new(),
    )
    .unwrap();
    let paths: BTreeSet<&str> = snapshot
        .entries
        .iter()
        .map(|record| record.path.as_str())
        .collect();
    assert_eq!(paths, ["a.txt", "d", "d/b.txt"].into());
}

#[test]
fn unknown_fields_and_header_bytes_survive_a_round_trip() {
    let (_dir, snapshot) = capture(SnapshotOptions::default());
    let mut bytes = Vec::new();
    snapshot.write_to(&mut bytes).unwrap();

    // Rewrite the payload the way a later build might: a newer format version,
    // an unknown header extension, and fields this build does not know.
    let payload = {
        let mut value: serde_json::Value = {
            let body = bytes.get(24..).unwrap().to_vec();
            let mut decoded = Vec::new();
            flate2::read::DeflateDecoder::new(body.as_slice())
                .read_to_end(&mut decoded)
                .unwrap();
            serde_json::from_slice(&decoded).unwrap()
        };
        value["future_field"] = serde_json::json!("kept");
        value["entries"][0]["future_entry_field"] = serde_json::json!(7);
        serde_json::to_vec(&value).unwrap()
    };

    let mut header = Vec::new();
    header.extend_from_slice(MAGIC);
    header.extend_from_slice(&(FORMAT_VERSION + 9).to_le_bytes());
    header.extend_from_slice(&MIN_READER_VERSION.to_le_bytes());
    header.extend_from_slice(&4u16.to_le_bytes());
    header.push(0);
    header.push(0);
    header.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    header.extend_from_slice(b"\xde\xad\xbe\xef");
    header.extend_from_slice(&payload);

    let reread = Snapshot::read_from(&mut Cursor::new(header)).unwrap();
    assert_eq!(
        reread.unknown.get("future_field"),
        Some(&serde_json::json!("kept"))
    );
    assert!(reread
        .entries
        .iter()
        .any(|record| record.unknown.contains_key("future_entry_field")));

    let mut again = Vec::new();
    reread.write_to(&mut again).unwrap();
    let third = Snapshot::read_from(&mut Cursor::new(again)).unwrap();
    assert_eq!(third, reread, "unknown fields must survive a rewrite");
}

/// A capture of a zip records that each DOS stamp holds a two-second tick,
/// and the recorded listing states it. A record written before the field
/// existed reads as an instant. A precision a later build writes reads as an
/// instant and is written back unchanged.
#[test]
fn a_recorded_time_keeps_the_precision_its_source_stated() {
    let fs = open_memory(zip_bytes(&[("a.txt", b"alpha")]), "x.zip").unwrap();
    let snapshot = Snapshot::capture(
        &fs,
        &VfsPath::root(),
        SnapshotOptions::default(),
        &Cancel::new(),
    )
    .unwrap();
    let payload = serde_json::to_value(&snapshot).unwrap();
    assert_eq!(
        payload["entries"][0]["time_fidelity"],
        serde_json::json!("local_two_second")
    );
    let path = VfsPath::parse("a.txt").unwrap();
    let listed = SnapshotFs::new(snapshot).metadata(&path).unwrap();
    assert_eq!(listed.time_fidelity, TimeFidelity::LocalTwoSecond);

    let older: SnapshotRecord =
        serde_json::from_str(r#"{"path":"a.txt","size":1,"modified":1000}"#).unwrap();
    assert_eq!(older.time_fidelity, None);
    let later: SnapshotRecord = serde_json::from_str(
        r#"{"path":"a.txt","size":1,"modified":1000,"time_fidelity":"nanosecond"}"#,
    )
    .unwrap();
    assert_eq!(
        later.time_fidelity,
        Some(RecordedFidelity::Unknown(serde_json::json!("nanosecond")))
    );
    assert!(later.unknown.is_empty(), "{:?}", later.unknown);
    assert_eq!(
        serde_json::to_value(&later).unwrap()["time_fidelity"],
        serde_json::json!("nanosecond")
    );
    for record in [older, later] {
        let snapshot = Snapshot {
            origin: "wherever".to_owned(),
            captured: None,
            has_crc: false,
            entries: vec![record],
            unknown: BTreeMap::default(),
        };
        let listed = SnapshotFs::new(snapshot).metadata(&path).unwrap();
        assert_eq!(listed.time_fidelity, TimeFidelity::Utc);
    }
}

#[test]
fn a_snapshot_needing_a_newer_reader_is_refused() {
    let (_dir, snapshot) = capture(SnapshotOptions::default());
    let mut bytes = Vec::new();
    snapshot.write_to(&mut bytes).unwrap();
    if let Some(slot) = bytes.get_mut(10..12) {
        slot.copy_from_slice(&(FORMAT_VERSION + 1).to_le_bytes());
    }
    let error = Snapshot::read_from(&mut Cursor::new(bytes)).unwrap_err();
    assert!(
        matches!(error, VfsError::Corrupt { .. }),
        "unexpected {error}"
    );
}

#[test]
fn a_file_that_is_not_a_snapshot_is_refused() {
    let error =
        Snapshot::read_from(&mut Cursor::new(b"not a snapshot at all".to_vec())).unwrap_err();
    assert!(
        matches!(error, VfsError::Corrupt { .. }),
        "unexpected {error}"
    );
}

#[test]
fn a_truncated_snapshot_is_refused() {
    let (_dir, snapshot) = capture(SnapshotOptions::default());
    let mut bytes = Vec::new();
    snapshot.write_to(&mut bytes).unwrap();
    for cut in [0usize, 4, 20, bytes.len() / 2] {
        let short = bytes.get(..cut).unwrap_or_default().to_vec();
        assert!(Snapshot::read_from(&mut Cursor::new(short)).is_err());
    }
}

fn recorded(path: &str) -> ca_vfs::snapshot::SnapshotRecord {
    ca_vfs::snapshot::SnapshotRecord {
        path: path.to_owned(),
        dir: false,
        size: 1,
        modified: None,
        time_fidelity: None,
        created: None,
        attributes: None,
        crc32: None,
        link: None,
        version_info: None,
        error: None,
        refused: false,
        unknown: BTreeMap::default(),
    }
}

#[test]
fn recorded_traversal_names_are_listed_with_an_error_and_never_opened() {
    let snapshot = Snapshot {
        origin: "wherever".to_owned(),
        captured: None,
        has_crc: false,
        entries: vec![
            recorded("../escaped.txt"),
            recorded("log-12:30.txt"),
            recorded("ok.txt"),
        ],
        unknown: BTreeMap::default(),
    };
    let fs = SnapshotFs::new(snapshot);
    let mut names: Vec<String> = fs
        .list(&VfsPath::root(), &Cancel::new())
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    names.sort_unstable();
    assert_eq!(names, ["__", "log-12_30.txt", "ok.txt"]);
    for (listed, stored) in [
        ("__/escaped.txt", "../escaped.txt"),
        ("log-12_30.txt", "log-12:30.txt"),
    ] {
        let path = VfsPath::parse(listed).unwrap();
        let entry = fs.metadata(&path).unwrap();
        let error = entry.error.clone().unwrap_or_default();
        assert!(error.contains(&format!("{stored:?}")), "{listed}: {error}");
        assert!(entry.refused, "{listed} is not marked refused");
        let refused = fs.open(&path, &Cancel::new()).unwrap_err();
        assert!(
            matches!(refused, VfsError::Refused { .. }),
            "{listed} is refused for its name, not for missing content: {refused}"
        );
    }
}
