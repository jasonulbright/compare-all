//! Builders for the cabinet, disc image, package and RAR containers the tests
//! read. Each one writes the structure field by field, so a test can also
//! build the malformed variants a real tool would refuse to write.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_possible_truncation,
    dead_code
)]

use std::io::{Cursor, Write};

/// A cabinet holding `entries` in one folder compressed with `compression`.
pub fn cab_bytes(entries: &[(&str, &[u8])], compression: cab::CompressionType) -> Vec<u8> {
    let mut builder = cab::CabinetBuilder::new();
    {
        let folder = builder.add_folder(compression);
        for (name, _) in entries {
            folder.add_file(*name);
        }
    }
    let mut writer = builder.build(Cursor::new(Vec::new())).unwrap();
    let mut index = 0usize;
    while let Some(mut file) = writer.next_file().unwrap() {
        file.write_all(entries[index].1).unwrap();
        index += 1;
    }
    writer.finish().unwrap().into_inner()
}

/// Which names a disc image carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsoNames {
    /// ISO 9660 names only.
    Plain,
    /// A Joliet supplementary volume beside the primary one.
    Joliet,
    /// Rock Ridge entries on the primary volume.
    RockRidge,
}

const SECTOR: usize = 2048;

fn both16(value: u16) -> [u8; 4] {
    let le = value.to_le_bytes();
    let be = value.to_be_bytes();
    [le[0], le[1], be[0], be[1]]
}

fn both32(value: u32) -> [u8; 8] {
    let le = value.to_le_bytes();
    let be = value.to_be_bytes();
    [le[0], le[1], le[2], le[3], be[0], be[1], be[2], be[3]]
}

/// One directory record.
pub fn iso_record(extent: u32, size: u32, flags: u8, name: &[u8], system_use: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; 33];
    out[2..10].copy_from_slice(&both32(extent));
    out[10..18].copy_from_slice(&both32(size));
    out[18..25].copy_from_slice(&[124, 5, 6, 7, 8, 9, 0]);
    out[25] = flags;
    out[28..32].copy_from_slice(&both16(1));
    out[32] = name.len() as u8;
    out.extend_from_slice(name);
    if name.len().is_multiple_of(2) {
        out.push(0);
    }
    out.extend_from_slice(system_use);
    out[0] = out.len() as u8;
    out
}

fn rock_name(name: &str, mode: u32) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"NM");
    out.push((5 + name.len()) as u8);
    out.push(1);
    out.push(0);
    out.extend_from_slice(name.as_bytes());
    out.extend_from_slice(b"PX");
    out.push(36);
    out.push(1);
    out.extend_from_slice(&both32(mode));
    out.extend_from_slice(&both32(1));
    out.extend_from_slice(&both32(0));
    out.extend_from_slice(&both32(0));
    out
}

fn plain_iso_name(name: &str, is_dir: bool) -> Vec<u8> {
    let upper = name.to_ascii_uppercase().replace([' ', '-'], "_");
    if is_dir {
        upper.into_bytes()
    } else if upper.contains('.') {
        format!("{upper};1").into_bytes()
    } else {
        format!("{upper}.;1").into_bytes()
    }
}

fn joliet_iso_name(name: &str, is_dir: bool) -> Vec<u8> {
    let text = if is_dir {
        name.to_owned()
    } else {
        format!("{name};1")
    };
    text.encode_utf16().flat_map(u16::to_be_bytes).collect()
}

