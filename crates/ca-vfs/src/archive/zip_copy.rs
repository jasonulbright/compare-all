//! The records a zip rewrite writes, copied byte for byte, and the directory
//! that lists them.
//!
//! The zip writer this crate builds on writes new headers for a record it
//! copies and leaves out every extra field of the record. The NTFS field and
//! the extended timestamp field, which hold the time of a record in UTC, are
//! extra fields, so a record the writer copies reads back as its DOS stamp.
//! The headers are written here instead, and every copied record keeps its
//! extra fields.

use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;

use super::zip_format::{directory_span, read_u16, read_u32, read_u64};
use super::ArchiveBacking;
use crate::cancel::Cancel;
use crate::error::{VfsError, VfsResult};

/// Signature of a local file header.
const LOCAL_SIGNATURE: u32 = 0x0403_4b50;
/// Signature of one central directory record.
const CENTRAL_SIGNATURE: u32 = 0x0201_4b50;
/// Signature of the end of central directory record.
const END_SIGNATURE: u32 = 0x0605_4b50;
/// Signature of the zip64 end of central directory record.
const ZIP64_END_SIGNATURE: u32 = 0x0606_4b50;
/// Signature of the zip64 end of central directory locator.
const ZIP64_LOCATOR_SIGNATURE: u32 = 0x0706_4b50;
/// Extra field that carries the zip64 sizes and offset.
const EXTRA_ZIP64: u16 = 0x0001;
/// A size or an offset at or above this value is stated in a zip64 field.
const SATURATED: u64 = 0xFFFF_FFFF;
/// A record count at or above this value is stated in a zip64 end record.
const SATURATED_COUNT: u64 = 0xFFFF;
/// General purpose flag: sizes and checksum follow the data.
const FLAG_DESCRIPTOR: u16 = 1 << 3;
/// General purpose flag: the name and the comment are UTF-8.
const FLAG_UTF8: u16 = 1 << 11;
/// Version needed to extract a record that states a zip64 field.
const VERSION_ZIP64: u16 = 45;
/// Bytes of the fixed part of a local header.
const LOCAL_FIXED: u64 = 30;
/// Bytes copied between two checks of the cancel flag.
const COPY_CHUNK: usize = 64 * 1024;

/// Extra field records a copy leaves out.
///
/// The copy states the zip64 field again where the record needs one. The
/// alignment padding and the encryption header describe the old record. A
/// Unicode name or comment record holds a checksum of the old name or comment
/// bytes, which the copy writes again as UTF-8, and the reader refuses the
/// whole container over a checksum that does not match.
const DROPPED_EXTRA: [u16; 5] = [EXTRA_ZIP64, 0xa11e, 0x9901, 0x7075, 0x6375];

/// The fixed fields of one record that its local header and its central
/// directory record share, with the sizes as whole numbers.
#[derive(Debug, Clone, Copy)]
struct Header {
    version_made_by: u16,
    version_needed: u16,
    flags: u16,
    method: u16,
    time: u16,
    date: u16,
    crc: u32,
    compressed: u64,
    size: u64,
    internal: u16,
    external: u32,
}

/// One record of a central directory as it is stored, with its zip64 sizes
/// and offset resolved.
#[derive(Debug)]
pub(super) struct StoredRecord {
    header: Header,
    /// The name bytes of the central directory record.
    name: Vec<u8>,
    /// Offset of the local header.
    offset: u64,
    /// Offset where the central directory starts.
    directory_offset: u64,
    /// The extra field of the central directory record.
    extra: Vec<u8>,
    /// The comment bytes of the central directory record.
    comment: Vec<u8>,
    /// Offset of this record in the central directory.
    central_header_start: u64,
}

impl StoredRecord {
    /// True when the record's UTF-8 flag describes its name and comment bytes.
    pub(super) fn has_valid_utf8_fields(&self) -> bool {
        self.header.flags & FLAG_UTF8 == 0
            || (std::str::from_utf8(&self.name).is_ok()
                && std::str::from_utf8(&self.comment).is_ok())
    }

