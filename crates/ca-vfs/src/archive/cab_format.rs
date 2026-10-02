//! Microsoft cabinets, read only.
//!
//! Entries of a folder are stored as one compressed stream, and a decoder
//! cannot start part way through one. The first read from a folder decodes
//! the whole folder once into a temporary file under the expansion ceilings;
//! every read from that folder then seeks into it.
//!
//! Stored, MSZIP and LZX folders are read. Quantum folders, and entries that
//! continue into another cabinet of a set, are listed with the reason they
//! cannot be read. A data block is reported damaged when it fails its
//! checksum or fails to expand to the size its header states.

use std::io::{self, Read, Seek, SeekFrom};
use std::sync::Mutex;

use super::span::{backing_len, decode_to_temp, range_reader, read_exact_or_corrupt};
use super::{civil_to_unix, unix_to_system_time, ArchiveBacking, BackingReader};
use crate::cancel::Cancel;
use crate::entry::{TimeFidelity, VfsAttributes, VfsEntry};
use crate::error::{VfsError, VfsResult};
use crate::fs::OpenFile;
use crate::limits::{carry, materialize, Budget, Limits};
use crate::path::VfsPath;
use crate::stored::{refuse, stored_path};
use crate::tree::RawEntry;

const FLAG_PREVIOUS: u16 = 0x0001;
const FLAG_NEXT: u16 = 0x0002;
const FLAG_RESERVE: u16 = 0x0004;
/// Largest expanded size one data block may state.
const MAX_BLOCK: usize = 32 * 1024;
/// Most bytes a stored name or a set member name may take.
const MAX_NAME: usize = 256;
/// Size of the dictionary an MSZIP block may refer back into.
const MSZIP_WINDOW: usize = 32 * 1024;

const ATTR_READ_ONLY: u16 = 0x01;
const ATTR_HIDDEN: u16 = 0x02;
const ATTR_SYSTEM: u16 = 0x04;
const ATTR_ARCHIVE: u16 = 0x20;
const ATTR_NAME_UTF8: u16 = 0x80;

/// How a folder's blocks are compressed.
#[derive(Debug, Clone)]
enum Method {
    Stored,
    MsZip,
    Lzx(lzxd::WindowSize),
    Refused(String),
}

impl Method {
    fn of(kind: u16) -> Self {
        match kind & 0x000F {
            0 => Self::Stored,
            1 => Self::MsZip,
            3 => {
                let window = match (kind >> 8) & 0x1F {
                    15 => lzxd::WindowSize::KB32,
                    16 => lzxd::WindowSize::KB64,
                    17 => lzxd::WindowSize::KB128,
                    18 => lzxd::WindowSize::KB256,
                    19 => lzxd::WindowSize::KB512,
                    20 => lzxd::WindowSize::MB1,
                    21 => lzxd::WindowSize::MB2,
                    other => {
                        return Self::Refused(format!("LZX window of 2^{other} bytes"));
                    }
                };
                Self::Lzx(window)
            }
            2 => Self::Refused("Quantum compression is not read".to_owned()),
            other => Self::Refused(format!("compression method {other}")),
        }
    }
}

/// One folder: where its data blocks start and how they are compressed.
#[derive(Debug, Clone)]
struct Folder {
    first_block: u64,
    blocks: u16,
    method: Method,
}

/// Where one entry lies in its folder's expanded stream.
#[derive(Debug, Clone, Copy)]
struct Placement {
    folder: usize,
    offset: u64,
    size: u64,
}

/// The folders and entry placements of one cabinet.
#[derive(Debug)]
pub(crate) struct CabIndex {
    folders: Vec<Folder>,
    placements: Vec<Placement>,
    data_reserve: u64,
    decoded: Mutex<Vec<Option<ArchiveBacking>>>,
}

fn le16(bytes: &[u8], at: usize) -> u16 {
    bytes
        .get(at..at + 2)
        .and_then(|field| <[u8; 2]>::try_from(field).ok())
        .map_or(0, u16::from_le_bytes)
}

