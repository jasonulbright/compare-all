//! Listing and reading every container format this build supports.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::collections::BTreeSet;
use std::io::{Cursor, Read};

use ca_vfs::{
    ArchiveEdit, ArchiveFormat, ArchiveFs, ArchiveOptions, Cancel, FileSystem, VfsError, VfsPath,
};
use support::{
    bzip2, gzip, open_memory, open_memory_with, sevenz_bytes, tar_bytes, xz, zip_bytes,
    zip_encrypted, zip_manual, zip_with_raw_name, ManualEntry,
};

const HELLO: &[u8] = b"hello from inside the container";

fn names(fs: &dyn FileSystem, dir: &str) -> BTreeSet<String> {
    fs.list(&VfsPath::parse(dir).unwrap(), &Cancel::new())
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect()
}

fn read(fs: &dyn FileSystem, path: &str) -> Vec<u8> {
    let mut open = fs
        .open(&VfsPath::parse(path).unwrap(), &Cancel::new())
        .unwrap();
    let mut out = Vec::new();
    open.read_to_end(&mut out).unwrap();
    out
}

#[test]
fn zip_lists_and_reads() {
    let bytes = zip_bytes(&[("a.txt", HELLO), ("dir/b.txt", b"second")]);
    let fs = open_memory(bytes, "fixture.zip").unwrap();
    assert_eq!(fs.format(), ArchiveFormat::Zip);
    assert_eq!(names(&fs, ""), ["a.txt", "dir"].map(str::to_owned).into());
    assert_eq!(names(&fs, "dir"), ["b.txt"].map(str::to_owned).into());
    assert_eq!(read(&fs, "a.txt"), HELLO);
    assert_eq!(read(&fs, "dir/b.txt"), b"second");
}

#[test]
fn zip_exposes_the_stored_crc() {
    let bytes = zip_bytes(&[("a.txt", HELLO)]);
    let fs = open_memory(bytes, "fixture.zip").unwrap();
    assert!(fs.capabilities().stored_crc);
    let entry = fs.metadata(&VfsPath::parse("a.txt").unwrap()).unwrap();
    let expected = crc32fast::hash(HELLO);
    assert_eq!(entry.crc32, Some(expected));
    assert_eq!(entry.size, HELLO.len() as u64);
    assert!(entry.modified.is_some());
}

#[test]
fn zip64_lists_many_entries() {
    let entries: Vec<(String, Vec<u8>)> = (0..80)
        .map(|index| {
            (
                format!("file{index:03}.txt"),
                format!("body {index}").into_bytes(),
            )
        })
        .collect();
    let borrowed: Vec<(&str, &[u8])> = entries
        .iter()
        .map(|(name, body)| (name.as_str(), body.as_slice()))
        .collect();
    let fs = open_memory(zip_bytes(&borrowed), "many.zip").unwrap();
    assert_eq!(names(&fs, "").len(), 80);
    assert_eq!(read(&fs, "file042.txt"), b"body 42");
}

#[test]
fn zip_legacy_encoded_names_decode() {
    // A name stored without the UTF-8 flag is read through the legacy code
    // page: byte 0x81 there is U+00FC, not a byte the UTF-8 decoder accepts.
    let mut raw = vec![0x81u8];
    raw.extend_from_slice(b".txt");
    let bytes = support::zip_with_legacy_name(&raw, HELLO);
    let fs = open_memory(bytes, "legacy.zip").unwrap();
    let listed = names(&fs, "");
    assert_eq!(listed, ["\u{00fc}.txt".to_owned()].into());
    assert_eq!(read(&fs, "\u{00fc}.txt"), HELLO);

    // A name that is valid UTF-8 round trips unchanged.
    let fs = open_memory(zip_with_raw_name("caf\u{00e9}.txt", HELLO), "utf8.zip").unwrap();
    assert_eq!(read(&fs, "caf\u{00e9}.txt"), HELLO);
}

#[test]
fn zip_encrypted_entry_needs_a_password() {
    let bytes = zip_encrypted("secret.txt", HELLO, "open sesame");
    let fs = open_memory(bytes.clone(), "locked.zip").unwrap();
    let error = fs
        .open(&VfsPath::parse("secret.txt").unwrap(), &Cancel::new())
        .unwrap_err();
    assert!(error.needs_password(), "unexpected error {error}");

    let options = ArchiveOptions {
        password: Some("open sesame".to_owned()),
        ..ArchiveOptions::default()
    };
    let fs = open_memory_with(bytes, "locked.zip", options).unwrap();
    assert_eq!(read(&fs, "secret.txt"), HELLO);
}

