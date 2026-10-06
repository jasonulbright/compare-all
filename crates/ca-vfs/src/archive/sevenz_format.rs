//! 7z, read only.
//!
//! Entries of a solid block are decoded in order, and the decoder cannot start
//! part way through one, so serving each entry with a pass of its own costs
//! the whole block per entry. One pass therefore fills a bounded cache with
//! the entries that follow the one asked for, which turns reading a container
//! end to end into a pass per cache fill rather than a pass per entry.

use std::collections::{BTreeMap, HashMap};
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::sync::{Arc, Mutex};

use sevenz_rust2::{ArchiveReader, Error as SevenZError, Password};

use super::ArchiveBacking;
use crate::cancel::Cancel;
use crate::entry::{EntryKind, VfsAttributes, VfsEntry};
use crate::error::{LimitKind, VfsError, VfsResult};
use crate::fs::OpenFile;
use crate::limits::{read_bounded, uncarried, Budget, Limits};
use crate::path::VfsPath;
use crate::stored::{refuse, stored_path};
use crate::tree::RawEntry;

/// Windows attribute bits a 7z entry can carry.
const READ_ONLY: u32 = 0x0000_0001;
const HIDDEN: u32 = 0x0000_0002;
const SYSTEM: u32 = 0x0000_0004;
const ARCHIVE: u32 = 0x0000_0020;

/// How much larger than the in-memory ceiling the whole cache may grow once it
/// is allowed to spill to temporary files.
const SPILL_MULTIPLE: u64 = 64;

fn password_of(password: Option<&str>) -> Password {
    password.map_or_else(Password::empty, Password::from)
}

/// Bytes shared with a reader handed to a caller.
#[derive(Debug, Clone)]
struct Shared(Arc<Vec<u8>>);

impl AsRef<[u8]> for Shared {
    fn as_ref(&self) -> &[u8] {
        self.0.as_slice()
    }
}

/// One entry that has already been decoded.
#[derive(Debug, Clone)]
enum Cached {
    Memory(Shared),
    File(Arc<tempfile::NamedTempFile>, u64),
}

impl Cached {
    fn len(&self) -> u64 {
        match self {
            Self::Memory(bytes) => bytes.0.len() as u64,
            Self::File(_, len) => *len,
        }
    }

    fn open(&self) -> VfsResult<OpenFile> {
        match self {
            Self::Memory(bytes) => {
                let len = bytes.0.len() as u64;
                Ok(OpenFile::seekable(Cursor::new(bytes.clone()), Some(len)))
            }
            Self::File(temp, len) => {
                let file = std::fs::File::open(temp.path())?;
                Ok(OpenFile::seekable(file, Some(*len)))
            }
        }
    }
}

/// Entries decoded out of a block and kept for the reads that follow, keyed
/// by their position in the container's file list.
#[derive(Debug)]
pub(crate) struct BlockCache {
    items: BTreeMap<usize, Cached>,
    used: u64,
    memory_ceiling: u64,
    total_ceiling: u64,
}

impl BlockCache {
    /// A cache that keeps entries up to `memory_ceiling` bytes in memory and
    /// spills the rest to temporary files.
    pub(crate) fn new(memory_ceiling: u64) -> Self {
        Self {
            items: BTreeMap::new(),
            used: 0,
            memory_ceiling,
            total_ceiling: memory_ceiling.saturating_mul(SPILL_MULTIPLE),
        }
    }

    fn take(&mut self, index: usize) -> Option<Cached> {
        // An entry is served once. Keeping it would hold the whole container
        // after a single pass over it.
        self.items.remove(&index).inspect(|item| {
            self.used = self.used.saturating_sub(item.len());
        })
    }

    fn has_room(&self) -> bool {
        self.used < self.total_ceiling
    }

    fn store(&mut self, index: usize, bytes: Vec<u8>) -> VfsResult<Cached> {
        let len = bytes.len() as u64;
        let item = if len <= self.memory_ceiling {
            Cached::Memory(Shared(Arc::new(bytes)))
        } else {
            let mut temp = tempfile::NamedTempFile::new()?;
            temp.write_all(&bytes)?;
            temp.flush()?;
            Cached::File(Arc::new(temp), len)
        };
        self.used = self.used.saturating_add(len);
        self.items.insert(index, item.clone());
        Ok(item)
    }
}

