//! Fixture builders shared by the test suites.
//!
//! Every container the tests read is built here, in memory, so the repository
//! carries no binary fixtures. The RAR container too is written by a builder
//! in `formats`, since no tool on a build machine is assumed to write RAR.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_possible_truncation,
    dead_code
)]

pub mod formats;

use std::io::{Cursor, Write};

use ca_vfs::{ArchiveBacking, ArchiveFs, ArchiveOptions};

/// A zip holding `entries`, plus one directory entry.
pub fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    for (name, content) in entries {
        writer.start_file(*name, options).unwrap();
        writer.write_all(content).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

/// A zip whose only entry is stored under a raw name, bypassing validation.
pub fn zip_with_raw_name(name: &str, content: &[u8]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    writer.start_file(name, options).unwrap();
    writer.write_all(content).unwrap();
    writer.finish().unwrap().into_inner()
}

/// A zip entry encrypted with the classic algorithm.
pub fn zip_encrypted(name: &str, content: &[u8], password: &str) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .with_aes_encryption(zip::AesMode::Aes256, password);
    writer.start_file(name, options).unwrap();
    writer.write_all(content).unwrap();
    writer.finish().unwrap().into_inner()
}

/// One entry written straight into the container, bypassing every check a zip
/// writer would otherwise apply to it.
pub struct ManualEntry {
    /// Name bytes exactly as they go into both headers.
    pub name: Vec<u8>,
    /// Stored content; no compression is applied.
    pub content: Vec<u8>,
    /// Checksum to record, or the real one when `None`.
    pub crc: Option<u32>,
    /// Extra field bytes, written into both headers.
    pub extra: Vec<u8>,
}

impl ManualEntry {
    /// An entry whose headers describe its content correctly.
    pub fn new(name: &str, content: &[u8]) -> Self {
        Self {
            name: name.as_bytes().to_vec(),
            content: content.to_vec(),
            crc: None,
            extra: Vec::new(),
        }
    }

    pub fn with_crc(mut self, crc: u32) -> Self {
        self.crc = Some(crc);
        self
    }

    pub fn with_extra(mut self, extra: Vec<u8>) -> Self {
        self.extra = extra;
        self
    }

    pub fn with_raw_name(mut self, name: &[u8]) -> Self {
        self.name = name.to_vec();
        self
    }
}

/// A zip assembled by hand, so a test can store what a writer would refuse:
/// a duplicate name, a wrong checksum, an unusable name, or an extra field.
pub fn zip_manual(entries: &[ManualEntry], comment: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut offsets = Vec::new();

    for entry in entries {
        offsets.push(out.len() as u32);
        let crc = entry.crc.unwrap_or_else(|| crc32fast::hash(&entry.content));
        let size = entry.content.len() as u32;
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&(entry.name.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entry.extra.len() as u16).to_le_bytes());
        out.extend_from_slice(&entry.name);
        out.extend_from_slice(&entry.extra);
        out.extend_from_slice(&entry.content);
    }

    let central_offset = out.len() as u32;
    let mut central = Vec::new();
    for (entry, offset) in entries.iter().zip(&offsets) {
        let crc = entry.crc.unwrap_or_else(|| crc32fast::hash(&entry.content));
        let size = entry.content.len() as u32;
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&(entry.name.len() as u16).to_le_bytes());
        central.extend_from_slice(&(entry.extra.len() as u16).to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u32.to_le_bytes());
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(&entry.name);
        central.extend_from_slice(&entry.extra);
    }
    let central_len = central.len() as u32;
    out.extend_from_slice(&central);

    let count = entries.len() as u16;
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&central_len.to_le_bytes());
    out.extend_from_slice(&central_offset.to_le_bytes());
    out.extend_from_slice(&(comment.len() as u16).to_le_bytes());
    out.extend_from_slice(comment);
    out
}

/// A zip holding one stored entry whose name bytes are written as given, with
/// the UTF-8 flag clear, so the reader must fall back to the legacy code page.
pub fn zip_with_legacy_name(name_bytes: &[u8], content: &[u8]) -> Vec<u8> {
    zip_manual(
        &[ManualEntry::new("", content).with_raw_name(name_bytes)],
        &[],
    )
}

/// An extended timestamp extra field recording `modified`.
pub fn extended_timestamp(modified: i32) -> Vec<u8> {
    let mut out = vec![0x55, 0x54, 0x05, 0x00, 0x01];
    out.extend_from_slice(&modified.to_le_bytes());
    out
}