#[test]
fn zip_wrong_password_is_reported_as_such() {
    let bytes = zip_encrypted("secret.txt", HELLO, "open sesame");
    let options = ArchiveOptions {
        password: Some("wrong".to_owned()),
        ..ArchiveOptions::default()
    };
    let fs = open_memory_with(bytes, "locked.zip", options).unwrap();
    let error = fs
        .open(&VfsPath::parse("secret.txt").unwrap(), &Cancel::new())
        .unwrap_err();
    assert!(error.needs_password(), "unexpected error {error}");
}

#[test]
fn tar_lists_and_reads() {
    let bytes = tar_bytes(&[("a.txt", HELLO), ("dir/b.txt", b"second")]);
    let fs = open_memory(bytes, "fixture.tar").unwrap();
    assert_eq!(fs.format(), ArchiveFormat::Tar);
    assert_eq!(names(&fs, ""), ["a.txt", "dir"].map(str::to_owned).into());
    assert_eq!(read(&fs, "dir/b.txt"), b"second");
    let entry = fs.metadata(&VfsPath::parse("a.txt").unwrap()).unwrap();
    assert!(entry.modified.is_some());
    assert!(entry.attributes.is_some_and(|a| a.unix_mode.is_some()));
}

#[test]
fn compressed_tars_list_and_read() {
    let plain = tar_bytes(&[("a.txt", HELLO)]);
    let cases: [(Vec<u8>, &str, ArchiveFormat); 3] = [
        (gzip(&plain), "fixture.tar.gz", ArchiveFormat::TarGz),
        (bzip2(&plain), "fixture.tar.bz2", ArchiveFormat::TarBz2),
        (xz(&plain), "fixture.tar.xz", ArchiveFormat::TarXz),
    ];
    for (bytes, name, expected) in cases {
        let fs = open_memory(bytes, name).unwrap();
        assert_eq!(fs.format(), expected, "format of {name}");
        assert_eq!(read(&fs, "a.txt"), HELLO, "content of {name}");
    }
}

#[test]
fn a_renamed_compressed_tar_still_opens_as_folders() {
    // The magic bytes decide, so the wrong extension does not hide the tar.
    let bytes = gzip(&tar_bytes(&[("a.txt", HELLO)]));
    let fs = open_memory(bytes, "misnamed.gz").unwrap();
    assert_eq!(fs.format(), ArchiveFormat::TarGz);
    assert_eq!(read(&fs, "a.txt"), HELLO);
}

#[test]
fn single_stream_containers_hold_one_file() {
    let cases: [(Vec<u8>, &str, &str, ArchiveFormat); 3] = [
        (gzip(HELLO), "notes.txt.gz", "notes.txt", ArchiveFormat::Gz),
        (
            bzip2(HELLO),
            "notes.txt.bz2",
            "notes.txt",
            ArchiveFormat::Bz2,
        ),
        (xz(HELLO), "notes.txt.xz", "notes.txt", ArchiveFormat::Xz),
    ];
    for (bytes, archive, inner, expected) in cases {
        let fs = open_memory(bytes, archive).unwrap();
        assert_eq!(fs.format(), expected, "format of {archive}");
        assert_eq!(names(&fs, ""), [inner.to_owned()].into());
        assert_eq!(read(&fs, inner), HELLO, "content of {archive}");
        let entry = fs.metadata(&VfsPath::parse(inner).unwrap()).unwrap();
        assert_eq!(entry.size, HELLO.len() as u64, "size of {archive}");
    }
}

#[test]
fn sevenz_lists_and_reads() {
    let bytes = sevenz_bytes(&[("a.txt", HELLO), ("dir/b.txt", b"second")]);
    let fs = open_memory(bytes, "fixture.7z").unwrap();
    assert_eq!(fs.format(), ArchiveFormat::SevenZip);
    assert_eq!(names(&fs, ""), ["a.txt", "dir"].map(str::to_owned).into());
    assert_eq!(read(&fs, "dir/b.txt"), b"second");
    assert!(fs.capabilities().stored_crc);
    assert!(!fs.capabilities().writable, "7z is read-only");
}

