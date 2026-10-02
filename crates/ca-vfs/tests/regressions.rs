//! Regression cases for this crate.
//!
//! Every test here describes behaviour a caller depends on, not the shape of
//! the code that provides it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_precision_loss,
    clippy::cast_lossless,
    clippy::default_trait_access,
    clippy::field_reassign_with_default
)]

mod support;

use std::io::Read;
use std::time::{Duration, Instant};

use ca_vfs::{
    walk, ArchiveFs, ArchiveOptions, Cancel, Capabilities, EntryKind, FileSystem, LimitKind,
    LocalFs, OpenFile, PathError, Snapshot, SnapshotFs, SnapshotOptions, TimeFidelity, VfsEntry,
    VfsError, VfsLinkKind, VfsPath, MAX_COMPONENTS,
};
use support::{
    extended_timestamp, gzip, gzip_two_members, open_memory, open_memory_with, sevenz_many,
    tar_bytes, tar_gz_many, zip_bytes, zip_manual, ManualEntry,
};

// --- deeply nested names -------------------------------------------------

#[test]
fn a_name_with_thousands_of_components_is_refused_rather_than_followed() {
    let deep = "a/".repeat(2000) + "f.txt";
    let error = VfsPath::parse(&deep).unwrap_err();
    assert!(matches!(error, PathError::TooManyComponents { .. }));
}

#[test]
fn a_container_holding_a_deeply_nested_name_still_opens() {
    let deep = "a/".repeat(2000) + "f.txt";
    let bytes = zip_bytes(&[(deep.as_str(), b"x" as &[u8]), ("ok.txt", b"fine")]);
    let fs = open_memory(bytes, "deep.zip").unwrap();
    let root = fs.list(&VfsPath::root(), &Cancel::new()).unwrap();
    assert!(root.iter().any(|entry| entry.name == "ok.txt"));
}

#[test]
fn a_name_just_inside_the_component_cap_is_accepted() {
    let deep = "a/".repeat(MAX_COMPONENTS - 1) + "f.txt";
    assert!(VfsPath::parse(&deep).is_ok());
}

// --- snapshot payload bounds ---------------------------------------------

/// A snapshot header in front of `body`, claiming `declared` payload bytes.
fn snapshot_with_header(declared: u64, compression: u8, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"CA-VFSSN");
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.push(compression);
    out.push(0);
    out.extend_from_slice(&declared.to_le_bytes());
    out.extend_from_slice(body);
    out
}

#[test]
fn a_snapshot_claiming_a_huge_payload_is_refused_before_it_is_believed() {
    let body = Vec::new();
    let bytes = snapshot_with_header(u64::MAX / 2, 1, &body);
    let error = Snapshot::read_from(&mut bytes.as_slice()).unwrap_err();
    assert!(
        matches!(
            error,
            VfsError::LimitExceeded {
                kind: LimitKind::EntrySize,
                ..
            }
        ),
        "unexpected {error}"
    );
}

#[test]
fn a_small_snapshot_that_inflates_enormously_stops_at_the_ceiling() {
    // A megabyte of zeros deflates to a few kilobytes, so the file is small
    // and the payload it would produce is not.
    let payload = vec![b'0'; 64 * 1024 * 1024];
    let compressed = {
        use std::io::Write;
        let mut encoder =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
        encoder.write_all(&payload).unwrap();
        encoder.finish().unwrap()
    };
    assert!(
        compressed.len() < 128 * 1024,
        "the fixture should be far smaller than what it expands to"
    );

    let bytes = snapshot_with_header(0, 1, &compressed);
    let mut limits = ca_vfs::Limits::default();
    limits.max_entry_bytes = 1024 * 1024;

    // The error is raised at the ceiling. Reaching it at all proves the read
    // stopped there rather than inflating the whole stream.
    let error = Snapshot::read_from_with(&mut bytes.as_slice(), &limits).unwrap_err();
    assert!(
        matches!(
            error,
            VfsError::LimitExceeded {
                kind: LimitKind::EntrySize,
                ..
            }
        ),
        "unexpected {error}"
    );
}

// --- tar reads -----------------------------------------------------------

#[test]
fn every_entry_of_a_compressed_tar_stays_readable() {
    let count = 64;
    let bytes = tar_gz_many(count);
    let mut options = ArchiveOptions::default();
    // Small enough that charging the whole stream to each read would run out
    // long before the last entry.
    options.limits.max_archive_bytes = 4 * 1024 * 1024;
    let fs = open_memory_with(bytes, "many.tar.gz", options).unwrap();

    for index in 0..count {
        let path = VfsPath::parse(&format!("d{}/f{index}.txt", index % 16)).unwrap();
        let mut open = fs
            .open(&path, &Cancel::new())
            .unwrap_or_else(|error| panic!("entry {index}: {error}"));
        let mut text = String::new();
        open.read_to_string(&mut text).unwrap();
        assert_eq!(text, format!("body {index}"));
    }
}

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "wall-clock budget holds for release builds"
)]
fn reading_a_compressed_tar_end_to_end_is_not_a_pass_per_entry() {
    let count = 4_000;
    let fs = open_memory(tar_gz_many(count), "many.tar.gz").unwrap();

    let started = Instant::now();
    for index in 0..count {
        let path = VfsPath::parse(&format!("d{}/f{index}.txt", index % 16)).unwrap();
        let mut open = fs.open(&path, &Cancel::new()).unwrap();
        let mut sink = Vec::new();
        open.read_to_end(&mut sink).unwrap();
    }
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(60),
        "reading {count} entries took {elapsed:?}; a pass over the stream per entry is the cause"
    );
}

#[test]
fn an_uncompressed_tar_reads_an_entry_without_walking_the_whole_container() {
    let bodies: Vec<(String, Vec<u8>)> = (0..500)
        .map(|index| (format!("f{index}.txt"), vec![b'x'; 4096]))
        .collect();
    let borrowed: Vec<(&str, &[u8])> = bodies
        .iter()
        .map(|(name, body)| (name.as_str(), body.as_slice()))
        .collect();
    let fs = open_memory(tar_bytes(&borrowed), "plain.tar").unwrap();

    let path = VfsPath::parse("f499.txt").unwrap();
    let mut open = fs.open(&path, &Cancel::new()).unwrap();
    let mut out = Vec::new();
    open.read_to_end(&mut out).unwrap();
    assert_eq!(out.len(), 4096);
}

// --- 7z solid blocks -----------------------------------------------------

#[test]
fn every_entry_of_a_small_solid_container_reads_back_its_own_body() {
    let count = 32;
    let fs = open_memory(sevenz_many(count), "many.7z").unwrap();
    for index in 0..count {
        let path = VfsPath::parse(&format!("f{index}.txt")).unwrap();
        let mut open = fs
            .open(&path, &Cancel::new())
            .unwrap_or_else(|error| panic!("entry {index}: {error}"));
        let mut text = String::new();
        open.read_to_string(&mut text).unwrap();
        assert_eq!(text, format!("body {index}"));
    }
}

#[test]
fn a_cancelled_read_of_a_small_solid_container_is_refused() {
    let fs = open_memory(sevenz_many(32), "many.7z").unwrap();
    let cancel = Cancel::new();
    cancel.cancel();
    let error = fs
        .open(&VfsPath::parse("f31.txt").unwrap(), &cancel)
        .unwrap_err();
    assert!(matches!(error, VfsError::Cancelled), "unexpected {error}");
}

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "wall-clock budget holds for release builds"
)]
fn reading_a_solid_container_end_to_end_is_not_a_pass_per_entry() {
    let count = 1_500;
    let fs = open_memory(sevenz_many(count), "many.7z").unwrap();

    let started = Instant::now();
    for index in 0..count {
        let path = VfsPath::parse(&format!("f{index}.txt")).unwrap();
        let mut open = fs
            .open(&path, &Cancel::new())
            .unwrap_or_else(|error| panic!("entry {index}: {error}"));
        let mut text = String::new();
        open.read_to_string(&mut text).unwrap();
        assert_eq!(text, format!("body {index}"));
    }
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(60),
        "reading {count} entries took {elapsed:?}; decoding the block once per entry is the cause"
    );
}

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "wall-clock budget holds for release builds"
)]
fn a_cancelled_read_of_a_solid_container_returns_promptly() {
    let fs = open_memory(sevenz_many(2_000), "many.7z").unwrap();
    let cancel = Cancel::new();
    cancel.cancel();

    let started = Instant::now();
    let error = fs
        .open(&VfsPath::parse("f1999.txt").unwrap(), &cancel)
        .unwrap_err();
    assert!(matches!(error, VfsError::Cancelled), "unexpected {error}");
    assert!(started.elapsed() < Duration::from_secs(10));
}

