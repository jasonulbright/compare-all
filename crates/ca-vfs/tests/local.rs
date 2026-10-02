//! The local file system against the standard library over the same tree.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::io::{Cursor, Read, SeekFrom};

use ca_vfs::{Cancel, FileSystem, LocalFs, VfsError, VfsPath};

fn tree() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sub/deeper")).unwrap();
    std::fs::write(dir.path().join("a.txt"), b"alpha").unwrap();
    std::fs::write(dir.path().join("sub/b.txt"), b"bravo").unwrap();
    std::fs::write(dir.path().join("sub/deeper/c.bin"), vec![0u8; 4096]).unwrap();
    dir
}

#[test]
fn listing_matches_the_standard_library() {
    let dir = tree();
    let fs = LocalFs::new(dir.path());
    let cancel = Cancel::new();

    for relative in ["", "sub", "sub/deeper"] {
        let path = VfsPath::parse(relative).unwrap();
        let mine: BTreeSet<String> = fs
            .list(&path, &cancel)
            .unwrap()
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        let theirs: BTreeSet<String> = std::fs::read_dir(path.to_native(dir.path()))
            .unwrap()
            .map(|item| item.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(mine, theirs, "listing of {relative:?}");
    }
}

#[test]
fn metadata_matches_the_standard_library() {
    let dir = tree();
    let fs = LocalFs::new(dir.path());
    let path = VfsPath::parse("sub/deeper/c.bin").unwrap();
    let entry = fs.metadata(&path).unwrap();
    let native = std::fs::metadata(dir.path().join("sub/deeper/c.bin")).unwrap();
    assert_eq!(entry.size, native.len());
    assert!(!entry.is_dir());
    assert_eq!(entry.modified, native.modified().ok());
    assert!(entry.attributes.is_some());
}

#[test]
fn walk_finds_every_entry() {
    let dir = tree();
    let fs = LocalFs::new(dir.path());
    let found: BTreeSet<String> = ca_vfs::walk(&fs, &VfsPath::root(), &Cancel::new())
        .unwrap()
        .into_iter()
        .map(|entry| entry.path.as_str().to_owned())
        .collect();
    assert_eq!(
        found,
        [
            "a.txt",
            "sub",
            "sub/b.txt",
            "sub/deeper",
            "sub/deeper/c.bin"
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    );
}

#[test]
fn open_reads_and_seeks() {
    let dir = tree();
    let fs = LocalFs::new(dir.path());
    let mut open = fs
        .open(&VfsPath::parse("a.txt").unwrap(), &Cancel::new())
        .unwrap();
    assert!(open.is_seekable());
    let mut text = String::new();
    open.read_to_string(&mut text).unwrap();
    assert_eq!(text, "alpha");
    open.seek(SeekFrom::Start(1)).unwrap();
    let mut rest = String::new();
    open.read_to_string(&mut rest).unwrap();
    assert_eq!(rest, "lpha");
}

#[test]
fn write_create_rename_and_delete_round_trip() {
    let dir = tree();
    let fs = LocalFs::new(dir.path());
    let cancel = Cancel::new();

    fs.create_dir(&VfsPath::parse("new/inner").unwrap(), &cancel)
        .unwrap();
    assert!(dir.path().join("new/inner").is_dir());

    let target = VfsPath::parse("new/inner/file.txt").unwrap();
    fs.write_file(&target, &mut Cursor::new(b"payload".to_vec()), &cancel)
        .unwrap();
    assert_eq!(
        std::fs::read(dir.path().join("new/inner/file.txt")).unwrap(),
        b"payload"
    );

    let moved = VfsPath::parse("new/moved.txt").unwrap();
    fs.rename(&target, &moved, &cancel).unwrap();
    assert!(!dir.path().join("new/inner/file.txt").exists());

    fs.delete(&VfsPath::parse("new").unwrap(), &cancel).unwrap();
    assert!(!dir.path().join("new").exists());
}

#[test]
fn local_write_does_not_replace_a_read_only_file() {
    let dir = tree();
    let target = dir.path().join("a.txt");
    let original_permissions = std::fs::metadata(&target).unwrap().permissions();
    let mut read_only = original_permissions.clone();
    read_only.set_readonly(true);
    std::fs::set_permissions(&target, read_only).unwrap();
    let fs = LocalFs::new(dir.path());
    let result = fs.write_file(
        &VfsPath::parse("a.txt").unwrap(),
        &mut Cursor::new(b"replacement"),
        &Cancel::new(),
    );
    let bytes = std::fs::read(&target).unwrap();
    std::fs::set_permissions(&target, original_permissions).unwrap();
    assert!(result.is_err());
    assert_eq!(bytes, b"alpha");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
}

#[test]
fn missing_paths_report_not_found() {
    let dir = tree();
    let fs = LocalFs::new(dir.path());
    let missing = VfsPath::parse("nope.txt").unwrap();
    assert!(matches!(
        fs.metadata(&missing),
        Err(VfsError::NotFound { .. })
    ));
    assert!(matches!(
        fs.open(&missing, &Cancel::new()),
        Err(VfsError::NotFound { .. })
    ));
    assert!(matches!(
        fs.open(&VfsPath::parse("sub").unwrap(), &Cancel::new()),
        Err(VfsError::IsADirectory { .. })
    ));
}

#[test]
fn cancellation_stops_a_listing() {
    let dir = tree();
    let fs = LocalFs::new(dir.path());
    let cancel = Cancel::new();
    cancel.cancel();
    assert!(matches!(
        fs.list(&VfsPath::root(), &cancel),
        Err(VfsError::Cancelled)
    ));
}

#[test]
fn a_crafted_relative_path_cannot_escape_the_root() {
    let dir = tree();
    let outside = dir.path().parent().unwrap().join("escaped.txt");
    let _fs = LocalFs::new(dir.path().join("sub"));
    assert!(VfsPath::parse("../../escaped.txt").is_err());
    assert!(!outside.exists());
}

/// `name` inside `dir`, spelled so the file system receives it unchanged.
///
/// The Win32 path layer drops a trailing dot or space from a name. The
/// canonical form of a Windows path carries the verbatim prefix, which
/// bypasses that layer.
fn exact(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    std::fs::canonicalize(dir).unwrap().join(name)
}

/// A local name that the path rules refuse lists under the shared mapping,
/// refused, and a capture of the folder records it the same way.
#[test]
fn a_local_name_the_rules_refuse_lists_under_its_mapped_name() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("ok.txt"), b"ok").unwrap();
    let stored = exact(dir.path(), "trailing. ");
    std::fs::write(&stored, b"dot").unwrap();
    let fs = LocalFs::new(dir.path());

    let listed = fs.list(&VfsPath::root(), &Cancel::new()).unwrap();
    assert_eq!(listed.len(), 2, "{listed:?}");
    assert!(
        listed.iter().all(|entry| !entry.path.is_root()),
        "no row names the folder itself: {listed:?}"
    );
    let row = listed
        .iter()
        .find(|entry| entry.path.as_str() == "trailing__")
        .expect("the refused row lists under the mapped name");
    assert!(row.refused);
    assert!(!row.is_dir());
    assert_eq!(row.size, 3);
    let reason = row.error.as_deref().unwrap_or_default();
    assert!(reason.contains("\"trailing. \""), "{reason}");

    let capture = ca_vfs::Snapshot::capture(
        &fs,
        &VfsPath::root(),
        ca_vfs::SnapshotOptions {
            include_crc: true,
            ..ca_vfs::SnapshotOptions::default()
        },
        &Cancel::new(),
    )
    .unwrap();
    let record = capture
        .entries
        .iter()
        .find(|record| record.path == "trailing__")
        .expect("the capture records the mapped name");
    assert!(record.refused);
    assert_eq!(record.crc32, None);
    let recorded = record.error.as_deref().unwrap_or_default();
    assert!(recorded.contains("\"trailing. \""), "{recorded}");
    assert!(!recorded.contains("checksum not taken"), "{recorded}");
    let replay = ca_vfs::SnapshotFs::new(capture);
    let path = VfsPath::parse("trailing__").unwrap();
    assert!(replay.metadata(&path).unwrap().refused);
    assert!(matches!(
        replay.open(&path, &Cancel::new()),
        Err(VfsError::Refused { .. })
    ));

    std::fs::remove_file(&stored).unwrap();
}

