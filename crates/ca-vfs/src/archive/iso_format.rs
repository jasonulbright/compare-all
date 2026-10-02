//! ISO 9660 disc images, read only, with Rock Ridge and Joliet names.
//!
//! Name sources are tried in this order: Rock Ridge on the primary volume,
//! then a Joliet supplementary volume, then the plain ISO 9660 names. A disc
//! that carries only UDF is refused.
//!
//! Every count and offset comes from the image and is checked against the
//! image length before it is used. Directories are walked with a queue, never
//! recursively, and a directory extent is visited once, so a loop of
//! directory records cannot hold the walk.

use std::collections::{BTreeSet, VecDeque};
use std::io::{Read, Seek, SeekFrom};

use super::span::{backing_len, read_exact_or_corrupt, Span, SpanIndex};
use super::{civil_to_unix, unix_to_system_time, ArchiveBacking};
use crate::cancel::Cancel;
use crate::entry::{TimeFidelity, VfsAttributes, VfsEntry, VfsLinkKind};
use crate::error::{VfsError, VfsResult};
use crate::path::VfsPath;
use crate::stored::{refuse, refused_by_parent, stored_child};
use crate::tree::RawEntry;

/// Sector size of the volume descriptor area, whatever the logical block size.
const SECTOR: u64 = 2048;
/// Offset of the first volume descriptor.
const DESCRIPTORS_START: u64 = 16 * SECTOR;
/// Most volume descriptors read before the terminator must have appeared.
const MAX_DESCRIPTORS: u64 = 64;
/// Most bytes one directory extent may occupy.
const MAX_DIRECTORY_BYTES: u64 = 64 * 1024 * 1024;
/// Most directory records the walk accepts across the whole image.
const MAX_RECORDS: usize = 4_000_000;
/// Most continuation areas followed for one directory record.
const MAX_CONTINUATIONS: usize = 16;
/// Most extents joined into one file.
const MAX_EXTENTS: usize = 4096;
/// Most bytes a Rock Ridge name may reach after continuation.
const MAX_NAME_BYTES: usize = 4096;

/// Directory record flag bits.
const FLAG_HIDDEN: u8 = 0x01;
const FLAG_DIRECTORY: u8 = 0x02;
const FLAG_MULTI_EXTENT: u8 = 0x80;

/// True when the bytes at the first volume descriptor name ISO 9660 or open a
/// UDF volume recognition sequence.
///
/// A UDF image is claimed here so that [`list`] refuses it by name rather
/// than the caller reporting no format at all.
///
/// # Errors
/// Returns [`VfsError::Io`] when the reader cannot seek or read.
pub(crate) fn is_iso<R: Read + Seek>(reader: &mut R) -> VfsResult<bool> {
    let mut id = [0u8; 6];
    let end = reader.seek(SeekFrom::End(0))?;
    if end < DESCRIPTORS_START + SECTOR {
        reader.seek(SeekFrom::Start(0))?;
        return Ok(false);
    }
    reader.seek(SeekFrom::Start(DESCRIPTORS_START))?;
    let found = reader.read_exact(&mut id).is_ok()
        && matches!(
            id.get(1..6),
            Some(b"CD001" | b"BEA01" | b"NSR02" | b"NSR03")
        );
    reader.seek(SeekFrom::Start(0))?;
    Ok(found)
}

/// How names are read out of directory records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Naming {
    RockRidge { skip: usize },
    Joliet,
    Plain,
}

/// One volume to walk.
#[derive(Debug, Clone, Copy)]
struct Volume {
    root_extent: u64,
    root_size: u64,
    block: u64,
    naming: Naming,
}

/// One directory record, decoded.
#[derive(Debug, Default)]
struct Record {
    extent: u64,
    size: u64,
    flags: u8,
    interleaved: bool,
    name: Vec<u8>,
    modified: Option<std::time::SystemTime>,
    system_use: Vec<u8>,
}

/// What the Rock Ridge entries of one record say.
#[derive(Debug, Default)]
struct RockRidge {
    name: Option<String>,
    mode: Option<u32>,
    uid: Option<u32>,
    gid: Option<u32>,
    symlink: bool,
    relocated: bool,
    child_link: Option<u64>,
}