    /// True when the stored record and the zip reader's index name the same
    /// entry and agree on its encryption, compression, timestamps and bounds.
    #[allow(deprecated)]
    pub(super) fn agrees_with<R: Read>(&self, file: &zip::read::ZipFile<'_, R>) -> bool {
        self.header.crc == file.crc32()
            && self.central_header_start == file.central_header_start()
            && self.header.compressed == file.compressed_size()
            && self.header.size == file.size()
            && self.header.method == file.compression().to_u16()
            && (self.header.flags & 1 != 0) == file.encrypted()
            && zip::DateTime::try_from_msdos(self.header.date, self.header.time).ok()
                == file.last_modified()
            && super::zip_format::ntfs_mtime(&self.extra)
                == super::zip_format::ntfs_mtime(file.extra_data().unwrap_or_default())
            && super::zip_format::extended_mtime(&self.extra)
                == super::zip_format::extended_mtime(file.extra_data().unwrap_or_default())
            && self.name == file.name_raw()
            && self.offset == file.header_start()
    }
}

/// Every record the central directory of `backing` lists, in its order, or
/// `None` when the end records describe a directory the bytes do not hold.
///
/// The records are read one at a time, so nothing is reserved from a size or
/// a count the container states.
///
/// # Errors
/// Returns [`VfsError::Io`] when the container cannot be read.
pub(super) fn stored_records(backing: &ArchiveBacking) -> VfsResult<Option<Vec<StoredRecord>>> {
    let mut reader = backing.reader()?;
    let Some(span) = directory_span(&mut reader) else {
        return Ok(None);
    };
    reader.seek(SeekFrom::Start(span.offset))?;
    let mut directory = BufReader::new(reader.take(span.size));
    let mut out = Vec::new();
    while (out.len() as u64) < span.total {
        let central_header_start = span
            .offset
            .checked_add(directory.stream_position()?)
            .ok_or_else(|| VfsError::corrupt("zip central-directory offset overflowed"))?;
        let Some(record) = read_stored_record(&mut directory, span.offset, central_header_start)
        else {
            return Ok(None);
        };
        out.push(record);
    }
    Ok(Some(out))
}

/// One central directory record, or `None` when the bytes are not one.
fn read_stored_record<R: Read>(
    reader: &mut R,
    directory_offset: u64,
    central_header_start: u64,
) -> Option<StoredRecord> {
    let mut fixed = [0u8; 46];
    reader.read_exact(&mut fixed).ok()?;
    if read_u32(&fixed, 0)? != CENTRAL_SIGNATURE {
        return None;
    }
    let mut name = vec![0u8; usize::from(read_u16(&fixed, 28)?)];
    reader.read_exact(&mut name).ok()?;
    let mut extra = vec![0u8; usize::from(read_u16(&fixed, 30)?)];
    reader.read_exact(&mut extra).ok()?;
    let mut comment = vec![0u8; usize::from(read_u16(&fixed, 32)?)];
    reader.read_exact(&mut comment).ok()?;
    let mut compressed = u64::from(read_u32(&fixed, 20)?);
    let mut size = u64::from(read_u32(&fixed, 24)?);
    let mut offset = u64::from(read_u32(&fixed, 42)?);
    resolve_zip64(&extra, &mut size, &mut compressed, &mut offset);
    Some(StoredRecord {
        header: Header {
            version_made_by: read_u16(&fixed, 4)?,
            version_needed: read_u16(&fixed, 6)?,
            flags: read_u16(&fixed, 8)?,
            method: read_u16(&fixed, 10)?,
            time: read_u16(&fixed, 12)?,
            date: read_u16(&fixed, 14)?,
            crc: read_u32(&fixed, 16)?,
            compressed,
            size,
            internal: read_u16(&fixed, 36)?,
            external: read_u32(&fixed, 38)?,
        },
        name,
        offset,
        directory_offset,
        extra,
        comment,
        central_header_start,
    })
}

