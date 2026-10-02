//! Containers built to do damage: traversal names, expansion bombs, and bytes
//! that are not what they claim to be.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::io::Read;

use ca_vfs::{ArchiveOptions, Cancel, FileSystem, LimitKind, VfsEntry, VfsError, VfsPath};
use support::{
    gzip, open_memory, open_memory_with, tar_bytes, zip_bytes, zip_manual, zip_with_raw_name,
    ManualEntry,
};

/// Names a container can carry that must never reach a real path.
const HOSTILE_NAMES: [&str; 8] = [
    "../escaped.txt",
    "../../escaped.txt",
    "a/../../escaped.txt",
    "/absolute.txt",
    "C:/Windows/system32/escaped.txt",
    "\\\\server\\share\\escaped.txt",
    "..\\escaped.txt",
    "dir/../../escaped.txt",
];

/// Every entry a walk of the whole file system reaches.
fn walk_all(fs: &dyn FileSystem) -> Vec<VfsEntry> {
    ca_vfs::walk(fs, &VfsPath::root(), &Cancel::new()).unwrap()
}

/// The row listed at `listed` stands for the record stored as `stored`: it
/// says so in its error, and nothing can open it.
fn assert_refused(fs: &dyn FileSystem, listed: &str, stored: &str) {
    let path = VfsPath::parse(listed).unwrap();
    let entry = fs
        .metadata(&path)
        .unwrap_or_else(|error| panic!("{listed:?} is not listed: {error}"));
    let error = entry
        .error
        .as_deref()
        .unwrap_or_else(|| panic!("{listed:?} carries no error"));
    assert!(
        error.contains(&format!("{stored:?}")),
        "{listed:?} does not name {stored:?}: {error}"
    );
    assert!(entry.refused, "{listed:?} is not marked refused");
    match fs.open(&path, &Cancel::new()) {
        Err(VfsError::Refused { reason, .. }) => assert_eq!(reason, error),
        Err(VfsError::IsADirectory { .. }) if entry.is_dir() => {}
        Err(other) => panic!("{listed:?} fails to open for another reason: {other}"),
        Ok(_) => panic!("{listed:?} opened"),
    }
}

#[test]
fn zip_slip_names_are_listed_under_a_safe_name_and_never_opened() {
    let root = std::path::Path::new("extraction-root");
    for name in HOSTILE_NAMES {
        let fs = open_memory(zip_with_raw_name(name, b"payload"), "evil.zip").unwrap();
        let files: Vec<VfsEntry> = walk_all(&fs)
            .into_iter()
            .filter(|entry| !entry.is_dir())
            .collect();
        assert_eq!(files.len(), 1, "{name:?} is one row: {files:?}");
        let row = &files[0];
        assert!(
            row.path.components().all(|part| part != ".."),
            "{name:?} is listed as {:?}",
            row.path
        );
        assert!(row.path.to_native(root).starts_with(root));
        assert_refused(&fs, row.path.as_str(), name);
    }
}

#[test]
fn zip_slip_names_cannot_be_opened() {
    for name in HOSTILE_NAMES {
        let fs = open_memory(zip_with_raw_name(name, b"payload"), "evil.zip").unwrap();
        assert!(VfsPath::parse(name).is_err(), "{name:?} parsed as a path");
        let error = fs
            .open(&VfsPath::parse("escaped.txt").unwrap(), &Cancel::new())
            .unwrap_err();
        assert!(matches!(error, VfsError::NotFound { .. }));
    }
}

#[test]
fn tar_traversal_names_are_listed_with_an_error_and_never_opened() {
    let bytes =
        support::tar_with_raw_names(&[("../escaped.txt", b"payload"), ("safe.txt", b"kept")]);
    let fs = open_memory(bytes, "evil.tar").unwrap();
    let mut names: Vec<String> = fs
        .list(&VfsPath::root(), &Cancel::new())
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    names.sort_unstable();
    assert_eq!(names, ["__", "safe.txt"]);
    assert_refused(&fs, "__/escaped.txt", "../escaped.txt");
    let mut kept = Vec::new();
    fs.open(&VfsPath::parse("safe.txt").unwrap(), &Cancel::new())
        .unwrap()
        .read_to_end(&mut kept)
        .unwrap();
    assert_eq!(kept, b"kept");
}

