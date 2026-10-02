//! Tar, on its own or inside one compressed stream.

use std::io::{self, Read, Seek, SeekFrom, Write};
use std::sync::Arc;

use super::{unix_to_system_time, ArchiveBacking};
use crate::cancel::Cancel;
use crate::detect::ArchiveFormat;
use crate::entry::{EntryKind, VfsAttributes, VfsEntry, VfsLinkKind};
use crate::error::{LimitKind, VfsError, VfsResult};
use crate::fs::OpenFile;
use crate::limits::{carry, materialize, uncarry_or, Budget, LimitedReader, Limits};
use crate::path::VfsPath;
use crate::stored::{refuse, stored_path};
use crate::tree::RawEntry;

/// The stream a tar is wrapped in, if any.
pub(crate) fn stream_format(format: ArchiveFormat) -> Option<ArchiveFormat> {
    match format {
        ArchiveFormat::TarGz => Some(ArchiveFormat::Gz),
        ArchiveFormat::TarBz2 => Some(ArchiveFormat::Bz2),
        ArchiveFormat::TarXz => Some(ArchiveFormat::Xz),
        _ => None,
    }
}

/// Open the tar byte stream, unwrapping one layer of compression.
fn plain_stream<'a>(
    backing: &ArchiveBacking,
    stream: Option<ArchiveFormat>,
    limits: &Limits,
    budget: &Budget,
    cancel: &Cancel,
) -> VfsResult<Box<dyn Read + 'a>> {
    let reader = backing.reader()?;
    let inner: Box<dyn Read> = match stream {
        Some(format) => super::single::decoder(format, reader)?,
        None => Box::new(reader),
    };
    // The ceilings apply to the tar stream as a whole: a tar carries no
    // per-entry stored size to measure one entry's expansion against.
    Ok(Box::new(ExtensionGuard::new(LimitedReader::new(
        inner,
        0,
        *limits,
        budget.clone(),
        cancel.clone(),
    ))))
}

/// Most bytes one GNU long name, long link or pax header may declare. The tar
/// reader holds such a member whole in memory before the entry it describes.
const MAX_EXTENSION_BYTES: u64 = 1024 * 1024;

const BLOCK: u64 = 512;

/// Follows the tar layout as the bytes pass and fails the read that reaches a
/// long name, long link or pax header declaring more than
/// [`MAX_EXTENSION_BYTES`], before the tar reader buffers it.
///
/// The guard stops checking at the first header whose checksum does not
/// match, so a layout it cannot follow is left to the tar reader to report.
struct ExtensionGuard<R> {
    inner: R,
    position: u64,
    next_header: u64,
    header: [u8; 512],
    filled: usize,
    /// Content size of a GNU sparse entry whose extension blocks follow.
    sparse_content: Option<u64>,
    /// A pax header being collected, and the position where it ends.
    pax: Option<(Vec<u8>, u64)>,
    /// A size a pax header states for the entry that follows it.
    pax_size: Option<u64>,
    active: bool,
}