/// A tar.gz holding `count` small entries named in order.
pub fn tar_gz_many(count: usize) -> Vec<u8> {
    let bodies: Vec<(String, Vec<u8>)> = (0..count)
        .map(|index| {
            (
                format!("d{}/f{index}.txt", index % 16),
                format!("body {index}").into_bytes(),
            )
        })
        .collect();
    let borrowed: Vec<(&str, &[u8])> = bodies
        .iter()
        .map(|(name, body)| (name.as_str(), body.as_slice()))
        .collect();
    gzip(&tar_bytes(&borrowed))
}

/// A 7z holding `count` small entries named in order.
pub fn sevenz_many(count: usize) -> Vec<u8> {
    let bodies: Vec<(String, Vec<u8>)> = (0..count)
        .map(|index| {
            (
                format!("f{index}.txt"),
                format!("body {index}").into_bytes(),
            )
        })
        .collect();
    let borrowed: Vec<(&str, &[u8])> = bodies
        .iter()
        .map(|(name, body)| (name.as_str(), body.as_slice()))
        .collect();
    sevenz_bytes(&borrowed)
}

/// A gzip stream holding two members back to back, whose footer describes the
/// last member only.
pub fn gzip_two_members(first: &[u8], second: &[u8]) -> Vec<u8> {
    let mut out = gzip(first);
    out.extend_from_slice(&gzip(second));
    out
}

/// A tar holding `entries`.
pub fn tar_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, content) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(1_600_000_000);
        header.set_cksum();
        builder.append_data(&mut header, name, *content).unwrap();
    }
    builder.into_inner().unwrap()
}

/// A tar whose entry names are written straight into the header, bypassing the
/// builder's own refusal to store a traversing name.
pub fn tar_with_raw_names(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, content) in entries {
        let mut header = [0u8; 512];
        let name_bytes = name.as_bytes();
        header
            .get_mut(..name_bytes.len())
            .unwrap()
            .copy_from_slice(name_bytes);
        write_octal(&mut header, 100, 8, 0o644);
        write_octal(&mut header, 108, 8, 0);
        write_octal(&mut header, 116, 8, 0);
        write_octal(&mut header, 124, 12, content.len() as u64);
        write_octal(&mut header, 136, 12, 1_600_000_000);
        header[156] = b'0';
        header
            .get_mut(257..263)
            .unwrap()
            .copy_from_slice(b"ustar\0");
        header.get_mut(263..265).unwrap().copy_from_slice(b"00");
        // The checksum is computed with its own field read as spaces.
        header.get_mut(148..156).unwrap().fill(b' ');
        let sum: u32 = header.iter().map(|byte| u32::from(*byte)).sum();
        write_octal(&mut header, 148, 7, u64::from(sum));
        header[155] = b' ';

        out.extend_from_slice(&header);
        out.extend_from_slice(content);
        let padding = (512 - content.len() % 512) % 512;
        out.extend(std::iter::repeat_n(0u8, padding));
    }
    out.extend(std::iter::repeat_n(0u8, 1024));
    out
}

/// Write a null terminated octal field.
fn write_octal(header: &mut [u8; 512], offset: usize, width: usize, value: u64) {
    let text = format!("{value:0width$o}", width = width - 1);
    let bytes = text.as_bytes();
    header
        .get_mut(offset..offset + bytes.len())
        .unwrap()
        .copy_from_slice(bytes);
}

/// `bytes` inside a gzip stream.
pub fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

/// `bytes` inside a bzip2 stream.
pub fn bzip2(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    banzai::encode(bytes, std::io::BufWriter::new(&mut out), 9).unwrap();
    out
}

/// `bytes` inside an xz stream.
pub fn xz(bytes: &[u8]) -> Vec<u8> {
    let options = lzma_rust2::XzOptions::with_preset(6);
    let mut writer = lzma_rust2::XzWriter::new(Vec::new(), options).unwrap();
    writer.write_all(bytes).unwrap();
    writer.finish().unwrap()
}

/// A 7z holding `entries`.
pub fn sevenz_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = sevenz_rust2::ArchiveWriter::new(Cursor::new(Vec::new())).unwrap();
    for (name, content) in entries {
        let entry = sevenz_rust2::ArchiveEntry::new_file(name);
        writer.push_archive_entry(entry, Some(*content)).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

/// Open a container held in memory.
pub fn open_memory(bytes: Vec<u8>, name: &str) -> ca_vfs::VfsResult<ArchiveFs> {
    ArchiveFs::open(
        ArchiveBacking::from_bytes(bytes),
        name,
        ArchiveOptions::default(),
    )
}

/// Open a container held in memory with explicit options.
pub fn open_memory_with(
    bytes: Vec<u8>,
    name: &str,
    options: ArchiveOptions,
) -> ca_vfs::VfsResult<ArchiveFs> {
    ArchiveFs::open(ArchiveBacking::from_bytes(bytes), name, options)
}