/// Names a Unix system stores freely that no Windows path can hold, with the
/// path each one is listed under.
const UNIX_ONLY_NAMES: [(&str, &str); 4] = [
    ("log-12:30.txt", "log-12_30.txt"),
    ("trailing-dot.", "trailing-dot_"),
    ("trailing-space ", "trailing-space_"),
    ("sub/12:00/x.txt", "sub/12_00/x.txt"),
];

#[test]
fn names_the_path_rules_refuse_are_listed_with_an_error_in_a_zip_and_a_tar() {
    let mut stored: Vec<(&str, &[u8])> = vec![("ok.txt", b"fine")];
    stored.extend(
        UNIX_ONLY_NAMES
            .iter()
            .map(|(name, _)| (*name, b"x" as &[u8])),
    );
    let manual: Vec<ManualEntry> = stored
        .iter()
        .map(|(name, content)| ManualEntry::new(name, content))
        .collect();
    let zip = zip_manual(&manual, &[]);
    let tar = support::tar_with_raw_names(&stored);
    for (bytes, label) in [(zip, "names.zip"), (tar, "names.tar")] {
        let fs = open_memory(bytes, label).unwrap();
        let files: Vec<String> = walk_all(&fs)
            .into_iter()
            .filter(|entry| !entry.is_dir())
            .map(|entry| entry.path.as_str().to_owned())
            .collect();
        assert_eq!(files.len(), 5, "{label}: every record is a row: {files:?}");
        for (name, listed) in UNIX_ONLY_NAMES {
            assert_refused(&fs, listed, name);
        }
        let mut content = Vec::new();
        fs.open(&VfsPath::parse("ok.txt").unwrap(), &Cancel::new())
            .unwrap()
            .read_to_end(&mut content)
            .unwrap();
        assert_eq!(content, b"fine");
    }
}

#[test]
fn an_oversized_central_directory_past_the_file_end_is_refused_before_parsing() {
    let mut bytes = zip_manual(&[ManualEntry::new("a.txt", b"hello")], &[]);
    assert_eq!(bytes.len(), 113);
    let end = bytes.len() - 22;
    bytes[end + 12..end + 16].copy_from_slice(&0xFFFF_FFF0u32.to_le_bytes());
    let Err(error) = open_memory(bytes, "size.zip") else {
        panic!("an oversized directory must be refused");
    };
    assert!(
        matches!(
            error,
            VfsError::LimitExceeded {
                kind: LimitKind::ArchiveSize,
                ..
            } | VfsError::Corrupt { .. }
        ),
        "unexpected {error}"
    );
}

#[test]
fn an_entry_whose_local_header_cannot_be_read_is_listed_and_refused() {
    let mut bytes = zip_manual(
        &[
            ManualEntry::new("a.txt", b"hello"),
            ManualEntry::new("b.txt", b"world"),
        ],
        &[],
    );
    // The second local header follows the first entry's 30 byte header, its
    // five byte name and its five bytes of content.
    bytes[40..44].copy_from_slice(b"XXXX");
    let fs = open_memory(bytes, "header.zip").unwrap();
    let entry = fs.metadata(&VfsPath::parse("b.txt").unwrap()).unwrap();
    assert!(entry.refused, "{entry:?}");
    assert!(entry
        .error
        .as_deref()
        .is_some_and(|text| text.contains("header cannot be read")));
    assert!(matches!(
        fs.open(&entry.path, &Cancel::new()),
        Err(VfsError::Refused { .. })
    ));
    let mut content = Vec::new();
    fs.open(&VfsPath::parse("a.txt").unwrap(), &Cancel::new())
        .unwrap()
        .read_to_end(&mut content)
        .unwrap();
    assert_eq!(content, b"hello");
}

