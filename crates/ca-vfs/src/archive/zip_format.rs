//! Zip, and the formats that are zip containers under another name.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use zip::result::ZipError;

use super::{civil_to_unix, unix_to_system_time, zip_copy, ArchiveBacking, BackingReader};
use crate::cancel::Cancel;
use crate::entry::{EntryKind, TimeFidelity, VfsAttributes, VfsEntry, VfsLinkKind};
use crate::error::{LimitKind, VfsError, VfsResult};
use crate::fs::OpenFile;
use crate::limits::{carry, materialize, uncarried, uncarry, Budget, Limits};
use crate::path::VfsPath;
use crate::stored::{refuse, stored_path};
use crate::tree::RawEntry;

/// Signature of the end of central directory record.
const END_SIGNATURE: u32 = 0x0605_4b50;
/// Signature of one central directory record.
const CENTRAL_SIGNATURE: u32 = 0x0201_4b50;
/// Signature of the zip64 end of central directory record.
const ZIP64_END_SIGNATURE: u32 = 0x0606_4b50;
/// Signature of the zip64 end of central directory locator.
const ZIP64_LOCATOR_SIGNATURE: u32 = 0x0706_4b50;
/// Extra field that carries the zip64 sizes and offset.
const EXTRA_ZIP64: u16 = 0x0001;
/// Bytes of the file end searched for the end record: the record itself and
/// the longest comment it can carry.
const END_WINDOW: u64 = 66 * 1024;
/// Maximum central-directory data parsed into in-memory entry records.
const MAX_DIRECTORY_BYTES: u64 = 32 * 1024 * 1024;
/// Maximum entries a ZIP central directory may ask the reader to index.
const MAX_DIRECTORY_ENTRIES: u64 = 100_000;

/// Why a record the reader cannot resolve by name is refused.
const SHADOWED: &str = "the container stores more than one entry under this name; only the last \
                        can be read through this reader";

/// What the container file looked like when it was opened.
///
/// A rewrite replaces the whole file, so it must not run against a file some
/// other process has replaced since the listing was built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Stamp {
    len: u64,
    modified: Option<std::time::SystemTime>,
    /// Hash of the trailing region that holds the central directory.
    tail_crc: u32,
}

/// Bytes of the tail hashed into [`Stamp::tail_crc`].
const STAMP_TAIL_BYTES: u64 = 64 * 1024;

impl Stamp {
    /// Record the state of a container that lives in a file.
    pub(crate) fn of(backing: &ArchiveBacking) -> VfsResult<Option<Self>> {
        let Some(path) = backing.path() else {
            return Ok(None);
        };
        let metadata = std::fs::metadata(path)?;
        let len = metadata.len();
        let mut reader = backing.reader()?;
        let tail = len.min(STAMP_TAIL_BYTES);
        reader.seek(SeekFrom::Start(len - tail))?;
        let mut bytes = vec![0u8; usize::try_from(tail).unwrap_or(0)];
        reader.read_exact(&mut bytes)?;
        Ok(Some(Self {
            len,
            modified: metadata.modified().ok(),
            tail_crc: crc32fast::hash(&bytes),
        }))
    }

    /// Fail unless the file still matches what was recorded.
    fn verify(&self, backing: &ArchiveBacking) -> VfsResult<()> {
        let current = Self::of(backing)?;
        match current {
            Some(current) if current == *self => Ok(()),
            Some(_) => Err(VfsError::ContainerChanged {
                detail: "size, modification time or central directory differs".to_owned(),
            }),
            None => Ok(()),
        }
    }
}

/// Build the listing.
///
/// Every record the central directory names reaches the listing. A record
/// whose name is not a usable relative path is listed under a name that is,
/// and refused, so it can never be opened, extracted or written through. A
/// record the reader cannot resolve, because a later record holds its name,
/// is listed and refused as well. Every other row carries the position of its
/// record as its locator.
///
/// # Errors
/// Returns [`VfsError::Corrupt`] when the central directory does not parse,
/// and [`VfsError::LimitExceeded`] when the directory is larger than the
/// listing allowance.
pub(crate) fn list(
    backing: &ArchiveBacking,
    _password: Option<&str>,
    allowance: &Budget,
) -> VfsResult<Vec<RawEntry>> {
    let mut reader = backing.reader()?;
    let span = checked_directory_span(&mut reader)?;
    allowance.take(span.size)?;
    let mut archive = zip::ZipArchive::new(reader).map_err(map_zip_open)?;
    let records = central_records(backing, span)?;
    let repeated = repeated_names(records.as_deref(), archive.len());

    let mut out: Vec<RawEntry> = Vec::new();
    // Names that differ only by case or by combining marks land on one file
    // when the container is extracted on Windows, so both are flagged.
    let mut folded: BTreeMap<String, usize> = BTreeMap::new();
    // The reader's own spelling of each name stored more than once.
    let mut spelled: HashMap<Vec<u8>, String> = HashMap::new();

    for position in 0..archive.len() {
        // Metadata only: the raw accessor never needs the password.
        let row = match archive.by_index_raw(position) {
            Ok(file) => {
                if repeated.contains(file.name_raw()) {
                    spelled.insert(file.name_raw().to_vec(), file.name().to_owned());
                }
                Ok(record_row(&file, position))
            }
            Err(error) => Err(error.to_string()),
        };
        let row = match row {
            Ok(row) => row,
            Err(error) => unreadable_row(archive.name_for_index(position), &error),
        };
        let Some(mut row) = row else {
            continue;
        };
        if !row.entry.refused {
            let key = collision_key(row.entry.path.as_str());
            if let Some(first) = folded.get(&key).copied() {
                let text = out
                    .get(first)
                    .map(|other| clash_text(&other.entry, &row.entry))
                    .unwrap_or_default();
                if let Some(other) = out.get_mut(first) {
                    note(&mut other.entry, &text);
                }
                note(&mut row.entry, &text);
            } else {
                folded.insert(key, out.len());
            }
        }
        out.push(row);
    }

    if let Some(records) = &records {
        out.extend(shadowed_duplicates(records, &repeated, &spelled));
    }
    Ok(out)
}

/// The listing row for the record at `position`, or `None` for a directory
/// record that names the root.
fn record_row<R: Read>(file: &zip::read::ZipFile<'_, R>, position: usize) -> Option<RawEntry> {
    let kind = if file.is_dir() {
        EntryKind::Directory
    } else {
        EntryKind::File
    };
    let (path, refusal) = stored_path(file.name(), kind == EntryKind::Directory).into_row()?;
    let mode = file.unix_mode();
    let attributes = VfsAttributes {
        read_only: mode.is_some_and(|mode| mode & 0o200 == 0),
        hidden: path.name().is_some_and(|name| name.starts_with('.')),
        system: false,
        archive: false,
        windows_bits: None,
        unix_mode: mode,
        uid: None,
        gid: None,
    };
    let (modified, fidelity) = read_timestamps(file);
    // A zero CRC is what an entry whose sizes live in a data descriptor
    // carries in the central directory of some writers, so it says nothing
    // about the content and is not offered as a checksum.
    let crc32 = match (kind, file.crc32()) {
        (EntryKind::File, 0) | (EntryKind::Directory, _) => None,
        (EntryKind::File, value) => Some(value),
    };
    let name = path.name().unwrap_or_default().to_owned();
    let mut entry = VfsEntry {
        path,
        name,
        kind,
        size: if kind == EntryKind::Directory {
            0
        } else {
            file.size()
        },
        size_is_exact: kind == EntryKind::File,
        modified,
        time_fidelity: fidelity,
        created: None,
        attributes: Some(attributes),
        crc32,
        link: file.is_symlink().then_some(VfsLinkKind::FileLink),
        version_info: None,
        error: None,
        refused: false,
    };
    let locator = match refusal {
        Some(reason) => {
            refuse(&mut entry, &reason);
            None
        }
        None => u64::try_from(position).ok(),
    };
    Some(RawEntry { entry, locator })
}