fn le32(bytes: &[u8], at: usize) -> u32 {
    bytes
        .get(at..at + 4)
        .and_then(|field| <[u8; 4]>::try_from(field).ok())
        .map_or(0, u32::from_le_bytes)
}

/// Read a NUL terminated string of at most [`MAX_NAME`] bytes.
fn read_name<R: Read>(reader: &mut R) -> VfsResult<Vec<u8>> {
    let mut out = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        read_exact_or_corrupt(reader, &mut byte, "cabinet name")?;
        if byte[0] == 0 {
            return Ok(out);
        }
        if out.len() >= MAX_NAME {
            return Err(VfsError::corrupt("cabinet name is too long"));
        }
        out.push(byte[0]);
    }
}

fn dos_time(date: u16, time: u16) -> Option<std::time::SystemTime> {
    civil_to_unix(
        1980 + i64::from(date >> 9),
        u32::from((date >> 5) & 0x0F),
        u32::from(date & 0x1F),
        u32::from(time >> 11),
        u32::from((time >> 5) & 0x3F),
        u32::from(time & 0x1F) * 2,
    )
    .and_then(unix_to_system_time)
}

/// Read the folder records that follow the header.
fn read_folders<R: Read + Seek>(
    reader: &mut R,
    count: u16,
    reserve: i64,
    len: u64,
) -> VfsResult<Vec<Folder>> {
    let mut folders = Vec::with_capacity(usize::from(count));
    for _ in 0..count {
        let mut record = [0u8; 8];
        read_exact_or_corrupt(reader, &mut record, "cabinet folder")?;
        let first_block = u64::from(le32(&record, 0));
        if first_block > len {
            return Err(VfsError::corrupt("cabinet folder starts past the file"));
        }
        folders.push(Folder {
            first_block,
            blocks: le16(&record, 4),
            method: Method::of(le16(&record, 6)),
        });
        reader.seek(SeekFrom::Current(reserve))?;
    }
    Ok(folders)
}

/// Build the listing and the placement of every entry.
///
/// # Errors
/// Returns [`VfsError::Corrupt`] when the header, a folder record or a file
/// record does not parse or points past the cabinet.
pub(crate) fn list(
    backing: &ArchiveBacking,
    cancel: &Cancel,
) -> VfsResult<(CabIndex, Vec<RawEntry>)> {
    let len = backing_len(backing)?;
    let mut reader = backing.reader()?;
    let mut header = [0u8; 36];
    read_exact_or_corrupt(&mut reader, &mut header, "cabinet header")?;
    if header.get(0..4) != Some(b"MSCF".as_slice()) {
        return Err(VfsError::corrupt("cabinet signature is wrong"));
    }
    let files_at = u64::from(le32(&header, 16));
    let folder_count = le16(&header, 26);
    let file_count = le16(&header, 28);
    let flags = le16(&header, 30);

    let (mut header_reserve, mut folder_reserve, mut data_reserve) = (0u64, 0i64, 0u64);
    if flags & FLAG_RESERVE != 0 {
        let mut sizes = [0u8; 4];
        read_exact_or_corrupt(&mut reader, &mut sizes, "cabinet reserve sizes")?;
        header_reserve = u64::from(le16(&sizes, 0));
        folder_reserve = i64::from(sizes[2]);
        data_reserve = u64::from(sizes[3]);
    }
    reader.seek(SeekFrom::Current(
        i64::try_from(header_reserve).unwrap_or(0),
    ))?;
    for flag in [FLAG_PREVIOUS, FLAG_NEXT] {
        if flags & flag != 0 {
            read_name(&mut reader)?;
            read_name(&mut reader)?;
        }
    }

    let folders = read_folders(&mut reader, folder_count, folder_reserve, len)?;

    if files_at > len {
        return Err(VfsError::corrupt("cabinet file table starts past the file"));
    }
    reader.seek(SeekFrom::Start(files_at))?;
    let mut placements = Vec::new();
    let mut out = Vec::new();
    for _ in 0..file_count {
        cancel.check()?;
        let mut record = [0u8; 16];
        read_exact_or_corrupt(&mut reader, &mut record, "cabinet file record")?;
        let raw_name = read_name(&mut reader)?;
        let size = u64::from(le32(&record, 0));
        let offset = u64::from(le32(&record, 4));
        let folder = le16(&record, 8);
        let attributes = le16(&record, 14);
        let name = if attributes & ATTR_NAME_UTF8 != 0 {
            String::from_utf8_lossy(&raw_name).into_owned()
        } else {
            raw_name.iter().map(|byte| char::from(*byte)).collect()
        };
        let Some((path, refusal)) = stored_path(&name, false).into_row() else {
            continue;
        };
        let mut entry = VfsEntry::file(path, size);
        entry.modified = dos_time(le16(&record, 10), le16(&record, 12));
        entry.time_fidelity = TimeFidelity::LocalTwoSecond;
        entry.attributes = Some(VfsAttributes {
            read_only: attributes & ATTR_READ_ONLY != 0,
            hidden: attributes & ATTR_HIDDEN != 0,
            system: attributes & ATTR_SYSTEM != 0,
            archive: attributes & ATTR_ARCHIVE != 0,
            windows_bits: Some(u32::from(attributes & 0x7F)),
            unix_mode: None,
            uid: None,
            gid: None,
        });
        let folder = usize::from(folder);
        let unreadable = match folders.get(folder) {
            None => Some("the entry continues in another cabinet of the set".to_owned()),
            Some(Folder {
                method: Method::Refused(reason),
                ..
            }) => Some(reason.clone()),
            Some(_) => None,
        };
        for reason in refusal.iter().chain(unreadable.iter()) {
            refuse(&mut entry, reason);
        }
        let locator = if entry.refused {
            None
        } else {
            placements.push(Placement {
                folder,
                offset,
                size,
            });
            Some((placements.len() - 1) as u64)
        };
        out.push(RawEntry { entry, locator });
    }

    let decoded = Mutex::new(vec![None; folders.len()]);
    Ok((
        CabIndex {
            folders,
            placements,
            data_reserve,
            decoded,
        },
        out,
    ))
}