#[test]
fn an_entry_past_the_absolute_ceiling_trips() {
    let big = vec![0u8; 4 * 1024 * 1024];
    let mut options = ArchiveOptions::default();
    options.limits.max_entry_bytes = 64 * 1024;
    options.limits.ratio_floor_bytes = 0;
    let fs = open_memory_with(
        zip_bytes(&[("bomb.bin", big.as_slice())]),
        "bomb.zip",
        options,
    )
    .unwrap();
    let error = fs
        .open(&VfsPath::parse("bomb.bin").unwrap(), &Cancel::new())
        .unwrap_err();
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
fn an_entry_past_the_expansion_ratio_trips() {
    let big = vec![0u8; 8 * 1024 * 1024];
    let mut options = ArchiveOptions::default();
    options.limits.max_expansion_ratio = 4;
    options.limits.ratio_floor_bytes = 1024;
    let fs = open_memory_with(
        zip_bytes(&[("bomb.bin", big.as_slice())]),
        "bomb.zip",
        options,
    )
    .unwrap();
    let error = fs
        .open(&VfsPath::parse("bomb.bin").unwrap(), &Cancel::new())
        .unwrap_err();
    assert!(
        matches!(error, VfsError::LimitExceeded { .. }),
        "unexpected {error}"
    );
}

/// Eight half-megabyte entries under the name each takes in the container.
fn half_megabyte_entries() -> Vec<(String, Vec<u8>)> {
    let body = vec![b'a'; 512 * 1024];
    (0..8)
        .map(|index| (format!("f{index}.bin"), body.clone()))
        .collect()
}

fn borrow(entries: &[(String, Vec<u8>)]) -> Vec<(&str, &[u8])> {
    entries
        .iter()
        .map(|(name, body)| (name.as_str(), body.as_slice()))
        .collect()
}

#[test]
fn one_read_past_the_container_budget_trips() {
    let entries = half_megabyte_entries();
    let mut options = ArchiveOptions::default();
    options.limits.max_archive_bytes = 256 * 1024;
    let fs = open_memory_with(zip_bytes(&borrow(&entries)), "bomb.zip", options).unwrap();

    let path = VfsPath::parse("f0.bin").unwrap();
    let error = fs
        .open(&path, &Cancel::new())
        .expect_err("the budget should run out");
    assert!(
        matches!(
            error,
            VfsError::LimitExceeded {
                kind: LimitKind::ArchiveSize,
                ..
            }
        ),
        "unexpected {error}"
    );
}

#[test]
fn repeated_reads_do_not_use_up_the_container_budget() {
    let entries = half_megabyte_entries();
    let mut options = ArchiveOptions::default();
    options.limits.max_archive_bytes = 1024 * 1024;
    let fs = open_memory_with(zip_bytes(&borrow(&entries)), "many.zip", options).unwrap();

    // Each entry is well inside the ceiling; the ceiling bounds one read, not
    // the lifetime of the file system, so every entry stays readable.
    for index in 0..8 {
        let path = VfsPath::parse(&format!("f{index}.bin")).unwrap();
        let mut open = fs
            .open(&path, &Cancel::new())
            .unwrap_or_else(|error| panic!("entry {index} should still read: {error}"));
        let mut out = Vec::new();
        open.read_to_end(&mut out).unwrap();
        assert_eq!(out.len(), 512 * 1024);
    }
}

/// A gzip tar whose first member is a GNU long name, or a pax header, of
/// `size` bytes, followed by one ordinary entry.
fn tar_gz_with_extension(kind: tar::EntryType, size: u64) -> Vec<u8> {
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut builder = tar::Builder::new(encoder);
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(kind);
    header.set_path("././@LongLink").unwrap();
    header.set_size(size);
    header.set_mode(0o644);
    header.set_cksum();
    builder
        .append(&header, std::io::repeat(b'a').take(size))
        .unwrap();
    let mut header = tar::Header::new_gnu();
    header.set_size(1);
    header.set_mode(0o644);
    header.set_cksum();
    builder
        .append_data(&mut header, "after.txt", &b"x"[..])
        .unwrap();
    builder.into_inner().unwrap().finish().unwrap()
}

#[test]
fn an_oversized_tar_long_name_or_pax_header_is_refused_before_it_is_held() {
    for kind in [tar::EntryType::GNULongName, tar::EntryType::XHeader] {
        let bytes = tar_gz_with_extension(kind, 48 * 1024 * 1024);
        assert!(bytes.len() < 1024 * 1024, "{}", bytes.len());
        let error = open_memory(bytes, "names.tar.gz").unwrap_err();
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
}

#[test]
fn a_tar_long_name_within_the_ceiling_still_names_its_entry() {
    let long = format!("{}/leaf.txt", "d".repeat(300));
    let fs = open_memory(tar_bytes(&[(&long, b"body")]), "long.tar").unwrap();
    let listed = walk_all(&fs);
    assert!(listed.iter().any(|entry| entry.path.as_str() == long));
}

#[test]
fn a_compressed_stream_bomb_trips_while_streaming() {
    let bomb = gzip(&vec![0u8; 16 * 1024 * 1024]);
    let mut options = ArchiveOptions::default();
    options.limits.max_entry_bytes = 128 * 1024;
    options.limits.ratio_floor_bytes = 0;
    let fs = open_memory_with(bomb, "bomb.bin.gz", options).unwrap();
    let error = fs
        .open(&VfsPath::parse("bomb.bin").unwrap(), &Cancel::new())
        .unwrap_err();
    assert!(
        matches!(error, VfsError::LimitExceeded { .. }),
        "unexpected {error}"
    );
}

#[test]
fn truncated_containers_report_errors_rather_than_panicking() {
    let full = zip_bytes(&[("a.txt", b"hello"), ("b/c.txt", b"world")]);
    for cut in [1usize, 8, 32, full.len() / 2, full.len() - 1] {
        let truncated = full.get(..cut).unwrap_or_default().to_vec();
        drive(truncated, "cut.zip");
    }

    let tarred = tar_bytes(&[("a.txt", b"hello")]);
    for cut in [1usize, 100, 512, tarred.len() / 2] {
        drive(tarred.get(..cut).unwrap_or_default().to_vec(), "cut.tar");
    }
}

#[test]
fn corrupt_containers_report_errors_rather_than_panicking() {
    let mut bytes = zip_bytes(&[("a.txt", b"hello"), ("b/c.txt", b"world")]);
    for index in (0..bytes.len()).step_by(7) {
        if let Some(byte) = bytes.get_mut(index) {
            *byte ^= 0xFF;
        }
    }
    drive(bytes, "corrupt.zip");
}

/// Open a container and touch everything it offers, asserting only that the
/// process survives.
fn drive(bytes: Vec<u8>, name: &str) {
    let Ok(fs) = open_memory(bytes, name) else {
        return;
    };
    let cancel = Cancel::new();
    let Ok(entries) = fs.list(&VfsPath::root(), &cancel) else {
        return;
    };
    for entry in entries {
        let _ = fs.metadata(&entry.path);
        if let Ok(mut open) = fs.open(&entry.path, &cancel) {
            let mut sink = Vec::new();
            let _ = open.read_to_end(&mut sink);
        }
        let _ = fs.list(&entry.path, &cancel);
    }
}

mod fuzz {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::{drive, support};
    use proptest::prelude::*;
    use support::{gzip, tar_bytes, zip_bytes};

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]

        /// Mutating a well formed container must never panic, whatever the
        /// mutation does to its structure.
        #[test]
        fn mutated_containers_never_panic(
            mutations in proptest::collection::vec((any::<u16>(), any::<u8>()), 1..24),
            which in 0usize..3,
        ) {
            let base = match which {
                0 => zip_bytes(&[("a.txt", b"hello"), ("dir/b.txt", b"world")]),
                1 => tar_bytes(&[("a.txt", b"hello"), ("dir/b.txt", b"world")]),
                _ => gzip(b"hello from a stream"),
            };
            let name = match which {
                0 => "fuzz.zip",
                1 => "fuzz.tar",
                _ => "fuzz.bin.gz",
            };
            let mut bytes = base;
            if bytes.is_empty() {
                return Ok(());
            }
            for (offset, value) in mutations {
                let index = usize::from(offset) % bytes.len();
                if let Some(byte) = bytes.get_mut(index) {
                    *byte = value;
                }
            }
            drive(bytes, name);
        }
    }
}