// --- a file and a directory of the same name -----------------------------

#[test]
fn a_container_holding_both_a_file_and_a_directory_of_one_name_loses_neither() {
    for order in [
        vec![("a", b"file" as &[u8]), ("a/b.txt", b"under")],
        vec![("a/b.txt", b"under" as &[u8]), ("a", b"file")],
    ] {
        let fs = open_memory(zip_bytes(&order), "clash.zip").unwrap();
        let root = fs.list(&VfsPath::root(), &Cancel::new()).unwrap();
        assert_eq!(root.len(), 2, "both entries are listed: {root:?}");

        let under = fs
            .list(&VfsPath::parse("a").unwrap(), &Cancel::new())
            .expect("the directory lists");
        assert_eq!(under.len(), 1);
        assert_eq!(under[0].name, "b.txt");

        let shadowed = root
            .iter()
            .find(|entry| entry.kind == EntryKind::File)
            .expect("the file survives under another name");
        assert!(shadowed.error.is_some(), "and says why it was renamed");
    }
}

#[test]
fn a_snapshot_holding_both_a_file_and_a_directory_of_one_name_loses_neither() {
    let snapshot = Snapshot {
        origin: "origin".to_owned(),
        captured: None,
        has_crc: false,
        entries: vec![
            record("a", false),
            record("a/b.txt", false),
            record("other.txt", false),
        ],
        unknown: Default::default(),
    };
    let fs = SnapshotFs::new(snapshot);
    let root = fs.list(&VfsPath::root(), &Cancel::new()).unwrap();
    assert_eq!(root.len(), 3);
    let under = fs
        .list(&VfsPath::parse("a").unwrap(), &Cancel::new())
        .unwrap();
    assert_eq!(under.len(), 1);
}

fn record(path: &str, dir: bool) -> ca_vfs::snapshot::SnapshotRecord {
    ca_vfs::snapshot::SnapshotRecord {
        path: path.to_owned(),
        dir,
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
        unknown: Default::default(),
    }
}

// --- checksums -----------------------------------------------------------

#[test]
fn a_stored_checksum_that_disagrees_with_the_content_is_reported() {
    let bytes = zip_manual(
        &[ManualEntry::new("f.txt", b"hello").with_crc(0xDEAD_BEEF)],
        &[],
    );
    let fs = open_memory(bytes, "bad.zip").unwrap();
    // The whole entry is expanded when it is opened, so the check lands here.
    let error = fs
        .open(&VfsPath::parse("f.txt").unwrap(), &Cancel::new())
        .expect_err("reading an entry whose checksum disagrees should not succeed");
    assert!(
        matches!(error, VfsError::ChecksumMismatch { .. }),
        "unexpected {error}"
    );
}

#[test]
fn an_entry_with_no_recorded_checksum_offers_none() {
    let bytes = zip_manual(&[ManualEntry::new("f.txt", b"hello").with_crc(0)], &[]);
    let fs = open_memory(bytes, "nocrc.zip").unwrap();
    let entry = fs.metadata(&VfsPath::parse("f.txt").unwrap()).unwrap();
    assert_eq!(
        entry.crc32, None,
        "a zero checksum is absence, not a value two entries can share"
    );
}

#[test]
fn a_matching_checksum_reads_through() {
    let bytes = zip_manual(&[ManualEntry::new("f.txt", b"hello")], &[]);
    let fs = open_memory(bytes, "ok.zip").unwrap();
    let mut open = fs
        .open(&VfsPath::parse("f.txt").unwrap(), &Cancel::new())
        .unwrap();
    let mut out = Vec::new();
    open.read_to_end(&mut out).unwrap();
    assert_eq!(out, b"hello");
}

// --- unverified sizes ----------------------------------------------------

#[test]
fn a_gzip_size_taken_from_the_footer_is_marked_unproven() {
    let bytes = gzip_two_members(b"1234", b"0123456789");
    let fs = open_memory(bytes, "two.gz").unwrap();
    let entries = fs.list(&VfsPath::root(), &Cancel::new()).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(
        !entries[0].size_is_exact,
        "the footer describes one member and nothing checked it"
    );
}

#[test]
fn a_counted_size_is_exact() {
    let fs = open_memory(support::xz(b"0123456789"), "one.xz").unwrap();
    let entries = fs.list(&VfsPath::root(), &Cancel::new()).unwrap();
    assert!(entries[0].size_is_exact);
    assert_eq!(entries[0].size, 10);
}

#[test]
fn a_single_member_gzip_still_reads_its_whole_content() {
    let fs = open_memory(gzip(b"0123456789"), "one.gz").unwrap();
    let mut open = fs
        .open(&VfsPath::parse("one").unwrap(), &Cancel::new())
        .unwrap();
    let mut out = Vec::new();
    open.read_to_end(&mut out).unwrap();
    assert_eq!(out, b"0123456789");
}

// --- timestamps ----------------------------------------------------------

#[test]
fn a_utc_timestamp_extra_field_is_preferred_over_the_dos_stamp() {
    let bytes = zip_manual(
        &[ManualEntry::new("f.txt", b"x").with_extra(extended_timestamp(1_600_000_000))],
        &[],
    );
    let fs = open_memory(bytes, "stamped.zip").unwrap();
    let entry = fs.metadata(&VfsPath::parse("f.txt").unwrap()).unwrap();
    assert_eq!(entry.time_fidelity, TimeFidelity::Utc);
    let seconds = entry
        .modified
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert_eq!(seconds, 1_600_000_000);
}

#[test]
fn a_dos_stamp_is_marked_as_a_local_wall_clock() {
    let bytes = zip_manual(&[ManualEntry::new("f.txt", b"x")], &[]);
    let fs = open_memory(bytes, "dos.zip").unwrap();
    let entry = fs.metadata(&VfsPath::parse("f.txt").unwrap()).unwrap();
    assert_eq!(
        entry.time_fidelity,
        TimeFidelity::LocalTwoSecond,
        "so a comparison against a live folder can allow a tolerance"
    );
}

// --- duplicate and colliding names ---------------------------------------

#[test]
fn two_entries_stored_under_one_name_are_both_listed() {
    let bytes = zip_manual(
        &[
            ManualEntry::new("dup.txt", b"first"),
            ManualEntry::new("dup.txt", b"second"),
        ],
        &[],
    );
    let fs = open_memory(bytes, "dup.zip").unwrap();
    let root = fs.list(&VfsPath::root(), &Cancel::new()).unwrap();
    assert_eq!(root.len(), 2, "neither entry disappears: {root:?}");
    assert!(
        root.iter().any(|entry| entry.error.is_some()),
        "and the one that could not keep the name says so"
    );
}

/// A zip64 container the writer produces for `entries`, with every occurrence
/// of the name `from` in its headers replaced by `to`, which has the same
/// length. The writer refuses to store one name twice, so this is how a
/// container holding a repeated name is made.
fn zip64_with_renamed(entries: &[(&str, &[u8])], from: &str, to: &str) -> Vec<u8> {
    use std::io::Write as _;

    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    writer.set_raw_zip64_extensible_data_sector(Vec::new().into_boxed_slice());
    for (name, content) in entries {
        writer.start_file(*name, options).unwrap();
        writer.write_all(content).unwrap();
    }
    let mut bytes = writer.finish().unwrap().into_inner();
    assert_eq!(from.len(), to.len());
    let mut at = 0;
    while let Some(found) = bytes.get(at..).and_then(|rest| {
        rest.windows(from.len())
            .position(|window| window == from.as_bytes())
    }) {
        let start = at + found;
        bytes[start..start + to.len()].copy_from_slice(to.as_bytes());
        at = start + to.len();
    }
    bytes
}