/// The row for a record the directory names but whose header cannot be read.
fn unreadable_row(name: Option<&str>, error: &str) -> Option<RawEntry> {
    let name = name?;
    let is_dir = name.ends_with('/') || name.ends_with('\\');
    let (path, refusal) = stored_path(name, is_dir).into_row()?;
    let mut entry = if is_dir {
        VfsEntry::directory(path)
    } else {
        VfsEntry::file(path, 0)
    };
    entry.size_is_exact = false;
    if let Some(reason) = refusal {
        refuse(&mut entry, &reason);
    }
    refuse(
        &mut entry,
        &format!("the entry header cannot be read: {error}"),
    );
    Some(RawEntry {
        entry,
        locator: None,
    })
}

/// What the central directory records for one entry.
///
/// The reader this crate builds on keys its index by name and so exposes one
/// entry per name. The central directory is read again here, on its own terms,
/// to find out whether the container names an entry more than once: a listing
/// that quietly showed one of them would hide content the container holds.
#[derive(Debug)]
struct CentralRecord {
    name: Vec<u8>,
    size: u64,
    crc: u32,
    is_dir: bool,
}

/// Every record the central directory lists, in the order it lists them.
///
/// Returns `Ok(None)` for a directory this cannot read on its own terms, in
/// which case no duplicate reporting happens and the reader's own view
/// stands. The records are read one at a time, so nothing is reserved from a
/// size or a count the container states.
///
/// # Errors
/// Returns [`VfsError::LimitExceeded`] when the directory is larger than the
/// bounded central-directory span, or when an archive listing allowance was
/// exhausted before this function was called.
fn central_records(
    backing: &ArchiveBacking,
    span: DirectorySpan,
) -> VfsResult<Option<Vec<CentralRecord>>> {
    let mut reader = backing.reader()?;
    reader.seek(SeekFrom::Start(span.offset))?;
    let mut directory = io::BufReader::new(reader.take(span.size));
    let mut out = Vec::new();
    while (out.len() as u64) < span.total {
        let Some(record) = read_central_record(&mut directory) else {
            return Ok(None);
        };
        out.push(record);
    }
    Ok(Some(out))
}

/// One central directory record, or `None` when the bytes are not one.
fn read_central_record<R: Read>(reader: &mut R) -> Option<CentralRecord> {
    let mut fixed = [0u8; 46];
    reader.read_exact(&mut fixed).ok()?;
    if read_u32(&fixed, 0)? != CENTRAL_SIGNATURE {
        return None;
    }
    let crc = read_u32(&fixed, 16)?;
    let size = read_u32(&fixed, 24)?;
    let mut name = vec![0u8; usize::from(read_u16(&fixed, 28)?)];
    reader.read_exact(&mut name).ok()?;
    let mut extra = vec![0u8; usize::from(read_u16(&fixed, 30)?)];
    reader.read_exact(&mut extra).ok()?;
    let comment = u64::from(read_u16(&fixed, 32)?);
    let skipped = io::copy(&mut reader.by_ref().take(comment), &mut io::sink()).ok()?;
    if skipped != comment {
        return None;
    }
    let size = if size == u32::MAX {
        zip64_size(&extra).unwrap_or(u64::from(size))
    } else {
        u64::from(size)
    };
    let is_dir = matches!(name.last(), Some(b'/' | b'\\'));
    Some(CentralRecord {
        name,
        size,
        crc,
        is_dir,
    })
}

/// The expanded size a zip64 extra field states, which comes first in it
/// whenever the record's own size field is saturated.
fn zip64_size(extra: &[u8]) -> Option<u64> {
    let mut found = None;
    for_each_extra(extra, |id, payload| {
        if id != EXTRA_ZIP64 {
            return false;
        }
        found = read_u64(payload, 0);
        true
    });
    found
}

/// Where the end records say the central directory lies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DirectorySpan {
    /// Records the directory holds.
    pub(super) total: u64,
    /// Offset of the first record.
    pub(super) offset: u64,
    /// Bytes the records occupy.
    pub(super) size: u64,
}

/// The directory the end records describe, or `None` when they describe one
/// that does not lie inside the file.
///
/// A zip64 locator in front of the end record takes precedence over the end
/// record's own fields. Neither is evidence, so the stated offset and size
/// are checked against the file length before anything is read from them.
pub(super) fn directory_span(reader: &mut BackingReader) -> Option<DirectorySpan> {
    let len = reader.seek(SeekFrom::End(0)).ok()?;
    let window = len.min(END_WINDOW);
    let window_start = len - window;
    reader.seek(SeekFrom::Start(window_start)).ok()?;
    let mut tail = vec![0u8; usize::try_from(window).ok()?];
    reader.read_exact(&mut tail).ok()?;

    let eocd = find_signature(&tail, END_SIGNATURE)?;
    let end = tail.get(eocd..)?;
    let total = u64::from(read_u16(end, 10)?);
    let size = u64::from(read_u32(end, 12)?);
    let offset = u64::from(read_u32(end, 16)?);
    let end_at = window_start.checked_add(u64::try_from(eocd).ok()?)?;
    let span = match zip64_span(reader, end_at) {
        Some(span) => span,
        None if size == u64::from(u32::MAX) || offset == u64::from(u32::MAX) => return None,
        None => DirectorySpan {
            total,
            offset,
            size,
        },
    };
    let inside = span
        .offset
        .checked_add(span.size)
        .is_some_and(|stop| stop <= len);
    inside.then_some(span)
}

/// Refuse directory declarations that cannot be safely indexed by the zip
/// reader before passing it the same already-checked stream.
fn checked_directory_span(reader: &mut BackingReader) -> VfsResult<DirectorySpan> {
    check_fallback_directory_limits(reader)?;
    let span = directory_span(reader).ok_or_else(|| {
        VfsError::corrupt("zip central directory is not bounded by its end record")
    })?;
    if span.size > MAX_DIRECTORY_BYTES {
        return Err(VfsError::LimitExceeded {
            kind: LimitKind::ArchiveSize,
            limit: MAX_DIRECTORY_BYTES,
        });
    }
    if span.total > MAX_DIRECTORY_ENTRIES {
        return Err(VfsError::LimitExceeded {
            kind: LimitKind::ArchiveEntries,
            limit: MAX_DIRECTORY_ENTRIES,
        });
    }
    Ok(span)
}