/// Build the listing and the index of every file's extents.
///
/// One walk holds the queue, the visited set and the multi-extent join, which
/// share state record by record.
/// # Errors
/// Returns [`VfsError::Unsupported`] for an image with UDF alone, and
/// [`VfsError::Corrupt`] when the descriptors do not parse.
#[allow(clippy::too_many_lines)]
pub(crate) fn list(
    backing: &ArchiveBacking,
    cancel: &Cancel,
) -> VfsResult<(SpanIndex, Vec<RawEntry>)> {
    let len = backing_len(backing)?;
    let mut reader = backing.reader()?;
    let volume = choose_volume(&mut reader, len)?;

    let mut index = SpanIndex::default();
    index.sources.push(backing.clone());
    let mut out: Vec<RawEntry> = Vec::new();
    let mut visited: BTreeSet<u64> = BTreeSet::new();
    // Each directory to walk carries its stored path when the rules refuse
    // that path, because everything under it is refused as well.
    let mut queue: VecDeque<(u64, u64, VfsPath, Option<String>)> = VecDeque::new();
    queue.push_back((volume.root_extent, volume.root_size, VfsPath::root(), None));
    let mut records = 0usize;

    while let Some((extent, size, dir, refused_dir)) = queue.pop_front() {
        cancel.check()?;
        if !visited.insert(extent) {
            continue;
        }
        let bytes = read_directory(&mut reader, len, extent, size, volume.block)?;
        let mut pending: Option<(VfsEntry, Span)> = None;
        for record in DirectoryRecords::new(&bytes, volume.block) {
            records += 1;
            if records > MAX_RECORDS {
                return Err(VfsError::corrupt("disc image names too many entries"));
            }
            let Some(record) = record else {
                break;
            };
            if record.name == [0] || record.name == [1] {
                continue;
            }
            let rock = match volume.naming {
                Naming::RockRidge { skip } => {
                    rock_ridge(&mut reader, len, volume.block, &record.system_use, skip)
                }
                _ => RockRidge::default(),
            };
            if rock.relocated {
                continue;
            }
            let name = match (&rock.name, volume.naming) {
                (Some(name), _) => name.clone(),
                (None, Naming::Joliet) => joliet_name(&record.name),
                (None, _) => plain_name(&record.name),
            };
            let Some((path, refusal)) = stored_child(&dir, &name).into_row() else {
                continue;
            };
            let refusal = match refused_dir.as_deref() {
                Some(parent) => Some(refused_by_parent(&format!("{parent}/{name}"))),
                None => refusal,
            };
            let is_dir = record.flags & FLAG_DIRECTORY != 0 || rock.child_link.is_some();
            let offset = record.extent.saturating_mul(volume.block);

            if let Some((mut entry, mut span)) = pending.take() {
                if entry.path == path && !is_dir && span.extents.len() < MAX_EXTENTS {
                    span.extents.push((offset, record.size));
                    entry.size = entry.size.saturating_add(record.size);
                    if record.flags & FLAG_MULTI_EXTENT != 0 {
                        pending = Some((entry, span));
                    } else {
                        out.push(file_row(entry, span, &mut index));
                    }
                    continue;
                }
                out.push(file_row(entry, span, &mut index));
            }

            if is_dir {
                let target = rock.child_link.unwrap_or(record.extent);
                let target_size = if rock.child_link.is_some() {
                    relocated_size(&mut reader, len, target, volume.block)?
                } else {
                    record.size
                };
                let refused_here = refusal.is_some().then(|| match refused_dir.as_deref() {
                    Some(parent) => format!("{parent}/{name}"),
                    None if dir.is_root() => name.clone(),
                    None => format!("{dir}/{name}"),
                });
                queue.push_back((target, target_size, path.clone(), refused_here));
                let mut entry = VfsEntry::directory(path);
                entry.modified = record.modified;
                entry.attributes = Some(attributes(&record, &rock, &name));
                if let Some(reason) = refusal {
                    refuse(&mut entry, &reason);
                }
                out.push(RawEntry {
                    entry,
                    locator: None,
                });
                continue;
            }

            let mut entry = VfsEntry::file(path, record.size);
            entry.modified = record.modified;
            entry.time_fidelity = TimeFidelity::Utc;
            entry.attributes = Some(attributes(&record, &rock, &name));
            if rock.symlink {
                entry.link = Some(VfsLinkKind::FileLink);
            }
            if let Some(reason) = refusal {
                refuse(&mut entry, &reason);
            }
            if record.interleaved {
                refuse(&mut entry, "interleaved file sections are not read");
            }
            let span = Span::single(0, offset, record.size);
            if record.flags & FLAG_MULTI_EXTENT != 0 {
                pending = Some((entry, span));
            } else {
                out.push(file_row(entry, span, &mut index));
            }
        }
        if let Some((entry, span)) = pending.take() {
            out.push(file_row(entry, span, &mut index));
        }
    }
    Ok((index, out))
}