#[test]
fn two_entries_stored_under_one_name_in_a_zip64_container_are_both_listed() {
    let bytes = zip64_with_renamed(
        &[("dup1.txt", b"first"), ("dup2.txt", b"second!")],
        "dup2.txt",
        "dup1.txt",
    );
    assert!(
        bytes
            .windows(4)
            .any(|window| window == 0x0606_4b50u32.to_le_bytes()),
        "the container carries a zip64 end record"
    );
    let fs = open_memory(bytes, "dup64.zip").unwrap();
    let root = fs.list(&VfsPath::root(), &Cancel::new()).unwrap();
    assert_eq!(root.len(), 2, "neither entry disappears: {root:?}");
    let shadowed = root
        .iter()
        .find(|entry| entry.error.is_some())
        .expect("the entry that could not keep the name says so");
    assert_eq!(
        shadowed.size, 5,
        "the shadowed row describes the first record"
    );
    let readable = root
        .iter()
        .find(|entry| entry.error.is_none())
        .expect("one row keeps the name");
    assert_eq!(readable.name, "dup1.txt");
    let mut content = Vec::new();
    fs.open(&readable.path, &Cancel::new())
        .unwrap()
        .read_to_end(&mut content)
        .unwrap();
    assert_eq!(content, b"second!");
}

#[test]
fn a_file_moved_aside_by_a_directory_reads_its_own_content_in_every_format() {
    let entries: [(&str, &[u8]); 3] = [
        ("src", b"AAAA"),
        ("src-x", b"x"),
        ("src/main.rs", b"fn main() {}"),
    ];
    let manual: Vec<ManualEntry> = entries
        .iter()
        .map(|(name, content)| ManualEntry::new(name, content))
        .collect();
    for (bytes, label) in [
        (zip_manual(&manual, &[]), "clash.zip"),
        (support::tar_with_raw_names(&entries), "clash.tar"),
        (support::sevenz_bytes(&entries), "clash.7z"),
    ] {
        let fs = open_memory(bytes, label).unwrap();
        for (listed, expected) in [
            ("src~1", b"AAAA" as &[u8]),
            ("src/main.rs", b"fn main() {}"),
        ] {
            let mut content = Vec::new();
            fs.open(&VfsPath::parse(listed).unwrap(), &Cancel::new())
                .unwrap_or_else(|error| panic!("{label}: {listed}: {error}"))
                .read_to_end(&mut content)
                .unwrap();
            assert_eq!(content, expected, "{label}: {listed}");
        }
    }
}

#[test]
fn two_entries_stored_under_one_name_read_back_their_own_content() {
    let entries: [(&str, &[u8]); 2] = [("dup.txt", b"first"), ("dup.txt", b"second")];
    for (bytes, label) in [
        (support::tar_with_raw_names(&entries), "dup.tar"),
        (support::sevenz_bytes(&entries), "dup.7z"),
    ] {
        let fs = open_memory(bytes, label).unwrap();
        let root = fs.list(&VfsPath::root(), &Cancel::new()).unwrap();
        assert_eq!(root.len(), 2, "{label}: {root:?}");
        for (listed, expected) in [("dup.txt", b"first" as &[u8]), ("dup.txt~1", b"second")] {
            let mut content = Vec::new();
            fs.open(&VfsPath::parse(listed).unwrap(), &Cancel::new())
                .unwrap_or_else(|error| panic!("{label}: {listed}: {error}"))
                .read_to_end(&mut content)
                .unwrap();
            assert_eq!(content, expected, "{label}: {listed}");
        }
    }
}

#[test]
fn a_zip_that_stores_one_name_twice_refuses_a_write() {
    let bytes = zip_manual(
        &[
            ManualEntry::new("dup.txt", b"first"),
            ManualEntry::new("dup.txt", b"second"),
        ],
        &[],
    );
    let (dir, fs) = on_disk(bytes.clone(), "dup.zip");
    let error = fs
        .write_file(
            &VfsPath::parse("new.txt").unwrap(),
            &mut b"added".as_slice(),
            &Cancel::new(),
        )
        .unwrap_err();
    assert!(
        matches!(error, VfsError::Unsupported { .. }),
        "unexpected {error}"
    );
    assert_eq!(std::fs::read(dir.path().join("dup.zip")).unwrap(), bytes);
}

#[test]
fn a_snapshot_of_a_container_keeps_the_refusal_of_a_row() {
    let bytes = zip_manual(
        &[
            ManualEntry::new("ok.txt", b"fine"),
            ManualEntry::new("log-12:30.txt", b"x"),
        ],
        &[],
    );
    let fs = open_memory(bytes, "names.zip").unwrap();
    let snapshot = Snapshot::capture(
        &fs,
        &VfsPath::root(),
        SnapshotOptions::default(),
        &Cancel::new(),
    )
    .unwrap();
    let record = snapshot
        .entries
        .iter()
        .find(|record| record.path == "log-12_30.txt")
        .expect("the refused row is recorded");
    assert!(record.refused);
    let recorded = SnapshotFs::new(snapshot);
    let path = VfsPath::parse("log-12_30.txt").unwrap();
    assert!(recorded.metadata(&path).unwrap().refused);
    assert!(matches!(
        recorded.open(&path, &Cancel::new()),
        Err(VfsError::Refused { .. })
    ));
}

#[test]
fn names_that_differ_only_by_case_are_flagged() {
    let bytes = zip_manual(
        &[
            ManualEntry::new("README.txt", b"first"),
            ManualEntry::new("readme.txt", b"second"),
        ],
        &[],
    );
    let fs = open_memory(bytes, "case.zip").unwrap();
    let root = fs.list(&VfsPath::root(), &Cancel::new()).unwrap();
    assert_eq!(root.len(), 2);
    assert!(
        root.iter().all(|entry| entry
            .error
            .as_deref()
            .is_some_and(|text| text.contains("differs only by case"))),
        "both sides of the collision are flagged: {root:?}"
    );
}

// --- writing -------------------------------------------------------------

/// A zip written to a real file, which is the only kind this crate rewrites.
fn on_disk(bytes: Vec<u8>, name: &str) -> (tempfile::TempDir, ArchiveFs) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    std::fs::write(&path, bytes).unwrap();
    let fs = ArchiveFs::open_path(&path, ArchiveOptions::default()).unwrap();
    (dir, fs)
}

#[test]
fn the_archive_comment_survives_a_write() {
    let bytes = zip_manual(
        &[ManualEntry::new("f.txt", b"x")],
        b"a comment worth keeping",
    );
    let (dir, fs) = on_disk(bytes, "commented.zip");
    fs.write_file(
        &VfsPath::parse("new.txt").unwrap(),
        &mut b"added".as_slice(),
        &Cancel::new(),
    )
    .unwrap();

    let written = std::fs::read(dir.path().join("commented.zip")).unwrap();
    let archive = zip::ZipArchive::new(std::io::Cursor::new(written)).unwrap();
    assert_eq!(archive.comment(), b"a comment worth keeping");
}

#[test]
fn a_zip_holding_encrypted_entries_refuses_a_write() {
    let bytes = support::zip_encrypted("secret.txt", b"classified", "pw");
    let (_dir, fs) = on_disk(bytes, "secret.zip");
    let error = fs
        .write_file(
            &VfsPath::parse("plain.txt").unwrap(),
            &mut b"in the clear".as_slice(),
            &Cancel::new(),
        )
        .unwrap_err();
    assert!(
        matches!(error, VfsError::Unsupported { .. }),
        "unexpected {error}"
    );
}