/// A zip reader may reject a later end record and fall back to an earlier one.
/// Check every structurally plausible candidate in the legal tail window
/// before handing the stream to it, so a small trailing record cannot hide an
/// oversized Zip64 directory from the limits below.
fn check_fallback_directory_limits(reader: &mut BackingReader) -> VfsResult<()> {
    let len = reader.seek(SeekFrom::End(0))?;
    let window = len.min(END_WINDOW);
    let window_start = len - window;
    reader.seek(SeekFrom::Start(window_start))?;
    let mut tail = vec![0u8; usize::try_from(window).unwrap_or(0)];
    reader.read_exact(&mut tail)?;

    let signature = END_SIGNATURE.to_le_bytes();
    let Some(last_start) = tail.len().checked_sub(22) else {
        return Ok(());
    };
    for at in (0..=last_start).rev() {
        if tail.get(at..at + 4) != Some(signature.as_slice()) {
            continue;
        }
        let Some(end) = tail.get(at..) else {
            continue;
        };
        let Some(comment_bytes) = read_u16(end, 20).map(u64::from) else {
            continue;
        };
        let end_at = window_start.saturating_add(u64::try_from(at).unwrap_or(u64::MAX));
        if end_at.saturating_add(22).saturating_add(comment_bytes) > len {
            continue;
        }
        let Some(total) = read_u16(end, 10).map(u64::from) else {
            continue;
        };
        let Some(size) = read_u32(end, 12).map(u64::from) else {
            continue;
        };
        let Some(offset) = read_u32(end, 16).map(u64::from) else {
            continue;
        };
        let span = match zip64_span(reader, end_at) {
            Some(span) => span,
            None if total == 0xFFFF
                || size == u64::from(u32::MAX)
                || offset == u64::from(u32::MAX) =>
            {
                continue;
            }
            None => DirectorySpan {
                total,
                offset,
                size,
            },
        };
        if !plausible_directory_start(reader, span, end_at)? {
            continue;
        }
        if span.size > MAX_DIRECTORY_BYTES {
            return Err(VfsError::LimitExceeded {
                kind: LimitKind::ArchiveSize,
                limit: MAX_DIRECTORY_BYTES,
            });
        }
        if span.total > MAX_DIRECTORY_ENTRIES {
            return Err(VfsError::LimitExceeded {
                kind: LimitKind::ArchiveEntries,
                limit: MAX_DIRECTORY_ENTRIES,
            });
        }
    }
    Ok(())
}