#[test]
fn a_nested_container_opens_through_its_parent() {
    let inner = zip_bytes(&[("inner.txt", HELLO)]);
    let outer = zip_bytes(&[("nested.zip", inner.as_slice())]);
    let fs = open_memory(outer, "outer.zip").unwrap();
    let nested = fs
        .open_nested(&VfsPath::parse("nested.zip").unwrap(), &Cancel::new())
        .unwrap();
    assert_eq!(read(&nested, "inner.txt"), HELLO);
}

#[test]
fn nesting_depth_is_bounded() {
    let inner = zip_bytes(&[("inner.txt", HELLO)]);
    let outer = zip_bytes(&[("nested.zip", inner.as_slice())]);
    let mut options = ArchiveOptions::default();
    options.limits.max_nesting_depth = 0;
    let fs = open_memory_with(outer, "outer.zip", options).unwrap();
    let error = fs
        .open_nested(&VfsPath::parse("nested.zip").unwrap(), &Cancel::new())
        .unwrap_err();
    assert!(
        matches!(error, VfsError::LimitExceeded { .. }),
        "unexpected error {error}"
    );
}

#[test]
fn an_empty_container_lists_nothing() {
    for (bytes, name) in [
        (zip_bytes(&[]), "empty.zip"),
        (tar_bytes(&[]), "empty.tar"),
        (sevenz_bytes(&[]), "empty.7z"),
    ] {
        let fs = open_memory(bytes, name).unwrap();
        assert!(names(&fs, "").is_empty(), "{name} should be empty");
    }
}

#[test]
fn opening_a_directory_entry_fails() {
    let fs = open_memory(zip_bytes(&[("dir/b.txt", b"x")]), "fixture.zip").unwrap();
    let error = fs
        .open(&VfsPath::parse("dir").unwrap(), &Cancel::new())
        .unwrap_err();
    assert!(matches!(error, VfsError::IsADirectory { .. }));
}

#[test]
fn a_missing_entry_reports_not_found() {
    let fs = open_memory(zip_bytes(&[("a.txt", b"x")]), "fixture.zip").unwrap();
    let error = fs
        .open(&VfsPath::parse("nope.txt").unwrap(), &Cancel::new())
        .unwrap_err();
    assert!(matches!(error, VfsError::NotFound { .. }));
}

#[test]
fn cancellation_stops_a_read_in_progress() {
    let big = vec![b'x'; 8 * 1024 * 1024];
    let fs = open_memory(zip_bytes(&[("big.bin", big.as_slice())]), "big.zip").unwrap();
    let cancel = Cancel::new();
    cancel.cancel();
    let error = fs
        .open(&VfsPath::parse("big.bin").unwrap(), &cancel)
        .unwrap_err();
    assert!(matches!(error, VfsError::Cancelled), "unexpected {error}");
}

#[test]
fn a_zip_on_disk_accepts_writes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("editable.zip");
    std::fs::write(&path, zip_bytes(&[("a.txt", HELLO), ("keep.txt", b"kept")])).unwrap();

    let fs = ArchiveFs::open_path(&path, ArchiveOptions::default()).unwrap();
    assert!(fs.capabilities().writable);
    let cancel = Cancel::new();

    fs.write_file(
        &VfsPath::parse("a.txt").unwrap(),
        &mut Cursor::new(b"replaced".to_vec()),
        &cancel,
    )
    .unwrap();
    assert_eq!(read(&fs, "a.txt"), b"replaced");
    assert_eq!(read(&fs, "keep.txt"), b"kept");

    fs.create_dir(&VfsPath::parse("added").unwrap(), &cancel)
        .unwrap();
    assert!(names(&fs, "").contains("added"));

    fs.rename(
        &VfsPath::parse("keep.txt").unwrap(),
        &VfsPath::parse("added/moved.txt").unwrap(),
        &cancel,
    )
    .unwrap();
    assert_eq!(read(&fs, "added/moved.txt"), b"kept");

    fs.delete(&VfsPath::parse("a.txt").unwrap(), &cancel)
        .unwrap();
    assert!(!names(&fs, "").contains("a.txt"));

    // The rewritten container is still a valid zip to a fresh reader.
    let reopened = ArchiveFs::open_path(&path, ArchiveOptions::default()).unwrap();
    assert_eq!(read(&reopened, "added/moved.txt"), b"kept");
}