impl CabIndex {
    /// Expand the entry recorded at `locator`.
    ///
    /// # Errors
    /// Returns [`VfsError::NotFound`] when the locator names no entry,
    /// [`VfsError::Corrupt`] when the folder does not expand or the entry
    /// lies past its end, and [`VfsError::LimitExceeded`] when a ceiling
    /// trips.
    pub(crate) fn open_entry(
        &self,
        backing: &ArchiveBacking,
        locator: Option<u64>,
        path: &VfsPath,
        limits: &Limits,
        budget: &Budget,
        cancel: &Cancel,
    ) -> VfsResult<OpenFile> {
        cancel.check()?;
        let placement = locator
            .and_then(|index| usize::try_from(index).ok())
            .and_then(|index| self.placements.get(index))
            .copied()
            .ok_or_else(|| VfsError::NotFound { path: path.clone() })?;
        let decoded = self.folder(backing, placement.folder, limits, cancel)?;
        let available = backing_len(&decoded)?;
        if placement.offset.saturating_add(placement.size) > available {
            return Err(VfsError::corrupt(format!(
                "{} lies past the end of its cabinet folder",
                path.as_str()
            )));
        }
        let reader = range_reader(&decoded, placement.offset, placement.size)?;
        materialize(reader, placement.size, limits, budget, cancel)
    }

    /// The expanded stream of folder `index`, decoding it on first use.
    fn folder(
        &self,
        backing: &ArchiveBacking,
        index: usize,
        limits: &Limits,
        cancel: &Cancel,
    ) -> VfsResult<ArchiveBacking> {
        let mut decoded = self
            .decoded
            .lock()
            .map_err(|_| VfsError::corrupt("cabinet folder lock poisoned"))?;
        if let Some(Some(existing)) = decoded.get(index) {
            return Ok(existing.clone());
        }
        let folder = self
            .folders
            .get(index)
            .ok_or_else(|| VfsError::corrupt("cabinet folder index"))?;
        let stream = FolderStream::new(backing, folder, self.data_reserve, cancel.clone())?;
        let budget = Budget::new(limits.max_archive_bytes);
        let expanded = decode_to_temp(stream, "cabinet folder", limits, &budget, cancel)?;
        if let Some(slot) = decoded.get_mut(index) {
            *slot = Some(expanded.clone());
        }
        Ok(expanded)
    }
}