/// A refused directory whose mapped name another item holds takes a suffix,
/// and a walk keeps its refusal and lists nothing under it.
#[test]
fn a_refused_local_directory_takes_a_free_name_and_is_not_walked() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("folder_"), b"plain").unwrap();
    let stored = exact(dir.path(), "folder.");
    std::fs::create_dir(&stored).unwrap();
    std::fs::write(stored.join("inner.txt"), b"inner").unwrap();
    let fs = LocalFs::new(dir.path());

    let walked = ca_vfs::walk(&fs, &VfsPath::root(), &Cancel::new()).unwrap();
    let paths: BTreeSet<&str> = walked.iter().map(|entry| entry.path.as_str()).collect();
    assert_eq!(
        paths,
        BTreeSet::from(["folder_", "folder_~1"]),
        "{walked:?}"
    );
    let refused = walked
        .iter()
        .find(|entry| entry.path.as_str() == "folder_~1")
        .unwrap();
    assert!(refused.refused);
    assert!(refused.is_dir());
    let reason = refused.error.as_deref().unwrap_or_default();
    assert!(reason.contains("\"folder.\""), "{reason}");
    let plain = walked
        .iter()
        .find(|entry| entry.path.as_str() == "folder_")
        .unwrap();
    assert!(!plain.refused);
    assert_eq!(plain.size, 5);

    std::fs::remove_dir_all(&stored).unwrap();
}