// -- cabinets, disc images and packages --------------------------------------

mod new_formats {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::{assert_refused, drive, support};
    use ca_vfs::{Cancel, VfsPath};
    use proptest::prelude::*;
    use support::formats::{
        ar_bytes, cab_bytes, deb_bytes, iso_bytes, rpm_bytes, rpm_with_payload, IsoNames,
    };

    /// One well formed container of each format and the name it goes by.
    fn fixtures() -> Vec<(Vec<u8>, &'static str)> {
        let files: [(&str, &[u8]); 2] = [("a.txt", b"hello"), ("dir/b.txt", b"world")];
        vec![
            (
                cab_bytes(
                    &[("a.txt", b"hello"), ("dir\\b.txt", b"world")],
                    cab::CompressionType::None,
                ),
                "fuzz.cab",
            ),
            (
                cab_bytes(
                    &[("a.txt", b"hello"), ("dir\\b.txt", b"world")],
                    cab::CompressionType::MsZip,
                ),
                "fuzz.cab",
            ),
            (iso_bytes(&files, IsoNames::Plain), "fuzz.iso"),
            (iso_bytes(&files, IsoNames::Joliet), "fuzz.iso"),
            (iso_bytes(&files, IsoNames::RockRidge), "fuzz.iso"),
            (deb_bytes(&files), "fuzz.deb"),
            (rpm_bytes(&files), "fuzz.rpm"),
            (lzx_cab(), "fuzz.cab"),
        ]
    }