/// Replace each saturated field with the value the zip64 field states.
///
/// A zip64 field of 24 bytes or more states all three values whatever the
/// saturation, which is how the zip reader reads it too, so both readings
/// agree on every record.
fn resolve_zip64(extra: &[u8], size: &mut u64, compressed: &mut u64, offset: &mut u64) {
    let mut at = 0usize;
    while let (Some(id), Some(len)) = (read_u16(extra, at), read_u16(extra, at + 2)) {
        let start = at + 4;
        let end = start + usize::from(len);
        let Some(payload) = extra.get(start..end) else {
            return;
        };
        if id == EXTRA_ZIP64 {
            let whole = payload.len() >= 24;
            let mut next = 0usize;
            for field in [size, compressed, offset] {
                if whole || *field == SATURATED {
                    if let Some(value) = read_u64(payload, next) {
                        *field = value;
                    }
                    next += 8;
                }
            }
            return;
        }
        at = end;
    }
}

/// The records of `extra` that a copy carries, as stored.
fn carried(extra: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(extra.len());
    let mut at = 0usize;
    while let (Some(id), Some(len)) = (read_u16(extra, at), read_u16(extra, at + 2)) {
        let end = at + 4 + usize::from(len);
        let Some(record) = extra.get(at..end) else {
            break;
        };
        if !DROPPED_EXTRA.contains(&id) {
            out.extend_from_slice(record);
        }
        at = end;
    }
    out
}

/// The extra field of the local header, and the offset its data starts at.
fn read_local_header<R: Read + Seek>(
    reader: &mut R,
    record: &StoredRecord,
) -> VfsResult<(Vec<u8>, u64)> {
    let bad_header = || VfsError::corrupt("a local header is not where the central directory says");
    let fixed_end = record
        .offset
        .checked_add(LOCAL_FIXED)
        .ok_or_else(bad_header)?;
    if fixed_end > record.directory_offset {
        return Err(bad_header());
    }
    reader.seek(SeekFrom::Start(record.offset))?;
    let mut fixed = [0u8; 30];
    reader.read_exact(&mut fixed).map_err(|_| bad_header())?;
    if read_u32(&fixed, 0) != Some(LOCAL_SIGNATURE) {
        return Err(bad_header());
    }
    let name_len = usize::from(read_u16(&fixed, 26).ok_or_else(bad_header)?);
    let extra_len = usize::from(read_u16(&fixed, 28).ok_or_else(bad_header)?);
    let data_start = fixed_end
        .checked_add(u64::try_from(name_len).map_err(|_| bad_header())?)
        .and_then(|start| start.checked_add(u64::try_from(extra_len).ok()?))
        .ok_or_else(bad_header)?;
    let data_end = data_start
        .checked_add(record.header.compressed)
        .ok_or_else(bad_header)?;
    if data_end > record.directory_offset {
        return Err(VfsError::corrupt(
            "a record's local header or data runs into the central directory",
        ));
    }

    let mut name = vec![0u8; name_len];
    reader.read_exact(&mut name).map_err(|_| bad_header())?;
    if name != record.name {
        return Err(VfsError::corrupt(
            "a local name differs from the central directory name",
        ));
    }
    let mut extra = vec![0u8; extra_len];
    reader
        .read_exact(&mut extra)
        .map_err(|_| VfsError::corrupt("a local header ends before its extra field"))?;
    Ok((extra, data_start))
}