/// The row for one file, with its extents recorded unless it is refused.
fn file_row(entry: VfsEntry, span: Span, index: &mut SpanIndex) -> RawEntry {
    let locator = (!entry.refused).then(|| index.push(span));
    RawEntry { entry, locator }
}

/// Pick the volume and naming to walk.
fn choose_volume<R: Read + Seek>(reader: &mut R, len: u64) -> VfsResult<Volume> {
    let mut primary: Option<Volume> = None;
    let mut joliet: Option<Volume> = None;
    let mut udf = false;
    for position in 0..MAX_DESCRIPTORS {
        let offset = DESCRIPTORS_START + position * SECTOR;
        if offset + SECTOR > len {
            break;
        }
        let mut sector = [0u8; 2048];
        reader.seek(SeekFrom::Start(offset))?;
        read_exact_or_corrupt(reader, &mut sector, "volume descriptor")?;
        let id = sector.get(1..6).unwrap_or_default();
        if id == b"BEA01" || id == b"NSR02" || id == b"NSR03" || id == b"TEA01" {
            udf = true;
            continue;
        }
        if id != b"CD001" {
            break;
        }
        let kind = sector.first().copied().unwrap_or(255);
        match kind {
            1 if primary.is_none() => primary = volume_of(&sector, Naming::Plain),
            2 if joliet.is_none() && is_joliet(&sector) => {
                joliet = volume_of(&sector, Naming::Joliet);
            }
            255 => break,
            _ => {}
        }
    }

    let Some(primary) = primary else {
        if udf {
            return Err(VfsError::unsupported(
                "disc images that carry only UDF are not read",
            ));
        }
        return Err(VfsError::corrupt("disc image has no primary volume"));
    };
    if let Some(skip) = rock_ridge_skip(reader, len, &primary)? {
        return Ok(Volume {
            naming: Naming::RockRidge { skip },
            ..primary
        });
    }
    Ok(joliet.unwrap_or(primary))
}

/// The volume a descriptor describes, where its fields are usable.
fn volume_of(sector: &[u8], naming: Naming) -> Option<Volume> {
    let block = u64::from(u16::from_le_bytes([*sector.get(128)?, *sector.get(129)?]));
    if !matches!(block, 512 | 1024 | 2048) {
        return None;
    }
    let root = sector.get(156..190)?;
    Some(Volume {
        root_extent: u64::from(le32(root, 2)?),
        root_size: u64::from(le32(root, 10)?),
        block,
        naming,
    })
}

/// True when a supplementary descriptor carries a Joliet escape sequence.
fn is_joliet(sector: &[u8]) -> bool {
    let escape = sector.get(88..91).unwrap_or_default();
    escape == b"%/@" || escape == b"%/C" || escape == b"%/E"
}

/// Where Rock Ridge entries start in each record, when the volume has them.
///
/// The root directory's own record carries the "SP" entry that announces the
/// extension and states how many bytes to skip in every later record.
fn rock_ridge_skip<R: Read + Seek>(
    reader: &mut R,
    len: u64,
    volume: &Volume,
) -> VfsResult<Option<usize>> {
    let bytes = read_directory(
        reader,
        len,
        volume.root_extent,
        volume.root_size.min(volume.block),
        volume.block,
    )?;
    let Some(Some(first)) = DirectoryRecords::new(&bytes, volume.block).next() else {
        return Ok(None);
    };
    let area = first.system_use.as_slice();
    if area.get(0..2) == Some(b"SP".as_slice()) && area.get(4..6) == Some([0xBE, 0xEF].as_slice()) {
        return Ok(Some(usize::from(area.get(6).copied().unwrap_or(0))));
    }
    Ok(None)
}

/// The size of a directory a child link points to.
///
/// A child link names only the extent, so the size comes from the `.` record
/// that opens it. A first block with no readable `.` record gives one block.
fn relocated_size<R: Read + Seek>(
    reader: &mut R,
    len: u64,
    extent: u64,
    block: u64,
) -> VfsResult<u64> {
    let first = read_directory(reader, len, extent, block, block)?;
    let size = match DirectoryRecords::new(&first, block).next() {
        Some(Some(record)) if record.name == [0] && record.size > 0 => record.size,
        _ => block,
    };
    Ok(size)
}