fn plausible_directory_start(
    reader: &mut BackingReader,
    span: DirectorySpan,
    end_at: u64,
) -> VfsResult<bool> {
    if span.total == 0 {
        return Ok(span.size == 0);
    }
    let expected = CENTRAL_SIGNATURE.to_le_bytes();
    let mut starts = [None, None];
    starts[0] = Some(span.offset);
    starts[1] = end_at.checked_sub(span.size);
    for start in starts.into_iter().flatten() {
        if start >= end_at || start.checked_add(4).is_none_or(|stop| stop > end_at) {
            continue;
        }
        reader.seek(SeekFrom::Start(start))?;
        let mut signature = [0; 4];
        if reader.read_exact(&mut signature).is_ok() && signature == expected {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The directory a zip64 end record describes, when a zip64 locator sits
/// directly in front of the end record at `end_at`.
fn zip64_span(reader: &mut BackingReader, end_at: u64) -> Option<DirectorySpan> {
    let locator_at = end_at.checked_sub(20)?;
    reader.seek(SeekFrom::Start(locator_at)).ok()?;
    let mut locator = [0u8; 20];
    reader.read_exact(&mut locator).ok()?;
    if read_u32(&locator, 0)? != ZIP64_LOCATOR_SIGNATURE {
        return None;
    }
    let record_at = read_u64(&locator, 8)?;
    if record_at.checked_add(56)? > locator_at {
        return None;
    }
    reader.seek(SeekFrom::Start(record_at)).ok()?;
    let mut record = [0u8; 56];
    reader.read_exact(&mut record).ok()?;
    if read_u32(&record, 0)? != ZIP64_END_SIGNATURE {
        return None;
    }
    Some(DirectorySpan {
        total: read_u64(&record, 32)?,
        size: read_u64(&record, 40)?,
        offset: read_u64(&record, 48)?,
    })
}

/// Offset of the last record carrying `signature`.
fn find_signature(bytes: &[u8], signature: u32) -> Option<usize> {
    let wanted = signature.to_le_bytes();
    (0..bytes.len().saturating_sub(3))
        .rev()
        .find(|at| bytes.get(*at..*at + 4) == Some(&wanted))
}

pub(super) fn read_u16(bytes: &[u8], at: usize) -> Option<u16> {
    let slice = bytes.get(at..at + 2)?;
    Some(u16::from_le_bytes([slice[0], slice[1]]))
}

pub(super) fn read_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let slice = bytes.get(at..at + 4)?;
    Some(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

pub(super) fn read_u64(bytes: &[u8], at: usize) -> Option<u64> {
    let slice = bytes.get(at..at + 8)?;
    let mut word = [0u8; 8];
    word.copy_from_slice(slice);
    Some(u64::from_le_bytes(word))
}

/// Raw names the central directory stores more than once, when it holds more
/// records than the reader exposes.
fn repeated_names(records: Option<&[CentralRecord]>, exposed: usize) -> HashSet<Vec<u8>> {
    let Some(records) = records else {
        return HashSet::new();
    };
    if records.len() <= exposed {
        return HashSet::new();
    }
    let mut seen: HashSet<&[u8]> = HashSet::new();
    let mut repeated = HashSet::new();
    for record in records {
        if !seen.insert(record.name.as_slice()) {
            repeated.insert(record.name.clone());
        }
    }
    repeated
}

/// Listing rows for the records of a repeated name that the reader does not
/// resolve to.
///
/// The reader resolves a name to the last record that carries it, so every
/// earlier record of that name cannot be opened through it. Each is listed
/// anyway, refused, because a comparison that showed one file where the
/// container holds two would be wrong about the container rather than merely
/// unable to read part of it. A repeated directory record holds no content
/// and lists nothing of its own.
fn shadowed_duplicates(
    records: &[CentralRecord],
    repeated: &HashSet<Vec<u8>>,
    spelled: &HashMap<Vec<u8>, String>,
) -> Vec<RawEntry> {
    let mut left: HashMap<&[u8], usize> = HashMap::new();
    for record in records {
        if repeated.contains(&record.name) {
            *left.entry(record.name.as_slice()).or_default() += 1;
        }
    }
    let mut out = Vec::new();
    for record in records {
        let Some(count) = left.get_mut(record.name.as_slice()) else {
            continue;
        };
        *count -= 1;
        if *count == 0 || record.is_dir {
            continue;
        }
        let name = spelled
            .get(&record.name)
            .cloned()
            .unwrap_or_else(|| String::from_utf8_lossy(&record.name).into_owned());
        let Some((path, refusal)) = stored_path(&name, false).into_row() else {
            continue;
        };
        let mut entry = VfsEntry::file(path, record.size);
        entry.crc32 = (record.crc != 0).then_some(record.crc);
        if let Some(reason) = refusal {
            refuse(&mut entry, &reason);
        }
        refuse(&mut entry, SHADOWED);
        out.push(RawEntry {
            entry,
            locator: None,
        });
    }
    out
}

/// What two records whose names share a collision key are told about each
/// other.
fn clash_text(first: &VfsEntry, second: &VfsEntry) -> String {
    let name = first.path.as_str();
    if first.path != second.path {
        return format!(
            "another entry in the container has a name that differs only by case or by character composition: {name}"
        );
    }
    if first.is_dir() == second.is_dir() {
        format!("another entry in the container has the same name: {name}")
    } else {
        format!("a file and a directory in the container have the same name: {name}")
    }
}

/// Append `text` to an entry's error field.
fn note(entry: &mut VfsEntry, text: &str) {
    entry.error = Some(match entry.error.take() {
        Some(existing) if existing.contains(text) => existing,
        Some(existing) => format!("{existing}; {text}"),
        None => text.to_owned(),
    });
}

/// A key two names share when a Windows extraction would merge them.
///
/// Case folding covers the common collision. Combining marks are dropped as
/// well, which catches a name stored in decomposed form beside the same name
/// in composed form without pulling in a full normalization table.
fn collision_key(name: &str) -> String {
    name.chars()
        .filter(|ch| !is_combining(*ch))
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_combining(ch: char) -> bool {
    matches!(u32::from(ch),
        0x0300..=0x036F
            | 0x1AB0..=0x1AFF
            | 0x1DC0..=0x1DFF
            | 0x20D0..=0x20FF
            | 0xFE20..=0xFE2F)
}

/// Extra field identifiers carrying a timestamp.
const EXTRA_NTFS: u16 = 0x000A;
const EXTRA_EXTENDED_TIMESTAMP: u16 = 0x5455;

/// The best modification time the entry carries, and how far it can be trusted.
///
/// A DOS stamp is a wall clock with no zone and a two-second tick, so it is
/// reported as such, read as UTC here and moved into the zone of the container
/// once the listing is built; the extra fields carry real UTC and are
/// preferred when present.
fn read_timestamps<R>(
    file: &zip::read::ZipFile<'_, R>,
) -> (Option<std::time::SystemTime>, TimeFidelity)
where
    R: Read,
{
    let extra = file.extra_data().unwrap_or_default();
    if let Some(time) = ntfs_mtime(extra).and_then(unix_to_system_time) {
        return (Some(time), TimeFidelity::Utc);
    }
    if let Some(time) = extended_mtime(extra).and_then(unix_to_system_time) {
        return (Some(time), TimeFidelity::Utc);
    }
    let dos = file.last_modified().and_then(|stamp| {
        civil_to_unix(
            i64::from(stamp.year()),
            u32::from(stamp.month()),
            u32::from(stamp.day()),
            u32::from(stamp.hour()),
            u32::from(stamp.minute()),
            u32::from(stamp.second()),
        )
        .and_then(unix_to_system_time)
    });
    (dos, TimeFidelity::LocalTwoSecond)
}

/// The instant `seconds` after the Unix epoch as the DOS stamp of a wall clock
/// `zone_offset_seconds` ahead of UTC, which is the zone a listing reads the
/// stamp back in.
///
/// A clock outside the range a DOS stamp holds gives the DOS epoch.
fn dos_stamp_of(seconds: i64, zone_offset_seconds: i32) -> zip::DateTime {
    let (year, month, day, hour, minute, second) = crate::remote::timestamp::unix_to_civil(
        seconds.saturating_add(i64::from(zone_offset_seconds)),
    );
    let field = |value: u32| u8::try_from(value).unwrap_or(0);
    u16::try_from(year)
        .ok()
        .and_then(|year| {
            zip::DateTime::from_date_and_time(
                year,
                field(month),
                field(day),
                field(hour),
                field(minute),
                field(second),
            )
            .ok()
        })
        .unwrap_or_default()
}

/// Walk the extra field, handing each record's id and payload to `visit`.
fn for_each_extra(extra: &[u8], mut visit: impl FnMut(u16, &[u8]) -> bool) {
    let mut at = 0usize;
    while at + 4 <= extra.len() {
        let id = u16::from_le_bytes([extra[at], extra[at + 1]]);
        let len = usize::from(u16::from_le_bytes([extra[at + 2], extra[at + 3]]));
        let start = at + 4;
        let Some(payload) = extra.get(start..start + len) else {
            return;
        };
        if visit(id, payload) {
            return;
        }
        at = start + len;
    }
}

/// Seconds since the Unix epoch from an NTFS extra field.
pub(super) fn ntfs_mtime(extra: &[u8]) -> Option<i64> {
    let mut found = None;
    for_each_extra(extra, |id, payload| {
        if id != EXTRA_NTFS || payload.len() < 12 {
            return false;
        }
        // Four reserved bytes, then tagged attributes; tag 1 holds three
        // Windows file times of eight bytes each.
        let mut at = 4usize;
        while at + 4 <= payload.len() {
            let tag = u16::from_le_bytes([payload[at], payload[at + 1]]);
            let size = usize::from(u16::from_le_bytes([payload[at + 2], payload[at + 3]]));
            let start = at + 4;
            let Some(body) = payload.get(start..start + size) else {
                return true;
            };
            if tag == 1 && body.len() >= 8 {
                let mut word = [0u8; 8];
                word.copy_from_slice(&body[..8]);
                found = windows_time_to_unix(u64::from_le_bytes(word));
                return true;
            }
            at = start + size;
        }
        false
    });
    found
}

/// Seconds since the Unix epoch from an extended timestamp extra field.
pub(super) fn extended_mtime(extra: &[u8]) -> Option<i64> {
    let mut found = None;
    for_each_extra(extra, |id, payload| {
        if id != EXTRA_EXTENDED_TIMESTAMP || payload.len() < 5 {
            return false;
        }
        // A flags byte, then the present stamps in order; bit 0 is modified.
        if payload[0] & 0x01 == 0 {
            return true;
        }
        let mut word = [0u8; 4];
        word.copy_from_slice(&payload[1..5]);
        found = Some(i64::from(i32::from_le_bytes(word)));
        true
    });
    found
}

/// Hundred-nanosecond ticks since 1601 turned into Unix seconds.
fn windows_time_to_unix(ticks: u64) -> Option<i64> {
    const TICKS_PER_SECOND: u64 = 10_000_000;
    const EPOCH_DIFFERENCE: i64 = 11_644_473_600;
    if ticks == 0 {
        return None;
    }
    i64::try_from(ticks / TICKS_PER_SECOND)
        .ok()?
        .checked_sub(EPOCH_DIFFERENCE)
}

/// Expand one entry, checking the content against the stored checksum.
///
/// # Errors
/// Returns [`VfsError::NeedsPassword`] or [`VfsError::WrongPassword`] for an
/// encrypted entry, [`VfsError::Unsupported`] for a compression method this
/// build does not carry, [`VfsError::ChecksumMismatch`] when the content does
/// not hash to what the container records, and [`VfsError::LimitExceeded`]
/// when expanding the entry passes a ceiling.
#[allow(
    clippy::too_many_arguments,
    reason = "one entry read needs the container, the index, the listing record, the credentials and the ceilings"
)]
pub(crate) fn open_entry(
    backing: &ArchiveBacking,
    locator: Option<u64>,
    path: &VfsPath,
    entry: &VfsEntry,
    password: Option<&str>,
    limits: &Limits,
    budget: &Budget,
    cancel: &Cancel,
) -> VfsResult<OpenFile> {
    let position = locator
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| VfsError::NotFound { path: path.clone() })?;
    let mut reader = backing.reader()?;
    checked_directory_span(&mut reader)?;
    let mut archive = zip::ZipArchive::new(reader).map_err(map_zip_open)?;

    let file = match password {
        Some(password) => archive.by_index_decrypt(position, password.as_bytes()),
        None => archive.by_index(position),
    }
    .map_err(|error| map_zip_entry(error, path))?;
    let stored = file.compressed_size();
    let verified = Verify {
        inner: file,
        hasher: crc32fast::Hasher::new(),
        expected: entry.crc32,
        path: path.clone(),
        done: false,
    };
    materialize(verified, stored, limits, budget, cancel)
}

/// A reader that checks the content against the container's checksum.
///
/// The check can only land at the end of the stream, so a caller that stops
/// early never sees it; that is the point at which the whole entry has been
/// read and a mismatch proves the container is wrong about its own content.
struct Verify<R> {
    inner: R,
    hasher: crc32fast::Hasher,
    expected: Option<u32>,
    path: VfsPath,
    done: bool,
}

impl<R: Read> Read for Verify<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = match self.inner.read(buf) {
            Ok(read) => read,
            // The decoder checks the checksum too, and reports a failure as an
            // ordinary read error. The content has already been hashed here,
            // so the disagreement is reported as what it is rather than
            // passed on as damage of an unknown kind.
            Err(error) => return Err(self.mismatch().unwrap_or(error)),
        };
        if read > 0 {
            self.hasher.update(buf.get(..read).unwrap_or_default());
            return Ok(read);
        }
        if !self.done {
            self.done = true;
            if let Some(error) = self.mismatch() {
                return Err(error);
            }
        }
        Ok(0)
    }
}