#[test]
fn a_zip_holding_an_unusable_name_refuses_a_write() {
    let bytes = zip_manual(
        &[
            ManualEntry::new("d/keep.txt", b"x"),
            ManualEntry::new("", b"y").with_raw_name(b"d/../escape.txt"),
        ],
        &[],
    );
    let (_dir, fs) = on_disk(bytes, "escape.zip");
    let error = fs
        .delete(&VfsPath::parse("d").unwrap(), &Cancel::new())
        .unwrap_err();
    assert!(
        matches!(error, VfsError::Unsupported { .. }),
        "unexpected {error}"
    );
}

#[test]
fn a_rewrite_refuses_to_replace_an_invalid_utf8_name_with_a_replacement_character() {
    let original_name = b"bad\xff.txt";
    let mut bytes = zip_manual(
        &[ManualEntry::new("", b"x").with_raw_name(original_name)],
        &[],
    );
    let eocd = bytes
        .windows(4)
        .rposition(|window| window == 0x0605_4b50_u32.to_le_bytes())
        .unwrap();
    let central_offset = usize::try_from(u32::from_le_bytes(
        bytes[eocd + 16..eocd + 20].try_into().unwrap(),
    ))
    .unwrap();
    let local_offset = usize::try_from(u32::from_le_bytes(
        bytes[central_offset + 42..central_offset + 46]
            .try_into()
            .unwrap(),
    ))
    .unwrap();
    for offset in [local_offset + 6, central_offset + 8] {
        let flags = u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap()) | (1 << 11);
        bytes[offset..offset + 2].copy_from_slice(&flags.to_le_bytes());
    }

    let (dir, fs) = on_disk(bytes.clone(), "invalid-utf8-name.zip");
    let error = fs
        .write_file(
            &VfsPath::parse("new.txt").unwrap(),
            &mut b"added".as_slice(),
            &Cancel::new(),
        )
        .unwrap_err();

    assert!(
        matches!(error, VfsError::Unsupported { .. }),
        "unexpected {error}"
    );
    assert_eq!(
        std::fs::read(dir.path().join("invalid-utf8-name.zip")).unwrap(),
        bytes,
        "a refused rewrite leaves the original bytes in place"
    );
}

#[test]
fn a_rewrite_refuses_to_replace_an_invalid_utf8_comment_with_a_replacement_character() {
    let mut bytes = zip_manual(&[ManualEntry::new("f.txt", b"x")], &[]);
    let eocd = bytes
        .windows(4)
        .rposition(|window| window == 0x0605_4b50_u32.to_le_bytes())
        .unwrap();
    let central_offset = usize::try_from(u32::from_le_bytes(
        bytes[eocd + 16..eocd + 20].try_into().unwrap(),
    ))
    .unwrap();
    let name_len = usize::from(u16::from_le_bytes(
        bytes[central_offset + 28..central_offset + 30]
            .try_into()
            .unwrap(),
    ));
    let extra_len = usize::from(u16::from_le_bytes(
        bytes[central_offset + 30..central_offset + 32]
            .try_into()
            .unwrap(),
    ));
    let comment_offset = central_offset + 46 + name_len + extra_len;
    let comment = b"bad\xff";
    bytes.splice(comment_offset..comment_offset, comment.iter().copied());
    bytes[central_offset + 32..central_offset + 34]
        .copy_from_slice(&u16::try_from(comment.len()).unwrap().to_le_bytes());
    let eocd = bytes
        .windows(4)
        .rposition(|window| window == 0x0605_4b50_u32.to_le_bytes())
        .unwrap();
    let directory_size = u32::from_le_bytes(bytes[eocd + 12..eocd + 16].try_into().unwrap());
    let comment_len = u32::try_from(comment.len()).unwrap();
    bytes[eocd + 12..eocd + 16].copy_from_slice(
        &directory_size
            .checked_add(comment_len)
            .unwrap()
            .to_le_bytes(),
    );
    for offset in [6, central_offset + 8] {
        let flags = u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap()) | (1 << 11);
        bytes[offset..offset + 2].copy_from_slice(&flags.to_le_bytes());
    }

    let (dir, fs) = on_disk(bytes.clone(), "invalid-utf8-comment.zip");
    let error = fs
        .write_file(
            &VfsPath::parse("new.txt").unwrap(),
            &mut b"added".as_slice(),
            &Cancel::new(),
        )
        .unwrap_err();

    assert!(
        matches!(error, VfsError::Unsupported { .. }),
        "unexpected {error}"
    );
    assert_eq!(
        std::fs::read(dir.path().join("invalid-utf8-comment.zip")).unwrap(),
        bytes,
        "a refused rewrite leaves the original bytes in place"
    );
}

#[test]
fn a_zip_with_different_local_and_central_names_refuses_a_write() {
    let mut bytes = zip_manual(&[ManualEntry::new("f.txt", b"x")], &[]);
    // The local header keeps x.txt while the central directory names f.txt.
    bytes[30..35].copy_from_slice(b"x.txt");
    let (dir, fs) = on_disk(bytes.clone(), "different-names.zip");

    let error = fs
        .write_file(
            &VfsPath::parse("new.txt").unwrap(),
            &mut b"added".as_slice(),
            &Cancel::new(),
        )
        .unwrap_err();

    assert!(
        matches!(
            error,
            VfsError::Unsupported { .. } | VfsError::Corrupt { .. }
        ),
        "unexpected {error}"
    );
    assert_eq!(
        std::fs::read(dir.path().join("different-names.zip")).unwrap(),
        bytes,
        "a refused rewrite leaves the original bytes in place"
    );
}

#[test]
fn a_rewrite_refuses_when_a_trailing_end_record_names_a_different_method() {
    let mut bytes = zip_manual(&[ManualEntry::new("f.txt", b"payload")], &[]);
    let eocd = bytes
        .windows(4)
        .rposition(|window| window == 0x0605_4b50_u32.to_le_bytes())
        .unwrap();
    let central_offset = usize::try_from(u32::from_le_bytes(
        bytes[eocd + 16..eocd + 20].try_into().unwrap(),
    ))
    .unwrap();
    let central_size = usize::try_from(u32::from_le_bytes(
        bytes[eocd + 12..eocd + 16].try_into().unwrap(),
    ))
    .unwrap();
    let central_end = central_offset + central_size;
    let mut alternate = bytes[central_offset..central_end].to_vec();
    alternate[10..12].copy_from_slice(&8u16.to_le_bytes());

    let alternate_offset = u32::try_from(bytes.len()).unwrap();
    bytes.extend_from_slice(&alternate);
    let mut trailing_end = [0u8; 22];
    trailing_end[..4].copy_from_slice(&0x0605_4b50_u32.to_le_bytes());
    trailing_end[8..10].copy_from_slice(&1u16.to_le_bytes());
    trailing_end[10..12].copy_from_slice(&1u16.to_le_bytes());
    trailing_end[12..16].copy_from_slice(&u32::try_from(central_size).unwrap().to_le_bytes());
    trailing_end[16..20].copy_from_slice(&alternate_offset.to_le_bytes());
    trailing_end[20..22].copy_from_slice(&u16::MAX.to_le_bytes());
    bytes.extend_from_slice(&trailing_end);

    let (dir, fs) = on_disk(bytes.clone(), "different-method.zip");
    let mut original = fs
        .open(&VfsPath::parse("f.txt").unwrap(), &Cancel::new())
        .unwrap();
    let mut original_content = Vec::new();
    original.read_to_end(&mut original_content).unwrap();
    assert_eq!(original_content, b"payload");

    let error = fs
        .write_file(
            &VfsPath::parse("new.txt").unwrap(),
            &mut b"added".as_slice(),
            &Cancel::new(),
        )
        .unwrap_err();
    assert!(
        matches!(error, VfsError::Unsupported { .. }),
        "unexpected {error}"
    );
    assert_eq!(
        std::fs::read(dir.path().join("different-method.zip")).unwrap(),
        bytes,
        "a refused rewrite leaves the original bytes in place"
    );
}