/// Most bytes the container header may occupy, stored or expanded. The header
/// is held whole in memory and every entry record is built from it.
const MAX_HEADER_BYTES: u64 = 32 * 1024 * 1024;
/// Signature, version, start header checksum and the start header itself.
const SIGNATURE_HEADER_BYTES: u64 = 32;
const SIGNATURE: [u8; 6] = [b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C];
const ID_END: u8 = 0x00;
const ID_PACK_INFO: u8 = 0x06;
const ID_UNPACK_INFO: u8 = 0x07;
const ID_SIZE: u8 = 0x09;
const ID_CRC: u8 = 0x0A;
const ID_FOLDER: u8 = 0x0B;
const ID_CODERS_UNPACK_SIZE: u8 = 0x0C;
const ID_ENCODED_HEADER: u8 = 0x17;

/// Refuse a container whose header would take more than [`MAX_HEADER_BYTES`].
///
/// The decoder expands an encoded header to whatever size it declares before
/// any entry is known, and a start header of zeros makes it try every
/// candidate header near the file end, so both are settled here from the raw
/// bytes before the decoder sees the container. What this check cannot read
/// is left for the decoder to report.
fn check_header(backing: &ArchiveBacking) -> VfsResult<()> {
    let mut reader = backing.reader()?;
    let len = reader.seek(SeekFrom::End(0))?;
    reader.seek(SeekFrom::Start(0))?;
    let mut start = [0u8; 32];
    if len < SIGNATURE_HEADER_BYTES || reader.read_exact(&mut start).is_err() {
        return Ok(());
    }
    if start.get(..6) != Some(SIGNATURE.as_slice()) || start[6] != 0 {
        return Ok(());
    }
    let record = &start[12..32];
    let crc = u32::from_le_bytes([start[8], start[9], start[10], start[11]]);
    if crc == 0 && record.iter().all(|byte| *byte == 0) {
        return Err(VfsError::corrupt(
            "7z start header is empty; the container was not finished",
        ));
    }
    if crc32fast::hash(record) != crc {
        return Ok(());
    }
    let word = |at: usize| {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&record[at..at + 8]);
        u64::from_le_bytes(bytes)
    };
    let (offset, size) = (word(0), word(8));
    if size > MAX_HEADER_BYTES {
        return Err(header_limit());
    }
    let Some(at) = SIGNATURE_HEADER_BYTES
        .checked_add(offset)
        .filter(|at| at.checked_add(size).is_some_and(|end| end <= len))
    else {
        return Ok(());
    };
    let mut header = vec![0u8; usize::try_from(size).unwrap_or(0)];
    reader.seek(SeekFrom::Start(at))?;
    reader.read_exact(&mut header)?;
    let mut cursor = HeaderCursor { bytes: &header };
    if header.is_empty() || cursor.byte()? != ID_ENCODED_HEADER {
        return Ok(());
    }
    check_encoded_header(&mut cursor, len)
}

fn header_limit() -> VfsError {
    VfsError::LimitExceeded {
        kind: LimitKind::ArchiveSize,
        limit: MAX_HEADER_BYTES,
    }
}