impl<R> Verify<R> {
    /// The typed error for a checksum that does not describe what was read,
    /// or `None` when there is nothing to disagree with.
    fn mismatch(&self) -> Option<io::Error> {
        let expected = self.expected?;
        let actual = self.hasher.clone().finalize();
        if actual == expected {
            return None;
        }
        Some(carry(VfsError::ChecksumMismatch {
            path: self.path.clone(),
            expected,
            actual,
        }))
    }
}

/// One change to apply while rewriting a zip.
#[derive(Debug)]
pub(crate) enum Mutation {
    /// Add a directory entry, and the directory entries above it.
    AddDirectory {
        /// Directory to add.
        path: VfsPath,
    },
    /// Create or replace a file.
    WriteFile {
        /// File to write.
        path: VfsPath,
        /// The position of the record the listing shows at `path`. The new
        /// content takes that record's place and its stored name, which
        /// differs from `path` for a file the listing moved aside for a
        /// directory.
        replaces: Option<usize>,
        /// Bytes to store.
        content: Vec<u8>,
        /// The modification time the record carries. `None` stamps it with
        /// the time of the rewrite.
        modified: Option<std::time::SystemTime>,
    },
    /// Remove an entry and everything under it.
    Delete {
        /// The records to remove.
        entry: Selected,
    },
    /// Move an entry and everything under it.
    Rename {
        /// The records to move.
        from: Selected,
        /// New path.
        to: VfsPath,
        /// The position of the record the listing shows at `to`. The moved
        /// record takes that record's place and its stored name, which
        /// differs from `to` for a file the listing moved aside for a
        /// directory.
        replaces: Option<usize>,
    },
}

/// The stored records a removal or a move acts on.
///
/// A file record and a directory can be stored under one name, and the
/// listing then shows the file under a suffixed name. An edit of either row
/// reaches that row's records only.
#[derive(Debug, Clone)]
pub(crate) enum Selected {
    /// The record at this position of the central directory, whatever name it
    /// is stored under.
    Record(usize),
    /// A directory: its own record and every record stored under it. A file
    /// record stored under the directory's own name is not part of it.
    Directory(VfsPath),
    /// Every record stored under this name or below it, for a name the
    /// listing does not hold.
    Stored(VfsPath),
}

impl Selected {
    /// True when the record at `position`, stored as `current`, is one this
    /// selection names.
    fn holds(&self, position: usize, current: &VfsPath, is_dir: bool) -> bool {
        match self {
            Self::Record(record) => *record == position,
            Self::Directory(path) => current.starts_with(path) && (is_dir || current != path),
            Self::Stored(path) => current.starts_with(path),
        }
    }

    /// The stored name a held record takes when the selection moves to `to`.
    fn moved(&self, current: &VfsPath, to: &VfsPath, is_dir: bool) -> VfsResult<String> {
        match self {
            Self::Record(_) => rename_name(current, current, to, is_dir),
            Self::Directory(from) | Self::Stored(from) => rename_name(current, from, to, is_dir),
        }
    }
}

/// Rewrite the whole container with `mutation` applied, then replace the
/// original in one rename.
///
/// A zip's central directory sits at the end of the file, so any change means
/// rewriting the container. Writing beside the original and renaming over it
/// means an interrupted write leaves the original intact. The rewrite is read
/// back through the zip reader before it replaces the original.
///
/// Every kept record keeps its stored bytes, compression method, checksum,
/// sizes, DOS stamp, attributes, comment and extra fields, the NTFS and the
/// extended timestamp field among them, so it reads back as the same instant
/// after any number of rewrites. A zip64 field is written again where the
/// sizes or the new position need one; the fields tied to the bytes of the
/// old record are left out (`zip_copy`). The archive comment is copied
/// because it belongs to the container rather than to any record. Record
/// order is the order the source lists them in. A record the batch adds
/// carries the time its write names, or the time of the rewrite, as a DOS
/// stamp in the zone `zone` seconds ahead of UTC and as an extended timestamp
/// field.
///
/// # Errors
/// Returns [`VfsError::ContainerChanged`] when the file no longer matches
/// `stamp`, [`VfsError::Unsupported`] when the container holds encrypted
/// entries, a name this crate will not write back, or one name stored more
/// than once, or when its central directory is not where its end record
/// says, [`VfsError::Io`] when the temporary file cannot be written or
/// renamed, and [`VfsError::Corrupt`] when the source does not parse, when a
/// record is not where its central directory record says, or when the
/// rewrite does not read back as it was written.
pub(crate) fn rewrite(
    backing: &ArchiveBacking,
    target: &Path,
    mutations: &[Mutation],
    _password: Option<&str>,
    stamp: Option<&Stamp>,
    zone: i32,
    cancel: &Cancel,
) -> VfsResult<()> {
    if let Some(stamp) = stamp {
        stamp.verify(backing)?;
    }

    let mut reader = backing.reader()?;
    checked_directory_span(&mut reader)?;
    let mut source = zip::ZipArchive::new(reader).map_err(map_zip_open)?;

    // Refused before anything is written: a rewrite stores new content with no
    // encryption, which would quietly weaken a container whose other entries
    // are encrypted.
    for position in 0..source.len() {
        let entry = source.by_index_raw(position).map_err(map_zip_open)?;
        if entry.encrypted() {
            return Err(VfsError::unsupported(
                "writing to a zip that holds encrypted entries",
            ));
        }
        let raw_name = entry.name().to_owned();
        if VfsPath::parse(&raw_name).is_err() {
            return Err(VfsError::unsupported(format!(
                "writing to a zip that holds the unusable entry name {raw_name:?}"
            )));
        }
    }

    // The reader resolves a repeated name to one record, so a rewrite that
    // copies what the reader exposes would drop the others.
    let stored = match zip_copy::stored_records(backing)? {
        Some(records) if records.len() > source.len() => {
            return Err(VfsError::unsupported(
                "writing to a zip that stores more than one entry under one name",
            ));
        }
        Some(records) if records.len() == source.len() => records,
        Some(_) | None => {
            return Err(VfsError::unsupported(
                "writing to a zip whose central directory does not lie where its end record says",
            ));
        }
    };

    if stored.iter().any(|record| !record.has_valid_utf8_fields()) {
        return Err(VfsError::unsupported(
            "writing to a zip that marks an invalid name or comment as UTF-8",
        ));
    }

    let comment = source.comment().to_vec();
    let mut replaced: HashMap<usize, String> = HashMap::new();
    let mut written = Written::default();
    let kept = kept_records(
        &mut source,
        &stored,
        mutations,
        &mut replaced,
        &mut written,
        cancel,
    )?;

    let directory = target.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(directory)?;
    let added = write_added(directory, mutations, &replaced, &mut written, zone, cancel)?;

    let temp = tempfile::NamedTempFile::new_in(directory)?;
    let (file, temp_path) = temp.into_parts();
    let mut output = zip_copy::Output::new(io::BufWriter::new(file));
    {
        let mut data = backing.reader()?;
        for kept in &kept {
            cancel.check()?;
            let record = stored
                .get(kept.position)
                .ok_or_else(|| VfsError::corrupt("zip entry missing from central directory"))?;
            output.copy(&mut data, record, &kept.name, &kept.comment, cancel)?;
        }
    }
    if let Some(added) = &added {
        copy_added(added, &mut output, cancel)?;
    }
    let (writer, expected) = output.finish(&comment)?;
    let file = writer
        .into_inner()
        .map_err(|error| VfsError::Io(error.into_error()))?;
    drop(file);
    zip_copy::verify(&temp_path, &expected)?;
    // Windows refuses to replace a file that is still open, and the source
    // archive holds the original open until it is dropped.
    drop(source);
    // The shared replacement keeps the attributes, access list and streams of
    // the container; a rename of the temporary over it would drop them.
    ca_io::replace_checked(
        target,
        |written: &mut dyn Write| -> VfsResult<()> {
            let mut verified = std::fs::File::open(&temp_path)?;
            io::copy(&mut verified, written)?;
            Ok(())
        },
        || stamp.map_or(Ok(()), |stamp| stamp.verify(backing)),
    )
}