/// A zip that stores a file `x` and a directory `x/` lists the directory as
/// `x` and the file as `x~1`.
fn clashing_zip(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("clash.zip");
    std::fs::write(
        &path,
        zip_bytes(&[("x", b"AAAA"), ("x/y.txt", b"y"), ("ok.txt", b"ok")]),
    )
    .unwrap();
    path
}

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

/// Deleting the directory row removes the directory's records and leaves the
/// file record stored under the same name.
#[test]
fn deleting_a_directory_row_keeps_the_file_record_of_the_same_name() {
    let dir = tempfile::tempdir().unwrap();
    let path = clashing_zip(dir.path());
    let fs = ArchiveFs::open_path(&path, ArchiveOptions::default()).unwrap();
    assert_eq!(names(&fs, ""), set(&["ok.txt", "x", "x~1"]));
    assert_eq!(read(&fs, "x~1"), b"AAAA");

    fs.apply_edits(
        &[ArchiveEdit::Delete {
            path: VfsPath::parse("x").unwrap(),
        }],
        &Cancel::new(),
    )
    .unwrap();

    let reopened = ArchiveFs::open_path(&path, ArchiveOptions::default()).unwrap();
    assert_eq!(names(&reopened, ""), set(&["ok.txt", "x"]));
    assert_eq!(read(&reopened, "x"), b"AAAA");
    assert_eq!(read(&reopened, "ok.txt"), b"ok");
}

/// Renaming the directory row moves the directory's records and leaves the
/// file record stored under the old name.
#[test]
fn renaming_a_directory_row_keeps_the_file_record_of_the_same_name() {
    let dir = tempfile::tempdir().unwrap();
    let path = clashing_zip(dir.path());
    let fs = ArchiveFs::open_path(&path, ArchiveOptions::default()).unwrap();

    fs.rename(
        &VfsPath::parse("x").unwrap(),
        &VfsPath::parse("w").unwrap(),
        &Cancel::new(),
    )
    .unwrap();

    let reopened = ArchiveFs::open_path(&path, ArchiveOptions::default()).unwrap();
    assert_eq!(names(&reopened, ""), set(&["ok.txt", "w", "x"]));
    assert_eq!(read(&reopened, "w/y.txt"), b"y");
    assert_eq!(read(&reopened, "x"), b"AAAA");
}

/// Deleting through the file system interface resolves the row the same way.
#[test]
fn a_single_delete_of_a_directory_row_keeps_the_file_record() {
    let dir = tempfile::tempdir().unwrap();
    let path = clashing_zip(dir.path());
    let fs = ArchiveFs::open_path(&path, ArchiveOptions::default()).unwrap();

    fs.delete(&VfsPath::parse("x").unwrap(), &Cancel::new())
        .unwrap();

    assert_eq!(names(&fs, ""), set(&["ok.txt", "x"]));
    assert_eq!(read(&fs, "x"), b"AAAA");
}

/// A delete of the file row `x~1` removes the record stored as `x` and
/// leaves the directory `x`.
#[test]
fn deleting_the_moved_aside_file_row_removes_that_record() {
    let dir = tempfile::tempdir().unwrap();
    let path = clashing_zip(dir.path());
    let fs = ArchiveFs::open_path(&path, ArchiveOptions::default()).unwrap();

    fs.apply_edits(
        &[ArchiveEdit::Delete {
            path: VfsPath::parse("x~1").unwrap(),
        }],
        &Cancel::new(),
    )
    .unwrap();

    let reopened = ArchiveFs::open_path(&path, ArchiveOptions::default()).unwrap();
    assert_eq!(names(&reopened, ""), set(&["ok.txt", "x"]));
    assert_eq!(read(&reopened, "x/y.txt"), b"y");
}

/// A rename of the file row `x~1` moves the record stored as `x`.
#[test]
fn renaming_the_moved_aside_file_row_moves_that_record() {
    let dir = tempfile::tempdir().unwrap();
    let path = clashing_zip(dir.path());
    let fs = ArchiveFs::open_path(&path, ArchiveOptions::default()).unwrap();

    fs.apply_edits(
        &[ArchiveEdit::Rename {
            from: VfsPath::parse("x~1").unwrap(),
            to: VfsPath::parse("z").unwrap(),
        }],
        &Cancel::new(),
    )
    .unwrap();

    let reopened = ArchiveFs::open_path(&path, ArchiveOptions::default()).unwrap();
    assert_eq!(names(&reopened, ""), set(&["ok.txt", "x", "z"]));
    assert_eq!(read(&reopened, "z"), b"AAAA");
    assert_eq!(read(&reopened, "x/y.txt"), b"y");
}