/// Read one directory extent, bounded by the image length.
fn read_directory<R: Read + Seek>(
    reader: &mut R,
    len: u64,
    extent: u64,
    size: u64,
    block: u64,
) -> VfsResult<Vec<u8>> {
    let offset = extent
        .checked_mul(block)
        .ok_or_else(|| VfsError::corrupt("directory extent past the image"))?;
    if size > MAX_DIRECTORY_BYTES || offset.saturating_add(size) > len {
        return Err(VfsError::corrupt("directory extent past the image"));
    }
    let mut bytes = vec![0u8; usize::try_from(size).unwrap_or(0)];
    reader.seek(SeekFrom::Start(offset))?;
    read_exact_or_corrupt(reader, &mut bytes, "directory")?;
    Ok(bytes)
}

/// Directory records of one extent. A record never crosses a block boundary,
/// so a zero length byte means the rest of the block is padding.
struct DirectoryRecords<'a> {
    bytes: &'a [u8],
    position: usize,
    block: usize,
}

impl<'a> DirectoryRecords<'a> {
    fn new(bytes: &'a [u8], block: u64) -> Self {
        Self {
            bytes,
            position: 0,
            block: usize::try_from(block).unwrap_or(2048).max(1),
        }
    }
}

impl Iterator for DirectoryRecords<'_> {
    /// `Some(None)` marks a malformed record, which ends the walk of the
    /// extent.
    type Item = Option<Record>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let length = usize::from(*self.bytes.get(self.position)?);
            if length == 0 {
                let next = (self.position / self.block + 1) * self.block;
                if next >= self.bytes.len() {
                    return None;
                }
                self.position = next;
                continue;
            }
            let raw = self.bytes.get(self.position..self.position + length);
            self.position += length;
            return Some(raw.and_then(parse_record));
        }
    }
}

fn parse_record(raw: &[u8]) -> Option<Record> {
    if raw.len() < 34 {
        return None;
    }
    let extended = u64::from(*raw.get(1)?);
    let extent = u64::from(le32(raw, 2)?).saturating_add(extended);
    let size = u64::from(le32(raw, 10)?);
    let flags = *raw.get(25)?;
    let interleaved =
        raw.get(26).copied().unwrap_or(0) != 0 || raw.get(27).copied().unwrap_or(0) != 0;
    let name_len = usize::from(*raw.get(32)?);
    let name = raw.get(33..33 + name_len)?.to_vec();
    let mut system_start = 33 + name_len;
    if name_len % 2 == 0 {
        system_start += 1;
    }
    let system_use = raw.get(system_start..).unwrap_or_default().to_vec();
    Some(Record {
        extent,
        size,
        flags,
        interleaved,
        name,
        modified: recording_time(raw.get(18..25)?),
        system_use,
    })
}

/// The seven byte recording time, as a UTC instant.
fn recording_time(field: &[u8]) -> Option<std::time::SystemTime> {
    let [year, month, day, hour, minute, second, zone] = <[u8; 7]>::try_from(field).ok()?;
    if month == 0 {
        return None;
    }
    let local = civil_to_unix(
        1900 + i64::from(year),
        u32::from(month),
        u32::from(day),
        u32::from(hour),
        u32::from(minute),
        u32::from(second),
    )?;
    let offset = i64::from(i8::from_ne_bytes([zone])) * 15 * 60;
    unix_to_system_time(local - offset)
}

fn le32(bytes: &[u8], at: usize) -> Option<u32> {
    let field = bytes.get(at..at + 4)?;
    Some(u32::from_le_bytes(<[u8; 4]>::try_from(field).ok()?))
}

/// A plain ISO 9660 name with its version suffix and empty extension removed.
fn plain_name(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    let stem = text.split(';').next().unwrap_or_default();
    let stem = stem.strip_suffix('.').unwrap_or(stem);
    stem.to_owned()
}

/// A Joliet name, stored as UCS-2 big endian.
fn joliet_name(raw: &[u8]) -> String {
    let units: Vec<u16> = raw
        .chunks_exact(2)
        .map(|pair| {
            u16::from_be_bytes([
                pair.first().copied().unwrap_or(0),
                pair.get(1).copied().unwrap_or(0),
            ])
        })
        .collect();
    let text = String::from_utf16_lossy(&units);
    let stem = text.split(';').next().unwrap_or_default();
    stem.to_owned()
}