/// One record a rewrite keeps.
struct Kept {
    /// Position of the record in the central directory of the source.
    position: usize,
    /// The name the record is stored under in the rewrite.
    name: String,
    /// The comment of the record.
    comment: String,
}

/// The records of `source` the batch keeps, each under the name it takes, in
/// the order the source lists them.
///
/// Each kept name is claimed in `written`, and the stored name of each
/// record a write replaces goes into `replaced`.
fn kept_records<R: Read + Seek>(
    source: &mut zip::ZipArchive<R>,
    stored: &[zip_copy::StoredRecord],
    mutations: &[Mutation],
    replaced: &mut HashMap<usize, String>,
    written: &mut Written,
    cancel: &Cancel,
) -> VfsResult<Vec<Kept>> {
    let overwritten = renamed_over_names(source, mutations)?;
    let mut kept = Vec::new();
    for (position, record) in stored.iter().enumerate() {
        cancel.check()?;
        let entry = source.by_index_raw(position).map_err(map_zip_open)?;
        if !record.agrees_with(&entry) {
            return Err(VfsError::unsupported(
                "writing to a zip whose central directory the reader reads differently",
            ));
        }
        let raw_name = entry.name().to_owned();
        let comment = entry.comment().to_owned();
        let parsed = VfsPath::parse(&raw_name).ok();
        let is_dir = entry.is_dir();
        drop(entry);

        let action = match parsed.as_ref() {
            Some(current) => {
                if replaces(mutations, position) {
                    replaced.insert(position, current.as_str().to_owned());
                }
                entry_action(mutations, position, current, is_dir, &overwritten)?
            }
            None => Action::Keep,
        };

        let name = match action {
            Action::Keep => raw_name,
            Action::Rename(name) => name,
            Action::Drop => continue,
        };
        written.claim(&name)?;
        kept.push(Kept {
            position,
            name,
            comment,
        });
    }
    Ok(kept)
}

/// The records `mutations` add, written by the zip writer into a scratch
/// container in `directory`, or `None` when the batch adds none.
///
/// The writer compresses the content and states the time fields of each
/// added record; the rewrite then copies the records as it copies a kept one.
fn write_added(
    directory: &Path,
    mutations: &[Mutation],
    replaced: &HashMap<usize, String>,
    written: &mut Written,
    zone: i32,
    cancel: &Cancel,
) -> VfsResult<Option<ArchiveBacking>> {
    let adds = mutations.iter().any(|mutation| match mutation {
        Mutation::AddDirectory { path } => !path.is_root(),
        Mutation::WriteFile { .. } => true,
        Mutation::Delete { .. } | Mutation::Rename { .. } => false,
    });
    if !adds {
        return Ok(None);
    }
    let scratch = tempfile::NamedTempFile::new_in(directory)?;
    let mut writer = zip::ZipWriter::new(scratch.as_file());
    append_records(&mut writer, mutations, replaced, written, zone, cancel)?;
    writer.finish().map_err(map_zip_open)?;
    Ok(Some(ArchiveBacking::Temp(std::sync::Arc::new(scratch))))
}

/// Copy every record of the scratch container `added` into `output`.
fn copy_added<W: Write>(
    added: &ArchiveBacking,
    output: &mut zip_copy::Output<W>,
    cancel: &Cancel,
) -> VfsResult<()> {
    let unreadable = || VfsError::corrupt("the records a batch adds do not read back");
    let stored = zip_copy::stored_records(added)?.ok_or_else(unreadable)?;
    let mut archive = zip::ZipArchive::new(added.reader()?).map_err(map_zip_open)?;
    if stored.len() != archive.len() {
        return Err(unreadable());
    }
    let mut data = added.reader()?;
    for (position, record) in stored.iter().enumerate() {
        cancel.check()?;
        let (name, comment) = {
            let entry = archive.by_index_raw(position).map_err(map_zip_open)?;
            if !record.agrees_with(&entry) {
                return Err(unreadable());
            }
            (entry.name().to_owned(), entry.comment().to_owned())
        };
        output.copy(&mut data, record, &name, &comment, cancel)?;
    }
    Ok(())
}

/// The stored name of each record a batched rename replaces, by position.
fn renamed_over_names<R: Read + Seek>(
    source: &mut zip::ZipArchive<R>,
    mutations: &[Mutation],
) -> VfsResult<HashMap<usize, String>> {
    let mut overwritten = HashMap::new();
    for mutation in mutations {
        if let Mutation::Rename {
            replaces: Some(position),
            ..
        } = mutation
        {
            let name = source
                .by_index_raw(*position)
                .map_err(map_zip_open)?
                .name()
                .to_owned();
            overwritten.insert(*position, name);
        }
    }
    Ok(overwritten)
}

/// Write the records the batch adds, after every kept record.
///
/// A file record carries the time its write names, and every other record
/// the time of the rewrite. A write that replaces a record takes that
/// record's stored name from `replaced`.
fn append_records<W: Write + Seek>(
    writer: &mut zip::ZipWriter<W>,
    mutations: &[Mutation],
    replaced: &HashMap<usize, String>,
    written: &mut Written,
    zone: i32,
    cancel: &Cancel,
) -> VfsResult<()> {
    let now = std::time::SystemTime::now();
    for mutation in mutations {
        cancel.check()?;
        match mutation {
            Mutation::AddDirectory { path } => {
                if !path.is_root() {
                    written.claim(&format!("{path}/"))?;
                    writer
                        .add_directory(path.as_str(), record_options(now, zone)?)
                        .map_err(map_zip_open)?;
                }
            }
            Mutation::WriteFile {
                path,
                replaces,
                content,
                modified,
            } => {
                let name = match replaces {
                    Some(position) => replaced
                        .get(position)
                        .map(String::as_str)
                        .ok_or_else(|| VfsError::NotFound { path: path.clone() })?,
                    None => path.as_str(),
                };
                written.claim(name)?;
                let options = record_options(modified.unwrap_or(now), zone)?;
                writer.start_file(name, options).map_err(map_zip_open)?;
                writer.write_all(content)?;
            }
            Mutation::Delete { .. } | Mutation::Rename { .. } => {}
        }
    }
    Ok(())
}

