//! Cabinets, disc images, Debian and Red Hat packages, and RAR read as
//! folders: what each lists, what each entry holds, and that each refuses a
//! write.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::io::Read;

use ca_vfs::{
    ArchiveFormat, ArchiveOptions, ArchiveTypes, Cancel, EntryKind, FileSystem, VfsError, VfsPath,
};
use support::formats::{
    cab_bytes, cpio_bytes, deb_bytes, iso_bytes, rar_bytes, rpm_bytes, rpm_with_payload, IsoNames,
};
use support::{open_memory, open_memory_with, xz};

fn path(text: &str) -> VfsPath {
    VfsPath::parse(text).unwrap()
}

fn read(fs: &dyn FileSystem, at: &str) -> Vec<u8> {
    let mut open = fs.open(&path(at), &Cancel::new()).unwrap();
    let mut out = Vec::new();
    open.read_to_end(&mut out).unwrap();
    out
}

fn names(fs: &dyn FileSystem, dir: &str) -> Vec<String> {
    let dir = if dir.is_empty() {
        VfsPath::root()
    } else {
        path(dir)
    };
    let mut out: Vec<String> = fs
        .list(&dir, &Cancel::new())
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    out.sort();
    out
}

fn assert_read_only(fs: &dyn FileSystem) {
    assert!(!fs.capabilities().writable);
    let error = fs.create_dir(&path("new"), &Cancel::new()).unwrap_err();
    assert!(
        matches!(error, VfsError::Unsupported { .. }),
        "a write was not refused: {error:?}"
    );
}

// -- cabinet -----------------------------------------------------------------

#[test]
fn a_cabinet_lists_and_reads_its_entries() {
    for compression in [cab::CompressionType::None, cab::CompressionType::MsZip] {
        let bytes = cab_bytes(
            &[
                ("readme.txt", b"hello cabinet"),
                ("sub\\inner.txt", b"inner"),
            ],
            compression,
        );
        let fs = open_memory(bytes, "setup.cab").unwrap();
        assert_eq!(fs.format(), ArchiveFormat::Cab);
        assert_eq!(names(&fs, ""), vec!["readme.txt", "sub"]);
        assert_eq!(read(&fs, "readme.txt"), b"hello cabinet");
        assert_eq!(read(&fs, "sub/inner.txt"), b"inner");
        assert_read_only(&fs);
    }
}

// -- disc image --------------------------------------------------------------

#[test]
fn a_cabinet_block_that_fails_its_checksum_is_reported_damaged() {
    let mut bytes = cab_bytes(
        &[("readme.txt", b"hello cabinet")],
        cab::CompressionType::None,
    );
    let at = bytes
        .windows(13)
        .position(|window| window == b"hello cabinet")
        .unwrap();
    bytes[at] = b'j';
    let fs = open_memory(bytes, "setup.cab").unwrap();
    assert_eq!(names(&fs, ""), vec!["readme.txt"]);
    let error = fs.open(&path("readme.txt"), &Cancel::new()).unwrap_err();
    assert!(
        matches!(&error, VfsError::Corrupt { .. }) && error.to_string().contains("checksum"),
        "{error:?}"
    );
}

#[test]
fn a_cabinet_block_with_no_checksum_reads() {
    let mut bytes = cab_bytes(
        &[("readme.txt", b"hello cabinet")],
        cab::CompressionType::None,
    );
    let first_block =
        usize::try_from(u32::from_le_bytes(bytes[36..40].try_into().unwrap())).unwrap();
    bytes[first_block..first_block + 4].copy_from_slice(&[0; 4]);
    let fs = open_memory(bytes, "setup.cab").unwrap();
    assert_eq!(read(&fs, "readme.txt"), b"hello cabinet");
}