/// A disc image holding `files`, with every parent directory implied.
#[allow(clippy::too_many_lines)]
pub fn iso_bytes(files: &[(&str, &[u8])], names: IsoNames) -> Vec<u8> {
    let mut dirs: Vec<String> = vec![String::new()];
    for (path, _) in files {
        let parts: Vec<&str> = path.split('/').collect();
        for depth in 1..parts.len() {
            let dir = parts[..depth].join("/");
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
    }
    dirs.sort();
    let volumes = if names == IsoNames::Joliet { 2 } else { 1 };
    let first_dir = 19usize;
    let dir_sector = |volume: usize, index: usize| (first_dir + volume * dirs.len() + index) as u32;
    let mut next = first_dir + volumes * dirs.len();
    let mut file_sectors = Vec::new();
    for (_, content) in files {
        file_sectors.push(next as u32);
        next += content.len().div_ceil(SECTOR).max(1);
    }
    let total = next;
    let mut image = vec![0u8; total * SECTOR];

    for volume in 0..volumes {
        let joliet = volume == 1;
        for (index, dir) in dirs.iter().enumerate() {
            let mut body = Vec::new();
            let own = dir_sector(volume, index);
            let parent = if dir.is_empty() {
                own
            } else {
                let up = dir.rsplit_once('/').map_or("", |(head, _)| head);
                dir_sector(volume, dirs.iter().position(|d| d == up).unwrap())
            };
            let sp: &[u8] = if names == IsoNames::RockRidge && dir.is_empty() {
                &[b'S', b'P', 7, 1, 0xBE, 0xEF, 0]
            } else {
                &[]
            };
            body.extend(iso_record(own, SECTOR as u32, 2, &[0], sp));
            body.extend(iso_record(parent, SECTOR as u32, 2, &[1], &[]));
            for (child_index, child) in dirs.iter().enumerate() {
                let (parent_of_child, rest) = child.rsplit_once('/').unwrap_or(("", child));
                if child.is_empty() || parent_of_child != dir {
                    continue;
                }
                let name = if joliet {
                    joliet_iso_name(rest, true)
                } else {
                    plain_iso_name(rest, true)
                };
                let su = if names == IsoNames::RockRidge {
                    rock_name(rest, 0o040_755)
                } else {
                    Vec::new()
                };
                body.extend(iso_record(
                    dir_sector(volume, child_index),
                    SECTOR as u32,
                    2,
                    &name,
                    &su,
                ));
            }
            for (file_index, (path, content)) in files.iter().enumerate() {
                let (head, leaf) = path.rsplit_once('/').unwrap_or(("", path));
                if head != dir {
                    continue;
                }
                let name = if joliet {
                    joliet_iso_name(leaf, false)
                } else {
                    plain_iso_name(leaf, false)
                };
                let su = if names == IsoNames::RockRidge {
                    rock_name(leaf, 0o100_644)
                } else {
                    Vec::new()
                };
                body.extend(iso_record(
                    file_sectors[file_index],
                    content.len() as u32,
                    0,
                    &name,
                    &su,
                ));
            }
            assert!(body.len() <= SECTOR, "test directory too large");
            let at = own as usize * SECTOR;
            image[at..at + body.len()].copy_from_slice(&body);
        }
    }
    for (file_index, (_, content)) in files.iter().enumerate() {
        let at = file_sectors[file_index] as usize * SECTOR;
        image[at..at + content.len()].copy_from_slice(content);
    }

    for volume in 0..volumes {
        let at = (16 + volume) * SECTOR;
        let descriptor = &mut image[at..at + SECTOR];
        descriptor[0] = if volume == 0 { 1 } else { 2 };
        descriptor[1..6].copy_from_slice(b"CD001");
        descriptor[6] = 1;
        descriptor[80..88].copy_from_slice(&both32(total as u32));
        if volume == 1 {
            descriptor[88..91].copy_from_slice(b"%/E");
        }
        descriptor[128..132].copy_from_slice(&both16(SECTOR as u16));
        let root = iso_record(dir_sector(volume, 0), SECTOR as u32, 2, &[0], &[]);
        descriptor[156..156 + 34].copy_from_slice(&root[..34]);
    }
    let end = (16 + volumes) * SECTOR;
    image[end] = 255;
    image[end + 1..end + 6].copy_from_slice(b"CD001");
    image[end + 6] = 1;
    image
}

/// An `ar` container holding `members`.
pub fn ar_bytes(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = b"!<arch>\n".to_vec();
    for (name, content) in members {
        let header = format!(
            "{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
            name,
            1_600_000_000,
            0,
            0,
            100_644,
            content.len()
        );
        assert_eq!(header.len(), 60);
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(content);
        if out.len() % 2 == 1 {
            out.push(b'\n');
        }
    }
    out
}

/// A Debian package with a gzip control tar and an xz data tar.
pub fn deb_bytes(data: &[(&str, &[u8])]) -> Vec<u8> {
    let control = super::gzip(&super::tar_bytes(&[("./control", b"Package: demo\n")]));
    let payload = super::xz(&super::tar_bytes(data));
    ar_bytes(&[
        ("debian-binary", b"2.0\n"),
        ("control.tar.gz", &control),
        ("data.tar.xz", &payload),
    ])
}

/// One cpio "newc" record.
pub fn cpio_record(name: &str, mode: u32, data: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "070701{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}",
        1,
        mode,
        0,
        0,
        1,
        1_700_000_000,
        data.len(),
        0,
        0,
        0,
        0,
        name.len() + 1,
        0
    )
    .into_bytes();
    out.extend_from_slice(name.as_bytes());
    out.push(0);
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
    out.extend_from_slice(data);
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
    out
}