/// The options of one added record stamped with `modified`: a DOS stamp in
/// the zone `zone` seconds ahead of UTC and, where the instant fits the
/// field, an extended timestamp field in UTC.
///
/// A listing reads the extended field before the DOS stamp, so the record
/// reads back as the instant, at a whole second and in every zone. The DOS
/// stamp holds even seconds only.
fn record_options(
    modified: std::time::SystemTime,
    zone: i32,
) -> VfsResult<zip::write::FullFileOptions<'static>> {
    let seconds = crate::remote::timestamp::unix_seconds(modified);
    let mut options =
        zip::write::FullFileOptions::default().last_modified_time(dos_stamp_of(seconds, zone));
    if let Ok(stamp) = i32::try_from(seconds) {
        // One flags byte, bit 0 set: the field holds the modification time.
        let mut field = vec![0x01];
        field.extend_from_slice(&stamp.to_le_bytes());
        options
            .add_extra_data(EXTRA_EXTENDED_TIMESTAMP, field, false)
            .map_err(map_zip_open)?;
    }
    Ok(options)
}

/// The stored names a rewrite has written so far.
///
/// The zip writer refuses a second record under one name with an error that
/// reads as a corrupt container, so a clash is caught first and reported as
/// the name that is taken.
#[derive(Default)]
struct Written(HashSet<String>);

impl Written {
    /// Take `name` for one record.
    ///
    /// # Errors
    /// Returns [`VfsError::AlreadyExists`] when a record of the rewrite
    /// already holds `name`.
    fn claim(&mut self, name: &str) -> VfsResult<()> {
        if self.0.insert(name.to_owned()) {
            return Ok(());
        }
        Err(VfsError::AlreadyExists {
            path: VfsPath::parse(name).unwrap_or_default(),
        })
    }
}

/// True when a write of the batch replaces the record at `position`.
fn replaces(mutations: &[Mutation], position: usize) -> bool {
    mutations.iter().any(|mutation| {
        matches!(mutation, Mutation::WriteFile { replaces: Some(record), .. } if *record == position)
    })
}

/// What one batch of changes does to the record at `position`, stored as
/// `current`.
///
/// A removal wins over a replacement, and a replacement wins over a move,
/// because a name the batch removes must not reappear under another name. A
/// directory added under a file's name leaves the file record in place, and
/// a new file never takes the place of a directory record. A record a move
/// lands on is dropped last, so a record the batch also moves away keeps its
/// content. `overwritten` holds the stored name of each record a move lands
/// on, which the moved record takes.
///
/// # Errors
/// Returns [`VfsError::InvalidPath`] when a move gives the record a path past
/// the length or depth ceilings. Dropping the record instead would lose it.
fn entry_action(
    mutations: &[Mutation],
    position: usize,
    current: &VfsPath,
    is_dir: bool,
    overwritten: &HashMap<usize, String>,
) -> VfsResult<Action> {
    for mutation in mutations {
        if let Mutation::Delete { entry } = mutation {
            if entry.holds(position, current, is_dir) {
                return Ok(Action::Drop);
            }
        }
    }
    for mutation in mutations {
        let replaced = match mutation {
            Mutation::WriteFile {
                replaces: Some(record),
                ..
            } => *record == position,
            Mutation::WriteFile {
                path,
                replaces: None,
                ..
            } => !is_dir && current == path,
            Mutation::AddDirectory { path } => is_dir && current == path,
            Mutation::Delete { .. } | Mutation::Rename { .. } => false,
        };
        if replaced {
            return Ok(Action::Drop);
        }
    }
    for mutation in mutations {
        if let Mutation::Rename { from, to, replaces } = mutation {
            if from.holds(position, current, is_dir) {
                if let Some(name) = replaces.and_then(|record| overwritten.get(&record)) {
                    return Ok(Action::Rename(name.clone()));
                }
                return from.moved(current, to, is_dir).map(Action::Rename);
            }
        }
    }
    let landed_on = mutations.iter().any(|mutation| {
        matches!(mutation, Mutation::Rename { replaces: Some(record), .. } if *record == position)
    });
    if landed_on {
        return Ok(Action::Drop);
    }
    Ok(Action::Keep)
}

/// What to do with one entry while rewriting.
enum Action {
    Keep,
    Rename(String),
    Drop,
}

/// The stored name an entry takes when its subtree moves from `from` to `to`.
fn rename_name(current: &VfsPath, from: &VfsPath, to: &VfsPath, is_dir: bool) -> VfsResult<String> {
    let rest = current
        .strip_prefix(from)
        .ok_or_else(|| VfsError::NotFound {
            path: current.clone(),
        })?;
    let mut moved = to.clone();
    for part in rest.components() {
        moved = moved.join(part)?;
    }
    let mut name = moved.as_str().to_owned();
    if is_dir {
        name.push('/');
    }
    Ok(name)
}

/// Map an archive level failure.
fn map_zip_open(error: ZipError) -> VfsError {
    if let Some(carried) = uncarried(&error) {
        return carried;
    }
    match error {
        ZipError::Io(error) => uncarry(error),
        ZipError::FileNotFound => VfsError::corrupt("zip entry missing from central directory"),
        other => VfsError::corrupt(other.to_string()),
    }
}