/// Read the Rock Ridge entries of one record, following continuation areas.
fn rock_ridge<R: Read + Seek>(
    reader: &mut R,
    len: u64,
    block: u64,
    system_use: &[u8],
    skip: usize,
) -> RockRidge {
    let mut out = RockRidge::default();
    let mut name: Vec<u8> = Vec::new();
    let mut name_seen = false;
    let mut areas: VecDeque<Vec<u8>> = VecDeque::new();
    areas.push_back(system_use.get(skip..).unwrap_or_default().to_vec());
    let mut followed = 0usize;

    while let Some(area) = areas.pop_front() {
        let mut position = 0usize;
        while let Some(header) = area.get(position..position + 4) {
            let signature = header.get(0..2).unwrap_or_default();
            let entry_len = usize::from(header.get(2).copied().unwrap_or(0));
            if entry_len < 4 {
                break;
            }
            let Some(entry) = area.get(position..position + entry_len) else {
                break;
            };
            let body = entry.get(4..).unwrap_or_default();
            match signature {
                b"NM" => {
                    let flags = body.first().copied().unwrap_or(0);
                    if flags & 0x02 != 0 {
                        name_seen = true;
                        name = b".".to_vec();
                    } else if flags & 0x04 != 0 {
                        name_seen = true;
                        name = b"..".to_vec();
                    } else if name.len() < MAX_NAME_BYTES {
                        name_seen = true;
                        name.extend_from_slice(body.get(1..).unwrap_or_default());
                    }
                }
                b"PX" => {
                    out.mode = le32(body, 0);
                    out.uid = le32(body, 16);
                    out.gid = le32(body, 24);
                }
                b"SL" => out.symlink = true,
                b"RE" => out.relocated = true,
                b"CL" => out.child_link = le32(body, 0).map(u64::from),
                b"CE" if followed < MAX_CONTINUATIONS => {
                    followed += 1;
                    let (Some(extent), Some(offset), Some(size)) =
                        (le32(body, 0), le32(body, 8), le32(body, 16))
                    else {
                        break;
                    };
                    let start = u64::from(extent)
                        .saturating_mul(block)
                        .saturating_add(u64::from(offset));
                    let size = u64::from(size).min(SECTOR);
                    if start.saturating_add(size) <= len {
                        let mut continued = vec![0u8; usize::try_from(size).unwrap_or(0)];
                        if reader.seek(SeekFrom::Start(start)).is_ok()
                            && reader.read_exact(&mut continued).is_ok()
                        {
                            areas.push_back(continued);
                        }
                    }
                }
                b"ST" => break,
                _ => {}
            }
            position += entry_len;
        }
    }

    if name_seen && !name.is_empty() && name != b"." && name != b".." {
        name.truncate(MAX_NAME_BYTES);
        out.name = Some(String::from_utf8_lossy(&name).into_owned());
    }
    out
}

/// Attributes from the record flags and, where present, the POSIX mode.
fn attributes(record: &Record, rock: &RockRidge, name: &str) -> VfsAttributes {
    let hidden = record.flags & FLAG_HIDDEN != 0 || (rock.mode.is_some() && name.starts_with('.'));
    VfsAttributes {
        read_only: rock.mode.is_none_or(|mode| mode & 0o200 == 0),
        hidden,
        system: false,
        archive: false,
        windows_bits: None,
        unix_mode: rock.mode.map(|mode| mode & 0o7777),
        uid: rock.uid,
        gid: rock.gid,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    #[test]
    fn plain_names_lose_their_version_and_empty_extension() {
        assert_eq!(plain_name(b"README.TXT;1"), "README.TXT");
        assert_eq!(plain_name(b"MAKEFILE.;1"), "MAKEFILE");
        assert_eq!(plain_name(b"DIR"), "DIR");
    }

    #[test]
    fn joliet_names_decode_from_big_endian() {
        let raw: Vec<u8> = "Été.txt;1"
            .encode_utf16()
            .flat_map(u16::to_be_bytes)
            .collect();
        assert_eq!(joliet_name(&raw), "Été.txt");
    }

    #[test]
    fn recording_time_applies_the_zone() {
        let at = recording_time(&[124, 1, 2, 3, 4, 5, 4]).unwrap();
        let expected = unix_to_system_time(civil_to_unix(2024, 1, 2, 2, 4, 5).unwrap()).unwrap();
        assert_eq!(at, expected);
    }

    #[test]
    fn a_truncated_record_ends_the_extent() {
        let bytes = [40u8, 0, 0];
        let mut records = DirectoryRecords::new(&bytes, 2048);
        assert!(matches!(records.next(), Some(None)));
    }
}