/// A write to the file row `x~1` replaces the content of the record stored
/// as `x`. It adds no second record, so the row keeps its name.
#[test]
fn writing_to_the_moved_aside_file_row_replaces_that_record() {
    let dir = tempfile::tempdir().unwrap();
    let path = clashing_zip(dir.path());
    let fs = ArchiveFs::open_path(&path, ArchiveOptions::default()).unwrap();

    fs.apply_edits(
        &[ArchiveEdit::WriteFile {
            path: VfsPath::parse("x~1").unwrap(),
            content: b"NEW".to_vec(),
            modified: None,
        }],
        &Cancel::new(),
    )
    .unwrap();

    let reopened = ArchiveFs::open_path(&path, ArchiveOptions::default()).unwrap();
    assert_eq!(names(&reopened, ""), set(&["ok.txt", "x", "x~1"]));
    assert_eq!(read(&reopened, "x~1"), b"NEW");
    assert_eq!(read(&reopened, "x/y.txt"), b"y");
    let entry = reopened.metadata(&VfsPath::parse("x").unwrap()).unwrap();
    assert!(entry.is_dir());
}

/// The single write of the file system interface resolves the row the same
/// way as a batch.
#[test]
fn a_single_write_to_the_moved_aside_file_row_replaces_that_record() {
    let dir = tempfile::tempdir().unwrap();
    let path = clashing_zip(dir.path());
    let fs = ArchiveFs::open_path(&path, ArchiveOptions::default()).unwrap();

    let mut content: &[u8] = b"NEW";
    fs.write_file(
        &VfsPath::parse("x~1").unwrap(),
        &mut content,
        &Cancel::new(),
    )
    .unwrap();

    assert_eq!(names(&fs, ""), set(&["ok.txt", "x", "x~1"]));
    assert_eq!(read(&fs, "x~1"), b"NEW");
}

/// A write to a directory row is refused, so it cannot replace the file record
/// stored under the directory's name.
#[test]
fn a_write_to_a_directory_row_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = clashing_zip(dir.path());
    let fs = ArchiveFs::open_path(&path, ArchiveOptions::default()).unwrap();

    let error = fs
        .apply_edits(
            &[ArchiveEdit::WriteFile {
                path: VfsPath::parse("x").unwrap(),
                content: b"NEW".to_vec(),
                modified: None,
            }],
            &Cancel::new(),
        )
        .unwrap_err();
    assert!(matches!(error, VfsError::IsADirectory { .. }), "{error:?}");
    assert_eq!(fs.rewrite_count(), 0);
    assert_eq!(read(&fs, "x~1"), b"AAAA");
}

/// Adding the directory row again keeps the file record stored under its
/// name, and a directory added over a file row is refused.
#[test]
fn a_directory_added_under_a_file_name_keeps_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = clashing_zip(dir.path());
    let fs = ArchiveFs::open_path(&path, ArchiveOptions::default()).unwrap();

    fs.apply_edits(
        &[ArchiveEdit::CreateDir {
            path: VfsPath::parse("x").unwrap(),
        }],
        &Cancel::new(),
    )
    .unwrap();
    let reopened = ArchiveFs::open_path(&path, ArchiveOptions::default()).unwrap();
    assert_eq!(names(&reopened, ""), set(&["ok.txt", "x", "x~1"]));
    assert_eq!(read(&reopened, "x~1"), b"AAAA");

    let error = reopened
        .create_dir(&VfsPath::parse("ok.txt").unwrap(), &Cancel::new())
        .unwrap_err();
    assert!(matches!(error, VfsError::AlreadyExists { .. }), "{error:?}");
    assert_eq!(read(&reopened, "ok.txt"), b"ok");
}