    /// A stored cabinet relabelled as LZX, so the LZX decoder meets data it
    /// did not write.
    fn lzx_cab() -> Vec<u8> {
        let mut bytes = cab_bytes(
            &[("a.txt", b"hello"), ("b.txt", b"world")],
            cab::CompressionType::None,
        );
        bytes[42..44].copy_from_slice(&0x1503u16.to_le_bytes());
        bytes
    }

    #[test]
    fn truncated_containers_report_errors_rather_than_panicking() {
        for (bytes, name) in fixtures() {
            let len = bytes.len();
            for cut in [0usize, 1, 4, 8, 60, 96, 120, len / 3, len / 2, len - 1] {
                drive(bytes.get(..cut.min(len)).unwrap_or_default().to_vec(), name);
            }
        }
    }

    #[test]
    fn a_magic_followed_by_noise_is_an_error_not_a_panic() {
        let mut state = 0x9E37_79B9_u32;
        let mut noise = |len: usize| -> Vec<u8> {
            (0..len)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 17;
                    state ^= state << 5;
                    state.to_le_bytes()[0]
                })
                .collect()
        };
        for (magic, name) in [
            (b"MSCF".as_slice(), "noise.cab"),
            (b"!<arch>\n".as_slice(), "noise.deb"),
            (&[0xED, 0xAB, 0xEE, 0xDB][..], "noise.rpm"),
        ] {
            for len in [16usize, 200, 4096] {
                let mut bytes = magic.to_vec();
                bytes.extend(noise(len));
                drive(bytes, name);
            }
        }
        let mut iso = vec![0u8; 40 * 1024];
        iso[32768] = 1;
        iso[32769..32774].copy_from_slice(b"CD001");
        let tail = noise(2048 - 6);
        iso[32774..32774 + tail.len()].copy_from_slice(&tail);
        drive(iso, "noise.iso");
    }

    #[test]
    fn traversal_names_in_a_package_are_listed_with_an_error_and_never_opened() {
        let tar = support::tar_with_raw_names(&[("../escaped.txt", b"x"), ("safe.txt", b"y")]);
        let deb = ar_bytes(&[("debian-binary", b"2.0\n"), ("data.tar", &tar)]);
        let fs = support::open_memory(deb, "evil.deb").unwrap();
        let listed = ca_vfs::walk(&fs, &VfsPath::root(), &Cancel::new()).unwrap();
        let mut paths: Vec<&str> = listed.iter().map(|entry| entry.path.as_str()).collect();
        paths.sort_unstable();
        assert_eq!(
            paths,
            vec![
                "data.tar",
                "data.tar/__",
                "data.tar/__/escaped.txt",
                "data.tar/safe.txt",
                "debian-binary"
            ]
        );
        assert_refused(&fs, "data.tar/__/escaped.txt", "../escaped.txt");

        let payload = support::gzip(&support::formats::cpio_bytes(&[("../../escaped", b"x")]));
        let fs = support::open_memory(rpm_with_payload("gzip", &payload), "evil.rpm").unwrap();
        assert_refused(&fs, "__/__/escaped", "../../escaped");

        let cab = cab_bytes(
            &[("..\\escaped.txt", b"x"), ("C:\\evil.txt", b"y")],
            cab::CompressionType::None,
        );
        let fs = support::open_memory(cab, "evil.cab").unwrap();
        assert_refused(&fs, "__/escaped.txt", "..\\escaped.txt");
        assert_refused(&fs, "C_/evil.txt", "C:\\evil.txt");
    }

    #[test]
    fn a_name_with_a_colon_is_listed_with_an_error_in_every_format() {
        let fs = support::open_memory(
            support::sevenz_bytes(&[("log-12:30.txt", b"x"), ("ok.txt", b"y")]),
            "names.7z",
        )
        .unwrap();
        assert_refused(&fs, "log-12_30.txt", "log-12:30.txt");

        let cab = cab_bytes(
            &[("log-12:30.txt", b"x"), ("ok.txt", b"y")],
            cab::CompressionType::None,
        );
        let fs = support::open_memory(cab, "names.cab").unwrap();
        assert_refused(&fs, "log-12_30.txt", "log-12:30.txt");

        let fs = support::open_memory(
            support::formats::rpm_bytes(&[("log-12:30.txt", b"x"), ("ok.txt", b"y")]),
            "names.rpm",
        )
        .unwrap();
        assert_refused(&fs, "log-12_30.txt", "log-12:30.txt");

        let deb = ar_bytes(&[("debian-binary", b"2.0\n"), ("notes:1", b"x")]);
        let fs = support::open_memory(deb, "names.deb").unwrap();
        assert_refused(&fs, "notes_1", "notes:1");

        let image = iso_bytes(
            &[("a:b.txt", b"x"), ("d:1/x.txt", b"y"), ("ok.txt", b"z")],
            IsoNames::RockRidge,
        );
        let fs = support::open_memory(image, "names.iso").unwrap();
        assert_refused(&fs, "a_b.txt", "a:b.txt");
        assert_refused(&fs, "d_1", "d:1");
        assert_refused(&fs, "d_1/x.txt", "d:1/x.txt");
    }

    #[test]
    fn a_directory_that_names_its_own_ancestor_ends() {
        let mut image = iso_bytes(&[("a/b.txt", b"x")], IsoNames::Plain);
        let root = 19 * 2048;
        let sector = image[root..root + 2048].to_vec();
        let at = sector
            .windows(2)
            .position(|pair| pair == [1, b'A'])
            .map(|found| root + found - 32)
            .unwrap();
        let le = 19u32.to_le_bytes();
        let be = 19u32.to_be_bytes();
        image[at + 2..at + 6].copy_from_slice(&le);
        image[at + 6..at + 10].copy_from_slice(&be);
        let fs = support::open_memory(image, "loop.iso").unwrap();
        let listed = ca_vfs::walk(&fs, &VfsPath::root(), &Cancel::new()).unwrap();
        assert!(listed.len() < 8, "the walk looped: {}", listed.len());
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// Mutating a well formed container must never panic, whatever the
        /// mutation does to its structure.
        #[test]
        fn mutated_containers_never_panic(
            mutations in proptest::collection::vec((any::<u32>(), any::<u8>()), 1..24),
            which in 0usize..8,
        ) {
            let (mut bytes, name) = fixtures().swap_remove(which);
            for (offset, value) in mutations {
                let index = offset as usize % bytes.len();
                if let Some(byte) = bytes.get_mut(index) {
                    *byte = value;
                }
            }
            drive(bytes, name);
        }

        /// Bytes that only claim a format must never panic either.
        #[test]
        fn random_bytes_never_panic(
            body in proptest::collection::vec(any::<u8>(), 0..3000),
            which in 0usize..3,
        ) {
            let (magic, name): (&[u8], &str) = match which {
                0 => (b"MSCF", "r.cab"),
                1 => (b"!<arch>\n", "r.deb"),
                _ => (&[0xED, 0xAB, 0xEE, 0xDB], "r.rpm"),
            };
            let mut bytes = magic.to_vec();
            bytes.extend(body);
            drive(bytes, name);
        }
    }
}

