//! Fixtures built byte by byte inside the test binary.
//!
//! Nothing here reads a file from the repository. Each builder writes the same
//! structure a real file carries, so a parser change that breaks a real file
//! breaks these too.

#![allow(dead_code)]

/// Encode text as little endian UTF-16 with a trailing null unit.
#[must_use]
pub fn utf16z(text: &str) -> Vec<u8> {
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

/// Build one `VS_VERSIONINFO` style block.
#[must_use]
pub fn version_block(key: &str, value: &[u8], text_type: bool, children: &[Vec<u8>]) -> Vec<u8> {
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
    let mut out = Vec::with_capacity(usize::from(total));
    out.extend_from_slice(&total.to_le_bytes());
    out.extend_from_slice(&value_length.to_le_bytes());
    out.extend_from_slice(&u16::from(text_type).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

/// Build a fixed file information structure.
#[must_use]
pub fn fixed_file_info(file_version: (u16, u16, u16, u16), product_major: u16) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0xFEEF_04BDu32.to_le_bytes());
    out.extend_from_slice(&0x0001_0000u32.to_le_bytes());
    let ms = (u32::from(file_version.0) << 16) | u32::from(file_version.1);
    let ls = (u32::from(file_version.2) << 16) | u32::from(file_version.3);
    out.extend_from_slice(&ms.to_le_bytes());
    out.extend_from_slice(&ls.to_le_bytes());
    out.extend_from_slice(&(u32::from(product_major) << 16).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0x3Fu32.to_le_bytes());
    out.extend_from_slice(&0x01u32.to_le_bytes());
    out.extend_from_slice(&0x0004_0004u32.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out
}

/// Build a whole version resource with one string table.
#[must_use]
pub fn version_resource(entries: &[(&str, &str)], file_version: (u16, u16, u16, u16)) -> Vec<u8> {
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
        &fixed_file_info(file_version, 3),
        false,
        &[string_file_info, var_file_info],
    )
}

/// Layout of the fixture binary, so a test can point at a structure.
pub const SECTION_RVA: u32 = 0x1000;
/// File offset of the one section's raw data.
pub const SECTION_RAW: u32 = 0x400;

/// Build a 32 bit Windows binary that carries one version resource.
///
/// The binary holds a DOS stub, a PE signature, one optional header, one
/// section and a three level resource directory with one version leaf.
#[must_use]
pub fn pe_with_version(resource: &[u8], is_64_bit: bool, signed: bool) -> Vec<u8> {
    let mut resource_section = Vec::new();
    // Level 1: one entry for the version resource type.
    push_dir(&mut resource_section, 1);
    push_entry(&mut resource_section, 16, 16 + 8, true);
    // Level 2: one named entry.
    push_dir(&mut resource_section, 1);
    push_entry(&mut resource_section, 1, 16 + 8 + 16 + 8, true);
    // Level 3: one language entry pointing at the data entry.
    push_dir(&mut resource_section, 1);
    let data_entry_at = 16 + 8 + 16 + 8 + 16 + 8;
    push_entry(&mut resource_section, 0x0409, data_entry_at, false);
    push_data_entry(&mut resource_section, data_entry_at, resource);
    pe_with_resource_section(&resource_section, is_64_bit, signed)
}

/// A resource section whose every type entry names one second level
/// directory, whose every entry names one third level directory, whose every
/// entry names one data entry for `resource`. Each level holds `count`
/// entries, so a walk that follows every entry reaches `count` cubed leaves.
#[must_use]
pub fn shared_resource_directories(count: u16, resource: &[u8]) -> Vec<u8> {
    let level = 16 + 8 * u32::from(count);
    let mut out = Vec::new();
    push_dir(&mut out, count);
    for _ in 0..count {
        push_entry(&mut out, 16, level, true);
    }
    push_dir(&mut out, count);
    for index in 0..count {
        push_entry(&mut out, u32::from(index) + 1, 2 * level, true);
    }
    push_dir(&mut out, count);
    let data_entry_at = 3 * level;
    for _ in 0..count {
        push_entry(&mut out, 0x0409, data_entry_at, false);
    }
    push_data_entry(&mut out, data_entry_at, resource);
    out
}

/// A resource section with one version type entry, `names` second level
/// entries, and a third level directory of its own under each of them with
/// `leaves` entries, all naming one data entry for `resource`.
#[must_use]
pub fn distinct_resource_directories(names: u16, leaves: u16, resource: &[u8]) -> Vec<u8> {
    let third = 16 + 8 * u32::from(leaves);
    let third_start = 16 + 8 + 16 + 8 * u32::from(names);
    let data_entry_at = third_start + u32::from(names) * third;
    let mut out = Vec::new();
    push_dir(&mut out, 1);
    push_entry(&mut out, 16, 16 + 8, true);
    push_dir(&mut out, names);
    for index in 0..names {
        let at = third_start + u32::from(index) * third;
        push_entry(&mut out, u32::from(index) + 1, at, true);
    }
    for _ in 0..names {
        push_dir(&mut out, leaves);
        for _ in 0..leaves {
            push_entry(&mut out, 0x0409, data_entry_at, false);
        }
    }
    push_data_entry(&mut out, data_entry_at, resource);
    out
}

/// A data entry at `at` of the section, followed by the resource it names.
fn push_data_entry(out: &mut Vec<u8>, at: u32, resource: &[u8]) {
    let payload_at = at + 16;
    out.extend_from_slice(&(SECTION_RVA + payload_at).to_le_bytes());
    out.extend_from_slice(&u32::try_from(resource.len()).unwrap_or(0).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(resource);
}

/// Build a 32 or 64 bit Windows binary whose one section is `resource_section`,
/// named by the resource data directory.
#[must_use]
pub fn pe_with_resource_section(resource_section: &[u8], is_64_bit: bool, signed: bool) -> Vec<u8> {
    let optional_size: usize = if is_64_bit { 240 } else { 224 };
    let mut out = vec![0u8; SECTION_RAW as usize];
    out[0] = b'M';
    out[1] = b'Z';
    let pe_at = 0x80usize;
    out[0x3C..0x40].copy_from_slice(&u32::try_from(pe_at).unwrap_or(0).to_le_bytes());
    out[pe_at..pe_at + 4].copy_from_slice(b"PE\0\0");
    let coff = pe_at + 4;
    let machine: u16 = if is_64_bit { 0x8664 } else { 0x014C };
    out[coff..coff + 2].copy_from_slice(&machine.to_le_bytes());
    out[coff + 2..coff + 4].copy_from_slice(&1u16.to_le_bytes());
    out[coff + 4..coff + 8].copy_from_slice(&0x1234_5678u32.to_le_bytes());
    out[coff + 16..coff + 18]
        .copy_from_slice(&u16::try_from(optional_size).unwrap_or(0).to_le_bytes());
    let optional = coff + 20;
    let magic: u16 = if is_64_bit { 0x020B } else { 0x010B };
    out[optional..optional + 2].copy_from_slice(&magic.to_le_bytes());
    out[optional + 68..optional + 70].copy_from_slice(&3u16.to_le_bytes());
    out[optional + 70..optional + 72].copy_from_slice(&0x4160u16.to_le_bytes());
    let dir_count_at = optional + if is_64_bit { 108 } else { 92 };
    out[dir_count_at..dir_count_at + 4].copy_from_slice(&16u32.to_le_bytes());
    let dirs = dir_count_at + 4;
    let resource_dir = dirs + 2 * 8;
    out[resource_dir..resource_dir + 4].copy_from_slice(&SECTION_RVA.to_le_bytes());
    out[resource_dir + 4..resource_dir + 8].copy_from_slice(
        &u32::try_from(resource_section.len())
            .unwrap_or(0)
            .to_le_bytes(),
    );
    if signed {
        let certificate = dirs + 4 * 8;
        out[certificate..certificate + 4].copy_from_slice(&0x3000u32.to_le_bytes());
        out[certificate + 4..certificate + 8].copy_from_slice(&0x200u32.to_le_bytes());
    }

    let section = optional + optional_size;
    out[section..section + 8].copy_from_slice(b".rsrc\0\0\0");
    let virtual_size = u32::try_from(resource_section.len()).unwrap_or(0);
    out[section + 8..section + 12].copy_from_slice(&virtual_size.to_le_bytes());
    out[section + 12..section + 16].copy_from_slice(&SECTION_RVA.to_le_bytes());
    out[section + 16..section + 20].copy_from_slice(&virtual_size.to_le_bytes());
    out[section + 20..section + 24].copy_from_slice(&SECTION_RAW.to_le_bytes());

    out.resize(SECTION_RAW as usize, 0);
    out.extend_from_slice(resource_section);
    out
}

fn push_dir(out: &mut Vec<u8>, id_entries: u16) {
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&id_entries.to_le_bytes());
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

/// Build an ID3 version 1 tag.
#[must_use]
pub fn id3v1(title: &str, artist: &str, album: &str, track: Option<u8>) -> Vec<u8> {
    let mut out = vec![0u8; 128];
    out[0..3].copy_from_slice(b"TAG");
    put_ascii(&mut out, 3, 30, title);
    put_ascii(&mut out, 33, 30, artist);
    put_ascii(&mut out, 63, 30, album);
    put_ascii(&mut out, 93, 4, "2001");
    if let Some(number) = track {
        out[125] = 0;
        out[126] = number;
    }
    out[127] = 17;
    out
}

fn put_ascii(out: &mut [u8], at: usize, len: usize, text: &str) {
    for (index, byte) in text.bytes().take(len).enumerate() {
        if let Some(slot) = out.get_mut(at + index) {
            *slot = byte;
        }
    }
}

/// Build an ID3 version 2.3 tag with text frames.
#[must_use]
pub fn id3v2(frames: &[(&str, &str)]) -> Vec<u8> {
    let mut body = Vec::new();
    for (id, text) in frames {
        let mut payload = vec![0u8];
        payload.extend_from_slice(text.as_bytes());
        body.extend_from_slice(id.as_bytes());
        body.extend_from_slice(&u32::try_from(payload.len()).unwrap_or(0).to_be_bytes());
        body.extend_from_slice(&[0, 0]);
        body.extend_from_slice(&payload);
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"ID3");
    out.push(3);
    out.push(0);
    out.push(0);
    out.extend_from_slice(&syncsafe(u32::try_from(body.len()).unwrap_or(0)));
    out.extend_from_slice(&body);
    out
}

fn syncsafe(value: u32) -> [u8; 4] {
    [
        u8::try_from((value >> 21) & 0x7F).unwrap_or(0),
        u8::try_from((value >> 14) & 0x7F).unwrap_or(0),
        u8::try_from((value >> 7) & 0x7F).unwrap_or(0),
        u8::try_from(value & 0x7F).unwrap_or(0),
    ]
}

/// Build `count` MPEG 1 Layer III frames at 128 kbps and 44100 Hz.
#[must_use]
pub fn mpeg_frames(count: usize) -> Vec<u8> {
    // 144 * 128000 / 44100 = 417 bytes per frame with no padding.
    let frame_len = 417usize;
    let mut out = Vec::with_capacity(frame_len * count);
    for _ in 0..count {
        out.extend_from_slice(&[0xFF, 0xFB, 0x90, 0x00]);
        out.extend(std::iter::repeat_n(0u8, frame_len - 4));
    }
    out
}

/// Build a minimal FLAC file with one stream information block.
#[must_use]
pub fn flac(sample_rate: u32, channels: u8, comments: &[(&str, &str)]) -> Vec<u8> {
    let mut stream_info = vec![0u8; 34];
    stream_info[10] = u8::try_from((sample_rate >> 12) & 0xFF).unwrap_or(0);
    stream_info[11] = u8::try_from((sample_rate >> 4) & 0xFF).unwrap_or(0);
    stream_info[12] =
        u8::try_from(((sample_rate & 0x0F) << 4) | (u32::from(channels - 1) << 1)).unwrap_or(0);
    stream_info[13] = 0xF0;

    let mut comment = Vec::new();
    let vendor = b"fixture";
    comment.extend_from_slice(&u32::try_from(vendor.len()).unwrap_or(0).to_le_bytes());
    comment.extend_from_slice(vendor);
    comment.extend_from_slice(&u32::try_from(comments.len()).unwrap_or(0).to_le_bytes());
    for (name, value) in comments {
        let line = format!("{name}={value}");
        comment.extend_from_slice(&u32::try_from(line.len()).unwrap_or(0).to_le_bytes());
        comment.extend_from_slice(line.as_bytes());
    }

    let mut out = Vec::new();
    out.extend_from_slice(b"fLaC");
    push_flac_block(&mut out, 0, false, &stream_info);
    push_flac_block(&mut out, 4, true, &comment);
    out
}

fn push_flac_block(out: &mut Vec<u8>, kind: u8, last: bool, body: &[u8]) {
    out.push(if last { kind | 0x80 } else { kind });
    let size = u32::try_from(body.len()).unwrap_or(0);
    out.push(u8::try_from((size >> 16) & 0xFF).unwrap_or(0));
    out.push(u8::try_from((size >> 8) & 0xFF).unwrap_or(0));
    out.push(u8::try_from(size & 0xFF).unwrap_or(0));
    out.extend_from_slice(body);
}

/// Build a minimal RIFF WAVE file with a format chunk and a data chunk.
#[must_use]
pub fn wav(sample_rate: u32, channels: u16, data_len: usize) -> Vec<u8> {
    let mut fmt = Vec::new();
    fmt.extend_from_slice(&1u16.to_le_bytes());
    fmt.extend_from_slice(&channels.to_le_bytes());
    fmt.extend_from_slice(&sample_rate.to_le_bytes());
    let byte_rate = sample_rate * u32::from(channels) * 2;
    fmt.extend_from_slice(&byte_rate.to_le_bytes());
    fmt.extend_from_slice(&(channels * 2).to_le_bytes());
    fmt.extend_from_slice(&16u16.to_le_bytes());

    let mut body = Vec::new();
    body.extend_from_slice(b"WAVE");
    body.extend_from_slice(b"fmt ");
    body.extend_from_slice(&u32::try_from(fmt.len()).unwrap_or(0).to_le_bytes());
    body.extend_from_slice(&fmt);
    body.extend_from_slice(b"data");
    body.extend_from_slice(&u32::try_from(data_len).unwrap_or(0).to_le_bytes());
    body.extend(std::iter::repeat_n(0u8, data_len));

    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&u32::try_from(body.len()).unwrap_or(0).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

/// Build a minimal MP4 file with a movie header and a metadata list.
#[must_use]
pub fn mp4(duration_ms: u64, items: &[(&str, &str)]) -> Vec<u8> {
    let mut mvhd = vec![0u8; 100];
    mvhd[12..16].copy_from_slice(&1000u32.to_le_bytes().map(|byte| byte).to_vec()[..4]);
    mvhd[12..16].copy_from_slice(&1000u32.to_be_bytes());
    mvhd[16..20].copy_from_slice(&u32::try_from(duration_ms).unwrap_or(0).to_be_bytes());

    let mut ilst = Vec::new();
    for (id, text) in items {
        let mut data = Vec::new();
        data.extend_from_slice(&u32::try_from(16 + text.len()).unwrap_or(0).to_be_bytes());
        data.extend_from_slice(b"data");
        data.extend_from_slice(&1u32.to_be_bytes());
        data.extend_from_slice(&0u32.to_be_bytes());
        data.extend_from_slice(text.as_bytes());
        // The item name is four bytes, one per character, so a name starting
        // with the character U+00A9 writes the single byte 0xA9.
        let raw: Vec<u8> = id
            .chars()
            .map(|ch| u8::try_from(u32::from(ch)).unwrap_or(b'?'))
            .collect();
        ilst.extend_from_slice(&u32::try_from(8 + data.len()).unwrap_or(0).to_be_bytes());
        ilst.extend_from_slice(&raw);
        ilst.extend_from_slice(&data);
    }

    let ilst_box = mp4_box(*b"ilst", &ilst);
    let mut meta_body = vec![0u8; 4];
    meta_body.extend_from_slice(&ilst_box);
    let meta = mp4_box(*b"meta", &meta_body);
    let udta = mp4_box(*b"udta", &meta);
    let mut moov_body = mp4_box(*b"mvhd", &mvhd);
    moov_body.extend_from_slice(&udta);
    let moov = mp4_box(*b"moov", &moov_body);

    let mut out = mp4_box(*b"ftyp", b"M4A \0\0\0\0M4A mp42");
    out.extend_from_slice(&moov);
    out
}

pub fn mp4_box(kind: [u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + body.len());
    out.extend_from_slice(&u32::try_from(8 + body.len()).unwrap_or(0).to_be_bytes());
    out.extend_from_slice(&kind);
    out.extend_from_slice(body);
    out
}

/// Encode text as UTF-16 little endian with a byte order mark.
#[must_use]
pub fn utf16_file(text: &str) -> Vec<u8> {
    let mut out = vec![0xFF, 0xFE];
    for unit in text.encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out
}
