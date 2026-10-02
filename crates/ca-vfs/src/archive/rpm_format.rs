//! Red Hat packages, read only.
//!
//! A package is a lead, a signature header, a main header and a compressed
//! cpio payload. The headers are read only for the payload's format and
//! compressor; the listing comes from the payload itself, which is decoded
//! once into a temporary file.

use std::io::{Read, Seek, SeekFrom};

use super::span::{
    backing_len, decode_to_temp, range_reader, read_exact_or_corrupt, Compression, SpanIndex,
};
use super::{cpio, ArchiveBacking};
use crate::cancel::Cancel;
use crate::error::{VfsError, VfsResult};
use crate::limits::{Budget, Limits};
use crate::tree::RawEntry;

const LEAD_BYTES: u64 = 96;
const LEAD_MAGIC: [u8; 4] = [0xED, 0xAB, 0xEE, 0xDB];
const HEADER_MAGIC: [u8; 4] = [0x8E, 0xAD, 0xE8, 0x01];
/// Most index entries one header may declare.
const MAX_INDEX_ENTRIES: u32 = 1 << 20;
/// Most bytes one header's data store may declare.
const MAX_STORE_BYTES: u32 = 256 * 1024 * 1024;

const TAG_PAYLOAD_FORMAT: u32 = 1124;
const TAG_PAYLOAD_COMPRESSOR: u32 = 1125;
const TYPE_STRING: u32 = 6;

/// One header structure: its tags and where it ends.
struct Header {
    index: Vec<(u32, u32, u32, u32)>,
    store: Vec<u8>,
    end: u64,
}

impl Header {
    /// The string value of `tag`, when the header holds one.
    fn string(&self, tag: u32) -> Option<String> {
        let (_, kind, offset, _) = self
            .index
            .iter()
            .find(|(candidate, _, _, _)| *candidate == tag)?;
        if *kind != TYPE_STRING {
            return None;
        }
        let start = usize::try_from(*offset).ok()?;
        let rest = self.store.get(start..)?;
        let end = rest.iter().position(|byte| *byte == 0)?;
        Some(String::from_utf8_lossy(rest.get(..end)?).into_owned())
    }
}

/// Read a header structure starting at `offset`.
fn read_header<R: Read + Seek>(reader: &mut R, offset: u64, len: u64) -> VfsResult<Header> {
    let mut intro = [0u8; 16];
    reader.seek(SeekFrom::Start(offset))?;
    read_exact_or_corrupt(reader, &mut intro, "package header")?;
    if intro.get(0..4) != Some(HEADER_MAGIC.as_slice()) {
        return Err(VfsError::corrupt("package header magic is wrong"));
    }
    let count = be32(&intro, 8);
    let store_len = be32(&intro, 12);
    if count > MAX_INDEX_ENTRIES || store_len > MAX_STORE_BYTES {
        return Err(VfsError::corrupt("package header is too large"));
    }
    let index_len = u64::from(count) * 16;
    let end = offset + 16 + index_len + u64::from(store_len);
    if end > len {
        return Err(VfsError::corrupt("package header runs past the file"));
    }
    let mut raw_index = vec![0u8; usize::try_from(index_len).unwrap_or(0)];
    read_exact_or_corrupt(reader, &mut raw_index, "package header index")?;
    let index = raw_index
        .chunks_exact(16)
        .map(|entry| {
            (
                be32(entry, 0),
                be32(entry, 4),
                be32(entry, 8),
                be32(entry, 12),
            )
        })
        .collect();
    let mut store = vec![0u8; usize::try_from(store_len).unwrap_or(0)];
    read_exact_or_corrupt(reader, &mut store, "package header store")?;
    Ok(Header { index, store, end })
}

fn be32(bytes: &[u8], at: usize) -> u32 {
    bytes
        .get(at..at + 4)
        .and_then(|field| <[u8; 4]>::try_from(field).ok())
        .map_or(0, u32::from_be_bytes)
}

/// The compression a payload compressor tag names.
fn compression_of(name: Option<&str>) -> VfsResult<Compression> {
    match name.unwrap_or("gzip") {
        "gzip" => Ok(Compression::Gzip),
        "bzip2" => Ok(Compression::Bzip2),
        "xz" => Ok(Compression::Xz),
        "lzma" => Ok(Compression::Lzma),
        "identity" | "none" => Ok(Compression::None),
        "zstd" => Err(VfsError::unsupported(
            "package payload compressed with zstd: this build has no zstd decoder",
        )),
        other => Err(VfsError::unsupported(format!(
            "package payload compressed with {other}"
        ))),
    }
}

/// Build the listing from the decoded payload.
///
/// # Errors
/// Returns [`VfsError::Unsupported`] for a payload compressor or format this
/// build does not read, [`VfsError::Corrupt`] when a structure does not
/// parse, and [`VfsError::LimitExceeded`] when the payload expands past a
/// ceiling.
pub(crate) fn list(
    backing: &ArchiveBacking,
    limits: &Limits,
    scan: &Budget,
    cancel: &Cancel,
) -> VfsResult<(SpanIndex, Vec<RawEntry>)> {
    let len = backing_len(backing)?;
    let mut reader = backing.reader()?;
    let mut lead = [0u8; 4];
    read_exact_or_corrupt(&mut reader, &mut lead, "package lead")?;
    if lead != LEAD_MAGIC || len < LEAD_BYTES {
        return Err(VfsError::corrupt("package lead magic is wrong"));
    }

    let signature = read_header(&mut reader, LEAD_BYTES, len)?;
    let main_start = signature.end.div_ceil(8) * 8;
    let main = read_header(&mut reader, main_start, len)?;
    drop(reader);

    let format = main.string(TAG_PAYLOAD_FORMAT);
    if format.as_deref().is_some_and(|format| format != "cpio") {
        return Err(VfsError::unsupported(format!(
            "package payload format {}",
            format.unwrap_or_default()
        )));
    }
    let compression = compression_of(main.string(TAG_PAYLOAD_COMPRESSOR).as_deref())?;

    let payload = range_reader(backing, main.end, len - main.end)?;
    let decoder = compression.decoder(payload)?;
    let plain = decode_to_temp(decoder, "package payload", limits, scan, cancel)?;

    let mut index = SpanIndex::default();
    index.sources.push(plain.clone());
    let entries = cpio::list(&plain, 0, &crate::path::VfsPath::root(), &mut index, cancel)?;
    Ok((index, entries))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    #[test]
    fn a_missing_compressor_means_gzip() {
        assert_eq!(compression_of(None).unwrap(), Compression::Gzip);
    }

    #[test]
    fn zstd_is_refused_with_a_reason() {
        let error = compression_of(Some("zstd")).unwrap_err();
        assert!(error.to_string().contains("zstd"), "{error}");
    }
}