/// Block by block expansion of one folder.
struct FolderStream {
    reader: BackingReader,
    len: u64,
    next_block: u64,
    blocks_left: u16,
    data_reserve: u64,
    decoder: BlockDecoder,
    current: Vec<u8>,
    position: usize,
    cancel: Cancel,
}

enum BlockDecoder {
    Stored,
    MsZip {
        inflater: flate2::Decompress,
        window: Vec<u8>,
    },
    Lzx(Box<lzxd::Lzxd>),
}

impl FolderStream {
    fn new(
        backing: &ArchiveBacking,
        folder: &Folder,
        data_reserve: u64,
        cancel: Cancel,
    ) -> VfsResult<Self> {
        let decoder = match &folder.method {
            Method::Stored => BlockDecoder::Stored,
            Method::MsZip => BlockDecoder::MsZip {
                inflater: flate2::Decompress::new(false),
                window: Vec::with_capacity(MSZIP_WINDOW),
            },
            Method::Lzx(window) => BlockDecoder::Lzx(Box::new(lzxd::Lzxd::new(*window))),
            Method::Refused(reason) => return Err(VfsError::unsupported(reason.clone())),
        };
        Ok(Self {
            reader: backing.reader()?,
            len: backing_len(backing)?,
            next_block: folder.first_block,
            blocks_left: folder.blocks,
            data_reserve,
            decoder,
            current: Vec::new(),
            position: 0,
            cancel,
        })
    }

    /// Read and expand the next data block into `current`.
    fn load(&mut self) -> io::Result<bool> {
        if self.blocks_left == 0 {
            return Ok(false);
        }
        self.blocks_left -= 1;
        let mut header = [0u8; 8];
        self.reader.seek(SeekFrom::Start(self.next_block))?;
        self.reader.read_exact(&mut header)?;
        let packed = u64::from(le16(&header, 4));
        let expanded = usize::from(le16(&header, 6));
        let data_at = self.next_block + 8 + self.data_reserve;
        let end = data_at + packed;
        if end > self.len || expanded > MAX_BLOCK {
            return Err(corrupt("cabinet data block runs past the file"));
        }
        let reserve_at = self.next_block + 8;
        self.next_block = end;
        let mut reserve = vec![0u8; usize::try_from(self.data_reserve).unwrap_or(0)];
        self.reader.seek(SeekFrom::Start(reserve_at))?;
        self.reader.read_exact(&mut reserve)?;
        let mut packed_bytes = vec![0u8; usize::try_from(packed).unwrap_or(0)];
        self.reader.read_exact(&mut packed_bytes)?;
        let stated = le32(&header, 0);
        let sizes = le32(&header, 4);
        // Zero states that no checksum was written. Writers disagree on
        // whether the reserved area is covered, so either form is accepted.
        if stated != 0
            && block_checksum(&[&reserve, &packed_bytes], sizes) != stated
            && (reserve.is_empty() || block_checksum(&[&packed_bytes], sizes) != stated)
        {
            return Err(corrupt("cabinet data block fails its checksum"));
        }

        self.current = match &mut self.decoder {
            BlockDecoder::Stored => packed_bytes,
            BlockDecoder::MsZip { inflater, window } => {
                inflate_block(inflater, window, &packed_bytes, expanded)?
            }
            BlockDecoder::Lzx(lzx) => lzx
                .decompress_next(&packed_bytes, expanded)
                .map_err(|error| corrupt(&format!("LZX block: {error}")))?
                .to_vec(),
        };
        if self.current.len() != expanded {
            return Err(corrupt("cabinet data block expanded to the wrong size"));
        }
        self.position = 0;
        Ok(true)
    }
}