/// A 7z container whose encoded header declares `unpacked` bytes behind a
/// 16-byte packed stream. The packed bytes are not a valid LZMA stream: the
/// refusal has to come before any decoding.
fn sevenz_with_encoded_header(unpacked: u64) -> Vec<u8> {
    let packed = [0x5Au8; 16];
    let mut header = vec![0x17, 0x06, 0x00, 0x01, 0x09, 0x10, 0x00];
    header.extend_from_slice(&[0x07, 0x0B, 0x01, 0x00, 0x01, 0x23, 0x03, 0x01, 0x01]);
    header.extend_from_slice(&[0x05, 0x5D, 0x00, 0x00, 0x01, 0x00]);
    header.push(0x0C);
    header.push(0xFF);
    header.extend_from_slice(&unpacked.to_le_bytes());
    header.extend_from_slice(&[0x00, 0x00]);

    let mut record = Vec::new();
    record.extend_from_slice(&(packed.len() as u64).to_le_bytes());
    record.extend_from_slice(&(header.len() as u64).to_le_bytes());
    record.extend_from_slice(&crc32fast::hash(&header).to_le_bytes());

    let mut bytes = vec![b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C, 0x00, 0x04];
    bytes.extend_from_slice(&crc32fast::hash(&record).to_le_bytes());
    bytes.extend_from_slice(&record);
    bytes.extend_from_slice(&packed);
    bytes.extend_from_slice(&header);
    bytes
}