impl<R> ExtensionGuard<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            position: 0,
            next_header: 0,
            header: [0; 512],
            filled: 0,
            sparse_content: None,
            pax: None,
            pax_size: None,
            active: true,
        }
    }

    fn observe(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut rest = bytes;
        while self.active && !rest.is_empty() {
            if self.position < self.next_header {
                let skip = usize::try_from(self.next_header - self.position)
                    .unwrap_or(usize::MAX)
                    .min(rest.len());
                let (passed, after) = rest.split_at(skip);
                if let Some((collected, end)) = self.pax.as_mut() {
                    let wanted = usize::try_from(end.saturating_sub(self.position))
                        .unwrap_or(usize::MAX)
                        .min(passed.len());
                    collected.extend_from_slice(passed.get(..wanted).unwrap_or_default());
                    if self.position + wanted as u64 >= *end {
                        self.pax_size = tar::PaxExtensions::new(collected)
                            .filter_map(Result::ok)
                            .find(|extension| extension.key_bytes() == b"size")
                            .and_then(|extension| extension.value().ok()?.parse().ok());
                        self.pax = None;
                    }
                }
                self.position += passed.len() as u64;
                rest = after;
                continue;
            }
            let take = (self.header.len() - self.filled).min(rest.len());
            let (block, after) = rest.split_at(take);
            if let Some(slot) = self.header.get_mut(self.filled..self.filled + take) {
                slot.copy_from_slice(block);
            }
            self.filled += take;
            self.position += take as u64;
            rest = after;
            if self.filled == self.header.len() {
                self.filled = 0;
                self.next_header = self.position;
                self.header_complete()?;
            }
        }
        Ok(())
    }

    fn header_complete(&mut self) -> io::Result<()> {
        if let Some(content) = self.sparse_content {
            // GNU sparse extension block: byte 504 says whether another follows.
            if self.header.get(504).copied().unwrap_or(0) == 0 {
                self.sparse_content = None;
                self.next_header = self.position.saturating_add(padded(content));
            }
            return Ok(());
        }
        if self.header.iter().all(|byte| *byte == 0) {
            return Ok(());
        }
        let header = tar::Header::from_byte_slice(&self.header);
        let checksum_matches = header.cksum().is_ok_and(|stated| {
            let sum: u32 = self
                .header
                .iter()
                .enumerate()
                .map(|(index, byte)| {
                    if (148..156).contains(&index) {
                        u32::from(b' ')
                    } else {
                        u32::from(*byte)
                    }
                })
                .sum();
            stated == sum
        });
        let Ok(stated_size) = header.entry_size() else {
            self.active = false;
            return Ok(());
        };
        if !checksum_matches {
            self.active = false;
            return Ok(());
        }
        let kind = header.entry_type();
        let recognized = header.as_gnu().is_some() || header.as_ustar().is_some();
        let extension = recognized
            && (kind.is_gnu_longname()
                || kind.is_gnu_longlink()
                || kind.is_pax_local_extensions()
                || kind.is_pax_global_extensions());
        let size = if extension {
            stated_size
        } else {
            self.pax_size.take().unwrap_or(stated_size)
        };
        if extension && size > MAX_EXTENSION_BYTES {
            return Err(carry(VfsError::LimitExceeded {
                kind: LimitKind::EntrySize,
                limit: MAX_EXTENSION_BYTES,
            }));
        }
        if recognized && kind.is_pax_local_extensions() {
            self.pax = Some((Vec::new(), self.position.saturating_add(size)));
        }
        if kind.is_gnu_sparse() && header.as_gnu().is_some_and(tar::GnuHeader::is_extended) {
            self.sparse_content = Some(size);
            return Ok(());
        }
        self.next_header = self.position.saturating_add(padded(size));
        Ok(())
    }
}

fn padded(size: u64) -> u64 {
    size.div_ceil(BLOCK).saturating_mul(BLOCK)
}

impl<R: Read> Read for ExtensionGuard<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.observe(buf.get(..read).unwrap_or_default())?;
        Ok(read)
    }
}

/// Turn a tar level failure into the crate's error type.
///
/// The decoders under a tar wrap whatever the reader returned, so a ceiling or
/// a cancellation raised inside one reaches here disguised as a read failure
/// and has to be recovered before it is reported as a damaged container.
fn map_tar(error: io::Error, what: &str) -> VfsError {
    let label = what.to_owned();
    uncarry_or(error, move |error| {
        VfsError::corrupt(format!("{label}: {error}"))
    })
}