#[test]
fn a_rewrite_refuses_when_a_trailing_end_record_marks_a_plain_entry_encrypted() {
    let mut bytes = zip_manual(&[ManualEntry::new("f.txt", b"payload")], &[]);
    let eocd = bytes
        .windows(4)
        .rposition(|window| window == 0x0605_4b50_u32.to_le_bytes())
        .unwrap();
    let central_offset = usize::try_from(u32::from_le_bytes(
        bytes[eocd + 16..eocd + 20].try_into().unwrap(),
    ))
    .unwrap();
    let central_size = usize::try_from(u32::from_le_bytes(
        bytes[eocd + 12..eocd + 16].try_into().unwrap(),
    ))
    .unwrap();
    let central_end = central_offset + central_size;
    let mut alternate = bytes[central_offset..central_end].to_vec();
    alternate[8..10].copy_from_slice(&1u16.to_le_bytes());

    let alternate_offset = u32::try_from(bytes.len()).unwrap();
    bytes.extend_from_slice(&alternate);
    let mut trailing_end = [0u8; 22];
    trailing_end[..4].copy_from_slice(&0x0605_4b50_u32.to_le_bytes());
    trailing_end[8..10].copy_from_slice(&1u16.to_le_bytes());
    trailing_end[10..12].copy_from_slice(&1u16.to_le_bytes());
    trailing_end[12..16].copy_from_slice(&u32::try_from(central_size).unwrap().to_le_bytes());
    trailing_end[16..20].copy_from_slice(&alternate_offset.to_le_bytes());
    trailing_end[20..22].copy_from_slice(&u16::MAX.to_le_bytes());
    bytes.extend_from_slice(&trailing_end);

    let (dir, fs) = on_disk(bytes.clone(), "different-encryption.zip");
    let mut original = fs
        .open(&VfsPath::parse("f.txt").unwrap(), &Cancel::new())
        .unwrap();
    let mut original_content = Vec::new();
    original.read_to_end(&mut original_content).unwrap();
    assert_eq!(original_content, b"payload");

    let error = fs
        .write_file(
            &VfsPath::parse("new.txt").unwrap(),
            &mut b"added".as_slice(),
            &Cancel::new(),
        )
        .unwrap_err();
    assert!(
        matches!(error, VfsError::Unsupported { .. }),
        "unexpected {error}"
    );
    assert_eq!(
        std::fs::read(dir.path().join("different-encryption.zip")).unwrap(),
        bytes,
        "a refused rewrite leaves the original bytes in place"
    );
}

#[test]
fn a_rewrite_refuses_when_a_trailing_end_record_changes_the_utc_timestamp() {
    let mut bytes = zip_manual(
        &[ManualEntry::new("f.txt", b"payload").with_extra(extended_timestamp(1_600_000_000))],
        &[],
    );
    let eocd = bytes
        .windows(4)
        .rposition(|window| window == 0x0605_4b50_u32.to_le_bytes())
        .unwrap();
    let central_offset = usize::try_from(u32::from_le_bytes(
        bytes[eocd + 16..eocd + 20].try_into().unwrap(),
    ))
    .unwrap();
    let central_size = usize::try_from(u32::from_le_bytes(
        bytes[eocd + 12..eocd + 16].try_into().unwrap(),
    ))
    .unwrap();
    let central_end = central_offset + central_size;
    let mut alternate = bytes[central_offset..central_end].to_vec();
    let name_len = usize::from(u16::from_le_bytes(alternate[28..30].try_into().unwrap()));
    let extra = 46 + name_len;
    assert_eq!(
        u16::from_le_bytes(alternate[extra..extra + 2].try_into().unwrap()),
        0x5455
    );
    alternate[extra + 5..extra + 9].copy_from_slice(&1_700_000_000_i32.to_le_bytes());

    let alternate_offset = u32::try_from(bytes.len()).unwrap();
    bytes.extend_from_slice(&alternate);
    let mut trailing_end = [0u8; 22];
    trailing_end[..4].copy_from_slice(&0x0605_4b50_u32.to_le_bytes());
    trailing_end[8..10].copy_from_slice(&1u16.to_le_bytes());
    trailing_end[10..12].copy_from_slice(&1u16.to_le_bytes());
    trailing_end[12..16].copy_from_slice(&u32::try_from(central_size).unwrap().to_le_bytes());
    trailing_end[16..20].copy_from_slice(&alternate_offset.to_le_bytes());
    trailing_end[20..22].copy_from_slice(&u16::MAX.to_le_bytes());
    bytes.extend_from_slice(&trailing_end);

    let (dir, fs) = on_disk(bytes.clone(), "different-timestamp.zip");
    let path = VfsPath::parse("f.txt").unwrap();
    let before = fs.metadata(&path).unwrap();
    assert_eq!(
        before
            .modified
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
        1_600_000_000
    );
    let mut original = fs.open(&path, &Cancel::new()).unwrap();
    let mut original_content = Vec::new();
    original.read_to_end(&mut original_content).unwrap();
    assert_eq!(original_content, b"payload");

    let error = fs
        .write_file(
            &VfsPath::parse("new.txt").unwrap(),
            &mut b"added".as_slice(),
            &Cancel::new(),
        )
        .unwrap_err();
    assert!(
        matches!(error, VfsError::Unsupported { .. }),
        "unexpected {error}"
    );
    assert_eq!(
        std::fs::read(dir.path().join("different-timestamp.zip")).unwrap(),
        bytes,
        "a refused rewrite leaves the original bytes in place"
    );
}

#[test]
fn a_rewrite_refuses_when_a_trailing_end_record_changes_external_attributes() {
    let mut bytes = zip_manual(&[ManualEntry::new("f.txt", b"payload")], &[]);
    let eocd = bytes
        .windows(4)
        .rposition(|window| window == 0x0605_4b50_u32.to_le_bytes())
        .unwrap();
    let central_offset = usize::try_from(u32::from_le_bytes(
        bytes[eocd + 16..eocd + 20].try_into().unwrap(),
    ))
    .unwrap();
    let central_size = usize::try_from(u32::from_le_bytes(
        bytes[eocd + 12..eocd + 16].try_into().unwrap(),
    ))
    .unwrap();
    let central_end = central_offset + central_size;
    let mut alternate = bytes[central_offset..central_end].to_vec();
    alternate[38..42].copy_from_slice(&0xA1FF_0000_u32.to_le_bytes());

    let alternate_offset = u32::try_from(bytes.len()).unwrap();
    bytes.extend_from_slice(&alternate);
    let mut trailing_end = [0u8; 22];
    trailing_end[..4].copy_from_slice(&0x0605_4b50_u32.to_le_bytes());
    trailing_end[8..10].copy_from_slice(&1u16.to_le_bytes());
    trailing_end[10..12].copy_from_slice(&1u16.to_le_bytes());
    trailing_end[12..16].copy_from_slice(&u32::try_from(central_size).unwrap().to_le_bytes());
    trailing_end[16..20].copy_from_slice(&alternate_offset.to_le_bytes());
    trailing_end[20..22].copy_from_slice(&u16::MAX.to_le_bytes());
    bytes.extend_from_slice(&trailing_end);

    let (dir, fs) = on_disk(bytes.clone(), "different-attributes.zip");
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes.clone())).unwrap();
    assert_eq!(archive.by_name("f.txt").unwrap().unix_mode(), None);

    match fs.write_file(
        &VfsPath::parse("new.txt").unwrap(),
        &mut b"added".as_slice(),
        &Cancel::new(),
    ) {
        Err(VfsError::Unsupported { .. }) => {}
        Ok(()) => {
            let rewritten = std::fs::read(dir.path().join("different-attributes.zip")).unwrap();
            let mut archive = zip::ZipArchive::new(std::io::Cursor::new(rewritten)).unwrap();
            let mode = archive.by_name("f.txt").unwrap().unix_mode();
            assert_eq!(mode, Some(0xA1FF));
            panic!("rewrite changed a regular file into a symbolic link");
        }
        Err(error) => panic!("unexpected {error}"),
    }
    assert_eq!(
        std::fs::read(dir.path().join("different-attributes.zip")).unwrap(),
        bytes,
        "a refused rewrite leaves the original bytes in place"
    );
}