impl Read for FolderStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.cancel.is_cancelled() {
            return Err(carry(VfsError::Cancelled));
        }
        while self.position >= self.current.len() {
            if !self.load()? {
                return Ok(0);
            }
        }
        let rest = self.current.get(self.position..).unwrap_or_default();
        let count = rest.len().min(buf.len());
        buf.get_mut(..count)
            .unwrap_or_default()
            .copy_from_slice(rest.get(..count).unwrap_or_default());
        self.position += count;
        Ok(count)
    }
}

/// The checksum of one data block over `parts`, folded with the packed and
/// expanded sizes that the block header holds as one little endian word.
///
/// Whole four byte words are read little endian. A tail of one to three
/// bytes is read big endian, as the format defines it.
fn block_checksum(parts: &[&[u8]], sizes: u32) -> u32 {
    let mut value = 0u32;
    let mut word = [0u8; 4];
    let mut filled = 0usize;
    for byte in parts.iter().flat_map(|part| part.iter()) {
        if let Some(slot) = word.get_mut(filled) {
            *slot = *byte;
        }
        filled += 1;
        if filled == 4 {
            value ^= u32::from_le_bytes(word);
            filled = 0;
        }
    }
    let tail = word
        .get(..filled)
        .unwrap_or_default()
        .iter()
        .fold(0u32, |tail, byte| (tail << 8) | u32::from(*byte));
    value ^ tail ^ sizes
}

fn corrupt(detail: &str) -> io::Error {
    carry(VfsError::corrupt(detail.to_owned()))
}

/// Expand one MSZIP block.
///
/// Each block is a complete deflate stream that may refer back into the last
/// 32 KiB of the output before it. The inflater is primed with that window
/// as one stored block, so a back reference resolves exactly as it would
/// have in one continuous stream.
fn inflate_block(
    inflater: &mut flate2::Decompress,
    window: &mut Vec<u8>,
    packed: &[u8],
    expanded: usize,
) -> io::Result<Vec<u8>> {
    let Some(body) = packed.strip_prefix(b"CK") else {
        return Err(corrupt("MSZIP block signature is wrong"));
    };
    inflater.reset(false);
    if !window.is_empty() {
        let length = u16::try_from(window.len()).unwrap_or(u16::MAX);
        let mut primer = Vec::with_capacity(window.len() + 5);
        primer.push(0);
        primer.extend_from_slice(&length.to_le_bytes());
        primer.extend_from_slice(&(!length).to_le_bytes());
        primer.extend_from_slice(window.get(..usize::from(length)).unwrap_or_default());
        let mut sink = Vec::with_capacity(window.len());
        inflater
            .decompress_vec(&primer, &mut sink, flate2::FlushDecompress::Sync)
            .map_err(|error| corrupt(&format!("MSZIP window: {error}")))?;
    }
    let mut out = Vec::with_capacity(expanded);
    inflater
        .decompress_vec(body, &mut out, flate2::FlushDecompress::Finish)
        .map_err(|error| corrupt(&format!("MSZIP block: {error}")))?;
    if out.len() >= MSZIP_WINDOW {
        window.clear();
        window.extend_from_slice(out.get(out.len() - MSZIP_WINDOW..).unwrap_or_default());
    } else {
        let keep = MSZIP_WINDOW - out.len();
        if window.len() > keep {
            window.drain(..window.len() - keep);
        }
        window.extend_from_slice(&out);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    #[test]
    fn dos_time_reads_the_packed_fields() {
        let date = ((2024 - 1980) << 9) | (5 << 5) | 6;
        let time = (7 << 11) | (8 << 5) | 5;
        let expected = unix_to_system_time(civil_to_unix(2024, 5, 6, 7, 8, 10).unwrap());
        assert_eq!(dos_time(date, time), expected);
    }

    #[test]
    fn the_block_checksum_matches_known_values() {
        assert_eq!(
            block_checksum(&[b"Hello, world!\n"], 0x000E_000E),
            0x7F2E_1A4C
        );
        assert_eq!(
            block_checksum(&[b"Hello, world!\n", b"See you later!\n"], 0x001D_001D),
            0x3509_541A
        );
    }

    #[test]
    fn quantum_is_refused_by_name() {
        assert!(matches!(Method::of(2), Method::Refused(reason) if reason.contains("Quantum")));
    }
}