/// Check the stream description that stands in for an encoded header: the
/// packed streams must lie inside the file and every declared output size must
/// be within [`MAX_HEADER_BYTES`].
fn check_encoded_header(cursor: &mut HeaderCursor<'_>, len: u64) -> VfsResult<()> {
    let mut id = cursor.byte()?;
    if id == ID_PACK_INFO {
        let pack_at = cursor.number()?;
        let streams = cursor.count()?;
        let mut packed_total: u64 = 0;
        loop {
            match cursor.byte()? {
                ID_SIZE => {
                    for _ in 0..streams {
                        packed_total = packed_total.saturating_add(cursor.number()?);
                    }
                }
                ID_CRC => {
                    let defined = cursor.defined(streams)?;
                    cursor.skip(defined.saturating_mul(4))?;
                }
                ID_END => break,
                _ => return Err(malformed()),
            }
        }
        let end = SIGNATURE_HEADER_BYTES
            .saturating_add(pack_at)
            .saturating_add(packed_total);
        if end > len {
            return Err(VfsError::corrupt(
                "7z encoded header lies past the file end",
            ));
        }
        id = cursor.byte()?;
    }
    if id != ID_UNPACK_INFO {
        return Ok(());
    }
    if cursor.byte()? != ID_FOLDER {
        return Err(malformed());
    }
    let blocks = cursor.count()?;
    if cursor.byte()? != 0 {
        return Ok(());
    }
    let mut outputs: u64 = 0;
    for _ in 0..blocks {
        outputs = outputs.saturating_add(skip_block(cursor)?);
    }
    if cursor.byte()? != ID_CODERS_UNPACK_SIZE {
        return Err(malformed());
    }
    for _ in 0..outputs {
        if cursor.number()? > MAX_HEADER_BYTES {
            return Err(header_limit());
        }
    }
    Ok(())
}

/// Step over one block description and return how many output streams it
/// declares.
fn skip_block(cursor: &mut HeaderCursor<'_>) -> VfsResult<u64> {
    let coders = cursor.count()?;
    let (mut inputs, mut outputs) = (0u64, 0u64);
    for _ in 0..coders {
        let flags = cursor.byte()?;
        if flags & 0x80 != 0 {
            return Err(VfsError::unsupported("7z alternative coder methods"));
        }
        let method = cursor.take(u64::from(flags & 0x0F))?;
        let (coder_in, coder_out) = if flags & 0x10 == 0 {
            (1, 1)
        } else {
            (cursor.count()?, cursor.count()?)
        };
        inputs = inputs.saturating_add(coder_in);
        outputs = outputs.saturating_add(coder_out);
        let properties = if flags & 0x20 != 0 {
            let length = cursor.count()?;
            cursor.take(length)?
        } else {
            &[]
        };
        check_coder(method, properties)?;
    }
    let pairs = outputs.checked_sub(1).ok_or_else(malformed)?;
    for _ in 0..pairs {
        cursor.number()?;
        cursor.number()?;
    }
    let packed = inputs.checked_sub(pairs).ok_or_else(malformed)?;
    if packed > 1 {
        for _ in 0..packed {
            cursor.number()?;
        }
    }
    Ok(outputs)
}

fn malformed() -> VfsError {
    VfsError::corrupt("7z header is truncated or malformed")
}

/// Most memory one coder may name for its dictionary or model, the same
/// ceiling the xz and LZMA readers apply.
const CODER_MEMORY_LIMIT: u64 = super::span::DICTIONARY_LIMIT_KIB as u64 * 1024;

const METHOD_LZMA: &[u8] = &[0x03, 0x01, 0x01];
const METHOD_LZMA2: &[u8] = &[0x21];
const METHOD_PPMD: &[u8] = &[0x03, 0x04, 0x01];

/// The memory the decoder of a coder claims before it reads any data, as its
/// properties name it, or `None` for a coder that claims a fixed amount.
fn coder_memory(method: &[u8], properties: &[u8]) -> Option<u64> {
    match method {
        METHOD_LZMA2 => {
            let bits = u32::from(*properties.first()?);
            match bits {
                0..=39 => Some(u64::from(2 | (bits & 1)) << (bits / 2 + 11)),
                40 => Some(u64::from(u32::MAX)),
                _ => None,
            }
        }
        METHOD_LZMA | METHOD_PPMD => {
            let field = properties.get(1..5)?;
            Some(u64::from(u32::from_le_bytes([
                field[0], field[1], field[2], field[3],
            ])))
        }
        _ => None,
    }
}