/// A refused row names no item on disk, so opening it states the refusal
/// instead of reporting that nothing is there.
#[test]
fn a_refused_local_row_does_not_open_and_states_why() {
    let dir = tempfile::tempdir().unwrap();
    let stored = exact(dir.path(), "dot.");
    std::fs::write(&stored, b"dot").unwrap();
    let fs = LocalFs::new(dir.path());

    let path = VfsPath::parse("dot_").unwrap();
    let row = fs.metadata(&path).unwrap();
    assert!(row.refused);
    assert_eq!(row.size, 3);
    match fs.open(&path, &Cancel::new()) {
        Err(VfsError::Refused { reason, .. }) => assert!(reason.contains("\"dot.\""), "{reason}"),
        other => panic!("the refused row opened as {other:?}"),
    }

    std::fs::remove_file(&stored).unwrap();
}

/// A name that is not valid Unicode lists refused under a name with `_` in
/// place of each sequence that does not decode, and its row does not open.
#[cfg(windows)]
#[test]
fn a_local_name_that_is_not_unicode_lists_refused_and_does_not_open() {
    use std::os::windows::ffi::OsStringExt;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("ok.txt"), b"ok").unwrap();
    let name = std::ffi::OsString::from_wide(&[0x61, 0xD800, 0x62]);
    let stored = std::fs::canonicalize(dir.path()).unwrap().join(&name);
    std::fs::write(&stored, b"surrogate").unwrap();
    let fs = LocalFs::new(dir.path());

    let listed = fs.list(&VfsPath::root(), &Cancel::new()).unwrap();
    assert_eq!(listed.len(), 2, "{listed:?}");
    let row = listed
        .iter()
        .find(|entry| entry.path.as_str() == "a_b")
        .expect("the row lists under the mapped name");
    assert!(row.refused);
    assert_eq!(row.size, 9);
    let reason = row.error.as_deref().unwrap_or_default();
    assert!(reason.contains("not valid Unicode"), "{reason}");
    let path = VfsPath::parse("a_b").unwrap();
    assert!(fs.metadata(&path).unwrap().refused);
    assert!(matches!(
        fs.open(&path, &Cancel::new()),
        Err(VfsError::Refused { .. })
    ));

    std::fs::remove_file(&stored).unwrap();
}

/// A DOS device name, with or without an extension, lists refused under a
/// mapped name: a path built from it reaches the device on Windows.
#[cfg(windows)]
#[test]
fn a_windows_device_name_lists_refused_and_does_not_open() {
    let dir = tempfile::tempdir().unwrap();
    let names = ["nul", "con.txt", "COM1.tar.gz", "aux .c."];
    for name in names {
        std::fs::write(exact(dir.path(), name), name.as_bytes()).unwrap();
    }
    let fs = LocalFs::new(dir.path());

    let listed = fs.list(&VfsPath::root(), &Cancel::new()).unwrap();
    let paths: BTreeSet<&str> = listed.iter().map(|entry| entry.path.as_str()).collect();
    assert_eq!(
        paths,
        BTreeSet::from(["nul_", "con_.txt", "COM1_.tar.gz", "aux _.c_"]),
        "{listed:?}"
    );
    for (mapped, stored) in [("nul_", "nul"), ("con_.txt", "con.txt")] {
        let row = listed
            .iter()
            .find(|entry| entry.path.as_str() == mapped)
            .unwrap();
        assert!(row.refused, "{row:?}");
        assert_eq!(row.size, stored.len() as u64);
        let reason = row.error.as_deref().unwrap_or_default();
        assert!(reason.contains("device name"), "{reason}");
        assert!(matches!(
            fs.open(&VfsPath::parse(mapped).unwrap(), &Cancel::new()),
            Err(VfsError::Refused { .. })
        ));
    }

    for name in names {
        std::fs::remove_file(exact(dir.path(), name)).unwrap();
    }
}