#[test]
fn a_zip_with_data_that_runs_into_its_directory_refuses_a_write() {
    let mut bytes = zip_manual(&[ManualEntry::new("f.txt", b"x")], &[]);
    let central = bytes
        .windows(4)
        .rposition(|window| window == 0x0201_4b50_u32.to_le_bytes())
        .unwrap();
    // The one-byte stored stream now claims two bytes, reaching into the
    // first byte of its central directory record.
    bytes[central + 20..central + 24].copy_from_slice(&2u32.to_le_bytes());
    let (dir, fs) = on_disk(bytes.clone(), "overlapping-data.zip");

    let error = fs
        .write_file(
            &VfsPath::parse("new.txt").unwrap(),
            &mut b"added".as_slice(),
            &Cancel::new(),
        )
        .unwrap_err();

    assert!(
        matches!(
            error,
            VfsError::Unsupported { .. } | VfsError::Corrupt { .. }
        ),
        "unexpected {error}"
    );
    assert_eq!(
        std::fs::read(dir.path().join("overlapping-data.zip")).unwrap(),
        bytes,
        "a refused rewrite leaves the original bytes in place"
    );
}

#[test]
fn a_write_refuses_a_container_that_changed_on_disk() {
    let bytes = zip_manual(&[ManualEntry::new("f.txt", b"x")], &[]);
    let (dir, fs) = on_disk(bytes, "racy.zip");

    // Someone else replaces the file between the listing and the write.
    let replacement = zip_manual(&[ManualEntry::new("other.txt", b"different content")], &[]);
    std::fs::write(dir.path().join("racy.zip"), replacement).unwrap();

    let error = fs
        .write_file(
            &VfsPath::parse("new.txt").unwrap(),
            &mut b"added".as_slice(),
            &Cancel::new(),
        )
        .unwrap_err();
    assert!(
        matches!(error, VfsError::ContainerChanged { .. }),
        "unexpected {error}"
    );
}

#[test]
fn an_ordinary_write_still_works() {
    let bytes = zip_manual(&[ManualEntry::new("f.txt", b"x")], &[]);
    let (_dir, fs) = on_disk(bytes, "plain.zip");
    fs.write_file(
        &VfsPath::parse("new.txt").unwrap(),
        &mut b"added".as_slice(),
        &Cancel::new(),
    )
    .unwrap();
    let mut open = fs
        .open(&VfsPath::parse("new.txt").unwrap(), &Cancel::new())
        .unwrap();
    let mut out = Vec::new();
    open.read_to_end(&mut out).unwrap();
    assert_eq!(out, b"added");
}

// --- typed errors through codecs -----------------------------------------

#[test]
fn a_cancelled_read_of_a_compressed_tar_is_reported_as_cancelled() {
    let fs = open_memory(tar_gz_many(500), "many.tar.gz").unwrap();
    let cancel = Cancel::new();
    cancel.cancel();
    let error = fs
        .open(&VfsPath::parse("d0/f0.txt").unwrap(), &cancel)
        .unwrap_err();
    assert!(
        matches!(error, VfsError::Cancelled),
        "a cancellation raised inside a decoder must not look like damage: {error}"
    );
}

#[test]
fn a_ceiling_reached_inside_a_compressed_tar_is_reported_as_a_limit() {
    let mut options = ArchiveOptions::default();
    options.limits.max_archive_bytes = 512;
    let error = open_memory_with(tar_gz_many(500), "many.tar.gz", options).unwrap_err();
    assert!(
        matches!(error, VfsError::LimitExceeded { .. }),
        "a ceiling raised inside a decoder must not look like damage: {error}"
    );
}

// --- walking a loop ------------------------------------------------------

/// A source whose single directory link points back at the root, which is what
/// a junction loop on disk looks like from here.
struct LoopingFs;

impl FileSystem for LoopingFs {
    fn capabilities(&self) -> Capabilities {
        Capabilities::read_only()
    }

    fn root_label(&self) -> String {
        "loop".to_owned()
    }

    fn list(&self, dir: &VfsPath, _cancel: &Cancel) -> ca_vfs::VfsResult<Vec<VfsEntry>> {
        let child = dir.join("down").unwrap();
        let mut entry = VfsEntry::directory(child);
        entry.link = Some(VfsLinkKind::DirectoryLink);
        Ok(vec![entry])
    }

    fn metadata(&self, path: &VfsPath) -> ca_vfs::VfsResult<VfsEntry> {
        Ok(VfsEntry::directory(path.clone()))
    }

    fn open(&self, path: &VfsPath, _cancel: &Cancel) -> ca_vfs::VfsResult<OpenFile> {
        Err(VfsError::ContentNotStored { path: path.clone() })
    }

    /// Every link resolves to the same place, which is what makes it a loop.
    fn link_identity(&self, _path: &VfsPath) -> Option<String> {
        Some("the one target".to_owned())
    }
}

#[test]
fn a_walk_does_not_follow_a_link_back_into_its_own_branch() {
    // The entry count is the bound: a walk that followed the loop would not
    // return at all.
    let entries = walk(&LoopingFs, &VfsPath::root(), &Cancel::new()).unwrap();
    assert!(
        entries.len() < 8,
        "the walk stopped at the loop rather than descending: {} entries",
        entries.len()
    );
    assert!(entries.iter().any(|entry| entry
        .error
        .as_deref()
        .is_some_and(|text| text.contains("link"))));
}

#[test]
fn a_walk_of_a_real_folder_still_reaches_everything() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
    std::fs::write(dir.path().join("a/b/f.txt"), b"x").unwrap();
    let fs = LocalFs::new(dir.path());
    let entries = walk(&fs, &VfsPath::root(), &Cancel::new()).unwrap();
    assert!(entries.iter().any(|entry| entry.name == "f.txt"));
}

// --- snapshot checksum failures ------------------------------------------

/// A source with one file whose content cannot be read.
struct UnreadableFs;

impl FileSystem for UnreadableFs {
    fn capabilities(&self) -> Capabilities {
        Capabilities::read_only()
    }

    fn root_label(&self) -> String {
        "unreadable".to_owned()
    }

    fn list(&self, dir: &VfsPath, _cancel: &Cancel) -> ca_vfs::VfsResult<Vec<VfsEntry>> {
        if dir.is_root() {
            return Ok(vec![VfsEntry::file(VfsPath::parse("f.txt").unwrap(), 3)]);
        }
        Ok(Vec::new())
    }

    fn metadata(&self, path: &VfsPath) -> ca_vfs::VfsResult<VfsEntry> {
        Ok(VfsEntry::file(path.clone(), 3))
    }

    fn open(&self, path: &VfsPath, _cancel: &Cancel) -> ca_vfs::VfsResult<OpenFile> {
        Err(VfsError::corrupt(format!("cannot read {path}")))
    }
}

#[test]
fn a_checksum_that_could_not_be_taken_is_recorded_as_such() {
    let options = SnapshotOptions {
        include_crc: true,
        ..SnapshotOptions::default()
    };
    let snapshot =
        Snapshot::capture(&UnreadableFs, &VfsPath::root(), options, &Cancel::new()).unwrap();
    let record = snapshot
        .entries
        .iter()
        .find(|record| record.path == "f.txt")
        .unwrap();
    assert_eq!(record.crc32, None);
    assert!(
        record
            .error
            .as_deref()
            .is_some_and(|text| text.contains("checksum")),
        "the failure is on the record, not discarded"
    );
}

#[test]
fn a_recorded_failure_survives_a_round_trip() {
    let options = SnapshotOptions {
        include_crc: true,
        ..SnapshotOptions::default()
    };
    let snapshot =
        Snapshot::capture(&UnreadableFs, &VfsPath::root(), options, &Cancel::new()).unwrap();
    let mut bytes = Vec::new();
    snapshot.write_to(&mut bytes).unwrap();
    let read = Snapshot::read_from(&mut bytes.as_slice()).unwrap();
    assert_eq!(read.entries, snapshot.entries);

    let fs = SnapshotFs::new(read);
    let entry = fs.metadata(&VfsPath::parse("f.txt").unwrap()).unwrap();
    assert!(entry.error.is_some());
}