#[test]
fn a_7z_encoded_header_that_declares_an_oversized_expansion_is_refused_before_decoding() {
    for unpacked in [1u64 << 40, 32 * 1024 * 1024 + 1] {
        let bytes = sevenz_with_encoded_header(unpacked);
        let error = open_memory(bytes, "header.7z").unwrap_err();
        assert!(
            matches!(
                error,
                VfsError::LimitExceeded {
                    kind: LimitKind::ArchiveSize,
                    ..
                }
            ),
            "unexpected {error}"
        );
    }
}

#[test]
fn a_7z_encoded_header_within_the_ceiling_still_lists() {
    let bytes = support::sevenz_many(64);
    let offset = u64::from_le_bytes(bytes[12..20].try_into().unwrap());
    let at = usize::try_from(32 + offset).unwrap();
    assert_eq!(bytes[at], 0x17, "the writer did not encode the header");
    let fs = open_memory(bytes, "many.7z").unwrap();
    assert_eq!(walk_all(&fs).len(), 64);
}

#[test]
fn a_7z_with_an_empty_start_header_is_refused_without_a_search() {
    let mut bytes = support::sevenz_many(4);
    bytes[8..32].fill(0);
    let error = open_memory(bytes, "unfinished.7z").unwrap_err();
    assert!(matches!(error, VfsError::Corrupt { .. }), "{error}");
}

#[test]
fn a_raised_flag_stops_7z_and_cabinet_listing() {
    let cancel = Cancel::new();
    cancel.cancel();
    let cases = [
        (support::sevenz_many(4), "many.7z"),
        (
            support::formats::cab_bytes(&[("a.txt", b"a")], cab::CompressionType::None),
            "a.cab",
        ),
    ];
    for (bytes, name) in cases {
        let error = ca_vfs::ArchiveFs::open_cancellable(
            ca_vfs::ArchiveBacking::from_bytes(bytes),
            name,
            ArchiveOptions::default(),
            &cancel,
        )
        .unwrap_err();
        assert!(matches!(error, VfsError::Cancelled), "{name}: {error}");
    }
}