/// A move that would give a record a path past the length ceiling fails the
/// whole batch and leaves the container as it was.
#[test]
fn a_rename_past_the_path_ceiling_keeps_every_record() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deep.zip");
    std::fs::write(&path, zip_bytes(&[("d/f.txt", b"f"), ("ok.txt", b"ok")])).unwrap();
    let fs = ArchiveFs::open_path(&path, ArchiveOptions::default()).unwrap();

    let long = "n".repeat(ca_vfs::MAX_PATH_BYTES - 2);
    let error = fs
        .apply_edits(
            &[ArchiveEdit::Rename {
                from: VfsPath::parse("d").unwrap(),
                to: VfsPath::parse(&long).unwrap(),
            }],
            &Cancel::new(),
        )
        .unwrap_err();
    assert!(matches!(error, VfsError::InvalidPath(_)), "{error:?}");

    let reopened = ArchiveFs::open_path(&path, ArchiveOptions::default()).unwrap();
    assert_eq!(names(&reopened, ""), set(&["d", "ok.txt"]));
    assert_eq!(read(&reopened, "d/f.txt"), b"f");
}

/// A file and a directory stored under one name are told so, not told that
/// their names differ by case.
#[test]
fn a_file_and_a_directory_of_one_name_are_not_called_a_case_clash() {
    let fs = open_memory(
        zip_manual(
            &[
                ManualEntry::new("x/", b""),
                ManualEntry::new("x", b"FILE"),
                ManualEntry::new("x/y.txt", b"y"),
            ],
            &[],
        ),
        "kinds.zip",
    )
    .unwrap();
    let root = fs.list(&VfsPath::root(), &Cancel::new()).unwrap();
    let directory = root
        .iter()
        .find(|entry| entry.is_dir())
        .expect("the directory row");
    let text = directory.error.as_deref().unwrap_or_default();
    assert!(
        text.contains("a file and a directory in the container have the same name: x"),
        "{text}"
    );
    assert!(!text.contains("differs only by case"), "{text}");
    let file = root
        .iter()
        .find(|entry| !entry.is_dir())
        .expect("the file row");
    assert!(
        !file
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("differs only by case"),
        "{file:?}"
    );
}

#[test]
fn an_in_memory_container_refuses_writes() {
    let fs = open_memory(zip_bytes(&[("a.txt", b"x")]), "fixture.zip").unwrap();
    assert!(!fs.capabilities().writable);
    let error = fs
        .create_dir(&VfsPath::parse("new").unwrap(), &Cancel::new())
        .unwrap_err();
    assert!(matches!(error, VfsError::ReadOnly), "unexpected {error}");
}

#[test]
fn read_only_formats_report_unsupported_writes() {
    let fs = open_memory(sevenz_bytes(&[("a.txt", HELLO)]), "fixture.7z").unwrap();
    let error = fs
        .create_dir(&VfsPath::parse("new").unwrap(), &Cancel::new())
        .unwrap_err();
    assert!(
        matches!(error, VfsError::Unsupported { .. }),
        "unexpected {error}"
    );
}

#[test]
fn an_unknown_container_is_unsupported() {
    let error = open_memory(b"not an archive at all".to_vec(), "mystery.dat").unwrap_err();
    assert!(
        matches!(error, VfsError::Unsupported { .. }),
        "unexpected {error}"
    );
}

#[test]
fn a_format_without_a_decoder_is_unsupported() {
    let mut bytes = b"ITSF".to_vec();
    bytes.resize(64, 0);
    let error = open_memory(bytes, "fixture.chm").unwrap_err();
    assert!(
        matches!(error, VfsError::Unsupported { .. }),
        "unexpected {error}"
    );
}

fn at(path: &str) -> VfsPath {
    VfsPath::parse(path).unwrap()
}

/// A single rename onto a name the listing holds is refused before anything
/// is rewritten, whatever record the row stands for.
#[test]
fn a_single_rename_onto_a_listed_name_is_refused_as_already_exists() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("two.zip");
    std::fs::write(
        &path,
        zip_bytes(&[("a.txt", b"from a"), ("b.txt", b"from b")]),
    )
    .unwrap();
    let fs = ArchiveFs::open_path(&path, ArchiveOptions::default()).unwrap();
    let error = fs
        .rename(&at("a.txt"), &at("b.txt"), &Cancel::new())
        .unwrap_err();
    assert!(matches!(error, VfsError::AlreadyExists { .. }), "{error:?}");
    assert_eq!(fs.rewrite_count(), 0);
    assert_eq!(read(&fs, "a.txt"), b"from a");
    assert_eq!(read(&fs, "b.txt"), b"from b");

    let clash = clashing_zip(dir.path());
    let fs = ArchiveFs::open_path(&clash, ArchiveOptions::default()).unwrap();
    let error = fs
        .rename(&at("ok.txt"), &at("x~1"), &Cancel::new())
        .unwrap_err();
    assert!(matches!(error, VfsError::AlreadyExists { .. }), "{error:?}");
    assert_eq!(fs.rewrite_count(), 0);
    assert_eq!(names(&fs, ""), set(&["ok.txt", "x", "x~1"]));
}