/// Refuse a coder whose decoder would claim more than [`CODER_MEMORY_LIMIT`].
///
/// The decoder library allocates whatever dictionary a coder names, so the
/// check runs on the raw properties before the library sees them.
fn check_coder(method: &[u8], properties: &[u8]) -> VfsResult<()> {
    match coder_memory(method, properties) {
        Some(memory) if memory > CODER_MEMORY_LIMIT => Err(VfsError::LimitExceeded {
            kind: LimitKind::DecoderMemory,
            limit: CODER_MEMORY_LIMIT,
        }),
        _ => Ok(()),
    }
}

/// [`check_coder`] for every coder of every block of a parsed container.
fn check_blocks(archive: &sevenz_rust2::Archive) -> VfsResult<()> {
    for block in &archive.blocks {
        for coder in &block.coders {
            check_coder(coder.encoder_method_id(), coder.properties())?;
        }
    }
    Ok(())
}

/// Reads raw header fields. A count is capped by the bytes left, since each
/// counted item takes at least one byte.
struct HeaderCursor<'a> {
    bytes: &'a [u8],
}

impl<'a> HeaderCursor<'a> {
    fn byte(&mut self) -> VfsResult<u8> {
        let (&first, rest) = self.bytes.split_first().ok_or_else(malformed)?;
        self.bytes = rest;
        Ok(first)
    }

    fn skip(&mut self, count: u64) -> VfsResult<()> {
        self.take(count).map(|_| ())
    }

    fn take(&mut self, count: u64) -> VfsResult<&'a [u8]> {
        let count = usize::try_from(count).map_err(|_| malformed())?;
        if count > self.bytes.len() {
            return Err(malformed());
        }
        let (taken, rest) = self.bytes.split_at(count);
        self.bytes = rest;
        Ok(taken)
    }

    /// The leading one bits of the first byte say how many little-endian
    /// bytes follow it.
    fn number(&mut self) -> VfsResult<u64> {
        let first = u64::from(self.byte()?);
        let mut mask = 0x80u64;
        let mut value = 0u64;
        for index in 0..8 {
            if first & mask == 0 {
                return Ok(value | ((first & (mask - 1)) << (8 * index)));
            }
            value |= u64::from(self.byte()?) << (8 * index);
            mask >>= 1;
        }
        Ok(value)
    }

    fn count(&mut self) -> VfsResult<u64> {
        let value = self.number()?;
        if value > self.bytes.len() as u64 {
            return Err(malformed());
        }
        Ok(value)
    }

    /// Read an all-defined byte or a bit field over `items` and return how
    /// many items are defined.
    fn defined(&mut self, items: u64) -> VfsResult<u64> {
        if self.byte()? != 0 {
            return Ok(items);
        }
        let mut set = 0u64;
        for _ in 0..items.div_ceil(8) {
            set += u64::from(self.byte()?.count_ones());
        }
        Ok(set)
    }
}

/// Build the listing. Each file carries its position in the container's file
/// list as its locator.
///
/// # Errors
/// Returns [`VfsError::NeedsPassword`] when the header itself is encrypted and
/// [`VfsError::Corrupt`] when the container does not parse,
/// [`VfsError::LimitExceeded`] when the header is larger than
/// [`MAX_HEADER_BYTES`] and [`VfsError::Cancelled`] when the flag is raised.
pub(crate) fn list(
    backing: &ArchiveBacking,
    password: Option<&str>,
    cancel: &Cancel,
) -> VfsResult<Vec<RawEntry>> {
    cancel.check()?;
    check_header(backing)?;
    let reader = backing.reader()?;
    let archive = ArchiveReader::new(reader, password_of(password))
        .map_err(|error| map_sevenz(error, &VfsPath::root()))?;

    let mut out = Vec::new();
    for (position, file) in archive.archive().files.iter().enumerate() {
        cancel.check()?;
        let Some((path, refusal)) = stored_path(&file.name, file.is_directory).into_row() else {
            continue;
        };
        let kind = if file.is_directory {
            EntryKind::Directory
        } else {
            EntryKind::File
        };
        let attributes = file.has_windows_attributes.then(|| {
            let bits = file.windows_attributes;
            VfsAttributes {
                read_only: bits & READ_ONLY != 0,
                hidden: bits & HIDDEN != 0,
                system: bits & SYSTEM != 0,
                archive: bits & ARCHIVE != 0,
                windows_bits: Some(bits),
                unix_mode: None,
                uid: None,
                gid: None,
            }
        });
        let modified = file
            .has_last_modified_date
            .then(|| std::time::SystemTime::from(file.last_modified_date));
        let created = file
            .has_creation_date
            .then(|| std::time::SystemTime::from(file.creation_date));
        let name = path.name().unwrap_or_default().to_owned();
        let mut entry = VfsEntry {
            path,
            name,
            kind,
            size: if file.is_directory { 0 } else { file.size },
            size_is_exact: true,
            modified,
            time_fidelity: crate::entry::TimeFidelity::Utc,
            created,
            attributes,
            crc32: (file.has_crc && !file.is_directory)
                .then(|| u32::try_from(file.crc & 0xFFFF_FFFF).unwrap_or_default()),
            link: None,
            version_info: None,
            error: None,
            refused: false,
        };
        let locator = match refusal {
            Some(reason) => {
                refuse(&mut entry, &reason);
                None
            }
            None if file.is_directory => None,
            None => u64::try_from(position).ok(),
        };
        out.push(RawEntry { entry, locator });
    }
    Ok(out)
}

