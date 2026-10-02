//! Fixtures built byte by byte inside the test binary.
//!
//! Nothing here reads a system file. Each builder writes the same structure a
//! real file carries, into a temporary folder the test owns.

use std::path::{Path, PathBuf};

const REG_HEADER: &str = "Windows Registry Editor Version 5.00\r\n\r\n";

/// Two registry export files: one changed value, one value and one key on the
/// left only, one binary value on the right only.
pub fn registry_pair(dir: &Path) -> (PathBuf, PathBuf) {
    let left = format!(
        "{REG_HEADER}[HKEY_CURRENT_USER\\Software\\Sample]\r\n\
         \"Changed\"=\"one\"\r\n\
         \"Kept\"=\"same\"\r\n\
         \"Gone\"=dword:00000001\r\n\r\n\
         [HKEY_CURRENT_USER\\Software\\Sample\\LeftOnly]\r\n\
         \"Value\"=\"x\"\r\n"
    );
    let right = format!(
        "{REG_HEADER}[HKEY_CURRENT_USER\\Software\\Sample]\r\n\
         \"Changed\"=\"two\"\r\n\
         \"Kept\"=\"same\"\r\n\
         \"Blob\"=hex:01,02,03\r\n"
    );
    write_pair(
        dir,
        "left.reg",
        left.as_bytes(),
        "right.reg",
        right.as_bytes(),
    )
}

/// Two registry export files with the same content.
pub fn registry_same(dir: &Path) -> (PathBuf, PathBuf) {
    let body = format!("{REG_HEADER}[HKEY_CURRENT_USER\\Software\\Same]\r\n\"A\"=\"b\"\r\n");
    write_pair(dir, "one.reg", body.as_bytes(), "two.reg", body.as_bytes())
}

/// Two small Windows binaries whose version resources differ in the file
/// version and in one string.
pub fn version_pair(dir: &Path) -> (PathBuf, PathBuf) {
    let left = pe_with_version(&version_resource(
        &[("CompanyName", "Example"), ("FileVersion", "1.0.0.0")],
        (1, 0, 0, 0),
    ));
    let right = pe_with_version(&version_resource(
        &[("CompanyName", "Example"), ("FileVersion", "1.2.0.0")],
        (1, 2, 0, 0),
    ));
    write_pair(dir, "left.dll", &left, "right.dll", &right)
}

/// Version resources whose left-side text needs quoting when copied as TSV.
pub fn version_copy_pair(dir: &Path) -> (PathBuf, PathBuf) {
    let left = pe_with_version(&version_resource(
        &[
            ("CompanyName", "Acme\tNorth\n\"Tools\""),
            ("FileVersion", "1.0.0.0"),
        ],
        (1, 0, 0, 0),
    ));
    let right = pe_with_version(&version_resource(
        &[("CompanyName", "Acme South"), ("FileVersion", "1.2.0.0")],
        (1, 2, 0, 0),
    ));
    write_pair(dir, "copy-left.dll", &left, "copy-right.dll", &right)
}

/// Two MPEG audio files whose title tags differ.
pub fn media_pair(dir: &Path) -> (PathBuf, PathBuf) {
    write_pair(
        dir,
        "left.mp3",
        &mp3(&[("TIT2", "First"), ("TPE1", "Band")], 4),
        "right.mp3",
        &mp3(&[("TIT2", "Second"), ("TPE1", "Band")], 4),
    )
}

/// ID3 tags whose left-side text needs quoting when copied as TSV.
pub fn media_copy_pair(dir: &Path) -> (PathBuf, PathBuf) {
    write_pair(
        dir,
        "copy-left.mp3",
        &mp3(
            &[("TIT2", "North\tDivision\n\"Tools\""), ("TPE1", "Band")],
            4,
        ),
        "copy-right.mp3",
        &mp3(&[("TIT2", "South Division"), ("TPE1", "Band")], 4),
    )
}