/// A 32-bit header field for `value`, saturated when a zip64 field states it.
fn narrow(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// A 16-bit length field.
fn length16(len: usize, what: &str) -> VfsResult<u16> {
    u16::try_from(len)
        .map_err(|_| VfsError::unsupported(format!("a zip record whose {what} passes 64 KiB")))
}

/// One record of the rewritten container as a reader has to find it.
#[derive(Debug)]
pub(super) struct Expected {
    name: String,
    crc: u32,
    compressed: u64,
    size: u64,
    offset: u64,
}

/// One record as its central directory record states it.
struct Listed {
    header: Header,
    name: Vec<u8>,
    extra: Vec<u8>,
    comment: Vec<u8>,
    offset: u64,
}

/// A container written record by record and closed by its central
/// directory.
pub(super) struct Output<W: Write> {
    writer: W,
    at: u64,
    listed: Vec<Listed>,
}

impl<W: Write> Output<W> {
    /// An empty container that starts at the first byte of `writer`.
    pub(super) fn new(writer: W) -> Self {
        Self {
            writer,
            at: 0,
            listed: Vec::new(),
        }
    }

    fn write(&mut self, bytes: &[u8]) -> VfsResult<()> {
        self.writer.write_all(bytes)?;
        self.at = self.at.saturating_add(bytes.len() as u64);
        Ok(())
    }

    /// Copy `record` of the container `source` reads, stored under `name`
    /// with `comment`.
    ///
    /// The data is copied byte for byte. Both headers are written again with
    /// the fields of the record, the extra fields the record carries in each
    /// header, and the sizes and the checksum in the local header, so no
    /// data descriptor follows the data.
    ///
    /// # Errors
    /// Returns [`VfsError::Corrupt`] when the local header or the data is not
    /// where the central directory says, [`VfsError::Unsupported`] when a
    /// name or an extra field passes its length field, [`VfsError::Io`] when
    /// a read or a write fails, and [`VfsError::Cancelled`] when `cancel` is
    /// raised.
    pub(super) fn copy<R: Read + Seek>(
        &mut self,
        source: &mut R,
        record: &StoredRecord,
        name: &str,
        comment: &str,
        cancel: &Cancel,
    ) -> VfsResult<()> {
        let (local_extra, data_start) = read_local_header(source, record)?;
        let offset = self.at;
        let mut header = record.header;
        let utf8 = if name.is_ascii() && comment.is_ascii() {
            0
        } else {
            FLAG_UTF8
        };
        header.flags = (header.flags & !(FLAG_DESCRIPTOR | FLAG_UTF8)) | utf8;
        if header.size >= SATURATED || header.compressed >= SATURATED || offset >= SATURATED {
            header.version_needed = header.version_needed.max(VERSION_ZIP64);
        }
        let local = local_header_bytes(&header, name.as_bytes(), &carried(&local_extra))?;
        self.write(&local)?;
        source.seek(SeekFrom::Start(data_start))?;
        self.copy_data(source, header.compressed, cancel)?;
        self.listed.push(Listed {
            header,
            name: name.as_bytes().to_vec(),
            extra: carried(&record.extra),
            comment: comment.as_bytes().to_vec(),
            offset,
        });
        Ok(())
    }

    fn copy_data<R: Read>(
        &mut self,
        source: &mut R,
        length: u64,
        cancel: &Cancel,
    ) -> VfsResult<()> {
        let mut left = length;
        let mut buffer = vec![0u8; COPY_CHUNK];
        while left > 0 {
            cancel.check()?;
            let want = usize::try_from(left).map_or(COPY_CHUNK, |left| left.min(COPY_CHUNK));
            let read = source.read(&mut buffer[..want])?;
            if read == 0 {
                return Err(VfsError::corrupt(
                    "a record's data ends before the size its header states",
                ));
            }
            self.write(&buffer[..read])?;
            left -= read as u64;
        }
        Ok(())
    }

    /// Write the central directory and the end records after the records,
    /// and hand back the writer with what a reader has to find.
    ///
    /// # Errors
    /// Returns [`VfsError::Unsupported`] when a name, an extra field or the
    /// comment passes its length field, and [`VfsError::Io`] when a write
    /// fails.
    pub(super) fn finish(mut self, comment: &[u8]) -> VfsResult<(W, Vec<Expected>)> {
        let directory_start = self.at;
        let listed = std::mem::take(&mut self.listed);
        let mut expected = Vec::with_capacity(listed.len());
        for record in &listed {
            let bytes = central_header_bytes(record)?;
            self.write(&bytes)?;
            expected.push(Expected {
                name: String::from_utf8_lossy(&record.name).into_owned(),
                crc: record.header.crc,
                compressed: record.header.compressed,
                size: record.header.size,
                offset: record.offset,
            });
        }
        let directory_size = self.at - directory_start;
        let end = end_records(
            listed.len() as u64,
            directory_start,
            directory_size,
            comment,
        )?;
        self.write(&end)?;
        self.writer.flush()?;
        Ok((self.writer, expected))
    }
}

/// The local header of a record: its fixed fields, `name`, and a zip64
/// field with both sizes when either needs one, then `carried`.
fn local_header_bytes(header: &Header, name: &[u8], carried: &[u8]) -> VfsResult<Vec<u8>> {
    let large = header.size >= SATURATED || header.compressed >= SATURATED;
    let mut extra = Vec::with_capacity(20 + carried.len());
    if large {
        extra.extend_from_slice(&EXTRA_ZIP64.to_le_bytes());
        extra.extend_from_slice(&16u16.to_le_bytes());
        extra.extend_from_slice(&header.size.to_le_bytes());
        extra.extend_from_slice(&header.compressed.to_le_bytes());
    }
    extra.extend_from_slice(carried);
    let (compressed, size) = if large {
        (u32::MAX, u32::MAX)
    } else {
        (narrow(header.compressed), narrow(header.size))
    };
    let mut out = Vec::with_capacity(30 + name.len() + extra.len());
    out.extend_from_slice(&LOCAL_SIGNATURE.to_le_bytes());
    out.extend_from_slice(&header.version_needed.to_le_bytes());
    out.extend_from_slice(&header.flags.to_le_bytes());
    out.extend_from_slice(&header.method.to_le_bytes());
    out.extend_from_slice(&header.time.to_le_bytes());
    out.extend_from_slice(&header.date.to_le_bytes());
    out.extend_from_slice(&header.crc.to_le_bytes());
    out.extend_from_slice(&compressed.to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&length16(name.len(), "name")?.to_le_bytes());
    out.extend_from_slice(&length16(extra.len(), "local extra field")?.to_le_bytes());
    out.extend_from_slice(name);
    out.extend_from_slice(&extra);
    Ok(out)
}

/// The central directory record of `record`, with a zip64 field that states
/// each size or offset its own field cannot hold, in the order the format
/// fixes: size, compressed size, offset.
fn central_header_bytes(record: &Listed) -> VfsResult<Vec<u8>> {
    let header = &record.header;
    let mut zip64 = Vec::new();
    for value in [header.size, header.compressed, record.offset] {
        if value >= SATURATED {
            zip64.extend_from_slice(&value.to_le_bytes());
        }
    }
    let mut extra = Vec::with_capacity(4 + zip64.len() + record.extra.len());
    if !zip64.is_empty() {
        extra.extend_from_slice(&EXTRA_ZIP64.to_le_bytes());
        extra.extend_from_slice(&length16(zip64.len(), "zip64 field")?.to_le_bytes());
        extra.extend_from_slice(&zip64);
    }
    extra.extend_from_slice(&record.extra);
    let mut out = Vec::with_capacity(46 + record.name.len() + extra.len() + record.comment.len());
    out.extend_from_slice(&CENTRAL_SIGNATURE.to_le_bytes());
    out.extend_from_slice(&header.version_made_by.to_le_bytes());
    out.extend_from_slice(&header.version_needed.to_le_bytes());
    out.extend_from_slice(&header.flags.to_le_bytes());
    out.extend_from_slice(&header.method.to_le_bytes());
    out.extend_from_slice(&header.time.to_le_bytes());
    out.extend_from_slice(&header.date.to_le_bytes());
    out.extend_from_slice(&header.crc.to_le_bytes());
    out.extend_from_slice(&narrow(header.compressed).to_le_bytes());
    out.extend_from_slice(&narrow(header.size).to_le_bytes());
    out.extend_from_slice(&length16(record.name.len(), "name")?.to_le_bytes());
    out.extend_from_slice(&length16(extra.len(), "central extra field")?.to_le_bytes());
    out.extend_from_slice(&length16(record.comment.len(), "comment")?.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&header.internal.to_le_bytes());
    out.extend_from_slice(&header.external.to_le_bytes());
    out.extend_from_slice(&narrow(record.offset).to_le_bytes());
    out.extend_from_slice(&record.name);
    out.extend_from_slice(&extra);
    out.extend_from_slice(&record.comment);
    Ok(out)
}

/// The end records of a directory of `count` records, `size` bytes long,
/// that starts at `start`: a zip64 end record and its locator where a value
/// passes its field, then the end record with the archive comment.
fn end_records(count: u64, start: u64, size: u64, comment: &[u8]) -> VfsResult<Vec<u8>> {
    let mut out = Vec::new();
    if count >= SATURATED_COUNT || start >= SATURATED || size >= SATURATED {
        let record_at = start + size;
        out.extend_from_slice(&ZIP64_END_SIGNATURE.to_le_bytes());
        out.extend_from_slice(&44u64.to_le_bytes());
        out.extend_from_slice(&VERSION_ZIP64.to_le_bytes());
        out.extend_from_slice(&VERSION_ZIP64.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&start.to_le_bytes());
        out.extend_from_slice(&ZIP64_LOCATOR_SIGNATURE.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&record_at.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
    }
    let count16 = u16::try_from(count).unwrap_or(u16::MAX);
    out.extend_from_slice(&END_SIGNATURE.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&count16.to_le_bytes());
    out.extend_from_slice(&count16.to_le_bytes());
    out.extend_from_slice(&narrow(size).to_le_bytes());
    out.extend_from_slice(&narrow(start).to_le_bytes());
    out.extend_from_slice(&length16(comment.len(), "archive comment")?.to_le_bytes());
    out.extend_from_slice(comment);
    Ok(out)
}

/// Read the container at `path` back through the zip reader and fail unless
/// it holds exactly `expected`, in order.
///
/// # Errors
/// Returns [`VfsError::Corrupt`] when the reader does not find what was
/// written, and [`VfsError::Io`] when the file cannot be opened.
pub(super) fn verify(path: &Path, expected: &[Expected]) -> VfsResult<()> {
    let differs = |detail: String| {
        VfsError::corrupt(format!(
            "the rewritten container does not read back as written: {detail}"
        ))
    };
    let file = std::fs::File::open(path)?;
    let mut archive = zip::ZipArchive::new(io::BufReader::new(file))
        .map_err(|error| differs(error.to_string()))?;
    if archive.len() != expected.len() {
        return Err(differs(format!(
            "{} records where {} were written",
            archive.len(),
            expected.len()
        )));
    }
    for (index, want) in expected.iter().enumerate() {
        let file = archive
            .by_index_raw(index)
            .map_err(|error| differs(error.to_string()))?;
        let same = file.name() == want.name
            && file.crc32() == want.crc
            && file.compressed_size() == want.compressed
            && file.size() == want.size
            && file.header_start() == want.offset;
        if !same {
            return Err(differs(format!("the record {}", want.name)));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    fn header(size: u64, compressed: u64) -> Header {
        Header {
            version_made_by: 0x0314,
            version_needed: 20,
            flags: 0,
            method: 8,
            time: 0x6000,
            date: 0x58CF,
            crc: 0x1234_5678,
            compressed,
            size,
            internal: 1,
            external: 0x81A4_0000,
        }
    }

    #[test]
    fn a_size_past_the_field_goes_to_a_zip64_field_in_both_headers() {
        let big = 5 * 1024 * 1024 * 1024_u64;
        let local = local_header_bytes(&header(big, 1_000), b"a.bin", &[]).unwrap();
        assert_eq!(read_u32(&local, 18), Some(u32::MAX));
        assert_eq!(read_u32(&local, 22), Some(u32::MAX));
        let extra = &local[35..];
        assert_eq!(read_u16(extra, 0), Some(EXTRA_ZIP64));
        assert_eq!(read_u16(extra, 2), Some(16));
        assert_eq!(read_u64(extra, 4), Some(big));
        assert_eq!(read_u64(extra, 12), Some(1_000));

        let listed = Listed {
            header: header(big, 1_000),
            name: b"a.bin".to_vec(),
            extra: Vec::new(),
            comment: Vec::new(),
            offset: big + 7,
        };
        let central = central_header_bytes(&listed).unwrap();
        assert_eq!(read_u32(&central, 20), Some(1_000));
        assert_eq!(read_u32(&central, 24), Some(u32::MAX));
        assert_eq!(read_u32(&central, 42), Some(u32::MAX));
        let extra = &central[51..];
        assert_eq!(read_u16(extra, 2), Some(16));
        assert_eq!(read_u64(extra, 4), Some(big));
        assert_eq!(read_u64(extra, 12), Some(big + 7));

        let (mut size, mut compressed, mut offset) = (SATURATED, 1_000, SATURATED);
        resolve_zip64(extra, &mut size, &mut compressed, &mut offset);
        assert_eq!((size, compressed, offset), (big, 1_000, big + 7));
    }

    #[test]
    fn a_directory_past_the_count_field_ends_with_zip64_records() {
        let end = end_records(70_000, 100, 200, b"note").unwrap();
        assert_eq!(read_u32(&end, 0), Some(ZIP64_END_SIGNATURE));
        assert_eq!(read_u64(&end, 24), Some(70_000));
        assert_eq!(read_u64(&end, 40), Some(200));
        assert_eq!(read_u64(&end, 48), Some(100));
        assert_eq!(read_u32(&end, 56), Some(ZIP64_LOCATOR_SIGNATURE));
        assert_eq!(read_u64(&end, 64), Some(300));
        assert_eq!(read_u32(&end, 76), Some(END_SIGNATURE));
        assert_eq!(read_u16(&end, 84), Some(u16::MAX));
        assert_eq!(&end[end.len() - 4..], b"note");

        let small = end_records(3, 100, 200, b"").unwrap();
        assert_eq!(read_u32(&small, 0), Some(END_SIGNATURE));
        assert_eq!(small.len(), 22);
    }

    #[test]
    fn a_copy_drops_the_fields_tied_to_the_old_record_only() {
        let mut extra = Vec::new();
        for (id, payload) in [
            (0x0001u16, vec![0u8; 16]),
            (0x5455, vec![1, 2, 3, 4, 5]),
            (0x7075, vec![1, 0, 0, 0, 0, b'n']),
            (0x000A, vec![0u8; 32]),
            (0xa11e, vec![0u8; 2]),
            (0x7875, vec![1, 4, 0, 0, 0, 0, 4, 0, 0, 0, 0]),
        ] {
            extra.extend_from_slice(&id.to_le_bytes());
            extra.extend_from_slice(&u16::try_from(payload.len()).unwrap().to_le_bytes());
            extra.extend_from_slice(&payload);
        }
        let kept = carried(&extra);
        let mut ids = Vec::new();
        let mut at = 0;
        while let (Some(id), Some(len)) = (read_u16(&kept, at), read_u16(&kept, at + 2)) {
            ids.push(id);
            at += 4 + usize::from(len);
        }
        assert_eq!(ids, [0x5455, 0x000A, 0x7875]);
        assert_eq!(at, kept.len());
    }
}