/// A cpio archive of `files`, each under a `./` prefix, with its trailer.
pub fn cpio_bytes(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, data) in files {
        out.extend(cpio_record(&format!("./{name}"), 0o100_644, data));
    }
    out.extend(cpio_record("TRAILER!!!", 0, b""));
    out
}

fn rpm_header(tags: &[(u32, &str)]) -> Vec<u8> {
    let mut index = Vec::new();
    let mut store = Vec::new();
    for (tag, value) in tags {
        index.extend_from_slice(&tag.to_be_bytes());
        index.extend_from_slice(&6u32.to_be_bytes());
        index.extend_from_slice(&(store.len() as u32).to_be_bytes());
        index.extend_from_slice(&1u32.to_be_bytes());
        store.extend_from_slice(value.as_bytes());
        store.push(0);
    }
    let mut out = vec![0x8E, 0xAD, 0xE8, 0x01, 0, 0, 0, 0];
    out.extend_from_slice(&(tags.len() as u32).to_be_bytes());
    out.extend_from_slice(&(store.len() as u32).to_be_bytes());
    out.extend(index);
    out.extend(store);
    out
}

/// A Red Hat package whose payload is `payload`, compressed as `compressor`
/// already names.
pub fn rpm_with_payload(compressor: &str, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![0xED, 0xAB, 0xEE, 0xDB, 3, 0];
    out.resize(96, 0);
    out.extend(rpm_header(&[]));
    while !out.len().is_multiple_of(8) {
        out.push(0);
    }
    out.extend(rpm_header(&[(1124, "cpio"), (1125, compressor)]));
    out.extend_from_slice(payload);
    out
}

/// A Red Hat package holding `files` in a gzip payload.
pub fn rpm_bytes(files: &[(&str, &[u8])]) -> Vec<u8> {
    rpm_with_payload("gzip", &super::gzip(&cpio_bytes(files)))
}

fn rar_block(kind: u8, flags: u16, body: &[u8], data: &[u8]) -> Vec<u8> {
    let size = (7 + body.len()) as u16;
    let mut rest = vec![kind];
    rest.extend_from_slice(&flags.to_le_bytes());
    rest.extend_from_slice(&size.to_le_bytes());
    rest.extend_from_slice(body);
    let crc = (crc32fast::hash(&rest) & 0xFFFF) as u16;
    let mut out = crc.to_le_bytes().to_vec();
    out.extend(rest);
    out.extend_from_slice(data);
    out
}

/// A RAR 4 container holding `files` stored without compression, with the
/// directories named in `dirs`.
pub fn rar_bytes(dirs: &[&str], files: &[(&str, &[u8])]) -> Vec<u8> {
    let stamp: u32 = (44 << 25) | (5 << 21) | (6 << 16) | (7 << 11) | (8 << 5) | 5;
    let entry = |name: &str, data: &[u8], is_dir: bool| {
        let mut body = Vec::new();
        body.extend_from_slice(&(data.len() as u32).to_le_bytes());
        body.extend_from_slice(&(data.len() as u32).to_le_bytes());
        body.push(3);
        body.extend_from_slice(&crc32fast::hash(data).to_le_bytes());
        body.extend_from_slice(&stamp.to_le_bytes());
        body.push(29);
        body.push(0x30);
        body.extend_from_slice(&(name.len() as u16).to_le_bytes());
        body.extend_from_slice(&(if is_dir { 0o040_755u32 } else { 0o100_644u32 }).to_le_bytes());
        body.extend_from_slice(name.as_bytes());
        let flags = 0x8000 | if is_dir { 0x00E0 } else { 0 };
        rar_block(0x74, flags, &body, data)
    };
    let mut out = b"Rar!\x1a\x07\x00".to_vec();
    out.extend(rar_block(0x73, 0, &[0u8; 6], &[]));
    for dir in dirs {
        out.extend(entry(dir, b"", true));
    }
    for (name, data) in files {
        out.extend(entry(name, data, false));
    }
    out.extend(rar_block(0x7B, 0x4000, &[], &[]));
    out
}