#[test]
fn a_disc_image_reads_under_each_naming() {
    let files: [(&str, &[u8]); 3] = [
        ("top.txt", b"top level"),
        ("docs/guide.txt", b"the guide"),
        ("docs/deep/note.md", b"deep note"),
    ];
    for (names_kind, expected_top) in [
        (IsoNames::Plain, vec!["DOCS", "TOP.TXT"]),
        (IsoNames::Joliet, vec!["docs", "top.txt"]),
        (IsoNames::RockRidge, vec!["docs", "top.txt"]),
    ] {
        let fs = open_memory(iso_bytes(&files, names_kind), "disc.iso").unwrap();
        assert_eq!(fs.format(), ArchiveFormat::DiskImage);
        assert_eq!(names(&fs, ""), expected_top, "{names_kind:?}");
        let (guide, note) = if names_kind == IsoNames::Plain {
            ("DOCS/GUIDE.TXT", "DOCS/DEEP/NOTE.MD")
        } else {
            ("docs/guide.txt", "docs/deep/note.md")
        };
        assert_eq!(read(&fs, guide), b"the guide");
        assert_eq!(read(&fs, note), b"deep note");
        assert_read_only(&fs);
    }
}

#[test]
fn rock_ridge_modes_reach_the_attributes() {
    let fs = open_memory(
        iso_bytes(&[("run.sh", b"#!/bin/sh\n")], IsoNames::RockRidge),
        "disc.iso",
    )
    .unwrap();
    let entry = fs.metadata(&path("run.sh")).unwrap();
    assert_eq!(entry.attributes.unwrap().unix_mode, Some(0o644));
}

fn both32(value: u32) -> [u8; 8] {
    let le = value.to_le_bytes();
    let be = value.to_be_bytes();
    [le[0], le[1], le[2], le[3], be[0], be[1], be[2], be[3]]
}

fn rock_name(name: &str) -> Vec<u8> {
    let mut out = vec![b'N', b'M', u8::try_from(5 + name.len()).unwrap(), 1, 0];
    out.extend_from_slice(name.as_bytes());
    out
}

/// A Rock Ridge image whose root holds a child link to a directory of two
/// sectors, with one file in each sector.
fn iso_with_a_relocated_two_sector_directory() -> Vec<u8> {
    const SECTOR: usize = 2048;
    let total = 24usize;
    let mut image = vec![0u8; total * SECTOR];
    let mut put = |sector: usize, offset: usize, bytes: &[u8]| {
        let at = sector * SECTOR + offset;
        image[at..at + bytes.len()].copy_from_slice(bytes);
    };
    let sp = [b'S', b'P', 7, 1, 0xBE, 0xEF, 0];
    let mut root = support::formats::iso_record(18, 2048, 2, &[0], &sp);
    root.extend(support::formats::iso_record(18, 2048, 2, &[1], &[]));
    let mut link = rock_name("deep");
    link.extend_from_slice(&[b'C', b'L', 12, 1]);
    link.extend_from_slice(&both32(20));
    root.extend(support::formats::iso_record(0, 0, 0, b"DEEP.;1", &link));
    put(18, 0, &root);
    let mut first = support::formats::iso_record(20, 4096, 2, &[0], &[]);
    first.extend(support::formats::iso_record(18, 2048, 2, &[1], &[]));
    first.extend(support::formats::iso_record(
        22,
        5,
        0,
        b"A.TXT;1",
        &rock_name("a.txt"),
    ));
    put(20, 0, &first);
    put(
        21,
        0,
        &support::formats::iso_record(23, 5, 0, b"B.TXT;1", &rock_name("b.txt")),
    );
    put(22, 0, b"first");
    put(23, 0, b"later");
    let mut descriptor = vec![0u8; SECTOR];
    descriptor[0] = 1;
    descriptor[1..6].copy_from_slice(b"CD001");
    descriptor[6] = 1;
    descriptor[80..88].copy_from_slice(&both32(u32::try_from(total).unwrap()));
    descriptor[128..132].copy_from_slice(&[0x00, 0x08, 0x08, 0x00]);
    let root_record = support::formats::iso_record(18, 2048, 2, &[0], &[]);
    descriptor[156..156 + 34].copy_from_slice(&root_record[..34]);
    put(16, 0, &descriptor);
    put(17, 0, &[255, b'C', b'D', b'0', b'0', b'1', 1]);
    image
}