/// A batched rename onto a listed file row replaces that row's record, as a
/// move over an existing file does on a disk.
#[test]
fn a_batched_rename_onto_a_file_row_replaces_that_record() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("two.zip");
    std::fs::write(
        &path,
        zip_bytes(&[("a.txt", b"from a"), ("b.txt", b"from b")]),
    )
    .unwrap();
    let fs = ArchiveFs::open_path(&path, ArchiveOptions::default()).unwrap();
    fs.apply_edits(
        &[ArchiveEdit::Rename {
            from: at("a.txt"),
            to: at("b.txt"),
        }],
        &Cancel::new(),
    )
    .unwrap();

    let reopened = ArchiveFs::open_path(&path, ArchiveOptions::default()).unwrap();
    assert_eq!(names(&reopened, ""), set(&["b.txt"]));
    assert_eq!(read(&reopened, "b.txt"), b"from a");
}

/// A batched rename onto the file row `x~1` replaces the record stored as
/// `x`. It adds no second record, so the row keeps its name.
#[test]
fn a_batched_rename_onto_the_moved_aside_file_row_replaces_that_record() {
    let dir = tempfile::tempdir().unwrap();
    let path = clashing_zip(dir.path());
    let fs = ArchiveFs::open_path(&path, ArchiveOptions::default()).unwrap();
    fs.apply_edits(
        &[ArchiveEdit::Rename {
            from: at("ok.txt"),
            to: at("x~1"),
        }],
        &Cancel::new(),
    )
    .unwrap();

    let reopened = ArchiveFs::open_path(&path, ArchiveOptions::default()).unwrap();
    assert_eq!(names(&reopened, ""), set(&["x", "x~1"]));
    assert_eq!(read(&reopened, "x~1"), b"ok");
    assert_eq!(read(&reopened, "x/y.txt"), b"y");
    assert!(reopened.metadata(&at("x")).unwrap().is_dir());
}

/// A batched rename onto a directory row is refused, so a file record never
/// lands under a directory's name.
#[test]
fn a_batched_rename_onto_a_directory_row_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = clashing_zip(dir.path());
    let fs = ArchiveFs::open_path(&path, ArchiveOptions::default()).unwrap();
    let error = fs
        .apply_edits(
            &[ArchiveEdit::Rename {
                from: at("ok.txt"),
                to: at("x"),
            }],
            &Cancel::new(),
        )
        .unwrap_err();
    assert!(matches!(error, VfsError::AlreadyExists { .. }), "{error:?}");
    assert_eq!(fs.rewrite_count(), 0);
    assert_eq!(names(&fs, ""), set(&["ok.txt", "x", "x~1"]));
}

/// A batch that moves a file away and then moves another file onto its name
/// keeps both contents: the move of the first record wins over the drop.
#[test]
fn a_batched_rename_onto_a_row_the_batch_moves_away_keeps_both_records() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("two.zip");
    std::fs::write(
        &path,
        zip_bytes(&[("a.txt", b"from a"), ("b.txt", b"from b")]),
    )
    .unwrap();
    let fs = ArchiveFs::open_path(&path, ArchiveOptions::default()).unwrap();
    fs.apply_edits(
        &[
            ArchiveEdit::Rename {
                from: at("b.txt"),
                to: at("c.txt"),
            },
            ArchiveEdit::Rename {
                from: at("a.txt"),
                to: at("b.txt"),
            },
        ],
        &Cancel::new(),
    )
    .unwrap();

    let reopened = ArchiveFs::open_path(&path, ArchiveOptions::default()).unwrap();
    assert_eq!(names(&reopened, ""), set(&["b.txt", "c.txt"]));
    assert_eq!(read(&reopened, "b.txt"), b"from a");
    assert_eq!(read(&reopened, "c.txt"), b"from b");
}