/// Build the listing, recording where each entry's content starts.
///
/// The offset is into the plain tar, which for a compressed container means
/// into the decoded stream rather than into the file.
///
/// # Errors
/// Returns [`VfsError::Corrupt`] when the tar headers do not parse, and the
/// typed error a ceiling or a cancellation raised inside the decoder.
pub(crate) fn list(
    backing: &ArchiveBacking,
    stream: Option<ArchiveFormat>,
    limits: &Limits,
    scan: &Budget,
    cancel: &Cancel,
) -> VfsResult<Vec<RawEntry>> {
    let reader = plain_stream(backing, stream, limits, scan, cancel)?;
    let mut archive = tar::Archive::new(reader);
    let entries = archive
        .entries()
        .map_err(|error| map_tar(error, "tar listing"))?;

    let mut out = Vec::new();
    for item in entries {
        cancel.check()?;
        let item = item.map_err(|error| map_tar(error, "tar entry"))?;
        let offset = item.raw_file_position();
        let header = item.header();
        let raw = String::from_utf8_lossy(&item.path_bytes()).into_owned();
        let kind = if header.entry_type().is_dir() {
            EntryKind::Directory
        } else {
            EntryKind::File
        };
        let Some((path, refusal)) = stored_path(&raw, kind == EntryKind::Directory).into_row()
        else {
            continue;
        };
        let link = match header.entry_type() {
            tar::EntryType::Symlink | tar::EntryType::Link => Some(VfsLinkKind::FileLink),
            _ => None,
        };
        let mode = header.mode().ok();
        let attributes = VfsAttributes {
            read_only: mode.is_some_and(|mode| mode & 0o200 == 0),
            hidden: path.name().is_some_and(|name| name.starts_with('.')),
            system: false,
            archive: false,
            windows_bits: None,
            unix_mode: mode,
            uid: header.uid().ok().and_then(|uid| u32::try_from(uid).ok()),
            gid: header.gid().ok().and_then(|gid| u32::try_from(gid).ok()),
        };

        let name = path.name().unwrap_or_default().to_owned();
        let mut entry = VfsEntry {
            path,
            name,
            kind,
            size: if kind == EntryKind::Directory {
                0
            } else {
                header.size().unwrap_or(0)
            },
            size_is_exact: true,
            modified: header
                .mtime()
                .ok()
                .and_then(|secs| i64::try_from(secs).ok().and_then(unix_to_system_time)),
            time_fidelity: crate::entry::TimeFidelity::Utc,
            created: None,
            attributes: Some(attributes),
            crc32: None,
            link,
            version_info: None,
            error: None,
            refused: false,
        };
        let locator = match refusal {
            Some(reason) => {
                refuse(&mut entry, &reason);
                None
            }
            None => (kind == EntryKind::File).then_some(offset),
        };
        out.push(RawEntry { entry, locator });
    }
    Ok(out)
}

/// Decompress the whole tar into a temporary file.
///
/// The file is deleted when the last handle to it drops, so it lives exactly
/// as long as the file system that needed it.
///
/// # Errors
/// Returns [`VfsError::LimitExceeded`] when the stream expands past the
/// container ceiling, [`VfsError::Cancelled`] when the flag is raised, and
/// [`VfsError::Io`] when the temporary file cannot be written.
pub(crate) fn decode_to_temp(
    backing: &ArchiveBacking,
    stream: Option<ArchiveFormat>,
    limits: &Limits,
    budget: &Budget,
    cancel: &Cancel,
) -> VfsResult<ArchiveBacking> {
    let mut reader = plain_stream(backing, stream, limits, budget, cancel)?;
    let mut temp = tempfile::NamedTempFile::new()?;
    let mut chunk = vec![0u8; 256 * 1024];
    loop {
        cancel.check()?;
        let read = match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) => return Err(map_tar(error, "tar stream")),
        };
        temp.write_all(chunk.get(..read).unwrap_or_default())?;
    }
    temp.flush()?;
    Ok(ArchiveBacking::Temp(Arc::new(temp)))
}

/// Expand one entry out of a plain tar.
///
/// `source` is the uncompressed tar and `offset` is where the entry's content
/// starts in it, so nothing before the entry is decoded or read again.
///
/// # Errors
/// Returns [`VfsError::NotFound`] when the listing recorded no position for
/// the entry, and [`VfsError::LimitExceeded`] when expanding it passes a
/// ceiling.
pub(crate) fn open_entry(
    source: &ArchiveBacking,
    offset: Option<u64>,
    path: &VfsPath,
    size: u64,
    limits: &Limits,
    budget: &Budget,
    cancel: &Cancel,
) -> VfsResult<OpenFile> {
    cancel.check()?;
    let offset = offset.ok_or_else(|| VfsError::NotFound { path: path.clone() })?;
    let mut reader = source.reader()?;
    reader.seek(SeekFrom::Start(offset))?;
    materialize(reader.take(size), size, limits, budget, cancel)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    #[test]
    fn a_ceiling_raised_inside_a_decoder_keeps_its_type() {
        let inner = carry(VfsError::LimitExceeded {
            kind: LimitKind::ArchiveSize,
            limit: 7,
        });
        let wrapped = io::Error::other(inner);
        assert!(matches!(
            map_tar(wrapped, "tar entry"),
            VfsError::LimitExceeded {
                kind: LimitKind::ArchiveSize,
                ..
            }
        ));
    }

    #[test]
    fn a_cancellation_raised_inside_a_decoder_keeps_its_type() {
        let wrapped = io::Error::other(carry(VfsError::Cancelled));
        assert!(matches!(map_tar(wrapped, "tar entry"), VfsError::Cancelled));
    }
}