// --- listing completeness ------------------------------------------------

#[test]
fn a_large_listing_reports_every_entry() {
    let count = 20_000;
    let bodies: Vec<(String, Vec<u8>)> = (0..count)
        .map(|index| {
            (
                format!("dir{:03}/file_{index}.txt", index % 200),
                Vec::new(),
            )
        })
        .collect();
    let borrowed: Vec<(&str, &[u8])> = bodies
        .iter()
        .map(|(name, body)| (name.as_str(), body.as_slice()))
        .collect();
    let fs = open_memory(tar_bytes(&borrowed), "big.tar").unwrap();
    let walked = walk(&fs, &VfsPath::root(), &Cancel::new()).unwrap();
    let files = walked
        .iter()
        .filter(|entry| entry.kind == EntryKind::File)
        .count();
    assert_eq!(files, count);
}

// --- DOS stamps and the zone they are read in ---------------------------

/// `bytes`, a zip of one entry, with the DOS time and date fields of both
/// headers set.
fn with_dos_stamp(mut bytes: Vec<u8>, time: u16, date: u16) -> Vec<u8> {
    bytes[10..12].copy_from_slice(&time.to_le_bytes());
    bytes[12..14].copy_from_slice(&date.to_le_bytes());
    let central = bytes
        .windows(4)
        .position(|window| window == 0x0201_4b50u32.to_le_bytes())
        .unwrap();
    bytes[central + 12..central + 14].copy_from_slice(&time.to_le_bytes());
    bytes[central + 14..central + 16].copy_from_slice(&date.to_le_bytes());
    bytes
}

fn unix_seconds(time: Option<std::time::SystemTime>) -> i64 {
    let time = time.unwrap();
    match time.duration_since(std::time::UNIX_EPOCH) {
        Ok(after) => i64::try_from(after.as_secs()).unwrap(),
        Err(before) => -i64::try_from(before.duration().as_secs()).unwrap(),
    }
}

/// A DOS stamp is the wall clock of the machine that wrote it. A container
/// opened with a zone offset reads the stamp as the instant that wall clock
/// names in that zone.
#[test]
fn a_dos_stamp_is_read_in_the_zone_the_container_is_opened_with() {
    // 2024-06-15 12:00:00 as a DOS time and date.
    let bytes = with_dos_stamp(
        zip_manual(&[ManualEntry::new("f.txt", b"x")], &[]),
        12 << 11,
        (44 << 9) | (6 << 5) | 15,
    );
    let wall_clock_as_utc = 1_718_452_800_i64;
    for zone in [-14_400, 0, 19_800] {
        let options = ArchiveOptions {
            zone_offset_seconds: zone,
            ..ArchiveOptions::default()
        };
        let fs = open_memory_with(bytes.clone(), "dos.zip", options).unwrap();
        let entry = fs.metadata(&VfsPath::parse("f.txt").unwrap()).unwrap();
        assert_eq!(entry.time_fidelity, TimeFidelity::LocalTwoSecond);
        assert_eq!(
            unix_seconds(entry.modified),
            wall_clock_as_utc - i64::from(zone),
            "zone {zone}"
        );
    }
}

/// A single compressed stream stores no time for the file it holds, and its
/// capabilities say that its listing carries none. A zip lists a time.
#[test]
fn a_single_compressed_stream_states_that_it_lists_no_time() {
    let gz = open_memory(gzip(b"payload"), "notes.txt.gz").unwrap();
    assert!(!gz.capabilities().supports_timestamps);
    let entry = gz.metadata(&VfsPath::parse("notes.txt").unwrap()).unwrap();
    assert!(entry.modified.is_none());
    let zip = open_memory(zip_bytes(&[("f.txt", b"x")]), "a.zip").unwrap();
    assert!(zip.capabilities().supports_timestamps);
}

/// A record this build writes carries the time its write names. A write that
/// names no time reads back as the time it was written. A copy names the
/// modification time of its source, and reads back as that instant, although
/// the instant falls on an odd second that a DOS stamp cannot hold: an
/// extended timestamp field carries it, and the DOS stamp holds the wall
/// clock of that instant in the zone the container is opened with. The next
/// rewrite of the container keeps the field, so the record still reads back
/// as that instant.
#[test]
fn a_record_written_here_reads_back_as_the_time_its_write_names() {
    const EMPTY_ZIP: &[u8] = &[
        0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    // 2024-06-15 12:00:01 UTC.
    let source_time = 1_718_452_801_i64;
    let now = || unix_seconds(Some(std::time::SystemTime::now()));
    for (zone, hour, minute) in [(-14_400, 8, 0), (19_800, 17, 30)] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("written.zip");
        std::fs::write(&path, EMPTY_ZIP).unwrap();
        let options = ArchiveOptions {
            zone_offset_seconds: zone,
            ..ArchiveOptions::default()
        };
        let fs = ArchiveFs::open_path(&path, options).unwrap();
        let before = now();
        fs.apply_edits(
            &[
                ca_vfs::ArchiveEdit::WriteFile {
                    path: VfsPath::parse("new.txt").unwrap(),
                    content: b"x".to_vec(),
                    modified: None,
                },
                ca_vfs::ArchiveEdit::WriteFile {
                    path: VfsPath::parse("copied.txt").unwrap(),
                    content: b"y".to_vec(),
                    modified: Some(
                        std::time::UNIX_EPOCH + Duration::from_secs(source_time.unsigned_abs()),
                    ),
                },
            ],
            &Cancel::new(),
        )
        .unwrap();
        let after = now();

        let entry = fs.metadata(&VfsPath::parse("new.txt").unwrap()).unwrap();
        let written = unix_seconds(entry.modified);
        assert!(
            (before - 2..=after + 2).contains(&written),
            "zone {zone}: {written} is not between {before} and {after}"
        );

        let copied = fs.metadata(&VfsPath::parse("copied.txt").unwrap()).unwrap();
        assert_eq!(unix_seconds(copied.modified), source_time, "zone {zone}");
        assert_eq!(copied.time_fidelity, TimeFidelity::Utc, "zone {zone}");
        let mut archive = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
        let stamp = archive
            .by_name("copied.txt")
            .unwrap()
            .last_modified()
            .unwrap();
        assert_eq!(
            (
                stamp.year(),
                stamp.month(),
                stamp.day(),
                stamp.hour(),
                stamp.minute(),
                stamp.second()
            ),
            (2024, 6, 15, hour, minute, 0),
            "zone {zone}"
        );
        drop(archive);

        fs.apply_edits(
            &[ca_vfs::ArchiveEdit::Delete {
                path: VfsPath::parse("new.txt").unwrap(),
            }],
            &Cancel::new(),
        )
        .unwrap();
        let kept = fs.metadata(&VfsPath::parse("copied.txt").unwrap()).unwrap();
        assert_eq!(unix_seconds(kept.modified), source_time, "zone {zone}");
        assert_eq!(kept.time_fidelity, TimeFidelity::Utc, "zone {zone}");
    }
}

// --- time fields a rewrite keeps -----------------------------------------

/// Where the extra fields of each record lie in a zip on disk: the name of
/// each record, its central extra field, and its local extra field.
fn extra_fields_on_disk(path: &std::path::Path) -> Vec<(String, Vec<u8>, Vec<u8>)> {
    let bytes = std::fs::read(path).unwrap();
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes.clone())).unwrap();
    let mut out = Vec::new();
    for index in 0..archive.len() {
        let file = archive.by_index_raw(index).unwrap();
        let at = usize::try_from(file.header_start()).unwrap();
        let name_len = usize::from(u16::from_le_bytes([bytes[at + 26], bytes[at + 27]]));
        let extra_len = usize::from(u16::from_le_bytes([bytes[at + 28], bytes[at + 29]]));
        let start = at + 30 + name_len;
        let local = bytes[start..start + extra_len].to_vec();
        let central = file.extra_data().unwrap_or_default().to_vec();
        out.push((file.name().to_owned(), central, local));
    }
    out
}