/// Expand the entry at position `locator` of the file list, filling the cache
/// with what follows it in the same pass.
///
/// # Errors
/// Returns [`VfsError::NeedsPassword`] for an encrypted entry with no
/// password, [`VfsError::NotFound`] when the container holds no such entry,
/// [`VfsError::Cancelled`] when the flag is raised while the decoder is
/// running, and [`VfsError::LimitExceeded`] when a ceiling trips.
#[allow(
    clippy::too_many_arguments,
    reason = "one entry read needs the container, the entry, the credentials, the ceilings and the cache"
)]
pub(crate) fn open_entry(
    backing: &ArchiveBacking,
    locator: Option<u64>,
    path: &VfsPath,
    password: Option<&str>,
    limits: &Limits,
    budget: &Budget,
    cancel: &Cancel,
    cache: &Mutex<BlockCache>,
) -> VfsResult<OpenFile> {
    cancel.check()?;
    let wanted = locator
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| VfsError::NotFound { path: path.clone() })?;
    {
        let mut guard = cache
            .lock()
            .map_err(|_| VfsError::corrupt("archive cache lock poisoned"))?;
        if let Some(hit) = guard.take(wanted) {
            budget.take(hit.len())?;
            return hit.open();
        }
    }

    check_header(backing)?;
    let reader = backing.reader()?;
    let mut archive = ArchiveReader::new(reader, password_of(password))
        .map_err(|error| map_sevenz(error, path))?;
    check_blocks(archive.archive())?;
    // The pass hands over references into the archive's own file list, and
    // that list does not move while the pass runs, so an entry's address
    // names its position even when two entries share a name.
    let position_of: HashMap<usize, usize> = archive
        .archive()
        .files
        .iter()
        .enumerate()
        .map(|(position, file)| (std::ptr::from_ref(file).addr(), position))
        .collect();

    // Decoding is speculative past the entry asked for, so it draws on an
    // allowance of its own; only what the caller receives is charged to theirs.
    let fill = Budget::new(limits.max_archive_bytes);
    let mut reached = false;
    let mut found: Option<Vec<u8>> = None;
    let mut ahead: Vec<(usize, Vec<u8>)> = Vec::new();
    let mut failure: Option<VfsError> = None;
    let mut room = {
        let guard = cache
            .lock()
            .map_err(|_| VfsError::corrupt("archive cache lock poisoned"))?;
        guard.has_room()
    };

    let outcome = archive.for_each_entries(|entry, stream| {
        if cancel.is_cancelled() {
            failure = Some(VfsError::Cancelled);
            return Ok(false);
        }
        let Some(candidate) = position_of.get(&std::ptr::from_ref(entry).addr()).copied() else {
            return Ok(true);
        };
        if !reached {
            if candidate != wanted {
                return Ok(true);
            }
            reached = true;
            return match read_bounded(stream, entry.compressed_size, limits, &fill, cancel) {
                Ok(bytes) => {
                    found = Some(bytes);
                    Ok(room)
                }
                Err(error) => {
                    failure = Some(error);
                    Ok(false)
                }
            };
        }
        if entry.is_directory {
            return Ok(true);
        }
        match read_bounded(stream, entry.compressed_size, limits, &fill, cancel) {
            Ok(bytes) => {
                room = room && bytes.len() as u64 <= fill_headroom(&ahead);
                ahead.push((candidate, bytes));
                Ok(room)
            }
            // A later entry failing says nothing about the one asked for, so
            // the pass simply stops filling.
            Err(_) => Ok(false),
        }
    });

    if let Some(error) = failure {
        return Err(error);
    }
    if let Some(carried) = outcome.as_ref().err().and_then(uncarried) {
        return Err(carried);
    }
    outcome.map_err(|error| map_sevenz(error, path))?;

    let Some(bytes) = found else {
        return Err(VfsError::NotFound { path: path.clone() });
    };

    let mut guard = cache
        .lock()
        .map_err(|_| VfsError::corrupt("archive cache lock poisoned"))?;
    for (position, bytes) in ahead {
        if !guard.has_room() {
            break;
        }
        guard.store(position, bytes)?;
    }
    drop(guard);

    budget.take(bytes.len() as u64)?;
    let len = bytes.len() as u64;
    Ok(OpenFile::seekable(
        Cursor::new(Shared(Arc::new(bytes))),
        Some(len),
    ))
}