fn write_pair(
    dir: &Path,
    left_name: &str,
    left: &[u8],
    right_name: &str,
    right: &[u8],
) -> (PathBuf, PathBuf) {
    let left_path = dir.join(left_name);
    let right_path = dir.join(right_name);
    std::fs::write(&left_path, left).unwrap_or_default();
    std::fs::write(&right_path, right).unwrap_or_default();
    (left_path, right_path)
}

/// An ID3 version 2.3 tag with text frames, followed by `frames` MPEG frames.
pub fn mp3(tags: &[(&str, &str)], frames: usize) -> Vec<u8> {
    let mut body = Vec::new();
    for (id, text) in tags {
        let mut payload = vec![0u8];
        payload.extend_from_slice(text.as_bytes());
        body.extend_from_slice(id.as_bytes());
        body.extend_from_slice(&u32::try_from(payload.len()).unwrap_or(0).to_be_bytes());
        body.extend_from_slice(&[0, 0]);
        body.extend_from_slice(&payload);
    }
    let size = u32::try_from(body.len()).unwrap_or(0);
    let mut out = Vec::new();
    out.extend_from_slice(b"ID3");
    out.extend_from_slice(&[3, 0, 0]);
    for shift in [21u32, 14, 7, 0] {
        out.push(u8::try_from((size >> shift) & 0x7F).unwrap_or(0));
    }
    out.extend_from_slice(&body);
    // MPEG 1 Layer III at 128 kbps and 44100 Hz: 417 bytes a frame.
    for _ in 0..frames {
        out.extend_from_slice(&[0xFF, 0xFB, 0x90, 0x00]);
        out.extend(std::iter::repeat_n(0u8, 413));
    }
    out
}

fn utf16z(text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for unit in text.encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out.extend_from_slice(&[0, 0]);
    out
}

/// Pad to a four byte boundary measured from the start of the block, which
/// begins six bytes before the body this function sees.
fn pad4(out: &mut Vec<u8>) {
    while !(out.len() + 6).is_multiple_of(4) {
        out.push(0);
    }
}