#[test]
fn a_relocated_directory_past_one_sector_lists_every_entry() {
    let fs = open_memory(iso_with_a_relocated_two_sector_directory(), "disc.iso").unwrap();
    assert_eq!(names(&fs, ""), vec!["deep"]);
    assert_eq!(names(&fs, "deep"), vec!["a.txt", "b.txt"]);
    assert_eq!(read(&fs, "deep/b.txt"), b"later");
}

#[test]
fn a_disc_image_with_udf_alone_names_udf_in_its_refusal() {
    let mut image = vec![0u8; 20 * 2048];
    for (sector, id) in [(16, b"BEA01"), (17, b"NSR02"), (18, b"TEA01")] {
        let at = sector * 2048;
        image[at + 1..at + 6].copy_from_slice(id);
        image[at + 6] = 1;
    }
    let error = open_memory(image, "disc.iso").unwrap_err();
    match error {
        VfsError::Unsupported { what } => assert!(what.contains("UDF"), "{what}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_img_opens_only_when_it_holds_iso_9660() {
    let iso = iso_bytes(&[("a.txt", b"a")], IsoNames::Joliet);
    let fs = open_memory(iso, "floppy.img").unwrap();
    assert_eq!(names(&fs, ""), vec!["a.txt"]);

    let raw = vec![0u8; 64 * 1024];
    let error = open_memory(raw, "floppy.img").unwrap_err();
    assert!(matches!(error, VfsError::Unsupported { .. }), "{error:?}");
}

// -- Debian package ----------------------------------------------------------

#[test]
fn a_debian_package_shows_both_tars_expanded() {
    let bytes = deb_bytes(&[("./usr/share/doc/demo/readme", b"package doc")]);
    let fs = open_memory(bytes, "demo_1.0_all.deb").unwrap();
    assert_eq!(fs.format(), ArchiveFormat::Deb);
    assert_eq!(
        names(&fs, ""),
        vec!["control.tar.gz", "data.tar.xz", "debian-binary"]
    );
    assert_eq!(read(&fs, "debian-binary"), b"2.0\n");
    assert_eq!(read(&fs, "control.tar.gz/control"), b"Package: demo\n");
    assert_eq!(
        read(&fs, "data.tar.xz/usr/share/doc/demo/readme"),
        b"package doc"
    );
    let folder = fs.metadata(&path("data.tar.xz")).unwrap();
    assert_eq!(folder.kind, EntryKind::Directory);
    assert_read_only(&fs);
}

#[test]
fn a_zstd_member_stays_a_file_and_says_why() {
    let bytes = support::formats::ar_bytes(&[
        ("debian-binary", b"2.0\n"),
        ("data.tar.zst", b"\x28\xb5\x2f\xfdnot really"),
    ]);
    let fs = open_memory(bytes, "new.deb").unwrap();
    let entry = fs.metadata(&path("data.tar.zst")).unwrap();
    assert_eq!(entry.kind, EntryKind::File);
    assert!(entry.error.unwrap().contains("zstd"));
}

/// GNU `ar` stores a member name longer than its header field in the `//`
/// member and names the member `/` and the offset of the name there.
#[test]
fn a_gnu_long_member_name_resolves_from_the_name_table() {
    let long = "a-member-name-longer-than-its-header.txt";
    let table = format!("{long}/\nsecond-long-member-name.bin/\n");
    let second = format!("/{}", long.len() + 2);
    let bytes = support::formats::ar_bytes(&[
        ("//", table.as_bytes()),
        ("/0", b"first"),
        (&second, b"second"),
        ("/999", b"lost"),
        ("short.txt/", b"short"),
    ]);
    let fs = open_memory(bytes, "long-names.deb").unwrap();
    assert_eq!(
        names(&fs, ""),
        vec!["_999", long, "second-long-member-name.bin", "short.txt"]
    );
    assert_eq!(read(&fs, long), b"first");
    assert_eq!(read(&fs, "second-long-member-name.bin"), b"second");
    assert_eq!(read(&fs, "short.txt"), b"short");

    let unresolved = fs.metadata(&path("_999")).unwrap();
    assert!(unresolved.refused, "{unresolved:?}");
    assert!(
        unresolved
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("/999"),
        "{unresolved:?}"
    );
}

// -- Red Hat package ---------------------------------------------------------

#[test]
fn a_red_hat_package_lists_its_payload() {
    let bytes = rpm_bytes(&[("usr/bin/tool", b"binary"), ("etc/tool.conf", b"k=v\n")]);
    let fs = open_memory(bytes, "tool-1.0.x86_64.rpm").unwrap();
    assert_eq!(fs.format(), ArchiveFormat::Rpm);
    assert_eq!(names(&fs, ""), vec!["etc", "usr"]);
    assert_eq!(read(&fs, "usr/bin/tool"), b"binary");
    assert_eq!(read(&fs, "etc/tool.conf"), b"k=v\n");
    assert_read_only(&fs);
}

#[test]
fn an_xz_payload_reads() {
    let payload = xz(&cpio_bytes(&[("a.txt", b"xz payload")]));
    let fs = open_memory(rpm_with_payload("xz", &payload), "a.rpm").unwrap();
    assert_eq!(read(&fs, "a.txt"), b"xz payload");
}

#[test]
fn a_zstd_payload_is_refused_with_a_message() {
    let error = open_memory(rpm_with_payload("zstd", b"\x28\xb5\x2f\xfd"), "a.rpm").unwrap_err();
    match error {
        VfsError::Unsupported { what } => assert!(what.contains("zstd"), "{what}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

// -- RAR ---------------------------------------------------------------------

#[test]
fn a_rar_reads_through_the_system_tar_or_says_why_not() {
    let bytes = rar_bytes(
        &["docs"],
        &[
            ("docs/readme.txt", b"hello from rar\n"),
            ("top.txt", b"top level\n"),
            ("with space.txt", b"spaced\n"),
        ],
    );
    match open_memory(bytes, "bundle.rar") {
        Ok(fs) => {
            assert_eq!(fs.format(), ArchiveFormat::Rar);
            assert_eq!(names(&fs, ""), vec!["docs", "top.txt", "with space.txt"]);
            assert_eq!(read(&fs, "docs/readme.txt"), b"hello from rar\n");
            assert_eq!(read(&fs, "with space.txt"), b"spaced\n");
            assert_read_only(&fs);
        }
        Err(VfsError::Unsupported { what }) => {
            assert!(what.contains("bsdtar"), "{what}");
            #[cfg(any(windows, target_os = "macos"))]
            panic!("the system tar ships with this platform: {what}");
        }
        Err(other) => panic!("unexpected failure: {other:?}"),
    }
}

#[test]
fn a_damaged_rar_is_an_error_not_a_panic() {
    let bytes = rar_bytes(&[], &[("a.txt", b"payload")]);
    for cut in [8usize, 20, bytes.len() / 2] {
        let result = open_memory(bytes[..cut].to_vec(), "cut.rar");
        if let Ok(fs) = result {
            let _ = fs.list(&VfsPath::root(), &Cancel::new());
        }
    }
}

// -- configuration -----------------------------------------------------------

#[test]
fn a_blank_mask_drops_each_new_format() {
    let cases: [(ArchiveFormat, Vec<u8>, &str); 4] = [
        (
            ArchiveFormat::Cab,
            cab_bytes(&[("a", b"a")], cab::CompressionType::None),
            "a.cab",
        ),
        (
            ArchiveFormat::DiskImage,
            iso_bytes(&[("a", b"a")], IsoNames::Plain),
            "a.iso",
        ),
        (ArchiveFormat::Deb, deb_bytes(&[("a", b"a")]), "a.deb"),
        (ArchiveFormat::Rpm, rpm_bytes(&[("a", b"a")]), "a.rpm"),
    ];
    for (format, bytes, name) in cases {
        let mut types = ArchiveTypes::default();
        types.set_mask(format, Vec::<String>::new());
        let options = ArchiveOptions {
            types,
            ..ArchiveOptions::default()
        };
        assert!(
            open_memory_with(bytes, name, options).is_err(),
            "{name} opened with its format dropped"
        );
    }
}