/// How much more one pass may read ahead before the cache is full.
fn fill_headroom(ahead: &[(usize, Vec<u8>)]) -> u64 {
    const PASS_CEILING: u64 = 512 * 1024 * 1024;
    let used: u64 = ahead.iter().map(|(_, bytes)| bytes.len() as u64).sum();
    PASS_CEILING.saturating_sub(used)
}

/// Map a 7z failure onto the crate's error type.
fn map_sevenz(error: SevenZError, path: &VfsPath) -> VfsError {
    if let Some(carried) = uncarried(&error) {
        return carried;
    }
    match error {
        SevenZError::PasswordRequired => VfsError::NeedsPassword { path: path.clone() },
        SevenZError::MaybeBadPassword(_) => VfsError::WrongPassword { path: path.clone() },
        SevenZError::UnsupportedCompressionMethod(method) => {
            VfsError::unsupported(format!("7z compression method {method}"))
        }
        SevenZError::Unsupported(detail) => VfsError::unsupported(detail.into_owned()),
        SevenZError::FileNotFound => VfsError::NotFound { path: path.clone() },
        SevenZError::Io(error, _) => crate::limits::uncarry(error),
        other => VfsError::corrupt(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::{check_coder, METHOD_LZMA, METHOD_LZMA2, METHOD_PPMD};

    #[test]
    fn a_coder_is_refused_only_above_the_memory_ceiling() {
        // LZMA2 property 32 names 256 MiB, 33 names 384 MiB, 40 names 4 GiB.
        assert!(check_coder(METHOD_LZMA2, &[24]).is_ok());
        assert!(check_coder(METHOD_LZMA2, &[32]).is_ok());
        assert!(check_coder(METHOD_LZMA2, &[33]).is_err());
        assert!(check_coder(METHOD_LZMA2, &[40]).is_err());
        let field = |bytes: u32| {
            let mut properties = vec![0x5d];
            properties.extend_from_slice(&bytes.to_le_bytes());
            properties
        };
        assert!(check_coder(METHOD_LZMA, &field(64 << 20)).is_ok());
        assert!(check_coder(METHOD_LZMA, &field(u32::MAX)).is_err());
        assert!(check_coder(METHOD_PPMD, &field(192 << 20)).is_ok());
        assert!(check_coder(METHOD_PPMD, &field(1 << 30)).is_err());
        assert!(check_coder(&[0x00], &[]).is_ok());
    }
}