/// The identifiers of the extra field records in `extra`.
fn extra_ids(extra: &[u8]) -> Vec<u16> {
    let mut ids = Vec::new();
    let mut at = 0;
    while at + 4 <= extra.len() {
        ids.push(u16::from_le_bytes([extra[at], extra[at + 1]]));
        at += 4 + usize::from(u16::from_le_bytes([extra[at + 2], extra[at + 3]]));
    }
    ids
}

/// A zip made on a machine two hours east of UTC: each record holds the
/// wall clock of that zone as its DOS stamp and the instant in UTC in an
/// extended timestamp field or in an NTFS field, in both headers.
fn zip_with_time_fields(path: &std::path::Path, entries: &[(&str, i64, u16)]) {
    const WRITER_ZONE: i64 = 7_200;
    let civil = |seconds: i64| {
        let (year, month, day, hour, minute, second) =
            ca_vfs::remote::timestamp::unix_to_civil(seconds);
        zip::DateTime::from_date_and_time(
            u16::try_from(year).unwrap(),
            u8::try_from(month).unwrap(),
            u8::try_from(day).unwrap(),
            u8::try_from(hour).unwrap(),
            u8::try_from(minute).unwrap(),
            u8::try_from(second).unwrap(),
        )
        .unwrap_or_default()
    };
    let file = std::fs::File::create(path).unwrap();
    let mut writer = zip::ZipWriter::new(file);
    for (name, instant, field) in entries {
        let method = if name.starts_with("deflated") {
            zip::CompressionMethod::Deflated
        } else {
            zip::CompressionMethod::Stored
        };
        let mut options = zip::write::FullFileOptions::default()
            .compression_method(method)
            .last_modified_time(civil(*instant + WRITER_ZONE));
        if *field == 0x5455 {
            let mut payload = vec![0x01];
            payload.extend_from_slice(&i32::try_from(*instant).unwrap().to_le_bytes());
            options.add_extra_data(0x5455, payload, false).unwrap();
        } else {
            let ticks = u64::try_from(*instant + 11_644_473_600).unwrap() * 10_000_000;
            let mut payload = vec![0u8; 4];
            payload.extend_from_slice(&1u16.to_le_bytes());
            payload.extend_from_slice(&24u16.to_le_bytes());
            for _ in 0..3 {
                payload.extend_from_slice(&ticks.to_le_bytes());
            }
            options.add_extra_data(0x000A, payload, false).unwrap();
        }
        if name.ends_with('/') {
            writer.add_directory(*name, options).unwrap();
        } else {
            writer.start_file(*name, options).unwrap();
            std::io::Write::write_all(&mut writer, format!("content of {name}").as_bytes())
                .unwrap();
        }
    }
    writer.finish().unwrap();
}

/// A zip made elsewhere names each time in UTC in an extended timestamp field
/// or an NTFS field. Every rewrite copies the records it keeps with those
/// fields in both headers, so a kept record reads back as the same instant,
/// at the same precision, after one rewrite and after the next, whatever the
/// zone the container is opened in.
#[test]
fn every_rewrite_keeps_the_utc_time_fields_of_the_records_it_keeps() {
    // Odd seconds, which a DOS stamp cannot hold, and a time before 1980,
    // which a DOS stamp cannot hold either.
    let entries: &[(&str, i64, u16)] = &[
        ("ut.txt", 1_718_452_801, 0x5455),
        ("ntfs.txt", 1_718_452_803, 0x000A),
        ("deflated.txt", 1_718_452_805, 0x5455),
        ("early.txt", -100, 0x5455),
        ("docs/", 1_718_452_807, 0x000A),
        ("docs/inner.txt", 1_718_452_809, 0x5455),
    ];
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("made-elsewhere.zip");
    zip_with_time_fields(&path, entries);
    let fs = ArchiveFs::open_path(
        &path,
        ArchiveOptions {
            zone_offset_seconds: -14_400,
            ..ArchiveOptions::default()
        },
    )
    .unwrap();
    let added_instant = 1_600_000_001_i64;
    let expect = |fs: &ArchiveFs, renamed: &[(&str, &str)], moment: &str| {
        let mut expected: Vec<(String, i64)> = entries
            .iter()
            .map(|(name, instant, _)| {
                let name = name.trim_end_matches('/');
                let name = renamed
                    .iter()
                    .find_map(|(from, to)| {
                        name.strip_prefix(from).map(|rest| format!("{to}{rest}"))
                    })
                    .unwrap_or_else(|| name.to_owned());
                (name, *instant)
            })
            .collect();
        if moment != "before any edit" {
            expected.push(("added.txt".to_owned(), added_instant));
        }
        for (name, instant) in &expected {
            let entry = fs.metadata(&VfsPath::parse(name).unwrap()).unwrap();
            assert_eq!(entry.time_fidelity, TimeFidelity::Utc, "{moment}: {name}");
            assert_eq!(unix_seconds(entry.modified), *instant, "{moment}: {name}");
        }
        for (name, central, local) in extra_fields_on_disk(&path) {
            let has_time = |extra: &[u8]| {
                let ids = extra_ids(extra);
                ids.contains(&0x5455) || ids.contains(&0x000A)
            };
            assert!(has_time(&central), "{moment}: {name}: central {central:?}");
            assert!(has_time(&local), "{moment}: {name}: local {local:?}");
        }
    };
    expect(&fs, &[], "before any edit");

    fs.apply_edits(
        &[ca_vfs::ArchiveEdit::WriteFile {
            path: VfsPath::parse("added.txt").unwrap(),
            content: b"added".to_vec(),
            modified: Some(std::time::UNIX_EPOCH + Duration::from_secs(1_600_000_001)),
        }],
        &Cancel::new(),
    )
    .unwrap();
    expect(&fs, &[], "after the first rewrite");

    fs.apply_edits(
        &[
            ca_vfs::ArchiveEdit::Rename {
                from: VfsPath::parse("ut.txt").unwrap(),
                to: VfsPath::parse("moved.txt").unwrap(),
            },
            ca_vfs::ArchiveEdit::Rename {
                from: VfsPath::parse("docs").unwrap(),
                to: VfsPath::parse("papers").unwrap(),
            },
        ],
        &Cancel::new(),
    )
    .unwrap();
    expect(
        &fs,
        &[("ut.txt", "moved.txt"), ("docs", "papers")],
        "after the second rewrite",
    );
    assert_eq!(fs.rewrite_count(), 2);

    let mut content = String::new();
    fs.open(&VfsPath::parse("deflated.txt").unwrap(), &Cancel::new())
        .unwrap()
        .read_to_string(&mut content)
        .unwrap();
    assert_eq!(content, "content of deflated.txt");
}

/// A rewrite replaces the container in place, so the attributes the file
/// carried survive the edit.
#[cfg(windows)]
#[test]
fn a_rewrite_keeps_the_hidden_flag_of_the_container() {
    use std::io::Write;
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};

    const HIDDEN: u32 = 0x0000_0002;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hidden.zip");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .attributes(HIDDEN)
        .open(&path)
        .unwrap();
    file.write_all(&zip_bytes(&[("a.txt", b"one")])).unwrap();
    drop(file);
    assert_ne!(
        std::fs::metadata(&path).unwrap().file_attributes() & HIDDEN,
        0
    );

    let fs = ArchiveFs::open_path(&path, ArchiveOptions::default()).unwrap();
    fs.apply_edits(
        &[ca_vfs::ArchiveEdit::WriteFile {
            path: VfsPath::parse("b.txt").unwrap(),
            content: b"two".to_vec(),
            modified: None,
        }],
        &Cancel::new(),
    )
    .unwrap();
    drop(fs);

    let reopened = ArchiveFs::open_path(&path, ArchiveOptions::default()).unwrap();
    assert_eq!(
        reopened
            .list(&VfsPath::root(), &Cancel::new())
            .unwrap()
            .len(),
        2
    );
    assert_ne!(
        std::fs::metadata(&path).unwrap().file_attributes() & HIDDEN,
        0,
        "the rewrite dropped the hidden flag of the container"
    );
}