/// Map a failure opening one entry, where a password may be the cause.
fn map_zip_entry(error: ZipError, path: &VfsPath) -> VfsError {
    if let Some(carried) = uncarried(&error) {
        return carried;
    }
    match error {
        ZipError::UnsupportedArchive(ZipError::PASSWORD_REQUIRED) => {
            VfsError::NeedsPassword { path: path.clone() }
        }
        ZipError::InvalidPassword => VfsError::WrongPassword { path: path.clone() },
        ZipError::UnsupportedArchive(detail) => VfsError::unsupported(detail),
        ZipError::CompressionMethodNotSupported(method) => {
            VfsError::unsupported(format!("zip compression method {method:?}"))
        }
        ZipError::FileNotFound => VfsError::NotFound { path: path.clone() },
        ZipError::Io(error) => uncarry(error),
        other => VfsError::corrupt(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    #[test]
    fn names_that_merge_on_extraction_share_a_key() {
        assert_eq!(collision_key("README.txt"), collision_key("readme.TXT"));
        // The same word composed and decomposed.
        assert_eq!(collision_key("cafe\u{0301}.txt"), collision_key("cafe.txt"));
        assert_ne!(collision_key("a.txt"), collision_key("b.txt"));
    }

    #[test]
    fn extended_timestamp_is_read_before_the_dos_stamp() {
        let extra = [0x55, 0x54, 0x05, 0x00, 0x01, 0x40, 0xE2, 0x01, 0x00];
        assert_eq!(extended_mtime(&extra), Some(123_456));
    }

    #[test]
    fn ntfs_times_convert_from_the_windows_epoch() {
        assert_eq!(windows_time_to_unix(116_444_736_000_000_000), Some(0));
        assert_eq!(windows_time_to_unix(0), None);
    }

    /// A zip holding one stored entry.
    fn one_entry_zip() -> Vec<u8> {
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        writer.start_file("a.txt", options).unwrap();
        writer.write_all(b"hello").unwrap();
        writer.finish().unwrap().into_inner()
    }

    fn classic_zip_with_entry_count(count: u16) -> Vec<u8> {
        let mut bytes = Vec::new();
        for index in 0..count {
            let name = format!("e{index}");
            let mut record = [0u8; 46];
            record[..4].copy_from_slice(&CENTRAL_SIGNATURE.to_le_bytes());
            record[4..6].copy_from_slice(&20u16.to_le_bytes());
            record[6..8].copy_from_slice(&20u16.to_le_bytes());
            record[28..30].copy_from_slice(&u16::try_from(name.len()).unwrap().to_le_bytes());
            bytes.extend_from_slice(&record);
            bytes.extend_from_slice(name.as_bytes());
        }
        let directory_size = u32::try_from(bytes.len()).unwrap();
        let mut end = [0u8; 22];
        end[..4].copy_from_slice(&END_SIGNATURE.to_le_bytes());
        end[8..10].copy_from_slice(&count.to_le_bytes());
        end[10..12].copy_from_slice(&count.to_le_bytes());
        end[12..16].copy_from_slice(&directory_size.to_le_bytes());
        bytes.extend_from_slice(&end);
        bytes
    }

    #[test]
    fn a_stated_directory_size_past_the_file_end_is_refused_before_any_read() {
        let mut bytes = one_entry_zip();
        let end = find_signature(&bytes, 0x0605_4b50).unwrap();
        bytes[end + 12..end + 16].copy_from_slice(&0xFFFF_FFF0u32.to_le_bytes());
        let len = bytes.len() as u64;
        let backing = ArchiveBacking::from_bytes(bytes);
        let mut reader = backing.reader().unwrap();
        let span = directory_span(&mut reader);
        assert!(
            span.is_none_or(|span| span
                .offset
                .checked_add(span.size)
                .is_some_and(|end| end <= len)),
            "{span:?} is read from a file of {len} bytes"
        );
    }

    #[test]
    fn an_oversized_zip_directory_is_refused_before_the_reader_parses_it() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        let size = MAX_DIRECTORY_BYTES + 1;
        let end_at = size;
        file.as_file().set_len(end_at + 22).unwrap();
        file.as_file().seek(SeekFrom::Start(end_at)).unwrap();
        let mut end = [0u8; 22];
        end[..4].copy_from_slice(&END_SIGNATURE.to_le_bytes());
        end[8..10].copy_from_slice(&1u16.to_le_bytes());
        end[10..12].copy_from_slice(&1u16.to_le_bytes());
        end[12..16].copy_from_slice(&u32::try_from(size).unwrap().to_le_bytes());
        end[16..20].copy_from_slice(&0u32.to_le_bytes());
        file.as_file_mut().write_all(&end).unwrap();

        let error = list(
            &ArchiveBacking::Path(file.path().to_path_buf()),
            None,
            &Budget::new(u64::MAX),
        )
        .unwrap_err();
        assert!(
            matches!(
                error,
                VfsError::LimitExceeded {
                    kind: LimitKind::ArchiveSize,
                    limit: MAX_DIRECTORY_BYTES
                }
            ),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn an_excessive_zip_entry_count_is_refused_before_the_reader_reserves_it() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        let directory_offset = 1_000_000u64;
        let zip64_end_offset = directory_offset;
        let locator_offset = zip64_end_offset + 56;
        let end_offset = locator_offset + 20;
        file.as_file().set_len(end_offset + 22).unwrap();

        file.as_file_mut()
            .seek(SeekFrom::Start(zip64_end_offset))
            .unwrap();
        let mut zip64_end = [0u8; 56];
        zip64_end[..4].copy_from_slice(&ZIP64_END_SIGNATURE.to_le_bytes());
        zip64_end[4..12].copy_from_slice(&44u64.to_le_bytes());
        zip64_end[32..40].copy_from_slice(&(MAX_DIRECTORY_ENTRIES + 1).to_le_bytes());
        zip64_end[40..48].copy_from_slice(&0u64.to_le_bytes());
        zip64_end[48..56].copy_from_slice(&directory_offset.to_le_bytes());
        file.as_file_mut().write_all(&zip64_end).unwrap();

        let mut locator = [0u8; 20];
        locator[..4].copy_from_slice(&ZIP64_LOCATOR_SIGNATURE.to_le_bytes());
        locator[8..16].copy_from_slice(&zip64_end_offset.to_le_bytes());
        locator[16..20].copy_from_slice(&1u32.to_le_bytes());
        file.as_file_mut().write_all(&locator).unwrap();

        let mut end = [0u8; 22];
        end[..4].copy_from_slice(&END_SIGNATURE.to_le_bytes());
        end[8..10].copy_from_slice(&u16::MAX.to_le_bytes());
        end[10..12].copy_from_slice(&u16::MAX.to_le_bytes());
        end[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        end[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        file.as_file_mut().write_all(&end).unwrap();

        let backing = ArchiveBacking::Path(file.path().to_path_buf());
        let error = list(&backing, None, &Budget::new(u64::MAX)).unwrap_err();
        assert!(
            matches!(
                error,
                VfsError::LimitExceeded {
                    kind: LimitKind::ArchiveEntries,
                    limit: MAX_DIRECTORY_ENTRIES
                }
            ),
            "unexpected error: {error}"
        );

        let path = VfsPath::parse("entry").unwrap();
        let entry = VfsEntry::file(path.clone(), 0);
        let error = open_entry(
            &backing,
            Some(0),
            &path,
            &entry,
            None,
            &Limits::default(),
            &Budget::new(u64::MAX),
            &Cancel::new(),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            VfsError::LimitExceeded {
                kind: LimitKind::ArchiveEntries,
                limit: MAX_DIRECTORY_ENTRIES
            }
        ));

        let output = tempfile::NamedTempFile::new().unwrap();
        let error =
            rewrite(&backing, output.path(), &[], None, None, 0, &Cancel::new()).unwrap_err();
        assert!(matches!(
            error,
            VfsError::LimitExceeded {
                kind: LimitKind::ArchiveEntries,
                limit: MAX_DIRECTORY_ENTRIES
            }
        ));
    }

    #[test]
    fn an_invalid_trailing_end_record_cannot_hide_an_oversized_zip64_directory() {
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for index in 0..=MAX_DIRECTORY_ENTRIES {
            writer
                .start_file(format!("entry-{index}"), options)
                .unwrap();
        }
        let mut bytes = writer.finish().unwrap().into_inner();

        let fake_end = u64::try_from(bytes.len()).unwrap();
        let mut end = [0u8; 22];
        end[..4].copy_from_slice(&END_SIGNATURE.to_le_bytes());
        end[8..10].copy_from_slice(&1u16.to_le_bytes());
        end[10..12].copy_from_slice(&1u16.to_le_bytes());
        end[16..20].copy_from_slice(&u32::try_from(fake_end).unwrap().to_le_bytes());
        bytes.extend_from_slice(&end);

        let archive = zip::ZipArchive::new(std::io::Cursor::new(bytes.clone())).unwrap();
        assert_eq!(
            archive.len(),
            usize::try_from(MAX_DIRECTORY_ENTRIES + 1).unwrap()
        );

        let backing = ArchiveBacking::from_bytes(bytes);
        let mut reader = backing.reader().unwrap();
        assert!(matches!(
            checked_directory_span(&mut reader),
            Err(VfsError::LimitExceeded {
                kind: LimitKind::ArchiveEntries,
                limit: MAX_DIRECTORY_ENTRIES
            })
        ));
    }

    #[test]
    fn a_classic_zip_with_exactly_65535_entries_is_not_treated_as_zip64() {
        let backing = ArchiveBacking::from_bytes(classic_zip_with_entry_count(u16::MAX));

        let rows = list(&backing, None, &Budget::new(u64::MAX)).unwrap();

        assert_eq!(rows.len(), usize::from(u16::MAX));
    }
}