fn version_block(key: &str, value: &[u8], text_type: bool, children: &[Vec<u8>]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&utf16z(key));
    pad4(&mut body);
    body.extend_from_slice(value);
    pad4(&mut body);
    for child in children {
        body.extend_from_slice(child);
        pad4(&mut body);
    }
    let value_length = if text_type {
        u16::try_from(value.len() / 2).unwrap_or(0)
    } else {
        u16::try_from(value.len()).unwrap_or(0)
    };
    let total = u16::try_from(6 + body.len()).unwrap_or(0);
    let mut out = Vec::new();
    out.extend_from_slice(&total.to_le_bytes());
    out.extend_from_slice(&value_length.to_le_bytes());
    out.extend_from_slice(&u16::from(text_type).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

fn fixed_file_info(file_version: (u16, u16, u16, u16)) -> Vec<u8> {
    let ms = (u32::from(file_version.0) << 16) | u32::from(file_version.1);
    let ls = (u32::from(file_version.2) << 16) | u32::from(file_version.3);
    let words: [u32; 13] = [
        0xFEEF_04BD,
        0x0001_0000,
        ms,
        ls,
        3 << 16,
        0,
        0x3F,
        0x01,
        0x0004_0004,
        1,
        0,
        0,
        0,
    ];
    words.iter().flat_map(|word| word.to_le_bytes()).collect()
}

fn version_resource(entries: &[(&str, &str)], file_version: (u16, u16, u16, u16)) -> Vec<u8> {
    let strings: Vec<Vec<u8>> = entries
        .iter()
        .map(|(name, text)| version_block(name, &utf16z(text), true, &[]))
        .collect();
    let table = version_block("040904B0", &[], false, &strings);
    let string_file_info = version_block("StringFileInfo", &[], false, &[table]);
    let mut translation = Vec::new();
    translation.extend_from_slice(&0x0409u16.to_le_bytes());
    translation.extend_from_slice(&0x04B0u16.to_le_bytes());
    let var = version_block("Translation", &translation, false, &[]);
    let var_file_info = version_block("VarFileInfo", &[], false, &[var]);
    version_block(
        "VS_VERSION_INFO",
        &fixed_file_info(file_version),
        false,
        &[string_file_info, var_file_info],
    )
}

const SECTION_RVA: u32 = 0x1000;
const SECTION_RAW: usize = 0x400;

fn push_dir(out: &mut Vec<u8>) {
    out.extend_from_slice(&[0u8; 14]);
    out.extend_from_slice(&1u16.to_le_bytes());
}

fn push_entry(out: &mut Vec<u8>, id: u32, offset: u32, directory: bool) {
    out.extend_from_slice(&id.to_le_bytes());
    let pointer = if directory {
        offset | 0x8000_0000
    } else {
        offset
    };
    out.extend_from_slice(&pointer.to_le_bytes());
}

fn put(out: &mut [u8], at: usize, bytes: &[u8]) {
    if let Some(slot) = out.get_mut(at..at + bytes.len()) {
        slot.copy_from_slice(bytes);
    }
}

/// A 32 bit Windows binary with one section holding one version resource.
fn pe_with_version(resource: &[u8]) -> Vec<u8> {
    let mut section = Vec::new();
    push_dir(&mut section);
    push_entry(&mut section, 16, 24, true);
    push_dir(&mut section);
    push_entry(&mut section, 1, 48, true);
    push_dir(&mut section);
    let data_entry_at = 72u32;
    push_entry(&mut section, 0x0409, data_entry_at, false);
    let payload_at = data_entry_at + 16;
    section.extend_from_slice(&(SECTION_RVA + payload_at).to_le_bytes());
    section.extend_from_slice(&u32::try_from(resource.len()).unwrap_or(0).to_le_bytes());
    section.extend_from_slice(&[0u8; 8]);
    section.extend_from_slice(resource);

    let optional_size = 224usize;
    let mut out = vec![0u8; SECTION_RAW];
    put(&mut out, 0, b"MZ");
    let pe_at = 0x80usize;
    put(&mut out, 0x3C, &0x80u32.to_le_bytes());
    put(&mut out, pe_at, b"PE\0\0");
    let coff = pe_at + 4;
    put(&mut out, coff, &0x014Cu16.to_le_bytes());
    put(&mut out, coff + 2, &1u16.to_le_bytes());
    put(&mut out, coff + 4, &0x1234_5678u32.to_le_bytes());
    put(&mut out, coff + 16, &224u16.to_le_bytes());
    let optional = coff + 20;
    put(&mut out, optional, &0x010Bu16.to_le_bytes());
    put(&mut out, optional + 68, &3u16.to_le_bytes());
    put(&mut out, optional + 70, &0x4160u16.to_le_bytes());
    let dir_count_at = optional + 92;
    put(&mut out, dir_count_at, &16u32.to_le_bytes());
    let resource_dir = dir_count_at + 4 + 2 * 8;
    let section_len = u32::try_from(section.len()).unwrap_or(0);
    put(&mut out, resource_dir, &SECTION_RVA.to_le_bytes());
    put(&mut out, resource_dir + 4, &section_len.to_le_bytes());
    let header = optional + optional_size;
    put(&mut out, header, b".rsrc\0\0\0");
    put(&mut out, header + 8, &section_len.to_le_bytes());
    put(&mut out, header + 12, &SECTION_RVA.to_le_bytes());
    put(&mut out, header + 16, &section_len.to_le_bytes());
    put(
        &mut out,
        header + 20,
        &u32::try_from(SECTION_RAW).unwrap_or(0).to_le_bytes(),
    );
    out.extend_from_slice(&section);
    out
}
